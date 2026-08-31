//! Product service.
//!
//! PRD `cpt-cf-qa-fr-catalog-products` ("manage products") is covered for
//! products (create/list/update/delete). Products no longer own a curated
//! version→branch mapping — legacy dropped `product_versions` in VHP-319:
//! runs are driven by branch selection, not by a curated table.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_catalog_sdk::Product;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, info, instrument};
use uuid::Uuid;

use super::validation::validate_name;
use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::repos::ProductsRepository;

/// Product service.
#[domain_model]
pub struct ProductsService<P: ProductsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<P>,
    policy_enforcer: PolicyEnforcer,
}

impl<P: ProductsRepository> ProductsService<P> {
    pub fn new(db: Arc<DbProvider>, repo: Arc<P>, policy_enforcer: PolicyEnforcer) -> Self {
        Self {
            db,
            repo,
            policy_enforcer,
        }
    }
}

// Business logic methods
impl<P: ProductsRepository> ProductsService<P> {
    #[instrument(skip(self, ctx))]
    pub async fn list_products(&self, ctx: &SecurityContext) -> Result<Vec<Product>, DomainError> {
        debug!("Listing products");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PRODUCT, actions::LIST, None)
            .await?;

        let conn = self.db.conn()?;
        self.repo.list(&conn, &scope).await
    }

    /// Distinct non-null folder names across the caller's visible products,
    /// ascending. There is no dedicated repository query — the tenant's
    /// product set is small (p1), so this PEP-scopes a LIST read and
    /// de-duplicates in code.
    #[instrument(skip(self, ctx))]
    pub async fn list_product_folders(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<String>, DomainError> {
        debug!("Listing distinct product folders");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PRODUCT, actions::LIST, None)
            .await?;

        let conn = self.db.conn()?;
        let products = self.repo.list(&conn, &scope).await?;

        let folders: std::collections::BTreeSet<String> =
            products.into_iter().filter_map(|p| p.folder).collect();
        Ok(folders.into_iter().collect())
    }

    #[instrument(skip(self, ctx, name, key, description, folder), fields(name = %name))]
    pub async fn create_product(
        &self,
        ctx: &SecurityContext,
        name: String,
        key: String,
        description: String,
        folder: Option<String>,
    ) -> Result<Product, DomainError> {
        info!("Creating product");

        validate_name("name", &name)?;

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PRODUCT, actions::CREATE, None)
            .await?;

        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();

        let product = self
            .repo
            .create(&conn, &scope, tenant_id, name, key, description, folder)
            .await?;

        info!("Successfully created product with id={}", product.id);
        Ok(product)
    }

    /// Replace a product's mutable fields (`name`, `key`, `description`,
    /// `folder`) — full replace, so `folder: None` moves the product back to
    /// the root.
    #[instrument(skip(self, ctx, name, key, description, folder), fields(product_id = %id, name = %name))]
    pub async fn update_product(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        name: String,
        key: String,
        description: String,
        folder: Option<String>,
    ) -> Result<Product, DomainError> {
        info!("Updating product");

        validate_name("name", &name)?;

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PRODUCT, actions::UPDATE, Some(id))
            .await?;

        let conn = self.db.conn()?;
        self.repo
            .update(&conn, &scope, id, name, key, description, folder)
            .await?
            .ok_or(DomainError::NotFound { id })
    }

    /// Delete a product.
    #[instrument(skip(self, ctx), fields(product_id = %id))]
    pub async fn delete_product(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        info!("Deleting product");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PRODUCT, actions::DELETE, Some(id))
            .await?;

        let conn = self.db.conn()?;
        let deleted = self.repo.delete(&conn, &scope, id).await?;
        if !deleted {
            return Err(DomainError::NotFound { id });
        }

        info!("Successfully deleted product");
        Ok(())
    }
}
