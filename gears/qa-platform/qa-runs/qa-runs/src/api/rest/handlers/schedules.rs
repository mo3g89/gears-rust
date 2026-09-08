//! Handlers for `/qa/v1/schedules`.
//!
//! Glue only, and unusually plainly so: [`ScheduleService`] owns the empty-name
//! check, the cron parse, the policy scope for every repository call and the
//! not-found-versus-forbidden collapse, so each handler below is a decode, a
//! call and a render.
//!
//! [`ScheduleService`]: crate::domain::service::schedules::ScheduleService

use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use axum::response::IntoResponse;
use qa_runs_sdk as sdk;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::dto::{NewScheduleReq, ScheduleDto, UpdateScheduleNotificationsReq};
use crate::api::rest::error::as_schedule_error;
use crate::gear::ConcreteAppServices;

/// `GET /qa/v1/schedules`
///
/// Every schedule the caller can see, by name. Unpaginated, matching the
/// service: schedules are operator-authored and are bounded by how many an
/// operator writes, unlike `qa_runs`, which grows on its own and is the reason
/// the runs list is a `Page`.
///
/// **That bound is per tenant, and the query has no `LIMIT`.**
/// `SchedulesRepository::list` issues a scoped `find()` with an `ORDER BY` and
/// nothing else, and an `AccessScope` can span tenants - so a platform-wide
/// reader materialises every tenant's schedules into one body. No knob bounds
/// it, which makes this the one list in this gear whose response size nothing
/// caps. Recorded as a tracked follow-up rather than capped here, because a cap
/// without a cursor silently truncates and this endpoint has no cursor to
/// offer.
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_schedules(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<Vec<ScheduleDto>>> {
    let schedules = svc.schedules.list(&ctx).await.map_err(as_schedule_error)?;
    Ok(Json(schedules.into_iter().map(ScheduleDto::from).collect()))
}

/// `GET /qa/v1/schedules/{id}`
///
/// A schedule that does not exist and one belonging to another tenant are the
/// same 404, which is the property every read in this gear preserves.
#[tracing::instrument(skip(svc, ctx), fields(schedule.id = %id))]
pub async fn get_schedule(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ScheduleDto>> {
    let schedule = svc
        .schedules
        .get(&ctx, id)
        .await
        .map_err(as_schedule_error)?;
    Ok(Json(ScheduleDto::from(schedule)))
}

/// Decode a schedule payload, attributing any refusal to the **schedule**.
///
/// Both writes go through this, and until the newtype below landed that was a
/// convention rather than a rule: `req.try_into()?` used to compile and to
/// silently report a schedule's rejected `name` as the *run* resource's, which
/// is what this endpoint shipped and what the live smoke caught.
///
/// **It no longer compiles**, because
/// [`ScheduleFieldError`](crate::api::rest::dto::ScheduleFieldError) has no
/// `Into<CanonicalError>` - verified by making the edit and building, not by
/// reading the trait bounds. That is defence in depth; the guarantee itself is
/// [`handler_tests::the_create_handler_attributes_a_service_refusal_to_the_schedule`].
///
/// Every `Validation` reachable from here is a schedule's field, including the
/// `target.*` ones `RunTargetDto`'s shared `TryFrom` raises: the input type is
/// `NewScheduleReq` and the caller is creating or replacing a schedule, so the
/// attribution is static rather than inferred. See
/// [`as_schedule_error`](crate::api::rest::error::as_schedule_error).
fn decode_payload(req: NewScheduleReq) -> Result<sdk::NewSchedule, CanonicalError> {
    sdk::NewSchedule::try_from(req).map_err(|e| as_schedule_error(e.into_domain()))
}

/// `POST /qa/v1/schedules`
///
/// 201 with the stored schedule and a `Location` header. A name already taken
/// within the tenant is a 409 rather than a 500: the repository maps the unique
/// violation to `DomainError::ScheduleNameExists`, and an unparseable cron
/// expression is a 400 from `ScheduleService::validate` rather than a schedule
/// that is stored and silently never fires.
///
/// The **service call** is wrapped as well as the decode, because
/// `ScheduleService::validate`'s empty-name refusal is a `Validation` raised
/// inside it and would otherwise be attributed to the run. That costs nothing:
/// [`as_schedule_error`](crate::api::rest::error::as_schedule_error) passes
/// every non-`Validation` straight to the ordinary mapping, so
/// `ScheduleNameExists`, `Forbidden` and `Database` are untouched.
///
/// # Nothing caller-supplied reaches the span
///
/// **`uri` is skipped as well as `req`, and leaving it out was a live leak.**
/// `#[tracing::instrument]` records every argument it is not told to skip, so
/// the full request target - query string included - was being written at DEBUG
/// and inherited by every child span: measured as
/// `create_schedule{uri=/qa/v1/schedules?leaked_secret=hunter2}`. A query string
/// is caller-controlled and bounded only by the request-line limit, and
/// `created_json` reads `uri.path()`, so nothing here ever needed the query.
///
/// `req` is skipped for the adjacent reason: a span field is attached on
/// **entry**, before `decode_payload` applies
/// [`MAX_SCHEDULE_NAME_LEN`](crate::api::rest::dto::MAX_SCHEDULE_NAME_LEN), so a
/// `schedule.name` field would have been bounded by the gateway's body limit
/// rather than by the column. Every other handler in this gear records ids and nothing
/// caller-supplied.
///
/// The same omission exists in the sibling gears' create handlers, which take a
/// `Uri` the same way and do not skip it. Not this task's to change, and
/// recorded here rather than nowhere.
#[tracing::instrument(skip(svc, ctx, req, uri))]
pub async fn create_schedule(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<NewScheduleReq>,
) -> ApiResult<impl IntoResponse> {
    let new = decode_payload(req)?;
    let schedule = svc
        .schedules
        .create(&ctx, new)
        .await
        .map_err(as_schedule_error)?;
    let id = schedule.id.to_string();
    Ok(created_json(ScheduleDto::from(schedule), &uri, &id).into_response())
}

/// `PUT /qa/v1/schedules/{id}`
///
/// **A full replace, not a patch.** Every caller-decidable field is taken from
/// the body, including `enabled` - which the body is required to state, for the
/// reason `NewScheduleReq` gives. What the body cannot touch is
/// `last_fired_tick`: `sdk::NewSchedule` has no such field, so the body cannot
/// carry one, and `schedules_sea_repo`'s update leaves the column
/// `ActiveValue::Unchanged` - which is what
/// [`handler_tests::an_edit_does_not_rewind_a_schedule_that_has_already_fired`]
/// measures against a schedule that really has fired.
///
/// **Two concurrent replaces are last-writer-wins, and the loser leaves no
/// trace.** There is no version, `ETag` or `If-Match` on this resource, so the
/// second write overwrites the first wholesale and neither caller is told. Two
/// operators editing one schedule in the same minute is the realistic case;
/// what they lose is one whole edit, not a merged field.
#[tracing::instrument(skip(svc, ctx, req), fields(schedule.id = %id))]
pub async fn replace_schedule(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req): Json<NewScheduleReq>,
) -> ApiResult<Json<ScheduleDto>> {
    let new = decode_payload(req)?;
    let schedule = svc
        .schedules
        .update(&ctx, id, new)
        .await
        .map_err(as_schedule_error)?;
    Ok(Json(ScheduleDto::from(schedule)))
}

/// `PUT /qa/v1/schedules/{id}/notifications`
///
/// **Three fields, and nothing else on the schedule moves.** That is the half of
/// the source system's endpoint that is behaviour rather than annotation
/// plumbing - *"this endpoint edits Slack settings only, so a schedule pinned to
/// exclusive (or to parallel) must come back pinned the same way"*
/// (`manager/src/routes/schedules.rs:854-856`). Legacy needs a whole
/// `CreateScheduleForm` copied field by field to keep that true, because it
/// deletes and recreates the `CronWorkflow`; here the other columns are simply
/// not in the `UPDATE`.
///
/// **`PUT`, where legacy is `POST`** (`manager/src/routes/mod.rs:95-98`) - a
/// deliberate adaptation of a surface that is not frozen, argued at
/// [`UpdateScheduleNotificationsReq`].
///
/// The service call is wrapped in `as_schedule_error` for the reason
/// [`create_schedule`] states: both of `validate_notifications`' refusals are
/// `Validation`s raised *inside* the service, and would otherwise be attributed
/// to the run.
#[tracing::instrument(skip(svc, ctx, req), fields(schedule.id = %id))]
pub async fn update_schedule_notifications(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateScheduleNotificationsReq>,
) -> ApiResult<Json<ScheduleDto>> {
    let schedule = svc
        .schedules
        .update_notifications(&ctx, id, sdk::ScheduleNotificationSettings::from(req))
        .await
        .map_err(as_schedule_error)?;
    Ok(Json(ScheduleDto::from(schedule)))
}

/// `DELETE /qa/v1/schedules/{id}`
///
/// 204, and the schedule's tick rows go with it by cascade. Not idempotent, and
/// deliberately unlike `cancel_run`: a second delete answers 404, because
/// nothing about a schedule makes "it was already gone" the same answer as "it
/// is gone now" - a caller deleting an id they no longer own should be told.
#[tracing::instrument(skip(svc, ctx), fields(schedule.id = %id))]
pub async fn delete_schedule(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.schedules
        .delete(&ctx, id)
        .await
        .map_err(as_schedule_error)?;
    Ok(no_content().into_response())
}

#[cfg(test)]
mod tests {
    use super::decode_payload;
    use crate::api::rest::dto::{MAX_SCHEDULE_NAME_LEN, NewScheduleReq, RunTargetDto};
    use toolkit::api::canonical_prelude::Problem;
    use uuid::Uuid;

    fn payload() -> NewScheduleReq {
        NewScheduleReq {
            name: "nightly".to_owned(),
            target: RunTargetDto {
                kind: "custom_plan".to_owned(),
                repo_id: None,
                path: None,
                test_file: None,
                custom_plan_id: Some(Uuid::from_u128(0x41)),
                collect_url: None,
            },
            environment_id: None,
            branch: None,
            cron: "0 3 * * *".to_owned(),
            exclusive_choice: "auto".to_owned(),
            enabled: true,
            include_tags: vec![],
            exclude_tags: vec![],
            parameters: vec![],
            legacy_platform_id: None,
        }
    }

    /// The body a client receives, as JSON, for the reason `domain::error`'s
    /// own test helper gives: `Problem` is the type that becomes the response, and a
    /// `Debug` rendering is a superset of the wire.
    fn wire(req: NewScheduleReq) -> (u16, String) {
        let error = decode_payload(req).expect_err("this payload must be refused");
        let problem = Problem::from_error(&error).expect("a problem must serialize");
        let status = problem.status.expect("a problem always carries a status");
        (
            status,
            serde_json::to_string(&problem).expect("a problem must serialize"),
        )
    }

    /// **A schedule's field violation names the schedule resource, not the
    /// run's.**
    ///
    /// `DomainError::Validation` carries no resource, so the blanket `From` impl
    /// attributes every one of them to `cf.qa.runs.run.v1~` - and tells a caller
    /// that a *run* rejected the schedule they were creating.
    ///
    /// Bypassing [`decode_payload`] with `req.try_into()?` no longer compiles
    /// (see `ScheduleFieldError`), but dropping the attribution *inside* it
    /// still does: `map_err(CanonicalError::from)` builds, behaves identically
    /// in every other respect, and reds this test and nothing else - measured,
    /// not assumed.
    ///
    /// All three of the payload's own refusals are covered, plus a `target.*`
    /// one from the `TryFrom` this DTO shares with the launch path - that arm is
    /// the reason the attribution has to be static rather than inferred from the
    /// field name.
    #[test]
    fn every_refusal_from_a_schedule_payload_names_the_schedule_resource() {
        let over_long = "n".repeat(MAX_SCHEDULE_NAME_LEN + 1);
        let cases = [
            (
                "name",
                NewScheduleReq {
                    name: over_long,
                    ..payload()
                },
            ),
            (
                "branch",
                NewScheduleReq {
                    branch: Some("b".repeat(1024)),
                    ..payload()
                },
            ),
            (
                "exclusive_choice",
                NewScheduleReq {
                    exclusive_choice: "maybe".to_owned(),
                    ..payload()
                },
            ),
            (
                "target.custom_plan_id",
                NewScheduleReq {
                    target: RunTargetDto {
                        custom_plan_id: None,
                        ..payload().target
                    },
                    ..payload()
                },
            ),
        ];

        for (field, req) in cases {
            let (status, body) = wire(req);
            assert_eq!(status, 400, "{field}: {body}");
            assert!(
                body.contains("cf.qa.runs.schedule.v1~"),
                "{field} must be attributed to the schedule resource: {body}"
            );
            assert!(
                !body.contains("cf.qa.runs.run.v1~"),
                "{field} must not be attributed to the run resource: {body}"
            );
            assert!(
                body.contains(field),
                "{field} must be named as the violated field: {body}"
            );
        }
    }

    /// **The refusal raised *inside* the service call is attributed too.**
    ///
    /// `ScheduleService::validate`'s empty-name check is the one `Validation` in
    /// the whole `create`/`update` call graph, and it is returned from the
    /// service rather than from [`decode_payload`] - so it was still reported as
    /// the run's after the decode was fixed. The comment justifying that said
    /// reaching it would mean re-attributing `ScheduleNameExists`, `Forbidden`
    /// and `Database` along with it, which the fall-through arm of
    /// `as_schedule_error` disproves.
    ///
    /// Driven through `as_schedule_error` directly, which pins the *function*.
    /// The composition - that the handlers actually apply it - is pinned
    /// separately and for real by
    /// [`the_create_handler_attributes_a_service_refusal_to_the_schedule`].
    #[test]
    fn the_empty_name_refusal_from_the_service_names_the_schedule_resource() {
        let empty = crate::domain::error::DomainError::Validation {
            field: "name".to_owned(),
            message: "a schedule name must not be empty".to_owned(),
        };
        let error = crate::api::rest::error::as_schedule_error(empty);
        let problem = Problem::from_error(&error).expect("a problem must serialize");
        let body = serde_json::to_string(&problem).expect("a problem must serialize");

        assert_eq!(problem.status, Some(400), "{body}");
        assert!(body.contains("cf.qa.runs.schedule.v1~"), "{body}");
        assert!(!body.contains("cf.qa.runs.run.v1~"), "{body}");
    }

    /// **The fall-through leaves everything else exactly as it was**, which is
    /// what makes wrapping a whole service call safe rather than a widening of
    /// the re-attribution.
    ///
    /// `ScheduleNameExists` and `ScheduleNotFound` already carry the schedule's
    /// type; `Database` is the one that must not acquire a schedule's identity
    /// or stop being an opaque 500.
    ///
    /// **`Forbidden` is deliberately absent from this list** and is asserted
    /// separately below — it is re-attributed on purpose, which is what makes
    /// this test's job "everything *else*" rather than "everything".
    #[test]
    fn every_other_error_passes_through_the_attribution_untouched() {
        use crate::domain::error::DomainError;

        /// One case: the status it must keep, and a way to mint the error
        /// twice. A named alias because `clippy::type_complexity` is denied.
        type Case = (u16, fn(Uuid) -> DomainError);

        let id = Uuid::from_u128(0x42);
        // Factories rather than values: `DomainError` is not `Clone`, and each
        // case needs the same error twice - once mapped directly, once through
        // the attribution - to compare the two renderings.
        let cases: [Case; 3] = [
            (409, |_| DomainError::ScheduleNameExists {
                name: "nightly".to_owned(),
            }),
            (404, |id| DomainError::ScheduleNotFound { id }),
            (500, |_| DomainError::database("driver text")),
        ];

        for (expected_status, make) in cases {
            let direct: toolkit::api::canonical_prelude::CanonicalError = make(id).into();
            let wrapped = crate::api::rest::error::as_schedule_error(make(id));
            assert_eq!(
                wrapped.status_code(),
                direct.status_code(),
                "the attribution must not change this error's status"
            );
            assert_eq!(wrapped.status_code(), expected_status);

            let wrapped_body = serde_json::to_string(
                &Problem::from_error(&wrapped).expect("a problem must serialize"),
            )
            .expect("a problem must serialize");
            let direct_body = serde_json::to_string(
                &Problem::from_error(&direct).expect("a problem must serialize"),
            )
            .expect("a problem must serialize");
            // Rendered identically, trace id aside - which is what "untouched"
            // has to mean for this to be a safe thing to wrap a service call in.
            assert_eq!(
                strip_trace(&wrapped_body),
                strip_trace(&direct_body),
                "the attribution altered a non-Validation error"
            );
        }
    }

    /// **A denial names the schedule, and still names nothing else.**
    ///
    /// The re-attribution has to change exactly one thing about a 403. Both
    /// halves are asserted because the risk runs both ways: leaving it as the
    /// run's points an operator at the wrong permission, and "fixing" it by
    /// describing the denial would open the cross-tenant oracle every read in
    /// this gear is written to close.
    ///
    /// Asserted on the whole `cf.qa.runs.run.v1~` token, not on `"run "` with a
    /// trailing space — which is how `domain::error`'s own
    /// `forbidden_is_403_and_says_nothing_about_what_was_denied` missed this for
    /// the whole of Phase B. The run's gts id has no trailing space, so that
    /// test's oracle list could never have matched it.
    #[test]
    fn a_denial_is_re_attributed_and_still_describes_nothing() {
        use crate::domain::error::DomainError;

        let wrapped = crate::api::rest::error::as_schedule_error(DomainError::Forbidden);
        let body = serde_json::to_string(
            &Problem::from_error(&wrapped).expect("a problem must serialize"),
        )
        .expect("a problem must serialize");

        assert_eq!(wrapped.status_code(), 403, "{body}");
        assert!(body.contains("cf.qa.runs.schedule.v1~"), "{body}");
        assert!(
            !body.contains("cf.qa.runs.run.v1~"),
            "a denial on a schedule must not point at run permissions: {body}"
        );
        assert!(body.contains("ACCESS_DENIED"), "{body}");
        for oracle in ["tenant", "scope", "platform", "schedule '", "nightly"] {
            assert!(
                !body.to_lowercase().contains(oracle),
                "a denial must not describe what was denied ({oracle}): {body}"
            );
        }
    }

    /// `trace_id` is minted per `Problem`, so two renderings of the same error
    /// differ by it and by nothing else.
    fn strip_trace(body: &str) -> String {
        let Some(start) = body.find("\"trace_id\":") else {
            return body.to_owned();
        };
        let rest = &body[start + "\"trace_id\":".len()..];
        let end = rest.find(',').unwrap_or(rest.len());
        format!("{}{}", &body[..start], &rest[end..])
    }

    /// The other direction, so the re-attribution cannot be "fixed" by
    /// attributing everything to the schedule: a payload with nothing wrong with
    /// it must convert, and the fields must survive.
    #[test]
    fn a_valid_payload_converts_untouched() {
        let new = decode_payload(payload()).expect("a valid payload converts");
        assert_eq!(new.name, "nightly");
        assert_eq!(new.cron, "0 3 * * *");
        assert_eq!(new.exclusive_choice, None, "`auto` is the inherit tier");
        assert!(new.enabled);
    }
}

#[cfg(test)]
#[path = "schedules_handler_tests.rs"]
mod handler_tests;
