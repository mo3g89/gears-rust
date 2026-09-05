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

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use qa_insights_sdk::{JiraConfig, JiraPollerConfig, NewJiraBug};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::JiraPollerService;
use crate::domain::error::DomainError;
use crate::domain::ports::jira_client::{
    IssueRef, JiraClient, JiraIssue, NewIssue, StatusCategory,
};
use crate::domain::ports::{CatalogReader, EnvironmentReader, RunsLauncher};
use crate::domain::repos::{JiraRepository, NewTestResult, ResultsRepository};
use crate::domain::service::jira::{JiraConfigInput, JiraService};
use crate::domain::service::test_support::{
    DEFAULT_BRANCH, FakeCatalog, FakePlatforms, TenantScopedAuthZ, UNIVERSE_TEST_PLAN_PATH,
    UNIVERSE_TEST_REPO_ID, ctx,
};
use crate::infra::storage::jira_sea_repo::OrmJiraRepository;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{inmem_db, scope};

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
        self.asked.lock().unwrap().push(jira_key.to_owned());
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
}

impl FakeLauncher {
    fn launches(&self) -> usize {
        self.launches.lock().unwrap().len()
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
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Fixture {
    service: JiraPollerService<OrmJiraRepository, OrmResultsRepository>,
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
    authz: Arc<dyn authz_resolver_sdk::AuthZResolverClient>,
) -> Fixture {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let jira_client = Arc::new(FakeJiraStatus {
        category: StatusCategory::new(category),
        asked: Mutex::new(Vec::new()),
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
    let service = JiraPollerService::new(
        Arc::clone(&jira_service),
        Arc::clone(&catalog) as Arc<dyn CatalogReader>,
        Arc::clone(&platforms) as Arc<dyn EnvironmentReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
    );
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
impl authz_resolver_sdk::AuthZResolverClient for TwoTenantAuthZ {
    async fn evaluate(
        &self,
        _request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, authz_resolver_sdk::AuthZResolverError>
    {
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
