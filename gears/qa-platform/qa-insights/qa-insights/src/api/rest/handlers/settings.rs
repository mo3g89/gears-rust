//! Handlers for the tenant-scoped settings surfaces under
//! `/qa/v1/settings/...`.
//!
//! # This module is founded by Task 32 and is expected to grow twice
//!
//! Stated up front because the plan lists these files under Task 38, which
//! would have left Task 32 unable to register the routes its own text mandates;
//! controller ruling R69 moves the module here. The two extensions are known and
//! neither should surprise a reader:
//!
//! * **Task 35** adds `GET/PUT /qa/v1/settings/jira-poller`, over
//!   [`JiraService::poller_config`](crate::domain::service::jira::JiraService::poller_config)
//!   and
//!   [`..::save_poller_config`](crate::domain::service::jira::JiraService::save_poller_config),
//!   which already existed — legacy's `api_get_jira_poller`/`api_update_jira_poller`
//!   (`manager/src/routes/settings.rs:510-542`). [`get_jira_poller_settings`]
//!   and [`update_jira_poller_settings`] are the two handlers this adds.
//! * **Task 38** adds the four notification-settings routes:
//!   [`get_notification_settings`]/[`update_notification_settings`],
//!   [`get_notification_log`] and the two non-CRUD handlers,
//!   [`test_notification`]/[`preview_notification`] — which is why this
//!   file's discipline below ("each handler is a decode, a call and a
//!   render") is stated as a *rule*, not only observed from the two JIRA
//!   pairs: it holds for a send and a render too, not only for a
//!   settings-page CRUD pair.
//!
//! Every one of those ten is the same shape, so the file is organised by
//! surface (a `// ==== JIRA ====`/`// ==== Notifications ====` band per
//! surface) rather than by verb.
//!
//! # The credential never reaches this layer, in either direction
//!
//! [`get_jira_settings`] renders a credential-store *reference*; there is no
//! masking step because there is nothing to mask.
//! `domain::service::jira`'s header carries the full argument and
//! `api::rest::dto`'s `the_jira_settings_response_has_no_api_token_field` is the
//! test that keeps the legacy field name from coming back.
//!
//! # No scope, no validation, no status-code decision
//!
//! `handlers::analytics`' and `handlers::saved_views`' discipline: each handler
//! is a decode, a call and a render.
//! [`crate::domain::service::jira::JiraService`] owns the authorization
//! decision, the defaults and the "an omitted reference keeps the stored one"
//! rule; [`as_jira_error`] owns the resource attribution.

use std::sync::Arc;

use axum::Extension;
use axum::extract::Query;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{
    JiraPollerConfigDto, JiraSettingsDto, NotificationConfigDto, NotificationLogEntryDto,
    NotificationLogQuery, NotificationPreviewDto, NotificationPreviewReq,
    NotificationTestOutcomeDto, NotificationTestReq,
};
use crate::api::rest::error::{as_jira_error, as_notification_error};
use crate::domain::service::jira::JiraConfigInput;
use crate::domain::service::notify::TestSend;
use crate::gear::ConcreteAppServices;

// ==================== JIRA (Task 32) ====================

/// `GET /qa/v1/settings/jira` — the tenant's JIRA settings.
///
/// `api_get_jira` (`manager/src/routes/settings.rs:248-275`). A tenant that has
/// never configured JIRA gets legacy's all-blank document rather than a 404 —
/// `domain::service::jira`'s header, item 1.
#[tracing::instrument(skip(svc, ctx))]
pub async fn get_jira_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<JiraSettingsDto>> {
    let config = svc
        .jira
        .get_jira_config(&ctx)
        .await
        .map_err(as_jira_error)?;
    Ok(Json(JiraSettingsDto::from(config)))
}

/// `PUT /qa/v1/settings/jira` — store the tenant's JIRA settings.
///
/// `api_update_jira` (`manager/src/routes/settings.rs:278-305`), including the
/// write half of legacy's masking mechanism: an empty
/// `api_token_credstore_ref` keeps the stored one, which is what legacy's
/// `"********"` sentinel does for a value that *is* masked. The service's
/// `save_jira_config` carries the argument.
///
/// # `req` is skipped on the span
///
/// `handlers::saved_views::create_saved_view`'s reason — an unvalidated body is
/// caller-controlled text with no bound this handler enforces — plus one
/// specific to this body: it names a credential-store reference, and a reference
/// is a locator into the credential store. Recording it would put an inventory
/// of every tenant's secret names into the trace pipeline.
#[tracing::instrument(skip(svc, ctx, req))]
pub async fn update_jira_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<JiraSettingsDto>,
) -> ApiResult<Json<JiraSettingsDto>> {
    let stored = svc
        .jira
        .save_jira_config(&ctx, JiraConfigInput::from(req))
        .await
        .map_err(as_jira_error)?;
    Ok(Json(JiraSettingsDto::from(stored)))
}

// ==================== JIRA poller (Task 35) ====================

/// `GET /qa/v1/settings/jira-poller` — the tenant's poller cadence and
/// auto-rerun switch.
///
/// `api_get_jira_poller` (`manager/src/routes/settings.rs:510-524`), over
/// [`JiraService::poller_config`](crate::domain::service::jira::JiraService::poller_config),
/// which already supplies legacy's defaults (300 seconds, auto-rerun on) and
/// the `.max(1)` clamp on read — this handler adds no defaulting of its own.
#[tracing::instrument(skip(svc, ctx))]
pub async fn get_jira_poller_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<JiraPollerConfigDto>> {
    let config = svc.jira.poller_config(&ctx).await.map_err(as_jira_error)?;
    Ok(Json(JiraPollerConfigDto::from(config)))
}

/// `PUT /qa/v1/settings/jira-poller` — store the tenant's poller settings.
///
/// `api_update_jira_poller` (`manager/src/routes/settings.rs:527-542`), over
/// [`JiraService::save_poller_config`](crate::domain::service::jira::JiraService::save_poller_config).
/// Stored as given; the `.max(1)` clamp is applied on the read, not here — see
/// that method's own doc for why.
#[tracing::instrument(skip(svc, ctx))]
pub async fn update_jira_poller_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<JiraPollerConfigDto>,
) -> ApiResult<Json<JiraPollerConfigDto>> {
    let stored = svc
        .jira
        .save_poller_config(&ctx, req.into())
        .await
        .map_err(as_jira_error)?;
    Ok(Json(JiraPollerConfigDto::from(stored)))
}

// ==================== Notifications (Task 38) ====================

/// `GET /qa/v1/settings/notifications` — the tenant's notification settings.
///
/// `api_get_notifications` (`manager/src/routes/settings.rs:434-444`). A
/// tenant that has never saved settings gets
/// `qa_insights_sdk::NotificationConfig::default()` rather than a 404 —
/// `domain::service::notify::NotifyService::get_config`'s own doc.
#[tracing::instrument(skip(svc, ctx))]
pub async fn get_notification_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<NotificationConfigDto>> {
    let config = svc
        .notify
        .get_config(&ctx)
        .await
        .map_err(as_notification_error)?;
    Ok(Json(NotificationConfigDto::from(config)))
}

/// `PUT /qa/v1/settings/notifications` — store the tenant's notification
/// settings.
///
/// `api_update_notifications` (`manager/src/routes/settings.rs:447-462`).
/// Stored as sent once validated, with no masking on the way back out — see
/// `NotifyService::save_config`'s own doc for the `slack_webhook_credstore_ref`
/// syntax check (Phase C's final review, Important 1) and for why this surface
/// has none of `handlers::update_jira_settings`'s credential-*preservation*
/// logic. `req` is still in `skip(..)` below: the reference is not secret
/// material, but it names one, and a settings body has no business in a span.
#[tracing::instrument(skip(svc, ctx, req))]
pub async fn update_notification_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<NotificationConfigDto>,
) -> ApiResult<Json<NotificationConfigDto>> {
    let stored = svc
        .notify
        .save_config(&ctx, req.into())
        .await
        .map_err(as_notification_error)?;
    Ok(Json(NotificationConfigDto::from(stored)))
}

/// `GET /qa/v1/settings/notifications/log` — the most recent audit entries.
///
/// `api_get_notification_log` (`manager/src/routes/settings.rs:752-773`).
/// `limit` reaches the service unchanged; an absent one defaults to 100 and
/// any value is clamped to at most 500 there, not here.
#[tracing::instrument(skip(svc, ctx), fields(notify.limit = ?query.limit))]
pub async fn get_notification_log(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<NotificationLogQuery>,
) -> ApiResult<Json<Vec<NotificationLogEntryDto>>> {
    let entries = svc
        .notify
        .list_log(&ctx, query.limit)
        .await
        .map_err(as_notification_error)?;
    Ok(Json(
        entries
            .into_iter()
            .map(NotificationLogEntryDto::from)
            .collect(),
    ))
}

/// `POST /qa/v1/settings/notifications/test` — send a test notification.
///
/// `api_test_notifications` (`manager/src/routes/settings.rs:465-496`). An
/// absent body is the generic settings-page test; a present one carries a
/// config override and an event, and tests the scheduled-run Slack template
/// instead. Unlike every other handler in this file, a failure here is not
/// necessarily the caller's own request being wrong — see
/// `NotifyService::send_test`'s `# Errors` for the three shapes a failure can
/// take, all of which this handler renders through the same
/// [`as_notification_error`].
///
/// # `req` is skipped on the span
///
/// [`update_jira_settings`]'s reason: an unvalidated body naming a credential
/// reference and, potentially, a Slack webhook reference of its own.
#[tracing::instrument(skip(svc, ctx, req))]
pub async fn test_notification(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    req: Option<Json<NotificationTestReq>>,
) -> ApiResult<Json<NotificationTestOutcomeDto>> {
    let request = match req {
        Some(Json(body)) => TestSend::ScheduledRun {
            config: Box::new(body.config.into()),
            event: body.event,
        },
        None => TestSend::Generic,
    };
    svc.notify
        .send_test(&ctx, request)
        .await
        .map_err(as_notification_error)?;
    Ok(Json(NotificationTestOutcomeDto {
        status: "sent".to_owned(),
    }))
}

/// `POST /qa/v1/settings/notifications/preview` — render a scheduled-run
/// preview without sending anything.
///
/// `api_preview_notifications` (`manager/src/routes/settings.rs:499-507`).
///
/// # `req` is skipped on the span
///
/// [`test_notification`]'s reason.
#[tracing::instrument(skip(svc, ctx, req))]
pub async fn preview_notification(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<NotificationPreviewReq>,
) -> ApiResult<Json<NotificationPreviewDto>> {
    let preview = svc
        .notify
        .preview_scheduled_run(&ctx, req.config.into(), &req.event)
        .await
        .map_err(as_notification_error)?;
    Ok(Json(NotificationPreviewDto::from(preview)))
}

#[cfg(test)]
#[path = "settings_handler_tests.rs"]
mod settings_handler_tests;
