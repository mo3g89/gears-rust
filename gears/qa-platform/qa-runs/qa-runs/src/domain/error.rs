//! The gear's domain error type.
//!
//! Started at Task 5 with only the variants the domain layer needs, so later
//! tasks extend rather than invent it.
//!
//! # Who constructs what, as of Task 13
//!
//! This header previously read *"Nothing constructs these variants yet — the
//! exclusivity core is pure and infallible."* That was true when Task 5 wrote it
//! and false from Task 9 onward; Task 13 edited this file and left it standing.
//! The current map, by module rather than by line so it does not rot on the next
//! edit:
//!
//! * `infra::storage::mapper` — [`DomainError::CorruptState`], and
//!   [`DomainError::Validation`] for the one out-of-range count it refuses.
//! * `domain::cron::parse_cron` — [`DomainError::InvalidCron`], and it is that
//!   function's only error. Task 18 added it.
//! * `domain::params::validate`, through the `#[from]` on
//!   [`DomainError::InvalidParameters`], reached by `launch`'s first step.
//! * `infra::storage::{runs_sea_repo, queue_sea_repo}` —
//!   [`DomainError::RunNameExists`], [`DomainError::QueueRowExists`].
//! * `infra::storage::schedules_sea_repo` —
//!   [`DomainError::ScheduleNameExists`], from both `create` and `update`. Task
//!   17 added it; the same module's `claim_tick` deliberately constructs
//!   **nothing**, mapping its unique violation to `Ok(None)` because a lost
//!   claim race is the expected path rather than a failure.
//! * `domain::repos::schedules_repo::SchedulesRepository::resolve_owned_schedule`
//!   — [`DomainError::ScheduleNotFound`], for absent and foreign alike. The
//!   schedules counterpart of `RunsRepository::resolve_owned`.
//! * `domain::repos::runs_repo::RunsRepository::resolve_owned` —
//!   [`DomainError::RunNotFound`], for absent and foreign alike.
//! * `infra::executor` — [`DomainError::ExecutorFailed`].
//! * `domain::service::launch` — [`DomainError::AmbiguousBranch`] (the
//!   multi-repository guard), [`DomainError::Validation`] (a custom plan with no
//!   files, so no repository default branch applies),
//!   [`DomainError::IllegalTransition`] (a state move the machine or the guarded
//!   `UPDATE` refused), [`DomainError::Internal`] (an exhausted name retry, and
//!   an admission outcome that is not representable), and
//!   [`DomainError::Catalog`] / [`DomainError::Environments`] (the two cross-gear
//!   reads).
//! * The `Admitter` seam, implemented by Task 14 — [`DomainError::QueueFull`],
//!   [`DomainError::ConcurrencyLimit`].
//! * `domain::service::ingest` — [`DomainError::IllegalTransition`] for a
//!   completion whose run is in a state that cannot reach the derived verdict,
//!   and [`DomainError::RunNotFound`] for an event about a run the caller cannot
//!   see.
//! * `domain::service::runs` — [`DomainError::QueueRowNotFound`] and
//!   [`DomainError::QueueRowNotQueued`] (the two queue operator actions),
//!   [`DomainError::Validation`] for a re-run of a run that records no branch,
//!   and [`DomainError::Internal`] for a queue row outside the readable window.
//! * The two `From` impls below — [`DomainError::Database`],
//!   [`DomainError::Internal`], [`DomainError::Forbidden`].
//!   [`DomainError::Forbidden`] has **no** other constructor: it is only ever an
//!   `EnforcerError` denial or compile failure.
//!
//! **Every variant now has a constructor.** This list ended, through Task 14,
//! with *"Still unconstructed, and named so the list stays falsifiable:
//! `QueueRowNotFound` and `QueueRowNotQueued`, both of which belong to Task 15's
//! queue operator actions."* Task 15 landed them, so the sentence is replaced
//! rather than left standing — the previous header made exactly this mistake and
//! says so two paragraphs up.

use qa_runs_sdk::{QueueState, RunState};
use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_macros::domain_model;
use uuid::Uuid;

// The two resource types the mapping below shares with
// `domain::error_attribution`'s call-site wrappers -- imported rather than
// re-declared so both raise the same `gts_id` per resource.
use crate::domain::error_attribution::{QueueResourceError, ScheduleResourceError};

/// Domain-specific errors using thiserror
#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    #[error("run {id} not found")]
    RunNotFound { id: Uuid },

    #[error("queue row {id} not found")]
    QueueRowNotFound { id: Uuid },

    /// A schedule does not exist **or** is not visible in the caller's scope.
    ///
    /// The two are deliberately indistinguishable, for the reason
    /// [`Self::RunNotFound`] carries: telling them apart is a cross-tenant
    /// existence oracle. Constructed only by
    /// `domain::repos::SchedulesRepository::resolve_owned_schedule`.
    #[error("schedule {id} not found")]
    ScheduleNotFound { id: Uuid },

    #[error("validation failed on {field}: {message}")]
    Validation { field: String, message: String },

    /// A cron expression is not five-field POSIX cron, or is one this evaluator
    /// refuses. Constructed only by `domain::cron::parse_cron`.
    ///
    /// Its own variant rather than a [`Self::Validation`] on a `cron` field
    /// because the two callers differ: a schedule's expression is rejected at
    /// create time *and* re-parsed on every evaluation, where there is no
    /// request field to name.
    ///
    /// **`message` is always `domain::cron`'s own text**, never the parser's:
    /// the `cron` crate renders an error as nothing but the expression it was
    /// handed, which is the *normalised* six-field form and so would report
    /// something the operator did not write. That is also what makes the whole
    /// variant disclosable — see [`Self::disclosable`] — since neither field
    /// carries another system's vocabulary.
    #[error("invalid cron expression {expression:?}: {message}")]
    InvalidCron { expression: String, message: String },

    /// A launch or re-run carried an illegal parameter set.
    ///
    /// Kept as its own variant wrapping [`crate::domain::params::ParamError`]
    /// rather than flattened into `Validation`, so the offending parameter's
    /// name survives to the message — `cpt-cf-qa-fr-runs-params` requires the
    /// failure to name it, and a `{field, message}` pair would force the caller
    /// to re-render what the error already knows.
    #[error("invalid launch parameters: {0}")]
    InvalidParameters(#[from] crate::domain::params::ParamError),

    /// A persisted column did not decode.
    ///
    /// Every decoder in `infra::storage::mapper` fails closed into this
    /// variant rather than falling back to a default, and every one of them
    /// names the offending row: a run whose `parameters` quietly decoded to
    /// `[]` would execute with the wrong environment, one whose target decoded
    /// to nothing would execute nothing while reporting success, and neither
    /// is diagnosable without the id.
    ///
    /// `what` is a `&'static str` because it always names a column
    /// (`run.state`, `queue.state`, …) chosen at the call site, never
    /// caller-controlled text.
    #[error("corrupt persisted state in {what} for row {id}: {value}")]
    CorruptState {
        what: &'static str,
        id: Uuid,
        value: String,
    },

    #[error("run '{name}' already exists")]
    RunNameExists { name: String },

    /// `(tenant_id, name)` on `qa_schedules` collided.
    ///
    /// Its own variant rather than a reuse of [`Self::RunNameExists`]: the two
    /// name a different resource, and a caller told "run 'nightly' already
    /// exists" when it was creating a *schedule* would look for the wrong row.
    /// Both indexes are tenant-prefixed, so both may safely quote the name.
    #[error("schedule '{name}' already exists")]
    ScheduleNameExists { name: String },

    #[error("run {run_id} already has a queue row")]
    QueueRowExists { run_id: Uuid },

    /// A conditional state update matched no row: the run had already left
    /// `state`, so the caller's transition is no longer legal. Constructed by
    /// the services (Tasks 13-15) from `RunsRepository::update_state`'s
    /// `false`; the repository itself reports only whether the guarded update
    /// matched.
    ///
    /// **`state` is observed after the fact and is advisory** — the row may
    /// have moved again between the failed update and the read that produced
    /// it. See `domain::repos`, "Guarded writes return `bool`": nothing may
    /// branch on this value; it exists so the message names something.
    #[error("run {} is in state {} and cannot {action}", id, state.as_str())]
    IllegalTransition {
        id: Uuid,
        state: RunState,
        action: String,
    },

    /// The `AND state = 'queued'` guard on a queue-row write matched nothing.
    /// Same split as [`Self::IllegalTransition`]: the repository returns
    /// `false`, the service names the row, and the state it names is advisory.
    #[error("queue row {} is {}, not queued", id, state.as_str())]
    QueueRowNotQueued { id: Uuid, state: QueueState },

    /// The execution plane could not be reached, or refused a request.
    ///
    /// **Never means "the execution is gone."** That distinction is the whole
    /// fail-safe direction of
    /// [`crate::domain::ports::run_executor::RunExecutor`]: an unreadable
    /// executor must leave a platform claim held, because releasing it is what
    /// lets a second run start beside an exclusive one — the source system
    /// treats an unreadable Argo as *busy* for exactly this reason
    /// (`manager/src/services/run_dispatcher.rs:237-252`) and skips the whole
    /// dispatcher tick rather than acting on an empty answer
    /// (`run_dispatcher.rs:353-359`). An execution that has genuinely ended is
    /// reported by *absence* from `list_active`, never by this error.
    ///
    /// A `String` rather than a structured cause, matching [`Self::Database`]:
    /// the executor's failures are another system's, and enumerating them here
    /// would be a vocabulary this gear cannot keep current.
    #[error("execution plane error: {0}")]
    ExecutorFailed(String),

    /// The queue is at `queue_max_depth` for this environment, **as counted
    /// under the caller's own access scope**.
    ///
    /// Not "the environment's queue", which is what this line used to say
    /// (as "the platform's queue", before Task 25's wire rename). `queued_depth`
    /// is a scoped count, so the limit binds per (scope, environment) rather
    /// than per environment: two tenants sharing a `platform_id` each get
    /// their own budget, and a hierarchical policy whose scope admits several
    /// tenants counts all of them while `insert` stamps only
    /// `subject_tenant_id`. See `service::admission`'s depth check and
    /// `DESIGN.md` §3.7 for why the queries are deliberately left that way.
    ///
    /// Kept distinct from [`Self::ConcurrencyLimit`] rather than folded into one
    /// "too many runs" error because the frozen guide enumerates **exactly two**
    /// causes for the launch path's 429 and gives each its own knob (guide lines
    /// 74, 240-242). One variant could not name which setting an operator has to
    /// change, which is the only actionable content either message has.
    #[error("run queue for environment {platform_id} is full: {queued} of {limit} slots used")]
    QueueFull {
        platform_id: Uuid,
        queued: usize,
        limit: u32,
    },

    /// The cluster-wide `max_concurrent_runs` cap is reached. The other of the
    /// guide's two 429 causes; see [`Self::QueueFull`].
    #[error("max concurrent runs limit reached ({limit})")]
    ConcurrencyLimit { limit: u32 },

    /// A custom plan spans more than one repository and the launch named no
    /// branch, so no single repository `default_branch` applies (parity spec
    /// §3.4 rule 3; `manager/src/routes/custom_plans.rs:671-687`).
    ///
    /// Maps to `FailedPrecondition`, i.e. **HTTP 400** and not 409 — which is
    /// also what the source system answers (`StatusCode::BAD_REQUEST` at
    /// `custom_plans.rs:681`). The caller can fix it by naming a branch; nothing
    /// about the server's state conflicts.
    #[error(
        "custom plan {plan_id} spans {groups} repositories, so a branch must be given \
         explicitly: no single repository default applies"
    )]
    AmbiguousBranch { plan_id: Uuid, groups: usize },

    /// qa-catalog refused or could not answer.
    ///
    /// A `String` rather than a re-export of `qa_catalog_sdk::QaCatalogError`,
    /// matching [`Self::Database`] and [`Self::ExecutorFailed`]: the failure is
    /// another gear's, and mirroring its vocabulary here would be a contract
    /// this gear cannot keep current.
    ///
    /// **Not every catalog failure becomes one of these.** A catalog error
    /// raised while *gathering exclusivity inputs* is swallowed — the launch
    /// resolves parallel and continues, exactly as the source system does
    /// (`manager/src/services/exclusivity.rs:177-179`). This variant carries the
    /// failures that genuinely stop a launch, such as an unresolvable plan.
    #[error("catalog error: {0}")]
    Catalog(String),

    /// qa-environments refused or could not answer. Same shape and reasoning as
    /// [`Self::Catalog`].
    ///
    /// A launch whose platform cannot be read **fails**, and that is deliberate:
    /// `qa_environment_leases` (renamed from `qa_platform_leases`) is keyed on a
    /// bare `platform_id` and is not tenant-partitioned, so persisting an
    /// unverified platform id would let one
    /// tenant's run take the global lease on another tenant's platform. See
    /// `domain::repos::NewQueueRow::platform_id`.
    #[error("environments error: {0}")]
    Environments(String),

    #[error("access denied")]
    Forbidden,

    /// A storage failure: the driver's own message, plus the error it came
    /// from when there was a typed one.
    ///
    /// # Why there is a boxed source here — review finding #25
    ///
    /// This was `Database(String)`, so `From<toolkit_db::DbError>` kept
    /// `e.to_string()` and dropped the error itself: `.source()` returned
    /// `None`, and anything a caller might have wanted from the original —
    /// `sea_orm::DbErr`'s SQLSTATE, a `sqlx::Error`'s constraint name, the
    /// whole cause chain rendered into a `tracing` field with `{:?}` — was
    /// unrecoverable by the time the error left `infra::storage`. The
    /// `TODO(DE1302)` that used to sit above the `From` impl below named
    /// exactly this fix.
    ///
    /// `message` keeps the rendered text and the `Display` string is
    /// unchanged (`"database error: {message}"`), so every response body, log
    /// line and persisted `error` column this variant reaches reads exactly as
    /// it did before.
    #[error("database error: {message}")]
    Database {
        /// The driver's rendered message — `DbError::to_string()` for a
        /// converted error, or the text a caller had in hand for one built
        /// with [`DomainError::database`].
        message: String,
        /// The error this was converted from. `From<toolkit_db::DbError>`
        /// always sets it; `None` for a failure that only ever existed as text
        /// (a `toolkit_odata::Error::Db` string, a test fixture), which is why
        /// this is an `Option` rather than a required field.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("internal error: {0}")]
    Internal(String),
}

/// What a persisted `error` field carries when the real cause is not the
/// caller's to see.
///
/// Deliberately says nothing. An operator who needs the cause reads the `warn!`
/// the redacting call site emits, which names the row.
pub(crate) const OPAQUE_ERROR_TEXT: &str =
    "the operation failed; see the service log for the cause";

impl DomainError {
    /// Whether this error's `Display` text may be **persisted to a row**, or
    /// must be replaced by [`OPAQUE_ERROR_TEXT`].
    ///
    /// # Why the decision lives here and not at the call site
    ///
    /// It started life as a private helper in `domain::service::launch`, which
    /// closed the one leak that had been found — `abandon` writing
    /// `cause.to_string()` into `qa_runs.error` verbatim, where a
    /// [`Self::Database`] cause carried raw driver text (index names, column
    /// names, key values). But
    /// `domain::repos::RunStatePatch::error` is written from more than one module.
    /// `dispatch::record_failed` is the second caller of this rule, and it arrived
    /// exactly as predicted: a private helper in a sibling module could not be
    /// reached from it, so it would have re-opened the identical leak — a
    /// `LeaseUnavailable(String)` carrying qa-environments' raw text, recorded via
    /// `RunStatePatch { error: Some(cause.to_string()), .. }`, disclosed on both
    /// boundaries, with the launch path's classification never consulted.
    ///
    /// So it lives **beside the variant list**, where somebody adding a variant
    /// sees it.
    ///
    /// # Three writers of that column do **not** call this, and that is correct
    ///
    /// **Corrected 2026-08-14 by Task 15**, which found this paragraph predicting
    /// that its cancel "records a cancellation reason" through this rule. It does
    /// not, because there is no error to classify:
    ///
    /// * `dispatch::reclaim_overdue` writes `TIMEOUT_REASON`, and
    ///   `service::runs`' cancel writes its own two cancellation constants. Each
    ///   is this gear's own sentence about the caller's own run, so it is
    ///   disclosable by construction — this method classifies [`DomainError`]s,
    ///   and passing a `&'static str` through it would require inventing one.
    /// * `service::ingest` writes `ExecutionEvent::Finished::message`, which **is**
    ///   another system's text and is nevertheless persisted verbatim. That is a
    ///   deliberate exception, not an oversight: the port defines that field as
    ///   the executor's *operator-facing reason* and documents it as landing in
    ///   `qa_runs.error`, which is where the source system puts the workflow's
    ///   `status.message` (`manager/src/services/argo.rs:2280-2283`). Redacting it
    ///   would delete the only description a failed run has. The distinction this
    ///   rule actually draws is between a curated message and an *error value*
    ///   whose `Display` was never written for a caller — and it is worth stating
    ///   that the executor adapter, not this enum, is what has to keep the former
    ///   true.
    ///
    /// # The match is exhaustive on purpose
    ///
    /// No `_` arm. A catch-all would have silently classified a new variant as
    /// opaque, which is the safe direction but still a decision made by absence;
    /// the earlier version of this rule claimed exactly that ("a variant added
    /// later is opaque until somebody decides it is safe") and it was true but
    /// weaker than it could be. Exhaustive means **adding a variant is a compile
    /// error here**, so the author decides rather than inherits.
    ///
    /// # The rule the classification follows
    ///
    /// Disclose only what is already the caller's own data or is an actionable
    /// name — a limit they hit, a field they sent, a state their run is in.
    /// Redact anything whose text originates in another system or in this gear's
    /// internals. [`Self::CorruptState`] sets the standard the whole way back at
    /// Task 5: its `what` is a `&'static str` *"because it always names a column
    /// … never caller-controlled text"* — but its `value` is the offending column
    /// contents, which is exactly what must not travel.
    pub(crate) const fn disclosable(&self) -> bool {
        match self {
            // The caller's own request, or their own row's state. Every one of
            // these is actionable and contains nothing they did not supply or
            // already own.
            Self::RunNotFound { .. }
            | Self::QueueRowNotFound { .. }
            | Self::ScheduleNotFound { .. }
            | Self::Validation { .. }
            | Self::InvalidCron { .. }
            | Self::InvalidParameters(_)
            | Self::RunNameExists { .. }
            | Self::ScheduleNameExists { .. }
            | Self::QueueRowExists { .. }
            | Self::IllegalTransition { .. }
            | Self::QueueRowNotQueued { .. }
            | Self::QueueFull { .. }
            | Self::ConcurrencyLimit { .. }
            | Self::AmbiguousBranch { .. }
            | Self::Forbidden => true,

            // Text that originates outside this gear or inside its internals.
            // `CorruptState` carries a persisted column's contents; `Catalog` and
            // `Environments` wrap another gear's error vocabulary;
            // `ExecutorFailed` wraps the execution plane's; `Database` is
            // `From<toolkit_db::DbError>` and carries driver text verbatim;
            // `Internal` is by definition not a contract.
            Self::CorruptState { .. }
            | Self::ExecutorFailed(_)
            | Self::Catalog(_)
            | Self::Environments(_)
            | Self::Database { .. }
            | Self::Internal(_) => false,
        }
    }

    /// A [`Self::Database`] from a rendered message alone, with no source to
    /// attach — for a storage failure that arrives as text rather than as a
    /// typed error (`infra::storage`'s `toolkit_odata` mapping, and test
    /// fixtures).
    ///
    /// Prefer `?` on a `toolkit_db::DbError`, which goes through
    /// `From<toolkit_db::DbError>` and keeps the original as `.source()`
    /// (review finding #25).
    #[must_use]
    pub fn database(message: impl Into<String>) -> Self {
        Self::Database {
            message: message.into(),
            source: None,
        }
    }

    /// The text that may be written to a row's `error` column.
    ///
    /// The **error returned to the caller is never affected** — services return
    /// the original value; this is only what gets written down.
    pub(crate) fn recorded_text(&self) -> String {
        if self.disclosable() {
            self.to_string()
        } else {
            OPAQUE_ERROR_TEXT.to_owned()
        }
    }
}

/// Review finding #25: the source is boxed into [`DomainError::Database`]
/// rather than flattened to `e.to_string()`, so `.source()` reaches the
/// original `DbError` and its own cause chain. See that variant's doc; the
/// TODO(DE1302) comment and its lint allowance, which named exactly this fix,
/// are gone with it.
impl From<toolkit_db::DbError> for DomainError {
    fn from(e: toolkit_db::DbError) -> Self {
        DomainError::Database {
            message: e.to_string(),
            source: Some(Box::new(e)),
        }
    }
}

impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        tracing::error!(error = %e, "AuthZ scope resolution failed");
        match e {
            // A denial, and the one `CompileFailed` shape `authz-resolver-sdk`
            // itself documents as a deny, are both about the caller.
            //
            // `CompileFailed` is not one thing — its two constructors
            // disagree about who the failure is about
            // (`authz-resolver-sdk/src/pep/compiler.rs:44-55`):
            //
            // - `ConstraintsRequiredButAbsent`: "a deny: the PEP asked for
            //   row-level constraints but received an empty set. Fail-closed"
            //   (`compiler.rs:45-48`) — the PDP answered allow but supplied no
            //   row scope. A scoped-permission refusal, the same shape as
            //   `Denied`, so it merges into this arm rather than getting a
            //   twin `=> Self::Forbidden` clippy would flag as
            //   `match_same_arms`.
            // - `AllConstraintsFailed` (below) means the PDP named predicates
            //   this PEP could not compile at all — a policy/PEP mismatch,
            //   not a fact about the caller's permissions. That is a fault,
            //   so it is a 500, not folded in here.
            //
            // Both `CompileFailed` shapes still fail closed: neither produces
            // an `AccessScope`, so no row becomes reachable either way. Only
            // the status differs. Review finding #3.
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(
                authz_resolver_sdk::pep::ConstraintCompileError::ConstraintsRequiredButAbsent,
            ) => Self::Forbidden,
            authz_resolver_sdk::EnforcerError::CompileFailed(err) => {
                Self::Internal(format!("authorization scope compilation failed: {err}"))
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(err) => {
                Self::Internal(err.to_string())
            }
        }
    }
}

// -- The boundary mapping ----------------------------------------------------
//
// **`impl From<DomainError> for CanonicalError` lives here, beside the enum it
// maps, and not in `api::rest::error` where it was written.** Every `?` in a
// handler still reaches it, because a trait impl is resolved by coherence
// rather than by an import -- and that is exactly why it had to move. While it
// sat in the transport layer, every `other => other.into()` in a *domain*
// module (`domain::error_attribution`'s two wrappers, most visibly) was a
// domain call into `api::rest::error`, and `api` is a transport over `domain`
// rather than the other way round. Review findings #15, #16.
//
// **Deleting the `use` line did not close that**, which is the mistake this
// move corrects: `crate::no_api_in_domain_tests` is a text scan over imports,
// so it could never have contradicted a claim about a trait impl. The scan
// pins the imports; the location of the impl is what pins this.
//
// `api::rest::error` re-exports what a handler needs, so a REST handler still
// imports its error rendering from exactly one module.
//
// # The two properties this mapping is written to keep
//
// * **The `match` has no catch-all arm**, so adding a [`DomainError`] variant
//   is a compile error here rather than a silent 500. That is the same
//   discipline [`DomainError::disclosable`] already applies to the redaction
//   rule, and for the same reason: a decision made by absence is not a
//   decision.
// * **What a caller may read is decided by [`DomainError::disclosable`], not by
//   this mapping.** `opaque_internal` is the only way an internal-category
//   error is built here, and it takes the domain error rather than a message so
//   it cannot be handed pre-formatted text by mistake.
//
// # The disclosure rule, restated where it is enforced
//
// `Database`, `Catalog`, `Environments`, `ExecutorFailed`, `Internal` and
// `CorruptState` carry text that originates in another system, in a database
// driver, or in this gear's internals. None of it may reach an HTTP body:
// driver text names indexes, columns and key values, and `CorruptState`
// carries a persisted column's contents verbatim. Each of those is logged at
// ERROR with the real cause.
//
// **What the client receives is the canonical internal detail**, *"An internal
// error occurred. Please retry later."*, which `toolkit-canonical-errors`
// supplies and this gear does not choose. Corrected 2026-08-15: this comment
// previously said the caller was "answered with `OPAQUE_ERROR_TEXT`", which is
// not true of an **error** body -- `InternalV1`'s `description` is
// `#[serde(skip)]`, so that string never reaches one. It is handed to the
// builder as the *diagnostic*, and the reason that still matters is
// `opaque_internal`.
//
// Scoped to error bodies deliberately: `OPAQUE_ERROR_TEXT` **does** reach
// clients, in `RunDto.error` on an ordinary 200, because it is what the domain
// layer recorded on the row. `api::rest::dto::RunDto::error` says so, and an
// unqualified "never reaches a body" here would contradict it.
//
// # Two false claims shipped with this mapping, and the rule that would have
// caught them
//
// Both were about the `Validation` arm, both were confident, and the second was
// written to retract the first.
//
// 1. That a schedule's field violation *"cannot be fixed here"* and would need
//    a new [`DomainError`] variant. `ScheduleResourceError` was already
//    declared -- in `domain::error_attribution` since Task 21, but declared all
//    the same -- and [`RunResourceError`]'s own visibility note already
//    described the pattern: a refusal raised where the resource is known.
// 2. That the empty-name refusal specifically could not be reached by a handler
//    `map_err` *"without also re-attributing `ScheduleNameExists`, `Forbidden`,
//    `Database`"*. The counter-example was the fall-through arm of the very
//    function being cited, which passes every non-`Validation` through
//    untouched.
//
// A third, in `handlers::schedules`, claimed the crate had no harness able to
// build a `ConcreteAppServices`; `test_support::Fleet::instance` had been
// returning one for a task and a half.
//
// **The rule, which is operational rather than a resolution to be careful: an
// impossibility gets a compile-or-test attempt before it gets written, and if
// the attempt succeeds the sentence becomes the negative.** Each of the three
// took under two minutes to falsify once anybody tried. Knowing that a
// correction is where the next false claim lands did not prevent the second
// one; running the experiment would have.
//
// `disclosable()` is the predicate that decides which variants land here. The
// two `match`es are separate, so nothing but
// `canonical_mapping_tests::the_mapping_and_the_disclosure_rule_agree_in_both_directions`
// stops them drifting apart - see that test for which direction each failure
// takes and for what it still does not enforce.

/// `pub(crate)` so a handler can raise a refusal this `match` cannot see
/// against the same resource type the mapping uses - one `gts_id` for runs, not
/// two that could disagree. `handlers::runs::subscriber_cap_reached` is the one
/// such refusal: the SSE subscriber cap is a boundary decision with no
/// [`DomainError`] behind it.
///
/// **Corrected 2026-08-17.** This said the visibility was for
/// `domain::local_client`'s *"Phase B refusals"*. Those are gone - Task 20
/// implemented the five schedule methods, so that module raises no error of its
/// own and no longer names this type at all.
#[resource_error(gts_id!("cf.qa.runs.run.v1~"))]
pub(crate) struct RunResourceError;

/// A 500 whose body says nothing, with the real cause logged.
///
/// Takes the [`DomainError`] rather than a string on purpose: a signature that
/// accepted a message would make it possible - and, on a busy day, likely - to
/// pass `e.to_string()`, which is exactly the leak this exists to prevent.
///
/// # Why the diagnostic is the opaque text and not the cause
///
/// The string handed to `CanonicalError::internal` becomes the error's
/// *diagnostic*, which `Problem::from_error` drops and
/// `Problem::from_error_debug` copies into `context["description"]`. The debug
/// renderer is documented "MUST NOT be used in production", but nothing in this
/// crate selects a renderer, so a later change could. Handing it
/// [`OPAQUE_ERROR_TEXT`] means even that renderer cannot leak - which
/// `canonical_mapping_tests::the_mapping_and_the_disclosure_rule_agree_in_both_directions`
/// checks
/// under both renderings rather than trusting the warning to hold.
///
/// # It reports a classification disagreement rather than assuming there is none
///
/// A variant routed here that `disclosable()` calls safe is a contradiction
/// between two rules that are supposed to agree. This logs it at ERROR rather
/// than letting the milder of the two win silently - and the log line is
/// asserted, by `canonical_mapping_tests::a_classification_disagreement_is_reported`,
/// because a
/// guarantee carried only by an unobserved log line is not a guarantee.
fn opaque_internal(e: &DomainError) -> CanonicalError {
    if e.disclosable() {
        tracing::error!(
            error = %e,
            "a disclosable error was mapped to an opaque 500; the mapping and \
             DomainError::disclosable disagree about this variant"
        );
    }
    tracing::error!(error = %e, "qa-runs internal error");
    CanonicalError::internal(OPAQUE_ERROR_TEXT).create()
}

impl From<DomainError> for CanonicalError {
    #[allow(
        clippy::cognitive_complexity,
        reason = "one arm per DomainError variant, each a single builder chain. The \
                  metric counts every arm as a branch, so the score tracks the size of \
                  the enum rather than the difficulty of the code. Splitting it would \
                  break the exhaustiveness that makes a new variant a compile error, \
                  which is this function's main safety property. Same diagnosis as \
                  `qa-environments/src/api/rest/error.rs`, which carries the bare allow."
    )]
    fn from(e: DomainError) -> Self {
        match &e {
            // -- 404 --------------------------------------------------------
            // Absent and foreign are indistinguishable by construction; see
            // `RunsRepository::resolve_owned`. Nothing here may add a
            // distinguishing detail.
            DomainError::RunNotFound { id } => {
                RunResourceError::not_found(format!("Run {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }
            DomainError::QueueRowNotFound { id } => {
                QueueResourceError::not_found(format!("Queue row {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }
            // Absent and foreign are indistinguishable here too; see
            // `SchedulesRepository::resolve_owned_schedule`.
            DomainError::ScheduleNotFound { id } => {
                ScheduleResourceError::not_found(format!("Schedule {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }

            // -- 409, already exists ----------------------------------------
            DomainError::RunNameExists { name } => {
                RunResourceError::already_exists(format!("Run '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }
            DomainError::ScheduleNameExists { name } => {
                ScheduleResourceError::already_exists(format!("Schedule '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }
            DomainError::QueueRowExists { run_id } => {
                QueueResourceError::already_exists(format!("Run {run_id} already has a queue row"))
                    .with_resource(run_id.to_string())
                    .create()
            }

            // -- 409, aborted -----------------------------------------------
            // Both carry a state that was observed *after* the guarded write
            // failed, so it is advisory - the row may have moved again. It is
            // rendered because a message has to name something; nothing may
            // branch on it, and in particular a client must not retry because
            // the state "looked retryable" (see `domain::repos`).
            DomainError::IllegalTransition { id, state, action } => {
                RunResourceError::aborted(format!(
                    "Run {id} is in state {} and cannot {action}",
                    state.as_str()
                ))
                .with_reason("ILLEGAL_TRANSITION")
                .with_resource(id.to_string())
                .create()
            }
            DomainError::QueueRowNotQueued { id, state } => QueueResourceError::aborted(format!(
                "Queue row {id} is {}, not queued",
                state.as_str()
            ))
            .with_reason("NOT_QUEUED")
            .with_resource(id.to_string())
            .create(),

            // -- 400, failed precondition -----------------------------------
            // `failed_precondition` renders **400**, not 409, and that is the
            // answer the source system gives too (`StatusCode::BAD_REQUEST` at
            // `manager/src/routes/custom_plans.rs:681`). Nothing about the
            // server's state conflicts: the caller fixes it by naming a branch.
            // The route registration must therefore declare `error_400` for
            // this, not `error_409`.
            DomainError::AmbiguousBranch { plan_id, groups } => {
                RunResourceError::failed_precondition()
                    .with_precondition_violation(
                        "branch",
                        format!(
                            "Custom plan {plan_id} spans {groups} repositories, so a branch \
                             must be given explicitly: no single repository default applies"
                        ),
                        "AMBIGUOUS_BRANCH",
                    )
                    .with_resource(plan_id.to_string())
                    .create()
            }

            // -- 429 --------------------------------------------------------
            // Two variants rather than one because the frozen guide gives each
            // of the launch path's 429 causes its own knob, and naming which
            // setting an operator has to change is the only actionable content
            // either message has.
            //
            // **Both are run-typed, and that is what raises them.**
            // `QueueFull` is constructed in exactly one place —
            // `service::admission`'s `admit` — which is on the launch path and
            // nowhere else, so the caller who meets it addressed `POST
            // /qa/v1/runs` or its re-run. It used to answer
            // `cf.qa.runs.queue_entry.v1~` with a **platform** uuid as the
            // resource name, so a caller who had asked about a run got a body
            // naming three different things and could act on none of them.
            // `ConcurrencyLimit` is also raisable from
            // `RunsService::force_start`, whose caller addressed a queue row;
            // that endpoint re-attributes through `as_queue_error`, because a
            // variant that two surfaces can raise cannot carry one resource
            // type of its own.
            //
            // Neither names a resource *instance*: the variants carry an
            // environment id and a cluster-wide limit respectively, and neither
            // is a run this gear could name. The environment stays in the
            // violation description, where it is a fact about the refusal
            // rather than a claim about which row the caller should go and
            // look at.
            DomainError::QueueFull {
                platform_id,
                queued,
                limit,
            } => RunResourceError::resource_exhausted(format!(
                "Run queue for environment {platform_id} is full: {queued} of {limit} slots used"
            ))
            // The subject is the *setting*, which is what an operator changes.
            .with_quota_violation(
                "queue_max_depth",
                format!("{queued} of {limit} queued rows for environment {platform_id}"),
            )
            .create(),
            DomainError::ConcurrencyLimit { limit } => RunResourceError::resource_exhausted(
                format!("Max concurrent runs limit reached ({limit})"),
            )
            .with_quota_violation(
                "max_concurrent_runs",
                format!("the cluster-wide limit of {limit} concurrent runs is reached"),
            )
            .create(),

            // -- 400, invalid argument --------------------------------------
            //
            // **The run is this arm's answer because the variant names no
            // resource, so every caller whose resource is not a run must
            // attribute its own** - `as_schedule_error` and `as_queue_error`
            // above.
            //
            // **What this arm is, and is not.** It is the default for a caller
            // that did not re-attribute; it is *not* evidence that no schedule
            // or queue field can reach it. Three surfaces owe attribution and
            // each has to discharge it separately: `handlers::schedules`,
            // `handlers::queue`, and `domain::local_client`, which is the one
            // that shipped without it and handed an in-process caller a
            // schedule's empty-name refusal as a run's. What holds the property
            // is a driven test per surface -
            // `handlers::{runs,queue,schedules}_handler_tests` and
            // `local_client::client::tests` - reading the `gts_id` out of the
            // rendered body, because nothing in the type system pairs a caller
            // with a resource type.
            //
            // This comment has been the defect three times, each time as an
            // absence claim about what "nothing" reaches; the module header
            // carries the history and the rule that would have caught all
            // three.
            DomainError::Validation { field, message } => RunResourceError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION")
                .create(),
            // A schedule's field, so `ScheduleResourceError` rather than
            // `RunResourceError` - a caller told a *run* rejected their cron
            // would look at the wrong resource. Both halves of the error are
            // rendered as the violation description: `expression` is what the
            // caller sent and `message` is this gear's own sentence about it
            // (`DomainError::InvalidCron`), and splitting them would drop one.
            DomainError::InvalidCron { .. } => ScheduleResourceError::invalid_argument()
                .with_field_violation("cron", e.to_string(), "INVALID_CRON")
                .create(),
            // The parameter's own name is inside the wrapped `ParamError`, and
            // `cpt-cf-qa-fr-runs-params` requires the failure to name it - so
            // the whole rendered error is the violation description rather than
            // being re-split into a field and a message this layer would have
            // to guess at.
            DomainError::InvalidParameters(inner) => RunResourceError::invalid_argument()
                .with_field_violation("parameters", inner.to_string(), "INVALID_PARAMETERS")
                .create(),

            // -- 403 --------------------------------------------------------
            // Never carries what was denied: that is the cross-tenant oracle
            // every read on this gear is written to close.
            DomainError::Forbidden => RunResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),

            // -- 500, opaque ------------------------------------------------
            // Exactly the variants `DomainError::disclosable` classifies as
            // unsafe to echo, in the order that method lists them, so the two
            // can be compared by reading. One or-pattern rather than an arm per
            // variant because `clippy::match_same_arms` is denied in this
            // workspace and every body here is the same call.
            //
            // `Catalog` and `Environments` are the two cross-gear reads, and
            // neither is `unavailable`/503: a failure here is as often "the plan
            // does not resolve" as "the gear is down", this layer cannot tell
            // which, and answering 503 to the first would invite a client to
            // retry something that will never succeed.
            DomainError::Database { .. }
            | DomainError::Internal(_)
            | DomainError::ExecutorFailed(_)
            | DomainError::CorruptState { .. }
            | DomainError::Catalog(_)
            | DomainError::Environments(_) => opaque_internal(&e),
        }
    }
}

#[cfg(test)]
mod canonical_mapping_tests {
    //! Every [`DomainError`] variant's category and HTTP status, plus the
    //! negative half: that no 500 body carries the text it was built from.
    //!
    //! Exhaustiveness is the compiler's job - the `match` above has no `_` arm.
    //! What these pin is the *classification*, which the compiler cannot check,
    //! and the disclosure, which is a property of the rendered body rather than
    //! of the type.

    use qa_runs_sdk::{QueueState, RunState};
    use uuid::Uuid;

    use super::{CanonicalError, DomainError};
    use crate::domain::error::OPAQUE_ERROR_TEXT;
    use toolkit_canonical_errors::Problem;

    /// What the client actually receives, as JSON.
    ///
    /// **`Problem::from_error` and not `format!("{ce:?}")`.** The `Debug`
    /// rendering was what these tests asserted against until 2026-08-15, and it
    /// is the wrong artifact: it is a superset of the wire, so the negative
    /// assertions held, but a leak introduced in the real rendering path would
    /// not have shown up in it at all. `Problem` is the type that becomes the
    /// response body.
    fn wire(e: DomainError) -> (u16, String) {
        let ce: CanonicalError = e.into();
        let problem = Problem::from_error(&ce).expect("a problem must serialize");
        let status = problem.status.expect("a problem always carries a status");
        (
            status,
            serde_json::to_string(&problem).expect("a problem must serialize"),
        )
    }

    /// The wire rendering under **both** renderers, for the drift guard.
    ///
    /// The second is `Problem::from_error_debug`, which
    /// `toolkit-canonical-errors` documents as "MUST NOT be used in production"
    /// because it copies the internal diagnostic into `context["description"]`.
    /// Asserted anyway, and deliberately: nothing in this crate selects a
    /// renderer, so a later change could. Handing `opaque_internal` the opaque
    /// text as the *diagnostic* - rather than the real cause - is what makes
    /// even that renderer safe, and that is worth pinning rather than relying on
    /// nobody ever switching.
    fn both_renderings(e: DomainError) -> (u16, String, String) {
        let ce: CanonicalError = e.into();
        let problem = Problem::from_error(&ce).expect("a problem must serialize");
        let status = problem.status.expect("a problem always carries a status");
        let body = serde_json::to_string(&problem).expect("a problem must serialize");
        let debug_body = serde_json::to_string(
            &Problem::from_error_debug(&ce).expect("a problem must serialize"),
        )
        .expect("a problem must serialize");
        (status, body, debug_body)
    }

    /// Kept for the per-variant tests below, which assert one status and one
    /// message each; it renders the same wire body [`wire`] does.
    fn rendered(e: DomainError) -> (u16, String) {
        wire(e)
    }

    /// The sentinel every sample carries in every free-text field it has.
    ///
    /// A real Postgres unique-violation message, which is what
    /// `From<toolkit_db::DbError>` puts into `Database` verbatim: it names an
    /// index, a column list, and a key value that is another tenant's id.
    const SENTINEL: &str = "duplicate key value violates unique constraint \
                            \"idx_qa_run_queue_tenant_run\" DETAIL: Key \
                            (tenant_id, run_id)=(0a11ce11-dead-beef) already exists";

    /// A stable name per variant, from an **exhaustive** `match`.
    ///
    /// This is the compile-time half of the drift guard: adding a
    /// [`DomainError`] variant fails to build *here*, in the test module, which
    /// is what puts an author in front of [`samples`] at the moment they add
    /// one.
    ///
    /// **What it does not enforce**, stated because the previous version of this
    /// module claimed a completeness it did not have: nothing makes the author
    /// then *add* the sample. The compile error is a prompt, not a proof. A
    /// variant added to the enum and to this `match` but not to [`samples`] is
    /// simply untested, silently.
    fn variant_tag(e: &DomainError) -> &'static str {
        match e {
            DomainError::RunNotFound { .. } => "RunNotFound",
            DomainError::QueueRowNotFound { .. } => "QueueRowNotFound",
            DomainError::ScheduleNotFound { .. } => "ScheduleNotFound",
            DomainError::Validation { .. } => "Validation",
            DomainError::InvalidCron { .. } => "InvalidCron",
            DomainError::InvalidParameters(_) => "InvalidParameters",
            DomainError::CorruptState { .. } => "CorruptState",
            DomainError::RunNameExists { .. } => "RunNameExists",
            DomainError::ScheduleNameExists { .. } => "ScheduleNameExists",
            DomainError::QueueRowExists { .. } => "QueueRowExists",
            DomainError::IllegalTransition { .. } => "IllegalTransition",
            DomainError::QueueRowNotQueued { .. } => "QueueRowNotQueued",
            DomainError::ExecutorFailed(_) => "ExecutorFailed",
            DomainError::QueueFull { .. } => "QueueFull",
            DomainError::ConcurrencyLimit { .. } => "ConcurrencyLimit",
            DomainError::AmbiguousBranch { .. } => "AmbiguousBranch",
            DomainError::Catalog(_) => "Catalog",
            DomainError::Environments(_) => "Environments",
            DomainError::Forbidden => "Forbidden",
            DomainError::Database { .. } => "Database",
            DomainError::Internal(_) => "Internal",
        }
    }

    /// One sample per variant, each carrying [`SENTINEL`] wherever it has a
    /// free-text field, plus a marker a *disclosable* variant must render.
    ///
    /// `None` for `Forbidden`, which carries nothing to disclose - the
    /// assertion for it is that it is neither a 500 nor opaque.
    fn samples() -> Vec<(DomainError, Option<String>)> {
        let id = Uuid::from_u128(0x5EED);
        vec![
            (DomainError::RunNotFound { id }, Some(id.to_string())),
            (DomainError::QueueRowNotFound { id }, Some(id.to_string())),
            (DomainError::ScheduleNotFound { id }, Some(id.to_string())),
            (
                DomainError::Validation {
                    field: "timeout_seconds".to_owned(),
                    message: "is 31536000; the maximum is 86400".to_owned(),
                },
                Some("timeout_seconds".to_owned()),
            ),
            (
                crate::domain::cron::parse_cron("99 * * * *")
                    .expect_err("minute 99 must not validate"),
                Some("99 * * * *".to_owned()),
            ),
            (
                DomainError::InvalidParameters(
                    crate::domain::params::validate(
                        &[qa_runs_sdk::RunParameter {
                            name: String::new(),
                            value: "v".to_owned(),
                        }],
                        &qa_product_sdk::access::RunVarContract::default(),
                    )
                    .expect_err("an empty parameter name must not validate"),
                ),
                Some("parameters".to_owned()),
            ),
            (
                DomainError::CorruptState {
                    what: "run.state",
                    id,
                    value: SENTINEL.to_owned(),
                },
                None,
            ),
            (
                DomainError::RunNameExists {
                    name: "smoke-7".to_owned(),
                },
                Some("smoke-7".to_owned()),
            ),
            (
                DomainError::ScheduleNameExists {
                    name: "nightly".to_owned(),
                },
                Some("nightly".to_owned()),
            ),
            (
                DomainError::QueueRowExists { run_id: id },
                Some(id.to_string()),
            ),
            (
                DomainError::IllegalTransition {
                    id,
                    state: RunState::Succeeded,
                    action: "be cancelled".to_owned(),
                },
                Some(id.to_string()),
            ),
            (
                DomainError::QueueRowNotQueued {
                    id,
                    state: QueueState::Running,
                },
                Some(id.to_string()),
            ),
            (DomainError::ExecutorFailed(SENTINEL.to_owned()), None),
            (
                DomainError::QueueFull {
                    platform_id: id,
                    queued: 20,
                    limit: 20,
                },
                Some("queue_max_depth".to_owned()),
            ),
            (
                DomainError::ConcurrencyLimit { limit: 50 },
                Some("max_concurrent_runs".to_owned()),
            ),
            (
                DomainError::AmbiguousBranch {
                    plan_id: id,
                    groups: 3,
                },
                Some(id.to_string()),
            ),
            (DomainError::Catalog(SENTINEL.to_owned()), None),
            (DomainError::Environments(SENTINEL.to_owned()), None),
            (DomainError::Forbidden, None),
            (DomainError::database(SENTINEL), None),
            (DomainError::Internal(SENTINEL.to_owned()), None),
        ]
    }

    /// **The drift guard, in both directions.**
    ///
    /// `DomainError::disclosable` is the redaction rule for everything this gear
    /// writes down; this module renders the same errors for HTTP. The two are
    /// separate `match`es, so nothing but this test stops them disagreeing - and
    /// the disagreement is silent either way round. Both directions are checked:
    ///
    /// * **Classified unsafe, rendered in full** - the leak. A variant moved
    ///   into `disclosable()`'s `=> false` arm while keeping a disclosing arm
    ///   here would ship its text to clients.
    /// * **Classified safe, rendered opaque** - the inverse, and not harmless:
    ///   `queue_max_depth` and `max_concurrent_runs` are the only actionable
    ///   content their 429s have, and redacting them turns a fixable refusal
    ///   into a shrug.
    ///
    /// Falsified by moving `Validation` into `disclosable()`'s `=> false` arm -
    /// the mutation that passed every test this module had at the time.
    #[test]
    fn the_mapping_and_the_disclosure_rule_agree_in_both_directions() {
        for (error, marker) in samples() {
            let tag = variant_tag(&error);
            let disclosable = error.disclosable();
            let (status, body, debug_body) = both_renderings(error);

            if disclosable {
                assert_ne!(
                    status, 500,
                    "{tag} is classified disclosable but renders as an internal error"
                );
                if let Some(marker) = marker {
                    assert!(
                        body.contains(&marker),
                        "{tag} is classified disclosable but {marker:?} did not survive \
                         to the wire: {body}"
                    );
                }
            } else {
                assert_eq!(status, 500, "{tag} is classified undisclosable");
                for (label, rendered) in [("wire", &body), ("debug wire", &debug_body)] {
                    // The **whole** sentinel, not a hand-picked subset. The
                    // subset that shipped had dropped `"tenant_id"` - the
                    // cross-tenant marker specifically - which is exactly the
                    // substring a reviewer would most want asserted.
                    assert!(
                        !rendered.contains(SENTINEL),
                        "{tag} leaked the sentinel into the {label} body: {rendered}"
                    );
                    for leak in [
                        "duplicate key",
                        "idx_qa_run_queue_tenant_run",
                        "0a11ce11",
                        "tenant_id",
                        "run.state",
                    ] {
                        assert!(
                            !rendered.contains(leak),
                            "{tag} leaked {leak:?} into the {label} body: {rendered}"
                        );
                    }
                }
            }
        }
    }

    /// **The contradiction report, asserted rather than assumed.**
    ///
    /// `opaque_internal` logs when it is handed an error `disclosable()` calls
    /// safe. That was the only thing standing behind the word "asserts" in its
    /// doc, and nothing observed it: inverting the condition to
    /// `if !e.disclosable()` left every test this module had at the time green.
    ///
    /// A disclosable error is passed here directly - the production `match`
    /// never routes one this way, which is the point: this pins the *detector*,
    /// so that if a future mapping does route one here, the operator finds out.
    #[test]
    #[tracing_test::traced_test]
    fn a_classification_disagreement_is_reported() {
        let safe = DomainError::Forbidden;
        assert!(
            safe.disclosable(),
            "premise: this test needs an error the rule calls safe"
        );
        let _rendered = super::opaque_internal(&safe);

        assert!(
            logs_contain("the mapping and DomainError::disclosable disagree"),
            "a disagreement between the two rules must be reported, not resolved \
             silently in favour of the milder one"
        );
    }

    /// The sample list is hand-written, so at least assert it has no duplicates
    /// and that every entry is the variant its tag names.
    #[test]
    fn every_variant_is_sampled_exactly_once() {
        let mut tags: Vec<&str> = samples()
            .iter()
            .map(|(error, _)| variant_tag(error))
            .collect();
        let before = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(before, tags.len(), "a variant is sampled twice: {tags:?}");

        // **The count, which is what makes the list complete rather than
        // merely tidy.** `variant_tag` is exhaustive, so adding a `DomainError`
        // variant is already a compile error there - but nothing made the
        // author then add a *sample*, and deleting the `Database` entry (the
        // single most important leak vector, the one carrying driver text
        // verbatim) left the suite green.
        //
        // A count in a test, deliberately, where this project bans them in doc
        // comments: a doc comment's count rots silently, whereas this one fails
        // the moment the enum and the list disagree, which is the whole job.
        assert_eq!(
            tags.len(),
            DOMAIN_ERROR_VARIANTS,
            "every DomainError variant must be sampled: {tags:?}"
        );
    }

    /// How many variants `DomainError` has.
    ///
    /// Paired with the exhaustive `variant_tag` match: a new variant fails to
    /// compile there, and fails this count here, so the two together make the
    /// sample list complete in both directions.
    const DOMAIN_ERROR_VARIANTS: usize = 21;

    #[test]
    fn run_not_found_is_404() {
        let id = Uuid::from_u128(0x11);
        let ce: CanonicalError = DomainError::RunNotFound { id }.into();
        assert_eq!(ce.status_code(), 404);
        assert!(matches!(ce, CanonicalError::NotFound { .. }), "{ce:?}");
    }

    #[test]
    fn queue_row_not_found_is_404() {
        let ce: CanonicalError = DomainError::QueueRowNotFound {
            id: Uuid::from_u128(0x12),
        }
        .into();
        assert_eq!(ce.status_code(), 404);
        assert!(matches!(ce, CanonicalError::NotFound { .. }), "{ce:?}");
    }

    #[test]
    fn run_name_exists_is_409_already_exists() {
        let ce: CanonicalError = DomainError::RunNameExists {
            name: "smoke-7".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::AlreadyExists { .. }), "{ce:?}");
    }

    #[test]
    fn queue_row_exists_is_409_already_exists() {
        let ce: CanonicalError = DomainError::QueueRowExists {
            run_id: Uuid::from_u128(0x13),
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::AlreadyExists { .. }), "{ce:?}");
    }

    #[test]
    fn an_illegal_transition_is_409_aborted() {
        let ce: CanonicalError = DomainError::IllegalTransition {
            id: Uuid::from_u128(0x14),
            state: RunState::Succeeded,
            action: "be cancelled".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::Aborted { .. }), "{ce:?}");
    }

    #[test]
    fn a_row_that_left_queued_is_409_aborted() {
        let ce: CanonicalError = DomainError::QueueRowNotQueued {
            id: Uuid::from_u128(0x15),
            state: QueueState::Running,
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::Aborted { .. }), "{ce:?}");
    }

    /// **400, not 409.** The route table registers `error_400` for this, and
    /// the source system answers `BAD_REQUEST` too. Getting it wrong is
    /// invisible without this assertion, because both are still `Err`.
    #[test]
    fn an_ambiguous_branch_is_400_failed_precondition() {
        let ce: CanonicalError = DomainError::AmbiguousBranch {
            plan_id: Uuid::from_u128(0x16),
            groups: 3,
        }
        .into();
        assert_eq!(ce.status_code(), 400);
        assert!(
            matches!(ce, CanonicalError::FailedPrecondition { .. }),
            "{ce:?}"
        );
    }

    /// Both 429 causes, and both messages naming their knob - which is the only
    /// content an operator can act on.
    ///
    /// **Both are attributed to the run, because the launch path is what raises
    /// them.** `QueueFull` used to be queue-typed and to carry a *platform*
    /// uuid as its resource name, so a caller who had asked about a run got a
    /// body naming three different resources; it is constructible in exactly
    /// one place, `service::admission`'s `admit`, which is on the launch path.
    /// `ConcurrencyLimit` is also raisable from `force_start`, and that
    /// endpoint re-attributes through `as_queue_error` -
    /// `handlers::queue`'s `handler_tests` pins that half.
    #[test]
    fn both_capacity_refusals_are_429_and_name_their_setting() {
        let platform = Uuid::from_u128(0x17);
        let (status, body) = rendered(DomainError::QueueFull {
            platform_id: platform,
            queued: 20,
            limit: 20,
        });
        assert_eq!(status, 429);
        assert!(body.contains("queue_max_depth"), "{body}");
        assert!(
            body.contains("cf.qa.runs.run.v1~"),
            "the launch path's refusal must name the run the caller launched: {body}"
        );
        assert!(
            !body.contains("cf.qa.runs.queue_entry.v1~"),
            "and not a queue row the caller never addressed: {body}"
        );
        assert!(
            !body.contains(&format!("\"resource_name\":\"{platform}\"")),
            "an environment id is not a resource this gear owns, so it must not be \
             the body's resource name: {body}"
        );

        let (status, body) = rendered(DomainError::ConcurrencyLimit { limit: 50 });
        assert_eq!(status, 429);
        assert!(body.contains("max_concurrent_runs"), "{body}");
        assert!(body.contains("50"), "{body}");
        assert!(body.contains("cf.qa.runs.run.v1~"), "{body}");
    }

    #[test]
    fn a_validation_failure_is_400_invalid_argument_naming_its_field() {
        let (status, body) = rendered(DomainError::Validation {
            field: "timeout_seconds".to_owned(),
            message: "is 31536000; it must be between 1 and 86400 seconds".to_owned(),
        });
        assert_eq!(status, 400);
        assert!(body.contains("timeout_seconds"), "{body}");
        assert!(body.contains("86400"), "the ceiling must survive: {body}");
    }

    /// **The offending parameter's own name reaches the body**, which is what
    /// `cpt-cf-qa-fr-runs-params` requires and what this test's name claims.
    ///
    /// It did not test that. It built the sample from `ParamError::EmptyName` -
    /// the one variant that by design carries no name - and asserted only the
    /// literal `"parameters"`, which is the *field-violation key* this arm
    /// hardcodes. Measured: deleting `inner.to_string()` from the violation
    /// description, so the body named the parameter nothing at all, left it
    /// green.
    ///
    /// The sample is now a violation that has a name to lose. `EmptyName` is
    /// still covered - it is the `InvalidParameters` entry in [`samples`] - and
    /// the two are complementary: that one pins the key, this one pins the
    /// name.
    #[test]
    fn invalid_launch_parameters_are_400_and_name_the_parameter() {
        let inner = crate::domain::params::validate(
            &[qa_runs_sdk::RunParameter {
                name: "9lives".to_owned(),
                value: "v".to_owned(),
            }],
            &qa_product_sdk::access::RunVarContract::default(),
        )
        .expect_err("a name that does not start with a letter or '_' must not validate");
        assert!(
            matches!(inner, crate::domain::params::ParamError::InvalidName { .. }),
            "premise: the sample must be a variant that carries a name: {inner:?}"
        );

        let (status, body) = rendered(DomainError::InvalidParameters(inner));
        assert_eq!(status, 400);
        assert!(
            body.contains("parameters"),
            "the field-violation key: {body}"
        );
        assert!(
            body.contains("9lives"),
            "and the parameter the caller has to fix: {body}"
        );
    }

    #[test]
    fn forbidden_is_403_and_says_nothing_about_what_was_denied() {
        let (status, body) = rendered(DomainError::Forbidden);
        assert_eq!(status, 403);
        for oracle in ["tenant", "scope", "platform", "run "] {
            assert!(
                !body.to_lowercase().contains(oracle),
                "a denial must not describe what was denied ({oracle}): {body}"
            );
        }
    }

    /// The other direction, so the redaction cannot be "fixed" by redacting
    /// everything: a caller's own refusal keeps its detail, because that detail
    /// is the only thing they can act on.
    #[test]
    fn a_callers_own_refusal_is_not_redacted() {
        let (_, body) = rendered(DomainError::RunNameExists {
            name: "smoke-7".to_owned(),
        });
        assert!(body.contains("smoke-7"), "{body}");
        assert!(!body.contains(OPAQUE_ERROR_TEXT), "{body}");
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! [`DomainError::disclosable`] is the redaction rule for every `error`
    //! column this gear writes, so it is pinned here, at its definition, rather
    //! than only through the one service that currently calls it.
    //!
    //! Exhaustiveness is the compiler's job — the `match` has no `_` arm, so a new
    //! variant fails to build. These tests pin the *classification*, which the
    //! compiler cannot check.
    use super::*;

    /// The leak that produced this rule: a `Database` cause is
    /// `From<toolkit_db::DbError>` and carries raw driver text — index names,
    /// column names, key values — and it used to be written to `qa_runs.error`
    /// verbatim.
    #[test]
    fn another_systems_text_is_never_recorded() {
        let raw = "duplicate key value violates unique constraint \
                   \"idx_qa_run_queue_tenant_run\" DETAIL: Key (tenant_id, run_id)=(0a11) exists";
        for error in [
            DomainError::database(raw),
            DomainError::Internal(raw.to_owned()),
            DomainError::ExecutorFailed(raw.to_owned()),
            DomainError::Catalog(raw.to_owned()),
            DomainError::Environments(raw.to_owned()),
            DomainError::CorruptState {
                what: "run.state",
                id: Uuid::nil(),
                value: raw.to_owned(),
            },
        ] {
            assert!(!error.disclosable(), "{error:?} must not be disclosable");
            let recorded = error.recorded_text();
            assert_eq!(recorded, OPAQUE_ERROR_TEXT, "for {error:?}");
            for leak in ["idx_qa_run_queue_tenant_run", "duplicate key", "0a11"] {
                assert!(
                    !recorded.contains(leak),
                    "{error:?} recorded {leak:?}: {recorded}"
                );
            }
            // The error itself is untouched — only what gets written down is
            // redacted, and services still return the real cause upward.
            assert!(error.to_string().contains(raw), "{error:?}");
        }
    }

    /// The other direction, which matters just as much: the frozen guide gives
    /// each 429 cause its own knob precisely so an operator can act on it (guide
    /// lines 74, 240-242), and redacting those would delete the only actionable
    /// content the message has.
    #[test]
    fn a_callers_own_refusal_survives_verbatim() {
        let full = DomainError::QueueFull {
            platform_id: Uuid::from_u128(0x0C01),
            queued: 20,
            limit: 20,
        };
        assert!(full.disclosable());
        assert_eq!(full.recorded_text(), full.to_string());
        assert!(full.recorded_text().contains("20 of 20 slots"));

        let cap = DomainError::ConcurrencyLimit { limit: 50 };
        assert!(cap.disclosable());
        assert!(cap.recorded_text().contains("(50)"));

        // A state the caller's own run is in, and a field they sent: both theirs.
        assert!(
            DomainError::IllegalTransition {
                id: Uuid::nil(),
                state: RunState::Created,
                action: "become queued".to_owned(),
            }
            .disclosable()
        );
        assert!(
            DomainError::Validation {
                field: "branch".to_owned(),
                message: "no files".to_owned(),
            }
            .disclosable()
        );
    }

    /// `From<EnforcerError> for DomainError`, pinned one arm per test so a
    /// future change to any single arm fails exactly one test rather than
    /// being lost in a combined assertion.
    mod enforcer_error_mapping {
        use authz_resolver_sdk::pep::ConstraintCompileError;
        use authz_resolver_sdk::{AuthZResolverError, EnforcerError};

        use super::*;

        /// The routine, expected outcome: a PDP deny is about the caller, and
        /// stays a 403.
        #[test]
        fn a_denial_is_still_forbidden() {
            let e = EnforcerError::Denied { deny_reason: None };
            assert!(matches!(DomainError::from(e), DomainError::Forbidden));
        }

        /// **Deliberately still a 403, not the 500 finding #3 is about.**
        ///
        /// `ConstraintsRequiredButAbsent` means the PDP answered *allow* but
        /// supplied no row-level constraints although the PEP asked for them.
        /// `authz-resolver-sdk`'s own compiler doc calls this a deny: "the PEP
        /// asked for row-level constraints but received an empty set.
        /// Fail-closed" (`authz-resolver-sdk/src/pep/compiler.rs:45-48`). That
        /// is a scoped-permission refusal — the same shape as `Denied` — not
        /// a broken policy engine, so a reader who knows only finding #3 ("a
        /// compile fault is a 500") would expect this test to assert
        /// `Internal` and would be wrong: this specific compile failure is
        /// about the caller, not the PDP. It is also exactly the shape
        /// `a_nil_tenant_context_is_denied_and_writes_nothing`
        /// (`domain::service::launch_tests`) exercises end to end: a nil-tenant
        /// context compiles zero constraints and must still land as
        /// `Forbidden`.
        #[test]
        fn a_scope_required_but_absent_is_still_forbidden() {
            let e =
                EnforcerError::CompileFailed(ConstraintCompileError::ConstraintsRequiredButAbsent);
            assert!(
                matches!(DomainError::from(e), DomainError::Forbidden),
                "ConstraintsRequiredButAbsent is a documented deny, not a fault"
            );
        }

        /// The half finding #3 is actually about: the PDP named predicates
        /// this PEP cannot compile at all — a configuration fault, not a fact
        /// about the caller's permissions (`compiler.rs:52`).
        #[test]
        fn all_constraints_failing_to_compile_is_internal_not_forbidden() {
            let e = EnforcerError::CompileFailed(ConstraintCompileError::AllConstraintsFailed {
                reason: "unknown predicate `frobnicate`".to_owned(),
            });
            assert!(
                matches!(DomainError::from(e), DomainError::Internal(_)),
                "AllConstraintsFailed must map to Internal (500), not Forbidden (403)"
            );
        }

        /// Unchanged: the PDP RPC itself failing is a fault, not a decision.
        #[test]
        fn an_evaluation_failure_is_internal() {
            let e = EnforcerError::EvaluationFailed(
                CanonicalError::service_unavailable()
                    .with_detail("plugin not registered")
                    .create(),
            );
            assert!(matches!(DomainError::from(e), DomainError::Internal(_)));
        }
    }

    /// Review finding #25, and the `TODO(DE1302)` that used to sit above
    /// `From<toolkit_db::DbError>`: the conversion keeps the original error as
    /// `.source()` instead of flattening it to its `Display` text. Without
    /// this, `sea_orm`'s SQLSTATE and `sqlx`'s constraint name are gone by the
    /// time the error leaves `infra::storage`.
    #[test]
    fn a_db_error_converted_to_a_domain_error_keeps_its_source() {
        let db_error = toolkit_db::DbError::UnknownDsn("mysql://nowhere".to_owned());
        let rendered = db_error.to_string();

        let domain = DomainError::from(db_error);

        // The `Display` string is unchanged by the boxing, which is what lets
        // every existing response body and `error` column stay as it was.
        assert_eq!(domain.to_string(), format!("database error: {rendered}"));

        let source = std::error::Error::source(&domain).expect("the DbError is now the source");
        assert!(
            source.is::<toolkit_db::DbError>(),
            "the source must be the original error, not a re-wrapping of its text"
        );
        assert_eq!(source.to_string(), rendered);
    }

    /// The other constructor deliberately has no source: a failure that only
    /// ever existed as text cannot invent one, and `.source()` says so rather
    /// than pointing at a stand-in.
    #[test]
    fn a_database_error_built_from_text_alone_has_no_source() {
        let domain = DomainError::database("relation \"qa\" does not exist");

        assert_eq!(
            domain.to_string(),
            "database error: relation \"qa\" does not exist"
        );
        assert!(std::error::Error::source(&domain).is_none());
    }
}
