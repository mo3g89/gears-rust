//! Tests for the `CredStorePluginClientV1` trait impl — the surface the
//! credstore gear actually calls.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use credstore_sdk::{CredStorePluginClientV1, OwnerId, SecretRef, SecretValue, TenantId};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::test_support::memory_service;

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(Uuid::new_v4())
        .subject_id(Uuid::new_v4())
        .build()
        .expect("test security context")
}

fn sref(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid secret ref")
}

#[tokio::test]
async fn spi_put_get_delete_roundtrip() {
    let svc = memory_service().await;
    let tenant = TenantId(Uuid::new_v4());
    let key = sref("written");

    svc.put(&ctx(), &tenant, &key, SecretValue::from("v1"), None)
        .await
        .expect("put");

    let got = svc.get(&ctx(), &tenant, &key, None).await.expect("get");
    assert_eq!(got.expect("present").as_bytes(), b"v1");

    svc.delete(&ctx(), &tenant, &key, None)
        .await
        .expect("delete");
    assert!(
        svc.get(&ctx(), &tenant, &key, None)
            .await
            .expect("get")
            .is_none()
    );
}

/// The SPI ignores the security context: the gear has already authorized the
/// request and resolved tenant/owner, so a *different* caller context must
/// resolve the same secret.
#[tokio::test]
async fn the_security_context_does_not_affect_resolution() {
    let svc = memory_service().await;
    let tenant = TenantId(Uuid::new_v4());
    let owner = OwnerId(Uuid::new_v4());
    let key = sref("ctx-independent");

    svc.put(&ctx(), &tenant, &key, SecretValue::from("v"), Some(&owner))
        .await
        .expect("put");

    let got = svc
        .get(&ctx(), &tenant, &key, Some(&owner))
        .await
        .expect("get");
    assert_eq!(got.expect("present").as_bytes(), b"v");
}

#[tokio::test]
async fn spi_get_of_a_missing_key_is_none_not_an_error() {
    let svc = memory_service().await;
    let got = svc
        .get(&ctx(), &TenantId(Uuid::new_v4()), &sref("absent"), None)
        .await
        .expect("get must not error");
    assert!(got.is_none());
}
