//! qa-catalog observability port — the typed metric-emission trait for product
//! plugin resolution.
//!
//! [`PluginResolutionMetrics`] owns the whole catalog declared in
//! [`crate::domain::metrics`]. One infra adapter
//! ([`crate::infra::metrics::QaCatalogMetricsMeter`]) implements it; `gear.rs`
//! hands [`crate::domain::service::QaProductRegistry`] an `Arc<dyn _>` view.
//!
//! # Why the timing lives here, at the caller, and not inside the plugins
//!
//! **This is the design decision this module exists to record.** The plugin
//! boundary is where a qa-platform deployment meets code it does not own, and
//! there were two places to measure it.
//!
//! The alternative was to instrument the plugins: have every implementation of
//! `qa_product_sdk::QaProductPluginV1` time and count its own work. That is
//! more faithful per plugin, and it is the wrong trade here for two reasons
//! that are both about `qa-product-sdk` rather than about metrics.
//!
//! * **It would widen the contract every plugin author must satisfy.**
//!   `qa-product-sdk` is deliberately narrow — its own feature documentation
//!   argues that the crate gates its own code rather than growing its
//!   dependency graph, and `qa-plugin-k8s` exists precisely so that the one
//!   heavy dependency in this subsystem sits in a crate only the plugins that
//!   need it link. Putting an `opentelemetry` dependency in the SDK would put
//!   it in every plugin, including plugins written outside this repository.
//! * **The caller can answer the question completely.** What a dashboard needs
//!   is *how long does this deployment's plugin take, and how often does it
//!   fail*, and both are visible from outside the call: the duration is the
//!   round trip, and the failure class is the value the plugin contract already
//!   requires a plugin to classify its failure as before it may cross the
//!   boundary. Nothing a plugin knows about itself is needed to produce either.
//!
//! What the caller genuinely cannot see is *why* a plugin was slow — which of
//! its own internal steps cost the seconds. That is a real loss, it is the
//! thing plugin-side instrumentation would have bought, and a plugin that wants
//! it can still emit its own metrics without any of this changing. It is
//! recorded here so the trade is visible rather than implied.
//!
//! # Two gears, two halves of one boundary
//!
//! This gear measures **resolution**: which plugin owns this product.
//! qa-environments measures the **round trip**: what that plugin then did. They
//! are two families in two catalogs on purpose — the failures are different
//! (a resolution fails because a gear is missing from the deployment; a round
//! trip fails because a target is unreachable), and they are paid by different
//! callers.
//!
//! They are also both **nested inside** qa-environments' per-environment
//! observation duration, which contains a resolution measured here plus a
//! credential read plus the round trip plus two database calls. All three docs
//! say so. Adding any two of them together counts the same seconds twice.
//!
//! # Cardinality: what is not a label, and the one that needed an argument
//!
//! No product id, no tenant id, no plugin GTS **instance** id. The first two
//! are the ordinary rule. The third is the one worth writing down, because the
//! instance id is right there in the value being resolved and it is the obvious
//! thing to label with.
//!
//! It is not a label because it is **a string this gear reads out of a database
//! column**. `qa_products.plugin_instance_id` is `VARCHAR(512)`; a product's
//! binding is checked against the running process's registrations when it is
//! *written* (`ProductPluginPresence`), and nothing re-checks a row written by
//! a migration, by an operator, or by a build that carried a plugin this one
//! does not. A metric label fed from such a column is bounded by nothing the
//! type system can see, and an unbounded label is not a mistake that shows up
//! as an error — it shows up as a Prometheus instance that stops ingesting.
//!
//! ## Why the plugin's GTS *type* id is not a label either — measured
//!
//! The natural repair is to label by the plugin's type rather than its
//! instance, on the argument that a type id is a closed set where an instance
//! id is not. That argument is sound and the conclusion does not follow here,
//! because in this platform the type id is a set of **one**:
//!
//! * `qa-product-sdk` declares exactly one product-plugin spec type,
//!   `QaProductPluginSpecV1`, whose `TYPE_ID` is a compile-time constant.
//! * `PluginV1::build_registration` composes an instance id as that `TYPE_ID`
//!   followed by the plugin's own instance segment, so **every** product plugin
//!   in any deployment carries the same type id and differs only in the tail.
//! * `QaProductRegistry::list_registered_plugins` relies on exactly that: it
//!   filters the types-registry listing by equality against that one constant.
//!
//! So a `type` label would be a dimension with one value in every deployment —
//! a constant that costs a label key and answers nothing — while the part that
//! actually distinguishes one plugin from another is the instance tail, which
//! is the value ruled out above.
//!
//! **What separates "the plugin is slow" from "the gear is slow" is therefore
//! the nesting, not a label**, and the nesting does separate them completely:
//! qa-environments' plugin-call duration contains the plugin's work and nothing
//! else, and its observation duration contains everything around it. That is
//! the distinction the boundary needed. If this platform ever grows a second
//! product-plugin spec type, this argument's premise fails and a type label
//! becomes worth its key; that is the condition to revisit it under, stated so
//! that the next person does not have to re-derive it.
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
//! no adapter is installed" means: a registry constructed without the adapter
//! still emits, and nothing observes.

use std::time::Duration;

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

// ════════════════════════════════════════════════════════════════════
//  Label-value taxonomy
// ════════════════════════════════════════════════════════════════════

/// `outcome` label on
/// [`crate::domain::metrics::QA_CATALOG_PLUGIN_RESOLUTION`] and its duration
/// histogram — **how one product-plugin resolution ended**.
///
/// Four values, read off `QaProductRegistry::plugin_for`'s own control flow: it
/// has one policy check, one connection, one row read that may find nothing,
/// one `ClientHub` probe that may find nothing, and one tail.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginResolutionOutcome {
    /// A plugin object came back.
    Resolved,
    /// The product named a plugin **this binary does not register**.
    ///
    /// **The value this family was worth building for.** It is a fact about how
    /// the deployment was composed, not about the request: the product row is
    /// fine, and the gear that registers that instance id is missing or was not
    /// linked in. Today it is visible only as one `warn!` line per attempt
    /// inside `plugin_for`, which means a deployment in this state observes
    /// nothing through that product and looks healthy from every other angle —
    /// the same shape of blind spot qa-environments' `unstarted` cycle closes.
    ///
    /// Its own value rather than folded into [`Self::Refused`] because the two
    /// are fixed by different people: this one by shipping a gear, that one by
    /// authoring policy or by asking about a product that exists.
    Unregistered,
    /// A rule decided there was nothing to resolve: policy denied the product
    /// read, or the product is not visible to this caller and therefore reads
    /// as absent.
    ///
    /// Not an incident. The product read is tenant-scoped, so "no such product"
    /// and "not yours" are deliberately the same answer — see `plugin_for`'s
    /// own doc — and both are facts about the caller.
    Refused,
    /// The database, the credential store, or this gear's own internals broke.
    /// **This is the series an alert fires on.**
    Failed,
}

impl PluginResolutionOutcome {
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
    pub const ALL: [Self; 4] = [
        Self::Resolved,
        Self::Unregistered,
        Self::Refused,
        Self::Failed,
    ];

    /// The label value, as it appears in the series.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Unregistered => "unregistered",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }
}

impl From<&DomainError> for PluginResolutionOutcome {
    /// The `Err` half of `plugin_for`, split three ways.
    ///
    /// `ProductPluginUnavailable` is lifted out first because it is the one
    /// failure that is about the *deployment* rather than about the caller or
    /// about this gear; see [`Self::Unregistered`].
    ///
    /// The remaining split is the one `api::rest::error` already makes: the
    /// failure side is exactly the variants that module renders as a 5xx —
    /// `CredStore`, `Storage`, `Database`, `Internal`, and `SyncFailed`'s 503 —
    /// whose text, in that module's own words, originates in a driver, in
    /// credstore, or in this gear's internals. That group is already this
    /// gear's answer to *whose failure was this*, so it is reused rather than
    /// re-decided, and
    /// `api::rest::error`'s `the_metric_label_agrees_with_what_the_api_may_disclose`
    /// is what stops the two drifting.
    ///
    /// **Exhaustive with no `_` arm**, so a variant added to [`DomainError`]
    /// must be classified by whoever adds it. Most of the caller-shaped
    /// variants below are not reachable from `plugin_for` at all — it neither
    /// syncs a repository nor writes a row — and are classified anyway, because
    /// this impl is on the type and not on the path.
    fn from(error: &DomainError) -> Self {
        match error {
            DomainError::ProductPluginUnavailable { .. } => Self::Unregistered,
            DomainError::CredStore(_)
            | DomainError::Storage(_)
            | DomainError::Database { .. }
            | DomainError::Internal(_)
            | DomainError::SyncFailed { .. } => Self::Failed,
            DomainError::NotFound { .. }
            | DomainError::PlanYamlInvalid { .. }
            | DomainError::PlanNotFound { .. }
            | DomainError::FileNotFound { .. }
            | DomainError::RepoNotSynced { .. }
            | DomainError::Validation { .. }
            | DomainError::RepositoryNameExists { .. }
            | DomainError::CustomPlanNameExists { .. }
            | DomainError::ProductNameExists { .. }
            | DomainError::SshKeyNameExists { .. }
            | DomainError::BranchCacheConflict { .. }
            | DomainError::Forbidden => Self::Refused,
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Port trait
// ════════════════════════════════════════════════════════════════════

/// The plugin boundary's telemetry, from the side that owns the binding.
///
/// # Implementations must not fail a caller
///
/// The method returns `()`, takes `&self`, and is called from a path whose
/// behaviour must not depend on whether metrics are wired. An implementation
/// must not panic, must not block, and has no error to propagate by
/// construction. Dropping a sample is the correct response to an adapter that
/// cannot record one. See this module's header, and `domain::service::emit`,
/// which is where that contract stops being a promise.
pub trait PluginResolutionMetrics: Send + Sync + 'static {
    /// One product-plugin resolution — `QaProductRegistry::plugin_for` — with
    /// how it ended and the wall-clock time it took. Counts into
    /// [`crate::domain::metrics::QA_CATALOG_PLUGIN_RESOLUTION`] and observes
    /// into
    /// [`crate::domain::metrics::QA_CATALOG_PLUGIN_RESOLUTION_DURATION`] — one
    /// call, two instruments, so the rate and the quantile can never disagree
    /// about how many resolutions there were.
    ///
    /// **Per resolution, not per cycle**, and that is bounded on purpose: the
    /// label is a closed four-value set, so a deployment resolving a hundred
    /// environments' plugins every tick contributes a hundred samples across
    /// the same four series.
    fn plugin_resolution(&self, outcome: PluginResolutionOutcome, duration: Duration);
}

// ════════════════════════════════════════════════════════════════════
//  No-op implementation
// ════════════════════════════════════════════════════════════════════

/// No-op implementation of the port.
///
/// Two uses, and the first is the one that keeps the "metrics must not change
/// behaviour" promise structural rather than documented:
///
/// * the safe default before an adapter is constructed, so a registry holding
///   it emits every signal a wired registry does and nothing observes;
/// * the in-test default for the many tests in this crate that do not assert
///   on metrics.
///
/// Zero-sized; an `Arc<NoopMetrics>` shares for free.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl PluginResolutionMetrics for NoopMetrics {
    fn plugin_resolution(&self, _outcome: PluginResolutionOutcome, _duration: Duration) {}
}
