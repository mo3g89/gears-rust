//! Which config field gates which notification event.
//!
//! Four events, ported from three legacy functions (Step 0, Task 36 brief):
//! [`Event::Queued`] and [`Event::QueueExpired`] from `notify_queue_event`
//! (`manager/src/services/notifications.rs:657-745`); [`Event::RunCompleted`]
//! from the generic (non-templated) alert inside `notify_run_completed`
//! (`notifications.rs:180-351`); [`Event::ScheduledRun`] from
//! `notify_scheduled_run_status` (`notifications.rs:431-538`), one of the six
//! tokens in `qa_runs_sdk::SLACK_NOTIFICATION_EVENTS` (Task 6's closed set).
//!
//! # Policy versus capability
//!
//! [`route`] answers *should this send, per configured business rules* and
//! deliberately never asks *can it physically send* — whether
//! [`NotificationConfig::slack_webhook_credstore_ref`] resolves to a live
//! webhook, whether `email_smtp_host` accepts a connection, is for whichever
//! task builds the sender, since answering it needs the credential store or a
//! socket and this module has neither. Legacy repeatedly blends the two
//! questions into one `if` (`!config.slack_enabled ||
//! config.slack_webhook_url.trim().is_empty()`, at `notifications.rs:444` and
//! `:694`, with the equivalent affirmative form at `:264`, `:314` and `:366`);
//! here they are two questions asked by two different layers. Concretely: **seven**
//! of the fifteen fields are pure data with no bearing on this module's
//! decision at all — `slack_webhook_credstore_ref`, `slack_channel`,
//! `manager_ui_base_url`, `email_smtp_host`, `email_smtp_port`, `email_from`,
//! `email_recipients` — because either they carry no on/off meaning
//! (`slack_channel`) or the meaning they carry (destination/credential
//! configured) is a capability question, not a policy one.
//!
//! # Three fields are dead in legacy, and stay dead here
//!
//! `notify_on_failure`, `notify_on_success` and `notify_on_schedule_completion`
//! are declared on `NotificationsConfig`, defaulted, round-tripped through the
//! settings API and the UI's default form
//! (`manager-ui/src/pages/notifications/notificationsShared.tsx:93-95`) — and
//! **read nowhere**. A repo-wide search of `manager/src/` for each identifier
//! turns up only the struct definition, its `Default` impl, and one JSON
//! snapshot literal (`models.rs:877-879`); `notify_run_completed`
//! (`notifications.rs:180-351`), the one function whose name suggests it would
//! consult them, never reads any of the three — its actual gates are
//! `is_scheduled_run`, `scheduled_completion_notifications_enabled`,
//! `config.slack_enabled`/`schedule_allows_slack` and `config.email_enabled`
//! plus its own three non-empty checks. The brief's Step 0 asked which flags
//! `notify_run_completed` consults and named these two; **that citation does
//! not survive contact with the code**, so per this plan's Step 0 rule ("if the
//! plan is wrong, the legacy code wins") this module preserves the actual
//! behaviour: all three fields are accepted and ignored, matching the dead code
//! they port.
//!
//! # `Expired` has no per-event toggle, but it still respects the master
//! # switch (R94)
//!
//! Legacy's own doc comment on `notify_queue_event` draws a two-axis
//! distinction that an earlier draft of this module collapsed into one axis:
//!
//! > `Expired` has no enable flag by design: the ticket makes it mandatory. It
//! > is still gated on Slack being configured, because there is nowhere to
//! > send otherwise.
//!
//! (`notifications.rs:654-656`.) "No enable flag" means no **per-event**
//! toggle — there is no `run_queue_expired_slack_enabled` column the way
//! [`Event::Queued`] has [`NotificationConfig::run_queue_queued_slack_enabled`].
//! It does not mean bypassing the tenant's master Slack switch:
//! `config.slack_enabled` still gates it, exactly as the shared check at
//! `notifications.rs:694-704` gates it in legacy (the `Queued`-only toggle
//! check at `:685-692` runs first and does not apply to `Expired` at all,
//! since it is guarded on `event == QueueNotificationEvent::Queued`).
//! [`route`] therefore gates [`Event::QueueExpired`] on `config.slack_enabled`
//! and nothing else — never on `run_queue_queued_slack_enabled` or any other
//! per-event toggle, and never on webhook-emptiness, which stays a capability
//! question per this module's "Policy versus capability" section above.
//! `slack_enabled` is the tenant's own on/off switch, not a fact about whether
//! anywhere exists to send — so it sits on the policy side of that line, not
//! the capability side.
//!
//! # Slack skips: audited or silent (R94a)
//!
//! The same doc comment on `notify_queue_event` (`notifications.rs:645-652`)
//! draws a second distinction, between two "did not send" outcomes: every
//! skip is written to `notification_log` **except one**. `Queued` skipped
//! only because `run_queue_queued_slack_enabled` is off
//! (`notifications.rs:685-692`) is deliberately silent — that is the default
//! state, already visible in Settings, and auditing it would put a row on
//! every admission and bury the rows that matter in a log with no retention
//! sweep. The shared `slack_enabled` skip (`:694-704`) *is* audited, for both
//! `Queued` and `QueueExpired` — and because the `Queued`-only check runs
//! first and returns before the shared check is ever reached, a `Queued` skip
//! is silent whenever the per-event toggle is off, *regardless* of
//! `slack_enabled`'s value. [`Decision::slack_skip_is_audited`] carries this so
//! that whichever task builds the sender (Tasks 37-40) does not have to
//! re-derive it from `config`, or lose the operator-visible skip row
//! entirely.
//!
//! `notify_scheduled_run_status` has no silent case at all: every one of its
//! five early returns (`notifications.rs:444`, `:455`, `:466`, `:477`,
//! `:492`) logs before returning, and so do its terminal send-failure
//! (`:518-525`) and send-success (`:529-536`) paths — verified by reading
//! every `log_notification` call in the function, seven in total. So
//! [`Event::ScheduledRun`]'s [`Decision::slack_skip_is_audited`] is `true`
//! unconditionally.
//!
//! `notify_run_completed` does **not** share that property, and an earlier
//! draft of this section wrongly said it did. Its Slack dispatch
//! (`notifications.rs:263-322`) is an `if`/`else if`/`else if` chain with
//! **no final `else`**, and all three arms require `config.slack_enabled &&
//! !webhook.is_empty()`. When `scheduled_completion_notifications_enabled`
//! is `true` (the `:208-219` early return, which *is* audited — it logs
//! before returning, independent of `config.slack_enabled`) but
//! `config.slack_enabled` is `false`, none of the three arms match and
//! **no `log_notification` call happens at all**: a second silent skip,
//! structurally the same shape as `Queued`'s. [`Event::RunCompleted`]'s
//! [`Decision::slack_skip_is_audited`] therefore mirrors both facts: audited
//! whenever the schedule-level gate is what blocked it (that early return
//! always logs, regardless of `config.slack_enabled`), and silent only in
//! the specific reachable state where the schedule-level gate passed but
//! `config.slack_enabled` is `false`.
//!
//! # Dedupe: `NotificationKind`, not `Channel` (R92)
//!
//! Every `notification_kind` value legacy ever writes is
//! `SCHEDULED_RUN_SLACK_NOTIFICATION_KIND = "scheduled_run_slack"`
//! (`notifications.rs:14`, bound at `:555` and `:580`) — legacy dedupes
//! scheduled-run Slack alerts and nothing else; `notify_queue_event` and the
//! generic alert in `notify_run_completed` never call
//! `reserve_run_notification` at all. That one constant already fuses a family
//! (`"scheduled_run"`) and a channel (`"slack"`) into one string, which is exactly
//! what [`NotificationClaim::kind`]'s own doc says the column holds: "the
//! notification family". Typing [`dedupe_key`]'s second parameter as a bare
//! `Channel` — the brief's original signature — would let two different
//! families that happen to share a channel collide on one claim slot: a
//! `ScheduledRun` Slack alert and a `RunCompleted` Slack alert for the same run
//! and the same event spelling would both key on plain `Channel::Slack`, and
//! one send would silently suppress the other. [`NotificationKind`] keeps
//! family and channel fused, the way legacy's constant does, so that cannot
//! happen. The brief's pinned property — two different kinds over the same run
//! and event must not collide — is unchanged; only the type honouring it is.
//!
//! # Dedupe event spelling: the closed-set token, not `label()`
//!
//! [`NotificationClaim::event`]'s own doc cites legacy's
//! `ScheduledRunNotificationEvent::label()` (`models.rs:974-983`), which
//! produces display text — `"In progress"`, with a space, Title Case — bound
//! into `event_type` at `reserve_run_notification`
//! (`notifications.rs:556`). This module uses the six
//! `SLACK_NOTIFICATION_EVENTS` tokens instead (`"in_progress"`, `snake_case`) for
//! every event's dedupe spelling, [`Event::ScheduledRun`] included, rather than
//! introduce a second vocabulary solely for this one column. The property
//! `NotificationClaim` needs — the same event always renders the same string,
//! two different events never collide — holds under either spelling; this
//! module picks the one the rest of the routing core (parsing, the per-schedule
//! allow-list) already treats as canonical, so there is exactly one place that
//! spells a scheduled-run event, not two that must be kept in sync.
//!
//! # `RunCompleted`'s per-schedule gate silences email too
//!
//! `notify_run_completed` computes
//! `scheduled_completion_notifications_enabled(run)` once
//! (`notifications.rs:208`, `is_scheduled_run(run) &&
//! run.slack_notifications_enabled.unwrap_or(true)`) and returns before
//! **either** the Slack or the email branch if it is false
//! (`notifications.rs:209-218`) — despite the field's name, it silences email
//! too. [`ScheduleNotificationSettings::slack_enabled`] is this port's
//! surviving name for that one flag (D9), so [`route`] reads it as the general
//! "does this schedule notify on completion at all" switch for
//! [`Event::RunCompleted`], not a Slack-only one.

use qa_insights_sdk::NotificationConfig;
use qa_runs_sdk::{SLACK_NOTIFICATION_EVENTS, ScheduleNotificationSettings};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::repos::NotificationClaim;

/// One thing that can trigger a notification.
///
/// See this module's header for the legacy function each variant ports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A launch was held rather than started. Optional; see
    /// [`NotificationConfig::run_queue_queued_slack_enabled`].
    Queued,
    /// A queued row outlived its TTL and will never start. Mandatory; see
    /// this module's header.
    QueueExpired,
    /// A scheduled run finished: the generic (non-templated) Slack alert and
    /// the email alert, both from `notify_run_completed`.
    RunCompleted,
    /// One of the six scheduled-run statuses. Build with [`Event::scheduled_run`]
    /// rather than the tuple constructor directly, so the token is always a
    /// validated member of [`SLACK_NOTIFICATION_EVENTS`].
    ScheduledRun(String),
}

impl Event {
    /// Validate `token` against [`SLACK_NOTIFICATION_EVENTS`] (Task 6's closed
    /// set, `qa-runs-sdk/src/models.rs:685-691`) and build the event. `None`
    /// for anything outside it — including a case-folded miss:
    /// `"InProgress"` lowercased is `"inprogress"`, which is not a member, and
    /// this deliberately does not fold case to find it (Step 0).
    #[must_use]
    pub fn scheduled_run(token: &str) -> Option<Self> {
        SLACK_NOTIFICATION_EVENTS
            .contains(&token)
            .then(|| Self::ScheduledRun(token.to_owned()))
    }

    /// The dedupe-key spelling for this event. See this module's header,
    /// "Dedupe event spelling".
    fn dedupe_token(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::QueueExpired => "expired",
            Self::RunCompleted => "run_completed",
            Self::ScheduledRun(token) => token,
        }
    }
}

/// The family+channel bundle written into `notification_kind`
/// (`NotificationClaim::kind`). See this module's header, "Dedupe:
/// `NotificationKind`, not `Channel`".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NotificationKind {
    /// Legacy's one and only kind, verbatim:
    /// `SCHEDULED_RUN_SLACK_NOTIFICATION_KIND` (`notifications.rs:14`).
    ScheduledRunSlack,
    /// `RunCompleted`'s Slack side. Legacy never deduped this alert; the
    /// ported repository generalizes the claim table beyond legacy's one use,
    /// per [`NotificationClaim::kind`]'s own "e.g." framing.
    RunCompletedSlack,
    /// `RunCompleted`'s email side.
    RunCompletedEmail,
    /// Either queue event's Slack alert. One kind for both — `Queued` and
    /// `QueueExpired` never collide because [`Event::dedupe_token`] still
    /// differs between them.
    QueueSlack,
}

impl NotificationKind {
    /// The string this kind is stored as, mirroring legacy's one constant for
    /// [`Self::ScheduledRunSlack`] and extending its naming convention
    /// (family, then `_`, then channel) for the rest.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScheduledRunSlack => "scheduled_run_slack",
            Self::RunCompletedSlack => "run_completed_slack",
            Self::RunCompletedEmail => "run_completed_email",
            Self::QueueSlack => "queue_slack",
        }
    }
}

/// The `(run_id, kind, event)` triple [`NotificationClaim`] claims on.
///
/// Same field names, same types, as [`NotificationClaim`]'s `run_id`, `kind`
/// and `event` — not a fresh tuple — so [`Self::into_claim`] is the only way
/// to get from one to the other and there is no second place that could spell
/// either differently (R93: the send-once protocol is the unique index over
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

/// A routing decision: which channel(s), if any, should carry this event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "three independent questions — sends over Slack, sends over \
              email, and whether a Slack non-send is audit-worthy (R94a) —  \
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
    /// `qa_notification_log`, or is one of legacy's deliberately silent
    /// skips. See this module's header, "Slack skips: audited or silent
    /// (R94a)" — there are two: `Queued` skipped by its own toggle, and
    /// `RunCompleted` skipped because `config.slack_enabled` is false while
    /// the schedule-level gate passed. Only meaningful when
    /// [`Self::sends_slack`] is `false`; defined as `true` when it is `true`,
    /// since there is nothing to skip.
    #[must_use]
    pub fn slack_skip_is_audited(self) -> bool {
        self.slack_skip_is_audited
    }
}

/// Decide whether `event` sends, and over which channel(s), given `config`
/// and the settings of the schedule the run belongs to (ignored by
/// [`Event::Queued`] and [`Event::QueueExpired`], which legacy never scopes to
/// a schedule at all).
///
/// See this module's header for the per-event legacy citation and every
/// adaptation this function makes.
#[must_use]
pub fn route(
    config: &NotificationConfig,
    event: &Event,
    schedule: &ScheduleNotificationSettings,
) -> Decision {
    match event {
        Event::Queued => {
            let toggle_on = config.run_queue_queued_slack_enabled;
            Decision {
                slack: config.slack_enabled && toggle_on,
                email: false,
                // Silent whenever the per-event toggle is off, regardless of
                // `slack_enabled` — legacy's `Queued`-only check
                // (`notifications.rs:685-692`) runs first and returns before
                // the shared audited check (`:694-704`) is ever reached.
                slack_skip_is_audited: toggle_on,
            }
        }
        Event::QueueExpired => Decision {
            slack: config.slack_enabled,
            email: false,
            // No per-event toggle exists, so every skip here is the shared,
            // audited one (`notifications.rs:694-704`).
            slack_skip_is_audited: true,
        },
        Event::RunCompleted => {
            let schedule_notifies = schedule.slack_enabled;
            Decision {
                slack: schedule_notifies && config.slack_enabled,
                email: schedule_notifies && config.email_enabled,
                // Audited whenever the schedule-level gate is what blocked
                // it: legacy's own early return for that
                // (`notifications.rs:208-219`) logs unconditionally, before
                // `config.slack_enabled` is ever consulted. Silent only in
                // the one reachable state where the schedule-level gate
                // passed but `config.slack_enabled` is false — see this
                // module's header, "Slack skips: audited or silent (R94a)".
                slack_skip_is_audited: !schedule_notifies || config.slack_enabled,
            }
        }
        Event::ScheduledRun(token) => {
            let event_allowed = schedule.slack_events.is_empty()
                || schedule.slack_events.iter().any(|allowed| allowed == token);
            let template_enabled = config
                .scheduled_run_slack_templates
                .template_for(token)
                .is_some_and(|template| template.enabled);
            Decision {
                slack: schedule.slack_enabled
                    && event_allowed
                    && config.slack_enabled
                    && config.scheduled_run_slack_enabled
                    && template_enabled,
                email: false,
                // Every one of `notify_scheduled_run_status`'s skip paths
                // logs (`notifications.rs:444,455,466,477,492,518-525`) —
                // confirmed independently, not inherited from the review.
                slack_skip_is_audited: true,
            }
        }
    }
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod routing_tests;
