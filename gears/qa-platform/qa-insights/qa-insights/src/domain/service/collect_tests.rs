//! Tests for the collect trigger, the runner's report, and the (callable, not
//! yet ticked) hourly cycle.
//!
//! Against the **real** repository (`OrmCollectRepository`) on in-memory
//! `SQLite`, matching `saved_views_tests`' shape: the property under test is
//! whether the scope and the natural key [`CollectService::record_count`]
//! writes under are the ones the read side actually looks for, and a
//! repository double would absorb exactly that. `qa-catalog` and qa-runs are
//! doubled — [`FakeCatalog`] and [`FakeRunsLauncher`] — because this module's
//! job is orchestration over those two ports, not their own contracts.
//!
//! The three tests named in this task's brief are the first three below, each
//! with the brief's own doc comment; the rest were added in fix round 1 of
//! this task's review, closing Critical 1 (a cross-tenant write on the
//! report endpoint) and its accompanying Important/Minor findings.
//!
//! Every fixture in this file signs with [`SECRET`], a fixed non-empty
//! string, so [`CollectService::record_count`]'s signature check passes by
//! default and every pre-existing test keeps testing what it always tested;
//! [`the_empty_signing_secret_fails_every_report_closed`] is the one test
//! that deliberately configures an empty secret instead.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use toolkit_db::DBProvider;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{
    CollectReportBaseUrl, CollectReportSigningSecret, CollectService, DefaultCollectBranch,
};
use crate::domain::error::DomainError;
use crate::domain::metrics::{
    QA_INSIGHTS_COLLECT, QA_INSIGHTS_COLLECT_DURATION, QA_INSIGHTS_COLLECT_REPORT,
};
use crate::domain::ports::metrics::{CollectMetrics, CollectOutcome, CollectReportOutcome};
use crate::domain::ports::{CatalogReader, RunsLauncher};
use crate::domain::repos::CollectRepository;
use crate::domain::service::test_support::{
    CatalogFailure, DenyAllAuthZ, FakeCatalog, RecordingAuthZ, SlowCatalog, TenantScopedAuthZ, ctx,
    universe_test,
};
use crate::domain::system_actor::TenantBound;
use crate::infra::metrics::probe::MetricsProbe;
use crate::infra::storage::collect_sea_repo::OrmCollectRepository;
use crate::infra::storage::test_db::{inmem_db, scope};

const TENANT: Uuid = Uuid::from_u128(0x0A);
const REPO: Uuid = Uuid::from_u128(0x50);
/// The signing secret every fixture but
/// [`the_empty_signing_secret_fails_every_report_closed`] configures.
const SECRET: &str = "test-secret-do-not-use-in-production";

/// A [`RunsLauncher`] double: succeeds for every repository except the ones
/// named through [`Self::fail_for`], and records every call it received.
///
/// The failure is generic — an opaque [`DomainError::Internal`] — on purpose:
/// [`RunsLauncher::launch_collect`]'s own header records that this port
/// cannot distinguish "this repository lacks the branch" from any other
/// launch failure, and [`CollectService::run_collect_cycle`] is written to
/// treat every failure the same way. A double that manufactured a
/// branch-specific error variant would test a distinction the real port does
/// not make.
#[derive(Default)]
struct FakeRunsLauncher {
    fails_for: Mutex<HashSet<Uuid>>,
    calls: Mutex<Vec<(Uuid, String, String)>>,
}

impl FakeRunsLauncher {
    fn fail_for(&self, repo_id: Uuid) {
        self.fails_for.lock().unwrap().insert(repo_id);
    }

    /// Every `(repo_id, branch, collect_url)` triple this double was asked to
    /// launch, in call order.
    fn calls(&self) -> Vec<(Uuid, String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl RunsLauncher for FakeRunsLauncher {
    async fn launch_collect(
        &self,
        _ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        collect_url: &str,
    ) -> Result<(), DomainError> {
        self.calls
            .lock()
            .unwrap()
            .push((repo_id, branch.to_owned(), collect_url.to_owned()));
        if self.fails_for.lock().unwrap().contains(&repo_id) {
            return Err(DomainError::Internal(
                "qa-runs: repository has no such branch".to_owned(),
            ));
        }
        Ok(())
    }

    /// Never called: this fixture is `CollectService`'s own, and nothing
    /// under `domain::service::collect` triggers a single-test rerun — that is
    /// `domain::service::jira_poller`'s call, with its own fake.
    async fn launch_test(
        &self,
        _ctx: &SecurityContext,
        _repo_id: Uuid,
        _plan_path: &str,
        _test_file: &str,
        _environment_id: Option<Uuid>,
        _branch: Option<&str>,
    ) -> Result<(), DomainError> {
        unimplemented!("collect never launches a single test")
    }
}

struct Fixture {
    service: CollectService<OrmCollectRepository>,
    db: Arc<DBProvider<DomainError>>,
    ctx: SecurityContext,
    launcher: Arc<FakeRunsLauncher>,
    /// The real `OpenTelemetry` adapter this fixture's service emits through,
    /// over a meter provider private to this fixture.
    ///
    /// **Every fixture carries one, whether or not its test reads it**, and
    /// the reason is the constraint that makes metric tests easy to get wrong:
    /// an emission is silent when no adapter is installed, so a test that
    /// asserted on a service built without one would pass while measuring
    /// nothing. Installing the real adapter everywhere means the metric
    /// assertions below are reading the same code path production runs, and
    /// the tests that ignore it are still exercising it.
    ///
    /// Private per fixture, not shared: a counter is cumulative, so a shared
    /// provider would make every `assert_eq!(.., 1)` here order-dependent.
    metrics: MetricsProbe,
    /// Retained so a test can make the universe read fail, which is the only
    /// way a cycle reaches an outcome other than `completed`.
    catalog: Arc<FakeCatalog>,
}

impl Fixture {
    /// The count [`CollectService::record_count`] wrote for `test_file`, on
    /// `REPO`/`"main"` — read back through the same repository the service
    /// writes through, independently of the service, so a test asserts what
    /// is actually stored rather than what the service's own read path would
    /// paper over.
    async fn count_for(&self, test_file: &str) -> u32 {
        let conn = self.db.conn().unwrap();
        let rows = OrmCollectRepository
            .list_counts_for(&conn, &scope(TENANT), &[REPO], "main")
            .await
            .unwrap();
        match rows.iter().find(|row| row.test_file == test_file) {
            Some(row) => row.case_count,
            None => panic!("no stored count for {test_file}: {rows:?}"),
        }
    }

    /// The valid signature for `(repo_id, branch, tenant_id)` under this
    /// fixture's own signing secret — [`CollectService::sign`] is private,
    /// and this module is a child of `collect`, so it can call it directly
    /// rather than duplicating the HMAC construction.
    fn sig_for(&self, repo_id: Uuid, branch: &str, tenant_id: Uuid) -> String {
        self.service.sign(repo_id, branch, tenant_id)
    }
}

/// One repository, one collect job, a permissive PDP. Sufficient for
/// [`CollectService::record_count`] and [`CollectService::trigger`], neither
/// of which needs more than one repository in the universe.
async fn fixture() -> Fixture {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    catalog.add(Uuid::new_v4(), "main", universe_test("tests/a.py"));
    let launcher = Arc::new(FakeRunsLauncher::default());
    let metrics = MetricsProbe::new();
    let service = CollectService::new(
        Arc::clone(&db),
        OrmCollectRepository,
        Arc::clone(&catalog) as Arc<dyn CatalogReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret(SECRET.to_owned()),
        Some(metrics.adapter()),
    );
    Fixture {
        service,
        db,
        ctx: ctx(TENANT),
        launcher,
        metrics,
        catalog,
    }
}

/// Two repositories in the universe; qa-runs will refuse the launch for the
/// second one, standing in for "this repository has no such branch" — see
/// [`FakeRunsLauncher`]'s own doc for why the fake does not name the failure
/// that specifically.
async fn fixture_with_two_repos_one_missing_the_branch() -> Fixture {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    let repo_with_branch = Uuid::from_u128(0x60);
    let repo_missing_branch = Uuid::from_u128(0x61);
    // Repository discovery is branch-independent (`run_collect_cycle`'s own
    // doc) — both repositories are registered on their own default branch,
    // `main`, regardless of the `"feature-x"` branch the test below asks to
    // collect.
    catalog.add(
        Uuid::new_v4(),
        "main",
        universe_test_for_repo(repo_with_branch, "tests/a.py"),
    );
    catalog.add(
        Uuid::new_v4(),
        "main",
        universe_test_for_repo(repo_missing_branch, "tests/b.py"),
    );
    let launcher = Arc::new(FakeRunsLauncher::default());
    launcher.fail_for(repo_missing_branch);
    let metrics = MetricsProbe::new();
    let service = CollectService::new(
        Arc::clone(&db),
        OrmCollectRepository,
        Arc::clone(&catalog) as Arc<dyn CatalogReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret(SECRET.to_owned()),
        Some(metrics.adapter()),
    );
    Fixture {
        service,
        db,
        ctx: ctx(TENANT),
        launcher,
        metrics,
        catalog,
    }
}

/// [`universe_test`] pinned to `repo_id`, rather than its own fixed
/// `Uuid::from_u128(0x30)` — needed here because
/// `a_repository_without_the_branch_is_skipped_not_fatal` is about *two
/// distinct repositories*, which `universe_test`'s single hardcoded repo id
/// cannot express.
fn universe_test_for_repo(repo_id: Uuid, test_file: &str) -> qa_catalog_sdk::UniverseTest {
    qa_catalog_sdk::UniverseTest {
        repo_id,
        ..universe_test(test_file)
    }
}

/// `api_collect_report:2614`: a negative count clamps to zero and an empty
/// file name is a 400. Both are cheap guards against a runner bug becoming a
/// permanently wrong "expected cases" number.
#[tokio::test]
async fn a_collect_report_clamps_negative_counts_and_rejects_an_empty_file() {
    let f = fixture().await;
    let sig = f.sig_for(REPO, "main", TENANT);
    let tenant = TenantBound::new(TENANT).unwrap();
    f.service
        .record_count(tenant, REPO, "main", &sig, "tests/a.py", -5)
        .await
        .expect("clamps");
    assert_eq!(f.count_for("tests/a.py").await, 0);

    let err = f
        .service
        .record_count(tenant, REPO, "main", &sig, "   ", 3)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

/// The brief's own citation is `launch_collect_for_repo:42-48` ("branch A
/// present in repo1 but not repo2 collects repo1 only"), and the property
/// that citation is really about — a per-repository launch failure must not
/// abort the whole cycle — is what this test exercises, over
/// [`FakeRunsLauncher::fail_for`] rather than over a real missing branch.
///
/// **Corrected in fix round 1: this is a control-flow test, not a
/// missing-branch test.** `RunsLauncher::launch_collect`'s own header (and
/// `domain::service::collect`'s, "`launched` counts launch calls...")
/// records why the *real* adapter cannot be relied on to fail synchronously
/// for a missing branch specifically — qa-runs validates branch existence
/// asynchronously, at dispatch, after `launch()` has already returned `Ok`.
/// So this test proves `run_collect_cycle` continues past **any** per-repository
/// launch failure (it would fail if the loop used `?` instead of matching);
/// it does not prove, and cannot prove without a real qa-runs, that a
/// missing branch specifically produces one synchronously.
#[tokio::test]
async fn a_repository_without_the_branch_is_skipped_not_fatal() {
    let f = fixture_with_two_repos_one_missing_the_branch().await;
    let launched = f
        .service
        .run_collect_cycle(&f.ctx, "feature-x")
        .await
        .expect("cycle runs");
    assert_eq!(launched, 1);

    // Both repositories were attempted, on the requested branch — the skip is
    // qa-runs' launch failing, not this service silently narrowing the set it
    // tries.
    let calls = f.launcher.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls.iter().all(|(_, branch, _)| branch == "feature-x"));
}

/// The report is an upsert keyed on `(repo, branch, file)`: re-collecting must
/// replace, never accumulate.
#[tokio::test]
async fn re_reporting_a_file_replaces_its_count() {
    let f = fixture().await;
    let sig = f.sig_for(REPO, "main", TENANT);
    let tenant = TenantBound::new(TENANT).unwrap();
    f.service
        .record_count(tenant, REPO, "main", &sig, "tests/a.py", 5)
        .await
        .expect("first report");
    f.service
        .record_count(tenant, REPO, "main", &sig, "tests/a.py", 9)
        .await
        .expect("second report replaces the first");

    assert_eq!(
        f.count_for("tests/a.py").await,
        9,
        "the latter count, not the sum and not the former"
    );
}

/// The hazard this task's brief names as the most likely way it ships broken:
/// a real branch name contains `/` (`feature/VHP-123-thing`), and a `/` in a
/// single path segment is a router 404 no test in this crate otherwise
/// crosses the HTTP boundary to catch. This drives the exact production
/// encode path (`CollectService::collect_url`) through the exact decode crate
/// axum's `Query` extractor uses (`serde_urlencoded`, confirmed
/// byte-identical to axum's decode by Task 27's own remedy), so a regression
/// that reintroduces the branch as a path segment — or that hand-rolls the
/// query string instead of encoding it — fails here rather than only in
/// production against a real branch name.
#[tokio::test]
async fn the_collect_url_round_trips_a_slash_bearing_branch_through_its_query_string() {
    /// The three fields `api::rest::dto::CollectReportQuery` decodes on the
    /// real route — duplicated here rather than imported, because domain code
    /// must not depend on `api::rest::dto` (this crate's own layering rule;
    /// see `dto.rs`'s header). All three must keep the same field names for
    /// the encode and decode sides to agree.
    #[derive(serde::Deserialize)]
    struct Decoded {
        branch: String,
        tenant_id: Uuid,
        sig: String,
    }

    let f = fixture().await;
    let branch = "feature/VHP-123-thing";

    let url = f.service.collect_url(REPO, branch, TENANT);

    assert!(
        !url.contains(&format!("/{branch}")),
        "the branch must not appear as a path segment: {url}"
    );
    let (_, query) = url.split_once('?').expect("a query string: {url}");

    let decoded: Decoded = serde_urlencoded::from_str(query)
        .expect("the exact decode path axum's Query extractor uses");

    assert_eq!(
        decoded.branch, branch,
        "the slash must survive the query string unmangled"
    );
    assert_eq!(decoded.tenant_id, TENANT);
    assert_eq!(
        decoded.sig,
        f.sig_for(REPO, branch, TENANT),
        "collect_url's embedded signature must be exactly what verify_signature will recompute"
    );
}

/// Fix round 1, Critical 1's own regression test: `record_count` must refuse
/// a `tenant_id` the caller asserts without a matching signature, which is
/// exactly the cross-tenant write the review found. A caller who knows only
/// `repo_id`, `branch` and an arbitrary `tenant_id` — everything visible on
/// the wire, nothing secret — must not be able to write under that tenant.
#[tokio::test]
async fn a_report_with_no_matching_signature_is_forbidden_not_written() {
    let f = fixture().await;
    let attacker_chosen_tenant = Uuid::from_u128(0xBAD);

    let err = f
        .service
        .record_count(
            TenantBound::new(attacker_chosen_tenant).unwrap(),
            REPO,
            "main",
            "not-a-real-signature",
            "tests/a.py",
            5,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, DomainError::Forbidden), "{err:?}");
    let conn = f.db.conn().unwrap();
    let rows = OrmCollectRepository
        .list_counts_for(&conn, &scope(attacker_chosen_tenant), &[REPO], "main")
        .await
        .unwrap();
    assert!(rows.is_empty(), "nothing must be written: {rows:?}");
}

/// A signature valid for one tenant must not verify for a different one —
/// the property that makes `tenant_id` in the query string an authenticated
/// claim rather than an ambient one. Forging a `record_count` call by
/// replaying a signature captured for a legitimate tenant against a
/// different, attacker-chosen `tenant_id` must fail.
#[tokio::test]
async fn a_signature_does_not_verify_for_a_different_tenant() {
    let f = fixture().await;
    let sig_for_this_tenant = f.sig_for(REPO, "main", TENANT);
    let other_tenant = Uuid::from_u128(0x0B);

    let err = f
        .service
        .record_count(
            TenantBound::new(other_tenant).unwrap(),
            REPO,
            "main",
            &sig_for_this_tenant,
            "tests/a.py",
            5,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Forbidden), "{err:?}");
}

/// The same replay-does-not-verify property, one field over: a signature
/// valid for one repository must not verify for a different one, proving
/// [`signing_payload`](super::signing_payload) actually binds `repo_id` and
/// is not, say, silently ignoring it.
#[tokio::test]
async fn a_signature_does_not_verify_for_a_different_repository() {
    let f = fixture().await;
    let sig = f.sig_for(REPO, "main", TENANT);
    let other_repo = Uuid::from_u128(0x99);

    let err = f
        .service
        .record_count(
            TenantBound::new(TENANT).unwrap(),
            other_repo,
            "main",
            &sig,
            "tests/a.py",
            5,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Forbidden), "{err:?}");
}

/// Fix round 1's fail-closed requirement: an unconfigured signing secret must
/// refuse every report, never accept one under a publicly-known empty key.
#[tokio::test]
async fn the_empty_signing_secret_fails_every_report_closed() {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    catalog.add(Uuid::new_v4(), "main", universe_test("tests/a.py"));
    let launcher = Arc::new(FakeRunsLauncher::default());
    let service = CollectService::new(
        db,
        OrmCollectRepository,
        catalog as Arc<dyn CatalogReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret(String::new()),
        None,
    );

    // The signature an attacker would have to guess is the HMAC of the empty
    // key over the payload — computable by anyone who reads this source, and
    // deliberately still refused.
    let sig = service.sign(REPO, "main", TENANT);
    let err = service
        .record_count(
            TenantBound::new(TENANT).unwrap(),
            REPO,
            "main",
            &sig,
            "tests/a.py",
            5,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Forbidden), "{err:?}");
}

/// Fix round 2, secret hygiene: `is_empty()` alone let a one-character or
/// whitespace-only secret count as "configured" and be accepted at face
/// value. A five-byte secret is well short of
/// `collect::MIN_SIGNING_SECRET_LEN` and must be refused exactly like an
/// empty one — even for a signature genuinely computed under that same weak
/// secret, which is the case that distinguishes this from
/// [`the_empty_signing_secret_fails_every_report_closed`]: the failure here
/// is the length floor, not a signature mismatch.
#[tokio::test]
async fn a_too_short_signing_secret_fails_every_report_closed() {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    catalog.add(Uuid::new_v4(), "main", universe_test("tests/a.py"));
    let launcher = Arc::new(FakeRunsLauncher::default());
    let service = CollectService::new(
        db,
        OrmCollectRepository,
        catalog as Arc<dyn CatalogReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret("short".to_owned()),
        None,
    );

    let sig = service.sign(REPO, "main", TENANT);
    let err = service
        .record_count(
            TenantBound::new(TENANT).unwrap(),
            REPO,
            "main",
            &sig,
            "tests/a.py",
            5,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Forbidden), "{err:?}");
}

/// The other half of the same guard: a secret that is only whitespace must
/// be treated the same as an empty one, not as "sixteen-plus characters, so
/// configured".
#[tokio::test]
async fn a_whitespace_only_signing_secret_fails_every_report_closed() {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    catalog.add(Uuid::new_v4(), "main", universe_test("tests/a.py"));
    let launcher = Arc::new(FakeRunsLauncher::default());
    let whitespace_secret = " ".repeat(20);
    let service = CollectService::new(
        db,
        OrmCollectRepository,
        catalog as Arc<dyn CatalogReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret(whitespace_secret),
        None,
    );

    let sig = service.sign(REPO, "main", TENANT);
    let err = service
        .record_count(
            TenantBound::new(TENANT).unwrap(),
            REPO,
            "main",
            &sig,
            "tests/a.py",
            5,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Forbidden), "{err:?}");
}

/// Minor 5 of fix round 1: the query-parameter move that closed the
/// branch-in-path hazard made an empty `branch` representable, where
/// legacy's path segment could not be empty at all. An empty-keyed row is one
/// `expected_cases` can never match, so it must be rejected rather than
/// silently stored.
#[tokio::test]
async fn an_empty_branch_is_rejected_even_with_a_valid_signature() {
    let f = fixture().await;
    // Signed for the *exact* triple being submitted, including the blank
    // branch — this is not a signature-verification failure, it is the
    // branch guard specifically.
    let sig = f.sig_for(REPO, "   ", TENANT);

    let err = f
        .service
        .record_count(
            TenantBound::new(TENANT).unwrap(),
            REPO,
            "   ",
            &sig,
            "tests/a.py",
            5,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::Validation { field, .. } if field == "branch"),
        "{err:?}"
    );
}

/// A granted trigger with no `branch` falls back to
/// `default_collect_branch` — legacy's own
/// `branch.unwrap_or(DEFAULT_COLLECT_BRANCH)` (`analytics.rs:2662`) — and
/// echoes it back, matching `api_collect_trigger`'s `{ launched, branch }`
/// response shape.
#[tokio::test]
async fn trigger_with_no_branch_falls_back_to_the_default_and_echoes_it() {
    let f = fixture().await;
    let (launched, branch) = f.service.trigger(&f.ctx, None).await.expect("granted");
    assert_eq!(launched, 1);
    assert_eq!(branch, "main");

    // A blank branch is the same as no branch — legacy's `normalize_optional`.
    let (_, branch) = f
        .service
        .trigger(&f.ctx, Some("   "))
        .await
        .expect("granted");
    assert_eq!(branch, "main");
}

/// `trigger` is [`CollectService`]'s **one** PDP-checked method — see
/// [`super::collect`]'s header for why `record_count` deliberately is not one.
/// A denied caller must not launch anything, and this is the only test in
/// this module that reaches the PDP at all: every other test above uses
/// [`TenantScopedAuthZ`], which always grants.
#[tokio::test]
async fn trigger_requires_the_qa_test_result_collect_grant() {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    catalog.add(Uuid::new_v4(), "main", universe_test("tests/a.py"));
    let launcher = Arc::new(FakeRunsLauncher::default());
    let service = CollectService::new(
        db,
        OrmCollectRepository,
        catalog as Arc<dyn CatalogReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::new(DenyAllAuthZ)),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret(SECRET.to_owned()),
        None,
    );

    let err = service.trigger(&ctx(TENANT), None).await.unwrap_err();
    assert!(matches!(err, DomainError::Forbidden), "{err:?}");
    assert!(
        launcher.calls().is_empty(),
        "a denied caller must not launch anything"
    );
}

/// **Important 2 of fix round 1: the `collect` action string had no
/// witness.** `trigger_requires_the_qa_test_result_collect_grant` (above)
/// would pass unchanged if `trigger` asked the PDP for `rebuild`, `list`, or
/// a misspelling of `collect` — a `DenyAllAuthZ` double denies every
/// request regardless of what it was asked. [`RecordingAuthZ`] is this
/// crate's own precedent for pinning the exact `(resource, action)` pair a
/// caller asks for — `reconcile_tests`' own heading calls the action string
/// "a security surface, and this is its only witness", and this module had
/// none until this fix round.
#[tokio::test]
async fn trigger_asks_the_pdp_for_exactly_qa_test_result_collect() {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    catalog.add(Uuid::new_v4(), "main", universe_test("tests/a.py"));
    let launcher = Arc::new(FakeRunsLauncher::default());
    let recorder = Arc::new(RecordingAuthZ::default());
    let service = CollectService::new(
        db,
        OrmCollectRepository,
        catalog as Arc<dyn CatalogReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::clone(&recorder) as Arc<dyn AuthZResolverApi>),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret(SECRET.to_owned()),
        None,
    );

    service.trigger(&ctx(TENANT), None).await.expect("granted");

    assert_eq!(
        recorder.asked(),
        vec![("qa.test_result".to_owned(), "collect".to_owned())],
    );
}

/// [`toolkit_security::AccessScope::for_tenant`] is exercised implicitly by
/// every test above
/// (`record_count` never reaches a PDP), so this pins the one property those
/// tests do not: two tenants' counts for the identical
/// `(repo, branch, file)` triple do not collide, and passing `TENANT`'s own
/// `TenantBound` is not accidentally load-bearing for that isolation.
#[tokio::test]
async fn record_count_is_scoped_by_the_callers_own_tenant_not_by_the_pdp() {
    let f = fixture().await;
    let other_tenant = Uuid::from_u128(0x0B);

    let sig = f.sig_for(REPO, "main", TENANT);
    let other_sig = f.sig_for(REPO, "main", other_tenant);

    f.service
        .record_count(
            TenantBound::new(TENANT).unwrap(),
            REPO,
            "main",
            &sig,
            "tests/a.py",
            3,
        )
        .await
        .expect("this tenant's report");
    f.service
        .record_count(
            TenantBound::new(other_tenant).unwrap(),
            REPO,
            "main",
            &other_sig,
            "tests/a.py",
            99,
        )
        .await
        .expect("the other tenant's report, same natural key, different tenant, its own valid signature");

    assert_eq!(
        f.count_for("tests/a.py").await,
        3,
        "the other tenant's write must not be visible under this tenant's scope"
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

/// **One cycle is one observation on both of its instruments.**
///
/// The pairing is the property: a rate read off the counter and a p95 read off
/// the histogram must never disagree about how many cycles there were, which is
/// why the port takes the duration rather than leaving the histogram to a
/// second call.
#[tokio::test]
async fn a_collect_cycle_records_one_observation() {
    let f = fixture().await;

    let launched = f
        .service
        .run_collect_cycle(&f.ctx, "main")
        .await
        .expect("the cycle runs");
    // A premise, not the assertion: a cycle that launched nothing would still
    // emit, and this test would then be measuring an empty loop.
    assert_eq!(launched, 1);

    let series = f.metrics.collect();
    assert_eq!(
        series.counter(QA_INSIGHTS_COLLECT),
        1,
        "one cycle, one increment; the exported names were {:?}",
        series.names()
    );
    assert_eq!(series.histogram_count(QA_INSIGHTS_COLLECT_DURATION), 1);
    assert_eq!(
        series.counter_with(QA_INSIGHTS_COLLECT, &[("outcome", "completed")]),
        1
    );
}

/// **A cycle whose per-repository launches all fail is still `completed`.**
///
/// `run_collect_cycle`'s own doc: a launch failure is logged, skipped and not
/// fatal. That is deliberate and this pins the metric to it — folding those
/// into the cycle's outcome would make `failed` mean "something, somewhere",
/// which is not a signal an alert can be written against. What the operator
/// reads instead is the returned count against the universe size.
#[tokio::test]
async fn a_cycle_whose_launches_were_refused_is_not_a_failed_cycle() {
    let f = fixture_with_two_repos_one_missing_the_branch().await;

    let launched = f
        .service
        .run_collect_cycle(&f.ctx, "feature-x")
        .await
        .expect("the cycle runs");
    assert_eq!(launched, 1, "premise: one of the two launches was refused");

    let series = f.metrics.collect();
    assert_eq!(
        series.counter_with(QA_INSIGHTS_COLLECT, &[("outcome", "completed")]),
        1
    );
    assert_eq!(
        series.counter_with(QA_INSIGHTS_COLLECT, &[("outcome", "failed")]),
        0
    );
}

/// **A broken sibling is this gear's failure; a refused one is not.**
///
/// The two arms are the same code path with a different error, so nothing but
/// the label can tell them apart — and they need opposite responses. A
/// `Forbidden` universe read is a policy to fix and must not page anybody; an
/// unreachable qa-catalog is the series an alert fires on.
#[tokio::test]
async fn a_refused_cycle_and_a_broken_one_record_different_outcomes() {
    let refused = fixture().await;
    refused.catalog.fail_reads(CatalogFailure::Forbidden);
    refused
        .service
        .run_collect_cycle(&refused.ctx, "main")
        .await
        .expect_err("a refused universe read fails the cycle");

    let series = refused.metrics.collect();
    assert_eq!(
        series.counter_with(QA_INSIGHTS_COLLECT, &[("outcome", "refused")]),
        1,
        "a denied universe read is the caller's policy, not this gear's fault"
    );
    assert_eq!(series.histogram_count(QA_INSIGHTS_COLLECT_DURATION), 1);

    let broken = fixture().await;
    broken.catalog.fail_reads(CatalogFailure::Internal);
    broken
        .service
        .run_collect_cycle(&broken.ctx, "main")
        .await
        .expect_err("an unreachable qa-catalog fails the cycle");

    let series = broken.metrics.collect();
    assert_eq!(
        series.counter_with(QA_INSIGHTS_COLLECT, &[("outcome", "failed")]),
        1,
        "an unreachable sibling is this gear's incident to answer"
    );
}

/// **An accepted report is counted, so the refusal rates below have a
/// denominator.**
#[tokio::test]
async fn an_accepted_report_records_itself() {
    let f = fixture().await;
    let sig = f.sig_for(REPO, "main", TENANT);

    f.service
        .record_count(
            TenantBound::new(TENANT).unwrap(),
            REPO,
            "main",
            &sig,
            "tests/a.py",
            5,
        )
        .await
        .expect("a correctly signed report");

    let series = f.metrics.collect();
    assert_eq!(
        series.counter_with(QA_INSIGHTS_COLLECT_REPORT, &[("outcome", "recorded")]),
        1,
        "the exported names were {:?}",
        series.names()
    );
}

/// **Each HMAC refusal path is counted as its own class.**
///
/// This is the finding the family exists for. All three refusals answer the
/// caller with one indistinguishable `Forbidden` — deliberately, so the
/// response is not an oracle — so **the metric is the only place they are
/// separable at all**, and an operator who cannot separate them cannot act:
/// `secret_unconfigured` is a config file to fix, `signature_invalid` is a
/// stale runner or somebody guessing.
///
/// Each arm builds its own service and its own probe, because the secret is
/// constructor state and the three cases need three different secrets.
#[tokio::test]
async fn every_hmac_refusal_is_counted_under_its_own_class() {
    // Unconfigured: fail-closed, even for a signature computed under the very
    // same empty secret.
    let (service, metrics) = metered_service("").await;
    let sig = service.sign(REPO, "main", TENANT);
    refuse(&service, "main", &sig).await;
    assert_eq!(
        metrics.collect().counter_with(
            QA_INSIGHTS_COLLECT_REPORT,
            &[("outcome", "secret_unconfigured")]
        ),
        1,
        "an unconfigured secret is a deployment mistake and must not read as an attack"
    );

    // Malformed: not hex at all, so there is no tag to compare.
    let (service, metrics) = metered_service(SECRET).await;
    refuse(&service, "main", "zzzz-not-hex").await;
    assert_eq!(
        metrics.collect().counter_with(
            QA_INSIGHTS_COLLECT_REPORT,
            &[("outcome", "signature_malformed")]
        ),
        1
    );

    // Invalid: well-formed hex that does not verify.
    let (service, metrics) = metered_service(SECRET).await;
    refuse(&service, "main", &"ab".repeat(32)).await;
    assert_eq!(
        metrics.collect().counter_with(
            QA_INSIGHTS_COLLECT_REPORT,
            &[("outcome", "signature_invalid")]
        ),
        1
    );

    // And a shape guard, which is a different class again: the signature
    // verified and the payload did not.
    let (service, metrics) = metered_service(SECRET).await;
    let blank_branch_sig = service.sign(REPO, "   ", TENANT);
    refuse(&service, "   ", &blank_branch_sig).await;
    let series = metrics.collect();
    assert_eq!(
        series.counter_with(QA_INSIGHTS_COLLECT_REPORT, &[("outcome", "invalid")]),
        1
    );
    assert_eq!(
        series.counter_with(
            QA_INSIGHTS_COLLECT_REPORT,
            &[("outcome", "signature_invalid")]
        ),
        0,
        "a valid signature over a malformed payload is not a signature failure"
    );
}

/// A [`CollectService`] over `secret`, with its own probe. The report tests
/// need three different secrets, and the secret is constructor state.
async fn metered_service(secret: &str) -> (CollectService<OrmCollectRepository>, MetricsProbe) {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    let launcher = Arc::new(FakeRunsLauncher::default());
    let metrics = MetricsProbe::new();
    let service = CollectService::new(
        db,
        OrmCollectRepository,
        catalog as Arc<dyn CatalogReader>,
        launcher as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret(secret.to_owned()),
        Some(metrics.adapter()),
    );
    (service, metrics)
}

/// Submit one report that must be refused, and assert only that it was — each
/// caller then reads which class it was counted under.
async fn refuse(service: &CollectService<OrmCollectRepository>, branch: &str, sig: &str) {
    let err = service
        .record_count(
            TenantBound::new(TENANT).unwrap(),
            REPO,
            branch,
            sig,
            "tests/a.py",
            5,
        )
        .await
        .expect_err("this report must be refused");
    assert!(
        matches!(err, DomainError::Forbidden | DomainError::Validation { .. }),
        "{err:?}"
    );
}

/// **A panicking metrics adapter does not fail the cycle, and is called once.**
///
/// Two properties in one test because the second is what makes the first
/// survivable in production. The guard catches the panic so the measured path
/// is unaffected; the latch means a *persistently* broken adapter is called
/// once and then never again, rather than writing a panic line to stderr on
/// every cycle.
#[tokio::test]
async fn a_broken_metrics_adapter_does_not_fail_a_collect_cycle() {
    #[derive(Default)]
    struct Panicking {
        calls: Mutex<usize>,
    }
    impl CollectMetrics for Panicking {
        fn collect_cycle(&self, _outcome: CollectOutcome, _duration: std::time::Duration) {
            *self.calls.lock().unwrap() += 1;
            panic!("this adapter is broken");
        }
        fn collect_report(&self, _outcome: CollectReportOutcome) {}
    }

    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    catalog.add(Uuid::new_v4(), "main", universe_test("tests/a.py"));
    let launcher = Arc::new(FakeRunsLauncher::default());
    let broken = Arc::new(Panicking::default());
    let service = CollectService::new(
        db,
        OrmCollectRepository,
        catalog as Arc<dyn CatalogReader>,
        Arc::clone(&launcher) as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret(SECRET.to_owned()),
        Some(Arc::clone(&broken) as Arc<dyn CollectMetrics>),
    );

    let ctx = ctx(TENANT);
    assert_eq!(
        service
            .run_collect_cycle(&ctx, "main")
            .await
            .expect("a broken metrics adapter must not fail the cycle"),
        1
    );
    assert_eq!(
        service
            .run_collect_cycle(&ctx, "main")
            .await
            .expect("nor the next one"),
        1
    );

    assert_eq!(
        *broken.calls.lock().unwrap(),
        1,
        "the second cycle must not reach an adapter that has already panicked"
    );
    assert_eq!(
        launcher.calls().len(),
        2,
        "premise: both cycles really ran and really launched"
    );
}

/// **The recorded cycle duration really is a clock around the cycle.**
///
/// Every other collect metric assertion here is about counts and labels, and
/// counts cannot see a *value*: a call site that recorded `Duration::ZERO`, or
/// a constant, or an `Instant` taken in the wrong place would satisfy all of
/// them and hand a dashboard a fabricated distribution. Measured in
/// qa-environments during Task 40 — a mutation replacing the elapsed time with
/// a six-second constant passed that gear's whole suite, and this family had
/// the same hole.
///
/// Written as **two bracketing assertions rather than one equality**, because
/// an equality would be a timing test:
///
/// * the catalog's universe read sleeps 150 ms and the cycle awaits it before
///   launching anything, so the sample cannot be in a bucket whose upper edge
///   is 100 ms or below — that direction is deterministic, since a sleep can
///   only overrun;
/// * and it must not be in the `(5 s, 10 s]` bucket, which no in-memory fixture
///   can honestly reach.
///
/// Probed edge by edge rather than through one call: `histogram_bucket_of`
/// answers for the single bucket a value falls in, so asking about one edge
/// says nothing about the buckets below it.
#[tokio::test]
async fn the_recorded_cycle_duration_tracks_the_cycle_it_measures() {
    let delay = std::time::Duration::from_millis(150);
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let catalog = Arc::new(FakeCatalog::default());
    catalog.add(Uuid::new_v4(), "main", universe_test("tests/a.py"));
    let metrics = MetricsProbe::new();
    let service = CollectService::new(
        Arc::clone(&db),
        OrmCollectRepository,
        Arc::new(SlowCatalog::new(Arc::clone(&catalog), delay)) as Arc<dyn CatalogReader>,
        Arc::new(FakeRunsLauncher::default()) as Arc<dyn RunsLauncher>,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        DefaultCollectBranch("main".to_owned()),
        CollectReportBaseUrl("http://insights.example".to_owned()),
        CollectReportSigningSecret(SECRET.to_owned()),
        Some(metrics.adapter()),
    );

    let launched = service
        .run_collect_cycle(&ctx(TENANT), "main")
        .await
        .expect("the cycle runs");
    assert_eq!(launched, 1, "premise: the cycle really did its work");

    let series = metrics.collect();
    assert_eq!(
        series.histogram_count(QA_INSIGHTS_COLLECT_DURATION),
        1,
        "premise: exactly one cycle was timed"
    );
    for edge in [0.01_f64, 0.05, 0.1] {
        assert_eq!(
            series.histogram_bucket_of(QA_INSIGHTS_COLLECT_DURATION, edge),
            Some(0),
            "a cycle whose universe read slept for {delay:?} cannot have been measured at \
             {edge} s or less, so that bucket must be empty -- a zero or a near-zero here \
             means the clock is not around the cycle"
        );
    }
    assert_eq!(
        series.histogram_bucket_of(QA_INSIGHTS_COLLECT_DURATION, 6.0),
        Some(0),
        "and it cannot have taken between five and ten seconds either: an in-memory \
         fixture does not, so a sample there is a fabricated or stale duration rather \
         than a measured one"
    );
}
