//! The plugin's value store: a persistent table for the two runtime-written
//! key classes, plus the in-memory config-seeded read fallbacks.
//!
//! # Where each key class lives, and why
//!
//! | key class | selected by | storage |
//! |---|---|---|
//! | `private` | `owner_id = Some` | table row (`owner_id NOT NULL`) |
//! | `tenant`  | `owner_id = None` | table row (`owner_id NULL`) |
//! | `shared`  | config only | in memory |
//! | `global`  | config only | in memory |
//!
//! Only `private` and `tenant` are ever written by `put`/`delete`, so only
//! those need to survive a restart. `shared`/`global` have no write path at
//! all — they are rebuilt from configuration on every boot and are read-only
//! fallbacks for `owner_id = None` lookups — so persisting them would add rows
//! that configuration already owns. Keeping them in memory also preserves the
//! invariant the in-memory plugin documents: a tenant-scoped delete (including
//! gear saga retries and reaper reconciliation) must never destroy an entry
//! that serves other tenants.
//!
//! The read order is therefore unchanged from the in-memory plugin:
//! `tenant (row) -> shared -> global` for `owner_id = None`, and the private
//! row alone for `owner_id = Some`.

use std::collections::HashMap;

use credstore_sdk::{CredStoreError, OwnerId, SecretRef, SecretValue, SharingMode, TenantId};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::PostgresCredStorePluginConfig;
use crate::infra::storage::error::StoreError;
use crate::infra::storage::repo::ValueRepo;

/// A config-seeded entry belonging to a persisted key class.
struct Seed {
    tenant_id: TenantId,
    owner_id: Option<OwnerId>,
    key: SecretRef,
    value: SecretValue,
}

/// Database-backed credstore backend.
///
/// A pure per-tenant value store implementing the `CredStorePluginClientV1`
/// contract: `owner_id = Some` selects the private key class, `None` the
/// tenant key class. Sharing, hierarchy, policy, TTL and type validation live
/// in the credstore gear, not here.
///
/// Not marked `#[domain_model]`: it owns its storage adapter directly, the way
/// the in-memory plugin's `Service` owns its `HashMap`s. A three-method value
/// store has no domain logic worth a port trait, and the tests exercise the
/// real `SeaORM` repository against `SQLite` rather than a double, so an
/// abstraction here would only add indirection.
pub struct Service {
    repo: ValueRepo,
    /// Config-seeded `shared` secrets — read fallback for the tenant class.
    shared: HashMap<(TenantId, SecretRef), SecretValue>,
    /// Config-seeded global secrets — final read fallback for the tenant class.
    global: HashMap<SecretRef, SecretValue>,
    /// Config-seeded `tenant`/`private` entries, applied by [`Service::seed`].
    seeds: Vec<Seed>,
}

/// Log a storage fault in full, then hand the caller a curated SPI error.
fn map_store_err(op: &'static str, err: StoreError) -> CredStoreError {
    warn!(
        target: "postgres_credstore_plugin",
        operation = op,
        error = %err,
        "credstore value store operation failed"
    );
    CredStoreError::from(err)
}

impl Service {
    /// Create a service over `repo`, validating and classifying `cfg.secrets`.
    ///
    /// Validation is identical to the in-memory plugin's, so a config that
    /// loads there loads here.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - any configured key fails `SecretRef` validation
    /// - duplicate keys within the same sharing scope
    /// - a secret without `owner_id` has an explicit `SharingMode::Private`
    /// - `tenant_id` or `owner_id` is an explicit nil UUID
    /// - `owner_id` is set without `tenant_id`
    pub fn from_config(
        repo: ValueRepo,
        cfg: &PostgresCredStorePluginConfig,
    ) -> anyhow::Result<Self> {
        let mut shared: HashMap<(TenantId, SecretRef), SecretValue> = HashMap::new();
        let mut global: HashMap<SecretRef, SecretValue> = HashMap::new();
        let mut seeds: Vec<Seed> = Vec::new();

        for entry in &cfg.secrets {
            let sharing = validate_entry(entry)?;
            let key = SecretRef::new(&entry.key)?;
            let value = SecretValue::from(entry.value.as_str());

            match (sharing, entry.tenant_id) {
                (SharingMode::Shared, None) => {
                    if global.contains_key(&key) {
                        anyhow::bail!("duplicate global secret key '{}'", entry.key);
                    }
                    global.insert(key, value);
                }
                (SharingMode::Shared, Some(raw_tenant_id)) => {
                    let tenant_id = TenantId(raw_tenant_id);
                    let map_key = (tenant_id, key);
                    if shared.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate shared secret key '{}' for tenant {}",
                            entry.key,
                            tenant_id
                        );
                    }
                    shared.insert(map_key, value);
                }
                (SharingMode::Tenant, _) => {
                    let tenant_id = TenantId(require_tenant(entry.tenant_id, &entry.key)?);
                    push_seed(&mut seeds, tenant_id, None, key, value, &entry.key)?;
                }
                (SharingMode::Private, _) => {
                    let tenant_id = TenantId(require_tenant(entry.tenant_id, &entry.key)?);
                    let owner_id = OwnerId(entry.owner_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': private sharing mode requires owner_id",
                            entry.key
                        )
                    })?);
                    push_seed(
                        &mut seeds,
                        tenant_id,
                        Some(owner_id),
                        key,
                        value,
                        &entry.key,
                    )?;
                }
            }
        }

        Ok(Self {
            repo,
            shared,
            global,
            seeds,
        })
    }

    /// Write the config-seeded `tenant`/`private` entries that are not in the
    /// table yet, and report how many rows were created.
    ///
    /// **Insert-if-absent, not upsert** — a deliberate, documented divergence
    /// from the in-memory plugin, which rebuilds its maps from configuration on
    /// every boot and therefore silently reverts a runtime rotation on restart.
    /// In a *persistent* backend that behaviour would destroy secret material a
    /// client wrote through the API, which is the exact failure this plugin
    /// exists to end. Absent seeds are still created, so a first boot behaves
    /// the same as the in-memory plugin's.
    ///
    /// # Errors
    ///
    /// Returns an error if any seed write fails; `init` propagates it so a gear
    /// that cannot honour its configured seeds does not come up half-seeded and
    /// silent.
    pub async fn seed(&self) -> anyhow::Result<usize> {
        let mut created = 0usize;
        for seed in &self.seeds {
            let inserted = self
                .repo
                .insert_if_absent(
                    &seed.tenant_id,
                    &seed.key,
                    seed.owner_id.as_ref(),
                    seed.value.as_bytes(),
                )
                .await?;
            if inserted {
                created += 1;
            }
        }
        if created > 0 {
            info!(
                target: "postgres_credstore_plugin",
                created,
                total = self.seeds.len(),
                "seeded configured secrets that had no stored value yet"
            );
        }
        Ok(created)
    }

    /// Read a value for the selected key class.
    ///
    /// `owner_id = Some` reads the private class; `None` reads the tenant class
    /// and falls back to config-seeded `shared` then global entries.
    ///
    /// # Errors
    ///
    /// Returns [`CredStoreError::ServiceUnavailable`] if the store is
    /// unreachable. A key that simply does not exist is `Ok(None)`.
    pub async fn get_value(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        if let Some(bytes) = self
            .repo
            .find(tenant_id, key, owner_id)
            .await
            .map_err(|e| map_store_err("get", e))?
        {
            return Ok(Some(SecretValue::new(bytes)));
        }
        if owner_id.is_some() {
            // Private lookups never fall back — matching the in-memory plugin,
            // where `Some(owner)` reads the `private` map alone.
            return Ok(None);
        }
        // `SecretValue` is not `Clone` (it zeroizes on drop), so reconstruct.
        let fallback = self
            .shared
            .get(&(*tenant_id, key.clone()))
            .or_else(|| self.global.get(key))
            .map(|v| SecretValue::new(v.as_bytes().to_vec()));
        Ok(fallback)
    }

    /// Insert or overwrite a value in the selected key class.
    ///
    /// # Errors
    ///
    /// Returns [`CredStoreError::ServiceUnavailable`] if the write fails.
    pub async fn put_value(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        value: SecretValue,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError> {
        self.repo
            .upsert(tenant_id, key, owner_id, value.as_bytes())
            .await
            .map_err(|e| map_store_err("put", e))
    }

    /// Remove a value from the selected key class.
    ///
    /// Deletes address the **stored** rows only. The config-seeded `shared` and
    /// global fallbacks are cross-tenant reference data: a tenant-scoped delete
    /// must never destroy an entry that serves other tenants. A miss is a
    /// no-op — the gear treats a missing backend value as success.
    ///
    /// # Errors
    ///
    /// Returns [`CredStoreError::ServiceUnavailable`] if the delete fails.
    pub async fn delete_value(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError> {
        self.repo
            .delete(tenant_id, key, owner_id)
            .await
            .map_err(|e| map_store_err("delete", e))
    }
}

/// Validate one config entry and return its resolved sharing mode.
fn validate_entry(entry: &crate::config::SecretConfig) -> anyhow::Result<SharingMode> {
    if entry.tenant_id == Some(Uuid::nil()) {
        anyhow::bail!("secret '{}': tenant_id must not be nil UUID", entry.key);
    }
    if entry.owner_id == Some(Uuid::nil()) {
        anyhow::bail!("secret '{}': owner_id must not be nil UUID", entry.key);
    }
    if entry.tenant_id.is_none() && entry.owner_id.is_some() {
        anyhow::bail!(
            "secret '{}': owner_id cannot be set without tenant_id",
            entry.key
        );
    }

    let sharing = entry.resolve_sharing();

    if entry.owner_id.is_some() && sharing != SharingMode::Private {
        anyhow::bail!(
            "secret '{}': owner_id is only valid for private sharing mode, \
             but resolved sharing is {sharing:?}",
            entry.key
        );
    }
    if entry.owner_id.is_none() && sharing == SharingMode::Private {
        anyhow::bail!(
            "secret '{}' with sharing mode 'private' requires an explicit owner_id",
            entry.key
        );
    }
    Ok(sharing)
}

fn require_tenant(tenant_id: Option<Uuid>, key: &str) -> anyhow::Result<Uuid> {
    tenant_id.ok_or_else(|| anyhow::anyhow!("secret '{key}': this sharing mode requires tenant_id"))
}

/// Push a seed, rejecting a duplicate within the same key class.
fn push_seed(
    seeds: &mut Vec<Seed>,
    tenant_id: TenantId,
    owner_id: Option<OwnerId>,
    key: SecretRef,
    value: SecretValue,
    raw_key: &str,
) -> anyhow::Result<()> {
    if seeds
        .iter()
        .any(|s| s.tenant_id == tenant_id && s.owner_id == owner_id && s.key == key)
    {
        anyhow::bail!(
            "duplicate seed secret key '{raw_key}' for tenant {tenant_id} owner {owner_id:?}"
        );
    }
    seeds.push(Seed {
        tenant_id,
        owner_id,
        key,
        value,
    });
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
