//! **Where a REST handler imports its error rendering from**, and the two
//! call-site renderers that belong to a handler rather than to a mapping.
//!
//! The blanket mapping itself — `impl From<DomainError> for CanonicalError`,
//! the three resource types it raises and the `opaque_internal` redaction
//! helper — is in [`crate::domain::error`], next to the enum it maps; the JIRA
//! attribution is in [`crate::domain::error_attribution`]. Both modules carry
//! the reasoning that used to be in this header, beside the code it is about.
//!
//! # Why the mapping is not here
//!
//! It was, and that put `domain::error_attribution::as_jira_error`'s
//! `other => other.into()` — a *domain* module — in the position of calling a
//! function in the transport layer. `domain::local_client` is an in-process
//! call that never touches HTTP; `api` is a transport over `domain` rather than
//! the other way round. Review findings #15, #16.
//!
//! **The `use` line was not the finding.** An earlier round deleted
//! `domain::error_attribution`'s `use crate::api::rest::error::...` and left the
//! edge standing, because a trait impl is resolved by coherence rather than by
//! an import: `other.into()` went on resolving into this file.
//! `crate::no_api_in_domain_tests` is a text scan over imports and could never
//! have contradicted that — it pins the imports, and the impl's location is what
//! pins the rest.
//!
//! # What stayed, and why
//!
//! [`as_saved_view_error`] and [`as_notification_error`]. Nothing in `domain`
//! calls either, and moving a renderer whose only caller is a handler would be
//! motion without a reason. The rule this layering exists to satisfy is about
//! the direction of a dependency, not about collecting every mapper in one
//! place.

use toolkit::api::canonical_prelude::*;

use crate::domain::error::DomainError;

// The blanket `DomainError -> CanonicalError` mapping and the three resource
// types it raises live in `domain::error`, and the JIRA attribution in
// `domain::error_attribution`: `domain::local_client` and `as_jira_error`'s
// fall-through both reach that mapping, and a domain module must not depend on
// the transport layer (review findings #15, #16). The types are imported back
// rather than re-declared so the mapping and the two call-site renderers below
// raise the same `gts_id` per resource rather than two that could disagree, and
// everything a handler needs is re-exported so this module stays the one place
// a REST handler imports error rendering from.
use crate::domain::error::{NotificationResourceError, SavedViewResourceError};
pub(crate) use crate::domain::error_attribution::as_jira_error;

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
    use crate::domain::service::resources;

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
            resource: resources::TEST_RESULT_NAME,
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
                DomainError::database(
                    "duplicate key value violates unique constraint \
                     \"qa_test_results_tenant_run_file_key\"",
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
