//! qa-runs observability ports — typed, segregated metric-emission traits.
//!
//! Each trait owns one path of the catalog declared in
//! [`crate::domain::metrics`]. One infra adapter (Task 37) implements both on a
//! single OpenTelemetry-backed struct; DI hands each service the trait it
//! actually needs.
//!
//! # Design choices
//!
//! These are `account-management`'s four, which its own ports module states,
//! applied to this gear:
//!
//! * **Trait segregation.** [`DispatchMetrics`] and [`IngestMetrics`] are
//!   separate because the dispatcher and the ingest pipeline are separate
//!   services. The adapter implements both; only the `dyn Trait` views split.
//! * **Typed label values.** Every label is a closed enum with a total
//!   `as_str`. There is no constructor taking a `&str`, so a typo cannot reach
//!   a dashboard and neither can a value nobody enumerated.
//! * **Bridging existing types instead of duplicating them.** Each label enum
//!   is derived from something this gear already decides —
//!   [`DomainError::disclosable`], [`crate::domain::state_machine::is_terminal`],
//!   `service::launch`'s admission outcome — through a `From` impl, so the
//!   metric layer holds no second copy of a classification that would then have
//!   to be kept in agreement. Where the source type is crate-private, the impl
//!   lives beside *it* rather than here: [`DispatchDecision`] says where.
//! * **Cardinality discipline, which here is also a disclosure rule.** No label
//!   accepts a free `&str`, and in particular **no label carries a tenant id**,
//!   a run name, a branch or a repository URL. A metrics pipeline is a second
//!   copy of whatever is put in a label, exported to a system whose audience is
//!   not the database's; the same argument [`DomainError::disclosable`] makes
//!   about the `error` column applies to a label, and more forcefully, because
//!   a label is also unbounded cardinality. If per-tenant attribution is ever
//!   genuinely needed, it is a trace attribute and it is a decision with a
//!   disclosure question attached.
//!
//! # Emission cannot fail and cannot be observed
//!
//! Every method returns `()` and takes `&self`. That is the contract, not an
//! accident of the current adapter: **an implementation must not panic, must
//! not block, and must not surface a failure to its caller.** There is
//! deliberately no error type to propagate, because a caller that could handle
//! a metric failure would be a caller whose behaviour depends on whether
//! metrics are wired — and measuring a path must not change it. An adapter that
//! cannot record a sample drops it.
//!
//! [`NoopMetrics`] is the same contract at zero cost, and is what "silent when
//! no adapter is installed" means: a service constructed without the adapter
//! still emits, and nothing observes.

use std::time::Duration;

use qa_runs_sdk::RunState;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::state_machine::is_terminal;

// ════════════════════════════════════════════════════════════════════
//  Label-value taxonomy
// ════════════════════════════════════════════════════════════════════

/// `outcome` label on [`crate::domain::metrics::QA_RUNS_DISPATCH`] and its
/// duration histogram — **how one dispatcher cycle ended**, not how one
/// submission ended.
///
/// # Three values, and why the failure half is not a per-variant taxonomy
///
/// The obvious design is one label value per [`DomainError`] variant. It was
/// rejected: that is a second partition of this gear's failure space which must
/// agree with [`DomainError::disclosable`] forever, with nothing checking that
/// it still does, and it puts twenty-odd values on a label whose useful
/// question is one bit wide.
///
/// The bit that matters to an operator is *whose fault the dispatch was*, and
/// `disclosable` already answers exactly that: `true` for "the caller's own
/// request, or their own row's state", `false` for "text that originates
/// outside this gear or inside its internals". Its match is exhaustive with no
/// `_` arm, so a variant added later is a compile error there — which is what
/// makes deriving from it drift-proof rather than merely shorter.
///
/// What is lost is which error it was, and that is not lost: the error is
/// logged and, for a run that a failed submit retires, written to the run's own
/// `error` column under the same disclosure rule.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// The cycle ran to the end and drained what it was allowed to drain —
    /// **including a cycle that found nothing to do**, which is the healthy
    /// steady state and the overwhelming majority of ticks. The word is the one
    /// the other three qa-platform gears use for the same shape of pass, and it
    /// says what it means: the tick reached its own end. How many rows it
    /// claimed is the dispatcher's own report, and whether a queued run reached
    /// an execution is [`crate::domain::metrics::QA_RUNS_QUEUE_WAIT`].
    Completed,
    /// A rule stopped the cycle — the concurrency cap, or a policy decision the
    /// decision point denied. Expected traffic under a configured limit rather
    /// than an incident, and deliberately not something an alert fires on.
    Refused,
    /// A pass this gear owns failed and stopped the cycle: the execution plane
    /// could not be listed, the claim window could not be read, the queued
    /// platforms could not be enumerated. This is the series an alert fires on.
    Failed,
}

impl DispatchOutcome {
    /// Every value, for the exhaustiveness the naming and closedness tests
    /// sweep. Declared rather than derived; see [`crate::domain::metrics::COUNTERS`]
    /// for the same caveat and the same reason it is worth having.
    pub const ALL: [Self; 3] = [Self::Completed, Self::Refused, Self::Failed];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }
}

impl From<&DomainError> for DispatchOutcome {
    /// The disclosure classification *is* the fault classification. See this
    /// type's header for why there is no third failure value.
    fn from(error: &DomainError) -> Self {
        if error.disclosable() {
            Self::Refused
        } else {
            Self::Failed
        }
    }
}

/// `decision` label on [`crate::domain::metrics::QA_RUNS_DISPATCH_DECISION`].
///
/// A projection of `service::launch`'s `Admission`, which is the enum the
/// launch path already decides — so a fourth admission outcome is a compile
/// error in the projection rather than a silently unlabelled emission. The
/// queue id that enum carries is deliberately dropped: it is per-run, and a
/// per-run label is unbounded cardinality.
///
/// **The `From<&Admission>` impl lives in `service::launch`, next to the enum
/// it projects, not here.** Two reasons beyond the dependency arrow: that
/// module is `pub(crate)`, so an impl written here would attach a
/// crate-private type to a public one — unusable and undocumentable outside
/// the crate from the module that advertises it; and the "a fourth outcome is
/// a compile error" guarantee only helps if it is where the author adding a
/// variant is already looking.
///
/// **Exclusivity tier is not a label here, and that is a decision.**
/// [`qa_runs_sdk::ExclusiveTier`] is closed and four-valued, so it would be
/// cheap, and it answers a real question — *why* a run queued. It is left out
/// because it belongs to the launch decision rather than to the dispatch one,
/// and adding a dimension to a family is not reversible once dashboards key on
/// it. It is recorded on the run row and in the launch log meanwhile.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchDecision {
    /// A queue row was inserted `dispatching` and the launch path dispatches
    /// inline. The run was never queued — `service::launch`'s
    /// `Admission::Dispatch` is explicit that this outcome does not pass
    /// through [`RunState::Queued`](qa_runs_sdk::RunState::Queued).
    Inline,
    /// A queue row was inserted `queued`; the run waits for the tick.
    Queued,
    /// No platform, so no queue row at all — nothing to coordinate.
    Unqueued,
}

impl DispatchDecision {
    /// Every value. See [`DispatchOutcome::ALL`].
    pub const ALL: [Self; 3] = [Self::Inline, Self::Queued, Self::Unqueued];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Queued => "queued",
            Self::Unqueued => "unqueued",
        }
    }
}

/// `reason` label on
/// [`crate::domain::metrics::QA_RUNS_FREE_TO_START_UNANCHORED`].
///
/// One value per way a drained run can fail to yield a
/// `cpt-cf-qa-nfr-dispatch-latency` sample. Closed and total, like every label
/// on this trait, so a fifth way to lose a sample is a compile error rather
/// than an unexplained gap between the drain's count and the histogram's.
///
/// **These are not errors.** Each one is a run for which the NFR's window is
/// genuinely undefined, and the counter exists so a reader can size that
/// population against the histogram's rather than assume it away — which is
/// the mistake the retracted 2026-09-18 measurement made in the other
/// direction, by reporting a quantile over a population that answered a
/// different question (`docs/DESIGN.md` §3.11, "The dispatch-latency window,
/// and the measurement that was retracted").
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnanchoredReason {
    /// The acquisition carried no free instant. Either it joined an existing
    /// parallel hold — so no free transition admitted this run — or the
    /// environment has no recorded transition to free at all, which is a
    /// never-leased environment or one last freed before the column existed.
    NoFreeInstant,
    /// The run was **not yet queued** when its environment became free: its
    /// `enqueued_at` is later than the free instant. Nothing about the free
    /// transition delayed it, so the NFR's window does not apply — its wait
    /// started afterwards, for one of the reasons
    /// [`crate::domain::metrics::QA_RUNS_QUEUE_WAIT_DURATION`]'s doc lists
    /// (a tick stopped at the concurrency cap, a failed executor listing, or
    /// simply the gap between sweeps).
    ///
    /// A rising count here is a real signal about the dispatcher, not noise:
    /// it means runs are increasingly arriving at an already-free environment
    /// and still waiting.
    NotWaitingAtFree,
    /// The row carried no `enqueued_at`, so whether it was waiting at the free
    /// instant is unknowable. Same fail-closed case
    /// [`crate::domain::metrics::QA_RUNS_QUEUE_WAIT_DURATION`] drops.
    NoEnqueueInstant,
    /// The computed span was negative — the stored instant is ahead of this
    /// process's clock. Skew between two writers, or a fixture. Dropped rather
    /// than clamped to zero: a zero would be a real-looking observation of
    /// something that did not happen.
    NegativeSpan,
}

impl UnanchoredReason {
    /// Every value. See [`DispatchOutcome::ALL`].
    pub const ALL: [Self; 4] = [
        Self::NoFreeInstant,
        Self::NotWaitingAtFree,
        Self::NoEnqueueInstant,
        Self::NegativeSpan,
    ];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoFreeInstant => "no_free_instant",
            Self::NotWaitingAtFree => "not_waiting_at_free",
            Self::NoEnqueueInstant => "no_enqueue_instant",
            Self::NegativeSpan => "negative_span",
        }
    }
}

/// `outcome` label on [`crate::domain::metrics::QA_RUNS_INGEST`] and its
/// duration histogram.
///
/// One value per way a single observation can land. The failure half is
/// [`DispatchOutcome`]'s, for the same reason and through the same bridge; the
/// success half is split three ways because the three are operationally
/// different events that a single `applied` would hide.
///
/// **The terminal *verdict* is not here.** Whether a run ended green is a
/// question about runs, not about the ingest path's latency, and putting
/// [`RunState`]'s six terminal values on this label would multiply this
/// family's series by six to answer a question the run row answers exactly.
/// [`Self::Completed`] records only that a terminal state was written.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// A non-terminal observation was recorded — a log line, a test result, a
    /// start. The overwhelming majority of passes.
    Applied,
    /// A terminal state was recorded and the run is over.
    Completed,
    /// A repeated completion whose reconciled state equalled the recorded one.
    /// Recorded, not refused, and its own value rather than folded into
    /// [`Self::Applied`]: executors retry, this is the shape that retry takes,
    /// and a rising duplicate rate is the signal that one is stuck retrying.
    Duplicate,
    /// A rule refused the observation — an illegal transition, a run not
    /// visible under the caller's scope, a denied policy decision.
    Refused,
    /// The pass failed for a reason inside this gear or in the database.
    Failed,
}

impl IngestOutcome {
    /// Every value. See [`DispatchOutcome::ALL`].
    pub const ALL: [Self; 5] = [
        Self::Applied,
        Self::Completed,
        Self::Duplicate,
        Self::Refused,
        Self::Failed,
    ];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Completed => "completed",
            Self::Duplicate => "duplicate",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }
}

impl From<&DomainError> for IngestOutcome {
    /// The same split [`DispatchOutcome`] uses, from the same source.
    fn from(error: &DomainError) -> Self {
        if error.disclosable() {
            Self::Refused
        } else {
            Self::Failed
        }
    }
}

impl From<RunState> for IngestOutcome {
    /// The state the pass left the run in: a terminal one completed the run,
    /// anything else applied an observation to a live one.
    ///
    /// Derived from [`crate::domain::state_machine::is_terminal`] rather than
    /// from a list of terminal states written here. A copy of that set in the
    /// metric layer would be a second definition of which states end a run,
    /// and nothing would notice the day the two stopped agreeing.
    fn from(state: RunState) -> Self {
        if is_terminal(state) {
            Self::Completed
        } else {
            Self::Applied
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Port traits — one per measured path
// ════════════════════════════════════════════════════════════════════

/// The dispatch path's telemetry — the proxy this gear can offer for
/// `cpt-cf-qa-nfr-dispatch-latency`, plus what admission decided upstream of it.
///
/// **Neither method measures that NFR's own quantity**, and the two catalog
/// docs say how far each falls short:
/// [`crate::domain::metrics::QA_RUNS_QUEUE_WAIT_DURATION`] is a two-sided proxy
/// for it, and [`crate::domain::metrics::QA_RUNS_DISPATCH_DURATION`] is the
/// dispatcher's own RED duration and is explicitly not it.
///
/// # Implementations must not fail a caller
///
/// Both methods return `()`, take `&self`, and are called from paths whose
/// behaviour must not depend on whether metrics are wired. An implementation
/// must not panic, must not block, and has no error to propagate by
/// construction. Dropping a sample is the correct response to an adapter that
/// cannot record one. See this module's header.
pub trait DispatchMetrics: Send + Sync + 'static {
    /// One **dispatcher cycle** — `service::dispatch`'s tick — with how it
    /// ended and the wall-clock time it took. Counts into
    /// [`crate::domain::metrics::QA_RUNS_DISPATCH`] and observes into
    /// [`crate::domain::metrics::QA_RUNS_DISPATCH_DURATION`] — one call, two
    /// instruments, so the rate and the quantile can never disagree about how
    /// many cycles there were.
    ///
    /// **Not one submission.** `dispatch_one` runs inside the cycle's drain
    /// loop and is not measured here; measuring it would time the force-sync
    /// and the bundle build rather than the dispatcher.
    fn dispatch_pass(&self, outcome: DispatchOutcome, duration: Duration);

    /// What admission decided for one launch. Counts into
    /// [`crate::domain::metrics::QA_RUNS_DISPATCH_DECISION`].
    fn dispatch_decision(&self, decision: DispatchDecision);

    /// One queued run reaching an accepted execution request, with how long it
    /// waited since it was enqueued. Counts into
    /// [`crate::domain::metrics::QA_RUNS_QUEUE_WAIT`] and observes into
    /// [`crate::domain::metrics::QA_RUNS_QUEUE_WAIT_DURATION`].
    ///
    /// **Per run, unlike the other two methods on this trait**, because the
    /// wait is a property of a run and there is no other unit it could have.
    /// Cardinality is unaffected: the family carries no label, so it is one
    /// series however many runs pass through it.
    ///
    /// Read [`crate::domain::metrics::QA_RUNS_QUEUE_WAIT_DURATION`]'s own doc
    /// before writing an alert on it: it is an upper bound on the NFR's
    /// quantity, not the quantity itself.
    fn queue_wait(&self, waited: Duration);

    /// One queued run reaching an accepted execution request, with how long it
    /// waited **since its environment became free**. Observes into
    /// [`crate::domain::metrics::QA_RUNS_FREE_TO_START_DURATION`].
    ///
    /// This is `cpt-cf-qa-nfr-dispatch-latency`'s own quantity, which
    /// [`Self::queue_wait`] is only an approximation of. The two are emitted
    /// from the same call site and share an end instant; they differ in where
    /// the clock starts.
    ///
    /// **Not every call to [`Self::queue_wait`] is accompanied by one of
    /// these** — see [`Self::free_to_start_unanchored`] for the runs that
    /// have no such window and why.
    fn free_to_start(&self, waited: Duration);

    /// One drained run that yielded **no** [`Self::free_to_start`] sample,
    /// with why. Counts into
    /// [`crate::domain::metrics::QA_RUNS_FREE_TO_START_UNANCHORED`].
    ///
    /// Emitted exactly once per drained run that reached an execution request
    /// without a usable anchor, so this family and the histogram's sample
    /// count partition the same population. That partition is the point: it is
    /// what lets a reader size the quantile's coverage instead of trusting it
    /// blind.
    fn free_to_start_unanchored(&self, reason: UnanchoredReason);
}

/// The ingest path's telemetry — the **gear-side half** of
/// `cpt-cf-qa-nfr-result-latency`, which is stated over runner emission → API
/// visibility and so shares neither endpoint with what this measures. See
/// [`crate::domain::metrics::QA_RUNS_INGEST_DURATION`].
///
/// The same infallibility contract as [`DispatchMetrics`], and it matters more
/// here: ingest runs per log line, so an implementation that allocated or
/// locked per call would change the throughput of the path it is measuring.
pub trait IngestMetrics: Send + Sync + 'static {
    /// One observation applied, with the wall-clock time it took. Counts into
    /// [`crate::domain::metrics::QA_RUNS_INGEST`] and observes into
    /// [`crate::domain::metrics::QA_RUNS_INGEST_DURATION`].
    fn ingest_batch(&self, outcome: IngestOutcome, duration: Duration);
}

// ════════════════════════════════════════════════════════════════════
//  No-op implementation
// ════════════════════════════════════════════════════════════════════

/// No-op implementation of every port.
///
/// Two uses, and the first is the one that keeps the "metrics must not change
/// behaviour" promise structural rather than documented:
///
/// * the safe default before an adapter is constructed, so a service holding it
///   emits every signal a wired service does and nothing observes;
/// * the in-test default for services whose tests do not assert on metrics.
///
/// Zero-sized; an `Arc<NoopMetrics>` shares for free.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl DispatchMetrics for NoopMetrics {
    fn dispatch_pass(&self, _outcome: DispatchOutcome, _duration: Duration) {}
    fn dispatch_decision(&self, _decision: DispatchDecision) {}
    fn queue_wait(&self, _waited: Duration) {}
    fn free_to_start(&self, _waited: Duration) {}
    fn free_to_start_unanchored(&self, _reason: UnanchoredReason) {}
}

impl IngestMetrics for NoopMetrics {
    fn ingest_batch(&self, _outcome: IngestOutcome, _duration: Duration) {}
}
