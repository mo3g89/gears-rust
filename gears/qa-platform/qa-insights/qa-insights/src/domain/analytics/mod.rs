//! The analytics domain: the row shape the pure cores consume, and the filter
//! that selects it.
//!
//! Only the input types existed at Task 11. The cores themselves — the universe
//! join, summary and lists, heatmap and trend, flaky and quality vectors, build
//! distribution and build-tests drill-down — arrived at Tasks 20-24 and are pure
//! functions over [`ExecRow`].
//!
//! * [`universe`] — Task 20: the alias join between qa-catalog's test universe
//!   and these rows, and the latest-status fold over it. **Every later
//!   aggregate in the phase keys on the file this module resolves**, so it is
//!   the first thing to read and the first thing to suspect when a test shows
//!   as `not_run`.
//! * [`query`] — Task 25a: the overview query string's five validation rules and
//!   their normalization, with the two day-count defaults legacy applies before
//!   [`aggregates`]' clamps see them. **A domain module and not a DTO**, so the
//!   rules are testable without a router; the HTTP status each rejection carries
//!   is [`crate::api::rest::error`]' single `Validation` arm and that module's
//!   header carries the split.
//! * [`aggregates`] — Tasks 21-24: the summary, the per-test tallies, the three
//!   lists, the heatmap, the trend, the flaky detector, the quality vectors, the
//!   three group breakdowns, the build distribution and the build-tests
//!   drill-down, all of them folds over [`universe`]'s output. **Task 24 closed
//!   the phase's pure folds** and **Task 25b closed the surface**:
//!   [`crate::domain::service::analytics`] assembles them in legacy's order and
//!   [`crate::api::rest::dto`] renders them.
//! * [`export`] — Task 26: `GET /qa/v1/analytics/export`'s reduction of
//!   [`crate::domain::service::analytics::AnalyticsOverview`] to one section (or
//!   all of them), as CSV or JSON. Holds the `section` vocabulary and its `400`,
//!   and the whole CSV builder — but not JSON assembly, which needs `serde` and
//!   therefore lives at the REST boundary; that module's header carries the
//!   split in full.
//!
//! The charts are the first folds in the phase that are not functions of the rows
//! alone: they take a `today: Date`, because every window in legacy is anchored
//! on `Utc::now().date_naive()` *inside* the fold. That date arrives from
//! [`Clock`](crate::domain::ports::Clock), which Task 22 added for the purpose,
//! and Task 23's `build_flaky` is the third fold to take it — its window is the
//! **trend's** clamp rather than a third one.
//!
//! Two row shapes enter the phase, not one: [`ExecRow`] is the file-level
//! outcome every fold reads, and [`CaseRow`] is the function-level outcome only
//! the per-case summary reads. See [`CaseRow`] for why the second is a distinct
//! type rather than `qa_insights_sdk::TestCaseResultRecord`.

use time::{Date, OffsetDateTime};
use uuid::Uuid;

pub mod aggregates;
pub mod export;
pub mod query;
pub mod universe;

/// One executed test result, flattened with the run attributes analytics reads.
///
/// # This is legacy's `ExecRowRaw`, not legacy's `ExecRow`
///
/// Legacy has **both**, and the plan cites only the second while describing
/// neither accurately. The two are a pipeline:
///
/// * `ExecRowRaw` (`manager/src/routes/analytics.rs:321-331`) is the `sqlx`
///   projection — nine fields straight off `test_results JOIN run_results`,
///   including `test_name` and a **nullable** `test_file`.
/// * `ExecRow` (`:267-277`) is the normalized row, built from it at `:1029` by
///   `load_universe_and_rows`. Eight fields. Three things happen in between,
///   and all three need the universe, which is why they cannot happen in SQL:
///   `resolve_row_test_file` (`:1750`) resolves an absent `test_file` through
///   an alias map keyed on `test_name`; the row is then dropped unless the
///   resolved file is in the universe; and `ts`/`day` are derived from
///   `finished_at ?? created_at`.
///
/// A repository cannot produce legacy's `ExecRow`, because two of those three
/// steps take the universe as input and the universe comes from qa-catalog.
/// So the type the repository returns is the *raw* one, and Tasks 20-24 do the
/// resolution — exactly where legacy does it. Legacy's post-resolution `ExecRow`
/// becomes a core-internal type at Task 20.
///
/// # What the plan got wrong about the fields
///
/// The plan lists `test_file`, `test_name`, `status`, `day`, `platform`,
/// `build`, `workflow_name`/`run_id`, `branch`, `ts`. Checked field by field
/// against `:267`:
///
/// * **`test_name` is not on legacy's `ExecRow`.** It is on `ExecRowRaw`, and
///   it is consumed by `resolve_row_test_file` and then dropped. It is here for
///   that reason and **not as an aggregation grain**: every analytics consumer
///   — `build_stats_map` (`:1212`), `build_heatmap` (`:1451`), `build_trend`
///   (`:1495`), `build_flaky` (`:1655`), `latest_per_test_snapshot` (`:1615`) —
///   keys on `test_file`. A core that grouped by `test_name` would compute a
///   different number from legacy and nothing would fail.
///
///   (The *dashboard* does group by `test_name` — `flaky_tests` at
///   `manager/src/routes/dashboard.rs:379-405` groups by `tr.test_name,
///   rr.plan_id`. Two surfaces, two grains, both in legacy. Analytics is the
///   one this type feeds.)
/// * **`branch` is not a field of either struct.** In legacy it is a *predicate*
///   — `COALESCE(NULLIF(r.source_ref,''), NULLIF(r.test_version,'')) = $N`
///   (`:966-969`) — so it belongs on [`UniverseFilter`], not on the row. Same
///   for the product version, which is legacy's `WHERE r.app_version = $1`.
/// * **`repo_id` is on legacy's `ExecRow` and the plan omits it** — and it is
///   *write-only*: assigned at `:1036` and read by nothing. Counted, not
///   eyeballed: `grep 'row\.repo_id'` over `analytics.rs` returns exactly the
///   one construction site. It is therefore **not** reproduced here. Carrying a
///   dead field forward is how a later task talks itself into keying on it.
/// * **`build` is on legacy's `ExecRow` and the plan lists it, correctly.** It
///   is here, as [`Self::build`], and the column it reads was added by Task 12
///   — see below.
///
/// # `build` reads a column Task 12 added, and Task 11 deliberately shipped
/// without
///
/// Legacy's `ExecRow::build` is `r.app_build`, normalized with an `"unknown"`
/// fallback (`:1032-1033`). Task 24 needs it —
/// `build_last_run_build_distribution` groups by it and `api_build_tests`
/// filters on it — and `qa_runs_sdk`'s run carries it.
///
/// **Task 10's `qa_test_results` had no `app_build` column.** It denormalized
/// `product_version` (legacy's `app_version`, which is the analytics *filter*)
/// and not `app_build` (which is the analytics *projection*). The two are
/// different values and only one crossed over.
///
/// Task 11 therefore left the field off entirely rather than shipping one that
/// was always `None`: a `None`-filled `build` would have emptied the build
/// distribution silently, on a screen that still rendered, while an absent
/// field made Task 24 fail to compile until someone closed the gap.
///
/// **Task 12 closed it**, in its Step 0b, and it was nearly free — the third
/// term of the original cost estimate was already done: `qa_runs_sdk::Run::app_build`
/// (`qa-runs-sdk/src/models.rs:316`) exists over a real `qa_runs.app_build`
/// column (`m20260813_000003_initial.rs:219`), and Task 13's ingest already
/// fetches the run object once per run and caches it for the batch. Nor was it
/// entangled with the plan's open question 1, which is about
/// `product_id`/`version`/`scope` and the qa-catalog mapping VHP-319 left
/// unresolved; `app_build` is an opaque snapshot string qa-runs already
/// denormalizes onto its own row.
///
/// It landed at **Task 12** rather than 13 because Task 12 writes the mapper: a
/// column arriving one task later would have meant writing that mapper and
/// immediately rewriting it, and every row Task 13 wrote in between would have
/// needed a backfill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecRow {
    /// The run this outcome came from. Legacy's `workflow_name`, which is its
    /// run identity.
    ///
    /// **Two folds read it, and the value reaches three rendered fields, every
    /// one of which only echoes it back to the UI.** This doc said
    /// "`LatestInfo::workflow_name` (`:1201`) is the only consumer" until Task 24,
    /// which added the second reader. The readers are
    /// [`universe::build_latest_map`](super::analytics::universe::build_latest_map)
    /// and
    /// [`aggregates::latest_per_test_snapshot`](super::analytics::aggregates::latest_per_test_snapshot);
    /// the three legacy fields are `AnalyticsListItem::last_run_name` (`:1421`),
    /// `BuildLastRunDistribution::latest_run_name` (the field at
    /// `analytics.rs:171`, filled at `:1578`) and `BuildTestDetailItem::run_name`
    /// (`:181`, filled at `:434`).
    ///
    /// Since this gear carries an id where legacy carried a *name*, all three
    /// fields are an unresolved **label** rather than an unresolved value — the same shape as
    /// [`Self::platform_id`]'s obligation below, and recorded on the two types
    /// that carry it:
    /// [`aggregates::LatestBuildTestSnapshot`](super::analytics::aggregates::LatestBuildTestSnapshot)
    /// and
    /// [`aggregates::BuildTestDetail`](super::analytics::aggregates::BuildTestDetail),
    /// both of which named Task 25 — the DTO owner — as who closes it.
    /// **Task 25b closed it by renaming rather than by resolving**: there is no
    /// bulk run-name read in this gear and a per-item `get_run` would be an N+1
    /// across a gear boundary on a list the size of the universe, so the wire
    /// fields are `last_run_id`, `latest_run_id` and `run_id` and
    /// [`crate::api::rest::dto::AnalyticsListItemDto`] carries the argument.
    /// Nothing between here and the response `Display`s the id into a name.
    pub run_id: Uuid,
    /// The stored path, **unnormalized and possibly empty**. `""` is legal and
    /// meaningful — the column is `NOT NULL DEFAULT ''` — and it is precisely
    /// the case [`Self::test_name`] exists to resolve. Task 20 applies
    /// `normalize_test_path` and the alias map before using it as a key.
    pub test_file: String,
    /// The file-level display name. **Not a grain** — see this type's header.
    /// Its one job is to let Task 20 attribute a row whose `test_file` is `""`.
    pub test_name: String,
    /// The raw uppercase status, exactly as the runner produced it. Deliberately
    /// not an enum and not validated: the bucketing into passed/failed/other is
    /// the cores' (`bucketize_status`), and an unrecognized spelling must bucket
    /// as "other" rather than fail the read.
    pub status: String,
    /// The build the run was executed against — `qa_test_results.app_build`,
    /// denormalized from `qa_runs.app_build`.
    ///
    /// **`None` is carried through, not resolved here.** Legacy substitutes
    /// `"unknown"` for an absent or empty build
    /// (`analytics.rs:1032-1033`, `normalize_optional(row.app_build.as_deref())
    /// .unwrap_or_else(|| "unknown".to_string())`), so its `ExecRow::build` is a
    /// `String` and this one is an `Option<String>`. The difference is
    /// deliberate: legacy normalizes at the *consumer*, and putting the label in
    /// the repository would make "the run reported no build" and "the run
    /// reported the literal string `unknown`" indistinguishable to every core
    /// downstream.
    ///
    /// Legacy's `normalize_optional` also maps `""` to absent. That collapse is
    /// the consumer's too, for the same reason.
    ///
    /// **Task 24 applied it, at both folds that report the value** — and its first
    /// pass applied it at one, because this doc claimed there was only one.
    /// Corrected under controller ruling R15. The rule is
    /// [`universe::collapse_build`](super::analytics::universe::collapse_build)
    /// and its label is
    /// [`universe::UNKNOWN_BUILD`](super::analytics::universe::UNKNOWN_BUILD);
    /// the two folds that collapse are
    /// [`universe::build_latest_map`](super::analytics::universe::build_latest_map)
    /// — whose output reaches the wire as `AnalyticsListItem::last_build`
    /// (legacy `:1422`, over a `LatestInfo::build` that legacy fills at `:1203`
    /// from an **already normalized** row) — and
    /// [`aggregates::latest_per_test_snapshot`](super::analytics::aggregates::latest_per_test_snapshot).
    ///
    /// The cost the paragraph above describes is now measured and pinned rather
    /// than predicted, once per fold:
    /// `the_build_distribution_collapses_absent_blank_and_literal_unknown_builds`
    /// asserts that a `None`, a `"   "` and a literal `"unknown"` land in one bar,
    /// and `the_latest_build_is_collapsed_exactly_as_legacy_collapses_it` asserts
    /// the same substitution and the trim on the list item.
    pub build: Option<String>,
    /// Legacy's `platform`, which is the platform *name*; this port stores the
    /// id. `None` for a run that named no platform — legacy normalizes `""` to
    /// `None` at `:1034` and this column is already nullable, so the two agree.
    ///
    /// **A carried obligation, because this is a `Uuid` and legacy's was a
    /// label.** `build_grouped_summaries` puts `row.platform` straight into a
    /// `BTreeSet<String>` (`analytics.rs:1116-1119`) and returns it as the
    /// UI-visible group key (`:1138`, then `:1144`), so the grouped-summaries
    /// surface would
    /// regress to raw UUIDs unless something resolves ids to names — a
    /// qa-environments lookup, done once per distinct id and outside the
    /// per-row path. The id is the right thing to *store* (Task 10's schema
    /// argument); it is not the right thing to *display*, and nothing between
    /// here and the response will notice the difference.
    ///
    /// **Task 23 shipped that surface and left the obligation open, deliberately
    /// and visibly.** This gear has no qa-environments port and inventing one was
    /// out of scope, so
    /// [`aggregates::PlatformGroupSummary`](super::analytics::aggregates::PlatformGroupSummary)
    /// keys the platform breakdown on `platform_id` instead of on legacy's
    /// `value: String`. A `Uuid` in a field named for a `Uuid` is a gap a reader
    /// can see; the same `Uuid` `Display`-formatted into a field named `value`
    /// would be a label nobody would question. The ordering changes with the
    /// resolution too — legacy orders that list by name — and that type says so.
    ///
    /// **Task 25a built the port
    /// ([`EnvironmentReader`](crate::domain::ports::EnvironmentReader)) and Task 25b
    /// called it.** The analytics service resolves the distinct ids of a whole
    /// payload in one cross-gear read, and
    /// [`crate::api::rest::dto::EnvironmentGroupSummaryDto`] renders each bar with
    /// **both** the id and the name — the id because a platform the caller cannot
    /// see resolves to nothing and the bar is kept rather than dropped. The
    /// re-sort onto the name happens there too, so the rendered order is legacy's
    /// again.
    pub platform_id: Option<Uuid>,
    /// `run_finished_at ?? run_created_at`, legacy's `:1028`. The fallback
    /// matters: a run still in progress has no finish instant, and legacy sorts
    /// and buckets those rows by **the run's** creation time rather than dropping
    /// them.
    ///
    /// **The fallback is the *run's* instant, not the row's**, and this doc said
    /// `created_at` until controller Ruling C. Legacy's `ExecRowRaw` is a
    /// `SELECT … r.finished_at, r.created_at` off `run_results`
    /// (`manager/src/routes/analytics.rs:961`), so its `:1028`
    /// `row.finished_at.unwrap_or(row.created_at)` is the run's creation instant
    /// — where `qa_test_results.created_at` is when *this row* was written, and
    /// ingest rewrites a run's rows on every result event. Reading the row's
    /// would have made an unfinished run's position in the newest-first order
    /// move every time another of its results landed, and with it which row
    /// `latest_per_test` calls latest.
    /// `qa_insights_sdk::TestResultRecord::run_created_at` carries the column and
    /// `infra::storage::results_sea_repo::effective_ts` the expression.
    pub ts: OffsetDateTime,
    /// [`Self::ts`]'s calendar day, legacy's `ts.date_naive()`. The heatmap and
    /// the trend key on it, and the flaky window compares against it.
    ///
    /// Derived rather than stored, and precomputed here rather than in the
    /// cores only because both would otherwise recompute it per row per
    /// section.
    pub day: Date,
}

/// One executed test **case** — a single test function within a file — flattened
/// with the one run attribute the per-case summary joins on.
///
/// # This is legacy's `CaseRow`, and it is four columns of ten
///
/// `attach_case_data` (`manager/src/routes/analytics.rs:1288-1395`) declares its
/// own private projection at `:1308-1319`:
///
/// ```sql
/// SELECT rr.workflow_name, tcr.test_file, tcr.status, tcr.ticket
/// FROM test_case_results tcr JOIN run_results rr ON rr.id = tcr.run_id
/// WHERE rr.workflow_name = ANY($1)
/// ```
///
/// Four columns, and this type is those four with legacy's run identity
/// (`workflow_name`) spelled as a run id, exactly as
/// [`ExecRow::run_id`] spells the same thing.
///
/// # Why not `qa_insights_sdk::TestCaseResultRecord`
///
/// Because it is the *wire* shape and it carries ten fields, six of which no
/// aggregate reads: `id`, `nodeid`, `name`, `duration`, `reason`, `created_at`.
/// A repository method returning it would have to materialize all ten for every
/// case row of every test's latest run, on a table
/// `cpt-cf-qa-nfr-scale` sizes in the millions, so that a fold could throw six
/// of them away. The write-side [`NewTestCaseResult`](crate::domain::repos::NewTestCaseResult)
/// is no better a fit: it has no `run_id` at all — the run is the argument of the
/// upsert it feeds — and the join key is precisely what this fold needs.
///
/// So the analytics side gets its own narrow row, for the same reason
/// [`ExecRow`] is not `qa_insights_sdk::TestResultRecord`. Added by Task 21,
/// which is the first task with a consumer for it; the repository method that
/// fills it is
/// [`ResultsRepository::case_rows_for_runs`](crate::domain::repos::ResultsRepository::case_rows_for_runs),
/// added by **Task 25b** alongside the rest of the read path.
///
/// # The join key is `(run_id, test_file)`, and both halves matter
///
/// Legacy indexes these rows on the **pair** (`:1332-1339`) and then looks each
/// universe file up under *its own latest run* (`:1352`). A fold keyed on
/// `test_file` alone would attribute an older run's cases to a file whose latest
/// run reported none — which is the exact case the file-level fallback exists to
/// cover, so the undercount would be replaced by a wrong count rather than by a
/// right one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseRow {
    /// The run this case outcome came from — legacy's `run_results.workflow_name`.
    /// Half of the join key; see this type's header.
    pub run_id: Uuid,
    /// The owning file, as `qa_test_case_results` stores it.
    ///
    /// **Not normalized and not alias-resolved**, and legacy does not resolve it
    /// either: it compares the stored string against
    /// [`ExecRow::test_file`] *after* Task 20's
    /// [`resolve_rows`](universe::resolve_rows) has rewritten that one to the
    /// universe's spelling. A case row whose file is spelled differently from its
    /// own file-level row therefore misses, and the file falls back to one
    /// synthetic case. Ported as-is under Phase B's standing instruction; the
    /// case-level table is written by the same ingest pass as the file-level one
    /// (`domain::service::ingest`), from the same
    /// `qa_runs_sdk::RunTestResult::test_file`, so in practice the two spellings
    /// agree at the source.
    pub test_file: String,
    /// The raw uppercase case status, verbatim. The case-level vocabulary is
    /// **wider** than the file-level one: it adds `XFAIL` and `XPASS`, which is
    /// why the per-case summary has six counters where the file-level summary has
    /// three, and why `effective_case_status` needs a six-way severity order.
    pub status: String,
    /// The case-level bug reference, when the runner marked one. Rolled up per
    /// file, sorted and de-duplicated, into the list item's `case_tickets`
    /// (`analytics.rs:1365-1369`, `:1387-1394`).
    pub ticket: Option<String>,
}

/// Which executed rows an analytics read is about.
///
/// The `WHERE` clause of legacy's two `ExecRowRaw` queries (`:959-1010`),
/// minus the parts this gear's schema spells differently.
///
/// # No longer provisional: Task 25b answered it, and added no field
///
/// Legacy's "all" scope reads
/// `r.product_key = $2 OR (r.product_key IS NULL AND r.plan_id = ANY($3))`
/// (`:992-995`, the disjunction itself at `:993-994`; this said `:990-993`
/// until Task 20 opened the file — `:990` is the `JOIN`). **There is no
/// `product_key` column in this schema and no `plan_id`**: VHP-319 deleted
/// legacy's product-version model, qa-catalog ships `products` +
/// `repo_branches` instead, and a plan's identity here is the
/// `(repo_id, plan_path)` pair. Resolving that disjunction was the plan's own
/// largest open question — its carried item 1.
///
/// **The answer, recorded by Task 25b in
/// [`crate::domain::service::analytics`]' `universe_filter`:** both of legacy's
/// scopes reduce to the **second** disjunct, and it is fed exactly as legacy
/// feeds it — from the universe's own plan set (`:942-947`, `universe_plan_ids`),
/// which is what [`Self::plans`] already described. The first disjunct is
/// **dropped**, because nothing in this subsystem records a product on a result
/// row; the practical loss is a run whose target names no plan (a custom plan or
/// a collect run), which ingest stores with a `NULL` `plan_path` deliberately —
/// `domain::service::ingest::plan_identity` records that the `NULL` exists *for*
/// keeping such runs out of a plan-scoped analytics read. The product itself is
/// applied one layer up, as
/// [`CatalogReader::list_universe`](crate::domain::ports::CatalogReader::list_universe)'s
/// `product_id` argument, which is what decides the universe the plan set is
/// derived from.
///
/// **Reassigned from Task 20 to Task 25**, in the plan and here, on
/// 2026-08-21 by Task 20 itself, and discharged by Task 25b. Task 20 was named on the assumption that it
/// would be this struct's first consumer, and it is not: the universe core is
/// pure, takes `&[UniverseTest]` and `&[ExecRow]`, and never sees a product, a
/// version or a scope — so there was nothing there for the mapping to be
/// recorded *against*. Tasks 21-24 are pure folds over an already-fetched
/// `Vec` for the same reason. **Task 25 is the first task that turns a request
/// into a read**: it authors `domain/service/analytics.rs`, ports
/// `normalize_overview_query` (`analytics.rs:2392`) and `parse_scope`
/// (`:2104`), compiles this filter, and passes `product_id` to
/// [`CatalogReader::list_universe`](crate::domain::ports::CatalogReader::list_universe).
/// Task 20 guessed at nothing, which is why this section still says
/// "provisional" rather than recording an answer.
///
/// So this struct carries the three predicates that *are* settled and
/// expressible against `qa_test_results`, and **Task 25b did not extend it** —
/// the mapping above needs no new field, which is the outcome this section's
/// standing instruction ("do not extend the struct without a call site") was
/// asking for.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UniverseFilter {
    /// Legacy's `WHERE r.app_version = $1`, denormalized here as
    /// `qa_test_results.product_version`. Legacy always binds it: the route
    /// parameter is required (`normalize_overview_query`) and both `ExecRowRaw`
    /// queries carry the predicate unconditionally (`analytics.rs:961`, `:988`).
    ///
    /// `None` therefore means "every version", **which legacy never asks for**,
    /// and it is the single widest read this type can express. Pair it with
    /// [`Self::since`]. (Corrected 2026-08-20: this field previously justified
    /// `None` as existing "for the dashboard, whose queries carry no version
    /// predicate at all". The dashboard indeed has no version predicate, but it
    /// never performs a raw-row read either — `dashboard.rs:381-401` aggregates
    /// in SQL, windowed to seven days with `LIMIT 10`. The justification cited a
    /// windowed, aggregated query to license an unwindowed, unaggregated one.)
    pub product_version: Option<String>,
    /// Lower bound on the row's effective timestamp — legacy's
    /// `COALESCE(finished_at, created_at)`, i.e. [`ExecRow::ts`]. `None` reads
    /// every row the other predicates admit.
    ///
    /// # Why a bound exists here when legacy's analytics query has none
    ///
    /// Legacy's two `ExecRowRaw` queries are bounded only by `app_version`
    /// (`:961`, `:988`), and every window the overview applies — the heatmap's
    /// 1-30 days, the trend's 7-365, the flaky cutoff — is applied **in memory
    /// over the already-fetched `Vec`**. That is affordable on a single-tenant
    /// installation and is not affordable here: `cpt-cf-qa-nfr-scale` targets 5M
    /// rows on `qa_test_results`, and `list_for_universe` returns an
    /// unaggregated `Vec<ExecRow>`. Pushing the window the caller already knows
    /// about down into SQL is the difference between reading a bounded slice and
    /// reading the table.
    ///
    /// # A lower bound only, deliberately
    ///
    /// Not a half-open range like `ResultsRepository::ingested_run_ids_between`.
    /// That method tiles consecutive reconcile windows, so it needs both ends to
    /// avoid skipping or replaying a run at the seam. Analytics never tiles: every
    /// consumer asks for "the last N days", and legacy's own windowed read is
    /// likewise open-ended above (`>= NOW() - INTERVAL '7 days'`,
    /// `dashboard.rs:391`). An upper bound would be a parameter with no caller.
    ///
    /// # What Task 12 must know to make this reach an index
    ///
    /// `idx_qa_test_results_tenant_finished` is `(tenant_id, run_finished_at
    /// DESC)` — exactly this window and exactly `list_for_universe`'s ordering.
    /// But the quantity being bounded and ordered is legacy's
    /// `COALESCE(run_finished_at, run_created_at)`, and **no index covers a
    /// `COALESCE`**, so written naively both the filter and the sort fall back to
    /// a scan and a filesort over the whole result set.
    ///
    /// The escape is [`Self::finished_only`]: when it is set,
    /// `run_finished_at IS NOT NULL` holds for every candidate row, so
    /// `COALESCE(run_finished_at, run_created_at)` is *provably*
    /// `run_finished_at`.
    /// The planner cannot deduce that — Task 12 must **write the bare column**
    /// in both the predicate and the `ORDER BY` on that path, at which point the
    /// index serves both. On the `finished_only == false` path there is no such
    /// equivalence and the filesort is real; no caller needs that path today.
    pub since: Option<OffsetDateTime>,
    /// The plans in scope, as `(repo_id, plan_path)` pairs.
    ///
    /// Empty means "no plan restriction". One entry is legacy's plan scope
    /// (`WHERE r.plan_id = $2`); many is the expressible half of its "all"
    /// scope (`plan_id = ANY($3)`, fed from the universe's own plan set).
    pub plans: Vec<PlanRef>,
    /// Legacy's branch predicate, as a plain equality: `None` applies **no
    /// predicate at all**, i.e. every branch (legacy's `$N::text IS NULL` guard
    /// at `:967-970`, and the route parameter's own doc at `:24-27`, "Absent =
    /// all branches"). `infra::storage::results_sea_repo`'s
    /// `an_absent_branch_filter_matches_every_row` pins both halves.
    ///
    /// # There is no fallback applied anywhere, and this doc used to say there
    /// # was
    ///
    /// Legacy coalesces `source_ref` over `test_version` at query time
    /// (`:967-970`). **This gear does not apply that fallback at ingest — it has
    /// nothing to apply it to.** `qa_runs_sdk::Run` carries exactly one branch
    /// field, `test_version`, documented as "Branch actually resolved and
    /// executed against" (`qa-runs-sdk/src/models.rs:297-300`), and `source_ref`
    /// does not exist anywhere in qa-runs: that gear collapsed legacy's pair at
    /// launch and records the decision at
    /// `qa-runs/src/domain/service/runs.rs:1047-1050` ("this port records one
    /// branch label in one column, so there is no fallback to write"). So the
    /// stored value *is* the effective one because there was only ever one
    /// source, not because a coalesce ran on write.
    ///
    /// Measured by Task 20, whose report §3 carries the grep; corrected here and
    /// in `domain::service::ingest`'s projection comment in the same pass. **Do
    /// not describe an ingest-time fallback again** — the next reader of this
    /// field will go looking for the code that does it.
    ///
    /// Note the asymmetry with the *universe* side of the same word:
    /// [`CatalogReader::list_universe`](crate::domain::ports::CatalogReader::list_universe)'s
    /// `branch: None` means each repository's **default** branch, not every
    /// branch. Legacy runs both at once.
    pub branch: Option<String>,
    /// Legacy's `r.phase IN ('Succeeded', 'Failed')` — only rows from a run
    /// that reached a terminal state.
    ///
    /// **This gear has no phase column to test.** The nearest expressible thing
    /// is `run_finished_at IS NOT NULL`, which is what an implementation must
    /// use, and it is *wider* than legacy: a cancelled or errored run has a
    /// finish instant and no `Succeeded`/`Failed` phase. Recorded rather than
    /// papered over — narrowing it needs a run-state column this schema does
    /// not have.
    ///
    /// **Task 25b looked at it with the rest of carried item 1 and left it
    /// wider**, deliberately: the only alternative is denormalizing
    /// `qa_runs_sdk::RunState` onto `qa_test_results`, which is a migration and a
    /// backfill for a difference that adds a cancelled run's partial results to
    /// an aggregate rather than removing a correct one. Every analytics caller
    /// sets this.
    pub finished_only: bool,
}

/// One plan's identity: the `(repo_id, plan_path)` pair this port uses
/// everywhere in place of a plan UUID.
///
/// There is no plan UUID to use. A plan is materialized on read from
/// qa-catalog, no plans table exists, and legacy's own `jira_bugs.plan_id` is a
/// lossy path-derived slug rather than a key (`compose_repo_plan_id`,
/// `manager/src/services/plans.rs:789-801`). `qa_insights_sdk`'s module header
/// states this once for the whole contract; this is the domain-side spelling of
/// the same pair.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanRef {
    pub repo_id: Uuid,
    /// The `plan.yaml` path within the repository's content root.
    pub plan_path: String,
}
