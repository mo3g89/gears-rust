//! Effective exclusivity of a launch: `launch ?? plan.yaml ?? OR(TEST_META
//! over the files that will run) ?? parallel`.
//!
//! Ported from `manager/src/services/exclusivity.rs` and the tag-admission half
//! of `manager/src/services/test_meta.rs`. Frozen semantics — the governing
//! document is `../testrunner/docs/guides/exclusive-runs-and-the-queue.md`.
//!
//! Pure by design, for the reason the source system gives
//! (`manager/src/services/exclusivity.rs:5-7`): the async half is plan lookup
//! and file reads, and the part worth testing is which tier wins. Everything
//! here takes already-fetched data, so the whole contract is unit-testable with
//! no database and no fixture tree. qa-catalog supplies the inputs (per-plan
//! and per-file flags plus tags) and deliberately does not interpret them
//! (DESIGN §3.2, qa-catalog "Responsibility boundaries" — `DESIGN.md:282`).
//!
//! Fail-closed here means fail *toward exclusive*: every ambiguity the source
//! system resolves, it resolves toward "give this run the platform to itself",
//! because an unnecessary wait is recoverable and a destructive test running
//! beside another is not (`manager/src/services/test_meta.rs:30-41`).
//!
//! **The subtlest rule in this module: `TestMeta`-false is not `Default`.**
//! Both answer "run this in parallel", and the two are never interchangeable.
//! `TestMeta` with `exclusive = false` means *files were read and every one of
//! them said parallel* — an explicit answer. `Default` means *nobody had an
//! opinion at all*: no `plan.yaml` flag, and not one file voted, because none
//! was read or the tag filter dropped them all. Only the tier tells an operator
//! which of those happened, so a `plan.yaml`-less exclusive suite that silently
//! stopped being scanned is visible as `default` in the log rather than
//! disguised as a deliberate `test_meta` parallel. The distinction is carried
//! by [`aggregate_test_meta`] (empty set ⇒ `None`), by
//! [`file_declares_exclusive`] (an admitted file always votes), and by
//! [`combine_nested`]'s fourth branch; legacy has an explicit test forbidding
//! the collapse (`manager/src/services/exclusivity.rs:656-664`), and it is the
//! reconciliation `DECOMPOSITION.md:148` flags for
//! `cpt-cf-qa-fr-runs-exclusivity`.

use qa_runs_sdk::ExclusiveTier;

/// One file's exclusivity-relevant metadata, as qa-catalog returns it.
///
/// A local projection of `qa_catalog_sdk::TestFileMeta` rather than the SDK
/// type itself: this module must stay callable from unit tests without
/// constructing the catalog's full model, and the projection documents exactly
/// which fields the rule reads. Only `tags` and `exclusive` decide anything;
/// `path` identifies the file the vote came from, for the caller's logging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMeta {
    pub path: String,
    pub tags: Vec<String>,
    /// Three-state as the catalog parses it: `None` = the key was absent
    /// (`qa-catalog/src/domain/parsing/test_meta.rs:108-120`).
    pub exclusive: Option<bool>,
}

/// The three tiers that can have an opinion, as named fields.
///
/// Named rather than three positional `Option<bool>`s because the arguments are
/// mutually indistinguishable to the compiler: transposing `launch` and `plan`
/// silently defeats an operator's override *and* mislabels the tier in the log.
/// Same reasoning the source system records
/// (`manager/src/services/exclusivity.rs:88-95`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tiers {
    /// The launch request's override. For a scheduled run this *is* the
    /// schedule's stored choice, delivered through the launch request — so by
    /// the time resolution runs, a schedule's choice is `launch` and the two
    /// can never disagree (`manager/src/services/exclusivity.rs:113-119`).
    pub launch: Option<bool>,
    /// `plan.yaml`'s declaration.
    pub plan: Option<bool>,
    /// The OR over the in-scope files' `TEST_META`, or `None` when no file
    /// contributed at all.
    pub test_meta: Option<bool>,
}

/// The effective flag plus where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub exclusive: bool,
    pub tier: ExclusiveTier,
}

/// `launch ?? plan.yaml ?? test_meta ?? false`. The first tier with an opinion
/// wins, and `false` **is** an opinion — that is the whole reason the upper
/// tiers are `Option<bool>`. With plain `bool` the rule "first one that is set
/// wins" is unimplementable, because unset and `false` collide and exclusivity
/// could only ever escalate (`manager/src/services/exclusivity.rs:108-143`;
/// guide lines 35-41).
#[must_use]
pub fn resolve_precedence(tiers: Tiers) -> Resolution {
    if let Some(exclusive) = tiers.launch {
        return Resolution {
            exclusive,
            tier: ExclusiveTier::Launch,
        };
    }
    if let Some(exclusive) = tiers.plan {
        return Resolution {
            exclusive,
            tier: ExclusiveTier::Plan,
        };
    }
    if let Some(exclusive) = tiers.test_meta {
        return Resolution {
            exclusive,
            tier: ExclusiveTier::TestMeta,
        };
    }
    Resolution {
        exclusive: false,
        tier: ExclusiveTier::Default,
    }
}

/// Aggregate the `TEST_META` tier over a file set: `None` when no file voted,
/// else the OR.
///
/// OR rather than "first wins" because per file `exclusive: False` honestly
/// means "I do not need the platform to myself", not "forbid exclusivity for
/// the whole run" — one destructive test makes the suite destructive
/// (`manager/src/services/exclusivity.rs:145-158`; guide lines 47-48).
#[must_use]
pub fn aggregate_test_meta(flags: &[bool]) -> Option<bool> {
    if flags.is_empty() {
        None
    } else {
        Some(flags.iter().any(|flag| *flag))
    }
}

/// Whether a file's tags satisfy an include/exclude filter.
///
/// Ported verbatim from `manager/src/services/test_meta.rs:92-108`, asymmetry
/// included and **on purpose**: a file with no tags is admitted by an
/// exclude-only filter and rejected by an include filter, because it cannot
/// prove membership. Fail-open for exclude, fail-closed for include
/// (`test_meta.rs:89-91`). All three inputs are trimmed and lowercased here so
/// no caller can normalize one side and forget the other
/// (`test_meta.rs:86-87`). qa-catalog deliberately leaves this to qa-runs
/// (`DECOMPOSITION.md:149`).
#[must_use]
pub fn tags_admit(tags: &[String], include_tags: &[String], exclude_tags: &[String]) -> bool {
    fn normalize(values: &[String]) -> Vec<String> {
        values
            .iter()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect()
    }
    let tags = normalize(tags);
    let included = normalize(include_tags);
    let excluded = normalize(exclude_tags);

    if !included.is_empty() && !tags.iter().any(|tag| included.contains(tag)) {
        return false;
    }
    !tags.iter().any(|tag| excluded.contains(tag))
}

/// One file's contribution to the `TEST_META` tier, or `None` when the run's
/// tag filter excludes it — the guide's "all its test files are considered
/// *after* tag filtering" (`manager/src/services/exclusivity.rs:160-173`; guide
/// lines 47-49).
///
/// The `Option<bool>` -> `bool` reconciliation flagged in DECOMPOSITION 2.2
/// (`DECOMPOSITION.md:148`) lives on the last line. The source system's
/// per-file parser returns `bool`, where a missing `exclusive` key is `false`
/// (`manager/src/services/test_meta.rs:42-54`, its test at `:138-142`), so an
/// admitted file **always votes** and votes `false` when it declared nothing
/// (`exclusivity.rs:172`). Only an unread file (never reaches here) or a
/// filtered-out one abstains. Reading catalog's `None` as "no opinion" instead
/// would misattribute a run whose files were all read and all parallel to tier
/// `Default`, which the source system has an explicit test forbidding
/// (`exclusivity.rs:656-664`).
#[must_use]
pub fn file_declares_exclusive(
    file: &FileMeta,
    include_tags: &[String],
    exclude_tags: &[String],
) -> Option<bool> {
    if !tags_admit(&file.tags, include_tags, exclude_tags) {
        return None;
    }
    Some(file.exclusive.unwrap_or(false))
}

/// Resolve one standard plan: its `plan.yaml` tier, falling through to
/// `TEST_META` over the files this run will execute.
///
/// `plan_flag` short-circuits: `plan.yaml` outranks `TEST_META`, so a declared
/// flag decides without any file being consulted
/// (`manager/src/services/exclusivity.rs:342-348`).
///
/// For a single-test run, pass a one-element `files` and **empty** tag filters:
/// a single-test run considers only that file and no filter applies
/// (`manager/src/services/exclusivity.rs:207-224`, which passes `&[], &[]`;
/// guide lines 49-50).
///
/// Never fails. A plan that cannot be resolved (wrong branch, deleted repo)
/// never reaches here at all: the caller logs a warning and resolves parallel,
/// and the launch then fails later at dispatch for the real reason
/// (`manager/src/services/exclusivity.rs:176-179, 234-249`; guide lines
/// 214-216).
///
/// **`files` must contain only files that were actually read.** A file whose
/// content could not be fetched must be *omitted*, never passed as a default
/// [`FileMeta`]: an admitted file always votes, so a default one votes
/// `Some(false)`, turning a `Default` resolution into a `TestMeta`-false one —
/// and an unreadable *destructive* file would have its vote replaced by a
/// parallel one. The caller also owns the operator-visible warning the source
/// system emits when files were unreadable and nothing else voted
/// (`manager/src/services/exclusivity.rs:440-449`), which is I/O-side and so is
/// not reproduced here.
#[must_use]
pub fn resolve_plan_tier(
    plan_flag: Option<bool>,
    files: &[FileMeta],
    include_tags: &[String],
    exclude_tags: &[String],
) -> Resolution {
    if let Some(declared) = plan_flag {
        return resolve_precedence(Tiers {
            plan: Some(declared),
            ..Tiers::default()
        });
    }
    let flags: Vec<bool> = files
        .iter()
        .filter_map(|file| file_declares_exclusive(file, include_tags, exclude_tags))
        .collect();
    resolve_precedence(Tiers {
        test_meta: aggregate_test_meta(&flags),
        ..Tiers::default()
    })
}

/// Combine the nested plans' answers for a custom-plan run: a custom plan is
/// one run, and its flag is the OR over the plans it composes.
///
/// Within one nested plan, `plan.yaml` outranks `TEST_META` (that happens in
/// [`resolve_plan_tier`]). Across plans the aggregate is an OR, and the
/// reported tier names the **strongest source that actually contributed** —
/// including the fourth branch, where nothing declared a plan flag and every
/// scanned file said parallel: that reports `TestMeta`, not `Default`
/// (`manager/src/services/exclusivity.rs:468-488`, its test at `:656-664`).
#[must_use]
pub fn combine_nested(plan_tier: Option<bool>, test_meta_tier: Option<bool>) -> Resolution {
    let exclusive = plan_tier.unwrap_or(false) || test_meta_tier.unwrap_or(false);
    // Branches 1-2 are the "contributed a `true`" cases, strongest source
    // first. Branches 3-4 are the "contributed a `false`" cases: nobody asked
    // for exclusivity, but somebody was *asked and answered*, so the tier names
    // them rather than collapsing to `Default` — see this module's header.
    let tier = if plan_tier == Some(true) {
        ExclusiveTier::Plan
    } else if test_meta_tier == Some(true) {
        ExclusiveTier::TestMeta
    } else if plan_tier.is_some() {
        ExclusiveTier::Plan
    } else if test_meta_tier.is_some() {
        ExclusiveTier::TestMeta
    } else {
        ExclusiveTier::Default
    };
    Resolution { exclusive, tier }
}

/// The whole rule in one call, for a single plan.
///
/// Separate from [`resolve_plan_tier`] so the launch-tier short-circuit is
/// visible and testable: a launch override wins outright and does no work on
/// the lower tiers at all (`manager/src/services/exclusivity.rs:181-187`),
/// which is why the same override must produce the same answer whether or not
/// any file metadata was fetched.
#[must_use]
pub fn resolve(
    launch: Option<bool>,
    plan_flag: Option<bool>,
    files: &[FileMeta],
    include_tags: &[String],
    exclude_tags: &[String],
) -> Resolution {
    if let Some(exclusive) = launch {
        return Resolution {
            exclusive,
            tier: ExclusiveTier::Launch,
        };
    }
    resolve_plan_tier(plan_flag, files, include_tags, exclude_tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(path: &str, tags: &[&str], exclusive: Option<bool>) -> FileMeta {
        FileMeta {
            path: path.to_owned(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            exclusive,
        }
    }

    // ---------- resolve_precedence: the cascade ----------

    #[test]
    fn launch_true_wins_over_plan_false() {
        let r = resolve_precedence(Tiers {
            launch: Some(true),
            plan: Some(false),
            test_meta: Some(false),
        });
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Launch);
    }

    /// The point of `Option<bool>` at the upper tiers: an operator can run an
    /// exclusive-marked suite in parallel tonight without editing test files
    /// (guide lines 38-41).
    #[test]
    fn launch_false_wins_over_exclusive_tests() {
        let r = resolve_precedence(Tiers {
            launch: Some(false),
            test_meta: Some(true),
            ..Tiers::default()
        });
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Launch);
    }

    #[test]
    fn plan_yaml_wins_when_there_is_no_launch_override() {
        let r = resolve_precedence(Tiers {
            plan: Some(true),
            test_meta: Some(false),
            ..Tiers::default()
        });
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);
    }

    #[test]
    fn plan_yaml_false_beats_an_exclusive_test() {
        let r = resolve_precedence(Tiers {
            plan: Some(false),
            test_meta: Some(true),
            ..Tiers::default()
        });
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);
    }

    #[test]
    fn test_meta_decides_when_the_upper_tiers_are_silent() {
        let r = resolve_precedence(Tiers {
            test_meta: Some(true),
            ..Tiers::default()
        });
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    /// The tier reconciliation DECOMPOSITION 2.2 flags (`DECOMPOSITION.md:148`):
    /// an explicit all-parallel answer from `TEST_META` must report the
    /// `TestMeta` tier, not `Default`. Legacy asserts this directly
    /// (`manager/src/services/exclusivity.rs:722-730`).
    #[test]
    fn test_meta_false_still_reports_the_test_meta_tier() {
        let r = resolve_precedence(Tiers {
            test_meta: Some(false),
            ..Tiers::default()
        });
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    #[test]
    fn nothing_declared_defaults_to_parallel() {
        let r = resolve_precedence(Tiers::default());
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Default);
    }

    // ---------- aggregate_test_meta ----------

    /// No file was read (a plan whose tests are missing from the snapshot), so
    /// the tier has no opinion — it must not masquerade as an explicit
    /// "parallel", or a `plan.yaml`-less exclusive suite looks deliberately
    /// parallel in the logs (`manager/src/services/exclusivity.rs:739-746`).
    #[test]
    fn aggregate_of_an_empty_file_set_has_no_opinion() {
        assert_eq!(aggregate_test_meta(&[]), None);
    }

    #[test]
    fn aggregate_is_an_or_over_files() {
        assert_eq!(aggregate_test_meta(&[false, true, false]), Some(true));
    }

    #[test]
    fn aggregate_of_all_parallel_files_is_an_explicit_false() {
        assert_eq!(aggregate_test_meta(&[false, false]), Some(false));
    }

    // ---------- file_declares_exclusive: the tag-filter asymmetry ----------

    /// A `destructive` test this run's `exclude_tags` drops is not going to
    /// execute, so it must not make the run exclusive
    /// (`manager/src/services/exclusivity.rs:758-767`).
    #[test]
    fn a_file_dropped_by_the_exclude_filter_does_not_contribute() {
        let f = meta("a.py", &["destructive"], Some(true));
        assert_eq!(
            file_declares_exclusive(&f, &[], &["destructive".to_owned()]),
            None
        );
    }

    #[test]
    fn a_file_kept_by_the_include_filter_contributes_its_flag() {
        let f = meta("a.py", &["e2e"], Some(true));
        assert_eq!(
            file_declares_exclusive(&f, &["e2e".to_owned()], &[]),
            Some(true)
        );
    }

    /// The `Option<bool>` -> `bool` reconciliation: a file that WAS read and
    /// declared nothing contributes an explicit `false`, because legacy's
    /// per-file parser returns `bool` and a missing key is false
    /// (`manager/src/services/test_meta.rs:42-54`, its test at `:138-142`;
    /// `exclusivity.rs:172`). Without this, an all-quiet plan is misattributed
    /// to tier `Default`.
    #[test]
    fn a_read_file_with_no_declaration_contributes_false_not_nothing() {
        let f = meta("a.py", &["e2e"], None);
        assert_eq!(
            file_declares_exclusive(&f, &[], &[]),
            Some(false),
            "a read file always votes; only an unread or filtered-out file abstains"
        );
    }

    #[test]
    fn a_file_declaring_parallel_contributes_false() {
        let f = meta("a.py", &[], Some(false));
        assert_eq!(file_declares_exclusive(&f, &[], &[]), Some(false));
    }

    // ---------- tags_admit: the deliberate asymmetry ----------

    #[test]
    fn an_empty_filter_admits_everything() {
        assert!(tags_admit(&["e2e".to_owned()], &[], &[]));
        assert!(tags_admit(&[], &[], &[]));
    }

    #[test]
    fn include_filter_keeps_only_matching_tags() {
        let tags = vec!["e2e".to_owned(), "destructive".to_owned()];
        assert!(tags_admit(&tags, &["e2e".to_owned()], &[]));
        assert!(!tags_admit(&tags, &["smoke".to_owned()], &[]));
    }

    /// The asymmetry, stated as two assertions so it is a decision rather than
    /// an accident: an untagged file cannot prove membership, so an include
    /// filter drops it (fail-closed), while an exclude-only filter admits it
    /// (fail-open). `manager/src/services/test_meta.rs:89-91`, `:214-215`.
    #[test]
    fn an_untagged_file_is_dropped_by_include_and_admitted_by_exclude() {
        assert!(
            !tags_admit(&[], &["smoke".to_owned()], &[]),
            "fail-closed for include: an untagged file cannot prove membership"
        );
        assert!(
            tags_admit(&[], &[], &["destructive".to_owned()]),
            "fail-open for exclude: an untagged file matches no exclusion"
        );
    }

    #[test]
    fn exclude_filter_drops_matching_tags() {
        let tags = vec!["e2e".to_owned(), "destructive".to_owned()];
        assert!(!tags_admit(&tags, &[], &["destructive".to_owned()]));
        assert!(tags_admit(&tags, &[], &["flaky".to_owned()]));
    }

    #[test]
    fn filters_are_trimmed_and_case_insensitive_on_both_sides() {
        let tags = vec!["Destructive".to_owned()];
        assert!(!tags_admit(&tags, &[], &["DESTRUCTIVE".to_owned()]));
        assert!(tags_admit(&tags, &[" destructive ".to_owned()], &[]));
    }

    /// Both sides normalize, so an entry that is only whitespace is dropped
    /// rather than becoming an unmatchable filter
    /// (`manager/src/services/test_meta.rs:93-99`).
    #[test]
    fn blank_filter_entries_are_ignored() {
        let tags = vec!["e2e".to_owned()];
        assert!(
            tags_admit(&tags, &["   ".to_owned()], &[]),
            "a whitespace-only include entry must not reject everything"
        );
    }

    // ---------- resolve_plan_tier: plan.yaml short-circuits ----------

    /// `plan.yaml` outranks `TEST_META`, so a declared plan flag decides without
    /// any file being consulted
    /// (`manager/src/services/exclusivity.rs:342-348`).
    #[test]
    fn a_declared_plan_flag_short_circuits_the_file_scan() {
        let files = vec![meta("a.py", &[], Some(true))];
        let r = resolve_plan_tier(Some(false), &files, &[], &[]);
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);
    }

    #[test]
    fn an_absent_plan_flag_falls_through_to_the_files() {
        let files = vec![
            meta("a.py", &[], Some(false)),
            meta("b.py", &[], Some(true)),
        ];
        let r = resolve_plan_tier(None, &files, &[], &[]);
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    /// Every file filtered out => nobody voted => `Default`, and critically NOT
    /// an explicit parallel. Legacy reaches the same state through
    /// `aggregate_test_meta(&[])` (`manager/src/services/exclusivity.rs:440-449`
    /// keeps this quiet when the filter legitimately excluded everything).
    #[test]
    fn a_plan_whose_every_file_is_filtered_out_resolves_default() {
        let files = vec![meta("a.py", &["destructive"], Some(true))];
        let r = resolve_plan_tier(None, &files, &[], &["destructive".to_owned()]);
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Default);
    }

    /// The composition of the two rules above: every file was read, none is
    /// filtered out, and none declared anything — so all of them vote
    /// `Some(false)`, the aggregate is `Some(false)`, and the run reports tier
    /// `TestMeta`. Reporting `Default` here is the misattribution legacy has an
    /// explicit test forbidding (`manager/src/services/exclusivity.rs:656-664`).
    ///
    /// The all-silent set is what makes this a guard rather than a
    /// restatement: with every file declaring nothing, the `unwrap_or(false)`
    /// in [`file_declares_exclusive`] is the only thing standing between this
    /// and a `Default` misattribution. A set that mixes in even one honest
    /// `Some(false)` would keep the aggregate non-empty on its own and pass
    /// either way.
    #[test]
    fn a_plan_whose_files_all_say_parallel_reports_test_meta_not_default() {
        let silent = vec![
            meta("a.py", &["smoke"], None),
            meta("b.py", &["smoke"], None),
        ];
        let r = resolve_plan_tier(None, &silent, &[], &[]);
        assert!(!r.exclusive);
        assert_eq!(
            r.tier,
            ExclusiveTier::TestMeta,
            "all files read and all parallel is an explicit TEST_META answer, \
             not 'nobody had an opinion'"
        );

        // The same answer once an honest `false` is mixed in, which drives the
        // `unwrap_or(false)` branch and the declared-`false` branch through one
        // composed call.
        let mixed = vec![
            meta("a.py", &["smoke"], None),
            meta("b.py", &["smoke"], Some(false)),
        ];
        assert_eq!(resolve_plan_tier(None, &mixed, &[], &[]), r);
    }

    /// One destructive file among many makes the whole run exclusive
    /// (guide lines 47-48).
    #[test]
    fn one_exclusive_file_among_fifty_makes_the_run_exclusive() {
        let mut files: Vec<FileMeta> = (0..49)
            .map(|i| meta(&format!("t{i}.py"), &["smoke"], Some(false)))
            .collect();
        files.push(meta("upgrade.py", &["destructive"], Some(true)));
        let r = resolve_plan_tier(None, &files, &[], &[]);
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    // ---------- combine_nested: custom plans ----------

    #[test]
    fn a_nested_plan_yaml_that_says_exclusive_makes_the_custom_plan_exclusive() {
        let r = combine_nested(Some(true), Some(false));
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);
    }

    #[test]
    fn a_nested_exclusive_test_makes_the_custom_plan_exclusive() {
        let r = combine_nested(Some(false), Some(true));
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    /// Two rules, not one: a declared `plan.yaml` parallel is an answer and
    /// keeps the `Plan` tier, while nothing declared anywhere is `Default`.
    #[test]
    fn a_custom_plan_of_parallel_plans_stays_parallel() {
        let r = combine_nested(Some(false), None);
        assert!(!r.exclusive, "a declared plan.yaml false is still parallel");
        assert_eq!(
            r.tier,
            ExclusiveTier::Plan,
            "a nested plan.yaml declared it, so the tier is Plan"
        );

        let r = combine_nested(None, None);
        assert!(!r.exclusive, "nothing declared anything, so parallel");
        assert_eq!(
            r.tier,
            ExclusiveTier::Default,
            "no plan.yaml and no file vote anywhere, so the tier is Default"
        );
    }

    /// The fourth cascade branch: no `plan.yaml` anywhere, and every scanned
    /// file said parallel. The answer is false, but it came from `TEST_META`
    /// rather than from nobody having an opinion — the log must not read
    /// `default` (`manager/src/services/exclusivity.rs:656-664`, which is an
    /// explicit legacy test).
    #[test]
    fn a_custom_plan_whose_tests_all_declare_parallel_reports_the_test_meta_tier() {
        let r = combine_nested(None, Some(false));
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    // ---------- the launch tier short-circuits everything ----------

    /// A launch-level override wins outright, so no file work is done for it
    /// (`manager/src/services/exclusivity.rs:181-187`). Asserted here as "the
    /// answer does not depend on the files at all".
    #[test]
    fn a_launch_override_ignores_every_lower_tier_input() {
        let files = vec![meta("a.py", &[], Some(true))];
        let with_files = resolve(Some(false), Some(true), &files, &[], &[]);
        let without_files = resolve(Some(false), Some(true), &[], &[], &[]);
        assert_eq!(with_files, without_files);
        assert!(!with_files.exclusive);
        assert_eq!(with_files.tier, ExclusiveTier::Launch);
    }

    /// With no launch override, [`resolve`] must be exactly
    /// [`resolve_plan_tier`] — the launch tier is the only thing it adds. Both
    /// arms matter: the first pins the fall-through to the files, the second
    /// pins that `plan_flag` is forwarded rather than dropped.
    ///
    /// The differential assertion is not tautological: the right-hand side
    /// calls [`resolve_plan_tier`] directly, so a dropped or transposed
    /// argument diverges the two sides, and the value assertions pin the answer
    /// independently of the delegation.
    #[test]
    fn resolve_without_a_launch_override_delegates_to_the_plan_tier() {
        let files = vec![meta("a.py", &[], Some(true))];

        let r = resolve(None, None, &files, &[], &[]);
        assert_eq!(r, resolve_plan_tier(None, &files, &[], &[]));
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);

        let r = resolve(None, Some(false), &files, &[], &[]);
        assert_eq!(r, resolve_plan_tier(Some(false), &files, &[], &[]));
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);
    }
}
