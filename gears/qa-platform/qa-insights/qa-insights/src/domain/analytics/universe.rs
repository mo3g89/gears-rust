//! The analytics universe: the join between qa-catalog's test universe and this
//! gear's executed rows, and the latest-status fold over it.
//!
//! Legacy's `load_universe_and_rows` (`manager/src/routes/analytics.rs:805-1044`)
//! in this architecture's two halves. The universe half — walk the plans, resolve
//! each entry to a file on a checkout, parse `TEST_META` — is qa-catalog's, and
//! arrives over [`CatalogReader`](crate::domain::ports::CatalogReader) as
//! `qa_catalog_sdk::UniverseTest`. The rows half is
//! [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe).
//! **This module is what legacy does between them**, and it is pure: no `async`,
//! no repository, no SQL.
//!
//! # The pipeline, in the order a caller must run it
//!
//! **Two steps, not three.**
//!
//! 1. [`resolve_rows`] over the rows — rewrites each row's `test_file` to the
//!    file the universe knows it by, and drops the rows that resolve to nothing
//!    or to a file outside the universe. Legacy does this at `:1018-1040`, once,
//!    before any aggregate runs. It builds the [`AliasMap`] **itself**, from the
//!    universe it is handed, so a caller neither builds nor passes one.
//! 2. [`build_latest_map`] and, from Task 21 on, the other folds — all of them
//!    keyed on the *resolved* file.
//!
//! [`build_alias_map`] and [`resolve_row_test_file`] are therefore **not** steps
//! in this pipeline; they are the two halves of step 1, exposed because the alias
//! rules are the phase's most easily-missed behaviour and are pinned directly by
//! `universe_tests`, and because a caller with one name and no row set — a plan
//! drill-down looking up a single test — has no other entry point. (This header
//! listed `build_alias_map` as step 1 until Task 20's fix round, which meant a
//! caller following it literally built a map, passed it nowhere, and had it
//! silently rebuilt inside step 1.)
//!
//! Step 1 is not optional and is not a tidying pass. `qa_test_results.test_file`
//! is `NOT NULL DEFAULT ''`, so a row whose producer reported no path stores the
//! empty string, and the only thing that can attribute it is
//! [`ExecRow::test_name`](super::ExecRow::test_name) through the alias map. A fold
//! run over unresolved rows silently keys those on `""` and every test they
//! belong to renders as `not_run`.
//!
//! [`build_latest_map`] takes the universe again and re-applies the membership
//! check that [`resolve_rows`] already applied. That redundancy is legacy's
//! (`:1190-1192` guards after `:1022` already filtered) and is kept: the two
//! functions are separately callable, and a fold that quietly trusted its input
//! would be the one place a future caller could skip step 1 without a symptom.
//!
//! # What is deliberately *not* ported
//!
//! Legacy re-sorts the resolved rows by `ts` descending at `:1042`. This port
//! does not, and the reason is on
//! [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe):
//! its order is `sort_key DESC, created_at DESC, ingest_ordinal DESC, id DESC`,
//! and neither `created_at` nor `ingest_ordinal` nor `id` is on an
//! [`ExecRow`](super::ExecRow) — so a re-sort here could only *lose* the
//! tiebreak, never reproduce it. Legacy gets away with it because `sort_by` is
//! stable and its SQL had already ordered on the same key. The order this module
//! receives is authoritative; [`resolve_rows`] preserves it exactly.
//!
//! `build_stats_map` (`:1212-1225`) and `build_status_rank` (`:1948-1955`) are
//! *not* here either. The first is Task 21's per-test tallies, and it lives in
//! [`aggregates`](super::aggregates). The second is only `api_build_tests`' sort
//! comparator (`:442-452`) and must not be wired into the latest-map, which is
//! why it is not in this module at all.
//!
//! **Corrected 2026-08-21 by Task 21.** This paragraph said `build_stats_map`'s
//! three-way split is "a **different** fold from [`bucketize_status`] despite
//! looking the same — it counts a `SKIPPED` row as skipped rather than as
//! not-run", which claims an arithmetic difference. Measured against `:1218-1222`
//! and `:1940-1946` there is none: both match `PASSED`, then `FAILED | ERROR`,
//! then a catch-all, so they are **the same partition under two names**. Only the
//! label of the third class differs, because the two feed different columns of the
//! UI. The rule that *is* different is `effective_case_status` (`:1270`), a
//! six-candidate severity pick at the case level; a fourth,
//! `latest_per_test_snapshot`'s mapping (`:1633-1639`), keeps `SKIPPED` and
//! passes an unrecognized status through verbatim.
//! [`aggregates`](super::aggregates)' header tabulates all four side by side.
//!
//! # Where the branch went
//!
//! Nowhere in this module, and that is the answer rather than an omission.
//! Legacy applies its branch in two places at once: the universe walk takes the
//! branch's checkout (`:810-816`), and the row query carries
//! `COALESCE(NULLIF(r.source_ref,''), NULLIF(r.test_version,'')) = $N` guarded by
//! `$N::text IS NULL` (`:967-970`, `:997-1000`). Here the first is
//! `CatalogReader::list_universe`'s `branch` argument and the second is
//! [`UniverseFilter::branch`](super::UniverseFilter::branch) in the repository's
//! `WHERE`. The two spell `None` differently — default branch there, *every*
//! branch here — and both are documented where they are implemented. An
//! [`ExecRow`](super::ExecRow) carries no branch to filter on, deliberately, so
//! there is no branch predicate for a pure core to apply.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use qa_catalog_sdk::UniverseTest;
use qa_insights_sdk::CollectCount;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::analytics::ExecRow;

/// The bucket a `PASSED` row lands in.
const PASSED: &str = "PASSED";

/// The bucket `FAILED` **and** `ERROR` rows land in.
const FAILED: &str = "FAILED";

/// The bucket every other status lands in, and the bucket a test with no row at
/// all reads as.
///
/// `pub` and named because two callers need the same literal for two different
/// reasons: [`bucketize_status`] returns it, and legacy's summary substitutes it
/// for a *missing* latest entry (`analytics.rs:1237-1241`,
/// `.unwrap_or("NOT_RUN")`). Tasks 21-27 make that substitution too, and a
/// hand-typed `"NOT_RUN"` in seven places is a typo that shows up as a rendered
/// zero rather than as a failure.
pub const NOT_RUN: &str = "NOT_RUN";

/// The build label a row that named no build is reported under.
///
/// Legacy substitutes it while **constructing the row** —
/// `normalize_optional(row.app_build.as_deref()).unwrap_or_else(|| "unknown".to_string())`
/// (`analytics.rs:1032-1033`; `normalize_optional` at `:2070-2075` is trim →
/// drop-empty → own) — so legacy's `ExecRow::build` is a `String` and **every**
/// fold downstream sees the label rather than the absence.
///
/// This gear carries [`ExecRow::build`](super::ExecRow::build) as an
/// `Option<String>` from the repository, for the reason that field's header
/// gives, and collapses it at the two folds that report it: [`build_latest_map`]
/// here and
/// [`latest_per_test_snapshot`](super::aggregates::latest_per_test_snapshot)
/// there. [`collapse_build`] is the shared rule, `pub` for the same reason
/// [`NOT_RUN`] is — two call sites in two modules, and a hand-typed `"unknown"`
/// is a typo that renders rather than fails.
///
/// `pub` and separate from
/// [`UNKNOWN_COMPONENT`](super::aggregates::UNKNOWN_COMPONENT) despite the equal
/// value: they are two independent legacy fallbacks (`:1033` and `:1102`) over
/// two different fields, and one constant serving both would tie a change to
/// either to the other.
pub const UNKNOWN_BUILD: &str = "unknown";

/// The alias index: every name a universe file answers to, and the file it
/// resolves to — or `None` where two files claimed the same name.
///
/// Legacy's `HashMap<String, Option<String>>` (`analytics.rs:1714`), behind a
/// newtype for two reasons. `Option<Option<String>>` is what a bare
/// `HashMap::get` on it yields, and the difference between the outer `None`
/// ("nothing registered that name") and the inner one ("two files did") is the
/// difference between a typo and a *poisoned* alias — a distinction worth a name
/// rather than a nesting level. And a `pub fn` taking a bare `HashMap` trips
/// `clippy::implicit_hasher`, which would push a `<S: BuildHasher>` parameter
/// onto every signature in this module for no caller's benefit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AliasMap(HashMap<String, Option<String>>);

impl AliasMap {
    /// Claim `alias` for `test_file`, poisoning it if another file already has it.
    ///
    /// `add_alias` (`analytics.rs:1766-1780`) verbatim, including both behaviours
    /// that look like oversights and are not:
    ///
    /// * **A blank alias is dropped, not stored** (`:1767-1769`). Without that
    ///   guard the first entry with a punctuation-only title would claim the
    ///   empty alias, the second would poison it, and every row whose
    ///   `test_name` normalizes to nothing would resolve to whichever file got
    ///   there first.
    /// * **Poisoning is permanent.** The `Some(Some(existing)) if existing ==
    ///   test_file` arm cannot match an already-poisoned `Some(None)`, so a third
    ///   claimant re-poisons rather than reclaiming. Attributing a result to one
    ///   of two candidate tests is worse than attributing it to neither: the
    ///   wrong test then renders a status it never produced.
    fn add(&mut self, alias: String, test_file: &str) {
        if alias.is_empty() {
            return;
        }

        match self.0.get(&alias) {
            None => {
                self.0.insert(alias, Some(test_file.to_owned()));
            }
            // The same file claiming the same alias twice is a no-op, which is
            // routine: legacy keys its universe on `(source, repo_id, test_file)`
            // (`:899-903`), so one file listed by two plans is two entries with
            // the same path, and the four alias sources overlap within one entry
            // as well.
            Some(Some(existing)) if existing == test_file => {}
            _ => {
                self.0.insert(alias, None);
            }
        }
    }

    /// The file `alias` resolves to: `None` if nothing registered it *or* if it
    /// was poisoned. `analytics.rs:1763`, `aliases.get(&key).cloned().flatten()`.
    fn resolve(&self, alias: &str) -> Option<String> {
        self.0.get(alias).cloned().flatten()
    }
}

/// The newest outcome for one universe file.
///
/// Legacy's `LatestInfo` (`analytics.rs:286-292`), field for field, with the two
/// renames this architecture forces: `workflow_name` — legacy's run identity — is
/// [`Self::run_id`], and `platform`, which legacy carries as a *name*, is
/// [`Self::platform_id`].
///
/// All five fields are rendered, so none is speculative: `AnalyticsListItem`'s
/// `last_status`, `last_platform`, `last_run_name`, `last_build` and
/// `last_run_finished_at` read them one for one (`:1419-1423`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatestInfo {
    /// [`bucketize_status`] of the newest row's status. A `&'static str` and not
    /// an enum, matching legacy's `bucketize_status -> &'static str` and its
    /// `status_bucket: String`, because Phase B's standing instruction is to port
    /// these folds verbatim and every consumer compares this against the same
    /// three literals (`:1094`, `:1133`, `:1239`, `:1371`, `:1433`). [`NOT_RUN`]
    /// is `pub` so a consumer can name the literal instead of typing it.
    pub status_bucket: &'static str,
    /// The platform the newest row ran on, as an id.
    ///
    /// **A carried obligation, restated here because this is where the value
    /// surfaces:** legacy's is a display *name*, and
    /// [`ExecRow::platform_id`](super::ExecRow::platform_id) records that a
    /// grouped-summaries surface keyed on this would regress to raw UUIDs unless
    /// something resolves ids to names. Task 23 shipped that surface **without**
    /// the resolution and made the gap a type instead —
    /// [`PlatformGroupSummary`](super::aggregates::PlatformGroupSummary) carries
    /// a `platform_id`, not a `value: String`, so nothing can render the id as a
    /// name by accident. Closing it needs a qa-environments port this gear does
    /// not have.
    pub platform_id: Option<Uuid>,
    /// The run the newest row came from — legacy's `workflow_name`, which is its
    /// run identity. `Option` because [`LatestInfo::default`] must express "no
    /// row", exactly as legacy's does (`:294-303`).
    pub run_id: Option<Uuid>,
    /// The build the newest row's run executed against, collapsed by
    /// [`collapse_build`] — so [`UNKNOWN_BUILD`] when the row named none, and the
    /// trimmed label otherwise.
    ///
    /// **`None` means "no row at all", and nothing else.** That is legacy
    /// field-for-field: its `LatestInfo::build` is `Some(row.build.clone())`
    /// (`analytics.rs:1203`) over a `row.build` that was **already** normalized at
    /// `:1032-1033`, and its `LatestInfo::default()` carries `build: None`
    /// (`:294-303`) — so the `Option` distinguishes "never executed" from
    /// "executed and reported no build", and never one padded label from another.
    ///
    /// **This doc said the substitution "is not applied here either" and the code
    /// matched it, and both were wrong** — controller ruling R15. The value
    /// reaches the wire as
    /// [`AnalyticsListItem::last_build`](super::aggregates::AnalyticsListItem::last_build)
    /// (`aggregates.rs`' `build_lists`, legacy `:1422`), so the port rendered
    /// `null` where legacy renders `"unknown"` and `"  9.1  "` where legacy
    /// renders `"9.1"`. Ruling R13 assigned the fallback to Task 24 and legacy
    /// applies it universally at row construction; collapsing at one of the two
    /// consumers did not discharge that.
    pub build: Option<String>,
    /// The newest row's effective instant — `run_finished_at ?? run_created_at`,
    /// already coalesced by the repository into
    /// [`ExecRow::ts`](super::ExecRow::ts). Named `finished_at` because that is
    /// what the response field is called (`last_run_finished_at`, `:1423`); it is
    /// not a claim that the run finished.
    pub finished_at: Option<OffsetDateTime>,
}

impl Default for LatestInfo {
    /// [`NOT_RUN`] and nothing else, which is legacy's
    /// `impl Default for LatestInfo` (`analytics.rs:294-303`).
    ///
    /// Used where a consumer wants a total function over the universe rather
    /// than a lookup that can miss — legacy's lists do exactly that at `:1407`,
    /// `latest.get(..).cloned().unwrap_or_default()`.
    fn default() -> Self {
        Self {
            status_bucket: NOT_RUN,
            platform_id: None,
            run_id: None,
            build: None,
            finished_at: None,
        }
    }
}

/// Fold a name into the form the alias map is keyed on.
///
/// `normalize_alias` (`analytics.rs:1782-1792`) verbatim: trim, lowercase,
/// replace every non-`ASCII`-alphanumeric character with a space, then collapse
/// runs of whitespace by splitting and re-joining with single spaces.
///
/// This is the function that lets `Cluster Upgrade`, `cluster_upgrade` and
/// `cluster-upgrade` name one file. Two consequences are worth stating because
/// they look wrong and are load-bearing:
///
/// * A **non-ASCII letter is punctuation** to `is_ascii_alphanumeric`, so
///   `Upgrade Über` normalizes to `upgrade ber`. Both sides of every comparison
///   go through this function, so the universe and the row are mangled
///   identically and still meet.
/// * A punctuation-only name normalizes to the **empty string**, which
///   [`AliasMap::add`] refuses to register — see there for why that matters.
///
/// The leading `trim` is redundant given the `split_whitespace`, and is kept
/// because it is in the original.
#[must_use]
pub fn normalize_alias(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fold a stored path into the form the universe is keyed on.
///
/// `normalize_test_path` (`analytics.rs:1965-1970`) verbatim: trim, strip every
/// leading `./`, strip every leading `/`, then turn backslashes into forward
/// slashes. `trim_start_matches` strips *repeated* prefixes, so `././tests/a.py`
/// and `//tests/a.py` both land on `tests/a.py`.
///
/// # The separator conversion happens last, and that is a bug this port keeps
///
/// A Windows-style `.\tests\a.py` has no `./` prefix to strip at the time the
/// stripping runs, so it survives as `./tests/a.py` — a path no universe
/// contains, which means the row is dropped and its test renders as `not_run`.
/// Ported verbatim under Phase B's standing instruction ("port them verbatim,
/// including behavior you would call wrong"), pinned by
/// `a_windows_style_prefix_survives_normalization`, and raised separately rather
/// than fixed here: changing it changes numbers that are already on screen.
#[must_use]
pub fn normalize_test_path(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .replace('\\', "/")
}

/// Index every name each universe file answers to.
///
/// `build_alias_map` (`analytics.rs:1714-1748`): **four** aliases per entry, in
/// this order, each through [`normalize_alias`] and each subject to
/// [`AliasMap::add`]'s poisoning rule.
///
/// 1. the `test_file` path,
/// 2. the path's **file stem** — `tests/cluster/test_upgrade.py` also answers to
///    `test upgrade`,
/// 3. the `test_name`, which is the display name,
/// 4. the `TEST_META` `title_alias`, when the file declares one.
///
/// The fourth is the one an implementation drops without noticing:
/// `qa_catalog_sdk::UniverseTest::title_alias`'s own doc says so — *"Omitting
/// this field silently turns those rows into `not_run`"* — because a runner that
/// reports a test by its human title has no other way in.
///
/// Order does not matter to the result: same-file duplicates are no-ops and
/// cross-file collisions poison symmetrically.
#[must_use]
pub fn build_alias_map(universe: &[UniverseTest]) -> AliasMap {
    let mut aliases = AliasMap::default();

    for test in universe {
        aliases.add(normalize_alias(&test.test_file), &test.test_file);

        // The stem is taken from the path as stored, exactly as legacy takes it
        // (`:1724-1730`) — `UniverseTest::test_file` is already
        // `normalize_test_path`-normalized on qa-catalog's side, which that
        // field's doc states.
        if let Some(stem) = Path::new(test.test_file.as_str())
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
        {
            aliases.add(normalize_alias(stem), &test.test_file);
        }

        aliases.add(normalize_alias(&test.test_name), &test.test_file);

        if let Some(title) = test.title_alias.as_deref() {
            aliases.add(normalize_alias(title), &test.test_file);
        }
    }

    aliases
}

/// The universe file one executed row belongs to, or `None`.
///
/// `resolve_row_test_file` (`analytics.rs:1750-1764`), and the order of the two
/// branches is the whole behaviour:
///
/// * **An explicit non-blank `test_file` wins outright** and is only
///   [`normalize_test_path`]-ed. The alias map is never consulted for it, so a
///   row that names its own path is unaffected by any ambiguity in the universe.
/// * Otherwise the row is looked up by its `test_name` through the alias map.
///
/// "Blank" is legacy's `normalize_optional` (`:2070-2075`): `None`, `""` and
/// `"   "` are all absent. That is not a hypothetical input —
/// `qa_test_results.test_file` is `NOT NULL DEFAULT ''`, so `""` is what a row
/// whose producer reported no path actually stores.
///
/// Note what this function does **not** do: it does not check that the resolved
/// path is in the universe. An explicit path resolves to itself whether the
/// universe holds it or not, which is why [`resolve_rows`] applies the membership
/// filter separately, exactly as legacy does at `:1022`.
#[must_use]
pub fn resolve_row_test_file(
    test_file: Option<&str>,
    test_name: &str,
    aliases: &AliasMap,
) -> Option<String> {
    if let Some(value) = normalize_optional(test_file) {
        let normalized = normalize_test_path(value);
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }

    aliases.resolve(&normalize_alias(test_name))
}

/// Attribute every row to a universe file, dropping the ones that cannot be.
///
/// `load_universe_and_rows`' post-query loop (`analytics.rs:1018-1040`): resolve,
/// keep only what the universe holds, and rewrite the row's `test_file` to the
/// resolved value so every later fold keys on one spelling.
///
/// **Order in is order out.** No sort, deliberately — this module's header and
/// [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe)
/// both say why the repository's four-key ordering cannot be reconstructed from
/// an [`ExecRow`].
///
/// Takes the rows by value because it rewrites one field of each and returns the
/// survivors; borrowing would mean cloning every row that lives.
#[must_use]
pub fn resolve_rows(universe: &[UniverseTest], rows: Vec<ExecRow>) -> Vec<ExecRow> {
    let aliases = build_alias_map(universe);
    let universe_files = file_set(universe);

    rows.into_iter()
        .filter_map(|mut row| {
            let resolved = resolve_row_test_file(
                Some(row.test_file.as_str()),
                row.test_name.as_str(),
                &aliases,
            )
            .filter(|file| universe_files.contains(file.as_str()))?;
            row.test_file = resolved;
            Some(row)
        })
        .collect()
}

/// Collapse a raw status into the three buckets the universe renders.
///
/// `bucketize_status` (`analytics.rs:1940-1946`) verbatim: `PASSED` stays,
/// `FAILED` and `ERROR` become `FAILED`, and **everything else** — `SKIPPED`,
/// `XFAIL`, `XPASS`, `RUNNING`, a lowercase `passed`, an empty string — becomes
/// [`NOT_RUN`].
///
/// # This is not [`classify`](crate::domain::service::ingest::classify), and the
/// # difference is deliberate
///
/// `domain::service::ingest`'s header tabulates the **seven** status
/// classifications legacy contains, which disagree with each other on purpose.
/// (It said five until Task 21b's Step 0 found the sixth — the dashboard's
/// `PASSED`+`FAILED`+`ERROR` denominator — and six until Task 24's found the
/// seventh, `latest_per_test_snapshot`'s mapping at `analytics.rs:1633-1639`.
/// **The count is restated in five places and no tool checks any of them**:
/// `domain::service::ingest`'s table header, `analytics::aggregates`' header,
/// here, `domain::service::dashboard`'s header, and `analytics::universe_tests`'
/// doc on the test below. Task 22 fixed three of the five and its own first pass
/// missed two of those; **Task 24 fixed two of the five and its first pass missed
/// three**, for the same reason both times — grepping for a phrase rather than
/// for the number near the concept. The three it missed spell it "six **status**
/// classifications", across a line wrap. Grep for `\bsix\b` within a few lines
/// of `classif`, not for a sentence, and expect a wrap between the number and the
/// noun.)
/// This is the third row of that table and
/// [`classify`](crate::domain::service::ingest::classify) is the first. Under
/// this fold a `SKIPPED` file is [`NOT_RUN`]; under `classify` it is
/// `StatusBucket::Skipped`. Both are correct for their own surface, and reusing
/// `classify` here would move every skipped test out of `NOT_RUN` and change a
/// number the UI already renders. `only_the_uppercase_spellings_bucket_and_skipped_is_not_run`
/// asserts both halves so a later unification is a failing test.
///
/// **Five folds read this rule, not one.** The summary and the three lists (Task
/// 21a); the heatmap and the trend as of Task 22 — where the consequence is
/// most visible, because a `SKIPPED` cell is rendered in the same colour as a day
/// on which nothing ran at all; and
/// [`build_grouped_summaries`](super::aggregates::build_grouped_summaries) as of
/// Task 23, which reads it through two latest maps of its own and then matches
/// this fold's three output words a third time, in `GroupCounters::add`.
/// `aggregates_tests`' `skipped_buckets_as_not_run`
/// asserts the rule at the two chart folds rather than at this function, which is
/// where reaching for `classify` would actually happen, and
/// `component_groups_partition_the_universe_and_blanks_group_under_unknown`
/// carries a `SKIPPED` row for the fifth.
///
/// The uppercase-only matching is verbatim too: legacy matches the runner's
/// spelling and nothing else, so a producer that emitted `passed` would have
/// every one of its results read as not-run. Pinned rather than "fixed" for the
/// same reason.
#[must_use]
pub fn bucketize_status(status: &str) -> &'static str {
    match status {
        "PASSED" => PASSED,
        "FAILED" | "ERROR" => FAILED,
        _ => NOT_RUN,
    }
}

/// [`UNKNOWN_BUILD`] for an absent, empty or whitespace-only build; the trimmed
/// label otherwise.
///
/// `normalize_optional(..).unwrap_or_else(|| "unknown".to_string())`
/// (`analytics.rs:1032-1033`, `:2070-2075`), moved from legacy's row constructor
/// to the two folds that report the value. See [`UNKNOWN_BUILD`] for why it moved
/// and what it costs.
///
/// **Both halves are load-bearing and both were missing here once.** The trim is
/// not cosmetic — legacy's list item renders `"9.1"` where an untrimmed port
/// renders `"  9.1  "` — and the substitution is not the aggregate's private
/// business, because legacy applies it before *any* fold sees the row.
/// `the_latest_build_is_collapsed_exactly_as_legacy_collapses_it` pins the pair
/// at [`build_latest_map`] and
/// `the_build_distribution_collapses_absent_blank_and_literal_unknown_builds`
/// at the other call site.
#[must_use]
pub fn collapse_build(build: Option<&str>) -> String {
    build
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(UNKNOWN_BUILD)
        .to_owned()
}

/// The newest outcome per universe file.
///
/// `build_latest_map` (`analytics.rs:1182-1210`). Two properties, both of them
/// legacy's and both easy to lose:
///
/// * **The first row seen per file wins**, and nothing here compares timestamps
///   (`:1193-1195`, `if latest.contains_key(..) { continue }`). The ordering is
///   the query's responsibility — this port's is
///   [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe),
///   which is contractually newest-first with a four-key tiebreak. Hand this
///   function rows in any other order and it answers a different question
///   without failing.
/// * **A row whose file is not in the universe is skipped** (`:1190-1192`), so a
///   test deleted from the plans stops appearing the moment the universe stops
///   listing it, however much history it has.
///
/// Expects [`resolve_rows`]' output. The membership check is re-applied rather
/// than assumed — see this module's header.
#[must_use]
pub fn build_latest_map(
    universe: &[UniverseTest],
    rows: &[ExecRow],
) -> HashMap<String, LatestInfo> {
    let universe_files = file_set(universe);
    let mut latest: HashMap<String, LatestInfo> = HashMap::new();

    for row in rows {
        if !universe_files.contains(row.test_file.as_str()) {
            continue;
        }

        // `or_insert_with` **is** legacy's first-wins guard, spelled as one
        // lookup instead of a `contains_key` followed by an `insert`
        // (`clippy::map_entry`): the closure does not run when the key is
        // already there.
        latest
            .entry(row.test_file.clone())
            .or_insert_with(|| LatestInfo {
                status_bucket: bucketize_status(row.status.as_str()),
                platform_id: row.platform_id,
                run_id: Some(row.run_id),
                build: Some(collapse_build(row.build.as_deref())),
                finished_at: Some(row.ts),
            });
    }

    latest
}

/// The "expected cases" total — legacy's per-file precedence between the
/// collect job's exact count and the static estimate parsed from source.
///
/// `analytics.rs:769-779`, in the handler rather than in `build_summary`
/// (that fold's own header — `aggregates.rs`' — says why): after loading
/// `filtered_universe`, legacy sums, per file,
/// `collect_counts.get(&(repo_id, test_file)).map(|c| c.max(0) as
/// usize).unwrap_or(t.case_count)` (`:770-779`). This gear's collect count is
/// `u32` (`CollectCount::case_count`), not legacy's `i32` column, so there is
/// no negative value the `.max(0)` guards against and no analogue of it here.
///
/// # Why exact wins — legacy's own comment at `:767-768`
///
/// [`UniverseTest::static_case_count`] is a regex count of `def test_*`
/// functions (and Playwright `test(...)` calls) over the file's source
/// (`analytics.rs:1860-1873`, run on qa-catalog's side of this port). It does
/// **not** expand `@pytest.mark.parametrize` or any other test-generation
/// decorator, so a file with one parametrized test and five generated cases
/// reports `1`. The collect job runs the real collector — the same tool the
/// suite itself would use to enumerate what it is about to run — and reports
/// the expanded count, so wherever a collect result exists for a file it is
/// the truthful number and the static estimate is only ever a lower bound.
/// [`UniverseTest::static_case_count`]'s own doc states the same precedence
/// from the catalog side.
///
/// # The set mismatch, both directions (task-29 brief hazard 3)
///
/// This iterates the **universe**, never `collect_counts`, exactly as
/// legacy's `filtered_universe.iter().map(...)` does (`:770-779`) rather than
/// looping the collect map and topping up from the universe:
///
/// * A universe file absent from `collect_counts` falls back to its own
///   [`UniverseTest::static_case_count`] — the ordinary case on any
///   deployment that has never run a collect, which is why this fold is
///   non-zero even then.
/// * A `collect_counts` entry naming a `(repo_id, test_file)` no universe
///   file has — a file removed from every plan, or one this request's group
///   filter or plan scope excluded from `filtered_universe` while its
///   repository stayed in scope for other files — is loaded by the caller
///   and never visited here, so it contributes **nothing**. This gear's own
///   read already narrows further than legacy's before this fold ever runs:
///   [`CollectRepository::list_counts_for`](crate::domain::repos::CollectRepository::list_counts_for)
///   scopes to the universe's own `repo_id`s and to one branch, where
///   legacy's `load_collect_counts` loads every repository's rows for a
///   branch (`analytics.rs:2672-2693`) and only the ones a universe file
///   names are ever looked up. Neither narrowing reaches the level of one
///   test file, so this per-file guard still matters on this gear's narrower
///   read too.
///
/// # Not windowed
///
/// `qa_test_case_collect` is a snapshot table — one row per `(repo_id,
/// branch, test_file)`, replaced on every recollect rather than accumulated
/// (`CollectRepository::upsert_count`'s doc) — so there is no time dimension
/// here for ruling R21's read window to bound, unlike
/// [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe).
/// `domain::service::analytics::AnalyticsService`'s collect read states the
/// same thing at the call site that supplies `collect_counts`.
#[must_use]
pub fn expected_cases(universe: &[UniverseTest], collect_counts: &[CollectCount]) -> usize {
    let exact: HashMap<(Uuid, &str), u32> = collect_counts
        .iter()
        .map(|count| ((count.repo_id, count.test_file.as_str()), count.case_count))
        .collect();

    universe
        .iter()
        .map(|test| {
            exact
                .get(&(test.repo_id, test.test_file.as_str()))
                .copied()
                .unwrap_or(test.static_case_count) as usize
        })
        .sum()
}

/// The universe's file paths, for membership tests.
///
/// Borrowed rather than owned: both callers hold the universe for their whole
/// body, and legacy builds the same set per call (`:950-951`, `:1184-1187`).
fn file_set(universe: &[UniverseTest]) -> HashSet<&str> {
    universe
        .iter()
        .map(|test| test.test_file.as_str())
        .collect()
}

/// Legacy's `normalize_optional` (`analytics.rs:2070-2075`): trim, and treat the
/// empty result as absent.
fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
#[path = "universe_tests.rs"]
mod universe_tests;
