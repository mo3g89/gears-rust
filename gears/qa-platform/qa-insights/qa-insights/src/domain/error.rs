//! The gear's domain error type.
//!
//! Started at Task 9 with the variants the plan names plus the four-variant
//! tail every sibling carries, so later tasks extend rather than invent it.
//! Shape copied from `qa-environments/src/domain/error.rs`.
//!
//! # Who constructs what, as of Task 19
//!
//! By module rather than by line, so it does not rot on the next edit. The
//! header said "**Nothing**" through Task 11, which was true then; Task 12
//! brought the repository layer and with it the first three constructors.
//! Restated rather than left to stand, which is the fix qa-runs' equivalent
//! header records after carrying its own "nothing constructs these yet"
//! sentence four tasks past the point where it stopped being true
//! (`qa-runs/src/domain/error.rs`, the "Who constructs what" block).
//!
//! * `infra::storage::mapper` — [`DomainError::CorruptState`] for every stored
//!   value it refuses to guess at, and [`DomainError::Validation`] for the one
//!   *caller* input it refuses (`query_json` that is not JSON).
//! * `infra::storage::saved_views_sea_repo` —
//!   [`DomainError::SavedViewNameExists`], from the unique index on
//!   `(tenant_id, owner_id, scope, plan_key, name)`, in both `create` and
//!   `update`.
//! * `infra::storage::*` — [`DomainError::Database`], through
//!   `db::db_err` (`mapper::db_err` until Task 17 moved it). `notify_sea_repo::claim_notification` deliberately
//!   constructs **nothing** for its unique violation: a lost claim race is
//!   `Ok(false)`, which is obligation #4 of the schema.
//!
//! Task 16 added three constructors, and one of them is a correction to the list
//! below rather than an addition to it:
//!
//! * `infra::clients::qa_runs` — [`DomainError::RunNotIngested`] for a qa-runs
//!   `not_found`, [`DomainError::Forbidden`] for a subject-level refusal from
//!   qa-runs' own PDP, and [`DomainError::Internal`] for anything else. That
//!   module's header holds the whole translation table and why `not_found` and
//!   "not visible to you" must stay indistinguishable.
//! * `domain::service::reconcile` — [`DomainError::Validation`] for a rebuild
//!   window that is not strictly forward, and [`DomainError::Forbidden`] for a
//!   caller carrying the nil tenant.
//! * `domain::service::refuse_scope_beyond_tenant` —
//!   [`DomainError::UnsupportedScope`], added in Task 16's fix round for a
//!   measured fail-open in the write path. It is the variant's only constructor
//!   and is intended to stay that way: a second request-scoped writer should
//!   *call* that function rather than raise this itself.
//!
//! Task 17 added one more, recorded here in Phase A's whole-phase fix wave,
//! which is the same lapse this header criticises qa-runs for above and had
//! committed itself by Task 19:
//!
//! * `infra::storage::db::odata_err` (`db.rs:144`) —
//!   [`DomainError::Validation`] for every `OData` mistake the two flat
//!   collections can be asked to make, with `field` naming the query parameter
//!   the caller typed: `$filter`, `$orderby` or `cursor`. The same function also
//!   constructs [`DomainError::Database`] (`:150`, a driver failure surfacing
//!   through the pager) and [`DomainError::Internal`] (`:151`). **Not `$top`**,
//!   despite a `"$top"` arm existing in `odata_field_of`: that arm is
//!   unreachable from `paginate_odata` and the wire parameter is `limit` with no
//!   `$` — the measurement and why the dead arm is kept are on those two
//!   functions.
//!
//! **A second Task 13 constructor, `infra::events::consumer::start`, lived here
//! until the consumer it belonged to was deleted.** It raised
//! [`DomainError::Internal`] when a broker subscription could not be
//! established at all. No deployment ever registered the client that consumer
//! needed, so the failure it guarded never fired in practice; it left with the
//! consumer and the event-broker dependency (`crate::gear`'s header).
//!
//! Still unconstructed — **two** variants, with the task that will. Named
//! rather than counted, so a `grep -rn 'DomainError::<variant>'` over this crate
//! can settle it. **[`DomainError::JiraNotConfigured`] came off this list at
//! Task 32**, which has two constructors for it:
//! `domain::service::jira::JiraService::check_status`, for a tenant with no
//! config or a disabled one, and `infra::jira::oagw_client`, which refuses to
//! provision egress for a disabled integration before it calls the gateway at
//! all.
//!
//! * [`DomainError::IngestConflict`] — `domain::service::ingest` (Task 13
//!   forecast this and did not need it: the projection's delete-then-insert runs
//!   inside the caller's transaction, so a lost race surfaces as a database
//!   error and rolls back rather than as this variant. It is mapped at the
//!   boundary anyway — see `api::rest::error` — and unconstructed).
//! * [`DomainError::UnsupportedEgress`] — the notification router (Tasks
//!   36-39).
//!
//! **[`DomainError::BugNotFound`] was in that list and should not have been.**
//! Corrected in Task 16's second fix round. `infra::storage::jira_sea_repo`
//! has constructed it since `ef878c22` (`jira_sea_repo.rs:189`), on the
//! concurrent-delete race in `upsert_bug` — the row is gone between the insert
//! and the re-read of the same transaction. That call site documents the race as
//! unreachable in practice, and *that* is the claim worth making; "nothing
//! constructs it" is a different and false one, and it is the one that would let
//! Tasks 31-34 skip a live path unexamined. The same conflation was caught and
//! reverted for [`DomainError::SavedViewNameExists`] one round earlier; the check
//! was simply never re-run across the rest of the list.
//!
//! # `IngestConflict` and `UnsupportedEgress` land here, not in the SDK
//!
//! The plan's Task 8 asked `qa-insights-sdk/src/errors.rs` to add these two to
//! a `QaInsightsError` enum. There is no such enum: all four QA SDKs define
//! `errors.rs` as a single `pub use toolkit_canonical_errors::CanonicalError`
//! re-export, so inventing one would have been the divergence. Task 8 kept the
//! re-export and recorded the two conditions in that file's header, pointing at
//! this one. This is where the pointer lands, and the mappings that header
//! promises — `IngestConflict` to `CanonicalError::Aborted`, `UnsupportedEgress`
//! to `CanonicalError::Unimplemented` — are Task 16's, in `api::rest::error`,
//! which is where every sibling puts its single `From<DomainError> for
//! CanonicalError` (`qa-runs/src/api/rest/error.rs:317`). They are not repeated
//! here, because a domain module that names `CanonicalError` is a domain module
//! that has learned about transports.
//!
//! **Discharged by Task 16**, both of them, with a test each
//! (`api::rest::error::tests::an_ingest_conflict_is_409_aborted` and
//! `..::an_unsupported_egress_channel_is_501_unimplemented`). The chain from
//! this file to there terminates for those two before it is picked back up below
//! for Task 28's own additions.
//!
//! # Task 28 adds one variant and reaches the boundary for another
//!
//! [`DomainError::SavedViewNotFound`] is new — `domain::service::saved_views`'
//! `update` and `delete` are its only constructors, folding the repository's
//! `Ok(None)`/`Ok(false)` "no row matched" answer into a 404, exactly as
//! `qa-runs`' `ScheduleService` does for `DomainError::ScheduleNotFound`. Not on
//! the plan's original file list; flagged here for the reason the brief asks
//! for one.
//!
//! And [`DomainError::SavedViewNameExists`] stops being "constructed but
//! unreachable at the boundary": this file's own header used to name it, beside
//! [`DomainError::BugNotFound`], as a case where "nothing reaches this boundary
//! yet" and "has no constructor" were two different claims. Task 28's
//! `POST`/`PUT` handlers are the callers that make the first claim false too.
//!
//! The chain from
//! `qa-insights-sdk/src/errors.rs` to here to there now terminates.

use thiserror::Error;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Domain-specific errors using thiserror.
#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    /// A run exists upstream but this gear holds no projection of it.
    ///
    /// The asynchronous-ingest contract (`cpt-cf-qa-principle-async-insights`)
    /// makes this a *normal* transient state, not a corruption: the run
    /// finished, the reconcile sweep has not caught up to it yet, and it will
    /// catch up within `reconcile_interval_seconds`. Callers that can render
    /// an empty history should prefer doing so over surfacing this.
    #[error("run {run_id} has not been ingested")]
    RunNotIngested { run_id: Uuid },

    /// The PDP granted the operation, but compiled to a scope this gear's write
    /// path cannot execute.
    ///
    /// # Why this is its own variant and not a `Forbidden`
    ///
    /// It is **not** a denial: the subject holds the grant. What it does not hold
    /// is a grant expressed in a shape the write path can honour, and the two
    /// need different answers because they need different fixes. A 403 sends an
    /// operator to ask for a permission they already have; this sends them, and
    /// whoever authors the policy, at the policy.
    ///
    /// # What cannot be honoured, precisely
    ///
    /// `ResultsRepository::upsert_run_results` filters its two `DELETE`s with the
    /// **full** compiled scope and inserts through `scope_unchecked`, which is a
    /// documented no-op (`libs/toolkit-db/src/secure/db_ops.rs:376-385`). A scope
    /// carrying any predicate besides `owner_tenant_id` therefore makes the
    /// delete match a narrower set than the insert writes, and the projection
    /// gains **duplicate** rows on every replay instead of being replaced. See
    /// `domain::service::refuse_scope_beyond_tenant`, which is the only
    /// constructor, and that repository method's own header.
    ///
    /// `resource` is the PEP resource type whose policy produced the scope
    /// (`"qa.test_result"`), and it is `&'static str` for the same reason
    /// [`Self::CorruptState`]'s `what` is: it is chosen at the call site from a
    /// declared constant, never caller-controlled text. The offending predicate
    /// *names* are logged at WARN and deliberately not carried here — a caller
    /// needs to know their policy's shape is unusable, not which properties this
    /// gear's tables happen to map.
    #[error("the authorization policy for {resource} yields a scope this operation cannot honour")]
    UnsupportedScope { resource: &'static str },

    /// A concurrent write to the same run's projection lost the
    /// delete-then-insert race that makes ingest idempotent.
    ///
    /// Retryable, and the reconciler retries whether or not the caller does.
    /// Named by `qa-insights-sdk/src/errors.rs`' header as one of the two
    /// conditions owed to this file.
    #[error("concurrent ingest for the same run, retry")]
    IngestConflict,

    /// A saved view with this name already exists in the same scope.
    ///
    /// Scope matters: legacy's unique index coalesces a null plan to the empty
    /// string, so a global view and a plan-scoped view may share a name. The
    /// name in this error is therefore not enough to identify the row on its
    /// own.
    ///
    /// Citation widened from the plan's `manager/migrations/001_initial.sql:194`
    /// to `:194-195`, verified 2026-08-20: `:194` is the index *name* line and
    /// the `COALESCE(plan_id, '')` that carries the whole semantics is on
    /// `:195`.
    #[error("saved view '{name}' already exists")]
    SavedViewNameExists { name: String },

    /// No saved view with this id matched the caller's owner scope.
    ///
    /// **Added by Task 28; not on the plan's original list, flagged here.**
    /// Legacy's `api_update_view` and `api_delete_view` both fold "no row" into
    /// a `404 Saved view not found` from `rows_affected() == 0`
    /// (`manager/src/routes/analytics.rs:683`, `:722`), and this gear's
    /// [`crate::domain::repos::saved_views_repo::SavedViewsRepository::update`]/
    /// [`..::delete`] answer the same fact as `Ok(None)`/`Ok(false)` rather than
    /// as an error — see that trait's header for why a `bool`/`Option` return
    /// deliberately does not say *why* no row matched. This variant is where
    /// `domain::service::saved_views` turns that fact into a 404, **absent and
    /// foreign alike** — the same cross-tenant (here, cross-owner) existence
    /// oracle `qa-runs`' `DomainError::ScheduleNotFound` closes, and for the
    /// same reason: a caller told "not found" for another owner's view learns
    /// nothing a caller told "not found" for a nonexistent id could not also
    /// have been told. `id` is the caller's own path parameter, never a value
    /// read off a row the caller may not see.
    #[error("saved view {id} not found")]
    SavedViewNotFound { id: Uuid },

    /// The tenant has no JIRA configuration, so a JIRA-dependent operation
    /// cannot proceed.
    ///
    /// Distinct from a JIRA call that failed: nothing was attempted.
    #[error("JIRA is not configured for this tenant")]
    JiraNotConfigured,

    /// No bug with this JIRA key is registered for the tenant.
    ///
    /// The key is the issue key (`VHP-319`), not a UUID: the registry is keyed
    /// on `(tenant_id, jira_key)` and the key is what every caller has.
    #[error("bug {key} not found")]
    BugNotFound { key: String },

    /// A notification was routed to a channel this deployment has no adapter
    /// for.
    ///
    /// The named case is email, which the design defers (D10): its config,
    /// routing, dedupe and logging all ship, and only the send does not. A
    /// deployment reaching this has a working configuration and a missing
    /// adapter, which is why it is its own variant and not a `Validation`.
    /// The second of the two conditions `qa-insights-sdk/src/errors.rs` owes to
    /// this file.
    #[error("egress channel '{channel}' is not supported by this deployment")]
    UnsupportedEgress { channel: String },

    /// A stored value this gear cannot decode, naming the row.
    ///
    /// Distinct from [`Self::Database`], which is a storage *failure*: this is a
    /// successful read of a value that should not exist. `infra::storage::mapper`
    /// is its only constructor, and every decoder there fails closed into it
    /// rather than substituting a default — a `scope` that quietly decoded to
    /// `all` would put a saved view in the wrong list, and a wrapped negative
    /// `case_count` would inflate every "expected cases" number with nothing
    /// failing.
    ///
    /// `what` is `&'static str` and not `String` on purpose: it is a
    /// column path (`saved_view.scope`, `test_case_collect.case_count`) chosen at
    /// the call site, never caller-controlled text. Same shape as qa-runs'
    /// variant of the same name.
    #[error("corrupt persisted state in {what} for row {id}: {value}")]
    CorruptState {
        what: &'static str,
        id: Uuid,
        value: String,
    },

    /// Input the domain refused.
    #[error("validation failed on {field}: {message}")]
    Validation { field: String, message: String },

    /// The caller is not authorized.
    ///
    /// **Four constructors, not one — corrected in Task 30's fix round, which
    /// found the count still said "three" a task after it stopped being true.**
    /// Through Task 15 this doc said the `From<EnforcerError>` conversion below
    /// was *"this variant's only intended constructor"*, which Task 16 falsified
    /// twice in one task:
    ///
    /// * `From<EnforcerError>` — a PDP deny or a constraint that will not
    ///   compile. Still the main one.
    /// * `infra::clients::qa_runs` — a `permission_denied` or `unauthenticated`
    ///   from **qa-runs'** PDP. Deliberately not folded into
    ///   [`Self::RunNotIngested`]: a decision about the subject is not evidence
    ///   about a row, and answering "nothing to project" to an operator whose
    ///   grant is missing hides the one thing they can fix.
    /// * `domain::service::reconcile::rebuild` — a caller carrying the nil
    ///   tenant, which is the platform-root sentinel. Refused before the PDP is
    ///   asked, because there is no cross-tenant rebuild to authorize.
    /// * `domain::service::collect::CollectService::record_count` — a collect
    ///   report whose `sig` does not verify against the `(repo_id, branch,
    ///   tenant_id)` triple under this deployment's signing secret, or whose
    ///   secret is unconfigured (fail-closed). **Not** a PDP decision — that
    ///   path takes no PDP grant at all (`domain::service::collect`'s header) —
    ///   but the same "this subject may not do this" meaning applies: a caller
    ///   presenting a forged or absent signature may not write as the tenant it
    ///   claims.
    ///
    /// The unifying rule, stated now that there is more than one site: this
    /// variant means *"this subject may not do this"*, and it must never be
    /// reachable from a fact about which rows exist.
    #[error("access denied")]
    Forbidden,

    /// A storage failure.
    #[error("database error: {0}")]
    Database(String),

    /// Anything else, and nothing a caller can act on.
    #[error("internal error: {0}")]
    Internal(String),
}

// TODO(DE1302): `Database(String)` only stores a formatted message, so this
// `From` impl drops the source error. Extend `Database` to hold a boxed source
// so `.source()` returns the original error, then remove this allow. Carried
// verbatim from `qa-environments/src/domain/error.rs`, which carries the same
// TODO against the same lint.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<toolkit_db::DbError> for DomainError {
    fn from(e: toolkit_db::DbError) -> Self {
        DomainError::Database(e.to_string())
    }
}

impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        tracing::error!(error = %e, "AuthZ scope resolution failed");
        match e {
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden,
            authz_resolver_sdk::EnforcerError::EvaluationFailed(err) => {
                Self::Internal(err.to_string())
            }
        }
    }
}
