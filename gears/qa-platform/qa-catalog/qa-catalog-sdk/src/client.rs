//! Object-safe client trait for inter-gear consumption via `ClientHub`.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::QaCatalogError;
use crate::models::{
    BundleRequest, CustomPlan, NewCustomPlan, NewProduct, NewTestRepository, Plan, Product,
    ProductUpdate, SshKey, SyncRequest, TestBundle, TestFileMeta, TestRepository,
    TestRepositoryUpdate, UniverseTest,
};

/// Object-safe client for the qa-catalog gear (Version 1).
///
/// Registered in `ClientHub`:
/// ```ignore
/// let catalog = hub.get::<dyn QaCatalogClientV1>()?;
/// ```
///
/// Primary consumer: qa-runs (plan resolution, `TEST_META` aggregation input,
/// bundle creation at launch).
#[async_trait]
pub trait QaCatalogClientV1: Send + Sync {
    // ==================== Repositories ====================

    async fn list_repos(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<TestRepository>, QaCatalogError>;

    async fn get_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TestRepository, QaCatalogError>;

    async fn create_repo(
        &self,
        ctx: &SecurityContext,
        new: NewTestRepository,
    ) -> Result<TestRepository, QaCatalogError>;

    /// Replace a registered repository's mutable fields.
    ///
    /// `default_branch` is part of [`TestRepositoryUpdate`] and may be
    /// changed. It selects the branch used when a caller names none; it does
    /// not identify the repository's synced content, so changing it does not
    /// invalidate anything already materialized.
    ///
    /// Changing `url` or `content_root` invalidates the existing working
    /// copy, so the synced state is cleared and content reads reject with
    /// `failed_precondition` (repository not synced) until the next
    /// [`sync_repo`](Self::sync_repo) — content is never served from the old
    /// URL.
    async fn update_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        update: TestRepositoryUpdate,
    ) -> Result<TestRepository, QaCatalogError>;

    async fn delete_repo(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaCatalogError>;

    /// Trigger a sync now; returns when the sync completes or fails.
    ///
    /// `req.branch` selects which branch's content snapshot is materialized
    /// (`None` = the repository's `default_branch`). An empty or
    /// whitespace-only `Some` is treated as `None`. `req.force` skips (and
    /// evicts) the in-memory freshness TTL entry. qa-runs' launch path always
    /// passes an explicit branch with `force: true` (parity spec §3.4 step 4).
    async fn sync_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: SyncRequest,
    ) -> Result<TestRepository, QaCatalogError>;

    /// Cached branch names for the repo (refreshed by sync / branch-cache task).
    async fn list_branches(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Vec<String>, QaCatalogError>;

    // ==================== Plans (discovered) + metadata ====================

    async fn list_plans(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
    ) -> Result<Vec<Plan>, QaCatalogError>;

    async fn get_plan(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        path: &str,
    ) -> Result<Plan, QaCatalogError>;

    /// Parsed `TEST_META` for the given files (used by qa-runs exclusivity OR-aggregation).
    async fn get_test_meta(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        files: &[String],
    ) -> Result<Vec<TestFileMeta>, QaCatalogError>;

    /// Every test file reachable from the given product's plans on `branch`,
    /// with its `TEST_META` attributes and static case count.
    ///
    /// A **read projection for qa-insights**, which has no git checkout of its
    /// own (ADR-0005 confines git egress to this gear) and so cannot compute
    /// the analytics universe legacy computed inline
    /// (`manager/src/routes/analytics.rs:805-951`). See [`UniverseTest`] for
    /// the field-by-field mapping and the four documented divergences.
    ///
    /// One call, not one per plan: analytics computes a whole-universe summary,
    /// and an N+1 across the SDK boundary would make the overview endpoint's
    /// latency a function of plan count.
    ///
    /// `product_id: None` means every product visible to `ctx`. `branch: None`
    /// falls back to each repository's own `default_branch`, which is legacy's
    /// no-branch-selected behavior (`analytics.rs:810-816`). A repository that
    /// is not synced for the selected branch contributes nothing rather than
    /// failing the whole call — legacy drops such repositories the same way
    /// (`analytics.rs:861-865`), and one unsynced repository must not blank the
    /// overview for all the others.
    ///
    /// Rows are ordered by `test_name`, matching legacy's sort
    /// (`analytics.rs:941`).
    async fn list_universe(
        &self,
        ctx: &SecurityContext,
        product_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<Vec<UniverseTest>, QaCatalogError>;

    // ==================== Custom plans ====================

    async fn list_custom_plans(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<CustomPlan>, QaCatalogError>;

    async fn get_custom_plan(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<CustomPlan, QaCatalogError>;

    async fn create_custom_plan(
        &self,
        ctx: &SecurityContext,
        new: NewCustomPlan,
    ) -> Result<CustomPlan, QaCatalogError>;

    async fn update_custom_plan(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        new: NewCustomPlan,
    ) -> Result<CustomPlan, QaCatalogError>;

    async fn delete_custom_plan(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaCatalogError>;

    // ==================== Products ====================

    async fn list_products(&self, ctx: &SecurityContext) -> Result<Vec<Product>, QaCatalogError>;

    async fn create_product(
        &self,
        ctx: &SecurityContext,
        new: NewProduct,
    ) -> Result<Product, QaCatalogError>;

    /// Replace a product's mutable fields — the whole of [`ProductUpdate`] —
    /// as a full replace (`folder: None` moves it back to the root,
    /// `plugin_instance_id: None` leaves its plugin binding **unchanged** —
    /// see `UpdateProductReq`'s own doc for why unbinding is not expressible).
    async fn update_product(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        update: ProductUpdate,
    ) -> Result<Product, QaCatalogError>;

    /// Delete a product.
    ///
    /// Fails if the product still owns test repositories — the
    /// `qa_test_repositories.product_id` foreign key is `ON DELETE RESTRICT`,
    /// so repositories must be reassigned or removed first.
    async fn delete_product(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaCatalogError>;

    // ==================== SSH keys ====================

    async fn list_ssh_keys(&self, ctx: &SecurityContext) -> Result<Vec<SshKey>, QaCatalogError>;

    /// Stores `private_key_pem` in credstore immediately; the material never
    /// persists in this gear — only the reference and fingerprint are returned.
    async fn create_ssh_key(
        &self,
        ctx: &SecurityContext,
        name: String,
        private_key_pem: String,
    ) -> Result<SshKey, QaCatalogError>;

    async fn delete_ssh_key(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaCatalogError>;

    // ==================== Bundles ====================

    /// Build an ephemeral bundle for a run (called by qa-runs at launch).
    async fn create_bundle(
        &self,
        ctx: &SecurityContext,
        req: BundleRequest,
    ) -> Result<TestBundle, QaCatalogError>;

    /// Fetch bundle bytes for download serving. (Streaming variant can come later.)
    async fn get_bundle_content(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Vec<u8>, QaCatalogError>;
}
