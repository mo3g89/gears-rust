//! qa-insights observability ports — typed, segregated metric-emission traits.
//!
//! Each trait owns one measured path of the catalog declared in
//! [`crate::domain::metrics`]. One infra adapter
//! ([`crate::infra::metrics::QaInsightsMetricsMeter`]) implements both on a
//! single `OpenTelemetry`-backed struct; DI hands each service the trait it
//! actually needs.
//!
//! # Design choices
//!
//! qa-runs' four, which its own ports module states, applied to this gear:
//!
//! * **Trait segregation.** [`CollectMetrics`] and [`JiraPollMetrics`] are
//!   separate because the collect service and the poller are separate
//!   services. The adapter implements both; only the `dyn Trait` views split.
//! * **Typed label values.** Every label is a closed enum with a total
//!   `as_str`. There is no constructor taking a `&str`, so a typo cannot reach
//!   a dashboard and neither can a value nobody enumerated.
//! * **Bridging existing types instead of duplicating them.** Each label enum
//!   is derived from something this gear already decides — the disclosure
//!   split the API boundary makes, `CollectService`'s own signature-refusal
//!   enum — through a `From` impl, so the metric layer holds no second copy of
//!   a classification that would then have to be kept in agreement. Where the
//!   source type is crate-private the impl lives beside *it* rather than here;
//!   [`CollectReportOutcome`] says where.
//! * **Cardinality discipline, which here is also a disclosure rule, and here
//!   the disclosure half is the live one.** No label accepts a free `&str`,
//!   and in particular **no label carries a tenant id, a JIRA issue key, a bug
//!   summary, a test name, a plan path, a branch, a repository id or a URL**.
//!   The JIRA poller's inputs are issues whose keys and summaries are free
//!   text written by whoever filed them; a metrics pipeline is a second copy
//!   of whatever is put in a label, exported to a system whose audience is not
//!   the database's. If per-tenant attribution is ever genuinely needed it is
//!   a trace attribute, and it is a decision with a disclosure question
//!   attached.
//!
//! # Emission cannot fail and cannot be observed
//!
//! Every method returns `()` and takes `&self`. That is the contract, not an
//! accident of the current adapter: **an implementation must not panic, must
//! not block, and must not surface a failure to its caller.** There is
//! deliberately no error type to propagate, because a caller that could handle
//! a metric failure would be a caller whose behaviour depends on whether
//! metrics are wired — and measuring a path must not change it. An adapter
//! that cannot record a sample drops it.
//!
//! [`NoopMetrics`] is the same contract at zero cost, and is what "silent when
//! no adapter is installed" means: a service constructed without the adapter
//! still emits, and nothing observes.

use std::time::Duration;

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

// ════════════════════════════════════════════════════════════════════
//  Label-value taxonomy
// ════════════════════════════════════════════════════════════════════

/// Whether a [`DomainError`] is something this gear failed at, or something
/// the caller's request or this deployment's configuration decided.
///
/// # This is the split the API boundary already makes, not a second one
///
/// qa-runs derives the same bit from `DomainError::disclosable`. This gear has
/// no such method; what it has is `From<DomainError> for CanonicalError` in
/// [`crate::domain::error`], whose match is exhaustive with no `_` arm and
/// whose final arm renders exactly [`DomainError::CorruptState`],
/// [`DomainError::Database`] and [`DomainError::Internal`] through
/// `opaque_internal` — the three whose text, in that module's own words, "may
/// not reach an HTTP body" because it originates in a driver, in this gear's
/// internals, or in a persisted column.
///
/// That group is exactly the answer to *whose failure was this?*, so it is
/// what both outcome bridges below use. This function's own match is
/// exhaustive with no `_` arm for the same reason the canonical mapping's is:
/// a variant added to [`DomainError`] must be classified by whoever adds it
/// rather than inheriting a default. `a_refusal_and_an_internal_failure_are_told_apart_by_what_the_api_may_disclose`
/// is what keeps the two partitions in agreement — it sweeps every variant and
/// asserts this function's answer equals "the canonical rendering is a 500".
///
/// What is lost — which error it was — is not lost: every call site that
/// classifies an error here has already logged it.
fn is_this_gears_own_failure(error: &DomainError) -> bool {
    match error {
        DomainError::CorruptState { .. }
        | DomainError::Database { .. }
        | DomainError::Internal(_) => true,
        DomainError::RunNotIngested { .. }
        | DomainError::UnsupportedScope { .. }
        | DomainError::IngestConflict
        | DomainError::SavedViewNameExists { .. }
        | DomainError::SavedViewNotFound { .. }
        | DomainError::JiraNotConfigured
        | DomainError::BugNotFound { .. }
        | DomainError::UnsupportedEgress { .. }
        | DomainError::Validation { .. }
        | DomainError::Forbidden => false,
    }
}

/// `outcome` label on [`crate::domain::metrics::QA_INSIGHTS_COLLECT`] and its
/// duration histogram — **how one collect cycle ended**, not how one
/// repository's launch ended.
///
/// Three values, and the failure half is deliberately not a per-variant
/// taxonomy of [`DomainError`]: that would be a second partition of this
/// gear's failure space with nothing checking it still agreed with the one the
/// API boundary makes, on a label whose useful question is one bit wide. See
/// [`is_this_gears_own_failure`].
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectOutcome {
    /// The cycle ran to the end and launched what it was able to launch —
    /// **including a cycle that launched nothing**, because the universe was
    /// empty or every repository's launch was refused. Per-repository launch
    /// failures are logged and skipped by design
    /// (`CollectService::run_collect_cycle`'s own doc), so they do not change
    /// this label; what a cycle returns is the accepted count.
    Completed,
    /// The cycle could not start because a rule refused it — the PDP denied
    /// the universe read, or the compiled scope constrains more than the
    /// tenant. Expected traffic under a configuration rather than an incident.
    Refused,
    /// The universe read failed for a reason inside this gear, qa-catalog or
    /// the database. This is the series an alert fires on.
    Failed,
}

impl CollectOutcome {
    /// Every value, for the exhaustiveness the naming and closedness tests
    /// sweep. Declared rather than derived; see
    /// [`crate::domain::metrics::COUNTERS`] for the same caveat and the same
    /// reason it is worth having.
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

impl From<&DomainError> for CollectOutcome {
    /// The disclosure classification *is* the fault classification. See
    /// [`is_this_gears_own_failure`] for why there is no third failure value.
    fn from(error: &DomainError) -> Self {
        if is_this_gears_own_failure(error) {
            Self::Failed
        } else {
            Self::Refused
        }
    }
}

/// `outcome` label on
/// [`crate::domain::metrics::QA_INSIGHTS_COLLECT_REPORT`] — what the runner's
/// signed callback route did with one report.
///
/// # The values are the refusal paths that exist, one each
///
/// `CollectService::record_count` refuses in exactly five places and accepts
/// in one, and each is its own value here. Three come out of
/// `verify_signature`, which folds them into a single
/// [`DomainError::Forbidden`] on purpose — the response must not become an
/// oracle telling an attacker which guess was closer — and that is precisely
/// why the *metric* has to separate them: the response deliberately cannot,
/// and an operator who cannot tell a deployment with no secret configured from
/// one under a guessing attack has no way to act on either.
///
/// **The `From<SignatureRefusal>` bridge lives in
/// `domain::service::collect`**, beside the enum it projects, rather than
/// here: that enum is crate-private, so an impl written here would attach a
/// crate-private type to a public one, and the "a fourth refusal path is a
/// compile error" guarantee only helps where the author adding one is already
/// looking.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectReportOutcome {
    /// The count was written.
    Recorded,
    /// This deployment has no usable signing secret, so the route fails closed
    /// and refuses every report. **A misconfiguration, not an attack**, and
    /// the one value here whose fix is in a config file rather than in a
    /// runner or a firewall. It cannot be told apart from the two below by the
    /// response, which is why it is told apart here.
    SecretUnconfigured,
    /// The `sig` parameter is not hex. A caller that is not echoing back a URL
    /// this gear built at all — a scanner, or a hand-written request.
    SignatureMalformed,
    /// The signature decoded but does not verify against this deployment's
    /// secret for the `(repo_id, branch, tenant_id)` triple presented. A
    /// runner holding a stale secret, or a forgery attempt. **A rising rate
    /// here against a flat [`Self::Recorded`] is the signal this family
    /// exists for.**
    SignatureInvalid,
    /// The signature verified and the report's shape did not: a blank `branch`
    /// or a blank `test_file`. One value for both, because both mean the same
    /// thing operationally — a runner sending a malformed report — and the
    /// field that failed is already in the `Validation` error the caller gets
    /// back.
    Invalid,
    /// The write failed for a reason inside this gear or in the database.
    Failed,
}

impl CollectReportOutcome {
    /// Every value. See [`CollectOutcome::ALL`].
    pub const ALL: [Self; 6] = [
        Self::Recorded,
        Self::SecretUnconfigured,
        Self::SignatureMalformed,
        Self::SignatureInvalid,
        Self::Invalid,
        Self::Failed,
    ];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::SecretUnconfigured => "secret_unconfigured",
            Self::SignatureMalformed => "signature_malformed",
            Self::SignatureInvalid => "signature_invalid",
            Self::Invalid => "invalid",
            Self::Failed => "failed",
        }
    }
}

/// `outcome` label on [`crate::domain::metrics::QA_INSIGHTS_JIRA_POLL`] and
/// its duration histogram — **how one tenant's poll pass ended**.
///
/// The failure half is [`CollectOutcome`]'s, through the same bridge. The
/// success half is split two ways because the two are operationally different
/// events that a single value would hide.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JiraPollOutcome {
    /// The pass read the tenant's open bugs and processed every one of them.
    ///
    /// **This does not mean every bug succeeded.** `poll_once` returns `Ok`
    /// whether one bug was dropped or forty were, because per-bug failures are
    /// logged and skipped by design. What a pass dropped is
    /// [`crate::domain::metrics::QA_INSIGHTS_JIRA_BUG`].
    Completed,
    /// The tenant has no JIRA configuration, or it is disabled, so the pass
    /// returned before looking at a single bug. The steady state for every
    /// tenant that does not use the JIRA integration, and deliberately not
    /// folded into [`Self::Completed`]: a deployment where *every* pass is
    /// skipped looks identical to one where every pass is clean, and only this
    /// value tells them apart.
    Skipped,
    /// A rule refused the pass — the PDP denied the config read or the
    /// open-bugs listing.
    Refused,
    /// The pass failed for a reason inside this gear or in the database. This
    /// is the series an alert fires on.
    Failed,
}

impl JiraPollOutcome {
    /// Every value. See [`CollectOutcome::ALL`].
    pub const ALL: [Self; 4] = [Self::Completed, Self::Skipped, Self::Refused, Self::Failed];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }
}

impl From<&DomainError> for JiraPollOutcome {
    /// The same split [`CollectOutcome`] uses, from the same source.
    fn from(error: &DomainError) -> Self {
        if is_this_gears_own_failure(error) {
            Self::Failed
        } else {
            Self::Refused
        }
    }
}

/// `outcome` label on [`crate::domain::metrics::QA_INSIGHTS_JIRA_BUG`] —
/// **where one bug's chain stopped inside a poll pass**. Exactly one value per
/// bug per pass.
///
/// # The taxonomy is the poller's failure points, not a list invented here
///
/// `domain::service::jira_poller` swallows every per-bug failure: each is
/// logged and skipped, never raised, because that is what the source system's
/// behaviour demands. The chain has exactly six places it can fail, and each
/// has a value below:
///
/// | Step | Failure value |
/// | --- | --- |
/// | `checked_status` — the JIRA status call | [`Self::StatusCheckFailed`] |
/// | `mark_resolved` — the local resolve write | [`Self::ResolveWriteFailed`] |
/// | `maybe_rerun` — the plan's latest version | [`Self::PlanVersionUnreadable`] |
/// | `resolve_branch` — the platform's default branch | [`Self::BranchUnresolved`] |
/// | `find_plan_test_file` — the catalog universe lookup | [`Self::TestFileUnresolved`] |
/// | `launch` — the rerun itself | [`Self::LaunchFailed`] |
///
/// Adding a seventh failure point without adding a value here is possible —
/// nothing in the type system prevents it — but every one of the six above is
/// a `tracing::warn!`-and-return arm, and the classification sits on the same
/// line as the return.
///
/// # Where the precedence lies, and why
///
/// [`Self::ResolveWriteFailed`] wins over any later value. The write failing
/// does not stop the chain in the code — `mark_resolved` is best-effort and
/// the rerun decision runs regardless — but it is the operationally dominant
/// fact, because the bug stays *open* in this gear's own table while JIRA
/// keeps reporting it done. The next pass therefore resolves it again, and
/// reruns it again, and the pass after that: a failing resolve write is a
/// **rerun-storm generator**, which is the one thing this family is read
/// alongside [`crate::domain::metrics::QA_INSIGHTS_JIRA_RERUN`] to catch.
///
/// # What is not a failure
///
/// [`Self::Unresolved`] and [`Self::Resolved`] are the healthy majority. The
/// four no-op paths inside `maybe_rerun` — the auto-rerun switch off, a bug
/// with no recorded version, a plan with no recorded build, and a latest build
/// equal to the one the bug was filed against — are all [`Self::Resolved`]:
/// each is a decision not to rerun, taken deliberately, and separating them
/// would put four more series on this label to answer a question nobody asks
/// of a counter.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JiraBugOutcome {
    /// JIRA still reports the bug open. Nothing to do, and the overwhelming
    /// majority of increments.
    Unresolved,
    /// JIRA reports the bug done, the local resolve write succeeded, and the
    /// bug's chain finished — with or without a rerun. Whether a rerun
    /// happened is [`crate::domain::metrics::QA_INSIGHTS_JIRA_RERUN`].
    Resolved,
    /// The JIRA status call failed, so this bug was skipped for this pass.
    /// **The one failure here that is retried**: the bug is still open, so the
    /// next pass checks it again. A rising rate is a JIRA outage or a
    /// credential problem, not a lost rerun.
    StatusCheckFailed,
    /// JIRA reports the bug done and the local resolve write failed. See this
    /// type's header for why this value takes precedence over anything the
    /// rerun chain then reported, and why it is the storm precursor.
    ResolveWriteFailed,
    /// The plan's latest recorded build could not be read, so the new-build
    /// comparison could not be made. The bug is already marked resolved, so
    /// this rerun is not retried.
    PlanVersionUnreadable,
    /// The bug's platform could not be asked for its default branch. The bug
    /// is already marked resolved, so this rerun is not retried.
    BranchUnresolved,
    /// No test on the resolved branch declares this bug's title in the catalog
    /// universe — **or the universe read itself failed**, which the poller
    /// deliberately treats as the same outcome (`find_plan_test_file`'s own
    /// doc: both leave the bug already resolved with nothing this pass can do
    /// about it). The two are not separated here because the poller does not
    /// separate them; separating them in the metric would be a distinction the
    /// code does not make.
    TestFileUnresolved,
    /// qa-runs refused the rerun launch. The bug is already marked resolved,
    /// so this rerun is not retried.
    LaunchFailed,
}

impl JiraBugOutcome {
    /// Every value. See [`CollectOutcome::ALL`].
    pub const ALL: [Self; 8] = [
        Self::Unresolved,
        Self::Resolved,
        Self::StatusCheckFailed,
        Self::ResolveWriteFailed,
        Self::PlanVersionUnreadable,
        Self::BranchUnresolved,
        Self::TestFileUnresolved,
        Self::LaunchFailed,
    ];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unresolved => "unresolved",
            Self::Resolved => "resolved",
            Self::StatusCheckFailed => "status_check_failed",
            Self::ResolveWriteFailed => "resolve_write_failed",
            Self::PlanVersionUnreadable => "plan_version_unreadable",
            Self::BranchUnresolved => "branch_unresolved",
            Self::TestFileUnresolved => "test_file_unresolved",
            Self::LaunchFailed => "launch_failed",
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Port traits — one per measured path
// ════════════════════════════════════════════════════════════════════

/// The collect path's telemetry — the cycle, and the runner's signed callback
/// into it.
///
/// # Implementations must not fail a caller
///
/// Both methods return `()`, take `&self`, and are called from paths whose
/// behaviour must not depend on whether metrics are wired. An implementation
/// must not panic, must not block, and has no error to propagate by
/// construction. Dropping a sample is the correct response to an adapter that
/// cannot record one. See this module's header.
pub trait CollectMetrics: Send + Sync + 'static {
    /// One **collect cycle** — `CollectService::run_collect_cycle` — with how
    /// it ended and the wall-clock time it took. Counts into
    /// [`crate::domain::metrics::QA_INSIGHTS_COLLECT`] and observes into
    /// [`crate::domain::metrics::QA_INSIGHTS_COLLECT_DURATION`] — one call,
    /// two instruments, so the rate and the quantile can never disagree about
    /// how many cycles there were.
    ///
    /// **Not one launch.** The per-repository launches inside the cycle are
    /// not measured; a failed one is logged and skipped and does not change
    /// the cycle's outcome.
    fn collect_cycle(&self, outcome: CollectOutcome, duration: Duration);

    /// One report on the runner's signed callback. Counts into
    /// [`crate::domain::metrics::QA_INSIGHTS_COLLECT_REPORT`].
    ///
    /// **Per request, unlike the method above**, because the signal is a rate
    /// over requests and a cycle boundary cannot carry it — the reports arrive
    /// on a public route, minutes to hours after the cycle that signed their
    /// URL. Cardinality is unaffected: the label is a closed six-value set.
    fn collect_report(&self, outcome: CollectReportOutcome);
}

/// The JIRA poller's telemetry — the pass, every bug inside it, and the reruns
/// it launches.
///
/// The same infallibility contract as [`CollectMetrics`].
pub trait JiraPollMetrics: Send + Sync + 'static {
    /// One tenant's poll pass, with how it ended and the wall-clock time it
    /// took. Counts into [`crate::domain::metrics::QA_INSIGHTS_JIRA_POLL`] and
    /// observes into
    /// [`crate::domain::metrics::QA_INSIGHTS_JIRA_POLL_DURATION`].
    fn poll_pass(&self, outcome: JiraPollOutcome, duration: Duration);

    /// One bug's own chain inside a pass. Counts into
    /// [`crate::domain::metrics::QA_INSIGHTS_JIRA_BUG`].
    ///
    /// **Per bug, and that is the point of the family** — see
    /// [`JiraBugOutcome`]'s header. It is the one per-item emission in this
    /// gear's catalog, and it is bounded: the label is a closed eight-value
    /// set, so a tenant with ten thousand open bugs contributes ten thousand
    /// samples across the same eight series.
    fn bug(&self, outcome: JiraBugOutcome);

    /// One auto-rerun launch attempted. Counts into
    /// [`crate::domain::metrics::QA_INSIGHTS_JIRA_RERUN`].
    ///
    /// Attempts rather than successes, and no label at all; that constant's
    /// own doc carries both arguments.
    fn auto_rerun(&self);
}

// ════════════════════════════════════════════════════════════════════
//  No-op implementation
// ════════════════════════════════════════════════════════════════════

/// No-op implementation of every port.
///
/// Two uses, and the first is the one that keeps the "metrics must not change
/// behaviour" promise structural rather than documented:
///
/// * the safe default before an adapter is constructed, so a service holding
///   it emits every signal a wired service does and nothing observes;
/// * the in-test default for services whose tests do not assert on metrics.
///
/// Zero-sized; an `Arc<NoopMetrics>` shares for free.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl CollectMetrics for NoopMetrics {
    fn collect_cycle(&self, _outcome: CollectOutcome, _duration: Duration) {}
    fn collect_report(&self, _outcome: CollectReportOutcome) {}
}

impl JiraPollMetrics for NoopMetrics {
    fn poll_pass(&self, _outcome: JiraPollOutcome, _duration: Duration) {}
    fn bug(&self, _outcome: JiraBugOutcome) {}
    fn auto_rerun(&self) {}
}
