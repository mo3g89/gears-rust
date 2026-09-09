//! Tests for [`super`]'s projection.
//!
//! Split into its own file, following the plan's file map, because
//! `ingest.rs`'s reasoning header is long and the two grow for different
//! reasons.
//!
//! Everything here drives the pure mapper [`super::project_rows`] directly.
//! The `async` half — the port reads, the vanished-run branch and the
//! repository write — is measured end-to-end by
//! `domain::service::reconcile_tests`, against a real transaction and the real
//! repository, driven through `ReconcileService::sweep`, `::rebuild` and
//! `::reproject`, the only callers `read_run_projection` and
//! `write_run_projection` have left. A transactional broker consumer's tests
//! covered the same properties through Task 40, against a real offset store as
//! well; that consumer and the event-broker dependency it needed were deleted
//! once it was established that no deployment ever registered the client it
//! needed (`crate::gear`'s header), and `reconcile_tests` is where those
//! properties live now.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qa_runs_sdk::{ExclusiveTier, Run, RunSource, RunState, RunTarget, RunTestResult};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{plan_identity, project_rows};

fn uuid(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

fn ts() -> OffsetDateTime {
    time::macros::datetime!(2026-08-18 10:00:00 UTC)
}

/// The run's own creation instant — **two hours before [`ts`]**, and distinct
/// from every other instant on the fixture run.
///
/// `qa_test_results.run_created_at` is the fallback half of legacy's
/// `COALESCE(rr.finished_at, rr.created_at)` and the only column
/// `project_rows` fills from `Run::created_at`. `Run` carries four instants and
/// `run_with` gave all four the same value until Task 21b's third fix round, so
/// `Some(run.started_at)` and `Some(run.updated_at)` both compiled and left the
/// suite green — on the one field a controller ruling had just added. Four
/// distinct values is the brief's own rule, applied late.
fn run_created() -> OffsetDateTime {
    time::macros::datetime!(2026-08-18 08:00:00 UTC)
}

/// The run's start — an hour after it was created and an hour before it
/// finished. Distinct so a mapper reading it in place of [`run_created`] or
/// [`ts`] is visible.
fn run_started() -> OffsetDateTime {
    time::macros::datetime!(2026-08-18 09:00:00 UTC)
}

/// The last write to the run row — **after** it finished, which is the ordinary
/// case (the terminal transition is itself a write). Distinct for the same
/// reason.
fn run_updated() -> OffsetDateTime {
    time::macros::datetime!(2026-08-18 10:30:00 UTC)
}

/// A finished plan run with every denormalized source populated, so a mapper
/// that dropped one is visible rather than merely untested.
fn run_with(target: RunTarget) -> Run {
    Run {
        id: uuid(1),
        name: "smoke-1".to_owned(),
        target,
        platform_id: Some(uuid(2)),
        test_version: Some("release/9.1".to_owned()),
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
        started_at: Some(run_started()),
        finished_at: Some(ts()),
        error: None,
        created_at: run_created(),
        updated_at: run_updated(),
    }
}

fn plan_run() -> Run {
    run_with(RunTarget::Plan {
        repo_id: uuid(3),
        path: "plans/smoke.yaml".to_owned(),
    })
}

fn result(test_file: &str, test_name: &str, nodeid: &str) -> RunTestResult {
    RunTestResult {
        run_id: uuid(1),
        test_file: test_file.to_owned(),
        test_name: test_name.to_owned(),
        status: "PASSED".to_owned(),
        duration: Some("1.5s".to_owned()),
        launch_id: Some("rp-77".to_owned()),
        jira_key: Some("QA-1".to_owned()),
        nodeid: nodeid.to_owned(),
        reason: None,
        ticket: None,
    }
}

/// Every one of the **eight** denormalized columns comes off the run, and each
/// has its own source field. A mapper that transposed two of them —
/// `app_version` into `app_build`, say — type-checks perfectly, and both are
/// `Option<String>` on both sides, so only distinct fixture values catch it.
///
/// # The two instants are the case this test was blind to
///
/// It said "seven" and asserted seven until Task 21b's third fix round.
/// `run_created_at` — added by controller Ruling A, and the fallback half of
/// legacy's `COALESCE(rr.finished_at, rr.created_at)` — was written by
/// `project_rows` (this crate's **only** non-test writer of that column) and
/// asserted nowhere. And `Run` carries *four* instants, all of which `run_with`
/// stamped with the same value, so three of the four wrong sources compiled and
/// passed: `Some(run.updated_at)`, `run.started_at` and `run.finished_at` (the
/// last two are already `Option`, so they need no `Some`). Only
/// `Some(run.started_at)` was a type error, and only by accident of that
/// wrapping.
///
/// That is this test's own stated purpose — making "a mapper that dropped one
/// visible rather than merely untested" — unapplied to the newest column, which
/// is worth naming so the next column added here does not repeat it. The fixture
/// now gives the four instants four distinct values ([`run_created`],
/// [`run_started`], [`ts`], [`run_updated`], in that order), and all three
/// compiling wrong sources were confirmed red.
#[test]
fn every_denormalized_column_comes_from_its_own_field_on_the_run() {
    let (files, _) = project_rows(&plan_run(), vec![result("tests/a.py", "test_a", "")]);

    let row = &files[0];
    assert_eq!(row.product_version.as_deref(), Some("9.1.0"));
    assert_eq!(row.app_build.as_deref(), Some("9.1.0-4412"));
    assert_eq!(row.platform_id, Some(uuid(2)));
    assert_eq!(row.repo_id, Some(uuid(3)));
    assert_eq!(row.plan_path.as_deref(), Some("plans/smoke.yaml"));
    assert_eq!(row.branch.as_deref(), Some("release/9.1"));
    assert_eq!(row.run_finished_at, Some(ts()));
    assert_eq!(
        row.run_created_at,
        Some(run_created()),
        "run_created_at is the run's *creation* instant, not its start \
         (run_started), its finish (ts) or its last write (run_updated) - all \
         four are Option<OffsetDateTime> or OffsetDateTime on the run and any of \
         them compiles here"
    );
}

/// The per-test half is carried verbatim. `status` in particular is an open set
/// and must not be normalized, re-cased or validated here: legacy writes
/// unvalidated runner text (`manager/src/routes/runs.rs:1115`), and the
/// producer has already applied its own `normalize_status`.
#[test]
fn the_per_test_fields_are_carried_through_untouched() {
    let mut row = result("tests/a.py", "test_a", "");
    row.status = "QUARANTINED".to_owned();

    let (files, _) = project_rows(&plan_run(), vec![row]);

    assert_eq!(files[0].test_file, "tests/a.py");
    assert_eq!(files[0].test_name, "test_a");
    assert_eq!(files[0].status, "QUARANTINED");
    assert_eq!(files[0].duration.as_deref(), Some("1.5s"));
    assert_eq!(files[0].launch_id.as_deref(), Some("rp-77"));
    assert_eq!(files[0].jira_key.as_deref(), Some("QA-1"));
}

/// An empty `nodeid` is the single spelling of "the producer reported none",
/// and it is what a file-level row carries. No case row may be written for it:
/// `qa_test_case_results.nodeid` would then hold `''` for a row that is not a
/// case at all, and every per-case aggregate would double-count the file.
#[test]
fn a_result_with_no_nodeid_produces_no_case_row() {
    let (files, cases) = project_rows(&plan_run(), vec![result("tests/a.py", "test_a", "")]);

    assert_eq!(files.len(), 1);
    assert!(
        cases.is_empty(),
        "an empty nodeid is not a case-level result"
    );
}

/// The other half of the discriminator, and the half that is easy to get
/// wrong: a case-carrying result produces **both** rows, not one instead of the
/// other. Legacy's parser writes both tables, and the file-level counters the
/// dashboard renders come from the file-level one — so an `if/else` here would
/// silently empty every count for any runner that reports node ids.
#[test]
fn a_result_with_a_nodeid_produces_both_a_file_row_and_a_case_row() {
    let (files, cases) = project_rows(
        &plan_run(),
        vec![result("tests/a.py", "test_a", "tests/a.py::test_a[1]")],
    );

    assert_eq!(files.len(), 1, "the file-level row is still written");
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].nodeid, "tests/a.py::test_a[1]");
    assert_eq!(cases[0].test_file, "tests/a.py");
    assert_eq!(
        cases[0].name, "test_a",
        "the case table spells the function `name`, and it is the same value"
    );
}

/// `reason` and `ticket` exist only on the case-level row — they are the two
/// columns the file-level table does not have — so a mapper that reached for
/// the wrong struct would drop them.
#[test]
fn the_case_only_columns_reach_the_case_row() {
    let mut row = result("tests/a.py", "test_a", "tests/a.py::test_a");
    row.status = "XFAIL".to_owned();
    row.reason = Some("known flake".to_owned());
    row.ticket = Some("QA-99".to_owned());

    let (_, cases) = project_rows(&plan_run(), vec![row]);

    assert_eq!(cases[0].status, "XFAIL");
    assert_eq!(cases[0].reason.as_deref(), Some("known flake"));
    assert_eq!(cases[0].ticket.as_deref(), Some("QA-99"));
}

/// `upsert_run_results` derives `ingest_ordinal` from this `Vec`'s order, and
/// that ordinal is the within-run half of the latest-wins tiebreak. Sorting or
/// partitioning here would invert it with nothing else failing, so the
/// preservation is pinned rather than assumed.
#[test]
fn the_file_rows_keep_the_producers_order() {
    let rows = vec![
        result("tests/a.py", "test_a", ""),
        result("tests/a.py", "test_b", "tests/a.py::test_b"),
        result("tests/z.py", "test_c", ""),
        result("tests/a.py", "test_a", ""),
    ];

    let (files, _) = project_rows(&plan_run(), rows);

    let seen: Vec<_> = files
        .iter()
        .map(|f| (f.test_file.as_str(), f.test_name.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("tests/a.py", "test_a"),
            ("tests/a.py", "test_b"),
            ("tests/z.py", "test_c"),
            ("tests/a.py", "test_a"),
        ],
        "neither sorted nor deduplicated"
    );
}

/// A run with no results yet is a real state — the run was created and nothing
/// has reported. It must produce two empty vectors rather than being skipped,
/// because `upsert_run_results` empties both tables for the run either way and
/// "this run now has no results" has to be expressible.
#[test]
fn a_run_with_no_results_projects_to_two_empty_batches() {
    let (files, cases) = project_rows(&plan_run(), Vec::new());

    assert!(files.is_empty());
    assert!(cases.is_empty());
}

/// A still-running run projects rows with no finish instant. Legacy writes
/// RUNNING and PENDING rows too, and `ExecRow::ts` coalesces to the creation
/// instant for them — so stamping "now" here instead of `None` would fabricate
/// a timestamp the analytics ordering reads.
#[test]
fn an_unfinished_run_projects_rows_with_no_finish_instant() {
    let mut run = plan_run();
    run.state = RunState::Running;
    run.finished_at = None;

    let (files, _) = project_rows(&run, vec![result("tests/a.py", "test_a", "")]);

    assert_eq!(files[0].run_finished_at, None);
}

/// The plan identity is `(repo_id, plan_path)` and the two kinds without one
/// get neither half. A collect run in particular carries a `repo_id` on its
/// target, and storing it here would make every plan-scoped analytics filter
/// match a run that executed no plan.
#[test]
fn only_the_two_plan_bearing_kinds_carry_a_plan_identity() {
    let repo = uuid(3);

    assert_eq!(
        plan_identity(&RunTarget::Plan {
            repo_id: repo,
            path: "plans/smoke.yaml".to_owned(),
        }),
        (Some(repo), Some("plans/smoke.yaml".to_owned()))
    );
    assert_eq!(
        plan_identity(&RunTarget::Test {
            repo_id: repo,
            path: "plans/smoke.yaml".to_owned(),
            test_file: "tests/a.py".to_owned(),
        }),
        (Some(repo), Some("plans/smoke.yaml".to_owned()))
    );
    assert_eq!(
        plan_identity(&RunTarget::CustomPlan { id: uuid(4) }),
        (None, None)
    );
    assert_eq!(
        plan_identity(&RunTarget::Collect {
            repo_id: repo,
            collect_url: "https://example.invalid/collect".to_owned(),
        }),
        (None, None),
        "a collect run enumerates a repository; it executes no plan"
    );
}

/// A run whose platform, versions and branch are all unset must project
/// `None`s rather than empty strings: `qa_test_results` distinguishes the two,
/// and Task 24's `"unknown"` build fallback depends on the distinction.
#[test]
fn absent_run_metadata_projects_as_null_not_as_empty_text() {
    let mut run = run_with(RunTarget::CustomPlan { id: uuid(4) });
    run.platform_id = None;
    run.test_version = None;
    run.app_version = None;
    run.app_build = None;

    let (files, _) = project_rows(&run, vec![result("tests/a.py", "test_a", "")]);

    assert_eq!(files[0].platform_id, None);
    assert_eq!(files[0].branch, None);
    assert_eq!(files[0].product_version, None);
    assert_eq!(files[0].app_build, None);
}

// ---------------------------------------------------------------------------
// Status classification (Task 14)
// ---------------------------------------------------------------------------

use super::{FAILED_STATUSES, PASSED_STATUSES, ResultCounts, StatusBucket, classify, classify_all};

/// The five counters, ported from `manager/src/routes/plans.rs:188-192`.
/// `XFAIL`/`XPASS` are deliberately in `total` and nowhere else — legacy counts
/// them separately (`manager/src/routes/analytics.rs:1359-1360`) and folding
/// them into `passed` would inflate every pass rate on the dashboard.
#[test]
fn xfail_and_xpass_count_toward_total_and_no_other_bucket() {
    let counts = classify_all(&["PASSED", "XFAIL", "XPASS", "FAILED"]);
    assert_eq!(counts.total, 4);
    assert_eq!(counts.passed, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.skipped, 0);
    assert_eq!(counts.in_progress, 0);
}

/// Status is an open set and the mapper must not fail closed: two legacy paths
/// write unconstrained text (`manager/src/routes/runs.rs:1115` unvalidated, and
/// `manager/src/services/argo.rs:2932-2943` falling through to
/// `other.to_uppercase()`). A ninth value is a runner change, not corruption.
#[test]
fn an_unrecognized_status_is_stored_and_counted_only_in_total() {
    let counts = classify_all(&["QUARANTINED"]);
    assert_eq!(counts.total, 1);
    assert_eq!(
        counts.passed + counts.failed + counts.skipped + counts.in_progress,
        0
    );
}

/// The whole eight-value vocabulary at once, against the SQL it ports.
///
/// The two tests above pin the two rules most likely to be got wrong; this one
/// pins the rest, because a `match` arm that quietly moved `ERROR` out of
/// `failed`, or `RUNNING` out of `in_progress`, would leave both of them green.
#[test]
fn the_whole_status_vocabulary_maps_the_way_the_legacy_sql_filters_do() {
    assert_eq!(classify("PASSED"), StatusBucket::Passed);
    assert_eq!(classify("FAILED"), StatusBucket::Failed);
    assert_eq!(
        classify("ERROR"),
        StatusBucket::Failed,
        "legacy's `status IN ('FAILED', 'ERROR')` is one counter, which is why \
         RunResult has five fields and not six"
    );
    assert_eq!(classify("SKIPPED"), StatusBucket::Skipped);
    assert_eq!(classify("PENDING"), StatusBucket::InProgress);
    assert_eq!(classify("RUNNING"), StatusBucket::InProgress);
    assert_eq!(classify("XFAIL"), StatusBucket::Uncounted);
    assert_eq!(classify("XPASS"), StatusBucket::Uncounted);
}

/// `total` is `COUNT(tr.id)` over every row, so a set that is *entirely*
/// uncounted still has a total. A caller deriving `total` by addition would get
/// zero here and divide by it.
#[test]
fn a_run_of_only_uncounted_statuses_still_has_a_total() {
    let counts = classify_all(&["XFAIL", "XPASS", "QUARANTINED"]);
    assert_eq!(counts.total, 3);
    assert_eq!(
        counts,
        ResultCounts {
            total: 3,
            ..ResultCounts::default()
        }
    );
}

/// Matching is exact — no trimming, no re-casing — because legacy's is. A
/// lowercase status moves no counter there either, and normalising it here
/// would be a silent divergence that starts moving one.
///
/// Trimming is the producer's job and is already done: `qa-runs`'
/// `normalize_status` trims and deliberately does not re-case.
#[test]
fn classification_is_case_sensitive_and_does_not_trim() {
    assert_eq!(classify("passed"), StatusBucket::Uncounted);
    assert_eq!(classify("Passed"), StatusBucket::Uncounted);
    assert_eq!(classify(" PASSED"), StatusBucket::Uncounted);
    assert_eq!(classify("PASSED "), StatusBucket::Uncounted);
}

/// An empty set is all zeroes rather than a panic or a `None`. A run with no
/// results is a real state — the projection writes it — and every caller
/// divides by `total`.
#[test]
fn an_empty_set_counts_to_zero_everywhere() {
    let counts = classify_all::<&str>(&[]);
    assert_eq!(counts, ResultCounts::default());
}

/// **[`FAILED_STATUSES`] is legacy's pair, and [`classify`] is defined from it.**
///
/// The constant exists because one read needs the predicate in SQL —
/// `ResultsRepository::recent_failures`, legacy's
/// `WHERE tr.status IN ('FAILED', 'ERROR') … LIMIT 10`
/// (`manager/src/routes/dashboard.rs:275-278`).
///
/// # What this test does *not* do, stated because it claimed to
///
/// Task 21b shipped this test asserting that the constant and [`classify`]'s
/// `Failed` arm agreed "in both directions", and its doc named the drift it
/// caught: a runner starts emitting `CRASHED`, [`classify`]'s arm grows, the
/// constant does not. **It could not catch that.** The check iterated a
/// hardcoded nine-status literal, and a genuinely new status is by definition
/// not in a hardcoded literal, so that mutation left this green. What it
/// actually pinned was drift *within* the nine already written down.
///
/// The fix is not a better test. [`classify`]'s arm is now
/// `s if FAILED_STATUSES.contains(&s)`, so the two are **one definition** and
/// the drift is not expressible — which is why what remains here is narrow and
/// says so:
///
/// * **The set is legacy's, verbatim.** `('FAILED', 'ERROR')` and nothing else,
///   which is a claim about the *citation* rather than about internal
///   consistency, and the one thing a shared definition cannot check itself.
///   Every legacy site agrees — `dashboard.rs:320` and `:324` (the KPI pair),
///   `:387` (the flaky fold), `:486` (the quality-vector fold), and
///   `plans.rs:189` (the five counters).
///
///   **Three of those five were off when this test was written** — `:389` and
///   `:488` are the `FROM test_results tr` lines of their statements and
///   `plans.rs:190` is the `SKIPPED` counter. Since the fix deliberately reduced
///   this test to a claim about the *citation*, the citation is the assertion,
///   and three-fifths of it missed. Corrected in the round after; each is now
///   labelled with which fold it is, so a future off-by-one is visible without
///   opening legacy.
/// * **The guard arm sits below `"PASSED"`**, so a literal arm above it still
///   wins. That ordering is what stops the constant capturing a status named
///   earlier in the `match`, and the assertion that it holds for the whole
///   vocabulary is
///   [`the_whole_status_vocabulary_maps_the_way_the_legacy_sql_filters_do`],
///   which is where it belongs.
///
/// So this is a regression guard on the constant's contents, and the mutation it
/// catches is an edit to `FAILED_STATUSES` itself — which now moves `classify`,
/// `failed_24h_count`, `failed_recent`, the daily trend and the five counters
/// together, and is therefore worth a test that names the citation it must match.
///
/// **Widening the set does not even compile**, which is the strongest form this
/// guard can take: `assert_eq!` between `[&str; 3]` and `[&str; 2]` is a type
/// error, so `FAILED_STATUSES = ["FAILED", "ERROR", "CRASHED"]` is a build break
/// rather than a test failure. Renaming a member — `"ERROR"` to `"FAILURE"`,
/// say — compiles and fails here at runtime, together with
/// [`the_whole_status_vocabulary_maps_the_way_the_legacy_sql_filters_do`] and the
/// four counter tests that depend on `ERROR` folding into `failed`. Both were
/// applied and observed.
#[test]
fn the_failed_status_set_is_legacys_pair_and_classify_is_defined_from_it() {
    assert_eq!(
        FAILED_STATUSES,
        ["FAILED", "ERROR"],
        "legacy's failure set is exactly this pair (manager/src/routes/\
         dashboard.rs:320, :324, :387, :486; plans.rs:189). Widening it here \
         widens classify with it, and with classify every counter in the gear",
    );

    for status in FAILED_STATUSES {
        assert_eq!(
            classify(status),
            StatusBucket::Failed,
            "{status} must reach the guard arm; a literal arm added above it \
             would shadow the constant",
        );
    }
}

/// **[`PASSED_STATUSES`] is legacy's single status, and [`classify`] is defined
/// from it.**
///
/// The sibling of
/// [`the_failed_status_set_is_legacys_pair_and_classify_is_defined_from_it`],
/// added by Task 23b for the same reason one method later: the flaky read needs
/// *both* status partitions in SQL, because its `HAVING` is two-sided
/// (`manager/src/routes/dashboard.rs:393-394`) and a `LIMIT` follows it.
///
/// The same narrowness applies and for the same reason — the constant and the
/// `match` arm are one definition, so internal agreement is not a thing a test
/// can be wrong about. What is left is the *citation*: legacy's numerator is
/// `tr.status = 'PASSED'`, an equality rather than an `IN`, at `dashboard.rs:333`
/// and `:342` (the KPI pair), `:386` (the flaky fold) and `:485` (the
/// quality-vector fold). Each line was re-derived at source; `:329` and `:337`,
/// which the KPI reads' own docs cite, are that pair's *denominator* lines and
/// not its numerator.
///
/// **Widening it does not compile**, exactly as widening the failure set does not:
/// `assert_eq!` between `[&str; 2]` and `[&str; 1]` is a type error. Renaming the
/// member compiles and turns this red together with
/// [`the_whole_status_vocabulary_maps_the_way_the_legacy_sql_filters_do`] and
/// every pass-rate test in the gear.
#[test]
fn the_passed_status_set_is_legacys_single_status_and_classify_is_defined_from_it() {
    assert_eq!(
        PASSED_STATUSES,
        ["PASSED"],
        "legacy's pass numerator is this one status (manager/src/routes/\
         dashboard.rs:333, :342, :386, :485). Widening it here widens classify \
         with it, and with classify every pass rate in the gear",
    );

    for status in PASSED_STATUSES {
        assert_eq!(
            classify(status),
            StatusBucket::Passed,
            "{status} must reach the guard arm; an arm added above it would \
             shadow the constant",
        );
    }
}

/// **The two counted sets are disjoint, which is what makes the flaky
/// denominator derivable as their union.**
///
/// `ResultsRepository::flaky_groups` builds legacy's
/// `('PASSED', 'FAILED', 'ERROR')` denominator (`dashboard.rs:388`) by chaining
/// [`PASSED_STATUSES`] and [`FAILED_STATUSES`] rather than spelling a third
/// literal. That derivation is only legacy's set if the two do not overlap: a
/// shared member would be counted twice by the chained `IN` list — harmless in
/// SQL, since `IN` de-duplicates — but would also mean one status in *both*
/// counters, so `passed + failed` would exceed `total` and
/// `FlakyGroup::total`'s claim that it is arithmetically their sum would be
/// false.
///
/// Asserted here rather than in the repository, because it is a property of the
/// two constants and not of any statement. `classify`'s arm order hides the
/// overlap if it ever appears — the earlier arm simply wins — which is exactly
/// why the overlap needs its own assertion; `classify`'s own comment points at
/// this test.
#[test]
fn the_two_counted_status_sets_are_disjoint_so_the_denominator_is_their_sum() {
    for passed in PASSED_STATUSES {
        assert!(
            !FAILED_STATUSES.contains(&passed),
            "{passed} is in both counted sets, so a flaky group's passed + \
             failed would exceed its total",
        );
    }
}
