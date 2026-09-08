//! The catalog's naming rules, and the closedness of the label taxonomy.
//!
//! Both properties are the kind a reader can only check by reading, and both
//! fail silently: a metric whose rendered name differs from the constant still
//! exports, it just exports under a name no dashboard queries, and a label
//! value that widens the cardinality surface still emits, it just makes the
//! series unbounded. Neither shows up as an error anywhere.
//!
//! Shape copied from qa-runs', qa-insights' and qa-environments'
//! `domain::metrics::tests`, including the two tests qa-runs' own review round
//! added (the injectivity sweep and the same-stem equality).

use std::collections::BTreeSet;
use std::time::Duration;

use uuid::Uuid;

use super::{COUNTERS, DURATIONS};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{
    NoopMetrics, PluginResolutionMetrics, PluginResolutionOutcome,
};

/// **Every metric constant is the literal Prometheus series name.**
///
/// The OTel→Prometheus translation adds `_total` to counters and a unit suffix
/// to instruments carrying a `.with_unit()` hint. Baking the suffix into the
/// constant and setting no unit hint makes the rendered name identical whether
/// the collector has `add_metric_suffixes` on or off.
///
/// A name that gets a suffix added at render time is a name that does not match
/// the dashboard query written against it.
#[test]
fn counter_names_carry_the_total_suffix_and_duration_names_carry_the_unit() {
    for name in COUNTERS {
        assert!(
            name.ends_with("_total"),
            "{name} is a counter and must carry the _total the exporter would add"
        );
    }
    for name in DURATIONS {
        assert!(
            name.ends_with("_seconds"),
            "{name} measures a duration and must carry its unit"
        );
    }
}

/// **Every metric name is prefixed with its gear.**
///
/// One Prometheus instance holds all four qa-platform gears; an unprefixed
/// `plugin_resolution_total` collides — and this family in particular has a
/// near-namesake one gear over, qa-environments' plugin-call pair, which
/// measures the *other* side of the same boundary.
#[test]
fn every_metric_is_namespaced_to_this_gear() {
    for name in COUNTERS.iter().chain(DURATIONS) {
        assert!(name.starts_with("qa_catalog_"), "{name} is not namespaced");
    }
}

/// **No label carries a product id, a tenant id, or a plugin instance id.**
///
/// Cardinality, and — for the third of those — an argument that had to be made
/// rather than assumed, because the instance id is right there in the resolved
/// value. It is a string this gear reads out of a database column, so as a
/// label it is bounded by nothing the type system can see. The taxonomy is a
/// closed enum with a total `as_str` and no `&str`-taking constructor, so the
/// property is structural — a free `&str` label would not compile against this
/// trait — and this test documents that it is deliberate.
///
/// See [`crate::domain::ports::metrics`]'s header for the whole argument,
/// including why the plugin's GTS *type* id is not a label either.
#[test]
fn label_taxonomies_are_closed_sets() {
    // `as_str` is total over a closed enum; a value outside it is
    // unconstructible. What this body checks is that every enumerated variant
    // renders to something — the closedness itself is the type system's, and
    // `no_label_value_collides_within_its_own_enum` is what checks the
    // rendering is injective.
    for outcome in PluginResolutionOutcome::ALL {
        assert!(!outcome.as_str().is_empty());
    }
}

/// **Two families never share a series name.**
///
/// The naming rules above are each satisfiable by a duplicate: two counters
/// both ending `_total` and both prefixed pass every other assertion here while
/// silently summing two unrelated signals into one series.
#[test]
fn no_two_metric_families_share_a_name() {
    let all: Vec<&str> = COUNTERS.iter().chain(DURATIONS).copied().collect();
    let distinct: BTreeSet<&str> = all.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        all.len(),
        "duplicate series name in {all:?}"
    );
}

/// **Every duration family names a counter family of the same stem.**
///
/// The two instruments carry the same label here, so a p95 read off the
/// histogram and a failure rate read off the counter are two queries over one
/// partition — which is what a quantile alert is normally paired with. An
/// orphan histogram cannot serve one.
///
/// The match is an equality rather than a prefix test: a `starts_with` form is
/// satisfiable by a *different* family that happens to share a prefix, which is
/// exactly what every family in a one-subject catalog like this one does.
#[test]
fn each_duration_family_has_a_counter_of_the_same_stem() {
    for duration in DURATIONS {
        let stem = duration
            .strip_suffix("_duration_seconds")
            .unwrap_or(duration);
        let counter = format!("{stem}_total");
        assert!(
            COUNTERS.contains(&counter.as_str()),
            "{duration} has no counter family named {counter}"
        );
    }
}

/// **Label values are lower snake case.**
///
/// Not cosmetic: the catalog's own names are, dashboards match on them
/// literally, and a `ProductNotFound` next to a `product_not_found` is two
/// series.
#[test]
fn every_label_value_is_lower_snake_case() {
    for value in PluginResolutionOutcome::ALL.map(PluginResolutionOutcome::as_str) {
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "{value} is not lower snake case"
        );
    }
}

/// **Within one enum, no two variants render to the same label value.**
///
/// The `ALL` array is hand-maintained (see [`COUNTERS`]'s own caveat), and so
/// is every `as_str` match — a copy-pasted arm returning its neighbour's string
/// compiles, passes every other test here, and silently sums two variants into
/// one series.
#[test]
fn no_label_value_collides_within_its_own_enum() {
    let values = PluginResolutionOutcome::ALL.map(PluginResolutionOutcome::as_str);
    let seen: BTreeSet<&str> = values.iter().copied().collect();
    assert_eq!(
        seen.len(),
        values.len(),
        "PluginResolutionOutcome renders two variants to the same label: {values:?}"
    );
}

/// **A deployment missing a plugin gear is told apart from a policy refusal and
/// from a broken database.**
///
/// The three answer to different people. `unregistered` means this binary does
/// not carry the gear that registers the product's plugin — a deployment
/// composition mistake, fixed by shipping the gear, and today visible only as a
/// `warn!` line inside `plugin_for`. `refused` is the caller's own policy or a
/// product it cannot see. `failed` is this gear's database or internals, and is
/// the one an alert fires on.
///
/// The refused/failed split matches what `api::rest::error` is willing to
/// disclose, which is this gear's existing answer to *whose failure was this*.
/// **The cross-check qa-insights writes for the same property cannot be written
/// here**: it compares the label against `CanonicalError::from(error)`'s status,
/// and `no_api_in_domain_tests` forbids any module under `src/domain` from
/// naming `crate::api`. It is written on the other side of that boundary
/// instead — `api::rest::error`'s
/// `the_metric_label_agrees_with_what_the_api_may_disclose`.
#[test]
fn the_three_reasons_a_resolution_can_end_without_a_plugin_are_told_apart() {
    assert_eq!(
        PluginResolutionOutcome::from(&DomainError::ProductPluginUnavailable {
            product_id: Uuid::nil(),
            instance_id: "gts.a.b.v1~c.d.v1".to_owned(),
        }),
        PluginResolutionOutcome::Unregistered,
        "a product naming a plugin this binary does not register is a deployment \
         composition mistake, not a refusal and not a fault"
    );
    assert_eq!(
        PluginResolutionOutcome::from(&DomainError::Forbidden),
        PluginResolutionOutcome::Refused
    );
    assert_eq!(
        PluginResolutionOutcome::from(&DomainError::NotFound { id: Uuid::nil() }),
        PluginResolutionOutcome::Refused,
        "a product outside the caller's tenant reads as absent, which is a fact about \
         the caller and not about this gear"
    );
    assert_eq!(
        PluginResolutionOutcome::from(&DomainError::Internal("boom".to_owned())),
        PluginResolutionOutcome::Failed
    );
    assert_eq!(
        PluginResolutionOutcome::from(&DomainError::database("boom")),
        PluginResolutionOutcome::Failed
    );
    assert_eq!(
        PluginResolutionOutcome::from(&DomainError::CredStore("sealed".to_owned())),
        PluginResolutionOutcome::Failed
    );
}

/// **Emitting with no adapter installed does nothing and cannot fail.**
///
/// The trait method returns `()` and takes `&self`, so there is no error for a
/// caller to handle and no interior state for an implementation to poison.
/// [`NoopMetrics`] is what "silent until an adapter is installed" means before
/// one is: a registry holding it emits every signal the wired registry does and
/// observes nothing.
#[test]
fn the_no_op_port_accepts_every_emission_and_returns_nothing() {
    let metrics = NoopMetrics;
    for outcome in PluginResolutionOutcome::ALL {
        metrics.plugin_resolution(outcome, Duration::from_millis(1));
    }
}
