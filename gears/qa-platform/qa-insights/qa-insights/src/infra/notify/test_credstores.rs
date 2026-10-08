//! Credential-store doubles shared by this module's adapter tests.
//!
//! One definition, imported by `slack_oagw_tests` and `mail_smtp_tests`, so
//! the two adapters are tested against the same misbehaving store rather than
//! two copies that can drift apart.
//!
//! - [`HangingCredStore`] never answers (the send deadlines' tests).
//! - [`DenyingCredStore`] answers every read with `AccessDenied`.
//! - [`SharingCredStore`] answers by credstore's sharing rule, so a test can
//!   tell which identity a secret was read as.

/// A credential store that never answers.
pub struct HangingCredStore;

#[async_trait::async_trait]
impl credstore_sdk::CredStoreClientV1 for HangingCredStore {
    async fn get(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _key: &credstore_sdk::SecretRef,
    ) -> Result<Option<credstore_sdk::GetSecretResponse>, credstore_sdk::CredStoreError> {
        std::future::pending().await
    }
}

/// A credential store that answers every read with `AccessDenied`, the SDK's
/// documented answer for a caller without read permission. The SDK's own
/// `test_util` has a constructor for `NotFound` only.
pub struct DenyingCredStore;

#[async_trait::async_trait]
impl credstore_sdk::CredStoreClientV1 for DenyingCredStore {
    async fn get(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _key: &credstore_sdk::SecretRef,
    ) -> Result<Option<credstore_sdk::GetSecretResponse>, credstore_sdk::CredStoreError> {
        Err(credstore_sdk::CredStoreError::AccessDenied)
    }
}

/// One secret [`SharingCredStore`] holds: who owns it, in which tenant,
/// shared how.
pub struct StoredSecret {
    /// The reference, a bare name as every credential reference is.
    pub reference: &'static str,
    pub value: &'static str,
    pub tenant: uuid::Uuid,
    pub owner: uuid::Uuid,
    pub sharing: credstore_sdk::SharingMode,
}

/// A credential store that applies credstore's sharing rule to the caller.
///
/// `credstore_sdk::test_util::MockCredStoreClient` answers the same value to
/// every caller, so it cannot tell a user's read from the qa-insights system
/// actor's — the distinction a settings test send has to make. This double
/// applies the visibility predicate of credstore's resolver (`resolve_for_get`
/// in `gears/credstore/credstore/src/infra/storage/repo_impl/reads.rs`) for a
/// one-tenant chain: `Private` is visible to its owner only, `Tenant` and
/// `Shared` to every subject of the owning tenant. A miss is `Ok(None)`, the
/// SDK's single anti-enumeration surface. Read-only: every write takes the
/// trait's default, which fails.
pub struct SharingCredStore {
    secrets: Vec<StoredSecret>,
}

impl SharingCredStore {
    pub const fn new(secrets: Vec<StoredSecret>) -> Self {
        Self { secrets }
    }

    fn visible(
        secret: &StoredSecret,
        ctx: &toolkit_security::SecurityContext,
        key: &credstore_sdk::SecretRef,
    ) -> bool {
        secret.reference == key.as_ref()
            && secret.tenant == ctx.subject_tenant_id()
            && match secret.sharing {
                credstore_sdk::SharingMode::Private => secret.owner == ctx.subject_id(),
                credstore_sdk::SharingMode::Tenant | credstore_sdk::SharingMode::Shared => true,
            }
    }
}

#[async_trait::async_trait]
impl credstore_sdk::CredStoreClientV1 for SharingCredStore {
    async fn get(
        &self,
        ctx: &toolkit_security::SecurityContext,
        key: &credstore_sdk::SecretRef,
    ) -> Result<Option<credstore_sdk::GetSecretResponse>, credstore_sdk::CredStoreError> {
        Ok(self
            .secrets
            .iter()
            .find(|s| Self::visible(s, ctx, key))
            .map(|s| credstore_sdk::GetSecretResponse {
                value: credstore_sdk::SecretValue::new(s.value.as_bytes().to_vec()),
                id: uuid::Uuid::nil(),
                owner_tenant_id: credstore_sdk::TenantId(s.tenant),
                sharing: s.sharing,
                is_inherited: false,
                version: 1,
                secret_type: credstore_sdk::SecretType::generic().gts_id().to_owned(),
                expires_at: None,
            }))
    }
}
