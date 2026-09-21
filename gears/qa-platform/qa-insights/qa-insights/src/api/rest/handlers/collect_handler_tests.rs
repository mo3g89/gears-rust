//! `POST /qa/v1/collect/{repo_id}` driven over real services.
//!
//! This route is `.public()` and authenticates with an HMAC over
//! `(repo_id, branch, tenant_id)`. The verification is sound —
//! `domain::service::collect`'s `verify_signature` is constant-time, fails
//! closed on an empty signing secret before touching `aws_lc_rs`, and mints
//! the system actor only after the check passes. Nothing drove it.
//!
//! Three paths, one test each: a nil tenant is refused before any signature
//! work, a bad signature is refused **and writes nothing**, and a correctly
//! signed report reaches the repository. Review finding #23.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use toolkit::api::canonical_prelude::{IntoResponse, Json};
use uuid::Uuid;

use super::report_collect_count;
use crate::api::rest::dto::{CollectCountReq, CollectReportQuery};
use crate::domain::service::test_support::Fleet;

const REPO: Uuid = Uuid::from_u128(0x0C01_0000_0000_0001);
const TENANT: Uuid = Uuid::from_u128(0x0C01_0000_0000_0002);
const BRANCH: &str = "main";
const SIGNING_SECRET: &str = "test-collect-secret";

async fn rendered(response: axum::response::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Build a [`Fleet`] with `secret` as `collect_report_signing_secret` — the
/// one dependency these three tests vary. A thin, collect-specific wrapper
/// over the general fixture: [`Fleet`] itself is not named or shaped around
/// collect (see its own doc), because Task 8 needs the identical struct
/// behind the notification and saved-view handlers.
async fn collect_fleet(secret: &str) -> Fleet {
    Fleet::new(secret).await
}

/// `hex(HMAC_SHA256(derive(secret, tenant_id), "{repo_id}|{branch}|{tenant_id}"))`
/// — independently reproducing `domain::service::collect::signing_payload`,
/// `derive_signing_key` (Task 7: the tenant-specific key HKDF-derives from
/// the root `collect_report_signing_secret`), and `CollectService::sign`'s
/// hex encoding, **not calling any of them**. A test that signed with the
/// code it verifies would prove only that the two agree with each other, and
/// would pass exactly as happily if both computed the HMAC over an empty
/// string, or under a key that ignored `secret`/`tenant_id` entirely.
///
/// The salt matches `collect.rs`'s `COLLECT_SIGNING_HKDF_SALT` literally
/// rather than importing it, for the same reason the HMAC construction below
/// is spelled out rather than calling `derive_signing_key`: importing the
/// constant would make a change to the real salt invisible to this test.
fn sign(repo_id: Uuid, branch: &str, tenant_id: Uuid, secret: &str) -> String {
    let salt = aws_lc_rs::hkdf::Salt::new(
        aws_lc_rs::hkdf::HKDF_SHA256,
        b"qa-insights/collect-report-signing/v1",
    );
    let prk = salt.extract(secret.as_bytes());
    let info: [&[u8]; 1] = [tenant_id.as_bytes().as_slice()];
    let okm = prk
        .expand(&info, aws_lc_rs::hkdf::HKDF_SHA256.hmac_algorithm())
        .unwrap();
    let key = aws_lc_rs::hmac::Key::from(okm);
    let tag = aws_lc_rs::hmac::sign(&key, format!("{repo_id}|{branch}|{tenant_id}").as_bytes());
    hex::encode(tag.as_ref())
}

/// **A nil tenant is refused before any signature work.**
///
/// `TenantBound::new` is what rejects it, and the refusal is a 400 naming the
/// field — not a 403, because the caller supplied a malformed request rather
/// than a wrong credential.
#[tokio::test]
async fn a_nil_tenant_id_is_a_400_naming_the_field() {
    let fleet = collect_fleet(SIGNING_SECRET).await;
    let query = CollectReportQuery {
        branch: BRANCH.to_owned(),
        tenant_id: Uuid::nil(),
        sig: sign(REPO, BRANCH, Uuid::nil(), SIGNING_SECRET),
    };
    let response = report_collect_count(
        Extension(Arc::clone(&fleet.services)),
        Path(REPO),
        Query(query),
        Json(CollectCountReq {
            test_file: "a.py".to_owned(),
            case_count: 3,
        }),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(status, 400, "a nil tenant must be a 400; body was {body}");
    assert!(
        body.contains("tenant_id"),
        "the refusal must name the field; body was {body}"
    );
}

/// **A bad signature is refused AND writes nothing.**
///
/// The second half is the one that matters and the one a status-only
/// assertion would miss: a handler that wrote first and checked afterwards
/// would pass a 403 assertion and still have persisted the row.
#[tokio::test]
async fn a_bad_signature_is_refused_and_writes_nothing() {
    let fleet = collect_fleet(SIGNING_SECRET).await;
    let query = CollectReportQuery {
        branch: BRANCH.to_owned(),
        tenant_id: TENANT,
        sig: "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
    };
    let response = report_collect_count(
        Extension(Arc::clone(&fleet.services)),
        Path(REPO),
        Query(query),
        Json(CollectCountReq {
            test_file: "a.py".to_owned(),
            case_count: 3,
        }),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(
        status, 403,
        "a bad signature must be a 403; body was {body}"
    );
    assert_eq!(
        fleet.collect_counts(TENANT, REPO, BRANCH).await.len(),
        0,
        "a refused report must not have written a count row"
    );
}

/// **A correctly signed report reaches the repository.**
///
/// The happy path, so the two refusals above are shown to be refusals of
/// something that otherwise works — without this, both could pass against a
/// handler that refuses everything.
#[tokio::test]
async fn a_correctly_signed_report_is_recorded() {
    let fleet = collect_fleet(SIGNING_SECRET).await;
    let query = CollectReportQuery {
        branch: BRANCH.to_owned(),
        tenant_id: TENANT,
        sig: sign(REPO, BRANCH, TENANT, SIGNING_SECRET),
    };
    let response = report_collect_count(
        Extension(Arc::clone(&fleet.services)),
        Path(REPO),
        Query(query),
        Json(CollectCountReq {
            test_file: "a.py".to_owned(),
            case_count: 3,
        }),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(
        status, 200,
        "a signed report must be accepted; body was {body}"
    );
    let recorded = fleet.collect_counts(TENANT, REPO, BRANCH).await;
    assert_eq!(recorded.len(), 1, "the count must have been written");
    assert_eq!(recorded[0].case_count, 3);
}

/// **An empty signing secret refuses even a correctly signed request.**
///
/// Pins the fail-closed ordering `domain::service::collect`'s header and
/// `verify_signature`'s own doc both call out: an unconfigured secret is
/// [`DomainError::Forbidden`](crate::domain::error::DomainError::Forbidden)
/// *before* `aws_lc_rs` ever runs, so there is no "empty secret, matching
/// signature" case that could slip through — the signature above is computed
/// with the identical `sign` helper the happy path uses, and is refused
/// anyway.
#[tokio::test]
async fn an_empty_signing_secret_refuses_even_a_correctly_signed_request() {
    let fleet = collect_fleet("").await;
    let query = CollectReportQuery {
        branch: BRANCH.to_owned(),
        tenant_id: TENANT,
        sig: sign(REPO, BRANCH, TENANT, ""),
    };
    let response = report_collect_count(
        Extension(Arc::clone(&fleet.services)),
        Path(REPO),
        Query(query),
        Json(CollectCountReq {
            test_file: "a.py".to_owned(),
            case_count: 3,
        }),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(
        status, 403,
        "an unconfigured signing secret must refuse; body was {body}"
    );
    assert_eq!(
        fleet.collect_counts(TENANT, REPO, BRANCH).await.len(),
        0,
        "a refused report must not have written a count row"
    );
}
