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
    /// can take it again (`notifications.rs:565-591`). **R100 (Task 38) is that
    /// task**: [`Self::release_notification`] is the port of it, and
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

    /// Release a claim after a failed send, so a retry can take the same slot
    /// again — legacy's `release_run_notification`
    /// (`notifications.rs:565-591`, R100).
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
    /// # R86 — scoped by tenant, not "whichever row matches"
    ///
    /// The delete carries an explicit `tenant_id` equality predicate alongside
    /// the compiled scope, matching every other `.one()`/delete site this
    /// crate has had to fix for the same defect
    /// (`JiraRepository::find_unclosed_for_test`'s own doc, `domain::service`'s
    /// R86 paragraph). `run_id`, `kind` and `event` narrow it to exactly the
    /// slot [`Self::claim_notification`] would have inserted.
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
    /// Egress failures are logged here and **never propagated to a caller**
    /// (design §4.8), which makes this table the only place an operator can see
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
    /// # `tenant_id` is explicit — R106, fix round 3
    ///
    /// A `LIST` does not have `get_config`'s `.one()` ambiguity (there is no
    /// "arbitrary row" to pick when every matching row is returned), but a
    /// scope spanning several tenants (`ScopeFilter::In`,
    /// `ScopeFilter::InTenantSubtree`) still means this could return another
    /// in-scope tenant's audit rows on the one settings page whose sibling
    /// read (`get_config`) is already pinned to the caller's own tenant. R86's
    /// principle — no read in this gear should answer with "whichever
    /// in-scope row the engine hands back" when a caller asked about their
    /// own tenant specifically — applies here for the same reason it applies
    /// to a `.one()`: `NotifyService::list_log`'s only caller passes its own
    /// tenant, never a caller-chosen one.
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
    /// webhook URL — obligation #3 of the schema. The column holds a
    /// credential-store reference and the material never enters this gear.
    ///
    /// # `tenant_id` is explicit — R86, R83's deferred item, closed here
    ///
    /// [`JiraRepository::get_config`](super::JiraRepository::get_config)'s own
    /// doc named this file as where its identical gap would be closed
    /// (controller ruling R83, "it is Task 38's file"): a `.one()` read under
    /// a scope that spans several tenants (a parent-tenant grant compiles to
    /// `ScopeFilter::In`/`InTenantSubtree`, not only a single-tenant one) has
    /// no `ORDER BY`, so an unpinned version of this method would return an
    /// **arbitrary** in-scope tenant's row — handing one tenant's Slack
    /// webhook reference and SMTP settings to a caller who asked about
    /// another. `tenant_id` is validated against the compiled scope and then
    /// applied as an explicit equality predicate, [`JiraRepository::get_config`]'s
    /// exact shape.
    async fn get_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<NotificationConfig>, DomainError>;

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
