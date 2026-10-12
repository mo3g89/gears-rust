//! `POST /qa/v1/runner-credentials` (finding #94, ADR-0008): a runner pod
//! fetches its run's credential files.
//!
//! **Internal: reachable in-cluster only.** The caller is a pod inside the
//! cluster. The route is not `.exposed()`, so it is not registered in the
//! gateway, and the UI nginx answers 404 for it rather than proxying it
//! (`qa-platform-ui/default.conf.template`, `location ^~
//! /qa/v1/runner-credentials`). Not `.exposed()` alone would not do: it only
//! affects the gateway and the `OpenAPI` flags, and the nginx's generic
//! `location /qa/v1/` proxies every path under it. **Anonymous** at the
//! toolkit layer, because a pod has no user: the projected `ServiceAccount`
//! token in `X-QA-Runner-Token` is the credential, proved by a `TokenReview`
//! in the domain service. Being anonymous, it cannot declare a license
//! requirement (`.anonymous()` moves the builder straight to `LicenseSet`).

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Environments";

pub(super) fn register_runner_credentials_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    router = OperationBuilder::post("/qa/v1/runner-credentials")
        .operation_id("qa_environments.runner_credentials")
        .summary("Hand a runner pod its run's credentials")
        .description(
            "Internal: not registered in the gateway and not proxied by the UI nginx, so \
             reachable in-cluster only. Called by the Argo runner pod with \
             its projected ServiceAccount token in X-QA-Runner-Token. Answers the credential \
             files of the environment the pod's live run targets. A missing token is a 401; \
             every refusal is the same 403; a dependency that cannot answer is a 503. See \
             ADR-0008.",
        )
        .tag(API_TAG)
        .anonymous()
        .handler(handlers::runner_credentials)
        .json_response_with_schema::<dto::RunnerCredentialsResp>(
            openapi,
            StatusCode::OK,
            "The files to write",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
}
