use async_trait::async_trait;
use qa_catalog_sdk::SshKey;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Repository trait for SSH key *metadata*. The private key material lives in
/// credstore and never reaches this layer — only `credstore_ref` and the
/// fingerprint are persisted.
#[async_trait]
pub trait SshKeysRepository: Send + Sync {
    /// List all SSH key metadata rows visible within the given security scope.
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<SshKey>, DomainError>;

    /// Create a new SSH key metadata row. `credstore_ref` must already point
    /// at stored material and `fingerprint` must already be derived from it.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        name: String,
        credstore_ref: String,
        fingerprint: String,
    ) -> Result<SshKey, DomainError>;

    /// Fetch one SSH key metadata row by ID, scoped as usual.
    ///
    /// Used by `ReposService::resolve_credential`: for an SSH remote a
    /// repository's `credential_ref` names one of these rows, and the
    /// credstore reference that actually holds the key material is
    /// `SshKey::credstore_ref` — which is deliberately never published over
    /// the REST API (`api::rest::dto::SshKeyDto`), so the gear has to make
    /// this hop itself.
    async fn find_by_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<SshKey>, DomainError>;

    /// Delete an SSH key metadata row by ID. Purging the credstore entry is
    /// the service's job.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;
}
