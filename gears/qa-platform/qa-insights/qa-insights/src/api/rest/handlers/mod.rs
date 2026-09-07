//! REST request handlers — thin delegation to the domain services.
//!
//! A handler extracts the request, calls into
//! [`crate::gear::ConcreteAppServices`] and converts the result to a DTO. **No
//! business logic, no database access, no PEP call, and no status-code
//! decision**: the first three live in `crate::domain::service`, and the fourth
//! lives in [`crate::domain::error`], reached by `?` through
//! `From<DomainError> for CanonicalError` for every handler except
//! [`saved_views`]'s four, which route through
//! `crate::api::rest::error::as_saved_view_error` instead — that function's own
//! doc says why the blanket mapping is not enough for this resource.
//!
//! * [`admin`] — the operator rebuild, Task 16.
//! * [`collections`] — the two flat `OData` collections, Task 17.
//! * [`dashboard`] — the dashboard aggregate (Task 18) and its coverage view
//!   (Task 19).
//! * [`analytics`] — the overview and its build-tests drill-down (Task 25b),
//!   the export (Task 26), and the three plan drill-downs (Task 27).
//! * [`saved_views`] — saved-view CRUD, Task 28.
//! * [`settings`] — the tenant-settings surfaces, Task 32. Founded with the
//!   JIRA pair; Task 35 extends it with the poller pair and Task 38 with the
//!   four notification routes, which that module's header states so neither is a
//!   surprise.
//! * [`collect`] — the collect trigger and the runner's report, Task 30.
//!   [`collect::report_collect_count`] is this crate's first `.public()`
//!   handler and does not extract `Extension<SecurityContext>` at all — see
//!   its own doc for why.
//! * [`jira`] — the bug registry, Task 33: `GET /qa/v1/jira/open-bugs` and
//!   `POST /qa/v1/jira/bugs`. That module's header says why this is not a
//!   third and fourth operation on [`settings`].

mod admin;
mod analytics;
mod collect;
mod collections;
mod dashboard;
mod jira;
mod saved_views;
mod settings;

pub(crate) use admin::rebuild;
pub(crate) use analytics::{
    analytics_build_tests, analytics_export, analytics_overview, analytics_plan_builds,
    analytics_plan_test_history, analytics_plan_tests,
};
pub(crate) use collect::{report_collect_count, trigger_collect};
pub(crate) use collections::{list_test_case_results, list_test_results};
pub(crate) use dashboard::{dashboard, dashboard_coverage};
pub(crate) use jira::{file_bugs, list_open_bugs};
pub(crate) use saved_views::{
    create_saved_view, delete_saved_view, list_saved_views, update_saved_view,
};
pub(crate) use settings::{
    get_jira_poller_settings, get_jira_settings, get_notification_log, get_notification_settings,
    preview_notification, test_notification, update_jira_poller_settings, update_jira_settings,
    update_notification_settings,
};
