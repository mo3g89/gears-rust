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
//!   boundary anyway — see the boundary mapping below — and unconstructed).
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
//! to `CanonicalError::Unimplemented` — are Task 16's, in the boundary mapping
//! **below in this same file**.
//!
//! They were written in `api::rest::error`, and this paragraph used to say they
//! were "not repeated here, because a domain module that names `CanonicalError`
//! is a domain module that has learned about transports". The review's own
//! findings #15/#16 went the other way: two domain modules were already naming
//! `CanonicalError`, and the mapping sitting one layer up is what made their
//! fall-through arms call *into* the transport layer. `qa-runs` moved its impl
//! next to its enum for the same reason (`qa-runs/src/domain/error.rs`), and so
//! did this one.
//!
//! **Discharged by Task 16**, both of them, with a test each
//! (`api::rest::error::tests::an_ingest_conflict_is_409_aborted` and
//! `..::an_unsupported_egress_channel_is_501_unimplemented` — the tests stayed
//! in `api::rest::error`, which still owns the two call-site renderers they
//! share a module with). The chain from `qa-insights-sdk` to those two arms
//! terminates here before it is picked back up below for Task 28's own
//! additions.
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
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_macros::domain_model;
use uuid::Uuid;

// The resource type the mapping below shares with
// `domain::error_attribution::as_jira_error` -- imported rather than
// re-declared so both raise the same `gts_id`.
use crate::domain::error_attribution::JiraBugResourceError;

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

    /// Anything else, and nothing a caller can act on.
    #[error("internal error: {0}")]
    Internal(String),
}

impl DomainError {
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
// rather than by an import -- and that is precisely why it had to move. While
// it sat in the transport layer, `domain::error_attribution::as_jira_error`'s
// `other => other.into()` fall-through was a *domain* module calling into
// `api::rest::error`, and `api` is a transport over `domain` rather than the
// other way round. Review findings #15, #16.
//
// **Deleting the `use` line did not close that**, which is the mistake this
// move corrects: `crate::no_api_in_domain_tests` is a text scan over imports,
// so it could never have contradicted a claim about a trait impl. The scan
// pins the imports; the location of the impl is what pins this.
//
// The three resource types the mapping raises came with it. `api::rest::error`
// imports them back for its own two call-site renderers and re-exports what a
// handler needs, so a REST handler still imports its error rendering from
// exactly one module.
//
// # What this mapping is written to keep
//
// * **The `match` has no catch-all arm**, so adding a [`DomainError`] variant
//   is a compile error here rather than a silent 500.
//
//   **Two** variants are mapped that nothing in this crate constructs yet, and
//   they are mapped anyway for exactly that reason: the task that raises one
//   should find the boundary decision already taken and reviewed, not discover
//   it as an unexplained 500. Named individually rather than counted, so that a
//   `grep -rn 'DomainError::<variant>'` can settle the claim:
//
//   * [`DomainError::UnsupportedEgress`] -- the notification router, Tasks
//     36-39.
//   * [`DomainError::IngestConflict`] -- **neither loop.** Task 13 forecast it
//     for the ingest path and did not need it; this module's own header records
//     why (the projection's delete-then-insert runs inside the caller's
//     transaction, so a lost race is a rolled-back database error).
//
//   [`DomainError::JiraNotConfigured`] left that list at Task 32, which is both
//   its first constructor and its first path to this boundary:
//   `domain::service::jira::JiraService::check_status` raises it for a tenant
//   with no config or a disabled one, and `infra::jira::oagw_client` raises it
//   before it will provision egress for a disabled integration. It is the third
//   variant to make that transition (after `SavedViewNameExists` and
//   `SavedViewNotFound` at Task 28) and the count above is adjusted rather than
//   the bullet merely deleted, because a stale count is what this paragraph is
//   about.
//
//   One variant is deliberately **not** in that list, and was wrongly put there
//   by earlier revisions:
//
//   * [`DomainError::BugNotFound`] -- constructed since `ef878c22` at
//     `infra/storage/jira_sea_repo.rs:189`, on the "the bug row is gone between
//     the insert and the re-read of the same transaction" race in `upsert_bug`.
//     That site's own comment calls the race unreachable in practice, which is a
//     different claim from "no code constructs it" -- and treating the two as
//     one is what would let a Tasks 31-34 engineer skip the path without ever
//     looking at it.
//
//   **The rule this paragraph keeps getting wrong**, stated so the next edit
//   does not: "has no constructor", "is unreachable in practice" and "cannot
//   reach a handler yet" are three different properties. Only the first belongs
//   in the list above, and only a grep decides it.
//
// # The two mappings this file *owed* before the mapping existed
//
// This module's header names them, and the debt is discharged below:
// [`DomainError::IngestConflict`] to `CanonicalError::Aborted` (409,
// retryable), and [`DomainError::UnsupportedEgress`] to
// `CanonicalError::Unimplemented` (501 -- a working configuration and a missing
// adapter, which is why it is not a `Validation`). `qa-insights-sdk/src/errors.rs`
// pointed here; the chain now terminates in this file rather than one layer up.
//
// # What a caller may read
//
// [`DomainError::Database`], [`DomainError::Internal`] and
// [`DomainError::CorruptState`] carry text that originates in a database
// driver, in this gear's internals, or in a persisted column. **None of it may
// reach an HTTP body**: driver text names indexes, columns and key values, and
// `CorruptState` carries the offending column's contents verbatim. Each is
// logged at ERROR with the real cause and answered with the canonical internal
// detail, which `toolkit-canonical-errors` supplies and this gear does not
// choose.
//
// `opaque_internal` takes the [`DomainError`] rather than a message, so it
// cannot be handed pre-formatted text by mistake -- qa-runs' idiom, and the
// reason it is worth copying is that the mistake it prevents is a one-word
// edit.

/// `qa_test_results` and `qa_test_case_results` — the projection this gear
/// exists to serve, and the resource the rebuild endpoint mutates.
///
/// Matches `domain::service::resources::TEST_RESULT`, deliberately: the PEP
/// resource type and the error resource type name the same thing, and a caller
/// denied on `qa.test_result` should be told about `cf.qa.insights.test_result`.
#[resource_error(gts_id!("cf.qa.insights.test_result.v1~"))]
pub(crate) struct TestResultResourceError;

/// `qa_saved_views`. Declared now because [`DomainError::SavedViewNameExists`]
/// already exists and the `match` below is exhaustive — a caller told a *test
/// result* already exists while naming a saved view would go looking for the
/// wrong row. Task 28 owns the endpoints.
#[resource_error(gts_id!("cf.qa.insights.saved_view.v1~"))]
pub(crate) struct SavedViewResourceError;

/// `qa_notification_config`, `qa_notification_log` and `qa_run_notifications`.
/// Tasks 36-39; [`as_notification_error`] is Task 38's call-site renderer.
#[resource_error(gts_id!("cf.qa.insights.notification.v1~"))]
pub(crate) struct NotificationResourceError;

/// Log the real cause, answer with the canonical internal detail.
///
/// Takes the error rather than a message so no call site can pass it text it
/// formatted itself — the payloads of these three variants are precisely what
/// must not cross the boundary.
fn opaque_internal(e: &DomainError) -> CanonicalError {
    tracing::error!(error = ?e, "internal error reached the API boundary");
    CanonicalError::internal("An internal error occurred").create()
}

impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        let ce = match &e {
            // -- 404, not found ---------------------------------------------
            //
            // Asynchronous ingest makes this a *normal transient state*, not a
            // corruption: the run finished and the event has not been consumed
            // yet. `DomainError::RunNotIngested`'s own doc says a caller that
            // can render an empty history should prefer doing so — so this arm
            // exists for the callers that genuinely cannot, and 404 is the
            // honest answer for them. It is also what keeps the cross-tenant
            // existence oracle closed: a run in another tenant reaches this
            // gear as the same variant (`infra::clients::qa_runs`).
            DomainError::RunNotIngested { run_id } => TestResultResourceError::not_found(format!(
                "No results have been ingested for run {run_id}"
            ))
            .with_resource(run_id.to_string())
            .create(),

            DomainError::BugNotFound { key } => {
                JiraBugResourceError::not_found(format!("Bug {key} is not tracked"))
                    .with_resource(key.clone())
                    .create()
            }

            // Absent and another owner's are the same 404 — this variant's own
            // doc states the cross-owner existence oracle it closes, matching
            // `qa-runs`' `DomainError::ScheduleNotFound`.
            DomainError::SavedViewNotFound { id } => {
                SavedViewResourceError::not_found(format!("Saved view {id} not found"))
                    .with_resource(id.to_string())
                    .create()
            }

            // -- 409, already exists / aborted ------------------------------
            DomainError::SavedViewNameExists { name } => SavedViewResourceError::already_exists(
                format!("A saved view named '{name}' already exists in this scope"),
            )
            .with_resource(name.clone())
            .create(),

            // Retryable, and the reconciler retries whether or not the caller
            // does — which is why this is `Aborted` (409, "try again") rather
            // than an opaque 500.
            DomainError::IngestConflict => {
                TestResultResourceError::aborted("Concurrent ingest for the same run, retry")
                    .with_reason("INGEST_CONFLICT")
                    .create()
            }

            // -- 400, failed precondition / invalid argument ----------------
            //
            // Nothing was attempted, which is what separates this from a JIRA
            // call that failed: the tenant has no configuration at all.
            DomainError::JiraNotConfigured => JiraBugResourceError::failed_precondition()
                .with_precondition_violation(
                    "jira_configuration",
                    "JIRA is not configured for this tenant",
                    "NOT_CONFIGURED",
                )
                .create(),

            // **This said "every `Validation` this gear can raise today comes
            // from the rebuild window", and that was already false when it was
            // written.** `domain::error`'s own header lists `infra::storage::db::
            // odata_err` — `$filter`, `$orderby` and `cursor` on the two flat
            // collections — and `infra::storage::mapper` raises one too. Task 25a
            // adds `domain::analytics::query`, the overview query string's five
            // rules (`product_id`, `version`, `scope`, `group_by`, `plan_id`) plus
            // the build-tests drill-down's `build`. Corrected rather than extended,
            // because the false premise is what made the conclusion look
            // load-bearing.
            //
            // `TEST_RESULT` is nevertheless right for this arm's *unrendered*
            // callers — every one of them sits behind a test-result read or
            // write: `resources::TEST_RESULT` with `actions::LIST` is the grant
            // the overview and both collections require, and
            // `actions::REBUILD` the one the rebuild does.
            //
            // **The saved-view name was the candidate this comment named, and
            // Task 28 is the fix round.** `domain::service::saved_views` raises
            // `Validation` on `scope`, `name` and `plan_path`, none of which is
            // a test-result field, so its four handlers do not reach this arm
            // at all — they wrap the service call in `as_saved_view_error`
            // instead, which re-attributes `Validation` (and `Forbidden`) to
            // `SavedViewResourceError` before this blanket `match` ever sees
            // them. This arm stays the right default for every caller that
            // still reaches it unwrapped, and the rule for the *next* one is
            // unchanged: a `Validation` whose resource is not the test result
            // needs a call-site renderer, exactly as qa-runs' `Validation` arm
            // records. Branching on the field name would mis-attribute the
            // next field added on either side.
            DomainError::Validation { field, message } => {
                TestResultResourceError::invalid_argument()
                    .with_field_violation(field, message, "VALIDATION")
                    .create()
            }

            // A **grant** the write path cannot execute, not a denial — so 400
            // with a named precondition violation and deliberately not the 403
            // below. An operator who sees 403 goes and asks for the `rebuild`
            // grant they already hold; what actually needs changing is the shape
            // of the policy that answers it. `JiraNotConfigured` above sets the
            // precedent for surfacing a deployment-configuration precondition
            // this way.
            //
            // Attributed to the test-result resource because that is the only
            // resource whose write path raises it today. A second raiser needs a
            // call-site renderer, exactly as the `Validation` arm below records —
            // and unlike that one, this variant carries the PEP resource type, so
            // the renderer has something to switch on.
            DomainError::UnsupportedScope { resource } => {
                tracing::error!(
                    %resource,
                    "the authorization policy compiled to a scope this gear cannot execute; \
                     the operation was refused. See domain::service::refuse_scope_beyond_tenant",
                );
                TestResultResourceError::failed_precondition()
                    .with_precondition_violation(
                        "authorization_policy",
                        format!(
                            "The policy for '{resource}' grants this operation with a scope \
                             constraining more than the tenant, which this operation cannot \
                             honour. It must constrain owner_tenant_id only."
                        ),
                        "SCOPE_NOT_TENANT_ONLY",
                    )
                    .create()
            }

            // -- 403 --------------------------------------------------------
            DomainError::Forbidden => TestResultResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),

            // -- 501 --------------------------------------------------------
            //
            // Email, which design decision D10 defers: its config, routing,
            // dedupe and logging all ship and only the send does not. A
            // deployment reaching this has a *working* configuration and a
            // missing adapter, so it is not a 400 — there is nothing the caller
            // can change about the request.
            DomainError::UnsupportedEgress { channel } => NotificationResourceError::unimplemented(
                format!("This deployment has no adapter for the '{channel}' channel"),
            )
            .create(),

            // -- 500, opaque ------------------------------------------------
            DomainError::CorruptState { .. }
            | DomainError::Database { .. }
            | DomainError::Internal(_) => opaque_internal(&e),
        };

        if let Some(diag) = ce.diagnostic() {
            tracing::debug!(diagnostic = %diag, "Canonical error diagnostic");
        }

        ce
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! `From<EnforcerError> for DomainError`, pinned one arm per test so a
    //! future change to any single arm fails exactly one test rather than
    //! being lost in a combined assertion.
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
    /// asked for row-level constraints but received an empty set. Fail-closed"
    /// (`authz-resolver-sdk/src/pep/compiler.rs:45-48`). That is a
    /// scoped-permission refusal — the same shape as `Denied` — not a broken
    /// policy engine, so a reader who knows only finding #3 ("a compile fault
    /// is a 500") would expect this test to assert `Internal` and would be
    /// wrong: this specific compile failure is about the caller, not the PDP.
    #[test]
    fn a_scope_required_but_absent_is_still_forbidden() {
        let e = EnforcerError::CompileFailed(ConstraintCompileError::ConstraintsRequiredButAbsent);
        assert!(
            matches!(DomainError::from(e), DomainError::Forbidden),
            "ConstraintsRequiredButAbsent is a documented deny, not a fault"
        );
    }

    /// The half finding #3 is actually about: the PDP named predicates this
    /// PEP cannot compile at all — a configuration fault, not a fact about
    /// the caller's permissions (`compiler.rs:52`).
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
