use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::header;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::gear::ConcreteAppServices;

/// Download a test bundle's bytes (`application/gzip`).
///
/// Bundle *creation* is deliberately **not** exposed over REST — it is an
/// SDK-only operation for the qa-runs dispatcher at launch time (see
/// `qa_catalog_sdk::QaCatalogClientV1::create_bundle`). An expired bundle
/// reads exactly like a missing one (404).
#[tracing::instrument(skip(svc, ctx), fields(bundle.id = %id))]
pub async fn download_bundle(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let bytes = svc.bundles.get_bundle_content(&ctx, id).await?;
    Ok(([(header::CONTENT_TYPE, "application/gzip")], bytes).into_response())
}
