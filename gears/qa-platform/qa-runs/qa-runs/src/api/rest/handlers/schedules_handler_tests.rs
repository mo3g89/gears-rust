//! The handler functions driven over a **real** `ConcreteAppServices`.
//!
//! `#[path]`-included from `handlers::schedules`, matching the convention
//! `domain::service` uses for its own suites - the module is 400 lines against
//! the 450 of the file it tests, and inlining both would have made one file
//! twice the size of every other handler module in the gear.
//!
//! # This module exists because two doc comments claimed it could not
//!
//! Task 20 shipped, twice, the sentence that closing the composition gap "needs
//! a test that can build a `ConcreteAppServices`, which is the crate-wide
//! harness this gear still does not have". `domain::service::test_support`'s
//! `Fleet::instance` had been returning exactly that since Task 19 — real
//! `Orm*Repository` trio, real migrator, real `PolicyEnforcer` — and five other
//! sites already built one. The claim cost nothing to check and neither author
//! checked it. It is the sixth false impossibility in this gear and the first
//! that was disclosed as a limitation and then *planned around*.
//!
//! # No router, no server
//!
//! The handlers are plain `async fn`s, so the extractors are constructed by
//! hand: `Extension(ctx)`, `Extension(services)`, `Path(id)`, `Json(req)`. That
//! leaves the routing table unexercised here — `routes::tests` is what covers
//! that — and exercises everything from the handler body inward, which is the
//! half that was untested.
use std::sync::Arc;

use axum::extract::Path;
use axum::http::Uri;
use axum::{Extension, Json};
use toolkit::api::canonical_prelude::IntoResponse;
use uuid::Uuid;

use super::{
    create_schedule, delete_schedule, get_schedule, list_schedule_ticks, list_schedules,
    replace_schedule, update_schedule_notifications,
};
use crate::api::rest::dto::{NewScheduleReq, RunTargetDto, UpdateScheduleNotificationsReq};
use crate::domain::service::test_support::{Fleet, ctx};

const TENANT: Uuid = Uuid::from_u128(0x0A11_0000_0000_0001);

fn payload(name: &str) -> NewScheduleReq {
    NewScheduleReq {
        name: name.to_owned(),
        target: RunTargetDto {
            kind: "plan".to_owned(),
            repo_id: Some(Uuid::from_u128(0x0B01)),
            path: Some("plans/smoke.yaml".to_owned()),
            test_file: None,
            custom_plan_id: None,
            collect_url: None,
        },
        environment_id: None,
        branch: Some("main".to_owned()),
        cron: "0 3 * * *".to_owned(),
        exclusive_choice: "auto".to_owned(),
        enabled: true,
        include_tags: vec![],
        exclude_tags: vec![],
        parameters: vec![],
        legacy_platform_id: None,
    }
}

/// Render whatever a handler answered into `(status, headers, body)`.
async fn rendered(response: axum::response::Response) -> (u16, Vec<(String, String)>, String) {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_owned(),
                v.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// **The composition step, pinned at last.**
///
/// `ScheduleService::validate`'s empty-name refusal is raised *inside* the
/// service call, so only the handler's own `.map_err(as_schedule_error)`
/// re-attributes it. Deleting that one call from `create_schedule` leaves
/// every other test in this crate green and turns this red with the exact
/// diagnostic - `cf.qa.runs.run.v1~` on a schedule's field.
///
/// This is what the two retracted doc comments said was impossible.
#[tokio::test]
async fn the_create_handler_attributes_a_service_refusal_to_the_schedule() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let response = create_schedule(
        Uri::from_static("/qa/v1/schedules"),
        Extension(ctx(TENANT)),
        Extension(services),
        Json(payload("   ")),
    )
    .await
    .into_response();

    let (status, _, body) = rendered(response).await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("cf.qa.runs.schedule.v1~"),
        "a schedule's empty name must be attributed to the schedule: {body}"
    );
    assert!(
        !body.contains("cf.qa.runs.run.v1~"),
        "and never to the run: {body}"
    );
    assert!(body.contains("name"), "{body}");
}

/// The 201 contract, which nothing pinned: the status, the `Location`
/// header `created_json` builds, and that the body is the stored schedule.
///
/// `created_json` joins `uri.path()` with the new id, so a handler that
/// passed the wrong `Uri` - or that answered 200 - would publish a
/// `Location` a client cannot follow. Only a real response can show that.
#[tokio::test]
async fn the_create_handler_answers_201_with_a_followable_location() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let response = create_schedule(
        Uri::from_static("/qa/v1/schedules"),
        Extension(ctx(TENANT)),
        Extension(services),
        Json(payload("nightly")),
    )
    .await
    .into_response();

    let (status, headers, body) = rendered(response).await;
    assert_eq!(status, 201, "{body}");

    let id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .expect("the body carries the stored id")
        .parse()
        .unwrap();
    assert_eq!(
        header(&headers, "location"),
        Some(format!("/qa/v1/schedules/{id}").as_str()),
        "Location must address the row that was just created: {headers:?}"
    );

    let rendered_body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(rendered_body["name"], "nightly");
    assert_eq!(rendered_body["exclusive_choice"], "auto");
    assert_eq!(rendered_body["enabled"], true);
}

/// A full CRUD round trip through the handlers, which is what pins that the
/// five of them agree about one resource: the id the create minted is the
/// one the get, the replace and the delete address, and the list sees the
/// same row.
///
/// The replace deliberately restates `enabled: false` and changes only the
/// cron - the legacy quirk this endpoint exists not to repeat - and the
/// assertion is that the schedule comes back **paused**.
#[tokio::test]
async fn the_handlers_agree_about_one_schedule_across_a_full_round_trip() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let (_, _, created) = rendered(
        create_schedule(
            Uri::from_static("/qa/v1/schedules"),
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Json(payload("nightly")),
        )
        .await
        .into_response(),
    )
    .await;
    let id: Uuid = serde_json::from_str::<serde_json::Value>(&created).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let listed = list_schedules(Extension(ctx(TENANT)), Extension(Arc::clone(&services)))
        .await
        .expect("the list must succeed");
    assert_eq!(listed.0.len(), 1);
    assert_eq!(listed.0[0].id, id);

    let fetched = get_schedule(
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&services)),
        Path(id),
    )
    .await
    .expect("the schedule must be readable");
    assert_eq!(fetched.0.cron, "0 3 * * *");

    let paused = replace_schedule(
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&services)),
        Path(id),
        Json(NewScheduleReq {
            cron: "30 2 * * 1-5".to_owned(),
            enabled: false,
            ..payload("nightly")
        }),
    )
    .await
    .expect("the replace must succeed");
    assert_eq!(paused.0.cron, "30 2 * * 1-5");
    assert!(
        !paused.0.enabled,
        "a replace that states `enabled: false` must leave the schedule paused"
    );
    assert_eq!(
        paused.0.last_fired_tick, None,
        "an edit must not move the fired-through cursor"
    );

    let (status, _, _) = rendered(
        delete_schedule(
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Path(id),
        )
        .await
        .expect("the delete must succeed")
        .into_response(),
    )
    .await;
    assert_eq!(status, 204);

    let after = list_schedules(Extension(ctx(TENANT)), Extension(services))
        .await
        .expect("the list must succeed");
    assert!(after.0.is_empty(), "the delete must remove the row");
}

/// **All five handlers attribute a denial to the schedule**, not just the
/// two that had a `map_err` before the 403 fix.
///
/// A loop rather than one case, because the residual it closes is precisely
/// the defect that was just fixed: `get`, `replace` and `delete` are the
/// three handlers that had **no** `map_err` call site at all, so dropping
/// one from any of them reintroduces "a schedule 403 names the run
/// resource" on that endpoint while every other test stays green. Measured:
/// before this test, deleting the wrapper from any of the three left 764
/// passing.
///
/// A 403 is the most likely error on a fresh deployment of these endpoints -
/// a policy engine that has not been taught `qa.schedule` refuses all five -
/// so this is the arm an operator is most likely to meet first, on whichever
/// endpoint they try.
#[tokio::test]
async fn every_handler_attributes_a_denial_to_the_schedule() {
    let fleet = Fleet::denying().await;
    let services = fleet.instance();
    let id = Uuid::from_u128(0x51);

    // `.into_response()` on the whole `Result` rather than `expect_err`:
    // the Ok types are not all `Debug`, and rendering either arm is what the
    // status assertion below reads anyway.
    let mut bodies: Vec<(&str, axum::response::Response)> = Vec::new();

    bodies.push((
        "list",
        list_schedules(Extension(ctx(TENANT)), Extension(Arc::clone(&services)))
            .await
            .into_response(),
    ));
    bodies.push((
        "get",
        get_schedule(
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Path(id),
        )
        .await
        .into_response(),
    ));
    bodies.push((
        "create",
        create_schedule(
            Uri::from_static("/qa/v1/schedules"),
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Json(payload("nightly")),
        )
        .await
        .into_response(),
    ));
    bodies.push((
        "replace",
        replace_schedule(
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Path(id),
            Json(payload("nightly")),
        )
        .await
        .into_response(),
    ));
    bodies.push((
        "delete",
        delete_schedule(
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Path(id),
        )
        .await
        .into_response(),
    ));

    for (handler, response) in bodies {
        let (status, _, body) = rendered(response).await;
        assert_eq!(status, 403, "{handler}: {body}");
        assert!(
            body.contains("cf.qa.runs.schedule.v1~"),
            "{handler}: a denial must name the schedule: {body}"
        );
        assert!(
            !body.contains("cf.qa.runs.run.v1~"),
            "{handler}: and must not point at run permissions: {body}"
        );
    }
}

/// **An edit cannot rewind the fired-through cursor**, measured against a
/// schedule that has actually fired.
///
/// **The column is already pinned three files away**, and the first version
/// of this comment claimed otherwise: it said nothing exercised the update
/// with a non-null cursor, which one grep for `advance_last_fired_tick`
/// falsifies. `schedules_sea_repo`'s
/// `an_update_replaces_every_field_but_the_fired_cursor` has done exactly
/// that since Task 17, at the repository.
///
/// What was missing is the same property through **this handler**: that
/// `replace_schedule` reaches that update rather than some other write, with
/// a `NewSchedule` that carries no cursor. The round trip above asserts
/// `last_fired_tick == None` on a schedule that never fired, which is
/// vacuous; this is the non-vacuous version, one layer up from the one that
/// was already covered.
///
/// If it were rewound, a cron edit would re-fire every occurrence since the
/// new cursor, which for an hourly destructive suite is the outcome
/// exactly-once exists to prevent.
#[tokio::test]
async fn an_edit_does_not_rewind_a_schedule_that_has_already_fired() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let (_, _, created) = rendered(
        create_schedule(
            Uri::from_static("/qa/v1/schedules"),
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Json(payload("nightly")),
        )
        .await
        .into_response(),
    )
    .await;
    let id: Uuid = serde_json::from_str::<serde_json::Value>(&created).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let fired_at = time::macros::datetime!(2026-08-13 03:00 UTC);
    fleet.advance_cursor(TENANT, id, fired_at).await;
    assert_eq!(
        fleet.cursor_of(TENANT, id).await,
        Some(fired_at),
        "premise: the schedule must really have a cursor for this to test anything"
    );

    let replaced = replace_schedule(
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&services)),
        Path(id),
        Json(NewScheduleReq {
            cron: "0 1 * * *".to_owned(),
            ..payload("nightly")
        }),
    )
    .await
    .expect("the replace must succeed");

    assert_eq!(replaced.0.cron, "0 1 * * *", "the edit must take effect");
    assert_eq!(
        replaced.0.last_fired_tick,
        Some(fired_at),
        "an edit must not rewind the cursor"
    );
    assert_eq!(
        fleet.cursor_of(TENANT, id).await,
        Some(fired_at),
        "and the stored column must agree with what was rendered"
    );
}

/// **The notification endpoint edits three fields and leaves the schedule
/// alone**, driven through the real handler over a real `ConcreteAppServices`.
///
/// The service suite proves the property at the service; this proves the
/// handler is wired to it and that `UpdateScheduleNotificationsReq`'s three
/// fields land on the three they name. A transposition of `slack_channel` and
/// one of the events would be a type error, but a handler calling
/// `update` instead of `update_notifications` would not be — and would wipe the
/// schedule.
#[tokio::test]
async fn the_notification_handler_edits_only_the_three_settings() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let created = create_schedule(
        Uri::from_static("/qa/v1/schedules"),
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&services)),
        Json(NewScheduleReq {
            exclusive_choice: "true".to_owned(),
            enabled: false,
            ..payload("nightly")
        }),
    )
    .await
    .into_response();
    let (status, _, body) = rendered(created).await;
    assert_eq!(status, 201, "{body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();

    let response = update_schedule_notifications(
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&services)),
        Path(id),
        Json(UpdateScheduleNotificationsReq {
            slack_enabled: true,
            slack_channel: Some("#qa-alerts".to_owned()),
            slack_events: vec!["failed".to_owned(), "error".to_owned()],
        }),
    )
    .await
    .into_response();

    let (status, _, body) = rendered(response).await;
    assert_eq!(status, 200, "{body}");
    let after: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(after["slack_notifications_enabled"], true, "{body}");
    assert_eq!(after["slack_channel"], "#qa-alerts", "{body}");
    assert_eq!(
        after["slack_notification_events"],
        serde_json::json!(["failed", "error"]),
        "{body}"
    );
    assert_eq!(
        after["exclusive_choice"], "true",
        "a schedule pinned exclusive must come back pinned exclusive: {body}"
    );
    assert_eq!(
        after["enabled"], false,
        "and a paused one must stay paused: {body}"
    );
    assert_eq!(after["cron"], created["cron"], "{body}");
    assert_eq!(after["name"], created["name"], "{body}");
}

/// An event name outside legacy's six is a **400 attributed to the schedule**,
/// not a 500 and not the run's.
///
/// The refusal is raised inside `ScheduleService::validate_notifications`, so
/// only the handler's own `.map_err(as_schedule_error)` re-attributes it —
/// deleting that one call leaves every other test in this crate green.
#[tokio::test]
async fn an_unknown_event_is_a_400_attributed_to_the_schedule() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let response = update_schedule_notifications(
        Extension(ctx(TENANT)),
        Extension(services),
        Path(Uuid::from_u128(0x99)),
        Json(UpdateScheduleNotificationsReq {
            slack_enabled: true,
            slack_channel: None,
            // The Rust *variant* spelling, which is what the plan for this task
            // listed. It is not an event name; `in_progress` is.
            slack_events: vec!["InProgress".to_owned()],
        }),
    )
    .await
    .into_response();

    let (status, _, body) = rendered(response).await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("slack_events"), "{body}");
    assert!(
        body.contains("cf.qa.runs.schedule.v1~"),
        "a schedule's field violation must name the schedule: {body}"
    );
    assert!(
        !body.contains("cf.qa.runs.run.v1~"),
        "and must not name the run: {body}"
    );
}

/// `GET /qa/v1/schedules/{id}/ticks`, driven over a real fired-and-failed
/// claim: the wiring this task adds, not the outcome logic `schedules_tests`
/// already covers at the service.
#[tokio::test]
async fn the_ticks_handler_returns_the_fire_history_newest_first() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();

    let (_, _, created) = rendered(
        create_schedule(
            Uri::from_static("/qa/v1/schedules"),
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Json(payload("nightly")),
        )
        .await
        .into_response(),
    )
    .await;
    let id: Uuid = serde_json::from_str::<serde_json::Value>(&created).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let earlier = time::macros::datetime!(2026-08-13 03:00 UTC);
    let later = time::macros::datetime!(2026-08-13 04:00 UTC);
    let run_id = Uuid::new_v4();
    fleet
        .seed_tick(TENANT, id, earlier, Some(run_id), None)
        .await;
    fleet
        .seed_tick(TENANT, id, later, None, Some("launch refused"))
        .await;

    let response = list_schedule_ticks(
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&services)),
        Path(id),
    )
    .await
    .into_response();
    let (status, _, body) = rendered(response).await;
    assert_eq!(status, 200, "{body}");

    let ticks: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ticks = ticks.as_array().expect("the body is an array");
    assert_eq!(ticks.len(), 2, "{body}");
    assert_eq!(
        ticks[0]["error"], "launch refused",
        "newest due_at first: {body}"
    );
    assert_eq!(ticks[0]["run_id"], serde_json::Value::Null, "{body}");
    assert_eq!(ticks[1]["run_id"], run_id.to_string(), "{body}");
    assert_eq!(ticks[1]["error"], serde_json::Value::Null, "{body}");

    // Another tenant gets the same 404 as `get_schedule` for this id.
    let denied = list_schedule_ticks(
        Extension(ctx(Uuid::from_u128(0xBAD))),
        Extension(services),
        Path(id),
    )
    .await
    .into_response();
    assert_eq!(denied.status(), 404);
}
