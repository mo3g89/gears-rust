//! The JIRA poller: one tenant-scoped pass over every open bug, resolving
//! each against JIRA and, when the tenant's auto-rerun switch is on and a new
//! build has appeared, launching a rerun through the normal admission path.
//!
//! # "One tenant-scoped pass" is now true of the statements, not only of this
//! # sentence — Phase C's final review, Critical 1/1b
//!
//! Until that review the first line above was a claim this module could not
//! keep. Two of the reads and writes a pass makes carried no `tenant_id`
//! predicate at all — only a compiled `AccessScope`, which under a
//! parent-tenant grant (`ScopeFilter::In`, `ScopeFilter::InTenantSubtree`) spans
//! several tenants by design:
//!
//! * [`JiraRepository::list_open`], the listing this pass iterates through
//!   [`JiraService::open_bugs`], enumerated **every in-scope tenant's** open
//!   bugs. Each foreign bug's `jira_key` was then checked against *this*
//!   tenant's `qa_jira_config` — which is tenant-pinned, so genuinely the wrong
//!   JIRA instance — and could reach [`RunsLauncher::launch_test`] for the
//!   foreign tenant's `repo_id` under this tenant's context.
//! * [`JiraRepository::resolve_bug`], the write at
//!   [`JiraPollerService::mark_resolved`], resolved every in-scope tenant's row
//!   carrying the same `jira_key` — a key two tenants are *expected* to
//!   collide on.
//!
//! Both are fixed in the repository rather than here, so the pin holds for
//! every caller and not only for this one; those two methods' own docs carry
//! the failure chains and the reason the fix did not go at the service.
//! `a_pass_does_not_touch_another_tenants_open_bug` is this module's guard, and
//! it is the test that goes red if either predicate is dropped again.
//!
//! Task 35. Ports legacy's `manager/src/services/jira_poller.rs` end to end —
//! Step 0's brief asks for five citations to be verified against the real
//! file, and all five checked out:
//!
//! 1. `:16-31` — the interval is `JiraPollerConfig.poll_interval_seconds.max(1)`,
//!    and the loop **sleeps first**, before the first pass. Both belong to
//!    the loop `start_jira_poller` owns and neither is [`JiraPollerService`]'s:
//!    the clamp already lives on [`JiraService::poller_config`]
//!    (`domain::service::jira`'s header, item 3), and the sleep-first shape is
//!    Task 40's ticker to build — see this header's own section below.
//! 2. `:40-43` — an absent or disabled config returns `Ok(())` with no bug
//!    looked at. Ported by [`JiraService::active_config`], whose own doc
//!    names this module as the consumer that calls it once per pass before it
//!    looks at any bug — [`JiraPollerService::poll_once`] does exactly that.
//! 3. `:54-58` — resolution is `status_category == "done"`
//!    ([`StatusCategory::is_resolved`](crate::domain::ports::jira_client::StatusCategory::is_resolved)),
//!    and `resolve_bug` runs **before** the auto-rerun decision, so a bug
//!    resolves even when the switch is off —
//!    `a_bug_resolves_even_when_auto_rerun_is_off` pins it.
//! 4. `:60-70` — the rerun needs **both** `auto_rerun_on_resolve` and a new
//!    build (D8) — `a_resolved_bug_without_a_new_build_does_not_rerun` pins
//!    the second half.
//! 5. `:88-96` and `:100-118` — the rerun goes through the normal admission
//!    path (`trigger_auto_rerun`'s own doc, and — because a stale comment
//!    elsewhere claims otherwise — verified directly against
//!    `qa-runs/src/domain/service/launch.rs:2518`, which bypasses admission
//!    only for `RunKind::Collect`; `RunTarget::Test` is not that kind), and the
//!    branch is resolved **once**, from the platform, and reused for both the
//!    plan lookup and the launch —
//!    `the_branch_is_resolved_once_and_reused_for_lookup_and_launch` pins it.
//!
//! # Step 0's sixth decision: `find_plan_test_file` against `CatalogReader::list_universe`
//!
//! Legacy's `find_plan_test_file` (`jira_poller.rs:268-292`, the function
//! body — not `:150-162`, which is `trigger_auto_rerun`'s call site and its
//! "no test file" warning) reads a plan's `test_files` off a local git
//! checkout and, through `declares_test_title` (`jira_poller.rs:301-305`),
//! greps each candidate for `TEST_TITLE`/`TEST_META.title`. This gear has no
//! checkout — ADR-0005
//! confines git egress to qa-catalog — so the read has to go through a
//! capability this gear already has, and
//! [`CatalogReader::list_universe`] is it: qa-catalog's own sync already
//! parses both `TEST_TITLE` and `TEST_META`
//! (`qa-catalog/src/domain/parsing/test_meta.rs:102`,
//! `manager/src/routes/tests.rs:1027-1036` for the legacy truth it ports) into
//! `UniverseTest::test_name` — the title when the file declares one, else a
//! fallback name — so filtering the universe by `(repo_id, test_name)` answers
//! exactly the question `find_plan_test_file` answers, without this gear
//! re-parsing a file it cannot read. [`JiraPollerService::find_plan_test_file`]
//! is that filter, and it is the only call site that reads the universe:
//! called once per rerun candidate, on the branch
//! [`JiraPollerService::rerun`] already resolved.
//!
//! One legacy step this gear's version skips entirely: resolving the *plan*
//! itself (`get_plan_with_repos`). Legacy's `plan_id` is an opaque slug it has
//! to re-resolve into a plan; this gear's [`JiraBug`] already carries the
//! plan's identity directly — `repo_id` and `plan_path`, Task 33's own two
//! columns — so there is no plan to look up, only the test file within it.
//!
//! # The branch, resolved once, is the whole point of this module's careful
//! # ordering
//!
//! [`JiraPollerService::rerun`] resolves `bug.environment_id`'s default-branch
//! override **exactly once**, into a local `branch`, and passes that same
//! value to both [`JiraPollerService::find_plan_test_file`] (the lookup) and
//! [`RunsLauncher::launch_test`] (the launch). Resolving it twice — once for
//! the lookup and again for the launch — or resolving the lookup against the
//! *repository's* default branch
//! instead of the platform's, would search a tree the run will not execute
//! against: a test that exists only on the platform's pinned branch would be
//! invisible to a lookup made against the repository default, and the rerun
//! would be silently dropped with nothing to show for it — the bug is already
//! marked resolved by the time this runs, so there is no second chance this
//! pass. Legacy's own comment at `jira_poller.rs:104-109` states the identical
//! hazard for its own two call sites.
//!
//! # Every per-bug failure is logged and skipped, never a whole-pass error
//!
//! Legacy's loop treats a failed JIRA status check this way
//! (`jira_poller.rs:83-85`, `tracing::warn!` and continue), and every other
//! per-bug step downstream — the local resolve write, the plan's latest
//! version, the platform's branch, the universe lookup, the launch itself —
//! gets the same treatment in
//! [`JiraPollerService::rerun`]/[`JiraPollerService::maybe_rerun`], for the
//! reason each log line states: the bug is already resolved by the time any
//! of these run, so a dropped rerun here is not retried next pass the way an
//! unresolved bug's status check is.
//!
//! # Interval and enable switch: ticked by Task 40
//!
//! Leader-elected, role `qa-insights-jira-poller`
//! ([`crate::infra::leader::ROLE_JIRA_POLLER`]).
//! [`JiraPollerService::poll_once`] is the droppable unit Task 35 shipped —
//! [`crate::domain::service::reconcile::ReconcileService::reconcile_once`] is
//! the precedent for the shape: a single tenant-scoped pass with no
//! `CancellationToken` and no sleep of its own. **Task 40 wraps it**, in
//! `crate::gear`'s `jira_poller_ticker`, once per tenant
//! [`TenantDirectory`](crate::domain::service::tenants::TenantDirectory)
//! reports, under a `system_actor::for_jira_poll` context minted from that
//! tenant.
//!
//! **The cadence is a process-level config knob, not legacy's per-tenant
//! interval**, and that is a decision rather than an omission:
//! `config::QaInsightsConfig::jira_poller_interval_seconds`' own doc carries it
//! in full. Legacy re-reads `JiraPollerConfig.poll_interval_seconds` inside its
//! loop (`jira_poller.rs:16-31`) because legacy has one tenant; honouring N
//! per-tenant intervals from one loop needs N timers and a per-tenant last-pass
//! instant, which is state no table here holds. The `.max(1)` clamp on that
//! column is still applied exactly where legacy applies it, on
//! [`JiraService::poller_config`], and the column still means what it meant to
//! `PUT /qa/v1/settings/jira-poller` — but no per-tenant timer reads it.
//!
//! This service **is** a field on [`crate::domain::service::AppServices`] as of
//! Task 40 (`AppServices::jira_poller`), which is the task that became its first
//! reader; it was deliberately absent until then, for that struct's own stated
//! reason.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use qa_insights_sdk::JiraBug;
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::jira_client::StatusCategory;
use crate::domain::ports::metrics::{
    JiraBugOutcome, JiraPollMetrics, JiraPollOutcome, NoopMetrics,
};
use crate::domain::ports::{CatalogReader, EnvironmentReader, RunsLauncher};
use crate::domain::repos::{JiraRepository, ResultsRepository};
use crate::domain::service::emit;
use crate::domain::service::jira::JiraService;

/// One tenant-scoped pass over every open bug, plus the auto-rerun it may
/// trigger. See this module's header for the full design.
pub struct JiraPollerService<J, R> {
    jira: Arc<JiraService<J, R>>,
    catalog: Arc<dyn CatalogReader>,
    platforms: Arc<dyn EnvironmentReader>,
    launcher: Arc<dyn RunsLauncher>,
    /// Where this service's telemetry goes. Never `Option`: [`NoopMetrics`]
    /// is the default a caller that wires nothing gets, so every emission
    /// below is unconditional and the "silent when no adapter is installed"
    /// promise is structural rather than a branch somebody has to remember.
    metrics: Arc<dyn JiraPollMetrics>,
    /// Latched by [`emit`] the first time an emission panics, after which this
    /// service stops calling its adapter. Per service, not global — see
    /// [`emit`]'s own doc.
    metrics_silenced: AtomicBool,
}

impl<J, R> JiraPollerService<J, R>
where
    J: JiraRepository,
    R: ResultsRepository,
{
    /// `metrics` is `None` for a caller that does not measure this service,
    /// and resolves to [`NoopMetrics`]. See
    /// [`crate::domain::service::collect::CollectService::new`] for why the
    /// parameter is an `Option` rather than a required port.
    #[must_use]
    pub fn new(
        jira: Arc<JiraService<J, R>>,
        catalog: Arc<dyn CatalogReader>,
        platforms: Arc<dyn EnvironmentReader>,
        launcher: Arc<dyn RunsLauncher>,
        metrics: Option<Arc<dyn JiraPollMetrics>>,
    ) -> Self {
        Self {
            jira,
            catalog,
            platforms,
            launcher,
            metrics: metrics.unwrap_or_else(|| Arc::new(NoopMetrics)),
            metrics_silenced: AtomicBool::new(false),
        }
    }

    /// Emit one per-bug outcome, guarded by this service's own latch.
    ///
    /// **One call site, deliberately**: [`Self::run_pass`]'s loop, on the
    /// outcome [`Self::poll_one_bug`] returned. Nothing inside that chain
    /// emits — every one of its classification points *returns* its label, and
    /// the emission happens once above them all. That is what makes "exactly
    /// one increment per bug per pass" a property of the shape rather than of
    /// a dozen return sites each remembering to fire exactly once, and that
    /// invariant is what lets a sum over the family be the denominator every
    /// per-class rate is read against.
    ///
    /// A named method rather than the [`emit`] call spelled out at that loop,
    /// so this service's latch is named in one place.
    fn record_bug(&self, outcome: JiraBugOutcome) {
        emit(&self.metrics_silenced, || self.metrics.bug(outcome));
    }

    /// One pass: check every open bug, resolve what JIRA reports done, and
    /// auto-rerun what qualifies.
    ///
    /// Legacy's `poll_resolved_bugs` (`manager/src/services/jira_poller.rs:35-90`),
    /// minus the config-read-and-sleep half `start_jira_poller`'s own loop
    /// owns — `crate::gear`'s `jira_poller_ticker` is what wraps a call to this
    /// method in that loop, once per tenant per tick.
    ///
    /// The listing is [`JiraService::open_bugs`] with both narrowing arguments
    /// absent, which since Phase C's final review means **this tenant's** open
    /// bugs and not the whole scope's — see this module's header for what the
    /// unpinned version did with the extra rows.
    ///
    /// An absent or disabled config is `Ok(())` with no bug looked at
    /// ([`JiraService::active_config`]). A tenant with no open bugs is
    /// likewise `Ok(())`. Every bug beyond that is processed independently,
    /// and a failure anywhere in one bug's own chain — the status check, the
    /// local resolve write, the new-build read, the branch resolution, the
    /// universe lookup or the launch — is logged and stops at that bug; see
    /// this module's header, "Every per-bug failure is logged and skipped".
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies the config read or the
    /// open-bugs listing. [`DomainError::Database`] on a query failure reading
    /// either. Nothing else propagates: every per-bug failure downstream of
    /// the open-bugs read is swallowed, by design.
    pub async fn poll_once(&self, ctx: &SecurityContext) -> Result<(), DomainError> {
        let started = Instant::now();
        let result = self.run_pass(ctx).await;
        let outcome = match &result {
            Ok(PassEnd::Skipped) => JiraPollOutcome::Skipped,
            Ok(PassEnd::Completed) => JiraPollOutcome::Completed,
            Err(error) => JiraPollOutcome::from(error),
        };
        // Read before the emission, so a slow adapter cannot inflate the
        // duration it is being handed.
        let elapsed = started.elapsed();
        emit(&self.metrics_silenced, || {
            self.metrics.poll_pass(outcome, elapsed);
        });
        result.map(|_| ())
    }

    /// [`Self::poll_once`]'s body, unmeasured, and saying which of the two
    /// successful ends it reached.
    ///
    /// `Result<(), _>` cannot carry that: a tenant with no JIRA configuration
    /// and a tenant whose every bug was processed both answer `Ok(())`, and
    /// folding them into one label would make a deployment where the
    /// integration is switched off everywhere indistinguishable from one where
    /// it is working. See [`JiraPollOutcome::Skipped`].
    async fn run_pass(&self, ctx: &SecurityContext) -> Result<PassEnd, DomainError> {
        if self.jira.active_config(ctx).await?.is_none() {
            return Ok(PassEnd::Skipped);
        }

        let poller_config = self.jira.poller_config(ctx).await?;
        let open_bugs = self.jira.open_bugs(ctx, None, None).await?;

        // One instant for the whole pass, not one `now_utc()` per bug —
        // `JiraRepository::resolve_bug`'s own doc: "the poller stamps every
        // bug in one pass with one instant."
        let resolved_at = OffsetDateTime::now_utc();

        for bug in &open_bugs {
            let outcome = self
                .poll_one_bug(ctx, bug, poller_config.auto_rerun_on_resolve, resolved_at)
                .await;
            // The emission is here rather than inside `poll_one_bug` so that
            // there is exactly one per bug however many ways that chain can
            // end. An emission at each classification site would be a dozen
            // returns to keep in step, and the end added by the next person to
            // touch that chain would emit nothing at all.
            self.record_bug(outcome);
        }

        Ok(PassEnd::Completed)
    }

    /// One bug's status check, resolution and (conditional) rerun.
    ///
    /// Never returns an error: every failure from here on is this bug's
    /// alone, logged and skipped, matching legacy's own per-bug resilience
    /// (`jira_poller.rs:83-85` for the status check; `trigger_auto_rerun`'s
    /// own `tracing::warn!`-and-return arms for the rest, `:126-132`,
    /// `:151-162`, `:211-220`).
    ///
    /// # The return value is the only record a caller gets
    ///
    /// Nothing above this function can see what happened to one bug — that is
    /// the design, and it is the defect [`JiraBugOutcome`] exists to make
    /// visible. The value returned here is what
    /// [`crate::domain::metrics::QA_INSIGHTS_JIRA_BUG`] counts; the logs
    /// remain the only place the individual error text lives.
    async fn poll_one_bug(
        &self,
        ctx: &SecurityContext,
        bug: &JiraBug,
        auto_rerun_on_resolve: bool,
        resolved_at: OffsetDateTime,
    ) -> JiraBugOutcome {
        let Some(status) = self.checked_status(ctx, bug).await else {
            return JiraBugOutcome::StatusCheckFailed;
        };
        if !status.is_resolved() {
            return JiraBugOutcome::Unresolved;
        }

        let resolved = self.mark_resolved(ctx, bug, resolved_at).await;

        if !auto_rerun_on_resolve {
            return Self::classify(resolved, JiraBugOutcome::Resolved);
        }

        let rerun = self.maybe_rerun(ctx, bug).await;
        Self::classify(resolved, rerun)
    }

    /// A failed local resolve write outranks whatever the rerun chain then
    /// reported.
    ///
    /// The write failing does **not** stop the chain — `mark_resolved` is
    /// best-effort by design and the rerun decision runs regardless, which
    /// this task did not change — so a bug can both fail the write and launch
    /// a rerun. This picks which of the two facts the bug's single increment
    /// records, and it picks the write, because the write is the one with a
    /// consequence for the *next* pass: the bug stays open in this gear's
    /// table while JIRA keeps reporting it done, so the next pass resolves it
    /// again and reruns it again, and the pass after that. A persistently
    /// failing resolve write is a rerun-storm generator, which is precisely
    /// what [`crate::domain::metrics::QA_INSIGHTS_JIRA_RERUN`] is read beside
    /// this family to catch.
    const fn classify(resolved: bool, otherwise: JiraBugOutcome) -> JiraBugOutcome {
        if resolved {
            otherwise
        } else {
            JiraBugOutcome::ResolveWriteFailed
        }
    }

    /// `bug`'s status category, or `None` when the check itself failed —
    /// already logged, so the caller only has to decide what "no answer"
    /// means for it.
    async fn checked_status(&self, ctx: &SecurityContext, bug: &JiraBug) -> Option<StatusCategory> {
        match self.jira.check_status(ctx, &bug.jira_key).await {
            Ok(status) => Some(status),
            Err(error) => {
                tracing::warn!(
                    jira_key = %bug.jira_key,
                    %error,
                    "failed to check JIRA status for this bug; skipping it for this pass \
                     (manager/src/services/jira_poller.rs:83-85)",
                );
                None
            }
        }
    }

    /// Record `bug` as resolved. Best-effort, matching legacy's
    /// `let _ = jira.resolve_bug(...).await;` (`jira_poller.rs:59`): the write
    /// is attempted regardless of what happens next, and its failure does not
    /// gate the auto-rerun decision that follows it — the two are
    /// independent, per this task's Step 0, point 3.
    ///
    /// Returns whether the write landed. The value is not a control-flow
    /// signal — the caller proceeds either way, exactly as before — only a
    /// classification for [`Self::classify`].
    async fn mark_resolved(
        &self,
        ctx: &SecurityContext,
        bug: &JiraBug,
        resolved_at: OffsetDateTime,
    ) -> bool {
        tracing::info!(jira_key = %bug.jira_key, "bug resolved in JIRA, marking as resolved");
        if let Err(error) = self.jira.resolve_bug(ctx, &bug.jira_key, resolved_at).await {
            tracing::warn!(
                jira_key = %bug.jira_key,
                %error,
                "failed to record the bug as resolved locally; the next pass will retry it \
                 while JIRA still reports it done",
            );
            return false;
        }
        true
    }

    /// D8: a resolved bug reruns only when a new build has appeared since the
    /// version it was filed against (`manager/src/services/jira_poller.rs:60-70`).
    ///
    /// The three no-op paths below all answer [`JiraBugOutcome::Resolved`]:
    /// each is a decision *not* to rerun, taken deliberately and on purpose,
    /// and none of them dropped anything. (The fourth such decision, the
    /// auto-rerun switch being off, is [`Self::poll_one_bug`]'s and never
    /// reaches this function.) Only the plan-version read can fail here, and
    /// that one did drop a rerun.
    async fn maybe_rerun(&self, ctx: &SecurityContext, bug: &JiraBug) -> JiraBugOutcome {
        // Legacy's own early return (`jira_poller.rs:226-229`): a bug with no
        // recorded version can never have "a new one" by comparison.
        let Some(bug_version) = bug.app_version.as_deref() else {
            return JiraBugOutcome::Resolved;
        };

        let latest_version = match self.jira.latest_version_for_plan(ctx, &bug.plan_path).await {
            Ok(version) => version,
            Err(error) => {
                tracing::warn!(
                    jira_key = %bug.jira_key,
                    plan_path = %bug.plan_path,
                    %error,
                    "failed to read the plan's latest build; the bug is already resolved, so \
                     this rerun will not be retried",
                );
                return JiraBugOutcome::PlanVersionUnreadable;
            }
        };
        // No versioned run recorded for this plan at all — legacy's `None`
        // arm (`jira_poller.rs:241`), not "a new build was found".
        let Some(latest_version) = latest_version else {
            return JiraBugOutcome::Resolved;
        };
        if latest_version == bug_version {
            // The very build that already failed. Reprising it proves
            // nothing and costs a platform slot — the whole reason D8 exists.
            return JiraBugOutcome::Resolved;
        }

        self.rerun(ctx, bug).await
    }

    /// Launch the auto-rerun through the normal admission path.
    ///
    /// Resolves `bug.environment_id`'s default branch **once** and reuses that
    /// same value for both [`Self::find_plan_test_file`] and the launch — see
    /// this module's header for why resolving it twice, or resolving the
    /// lookup against a different default, silently drops the rerun.
    async fn rerun(&self, ctx: &SecurityContext, bug: &JiraBug) -> JiraBugOutcome {
        let Some(branch) = self.resolve_branch(ctx, bug).await else {
            return JiraBugOutcome::BranchUnresolved;
        };

        let Some(test_file) = self
            .find_plan_test_file(ctx, bug.repo_id, branch.as_deref(), &bug.test_name)
            .await
        else {
            tracing::warn!(
                jira_key = %bug.jira_key,
                test_name = %bug.test_name,
                %bug.repo_id,
                branch = ?branch,
                "auto-rerun skipped: no test on the resolved branch declares this title in the \
                 catalog universe; the bug is already resolved, so this rerun will not be \
                 retried",
            );
            return JiraBugOutcome::TestFileUnresolved;
        };

        self.launch(ctx, bug, &test_file, branch.as_deref()).await
    }

    /// `bug.environment_id`'s default-branch override, or `None` when there is
    /// no platform to ask — [`RunsLauncher::launch_test`]'s own doc calls
    /// this port's `None` "no override", which is legacy's no-branch-selected
    /// shape and not a failure.
    ///
    /// The outer `Option` is this call's own success/failure: `None` means
    /// the read itself failed, already logged, and the caller should give up
    /// on this rerun rather than treat a failed lookup as "no override".
    async fn resolve_branch(&self, ctx: &SecurityContext, bug: &JiraBug) -> Option<Option<String>> {
        let Some(environment_id) = bug.environment_id else {
            return Some(None);
        };
        match self.platforms.default_branch(ctx, environment_id).await {
            Ok(branch) => Some(branch),
            Err(error) => {
                tracing::warn!(
                    jira_key = %bug.jira_key,
                    %environment_id,
                    %error,
                    "failed to resolve the platform's default branch; the bug is already \
                     resolved, so this rerun will not be retried",
                );
                None
            }
        }
    }

    /// The launch itself, once a `test_file` and a `branch` have been
    /// resolved.
    ///
    /// # The rerun counter is incremented here, before the call
    ///
    /// [`crate::domain::metrics::QA_INSIGHTS_JIRA_RERUN`] counts **attempts**,
    /// and this is the one place an attempt exists: every path above this one
    /// decided not to rerun or could not work out what to rerun, and none of
    /// them reached qa-runs. Incrementing before the `await` rather than in
    /// the `Ok` arm is what makes the series a measure of the load this
    /// poller puts on the admission path — a storm of launches qa-runs refuses
    /// costs the same calls as a storm it accepts, and is the same incident.
    /// How each one went is this function's return value, on the per-bug
    /// family.
    async fn launch(
        &self,
        ctx: &SecurityContext,
        bug: &JiraBug,
        test_file: &str,
        branch: Option<&str>,
    ) -> JiraBugOutcome {
        emit(&self.metrics_silenced, || self.metrics.auto_rerun());
        match self
            .launcher
            .launch_test(
                ctx,
                bug.repo_id,
                &bug.plan_path,
                test_file,
                bug.environment_id,
                branch,
            )
            .await
        {
            Ok(()) => {
                tracing::info!(
                    jira_key = %bug.jira_key,
                    test_name = %bug.test_name,
                    test_file,
                    "auto-triggered a re-run",
                );
                JiraBugOutcome::Resolved
            }
            Err(error) => {
                tracing::error!(
                    jira_key = %bug.jira_key,
                    test_name = %bug.test_name,
                    %error,
                    "failed to auto-trigger a re-run; the bug is already resolved, so this \
                     rerun will not be retried",
                );
                JiraBugOutcome::LaunchFailed
            }
        }
    }

    /// Step 0's sixth decision: the port of legacy's `find_plan_test_file`
    /// (`jira_poller.rs:268-292`) against
    /// [`CatalogReader::list_universe`](crate::domain::ports::CatalogReader::list_universe) —
    /// see this module's header for why. The **only** call site that reads
    /// the universe, so "resolved once" ([`Self::rerun`]'s `branch`) also
    /// means "looked up once".
    ///
    /// A universe read failure is logged and treated as "no match", the same
    /// outcome an absent title gets: both leave the bug already marked
    /// resolved with nothing this pass can do about it, so a distinct error
    /// path would buy nothing a caller could act on differently.
    async fn find_plan_test_file(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: Option<&str>,
        test_name: &str,
    ) -> Option<String> {
        let universe = match self.catalog.list_universe(ctx, None, branch).await {
            Ok(universe) => universe,
            Err(error) => {
                tracing::warn!(
                    %repo_id,
                    branch = ?branch,
                    %error,
                    "failed to read the catalog universe while resolving an auto-rerun's test \
                     file",
                );
                return None;
            }
        };
        universe
            .into_iter()
            .find(|entry| entry.repo_id == repo_id && entry.test_name == test_name)
            .map(|entry| entry.test_file)
    }
}

/// Which of [`JiraPollerService::run_pass`]'s two successful ends a pass
/// reached.
///
/// A type rather than a `bool` for the reason `run_pass`' own doc gives: the
/// two are different operational events, and the one thing a `bool` return
/// would not survive is somebody reading it the wrong way round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassEnd {
    /// The tenant has no active JIRA configuration; no bug was looked at.
    Skipped,
    /// Every open bug this tenant has was processed.
    Completed,
}

#[cfg(test)]
#[path = "jira_poller_tests.rs"]
mod tests;
