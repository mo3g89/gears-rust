//! The catalog's naming rules and the label taxonomy's closedness.
//!
//! Both properties are the kind that a reader can only check by reading, and
//! both fail silently: a metric whose rendered name differs from the constant
//! still exports, it just exports under a name no dashboard queries, and a
//! label value that widens the cardinality surface still emits, it just makes
//! the series unbounded. Neither shows up as an error anywhere.

use std::collections::BTreeSet;

use qa_runs_sdk::RunState;
use uuid::Uuid;

use super::{COUNTERS, DURATIONS, QA_RUNS_DISPATCH, QA_RUNS_INGEST};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{
    DispatchDecision, DispatchMetrics, DispatchOutcome, IngestMetrics, IngestOutcome, NoopMetrics,
};
use crate::domain::service::launch::Admission;
use crate::domain::state_machine::TERMINAL_STATES;

/// **Every metric constant is the literal Prometheus series name.**
///
/// The OTel→Prometheus translation adds `_total` to counters and a unit suffix
/// to instruments carrying a `.with_unit()` hint. Baking the suffix into the
/// constant and setting no unit hint makes the rendered name identical whether
/// the collector has `add_metric_suffixes` on or off -- the mechanism
/// account-management's `domain::metrics` documents in its module header under
/// *Metric naming*, and the reason its constants look the way they do.
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
/// One Prometheus instance holds all four gears; an unprefixed
/// `dispatch_total` collides.
#[test]
fn every_metric_is_namespaced_to_this_gear() {
    for name in COUNTERS.iter().chain(DURATIONS) {
        assert!(name.starts_with("qa_runs_"), "{name} is not namespaced");
    }
}

/// **No label carries a tenant id, a run name, a branch or a URL.**
///
/// Cardinality *and* disclosure: a metrics pipeline is a second copy of
/// whatever is put in a label, exported to a system with a different audience
/// from the database. This subsystem's labels are all closed enums, so this
/// asserts the property structurally -- a free `&str` label would not compile
/// against these traits, and this test documents why that is deliberate.
#[test]
fn label_taxonomies_are_closed_sets() {
    // Each `as_str` is total over a closed enum; a value outside it is
    // unconstructible. What this body checks is that every enumerated variant
    // renders to something -- the closedness itself is the type system's, and
    // `no_label_value_collides_within_its_own_enum` is what checks the
    // rendering is injective.
    for outcome in DispatchOutcome::ALL {
        assert!(!outcome.as_str().is_empty());
    }
    for decision in DispatchDecision::ALL {
        assert!(!decision.as_str().is_empty());
    }
    for outcome in IngestOutcome::ALL {
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

/// **Every duration family names a counter family.**
///
/// A histogram whose companion counter is missing cannot be turned into a
/// failure rate, which is the query every p95 alert is paired with.
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
/// literally, and a `TimedOut` next to a `timed_out` is two series.
#[test]
fn every_label_value_is_lower_snake_case() {
    let values = DispatchOutcome::ALL
        .iter()
        .map(|v| v.as_str())
        .chain(DispatchDecision::ALL.iter().map(|v| v.as_str()))
        .chain(IngestOutcome::ALL.iter().map(|v| v.as_str()));
    for value in values {
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "{value} is not lower snake case"
        );
    }
}

/// **The failure split is [`DomainError::disclosable`]'s split, not a second
/// one.**
///
/// This is the whole reason neither outcome enum carries a per-variant failure
/// taxonomy. `disclosable` already partitions this gear's errors into "the
/// caller's own request or their own row's state" and "text that originates
/// outside this gear or inside its internals", and it is exhaustive by
/// construction -- adding a variant is a compile error there. A metric-side
/// partition would have to agree with it forever, with nothing checking that it
/// still did.
#[test]
fn a_refusal_and_an_internal_failure_are_told_apart_by_the_disclosure_rule() {
    assert_eq!(
        DispatchOutcome::from(&DomainError::Forbidden),
        DispatchOutcome::Refused
    );
    assert_eq!(
        DispatchOutcome::from(&DomainError::Internal("boom".to_owned())),
        DispatchOutcome::Failed
    );
    assert_eq!(
        IngestOutcome::from(&DomainError::Forbidden),
        IngestOutcome::Refused
    );
    assert_eq!(
        IngestOutcome::from(&DomainError::Internal("boom".to_owned())),
        IngestOutcome::Failed
    );
}

/// **A terminal state completes an ingest pass; anything else applies one.**
///
/// The oracle is written out below rather than read from
/// [`crate::domain::state_machine::is_terminal`], which is what the
/// implementation derives from — a test that called the same function would
/// assert only that the function equals itself.
///
/// [`expected_outcome`]'s match has no `_` arm, so a state added to
/// [`RunState`] does not compile until somebody classifies it here. The
/// cross-check against [`TERMINAL_STATES`] catches one half of the other
/// defect — a **terminal** state left out of [`EVERY_RUN_STATE`], which would
/// make the completed count come up short. It says nothing about a **live**
/// state left out: dropping one changes neither side of that comparison. What
/// catches that is [`EVERY_RUN_STATE`]'s own `[RunState; 10]` type annotation,
/// which is a compile error one element short.
#[test]
fn a_terminal_state_completes_the_pass_and_a_live_one_applies_it() {
    for state in EVERY_RUN_STATE {
        assert_eq!(
            IngestOutcome::from(state),
            expected_outcome(state),
            "{}",
            state.as_str()
        );
    }

    let completed = EVERY_RUN_STATE
        .into_iter()
        .filter(|state| expected_outcome(*state) == IngestOutcome::Completed)
        .count();
    assert_eq!(
        completed,
        TERMINAL_STATES.len(),
        "the state list has drifted from the state machine's terminal set"
    );
}

/// Every state a run can be in.
///
/// Hand-maintained, and its own oracle in the way
/// [`crate::domain::metrics::COUNTERS`] is — the compiler cannot enumerate an
/// enum. What keeps it honest is the pair of checks in
/// [`a_terminal_state_completes_the_pass_and_a_live_one_applies_it`]: a new
/// state fails to compile in [`expected_outcome`], and a terminal state missing
/// from this list fails the count.
const EVERY_RUN_STATE: [RunState; 10] = [
    RunState::Created,
    RunState::Queued,
    RunState::Dispatching,
    RunState::Running,
    RunState::Succeeded,
    RunState::Failed,
    RunState::Canceled,
    RunState::TimedOut,
    RunState::Expired,
    RunState::Error,
];

/// Which outcome each state should produce, spelled out.
///
/// Exhaustive with no `_` arm on purpose — the same device
/// [`DomainError::disclosable`] uses, and for the same reason: a state added
/// later gets classified by whoever adds it rather than inheriting a default.
fn expected_outcome(state: RunState) -> IngestOutcome {
    match state {
        RunState::Created | RunState::Queued | RunState::Dispatching | RunState::Running => {
            IngestOutcome::Applied
        }
        RunState::Succeeded
        | RunState::Failed
        | RunState::Canceled
        | RunState::TimedOut
        | RunState::Expired
        | RunState::Error => IngestOutcome::Completed,
    }
}

/// **Each admission outcome has its own decision label.**
///
/// [`Admission`] is the enum the launch path already decides; the label is a
/// projection of it, so a fourth admission outcome is a compile error in the
/// `From` impl rather than a silently unlabelled emission.
#[test]
fn every_admission_projects_to_its_own_decision_label() {
    let queue_id = Uuid::nil();
    assert_eq!(
        DispatchDecision::from(&Admission::Dispatch { queue_id }),
        DispatchDecision::Inline
    );
    assert_eq!(
        DispatchDecision::from(&Admission::Queued { queue_id }),
        DispatchDecision::Queued
    );
    assert_eq!(
        DispatchDecision::from(&Admission::Unqueued),
        DispatchDecision::Unqueued
    );
}

/// **Within one enum, no two variants render to the same label value.**
///
/// The `ALL` arrays are hand-maintained (see [`COUNTERS`]'s own caveat), and so
/// is every `as_str` match — a copy-pasted arm returning its neighbour's string
/// compiles, passes every other test here, and silently sums two variants into
/// one series. Swept over all three enums rather than one: this file is the
/// template the other three gears copy, so a gap here propagates.
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

    distinct(
        &DispatchOutcome::ALL.map(DispatchOutcome::as_str),
        "DispatchOutcome",
    );
    distinct(
        &DispatchDecision::ALL.map(DispatchDecision::as_str),
        "DispatchDecision",
    );
    distinct(
        &IngestOutcome::ALL.map(IngestOutcome::as_str),
        "IngestOutcome",
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
    for outcome in DispatchOutcome::ALL {
        metrics.dispatch_pass(outcome, std::time::Duration::from_millis(1));
    }
    for decision in DispatchDecision::ALL {
        metrics.dispatch_decision(decision);
    }
    for outcome in IngestOutcome::ALL {
        metrics.ingest_batch(outcome, std::time::Duration::from_millis(1));
    }
    assert_eq!(QA_RUNS_DISPATCH, "qa_runs_dispatch_total");
    assert_eq!(QA_RUNS_INGEST, "qa_runs_ingest_total");
}
