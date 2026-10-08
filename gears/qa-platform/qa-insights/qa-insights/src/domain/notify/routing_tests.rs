//! Tests for the notification routing core (Task 36 brief, Step 0/1).
//!
//! # Four tests were deleted with the events they covered
//!
//! `the_expired_queue_event_routes_with_every_toggle_off`,
//! `the_expired_queue_event_does_not_route_when_slack_is_disabled`,
//! `the_queued_toggle_off_skip_is_silent_but_the_slack_disabled_skip_is_audited`
//! and `the_queued_event_is_off_by_default` pinned `Event::Queued` and
//! `Event::QueueExpired`, which the owner's ruling deleted for want of any
//! producer (`routing.rs`'s header). They are deleted rather than adapted: a
//! test that keeps passing over deleted-adjacent code is worse than no test,
//! because it reports coverage of a decision nothing makes any more. The
//! *reasoning* they carried survives where it still applies — the
//! audited/silent split (`routing.rs`'s header, "Slack skips: audited or
//! silent") is now pinned on `RunCompleted`'s own two skips, by the two tests
//! that always pinned them.
//!
//! [`every_notification_config_field_gates_what_step_0_found`] lost its Queued
//! and `ScheduledRun` rows the same way; its own doc records what that costs.
//!
//! # The 2026-09-29 ruling's three behaviours
//!
//! `notify_on_failure` and `notify_on_success` stopped being inert, email
//! stopped reading the schedule's Slack flag, and an ad-hoc run stopped being
//! silent (`routing.rs`'s header carries the ruling). Two rows of the table
//! above became positive claims as a result, and the four tests under "The
//! owner's 2026-09-29 ruling" below pin the rest. Each one was mutated
//! against [`route`] before it was trusted: the mutations are named in each
//! test's own doc.
//!
//! # `the_dedupe_key_is_run_kind_and_event`
//!
//! The brief's own test, adapted where the brief's signature did not survive
//! contact with the code: its second argument is [`NotificationKind`], not
//! `Channel`. `routing.rs`'s header, "Dedupe: `NotificationKind`, not
//! `Channel`", is why. The property the brief pins — two different kinds over
//! the same run and event must not collide — is unchanged.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use uuid::Uuid;

use qa_insights_sdk::{NotificationConfig, ScheduledRunSlackTemplate, ScheduledRunSlackTemplates};
use qa_runs_sdk::ScheduleNotificationSettings;

use super::{Event, NotificationKind, RunOutcome, dedupe_key, route};

fn run_id() -> Uuid {
    Uuid::new_v4()
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
        email_smtp_username: "qa-insights@example.test".to_owned(),
        email_smtp_credstore_ref: "qa-smtp-password".to_owned(),
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

/// Task 36 review, Important: `RunCompleted` has a silent skip too, and an
/// earlier draft of `routing.rs`'s "Slack skips: audited or silent" section
/// wrongly claimed it did not. `notify_run_completed`'s Slack dispatch
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
    let decision = route(
        &config,
        &Event::RunCompleted,
        Some(&schedule),
        RunOutcome::Failed,
    );
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
    let decision = route(
        &config,
        &Event::RunCompleted,
        Some(&schedule),
        RunOutcome::Failed,
    );
    assert!(!decision.sends_slack());
    assert!(
        decision.slack_skip_is_audited(),
        "the schedule-level gate's own early return always logs in legacy"
    );
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

/// One row per `NotificationConfig` field (seventeen fields) rather than
/// seventeen functions. Each row flips exactly one field from
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
/// # Every row is `RunCompleted` now, and that is a real loss of reach
///
/// Five rows ran against `Event::Queued` and two against
/// `Event::ScheduledRun("succeeded")`; those events were deleted for want of
/// a producer (`routing.rs`'s header), so their rows moved to the one event
/// that remains rather than being dropped — the field still has to be
/// accounted for, and "this field does not affect the only decision this
/// module makes" is the true statement about it now.
///
/// The consequence is stated rather than hidden: **thirteen of the seventeen
/// rows are negative claims**. `run_queue_queued_slack_enabled`,
/// `scheduled_run_slack_enabled` and `scheduled_run_slack_templates` used to
/// be positive ones and are no longer read by [`route`] at all. Only
/// `scheduled_run_slack_templates` is still read (by `render_scheduled_run`,
/// behind `NotifyService::preview_scheduled_run` and `send_test`, which have
/// their own tests); the other two are read by nothing.
///
/// # The four positive rows, and what each was mutated against
///
/// `slack_enabled` (row 4), `email_enabled` (row 17), `notify_on_failure`
/// (row 5) and `notify_on_success` (row 6) are this table's mutation-proof
/// rows: dropping each field's condition from [`route`]'s one arm was
/// observed to turn this test red on exactly that row. The two outcome rows
/// are also the table's only asymmetric ones — each runs under the
/// [`RunOutcome`] its field speaks about, and each expects `(false, false)`
/// rather than one channel, because the outcome policy sits **above** both
/// channel gates rather than beside them.
#[test]
fn every_notification_config_field_gates_what_step_0_found() {
    struct Row {
        field: &'static str,
        event: Event,
        /// The outcome the row is routed under. Every row but
        /// `notify_on_success`'s runs as a **failing** run, because
        /// `fully_enabled_config`'s `notify_on_failure` is what admits the
        /// baseline; the one row about the success policy has to be a passing
        /// run or it would be asserting that the field it names is ignored.
        outcome: RunOutcome,
        baseline: (bool, bool),
        mutated: (bool, bool),
    }

    let baseline_schedule = fully_enabled_schedule();

    // The fully enabled baseline, verified once here rather than rederived
    // per row: `RunCompleted` sends over both channels.
    let run_completed_baseline = (true, true);

    let rows = vec![
        // 1. Data only: whether a webhook reference is configured is a
        //    capability question, not a policy one — `routing.rs`'s header.
        Row {
            field: "slack_webhook_credstore_ref",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 2. Data only.
        Row {
            field: "slack_channel",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 3. Data only.
        Row {
            field: "manager_ui_base_url",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 4. Mutation-proof: the master Slack switch gates the Slack side,
        //    and only it — the email side is a separate gate, which is what
        //    makes this row's `(false, true)` worth more than a `(false,
        //    false)` would be.
        Row {
            field: "slack_enabled",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: (false, true),
        },
        // 5. Dead in legacy, **live here** since the 2026-09-29 ruling
        //    (`routing.rs`'s header): this run failed, so the failure policy
        //    is what admits it, and turning the policy off silences **both**
        //    channels — the outcome gate sits above the channel gates.
        Row {
            field: "notify_on_failure",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: (false, false),
        },
        // 6. The same, for a run that passed. This row is the reason `Row`
        //    carries an outcome at all: under `RunOutcome::Failed` this field
        //    is not consulted, so a row that did not switch outcomes would
        //    assert the opposite of the property it is named for.
        Row {
            field: "notify_on_success",
            event: Event::RunCompleted,
            outcome: RunOutcome::Succeeded,
            baseline: run_completed_baseline,
            mutated: (false, false),
        },
        // 7. Still dead, and deliberately: see `routing.rs`'s header for why
        //    this one was left out of the ruling.
        Row {
            field: "notify_on_schedule_completion",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 8. Gated `Event::ScheduledRun`, which is deleted. Still read by
        //    the preview and test-send surfaces; no bearing on routing.
        Row {
            field: "scheduled_run_slack_enabled",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 9. Likewise: the per-event template `.enabled`, read when a
        //    template is rendered and never when a run-completed alert is
        //    routed.
        Row {
            field: "scheduled_run_slack_templates",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 10. Gated `Event::Queued`, which is deleted. Read by nothing now —
        //     kept as a settings field, not as a gate.
        Row {
            field: "run_queue_queued_slack_enabled",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 11. Data only (capability, not policy — see this module's header).
        Row {
            field: "email_smtp_host",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 12. Data only.
        Row {
            field: "email_smtp_port",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 12a/12b. Data only, exactly as `email_smtp_port` is: the SMTP
        // credential pair is read by the *adapter*, at send time, and routing
        // has never consulted anything about how the relay is reached.
        Row {
            field: "email_smtp_username",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        Row {
            field: "email_smtp_credstore_ref",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 13. Data only.
        Row {
            field: "email_from",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 14. Data only.
        Row {
            field: "email_recipients",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: run_completed_baseline,
        },
        // 15. The master email switch gates RunCompleted's email side.
        Row {
            field: "email_enabled",
            event: Event::RunCompleted,
            outcome: RunOutcome::Failed,
            baseline: run_completed_baseline,
            mutated: (true, false),
        },
    ];

    assert_eq!(rows.len(), 17, "one row per NotificationConfig field");

    for row in rows {
        let baseline_decision = route(
            &fully_enabled_config(),
            &row.event,
            Some(&baseline_schedule),
            row.outcome,
        );
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
        let mutated_decision = route(
            &mutated_config,
            &row.event,
            Some(&baseline_schedule),
            row.outcome,
        );
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
        "email_smtp_username" => config.email_smtp_username = String::new(),
        "email_smtp_credstore_ref" => config.email_smtp_credstore_ref = String::new(),
        "email_from" => config.email_from = String::new(),
        "email_recipients" => config.email_recipients = String::new(),
        "email_enabled" => config.email_enabled = false,
        other => panic!("unknown NotificationConfig field in table: {other}"),
    }
    config
}

// ---------------------------------------------------------------------------
// The owner's 2026-09-29 ruling: three behaviours that used to be silence
// ---------------------------------------------------------------------------

/// **Email no longer depends on the schedule's Slack flag.** The schedule has
/// notifications off, which used to return before *both* branches
/// (`notifications.rs:209-218`, ported exactly until this ruling), so email
/// was silenced by a flag that does not name it. Slack is still blocked by
/// it — that is the flag doing what it is called.
///
/// Mutated against: restoring `schedule_allows_slack &&` to [`route`]'s
/// `email` term turns this red on the `sends_email` assertion.
#[test]
fn email_sends_when_the_schedules_slack_flag_is_off() {
    let schedule = ScheduleNotificationSettings {
        slack_enabled: false,
        ..fully_enabled_schedule()
    };
    let decision = route(
        &fully_enabled_config(),
        &Event::RunCompleted,
        Some(&schedule),
        RunOutcome::Failed,
    );
    assert!(
        !decision.sends_slack(),
        "the schedule's own Slack flag must still gate Slack"
    );
    assert!(
        decision.sends_email(),
        "a flag named for Slack must not silence mail"
    );
}

/// **An ad-hoc run notifies on the tenant's settings.** `None` is the run
/// that legacy's `is_scheduled_run` gate returned on before consulting
/// anything (`notifications.rs:193-206`); it now means "nothing to narrow
/// with", so the tenant's own two switches decide, and turning them off still
/// silences it.
///
/// Both halves are asserted from one baseline, so the positive half cannot
/// pass by [`route`] ignoring its inputs.
///
/// Mutated against: restoring `schedule.is_some_and(|s| s.slack_enabled)`
/// turns the first assertion red; making `None` mean "send regardless" turns
/// the second red.
#[test]
fn an_ad_hoc_run_routes_on_the_tenants_own_settings() {
    let decision = route(
        &fully_enabled_config(),
        &Event::RunCompleted,
        None,
        RunOutcome::Failed,
    );
    assert_eq!(
        (decision.sends_slack(), decision.sends_email()),
        (true, true),
        "a run no schedule launched must route on the tenant's settings"
    );

    let silent = NotificationConfig {
        slack_enabled: false,
        email_enabled: false,
        ..fully_enabled_config()
    };
    let decision = route(&silent, &Event::RunCompleted, None, RunOutcome::Failed);
    assert_eq!(
        (decision.sends_slack(), decision.sends_email()),
        (false, false),
        "and the tenant's settings must still be able to silence it"
    );
}

/// **`notify_on_failure` and `notify_on_success` decide by outcome.** One
/// table over the four (policy, outcome) combinations that matter, asserted
/// on both channels because the outcome gate sits above both: a failing run
/// is admitted only by the failure policy, a passing run only by the success
/// policy, and each is silent under the other's.
///
/// The expectations are literals rather than a second [`route`] call, for the
/// reason [`every_notification_config_field_gates_what_step_0_found`]'s doc
/// gives: a derived expectation would agree with an implementation that
/// ignored its inputs entirely.
///
/// Mutated against: swapping [`RunOutcome::notifies`]'s two arms — so the
/// failure policy answers for a passing run and vice versa — turns rows 2 and
/// 3 red; hard-coding `outcome_notifies = true` turns rows 2 and 4 red.
#[test]
fn the_outcome_policy_admits_the_outcome_it_names_and_no_other() {
    let rows = [
        // (notify_on_failure, notify_on_success, outcome, sends)
        (true, false, RunOutcome::Failed, true),
        (true, false, RunOutcome::Succeeded, false),
        (false, true, RunOutcome::Failed, false),
        (false, true, RunOutcome::Succeeded, true),
    ];

    for (on_failure, on_success, outcome, sends) in rows {
        let config = NotificationConfig {
            notify_on_failure: on_failure,
            notify_on_success: on_success,
            ..fully_enabled_config()
        };
        let decision = route(
            &config,
            &Event::RunCompleted,
            Some(&fully_enabled_schedule()),
            outcome,
        );
        assert_eq!(
            (decision.sends_slack(), decision.sends_email()),
            (sends, sends),
            "notify_on_failure={on_failure}, notify_on_success={on_success}, {outcome:?}"
        );
        assert!(
            sends || decision.slack_skip_is_audited(),
            "an outcome-policy skip over a channel that is switched on is audited — \
             the silent skip is about an unconfigured channel, not about which gate \
             declined ({outcome:?})"
        );
    }
}

/// A run that neither passed nor failed — no results at all, the shape
/// `domain::service::reconcile`'s header says the sweep re-projects on every
/// tick — is admitted by **either** policy and silenced only when both are
/// off. No policy speaks about it, so the only configuration that stops it is
/// the one that stops everything.
///
/// Mutated against: making [`RunOutcome::Indeterminate`] read
/// `notify_on_failure` alone turns row 2 red; making it unconditionally
/// `true` turns row 4 red.
#[test]
fn a_run_that_neither_passed_nor_failed_needs_only_one_policy() {
    let rows = [
        (true, true, true),
        (false, true, true),
        (true, false, true),
        (false, false, false),
    ];

    for (on_failure, on_success, sends) in rows {
        let config = NotificationConfig {
            notify_on_failure: on_failure,
            notify_on_success: on_success,
            ..fully_enabled_config()
        };
        let decision = route(
            &config,
            &Event::RunCompleted,
            Some(&fully_enabled_schedule()),
            RunOutcome::Indeterminate,
        );
        assert_eq!(
            (decision.sends_slack(), decision.sends_email()),
            (sends, sends),
            "notify_on_failure={on_failure}, notify_on_success={on_success}, indeterminate"
        );
    }
}
