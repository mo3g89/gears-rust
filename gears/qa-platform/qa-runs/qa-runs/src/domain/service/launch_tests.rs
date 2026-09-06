//! Unit tests for the launch service, driven entirely through mock ports.
//!
//! Included from `launch.rs` rather than declared in `service/mod.rs`, so this
//! module is a **child** of `launch` and can reach its private pure helpers
//! (`resolve_branch`, `resolve_timeout_seconds`, `group_by_repo`,
//! `default_name_base`). A sibling module could not, and the alternative —
//! widening four private functions to `pub(super)` so a sibling could see
//! them — would be visibility written for the tests' convenience.
//!
//! Every expected value for ported logic comes from **reading the source
//! system**, never from running this code and recording what it did.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use qa_runs_sdk::{LaunchOutcome, LaunchRequest, RunParameter, RunSource, RunState, RunTarget};

use super::*;
use crate::domain::repos::RunsRepository;
use crate::domain::service::test_support::{
    CUSTOM_PLAN_ID, MockCatalog, MockEnvironments, MockRunsRepository, NullLogArchive,
    OTHER_REPO_ID, OTHER_TENANT, OWNER_TENANT, PLATFORM_ID, PermissiveAuthZ, QUEUE_ID, REPO_ID,
    RecordingAdmitter, RecordingDispatcher, ctx, custom_plan_fixture, custom_plan_fixture_nested,
    meta_fixture, plan_fixture, platform_fixture, platform_fixture_with_branch, test_db_provider,
};
use crate::domain::service::{AppServices, LogArchive, ServiceDeps};
use crate::domain::timeout::MAX_LAUNCH_TIMEOUT_SECONDS;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    runs: Arc<MockRunsRepository>,
    catalog: Arc<MockCatalog>,
    environments: Arc<MockEnvironments>,
    admitter: Arc<RecordingAdmitter>,
    dispatcher: Arc<RecordingDispatcher>,
    /// The log fan-out the service was built over, so a test can assert that a
    /// refused launch released the run's channel.
    logs: Arc<crate::domain::service::ingest::tests::RecordingLogs>,
    service: LaunchService<MockRunsRepository>,
}

struct Builder {
    runs: Arc<MockRunsRepository>,
    catalog: Arc<MockCatalog>,
    environments: Arc<MockEnvironments>,
    admitter: Arc<RecordingAdmitter>,
    dispatcher: Option<Arc<RecordingDispatcher>>,
    default_timeout_seconds: u64,
}

impl Builder {
    /// A launch that resolves one single-file plan on `main`, has no platform,
    /// and is admitted for immediate dispatch.
    fn new() -> Self {
        Self {
            runs: Arc::new(MockRunsRepository::empty()),
            catalog: Arc::new(
                MockCatalog::new()
                    .with_plan(plan_fixture("Smoke Tests", &["tests/test_smoke.py"]))
                    .with_meta(
                        "tests/test_smoke.py",
                        meta_fixture("tests/test_smoke.py", &[], None),
                    ),
            ),
            environments: Arc::new(MockEnvironments::empty()),
            admitter: Arc::new(RecordingAdmitter::answering(Admission::Unqueued)),
            dispatcher: None,
            default_timeout_seconds: 0,
        }
    }

    fn runs(mut self, runs: MockRunsRepository) -> Self {
        self.runs = Arc::new(runs);
        self
    }

    fn catalog(mut self, catalog: MockCatalog) -> Self {
        self.catalog = Arc::new(catalog);
        self
    }

    fn environments(mut self, environments: MockEnvironments) -> Self {
        self.environments = Arc::new(environments);
        self
    }

    fn admitter(mut self, admitter: RecordingAdmitter) -> Self {
        self.admitter = Arc::new(admitter);
        self
    }

    fn dispatcher(mut self, dispatcher: RecordingDispatcher) -> Self {
        self.dispatcher = Some(Arc::new(dispatcher));
        self
    }

    fn default_timeout(mut self, seconds: u64) -> Self {
        self.default_timeout_seconds = seconds;
        self
    }

    async fn build(self) -> Harness {
        let dispatcher = self
            .dispatcher
            .unwrap_or_else(|| Arc::new(RecordingDispatcher::starting(Arc::clone(&self.runs))));
        let db = test_db_provider().await;
        let logs = Arc::new(crate::domain::service::ingest::tests::RecordingLogs::default());
        let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
        let service = LaunchService::new(
            db,
            Arc::clone(&self.runs),
            Arc::clone(&self.catalog) as Arc<dyn QaCatalogClientV1>,
            Arc::clone(&self.environments) as Arc<dyn QaEnvironmentsClientV1>,
            // Task 15: a terminal transition releases the run's live log channel.
            Arc::clone(&logs) as Arc<dyn crate::domain::service::LogFanout>,
            Arc::clone(&self.admitter) as Arc<dyn Admitter>,
            Arc::clone(&dispatcher) as Arc<dyn InlineDispatcher>,
            self.default_timeout_seconds,
            enforcer,
        );
        Harness {
            runs: self.runs,
            catalog: self.catalog,
            environments: self.environments,
            admitter: self.admitter,
            dispatcher,
            logs,
            service,
        }
    }
}

/// A plan-kind launch against `REPO_ID`, with nothing overridden.
fn plan_request() -> LaunchRequest {
    LaunchRequest {
        target: RunTarget::Plan {
            repo_id: REPO_ID,
            path: "tests/plan.yaml".to_owned(),
        },
        platform_id: None,
        branch: None,
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        parameters: Vec::new(),
        exclusive: None,
        timeout_seconds: None,
        source: RunSource::Manual,
        schedule_id: None,
    }
}

fn custom_plan_request() -> LaunchRequest {
    LaunchRequest {
        target: RunTarget::CustomPlan { id: CUSTOM_PLAN_ID },
        ..plan_request()
    }
}

fn param(name: &str, value: &str) -> RunParameter {
    RunParameter {
        name: name.to_owned(),
        value: value.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Rule 1: branch resolution
// ---------------------------------------------------------------------------

/// `manager/src/services/exclusivity.rs:302-304` returns the requested branch
/// before it looks at anything else.
#[test]
fn an_explicit_branch_wins_over_both_defaults() {
    assert_eq!(
        resolve_branch(Some("feature/x"), Some("release"), "main"),
        "feature/x"
    );
}

/// `exclusivity.rs:302` trims and filters the requested branch, so whitespace
/// is not a branch name and falls through.
#[test]
fn a_blank_explicit_branch_falls_through_to_the_platform_default() {
    assert_eq!(
        resolve_branch(Some("   "), Some("release"), "main"),
        "release"
    );
    assert_eq!(resolve_branch(Some(""), Some("release"), "main"), "release");
}

/// `exclusivity.rs:305-311`: the platform default is consulted before the
/// repository's, and `manager/src/routes/runs.rs:554-566` folds it into the
/// value the repository default then fills at `:639-644`. (`:553` is blank and a
/// span ending at `:562` cuts the fold's closure in half.)
#[test]
fn the_platform_default_wins_over_the_repository_default() {
    assert_eq!(resolve_branch(None, Some("release"), "main"), "release");
}

/// `exclusivity.rs:310-311` trims the platform default and filters an empty
/// one, which is what makes it fall through rather than resolve to `""`.
#[test]
fn a_blank_platform_default_falls_through_to_the_repository_default() {
    assert_eq!(resolve_branch(None, Some("  "), "main"), "main");
    assert_eq!(resolve_branch(None, None, "main"), "main");
}

/// `platform_default_branch` reads the field, which the tests above cannot
/// check because they call `resolve_branch` **directly** with a literal middle
/// argument.
///
/// That is worth being explicit about, because it is what kept those four honest
/// while the tier was inert: none of them routed through
/// `platform_default_branch`, so none was vacuous, and none needed changing when
/// Task 13b landed the field. What was genuinely missing was any test that the
/// platform's *stored* override reaches the chain at all — a
/// `platform_default_branch` that returned `None` unconditionally (which it did
/// until 2026-08-14) satisfied every one of them.
///
/// This test and the two below close that gap: the accessor, and then the whole
/// launch path.
///
/// Replaces `platform_defaults_are_not_available_yet`, which asserted `None ==
/// None` and was deleted when the tier went live. The tripwire it was mistaken
/// for is the exhaustive destructuring inside `platform_default_branch`, and it
/// fired: adding the SDK field produced `E0027` at that function.
#[test]
fn the_platform_tier_reads_the_platforms_stored_override() {
    let pinned = platform_fixture_with_branch(Some("release-9.0"));
    assert_eq!(platform_default_branch(Some(&pinned)), Some("release-9.0"));

    let unpinned = platform_fixture(Some("2.0"), Some("b7"));
    assert_eq!(
        platform_default_branch(Some(&unpinned)),
        None,
        "a platform with no override must not synthesise one"
    );

    assert_eq!(
        platform_default_branch(None),
        None,
        "and a run with no platform at all has no middle tier to read"
    );
}

/// The end-to-end shape of rule 1's middle tier: a platform pinned to a branch
/// makes **every** catalog read use that branch, not the repository's default.
///
/// This is the behaviour the whole of Task 13b exists to restore. Before the
/// field landed, this launch read `develop` — the repository default — where the
/// source system reads `release-9.0`, with no error and no warning. The mock
/// catalog's `with_repo_default("develop")` is what makes the assertion
/// discriminating: a resolution that ignored the platform would produce a
/// different, equally plausible-looking string.
#[tokio::test]
async fn a_platforms_default_branch_overrides_the_repository_default_end_to_end() {
    let harness = Builder::new()
        .environments(MockEnvironments::with_platform(
            OWNER_TENANT,
            platform_fixture_with_branch(Some("release-9.0")),
        ))
        .catalog(
            MockCatalog::new()
                .with_repo_default("develop")
                .with_plan(plan_fixture("Smoke", &["tests/test_smoke.py"]))
                .with_meta(
                    "tests/test_smoke.py",
                    meta_fixture("tests/test_smoke.py", &[], None),
                ),
        )
        .build()
        .await;

    let request = LaunchRequest {
        platform_id: Some(PLATFORM_ID),
        ..plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let seen = harness.catalog.branches_seen();
    assert!(
        !seen.is_empty(),
        "the launch must have read the catalog at least once"
    );
    assert!(
        seen.iter().all(|branch| branch == "release-9.0"),
        "the platform's default_branch must win over the repository's; got {seen:?}"
    );
}

/// And an explicit request branch still beats the platform's override, which is
/// the tier ordering the previous test on its own cannot distinguish from
/// "the platform default always wins".
#[tokio::test]
async fn an_explicit_branch_still_beats_a_pinned_platform_end_to_end() {
    let harness = Builder::new()
        .environments(MockEnvironments::with_platform(
            OWNER_TENANT,
            platform_fixture_with_branch(Some("release-9.0")),
        ))
        .catalog(
            MockCatalog::new()
                .with_repo_default("develop")
                .with_plan(plan_fixture("Smoke", &["tests/test_smoke.py"]))
                .with_meta(
                    "tests/test_smoke.py",
                    meta_fixture("tests/test_smoke.py", &[], None),
                ),
        )
        .build()
        .await;

    let request = LaunchRequest {
        branch: Some("hotfix/x".to_owned()),
        platform_id: Some(PLATFORM_ID),
        ..plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let seen = harness.catalog.branches_seen();
    // `all` is vacuously true on an empty vector, so the emptiness check is what
    // makes the assertion below mean "every read used hotfix/x" rather than
    // "no read contradicted it". Without it this test would still pass if the
    // catalog were never consulted at all -- which is precisely the class of
    // structural blindness this task exists to correct.
    assert!(
        !seen.is_empty(),
        "the launch must have read the catalog at least once"
    );
    assert!(
        seen.iter().all(|branch| branch == "hotfix/x"),
        "an explicit branch must win over a pinned platform; got {seen:?}"
    );
}

#[tokio::test]
async fn a_run_with_no_platform_uses_the_repository_default() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_repo_default("develop")
                .with_plan(plan_fixture("Smoke", &["tests/test_smoke.py"]))
                .with_meta(
                    "tests/test_smoke.py",
                    meta_fixture("tests/test_smoke.py", &[], None),
                ),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    assert!(
        harness
            .catalog
            .branches_seen()
            .iter()
            .all(|branch| branch == "develop"),
        "every catalog read must use the resolved branch, got {:?}",
        harness.catalog.branches_seen()
    );
}

// ---------------------------------------------------------------------------
// Rules 2 and 3: grouping and the ambiguity guard
// ---------------------------------------------------------------------------

#[test]
fn a_single_repo_custom_plan_produces_one_group() {
    let groups = group_by_repo(&[
        (REPO_ID, "tests/a.py".to_owned()),
        (REPO_ID, "tests/b.py".to_owned()),
        // Deduplicated, as `manager/src/services/exclusivity.rs:456-466` does.
        (REPO_ID, "tests/a.py".to_owned()),
    ]);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[&REPO_ID], vec!["tests/a.py", "tests/b.py"]);
}

#[tokio::test]
async fn a_multi_repo_custom_plan_with_an_explicit_branch_produces_one_group_per_repo() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture(&[
                    (REPO_ID, "tests/a.py"),
                    (OTHER_REPO_ID, "ui/b.spec.ts"),
                ]))
                .with_meta("tests/a.py", meta_fixture("tests/a.py", &[], None))
                .with_meta("ui/b.spec.ts", meta_fixture("ui/b.spec.ts", &[], None)),
        )
        .build()
        .await;

    let request = LaunchRequest {
        branch: Some("release-9".to_owned()),
        ..custom_plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    // One `TEST_META` read per repository group, all on the one explicit
    // branch: the property that makes rule 3's guard necessary.
    assert_eq!(harness.catalog.meta_calls(), 2);
    assert_eq!(
        harness.catalog.branches_seen(),
        vec!["release-9".to_owned(), "release-9".to_owned()]
    );
}

/// `manager/src/routes/custom_plans.rs:671-687`.
#[tokio::test]
async fn a_multi_repo_custom_plan_without_an_explicit_branch_is_a_failed_precondition() {
    let harness = Builder::new()
        .catalog(MockCatalog::new().with_custom_plan(custom_plan_fixture(&[
            (REPO_ID, "tests/a.py"),
            (OTHER_REPO_ID, "ui/b.spec.ts"),
        ])))
        .build()
        .await;

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .expect_err("a multi-repo custom plan needs an explicit branch");

    assert!(
        matches!(error, DomainError::AmbiguousBranch { groups: 2, .. }),
        "expected AmbiguousBranch, got {error:?}"
    );
    assert!(
        harness.runs.created().is_empty(),
        "the guard must fire before any run row is written"
    );
}

#[test]
fn the_ambiguous_branch_error_names_the_constraint_and_the_group_count() {
    let message = DomainError::AmbiguousBranch {
        plan_id: CUSTOM_PLAN_ID,
        groups: 3,
    }
    .to_string();
    assert!(message.contains("3 repositories"), "{message}");
    assert!(message.contains("branch must be given"), "{message}");
    assert!(
        message.contains(&CUSTOM_PLAN_ID.to_string()),
        "the message must name the plan: {message}"
    );
}

// ---------------------------------------------------------------------------
// Rule 6: test_version
// ---------------------------------------------------------------------------

/// `manager/src/routes/runs.rs:645` and `custom_plans.rs:960`:
/// `let test_version = Some(branch.clone());`. The branch label **is** the test
/// version; there is no version-to-branch mapping.
#[tokio::test]
async fn the_resolved_branch_label_is_recorded_as_the_test_version() {
    let harness = Builder::new().build().await;
    let request = LaunchRequest {
        branch: Some("  release-9  ".to_owned()),
        ..plan_request()
    };

    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let created = harness.runs.created();
    assert_eq!(created.len(), 1);
    assert_eq!(
        created[0].test_version,
        Some("release-9".to_owned()),
        "the trimmed, resolved branch label is the recorded test version"
    );
}

// ---------------------------------------------------------------------------
// Exclusivity wiring
// ---------------------------------------------------------------------------

/// `manager/src/services/exclusivity.rs:342-348`: `plan.yaml` outranks
/// `TEST_META`, so a declared flag decides without any file being consulted.
#[tokio::test]
async fn a_plan_flag_decides_without_the_catalog_being_asked_for_test_meta() {
    let mut plan = plan_fixture("Smoke", &["tests/test_smoke.py"]);
    plan.exclusive = Some(true);
    let harness = Builder::new()
        .catalog(MockCatalog::new().with_plan(plan))
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    assert_eq!(
        harness.catalog.meta_calls(),
        0,
        "a declared plan flag must short-circuit the TEST_META scan"
    );
    let created = &harness.runs.created()[0];
    assert!(created.resolved_exclusive);
    assert_eq!(created.exclusive_tier, ExclusiveTier::Plan);
}

/// `exclusivity.rs:145-158`: the `TEST_META` tier is the OR over the in-scope
/// files, and one destructive test makes the suite destructive.
#[tokio::test]
async fn an_exclusive_test_file_makes_the_run_exclusive_and_reports_the_test_meta_tier() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture("Smoke", &["tests/a.py", "tests/b.py"]))
                .with_meta("tests/a.py", meta_fixture("tests/a.py", &[], Some(false)))
                .with_meta("tests/b.py", meta_fixture("tests/b.py", &[], Some(true))),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(created.resolved_exclusive);
    assert_eq!(created.exclusive_tier, ExclusiveTier::TestMeta);
}

/// `exclusivity.rs:181-187` plus guide lines 38-41: an operator can run an
/// exclusive-marked suite in parallel tonight without editing test files. The
/// override also does no work on the lower tiers.
#[tokio::test]
async fn a_launch_override_of_false_beats_an_exclusive_test_file() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture("Smoke", &["tests/b.py"]))
                .with_meta("tests/b.py", meta_fixture("tests/b.py", &[], Some(true))),
        )
        .build()
        .await;

    let request = LaunchRequest {
        exclusive: Some(false),
        ..plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(!created.resolved_exclusive);
    assert_eq!(created.exclusive_tier, ExclusiveTier::Launch);
    assert_eq!(
        harness.catalog.meta_calls(),
        0,
        "a launch override must short-circuit before any file is read"
    );
}

/// The guide's "all its test files are considered *after* tag filtering"
/// (`exclusivity.rs:160-173`). With the only exclusive file excluded, nothing
/// votes at all — which is `Default`, not `TestMeta`-false.
#[tokio::test]
async fn files_dropped_by_the_exclude_filter_do_not_make_the_run_exclusive() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture("Smoke", &["tests/destructive.py"]))
                .with_meta(
                    "tests/destructive.py",
                    meta_fixture("tests/destructive.py", &["destructive"], Some(true)),
                ),
        )
        .build()
        .await;

    let request = LaunchRequest {
        exclude_tags: vec!["Destructive".to_owned()],
        ..plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(!created.resolved_exclusive);
    assert_eq!(
        created.exclusive_tier,
        ExclusiveTier::Default,
        "a filtered-out file abstains; nothing voted, so nobody had an opinion"
    );
}

/// **Superseded by review finding #9.** This used to assert the opposite of
/// what it now does: `exclusivity.rs:177-179`'s "resolution never fails a
/// launch" was, before this finding, applied to *every* `TEST_META` failure,
/// including a fault that is not "the file is absent" -- and swallowing a
/// fault the same way as an absent file is exactly the defect finding #9
/// describes. A wholly unavailable catalog during the scan is indistinguishable
/// from any other non-absent failure (both are `Internal` through this SDK),
/// so it now fails the launch closed rather than silently resolving parallel.
///
/// The claim `exclusivity.rs:177-179` makes still holds for a genuinely
/// *missing* input -- see `an_entirely_unreadable_file_set_resolves_default_not_test_meta_false`
/// and `an_unresolvable_nested_plan_contributes_nothing_and_does_not_fail_the_launch`
/// -- it just no longer extends to a fault.
#[tokio::test]
async fn a_catalog_fault_while_resolving_exclusivity_fails_the_launch() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture("Smoke", &["tests/a.py"]))
                .meta_unavailable(),
        )
        .build()
        .await;

    let err = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Catalog(_)),
        "a catalog fault while scanning TEST_META must fail the launch rather than \
         silently resolving it parallel; got {err:?}"
    );
    assert!(
        harness.runs.created().is_empty(),
        "resolution runs before the run row is created, so a resolution failure must \
         leave no row behind"
    );
}

/// `exclusivity.rs:397-398`: "No `force_sync`: admission must stay cheap - the
/// sync and the bundle build are what `dispatch` is for." (The sentence runs to
/// `:400`; `scan_test_meta`'s whole doc comment is `:394-402`.)
#[tokio::test]
async fn exclusivity_resolution_never_force_syncs() {
    let harness = Builder::new().build().await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    assert_eq!(
        harness.catalog.sync_calls(),
        0,
        "the launch path must not sync; rules 4 and 5 belong to dispatch"
    );
}

/// **Obligation: omit unreadable files, never substitute a default.**
///
/// `exclusivity::resolve_plan_tier` says it in full: an admitted file always
/// votes, so a default `FileMeta` votes `Some(false)`. If this launch path
/// passed one for an unfetchable file, the tier below would read `TestMeta`
/// rather than `Default` — the two are never interchangeable
/// (`exclusivity.rs:656-664` has an explicit legacy test forbidding the
/// collapse), and a *destructive* unreadable file would have had its vote
/// replaced by a parallel one.
///
/// This is also the state in which the ported operator warning fires
/// (`exclusivity.rs:440-448`, `unreadable > 0 && flags.is_empty()`, its rationale
/// at `:433-439`): the warning's own predicate is exactly "some file was
/// unreadable **and** the tier came out `Default`".
///
/// **This test pins the state, not the emission.** No test in this crate
/// asserts that the `warn!` fires, and nothing prevents one: `tracing-test` is
/// a dev-dependency and `service::dispatch`'s log assertions use it. The gap is
/// that the test was not written, which is what `LaunchService::resolve_exclusivity`'s
/// own doc already says about all three of these warnings.
///
/// That is a correction. This shipped as *"none can as shipped — see the
/// comment at the `warn!` site in `launch.rs`"*, forwarding to a comment that
/// does not exist; the paragraph it meant is on `resolve_exclusivity`, and it
/// had already retracted the impossibility a task earlier.
#[tokio::test]
async fn an_entirely_unreadable_file_set_resolves_default_not_test_meta_false() {
    let harness = Builder::new()
        .catalog(
            // The plan names a file the catalog has no meta for: genuinely
            // *absent*, i.e. `QaCatalogError::NotFound` through this SDK --
            // not a denial or a fault, which now fail the launch instead
            // (review finding #9).
            MockCatalog::new().with_plan(plan_fixture("Smoke", &["tests/gone.py"])),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(!created.resolved_exclusive);
    assert_eq!(
        created.exclusive_tier,
        ExclusiveTier::Default,
        "an unreadable file must abstain; a default FileMeta would vote \
         Some(false) and report TestMeta"
    );
}

/// The per-file fallback, and why it exists: qa-catalog's `get_test_meta` is
/// all-or-nothing, so a batch containing one absent file fails outright with
/// `NotFound`. The source system reads each file separately and skips only the
/// absent one (`exclusivity.rs:411-432`, the read at `:421`), so a readable
/// **destructive** file still wins.
/// Without the fallback this run would resolve parallel.
#[tokio::test]
async fn a_partially_unreadable_group_still_lets_the_readable_files_vote() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture("Smoke", &["tests/gone.py", "tests/here.py"]))
                .with_meta(
                    "tests/here.py",
                    meta_fixture("tests/here.py", &[], Some(true)),
                ),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "the readable destructive file must still make the run exclusive"
    );
    assert_eq!(created.exclusive_tier, ExclusiveTier::TestMeta);
    assert_eq!(
        harness.catalog.meta_calls(),
        3,
        "one failed batch, then one call per file"
    );
}

/// **A `Forbidden` from the catalog must not resolve an exclusive suite as
/// parallel.**
///
/// The per-file `TEST_META` fallback used to count every failure as
/// "unreadable", and an unreadable file casts no vote. So a policy that denies
/// this gear's system actor the catalog read turned a suite whose files
/// declare `exclusive: True` into a **parallel** run -- a destructive test
/// losing its platform-to-itself guarantee, silently, on a deployment whose
/// only fault is a missing grant.
///
/// Omitting a genuinely *missing* file is correct and stays
/// (`a_missing_file_is_omitted_and_the_rest_still_vote` below). This is about
/// the other failures. Review finding #9.
#[tokio::test]
async fn a_forbidden_test_meta_read_fails_the_launch_rather_than_going_parallel() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture("Smoke", &["tests/destructive.py"]))
                .with_meta(
                    "tests/destructive.py",
                    meta_fixture("tests/destructive.py", &[], Some(true)),
                )
                .deny_test_meta(),
        )
        .build()
        .await;

    let err = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "a denied catalog read must surface, not silently resolve parallel; got {err:?}"
    );
    assert!(
        harness.runs.created().is_empty(),
        "resolution runs before the run row is created, so a resolution failure must \
         leave no row behind"
    );
}

/// The half that must not regress: a genuinely missing file is still omitted,
/// and the files that *are* readable still vote.
///
/// Without this, the fix above could be "propagate everything", which would
/// break the source system's behaviour on an input it handles correctly
/// (`manager/src/services/exclusivity.rs:411-432`). This is the same fixture
/// shape as `a_partially_unreadable_group_still_lets_the_readable_files_vote`
/// above, named and pinned separately because it is the test that must stay
/// green while `a_forbidden_test_meta_read_fails_the_launch_rather_than_going_parallel`
/// turns red-to-green (review finding #9).
#[tokio::test]
async fn a_missing_file_is_omitted_and_the_rest_still_vote() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture(
                    "Smoke",
                    &["tests/gone.py", "tests/destructive.py"],
                ))
                .with_meta(
                    "tests/destructive.py",
                    meta_fixture("tests/destructive.py", &[], Some(true)),
                ),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "the readable file declared exclusive: True, so the suite must be exclusive"
    );
    assert_eq!(
        created.exclusive_tier,
        ExclusiveTier::TestMeta,
        "the vote came from TEST_META, not a plan.yaml declaration"
    );
}

/// A custom plan is scanned with **no tag filter of its own**
/// (`exclusivity.rs:552-558`), so the request's tags must not reach the scan.
#[tokio::test]
async fn a_custom_plans_scan_ignores_the_requests_tag_filter() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture(&[(REPO_ID, "tests/destructive.py")]))
                .with_meta(
                    "tests/destructive.py",
                    meta_fixture("tests/destructive.py", &["destructive"], Some(true)),
                ),
        )
        .build()
        .await;

    let request = LaunchRequest {
        exclude_tags: vec!["destructive".to_owned()],
        ..custom_plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "the request's exclude filter must not reach a custom plan's scan"
    );
    assert_eq!(
        created.exclude_tags,
        vec!["destructive".to_owned()],
        "the filter is still recorded on the run - it governs execution, not \
         the exclusivity scan"
    );
}

// ---------------------------------------------------------------------------
// Custom plans: the nested `plan.yaml` tier (`custom_plan_tier`)
// ---------------------------------------------------------------------------

/// **The input the divergence table named**, and the reason
/// `CustomPlanEntry::plan_path` exists: a custom plan composed of a plan whose
/// `plan.yaml` declares `exclusive: true`, every one of whose test files is
/// silent.
///
/// Legacy answers exclusive / tier `Plan`
/// (`manager/src/services/exclusivity.rs:549-550` then `:579-582`). Before
/// `plan_path` this port answered **parallel / tier `TestMeta`**, because every
/// file voted `Some(false)` with no plan tier to outrank them — a destructive
/// suite silently losing its platform-to-itself guarantee.
///
/// The silent metas reproduce that exact input: `Some(false)` is a *vote*, so
/// without them the pre-fix answer would have been `Default` rather than
/// `TestMeta` — a different (and less alarming) bug than the one this closes.
/// They are not what makes the test fail if the fix is removed; the tier
/// assertion does that either way.
#[tokio::test]
async fn a_nested_plans_declaration_makes_a_custom_plan_exclusive() {
    let mut nested = plan_fixture("Destructive", &["tests/a.py", "tests/b.py"]);
    nested.exclusive = Some(true);

    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[
                    (REPO_ID, "tests/a.py", Some("plans/destructive.yaml")),
                    (REPO_ID, "tests/b.py", Some("plans/destructive.yaml")),
                ]))
                .with_plan_at(REPO_ID, "plans/destructive.yaml", nested)
                // Both files silent: `Some(false)` is a vote, not an abstention.
                .with_meta("tests/a.py", meta_fixture("tests/a.py", &[], Some(false)))
                .with_meta("tests/b.py", meta_fixture("tests/b.py", &[], Some(false))),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "a nested plan declaring exclusive: true must make the custom plan exclusive"
    );
    assert_eq!(created.exclusive_tier, ExclusiveTier::Plan);
    assert_eq!(
        harness.catalog.meta_calls(),
        0,
        "the nested plan's declaration must short-circuit its own TEST_META scan \
         (exclusivity.rs:549-550)"
    );
}

/// The OR across nested plans that [`combine_nested`] is for: one plan declares
/// `false`, another `true`, and the run is exclusive at tier `Plan`
/// (`manager/src/services/exclusivity.rs:579-582`).
///
/// A flat scan over the union of files could not produce this answer at all.
#[tokio::test]
async fn a_custom_plan_ors_the_declarations_of_the_plans_it_composes() {
    let mut parallel = plan_fixture("Parallel", &["tests/a.py"]);
    parallel.exclusive = Some(false);
    let mut destructive = plan_fixture("Destructive", &["tests/b.py"]);
    destructive.exclusive = Some(true);

    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[
                    (REPO_ID, "tests/a.py", Some("plans/parallel.yaml")),
                    (REPO_ID, "tests/b.py", Some("plans/destructive.yaml")),
                ]))
                .with_plan_at(REPO_ID, "plans/parallel.yaml", parallel)
                .with_plan_at(REPO_ID, "plans/destructive.yaml", destructive),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(created.resolved_exclusive, "one destructive plan is enough");
    assert_eq!(created.exclusive_tier, ExclusiveTier::Plan);
}

/// `combine_nested`'s third branch: every nested plan was **asked and answered
/// `false`**, so the tier names `Plan` rather than collapsing to `Default`
/// (`manager/src/services/exclusivity.rs:468-488`).
///
/// The distinction is not cosmetic — `Default` means "nobody was asked".
#[tokio::test]
async fn a_nested_plan_declaring_parallel_reports_tier_plan_not_default() {
    let mut nested = plan_fixture("Parallel", &["tests/a.py"]);
    nested.exclusive = Some(false);

    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[(
                    REPO_ID,
                    "tests/a.py",
                    Some("plans/parallel.yaml"),
                )]))
                .with_plan_at(REPO_ID, "plans/parallel.yaml", nested),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(!created.resolved_exclusive);
    assert_eq!(created.exclusive_tier, ExclusiveTier::Plan);
}

/// A nested plan that declares **nothing** falls through to its own `TEST_META`
/// scan, in its own repository, with **no tag filter**
/// (`manager/src/services/exclusivity.rs:551-558`, which passes `&[], &[]`).
///
/// The request here carries an `exclude_tags` that would drop the destructive
/// file if it reached the scan, so the assertion distinguishes "the filter was
/// not threaded" from "the file happened to vote".
#[tokio::test]
async fn a_nested_plan_declaring_nothing_scans_its_own_files_without_a_tag_filter() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[(
                    REPO_ID,
                    "tests/destructive.py",
                    Some("plans/silent.yaml"),
                )]))
                // `exclusive: None` — the plan.yaml says nothing.
                .with_plan_at(
                    REPO_ID,
                    "plans/silent.yaml",
                    plan_fixture("Silent", &["tests/destructive.py"]),
                )
                .with_meta(
                    "tests/destructive.py",
                    meta_fixture("tests/destructive.py", &["destructive"], Some(true)),
                ),
        )
        .build()
        .await;

    let request = LaunchRequest {
        exclude_tags: vec!["destructive".to_owned()],
        ..custom_plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "the request's exclude filter must not reach a nested plan's scan"
    );
    assert_eq!(created.exclusive_tier, ExclusiveTier::TestMeta);
}

/// An entry naming **no** nested plan behaves exactly as it did before
/// `plan_path` existed: no plan is looked up, and the file's `TEST_META` decides.
///
/// This is the compatibility half of the change, and it is what every row stored
/// before `plan_path` decodes to. `plan_lookups` being empty is the assertion
/// that matters: a `None` must not be resolved as some default path.
#[tokio::test]
async fn an_entry_naming_no_nested_plan_resolves_from_test_meta_alone() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture(&[(REPO_ID, "tests/a.py")]))
                .with_meta("tests/a.py", meta_fixture("tests/a.py", &[], Some(true))),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .unwrap();

    assert!(
        harness.catalog.plan_lookups().is_empty(),
        "an entry with no plan_path must not trigger a plan lookup"
    );
    let created = &harness.runs.created()[0];
    assert!(created.resolved_exclusive);
    assert_eq!(created.exclusive_tier, ExclusiveTier::TestMeta);
}

/// Each nested plan is resolved and scanned in **its own** repository, because a
/// custom plan can span repositories (`exclusivity.rs:551-556`).
///
/// Keyed lookups are the point: the fixture registers the same `plan.yaml` path
/// in two repositories with opposite declarations, so a resolution that dropped
/// `repo_id` from the key would answer with whichever it found first.
#[tokio::test]
async fn each_nested_plan_is_resolved_in_its_own_repository() {
    let mut here = plan_fixture("Here", &["tests/a.py"]);
    here.exclusive = Some(false);
    let mut there = plan_fixture("There", &["tests/b.py"]);
    there.exclusive = Some(true);

    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[
                    (REPO_ID, "tests/a.py", Some("plans/p.yaml")),
                    (OTHER_REPO_ID, "tests/b.py", Some("plans/p.yaml")),
                ]))
                .with_plan_at(REPO_ID, "plans/p.yaml", here)
                .with_plan_at(OTHER_REPO_ID, "plans/p.yaml", there),
        )
        .build()
        .await;

    let request = LaunchRequest {
        // Rule 3: a multi-repository custom plan needs an explicit branch.
        branch: Some("main".to_owned()),
        ..custom_plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    assert_eq!(
        harness.catalog.plan_lookups(),
        vec![
            (REPO_ID, "plans/p.yaml".to_owned()),
            (OTHER_REPO_ID, "plans/p.yaml".to_owned()),
        ],
        "the same plan path in two repositories is two different plans"
    );
    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "the other repository's plan declared exclusive"
    );
    assert_eq!(created.exclusive_tier, ExclusiveTier::Plan);
}

/// A nested plan whose `plan.yaml` is genuinely **absent** (`NotFound`)
/// contributes nothing and **does not fail the launch**
/// (`manager/src/services/exclusivity.rs:540-546`, and the whole-function
/// contract at `:177-179`: resolution never fails a launch on a missing
/// input).
///
/// The second, resolvable plan still decides, which is what "computed from the
/// rest" means.
///
/// **This is the guard against over-propagating, for review finding #9's fix
/// round 1** (`resolve_one_nested_plan`'s `get_plan` arm, alongside
/// `a_denied_nested_plan_yaml_read_fails_the_launch_rather_than_going_parallel`
/// below): it must stay green exactly as it was before that fix, because
/// `MockCatalog::with_unresolvable_plan` answers `NotFound`, not a denial or a
/// fault, and `NotFound` is the one outcome that must still contribute
/// nothing rather than fail the launch.
#[tokio::test]
async fn an_unresolvable_nested_plan_contributes_nothing_and_does_not_fail_the_launch() {
    let mut destructive = plan_fixture("Destructive", &["tests/b.py"]);
    destructive.exclusive = Some(true);

    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[
                    (REPO_ID, "tests/a.py", Some("plans/gone.yaml")),
                    (REPO_ID, "tests/b.py", Some("plans/destructive.yaml")),
                ]))
                .with_unresolvable_plan(REPO_ID, "plans/gone.yaml")
                .with_plan_at(REPO_ID, "plans/destructive.yaml", destructive),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .expect("an unresolvable nested plan must not fail the launch");

    let created = &harness.runs.created()[0];
    assert!(created.resolved_exclusive);
    assert_eq!(created.exclusive_tier, ExclusiveTier::Plan);
    assert_eq!(
        harness.catalog.meta_calls(),
        0,
        "an unresolvable plan's files must not be scanned either - it contributes \
         nothing, rather than falling through to TEST_META"
    );
}

/// **The same missing grant, one call earlier.** Review finding #9's fix
/// round 1: `get_plan` is scoped by the same kind of grant as `get_test_meta`,
/// so a policy that denies this gear's system actor the catalog read denies a
/// custom plan's nested `plan.yaml` lookup *before* `scan_nested_plan` -- the
/// branch the original fix hardened -- is ever reached. The old `Err` arm
/// folded that into "unresolvable nested plan, contributes nothing" exactly
/// as the old `TEST_META` fallback folded a denial into "unreadable", so a
/// declared-exclusive custom plan would have resolved parallel here too, on
/// the identical operational fault: a missing grant, not a missing plan.
///
/// The counterpart that must not regress is
/// `an_unresolvable_nested_plan_contributes_nothing_and_does_not_fail_the_launch`
/// above: a genuinely absent `plan.yaml` (`NotFound`) still contributes
/// nothing and the launch still succeeds.
#[tokio::test]
async fn a_denied_nested_plan_yaml_read_fails_the_launch_rather_than_going_parallel() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[(
                    REPO_ID,
                    "tests/destructive.py",
                    Some("plans/destructive.yaml"),
                )]))
                .with_denied_plan(REPO_ID, "plans/destructive.yaml"),
        )
        .build()
        .await;

    let err = harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "a denied nested plan.yaml read must surface, not silently resolve parallel; got {err:?}"
    );
    assert!(
        harness.runs.created().is_empty(),
        "resolution runs before the run row is created, so a resolution failure must \
         leave no row behind"
    );
}

/// Every nested plan unresolvable: parallel at tier `Default`, and still no
/// error. `Default` is the honest answer — nothing was asked and answered — and
/// it is what the aggregate warning exists to make visible.
#[tokio::test]
async fn a_custom_plan_whose_every_nested_plan_is_unresolvable_resolves_default() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[(
                    REPO_ID,
                    "tests/a.py",
                    Some("plans/gone.yaml"),
                )]))
                .with_unresolvable_plan(REPO_ID, "plans/gone.yaml"),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(!created.resolved_exclusive);
    assert_eq!(created.exclusive_tier, ExclusiveTier::Default);
}

/// The launch override wins outright for a custom plan too, and legacy checks it
/// **before** it dispatches on the intent
/// (`manager/src/services/exclusivity.rs:181-187`, above the `match` at `:189`),
/// so no nested plan is read at all.
///
/// Asserted by absence on both reads: an override that still resolved nested
/// plans would give the same `exclusive` bool and a different tier, and would
/// cost a launch every nested plan's lookup.
#[tokio::test]
async fn a_launch_override_skips_a_custom_plans_nested_plans_entirely() {
    let mut nested = plan_fixture("Destructive", &["tests/a.py"]);
    nested.exclusive = Some(true);

    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[(
                    REPO_ID,
                    "tests/a.py",
                    Some("plans/destructive.yaml"),
                )]))
                .with_plan_at(REPO_ID, "plans/destructive.yaml", nested),
        )
        .build()
        .await;

    let request = LaunchRequest {
        exclusive: Some(false),
        ..custom_plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    assert!(
        harness.catalog.plan_lookups().is_empty(),
        "a launch override must not resolve any nested plan"
    );
    assert_eq!(harness.catalog.meta_calls(), 0);
    let created = &harness.runs.created()[0];
    assert!(!created.resolved_exclusive);
    assert_eq!(created.exclusive_tier, ExclusiveTier::Launch);
}

/// A nested plan whose files are **entirely unreadable** resolves `Default`, not
/// `TestMeta`-false — the Task 13 obligation, on the new path.
///
/// `exclusivity.rs:184` makes an admitted file vote `Some(false)`, so a default
/// `FileMeta` substituted for an unreadable one would report `TestMeta` and
/// parallel, replacing a possibly-destructive file's vote with a parallel one.
/// The nested path reads through the same `gather_group_meta` for exactly this
/// reason; this pins that it does.
#[tokio::test]
async fn an_entirely_unreadable_nested_plan_resolves_default_not_test_meta_false() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[(
                    REPO_ID,
                    "tests/missing.py",
                    Some("plans/silent.yaml"),
                )]))
                .with_plan_at(
                    REPO_ID,
                    "plans/silent.yaml",
                    plan_fixture("Silent", &["tests/missing.py"]),
                ),
            // No `with_meta`, so the file is unreadable.
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(!created.resolved_exclusive);
    assert_eq!(
        created.exclusive_tier,
        ExclusiveTier::Default,
        "an unreadable file must be omitted, never defaulted into a parallel vote"
    );
}

/// Two nested plans in one repository are two groups, and one declaring plan
/// does not short-circuit the other's scan.
///
/// This is the case a grouping keyed only on `repo_id` would collapse: the union
/// of both plans' files scanned once, with the declaring plan's files wrongly
/// included in the scan and its declaration lost.
#[tokio::test]
async fn two_nested_plans_in_one_repository_are_two_groups() {
    let mut declaring = plan_fixture("Declaring", &["tests/a.py"]);
    declaring.exclusive = Some(false);

    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture_nested(&[
                    (REPO_ID, "tests/a.py", Some("plans/declaring.yaml")),
                    (REPO_ID, "tests/b.py", Some("plans/silent.yaml")),
                ]))
                .with_plan_at(REPO_ID, "plans/declaring.yaml", declaring)
                .with_plan_at(
                    REPO_ID,
                    "plans/silent.yaml",
                    plan_fixture("Silent", &["tests/b.py"]),
                )
                // Only the silent plan's file is scanned, and it is destructive.
                .with_meta("tests/b.py", meta_fixture("tests/b.py", &[], Some(true))),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .unwrap();

    assert_eq!(
        harness.catalog.plan_lookups(),
        vec![
            (REPO_ID, "plans/declaring.yaml".to_owned()),
            (REPO_ID, "plans/silent.yaml".to_owned()),
        ],
        "each nested plan is resolved separately"
    );
    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "the silent plan's destructive file must still vote"
    );
    assert_eq!(
        created.exclusive_tier,
        ExclusiveTier::TestMeta,
        "combine_nested: the plan tier answered false, TEST_META answered true, \
         and the strongest source that said `true` names the tier"
    );
}

// ---------------------------------------------------------------------------
// The pytest node-id strip, which legacy does and this port did not
// ---------------------------------------------------------------------------

/// `test_file_part` must match legacy's exactly
/// (`manager/src/services/test_bundles.rs:310-315`: `value.find("::")` then
/// `&value[..idx]`), edges included, because the edges are where a "close enough"
/// port silently changes which file is read.
#[test]
fn the_node_id_strip_matches_legacys_test_file_part() {
    assert_eq!(test_file_part("tests/a.py"), "tests/a.py", "no selector");
    assert_eq!(test_file_part("tests/a.py::TestC"), "tests/a.py");
    assert_eq!(
        test_file_part("tests/a.py::TestC::test_m"),
        "tests/a.py",
        "legacy uses `find`, not `rfind`: only the FIRST `::` splits"
    );
    assert_eq!(
        test_file_part("::TestC"),
        "",
        "a leading `::` yields the empty string, not the input - which then fails \
         to resolve and is counted unreadable, exactly as legacy's \
         `read_to_string` on a directory does"
    );
    assert_eq!(
        test_file_part(" tests/a.py ::C"),
        " tests/a.py ",
        "nothing is trimmed here, because test_bundles.rs:310-315 does not trim"
    );
}

/// **The single-test run name is legacy's four steps, `normalize_test_path`
/// included — it was three.**
///
/// `test_name_stem`'s doc claimed to match `manager/src/services/argo.rs:723-728`
/// *"so do not 'fix' the name path"*, while omitting the `normalize_test_path` call
/// at `:723` entirely. Found by running both implementations side by side, not by
/// re-reading the citation:
///
/// ```text
/// "tests\\test_b.py"              port "tests\\test-b"               legacy "b"
/// "ui\\specs\\test_login.spec.ts" port "ui\\specs\\test-login.spec"  legacy "login.spec"
/// ```
///
/// On a non-Windows host `\` is not a separator, so `file_stem` returned the whole
/// string and the directory part landed inside the run name. Confined to the name
/// (and thence the `LIKE '{prefix}-%'` sequence), never to which tests ran — but
/// legacy's `replace('\\', "/")` exists because such paths occur.
#[test]
fn the_single_test_name_stem_ports_all_four_of_legacys_steps() {
    // The plain case, unchanged by the fix.
    assert_eq!(test_name_stem("tests/test_b.py"), "b");
    // `normalize_test_path`'s backslash step - the one that was missing.
    assert_eq!(
        test_name_stem("tests\\test_b.py"),
        "b",
        "argo.rs:2441-2446 replaces backslashes before `file_stem` sees the path"
    );
    assert_eq!(
        test_name_stem("ui\\specs\\test_login.spec.ts"),
        "login.spec"
    );
    // Its trim and its two leading-prefix strips.
    assert_eq!(test_name_stem("  tests/test_b.py  "), "b");
    assert_eq!(test_name_stem("./tests/test_b.py"), "b");
    assert_eq!(test_name_stem("/tests/test_b.py"), "b");
    // The `_`->`-` step and the `test-` strip still compose in legacy's order:
    // strip AFTER the replace, or `test_foo.py` would keep its prefix.
    assert_eq!(test_name_stem("tests/test_foo_bar.py"), "foo-bar");
    // A node id is NOT stripped for the name (see `test_file_part`).
    assert_eq!(test_name_stem("tests/test_b.py::TestB::test_x"), "b");
}

/// `normalize_test_path`'s step order is legacy's and it is observable: the
/// backslash replace runs **last**, so a leading `.\` is not yet `./` when the
/// `./` strip runs and therefore survives it. Pinned so nobody "tidies" the order.
#[test]
fn the_path_normalizer_keeps_legacys_step_order() {
    assert_eq!(normalize_test_path("  tests\\a.py "), "tests/a.py");
    assert_eq!(normalize_test_path("./tests/a.py"), "tests/a.py");
    assert_eq!(normalize_test_path("/tests/a.py"), "tests/a.py");
    assert_eq!(
        normalize_test_path(".\\tests\\a.py"),
        "./tests/a.py",
        "the replace runs after the `./` strip, so a leading `.\\` survives it - \
         reordering the steps would change this"
    );
}

/// **The strip is load-bearing in the dangerous direction.**
///
/// Legacy strips a node selector before reading `TEST_META`, and says why:
/// *"Strip a `::Class::method` selection: `TEST_META` lives in the file"*
/// (`manager/src/services/exclusivity.rs:417-418`, calling `test_file_part`).
/// Unstripped, the path is not a file, so qa-catalog's `get_test_meta` answers
/// `FileNotFound` (`qa-catalog/src/domain/service/plans.rs:225-226` via
/// `resolve_under_root`'s `canonicalize`, `:371-383`) — the selector-carrying file
/// would be counted **unreadable**, its vote discarded, and this destructive test
/// would resolve **parallel**, losing its platform-to-itself guarantee.
#[tokio::test]
async fn a_class_method_selector_is_stripped_before_the_test_meta_read() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture(
                    "Upgrade",
                    &["tests/test_upgrade.py::TestUpgrade::test_destroys"],
                ))
                // The catalog only knows the file, which is the whole point.
                .with_meta(
                    "tests/test_upgrade.py",
                    meta_fixture("tests/test_upgrade.py", &[], Some(true)),
                ),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "the selector-stripped file declares `exclusive: True` and must win; \
         without the strip it is unreadable and the run resolves parallel"
    );
    assert_eq!(created.exclusive_tier, ExclusiveTier::TestMeta);
    assert_eq!(
        harness.catalog.meta_calls(),
        1,
        "the stripped path resolves on the first batch, so the per-file fallback \
         is never entered"
    );
}

/// The same strip on a single-test target, where the selector is caller-supplied
/// rather than read out of `plan.yaml`.
///
/// The name is asserted too, because it must **not** be stripped: legacy derives a
/// single test's name from the unstripped node id (`argo.rs:723-728`, whose
/// `normalize_test_path` at `:2441-2446` does not touch `::`). For this node-id
/// shape `Path::file_stem` yields `test_b` either way, so this assertion cannot by
/// itself distinguish the two policies — the claim about the name rests on reading
/// `normalize_test_path`, which `test_file_part`'s doc says plainly.
#[tokio::test]
async fn a_single_test_selector_is_stripped_for_the_scan() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture("Smoke", &["tests/test_b.py"]))
                .with_meta(
                    "tests/test_b.py",
                    meta_fixture("tests/test_b.py", &[], Some(true)),
                ),
        )
        .build()
        .await;

    let request = LaunchRequest {
        target: RunTarget::Test {
            repo_id: REPO_ID,
            path: "tests/plan.yaml".to_owned(),
            test_file: "tests/test_b.py::TestB::test_x".to_owned(),
        },
        ..plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "the selected class lives in an exclusive file, and the file is what \
         TEST_META is read from"
    );
    assert_eq!(created.exclusive_tier, ExclusiveTier::TestMeta);
    assert_eq!(created.name, "b-1");
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_reserved_parameter_name_fails_the_launch_before_any_catalog_call() {
    let harness = Builder::new().build().await;
    let request = LaunchRequest {
        parameters: vec![param("TEST_FILES", "a.py")],
        platform_id: Some(PLATFORM_ID),
        ..plan_request()
    };

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .expect_err("a reserved parameter name is rejected");

    assert!(
        matches!(error, DomainError::InvalidParameters(_)),
        "got {error:?}"
    );
    assert!(
        harness.catalog.branches_seen().is_empty(),
        "validation must precede every catalog read"
    );
    assert!(
        harness.environments.lookups().is_empty(),
        "validation must precede the platform read too"
    );
}

/// A re-run replays a stored parameter set, and it is validated again rather
/// than trusted: the reserved list can grow between the original launch and the
/// replay.
#[tokio::test]
async fn a_replayed_parameter_set_is_re_validated() {
    let harness = Builder::new().build().await;
    let replayed = vec![param("APP_VERSION", "9.9")];
    let request = LaunchRequest {
        parameters: replayed,
        ..plan_request()
    };

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .expect_err("a replayed set is re-validated, not trusted");
    assert!(matches!(error, DomainError::InvalidParameters(_)));
}

/// **The normalized list is what gets persisted**, never the request's raw one.
/// Task 14 assembles the environment from the *stored* parameters, so a raw
/// `"  FOO  "` would become a variable literally named `"  FOO  "` and a
/// fully-blank editor row would become `""=""` - on the first run, with no
/// replay needed.
#[tokio::test]
async fn the_normalized_parameter_list_is_what_gets_persisted() {
    let harness = Builder::new().build().await;
    let request = LaunchRequest {
        parameters: vec![
            param("  FOO  ", "  spaced value  "),
            // Blank in both fields: dropped by `normalize`.
            param("", ""),
        ],
        ..plan_request()
    };

    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert_eq!(created.parameters.len(), 1, "the blank row is dropped");
    assert_eq!(created.parameters[0].name, "FOO", "the name is trimmed");
    // The value is NOT trimmed. `normalize_run_parameters`
    // (`manager/src/routes/settings.rs:183-195`) trims `name` at `:189` and
    // carries `value` through bare at `:190`, and the blank-row filter at `:193`
    // requires *both* to be empty.
    //
    // **The behaviour is ported; no rationale is, because legacy states none.**
    // Its doc comment (`:180-182`) justifies exactly three things - name
    // trimming, dropping fully-blank editor rows, and forcing `secure = false` -
    // and says nothing about values. An earlier version of this assertion claimed
    // "a padded value can be meaningful"; that reason appears nowhere in the
    // source system (`grep -rn "padded" manager/src/` is zero hits) and was
    // invented here.
    assert_eq!(
        created.parameters[0].value, "  spaced value  ",
        "the value is carried through untrimmed, as settings.rs:190 does"
    );
}

// ---------------------------------------------------------------------------
// Timeout
// ---------------------------------------------------------------------------

/// The clamp through the whole launch, which is where it matters: the run row must
/// carry a deadline the sweep can act on.
#[tokio::test]
async fn an_unbounded_caller_timeout_still_leaves_the_run_reclaimable() {
    let harness = Builder::new().build().await;
    let request = LaunchRequest {
        timeout_seconds: Some(u64::MAX),
        ..plan_request()
    };
    let before = OffsetDateTime::now_utc();

    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let ceiling = time::Duration::seconds(saturating_i64(MAX_LAUNCH_TIMEOUT_SECONDS));
    let deadline = harness.runs.created()[0].timeout_at.expect(
        "a NULL timeout_at is excluded from list_timeout_candidates, so the run \
         could never be reclaimed and an exclusive one would hold the platform \
         lease forever",
    );
    assert!(deadline >= before + ceiling);
    assert!(deadline <= OffsetDateTime::now_utc() + ceiling);
}

/// The floor through the whole launch.
#[tokio::test]
async fn a_zero_caller_timeout_does_not_expire_the_run_on_the_tick_it_launched() {
    let harness = Builder::new().build().await;
    let request = LaunchRequest {
        timeout_seconds: Some(0),
        ..plan_request()
    };
    let before = OffsetDateTime::now_utc();

    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let deadline = harness.runs.created()[0].timeout_at.unwrap();
    assert!(
        deadline > before,
        "a zero override used to make the deadline the launch instant itself, so \
         the first sweep tick cancelled the run"
    );
}

#[tokio::test]
async fn the_resolved_timeout_becomes_a_deadline_on_the_run() {
    let harness = Builder::new().build().await;
    let request = LaunchRequest {
        timeout_seconds: Some(120),
        ..plan_request()
    };
    let before = OffsetDateTime::now_utc();

    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let deadline = harness.runs.created()[0]
        .timeout_at
        .expect("a resolved timeout becomes a control-plane deadline");
    assert!(deadline >= before + time::Duration::seconds(120));
    assert!(deadline <= OffsetDateTime::now_utc() + time::Duration::seconds(120));
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_admissible_launch_returns_started() {
    let harness = Builder::new()
        .admitter(RecordingAdmitter::answering(Admission::Dispatch {
            queue_id: QUEUE_ID,
        }))
        .build()
        .await;

    let outcome = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    let LaunchOutcome::Started { run } = outcome else {
        panic!("an admitted launch answers 200 with the run: {outcome:?}");
    };
    assert_eq!(
        harness.dispatcher.calls(),
        vec![(run.id, Some(QUEUE_ID))],
        "the queue row's id travels to the dispatcher so it can be marked running"
    );
    assert_eq!(
        run.state,
        RunState::Running,
        "the reported run is re-read after dispatch, not the row as inserted"
    );
    assert_eq!(run.execution_ref.as_deref(), Some("mock-execution-1"));
    assert_eq!(
        harness.runs.transitions(),
        vec![(RunState::Created, RunState::Dispatching)],
    );
}

#[tokio::test]
async fn a_blocked_launch_returns_queued_with_its_queue_id() {
    let harness = Builder::new()
        .admitter(RecordingAdmitter::answering(Admission::Queued {
            queue_id: QUEUE_ID,
        }))
        .environments(MockEnvironments::with_platform(
            OWNER_TENANT,
            platform_fixture(None, None),
        ))
        .build()
        .await;

    let request = LaunchRequest {
        platform_id: Some(PLATFORM_ID),
        ..plan_request()
    };
    let outcome = harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let LaunchOutcome::Queued { run_id, queue_id } = outcome else {
        panic!("a blocked launch answers 202: {outcome:?}");
    };
    assert_eq!(queue_id, QUEUE_ID);
    assert!(
        harness.dispatcher.calls().is_empty(),
        "a queued run is started by the dispatcher tick, not inline"
    );
    assert_eq!(
        harness.runs.transitions(),
        vec![(RunState::Created, RunState::Queued)]
    );
    assert_eq!(harness.admitter.admitted().unwrap().id, run_id);
}

/// Guide line 76 and `manager/src/services/run_dispatcher.rs:103-106`: a run
/// with no platform is never queued and never counts as occupancy.
#[tokio::test]
async fn a_platformless_launch_is_never_queued() {
    let harness = Builder::new()
        .admitter(RecordingAdmitter::answering(Admission::Unqueued))
        .build()
        .await;

    let outcome = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    let LaunchOutcome::Started { run } = outcome else {
        panic!("an unqueued launch dispatches inline: {outcome:?}");
    };
    assert_eq!(
        harness.dispatcher.calls(),
        vec![(run.id, None)],
        "no queue row exists, so none is named"
    );
    assert!(
        harness.environments.lookups().is_empty(),
        "no platform means no platform read"
    );
}

/// The frozen guide enumerates **exactly two** 429 causes and each must name
/// its own knob (guide lines 74, 240-242).
#[tokio::test]
async fn a_full_queue_surfaces_the_queue_full_error_naming_the_limit() {
    let harness = Builder::new()
        .admitter(RecordingAdmitter::refusing(DomainError::QueueFull {
            platform_id: PLATFORM_ID,
            queued: 20,
            limit: 20,
        }))
        .build()
        .await;

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect_err("a full queue refuses the launch");

    assert!(matches!(error, DomainError::QueueFull { limit: 20, .. }));
    let message = error.to_string();
    assert!(message.contains("20 of 20 slots"), "{message}");
    assert!(message.contains(&PLATFORM_ID.to_string()), "{message}");
}

#[tokio::test]
async fn the_concurrency_limit_surfaces_its_own_error_naming_its_own_limit() {
    let harness = Builder::new()
        .admitter(RecordingAdmitter::refusing(DomainError::ConcurrencyLimit {
            limit: 50,
        }))
        .build()
        .await;

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect_err("the cluster cap refuses the launch");

    assert!(matches!(error, DomainError::ConcurrencyLimit { limit: 50 }));
    assert!(
        error
            .to_string()
            .contains("max concurrent runs limit reached (50)"),
        "the message must name the knob: {error}"
    );
    assert!(
        !error.to_string().contains("queue"),
        "the two 429 causes must not share a message: {error}"
    );
}

/// The tier is persisted, not just the boolean: the source system's own
/// rationale for computing it is so an operator can always tell *why* a run
/// became exclusive (`manager/src/services/exclusivity.rs:13-14`), which a log
/// line cannot answer once the log has rotated.
#[tokio::test]
async fn the_run_row_records_the_resolved_exclusivity_tier() {
    let mut plan = plan_fixture("Smoke", &["tests/a.py"]);
    plan.exclusive = Some(false);
    let harness = Builder::new()
        .catalog(MockCatalog::new().with_plan(plan))
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(!created.resolved_exclusive);
    assert_eq!(
        created.exclusive_tier,
        ExclusiveTier::Plan,
        "a declared plan.yaml `false` is an opinion, and reporting it as \
         `default` is what exclusivity.rs:656-664 forbids"
    );
    // The same value reaches the admitter, which is what decides the queue row.
    assert_eq!(
        harness.admitter.admitted().unwrap().exclusive_tier,
        ExclusiveTier::Plan
    );
}

/// A refused admission must not strand a row in `created`.
#[tokio::test]
async fn a_refused_admission_retires_the_run() {
    let harness = Builder::new()
        .admitter(RecordingAdmitter::refusing(DomainError::QueueFull {
            platform_id: PLATFORM_ID,
            queued: 20,
            limit: 20,
        }))
        .build()
        .await;

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect_err("a full queue refuses the launch");
    let error_text = error.to_string();

    let stored = harness.runs.stored();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].state,
        RunState::Canceled,
        "the only terminal state `created` can legally reach"
    );
    assert_eq!(stored[0].error.as_deref(), Some(error_text.as_str()));
    assert!(stored[0].finished_at.is_some());
    assert_eq!(
        stored[0].started_at, None,
        "a run that never started has no start instant"
    );
}

/// **The redacting direction: an admission failure the caller is not entitled to
/// must not travel.**
///
/// `abandon` writes its `error` into `qa_runs.error`, served verbatim by
/// `GET /runs/{id}`. `Admitter::admit`'s own `# Errors` doc says the non-429
/// case is "a persistence or lease failure", i.e. `DomainError::Database`,
/// which is `From<toolkit_db::DbError>` and carries raw driver text. Observed
/// before the fix: index name, column names and key values, on the row.
#[tokio::test]
async fn a_database_admission_failure_does_not_reach_the_run_row() {
    const RAW: &str = "error returned from database: duplicate key value violates unique \
                       constraint \"idx_qa_run_queue_tenant_run\" DETAIL: Key (tenant_id, \
                       run_id)=(0a11, 0b02) already exists";
    let harness = Builder::new()
        .admitter(RecordingAdmitter::refusing(DomainError::Database(
            RAW.to_owned(),
        )))
        .build()
        .await;

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect_err("a persistence failure refuses the launch");

    // **The error returned to the caller does not change** - the service layer
    // still reports the real cause upward, and the REST layer decides what to
    // render. Only what is *persisted* is redacted.
    assert!(
        error.to_string().contains("idx_qa_run_queue_tenant_run"),
        "the returned error must be untouched: {error}"
    );

    let stored = harness.runs.stored();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].state,
        RunState::Canceled,
        "the row is still retired"
    );
    let recorded = stored[0]
        .error
        .as_deref()
        .expect("a retired run still records that it closed");
    for leak in [
        "idx_qa_run_queue_tenant_run",
        "duplicate key",
        "tenant_id",
        "0a11",
    ] {
        assert!(
            !recorded.contains(leak),
            "run.error leaked {leak:?}: {recorded:?}"
        );
    }
}

/// **A queued run with no platform is not representable, and must not be papered
/// over with the nil sentinel.**
///
/// `Admission::Queued` means the admitter wrote a `qa_run_queue` row, whose
/// `platform_id` is not nullable. This case used to be papered over with
/// `run.platform_id.unwrap_or_else(Uuid::nil)` under a comment asserting the
/// fallback was unreachable *because of what Task 14's admitter does*. Nil is
/// this subsystem's platform-root / cross-tenant sentinel, so a consumer
/// aggregating queue depth by platform would have booked the run against
/// platform-root.
#[tokio::test]
async fn queueing_a_platformless_run_is_an_internal_error_not_a_nil_platform_event() {
    let harness = Builder::new()
        // `plan_request()` carries `platform_id: None`, so this admitter answers
        // an outcome that cannot be described.
        .admitter(RecordingAdmitter::answering(Admission::Queued {
            queue_id: QUEUE_ID,
        }))
        .build()
        .await;

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect_err("a queue row without a platform is not representable");

    assert!(matches!(error, DomainError::Internal(_)), "got {error:?}");
    assert!(
        harness.runs.transitions().is_empty(),
        "the guard fires before the state is recorded, so nothing claims the run \
         was queued"
    );
    assert!(
        harness.dispatcher.calls().is_empty(),
        "and nothing is dispatched"
    );
}

/// **The nil-tenant denial, which nothing exercised until now.**
///
/// `domain::system_actor` and `test_support`'s `permissive_response` both state
/// that a nil-tenant context yields zero constraints and therefore "fails
/// compilation and lands as `DomainError::Forbidden`" — and that claim is what
/// makes the system-actor module's enumeration/write split safe, because it is why
/// a nil-tenant context cannot write. The mechanism is real:
/// `PolicyEnforcer::access_scope` requires constraints by default, an empty set is
/// `EnforcerError::CompileFailed(ConstraintCompileError::ConstraintsRequiredButAbsent)`
/// -- named explicitly because `CompileFailed` has a second shape,
/// `AllConstraintsFailed`, that `DomainError`'s `From` maps to `Internal`
/// instead (the deny/fault split this plan made; see that `impl From` in
/// `domain/error.rs`) -- and this shape is the one `From` maps to `Forbidden`.
/// But `ctx(Uuid::nil())` appeared in no shipped test, so a load-bearing claim
/// rested entirely on reading two other crates.
///
/// The cross-gear catalog reads *do* happen first — they are authorized on the far
/// side and the double here is permissive — so what this pins is that nothing is
/// **written**.
#[tokio::test]
async fn a_nil_tenant_context_is_denied_and_writes_nothing() {
    let harness = Builder::new().build().await;

    let error = harness
        .service
        .launch(&ctx(Uuid::nil()), plan_request())
        .await
        .expect_err("a nil-tenant context compiles no constraints and is denied");

    assert!(matches!(error, DomainError::Forbidden), "got {error:?}");
    assert!(
        harness.runs.created().is_empty(),
        "a denied launch must not create a run row"
    );
    assert!(
        harness.runs.stored().is_empty(),
        "and must leave nothing behind"
    );
    assert!(
        harness.dispatcher.calls().is_empty(),
        "and must not reach the execution plane"
    );
    // Pins *where* the denial came from, so this is not a test that would pass on
    // any failure at all: resolution ran to completion against the permissive
    // cross-gear doubles, and the refusal is this gear's own PEP failing to
    // compile an empty constraint set at the run insert.
    assert!(
        !harness.catalog.branches_seen().is_empty(),
        "the catalog was consulted, so the denial is the PEP's and not a \
         short-circuit somewhere earlier"
    );
}

/// The launch path must not submit anything to the executor when the platform
/// belongs to another tenant. `qa_environment_leases` (renamed from
/// `qa_platform_leases`) is keyed on a bare `platform_id` and is **not**
/// tenant-partitioned, so a run row carrying a
/// foreign platform drives its dispatcher to take the *global* lease on it.
#[tokio::test]
async fn a_platform_owned_by_another_tenant_fails_the_launch() {
    let harness = Builder::new()
        .environments(MockEnvironments::with_platform(
            OTHER_TENANT,
            platform_fixture(None, None),
        ))
        .build()
        .await;

    let request = LaunchRequest {
        platform_id: Some(PLATFORM_ID),
        ..plan_request()
    };
    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .expect_err("a foreign platform must not reach a run row");

    assert!(
        matches!(error, DomainError::Environments(_)),
        "got {error:?}"
    );
    assert!(
        harness.runs.created().is_empty(),
        "nothing may be persisted carrying another tenant's platform id"
    );
}

/// The platform's observed version and build are **snapshotted at launch**, so
/// a later platform upgrade cannot change a queued run's or a re-run's
/// `APP_VERSION`.
///
/// Where legacy does it, in three steps rather than one: `manager/src/routes/runs.rs:579-587`
/// *reads* them out of `get_platform_execution_context`, `:594` renames
/// `platform_version` to `app_version`, and `:768-769` is where they are actually
/// stamped onto the submission (`app_version.as_deref(), app_build.as_deref()`
/// passed to `submit_workflow`). Columns: `manager/migrations/001_initial.sql:56`,
/// `:149`.
#[tokio::test]
async fn the_platforms_version_and_build_are_snapshotted_onto_the_run() {
    let harness = Builder::new()
        .environments(MockEnvironments::with_platform(
            OWNER_TENANT,
            platform_fixture(Some("9.4.1"), Some("build-77")),
        ))
        .build()
        .await;

    let request = LaunchRequest {
        platform_id: Some(PLATFORM_ID),
        ..plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert_eq!(created.app_version.as_deref(), Some("9.4.1"));
    assert_eq!(created.app_build.as_deref(), Some("build-77"));
    assert_eq!(harness.environments.lookups(), vec![PLATFORM_ID]);
}

// ---------------------------------------------------------------------------
// Naming, and the uniqueness constraint that makes the sequence work
// ---------------------------------------------------------------------------

/// `naming::name_base` takes `default_base` **unslugified**
/// (`manager/src/services/argo.rs:2486-2488`), so it reaches the run name and
/// from there a `LIKE '{prefix}-%'` pattern in which `_` is a single-character
/// wildcard. `RunKind::CustomPlan.as_str()` is `"custom_plan"` and must never be
/// passed.
#[test]
fn every_default_name_base_is_slug_safe_and_free_of_underscores() {
    for kind in [RunKind::Plan, RunKind::Test, RunKind::CustomPlan] {
        let base = default_name_base(kind);
        assert!(!base.is_empty(), "{kind:?}");
        assert!(
            base.chars().all(|c| c.is_ascii_lowercase()),
            "{kind:?} yielded {base:?}, which is not an ASCII slug"
        );
        assert!(
            !base.contains('_'),
            "{kind:?} yielded {base:?}; `_` is a LIKE wildcard"
        );
    }
    assert_ne!(
        default_name_base(RunKind::CustomPlan),
        RunKind::CustomPlan.as_str(),
        "the run kind's own spelling is `custom_plan` and is exactly what must \
         not be used here"
    );
}

/// `manager/src/services/argo.rs:416-422` recomputes the number **inside** a
/// three-attempt loop and `:677-686` retries on the create's 409. The number is
/// advisory; the unique index decides.
#[tokio::test]
async fn a_name_collision_recomputes_the_sequence_and_retries() {
    let harness = Builder::new()
        // The winner of the race stored `smoke-tests-1` a moment ago.
        .runs(MockRunsRepository::empty().colliding_on("smoke-tests-1"))
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect("a bounded retry recovers from a name collision");

    let created = harness.runs.created();
    assert_eq!(created.len(), 1, "only the successful attempt is recorded");
    assert_eq!(
        created[0].name, "smoke-tests-2",
        "the retry re-reads the names and moves past the row that won"
    );
}

/// Bounded, not a loop: an unbounded retry turns a genuinely stuck name into a
/// hang (`domain::naming`, composition obligation 2).
#[tokio::test]
async fn the_name_retry_is_bounded_and_gives_up() {
    let harness = Builder::new()
        .runs(
            MockRunsRepository::empty()
                .colliding_on("smoke-tests-1")
                .colliding_on("smoke-tests-2")
                .colliding_on("smoke-tests-3"),
        )
        .build()
        .await;

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect_err("three collisions exhaust the bounded retry");

    assert!(
        matches!(error, DomainError::RunNameExists { .. }),
        "got {error:?}"
    );
}

/// A run name is `{prefix}-{n}` and the sequence continues from the highest
/// existing name (`manager/src/services/run_history.rs:470-484`).
#[tokio::test]
async fn the_run_name_continues_the_prefixs_sequence() {
    let harness = Builder::new()
        .runs(
            MockRunsRepository::empty()
                .with_existing(OWNER_TENANT, "smoke-tests-4")
                // A longer prefix that merely starts with this one must not
                // bump this prefix's counter.
                .with_existing(OWNER_TENANT, "smoke-tests-slow-9")
                // Another tenant's run is invisible to a tenant-scoped `list`,
                // so it cannot influence this sequence either.
                .with_existing(OTHER_TENANT, "smoke-tests-99"),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();

    assert_eq!(harness.runs.created()[0].name, "smoke-tests-5");
}

/// **The rule `OwnedRunId` places on every double, exercised.** A token proves
/// only that *some* `RunsRepository::get` answered `Some` under this scope, so a
/// double whose `get` ignored the scope would mint tokens freely and make every
/// ownership test in Tasks 14 and 15 vacuous.
#[tokio::test]
async fn a_foreign_tenant_cannot_resolve_an_owned_run_id() {
    let harness = Builder::new().build().await;
    let outcome = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .unwrap();
    let run_id = outcome.run_id();

    let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
    let db = test_db_provider().await;
    let conn = db.conn().unwrap();

    let owner_scope = enforcer
        .access_scope(
            &ctx(OWNER_TENANT),
            &resources::RUN,
            actions::GET,
            Some(run_id),
        )
        .await
        .unwrap();
    let owned = harness
        .runs
        .resolve_owned(&conn, &owner_scope, run_id)
        .await
        .expect("the owning tenant resolves its own run");
    assert_eq!(owned.get(), run_id);

    let foreign_scope = enforcer
        .access_scope(
            &ctx(OTHER_TENANT),
            &resources::RUN,
            actions::GET,
            Some(run_id),
        )
        .await
        .unwrap();
    let error = harness
        .runs
        .resolve_owned(&conn, &foreign_scope, run_id)
        .await
        .expect_err("a foreign tenant must not mint a token");
    assert!(
        matches!(error, DomainError::RunNotFound { id } if id == run_id),
        "absent and foreign must be indistinguishable: {error:?}"
    );
}

/// A single-test run scans only its own file, with no tag filter
/// (`manager/src/services/exclusivity.rs:207-224`, which passes `&[], &[]`), and
/// names itself after the test rather than the plan
/// (`manager/src/services/argo.rs:722-737`).
#[tokio::test]
async fn a_single_test_run_scans_only_its_own_file_and_is_named_after_it() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_plan(plan_fixture(
                    "Smoke",
                    &["tests/test_a.py", "tests/test_b.py"],
                ))
                .with_meta(
                    "tests/test_b.py",
                    meta_fixture("tests/test_b.py", &["destructive"], Some(true)),
                ),
        )
        .build()
        .await;

    let request = LaunchRequest {
        target: RunTarget::Test {
            repo_id: REPO_ID,
            path: "tests/plan.yaml".to_owned(),
            test_file: "tests/test_b.py".to_owned(),
        },
        exclude_tags: vec!["destructive".to_owned()],
        ..plan_request()
    };
    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let created = &harness.runs.created()[0];
    assert!(
        created.resolved_exclusive,
        "no tag filter applies to a single-test scan, so the exclude must not \
         drop the file"
    );
    assert_eq!(created.exclusive_tier, ExclusiveTier::TestMeta);
    assert_eq!(
        created.name, "b-1",
        "argo.rs:725-733: file stem, `_`->`-`, then a leading `test-` stripped"
    );
}

/// `manager/src/services/argo.rs:1046-1047` names a custom-plan run
/// `custom-{plan.id}-{n}` and never calls `workflow_name_base`.
#[tokio::test]
async fn a_custom_plan_run_is_named_after_the_plans_id() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                .with_custom_plan(custom_plan_fixture(&[(REPO_ID, "tests/a.py")]))
                .with_meta("tests/a.py", meta_fixture("tests/a.py", &[], None)),
        )
        .build()
        .await;

    harness
        .service
        .launch(&ctx(OWNER_TENANT), custom_plan_request())
        .await
        .unwrap();

    assert_eq!(
        harness.runs.created()[0].name,
        format!("custom-{CUSTOM_PLAN_ID}-1")
    );
}

/// A dispatch failure propagates. Task 14 owns releasing the claim and moving
/// the run to a terminal state before it returns, so this path must not
/// second-guess it — but it must not answer 200 either.
#[tokio::test]
async fn a_failed_inline_dispatch_fails_the_launch() {
    let harness = Builder::new()
        .dispatcher(RecordingDispatcher::failing("executor refused"))
        .build()
        .await;

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect_err("a failed submit must not answer 200");

    assert!(
        matches!(error, DomainError::ExecutorFailed(ref reason) if reason == "executor refused"),
        "got {error:?}"
    );
    assert_eq!(
        harness.runs.transitions(),
        vec![(RunState::Created, RunState::Dispatching)],
        "the run reached `dispatching` before the submit was attempted"
    );
}

/// The configured default is threaded from [`ServiceDeps`] through the service
/// to [`resolve_timeout_seconds`]. The pure tests above pin the arithmetic; this
/// one pins the wiring, which they cannot see.
///
/// **Driven through `RunKind::Test`, deliberately.** It used to use `RunKind::Plan`
/// with a fixture whose `timeout_seconds` was forced to `None` — a state no
/// production input produces, because qa-catalog always emits `Some` for a
/// discovered plan (see `the_configured_default_is_used_when_the_plan_declares_none`).
/// A single test is legacy's *actual* chain to the configured default
/// (`manager/src/services/argo.rs:746`, `self.default_timeout_seconds(1800)`, with
/// the plan's own value deliberately not consulted), so this now pins the same
/// wiring on a path a real launch takes — and, as a bonus, pins that the plan's
/// declared 300 is ignored rather than winning.
#[tokio::test]
async fn the_configured_default_reaches_the_run_deadline() {
    let harness = Builder::new()
        .catalog(
            MockCatalog::new()
                // Production shape: the plan declares a timeout, and a single-test
                // run must ignore it.
                .with_plan(plan_fixture("Smoke", &["tests/a.py"]))
                .with_meta("tests/a.py", meta_fixture("tests/a.py", &[], None)),
        )
        .default_timeout(4242)
        .build()
        .await;
    let request = LaunchRequest {
        target: RunTarget::Test {
            repo_id: REPO_ID,
            path: "tests/plan.yaml".to_owned(),
            test_file: "tests/a.py".to_owned(),
        },
        ..plan_request()
    };
    let before = OffsetDateTime::now_utc();

    harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .unwrap();

    let deadline = harness.runs.created()[0].timeout_at.unwrap();
    assert!(
        deadline >= before + time::Duration::seconds(4242),
        "the configured default must reach the deadline, not the plan's 300"
    );
    assert!(deadline <= OffsetDateTime::now_utc() + time::Duration::seconds(4242));
}

/// The DI container wires up: `AppServices::new` builds one `PolicyEnforcer`
/// and hands it to the launch service, and the seam implementations arrive
/// through [`ServiceDeps`] rather than being constructed here — which is what
/// lets Task 14 supply the real ones without touching this file.
#[tokio::test]
async fn the_di_container_assembles_a_working_launch_service() {
    let runs = Arc::new(MockRunsRepository::empty());
    let catalog = Arc::new(
        MockCatalog::new()
            .with_plan(plan_fixture("Smoke Tests", &["tests/a.py"]))
            .with_meta("tests/a.py", meta_fixture("tests/a.py", &[], None)),
    );
    let services = AppServices::new(
        Arc::clone(&runs),
        // Task 14 gave the container a queue repository. This test drives the
        // launch path through the two seam overrides below, so nothing ever
        // reaches the queue — the real ORM repository is passed because it is a
        // stateless unit struct and a second double would only assert that it is
        // never called.
        Arc::new(crate::infra::storage::OrmQueueRepository),
        // Task 19's schedules repository, passed for the same reason as the
        // queue one above: this test never reaches it.
        Arc::new(crate::infra::storage::OrmSchedulesRepository),
        ServiceDeps {
            db: test_db_provider().await,
            authz: Arc::new(PermissiveAuthZ),
            catalog: catalog as Arc<dyn QaCatalogClientV1>,
            environments: Arc::new(MockEnvironments::empty()),
            // The launch path resolves no plugin: `params::validate` runs
            // against the platform floor before any I/O, and the product's own
            // reserved names are `build_spec`'s to union. Present because the
            // container requires it, never called by these tests.
            product_plugins: Arc::new(
                crate::domain::service::admission::tests::fakes::FakeProductPlugins::default(),
            ),
            executor: Arc::new(crate::infra::executor::mock::MockRunExecutor::new()),
            // Task 15 added result ingestion to the container; the launch path
            // never reaches it, so the real broadcaster is passed rather than a
            // double that would only assert it is never called.
            logs: Arc::new(crate::infra::logs::RunLogBroadcaster::default()),
            archive: Arc::new(NullLogArchive) as Arc<dyn LogArchive>,
            // `Some(..)`: the launch contract is exercised against scripted
            // halves, not against the concurrency core. `None` would wire the
            // real admission and dispatch services — see `ServiceDeps::admitter`.
            admitter: Some(Arc::new(RecordingAdmitter::answering(Admission::Unqueued))),
            dispatcher: Some(Arc::new(RecordingDispatcher::starting(Arc::clone(&runs)))),
            // The launch path never attaches an observer - that is the
            // dispatcher tick's pass - so the real watcher is wired rather than
            // a double that would only assert it is never called.
            watcher: None,
            default_timeout_seconds: 900,
            limits: crate::domain::service::QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            },
            orphan_timeout_seconds: 600,
        },
    );

    let outcome = services
        .launch
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect("the assembled container launches");
    assert!(matches!(outcome, LaunchOutcome::Started { .. }));
    assert_eq!(runs.created()[0].name, "smoke-tests-1");
}

/// **The fifth reap site, added 2026-08-14 after the code-quality review proved
/// the guarantee was `DispatchService`-local.**
///
/// `service::dispatch::transition`'s doc claimed a choke point meant "a fifth
/// cannot forget it". The fifth path already existed here — [`abandon`] retires a
/// refused launch to `Canceled` — and the reap was not forgotten but
/// **unreachable**, because `LaunchService` had no field to call it through. That
/// is a compile-level fact, not a test failure, which is why no test caught it.
///
/// The window is narrow but real: `RunLogBroadcaster::subscribe` mints a channel
/// for any run id, and `LogSubscription::recv` answers `None` only on reap, so a
/// client that subscribed the instant the run was created would have held its
/// connection open forever.
#[tokio::test]
async fn a_refused_launch_releases_its_runs_log_channel() {
    let harness = Builder::new()
        .admitter(RecordingAdmitter::refusing(DomainError::ConcurrencyLimit {
            limit: 5,
        }))
        .build()
        .await;

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), plan_request())
        .await
        .expect_err("admission was refused");
    assert!(matches!(error, DomainError::ConcurrencyLimit { .. }));

    let retired = harness.runs.stored();
    assert_eq!(
        retired.len(),
        1,
        "premise: a row was created and then retired"
    );
    assert_eq!(retired[0].state, RunState::Canceled);
    assert_eq!(
        harness.logs.reaped(),
        vec![retired[0].id],
        "a refused launch is a terminal transition, so its channel must be released"
    );
}

// ---------------------------------------------------------------------------
// The `Collect` run kind
// ---------------------------------------------------------------------------

/// The report route qa-insights will serve (Task 30). Its *shape* mirrors
/// legacy's `format!("{}/api/collect/{}/{}", base, repo.id, branch)`
/// (`manager/src/services/collect.rs:90`), but qa-runs never builds it: the
/// runner posts wherever it is told, which is exactly what lets another gear own
/// the route.
const COLLECT_URL: &str = "https://insights.example/qa/v1/collect/r/main";

fn collect_request(repo_id: Uuid, branch: &str, collect_url: &str) -> LaunchRequest {
    LaunchRequest {
        target: RunTarget::Collect {
            repo_id,
            collect_url: collect_url.to_owned(),
        },
        // The branch travels on the request's own field, as it does for every
        // other kind — see `RunTarget::Collect`.
        branch: Some(branch.to_owned()),
        ..plan_request()
    }
}

/// A collect launch never enters the queue. Legacy submits it directly and
/// passes `exclusive: false` explicitly (`manager/src/services/argo.rs:369-372`);
/// admitting it would let a busy platform starve the hourly cycle that keeps
/// analytics' expected-case numbers current.
///
/// The plan's draft of this test asserted `f.queue_depth().await == 0`. This
/// harness has no queue at all — [`RecordingAdmitter`] scripts the decision — so
/// the assertion is made one step earlier and is strictly stronger: the admitter
/// was **never consulted**, which is what "bypasses admission" means and which no
/// depth reading can distinguish from "admitted and answered `Unqueued`".
#[tokio::test]
async fn a_collect_launch_bypasses_admission_and_is_never_exclusive() {
    let harness = Builder::new()
        // Scripted to insert a queue row: if the bypass is removed, this is what
        // the launch would go through, and the assertions below turn red.
        .admitter(RecordingAdmitter::answering(Admission::Dispatch {
            queue_id: QUEUE_ID,
        }))
        .build()
        .await;

    let outcome = harness
        .service
        .launch(
            &ctx(OWNER_TENANT),
            collect_request(REPO_ID, "main", COLLECT_URL),
        )
        .await
        .expect("collect launches");

    let LaunchOutcome::Started { run } = outcome else {
        panic!("a collect launch must start immediately, never queue");
    };
    assert!(!run.resolved_exclusive, "collect is always parallel");
    assert_eq!(
        run.exclusive_tier,
        ExclusiveTier::Default,
        "no tier resolved it: the cascade is never entered"
    );
    assert!(
        harness.admitter.admitted().is_none(),
        "admission must not be consulted, so nothing can be enqueued"
    );
    assert_eq!(
        harness.dispatcher.calls(),
        vec![(run.id, None)],
        "dispatched inline with no queue row"
    );
}

/// A collect launch that names a platform is **refused**, not quietly stripped.
///
/// `manager/src/services/collect.rs:128-149` passes `None` for every
/// platform-derived argument of `submit_workflow` and says why at `:146-148`:
/// *"Collection runs never target a real platform (see the `None`s above), so
/// there is nothing to be exclusive about."* Accepting one here would mount that
/// platform's kubeconfig, snapshot its `APP_VERSION`/`APP_BUILD` and admit its
/// variables tier into a run that only enumerates.
#[tokio::test]
async fn a_collect_launch_may_not_name_a_platform() {
    let harness = Builder::new()
        .environments(MockEnvironments::with_platform(
            OWNER_TENANT,
            platform_fixture(Some("7.1"), None),
        ))
        .build()
        .await;

    let mut request = collect_request(REPO_ID, "main", COLLECT_URL);
    request.platform_id = Some(PLATFORM_ID);

    let error = harness
        .service
        .launch(&ctx(OWNER_TENANT), request)
        .await
        .expect_err("a collect run has no platform");
    match error {
        DomainError::Validation { field, .. } => assert_eq!(field, "platform_id"),
        other => panic!("expected a Validation on platform_id, got {other:?}"),
    }
    assert!(
        harness.runs.stored().is_empty(),
        "the refusal is before the insert, so no row is stranded"
    );
}

/// Rule 6 for a collect run: the branch label **is** the recorded test version,
/// and rule 1's repository tier still fills an absent one. The platform tier is
/// unreachable — a collect launch has no platform — so the chain is
/// explicit-then-repository.
///
/// The recorded version is what `service::dispatch` enumerates against
/// (`a_collect_dispatch_bundles_every_plan_on_the_branch`), so this is the seam
/// between the two halves rather than a restatement of `resolve_branch`.
#[tokio::test]
async fn a_collect_run_records_its_branch_and_falls_back_to_the_repository_default() {
    for (requested, expected) in [(Some("release-9"), "release-9"), (None, "main")] {
        let harness = Builder::new()
            .catalog(MockCatalog::new().with_repo_default("main"))
            .build()
            .await;

        let mut request = collect_request(REPO_ID, "unused", COLLECT_URL);
        request.branch = requested.map(str::to_owned);

        let outcome = harness
            .service
            .launch(&ctx(OWNER_TENANT), request)
            .await
            .expect("collect launches");
        let LaunchOutcome::Started { run } = outcome else {
            panic!("a collect launch starts immediately");
        };
        assert_eq!(run.test_version.as_deref(), Some(expected));
        assert!(
            run.name.starts_with(&format!("collect-{REPO_ID}-")),
            "the run name is legacy's `collect ...` base, slugified: {}",
            run.name
        );
    }
}

/// The collect deadline reaches the run row: `domain::timeout` resolves 600 for
/// this kind (legacy's synthetic `timeout_seconds: 600`,
/// `manager/src/services/collect.rs:106`) and `resolve` must actually pass the
/// kind through. A launch that hard-coded `RunKind::Plan` there would give a
/// collect run 300 seconds and nothing else would notice.
#[tokio::test]
async fn a_collect_runs_deadline_is_the_legacy_ten_minutes() {
    let before = OffsetDateTime::now_utc();
    let harness = Builder::new()
        // Deliberately non-zero, and deliberately not 600: the collect arm
        // consults neither the configured default nor any plan.
        .default_timeout(900)
        .build()
        .await;

    let outcome = harness
        .service
        .launch(
            &ctx(OWNER_TENANT),
            collect_request(REPO_ID, "main", COLLECT_URL),
        )
        .await
        .expect("collect launches");
    let LaunchOutcome::Started { run } = outcome else {
        panic!("a collect launch starts immediately");
    };
    let deadline = run.timeout_at.expect("a collect run has a deadline");
    let seconds = (deadline - before).whole_seconds();
    assert!(
        (595..=605).contains(&seconds),
        "expected ~600s (legacy's collect.rs:106), got {seconds}"
    );
}
