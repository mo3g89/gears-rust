//! SSH key metadata service.
//!
//! The private key material is written to credstore immediately and never
//! persists in this gear — the database row carries only the credstore
//! reference and a fingerprint. The material is never logged and never
//! embedded in error messages.
//!
//! # What "stored in credstore" does and does not guarantee
//!
//! The material is written with [`SharingMode::Tenant`], so it is **not**
//! unrecoverable: credstore binds reads to the owning user only for
//! `SharingMode::Private` (see
//! `gears/credstore/credstore/src/domain/secret/service.rs`, the
//! `sharing == Private` owner check on the read path). Any tenant member who
//! holds the reference can therefore read the key back through credstore's
//! own `GET /credstore/v1/secrets/{ref}`. This gear's guarantee is narrower
//! and exact: **qa-catalog never persists the material and never returns it
//! (nor its credstore reference) over its own API** — the REST `SshKeyDto`
//! deliberately omits `credstore_ref` for this reason.
//!
//! What that leaves, in p1:
//!
//! * References are unguessable (`qa-catalog-ssh-key-<v4 uuid>`) and
//!   credstore exposes no enumeration surface — its REST API is exactly
//!   `POST /credstore/v1/secrets` plus `GET`/`PUT`/`DELETE
//!   /credstore/v1/secrets/{ref}`, and `CredStoreClientV1` has no list or
//!   search operation (the `list_unfenced` / `list_stale_pending` repository
//!   methods back credstore's internal fence and reaper jobs; they are not
//!   reachable through REST or the SDK). So a tenant member cannot discover
//!   another member's reference through a supported API path.
//! * Cross-tenant reads are impossible either way: the credstore read path
//!   fails closed on a tenant-scope mismatch, and this gear's rows are
//!   PEP-scoped per tenant.
//! * Residual, accepted for p1 and worth a team decision before keys are
//!   wired into sync (`SharingMode::Private` vs. accepting tenant-readable
//!   keys): a member who learns a reference out-of-band — or any in-process
//!   consumer of the SDK `SshKey` model, which still carries it — can read
//!   the material as a tenant member. Nothing in p1 hands that reference to
//!   a REST client, and SSH keys are not yet wired to repository sync at all
//!   (p1 repositories are http(s)-only per ADR-0005, and a repository's
//!   `credential_ref` is a free-form value supplied by the client, never
//!   derived from an `SshKey` row).

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::{
    CredStoreClientV1, CredStoreError, SecretRef, SecretValue, SharingMode, WritePrecondition,
};
use qa_catalog_sdk::SshKey;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use super::validation::validate_name;
use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::repos::SshKeysRepository;

/// SSH key metadata service.
#[domain_model]
pub struct SshKeysService<K: SshKeysRepository> {
    db: Arc<DbProvider>,
    repo: Arc<K>,
    credstore: Arc<dyn CredStoreClientV1>,
    policy_enforcer: PolicyEnforcer,
}

impl<K: SshKeysRepository> SshKeysService<K> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<K>,
        credstore: Arc<dyn CredStoreClientV1>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repo,
            credstore,
            policy_enforcer,
        }
    }
}

// Business logic methods
impl<K: SshKeysRepository> SshKeysService<K> {
    #[instrument(skip(self, ctx))]
    pub async fn list_ssh_keys(&self, ctx: &SecurityContext) -> Result<Vec<SshKey>, DomainError> {
        debug!("Listing SSH keys");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::SSH_KEY, actions::LIST, None)
            .await?;

        let conn = self.db.conn()?;
        self.repo.list(&conn, &scope).await
    }

    /// Store `private_key_pem` in credstore under a freshly generated
    /// reference and persist metadata (reference + fingerprint) only.
    #[instrument(skip(self, ctx, name, private_key_pem), fields(name = %name))]
    pub async fn create_ssh_key(
        &self,
        ctx: &SecurityContext,
        name: String,
        private_key_pem: String,
    ) -> Result<SshKey, DomainError> {
        info!("Creating SSH key");

        validate_name("name", &name)?;
        if private_key_pem.trim().is_empty() {
            return Err(DomainError::Validation {
                field: "private_key_pem".to_owned(),
                message: "must not be empty".to_owned(),
            });
        }

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::SSH_KEY, actions::CREATE, None)
            .await?;

        let fingerprint = fingerprint(&private_key_pem);

        // Material goes to credstore FIRST; only the reference reaches the DB.
        let raw_ref = format!("qa-catalog-ssh-key-{}", Uuid::new_v4().simple());
        let secret_ref = SecretRef::new(raw_ref.clone())
            .map_err(|e| DomainError::Internal(format!("generated secret ref invalid: {e}")))?;
        self.credstore
            .create(
                ctx,
                &secret_ref,
                SecretValue::from(private_key_pem),
                SharingMode::Tenant,
            )
            .await
            .map_err(map_credstore_error)?;

        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();

        let created = self
            .repo
            .create(&conn, &scope, tenant_id, name, raw_ref, fingerprint)
            .await;

        match created {
            Ok(key) => {
                info!("Successfully created SSH key with id={}", key.id);
                Ok(key)
            }
            Err(db_err) => {
                // Don't orphan the just-written secret when the metadata row
                // fails (e.g. duplicate name): best-effort cleanup, then
                // propagate the original error.
                if let Err(cleanup_err) = self
                    .credstore
                    .delete(ctx, &secret_ref, WritePrecondition::Exists)
                    .await
                {
                    warn!(error = %cleanup_err, "Failed to clean up credstore secret after DB error");
                }
                Err(db_err)
            }
        }
    }

    /// Delete an SSH key: the credstore secret is removed first, then the
    /// metadata row — a dangling reference is worse than a briefly orphaned
    /// row (which a retried delete removes).
    #[instrument(skip(self, ctx), fields(ssh_key_id = %id))]
    pub async fn delete_ssh_key(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        info!("Deleting SSH key");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::SSH_KEY, actions::DELETE, Some(id))
            .await?;

        // Resolve the row (for its credstore_ref) under its own GET scope.
        // The repository trait deliberately has no point-get — key metadata
        // is only ever listed — so resolve via the scoped list.
        let get_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::SSH_KEY, actions::GET, Some(id))
            .await?;
        let conn = self.db.conn()?;
        let key = self
            .repo
            .list(&conn, &get_scope)
            .await?
            .into_iter()
            .find(|k| k.id == id)
            .ok_or(DomainError::NotFound { id })?;

        // Secret first. A ref that is already gone is fine (idempotent
        // retry after a partial failure); anything else aborts before the
        // row is touched.
        if let Ok(secret_ref) = SecretRef::new(key.credstore_ref.clone()) {
            match self
                .credstore
                .delete(ctx, &secret_ref, WritePrecondition::Exists)
                .await
            {
                Ok(()) | Err(CredStoreError::NotFound | CredStoreError::Conflict) => {}
                Err(e) => return Err(map_credstore_error(e)),
            }
        }

        let deleted = self.repo.delete(&conn, &scope, id).await?;
        if !deleted {
            return Err(DomainError::NotFound { id });
        }

        info!("Successfully deleted SSH key");
        Ok(())
    }
}

/// Compute the key fingerprint.
///
/// p1 fallback (documented per the plan): deriving the public key from the
/// PEM would require a new SSH/crypto dependency, so the fingerprint is the
/// SHA-256 of the PEM material itself, rendered as `SHA256:<hex>`. It still
/// uniquely identifies the key for display/dedup purposes; it is NOT the
/// OpenSSH public-key fingerprint. Revisit if an ssh-key crate lands in the
/// workspace.
fn fingerprint(private_key_pem: &str) -> String {
    let hex = super::bundles::sha256_hex(private_key_pem.as_bytes());
    format!("SHA256:{hex}")
}

fn map_credstore_error(e: CredStoreError) -> DomainError {
    match e {
        CredStoreError::AccessDenied => DomainError::Forbidden,
        other => DomainError::CredStore(other.to_string()),
    }
}
