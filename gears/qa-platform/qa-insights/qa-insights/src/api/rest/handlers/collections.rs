//! The two flat `OData` collections. Task 17.
//!
//! Both handlers are three lines, and every line that is *not* here is
//! deliberate: no scope, no validation of the `OData` query, no page-size
//! decision, no status code. The scope is derived by
//! [`ResultsService`](crate::domain::service::results::ResultsService)
//! immediately before the read; the query's field allow-list, sort validation and
//! page clamp belong to the repository, next to the entity and the index they are
//! about; and `?` resolves the error through
//! [`crate::api::rest::error`].
//!
//! # The `OData` extractor is where a malformed query is refused
//!
//! `OData` is an axum extractor
//! (`libs/toolkit/src/api/odata.rs:303-319`), so a `$filter` that will not
//! parse, a `$top=0`, or a cursor **and** an `$orderby` together are rejected
//! before either function below is entered — as a `CanonicalError` the extractor
//! builds itself, not through this gear's mapping. What reaches the repository is
//! a syntactically valid query whose *field names* are still unchecked; those are
//! checked against the field enum inside `paginate_odata`, and come back as a
//! `DomainError::Validation` naming `$filter` or `$orderby`.

use std::sync::Arc;

use axum::Extension;

use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit::api::response::JsonPage;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{TestCaseResultDto, TestResultDto};
use crate::gear::ConcreteAppServices;

/// `GET /qa/v1/test-results` — one page of file-level outcomes.
///
/// The `OData` query is handed to the service untouched. A `$filter` cannot widen
/// what this returns: the service resolves the caller's `AccessScope` from the
/// policy enforcer first and the repository composes the two on a select that
/// cannot be un-scoped.
#[tracing::instrument(skip(svc, ctx, query))]
pub async fn list_test_results(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<TestResultDto>> {
    let page = svc.results.list_results(&ctx, &query).await?;
    Ok(Json(page.map_items(TestResultDto::from)))
}

/// `GET /qa/v1/test-case-results` — one page of function-level outcomes.
///
/// [`list_test_results`]' sibling, under the same resource type and action.
#[tracing::instrument(skip(svc, ctx, query))]
pub async fn list_test_case_results(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<TestCaseResultDto>> {
    let page = svc.results.list_case_results(&ctx, &query).await?;
    Ok(Json(page.map_items(TestCaseResultDto::from)))
}
