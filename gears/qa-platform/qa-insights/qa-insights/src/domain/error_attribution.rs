//! **Where a domain error gets its resource attribution**, for the refusals
//! whose resource type the blanket `DomainError -> CanonicalError` mapping
//! cannot see.
//!
//! # Why this is a domain module and not part of `api::rest::error`
//!
//! It was part of it, and that put a *domain* module — `domain::local_client`,
//! moved here from `infra/` by the plugin rework — in the position of
//! importing `crate::api::rest::error` to attribute its own errors. A local
//! client is an in-process call that never touches HTTP; its error attribution
//! must not come from the HTTP layer, and `api` is a transport over `domain`
//! rather than the other way round. Review findings #15, #16.
//!
//! So [`as_jira_error`] and the resource type it raises live here, and
//! `api::rest::error` re-exports the function — a REST handler still imports
//! its error rendering from exactly one module, and the three surfaces that
//! share this attribution decision share one definition of it rather than
//! three `gts_id`s that could disagree.
//!
//! # The fall-through arm, and why the blanket mapping had to move too
//!
//! [`as_jira_error`] ends in `other => other.into()`, which resolves to
//! `impl From<DomainError> for CanonicalError`. Moving this function out of
//! `api::rest::error` therefore did **not**, on its own, remove the edge the
//! findings were about: the impl was still declared in the transport layer, so
//! every fall-through here was a domain call into it. That impl now lives in
//! [`crate::domain::error`], beside the enum it maps, together with the three
//! resource types it raises; `api::rest::error` imports two of them back for
//! its own call-site renderers.
//!
//! **`crate::no_api_in_domain_tests` cannot see any of this.** It is a text
//! scan over imports; a trait impl is resolved by coherence and leaves no
//! `use` line to find. It pins the imports and nothing more, which is worth
//! stating because an earlier revision of this header cited it as the thing
//! keeping the property — a guard that could not have contradicted the claim
//! it was offered as evidence for.
//!
//! **Only the JIRA attribution moved.** `as_saved_view_error`,
//! `as_notification_error` and their resource types stayed in
//! `api::rest::error`: nothing in `domain` calls them, and moving a mapper
//! whose only caller is a handler would be motion without a reason. The rule
//! this file exists to satisfy is about the direction of an import, not about
//! collecting every mapper in one place.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

/// `qa_jira_bugs` and the two JIRA configuration singletons. Tasks 31-35.
///
/// `pub(crate)` for `domain::error`, whose blanket mapping raises this
/// same type for `JiraBugNotTracked` and `JiraNotConfigured` -- one
/// `gts_id` per resource, not two that could disagree.
#[resource_error(gts_id!("cf.qa.insights.jira_bug.v1~"))]
pub(crate) struct JiraBugResourceError;

/// Render an error from a **JIRA** operation — settings or bug registry —
/// attributing a `Validation` or a `Forbidden` to the JIRA resource rather
/// than to the test result.
///
/// [`as_saved_view_error`](crate::api::rest::error::as_saved_view_error)'s
/// reason, and this call site has one of its own.
/// Task 32's settings surface raises `Validation` naming `url` or
/// `poll_interval_seconds`; Task 33's registry raises it naming `plan_path`
/// (R85's "together or not at all" rule on `GET /qa/v1/jira/open-bugs`) — none
/// of the three is a test-result field, and every PDP denial on either surface
/// is about `qa.jira_config` or `qa.jira_bug`. `JiraNotConfigured` and
/// `RunNotIngested` already carry their own resource in the blanket `match`
/// (the latter is `TestResultResourceError`, `domain::error`'s own, on
/// purpose —
/// [`JiraService::file_bugs`](crate::domain::service::jira::JiraService::file_bugs)'s
/// own doc says why an unprojected run is that error and not a JIRA one), so
/// the fall-through arm leaves both of them (and `Database`) untouched.
///
/// One function for both surfaces rather than a settings-only
/// `as_jira_config_error` (this function's Task 32 name) plus a bug-only
/// second one: [`JiraBugResourceError`] already covers "`qa_jira_bugs` and
/// the two JIRA configuration singletons" in one type (its declaration,
/// above), so a second function would only be routing two
/// identical `match` arms to the same resource through two names.
pub(crate) fn as_jira_error(e: DomainError) -> CanonicalError {
    match e {
        DomainError::Validation { field, message } => JiraBugResourceError::invalid_argument()
            .with_field_violation(field, message, "VALIDATION")
            .create(),
        DomainError::Forbidden => JiraBugResourceError::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create(),
        other => other.into(),
    }
}
