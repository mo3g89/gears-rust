//! Tests for the notification routing core (Task 36 brief, Step 0/1).
//!
//! The first three are the brief's own tests, adapted where the brief's
//! signature did not survive contact with the code:
//!
//! * `the_dedupe_key_is_run_kind_and_event` types its second argument
//!   [`NotificationKind`], not `Channel`. R92 (this module's header, "Dedupe:
//!   `NotificationKind`, not `Channel`") is why: every `notification_kind`
//!   legacy ever writes fuses a family and a channel into one string, and
//!   typing the parameter as a bare channel would let two different families
//!   sharing a channel collide on one claim slot. The property the brief pins
//!   — two different kinds over the same run and event must not collide — is
//!   unchanged.
//!
//! The fourth is the brief's explicit fourth requirement: one row per config
//! field found in Step 0, not one function per field.
//!
//! Mutation evidence for two representative rows (`slack_enabled` gating
//! `Queued`, and `run_queue_queued_slack_enabled` gating `Queued` but not
//! `QueueExpired`) is recorded in the Task 36 report rather than committed as
//! code, per the verification gate.
//!
//! # R94 fix round: `the_expired_queue_event_routes_with_every_toggle_off`
//!
//! This test's *name* and *pinned property* are unchanged from the brief:
//! `Expired` has no per-event toggle, so a config with every per-event toggle
//! off must still route it. Its *fixture* changed. The first implementation
//! read the controller's dispatch ("must go red against an implementation
//! that gates `Expired` on any config flag at all") literally and used a
//! config with `slack_enabled` off too — which was wrong, per legacy's own
//! doc comment (`notifications.rs:654-656`, quoted in `routing.rs`'s header):
//! "no enable flag" is about the missing **per-event** toggle, not the
//! tenant's master Slack switch. The fixture is now "every per-event toggle
//! off, Slack egress on"
//! ([`every_event_toggle_off_slack_on_config`]), and a companion test,
//! [`the_expired_queue_event_does_not_route_when_slack_is_disabled`], pins the
//! other half: `Expired` *does* respect `slack_enabled`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use uuid::Uuid;

use qa_insights_sdk::{NotificationConfig, ScheduledRunSlackTemplate, ScheduledRunSlackTemplates};
use qa_runs_sdk::ScheduleNotificationSettings;

use super::{Event, NotificationKind, dedupe_key, route};

fn run_id() -> Uuid {
    Uuid::new_v4()
}

/// Every *per-event* toggle off — including
/// [`NotificationConfig::notify_on_failure`], legacy's only gate that defaults
/// on — but the tenant's master Slack switch on. R94: this is deliberately
/// not "every field off", because `slack_enabled` is not a per-event toggle
/// and must stay on for this fixture to isolate what it's testing.
fn every_event_toggle_off_slack_on_config() -> NotificationConfig {
    NotificationConfig {
        slack_enabled: true,
        notify_on_failure: false,
        ..NotificationConfig::default()
    }
}

/// Legacy's actual defaults (`manager/src/models.rs:1364-1384`): every gate
/// off except `notify_on_failure`.
fn default_config() -> NotificationConfig {
    NotificationConfig::default()
}

/// A schedule that has never had its notification settings touched:
/// `slack_enabled: false`, no channel override, no event narrowing.
fn no_schedule_settings() -> ScheduleNotificationSettings {
    ScheduleNotificationSettings::default()
}

fn enabled_template() -> ScheduledRunSlackTemplate {
    ScheduledRunSlackTemplate {
        enabled: true,
        ..ScheduledRunSlackTemplate::default()
    }
}

/// Every routing gate on, so a table row can flip exactly one field off and
/// attribute any change in the decision to that field alone.
fn fully_enabled_config() -> NotificationConfig {
    let template = enabled_template();
    NotificationConfig {
        slack_webhook_credstore_ref: "credstore:slack-webhook".to_owned(),
        slack_channel: "#qa-alerts".to_owned(),
        manager_ui_base_url: "https://qa.example.test".to_owned(),
        slack_enabled: true,
        notify_on_failure: true,
        notify_on_success: true,
        notify_on_schedule_completion: true,
        scheduled_run_slack_enabled: true,
        scheduled_run_slack_templates: ScheduledRunSlackTemplates {
            pending: template.clone(),
            in_progress: template.clone(),
            succeeded: template.clone(),
            failed: template.clone(),
            error: template.clone(),
            skipped: template,
        },
        run_queue_queued_slack_enabled: true,
        email_smtp_host: "smtp.example.test".to_owned(),
        email_smtp_port: 587,
        email_from: "qa-insights@example.test".to_owned(),
        email_recipients: "oncall@example.test".to_owned(),
        email_enabled: true,
    }
}

fn fully_enabled_schedule() -> ScheduleNotificationSettings {
    ScheduleNotificationSettings {
        slack_enabled: true,
        slack_channel: None,
        slack_events: Vec::new(),
    }
}

/// `QueueNotificationEvent::Expired` is mandatory and un-toggleable — the
/// model comment calls it "the event that stops a run vanishing silently".
/// This module's header ("`Expired` has no per-event toggle, but it still
/// respects the master switch (R94)") records that legacy's own doc
/// (`notifications.rs:654-656`) means *no per-event toggle*, not *no gate at
/// all*: a config with every **per-event** toggle off, and Slack egress on,
/// must still route it.
#[test]
fn the_expired_queue_event_routes_with_every_toggle_off() {
    let decision = route(
        &every_event_toggle_off_slack_on_config(),
        &Event::QueueExpired,
        &no_schedule_settings(),
    );
    assert!(decision.sends_slack(), "expired has no per-event toggle");
}

/// The other half of R94: `Expired` has no per-event toggle, but it is not
/// toggle-immune outright — it still respects the tenant's master Slack
/// switch, exactly as legacy's shared gate does
/// (`notifications.rs:694-704`). Without this test, reintroducing the R94
/// divergence (bypassing `slack_enabled` too) would not be caught.
#[test]
fn the_expired_queue_event_does_not_route_when_slack_is_disabled() {
    let config = NotificationConfig {
        slack_enabled: false,
        run_queue_queued_slack_enabled: true,
        ..NotificationConfig::default()
    };
    let decision = route(&config, &Event::QueueExpired, &no_schedule_settings());
    assert!(
        !decision.sends_slack(),
        "expired still respects the master Slack switch"
    );
}

/// R94a: legacy audits every "did not send" outcome from `notify_queue_event`
/// except one. `Queued` skipped only because
/// `run_queue_queued_slack_enabled` is off (`notifications.rs:685-692`) is
/// deliberately silent — the function's own doc comment
/// (`notifications.rs:645-652`) explains why: that is the default state,
/// already visible in Settings, and auditing it would put a row on every
/// admission and bury the rows that matter in a log with no retention sweep.
/// The shared `slack_enabled` skip (`notifications.rs:694-704`) *is* audited.
/// Because the `Queued`-only check runs first, a `Queued` skip is silent
/// whenever the per-event toggle is off, regardless of `slack_enabled`.
#[test]
fn the_queued_toggle_off_skip_is_silent_but_the_slack_disabled_skip_is_audited() {
    let toggle_off_only = NotificationConfig {
        slack_enabled: true,
        run_queue_queued_slack_enabled: false,
        ..NotificationConfig::default()
    };
    let toggle_off_decision = route(&toggle_off_only, &Event::Queued, &no_schedule_settings());
    assert!(!toggle_off_decision.sends_slack());
    assert!(
        !toggle_off_decision.slack_skip_is_audited(),
        "the default-state, per-event-toggle skip is silent"
    );

    let slack_disabled = NotificationConfig {
        slack_enabled: false,
        run_queue_queued_slack_enabled: true,
        ..NotificationConfig::default()
    };
    let slack_disabled_decision = route(&slack_disabled, &Event::Queued, &no_schedule_settings());
    assert!(!slack_disabled_decision.sends_slack());
    assert!(
        slack_disabled_decision.slack_skip_is_audited(),
        "the shared slack-disabled skip is audited"
    );

    // Even with both off at once, the toggle-off check runs first in legacy
    // and this module preserves that precedence: still silent.
    let both_off = NotificationConfig {
        slack_enabled: false,
        run_queue_queued_slack_enabled: false,
        ..NotificationConfig::default()
    };
    let both_off_decision = route(&both_off, &Event::Queued, &no_schedule_settings());
    assert!(!both_off_decision.sends_slack());
    assert!(
        !both_off_decision.slack_skip_is_audited(),
        "the per-event toggle check is checked first, so this stays silent"
    );
}

/// R96 (Task 36 review, Important): `RunCompleted` has a silent skip too, and
/// an earlier draft of `routing.rs`'s R94a section wrongly claimed it did
/// not. `notify_run_completed`'s Slack dispatch
/// (`notifications.rs:263-322`) is an `if`/`else if`/`else if` chain with no
/// final `else`, and all three arms require `config.slack_enabled`. When the
/// schedule-level gate passed (`schedule.slack_enabled == true`) but
/// `config.slack_enabled` is `false`, none of the three arms match and no
/// `log_notification` call happens — the reachable state this test pins.
#[test]
fn run_completed_is_silent_when_the_schedule_passed_but_slack_is_disabled() {
    let config = NotificationConfig {
        slack_enabled: false,
        ..fully_enabled_config()
    };
    let schedule = ScheduleNotificationSettings {
        slack_enabled: true,
        ..fully_enabled_schedule()
    };
    let decision = route(&config, &Event::RunCompleted, &schedule);
    assert!(!decision.sends_slack());
    assert!(
        !decision.slack_skip_is_audited(),
        "the schedule gate passed, so this reaches the else-if chain and logs nothing"
    );
}

/// The other reachable "did not send" state for `RunCompleted`: the
/// schedule-level gate itself is what blocked it
/// (`schedule.slack_enabled == false`). Legacy's own early return for that
/// (`notifications.rs:208-219`) logs unconditionally, before
/// `config.slack_enabled` is ever consulted — so this one is audited even
/// though `config.slack_enabled` is also off here.
#[test]
fn run_completed_is_audited_when_the_schedule_gate_itself_blocked_it() {
    let config = NotificationConfig {
        slack_enabled: false,
        ..fully_enabled_config()
    };
    let schedule = ScheduleNotificationSettings {
        slack_enabled: false,
        ..fully_enabled_schedule()
    };
    let decision = route(&config, &Event::RunCompleted, &schedule);
    assert!(!decision.sends_slack());
    assert!(
        decision.slack_skip_is_audited(),
        "the schedule-level gate's own early return always logs in legacy"
    );
}

/// `Queued` is opt-in and off by default (`run_queue_queued_slack_enabled`).
#[test]
fn the_queued_event_is_off_by_default() {
    assert!(!route(&default_config(), &Event::Queued, &no_schedule_settings()).sends_slack());
}

/// The dedupe key is `(run, kind, event)` (`001_initial.sql:197-203`). Two
/// instances racing on the same run must produce exactly one claim slot, and
/// two different kinds over the same run and event must produce two
/// different ones. See this file's header for why the second argument is
/// [`NotificationKind`] and not the brief's `Channel`.
#[test]
fn the_dedupe_key_is_run_kind_and_event() {
    let run = run_id();
    assert_eq!(
        dedupe_key(
            run,
            NotificationKind::RunCompletedSlack,
            &Event::RunCompleted
        ),
        dedupe_key(
            run,
            NotificationKind::RunCompletedSlack,
            &Event::RunCompleted
        )
    );
    assert_ne!(
        dedupe_key(
            run,
            NotificationKind::RunCompletedSlack,
            &Event::RunCompleted
        ),
        dedupe_key(
            run,
            NotificationKind::RunCompletedEmail,
            &Event::RunCompleted
        )
    );
}

/// One row per `NotificationConfig` field (fifteen fields, Step 0) rather than
/// fifteen functions. Each row flips exactly one field from
/// [`fully_enabled_config`]/[`fully_enabled_schedule`] and pins **both**
/// halves of the property: what the fully-enabled baseline decides (so a
/// row's "no effect" is proven against a known-on starting point, not just
/// asserted) and what flipping the field changes it to. Every expectation is
/// a literal `bool`, never derived by calling [`route`] a second time and
/// comparing outputs to each other — that construction is exactly the
/// vacuous-table trap: it would pass against an implementation that always
/// returns the same [`Decision`] regardless of input, since baseline and
/// mutated would trivially agree. This shape was verified to reject that
/// implementation (Task 36 report, RED evidence).
///
/// `slack_enabled` (row 4) and `run_queue_queued_slack_enabled` (row 10) are
/// this table's two mutation-proof rows: the Task 36 report records reverting
/// each field's condition in [`route`] and observing this test go red on
/// exactly that row.
#[test]
fn every_notification_config_field_gates_what_step_0_found() {
    struct Row {
        field: &'static str,
        event: Event,
        baseline: (bool, bool),
        mutated: (bool, bool),
    }

    let baseline_schedule = fully_enabled_schedule();
    let succeeded = Event::scheduled_run("succeeded").expect("succeeded is a valid token");

    // Fully enabled baseline per event, verified once here rather than
    // rederived per row: Queued and ScheduledRun("succeeded") both send Slack
    // only; RunCompleted sends both channels.
    let queued_baseline = (true, false);
    let run_completed_baseline = (true, true);
    let scheduled_run_baseline = (true, false);

    let rows = vec![
        // 1. Data only: no bearing on whether Queued sends (policy vs
        //    capability — this module's header).
        Row {
            field: "slack_webhook_credstore_ref",
            event: Event::Queued,
            baseline: queued_baseline,
            mutated: queued_baseline,
        },
        // 2. Data only.
        Row {
            field: "slack_channel",
            event: Event::Queued,
            baseline: queued_baseline,
            mutated: queued_baseline,
        },
        // 3. Data only.
        Row {
            field: "manager_ui_base_url",
            event: Event::Queued,
            baseline: queued_baseline,
            mutated: queued_baseline,
        },
        // 4. Mutation-proof: the master Slack switch gates Queued.
        Row {
            field: "slack_enabled",
            event: Event::Queued,
            baseline: queued_baseline,
            mutated: (false, false),
        },
        // 5. Dead in legacy (Step 0): no effect on RunCompleted either.
        Row {
            field: "notify_on_failure",
            event: Event::RunCompleted,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 6. Dead in legacy.
        Row {
            field: "notify_on_success",
            event: Event::RunCompleted,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 7. Dead in legacy.
        Row {
            field: "notify_on_schedule_completion",
            event: Event::RunCompleted,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 8. Gates the scheduled-run Slack template path specifically.
        Row {
            field: "scheduled_run_slack_enabled",
            event: succeeded.clone(),
            baseline: scheduled_run_baseline,
            mutated: (false, false),
        },
        // 9. Per-event template `.enabled`; flipping "succeeded"'s alone.
        Row {
            field: "scheduled_run_slack_templates",
            event: succeeded,
            baseline: scheduled_run_baseline,
            mutated: (false, false),
        },
        // 10. Mutation-proof: gates Queued, contrasted with QueueExpired's
        //     immunity to every toggle (the "one with teeth" test above).
        Row {
            field: "run_queue_queued_slack_enabled",
            event: Event::Queued,
            baseline: queued_baseline,
            mutated: (false, false),
        },
        // 11. Data only (capability, not policy — see this module's header).
        Row {
            field: "email_smtp_host",
            event: Event::RunCompleted,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 12. Data only.
        Row {
            field: "email_smtp_port",
            event: Event::RunCompleted,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 13. Data only.
        Row {
            field: "email_from",
            event: Event::RunCompleted,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 14. Data only.
        Row {
            field: "email_recipients",
            event: Event::RunCompleted,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 15. The master email switch gates RunCompleted's email side.
        Row {
            field: "email_enabled",
            event: Event::RunCompleted,
            baseline: run_completed_baseline,
            mutated: (true, false),
        },
    ];

    assert_eq!(rows.len(), 15, "one row per NotificationConfig field");

    for row in rows {
        let baseline_decision = route(&fully_enabled_config(), &row.event, &baseline_schedule);
        assert_eq!(
            (
                baseline_decision.sends_slack(),
                baseline_decision.sends_email()
            ),
            row.baseline,
            "field {}: fully-enabled baseline",
            row.field
        );

        let mutated_config = mutate(row.field);
        let mutated_decision = route(&mutated_config, &row.event, &baseline_schedule);
        assert_eq!(
            (
                mutated_decision.sends_slack(),
                mutated_decision.sends_email()
            ),
            row.mutated,
            "field {}: after mutation",
            row.field
        );
    }
}

/// Turn off (or blank) exactly one field of [`fully_enabled_config`], named by
/// its `NotificationConfig` identifier, and leave every other field on.
fn mutate(field: &str) -> NotificationConfig {
    let mut config = fully_enabled_config();
    match field {
        "slack_webhook_credstore_ref" => config.slack_webhook_credstore_ref = String::new(),
        "slack_channel" => config.slack_channel = String::new(),
        "manager_ui_base_url" => config.manager_ui_base_url = String::new(),
        "slack_enabled" => config.slack_enabled = false,
        "notify_on_failure" => config.notify_on_failure = false,
        "notify_on_success" => config.notify_on_success = false,
        "notify_on_schedule_completion" => config.notify_on_schedule_completion = false,
        "scheduled_run_slack_enabled" => config.scheduled_run_slack_enabled = false,
        "scheduled_run_slack_templates" => {
            config.scheduled_run_slack_templates.succeeded = ScheduledRunSlackTemplate::default();
        }
        "run_queue_queued_slack_enabled" => config.run_queue_queued_slack_enabled = false,
        "email_smtp_host" => config.email_smtp_host = String::new(),
        "email_smtp_port" => config.email_smtp_port = 0,
        "email_from" => config.email_from = String::new(),
        "email_recipients" => config.email_recipients = String::new(),
        "email_enabled" => config.email_enabled = false,
        other => panic!("unknown NotificationConfig field in table: {other}"),
    }
    config
}
