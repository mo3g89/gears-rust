//! Local (in-process) implementation of `QaProductPluginResolverV1`.

use std::sync::Arc;

use async_trait::async_trait;
use qa_catalog_sdk::{QaCatalogError, QaProductPluginResolverV1};
use qa_product_sdk::QaProductPluginV1;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::repos::ProductsRepository;
use crate::domain::service::QaProductRegistry;

/// Adapts `QaProductRegistry` to the object-safe SDK trait, which is what
/// `qa-environments` and `qa-runs` consume.
///
/// A separate local client from [`QaCatalogLocalClient`], because it serves a
/// separate trait: `QaCatalogClientV1` has test doubles in two other gears,
/// and putting `plugin_for` on it would have made each of them stub a method
/// it never calls.
///
/// [`QaCatalogLocalClient`]: super::QaCatalogLocalClient
pub struct QaProductPluginResolverLocalClient<P: ProductsRepository> {
    registry: Arc<QaProductRegistry<P>>,
}

impl<P: ProductsRepository> QaProductPluginResolverLocalClient<P> {
    pub fn new(registry: Arc<QaProductRegistry<P>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl<P: ProductsRepository + 'static> QaProductPluginResolverV1
    for QaProductPluginResolverLocalClient<P>
{
    async fn plugin_for(
        &self,
        ctx: &SecurityContext,
        product_id: Uuid,
    ) -> Result<Arc<dyn QaProductPluginV1>, QaCatalogError> {
        self.registry
            .plugin_for(ctx, product_id)
            .await
            .map_err(Into::into)
    }
}
