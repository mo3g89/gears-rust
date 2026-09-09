//! Tests for the overview query's rules.
//!
//! **One test per rejection, and each names the legacy line it pins**, which is
//! this task's whole brief for this module: *"a 400 that legacy returns and this
//! gear does not is a behaviour change the SPA will notice."* **Six** rejections,
//! six tests, plus the ones that pin what is deliberately *not* rejected — which
//! is the other half of the same property, because a gear that 400s where legacy
//! accepts is the same kind of break in the other direction.
//!
//! Six: [`super::normalize_overview_query`]'s five, plus
//! [`super::normalize_build`]'s, which is `api_build_tests`' own check and runs
//! before that function rather than inside it. This header and three docs below
//! said "five" — the count of the *first* function's rules mistaken for the count
//! of the module's.
//!
//! # The status code is asserted, not assumed
//!
//! Every rejection is checked twice: for the `field`/`message` pair here, and —
//! once, in [`every_rejection_is_a_four_hundred_invalid_argument`] — for the
//! canonical variant the REST tier maps it to. The second assertion is what makes
//! "400" a tested claim rather than a comment, and it is done once over all six
//! rather than six times because the mapping is one arm in
//! [`crate::domain::error`] and not per-rule.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{
    NormalizedOverviewQuery, OverviewQuery, Scope, normalize_build, normalize_overview_query,
    parse_group, parse_scope,
};
use crate::domain::analytics::aggregates::GroupBy;
use crate::domain::error::DomainError;

/// A query that passes every rule, for a test that varies one field.
fn valid() -> OverviewQuery {
    OverviewQuery {
        product_id: "vhp".to_owned(),
        version: "5.0.1".to_owned(),
        scope: "all".to_owned(),
        ..OverviewQuery::default()
    }
}

/// The `(field, message)` of a [`DomainError::Validation`], or a panic naming
/// what arrived instead.
fn refusal(err: DomainError) -> (String, String) {
    match err {
        DomainError::Validation { field, message } => (field, message),
        other => panic!("expected a Validation, got {other:?}"),
    }
}

/// The `(field, message)` a query that must be refused is refused with.
fn refuse(query: &OverviewQuery) -> (String, String) {
    refusal(normalize_overview_query(query).expect_err("the query must be refused"))
}

/// The normalized form of a query that must be accepted.
fn accept(query: &OverviewQuery) -> NormalizedOverviewQuery {
    normalize_overview_query(query).expect("the query must be accepted")
}

// ---------------------------------------------------------------------------
// The six rejections
// ---------------------------------------------------------------------------

/// `analytics.rs:2395-2401`: a `product_id` that is empty **after trimming** is
/// refused with `"product_id is required"`.
///
/// Whitespace-only as well as empty, because legacy tests `trim().is_empty()` and
/// a query string carrying `?product_id=%20` is what a form with an untouched
/// field sends.
#[test]
fn a_blank_product_id_is_refused_with_legacys_message() {
    for blank in ["", "   ", "\t\n"] {
        let query = OverviewQuery {
            product_id: blank.to_owned(),
            ..valid()
        };
        assert_eq!(
            refuse(&query),
            ("product_id".to_owned(), "product_id is required".to_owned()),
            "for {blank:?}",
        );
    }
}

/// `analytics.rs:2403-2406`: a blank `version` is refused with
/// `"version is required"`.
#[test]
fn a_blank_version_is_refused_with_legacys_message() {
    let query = OverviewQuery {
        version: "  ".to_owned(),
        ..valid()
    };
    assert_eq!(
        refuse(&query),
        ("version".to_owned(), "version is required".to_owned()),
    );
}

/// `analytics.rs:2104-2113`: a `scope` that is neither `all` nor `plan` is
/// refused with `"scope must be 'all' or 'plan'"`, quotes included.
///
/// The empty string is in the fixture list deliberately: `scope` has **no**
/// default, unlike `group_by`, so an absent-or-blank scope is a 400 rather than
/// `all`. A parser that defaulted it would accept every one of these.
#[test]
fn an_unknown_scope_is_refused_with_legacys_message() {
    for bad in ["", "  ", "ALL_PLANS", "plans", "product"] {
        assert_eq!(
            refusal(parse_scope(bad).expect_err("refused")),
            (
                "scope".to_owned(),
                "scope must be 'all' or 'plan'".to_owned()
            ),
            "for {bad:?}",
        );
    }
}

/// `analytics.rs:2115-2127`: legacy refuses an unknown `group_by` with
/// `"group_by must be one of: none, component, tag, platform"`. **This message is
/// not legacy's verbatim after Step 0**: ours ends `…, tag, environment` instead —
/// the same deliberate divergence `parse_group`'s own doc records, not parity.
///
/// **`Some("")` is refused and `None` is not**, which is legacy's asymmetry
/// (`:2116` defaults an absent parameter to the literal `"none"`, and a blank one
/// trims to `""`, which matches no arm). Both halves are asserted here, because
/// "empty means absent" is the natural assumption and it would turn a 400 into a
/// silent `none`.
///
/// **`Some("platform")` is asserted here too.** It was the fourth grouping's
/// name before the `group_by=environment` rename and is no longer one of the
/// four arms, so it must fail loudly rather than fall back to
/// [`GroupBy::None`] — the shape Critical-1 warned about, ruled out by reading
/// `parse_group` and pinned here rather than merely assumed.
#[test]
fn an_unknown_group_by_is_refused_and_a_blank_one_counts_as_unknown() {
    // `Some("plan")` is a *scope* word and is not one of the four groupings;
    // `Some("components")` is the plural, which is the likeliest real mistake.
    // `Some("platform")` is the retired fourth grouping's own former name.
    // Note what is **not** here: `Some("component ")` is *accepted*, because
    // legacy trims before matching — `the_vocabularies_parse_case_insensitively_and_trimmed`
    // is where that lives, and this fixture asserted the opposite until it was run.
    for bad in [
        Some(""),
        Some("   "),
        Some("plan"),
        Some("components"),
        Some("platform"),
    ] {
        assert_eq!(
            refusal(parse_group(bad).expect_err("refused")),
            (
                "group_by".to_owned(),
                "group_by must be one of: none, component, tag, environment".to_owned()
            ),
            "for {bad:?}",
        );
    }
    assert_eq!(
        parse_group(None).expect("an absent group_by defaults"),
        GroupBy::None,
        "absent is `none`, where present-and-blank is a 400",
    );
}

/// `analytics.rs:2413-2418`: `scope=plan` with no `plan_id` is refused with
/// `"plan_id is required when scope=plan"`.
///
/// Blank counts as absent, because `plan_id` goes through `normalize_optional`
/// (`:2412`) **before** the rule reads it — so `?scope=plan&plan_id=` is refused
/// too, and a rule that tested `is_none()` on the raw parameter would accept it.
#[test]
fn a_plan_scope_without_a_plan_id_is_refused_with_legacys_message() {
    for missing in [None, Some(""), Some("  ")] {
        let query = OverviewQuery {
            scope: "plan".to_owned(),
            plan_id: missing.map(str::to_owned),
            ..valid()
        };
        assert_eq!(
            refuse(&query),
            (
                "plan_id".to_owned(),
                "plan_id is required when scope=plan".to_owned()
            ),
            "for {missing:?}",
        );
    }
}

/// `analytics.rs:373-376`: `api_build_tests` refuses an empty-after-trim `build`
/// with `"build is required"`.
///
/// The **sixth** rejection and the only one outside `normalize_overview_query` —
/// this doc said "the fifth", which is what a reader counting only that
/// function's rules gets. A non-blank build is returned **trimmed**, which
/// matters because it becomes a grouping key: an untrimmed `" 20260818.3"` would
/// form a build bucket of its own beside the real one.
#[test]
fn a_blank_build_is_refused_with_legacys_message() {
    for blank in ["", " ", "\n\t "] {
        assert_eq!(
            refusal(normalize_build(blank).expect_err("refused")),
            ("build".to_owned(), "build is required".to_owned()),
            "for {blank:?}",
        );
    }
    assert_eq!(
        normalize_build("  20260818.3 ").expect("accepted"),
        "20260818.3",
    );
}

/// **Every one of the six is a 400**, as the canonical `InvalidArgument` the
/// REST tier renders.
///
/// This is the assertion the brief calls the whole point: legacy returns
/// `(StatusCode::BAD_REQUEST, String)` from all six sites, and a rejection that
/// arrived here as any other `DomainError` variant would be a different status
/// on the wire — `Internal` is an opaque 500, `Forbidden` a 403 — with the tests
/// above still passing, because they only look at the message.
///
/// `CanonicalError::InvalidArgument` is the variant `domain::error` maps
/// `Validation` to, and `api::rest::error`'s own
/// `a_refused_window_is_400_invalid_argument` is what
/// ties that variant to the status code. Asserted once over all six rather than
/// six times, because the mapping is one arm and not per-rule.
///
/// The array below has always held six entries; this doc said "five" three times
/// over it, which is the count of `normalize_overview_query`'s rules rather than
/// of this module's.
#[test]
fn every_rejection_is_a_four_hundred_invalid_argument() {
    let blank_product = OverviewQuery {
        product_id: String::new(),
        ..valid()
    };
    let blank_version = OverviewQuery {
        version: String::new(),
        ..valid()
    };
    let bad_scope = OverviewQuery {
        scope: "nonsense".to_owned(),
        ..valid()
    };
    let bad_group = OverviewQuery {
        group_by: Some("nonsense".to_owned()),
        ..valid()
    };
    let plan_without_id = OverviewQuery {
        scope: "plan".to_owned(),
        ..valid()
    };

    let refusals = [
        normalize_overview_query(&blank_product).expect_err("refused"),
        normalize_overview_query(&blank_version).expect_err("refused"),
        normalize_overview_query(&bad_scope).expect_err("refused"),
        normalize_overview_query(&bad_group).expect_err("refused"),
        normalize_overview_query(&plan_without_id).expect_err("refused"),
        normalize_build("").expect_err("refused"),
    ];

    for err in refusals {
        let rendered: toolkit_canonical_errors::CanonicalError = err.into();
        assert!(
            matches!(
                rendered,
                toolkit_canonical_errors::CanonicalError::InvalidArgument { .. }
            ),
            "every overview rejection must be a 400: {rendered:?}",
        );
    }
}

/// The **first** failing rule is the one reported, and legacy's order is
/// `product_id`, `version`, `scope`, `group_by`, then the plan-scope rule.
///
/// A query wrong in every way answers about `product_id`. Reordering the checks
/// changes what a client is told to fix while leaving the status code and all six
/// rejection tests above green, which is why this pins the order rather than each
/// rule alone.
#[test]
fn the_first_failing_rule_is_the_one_reported() {
    let all_wrong = OverviewQuery {
        product_id: String::new(),
        version: String::new(),
        scope: "nonsense".to_owned(),
        group_by: Some("nonsense".to_owned()),
        ..OverviewQuery::default()
    };
    assert_eq!(refuse(&all_wrong).0, "product_id");

    // Each step fixes exactly the field the previous step was told about, so the
    // sequence walks legacy's order rather than asserting four independent facts.
    let version_first = OverviewQuery {
        product_id: "vhp".to_owned(),
        ..all_wrong
    };
    assert_eq!(refuse(&version_first).0, "version");

    let scope_first = OverviewQuery {
        version: "5.0.1".to_owned(),
        ..version_first
    };
    assert_eq!(
        refuse(&scope_first).0,
        "scope",
        "scope is parsed before group_by (`:2408` before `:2409`)",
    );

    let group_first = OverviewQuery {
        scope: "plan".to_owned(),
        ..scope_first
    };
    assert_eq!(
        refuse(&group_first).0,
        "group_by",
        "group_by is parsed before the plan-scope rule (`:2409` before `:2413`), \
         so a plan scope with no plan_id and a bad group is told about the group",
    );
}

// ---------------------------------------------------------------------------
// What is deliberately accepted
// ---------------------------------------------------------------------------

/// Both scopes and all four groupings parse, case-insensitively and with
/// surrounding whitespace, and an absent `group_by` is `none`.
///
/// The positive half of the two parsers. `?scope=%20PLAN%20` reaching a 400 would
/// be a break in the other direction, and `to_ascii_lowercase` on a trimmed value
/// is what legacy does (`:2105`, `:2116`).
#[test]
fn the_vocabularies_parse_case_insensitively_and_trimmed() {
    assert_eq!(parse_scope(" All ").unwrap(), Scope::All);
    assert_eq!(parse_scope("PLAN").unwrap(), Scope::Plan);
    for (raw, expected) in [
        (" None ", GroupBy::None),
        ("COMPONENT", GroupBy::Component),
        ("Tag", GroupBy::Tag),
        (" environment", GroupBy::Environment),
    ] {
        assert_eq!(parse_group(Some(raw)).unwrap(), expected, "for {raw:?}");
    }
}

/// The three optional strings are trimmed and blank-to-`None`, and **nothing
/// rejects them** — `normalize_optional` (`:2070-2075`) applied at `:2410`,
/// `:2412` and `:2425`.
///
/// `branch: None` is every branch here, the opposite of what it means on the
/// universe side; `group_value: None` narrows nothing rather than matching
/// nothing. Both are recorded on the fields they feed, and this asserts that a
/// blank arrives as `None` so those two rules see the case they document.
#[test]
fn the_optional_strings_are_trimmed_and_blank_becomes_absent() {
    let query = OverviewQuery {
        plan_id: Some("  plans/smoke.yaml ".to_owned()),
        branch: Some("   ".to_owned()),
        group_by: Some("component".to_owned()),
        group_value: Some(" cluster ".to_owned()),
        ..valid()
    };

    let normalized = accept(&query);

    assert_eq!(normalized.plan_id.as_deref(), Some("plans/smoke.yaml"));
    assert_eq!(normalized.branch, None, "blank means every branch");
    assert_eq!(normalized.group_value.as_deref(), Some("cluster"));
    assert_eq!(normalized.group_by, GroupBy::Component);
}

/// A `plan_id` on an `all` scope is **carried, not rejected** (`:2412` normalizes
/// it unconditionally and `:2413`'s rule only fires for `Scope::Plan`).
///
/// Legacy simply never reads it in that case. A parser that cleared it would be
/// tidier and would change what the endpoint echoes back to the client — the
/// response's eight non-computed fields include the query.
#[test]
fn a_plan_id_on_an_all_scope_is_carried_rather_than_cleared() {
    let query = OverviewQuery {
        scope: "all".to_owned(),
        plan_id: Some("plans/smoke.yaml".to_owned()),
        ..valid()
    };

    let normalized = accept(&query);

    assert_eq!(normalized.scope, Scope::All);
    assert_eq!(normalized.plan_id.as_deref(), Some("plans/smoke.yaml"));
}

/// The two day counts default to `7` and `90` and clamp to `[1, 30]` and
/// `[7, 365]` — legacy's `:2426` and `:2427`.
///
/// **The two clamps differ at both ends and sharing one would change both
/// charts**, which is the property `aggregates::heatmap_days`' header states:
/// `90` is a legal trend window and clamps to `30` here, and `1` is a legal
/// heatmap window and clamps to `7` there. The fixtures straddle all four
/// boundaries plus the defaults, so a swapped pair of calls fails on every row.
#[test]
fn the_day_counts_default_and_clamp_the_way_legacy_clamps_them() {
    let defaults = accept(&valid());
    assert_eq!((defaults.days_heatmap, defaults.days_trend), (7, 90));

    for (heatmap, trend, expected) in [
        (Some(0), Some(0), (1, 7)),
        (Some(1), Some(1), (1, 7)),
        (Some(30), Some(30), (30, 30)),
        (Some(90), Some(90), (30, 90)),
        (Some(365), Some(365), (30, 365)),
        (Some(u32::MAX), Some(u32::MAX), (30, 365)),
    ] {
        let query = OverviewQuery {
            days_heatmap: heatmap,
            days_trend: trend,
            ..valid()
        };
        let normalized = accept(&query);
        assert_eq!(
            (normalized.days_heatmap, normalized.days_trend),
            expected,
            "for {heatmap:?}/{trend:?}",
        );
    }
}

/// `product_id` and `version` are carried **trimmed**.
///
/// `:2422-2423` copy the already-trimmed locals rather than the raw fields, so
/// `?version=%205.0.1%20` becomes the predicate `product_version = '5.0.1'`
/// rather than one that matches nothing. A rule that only *checked* the trimmed
/// value and carried the raw one would pass every rejection test above.
#[test]
fn the_required_strings_are_carried_trimmed() {
    let query = OverviewQuery {
        product_id: "  vhp ".to_owned(),
        version: " 5.0.1\t".to_owned(),
        ..valid()
    };

    let normalized = accept(&query);

    assert_eq!(normalized.product_id, "vhp");
    assert_eq!(normalized.version, "5.0.1");
}
