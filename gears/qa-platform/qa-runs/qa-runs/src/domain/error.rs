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
use toolkit_macros::domain_model;
use uuid::Uuid;

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
            let e = EnforcerError::EvaluationFailed(AuthZResolverError::ServiceUnavailable(
                "plugin not registered".to_owned(),
            ));
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
