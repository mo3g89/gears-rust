use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::header;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;

use crate::api::rest::dto::BundleDownloadQuery;
use crate::gear::ConcreteAppServices;

/// Download a test bundle's bytes (`application/gzip`).
///
/// Bundle *creation* is deliberately **not** exposed over REST — it is an
/// SDK-only operation for the qa-runs dispatcher at launch time (see
/// `qa_catalog_sdk::QaCatalogClientV1::create_bundle`). An expired bundle
/// reads exactly like a missing one (404).
///
/// # No `Extension<SecurityContext>` — not because nothing checks the caller,
/// # but because the check is a signature, not a session
///
/// This route is registered `.anonymous().exposed()`
/// (`api::rest::routes::bundles`, whose header carries the whole argument).
/// `api-gateway`'s auth middleware resolves that to `AuthRequirement::None`
/// and runs this handler with an anonymous `SecurityContext` inserted, without
/// reading the `Authorization` header at all. An anonymous context carries no
/// tenant this read could use, so this handler never extracts one: the tenant
/// comes off the **descriptor row**, inside
/// `BundlesService::get_bundle_content_signed`, and the `sig` query parameter
/// is what proves the caller may have it.
///
/// The caller is an Argo workflow pod, which the Argo adapter handed a
/// `TEST_BUNDLE_URL` with this tag already in its query string. It replaced a
/// `client_credentials` exchange performed *inside* that pod against a
/// deployment-wide confidential client — see the route module's header for
/// what that credential could reach and why deleting it was the point.
#[tracing::instrument(skip(svc, query), fields(bundle.id = %id))]
pub async fn download_bundle(
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Query(query): Query<BundleDownloadQuery>,
) -> ApiResult<impl IntoResponse> {
    let bytes = svc.bundles.get_bundle_content_signed(id, &query.sig).await?;
    Ok(([(header::CONTENT_TYPE, "application/gzip")], bytes).into_response())
}
