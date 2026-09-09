//! The bug registry routes — `GET /qa/v1/jira/open-bugs` and
//! `POST /qa/v1/jira/bugs`. Task 33.
//!
//! `handlers::jira`'s header says why this is a module of its own rather than
//! a third and fourth operation on `routes::settings`.
//!
//! # `POST /qa/v1/jira/bugs` answers `200`, not Task 28's `201`
//!
//! Task 28's convention — create returns `201` with a `Location` header —
//! does not fit an endpoint whose response is a list of zero, one or many
//! filed-or-found bugs with no single created resource for a `Location` to
//! name, and R84's own text says so directly: "a partial success is a `200`
//! with fewer entries". Legacy's `api_create_jira_ticket` answers `200` with
//! the same `Vec<JiraCreateResponse>` shape (`manager/src/routes/settings.rs:630`,
//! the bare `Ok(Json(responses))`), so this is not a new choice, only a
//! confirmed one.
//!
//! # `POST /qa/v1/jira/bugs` needs three grants, not one — fix round 1,
//! # Important 4
//!
//! An earlier revision of this description named only `qa.jira_bug/create`.
//! [`crate::domain::service::jira::JiraService::file_bugs`] also reads
//! `qa_test_results`/`qa_test_case_results` under a separately-compiled
//! `qa.test_result/list` scope (controller ruling R87) and reads the tenant's
//! JIRA settings under `qa.jira_config/get` (via
//! [`crate::domain::service::jira::JiraService::active_config`]) before
//! filing starts — an operator who granted exactly what the old text said
//! would see every request refused. The description below names all three,
//! and states that the config-read denial is a bulk `403` rather than a
//! per-test swallow, matching
//! [`JiraService::file_bugs`](crate::domain::service::jira::JiraService::file_bugs)'s
//! own "What is never swallowed" doc.

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};

pub(super) fn register_jira_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/jira/open-bugs
    router = OperationBuilder::get("/qa/v1/jira/open-bugs")
        .operation_id("qa_insights.list_open_bugs")
        .summary("List open JIRA bugs")
        .description(
            "Every bug this tenant's registry still considers open (status = 'Open'), or - when \
             repo_id and plan_path are both supplied - only those filed against that plan. The \
             two must be supplied together; one without the other is a 400. This is the same \
             (repo_id, plan_path) identity the runner's skip list is built from: a bug that \
             suppresses a test at launch is the same bug this endpoint lists, which is why the \
             match is exact rather than the analytics drill-downs' single plan_path matched \
             across every repository a caller's scope admits. A bug the poller has since \
             resolved leaves this list even though POST /qa/v1/jira/bugs' local re-file dedupe \
             may still recognise it - see that endpoint's own description. Requires the \
             qa.jira_bug/list grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "repo_id",
            false,
            "The plan's repository. Required together with plan_path.",
            "string",
        )
        .query_param_typed(
            "plan_path",
            false,
            "The plan's path within its repository. Required together with repo_id.",
            "string",
        )
        .handler(handlers::list_open_bugs)
        .json_array_response_with_schema::<dto::JiraBugDto>(
            openapi,
            StatusCode::OK,
            "The open bugs, narrowed to one plan when both query parameters are given",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/jira/bugs
    OperationBuilder::post("/qa/v1/jira/bugs")
        .operation_id("qa_insights.file_jira_bugs")
        .summary("File or find JIRA bugs for a run's failed tests")
        .description(
            "File a JIRA issue for every FAILED test of run_id, or - when test_name is given - \
             for just that one test if it failed. A test already registered locally, or one \
             JIRA's own search already tracks, is not re-filed: the entry for it carries \
             created: false and the existing key. A test this call cannot file for (JIRA is not \
             configured or disabled, the test has no plan identity in this run's projection, or \
             the JIRA call itself failed) is silently dropped from the response rather than \
             failing the whole request - a partial success is a 200 with fewer entries than \
             failed tests, matching the system being replaced's own per-test error handling. \
             404 means run_id has no ingested results at all, which is distinct from a run with \
             no failures (a 200 with an empty list). Requires three grants: qa.jira_bug/create \
             for the registry write, qa.test_result/list to read the run's own results, and \
             qa.jira_config/get to read the tenant's JIRA settings. Unlike the per-test failures \
             above, a denial on any of the three refuses the whole request with a 403 rather \
             than being swallowed - authorization is checked once, before any test is filed.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::FileJiraBugsReq>(
            openapi,
            "The run to file against, and an optional test_name to narrow to",
        )
        .handler(handlers::file_bugs)
        .json_array_response_with_schema::<dto::JiraBugFilingDto>(
            openapi,
            StatusCode::OK,
            "One entry per test this call successfully filed or found a bug for",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
