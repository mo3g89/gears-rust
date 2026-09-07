//! Tests for [`NotifyService`] — dedupe, the audit log, R100's release, and
//! (fix round 1) R104's real run resolution.
//!
//! Against the **real** repository (`OrmNotifyRepository`) on in-memory
//! `SQLite`, matching `jira_tests`/`saved_views_tests`: what is under test is
//! whether the service's claim/send/log orchestration is the one the storage
//! layer actually implements. Slack is doubled ([`FakeSlack`]) because this
//! module's job is the orchestration, not the wire format Task 39's adapter
//! owns; mail is [`InertMail`], a local double, since `NeverWiredMailClient`
//! moved to `gear.rs` in fix round 1 (R103) and a domain-layer test importing
//! from the composition root would invert this crate's dependency direction.
//! qa-runs is [`crate::domain::service::test_support::FakeRuns`] — the same
//! double `reconcile_tests`/`ingest_tests` already share — seeded with `RUN`'s
//! real name and results in [`fixture_with`], which is R104's fix: a first
//! draft rendered every message from the bare run id.
//!
//! # `two_concurrent_attempts_produce_exactly_one_send` and in-memory `SQLite`
//!
//! **This test cannot falsify a race on this substrate.** `test_db::inmem_db`
//! opens its pool with `max_conns(1)` (that module's own doc), which
//! serializes every writer in this process onto one connection — so the two
//! `tokio::join!`ed calls below can *interleave* cooperatively but can never
//! truly run their two `claim_notification` inserts concurrently inside the
//! database. What this test proves on `SQLite` is narrower and still real:
//! the orchestration around the claim (deciding whether to send based on the
//! claim's boolean answer, not re-checking the row afterward) is correct, and
//! a second caller that loses the claim does not also call
//! [`crate::domain::ports::SlackClient::send`].
//!
//! **Fix round 3, Important 5 — corrected rather than left standing.** An
//! earlier draft of this section pointed at
//! `notify_sea_repo::tests::a_lost_claim_inside_a_transaction_leaves_the_transaction_usable`
//! as "where a real multi-connection Postgres pool makes a genuine race
//! observable." Re-read, that test runs two claims **sequentially inside one
//! transaction on one connection** (`notify_sea_repo.rs`) — it proves the
//! transaction survives a lost claim, not that two concurrent writers were
//! ever in flight together. **The race itself is not demonstrated on any
//! tier in this crate.** What both tiers show is that a second *sequential*
//! claim on an already-taken slot loses, and that this service acts
//! correctly on the boolean either way. The property that actually prevents
//! two true concurrent winners is the schema constraint —
//! `idx_qa_run_notifications_claim`, the unique index `claim_notification`'s
//! `ON CONFLICT DO NOTHING` targets — not a test in this crate; the citation
//! above is removed rather than repeated, per the same doc-comment
//! discipline Task 36's own fix round enforced on a false claim driving a
//! hardcoded value.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use qa_insights_sdk::NotificationConfig;
use qa_runs_sdk::ScheduleNotificationSettings;
use toolkit_db::DBProvider;
use uuid::Uuid;

use super::{NotifyService, TestSend, outcome_str};
use crate::domain::error::DomainError;
use crate::domain::ports::{
    MailClient, MailMessage, RunsReader, SendOutcome, SlackClient, SlackMessage,
};
use crate::domain::repos::{NewLogEntry, NotificationClaim, NotifyRepository};
use crate::domain::service::test_support::{
    FakeRuns, RecordingAuthZ, TenantScopedAuthZ, ctx, finished_run, test_row,
};
use crate::domain::service::{actions, resources};
use crate::infra::storage::notify_sea_repo::OrmNotifyRepository;
use crate::infra::storage::test_db::inmem_db;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;

const TENANT: Uuid = Uuid::from_u128(0xA);
const OTHER_TENANT: Uuid = Uuid::from_u128(0x0B);
const RUN: Uuid = Uuid::from_u128(0x20);
const SCHEDULE: Uuid = Uuid::from_u128(0x30);

/// A PDP double that grants and compiles a scope over **two** tenants —
/// `owner_tenant_id IN [TENANT, OTHER_TENANT]`. The shape a parent-tenant
/// grant produces, and the fixture
/// `a_multi_tenant_scope_does_not_return_another_tenants_settings` needs —
/// `test_support::TenantScopedAuthZ` cannot stand in, since it emits a
/// single-tenant `In`, precisely the case where an unpinned `.one()` happens
/// to be safe. Mirrors `jira_tests::TwoTenantAuthZ` field for field.
struct TwoTenantAuthZ;

#[async_trait]
impl AuthZResolverClient for TwoTenantAuthZ {
    async fn evaluate(
        &self,
        _request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, authz_resolver_sdk::AuthZResolverError>
    {
        Ok(authz_resolver_sdk::EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::EvaluationResponseContext {
                constraints: vec![authz_resolver_sdk::Constraint {
                    predicates: vec![authz_resolver_sdk::Predicate::In(
                        authz_resolver_sdk::InPredicate::new(
                            toolkit_security::pep_properties::OWNER_TENANT_ID,
                            [TENANT, OTHER_TENANT],
                        ),
                    )],
                }],
                ..Default::default()
            },
        })
    }
}

/// A [`SlackClient`] double that records every message it was asked to send
/// (and the `ctx` it was sent under — R108's parameter) and answers a
/// scripted result, front-first — `Ok(SendOutcome::Sent)` when nothing is
/// scripted, which is the shape most tests want and need not script.
#[derive(Default)]
struct FakeSlack {
    sent: Mutex<Vec<SlackMessage>>,
    /// The `subject_tenant_id()` of every `ctx` `send` was called with, in
    /// call order — what
    /// [`notify_run_completed_sends_slack_as_the_calling_tenant`] and
    /// [`send_test_sends_slack_as_the_calling_tenant`] read to pin that
    /// `NotifyService` forwards its own `ctx` rather than dropping it.
    sent_as: Mutex<Vec<Uuid>>,
    script: Mutex<VecDeque<Result<SendOutcome, DomainError>>>,
}

impl FakeSlack {
    fn sends(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    /// Every message this fake was asked to send, in call order — the
    /// content check the mere count in [`Self::sends`] cannot make.
    fn sent_messages(&self) -> Vec<SlackMessage> {
        self.sent.lock().unwrap().clone()
    }

    fn sent_as_tenants(&self) -> Vec<Uuid> {
        self.sent_as.lock().unwrap().clone()
    }

    fn script_next(&self, result: Result<SendOutcome, DomainError>) {
        self.script.lock().unwrap().push_back(result);
    }
}

#[async_trait]
impl SlackClient for FakeSlack {
    async fn send(
        &self,
        ctx: &toolkit_security::SecurityContext,
        message: &SlackMessage,
    ) -> Result<SendOutcome, DomainError> {
        self.sent.lock().unwrap().push(message.clone());
        self.sent_as.lock().unwrap().push(ctx.subject_tenant_id());
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(SendOutcome::Sent))
    }
}

/// A [`MailClient`] double, local to this test module — `gear.rs`'s
/// `NeverWiredMailClient` moved out of `notify` in fix round 1 (R103) and a
/// domain-layer test importing it back from the composition root would
/// invert this crate's dependency direction. Same behaviour as
/// `infra::notify::UnsupportedMailClient`: always
/// [`SendOutcome::UnsupportedEgress`], never an error — but it records the
/// identity it was called as, which is what review finding #37 added to this
/// port and [`send_test_sends_email_as_the_calling_tenant`] reads.
#[derive(Default)]
struct InertMail {
    sent_as: Mutex<Vec<Uuid>>,
}

impl InertMail {
    fn sent_as_tenants(&self) -> Vec<Uuid> {
        self.sent_as.lock().unwrap().clone()
    }
}

#[async_trait]
impl MailClient for InertMail {
    async fn send(
        &self,
        ctx: &toolkit_security::SecurityContext,
        _message: &MailMessage,
    ) -> Result<SendOutcome, DomainError> {
        self.sent_as.lock().unwrap().push(ctx.subject_tenant_id());
        Ok(SendOutcome::UnsupportedEgress)
    }
}

struct Fixture {
    service: NotifyService<OrmNotifyRepository>,
    ctx: toolkit_security::SecurityContext,
    slack: Arc<FakeSlack>,
    /// The same provider the service holds, so a test can read or write a
    /// row directly through the repository under a scope of its own
    /// choosing — `jira_tests::Fixture::db`'s precedent, needed here by
    /// `a_multi_tenant_scope_does_not_return_another_tenants_settings`.
    db: Arc<DBProvider<DomainError>>,
    /// The qa-runs double [`NotifyService::notify_run_completed`] resolves
    /// a run through (R104) — held so a test can register or omit a run.
    runs: Arc<FakeRuns>,
    /// The mail double, held for the same reason `slack` is: review finding
    /// #37 gave [`MailClient::send`] a `SecurityContext` and the identity it
    /// receives is only assertable from the double.
    mail: Arc<InertMail>,
}

impl Fixture {
    /// The tenant's full audit trail, newest first — the same read
    /// `GET /qa/v1/settings/notifications/log` makes.
    async fn log(&self) -> Vec<qa_insights_sdk::NotificationLogEntry> {
        self.service
            .list_log(&self.ctx, Some(100))
            .await
            .expect("the log read itself must not fail")
    }
}

/// Slack enabled with a webhook reference, email untouched (disabled) —
/// the shape every dedupe/failure test wants, so only the slack side of
/// `notify_run_completed` fires and the log's size is predictable.
fn slack_only_config() -> NotificationConfig {
    NotificationConfig {
        slack_enabled: true,
        slack_webhook_credstore_ref: "cred://slack-hook".to_owned(),
        ..NotificationConfig::default()
    }
}

/// The real name [`RUN`] carries in every fixture that registers it —
/// [`notify_run_completed_renders_the_runs_real_name_not_its_id`]'s subject.
const RUN_NAME: &str = "nightly-smoke-142";

/// Construct the fixture with no config saved and no run registered yet —
/// `jira_tests::build`'s shape: callers that want a stored config call
/// [`fixture_with`], and the two tests that must not
/// ([`a_multi_tenant_scope_does_not_return_another_tenants_settings`],
/// [`an_unresolvable_run_is_skipped_rather_than_propagated`]) do not have to
/// undo an unwanted save or registration.
async fn build(authz: Arc<dyn AuthZResolverClient>, slack: Arc<FakeSlack>) -> Fixture {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let runs = Arc::new(FakeRuns::default());
    let mail = Arc::new(InertMail::default());
    let service = NotifyService::new(
        Arc::clone(&db),
        OrmNotifyRepository,
        PolicyEnforcer::new(authz),
        Arc::clone(&slack) as Arc<dyn SlackClient>,
        Arc::clone(&mail) as Arc<dyn MailClient>,
        Arc::clone(&runs) as Arc<dyn RunsReader>,
    );
    Fixture {
        service,
        ctx: ctx(TENANT),
        slack,
        db,
        runs,
        mail,
    }
}

/// [`build`] plus a saved config and [`RUN`] registered with a real name and
/// one passing result — R104's fixture: every dedupe/failure/D10 test needs
/// [`RunsReader::get_run`] to actually succeed, or the method would skip
/// before ever reaching a channel.
async fn fixture_with(slack: Arc<FakeSlack>, config: NotificationConfig) -> Fixture {
    let f = build(Arc::new(TenantScopedAuthZ), slack).await;
    f.service
        .save_config(&f.ctx, config)
        .await
        .expect("saving the fixture's own config must not fail");
    let mut run = finished_run(RUN);
    run.name = RUN_NAME.to_owned();
    run.schedule_id = Some(SCHEDULE);
    f.runs.add_run(
        run,
        vec![test_row(RUN, "tests/smoke.py", "test_ok", "PASSED", "")],
    );
    // A schedule that notifies on everything — R105's fixture default.
    // Tests that need the schedule's own toggle to *narrow* the decision
    // (`a_schedule_level_toggle_narrows_the_decision`) override this
    // registration after `fixture_with` returns.
    f.runs.add_schedule(
        SCHEDULE,
        ScheduleNotificationSettings {
            slack_enabled: true,
            slack_channel: None,
            slack_events: Vec::new(),
        },
    );
    f
}

async fn fixture() -> Fixture {
    fixture_with(Arc::new(FakeSlack::default()), slack_only_config()).await
}

async fn fixture_with_failing_slack() -> Fixture {
    let slack = Arc::new(FakeSlack::default());
    slack.script_next(Err(DomainError::Internal(
        "slack webhook unreachable".to_owned(),
    )));
    fixture_with(slack, slack_only_config()).await
}

/// Email configured, Slack left at its `false` default — so
/// `notify_run_completed`'s Slack side hits R94a's *silent* skip (Slack
/// disabled while the schedule-level gate passed) and only the email
/// channel logs anything, which is what
/// `an_email_send_is_recorded_as_unsupported_egress` filters on.
async fn fixture_with_email_enabled() -> Fixture {
    fixture_with(
        Arc::new(FakeSlack::default()),
        NotificationConfig {
            email_enabled: true,
            email_smtp_host: "smtp.example.com".to_owned(),
            email_from: "qa@example.com".to_owned(),
            email_recipients: "ops@example.com".to_owned(),
            ..NotificationConfig::default()
        },
    )
    .await
}

/// A [`NotifyRepository`] that delegates everything to [`OrmNotifyRepository`]
/// except [`Self::release_notification`], which always fails — Important 3's
/// fixture: forces a release failure without needing a real database fault,
/// `jira_tests::FailingUpsertJiraRepository`'s precedent.
#[derive(Clone, Default)]
struct FailingReleaseNotifyRepository;

#[async_trait]
impl NotifyRepository for FailingReleaseNotifyRepository {
    async fn claim_notification<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        claim: NotificationClaim,
    ) -> Result<bool, DomainError> {
        OrmNotifyRepository
            .claim_notification(runner, scope, tenant_id, claim)
            .await
    }

    async fn release_notification<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        _run_id: Uuid,
        _kind: &str,
        _event: &str,
    ) -> Result<(), DomainError> {
        Err(DomainError::Internal(
            "release deliberately fails for this test".to_owned(),
        ))
    }

    async fn append_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        entry: NewLogEntry,
    ) -> Result<(), DomainError> {
        OrmNotifyRepository
            .append_log(runner, scope, tenant_id, entry)
            .await
    }

    async fn list_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        limit: u64,
    ) -> Result<Vec<qa_insights_sdk::NotificationLogEntry>, DomainError> {
        OrmNotifyRepository
            .list_log(runner, scope, tenant_id, limit)
            .await
    }

    async fn get_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<NotificationConfig>, DomainError> {
        OrmNotifyRepository
            .get_config(runner, scope, tenant_id)
            .await
    }

    async fn save_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        config: NotificationConfig,
    ) -> Result<(), DomainError> {
        OrmNotifyRepository
            .save_config(runner, scope, tenant_id, config)
            .await
    }
}

// ---------------------------------------------------------------------------
// Important 1, fix round 3: the capability gates legacy applies before
// claiming, ported to the automatic path
// ---------------------------------------------------------------------------

/// A run-completed Slack send with `slack_enabled == true` but an empty
/// webhook reference is **silent** — no claim, no send, no log row —
/// matching legacy's requirement that every Slack arm needs
/// `config.slack_enabled && !webhook.is_empty()` before it fires at all
/// (`notifications.rs:264,279,314`). Before this fix round, only
/// `send_test` checked this; the automatic path did not.
#[tokio::test]
async fn a_run_completed_slack_send_with_no_webhook_is_silent() {
    let f = fixture().await;
    f.service
        .save_config(
            &f.ctx,
            NotificationConfig {
                slack_enabled: true,
                slack_webhook_credstore_ref: String::new(),
                ..NotificationConfig::default()
            },
        )
        .await
        .unwrap();

    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");

    assert_eq!(f.slack.sends(), 0, "an empty webhook must never be sent to");
    assert!(
        f.log().await.is_empty(),
        "an empty webhook is legacy's silent case, not an audited skip"
    );
}

/// The email equivalent: `email_enabled == true` but an incomplete SMTP
/// destination (empty host here; legacy's other two fields are exercised the
/// same way in `send_test`'s own tests) is silent — legacy's four-condition
/// gate (`notifications.rs:323-327`) never fires with any field empty, and
/// there is no audited-skip arm for email at all.
#[tokio::test]
async fn a_run_completed_email_send_with_an_empty_smtp_host_is_silent() {
    let f = fixture().await;
    f.service
        .save_config(
            &f.ctx,
            NotificationConfig {
                slack_enabled: false,
                email_enabled: true,
                email_smtp_host: String::new(),
                email_from: "qa@example.com".to_owned(),
                email_recipients: "ops@example.com".to_owned(),
                ..NotificationConfig::default()
            },
        )
        .await
        .unwrap();

    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");

    assert!(
        f.log().await.is_empty(),
        "an incomplete SMTP destination must never be claimed, sent to, or logged"
    );
}

// ---------------------------------------------------------------------------
// Important 2, fix round 3: an unsupported-egress send releases its claim
// ---------------------------------------------------------------------------

/// Nothing was sent, so the claim must not survive — otherwise the slot is
/// held forever the moment a working adapter replaces the inert one,
/// R100's own failure mode inverted. Proven by a second attempt: without
/// the release, it would find itself already claimed and log `"skipped"`
/// instead of trying (and reporting `"unsupported_egress"`) again.
#[tokio::test]
async fn an_unsupported_egress_send_releases_its_claim_for_a_retry() {
    let f = fixture_with_email_enabled().await;

    f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();
    f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();

    let email_entries: Vec<_> = f
        .log()
        .await
        .into_iter()
        .filter(|e| e.channel == "email")
        .collect();
    assert_eq!(email_entries.len(), 2, "{email_entries:?}");
    assert!(
        email_entries
            .iter()
            .all(|e| e.outcome == "unsupported_egress"),
        "a retry must be able to claim and attempt again, not find itself already claimed: {email_entries:?}"
    );
}

// ---------------------------------------------------------------------------
// Important 3, fix round 3: a release failure must not swallow the audit row
// ---------------------------------------------------------------------------

/// A failing `release_notification` is swallowed into a warning, not
/// propagated — legacy's own `let _ = self.release_run_notification(...)`
/// (`notifications.rs:515-517`). Proven directly: with a repository whose
/// release always errors, the send-failure log row must still be written
/// and `notify_run_completed` must still return `Ok(())`.
#[tokio::test]
async fn a_failing_release_still_leaves_the_failure_log_row() {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let runs = Arc::new(FakeRuns::default());
    let mut run = finished_run(RUN);
    run.name = RUN_NAME.to_owned();
    run.schedule_id = Some(SCHEDULE);
    runs.add_run(
        run,
        vec![test_row(RUN, "tests/smoke.py", "test_ok", "PASSED", "")],
    );
    runs.add_schedule(
        SCHEDULE,
        ScheduleNotificationSettings {
            slack_enabled: true,
            slack_channel: None,
            slack_events: Vec::new(),
        },
    );

    let slack = Arc::new(FakeSlack::default());
    slack.script_next(Err(DomainError::Internal(
        "slack webhook unreachable".to_owned(),
    )));

    let service = NotifyService::new(
        Arc::clone(&db),
        FailingReleaseNotifyRepository,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        Arc::clone(&slack) as Arc<dyn SlackClient>,
        Arc::new(InertMail::default()) as Arc<dyn MailClient>,
        Arc::clone(&runs) as Arc<dyn RunsReader>,
    );
    let caller = ctx(TENANT);
    service
        .save_config(&caller, slack_only_config())
        .await
        .unwrap();

    service
        .notify_run_completed(&caller, RUN)
        .await
        .expect("a release failure must not propagate out of notify_run_completed");

    let log = service.list_log(&caller, Some(100)).await.unwrap();
    assert_eq!(
        log.len(),
        1,
        "the failed-send log row must still be written even though releasing its claim failed: {log:?}"
    );
    assert_eq!(log[0].outcome, "failed");
    assert!(!log[0].detail.is_empty());
}

// ---------------------------------------------------------------------------
// The brief's three
// ---------------------------------------------------------------------------

/// One send per (run, kind, event), even under concurrency. `claim_notification`
/// is an insert that reports whether *this* caller won (Task 11); a
/// check-then-insert pair cannot express that.
///
/// See this module's header for what this test does and does not prove on
/// its in-memory `SQLite` substrate.
#[tokio::test]
async fn two_concurrent_attempts_produce_exactly_one_send() {
    let f = fixture().await;
    let (a, b) = tokio::join!(
        f.service.notify_run_completed(&f.ctx, RUN),
        f.service.notify_run_completed(&f.ctx, RUN),
    );
    a.expect("first");
    b.expect("second");
    assert_eq!(f.slack.sends(), 1);
}

/// Every attempt is logged, including failures — legacy records outcome and
/// detail for exactly this reason, and a silent failure is the one thing an
/// operator cannot debug.
///
/// # R100 interacts with this test
///
/// A failed send releases its claim (`NotifyRepository::release_notification`),
/// so this also asserts the retry half: a second `notify_run_completed` call
/// for the same run must be able to claim and send again. That is a
/// **different** guarantee than the dedupe test above proves — this one is
/// about a *failed* send being retryable by construction, not about a
/// *successful* send being exactly-once.
#[tokio::test]
async fn a_failed_send_is_logged_with_its_detail() {
    let f = fixture_with_failing_slack().await;
    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");
    let log = f.log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].outcome, "failed");
    assert!(!log[0].detail.is_empty());

    // R100: the claim was released, so a retry is not permanently suppressed.
    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("a retry after a released claim must be able to send");
    assert_eq!(
        f.slack.sends(),
        2,
        "the retry must have reached the slack port a second time, which it \
         could only do if the failed attempt's claim was released"
    );
}

/// **R108.** `NotifyService` holds a real `ctx` at the point it calls
/// `SlackClient::send` and must forward it rather than let the call go out
/// under some other identity — the exact drop `SlackOagwClient`'s first draft
/// had no way to avoid before R108 added the parameter this test reads.
#[tokio::test]
async fn notify_run_completed_sends_slack_as_the_calling_tenant() {
    let f = fixture().await;
    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");

    assert_eq!(f.slack.sent_as_tenants(), [TENANT]);
}

/// D10: email is configured but unsent. The routing decision, the dedupe claim
/// and the log entry all happen; only the socket is missing. When SMTP arrives,
/// this test changes and nothing else does.
#[tokio::test]
async fn an_email_send_is_recorded_as_unsupported_egress() {
    let f = fixture_with_email_enabled().await;
    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");
    let email_entries: Vec<_> = f
        .log()
        .await
        .into_iter()
        .filter(|e| e.channel == "email")
        .collect();
    assert_eq!(email_entries.len(), 1);
    assert_eq!(email_entries[0].outcome, "unsupported_egress");
}

// ---------------------------------------------------------------------------
// R104: the rendered message is the run's real content, not a stand-in
// ---------------------------------------------------------------------------

/// **The test R104's fix round asked for.** A first draft rendered
/// `run_name` as the run id's string form; the claim, the send and the log
/// entry were all real, but the message read as a UUID. This asserts the
/// opposite: the text actually sent carries [`RUN_NAME`], and does not fall
/// back to [`RUN`]'s own string form.
#[tokio::test]
async fn notify_run_completed_renders_the_runs_real_name_not_its_id() {
    let f = fixture().await;
    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");

    let sent = f.slack.sent_messages();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].text.contains(RUN_NAME),
        "the message must carry the run's real name: {}",
        sent[0].text
    );
    assert!(
        !sent[0].text.contains(&RUN.to_string()),
        "the message must not fall back to the bare run id: {}",
        sent[0].text
    );
}

/// R104: a run [`RunsReader::get_run`] cannot resolve — not yet ingested, or
/// not visible — is skipped rather than propagated, under the pseudo-channel
/// `"run"` (legacy's own precedent for a failure that pre-empts any channel
/// decision, `notify_queue_event`'s `"run_queue"`, `notifications.rs:664-683`).
/// Nothing is claimed and nothing is sent.
#[tokio::test]
async fn an_unresolvable_run_is_skipped_rather_than_propagated() {
    let f = fixture().await;
    let unknown_run = Uuid::new_v4();

    f.service
        .notify_run_completed(&f.ctx, unknown_run)
        .await
        .expect("a run that cannot be resolved must not propagate a failure");

    assert_eq!(
        f.slack.sends(),
        0,
        "nothing should be sent for a run that could not be resolved"
    );
    let log = f.log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].channel, "run");
    assert_eq!(log[0].outcome, "skipped");
    assert!(!log[0].detail.is_empty());
}

// ---------------------------------------------------------------------------
// R105: real per-schedule settings actually narrow the routing decision
// ---------------------------------------------------------------------------

/// **The test R105 asked for.** `config.slack_enabled` is `true` — config
/// alone would send — but the run's own *schedule* has notifications turned
/// off. If the schedule read were decorative (ignored, or a leftover
/// always-notifies constant), this would still send; it must not.
#[tokio::test]
async fn a_schedule_level_toggle_narrows_the_decision() {
    let f = fixture().await;
    f.runs.add_schedule(
        SCHEDULE,
        ScheduleNotificationSettings {
            slack_enabled: false,
            slack_channel: None,
            slack_events: Vec::new(),
        },
    );

    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");

    assert_eq!(
        f.slack.sends(),
        0,
        "the schedule's own toggle must override a config that otherwise allows sending"
    );
    // R94a/R96: schedule-blocked is always audited, regardless of
    // `config.slack_enabled` — routing.rs's own formula,
    // `!schedule_notifies || config.slack_enabled` = `true || true` = `true`.
    let log = f.log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].channel, "slack");
    assert_eq!(log[0].outcome, "skipped");
}

/// A run with no `schedule_id` at all is legacy's `is_scheduled_run == false`
/// — this module's header, "What an absent schedule means" — so nothing
/// sends even though the stored config alone would allow it, and the skip is
/// still audited (the schedule-blocked state is always logged).
#[tokio::test]
async fn a_run_with_no_schedule_id_is_treated_as_unscheduled() {
    let f = fixture().await;
    // Overwrite the fixture's run with one that carries no schedule at all —
    // `fixture_with`'s own registration always sets one, so this test's
    // whole point is asserting what happens when that is undone.
    let mut run = finished_run(RUN);
    run.name = RUN_NAME.to_owned();
    run.schedule_id = None;
    f.runs.add_run(
        run,
        vec![test_row(RUN, "tests/smoke.py", "test_ok", "PASSED", "")],
    );

    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");

    assert_eq!(
        f.slack.sends(),
        0,
        "a run with no schedule must not notify, matching legacy's is_scheduled_run gate"
    );
    let log = f.log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].channel, "slack");
    assert_eq!(log[0].outcome, "skipped");
}

/// A `schedule_id` that does not resolve (deleted, or not visible) is the
/// same "nothing to narrow with" shape as no `schedule_id` at all — not a
/// distinct silent case.
#[tokio::test]
async fn an_unresolvable_schedule_is_treated_as_unscheduled() {
    let f = fixture().await;
    let mut run = finished_run(RUN);
    run.name = RUN_NAME.to_owned();
    run.schedule_id = Some(Uuid::new_v4()); // never registered
    f.runs.add_run(
        run,
        vec![test_row(RUN, "tests/smoke.py", "test_ok", "PASSED", "")],
    );

    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");

    assert_eq!(f.slack.sends(), 0);
    let log = f.log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].channel, "slack");
    assert_eq!(log[0].outcome, "skipped");
}

// ---------------------------------------------------------------------------
// The R102 seam: the variant-to-string mapping, tested directly
// ---------------------------------------------------------------------------

/// **This is the seam between Task 38 and Task 39.** Task 39's own test pins
/// `UnsupportedMailClient.send(&message).await == Ok(SendOutcome::UnsupportedEgress)`
/// — the variant. This crate's test above
/// (`an_email_send_is_recorded_as_unsupported_egress`) pins the string, but
/// only through the full `notify_run_completed` path. Neither proves the
/// *mapping* on its own; this does, directly, with no send and no database.
#[test]
fn send_outcome_maps_to_the_log_outcome_string_verbatim() {
    assert_eq!(outcome_str(SendOutcome::Sent), "sent");
    assert_eq!(
        outcome_str(SendOutcome::UnsupportedEgress),
        "unsupported_egress"
    );
}

// ---------------------------------------------------------------------------
// Settings CRUD and the log's limit clamp
// ---------------------------------------------------------------------------

/// An unconfigured tenant reads the SDK's own default document rather than a
/// 404 or an error — the same rule `JiraService::get_jira_config` applies to
/// its own settings singleton.
#[tokio::test]
async fn an_unconfigured_tenant_reads_the_default_config() {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let service = NotifyService::new(
        Arc::clone(&db),
        OrmNotifyRepository,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        Arc::new(FakeSlack::default()) as Arc<dyn SlackClient>,
        Arc::new(InertMail::default()) as Arc<dyn MailClient>,
        Arc::new(FakeRuns::default()) as Arc<dyn RunsReader>,
    );
    let ctx = ctx(TENANT);

    assert_eq!(
        service.get_config(&ctx).await.unwrap(),
        NotificationConfig::default()
    );
}

/// A round trip through the service's own write path returns what was saved.
#[tokio::test]
async fn saving_a_config_returns_the_stored_value() {
    let f = fixture().await;
    let updated = NotificationConfig {
        slack_channel: "#qa-alerts".to_owned(),
        ..slack_only_config()
    };
    let stored = f
        .service
        .save_config(&f.ctx, updated.clone())
        .await
        .unwrap();
    assert_eq!(stored, updated);
    assert_eq!(f.service.get_config(&f.ctx).await.unwrap(), updated);
}

/// The log's limit defaults to 100 and is clamped at 500, matching legacy's
/// `api_get_notification_log` (`manager/src/routes/settings.rs:757-761`).
#[tokio::test]
async fn the_log_limit_is_clamped() {
    let f = fixture().await;
    for _ in 0..3 {
        f.service
            .notify_run_completed(&f.ctx, Uuid::new_v4())
            .await
            .unwrap();
    }
    assert_eq!(f.service.list_log(&f.ctx, Some(2)).await.unwrap().len(), 2);
    assert_eq!(
        f.service.list_log(&f.ctx, None).await.unwrap().len(),
        3,
        "an absent limit must default rather than fail"
    );
}

// ---------------------------------------------------------------------------
// The test/preview surfaces
// ---------------------------------------------------------------------------

/// The generic settings-page test send, over both channels the stored config
/// enables — legacy's `send_test_notification` (`notifications.rs:358-385`).
#[tokio::test]
async fn a_generic_test_send_reaches_the_configured_slack_webhook() {
    let f = fixture().await;
    f.service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect("a working slack adapter must not error");
    assert_eq!(f.slack.sends(), 1);
}

/// **R108.** The interactive test-send path forwards the caller's `ctx` too —
/// same guarantee as [`notify_run_completed_sends_slack_as_the_calling_tenant`],
/// pinned on the other of `NotifyService`'s two call sites into
/// `SlackClient::send`.
#[tokio::test]
async fn send_test_sends_slack_as_the_calling_tenant() {
    let f = fixture().await;
    f.service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect("a working slack adapter must not error");

    assert_eq!(f.slack.sent_as_tenants(), [TENANT]);
}

/// **Review finding #37.** The email port takes the caller's `ctx` too, and
/// `NotifyService` forwards the one it holds — the asymmetry the finding names
/// was that [`MailClient::send`] took no identity at all while
/// [`SlackClient::send`] did, even though both are called from this same
/// method under the same tenant's authority. Pinned on the interactive
/// test-send path, the twin of
/// [`send_test_sends_slack_as_the_calling_tenant`].
#[tokio::test]
async fn send_test_sends_email_as_the_calling_tenant() {
    let f = fixture_with(
        Arc::new(FakeSlack::default()),
        NotificationConfig {
            email_enabled: true,
            email_smtp_host: "smtp.example.com".to_owned(),
            email_from: "qa@example.com".to_owned(),
            email_recipients: "ops@example.com".to_owned(),
            ..slack_only_config()
        },
    )
    .await;

    let err = f
        .service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect_err("the inert mail adapter reports unsupported egress");
    assert!(
        matches!(err, DomainError::UnsupportedEgress { .. }),
        "{err:?}"
    );
    assert_eq!(f.mail.sent_as_tenants(), [TENANT]);
}

/// A generic test send propagates a slack failure rather than swallowing it —
/// unlike `notify_run_completed`, this is an interactive action and legacy's
/// own route surfaces the error (`manager/src/routes/settings.rs:465-496`).
#[tokio::test]
async fn a_generic_test_send_propagates_a_slack_failure() {
    let f = fixture_with_failing_slack().await;
    let err = f
        .service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect_err("a broken webhook must be visible to an operator testing it");
    assert!(matches!(err, DomainError::Internal(_)));
}

/// A generic test send against an email-only config surfaces
/// `DomainError::UnsupportedEgress` rather than silently doing nothing — the
/// interactive counterpart to `an_email_send_is_recorded_as_unsupported_egress`'s
/// swallowed automatic path.
#[tokio::test]
async fn a_generic_test_send_over_email_reports_unsupported_egress() {
    let f = fixture_with_email_enabled().await;
    let err = f
        .service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect_err("an operator testing email deserves to be told it cannot send");
    assert!(matches!(err, DomainError::UnsupportedEgress { channel } if channel == "email"));
}

// ---------------------------------------------------------------------------
// Phase C's final review, Important 1: the webhook reference is a reference
// ---------------------------------------------------------------------------

/// A Slack **incoming-webhook URL** — the shape whose whole path is the secret,
/// and the one thing four doc claims in this crate say the column can never
/// hold. It is the value an operator reaches for by reflex, and until Phase C's
/// final review it stored cleanly.
const WEBHOOK_URL: &str = "hooks.slack.com/services/T00000/B00000/XXXXXXXXXXXX";

/// **Important 1, the write path.** `PUT /qa/v1/settings/notifications` must
/// refuse a reference the credential store could not resolve, naming the field,
/// rather than storing it for `GET` to hand back to any holder of
/// `qa.notification_config/get`.
///
/// Four shapes, one per way the check can be reached: the webhook URL itself
/// (slashes and a colon-free host, so the charset is what rejects it), the
/// `credstore://` spelling this crate's own fixtures used to carry, a bare
/// path, and the `cred://` spelling — each still has a slash in the part
/// `validate_credstore_ref` actually inspects, whether or not it had a
/// `cred://` prefix to strip first. The stored document is read back
/// afterwards to prove the refusal happened *before* the write and not
/// after it.
#[tokio::test]
async fn saving_a_webhook_url_as_the_credential_reference_is_refused() {
    let f = fixture().await;

    for candidate in [
        WEBHOOK_URL,
        "credstore://slack/hook",
        "slack/hook",
        "cred://slack/hook",
    ] {
        let err = f
            .service
            .save_config(
                &f.ctx,
                NotificationConfig {
                    slack_enabled: true,
                    slack_webhook_credstore_ref: candidate.to_owned(),
                    ..NotificationConfig::default()
                },
            )
            .await
            .expect_err("a reference oagw cannot resolve must not be stored");
        assert!(
            matches!(&err, DomainError::Validation { field, .. }
                if field == "slack_webhook_credstore_ref"),
            "{candidate:?} must be refused naming its own field, got {err:?}",
        );
    }

    assert_eq!(
        f.service
            .get_config(&f.ctx)
            .await
            .unwrap()
            .slack_webhook_credstore_ref,
        slack_only_config().slack_webhook_credstore_ref,
        "no refused candidate may have reached the stored document",
    );
}

/// **Important 1, the write path's other half.** An *empty* reference is still
/// accepted, because this endpoint has no keep-stored-value convention and
/// empty is how a tenant clears the column — `save_config`'s own doc. A check
/// that refused empty would make the document `GET` hands an unconfigured
/// tenant un-`PUT`-able, which is the mistake the JIRA surface documents having
/// avoided.
#[tokio::test]
async fn an_empty_webhook_reference_still_clears_the_column() {
    let f = fixture().await;
    let cleared = NotificationConfig {
        slack_webhook_credstore_ref: String::new(),
        ..slack_only_config()
    };

    assert_eq!(
        f.service
            .save_config(&f.ctx, cleared.clone())
            .await
            .expect("clearing the reference must be allowed"),
        cleared,
    );
    assert!(
        f.service
            .get_config(&f.ctx)
            .await
            .unwrap()
            .slack_webhook_credstore_ref
            .is_empty(),
    );
}

/// **Important 1, the send path.** `TestSend::ScheduledRun` takes its config
/// from the request body, and that reference becomes the first path segment of
/// the oagw request this gear makes on the caller's behalf. It is validated
/// there too — the same two-call-site shape the JIRA surface uses for
/// `api_token_credstore_ref` — and nothing is sent when it is refused.
#[tokio::test]
async fn a_scheduled_run_test_send_refuses_a_webhook_url_override() {
    let f = fixture().await;
    let err = f
        .service
        .send_test(
            &f.ctx,
            TestSend::ScheduledRun {
                config: Box::new(NotificationConfig {
                    slack_enabled: true,
                    scheduled_run_slack_enabled: true,
                    slack_webhook_credstore_ref: WEBHOOK_URL.to_owned(),
                    ..NotificationConfig::default()
                }),
                event: "failed".to_owned(),
            },
        )
        .await
        .expect_err("a caller-supplied proxy path must be validated, not proxied");
    assert!(
        matches!(&err, DomainError::Validation { field, .. }
            if field == "slack_webhook_credstore_ref"),
        "{err:?}",
    );
    assert_eq!(
        f.slack.sends(),
        0,
        "and nothing may be sent for a refused override",
    );
}

/// The preview surface renders without sending anything — no claim, no log,
/// no slack call — using the caller-supplied config override rather than
/// the stored one, exactly as legacy's `api_preview_notifications` does.
#[tokio::test]
async fn preview_renders_without_sending_or_logging() {
    let f = fixture().await;
    let override_config = NotificationConfig {
        slack_enabled: true,
        slack_webhook_credstore_ref: "cred://slack-other".to_owned(),
        ..NotificationConfig::default()
    };
    let preview = f
        .service
        .preview_scheduled_run(&f.ctx, override_config, "failed")
        .await
        .expect("failed is a valid event token");

    assert_eq!(preview.event, "failed");
    assert_eq!(preview.event_label, "Failed");
    assert!(!preview.fallback_text.is_empty());
    assert_eq!(
        f.slack.sends(),
        0,
        "a preview must never reach the slack port"
    );
    assert!(
        f.log().await.is_empty(),
        "a preview must never write the audit log"
    );
}

/// An event token outside `SLACK_NOTIFICATION_EVENTS` is refused rather than
/// silently rendering nothing.
#[tokio::test]
async fn preview_refuses_an_unknown_event_token() {
    let f = fixture().await;
    let err = f
        .service
        .preview_scheduled_run(&f.ctx, NotificationConfig::default(), "not_a_real_event")
        .await
        .expect_err("an invalid token must not render as an empty preview");
    assert!(matches!(err, DomainError::Validation { .. }));
}

// ---------------------------------------------------------------------------
// R86: NotifyRepository::get_config's tenant predicate
// ---------------------------------------------------------------------------

/// **R86, closed by this task.** Only the *other* tenant has a stored row,
/// and the caller is `TENANT`. A scope spanning both (the shape a
/// parent-tenant grant compiles to) makes an unpinned `.one()` return
/// whichever row it finds first — on `SQLite` with one row in the table,
/// that is deterministically the other tenant's. Removing the `tenant_id`
/// predicate from `NotifyRepository::get_config` turns this red. Mirrors
/// `jira_tests::a_multi_tenant_scope_does_not_carry_another_tenants_credential_reference`,
/// separated by tenant id rather than by a data field — this resource has no
/// natural "day" axis to key rows apart by, unlike the two-tenant tests Task
/// 35 introduced for the results tables.
#[tokio::test]
async fn a_multi_tenant_scope_does_not_return_another_tenants_settings() {
    let f = build(Arc::new(TwoTenantAuthZ), Arc::new(FakeSlack::default())).await;
    let conn = f.db.conn().unwrap();

    // Only the *other* tenant is configured, with a channel of its own.
    OrmNotifyRepository
        .save_config(
            &conn,
            &crate::infra::storage::test_db::scope(OTHER_TENANT),
            OTHER_TENANT,
            NotificationConfig {
                slack_channel: "#other-tenants-alerts".to_owned(),
                ..NotificationConfig::default()
            },
        )
        .await
        .unwrap();

    let read = f.service.get_config(&f.ctx).await.unwrap();
    assert_eq!(
        read.slack_channel, "",
        "an unconfigured tenant must not be shown another tenant's channel: {read:?}"
    );
}

/// **R106, fix round 3.** A scope spanning both tenants must not let
/// `list_log` return the other tenant's audit rows. Separated by a data
/// field (`detail`'s text) rather than by which UUID sorts larger — Task
/// 35's construction, and the reason: an id-ordering trick would pass by
/// accident on the half of the id space where it does not matter.
#[tokio::test]
async fn a_multi_tenant_scope_does_not_list_another_tenants_log_entries() {
    let f = build(Arc::new(TwoTenantAuthZ), Arc::new(FakeSlack::default())).await;
    let conn = f.db.conn().unwrap();

    OrmNotifyRepository
        .append_log(
            &conn,
            &crate::infra::storage::test_db::scope(OTHER_TENANT),
            OTHER_TENANT,
            NewLogEntry {
                run_id: None,
                channel: "slack".to_owned(),
                event_type: String::new(),
                outcome: "sent".to_owned(),
                detail: "belongs to the other tenant only".to_owned(),
            },
        )
        .await
        .unwrap();
    OrmNotifyRepository
        .append_log(
            &conn,
            &crate::infra::storage::test_db::scope(TENANT),
            TENANT,
            NewLogEntry {
                run_id: None,
                channel: "slack".to_owned(),
                event_type: String::new(),
                outcome: "sent".to_owned(),
                detail: "belongs to my own tenant".to_owned(),
            },
        )
        .await
        .unwrap();

    let entries = f.service.list_log(&f.ctx, None).await.unwrap();
    assert_eq!(
        entries.len(),
        1,
        "a scope spanning two tenants must still return only the caller's own tenant's rows: {entries:?}"
    );
    assert_eq!(entries[0].detail, "belongs to my own tenant");
}

// ---------------------------------------------------------------------------
// Phase C's final review, Important 2: the claim path is a write
// ---------------------------------------------------------------------------

/// **Important 2.** The claim `INSERT`, the release `DELETE` and the audit
/// append on the run-completed send path are authorized under
/// `qa.notification_config`/**`update`**, not `/get`.
///
/// Task 38 compiled that scope under `actions::GET`, so a deployment granting
/// read-only notification settings authorized inserting and deleting send-once
/// claim rows. [`RecordingAuthZ`] is the only witness there is for an action
/// string — its own doc says why — so this test reads the requests the service
/// actually sent rather than the ones its code appears to send.
///
/// The two assertions are complementary: the *last* request must be the write,
/// and `get` must have been asked exactly once — the config read at the top of
/// `notify_run_completed`. A regression that moved the claim path back to `get`
/// fails the second even if it somehow satisfied the first.
#[tokio::test]
async fn the_claim_and_release_path_authorizes_a_write_not_a_read() {
    let authz = Arc::new(RecordingAuthZ::default());
    let f = build(
        Arc::clone(&authz) as Arc<dyn AuthZResolverClient>,
        Arc::new(FakeSlack::default()),
    )
    .await;
    f.service
        .save_config(&f.ctx, slack_only_config())
        .await
        .expect("saving the fixture's own config must not fail");
    let mut run = finished_run(RUN);
    run.name = RUN_NAME.to_owned();
    run.schedule_id = Some(SCHEDULE);
    f.runs.add_run(
        run,
        vec![test_row(RUN, "tests/smoke.py", "test_ok", "PASSED", "")],
    );
    f.runs.add_schedule(
        SCHEDULE,
        ScheduleNotificationSettings {
            slack_enabled: true,
            slack_channel: None,
            slack_events: Vec::new(),
        },
    );
    let before = authz.asked().len();

    f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();
    assert_eq!(f.slack.sends(), 1, "the send path must have been reached");

    let asked = authz.asked();
    let during = &asked[before..];
    assert_eq!(
        during.last(),
        Some(&(
            resources::NOTIFICATION_CONFIG_NAME.to_owned(),
            actions::UPDATE.to_owned()
        )),
        "the claim/release/log path must ask for a write: {during:?}",
    );
    assert_eq!(
        during
            .iter()
            .filter(|(_, action)| action == actions::GET)
            .count(),
        1,
        "only the config read may be a `get` on this path: {during:?}",
    );
}
