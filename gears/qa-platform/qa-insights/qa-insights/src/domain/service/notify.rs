//! The notification service: settings CRUD, the audit log, the test/preview
//! surfaces, and [`Self::notify_run_completed`] — the send-once path that
//! [`crate::domain::notify::routing`] and [`crate::domain::notify::render`]
//! feed into. Task 38, fix round 1 (rulings R103, R104).
//!
//! # Where this task's four legacy citations live
//!
//! `api_get_notifications`/`api_update_notifications`
//! (`manager/src/routes/settings.rs:434-462`) are [`Self::get_config`] and
//! [`Self::save_config`]; `api_get_notification_log`
//! (`:752-773`, over `get_notification_log`,
//! `manager/src/services/notifications.rs:622-637`) is [`Self::list_log`];
//! `api_test_notifications`/`api_preview_notifications` (`:465-507`, over
//! `send_test_notification` `notifications.rs:358-385`,
//! `send_scheduled_run_test_notification` `:407-428` and
//! `preview_scheduled_run_message` `:388-404`) are [`Self::send_test`] and
//! [`Self::preview_scheduled_run`].
//!
//! # R104: `notify_run_completed` resolves a real run through [`RunsReader`]
//!
//! **Fix round 1 replaced a false green.** The first draft rendered
//! [`render::RunCompletedRenderContext`] from the bare run id alone
//! (`run_name` was the id's string form) — the claim, the send and the log
//! entry were all real, but the message a human would actually receive read
//! as a UUID. [`RunsReader::get_run`] and [`RunsReader::list_run_test_results`]
//! already exist on a port this crate already owns (`domain/ports/runs_reader.rs:84`,
//! `:101`) and carry everything [`render::RunCompletedRenderContext`] needs:
//! [`Self::notify_run_completed`] now calls both and builds the context from
//! the real [`qa_runs_sdk::Run`] and its result rows. `platform` and
//! `product_key` stay `None` — not a stand-in, but this context's honest
//! answer: `product_key` has no equivalent in this architecture at all
//! (`domain::notify::routing`'s header records the removal, VHP-319), and
//! resolving `platform_id` to a display name needs
//! [`crate::domain::ports::EnvironmentReader`], which nothing wires into this
//! service — [`render::render_run_completed`]'s own `display_value` already
//! renders an absent field as `"-"`, which is what legacy shows for a field
//! it never had either, not a fabricated value standing in for a real one.
//!
//! # `RunNotIngested` is a normal, expected state here — legacy has no
//! # equivalent to compare against
//!
//! Legacy's `notify_run_completed` takes the already-resolved `WorkflowRun`
//! as a parameter; nothing in legacy ever fetches a run partway through
//! sending its notification, so there is no legacy behaviour to port for
//! "the run does not exist yet, or is not visible." This gear's async-ingest
//! architecture makes that a real, reachable state — the event that should
//! eventually call this method can race the projection that would make
//! [`RunsReader::get_run`] succeed — and [`DomainError::RunNotIngested`]'s
//! own doc across this crate treats it as transient, not a corruption
//! (`domain::service::ingest`'s header). [`Self::notify_run_completed`]
//! follows that precedent: a `RunNotIngested` (or a `list_run_test_results`
//! failure) is logged under the pseudo-channel `"run"` — legacy's own
//! precedent for a failure that pre-empts any channel decision at all is
//! `notify_queue_event`'s config-load-failure branch, which logs under the
//! pseudo-channel `"run_queue"` rather than `"slack"`/`"email"`
//! (`notifications.rs:664-683`) — and the method returns `Ok(())` without
//! claiming or sending anything, matching this method's own
//! never-propagates-a-failure contract.
//!
//! # R104's other half, closed by R105: real per-schedule settings
//!
//! **Fix round 2.** `qa_runs_sdk::Run` carries `schedule_id: Option<Uuid>` and
//! nothing else schedule-shaped; the settings themselves live on
//! `qa_runs_sdk::Schedule`. Fix round 1 found the gap and reported it rather
//! than silently defaulting it a second time; controller ruling R105 settled
//! it in this same task, on the R73 precedent (a port grows in the task that
//! consumes it) rather than deferred to Task 40's wiring pass.
//! [`RunsReader::get_schedule_notifications`] is the new port method — added
//! here, with its adapter in `infra::clients::qa_runs` — and
//! [`Self::notify_run_completed`] now calls it whenever `run.schedule_id` is
//! `Some`, and applies [`no_scheduled_notifications`] when it is `None`.
//!
//! # What an absent schedule means, checked against legacy rather than
//! # guessed
//!
//! Legacy's `notify_run_completed` gates its **entire** body on
//! `is_scheduled_run(run)` first (`notifications.rs:193-206`): a run that is
//! not scheduled logs `"skipped"` unconditionally and returns before any of
//! `config.slack_enabled`, `config.email_enabled` or a per-run override is
//! ever consulted. [`no_scheduled_notifications`] ports that exactly:
//! `ScheduleNotificationSettings { slack_enabled: false, .. }` makes
//! `routing::route`'s `Event::RunCompleted` arm compute `schedule_notifies =
//! false`, which — R94a/R96's own formula, `!schedule_notifies ||
//! config.slack_enabled` — is unconditionally `true`, so the skip is always
//! audited, exactly as legacy's early return always logs regardless of
//! `config.slack_enabled`'s value. The same fallback applies when
//! `schedule_id` is `Some` but [`RunsReader::get_schedule_notifications`]
//! answers `None` (deleted, or not visible to this context): that is
//! "nothing to narrow with", the identical shape as no schedule at all, and
//! routing proceeds normally from there. **An `Err` from that call is
//! different and is not folded the same way**: it is a transport or gateway
//! failure, not a fact about the schedule, so [`Self::notify_run_completed`]
//! treats it exactly like a `get_run`/`list_run_test_results` failure above
//! — logged under the pseudo-channel `"run"` with outcome `"failed"`, and
//! the method returns before `routing::route` is ever called at all, rather
//! than falling through to [`no_scheduled_notifications`]. Silently
//! degrading a qa-runs outage into "this schedule doesn't notify" would hide
//! the actual problem behind a plausible-looking skip.
//!
//! This gear infers "not a scheduled run" from `schedule_id.is_none()`
//! rather than re-deriving legacy's `is_scheduled_run` (which also checks
//! `run.name`/`run.message` for a `"cron-"` prefix or substring — a
//! workaround for a system that had no first-class schedule reference at
//! all). `qa_runs_sdk::Run::schedule_id` **is** that first-class reference,
//! set precisely when a schedule launched the run, so it is the direct
//! successor to legacy's heuristic rather than a second guess alongside it.
//!
//! # R103: the two egress ports move to `gear.rs`
//!
//! Task 39 ships `SlackOagwClient` and `UnsupportedMailClient`; until then,
//! `gear.rs` builds [`Self`]'s two egress ports from its own
//! `NeverWiredSlackClient`/`NeverWiredMailClient` stand-ins rather than this
//! module's — controller ruling R103 moved them there: infra stand-ins
//! belong in the file that owns infra binding (`gear.rs`'s own doc on
//! `ConcreteAppServices`), not private to a domain service module. This
//! module receives them as plain `Arc<dyn SlackClient>`/`Arc<dyn MailClient>`
//! constructor arguments and does not know or care that they are
//! placeholders — those two [`Self::new`] parameters accept whichever
//! concrete adapter a caller hands them, unchanged either way.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_insights_sdk::{NotificationConfig, NotificationLogEntry};
use qa_runs_sdk::{Run, RunTestResult, SLACK_NOTIFICATION_EVENTS, ScheduleNotificationSettings};
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::notify::render::{self, RunCompletedRenderContext, ScheduledRunRenderContext};
use crate::domain::notify::routing::{self, Event, NotificationKind};
use crate::domain::ports::{
    MailClient, MailMessage, RunsReader, SendOutcome, SlackBlock, SlackClient, SlackMessage,
    validate_credstore_ref,
};
use crate::domain::repos::{NewLogEntry, NotifyRepository};
use crate::domain::service::{DbProvider, actions, resources};

/// `outcome = "sent"` — a send that reached the far side.
const OUTCOME_SENT: &str = "sent";
/// `outcome = "skipped"` — a routing decision or a lost dedupe claim, neither
/// of which is a failure.
const OUTCOME_SKIPPED: &str = "skipped";
/// `outcome = "failed"` — a send attempt that errored. Legacy's own log uses
/// `"error"` for this case (`notifications.rs:308`, `:346`, `:522`); this
/// gear's vocabulary spells it `"failed"` instead, per this task's brief,
/// which pins the string verbatim in
/// `a_failed_send_is_logged_with_its_detail`.
const OUTCOME_FAILED: &str = "failed";
/// `outcome = "unsupported_egress"` — **added by this task.** The prior
/// vocabulary, enumerated above so the addition is visibly one: `"sent"`,
/// `"skipped"`, `"failed"`.
const OUTCOME_UNSUPPORTED_EGRESS: &str = "unsupported_egress";

/// The settings page's generic test message, verbatim from legacy
/// (`notifications.rs:364`).
const GENERIC_TEST_MESSAGE: &str = "VHP Test Manager: test notification from Settings page";

/// The R102 seam: [`SendOutcome`] to this audit log's `outcome` string.
///
/// **This is the one place that maps the variant to the string.** Task 39's
/// tests pin the variant (`UnsupportedMailClient` returns it); this crate's
/// own tests pin the string (the log's `outcome` column); nothing but this
/// function connects the two, so it is named and tested directly rather than
/// left implicit inside a `match` arm at the call site.
#[must_use]
pub(crate) fn outcome_str(outcome: SendOutcome) -> &'static str {
    match outcome {
        SendOutcome::Sent => OUTCOME_SENT,
        SendOutcome::UnsupportedEgress => OUTCOME_UNSUPPORTED_EGRESS,
    }
}

/// What `POST /qa/v1/settings/notifications/test` sends — legacy's payload
/// dispatch: `Option<Json<ScheduledRunNotificationPreviewRequest>>`
/// (`manager/src/routes/settings.rs:465-496`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestSend {
    /// No payload — the settings page's generic test button.
    /// `send_test_notification` (`notifications.rs:358-385`).
    Generic,
    /// A payload naming an override config and an event token —
    /// `send_scheduled_run_test_notification` (`:407-428`). `config` is
    /// boxed: [`NotificationConfig`] carries the six-template document, and
    /// clippy's `large_enum_variant` is right that leaving it unboxed would
    /// size every [`TestSend::Generic`] the same as the variant that
    /// actually needs the space.
    ScheduledRun {
        config: Box<NotificationConfig>,
        event: String,
    },
}

/// A rendered scheduled-run preview, plus the event it was rendered for —
/// legacy's `ScheduledRunNotificationPreviewResponse`
/// (`manager/src/models.rs`, surfaced by `preview_scheduled_run_message`,
/// `notifications.rs:388-404`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledRunPreview {
    /// The token the caller supplied, e.g. `"failed"`.
    pub event: String,
    /// The Title Case label legacy's `ScheduledRunNotificationEvent::label()`
    /// produces (`manager/src/models.rs:975-984`) — see
    /// [`scheduled_run_event_label`].
    pub event_label: String,
    pub rendered_message: String,
    pub fallback_text: String,
    /// The layout, in this gear's vocabulary; `api::rest::dto` encodes it into
    /// Block Kit for the response, with the same encoder the Slack adapter
    /// uses (review finding #17).
    pub blocks: Vec<SlackBlock>,
}

/// The [`DomainError::Validation`] a token outside
/// [`SLACK_NOTIFICATION_EVENTS`] gets, naming `event` and listing the valid
/// tokens.
///
/// One function rather than the literal at each site because three callers
/// raise it — [`NotifyService::preview_scheduled_run`] on the label lookup and
/// again on the render, and [`NotifyService::send_test`]'s
/// [`TestSend::ScheduledRun`] arm — and a caller fixing `event` should see one
/// message rather than three that can drift.
///
/// **The twelve-line doc block that used to sit here belongs to
/// [`scheduled_run_event_label`]** and has been moved there (Phase C's final
/// review): it described a `None` return and a ported label table, and this
/// function returns a `DomainError` unconditionally and has no `None` case at
/// all.
fn invalid_event_error(token: &str) -> DomainError {
    DomainError::Validation {
        field: "event".to_owned(),
        message: format!(
            "'{token}' is not one of {}",
            SLACK_NOTIFICATION_EVENTS.join(", ")
        ),
    }
}

/// Ported from `ScheduledRunNotificationEvent::label`
/// (`manager/src/models.rs:975-984`) — the same six words
/// `domain::notify::render`'s private `status_defaults` carries for its own
/// purposes. Duplicated rather than exposed from that module: `render`'s
/// table is `fn`-private and Task 37's file is consumed here, not extended
/// (this task's mandate); a six-arm, one-word-per-arm table repeated once
/// more is the same shape `render.rs` itself accepts for its `truthy_fields`
/// tables, which exist once per reader for the same reason.
///
/// `None` for anything outside [`SLACK_NOTIFICATION_EVENTS`] — the same
/// partiality as [`routing::Event::scheduled_run`] and
/// [`render::render_scheduled_run`], so all three agree about which tokens
/// are valid.
fn scheduled_run_event_label(token: &str) -> Option<&'static str> {
    Some(match token {
        "pending" => "Pending",
        "in_progress" => "In progress",
        "succeeded" => "Succeeded",
        "failed" => "Failed",
        "error" => "Error",
        "skipped" => "Skipped",
        _ => return None,
    })
}

/// The schedule settings for a run this method treats as **not** scheduled
/// — no `schedule_id` at all, or one [`RunsReader::get_schedule_notifications`]
/// could not resolve. See this module's header, "What an absent schedule
/// means", for why `slack_enabled: false` is legacy's own answer
/// (`is_scheduled_run`'s unconditional skip, `notifications.rs:193-206`)
/// and not a guess.
fn no_scheduled_notifications() -> ScheduleNotificationSettings {
    ScheduleNotificationSettings {
        slack_enabled: false,
        slack_channel: None,
        slack_events: Vec::new(),
    }
}

/// A representative scheduled-run render context for the preview and
/// scheduled-run-test surfaces — legacy's `sample_run_for_event`/
/// `sample_results_for_event` (`notifications.rs:1503-1602`) collapsed to
/// **one** context shared by every event token rather than six bespoke ones.
///
/// This is a narrower port than legacy's, and deliberately so: nothing in
/// [`render::render_scheduled_run`]'s logic reads this context's fields
/// against the event token for internal consistency (a `"failed"` render
/// with a `"Succeeded"`-flavoured `phase` still renders every placeholder
/// correctly) — only the *defaults table* `render` holds internally varies
/// by token. Six near-identical literals would document only that legacy
/// picked flavour text per status, which this preview does not need to
/// reproduce to prove the template renders.
fn sample_render_context() -> ScheduledRunRenderContext {
    ScheduledRunRenderContext {
        run_name: "nightly-smoke-142".to_owned(),
        plan_id: "vhp/smoke".to_owned(),
        phase: "Preview".to_owned(),
        run_source: "scheduled".to_owned(),
        platform: Some("vp-nightly-1".to_owned()),
        product_key: Some("VHP".to_owned()),
        app_version: Some("8.0.1".to_owned()),
        app_build: Some("1024".to_owned()),
        test_version: Some("main".to_owned()),
        schedule_id: Some("nightly".to_owned()),
        repo_name: Some("vhp-e2e".to_owned()),
        source_ref: Some("main".to_owned()),
        source_ref_kind: Some("branch".to_owned()),
        started_at: Some("2026-04-07T01:01:00Z".to_owned()),
        finished_at: Some("2026-04-07T01:13:24Z".to_owned()),
        duration: Some("12m 24s".to_owned()),
        message: Some("Sample preview data".to_owned()),
        result_statuses: vec![
            "PASSED".to_owned(),
            "PASSED".to_owned(),
            "FAILED".to_owned(),
            "SKIPPED".to_owned(),
        ],
    }
}

/// `slack_channel`, normalized: `None` for absent or blank, matching legacy's
/// `normalized_channel` (`notifications.rs:834-838`), now over the real
/// schedule (R105) — a schedule's own `slack_channel` override wins,
/// exactly as legacy's `effective_slack_channel(&config, Some(run))`
/// prefers `run.slack_channel` (`notifications.rs:840-847`) — falling back
/// to the tenant-wide config channel when the schedule has none, or when
/// `schedule` is `None` — [`Self::send_test`]'s two callers have no run and
/// therefore no schedule at all, legacy's own `effective_slack_channel(&config,
/// None)` (`notifications.rs:367`, `:420`).
fn effective_channel(
    config: &NotificationConfig,
    schedule: Option<&ScheduleNotificationSettings>,
) -> Option<String> {
    schedule
        .and_then(|schedule| normalize_channel(schedule.slack_channel.as_deref()))
        .or_else(|| normalize_channel(Some(config.slack_channel.as_str())))
}

/// `None` for absent or blank, matching legacy's `normalized_channel`
/// (`notifications.rs:834-838`).
fn normalize_channel(value: Option<&str>) -> Option<String> {
    let trimmed = value.map(str::trim).unwrap_or_default();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Legacy's Slack **capability** gate: a non-empty webhook reference —
/// `!config.slack_webhook_url.trim().is_empty()` at every one of
/// `notifications.rs:264`, `:279`, `:314`. Deliberately **not** also
/// checking `config.slack_enabled` here: `routing::route`'s `Decision`
/// already encodes that half (`domain::notify::routing`'s header, "Policy
/// versus capability"), so [`Self::notify_run_completed`] tests this
/// alongside `decision.sends_slack()` rather than folding the two into one
/// predicate that would re-test `slack_enabled` a second time.
/// [`Self::send_test`]'s `TestSend::Generic` arm has no `Decision` to lean
/// on, so it tests `config.slack_enabled` explicitly alongside this.
///
/// **Fix round 3, Important 1.** `notify_run_completed` did not call this
/// at all before this round — see this module's header for what that let
/// through.
fn slack_capable(config: &NotificationConfig) -> bool {
    !config.slack_webhook_credstore_ref.trim().is_empty()
}

/// The field name every refusal about the Slack webhook reference carries — the
/// `qa_notification_config` column and the `NotificationConfigDto` field, which
/// is what an operator can act on.
const SLACK_REF_FIELD: &str = "slack_webhook_credstore_ref";

/// Refuse a Slack webhook reference the credential store could not resolve.
///
/// # Phase C's final review, Important 1: the column had no check at all
///
/// [`SLACK_REF_FIELD`] is copied verbatim into the oagw proxy *path*
/// (`infra::notify::slack_oagw`'s `webhook_path`), and four separate doc claims
/// in this crate said it "is a credential-store reference, never a URL". Nothing
/// enforced that: `hooks.slack.com/services/T…/B…/XXXX` — a Slack
/// incoming-webhook URL, whose whole path *is* the secret — stored cleanly, and
/// `GET /qa/v1/settings/notifications` handed it back verbatim to any holder of
/// `qa.notification_config/get`. Ruling **R77** forecast exactly this wall:
/// `SecretRef` is `[A-Za-z0-9_-]{1,255}` with colons explicitly prohibited, so a
/// URL-shaped value can never resolve to a secret anyway — it can only be
/// mistaken for one by a human reading the settings page.
///
/// The rule is [`validate_credstore_ref`]'s, unchanged and not re-derived: the
/// JIRA half of this same phase already solved it for
/// `api_token_credstore_ref` and applies it at both the `PUT` and the adapter.
/// This is the same function with this surface's field name.
///
/// An **empty** reference is accepted, which is the one place this differs from
/// the JIRA `PUT`: there is no keep-stored-value convention here, so empty is
/// how a tenant clears the reference (see [`NotifyService::save_config`]), and
/// [`slack_capable`] is what stops an empty one from being sent over.
///
/// # Errors
///
/// [`DomainError::Validation`] naming [`SLACK_REF_FIELD`].
fn validate_slack_ref(reference: &str) -> Result<(), DomainError> {
    if reference.trim().is_empty() {
        return Ok(());
    }
    validate_credstore_ref(SLACK_REF_FIELD, reference)
}

/// Legacy's email **capability** gate: SMTP host, sender and recipients all
/// non-empty — `notifications.rs:323-327`'s three conditions (its fourth,
/// `config.email_enabled`, is [`slack_capable`]'s reason not repeated here
/// either — see that function's doc).
///
/// **Fix round 3, Important 1.**
fn email_capable(config: &NotificationConfig) -> bool {
    !config.email_smtp_host.trim().is_empty()
        && !config.email_from.trim().is_empty()
        && !config.email_recipients.trim().is_empty()
}

/// `run.target`'s plan identity, as a plain string —
/// [`render::RunCompletedRenderContext::plan_id`]'s value. A discovered or
/// single-test plan is identified by its `plan.yaml` path, matching legacy's
/// own plan id; a persisted custom plan has no path at all, so its `id` is
/// the nearest equivalent; a collect enumeration is not a plan run in
/// legacy's sense, so it gets a literal label instead of a fabricated path.
fn plan_id_for_target(target: &qa_runs_sdk::RunTarget) -> String {
    match target {
        qa_runs_sdk::RunTarget::Plan { path, .. } | qa_runs_sdk::RunTarget::Test { path, .. } => {
            path.clone()
        }
        qa_runs_sdk::RunTarget::CustomPlan { id } => id.to_string(),
        qa_runs_sdk::RunTarget::Collect { .. } => "collect".to_owned(),
    }
}

/// Build [`Self::notify_run_completed`]'s render context from the real run
/// [`RunsReader::get_run`] returned and the real rows
/// [`RunsReader::list_run_test_results`] returned (R104).
///
/// `platform` and `product_key` are `None` — this module's header,
/// "R104: `notify_run_completed` resolves a real run", says why that is an
/// honest absence and not a stand-in: `product_key` has no equivalent in
/// this architecture, and `platform_id` is not resolved to a display name
/// here because doing so needs [`crate::domain::ports::EnvironmentReader`],
/// which nothing wires into this service.
fn run_completed_render_context(run: &Run, results: &[RunTestResult]) -> RunCompletedRenderContext {
    RunCompletedRenderContext {
        run_name: run.name.clone(),
        plan_id: plan_id_for_target(&run.target),
        platform: None,
        product_key: None,
        result_statuses: results.iter().map(|row| row.status.clone()).collect(),
    }
}

/// The tenant's notification settings, the audit log, and the send-once
/// path over [`crate::domain::notify::routing`] and
/// [`crate::domain::notify::render`].
///
/// Generic over the repository for
/// [`crate::domain::service::jira::JiraService`]'s reason:
/// [`NotifyRepository`]'s methods are generic over the `DBRunner` they run
/// on, so the trait is not object-safe.
pub struct NotifyService<N: NotifyRepository + Clone + 'static> {
    db: Arc<DbProvider>,
    repo: N,
    policy_enforcer: PolicyEnforcer,
    slack: Arc<dyn SlackClient>,
    mail: Arc<dyn MailClient>,
    /// The qa-runs read [`Self::notify_run_completed`] resolves a run
    /// through (R104) — the same port [`crate::domain::service::reconcile`]
    /// and friends already hold.
    runs: Arc<dyn RunsReader>,
}

impl<N: NotifyRepository + Clone + 'static> NotifyService<N> {
    #[must_use]
    pub fn new(
        db: Arc<DbProvider>,
        repo: N,
        policy_enforcer: PolicyEnforcer,
        slack: Arc<dyn SlackClient>,
        mail: Arc<dyn MailClient>,
        runs: Arc<dyn RunsReader>,
    ) -> Self {
        Self {
            db,
            repo,
            policy_enforcer,
            slack,
            mail,
            runs,
        }
    }

    /// Compile the caller's scope for one settings operation.
    ///
    /// `resource_id` is always `None` — [`resources::NOTIFICATION_CONFIG`]
    /// declares `OWNER_TENANT_ID` only, [`resources::JIRA_CONFIG`]'s reason:
    /// none of `qa_notification_config`, `qa_notification_log` or
    /// `qa_run_notifications` has a row this gear's routes ever address by
    /// id.
    async fn scope(&self, ctx: &SecurityContext, action: &str) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::NOTIFICATION_CONFIG, action, None)
            .await?)
    }

    /// The tenant's notification settings — `GET /qa/v1/settings/notifications`.
    ///
    /// `api_get_notifications` (`manager/src/routes/settings.rs:434-444`).
    /// A tenant that has never saved settings reads
    /// [`NotificationConfig::default`] rather than a 404 or an error — legacy's
    /// `get_or_default` does the same (`services/notifications.rs:85-89`).
    ///
    /// Passes `ctx.subject_tenant_id()` explicitly to
    /// [`NotifyRepository::get_config`], not only the compiled scope — R86,
    /// closed on this repository method by this task (see that method's own
    /// doc): a scope spanning several tenants would otherwise let the
    /// repository's `.one()` return an arbitrary in-scope tenant's settings.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure.
    pub async fn get_config(
        &self,
        ctx: &SecurityContext,
    ) -> Result<NotificationConfig, DomainError> {
        let access = self.scope(ctx, actions::GET).await?;
        let conn = self.db.conn()?;
        Ok(self
            .repo
            .get_config(&conn, &access, ctx.subject_tenant_id())
            .await?
            .unwrap_or_default())
    }

    /// Store the tenant's notification settings —
    /// `PUT /qa/v1/settings/notifications`.
    ///
    /// `api_update_notifications` (`manager/src/routes/settings.rs:447-462`).
    /// A full replace: every field is stored as given, with no defaulting and
    /// no masking on the way back out.
    ///
    /// # `slack_webhook_credstore_ref` is validated — Phase C's final review,
    /// # Important 1
    ///
    /// It is a credential-store reference and **never a URL**, for the reason
    /// the `GET` route's own description gives: possession of a Slack
    /// incoming-webhook URL is itself the authorization to post, so the column
    /// is treated exactly like a JIRA API token's reference. Until that review
    /// nothing in this write path checked anything at all, so the column could
    /// hold the webhook secret and this endpoint's sibling `GET` would hand it
    /// to any holder of `qa.notification_config/get`. [`validate_slack_ref`] is
    /// the check — the JIRA surface's own [`validate_credstore_ref`], with this
    /// field's name — and its doc carries the argument in full.
    ///
    /// # There is still no keep-stored-value convention, and that is the *only*
    /// # way this differs from the JIRA `PUT`
    ///
    /// An empty `slack_webhook_credstore_ref` **clears** the reference rather
    /// than preserving the stored one, where
    /// [`JiraService::save_jira_config`](crate::domain::service::jira::JiraService::save_jira_config)
    /// substitutes the stored value for an empty input. That is a difference in
    /// how the two `PUT`s treat *absence*, not a difference in how secret the
    /// two columns are: neither surface ever masks a reference on read, so a
    /// form here always has the real value to resend, and legacy's sentinel
    /// (`"********"`) — the thing JIRA's convention exists to replace — has no
    /// equivalent on this document at all. Clearing being reachable is why this
    /// endpoint needs no separate "stop using Slack" switch beyond
    /// `slack_enabled`.
    ///
    /// Returns the stored config, [`crate::domain::service::jira::JiraService::save_jira_config`]'s
    /// reason: a caller need not re-read to render the form it just posted.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming `slack_webhook_credstore_ref` for a
    /// non-empty reference the credential store could not resolve.
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure.
    pub async fn save_config(
        &self,
        ctx: &SecurityContext,
        config: NotificationConfig,
    ) -> Result<NotificationConfig, DomainError> {
        let access = self.scope(ctx, actions::UPDATE).await?;
        validate_slack_ref(&config.slack_webhook_credstore_ref)?;
        let conn = self.db.conn()?;
        self.repo
            .save_config(&conn, &access, ctx.subject_tenant_id(), config.clone())
            .await?;
        Ok(config)
    }

    /// The most recent audit entries, newest first —
    /// `GET /qa/v1/settings/notifications/log`.
    ///
    /// `api_get_notification_log` (`manager/src/routes/settings.rs:752-773`):
    /// `limit` defaults to 100 and is clamped to at most 500 — legacy's own
    /// `.unwrap_or(100).min(500)` (`:757-761`).
    ///
    /// Passes `ctx.subject_tenant_id()` explicitly to
    /// [`NotifyRepository::list_log`] — R106, fix round 3: a scope spanning
    /// several tenants must not let this settings page's log read return
    /// another in-scope tenant's rows, the way [`Self::get_config`] was
    /// already pinned for the identical reason (R86).
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure.
    pub async fn list_log(
        &self,
        ctx: &SecurityContext,
        limit: Option<u64>,
    ) -> Result<Vec<NotificationLogEntry>, DomainError> {
        const DEFAULT_LIMIT: u64 = 100;
        const MAX_LIMIT: u64 = 500;

        let access = self.scope(ctx, actions::GET).await?;
        let conn = self.db.conn()?;
        let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
        self.repo
            .list_log(&conn, &access, ctx.subject_tenant_id(), limit)
            .await
    }

    /// Render a scheduled-run preview against a **caller-supplied** config
    /// override — `POST /qa/v1/settings/notifications/preview`.
    ///
    /// `api_preview_notifications` (`manager/src/routes/settings.rs:499-507`),
    /// over `preview_scheduled_run_message` (`notifications.rs:388-404`).
    /// Never claims, sends or logs — `render::preview_scheduled_run` is pure,
    /// and this method calls nothing else that is not.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming `event` when `token` is not one of
    /// [`SLACK_NOTIFICATION_EVENTS`].
    /// [`DomainError::Forbidden`] when the PDP denies.
    pub async fn preview_scheduled_run(
        &self,
        ctx: &SecurityContext,
        config: NotificationConfig,
        token: &str,
    ) -> Result<ScheduledRunPreview, DomainError> {
        // A read-shaped, side-effect-free render over the tenant's own
        // notification-config resource; gated the same as `get_config` since
        // legacy applies no PDP at all and this task's own judgement is that
        // a preview should not be reachable by a caller who could not read
        // the tenant's settings in the first place.
        self.scope(ctx, actions::GET).await?;

        let event_label = scheduled_run_event_label(token)
            .ok_or_else(|| invalid_event_error(token))?
            .to_owned();
        let sample = sample_render_context();
        let rendered = render::preview_scheduled_run(&config, token, &sample)
            .ok_or_else(|| invalid_event_error(token))?;

        Ok(ScheduledRunPreview {
            event: token.to_owned(),
            event_label,
            rendered_message: rendered.rendered_message,
            fallback_text: rendered.fallback_text,
            blocks: rendered.blocks,
        })
    }

    /// Send a test notification — `POST /qa/v1/settings/notifications/test`.
    ///
    /// `api_test_notifications` (`manager/src/routes/settings.rs:465-496`).
    /// **Unlike [`Self::notify_run_completed`], a failure here propagates**:
    /// this is an interactive action an operator is watching, and legacy's
    /// own route surfaces the underlying error rather than swallowing it.
    /// Never claims or logs — legacy's two test-send functions call neither
    /// `reserve_run_notification` nor `log_notification`.
    ///
    /// [`TestSend::Generic`] sends over every channel the **stored** config
    /// enables, using its own fixed text (`"VHP Test Manager: test
    /// notification from Settings page"`, `notifications.rs:364`).
    /// [`TestSend::ScheduledRun`] renders the caller-supplied config override
    /// with [`render::preview_scheduled_run`] and sends the Slack side only.
    ///
    /// **Only the [`TestSend::ScheduledRun`] arm validates the webhook
    /// reference's syntax, and that asymmetry is the point** (Phase C's final
    /// review, Important 1): [`TestSend::Generic`] reads the stored document,
    /// which [`Self::save_config`] has already validated, while
    /// [`TestSend::ScheduledRun`]'s config arrives in the request body and its
    /// reference becomes a path segment of a gateway request this gear makes on
    /// the caller's behalf. See [`validate_slack_ref`].
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] for [`TestSend::ScheduledRun`] when the
    /// override config has Slack disabled or an empty webhook reference
    /// (legacy's own two checks, `notifications.rs:412-417`), when that
    /// reference is one the credential store could not resolve, or when `event`
    /// is not a valid token.
    /// [`DomainError::UnsupportedEgress`] when [`TestSend::Generic`] is asked
    /// to test a channel this deployment cannot send over — **the one place
    /// [`SendOutcome::UnsupportedEgress`] surfaces as an error rather than a
    /// log entry**, because an operator explicitly testing a channel deserves
    /// to be told it does not work, not a silent success.
    /// Whatever [`SlackClient::send`]/[`MailClient::send`] return as `Err`.
    /// [`DomainError::Forbidden`] when the PDP denies.
    pub async fn send_test(
        &self,
        ctx: &SecurityContext,
        request: TestSend,
    ) -> Result<(), DomainError> {
        self.scope(ctx, actions::TEST).await?;

        match request {
            TestSend::Generic => {
                let config = self.get_config(ctx).await?;

                if config.slack_enabled && slack_capable(&config) {
                    let outcome = self
                        .slack
                        .send(
                            ctx,
                            &SlackMessage {
                                webhook_credstore_ref: config.slack_webhook_credstore_ref.clone(),
                                channel: effective_channel(&config, None),
                                text: GENERIC_TEST_MESSAGE.to_owned(),
                                blocks: Vec::new(),
                            },
                        )
                        .await?;
                    if outcome == SendOutcome::UnsupportedEgress {
                        return Err(DomainError::UnsupportedEgress {
                            channel: "slack".to_owned(),
                        });
                    }
                }
                if config.email_enabled && email_capable(&config) {
                    let outcome = self
                        .mail
                        .send(
                            ctx,
                            &MailMessage {
                                smtp_host: config.email_smtp_host.clone(),
                                smtp_port: config.email_smtp_port,
                                from: config.email_from.clone(),
                                recipients: config.email_recipients.clone(),
                                subject: "VHP test notification".to_owned(),
                                body: GENERIC_TEST_MESSAGE.to_owned(),
                            },
                        )
                        .await?;
                    if outcome == SendOutcome::UnsupportedEgress {
                        return Err(DomainError::UnsupportedEgress {
                            channel: "email".to_owned(),
                        });
                    }
                }
                Ok(())
            }
            TestSend::ScheduledRun { config, event } => {
                if !config.slack_enabled {
                    return Err(DomainError::Validation {
                        field: "slack_enabled".to_owned(),
                        message: "Slack notifications must be enabled before sending a test"
                            .to_owned(),
                    });
                }
                if config.slack_webhook_credstore_ref.trim().is_empty() {
                    return Err(DomainError::Validation {
                        field: SLACK_REF_FIELD.to_owned(),
                        message: "Slack webhook reference is required before sending a test"
                            .to_owned(),
                    });
                }
                // Phase C's final review, Important 1. This arm's `config` is
                // **caller-supplied**, not the stored document, and its
                // reference becomes the first segment of the oagw proxy path
                // this gear builds on the caller's behalf
                // (`infra::notify::slack_oagw`'s `webhook_path`). oagw's
                // per-tenant upstream resolution bounds that to the tenant's
                // own registered upstreams, so it is not an open SSRF — but it
                // is a caller-controlled proxy path, and the sibling JIRA
                // surface validates its reference at both the write and the
                // adapter. Same rule, same function, applied to the override.
                validate_slack_ref(&config.slack_webhook_credstore_ref)?;

                let sample = sample_render_context();
                let rendered = render::preview_scheduled_run(&config, &event, &sample)
                    .ok_or_else(|| invalid_event_error(&event))?;

                let outcome = self
                    .slack
                    .send(
                        ctx,
                        &SlackMessage {
                            webhook_credstore_ref: config.slack_webhook_credstore_ref.clone(),
                            channel: effective_channel(&config, None),
                            text: rendered.fallback_text,
                            blocks: rendered.blocks,
                        },
                    )
                    .await?;
                if outcome == SendOutcome::UnsupportedEgress {
                    return Err(DomainError::UnsupportedEgress {
                        channel: "slack".to_owned(),
                    });
                }
                Ok(())
            }
        }
    }

    /// Send (or skip, or fail-and-log) the completion notifications for one
    /// run — the send-once path over [`routing::Event::RunCompleted`].
    ///
    /// Ports legacy's generic (non-templated) alert inside
    /// `notify_run_completed` (`manager/src/services/notifications.rs:180-355`).
    /// See this module's header, "R104: `notify_run_completed` resolves a real
    /// run through `RunsReader`", for the two fields this method leaves `None`
    /// and why that is an honest absence. **That reference used to name a
    /// section called "`notify_run_completed` does not yet resolve a real run"**
    /// (Phase C's final review, cluster B): R104's fix round renamed the section
    /// and inverted its claim, and this one site was missed while `:377` was
    /// updated — so it pointed at a heading that no longer existed and asserted
    /// the opposite of shipped behaviour.
    ///
    /// **Never propagates a send failure** — matching legacy, and unlike
    /// [`Self::send_test`]. Every attempt (sent, skipped, failed or
    /// unsupported) is logged except the one case
    /// [`routing::Decision::slack_skip_is_audited`] says is deliberately
    /// silent (R94a); a failed send releases its claim (R100) before
    /// returning `Ok(())`.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies, and any repository
    /// failure reading the config or claiming a slot — a broken database is
    /// not swallowed the way a broken egress is, because there is nothing to
    /// log it into.
    pub async fn notify_run_completed(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<(), DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        // R104: resolve the real run before anything else. A `RunNotIngested`
        // here is a normal, expected race in this gear's async-ingest
        // architecture, not a corruption — see this module's header. Any
        // other failure (a genuine qa-runs outage) is logged the same way,
        // under the pseudo-channel `"run"`, because no channel decision was
        // ever reached: legacy's own precedent for that shape is
        // `notify_queue_event`'s config-load-failure branch, logged under
        // `"run_queue"` rather than `"slack"`/`"email"`
        // (`notifications.rs:664-683`).
        let run = match self.runs.get_run(ctx, run_id).await {
            Ok(run) => run,
            Err(DomainError::RunNotIngested { .. }) => {
                self.log(
                    tenant_id,
                    Some(run_id),
                    "run",
                    "run_completed",
                    OUTCOME_SKIPPED,
                    "The run has not been ingested yet, or is not visible to this context; nothing to notify with",
                )
                .await;
                return Ok(());
            }
            Err(error) => {
                self.log(
                    tenant_id,
                    Some(run_id),
                    "run",
                    "run_completed",
                    OUTCOME_FAILED,
                    &error.to_string(),
                )
                .await;
                return Ok(());
            }
        };
        let results = match self.runs.list_run_test_results(ctx, run_id).await {
            Ok(results) => results,
            Err(DomainError::RunNotIngested { .. }) => {
                self.log(
                    tenant_id,
                    Some(run_id),
                    "run",
                    "run_completed",
                    OUTCOME_SKIPPED,
                    "The run's results are not available yet; nothing to notify with",
                )
                .await;
                return Ok(());
            }
            Err(error) => {
                self.log(
                    tenant_id,
                    Some(run_id),
                    "run",
                    "run_completed",
                    OUTCOME_FAILED,
                    &error.to_string(),
                )
                .await;
                return Ok(());
            }
        };
        let render_ctx = run_completed_render_context(&run, &results);

        // R105: resolve the run's real schedule settings. `None` here — no
        // `schedule_id`, or one that could not be resolved — is not a
        // failure; it is legacy's own "not a scheduled run" answer, ported
        // by `no_scheduled_notifications` — see this module's header, "What
        // an absent schedule means".
        let schedule = match run.schedule_id {
            None => no_scheduled_notifications(),
            Some(schedule_id) => match self.runs.get_schedule_notifications(ctx, schedule_id).await
            {
                Ok(settings) => settings.unwrap_or_else(no_scheduled_notifications),
                Err(error) => {
                    self.log(
                        tenant_id,
                        Some(run_id),
                        "run",
                        "run_completed",
                        OUTCOME_FAILED,
                        &error.to_string(),
                    )
                    .await;
                    return Ok(());
                }
            },
        };

        let config = self.get_config(ctx).await?;
        let event = Event::RunCompleted;
        let decision = routing::route(&config, &event, &schedule);
        // The event's dedupe-key spelling, shared by every claim and log
        // write below so a claim and its log row always agree about which
        // event they are about. The `kind` argument does not affect this
        // half of the key (`Event::dedupe_token` does not vary by kind), so
        // either channel's kind produces the same token.
        let event_token =
            routing::dedupe_key(run_id, NotificationKind::RunCompletedSlack, &event).event;

        if decision.sends_slack() && slack_capable(&config) {
            self.send_run_completed_channel(RunCompletedChannelSend {
                ctx,
                tenant_id,
                run_id,
                event: &event,
                kind: NotificationKind::RunCompletedSlack,
                channel: RunCompletedChannel::Slack {
                    webhook_credstore_ref: config.slack_webhook_credstore_ref.clone(),
                    channel: effective_channel(&config, Some(&schedule)),
                },
                render_ctx: &render_ctx,
            })
            .await?;
        } else if !decision.sends_slack() && decision.slack_skip_is_audited() {
            self.log(
                tenant_id,
                Some(run_id),
                "slack",
                &event_token,
                OUTCOME_SKIPPED,
                "Routing decided this event does not send over Slack",
            )
            .await;
        }
        // Two silent cases, both matching legacy exactly rather than one:
        // an unaudited routing skip (R94a — `!decision.sends_slack() &&
        // !decision.slack_skip_is_audited()`), and routing wanting to send
        // while the webhook reference is empty (`decision.sends_slack() &&
        // !slack_capable(&config)`) — legacy's three Slack arms all require
        // `config.slack_enabled && !webhook.is_empty()`
        // (`notifications.rs:264,279,314`), so an empty webhook reaches none
        // of them and logs nothing, independent of what `slack_skip_is_audited`
        // would otherwise say (Important 1, fix round 3).

        if decision.sends_email() && email_capable(&config) {
            self.send_run_completed_channel(RunCompletedChannelSend {
                ctx,
                tenant_id,
                run_id,
                event: &event,
                kind: NotificationKind::RunCompletedEmail,
                channel: RunCompletedChannel::Mail {
                    smtp_host: config.email_smtp_host.clone(),
                    smtp_port: config.email_smtp_port,
                    from: config.email_from.clone(),
                    recipients: config.email_recipients.clone(),
                },
                render_ctx: &render_ctx,
            })
            .await?;
        }
        // No `else` and no audited-skip case for email ever, capability
        // gate included: legacy's email branch has no `else` at all — see
        // `domain::notify::render`'s "Email rendering" section and this
        // module's own header for the citation.

        Ok(())
    }

    /// Claim, render, send and log one channel of [`Self::notify_run_completed`].
    ///
    /// Shared by the Slack and mail arms so the claim/send/log/release
    /// sequence exists in exactly one place; `job.channel` carries the one
    /// thing that differs. Bundled into [`RunCompletedChannelSend`] rather
    /// than seven bare parameters purely because `clippy::too_many_arguments`
    /// (7) says so once R104 added `render_ctx` as an eighth — every field
    /// here already existed as a parameter before that.
    ///
    /// # Authorized under `actions::UPDATE`, not `actions::GET` — Phase C's
    /// # final review, Important 2
    ///
    /// This method **writes**: [`NotifyRepository::claim_notification`] is an
    /// `INSERT` into `qa_run_notifications` and
    /// [`NotifyRepository::release_notification`] is a `DELETE` from it, and
    /// [`Self::log`] appends an audit row. Task 38 compiled the scope for all
    /// three under [`actions::GET`], so a deployment granting *read-only*
    /// notification settings (`qa.notification_config/get`) authorized
    /// inserting and deleting send-once claim rows.
    ///
    /// The sibling case is
    /// [`JiraService::resolve_bug`](crate::domain::service::jira::JiraService::resolve_bug),
    /// which Task 35 got right and documented: the same resource its reads
    /// compile a scope over, for a write rather than a read on it. `UPDATE`
    /// rather than [`actions::CREATE`] for two reasons — the path both inserts
    /// and deletes, so `CREATE` would describe half of it, and
    /// [`Self::save_config`] already spends `qa.notification_config/update` on
    /// "may write this tenant's notification state", so no operator has to
    /// learn a fourth verb for a resource with three.
    ///
    /// **Reachability today is nil** — `notify_run_completed` still has no
    /// producer (release-gate item R111) — which is why this was Important
    /// rather than Critical. It is fixed now anyway, on Task 39's own ruling: a
    /// port signature defect found before wiring is a signature change; found
    /// after wiring it is an incident.
    ///
    /// The action change does not touch the six-outcome claim lifecycle below.
    /// It changes which grant authorizes the path, not how many arms it has.
    async fn send_run_completed_channel(
        &self,
        job: RunCompletedChannelSend<'_>,
    ) -> Result<(), DomainError> {
        let RunCompletedChannelSend {
            ctx,
            tenant_id,
            run_id,
            event,
            kind,
            channel,
            render_ctx,
        } = job;
        let access = self.scope(ctx, actions::UPDATE).await?;
        let conn = self.db.conn()?;
        let key = routing::dedupe_key(run_id, kind, event);
        let claim = key.clone().into_claim(OffsetDateTime::now_utc());

        let won = self
            .repo
            .claim_notification(&conn, &access, tenant_id, claim)
            .await?;
        if !won {
            self.log(
                tenant_id,
                Some(run_id),
                channel.name(),
                &key.event,
                OUTCOME_SKIPPED,
                "Already sent (duplicate reservation)",
            )
            .await;
            return Ok(());
        }

        let rendered = render::render_run_completed(render_ctx);

        let outcome = channel.send(ctx, &rendered, self).await;
        match outcome {
            Ok(SendOutcome::Sent) => {
                self.log(
                    tenant_id,
                    Some(run_id),
                    channel.name(),
                    &key.event,
                    OUTCOME_SENT,
                    "Run-completed notification",
                )
                .await;
            }
            Ok(SendOutcome::UnsupportedEgress) => {
                // Fix round 3, Important 2: nothing was sent, so the claim
                // must not survive either — the same "a claim that was
                // never sent should not be held forever" reasoning as the
                // `Err` arm below. Without this, the slot stays taken
                // permanently the moment a working adapter replaces the
                // inert one, which is R100's own failure mode turned inside
                // out.
                self.release_claim_ignoring_failure(&conn, &access, tenant_id, run_id, &key)
                    .await;
                self.log(
                    tenant_id,
                    Some(run_id),
                    channel.name(),
                    &key.event,
                    outcome_str(SendOutcome::UnsupportedEgress),
                    "This deployment has no adapter for this channel (D10)",
                )
                .await;
            }
            Err(error) => {
                // R100: release so a retry is not permanently suppressed.
                // Fix round 3, Important 3: the release's own failure is
                // swallowed rather than propagated — legacy's identical
                // `let _ = self.release_run_notification(...)`
                // (`notifications.rs:515-517`), then logs, then returns. A
                // bare `?` here would skip the `OUTCOME_FAILED` log write
                // immediately below for the one case it exists to record —
                // a send failure — breaking both "never propagates" and
                // "every attempt is logged" in one step.
                self.release_claim_ignoring_failure(&conn, &access, tenant_id, run_id, &key)
                    .await;
                self.log(
                    tenant_id,
                    Some(run_id),
                    channel.name(),
                    &key.event,
                    OUTCOME_FAILED,
                    &error.to_string(),
                )
                .await;
            }
        }
        Ok(())
    }

    /// [`NotifyRepository::release_notification`], with its own failure
    /// swallowed into a `tracing::warn!` — legacy's `let _ =
    /// self.release_run_notification(...)` (`notifications.rs:515-517`).
    /// Shared by both of [`Self::send_run_completed_channel`]'s
    /// claim-was-never-sent arms (`UnsupportedEgress` and `Err`, fix round 3
    /// Important 2/3) so the swallow-and-warn behaviour exists in one place.
    async fn release_claim_ignoring_failure(
        &self,
        conn: &toolkit_db::secure::DbConn<'_>,
        access: &AccessScope,
        tenant_id: Uuid,
        run_id: Uuid,
        key: &routing::DedupeKey,
    ) {
        if let Err(error) = self
            .repo
            .release_notification(
                conn,
                access,
                tenant_id,
                run_id,
                key.kind.as_str(),
                key.event.as_str(),
            )
            .await
        {
            tracing::warn!(
                %error,
                %run_id,
                kind = key.kind.as_str(),
                event = key.event.as_str(),
                "failed to release a notification claim that was never sent; a retry will stay suppressed until this is fixed",
            );
        }
    }

    /// Write one audit entry, swallowing a write failure into a `tracing::warn!`
    /// — legacy's own `log_notification` (`notifications.rs:594-619`) does the
    /// same, which this module's callers rely on: an audit-log outage must not
    /// also break the send path it is auditing.
    async fn log(
        &self,
        tenant_id: Uuid,
        run_id: Option<Uuid>,
        channel: &str,
        event_type: &str,
        outcome: &str,
        detail: &str,
    ) {
        // `AccessScope::for_tenant` rather than a caller-derived scope: the
        // write is always to this same tenant's own log, regardless of which
        // action compiled the scope that authorized the operation that is
        // being audited.
        let access = AccessScope::for_tenant(tenant_id);
        let conn = match self.db.conn() {
            Ok(conn) => conn,
            Err(error) => {
                tracing::warn!(%error, "failed to open a connection to write the notification log");
                return;
            }
        };
        if let Err(error) = self
            .repo
            .append_log(
                &conn,
                &access,
                tenant_id,
                NewLogEntry {
                    run_id,
                    channel: channel.to_owned(),
                    event_type: event_type.to_owned(),
                    outcome: outcome.to_owned(),
                    detail: detail.to_owned(),
                },
            )
            .await
        {
            tracing::warn!(%error, "failed to write the notification log");
        }
    }
}

/// Everything one call to [`NotifyService::send_run_completed_channel`]
/// needs — see that method's own doc for why this is a struct rather than
/// seven parameters.
struct RunCompletedChannelSend<'a> {
    ctx: &'a SecurityContext,
    tenant_id: Uuid,
    run_id: Uuid,
    event: &'a Event,
    kind: NotificationKind,
    channel: RunCompletedChannel,
    render_ctx: &'a RunCompletedRenderContext,
}

/// One channel of [`NotifyService::send_run_completed_channel`] — which port
/// to call and what to build the outgoing message from.
enum RunCompletedChannel {
    Slack {
        webhook_credstore_ref: String,
        channel: Option<String>,
    },
    Mail {
        smtp_host: String,
        smtp_port: u16,
        from: String,
        recipients: String,
    },
}

impl RunCompletedChannel {
    fn name(&self) -> &'static str {
        match self {
            Self::Slack { .. } => "slack",
            Self::Mail { .. } => "email",
        }
    }

    async fn send<N: NotifyRepository + Clone + 'static>(
        &self,
        ctx: &SecurityContext,
        rendered: &render::RenderedRunCompletedMessage,
        service: &NotifyService<N>,
    ) -> Result<SendOutcome, DomainError> {
        match self {
            Self::Slack {
                webhook_credstore_ref,
                channel,
            } => {
                service
                    .slack
                    .send(
                        ctx,
                        &SlackMessage {
                            webhook_credstore_ref: webhook_credstore_ref.clone(),
                            channel: channel.clone(),
                            text: rendered.text.clone(),
                            blocks: Vec::new(),
                        },
                    )
                    .await
            }
            Self::Mail {
                smtp_host,
                smtp_port,
                from,
                recipients,
            } => {
                service
                    .mail
                    .send(
                        ctx,
                        &MailMessage {
                            smtp_host: smtp_host.clone(),
                            smtp_port: *smtp_port,
                            from: from.clone(),
                            recipients: recipients.clone(),
                            subject: rendered.email_subject.clone(),
                            body: rendered.text.clone(),
                        },
                    )
                    .await
            }
        }
    }
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod notify_tests;
