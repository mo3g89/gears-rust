//! Tests for the summary, the per-test tallies, the three lists, the two charts,
//! the flaky detector, the quality vectors, the three group breakdowns, the build
//! distribution and the build-tests drill-down.
//!
//! Every test drives a pure function directly, exactly as `universe_tests`
//! does: the universe
//! arrives over [`CatalogReader`](crate::domain::ports::CatalogReader), the
//! file-level rows over
//! [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe)
//! and the case-level rows over
//! [`ResultsRepository::case_rows_for_runs`](crate::domain::repos::ResultsRepository::case_rows_for_runs)
//! (Task 25b), and none of those tiers is reachable from here.
//!
//! # The brief's two tests, and where they are
//!
//! Task 21's Step 1 gives two tests as pseudo-code. Both are here, with their
//! names and their assertions unchanged —
//! [`a_file_without_case_rows_contributes_one_case_of_its_file_status`] and
//! [`the_worst_non_passing_case_status_wins_and_error_folds_into_failed`]. One
//! call shape changed and it is a finding rather than a liberty: the brief writes
//! `summarize(&[file_result("tests/a.py", "PASSED")], &[])`, i.e. a summary over
//! a flat list of file results, and legacy's summary is a fold over the
//! **universe** joined to the latest map (`analytics.rs:1227-1230`,
//! `:1288-1293`). A universe file with no row at all is `NOT_RUN` and a row
//! outside the universe is invisible, and neither property is expressible over a
//! flat list — so the fixture builds the two inputs legacy folds. The assertions
//! are the brief's.
//!
//! # What is here beyond the brief
//!
//! One falsifiable test per further rule Step 0 turned up. Two of them exist
//! because the rule looks like a defect and a later reader would "fix" it:
//!
//! * [`a_not_run_file_contributes_no_synthetic_case_so_the_undercount_is_real`] —
//!   `attach_case_data`'s doc claims totals "never undercount", and its fallback
//!   arm counts only `PASSED` and `FAILED` (`:1371-1376`), so a `NOT_RUN` file
//!   without case rows contributes nothing.
//! * [`an_unrecognized_case_status_is_counted_in_no_case_counter`] — `case_total`
//!   is the sum of five named counters (`:1385`), not the number of case rows.
//!
//! Both are ported verbatim under Phase B's standing instruction and pinned
//! here so a later change to a rendered number is a failing test.
//!
//! A third test used to stand beside these two,
//! `the_summary_total_is_the_universe_length_so_a_file_in_two_plans_counts_twice`,
//! asserting that `total` (`universe.len()`, `:1231`) counts a file listed by
//! two plans twice. It hand-built a universe no production reader can produce:
//! qa-catalog's `walk_repo_universe` merges a second plan's contribution into
//! the first, within one repository, before the universe ever reaches this
//! module, so that input never occurs. Removed, along with the three doc
//! comments that rested on the same wrong reading —
//! [`OverviewSummary::total`], [`build_heatmap`]'s header and
//! [`the_heatmap_emits_one_row_per_universe_entry_in_the_universes_order`]'s —
//! rewritten rather than deleted, because each was pointing at a real
//! invariant and describing it wrongly.
//!
//! **No existing test here varied `repo_id`.** Two repositories can list the
//! same `test_file` — `(tenant_id, product_id)` is not a unique index — and
//! that is the input the suite was missing; see [`ExecRow::repo_id`] and
//! `the_analytics_folds_key_on_repo_id_and_test_file_not_the_path_alone`.
//!
//! # Task 23's five brief tests, and the one it does not contain
//!
//! Four of the brief's five are here with their names and assertions unchanged —
//! [`flaky_requires_five_executions_and_a_pass_rate_inside_the_band`],
//! [`flaky_sorts_by_pass_rate_then_by_evidence`],
//! [`a_test_absent_from_the_universe_is_not_reported_flaky`] and
//! [`flaky_folds_error_into_fail_and_everything_else_into_skipped`]. One call
//! shape changed, and it is the standing ruling rather than a liberty: the brief
//! writes `build_flaky(universe, rows, 7)`, three arguments, where every dated
//! fold in this module takes `today: Date` as a fourth and reads no clock of its
//! own. [`TODAY`] is passed.
//!
//! **The fifth is deliberately absent, and it is a Step 0 finding.** The brief
//! asks for `the_component_falls_back_to_the_second_path_segment_under_tests`
//! over an `infer_component_from_path` ported into this crate. That rule is
//! **already ported, in the gear that owns it**:
//! `qa-catalog/src/domain/service/plans.rs:492` `component_for` is legacy's
//! `infer_component_from_path` (`analytics.rs:1794-1803`), and qa-catalog pins it
//! with `a_file_without_meta_falls_back_to_stem_name_and_inferred_component`. By
//! the time a `UniverseTest` reaches this gear its `component` is resolved, so a
//! second copy here would be a second spelling of one rule with no consumer and
//! nothing keeping the two in agreement. What this crate *does* own is the
//! `"unknown"` label legacy applies afterwards (`:1098-1103`), and
//! [`component_groups_partition_the_universe_and_blanks_group_under_unknown`]
//! covers it.
//!
//! # What Task 23 adds beyond the brief
//!
//! One falsifiable test per further rule its Step 0 turned up, and four of them
//! exist because the rule looks like a defect:
//!
//! * [`a_future_dated_row_is_flaky_evidence_but_not_chart_data`] — the flaky
//!   window is one-sided (`:1661-1663`) where the charts' is a closed set, and
//!   this is the only test that can tell `>= cutoff` from
//!   `window.contains(day)`.
//! * [`a_multiply_tagged_file_is_counted_once_per_tag`] — the tag breakdown is
//!   not a partition; its counters sum to more than the universe size.
//! * [`the_environment_grouping_does_not_narrow_the_universe`] — selecting an
//!   environment bar returns every test (`:1178`).
//! * [`a_component_of_none_matches_no_component_filter`] — the files grouped
//!   under `"unknown"` are unreachable through the drill-down (`:1165`).
//!
//! And two guard against a *this-crate* mistake rather than a legacy one:
//! [`the_flaky_cutoff_is_the_trend_windows_first_day`] passes only out-of-range
//! day counts, so a fold that forgot to clamp or reached for the heatmap's clamp
//! fails (the Task 22 lesson), and
//! [`flaky_and_the_per_test_tally_split_a_status_the_same_way`] asserts from the
//! outside that the shared `tally` helper has not been re-forked.
//!
//! # Task 24's three brief tests, and what Step 0 found wrong about them
//!
//! All three are here with their names and their assertions unchanged —
//! [`builds_sort_newest_first_by_numeric_segment`],
//! [`a_non_numeric_segment_falls_back_to_reverse_string_order`] and
//! [`identical_builds_compare_equal`]. Nothing about the call shape changed;
//! [`compare_build_desc`] is a bare comparator and takes no clock, no date and no
//! universe.
//!
//! **What did change is the claim the first one was given to support.** The brief
//! says the padding in [`compare_build_desc`] and the `zip` in
//! [`sorted_versions_desc`] "therefore disagree" about `"1.2"` against `"1.2.1"`,
//! and that "the difference is exactly what the plan's first test pins". Measured
//! over every plausible build label, the two comparators **agree** on that pair
//! and on every other one: the padded comparison at the extra index and the
//! whole-string tiebreak point the same way unless the extra segment sorts below
//! `"0"`. So the brief's first test passes against a zipping implementation too,
//! and it says so in its own doc.
//! [`the_build_distribution_comparator_diverges_from_sorted_versions_desc_two_ways`] is
//! the test that actually separates them, and
//! [`compare_build_desc`]'s header carries the corrected reason the two functions
//! stay two.
//!
//! # What Task 24 adds beyond the brief
//!
//! The brief's own run line is `cargo test -p qa-insights build_distribution
//! build_tests`, a filter that matches none of the three tests above (and which
//! `cargo` rejects outright — it takes one positional `TESTNAME`, so the two
//! filters have to go after `--`). Eleven tests carry one of those two substrings
//! so the command selects something, and each pins a rule Step 0 turned up:
//!
//! * [`the_build_distribution_counts_only_the_latest_row_per_test`] — the grain
//!   is the test, not the row, and `ERROR` folds into `FAILED`.
//! * [`the_build_distribution_sorts_unknown_last_and_the_rest_newest_first`] —
//!   the `eq_ignore_ascii_case` special case ahead of the comparator, including
//!   what happens between *two* unknowns.
//! * [`the_build_distribution_collapses_absent_blank_and_literal_unknown_builds`]
//!   — ruling R13: three inputs, one bar, and legacy cannot tell them apart
//!   either.
//! * [`the_build_distribution_picks_the_latest_run_id_by_a_strictly_greater_scan`]
//!   and [`the_build_tests_snapshots_come_back_in_test_file_order`] — ruling
//!   R14's determinism, and the tie it decides.
//! * [`the_build_distribution_passes_an_unrecognized_status_through_and_counts_it_nowhere`]
//!   — ruling R13's seventh classification, and the bar of zeroes it produces.
//! * [`the_build_distribution_is_empty_without_snapshots`] — the early return and
//!   the universe membership check, together.
//! * [`build_tests_are_ranked_failed_then_passed_then_skipped_then_other`],
//!   [`build_tests_filter_the_build_case_insensitively_and_untrimmed`] and
//!   [`build_tests_join_their_metadata_off_the_universe_entry`] — the three halves
//!   of the drill-down: the rank, the seam where the trim is *not*, and the join.
//! * [`the_build_distribution_comparator_diverges_from_sorted_versions_desc_two_ways`] —
//!   above.
//!
//! One rule is deliberately **untested and says so in its own doc**: legacy's
//! `filter_map` at `:429` drops a snapshot whose file is missing from the universe
//! map, and no input can reach it, because that map and
//! [`latest_per_test_snapshot`]'s membership set are built from the same slice.

// `assert_eq!` on the three percentages and on `pct` itself. Every value under
// test is the output of `pct`, whose contract *is* an exact one-decimal
// quantity — `((v / t) * 1000.0).round() / 10.0` (`analytics.rs:1957-1963`) — so
// an epsilon comparison would accept the truncation this test exists to reject.
#![allow(clippy::float_cmp)]
// `.expect` on `points.last()` and on a `find` over the day axis. Both are
// assertions about the axis this module builds — a `None` there *is* the failure
// — and the message names which one broke. `dashboard_tests` allows the same
// pair for the same reason.
#![allow(clippy::expect_used)]

use std::collections::HashMap;

use qa_catalog_sdk::UniverseTest;
use time::macros::{date, datetime};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    AnalyticsListItem, AnalyticsLists, FlakyTest, GroupBy, GroupSummary, PlatformGroupSummary,
    UNKNOWN_COMPONENT, UNTAGGED, apply_universe_group_filter, build_case_data, build_flaky,
    build_grouped_summaries, build_heatmap, build_last_run_build_distribution, build_lists,
    build_quality_vector_summary, build_stats_map, build_status_rank, build_test_details,
    build_trend, compare_build_desc, effective_case_status, flaky_cutoff, heatmap_days,
    latest_per_test_snapshot, pct, recent_days, sorted_versions_desc, summarize, trend_days,
};
use crate::domain::analytics::universe::{NOT_RUN, UNKNOWN_BUILD, build_latest_map};
use crate::domain::analytics::{CaseRow, ExecRow};
use crate::domain::ports::Clock;
use crate::domain::service::test_support::{
    FixedClock, TODAY, UNIVERSE_TEST_REPO_ID, exec_row_at, ts, universe_test, universe_test_full,
};

/// A run id chosen per test rather than random, because the case-level join key
/// is `(run_id, test_file)` and a fixture that could not name a run could not
/// discriminate "this file's latest run" from "any run".
fn run(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// One file-level row for `test_file` with `status`, attributed to `run_id`.
///
/// Two departures from `exec_row_at`, and both exist to make a fixture able to
/// fail:
///
/// * **The run id is named**, where `exec_row_at` invents a fresh one per call.
///   The case-level fold joins on `(run_id, test_file)`, so a fixture that could
///   not name a run could not tell "this file's latest run" from "any run".
/// * **`test_name` is deliberately not the path.** `exec_row_at` sets the two
///   equal, which is right for the universe tests and blinding here:
///   `build_stats_map` keys on `test_file` (`:1216`) and a fixture where the two
///   agree passes just as happily against an implementation that keys on
///   `test_name` — the grain `ExecRow`'s header exists to warn against.
/// * **The instant is [`ts`]**, not a literal, so `row()`'s `day` is [`TODAY`]
///   by construction. Six of the chart tests depend on that; a literal would let
///   [`TODAY`] move without them, and the failures would read as broken folds
///   rather than as a stale fixture. [`ts`]'s own doc pairs the two, and
///   [`the_frozen_clock_and_the_row_fixtures_agree_about_today`] asserts the
///   chain end to end.
fn row(run_id: Uuid, test_file: &str, status: &str) -> ExecRow {
    row_at(run_id, test_file, status, ts())
}

/// [`row`] at a chosen instant.
///
/// Separate from `row` because `ExecRow` carries `ts` **and** its derived `day`,
/// so a fixture that overrode `ts` with struct-update syntax would leave `day`
/// pointing at a different date — a fixture inconsistency that no assertion in
/// this module would notice.
fn row_at(run_id: Uuid, test_file: &str, status: &str, at: OffsetDateTime) -> ExecRow {
    ExecRow {
        run_id,
        test_name: format!("display name of {test_file}"),
        ..exec_row_at(test_file, status, at)
    }
}

/// [`row`] on the day `days_ago` before [`TODAY`], keeping the fixture's
/// time of day.
///
/// The day offset rather than a literal date because the window tests are about
/// distance from today, and a literal would have to be recomputed every time
/// [`TODAY`] moves. Whole days keep the time-of-day, so the row's `day` is
/// exactly `TODAY - days_ago` and never slips across midnight.
fn row_on(run_id: Uuid, test_file: &str, status: &str, days_ago: i64) -> ExecRow {
    row_at(run_id, test_file, status, ts() - Duration::days(days_ago))
}

/// One case-level row with no ticket.
fn case(run_id: Uuid, test_file: &str, status: &str) -> CaseRow {
    CaseRow {
        run_id,
        test_file: test_file.to_owned(),
        status: status.to_owned(),
        ticket: None,
    }
}

/// One case-level row carrying a ticket.
fn case_with_ticket(run_id: Uuid, test_file: &str, status: &str, ticket: &str) -> CaseRow {
    CaseRow {
        ticket: Some(ticket.to_owned()),
        ..case(run_id, test_file, status)
    }
}

/// A universe entry whose display name is chosen independently of its path, which
/// is what the list-ordering test needs: `test_support::universe_test` derives the
/// name from the file, so a fixture built with it could not tell a sort by
/// `test_name` from a sort by `test_file`.
fn named(test_file: &str, test_name: &str) -> UniverseTest {
    universe_test_full(test_file, test_name, None)
}

/// The three list names in one place, so a test can assert about the whole
/// partition rather than one arm of it.
fn names(items: &[AnalyticsListItem]) -> Vec<&str> {
    items.iter().map(|item| item.test_name.as_str()).collect()
}

// ---------------------------------------------------------------------------
// The repository key
// ---------------------------------------------------------------------------

/// A product owns several repositories and `(tenant_id, product_id)` is not a
/// unique index, so two repositories can each hold `tests/test_smoke.py`.
/// qa-catalog's `walk_repo_universe` deduplicates only *within* one
/// repository, so the universe then holds two entries sharing that
/// `test_file` — and every fold here must key on `(repo_id, test_file)`, not
/// the path alone, or one repository's row answers for the other's test.
///
/// **No test before this one varied `repo_id`.** Every other fixture over
/// `ExecRow`/`UniverseTest` in this module and in `universe_tests` builds its
/// rows and its universe under one repository
/// (`test_support::UNIVERSE_TEST_REPO_ID`), so a fold keyed on `test_file`
/// alone satisfied every existing assertion by accident — that absence is
/// this defect's whole reason for shipping.
#[test]
fn the_analytics_folds_key_on_repo_id_and_test_file_not_the_path_alone() {
    const SHARED_FILE: &str = "tests/test_smoke.py";

    let repo_a = Uuid::from_u128(0xa1a1);
    let repo_b = Uuid::from_u128(0xb2b2);

    let universe = vec![
        UniverseTest {
            repo_id: repo_a,
            ..named(SHARED_FILE, "test_smoke")
        },
        UniverseTest {
            repo_id: repo_b,
            ..named(SHARED_FILE, "test_smoke")
        },
    ];
    let rows = vec![
        ExecRow {
            repo_id: repo_a,
            ..row_at(run(1), SHARED_FILE, "PASSED", ts())
        },
        ExecRow {
            repo_id: repo_b,
            ..row_at(run(2), SHARED_FILE, "FAILED", ts())
        },
    ];

    let latest = build_latest_map(&universe, &rows);

    assert_eq!(
        latest[&(repo_a, SHARED_FILE.to_owned())].status_bucket,
        "PASSED",
        "repository A's row must report repository A's own status",
    );
    assert_eq!(
        latest[&(repo_b, SHARED_FILE.to_owned())].status_bucket,
        "FAILED",
        "repository B's row must report repository B's own status, not A's",
    );

    let summary = summarize(
        &universe,
        &latest,
        &build_case_data(&universe, &latest, &[]),
    );
    assert_eq!(
        summary.total, 2,
        "two repositories' entries, not one path collapsed into one",
    );
    assert_eq!(summary.passed, 1, "only repository A's test passed");
    assert_eq!(
        summary.failed, 1,
        "only repository B's test failed, not both"
    );
}

/// Fix-round regression: `StatsMap` and `CaseData::by_file` were still keyed
/// on the bare `test_file`, so `build_lists`' `pass_count`/`fail_count` and
/// `case_tickets` attributed one repository's runs and case tickets to the
/// other's test whenever two repositories shared a path — the exact "case
/// counts doubled" symptom the defect names, surviving past this module's
/// first repository-key test because that test never called `build_lists`.
#[test]
fn build_lists_keeps_two_repositories_pass_fail_and_case_signal_separate() {
    const SHARED_FILE: &str = "tests/test_smoke.py";

    let repo_a = Uuid::from_u128(0xa1a1);
    let repo_b = Uuid::from_u128(0xb2b2);

    let universe = vec![
        UniverseTest {
            repo_id: repo_a,
            ..named(SHARED_FILE, "test_smoke")
        },
        UniverseTest {
            repo_id: repo_b,
            ..named(SHARED_FILE, "test_smoke")
        },
    ];
    // Repository A's latest run (`run(1)`) is its first-listed row, `PASSED`;
    // a second row on the same run is `FAILED`, so `StatsMap` — which counts
    // every row rather than picking one — must tally 1 pass/1 fail for A
    // alone. Repository B's latest run (`run(2)`) is two `PASSED` rows.
    let rows = vec![
        ExecRow {
            repo_id: repo_a,
            ..row_at(run(1), SHARED_FILE, "PASSED", ts())
        },
        ExecRow {
            repo_id: repo_a,
            ..row_at(run(1), SHARED_FILE, "FAILED", ts())
        },
        ExecRow {
            repo_id: repo_b,
            ..row_at(run(2), SHARED_FILE, "PASSED", ts())
        },
        ExecRow {
            repo_id: repo_b,
            ..row_at(run(2), SHARED_FILE, "PASSED", ts())
        },
    ];
    let case_rows = vec![
        case_with_ticket(run(1), SHARED_FILE, "FAILED", "REPO-A-1"),
        case_with_ticket(run(2), SHARED_FILE, "PASSED", "REPO-B-1"),
    ];

    let latest = build_latest_map(&universe, &rows);
    let stats = build_stats_map(&rows);
    let cases = build_case_data(&universe, &latest, &case_rows);
    let lists = build_lists(&universe, &latest, &stats, &cases);

    let all_items = lists
        .passed
        .iter()
        .chain(lists.failed.iter())
        .chain(lists.not_run.iter());
    let mut item_a = None;
    let mut item_b = None;
    for item in all_items {
        if item.plan.repo_id == repo_a {
            item_a = Some(item);
        } else if item.plan.repo_id == repo_b {
            item_b = Some(item);
        }
    }
    let item_a = item_a.expect("repository A's item");
    let item_b = item_b.expect("repository B's item");

    assert_eq!(
        (item_a.pass_count, item_a.fail_count),
        (1, 1),
        "repository A's own tally, not the two repositories' rows merged",
    );
    assert_eq!(
        (item_b.pass_count, item_b.fail_count),
        (2, 0),
        "repository B's own tally, not repository A's",
    );
    assert_eq!(
        item_a.case_tickets,
        vec!["REPO-A-1".to_owned()],
        "repository A's own case ticket",
    );
    assert_eq!(
        item_b.case_tickets,
        vec!["REPO-B-1".to_owned()],
        "repository B's own case ticket, not repository A's - a bare `test_file` \
         key made the second repository processed overwrite the first's entry \
         in `CaseData::by_file`",
    );
}

/// Fix-round regression: `build_flaky`'s `by_test` and `universe_by_file` were
/// still keyed on the bare `test_file`, so two repositories sharing a path had
/// their rows tallied into one `StatusStats` and one pass rate — able to hide
/// a genuinely flaky test behind an unrelated repository's clean run, or the
/// reverse.
#[test]
fn build_flaky_keeps_two_repositories_pass_rates_separate() {
    const SHARED_FILE: &str = "tests/test_flaky.py";

    let repo_a = Uuid::from_u128(0xa1a1);
    let repo_b = Uuid::from_u128(0xb2b2);

    let universe = vec![
        UniverseTest {
            repo_id: repo_a,
            ..named(SHARED_FILE, "test_flaky")
        },
        UniverseTest {
            repo_id: repo_b,
            ..named(SHARED_FILE, "test_flaky")
        },
    ];

    let mut rows = Vec::new();
    // Repository A: 4 passed, 1 failed — 80.0, the flaky band's own upper
    // edge, inclusive.
    for status in ["PASSED", "PASSED", "PASSED", "PASSED", "FAILED"] {
        rows.push(ExecRow {
            repo_id: repo_a,
            ..row(run(1), SHARED_FILE, status)
        });
    }
    // Repository B: 5 passed — 100.0, outside the band and not flaky. Merged
    // under a bare `test_file` key this is 9 passed of 10, 90.0 — also
    // outside the band — so repository A's own flakiness would vanish along
    // with the merge, not just get attributed to the wrong repository.
    for status in ["PASSED", "PASSED", "PASSED", "PASSED", "PASSED"] {
        rows.push(ExecRow {
            repo_id: repo_b,
            ..row(run(2), SHARED_FILE, status)
        });
    }

    let flaky = build_flaky(&universe, &rows, 7, TODAY);

    assert_eq!(
        flaky.len(),
        1,
        "only repository A's test is flaky; merging the two repositories' \
         rows would read 90.0 and report neither",
    );
    assert_eq!(
        flaky[0].executions, 5,
        "repository A's own five executions, not the combined ten",
    );
    assert_eq!(flaky[0].pass_count, 4);
    assert_eq!(flaky[0].fail_count, 1);
    assert_eq!(flaky[0].pass_rate, 80.0);
}

/// Fix-round regression: `buckets_by_file_and_day`'s `DayBuckets` was still
/// keyed on the bare `test_file`, so two repositories sharing a path had
/// their day cells folded onto one calendar and the first-wins rule picked
/// one repository's status for both — the same collision
/// [`build_latest_map`](super::universe::build_latest_map) already closes,
/// reached here through the heatmap and the trend instead.
#[test]
fn build_heatmap_keeps_two_repositories_day_buckets_separate() {
    const SHARED_FILE: &str = "tests/test_smoke.py";

    let repo_a = Uuid::from_u128(0xa1a1);
    let repo_b = Uuid::from_u128(0xb2b2);

    let universe = vec![
        UniverseTest {
            repo_id: repo_a,
            ..named(SHARED_FILE, "test_smoke")
        },
        UniverseTest {
            repo_id: repo_b,
            ..named(SHARED_FILE, "test_smoke")
        },
    ];
    let rows = vec![
        ExecRow {
            repo_id: repo_a,
            ..row(run(1), SHARED_FILE, "PASSED")
        },
        ExecRow {
            repo_id: repo_b,
            ..row(run(2), SHARED_FILE, "FAILED")
        },
    ];

    let heat = build_heatmap(&universe, &rows, 1, TODAY);

    assert_eq!(
        heat.rows.len(),
        2,
        "one row per universe entry, not one path collapsed into one",
    );
    assert_eq!(
        heat.rows[0].values,
        vec!["PASSED"],
        "repository A's own day, not repository B's - a bare `test_file` key \
         would have repository A's first-inserted row win both cells",
    );
    assert_eq!(
        heat.rows[1].values,
        vec!["FAILED"],
        "repository B's own day, not repository A's",
    );
}

/// Fix-round regression: `build_test_details`'s `by_file` map was still keyed
/// on the bare `test_file`, so two repositories sharing a path had their
/// universe metadata collapse to one `HashMap` slot — whichever repository's
/// entry `collect` visited last — and both repositories' drill-down rows
/// would render that repository's `component`/`tags`.
#[test]
fn build_test_details_keeps_two_repositories_metadata_separate() {
    const SHARED_FILE: &str = "tests/test_smoke.py";

    let repo_a = Uuid::from_u128(0xa1a1);
    let repo_b = Uuid::from_u128(0xb2b2);

    let universe = vec![
        UniverseTest {
            repo_id: repo_a,
            component: Some("component-a".to_owned()),
            ..named(SHARED_FILE, "test_smoke")
        },
        UniverseTest {
            repo_id: repo_b,
            component: Some("component-b".to_owned()),
            ..named(SHARED_FILE, "test_smoke")
        },
    ];
    let rows = vec![
        ExecRow {
            repo_id: repo_a,
            ..build_row(run(1), SHARED_FILE, "PASSED", Some("9.1"), ts())
        },
        ExecRow {
            repo_id: repo_b,
            ..build_row(run(2), SHARED_FILE, "FAILED", Some("9.1"), ts())
        },
    ];

    let details = build_test_details(&universe, &rows, "9.1");

    assert_eq!(
        details.len(),
        2,
        "one row per repository, not merged into one"
    );
    let item_a = details
        .iter()
        .find(|item| item.status == "PASSED")
        .expect("repository A's item");
    let item_b = details
        .iter()
        .find(|item| item.status == "FAILED")
        .expect("repository B's item");
    assert_eq!(
        item_a.component.as_deref(),
        Some("component-a"),
        "repository A's own component, not repository B's",
    );
    assert_eq!(
        item_b.component.as_deref(),
        Some("component-b"),
        "repository B's own component, not repository A's",
    );
}

// ---------------------------------------------------------------------------
// The file-level summary
// ---------------------------------------------------------------------------

/// `build_summary:1227-1246`: every universe file lands in exactly one of three
/// counters, keyed on its *bucketized* latest status, and a file with **no latest
/// entry at all** reads as `NOT_RUN` (`:1237-1240`, `.unwrap_or("NOT_RUN")`).
///
/// The fixture carries all four cases at once because the third and fourth are
/// the ones an implementation loses: `SKIPPED` buckets to `NOT_RUN` under
/// `bucketize_status:1940` — not to a fourth counter — and a file no row ever
/// touched is counted rather than skipped. Drop the `NOT_RUN` default and
/// `not_run` reads 1; send `SKIPPED` anywhere else and it reads 1 as well.
#[test]
fn every_universe_file_lands_in_exactly_one_of_the_three_file_counters() {
    let universe = vec![
        universe_test("tests/pass.py"),
        universe_test("tests/fail.py"),
        universe_test("tests/skip.py"),
        universe_test("tests/never.py"),
    ];
    let rows = vec![
        row(run(1), "tests/pass.py", "PASSED"),
        row(run(1), "tests/fail.py", "FAILED"),
        row(run(1), "tests/skip.py", "SKIPPED"),
    ];
    let latest = build_latest_map(&universe, &rows);

    let summary = summarize(
        &universe,
        &latest,
        &build_case_data(&universe, &latest, &[]),
    );

    assert_eq!(summary.total, 4);
    assert_eq!(summary.passed, 1);
    assert_eq!(summary.failed, 1);
    assert_eq!(
        summary.not_run, 2,
        "a SKIPPED file and a file with no row at all are both NOT_RUN",
    );
    assert_eq!(summary.passed + summary.failed + summary.not_run, 4);
}

/// `pct:1957-1963`: one decimal place, produced by scaling to tenths, **rounding**
/// and scaling back — and `0.0` rather than a `NaN` when the total is zero.
///
/// `2 / 3` is the discriminating input: the contract yields `66.7`, and the two
/// plausible mutations yield something else. Truncating instead of rounding gives
/// `66.6`; scaling by `100.0` and rounding to a whole percent gives `67.0`.
#[test]
fn percentages_are_rounded_to_one_decimal_and_an_empty_universe_is_zero() {
    assert_eq!(pct(2.0, 3.0), 66.7);
    assert_eq!(pct(1.0, 3.0), 33.3);
    assert_eq!(pct(1.0, 2.0), 50.0);
    assert_eq!(pct(1.0, 0.0), 0.0, "no division by zero, and no NaN");

    let universe = vec![
        universe_test("tests/a.py"),
        universe_test("tests/b.py"),
        universe_test("tests/c.py"),
    ];
    let rows = vec![
        row(run(1), "tests/a.py", "PASSED"),
        row(run(1), "tests/b.py", "PASSED"),
    ];
    let latest = build_latest_map(&universe, &rows);
    let summary = summarize(
        &universe,
        &latest,
        &build_case_data(&universe, &latest, &[]),
    );

    assert_eq!(summary.passed_pct, 66.7);
    assert_eq!(summary.not_run_pct, 33.3);
    assert_eq!(summary.failed_pct, 0.0);

    let empty = summarize(
        &[],
        &HashMap::new(),
        &build_case_data(&[], &HashMap::new(), &[]),
    );
    assert_eq!(empty.total, 0);
    assert_eq!(empty.passed_pct, 0.0);
    assert_eq!(empty.not_run_pct, 0.0);
}

/// `build_summary:1264` leaves `case_expected` at zero, and that is not a stub of
/// this task's: legacy fills it **outside** the fold, in the handler
/// (`:769-779`), from `load_collect_counts` (`:2672-2693`) with
/// `UniverseTest::static_case_count` as the fallback.
/// `domain::analytics::universe::expected_cases` (Task 29) is that fold, and
/// it is called from `AnalyticsService::overview` **after** `summarize`
/// returns — which is why the field is present and zero out of `summarize`
/// itself rather than absent.
///
/// The fixture's three entries each declare `static_case_count: 1`
/// (`test_support::universe_test_full`), so an implementation that "helpfully"
/// summed the static counts inside `summarize` would read 3 here — and then
/// double-count, since `AnalyticsService::overview` adds `expected_cases`'
/// own sum on top of whatever `summarize` already returned. Both halves now
/// exist, so this guards the boundary between them rather than a still-open
/// gap.
#[test]
fn case_expected_stays_zero_out_of_summarize_so_the_service_layer_does_not_double_count() {
    let universe = vec![
        universe_test("tests/a.py"),
        universe_test("tests/b.py"),
        universe_test("tests/c.py"),
    ];
    let latest = build_latest_map(&universe, &[]);

    let summary = summarize(
        &universe,
        &latest,
        &build_case_data(&universe, &latest, &[]),
    );

    assert_eq!(summary.case_expected, 0);
    assert_eq!(summary.total, 3, "the universe is not empty, the field is");
}

// ---------------------------------------------------------------------------
// The per-case summary
// ---------------------------------------------------------------------------

/// `manager/src/routes/analytics.rs:1288`, the `attach_case_data` doc comment:
/// a file with no per-case rows contributes one case of its file-level status,
/// "so totals never undercount". Dropping it makes `case_total` smaller than
/// `total` on any run predating case markers.
#[test]
fn a_file_without_case_rows_contributes_one_case_of_its_file_status() {
    let universe = vec![universe_test("tests/a.py")];
    let latest = build_latest_map(&universe, &[row(run(1), "tests/a.py", "PASSED")]);

    let summary = summarize(
        &universe,
        &latest,
        &build_case_data(&universe, &latest, &[]),
    );

    assert_eq!(summary.case_total, 1);
    assert_eq!(summary.case_passed, 1);
}

/// `effective_case_status:1270`: the worst non-passing case wins, and ERROR
/// folds into FAILED.
///
/// The brief's three assertions are the first three. The rest pin the *order* of
/// the six-candidate list (`:1271`), which the first three do not: `["PASSED",
/// "XFAIL"] -> XFAIL` is satisfied by any order that puts `PASSED` last, and a
/// list reordered to `SKIPPED > XFAIL > XPASS > ERROR > FAILED > PASSED` would
/// pass it.
///
/// `ERROR` needs its **own** rank assertion and not just `ERROR > PASSED`: it is
/// the one candidate whose position is invisible on output, because it is renamed
/// to `FAILED` on the way out (`:1273-1274`). Demote it below `XPASS` and a file
/// holding an `ERROR` case and an `XPASS` case renders `XPASS` — a green-ish
/// signal for an errored file — while every other assertion here still passes.
#[test]
fn the_worst_non_passing_case_status_wins_and_error_folds_into_failed() {
    assert_eq!(
        effective_case_status(&["PASSED".into(), "XFAIL".into()]).as_deref(),
        Some("XFAIL")
    );
    assert_eq!(
        effective_case_status(&["PASSED".into(), "ERROR".into()]).as_deref(),
        Some("FAILED")
    );
    assert_eq!(effective_case_status(&[]), None);

    // FAILED outranks every other candidate, including ERROR — which matters
    // because both fold to the same word and only the order decides which arm
    // produced it.
    assert_eq!(
        effective_case_status(&["XPASS".into(), "FAILED".into(), "SKIPPED".into()]).as_deref(),
        Some("FAILED")
    );
    // XPASS outranks XFAIL, which outranks SKIPPED, which outranks PASSED.
    assert_eq!(
        effective_case_status(&["XPASS".into(), "ERROR".into()]).as_deref(),
        Some("FAILED"),
    );
    assert_eq!(
        effective_case_status(&["XFAIL".into(), "XPASS".into()]).as_deref(),
        Some("XPASS")
    );
    assert_eq!(
        effective_case_status(&["SKIPPED".into(), "XFAIL".into()]).as_deref(),
        Some("XFAIL")
    );
    assert_eq!(
        effective_case_status(&["PASSED".into(), "SKIPPED".into()]).as_deref(),
        Some("SKIPPED")
    );
    // A status outside the six is not a candidate at all, so a file whose only
    // case is unrecognized has no effective status (`:1280`, the fall-through).
    assert_eq!(effective_case_status(&["RUNNING".into()]), None);
}

/// `attach_case_data:1371-1376`, the fallback arm — and the limit of the doc
/// comment's "never undercount".
///
/// The arm counts `PASSED` and `FAILED`. Its third pattern is `"SKIPPED"`, which
/// **cannot match**: `info.status_bucket` is `bucketize_status`' output and that
/// function emits only `PASSED`, `FAILED` and `NOT_RUN` (`:1940-1946`). So a
/// `NOT_RUN` file with no case rows contributes *nothing*, and `case_total` is
/// legitimately smaller than `total`.
///
/// Two universe files, two reasons to contribute nothing: one whose latest row is
/// `SKIPPED` (bucket `NOT_RUN`), one with no row at all (skipped at `:1346-1348`
/// before the bucket is even consulted). Counting either as one case — the
/// obvious "fix" for the undercount — fails this test.
#[test]
fn a_not_run_file_contributes_no_synthetic_case_so_the_undercount_is_real() {
    let universe = vec![
        universe_test("tests/pass.py"),
        universe_test("tests/skip.py"),
        universe_test("tests/never.py"),
    ];
    let rows = vec![
        row(run(1), "tests/pass.py", "PASSED"),
        row(run(1), "tests/skip.py", "SKIPPED"),
    ];
    let latest = build_latest_map(&universe, &rows);

    let summary = summarize(
        &universe,
        &latest,
        &build_case_data(&universe, &latest, &[]),
    );

    assert_eq!(summary.total, 3);
    assert_eq!(
        summary.case_total, 1,
        "only the PASSED file contributes a synthetic case",
    );
    assert_eq!(summary.case_passed, 1);
    assert_eq!(summary.case_skipped, 0, "the SKIPPED arm is unreachable");
}

/// `attach_case_data:1352`: the per-case rows are indexed on `(run, file)` and
/// looked up under the file's **own latest run**, so cases from an older run of
/// the same file are not its cases.
///
/// The fixture gives one file two runs. `build_latest_map` takes the first row it
/// sees (`:1193-1195`), so run 2 is the latest, and the only case rows belong to
/// run 1. Legacy therefore finds nothing and falls back to one synthetic case of
/// the latest bucket, and stamps no `case_status`. An implementation that keyed on
/// `test_file` alone would find run 1's three cases and report
/// `case_total == 3` with `case_status == Some("FAILED")`.
#[test]
fn case_rows_from_an_older_run_are_not_the_files_cases() {
    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![
        row(run(2), "tests/a.py", "PASSED"),
        row(run(1), "tests/a.py", "FAILED"),
    ];
    let latest = build_latest_map(&universe, &rows);
    let case_rows = vec![
        case(run(1), "tests/a.py", "FAILED"),
        case(run(1), "tests/a.py", "PASSED"),
        case(run(1), "tests/a.py", "SKIPPED"),
    ];
    let cases = build_case_data(&universe, &latest, &case_rows);

    let summary = summarize(&universe, &latest, &cases);
    assert_eq!(
        summary.case_total, 1,
        "run 1's cases belong to run 1, not to the file",
    );
    assert_eq!(summary.case_passed, 1);
    assert_eq!(summary.case_failed, 0);

    let lists = build_lists(&universe, &latest, &build_stats_map(&rows), &cases);
    assert_eq!(lists.passed.len(), 1);
    assert_eq!(
        lists.passed[0].case_status, None,
        "no case rows for the latest run means no case signal",
    );
}

/// `attach_case_data:1355-1362` counts five named statuses and `case_total` is
/// their sum (`:1385`), so a status outside the five — `RUNNING`, a lowercase
/// `passed`, anything a future runner invents — is counted **nowhere** and
/// `case_total` is smaller than the number of case rows.
///
/// This is the case-level twin of `bucketize_status`' uppercase-only matching and
/// the reason `domain::service::ingest::classify` must not be reused here:
/// `classify` has a total fold with an every-row counter, so wiring it in would
/// turn this 1 into a 3.
#[test]
fn an_unrecognized_case_status_is_counted_in_no_case_counter() {
    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![row(run(1), "tests/a.py", "PASSED")];
    let latest = build_latest_map(&universe, &rows);
    let case_rows = vec![
        case(run(1), "tests/a.py", "PASSED"),
        case(run(1), "tests/a.py", "RUNNING"),
        case(run(1), "tests/a.py", "passed"),
    ];

    let summary = summarize(
        &universe,
        &latest,
        &build_case_data(&universe, &latest, &case_rows),
    );

    assert_eq!(summary.case_total, 1, "three case rows, one counted");
    assert_eq!(summary.case_passed, 1);
}

/// `attach_case_data:1355-1362`, the five counters, over the full case-level
/// vocabulary — which is **wider** than the file-level one: it adds `XFAIL` and
/// `XPASS`, and it folds `ERROR` into `case_failed` exactly as the file-level
/// buckets do.
///
/// Six rows, five counters, one of each plus the `ERROR` fold. Dropping `ERROR`
/// from the failed arm reads `case_failed == 1` and `case_total == 5`; putting
/// `XPASS` in the passed arm reads `case_passed == 2` and `case_xpass == 0`.
#[test]
fn the_six_case_counters_fold_error_into_failed_and_count_xfail_and_xpass_apart() {
    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![row(run(1), "tests/a.py", "FAILED")];
    let latest = build_latest_map(&universe, &rows);
    let case_rows = vec![
        case(run(1), "tests/a.py", "PASSED"),
        case(run(1), "tests/a.py", "FAILED"),
        case(run(1), "tests/a.py", "ERROR"),
        case(run(1), "tests/a.py", "SKIPPED"),
        case(run(1), "tests/a.py", "XFAIL"),
        case(run(1), "tests/a.py", "XPASS"),
    ];

    let summary = summarize(
        &universe,
        &latest,
        &build_case_data(&universe, &latest, &case_rows),
    );

    assert_eq!(summary.case_passed, 1);
    assert_eq!(summary.case_failed, 2, "ERROR folds into FAILED");
    assert_eq!(summary.case_skipped, 1);
    assert_eq!(summary.case_xfail, 1);
    assert_eq!(summary.case_xpass, 1);
    assert_eq!(summary.case_total, 6);
}

// ---------------------------------------------------------------------------
// The per-test tallies
// ---------------------------------------------------------------------------

/// `build_stats_map:1212-1225`: a three-way tally per **file**, where the third
/// bucket is a catch-all named `skipped_count` — `PASSED`, `FAILED`+`ERROR`, and
/// everything else.
///
/// Five rows on one file and one on another. Dropping `ERROR` from the fail arm
/// reads `fail_count == 1`; treating the catch-all as "unknown" and discarding it
/// reads `skipped_count == 1`; keying on anything but the file merges the two
/// files.
///
/// A file the rows never mention tallies as three zeros, which is legacy's
/// `stats.get(..).cloned().unwrap_or_default()` at `:1408` and the reason
/// `StatsMap::get` is total rather than an `Option`.
#[test]
fn the_per_test_tally_splits_rows_three_ways_and_keys_on_the_file() {
    let rows = vec![
        row(run(1), "tests/a.py", "PASSED"),
        row(run(1), "tests/a.py", "FAILED"),
        row(run(1), "tests/a.py", "ERROR"),
        row(run(1), "tests/a.py", "SKIPPED"),
        row(run(1), "tests/a.py", "XFAIL"),
        row(run(1), "tests/b.py", "PASSED"),
    ];

    let stats = build_stats_map(&rows);

    let a = stats.get(UNIVERSE_TEST_REPO_ID, "tests/a.py");
    assert_eq!(a.pass_count, 1);
    assert_eq!(a.fail_count, 2, "ERROR is a failure here too");
    assert_eq!(
        a.skipped_count, 2,
        "SKIPPED and XFAIL both land in the catch-all"
    );

    let b = stats.get(UNIVERSE_TEST_REPO_ID, "tests/b.py");
    assert_eq!(b.pass_count, 1);
    assert_eq!(b.fail_count, 0);

    let missing = stats.get(UNIVERSE_TEST_REPO_ID, "tests/never.py");
    assert_eq!(missing.pass_count, 0);
    assert_eq!(missing.fail_count, 0);
    assert_eq!(missing.skipped_count, 0);
}

// ---------------------------------------------------------------------------
// The three lists
// ---------------------------------------------------------------------------

/// `build_lists:1397-1449`: one item per universe entry, pushed into the list its
/// **latest bucket** names (`:1433-1437`), and each list sorted by `test_name`
/// ascending (`:1440-1442`).
///
/// The fixture's display names are deliberately anti-sorted relative to the
/// universe order — `zeta` before `alpha` in the `passed` list — so an
/// implementation that forgot to sort, or sorted descending, or sorted by
/// `test_file` instead, produces a different vector. `gamma` has no row at all
/// and lands in `not_run` through `LatestInfo::default` (`:1407`), which is the
/// arm that disappears when a caller iterates the *rows* rather than the
/// universe.
#[test]
fn the_three_lists_split_on_the_latest_bucket_and_each_sorts_by_test_name() {
    // Every path sorts *against* its display name, in both lists that hold more
    // than one entry: `aa.py`/`zz.py` for the passed pair and `yy.py`/`cc.py`
    // for the not-run pair. A fixture whose paths and names agree cannot tell a
    // sort by `test_name` (`:1440-1442`) from a sort by `test_file`.
    let universe = vec![
        named("tests/aa.py", "zeta"),
        named("tests/zz.py", "alpha"),
        named("tests/bb.py", "beta"),
        named("tests/yy.py", "gamma"),
        named("tests/cc.py", "sigma"),
    ];
    let rows = vec![
        row(run(1), "tests/aa.py", "PASSED"),
        row(run(1), "tests/zz.py", "PASSED"),
        row(run(1), "tests/bb.py", "ERROR"),
        row(run(1), "tests/cc.py", "SKIPPED"),
    ];
    let latest = build_latest_map(&universe, &rows);
    let cases = build_case_data(&universe, &latest, &[]);

    let lists = build_lists(&universe, &latest, &build_stats_map(&rows), &cases);

    assert_eq!(names(&lists.passed), vec!["alpha", "zeta"]);
    assert_eq!(
        names(&lists.failed),
        vec!["beta"],
        "ERROR buckets as FAILED"
    );
    assert_eq!(
        names(&lists.not_run),
        vec!["gamma", "sigma"],
        "no row at all and SKIPPED are both NOT_RUN",
    );
    assert_eq!(lists.not_run[0].last_status, NOT_RUN);
    assert_eq!(lists.not_run[0].last_run_id, None);
}

/// The path of the one file both list-item tests are about.
const CARRIED: &str = "tests/carried/test_carried.py";

/// The one fixture behind
/// [`a_list_item_carries_its_tally_its_case_status_and_its_deduplicated_tickets`]
/// and [`a_list_item_carries_every_field_it_renders`], folded all the way to the
/// three lists.
///
/// Shared rather than duplicated because the two tests assert about *the same*
/// item — one about the numbers it computes, one about the values it copies — and
/// two fixtures that could drift would let a field-copy assertion pass against a
/// universe entry the tally assertions never saw.
///
/// # Why every value in it is distinct
///
/// `component`, `tags`, `plan`, `plan_name`, `versions`, `last_environment_id`,
/// `last_build` and `last_run_finished_at` are copied straight across
/// (`:1412-1423`), and a copy is the kind of wiring a test only catches if the
/// fixture can tell the fields apart. So no two same-typed fields share a value:
/// three distinct `Uuid`s (`repo_id`, the latest row's `environment_id`, its
/// `run_id`), six distinct strings, two distinct `Vec<String>`s and two distinct
/// instants. A transposition — `last_environment_id` sourced from `run_id`, `tags`
/// from `versions`, `plan_name` from `component` — fails rather than passing on a
/// coincidence.
///
/// Two properties need more than one row to pin, which is why `run 1` carries
/// three:
///
/// * `last_build`, `last_environment_id` and `last_run_finished_at` must come from
///   the **latest** row (`run 2`), so `run 1`'s rows carry a different build, a
///   different platform and an earlier instant. Sourcing any of them from an older
///   row fails.
/// * the four tallies must count **every** row, including the skipped one.
///
/// And `test_name` must come from the universe entry, not the row: `ExecRow`
/// carries a `test_name` too, and here the two deliberately differ — `ExecRow`'s
/// header records that it is not an aggregation grain.
fn carried_lists() -> AnalyticsLists {
    // Distinct from `exec_row_at`'s `UNIVERSE_TEST_REPO_ID`, and every row below
    // is stamped with it too — `build_latest_map` now joins on `(repo_id,
    // test_file)`, so a universe entry and its rows disagreeing on `repo_id`
    // would simply never meet.
    let repo_id = Uuid::from_u128(0xa1);
    let universe = vec![UniverseTest {
        repo_id,
        plan_path: "plans/carried.yaml".to_owned(),
        plan_name: "Carried plan name".to_owned(),
        component: Some("carried-component".to_owned()),
        tags: vec!["carried-tag-a".to_owned(), "carried-tag-b".to_owned()],
        versions: vec!["9.1".to_owned(), "10.0".to_owned()],
        ..universe_test_full(CARRIED, "carried display name", None)
    }];
    let rows = vec![
        ExecRow {
            repo_id,
            build: Some("9.1.0-4412".to_owned()),
            environment_id: Some(Uuid::from_u128(0xb2)),
            ..row_at(
                run(2),
                CARRIED,
                "FAILED",
                datetime!(2026-08-20 07:30:00 UTC),
            )
        },
        ExecRow {
            repo_id,
            build: Some("8.0.0-1111".to_owned()),
            environment_id: Some(Uuid::from_u128(0xb9)),
            ..row_at(
                run(1),
                CARRIED,
                "PASSED",
                datetime!(2026-08-18 12:00:00 UTC),
            )
        },
        ExecRow {
            repo_id,
            ..row_at(run(1), CARRIED, "ERROR", datetime!(2026-08-18 12:00:00 UTC))
        },
        ExecRow {
            repo_id,
            ..row_at(
                run(1),
                CARRIED,
                "SKIPPED",
                datetime!(2026-08-18 12:00:00 UTC),
            )
        },
    ];
    let latest = build_latest_map(&universe, &rows);
    let case_rows = vec![
        case_with_ticket(run(2), CARRIED, "FAILED", "VHP-9"),
        case_with_ticket(run(2), CARRIED, "PASSED", "VHP-1"),
        case_with_ticket(run(2), CARRIED, "XFAIL", "VHP-9"),
    ];
    let cases = build_case_data(&universe, &latest, &case_rows);

    build_lists(&universe, &latest, &build_stats_map(&rows), &cases)
}

/// `build_lists:1409` — `total_runs` is the **sum of the three tallies**, not the
/// pass and fail counts — together with the case stamp legacy applies at
/// `:1387-1394` and the ticket roll-up at `:1365-1368` (collect, sort, dedup).
///
/// Four rows, one of them skipped, so `total_runs == 4` and any formula that omits
/// `skipped_count` reads 3. The tickets arrive out of order and with a duplicate,
/// so an unsorted or un-deduplicated roll-up reads
/// `["VHP-9", "VHP-1", "VHP-9"]`.
#[test]
fn a_list_item_carries_its_tally_its_case_status_and_its_deduplicated_tickets() {
    let lists = carried_lists();

    assert!(lists.passed.is_empty());
    assert_eq!(lists.failed.len(), 1);
    let item = &lists.failed[0];

    // The tallies (`:1424-1427`).
    assert_eq!(item.pass_count, 1);
    assert_eq!(item.fail_count, 2);
    assert_eq!(item.skipped_count, 1);
    assert_eq!(item.total_runs, 4, "skipped runs are runs");

    // The case stamp (`:1387-1394`) and the ticket roll-up (`:1365-1368`).
    assert_eq!(
        item.case_status.as_deref(),
        Some("FAILED"),
        "the worst of the latest run's three cases",
    );
    assert_eq!(
        item.case_tickets,
        vec!["VHP-1".to_owned(), "VHP-9".to_owned()]
    );
}

/// `build_lists:1412-1423` — the eight fields a list item **copies** rather than
/// computes, plus the two it takes from the universe entry's identity.
///
/// Split from the tally test above rather than folded into it: eighteen
/// assertions in one body trips `clippy::cognitive_complexity`, and the two
/// halves fail for genuinely different reasons — a wrong number means the fold is
/// wrong, a wrong string means the wiring is. Both drive
/// [`carried_lists`], whose header carries the fixture's distinctness argument.
///
/// Every assertion here is a field no other test in this module touches, and all
/// of them are rendered columns: dropping `last_build` to `None`, or sourcing
/// `last_environment_id` from `run_id`, changes the screen and nothing else fails.
#[test]
fn a_list_item_carries_every_field_it_renders() {
    let lists = carried_lists();
    let item = &lists.failed[0];

    // The universe pass-throughs (`:1412-1418`).
    assert_eq!(item.test_file, CARRIED);
    assert_eq!(
        item.test_name, "carried display name",
        "the display name is the universe entry's, not the row's",
    );
    assert_eq!(item.component.as_deref(), Some("carried-component"));
    assert_eq!(
        item.tags,
        vec!["carried-tag-a".to_owned(), "carried-tag-b".to_owned()]
    );
    assert_eq!(item.plan.repo_id, Uuid::from_u128(0xa1));
    assert_eq!(item.plan.plan_path, "plans/carried.yaml");
    assert_eq!(item.plan_name, "Carried plan name");
    assert_eq!(
        item.versions,
        vec!["10.0".to_owned(), "9.1".to_owned()],
        "`sorted_versions_desc` is applied on the way through (`:1418`)",
    );

    // The latest-row pass-throughs (`:1419-1423`), every one of which must come
    // from run 2 rather than from run 1 or from the head of the slice.
    assert_eq!(item.last_status, "FAILED");
    assert_eq!(item.last_run_id, Some(run(2)));
    assert_eq!(item.last_environment_id, Some(Uuid::from_u128(0xb2)));
    assert_eq!(item.last_build.as_deref(), Some("9.1.0-4412"));
    assert_eq!(
        item.last_run_finished_at,
        Some(datetime!(2026-08-20 07:30:00 UTC))
    );
}

/// `build_lists:1418`, `sorted_versions_desc:2164-2186`: trim, drop blanks, sort
/// **descending** comparing dot-separated components **numerically** where both
/// sides parse, then drop consecutive duplicates.
///
/// `10.0` before `9.10` before `9.1` is the discriminating order. A lexicographic
/// descending sort answers `["9.10", "9.1.2", "9.1", "10.0"]`; dropping the
/// `dedup` leaves `9.1` twice; dropping the blank filter leaves an empty string in
/// the output.
///
/// `9.1.2` is in the fixture for one arm the other inputs never reach: the
/// comparator's `unwrap_or_else` fallback (`:2182`), which fires only when the
/// zipped components **all agree** — `zip` truncates to the shorter label, so
/// `9.1.2` against `9.1` compares two equal pairs and falls through to the
/// whole-string comparison. Without such a pair the fallback is dead in the
/// fixture and its swapped arguments — the thing that makes the whole order
/// descending — are unpinned.
///
/// The one input this port ever passes it is empty —
/// `qa_catalog_sdk::UniverseTest::versions` is documented as always empty,
/// matching legacy, whose `UniverseTest::versions` is `Vec::new()` at `:918` and
/// never written again. The function is ported and pinned anyway because it is a
/// cited line of `build_lists` and because that SDK field's doc names it as how
/// the value is rendered; the empty case is asserted so the always-empty path is
/// the one under test too.
#[test]
fn versions_sort_descending_by_numeric_component_and_blanks_are_dropped() {
    let input = vec![
        "9.1".to_owned(),
        "10.0".to_owned(),
        "   ".to_owned(),
        " 9.1 ".to_owned(),
        "9.10".to_owned(),
        "9.1.2".to_owned(),
    ];

    assert_eq!(
        sorted_versions_desc(&input),
        vec![
            "10.0".to_owned(),
            "9.10".to_owned(),
            "9.1.2".to_owned(),
            "9.1".to_owned(),
        ],
    );
    assert!(sorted_versions_desc(&[]).is_empty());
}

// ---------------------------------------------------------------------------
// The heatmap and the trend
// ---------------------------------------------------------------------------

/// Two windows, two clamps. `build_heatmap:1452` is `clamp_days(days, 1, 30)`;
/// `build_trend:1496` is `clamp_days(days, 7, 365)`. Sharing one clamp silently
/// changes both charts.
///
/// The straddle is the middle pair: **90 is a legal trend window and an illegal
/// heatmap one**, so one input with two answers is what a single shared clamp
/// cannot produce. The bounds are asserted on both sides — `30`/`31` and
/// `365`/`366`, `1`/`0` and `7`/`1` — because an inclusive bound written
/// exclusive is off by one day at exactly one input, and an in-range value is
/// asserted to pass through so a clamp replaced by a constant fails.
///
/// Legacy applies each clamp **twice**: once in `normalize_overview_query`
/// (`:2426-2427`, over the defaults `7` and `90`) and again inside the fold. Task
/// 25 owns the first application and calls these same two functions for it —
/// their headers record the defaults. The bounds are identical at both
/// applications and `clamp` is idempotent, so the composition is legacy for every
/// input.
///
/// This test is about the two *functions*; which function each fold reaches for is
/// [`each_fold_applies_its_own_clamp`], and it needs its own fixtures — see there.
#[test]
fn the_heatmap_and_trend_windows_clamp_differently() {
    assert_eq!(heatmap_days(0), 1);
    assert_eq!(heatmap_days(90), 30);
    assert_eq!(trend_days(1), 7);
    assert_eq!(trend_days(1000), 365);

    // One input, two answers.
    assert_eq!(trend_days(90), 90);

    // Both bounds inclusive, and an in-range value passed through.
    assert_eq!(heatmap_days(1), 1);
    assert_eq!(heatmap_days(7), 7);
    assert_eq!(heatmap_days(30), 30);
    assert_eq!(heatmap_days(31), 30);
    assert_eq!(trend_days(7), 7);
    assert_eq!(trend_days(365), 365);
    assert_eq!(trend_days(366), 365);
}

/// Each fold applies **its own** clamp, asserted through the fold with day counts
/// **outside** the range.
///
/// # Why the sibling test above cannot cover this
///
/// It exercises [`heatmap_days`] and [`trend_days`] as functions. Every other
/// fold call in this module passes an in-range `days` — 3, 2 and 1 for the
/// heatmap, 7 for the trend — and three mutations survive that:
///
/// * **`build_trend` calling [`heatmap_days`]**: `heatmap_days(7) ==
///   trend_days(7) == 7`, so nothing fails. In production `days_trend` defaults
///   to `90` (`:2427`), so the chart would render 30 points where legacy renders
///   90 — a wrong rendered number under a green suite.
/// * **either fold dropping its clamp entirely**: with an in-range `days` a clamp
///   is the identity, so nothing fails.
/// * only `build_heatmap` borrowing the trend's clamp is caught elsewhere, and
///   only because `3 != 7`.
///
/// So the fixtures here straddle both bounds of both ranges, which is the standard
/// this task's own clamp rule is held to. `dashboard_tests`'
/// `the_callers_day_count_is_clamped_and_governs_the_trend_length` is the
/// precedent: it asserts out-of-range inputs through the caller rather than
/// against the clamp helper.
///
/// The universe is empty because only the axis length is under test, and an empty
/// universe is a legal input (`CatalogReader::list_universe`'s header: an unsynced
/// repository contributes nothing).
#[test]
fn each_fold_applies_its_own_clamp() {
    assert_eq!(build_heatmap(&[], &[], 90, TODAY).days.len(), 30);
    assert_eq!(build_heatmap(&[], &[], 0, TODAY).days.len(), 1);
    assert_eq!(build_trend(&[], &[], 1, TODAY).points.len(), 7);
    assert_eq!(build_trend(&[], &[], 400, TODAY).points.len(), 365);
}

/// The day axis is `days` dates **ending today, oldest first**
/// (`recent_days:2077-2082`, `today - (days - 1 - offset)`), and both charts
/// label themselves from it (`build_heatmap:1466-1469`,
/// `build_trend:1526`).
///
/// Asserted against literal dates rather than against `recent_days` itself,
/// which would be tautological — both folds call it. What that catches: a
/// reversed axis (newest first renders the chart backwards), and the off-by-one
/// in `days - 1` (a window of three ending the 18th opens on the **16th**, and
/// `days` instead of `days - 1` would open it on the 15th and drop today).
#[test]
fn the_day_axis_runs_oldest_first_and_ends_today() {
    assert_eq!(
        recent_days(TODAY, 3),
        [date!(2026 - 08 - 16), date!(2026 - 08 - 17), TODAY],
    );
    assert_eq!(recent_days(TODAY, 1), [TODAY]);

    let heat = build_heatmap(&[universe_test("tests/a.py")], &[], 3, TODAY);
    assert_eq!(
        heat.days,
        [date!(2026 - 08 - 16), date!(2026 - 08 - 17), TODAY],
    );

    let trend = build_trend(&[universe_test("tests/a.py")], &[], 7, TODAY);
    assert_eq!(trend.points.len(), 7);
    assert_eq!(trend.points[0].day, date!(2026 - 08 - 12));
    assert_eq!(trend.points[6].day, TODAY);
}

/// A cell with no run for that day renders the literal `NOT_RUN`
/// (`build_heatmap:1476-1479`, the `unwrap_or_else`), not an empty string and not
/// a missing element — the row's length must equal the day count.
///
/// The empty-day rendering differs between the two charts and this is the
/// heatmap's half: here it is a cell value, on the trend it is a `not_run`
/// **counter** (see
/// [`every_trend_point_totals_the_universe_size`]). An implementation that
/// pushed nothing for a missing day would produce a shorter row than `days`, and
/// the chart would silently shift every cell one column left.
#[test]
fn a_day_without_a_run_renders_not_run() {
    let heat = build_heatmap(&[universe_test("tests/a.py")], &[], 3, TODAY);

    assert_eq!(heat.days.len(), 3);
    assert_eq!(heat.rows.len(), 1);
    assert_eq!(heat.rows[0].values, [NOT_RUN, NOT_RUN, NOT_RUN]);
}

/// `bucketize_status:1940` collapses to three values: `PASSED`; `FAILED` (with
/// `ERROR` folded in); `NOT_RUN` for **everything else, including `SKIPPED`**. A
/// skipped test reads as not-run on both charts, which is legacy behavior and not
/// a bug to fix here.
///
/// # Both charts, not the function
///
/// The brief writes this test as four calls to `bucketize_status` itself. Those
/// are already pinned, by Task 20's
/// `only_the_uppercase_spellings_bucket_and_skipped_is_not_run`
/// (`universe_tests.rs:444-452`), so this is the same rule asserted where it can
/// still be lost: **at the two folds**, which is ruling R5's actual risk. A fold
/// that reached for `domain::service::ingest::classify` instead would put this
/// row in a `Skipped` bucket the heatmap has no colour for and the trend has no
/// counter for, and nothing else in this crate would fail.
///
/// The distinction the brief flags is visible here too: this cell reads
/// `NOT_RUN` for a run that *happened*, exactly as
/// [`a_day_without_a_run_renders_not_run`]'s cell does for a day that had none.
/// The two are indistinguishable on the chart, in legacy as here.
///
/// # The trailing `PASSED` row is what makes the fixture able to fail
///
/// With a `SKIPPED` row alone, an implementation that *dropped* the row before
/// the map — never bucketizing it at all — produces the same cell and the same
/// counters as one that bucketized it to `NOT_RUN`. The two are not equivalent:
/// dropping it frees the `(file, day)` slot, so the next row for that pair wins.
/// The second row makes that visible — the cell must **stay** `NOT_RUN`, because
/// `SKIPPED` arrived first and consumed the slot (`:1462`).
#[test]
fn skipped_buckets_as_not_run() {
    let universe = [universe_test("tests/a.py")];
    let rows = [
        row(run(1), "tests/a.py", "SKIPPED"),
        row(run(2), "tests/a.py", "PASSED"),
    ];

    let heat = build_heatmap(&universe, &rows, 1, TODAY);
    assert_eq!(
        heat.rows[0].values,
        [NOT_RUN],
        "SKIPPED bucketizes to NOT_RUN and keeps the slot; it is not dropped",
    );

    let trend = build_trend(&universe, &rows, 7, TODAY);
    let today = trend.points.last().expect("the window ends today");
    assert_eq!(
        (today.passed, today.failed, today.not_run),
        (0, 0, 1),
        "a SKIPPED row is a not-run test on the trend, not a fourth counter",
    );
}

/// `build_heatmap:1462` uses `or_insert_with`, so for a given (file, day) the
/// **first row encountered wins** — not the worst status, and not the latest.
/// Row order therefore matters, and a reordering of the input changes the chart.
///
/// # Both orders, and both charts
///
/// One order cannot pin this. An implementation that took the *worst* status, or
/// that preferred `PASSED`, or that took the last row, agrees with the
/// first assertion and disagrees with the second — so the reversed input is the
/// assertion that does the work.
///
/// The trend is asserted too because this port folds the map **once** and shares
/// it, where legacy writes the same nine lines twice (`:1456-1464` and
/// `:1500-1508`, character for character). Sharing is only faithful while the
/// two agree, so the shared fold is pinned from both sides.
///
/// The incoming order is `ResultsRepository::list_for_universe`'s: a sort key
/// descending, then `created_at DESC, ingest_ordinal DESC, id DESC`. The sort key
/// is `run_finished_at` alone on the `finished_only` path and
/// `COALESCE(run_finished_at, run_created_at)` otherwise
/// (`infra::storage::results_sea_repo`'s `sort_key`, `:232-238`) — the two agree
/// on that path, which is why the rewrite is allowed. Either way "first" means
/// "newest" in production, but only because of the query, so this fold is pinned
/// on order alone.
#[test]
fn the_first_row_for_a_file_and_day_wins() {
    let universe = [universe_test("tests/a.py")];
    let passed_first = [
        row(run(1), "tests/a.py", "PASSED"),
        row(run(2), "tests/a.py", "FAILED"),
    ];
    let failed_first = [
        row(run(2), "tests/a.py", "FAILED"),
        row(run(1), "tests/a.py", "PASSED"),
    ];

    let heat = build_heatmap(&universe, &passed_first, 1, TODAY);
    assert_eq!(heat.rows[0].values, ["PASSED"]);

    let reversed = build_heatmap(&universe, &failed_first, 1, TODAY);
    assert_eq!(reversed.rows[0].values, ["FAILED"]);

    let trend = build_trend(&universe, &failed_first, 7, TODAY);
    let today = trend.points.last().expect("the window ends today");
    assert_eq!(
        (today.passed, today.failed),
        (0, 1),
        "the trend must read the same first-wins map as the heatmap",
    );
}

/// The trend counts over the **whole universe** each day, so a test that never
/// ran still contributes to `not_run` (`build_trend:1512-1523`). Totals per
/// point therefore always equal the universe size.
///
/// # Three distinct counters and one row that must not count
///
/// The counters are `1`, `2` and `3` rather than three ones: `passed`, `failed`
/// and `not_run` are three `usize` fields and a transposition between any two
/// would compile and would pass a fixture where they are equal. The `2` also
/// carries `ERROR`, which folds into `failed`.
///
/// `tests/z.py` is **not in the universe** and is the one row that must move no
/// counter. That is not decoration: legacy counts by looking each *universe*
/// entry up in the map (`:1516-1523`), and an implementation that iterated the
/// **map** instead — the shorter way to write it — would count `tests/z.py` and
/// report a total of seven over a universe of six.
#[test]
fn every_trend_point_totals_the_universe_size() {
    let universe = [
        universe_test("tests/a.py"),
        universe_test("tests/b.py"),
        universe_test("tests/c.py"),
        universe_test("tests/d.py"),
        universe_test("tests/e.py"),
        universe_test("tests/f.py"),
    ];
    let rows = [
        row(run(1), "tests/a.py", "PASSED"),
        row(run(2), "tests/b.py", "FAILED"),
        row(run(3), "tests/c.py", "ERROR"),
        row(run(4), "tests/z.py", "PASSED"),
    ];

    let trend = build_trend(&universe, &rows, 7, TODAY);
    let today = trend.points.last().expect("the window ends today");

    assert_eq!(today.passed, 1);
    assert_eq!(today.failed, 2, "ERROR folds into failed");
    assert_eq!(today.not_run, 3);
    assert_eq!(today.passed + today.failed + today.not_run, universe.len());
    assert!(
        trend.points[..6]
            .iter()
            .all(|point| (point.passed, point.failed, point.not_run) == (0, 0, 6)),
        "every earlier day is the whole universe as not-run: {trend:?}",
    );
}

/// A row is placed on **its own day** and a row outside the window is placed
/// nowhere. `build_heatmap:1461-1463` keys the map on `(row.test_file, row.day)`
/// and looks it up per axis day; `row.day` is
/// `COALESCE(run_finished_at, run_created_at)`'s calendar day, already derived by
/// the repository.
///
/// What it catches: an implementation that stamped every row on `today` (the
/// cells would read `[NOT_RUN, "PASSED"]` instead), and one that ignored the
/// window and let an older row light up today's cell.
#[test]
fn a_row_lands_on_its_own_day_and_not_on_today() {
    let universe = [universe_test("tests/a.py")];
    let rows = [row_on(run(1), "tests/a.py", "PASSED", 1)];

    let heat = build_heatmap(&universe, &rows, 2, TODAY);
    assert_eq!(heat.rows[0].values, ["PASSED", NOT_RUN]);

    let narrowed = build_heatmap(&universe, &rows, 1, TODAY);
    assert_eq!(
        narrowed.rows[0].values,
        [NOT_RUN],
        "a one-day window ends and starts today, so yesterday's row is outside it",
    );

    let trend = build_trend(&universe, &rows, 7, TODAY);
    let day = |wanted| {
        trend
            .points
            .iter()
            .find(|point| point.day == wanted)
            .expect("the day is on the axis")
    };
    assert_eq!(
        (day(TODAY - Duration::DAY).passed, day(TODAY).passed),
        (1, 0)
    );
}

/// One heatmap row per **universe entry**, in the universe's own order and with
/// no de-duplication (`build_heatmap:1472-1487`, a plain `for test in universe`).
///
/// Two mutations, and the first is the likely one: the sibling fold in this file
/// **does** sort — `build_lists` sorts each list by `test_name` (`:1440-1442`) —
/// so a sort added here for symmetry would reorder every chart's y-axis and no
/// other test would notice.
///
/// **The second used to be described as "a file listed by two plans is two
/// universe entries and therefore two identical rows, the same grain
/// `summarize`'s `total` counts twice" — that reading was wrong.** Two plans of
/// the *same* repository listing one file never reach this fold as two
/// entries: qa-catalog's `walk_repo_universe` merges the second plan's
/// contribution into the first before the universe exists. The fixture below
/// hands `build_heatmap` two identical entries directly, bypassing that merge,
/// to pin a narrower and still-real property: this fold has no de-duplication
/// pass of its own and trusts its input, so *if* it were ever handed a universe
/// already holding a repeated entry, it would emit a repeated row rather than
/// collapsing it silently.
#[test]
fn the_heatmap_emits_one_row_per_universe_entry_in_the_universes_order() {
    let universe = [
        named("tests/b.py", "beta"),
        named("tests/a.py", "alpha"),
        named("tests/b.py", "beta"),
    ];

    let heat = build_heatmap(&universe, &[], 1, TODAY);

    assert_eq!(
        heat.rows
            .iter()
            .map(|heat_row| heat_row.test_name.as_str())
            .collect::<Vec<_>>(),
        ["beta", "alpha", "beta"],
    );
}

/// A heatmap row carries the universe entry's **file** and its **display name**,
/// which are two `String` fields a transposition would swap without failing to
/// compile (`build_heatmap:1483-1485`).
///
/// The two fixture values share no substring for that reason. This is the
/// field-copy assertion Task 21a's review found missing from its own mutation
/// sweep; `HeatmapRow` has exactly one such pair and `TrendPoint` has none.
#[test]
fn a_heatmap_row_carries_its_file_and_its_display_name() {
    let universe = [universe_test_full("tests/a.py", "the alpha suite", None)];

    let heat = build_heatmap(&universe, &[], 1, TODAY);

    assert_eq!(heat.rows[0].test_file, "tests/a.py");
    assert_eq!(heat.rows[0].test_name, "the alpha suite");
}

/// The frozen clock and the row builders agree about today.
///
/// `domain::ports::clock` exists because `recent_days:2078` and
/// `build_flaky:1657` both read `Utc::now().date_naive()` inside the fold. The
/// port's production adapter is `infra::clock::SystemClock` and its double is
/// `test_support::FixedClock`; the folds take the `Date` it returns, so this is
/// the one test in this module that goes through the trait.
///
/// It also pins the fixture invariant, which is the failure Task 21b's review
/// named in reverse: `test_support::TODAY` and `test_support::ts()` are two
/// constants that **must** denote the same day. If they drift, every row built by
/// `exec_row_at` falls off the end of every window and the failures read as
/// broken folds rather than as a stale fixture.
#[test]
fn the_frozen_clock_and_the_row_fixtures_agree_about_today() {
    assert_eq!(FixedClock::default().today(), TODAY);
    assert_eq!(ts().date(), TODAY);
    assert_eq!(
        row(run(1), "tests/a.py", "PASSED").day,
        TODAY,
        "every row built by `row()` must land on the last column of every window",
    );

    let heat = build_heatmap(
        &[universe_test("tests/a.py")],
        &[exec_row_at("tests/a.py", "PASSED", ts())],
        1,
        FixedClock::default().today(),
    );
    assert_eq!(heat.rows[0].values, ["PASSED"]);
}

// ---------------------------------------------------------------------------
// The flaky detector, the quality vectors and the grouped summaries (Task 23)
// ---------------------------------------------------------------------------

/// One row per status, all of them dated [`TODAY`], for `test_file`.
///
/// The brief's `exec_rows`. Every row lands on the same day on purpose: the
/// execution-count and band gates are day-blind, and the window tests below place
/// their own rows with [`row_on`] so that a fixture cannot pass a window rule by
/// accident.
fn exec_rows(test_file: &str, statuses: &[&str]) -> Vec<ExecRow> {
    statuses
        .iter()
        .map(|status| exec_row_at(test_file, status, ts()))
        .collect()
}

/// `passed` passing rows and `failed` failing ones, dated [`TODAY`].
///
/// Counted rather than spelled because the band test needs pass rates a
/// five-element fixture cannot express: `39.9` and `80.1` are the *nearest
/// representable* values outside `[40.0, 80.0]` under [`pct`]'s one-decimal
/// rounding, and reaching them takes a thousand rows.
fn exec_rows_counts(test_file: &str, passed: usize, failed: usize) -> Vec<ExecRow> {
    let mut statuses = vec!["PASSED"; passed];
    statuses.extend(vec!["FAILED"; failed]);
    exec_rows(test_file, &statuses)
}

/// A universe entry with a chosen component and tag set.
///
/// `test_support::universe_test` leaves both empty, which is right for the
/// latest-map tests and blinding for the grouped summaries: those two fields
/// *are* the group keys.
fn grouped(test_file: &str, component: Option<&str>, tags: &[&str]) -> UniverseTest {
    UniverseTest {
        component: component.map(str::to_owned),
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        ..universe_test(test_file)
    }
}

/// A universe entry declaring `vectors` as its `TEST_META` quality vectors.
fn vectored(test_file: &str, vectors: &[&str]) -> UniverseTest {
    UniverseTest {
        quality_vectors: vectors.iter().map(|item| (*item).to_owned()).collect(),
        ..universe_test(test_file)
    }
}

/// One row dated [`TODAY`], attributed to `environment_id`.
///
/// `exec_row_at` stamps one fixed platform on every row, which would make every
/// platform-group test a single-group test.
fn platform_row(test_file: &str, status: &str, environment_id: Option<Uuid>) -> ExecRow {
    ExecRow {
        environment_id,
        ..exec_row_at(test_file, status, ts())
    }
}

/// Two platform ids, distinct from `exec_row_at`'s so a fixture that forgot to
/// override one is visible.
const PLATFORM_A: Uuid = Uuid::from_u128(0xa1);
const PLATFORM_B: Uuid = Uuid::from_u128(0xb2);

/// `build_flaky:1655`, all four gates in one place:
///
/// * the window is `trend_days` clamped to `[7, 365]`, cutoff `today - (days-1)`;
/// * a test needs **at least 5 executions** (`:1682`, `if executions < 5`);
/// * its pass rate must fall **inside `[40.0, 80.0]` inclusive** (`:1685`) —
///   a test that always fails is broken, not flaky, and is deliberately excluded;
/// * `pct:1957` rounds to one decimal: `((v/total)*1000).round()/10`.
///
/// The brief's test, with legacy's `Utc::now()` passed in as `today` per the
/// standing ruling that the folds take a `Date`. What each third catches: raise
/// the floor to six and the first case empties; drop the floor and the second
/// case appears; widen the band's floor to `0.0` and the third appears.
#[test]
fn flaky_requires_five_executions_and_a_pass_rate_inside_the_band() {
    // 3 of 5 passed = 60.0% — inside the band, at the execution floor.
    let flaky = build_flaky(
        &[universe_test("tests/a.py")],
        &exec_rows(
            "tests/a.py",
            &["PASSED", "PASSED", "PASSED", "FAILED", "FAILED"],
        ),
        7,
        TODAY,
    );
    assert_eq!(flaky.len(), 1);
    assert_eq!(flaky[0].pass_rate, 60.0);
    assert_eq!(flaky[0].executions, 5);

    // 2 of 4 passed = 50.0%, inside the band but one execution short.
    let too_few = build_flaky(
        &[universe_test("tests/a.py")],
        &exec_rows("tests/a.py", &["PASSED", "PASSED", "FAILED", "FAILED"]),
        7,
        TODAY,
    );
    assert!(
        too_few.is_empty(),
        "four executions is below the floor of five"
    );

    // 0 of 5 passed = 0.0% — outside the band. Consistently broken, not flaky.
    let always_failing = build_flaky(
        &[universe_test("tests/a.py")],
        &exec_rows("tests/a.py", &["FAILED"; 5]),
        7,
        TODAY,
    );
    assert!(always_failing.is_empty(), "0% is outside [40, 80]");
}

/// `:1685-1688` is `(40.0..=80.0).contains(&pass_rate)` — **inclusive** at both
/// ends.
///
/// Four fixtures straddling the two boundaries, and the outside pair is the
/// *nearest representable* one rather than a comfortable 30/90: `39.9` and `80.1`
/// are one tenth of a percent out, which is the granularity [`pct`] produces. A
/// half-open `40.0..80.0` keeps `40.0` and drops `80.0`; an exclusive
/// `(40.0, 80.0)` drops both; either reads as a rounding quirk against a fixture
/// sitting at 60.
#[test]
fn the_flaky_band_is_inclusive_at_both_ends() {
    let rates = |passed: usize, failed: usize| {
        build_flaky(
            &[universe_test("tests/a.py")],
            &exec_rows_counts("tests/a.py", passed, failed),
            7,
            TODAY,
        )
    };

    let at_floor = rates(2, 3);
    assert_eq!(at_floor.len(), 1, "40.0 is inside the band");
    assert_eq!(at_floor[0].pass_rate, 40.0);

    let at_ceiling = rates(4, 1);
    assert_eq!(at_ceiling.len(), 1, "80.0 is inside the band");
    assert_eq!(at_ceiling[0].pass_rate, 80.0);

    assert!(rates(399, 601).is_empty(), "39.9 is below the band");
    assert!(rates(801, 199).is_empty(), "80.1 is above the band");
}

/// `:1705-1710`: ascending pass rate, then **descending** executions as the
/// tiebreak — the flakiest first, and among equally flaky ones the
/// best-evidenced first.
///
/// Three tests, two of them sharing a pass rate with different evidence, so the
/// whole order is pinned rather than one inequality: reverse the primary key and
/// the list starts at 80.0, reverse the tiebreak and the two 50.0s swap. The
/// input is a `HashMap` fold, so an implementation that forgot to sort at all
/// fails intermittently rather than never — the full-order assertion makes it
/// fail every time.
#[test]
fn flaky_sorts_by_pass_rate_then_by_evidence() {
    let universe = vec![
        universe_test("tests/thin.py"),
        universe_test("tests/thick.py"),
        universe_test("tests/green.py"),
    ];
    let mut rows = exec_rows_counts("tests/thin.py", 3, 3);
    rows.extend(exec_rows_counts("tests/thick.py", 6, 6));
    rows.extend(exec_rows_counts("tests/green.py", 8, 2));

    let flaky = build_flaky(&universe, &rows, 7, TODAY);

    let order: Vec<(&str, f64, u32)> = flaky
        .iter()
        .map(|item| (item.test_file.as_str(), item.pass_rate, item.executions))
        .collect();
    assert_eq!(
        order,
        vec![
            ("tests/thick.py", 50.0, 12),
            ("tests/thin.py", 50.0, 6),
            ("tests/green.py", 80.0, 10),
        ],
        "ascending pass rate, then descending executions"
    );
}

/// `:1690` — a test with rows but no universe entry is dropped. Rows outlive
/// deleted test files, and a flaky list naming files that no longer exist is
/// noise.
#[test]
fn a_test_absent_from_the_universe_is_not_reported_flaky() {
    let flaky = build_flaky(
        &[],
        &exec_rows(
            "tests/gone.py",
            &["PASSED", "FAILED", "PASSED", "FAILED", "PASSED"],
        ),
        7,
        TODAY,
    );
    assert!(flaky.is_empty());
}

/// `:1668-1674`: PASSED counts as a pass, FAILED **and ERROR** as a fail, and
/// everything else — SKIPPED, XFAIL, XPASS — as skipped. All three feed the
/// execution count, so a mostly-skipped test can reach the floor.
#[test]
fn flaky_folds_error_into_fail_and_everything_else_into_skipped() {
    let flaky = build_flaky(
        &[universe_test("tests/a.py")],
        &exec_rows(
            "tests/a.py",
            &["PASSED", "PASSED", "ERROR", "SKIPPED", "XFAIL"],
        ),
        7,
        TODAY,
    );
    assert_eq!(flaky.len(), 1);
    assert_eq!(flaky[0].pass_count, 2);
    assert_eq!(flaky[0].fail_count, 1);
    assert_eq!(flaky[0].skipped_count, 2);
    assert_eq!(
        flaky[0].pass_rate, 40.0,
        "2/5 = 40.0, the band's lower edge"
    );
}

/// The flaky split and [`build_stats_map`]'s are **one** classification, shared
/// through `tally` — legacy writes the same match twice (`:1218-1222`,
/// `:1667-1671`).
///
/// Asserted from the outside, over a status set that exercises all three arms, so
/// a future edit that re-forks the two is a failure rather than a latent drift.
/// The whole reason the shared helper exists.
#[test]
fn flaky_and_the_per_test_tally_split_a_status_the_same_way() {
    let rows = exec_rows(
        "tests/a.py",
        &[
            "PASSED", "PASSED", "PASSED", "PASSED", "ERROR", "FAILED", "SKIPPED", "XPASS",
        ],
    );
    let flaky = build_flaky(&[universe_test("tests/a.py")], &rows, 7, TODAY);
    let stats = build_stats_map(&rows).get(UNIVERSE_TEST_REPO_ID, "tests/a.py");

    assert_eq!(flaky.len(), 1, "4 of 8 is 50.0, inside the band");
    assert_eq!(flaky[0].executions, 8, "all three arms feed the count");
    assert_eq!(flaky[0].pass_count, stats.pass_count);
    assert_eq!(flaky[0].fail_count, stats.fail_count);
    assert_eq!(flaky[0].skipped_count, stats.skipped_count);
}

/// `flaky_cutoff` is the **trend** window's first day (`:1656-1657`), and the
/// clamp is applied inside the fold — not only inside `trend_days`.
///
/// The Task 22 lesson, applied here: every value passed to [`build_flaky`] below
/// is **outside** `[7, 365]`, so a fold that forgot to clamp — or that reached for
/// [`heatmap_days`] instead — renders a different window. `days = 1` under the
/// heatmap's clamp is a one-day window; under the trend's it is seven.
#[test]
fn the_flaky_cutoff_is_the_trend_windows_first_day() {
    for days in [0usize, 1, 6, 400, 10_000] {
        assert_eq!(
            flaky_cutoff(TODAY, days),
            recent_days(TODAY, trend_days(days))[0],
            "the cutoff is the trend window's oldest day"
        );
    }
    assert_eq!(flaky_cutoff(TODAY, 1), TODAY - Duration::days(6));
    assert_eq!(flaky_cutoff(TODAY, 400), TODAY - Duration::days(364));
    assert_ne!(
        flaky_cutoff(TODAY, 1),
        recent_days(TODAY, heatmap_days(1))[0],
        "the heatmap's clamp would make this a one-day window"
    );
}

/// The window drops rows **older** than the cutoff and keeps the cutoff day
/// itself (`:1661-1663`, `if row.day < cutoff`).
///
/// `days = 1` clamps up to 7, so the cutoff is `TODAY - 6`. Five rows on the
/// cutoff day reach the floor; move the sixth one day further back and it stops
/// contributing, which is the off-by-one an `<=` would introduce.
#[test]
fn the_flaky_window_keeps_the_cutoff_day_and_drops_the_day_before() {
    let universe = vec![universe_test("tests/a.py")];
    let mut rows = vec![
        row_on(run(1), "tests/a.py", "PASSED", 6),
        row_on(run(1), "tests/a.py", "PASSED", 6),
        row_on(run(1), "tests/a.py", "FAILED", 6),
        row_on(run(1), "tests/a.py", "FAILED", 6),
        row_on(run(1), "tests/a.py", "FAILED", 6),
    ];
    let inside = build_flaky(&universe, &rows, 1, TODAY);
    assert_eq!(inside.len(), 1, "the cutoff day is inside the window");
    assert_eq!(inside[0].executions, 5);
    assert_eq!(inside[0].pass_rate, 40.0);

    rows.push(row_on(run(1), "tests/a.py", "PASSED", 7));
    let still_five = build_flaky(&universe, &rows, 1, TODAY);
    assert_eq!(
        still_five[0].executions, 5,
        "the day before the cutoff contributes nothing"
    );
    assert_eq!(still_five[0].pass_rate, 40.0);
}

/// The flaky filter is **one-sided** where the charts' is a closed set: a row
/// dated *after* today is flaky evidence (`:1661-1663`, only `row.day < cutoff`
/// is dropped) and is invisible to [`build_trend`] (`:1500-1508`,
/// `date_set.contains`).
///
/// Not theoretical: the row's day comes from the run's timestamp and the cutoff
/// from this gear's clock, so any skew between the two produces one. Implementing
/// the cutoff as `window.contains(row.day)` — the obvious reuse, since the window
/// is already computed — silently drops it here, and this is the only test that
/// can tell the two apart.
#[test]
fn a_future_dated_row_is_flaky_evidence_but_not_chart_data() {
    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![
        row_on(run(1), "tests/a.py", "PASSED", -1),
        row_on(run(1), "tests/a.py", "PASSED", 0),
        row_on(run(1), "tests/a.py", "FAILED", 0),
        row_on(run(1), "tests/a.py", "FAILED", 0),
        row_on(run(1), "tests/a.py", "FAILED", 0),
    ];

    let flaky = build_flaky(&universe, &rows, 7, TODAY);
    assert_eq!(flaky.len(), 1, "five executions, tomorrow's row included");
    assert_eq!(flaky[0].executions, 5);
    assert_eq!(flaky[0].pass_count, 2);

    let trend = build_trend(&universe, &rows, 7, TODAY);
    assert_eq!(
        trend.points.len(),
        7,
        "the trend axis ends today and has no column for tomorrow"
    );
    assert!(
        trend.points.iter().all(|point| point.day <= TODAY),
        "no chart column is dated after today"
    );
}

/// The analytics flaky list is **not** truncated (`:1711` returns the whole
/// `Vec`), where the dashboard's flaky query is `LIMIT 10`
/// (`dashboard.rs:399`).
///
/// Twelve qualifying tests come back as twelve. The two folds are different
/// surfaces with different grains and different classifications — [`build_flaky`]'s
/// header tabulates the difference — and borrowing the dashboard's cap would
/// silently shorten a list legacy renders whole.
#[test]
fn the_flaky_list_is_not_truncated() {
    let files: Vec<String> = (0..12).map(|n| format!("tests/t{n}.py")).collect();
    let universe: Vec<UniverseTest> = files
        .iter()
        .map(|file| universe_test(file.as_str()))
        .collect();
    let rows: Vec<ExecRow> = files
        .iter()
        .flat_map(|file| exec_rows_counts(file.as_str(), 3, 2))
        .collect();

    assert_eq!(build_flaky(&universe, &rows, 7, TODAY).len(), 12);
}

/// The four catalog-sourced fields are copied off the matching [`UniverseTest`]
/// (`:1692-1695`) and the five counters off the tally.
///
/// The field-copy family: **all nine values are distinct** — `50.0`, `6`, `3`,
/// `2`, `1`, a path, a display name, a component and a two-element tag list — and
/// the display name is deliberately not the path, so a transposition between
/// `test_file` and `test_name`, or between `pass_count` and `fail_count`, cannot
/// pass. (The fixture was `PASSED, PASSED, FAILED, ERROR, SKIPPED` until Task
/// 23's fix round, which put `2` in both counters and left that second
/// same-typed adjacent pair unpinned by this test while its doc claimed
/// otherwise.)
#[test]
fn a_flaky_entry_carries_every_field_it_renders() {
    let entry = UniverseTest {
        component: Some("cluster".to_owned()),
        tags: vec!["nightly".to_owned(), "slow".to_owned()],
        ..universe_test_full("tests/cluster/test_failover.py", "failover", None)
    };
    let rows = exec_rows(
        "tests/cluster/test_failover.py",
        &["PASSED", "PASSED", "PASSED", "FAILED", "ERROR", "SKIPPED"],
    );

    let flaky = build_flaky(&[entry], &rows, 7, TODAY);
    assert_eq!(flaky.len(), 1);
    assert_eq!(
        flaky[0],
        FlakyTest {
            test_file: "tests/cluster/test_failover.py".to_owned(),
            test_name: "failover".to_owned(),
            component: Some("cluster".to_owned()),
            tags: vec!["nightly".to_owned(), "slow".to_owned()],
            pass_rate: 50.0,
            executions: 6,
            pass_count: 3,
            fail_count: 2,
            skipped_count: 1,
        }
    );
}

/// `build_quality_vector_summary:1047-1083`: one count per vector, over
/// **distinct files**, sorted by count descending then vector ascending
/// (`:1071-1076`).
///
/// The tiebreak is the half an implementation loses: `perf` and `scale` both hold
/// one file, and sorting by count alone leaves their order to the map. Reverse the
/// primary key and `perf` leads; drop the tiebreak and this fails intermittently,
/// which is why both are asserted as a whole list.
#[test]
fn quality_vectors_count_files_and_sort_by_count_then_name() {
    let universe = vec![
        vectored("tests/a.py", &["security", "perf"]),
        vectored("tests/b.py", &["security"]),
        vectored("tests/c.py", &["security", "scale"]),
    ];

    let summary = build_quality_vector_summary(&universe);

    let items: Vec<(&str, usize)> = summary
        .items
        .iter()
        .map(|item| (item.vector.as_str(), item.tests))
        .collect();
    assert_eq!(items, vec![("security", 3), ("perf", 1), ("scale", 1)]);
    assert_eq!(summary.total_tests, 3);
    assert_eq!(summary.unclassified_tests, 0);
}

/// A file declaring no vector increments `unclassified_tests` (`:1053-1056`) and
/// is still counted in `total_tests` (`:1081`, `vectors_by_file.len()`).
///
/// Three distinct numbers — 1 unclassified, 3 total, 2 on the one item — so a
/// transposition between the two counters, or an implementation that excluded the
/// unclassified file from the total, changes exactly one of them.
#[test]
fn a_file_declaring_no_vector_is_unclassified_and_still_counted_in_total() {
    let universe = vec![
        vectored("tests/a.py", &["security"]),
        vectored("tests/b.py", &["security"]),
        universe_test("tests/c.py"),
    ];

    let summary = build_quality_vector_summary(&universe);

    assert_eq!(summary.unclassified_tests, 1);
    assert_eq!(summary.total_tests, 3);
    assert_eq!(summary.items.len(), 1);
    assert_eq!(summary.items[0].tests, 2);
}

/// Counting is case-insensitive (`:1060-1062`, `to_ascii_lowercase` as the map
/// key) and a blank vector is dropped (`:875-878`).
///
/// One file declaring `Security`, `security` and `SECURITY` is **one** test for
/// the vector, not three: the per-file fold case-folds first (`:879-881`). Drop
/// either fold and the count reads 3 or the two spellings become two items.
#[test]
fn quality_vectors_fold_case_variants_and_drop_blanks() {
    let universe = vec![vectored(
        "tests/a.py",
        &["Security", "security", "SECURITY", "  ", ""],
    )];

    let summary = build_quality_vector_summary(&universe);

    assert_eq!(summary.items.len(), 1, "one vector, three spellings");
    assert_eq!(summary.items[0].tests, 1);
    assert_eq!(
        summary.items[0].vector, "Security",
        "all three spellings fold to one key, so the first in declaration order \
         wins - legacy's or_insert_with at :876-881"
    );
    assert_eq!(summary.unclassified_tests, 0, "a blank is not a vector");
}

/// `total_tests` is the number of **distinct files** (`:1081`), not
/// `universe.len()`.
///
/// Legacy keys the vector map on the normalized test file (`:839`). This fixture
/// hands the fold two entries for one file directly — bypassing
/// `walk_repo_universe`'s own same-repository merge, which is why the two
/// plans reach this fold as two entries at all — to pin that this fold
/// de-duplicates on the file itself rather than trusting the universe to have
/// done so. Folding over the slice without that de-duplication would
/// double-count the file in `total_tests` and in the item's `tests`; the two
/// plans' vector sets are merged rather than counted apart.
#[test]
fn a_file_in_two_plans_is_one_quality_vector_test() {
    let universe = vec![
        UniverseTest {
            plan_path: "plans/smoke.yaml".to_owned(),
            ..vectored("tests/a.py", &["security"])
        },
        UniverseTest {
            plan_path: "plans/nightly.yaml".to_owned(),
            ..vectored("tests/a.py", &["perf"])
        },
    ];

    let summary = build_quality_vector_summary(&universe);

    assert_eq!(summary.total_tests, 1, "one file, two plans");
    assert_eq!(summary.items.len(), 2, "both plans' vectors are merged in");
    assert!(
        summary.items.iter().all(|item| item.tests == 1),
        "each vector is carried by one file, not two"
    );
}

/// A `test_file` collision **across two repositories** is two files, not one —
/// fix-round-2's correction to `by_file`'s key.
///
/// Unlike [`a_file_in_two_plans_is_one_quality_vector_test`], which pins that
/// one repo's file listed by two plans collapses to one entry, this pins the
/// opposite: two *different* repositories that each happen to have a
/// `tests/test_smoke.py` are two distinct files. Keying `by_file` on the bare
/// path would merge them, so repo B's untagged file would inherit repo A's
/// `security` vector and never count toward `unclassified_tests` — silently
/// losing the fact that repo B declares nothing at all. Keyed on
/// `(repo_id, test_file)`, the two stay apart: `total_tests` counts both, and
/// repo B's file is the one entry behind `unclassified_tests`.
///
/// Observed red under the previous `test_file`-only key, which reports
/// `total_tests: 1` and `unclassified_tests: 0`.
#[test]
fn a_cross_repo_path_collision_is_two_files_not_one() {
    let repo_a = Uuid::from_u128(0x41);
    let repo_b = Uuid::from_u128(0x42);
    let universe = vec![
        UniverseTest {
            repo_id: repo_a,
            ..vectored("tests/test_smoke.py", &["security"])
        },
        UniverseTest {
            repo_id: repo_b,
            ..vectored("tests/test_smoke.py", &[])
        },
    ];

    let summary = build_quality_vector_summary(&universe);

    assert_eq!(
        summary.total_tests, 2,
        "two repositories' files, not one merged file"
    );
    assert_eq!(
        summary.unclassified_tests, 1,
        "repo B's file declares no vector and must still count as unclassified, \
         rather than inheriting repo A's `security`"
    );
    assert_eq!(
        summary.items.len(),
        1,
        "only repo A's `security` is declared"
    );
    assert_eq!(
        summary.items[0].tests, 1,
        "security is carried by repo A's file alone"
    );
}

/// `build_grouped_summaries:1085-1113`: the component breakdown is a partition of
/// the universe, keyed on the latest bucket, and a blank component groups under
/// `"unknown"` (`:1097-1103`).
///
/// The four counters are four distinct numbers, so a transposition inside
/// `accumulate_group`'s tuple layout (`:2225-2232`, `(total, passed, failed,
/// not_run)`) fails rather than reading plausibly. `None`, `""` and `"  "` are all
/// present because the trim is the half an implementation drops.
#[test]
fn component_groups_partition_the_universe_and_blanks_group_under_unknown() {
    let universe = vec![
        grouped("tests/a.py", Some("cluster"), &[]),
        grouped("tests/b.py", Some("cluster"), &[]),
        grouped("tests/c.py", None, &[]),
        grouped("tests/d.py", Some("   "), &[]),
        grouped("tests/e.py", Some(""), &[]),
    ];
    let rows = vec![
        exec_row_at("tests/a.py", "PASSED", ts()),
        exec_row_at("tests/b.py", "ERROR", ts()),
        exec_row_at("tests/c.py", "SKIPPED", ts()),
        exec_row_at("tests/d.py", "PASSED", ts()),
    ];

    let grouped_summaries = build_grouped_summaries(&universe, &rows);

    assert_eq!(
        grouped_summaries.component,
        vec![
            GroupSummary {
                value: "cluster".to_owned(),
                total: 2,
                passed: 1,
                failed: 1,
                not_run: 0,
            },
            GroupSummary {
                value: UNKNOWN_COMPONENT.to_owned(),
                total: 3,
                passed: 1,
                failed: 0,
                not_run: 2,
            },
        ],
        "alphabetical by key, and SKIPPED plus the unrun file are both not_run"
    );
}

/// A file with two tags is counted **once per tag** (`:1106-1112`), so the tag
/// breakdown is not a partition; a file with none groups under `"untagged"`.
///
/// The counters sum to four over a universe of three, which is the property a
/// reader will mistake for a bug. Fold the tag loop into a single first-tag
/// lookup and the totals drop to three.
#[test]
fn a_multiply_tagged_file_is_counted_once_per_tag() {
    let universe = vec![
        grouped("tests/a.py", None, &["nightly", "slow"]),
        grouped("tests/b.py", None, &["slow"]),
        grouped("tests/c.py", None, &[]),
    ];
    let rows = vec![
        exec_row_at("tests/a.py", "PASSED", ts()),
        exec_row_at("tests/b.py", "FAILED", ts()),
    ];

    let tags = build_grouped_summaries(&universe, &rows).tag;

    assert_eq!(
        tags,
        vec![
            GroupSummary {
                value: "nightly".to_owned(),
                total: 1,
                passed: 1,
                failed: 0,
                not_run: 0,
            },
            GroupSummary {
                value: "slow".to_owned(),
                total: 2,
                passed: 1,
                failed: 1,
                not_run: 0,
            },
            GroupSummary {
                value: UNTAGGED.to_owned(),
                total: 1,
                passed: 0,
                failed: 0,
                not_run: 1,
            },
        ]
    );
    let counted: usize = tags.iter().map(|group| group.total).sum();
    assert_eq!(counted, 4, "three files, four tag memberships");
}

/// Every platform group is counted over the **whole universe** (`:1129-1138`), so
/// a test that never ran on a platform is `not_run` there and each group's `total`
/// is `universe.len()`.
///
/// Two platforms, three tests, and each platform ran a different subset. An
/// implementation that partitioned one latest map instead of rebuilding one per
/// platform reports `total: 1` per group and loses the `not_run` column entirely —
/// which is exactly the column the chart exists to show.
#[test]
fn every_platform_group_counts_the_whole_universe() {
    let universe = vec![
        universe_test("tests/a.py"),
        universe_test("tests/b.py"),
        universe_test("tests/c.py"),
    ];
    let rows = vec![
        platform_row("tests/a.py", "PASSED", Some(PLATFORM_A)),
        platform_row("tests/b.py", "FAILED", Some(PLATFORM_A)),
        platform_row("tests/a.py", "FAILED", Some(PLATFORM_B)),
    ];

    let platforms = build_grouped_summaries(&universe, &rows).platform;

    assert_eq!(
        platforms,
        vec![
            PlatformGroupSummary {
                environment_id: PLATFORM_A,
                total: 3,
                passed: 1,
                failed: 1,
                not_run: 1,
            },
            PlatformGroupSummary {
                environment_id: PLATFORM_B,
                total: 3,
                passed: 0,
                failed: 1,
                not_run: 2,
            },
        ]
    );
}

/// A row with no platform joins **no** group (`:1116-1119`,
/// `filter_map(normalize_optional(..))`) — there is no `"unknown"` platform the
/// way there is an unknown component.
///
/// Asserted alongside a real platform so "no groups at all" cannot pass: the
/// answer is one group, not two and not zero.
#[test]
fn a_row_without_a_platform_joins_no_platform_group() {
    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![
        platform_row("tests/a.py", "PASSED", None),
        platform_row("tests/a.py", "FAILED", Some(PLATFORM_A)),
    ];

    let platforms = build_grouped_summaries(&universe, &rows).platform;

    assert_eq!(platforms.len(), 1);
    assert_eq!(platforms[0].environment_id, PLATFORM_A);
}

/// `group_map_to_vec:2212` returns the [`BTreeMap`](std::collections::BTreeMap)'s
/// order, which is alphabetical by key — **not** sorted by size.
///
/// The sibling folds [`build_lists`] and [`build_flaky`] both sort by a measure,
/// so reaching for one here is the plausible mistake. `zebra` holds three files
/// and `alpha` one, and `alpha` still comes first.
#[test]
fn group_summaries_are_alphabetical_and_not_sorted_by_size() {
    let universe = vec![
        grouped("tests/a.py", Some("zebra"), &[]),
        grouped("tests/b.py", Some("zebra"), &[]),
        grouped("tests/c.py", Some("zebra"), &[]),
        grouped("tests/d.py", Some("alpha"), &[]),
    ];

    let components = build_grouped_summaries(&universe, &[]).component;

    let order: Vec<&str> = components
        .iter()
        .map(|group| group.value.as_str())
        .collect();
    assert_eq!(order, vec!["alpha", "zebra"]);
}

/// `apply_universe_group_filter:1148-1155`: a `group_value` that is `None`, empty
/// or whitespace narrows **nothing**, whatever the grouping says.
///
/// So "grouped by component with no component chosen" is the unfiltered overview
/// rather than an empty one. Drop the trim and `"   "` returns zero tests, and the
/// screen renders an empty suite.
#[test]
fn a_blank_group_value_narrows_nothing() {
    let universe = vec![
        grouped("tests/a.py", Some("cluster"), &["nightly"]),
        grouped("tests/b.py", Some("network"), &[]),
    ];

    for value in [None, Some(""), Some("   ")] {
        assert_eq!(
            apply_universe_group_filter(&universe, GroupBy::Component, value).len(),
            2,
            "a blank group value is not a filter"
        );
    }
}

/// The component and tag filters both match with `eq_ignore_ascii_case`
/// (`:1164`, `:1174`), and the value is trimmed first (`:1153`).
///
/// Note the asymmetry this pins against
/// [`build_grouped_summaries`], which groups on the **verbatim** string: two
/// components differing only in case are two bars and one selection.
#[test]
fn the_component_and_tag_filters_are_case_insensitive() {
    let universe = vec![
        grouped("tests/a.py", Some("cluster"), &["nightly"]),
        grouped("tests/b.py", Some("network"), &["Nightly"]),
    ];

    let by_component =
        apply_universe_group_filter(&universe, GroupBy::Component, Some("  CLUSTER "));
    assert_eq!(by_component.len(), 1);
    assert_eq!(by_component[0].test_file, "tests/a.py");

    let by_tag = apply_universe_group_filter(&universe, GroupBy::Tag, Some("nightly"));
    assert_eq!(by_tag.len(), 2, "both spellings of the tag match");
}

/// A universe entry whose component is `None` matches no component filter
/// (`:1165`, `.unwrap_or(false)`), so the files grouped under
/// [`UNKNOWN_COMPONENT`] are unreachable through the drill-down.
///
/// The label is a grouping key, not a value any test carries — selecting the
/// `unknown` bar returns nothing. Ported as-is, and asserted because an
/// implementation that treated the label as a value would make it return
/// everything unlabelled.
#[test]
fn a_component_of_none_matches_no_component_filter() {
    let universe = vec![
        grouped("tests/a.py", None, &[]),
        grouped("tests/b.py", Some("cluster"), &[]),
    ];

    assert!(
        apply_universe_group_filter(&universe, GroupBy::Component, Some(UNKNOWN_COMPONENT))
            .is_empty()
    );
}

/// `GroupBy::Environment` falls to legacy's `_` arm together with `GroupBy::None`
/// (`:1178`) and narrows **nothing**.
///
/// The rows carry the environment and the universe does not, so there is nothing to
/// filter on — selecting an environment bar returns the whole universe rather than the
/// tests that ran there. Ported as-is under Phase B's standing instruction, and
/// pinned because an implementation that "fixed" it would change a rendered list.
#[test]
fn the_environment_grouping_does_not_narrow_the_universe() {
    let universe = vec![
        grouped("tests/a.py", Some("cluster"), &["nightly"]),
        grouped("tests/b.py", Some("network"), &[]),
    ];

    for group_by in [GroupBy::Environment, GroupBy::None] {
        assert_eq!(
            apply_universe_group_filter(&universe, group_by, Some(&PLATFORM_A.to_string())).len(),
            2
        );
    }
}

// ---------------------------------------------------------------------------
// The build distribution and the build-tests fold (Task 24)
// ---------------------------------------------------------------------------

/// One file-level row carrying a chosen build and a chosen instant.
///
/// `build` is an `Option<&str>` and is written through **verbatim**, blanks
/// included, because ruling R13's collapse — `None`, `"  "` and the literal
/// `"unknown"` landing in one bucket — is not expressible over a fixture that
/// normalizes for the fold. `test_support::exec_row_at` hardcodes
/// `Some("9.1.0-4412")`, so a build test built on it could not vary the field at
/// all.
///
/// The run id is named for the same reason [`row`]'s is: the distribution's
/// `latest_run_id` is picked by a scan over the snapshots, and a fixture with
/// random ids could not say which snapshot won.
fn build_row(
    run_id: Uuid,
    test_file: &str,
    status: &str,
    build: Option<&str>,
    at: OffsetDateTime,
) -> ExecRow {
    ExecRow {
        run_id,
        build: build.map(str::to_owned),
        ..exec_row_at(test_file, status, at)
    }
}

/// `compare_build_desc:2188`. Newest first, segment by segment, with a missing
/// segment reading as `0` — so `1.2` sorts after `1.2.1`, not before it.
///
/// **This test does not discriminate the padding from the zipping**, and Step 0
/// measured that rather than assuming it — see
/// [`the_build_distribution_comparator_diverges_from_sorted_versions_desc_two_ways`],
/// which is the test that does. Kept with the brief's name and assertions
/// unchanged because it is still the only test that pins the *direction* of every
/// numeric segment.
#[test]
fn builds_sort_newest_first_by_numeric_segment() {
    let mut builds = vec![
        "1.2.1".to_owned(),
        "1.10.0".to_owned(),
        "1.2".to_owned(),
        "1.9.9".to_owned(),
    ];
    builds.sort_by(|a, b| compare_build_desc(a, b));
    assert_eq!(builds, vec!["1.10.0", "1.9.9", "1.2.1", "1.2"]);
}

/// A non-numeric segment falls back to a reverse **string** compare, not to an
/// error and not to zero (`:2200-2202`, the `_ =>` arm).
#[test]
fn a_non_numeric_segment_falls_back_to_reverse_string_order() {
    assert_eq!(
        compare_build_desc("1.rc2", "1.rc1"),
        std::cmp::Ordering::Less
    );
}

/// Equal through every segment falls through to the whole-string reverse
/// compare at `:2209`.
#[test]
fn identical_builds_compare_equal() {
    assert_eq!(
        compare_build_desc("2.0.0", "2.0.0"),
        std::cmp::Ordering::Equal
    );
}

/// [`compare_build_desc`] and [`sorted_versions_desc`]' comparator disagree by
/// **two** independent mechanisms, which is what makes them two functions.
///
/// This test asserted only the first mechanism and its doc claimed the two
/// comparators "agree on every realistic label". That was wrong, and wrong in the
/// direction that invites deleting one of them.
///
/// 1. **Length.** [`compare_build_desc`] pads a missing segment with `"0"` over
///    `max_len` (`:2194-2195`); the other `zip`s and stops at the shorter label
///    (`:2172-2173`). Visible only where the extra segment sorts below `"0"`, so
///    `"1.2."` against `"1.2"`: the padded comparator reaches index 2, compares
///    `""` against `"0"` as strings and answers "`1.2` first"; the zipping one
///    never reaches index 2 and falls to `"1.2.".cmp("1.2")`, which answers
///    "`1.2.` first".
/// 2. **A third arm.** [`sorted_versions_desc`]' comparator has
///    `_ if right_part != left_part` (legacy `:2178`) — it fires when two
///    segments are numerically **equal** but textually different, and
///    short-circuits, so no later segment is consulted. [`compare_build_desc`]
///    falls through such a pair and keeps walking. **No length difference is
///    needed and no exotic label**: `"2024.01.20"` against `"2024.1.15"` is an
///    ordinary date stamp, `01` and `1` are numerically equal, and the two
///    comparators answer opposite ways because only one of them ever reaches
///    `20` against `15`.
///
/// A future reader who unifies them on the strength of mechanism 1 alone would
/// silently reorder every zero-padded build label, which is why the second
/// assertion pair is here.
#[test]
fn the_build_distribution_comparator_diverges_from_sorted_versions_desc_two_ways() {
    // Mechanism 1: the pad against the zip.
    let mut builds = vec!["1.2.".to_owned(), "1.2".to_owned()];
    builds.sort_by(|left, right| compare_build_desc(left, right));
    assert_eq!(builds, vec!["1.2", "1.2."], "the pad reaches index 2");

    assert_eq!(
        sorted_versions_desc(&["1.2.".to_owned(), "1.2".to_owned()]),
        vec!["1.2.", "1.2"],
        "the zip stops at index 1 and the whole-string tiebreak decides",
    );

    // Mechanism 2: the `:2178` arm, on a leading zero and equal lengths.
    let mut stamps = vec!["2024.1.15".to_owned(), "2024.01.20".to_owned()];
    stamps.sort_by(|left, right| compare_build_desc(left, right));
    assert_eq!(
        stamps,
        vec!["2024.01.20", "2024.1.15"],
        "falls through 01 == 1 and decides on 20 against 15",
    );

    assert_eq!(
        sorted_versions_desc(&["2024.1.15".to_owned(), "2024.01.20".to_owned()]),
        vec!["2024.1.15", "2024.01.20"],
        "short-circuits on 01 against 1 and never reaches the third segment",
    );
}

/// `build_last_run_build_distribution:1536` counts **one row per universe file**
/// — `latest_per_test_snapshot:1615`'s first-wins scan (`:1630-1632`) — and
/// splits it three ways: `PASSED`, `FAILED`+`ERROR`, and `SKIPPED` as
/// `executed_total` only (`:1557-1569`).
///
/// The fixture puts an older row on `tests/a.py` under a *different* build, so an
/// implementation that folded every row rather than the latest one would report
/// two builds instead of one. `ERROR` is sent on `tests/b.py` because the
/// snapshot mapping folds it into `FAILED` (`:1635`) and a fold that passed it
/// through would leave `failed` at zero.
#[test]
fn the_build_distribution_counts_only_the_latest_row_per_test() {
    let universe = vec![
        universe_test("tests/a.py"),
        universe_test("tests/b.py"),
        universe_test("tests/c.py"),
    ];
    let rows = vec![
        build_row(run(1), "tests/a.py", "PASSED", Some("2.0"), ts()),
        build_row(run(1), "tests/b.py", "ERROR", Some("2.0"), ts()),
        build_row(run(1), "tests/c.py", "SKIPPED", Some("2.0"), ts()),
        build_row(
            run(2),
            "tests/a.py",
            "FAILED",
            Some("1.0"),
            ts() - Duration::days(1),
        ),
    ];

    let distribution = build_last_run_build_distribution(&universe, &rows);

    assert_eq!(
        distribution.len(),
        1,
        "the older 1.0 row is nobody's latest"
    );
    assert_eq!(distribution[0].build, "2.0");
    assert_eq!(distribution[0].passed, 1);
    assert_eq!(distribution[0].failed, 1, "ERROR folds into FAILED");
    assert_eq!(
        distribution[0].executed_total, 3,
        "SKIPPED is executed and is neither passed nor failed",
    );
}

/// The sort at `:1593-1601`: `"unknown"` last whatever it compares as, and
/// everything else by [`compare_build_desc`].
///
/// The special case is `eq_ignore_ascii_case` (`:1594-1595`), so a build the
/// runner spelled `"UNKNOWN"` sorts last too — and it is a *separate bar* from
/// the substituted label, because the substitution only fires for an absent or
/// blank build. Both halves are asserted: drop the special case and `unknown`
/// sorts by its `u`, ahead of nothing and behind every numeric label, which is
/// where a reader would never look for it.
///
/// **Two `unknown`s fall to legacy's `_` arm together** (`:1599`), so the pair is
/// ordered by [`compare_build_desc`] like any other — a reverse byte-wise compare
/// of one non-numeric segment, which puts lowercase `unknown` *before*
/// `UNKNOWN`. Measured rather than assumed: this assertion read the other way
/// round first.
#[test]
fn the_build_distribution_sorts_unknown_last_and_the_rest_newest_first() {
    let universe = vec![
        universe_test("tests/a.py"),
        universe_test("tests/b.py"),
        universe_test("tests/c.py"),
        universe_test("tests/d.py"),
        universe_test("tests/e.py"),
    ];
    let rows = vec![
        build_row(run(1), "tests/a.py", "PASSED", Some("1.2"), ts()),
        build_row(run(1), "tests/b.py", "PASSED", Some("1.10.0"), ts()),
        build_row(run(1), "tests/c.py", "PASSED", None, ts()),
        build_row(run(1), "tests/d.py", "PASSED", Some("1.2.1"), ts()),
        build_row(run(1), "tests/e.py", "PASSED", Some("UNKNOWN"), ts()),
    ];

    let distribution = build_last_run_build_distribution(&universe, &rows);
    let order: Vec<&str> = distribution
        .iter()
        .map(|item| item.build.as_str())
        .collect();

    assert_eq!(order, vec!["1.10.0", "1.2.1", "1.2", "unknown", "UNKNOWN"]);
}

/// Ruling R13's collapse, at the one place that reads
/// [`ExecRow::build`](crate::domain::analytics::ExecRow::build): a row with **no**
/// build, a row whose build is blank and a row whose build is the literal
/// `"unknown"` all land in the *same* bucket.
///
/// Legacy applies `normalize_optional(row.app_build.as_deref())
/// .unwrap_or_else(|| "unknown".to_string())` while *building* the row
/// (`:1032-1033`, `normalize_optional` at `:2070-2075`), so by the time its folds
/// see the value the three cases are already indistinguishable. This gear carries
/// the `Option` as far as this fold and then collapses it identically — one bar,
/// not three, and not two.
///
/// **The indistinguishability is ported, not fixed.** A deployment that really
/// ran a build called `unknown` would have its results merged with the
/// build-less ones on this chart. Legacy cannot tell them apart either and the
/// standing instruction is parity, so the merge is asserted rather than avoided.
#[test]
fn the_build_distribution_collapses_absent_blank_and_literal_unknown_builds() {
    let universe = vec![
        universe_test("tests/a.py"),
        universe_test("tests/b.py"),
        universe_test("tests/c.py"),
    ];
    let rows = vec![
        build_row(run(1), "tests/a.py", "PASSED", None, ts()),
        build_row(run(1), "tests/b.py", "PASSED", Some("   "), ts()),
        build_row(run(1), "tests/c.py", "PASSED", Some(UNKNOWN_BUILD), ts()),
    ];

    let distribution = build_last_run_build_distribution(&universe, &rows);

    assert_eq!(distribution.len(), 1, "one bucket, not three");
    assert_eq!(distribution[0].build, UNKNOWN_BUILD);
    assert_eq!(distribution[0].passed, 3);
}

/// `latest_run_id` is picked by the strictly-`>` first-wins scan at `:1572-1579`,
/// and an exact tie therefore resolves to whichever snapshot came first.
///
/// Two assertions, and the second is ruling R14's:
///
/// * **Strictly greater wins.** The newer snapshot's run replaces the older
///   one's, whatever order the snapshots arrive in.
/// * **An exact tie keeps the first snapshot**, which under this port is the
///   alphabetically first `test_file` rather than legacy's arbitrary `HashMap`
///   yield (`:1652`). The rows are handed over in the *reverse* of that order, so
///   a fold that took the row order instead would answer `run(2)`.
#[test]
fn the_build_distribution_picks_the_latest_run_id_by_a_strictly_greater_scan() {
    let universe = vec![universe_test("tests/a.py"), universe_test("tests/b.py")];

    let newer_on_b = vec![
        build_row(
            run(8),
            "tests/b.py",
            "PASSED",
            Some("2.0"),
            ts() + Duration::hours(1),
        ),
        build_row(run(9), "tests/a.py", "PASSED", Some("2.0"), ts()),
    ];
    assert_eq!(
        build_last_run_build_distribution(&universe, &newer_on_b)[0].latest_run_id,
        Some(run(8)),
        "the strictly newer snapshot's run wins",
    );

    let tied = vec![
        build_row(run(2), "tests/b.py", "PASSED", Some("2.0"), ts()),
        build_row(run(1), "tests/a.py", "PASSED", Some("2.0"), ts()),
    ];
    assert_eq!(
        build_last_run_build_distribution(&universe, &tied)[0].latest_run_id,
        Some(run(1)),
        "an exact tie keeps the first snapshot, which is tests/a.py",
    );
}

/// Ruling R14's determinism: [`latest_per_test_snapshot`] returns its snapshots
/// ordered by `test_file`, where legacy returns `latest.into_values()` (`:1652`)
/// in a `HashMap`'s arbitrary order.
///
/// Both the universe order and the row order disagree with the answer, so a fold
/// that leaked either one fails. This is the one departure from legacy in this
/// task and the function's header carries the argument for it; the test exists
/// because the property is otherwise unobservable until an exact-`ts` tie makes
/// it decide a rendered `latest_run_id`.
#[test]
fn the_build_tests_snapshots_come_back_in_test_file_order() {
    let universe = vec![
        universe_test("tests/zebra.py"),
        universe_test("tests/alpha.py"),
        universe_test("tests/middle.py"),
    ];
    let rows = vec![
        build_row(run(1), "tests/middle.py", "PASSED", Some("2.0"), ts()),
        build_row(run(2), "tests/zebra.py", "PASSED", Some("2.0"), ts()),
        build_row(run(3), "tests/alpha.py", "PASSED", Some("2.0"), ts()),
    ];

    let snapshots = latest_per_test_snapshot(&universe, &rows);
    let files: Vec<&str> = snapshots
        .iter()
        .map(|snapshot| snapshot.test_file.as_str())
        .collect();

    assert_eq!(
        files,
        vec!["tests/alpha.py", "tests/middle.py", "tests/zebra.py"],
    );
}

/// `api_build_tests:444-452` sorts by `(build_status_rank(status), test_name)`,
/// and [`build_status_rank`] (`:1948-1955`) ranks
/// `FAILED < PASSED < SKIPPED < everything else`.
///
/// Failures first is the whole point of the screen, so the rank is not
/// alphabetical and is not the universe order. The two `FAILED` entries are named
/// `zulu` and `alpha` to pin the second key, and the `XFAIL` entry pins the
/// `other => 3` arm — which is reachable *only* because the snapshot mapping
/// passes an unrecognized status through verbatim (`:1637`). Rank them with
/// [`bucketize_status`](crate::domain::analytics::universe::bucketize_status)
/// instead and `XFAIL` becomes `NOT_RUN`, which ranks the same but renders a
/// different word.
#[test]
fn build_tests_are_ranked_failed_then_passed_then_skipped_then_other() {
    let universe = vec![
        named("tests/1.py", "zulu"),
        named("tests/2.py", "alpha"),
        named("tests/3.py", "beta"),
        named("tests/4.py", "charlie"),
        named("tests/5.py", "delta"),
    ];
    let rows = vec![
        build_row(run(1), "tests/1.py", "FAILED", Some("2.0"), ts()),
        build_row(run(1), "tests/2.py", "ERROR", Some("2.0"), ts()),
        build_row(run(1), "tests/3.py", "PASSED", Some("2.0"), ts()),
        build_row(run(1), "tests/4.py", "SKIPPED", Some("2.0"), ts()),
        build_row(run(1), "tests/5.py", "XFAIL", Some("2.0"), ts()),
    ];

    let details = build_test_details(&universe, &rows, "2.0");

    let names: Vec<&str> = details.iter().map(|item| item.test_name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "zulu", "beta", "charlie", "delta"]);

    let statuses: Vec<&str> = details.iter().map(|item| item.status.as_str()).collect();
    assert_eq!(
        statuses,
        vec!["FAILED", "FAILED", "PASSED", "SKIPPED", "XFAIL"],
    );

    assert_eq!(build_status_rank("FAILED"), 0);
    assert_eq!(build_status_rank("PASSED"), 1);
    assert_eq!(build_status_rank("SKIPPED"), 2);
    assert_eq!(build_status_rank("XFAIL"), 3);
    assert_eq!(build_status_rank(""), 3);
}

/// `api_build_tests:427` matches the build with `eq_ignore_ascii_case`, and
/// **does not trim** — the trim is the handler's, at `:373`.
///
/// The third assertion is the one that records the seam: a caller that hands this
/// fold an untrimmed query parameter gets an empty list rather than the build it
/// asked for, so the trim and the empty-build rejection stay with the *handler*
/// where legacy keeps them. Both are
/// [`crate::domain::analytics::query::normalize_build`] as of Task 25a, and
/// `a_blank_build_is_refused_with_legacys_message` is the other end of this
/// seam.
#[test]
fn build_tests_filter_the_build_case_insensitively_and_untrimmed() {
    let universe = vec![universe_test("tests/a.py"), universe_test("tests/b.py")];
    let rows = vec![
        build_row(run(1), "tests/a.py", "PASSED", Some("9.1.0-RC1"), ts()),
        build_row(run(1), "tests/b.py", "PASSED", Some("8.0"), ts()),
    ];

    let matched = build_test_details(&universe, &rows, "9.1.0-rc1");
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].test_file, "tests/a.py");

    assert!(build_test_details(&universe, &rows, "7.0").is_empty());
    assert!(
        build_test_details(&universe, &rows, " 9.1.0-rc1 ").is_empty(),
        "the trim is the caller's, exactly as it is in legacy",
    );
}

/// `api_build_tests:428-441`: every field but `status`, `run_id` and
/// `run_finished_at` is joined off the **universe** entry, not off the row.
///
/// `test_name`, `component` and `tags` are the universe's (`:432`, `:436-437`),
/// which is why the fixture gives the row a `test_name` that disagrees with it —
/// a fold that read the row's would render the wrong label and no other
/// assertion in this module would notice.
///
/// `run_finished_at` is asserted as an [`OffsetDateTime`] rather than as legacy's
/// RFC-3339 `String` (`:435`, `item.ts.to_rfc3339()`). The formatting is the
/// DTO's, one task later, matching
/// [`LatestInfo::finished_at`](crate::domain::analytics::universe::LatestInfo::finished_at).
#[test]
fn build_tests_join_their_metadata_off_the_universe_entry() {
    let universe = vec![UniverseTest {
        component: Some("cluster".to_owned()),
        tags: vec!["nightly".to_owned()],
        ..named("tests/a.py", "the display name")
    }];
    let rows = vec![build_row(run(7), "tests/a.py", "PASSED", Some("2.0"), ts())];

    let details = build_test_details(&universe, &rows, "2.0");

    assert_eq!(details.len(), 1);
    assert_eq!(details[0].test_file, "tests/a.py");
    assert_eq!(details[0].test_name, "the display name");
    assert_eq!(details[0].component.as_deref(), Some("cluster"));
    assert_eq!(details[0].tags, vec!["nightly".to_owned()]);
    assert_eq!(details[0].run_id, run(7));
    assert_eq!(details[0].run_finished_at, ts());
}

/// The snapshot mapping (`:1633-1639`) is a **fourth** classification, not
/// [`bucketize_status`](crate::domain::analytics::universe::bucketize_status) and
/// not [`build_status_rank`]: an unrecognized status is passed through verbatim.
///
/// Ruling R13's seventh row of `domain::service::ingest`'s table. The consequence
/// on the chart is that such a build gets a **bar with no counters at all** —
/// legacy's aggregation `match` has an empty `_ => {}` arm (`:1569`) but the
/// `entry(..).or_default()` above it (`:1555`) has already created the bucket, so
/// the build is listed with `passed`, `failed` and `executed_total` all zero and a
/// `latest_run_id` set. Reach for `bucketize_status` here and the status renders
/// as `NOT_RUN`; reach for `build_stats_map`'s split and `executed_total` reads 1.
#[test]
fn the_build_distribution_passes_an_unrecognized_status_through_and_counts_it_nowhere() {
    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![build_row(run(4), "tests/a.py", "XPASS", Some("2.0"), ts())];

    let distribution = build_last_run_build_distribution(&universe, &rows);
    assert_eq!(distribution.len(), 1, "the bar exists");
    assert_eq!(distribution[0].passed, 0);
    assert_eq!(distribution[0].failed, 0);
    assert_eq!(distribution[0].executed_total, 0, "counted nowhere");
    assert_eq!(distribution[0].latest_run_id, Some(run(4)));

    let details = build_test_details(&universe, &rows, "2.0");
    assert_eq!(details[0].status, "XPASS");
    assert_ne!(
        details[0].status, NOT_RUN,
        "the snapshot mapping is not bucketize_status",
    );
}

/// `build_last_run_build_distribution:1540-1542` returns early on an empty
/// snapshot set, and `latest_per_test_snapshot:1630-1631` drops a row whose file
/// is not in the universe.
///
/// So the two ways of having nothing to chart — an empty universe, and rows that
/// belong to no universe file — both give an empty `Vec` rather than one
/// `"unknown"` bar or a bar built from unattributed rows. The third case is the
/// one that would be a real regression: a universe with no rows at all must not
/// produce a bar either, because every one of its files is `NOT_RUN` and the
/// distribution is a fold over *rows*.
#[test]
fn the_build_distribution_is_empty_without_snapshots() {
    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![build_row(run(1), "tests/a.py", "PASSED", Some("2.0"), ts())];

    assert!(build_last_run_build_distribution(&[], &rows).is_empty());
    assert!(build_last_run_build_distribution(&universe, &[]).is_empty());
    assert!(
        build_last_run_build_distribution(&[universe_test("tests/other.py")], &rows).is_empty(),
        "a row outside the universe is not evidence of a build",
    );
    assert!(build_test_details(&universe, &[], "2.0").is_empty());
}

/// The stable `sort_by` at `api_build_tests:442-452` is the **second** place
/// [`latest_per_test_snapshot`]'s order is observable, and ruling R14 made that
/// order deterministic for both.
///
/// Two entries tied on *both* sort keys keep their arrival order, and arrival
/// order is now `test_file` ascending. Reachable rather than theoretical: legacy
/// keys its universe on `(source, repo_id, test_file)` (`:899-903`), so two
/// distinct files can carry the same `test_name` — here `tests/z.py` and
/// `tests/a.py` both display as `duplicate`, both `FAILED`, both under one build.
///
/// The fixture hands the rows over `z` first, so a fold that leaked the row order
/// fails, and `sort_unstable_by` fails too. Without this test the mutation was
/// invisible: the first observation point (the distribution's `latest_run_id`)
/// had a test and this one had only a doc sentence.
#[test]
fn build_tests_tied_on_both_sort_keys_keep_the_snapshot_order() {
    let universe = vec![
        named("tests/z.py", "duplicate"),
        named("tests/a.py", "duplicate"),
    ];
    let rows = vec![
        build_row(run(1), "tests/z.py", "FAILED", Some("2.0"), ts()),
        build_row(run(2), "tests/a.py", "FAILED", Some("2.0"), ts()),
    ];

    let details = build_test_details(&universe, &rows, "2.0");

    let files: Vec<&str> = details.iter().map(|item| item.test_file.as_str()).collect();
    assert_eq!(
        files,
        vec!["tests/a.py", "tests/z.py"],
        "tied on (rank, test_name), so the snapshot order decides",
    );
}
