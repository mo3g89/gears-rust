//! The one capability other gears need from `qa-catalog`'s product-plugin
//! binding: turning a product id into a live plugin object.

use std::sync::Arc;

use async_trait::async_trait;
use qa_product_sdk::QaProductPluginV1;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::QaCatalogError;

/// Resolve a product's plugin.
///
/// # Why this is its own trait, and not a method on [`QaCatalogClientV1`]
///
/// [`QaCatalogClientV1`] has three test doubles outside this SDK's own gear
/// (in `qa-insights` and twice in `qa-runs`), and each of them would have had
/// to stub a plugin method it never calls. A capability-per-trait is also how
/// every other cross-gear capability in this workspace is consumed —
/// credstore, types-registry and authz are each one trait with one job — so a
/// consumer that only needs to resolve a plugin depends on exactly that.
///
/// Registered **unscoped** in the `ClientHub` by `qa-catalog`'s `init`, beside
/// its `QaCatalogClientV1` local client:
///
/// ```ignore
/// let resolver = hub.get::<dyn QaProductPluginResolverV1>()?;
/// let plugin = resolver.plugin_for(&ctx, product_id).await?;
/// ```
///
/// The *plugins* are the scoped registrations (one per GTS instance id); this
/// resolver is the single thing that knows which of them a given product is
/// bound to, because the binding is a column on `qa_products` and products
/// are `qa-catalog`'s aggregate.
///
/// [`QaCatalogClientV1`]: crate::QaCatalogClientV1
#[async_trait]
pub trait QaProductPluginResolverV1: Send + Sync {
    /// The plugin bound to `product_id`, resolved through the `ClientHub`
    /// under the product's stored `plugin_instance_id`.
    ///
    /// `ctx` is enforced: the product read is tenant-scoped, so a product
    /// outside the caller's tenant is not resolvable and reads as absent.
    ///
    /// # Errors
    ///
    /// - `NotFound` when no product with `product_id` is visible to `ctx`.
    /// - `NotFound` when the product names no plugin, or names one that is
    ///   not registered in this binary (the two are distinguished in the
    ///   gear's log and error message — a missing binding is a
    ///   misconfiguration, an unregistered id is a gear that was not linked
    ///   in).
    /// - `PermissionDenied` when policy denies the caller the product read.
    async fn plugin_for(
        &self,
        ctx: &SecurityContext,
        product_id: Uuid,
    ) -> Result<Arc<dyn QaProductPluginV1>, QaCatalogError>;
}
