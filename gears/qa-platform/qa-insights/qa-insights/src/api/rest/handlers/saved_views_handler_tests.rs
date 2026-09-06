//! The saved-view handlers driven over real services. Same property, same
//! reason as `settings_handler_tests`: `as_saved_view_error` re-attributes a
//! refusal to `qa.saved_view` and nothing drove it through a handler.
//! Review finding #40.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::Extension;
use axum::extract::Query;
use axum::http::Uri;
use toolkit::api::canonical_prelude::{IntoResponse, Json};
use uuid::Uuid;

use super::{create_saved_view, list_saved_views};
use crate::api::rest::dto::{NewSavedViewReq, SavedViewsListQuery};
use crate::domain::service::test_support::{Fleet, ctx};

const TENANT: Uuid = Uuid::from_u128(0x0E02_0000_0000_0001);

/// The resource type the saved-view endpoints must attribute a refusal to —
/// confirmed against `SavedViewResourceError`'s own declaration
/// (`api/rest/error.rs:108`).
const SAVED_VIEW_GTS: &str = "cf.qa.insights.saved_view.v1~";

async fn rendered(response: axum::response::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn a_denied_saved_view_list_names_the_saved_view() {
    let fleet = Fleet::denying().await;

    let response = list_saved_views(
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&fleet.services)),
        Query(SavedViewsListQuery {
            scope: "all".to_owned(),
            repo_id: None,
            plan_path: None,
        }),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(status, 403, "body was {body}");
    assert!(
        body.contains(SAVED_VIEW_GTS),
        "a denial must name qa.saved_view, not another resource type; body was {body}"
    );
}

#[tokio::test]
async fn a_denied_saved_view_create_names_the_saved_view() {
    let fleet = Fleet::denying().await;

    let response = create_saved_view(
        Uri::from_static("/qa/v1/analytics/views"),
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&fleet.services)),
        Json(NewSavedViewReq {
            scope: "all".to_owned(),
            repo_id: None,
            plan_path: None,
            name: "denied-view-check".to_owned(),
            query_json: serde_json::json!({}),
        }),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(status, 403, "body was {body}");
    assert!(body.contains(SAVED_VIEW_GTS), "body was {body}");
}
