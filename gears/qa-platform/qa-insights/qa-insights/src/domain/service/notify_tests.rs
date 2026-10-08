//! Tests for [`NotifyService`] — dedupe, the audit log, the release of a failed
//! send's claim, and (fix round 1) the real run resolution.
//!
//! Against the **real** repository (`OrmNotifyRepository`) on in-memory
//! `SQLite`, matching `jira_tests`/`saved_views_tests`: what is under test is
//! whether the service's claim/send/log orchestration is the one the storage
//! layer actually implements. Slack is doubled ([`FakeSlack`]) because this
//! module's job is the orchestration, not the wire format Task 39's adapter
//! owns; mail is [`InertMail`], a local double, since `NeverWiredMailClient`
//! moved to `gear.rs` in fix round 1 and a domain-layer test importing from the
//! composition root would invert this crate's dependency direction. qa-runs is
//! [`crate::domain::service::test_support::FakeRuns`] — the same double
//! `reconcile_tests`/`ingest_tests` already share — seeded with `RUN`'s real
//! name and results in [`fixture_with`], which is fix round 1's fix: a first
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
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use qa_insights_sdk::NotificationConfig;
use qa_runs_sdk::ScheduleNotificationSettings;
use toolkit_db::DBProvider;
use uuid::Uuid;

use super::{NotifyService, TestSend, failure_outcome_str, outcome_str};
use crate::domain::error::DomainError;
use crate::domain::ports::{
    MailClient, MailMessage, RunsReader, SendOutcome, SlackClient, SlackMessage,
};
use crate::domain::repos::{NewLogEntry, NotificationClaim, NotifyRepository};
use crate::domain::service::test_support::{
    FakeRuns, RecordingAuthZ, TenantScopedAuthZ, ctx, finished_run, test_row,
};
use crate::domain::service::{actions, resources};
use crate::domain::system_actor::{self, QA_INSIGHTS_SYSTEM_ACTOR_UUID, TenantBound};
use crate::infra::storage::notify_sea_repo::OrmNotifyRepository;
use crate::infra::storage::test_db::inmem_db;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use toolkit_security::PlatformSecurityContext;

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
impl AuthZResolverApi for TwoTenantAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, CanonicalError> {
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
/// (and the `ctx` it was sent under — `SlackClient::send`'s `ctx` parameter)
/// and answers a scripted result, front-first — `Ok(SendOutcome::Sent)` when
/// nothing is scripted, which is the shape most tests want and need not script.
#[derive(Default)]
struct FakeSlack {
    sent: Mutex<Vec<SlackMessage>>,
    /// The `subject_tenant_id()` of every `ctx` `send` was called with, in
    /// call order — what
    /// [`notify_run_completed_sends_slack_as_the_calling_tenant`] and
    /// [`send_test_sends_slack_as_the_calling_tenant`] read to pin that
    /// `NotifyService` forwards its own `ctx` rather than dropping it.
    sent_as: Mutex<Vec<Uuid>>,
    /// The `subject_id()` of every `ctx` `send` was called with:
    /// which *identity* a send resolved its secret as, not only which tenant.
    sent_as_subjects: Mutex<Vec<Uuid>>,
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

    fn sent_as_subjects(&self) -> Vec<Uuid> {
        self.sent_as_subjects.lock().unwrap().clone()
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
        self.sent_as_subjects.lock().unwrap().push(ctx.subject_id());
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(SendOutcome::Sent))
    }
}

/// A [`MailClient`] double, local to this test module — `gear.rs`'s
/// `NeverWiredMailClient` moved out of `notify` in fix round 1 and a
/// domain-layer test importing it back from the composition root would
/// invert this crate's dependency direction. Same behaviour as
/// `infra::notify::UnsupportedMailClient`: every send fails with
/// [`DomainError::UnsupportedEgress`] — but it records the identity it was
/// called as, which is what review finding #37 added to this port and
/// [`send_test_sends_email_as_the_calling_tenant`] reads.
///
/// **It used to answer `Ok(SendOutcome::UnsupportedEgress)`**, which is what
/// the real adapter did until the SMTP follow-up; the rename from "inert" would
/// be cosmetic, and the name is what the tests below still call it.
#[derive(Default)]
struct InertMail {
    sent_as: Mutex<Vec<Uuid>>,
    /// The `subject_id()` of every `ctx` `send` was called with — the
    /// identity the SMTP password would be resolved as.
    sent_as_subjects: Mutex<Vec<Uuid>>,
}

impl InertMail {
    fn sent_as_tenants(&self) -> Vec<Uuid> {
        self.sent_as.lock().unwrap().clone()
    }

    fn sent_as_subjects(&self) -> Vec<Uuid> {
        self.sent_as_subjects.lock().unwrap().clone()
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
        self.sent_as_subjects.lock().unwrap().push(ctx.subject_id());
        Err(DomainError::UnsupportedEgress {
            channel: "email".to_owned(),
        })
    }
}

struct Fixture {
    service: NotifyService<OrmNotifyRepository>,
    ctx: toolkit_security::SecurityContext,
    slack: Arc<FakeSlack>,
    /// This deployment's notification cutoff, as the migration wrote it —
    /// read once here so every fixture can place its run on a *stated* side of
    /// it. See [`read_cutoff`].
    cutoff: time::OffsetDateTime,
    /// The same provider the service holds, so a test can read or write a
    /// row directly through the repository under a scope of its own
    /// choosing — `jira_tests::Fixture::db`'s precedent, needed here by
    /// `a_multi_tenant_scope_does_not_return_another_tenants_settings`.
    db: Arc<DBProvider<DomainError>>,
    /// The qa-runs double [`NotifyService::notify_run_completed`] resolves
    /// a run through — held so a test can register or omit a run.
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
        slack_webhook_credstore_ref: "slack-hook".to_owned(),
        // [`fixture_with`]'s run has one **passing** result, and the outcome
        // policy went live on 2026-09-29 (`domain::notify::routing`'s
        // header). At `NotificationConfig::default`'s `notify_on_success:
        // false` every test built on this config would be asserting against a
        // run the policy silenced rather than the channel gate it names — the
        // whole `notify_run_completed` half of this module went red together
        // when the ruling landed, which is how that was found rather than
        // shipped. Set here so these tests measure the channel gates.
        notify_on_success: true,
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
async fn build(authz: Arc<dyn AuthZResolverApi>, slack: Arc<FakeSlack>) -> Fixture {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let cutoff = read_cutoff(&db).await;
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
        cutoff,
        db,
        runs,
        mail,
    }
}

/// This deployment's notification cutoff, as
/// `m20260929_000004_run_completed_notification_cutoff` wrote it when
/// [`inmem_db`] applied the chain — so, in a test, a moment ago.
///
/// Every run registered below is placed **relative to this value** rather than
/// at a calendar literal. `finished_run`'s own instant is `2026-08-18`, which
/// this cutoff makes history: without the offset, every test in this module
/// would be asserting against a run `notify_run_completed` declines before it
/// reaches a channel at all. They went red together when the cutoff landed,
/// which is how this was found rather than shipped.
async fn read_cutoff(db: &Arc<DBProvider<DomainError>>) -> time::OffsetDateTime {
    let conn = db.conn().unwrap();
    OrmNotifyRepository
        .run_completed_cutoff(&conn)
        .await
        .expect("the cutoff read must not fail")
        .expect("a migrated database always has the cutoff row")
}

/// An instant comfortably **after** [`Fixture::cutoff`] — a run that finished
/// since this deployment started notifying, which is what every test here
/// except the two about history wants.
fn after_cutoff(f: &Fixture) -> time::OffsetDateTime {
    f.cutoff + time::Duration::hours(1)
}

/// [`build`] plus a saved config and [`RUN`] registered with a real name and
/// one passing result — fix round 1's fixture: every dedupe/failure/mail test
/// needs [`RunsReader::get_run`] to actually succeed, or the method would skip
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
    run.finished_at = Some(after_cutoff(&f));
    f.runs.add_run(
        run,
        vec![test_row(RUN, "tests/smoke.py", "test_ok", "PASSED", "")],
    );
    // A schedule that notifies on everything — the fixture default.
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
/// `notify_run_completed`'s Slack side hits the *silent* skip (Slack disabled
/// while the schedule-level gate passed) and only the email channel logs
/// anything, which is what `an_email_send_is_recorded_as_unsupported_egress`
/// filters on.
async fn fixture_with_email_enabled() -> Fixture {
    fixture_with(
        Arc::new(FakeSlack::default()),
        NotificationConfig {
            email_enabled: true,
            email_smtp_host: "smtp.example.com".to_owned(),
            email_from: "qa@example.com".to_owned(),
            email_recipients: "ops@example.com".to_owned(),
            notify_on_success: true,
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

    async fn claimed_kinds<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        run_id: Uuid,
        event: &str,
    ) -> Result<Vec<String>, DomainError> {
        OrmNotifyRepository
            .claimed_kinds(runner, scope, tenant_id, run_id, event)
            .await
    }

    /// Delegated like everything but the release: this double exists to fail
    /// *one* method, and a cutoff answered differently here would silently
    /// change which runs the tests around it notify at all.
    async fn run_completed_cutoff<C: DBRunner>(
        &self,
        runner: &C,
    ) -> Result<Option<time::OffsetDateTime>, DomainError> {
        OrmNotifyRepository.run_completed_cutoff(runner).await
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
                notify_on_success: true,
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
                notify_on_success: true,
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

/// Nothing was sent, so the claim must not survive — otherwise the slot is held
/// forever the moment a working adapter replaces the inert one, the
/// claim-release rule's own failure mode inverted. Proven by a second attempt:
/// without the release, it would find itself already claimed and log
/// `"skipped"` instead of trying (and reporting `"unsupported_egress"`) again.
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
    // After the cutoff, for [`read_cutoff`]'s reason. This test builds its own
    // service rather than going through `build`, so it reads the cutoff itself.
    run.finished_at = Some(read_cutoff(&db).await + time::Duration::hours(1));
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
/// is an insert that reports whether *this* caller won; a
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
/// # The release of a failed send's claim interacts with this test
///
/// A failed send releases its claim (`NotifyRepository::release_notification`),
/// so this also asserts the retry half: a second `notify_run_completed` call
/// for the same run must be able to claim and send again. That is a
/// **different** guarantee than the dedupe test above proves — this one is
/// about a *failed* send being retryable by construction, not about a
/// *successful* send being exactly-once.
///
/// **Retryable is not retried**, and the second call here is this test making
/// one, not the gear. Nothing in the sweep re-projects a run that already has
/// result rows, so in production that second call comes from an operator
/// rebuild — see `NotifyService::release_claim_ignoring_failure` and
/// `DESIGN.md` §3.5 "Egress" (final review, finding 2).
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

    // The claim was released, so a retry is not permanently suppressed.
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

/// `NotifyService` holds a real `ctx` at the point it calls `SlackClient::send`
/// and must forward it rather than let the call go out under some other
/// identity — the exact drop `SlackOagwClient`'s first draft had no way to
/// avoid before the port grew the `ctx` parameter this test reads.
#[tokio::test]
async fn notify_run_completed_sends_slack_as_the_calling_tenant() {
    let f = fixture().await;
    f.service
        .notify_run_completed(&f.ctx, RUN)
        .await
        .expect("does not propagate");

    assert_eq!(f.slack.sent_as_tenants(), [TENANT]);
}

/// Email is configured and the mail adapter is [`InertMail`], which fails every
/// send the way `UnsupportedMailClient` does when `smtp_allowed_hosts` is
/// empty. The routing decision, the dedupe claim and the log entry all happen,
/// and the entry's outcome is `unsupported_egress`.
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
// The rendered message is the run's real content, not a stand-in
// ---------------------------------------------------------------------------

/// **The test fix round 1 asked for.** A first draft rendered
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

/// A run [`RunsReader::get_run`] cannot resolve — not yet ingested, or
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
// Real per-schedule settings actually narrow the routing decision
// ---------------------------------------------------------------------------

/// **The test fix round 2 asked for.** `config.slack_enabled` is `true` —
/// config alone would send — but the run's own *schedule* has notifications
/// turned off. If the schedule read were decorative (ignored, or a leftover
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
    // Schedule-blocked is always audited, regardless of
    // `config.slack_enabled` — routing.rs's own formula,
    // `!schedule_notifies || config.slack_enabled` = `true || true` = `true`.
    let log = f.log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].channel, "slack");
    assert_eq!(log[0].outcome, "skipped");
}

/// **The owner's 2026-09-29 ruling, at the service seam.** A run with no
/// `schedule_id` at all was legacy's `is_scheduled_run == false` and notified
/// on no channel ever; it now routes on the tenant's own settings — this
/// module's header, "What an absent schedule means", and
/// `domain::notify::routing`'s header for the ruling.
///
/// The second half is what stops the first from being a one-way widening:
/// the same ad-hoc run under a tenant that has Slack off sends nothing. Both
/// run against the same registration, so neither can pass by the method
/// ignoring the run it was handed.
///
/// Mutated against: restoring the synthetic `ScheduleNotificationSettings {
/// slack_enabled: false, .. }` for a `None` schedule turns the first
/// assertion red on `sends() == 1`.
#[tokio::test]
async fn an_ad_hoc_run_notifies_on_the_tenants_own_settings() {
    let f = fixture().await;
    // Overwrite the fixture's run with one that carries no schedule at all —
    // `fixture_with`'s own registration always sets one, so this test's
    // whole point is asserting what happens when that is undone.
    let mut run = finished_run(RUN);
    run.name = RUN_NAME.to_owned();
    run.schedule_id = None;
    run.finished_at = Some(after_cutoff(&f));
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
        1,
        "a run no schedule launched must notify on the tenant's settings"
    );

    // The same run, for a tenant whose Slack channel is off: the widening is
    // "the schedule narrows nothing", not "an ad-hoc run always sends".
    let g = fixture_with(
        Arc::new(FakeSlack::default()),
        NotificationConfig {
            slack_enabled: false,
            slack_webhook_credstore_ref: "slack-hook".to_owned(),
            notify_on_success: true,
            ..NotificationConfig::default()
        },
    )
    .await;
    let mut run = finished_run(RUN);
    run.name = RUN_NAME.to_owned();
    run.schedule_id = None;
    run.finished_at = Some(after_cutoff(&g));
    g.runs.add_run(
        run,
        vec![test_row(RUN, "tests/smoke.py", "test_ok", "PASSED", "")],
    );

    g.service
        .notify_run_completed(&g.ctx, RUN)
        .await
        .expect("does not propagate");

    assert_eq!(
        g.slack.sends(),
        0,
        "the tenant's own switch must still be able to silence an ad-hoc run"
    );
}

/// **Finding 6.** A run whose every channel has been decided costs one indexed
/// claim read on the next pass and nothing else.
///
/// The sweep re-projects a run with zero result rows on **every** tick for as
/// long as it sits in the lookback window — 12 times an hour at the defaults,
/// and forever on a tenant whose sweep has wedged — so each of those passes
/// used to repeat two cross-gear reads that could produce nothing and append
/// another `Already sent (duplicate reservation)` row to a log with no
/// retention sweep. 1095 of the dev stand's 2326 finished runs are that shape.
///
/// `result_reads` is the observable: it counts `list_run_test_results`, the
/// second of the two cross-gear calls, so a pass that short-circuits before
/// them leaves it unmoved. Asserted alongside the log size and the send count
/// because all three are the same claim — the pass did nothing at all, not
/// just nothing visible.
#[tokio::test]
async fn a_decided_run_costs_no_further_cross_gear_reads_or_audit_rows() {
    let f = fixture().await;

    f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();
    assert_eq!(f.slack.sends(), 1, "the first pass must actually send");
    let rows_after_the_send = f.log().await.len();
    let reads_after_the_send = f.runs.result_reads();
    assert!(
        reads_after_the_send > 0,
        "the first pass must have read the run's results, or this test is measuring nothing"
    );

    for _ in 0..3 {
        f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();
    }

    assert_eq!(
        f.runs.result_reads(),
        reads_after_the_send,
        "a decided run must not be read out of qa-runs again on every sweep tick"
    );
    assert_eq!(
        f.log().await.len(),
        rows_after_the_send,
        "a decided run must not append an audit row on every sweep tick: {:?}",
        f.log().await
    );
    assert_eq!(f.slack.sends(), 1, "and it must certainly not send again");
}

/// **Finding 1 and finding 6, at the service seam.** A routing skip takes the
/// channel's claim slot even though nothing was sent, which has two
/// consequences this test pins together because they are the same row:
///
/// 1. The skip is audited **once**, not once per pass. The sweep re-projects a
///    run with zero result rows on every tick (12 times an hour at the
///    defaults), and before this each pass appended another identical
///    `skipped` row to a log with no retention sweep.
/// 2. Turning the channel on afterwards does not make a later pass announce
///    the run: the decision was already recorded, so the claim is lost and
///    nothing is sent. That is what stops `POST /qa/v1/insights/rebuild` from
///    mailing a window's worth of previously-declined runs.
///
/// The schedule-level gate is used to decline here, which is routing's
/// **audited** skip (`!schedule_notifies || config.slack_enabled`); the silent
/// one is covered end-to-end by
/// `reconcile_tests::a_run_whose_routing_declined_is_not_announced_by_a_later_rebuild`.
#[tokio::test]
async fn a_routing_skip_claims_its_slot_and_is_audited_once() {
    let f = fixture().await;
    f.runs.add_schedule(
        SCHEDULE,
        ScheduleNotificationSettings {
            slack_enabled: false,
            slack_channel: None,
            slack_events: Vec::new(),
        },
    );

    f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();
    f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();

    let log = f.log().await;
    assert_eq!(
        log.len(),
        1,
        "one skip, one row: a second pass over the same run must not append another \
         identical one; got {log:?}"
    );
    assert_eq!(log[0].outcome, "skipped");

    // The schedule starts notifying. The decision for this run was already
    // made and recorded, so the run it declined stays declined.
    f.runs.add_schedule(
        SCHEDULE,
        ScheduleNotificationSettings {
            slack_enabled: true,
            slack_channel: None,
            slack_events: Vec::new(),
        },
    );
    f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();

    assert_eq!(
        f.slack.sends(),
        0,
        "a run whose routing declined must not be announced once the gate that declined it \
         is turned on"
    );
}

/// A `schedule_id` that does not resolve (deleted, or not visible) is the
/// same "nothing to narrow with" shape as no `schedule_id` at all — not a
/// distinct silent case, and since the 2026-09-29 ruling not a silent case
/// at all: it routes on the tenant's settings exactly as an ad-hoc run does.
///
/// Pinned as its own test rather than folded into the one above because the
/// two reach `None` by different code paths — `run.schedule_id.is_none()`
/// and `get_schedule_notifications` answering `Ok(None)` — and only one of
/// them is a qa-runs round trip.
#[tokio::test]
async fn an_unresolvable_schedule_is_treated_as_having_no_schedule() {
    let f = fixture().await;
    let mut run = finished_run(RUN);
    run.name = RUN_NAME.to_owned();
    run.schedule_id = Some(Uuid::new_v4()); // never registered
    run.finished_at = Some(after_cutoff(&f));
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
        1,
        "a schedule that cannot be resolved narrows nothing, so the tenant's settings decide"
    );
}

// ---------------------------------------------------------------------------
// The owner's 2026-09-29 ruling, at the service seam
// ---------------------------------------------------------------------------

/// **Email no longer rides the schedule's Slack flag.** The schedule has
/// notifications off, which used to return before both branches, so a tenant
/// with email configured got nothing. Slack is still blocked by that flag —
/// both halves are asserted, so the email half cannot pass by the gate simply
/// having been deleted.
///
/// The email channel's observable is its **log row**, not a delivered
/// message: this deployment's mail port is still the inert one, so a
/// reached email channel records `unsupported_egress`. That it was reached at
/// all is the property.
///
/// Mutated against: restoring `schedule_allows_slack &&` to `route`'s `email`
/// term leaves the log with no email row and turns this red.
#[tokio::test]
async fn email_is_attempted_when_the_schedules_slack_flag_is_off() {
    let f = fixture_with(
        Arc::new(FakeSlack::default()),
        NotificationConfig {
            slack_enabled: true,
            slack_webhook_credstore_ref: "slack-hook".to_owned(),
            email_enabled: true,
            email_smtp_host: "smtp.example.com".to_owned(),
            email_from: "qa@example.com".to_owned(),
            email_recipients: "ops@example.com".to_owned(),
            notify_on_success: true,
            ..NotificationConfig::default()
        },
    )
    .await;
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
        "the schedule's own Slack flag must still gate Slack"
    );
    let log = f.log().await;
    let email: Vec<_> = log.iter().filter(|e| e.channel == "email").collect();
    assert_eq!(
        email.len(),
        1,
        "the email channel must have been reached despite the schedule's Slack flag: {log:?}"
    );
    assert_eq!(email[0].outcome, "unsupported_egress");
}

/// **A skip the outcome policy made still claims its slot** — the invariant
/// the whole widening rests on (finding 1). The run passed and
/// `notify_on_success` is off, so both channels decline; turning the policy
/// on afterwards must not make a later pass announce it, exactly as turning a
/// *channel* on afterwards does not
/// ([`a_routing_skip_claims_its_slot_and_is_audited_once`]).
///
/// Without this, every run a deployment declined on outcome would be a run an
/// operator's `POST /qa/v1/insights/rebuild` could still mail — the class of
/// bug the claim-on-decline rule exists to close, reopened by the new gate
/// that creates the declines.
///
/// The audited half is asserted too, and it is the only test that reaches it:
/// `slack_skip_is_audited`'s formula is unchanged by the ruling, and this is
/// the state where its `config.slack_enabled` term — rather than its
/// schedule term — is what makes an outcome-policy skip visible
/// (`domain::notify::routing`'s header, "Slack skips: audited or silent").
///
/// Mutated against: dropping `|| config.slack_enabled` from
/// `slack_skip_is_audited` leaves the log empty and turns the row assertion
/// red, and turns nothing else in the suite red; making
/// `decline_run_completed_channel` return without claiming turns the final
/// assertion red on a send.
#[tokio::test]
async fn an_outcome_policy_skip_claims_its_slot_so_a_later_pass_stays_silent() {
    let f = fixture_with(
        Arc::new(FakeSlack::default()),
        NotificationConfig {
            slack_enabled: true,
            slack_webhook_credstore_ref: "slack-hook".to_owned(),
            // The fixture's run passes; this is the policy that declines it.
            notify_on_success: false,
            ..NotificationConfig::default()
        },
    )
    .await;

    f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();
    assert_eq!(
        f.slack.sends(),
        0,
        "a passing run must not be announced while notify_on_success is off"
    );
    let log = f.log().await;
    assert_eq!(
        log.len(),
        1,
        "an outcome-policy skip is audited, not silent: {log:?}"
    );
    assert_eq!(log[0].channel, "slack");
    assert_eq!(log[0].outcome, "skipped");

    // The operator turns the policy on and the run is considered again.
    f.service
        .save_config(
            &f.ctx,
            NotificationConfig {
                slack_enabled: true,
                slack_webhook_credstore_ref: "slack-hook".to_owned(),
                notify_on_success: true,
                ..NotificationConfig::default()
            },
        )
        .await
        .unwrap();
    f.service.notify_run_completed(&f.ctx, RUN).await.unwrap();

    assert_eq!(
        f.slack.sends(),
        0,
        "the decision was already recorded: enabling the policy must not retroactively \
         announce the run it declined"
    );
}

// ---------------------------------------------------------------------------
// The send-outcome seam: the variant-to-string mapping, tested directly
// ---------------------------------------------------------------------------

/// **This is the send-outcome seam** (`domain::ports`, "The send-outcome
/// seam"), and it has two halves since the SMTP follow-up:
/// a successful send's outcome comes from the [`SendOutcome`] variant, and a
/// failed one's comes from the [`DomainError`] variant. The test above
/// (`an_email_send_is_recorded_as_unsupported_egress`) pins the strings through
/// the full `notify_run_completed` path; this pins the mappings directly, with
/// no send and no database.
///
/// The `unsupported_egress` half moved here from the enum when the inert mail
/// adapter started failing instead of reporting: keeping the string but
/// choosing it from the error is what stopped "this deployment cannot send
/// email" from collapsing into the same audit row as "the relay refused this
/// message".
#[test]
fn send_outcome_maps_to_the_log_outcome_string_verbatim() {
    assert_eq!(outcome_str(SendOutcome::Sent), "sent");
    assert_eq!(
        failure_outcome_str(&DomainError::UnsupportedEgress {
            channel: "email".to_owned()
        }),
        "unsupported_egress"
    );
    assert_eq!(
        failure_outcome_str(&DomainError::Internal("the relay refused it".to_owned())),
        "failed",
        "an ordinary transport failure must not be recorded as a missing adapter"
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

/// The interactive test-send path forwards the caller's `ctx` too — same
/// guarantee as [`notify_run_completed_sends_slack_as_the_calling_tenant`],
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

/// The settings test send resolves the Slack webhook and the
/// SMTP password as the qa-insights system actor, bound to the caller's tenant
/// — the identity every real send has — so a secret real sends cannot read
/// fails the test instead of passing it. Authorization of the *test* itself
/// stays the caller's (the PDP is asked as `f.ctx`).
#[tokio::test]
async fn a_generic_test_send_resolves_secrets_as_the_system_actor() {
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
    assert_ne!(f.ctx.subject_id(), QA_INSIGHTS_SYSTEM_ACTOR_UUID);

    let err = f
        .service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect_err("the inert mail adapter reports unsupported egress");
    assert!(
        matches!(err, DomainError::UnsupportedEgress { .. }),
        "{err:?}"
    );

    assert_eq!(f.slack.sent_as_subjects(), [QA_INSIGHTS_SYSTEM_ACTOR_UUID]);
    assert_eq!(f.mail.sent_as_subjects(), [QA_INSIGHTS_SYSTEM_ACTOR_UUID]);
    assert_eq!(f.slack.sent_as_tenants(), [TENANT]);
    assert_eq!(f.mail.sent_as_tenants(), [TENANT]);
}

/// The scheduled-run arm too: its override names a reference the real send
/// would resolve as the same actor.
#[tokio::test]
async fn a_scheduled_run_test_send_resolves_the_webhook_as_the_system_actor() {
    let f = fixture().await;

    f.service
        .send_test(
            &f.ctx,
            TestSend::ScheduledRun {
                config: Box::new(NotificationConfig {
                    slack_enabled: true,
                    scheduled_run_slack_enabled: true,
                    slack_webhook_credstore_ref: "slack-hook".to_owned(),
                    ..NotificationConfig::default()
                }),
                event: "failed".to_owned(),
            },
        )
        .await
        .expect("the slack double accepts by default");

    assert_eq!(f.slack.sent_as_subjects(), [QA_INSIGHTS_SYSTEM_ACTOR_UUID]);
}

/// The test send and the real send reach the port as **the same subject** —
/// pinned against the real send's own factory rather than against the
/// constant alone, so a later change to either side shows up here.
#[tokio::test]
async fn the_test_send_and_a_real_send_resolve_secrets_as_the_same_subject() {
    let f = fixture().await;
    f.service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect("the slack double accepts by default");
    let real = system_actor::for_reconcile_sweep(TenantBound::new(TENANT).expect("non-nil"));
    f.service
        .notify_run_completed(&real, RUN)
        .await
        .expect("the fixture's run notifies");

    let subjects = f.slack.sent_as_subjects();
    assert_eq!(
        subjects.len(),
        2,
        "one test send and one real send: {subjects:?}"
    );
    assert_eq!(subjects[0], subjects[1]);
}

/// A PDP double that grants **every** caller [`TENANT`]'s scope, whatever
/// tenant the subject carries — so a nil-tenant caller gets past
/// `NotifyService::scope` and the only thing left to refuse it is
/// `send_test`'s own [`TenantBound`] check. [`TenantScopedAuthZ`] cannot stand
/// in here: it compiles no constraint for a nil tenant, so the PDP step would
/// refuse first and the test would pass with the check deleted.
struct GrantsTenantToAnyCallerAuthZ;

#[async_trait]
impl AuthZResolverApi for GrantsTenantToAnyCallerAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::models::EvaluationResponse, CanonicalError> {
        use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
        Ok(authz_resolver_sdk::models::EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::models::EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        toolkit_security::pep_properties::OWNER_TENANT_ID,
                        [TENANT],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// A caller with no tenant cannot mint a tenant-bound actor; refused before any
/// port, as `ReconcileService::rebuild` refuses it — on both arms, and even
/// when the PDP has granted the test.
#[tokio::test]
async fn a_test_send_from_a_tenantless_caller_is_forbidden_before_any_port() {
    let f = build(
        Arc::new(GrantsTenantToAnyCallerAuthZ),
        Arc::new(FakeSlack::default()),
    )
    .await;
    f.service
        .save_config(&f.ctx, slack_only_config())
        .await
        .expect("saving the fixture's own config must not fail");
    let tenantless = ctx(Uuid::nil());

    for request in [
        TestSend::Generic,
        TestSend::ScheduledRun {
            config: Box::new(NotificationConfig {
                slack_enabled: true,
                scheduled_run_slack_enabled: true,
                slack_webhook_credstore_ref: "slack-hook".to_owned(),
                ..NotificationConfig::default()
            }),
            event: "failed".to_owned(),
        },
    ] {
        let err = f
            .service
            .send_test(&tenantless, request)
            .await
            .expect_err("a nil tenant cannot be bound");
        assert!(matches!(err, DomainError::Forbidden), "{err:?}");
    }
    assert_eq!(f.slack.sends(), 0);
    assert!(f.mail.sent_as_tenants().is_empty());
}

/// A generic test send propagates a slack failure rather than swallowing it —
/// unlike `notify_run_completed`, this is an interactive action and legacy's
/// own route surfaces the error (`manager/src/routes/settings.rs:465-496`).
///
/// This proves the propagation in a debug build only. What keeps it true in
/// release is `source_hygiene_tests`, which refuses a `?`, `.await` or `&mut`
/// inside a debug assertion (where release builds never evaluate it).
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

/// With both channels enabled, a Slack failure does not stop the email
/// attempt: an operator testing "my notifications" learns about both channels
/// from one click, and the audit log holds a row for each. The response
/// carries the first failure in channel order (Slack, then email).
#[tokio::test]
async fn a_generic_test_send_tries_email_after_a_slack_failure_and_audits_both() {
    let slack = Arc::new(FakeSlack::default());
    slack.script_next(Err(DomainError::Internal(
        "slack webhook unreachable".to_owned(),
    )));
    let f = fixture_with(
        Arc::clone(&slack),
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
        .expect_err("both channels failed");

    assert!(
        matches!(err, DomainError::Internal(ref m) if m == "slack webhook unreachable"),
        "{err:?}"
    );
    assert_eq!(
        f.mail.sent_as_tenants().len(),
        1,
        "email must be attempted after Slack failed"
    );
    let rows: Vec<_> = f
        .log()
        .await
        .into_iter()
        .filter(|r| r.event_type == "test")
        .collect();
    let mut channels: Vec<_> = rows
        .iter()
        .map(|r| (r.channel.clone(), r.outcome.clone()))
        .collect();
    channels.sort();
    assert_eq!(
        channels,
        [
            ("email".to_owned(), "unsupported_egress".to_owned()),
            ("slack".to_owned(), "failed".to_owned()),
        ],
        "{rows:?}"
    );
}

/// A disabled channel is neither tried nor audited. "Every enabled channel is
/// attempted" must not widen into every channel: with email configured but
/// switched off, a failing Slack test is the only attempt and the only audit
/// row, and its failure is the answer.
#[tokio::test]
async fn a_generic_test_send_neither_tries_nor_audits_a_disabled_channel() {
    let slack = Arc::new(FakeSlack::default());
    slack.script_next(Err(DomainError::Internal(
        "slack webhook unreachable".to_owned(),
    )));
    let f = fixture_with(
        Arc::clone(&slack),
        NotificationConfig {
            // Fully configured, so only the flag keeps it out.
            email_enabled: false,
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
        .expect_err("the only enabled channel failed");

    assert!(
        matches!(err, DomainError::Internal(ref m) if m == "slack webhook unreachable"),
        "{err:?}"
    );
    assert_eq!(f.slack.sends(), 1);
    assert!(
        f.mail.sent_as_tenants().is_empty(),
        "a disabled email channel must not be attempted"
    );
    let channels: Vec<_> = f
        .log()
        .await
        .into_iter()
        .filter(|r| r.event_type == "test")
        .map(|r| (r.channel, r.outcome))
        .collect();
    assert_eq!(
        channels,
        [("slack".to_owned(), "failed".to_owned())],
        "exactly one test audit row, the enabled channel's"
    );
}

/// The other order: Slack succeeds, email fails. The email failure is what the
/// caller gets, and Slack's success is still audited.
#[tokio::test]
async fn a_generic_test_send_reports_an_email_failure_after_a_slack_success() {
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
        .expect_err("the inert mail adapter fails");

    assert!(
        matches!(err, DomainError::UnsupportedEgress { ref channel } if channel == "email"),
        "{err:?}"
    );
    assert_eq!(f.slack.sends(), 1);
    let outcomes: Vec<_> = f
        .log()
        .await
        .into_iter()
        .filter(|r| r.event_type == "test")
        .map(|r| (r.channel, r.outcome))
        .collect();
    assert!(
        outcomes.contains(&("slack".to_owned(), "sent".to_owned())),
        "{outcomes:?}"
    );
    assert!(
        outcomes.contains(&("email".to_owned(), "unsupported_egress".to_owned())),
        "{outcomes:?}"
    );
}

/// **A test send that failed leaves a row in the audit log.**
///
/// This is the outcome the log exists for: the settings log is the only place
/// an operator sees that Slack or SMTP is broken (DESIGN 3.5 "Egress"), and
/// `send_test` — the one surface reached *because* something is wrong — wrote
/// nothing at all until this task.
///
/// The `outcome` is the same `"failed"` the automatic path writes, and the
/// detail carries the failure's own text, so an operator reading the log
/// cannot tell whether the attempt came from a sweep or from a button — only
/// `event_type = "test"` says that, which is why the row is asserted on the
/// token rather than on the text.
#[tokio::test]
async fn a_failed_test_send_is_audited() {
    let f = fixture_with_failing_slack().await;

    f.service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect_err("the fixture's slack double is scripted to fail");

    let rows = f.log().await;
    let row = rows
        .iter()
        .find(|r| r.event_type == "test")
        .unwrap_or_else(|| panic!("no test-send row in the audit log: {rows:?}"));
    assert_eq!(row.channel, "slack");
    assert_eq!(row.outcome, "failed");
    assert_eq!(row.run_id, None, "a test send is about no run");
    assert!(
        row.detail.contains("slack webhook unreachable"),
        "the failure's own text must reach the log, or the row says only that something \
         happened: {}",
        row.detail
    );
}

/// The other outcome. A test send that reached the far side is audited as
/// `"sent"`.
///
/// Without this, [`a_failed_test_send_is_audited`] would also pass for an
/// implementation that logged only failures — which is a defensible design and
/// not the one `notify_run_completed` uses, so the two surfaces would have
/// drifted apart in the one table an operator reads both from.
#[tokio::test]
async fn a_successful_test_send_is_audited_too() {
    let f = fixture().await;

    f.service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect("the fixture's slack double accepts by default");

    let rows = f.log().await;
    let row = rows
        .iter()
        .find(|r| r.event_type == "test")
        .unwrap_or_else(|| panic!("no test-send row in the audit log: {rows:?}"));
    assert_eq!(row.channel, "slack");
    assert_eq!(row.outcome, "sent");
}

/// A refusal `send_test` makes **before** reaching a port writes no row.
///
/// Nothing was attempted, so there is no attempt to record — and the log's
/// `outcome` vocabulary has no word for "the request was malformed". Asserted
/// rather than left implicit because the natural way to satisfy the two tests
/// above is a blanket "log everything that leaves this method", which would
/// fill the one table an operator reads during an outage with rows about
/// unconfigured tenants.
#[tokio::test]
async fn a_test_send_refused_before_any_port_writes_no_row() {
    // Both channels default to disabled, so `send_test` refuses with a
    // `Validation` before touching either port.
    let f = fixture_with(
        Arc::new(FakeSlack::default()),
        NotificationConfig::default(),
    )
    .await;

    f.service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect_err("no channel is enabled");

    assert_eq!(f.slack.sends(), 0, "nothing was attempted");
    assert!(
        f.log().await.iter().all(|r| r.event_type != "test"),
        "a refusal that never reached a port must not be audited as a send attempt"
    );
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

/// **The bug this task fixes.** A fresh tenant's default config has both
/// `slack_enabled` and `email_enabled` `false` — [`NotificationConfig::default`]
/// — so a generic test send used to fall through both `if`s straight to
/// `Ok(())`, telling an operator testing a channel that it worked when
/// nothing was sent at all. `send_test`'s own doc contradicts exactly that:
/// "an operator explicitly testing a channel deserves to be told it does not
/// work, not a silent success."
#[tokio::test]
async fn a_generic_test_send_with_no_channel_enabled_is_a_validation_error() {
    let slack = Arc::new(FakeSlack::default());
    let f = build(Arc::new(TenantScopedAuthZ), Arc::clone(&slack)).await;

    let err = f
        .service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect_err("a fresh tenant with no channel enabled must not report success");
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "notification_channels"),
        "{err:?}"
    );
    assert_eq!(
        slack.sends(),
        0,
        "nothing must be attempted, not just nothing sent"
    );
}

/// The same fall-through, reached a different way: `slack_enabled` is `true`
/// but the webhook reference is empty (`slack_capable` is `false`), and email
/// is left disabled — so, exactly as the all-disabled case above, neither
/// channel is ever attempted. `send_test`'s contract does not distinguish
/// "disabled" from "enabled but not configured": both mean nothing was sent.
#[tokio::test]
async fn a_generic_test_send_with_slack_enabled_but_no_webhook_is_a_validation_error() {
    let slack = Arc::new(FakeSlack::default());
    let f = fixture_with(
        Arc::clone(&slack),
        NotificationConfig {
            slack_enabled: true,
            ..NotificationConfig::default()
        },
    )
    .await;

    let err = f
        .service
        .send_test(&f.ctx, TestSend::Generic)
        .await
        .expect_err("an enabled but unconfigured channel must not report success either");
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "notification_channels"),
        "{err:?}"
    );
    assert_eq!(slack.sends(), 0);
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
/// `gts.cf.qa.insights.notification_config.v1~/get`.
///
/// Four shapes, one per way the check can be reached: the webhook URL itself
/// (slashes and a colon-free host, so the charset is what rejects it), the
/// `credstore://` spelling this crate's own fixtures used to carry, a bare
/// path, and the `cred://` spelling — a `cred://` prefix is refused whatever
/// follows it. The stored document is read back
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
        "cred://slack-hook",
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

/// The SMTP credential pair is all-or-nothing on the write path.
///
/// Either half alone is a configuration an operator meant to finish, and
/// `mail_credentials` reads a half-filled pair as "no credential" — so without
/// this check the mail would go out unauthenticated and nothing would say so.
/// Both directions are exercised, because a check written as one `if` on the
/// username would pass the first case and miss the second.
#[tokio::test]
async fn a_half_filled_smtp_credential_is_refused() {
    let f = fixture().await;

    for (username, reference) in [("qa@example.com", ""), ("", "qa-smtp-password")] {
        let err = f
            .service
            .save_config(
                &f.ctx,
                NotificationConfig {
                    email_smtp_username: username.to_owned(),
                    email_smtp_credstore_ref: reference.to_owned(),
                    ..slack_only_config()
                },
            )
            .await
            .expect_err("half a credential must not be stored");
        assert!(
            matches!(&err, DomainError::Validation { field, .. }
                if field == "email_smtp_credstore_ref"),
            "({username:?}, {reference:?}) must be refused naming the reference field, got {err:?}",
        );
    }
}

/// The SMTP reference gets the same syntax rule as the Slack one, at the same
/// place: a value `SecretRef::new` would reject can never resolve, and refusing
/// it at the `PUT` is the difference between an operator seeing it on the
/// settings page and seeing a failed notification hours later.
///
/// A *password-shaped* candidate is among them on purpose — an operator who
/// pastes the password itself into the reference box is the mistake this field
/// name exists to prevent, and `p@ssw0rd!` is refused by the charset.
#[tokio::test]
async fn an_unresolvable_smtp_reference_is_refused() {
    let f = fixture().await;

    for candidate in [
        "smtp/password",
        "cred://smtp/password",
        "cred://smtp-password",
        "p@ssw0rd!",
    ] {
        let err = f
            .service
            .save_config(
                &f.ctx,
                NotificationConfig {
                    email_smtp_username: "qa@example.com".to_owned(),
                    email_smtp_credstore_ref: candidate.to_owned(),
                    ..slack_only_config()
                },
            )
            .await
            .expect_err("a reference the credential store cannot resolve must not be stored");
        assert!(
            matches!(&err, DomainError::Validation { field, .. }
                if field == "email_smtp_credstore_ref"),
            "{candidate:?} must be refused naming its own field, got {err:?}",
        );
    }

    assert!(
        f.service
            .get_config(&f.ctx)
            .await
            .unwrap()
            .email_smtp_credstore_ref
            .is_empty(),
        "no refused candidate may have reached the stored document",
    );
}

/// Both halves empty is the normal shape for an unauthenticated relay — and the
/// state of every row written before the credential columns existed — so it
/// must round-trip rather than be caught by the all-or-nothing rule above.
#[tokio::test]
async fn an_empty_smtp_credential_pair_round_trips() {
    let f = fixture().await;
    let unauthenticated = NotificationConfig {
        email_enabled: true,
        email_smtp_host: "smtp.example.com".to_owned(),
        email_from: "qa@example.com".to_owned(),
        email_recipients: "ops@example.com".to_owned(),
        ..slack_only_config()
    };

    assert_eq!(
        f.service
            .save_config(&f.ctx, unauthenticated.clone())
            .await
            .expect("an unauthenticated relay is a legal configuration"),
        unauthenticated,
    );
    let stored = f.service.get_config(&f.ctx).await.unwrap();
    assert!(stored.email_smtp_username.is_empty());
    assert!(stored.email_smtp_credstore_ref.is_empty());
}

/// The reference survives a settings round-trip, and the surface never grows a
/// password field to leak one through: `GET` answers with the reference it was
/// given, exactly as it does for the Slack webhook.
#[tokio::test]
async fn a_stored_smtp_reference_is_returned_by_the_get_surface() {
    let f = fixture().await;
    let authenticated = NotificationConfig {
        email_enabled: true,
        email_smtp_host: "smtp.example.com".to_owned(),
        email_smtp_username: "qa@example.com".to_owned(),
        email_smtp_credstore_ref: "qa-smtp-password".to_owned(),
        email_from: "qa@example.com".to_owned(),
        email_recipients: "ops@example.com".to_owned(),
        ..slack_only_config()
    };

    f.service
        .save_config(&f.ctx, authenticated)
        .await
        .expect("a complete credential is a legal configuration");

    let stored = f.service.get_config(&f.ctx).await.unwrap();
    assert_eq!(stored.email_smtp_username, "qa@example.com");
    assert_eq!(stored.email_smtp_credstore_ref, "qa-smtp-password");
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
        slack_webhook_credstore_ref: "slack-other".to_owned(),
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
// The explicit-`tenant_id` rule: NotifyRepository::get_config's tenant
// predicate
// ---------------------------------------------------------------------------

/// **The explicit-`tenant_id` rule, closed by this task.** Only the *other*
/// tenant has a stored row, and the caller is `TENANT`. A scope spanning both
/// (the shape a parent-tenant grant compiles to) makes an unpinned `.one()`
/// return whichever row it finds first — on `SQLite` with one row in the table,
/// that is deterministically the other tenant's. Removing the `tenant_id`
/// predicate from `NotifyRepository::get_config` turns this red. Mirrors
/// `jira_tests::a_multi_tenant_scope_does_not_carry_another_tenants_credential_reference`,
/// separated by tenant id rather than by a data field — this resource has no
/// natural "day" axis to key rows apart by, unlike the two-tenant tests Task 35
/// introduced for the results tables.
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

/// **The explicit-`tenant_id` rule, fix round 3.** A scope spanning both
/// tenants must not let `list_log` return the other tenant's audit rows.
/// Separated by a data field (`detail`'s text) rather than by which UUID sorts
/// larger — Task 35's construction, and the reason: an id-ordering trick would
/// pass by accident on the half of the id space where it does not matter.
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
/// and `get` must have been asked exactly twice — the already-decided read
/// that opens `notify_run_completed` (finding 6) and the config read further
/// down. A regression that moved the claim path back to `get` fails the second
/// even if it somehow satisfied the first.
///
/// **The count was one until the already-decided read landed**, and it is
/// raised here rather than loosened to "at least one": the number is the
/// assertion. A third `get` on this path would mean another read was added
/// without anyone deciding it belonged.
#[tokio::test]
async fn the_claim_and_release_path_authorizes_a_write_not_a_read() {
    let authz = Arc::new(RecordingAuthZ::default());
    let f = build(
        Arc::clone(&authz) as Arc<dyn AuthZResolverApi>,
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
    run.finished_at = Some(after_cutoff(&f));
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
        2,
        "only the already-decided read and the config read may be a `get` on this path: \
         {during:?}",
    );
}
