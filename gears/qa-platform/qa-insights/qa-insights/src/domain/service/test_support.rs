//! Test doubles shared by this layer's tests.
//!
//! # One fake qa-runs, not two
//!
//! [`FakeRuns`] started life inside the transactional broker consumer's test
//! module at Task 13, back when that consumer was this crate's second user of
//! the double — it and the event-broker dependency it needed were deleted once
//! it was established that no deployment ever registered the client it needed
//! (`crate::gear`'s header). Task 15 needed the same double before that —
//! a run that finished at a chosen instant, with a chosen set of results, and a
//! way to make a read fail — and a second copy would have been two fakes that
//! could disagree about what qa-runs does. The plan's Task 15 Step 3 asked for
//! it here; this is that move, and it is now this module's only home.
//!
//! # It holds whole runs
//!
//! The projection reads a run *and* its result rows, and the two have to agree:
//! a fake that let them drift would make a mapper defect look like a fixture
//! defect. So the unit of insertion is a run plus its rows, and the two reads
//! are served from the same entry.
//!
//! # What it deliberately does not do
//!
//! No latency, no partial pages, no per-call scripting beyond the two failure
//! switches. Every property that needs a *real* qa-runs — authorization on the
//! far side, the `not_found`-covers-forbidden rule, transport retries — belongs
//! to Task 40's wiring, not here.
//!
//! # The `AuthZ` doubles, added by Task 16
//!
//! [`ReconcileService::rebuild`](crate::domain::service::reconcile::ReconcileService::rebuild)
//! is this gear's first PEP caller, so this module gained the three doubles every
//! sibling's test support carries: [`TenantScopedAuthZ`] (grants, and compiles to
//! a *real* `owner_tenant_id IN [tenant]` scope), [`DenyAllAuthZ`] (the PDP-deny
//! path), and [`RecordingAuthZ`] (grants, and records what was asked).
//!
//! **[`RecordingAuthZ`] is not a convenience.** The action string a route
//! authorizes under is a security surface with no other witness: nothing in this
//! crate fails if `actions::REBUILD` is silently replaced by `actions::GET`, and
//! no integration test in this workspace evaluates a real policy. It is the one
//! test that can see the request this gear actually sends.
//!
//! [`permissive_response`] is shared by all three rather than reimplemented per
//! double, for the reason qa-environments' copy records: two definitions of
//! "permissive" that can drift are worse than one that is used everywhere. The
//! shape is copied from `qa-environments/src/test_support.rs`, which is the
//! subsystem's reference for it.
//!
//! # The analytics fixtures, added by Task 20
//!
//! [`FakeCatalog`], [`universe_test`], [`universe_test_full`] and [`exec_row_at`]
//! are here rather than inside `domain::analytics::universe_tests` for the reason
//! [`FakeRuns`] is: Tasks 21-27 each fold the *same* two inputs — a
//! `qa_catalog_sdk::UniverseTest` list and an
//! [`ExecRow`](crate::domain::analytics::ExecRow) list — and seven copies of the
//! same two builders could disagree about what a universe entry looks like.
//!
//! # The clock double, added by Task 22
//!
//! [`FixedClock`] is the constant-date [`Clock`] the analytics windows are
//! anchored on. It lives here rather than in `analytics::aggregates_tests`
//! because Tasks 22-25 all anchor on the same date and because the *service*
//! tier (Task 25b) is what holds the port; a second copy inside one test module
//! would have been the one `domain::service::analytics_tests` could not reach,
//! and it is now the module that constructs this.
//!
//! [`TODAY`] is the date it reports, and it is deliberately the calendar day of
//! [`ts`] — the instant this layer's row builders already stamp — so a row built
//! by [`exec_row_at`] with `ts()` lands on the last column of a window ending
//! [`TODAY`] rather than off the end of it.
//!
//! # The platform-name double, added by Task 25a
//!
//! [`FakePlatforms`] is the `Vec`-backed double behind
//! [`EnvironmentReader`](crate::domain::ports::EnvironmentReader), which Task 25a added
//! because `ExecRow::platform_id` is a `Uuid` where legacy's `platform` was a
//! display name. It lives here rather than in the analytics test modules for
//! [`FakeCatalog`]'s reason: Task 25b's service is what resolves the names, and
//! the DTO tier renders them, so two modules need the same double.
//!
//! It **records the id batches** as well as answering them —
//! [`FakePlatforms::batches`] — because the resolution being once-per-request
//! rather than once-per-row is a property with no other witness. Same shape and
//! same reason as [`FakeRuns::recent_limits`], and
//! `the_platform_names_are_resolved_in_one_batch_of_distinct_ids` (Task 25b) is
//! the assertion it exists for.
//!
//! They are builders and not fakes, with two exceptions: [`FakeCatalog`] is the
//! `Vec`-backed double behind [`CatalogReader`]. **This said nothing in
//! production implements that port and named Task 40 as the adapter's owner**;
//! Task 25a shipped `infra::clients::qa_catalog::QaCatalogReader`, and
//! `domain::ports::catalog_reader`'s header carries why the Task 40 assignment
//! could not have worked. This double is still the only implementation any test
//! in this crate folds — the adapter is one `map_err` and its own tests drive that
//! function directly. [`FakePlatforms`] is the second, and its production
//! counterpart is `infra::clients::qa_environments::QaEnvironmentsReader`.
//!
//! # [`Fleet`], added by Task 7's review-remediation pass
//!
//! Every fixture above builds one *service* over doubled ports. Task 7 needed
//! more than that: `handlers::collect::report_collect_count` takes
//! `Extension<Arc<ConcreteAppServices>>` — the whole DI container, wired with
//! every one of this gear's six real repositories over one real (in-memory)
//! database — because a fake standing in for [`CollectRepository`] could
//! answer "wrote nothing" without the write path itself ever having refused
//! anything. [`Fleet`] is this crate's answer to `qa-runs`'s own `Fleet`
//! (`qa-runs/qa-runs/src/domain/service/test_support.rs:1898`), the
//! established shape for that: one shared database, real repositories over
//! it, and every collaborator this gear cannot avoid wiring — permissive or
//! inert wherever nothing in this suite exercises it — behind the one knob
//! Task 7's tests actually vary, `collect_report_signing_secret`.
//!
//! It is deliberately not named or shaped around collect alone — Task 8 wires
//! the same struct behind the notification and saved-view handlers — so a
//! caller wanting a different knob varied (or a different double swapped in)
//! extends [`Fleet`], rather than a task-7-specific `collect_fleet` that would
//! have needed rewriting the moment a second handler suite needed it.

// A test double: every accessor unwraps a `Mutex` whose only contention is
// this crate's own tests, and a poisoned lock there is a test that already
// panicked. Documenting a `# Panics` section on each would be noise.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError};
use qa_catalog_sdk::{SOURCE_REPO, UniverseTest};
use qa_insights_sdk::CollectCount;
use qa_runs_sdk::{
    ExclusiveTier, Run, RunSource, RunState, RunTarget, RunTestResult, ScheduleNotificationSettings,
};
use time::{Date, Duration, OffsetDateTime};
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::analytics::ExecRow;
use crate::domain::error::DomainError;
use crate::domain::ports::{
    CatalogReader, Clock, EnvironmentReader, IssueRef, JiraClient, JiraIssue, MailClient, NewIssue,
    RunsLauncher, RunsReader, SendOutcome, SlackClient, SlackMessage, StatusCategory,
};
use crate::domain::repos::CollectRepository;
use crate::domain::service::{AppServices, DbProvider, ServiceDeps};
use crate::gear::ConcreteAppServices;
use crate::infra::notify::UnsupportedMailClient;
use crate::infra::storage::collect_sea_repo::OrmCollectRepository;
use crate::infra::storage::jira_sea_repo::OrmJiraRepository;
use crate::infra::storage::notify_sea_repo::OrmNotifyRepository;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::saved_views_sea_repo::OrmSavedViewsRepository;
use crate::infra::storage::watermark_sea_repo::OrmWatermarkRepository;
use crate::infra::storage::test_db::{inmem_db, scope};

/// One run as this fake holds it: its metadata and its per-test rows.
type StoredRun = (Run, Vec<RunTestResult>);

/// An in-memory qa-runs.
#[derive(Default)]
pub struct FakeRuns {
    runs: Mutex<HashMap<Uuid, StoredRun>>,
    /// Makes every [`RunsReader::list_run_test_results`] fail, which is how a
    /// test forces the projection to abort *after* the run lookup succeeded —
    /// the shape that distinguishes a rolled-back transaction from one that
    /// never started.
    fail_result_reads: Mutex<bool>,
    /// Makes [`RunsReader::list_run_test_results`] fail for exactly one run id.
    /// The reconcile sweep needs one run in a page to fail while its
    /// neighbours succeed.
    fail_result_reads_for: Mutex<Option<Uuid>>,
    /// Makes every [`RunsReader::list_runs_finished_since`] fail.
    fail_listing: Mutex<bool>,
    /// How many times the projection re-read a run. The only way to observe
    /// that a redelivery did work rather than being skipped upstream.
    result_reads: AtomicUsize,
    /// The `limit` of every [`RunsReader::list_recent_runs`] call, in order.
    ///
    /// Added by Task 18's fix round for one assertion nothing else could make:
    /// the dashboard's active and queued counts are exact only up to the ceiling
    /// it asks for, so *which* ceiling it asks for is a documented bound
    /// (`domain::service::dashboard::RUN_PAGE`) with — until this — nothing
    /// failing if the call quietly passed something else.
    recent_limits: Mutex<Vec<u32>>,
    /// How many times a caller listed the finished-since window.
    ///
    /// Added by Task 16 for one assertion that nothing else could make: the
    /// rebuild compiles its PEP scope **before** it lists qa-runs, so a denied
    /// caller must not be able to use the endpoint to learn which runs exist in
    /// a window. `result_reads` cannot see that — it counts per-run reads, which
    /// a denied caller would not reach anyway.
    listings: AtomicUsize,
    /// Per-schedule notification settings, behind
    /// [`RunsReader::get_schedule_notifications`] — Task 38's fix round 1
    /// (R105). Absent means "no such schedule, or not visible", matching the
    /// port's own fold of both into `None`.
    schedules: Mutex<HashMap<Uuid, ScheduleNotificationSettings>>,
    /// Added by Task 5a. Reproduces the one thing this fake could not
    /// otherwise reproduce: the real local client's own `db.conn()` call.
    ///
    /// In a single-binary deployment the qa-runs client `ClientHub` resolves
    /// is qa-runs' own in-process service, and its `get`
    /// (`qa-runs/src/domain/service/runs.rs:321`) calls **qa-runs'** `db.conn()`
    /// before it ever reaches a repository. `toolkit_db`'s transaction-bypass
    /// guard is task-local rather than instance-local
    /// (`libs/toolkit-db/src/secure/db.rs:10-21` — deliberate defence-in-depth
    /// against a captured `Arc<AppServices>` bypassing a transaction), so that
    /// nested call fails with `DbError::ConnRequestedInsideTx` whenever *this
    /// gear's own* transaction is already open on the same task — indistinguishable,
    /// to the guard, from the bypass it exists to catch.
    ///
    /// `None` (the default, via `#[derive(Default)]`) is every test that
    /// predates this field: `get_run` never touches a `Db` at all, so nothing
    /// about their behaviour changes. `Some(db)`, set through
    /// [`Self::trip_guard_via`], is what lets one test see the bug the others
    /// could not: a fake that answers from memory has nothing to trip the
    /// guard with.
    trip_guard_via: Mutex<Option<toolkit_db::Db>>,
    /// Runs [`RunsReader::list_runs_finished_since`] reports that
    /// [`RunsReader::get_run`] cannot see — see
    /// [`Self::add_finished_run_that_vanishes`] for why this is a second list
    /// rather than a flag on an existing entry.
    phantom_listed: Mutex<Vec<Run>>,
}

impl FakeRuns {
    pub fn add_run(&self, run: Run, results: Vec<RunTestResult>) -> Uuid {
        let id = run.id;
        self.runs.lock().unwrap().insert(id, (run, results));
        id
    }

    /// A run the finished-since listing reports, that a subsequent
    /// [`RunsReader::get_run`] for the same id answers
    /// [`DomainError::RunNotIngested`] — reproducing the race
    /// `read_run_projection`'s `RunNotIngested => Ok(None)` arm exists for: a
    /// run qa-runs reported as finished a moment ago, gone (deleted, or never
    /// really visible to this read) by the time this gear asks for it by id.
    ///
    /// A genuinely separate list rather than an "invisible" flag on
    /// [`Self::runs`]' ordinary entries, because `get_run` and
    /// `list_runs_finished_since` both read `self.runs` today — sharing one
    /// map cannot express "listed, not gettable" no matter what is stored
    /// alongside the entry; the two reads need two different sources to
    /// disagree at all.
    pub fn add_finished_run_that_vanishes(&self, finished_at: OffsetDateTime) -> Uuid {
        let id = Uuid::new_v4();
        let mut run = finished_run(id);
        run.finished_at = Some(finished_at);
        self.phantom_listed.lock().unwrap().push(run);
        id
    }

    /// A finished run with `results` passing tests, and nothing else
    /// distinguishing. The shape most reconcile tests want.
    pub fn add_finished_run_with_results(
        &self,
        finished_at: OffsetDateTime,
        results: usize,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let mut run = finished_run(id);
        run.finished_at = Some(finished_at);
        let rows = (0..results)
            .map(|n| {
                test_row(
                    id,
                    &format!("tests/t{n}.py"),
                    &format!("test_{n}"),
                    "PASSED",
                    "",
                )
            })
            .collect();
        self.add_run(run, rows)
    }

    pub fn fail_result_reads(&self, fail: bool) {
        *self.fail_result_reads.lock().unwrap() = fail;
    }

    pub fn fail_result_reads_for(&self, run_id: Uuid) {
        *self.fail_result_reads_for.lock().unwrap() = Some(run_id);
    }

    pub fn fail_listing(&self, fail: bool) {
        *self.fail_listing.lock().unwrap() = fail;
    }

    pub fn result_reads(&self) -> usize {
        self.result_reads.load(Ordering::SeqCst)
    }

    pub fn listings(&self) -> usize {
        self.listings.load(Ordering::SeqCst)
    }

    /// The `limit` every `list_recent_runs` call asked for, in order.
    pub fn recent_limits(&self) -> Vec<u32> {
        self.recent_limits.lock().unwrap().clone()
    }

    /// Register `schedule_id`'s notification settings — Task 38's fix round
    /// 1 fixture for [`RunsReader::get_schedule_notifications`]. A schedule
    /// never registered here answers `Ok(None)`, matching the port's own
    /// "not found or not visible" fold.
    pub fn add_schedule(&self, schedule_id: Uuid, settings: ScheduleNotificationSettings) {
        self.schedules.lock().unwrap().insert(schedule_id, settings);
    }

    /// Make [`RunsReader::get_run`] call `db.conn()` before answering, exactly
    /// as the real local client's own read does on the far side
    /// (`qa-runs/src/domain/service/runs.rs:321`). Pass the **same** `Db` a
    /// caller's own transaction runs on to reproduce the Task 5a bug: a
    /// caller that holds that transaction open when it calls this fake trips
    /// `toolkit_db`'s transaction-bypass guard, the same way it would against
    /// a real single-binary deployment.
    ///
    /// A flag rather than a default, per this field's own doc: every test
    /// that does not call this keeps testing exactly what it tested before.
    pub fn trip_guard_via(&self, db: toolkit_db::Db) {
        *self.trip_guard_via.lock().unwrap() = Some(db);
    }
}

#[async_trait]
impl RunsReader for FakeRuns {
    async fn get_run(&self, _ctx: &SecurityContext, run_id: Uuid) -> Result<Run, DomainError> {
        // Reproduces the real local client's own `db.conn()`
        // (`qa-runs/src/domain/service/runs.rs:321`) rather than approximating
        // it — see `Self::trip_guard_via`'s doc for the whole mechanism. The
        // translation matches `infra::clients::qa_runs::on_subject`, the real
        // adapter's: neither `NotFound` nor a subject-level refusal, so it
        // becomes `DomainError::Internal("qa-runs read failed: …")`.
        if let Some(db) = self.trip_guard_via.lock().unwrap().as_ref() {
            db.conn()
                .map_err(|err| DomainError::Internal(format!("qa-runs read failed: {err}")))?;
        }
        self.runs
            .lock()
            .unwrap()
            .get(&run_id)
            .map(|(run, _)| run.clone())
            .ok_or(DomainError::RunNotIngested { run_id })
    }

    async fn list_run_test_results(
        &self,
        _ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Vec<RunTestResult>, DomainError> {
        self.result_reads.fetch_add(1, Ordering::SeqCst);
        if *self.fail_result_reads.lock().unwrap()
            || *self.fail_result_reads_for.lock().unwrap() == Some(run_id)
        {
            return Err(DomainError::Internal("qa-runs is unreachable".to_owned()));
        }
        self.runs
            .lock()
            .unwrap()
            .get(&run_id)
            .map(|(_, rows)| rows.clone())
            .ok_or(DomainError::RunNotIngested { run_id })
    }

    async fn list_runs_finished_since(
        &self,
        _ctx: &SecurityContext,
        since: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Run>, DomainError> {
        // Counted before the failure switch: a listing that was *attempted* is
        // what the ordering assertions are about.
        self.listings.fetch_add(1, Ordering::SeqCst);
        if *self.fail_listing.lock().unwrap() {
            return Err(DomainError::Internal("qa-runs is unreachable".to_owned()));
        }
        let mut runs: Vec<Run> = self
            .runs
            .lock()
            .unwrap()
            .values()
            // The contract's two filters, reproduced rather than approximated:
            // `finished_at >= since`, inclusive, and a run that is not yet
            // terminal is never returned.
            .filter(|(run, _)| run.finished_at.is_some_and(|at| at >= since))
            .map(|(run, _)| run.clone())
            .chain(
                // See `Self::add_finished_run_that_vanishes`: reported here,
                // deliberately absent from `get_run`.
                self.phantom_listed
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|run| run.finished_at.is_some_and(|at| at >= since))
                    .cloned(),
            )
            .collect();
        // Oldest first, which the sweep depends on. `run.id` breaks ties so the
        // fake is deterministic; the real client's tiebreak is its own business.
        runs.sort_by(|a, b| a.finished_at.cmp(&b.finished_at).then(a.id.cmp(&b.id)));
        runs.truncate(limit as usize);
        Ok(runs)
    }

    async fn list_recent_runs(
        &self,
        _ctx: &SecurityContext,
        limit: u32,
    ) -> Result<Vec<Run>, DomainError> {
        // Recorded before the failure switch, like `listings`: the ceiling a
        // caller *asked* for is what the assertion is about.
        self.recent_limits.lock().unwrap().push(limit);
        self.listings.fetch_add(1, Ordering::SeqCst);
        if *self.fail_listing.lock().unwrap() {
            return Err(DomainError::Internal("qa-runs is unreachable".to_owned()));
        }
        let mut runs: Vec<Run> = self
            .runs
            .lock()
            .unwrap()
            .values()
            // No state predicate and no `finished_at` predicate, which is the
            // half of this contract that differs from the sweep's: a run that
            // has not finished is exactly what the dashboard's active and queued
            // counts are about, so a fake that dropped it would make those
            // counts untestable.
            .map(|(run, _)| run.clone())
            .collect();
        // **Newest first**, the opposite of the sweep above, because that is the
        // SDK's contract for `list_runs` (`qa-runs-sdk/src/client.rs:42-46`).
        // Keyed on `created_at` rather than `finished_at`, since a run that has
        // not finished has no finish instant; `run.id` breaks ties so the fake
        // is deterministic.
        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        runs.truncate(limit as usize);
        Ok(runs)
    }

    async fn get_schedule_notifications(
        &self,
        _ctx: &SecurityContext,
        schedule_id: Uuid,
    ) -> Result<Option<ScheduleNotificationSettings>, DomainError> {
        Ok(self.schedules.lock().unwrap().get(&schedule_id).cloned())
    }
}

/// 2026-08-18 10:00:00 UTC, the fixture instant this layer's tests share.
#[must_use]
pub fn ts() -> OffsetDateTime {
    time::macros::datetime!(2026-08-18 10:00:00 UTC)
}

/// 2026-08-18 — [`ts`]'s calendar day, and what [`FixedClock`] calls today.
///
/// The same date rather than an independent one on purpose: every analytics
/// window ends today, and a fixture whose rows were stamped one day and whose
/// clock reported another would put those rows off the end of the window. The
/// failure would read as a broken fold.
pub const TODAY: Date = time::macros::date!(2026 - 08 - 18);

/// A [`Clock`] frozen at [`TODAY`].
///
/// # Why this exists and `ts()` is not enough
///
/// The row builders take an instant; the folds take a *date*. Both are needed
/// and they have to agree — see [`TODAY`]. This is the double behind the port,
/// for the tier that holds it: `domain::ports::clock`'s header records that the
/// folds take a [`Date`] and the service holds the [`Clock`], so
/// `domain::service::analytics_tests` constructs this (Task 25b) and Task 22's
/// fold tests mostly pass [`TODAY`] directly.
///
/// A chosen date rather than only the constant, because a window test has to be
/// able to sit a fixture either side of a boundary.
pub struct FixedClock(pub Date);

impl Default for FixedClock {
    fn default() -> Self {
        Self(TODAY)
    }
}

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
    }
}

/// A finished plan run with every denormalized source populated, so a mapper
/// that dropped one is visible rather than merely untested.
#[must_use]
pub fn finished_run(id: Uuid) -> Run {
    Run {
        id,
        name: "smoke-1".to_owned(),
        target: RunTarget::Plan {
            repo_id: Uuid::from_u128(0x30),
            path: "plans/smoke.yaml".to_owned(),
        },
        platform_id: Some(Uuid::from_u128(0x31)),
        test_version: Some("main".to_owned()),
        app_version: Some("9.1.0".to_owned()),
        app_build: Some("9.1.0-4412".to_owned()),
        state: RunState::Succeeded,
        resolved_exclusive: false,
        exclusive_tier: ExclusiveTier::Default,
        is_validation: false,
        parameters: Vec::new(),
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        source: RunSource::Manual,
        schedule_id: None,
        bundle_ids: Vec::new(),
        execution_ref: Some("wf-1".to_owned()),
        log_storage_ref: None,
        timeout_at: None,
        started_at: Some(ts()),
        finished_at: Some(ts()),
        error: None,
        created_at: ts(),
        updated_at: ts(),
    }
}

/// A run in `state`, created `age_secs` seconds **before** [`ts`].
///
/// [`finished_run`]'s counterpart for the dashboard tests, which need runs that
/// are *not* finished: a run in `Created`, `Queued`, `Dispatching` or `Running`
/// has no finish instant, and one that carried a stale `finished_at` from the
/// finished fixture would let an active-run assertion pass for the wrong reason.
///
/// `age_secs` exists because `RunsReader::list_recent_runs` is ordered
/// newest-first and a test asserting *which* ten runs a page holds needs the
/// order to be a fact about the fixture rather than about `Uuid` bytes. A
/// terminal `state` also gets a `finished_at`, so the fixture is never a run in
/// a state it could not be in.
#[must_use]
pub fn run_in_state(state: RunState, age_secs: i64) -> Run {
    let created = ts() - time::Duration::seconds(age_secs);
    let terminal = matches!(
        state,
        RunState::Succeeded
            | RunState::Failed
            | RunState::Canceled
            | RunState::TimedOut
            | RunState::Expired
            | RunState::Error
    );
    Run {
        state,
        created_at: created,
        updated_at: created,
        // A running or terminal run has started; a `Created`, `Queued` or
        // `Dispatching` one has not.
        started_at: (terminal || matches!(state, RunState::Running)).then_some(created),
        finished_at: terminal.then_some(created),
        ..finished_run(Uuid::new_v4())
    }
}

/// One per-test row as qa-runs would report it.
#[must_use]
pub fn test_row(run_id: Uuid, file: &str, name: &str, status: &str, nodeid: &str) -> RunTestResult {
    RunTestResult {
        run_id,
        test_file: file.to_owned(),
        test_name: name.to_owned(),
        status: status.to_owned(),
        duration: Some("1.5s".to_owned()),
        launch_id: None,
        jira_key: None,
        nodeid: nodeid.to_owned(),
        reason: None,
        ticket: None,
    }
}

// ---------------------------------------------------------------------------
// The request-scoped caller's identity, and the `AuthZ` doubles behind it
// ---------------------------------------------------------------------------

/// A `SecurityContext` for `tenant_id` with a fresh random subject — an
/// operator, not this gear's system actor.
///
/// Deliberately *not* built through [`crate::domain::system_actor`]: a test that
/// authorized the rebuild as any of this gear's system actors would be testing a
/// path no HTTP request can take, and would hide the one thing the rebuild does
/// differently from every other caller in this gear.
#[must_use]
pub fn ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("both builder fields are set")
}

/// The decision logic all three `AuthZ` doubles below share: grant, and return
/// the tenant `IN` constraint a real PDP's default tenant-isolation policy
/// returns.
///
/// Tenant is resolved from the explicit PEP tenant context first, falling back
/// to the subject's `tenant_id` property, exactly as a real PDP does; a nil
/// UUID is treated as unset and yields **no** constraint, which is what makes
/// the platform-root case observable rather than silently permissive.
///
/// The compiled scope is applied against a real `SQLite` database by every test
/// in `reconcile_tests`, so an over-permissive scope shows up as a
/// tenant-isolation failure rather than being absorbed by a mock.
#[must_use]
pub fn permissive_response(request: &EvaluationRequest) -> EvaluationResponse {
    let root_id = request
        .context
        .tenant_context
        .as_ref()
        .and_then(|tc| tc.root_id)
        .or_else(|| {
            request
                .subject
                .properties
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .filter(|id| !id.is_nil());

    let constraints = root_id.map_or_else(Vec::new, |id| {
        vec![Constraint {
            predicates: vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [id],
            ))],
        }]
    });

    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints,
            ..Default::default()
        },
    }
}

/// Grants, and compiles to a real `owner_tenant_id IN [subject_tenant]` scope.
/// The default double for the rebuild tests.
pub struct TenantScopedAuthZ;

#[async_trait]
impl AuthZResolverClient for TenantScopedAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(permissive_response(&request))
    }
}

/// Denies every request. The enforcer turns this into `EnforcerError::Denied`,
/// which `DomainError::from` maps to [`DomainError::Forbidden`].
pub struct DenyAllAuthZ;

#[async_trait]
impl AuthZResolverClient for DenyAllAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Grants like [`TenantScopedAuthZ`], and records the `(resource_type, action)`
/// pair of every evaluation it was asked for.
///
/// Records the pair rather than the whole request because those two strings are
/// what a policy is written against, and asserting on a whole
/// `EvaluationRequest` would fail on every unrelated field the SDK adds.
#[derive(Default)]
pub struct RecordingAuthZ {
    asked: Mutex<Vec<(String, String)>>,
}

impl RecordingAuthZ {
    /// Every `(resource_type, action)` asked about, in order.
    pub fn asked(&self) -> Vec<(String, String)> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl AuthZResolverClient for RecordingAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.asked.lock().unwrap().push((
            request.resource.resource_type.clone(),
            request.action.name.clone(),
        ));
        Ok(permissive_response(&request))
    }
}

/// Grants, and returns a constraint on `owner_tenant_id` **and** `resource_id` —
/// a scope narrower than the whole tenant.
///
/// This is the shape a real PDP produces for a policy that scopes a grant down
/// to particular rows, and [`resources::TEST_RESULT`](crate::domain::service::resources::TEST_RESULT)
/// declares `RESOURCE_ID` as supported, so it compiles through rather than being
/// dropped fail-closed. It is the input `domain::service::refuse_scope_beyond_tenant`
/// exists to refuse: the projection write's `DELETE`s are filtered with the full
/// scope while its inserts are not, so a replay under this scope duplicates every
/// row instead of replacing it.
///
/// The `resource_id` is a fresh random UUID and so matches no stored row, which
/// is the realistic case *and* the damaging one — the delete matches nothing.
pub struct ResourceConstrainedAuthZ;

#[async_trait]
impl AuthZResolverClient for ResourceConstrainedAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let mut response = permissive_response(&request);
        for constraint in &mut response.context.constraints {
            constraint.predicates.push(Predicate::In(InPredicate::new(
                pep_properties::RESOURCE_ID,
                [Uuid::new_v4()],
            )));
        }
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// The analytics universe: its port double and the two row builders
// ---------------------------------------------------------------------------

/// One `list_universe` call's two arguments, as [`FakeCatalog::requests`] records
/// them.
///
/// A named alias because `clippy::type_complexity` denies the nested-`Option`
/// tuple inline. Both halves mean "unnarrowed" when `None`, and they mean it
/// differently — `product_id: None` is every product, `branch: None` is each
/// repository's **default** branch — which is the asymmetry
/// `domain::ports::catalog_reader`' header calls out.
pub type UniverseRequest = (Option<Uuid>, Option<String>);

/// The branch [`FakeCatalog`] treats as every repository's default.
///
/// `CatalogReader::list_universe(.., branch = None)` falls back to each
/// repository's own `default_branch`. A `Vec`-backed double has no repository
/// table to hold one, so it holds a single label and documents the gap rather
/// than pretending the fallback is per-repository: the real per-repository
/// resolution is qa-catalog's own — `branch.unwrap_or(repo.default_branch)` at
/// `qa-catalog/src/domain/service/plans.rs:308-311` — and it is tested there, by
/// `list_universe_without_a_branch_uses_the_repository_default`
/// (`qa-catalog/src/domain/service/plans_tests.rs:822`).
pub const DEFAULT_BRANCH: &str = "main";

/// The `repo_id` every [`universe_test_full`] entry carries.
///
/// Exported, added by Task 35, so a caller building a matching bug or run
/// fixture (`domain::service::jira_poller_tests`) does not have to guess the
/// value `universe_test_full` hardcodes.
pub const UNIVERSE_TEST_REPO_ID: Uuid = Uuid::from_u128(0x30);

/// The `plan_path` every [`universe_test_full`] entry carries. [`UNIVERSE_TEST_REPO_ID`]'s
/// reason.
pub const UNIVERSE_TEST_PLAN_PATH: &str = "plans/smoke.yaml";

/// An in-memory qa-catalog universe.
///
/// One `Vec` of `(product_id, branch, entry)` triples. `list_universe` filters it
/// and sorts by `test_name`, which is the SDK's documented ordering
/// (`qa-catalog-sdk/src/client.rs:132-133`, "Rows are ordered by `test_name`,
/// matching legacy's sort").
///
/// **It reproduces the contract, it does not verify it.** A test asserting that
/// this double filters by branch is asserting about these twenty lines; what the
/// analytics tests use it for is to supply a universe the *core* then folds.
#[derive(Default)]
pub struct FakeCatalog {
    entries: Mutex<Vec<(Uuid, String, UniverseTest)>>,
    /// Every `(product_id, branch)` pair a caller listed with, in order.
    ///
    /// Added by Task 25a for two assertions nothing else can make. The first is
    /// that a read happened **at all**: `DashboardService::stats` skips the
    /// catalog entirely when no row was counted, which is legacy's own
    /// short-circuit (`dashboard.rs:498-500`, *"skip the expensive `TEST_META`
    /// parse entirely"*) and is invisible in the payload — the section is empty either
    /// way. The second is *which* read: `None, None` is "every product, each
    /// repository's default branch", and a service that narrowed either argument
    /// would return a smaller universe that still looked plausible.
    requests: Mutex<Vec<UniverseRequest>>,
    /// Makes every [`CatalogReader::list_universe`] fail with the given error.
    ///
    /// Added by Task 25b for the divergence
    /// `domain::service::analytics`' header argues hardest for and that nothing
    /// could reach before: the analytics overview reads the universe
    /// **unconditionally** — it is the denominator of every number in the payload
    /// — so a refused or broken qa-catalog fails the whole response rather than
    /// degrading to an empty universe, which would be indistinguishable from a
    /// deployment with nothing synced. [`FakePlatforms::fail_reads`] is the same
    /// switch one port over, and this one takes the error because the two arms
    /// that matter here are different outcomes for a caller: `Forbidden` is a
    /// missing grant and `Internal` is a broken sibling.
    fail_with: Mutex<Option<CatalogFailure>>,
}

/// How a [`FakeCatalog`] read fails, when it is told to.
///
/// A two-variant enum rather than a stored [`DomainError`] because that type is
/// deliberately not `Clone` — it carries the `thiserror` sources — and a switch
/// that could only fire once would be a fixture that behaves differently on a
/// retry. The two variants are exactly
/// [`CatalogReader::list_universe`]' documented failures.
#[derive(Clone, Copy, Debug)]
pub enum CatalogFailure {
    /// qa-catalog refused the subject.
    Forbidden,
    /// The transport or the far side broke.
    Internal,
}

impl From<CatalogFailure> for DomainError {
    fn from(failure: CatalogFailure) -> Self {
        match failure {
            CatalogFailure::Forbidden => Self::Forbidden,
            CatalogFailure::Internal => Self::Internal("qa-catalog is unreachable".to_owned()),
        }
    }
}

impl FakeCatalog {
    /// Register one universe entry under `product_id` on `branch`.
    pub fn add(&self, product_id: Uuid, branch: &str, test: UniverseTest) {
        self.entries
            .lock()
            .unwrap()
            .push((product_id, branch.to_owned(), test));
    }

    /// Every `(product_id, branch)` a caller listed with, in call order. Empty
    /// means the port was never reached.
    #[must_use]
    pub fn requests(&self) -> Vec<UniverseRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Make every universe read fail. The call is still recorded in
    /// [`Self::requests`], so a test can tell "refused" from "never reached".
    pub fn fail_reads(&self, failure: CatalogFailure) {
        *self.fail_with.lock().unwrap() = Some(failure);
    }

    /// Register a universe entry visible **only** on `branch` — Task 35's
    /// fixture for `the_branch_is_resolved_once_and_reused_for_lookup_and_launch`.
    ///
    /// [`UNIVERSE_TEST_REPO_ID`]/[`UNIVERSE_TEST_PLAN_PATH`], like every
    /// [`universe_test_full`] entry, so a bug fixture built against those same
    /// constants matches it by `repo_id`. The entry is absent from
    /// [`DEFAULT_BRANCH`] and from every other branch on purpose: a poller
    /// that resolved the wrong branch, or resolved twice and got two different
    /// answers, would not find it, which is the failure that test exists to
    /// catch.
    pub fn add_test_on_branch_only(&self, branch: &str, test_file: &str, test_name: &str) {
        self.add(
            Uuid::from_u128(0x40),
            branch,
            universe_test_full(test_file, test_name, None),
        );
    }

    /// The `branch` argument of every [`CatalogReader::list_universe`] call, in
    /// order — [`Self::requests`] narrowed to the half a caller resolving a
    /// branch once cares about. `None` (the fallback-to-default case) is
    /// dropped rather than rendered as a string, since Task 35's own
    /// assertion is about a call that **did** name one.
    #[must_use]
    pub fn lookup_branches(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter_map(|(_, branch)| branch)
            .collect()
    }
}

#[async_trait]
impl CatalogReader for FakeCatalog {
    async fn list_universe(
        &self,
        _ctx: &SecurityContext,
        product_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<Vec<UniverseTest>, DomainError> {
        self.requests
            .lock()
            .unwrap()
            .push((product_id, branch.map(str::to_owned)));
        if let Some(failure) = *self.fail_with.lock().unwrap() {
            return Err(failure.into());
        }
        // `None` is the default branch, **not** every branch — the asymmetry
        // with `UniverseFilter::branch` that the port's doc calls out.
        let wanted_branch = branch.unwrap_or(DEFAULT_BRANCH);
        let mut found: Vec<UniverseTest> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|(product, entry_branch, _)| {
                product_id.is_none_or(|wanted| wanted == *product) && entry_branch == wanted_branch
            })
            .map(|(_, _, test)| test.clone())
            .collect();
        found.sort_by(|a, b| a.test_name.cmp(&b.test_name));
        Ok(found)
    }
}

// ---------------------------------------------------------------------------
// The platform-name resolution: its port double
// ---------------------------------------------------------------------------

/// An in-memory qa-environments platform table, behind [`EnvironmentReader`].
///
/// One `id -> name` map, and the same filtering the real adapter does: an id
/// that is not in the map is **absent from the answer**, which is the port's
/// contract (`EnvironmentReader::names`, "an id that resolves to nothing is absent
/// from the map"). A test that wants the unresolvable case simply does not
/// [`add`](Self::add) the id.
///
/// # It records the batches, and that is the point
///
/// [`Self::batches`] is every `ids` slice a caller handed over, in order. The
/// property this exists to make assertable is the one
/// [`EnvironmentReader`]'s signature is shaped for and nothing else can see: the
/// resolution happens **once per request, over the distinct ids**, and never once
/// per [`ExecRow`]. A service that resolved per row would return the same payload
/// and pass every assertion about its *contents* — the batch log is what fails.
///
/// Same reason `FakeRuns::recent_limits` exists one port over: "which call was
/// made" is a documented property with, until it is recorded, nothing failing if
/// the call quietly changes shape.
#[derive(Default)]
pub struct FakePlatforms {
    names: Mutex<HashMap<Uuid, String>>,
    /// Every `ids` slice [`EnvironmentReader::names`] was called with, in order.
    batches: Mutex<Vec<Vec<Uuid>>>,
    /// Makes every [`EnvironmentReader::names`] fail with
    /// [`DomainError::Forbidden`] — the shape a caller missing the far side's
    /// `platform:list` grant sees, and the one the port's `# Errors` section says
    /// must not degrade to an empty map.
    fail_reads: Mutex<bool>,
    /// Per-platform default-branch overrides — [`EnvironmentReader::default_branch`],
    /// added by Task 35. A second map rather than a second field on
    /// [`Self::names`]' entries: a platform can have a display name and no
    /// branch override, or the reverse, and the port's contract keeps the two
    /// reads independent.
    default_branches: Mutex<HashMap<Uuid, String>>,
}

impl FakePlatforms {
    /// Register one platform's display name.
    pub fn add(&self, platform_id: Uuid, name: &str) {
        self.names
            .lock()
            .unwrap()
            .insert(platform_id, name.to_owned());
    }

    /// Make every resolution fail with [`DomainError::Forbidden`].
    pub fn fail_reads(&self, fail: bool) {
        *self.fail_reads.lock().unwrap() = fail;
    }

    /// Every `ids` slice a caller passed, in call order. Empty means the port
    /// was never reached.
    #[must_use]
    pub fn batches(&self) -> Vec<Vec<Uuid>> {
        self.batches.lock().unwrap().clone()
    }

    /// Register `platform_id`'s default-branch override.
    pub fn set_default_branch(&self, platform_id: Uuid, branch: &str) {
        self.default_branches
            .lock()
            .unwrap()
            .insert(platform_id, branch.to_owned());
    }
}

#[async_trait]
impl EnvironmentReader for FakePlatforms {
    async fn names(
        &self,
        _ctx: &SecurityContext,
        ids: &[Uuid],
    ) -> Result<HashMap<Uuid, String>, DomainError> {
        self.batches.lock().unwrap().push(ids.to_vec());
        if *self.fail_reads.lock().unwrap() {
            return Err(DomainError::Forbidden);
        }
        // The batch is recorded *before* the empty check, deliberately: the real
        // adapter short-circuits an empty `ids` without a round trip
        // (`infra::clients::qa_environments`,
        // `no_ids_means_no_cross_gear_call`), and a test asserting that a service
        // resolved nothing needs to distinguish "called with nothing" from "not
        // called" — which is exactly what an unrecorded empty call would hide.
        let stored = self.names.lock().unwrap();
        Ok(ids
            .iter()
            .filter_map(|id| stored.get(id).map(|name| (*id, name.clone())))
            .collect())
    }

    async fn default_branch(
        &self,
        _ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<Option<String>, DomainError> {
        if *self.fail_reads.lock().unwrap() {
            return Err(DomainError::Forbidden);
        }
        Ok(self
            .default_branches
            .lock()
            .unwrap()
            .get(&platform_id)
            .cloned())
    }
}

/// A universe entry for `test_file` with nothing else distinguishing: its
/// display name is the file stem and it declares no `TEST_META` title.
///
/// The shape most latest-map and fold tests want, where the row already names
/// its file and the alias map is not the subject.
#[must_use]
pub fn universe_test(test_file: &str) -> UniverseTest {
    let stem = test_file
        .rsplit('/')
        .next()
        .unwrap_or(test_file)
        .trim_end_matches(".py");
    universe_test_full(test_file, stem, None)
}

/// A universe entry with all three alias sources chosen: the file, the display
/// name, and the `TEST_META` title.
///
/// The fourth alias — the file *stem* — is derived from `test_file` by
/// `build_alias_map`, so there is nothing to pass for it.
#[must_use]
pub fn universe_test_full(test_file: &str, test_name: &str, title: Option<&str>) -> UniverseTest {
    UniverseTest {
        repo_id: UNIVERSE_TEST_REPO_ID,
        plan_path: UNIVERSE_TEST_PLAN_PATH.to_owned(),
        plan_name: "Smoke".to_owned(),
        test_file: test_file.to_owned(),
        test_name: test_name.to_owned(),
        title_alias: title.map(str::to_owned),
        component: None,
        tags: Vec::new(),
        quality_vectors: Vec::new(),
        source: SOURCE_REPO.to_owned(),
        versions: Vec::new(),
        static_case_count: 1,
    }
}

/// One executed row: `test_file` reported verbatim, `status` verbatim, at `at`.
///
/// `test_name` is derived from the file rather than taken as a parameter,
/// because a row that names its file never consults the alias map — the tests
/// that *are* about the alias map build their rows inline with the empty
/// `test_file` the column actually stores.
#[must_use]
pub fn exec_row_at(test_file: &str, status: &str, at: OffsetDateTime) -> ExecRow {
    ExecRow {
        run_id: Uuid::new_v4(),
        test_file: test_file.to_owned(),
        test_name: test_file.to_owned(),
        status: status.to_owned(),
        build: Some("9.1.0-4412".to_owned()),
        platform_id: Some(Uuid::from_u128(0x31)),
        ts: at,
        day: at.date(),
    }
}

// ---------------------------------------------------------------------------
// The full-gear fixture, added by Task 7
// ---------------------------------------------------------------------------

/// [`RunsLauncher`] double that panics if called.
///
/// [`Fleet`]'s tests exercise `CollectService::record_count`, never `trigger`
/// or the collect cycle, so nothing behind [`Fleet::services`] should ever
/// reach a launcher — [`domain::local_client::client`](crate::domain::local_client::client)'s
/// `UnusedJiraClient` is this same idiom one port over: a double that
/// plausibly answered would turn a wiring mistake into a silently wrong
/// fixture instead of a loud failure.
struct UnreachableRunsLauncher;

#[async_trait]
impl RunsLauncher for UnreachableRunsLauncher {
    async fn launch_collect(
        &self,
        _ctx: &SecurityContext,
        _repo_id: Uuid,
        _branch: &str,
        _collect_url: &str,
    ) -> Result<(), DomainError> {
        unreachable!("Fleet's tests do not launch a collect run")
    }

    async fn launch_test(
        &self,
        _ctx: &SecurityContext,
        _repo_id: Uuid,
        _plan_path: &str,
        _test_file: &str,
        _platform_id: Option<Uuid>,
        _branch: Option<&str>,
    ) -> Result<(), DomainError> {
        unreachable!("Fleet's tests do not launch a single-test run")
    }
}

/// [`JiraClient`] double, [`UnreachableRunsLauncher`]'s reason: nothing built
/// over [`Fleet`] yet files a bug or polls JIRA status.
struct UnreachableJiraClient;

#[async_trait]
impl JiraClient for UnreachableJiraClient {
    async fn create_or_find_issue(
        &self,
        _ctx: &SecurityContext,
        _config: &qa_insights_sdk::JiraConfig,
        _issue: NewIssue,
    ) -> Result<IssueRef, DomainError> {
        unreachable!("Fleet's tests do not file or search for a bug")
    }

    async fn check_status(
        &self,
        _ctx: &SecurityContext,
        _config: &qa_insights_sdk::JiraConfig,
        _jira_key: &str,
    ) -> Result<StatusCategory, DomainError> {
        unreachable!("Fleet's tests do not poll JIRA status")
    }

    async fn get_issue(
        &self,
        _ctx: &SecurityContext,
        _config: &qa_insights_sdk::JiraConfig,
        _jira_key: &str,
    ) -> Result<JiraIssue, DomainError> {
        unreachable!("Fleet's tests do not read a JIRA issue")
    }
}

/// [`SlackClient`] double, [`UnreachableRunsLauncher`]'s reason: nothing
/// built over [`Fleet`] yet sends a notification.
///
/// Unlike [`Fleet`]'s `mail_client`, there is no inert *production* adapter to
/// reuse here — `infra::notify::SlackOagwClient` genuinely dials out — so this
/// double is [`Fleet`]'s own, not a second copy of one qa-insights already
/// ships.
struct UnreachableSlackClient;

#[async_trait]
impl SlackClient for UnreachableSlackClient {
    async fn send(
        &self,
        _ctx: &SecurityContext,
        _message: &SlackMessage,
    ) -> Result<SendOutcome, DomainError> {
        unreachable!("Fleet's tests do not send a Slack message")
    }
}

/// One in-memory database and one [`ConcreteAppServices`] wired over it with
/// this gear's **real** repositories — the fixture the HTTP-boundary suites
/// under `api::rest::handlers` drive their handlers against, rather than
/// through a fake standing in for the whole write path.
///
/// Modeled on `qa-runs`'s own `Fleet`
/// (`qa-runs/qa-runs/src/domain/service/test_support.rs:1898`, "one database
/// and one set of collaborators, over which any number of instances can be
/// built"). This gear has no second *instance* to build — nothing here plays
/// qa-runs' replica role — so [`Fleet::services`] is a plain field rather
/// than a per-call constructor; what carries over is the rest of that
/// struct's shape: one shared database, real repositories over it, and every
/// dependency [`AppServices::new`] cannot be built without.
///
/// # Everything but the signing secret is a permissive or inert double
///
/// `signing_secret` is the one collaborator Task 7's tests vary — see
/// [`Self::new`]. Every other [`ServiceDeps`] field is a double that answers
/// plausibly and that nothing built over [`Fleet`] yet exercises:
/// [`TenantScopedAuthZ`] grants, [`FakeRuns`]/[`FakeCatalog`]/[`FakePlatforms`]
/// hold nothing, [`FixedClock`] reports [`TODAY`], and
/// [`UnreachableRunsLauncher`]/[`UnreachableJiraClient`]/[`UnreachableSlackClient`]
/// panic rather than answer — [`UnusedJiraClient`](crate::domain::local_client::client)'s
/// own doc gives the reasoning: a double that answered plausibly would hide a
/// wiring mistake as a silently wrong fixture rather than surface it as a
/// failure. The one exception is `mail_client`: `infra::notify`'s
/// [`UnsupportedMailClient`] is already the *production* answer for a
/// deployment with no SMTP relay configured, so it is reused rather than
/// doubled a second time.
///
/// A future caller that needs one of these doubles to answer for real — Task
/// 8's notify suite, most likely, once it reaches the send-once path — extends
/// this struct rather than reaching around it, which is the whole reason it
/// lives here and not inside `collect_handler_tests.rs`.
pub struct Fleet {
    db: Arc<DbProvider>,
    /// The DI container every handler suite drives directly.
    pub(crate) services: Arc<ConcreteAppServices>,
}

impl Fleet {
    /// Build with `signing_secret` as `collect_report_signing_secret` — the
    /// one dependency Task 7's tests vary. See this struct's own doc for
    /// every other collaborator.
    pub async fn new(signing_secret: &str) -> Self {
        let db = Arc::new(DbProvider::new(inmem_db().await));

        let deps = ServiceDeps {
            db: Arc::clone(&db),
            authz: Arc::new(TenantScopedAuthZ) as Arc<dyn AuthZResolverClient>,
            runs: Arc::new(FakeRuns::default()) as Arc<dyn RunsReader>,
            catalog: Arc::new(FakeCatalog::default()) as Arc<dyn CatalogReader>,
            platforms: Arc::new(FakePlatforms::default()) as Arc<dyn EnvironmentReader>,
            clock: Arc::new(FixedClock::default()) as Arc<dyn Clock>,
            runs_launcher: Arc::new(UnreachableRunsLauncher) as Arc<dyn RunsLauncher>,
            jira_client: Arc::new(UnreachableJiraClient) as Arc<dyn JiraClient>,
            slack_client: Arc::new(UnreachableSlackClient) as Arc<dyn SlackClient>,
            mail_client: Arc::new(UnsupportedMailClient) as Arc<dyn MailClient>,
            reconcile_lookback: Duration::seconds(3600),
            reconcile_page_size: 100,
            default_collect_branch: DEFAULT_BRANCH.to_owned(),
            collect_report_base_url: "https://qa-insights.example.test".to_owned(),
            collect_report_signing_secret: signing_secret.to_owned(),
        };

        let services = Arc::new(AppServices::new(
            OrmResultsRepository,
            OrmWatermarkRepository,
            OrmSavedViewsRepository,
            OrmCollectRepository,
            OrmJiraRepository,
            OrmNotifyRepository,
            deps,
        ));

        Self { db, services }
    }

    /// Every count row actually persisted for `(repo_id, branch)` under
    /// `tenant_id` — read back through the real [`OrmCollectRepository`]
    /// this [`Fleet`] wired [`Self::services`] with, not tracked in memory
    /// by a second, competing fixture.
    ///
    /// This is the load-bearing half of the "a bad signature writes nothing"
    /// assertion: a handler that wrote the row and only then checked the
    /// signature would still pass a status-only test, and would still pass a
    /// count tracked by a fake that the write path never actually reached.
    /// Reading the real repository, over the same database
    /// [`Self::services`] writes through, is what makes the absence of a row
    /// here a fact about persistence rather than about this fixture's own
    /// bookkeeping.
    pub async fn collect_counts(
        &self,
        tenant_id: Uuid,
        repo_id: Uuid,
        branch: &str,
    ) -> Vec<CollectCount> {
        let conn = self
            .db
            .conn()
            .expect("the fixture's own database connection must be reachable");
        OrmCollectRepository
            .list_counts_for(&conn, &scope(tenant_id), &[repo_id], branch)
            .await
            .expect("the fixture's own read must not fail")
    }
}
