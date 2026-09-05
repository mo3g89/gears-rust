//! Handlers for the bug registry — `GET /qa/v1/jira/open-bugs` and
//! `POST /qa/v1/jira/bugs`. Task 33.
//!
//! # A module of its own, not folded into `handlers::settings`
//!
//! Both files sit under the same `qa_jira_*` table family and share
//! [`crate::api::rest::error::as_jira_error`], but `settings` is the
//! *connection* — the URL, the credential reference, the poll cadence a
//! tenant configures once — and this is the *registry* the connection feeds:
//! the bugs a tenant's failures have actually produced. `handlers::settings`'
//! own header names the two extensions it still expects (Task 35's poller
//! pair, Task 38's four notification routes); none of those is this table
//! family either, and grouping "everything JIRA" into one file would mean a
//! reader looking for the filing path scrolling past four settings endpoints
//! to find it.
//!
//! # No scope, no validation, no status-code decision
//!
//! `handlers::analytics`' and `handlers::saved_views`' discipline, restated
//! for a third resource: [`crate::domain::service::jira::JiraService`] owns
//! the authorization decision (`bug_scope`, over
//! [`crate::domain::service::resources::JIRA_BUG`]), the R85 pairing rule on
//! `open_bugs`, and the R80/R84 decisions inside `file_bugs`. Each handler
//! here is a decode, a call and a render — the same shape
//! [`crate::api::rest::handlers::settings`] already uses for this service's
//! other two operations.

use std::sync::Arc;

use axum::Extension;
use axum::extract::Query;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{FileJiraBugsReq, JiraBugDto, JiraBugFilingDto, OpenBugsQuery};
use crate::api::rest::error::as_jira_error;
use crate::gear::ConcreteAppServices;

/// `GET /qa/v1/jira/open-bugs` — every open bug, or one plan's.
///
/// `api_open_bugs` (`manager/src/routes/settings.rs:634-651`). `query` is
/// **not** skipped on the span, unlike
/// [`crate::api::rest::handlers::saved_views::list_saved_views`]'s `scope`:
/// both fields here are already axum-typed (`Option<Uuid>`, `Option<String>`)
/// rather than free-form caller text with no bound, so recording them ahead of
/// [`JiraService::open_bugs`]'s own pairing check carries nothing an attacker
/// controls beyond what a `Uuid` or a path string already is.
///
/// [`JiraService::open_bugs`]: crate::domain::service::jira::JiraService::open_bugs
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_open_bugs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<OpenBugsQuery>,
) -> ApiResult<Json<Vec<JiraBugDto>>> {
    let bugs = svc
        .jira
        .open_bugs(&ctx, query.repo_id, query.plan_path.as_deref())
        .await
        .map_err(as_jira_error)?;
    Ok(Json(bugs.into_iter().map(JiraBugDto::from).collect()))
}

/// `POST /qa/v1/jira/bugs` — file (or find) a bug for a run's failed tests.
///
/// `api_create_jira_ticket` (`manager/src/routes/settings.rs:581-631`). A
/// partial success is still `200` with fewer entries than failed tests —
/// [`JiraService::file_bugs`]'s own doc states exactly what is swallowed and
/// what is not, and this handler makes no attempt to tell the two apart on
/// the wire: legacy's own response carries no such signal either.
///
/// # `req` is skipped on the span
///
/// [`crate::api::rest::handlers::settings::update_jira_settings`]'s reason: an
/// unvalidated body is caller-controlled text with no bound this handler
/// enforces. Nothing here is a credential-store reference, but `test_name` is
/// still free text this handler has not looked at yet.
///
/// [`JiraService::file_bugs`]: crate::domain::service::jira::JiraService::file_bugs
#[tracing::instrument(skip(svc, ctx, req))]
pub async fn file_bugs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<FileJiraBugsReq>,
) -> ApiResult<Json<Vec<JiraBugFilingDto>>> {
    let filed = svc
        .jira
        .file_bugs(&ctx, req.run_id, req.test_name.as_deref())
        .await
        .map_err(as_jira_error)?;
    Ok(Json(
        filed.into_iter().map(JiraBugFilingDto::from).collect(),
    ))
}
