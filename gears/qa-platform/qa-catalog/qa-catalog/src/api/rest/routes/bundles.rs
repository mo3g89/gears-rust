use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::handlers;

const API_TAG: &str = "QA Catalog";

pub(super) fn register_bundle_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/test-bundles/{id} - Download bundle bytes.
    //
    // Binary (non-JSON) response: `text_response` is the builder's
    // custom-media-type registration (a raw media type with `schema: None`) —
    // the same call file-parser uses for its `text/markdown` stream. The name
    // says "text", but it is media-type agnostic and emits exactly the
    // schema-less `application/gzip` content block this endpoint needs.
    //
    // Bundle creation is SDK-only (qa-runs at launch); see
    // `handlers::download_bundle`.
    router = OperationBuilder::get("/qa/v1/test-bundles/{id}")
        .operation_id("qa_catalog.download_bundle")
        .summary("Download a test bundle")
        .description(
            "Download the tar.gz bytes of an ephemeral test bundle; an \
             expired bundle reads exactly like a missing one (404)",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Bundle UUID")
        .handler(handlers::download_bundle)
        .text_response(StatusCode::OK, "Bundle tar.gz bytes", "application/gzip")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
