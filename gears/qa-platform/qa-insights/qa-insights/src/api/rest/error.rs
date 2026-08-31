//! Canonical error mapping: [`DomainError`] to `CanonicalError`.
//!
//! **The single place in this gear that decides an HTTP-visible error shape.**
//! `api/mod.rs` said so from Task 9, before there was a mapping to hold to it;
//! this is that file. Shape copied from
//! `qa-environments/src/api/rest/error.rs`.
//!
//! * **The `match` has no catch-all arm**, so adding a [`DomainError`] variant is
//!   a compile error here rather than a silent 500.
//!
//!   **Corrected in Task 16's fix round.** This bullet was introduced as "the one
//!   discipline qa-runs added and this gear adopts". It is not: qa-environments'
//!   own `match` (`qa-environments/src/api/rest/error.rs:24-98`) is already
//!   exhaustive with no catch-all, so the *rule* was inherited straight from the
//!   template. What qa-runs added is the **statement** of the rule — naming it in
//!   the header as a property to preserve rather than leaving it as something a
//!   later edit could quietly drop. That is worth inheriting too, and it is what
//!   this bullet actually does. qa-runs' own header makes the same slip about its
//!   own template; the point of writing this down is not to inherit it here.
//!
//!   **Two** variants are mapped below that nothing in this crate constructs
//!   yet, and they are mapped anyway for exactly the reason above: the task that
//!   raises one should find the boundary decision already taken and reviewed,
//!   not discover it as an unexplained 500. Named individually rather than
//!   counted, so that a `grep -rn 'DomainError::<variant>'` can settle the claim:
//!
//!   * [`DomainError::UnsupportedEgress`] — the notification router, Tasks
//!     36-39.
//!   * [`DomainError::IngestConflict`] — **neither loop.** Task 13 forecast it
//!     for the ingest path and did not need it; `domain/error.rs`' own list
//!     records why (the projection's delete-then-insert runs inside the caller's
//!     transaction, so a lost race is a rolled-back database error).
//!
//!   **[`DomainError::JiraNotConfigured`] left that list at Task 32**, which is
//!   both its first constructor and its first path to this boundary:
//!   `domain::service::jira::JiraService::check_status` raises it for a tenant
//!   with no config or a disabled one, and `infra::jira::oagw_client` raises it
//!   before it will provision egress for a disabled integration. It is the
//!   third variant to make that transition (after `SavedViewNameExists` and
//!   `SavedViewNotFound` at Task 28) and the count above is adjusted rather than
//!   the bullet merely deleted, because a stale count is what this section's own
//!   preamble is about.
//!
//!   One variant is deliberately **not** in that list, and was wrongly put
//!   there by earlier revisions of this paragraph:
//!
//!   * [`DomainError::BugNotFound`] — constructed since `ef878c22` at
//!     `infra/storage/jira_sea_repo.rs:189`, on the "the bug row is gone between
//!     the insert and the re-read of the same transaction" race in `upsert_bug`.
//!     That site's own comment calls the race unreachable in practice, which is
//!     a different claim from "no code constructs it" — and treating the two as
//!     one is what would let a Tasks 31-34 engineer skip the path without ever
//!     looking at it.
//!
//!   **The rule this paragraph keeps getting wrong**, stated so the next edit
//!   does not: "has no constructor", "is unreachable in practice" and "cannot
//!   reach a handler yet" are three different properties. Only the first belongs
//!   in the list above, and only a grep decides it.
//!
//!   [`DomainError::SavedViewNameExists`] used to be a second example here —
//!   "constructed, but nothing reaches this boundary yet" — and Task 28's
//!   `POST`/`PUT` handlers are exactly what makes that no longer true.
//!   [`DomainError::SavedViewNotFound`] is Task 28's own new variant, mapped
//!   and constructed in the same commit, so it never spent a task in either
//!   limbo.
//!
//! # The two mappings this file *owed* before it existed
//!
//! `domain/error.rs`' header names them, and the debt is discharged here:
//! [`DomainError::IngestConflict`] to `CanonicalError::Aborted` (409, retryable),
//! and [`DomainError::UnsupportedEgress`] to `CanonicalError::Unimplemented`
//! (501 — a working configuration and a missing adapter, which is why it is not
//! a `Validation`). `qa-insights-sdk/src/errors.rs` pointed at `domain/error.rs`,
//! which pointed here; the chain now terminates.
//!
//! # What a caller may read
//!
//! [`DomainError::Database`], [`DomainError::Internal`] and
//! [`DomainError::CorruptState`] carry text that originates in a database
//! driver, in this gear's internals, or in a persisted column. **None of it may
//! reach an HTTP body**: driver text names indexes, columns and key values, and
//! `CorruptState` carries the offending column's contents verbatim. Each is
//! logged at ERROR with the real cause and answered with the canonical internal
//! detail, which `toolkit-canonical-errors` supplies and this gear does not
//! choose.
//!
//! `opaque_internal` takes the [`DomainError`] rather than a message, so it
//! cannot be handed pre-formatted text by mistake — qa-runs' idiom, and the
//! reason it is worth copying is that the mistake it prevents is a one-word edit.

use toolkit::api::canonical_prelude::*;

use crate::domain::error::DomainError;

/// `qa_test_results` and `qa_test_case_results` — the projection this gear
/// exists to serve, and the resource the rebuild endpoint mutates.
///
/// Matches `domain::service::resources::TEST_RESULT`, deliberately: the PEP
/// resource type and the error resource type name the same thing, and a caller
/// denied on `qa.test_result` should be told about `cf.qa.insights.test_result`.
#[resource_error(gts_id!("cf.qa.insights.test_result.v1~"))]
struct TestResultResourceError;

/// `qa_saved_views`. Declared now because [`DomainError::SavedViewNameExists`]
/// already exists and the `match` below is exhaustive — a caller told a *test
/// result* already exists while naming a saved view would go looking for the
/// wrong row. Task 28 owns the endpoints.
#[resource_error(gts_id!("cf.qa.insights.saved_view.v1~"))]
struct SavedViewResourceError;

/// `qa_jira_bugs` and the two JIRA configuration singletons. Tasks 31-35.
#[resource_error(gts_id!("cf.qa.insights.jira_bug.v1~"))]
struct JiraBugResourceError;

/// `qa_notification_config`, `qa_notification_log` and `qa_run_notifications`.
/// Tasks 36-39; [`as_notification_error`] is Task 38's call-site renderer.
#[resource_error(gts_id!("cf.qa.insights.notification.v1~"))]
struct NotificationResourceError;

impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        let ce = match &e {
            // -- 404, not found ---------------------------------------------
            //
            // Asynchronous ingest makes this a *normal transient state*, not a
            // corruption: the run finished and the event has not been consumed
            // yet. `DomainError::RunNotIngested`'s own doc says a caller that
            // can render an empty history should prefer doing so — so this arm
            // exists for the callers that genuinely cannot, and 404 is the
            // honest answer for them. It is also what keeps the cross-tenant
            // existence oracle closed: a run in another tenant reaches this
            // gear as the same variant (`infra::clients::qa_runs`).
            DomainError::RunNotIngested { run_id } => TestResultResourceError::not_found(format!(
                "No results have been ingested for run {run_id}"
            ))
            .with_resource(run_id.to_string())
            .create(),

            DomainError::BugNotFound { key } => {
                JiraBugResourceError::not_found(format!("Bug {key} is not tracked"))
                    .with_resource(key.clone())
                    .create()
            }

            // Absent and another owner's are the same 404 — this variant's own
            // doc states the cross-owner existence oracle it closes, matching
            // `qa-runs`' `DomainError::ScheduleNotFound`.
            DomainError::SavedViewNotFound { id } => {
                SavedViewResourceError::not_found(format!("Saved view {id} not found"))
                    .with_resource(id.to_string())
                    .create()
            }

            // -- 409, already exists / aborted ------------------------------
            DomainError::SavedViewNameExists { name } => SavedViewResourceError::already_exists(
                format!("A saved view named '{name}' already exists in this scope"),
            )
            .with_resource(name.clone())
            .create(),

            // Retryable, and the reconciler retries whether or not the caller
            // does — which is why this is `Aborted` (409, "try again") rather
            // than an opaque 500.
            DomainError::IngestConflict => {
                TestResultResourceError::aborted("Concurrent ingest for the same run, retry")
                    .with_reason("INGEST_CONFLICT")
                    .create()
            }

            // -- 400, failed precondition / invalid argument ----------------
            //
            // Nothing was attempted, which is what separates this from a JIRA
            // call that failed: the tenant has no configuration at all.
            DomainError::JiraNotConfigured => JiraBugResourceError::failed_precondition()
                .with_precondition_violation(
                    "jira_configuration",
                    "JIRA is not configured for this tenant",
                    "NOT_CONFIGURED",
                )
                .create(),

            // **This said "every `Validation` this gear can raise today comes
            // from the rebuild window", and that was already false when it was
            // written.** `domain::error`'s own header lists `infra::storage::db::
            // odata_err` — `$filter`, `$orderby` and `cursor` on the two flat
            // collections — and `infra::storage::mapper` raises one too. Task 25a
            // adds `domain::analytics::query`, the overview query string's five
            // rules (`product_id`, `version`, `scope`, `group_by`, `plan_id`) plus
            // the build-tests drill-down's `build`. Corrected rather than extended,
            // because the false premise is what made the conclusion look
            // load-bearing.
            //
            // `TEST_RESULT` is nevertheless right for this arm's *unrendered*
            // callers — every one of them sits behind a test-result read or
            // write: `resources::TEST_RESULT` with `actions::LIST` is the grant
            // the overview and both collections require, and
            // `actions::REBUILD` the one the rebuild does.
            //
            // **The saved-view name was the candidate this comment named, and
            // Task 28 is the fix round.** `domain::service::saved_views` raises
            // `Validation` on `scope`, `name` and `plan_path`, none of which is
            // a test-result field, so its four handlers do not reach this arm
            // at all — they wrap the service call in `as_saved_view_error`
            // instead, which re-attributes `Validation` (and `Forbidden`) to
            // `SavedViewResourceError` before this blanket `match` ever sees
            // them. This arm stays the right default for every caller that
            // still reaches it unwrapped, and the rule for the *next* one is
            // unchanged: a `Validation` whose resource is not the test result
            // needs a call-site renderer, exactly as qa-runs' `Validation` arm
            // records. Branching on the field name would mis-attribute the
            // next field added on either side.
            DomainError::Validation { field, message } => {
                TestResultResourceError::invalid_argument()
                    .with_field_violation(field, message, "VALIDATION")
                    .create()
            }

            // A **grant** the write path cannot execute, not a denial — so 400
            // with a named precondition violation and deliberately not the 403
            // below. An operator who sees 403 goes and asks for the `rebuild`
            // grant they already hold; what actually needs changing is the shape
            // of the policy that answers it. `JiraNotConfigured` above sets the
            // precedent for surfacing a deployment-configuration precondition
            // this way.
            //
            // Attributed to the test-result resource because that is the only
            // resource whose write path raises it today. A second raiser needs a
            // call-site renderer, exactly as the `Validation` arm below records —
            // and unlike that one, this variant carries the PEP resource type, so
            // the renderer has something to switch on.
            DomainError::UnsupportedScope { resource } => {
                tracing::error!(
                    %resource,
                    "the authorization policy compiled to a scope this gear cannot execute; \
                     the operation was refused. See domain::service::refuse_scope_beyond_tenant",
                );
                TestResultResourceError::failed_precondition()
                    .with_precondition_violation(
                        "authorization_policy",
                        format!(
                            "The policy for '{resource}' grants this operation with a scope \
                             constraining more than the tenant, which this operation cannot \
                             honour. It must constrain owner_tenant_id only."
                        ),
                        "SCOPE_NOT_TENANT_ONLY",
                    )
                    .create()
            }

            // -- 403 --------------------------------------------------------
            DomainError::Forbidden => TestResultResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),

            // -- 501 --------------------------------------------------------
            //
            // Email, which design decision D10 defers: its config, routing,
            // dedupe and logging all ship and only the send does not. A
            // deployment reaching this has a *working* configuration and a
            // missing adapter, so it is not a 400 — there is nothing the caller
            // can change about the request.
            DomainError::UnsupportedEgress { channel } => NotificationResourceError::unimplemented(
                format!("This deployment has no adapter for the '{channel}' channel"),
            )
            .create(),

            // -- 500, opaque ------------------------------------------------
            DomainError::CorruptState { .. }
            | DomainError::Database(_)
            | DomainError::Internal(_) => opaque_internal(&e),
        };

        if let Some(diag) = ce.diagnostic() {
            tracing::debug!(diagnostic = %diag, "Canonical error diagnostic");
        }

        ce
    }
}

/// Log the real cause, answer with the canonical internal detail.
///
/// Takes the error rather than a message so no call site can pass it text it
/// formatted itself — the payloads of these three variants are precisely what
/// must not cross the boundary.
fn opaque_internal(e: &DomainError) -> CanonicalError {
    tracing::error!(error = ?e, "internal error reached the API boundary");
    CanonicalError::internal("An internal error occurred").create()
}

/// Render an error from a **saved-view** operation, attributing a `Validation`
/// or a `Forbidden` to the saved-view resource rather than to the test result.
///
/// This is the call-site renderer this file's own header names as still
/// owed: the blanket `match` above has no per-call-site information, so its
/// `Validation` and `Forbidden` arms are attributed to
/// [`TestResultResourceError`] unconditionally — right for every raiser that
/// existed before Task 28, wrong for `domain::service::saved_views`'s four
/// operations, whose `scope`/`name`/`plan_path` refusals and PDP denials are
/// about `qa.saved_view`. `qa-runs`' `as_schedule_error` is the precedent this
/// follows, field for field: every one of Task 28's four handlers wraps its
/// service call in this rather than using a bare `?`.
///
/// `SavedViewNameExists` and `SavedViewNotFound` already carry the saved-view
/// resource in the blanket `match`, so the fall-through arm leaves them
/// (and `Database`) untouched — the same "everything else passes through"
/// guarantee `as_schedule_error`'s own tests pin.
pub(crate) fn as_saved_view_error(e: DomainError) -> CanonicalError {
    match e {
        DomainError::Validation { field, message } => SavedViewResourceError::invalid_argument()
            .with_field_violation(field, message, "VALIDATION")
            .create(),
        DomainError::Forbidden => SavedViewResourceError::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create(),
        other => other.into(),
    }
}

/// Render an error from a **JIRA** operation — settings or bug registry —
/// attributing a `Validation` or a `Forbidden` to the JIRA resource rather
/// than to the test result.
///
/// [`as_saved_view_error`]'s reason, and this call site has one of its own.
/// Task 32's settings surface raises `Validation` naming `url` or
/// `poll_interval_seconds`; Task 33's registry raises it naming `plan_path`
/// (R85's "together or not at all" rule on `GET /qa/v1/jira/open-bugs`) — none
/// of the three is a test-result field, and every PDP denial on either surface
/// is about `qa.jira_config` or `qa.jira_bug`. `JiraNotConfigured` and
/// `RunNotIngested` already carry their own resource in the blanket `match`
/// (the latter is [`TestResultResourceError`] on purpose —
/// [`JiraService::file_bugs`](crate::domain::service::jira::JiraService::file_bugs)'s
/// own doc says why an unprojected run is that error and not a JIRA one), so
/// the fall-through arm leaves both of them (and `Database`) untouched.
///
/// One function for both surfaces rather than a settings-only
/// `as_jira_config_error` (this function's Task 32 name) plus a bug-only
/// second one: [`JiraBugResourceError`] already covers "`qa_jira_bugs` and
/// the two JIRA configuration singletons" in one type (this file's own
/// declaration, above), so a second function would only be routing two
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

/// Render an error from a **notification** operation — settings, the log, or
/// the test/preview surfaces — attributing a `Validation` or a `Forbidden`
/// to the notification resource rather than to the test result.
///
/// [`as_saved_view_error`]'s reason. `domain::service::notify` raises
/// `Validation` naming `event` (an unrecognized token),
/// `slack_enabled`/`slack_webhook_credstore_ref` (a scheduled-run test asked
/// for before Slack is configured) — none of which is a test-result field —
/// and every PDP denial on this surface is about `qa.notification_config`.
/// [`DomainError::UnsupportedEgress`] already carries its own resource in
/// the blanket `match` above, so the fall-through arm leaves it (and
/// `Database`) untouched; it is the one variant `NotifyService::send_test`
/// can also raise that this function does not need to re-attribute.
pub(crate) fn as_notification_error(e: DomainError) -> CanonicalError {
    match e {
        DomainError::Validation { field, message } => NotificationResourceError::invalid_argument()
            .with_field_violation(field, message, "VALIDATION")
            .create(),
        DomainError::Forbidden => NotificationResourceError::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create(),
        other => other.into(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Locks the category **and** the HTTP status of every arm above.
    //!
    //! Both, because a regression that keeps the status and changes the category
    //! still changes the RFC-9457 `type` a client matches on, and one that keeps
    //! the category and changes the status is invisible in a `matches!`. Every
    //! variant is covered: the exhaustive `match` guarantees an arm *exists* for
    //! a new variant, and these tests are what say the arm is the right one.

    use uuid::Uuid;

    use super::{CanonicalError, DomainError};

    #[test]
    fn a_run_with_no_projection_is_404_not_found() {
        let ce: CanonicalError = DomainError::RunNotIngested {
            run_id: Uuid::new_v4(),
        }
        .into();
        assert_eq!(ce.status_code(), 404);
        assert!(matches!(ce, CanonicalError::NotFound { .. }), "{ce:?}");
    }

    #[test]
    fn an_untracked_bug_is_404_not_found() {
        let ce: CanonicalError = DomainError::BugNotFound {
            key: "VHP-319".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 404);
        assert!(matches!(ce, CanonicalError::NotFound { .. }), "{ce:?}");
    }

    #[test]
    fn a_duplicate_saved_view_name_is_409_already_exists() {
        let ce: CanonicalError = DomainError::SavedViewNameExists {
            name: "flaky".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::AlreadyExists { .. }), "{ce:?}");
    }

    /// One of the two mappings `domain/error.rs`' header owed this file.
    /// Retryable, so 409 `Aborted` rather than an opaque 500 — a client that saw
    /// 500 would not know it may simply try again.
    #[test]
    fn an_ingest_conflict_is_409_aborted() {
        let ce: CanonicalError = DomainError::IngestConflict.into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::Aborted { .. }), "{ce:?}");
    }

    #[test]
    fn an_unconfigured_jira_is_400_failed_precondition() {
        let ce: CanonicalError = DomainError::JiraNotConfigured.into();
        assert_eq!(ce.status_code(), 400);
        assert!(
            matches!(ce, CanonicalError::FailedPrecondition { .. }),
            "{ce:?}"
        );
    }

    /// The rebuild endpoint's refused-window answer.
    #[test]
    fn a_refused_window_is_400_invalid_argument() {
        let ce: CanonicalError = DomainError::Validation {
            field: "to".to_owned(),
            message: "must be strictly after 'from'".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 400);
        assert!(
            matches!(ce, CanonicalError::InvalidArgument { .. }),
            "{ce:?}"
        );
    }

    /// A grant the write path cannot execute is **400 with a named precondition
    /// violation**, not the 403 below.
    ///
    /// Both halves are asserted because the distinction is the whole point: an
    /// operator seeing 403 concludes their grant is missing and asks for one they
    /// already hold, while the shape of the policy that answers it goes
    /// unexamined. The test also pins that this is *not* `PermissionDenied`, so a
    /// later simplification into the `Forbidden` arm fails here.
    #[test]
    fn an_unexecutable_scope_is_400_failed_precondition_and_not_a_denial() {
        let ce: CanonicalError = DomainError::UnsupportedScope {
            resource: "qa.test_result",
        }
        .into();
        assert_eq!(ce.status_code(), 400);
        assert!(
            matches!(ce, CanonicalError::FailedPrecondition { .. }),
            "expected FailedPrecondition, not a denial: {ce:?}"
        );
    }

    #[test]
    fn a_refusal_is_403_permission_denied() {
        let ce: CanonicalError = DomainError::Forbidden.into();
        assert_eq!(ce.status_code(), 403);
        assert!(
            matches!(ce, CanonicalError::PermissionDenied { .. }),
            "{ce:?}"
        );
    }

    /// The other mapping this file owed. 501, not 400: the caller's request is
    /// fine and the deployment is what is missing a piece.
    #[test]
    fn an_unsupported_egress_channel_is_501_unimplemented() {
        let ce: CanonicalError = DomainError::UnsupportedEgress {
            channel: "email".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 501);
        assert!(matches!(ce, CanonicalError::Unimplemented { .. }), "{ce:?}");
    }

    /// The three opaque variants, and the property that matters is not the
    /// status: it is that the payload each carries does **not** appear in what
    /// the caller is handed.
    #[test]
    fn the_three_internal_variants_are_500_and_disclose_nothing() {
        let secrets = [
            (
                DomainError::Database(
                    "duplicate key value violates unique constraint \
                     \"qa_test_results_tenant_run_file_key\""
                        .to_owned(),
                ),
                "qa_test_results_tenant_run_file_key",
            ),
            (
                DomainError::Internal("connect to 10.4.2.9:5432 refused".to_owned()),
                "10.4.2.9",
            ),
            (
                DomainError::CorruptState {
                    what: "saved_view.scope",
                    id: Uuid::nil(),
                    value: "not-a-scope".to_owned(),
                },
                "not-a-scope",
            ),
        ];

        for (err, secret) in secrets {
            let ce: CanonicalError = err.into();
            assert_eq!(ce.status_code(), 500);
            assert!(matches!(ce, CanonicalError::Internal { .. }), "{ce:?}");

            let rendered = format!("{ce:?}");
            assert!(
                !rendered.contains(secret),
                "'{secret}' reached the boundary in {rendered}"
            );
        }
    }

    /// An id no view in the caller's owner scope matched is 404 not found —
    /// Task 28's own new variant, mapped and constructed in the same commit.
    #[test]
    fn an_unmatched_saved_view_id_is_404_not_found() {
        let ce: CanonicalError = DomainError::SavedViewNotFound { id: Uuid::nil() }.into();
        assert_eq!(ce.status_code(), 404);
        assert!(matches!(ce, CanonicalError::NotFound { .. }), "{ce:?}");
    }

    /// **`as_saved_view_error` re-attributes exactly `Validation` and
    /// `Forbidden`, and both status and body carry the change.**
    ///
    /// Without this renderer, `domain::service::saved_views`'s `scope`
    /// refusal would fall through to the blanket `match`'s `Validation` arm
    /// and name `cf.qa.insights.test_result.v1~` — a caller fixing a saved
    /// view's `scope` parameter told they need a *test-result* grant.
    #[test]
    fn a_saved_views_validation_is_re_attributed_to_the_saved_view_resource() {
        use toolkit::api::canonical_prelude::Problem;

        let err = DomainError::Validation {
            field: "scope".to_owned(),
            message: "scope must be 'all' or 'plan'".to_owned(),
        };
        let wrapped = super::as_saved_view_error(err);
        let body = serde_json::to_string(
            &Problem::from_error(&wrapped).expect("a problem must serialize"),
        )
        .expect("a problem must serialize");

        assert_eq!(wrapped.status_code(), 400, "{body}");
        assert!(body.contains("cf.qa.insights.saved_view.v1~"), "{body}");
        assert!(!body.contains("cf.qa.insights.test_result.v1~"), "{body}");
        assert!(body.contains("scope"), "{body}");
    }

    /// The same re-attribution for a PDP denial — a caller denied on
    /// `qa.saved_view` must not be pointed at `qa.test_result`.
    #[test]
    fn a_saved_views_denial_is_re_attributed_to_the_saved_view_resource() {
        use toolkit::api::canonical_prelude::Problem;

        let wrapped = super::as_saved_view_error(DomainError::Forbidden);
        let body = serde_json::to_string(
            &Problem::from_error(&wrapped).expect("a problem must serialize"),
        )
        .expect("a problem must serialize");

        assert_eq!(wrapped.status_code(), 403, "{body}");
        assert!(body.contains("cf.qa.insights.saved_view.v1~"), "{body}");
        assert!(!body.contains("cf.qa.insights.test_result.v1~"), "{body}");
    }

    /// **Everything else passes through untouched** — in particular
    /// `SavedViewNameExists` and `SavedViewNotFound`, which already carry the
    /// right resource from the blanket `match` and must not be re-wrapped
    /// into a different shape.
    #[test]
    fn as_saved_view_error_leaves_every_other_variant_untouched() {
        let direct: CanonicalError = DomainError::SavedViewNameExists {
            name: "Regressions".to_owned(),
        }
        .into();
        let wrapped = super::as_saved_view_error(DomainError::SavedViewNameExists {
            name: "Regressions".to_owned(),
        });
        assert_eq!(wrapped.status_code(), direct.status_code());
        assert_eq!(wrapped.status_code(), 409);

        let id = Uuid::from_u128(0x42);
        let direct: CanonicalError = DomainError::SavedViewNotFound { id }.into();
        let wrapped = super::as_saved_view_error(DomainError::SavedViewNotFound { id });
        assert_eq!(wrapped.status_code(), direct.status_code());
        assert_eq!(wrapped.status_code(), 404);
    }

    /// **`as_jira_error` re-attributes `Validation` from the JIRA registry
    /// path (Task 33's R85 pairing rule) to the JIRA resource, not the
    /// test-result one the blanket `match` would otherwise pick.**
    ///
    /// Without this renderer, `domain::service::jira::JiraService::open_bugs`'s
    /// `plan_path` refusal would fall through to the blanket `match` and name
    /// `cf.qa.insights.test_result.v1~` — a caller fixing the plan pair on a
    /// JIRA endpoint told they need a *test-result* grant.
    #[test]
    fn a_jira_registry_validation_is_re_attributed_to_the_jira_resource() {
        use toolkit::api::canonical_prelude::Problem;

        let err = DomainError::Validation {
            field: "plan_path".to_owned(),
            message: "repo_id and plan_path must be supplied together, or not at all".to_owned(),
        };
        let wrapped = super::as_jira_error(err);
        let body = serde_json::to_string(
            &Problem::from_error(&wrapped).expect("a problem must serialize"),
        )
        .expect("a problem must serialize");

        assert_eq!(wrapped.status_code(), 400, "{body}");
        assert!(body.contains("cf.qa.insights.jira_bug.v1~"), "{body}");
        assert!(!body.contains("cf.qa.insights.test_result.v1~"), "{body}");
        assert!(body.contains("plan_path"), "{body}");
    }

    /// The same re-attribution for a PDP denial on `qa.jira_bug`.
    #[test]
    fn a_jira_registry_denial_is_re_attributed_to_the_jira_resource() {
        use toolkit::api::canonical_prelude::Problem;

        let wrapped = super::as_jira_error(DomainError::Forbidden);
        let body = serde_json::to_string(
            &Problem::from_error(&wrapped).expect("a problem must serialize"),
        )
        .expect("a problem must serialize");

        assert_eq!(wrapped.status_code(), 403, "{body}");
        assert!(body.contains("cf.qa.insights.jira_bug.v1~"), "{body}");
        assert!(!body.contains("cf.qa.insights.test_result.v1~"), "{body}");
    }

    /// **Everything else passes through untouched** — in particular
    /// `RunNotIngested`, which [`JiraService::file_bugs`] raises for an
    /// unprojected run and which must keep naming the test-result resource
    /// from the blanket `match` rather than being re-wrapped as a JIRA one.
    ///
    /// [`JiraService::file_bugs`]: crate::domain::service::jira::JiraService::file_bugs
    #[test]
    fn as_jira_error_leaves_run_not_ingested_untouched() {
        let run_id = Uuid::from_u128(0x42);
        let direct: CanonicalError = DomainError::RunNotIngested { run_id }.into();
        let wrapped = super::as_jira_error(DomainError::RunNotIngested { run_id });
        assert_eq!(wrapped.status_code(), direct.status_code());
        assert_eq!(wrapped.status_code(), 404);

        let body = serde_json::to_string(
            &toolkit::api::canonical_prelude::Problem::from_error(&wrapped)
                .expect("a problem must serialize"),
        )
        .expect("a problem must serialize");
        assert!(
            body.contains("cf.qa.insights.test_result.v1~"),
            "an unprojected run must still name the test-result resource, not the JIRA one: \
             {body}",
        );
    }
}
