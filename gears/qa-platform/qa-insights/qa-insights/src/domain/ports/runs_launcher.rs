//! The one launch qa-insights performs against qa-runs: a collect-only run.
//!
//! # Why a port, and why it is one method
//!
//! [`RunsReader`](super::RunsReader)'s header gives the argument this port
//! shares: naming exactly the calls this gear makes keeps the cross-gear
//! surface greppable and keeps a fake to a handful of lines rather than a
//! stub of `qa_runs_sdk::QaRunsClientV1`'s whole seventeen-method trait
//! (`qa-runs-sdk/src/client.rs:24-200`). This gear performs exactly one kind
//! of launch — a `Collect`-kind run enumerating one repository's test cases on
//! one branch — so the port is one method rather than a generic `launch`
//! forwarder. Every other launch shape `QaRunsClientV1::launch` supports
//! (plan runs, test runs, custom plans, scheduling) has no caller here and is
//! deliberately absent.
//!
//! `qa_runs_sdk`'s own header names this gear's use of `launch` explicitly:
//! *"Primary consumer: qa-insights' auto-rerun ... the one back-edge in the
//! subsystem, taken deliberately through this public contract"*
//! (`qa-runs-sdk/src/client.rs:21-22`). The collect launch is a second,
//! earlier user of that same sanctioned back-edge — Task 30 rather than the
//! auto-rerun Task 35 forecasts — and it is the same edge, not a new one.
//!
//! # What this port does not decide
//!
//! It does not resolve which repositories exist, whether a branch is real, or
//! how the runner's file set is built — all three moved to qa-runs' own
//! `Collect` target resolution (`qa-runs/src/domain/service/launch.rs:1246-1287`,
//! `collect_target_facts`) and, downstream of that, to `qa-catalog`'s plan
//! listing. This port is the thinnest possible wrapper: repository, branch,
//! and the URL the runner should report to.
//!
//! # The URL crosses this port as a plain `&str`, already built
//!
//! [`RunTarget::Collect::collect_url`](qa_runs_sdk::RunTarget::Collect)'s own
//! doc records why qa-runs does not build it: *"the control plane builds it
//! ... because there the control plane *is* the analytics service. Here it is
//! not: qa-insights owns the report route, so it hands the URL in."* This
//! port's caller — [`crate::domain::service::collect::CollectService`] — is
//! that control plane, and the URL-building decision (including the
//! branch-in-a-query-parameter choice that closes Task 27's router-404
//! hazard) lives there, not here.
//!
//! # A launch failure is not diagnosed here
//!
//! [`launch_collect`](RunsLauncher::launch_collect) returns one error shape
//! for every failure qa-runs can report — a denied subject, an unreachable
//! transport, or (per `collect_target_facts`'s own doc) a validation this
//! gear's caller never itself constructs (a `platform_id`, which this port's
//! caller never sets). It does **not** distinguish "this repository does not
//! have this branch" from any other failure, and that is a discovered fact
//! about qa-runs' `Collect` target rather than a choice made here: branch
//! resolution for a **specific**, caller-supplied branch happens
//! synchronously inside `launch()` (`resolve_branch` picks the explicit
//! branch immediately), but nothing validates that branch exists until
//! dispatch, which runs after `launch()` has already returned. So whether a
//! missing branch surfaces as an `Err` from this method, or asynchronously as
//! a run that later fails, is qa-runs' implementation detail and not a
//! contract this port can rely on today.
//!
//! [`crate::domain::service::collect::CollectService::run_collect_cycle`]
//! is built around that uncertainty rather than against it: it treats
//! **every** error from this method as "skip this repository, keep going" —
//! which is legacy's own per-repo fail-open
//! (`manager/src/services/collect.rs:168-176`, `Err(err) => { warn!(..) }`,
//! no early return) and is *more* robust than legacy in one respect: legacy's
//! skip is deliberately scoped to "no such branch" and "no test files";
//! this port's caller skips on any cause, which cannot silently narrow later
//! if qa-runs starts reporting a new failure mode this port did not
//! anticipate.
//!
//! The production adapter is [`crate::infra::clients::QaRunsReader`], which
//! implements this port on the same struct that implements
//! [`RunsReader`](super::RunsReader) — one `Arc<dyn QaRunsClientV1>`, two
//! traits, because both wrap the identical client and neither needs state the
//! other does not already hold.
//!
//! # A second method, added by Task 35 — the auto-rerun this header forecast
//!
//! [`RunsLauncher::launch_test`] is the single-test launch the paragraph above
//! names: the auto-rerun's *other* consumer of `qa_runs_sdk`'s sanctioned
//! back-edge, and a **second** kind of launch, deliberately not folded into
//! [`launch_collect`](RunsLauncher::launch_collect). The two could not share a
//! method: a collect launch is `RunTarget::Collect`, bypasses admission by
//! construction and discards `LaunchOutcome`; a single-test launch is
//! `RunTarget::Test`, and legacy's own history is that it must **not** bypass
//! anything (VHP-2618, see [`launch_test`](RunsLauncher::launch_test)'s own
//! doc) — one bool parameter could not carry that difference honestly where
//! two methods can.
//!
//! Controller ruling R73: this port grows only for a read or write a test in
//! this crate exercises, and `domain::service::jira_poller`'s
//! `an_auto_rerun_goes_through_the_normal_launch_path` and
//! `the_branch_is_resolved_once_and_reused_for_lookup_and_launch` are that
//! test.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The two launches this gear performs against qa-runs.
#[async_trait]
pub trait RunsLauncher: Send + Sync {
    /// Launch a `Collect`-kind run: enumerate `repo_id`'s test cases on
    /// `branch` and report exact per-file counts to `collect_url`.
    ///
    /// `qa_runs_sdk::QaRunsClientV1::launch` wrapped with
    /// `RunTarget::Collect { repo_id, collect_url }` and `branch` on the
    /// request — Task 5's run kind, verbatim; see that type's own doc for the
    /// `COLLECT_ONLY`/`VHP_COLLECT_URL` contract this preserves (D2).
    ///
    /// Never queued and never exclusive: `RunKind::Collect` bypasses
    /// admission on qa-runs' side
    /// (`qa-runs/src/domain/service/launch.rs:2518`,
    /// `if matches!(run.target.kind(), RunKind::Collect)`), so a busy platform
    /// cannot starve the hourly cycle that keeps analytics' expected-case
    /// numbers current. This port surfaces only the two outcomes that matter
    /// to its one caller — launched, or not — and discards
    /// `LaunchOutcome`'s distinction between `Started` and `Queued`, because a
    /// collect launch can only ever be the former.
    ///
    /// # Errors
    ///
    /// See this trait's own header — every qa-runs failure crosses as the
    /// same shape, and the caller treats every one of them as "skip this
    /// repository".
    async fn launch_collect(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        collect_url: &str,
    ) -> Result<(), DomainError>;

    /// Launch a single-test run: `repo_id`'s plan at `plan_path`, exactly the
    /// one file `test_file` names, targeting `platform_id` on `branch`.
    ///
    /// `qa_runs_sdk::QaRunsClientV1::launch` wrapped with `RunTarget::Test {
    /// repo_id, path: plan_path, test_file }` — Task 35's auto-rerun, the port
    /// of legacy's `trigger_auto_rerun`'s launch call
    /// (`manager/src/services/run_dispatcher::launch` with
    /// `RunIntent::Test { plan_id, request }`,
    /// `manager/src/services/jira_poller.rs:164-189`).
    ///
    /// # This is the **normal** admission path, and that is the whole point
    ///
    /// Unlike [`Self::launch_collect`], a call here is admitted, queued behind
    /// an exclusive run, and has its exclusivity resolved from the tiers —
    /// exactly like a manual or scheduled test launch. Verified in the qa-runs
    /// tree, not assumed: `qa-runs/src/domain/service/launch.rs:2518` bypasses
    /// admission only for `matches!(run.target.kind(), RunKind::Collect)`,
    /// and its own comment at `:2489-2510` states the history this citation
    /// depends on — legacy's `jira_poller` used to call
    /// `ArgoService::submit_workflow` directly (the one bypass of the
    /// per-platform queue, VHP-2618), a stale comment at legacy
    /// `services/argo.rs:369-372` still lists `jira_poller` among the paths
    /// that bypass admission, and that comment's claim is **not** reproduced
    /// here: an auto-rerun through this method is an ordinary
    /// `RunKind::Test` launch, full stop.
    ///
    /// `exclusive: None` on the request — legacy's own choice
    /// (`jira_poller.rs:182-186`, *"No opinion, like a UI launch left on Auto:
    /// let plan.yaml and `TEST_META` decide"*, the comment directly above the
    /// field it annotates) — and `platform_id`/`branch` travel
    /// exactly as given; this port does not default a branch itself. Whoever
    /// calls this has already resolved the branch it wants the plan searched
    /// on and the run executed against, and must pass the **same** value to
    /// both — see `domain::service::jira_poller`'s module doc for why
    /// resolving twice, or resolving the two differently, silently drops a
    /// rerun.
    ///
    /// `Ok(())` regardless of whether qa-runs started the run immediately or
    /// queued it — [`Self::launch_collect`]'s reason for discarding
    /// `LaunchOutcome` does not apply here (a single-test launch **can**
    /// queue), but this port's one caller does not need to tell the two apart
    /// either: legacy's own log line is the only thing that varies between
    /// them (`jira_poller.rs:194-209`), and this gear's caller logs at the
    /// call site instead.
    ///
    /// # Errors
    ///
    /// See this trait's own header — every qa-runs failure crosses as the
    /// same shape, and this port does not distinguish "no such branch" or "no
    /// such test file" from any other qa-runs refusal.
    async fn launch_test(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        plan_path: &str,
        test_file: &str,
        platform_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<(), DomainError>;
}
