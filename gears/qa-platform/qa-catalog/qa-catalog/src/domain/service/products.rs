//! Product service.
//!
//! PRD `cpt-cf-qa-fr-catalog-products` ("manage products") is covered for
//! products (create/list/update/delete). Products no longer own a curated
//! version→branch mapping — legacy dropped `product_versions` in VHP-319:
//! runs are driven by branch selection, not by a curated table.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_catalog_sdk::{NewProduct, Product, ProductUpdate};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, info, instrument};
use uuid::Uuid;

use super::ProductPluginPresence;
use super::validation::{validate_name, validate_plugin_instance_id};
use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::repos::ProductsRepository;

/// Product service.
#[domain_model]
pub struct ProductsService<P: ProductsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<P>,
    policy_enforcer: PolicyEnforcer,
    /// Whether a submitted `plugin_instance_id` resolves in this process.
    ///
    /// A narrow port rather than the resolver, for the reasons on
    /// [`ProductPluginPresence`] — chiefly that a create has no `product_id`
    /// to resolve *from*, and that the resolver holds this service's own
    /// repository.
    plugin_presence: Arc<dyn ProductPluginPresence>,
}

impl<P: ProductsRepository> ProductsService<P> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<P>,
        policy_enforcer: PolicyEnforcer,
        plugin_presence: Arc<dyn ProductPluginPresence>,
    ) -> Self {
        Self {
            db,
            repo,
            policy_enforcer,
            plugin_presence,
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

    #[instrument(skip(self, ctx, new), fields(name = %new.name))]
    pub async fn create_product(
        &self,
        ctx: &SecurityContext,
        new: NewProduct,
    ) -> Result<Product, DomainError> {
        info!("Creating product");

        validate_name("name", &new.name)?;
        // **Create requires a binding, structurally.**
        // `NewProduct::plugin_instance_id` is a plain `String` since Task 20,
        // so there is nothing to check here -- the state this used to refuse
        // cannot be constructed. The wire's `Option` dies one layer out, at
        // `TryFrom<CreateProductReq>`, which is where the message that names
        // `GET /qa/v1/product-plugins` lives (finding FW-1).
        //
        // `update_product` keeps its `Option`, and that asymmetry is
        // deliberate: there `None` means "leave the stored binding alone"
        // (ruling D-18), a third state a create does not have.
        validate_plugin_instance_id(Some(&new.plugin_instance_id))?;

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PRODUCT, actions::CREATE, None)
            .await?;

        // **After the authorization check, not before it** (review finding
        // m-2). The validators above inspect the *request*, which the caller
        // already knows; this one queries **process state** -- which plugins
        // this deployment registers. Run first, it answered an unauthorized
        // caller with `Validation` for an unregistered id and `Forbidden` for
        // a registered one, which is an oracle for deployment topology. The
        // same set is listed to an authorized caller by
        // `GET /qa/v1/product-plugins`, so nothing is lost by asking after.
        self.require_registered_plugin(&new.plugin_instance_id)?;

        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();

        let product = self.repo.create(&conn, &scope, tenant_id, new).await?;

        info!("Successfully created product with id={}", product.id);
        Ok(product)
    }

    /// Replace a product's mutable fields — the whole of [`ProductUpdate`] —
    /// as a full replace, so `folder: None` moves the product back to the
    /// root. **`plugin_instance_id: None` does NOT unbind** — it leaves the
    /// current binding alone (ruling D-18; corrected at the Phase E review,
    /// finding FW-3, which found three copies of the old sentence still
    /// standing).
    #[instrument(skip(self, ctx, update), fields(product_id = %id, name = %update.name))]
    pub async fn update_product(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        update: ProductUpdate,
    ) -> Result<Product, DomainError> {
        info!("Updating product");

        validate_name("name", &update.name)?;
        validate_plugin_instance_id(update.plugin_instance_id.as_deref())?;

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PRODUCT, actions::UPDATE, Some(id))
            .await?;

        // A rebind must resolve for create's reason, and asks **after**
        // authorization for create's reason too (finding m-2). `None` still
        // means "leave the binding alone" (ruling D-18) and reaches neither
        // check.
        if let Some(instance_id) = update.plugin_instance_id.as_deref() {
            self.require_registered_plugin(instance_id)?;
        }

        let conn = self.db.conn()?;
        self.repo
            .update(&conn, &scope, id, update)
            .await?
            .ok_or(DomainError::NotFound { id })
    }

    /// Refuse a binding that names no plugin this process registers.
    ///
    /// **At the API, not at first use** — Task 20 Step 3's own words, and
    /// ruling F-10. Accepting an id that resolves to nothing produces a
    /// product whose every environment is silently unobservable and
    /// undispatchable until somebody notices, which is the failure shape
    /// **D6** ("every product names a plugin; there is no fallback path")
    /// exists to prevent.
    ///
    /// The message names the endpoint that lists valid values rather than just
    /// refusing, because the realistic cause is a typo or a plugin gear that
    /// has not shipped yet, and both are actionable from that list.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming the field. The id **is** echoed: it
    /// is operator-supplied configuration, not credential material, and an
    /// operator who mistyped a 100-character GTS id cannot fix it without
    /// seeing what arrived.
    fn require_registered_plugin(&self, instance_id: &str) -> Result<(), DomainError> {
        if self.plugin_presence.is_registered(instance_id) {
            return Ok(());
        }
        Err(DomainError::Validation {
            field: "plugin_instance_id".to_owned(),
            message: format!(
                "no product plugin registered in this deployment answers to `{instance_id}`: \
                 list the available ones at GET /qa/v1/product-plugins"
            ),
        })
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
