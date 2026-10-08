//! Which config field gates the run-completed notification.
//!
//! One event, [`Event::RunCompleted`], ported from the generic
//! (non-templated) alert inside legacy's `notify_run_completed`
//! (`manager/src/services/notifications.rs:180-351`).
//!
//! # Three events were deleted here, on an owner's ruling
//!
//! This module routed four events through Task 36 and the branch's dead-code
//! triage: `Queued` and `QueueExpired` from `notify_queue_event`
//! (`notifications.rs:657-745`), and `ScheduledRun` from
//! `notify_scheduled_run_status` (`:431-538`). **Nothing ever raised any of
//! them.** The transactional broker consumer that would have was deleted once
//! it was established that no deployment registered the client it needed
//! (`crate::gear`'s header), and no other producer was ever built: the events
//! they answer — `run.queue_expired`, `schedule.fired` — are raised by no gear
//! in this subsystem. Finding #38's triage left them standing under a
//! `#[allow(dead_code)]` and escalated the question; the owner's ruling was to
//! delete them, with the tests that existed only to cover them, rather than
//! keep a suite green over code no caller reaches. Wiring them back is a
//! cross-gear feature with a producer, not a routing arm; the legacy
//! citations above are where that work would start reading.
//!
//! What survives here is what the run-completed path actually consults, and
//! [`NotificationKind`] now holds exactly the two kinds
//! `m20260929_000003_seed_run_completed_notification_claims` seeds.
//!
//! # Policy versus capability
//!
//! [`route`] answers *should this send, per configured business rules* and
//! deliberately never asks *can it physically send* — whether
//! [`NotificationConfig::slack_webhook_credstore_ref`] resolves to a live
//! webhook, whether `email_smtp_host` accepts a connection, is
//! [`crate::domain::service::notify::NotifyService`]'s question, since
//! answering it needs the credential store or a socket and this module has
//! neither. Legacy repeatedly blends the two questions into one `if`
//! (`!config.slack_enabled || config.slack_webhook_url.trim().is_empty()`, at
//! `notifications.rs:444` and `:694`, with the equivalent affirmative form at
//! `:264`, `:314` and `:366`); here they are two questions asked by two
//! different layers. Concretely: **seven** of the seventeen fields are pure
//! data with no bearing on this module's decision at all —
//! `slack_webhook_credstore_ref`, `slack_channel`, `manager_ui_base_url`,
//! `email_smtp_host`, `email_smtp_port`, `email_from`, `email_recipients` —
//! because either they carry no on/off meaning (`slack_channel`) or the
//! meaning they carry (destination/credential configured) is a capability
//! question, not a policy one.
//!
//! # Two of those three dead fields are live here, on an owner's ruling
//!
//! `notify_on_failure`, `notify_on_success` and `notify_on_schedule_completion`
//! are declared on `NotificationsConfig`, defaulted, round-tripped through the
//! settings API and the UI's default form
//! (`manager-ui/src/pages/notifications/notificationsShared.tsx:93-95`) — and
//! **read nowhere in legacy**. A repo-wide search of `manager/src/` for each
//! identifier turns up only the struct definition, its `Default` impl, and one
//! JSON snapshot literal (`models.rs:877-879`); `notify_run_completed`
//! (`notifications.rs:180-351`), the one function whose name suggests it would
//! consult them, never reads any of the three — its actual gates are
//! `is_scheduled_run`, `scheduled_completion_notifications_enabled`,
//! `config.slack_enabled`/`schedule_allows_slack` and `config.email_enabled`
//! plus its own three non-empty checks. The brief's Step 0 asked which flags
//! `notify_run_completed` consults and named these two; **that citation does
//! not survive contact with the code**, and this port carried the deadness
//! forward rather than inventing a meaning for it.
//!
//! **That is no longer the behaviour.** On 2026-09-29 the owner ruled that
//! `notify_on_failure` and `notify_on_success` become live — the second of the
//! three product questions the audit closure report left open
//! ("OPEN-NEEDS-OWNER" #2: "Leaving them is free but keeps four columns that
//! lie about what they do"). [`route`] now reads both, through [`RunOutcome`],
//! and that is a **deliberate divergence from legacy**, accepted as such:
//!
//! * a run whose results say it did not pass notifies only if
//!   `notify_on_failure` is set;
//! * one that passed, only if `notify_on_success` is;
//! * one that is neither — no results at all, or only statuses that are
//!   neither passed nor failed — notifies if **either** flag is set, because
//!   no outcome policy has an opinion about it and the alternative would be a
//!   class of run that no configuration an operator can reach turns on.
//!
//! **The direction of the change is not uniform, and the narrowing half is
//! the one to notice.** `NotificationConfig::default` is legacy's own
//! `notify_on_failure` on / `notify_on_success` off, so a deployment that has
//! never touched either field keeps its failure alerts and **does not announce
//! passing runs**, which the routing first shipped on 2026-09-29 (every
//! scheduled run notified, whatever its outcome) did. That is what making the
//! flags mean what they say costs, and it is the ruling's consequence rather
//! than a defect. `qa-platform-ui`'s notification settings page grew the two
//! switches in the same change, so an operator can say otherwise.
//!
//! `notify_on_schedule_completion` is **not** in that ruling and stays as dead
//! as legacy leaves it — accepted, stored, round-tripped, read by nothing.
//! Legacy's `scheduled_completion_notifications_enabled`
//! (`notifications.rs:208`) is a *per-run* computation over the run's own
//! schedule, not this tenant-wide field, so reviving this one would mean
//! inventing a meaning for it rather than finding one.
//!
//! `scheduled_run_slack_enabled` is in the same class as
//! `notify_on_schedule_completion`: stored, round-tripped, read by nothing in `domain` — neither the routing
//! decision (the event it gated is gone) nor the scheduled-run template
//! surfaces. `scheduled_run_slack_templates` is the one that is live:
//! `render::render_scheduled_run` reads it, which is what
//! `NotifyService::preview_scheduled_run` and the `TestSend::ScheduledRun` arm
//! of `NotifyService::send_test` render from. `run_queue_queued_slack_enabled` is
//! read by nothing at all either; both are kept as stored settings fields
//! rather than dropped, because removing a column from a settings document an
//! operator's UI round-trips is a migration and a UI change, not a routing
//! change.
//!
//! # Slack skips: audited or silent
//!
//! Legacy's doc comment on `notify_queue_event` (`notifications.rs:645-652`)
//! draws a distinction between two "did not send" outcomes: every skip is
//! written to `notification_log` except one. That distinction outlives the
//! events it was written about, because `notify_run_completed` has a silent
//! skip of its own.
//!
//! Its Slack dispatch (`notifications.rs:263-322`) is an `if`/`else if`/`else
//! if` chain with **no final `else`**, and all three arms require
//! `config.slack_enabled && !webhook.is_empty()`. When
//! `scheduled_completion_notifications_enabled` is `true` (the `:208-219`
//! early return, which *is* audited — it logs before returning, independent of
//! `config.slack_enabled`) but `config.slack_enabled` is `false`, none of the
//! three arms match and **no `log_notification` call happens at all**.
//! [`Decision::slack_skip_is_audited`] mirrors both facts: audited whenever the
//! schedule-level gate is what blocked it, and silent only in the specific
//! reachable state where the schedule-level gate passed but
//! `config.slack_enabled` is `false`.
//!
//! **The 2026-09-29 outcome gate needed no term in that formula**, which is
//! why it has none. Read as a rule about the *channel* rather than about
//! which gate fired, the formula already says the right thing for a skip the
//! outcome policy made: Slack is on, so an operator who turned
//! `notify_on_success` off and wonders why a green run said nothing finds the
//! row in `qa_notification_log`; Slack is off, so a tenant who does not use
//! the channel gets no row about it, which is the whole point of the silence.
//! An `!outcome_notifies ||` term was written here first and removed: its only
//! effect was to start writing Slack skip rows for tenants with Slack
//! switched off, and no test could tell the two formulas apart on any state
//! anyone wanted. [`an_outcome_policy_skip_claims_its_slot_so_a_later_pass_stays_silent`](crate::domain::service::notify)
//! is what pins the half that is reachable.
//!
//! **Silence is about the audit log and nothing else.** Both skips claim their
//! dedupe slot — see
//! [`NotifyService::decline_run_completed_channel`](crate::domain::service::notify)
//! — because the claim table records what was *decided*, and a decision that
//! was not worth an operator's attention is still a decision.
//!
//! # Dedupe: `NotificationKind`, not `Channel`
//!
//! Every `notification_kind` value legacy ever writes is
//! `SCHEDULED_RUN_SLACK_NOTIFICATION_KIND = "scheduled_run_slack"`
//! (`notifications.rs:14`, bound at `:555` and `:580`) — legacy dedupes
//! scheduled-run Slack alerts and nothing else; the generic alert in
//! `notify_run_completed` never calls `reserve_run_notification` at all. That
//! one constant already fuses a family (`"scheduled_run"`) and a channel
//! (`"slack"`) into one string, which is exactly what [`NotificationClaim::kind`]'s
//! own doc says the column holds: "the notification family". Typing
//! [`dedupe_key`]'s second parameter as a bare `Channel` — the brief's original
//! signature — would let two different families that happen to share a channel
//! collide on one claim slot, and one send would silently suppress the other.
//! [`NotificationKind`] keeps family and channel fused, the way legacy's
//! constant does, so that cannot happen.
//!
//! The type is down to one family now that the scheduled-run and queue events
//! are gone, which makes the distinction invisible in today's values and no
//! less load-bearing: `run_completed_slack` and `run_completed_email` are two
//! claim slots over the same run and the same event token, and a `Channel` is
//! precisely what they would collapse to.
//!
//! # The schedule's Slack flag gates Slack only, and an absent schedule gates
//! # nothing
//!
//! `notify_run_completed` computes
//! `scheduled_completion_notifications_enabled(run)` once
//! (`notifications.rs:208`, `is_scheduled_run(run) &&
//! run.slack_notifications_enabled.unwrap_or(true)`) and returns before
//! **either** the Slack or the email branch if it is false
//! (`notifications.rs:209-218`) — despite the field's name, it silences email
//! too; and because the same expression's left half is `is_scheduled_run`, a
//! run no schedule launched returns there as well and notifies on no channel
//! at all. This module ported both of those exactly, and **the same
//! 2026-09-29 ruling reversed both** (audit closure report,
//! "OPEN-NEEDS-OWNER" #1 and #3):
//!
//! * [`ScheduleNotificationSettings::slack_enabled`] is qa-runs' name for
//!   that flag and is now read as what it is called — the schedule's
//!   **Slack** switch. It gates [`Decision::sends_slack`] and nothing else.
//!   Email is gated by the tenant's own `email_enabled` (and by the outcome
//!   policy), so turning a schedule's Slack alerts off no longer silences a
//!   channel it does not name. Question #3 asked whether email should get a
//!   schedule-level gate of its own; it did not get one, because that would
//!   be a qa-runs schema and SDK change, and a tenant-level gate is what
//!   exists to read.
//! * `schedule` is [`Option`], and `None` — an ad-hoc run, or one whose
//!   schedule could not be resolved — means **there is nothing to narrow
//!   with**, not "this does not notify". Such a run routes on the tenant's
//!   settings alone. Widening this way is safe against a rebuild precisely
//!   because every decline already claims its slot (see
//!   [`NotifyService::decline_run_completed_channel`](crate::domain::service::notify)):
//!   an ad-hoc run this deployment has already considered holds both claims
//!   and cannot be re-announced, and one older than the notification cutoff
//!   is declined by
//!   [`NotifyService::is_history`](crate::domain::service::notify) before
//!   [`route`] is ever called.

use qa_insights_sdk::NotificationConfig;
use qa_runs_sdk::ScheduleNotificationSettings;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::repos::NotificationClaim;

/// One thing that can trigger a notification.
///
/// One variant, and an enum rather than a unit type: the dedupe token and the
/// routing decision are both per-event, and this module's header records three
/// further events that were routed here and deleted for want of a producer. A
/// second event arrives as a variant, not as a second `route` function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A scheduled run finished: the generic (non-templated) Slack alert and
    /// the email alert, both from `notify_run_completed`.
    RunCompleted,
}

impl Event {
    /// The dedupe-key spelling for this event, and the `event_type` column's
    /// value. `m20260929_000003_seed_run_completed_notification_claims` writes
    /// this same literal, which
    /// `the_seeded_kinds_are_the_ones_routing_claims` keeps in step.
    fn dedupe_token(&self) -> &str {
        match self {
            Self::RunCompleted => "run_completed",
        }
    }
}

/// The family+channel bundle written into `notification_kind`
/// (`NotificationClaim::kind`). See this module's header, "Dedupe:
/// `NotificationKind`, not `Channel`".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NotificationKind {
    /// `RunCompleted`'s Slack side. Legacy never deduped this alert; the
    /// ported repository generalizes the claim table beyond legacy's one use,
    /// per [`NotificationClaim::kind`]'s own "e.g." framing.
    RunCompletedSlack,
    /// `RunCompleted`'s email side.
    RunCompletedEmail,
}

impl NotificationKind {
    /// The string this kind is stored as, following legacy's naming convention
    /// for its one constant (family, then `_`, then channel).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunCompletedSlack => "run_completed_slack",
            Self::RunCompletedEmail => "run_completed_email",
        }
    }
}

/// The `(run_id, kind, event)` triple [`NotificationClaim`] claims on.
///
/// Same field names, same types, as [`NotificationClaim`]'s `run_id`, `kind`
/// and `event` — not a fresh tuple — so [`Self::into_claim`] is the only way
/// to get from one to the other and there is no second place that could spell
/// either differently (the send-once protocol is the unique index over
/// exactly this triple, so a key computed any other way could drift from what
/// actually gets claimed).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DedupeKey {
    pub run_id: Uuid,
    pub kind: String,
    pub event: String,
}

impl DedupeKey {
    /// Attach the timestamp a claim only gets once it is actually inserted,
    /// producing the exact [`NotificationClaim`] this key identifies.
    #[must_use]
    pub fn into_claim(self, sent_at: OffsetDateTime) -> NotificationClaim {
        NotificationClaim {
            run_id: self.run_id,
            kind: self.kind,
            event: self.event,
            sent_at,
        }
    }
}

/// The identity of one notification claim slot: same run, same kind, same
/// event always produce the same key; any difference in kind or event
/// produces a different one. Pinned by
/// `the_dedupe_key_is_run_kind_and_event` (`001_initial.sql:197-203`).
#[must_use]
pub fn dedupe_key(run_id: Uuid, kind: NotificationKind, event: &Event) -> DedupeKey {
    DedupeKey {
        run_id,
        kind: kind.as_str().to_owned(),
        event: event.dedupe_token().to_owned(),
    }
}

/// What a finished run's own results say about it — the outcome
/// `notify_on_failure` and `notify_on_success` are policies over.
///
/// # Three values, because the message already has three headlines
///
/// This is deliberately *not* a two-valued pass/fail, and not derived from
/// `qa_runs_sdk::RunState` either. The alert this decision gates already
/// states a verdict in its own first line — `"FAILED"`, `"SUCCEEDED"` or
/// `"COMPLETED"`, chosen by `render::run_completed_headline` from legacy's
/// `notifications.rs:191-192,222-228` — and that verdict is computed from the
/// run's result rows, not from its state column. A gate keyed off anything
/// else could suppress a message whose own headline says `FAILED`, or admit
/// one that says `SUCCEEDED`, while the flag the operator set said the
/// opposite. So the classification has exactly one home,
/// [`render::run_completed_outcome`](super::render::run_completed_outcome),
/// and the headline is derived from *this* enum rather than computed
/// alongside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// At least one result is `FAILED` or `ERROR`. Headline `"FAILED"`.
    Failed,
    /// Every result passed, and there was at least one. Headline
    /// `"SUCCEEDED"`.
    Succeeded,
    /// Neither: no results at all, or only statuses that are neither passed
    /// nor failed (`"PENDING"`, `"RUNNING"`, a lone `"SKIPPED"`). Headline
    /// `"COMPLETED"`.
    ///
    /// Not a rare corner: `domain::service::reconcile`'s header records that
    /// a finished run with zero result rows is re-projected on every sweep
    /// tick, so this is the outcome of the very runs that reach
    /// `notify_run_completed` most often.
    Indeterminate,
}

impl RunOutcome {
    /// Whether the tenant's outcome policy admits a run with this outcome.
    ///
    /// [`Self::Indeterminate`] is admitted by *either* flag — see this
    /// module's header, "Two of those three dead fields are live here": no
    /// policy speaks about a run that neither passed nor failed, so the only
    /// configuration that silences one is the one that silences everything.
    fn notifies(self, config: &NotificationConfig) -> bool {
        match self {
            Self::Failed => config.notify_on_failure,
            Self::Succeeded => config.notify_on_success,
            Self::Indeterminate => config.notify_on_failure || config.notify_on_success,
        }
    }
}

/// A routing decision: which channel(s), if any, should carry this event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "three independent questions — sends over Slack, sends over \
              email, and whether a Slack non-send is audit-worthy —  \
              not flags of one state machine; a state-machine or two-variant-\
              enum refactor would need one axis per bool anyway"
)]
pub struct Decision {
    slack: bool,
    email: bool,
    slack_skip_is_audited: bool,
}

impl Decision {
    #[must_use]
    pub fn sends_slack(self) -> bool {
        self.slack
    }

    #[must_use]
    pub fn sends_email(self) -> bool {
        self.email
    }

    /// Whether a `false` [`Self::sends_slack`] is worth a row in
    /// `qa_notification_log`, or is legacy's deliberately silent skip. See
    /// this module's header, "Slack skips: audited or silent": the
    /// silent one is `RunCompleted` blocked because `config.slack_enabled` is
    /// false while the schedule-level gate passed. Only meaningful when
    /// [`Self::sends_slack`] is `false`; defined as `true` when it is `true`,
    /// since there is nothing to skip.
    ///
    /// It says nothing about the *claim*, which a skip takes either way.
    #[must_use]
    pub fn slack_skip_is_audited(self) -> bool {
        self.slack_skip_is_audited
    }
}

/// Decide whether `event` sends, and over which channel(s), given `config`,
/// the settings of the schedule the run belongs to — `None` for an ad-hoc run
/// or one whose schedule could not be resolved — and what the run's own
/// results say about it.
///
/// See this module's header for the legacy citation, the three 2026-09-29
/// rulings that diverge from it, and every other adaptation this function
/// makes.
///
/// `outcome` is carried as a parameter rather than as data on [`Event`] for a
/// concrete reason: `NotifyService::notify_run_completed` builds the [`Event`]
/// before it has fetched the run, to ask whether every slot is already claimed
/// and stop there. The dedupe token must be knowable without the run; the
/// policy input must not be.
#[must_use]
pub fn route(
    config: &NotificationConfig,
    event: &Event,
    schedule: Option<&ScheduleNotificationSettings>,
    outcome: RunOutcome,
) -> Decision {
    match event {
        Event::RunCompleted => {
            // `None` narrows nothing — see this module's header, "The
            // schedule's Slack flag gates Slack only, and an absent schedule
            // gates nothing". Legacy returned before both channels here; this
            // routes on the tenant's settings instead, per the owner's ruling.
            let schedule_allows_slack = schedule.is_none_or(|schedule| schedule.slack_enabled);
            let outcome_notifies = outcome.notifies(config);
            Decision {
                slack: outcome_notifies && schedule_allows_slack && config.slack_enabled,
                // No schedule-level term at all: the flag above is the
                // schedule's *Slack* switch and says nothing about mail.
                email: outcome_notifies && config.email_enabled,
                // Unchanged by the ruling, and deliberately so. Audited
                // whenever the schedule-level gate is what blocked it: legacy's
                // own early return for that (`notifications.rs:208-219`) logs
                // unconditionally, before `config.slack_enabled` is ever
                // consulted. Silent only in the one reachable state where the
                // schedule-level gate passed but `config.slack_enabled` is
                // false — see this module's header, "Slack skips: audited or
                // silent", which also says why the new outcome gate needs no
                // term here.
                slack_skip_is_audited: !schedule_allows_slack || config.slack_enabled,
            }
        }
    }
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod routing_tests;
