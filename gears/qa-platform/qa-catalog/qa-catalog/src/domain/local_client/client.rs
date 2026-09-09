//! Local client adapter: implements the object-safe `QaCatalogClientV1`
//! by delegating to `AppServices`, converting `DomainError` into
//! `QaCatalogError` (`CanonicalError`) via the `From` impl in
//! `api::rest::error`.

use std::sync::Arc;

use async_trait::async_trait;
use qa_catalog_sdk::{
    BundleRequest, CustomPlan, NewCustomPlan, NewProduct, NewTestRepository, Plan, Product,
    ProductUpdate, QaCatalogClientV1, QaCatalogError, SshKey, SyncRequest, TestBundle,
    TestFileMeta, TestRepository, TestRepositoryUpdate, UniverseTest,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::gear::ConcreteAppServices;

/// Local implementation of the object-safe `QaCatalogClientV1`.
pub struct QaCatalogLocalClient {
    services: Arc<ConcreteAppServices>,
}

impl QaCatalogLocalClient {
    #[must_use]
    pub fn new(services: Arc<ConcreteAppServices>) -> Self {
        Self { services }
    }
}

#[async_trait]
impl QaCatalogClientV1 for QaCatalogLocalClient {
    // ==================== Repositories ====================

    async fn list_repos(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<TestRepository>, QaCatalogError> {
        self.services
            .repos
            .list_repos(ctx)
            .await
            .map_err(Into::into)
    }

    async fn get_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TestRepository, QaCatalogError> {
        self.services
            .repos
            .get_repo(ctx, id)
            .await
            .map_err(Into::into)
    }

    async fn create_repo(
        &self,
        ctx: &SecurityContext,
        new: NewTestRepository,
    ) -> Result<TestRepository, QaCatalogError> {
        self.services
            .repos
            .create_repo(ctx, new)
            .await
            .map_err(Into::into)
    }

    async fn update_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        update: TestRepositoryUpdate,
    ) -> Result<TestRepository, QaCatalogError> {
        self.services
            .repos
            .update_repo(ctx, id, update)
            .await
            .map_err(Into::into)
    }

    async fn delete_repo(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaCatalogError> {
        self.services
            .repos
            .delete_repo(ctx, id)
            .await
            .map_err(Into::into)
    }

    async fn sync_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: SyncRequest,
    ) -> Result<TestRepository, QaCatalogError> {
        // The service spells "no branch named" as the empty selector and
        // resolves it to the repository's `default_branch`, which is the
        // source system's fallback (`manager/src/services/test_repos.rs:498-502`).
        self.services
            .repos
            .sync_repo(ctx, id, req.branch.as_deref().unwrap_or(""), req.force)
            .await
            .map_err(Into::into)
    }

    async fn list_branches(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Vec<String>, QaCatalogError> {
        self.services
            .repos
            .list_branches(ctx, id)
            .await
            .map_err(Into::into)
    }

    // ==================== Plans (discovered) + metadata ====================

    async fn list_plans(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
    ) -> Result<Vec<Plan>, QaCatalogError> {
        self.services
            .plans
            .list_plans(ctx, repo_id, branch)
            .await
            .map_err(Into::into)
    }

    async fn get_plan(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        path: &str,
    ) -> Result<Plan, QaCatalogError> {
        self.services
            .plans
            .get_plan(ctx, repo_id, branch, path)
            .await
            .map_err(Into::into)
    }

    async fn get_test_meta(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        files: &[String],
    ) -> Result<Vec<TestFileMeta>, QaCatalogError> {
        self.services
            .plans
            .get_test_meta(ctx, repo_id, branch, files)
            .await
            .map_err(Into::into)
    }

    async fn list_universe(
        &self,
        ctx: &SecurityContext,
        product_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<Vec<UniverseTest>, QaCatalogError> {
        self.services
            .plans
            .list_universe(ctx, product_id, branch)
            .await
            .map_err(Into::into)
    }

    // ==================== Custom plans ====================

    async fn list_custom_plans(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<CustomPlan>, QaCatalogError> {
        self.services
            .custom_plans
            .list_custom_plans(ctx)
            .await
            .map_err(Into::into)
    }

    async fn get_custom_plan(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<CustomPlan, QaCatalogError> {
        self.services
            .custom_plans
            .get_custom_plan(ctx, id)
            .await
            .map_err(Into::into)
    }

    async fn create_custom_plan(
        &self,
        ctx: &SecurityContext,
        new: NewCustomPlan,
    ) -> Result<CustomPlan, QaCatalogError> {
        self.services
            .custom_plans
            .create_custom_plan(ctx, new)
            .await
            .map_err(Into::into)
    }

    async fn update_custom_plan(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        new: NewCustomPlan,
    ) -> Result<CustomPlan, QaCatalogError> {
        self.services
            .custom_plans
            .update_custom_plan(ctx, id, new)
            .await
            .map_err(Into::into)
    }

    async fn delete_custom_plan(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaCatalogError> {
        self.services
            .custom_plans
            .delete_custom_plan(ctx, id)
            .await
            .map_err(Into::into)
    }

    // ==================== Products ====================

    async fn list_products(&self, ctx: &SecurityContext) -> Result<Vec<Product>, QaCatalogError> {
        self.services
            .products
            .list_products(ctx)
            .await
            .map_err(Into::into)
    }

    async fn create_product(
        &self,
        ctx: &SecurityContext,
        new: NewProduct,
    ) -> Result<Product, QaCatalogError> {
        self.services
            .products
            .create_product(ctx, new)
            .await
            .map_err(Into::into)
    }

    async fn update_product(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        update: ProductUpdate,
    ) -> Result<Product, QaCatalogError> {
        self.services
            .products
            .update_product(ctx, id, update)
            .await
            .map_err(Into::into)
    }

    async fn delete_product(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaCatalogError> {
        self.services
            .products
            .delete_product(ctx, id)
            .await
            .map_err(Into::into)
    }

    // ==================== SSH keys ====================

    async fn list_ssh_keys(&self, ctx: &SecurityContext) -> Result<Vec<SshKey>, QaCatalogError> {
        self.services
            .ssh_keys
            .list_ssh_keys(ctx)
            .await
            .map_err(Into::into)
    }

    async fn create_ssh_key(
        &self,
        ctx: &SecurityContext,
        name: String,
        private_key_pem: String,
    ) -> Result<SshKey, QaCatalogError> {
        self.services
            .ssh_keys
            .create_ssh_key(ctx, name, private_key_pem)
            .await
            .map_err(Into::into)
    }

    async fn delete_ssh_key(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaCatalogError> {
        self.services
            .ssh_keys
            .delete_ssh_key(ctx, id)
            .await
            .map_err(Into::into)
    }

    // ==================== Bundles ====================

    async fn create_bundle(
        &self,
        ctx: &SecurityContext,
        req: BundleRequest,
    ) -> Result<TestBundle, QaCatalogError> {
        self.services
            .bundles
            .create_bundle(ctx, req)
            .await
            .map_err(Into::into)
    }

    async fn get_bundle_content(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Vec<u8>, QaCatalogError> {
        self.services
            .bundles
            .get_bundle_content(ctx, id)
            .await
            .map_err(Into::into)
    }
}
