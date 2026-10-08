//! Notification dedupe, the audit trail, and the egress settings.

use async_trait::async_trait;
use qa_insights_sdk::{NotificationConfig, NotificationLogEntry};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The identity of one send-once notification claim.
///
/// A struct rather than four parameters because `kind` and `event` are both
/// `&str` and adjacent: a call site that transposed them would compile, would
/// claim a slot nothing else ever claims, and would therefore dedupe nothing at
/// all while looking like it worked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationClaim {
    pub run_id: Uuid,
    /// The notification family, e.g. legacy's
    /// `SCHEDULED_RUN_SLACK_NOTIFICATION_KIND`.
    pub kind: String,
    /// The run event that triggered it — legacy's
    /// `ScheduledRunNotificationEvent::label()`.
    pub event: String,
    /// Stamped on the claim row so an operator can tell a stale claim from a
    /// fresh one without joining the audit log.
    pub sent_at: OffsetDateTime,
}

/// One attempt to write to the audit trail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewLogEntry {
    /// `None` for an attempt that belongs to no run — a settings `/test` send.
    pub run_id: Option<Uuid>,
    /// `slack` | `email` — the egress that was tried.
    pub channel: String,
    /// `""` rather than absent for an unattributed entry, matching legacy's
    /// `NOT NULL DEFAULT ''` (`001_initial.sql:210`).
    pub event_type: String,
    pub outcome: String,
    /// Failure detail; `""` on success.
    pub detail: String,
}

/// Persistence for `qa_run_notifications`, `qa_notification_log` and
/// `qa_notification_config`.
#[async_trait]
pub trait NotifyRepository: Send + Sync {
    /// Claim the right to send one notification. `true` when **this** caller
    /// won the claim and must send; `false` when someone else already has.
    ///
    /// # Why this returns `bool` and not `()`
    ///
    /// **The insert *is* the dedupe answer, and an "insert then check" pair
    /// cannot express it.** Two instances racing on the same finished run both
    /// see no claim row, both insert, and both send — unless the answer comes
    /// from the insert itself. `idx_qa_run_notifications_claim` —
    /// `(tenant_id, run_id, notification_kind, event_type)` — is what makes one
    /// of the two inserts fail, and reporting *which* caller won is the only
    /// thing that turns a unique index into a send-once protocol. A method
    /// returning `()` would leave the caller with no way to know, and the
    /// natural repair — read, then insert if absent — reintroduces exactly the
    /// race the index closes.
    ///
    /// This is legacy's shape too: `INSERT ... ON CONFLICT DO NOTHING` followed
    /// by `rows_affected() > 0` (`manager/src/services/notifications.rs:548-562`,
    /// `reserve_run_notification`).
    ///
    /// # Obligation #4 of the schema
    ///
    /// **A unique violation here is "already sent", not an error.** An
    /// implementation that lets it surface as [`DomainError::Database`] turns
    /// every duplicate delivery attempt into a 500 on a path whose entire
    /// purpose is to be idempotent.
    ///
    /// Legacy also has a *release* — a failed send deletes the claim so a retry
    /// can take it again (`notifications.rs:565-591`).
    /// [`Self::release_notification`] is the port of it, and
    /// [`crate::domain::service::notify::NotifyService`] calls it on a failed
    /// send. The exactly-once guarantee this method documents above holds for
    /// **successful** sends only; a failed send is retryable by construction —
    /// see [`Self::release_notification`]'s own doc for the release half of
    /// that split.
    async fn claim_notification<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        claim: NotificationClaim,
    ) -> Result<bool, DomainError>;

    /// Which `notification_kind`s are already claimed for `(tenant_id,
    /// run_id, event)` — the other half of [`Self::claim_notification`]'s
    /// answer, asked *before* spending any work rather than as part of
    /// spending it.
    ///
    /// # Why a read, when the insert is the dedupe answer
    ///
    /// It is not a dedupe check and must never be used as one: a read
    /// followed by an insert is exactly the race
    /// [`Self::claim_notification`]'s own doc says cannot be repaired that
    /// way, and the send-once protocol still rests entirely on the insert.
    /// What this answers is a different question —
    /// [`crate::domain::service::notify::NotifyService::notify_run_completed`]
    /// reaches two cross-gear reads (`get_run`, `list_run_test_results`) and
    /// a settings read before it can decide anything, and the reconcile sweep
    /// re-projects a run with zero result rows on **every** tick for as long
    /// as it sits in the lookback window. For a run whose every channel has
    /// already been decided there is no outcome that work can produce, so
    /// asking first turns twelve cross-gear round trips an hour into one
    /// indexed read (final review, finding 6). Losing the race here costs a
    /// redundant pass, never a duplicate send.
    ///
    /// Scoped by `tenant_id` explicitly as well as by `scope`, for the
    /// explicit-`tenant_id` rule's reason: a compiled scope may legitimately
    /// span several tenants.
    async fn claimed_kinds<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        run_id: Uuid,
        event: &str,
    ) -> Result<Vec<String>, DomainError>;

    /// Release a claim after a failed send, so a retry can take the same slot
    /// again — legacy's `release_run_notification`
    /// (`notifications.rs:565-591`).
    ///
    /// # Why this is safe to call unconditionally on a failure
    ///
    /// A failed send means the message never reached the far side, so no
    /// duplicate delivery is possible if a second attempt later wins the same
    /// claim; the exactly-once guarantee [`Self::claim_notification`]'s own
    /// doc describes is a guarantee about **sent** notifications; a released
    /// claim was, by definition, never sent. Without this call a transient
    /// Slack or SMTP outage would silently and *permanently* suppress the
    /// notification — this trait's own header states that cost.
    ///
    /// # Scoped by tenant, not "whichever row matches"
    ///
    /// The delete carries an explicit `tenant_id` equality predicate alongside
    /// the compiled scope, matching every other `.one()`/delete site this crate
    /// has had to fix for the same defect
    /// (`JiraRepository::find_unclosed_for_test`'s own doc, and
    /// [`JiraRepository`](super::JiraRepository)'s explicit-`tenant_id` rule).
    /// `run_id`, `kind` and `event` narrow it to exactly the slot
    /// [`Self::claim_notification`] would have inserted.
    ///
    /// Takes no error for "no matching row" — a caller that races a release
    /// against another release (or against a claim that was never taken)
    /// finds nothing to delete, which is not a failure.
    async fn release_notification<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        run_id: Uuid,
        kind: &str,
        event: &str,
    ) -> Result<(), DomainError>;

    /// Record one delivery attempt.
    ///
    /// Egress failures are logged here and **never propagated to a caller**,
    /// which makes this table the only place an operator can see
    /// that Slack or SMTP is broken. Legacy swallows even this insert's own
    /// error into a `tracing::warn!` (`notifications.rs:594-617`); the
    /// `Result` here lets the caller make that choice explicitly rather than
    /// having it made for them.
    async fn append_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        entry: NewLogEntry,
    ) -> Result<(), DomainError>;

    /// The most recent audit entries, newest first.
    ///
    /// Legacy `get_notification_log(limit)` (`notifications.rs:618-635`),
    /// `ORDER BY created_at DESC LIMIT $1`, which
    /// `idx_qa_notification_log_tenant_created` serves.
    ///
    /// # `tenant_id` is explicit — the explicit-`tenant_id` rule, fix round 3
    ///
    /// A `LIST` does not have `get_config`'s `.one()` ambiguity (there is no
    /// "arbitrary row" to pick when every matching row is returned), but a
    /// scope spanning several tenants (`ScopeFilter::In`,
    /// `ScopeFilter::InTenantSubtree`) still means this could return another
    /// in-scope tenant's audit rows on the one settings page whose sibling read
    /// (`get_config`) is already pinned to the caller's own tenant. The
    /// explicit-`tenant_id` rule's principle — no read in this gear should
    /// answer with "whichever in-scope row the engine hands back" when a caller
    /// asked about their own tenant specifically — applies here for the same
    /// reason it applies to a `.one()`: `NotifyService::list_log`'s only caller
    /// passes its own tenant, never a caller-chosen one.
    async fn list_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        limit: u64,
    ) -> Result<Vec<NotificationLogEntry>, DomainError>;

    /// The tenant's notification settings, or `None` when never saved.
    ///
    /// `None` is distinct from a row at its column defaults: a tenant that has
    /// never opened the settings page has no row, and legacy's equivalent — a
    /// missing `settings` key — is what its `Default` impl covers. Applying the
    /// default belongs to the service, not here, so that a caller can still
    /// tell "unconfigured" from "configured to the defaults".
    ///
    /// Returns [`NotificationConfig::slack_webhook_credstore_ref`] and never the
    /// webhook URL — obligation #3 of the schema.
    /// The column holds a credential-store reference. The webhook URL it names is
    /// resolved only inside `infra::notify::slack_oagw`, for the length of one
    /// send, and is never stored, returned or formatted.
    ///
    /// # `tenant_id` is explicit — the explicit-`tenant_id` rule, deferred
    /// # from Task 33 and closed here
    ///
    /// [`JiraRepository::get_config`](super::JiraRepository::get_config)'s own
    /// doc named this file as where its identical gap would be closed ("it is
    /// Task 38's file"): a `.one()` read under a scope that spans several
    /// tenants (a parent-tenant grant compiles to
    /// `ScopeFilter::In`/`InTenantSubtree`, not only a single-tenant one) has
    /// no `ORDER BY`, so an unpinned version of this method would return an
    /// **arbitrary** in-scope tenant's row — handing one tenant's Slack webhook
    /// reference and SMTP settings to a caller who asked about another.
    /// `tenant_id` is validated against the compiled scope and then applied as
    /// an explicit equality predicate, [`JiraRepository::get_config`]'s exact
    /// shape.
    async fn get_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<NotificationConfig>, DomainError>;

    /// When this deployment started notifying — `None` on a database that has
    /// not run `m20260929_000004_run_completed_notification_cutoff`.
    ///
    /// # What the caller does with `None`, and why it is not "notify nothing"
    ///
    /// `None` cannot happen on a migrated database: that migration creates the
    /// table *and* writes the row in one `up()`, so "table exists, row absent"
    /// is unreachable. It is `Option` because the type has to be, and
    /// [`crate::domain::service::notify::NotifyService::notify_run_completed`]
    /// reads it as **no cutoff — notify everything**, which is the answer that
    /// fails towards the behaviour the gear had before the cutoff existed
    /// rather than towards silence. A missing row must not be able to turn the
    /// whole feature off quietly; a feature that has stopped sending is the
    /// harder outage to notice, because nothing errors.
    ///
    /// # The row is deployment-wide, and the scope is still a tenant's
    ///
    /// One row, `tenant_id` nil, because the upgrade happened once for the
    /// whole deployment — and because it *cannot* be derived per tenant from
    /// this gear's own tables: the runs it exists to suppress are exactly the
    /// ones that left no row in `qa_test_results` (that migration's header
    /// carries the measurement). Implementations read it under
    /// `AccessScope::for_tenant(Uuid::nil())`, never
    /// `AccessScope::allow_all()`: `crate::domain::elevated`'s doc reserves the
    /// unrestricted scope for the one cross-tenant enumeration, and a
    /// single-row read by primary key is not one.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure — **and that is
    /// deliberately not folded into `None`.** A database that cannot answer
    /// "where does history end?" must not be read as "there is no history",
    /// which would notify every run in the lookback window on the next sweep.
    async fn run_completed_cutoff<C: DBRunner>(
        &self,
        runner: &C,
    ) -> Result<Option<OffsetDateTime>, DomainError>;

    /// Create or replace the tenant's notification settings.
    ///
    /// One row per tenant, enforced by `idx_qa_notification_config_tenant`.
    ///
    /// Returns `()`, not the stored config: [`NotificationConfig`] has no `id`
    /// and no timestamps, so the repository mints nothing and the return value
    /// could only be the argument handed back. `upsert_count` returns `()` for
    /// the same reason. The one upsert here that *does* return its row is
    /// `JiraRepository::upsert_bug`, and that difference is real rather than
    /// stylistic — its `DO NOTHING` case can leave a row that differs from the
    /// input.
    async fn save_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        config: NotificationConfig,
    ) -> Result<(), DomainError>;
}
