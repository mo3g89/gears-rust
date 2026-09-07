//! The notification-config handlers driven over real services, for the one
//! property nothing else checks: **a refusal names `qa.notification_config`**.
//!
//! `domain::error`'s match is exhaustive over `DomainError` *variants*, so
//! the compiler guarantees every variant has a status and says nothing about
//! which resource type an endpoint attributes a refusal to. A 403 is the most
//! likely error on a fresh deployment — a policy engine not yet taught
//! `qa.notification_config` refuses everything — and it carries no field, so
//! the resource type is the only actionable thing in the body.
//!
//! # The rendered id is `notification`, not `notification_config`
//!
//! `NotificationResourceError`'s own declaration
//! (`domain/error.rs:536`) is `cf.qa.insights.notification.v1~` — the PEP
//! resource type this gear's PDP calls are made against is
//! `qa.notification_config` (`domain::service::resources::NOTIFICATION_CONFIG`),
//! but the REST error id drops the `_config`. Every other resource in this
//! gear keeps the two aligned (`qa.test_result`/`test_result`,
//! `qa.saved_view`/`saved_view`, `qa.jira_bug`/`jira_bug`); this one does not.
//! That asymmetry is a known, deliberate deferral — the `gts_id` is the
//! RFC-9457 `type` a client matches on, so renaming it is a wire change out of
//! scope here — and this module asserts the id [`NotificationResourceError`]
//! actually emits rather than the one a symmetric naming scheme would predict.
//!
//! Same shape as the saved-view and JIRA wrappers' existing tests.
//! Review finding #24.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::Extension;
use toolkit::api::canonical_prelude::{IntoResponse, Json};
use uuid::Uuid;

use super::{get_notification_settings, update_notification_settings};
use crate::api::rest::dto::NotificationConfigDto;
use crate::domain::service::test_support::{Fleet, ctx};
use qa_insights_sdk::NotificationConfig;

const TENANT: Uuid = Uuid::from_u128(0x0E01_0000_0000_0001);

/// The resource type the notification endpoints must attribute a refusal to.
///
/// Confirmed against `NotificationResourceError`'s own declaration at
/// `domain/error.rs:536` — **not** `cf.qa.insights.notification_config.v1~`,
/// which this module's own header explains.
const NOTIFICATION_GTS: &str = "cf.qa.insights.notification.v1~";

async fn rendered(response: axum::response::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn a_denied_notification_read_names_the_notification_config() {
    let fleet = Fleet::denying().await;

    let response = get_notification_settings(
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&fleet.services)),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(status, 403, "body was {body}");
    assert!(
        body.contains(NOTIFICATION_GTS),
        "a denial must name qa.notification_config, not another resource type; \
         body was {body}"
    );
}

#[tokio::test]
async fn a_denied_notification_write_names_the_notification_config() {
    let fleet = Fleet::denying().await;
    let body = NotificationConfigDto::from(NotificationConfig::default());

    let response = update_notification_settings(
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&fleet.services)),
        Json(body),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(status, 403, "body was {body}");
    assert!(body.contains(NOTIFICATION_GTS), "body was {body}");
}
