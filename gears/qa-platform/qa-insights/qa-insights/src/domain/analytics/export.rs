//! The analytics export — `GET /qa/v1/analytics/export`. Task 26.
//!
//! The port of legacy's `api_export` (`manager/src/routes/analytics.rs:457-519`),
//! its query at `:35-48`, `overview_section_json` (`:2234-2260`),
//! `overview_to_csv` (`:2262-2367`) and `csv_escape` (`:2369-2375`). All five
//! citations land at exactly those lines, read against the `../vhp-testrunner`
//! working tree on 2026-08-24.
//!
//! Legacy's export is not a new read: `api_export` builds a normal
//! `AnalyticsOverviewQuery` from its own first nine fields, calls
//! `build_overview` and then *reduces* the result to one section (or all of
//! them), rendered as JSON or CSV. This module is that reduction, over
//! [`AnalyticsOverview`] — Task 25b's assembly of the same eight sections this
//! gear already computes for `GET /qa/v1/analytics/overview`. Nothing here reads
//! anything; `crate::api::rest::handlers::analytics_export` calls
//! [`AnalyticsService::overview`](crate::domain::service::analytics::AnalyticsService::overview)
//! first, exactly as the overview endpoint does, and hands the result here.
//!
//! # The `section` vocabulary is six spellings, and three of the eight sections
//! # are unreachable by name
//!
//! `overview_section_json`'s `match` (`:2238-2250`) and `overview_to_csv`'s
//! `include` closure (`:2265`, `section == "all" || section == name`) agree on
//! exactly six values: `summary`, `lists`, `heatmap`, `trend`, `flaky`, `all`.
//! **`build_distribution`, `quality_vectors` and `grouped` have no name of their
//! own** — legacy's `AnalyticsOverviewResponse` carries all eight
//! (`analytics.rs:231-248`, sixteen fields including the query echo), so those
//! three are visible only through `section=all`, which serializes or
//! CSV-renders the whole payload. This is legacy's own gap, not a gear
//! omission, and [`ExportSection`] reproduces it rather than adding the three
//! missing arms: `parse_export_section` ported verbatim would still refuse
//! `?section=quality_vectors` today, and doing anything else would be scope
//! this task was not asked for.
//!
//! # The `format` vocabulary has no rejection, and that asymmetry is legacy's
//!
//! `api_export` normalizes both parameters the same way — trim, lower-case,
//! default (`format` to `"json"` at `:475-480`, `section` to `"all"` at
//! `:481-486`) — and then only `section` is checked against a vocabulary. The
//! branch on `format` (`:488`, `if format == "csv"`) has no `else if`. After
//! normalization, `"CSV"`, `" Csv "` and the like all *do* match, because
//! trimming and lower-casing collapse them to the literal `csv` first; what
//! falls through to the JSON branch is anything normalization leaves as
//! something other than `csv` — `"xml"`, a typo, or an absent parameter.
//! [`is_csv_format`] is that one boolean, ported without inventing a `400`
//! legacy never returns.
//!
//! # The two branches disagree about whether an unknown `section` is an error,
//! # and that is the sharpest legacy quirk this module ports
//!
//! `overview_section_json` validates `section` and answers **400** for anything
//! outside the six spellings (`:2245-2250`). `overview_to_csv` never validates
//! it: its `include` closure just compares strings, so an unrecognized `section`
//! matches none of the five `if include(...)` blocks and the function returns
//! `lines.join("\n")` over an **empty** `Vec` — an empty string, `200 OK`, no
//! error. So `?section=bogus&format=csv` and `?section=bogus` (format defaults
//! to JSON) answer completely differently for the same `section`, and this is
//! not a bug to fix: [`overview_to_csv`] is never given the chance to validate
//! because nothing calls [`parse_export_section`] on that branch, exactly as
//! legacy's two functions never call each other's checks.
//! `an_unrecognized_csv_section_renders_an_empty_body_not_an_error` pins it.
//!
//! # `csv_escape`'s blind spots are the point, not a defect to harden away
//!
//! [`csv_escape`] quotes a value that contains a comma, a double quote or a
//! `\n`, and doubles embedded quotes inside the wrapper (`:2370-2374`). Two
//! things it deliberately does **not** do, because legacy does not:
//!
//! * **A bare `\r` is not a quoting trigger.** `value.contains('\n')` tests
//!   exactly one control character, not the line-ending pair; a lone carriage
//!   return sails through unescaped. A future reader "fixing" this would be
//!   introducing a difference from legacy, not removing one.
//! * **A leading `=` (or `+`, `-`, `@`) is not escaped.** Legacy's CSV writer
//!   predates the CSV-injection mitigations some spreadsheet tools now expect,
//!   and adding one here would be the same kind of drift — a client comparing
//!   this endpoint's byte-for-byte output against legacy's would see a
//!   difference legacy never had a reason to produce.
//!
//! Ported as measured. If either blind spot is a real security concern for this
//! deployment, that is a product decision for a follow-up task, not a silent
//! change smuggled into a port whose whole job is behavioral parity.
//!
//! # Why `overview_to_csv` lives here and `overview_section_json` does not
//!
//! [`overview_to_csv`] builds its body with `format!` and plain string
//! concatenation — no serialization framework, exactly as legacy's does — so it
//! is a pure function over already-computed aggregate values, the same shape as
//! every fold in [`super::aggregates`]. `overview_section_json`'s JSON case is
//! different in kind: it calls `serde_json::to_string_pretty` on the *response*
//! type, and this gear's domain layer carries no **wire-contract** `serde` at
//! all — `crate::api::rest::dto`'s module header states the boundary
//! explicitly: "these types never leak into ... the domain layer ... the
//! domain layer speaks SDK models plus `DomainError`." (Qualified in the
//! Phase B fix wave, Finding 7: this used to say "no `serde` at all", full
//! stop, which stopped being true once `domain::service::collect` derived
//! `serde::Serialize` on a private struct to drive `serde_urlencoded` for its
//! own outbound query string. That derive has no bearing on the argument
//! below — it is not a response type, not `serde_json`, and never crosses
//! the REST boundary — but the absolute phrasing was no longer accurate
//! anywhere it appeared.) Moving JSON assembly into this module
//! would mean either giving the pure aggregate types a wire `Serialize` (which
//! is exactly the leak that header forbids) or duplicating every DTO as a
//! second, domain-side mirror. Neither is warranted for one export endpoint, so
//! the **vocabulary and its 400** live here — [`ExportSection`] and
//! [`parse_export_section`], the same kind of pure validation
//! [`super::query`] already holds — and the JSON *rendering* lives at the REST
//! boundary, in `crate::api::rest::handlers::analytics`, over the same
//! `AnalyticsOverviewDto` the overview endpoint already builds.
//!
//! # [`ExportOverview`] is a borrowed view, not a dependency on the service layer
//!
//! [`overview_to_csv`] needs five of [`AnalyticsOverview`]'s fields — the same
//! five `section` can name — plus the platform-name map every list item's
//! `last_platform` column reads. Taking `&AnalyticsOverview` directly would
//! make `domain::analytics` (the pure-cores layer) depend on
//! `domain::service::analytics` (the orchestration layer built *on top of* it),
//! which is backwards everywhere else in this phase:
//! `crate::domain::service::analytics` imports [`super::aggregates`],
//! [`super::query`] and [`super::universe`], never the other way around. This
//! borrowed struct keeps that direction intact and, as a side effect, makes the
//! CSV builder testable with a handful of fixture values and no
//! `AnalyticsService`, `CatalogReader` or `PlatformReader` in sight.
//!
//! # `last_run`, again
//!
//! Legacy's list-item CSV row carries `last_run` as a run **name** — the same
//! field [`crate::api::rest::dto::AnalyticsListItemDto`]'s header renamed to
//! `last_run_id` for the wire, because "there is no run-name read in this
//! gear". The CSV column here is renamed the same way, to `last_run_id`, and
//! carries the id's own `Display` rather than a name that does not exist. Not a
//! new decision — the one Task 25b already made for the same field on the JSON
//! side, applied to the second surface that renders it.

use std::collections::HashMap;

use uuid::Uuid;

use crate::domain::analytics::aggregates::{
    AnalyticsListItem, AnalyticsLists, FlakyTest, HeatmapData, OverviewSummary, TrendData,
};
use crate::domain::error::DomainError;

/// The borrowed slice of [`AnalyticsOverview`](crate::domain::service::analytics::AnalyticsOverview)
/// [`overview_to_csv`] reads.
///
/// See this module's header for why this is a view rather than the whole
/// struct. Every field is a direct reference into the caller's
/// `AnalyticsOverview` — nothing here is cloned or owned.
#[derive(Clone, Copy, Debug)]
pub struct ExportOverview<'a> {
    pub summary: &'a OverviewSummary,
    pub lists: &'a AnalyticsLists,
    pub heatmap: &'a HeatmapData,
    pub trend: &'a TrendData,
    pub flaky: &'a [FlakyTest],
    /// The label side of every `last_platform_id` a list item carries. See
    /// [`crate::domain::service::analytics::AnalyticsOverview::platform_names`]
    /// for why the join is a map lookup rather than a value already on the
    /// item.
    pub platform_names: &'a HashMap<Uuid, String>,
}

/// Which slice of the overview a `format=json` export may name.
///
/// Legacy's six spellings, verbatim — see this module's header for why only
/// six of the eight sections have one. `Copy`, like [`super::query::Scope`]: a
/// closed, cheap-to-copy vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportSection {
    Summary,
    Lists,
    Heatmap,
    Trend,
    Flaky,
    /// The whole payload — every field of
    /// [`crate::api::rest::dto::AnalyticsOverviewDto`] on the JSON branch, and
    /// the concatenation of the other five (minus `All` itself) on the CSV
    /// branch. The two branches' notions of "all" are independent code paths
    /// that happen to agree only because both were ported from the same
    /// legacy source; a change to one does not have to keep the other in
    /// sync, and there is no shared constant enforcing that it will.
    All,
}

/// `true` exactly when the caller asked for CSV.
///
/// `api_export`'s `format` normalization (`:475-480`) plus its only use
/// (`:488`). Trimmed and case-folded like every other string parameter in this
/// phase; **absent, blank or unrecognized all mean JSON**, and none of them is
/// a `400` — see this module's header for why that asymmetry with `section` is
/// legacy's own and not ported by accident.
#[must_use]
pub fn is_csv_format(value: Option<&str>) -> bool {
    value.unwrap_or("json").trim().eq_ignore_ascii_case("csv")
}

/// `section`, trimmed, case-folded and defaulted to `all` — legacy's
/// `:481-486`, shared by both branches before either looks at the vocabulary.
///
/// The CSV branch uses this string as-is, unchecked; the JSON branch passes it
/// on to [`parse_export_section`]. Splitting the shared normalization from the
/// JSON-only validation is what lets the two branches disagree about an
/// unrecognized value without duplicating the trim/lower-case/default step.
#[must_use]
pub fn normalize_export_section(value: Option<&str>) -> String {
    value.unwrap_or("all").trim().to_ascii_lowercase()
}

/// The **JSON** branch's vocabulary check — legacy's `overview_section_json`
/// `match` (`:2238-2250`), including its `_` arm's message verbatim.
///
/// Takes the already-normalized string [`normalize_export_section`] produces,
/// not the raw query parameter — mirroring `overview_section_json`, which
/// receives `section` from `api_export` post-normalization (`:505`) rather
/// than normalizing it itself.
///
/// # Errors
///
/// [`DomainError::Validation`] on `section`, naming legacy's six spellings, for
/// anything else. **Never called by the CSV branch** — see this module's
/// header for why that is deliberate rather than an oversight.
pub fn parse_export_section(normalized: &str) -> Result<ExportSection, DomainError> {
    match normalized {
        "summary" => Ok(ExportSection::Summary),
        "lists" => Ok(ExportSection::Lists),
        "heatmap" => Ok(ExportSection::Heatmap),
        "trend" => Ok(ExportSection::Trend),
        "flaky" => Ok(ExportSection::Flaky),
        "all" => Ok(ExportSection::All),
        _ => Err(DomainError::Validation {
            field: "section".to_owned(),
            message: "section must be one of: summary, lists, heatmap, trend, flaky, all"
                .to_owned(),
        }),
    }
}

/// Quote a CSV field exactly as legacy does — and exactly as it does not.
///
/// `csv_escape` (`:2369-2375`). Quotes only when `value` contains a comma, a
/// double quote or a `\n`, doubling embedded quotes inside the wrapper.
/// **Not** a trigger: a bare `\r` (legacy tests `\n` alone, not the line-ending
/// pair) and a leading `=`, `+`, `-` or `@` (legacy predates
/// spreadsheet-formula-injection mitigations). See this module's header —
/// these are ported blind spots, not gaps to close.
#[must_use]
pub fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// The CSV body for one export section, or the concatenation of five of them
/// for `"all"`.
///
/// `overview_to_csv` (`:2262-2367`). `section` is the already-normalized,
/// **unchecked** string [`normalize_export_section`] produces — this function
/// validates nothing, exactly as legacy's does not, which is what makes an
/// unrecognized value render an empty body rather than fail. See this module's
/// header for the full argument.
///
/// # Section order and the blank-line separator, both load-bearing
///
/// `"all"` renders summary, then lists, then heatmap, then trend, then flaky,
/// each block (**except flaky**, which is always last) followed by one blank
/// line — legacy's own asymmetry (`:2285`, `:2314`, `:2334`, `:2345` each push
/// an empty line; the flaky block at `:2348-2364` does not). So a single-section
/// export of anything but `flaky` ends in a trailing blank line and a
/// single-section export of `flaky` does not — reproduced rather than
/// smoothed over, because a byte-for-byte comparison against legacy's output
/// is exactly the property this port exists to preserve.
#[must_use]
pub fn overview_to_csv(section: &str, overview: &ExportOverview<'_>) -> String {
    let mut lines: Vec<String> = Vec::new();
    let include = |name: &str| section == "all" || section == name;

    if include("summary") {
        push_summary_rows(&mut lines, overview.summary);
    }
    if include("lists") {
        push_list_rows(&mut lines, overview.lists, overview.platform_names);
    }
    if include("heatmap") {
        push_heatmap_rows(&mut lines, overview.heatmap);
    }
    if include("trend") {
        push_trend_rows(&mut lines, overview.trend);
    }
    if include("flaky") {
        push_flaky_rows(&mut lines, overview.flaky);
    }

    lines.join("\n")
}

/// The `summary` block — `:2267-2286`. **Seven metrics, not fourteen**: legacy
/// exports only `total`, `passed`, `failed`, `not_run` and their three
/// percentages, and never the six per-case counters or `case_expected` that
/// [`OverviewSummary`] also carries and the JSON branch's `summary` section
/// does render. Reproduced rather than "completed" — the omission is legacy's
/// `overview_to_csv` body, not a gap in this port.
fn push_summary_rows(lines: &mut Vec<String>, summary: &OverviewSummary) {
    lines.push("section,metric,value".to_owned());
    lines.push(format!("summary,total,{}", summary.total));
    lines.push(format!("summary,passed,{}", summary.passed));
    lines.push(format!("summary,failed,{}", summary.failed));
    lines.push(format!("summary,not_run,{}", summary.not_run));
    lines.push(format!("summary,passed_pct,{}", summary.passed_pct));
    lines.push(format!("summary,failed_pct,{}", summary.failed_pct));
    lines.push(format!("summary,not_run_pct,{}", summary.not_run_pct));
    lines.push(String::new());
}

/// The `lists` block — `:2288-2315`. One row per universe entry, across all
/// three buckets in legacy's order (`passed`, `failed`, `not_run`).
fn push_list_rows(lines: &mut Vec<String>, lists: &AnalyticsLists, names: &HashMap<Uuid, String>) {
    lines.push(
        "section,bucket,test_name,test_file,component,tags,plan_name,last_status,\
         last_platform,last_run_id,pass_count,fail_count,skipped_count,total_runs"
            .to_owned(),
    );
    for (bucket, items) in [
        ("passed", &lists.passed),
        ("failed", &lists.failed),
        ("not_run", &lists.not_run),
    ] {
        for item in items {
            lines.push(list_item_row(bucket, item, names));
        }
    }
    lines.push(String::new());
}

/// One `lists` row. `:2296-2311`, with `last_run_id` in place of legacy's
/// `last_run_name` — this module's header says why.
fn list_item_row(bucket: &str, item: &AnalyticsListItem, names: &HashMap<Uuid, String>) -> String {
    let last_platform = item
        .last_platform_id
        .and_then(|id| names.get(&id))
        .map_or("", String::as_str);
    let last_run_id = item
        .last_run_id
        .map(|id| id.to_string())
        .unwrap_or_default();

    format!(
        "lists,{},{},{},{},{},{},{},{},{},{},{},{},{}",
        bucket,
        csv_escape(item.test_name.as_str()),
        csv_escape(item.test_file.as_str()),
        csv_escape(item.component.as_deref().unwrap_or("")),
        csv_escape(item.tags.join("|").as_str()),
        csv_escape(item.plan_name.as_str()),
        item.last_status,
        csv_escape(last_platform),
        csv_escape(last_run_id.as_str()),
        item.pass_count,
        item.fail_count,
        item.skipped_count,
        item.total_runs,
    )
}

/// The `heatmap` block — `:2317-2335`. The day axis becomes header columns
/// after the three fixed ones, in the same order [`HeatmapData::days`] carries.
fn push_heatmap_rows(lines: &mut Vec<String>, heatmap: &HeatmapData) {
    let mut header = vec![
        "section".to_owned(),
        "test_name".to_owned(),
        "test_file".to_owned(),
    ];
    header.extend(heatmap.days.iter().map(|day| csv_escape(&day.to_string())));
    lines.push(header.join(","));

    for row in &heatmap.rows {
        let mut line = vec![
            "heatmap".to_owned(),
            csv_escape(row.test_name.as_str()),
            csv_escape(row.test_file.as_str()),
        ];
        line.extend(row.values.iter().map(|value| csv_escape(value)));
        lines.push(line.join(","));
    }
    lines.push(String::new());
}

/// The `trend` block — `:2337-2346`. No field here is ever escaped in legacy —
/// a day, and three counts — so none is here either.
fn push_trend_rows(lines: &mut Vec<String>, trend: &TrendData) {
    lines.push("section,day,passed,failed,not_run".to_owned());
    for point in &trend.points {
        lines.push(format!(
            "trend,{},{},{},{}",
            point.day, point.passed, point.failed, point.not_run
        ));
    }
    lines.push(String::new());
}

/// The `flaky` block — `:2348-2364`. **No trailing blank line** — see this
/// function's caller's doc for why that is load-bearing rather than an
/// oversight.
fn push_flaky_rows(lines: &mut Vec<String>, flaky: &[FlakyTest]) {
    lines.push(
        "section,test_name,test_file,component,tags,pass_rate,executions,pass_count,\
         fail_count,skipped_count"
            .to_owned(),
    );
    for item in flaky {
        lines.push(format!(
            "flaky,{},{},{},{},{},{},{},{},{}",
            csv_escape(item.test_name.as_str()),
            csv_escape(item.test_file.as_str()),
            csv_escape(item.component.as_deref().unwrap_or("")),
            csv_escape(item.tags.join("|").as_str()),
            item.pass_rate,
            item.executions,
            item.pass_count,
            item.fail_count,
            item.skipped_count,
        ));
    }
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod export_tests;
