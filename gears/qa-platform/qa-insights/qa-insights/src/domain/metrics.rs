//! qa-insights observability metric catalog.
//!
//! Two of this gear's background paths are measured here: the collect cycle
//! (with the runner's signed callback beside it) and the JIRA poll. Before
//! this module existed nothing in any qa-platform gear measured either.
//!
//! # Metric naming
//!
//! These constants are the **full, literal Prometheus series names** — what
//! appears in Prometheus / `VictoriaMetrics`. They bake in the suffix the
//! OTel→Prometheus translation would otherwise add: counters carry `_total`,
//! histograms carry the unit word. No `.with_unit()` hint is set on any
//! instrument, so the rendered name is identical whether the collector has
//! `add_metric_suffixes` on or off. qa-runs' `domain::metrics` and
//! `account-management`'s document the same mechanism in their module headers;
//! this follows them.
//!
//! `counter_names_carry_the_total_suffix_and_duration_names_carry_the_unit`
//! and `every_metric_is_namespaced_to_this_gear` are the gate. They are worth
//! having because the defect they catch is invisible: a series exported under a
//! name nobody queries looks, from inside this process, exactly like a series
//! that works.
//!
//! # Why typed ports and not an `emit_metric(&str, …)` facade
//!
//! `account-management` has both, and its own docs call the stringly-typed
//! facade *"a transitional surface"* whose typed ports are *"the long-term
//! API"*; it exists there because that gear had call sites predating its
//! ports. This gear has none, so it starts where AM is going. See
//! [`crate::domain::ports::metrics`].
//!
//! # Seven families, and why the per-bug one is not at a cycle boundary
//!
//! Every other family here counts a whole pass — one collect cycle, one JIRA
//! poll — which is the unit an alert is written over and the unit that keeps
//! sample volume proportional to the tick rate rather than to the workload.
//!
//! [`QA_INSIGHTS_JIRA_BUG`] is the deliberate exception, and the reason is the
//! defect it exists to fix rather than a preference: `domain::service::jira_poller`'s
//! own header states that **every per-bug failure is logged and skipped, never
//! a whole-pass error**, because that is what the source system's behaviour
//! demands. The consequence is that a pass which silently skipped forty bugs
//! is indistinguishable, at the pass level, from a clean one — [`QA_INSIGHTS_JIRA_POLL`]
//! reports `completed` for both. A per-bug counter is the only place that
//! difference can appear. Its cardinality is bounded by
//! [`crate::domain::ports::metrics::JiraBugOutcome`]'s closed set and not by
//! the bug count: nothing about a bug — not its key, not its summary, not its
//! plan — reaches a label.

/// One collect cycle: `domain::service::collect`'s `run_collect_cycle`, from
/// the universe read to the last repository's launch, counted by how the cycle
/// itself ended.
///
/// **Not one launch.** A cycle launches a collect workflow per repository in
/// the caller's universe; a per-repository launch failure is logged, skipped
/// and not fatal, and is not counted here.
///
/// Labelled by [`crate::domain::ports::metrics::CollectOutcome`].
pub const QA_INSIGHTS_COLLECT: &str = "qa_insights_collect_total";

/// Wall-clock duration of one collect cycle. Same label set as
/// [`QA_INSIGHTS_COLLECT`], so a rate and a quantile can be read side by side.
///
/// A cycle contains one qa-catalog universe read and one qa-runs launch call
/// per repository, all issued serially, so this grows with the size of the
/// universe as well as with either sibling's latency. It is the dispatcher's
/// own RED duration and it is not an NFR: this gear's design states no bound
/// over the collect path.
pub const QA_INSIGHTS_COLLECT_DURATION: &str = "qa_insights_collect_duration_seconds";

/// One report on the runner's signed callback —
/// `POST /qa/v1/collect/{repo_id}`, `domain::service::collect`'s
/// `record_count` — counted by what the route did with it.
///
/// **This is the one family here that is not a background path.** It is a
/// per-request counter because the thing worth watching is a *rate over
/// requests*: `signature_invalid` rising against a flat `recorded` is either a
/// runner holding a stale secret or somebody guessing at the endpoint, and
/// neither was visible before this family existed. That is also why the
/// accepted case is a value rather than being left uncounted — without a
/// denominator there is no rate, only an absolute number nobody can calibrate.
///
/// Labelled by [`crate::domain::ports::metrics::CollectReportOutcome`], whose
/// values are the refusal paths `verify_signature` and the two shape guards
/// around it actually take.
pub const QA_INSIGHTS_COLLECT_REPORT: &str = "qa_insights_collect_report_total";

/// One JIRA poll pass: `domain::service::jira_poller`'s `poll_once`, for one
/// tenant, counted by how the pass itself ended.
///
/// **A `completed` pass is not a clean pass.** Every per-bug failure inside it
/// is swallowed by design, so this family reports how the *pass* ended and
/// says nothing about how many bugs it dropped on the way. [`QA_INSIGHTS_JIRA_BUG`]
/// is where that lives, and the two are meant to be read together.
///
/// Labelled by [`crate::domain::ports::metrics::JiraPollOutcome`].
pub const QA_INSIGHTS_JIRA_POLL: &str = "qa_insights_jira_poll_total";

/// Wall-clock duration of one JIRA poll pass. Same label set as
/// [`QA_INSIGHTS_JIRA_POLL`].
///
/// A pass makes one JIRA status call per open bug, serially, so this scales
/// with the tenant's open-bug count and with JIRA's own latency. That is the
/// question it answers — "is a pass still finishing inside its tick interval"
/// — and it is not an NFR: none is stated over this path.
pub const QA_INSIGHTS_JIRA_POLL_DURATION: &str = "qa_insights_jira_poll_duration_seconds";

/// One bug's own pass inside a JIRA poll, counted by where that bug's chain
/// stopped.
///
/// **The highest-value family in this gear's catalog**, and the reason is in
/// `domain::service::jira_poller`'s header: per-bug failures are logged and
/// skipped rather than raised, so nothing above them can see them. Exactly one
/// increment per bug per pass.
///
/// Labelled by [`crate::domain::ports::metrics::JiraBugOutcome`], whose values
/// are derived from the poller's own steps — the status check, the local
/// resolve write, the plan's latest version, the platform's branch, the
/// universe lookup and the launch — one failure value per failure point that
/// exists in the code.
pub const QA_INSIGHTS_JIRA_BUG: &str = "qa_insights_jira_bug_total";

/// One auto-rerun launch attempted by the JIRA poller.
///
/// Counted separately from the poll because **it is a launch, not a poll
/// step**: it puts a run into qa-runs' normal admission path, and a rerun
/// storm — the failure mode the poller's leader claim row exists to prevent —
/// is a rate on this series and on nothing else. A pass that resolves forty
/// bugs and reruns all forty is one increment on [`QA_INSIGHTS_JIRA_POLL`] and
/// forty here.
///
/// **It counts attempts, not successes, and carries no label.** A storm's cost
/// is the calls, which is what an attempt is; how the launch went is
/// [`crate::domain::ports::metrics::JiraBugOutcome::LaunchFailed`] on the
/// per-bug family, so the two together give the accepted count without this
/// family carrying a second copy of it. Every dimension that would
/// distinguish two reruns — the tenant, the bug, the plan, the platform — is
/// either unbounded cardinality or free text out of JIRA, so it is one series.
pub const QA_INSIGHTS_JIRA_RERUN: &str = "qa_insights_jira_rerun_total";

/// Every counter family this gear exports.
///
/// Declared rather than derived, and therefore its own oracle. What makes it
/// worth having anyway is that the naming rules are properties *of the set*:
/// "every counter ends `_total`" is unstatable one constant at a time, and a
/// constant added without being listed here is a constant no rule checks.
pub const COUNTERS: &[&str] = &[
    QA_INSIGHTS_COLLECT,
    QA_INSIGHTS_COLLECT_REPORT,
    QA_INSIGHTS_JIRA_POLL,
    QA_INSIGHTS_JIRA_BUG,
    QA_INSIGHTS_JIRA_RERUN,
];

/// Every duration histogram this gear exports. See [`COUNTERS`] for why the
/// list is declared.
pub const DURATIONS: &[&str] = &[QA_INSIGHTS_COLLECT_DURATION, QA_INSIGHTS_JIRA_POLL_DURATION];

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metrics_tests.rs"]
mod tests;
