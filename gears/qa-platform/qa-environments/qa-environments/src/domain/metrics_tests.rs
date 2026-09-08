//! The catalog's naming rules, and the closedness and agreement of the label
//! taxonomy.
//!
//! Both properties are the kind a reader can only check by reading, and both
//! fail silently: a metric whose rendered name differs from the constant still
//! exports, it just exports under a name no dashboard queries, and a label
//! value that widens the cardinality surface still emits, it just makes the
//! series unbounded. Neither shows up as an error anywhere.
//!
//! Shape copied from qa-runs' and qa-insights' `domain::metrics::tests`,
//! including the two tests qa-runs' own review round added (the injectivity
//! sweep and the same-stem equality).

use std::collections::BTreeSet;
use std::time::Duration;

use qa_product_sdk::observation::FailureClass;
use uuid::Uuid;

use super::{COUNTERS, DURATIONS};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{
    CycleOutcome, EnvironmentOutcome, NoopMetrics, ObservationClass, ObservationMetrics,
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
/// `observation_total` collides.
#[test]
fn every_metric_is_namespaced_to_this_gear() {
    for name in COUNTERS.iter().chain(DURATIONS) {
        assert!(
            name.starts_with("qa_environments_"),
            "{name} is not namespaced"
        );
    }
}

/// **No label carries an environment id or name, a tenant id, a cluster URL or
/// anything read out of a kubeconfig.**
///
/// Cardinality *and* disclosure, and in this gear the disclosure half is the
/// live one: an environment's identity and its cluster's address are exactly
/// what this gear spends most of its code keeping out of anything a caller can
/// read. This subsystem's labels are all closed enums, so the property is
/// structural — a free `&str` label would not compile against this trait — and
/// this test documents that it is deliberate.
#[test]
fn label_taxonomies_are_closed_sets() {
    // Each `as_str` is total over a closed enum; a value outside it is
    // unconstructible. What this body checks is that every enumerated variant
    // renders to something — the closedness itself is the type system's, and
    // `no_label_value_collides_within_its_own_enum` is what checks the
    // rendering is injective.
    for outcome in CycleOutcome::ALL {
        assert!(!outcome.as_str().is_empty());
    }
    for outcome in EnvironmentOutcome::ALL {
        assert!(!outcome.as_str().is_empty());
    }
    for class in ObservationClass::ALL {
        assert!(!class.as_str().is_empty());
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
/// # What the pairing buys, and what it buys only for the cycle
///
/// For the **cycle** pair the two instruments carry the same label, so a p95
/// read off the histogram and a failure rate read off the counter are two
/// queries over one partition — which is what a quantile alert is normally
/// paired with.
///
/// For the **per-environment** pair that is not what the stem-mate provides,
/// and saying it did would be false: the histogram is labelled by `class` and
/// its stem-mate counter by `outcome`, so no per-class rate exists on that
/// counter at all. The per-class rate is the histogram's own per-label count,
/// exactly as `ObservationMetrics::environment_observed`'s doc says, and the
/// counter is there because it is the report's own two numbers. **The naming
/// rule is still worth enforcing across both pairs**, for the narrower reason
/// that a stem shared by a counter and a histogram is how a reader (and a
/// dashboard's autocomplete) finds the two halves of one subject; it just is
/// not a claim that either half answers the other's question.
///
/// The match is an equality rather than a prefix test: every family in this
/// gear shares the `qa_environments_observation` prefix, so a `starts_with`
/// form would be satisfied by a *different* family and would assert nothing at
/// all here.
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
/// literally, and an `AuthRejected` next to an `auth_rejected` is two series.
#[test]
fn every_label_value_is_lower_snake_case() {
    let values = CycleOutcome::ALL
        .iter()
        .map(|v| v.as_str())
        .chain(EnvironmentOutcome::ALL.iter().map(|v| v.as_str()))
        .chain(ObservationClass::ALL.iter().map(|v| v.as_str()));
    for value in values {
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
/// The `ALL` arrays are hand-maintained (see [`COUNTERS`]'s own caveat), and so
/// is every `as_str` match — a copy-pasted arm returning its neighbour's string
/// compiles, passes every other test here, and silently sums two variants into
/// one series. Swept over all three enums rather than one.
#[test]
fn no_label_value_collides_within_its_own_enum() {
    fn distinct(values: &[&'static str], enum_name: &str) {
        let seen: BTreeSet<&str> = values.iter().copied().collect();
        assert_eq!(
            seen.len(),
            values.len(),
            "{enum_name} renders two variants to the same label: {values:?}"
        );
    }

    distinct(&CycleOutcome::ALL.map(CycleOutcome::as_str), "CycleOutcome");
    distinct(
        &EnvironmentOutcome::ALL.map(EnvironmentOutcome::as_str),
        "EnvironmentOutcome",
    );
    distinct(
        &ObservationClass::ALL.map(ObservationClass::as_str),
        "ObservationClass",
    );
}

/// **Every `FailureClass` the plugin contract declares has a label value of its
/// own, and no two share one.**
///
/// [`ObservationClass`] is a projection of `qa_product_sdk`'s `FailureClass`,
/// not a second taxonomy: the point of labelling by it is that the gear already
/// reasons in it. A class folded into a neighbour here would be a failure mode
/// an operator can see in the environment's own row and cannot see in any
/// series.
///
/// [`every_failure_class`] supplies the values, and **its completeness is this
/// test's premise rather than its subject** — see its own header for which
/// links in that chain are held and which one is not.
#[test]
fn every_failure_class_projects_to_its_own_label() {
    let projected: BTreeSet<&str> = every_failure_class()
        .iter()
        .map(|class| ObservationClass::from(*class).as_str())
        .collect();
    assert_eq!(
        projected.len(),
        every_failure_class().len(),
        "two FailureClass variants project to the same label value: {projected:?}"
    );
    assert!(
        !projected.contains(ObservationClass::Detected.as_str()),
        "no failure class may project onto the success value"
    );
}

/// One of every `FailureClass` variant.
///
/// # What holds it, link by link, and which link is not held
///
/// This list is written out by hand — the compiler cannot enumerate an enum's
/// variants, and `FailureClass` lives in `qa-product-sdk`, which declares no
/// `ALL` of its own.
///
/// 1. **A variant added to `FailureClass` is a compile error in
///    [`failure_class_index`]** and in `From<FailureClass> for
///    ObservationClass`, neither of which has a `_` arm. Both land the author
///    of that variant in code they must classify, and one of the two is three
///    lines above this array.
/// 2. **The array's length is [`FAILURE_CLASS_VARIANTS`]**, so adding a value
///    without raising the count, or raising the count without adding a value,
///    does not compile. That link is the compiler's.
/// 3. **[`the_sweep_above_covers_every_failure_class`] asserts the numbers this
///    array yields are exactly `0..`[`FAILURE_CLASS_VARIANTS`]**, so a
///    duplicated entry cannot stand in for a missing one and a renumbering that
///    leaves a hole fails.
/// 4. **Nothing forces [`FAILURE_CLASS_VARIANTS`] to equal the number of
///    variants the enum actually has.** An author who classifies a new variant
///    in link 1 and stops there leaves this sweep silently one variant
///    narrower, and every test here still passes. That link cannot be closed
///    from inside a test in stable Rust: no test can observe a variant nobody
///    constructed, `std::mem::variant_count` is nightly-only, and a derive
///    macro for one array is a larger thing than the array. qa-insights' own
///    catalog carries the identical chain and the identical gap, having found
///    it the hard way; it is written down here rather than argued away.
const fn every_failure_class() -> [FailureClass; FAILURE_CLASS_VARIANTS] {
    [
        FailureClass::Unreachable,
        FailureClass::AuthRejected,
        FailureClass::NotFound,
        FailureClass::Malformed,
        FailureClass::Timeout,
        FailureClass::Internal,
    ]
}

/// How many variants `FailureClass` has, which is also the length of
/// [`every_failure_class`]'s array.
///
/// Bumping this without adding a value to that array is a **compile** error:
/// the array literal would then be one element short of its declared length.
/// That is the one link in the chain the compiler holds on its own.
const FAILURE_CLASS_VARIANTS: usize = 6;

/// A number per `FailureClass` variant, in declaration order.
///
/// **Exhaustive with no `_` arm on purpose**: a variant added to `FailureClass`
/// does not compile here until somebody numbers it, which is what puts the
/// author of that variant in this file, next to the array they also have to
/// extend.
///
/// The numbers mean nothing beyond being distinct and contiguous from zero. The
/// last arm is written in terms of [`FAILURE_CLASS_VARIANTS`] rather than as a
/// literal so that raising the count *moves* it — leaving a hole in the middle
/// of the range that [`the_sweep_above_covers_every_failure_class`] reports —
/// instead of sitting quietly at a number a stale count still agrees with.
const fn failure_class_index(class: FailureClass) -> usize {
    match class {
        FailureClass::Unreachable => 0,
        FailureClass::AuthRejected => 1,
        FailureClass::NotFound => 2,
        FailureClass::Malformed => 3,
        FailureClass::Timeout => 4,
        FailureClass::Internal => FAILURE_CLASS_VARIANTS - 1,
    }
}

/// **The sweep's array numbers exactly `0..`[`FAILURE_CLASS_VARIANTS`], once
/// each.**
///
/// [`every_failure_class_projects_to_its_own_label`] iterates a hand-written
/// array, so its coverage is whatever that array happens to hold. This checks
/// the two ways that array can be wrong *without the count also being wrong*: a
/// duplicated entry standing in for a missing one, and a renumbering in
/// [`failure_class_index`] that leaves a hole. It compares the *set* of
/// numbers, not the count of them, which is what makes the first of those
/// visible.
///
/// It would **not** catch a variant added to `FailureClass` and left out of the
/// array with the count left alone — see link 4 of [`every_failure_class`]'s
/// chain. What catches that is the compile error in [`failure_class_index`].
#[test]
fn the_sweep_above_covers_every_failure_class() {
    let swept: BTreeSet<usize> = every_failure_class()
        .into_iter()
        .map(failure_class_index)
        .collect();
    let expected: BTreeSet<usize> = (0..FAILURE_CLASS_VARIANTS).collect();
    assert_eq!(
        swept, expected,
        "every_failure_class must carry one of each FailureClass variant; \
         a number missing here is a class the label sweep never sees"
    );
}

/// **The fine per-environment taxonomy rolls up to the coarse one the report
/// emits, with nothing left over.**
///
/// [`QA_ENVIRONMENTS_OBSERVATION`] is labelled by [`EnvironmentOutcome`] and
/// driven from `ObservationCycleReport`'s own two counters;
/// [`super::QA_ENVIRONMENTS_OBSERVATION_DURATION`] is labelled by the
/// nine-valued [`ObservationClass`] and emitted per environment. Two partitions
/// of the same events is exactly the shape that drifts, so the relationship is
/// asserted rather than described: every class maps to one outcome, and the two
/// halves of that map are both non-empty, which is what makes the roll-up a
/// refinement rather than a constant function.
///
/// The **direction** matters and is why this is not a tautology: an observation
/// counts as `observed` exactly when `observe_environment` returned `Ok`, and
/// every plugin-reported failure — a cluster that could not be reached, a
/// credential the cluster refused — is an `Ok` with a failure *value* recorded
/// on the row. `EnvironmentsService::observe_environment`'s own doc calls that
/// out as the gear's central rule about observation, and a label taxonomy that
/// quietly disagreed with it would report those environments as this gear's
/// failures.
#[test]
fn the_two_environment_taxonomies_agree_on_what_counts_as_observed() {
    let mut observed = Vec::new();
    let mut failed = Vec::new();
    for class in ObservationClass::ALL {
        if class.counts_as_observed() {
            observed.push(class);
        } else {
            failed.push(class);
        }
    }

    assert!(
        observed.contains(&ObservationClass::Detected),
        "a detection that succeeded must roll up to the report's observed count"
    );
    for class in every_failure_class() {
        assert!(
            ObservationClass::from(class).counts_as_observed(),
            "a plugin-reported failure is a persisted observation, not a failure of \
             the cycle; {class:?} must roll up to observed"
        );
    }
    assert_eq!(
        failed,
        vec![ObservationClass::Refused, ObservationClass::Failed],
        "only an Err out of observe_environment may roll up to the report's failed count"
    );

    assert_eq!(
        EnvironmentOutcome::ALL.len(),
        2,
        "the coarse taxonomy has exactly the report's two counters"
    );
}

/// **A policy refusal and this gear's own failure are told apart.**
///
/// The two live in one [`ObservationClass`] pair because they want opposite
/// responses: a `Forbidden` out of the observation cycle means the deployment's
/// PDP has no policy for this gear's system actor — which
/// `run_observation_cycle`'s own doc warns about by name — and is fixed by
/// authoring policy, while a database or credstore failure is an incident.
///
/// The split matches the three variants `api::rest::error` renders as an opaque
/// 500 (`CredStore`, `Database`, `Internal`), which is this gear's existing
/// answer to *whose failure was this*. **The cross-check qa-insights writes for
/// the same property cannot be written here**: it compares the label against
/// `CanonicalError::from(error).status_code()`, and `no_api_in_domain_tests`
/// forbids any module under `src/domain` from naming `crate::api`. It is
/// written on the other side of that boundary instead —
/// `api::rest::error`'s `the_metric_label_agrees_with_what_the_api_may_disclose`.
#[test]
fn a_policy_refusal_and_an_internal_failure_are_told_apart() {
    assert_eq!(
        ObservationClass::from(&DomainError::Forbidden),
        ObservationClass::Refused
    );
    assert_eq!(
        ObservationClass::from(&DomainError::Internal("boom".to_owned())),
        ObservationClass::Failed
    );
    assert_eq!(
        ObservationClass::from(&DomainError::database("boom")),
        ObservationClass::Failed
    );
    assert_eq!(
        ObservationClass::from(&DomainError::CredStore("sealed".to_owned())),
        ObservationClass::Failed
    );
    assert_eq!(
        ObservationClass::from(&DomainError::EnvironmentNotFound { id: Uuid::nil() }),
        ObservationClass::Refused,
        "an environment deleted between the enumeration and its own observation is \
         not an incident; see ObservationClass::Refused's own doc"
    );
}

/// **Emitting with no adapter installed does nothing and cannot fail.**
///
/// The trait methods return `()` and take `&self`, so there is no error for a
/// caller to handle and no interior state for an implementation to poison.
/// [`NoopMetrics`] is what "silent until an adapter is installed" means before
/// one is: a service holding it emits every signal the wired service does and
/// observes nothing.
#[test]
fn the_no_op_port_accepts_every_emission_and_returns_nothing() {
    let metrics = NoopMetrics;
    for outcome in CycleOutcome::ALL {
        metrics.observation_cycle(outcome, Duration::from_millis(1));
    }
    for outcome in EnvironmentOutcome::ALL {
        metrics.cycle_environments(outcome, 3);
    }
    for class in ObservationClass::ALL {
        metrics.environment_observed(class, Duration::from_millis(1));
    }
}
