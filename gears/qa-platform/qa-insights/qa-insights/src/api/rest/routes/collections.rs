//! The two flat `OData` collections. Task 17, and per D7 the only `OData` in this
//! gear.
//!
//! # Three declarations per route carry the whole contract
//!
//! `.with_odata_filter::<F>()` and `.with_odata_orderby::<F>()` publish the
//! filterable and sortable field sets in the `OpenAPI` document, and `F` is the
//! **same** type the repository translates with. That is what stops the advertised
//! `$filter` fields and the SQL-translatable ones drifting apart, and
//! `infra::storage::odata`'s header states it as that module's purpose — together
//! with the one place it does *not* hold: `with_odata_orderby` ignores
//! `is_orderable`, so the published sort list is wider than the accepted one, and
//! the caveat in the first description below is the only warning a caller gets.
//! `json_response_with_schema::<Page<..>>` is the third: a `Page<T>` rather than a
//! `Vec<T>`, because every `Vec<_>` collapses onto one `OpenAPI` component name
//! and a collision **aborts the process** at startup — qa-runs records that
//! measurement, and `routes::tests` is what makes it visible to `cargo test`.
//!
//! # Why the page-size numbers are literals in the descriptions
//!
//! `PAGE_LIMITS` is a `const` in `infra::storage::db`, so a description cannot
//! interpolate it — `OperationBuilder::description` takes a `&str`. The numbers
//! are therefore written out, once per route, exactly as qa-runs writes them into
//! its own runs-listing description.
//!
//! **So 200 and 500 appear in the constant and again in each description below**,
//! and nothing checks the copies against the constant.
//! `db::tests::the_page_limits_match_the_subsystem_convention` pins the constant
//! alone; a change to it has to be carried into both strings by hand. Recorded
//! rather than hidden — it is worse than a format argument and better than an
//! undocumented ceiling, and the field *lists* in the same descriptions are
//! checked, by
//! `api::rest::routes::tests::each_description_names_every_field_its_enum_admits`.

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};
use crate::infra::storage::odata::{TestCaseResultsField, TestResultsField};

pub(super) fn register_collection_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/test-results — the file-level collection.
    //
    // `.authenticated()` is not the authorization: `ResultsService` compiles a
    // `PolicyEnforcer` decision on `qa.test_result` / `list` immediately before
    // the read, as every service in this subsystem does. 403 is therefore a
    // documented outcome for an authenticated caller.
    router = OperationBuilder::get("/qa/v1/test-results")
        .operation_id("qa_insights.list_test_results")
        .summary("List file-level test results")
        .description(
            "One page of the file-level test outcomes visible to the caller. Supports OData \
             $filter, $orderby and cursor pagination; the page size defaults to 200 and is \
             clamped to 500. The filterable fields are the indexed ones: id, run_id, \
             test_file, test_name and run_finished_at, and no others. These tables hold \
             millions of rows and a filter on an unindexed column would be a full scan. \
             run_finished_at can be filtered but not sorted, because it is null for a run \
             still in progress and a null sort key truncates pagination. The default order \
             is by row id descending, which is stable but arbitrary. There is no \
             chronological sort, because no index covers created_at. Express recency as a \
             filter instead: `$filter=run_finished_at ge 2026-08-01T00:00:00Z`. The sortable \
             fields are id, run_id, test_file and test_name.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_test_results)
        .json_response_with_schema::<toolkit_odata::Page<dto::TestResultDto>>(
            openapi,
            StatusCode::OK,
            "One page of file-level test results",
        )
        // The same enum `ResultsRepository::list_page` translates with.
        .with_odata_filter::<TestResultsField>()
        .with_odata_orderby::<TestResultsField>()
        // 400 is reachable and is the caller's: a `$filter` or `$orderby` naming
        // a field outside the allow-list, a `$top` of zero, or a cursor from a
        // different sort order. There is no 404 — an empty page is a successful
        // answer, and asynchronous ingest makes "no rows yet" a normal state
        // rather than a missing resource.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/test-case-results — the function-level collection.
    OperationBuilder::get("/qa/v1/test-case-results")
        .operation_id("qa_insights.list_test_case_results")
        .summary("List function-level test case results")
        .description(
            "One page of the per-function test case outcomes visible to the caller: the \
             xfail/xpass/skip/pass/fail rows parsed from the runner's TEST_CASE markers, one \
             per test function per run. Same paging as /qa/v1/test-results: OData $filter, \
             $orderby and cursor pagination, page size 200 by default and clamped to 500. \
             The filterable fields are a different set, because this table carries different \
             indexes: id, run_id, test_file and status. status is filterable here and not on \
             the file-level collection. Matching it is exact and case-sensitive against \
             whatever the runner reported, because nothing normalises that value; a \
             mismatched case therefore answers an empty page rather than an error. There is \
             no time field at all, because case rows carry no denormalized run columns, so a \
             time window is expressed by taking run ids from /qa/v1/test-results and \
             filtering run_id here. The default order is by row id descending; all four \
             fields are sortable.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_test_case_results)
        .json_response_with_schema::<toolkit_odata::Page<dto::TestCaseResultDto>>(
            openapi,
            StatusCode::OK,
            "One page of function-level test case results",
        )
        .with_odata_filter::<TestCaseResultsField>()
        .with_odata_orderby::<TestCaseResultsField>()
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
