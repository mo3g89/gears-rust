//! The tenant-settings routes under `/qa/v1/settings/...`. Task 32.
//!
//! # Founded here, and expected to grow twice
//!
//! The plan lists this file under Task 38 only, which would have left Task 32
//! unable to register the `settings/jira` pair its own text mandates; controller
//! ruling R69 moves the module here. `handlers::settings`' header names both
//! extensions — Task 35's `settings/jira-poller` pair and Task 38's four
//! notification routes — and each is a `router = OperationBuilder::…` block
//! beside the two below, in a band of its own. Nothing about adding one requires
//! reshaping this function.
//!
//! Two paths, four operations, registered in one function for
//! `routes::saved_views`' reason: neither description is long enough on its own
//! to justify a function, and `routes::dashboard`'s
//! two-operations-in-one-function is the closer precedent. Task 35 added the
//! second path, `/qa/v1/settings/jira-poller`, in its own band below, and Task
//! 38 the third, `/qa/v1/settings/notifications` plus its `/test`,
//! `/preview` and `/log` siblings — four paths, six operations, three bands.
//!
//! # What the descriptions must say, and why it is not decoration
//!
//! `api_token_credstore_ref` is the field an operator will get wrong, and it has
//! **two** unguessable contracts rather than one. Its *contents*: because oagw's
//! apikey auth plugin concatenates a prefix onto whatever the reference resolves
//! to (`gears/system/oagw/oagw/src/infra/plugin/apikey_auth.rs:60-61`), the
//! secret behind it must be `base64("email:api_token")` rather than the bare
//! token. Its *name*: the reference itself reaches `SecretRef::new` after a
//! `cred://` strip, and that accepts `[a-zA-Z0-9_-]` only
//! (`gears/credstore/credstore-sdk/src/models.rs:42-83`) — **added in fix round
//! 1, finding 1**, which found the syntax contract undocumented and violated by
//! every example in the change that introduced it.
//! `infra::jira::oagw_client`'s header carries the contents argument and
//! `domain::ports::jira_client::validate_credstore_ref` the name one; the `PUT`
//! description below is the only place an operator will ever read either, so it
//! states both in prose.
//!
//! # `slack_webhook_credstore_ref` has the same *name* contract, and the two
//! # descriptions used to disagree about it
//!
//! **Phase C's final review, Important 1 and 4.** The notification `GET`'s
//! description said the field "is a credential-store reference, never a URL"
//! and is "treated the same as a JIRA API token"; the `PUT`'s said "nothing
//! here is a secret in need of a keep-stored-value convention". Both sentences
//! were in one `OpenAPI` document, and nothing in the write path enforced
//! either reading — a Slack incoming-webhook URL, whose path *is* the secret,
//! stored cleanly.
//!
//! The posture is now the `GET`'s, because
//! `domain::service::notify::NotifyService::save_config` applies
//! `validate_credstore_ref` and makes it true. The `PUT`'s surviving claim is
//! the narrower, factual one it always had underneath: this endpoint has no
//! keep-stored-value convention, so an empty value *clears* the reference. That
//! is a statement about how the two `PUT`s treat absence — legacy's masking
//! sentinel has no equivalent on this document — and not about how sensitive
//! the column is.

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};

pub(super) fn register_settings_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // ==================== JIRA (Task 32) ====================

    // GET /qa/v1/settings/jira
    router = OperationBuilder::get("/qa/v1/settings/jira")
        .operation_id("qa_insights.get_jira_settings")
        .summary("Get the tenant's JIRA integration settings")
        .description(
            "The JIRA instance this tenant files bugs against. api_token_credstore_ref is a \
             credential-store reference and never a token - this endpoint cannot return \
             credential material, because the material is never stored here or held by this \
             service. A tenant that has never configured JIRA gets a document with empty \
             strings, issue_type Bug and enabled false, rather than a 404. Requires the \
             qa.jira_config/get grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_jira_settings)
        .json_response_with_schema::<dto::JiraSettingsDto>(
            openapi,
            StatusCode::OK,
            "The tenant's JIRA settings, or the unconfigured document",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/settings/jira
    router = OperationBuilder::put("/qa/v1/settings/jira")
        .operation_id("qa_insights.update_jira_settings")
        .summary("Replace the tenant's JIRA integration settings")
        .description(
            "A full replace of the tenant's JIRA settings. api_token_credstore_ref is a \
             credential-store reference, not a token, and it has two contracts. Its NAME \
             must be letters, digits, underscores and dashes only, at most 255 characters, \
             optionally prefixed with cred:// - slashes and colons are rejected by the \
             credential store, so a URL-shaped value such as credstore://qa/jira/token is \
             refused here with a 400 rather than failing later on the first JIRA call. Its \
             CONTENTS must be the base64 encoding of email:api_token, because JIRA's REST \
             API takes HTTP basic auth and the gateway's credential plugin prepends Basic to \
             whatever the reference resolves to. Sending an empty api_token_credstore_ref \
             keeps the reference already stored, so a form that does not resend it cannot \
             blank the credential; the reference cannot be cleared through this endpoint, and \
             turning enabled off is how a tenant stops using JIRA. A config with enabled true \
             must name a reference. The url may carry a context path (https://host/jira, the \
             usual shape for JIRA Data Center) and it is preserved. Every other field is \
             taken from the body as sent. Requires the qa.jira_config/update grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::JiraSettingsDto>(openapi, "The JIRA settings, in full")
        .handler(handlers::update_jira_settings)
        .json_response_with_schema::<dto::JiraSettingsDto>(
            openapi,
            StatusCode::OK,
            "The stored settings",
        )
        // 400 covers three things: a body that does not deserialize (axum's own
        // `Json` rejection, before this handler runs), a credential reference
        // whose syntax the credential store would refuse, and an enabled config
        // naming no reference at all. The **URL** is still not parsed here — the
        // egress adapter refuses an unusable one at call time, naming `url`, so
        // that the parse exists in exactly one place
        // (`infra::jira::oagw_client::egress_for`). That asymmetry is
        // deliberate: the reference's syntax is a fixed rule this gear can state,
        // while the URL's usability depends on what oagw will accept as an
        // endpoint.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // ==================== JIRA poller (Task 35) ====================

    // GET /qa/v1/settings/jira-poller
    router = OperationBuilder::get("/qa/v1/settings/jira-poller")
        .operation_id("qa_insights.get_jira_poller_settings")
        .summary("Get the tenant's JIRA poller cadence and auto-rerun switch")
        .description(
            "The interval between poller passes and whether a bug resolving in JIRA, plus a \
             new build, triggers an automatic re-run. A tenant that has never saved this \
             gets the defaults: 300 seconds and auto-rerun on. poll_interval_seconds is \
             clamped to at least one on this read - a value the tenant saved as zero, which \
             would otherwise be a hot loop against the poller's own cadence, is never returned \
             as zero. Requires the qa.jira_config/get grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_jira_poller_settings)
        .json_response_with_schema::<dto::JiraPollerConfigDto>(
            openapi,
            StatusCode::OK,
            "The tenant's poller settings, defaulted and clamped",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/settings/jira-poller
    router = OperationBuilder::put("/qa/v1/settings/jira-poller")
        .operation_id("qa_insights.update_jira_poller_settings")
        .summary("Replace the tenant's JIRA poller cadence and auto-rerun switch")
        .description(
            "A full replace of the tenant's poller settings. poll_interval_seconds is stored \
             as sent - the clamp to at least one applies only when this value is read back, \
             not here, so the settings screen never shows a value the tenant did not save. \
             auto_rerun_on_resolve gates only the automatic re-run: turning it off does not \
             stop a resolved bug from being marked resolved on the next poll. Requires the \
             qa.jira_config/update grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::JiraPollerConfigDto>(openapi, "The poller settings, in full")
        .handler(handlers::update_jira_poller_settings)
        .json_response_with_schema::<dto::JiraPollerConfigDto>(
            openapi,
            StatusCode::OK,
            "The stored settings",
        )
        // A body that does not deserialize (axum's own `Json` rejection) and a
        // stored interval that does not fit the column both surface as 400 -
        // see `JiraService::save_poller_config`'s own `# Errors` section.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    register_notification_routes(router, openapi)
}

/// The `// ==== Notifications ==== ` band, split out from
/// [`register_settings_routes`] rather than left inline: `clippy::too_many_lines`
/// (200) is `-D` in this workspace and the combined function crossed it. Four
/// paths' worth of `OperationBuilder` prose does not shrink, so the fix is the
/// same shape [`super::analytics::overview_query_params`] already uses for the
/// same reason — one function per surface's registrations, called from the
/// module's single entry point, not a second module.
fn register_notification_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/settings/notifications
    router = OperationBuilder::get("/qa/v1/settings/notifications")
        .operation_id("qa_insights.get_notification_settings")
        .summary("Get the tenant's notification settings")
        .description(
            "Slack and email egress configuration, including the six per-status scheduled-run \
             Slack templates. A tenant that has never saved this gets every field at its \
             default (every gate off except notify_on_failure, SMTP port 587) rather than a \
             404. slack_webhook_credstore_ref is a credential-store reference, never a URL - \
             possession of a Slack incoming-webhook URL is itself the authorization to post, \
             so it is treated the same as a JIRA API token, and the PUT enforces the same \
             syntax: letters, digits, underscores and dashes only, optionally prefixed with \
             cred://. Requires the qa.notification_config/get grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_notification_settings)
        .json_response_with_schema::<dto::NotificationConfigDto>(
            openapi,
            StatusCode::OK,
            "The tenant's notification settings, defaulted",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/settings/notifications
    router = OperationBuilder::put("/qa/v1/settings/notifications")
        .operation_id("qa_insights.update_notification_settings")
        .summary("Replace the tenant's notification settings")
        .description(
            "A full replace of the tenant's notification settings. \
             slack_webhook_credstore_ref must be a credential-store reference and never a \
             webhook URL - letters, digits, underscores and dashes only, at most 255 \
             characters, optionally prefixed with cred://, exactly as the JIRA endpoint's \
             api_token_credstore_ref. Slashes and colons are rejected by the credential \
             store, so a URL-shaped value such as https://hooks.slack.com/services/T00/B00/XXX \
             is refused here with a 400 naming that field rather than a row stored for the GET \
             to hand back. Unlike the JIRA endpoint there is no \
             keep-stored-value convention, so an empty slack_webhook_credstore_ref clears the \
             reference rather than preserving it; that is a difference in how absence is \
             treated, not a claim that the field is less sensitive. Every other field is \
             stored exactly as sent. Requires the qa.notification_config/update grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::NotificationConfigDto>(openapi, "The notification settings, in full")
        .handler(handlers::update_notification_settings)
        .json_response_with_schema::<dto::NotificationConfigDto>(
            openapi,
            StatusCode::OK,
            "The stored settings",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/settings/notifications/test
    router = OperationBuilder::post("/qa/v1/settings/notifications/test")
        .operation_id("qa_insights.test_notification")
        .summary("Send a test notification")
        .description(
            "Sends a real notification right now, over whichever channel(s) are enabled. With \
             no request body, sends the settings page's generic test message using the \
             tenant's stored settings. With a body, tests one scheduled-run Slack template \
             against the given config override and event token (one of pending, in_progress, \
             succeeded, failed, error, skipped) rather than the stored settings - the config \
             override must have Slack enabled with a non-empty webhook reference, or this is \
             refused with a 400 before anything is sent. Neither shape claims a dedupe slot or \
             writes the audit log; both are pinned by qa_insights_sdk::SLACK_NOTIFICATION_EVENTS. \
             Unlike the automatic completion path, a send failure here is returned rather than \
             swallowed - an operator testing a channel deserves to know it does not work, \
             including a 501 when the channel this deployment ships has no adapter at all (D10). \
             This endpoint's OpenAPI schema shows the body as required; posting no body at all \
             is also accepted, matching legacy. Requires the qa.notification_config/test grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::NotificationTestReq>(
            openapi,
            "Optional: a config override and event token to test a scheduled-run template \
             instead of the stored generic test",
        )
        .handler(handlers::test_notification)
        .json_response_with_schema::<dto::NotificationTestOutcomeDto>(
            openapi,
            StatusCode::OK,
            "{\"status\": \"sent\"}",
        )
        // 400 covers a body that does not deserialize, an unrecognized event
        // token, and the scheduled-run override's own two preconditions
        // (Slack enabled, webhook reference present) - see
        // `NotifyService::send_test`'s `# Errors`. There is no declared 501
        // here: `OperationBuilder` has no `error_501` (only the seven fixed
        // codes above and `error_500`), so `DomainError::UnsupportedEgress`'s
        // mapping is documented in prose above rather than in the schema.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/settings/notifications/preview
    router = OperationBuilder::post("/qa/v1/settings/notifications/preview")
        .operation_id("qa_insights.preview_notification")
        .summary("Render a scheduled-run Slack preview")
        .description(
            "Renders one scheduled-run Slack template against the given config override and a \
             fixed sample run, without sending anything or touching the stored settings. event \
             must be one of pending, in_progress, succeeded, failed, error, skipped. Requires \
             the qa.notification_config/get grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::NotificationPreviewReq>(
            openapi,
            "The config override and event to preview",
        )
        .handler(handlers::preview_notification)
        .json_response_with_schema::<dto::NotificationPreviewDto>(
            openapi,
            StatusCode::OK,
            "The rendered preview",
        )
        // 400 covers a body that does not deserialize and an unrecognized
        // event token.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/settings/notifications/log
    OperationBuilder::get("/qa/v1/settings/notifications/log")
        .operation_id("qa_insights.get_notification_log")
        .summary("Get the tenant's notification audit log")
        .description(
            "The most recent notification delivery attempts, newest first, across every \
             channel and every kind: automatic completion alerts, tests, and every skip and \
             failure. limit defaults to 100 and is clamped to at most 500. run_id is null for \
             an entry that belongs to no run, such as a settings-page test send. Requires the \
             qa.notification_config/get grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "limit",
            false,
            "Maximum entries to return. Defaults to 100, clamped to at most 500.",
            "integer",
        )
        .handler(handlers::get_notification_log)
        .json_array_response_with_schema::<dto::NotificationLogEntryDto>(
            openapi,
            StatusCode::OK,
            "The tenant's most recent audit entries, newest first",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
