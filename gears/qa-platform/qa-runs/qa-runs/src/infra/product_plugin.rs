//! The [`ProductPluginPort`](crate::domain::ports::product_plugin::ProductPluginPort)
//! adapter: `qa-catalog`'s resolver, reached through
//! the `ClientHub`.
//!
//! # Why the resolver is looked up per call rather than in `Gear::init`
//!
//! An eager `client_hub().get::<dyn QaProductPluginResolverV1>()` in `init`
//! would make this gear **fail to boot** in a deployment where `qa-catalog`
//! initialises after it, or is absent. Lazy resolution makes the same
//! deployment fail only where the capability is actually needed — one run's
//! dispatch, with the run's own error saying so — which is design §4.2's
//! principle and the ruling (D-14) `qa-catalog` already applied to its
//! types-registry client for the identical reason. `qa-environments`'
//! `infra::product_plugin` is the same adapter for the same trait, arrived at
//! the same way.
//!
//! The hub lookup is a `TypeId` hash probe, so doing it per dispatch costs
//! nothing measurable next to the catalog reads and the bundle builds around
//! it.
//!
//! # ADR-0001
//!
//! This file names `qa-catalog-sdk` and `qa-product-sdk`; neither depends on
//! `kube` or `k8s-openapi`, so this gear's Kubernetes surface is unchanged —
//! it remains the `argo` adapter alone, behind its non-default feature.

use std::sync::Arc;

use async_trait::async_trait;
use qa_catalog_sdk::QaProductPluginResolverV1;
use qa_product_sdk::QaProductPluginV1;
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;
use tracing::warn;
use uuid::Uuid;

use crate::domain::ports::product_plugin::{PluginUnavailable, ProductPluginPort};

/// Resolves a product's plugin through `qa-catalog`'s
/// [`QaProductPluginResolverV1`], which `qa-catalog` registers **unscoped** in
/// the `ClientHub` at its own `init`.
pub struct HubProductPluginResolver {
    client_hub: Arc<ClientHub>,
}

impl HubProductPluginResolver {
    #[must_use]
    pub const fn new(client_hub: Arc<ClientHub>) -> Self {
        Self { client_hub }
    }
}

#[async_trait]
impl ProductPluginPort for HubProductPluginResolver {
    async fn plugin_for(
        &self,
        ctx: &SecurityContext,
        product_id: Uuid,
    ) -> Result<Arc<dyn QaProductPluginV1>, PluginUnavailable> {
        let resolver = match self.client_hub.get::<dyn QaProductPluginResolverV1>() {
            Ok(resolver) => resolver,
            Err(error) => {
                warn!(
                    %error,
                    "qa-runs: no product-plugin resolver is registered in the ClientHub; \
                     this deployment has no qa-catalog beside this gear"
                );
                return Err(PluginUnavailable::ResolverAbsent);
            }
        };

        // `QaCatalogError`'s own message is not returned or persisted: the far
        // side already logged which of the three causes it was (no such
        // product, no plugin bound, plugin not registered), and this gear
        // records the port's fixed text instead. `%error` here is a log line,
        // not a stored or published value — and it is qa-catalog's own
        // canonical error, never anything derived from a credential.
        resolver.plugin_for(ctx, product_id).await.map_err(|error| {
            warn!(
                %product_id,
                %error,
                "qa-runs: this run's product plugin could not be resolved"
            );
            PluginUnavailable::Unresolvable
        })
    }
}
