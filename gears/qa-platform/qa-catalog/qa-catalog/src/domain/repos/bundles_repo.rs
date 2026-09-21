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

    /// The tenant that owns bundle `id`, or `None` when no such descriptor
    /// exists — a **one-column** read, and the only cross-tenant read on a
    /// request path in this gear.
    ///
    /// # Why the download path needs this and `get` cannot serve it
    ///
    /// `GET /qa/v1/test-bundles/{id}?sig=...` is anonymous: there is no caller
    /// tenant to build a scope from, and the tag it presents is verified under
    /// the *owning tenant's* derived key, so the tenant has to be known before
    /// anything can be verified. [`Self::get`] is scoped, which is exactly
    /// right for the read that follows and exactly wrong for this one.
    ///
    /// **It projects `tenant_id` and nothing else, deliberately.** A caller
    /// that could see the descriptor here — `storage_ref`, `expires_at`,
    /// `checksum_sha256` — would have a cross-tenant read of bundle metadata
    /// reachable without a signature, since this call necessarily happens
    /// *before* verification. One opaque UUID that the caller never sees (it is
    /// consumed inside `BundlesService::get_bundle_content_signed` and never
    /// returned, not even in an error) is the whole of what the verification
    /// step needs.
    ///
    /// Pass `domain::elevated::enumeration_scope()`; the tenant-scoped read of
    /// the same row still happens afterwards, under
    /// `system_actor::for_bundle_download`, and that read is what actually
    /// authorises serving the bytes.
    async fn tenant_of<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Uuid>, DomainError>;

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
