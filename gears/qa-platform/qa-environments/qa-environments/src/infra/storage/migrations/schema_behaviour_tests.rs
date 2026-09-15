//! Behaviour the collapsed schema must have, which a schema dump cannot prove.
//!
//! Column names, types, nullability, defaults, index shape and foreign-key
//! targets are all compared against the pre-collapse schema by an
//! order-insensitive fingerprint taken from `information_schema`. What that
//! cannot see is whether a declared default is actually applied on insert,
//! whether a cascade actually cascades, and whether a unique index is scoped to
//! the tenant or to the whole table.
//!
//! Carried over from the ten migrations the chain was collapsed into
//! `m20260812_000001_initial`. What was dropped with those files was the
//! mechanics of the transition — the per-column `ALTER` spellings, the
//! backfills from the Kubernetes-shaped columns, the `down` paths, the SQLite
//! table rebuilds — which describe a sequence a fresh database never performs.

use sea_orm::{
    ActiveModelTrait, ActiveValue, ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    EntityTrait,
};
use sea_orm_migration::MigratorTrait as _;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::infra::storage::entity::{environment, environment_lease, environment_variable};

fn uuid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
}

/// An in-memory database with the whole (now single-migration) chain applied.
async fn migrated_db() -> DatabaseConnection {
    let mut opts = ConnectOptions::new("sqlite::memory:".to_owned());
    opts.max_connections(1).min_connections(1);
    let conn = Database::connect(opts)
        .await
        .expect("failed to connect to in-memory sqlite database");
    conn.execute_unprepared("PRAGMA foreign_keys = ON;")
        .await
        .expect("failed to enable sqlite foreign key enforcement");
    let manager = sea_orm_migration::SchemaManager::new(&conn);
    for migration in super::Migrator::migrations() {
        migration
            .up(&manager)
            .await
            .expect("failed to run a qa-environments migration");
    }
    conn
}

/// A fully-populated environment, so that every column the entity spells is
/// named in the generated INSERT — which is what makes a column the DDL and the
/// entity disagree about fail here rather than at runtime.
fn environment_am(id: Uuid, tenant: Uuid, name: &str) -> environment::ActiveModel {
    environment::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(tenant),
        name: ActiveValue::Set(name.to_owned()),
        product_id: ActiveValue::Set(uuid(0x99)),
        description: ActiveValue::Set(Some("staging".to_owned())),
        available: ActiveValue::Set(true),
        observed_version: ActiveValue::Set(Some("7.4.0".to_owned())),
        observed_build: ActiveValue::Set(Some("82".to_owned())),
        default_branch: ActiveValue::Set(Some("release/7.4".to_owned())),
        is_default: ActiveValue::Set(true),
        version_detect_error: ActiveValue::Set(None),
        version_detected_at: ActiveValue::Set(Some(now())),
        credentials: ActiveValue::Set(serde_json::json!([{"key": "kubeconfig", "credstore_ref": "credstore://r"}])),
        observed_attrs: ActiveValue::Set(serde_json::json!({"nodes": 3})),
        config: ActiveValue::Set(serde_json::json!({"namespace": "vzt"})),
        observed_base_url: ActiveValue::Set(Some("https://example.invalid".to_owned())),
        health_state: ActiveValue::Set("degraded".to_owned()),
        health_detail: ActiveValue::Set(Some("one node not ready".to_owned())),
        health_checked_at: ActiveValue::Set(Some(now())),
        created_at: ActiveValue::Set(now()),
        updated_at: ActiveValue::Set(now()),
    }
}

fn variable_am(id: Uuid, tenant: Uuid, env: Uuid, name: &str) -> environment_variable::ActiveModel {
    environment_variable::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(tenant),
        environment_id: ActiveValue::Set(env),
        name: ActiveValue::Set(name.to_owned()),
        value: ActiveValue::Set("v".to_owned()),
        created_at: ActiveValue::Set(now()),
        updated_at: ActiveValue::Set(now()),
    }
}

/// Every column round-trips through the entity, which is what proves the
/// declared column and the field the entity spells are the same column.
#[tokio::test]
async fn an_environment_round_trips_through_the_declared_columns() {
    let conn = migrated_db().await;
    environment_am(uuid(1), uuid(2), "staging-a")
        .insert(&conn)
        .await
        .expect("the environment row must insert");

    let stored = environment::Entity::find_by_id(uuid(1))
        .one(&conn)
        .await
        .unwrap()
        .expect("the environment row must read back");
    assert_eq!(stored.product_id, uuid(0x99));
    assert_eq!(stored.observed_build.as_deref(), Some("82"));
    assert_eq!(stored.default_branch.as_deref(), Some("release/7.4"));
    assert!(stored.is_default);
    assert_eq!(stored.health_state, "degraded");
    assert_eq!(stored.observed_attrs, serde_json::json!({"nodes": 3}));
    assert_eq!(stored.config, serde_json::json!({"namespace": "vzt"}));
    assert_eq!(stored.version_detected_at, Some(now()));
}

/// **The four `NOT NULL` columns apply their declared defaults.**
///
/// Unreachable through an `ActiveModel`, which names every column: only a raw
/// insert that omits them can show that the default is the column's and not the
/// caller's.
#[tokio::test]
async fn the_four_not_null_columns_apply_their_declared_defaults() {
    let conn = migrated_db().await;
    conn.execute_unprepared(
        "INSERT INTO qa_environments
             (id, tenant_id, name, product_id, created_at, updated_at)
         VALUES ('e1', 't1', 'defaulted', 'p1', '2026-08-12', '2026-08-12')",
    )
    .await
    .expect("a row naming only the required columns must insert");

    let row = conn
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT available || '/' || credentials || '/' || observed_attrs || '/' || config
                 || '/' || health_state AS v FROM qa_environments WHERE id = 'e1'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .expect("the row must read back");
    let got: String = row.try_get("", "v").unwrap();
    assert_eq!(
        got, "1/[]/{}/{}/unknown",
        "available, credentials, observed_attrs, config and health_state must take \
         their declared defaults; a NULL in any of them is a second, silent reading \
         of the same state"
    );
}

/// An environment naming no product is refused by the column, not merely by the
/// service: there is no fallback product, and a product is what resolves the
/// plugin that governs the environment.
#[tokio::test]
async fn an_environment_cannot_omit_its_product() {
    let conn = migrated_db().await;
    let refused = conn
        .execute_unprepared(
            "INSERT INTO qa_environments
                 (id, tenant_id, name, created_at, updated_at)
             VALUES ('e2', 't1', 'productless', '2026-08-12', '2026-08-12')",
        )
        .await;
    assert!(refused.is_err(), "product_id must be NOT NULL");
}

/// **A variable name is unique per tenant, not globally.**
///
/// Without `tenant_id` in the key, one tenant's variable name collides with
/// another's on the same environment id — a cross-tenant failure no test that
/// inserts as a single tenant can see.
#[tokio::test]
async fn a_variable_name_is_unique_per_tenant_not_globally() {
    let conn = migrated_db().await;
    let env = uuid(1);
    environment_am(env, uuid(2), "staging-a")
        .insert(&conn)
        .await
        .unwrap();

    variable_am(uuid(10), uuid(2), env, "TOKEN")
        .insert(&conn)
        .await
        .expect("the first tenant's variable must insert");
    variable_am(uuid(11), uuid(3), env, "TOKEN")
        .insert(&conn)
        .await
        .expect("a second tenant must be able to use the same variable name");

    variable_am(uuid(12), uuid(2), env, "TOKEN")
        .insert(&conn)
        .await
        .expect_err("the same tenant must not repeat a name on one environment");
}

/// Distinct names coexist on one environment, and one name coexists across two
/// environments of the same tenant. The companions to the uniqueness test: a
/// key that was too *narrow* would fail these instead.
#[tokio::test]
async fn distinct_names_and_distinct_environments_coexist() {
    let conn = migrated_db().await;
    let tenant = uuid(2);
    environment_am(uuid(1), tenant, "staging-a")
        .insert(&conn)
        .await
        .unwrap();
    environment_am(uuid(4), tenant, "staging-b")
        .insert(&conn)
        .await
        .unwrap();

    variable_am(uuid(10), tenant, uuid(1), "TOKEN")
        .insert(&conn)
        .await
        .unwrap();
    variable_am(uuid(11), tenant, uuid(1), "OTHER")
        .insert(&conn)
        .await
        .expect("two names on one environment must coexist");
    variable_am(uuid(12), tenant, uuid(4), "TOKEN")
        .insert(&conn)
        .await
        .expect("one name on two environments must coexist");
}

/// Deleting an environment takes its variables and its lease with it.
///
/// Both foreign keys are declared `ON DELETE CASCADE`; a dump shows the clause,
/// only an execution shows that it fires.
#[tokio::test]
async fn deleting_an_environment_cascades_to_its_variables_and_lease() {
    let conn = migrated_db().await;
    let tenant = uuid(2);
    let env = uuid(1);
    environment_am(env, tenant, "staging-a")
        .insert(&conn)
        .await
        .unwrap();
    variable_am(uuid(10), tenant, env, "TOKEN")
        .insert(&conn)
        .await
        .unwrap();
    environment_lease::ActiveModel {
        environment_id: ActiveValue::Set(env),
        tenant_id: ActiveValue::Set(tenant),
        mode: ActiveValue::Set("exclusive".to_owned()),
        holders: ActiveValue::Set(serde_json::json!([])),
        version: ActiveValue::Set(0),
        updated_at: ActiveValue::Set(now()),
    }
    .insert(&conn)
    .await
    .unwrap();

    environment::Entity::delete_by_id(env)
        .exec(&conn)
        .await
        .unwrap();

    assert!(
        environment_variable::Entity::find_by_id(uuid(10))
            .one(&conn)
            .await
            .unwrap()
            .is_none(),
        "a deleted environment must take its variables with it"
    );
    assert!(
        environment_lease::Entity::find_by_id(env)
            .one(&conn)
            .await
            .unwrap()
            .is_none(),
        "a deleted environment must take its lease with it"
    );
}
