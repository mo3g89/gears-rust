//! Plugin configuration.
//!
//! Deliberately the same shape as `static-credstore-plugin`'s config
//! (`vendor`, `priority`, `secrets`) so a deployment can move a credstore
//! stanza from one plugin to the other by changing the vendor string, without
//! reshaping its config. The one difference is the default `vendor`: this
//! plugin ships its own so that both plugins can be compiled into the same
//! binary and `credstore.config.vendor` alone decides which one is live.

use serde::Deserialize;
use uuid::Uuid;

use credstore_sdk::SharingMode;

/// Default vendor string for this plugin.
///
/// Distinct from `static-credstore-plugin`'s `constructorfabric` on purpose:
/// [`choose_plugin_instance`](toolkit::plugins::choose_plugin_instance) matches
/// the vendor by exact string equality, so two plugins with different vendors
/// can coexist in one binary and the credstore gear's own
/// `config.vendor` selects between them.
pub const DEFAULT_VENDOR: &str = "constructorfabric-postgres";

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize, toolkit_macros::ExpandVars)]
#[serde(default, deny_unknown_fields)]
pub struct PostgresCredStorePluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,

    /// Optional seed secrets, same semantics as the static plugin's.
    ///
    /// `shared`/global entries stay in memory (read-only fallbacks);
    /// `tenant`/`private` entries are inserted into the table **only if no row
    /// for that key exists yet** — see
    /// [`Service::seed`](crate::domain::Service::seed).
    #[expand_vars]
    pub secrets: Vec<SecretConfig>,
}

impl Default for PostgresCredStorePluginConfig {
    fn default() -> Self {
        Self {
            vendor: DEFAULT_VENDOR.to_owned(),
            priority: 100,
            secrets: Vec::new(),
        }
    }
}

/// A single seed secret in the plugin configuration.
///
/// Field-for-field identical to `static-credstore-plugin`'s `SecretConfig`.
#[derive(Clone, Deserialize, toolkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct SecretConfig {
    /// Tenant that owns this secret.
    ///
    /// - `None` -> **global** secret, readable by any tenant (in-memory).
    /// - `Some` with `SharingMode::Shared` -> **shared** secret scoped to this
    ///   tenant (in-memory).
    /// - `Some` with `SharingMode::Tenant` -> **tenant** secret (persisted).
    ///
    /// `owner_id` cannot be set without `tenant_id`.
    pub tenant_id: Option<Uuid>,

    /// Owner (subject) of this secret. **Only valid for `Private` sharing.**
    pub owner_id: Option<Uuid>,

    /// Secret reference key (validated as a `SecretRef` at init).
    pub key: String,

    /// Secret value (plaintext string, converted to bytes at init).
    #[expand_vars]
    pub value: String,

    /// Sharing mode for this secret.
    ///
    /// When `None`, inferred from `tenant_id`/`owner_id`:
    /// - `tenant_id=None` -> `Shared`
    /// - `tenant_id=Some`, `owner_id=None` -> `Tenant`
    /// - `tenant_id=Some`, `owner_id=Some` -> `Private`
    pub sharing: Option<SharingMode>,
}

impl SecretConfig {
    /// Resolve the effective sharing mode from the explicit value or the
    /// `tenant_id`/`owner_id` combination.
    #[must_use]
    pub fn resolve_sharing(&self) -> SharingMode {
        self.sharing
            .unwrap_or(match (self.tenant_id, self.owner_id) {
                (None, _) => SharingMode::Shared,
                (Some(_), None) => SharingMode::Tenant,
                (Some(_), Some(_)) => SharingMode::Private,
            })
    }
}

impl core::fmt::Debug for SecretConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SecretConfig")
            .field("tenant_id", &self.tenant_id)
            .field("owner_id", &self.owner_id)
            .field("key", &self.key)
            .field("value", &"<redacted>")
            .field("sharing", &self.resolve_sharing())
            .finish()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
