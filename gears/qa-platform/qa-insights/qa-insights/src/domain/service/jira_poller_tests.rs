//! Tests for the JIRA poller — Task 35.
//!
//! Against the **real** repositories (`OrmJiraRepository`,
//! `OrmResultsRepository`) on in-memory `SQLite`, `jira_tests`' shape: the
//! property under test is whether this service's calls into
//! [`JiraService`] actually reach the rows a real scope and a real repository
//! store, not whether a hand-rolled double agrees with itself. JIRA itself is
//! doubled — [`FakeJiraStatus`] — because this module's job is the poller's
//! own orchestration, not the wire format `infra::jira::oagw_client`'s own
//! tests cover. qa-catalog, qa-environments and qa-runs are doubled too —
//! [`FakeCatalog`], [`FakePlatforms`], [`FakeLauncher`] — for
//! `collect_tests`' identical reason: this module's job is what this service
//! does with those three ports, not their own contracts.
//!
//! The four tests named in this task's brief are the first four below, each
//! with the brief's own doc comment, verbatim.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use qa_insights_sdk::{JiraConfig, JiraPollerConfig, NewJiraBug};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_db::DBProvider;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::JiraPollerService;
use crate::domain::error::DomainError;
use crate::domain::metrics::{
    QA_INSIGHTS_JIRA_BUG, QA_INSIGHTS_JIRA_POLL, QA_INSIGHTS_JIRA_POLL_DURATION,
    QA_INSIGHTS_JIRA_RERUN,
};
use crate::domain::ports::jira_client::{
    IssueRef, JiraClient, JiraIssue, NewIssue, StatusCategory,
};
use crate::domain::ports::metrics::{JiraBugOutcome, JiraPollMetrics, JiraPollOutcome};
use crate::domain::ports::{CatalogReader, EnvironmentReader, RunsLauncher};
use crate::domain::repos::{JiraRepository, NewTestResult, ResultsRepository};
use crate::domain::service::jira::{JiraConfigInput, JiraService};
use crate::domain::service::test_support::{
    DEFAULT_BRANCH, FakeCatalog, FakePlatforms, SlowCatalog, TenantScopedAuthZ,
    UNIVERSE_TEST_PLAN_PATH, UNIVERSE_TEST_REPO_ID, ctx,
};
use crate::infra::leader::{LeaderElector, LeaderWorkFn, NoopLeaderElector, work_fn};
use crate::infra::metrics::probe::MetricsProbe;
use crate::infra::storage::jira_sea_repo::OrmJiraRepository;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{inmem_db, scope};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

const TENANT: Uuid = Uuid::from_u128(0x0A);

/// The bug's own recorded version, older than every "new build" fixture
/// stores.
const OLD_VERSION: &str = "1.0.0";
/// The version a fixture writes to simulate a new build having landed.
const NEW_VERSION: &str = "2.0.0";

/// The one `jira_key` every fixture in this file uses — matching the brief's
/// own `f.bug_is_resolved("V-1")`.
const JIRA_KEY: &str = "V-1";

// ---------------------------------------------------------------------------
// The JIRA double: a scripted status category, nothing else
// ---------------------------------------------------------------------------

/// A rendezvous point for two replicas polling the same database.
///
/// **Task 26's, and it is what makes
/// [`two_concurrent_pollers_produce_one_rerun`] a guard rather than a coin
/// flip.** The double-launch that finding #5 is about needs both replicas to
/// have read `open_bugs` *before* either writes `resolve_bug`; if the first
/// replica gets all the way to its resolve write first, the second sees no open
/// bug and does not launch. That is the "race, not a lock" `crate::gear`'s
/// `jira_poller_ticker` doc describes, and it means the unguarded code
/// sometimes produces one launch and sometimes two — measured, both outcomes
/// observed on this fixture before this type existed.
///
/// [`Self::arrive`] holds each caller until its peer arrives, or until the
/// timeout, so the interleaving happens every run. The timeout is what lets it
/// work in the fixed direction too: with an elector, the loser never arrives,
/// and the winner must not wait forever for it.
///
/// # Single-shot, on purpose, and it is one bug's worth of purpose
///
/// [`Self::arrived`] is never reset, so a *second* `check_status` anywhere in
/// the process walks straight through. That is deliberate and it is exactly
/// sized to the one fixture that uses it:
/// [`two_pollers_over_one_database`] stages **one** bug and two replicas, so a
/// complete run is two arrivals and `expected` is 2. It is also the safe
/// direction to be wrong in — a stale count can only make the rendezvous
/// *weaker*, never make it park a caller that should have proceeded.
///
/// A second bug, or a second pass, would need a generation counter here
/// (`arrived / expected` as the round, or a `tokio::sync::Barrier` per round).
/// Adding one now would be untested machinery; this comment is the marker for
/// whoever needs it.
///
/// The 5ms poll rather than a `Notify` is the same trade: a condvar-shaped
/// rendezvous is the right structure and buys nothing at two parties and one
/// round, where the poll is four lines with no wake-up ordering to get wrong.
struct Rendezvous {
    /// Monotonic — see the type's own doc for why it is never reset.
    arrived: std::sync::atomic::AtomicUsize,
    expected: usize,
    timeout: std::time::Duration,
}

impl Rendezvous {
    async fn arrive(&self) {
        use std::sync::atomic::Ordering;
        self.arrived.fetch_add(1, Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + self.timeout;
        while self.arrived.load(Ordering::SeqCst) < self.expected
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

/// A [`JiraClient`] double that answers one scripted status category to every
/// `check_status` call and records the keys it was asked about.
///
/// Only [`JiraClient::check_status`] is implemented: this task's poller is
/// documented (`domain::service::jira`'s header) to call exactly that one
/// method, never [`JiraClient::create_or_find_issue`] or
/// [`JiraClient::get_issue`] — a poller does not file bugs.
struct FakeJiraStatus {
    category: StatusCategory,
    asked: Mutex<Vec<String>>,
    /// `None` for every fixture but the two-replica one — see [`Rendezvous`].
    ///
    /// It hangs off *this* double because `check_status` is the first thing a
    /// pass does after `open_bugs` and the last thing it does before
    /// `resolve_bug`: parking here is exactly the window the race needs open.
    rendezvous: Option<Arc<Rendezvous>>,
    /// Makes every `check_status` fail. Added by Task 38 of the observability
    /// plan: the poller logs and skips a failed status check, so before the
    /// per-bug counter existed there was no observable difference between a
    /// pass whose every check failed and a pass whose every bug was still
    /// open, and therefore nothing to write a test against.
    fail_checks: AtomicBool,
}

impl FakeJiraStatus {
    /// Make every subsequent `check_status` fail with an opaque internal
    /// error. The failure is generic on purpose: the poller does not branch on
    /// which error it was, it logs whatever it got and skips the bug.
    fn fail_status_checks(&self) {
        self.fail_checks.store(true, AtomicOrdering::SeqCst);
    }
}

#[async_trait]
impl JiraClient for FakeJiraStatus {
    async fn create_or_find_issue(
        &self,
        _ctx: &SecurityContext,
        _config: &JiraConfig,
        _issue: NewIssue,
    ) -> Result<IssueRef, DomainError> {
        unimplemented!("the poller never files a bug")
    }

    async fn check_status(
        &self,
        _ctx: &SecurityContext,
        _config: &JiraConfig,
        jira_key: &str,
    ) -> Result<StatusCategory, DomainError> {
        if let Some(rendezvous) = &self.rendezvous {
            rendezvous.arrive().await;
        }
        self.asked.lock().unwrap().push(jira_key.to_owned());
        if self.fail_checks.load(AtomicOrdering::SeqCst) {
            return Err(DomainError::Internal("JIRA is unreachable".to_owned()));
        }
        Ok(self.category.clone())
    }

    async fn get_issue(
        &self,
        _ctx: &SecurityContext,
        _config: &JiraConfig,
        _jira_key: &str,
    ) -> Result<JiraIssue, DomainError> {
        unimplemented!("nothing in this task calls it")
    }
}

// ---------------------------------------------------------------------------
// The launcher double: records every launch, and how it was launched
// ---------------------------------------------------------------------------

/// One [`RunsLauncher`] call this double was asked to make.
///
/// `bypassed_admission` is `true` only for a
/// [`RunsLauncher::launch_collect`] call — never for
/// [`RunsLauncher::launch_test`] — so `an_auto_rerun_goes_through_the_normal_launch_path`
/// has a real signal to assert against rather than a field this double always
/// sets one way: a poller that (by defect) reached for the collect-shaped
/// method instead of the test-shaped one would flip this to `true` and the
/// test would catch it.
#[derive(Clone, Debug)]
struct RecordedLaunch {
    repo_id: Uuid,
    plan_path: String,
    test_file: String,
    #[expect(
        dead_code,
        reason = "carried for completeness; no test in this file reads it"
    )]
    platform_id: Option<Uuid>,
    branch: Option<String>,
    bypassed_admission: bool,
}

#[derive(Default)]
struct FakeLauncher {
    launches: Mutex<Vec<RecordedLaunch>>,
    /// Makes every `launch_test` fail, while still recording the attempt.
    ///
    /// Added by Task 38 of the observability plan, and the *recording* half is
    /// what it is for: a refused launch is still a call into qa-runs'
    /// admission path, so the rerun counter must count it, and only a double
    /// that records before it fails can tell a counted attempt from an
    /// uncounted one.
    fail_launches: AtomicBool,
}

impl FakeLauncher {
    fn launches(&self) -> usize {
        self.launches.lock().unwrap().len()
    }

    /// Make every subsequent `launch_test` fail, after recording it.
    fn fail_test_launches(&self) {
        self.fail_launches.store(true, AtomicOrdering::SeqCst);
    }

    /// The most recent launch. Panics if none was made — every caller of this
    /// method has already asserted [`Self::launches`] is at least `1`.
    fn last_launch(&self) -> RecordedLaunch {
        self.launches
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("no launch was recorded")
    }
}

#[async_trait]
impl RunsLauncher for FakeLauncher {
    async fn launch_collect(
        &self,
        _ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        _collect_url: &str,
    ) -> Result<(), DomainError> {
        self.launches.lock().unwrap().push(RecordedLaunch {
            repo_id,
            plan_path: String::new(),
            test_file: String::new(),
            platform_id: None,
            branch: Some(branch.to_owned()),
            bypassed_admission: true,
        });
        Ok(())
    }

    async fn launch_test(
        &self,
        _ctx: &SecurityContext,
        repo_id: Uuid,
        plan_path: &str,
        test_file: &str,
        platform_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<(), DomainError> {
        self.launches.lock().unwrap().push(RecordedLaunch {
            repo_id,
            plan_path: plan_path.to_owned(),
            test_file: test_file.to_owned(),
            platform_id,
            branch: branch.map(str::to_owned),
            bypassed_admission: false,
        });
        if self.fail_launches.load(AtomicOrdering::SeqCst) {
            return Err(DomainError::Internal(
                "qa-runs refused the launch".to_owned(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Fixture {
    /// `Arc` rather than a plain value since Task 26: [`Fixture::work`] hands
    /// the service to a `LeaderWorkFn`, which is `'static`. Every existing
    /// `f.service.poll_once(...)` call site reads the same through `Deref`.
    service: Arc<JiraPollerService<OrmJiraRepository, OrmResultsRepository>>,
    jira_service: Arc<JiraService<OrmJiraRepository, OrmResultsRepository>>,
    jira_client: Arc<FakeJiraStatus>,
    ctx: SecurityContext,
    launcher: Arc<FakeLauncher>,
    catalog: Arc<FakeCatalog>,
    platforms: Arc<FakePlatforms>,
    db: Arc<DBProvider<DomainError>>,
    /// Set by [`fixture_with_platform_branch`]; unused (and left nil) by every
    /// other fixture in this file, which files its bug with no platform at
    /// all — legacy's own `platform: None` branch.
    platform_id: Uuid,
    /// The elector this fixture's replica contends under.
    ///
    /// **Task 26's, and the only field here that is not a collaborator of the
    /// service.** Every test above this one drives `poll_once` directly and
    /// ignores it; `two_concurrent_pollers_produce_one_rerun` is the one that
    /// goes through `run_role`, because leadership is the thing it measures.
    /// [`build_with_authz`] leaves it a
    /// [`NoopLeaderElector`](crate::infra::leader::NoopLeaderElector), which is
    /// what `crate::gear` gives the *other* two tickers.
    #[cfg_attr(
        not(feature = "integration"),
        expect(
            dead_code,
            reason = "read only by two_concurrent_pollers_produce_one_rerun, which needs a \
                      shared Postgres and is therefore integration-gated"
        )
    )]
    elector: Arc<dyn LeaderElector>,
    /// The real `OpenTelemetry` adapter this fixture's poller emits through,
    /// over a meter provider private to this fixture.
    ///
    /// **Every fixture carries one, whether or not its test reads it.** An
    /// emission is silent when no adapter is installed, so a metric assertion
    /// made against a service built without one would pass while measuring
    /// nothing; installing the real adapter everywhere means the assertions
    /// below read the same code path production runs, and the tests that
    /// ignore it exercise it anyway.
    ///
    /// Private per fixture, not shared: a counter is cumulative, so a shared
    /// provider would make every `assert_eq!(.., 1)` here order-dependent.
    metrics: MetricsProbe,
}

fn stored_jira_config() -> JiraConfigInput {
    JiraConfigInput {
        url: "https://jira.example.com".to_owned(),
        project_key: "VHP".to_owned(),
        email: "qa@example.com".to_owned(),
        api_token_credstore_ref: "cred://qa-jira-api-token".to_owned(),
        issue_type: Some("Bug".to_owned()),
        enabled: true,
    }
}

/// Every fixture but [`no_jira_config_at_all_is_a_silent_no_op`] wants JIRA
/// enabled and a resolved-status double; that test builds its own `Fixture`
/// by hand instead, since an unconfigured tenant is its whole point.
async fn build() -> Fixture {
    build_with_category(StatusCategory::DONE, true).await
}

/// The general constructor every other builder in this file goes through.
///
/// `category` is the status [`FakeJiraStatus`] answers every `check_status`
/// call with; `configured` decides whether a `qa_jira_config` row is saved at
/// all, which is what [`no_jira_config_at_all_is_a_silent_no_op`] needs to
/// vary.
async fn build_with_category(category: &str, configured: bool) -> Fixture {
    build_with_authz(category, configured, Arc::new(TenantScopedAuthZ)).await
}

/// [`build_with_category`] with the PDP double as a parameter, so
/// [`a_pass_does_not_touch_another_tenants_open_bug`] can compile a scope that
/// spans two tenants. Every other fixture in this file goes through
/// [`build_with_category`]'s single-tenant [`TenantScopedAuthZ`].
async fn build_with_authz(
    category: &str,
    configured: bool,
    authz: Arc<dyn authz_resolver_sdk::AuthZResolverApi>,
) -> Fixture {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    build_on(
        db,
        Arc::new(NoopLeaderElector),
        None,
        category,
        configured,
        authz,
    )
    .await
}

/// [`build_with_authz`] with the database and the elector as parameters.
///
/// **Task 26's, and the only reason it exists is that a leadership test needs
/// two fixtures over *one* database.** Every other builder in this file makes
/// its own in-memory `SQLite`, which is right for them and useless here: two
/// pollers with a database each both win trivially and the test proves
/// nothing. Splitting the constructor rather than parameterising the existing
/// one keeps that argument at the one call site that needs it.
async fn build_on(
    db: Arc<DBProvider<DomainError>>,
    elector: Arc<dyn LeaderElector>,
    rendezvous: Option<Arc<Rendezvous>>,
    category: &str,
    configured: bool,
    authz: Arc<dyn authz_resolver_sdk::AuthZResolverApi>,
) -> Fixture {
    let jira_client = Arc::new(FakeJiraStatus {
        category: StatusCategory::new(category),
        asked: Mutex::new(Vec::new()),
        rendezvous,
        fail_checks: AtomicBool::new(false),
    });
    let jira_service = Arc::new(JiraService::new(
        Arc::clone(&db),
        OrmJiraRepository,
        OrmResultsRepository,
        Arc::clone(&jira_client) as Arc<dyn JiraClient>,
        PolicyEnforcer::new(authz),
    ));
    let catalog = Arc::new(FakeCatalog::default());
    let platforms = Arc::new(FakePlatforms::default());
    let launcher = Arc::new(FakeLauncher::default());
    let metrics = MetricsProbe::new();
    let service = Arc::new(JiraPollerService::new(
        Arc::clone(&jira_service),
        Arc::clone(&catalog) as Arc<dyn CatalogReader>,
        Arc::clone(&platforms) as Arc<dyn EnvironmentReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
        Some(metrics.adapter()),
    ));
    let ctx = ctx(TENANT);

    if configured {
        // Saved through the service's own write path so no fixture in this
        // file can pass against a write shape production does not use.
        jira_service
            .save_jira_config(&ctx, stored_jira_config())
            .await
            .expect("saving the jira config must succeed");
    }

    Fixture {
        service,
        jira_service,
        jira_client,
        ctx,
        launcher,
        catalog,
        platforms,
        db,
        platform_id: Uuid::nil(),
        elector,
        metrics,
    }
}

impl Fixture {
    /// File [`JIRA_KEY`] against `test_name`, with `app_version` and
    /// `platform_id` as given — always under [`UNIVERSE_TEST_REPO_ID`]/
    /// [`UNIVERSE_TEST_PLAN_PATH`], so a catalog entry built from
    /// `universe_test_full` (via [`FakeCatalog::add_test_on_branch_only`])
    /// matches it by `repo_id`.
    async fn file_bug(
        &self,
        test_name: &str,
        app_version: Option<&str>,
        platform_id: Option<Uuid>,
    ) {
        let conn = self.db.conn().unwrap();
        let tenant_scope = scope(TENANT);
        OrmJiraRepository
            .upsert_bug(
                &conn,
                &tenant_scope,
                TENANT,
                NewJiraBug {
                    jira_key: JIRA_KEY.to_owned(),
                    test_name: test_name.to_owned(),
                    repo_id: UNIVERSE_TEST_REPO_ID,
                    plan_path: UNIVERSE_TEST_PLAN_PATH.to_owned(),
                    app_version: app_version.map(str::to_owned),
                    platform_id,
                    summary: "s".to_owned(),
                },
            )
            .await
            .expect("filing the fixture bug must succeed");
    }

    /// Record one ingested result carrying `product_version`, against the
    /// same `(repo_id, plan_path)` every bug in this file is filed under —
    /// [`JiraService::latest_version_for_plan`]'s read.
    async fn record_build(&self, product_version: &str, branch: Option<&str>) {
        let conn = self.db.conn().unwrap();
        let tenant_scope = scope(TENANT);
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &tenant_scope,
                TENANT,
                Uuid::new_v4(),
                vec![NewTestResult {
                    test_file: "tests/build_marker.py".to_owned(),
                    test_name: "build_marker".to_owned(),
                    status: "PASSED".to_owned(),
                    duration: None,
                    launch_id: None,
                    jira_key: None,
                    product_version: Some(product_version.to_owned()),
                    app_build: None,
                    platform_id: None,
                    repo_id: Some(UNIVERSE_TEST_REPO_ID),
                    plan_path: Some(UNIVERSE_TEST_PLAN_PATH.to_owned()),
                    branch: branch.map(str::to_owned),
                    run_finished_at: None,
                    // `list_for_plan`'s window (`kpi_window`) matches a row
                    // with **both** timestamps `NULL` on neither of its two
                    // branches — the "unfinished" arm needs `run_created_at`
                    // to compare against `since`. A run always has one, so a
                    // fixture with neither is not a shape production ever
                    // produces; this is the minimum that makes the row
                    // reachable at all.
                    run_created_at: Some(OffsetDateTime::now_utc()),
                }],
                Vec::new(),
            )
            .await
            .expect("recording the fixture build must succeed");
    }

    /// Whether [`JIRA_KEY`] is stored as resolved — a direct repository read,
    /// deliberately not through [`JiraPollerService`]: the property under
    /// test is what the poller *wrote*, and reading it back through the same
    /// service under test would not distinguish a real write from a
    /// no-op that happened to leave the row already looking right.
    async fn bug_is_resolved(&self, jira_key: &str) -> bool {
        let conn = self.db.conn().unwrap();
        let tenant_scope = scope(TENANT);
        OrmJiraRepository
            .find_by_key(&conn, &tenant_scope, TENANT, jira_key)
            .await
            .expect("reading the bug back must succeed")
            .is_some_and(|bug| bug.status == "Resolved")
    }
}

// ---------------------------------------------------------------------------
// Fixture builders named by the brief
// ---------------------------------------------------------------------------

/// A resolved bug (`app_version = OLD_VERSION`) with no build recorded for its
/// plan at all — `latest_version_for_plan` answers `None`, so D8's gate never
/// sees a new build.
///
/// **A matching catalog entry is registered anyway**, on purpose: if D8's
/// gate were ever bypassed, this fixture must be able to reach a real
/// launch, or a defect that dropped the gate entirely would pass this test
/// by accident (no catalog entry) rather than because D8 actually stopped it.
async fn fixture_with_resolved_bug_and_no_new_build() -> Fixture {
    let f = build().await;
    f.file_bug("T1", Some(OLD_VERSION), None).await;
    f.catalog
        .add_test_on_branch_only(DEFAULT_BRANCH, "tests/t1.py", "T1");
    f
}

/// A resolved bug with auto-rerun turned off — the switch this fixture exists
/// to disable, saved through the service's own write path.
///
/// **A new build and a matching catalog entry are registered too**, for
/// [`fixture_with_resolved_bug_and_no_new_build`]'s own reason: this fixture
/// must be able to reach a real launch if the switch were ignored, so that
/// `a_bug_resolves_even_when_auto_rerun_is_off`'s `launches() == 0` is
/// evidence the switch worked, not evidence D8 or the catalog happened to
/// block it for an unrelated reason.
async fn fixture_with_resolved_bug_auto_rerun_off() -> Fixture {
    let f = build().await;
    f.jira_service
        .save_poller_config(
            &f.ctx,
            JiraPollerConfig {
                poll_interval_seconds: 300,
                auto_rerun_on_resolve: false,
            },
        )
        .await
        .expect("saving the poller config must succeed");
    f.file_bug("T1", Some(OLD_VERSION), None).await;
    f.record_build(NEW_VERSION, None).await;
    f.catalog
        .add_test_on_branch_only(DEFAULT_BRANCH, "tests/t1.py", "T1");
    f
}

/// A resolved bug with a genuinely newer build recorded for its plan, and a
/// catalog entry on the repository default branch (`platform_id: None`, so
/// the poller never resolves an override) that declares the same test name —
/// everything the rerun needs to actually launch.
async fn fixture_with_resolved_bug_and_new_build() -> Fixture {
    let f = build().await;
    f.file_bug("T1", Some(OLD_VERSION), None).await;
    f.record_build(NEW_VERSION, None).await;
    f.catalog
        .add_test_on_branch_only(DEFAULT_BRANCH, "tests/t1.py", "T1");
    f
}

/// A fixture whose bug is filed against a **platform**, so the branch the
/// rerun must resolve is the platform's default-branch override rather than
/// `None` — the shape `the_branch_is_resolved_once_and_reused_for_lookup_and_launch`
/// needs. The bug itself is added separately, by
/// [`Fixture::add_resolved_bug_with_new_build`], matching the brief's own
/// two-call fixture shape.
async fn fixture_with_platform_branch(branch: &str) -> Fixture {
    let mut f = build().await;
    let platform_id = Uuid::new_v4();
    f.platforms.set_default_branch(platform_id, branch);
    f.platform_id = platform_id;
    f
}

impl Fixture {
    /// File a resolved bug for `test_name`, with a newer build recorded on
    /// `branch` — against [`Self::platform_id`], which
    /// [`fixture_with_platform_branch`] already registered a default-branch
    /// override for.
    async fn add_resolved_bug_with_new_build(&self, test_name: &str, branch: &str) {
        self.file_bug(test_name, Some(OLD_VERSION), Some(self.platform_id))
            .await;
        self.record_build(NEW_VERSION, Some(branch)).await;
    }

    /// One poll pass, wrapped as the work a [`LeaderElector`] runs, ending the
    /// term by cancelling `stop`.
    ///
    /// **The cancellation is not decoration.** `ClaimRowElector::run_role`
    /// loops until its token fires — it has to, or a replica that lost once
    /// could never take over from a leader that died, which is the entire
    /// point of the thing — so a term with no shutdown in it does not end. The
    /// gear's real `jira_poller_ticker` is an infinite `tokio::time::interval`
    /// loop that returns on exactly this token; a fixture's pass is one tick of
    /// it, so it fires the same token when the tick is done. Handing both
    /// replicas the *same* token is what makes "the winner finished, so we are
    /// shutting down" reach the loser.
    #[cfg_attr(
        not(feature = "integration"),
        expect(
            dead_code,
            reason = "called only by two_concurrent_pollers_produce_one_rerun, which needs a \
                      shared Postgres and is therefore integration-gated"
        )
    )]
    fn work(&self, stop: CancellationToken) -> LeaderWorkFn {
        let service = Arc::clone(&self.service);
        let ctx = self.ctx.clone();
        work_fn(move |_term| {
            let service = Arc::clone(&service);
            let ctx = ctx.clone();
            let stop = stop.clone();
            async move {
                let outcome = service.poll_once(&ctx).await;
                stop.cancel();
                outcome.map_err(anyhow::Error::from)
            }
        })
    }
}

// ---------------------------------------------------------------------------
// The brief's four tests, verbatim
// ---------------------------------------------------------------------------

/// D8: resolution alone is not enough. `manager/src/services/jira_poller.rs:65-70`
/// requires a new build too — otherwise every resolved bug reruns against the
/// same build that failed, which proves nothing and costs a platform slot.
#[tokio::test]
async fn a_resolved_bug_without_a_new_build_does_not_rerun() {
    let f = fixture_with_resolved_bug_and_no_new_build().await;
    f.service.poll_once(&f.ctx).await.expect("poll");
    assert_eq!(f.launcher.launches(), 0);
}

/// The bug is marked resolved even when auto-rerun is disabled
/// (`jira_poller.rs:54-58`: `resolve_bug` runs before the `continue`).
#[tokio::test]
async fn a_bug_resolves_even_when_auto_rerun_is_off() {
    let f = fixture_with_resolved_bug_auto_rerun_off().await;
    f.service.poll_once(&f.ctx).await.expect("poll");
    assert!(f.bug_is_resolved("V-1").await);
    assert_eq!(f.launcher.launches(), 0);
}

/// The rerun is an ordinary launch. VHP-2618 removed the one bypass that used
/// to exist (`jira_poller.rs:8-15`), and the stale comment at
/// `services/argo.rs:369-372` claiming otherwise must not be reproduced.
#[tokio::test]
async fn an_auto_rerun_goes_through_the_normal_launch_path() {
    let f = fixture_with_resolved_bug_and_new_build().await;
    f.service.poll_once(&f.ctx).await.expect("poll");
    assert_eq!(f.launcher.launches(), 1);
    let launch = f.launcher.last_launch();
    assert!(!launch.bypassed_admission);
    assert_eq!(launch.repo_id, UNIVERSE_TEST_REPO_ID);
    assert_eq!(launch.plan_path, UNIVERSE_TEST_PLAN_PATH);
    assert_eq!(launch.test_file, "tests/t1.py");
}

/// The branch is resolved once and reused (`jira_poller.rs:100-118`). Two
/// resolutions can disagree, and the failure is silent: a rerun that drops
/// because the test "does not exist" on a tree it was never going to run on.
#[tokio::test]
async fn the_branch_is_resolved_once_and_reused_for_lookup_and_launch() {
    // The platform's default branch is `release-1.2`; the test exists only
    // there, not on the repository default. Legacy resolves the branch once
    // from the platform (`jira_poller.rs:110-113`) and passes the same value to
    // the plan lookup and the launch. Resolving twice — or resolving the lookup
    // against the repository default — searches a tree the run will not execute
    // against, and the rerun is silently dropped.
    let f = fixture_with_platform_branch("release-1.2").await;
    f.catalog
        .add_test_on_branch_only("release-1.2", "tests/a.py", "test_a");
    f.add_resolved_bug_with_new_build("test_a", "release-1.2")
        .await;

    f.service.poll_once(&f.ctx).await.expect("poll");

    assert_eq!(f.launcher.launches(), 1, "the rerun must not be dropped");
    assert_eq!(
        f.catalog.lookup_branches(),
        vec!["release-1.2"],
        "resolved once"
    );
    let launch = f.launcher.last_launch();
    assert_eq!(launch.branch.as_deref(), Some("release-1.2"));
    assert_eq!(
        launch.test_file, "tests/a.py",
        "the file the catalog resolved on that branch"
    );
    assert_eq!(launch.repo_id, UNIVERSE_TEST_REPO_ID);
    assert_eq!(launch.plan_path, UNIVERSE_TEST_PLAN_PATH);
}

// ---------------------------------------------------------------------------
// Beyond the floor
// ---------------------------------------------------------------------------

/// An absent JIRA config is a silent no-op — legacy's own short-circuit
/// (`jira_poller.rs:40-43`), already [`JiraService::active_config`]'s
/// contract; this pins that [`JiraPollerService::poll_once`] actually
/// consumes it rather than re-deriving the predicate.
#[tokio::test]
async fn no_jira_config_at_all_is_a_silent_no_op() {
    let f = build_with_category(StatusCategory::DONE, false).await;
    f.file_bug("T1", Some(OLD_VERSION), None).await;

    f.service
        .poll_once(&f.ctx)
        .await
        .expect("an unconfigured tenant is not an error");

    assert_eq!(f.launcher.launches(), 0);
    assert!(
        f.jira_client.asked.lock().unwrap().is_empty(),
        "JIRA must never be asked when there is no usable config",
    );
}

/// A bug JIRA still reports open is left untouched: no resolve write, no
/// rerun. The poller's `Ok(_)` arm for "still open" (`jira_poller.rs:80-82`).
#[tokio::test]
async fn a_bug_still_open_in_jira_is_neither_resolved_nor_rerun() {
    let f = build_with_category("indeterminate", true).await;
    f.file_bug("T1", Some(OLD_VERSION), None).await;

    f.service.poll_once(&f.ctx).await.expect("poll");

    assert!(!f.bug_is_resolved(JIRA_KEY).await);
    assert_eq!(f.launcher.launches(), 0);
}

// ---------------------------------------------------------------------------
// Phase C's final review, Critical 1/1b: one pass, one tenant
// ---------------------------------------------------------------------------

/// A PDP double that grants and compiles a scope over **two** tenants —
/// `owner_tenant_id IN [TENANT, OTHER_TENANT]`.
///
/// The shape a parent-tenant grant produces, and the one this module's
/// cross-tenant test needs: `test_support::TenantScopedAuthZ` emits a
/// single-tenant `In`, which is exactly the case where an unpinned statement
/// happens to be safe. Local to this module for `jira_tests`'
/// `TwoTenantAuthZ`'s reason — a shared double whose whole point is a *wider*
/// scope is an easy thing to reach for by accident.
struct TwoTenantAuthZ;

#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for TwoTenantAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, CanonicalError> {
        Ok(authz_resolver_sdk::EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::EvaluationResponseContext {
                constraints: vec![authz_resolver_sdk::Constraint {
                    predicates: vec![authz_resolver_sdk::Predicate::In(
                        authz_resolver_sdk::InPredicate::new(
                            toolkit_security::pep_properties::OWNER_TENANT_ID,
                            [TENANT, OTHER_TENANT],
                        ),
                    )],
                }],
                ..Default::default()
            },
        })
    }
}

/// The second tenant [`TwoTenantAuthZ`]'s scope admits — a child of [`TENANT`]
/// in the shape `ScopeFilter::InTenantSubtree` produces.
const OTHER_TENANT: Uuid = Uuid::from_u128(0x0B);

/// **Phase C's final review, Critical 1 and 1b, pinned at the tier that owns
/// the contract.** A pass minted for one tenant must not read, resolve or rerun
/// another tenant's open bug, even when the grant it runs under spans both.
///
/// # What the fixture arranges, and why each piece is load-bearing
///
/// The grant is [`TwoTenantAuthZ`] — the shape
/// `system_actor::for_jira_poll(TENANT)` gets from a real
/// `ScopeFilter::InTenantSubtree` policy, and the shape R112a would grant. The
/// only open bug in the database belongs to [`OTHER_TENANT`]; [`TENANT`] has
/// none of its own, so every effect this test can observe is an effect on
/// someone else's row.
///
/// The other three pieces exist so that the **broken** version reaches a real
/// launch rather than being stopped by something incidental:
///
/// * `FakeJiraStatus` answers `done` to every key, so the resolve write is
///   reachable.
/// * The new build is recorded under [`TENANT`], not [`OTHER_TENANT`] — which
///   is exactly what makes this realistic rather than contrived:
///   `latest_version_for_plan` *is* tenant-pinned (R86, Task 35), so D8's
///   new-build gate for the foreign bug was answered from **this** tenant's
///   run history. A build recorded under `OTHER_TENANT` would have been
///   invisible and the gate would have closed for the wrong reason.
/// * A catalog entry matching the bug's `test_name` on the resolved branch, so
///   `find_plan_test_file` succeeds and `launch_test` is the next step.
///
/// So with either `tenant_id` predicate removed, this test goes red three
/// separate ways: `asked()` carries the foreign key, the foreign row is stored
/// `'Resolved'`, and a rerun is launched for the foreign tenant's `repo_id`
/// under this tenant's context.
#[tokio::test]
async fn a_pass_does_not_touch_another_tenants_open_bug() {
    let f = build_with_authz(StatusCategory::DONE, true, Arc::new(TwoTenantAuthZ)).await;

    // The foreign tenant's open bug, filed directly under its own single-tenant
    // scope: this test is about what a pass for `TENANT` does with it, not
    // about how it got there.
    {
        let conn = f.db.conn().unwrap();
        OrmJiraRepository
            .upsert_bug(
                &conn,
                &scope(OTHER_TENANT),
                OTHER_TENANT,
                NewJiraBug {
                    jira_key: JIRA_KEY.to_owned(),
                    test_name: "T1".to_owned(),
                    repo_id: UNIVERSE_TEST_REPO_ID,
                    plan_path: UNIVERSE_TEST_PLAN_PATH.to_owned(),
                    app_version: Some(OLD_VERSION.to_owned()),
                    platform_id: None,
                    summary: "the other tenant's bug".to_owned(),
                },
            )
            .await
            .expect("filing the other tenant's bug must succeed");
    }

    // A newer build, recorded under *this* tenant — see this test's doc.
    f.record_build(NEW_VERSION, None).await;
    f.catalog
        .add_test_on_branch_only(DEFAULT_BRANCH, "tests/t1.py", "T1");

    f.service
        .poll_once(&f.ctx)
        .await
        .expect("a pass with no bugs of its own is not an error");

    let asked = f.jira_client.asked.lock().unwrap().clone();
    assert!(
        asked.is_empty(),
        "the pass must not even ask JIRA about another tenant's key: {asked:?}",
    );
    assert_eq!(
        f.launcher.launches(),
        0,
        "and it must not launch a rerun for another tenant's repository",
    );

    let conn = f.db.conn().unwrap();
    let foreign = OrmJiraRepository
        .find_by_key(&conn, &scope(OTHER_TENANT), OTHER_TENANT, JIRA_KEY)
        .await
        .unwrap()
        .expect("the other tenant's row must still exist");
    assert_eq!(
        foreign.status, "Open",
        "and the other tenant's bug must still be open - {foreign:?}",
    );
}

// ---------------------------------------------------------------------------
// Task 26: leadership, on the tier that can falsify it
// ---------------------------------------------------------------------------

/// Two replicas of this gear over one database, each with its own connection
/// pool and its own elector — as close to two pods as one process gets.
///
/// The container's own pool is one replica's and
/// [`pg_second_pool`](crate::infra::storage::test_db::pg_second_pool) is the
/// other's, so the two contend through the server rather than through a shared
/// pool. Everything a rerun needs is staged on **both** fixtures: the rows
/// (`qa_jira_config`, the bug, the build) are shared because the database is,
/// and the three doubles (`FakeJiraStatus`, `FakeCatalog`, `FakePlatforms`)
/// are per-fixture and are given the same answers. That symmetry is what makes
/// the assertion mean something — either replica *could* launch, so a single
/// launch is evidence that election stopped one of them and not that the
/// loser's fixture was quietly incapable.
#[cfg(feature = "integration")]
async fn two_pollers_over_one_database()
-> (crate::infra::storage::test_db::PgHarness, Fixture, Fixture) {
    use crate::infra::leader::claim_row::ClaimRowElector;
    use crate::infra::storage::test_db::{pg_db, pg_second_pool};
    use std::time::Duration;

    let harness = pg_db().await;
    let first = Arc::new(DBProvider::<DomainError>::new(harness.db.clone()));
    let second = Arc::new(DBProvider::<DomainError>::new(
        pg_second_pool(&harness.url).await,
    ));

    // Test cadences, for the reason `claim_row`'s own suite gives: a 10ms
    // retry so the loser's wait is observable inside a test. The TTL is long
    // because nothing here is meant to expire.
    let elector = |db: &Arc<DBProvider<DomainError>>| {
        Arc::new(ClaimRowElector::with_timings(
            Arc::clone(db),
            Duration::from_secs(30),
            Duration::from_secs(10),
            Duration::from_millis(10),
        )) as Arc<dyn LeaderElector>
    };

    // Both replicas share one rendezvous, so the pass that gets there first
    // waits for its peer instead of racing ahead to `resolve_bug` — see
    // [`Rendezvous`] for why the test is a coin flip without it.
    //
    // **The timeout is asymmetric and that is why it is generous.** In the
    // fixed direction it is pure cost, paid once: only one replica ever
    // arrives, and it waits the whole thing out. In the broken direction it is
    // the guard itself — if a loaded machine put more than this between the
    // two replicas' `open_bugs` reads, the first would time out, proceed to
    // `resolve_bug`, and the test would go green against the defect. That is
    // the coin flip this type exists to remove, reintroduced at a longer
    // timescale. Three seconds against two round trips to a container on
    // loopback; raising it further costs only the green path's one-time wait.
    let rendezvous = Arc::new(Rendezvous {
        arrived: std::sync::atomic::AtomicUsize::new(0),
        expected: 2,
        timeout: Duration::from_secs(3),
    });

    let a = build_on(
        Arc::clone(&first),
        elector(&first),
        Some(Arc::clone(&rendezvous)),
        StatusCategory::DONE,
        true,
        Arc::new(TenantScopedAuthZ),
    )
    .await;
    let b = build_on(
        Arc::clone(&second),
        elector(&second),
        Some(Arc::clone(&rendezvous)),
        StatusCategory::DONE,
        true,
        Arc::new(TenantScopedAuthZ),
    )
    .await;

    // One bug, one newer build — written once, seen by both, because the
    // database is the same one.
    a.file_bug("T1", Some(OLD_VERSION), None).await;
    a.record_build(NEW_VERSION, None).await;
    // The catalog is a per-fixture double, so both replicas get the entry.
    // Without this on `b`, `b` could never launch and the assertion below
    // would pass against any elector at all.
    for f in [&a, &b] {
        f.catalog
            .add_test_on_branch_only(DEFAULT_BRANCH, "tests/t1.py", "T1");
    }

    (harness, a, b)
}

/// **Two pollers, one rerun.**
///
/// The JIRA poller's effect is `RunsLauncher::launch_test` -- a new run, not an
/// idempotent write -- so two replicas polling the same resolved bug launch it
/// twice. The reconciler's "election is an optimisation" argument
/// (`infra::leader`'s header) is correct and does not extend here: nothing
/// downstream of `maybe_rerun` deduplicates.
///
/// `replicaCount: 1` is what prevents this today, and a chart value is not
/// where a correctness property belongs. Review finding #5.
///
/// # Why this cannot live in the unit tier
///
/// It needs a **genuinely shared** database that two writers can be inside at
/// once. This file's other tests run on in-memory `SQLite`, where a fixture's
/// database is its own — two pollers there would each hold an uncontended
/// claim and both would win, and the test would pass against an elector that
/// did nothing. Pointing both at one `SQLite` file would fix the sharing and
/// not the race: `inmem_db`'s `max_conns(1)` serialises every writer in this
/// process, so the interleaving the claim row exists to survive cannot occur.
/// This is the same argument `Cargo.toml`'s `integration` feature makes for
/// the ingest races, applied to a different race.
///
/// # It ran red first
///
/// With `NoopLeaderElector` in place of `ClaimRowElector` — the elector the
/// other two tickers still use — this test reports two launches, which is the
/// defect finding #5 names.
///
/// # The loser may still run a second, empty term, and the assertion survives it
///
/// `ClaimRowElector::run_role` releases its claim *before* it checks for
/// shutdown — deliberately, so a rolling restart does not idle the role for a
/// whole TTL — which leaves a window in which the loser's 10ms retry can win a
/// term of its own. That pass finds nothing: the winner's `resolve_bug` has
/// committed, so `open_bugs` is empty and there is no bug to launch. Worth
/// saying out loud because it means the *green* direction leans partly on the
/// resolve write, which is the very thing [`Rendezvous`] neutralises in the
/// red direction. The asymmetry is fine — a second term that launches nothing
/// is the correct behaviour for a role that really is free — but a reader
/// should not take this test as proving the loser never runs at all.
/// `infra::leader::claim_row::tests::only_the_holder_runs_the_work` is the one
/// that proves that, on a fixture where nothing else can end the term.
#[cfg(feature = "integration")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_pollers_produce_one_rerun() {
    use crate::infra::leader::ROLE_JIRA_POLLER;

    let (_harness, a, b) = two_pollers_over_one_database().await;
    let stop = CancellationToken::new();

    let (ra, rb) = tokio::join!(
        a.elector
            .run_role(ROLE_JIRA_POLLER, stop.clone(), a.work(stop.clone())),
        b.elector
            .run_role(ROLE_JIRA_POLLER, stop.clone(), b.work(stop.clone())),
    );
    ra.unwrap();
    rb.unwrap();

    assert_eq!(
        a.launcher.launches() + b.launcher.launches(),
        1,
        "exactly one of the two pollers may rerun the bug"
    );
    assert!(
        a.bug_is_resolved(JIRA_KEY).await,
        "and the pass that won must still have done its work"
    );
}

// ---------------------------------------------------------------------------
//  Metrics — Task 38 of the observability plan
// ---------------------------------------------------------------------------
//
// Every assertion below reads the **rendered series** back out of a real
// `OpenTelemetry` pipeline (`MetricsProbe`), not a mock of the port. A mock
// would prove only that the call site calls something; what has to be true is
// that a dashboard query finds the sample.

/// A PDP double that grants everything except one `(resource_type, action)`
/// pair.
///
/// Local to this module, and narrower than `DenyAllAuthZ` on purpose: the
/// per-bug failures this file has to reach are *individual steps* of one pass,
/// and a double that denied everything would stop the pass at its first read
/// instead. `resolve_bug` is `(qa.jira_bug, update)` and
/// `latest_version_for_plan` is `(qa.test_result, list)`, so denying exactly
/// one of those puts the failure at exactly one step with every other step
/// still working — which is the shape a real partial policy has, and the only
/// way to make the poller swallow a failure at a chosen point.
struct DenyOneAuthZ {
    resource: &'static str,
    action: &'static str,
}

#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for DenyOneAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, CanonicalError> {
        if request.resource.resource_type == self.resource && request.action.name == self.action {
            return Ok(authz_resolver_sdk::EvaluationResponse {
                decision: false,
                context: authz_resolver_sdk::EvaluationResponseContext::default(),
            });
        }
        Ok(authz_resolver_sdk::EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::EvaluationResponseContext {
                constraints: vec![authz_resolver_sdk::Constraint {
                    predicates: vec![authz_resolver_sdk::Predicate::In(
                        authz_resolver_sdk::InPredicate::new(
                            toolkit_security::pep_properties::OWNER_TENANT_ID,
                            [TENANT],
                        ),
                    )],
                }],
                ..Default::default()
            },
        })
    }
}

/// **One pass is one observation on both of its instruments.**
///
/// The premise assertion matters as much as the metric one: a pass that looked
/// at no bug would still emit, and this test would then be measuring an empty
/// loop rather than a working one.
#[tokio::test]
async fn a_poll_pass_records_one_observation() {
    let f = fixture_with_resolved_bug_and_new_build().await;

    f.service.poll_once(&f.ctx).await.expect("the pass runs");
    assert_eq!(f.launcher.launches(), 1, "premise: the pass really ran");

    let series = f.metrics.collect();
    assert_eq!(
        series.counter(QA_INSIGHTS_JIRA_POLL),
        1,
        "one pass, one increment; the exported names were {:?}",
        series.names()
    );
    assert_eq!(series.histogram_count(QA_INSIGHTS_JIRA_POLL_DURATION), 1);
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_POLL, &[("outcome", "completed")]),
        1
    );
}

/// **A tenant with no JIRA configuration is `skipped`, not `completed`.**
///
/// Both answer `Ok(())`, so nothing but the label separates them — and the
/// difference is the whole question an operator asks first: a deployment where
/// every pass is skipped because nobody configured JIRA looks, on a
/// `completed`-only counter, exactly like one where every pass is working.
#[tokio::test]
async fn a_pass_for_a_tenant_without_jira_is_skipped_not_completed() {
    let f = build_with_category(StatusCategory::DONE, false).await;

    f.service.poll_once(&f.ctx).await.expect("a silent no-op");

    let series = f.metrics.collect();
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_POLL, &[("outcome", "skipped")]),
        1
    );
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_POLL, &[("outcome", "completed")]),
        0,
        "a pass that looked at no bug has not completed anything"
    );
    assert_eq!(
        series.counter(QA_INSIGHTS_JIRA_BUG),
        0,
        "and it contributes nothing to the per-bug family"
    );
}

/// **A pass the PDP refuses is `refused`, not `failed`.**
///
/// The two need opposite responses — a missing grant is a policy to fix and
/// must not page anybody — and the pass returns the same `Err` shape either
/// way, so only the label tells them apart.
#[tokio::test]
async fn a_refused_pass_is_not_a_failed_pass() {
    // Denied at exactly the config *read* the pass opens with. A blanket
    // `DenyAllAuthZ` cannot serve here: this fixture saves its JIRA config
    // through the service's own write path, which that double refuses too, so
    // the test would fail while building rather than while polling.
    let f = build_with_authz(
        StatusCategory::DONE,
        true,
        Arc::new(DenyOneAuthZ {
            resource: "qa.jira_config",
            action: "get",
        }),
    )
    .await;

    f.service
        .poll_once(&f.ctx)
        .await
        .expect_err("a denied config read fails the pass");

    let series = f.metrics.collect();
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_POLL, &[("outcome", "refused")]),
        1
    );
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_POLL, &[("outcome", "failed")]),
        0
    );
    assert_eq!(series.histogram_count(QA_INSIGHTS_JIRA_POLL_DURATION), 1);
}

/// **An open bug and a resolved one are counted apart, one increment each.**
#[tokio::test]
async fn an_open_bug_and_a_resolved_one_are_counted_apart() {
    let still_open = build_with_category("indeterminate", true).await;
    still_open.file_bug("T1", Some(OLD_VERSION), None).await;
    still_open
        .service
        .poll_once(&still_open.ctx)
        .await
        .expect("the pass runs");
    assert!(
        !still_open.bug_is_resolved(JIRA_KEY).await,
        "premise: JIRA still reports this one open"
    );
    assert_eq!(
        still_open
            .metrics
            .collect()
            .counter_with(QA_INSIGHTS_JIRA_BUG, &[("outcome", "unresolved")]),
        1
    );

    let resolved = fixture_with_resolved_bug_and_no_new_build().await;
    resolved
        .service
        .poll_once(&resolved.ctx)
        .await
        .expect("the pass runs");
    assert!(
        resolved.bug_is_resolved(JIRA_KEY).await,
        "premise: this one really was resolved"
    );
    let series = resolved.metrics.collect();
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_BUG, &[("outcome", "resolved")]),
        1
    );
    assert_eq!(
        series.counter(QA_INSIGHTS_JIRA_BUG),
        1,
        "exactly one increment per bug per pass, whatever the chain did"
    );
}

/// **Every failure the poller swallows is counted under its own class.**
///
/// This is the finding the family exists for, and it is swept rather than
/// sampled. `domain::service::jira_poller`'s header states that every per-bug
/// failure is logged and skipped and never becomes a pass error — so the pass
/// answers `Ok(())` in all six arms below, and **the per-bug counter is the
/// only place any of them is observable at all**. A pass that silently dropped
/// forty bugs was, before this, indistinguishable from a clean one.
///
/// Each arm arranges its failure at one step and leaves every other step
/// working, so a defect that mislabels one arm cannot be hidden by another
/// failing first.
#[tokio::test]
async fn every_swallowed_per_bug_failure_is_counted_under_its_own_class() {
    // 1. The JIRA status call.
    let f = build().await;
    f.file_bug("T1", Some(OLD_VERSION), None).await;
    f.jira_client.fail_status_checks();
    assert_bug_outcome(&f, "status_check_failed").await;

    // 2. The local resolve write, denied at exactly (qa.jira_bug, update) so
    //    the listing above it still works.
    let f = build_with_authz(
        StatusCategory::DONE,
        true,
        Arc::new(DenyOneAuthZ {
            resource: "qa.jira_bug",
            action: "update",
        }),
    )
    .await;
    f.file_bug("T1", Some(OLD_VERSION), None).await;
    assert_bug_outcome(&f, "resolve_write_failed").await;

    // 3. The plan's latest build, denied at (qa.test_result, list).
    let f = build_with_authz(
        StatusCategory::DONE,
        true,
        Arc::new(DenyOneAuthZ {
            resource: "qa.test_result",
            action: "list",
        }),
    )
    .await;
    f.file_bug("T1", Some(OLD_VERSION), None).await;
    assert_bug_outcome(&f, "plan_version_unreadable").await;

    // 4. The platform's default branch.
    let f = fixture_with_platform_branch(DEFAULT_BRANCH).await;
    f.add_resolved_bug_with_new_build("T1", DEFAULT_BRANCH)
        .await;
    f.catalog
        .add_test_on_branch_only(DEFAULT_BRANCH, "tests/t1.py", "T1");
    f.platforms.fail_reads(true);
    assert_bug_outcome(&f, "branch_unresolved").await;

    // 5. The catalog universe lookup — here, a universe with no test
    //    declaring this bug's title, which `find_plan_test_file` answers
    //    identically to a failed read.
    let f = build().await;
    f.file_bug("T1", Some(OLD_VERSION), None).await;
    f.record_build(NEW_VERSION, None).await;
    assert_bug_outcome(&f, "test_file_unresolved").await;

    // 6. The launch itself.
    let f = fixture_with_resolved_bug_and_new_build().await;
    f.launcher.fail_test_launches();
    assert_bug_outcome(&f, "launch_failed").await;
}

/// Run one pass and assert its single bug was counted under `outcome`, and
/// under nothing else.
///
/// The `Ok(())` assertion is not incidental: it is the premise the whole family
/// rests on — the pass really did swallow the failure — and a change that
/// started propagating per-bug errors would fail here rather than silently
/// making these assertions vacuous.
async fn assert_bug_outcome(f: &Fixture, outcome: &str) {
    f.service
        .poll_once(&f.ctx)
        .await
        .expect("a per-bug failure never fails the pass");

    let series = f.metrics.collect();
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_BUG, &[("outcome", outcome)]),
        1,
        "the bug was not counted as {outcome}; the exported names were {:?}",
        series.names()
    );
    assert_eq!(
        series.counter(QA_INSIGHTS_JIRA_BUG),
        1,
        "and it must be counted once, under one class only"
    );
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_POLL, &[("outcome", "completed")]),
        1,
        "the pass itself completed, which is exactly why the per-bug family \
         is the only place this failure is visible"
    );
}

/// **A rerun is counted as a launch, separately from the poll.**
///
/// A pass is one increment on the poll family however many reruns it fired, so
/// a rerun storm is invisible there by construction. This family is where it
/// shows, and the two halves below are the two ways a counter of successes
/// would get it wrong: a pass that reruns nothing must count nothing, and a
/// launch qa-runs **refuses** must still count, because it cost the same call
/// into the admission path that a storm is made of.
#[tokio::test]
async fn an_auto_rerun_is_counted_as_a_launch_whether_or_not_it_was_accepted() {
    let accepted = fixture_with_resolved_bug_and_new_build().await;
    accepted
        .service
        .poll_once(&accepted.ctx)
        .await
        .expect("the pass runs");
    assert_eq!(accepted.launcher.launches(), 1, "premise: a rerun happened");
    assert_eq!(
        accepted.metrics.collect().counter(QA_INSIGHTS_JIRA_RERUN),
        1
    );

    let refused = fixture_with_resolved_bug_and_new_build().await;
    refused.launcher.fail_test_launches();
    refused
        .service
        .poll_once(&refused.ctx)
        .await
        .expect("the pass runs");
    let series = refused.metrics.collect();
    assert_eq!(
        series.counter(QA_INSIGHTS_JIRA_RERUN),
        1,
        "a refused launch is still a call into qa-runs and still part of a storm"
    );
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_BUG, &[("outcome", "launch_failed")]),
        1,
        "and how it went is on the per-bug family, which is why this one carries no label"
    );

    let no_rerun = fixture_with_resolved_bug_and_no_new_build().await;
    no_rerun
        .service
        .poll_once(&no_rerun.ctx)
        .await
        .expect("the pass runs");
    assert_eq!(no_rerun.launcher.launches(), 0, "premise: D8 stopped it");
    assert_eq!(
        no_rerun.metrics.collect().counter(QA_INSIGHTS_JIRA_RERUN),
        0,
        "a bug that was never a rerun candidate must not inflate the rerun rate"
    );
}

/// **A panicking metrics adapter does not fail the pass, and is called once.**
///
/// Two properties, and the second is what makes the first survivable here in
/// particular: the per-bug emission fires once per open bug per pass, so
/// without the latch a persistently broken adapter writes a panic line to
/// stderr per bug — the "never logs per-emission" failure, at the worst
/// possible multiplier.
#[tokio::test]
async fn a_broken_metrics_adapter_does_not_fail_a_poll_pass() {
    #[derive(Default)]
    struct Panicking {
        calls: Mutex<usize>,
    }
    impl JiraPollMetrics for Panicking {
        fn poll_pass(&self, _outcome: JiraPollOutcome, _duration: std::time::Duration) {
            *self.calls.lock().unwrap() += 1;
            panic!("this adapter is broken");
        }
        fn bug(&self, _outcome: JiraBugOutcome) {
            *self.calls.lock().unwrap() += 1;
            panic!("this adapter is broken");
        }
        fn auto_rerun(&self) {
            *self.calls.lock().unwrap() += 1;
            panic!("this adapter is broken");
        }
    }

    let f = fixture_with_resolved_bug_and_new_build().await;
    let broken = Arc::new(Panicking::default());
    let service = JiraPollerService::new(
        Arc::clone(&f.jira_service),
        Arc::clone(&f.catalog) as Arc<dyn CatalogReader>,
        Arc::clone(&f.platforms) as Arc<dyn EnvironmentReader>,
        Arc::clone(&f.launcher) as Arc<dyn RunsLauncher>,
        Some(Arc::clone(&broken) as Arc<dyn JiraPollMetrics>),
    );

    service
        .poll_once(&f.ctx)
        .await
        .expect("a broken metrics adapter must not fail the pass");
    service.poll_once(&f.ctx).await.expect("nor the next one");

    assert_eq!(
        *broken.calls.lock().unwrap(),
        1,
        "the latch must stop the adapter being called again after its first \
         panic; this path emits once per bug, so an unlatched flood is per bug per pass"
    );
    assert!(
        f.launcher.launches() >= 1,
        "premise: the passes really ran and really launched"
    );
}

/// **The recorded pass duration really is a clock around the pass.**
///
/// Every other poller metric assertion here is about counts and labels, and
/// counts cannot see a *value*: a call site that recorded `Duration::ZERO`, or
/// a constant, or an `Instant` taken in the wrong place would satisfy all of
/// them and hand a dashboard a fabricated distribution. Measured in
/// qa-environments during Task 40 — a mutation replacing the elapsed time with
/// a six-second constant passed that gear's whole suite, and this family had
/// the same hole.
///
/// The service is rebuilt over the fixture's own collaborators with a slow
/// catalog swapped in, rather than a new fixture builder being added: what has
/// to change is one of five constructor arguments, and the bug, the build and
/// the JIRA double all have to be the ones `fixture_with_resolved_bug_and_new_build`
/// already set up for the pass to reach the catalog at all.
///
/// Written as **two bracketing assertions rather than one equality**, because
/// an equality would be a timing test:
///
/// * the catalog read inside the per-bug rerun chain sleeps 150 ms and the pass
///   awaits it, so the sample cannot be in a bucket whose upper edge is 100 ms
///   or below — that direction is deterministic, since a sleep can only
///   overrun;
/// * and it must not be in the `(5 s, 10 s]` bucket, which no in-memory fixture
///   can honestly reach.
///
/// Probed edge by edge rather than through one call: `histogram_bucket_of`
/// answers for the single bucket a value falls in, so asking about one edge
/// says nothing about the buckets below it.
#[tokio::test]
async fn the_recorded_pass_duration_tracks_the_pass_it_measures() {
    let delay = std::time::Duration::from_millis(150);
    let f = fixture_with_resolved_bug_and_new_build().await;
    let service = JiraPollerService::new(
        Arc::clone(&f.jira_service),
        Arc::new(SlowCatalog::new(Arc::clone(&f.catalog), delay)) as Arc<dyn CatalogReader>,
        Arc::clone(&f.platforms) as Arc<dyn EnvironmentReader>,
        Arc::clone(&f.launcher) as Arc<dyn RunsLauncher>,
        Some(f.metrics.adapter()),
    );

    service.poll_once(&f.ctx).await.expect("the pass runs");
    assert_eq!(
        f.launcher.launches(),
        1,
        "premise: the pass really reached the catalog and launched the rerun"
    );

    let series = f.metrics.collect();
    assert_eq!(
        series.histogram_count(QA_INSIGHTS_JIRA_POLL_DURATION),
        1,
        "premise: exactly one pass was timed"
    );
    for edge in [0.01_f64, 0.05, 0.1] {
        assert_eq!(
            series.histogram_bucket_of(QA_INSIGHTS_JIRA_POLL_DURATION, edge),
            Some(0),
            "a pass whose catalog read slept for {delay:?} cannot have been measured at \
             {edge} s or less, so that bucket must be empty -- a zero or a near-zero here \
             means the clock is not around the pass"
        );
    }
    assert_eq!(
        series.histogram_bucket_of(QA_INSIGHTS_JIRA_POLL_DURATION, 6.0),
        Some(0),
        "and it cannot have taken between five and ten seconds either: an in-memory \
         fixture does not, so a sample there is a fabricated or stale duration rather \
         than a measured one"
    );
}
