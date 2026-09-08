//! qa-environments observability ports — the typed metric-emission traits for
//! the observation cycle and for the plugin boundary.
//!
//! [`ObservationMetrics`] and [`PluginMetrics`] own the whole catalog declared
//! in [`crate::domain::metrics`]. One infra adapter
//! ([`crate::infra::metrics::QaEnvironmentsMetricsMeter`]) implements both; DI
//! hands [`crate::domain::service::EnvironmentsService`] an `Arc<dyn _>` view of
//! each.
//!
//! # Why the plugin boundary is timed here, at the caller, and not in the
//! plugins
//!
//! **The design decision [`PluginMetrics`] exists to record.** The alternative
//! was to instrument the plugins: have every implementation of
//! `qa_product_sdk::QaProductPluginV1` time and count its own work. That is
//! more faithful per plugin, and it is the wrong trade here for two reasons
//! that are both about `qa-product-sdk` rather than about metrics.
//!
//! * **It would widen the contract every plugin author must satisfy.**
//!   `qa-product-sdk` is deliberately narrow — its own feature documentation
//!   argues that the crate gates its own code rather than growing its
//!   dependency graph, and `qa-connector-k8s` exists precisely so that the one
//!   heavy dependency in this subsystem sits in a crate only the plugins that
//!   need it link. Putting an `opentelemetry` dependency in the SDK would put
//!   it in every plugin, including plugins written outside this repository.
//! * **The caller can answer the question completely.** What a dashboard needs
//!   is *how long does this deployment's plugin take, and how often does it
//!   fail*, and both are visible from outside the call: the duration is the
//!   round trip, and the failure class is
//!   `qa_product_sdk::observation::FailureClass`, which the plugin contract
//!   already requires a plugin to classify its failure as before it may cross
//!   the boundary. Nothing a plugin knows about itself is needed for either.
//!
//! What the caller genuinely cannot see is *why* a plugin was slow — which of
//! its own internal steps cost the seconds. That is a real loss, it is what
//! plugin-side instrumentation would have bought, and a plugin that wants it
//! can still emit its own metrics without any of this changing. It is recorded
//! here so the trade is visible rather than implied.
//!
//! ## No plugin identity is a label, including its GTS type id — measured
//!
//! The obvious dimension for a plugin-boundary family is *which plugin*, and it
//! is absent. Two separate reasons, and the second needed checking rather than
//! assuming.
//!
//! * **The instance id is not available here at all.** This gear resolves a
//!   plugin through [`crate::domain::ports::ProductPluginPort`], which hands
//!   back an `Arc<dyn QaProductPluginV1>` and nothing else;
//!   `QaProductPluginV1` declares no identity method, and the id lives in
//!   qa-catalog's `qa_products.plugin_instance_id` column. Labelling by it
//!   would mean widening a cross-gear SDK trait to carry a metric dimension.
//! * **The GTS *type* id is a set of one, so it would be a constant label.**
//!   `qa-product-sdk` declares exactly one product-plugin spec type,
//!   `QaProductPluginSpecV1`; `toolkit_gts::PluginV1::build_registration`
//!   composes an instance id as that type id followed by the plugin's own
//!   instance segment; and qa-catalog's `list_registered_plugins` filters the
//!   types-registry by equality against that one constant. So every product
//!   plugin in any deployment shares one type id, and only the tail
//!   distinguishes them.
//!
//! **What separates "the plugin is slow" from "the gear is slow" is therefore
//! the nesting, not a label** — see [`crate::domain::metrics`]' header — and
//! the nesting separates them completely. If this platform ever grows a second
//! product-plugin spec type, the second reason's premise fails and a type label
//! becomes worth its key; that is the condition to revisit this under.
//!
//! # Design choices
//!
//! qa-runs' four, which its own ports module states, applied to this gear:
//!
//! * **Trait segregation.** Two measured paths, two traits. The observation
//!   cycle's port and the plugin boundary's port are separate because their
//!   emissions have different populations and different lifetimes: a cycle
//!   emits once per tick, a plugin call once per round trip actually made, and
//!   a consumer of one has no use for the other. Widening
//!   [`ObservationMetrics`] would have made every implementation of it — the
//!   adapter, [`NoopMetrics`], and every test double — carry a method about a
//!   subject it has nothing to say about.
//! * **Typed label values.** Every label is a closed enum with a total
//!   `as_str`. There is no constructor taking a `&str`, so a typo cannot reach
//!   a dashboard and neither can a value nobody enumerated.
//! * **Bridging existing types instead of duplicating them.**
//!   [`ObservationClass`]'s failure half is a projection of
//!   `qa_product_sdk::observation::FailureClass` — the taxonomy this gear
//!   already reasons in, persists into an environment's row and renders on its
//!   page — through a `From` impl, so the metric layer holds no second copy of
//!   a classification that would then have to be kept in agreement. A variant
//!   added to that enum is a compile error in the impl.
//! * **Cardinality discipline, which here is a disclosure rule first.** No
//!   label accepts a free `&str`, and in particular **no label carries an
//!   environment id, an environment name, a tenant id, a cluster URL, or any
//!   value read out of a kubeconfig.**
//!
//!   That is not the usual cardinality argument wearing a disclosure hat. This
//!   gear's whole design is about keeping exactly those values from leaving:
//!   `EnvironmentDto` deliberately drops `kubeconfig_credstore_ref` because
//!   under `SharingMode::Tenant` the reference *is* a read path to the
//!   kubeconfig; `PluginFailure::detail` is `&'static str` because a measured
//!   leak on 2026-08-28 put a PEM private key on the environment page through a
//!   formatted error. A metrics pipeline is a second copy of whatever is put in
//!   a label, exported continuously to a system whose audience is not the
//!   database's and whose retention nobody in this gear controls. An
//!   environment name in a label would re-open, in a channel with no DTO
//!   between it and the outside, precisely the disclosure those two decisions
//!   closed.
//!
//!   The consequence is accepted rather than worked around: **no series here
//!   can tell an operator which environment was slow.** The environment's own
//!   row can, and already does — `version_detect_error` and `health_detail`
//!   carry the plugin's own text — and so does the `warn!` line the cycle
//!   writes with the environment id on it. What the series adds is the shape
//!   of the population, which is the question a row cannot answer.
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

use qa_product_sdk::observation::FailureClass;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

// ════════════════════════════════════════════════════════════════════
//  Label-value taxonomy
// ════════════════════════════════════════════════════════════════════

/// `outcome` label on [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION_CYCLE`]
/// and its duration histogram — **how one observation cycle ended**, not how
/// any environment in it fared.
///
/// Three values, read off `run_observation_cycle`'s own control flow: it has
/// two early returns before the loop, one early return inside it, and one tail.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleOutcome {
    /// The cycle reached the end of the environment list.
    ///
    /// **Including a cycle that found nothing to do** — a deployment with no
    /// registered environments completes every cycle, and
    /// [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION`] is where the
    /// population shows up.
    Completed,
    /// Shutdown was signalled between two environments, so the cycle returned
    /// the partial report it had built.
    ///
    /// Not a failure: `run_observation_cycle`'s own doc calls this out as a
    /// designed path — the next tick, or the next process, re-sweeps every
    /// environment from scratch. It has its own value rather than being folded
    /// into [`Self::Completed`] because a cancelled cycle's `attempted` is a
    /// truncation of the population and its duration is a truncation of the
    /// real cost, so mixing it into either distribution would understate both.
    Cancelled,
    /// The cycle never reached its first environment: the database connection
    /// could not be acquired, or the cross-tenant environment listing failed.
    ///
    /// **This is the value that closes the gear's worst blind spot.** Both of
    /// those paths return an all-zero `ObservationCycleReport`, which is
    /// byte-identical to the report a deployment with no registered
    /// environments produces — so before this series existed, "the database is
    /// down and nothing has been observed for an hour" and "this deployment
    /// has no environments" looked exactly alike outside the log.
    Unstarted,
}

impl CycleOutcome {
    /// Every value, for the exhaustiveness the naming and closedness tests
    /// sweep. Declared rather than derived; see
    /// [`crate::domain::metrics::COUNTERS`] for the same caveat and the same
    /// reason it is worth having.
    ///
    /// # Read only by the tests, and that is not dead code
    ///
    /// The allowance is on this constant alone rather than on the `impl` block,
    /// so that a method added here later and genuinely left unused is still
    /// reported. `as_str` beside it has production readers (the adapter) and
    /// needs no allowance.
    #[allow(
        dead_code,
        reason = "swept by the catalog and adapter tests and has no production reader; \
                  `domain` is pub(crate) in this gear, so the compiler sees that as dead. \
                  See crate::domain::metrics::COUNTERS for the same allowance and the same \
                  argument."
    )]
    pub const ALL: [Self; 3] = [Self::Completed, Self::Cancelled, Self::Unstarted];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Unstarted => "unstarted",
        }
    }
}

/// `outcome` label on [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION`]
/// — **the two counters `ObservationCycleReport` already carries**, and
/// nothing else.
///
/// This enum exists so that the emission names its label at the call site
/// instead of passing two same-typed counts positionally: the alternative
/// signature, `fn cycle_environments(&self, observed: u32, failed: u32)`, is
/// one transposition away from reporting a healthy cycle as a broken one, and
/// no type would notice.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentOutcome {
    /// `ObservationCycleReport::observed` — `observe_environment` returned
    /// `Ok`, so an outcome was **persisted**.
    ///
    /// **Not "the environment is healthy".** A cluster that could not be
    /// reached is a successful observation of an unreachable cluster: the
    /// failure is recorded on the row as a value, which
    /// `EnvironmentsService::observe_environment`'s own doc gives as the gear's
    /// central rule. Which kind of outcome it was is
    /// [`ObservationClass`], on the duration histogram.
    Observed,
    /// `ObservationCycleReport::failed` — `observe_environment` returned
    /// `Err`, or the environment was skipped without being attempted at all.
    ///
    /// Nothing was persisted for that environment this cycle, so its row still
    /// says whatever the last successful cycle wrote — which is why this is the
    /// count worth alerting on: a rising `failed` is a page whose contents are
    /// quietly going stale.
    Failed,
}

impl EnvironmentOutcome {
    /// Every value. See [`CycleOutcome::ALL`], including for why the allowance
    /// below sits on the constant rather than on this block.
    #[allow(dead_code, reason = "see the allowance on `CycleOutcome::ALL`")]
    pub const ALL: [Self; 2] = [Self::Observed, Self::Failed];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Failed => "failed",
        }
    }
}

/// `class` label on
/// [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION_DURATION`] — **where
/// one environment's observation ended**, at the granularity the plugin
/// contract already classifies failures at.
///
/// # The failure half is `FailureClass`, not a taxonomy invented here
///
/// Six of the nine values are `qa_product_sdk::observation::FailureClass`
/// projected one-for-one. That enum is already a closed set, already what a
/// plugin must classify its failure as before it may cross the boundary, and
/// already what this gear persists and renders. Copying it into a second
/// vocabulary here would create two partitions of the same failure space with
/// nothing keeping them in agreement; `From<FailureClass>` below has no `_`
/// arm, so a seventh class is a compile error rather than a silent fold into
/// `internal`.
///
/// # Why a class label rather than an environment label
///
/// The question this histogram exists to answer is *"is one cluster hanging,
/// or are a hundred each a little slow?"*, and the class answers it: a single
/// `unreachable` sample at the connect timeout is the first shape, a
/// hundred-sample bulge under `detected` is the second. An environment label
/// would answer *"which one"*, and it may not be asked here — see this
/// module's header for why that is a disclosure rule and not a preference,
/// and where the answer does live.
///
/// # Nine values, and the two that are not the plugin's
///
/// [`Self::Refused`] and [`Self::Failed`] cover the `Err` half of
/// `observe_environment`, which never reaches a plugin at all. They are split
/// because they want opposite responses; each carries its own reason.
///
/// # There is no value for the environment the cycle skips
///
/// A row carrying a nil tenant id is skipped before anything is contacted
/// (`run_observation_cycle`'s loop), so it records no duration sample: nothing
/// was measured, and a near-zero sample would drag down the quantile this
/// family exists to report. It is still counted, as
/// [`EnvironmentOutcome::Failed`] on the counter, so the skip is visible — the
/// two families differ by exactly that population, and
/// `an_environment_skipped_for_a_nil_tenant_is_counted_but_not_timed` pins it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationClass {
    /// The plugin returned attributes: the environment was reached and read.
    /// The healthy majority, and the population a p95 is normally read over.
    Detected,
    /// The plugin could not reach the environment's target at all.
    ///
    /// **The class this family was built for.** It is the one whose duration is
    /// paid in a connect timeout rather than in work, so a single environment
    /// in this state can dominate a whole cycle's duration while every other
    /// environment is fine.
    Unreachable,
    /// The target was reached and refused the credential — a rotated or
    /// revoked kubeconfig, typically. Fast, and fixed by re-saving the
    /// environment's credentials rather than by looking at the cluster.
    AuthRejected,
    /// The target was reached but the thing being read is not there.
    NotFound,
    /// Something was read and could not be understood.
    Malformed,
    /// The plugin's own timeout fired.
    ///
    /// Distinct from [`Self::Unreachable`] in the plugin contract and kept
    /// distinct here: one means nothing answered, the other means something
    /// answered too slowly, and the duration distributions of the two are the
    /// evidence for which.
    Timeout,
    /// Something on the plugin's side of the wire — the plugin or its
    /// configuration, not the target.
    ///
    /// The plugin contract's own residual bucket, and this gear is one of its
    /// producers: `EnvironmentsService`'s `not_observed` classifies an
    /// unresolvable product plugin and an unreadable credential as this. See
    /// `FailureClass::Internal`'s own doc, which records that this value is
    /// carrying two meanings and that this label is the wire form its split was
    /// deferred to.
    Internal,
    /// `observe_environment` returned an `Err` a rule decided.
    ///
    /// In the cycle this almost always means one thing: **the deployment's PDP
    /// has no policy for this gear's system actor.** `run_observation_cycle`'s
    /// own doc warns that a per-resource-type policy must grant it `qa.platform`
    /// `update` *and* `qa.product` `get`, and a deployment missing either
    /// observes nothing while looking healthy from every other angle. It is
    /// fixed by authoring policy, not by an incident response, which is why it
    /// is not [`Self::Failed`].
    ///
    /// An environment deleted between the cycle's enumeration and its own
    /// observation lands here too, through `EnvironmentNotFound`: it is a
    /// benign race — the row is gone and the next cycle will not attempt it —
    /// and it is emphatically not an incident, which is the only distinction
    /// this pair makes.
    Refused,
    /// `observe_environment` returned an `Err` from the database, from
    /// credstore, or from this gear's own internals. **This is the series an
    /// alert fires on.**
    Failed,
}

impl ObservationClass {
    /// Every value. See [`CycleOutcome::ALL`], including for why the allowance
    /// below sits on the constant rather than on this block.
    #[allow(dead_code, reason = "see the allowance on `CycleOutcome::ALL`")]
    pub const ALL: [Self; 9] = [
        Self::Detected,
        Self::Unreachable,
        Self::AuthRejected,
        Self::NotFound,
        Self::Malformed,
        Self::Timeout,
        Self::Internal,
        Self::Refused,
        Self::Failed,
    ];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::Unreachable => "unreachable",
            Self::AuthRejected => "auth_rejected",
            Self::NotFound => "not_found",
            Self::Malformed => "malformed",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }

    /// Whether this class rolls up to [`EnvironmentOutcome::Observed`] on
    /// [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION`].
    ///
    /// **The rule is "did an outcome get persisted", not "did it go well".**
    /// Every class the plugin can report is an `Ok` out of
    /// `observe_environment` — a failure to reach a cluster is written into the
    /// environment's row as a value, which that method's doc gives as the
    /// gear's central rule about observation — so all seven of them count as
    /// observed. Only the two `Err` classes do not.
    ///
    /// `the_two_environment_taxonomies_agree_on_what_counts_as_observed` is
    /// what keeps this in step with the report the counter is driven from.
    #[must_use]
    #[allow(dead_code, reason = "see the allowance on `CycleOutcome::ALL`")]
    pub const fn counts_as_observed(self) -> bool {
        match self {
            Self::Detected
            | Self::Unreachable
            | Self::AuthRejected
            | Self::NotFound
            | Self::Malformed
            | Self::Timeout
            | Self::Internal => true,
            Self::Refused | Self::Failed => false,
        }
    }
}

impl From<FailureClass> for ObservationClass {
    /// One-for-one, and **exhaustive with no `_` arm on purpose**: a seventh
    /// `FailureClass` must be classified by whoever adds it rather than
    /// inheriting [`Self::Internal`] by default, which is exactly the fold
    /// `FailureClass::Internal`'s own doc says has already happened once inside
    /// that enum.
    ///
    /// Here rather than beside `FailureClass` because that type lives in
    /// `qa-product-sdk`, a crate this gear only reads: an impl there would put
    /// one gear's metric label in the contract every product plugin depends on.
    /// The rule that puts a bridge beside its source is about *crate-private*
    /// sources, where the visibility mismatch is the problem; `FailureClass` is
    /// public, so the orphan rule is the only constraint and it is satisfied
    /// here.
    fn from(class: FailureClass) -> Self {
        match class {
            FailureClass::Unreachable => Self::Unreachable,
            FailureClass::AuthRejected => Self::AuthRejected,
            FailureClass::NotFound => Self::NotFound,
            FailureClass::Malformed => Self::Malformed,
            FailureClass::Timeout => Self::Timeout,
            FailureClass::Internal => Self::Internal,
        }
    }
}

impl From<&DomainError> for ObservationClass {
    /// The `Err` half of `observe_environment`, split into "a rule decided
    /// this" and "something broke".
    ///
    /// The failure side is exactly the three variants `api::rest::error`
    /// renders as an opaque 500 — `CredStore`, `Database`, `Internal` — whose
    /// text, in that module's own words, may not reach a client because it
    /// originates in a driver, in credstore, or in this gear's internals. That
    /// group is already this gear's answer to *whose failure was this*, so it
    /// is reused rather than re-decided.
    ///
    /// **Exhaustive with no `_` arm**, so a variant added to [`DomainError`]
    /// must be classified by whoever adds it. The seven caller-shaped variants
    /// below are not reachable from the observation cycle — its caller is a
    /// system actor with no request to be invalid — and are classified anyway,
    /// because this impl is on the type and not on the path.
    fn from(error: &DomainError) -> Self {
        match error {
            DomainError::CredStore(_) | DomainError::Database { .. } | DomainError::Internal(_) => {
                Self::Failed
            }
            DomainError::EnvironmentNotFound { .. }
            | DomainError::VariableNotFound { .. }
            | DomainError::EnvironmentNameExists { .. }
            | DomainError::VariableNameExists { .. }
            | DomainError::EnvironmentUnavailable { .. }
            | DomainError::EnvironmentLeased { .. }
            | DomainError::Validation { .. }
            | DomainError::LeaseConflict
            | DomainError::Forbidden => Self::Refused,
        }
    }
}

/// `class` label on
/// [`crate::domain::metrics::QA_ENVIRONMENTS_PLUGIN_CALL`] and its duration
/// histogram — **what one product plugin answered**, at the granularity the
/// plugin contract itself classifies at.
///
/// # Seven values, and every one of them is the plugin's own answer
///
/// `QaProductPluginV1::observe` returns a `PluginObservation` whose environment
/// half is either `Detected` or `Failed(PluginFailure)`, and a `PluginFailure`
/// carries a `FailureClass`. So the taxonomy is `Detected` plus the six
/// `FailureClass` values, and it is total over what a plugin can say: there is
/// no eighth end to that call.
///
/// # Why this is not [`ObservationClass`], which has the same six in it
///
/// [`ObservationClass`] labels *the whole per-environment observation*, and
/// carries two values this enum must not have — `refused` and `failed` — which
/// are the `Err` half of `observe_environment`. Those are ends the observation
/// can reach **without a plugin ever being called**: a PDP denial, a failed
/// database connection, a missing row.
///
/// Sharing one enum between the two families would put two values on this one
/// that can never be emitted, which is the "series that reads as coverage"
/// defect: a dashboard would show a permanently flat `refused` line on the
/// plugin-call family and a reader would conclude no plugin call is ever
/// refused, which is true only because the concept does not apply.
/// `From<FailureClass>` is implemented for both enums, from the same source, so
/// the six shared values cannot drift apart and
/// `the_two_class_taxonomies_agree_on_every_failure_class` is what asserts it.
///
/// # There is no value for a plugin that was never reached
///
/// Deliberately, and it is the same argument as above from the other side.
/// `observe_through_plugin` has four exits before the round trip — no product,
/// no resolvable plugin, no resolver at all, an unreadable credential — and
/// none of them emits into this family, because nothing was called and a
/// near-zero sample would corrupt the distribution this family exists to
/// report. They are still counted, as
/// [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION`]'s `failed` and as
/// [`ObservationClass::Internal`] on the observation histogram, so the
/// population is not lost — the two families differ by exactly that set, and
/// that difference is itself the signal that resolution rather than the plugin
/// is what is failing.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCallClass {
    /// The plugin returned attributes: it reached the target and read it.
    /// The healthy majority, and the population a p95 is normally read over.
    Detected,
    /// The plugin could not reach the target at all.
    ///
    /// The class whose duration is paid in a connect timeout rather than in
    /// work, so one environment in this state can dominate a whole cycle.
    Unreachable,
    /// The target was reached and refused the credential.
    AuthRejected,
    /// The target was reached but the thing being read is not there.
    NotFound,
    /// Something was read and could not be understood.
    Malformed,
    /// The plugin's own timeout fired.
    ///
    /// Distinct from [`Self::Unreachable`] in the plugin contract and kept
    /// distinct here: one means nothing answered, the other means something
    /// answered too slowly, and on *this* family — which contains the round
    /// trip and nothing else — the duration distributions of the two are the
    /// cleanest evidence available for which.
    Timeout,
    /// Something on the plugin's side of the wire — the plugin or its
    /// configuration, not the target.
    ///
    /// The plugin contract's own residual bucket. **On this family it means
    /// only what a plugin said**, unlike [`ObservationClass::Internal`], which
    /// this gear also produces for an unresolvable plugin or an unreadable
    /// credential. That is the sharpest illustration of why the two enums are
    /// separate: the same word answers a different question on each family.
    Internal,
}

impl PluginCallClass {
    /// Every value. See [`CycleOutcome::ALL`], including for why the allowance
    /// below sits on the constant rather than on this block.
    #[allow(dead_code, reason = "see the allowance on `CycleOutcome::ALL`")]
    pub const ALL: [Self; 7] = [
        Self::Detected,
        Self::Unreachable,
        Self::AuthRejected,
        Self::NotFound,
        Self::Malformed,
        Self::Timeout,
        Self::Internal,
    ];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::Unreachable => "unreachable",
            Self::AuthRejected => "auth_rejected",
            Self::NotFound => "not_found",
            Self::Malformed => "malformed",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
        }
    }
}

impl From<FailureClass> for PluginCallClass {
    /// One-for-one, and **exhaustive with no `_` arm on purpose**: a seventh
    /// `FailureClass` must be classified by whoever adds it rather than
    /// inheriting [`Self::Internal`] by default.
    ///
    /// Here rather than beside `FailureClass` for the reason
    /// `From<FailureClass> for ObservationClass` gives: that type lives in
    /// `qa-product-sdk`, a crate this gear only reads, and an impl there would
    /// put one gear's metric label in the contract every product plugin depends
    /// on.
    fn from(class: FailureClass) -> Self {
        match class {
            FailureClass::Unreachable => Self::Unreachable,
            FailureClass::AuthRejected => Self::AuthRejected,
            FailureClass::NotFound => Self::NotFound,
            FailureClass::Malformed => Self::Malformed,
            FailureClass::Timeout => Self::Timeout,
            FailureClass::Internal => Self::Internal,
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Port trait
// ════════════════════════════════════════════════════════════════════

/// The observation cycle's telemetry — the cycle, the environments it reported
/// on, and each environment's own round trip.
///
/// # Implementations must not fail a caller
///
/// Every method returns `()`, takes `&self`, and is called from a path whose
/// behaviour must not depend on whether metrics are wired. An implementation
/// must not panic, must not block, and has no error to propagate by
/// construction. Dropping a sample is the correct response to an adapter that
/// cannot record one. See this module's header, and
/// `domain::service::emit`, which is where that contract stops being a promise.
pub trait ObservationMetrics: Send + Sync + 'static {
    /// One **observation cycle** — `EnvironmentsService::run_observation_cycle`
    /// — with how it ended and the wall-clock time it took. Counts into
    /// [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION_CYCLE`] and
    /// observes into
    /// [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION_CYCLE_DURATION`]
    /// — one call, two instruments, so the rate and the quantile can never
    /// disagree about how many cycles there were.
    fn observation_cycle(&self, outcome: CycleOutcome, duration: Duration);

    /// One of `ObservationCycleReport`'s two counters, as it stands at the end
    /// of a cycle. Adds `count` to
    /// [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION`] under
    /// `outcome`'s label.
    ///
    /// **Called from the report, not from the loop.** The report is the value
    /// the ticker already logs at `debug!`, so driving the series from its
    /// fields is what makes the log line and the series structurally unable to
    /// disagree: neither can move without the other. A counter incremented
    /// inside the loop would be a second tally of the same events, correct
    /// today and unpoliced afterwards.
    ///
    /// Called once per value of [`EnvironmentOutcome`] per cycle, **including
    /// with `count == 0`**: a zero addition creates the series, and a series
    /// that exists and reads zero is a different answer from a series that is
    /// absent.
    fn cycle_environments(&self, outcome: EnvironmentOutcome, count: u32);

    /// One environment's own observation inside a cycle, with where it ended
    /// and the wall-clock time it took. Observes into
    /// [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION_DURATION`].
    ///
    /// **Per environment, and that is the point of the family** — see
    /// [`ObservationClass`]'s header and the constant's own doc. It is the one
    /// per-item emission in this gear's catalog, and it is bounded: the label
    /// is a closed nine-value set, so a deployment with ten thousand
    /// environments contributes ten thousand samples across the same nine
    /// series.
    ///
    /// It drives the histogram alone rather than a counter as well. The
    /// histogram's own per-label count already *is* the per-class rate, so a
    /// counter here would be a third tally of the same events; the counter of
    /// the same stem is [`Self::cycle_environments`], which carries the coarser
    /// label the report can supply.
    fn environment_observed(&self, class: ObservationClass, duration: Duration);
}

/// The plugin boundary's telemetry: one call into code this deployment does not
/// own.
///
/// # Implementations must not fail a caller
///
/// The method returns `()`, takes `&self`, and is called from a path whose
/// behaviour must not depend on whether metrics are wired. An implementation
/// must not panic, must not block, and has no error to propagate by
/// construction. Dropping a sample is the correct response to an adapter that
/// cannot record one. See this module's header, and `domain::service::emit`,
/// which is where that contract stops being a promise.
pub trait PluginMetrics: Send + Sync + 'static {
    /// One `QaProductPluginV1::observe` round trip, with what the plugin
    /// answered and the wall-clock time it took. Counts into
    /// [`crate::domain::metrics::QA_ENVIRONMENTS_PLUGIN_CALL`] and observes
    /// into [`crate::domain::metrics::QA_ENVIRONMENTS_PLUGIN_CALL_DURATION`] —
    /// one call, two instruments, so the rate and the quantile can never
    /// disagree about how many plugin calls there were.
    ///
    /// **The duration is the round trip and nothing else**, which is the whole
    /// point of the family and the one thing an implementation must not be
    /// asked to widen: it is nested inside
    /// [`crate::domain::metrics::QA_ENVIRONMENTS_OBSERVATION_DURATION`], and
    /// the two are useful precisely because their difference is everything
    /// this gear does *around* a plugin.
    ///
    /// **Per call, and that is bounded**: the label is a closed seven-value
    /// set, so a deployment with ten thousand environments contributes ten
    /// thousand samples across the same seven series.
    fn plugin_call(&self, class: PluginCallClass, duration: Duration);
}

// ════════════════════════════════════════════════════════════════════
//  No-op implementation
// ════════════════════════════════════════════════════════════════════

/// No-op implementation of the port.
///
/// Two uses, and the first is the one that keeps the "metrics must not change
/// behaviour" promise structural rather than documented:
///
/// * the safe default before an adapter is constructed, so a service holding
///   it emits every signal a wired service does and nothing observes;
/// * the in-test default for the many tests in this crate that do not assert
///   on metrics.
///
/// Zero-sized; an `Arc<NoopMetrics>` shares for free.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl ObservationMetrics for NoopMetrics {
    fn observation_cycle(&self, _outcome: CycleOutcome, _duration: Duration) {}
    fn cycle_environments(&self, _outcome: EnvironmentOutcome, _count: u32) {}
    fn environment_observed(&self, _class: ObservationClass, _duration: Duration) {}
}

impl PluginMetrics for NoopMetrics {
    fn plugin_call(&self, _class: PluginCallClass, _duration: Duration) {}
}
