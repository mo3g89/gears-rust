//! Schedule CRUD and firing, against a **real** in-memory `SQLite` database.
//!
//! # Why this suite is DB-backed where most service suites are not
//!
//! The property this task exists to demonstrate —
//! `cpt-cf-qa-nfr-scheduler-exactly-once` — **is a unique index**. A hand-rolled
//! `SchedulesRepository` double would have to reimplement
//! `idx_qa_schedule_ticks_claim` in order for any exactly-once test to pass, and
//! the test would then be asserting that the double honours the constraint the
//! double invented. Dropping the real index would leave such a suite green,
//! which makes it worthless as evidence and worthless as a break-test.
//!
//! So the repositories, the migrations, the `PolicyEnforcer` and the scoping are
//! all production types here. What is doubled is what the launch path reaches
//! *outside* this gear — qa-catalog, qa-environments, the executor and the event
//! sink — plus the admission seam, which is the instrument that counts launches.
//! `tenant_scoping_tests` is the precedent and takes the same shape.
//!
//! # The `AuthZ` double is this file's own, and that is not duplication
//!
//! `admission_tests::fakes::SystemGrantingAuthZ` stamps a resource-type marker
//! onto `RESOURCE_ID`, and its own doc says what that costs: *"handing one to a
//! real `Orm*Repository` would filter out every row … if a future test ever
//! wires `SystemGrantingAuthZ` to an ORM repository, this is why it returns
//! nothing."* This suite is exactly that future test. [`SchedulerAuthZ`] is the
//! same policy shape without the marker.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use qa_runs_sdk::{ExclusiveTier, RunParameter, RunState, RunTarget, ScheduleNotificationSettings};
use time::macros::datetime;

use super::*;
use crate::domain::service::admission::tests::fakes::{PLATFORM_A, REPO};
use crate::domain::service::test_support::{
    Asked, Fleet, OTHER_TENANT, OWNER_TENANT, QueueingAdmitter, SchedulerAuthZ, ctx,
};
use crate::gear::ConcreteAppServices;
use crate::infra::storage::OrmSchedulesRepository;
use crate::infra::storage::test_db::scope;

/// Every hour, on the hour. The occurrence at or before [`NOON_THIRTY`] is
/// [`NOON`], and the next one is an hour later — which is what the failover and
/// outage tests need in order to name two distinct due times.
const HOURLY: &str = "0 * * * *";

/// A fixed evaluation instant. Every test that names a due time passes one of
/// these rather than reading a clock, for the reason `domain::cron` is a pure
/// module: exactly-once cannot be asserted against a wall clock, and a suite
/// that read one would race the minute boundary.
const NOON_THIRTY: OffsetDateTime = datetime!(2026-08-13 12:30 UTC);
const NOON: OffsetDateTime = datetime!(2026-08-13 12:00 UTC);
const ONE_THIRTY: OffsetDateTime = datetime!(2026-08-13 13:30 UTC);
const ONE_PM: OffsetDateTime = datetime!(2026-08-13 13:00 UTC);
/// Six hours before [`NOON_THIRTY`], for the catch-up policy.
const SIX_AM: OffsetDateTime = datetime!(2026-08-13 06:00 UTC);

/// A schedule aimed at the fixture plan and platform.
fn schedule_payload(name: &str, cron: &str) -> NewSchedule {
    NewSchedule {
        name: name.to_owned(),
        target: RunTarget::Plan {
            repo_id: REPO,
            path: "plans/smoke.yaml".to_owned(),
        },
        environment_id: Some(PLATFORM_A),
        branch: Some("main".to_owned()),
        cron: cron.to_owned(),
        exclusive_choice: None,
        enabled: true,
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        parameters: Vec::new(),
    }
}

/// A schedule whose four **carried** fields are all populated and all
/// distinguishable from one another.
///
/// [`schedule_payload`] leaves `parameters`, `include_tags` and `exclude_tags`
/// empty and every other fixture in this suite inherits it, which made
/// `launch_request_for`'s copies of them untestable: replacing `parameters`
/// with `Vec::new()` **and** transposing `include_tags`/`exclude_tags` - the
/// exact mistake that function's own doc names - left the whole suite at 766
/// passing. Empty lists are equal to each other and equal to a dropped field,
/// so an all-empty fixture cannot tell a copy from an omission or a swap.
///
/// The two tag lists are deliberately different lengths as well as different
/// contents, so a transposition is visible even to a reader skimming the
/// failure.
fn carrying_payload(name: &str, cron: &str) -> NewSchedule {
    NewSchedule {
        branch: Some("release/5.0".to_owned()),
        include_tags: vec!["smoke".to_owned(), "e2e".to_owned()],
        exclude_tags: vec!["slow".to_owned()],
        parameters: vec![
            RunParameter {
                name: "SUITE_TIER".to_owned(),
                value: "gold".to_owned(),
            },
            RunParameter {
                name: "RETRY_COUNT".to_owned(),
                value: "2".to_owned(),
            },
        ],
        ..schedule_payload(name, cron)
    }
}

/// Create one enabled hourly schedule under `tenant` and return it.
async fn seed(services: &ConcreteAppServices, tenant: Uuid) -> Schedule {
    services
        .schedules
        .create(&ctx(tenant), schedule_payload("nightly", HOURLY))
        .await
        .expect("a schedule in the caller's own tenant is creatable")
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// An expression that cannot be parsed must fail at the write, not silently
/// never fire.
///
/// The refusal is [`DomainError::InvalidCron`] rather than a
/// `Validation { field: "cron" }` — see `crate::domain::error::DomainError::InvalidCron`,
/// which exists because the same refusal happens at evaluation time, where
/// there is no request field to name. (The plan's Step 1 says "`Validation` on
/// … an unparseable cron"; the variant Task 5 built for this is the one used.)
#[tokio::test]
async fn an_unparseable_cron_is_rejected_at_create() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    for bad in ["", "not a cron", "* * * *", "99 * * * *", "@reboot"] {
        let error = services
            .schedules
            .create(&ctx(OWNER_TENANT), schedule_payload("nightly", bad))
            .await
            .expect_err("an unparseable expression must be refused");
        assert!(
            matches!(error, DomainError::InvalidCron { .. }),
            "{bad:?} produced {error:?}"
        );
    }

    assert!(
        services
            .schedules
            .list(&ctx(OWNER_TENANT))
            .await
            .unwrap()
            .is_empty(),
        "a refused expression must leave no row behind"
    );
}

/// The same at update, which is the half a create-only check misses: a schedule
/// edited into an unparseable expression would keep firing until the edit, then
/// stop for ever with nothing but a WARN.
#[tokio::test]
async fn an_unparseable_cron_is_rejected_at_update() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;

    let error = services
        .schedules
        .update(
            &ctx(OWNER_TENANT),
            stored.id,
            schedule_payload("nightly", "0 25 * * *"),
        )
        .await
        .expect_err("an unparseable expression must be refused at update too");
    assert!(
        matches!(error, DomainError::InvalidCron { .. }),
        "{error:?}"
    );

    assert_eq!(
        services
            .schedules
            .get(&ctx(OWNER_TENANT), stored.id)
            .await
            .unwrap()
            .cron,
        HOURLY,
        "the refused edit must not have reached the row"
    );
}

/// **A padded name is stored trimmed, for every caller.**
///
/// The invariant is *the stored form is the validated form*: `validate` refuses
/// a name that is empty **after trimming**, so a name that survives with padding
/// is a name checked in one form and written in another. The visible cost is
/// `idx_qa_schedules_tenant_name` — on the raw column — accepting `"nightly "`
/// beside `"nightly"`, giving a tenant two schedules that render identically and
/// no 409 between them.
///
/// # Why this test is here and not at the REST boundary
///
/// It was first fixed in `api::rest::dto`, which closed it for HTTP callers and
/// left it open for every in-process one: `QaRunsLocalClient::create_schedule`
/// hands an `sdk::NewSchedule` straight to this service and never constructs a
/// DTO. `ScheduleService::normalize` is the one function both entry points
/// share, so this is the layer at which the property is true of *callers* rather
/// than of one transport — which is why the assertion is written against the
/// service rather than repeated in two places.
///
/// Both directions are covered: the stored name is trimmed, and the padded
/// duplicate therefore collides instead of creating a second row.
#[tokio::test]
async fn a_padded_name_is_stored_trimmed_for_every_caller() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let stored = services
        .schedules
        .create(
            &ctx(OWNER_TENANT),
            NewSchedule {
                name: "  nightly  ".to_owned(),
                ..schedule_payload("ignored", HOURLY)
            },
        )
        .await
        .expect("a padded name is legal, just normalised");
    assert_eq!(stored.name, "nightly", "the stored name must be trimmed");

    let clash = services
        .schedules
        .create(
            &ctx(OWNER_TENANT),
            NewSchedule {
                name: "nightly ".to_owned(),
                ..schedule_payload("ignored", HOURLY)
            },
        )
        .await
        .expect_err("a padded duplicate must collide, not create a second row");
    assert!(
        matches!(&clash, DomainError::ScheduleNameExists { name } if name == "nightly"),
        "got {clash:?}"
    );

    // And an edit normalises too, or a rename could reintroduce the pair.
    let renamed = services
        .schedules
        .update(
            &ctx(OWNER_TENANT),
            stored.id,
            NewSchedule {
                name: "  weekly\t".to_owned(),
                ..schedule_payload("ignored", HOURLY)
            },
        )
        .await
        .expect("the rename must succeed");
    assert_eq!(renamed.name, "weekly");
}

/// A schedule with no name has no stable identity to an operator — the name is
/// its tenant-unique key.
#[tokio::test]
async fn an_empty_name_is_rejected() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    // Whitespace as well as empty: `"   "` is stored as a name that cannot be
    // typed, searched for, or told apart from another one.
    for blank in ["", "   ", "\t\n"] {
        let error = services
            .schedules
            .create(&ctx(OWNER_TENANT), schedule_payload(blank, HOURLY))
            .await
            .expect_err("a blank name must be refused");
        assert!(
            matches!(&error, DomainError::Validation { field, .. } if field == "name"),
            "{blank:?} produced {error:?}"
        );
    }

    // And at update, so an edit cannot blank a name a create refused.
    let stored = seed(&services, OWNER_TENANT).await;
    let error = services
        .schedules
        .update(&ctx(OWNER_TENANT), stored.id, schedule_payload("", HOURLY))
        .await
        .expect_err("a blank name must be refused at update too");
    assert!(matches!(error, DomainError::Validation { .. }), "{error:?}");
}

/// A schedule may not carry a collect target, at create **or** at update.
///
/// A collect run bypasses admission (`service::launch`;
/// `manager/src/services/argo.rs:369-372`), so a schedule holding one would fire
/// admission-bypassing launches on a cron — the same starvation vector
/// `LaunchRunReq::into_domain` and `service::runs::replay` refuse, automated.
/// The source system's collection is an hourly poller and a plain function
/// (`manager/src/services/collect.rs:157-179`), never a `CronWorkflow`, so no
/// legacy schedule can carry one either.
///
/// **Update is checked, not assumed.** `validate` is called from both paths, but
/// that is a property of `ScheduleService`, not of this rule — an edit that
/// could turn a plan schedule into a collect schedule would reopen the hole with
/// the create path still green.
///
/// The row remains *representable*: `qa_schedules.target_collect_url` exists so
/// the shared target codec has no silent drop, and
/// `infra::storage::schedules_sea_repo::schedule_columns` records what would
/// have to become true before the state were reachable. This test is the guard
/// that keeps it unreachable.
///
/// Break-verified: removing the `matches!(.., RunKind::Collect)` arm in
/// `ScheduleService::validate` turns both halves of this red.
///
/// **"and nothing else" was true when Task 5 wrote it and is not any more.**
/// Task 6 added
/// [`notification_settings_cannot_smuggle_a_collect_target_past_the_guard`],
/// which asserts the same guard from the notification edit's side and reddens
/// with it. Corrected here rather than left as a claim a reader would check and
/// find false.
#[tokio::test]
async fn a_collect_target_cannot_be_scheduled() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let collect_payload = |name: &str| NewSchedule {
        target: RunTarget::Collect {
            repo_id: REPO,
            collect_url: "https://insights.example/qa/v1/collect/r/main".to_owned(),
        },
        environment_id: None,
        ..schedule_payload(name, HOURLY)
    };

    let error = services
        .schedules
        .create(&ctx(OWNER_TENANT), collect_payload("collect-nightly"))
        .await
        .expect_err("a collect schedule must be refused");
    assert!(
        matches!(&error, DomainError::Validation { field, .. } if field == "target.kind"),
        "{error:?}"
    );

    // And at update, so an edit cannot install what a create refused.
    let stored = seed(&services, OWNER_TENANT).await;
    let error = services
        .schedules
        .update(
            &ctx(OWNER_TENANT),
            stored.id,
            collect_payload("collect-nightly"),
        )
        .await
        .expect_err("a collect schedule must be refused at update too");
    assert!(
        matches!(&error, DomainError::Validation { field, .. } if field == "target.kind"),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Notification settings (D9)
// ---------------------------------------------------------------------------

/// A schedule with every field the notification edit must not disturb set to
/// something distinguishable — pinned **exclusive**, on a non-default branch,
/// with both tag lists and parameters populated, and disabled.
///
/// `exclusive_choice: Some(true)` is the field legacy's own handler comments on
/// (`manager/src/routes/schedules.rs:854-856`), and `enabled: false` is the one
/// whose loss would be silent and expensive: a paused schedule quietly resuming
/// is the exact failure legacy's delete-and-recreate has to restore by hand.
fn pinned_payload(name: &str) -> NewSchedule {
    NewSchedule {
        exclusive_choice: Some(true),
        enabled: false,
        ..carrying_payload(name, HOURLY)
    }
}

/// The settings used by most tests below: all three at non-default values, so a
/// dropped column is visible.
fn settings() -> ScheduleNotificationSettings {
    ScheduleNotificationSettings {
        slack_enabled: true,
        slack_channel: Some("#qa-alerts".to_owned()),
        slack_events: vec!["failed".to_owned(), "error".to_owned()],
    }
}

/// Editing notification settings must not disturb any other field — legacy is
/// explicit that a schedule pinned exclusive comes back pinned exclusive
/// (`manager/src/routes/schedules.rs`, the `exclusive` comment in
/// `api_update_notifications`, at `schedules.rs:854-856`).
///
/// **The plan's version of this test spelled the events `"Failed"` and
/// `"Error"`.** Those are the Rust *variant* names;
/// `ScheduledRunNotificationEvent` carries `#[serde(rename_all = "snake_case")]`
/// (`manager/src/models.rs:949-959`), so the strings that cross a wire and land
/// in a column are lowercase. Corrected here, and refused by
/// [`an_event_outside_legacys_vocabulary_is_refused`] rather than stored.
///
/// Every field is compared, not a sample: this is the one test that says "and
/// nothing else moved", and a sample would let the untested half move.
#[tokio::test]
async fn updating_notification_settings_leaves_every_other_field_untouched() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let before = services
        .schedules
        .create(&ctx(OWNER_TENANT), pinned_payload("nightly"))
        .await
        .expect("the fixture must be creatable");

    let after = services
        .schedules
        .update_notifications(&ctx(OWNER_TENANT), before.id, settings())
        .await
        .expect("update succeeds");

    assert!(after.slack_notifications_enabled);
    assert_eq!(after.slack_channel.as_deref(), Some("#qa-alerts"));
    assert_eq!(after.slack_notification_events, vec!["failed", "error"]);

    assert_eq!(after.id, before.id);
    assert_eq!(after.name, before.name);
    assert_eq!(after.target, before.target);
    assert_eq!(after.environment_id, before.environment_id);
    assert_eq!(after.branch, before.branch);
    assert_eq!(after.cron, before.cron);
    assert_eq!(
        after.exclusive_choice, before.exclusive_choice,
        "a schedule pinned exclusive must come back pinned exclusive"
    );
    assert_eq!(
        after.enabled, before.enabled,
        "a paused schedule must not resume because somebody edited its Slack channel"
    );
    assert_eq!(after.include_tags, before.include_tags);
    assert_eq!(after.exclude_tags, before.exclude_tags);
    assert_eq!(after.parameters, before.parameters);
    assert_eq!(
        after.last_fired_tick, before.last_fired_tick,
        "the cron cursor is not a caller-editable field"
    );
    assert_eq!(after.created_at, before.created_at);
    assert!(
        after.updated_at >= before.updated_at,
        "the row did change, so `updated_at` moves"
    );

    // ...and it is the row that changed, not just the returned value.
    let reread = services
        .schedules
        .get(&ctx(OWNER_TENANT), before.id)
        .await
        .unwrap();
    assert_eq!(
        reread, after,
        "the returned schedule must be the stored one"
    );
}

/// A newly created schedule notifies nobody, and a **full replace leaves the
/// settings standing**.
///
/// `NewSchedule` deliberately carries no notification fields, so the replace
/// cannot express them; `schedules_sea_repo::schedule_columns` records why they
/// are left `NotSet` rather than written. The observable legacy behaviour is the
/// same — its edit form is seeded from the schedule being edited precisely so an
/// edit does not wipe them
/// (`manager-ui/src/components/schedules/CreateScheduleDialog.tsx:305-309`) —
/// but here it is a property of the `UPDATE` rather than of the client.
///
/// Break-verified, and this one really is alone: replacing `schedule_columns`'
/// three `ActiveValue::NotSet` lines with `ActiveValue::Set(false)` /
/// `Set(None)` / `Set(json!([]))` — the shape a careless "fill in the new
/// columns" edit would take — reddens **this test and no other**, measured. A
/// replace would then silently clear an operator's Slack settings on every cron
/// edit, and nothing else in 837 tests would say so.
#[tokio::test]
async fn a_full_replace_leaves_the_notification_settings_standing() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let created = services
        .schedules
        .create(&ctx(OWNER_TENANT), pinned_payload("nightly"))
        .await
        .unwrap();

    assert!(
        !created.slack_notifications_enabled,
        "a created schedule must notify nobody until somebody says otherwise"
    );
    assert_eq!(created.slack_channel, None);
    assert!(created.slack_notification_events.is_empty());

    services
        .schedules
        .update_notifications(&ctx(OWNER_TENANT), created.id, settings())
        .await
        .unwrap();

    let replaced = services
        .schedules
        .update(
            &ctx(OWNER_TENANT),
            created.id,
            NewSchedule {
                cron: "0 4 * * *".to_owned(),
                ..pinned_payload("nightly")
            },
        )
        .await
        .expect("the replace must succeed");

    assert_eq!(replaced.cron, "0 4 * * *", "the replace did happen");
    assert!(
        replaced.slack_notifications_enabled,
        "a cron edit must not silently unsubscribe an operator from Slack"
    );
    assert_eq!(replaced.slack_channel.as_deref(), Some("#qa-alerts"));
    assert_eq!(replaced.slack_notification_events, vec!["failed", "error"]);
}

/// The event vocabulary is legacy's, closed, and **case-sensitive on the
/// serialized spelling**.
///
/// Legacy's form deserializes straight into `ScheduledRunNotificationEvent`
/// (`manager/src/models.rs:291-299`), so an unrecognised name is refused there
/// and never stored. Storing one here would be a subscription that silently
/// never fires, because qa-insights' routing core (Task 36) can only act on
/// names it knows.
///
/// `"InProgress"` is in the refused list on purpose: it is the *variant* name,
/// which is what the plan for this task listed, and it is not an event name.
/// Legacy's own `ScheduledRunNotificationEvent::parse` agrees — it lowercases
/// its input, and `"inprogress"` matches nothing.
#[tokio::test]
async fn an_event_outside_legacys_vocabulary_is_refused() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;

    for bad in ["InProgress", "Failed", "FAILED", "running", "", "failed "] {
        let error = services
            .schedules
            .update_notifications(
                &ctx(OWNER_TENANT),
                stored.id,
                ScheduleNotificationSettings {
                    slack_events: vec![bad.to_owned()],
                    ..settings()
                },
            )
            .await
            .expect_err("an unknown event must be refused");
        assert!(
            matches!(&error, DomainError::Validation { field, .. } if field == "slack_events"),
            "{bad:?} produced {error:?}"
        );
    }

    // All six legacy spellings are accepted, which is the other half: a check
    // that refused everything would pass the loop above.
    let all_six = services
        .schedules
        .update_notifications(
            &ctx(OWNER_TENANT),
            stored.id,
            ScheduleNotificationSettings {
                slack_events: qa_runs_sdk::SLACK_NOTIFICATION_EVENTS
                    .iter()
                    .map(|e| (*e).to_owned())
                    .collect(),
                ..settings()
            },
        )
        .await
        .expect("legacy's six event names must all be accepted");
    assert_eq!(
        all_six.slack_notification_events,
        vec![
            "pending",
            "in_progress",
            "succeeded",
            "failed",
            "error",
            "skipped"
        ],
        "the six are stored verbatim and in the order given"
    );

    // And the refusal left nothing behind.
    let reread = services
        .schedules
        .get(&ctx(OWNER_TENANT), stored.id)
        .await
        .unwrap();
    assert_eq!(reread.slack_notification_events.len(), 6);
}

/// A channel wider than `qa_schedules.slack_channel` is a 400, not a 500.
///
/// `SQLite` does not enforce `VARCHAR` widths — it has type affinity, not width
/// — so this test would pass with no check at all if it asserted on the write
/// succeeding. It asserts on the refusal instead. On Postgres the unchecked
/// value raises `22001`, which surfaces as an opaque 500 naming no field.
#[tokio::test]
async fn an_over_long_slack_channel_is_refused() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;

    let at_the_width = "#".to_owned() + &"c".repeat(254);
    services
        .schedules
        .update_notifications(
            &ctx(OWNER_TENANT),
            stored.id,
            ScheduleNotificationSettings {
                slack_channel: Some(at_the_width),
                ..settings()
            },
        )
        .await
        .expect("exactly the column width must be accepted");

    let error = services
        .schedules
        .update_notifications(
            &ctx(OWNER_TENANT),
            stored.id,
            ScheduleNotificationSettings {
                slack_channel: Some("c".repeat(256)),
                ..settings()
            },
        )
        .await
        .expect_err("one byte over must be refused");
    assert!(
        matches!(&error, DomainError::Validation { field, .. } if field == "slack_channel"),
        "{error:?}"
    );
}

/// **The notification edit is not a second way past Task 5's collect guard.**
///
/// `ScheduleService::validate` refuses `RunKind::Collect` on create and on
/// update, and `update_notifications` does not call it. It does not need to:
/// `ScheduleNotificationSettings` has no target and no kind, and
/// `SchedulesRepository::update_notifications` leaves `run_kind` and every
/// `target_*` column `NotSet`, so they are not in the `SET` list at all.
///
/// That is a claim about a *type*, which is the strongest form this can take —
/// there is no payload a caller could construct that names a run kind. This test
/// is the residual check that the kind and target really do survive the write
/// unchanged, so the guard still governs the only two paths that can set them.
///
/// Break-verified: deleting the `matches!(.., RunKind::Collect)` arm from
/// `ScheduleService::validate` reddens exactly two tests — the second half of
/// this one and [`a_collect_target_cannot_be_scheduled`].
#[tokio::test]
async fn notification_settings_cannot_smuggle_a_collect_target_past_the_guard() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = services
        .schedules
        .create(&ctx(OWNER_TENANT), pinned_payload("nightly"))
        .await
        .unwrap();

    let after = services
        .schedules
        .update_notifications(&ctx(OWNER_TENANT), stored.id, settings())
        .await
        .unwrap();

    assert_eq!(
        after.target, stored.target,
        "a notification edit must not be able to repoint a schedule"
    );
    assert_eq!(
        after.target.kind(),
        qa_runs_sdk::RunKind::Plan,
        "and it must not be able to change its kind"
    );

    // The guard itself is still the only thing deciding this, and still says no.
    let error = services
        .schedules
        .update(
            &ctx(OWNER_TENANT),
            stored.id,
            NewSchedule {
                target: RunTarget::Collect {
                    repo_id: REPO,
                    collect_url: "https://insights.example/qa/v1/collect/r/main".to_owned(),
                },
                environment_id: None,
                ..pinned_payload("nightly")
            },
        )
        .await
        .expect_err("Task 5's guard must be untouched");
    assert!(
        matches!(&error, DomainError::Validation { field, .. } if field == "target.kind"),
        "{error:?}"
    );
}

/// A schedule that does not exist is a not-found, not a silent no-op.
#[tokio::test]
async fn updating_notifications_on_a_missing_schedule_is_not_found() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let error = services
        .schedules
        .update_notifications(&ctx(OWNER_TENANT), Uuid::from_u128(0xDEAD), settings())
        .await
        .expect_err("a missing schedule must be refused");
    assert!(
        matches!(error, DomainError::ScheduleNotFound { .. }),
        "{error:?}"
    );
}

/// `enabled: false` is not a soft delete and not a filter the caller applies —
/// the enumeration itself must not return the row.
#[tokio::test]
async fn a_disabled_schedule_never_fires() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    services
        .schedules
        .create(
            &ctx(OWNER_TENANT),
            NewSchedule {
                enabled: false,
                ..schedule_payload("nightly", HOURLY)
            },
        )
        .await
        .unwrap();

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    assert_eq!(
        report,
        ScheduleTickReport::default(),
        "a disabled schedule must not even be evaluated"
    );
    assert!(fleet.admitter.admitted().is_empty());
    assert!(fleet.runs_of(OWNER_TENANT).await.is_empty());
}

/// **The legacy quirk that does not port.**
///
/// The source system edits a schedule by deleting and recreating the
/// `CronWorkflow`, then re-suspending it by hand — with an error message that
/// has to tell the operator the schedule is now *running* if the restore fails
/// (`manager/src/routes/schedules.rs:708-733`). Here `enabled` is a field of the
/// payload, so an edit states it and there is nothing to restore.
///
/// Asserted in both directions, because "preserved" is only meaningful if the
/// field is honoured at all: a service that ignored `enabled` on update would
/// pass a one-directional version of this test.
#[tokio::test]
async fn editing_a_schedule_preserves_its_enabled_state() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;
    assert!(stored.enabled, "premise: the fixture starts enabled");

    // An edit that changes everything else and restates `enabled: true`.
    let edited = services
        .schedules
        .update(
            &ctx(OWNER_TENANT),
            stored.id,
            NewSchedule {
                branch: Some("release/5.0".to_owned()),
                ..schedule_payload("renamed", "0 3 * * *")
            },
        )
        .await
        .unwrap();
    assert!(edited.enabled, "an edit must not disable a live schedule");
    assert_eq!(edited.name, "renamed");
    assert_eq!(edited.cron, "0 3 * * *");
    assert!(
        services
            .schedules
            .get(&ctx(OWNER_TENANT), stored.id)
            .await
            .unwrap()
            .enabled
    );

    // And the other direction, so the assertion above is not vacuous.
    let disabled = services
        .schedules
        .update(
            &ctx(OWNER_TENANT),
            stored.id,
            NewSchedule {
                enabled: false,
                ..schedule_payload("renamed", "0 3 * * *")
            },
        )
        .await
        .unwrap();
    assert!(!disabled.enabled);
}

// ---------------------------------------------------------------------------
// Firing
// ---------------------------------------------------------------------------

/// A due schedule reaches the admitter, which is the seam only
/// `LaunchService::launch` calls — so this is "one creation path", asserted
/// rather than argued.
#[tokio::test]
async fn a_due_schedule_fires_through_the_launch_service() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    assert_eq!(
        report,
        ScheduleTickReport {
            evaluated: 1,
            fired: 1,
            lost: 0,
            failed: 0,
            deferred: 0,
        }
    );
    assert_eq!(
        fleet.admitter.admitted().len(),
        1,
        "the fire must go through the admission seam a manual launch goes through"
    );

    // The cursor records the occurrence that fired, not the instant the tick
    // ran: `NOON`, not `NOON_THIRTY`. [`the_produced_run_records_its_schedule_id_and_scheduled_source`]
    // separately pins that the produced run carries `schedule_id: Some(stored.id)`.
    assert_eq!(
        fleet.cursor_of(OWNER_TENANT, stored.id).await,
        Some(NOON),
        "the cursor advances to the occurrence's due time, not the tick's"
    );
}

/// **A fire carries the schedule's own parameters, tag filter and branch.**
///
/// [`launch_request_for`] copies four fields out of the stored schedule and
/// every one of them is `Vec<String>`, `Vec<RunParameter>` or
/// `Option<String>` - mutually assignable, and until [`carrying_payload`]
/// existed, all empty in every fixture. Two mutations that the suite could not
/// see, both applied to the shipped code and both measured green at 766:
///
/// * `parameters: Vec::new()`, which silently drops a scheduled suite's whole
///   configuration - a nightly run that needs `SUITE_TIER=gold` quietly runs
///   without it;
/// * `include_tags`/`exclude_tags` transposed, which **inverts which tests
///   run**. That is the mistake `launch_request_for`'s doc names in as many
///   words, and it had no test behind it.
///
/// Asserted on the `Run` the admitter saw, not on the `LaunchRequest`: the
/// admission seam is the one thing on the path from a schedule to a run, so
/// this pins that the values survived the whole launch rather than that a
/// constructor copied them.
///
/// `branch` is read back as `test_version`, which is what rule 6 of
/// `service::launch` makes it - `Resolved::clone_new_run`'s
/// `test_version: Some(facts.branch)`.
#[tokio::test]
async fn a_fire_carries_the_schedules_parameters_tags_and_branch() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = services
        .schedules
        .create(&ctx(OWNER_TENANT), carrying_payload("nightly", HOURLY))
        .await
        .expect("the schedule must be creatable");

    // Premise: the *stored* row really carries them, so a failure below is
    // about the fire and not about the create having dropped them.
    assert_eq!(stored.include_tags, vec!["smoke", "e2e"]);
    assert_eq!(stored.exclude_tags, vec!["slow"]);
    assert_eq!(stored.parameters.len(), 2);
    assert_eq!(stored.branch.as_deref(), Some("release/5.0"));

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;
    assert_eq!(report.fired, 1, "premise: the schedule must actually fire");

    let admitted = fleet.admitter.admitted();
    assert_eq!(admitted.len(), 1);
    let run = &admitted[0];

    assert_eq!(
        run.parameters,
        vec![
            RunParameter {
                name: "SUITE_TIER".to_owned(),
                value: "gold".to_owned(),
            },
            RunParameter {
                name: "RETRY_COUNT".to_owned(),
                value: "2".to_owned(),
            },
        ],
        "a scheduled run executes with the schedule's parameters or with the wrong \
         configuration"
    );
    assert_eq!(
        run.include_tags,
        vec!["smoke", "e2e"],
        "the include filter must arrive as the include filter"
    );
    assert_eq!(
        run.exclude_tags,
        vec!["slow"],
        "and the exclude filter as the exclude filter - transposing the two inverts \
         which tests run"
    );
    assert_eq!(
        run.test_version.as_deref(),
        Some("release/5.0"),
        "the schedule's branch is what the run is executed against"
    );
}

/// The stored row is what is carried, **not the payload that created it** - so
/// an edit is honoured by the next fire.
///
/// The companion to the test above, and not a duplicate of it: that one could
/// pass against a `fire` that had somehow kept the create payload around. This
/// changes all four fields through `update` and fires afterwards, which is the
/// shape a real schedule takes - created once, retuned repeatedly.
#[tokio::test]
async fn a_fire_carries_the_edited_values_not_the_created_ones() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = services
        .schedules
        .create(&ctx(OWNER_TENANT), carrying_payload("nightly", HOURLY))
        .await
        .unwrap();

    services
        .schedules
        .update(
            &ctx(OWNER_TENANT),
            stored.id,
            NewSchedule {
                branch: Some("main".to_owned()),
                include_tags: vec!["nightly".to_owned()],
                exclude_tags: vec!["flaky".to_owned(), "manual".to_owned()],
                parameters: vec![RunParameter {
                    name: "SUITE_TIER".to_owned(),
                    value: "silver".to_owned(),
                }],
                ..schedule_payload("nightly", HOURLY)
            },
        )
        .await
        .unwrap();

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;
    assert_eq!(report.fired, 1);

    let admitted = fleet.admitter.admitted();
    let run = &admitted[0];
    assert_eq!(run.include_tags, vec!["nightly"]);
    assert_eq!(run.exclude_tags, vec!["flaky", "manual"]);
    assert_eq!(run.parameters.len(), 1);
    assert_eq!(run.parameters[0].value, "silver");
    assert_eq!(run.test_version.as_deref(), Some("main"));
}

/// **Every scope the tick derives *for a write* is for `qa.schedule`, for the
/// action being performed, and about the schedule being fired.**
///
/// Nothing else in this suite can see any of that. The policy double grants
/// unconditionally and injects only a tenant predicate, so — measured on the
/// version before it recorded — pointing `write_tick_outcome` at
/// `actions::LIST` with no id, or swapping `&resources::SCHEDULE` for
/// `&resources::RUN` across the whole service, each left the suite green. Both
/// are one-token edits and neither is visible in a row.
///
/// **The enumeration itself is absent, on purpose.** Since the nil-tenant
/// enumerations were moved to `domain::elevated`, `enabled_schedules` routes
/// through `domain::elevated::enumeration_scope` instead of the PEP, exactly so a
/// nil-tenant tick does not have to be granted a covering constraint set by
/// the deployment's policy — see that module's doc. `fleet.authz` only records
/// requests that reached `evaluate`, so the cross-tenant listing this test
/// used to assert as `Asked::new(schedule, actions::LIST, None)` leaves no
/// trace here any more; its absence is exactly what
/// `the_ttl_sweep_does_not_consult_the_policy_engine` (`dispatch_tests.rs`)
/// asserts for its six sibling reads.
///
/// The expected sequence is spelled out in full rather than asserted as a set,
/// which pins `GET` → the `FIRE` block: the ownership resolve precedes the
/// claim (`domain::repos::OwnedScheduleId`).
///
/// **It does not discriminate within the `FIRE` block**, and saying so is
/// cheaper than letting a reader assume otherwise: the three rows are
/// identical, so swapping `record_outcome` and `advance_cursor` leaves this
/// green. That ordering is enforced by the compiler instead — both take a
/// `tick_id` that does not exist until the claim has returned one.
///
/// The resource *type* comes from the constant rather than a re-typed literal,
/// so renaming `resources::SCHEDULE` cannot make this pass against a stale
/// string.
#[tokio::test]
async fn the_tick_asks_the_pdp_about_the_schedule_it_is_firing() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;
    fleet.authz.forget_setup();

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;
    assert_eq!(
        report.fired, 1,
        "premise: the fire went all the way through"
    );

    let scheduled = resources::SCHEDULE;
    let schedule = scheduled.name();
    assert_eq!(
        fleet.authz.asked_about(schedule),
        vec![
            // No enumeration entry: the cross-tenant listing is elevated
            // through `domain::elevated`, not the PEP. See this test's doc.
            //
            // The ownership token, under the firing tenant's own scope.
            Asked::new(schedule, actions::GET, Some(stored.id)),
            // The claim, and then the two writes that settle it. Each names the
            // schedule — a tick id is not a `qa.schedule` resource id, and
            // passing one mixed two id namespaces under one declared type.
            Asked::new(schedule, actions::FIRE, Some(stored.id)),
            Asked::new(schedule, actions::FIRE, Some(stored.id)),
            Asked::new(schedule, actions::FIRE, Some(stored.id)),
        ],
    );

    // And the launch really did derive its own run scopes rather than the
    // schedule service asking about `qa.run` itself — which is the other
    // direction the resource-type swap could have gone.
    let runs = fleet.authz.asked_about(resources::RUN.name());
    assert!(
        runs.iter().any(|asked| asked.action == actions::CREATE),
        "the shared launch path must ask about qa.run/create: {runs:?}"
    );
}

/// The stored choice is delivered as the **launch** tier, which is the top of
/// the precedence chain.
#[tokio::test]
async fn the_stored_exclusivity_choice_arrives_as_the_launch_tier() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    services
        .schedules
        .create(
            &ctx(OWNER_TENANT),
            NewSchedule {
                exclusive_choice: Some(true),
                ..schedule_payload("nightly", HOURLY)
            },
        )
        .await
        .unwrap();

    services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    let run = fleet.admitter.admitted().remove(0);
    assert!(run.resolved_exclusive);
    assert_eq!(
        run.exclusive_tier,
        ExclusiveTier::Launch,
        "a schedule's choice IS the launch tier; anything else lets a lower tier \
         override an operator's explicit decision"
    );
}

/// `auto` is `None`, which is **not** `Some(false)`: it inherits, so a
/// `TEST_META` declaration below it wins.
#[tokio::test]
async fn an_auto_choice_arrives_as_none_and_is_inherited() {
    let fleet = Fleet::new().await.with_exclusive_test_file();
    let services = fleet.instance();
    services
        .schedules
        .create(
            &ctx(OWNER_TENANT),
            NewSchedule {
                exclusive_choice: None,
                ..schedule_payload("nightly", HOURLY)
            },
        )
        .await
        .unwrap();

    services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    let run = fleet.admitter.admitted().remove(0);
    assert!(
        run.resolved_exclusive,
        "auto must inherit the exclusive test file rather than defaulting to parallel"
    );
    assert_eq!(run.exclusive_tier, ExclusiveTier::TestMeta);
}

/// The other half of the tri-state, and the one that makes `auto` worth having:
/// an explicit `false` is an *opinion* and suppresses the tier below it.
#[tokio::test]
async fn an_explicit_false_choice_suppresses_an_exclusive_test_file() {
    let fleet = Fleet::new().await.with_exclusive_test_file();
    let services = fleet.instance();
    services
        .schedules
        .create(
            &ctx(OWNER_TENANT),
            NewSchedule {
                exclusive_choice: Some(false),
                ..schedule_payload("nightly", HOURLY)
            },
        )
        .await
        .unwrap();

    services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    let run = fleet.admitter.admitted().remove(0);
    assert!(
        !run.resolved_exclusive,
        "an explicit false must beat an exclusive TEST_META; collapsing it to \
         'unset' would make a schedule unable to opt out"
    );
    assert_eq!(run.exclusive_tier, ExclusiveTier::Launch);
}

/// A schedule whose cursor is already at the most recent occurrence is not due,
/// and must not reach the claim at all.
#[tokio::test]
async fn a_schedule_that_is_not_due_does_not_fire() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;

    // The cursor's only writer, used here to place the schedule exactly where a
    // successful fire would have left it.
    {
        let conn = fleet.db.conn().unwrap();
        assert!(
            OrmSchedulesRepository
                .advance_last_fired_tick(&conn, &scope(OWNER_TENANT), stored.id, NOON)
                .await
                .unwrap()
        );
    }

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    assert_eq!(
        report,
        ScheduleTickReport {
            evaluated: 1,
            fired: 0,
            lost: 0,
            failed: 0,
            deferred: 0,
        },
        "the schedule is enumerated and evaluated, and nothing is due"
    );
    assert!(fleet.admitter.admitted().is_empty());
    assert!(
        fleet.ticks_of(OWNER_TENANT).await.is_empty(),
        "a schedule that is not due must not write a claim"
    );
}

#[tokio::test]
async fn last_fired_tick_advances_after_a_successful_fire() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;
    assert_eq!(stored.last_fired_tick, None, "premise: nothing fired yet");

    services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    assert_eq!(
        services
            .schedules
            .get(&ctx(OWNER_TENANT), stored.id)
            .await
            .unwrap()
            .last_fired_tick,
        Some(NOON),
        "the cursor moves to the occurrence that fired, not to the tick's own instant"
    );
}

/// The run a schedule produces has to be identifiable as scheduled, or the
/// source-normalisation the whole subsystem does (`manual` vs `scheduled`) is
/// wrong for exactly the runs it exists for.
#[tokio::test]
async fn the_produced_run_records_its_schedule_id_and_scheduled_source() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;

    services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    let runs = fleet.runs_of(OWNER_TENANT).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].schedule_id, Some(stored.id));
    assert_eq!(runs[0].source, RunSource::Scheduled);
    // Read back off the stored row, not off the request: this is what a
    // consumer reading the run — directly or through the reconcile sweep —
    // actually sees.
    assert_eq!(runs[0].environment_id, Some(PLATFORM_A));
}

/// **A pass fires at most [`MAX_FIRES_PER_TICK`], and defers the rest rather
/// than skipping them.**
///
/// Cron alignment is the realistic trigger — `0 0 * * *` across a fleet makes
/// every schedule due in one pass — and the deferral is only acceptable because
/// an unfired schedule keeps its cursor and claims the same occurrence next
/// pass. Both halves are asserted: the cap holds, *and* the second pass finishes
/// the job with no due time lost and no schedule fired twice.
#[tokio::test]
async fn a_pass_fires_at_most_the_cap_and_defers_the_rest() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let over = MAX_FIRES_PER_TICK + 3;
    // The row counts below are `usize`; converting once here keeps the
    // assertions free of casts that clippy reads as possible truncation.
    let over_rows = usize::try_from(over).unwrap();
    for n in 0..over {
        services
            .schedules
            .create(
                &ctx(OWNER_TENANT),
                schedule_payload(&format!("s{n}"), HOURLY),
            )
            .await
            .unwrap();
    }

    let first = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;
    assert_eq!(first.evaluated, over, "every schedule is still enumerated");
    assert_eq!(first.fired, MAX_FIRES_PER_TICK);
    assert_eq!(first.deferred, over - MAX_FIRES_PER_TICK);
    assert_eq!(first.failed, 0, "a deferral is not a failure");
    assert_eq!(
        fleet.runs_of(OWNER_TENANT).await.len(),
        usize::try_from(MAX_FIRES_PER_TICK).unwrap()
    );

    // The same instant, so the deferred schedules are due for the *same*
    // occurrence they were deferred from — which is the property that makes the
    // cap safe, and which a read window could not give.
    let second = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;
    assert_eq!(second.fired, over - MAX_FIRES_PER_TICK);
    assert_eq!(second.deferred, 0);
    assert_eq!(
        second.lost, 0,
        "the ones already fired are not due again, so nothing re-attempts a claim"
    );
    assert_eq!(
        fleet.runs_of(OWNER_TENANT).await.len(),
        over_rows,
        "every schedule fired exactly once across the two passes"
    );
    assert_eq!(fleet.ticks_of(OWNER_TENANT).await.len(), over_rows);
}

// ---------------------------------------------------------------------------
// Exactly-once (cpt-cf-qa-nfr-scheduler-exactly-once)
// ---------------------------------------------------------------------------

/// **The NFR, demonstrated deterministically.**
///
/// Two instances, one store, one due time, no clock and no concurrency: the
/// claims are attempted in sequence and the second one loses.
///
/// # Why this drives `fire` rather than the whole tick
///
/// Two *whole* ticks in sequence would not exercise the claim at all. The first
/// advances `last_fired_tick`, so the second computes no due time and stops
/// before the claim — the run count would come out right for the wrong reason,
/// and dropping the unique index would leave it green. What two concurrent
/// replicas actually do is evaluate the **same** schedule row, before either has
/// advanced anything, and that is what handing both instances one [`Fire`]
/// models. Everything below it — the resolve, the claim, the launch, the
/// recording — is the production path, against production repositories.
///
/// `a_failover_mid_fire_does_not_produce_a_second_run` covers the same
/// guarantee end to end, through the public tick.
///
/// **Verified by breaking it**: with `idx_qa_schedule_ticks_claim` changed from
/// `CREATE UNIQUE INDEX` to `CREATE INDEX` in the migration's `SQLITE_UP`, the
/// second claim succeeds and two runs appear.
#[tokio::test]
async fn two_instances_evaluating_the_same_due_time_produce_exactly_one_run() {
    let fleet = Fleet::new().await;
    let first = fleet.instance();
    let second = fleet.instance();
    let stored = seed(&first, OWNER_TENANT).await;

    let due = Fire {
        schedule: &stored,
        tenant: TenantBound::new(OWNER_TENANT).unwrap(),
        due_at: NOON,
    };

    let mut first_report = ScheduleTickReport::default();
    first.schedules.fire(&due, &mut first_report).await;
    let mut second_report = ScheduleTickReport::default();
    second.schedules.fire(&due, &mut second_report).await;

    assert_eq!(first_report.fired, 1);
    assert_eq!(second_report.fired, 0);
    assert_eq!(second_report.lost, 1);

    assert_eq!(
        fleet.admitter.admitted().len(),
        1,
        "exactly one launch may reach the admitter for one due time"
    );
    assert_eq!(
        fleet.runs_of(OWNER_TENANT).await.len(),
        1,
        "and exactly one run may exist"
    );
    assert_eq!(
        fleet.ticks_of(OWNER_TENANT).await.len(),
        1,
        "the unique index is what makes that true: one claim row, not two"
    );
}

/// A lost claim is the normal path on every non-winning replica, so it must not
/// be reported as a failure and must not be logged as one.
///
/// Driven end to end through the public tick: an instance that died after
/// claiming leaves the row behind, and the surviving instance's whole tick has
/// to treat it as routine.
#[tokio::test]
#[tracing_test::traced_test]
async fn the_losing_instance_treats_the_lost_claim_as_normal_not_an_error() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;
    claim_as_a_departed_instance(&fleet, stored.id, NOON).await;

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    assert_eq!(
        report,
        ScheduleTickReport {
            evaluated: 1,
            fired: 0,
            lost: 1,
            failed: 0,
            deferred: 0,
        },
        "a lost race is not a failure"
    );
    assert!(
        !logs_contain("claiming a due time failed"),
        "and it must not be logged as one"
    );
    assert!(
        !logs_contain("a schedule's launch failed"),
        "nor as a launch failure, which never happened"
    );
}

/// **The claim is durable, and the launch behind it is never retried.**
///
/// The instance that won the claim died before it recorded anything, so the tick
/// row exists with NULL columns and the cursor never moved. A surviving instance
/// evaluating the same occurrence must produce no run — and then must go on
/// firing normally at the *next* occurrence, which is what makes the orphaned
/// claim self-healing rather than a permanently wedged schedule.
///
/// That second half depends on `domain::cron::next_due` answering with the most
/// recent occurrence rather than the earliest outstanding one; the interlock is
/// recorded in both modules.
#[tokio::test]
async fn a_failover_mid_fire_does_not_produce_a_second_run() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;
    claim_as_a_departed_instance(&fleet, stored.id, NOON).await;

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;
    assert_eq!(report.lost, 1);
    assert!(
        fleet.admitter.admitted().is_empty(),
        "the orphaned claim's due time must never be launched a second time"
    );
    assert!(fleet.runs_of(OWNER_TENANT).await.is_empty());

    // The next occurrence is a different `due_at`, so it claims cleanly.
    let recovered = services.schedules.fire_due_schedules_at(ONE_THIRTY).await;
    assert_eq!(recovered.fired, 1, "the schedule is not wedged");
    assert_eq!(fleet.runs_of(OWNER_TENANT).await.len(), 1);
    assert_eq!(
        services
            .schedules
            .get(&ctx(OWNER_TENANT), stored.id)
            .await
            .unwrap()
            .last_fired_tick,
        Some(ONE_PM),
        "and the cursor skipped the lost occurrence rather than back-filling it"
    );
}

/// A launch that fails after its claim is recorded on the tick row and is not
/// attempted again — for this due time or any later tick.
///
/// The recorded text goes through `DomainError::recorded_text`, so an error
/// whose vocabulary belongs to another system is redacted before it reaches a
/// column an operator reads.
///
/// **A refused launch leaves a retired run row behind**, and that is
/// `LaunchService::abandon`'s contract rather than a leak: the run row has to
/// exist before admission, so a refusal moves it to `Canceled` rather than
/// stranding it in `created`. What "no run" would mean here is *no executing
/// run*, and the assertions below say that precisely — one row, terminal, and
/// no second launch ever attempted.
#[tokio::test]
async fn a_launch_failure_is_recorded_on_the_tick_and_not_retried() {
    let fleet = Fleet::with(QueueingAdmitter::refusing(), SchedulerAuthZ::fleet()).await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;
    assert_eq!(
        report,
        ScheduleTickReport {
            evaluated: 1,
            fired: 0,
            lost: 0,
            failed: 1,
            deferred: 0,
        }
    );
    let refused = fleet.runs_of(OWNER_TENANT).await;
    assert_eq!(refused.len(), 1);
    assert_eq!(
        refused[0].state,
        RunState::Canceled,
        "the refused launch must be retired, not left executing or dangling"
    );

    let ticks = fleet.ticks_of(OWNER_TENANT).await;
    assert_eq!(ticks.len(), 1, "the claim is durable, not rolled back");
    assert_eq!(ticks[0].run_id, None);
    let recorded = ticks[0].error.clone().expect("the reason is written down");
    assert!(
        !recorded.contains("kubeconfig"),
        "an executor's own vocabulary must not reach a column an operator reads: {recorded}"
    );

    // Not retried, at the same instant or at any later one before the next
    // occurrence. One more admission attempt would be a second destructive run.
    let again = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;
    assert_eq!(again.fired, 0);
    assert_eq!(again.failed, 0, "and it is not re-attempted and re-failed");
    assert_eq!(
        fleet.admitter.admitted().len(),
        1,
        "exactly one launch attempt, ever, for this due time"
    );
    assert_eq!(fleet.runs_of(OWNER_TENANT).await.len(), 1);
    assert_eq!(fleet.ticks_of(OWNER_TENANT).await.len(), 1);
    assert_eq!(
        services
            .schedules
            .get(&ctx(OWNER_TENANT), stored.id)
            .await
            .unwrap()
            .last_fired_tick,
        Some(NOON),
    );
}

/// **The catch-up policy, end to end.** Six hours of an hourly schedule missed;
/// one run, not six.
///
/// The unit-level rule lives in `domain::cron`; this is the one that shows the
/// service honours it, because the failure mode it prevents is a service-level
/// one — six exclusive runs draining one per dispatcher tick, with every other
/// tenant's work behind them.
#[tokio::test]
async fn a_long_outage_fires_once_not_once_per_missed_occurrence() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;

    {
        let conn = fleet.db.conn().unwrap();
        OrmSchedulesRepository
            .advance_last_fired_tick(&conn, &scope(OWNER_TENANT), stored.id, SIX_AM)
            .await
            .unwrap();
    }

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    assert_eq!(report.fired, 1);
    assert_eq!(
        fleet.runs_of(OWNER_TENANT).await.len(),
        1,
        "six occurrences were missed and exactly one run comes out"
    );
    assert_eq!(
        fleet.ticks_of(OWNER_TENANT).await.len(),
        1,
        "and exactly one claim was written"
    );
    assert_eq!(
        services
            .schedules
            .get(&ctx(OWNER_TENANT), stored.id)
            .await
            .unwrap()
            .last_fired_tick,
        Some(NOON),
        "the cursor jumps to the most recent occurrence; the rest are skipped for good"
    );
}

/// Claim a due time the way a replica that then died would have: the row is
/// there, `run_id` and `error` are NULL, and the cursor never moved.
///
/// Through the real repository and a real `OwnedScheduleId` — the token's field
/// is private to `domain::repos::schedules_repo`, so `resolve_owned_schedule` is
/// the only way a test can obtain one, which is the property the type exists
/// for.
async fn claim_as_a_departed_instance(fleet: &Fleet, schedule_id: Uuid, due_at: OffsetDateTime) {
    let conn = fleet.db.conn().unwrap();
    let owned = OrmSchedulesRepository
        .resolve_owned_schedule(&conn, &scope(OWNER_TENANT), schedule_id)
        .await
        .unwrap();
    let claimed = OrmSchedulesRepository
        .claim_tick(
            &conn,
            &scope(OWNER_TENANT),
            OWNER_TENANT,
            owned,
            due_at,
            "qa-runs/the-instance-that-died",
        )
        .await
        .unwrap();
    assert!(claimed.is_some(), "premise: the departed instance won it");
}

// ---------------------------------------------------------------------------
// Tenancy
// ---------------------------------------------------------------------------

/// The run a schedule produces belongs to the schedule's tenant, and to no
/// other — the enumeration is cross-tenant and every write after it is not.
#[tokio::test]
async fn a_schedule_fires_under_its_own_tenant() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    // Deliberately **not** the tenant the fixtures elsewhere default to: a
    // write bound to the wrong one would otherwise land where the assertion
    // expects it.
    let stored = seed(&services, OTHER_TENANT).await;

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;
    assert_eq!(report.fired, 1);

    let owner_runs = fleet.runs_of(OTHER_TENANT).await;
    assert_eq!(owner_runs.len(), 1);
    assert_eq!(owner_runs[0].schedule_id, Some(stored.id));
    assert!(
        fleet.runs_of(OWNER_TENANT).await.is_empty(),
        "the run must not be visible to a tenant that owns no schedule here"
    );
    assert!(
        fleet.ticks_of(OWNER_TENANT).await.is_empty(),
        "nor may the claim row be"
    );
}

/// **A nil tenant is the platform-root sentinel, not a tenant.**
///
/// A schedule row carrying one is corrupt, and firing it would mint a
/// tenant-bound context from the cross-tenant enumeration identity — the escape
/// `domain::system_actor::TenantBound` exists to close. The tick refuses it and
/// says so; it does not fire it and does not stop the pass.
///
/// The enumeration scope here admits nil on purpose: without that the row is
/// filtered out before the guard is reached and the test proves nothing.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_nil_tenant_schedule_is_refused_rather_than_fired() {
    let fleet = Fleet::with(QueueingAdmitter::new(), SchedulerAuthZ::admitting_nil()).await;
    let services = fleet.instance();

    // Written straight through the repository: no `SecurityContext` can carry a
    // nil tenant into `create`, which is the point — this is a corrupt row, not
    // something a caller can ask for.
    {
        let conn = fleet.db.conn().unwrap();
        OrmSchedulesRepository
            .create(
                &conn,
                &scope(Uuid::nil()),
                Uuid::nil(),
                schedule_payload("orphaned", HOURLY),
            )
            .await
            .expect("the fixture row is written directly, bypassing the service");
    }
    // A healthy neighbour, so "the pass continues" is observable rather than
    // assumed.
    seed(&services, OWNER_TENANT).await;

    let report = services.schedules.fire_due_schedules_at(NOON_THIRTY).await;

    assert_eq!(
        report,
        ScheduleTickReport {
            evaluated: 2,
            fired: 1,
            lost: 0,
            failed: 1,
            deferred: 0,
        },
        "the corrupt row is refused and the healthy one still fires"
    );
    assert!(logs_contain("nil tenant id"));
    assert_eq!(
        fleet.admitter.admitted().len(),
        1,
        "only the healthy schedule launched"
    );
    assert!(
        fleet.ticks_of(Uuid::nil()).await.is_empty(),
        "and the refused row wrote no claim under the platform-root identity"
    );
}

/// Absent and foreign are indistinguishable on every read and every write —
/// telling them apart is the cross-tenant existence oracle.
#[tokio::test]
async fn another_tenants_schedule_is_invisible() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let stored = seed(&services, OWNER_TENANT).await;
    let stranger = ctx(OTHER_TENANT);

    assert!(
        matches!(
            services.schedules.get(&stranger, stored.id).await,
            Err(DomainError::ScheduleNotFound { .. })
        ),
        "a foreign schedule must read as not-found, never as forbidden"
    );
    assert!(services.schedules.list(&stranger).await.unwrap().is_empty());
    assert!(matches!(
        services
            .schedules
            .update(&stranger, stored.id, schedule_payload("hijacked", HOURLY))
            .await,
        Err(DomainError::ScheduleNotFound { .. })
    ));
    assert!(matches!(
        services.schedules.delete(&stranger, stored.id).await,
        Err(DomainError::ScheduleNotFound { .. })
    ));
    // The notification edit is a **mutating cross-gear** method — qa-insights is
    // meant to reach it over the SDK — so it is listed here rather than left to
    // "the same scope as `update`". It resolves its own `AccessScope` under
    // `actions::UPDATE`, and the repository filters before it scopes, exactly as
    // the other four do.
    assert!(
        matches!(
            services
                .schedules
                .update_notifications(
                    &stranger,
                    stored.id,
                    qa_runs_sdk::ScheduleNotificationSettings {
                        slack_enabled: true,
                        slack_channel: Some("#stranger".to_owned()),
                        slack_events: vec!["failed".to_owned()],
                    },
                )
                .await,
            Err(DomainError::ScheduleNotFound { .. })
        ),
        "a foreign schedule's Slack settings must read as not-found, never as forbidden"
    );

    // The owner's row is untouched by all of that.
    let mine = services
        .schedules
        .get(&ctx(OWNER_TENANT), stored.id)
        .await
        .unwrap();
    assert_eq!(mine.name, "nightly");
    assert!(
        !mine.slack_notifications_enabled,
        "the stranger's edit must not have reached the row"
    );
    assert_eq!(mine.slack_channel, None);
    assert!(mine.slack_notification_events.is_empty());
}
