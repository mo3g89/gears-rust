//! The port that turns an environment's product into the plugin that observes
//! it.
//!
//! Named types are `Uuid`, `SecurityContext` and
//! `qa_product_sdk::QaProductPluginV1` — never `qa-catalog`'s SDK client, and
//! never a `ClientHub`. `infra::product_plugin` is the only impl that knows
//! either exists, exactly as `infra::runner_secret_writer` is the only impl
//! that knows Kubernetes does.

use std::sync::Arc;

use async_trait::async_trait;
use qa_product_sdk::QaProductPluginV1;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Why a product's plugin could not be resolved.
///
/// Deliberately **not** an error type and deliberately not a `String`: an
/// unresolvable plugin is a *fact about the environment*, recorded on its row
/// as a failed observation the way an unresolvable kubeconfig already is, so
/// what the caller needs is fixed text an operator can act on. Each variant
/// carries no payload for the reason `qa_product_sdk::observation::PluginFailure`
/// makes `detail` a `&'static str`: nothing formatted crosses this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginUnavailable {
    /// The environment names no product, so there is nothing to resolve a
    /// plugin from. `Environment::product_id` is still `Option<Uuid>` until
    /// Task 20 makes the column `NOT NULL`.
    NoProduct,
    /// The product is not visible to this caller, names no plugin, or names
    /// one this binary does not register. `qa-catalog` distinguishes all
    /// three in its own log and message and projects them onto one error;
    /// this gear does not re-split what it cannot see.
    Unresolvable,
    /// The resolver itself could not be reached — the deployment is missing
    /// `qa-catalog`, or its gear failed before registering.
    ResolverAbsent,
}

impl PluginUnavailable {
    /// Fixed, operator-facing text, persisted as this environment's
    /// observation failure.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::NoProduct => {
                "this environment names no product, so no product plugin can observe it: \
                 assign it a product"
            }
            Self::Unresolvable => {
                "this environment's product names a product plugin this deployment does \
                 not carry: check the product's plugin binding"
            }
            Self::ResolverAbsent => {
                "the product-plugin resolver is not available in this deployment: \
                 qa-catalog is not running beside this gear"
            }
        }
    }
}

/// Resolve the plugin bound to a product.
#[async_trait]
pub trait ProductPluginPort: Send + Sync {
    /// The plugin bound to `product_id`, or why there is none.
    ///
    /// `ctx` is enforced on the far side: the product read is tenant-scoped,
    /// so a product outside the caller's tenant reads as absent rather than
    /// forbidden.
    async fn plugin_for(
        &self,
        ctx: &SecurityContext,
        product_id: Uuid,
    ) -> Result<Arc<dyn QaProductPluginV1>, PluginUnavailable>;
}
