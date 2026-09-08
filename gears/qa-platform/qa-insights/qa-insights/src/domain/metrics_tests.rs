//! The catalog's naming rules and the label taxonomy's closedness.
//!
//! Both properties are the kind a reader can only check by reading, and both
//! fail silently: a metric whose rendered name differs from the constant still
//! exports, it just exports under a name no dashboard queries, and a label
//! value that widens the cardinality surface still emits, it just makes the
//! series unbounded. Neither shows up as an error anywhere.
//!
//! Shape copied from qa-runs' `domain::metrics::tests`, including the two
//! tests its own review round added (the injectivity sweep and the
//! same-stem equality).

use std::collections::BTreeSet;

use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use super::{
    COUNTERS, DURATIONS, QA_INSIGHTS_COLLECT, QA_INSIGHTS_COLLECT_REPORT, QA_INSIGHTS_JIRA_BUG,
    QA_INSIGHTS_JIRA_POLL, QA_INSIGHTS_JIRA_RERUN,
};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{
    CollectMetrics, CollectOutcome, CollectReportOutcome, JiraBugOutcome, JiraPollMetrics,
    JiraPollOutcome, NoopMetrics,
};
use crate::domain::service::collect::SignatureRefusal;

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
/// `collect_total` collides.
#[test]
fn every_metric_is_namespaced_to_this_gear() {
    for name in COUNTERS.iter().chain(DURATIONS) {
        assert!(name.starts_with("qa_insights_"), "{name} is not namespaced");
    }
}

/// **No label carries a tenant id, a JIRA issue key, a bug summary, a run
/// name, a branch or a URL.**
///
/// Cardinality *and* disclosure, and in this gear the disclosure half is the
/// live one: the poller's inputs are JIRA issues, whose keys and summaries are
/// free text written by whoever filed the bug. This subsystem's labels are all
/// closed enums, so the property is structural — a free `&str` label would not
/// compile against these traits — and this test documents that it is
/// deliberate.
#[test]
fn label_taxonomies_are_closed_sets() {
    // Each `as_str` is total over a closed enum; a value outside it is
    // unconstructible. What this body checks is that every enumerated variant
    // renders to something — the closedness itself is the type system's, and
    // `no_label_value_collides_within_its_own_enum` is what checks the
    // rendering is injective.
    for outcome in CollectOutcome::ALL {
        assert!(!outcome.as_str().is_empty());
    }
    for outcome in CollectReportOutcome::ALL {
        assert!(!outcome.as_str().is_empty());
    }
    for outcome in JiraPollOutcome::ALL {
        assert!(!outcome.as_str().is_empty());
    }
    for outcome in JiraBugOutcome::ALL {
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
/// A histogram whose companion counter is missing cannot be turned into a
/// failure rate, which is the query every p95 alert is paired with. The match
/// is an equality rather than a prefix test: this gear has several families
/// sharing a prefix (three begin `qa_insights_jira_`), so a `starts_with`
/// form would be satisfied by a *different* family that happens to share one.
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
/// literally, and a `SignatureInvalid` next to a `signature_invalid` is two
/// series.
#[test]
fn every_label_value_is_lower_snake_case() {
    let values = CollectOutcome::ALL
        .iter()
        .map(|v| v.as_str())
        .chain(CollectReportOutcome::ALL.iter().map(|v| v.as_str()))
        .chain(JiraPollOutcome::ALL.iter().map(|v| v.as_str()))
        .chain(JiraBugOutcome::ALL.iter().map(|v| v.as_str()));
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
/// one series. Swept over all four enums rather than one.
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
        &CollectOutcome::ALL.map(CollectOutcome::as_str),
        "CollectOutcome",
    );
    distinct(
        &CollectReportOutcome::ALL.map(CollectReportOutcome::as_str),
        "CollectReportOutcome",
    );
    distinct(
        &JiraPollOutcome::ALL.map(JiraPollOutcome::as_str),
        "JiraPollOutcome",
    );
    distinct(
        &JiraBugOutcome::ALL.map(JiraBugOutcome::as_str),
        "JiraBugOutcome",
    );
}

/// **The refusal/failure split is the split the API boundary already makes.**
///
/// This gear has no `DomainError::disclosable` for the two `From<&DomainError>`
/// bridges to derive from, which is qa-runs' answer. What it has instead is
/// `From<DomainError> for CanonicalError`, whose match is exhaustive with no
/// `_` arm and whose last arm renders exactly three variants
/// (`CorruptState`, `Database`, `Internal`) through `opaque_internal` — the
/// group whose text "may not reach an HTTP body" because it originates in a
/// driver, in this gear's internals, or in a persisted column.
///
/// That is the same question a metric label asks: *was this our failure, or
/// the caller's request?* So this test does not restate the classification, it
/// **cross-checks the two partitions variant by variant**: an error is
/// [`CollectOutcome::Failed`] exactly when the canonical rendering is a 500.
/// A variant reclassified on one side and not the other fails here.
///
/// [`every_domain_error`] is what makes the sweep total; the bridges' own
/// matches have no `_` arm, so a variant added to [`DomainError`] fails to
/// compile there before it can reach this test.
#[test]
fn a_refusal_and_an_internal_failure_are_told_apart_by_what_the_api_may_disclose() {
    for error in every_domain_error() {
        let rendered = format!("{error:?}");
        let collect_failed = CollectOutcome::from(&error) == CollectOutcome::Failed;
        let poll_failed = JiraPollOutcome::from(&error) == JiraPollOutcome::Failed;
        // Consumes the error, which is why the two labels are read first:
        // `DomainError` is not `Clone` (its `Database` variant boxes a
        // `dyn Error`), so there is exactly one conversion per variant.
        let opaque = CanonicalError::from(error).status_code() == 500;
        assert_eq!(
            collect_failed, opaque,
            "the collect label disagrees with the canonical rendering for {rendered}"
        );
        assert_eq!(
            poll_failed, opaque,
            "the poll label disagrees with the canonical rendering for {rendered}"
        );
    }
}

/// One of every [`DomainError`] variant.
///
/// Hand-maintained, and its own oracle in the way
/// [`crate::domain::metrics::COUNTERS`] is — the compiler cannot enumerate an
/// enum's variants. What keeps it honest is that both `From<&DomainError>`
/// bridges match exhaustively with no `_` arm, so a variant added to
/// [`DomainError`] is a compile error in the bridge, at which point whoever
/// adds it is already editing the file this list guards.
///
/// A function rather than a `const`, because four variants own a `String` and
/// one boxes an error.
fn every_domain_error() -> [DomainError; 12] {
    [
        DomainError::RunNotIngested {
            run_id: Uuid::nil(),
        },
        DomainError::UnsupportedScope {
            resource: "qa.test_result",
        },
        DomainError::IngestConflict,
        DomainError::SavedViewNameExists {
            name: "v".to_owned(),
        },
        DomainError::SavedViewNotFound { id: Uuid::nil() },
        DomainError::JiraNotConfigured,
        DomainError::BugNotFound {
            key: "V-1".to_owned(),
        },
        DomainError::UnsupportedEgress {
            channel: "email".to_owned(),
        },
        DomainError::CorruptState {
            what: "scope",
            id: Uuid::nil(),
            value: "?".to_owned(),
        },
        DomainError::Validation {
            field: "branch".to_owned(),
            message: "required".to_owned(),
        },
        DomainError::Forbidden,
        DomainError::database("boom"),
    ]
}

/// **Every signature refusal path has its own label value.**
///
/// [`SignatureRefusal`] is the enum `CollectService::verify_signature` already
/// decides between; the label is a projection of it, so a fourth refusal path
/// is a compile error in the `From` impl rather than a silently unlabelled
/// emission. That the three are *distinct* is
/// [`no_label_value_collides_within_its_own_enum`]'s job; what this pins is
/// that each maps to the value naming its own path, because folding
/// `secret_unconfigured` into `signature_invalid` is exactly the
/// misclassification that would make a deployment-configuration mistake look
/// like an attack.
#[test]
fn every_signature_refusal_projects_to_its_own_label() {
    assert_eq!(
        CollectReportOutcome::from(SignatureRefusal::SecretUnconfigured),
        CollectReportOutcome::SecretUnconfigured
    );
    assert_eq!(
        CollectReportOutcome::from(SignatureRefusal::Malformed),
        CollectReportOutcome::SignatureMalformed
    );
    assert_eq!(
        CollectReportOutcome::from(SignatureRefusal::Mismatch),
        CollectReportOutcome::SignatureInvalid
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
    for outcome in CollectOutcome::ALL {
        metrics.collect_cycle(outcome, std::time::Duration::from_millis(1));
    }
    for outcome in CollectReportOutcome::ALL {
        metrics.collect_report(outcome);
    }
    for outcome in JiraPollOutcome::ALL {
        metrics.poll_pass(outcome, std::time::Duration::from_millis(1));
    }
    for outcome in JiraBugOutcome::ALL {
        metrics.bug(outcome);
    }
    metrics.auto_rerun();
    assert_eq!(QA_INSIGHTS_COLLECT, "qa_insights_collect_total");
    assert_eq!(
        QA_INSIGHTS_COLLECT_REPORT,
        "qa_insights_collect_report_total"
    );
    assert_eq!(QA_INSIGHTS_JIRA_POLL, "qa_insights_jira_poll_total");
    assert_eq!(QA_INSIGHTS_JIRA_BUG, "qa_insights_jira_bug_total");
    assert_eq!(QA_INSIGHTS_JIRA_RERUN, "qa_insights_jira_rerun_total");
}
