//! Tenant isolation against a **real** in-memory `SQLite` database: this
//! gear's actual migrations, its `SecureORM` repositories, and a
//! `PolicyEnforcer` compiling a real tenant constraint.
//!
//! # Why this suite exists beside the mock-backed ones
//!
//! Every other `*_tests` module in this module tree drives hand-rolled
//! repository doubles. Those prove service logic, and they prove it against
//! doubles that *choose* to honour the scope they are handed - which is the one
//! thing tenant isolation must not rest on. Here the chain runs end to end:
//! `PolicyEnforcer` -> `AccessScope` -> `.secure().scope_with(scope)` ->
//! `SQLite`, so a repository that forgot to scope a query fails at the row
//! level rather than in plumbing.
//!
//! Adapted from `qa-catalog/src/domain/service/tests_tenant_scoping.rs`, which
//! adapts qa-environments' in turn.
//!
//! # The mocks carry real tenant ids
//!
//! `OWNER_TENANT` and `OTHER_TENANT` are non-nil and distinct, and every
//! fixture is written under one of them. A suite using `Uuid::nil()` would pass
//! against a repository with no tenant predicate at all, because the nil tenant
//! is also what an unconstrained scope produces.
//!
//! # What this suite does not cover
//!
//! The cross-gear reads - qa-catalog and qa-environments - are doubles here, so
//! nothing below proves that *their* tenancy holds. What it proves is that a
//! foreign platform or plan id, however obtained, cannot be turned into a run
//! or a queue row in this gear's tables.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use crate::domain::service::DbProvider;
use qa_runs_sdk::{Exclusivity, FinishedRunCursor, LaunchRequest, RunSource, RunState, RunTarget};
use toolkit_db::DBProvider;
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use super::admission::tests::fakes::{FakeCatalog, FakeEnvironments, PLATFORM_A, REPO};
use super::test_support::{NullLogArchive, OTHER_TENANT, OWNER_TENANT, ctx};
use super::{AppServices, LogArchive, QueueLimits, ServiceDeps};
use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::RunExecutor;
use crate::domain::repos::RunsRepository;
use crate::gear::ConcreteAppServices;
use crate::infra::executor::mock::MockRunExecutor;
use crate::infra::logs::RunLogBroadcaster;
use crate::infra::storage::test_db::{inmem_db, scope};
use crate::infra::storage::{OrmQueueRepository, OrmRunsRepository};

/// The real services over a real database.
///
/// The repositories and the `PolicyEnforcer` are production types; only the
/// two cross-gear clients and the executor are doubles, and none of those
/// touches this gear's tables.
/// The `DbProvider` is returned alongside, because several tests read the
/// tables **directly**: the service's own reads are themselves scoped, so they
/// could not tell "nothing was written" from "you are not allowed to look".
async fn services() -> (Arc<ConcreteAppServices>, Arc<DbProvider>) {
    services_with(
        Arc::new(FakeCatalog::serving(&["tests/a.py"])),
        Arc::new(FakeEnvironments::free()),
        Arc::new(super::test_support::PermissiveAuthZ),
    )
    .await
}

/// [`services`] with the three doubles a test needs to vary: what qa-catalog
/// answers, what qa-environments answers, and what the policy decision point
/// decides.
async fn services_with(
    catalog: Arc<FakeCatalog>,
    environments: Arc<FakeEnvironments>,
    authz: Arc<dyn authz_resolver_sdk::AuthZResolverApi>,
) -> (Arc<ConcreteAppServices>, Arc<DbProvider>) {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let services = Arc::new(AppServices::new(
        Arc::new(OrmRunsRepository),
        Arc::new(OrmQueueRepository),
        Arc::new(crate::infra::storage::OrmSchedulesRepository),
        ServiceDeps {
            db: Arc::clone(&db),
            authz,
            catalog,
            environments,
            product_plugins: Arc::new(
                crate::domain::service::admission::tests::fakes::FakeProductPlugins::default(),
            ),
            executor: Arc::new(MockRunExecutor::new()) as Arc<dyn RunExecutor>,
            logs: Arc::new(RunLogBroadcaster::new(8)),
            archive: Arc::new(NullLogArchive) as Arc<dyn LogArchive>,
            admitter: None,
            dispatcher: None,
            // `None` wires the real watcher, which is what these tests want: the
            // point is that a production wiring stays tenant-scoped.
            watcher: None,
            cancel: tokio_util::sync::CancellationToken::new(),
            default_timeout_seconds: 3600,
            limits: QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            },
            orphan_timeout_seconds: 600,
            // `None` is the production default: `NoopMetrics`, which emits
            // everything a wired gear emits and lets nothing observe it.
            dispatch_metrics: None,
            ingest_metrics: None,
        },
    ));
    (services, db)
}

/// A launch aimed at `platform`, in whatever tenant the caller is in.
fn launch_against(platform: Option<Uuid>) -> LaunchRequest {
    LaunchRequest {
        target: RunTarget::Plan {
            repo_id: REPO,
            path: "plans/smoke.yaml".to_owned(),
        },
        environment_id: platform,
        branch: Some("main".to_owned()),
        include_tags: vec![],
        exclude_tags: vec![],
        parameters: vec![],
        exclusive: Exclusivity::Inherit,
        timeout_seconds: None,
        source: RunSource::Manual,
        schedule_id: None,
    }
}

/// Launch a run under `tenant` and return its id.
async fn seed_run(svc: &ConcreteAppServices, tenant: Uuid) -> Uuid {
    svc.launch
        .launch(&ctx(tenant), launch_against(Some(PLATFORM_A)))
        .await
        .expect("a launch in the caller's own tenant must succeed")
        .run_id()
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_is_invisible_to_another_tenant() {
    let (svc, _db) = services().await;
    let run = seed_run(&svc, OWNER_TENANT).await;

    assert_eq!(svc.runs.get(&ctx(OWNER_TENANT), run).await.unwrap().id, run);

    let stranger = svc.runs.get(&ctx(OTHER_TENANT), run).await;
    assert!(
        matches!(stranger, Err(DomainError::RunNotFound { .. })),
        "absent and foreign must be indistinguishable: {stranger:?}"
    );
    assert!(
        svc.runs
            .list(&ctx(OTHER_TENANT), &ODataQuery::new())
            .await
            .unwrap()
            .items
            .is_empty(),
        "and it must not appear in the other tenant's listing either"
    );
}

#[tokio::test]
async fn a_queue_row_is_invisible_to_another_tenant() {
    let (svc, _db) = services().await;
    // The launch files its own row - `insert` is unique on `(tenant, run)`, so
    // filing a second one here would fail with `QueueRowExists` rather than
    // testing anything.
    let _run = seed_run(&svc, OWNER_TENANT).await;

    assert_eq!(
        svc.runs
            .queue_page(&ctx(OWNER_TENANT), None, &ODataQuery::new())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(
        svc.runs
            .queue_page(&ctx(OTHER_TENANT), None, &ODataQuery::new())
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Launch
// ---------------------------------------------------------------------------

/// **Not-found, not forbidden.** Telling a caller that a platform exists but is
/// not theirs is the cross-tenant existence oracle every read in this gear is
/// written to close, and the launch path is where an attacker would probe it.
#[tokio::test]
async fn launching_against_another_tenants_platform_is_not_found_not_forbidden() {
    // `FakeEnvironments::default()` serves the platform read to **no** tenant,
    // which is what a platform in another tenant looks like from here: the
    // sibling gear refuses, indistinguishably from one that does not exist.
    // `free()` would have served it - the double gates on tenant, not on
    // platform id - and this test passed against a launch that ignored the
    // refusal entirely until that was corrected.
    let (svc, db) = services_with(
        Arc::new(FakeCatalog::serving(&["tests/a.py"])),
        Arc::new(FakeEnvironments::default()),
        Arc::new(super::test_support::PermissiveAuthZ),
    )
    .await;

    let error = svc
        .launch
        .launch(&ctx(OWNER_TENANT), launch_against(Some(PLATFORM_A)))
        .await
        .expect_err("a platform this caller cannot read must not be launchable against");

    assert!(
        !matches!(error, DomainError::Forbidden),
        "a denial must not distinguish 'exists but not yours' from 'does not exist': {error:?}"
    );
    assert!(
        OrmRunsRepository
            .list(&db.conn().unwrap(), &scope(OWNER_TENANT))
            .await
            .unwrap()
            .is_empty(),
        "and it must fail before writing a run"
    );
}

#[tokio::test]
async fn launching_against_another_tenants_custom_plan_is_not_found() {
    let (svc, db) = services_with(
        Arc::new(FakeCatalog::refusing_custom_plans()),
        Arc::new(FakeEnvironments::free()),
        Arc::new(super::test_support::PermissiveAuthZ),
    )
    .await;
    let foreign_plan = Uuid::from_u128(0xC0FF);

    let error = svc
        .launch
        .launch(
            &ctx(OWNER_TENANT),
            LaunchRequest {
                target: RunTarget::CustomPlan { id: foreign_plan },
                ..launch_against(Some(PLATFORM_A))
            },
        )
        .await
        .expect_err("a custom plan this caller cannot read must not be launchable");

    assert!(!matches!(error, DomainError::Forbidden), "{error:?}");
    assert!(
        OrmRunsRepository
            .list(&db.conn().unwrap(), &scope(OWNER_TENANT))
            .await
            .unwrap()
            .is_empty(),
        "a refused plan must not leave a run behind"
    );
}

/// A policy decision point that denies must fail the launch **closed** - no
/// run, no queue row - rather than falling back to an unconstrained scope.
#[tokio::test]
async fn a_pdp_denial_on_create_fails_the_launch_closed() {
    let (svc, db) = services_with(
        Arc::new(FakeCatalog::serving(&["tests/a.py"])),
        Arc::new(FakeEnvironments::free()),
        Arc::new(super::admission::tests::fakes::DenyingAuthZ),
    )
    .await;

    let error = svc
        .launch
        .launch(&ctx(OWNER_TENANT), launch_against(Some(PLATFORM_A)))
        .await
        .expect_err("a denied decision must not produce a run");
    assert!(matches!(error, DomainError::Forbidden), "{error:?}");

    // Read the table directly: the service's own list would also be denied,
    // so it could not tell "nothing was written" from "you cannot look".
    assert!(
        OrmRunsRepository
            .list(&db.conn().unwrap(), &scope(OWNER_TENANT))
            .await
            .unwrap()
            .is_empty(),
        "a denied launch must leave no row behind"
    );
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_another_tenants_run_is_not_found() {
    let (svc, _db) = services().await;
    let run = seed_run(&svc, OWNER_TENANT).await;

    let error = svc
        .runs
        .cancel(&ctx(OTHER_TENANT), run)
        .await
        .expect_err("a foreign run must not be cancellable");
    assert!(
        matches!(error, DomainError::RunNotFound { .. }),
        "{error:?}"
    );

    assert_ne!(
        svc.runs.get(&ctx(OWNER_TENANT), run).await.unwrap().state,
        RunState::Canceled,
        "and the owner's run must be untouched"
    );
}

#[tokio::test]
async fn force_starting_another_tenants_queue_row_is_not_found() {
    let (svc, _db) = services().await;
    let _run = seed_run(&svc, OWNER_TENANT).await;
    let row = svc
        .runs
        .queue_page(&ctx(OWNER_TENANT), None, &ODataQuery::new())
        .await
        .unwrap()
        .items
        .pop()
        .expect("the launch files a queue row");

    let error = svc
        .runs
        .force_start(&ctx(OTHER_TENANT), row.id)
        .await
        .expect_err("a foreign queue row must not be force-startable");
    assert!(
        matches!(error, DomainError::QueueRowNotFound { .. }),
        "{error:?}"
    );
}

/// The uniqueness index is `(tenant_id, name)`, not `(name)`. A global one
/// would leak the existence of another tenant's run through a name collision -
/// and would make two tenants' naming sequences interfere.
#[tokio::test]
async fn the_same_run_name_in_two_tenants_does_not_collide() {
    let (svc, _db) = services().await;
    let mine = seed_run(&svc, OWNER_TENANT).await;
    let theirs = seed_run(&svc, OTHER_TENANT).await;

    let mine_name = svc.runs.get(&ctx(OWNER_TENANT), mine).await.unwrap().name;
    let theirs_name = svc.runs.get(&ctx(OTHER_TENANT), theirs).await.unwrap().name;

    assert_eq!(
        mine_name, theirs_name,
        "both tenants' first run of the same plan takes the same name, which is \
         only possible because the index is per tenant"
    );
    assert_ne!(mine, theirs);
}

/// **Both of the reconciler's reads, end to end, through the SDK client
/// itself.**
///
/// `list_runs_finished_since` and `list_run_test_results` exist for exactly one
/// consumer - qa-insights' reconciler - and that consumer runs in another gear
/// under whatever identity the broker gap handed it. So the interesting
/// question is not whether the service filters, it is whether the *shipped*
/// path filters: `QaRunsLocalClient` -> `RunsService` -> real `PolicyEnforcer`
/// -> `SecureORM` -> real migrated tables. Everything else in this pair's
/// coverage runs against doubles that choose to honour a scope.
///
/// The fixture is deliberately symmetric - both tenants own a run that finished
/// inside the same window and carries a per-test row - so "returns nothing"
/// cannot pass by the query being broken for everybody.
#[tokio::test]
async fn the_reconciler_reads_are_invisible_to_another_tenant() {
    use crate::domain::local_client::QaRunsLocalClient;
    use crate::domain::repos::{NewTestResult, RunStatePatch};
    use qa_runs_sdk::QaRunsClientV1;
    use time::macros::datetime;

    const FINISHED: time::OffsetDateTime = datetime!(2026-08-18 10:00:00 UTC);
    const WATERMARK: time::OffsetDateTime = datetime!(2026-08-18 09:00:00 UTC);

    let (svc, db) = services().await;
    let conn = db.conn().unwrap();

    // Finish a launched run at `FINISHED` and give it one per-test row, under
    // `tenant`. Written through the real repository rather than by hand: these
    // are the same two writers the ingest path uses, so the fixture cannot be a
    // row shape production never produces.
    let seed = async |tenant: Uuid| -> Uuid {
        let run = seed_run(&svc, tenant).await;
        let state = svc.runs.get(&ctx(tenant), run).await.unwrap().state;
        let moved = OrmRunsRepository
            .update_state(
                &conn,
                &scope(tenant),
                run,
                state,
                RunState::Succeeded,
                RunStatePatch {
                    started_at: None,
                    finished_at: Some(FINISHED),
                    error: None,
                },
            )
            .await
            .unwrap();
        assert!(moved, "the fixture run must reach a terminal state");
        let owned = OrmRunsRepository
            .resolve_owned(&conn, &scope(tenant), run)
            .await
            .unwrap();
        OrmRunsRepository
            .upsert_test_result(
                &conn,
                &scope(tenant),
                tenant,
                owned,
                NewTestResult {
                    test_file: "tests/authn/test_a.py".to_owned(),
                    test_name: "test_login".to_owned(),
                    status: "PASSED".to_owned(),
                    duration: Some("1.5s".to_owned()),
                    launch_id: None,
                    jira_key: None,
                    nodeid: Some("tests/authn/test_a.py::test_login".to_owned()),
                    reason: None,
                    ticket: None,
                },
            )
            .await
            .unwrap();
        run
    };

    let mine = seed(OWNER_TENANT).await;
    let theirs = seed(OTHER_TENANT).await;

    let client = QaRunsLocalClient::new(Arc::clone(&svc));

    // The sweep: each tenant sees its own run and only its own.
    let swept = client
        .list_runs_finished_since(
            &ctx(OWNER_TENANT),
            FinishedRunCursor::starting_at(WATERMARK),
            100,
        )
        .await
        .unwrap();
    assert_eq!(
        swept.iter().map(|run| run.id).collect::<Vec<_>>(),
        vec![mine],
        "the other tenant's run finished inside the same window; a sweep is \
         wider than a page in time, never in tenancy"
    );
    assert!(
        client
            .list_runs_finished_since(
                &ctx(OTHER_TENANT),
                FinishedRunCursor::starting_at(WATERMARK),
                100
            )
            .await
            .unwrap()
            .iter()
            .all(|run| run.id != mine),
        "and symmetrically"
    );

    // The per-test rows: visible to the owner, `not_found` to the stranger -
    // not an empty list, which would still confirm the run exists.
    let rows = client
        .list_run_test_results(&ctx(OWNER_TENANT), mine)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run_id, mine);
    assert_eq!(rows[0].nodeid, "tests/authn/test_a.py::test_login");

    let refused = client.list_run_test_results(&ctx(OTHER_TENANT), mine).await;
    let problem = refused.expect_err("another tenant must not read these rows");
    assert_eq!(
        toolkit::api::canonical_prelude::Problem::from_error(&problem)
            .expect("a problem must serialize")
            .status,
        Some(404),
        "and it must be indistinguishable from a run that does not exist, \
         which is what closes the cross-tenant existence oracle: {problem}"
    );
    // The stranger's own rows are still readable, so the refusal above is the
    // scope talking and not a broken read.
    assert_eq!(
        client
            .list_run_test_results(&ctx(OTHER_TENANT), theirs)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// **The ownership precheck, at the row level.** `qa_run_queue.run_id`
/// references `qa_runs(id)` with no tenant component, so an insert carrying an
/// attacker-chosen `run_id` would succeed if that run exists in *any* tenant -
/// turning the foreign key into a membership test over another tenant's run
/// ids. `OwnedRunId` is what stops it, and this drives that: resolving the
/// foreign run under the attacker's own scope is what fails, before any insert.
#[tokio::test]
async fn a_queue_row_cannot_be_created_against_another_tenants_run() {
    let (svc, db) = services().await;
    let victim = seed_run(&svc, OWNER_TENANT).await;

    let conn = db.conn().unwrap();
    let attacker = scope(OTHER_TENANT);

    let refused = OrmRunsRepository
        .resolve_owned(&conn, &attacker, victim)
        .await;
    assert!(
        matches!(refused, Err(DomainError::RunNotFound { .. })),
        "the token that every queue insert demands must not be mintable for a \
         run the caller cannot see: {refused:?}"
    );
}

// ---------------------------------------------------------------------------
// The crate-wide ban
// ---------------------------------------------------------------------------

/// **`AccessScope::allow_all()` appears in no production path.**
///
/// It exists because a probe using `allow_all()` created a cross-tenant
/// existence oracle in qa-environments: an unscoped read answers about every
/// tenant's rows, and every other guarantee in this crate is downstream of the
/// scope being real.
///
/// Textual, and reading the crate's own sources - the shape
/// `domain::system_actor`'s `every_factory_in_this_module_is_classified`
/// already uses here, for the same reason: there is no type-level way to ask
/// "does this call appear outside test code?".
///
/// # What it catches, and what it does not
///
/// It catches the direct call, which is how the qa-environments defect was
/// written. It does **not** catch an alias, a re-export under another name, a
/// scope built field-by-field to be equivalent, or a call assembled from
/// string fragments. It is a tripwire on the obvious spelling, not a proof, and
/// a reviewer should not read a green run here as "the crate cannot be
/// unscoped".
///
/// Comment lines are skipped: this crate discusses the ban in several module
/// headers, and a test that failed on its own documentation would be deleted
/// within a week.
///
/// # The exemption is `_tests.rs`, `domain/elevated.rs`, and nothing else
///
/// It used to be `starts_with("test") || ends_with("_tests.rs")`, and the first
/// half was a hole with no `#[cfg(test)]` awareness behind it: it exempted
/// `test_support.rs` silently, and it would exempt a *production* `test_utils.rs`
/// or `testing.rs` the day one was added - in a crate whose subject is test
/// runs, so those are plausible names here rather than contrived ones. Nothing
/// recorded that the exemption had been earned; the file simply had the right
/// first four letters.
///
/// Dropping it left one suffix, which every whole-file test module in this
/// crate already uses. The one file that did not, and was exempt only through
/// the prefix arm, was **this one** - renamed from `tests_tenant_scoping.rs` in
/// the same change, because it names the banned call three times in its own
/// code and would otherwise report itself. That is the whole cost of the
/// tightening, and it is the right direction: an exemption should be spelled
/// the same way everywhere or not exist.
///
/// **A second, production exemption was added when the nil-tenant
/// enumerations were moved to `domain::elevated`.** `domain::elevated` is
/// this crate's one legitimate call - its own module doc opens with *"The one
/// place this gear elevates past the policy engine"* and states why an
/// unscoped nil-tenant enumeration is not the existence oracle this guard
/// exists to catch: the trust it grants is read-only, and every write that
/// follows is re-scoped per row under a tenant-bound `system_actor` factory
/// before this crate does anything mutating with it. Exempting the file by
/// name rather than deleting the line it would otherwise flag keeps the ban
/// live everywhere else: a second, unrelated `allow_all()` anywhere outside
/// this one small, single-export file is still caught.
#[test]
fn no_production_path_uses_allow_all() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    walk(&src, &mut |path, contents| {
        // Whole-file test modules are exempt - including this one, which names
        // the banned call in its own assertion message. Files that merely
        // *contain* a `#[cfg(test)] mod tests` are **not** exempt, so a test
        // written inline in a production file would still be flagged. That is a
        // false positive waiting to happen and it is the safe direction: the
        // fix is to move the test, not to widen the rule.
        //
        // `domain/elevated.rs` is exempt too, and only that one path - see this
        // function's doc for why a second, named exemption is the right
        // instrument here rather than deleting or rewording the line it holds.
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.ends_with("_tests.rs") || path.ends_with("domain/elevated.rs") {
            return;
        }
        for (number, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if line.contains("allow_all") {
                offenders.push(format!("{}:{}", path.display(), number + 1));
            }
        }
    });

    assert!(
        offenders.is_empty(),
        "AccessScope::allow_all() must not appear in a production path: {offenders:?}"
    );
}

/// Recurse over `.rs` files under `dir`.
///
/// A hand-rolled walk rather than a dependency: `walkdir` is not a
/// dev-dependency of this crate and adding one for six lines would be a worse
/// trade than the six lines.
fn walk(dir: &std::path::Path, visit: &mut impl FnMut(&std::path::Path, &str)) {
    let entries = std::fs::read_dir(dir).expect("the crate's own src/ must be readable");
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            walk(&path, visit);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let contents = std::fs::read_to_string(&path).expect("a readable source file");
            visit(&path, &contents);
        }
    }
}
