//! [`RunsReader`] and, since Task 30, [`RunsLauncher`], both over
//! `qa_runs_sdk::QaRunsClientV1`.
//!
//! # It translates errors and nothing else
//!
//! Every method here is one SDK call plus error translation — a plain
//! `map_err` for four of the five, and a `match` for
//! [`QaRunsReader::get_schedule_notifications`] (Task 38, R105) only because
//! `NotFound` there folds into `Ok(None)` rather than an error, per that
//! port method's own doc. That is still deliberate translation and not a
//! decision: the port's five methods were chosen to match qa-runs' contract
//! exactly — same bounds, same ordering guarantee, same "a run still
//! running is never returned" — precisely so that this file could hold no
//! behaviour worth a bug. Anything that looks like a decision belongs on
//! the far side of the port, in [`crate::domain::service`], where a fake can
//! reach it.
//!
//! # One struct, two traits — not two structs
//!
//! [`RunsLauncher`] is implemented on [`QaRunsReader`] itself rather than on a
//! second `QaRunsLauncher` type: both traits wrap the identical
//! `Arc<dyn QaRunsClientV1>`, neither needs state the other does not already
//! hold, and `gear::init` resolves the client from `ClientHub` exactly once
//! either way. [`RunsLauncher::launch_collect`] shares [`on_subject`], the
//! same subject-level translation [`RunsReader::list_recent_runs`] and
//! [`RunsReader::list_runs_finished_since`] use — a launch failure is a
//! failure about the *caller*, not about a particular run's existence, so
//! [`on_run`]'s `NotFound`-is-`RunNotIngested` arm has no place here.
//!
//! # The error translation is the security-relevant part
//!
//! `QaRunsError` is `toolkit_canonical_errors::CanonicalError`, and
//! [`RunsReader`]'s own header fixes the contract this file implements: *"a
//! `not_found` from it means 'the run does not exist **or** is not visible to
//! this context' — the two are deliberately indistinguishable there, which is
//! what closes the cross-tenant existence oracle."*
//!
//! So `NotFound` becomes [`DomainError::RunNotIngested`] and **nothing else
//! does**. Two consequences, both intended:
//!
//! * A run in another tenant is reported as "not ingested", identically to a run
//!   that was deleted. The projection treats that as "nothing to project" and
//!   acknowledges (see [`crate::domain::service::ingest`]), so a cross-tenant
//!   probe learns nothing from the outcome *or* from the timing.
//! * `PermissionDenied` and `Unauthenticated` map to [`DomainError::Forbidden`]
//!   instead, because those are decisions about the **subject** rather than about
//!   the row: qa-runs' PDP refused the action for this caller whatever runs
//!   exist. Folding them into `RunNotIngested` would answer 200-with-nothing to
//!   an operator whose grant is simply missing, which is the failure this gear's
//!   siblings each spent a task learning to make visible. It is not an oracle
//!   for the same reason it is useful: the answer does not depend on which run
//!   was asked about.
//!
//! Everything else — transport failures, gateway errors, a 500 from the far side
//! — becomes [`DomainError::Internal`], which is retryable at every call site
//! that has a retry and an opaque 500 at the one that does not
//! ([`crate::domain::error`]'s boundary mapping never discloses its payload).

use std::sync::Arc;

use async_trait::async_trait;
use qa_runs_sdk::{
    Exclusivity, LaunchRequest, QaRunsClientV1, QaRunsError, Run, RunSource, RunTarget,
    RunTestResult, ScheduleNotificationSettings,
};
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::{RunsLauncher, RunsReader};

/// The qa-runs reads, over the `ClientHub`-resolved client.
pub struct QaRunsReader {
    client: Arc<dyn QaRunsClientV1>,
}

impl QaRunsReader {
    #[must_use]
    pub const fn new(client: Arc<dyn QaRunsClientV1>) -> Self {
        Self { client }
    }
}

/// A qa-runs error on a read that addresses **one** run.
///
/// `run_id` is carried rather than parsed out of the error text: `NotFound`'s
/// `resource_name` is optional and formatted by the far side, and the id this
/// gear needs is the one it asked about.
fn on_run(run_id: Uuid, err: QaRunsError) -> DomainError {
    match err {
        QaRunsError::NotFound { .. } => DomainError::RunNotIngested { run_id },
        other => on_subject(other),
    }
}

/// A qa-runs error on a read that addresses one schedule, where "no such
/// schedule" is a normal answer and not a failure to translate.
///
/// `Ok(None)` for `NotFound` — [`RunsReader::get_schedule_notifications`]'s
/// own doc says why a schedule that cannot be resolved (deleted, or not
/// visible to `ctx`) folds into "no schedule" rather than an error this
/// port's one caller would have to handle specially. Everything else shares
/// [`on_subject`]'s translation, the same as [`on_run`] does.
fn on_schedule(err: QaRunsError) -> Result<Option<ScheduleNotificationSettings>, DomainError> {
    match err {
        QaRunsError::NotFound { .. } => Ok(None),
        other => Err(on_subject(other)),
    }
}

/// A qa-runs error on a read that addresses no single run.
///
/// Deliberately has **no** `NotFound` arm. `list_runs_finished_since`'s contract
/// says "there is no not-found case: an empty window is an empty `Vec`", so a
/// `NotFound` here is a contract break on the far side, not an empty window —
/// and turning it into one would silently report a healthy idle system while
/// qa-runs was answering nonsense. It falls through to `Internal`, which is what
/// makes it visible.
///
/// `list_recent_runs` shares it for the same reason: an empty deployment answers
/// with an empty `Vec`, so a `NotFound` there would make a broken qa-runs look
/// like a dashboard with nothing on it.
fn on_subject(err: QaRunsError) -> DomainError {
    match err {
        QaRunsError::PermissionDenied { .. } | QaRunsError::Unauthenticated { .. } => {
            DomainError::Forbidden
        }
        // `{err}` rather than the raw detail: `CanonicalError`'s `Display` is the
        // sibling's own rendering, and this string never reaches a client —
        // `DomainError::Internal` is mapped to an opaque 500.
        other => DomainError::Internal(format!("qa-runs read failed: {other}")),
    }
}

#[async_trait]
impl RunsReader for QaRunsReader {
    async fn get_run(&self, ctx: &SecurityContext, run_id: Uuid) -> Result<Run, DomainError> {
        self.client
            .get_run(ctx, run_id)
            .await
            .map_err(|err| on_run(run_id, err))
    }

    async fn list_run_test_results(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Vec<RunTestResult>, DomainError> {
        self.client
            .list_run_test_results(ctx, run_id)
            .await
            .map_err(|err| on_run(run_id, err))
    }

    async fn list_runs_finished_since(
        &self,
        ctx: &SecurityContext,
        since: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Run>, DomainError> {
        // Neither bound nor ordering is re-applied here. The SDK method's
        // contract is the port's contract, word for word — `finished_at >=
        // since`, oldest first, capped at `limit`, never a run that is not
        // terminal — and re-sorting or re-filtering would create a second place
        // where the two could disagree. In particular, **re-sorting would
        // silently invert the ingest ordinal**, which is the hazard
        // `RunsReader::list_run_test_results` spells out.
        self.client
            .list_runs_finished_since(ctx, since, limit)
            .await
            .map_err(on_subject)
    }

    async fn list_recent_runs(
        &self,
        ctx: &SecurityContext,
        limit: u32,
    ) -> Result<Vec<Run>, DomainError> {
        // No re-sort and no state filter. "Newest first, every state" is the
        // SDK method's contract and the port's, and the dashboard's active and
        // queued counts are folds over exactly the page qa-runs returned — a
        // filter applied here would make the counts disagree with the list they
        // are counted from, which is the failure `summarize_runs` and its test
        // exist to rule out.
        self.client.list_runs(ctx, limit).await.map_err(on_subject)
    }

    /// `QaRunsClientV1::get_schedule`, narrowed to the three notification
    /// fields and with `NotFound` folded into `Ok(None)` — [`RunsReader::get_schedule_notifications`]'s
    /// own doc says why that fold (and not [`on_run`]'s `RunNotIngested`) is
    /// correct here: there is no `ScheduleNotIngested` variant, and a
    /// schedule that cannot be resolved is this port's caller's decision to
    /// make, not an error this adapter raises on its behalf.
    async fn get_schedule_notifications(
        &self,
        ctx: &SecurityContext,
        schedule_id: Uuid,
    ) -> Result<Option<ScheduleNotificationSettings>, DomainError> {
        match self.client.get_schedule(ctx, schedule_id).await {
            Ok(schedule) => Ok(Some(ScheduleNotificationSettings {
                slack_enabled: schedule.slack_notifications_enabled,
                slack_channel: schedule.slack_channel,
                slack_events: schedule.slack_notification_events,
            })),
            Err(err) => on_schedule(err),
        }
    }
}

#[async_trait]
impl RunsLauncher for QaRunsReader {
    /// One `QaRunsClientV1::launch` call, `RunTarget::Collect`-shaped.
    ///
    /// Every field qa-runs' `collect_target_facts`
    /// (`qa-runs/src/domain/service/launch.rs:1248-1289`) does not itself
    /// derive is the neutral value for a run that targets no platform and
    /// bypasses admission: no tags, no parameters, no exclusivity override
    /// (`collect_target_facts` resolves `ExclusivitySource::None` regardless
    /// of what this field carries), no timeout override (the `RunKind::Collect`
    /// timeout arm ignores it too), `RunSource::Manual` (legacy's own
    /// normalization target for a collect run,
    /// `qa_runs_sdk::RunKind::as_str`'s doc), and no schedule.
    ///
    /// `LaunchOutcome::Queued` is not reachable for a collect target — Task
    /// 5's own test,
    /// `a_collect_launch_bypasses_admission_and_is_never_exclusive`, pins
    /// that a collect launch is always `Started` — so this method discards
    /// the outcome rather than branching on it: there is nothing this port's
    /// one caller would do differently for `Queued`.
    async fn launch_collect(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        collect_url: &str,
    ) -> Result<(), DomainError> {
        let request = LaunchRequest {
            target: RunTarget::Collect {
                repo_id,
                collect_url: collect_url.to_owned(),
            },
            environment_id: None,
            branch: Some(branch.to_owned()),
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            parameters: Vec::new(),
            exclusive: Exclusivity::Inherit,
            timeout_seconds: None,
            source: RunSource::Manual,
            schedule_id: None,
        };
        self.client
            .launch(ctx, request)
            .await
            .map(|_outcome| ())
            .map_err(on_subject)
    }

    /// One `QaRunsClientV1::launch` call, `RunTarget::Test`-shaped — Task 35's
    /// auto-rerun.
    ///
    /// Every field this method does not take is the neutral value: no tags, no
    /// parameters, no timeout override, `RunSource::Manual` (legacy's own
    /// normalization target for this path: `normalize_run_source` folds every
    /// non-scheduled source to `manual`, `manager/src/services/argo.rs:2461-2466`,
    /// called at `:2293-2296` for the run-source annotation this path would
    /// otherwise carry) and no schedule.
    /// `exclusive: Exclusivity::Inherit` is the field this method's own port
    /// doc calls out — legacy's `SubmitSingleTestRunRequest.exclusive: None`
    /// — so the launch resolves its exclusivity from the plan and the tiers
    /// rather than this adapter overriding it.
    ///
    /// `environment_id` and `branch` cross verbatim, unresolved further: this
    /// adapter does not default a branch, and neither does
    /// `RunsLauncher::launch_test`'s own doc claim it should — the caller has
    /// already resolved the one value it wants both the plan lookup and this
    /// launch to agree on.
    async fn launch_test(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        plan_path: &str,
        test_file: &str,
        environment_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<(), DomainError> {
        let request = LaunchRequest {
            target: RunTarget::Test {
                repo_id,
                path: plan_path.to_owned(),
                test_file: test_file.to_owned(),
            },
            environment_id,
            branch: branch.map(str::to_owned),
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            parameters: Vec::new(),
            exclusive: Exclusivity::Inherit,
            timeout_seconds: None,
            source: RunSource::Manual,
            schedule_id: None,
        };
        self.client
            .launch(ctx, request)
            .await
            .map(|_outcome| ())
            .map_err(on_subject)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! The translation table, which is the only thing in this file worth a test.
    //!
    //! These drive the two mapping functions directly rather than through a fake
    //! client. What is under test is *which* [`DomainError`] a given
    //! `CanonicalError` becomes, and a fake `QaRunsClientV1` would be seventeen
    //! `unimplemented!()`s — the exact cost `domain::ports::runs_reader`'s header
    //! cites as the reason the port exists — added between the assertion and the
    //! decision without adding coverage. (Seventeen, not nineteen: the trait is
    //! `qa-runs-sdk/src/client.rs:24-200` and both this line and the port's
    //! header said nineteen until Task 18 counted them.)
    //!
    //! # The inputs are built the way qa-runs builds them
    //!
    //! `CanonicalError`'s variants are `#[non_exhaustive]`, so a struct literal
    //! does not compile — which is convenient, because the alternative is
    //! better: [`FarSide`] is a `#[resource_error]` type over **qa-runs' own**
    //! GTS id, so these fixtures come out of the same builder chain the real
    //! client's errors do. A test that hand-rolled the enum could drift from what
    //! the far side actually sends; this one cannot.

    use toolkit::api::canonical_prelude::*;
    use uuid::Uuid;

    use super::{on_run, on_schedule, on_subject};
    use crate::domain::error::DomainError;

    /// qa-runs' run resource, spelled exactly as
    /// `qa-runs/src/domain/error.rs:597` spells it — this is the id its errors
    /// carry.
    #[resource_error(gts_id!("cf.qa.runs.run.v1~"))]
    struct FarSide;

    fn not_found(detail: &str) -> CanonicalError {
        FarSide::not_found(detail.to_owned())
            .with_resource("some-run")
            .create()
    }

    /// **The oracle-closing arm.** A run in another tenant and a run that was
    /// deleted both reach this gear as `NotFound`, and both must leave it as the
    /// same `RunNotIngested` carrying the id *this* gear asked about — the
    /// projection then treats either as "nothing to project".
    #[test]
    fn a_not_found_run_becomes_run_not_ingested_and_keeps_the_id_we_asked_for() {
        let id = Uuid::new_v4();
        match on_run(id, not_found("run not found")) {
            DomainError::RunNotIngested { run_id } => assert_eq!(run_id, id),
            other => panic!("expected RunNotIngested, got {other:?}"),
        }
    }

    /// A subject-level refusal is **not** an absent run. Folding it into
    /// `RunNotIngested` would answer "nothing to project" to an operator whose
    /// grant is missing, and the reconciler would report a healthy sweep forever.
    #[test]
    fn a_permission_denial_is_forbidden_rather_than_a_missing_run() {
        let denied = FarSide::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create();
        assert!(matches!(
            on_run(Uuid::new_v4(), denied),
            DomainError::Forbidden
        ));
    }

    /// Anything else is internal, and the message says which gear failed so a
    /// log line is attributable. Opaque to a client — `DomainError::Internal`
    /// maps to the canonical internal detail in `domain::error`.
    #[test]
    fn any_other_failure_is_internal() {
        let boom = CanonicalError::internal("upstream exploded").create();
        match on_run(Uuid::new_v4(), boom) {
            DomainError::Internal(msg) => assert!(msg.contains("qa-runs"), "{msg}"),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// Notably including `NotFound` on the **listing** path, which has no
    /// not-found case at all: an empty window is an empty `Vec`, so a `NotFound`
    /// there is a far-side contract break and must not be laundered into
    /// "nothing finished" — that would make a broken qa-runs look like a healthy
    /// idle system, which is the one thing
    /// `a_listing_failure_is_an_error_rather_than_an_empty_sweep` exists to
    /// prevent from the other end.
    #[test]
    fn a_listing_not_found_is_internal_rather_than_an_empty_window() {
        assert!(matches!(
            on_subject(not_found("no such collection")),
            DomainError::Internal(_)
        ));
    }

    /// The listing path shares the subject-level arm: a denied operator gets 403
    /// rather than an empty page.
    #[test]
    fn a_listing_denial_is_forbidden() {
        let denied = FarSide::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create();
        assert!(matches!(on_subject(denied), DomainError::Forbidden));
    }

    /// R105: a schedule that cannot be resolved is `Ok(None)`, not an error —
    /// [`RunsReader::get_schedule_notifications`]'s own doc says why "deleted"
    /// and "not visible" are folded together and treated as "no schedule"
    /// rather than surfaced to `NotifyService`.
    #[test]
    fn a_not_found_schedule_folds_to_none_rather_than_an_error() {
        assert_eq!(on_schedule(not_found("no such schedule")).unwrap(), None);
    }

    /// A denied subject is still `Forbidden`, not "no schedule" — folding a
    /// PDP refusal into `None` would answer "nothing to narrow with" to an
    /// operator whose grant on the schedule resource is simply missing.
    #[test]
    fn a_schedule_permission_denial_is_forbidden_rather_than_none() {
        let denied = FarSide::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create();
        assert!(matches!(on_schedule(denied), Err(DomainError::Forbidden)));
    }

    /// Anything else is internal, matching every other read on this client.
    #[test]
    fn any_other_schedule_failure_is_internal() {
        let boom = CanonicalError::internal("upstream exploded").create();
        match on_schedule(boom) {
            Err(DomainError::Internal(msg)) => assert!(msg.contains("qa-runs"), "{msg}"),
            other => panic!("expected Internal, got {other:?}"),
        }
    }
}
