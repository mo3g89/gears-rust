//! The port that turns a run's target environment into the plugin that knows
//! how to reach it.
//!
//! Named types are `Uuid`, `SecurityContext` and
//! `qa_product_sdk::QaProductPluginV1` — never `qa-catalog`'s SDK client, and
//! never a `ClientHub`. [`crate::infra::product_plugin`] is the only impl that
//! knows either exists.
//!
//! # The same shape `qa-environments` declares, and why it is a second
//! declaration rather than a shared one
//!
//! `qa-environments::domain::ports::ProductPluginPort` is identical, and both
//! wrap the same `qa_catalog_sdk::QaProductPluginResolverV1`. Sharing one would
//! mean this gear depending on that gear's *domain*, which is the direction the
//! layering forbids — a port belongs to the layer that needs it, and each gear
//! needs its own for its own reasons. What is genuinely shared is the resolver
//! trait in `qa-catalog-sdk` and the plugin trait in `qa-product-sdk`; both are
//! contract crates, which is what contract crates are for.
//!
//! The variants differ from that gear's, and the difference is the point: an
//! unresolvable plugin is a *recorded observation* there and a **refused
//! dispatch** here, so this enum carries what a dispatch failure needs.

use std::sync::Arc;

use async_trait::async_trait;
use qa_product_sdk::QaProductPluginV1;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Why a run's product plugin could not be resolved.
///
/// Deliberately not a `String`: each variant carries fixed text, for the reason
/// `qa_product_sdk::observation::PluginFailure` makes `detail` a
/// `&'static str` — nothing formatted crosses this boundary, and a dispatch
/// failure's message is persisted on the run row and rendered to an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginUnavailable {
    /// The target environment names no product, so there is nothing to resolve
    /// a plugin from. `Environment::product_id` is still `Option<Uuid>` until
    /// Task 20 makes the column `NOT NULL`.
    NoProduct,
    /// The product is not visible to this caller, names no plugin, or names one
    /// this binary does not register. `qa-catalog` distinguishes all three in
    /// its own log and message and projects them onto one error; this gear does
    /// not re-split what it cannot see.
    Unresolvable,
    /// The resolver itself could not be reached — the deployment is missing
    /// `qa-catalog`, or its gear failed before registering.
    ResolverAbsent,
}

impl PluginUnavailable {
    /// Fixed, operator-facing text, recorded as the run's failure.
    ///
    /// Each one names the action that fixes it, because a run that failed to
    /// dispatch tells an operator nothing else: the alternative these replaced
    /// was a run stuck with a mount pointing at an environment nobody could
    /// say how to reach.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::NoProduct => {
                "this run's target environment names no product, so no product plugin \
                 can say how to reach it: assign the environment a product"
            }
            Self::Unresolvable => {
                "this run's target environment belongs to a product that names no \
                 product plugin, or names one this deployment does not carry: check \
                 the product's plugin binding"
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
    /// `ctx` is enforced on the far side: the product read is tenant-scoped, so
    /// a product outside the caller's tenant reads as absent rather than
    /// forbidden.
    async fn plugin_for(
        &self,
        ctx: &SecurityContext,
        product_id: Uuid,
    ) -> Result<Arc<dyn QaProductPluginV1>, PluginUnavailable>;
}
