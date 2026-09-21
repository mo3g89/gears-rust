//! Operator routes. `OperationBuilder` shape copied from
//! `qa-environments/src/api/rest/routes/variables.rs`, as the plan's Task 16
//! specifies.

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};

pub(super) fn register_admin_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // POST /qa/v1/insights/rebuild — replay a closed window from qa-runs.
    //
    // **`.authenticated()` is not the authorization**, and that is worth saying
    // where a reviewer will look for it: the admin requirement is enforced by
    // the `PolicyEnforcer` decision `ReconcileService::rebuild` compiles, on
    // `qa.test_result` / `rebuild`. Every sibling gear in this subsystem puts
    // authorization in the service and not in the route chain, for the reason
    // `domain::service` states — a fresh scope is derived immediately before
    // every repository call, which a route-level check cannot do.
    //
    // 403 is therefore a documented outcome for an authenticated caller, and it
    // is the one an operator without the grant will see.
    OperationBuilder::post("/qa/v1/insights/rebuild")
        .operation_id("qa_insights.rebuild")
        .summary("Replay a time window from qa-runs")
        .description(
            "Re-read every run that finished in [from, to) from qa-runs and rewrite this \
             gear's projection of it, run by run. For a window whose results are known to be \
             wrong or missing. Requires the gts.cf.qa.insights.test_result.v1~/rebuild grant. The window is \
             half-open, so adjoining windows neither skip a run nor replay one, and `to` must \
             be strictly after `from`. It does not move the reconciler's watermark, in either \
             direction: this is a repair for a known window, not a reset. It deletes nothing \
             it does not immediately rewrite, so rows for runs qa-runs no longer has are left \
             standing. The window is walked in pages under a fixed budget, so a window wider \
             than one page is replayed in full rather than truncated: check `complete` on the \
             response, and when it is false post `resume_from` back as `from` with the same \
             `to` until it is true.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::RebuildReq>(openapi, "The window to replay")
        .handler(handlers::rebuild)
        .json_response_with_schema::<dto::RebuildOutcomeDto>(
            openapi,
            StatusCode::OK,
            "What the rebuild did",
        )
        // 400 covers a window that is not strictly forward and a `from`/`to`
        // that is not RFC 3339. There is no 404: an empty window is a
        // successful no-op, which is what makes the endpoint safe to re-run.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
