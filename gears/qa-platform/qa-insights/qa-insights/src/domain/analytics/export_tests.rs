//! Tests for the analytics export: `csv_escape`'s exact quoting rule, the
//! `section`/`format` vocabularies, and the CSV builder's section order.
//!
//! Every legacy citation here was read against `manager/src/routes/analytics.rs`
//! on the `../vhp-testrunner` working tree on 2026-08-24, the same session as
//! [`super`]'s module header.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use time::macros::date;
use uuid::Uuid;

use super::{
    ExportOverview, ExportSection, csv_escape, is_csv_format, normalize_export_section,
    overview_to_csv, parse_export_section,
};
use crate::domain::analytics::PlanRef;
use crate::domain::analytics::aggregates::{
    AnalyticsListItem, AnalyticsLists, FlakyTest, HeatmapData, HeatmapRow, OverviewSummary,
    TrendData, TrendPoint,
};
use crate::domain::error::DomainError;

// ---------------------------------------------------------------------------
// csv_escape — the brief's Step 1 test, verbatim
// ---------------------------------------------------------------------------

/// `csv_escape:2369`. Quote only when the value contains a comma, a double
/// quote or a newline; inside a quoted value, double the quotes. Note what is
/// **not** escaped: a carriage return alone, and a leading `=` — legacy does
/// neither, so neither does this. Port the blind spots.
#[test]
fn csv_escaping_matches_legacy_for_commas_quotes_and_newlines() {
    assert_eq!(csv_escape("plain"), "plain");
    assert_eq!(csv_escape("a,b"), "\"a,b\"");
    assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
    assert_eq!(
        csv_escape("trailing\r"),
        "trailing\r",
        "a bare CR is not a trigger in legacy"
    );
}

/// The second blind spot the brief names: a leading `=` is not a
/// CSV-injection guard trigger. `csv_escape:2369-2375` tests only `,`, `"`
/// and `\n` — nothing about the value's first character.
#[test]
fn csv_escaping_does_not_guard_against_a_leading_formula_character() {
    for value in ["=SUM(A1:A9)", "+1", "-1", "@cmd"] {
        assert_eq!(csv_escape(value), value, "legacy escapes none of these");
    }
}

// ---------------------------------------------------------------------------
// format — no vocabulary, no rejection
// ---------------------------------------------------------------------------

/// `api_export:475-480,488`: only the literal `csv`, case-insensitively and
/// after trimming, selects the CSV branch.
#[test]
fn only_csv_case_insensitively_selects_the_csv_branch() {
    for value in ["csv", "CSV", " Csv ", "cSv"] {
        assert!(is_csv_format(Some(value)), "{value:?} should be csv");
    }
}

/// `api_export:475-480,488`: absent, blank or any other spelling all answer
/// as JSON — there is no `400` for an unrecognized `format`, unlike `section`.
#[test]
fn anything_else_including_absent_answers_as_json() {
    for value in [None, Some(""), Some("json"), Some("xml"), Some("  ")] {
        assert!(!is_csv_format(value), "{value:?} should not be csv");
    }
}

// ---------------------------------------------------------------------------
// section — six spellings, and only for the JSON branch
// ---------------------------------------------------------------------------

/// `overview_section_json:2238-2250`: exactly six spellings are accepted, and
/// `parse_export_section` takes the already-normalized string
/// `normalize_export_section` produces — so case and surrounding whitespace
/// are exercised through that function, not re-tested here.
#[test]
fn the_six_section_spellings_are_accepted() {
    let cases = [
        ("summary", ExportSection::Summary),
        ("lists", ExportSection::Lists),
        ("heatmap", ExportSection::Heatmap),
        ("trend", ExportSection::Trend),
        ("flaky", ExportSection::Flaky),
        ("all", ExportSection::All),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            parse_export_section(raw).expect("must be accepted"),
            expected,
            "for {raw:?}"
        );
    }
}

/// `overview_section_json:2245-2250`: anything else is refused with legacy's
/// message verbatim, naming `section`. `DomainError` carries no `PartialEq`
/// (`domain::error`'s own derive list), so the rejection is destructured
/// rather than compared whole — the same shape `domain::analytics::query`'s
/// own tests use.
#[test]
fn an_unrecognized_section_is_refused_with_legacys_message() {
    let err = parse_export_section("bogus").expect_err("must be refused");
    assert_eq!(
        refusal(err),
        (
            "section".to_owned(),
            "section must be one of: summary, lists, heatmap, trend, flaky, all".to_owned(),
        )
    );
}

/// `api_export:481-486`: shared by both branches, trimmed, case-folded,
/// defaulting to `all`.
#[test]
fn normalize_export_section_trims_lowercases_and_defaults_to_all() {
    assert_eq!(normalize_export_section(None), "all");
    assert_eq!(normalize_export_section(Some("")), "");
    assert_eq!(normalize_export_section(Some(" Lists ")), "lists");
}

/// The `(field, message)` of a [`DomainError::Validation`], or a panic naming
/// what arrived instead — `domain::analytics::query_tests`' own helper,
/// copied here rather than shared because the two modules should not depend
/// on each other's test-only code.
fn refusal(err: DomainError) -> (String, String) {
    match err {
        DomainError::Validation { field, message } => (field, message),
        other => panic!("expected a Validation, got {other:?}"),
    }
}

/// The sharpest asymmetry this module ports: `overview_to_csv` never calls
/// [`parse_export_section`], so a `section` neither of the two functions
/// recognizes is a `400` on the JSON branch and an **empty, `200` body** on
/// the CSV one. `overview_to_csv:2262-2367`'s `include` closure (`:2265`)
/// matches none of its five `if` blocks for an unrecognized name, and
/// `lines.join("\n")` over an empty `Vec` is `""`.
#[test]
fn an_unrecognized_csv_section_renders_an_empty_body_not_an_error() {
    let fixture = Fixture::empty();
    assert_eq!(overview_to_csv("bogus", &fixture.view()), "");
    // And the JSON-only check would have refused the very same string.
    assert!(parse_export_section("bogus").is_err());
}

// ---------------------------------------------------------------------------
// overview_to_csv — section content, order and the blank-line asymmetry
// ---------------------------------------------------------------------------

/// `overview_to_csv:2267-2286`. **Seven metrics only** — the six per-case
/// counters and `case_expected` that [`OverviewSummary`] also carries (and
/// that the JSON `summary` section does render) are absent from the CSV
/// projection, in legacy and here.
#[test]
fn the_summary_section_exports_seven_metrics_and_no_case_counters() {
    let fixture = Fixture::with_data();
    let csv = overview_to_csv("summary", &fixture.view());
    assert_eq!(
        csv,
        "section,metric,value\n\
         summary,total,10\n\
         summary,passed,6\n\
         summary,failed,3\n\
         summary,not_run,1\n\
         summary,passed_pct,60\n\
         summary,failed_pct,30\n\
         summary,not_run_pct,10\n"
    );
    assert!(
        !csv.contains("case_"),
        "the case-level counters must not appear in the CSV summary: {csv:?}"
    );
}

/// `overview_to_csv:2296-2311`, with `last_run_id` in place of legacy's
/// `last_run_name` (this module's header). Also exercises [`csv_escape`]
/// through a component containing a comma and a platform name containing one
/// too, so the two units are pinned working together and not only in
/// isolation.
#[test]
fn the_lists_section_renders_one_row_per_bucket_with_escaped_fields_and_a_resolved_environment_name()
 {
    let platform_id = Uuid::from_u128(0xA11);
    let run_id = Uuid::from_u128(0xB22);
    let mut names = HashMap::new();
    names.insert(platform_id, "Windows, 64-bit".to_owned());

    let mut lists = AnalyticsLists::default();
    lists.failed.push(AnalyticsListItem {
        component: Some("net,work".to_owned()),
        last_platform_id: Some(platform_id),
        last_run_id: Some(run_id),
        ..sample_list_item("tests/a.py")
    });

    let fixture = Fixture {
        summary: OverviewSummary::default(),
        lists,
        heatmap: empty_heatmap(),
        trend: empty_trend(),
        flaky: Vec::new(),
        platform_names: names,
    };

    let csv = overview_to_csv("lists", &fixture.view());
    let mut rows = csv.lines();
    assert_eq!(
        rows.next().unwrap(),
        "section,bucket,test_name,test_file,component,tags,plan_name,last_status,\
         last_environment,last_run_id,pass_count,fail_count,skipped_count,total_runs"
    );
    assert_eq!(
        rows.next().unwrap(),
        format!(
            "lists,failed,tests/a.py display,tests/a.py,\"net,work\",tag-a|tag-b,plan display,\
             FAILED,\"Windows, 64-bit\",{run_id},1,2,0,3"
        )
    );
    assert_eq!(rows.next(), None, "exactly one data row");
    assert!(
        csv.ends_with('\n'),
        "the section ends with a blank line, which `str::lines` does not \
         yield as an element: {csv:?}"
    );
}

/// `overview_to_csv`'s bucket loop, legacy `:2290-2294`: the three buckets
/// are visited `passed`, `failed`, `not_run`, in that order. The test above
/// populates only `failed`, so it never observes this — one item per bucket,
/// named so each row's identity says which bucket produced it.
#[test]
fn the_lists_section_visits_buckets_in_passed_failed_not_run_order() {
    let lists = AnalyticsLists {
        passed: vec![sample_list_item("tests/passed.py")],
        failed: vec![sample_list_item("tests/failed.py")],
        not_run: vec![sample_list_item("tests/not_run.py")],
    };
    let fixture = Fixture {
        summary: OverviewSummary::default(),
        lists,
        heatmap: empty_heatmap(),
        trend: empty_trend(),
        flaky: Vec::new(),
        platform_names: HashMap::new(),
    };

    let csv = overview_to_csv("lists", &fixture.view());
    let buckets: Vec<&str> = csv
        .lines()
        .skip(1) // the header
        .map(|line| line.split(',').nth(1).expect("a bucket column"))
        .collect();

    assert_eq!(buckets, vec!["passed", "failed", "not_run"]);
}

/// `overview_to_csv:2317-2335`: the day axis becomes header columns after the
/// three fixed ones, in [`HeatmapData::days`]' order.
#[test]
fn the_heatmap_section_puts_the_day_axis_after_the_three_fixed_columns() {
    let fixture = Fixture {
        summary: OverviewSummary::default(),
        lists: AnalyticsLists::default(),
        heatmap: HeatmapData {
            days: vec![date!(2026 - 08 - 20), date!(2026 - 08 - 21)],
            rows: vec![HeatmapRow {
                test_file: "tests/a.py".to_owned(),
                test_name: "a display".to_owned(),
                values: vec!["PASSED", "FAILED"],
            }],
        },
        trend: empty_trend(),
        flaky: Vec::new(),
        platform_names: HashMap::new(),
    };

    let csv = overview_to_csv("heatmap", &fixture.view());
    let mut rows = csv.lines();
    assert_eq!(
        rows.next().unwrap(),
        "section,test_name,test_file,2026-08-20,2026-08-21"
    );
    assert_eq!(
        rows.next().unwrap(),
        "heatmap,a display,tests/a.py,PASSED,FAILED"
    );
}

/// `overview_to_csv:2262-2367`, `"all"`: summary, lists, heatmap, trend,
/// flaky, in that order, each of the first four followed by one blank line
/// and flaky followed by none (`:2285`, `:2314`, `:2334`, `:2345` push an
/// empty line; `:2348-2364` does not).
#[test]
fn all_concatenates_the_five_sections_in_legacys_order_with_flaky_last_and_unterminated() {
    let fixture = Fixture::with_data();
    let csv = overview_to_csv("all", &fixture.view());
    let sections: Vec<&str> = csv
        .split("\n\n")
        .map(|block| block.lines().next().unwrap_or(""))
        .collect();

    assert_eq!(
        sections,
        vec![
            "section,metric,value",
            "section,bucket,test_name,test_file,component,tags,plan_name,last_status,\
             last_environment,last_run_id,pass_count,fail_count,skipped_count,total_runs",
            "section,test_name,test_file,2026-08-20",
            "section,day,passed,failed,not_run",
            "section,test_name,test_file,component,tags,pass_rate,executions,pass_count,\
             fail_count,skipped_count",
        ]
    );
    assert!(
        !csv.ends_with('\n'),
        "flaky is last and adds no trailing blank line: {csv:?}"
    );
}

/// A single-section export of anything but `flaky` ends in a trailing blank
/// line (`overview_to_csv` always pushes one after that block), and `flaky`
/// alone does not — the same asymmetry the `"all"` test above exercises, pinned
/// once on its own so a change to one section's block cannot hide behind the
/// other four.
#[test]
fn a_lone_trend_export_ends_in_a_blank_line_and_a_lone_flaky_export_does_not() {
    let fixture = Fixture::with_data();
    assert!(overview_to_csv("trend", &fixture.view()).ends_with('\n'));
    assert!(!overview_to_csv("flaky", &fixture.view()).ends_with('\n'));
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// An owned stand-in for [`crate::domain::service::analytics::AnalyticsOverview`],
/// holding exactly the fields [`ExportOverview`] borrows. Owned rather than
/// borrowed so each test can build one inline without fighting lifetimes, and
/// [`Self::view`] is the one place that turns it into the borrowed shape
/// [`overview_to_csv`] actually takes.
struct Fixture {
    summary: OverviewSummary,
    lists: AnalyticsLists,
    heatmap: HeatmapData,
    trend: TrendData,
    flaky: Vec<FlakyTest>,
    platform_names: HashMap<Uuid, String>,
}

impl Fixture {
    fn view(&self) -> ExportOverview<'_> {
        ExportOverview {
            summary: &self.summary,
            lists: &self.lists,
            heatmap: &self.heatmap,
            trend: &self.trend,
            flaky: &self.flaky,
            platform_names: &self.platform_names,
        }
    }

    /// Every section empty — for the tests that only care that nothing
    /// panics on an empty universe, or that an unrecognized section renders
    /// nothing.
    fn empty() -> Self {
        Self {
            summary: OverviewSummary::default(),
            lists: AnalyticsLists::default(),
            heatmap: empty_heatmap(),
            trend: empty_trend(),
            flaky: Vec::new(),
            platform_names: HashMap::new(),
        }
    }

    /// One row in every section, for the tests that assert section order or
    /// the blank-line boundary rather than a specific column value.
    fn with_data() -> Self {
        Self {
            summary: OverviewSummary {
                total: 10,
                passed: 6,
                failed: 3,
                not_run: 1,
                passed_pct: 60.0,
                failed_pct: 30.0,
                not_run_pct: 10.0,
                // Non-zero on purpose: `the_summary_section_exports_seven_metrics_and_no_case_counters`
                // asserts these never reach the CSV.
                case_total: 99,
                case_passed: 99,
                case_failed: 0,
                case_skipped: 0,
                case_xfail: 0,
                case_xpass: 0,
                // Also non-zero on purpose, and the one of the seven this
                // fixture used to leave at 0 even after Task 29 made it a
                // real number in production payloads: the pin above needs
                // every excluded counter actually excluded, not merely the
                // ones that happened to be non-zero already.
                case_expected: 99,
            },
            lists: {
                let mut lists = AnalyticsLists::default();
                lists.failed.push(sample_list_item("tests/a.py"));
                lists
            },
            heatmap: HeatmapData {
                days: vec![date!(2026 - 08 - 20)],
                rows: vec![HeatmapRow {
                    test_file: "tests/a.py".to_owned(),
                    test_name: "a display".to_owned(),
                    values: vec!["FAILED"],
                }],
            },
            trend: TrendData {
                points: vec![TrendPoint {
                    day: date!(2026 - 08 - 20),
                    passed: 6,
                    failed: 3,
                    not_run: 1,
                }],
            },
            flaky: vec![FlakyTest {
                test_file: "tests/a.py".to_owned(),
                test_name: "a display".to_owned(),
                component: None,
                tags: Vec::new(),
                pass_rate: 50.0,
                executions: 6,
                pass_count: 3,
                fail_count: 3,
                skipped_count: 0,
            }],
            platform_names: HashMap::new(),
        }
    }
}

fn empty_heatmap() -> HeatmapData {
    HeatmapData {
        days: Vec::new(),
        rows: Vec::new(),
    }
}

fn empty_trend() -> TrendData {
    TrendData { points: Vec::new() }
}

/// One item per list-rendering test, with every field filled so a test can
/// override only the ones it cares about via `..sample_list_item(..)`.
/// [`AnalyticsListItem`] derives no `Default`, so every field is named here
/// once rather than at every call site.
fn sample_list_item(test_file: &str) -> AnalyticsListItem {
    AnalyticsListItem {
        test_file: test_file.to_owned(),
        test_name: format!("{test_file} display"),
        component: None,
        tags: vec!["tag-a".to_owned(), "tag-b".to_owned()],
        plan: PlanRef {
            repo_id: Uuid::from_u128(1),
            plan_path: "plans/a.yaml".to_owned(),
        },
        plan_name: "plan display".to_owned(),
        versions: Vec::new(),
        last_status: "FAILED",
        last_platform_id: None,
        last_run_id: None,
        last_build: None,
        last_run_finished_at: None,
        pass_count: 1,
        fail_count: 2,
        skipped_count: 0,
        total_runs: 3,
        case_status: None,
        case_tickets: Vec::new(),
    }
}
