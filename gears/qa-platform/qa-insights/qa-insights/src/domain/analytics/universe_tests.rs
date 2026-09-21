//! Tests for the analytics universe core.
//!
//! Every test here drives a pure function directly. There is no service, no
//! repository and no `async` in this module, because there is none in the core:
//! the universe arrives over [`CatalogReader`](crate::domain::ports::CatalogReader)
//! and the rows over
//! [`ResultsRepository::list_for_universe`](crate::domain::repos::ResultsRepository::list_for_universe),
//! and both are somebody else's tier.
//!
//! # The brief's six tests, and where each one is
//!
//! Task 20's Step 1 gives six tests as pseudo-code. Five are here with their
//! assertions unchanged —
//! [`alias_normalization_collapses_punctuation_and_case`],
//! [`a_row_with_only_a_test_name_resolves_through_any_of_the_four_aliases`],
//! [`an_ambiguous_alias_resolves_to_nothing`],
//! [`an_explicit_test_file_bypasses_the_alias_map`] and
//! [`the_latest_status_is_the_first_row_and_rows_outside_the_universe_are_ignored`].
//! One fixture signature changed: `exec_row_at` takes an `OffsetDateTime` built
//! with `time::macros::datetime!` rather than an RFC-3339 `&str`, because
//! `time`'s parser is behind a feature this crate does not enable and every other
//! test module in this gear already builds instants that way.
//!
//! **The sixth, `an_absent_branch_filter_matches_every_row`, is not here, and
//! that is a finding rather than an omission.** It is written against
//! `filter_by_branch(&rows, Some("main"))`, and there is no branch on a row to
//! filter: [`ExecRow`](crate::domain::analytics::ExecRow) carries none,
//! deliberately — its header records that legacy's branch is a *predicate*
//! (`analytics.rs:967-970`), and `infra::storage::mapper`'s
//! `exec_row_from_result` drops the stored column for that reason. The rule the
//! test pins is real and it lives one tier down, in the repository's conditional
//! `WHERE`: `infra::storage::results_sea_repo`'s
//! `an_absent_branch_filter_matches_every_row` is that test, added by this task
//! beside the existing `the_universe_filter_applies_version_branch_and_plan`,
//! which had only ever pinned the *present*-branch half. Writing it here instead
//! would have meant adding a row field nothing reads and a filter nothing calls.
//!
//! # What is here beyond the brief
//!
//! One test per further rule Step 0 turned up, and two of them exist because the
//! rule looks like a defect: [`normalize_test_path`](super::normalize_test_path)
//! leaves a Windows-style `.\` prefix in place
//! ([`a_windows_style_prefix_survives_normalization`]) and
//! [`bucketize_status`](super::bucketize_status) sends a lowercase `passed` to
//! `NOT_RUN` ([`only_the_uppercase_spellings_bucket_and_skipped_is_not_run`]).
//! Both are ported verbatim under the phase's standing instruction, and both are
//! pinned here so a later "fix" is a failing test rather than a silent change to
//! a rendered number.

#![allow(clippy::unwrap_used)]

use time::macros::datetime;
use uuid::Uuid;

use super::{
    LatestInfo, NOT_RUN, UNKNOWN_BUILD, bucketize_status, build_alias_map, build_latest_map,
    collapse_build, normalize_alias, normalize_test_path, resolve_row_test_file, resolve_rows,
};
use crate::domain::analytics::ExecRow;
use crate::domain::ports::CatalogReader;
use crate::domain::service::ingest::{StatusBucket, classify};
use crate::domain::service::test_support::{
    DEFAULT_BRANCH, FakeCatalog, UNIVERSE_TEST_REPO_ID, ctx, exec_row_at, universe_test,
    universe_test_full,
};

/// A row whose producer reported no file at all — the case the whole alias map
/// exists for. `qa_test_results.test_file` is `NOT NULL DEFAULT ''`, so this is
/// what is actually stored, not a contrived input.
fn row_named_only(test_name: &str, status: &str) -> ExecRow {
    ExecRow {
        test_file: String::new(),
        test_name: test_name.to_owned(),
        ..exec_row_at("", status, datetime!(2026-08-18 12:00:00 UTC))
    }
}

// ---------------------------------------------------------------------------
// Alias normalization
// ---------------------------------------------------------------------------

/// `normalize_alias:1782`: trim, lowercase, replace every non-alphanumeric
/// character with a space, collapse runs of whitespace, join with single
/// spaces. This is what lets `Cluster Upgrade`, `cluster_upgrade` and
/// `cluster-upgrade` all resolve to one file.
#[test]
fn alias_normalization_collapses_punctuation_and_case() {
    assert_eq!(
        normalize_alias("  Cluster_Upgrade--Test "),
        "cluster upgrade test"
    );
    assert_eq!(normalize_alias("test_a.py"), "test a py");
    assert_eq!(normalize_alias("!!!"), "");
}

/// `normalize_alias:1787` maps every non-**ASCII**-alphanumeric character to a
/// space, so a non-ASCII letter is punctuation to it.
///
/// Verbatim from `char::is_ascii_alphanumeric`, and stated as a rule rather than
/// left to be discovered: a universe entry titled `Upgrade Uber` with an umlaut
/// registers the alias `upgrade ber`, and a row naming the same title resolves
/// through the same mangling — so the two agree, which is the only property that
/// matters.
///
/// The umlauts are written as `\u{dc}`/`\u{fc}` because the workspace denies
/// `clippy::non_ascii_literal`; they are a capital and a lowercase U-umlaut.
#[test]
fn a_non_ascii_letter_normalizes_to_a_space() {
    assert_eq!(normalize_alias("Upgrade \u{dc}ber"), "upgrade ber");

    let universe = vec![universe_test_full(
        "tests/upgrade.py",
        "test_upgrade",
        Some("Upgrade \u{dc}ber"),
    )];
    let aliases = build_alias_map(&universe);
    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "upgrade \u{fc}ber", &aliases)
            .as_deref(),
        Some("tests/upgrade.py"),
        "the row and the universe are mangled identically, so they still meet",
    );
}

// ---------------------------------------------------------------------------
// Path normalization
// ---------------------------------------------------------------------------

/// `normalize_test_path:1965`: trim, strip **every** leading `./`, strip every
/// leading `/`, then turn backslashes into forward slashes.
#[test]
fn path_normalization_strips_leading_dot_slashes_and_flips_separators() {
    assert_eq!(normalize_test_path("  ./tests/a.py "), "tests/a.py");
    assert_eq!(normalize_test_path("././tests/a.py"), "tests/a.py");
    assert_eq!(normalize_test_path("//tests/a.py"), "tests/a.py");
    assert_eq!(
        normalize_test_path("tests\\cluster\\a.py"),
        "tests/cluster/a.py"
    );
}

/// `normalize_test_path:1965-1970` strips the `./` prefix **before** it converts
/// separators, so a Windows-style `.\` prefix survives as `./`.
///
/// Ported verbatim under Phase B's standing instruction. The consequence is real
/// and is the reason it is pinned: such a row normalizes to `./tests/a.py`,
/// which is in no universe, so it is dropped and its test shows as `not_run`.
/// Fixing it here would change a rendered number without a decision to do so.
#[test]
fn a_windows_style_prefix_survives_normalization() {
    assert_eq!(normalize_test_path(".\\tests\\a.py"), "./tests/a.py");

    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![ExecRow {
        test_file: ".\\tests\\a.py".to_owned(),
        ..exec_row_at("", "PASSED", datetime!(2026-08-18 12:00:00 UTC))
    }];
    assert!(
        resolve_rows(&universe, rows).is_empty(),
        "the surviving `./` prefix is what drops the row",
    );
}

// ---------------------------------------------------------------------------
// The alias map
// ---------------------------------------------------------------------------

/// `build_alias_map:1714` registers four aliases per universe entry: the
/// normalized path, the normalized file **stem**, the normalized test name, and
/// the normalized `TEST_META` title. A row carrying only a test name resolves
/// through any of them (`resolve_row_test_file:1762-1763`).
///
/// # The display name is `"upgrade"`, not `"test_upgrade"`, and that is load-bearing
///
/// Task 20's brief writes this fixture with `test_name = "test_upgrade"`, which
/// normalizes to `"test upgrade"` — **the same key the file stem already
/// produces**. Under that fixture the stem source and the name source are
/// indistinguishable, and deleting either one from `build_alias_map` leaves the
/// whole suite green. Found by review, re-measured here.
///
/// A real universe entry does not look like that. `qa_catalog_sdk::UniverseTest::test_name`
/// is the `TEST_META` title when there is one and otherwise legacy's
/// `fallback_test_name` (`analytics.rs:1805-1811`: take the file stem, strip a
/// leading `test_`, turn `_` into a space), so a file at
/// `tests/cluster/test_upgrade.py` with no title has display name `"upgrade"`
/// while its stem alias is `"test upgrade"`. Two distinct keys, and a row may
/// arrive naming either. The fixture now uses that shape, so each of the four
/// sources has an assertion only it can satisfy:
///
/// * `"upgrade"` → the **`test_name`** source and nothing else,
/// * `"test upgrade"` / `"test_upgrade"` → the **stem** source,
/// * `"Cluster Upgrade"` → the **title** source,
/// * the full path → the **path** source.
#[test]
fn a_row_with_only_a_test_name_resolves_through_any_of_the_four_aliases() {
    let universe = vec![universe_test_full(
        "tests/cluster/test_upgrade.py",
        // `fallback_test_name("tests/cluster/test_upgrade.py")`.
        "upgrade",
        Some("Cluster Upgrade"),
    )];
    let aliases = build_alias_map(&universe);

    for name in [
        "upgrade",
        "test_upgrade",
        "test upgrade",
        "Cluster Upgrade",
        "tests/cluster/test_upgrade.py",
    ] {
        assert_eq!(
            resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, name, &aliases).as_deref(),
            Some("tests/cluster/test_upgrade.py"),
            "alias {name} must resolve"
        );
    }
}

/// `add_alias:1766`: an alias claimed by two **different** files is poisoned to
/// `None` and resolves to nothing thereafter. Guessing one of the two would
/// attribute a result to the wrong test, which is worse than not attributing it.
#[test]
fn an_ambiguous_alias_resolves_to_nothing() {
    let universe = vec![
        universe_test_full("tests/a/test_smoke.py", "test_smoke", None),
        universe_test_full("tests/b/test_smoke.py", "test_smoke", None),
    ];
    let aliases = build_alias_map(&universe);
    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "test_smoke", &aliases),
        None
    );
}

/// `add_alias:1771-1779`: poisoning is **permanent** and does not spread.
///
/// The three-entry case is the one that could plausibly have been written the
/// other way: once `test_smoke` is ambiguous, a *third* entry claiming it does
/// not reclaim it (`Some(Some(existing)) if existing == test_file` cannot match a
/// poisoned `None`, so the `_` arm re-poisons), while every alias that is still
/// unique keeps resolving.
#[test]
fn poisoning_is_permanent_and_confined_to_the_ambiguous_alias() {
    let universe = vec![
        universe_test_full("tests/a/test_smoke.py", "test_smoke", None),
        universe_test_full("tests/b/test_smoke.py", "test_smoke", None),
        universe_test_full("tests/c/test_smoke.py", "test_smoke", None),
    ];
    let aliases = build_alias_map(&universe);

    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "test_smoke", &aliases),
        None
    );
    assert_eq!(
        resolve_row_test_file(
            UNIVERSE_TEST_REPO_ID,
            None,
            "tests/b/test_smoke.py",
            &aliases
        )
        .as_deref(),
        Some("tests/b/test_smoke.py"),
        "the path alias of each entry is still unique, so it still resolves",
    );
}

/// `add_alias:1775`: the same alias registered twice for the **same** file is a
/// no-op, not an ambiguity.
///
/// Reachable rather than theoretical: legacy keys its universe on
/// `(source, repo_id, test_file)` (`analytics.rs:899-903`), so one file listed by
/// two plans in two repositories is two entries with the same path — and the four
/// alias sources overlap within a single entry too (a file named `test_upgrade.py`
/// whose display name is `test_upgrade` registers the same stem twice).
#[test]
fn the_same_alias_for_the_same_file_is_not_an_ambiguity() {
    let universe = vec![
        universe_test_full(
            "tests/test_upgrade.py",
            "test_upgrade",
            Some("test upgrade"),
        ),
        universe_test_full("tests/test_upgrade.py", "test_upgrade", None),
    ];
    let aliases = build_alias_map(&universe);
    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "test_upgrade", &aliases).as_deref(),
        Some("tests/test_upgrade.py"),
    );
}

/// `add_alias:1767-1769`: an alias that normalizes to the empty string is never
/// registered, so it can neither resolve nor poison.
///
/// Without the guard this entry's punctuation title claims the empty alias, and
/// **every** row whose `test_name` is punctuation — or empty, which is what a
/// producer that reported no name at all stores — resolves to this file.
///
/// # The universe here is deliberately one entry
///
/// This test held two entries with blank titles until Task 20's fix round, and in
/// that shape it discriminated nothing: with the guard removed the first entry
/// claims `""` and the second poisons it back to `None`, so all three assertions
/// passed either way. Measured both ways by the reviewer, then re-measured here.
/// One entry is what makes the guard's absence visible.
#[test]
fn a_blank_alias_is_never_registered() {
    let universe = vec![universe_test_full("tests/a.py", "a", Some("!!!"))];
    let aliases = build_alias_map(&universe);

    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "***", &aliases),
        None
    );
    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "", &aliases),
        None
    );
    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "a", &aliases).as_deref(),
        Some("tests/a.py"),
        "the blank title must not have disturbed the entry's other aliases",
    );
}

/// `resolve_row_test_file:1755-1760`: an explicit non-empty `test_file` wins
/// outright and is only normalized — the alias map is never consulted for it.
///
/// # The alias map deliberately *conflicts* with the explicit path
///
/// This test used an empty universe until Task 20's fix round, which made it
/// vacuous: with no aliases registered, an implementation that consulted the map
/// **first** and fell back to the explicit path passed it too. The universe now
/// registers `irrelevant` against a different file, so the two candidate answers
/// are distinguishable and only the documented precedence gives `tests/a.py`.
///
/// Getting the precedence backwards is a misattribution, not a miss: the row
/// named its own file and would be counted against somebody else's.
#[test]
fn an_explicit_test_file_bypasses_the_alias_map() {
    let universe = vec![universe_test_full("tests/b.py", "irrelevant", None)];
    let aliases = build_alias_map(&universe);

    // The alias resolves — to the *other* file.
    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "irrelevant", &aliases).as_deref(),
        Some("tests/b.py"),
    );
    assert_eq!(
        resolve_row_test_file(
            UNIVERSE_TEST_REPO_ID,
            Some("./tests/a.py"),
            "irrelevant",
            &aliases
        )
        .as_deref(),
        Some("tests/a.py"),
    );
}

/// `resolve_row_test_file:1755` treats a blank `test_file` as absent, which is
/// the only reason [`ExecRow::test_name`] is carried at all.
///
/// `normalize_optional` (`analytics.rs:2070-2075`) maps `""` and `"   "` to
/// `None`, so the alias map is consulted for both.
#[test]
fn a_blank_test_file_falls_through_to_the_alias_map() {
    let universe = vec![universe_test_full(
        "tests/cluster/test_upgrade.py",
        "test_upgrade",
        None,
    )];
    let aliases = build_alias_map(&universe);

    for stored in ["", "   "] {
        assert_eq!(
            resolve_row_test_file(
                UNIVERSE_TEST_REPO_ID,
                Some(stored),
                "test_upgrade",
                &aliases
            )
            .as_deref(),
            Some("tests/cluster/test_upgrade.py"),
            "a stored {stored:?} is absent, not a path",
        );
    }
}

// ---------------------------------------------------------------------------
// The row join
// ---------------------------------------------------------------------------

/// `load_universe_and_rows:1018-1040`: a row is rewritten to its resolved file
/// and dropped unless that file is in the universe.
#[test]
fn resolution_rewrites_the_row_and_drops_what_the_universe_does_not_hold() {
    let universe = vec![universe_test_full(
        "tests/cluster/test_upgrade.py",
        "test_upgrade",
        Some("Cluster Upgrade"),
    )];
    let rows = vec![
        row_named_only("Cluster Upgrade", "PASSED"),
        row_named_only("test_nowhere", "FAILED"),
        ExecRow {
            test_file: "tests/deleted.py".to_owned(),
            ..exec_row_at(
                "tests/deleted.py",
                "PASSED",
                datetime!(2026-08-18 12:00:00 UTC),
            )
        },
    ];

    let resolved = resolve_rows(&universe, rows);
    assert_eq!(
        resolved
            .iter()
            .map(|row| row.test_file.as_str())
            .collect::<Vec<_>>(),
        vec!["tests/cluster/test_upgrade.py"],
        "the title-aliased row is attributed; the unresolvable and the \
         out-of-universe rows are dropped",
    );
}

/// The resolved order is the order the repository returned, unchanged.
///
/// Legacy re-sorts by `ts` descending at `analytics.rs:1042` and gets away with
/// it because `sort_by` is stable and its SQL already ordered on
/// `COALESCE(finished_at, created_at) DESC, t.id DESC`. This port must **not**
/// re-sort: `ResultsRepository::list_for_universe`'s four-key tiebreak
/// (`created_at DESC, ingest_ordinal DESC, id DESC`) is not reconstructible from
/// an [`ExecRow`], which carries no id and no ordinal, so a re-sort could only
/// lose it. That contract is stated on the repository method; this is the test
/// that fails if the core sorts anyway.
#[test]
fn resolution_preserves_the_repositorys_order() {
    let universe = vec![universe_test("tests/a.py"), universe_test("tests/b.py")];
    // Deliberately *not* newest-first: the core must not know or care.
    let rows = vec![
        exec_row_at("tests/a.py", "PASSED", datetime!(2026-08-18 09:00:00 UTC)),
        exec_row_at("tests/b.py", "FAILED", datetime!(2026-08-18 12:00:00 UTC)),
        exec_row_at("tests/a.py", "FAILED", datetime!(2026-08-18 11:00:00 UTC)),
    ];

    let resolved = resolve_rows(&universe, rows);
    assert_eq!(
        resolved.iter().map(|row| row.ts).collect::<Vec<_>>(),
        vec![
            datetime!(2026-08-18 09:00:00 UTC),
            datetime!(2026-08-18 12:00:00 UTC),
            datetime!(2026-08-18 11:00:00 UTC),
        ],
    );
}

// ---------------------------------------------------------------------------
// The status fold
// ---------------------------------------------------------------------------

/// `bucketize_status:1940-1946`: `PASSED`, `FAILED`/`ERROR`, and **everything
/// else** — including `SKIPPED` — is `NOT_RUN`.
///
/// The assertion against
/// [`classify`](crate::domain::service::ingest::classify) is the point of this
/// test. Legacy has seven status classifications that disagree with each other on
/// purpose (`domain::service::ingest`'s header tabulates all seven — five until
/// Task 21b found the KPI denominator, six until Task 24 found
/// `latest_per_test_snapshot`'s mapping, and this line said five until Task 22's
/// fix round and six until Task 24's), and this is
/// the pair that disagrees most damagingly: reusing `classify` here would move
/// every skipped test out of `NOT_RUN` and change a number the UI already
/// renders. A lowercase `passed` buckets to `NOT_RUN` for the same verbatim
/// reason — legacy matches the runner's uppercase spelling and nothing else.
#[test]
fn only_the_uppercase_spellings_bucket_and_skipped_is_not_run() {
    assert_eq!(bucketize_status("PASSED"), "PASSED");
    assert_eq!(bucketize_status("FAILED"), "FAILED");
    assert_eq!(bucketize_status("ERROR"), "FAILED");
    assert_eq!(bucketize_status("SKIPPED"), NOT_RUN);
    assert_eq!(bucketize_status("XFAIL"), NOT_RUN);
    assert_eq!(bucketize_status("passed"), NOT_RUN);
    assert_eq!(bucketize_status(""), NOT_RUN);

    assert_eq!(
        classify("SKIPPED"),
        StatusBucket::Skipped,
        "the two folds disagree deliberately; if this ever matches NOT_RUN's \
         shape, one of them was unified by mistake",
    );
}

/// `build_latest_map:1194`: the **first** row per file wins, and rows outside
/// the universe are skipped entirely (`:1190`). The ordering is the query's
/// responsibility, so the repository must return newest-first.
///
/// # The fixture is deliberately *not* in timestamp order, and that is the point
///
/// Task 20's brief writes this test with the `FAILED` row first **and** newest,
/// which cannot tell first-wins from newest-wins: a `max(ts)` fold answers
/// `FAILED` too. The rows are therefore inverted here — the older `PASSED` row
/// comes first — so the assertion `PASSED` holds only for a fold that takes the
/// first row it sees and never looks at [`ExecRow::ts`]. That is the exact
/// property the brief singles out (`build_latest_map` "does not compare
/// timestamps itself"), and it is not academic: `ResultsRepository::list_for_universe`'s
/// own contract says rows sharing a timestamp are "exactly what ingesting a
/// single run produces", so on a tie the repository's four-key order is the only
/// thing that decides, and a fold that re-derived "latest" from `ts` would
/// silently pick a different winner.
///
/// The out-of-universe row keeps the newest timestamp of the three, so it would
/// win any ordering — which is what makes its absence from the map a statement
/// about the membership skip rather than about the ordering.
#[test]
fn the_latest_status_is_the_first_row_and_rows_outside_the_universe_are_ignored() {
    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![
        exec_row_at("tests/a.py", "PASSED", datetime!(2026-08-18 09:00:00 UTC)),
        exec_row_at("tests/a.py", "FAILED", datetime!(2026-08-18 12:00:00 UTC)),
        exec_row_at(
            "tests/deleted.py",
            "PASSED",
            datetime!(2026-08-18 13:00:00 UTC),
        ),
    ];
    let latest = build_latest_map(&universe, &rows);
    assert_eq!(latest.len(), 1);
    assert_eq!(
        latest[&(UNIVERSE_TEST_REPO_ID, "tests/a.py".to_owned())].status_bucket,
        "PASSED",
        "first row wins; a fold comparing timestamps would answer FAILED",
    );
}

/// `build_latest_map:1199-1205` carries five fields, and all five are rendered:
/// `AnalyticsListItem`'s `last_status`, `last_platform`, `last_run_name`,
/// `last_build` and `last_run_finished_at` read them one for one
/// (`analytics.rs:1419-1423`).
///
/// Pinned because four of the five are pass-through and nothing else in this
/// crate would notice if a later edit dropped one — the list would simply render
/// blanks.
#[test]
fn the_latest_entry_carries_every_field_the_lists_render() {
    let universe = vec![universe_test("tests/a.py")];
    let row = exec_row_at("tests/a.py", "PASSED", datetime!(2026-08-18 12:00:00 UTC));
    let expected = row.clone();

    let latest = build_latest_map(&universe, &[row]);
    let info = &latest[&(UNIVERSE_TEST_REPO_ID, "tests/a.py".to_owned())];

    assert_eq!(info.status_bucket, "PASSED");
    assert_eq!(info.environment_id, expected.environment_id);
    assert_eq!(info.run_id, Some(expected.run_id));
    assert_eq!(info.build, expected.build);
    assert_eq!(info.finished_at, Some(expected.ts));
}

/// `build_latest_map:1203` is `build: Some(row.build.clone())` over a `row.build`
/// that legacy **already normalized** at `:1032-1033`, so the value the list item
/// renders is trimmed and carries `"unknown"` for a run that named no build.
/// `LatestInfo::default()` keeps `build: None` (`:294-303`), so the `Option`
/// distinguishes "never executed" and nothing else.
///
/// **Controller ruling R15, and a live parity gap until it.** This port read
/// `row.build` straight through, so it rendered `null` where legacy renders
/// `"unknown"` and `"  9.1  "` where legacy renders `"9.1"` — on
/// `AnalyticsListItem::last_build`, a column the UI already draws. The gap was
/// hidden by a doc that called the aggregate the only reader of the field.
///
/// All four cases are in one fixture because it is the *pairing* that is the
/// rule: apply the substitution without the trim and `padded` fails; apply the
/// trim without the substitution and `absent` and `blank` fail; apply either to
/// the `Default` and `never` fails.
#[test]
fn the_latest_build_is_collapsed_exactly_as_legacy_collapses_it() {
    let universe = vec![
        universe_test("tests/absent.py"),
        universe_test("tests/blank.py"),
        universe_test("tests/padded.py"),
        universe_test("tests/never.py"),
    ];
    let at = datetime!(2026-08-18 12:00:00 UTC);
    let rows = vec![
        ExecRow {
            build: None,
            ..exec_row_at("tests/absent.py", "PASSED", at)
        },
        ExecRow {
            build: Some("   ".to_owned()),
            ..exec_row_at("tests/blank.py", "PASSED", at)
        },
        ExecRow {
            build: Some("  9.1  ".to_owned()),
            ..exec_row_at("tests/padded.py", "PASSED", at)
        },
    ];

    let latest = build_latest_map(&universe, &rows);

    assert_eq!(
        latest[&(UNIVERSE_TEST_REPO_ID, "tests/absent.py".to_owned())]
            .build
            .as_deref(),
        Some(UNKNOWN_BUILD),
        "a run that named no build is `unknown`, not absent",
    );
    assert_eq!(
        latest[&(UNIVERSE_TEST_REPO_ID, "tests/blank.py".to_owned())]
            .build
            .as_deref(),
        Some(UNKNOWN_BUILD),
        "`normalize_optional` maps a blank to absent first",
    );
    assert_eq!(
        latest[&(UNIVERSE_TEST_REPO_ID, "tests/padded.py".to_owned())]
            .build
            .as_deref(),
        Some("9.1"),
        "the label is trimmed before it is rendered",
    );
    assert!(
        !latest.contains_key(&(UNIVERSE_TEST_REPO_ID, "tests/never.py".to_owned())),
        "no row means no entry, and `LatestInfo::default` is the `None`",
    );
    assert_eq!(LatestInfo::default().build, None);

    // The rule itself, at the boundary the two folds share.
    assert_eq!(collapse_build(None), UNKNOWN_BUILD);
    assert_eq!(collapse_build(Some("")), UNKNOWN_BUILD);
    assert_eq!(collapse_build(Some("\t ")), UNKNOWN_BUILD);
    assert_eq!(collapse_build(Some(" 9.1.0-4412 ")), "9.1.0-4412");
    assert_eq!(
        collapse_build(Some(UNKNOWN_BUILD)),
        UNKNOWN_BUILD,
        "a literal `unknown` is indistinguishable from the substitution, here as \
         in legacy",
    );
}

/// A file with no row at all has no entry, and the absence is what reads as
/// `NOT_RUN` (`build_summary:1237-1241`, `.unwrap_or("NOT_RUN")`;
/// `LatestInfo::default` at `:294-303`).
///
/// This is the failure mode the whole alias map exists to prevent, so it is
/// asserted rather than left implicit: an unattributed row and a never-executed
/// test are indistinguishable downstream.
#[test]
fn a_file_with_no_row_has_no_entry_and_defaults_to_not_run() {
    let universe = vec![universe_test("tests/a.py"), universe_test("tests/b.py")];
    let rows = vec![exec_row_at(
        "tests/a.py",
        "PASSED",
        datetime!(2026-08-18 12:00:00 UTC),
    )];

    let latest = build_latest_map(&universe, &rows);
    assert!(!latest.contains_key(&(UNIVERSE_TEST_REPO_ID, "tests/b.py".to_owned())));

    // Legacy's own consumer pattern, verbatim (`:1407`,
    // `latest.get(..).cloned().unwrap_or_default()`). Asserting the whole value
    // rather than only its bucket is what pins `LatestInfo::default` — a default
    // that acquired a `PASSED` bucket, or a stray `run_id`, would make every
    // never-executed test render as a run that happened.
    let absent = latest
        .get(&(UNIVERSE_TEST_REPO_ID, "tests/b.py".to_owned()))
        .cloned()
        .unwrap_or_default();
    assert_eq!(absent, LatestInfo::default());
    assert_eq!(absent.status_bucket, NOT_RUN);
    assert_eq!(absent.run_id, None);
    assert_eq!(absent.finished_at, None);
}

// ---------------------------------------------------------------------------
// The port the universe arrives over
// ---------------------------------------------------------------------------

/// The universe a [`CatalogReader`] returns is exactly what
/// [`build_alias_map`](super::build_alias_map) consumes.
///
/// A composition test rather than a fake-fidelity one: it fails if the port's
/// return type stops being the core's input type, and it is the only place in
/// this task where the two tiers meet. `branch: None` selects the default
/// branch here — **not** every branch, which is what the same `None` means on
/// `UniverseFilter::branch`; the port's doc carries the asymmetry.
#[tokio::test]
async fn the_universe_a_reader_returns_feeds_the_alias_map() {
    let product = Uuid::from_u128(0x40);
    let catalog = FakeCatalog::default();
    catalog.add(
        product,
        DEFAULT_BRANCH,
        universe_test_full(
            "tests/cluster/test_upgrade.py",
            "test_upgrade",
            Some("Cluster Upgrade"),
        ),
    );
    catalog.add(
        product,
        "release/5.0",
        universe_test_full("tests/old/test_gone.py", "test_gone", None),
    );

    let universe = catalog
        .list_universe(&ctx(Uuid::from_u128(0x41)), Some(product), None)
        .await
        .unwrap();
    let aliases = build_alias_map(&universe);

    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "Cluster Upgrade", &aliases).as_deref(),
        Some("tests/cluster/test_upgrade.py"),
    );
    assert_eq!(
        resolve_row_test_file(UNIVERSE_TEST_REPO_ID, None, "test_gone", &aliases),
        None,
        "the other branch's universe is not in scope, so its aliases are not \
         registered",
    );
}

// ---------------------------------------------------------------------------
// Expected cases (Task 29)
// ---------------------------------------------------------------------------

/// [`expected_cases`](super::expected_cases)' own tests, in their own module
/// because the task-29 brief's fixture names — `universe_test` and
/// `collect_count` — collide with this file's shared, one-argument
/// `universe_test` (`crate::domain::service::test_support`, imported above)
/// and with nothing that names a `collect_count` at all. [`universe_test`]
/// here fixes `static_case_count` at `1` for every caller; these tests vary
/// it and the collect count besides, so a locally-scoped pair of builders is
/// cheaper than overloading the shared one or renaming every one of this
/// file's other call sites.
mod expected_cases_tests {
    use qa_catalog_sdk::UniverseTest;
    use qa_insights_sdk::CollectCount;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::domain::analytics::universe::expected_cases;

    /// The one repository every fixture in this module uses.
    /// [`expected_cases`] keys on `(repo_id, test_file)`, so every universe
    /// entry and every collect count needs one, and no test here varies it.
    fn repo_id() -> Uuid {
        Uuid::from_u128(0x90)
    }

    /// A universe entry for `test_file` whose static (regex) case count is
    /// `static_case_count`. Everything else is filler: [`expected_cases`]
    /// reads only `repo_id`, `test_file` and `static_case_count`, so the rest
    /// come from [`UniverseTest::default`].
    fn universe_test(test_file: &str, static_case_count: u32) -> UniverseTest {
        UniverseTest {
            repo_id: repo_id(),
            test_file: test_file.to_owned(),
            static_case_count,
            ..UniverseTest::default()
        }
    }

    /// One collect-job report for `test_file`, under [`repo_id`], on the
    /// branch every fixture here uses (`"main"` — legacy's
    /// `DEFAULT_COLLECT_BRANCH`). [`expected_cases`] does not read `branch`
    /// or `collected_at` at all — the caller has already filtered to one
    /// branch before this fold ever sees a [`CollectCount`] — so neither
    /// value is varied here.
    fn collect_count(test_file: &str, case_count: u32) -> CollectCount {
        CollectCount {
            repo_id: repo_id(),
            branch: "main".to_owned(),
            test_file: test_file.to_owned(),
            case_count,
            collected_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// Exact beats static. The static count does not expand parametrize
    /// (`manager/src/routes/analytics.rs:105-108`), so where the collect job
    /// has run, its number is the truthful one.
    ///
    /// Task-29 brief, Step 1, test 1 verbatim.
    #[test]
    fn an_exact_collect_count_overrides_the_static_count() {
        let expected = expected_cases(
            &[universe_test("tests/a.py", 3)],
            &[collect_count("tests/a.py", 11)],
        );
        assert_eq!(expected, 11);
    }

    /// Task-29 brief, Step 1, test 2 verbatim.
    #[test]
    fn the_static_count_is_used_where_no_collect_count_exists() {
        let expected = expected_cases(&[universe_test("tests/a.py", 3)], &[]);
        assert_eq!(expected, 3);
    }

    /// Task-29 brief hazard 1: "exact beats static" is per-file, not
    /// per-payload. A universe with one collected file and one uncollected
    /// file must mix the two sources file by file rather than picking one
    /// source for the whole total. `analytics.rs:770-779` sums
    /// `.get(&key)....unwrap_or(t.case_count)` **inside** one
    /// `.map(...).sum()` over `filtered_universe`, so this is that expression
    /// evaluated over more than one element — not two separate totals.
    #[test]
    fn a_mixed_universe_sums_the_exact_count_for_one_file_and_the_static_count_for_the_other() {
        let expected = expected_cases(
            &[
                universe_test("tests/a.py", 3),
                universe_test("tests/b.py", 2),
            ],
            &[collect_count("tests/a.py", 11)],
        );
        assert_eq!(expected, 11 + 2, "a.py's exact 11 plus b.py's static 2");
    }

    /// Task-29 brief hazard 3, the collect-only half: a `(repo_id, test_file)`
    /// pair the collect table names but the universe does not contain
    /// contributes **nothing** to the total. `expected_cases` iterates the universe, not
    /// `collect_counts`, exactly as legacy's `filtered_universe.iter().map(...)`
    /// does (`:770-779`) — a collect row for a file outside the current
    /// universe (deleted from the plans, on another branch, in another
    /// product's scope) is loaded and never looked up, the same way
    /// `load_collect_counts` loads every repository's rows for a branch and
    /// only some are ever consulted (`domain::repos::CollectRepository::
    /// list_counts_for`'s doc).
    #[test]
    fn a_collect_count_for_a_file_outside_the_universe_is_never_counted() {
        let expected = expected_cases(
            &[universe_test("tests/a.py", 3)],
            &[collect_count("tests/gone.py", 99)],
        );
        assert_eq!(
            expected, 3,
            "tests/gone.py's 99 must not appear anywhere in the total"
        );
    }

    /// Task-29 brief hazard 3, the universe-only half: a universe file the
    /// collect table has never heard of falls back to its own static count,
    /// which is the same rule
    /// [`the_static_count_is_used_where_no_collect_count_exists`] pins for a
    /// wholly-empty collect table — restated here with a *non-empty* one, so
    /// the fallback is shown to be per-file rather than "empty table only".
    #[test]
    fn a_universe_file_absent_from_a_non_empty_collect_table_still_falls_back_to_static() {
        let expected = expected_cases(
            &[
                universe_test("tests/a.py", 3),
                universe_test("tests/b.py", 2),
            ],
            &[collect_count("tests/other_repos_file.py", 40)],
        );
        assert_eq!(expected, 3 + 2);
    }
}
