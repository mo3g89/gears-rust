use async_trait::async_trait;
use qa_catalog_sdk::TestBundle;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Repository trait for ephemeral test-bundle descriptors. The blob itself
/// lives in the `BundleStore`; this table only tracks where it is and when it
/// expires.
#[async_trait]
pub trait BundlesRepository: Send + Sync {
    /// Persist a bundle descriptor.
    ///
    /// The descriptor is stored as given, including `id` and `created_at`: the
    /// service allocates the ID *before* calling `BundleStore::put`, so the
    /// blob and this row share one identity.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        bundle: TestBundle,
    ) -> Result<TestBundle, DomainError>;

    /// Find a bundle descriptor by ID within the given security scope.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TestBundle>, DomainError>;

    /// Delete every descriptor that expired at or before `now` and return the
    /// deleted rows, so the caller can drop the matching blobs from the bundle
    /// store. Select-then-delete inside the caller's runner: pass a
    /// transaction runner to make the pair atomic.
    ///
    /// One call purges exactly the tenant(s) `scope` covers — a platform-wide
    /// GC sweep calls this once per tenant, under that tenant's own scope, so
    /// the atomic select+delete stays inside a transaction that spans one
    /// tenant only. See `BundlesService::purge_expired` and
    /// `domain::system_actor::for_bundle_delete`.
    async fn delete_expired<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        now: OffsetDateTime,
    ) -> Result<Vec<TestBundle>, DomainError>;

    /// Every tenant with at least one bundle descriptor expired at or before
    /// `now`, ascending, `DISTINCT`.
    ///
    /// The enumerating half of the GC pass: `BundlesService::purge_expired`
    /// runs once per tenant this answers with, each under its own
    /// [`Self::delete_expired`] transaction. Kept separate from
    /// `delete_expired` itself specifically so the atomic select+delete stays
    /// tenant-scoped even though the read that discovers *which* tenants to
    /// sweep is cross-tenant — see `crate::domain::elevated`'s module doc.
    async fn tenants_with_expired_bundles<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        now: OffsetDateTime,
    ) -> Result<Vec<Uuid>, DomainError>;
}
