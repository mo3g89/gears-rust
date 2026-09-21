//! Behaviour tests for the database-backed value store.
//!
//! Every test runs against a real `SeaORM`/`SecureORM` path over `SQLite`,
//! with the schema built from this plugin's own migration definitions — no raw
//! SQL, no mock repository. The one exception is
//! [`duplicate_tenant_class_rows_are_rejected`], which inserts through the
//! entity twice on purpose to prove the partial unique index is really there.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::doc_markdown
)]

use credstore_sdk::{OwnerId, SecretRef, SecretValue, SharingMode, TenantId};
use uuid::Uuid;

use crate::config::{PostgresCredStorePluginConfig, SecretConfig};
use crate::domain::Service;
use crate::test_support::{
    connect_migrated, file_dsn, memory_dsn, memory_service, provider_and_repo, service_over,
    store_over,
};

fn sref(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid secret ref")
}

fn tid() -> TenantId {
    TenantId(Uuid::new_v4())
}

// ── the two persisted key classes ───────────────────────────────────────────

#[tokio::test]
async fn put_then_get_roundtrips_the_tenant_class() {
    let svc = memory_service().await;
    let tenant = tid();
    let key = sref("ssh-key");

    svc.put_value(&tenant, &key, SecretValue::from("PRIVATE-KEY-BODY"), None)
        .await
        .expect("put");

    let got = svc.get_value(&tenant, &key, None).await.expect("get");
    assert_eq!(got.expect("present").as_bytes(), b"PRIVATE-KEY-BODY");
}

#[tokio::test]
async fn put_then_get_roundtrips_the_private_class() {
    let svc = memory_service().await;
    let tenant = tid();
    let owner = OwnerId(Uuid::new_v4());
    let key = sref("ssh-key");

    svc.put_value(&tenant, &key, SecretValue::from("owned"), Some(&owner))
        .await
        .expect("put");

    let got = svc
        .get_value(&tenant, &key, Some(&owner))
        .await
        .expect("get");
    assert_eq!(got.expect("present").as_bytes(), b"owned");
}

#[tokio::test]
async fn the_two_key_classes_coexist_under_one_reference() {
    let svc = memory_service().await;
    let tenant = tid();
    let owner = OwnerId(Uuid::new_v4());
    let key = sref("shared-name");

    svc.put_value(&tenant, &key, SecretValue::from("tenant-value"), None)
        .await
        .expect("put tenant");
    svc.put_value(
        &tenant,
        &key,
        SecretValue::from("private-value"),
        Some(&owner),
    )
    .await
    .expect("put private");

    assert_eq!(
        svc.get_value(&tenant, &key, None)
            .await
            .expect("get")
            .expect("present")
            .as_bytes(),
        b"tenant-value"
    );
    assert_eq!(
        svc.get_value(&tenant, &key, Some(&owner))
            .await
            .expect("get")
            .expect("present")
            .as_bytes(),
        b"private-value"
    );
}

#[tokio::test]
async fn a_private_lookup_never_falls_back_to_the_tenant_row() {
    let svc = memory_service().await;
    let tenant = tid();
    let owner = OwnerId(Uuid::new_v4());
    let key = sref("tenant-only");

    svc.put_value(&tenant, &key, SecretValue::from("v"), None)
        .await
        .expect("put");

    assert!(
        svc.get_value(&tenant, &key, Some(&owner))
            .await
            .expect("get")
            .is_none(),
        "owner_id = Some must read the private class alone"
    );
}

#[tokio::test]
async fn overwrite_replaces_the_value_in_place() {
    let dsn = memory_dsn();
    let svc = service_over(&dsn).await;
    let tenant = tid();
    let key = sref("rotated");

    svc.put_value(&tenant, &key, SecretValue::from("v1"), None)
        .await
        .expect("put v1");
    svc.put_value(&tenant, &key, SecretValue::from("v2"), None)
        .await
        .expect("put v2");

    assert_eq!(
        svc.get_value(&tenant, &key, None)
            .await
            .expect("get")
            .expect("present")
            .as_bytes(),
        b"v2"
    );

    // One row, not two: the overwrite must not have inserted a second row.
    let db = connect_migrated(&dsn).await;
    let count = row_count(&db).await;
    assert_eq!(count, 1, "overwrite must update in place, not append");
}

#[tokio::test]
async fn missing_key_reads_as_none() {
    let svc = memory_service().await;
    assert!(
        svc.get_value(&tid(), &sref("absent"), None)
            .await
            .expect("get")
            .is_none()
    );
}

#[tokio::test]
async fn delete_removes_the_value_and_a_miss_is_a_no_op() {
    let svc = memory_service().await;
    let tenant = tid();
    let key = sref("doomed");

    svc.put_value(&tenant, &key, SecretValue::from("v"), None)
        .await
        .expect("put");
    svc.delete_value(&tenant, &key, None).await.expect("delete");

    assert!(
        svc.get_value(&tenant, &key, None)
            .await
            .expect("get")
            .is_none()
    );

    // A second delete of the same, now-absent key must succeed: the gear
    // treats a missing backend value as success.
    svc.delete_value(&tenant, &key, None)
        .await
        .expect("delete miss is a no-op");
}

#[tokio::test]
async fn delete_of_one_key_class_leaves_the_other_alone() {
    let svc = memory_service().await;
    let tenant = tid();
    let owner = OwnerId(Uuid::new_v4());
    let key = sref("both");

    svc.put_value(&tenant, &key, SecretValue::from("t"), None)
        .await
        .expect("put tenant");
    svc.put_value(&tenant, &key, SecretValue::from("p"), Some(&owner))
        .await
        .expect("put private");

    svc.delete_value(&tenant, &key, None).await.expect("delete");

    assert!(
        svc.get_value(&tenant, &key, None)
            .await
            .expect("get")
            .is_none()
    );
    assert_eq!(
        svc.get_value(&tenant, &key, Some(&owner))
            .await
            .expect("get")
            .expect("private survives")
            .as_bytes(),
        b"p"
    );
}

// ── tenant isolation ────────────────────────────────────────────────────────

/// Fails if tenant scoping is ever dropped from the read, write or delete path.
///
/// Two tenants store *different* values under the *same* reference. If any
/// query lost its tenant predicate — the `SecureORM` clamp or the row's
/// `tenant_id` on insert — one of these assertions reports the other tenant's
/// bytes, or `delete` for tenant A destroys tenant B's row.
#[tokio::test]
async fn tenant_isolation_holds_across_read_write_and_delete() {
    let svc = memory_service().await;
    let tenant_a = tid();
    let tenant_b = tid();
    let key = sref("ssh-key");

    svc.put_value(&tenant_a, &key, SecretValue::from("A-SECRET"), None)
        .await
        .expect("put a");
    svc.put_value(&tenant_b, &key, SecretValue::from("B-SECRET"), None)
        .await
        .expect("put b");

    assert_eq!(
        svc.get_value(&tenant_a, &key, None)
            .await
            .expect("get a")
            .expect("a present")
            .as_bytes(),
        b"A-SECRET"
    );
    assert_eq!(
        svc.get_value(&tenant_b, &key, None)
            .await
            .expect("get b")
            .expect("b present")
            .as_bytes(),
        b"B-SECRET"
    );

    // A tenant with nothing stored must not see either.
    assert!(
        svc.get_value(&tid(), &key, None)
            .await
            .expect("get c")
            .is_none(),
        "a third tenant must not resolve another tenant's reference"
    );

    // A delete issued for tenant A must not touch tenant B's row.
    svc.delete_value(&tenant_a, &key, None)
        .await
        .expect("delete a");
    assert!(
        svc.get_value(&tenant_a, &key, None)
            .await
            .expect("get a")
            .is_none()
    );
    assert_eq!(
        svc.get_value(&tenant_b, &key, None)
            .await
            .expect("get b")
            .expect("b survives")
            .as_bytes(),
        b"B-SECRET"
    );
}

/// The private class is keyed by owner as well as tenant.
#[tokio::test]
async fn owner_isolation_holds_within_one_tenant() {
    let svc = memory_service().await;
    let tenant = tid();
    let alice = OwnerId(Uuid::new_v4());
    let bob = OwnerId(Uuid::new_v4());
    let key = sref("personal-token");

    svc.put_value(&tenant, &key, SecretValue::from("alice"), Some(&alice))
        .await
        .expect("put alice");

    assert!(
        svc.get_value(&tenant, &key, Some(&bob))
            .await
            .expect("get bob")
            .is_none(),
        "bob must not resolve alice's private secret"
    );
}

// ── restart survival — the claim this plugin exists for ─────────────────────

/// Writes a secret, **drops the whole service and its database handle**, then
/// rebuilds both against the same database file and reads the secret back.
///
/// A file-backed database is essential: a `mode=memory` SQLite database is
/// destroyed when its last connection closes, so the drop under test would
/// destroy the evidence. This is the closest in-process analogue of the
/// container being recreated, and the same sequence is exercised against a real
/// PostgreSQL server by `tests/restart_survival_pg.rs`.
#[tokio::test]
async fn a_secret_survives_dropping_and_rebuilding_the_service() {
    let dir = std::env::temp_dir().join(format!("pg_credstore_plugin_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("values.db");
    let dsn = file_dsn(&path);

    let tenant = tid();
    let owner = OwnerId(Uuid::new_v4());
    let tenant_key = sref("ssh-key");
    let private_key = sref("personal-token");

    // ── boot 1 ──
    {
        let svc = service_over(&dsn).await;
        svc.put_value(
            &tenant,
            &tenant_key,
            SecretValue::from("SSH-PRIVATE-KEY-BODY"),
            None,
        )
        .await
        .expect("put tenant");
        svc.put_value(
            &tenant,
            &private_key,
            SecretValue::from("PERSONAL-TOKEN"),
            Some(&owner),
        )
        .await
        .expect("put private");
        // `svc` — and with it the repository, the `DBProvider`, the `Db` handle
        // and the connection pool — is dropped at the end of this scope.
    }

    // ── boot 2: a brand-new service over the same database ──
    {
        let svc = service_over(&dsn).await;
        assert_eq!(
            svc.get_value(&tenant, &tenant_key, None)
                .await
                .expect("get tenant")
                .expect("tenant secret survived the restart")
                .as_bytes(),
            b"SSH-PRIVATE-KEY-BODY"
        );
        assert_eq!(
            svc.get_value(&tenant, &private_key, Some(&owner))
                .await
                .expect("get private")
                .expect("private secret survived the restart")
                .as_bytes(),
            b"PERSONAL-TOKEN"
        );

        // A delete must survive a restart too, or the gear's view and the
        // backend's would diverge after every boot.
        svc.delete_value(&tenant, &tenant_key, None)
            .await
            .expect("delete");
    }

    // ── boot 3: the delete stuck ──
    {
        let svc = service_over(&dsn).await;
        assert!(
            svc.get_value(&tenant, &tenant_key, None)
                .await
                .expect("get")
                .is_none(),
            "a delete must survive a restart as well as a write"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

// ── config-seeded classes ───────────────────────────────────────────────────

fn seeded_config() -> PostgresCredStorePluginConfig {
    PostgresCredStorePluginConfig {
        secrets: vec![
            // global (no tenant)
            SecretConfig {
                tenant_id: None,
                owner_id: None,
                key: "global-key".to_owned(),
                value: "global-value".to_owned(),
                sharing: None,
            },
            // shared, scoped to a tenant
            SecretConfig {
                tenant_id: Some(SHARED_TENANT),
                owner_id: None,
                key: "shared-key".to_owned(),
                value: "shared-value".to_owned(),
                sharing: Some(SharingMode::Shared),
            },
            // tenant class — this one is persisted
            SecretConfig {
                tenant_id: Some(SHARED_TENANT),
                owner_id: None,
                key: "seeded-tenant-key".to_owned(),
                value: "seeded-tenant-value".to_owned(),
                sharing: Some(SharingMode::Tenant),
            },
        ],
        ..Default::default()
    }
}

const SHARED_TENANT: Uuid = Uuid::from_u128(0x1);

async fn seeded_service(dsn: &str) -> Service {
    let db = connect_migrated(dsn).await;
    Service::from_config(store_over(db), &seeded_config()).expect("config builds")
}

#[tokio::test]
async fn shared_and_global_seeds_are_read_fallbacks_and_are_not_persisted() {
    let dsn = memory_dsn();
    let svc = seeded_service(&dsn).await;
    svc.seed().await.expect("seed");

    let tenant = TenantId(SHARED_TENANT);

    // shared and global resolve for `owner_id = None`.
    assert_eq!(
        svc.get_value(&tenant, &sref("shared-key"), None)
            .await
            .expect("get")
            .expect("shared present")
            .as_bytes(),
        b"shared-value"
    );
    assert_eq!(
        svc.get_value(&tenant, &sref("global-key"), None)
            .await
            .expect("get")
            .expect("global present")
            .as_bytes(),
        b"global-value"
    );
    // global is cross-tenant, shared is not.
    let other = tid();
    assert_eq!(
        svc.get_value(&other, &sref("global-key"), None)
            .await
            .expect("get")
            .expect("global is cross-tenant")
            .as_bytes(),
        b"global-value"
    );
    assert!(
        svc.get_value(&other, &sref("shared-key"), None)
            .await
            .expect("get")
            .is_none(),
        "a shared seed is scoped to its configured tenant"
    );

    // Exactly one row exists: the tenant-class seed. Neither the shared nor
    // the global seed was written to the table.
    let db = connect_migrated(&dsn).await;
    assert_eq!(
        row_count(&db).await,
        1,
        "only the tenant-class seed may be persisted"
    );

    // A second service with *no* config, over the same database, sees the
    // persisted seed and neither in-memory fallback.
    let plain = service_over(&dsn).await;
    assert_eq!(
        plain
            .get_value(&tenant, &sref("seeded-tenant-key"), None)
            .await
            .expect("get")
            .expect("persisted")
            .as_bytes(),
        b"seeded-tenant-value"
    );
    assert!(
        plain
            .get_value(&tenant, &sref("shared-key"), None)
            .await
            .expect("get")
            .is_none()
    );
    assert!(
        plain
            .get_value(&tenant, &sref("global-key"), None)
            .await
            .expect("get")
            .is_none()
    );
}

#[tokio::test]
async fn a_tenant_delete_cannot_destroy_a_shared_or_global_seed() {
    let svc = seeded_service(&memory_dsn()).await;
    let tenant = TenantId(SHARED_TENANT);

    svc.delete_value(&tenant, &sref("shared-key"), None)
        .await
        .expect("delete shared");
    svc.delete_value(&tenant, &sref("global-key"), None)
        .await
        .expect("delete global");

    assert!(
        svc.get_value(&tenant, &sref("shared-key"), None)
            .await
            .expect("get")
            .is_some(),
        "config-seeded shared entries are cross-tenant reference data"
    );
    assert!(
        svc.get_value(&tenant, &sref("global-key"), None)
            .await
            .expect("get")
            .is_some()
    );
}

#[tokio::test]
async fn seed_inserts_absent_rows_but_never_clobbers_a_runtime_rotation() {
    let dsn = memory_dsn();
    let tenant = TenantId(SHARED_TENANT);
    let key = sref("seeded-tenant-key");

    // Boot 1: the seed is written because nothing is stored yet.
    let svc = seeded_service(&dsn).await;
    assert_eq!(svc.seed().await.expect("seed"), 1);

    // A client rotates the value through the API.
    svc.put_value(&tenant, &key, SecretValue::from("ROTATED"), None)
        .await
        .expect("put");

    // Boot 2: the same config runs `seed` again. It must leave the rotation
    // alone — reverting to the YAML value here would destroy secret material
    // a client wrote, which is the failure this plugin exists to end.
    let svc2 = seeded_service(&dsn).await;
    assert_eq!(
        svc2.seed().await.expect("seed"),
        0,
        "seeding must be insert-if-absent, not upsert"
    );
    assert_eq!(
        svc2.get_value(&tenant, &key, None)
            .await
            .expect("get")
            .expect("present")
            .as_bytes(),
        b"ROTATED"
    );
}

// ── config validation parity with the in-memory plugin ──────────────────────

fn cfg_with(secret: SecretConfig) -> PostgresCredStorePluginConfig {
    PostgresCredStorePluginConfig {
        secrets: vec![secret],
        ..Default::default()
    }
}

async fn expect_config_error(secret: SecretConfig, needle: &str) {
    let db = connect_migrated(&memory_dsn()).await;
    let err = match Service::from_config(store_over(db), &cfg_with(secret)) {
        Ok(_) => panic!("config must be rejected"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains(needle),
        "error {err:?} should mention {needle:?}"
    );
}

#[tokio::test]
async fn nil_tenant_id_is_rejected() {
    expect_config_error(
        SecretConfig {
            tenant_id: Some(Uuid::nil()),
            owner_id: None,
            key: "k".to_owned(),
            value: "v".to_owned(),
            sharing: None,
        },
        "tenant_id must not be nil",
    )
    .await;
}

#[tokio::test]
async fn owner_without_tenant_is_rejected() {
    expect_config_error(
        SecretConfig {
            tenant_id: None,
            owner_id: Some(Uuid::new_v4()),
            key: "k".to_owned(),
            value: "v".to_owned(),
            sharing: None,
        },
        "owner_id cannot be set without tenant_id",
    )
    .await;
}

#[tokio::test]
async fn owner_with_non_private_sharing_is_rejected() {
    expect_config_error(
        SecretConfig {
            tenant_id: Some(Uuid::new_v4()),
            owner_id: Some(Uuid::new_v4()),
            key: "k".to_owned(),
            value: "v".to_owned(),
            sharing: Some(SharingMode::Tenant),
        },
        "only valid for private sharing mode",
    )
    .await;
}

#[tokio::test]
async fn private_sharing_without_owner_is_rejected() {
    expect_config_error(
        SecretConfig {
            tenant_id: Some(Uuid::new_v4()),
            owner_id: None,
            key: "k".to_owned(),
            value: "v".to_owned(),
            sharing: Some(SharingMode::Private),
        },
        "requires an explicit owner_id",
    )
    .await;
}

#[tokio::test]
async fn an_invalid_secret_ref_is_rejected() {
    expect_config_error(
        SecretConfig {
            tenant_id: Some(Uuid::new_v4()),
            owner_id: None,
            key: "has:colon".to_owned(),
            value: "v".to_owned(),
            sharing: None,
        },
        "invalid characters",
    )
    .await;
}

#[tokio::test]
async fn duplicate_seeds_in_the_same_key_class_are_rejected() {
    let db = connect_migrated(&memory_dsn()).await;
    let tenant = Uuid::new_v4();
    let cfg = PostgresCredStorePluginConfig {
        secrets: vec![
            SecretConfig {
                tenant_id: Some(tenant),
                owner_id: None,
                key: "dup".to_owned(),
                value: "a".to_owned(),
                sharing: None,
            },
            SecretConfig {
                tenant_id: Some(tenant),
                owner_id: None,
                key: "dup".to_owned(),
                value: "b".to_owned(),
                sharing: None,
            },
        ],
        ..Default::default()
    };
    let err = match Service::from_config(store_over(db), &cfg) {
        Ok(_) => panic!("duplicate must be rejected"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("duplicate"), "unexpected error: {err}");
}

// ── the partial unique indexes ──────────────────────────────────────────────

/// Proves the tenant-class partial unique index exists and bites.
///
/// Inserts two rows with the same `(tenant_id, secret_ref)` and `owner_id
/// NULL`, bypassing `upsert`'s update-first path on purpose. With one plain
/// composite `UNIQUE (tenant_id, owner_id, secret_ref)` this would *succeed*,
/// because `NULL` is not equal to `NULL` in a unique index — which is exactly
/// why the migration builds two partial indexes instead.
#[tokio::test]
async fn duplicate_tenant_class_rows_are_rejected() {
    use sea_orm::{ActiveValue, EntityTrait};
    use toolkit_db::secure::SecureInsertExt;
    use toolkit_security::AccessScope;

    let (provider, _repo) = provider_and_repo(&memory_dsn()).await;
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let conn = provider.conn().expect("conn");

    let row = |value: &str| crate::infra::storage::entity::ActiveModel {
        id: ActiveValue::Set(Uuid::new_v4()),
        tenant_id: ActiveValue::Set(tenant),
        owner_id: ActiveValue::Set(None),
        secret_ref: ActiveValue::Set("dup-ref".to_owned()),
        secret_value: ActiveValue::Set(value.as_bytes().to_vec()),
        created_at: ActiveValue::Set(time::OffsetDateTime::now_utc()),
        updated_at: ActiveValue::Set(time::OffsetDateTime::now_utc()),
    };

    crate::infra::storage::entity::Entity::insert(row("first"))
        .secure()
        .scope_unchecked(&scope)
        .expect("scope")
        .exec(&conn)
        .await
        .expect("first insert");

    let second = crate::infra::storage::entity::Entity::insert(row("second"))
        .secure()
        .scope_unchecked(&scope)
        .expect("scope")
        .exec(&conn)
        .await;

    assert!(
        second.is_err(),
        "the tenant-class partial unique index must reject a duplicate \
         (tenant_id, secret_ref) with owner_id NULL"
    );
}

/// The private-class index must not stop two owners holding the same reference.
#[tokio::test]
async fn two_owners_may_hold_the_same_reference() {
    let svc = memory_service().await;
    let tenant = tid();
    let alice = OwnerId(Uuid::new_v4());
    let bob = OwnerId(Uuid::new_v4());
    let key = sref("same-ref");

    svc.put_value(&tenant, &key, SecretValue::from("a"), Some(&alice))
        .await
        .expect("put alice");
    svc.put_value(&tenant, &key, SecretValue::from("b"), Some(&bob))
        .await
        .expect("put bob");

    assert_eq!(
        svc.get_value(&tenant, &key, Some(&alice))
            .await
            .expect("get")
            .expect("alice")
            .as_bytes(),
        b"a"
    );
    assert_eq!(
        svc.get_value(&tenant, &key, Some(&bob))
            .await
            .expect("get")
            .expect("bob")
            .as_bytes(),
        b"b"
    );
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Number of rows in the value table, read through the entity (not raw SQL).
async fn row_count(db: &toolkit_db::Db) -> u64 {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    use toolkit_security::AccessScope;

    let provider = crate::test_support::provider_over(db.clone());
    let conn = provider.conn().expect("conn");
    crate::infra::storage::entity::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .count(&conn)
        .await
        .expect("count")
}
