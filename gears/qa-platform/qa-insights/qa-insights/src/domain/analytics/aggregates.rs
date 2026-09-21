//! The analytics aggregates: the summary, the per-test tallies, the per-case
//! roll-up, the three lists, the two charts, the flaky detector, the quality
//! vectors, the three group breakdowns, the build distribution and the
//! build-tests drill-down.
//!
//! Legacy's `build_stats_map` (`manager/src/routes/analytics.rs:1212`),
//! `build_summary` (`:1227`), `effective_case_status` (`:1270`),
//! `attach_case_data` (`:1288`), `build_lists` (`:1397`), `build_heatmap`
//! (`:1451`), `build_trend` (`:1495`), `build_flaky` (`:1655`),
//! `build_quality_vector_summary` (`:1047`), `build_grouped_summaries` (`:1085`),
//! `apply_universe_group_filter` (`:1148`), `latest_per_test_snapshot` (`:1615`),
//! `build_last_run_build_distribution` (`:1536`), `build_status_rank` (`:1948`),
//! `compare_build_desc` (`:2188`) and the pure half of `api_build_tests`
//! (`:425-452`), ported as pure functions over
//! [`universe`](super::universe)'s output. No `async`, no repository, no SQL —
//! the two reads legacy interleaves with these folds are hoisted out, and this
//! module's header records where they went.
//!
//! Task 21a shipped the first five; **Task 22 added the two charts** and, with
//! them, `recent_days` (`:2077`) and `clamp_days` (`:2084`) as the two named
//! windows [`heatmap_days`] and [`trend_days`]; **Task 23 added the four group
//! and quality-vector folds**; **Task 24 added the last five** and with them
//! closed Phase B's pure folds. **Task 25b closed the surface**:
//! [`crate::domain::service::analytics`] assembles this file in legacy's order
//! and [`crate::api::rest::dto`] renders it.
//!
//! **Task 24 shipped no route**, and could not: legacy's `api_build_tests`
//! (`:369-455`) is not a thin endpoint — it calls `normalize_overview_query`
//! (`:2392`), `load_universe_and_rows` (`:745`) and
//! `apply_universe_group_filter` before it reaches [`build_test_details`] — and
//! every one of those was Task 25's, as was the production `CatalogReader`
//! adapter without which no route here could serve. [`build_test_details`] is the
//! fold at `:425-452` and nothing more; **Task 25b registered the route over it**
//! (`GET /qa/v1/analytics/build-tests`) under controller ruling R10, beside the
//! overview's.
//!
//! # The pipeline, in the order a caller must run it
//!
//! Everything here consumes [`resolve_rows`](super::universe::resolve_rows)'
//! output and the [`build_latest_map`](super::universe::build_latest_map) fold
//! over it. A caller that skips step 1 of that pipeline gets numbers rather than
//! a failure — see that module's header.
//!
//! 1. [`build_stats_map`] over the resolved rows — the per-file pass/fail/other
//!    tallies. Independent of the latest map.
//! 2. [`build_case_data`] over the universe, the latest map and the case-level
//!    rows — the per-case counters and the per-file case signal. **Once**: both
//!    of the next two steps read it, and legacy computes it once too (`:766`).
//! 3. [`summarize`] and [`build_lists`], in either order.
//! 4. [`build_heatmap`] and [`build_trend`], in either order and independent of
//!    steps 1-3 — they read the resolved rows and the universe directly, not the
//!    stats map or the latest map.
//! 5. [`build_flaky`], [`build_quality_vector_summary`] and
//!    [`build_grouped_summaries`], in any order and independent of everything
//!    above. [`build_quality_vector_summary`] reads the **universe alone** — its
//!    input is [`UniverseTest::quality_vectors`], not a row — and
//!    [`build_grouped_summaries`] builds its own latest maps, one per platform,
//!    rather than consuming step 2's.
//! 6. [`build_last_run_build_distribution`] (legacy runs it here, at `:782`) and
//!    [`build_test_details`], which is a *different endpoint's* fold and never
//!    runs in the same request as the other nine. Both are folds over
//!    [`latest_per_test_snapshot`], which is a second latest-per-file scan and
//!    **not** step 1's or step 2's — it keeps a status the latest map has already
//!    bucketized away. Neither reads the stats map, the case data or the latest
//!    map, so both are independent of steps 1-5.
//!
//! # [`apply_universe_group_filter`] narrows *some* of the folds, and getting
//! # this wrong collapses the group chart to one bar
//!
//! It is **not** a step in the pipeline above: legacy calls it once, at
//! `:749-750`, and then hands two different universes to two different sets of
//! folds. Which fold gets which is not a detail — it is the difference between a
//! group chart and a single selected bar:
//!
//! | Fold | Universe / rows it gets | Legacy line |
//! |---|---|---|
//! | [`build_grouped_summaries`] | the **unfiltered** universe and **all** rows | `:747` — *before* the filter call |
//! | [`build_quality_vector_summary`] | never narrowed at all; its map is built inside `load_universe_and_rows` | `:744-745`, map at `:871-882` |
//! | [`build_latest_map`](super::universe::build_latest_map), [`build_stats_map`], [`summarize`], [`build_lists`], [`build_heatmap`], [`build_trend`], the build distribution and **[`build_flaky`]** | `filtered_universe` + `rows_for_scope` | `:751-783`, `build_flaky` at `:783` |
//!
//! So of the three folds in step 5, **exactly one takes the filtered universe**
//! and it is [`build_flaky`]. An assembler that narrowed the other two would
//! compute the group breakdown over the selected group — one bar, in a chart whose
//! job is comparison — and shrink [`QualityVectorSummary::total_tests`] to the
//! selection.
//!
//! (This header said the filter "runs *before* all of it, narrowing the universe
//! every later fold then reads", citing `:791`. Both halves were wrong: `:791` is
//! `branch: query.branch.clone()`, and `:747` puts the grouped summaries ahead of
//! the filter. Retracted rather than deleted, because it was an instruction to
//! Task 25 and a reader who followed it once may follow it again.)
//!
//! Steps 1-3 are date-blind; step 4 is not, and step 5 is half — [`build_flaky`]
//! windows and the other two do not. Every date-aware fold takes a `today: Date`,
//! and
//! **the caller must read its clock once and pass the same value to both** —
//! **the caller must read its clock once and pass the same value to all of
//! them** — `days` labelled columns and `days` labelled points that disagree by a
//! day are the failure, and a flaky window that opened a day away from the trend
//! chart beside it is the same failure one screen over.
//! [`Clock`](crate::domain::ports::Clock) is the port that value
//! comes from and its header carries the argument for taking a `Date` here rather
//! than the port itself.
//!
//! Legacy's own order is `:761-783`, and it differs in one way that is a
//! consequence of the hoisted read rather than a choice: `attach_case_data` is
//! called *after* `build_summary` and `build_lists` and mutates both of them in
//! place, because it is `async` and they are not. With the read lifted out there
//! is nothing to defer, so the case roll-up becomes an input to the two folds
//! instead of a patch applied to their outputs. The numbers are identical; what
//! changes is that a caller can no longer forget the patch and ship a summary
//! whose six per-case counters are all zero.
//!
//! # The two reads that are not here
//!
//! * **The per-case rows.** `attach_case_data` runs its own four-column query
//!   (`:1308-1319`) against `test_case_results`, keyed on the set of run names
//!   the latest map names. Here that read is the caller's and its result arrives
//!   as `&[`[`super::CaseRow`]`]` — see that type for why it is neither
//!   `qa_insights_sdk::TestCaseResultRecord` nor
//!   [`NewTestCaseResult`](crate::domain::repos::NewTestCaseResult).
//!
//!   **A successful read of zero rows is not [`CaseData::default`], and the
//!   difference costs every synthetic case.** Legacy has two early returns — no
//!   run names (`:1304-1306`) and a *failed* query (`:1326-1329`) — and both
//!   leave the six counters at zero. A query that succeeds and returns nothing
//!   does **not** return early: `by_key` is simply empty, so every universe file
//!   falls into the fallback arm at `:1371-1376` and each `PASSED`/`FAILED` file
//!   contributes one case of its file-level status. So the faithful value for
//!   "this universe has no case rows" is `build_case_data(universe, latest, &[])`,
//!   which is *not* all zeros —
//!   `a_file_without_case_rows_contributes_one_case_of_its_file_status` asserts
//!   exactly that. [`CaseData::default`] is the **failed-read** value only; see
//!   that type's header.
//! * **`case_expected`.** Legacy fills it in the handler, not in `build_summary`
//!   (`:769-779`), from `load_collect_counts` (`:2672-2693`) with
//!   `UniverseTest::static_case_count` as the per-file fallback. That fold is
//!   [`expected_cases`](super::universe::expected_cases) (Task 29), which
//!   lives beside the rest of the universe's pure folds in
//!   [`super::universe`] and is called from
//!   `domain::service::analytics::AnalyticsService::overview`, after this
//!   function returns, over its own collect-table read.
//!   [`OverviewSummary::case_expected`] is therefore present and left at zero
//!   here, which is precisely what legacy's `build_summary` does at `:1264` —
//!   the field is on the response shape and this fold is not what computes it.
//!
//! # Five status rules meet in this file and none of them is the others
//!
//! `domain::service::ingest`'s header tabulates the seven classifications legacy
//! contains — five until Task 21b's Step 0 found the sixth and six until Task
//! 24's found the seventh, and this heading said three, then four, before it said
//! five. **Four of the seven partition a status and a fifth only ranks one**; the
//! table below is the four, and the paragraph after it is the fifth, which is why
//! the heading counts five and the table has four rows. The whole reason
//! [ruling R5](crate::domain::service::ingest::classify) exists is that they look
//! interchangeable:
//!
//! | Where | Rule | Third class |
//! |---|---|---|
//! | [`build_stats_map`] (`:1218-1222`) | `PASSED` / `FAILED`+`ERROR` / everything else | counted, as `skipped_count` |
//! | [`bucketize_status`](super::universe::bucketize_status) (`:1940-1946`) | the same three-way split | counted, as `NOT_RUN` |
//! | [`effective_case_status`] (`:1271`) | a six-candidate *severity* pick | not a bucket at all |
//! | [`latest_per_test_snapshot`] (`:1633-1639`) | `PASSED` / `FAILED`+`ERROR` / `SKIPPED` / **everything else verbatim** | kept, as itself |
//!
//! **The fifth is [`build_status_rank`]** (`:1948-1955`), and it is deliberately
//! not a row above because it partitions nothing: it never merges two statuses'
//! counts, it only decides which is drawn first. It is a row of ruling R5's index
//! because it does read a status vocabulary, and the vocabulary it reads is the
//! fourth row's — its `other => 3` arm exists precisely because that row passes
//! an unknown status through.
//!
//! **The first two are the same partition under two names**, and the universe
//! module's header claimed otherwise — it said `build_stats_map` "counts a
//! `SKIPPED` row as skipped rather than as not-run", implying an arithmetic
//! difference. Measured against `:1218-1222` and `:1940-1946` there is none: both
//! match `PASSED`, then `FAILED | ERROR`, then a catch-all. Only the *label* of
//! the catch-all differs, and it differs because the two feed different columns
//! of the UI. Corrected in that header by this task.
//!
//! **Task 22 doubled the number of folds that *call* the second row.** Before it
//! there was exactly one — [`build_latest_map`](super::universe::build_latest_map)
//! (`universe.rs:487`); the summary and the three lists read the result
//! transitively, through
//! [`LatestInfo::status_bucket`](super::universe::LatestInfo::status_bucket), and
//! never invoke the rule themselves. [`build_heatmap`] and [`build_trend`] are the
//! second and third call sites, and they are where reaching for the wrong
//! classifier is least visible — a `SKIPPED` cell renders exactly like a day on
//! which nothing ran. `skipped_buckets_as_not_run` pins the rule at those two
//! folds, which is where the mistake would be made.
//!
//! **Task 23 added a fourth call site and a fifth reader.** [`build_flaky`] calls
//! the *first* row, not the second, and it does so through the same `tally` helper
//! [`build_stats_map`] uses — legacy writes that match twice, character for
//! character (`:1218-1222`, `:1667-1671`), and
//! `flaky_and_the_per_test_tally_split_a_status_the_same_way` asserts the two
//! still agree. [`build_grouped_summaries`] is the fifth *reader* of the second
//! row and calls it only transitively, through two latest maps of its own.
//!
//! **And a sixth classification is Task 23's without appearing in this file at
//! all.** The *dashboard's* flaky fold counts under ruling R5's sixth row — the
//! `PASSED`+`FAILED`+`ERROR` denominator — and groups by `test_name`, so it is
//! neither this file's grain nor this file's partition. [`build_flaky`]'s header
//! states the difference; `crate::domain::service::dashboard`'s `kpi_of` is where
//! that rule already lives.
//!
//! **Task 24 added a seventh, and it is the fourth row above rather than an
//! absentee.** [`latest_per_test_snapshot`] is a *second* latest-per-file scan
//! standing beside [`build_latest_map`](super::universe::build_latest_map), and
//! the only thing that makes it a second one is the status: the latest map keeps
//! `bucketize_status`' three words and this one keeps the runner's. So the two
//! folds answer "what is the newest row for this file" differently, and the
//! difference is invisible until a `SKIPPED` renders as never-run or an `XFAIL`
//! renders as `NOT_RUN`. Neither of the two folds that read it calls
//! `bucketize_status` at all, which is why the call-site count above is unchanged
//! by this task.
//!
//! The third is genuinely different and is the one to keep separate: it ranks
//! `FAILED > ERROR > XPASS > XFAIL > SKIPPED > PASSED` and folds `ERROR` into
//! `FAILED` on output, so it is a *case*-level severity pick and never a
//! file-level bucket. It is also the only one of the seven that can answer "no
//! opinion" **in its return type** — `None`, when the file's latest run reported
//! no case whose status is one of its six candidates. That uniqueness is narrower
//! than it was when this sentence was written for five rules, and narrower again
//! now: the sixth, the KPI denominator, leaves a row in *no* counter, and the
//! seventh's consumer does the same — an unrecognized status is passed through
//! and then counted by neither
//! [`BuildLastRunDistribution::passed`], [`BuildLastRunDistribution::failed`] nor
//! [`BuildLastRunDistribution::executed_total`]. So three of the seven decline to
//! have an opinion and the difference that still matters is where the silence
//! goes — an `Option` a caller must handle, versus a row that is simply absent
//! from a total.
//!
//! # Two windows, and they are not the same window
//!
//! [`heatmap_days`] clamps to `[1, 30]` and [`trend_days`] to `[7, 365]`. They
//! are separate query parameters in legacy (`days_heatmap`, `days_trend`) with
//! separate defaults (`7` and `90`, `:2426-2427`), and [`build_flaky`] (`:1656`)
//! windows on the **trend's** clamp rather than a third one — so [`flaky_cutoff`]
//! calls [`trend_days`] and takes [`recent_days`]' first element rather than
//! re-deriving either. **Three folds, two windows**, and
//! `the_flaky_cutoff_is_the_trend_windows_first_day` asserts the sharing rather
//! than restating the arithmetic.
//!
//! # What the platform id costs this surface
//!
//! [`AnalyticsListItem::last_environment_id`] is a `Uuid` where legacy's
//! `last_platform` was a display name, which is
//! [`ExecRow::environment_id`](super::ExecRow::environment_id)'s carried obligation
//! surfacing. This module resolves nothing and adds no lookup: the
//! id travels to the DTO, and the task that owns the resolution owns it there
//! too.
//!
//! **Task 23 was named as that task and could not discharge it**, because the
//! lookup is a qa-environments read and this gear has no port for one — see
//! [`PlatformGroupSummary`], which is where the obligation now lives and which
//! makes it a type rather than a paragraph. The difference from the list item is
//! that the id there sits beside a name nothing needs, where in a *group
//! breakdown* the id **is** the label; so that type carries `environment_id` rather
//! than a `value: String`, and no response DTO can be written over it without
//! deciding what the label is.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::BuildHasher;
use std::ops::RangeInclusive;

use qa_catalog_sdk::UniverseTest;
use time::{Date, Duration, OffsetDateTime};
use uuid::Uuid;

use crate::domain::analytics::universe::{
    LatestInfo, NOT_RUN, UNKNOWN_BUILD, bucketize_status, build_latest_map, collapse_build,
};
use crate::domain::analytics::{CaseRow, ExecRow, PlanRef};

/// The file-level counters and their percentages, plus the per-case roll-up.
///
/// Legacy's `OverviewSummary` (`analytics.rs:88-109`), field for field. The
/// widths are legacy's too: `usize` counters and `f64` percentages, because the
/// percentages are one-decimal quantities produced by [`pct`] and nothing
/// downstream recomputes them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverviewSummary {
    /// `universe.len()` (`:1231`) — one entry per `(repo_id, test_file)`, not
    /// per path. **This used to say a file listed by two plans is two entries
    /// and is counted twice, and that reading was wrong**: qa-catalog's
    /// `walk_repo_universe` keys its per-repository walk on `test_file` and
    /// merges a second plan's contribution into the first plan's entry
    /// (`qa-catalog/src/domain/service/plans.rs`), so a file two plans of the
    /// **same** repository both list is one universe entry and is counted
    /// once, here and in every other counter on this struct. Two **different**
    /// repositories sharing a path are a different case: each keeps its own
    /// entry, and each is counted, because they are two distinct tests that
    /// happen to share a name — that is the case
    /// [`ExecRow::repo_id`](super::ExecRow::repo_id) exists to keep distinct
    /// through every fold, not a duplicate to collapse.
    pub total: usize,
    /// Universe files whose latest bucket is `PASSED`.
    pub passed: usize,
    /// Universe files whose latest bucket is `FAILED`.
    pub failed: usize,
    /// Everything else, including every file no row ever touched — legacy's
    /// `.unwrap_or("NOT_RUN")` (`:1237-1240`) — and every file whose latest
    /// status bucketed to [`NOT_RUN`].
    pub not_run: usize,
    /// [`Self::passed`] over [`Self::total`], as a percentage; see [`pct`].
    pub passed_pct: f64,
    /// [`Self::failed`] over [`Self::total`], as a percentage.
    pub failed_pct: f64,
    /// [`Self::not_run`] over [`Self::total`], as a percentage.
    pub not_run_pct: f64,
    /// The five per-case counters below, summed (`:1385`). **Not the number of
    /// case rows**: a status outside the five is counted nowhere, so this can be
    /// smaller. Nor is it bounded below by [`Self::total`] — see
    /// [`CaseData`]'s header for the file-level fallback and its limit.
    pub case_total: usize,
    /// Cases of the latest runs with status `PASSED`.
    pub case_passed: usize,
    /// Cases with status `FAILED` **or** `ERROR` (`:1357`).
    pub case_failed: usize,
    /// Cases with status `SKIPPED`.
    pub case_skipped: usize,
    /// Cases with status `XFAIL` — an expected failure that failed.
    pub case_xfail: usize,
    /// Cases with status `XPASS` — an expected failure that passed.
    pub case_xpass: usize,
    /// The static "collect" number: how many cases the universe is *expected* to
    /// contain, available without any run.
    ///
    /// **Always zero out of [`summarize`], exactly as it is out of legacy's
    /// `build_summary` (`:1264`).** [`expected_cases`](super::universe::expected_cases)
    /// (Task 29) is what fills it, over its own collect-table read; this
    /// module's header says why that fold is not here.
    pub case_expected: usize,
}

/// The per-file pass/fail/other tallies over every row in scope.
///
/// Legacy's `HashMap<String, StatusStats>` (`analytics.rs:1212`), behind a
/// newtype for the reason [`AliasMap`](super::universe::AliasMap) is one: a
/// `pub fn` taking a bare `HashMap` trips `clippy::implicit_hasher`. It also
/// makes the lookup total — legacy spells the default at every call site
/// (`:1408`, `stats.get(..).cloned().unwrap_or_default()`) and there is only ever
/// one right answer for a file no row mentions.
///
/// **Keyed on `(repo_id, test_file)`, not `test_file` alone** — a fix-round
/// correction. A product owns several repositories and `(tenant_id,
/// product_id)` is not a unique index, so two repositories can each hold
/// `tests/test_smoke.py`; a fold keyed on the path alone would tally both
/// repositories' rows into one bucket and every list item's `pass_count`,
/// `fail_count` and `skipped_count` would count one repository's runs against
/// the other's test. See [`ExecRow::repo_id`](super::ExecRow::repo_id).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatsMap(HashMap<(Uuid, String), StatusStats>);

impl StatsMap {
    /// The tally for `(repo_id, test_file)`, or three zeros if no row
    /// mentioned it.
    ///
    /// Legacy's `:1408` with the `unwrap_or_default` folded in.
    #[must_use]
    pub fn get(&self, repo_id: Uuid, test_file: &str) -> StatusStats {
        self.0
            .get(&(repo_id, test_file.to_owned()))
            .copied()
            .unwrap_or_default()
    }
}

/// One file's tallies across every row in scope.
///
/// Legacy's `StatusStats` (`analytics.rs:278-283`), including the `u32` widths and
/// the name of the third counter. `Copy` because it is three `u32`s and
/// [`StatsMap::get`] returns it by value.
///
/// # `skipped_count` is a catch-all, not a `SKIPPED` counter
///
/// `:1221` is `_ => entry.skipped_count += 1`, so an `XFAIL`, an `XPASS`, a
/// `RUNNING` and a lowercase `passed` all land here. The name is legacy's and is
/// kept; this module's header tabulates how the same partition is named
/// [`NOT_RUN`] one fold over.
// `pass_count` / `fail_count` / `skipped_count` all end in `count`, which trips
// `clippy::struct_field_names`. The names are legacy's (`analytics.rs:280-282`)
// and `AnalyticsListItem` copies all three one for one (`:1424-1426`), so
// shortening them here would leave the port with two spellings of the same three
// numbers and nothing keeping them in agreement.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusStats {
    pub pass_count: u32,
    pub fail_count: u32,
    /// The catch-all — see this type's header.
    pub skipped_count: u32,
}

/// The per-case roll-up over the latest run of every universe file.
///
/// The pure half of `attach_case_data` (`analytics.rs:1288-1395`): the five
/// counters it writes onto the summary, and the per-file signal it stamps onto the
/// list items.
///
/// # The file-level fallback, and where "never undercount" stops being true
///
/// Legacy's doc comment says a file with no per-case rows "contributes one case of
/// its file-level status, so totals never undercount" (`:1283-1287`; `:1288` is
/// the `async fn` itself). The arm that
/// does it is `:1371-1376`, and it counts **`PASSED` and `FAILED` only**. Its
/// third pattern is `"SKIPPED"`, which cannot match: it tests
/// `LatestInfo::status_bucket`, which is
/// [`bucketize_status`](super::universe::bucketize_status)' output, and that
/// function emits three words of which `SKIPPED` is not one. So:
///
/// * a `PASSED` or `FAILED` file with no case rows contributes one case;
/// * a [`NOT_RUN`] file with no case rows contributes **nothing**;
/// * a file with no row at all is skipped before the bucket is consulted
///   (`:1346-1348`).
///
/// [`Self::total`] is therefore genuinely smaller than `OverviewSummary::total`
/// on a universe with unrun tests, and the dead `"SKIPPED"` arm is *why* the doc
/// comment overstates its own guarantee. Ported verbatim under Phase B's standing
/// instruction — the arm is omitted rather than written, because writing an arm
/// that cannot match is how a reader concludes it can — and pinned by
/// `a_not_run_file_contributes_no_synthetic_case_so_the_undercount_is_real`.
///
/// # [`Self::default`] is the **failed read**, not the empty one
///
/// `attach_case_data` returns before counting anything in exactly two situations:
/// the latest map names no run (`:1304-1306`) and the query **fails**
/// (`:1326-1329`). Both leave all six counters at zero and stamp no list item, and
/// this value is that outcome — so a caller that could not read the case rows has
/// something faithful to pass rather than a second code path. (The first of the
/// two coincides with [`build_case_data`]'s own answer: if no file has a latest
/// run there is nothing for the fallback arm to count either.)
///
/// **It is not the value for "the read succeeded and returned nothing".** That
/// path does not early-return in legacy: `by_key` is empty, every universe file
/// falls into the fallback arm at `:1371-1376`, and each `PASSED`/`FAILED` file
/// contributes one synthetic case. The faithful value there is
/// `build_case_data(universe, latest, &[])`, whose [`Self::total`] is the count of
/// run files rather than zero —
/// `a_file_without_case_rows_contributes_one_case_of_its_file_status` pins it.
///
/// The distinction is worth this many words because the two are the same *type*
/// and only one of them is reachable by accident: a caller that reaches for
/// `CaseData::default()` when it has an empty `Vec` in hand loses every synthetic
/// case, on a screen that still renders.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaseData {
    /// The five counters below, summed (`:1385`).
    pub total: usize,
    pub passed: usize,
    /// `FAILED` and `ERROR` together (`:1357`).
    pub failed: usize,
    pub skipped: usize,
    pub xfail: usize,
    pub xpass: usize,
    /// The per-file signal, keyed on `(repo_id, test_file)` — a fix-round
    /// correction from the resolved `test_file` alone, for the same
    /// cross-repository reason [`StatsMap`] carries the pair — and present
    /// **only** for a file whose latest run actually reported cases (`:1369`).
    /// A file that took the fallback arm has no entry, which is why a list item's
    /// `case_status` stays `None` rather than echoing its file-level status.
    pub by_file: HashMap<(Uuid, String), CaseFileSignal>,
}

impl CaseData {
    /// Count one case row's status. `:1355-1362`, including the `_ => {}`.
    fn count_case(&mut self, status: &str) {
        match status {
            "PASSED" => self.passed += 1,
            "FAILED" | "ERROR" => self.failed += 1,
            "SKIPPED" => self.skipped += 1,
            "XFAIL" => self.xfail += 1,
            "XPASS" => self.xpass += 1,
            // A status outside the five moves nothing, so `total` is not the row
            // count. Pinned by
            // `an_unrecognized_case_status_is_counted_in_no_case_counter`.
            _ => {}
        }
    }

    /// Count the one synthetic case a file with no case rows contributes.
    ///
    /// `:1371-1376`. Legacy's `"SKIPPED"` arm is not reproduced because
    /// `status_bucket` cannot hold that word — see this type's header.
    fn count_file_level(&mut self, status_bucket: &str) {
        match status_bucket {
            "PASSED" => self.passed += 1,
            "FAILED" => self.failed += 1,
            _ => {}
        }
    }
}

/// What the latest run's cases say about one file.
///
/// Legacy's `(Option<String>, Vec<String>)` at `:1343`, named. Both halves are
/// rendered: `AnalyticsListItem::case_status` colours the UI's dot and
/// `case_tickets` lists the bugs, "without a separate fetch" (`:1284-1285`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaseFileSignal {
    /// [`effective_case_status`] over the latest run's case statuses. `None` when
    /// none of them is one of that function's six candidates.
    pub status: Option<String>,
    /// Every distinct ticket the latest run's cases named, sorted
    /// (`:1365-1368`). Cases without a ticket contribute nothing.
    pub tickets: Vec<String>,
}

/// One row of the passed / failed / not-run lists.
///
/// Legacy's `AnalyticsListItem` (`analytics.rs:112-134`) with the four renames
/// this architecture forces — the same four
/// [`super::universe::LatestInfo`] carries, plus the plan identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalyticsListItem {
    pub test_file: String,
    pub test_name: String,
    pub component: Option<String>,
    pub tags: Vec<String>,
    /// Legacy's `plan_id`, which is a lossy path-derived slug rather than a key
    /// (`services/plans.rs:789-801`). This port carries the pair that *is* a
    /// plan's identity here — [`super::PlanRef`]'s header says why there is no
    /// UUID to carry instead.
    pub plan: PlanRef,
    pub plan_name: String,
    /// [`sorted_versions_desc`] of the universe entry's versions, which is always
    /// empty — in legacy too. See that function's header.
    pub versions: Vec<String>,
    /// The latest bucket, or [`NOT_RUN`] when no row touched this file.
    pub last_status: &'static str,
    /// **An id where legacy had a display name**; see this module's header.
    pub last_environment_id: Option<Uuid>,
    /// Legacy's `last_run_name`, which was its run identity.
    pub last_run_id: Option<Uuid>,
    /// The build the latest run executed against, already collapsed:
    /// [`UNKNOWN_BUILD`] when the row named none, the trimmed label otherwise, and
    /// `None` only when no row touched this file at all.
    ///
    /// Legacy's `last_build` (`analytics.rs:1422`), which reads
    /// `LatestInfo::build` one for one — and that field is
    /// `Some(row.build.clone())` over an **already normalized** `row.build`
    /// (`:1203`, `:1032-1033`). [`ExecRow::build`](super::ExecRow::build)'s header
    /// records why the `Option` survives this far and
    /// [`collapse_build`](super::universe::collapse_build) is where it collapses.
    ///
    /// **This field had no doc at all until controller ruling R15**, which is how
    /// a live parity gap sat behind it: the port rendered `null` where legacy
    /// renders `"unknown"`. The remaining question on it is a *label*, not a
    /// value — whether the UI should be shown the word `unknown` at all — and
    /// **Task 25b answered it: yes.**
    /// [`crate::api::rest::dto::AnalyticsListItemDto::last_build`] renders the
    /// label verbatim, so `null` there means "no latest row at all" and nothing
    /// else, which is legacy's own distinction.
    pub last_build: Option<String>,
    /// Legacy renders this as RFC-3339 at `:1423`; formatting is the DTO's, so
    /// the domain keeps the instant. It is
    /// [`ExecRow::ts`](super::ExecRow::ts) — `run_finished_at ?? run_created_at`
    /// — and so is not a claim that the run finished. The fallback is the **run's**
    /// creation instant, not the result row's; that field's doc says why the
    /// distinction is load-bearing.
    pub last_run_finished_at: Option<OffsetDateTime>,
    pub pass_count: u32,
    pub fail_count: u32,
    /// The catch-all — see [`StatusStats::skipped_count`].
    pub skipped_count: u32,
    /// The three tallies summed (`:1409`), **including** the catch-all.
    pub total_runs: u32,
    /// [`CaseFileSignal::status`] for this file, or `None` when its latest run
    /// reported no cases.
    pub case_status: Option<String>,
    /// [`CaseFileSignal::tickets`] for this file, empty when there are none.
    pub case_tickets: Vec<String>,
}

/// The universe, partitioned by latest bucket and sorted within each part.
///
/// Legacy's `AnalyticsLists` (`analytics.rs:136-141`). The three lists are a
/// partition: every universe entry is in exactly one of them, so their lengths
/// sum to `OverviewSummary::total`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnalyticsLists {
    pub passed: Vec<AnalyticsListItem>,
    pub failed: Vec<AnalyticsListItem>,
    pub not_run: Vec<AnalyticsListItem>,
}

/// A share of a total, as a percentage rounded to one decimal place.
///
/// `pct` (`analytics.rs:1957-1963`) verbatim, both halves:
///
/// * a non-positive total is `0.0`, not a `NaN` and not a division by zero — the
///   case that matters is an **empty universe**, which a filtered scope reaches
///   routinely;
/// * the rounding is `((value / total) * 1000.0).round() / 10.0`, i.e. scale to
///   tenths of a percent, round, scale back. Truncating instead of rounding, or
///   scaling by `100.0`, changes a rendered number: `2 / 3` is `66.7` under this
///   rule, `66.6` under truncation and `67.0` under whole-percent rounding.
#[must_use]
pub fn pct(value: f64, total: f64) -> f64 {
    if total <= 0.0 {
        0.0
    } else {
        ((value / total) * 1000.0).round() / 10.0
    }
}

/// The per-file pass/fail/other tallies across every row in scope.
///
/// `build_stats_map` (`analytics.rs:1212-1225`), keyed on `(repo_id,
/// test_file)` rather than legacy's bare `test_file` — a fix-round correction,
/// for the same reason [`build_latest_map`](super::universe::build_latest_map)
/// carries `repo_id`: two repositories can share a `test_file`, and legacy has
/// no such collision to guard against because it has no concept of several
/// repositories feeding one universe. `test_file` is still the grain every
/// analytics consumer uses on top of that — [`ExecRow`]'s header records that
/// `test_name` is not one — and this expects
/// [`resolve_rows`](super::universe::resolve_rows)' output, so that one file's
/// history is under one spelling.
///
/// Unlike [`build_latest_map`](super::universe::build_latest_map) this fold is
/// **order-independent**: it counts every row rather than picking one, so a caller
/// that hands it rows in the wrong order still gets the right tallies. It also
/// applies no universe-membership filter, exactly as legacy does not — the rows it
/// is handed have already been filtered to the universe by `resolve_rows`, and a
/// second filter here would be the only place a future caller could skip that
/// step without a symptom.
#[must_use]
pub fn build_stats_map(rows: &[ExecRow]) -> StatsMap {
    let mut stats: HashMap<(Uuid, String), StatusStats> = HashMap::new();

    for row in rows {
        tally(
            stats
                .entry((row.repo_id, row.test_file.clone()))
                .or_default(),
            row.status.as_str(),
        );
    }

    StatsMap(stats)
}

/// Add one row's status to a [`StatusStats`], under the three-way split.
///
/// Legacy writes this match **twice, character for character** — `:1218-1222` in
/// `build_stats_map` and `:1667-1671` in `build_flaky` — and the plan's carried
/// item for Task 23 describes the second as folding "via `build_stats_map`'s
/// three-way split" for that reason. Shared here rather than copied, because two
/// spellings of `FAILED | ERROR` in one module are two places for the vocabulary
/// to drift with nothing keeping them in agreement — the argument
/// [`ResultsRepository::recent_failures`](crate::domain::repos::ResultsRepository::recent_failures)
/// makes about its own `statuses` parameter.
///
/// It is **not** shared with [`build_flaky`] as a whole map: that fold windows its
/// rows first and keys on a borrowed `&str`, so it reuses the classification and
/// not the container. `flaky_and_the_per_test_tally_split_a_status_the_same_way`
/// asserts the two agree from the outside.
fn tally(entry: &mut StatusStats, status: &str) {
    match status {
        "PASSED" => entry.pass_count += 1,
        "FAILED" | "ERROR" => entry.fail_count += 1,
        // The catch-all, named `skipped_count` — see `StatusStats`.
        _ => entry.skipped_count += 1,
    }
}

/// The worst of a file's case statuses, or `None`.
///
/// `effective_case_status` (`analytics.rs:1270-1281`) verbatim, and its doc
/// comment verbatim too: *"Worst non-passing case status (so an xfail inside an
/// otherwise-green file wins). ERROR folds into FAILED. None when no statuses."*
///
/// Three properties, all of them load-bearing:
///
/// * The order is `FAILED > ERROR > XPASS > XFAIL > SKIPPED > PASSED` (`:1271`),
///   scanned first-match-wins over the *candidates*, not over the input — so the
///   input's order is irrelevant and a single non-passing case decides the file.
/// * `ERROR` is returned as `FAILED` (`:1273-1274`), so the two are
///   indistinguishable on output while remaining distinct in the ranking. That
///   distinction is invisible today because nothing ranks below `ERROR` and above
///   `XPASS`; it becomes visible the moment a status is inserted between them.
/// * A status outside the six is **not a candidate**, so a file whose only case is
///   `RUNNING` has no effective status at all. This is the one classification of
///   legacy's six that can answer "no opinion", and the `None` is what keeps a
///   list item's dot uncoloured rather than green.
///
/// Takes `&[String]` rather than `&[&str]` because legacy does and because the
/// caller collects owned statuses out of its case rows either way.
#[must_use]
pub fn effective_case_status(statuses: &[String]) -> Option<String> {
    for candidate in ["FAILED", "ERROR", "XPASS", "XFAIL", "SKIPPED", "PASSED"] {
        if statuses.iter().any(|status| status == candidate) {
            return Some(if candidate == "ERROR" {
                "FAILED".to_owned()
            } else {
                candidate.to_owned()
            });
        }
    }

    None
}

/// Roll the per-case rows up over the latest run of every universe file.
///
/// The pure half of `attach_case_data` (`analytics.rs:1288-1395`), minus the query
/// at `:1308-1330` and minus the in-place patching at `:1380-1394` — see this
/// module's header for both.
///
/// # The join is on `(run, file)` and the iteration is over the universe
///
/// Legacy indexes the case rows on the pair (`:1332-1339`) and then walks the
/// **universe**, looking each file up under *its own latest run* (`:1352`). Both
/// halves matter:
///
/// * A fold keyed on `test_file` alone would attribute an older run's cases to a
///   file whose latest run reported none — replacing the documented undercount
///   with a wrong count. Pinned by
///   `case_rows_from_an_older_run_are_not_the_files_cases`.
/// * Iterating the case rows instead of the universe would count cases belonging
///   to runs that are nobody's latest, and would lose the file-level fallback
///   entirely.
///
/// Rows whose `(run, file)` pair matches no universe file's latest run are
/// therefore ignored, and no error is raised: legacy's query is bounded by run
/// name only (`:1319`), so it routinely returns cases for files outside the
/// filtered scope.
///
/// [`CaseData`]'s header carries the fallback rule and the sense in which the
/// totals can still undercount.
#[must_use]
pub fn build_case_data<S: BuildHasher>(
    universe: &[UniverseTest],
    latest: &HashMap<(Uuid, String), LatestInfo, S>,
    rows: &[CaseRow],
) -> CaseData {
    let mut by_key: HashMap<(Uuid, &str), Vec<&CaseRow>> = HashMap::new();
    for row in rows {
        by_key
            .entry((row.run_id, row.test_file.as_str()))
            .or_default()
            .push(row);
    }

    let mut data = CaseData::default();

    for test in universe {
        let Some(info) = latest.get(&(test.repo_id, test.test_file.clone())) else {
            continue;
        };
        let Some(run_id) = info.run_id else {
            continue;
        };

        // The `!cases.is_empty()` guard is legacy's (`:1353`) and is vacuous —
        // `by_key` only ever holds entries a `push` created. Kept as the
        // `match`'s shape rather than reproduced as a redundant test.
        if let Some(cases) = by_key.get(&(run_id, test.test_file.as_str())) {
            for case in cases {
                data.count_case(case.status.as_str());
            }

            let statuses: Vec<String> = cases.iter().map(|case| case.status.clone()).collect();
            let mut tickets: Vec<String> = cases
                .iter()
                .filter_map(|case| case.ticket.clone())
                .collect();
            tickets.sort();
            tickets.dedup();

            data.by_file.insert(
                (test.repo_id, test.test_file.clone()),
                CaseFileSignal {
                    status: effective_case_status(&statuses),
                    tickets,
                },
            );
        } else {
            data.count_file_level(info.status_bucket);
        }
    }

    data.total = data.passed + data.failed + data.skipped + data.xfail + data.xpass;
    data
}

/// The file-level counters, their percentages, and the per-case roll-up.
///
/// `build_summary` (`analytics.rs:1227-1266`) with `attach_case_data`'s six
/// assignments (`:1380-1385`) folded in, which is what makes this a total function
/// of its inputs rather than a value that has to be patched afterwards — this
/// module's header records why the two halves merged.
///
/// # The grain is the universe, not the rows
///
/// Every counter walks `universe` and *looks up* the latest map (`:1236-1246`), so
/// a file no row ever touched is counted as [`NOT_RUN`] (`:1237-1240`,
/// `.unwrap_or("NOT_RUN")`) rather than omitted, and a row for a file the universe
/// does not list is invisible. Inverting that — iterating the rows and counting
/// what they mention — makes `total` shrink as tests go unrun, silently, and is
/// the reason this takes the universe at all.
///
/// The three counters partition the universe, so they sum to
/// [`OverviewSummary::total`]. The six per-case counters do not: see [`CaseData`].
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn summarize<S: BuildHasher>(
    universe: &[UniverseTest],
    latest: &HashMap<(Uuid, String), LatestInfo, S>,
    cases: &CaseData,
) -> OverviewSummary {
    let total = universe.len();
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut not_run = 0usize;

    for test in universe {
        match latest
            .get(&(test.repo_id, test.test_file.clone()))
            .map_or(NOT_RUN, |item| item.status_bucket)
        {
            "PASSED" => passed += 1,
            "FAILED" => failed += 1,
            _ => not_run += 1,
        }
    }

    // `usize as f64` is lossy above 2^53 entries, which is not a universe; the
    // alternative is a fallible conversion with no failure a caller could act on.
    let total_f = total as f64;

    OverviewSummary {
        total,
        passed,
        failed,
        not_run,
        passed_pct: pct(passed as f64, total_f),
        failed_pct: pct(failed as f64, total_f),
        not_run_pct: pct(not_run as f64, total_f),
        case_total: cases.total,
        case_passed: cases.passed,
        case_failed: cases.failed,
        case_skipped: cases.skipped,
        case_xfail: cases.xfail,
        case_xpass: cases.xpass,
        // Filled by the caller after this function returns —
        // `AnalyticsService::overview` calls `expected_cases` over its own
        // collect-table read (Task 29). Legacy's `build_summary` leaves it at
        // zero too (`:1264`); this module's header says why it is not here.
        case_expected: 0,
    }
}

/// The universe as three lists, split on the latest bucket and sorted by display
/// name.
///
/// `build_lists` (`analytics.rs:1397-1449`) with `attach_case_data`'s in-place
/// stamp (`:1387-1394`) applied at construction instead.
///
/// Three properties an implementation loses without noticing:
///
/// * **One item per universe entry**, so a file with no rows is a full row of the
///   `not_run` list built from [`LatestInfo::default`](super::universe::LatestInfo)
///   (`:1407`) — not an omission. This is the same grain argument as
///   [`summarize`]'s.
/// * **`total_runs` includes the catch-all tally** (`:1409`), so a test whose only
///   executions were skipped still reports runs.
/// * **Each list is sorted by `test_name`** (`:1440-1442`), by
///   `String::cmp` — a byte-wise ordering, not a locale-aware or
///   case-insensitive one — with `sort_by`, which is stable, so entries sharing a
///   display name keep their universe order.
#[must_use]
pub fn build_lists<S: BuildHasher>(
    universe: &[UniverseTest],
    latest: &HashMap<(Uuid, String), LatestInfo, S>,
    stats: &StatsMap,
    cases: &CaseData,
) -> AnalyticsLists {
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    let mut not_run = Vec::new();

    for test in universe {
        let info = latest
            .get(&(test.repo_id, test.test_file.clone()))
            .cloned()
            .unwrap_or_default();
        let tally = stats.get(test.repo_id, test.test_file.as_str());
        let signal = cases.by_file.get(&(test.repo_id, test.test_file.clone()));
        let bucket = info.status_bucket;

        let item = AnalyticsListItem {
            test_file: test.test_file.clone(),
            test_name: test.test_name.clone(),
            component: test.component.clone(),
            tags: test.tags.clone(),
            plan: PlanRef {
                repo_id: test.repo_id,
                plan_path: test.plan_path.clone(),
            },
            plan_name: test.plan_name.clone(),
            versions: sorted_versions_desc(&test.versions),
            last_status: bucket,
            last_environment_id: info.environment_id,
            last_run_id: info.run_id,
            last_build: info.build,
            last_run_finished_at: info.finished_at,
            pass_count: tally.pass_count,
            fail_count: tally.fail_count,
            skipped_count: tally.skipped_count,
            total_runs: tally.pass_count + tally.fail_count + tally.skipped_count,
            case_status: signal.and_then(|signal| signal.status.clone()),
            case_tickets: signal.map_or_else(Vec::new, |signal| signal.tickets.clone()),
        };

        match bucket {
            "PASSED" => passed.push(item),
            "FAILED" => failed.push(item),
            _ => not_run.push(item),
        }
    }

    for list in [&mut passed, &mut failed, &mut not_run] {
        list.sort_by(|left, right| left.test_name.cmp(&right.test_name));
    }

    AnalyticsLists {
        passed,
        failed,
        not_run,
    }
}

/// Version labels, trimmed, blank-free, newest first, de-duplicated.
///
/// `sorted_versions_desc` (`analytics.rs:2164-2186`) verbatim. The comparator
/// zips the two labels' dot-separated components and takes the **first** that
/// differs, comparing numerically when both sides parse as `i64` and
/// byte-wise otherwise, with the arguments swapped so the order is descending; a
/// pair that agrees on every component either side has falls back to a whole-string
/// comparison, also swapped. `dedup` then drops consecutive duplicates, which after
/// the sort is every duplicate.
///
/// # Its input is always empty, and it is ported anyway
///
/// `qa_catalog_sdk::UniverseTest::versions` is documented as always empty and so
/// is legacy's: `UniverseTest::versions` is written as `Vec::new()` at `:918` and
/// never touched again, and `TestPlanInfo::versions` likewise at
/// `services/plans.rs:581` and `:717`. So no deployment exercises the comparator.
///
/// It is here because it is a cited line of [`build_lists`] (`:1418`) and because
/// that SDK field's own doc names this function as how the value reaches the
/// screen — dropping it would make that doc false and would silently decide, on
/// behalf of whoever populates the field, that the order does not matter. The cost
/// is fifteen lines and one test.
#[must_use]
pub fn sorted_versions_desc(values: &[String]) -> Vec<String> {
    let mut items = values
        .iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    items.sort_by(|left, right| {
        right
            .split('.')
            .zip(left.split('.'))
            .find_map(|(right_part, left_part)| {
                let right_num = right_part.parse::<i64>().ok();
                let left_num = left_part.parse::<i64>().ok();
                match (right_num, left_num) {
                    (Some(right_value), Some(left_value)) if right_value != left_value => {
                        Some(right_value.cmp(&left_value))
                    }
                    _ if right_part != left_part => Some(right_part.cmp(left_part)),
                    _ => None,
                }
            })
            .unwrap_or_else(|| right.cmp(left))
    });
    items.dedup();

    items
}

// ---------------------------------------------------------------------------
// The heatmap and the trend
// ---------------------------------------------------------------------------

/// One row of the heatmap: a universe entry, and its bucket per axis day.
///
/// Legacy's `HeatmapRow` (`analytics.rs:143-147`), field for field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeatmapRow {
    /// The universe entry's file, verbatim.
    pub test_file: String,
    /// The universe entry's display name. A second `String` next to the first,
    /// so a transposition compiles —
    /// `a_heatmap_row_carries_its_file_and_its_display_name` is what catches it.
    pub test_name: String,
    /// One bucket per day of [`HeatmapData::days`], **in the same order and of
    /// the same length**, [`NOT_RUN`] where that day had no row.
    ///
    /// `&'static str` and not `String`, for
    /// [`LatestInfo::status_bucket`](super::universe::LatestInfo::status_bucket)'s
    /// reason: the value is always one of
    /// [`bucketize_status`](super::universe::bucketize_status)' three literals,
    /// and the type says so.
    pub values: Vec<&'static str>,
}

/// The heatmap: a day axis, and one row per universe entry.
///
/// Legacy's `HeatmapData` (`analytics.rs:150-153`).
///
/// # `days` is a `Date`, where legacy's is a pre-formatted `String`
///
/// Legacy formats in the fold — `day.format("%Y-%m-%d")` (`:1468` for the
/// heatmap axis, `:1526` for a trend point). Here the
/// domain keeps the date and the DTO formats it, which is the choice
/// [`AnalyticsListItem::last_run_finished_at`] already records for its instant
/// and which `api::rest::dto`'s `DailyStatusPointDto` already implements for
/// exactly this `YYYY-MM-DD` shape (`dto.rs:413-443`, formatted by hand rather
/// than left to a serde attribute).
///
/// **The obligation that travels with it belongs to the DTO task (26-27):** the
/// wire shape is `YYYY-MM-DD` in UTC, and reusing `DailyStatusPointDto`'s
/// four-line `format!` is the whole of it. A `Date` serialized by whatever
/// `time` feature happens to be enabled is not the same contract.
///
/// # No `Default` on any of the four chart types
///
/// [`AnalyticsLists`] has one because an empty partition is a real value;
/// nothing needs an empty chart. A defaulted [`HeatmapRow`] would be a row with
/// no cells against an axis that has some — an invalid state a later caller could
/// reach by accident — and `time::Date` has no `Default` at all, so [`TrendPoint`]
/// could only get one by synthesizing a day. The empty chart is two
/// `Vec::new()`s at whichever call site turns out to want it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeatmapData {
    /// The x-axis: [`heatmap_days`] dates, oldest first, ending on the caller's
    /// today, rendered from it at `:1466-1469`. See [`recent_days`].
    pub days: Vec<Date>,
    /// One row per universe entry, in the universe's order — **not** sorted, and
    /// **not** de-duplicated. See [`build_heatmap`].
    pub rows: Vec<HeatmapRow>,
}

/// One day of the trend: the whole universe partitioned three ways.
///
/// Legacy's `TrendPoint` (`analytics.rs:156-161`). No `Default` — see
/// [`HeatmapData`]'s header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrendPoint {
    /// The day this point counts, in UTC. A `Date` for
    /// [`HeatmapData::days`]' reason.
    pub day: Date,
    /// Universe entries whose bucket that day was `PASSED`.
    pub passed: usize,
    /// Universe entries whose bucket that day was `FAILED` — `ERROR` folded in
    /// by [`bucketize_status`](super::universe::bucketize_status).
    pub failed: usize,
    /// Everything else, **including every universe entry with no row that day**.
    /// See [`build_trend`].
    pub not_run: usize,
}

/// The trend: one point per day, oldest first.
///
/// Legacy's `TrendData` (`analytics.rs:164-166`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrendData {
    /// One point per day, **oldest first**, exactly [`trend_days`] of them — the
    /// same axis [`HeatmapData::days`] carries explicitly, inlined into the points
    /// because legacy's `TrendPoint` carries its own day (`:1526`).
    pub points: Vec<TrendPoint>,
}

/// The heatmap's day count, clamped to `[1, 30]`.
///
/// `clamp_days(days, 1, 30)` (`build_heatmap:1452`, `clamp_days` itself at
/// `:2084-2086`, which is `days.clamp(min, max)`).
///
/// # This is **not** [`trend_days`], and the two are the likeliest thing to
/// # confuse
///
/// The bounds differ at both ends: `90` is a legal trend window and clamps to
/// `30` here; `1` is a legal heatmap window and clamps to `7` there. Sharing one
/// clamp changes both charts and nothing fails.
/// `the_heatmap_and_trend_windows_clamp_differently` straddles both boundaries.
///
/// # The default is `7`, and it lives in [`super::query`] (Task 25a)
///
/// Legacy clamps **twice**: `normalize_overview_query` maps the absent query
/// parameter to `7` and clamps it (`:2426`, `clamp_days(query.days_heatmap
/// .unwrap_or(7) as usize, 1, 30)`), and [`build_heatmap`] clamps again on the
/// way in (`:1452`). Both are ported: this function is the second, and
/// [`super::query::normalize_overview_query`] is the first — it calls **this**
/// function for it rather than re-spelling the bounds, which is what keeps the
/// two clamps one definition. The default lives with that module because the
/// `Option` it defaults is a *query* parameter, and nothing in a pure fold can
/// see whether the caller asked.
#[must_use]
pub fn heatmap_days(days: usize) -> usize {
    days.clamp(1, 30)
}

/// The trend's day count, clamped to `[7, 365]`.
///
/// `clamp_days(days, 7, 365)` (`build_trend:1496`). See [`heatmap_days`] for why
/// the two are separate functions and not one.
///
/// # Two callers, and the second is Task 23's
///
/// `build_flaky` clamps its window with **these** bounds, not the heatmap's:
/// `clamp_days(trend_days, 7, 365)` (`:1656`), from the same `days_trend` query
/// parameter (`:783`). So the flaky detector reuses this function rather than
/// re-deriving a third window, and its cutoff is this window's first day —
/// `Utc::now().date_naive() - (trend_days - 1)` (`:1657`), which is
/// `recent_days(today, trend_days(days))[0]`.
///
/// The default is `90` (`:2427`, `clamp_days(query.days_trend.unwrap_or(90) as
/// usize, 7, 365)`), and it lives in [`super::query`] for [`heatmap_days`]'
/// reason — Task 25a, which calls this function for legacy's first clamp.
#[must_use]
pub fn trend_days(days: usize) -> usize {
    days.clamp(7, 365)
}

/// `days` consecutive dates ending on `today`, **oldest first**.
///
/// `recent_days` (`analytics.rs:2077-2082`), with legacy's `Utc::now()
/// .date_naive()` lifted into the `today` parameter — see
/// [`Clock`](crate::domain::ports::Clock) for why that parameter exists and why
/// it is a `Date` rather than the port itself.
///
/// The ordering is the chart's x-axis and legacy's expression makes it explicit:
/// `(0..days).map(|offset| today - Duration::days((days - 1 - offset) as i64))`,
/// so offset `0` is the *oldest* day and the last element is `today`. Reversing
/// it renders both charts backwards, and using `days` where legacy uses
/// `days - 1` shifts the whole axis one day into the past and drops today.
///
/// `days == 0` is an empty window, exactly as legacy's empty range is. Every
/// caller in this module clamps first, so it is unreachable from here; it is
/// defined rather than debated because this function is `pub`.
#[must_use]
pub fn recent_days(today: Date, days: usize) -> Vec<Date> {
    let mut window = Vec::with_capacity(days);
    let mut day = today;
    for _ in 0..days {
        window.push(day);
        day -= Duration::DAY;
    }
    window.reverse();
    window
}

/// The bucket of the first row seen for each `(file, day)` inside the window.
///
/// Legacy writes this fold **twice**, character for character — `:1456-1464` in
/// `build_heatmap` and `:1500-1508` in `build_trend`, nine lines each and line
/// `1456 + n` identical to line `1500 + n` throughout. Shared here rather than
/// duplicated, and `the_first_row_for_a_file_and_day_wins` asserts it from both
/// charts so the sharing stays faithful.
///
/// # First seen wins, which makes the caller's row order load-bearing
///
/// `or_insert_with` (`:1462`), not "worst status" and not "latest instant". In
/// production "first" means "newest" because
/// [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe)
/// orders on a sort key descending, then `created_at DESC, ingest_ordinal DESC,
/// id DESC`. **The sort key itself is conditional**:
/// `infra::storage::results_sea_repo`'s `sort_key` (`:232-238`) emits the bare
/// `run_finished_at` on the `finished_only` path — where the column is provably
/// non-null, so the index can serve it — and
/// `COALESCE(run_finished_at, run_created_at)` otherwise. Newest-first holds on
/// both paths, which is all this fold needs; the same reason
/// [`build_latest_map`](super::universe::build_latest_map) states, and the same
/// consequence: hand this rows in another order and it answers a different
/// question without failing.
///
/// # The window filter is an optimization, not a rule
///
/// Legacy's `if !date_set.contains(&row.day) { continue }` (`:1458-1460`) cannot
/// change an answer: the map is only ever read at keys whose day is *in* the
/// window, so an out-of-window entry is unreachable. It is ported because it
/// keeps the map proportional to the window rather than to the whole fetched
/// `Vec` — and it is called out here because no test can distinguish its
/// presence from its absence, so nothing in this crate pins it.
///
/// # A two-level map where legacy has a `(String, NaiveDate)` tuple key
///
/// **The outer key is `(repo_id, test_file)`, not `test_file` alone — a
/// fix-round correction.** A product owns several repositories and
/// `(tenant_id, product_id)` is not a unique index, so two repositories can
/// each hold `tests/test_smoke.py`; a fold keyed on the path alone would fold
/// both repositories' rows onto one calendar and hand one repository's
/// heatmap and trend cells to the other's test. This is the same collision
/// [`build_latest_map`](super::universe::build_latest_map) closes, reached
/// here instead through [`ExecRow::repo_id`](super::ExecRow::repo_id).
///
/// This costs back the lookup-allocation saving the paragraph below used to
/// describe: `(Uuid, String)` has no `Borrow<(Uuid, &str)>`, so
/// [`bucket_at`] now allocates one `String` per lookup — `universe.len() *
/// days` of them, exactly the quantity the old two-level split avoided. That is
/// a bounded cost (the universe and the axis, not the row count this module's
/// other headers worry about), and correctness is not optional, so the
/// allocation is accepted rather than engineered around.
///
/// What the two-level split still saves, unchanged: the **insert** clones once
/// per in-window row either way — `HashMap::entry` takes its key by value, so
/// the `clone()` below runs before the map can say whether the key is already
/// present, exactly as legacy's tuple-keyed insert does (`:1461`) — so splitting
/// the map was never an insert-side saving, only ever a lookup-side one, and the
/// paragraph above is what that saving now costs to keep correct.
fn buckets_by_file_and_day(rows: &[ExecRow], window: &HashSet<Date>) -> DayBuckets {
    let mut buckets: DayBuckets = HashMap::new();

    for row in rows {
        if !window.contains(&row.day) {
            continue;
        }
        buckets
            .entry((row.repo_id, row.test_file.clone()))
            .or_default()
            .entry(row.day)
            .or_insert_with(|| bucketize_status(row.status.as_str()));
    }

    buckets
}

/// [`buckets_by_file_and_day`]'s index: `(repo_id, test_file)`, then day, then
/// bucket.
type DayBuckets = HashMap<(Uuid, String), HashMap<Date, &'static str>>;

/// One file's bucket on one day, or [`NOT_RUN`].
///
/// Legacy's `map.get(&key).cloned().unwrap_or_else(|| "NOT_RUN".to_string())`
/// (`:1476-1479`) and `map.get(&key).map(|v| v.as_str()).unwrap_or("NOT_RUN")`
/// (`:1518`) are the same expression twice; this is it once. **The substitution
/// is what makes a day with no run renderable at all** — the cell is the literal
/// `NOT_RUN`, not an empty string and not an absent element, so every row is
/// exactly as long as the axis.
fn bucket_at(buckets: &DayBuckets, repo_id: Uuid, test_file: &str, day: Date) -> &'static str {
    buckets
        .get(&(repo_id, test_file.to_owned()))
        .and_then(|by_day| by_day.get(&day))
        .copied()
        .unwrap_or(NOT_RUN)
}

/// The heatmap: every universe entry's bucket per day, over the last
/// [`heatmap_days`] days ending `today`.
///
/// `build_heatmap` (`analytics.rs:1451-1493`). Expects
/// [`resolve_rows`](super::universe::resolve_rows)' output, so that one file's
/// history is under one spelling — this fold keys on `test_file` and looks each
/// *universe* file up, so an unresolved row keyed on `""` renders as a blank row
/// for the test it belongs to.
///
/// Four properties an implementation loses without noticing:
///
/// * **One row per universe entry, in the universe's order** (`:1472`, a plain
///   `for test in universe`). Not sorted — the sibling fold [`build_lists`]
///   *does* sort, so symmetry is the trap. **Not de-duplicated either, and this
///   used to describe that as a live defect: "a file listed by two plans is
///   two identical rows, the same grain `summarize`'s `total` counts twice."**
///   That input cannot occur: qa-catalog's `walk_repo_universe` merges a
///   second plan's contribution into the first, within one repository, before
///   the universe ever reaches this fold, so a file two plans of the **same**
///   repository list is one universe entry here too. Two **different**
///   repositories listing the same path is a different, real case, and — a
///   second, fix-round correction — this fold is now part of the fix that
///   closed that gap: [`buckets_by_file_and_day`]'s map is keyed on
///   `(repo_id, test_file)`, per [`ExecRow::repo_id`](super::ExecRow::repo_id),
///   not on the path alone.
/// * **Every row is `days` long**, [`NOT_RUN`] where nothing ran. See
///   [`bucket_at`].
/// * **First row per `(file, day)` wins**, so the caller's order decides the
///   chart. See [`buckets_by_file_and_day`].
/// * **A row is placed on its own `day`**, which is
///   [`ExecRow::ts`](super::ExecRow::ts)'s calendar day — `run_finished_at ??
///   run_created_at` — and not on `today`.
///
/// A row whose file is outside the universe contributes nothing, and that needs
/// no filter: the fold reads the map only at universe files, so such an entry is
/// simply never looked up. Legacy relies on the same structure.
#[must_use]
pub fn build_heatmap(
    universe: &[UniverseTest],
    rows: &[ExecRow],
    days: usize,
    today: Date,
) -> HeatmapData {
    let axis = recent_days(today, heatmap_days(days));
    let window: HashSet<Date> = axis.iter().copied().collect();
    let buckets = buckets_by_file_and_day(rows, &window);

    let heat_rows = universe
        .iter()
        .map(|test| HeatmapRow {
            test_file: test.test_file.clone(),
            test_name: test.test_name.clone(),
            values: axis
                .iter()
                .map(|day| bucket_at(&buckets, test.repo_id, test.test_file.as_str(), *day))
                .collect(),
        })
        .collect();

    HeatmapData {
        days: axis,
        rows: heat_rows,
    }
}

/// The trend: the whole universe partitioned three ways per day, over the last
/// [`trend_days`] days ending `today`.
///
/// `build_trend` (`analytics.rs:1495-1534`). Same inputs and same first-wins map
/// as [`build_heatmap`]; what differs is the clamp ([`trend_days`], `[7, 365]`)
/// and that the per-day result is three counters rather than one cell per test.
///
/// # The denominator is the universe, so every point totals `universe.len()`
///
/// The inner loop is over `universe` and not over the map (`:1516-1523`), and a
/// file with no row that day falls to the `_` arm — so `passed + failed +
/// not_run` is the universe size on **every** point, including days before any
/// row exists. That is the trend's answer to "no run that day", where the
/// heatmap's is a [`NOT_RUN`] cell: the same fact, rendered as a counter rather
/// than as a colour.
///
/// Iterating the map instead is the shorter way to write it and is wrong twice:
/// it would count rows outside the universe, and it would report a total that
/// moved with the data rather than with the universe.
/// `every_trend_point_totals_the_universe_size` carries an out-of-universe row
/// for exactly that reason.
///
/// The three-way match is legacy's own literals (`:1519-1521`), matching
/// [`bucketize_status`](super::universe::bucketize_status)' output rather than
/// re-deriving it — ruling R5's distinction, and the reason a `SKIPPED` row is a
/// `not_run` test here and not a fourth counter.
#[must_use]
pub fn build_trend(
    universe: &[UniverseTest],
    rows: &[ExecRow],
    days: usize,
    today: Date,
) -> TrendData {
    let axis = recent_days(today, trend_days(days));
    let window: HashSet<Date> = axis.iter().copied().collect();
    let buckets = buckets_by_file_and_day(rows, &window);

    let points = axis
        .into_iter()
        .map(|day| {
            let mut passed = 0usize;
            let mut failed = 0usize;
            let mut not_run = 0usize;

            for test in universe {
                match bucket_at(&buckets, test.repo_id, test.test_file.as_str(), day) {
                    "PASSED" => passed += 1,
                    "FAILED" => failed += 1,
                    _ => not_run += 1,
                }
            }

            TrendPoint {
                day,
                passed,
                failed,
                not_run,
            }
        })
        .collect();

    TrendData { points }
}

// ==================== The flaky detector ====================

/// One entry of the overview's flaky list.
///
/// Legacy's `FlakyTest` (`analytics.rs:189-199`), field for field, including the
/// `u32` widths it inherits from [`StatusStats`] and the `f64` pass rate
/// [`pct`] produces.
///
/// The first four fields are copied off the matching [`UniverseTest`] rather than
/// derived from the rows (`:1692-1695`): the rows know a file, and the *name*,
/// the component and the tags are the catalog's.
#[derive(Clone, Debug, PartialEq)]
pub struct FlakyTest {
    pub test_file: String,
    pub test_name: String,
    pub component: Option<String>,
    pub tags: Vec<String>,
    /// [`Self::pass_count`] over [`Self::executions`], as a **percentage**
    /// rounded to one decimal by [`pct`] — not a ratio in `[0, 1]` the way
    /// `DashboardStats::pass_rate_24h` is.
    pub pass_rate: f64,
    /// The three counters below, summed (`:1681`). Every row in the window
    /// counts, including the ones the catch-all absorbs, so a mostly-skipped test
    /// can reach [`FLAKY_MIN_EXECUTIONS`].
    pub executions: u32,
    pub pass_count: u32,
    pub fail_count: u32,
    /// The catch-all, not a `SKIPPED` counter — see [`StatusStats`].
    pub skipped_count: u32,
}

/// Executions a test needs inside the window before [`build_flaky`] will look at
/// its pass rate at all.
///
/// `if executions < 5 { continue }` (`analytics.rs:1682-1684`). The gate exists
/// because a pass rate over two or three runs is noise: `1/2` is 50.0 and lands
/// squarely inside the band, and a list of two-run coin flips is what makes a
/// flaky report unreadable.
pub const FLAKY_MIN_EXECUTIONS: u32 = 5;

/// The **inclusive** pass-rate band, in percent, a test must land inside to be
/// called flaky.
///
/// `if !(40.0..=80.0).contains(&pass_rate) { continue }`
/// (`analytics.rs:1685-1688`). Both ends are open questions a reader will get
/// wrong in the same direction, so both are stated:
///
/// * **A test that always fails is not flaky, it is broken**, and 0.0 is outside
///   the band by design. Widening the floor to `0.0` would fill the flaky list
///   with the failures the *failed* list already names.
/// * **A test that almost always passes is not flaky either.** 90.0 is outside
///   the band, so the one-bad-run-in-ten test does not appear.
///
/// Inclusive at both ends: 40.0 and 80.0 are flaky, 39.9 and 80.1 are not.
/// `the_flaky_band_is_inclusive_at_both_ends` straddles all four values.
pub const FLAKY_PASS_RATE_BAND: RangeInclusive<f64> = 40.0..=80.0;

/// The oldest day [`build_flaky`] counts, for a `days` request ending `today`.
///
/// `Utc::now().date_naive() - Duration::days(trend_days - 1)`
/// (`analytics.rs:1657`), which is [`recent_days`]' first element for the
/// **trend's** clamp — see [`trend_days`] for why the flaky window is that clamp
/// and not a third one.
///
/// Expressed through [`recent_days`] rather than by subtracting, so the flaky
/// window and the trend chart cannot disagree about which day is the oldest:
/// they are the same expression, and `the_flaky_cutoff_is_the_trend_windows_first_day`
/// asserts the identity rather than re-deriving it.
///
/// # Why this is a bound and not a set
///
/// [`build_flaky`]'s filter is **one-sided**: `if row.day < cutoff { continue }`
/// (`:1661-1663`), where [`build_heatmap`] and [`build_trend`] test membership of
/// a closed [`HashSet`](std::collections::HashSet) window. The two differ for
/// exactly one kind of row — one dated *after* `today` — and legacy counts it.
/// Clock skew between whatever wrote the run's timestamp and whatever reads this
/// gear's clock is enough to produce one, so the difference is reachable rather
/// than theoretical, and `a_future_dated_row_is_flaky_evidence_but_not_chart_data`
/// pins both halves against each other.
#[must_use]
pub fn flaky_cutoff(today: Date, days: usize) -> Date {
    let window = recent_days(today, trend_days(days));
    // `trend_days` clamps to at least 7, so the window is never empty and the
    // first element always exists. Taken with `next()` rather than `[0]` only to
    // keep `clippy::indexing_slicing` off a `pub fn`; the `unwrap_or` **masks** an
    // empty window rather than proving one impossible, and `days == 0` reaching
    // here would silently answer `today`. The clamp is what makes that
    // unreachable, and `the_flaky_cutoff_is_the_trend_windows_first_day` passes
    // `0` for exactly that reason.
    window.into_iter().next().unwrap_or(today)
}

/// The tests whose recent history is neither reliably green nor reliably red.
///
/// `build_flaky` (`analytics.rs:1655-1712`), with legacy's `Utc::now()` lifted
/// into `today` — the signature convention [`build_heatmap`] and [`build_trend`]
/// already follow, and the reason [`Clock`](crate::domain::ports::Clock) is read
/// once at the service edge.
///
/// Four gates in order, and each one changes which tests are listed:
///
/// 1. **The window.** Rows older than [`flaky_cutoff`] are dropped, one-sidedly
///    — see that function.
/// 2. **The tally.** Three-way, via [`tally`]: `PASSED`, `FAILED`+`ERROR`, and
///    everything else. All three feed [`FlakyTest::executions`].
/// 3. **[`FLAKY_MIN_EXECUTIONS`]**, then **[`FLAKY_PASS_RATE_BAND`]**, in
///    legacy's order (`:1682`, `:1685`) — and the order is **not** observable:
///    both are unconditional rejections with no side effect, so swapping them
///    changes no output. Kept in legacy's sequence because it is legacy's, not
///    because a test could tell. (This read "the order is observable, because a
///    two-run test at 50.0 is inside the band and still excluded", which argues
///    that *both* gates exist and not that their order matters — the same test is
///    excluded either way round.)
/// 4. **Universe membership** (`:1690`, `if let Some(test) = ..`). A file with
///    rows and no universe entry is dropped, because rows outlive deleted test
///    files and a flaky list naming files that no longer exist is noise.
///
/// # It is keyed on `(repo_id, test_file)`, and the *dashboard's* flaky fold
/// # is not
///
/// **Fix-round correction**: this fold used to key on `test_file` alone, so
/// two repositories sharing a path had their rows tallied together and their
/// flaky verdicts (and evidence counts) attributed to whichever repository's
/// universe entry the lookup happened to find. A product owns several
/// repositories and `(tenant_id, product_id)` is not a unique index, so that
/// collision is real, not hypothetical — the same one
/// [`ExecRow::repo_id`](super::ExecRow::repo_id) exists to close.
///
/// Legacy has **two** flaky folds and they agree on nothing but the word. This
/// one keys on `(repo_id, test_file)` and uses the three-way split; the dashboard's
/// (`manager/src/routes/dashboard.rs:379-405`) groups by `tr.test_name,
/// rr.plan_id` and counts under ruling R5's *sixth* classification, whose
/// denominator excludes everything that is not `PASSED`, `FAILED` or `ERROR`.
/// [`ExecRow`]'s header records the grain half and
/// [`crate::domain::service::ingest`]'s R5 table the classification half. They
/// are deliberately **not** unified here; `qa_insights_sdk::FlakyTestCard` is the
/// other one's output shape and this type is this one's.
///
/// **The other one is shipped**, by Task 23b:
/// [`crate::domain::repos::ResultsRepository::flaky_groups`] is the read and
/// `crate::domain::service::dashboard`'s `flaky_card` the projection. Unifying
/// the two would change numbers the UI already renders and **nothing here would
/// fail**, which is why the difference is written down in four places rather than
/// left to be rediscovered.
///
/// # No limit, and no `LIMIT` to port
///
/// The dashboard's is `LIMIT 10` (`dashboard.rs:399`). This one returns every
/// qualifying test — legacy's `flaky` vector is returned whole (`:1711`) — so a
/// caller that wants ten takes ten. `the_flaky_list_is_not_truncated` pins the
/// absence, because adding a cap is the change nothing else here would catch.
///
/// # Ordering
///
/// Ascending pass rate, then **descending** executions (`:1705-1710`): the
/// flakiest first, and among equally flaky tests the best-evidenced first.
/// `partial_cmp` with `Ordering::Equal` for the incomparable case is legacy's own
/// spelling and cannot fire — [`pct`] never returns `NaN`, because its
/// non-positive-total guard is the only division it performs.
#[must_use]
pub fn build_flaky(
    universe: &[UniverseTest],
    rows: &[ExecRow],
    days: usize,
    today: Date,
) -> Vec<FlakyTest> {
    let cutoff = flaky_cutoff(today, days);

    let mut by_test: HashMap<(Uuid, &str), StatusStats> = HashMap::new();
    for row in rows {
        // `< cutoff`, not `window.contains(..)` — see `flaky_cutoff`.
        if row.day < cutoff {
            continue;
        }
        tally(
            by_test
                .entry((row.repo_id, row.test_file.as_str()))
                .or_default(),
            row.status.as_str(),
        );
    }

    let universe_by_file: HashMap<(Uuid, &str), &UniverseTest> = universe
        .iter()
        .map(|item| ((item.repo_id, item.test_file.as_str()), item))
        .collect();

    let mut flaky: Vec<FlakyTest> = by_test
        .into_iter()
        .filter_map(|((repo_id, test_file), stats)| {
            flaky_entry(&universe_by_file, repo_id, test_file, stats)
        })
        .collect();

    flaky.sort_by(|left, right| {
        left.pass_rate
            .partial_cmp(&right.pass_rate)
            .unwrap_or(Ordering::Equal)
            .then_with(|| right.executions.cmp(&left.executions))
    });
    flaky
}

/// One candidate through gates 3 and 4 of [`build_flaky`], or `None`.
///
/// Split out so the fold body stays under `clippy::cognitive_complexity` and so
/// the two rejections are one expression each rather than two `continue`s inside
/// three levels of nesting. Legacy's gate order is preserved: executions, then
/// the band, then the universe lookup.
fn flaky_entry(
    universe_by_file: &HashMap<(Uuid, &str), &UniverseTest>,
    repo_id: Uuid,
    test_file: &str,
    stats: StatusStats,
) -> Option<FlakyTest> {
    let executions = stats.pass_count + stats.fail_count + stats.skipped_count;
    if executions < FLAKY_MIN_EXECUTIONS {
        return None;
    }

    let pass_rate = pct(f64::from(stats.pass_count), f64::from(executions));
    if !FLAKY_PASS_RATE_BAND.contains(&pass_rate) {
        return None;
    }

    let test = universe_by_file.get(&(repo_id, test_file))?;
    Some(FlakyTest {
        test_file: test_file.to_owned(),
        test_name: test.test_name.clone(),
        component: test.component.clone(),
        tags: test.tags.clone(),
        pass_rate,
        executions,
        pass_count: stats.pass_count,
        fail_count: stats.fail_count,
        skipped_count: stats.skipped_count,
    })
}

// ==================== Quality vectors ====================

/// How many tests carry one Quality Vector.
///
/// Legacy's `QualityVectorCount` (`analytics.rs:218-221`), two fields, two
/// fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityVectorCount {
    /// The vector as some file spelled it — see
    /// [`build_quality_vector_summary`] for which spelling wins.
    pub vector: String,
    /// Distinct **files** carrying it, not executions.
    pub tests: usize,
}

/// The overview's Quality Vector breakdown.
///
/// Legacy's `QualityVectorSummary` (`analytics.rs:224-228`), three fields, three
/// fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QualityVectorSummary {
    /// Descending by [`QualityVectorCount::tests`], then ascending by vector.
    pub items: Vec<QualityVectorCount>,
    /// Files whose `TEST_META` declared no vector at all (`:1053-1056`).
    pub unclassified_tests: usize,
    /// **Distinct files**, and therefore not `universe.len()` — see this fold's
    /// header.
    pub total_tests: usize,
}

/// The per-vector file counts, plus how many files declare no vector.
///
/// `build_quality_vector_summary` (`analytics.rs:1047-1083`).
///
/// # Its input is a `file -> vectors` map, and the grain is the difference
///
/// Legacy builds that map while walking the plans (`:839`, `:871-882`) keyed on
/// the **normalized test file**, where the universe it builds in the same loop is
/// keyed on `(source, repo_id, test_file)` (`:899-903`). So a file listed by two
/// plans is *two* universe entries and *one* vector entry, and
/// [`QualityVectorSummary::total_tests`] — `vectors_by_file.len()` (`:1081`) — is
/// smaller than [`OverviewSummary::total`] on any such universe. Folding this
/// over `universe.len()` instead would double-count that file in
/// `total_tests`, in `unclassified_tests` and in every item's `tests`, which is
/// why this fold re-keys rather than iterating the slice.
/// `a_file_in_two_plans_is_one_quality_vector_test` pins it.
///
/// # Keyed on `(repo_id, test_file)`, not the bare path — a fix-round-2 correction
///
/// Two repositories can each hold a `tests/test_smoke.py`; keying `by_file` on
/// the path alone would merge their vector sets, so repo B's untagged file
/// would inherit repo A's `quality_vectors` and stop counting toward
/// `unclassified_tests` — carrying that metadata correctly is this fold's whole
/// job. [`StatsMap`] is keyed the same way for the same reason; see its own
/// doc. `plan_path` stays out of the key: it plays no part in the collision
/// this section is about, and the section above already establishes that a
/// file listed twice **within one repo** de-duplicates to one vector test —
/// adding `plan_path` to the key would undo that.
///
/// # Where the vectors come from, and what happened to legacy's cache
///
/// Legacy parses `TEST_META` off disk. There are **two** such readers and only
/// one of them is cached:
///
/// * this fold's input, built inline by `load_universe_and_rows` (`:871-882`) on
///   **every** overview request, with no cache at all;
/// * `collect_quality_vectors_by_file` (`:1991-2007`), the *dashboard's*, behind
///   a process-global 1800-second TTL. That one is ported by
///   `crate::domain::service::dashboard`'s `quality_vector_pass_rates` (Task
///   25a) and is a different fold over different columns: it divides
///   **executions** per vector where this one counts **files**, and it has no
///   unclassified residue. In this architecture both read the same catalog
///   projection, so the TTL has no counterpart on either side.
///
/// In this architecture the vectors are a field of the catalog's own projection,
/// [`UniverseTest::quality_vectors`], which arrives with the universe over
/// [`CatalogReader::list_universe`](crate::domain::ports::CatalogReader::list_universe).
/// So there is nothing to cache here and nothing to invalidate: this fold reads a
/// value it was handed, exactly as legacy's uncached reader did.
///
/// # Case folding: counted case-insensitively, displayed as first seen
///
/// `vector_counts.entry(vector.to_ascii_lowercase()).or_insert_with(|| (vector
/// .clone(), 0))` (`:1060-1062`) — so `Security` and `security` are one count
/// under whichever spelling arrived first, and the same fold runs per file first
/// (`:879-881`) so one file declaring both counts once. **Legacy's "first" is
/// undefined**: both maps it iterates are `HashMap`s, so which spelling is
/// rendered varies between two runs over identical data. This fold walks files in
/// path order and vectors in case-folded order, which makes the winner
/// deterministic without changing any count.
///
/// **The dashboard's fold does the opposite and that is legacy's own
/// inconsistency**, measured by Task 25a: `dashboard.rs:520` keys its aggregate
/// on the *display* string, so two different files spelling one vector
/// differently are two rows there and one count here. Both are ported verbatim.
/// A reader comparing the overview's vector list against the dashboard's and
/// finding one row where the other has two has found this, not a bug.
#[must_use]
pub fn build_quality_vector_summary(universe: &[UniverseTest]) -> QualityVectorSummary {
    let mut by_file: BTreeMap<(Uuid, &str), BTreeMap<String, &str>> = BTreeMap::new();
    for test in universe {
        let bucket = by_file
            .entry((test.repo_id, test.test_file.as_str()))
            .or_default();
        for vector in &test.quality_vectors {
            let trimmed = vector.trim();
            if trimmed.is_empty() {
                continue;
            }
            bucket
                .entry(trimmed.to_ascii_lowercase())
                .or_insert(trimmed);
        }
    }

    let mut vector_counts: BTreeMap<&str, (&str, usize)> = BTreeMap::new();
    let mut unclassified_tests = 0usize;
    for vectors in by_file.values() {
        if vectors.is_empty() {
            unclassified_tests += 1;
            continue;
        }
        for (folded, display) in vectors {
            let entry = vector_counts.entry(folded.as_str()).or_insert((display, 0));
            entry.1 += 1;
        }
    }

    let mut items: Vec<QualityVectorCount> = vector_counts
        .into_values()
        .map(|(vector, tests)| QualityVectorCount {
            vector: vector.to_owned(),
            tests,
        })
        .collect();
    items.sort_by(|left, right| {
        right
            .tests
            .cmp(&left.tests)
            .then_with(|| left.vector.cmp(&right.vector))
    });

    QualityVectorSummary {
        items,
        unclassified_tests,
        total_tests: by_file.len(),
    }
}

// ==================== Grouped summaries ====================

/// The four counters every group carries.
///
/// Legacy's is a bare `(usize, usize, usize, usize)` (`:1088-1089`) whose layout
/// is `(total, passed, failed, not_run)` — established by `accumulate_group`
/// (`:2225-2232`), which increments `.0` unconditionally and then one of `.1`,
/// `.2`, `.3`, and by `group_map_to_vec` (`:2212-2223`), which destructures in
/// that same order. A named struct here rather than the tuple, because the two
/// middle fields are the same type and a transposition between the fold and the
/// projection would swap `passed` with `failed` on the rendered chart with
/// nothing failing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GroupCounters {
    total: usize,
    passed: usize,
    failed: usize,
    not_run: usize,
}

impl GroupCounters {
    /// One universe file's latest bucket, added to a group.
    ///
    /// `accumulate_group` (`analytics.rs:2225-2232`). `total` counts **every**
    /// file offered, so `passed + failed + not_run == total` always holds, and the
    /// third counter is the catch-all: `bucketize_status` emits three words, and
    /// anything that is not `PASSED` or `FAILED` — including the [`NOT_RUN`]
    /// substituted for a file with no row at all — lands in `not_run`.
    fn add(&mut self, bucket: &str) {
        self.total += 1;
        match bucket {
            "PASSED" => self.passed += 1,
            "FAILED" => self.failed += 1,
            _ => self.not_run += 1,
        }
    }
}

/// One component or tag group, as the overview renders it.
///
/// Legacy's `GroupSummary` (`analytics.rs:202-208`), five fields, five fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupSummary {
    /// The group key: a component name, or a tag. Alphabetical across the
    /// returned `Vec` — see [`build_grouped_summaries`].
    pub value: String,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub not_run: usize,
}

/// One platform group — [`GroupSummary`] with an id where legacy has a name.
///
/// # A distinct type, because the value is not a label
///
/// Legacy's platform grouping puts `row.platform` — a display **name** — straight
/// into a `BTreeSet<String>` (`analytics.rs:1116-1119`) and returns it as
/// `GroupSummary::value` (`:1138`, then `:1144`), i.e. as the string the chart's
/// axis draws. This
/// gear stores [`ExecRow::environment_id`](super::ExecRow::environment_id), a `Uuid`,
/// for the reason that field's header gives, and **nothing between here and the
/// response resolves it**: the lookup is a qa-environments read, once per
/// distinct id and outside the per-row path.
///
/// **That port now exists** —
/// [`EnvironmentReader::names`](crate::domain::ports::EnvironmentReader::names), Task
/// 25a, with exactly that shape (a slice of ids in, a map out, one call). This
/// fold still returns the id, and deliberately: it is pure, it takes the universe
/// and the rows and nothing else, and a fold that awaited a cross-gear read would
/// stop being testable without one. The resolution belongs to the service that
/// assembles the payload, and the label to the DTO.
///
/// So the id is carried in a field named for what it is, rather than
/// `Display`-formatted into [`GroupSummary::value`]. That is deliberate and it is
/// the same move [`ExecRow::build`](super::ExecRow::build)'s `Option<String>`
/// makes: a gap that fails to compile gets closed, and a gap that renders a
/// plausible string does not. A response DTO cannot be written over this type
/// without deciding what the label is.
///
/// **Consequences a later task must settle**, both recorded rather than papered
/// over:
///
/// * the group's *label* is unresolved **in this type**, and resolving it is the
///   assembling service's job through
///   [`EnvironmentReader`](crate::domain::ports::EnvironmentReader); what an id that
///   resolves to no platform renders as is still the DTO's decision, exactly as
///   [`ExecRow::run_id`](super::ExecRow::run_id)'s label is;
/// * the ordering is by **id**, where legacy's `BTreeSet<String>` orders by name.
///   Resolving the names will reorder this list, so a client must not treat the
///   position as stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformGroupSummary {
    /// The platform, as an id. See this type's header for why it is not a name.
    pub environment_id: Uuid,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub not_run: usize,
}

/// The three group breakdowns the overview renders side by side.
///
/// Legacy's `GroupedSummaries` (`analytics.rs:211-215`), three fields, three
/// fields — the third with [`PlatformGroupSummary`]'s substitution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupedSummaries {
    pub component: Vec<GroupSummary>,
    pub tag: Vec<GroupSummary>,
    pub platform: Vec<PlatformGroupSummary>,
}

/// The label a universe entry with no usable component is grouped under.
///
/// `.unwrap_or("unknown")` (`analytics.rs:1102`; the whole expression is
/// `:1097-1103`), applied after a trim, so a
/// component of `None`, of `""` and of `"   "` all land here.
///
/// **The `tests/<component>/…` path inference is not applied here and is not this
/// gear's**: legacy runs `infer_component_from_path` (`:1794-1803`) while building
/// the universe (`:910-912`, `:922-925`), which in this architecture is
/// qa-catalog's half — `qa-catalog/src/domain/service/plans.rs:492`
/// `component_for` is that
/// port, pinned by its own
/// `a_file_without_meta_falls_back_to_stem_name_and_inferred_component`. By the
/// time a [`UniverseTest`] reaches this gear its `component` is already inferred,
/// so this label covers only the files for which the inference itself declined.
pub const UNKNOWN_COMPONENT: &str = "unknown";

/// The tag a universe entry with no tags is grouped under.
///
/// `by_tag.entry("untagged".to_string())` (`analytics.rs:1107`). A file with tags
/// is counted once **per tag** and never here, so the tag breakdown's counters sum
/// to more than the universe size whenever any file carries two tags —
/// `a_multiply_tagged_file_is_counted_once_per_tag` pins that, because a reader
/// who expects a partition will read the chart as broken.
pub const UNTAGGED: &str = "untagged";

/// The overview's three group breakdowns, each over the whole universe.
///
/// `build_grouped_summaries` (`analytics.rs:1085-1146`). Every counter is a fold
/// over [`build_latest_map`](super::universe::build_latest_map)'s buckets, so a
/// file with no row is `not_run` rather than absent, and each group's `total` is
/// how many universe entries it contains.
///
/// # Three groupings, three different denominators
///
/// * **Component** — one group per distinct component, [`UNKNOWN_COMPONENT`] for
///   the files without one. A partition: the counters sum to `universe.len()`.
/// * **Tag** — one group per tag, [`UNTAGGED`] for the files with none. **Not** a
///   partition; see that constant.
/// * **Platform** — one group per distinct [`ExecRow::environment_id`](super::ExecRow::environment_id)
///   *seen in the rows*, and each group is counted over the **whole universe**
///   (`:1129-1138`), not over the tests that ran on that platform. So a test that
///   never ran on a platform is `not_run` in that platform's group, and every
///   platform group's `total` is `universe.len()`. That is what makes the chart
///   answer "how much of the suite is green here", and it is why the platform
///   loop rebuilds a latest map per platform rather than partitioning one.
///
/// A row whose `environment_id` is `None` contributes to no platform group and is
/// not grouped under a placeholder — legacy's `filter_map(normalize_optional(..))`
/// (`:1116-1119`) drops it, and there is no `"unknown"` platform the way there is
/// an unknown component.
///
/// # What it costs
///
/// The platform loop is `O(platforms × rows)`: legacy re-filters the whole row
/// slice per platform and rebuilds the latest map from it (`:1121-1139`), and this
/// is that, verbatim. Ported rather than improved under Phase B's standing
/// instruction, and stated because the row slice here is
/// [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe)'s
/// unaggregated `Vec` — the same `Vec` `UniverseFilter::since` exists to bound.
/// A single pass keyed on `(platform, file)` would give the same answer; it is a
/// change to make with a benchmark and not in passing.
#[must_use]
pub fn build_grouped_summaries(universe: &[UniverseTest], rows: &[ExecRow]) -> GroupedSummaries {
    let latest = build_latest_map(universe, rows);

    let mut by_component: BTreeMap<&str, GroupCounters> = BTreeMap::new();
    let mut by_tag: BTreeMap<&str, GroupCounters> = BTreeMap::new();

    for test in universe {
        let bucket = latest
            .get(&(test.repo_id, test.test_file.clone()))
            .map_or(NOT_RUN, |value| value.status_bucket);

        let component = test
            .component
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(UNKNOWN_COMPONENT);
        by_component.entry(component).or_default().add(bucket);

        if test.tags.is_empty() {
            by_tag.entry(UNTAGGED).or_default().add(bucket);
        } else {
            for tag in &test.tags {
                by_tag.entry(tag.as_str()).or_default().add(bucket);
            }
        }
    }

    GroupedSummaries {
        component: group_map_to_vec(by_component),
        tag: group_map_to_vec(by_tag),
        platform: platform_groups(universe, rows),
    }
}

/// The platform half of [`build_grouped_summaries`] (`analytics.rs:1115-1139`).
///
/// Split out to keep the parent under `clippy::cognitive_complexity`; the nesting
/// is legacy's own, three levels of it.
fn platform_groups(universe: &[UniverseTest], rows: &[ExecRow]) -> Vec<PlatformGroupSummary> {
    let platforms: BTreeSet<Uuid> = rows.iter().filter_map(|row| row.environment_id).collect();

    platforms
        .into_iter()
        .map(|environment_id| {
            let platform_rows: Vec<ExecRow> = rows
                .iter()
                .filter(|row| row.environment_id == Some(environment_id))
                .cloned()
                .collect();
            let latest = build_latest_map(universe, &platform_rows);

            let mut totals = GroupCounters::default();
            for test in universe {
                totals.add(
                    latest
                        .get(&(test.repo_id, test.test_file.clone()))
                        .map_or(NOT_RUN, |value| value.status_bucket),
                );
            }

            PlatformGroupSummary {
                environment_id,
                total: totals.total,
                passed: totals.passed,
                failed: totals.failed,
                not_run: totals.not_run,
            }
        })
        .collect()
}

/// A group map, projected into the response's `Vec`, alphabetically by key.
///
/// `group_map_to_vec` (`analytics.rs:2212-2223`). The order is the
/// [`BTreeMap`]'s, exactly as legacy's is, so the chart's bars are alphabetical
/// and not sorted by size — the sibling folds [`build_lists`] and [`build_flaky`]
/// both sort by a measure, so reaching for one here is the mistake.
fn group_map_to_vec(values: BTreeMap<&str, GroupCounters>) -> Vec<GroupSummary> {
    values
        .into_iter()
        .map(|(value, counters)| GroupSummary {
            value: value.to_owned(),
            total: counters.total,
            passed: counters.passed,
            failed: counters.failed,
            not_run: counters.not_run,
        })
        .collect()
}

/// Which grouping the overview was asked for.
///
/// Legacy's `GroupBy` (`analytics.rs:313-318`), four variants, four variants.
///
/// Parsed out of the query string by [`super::query::parse_group`] (Task 25a),
/// which returns **this** enum rather than declaring a second one — the group
/// vocabulary has one home and the parser and this fold cannot drift.
/// [`Default`] is [`Self::None`] because legacy defaults the absent parameter to
/// the literal `"none"` (`:2116`); note that a *present but blank* `group_by` is
/// a 400 there rather than the default, which that parser's doc records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupBy {
    /// No grouping selected — the whole universe, whatever `group_value` says.
    #[default]
    None,
    Component,
    Tag,
    Environment,
}

/// Narrow the universe to one group, for the drill-down the overview's group
/// chart links to.
///
/// `apply_universe_group_filter` (`analytics.rs:1148-1180`). Three properties,
/// and two of them look like defects:
///
/// * **A blank `group_value` narrows nothing.** `None`, `""` and `"   "` all
///   return the whole universe (`:1153-1155`), so "grouped by component with no
///   component chosen" is the unfiltered overview rather than an empty one.
/// * **The match is case-insensitive**, on both groupings —
///   `eq_ignore_ascii_case` (`:1164`, `:1174`) — so `?group_value=Cluster` finds
///   the `cluster` component. Note the asymmetry with
///   [`build_grouped_summaries`], which groups on the **verbatim** string: two
///   components differing only in case are two bars on the chart and one
///   selection through this filter.
/// * **[`GroupBy::Environment`] narrows nothing at all.** It falls to legacy's `_`
///   arm together with [`GroupBy::None`] (`:1178`), so selecting an environment bar
///   returns the whole universe rather than the tests that ran there — the rows
///   carry the environment and the universe does not, so there is nothing to filter
///   on. Ported as-is under Phase B's standing instruction and pinned by
///   `the_environment_grouping_does_not_narrow_the_universe`, because an
///   implementation that "fixed" it would change a rendered list.
///
/// A component of `None` never matches (`:1165`), so a component drill-down
/// cannot reach the files grouped under [`UNKNOWN_COMPONENT`] — the label is a
/// grouping key and not a value any test carries.
#[must_use]
pub fn apply_universe_group_filter(
    universe: &[UniverseTest],
    group_by: GroupBy,
    group_value: Option<&str>,
) -> Vec<UniverseTest> {
    let Some(group_value) = group_value.map(str::trim).filter(|value| !value.is_empty()) else {
        return universe.to_vec();
    };

    match group_by {
        GroupBy::Component => universe
            .iter()
            .filter(|item| {
                item.component
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|value| value.eq_ignore_ascii_case(group_value))
            })
            .cloned()
            .collect(),
        GroupBy::Tag => universe
            .iter()
            .filter(|item| {
                item.tags
                    .iter()
                    .any(|tag| tag.eq_ignore_ascii_case(group_value))
            })
            .cloned()
            .collect(),
        // `GroupBy::None` and `GroupBy::Environment` both narrow nothing — see this
        // function's header for why the second one does not.
        GroupBy::None | GroupBy::Environment => universe.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// The build distribution and the build-tests fold
// ---------------------------------------------------------------------------

/// The newest row per universe file, with the build it ran against.
///
/// Legacy's private `LatestBuildTestSnapshot` (`analytics.rs:1607-1613`), field
/// for field with ruling R12's one substitution, plus [`Self::repo_id`], a
/// fix-round addition legacy's shape has no analogue of — legacy has one
/// repository's worth of tests to key on and this port does not. It is `pub` here where legacy's
/// is private because both of its consumers' *outputs* —
/// [`BuildLastRunDistribution`] and [`BuildTestDetail`] — are shaped by it and
/// the DTOs a task later are written over those; a private type would force the
/// two folds into one function to keep it so.
///
/// # It is not [`LatestInfo`](super::universe::LatestInfo), and the difference is
/// # the status
///
/// Both are "the newest row per file, first-wins over a newest-first slice", and
/// legacy has both for one reason: `build_latest_map` (`:1182`) stores
/// `bucketize_status`' three-way bucket, and this fold stores a *fourth*
/// classification that keeps `SKIPPED` and passes an unrecognized status through
/// verbatim. A build chart drawn off the bucket would report every skipped test
/// as never-run, and the build-tests list would render `NOT_RUN` where the runner
/// said `XFAIL`. See [`latest_per_test_snapshot`] for the mapping itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatestBuildTestSnapshot {
    /// The build, already collapsed to [`UNKNOWN_BUILD`] when the row named
    /// none. Verbatim otherwise — including the literal `"unknown"`, which
    /// legacy cannot distinguish from the substitution either.
    pub build: String,
    /// The repository this snapshot's universe entry belongs to — a
    /// fix-round addition. [`build_test_details`] joins a snapshot back to its
    /// universe entry's metadata (`test_name`, `component`, `tags`), and doing
    /// that on `test_file` alone would hand one repository's component and
    /// tags to another repository's test of the same name; the pair is what
    /// [`latest_per_test_snapshot`] already folds on, so this is that key's
    /// other half riding along on the value.
    pub repo_id: Uuid,
    /// The universe file this is the newest row for.
    pub test_file: String,
    /// The row's status under this fold's own mapping (`:1633-1639`) — *not*
    /// [`bucketize_status`](super::universe::bucketize_status)'.
    pub status: String,
    /// The run the newest row came from.
    ///
    /// **Legacy carries `workflow_name`, a display string, and this is a
    /// `Uuid`** — [`ExecRow`](super::ExecRow) has no run name to carry. Ruling
    /// R12: the same substitution
    /// [`LatestInfo::run_id`](super::universe::LatestInfo::run_id) already makes,
    /// and the same open obligation. It reaches the UI as
    /// `BuildLastRunDistribution::latest_run_name` (`:171`) and
    /// `BuildTestDetailItem::run_name` (`:181`) in legacy, so **Task 25**, which
    /// owns the DTOs, was where the name had to come from or the field had to be
    /// renamed. **Task 25b renamed**, for the reason
    /// [`ExecRow::run_id`](super::ExecRow::run_id) now records: there is no bulk
    /// run-name read in this gear. Nothing here `Display`s the id into a name.
    pub run_id: Uuid,
    /// The row's effective instant, `run_finished_at ?? run_created_at`, already
    /// coalesced into [`ExecRow::ts`](super::ExecRow::ts).
    pub ts: OffsetDateTime,
}

/// The newest row per universe file, in `test_file` order.
///
/// `latest_per_test_snapshot` (`analytics.rs:1615-1653`). Three of legacy's
/// properties, all of them easy to lose, plus one deliberate departure:
///
/// * **The first row seen per file wins** and nothing compares timestamps
///   (`:1628-1632`). The rows must arrive newest-first — the same premise
///   [`build_latest_map`](super::universe::build_latest_map) states, and this
///   port's guarantee is
///   [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe)'s
///   contractual ordering. Hand this function rows in another order and it
///   answers a different question without failing.
/// * **A row whose file is not in the universe is skipped** (`:1626-1628`), so
///   the caller's narrowing is respected. It must be the *caller's*: this fold
///   and its two consumers take `filtered_universe` + `rows_for_scope`
///   (`:751-783`, and `api_build_tests` re-derives the same pair at `:403-417`),
///   and neither filters internally — see this module's header table for what
///   goes wrong when a fold narrows on its own.
/// * **The build is collapsed here** by
///   [`collapse_build`](super::universe::collapse_build) —
///   [`UNKNOWN_BUILD`](super::universe::UNKNOWN_BUILD) for an absent or blank
///   label, trimmed otherwise. This is **one of two** collapse sites, not the
///   only reader of the field: [`build_latest_map`](super::universe::build_latest_map)
///   is the other (`universe.rs`' `build` field, legacy `:1203`), and the belief
///   that this fold was the only one is what hid the parity gap ruling R15
///   closed. Legacy collapses once, at row construction (`:1032-1033`), which is
///   why both consumers have to.
/// * **The status is this fold's own mapping** (`:1633-1639`): `PASSED` stays
///   `PASSED`, `FAILED` and `ERROR` both become `FAILED`, `SKIPPED` stays
///   `SKIPPED`, and **anything else is passed through verbatim**. That is a
///   fourth classification — ruling R13's seventh row of
///   [`domain::service::ingest`](crate::domain::service::ingest)'s table — and it
///   is neither [`bucketize_status`](super::universe::bucketize_status), which
///   collapses `SKIPPED` and the unknowns into one `NOT_RUN`, nor
///   [`build_status_rank`], which ranks the passed-through value in its `other`
///   arm and is why the two must stay separate functions.
///
///   Legacy's first and third arms are redundant with its catch-all — the whole
///   match reduces to `"FAILED" | "ERROR" => "FAILED", other => other` — and are
///   written out anyway, here as there, because the three named statuses are the
///   contract and the catch-all is the escape hatch.
///
/// # The order is deterministic here and arbitrary in legacy
///
/// Legacy ends with `latest.into_values().collect()` (`:1652`) over a
/// `HashMap`, so its order is whatever the hasher yields. That order is
/// **observable twice**: [`build_last_run_build_distribution`]'s `latest_run_id`
/// is chosen by a strictly-`>` first-wins scan (`:1572-1579`), so an exact `ts`
/// tie resolves to whichever snapshot came first; and
/// [`build_test_details`]' final `sort_by` is stable, so two entries agreeing on
/// both sort keys keep their arrival order.
///
/// This port returns them ordered by `(repo_id, test_file)` instead, which is
/// ruling R14. **It is not a parity break**: legacy's pick among exact ties is
/// arbitrary rather than specified, so no particular legacy output is
/// contradicted. It is a testability requirement — a non-reproducible
/// aggregate cannot be pinned, and this crate has to pin it. The `>`
/// comparison in the consumer is kept verbatim so the *rule* still reads as
/// legacy's; only the tie-break becomes nameable.
/// `the_build_tests_snapshots_come_back_in_test_file_order` and
/// `the_build_distribution_picks_the_latest_run_id_by_a_strictly_greater_scan`
/// are the two tests, and the first is unaffected by the repository joining
/// the key: every fixture in this module holds one repository, so ordering by
/// the pair and ordering by the path alone agree.
///
/// # Keyed on `(repo_id, test_file)`, not `test_file` alone
///
/// A product owns several repositories and `(tenant_id, product_id)` is not a
/// unique index, so two repositories can each hold `tests/test_smoke.py`.
/// Keying this fold — and its membership set — on the path alone would let
/// one repository's snapshot stand in for the other's, which is exactly the
/// failure [`ExecRow::repo_id`](super::ExecRow::repo_id) exists to close.
#[must_use]
pub fn latest_per_test_snapshot(
    universe: &[UniverseTest],
    rows: &[ExecRow],
) -> Vec<LatestBuildTestSnapshot> {
    let universe_files: HashSet<(Uuid, &str)> = universe
        .iter()
        .map(|test| (test.repo_id, test.test_file.as_str()))
        .collect();

    // A `BTreeMap` rather than legacy's `HashMap` is the whole of ruling R14's
    // determinism: the key is `(repo_id, test_file)`, so `into_values` yields
    // the snapshots in that order instead of the hasher's.
    let mut latest: BTreeMap<(Uuid, &str), LatestBuildTestSnapshot> = BTreeMap::new();

    for row in rows {
        if !universe_files.contains(&(row.repo_id, row.test_file.as_str())) {
            continue;
        }

        // `or_insert_with` **is** legacy's first-wins guard (`:1629-1631`),
        // spelled as one lookup rather than a `contains_key` and an `insert`
        // (`clippy::map_entry`): the closure does not run when the key is there.
        latest
            .entry((row.repo_id, row.test_file.as_str()))
            .or_insert_with(|| LatestBuildTestSnapshot {
                build: collapse_build(row.build.as_deref()),
                repo_id: row.repo_id,
                test_file: row.test_file.clone(),
                status: snapshot_status(row.status.as_str()).to_owned(),
                run_id: row.run_id,
                ts: row.ts,
            });
    }

    latest.into_values().collect()
}

/// The snapshot fold's status mapping (`analytics.rs:1633-1639`), verbatim.
///
/// The **fourth** of this file's classifications and the seventh of the gear's;
/// [`latest_per_test_snapshot`]'s header states the rule and why it is not
/// [`bucketize_status`](super::universe::bucketize_status) or
/// [`build_status_rank`], and
/// [`domain::service::ingest`](crate::domain::service::ingest)'s table indexes
/// all seven.
fn snapshot_status(status: &str) -> &str {
    match status {
        "PASSED" => "PASSED",
        "FAILED" | "ERROR" => "FAILED",
        "SKIPPED" => "SKIPPED",
        other => other,
    }
}

/// One build's bar in the last-run distribution.
///
/// Legacy's `BuildLastRunDistribution` (`analytics.rs:168-175`), five fields,
/// five fields — the second with ruling R12's substitution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildLastRunDistribution {
    /// The build, [`UNKNOWN_BUILD`] included. Sorted newest-first across the
    /// returned `Vec` with `"unknown"` last — see
    /// [`build_last_run_build_distribution`].
    pub build: String,
    /// The run behind the newest snapshot in this build.
    ///
    /// **Legacy's is `latest_run_name: Option<String>`** (`:171`, from
    /// `snapshot.workflow_name` at `:1578`). Ruling R12: this gear's rows carry
    /// no run name, and **Task 25** — the DTO owner — was where the name is
    /// resolved or the field renamed; **Task 25b renamed**, and
    /// [`ExecRow::run_id`](super::ExecRow::run_id) carries why. `Option` because
    /// legacy's is, and for the same reason: the aggregate is built empty and
    /// filled by the scan.
    pub latest_run_id: Option<Uuid>,
    /// Tests whose latest row under this build passed.
    pub passed: usize,
    /// Tests whose latest row under this build failed or errored.
    pub failed: usize,
    /// `passed` + `failed` + the skipped ones. **Not the number of tests in the
    /// bucket**: a test whose status is none of the four counted words is in no
    /// counter at all, this one included — see
    /// [`build_last_run_build_distribution`].
    pub executed_total: usize,
}

/// One build's aggregation while the fold runs.
///
/// Legacy's local `BuildAgg` (`analytics.rs:1545-1552`). Module-level here
/// because a `struct` inside a function body trips
/// `clippy::items_after_statements`.
#[derive(Default)]
struct BuildAgg {
    passed: usize,
    failed: usize,
    executed_total: usize,
    latest_run_id: Option<Uuid>,
    latest_ts: Option<OffsetDateTime>,
}

/// The per-build split of every test's latest run.
///
/// `build_last_run_build_distribution` (`analytics.rs:1536-1604`), a fold over
/// [`latest_per_test_snapshot`]'s output. Four properties are legacy's and each
/// is a rendered number:
///
/// * **The grain is the test, not the row.** One snapshot per universe file, so a
///   build that ran a test twice contributes one.
/// * **The counters are a partition with a hole** (`:1557-1569`). `PASSED` and
///   `FAILED` each increment their counter *and* `executed_total`; `SKIPPED`
///   increments only `executed_total`; **everything else increments nothing**
///   — the `_ => {}` arm at `:1569`. The bucket still exists, because
///   `entry(..).or_default()` (`:1555`) runs before the match, so a build whose
///   every test reported an unrecognized status is drawn as a bar of zeroes with
///   a `latest_run_id`. That is legacy's output and
///   `the_build_distribution_passes_an_unrecognized_status_through_and_counts_it_nowhere`
///   pins it.
/// * **`latest_run_id` is a strictly-`>` first-wins scan** (`:1572-1579`), which
///   is why the snapshot order is observable and why
///   [`latest_per_test_snapshot`] makes it deterministic.
/// * **`"unknown"` sorts last**, ahead of [`compare_build_desc`] and by
///   `eq_ignore_ascii_case` (`:1593-1601`), so a runner that spelled the label
///   `UNKNOWN` is sorted last too — while still being a **separate bar** from
///   the substituted [`UNKNOWN_BUILD`], because the substitution fires only for
///   an absent or blank build.
///
/// The early return on an empty snapshot set (`:1540-1542`) is legacy's and is
/// redundant — the fold over nothing produces nothing — and is kept because it is
/// cheap and because deleting it invites a reader to wonder what it guarded.
///
/// Takes `filtered_universe` + `rows_for_scope` (`:751`, `:774`) and narrows
/// nothing itself; this module's header table says which folds get which.
#[must_use]
pub fn build_last_run_build_distribution(
    universe: &[UniverseTest],
    rows: &[ExecRow],
) -> Vec<BuildLastRunDistribution> {
    let snapshots = latest_per_test_snapshot(universe, rows);
    if snapshots.is_empty() {
        return Vec::new();
    }

    let mut items = aggregate_builds(snapshots)
        .into_iter()
        .map(|(build, agg)| BuildLastRunDistribution {
            build,
            latest_run_id: agg.latest_run_id,
            passed: agg.passed,
            failed: agg.failed,
            executed_total: agg.executed_total,
        })
        .collect::<Vec<_>>();

    items.sort_by(|left, right| {
        let left_unknown = left.build.eq_ignore_ascii_case(UNKNOWN_BUILD);
        let right_unknown = right.build.eq_ignore_ascii_case(UNKNOWN_BUILD);
        match (left_unknown, right_unknown) {
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => compare_build_desc(left.build.as_str(), right.build.as_str()),
        }
    });

    items
}

/// The counting half of [`build_last_run_build_distribution`]
/// (`analytics.rs:1554-1580`).
///
/// Split out to keep the parent under `clippy::cognitive_complexity`. The
/// returned map's own order is irrelevant: the parent sorts the whole `Vec`, and
/// no two distinct build labels compare [`Ordering::Equal`] under
/// [`compare_build_desc`] — its final tiebreak is a total order over the whole
/// string — so the sort is total and the `HashMap` is not observable. That is
/// what lets this stay legacy's `HashMap` while
/// [`latest_per_test_snapshot`] had to become a [`BTreeMap`].
fn aggregate_builds(snapshots: Vec<LatestBuildTestSnapshot>) -> HashMap<String, BuildAgg> {
    let mut by_build: HashMap<String, BuildAgg> = HashMap::new();

    for snapshot in snapshots {
        let entry = by_build.entry(snapshot.build).or_default();

        match snapshot.status.as_str() {
            "PASSED" => {
                entry.passed += 1;
                entry.executed_total += 1;
            }
            "FAILED" => {
                entry.failed += 1;
                entry.executed_total += 1;
            }
            "SKIPPED" => {
                entry.executed_total += 1;
            }
            // Legacy's `_ => {}` (`:1569`). The bucket above already exists, so
            // this is "a bar with no counters", not "no bar".
            _ => {}
        }

        // `is_none_or` is legacy's `match entry.latest_ts { Some(ts) =>
        // snapshot.ts > ts, None => true }` (`:1572-1575`) — clippy rejects the
        // `match` as `option_if_let_else`. The `>` is verbatim, which is the half
        // ruling R14 requires be recognizable.
        if entry
            .latest_ts
            .is_none_or(|latest_ts| snapshot.ts > latest_ts)
        {
            entry.latest_ts = Some(snapshot.ts);
            entry.latest_run_id = Some(snapshot.run_id);
        }
    }

    by_build
}

/// The sort rank of a status on the build-tests list: failures first.
///
/// `build_status_rank` (`analytics.rs:1948-1955`) verbatim — `FAILED` 0,
/// `PASSED` 1, `SKIPPED` 2, everything else 3. Ruling R5's **fifth** row, which
/// names this task its porter.
///
/// The `other => 3` arm is reachable only because
/// [`latest_per_test_snapshot`]'s mapping passes an unrecognized status through
/// verbatim; under
/// [`bucketize_status`](super::universe::bucketize_status) there would be nothing
/// left for it to rank. That is the whole reason the two are separate functions
/// and not one — see [`latest_per_test_snapshot`]'s header.
///
/// Note it is a *rank* and not a bucket: it never merges two statuses' counts, it
/// only decides which of them is drawn first.
#[must_use]
pub fn build_status_rank(status: &str) -> u8 {
    match status {
        "FAILED" => 0,
        "PASSED" => 1,
        "SKIPPED" => 2,
        _ => 3,
    }
}

/// One row of the build-tests drill-down.
///
/// Legacy's `BuildTestDetailItem` (`analytics.rs:177-186`), seven fields, seven
/// fields, with two substitutions and no other change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildTestDetail {
    /// The universe file, from the snapshot.
    pub test_file: String,
    /// The display name, from the **universe entry** (`:432`) — not from the row.
    pub test_name: String,
    /// The snapshot's status: [`latest_per_test_snapshot`]'s mapping, so
    /// `SKIPPED` survives and an unrecognized status is the runner's word.
    pub status: String,
    /// The run behind the snapshot.
    ///
    /// **Legacy's is `run_name: String`** (`:181`, filled at `:434` from
    /// `item.workflow_name`).
    /// Ruling R12, as on [`LatestBuildTestSnapshot::run_id`]: this gear's rows
    /// carry no run name, and **Task 25b renamed the wire field** rather than
    /// resolving one — [`ExecRow::run_id`](super::ExecRow::run_id) carries why.
    /// Not an `Option`, because a snapshot exists only where a row did.
    pub run_id: Uuid,
    /// The snapshot's instant, `run_finished_at ?? run_created_at`.
    ///
    /// **Legacy renders this as an RFC-3339 `String`** (`:435`,
    /// `item.ts.to_rfc3339()`). Kept typed here and formatted by the DTO, which
    /// is this crate's standing convention —
    /// [`LatestInfo::finished_at`](super::universe::LatestInfo::finished_at) is
    /// the precedent. Named `run_finished_at` because that is what the response
    /// field is called; it is not a claim that the run finished.
    pub run_finished_at: OffsetDateTime,
    /// The universe entry's component (`:436`).
    pub component: Option<String>,
    /// The universe entry's tags (`:437`).
    pub tags: Vec<String>,
}

/// One build's tests, failures first.
///
/// The pure half of legacy's `api_build_tests` (`analytics.rs:425-452`): filter
/// [`latest_per_test_snapshot`]'s snapshots to `build`, join each to its universe
/// entry, sort by `(build_status_rank(status), test_name)`.
///
/// Four properties, and the first two are seams rather than arithmetic:
///
/// * **The build is matched with `eq_ignore_ascii_case` and is not trimmed**
///   (`:427`). Legacy trims the query parameter and rejects an empty one in the
///   handler (`:373-376`), so an untrimmed argument matching nothing is legacy's
///   behaviour too. That validation is [`super::query::normalize_build`] (Task
///   25a) — **not** part of `normalize_overview_query`, which this line said it
///   was: it is `api_build_tests`' own check and it runs *before* that function
///   (`:373` precedes the call at `:379`). Passing `"  9.1  "` here still yields
///   an empty list, because this fold is where legacy's untrimmed match is.
/// * **The universe and the rows are already narrowed** — `filtered_universe` and
///   `rows_for_scope` (`:403-417`). This fold narrows nothing itself.
/// * **The metadata is the universe entry's, not the row's** (`:432`,
///   `:436-437`), joined on `(repo_id, test_file)` — a fix-round correction.
///   This used to join on `test_file` alone and cite "legacy's behaviour
///   exactly" for doing so, reasoning from the same wrong "two plans" premise
///   corrected elsewhere in this module (a universe file two plans of one
///   repository list is *one* entry, merged upstream by
///   `walk_repo_universe` — that input never reaches this map at all). What
///   the bare key actually collided on is two **different** repositories
///   sharing a path: their metadata would land in one `HashMap` slot and the
///   `collect`-built map's last-wins order would hand one repository's
///   component and tags to the other's test. [`LatestBuildTestSnapshot::repo_id`]
///   exists so this join can tell them apart.
/// * **The sort is stable** (`:444-452`), so two entries agreeing on both keys
///   keep [`latest_per_test_snapshot`]'s order — reachable, because legacy keys
///   the universe on `(source, repo_id, test_file)` (`:899-903`) and two distinct
///   files can therefore share a `test_name`, and pinned by
///   `build_tests_tied_on_both_sort_keys_keep_the_snapshot_order`. Ruling R14 made
///   deterministic, so this list is reproducible where legacy's was not.
///
/// Legacy's `filter_map` drops a snapshot whose file is absent from the map
/// (`:429`, `let universe_item = universe_map.get(..)?`). Ported, and
/// **unreachable as written**: both the map and the snapshot fold's membership
/// set are built from the same `universe` slice, so a snapshot that survived the
/// first check cannot miss the second. Kept because it is legacy's shape and
/// because a future caller passing two different universes would need it; not
/// tested, because no input reaches it.
#[must_use]
pub fn build_test_details(
    universe: &[UniverseTest],
    rows: &[ExecRow],
    build: &str,
) -> Vec<BuildTestDetail> {
    let by_file: HashMap<(Uuid, &str), &UniverseTest> = universe
        .iter()
        .map(|test| ((test.repo_id, test.test_file.as_str()), test))
        .collect();

    let mut items = latest_per_test_snapshot(universe, rows)
        .into_iter()
        .filter(|snapshot| snapshot.build.eq_ignore_ascii_case(build))
        .filter_map(|snapshot| {
            // The three `.clone()`s are bound before `snapshot.test_file`
            // moves below: `by_file`'s key is now `(Uuid, &str)`, not the
            // lifetime-erased `&str` legacy's shape let this borrow past a
            // move for free, so `test`'s borrow of `snapshot.test_file` has
            // to end before this closure moves it.
            let test = by_file.get(&(snapshot.repo_id, snapshot.test_file.as_str()))?;
            let test_name = test.test_name.clone();
            let component = test.component.clone();
            let tags = test.tags.clone();
            Some(BuildTestDetail {
                test_file: snapshot.test_file,
                test_name,
                status: snapshot.status,
                run_id: snapshot.run_id,
                run_finished_at: snapshot.ts,
                component,
                tags,
            })
        })
        .collect::<Vec<_>>();

    items.sort_by(|left, right| {
        (
            build_status_rank(left.status.as_str()),
            left.test_name.as_str(),
        )
            .cmp(&(
                build_status_rank(right.status.as_str()),
                right.test_name.as_str(),
            ))
    });

    items
}

/// Two build labels, newest first.
///
/// `compare_build_desc` (`analytics.rs:2188-2210`) verbatim: split both on `.`,
/// walk the segments to the **longer** of the two labels with a missing segment
/// reading as `"0"`, compare each pair numerically when both sides parse as
/// `i64` and byte-wise otherwise — arguments swapped either way, so the order is
/// descending — and return the first non-equal answer. A pair equal through every
/// segment falls through to a swapped whole-string compare (`:2209`), which makes
/// the relation a total order: no two distinct labels compare
/// [`Ordering::Equal`].
///
/// # It is not [`sorted_versions_desc`]' comparator, and they diverge two ways
///
/// The two are separate functions in legacy and stay separate here, and they are
/// **not** interchangeable. Two independent mechanisms make them disagree:
///
/// 1. **Length.** This one pads a missing segment with `"0"` over `max_len`
///    (`:2194-2195`); [`sorted_versions_desc`]' comparator `zip`s and therefore
///    stops at the shorter label (`:2172-2173`). The pad only becomes *visible*
///    when the longer label's extra segment sorts below `"0"` — for a build label,
///    an empty segment, i.e. a trailing dot — because otherwise the padded
///    comparison and the whole-string tiebreak point the same way.
/// 2. **A third arm this comparator does not have.**
///    [`sorted_versions_desc`]' has `_ if right_part != left_part =>
///    Some(right_part.cmp(left_part))` (legacy `:2178`, ours `:911`), which fires
///    whenever the two segments are **numerically equal but textually
///    different** — and short-circuits, so no later segment is ever consulted.
///    This one falls through such a pair and keeps walking. It needs no length
///    difference and no exotic label: `"2024.01.20"` against `"2024.1.15"` sorts
///    `["2024.01.20", "2024.1.15"]` here and `["2024.1.15", "2024.01.20"]` there,
///    because `01` and `1` are numerically equal, textually different, and the
///    zipping comparator decides on them and never reaches `20` against `15`.
///    Any zero-padded date or component stamp hits this.
///
/// So the earlier claim in this doc — that the two "agree everywhere" over
/// plausible labels and part only on a trailing dot — was **wrong**, and wrong in
/// the direction that invites a unification. Measured over a 22-label candidate
/// set the two disagree on twelve pairs, three of them from mechanism 2 with no
/// length difference at all; the pair count depends on the candidate set, the two
/// mechanisms do not.
/// `the_build_distribution_comparator_diverges_from_sorted_versions_desc_two_ways`
/// pins one input per mechanism.
///
/// What is still true is that `"1.2"` against `"1.2.1"` — which the plan and this
/// task's brief both offer as *the* discriminating case — is **not** one: both
/// comparators answer `1.2.1` first. So the brief's
/// `builds_sort_newest_first_by_numeric_segment` does not discriminate the two,
/// and it says so in its own doc.
///
/// Beyond the arithmetic, [`sorted_versions_desc`] is a whole *pipeline* — trim,
/// drop blanks, sort, dedup — over `UniverseTest::versions`, where this is a bare
/// comparator over a build label. Legacy kept them apart, and Phase B's standing
/// instruction is to port rather than to improve.
#[must_use]
pub fn compare_build_desc(left: &str, right: &str) -> Ordering {
    let left_parts = left.split('.').collect::<Vec<_>>();
    let right_parts = right.split('.').collect::<Vec<_>>();
    let max_len = left_parts.len().max(right_parts.len());

    for idx in 0..max_len {
        // The `"0"` pad is one of the two things that distinguish this comparator
        // from `sorted_versions_desc`' zipping one (`:2194-2195`); the other is
        // the arm that one has and this one does not — see this function's header.
        let left_raw = left_parts.get(idx).copied().unwrap_or("0");
        let right_raw = right_parts.get(idx).copied().unwrap_or("0");

        let left_num = left_raw.parse::<i64>().ok();
        let right_num = right_raw.parse::<i64>().ok();

        let cmp = match (left_num, right_num) {
            (Some(left_value), Some(right_value)) => right_value.cmp(&left_value),
            // Either side unparsable: a reverse **string** compare, not an
            // error and not a zero (`:2202`).
            _ => right_raw.cmp(left_raw),
        };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }

    right.cmp(left)
}

#[cfg(test)]
#[path = "aggregates_tests.rs"]
mod aggregates_tests;
