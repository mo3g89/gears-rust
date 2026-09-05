//! Restart survival against a **real PostgreSQL server**.
//!
//! The in-lib test `a_secret_survives_dropping_and_rebuilding_the_service`
//! proves the same sequence over a file-backed SQLite database, which is enough
//! to exercise the code path but not the production engine. This one runs it on
//! PostgreSQL, and additionally checks the two properties that are engine
//! specific:
//!
//! * the migration lands in **this gear's own** history table
//!   (`toolkit_migrations__postgres_credstore_plugin__<hash8>`), which is what
//!   makes sharing the `credstore` database with the credstore gear safe;
//! * the two **partial** unique indexes reject duplicates in both key classes
//!   while still allowing two owners to hold the same reference — the property
//!   a single composite index over `(tenant_id, owner_id, secret_ref)` silently
//!   loses, because `NULL` is not equal to `NULL` in a unique index.
//!
//! # Running it
//!
//! Skipped unless `CREDSTORE_PG_TEST_DSN` is set, so a default `cargo test`
//! never needs a database. It prints a skip notice rather than passing
//! silently.
//!
//! ```text
//! docker run -d --name scratch-pg -e POSTGRES_PASSWORD=scratch \
//!     -e POSTGRES_USER=scratch -e POSTGRES_DB=credstore \
//!     -p 127.0.0.1:55433:5432 postgres:16-alpine
//! CREDSTORE_PG_TEST_DSN=postgres://scratch:scratch@127.0.0.1:55433/credstore \
//!     cargo test -p cf-gears-postgres-credstore-plugin --test restart_survival_pg
//! ```
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use credstore_sdk::{OwnerId, SecretRef, SecretValue, TenantId};
use postgres_credstore_plugin::config::PostgresCredStorePluginConfig;
use postgres_credstore_plugin::domain::Service;
use postgres_credstore_plugin::infra::storage::error::StoreError;
use postgres_credstore_plugin::infra::storage::migrations::Migrator;
use postgres_credstore_plugin::infra::storage::repo::ValueRepo;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_gear;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

/// The gear name the platform's DB phase would pass, so the history table this
/// test creates is byte-for-byte the one production uses.
const GEAR_NAME: &str = "postgres-credstore-plugin";

const DSN_ENV: &str = "CREDSTORE_PG_TEST_DSN";

/// Build a service over the DSN, running migrations first (idempotent).
async fn boot(dsn: &str) -> Service {
    let db = connect_db(
        dsn,
        ConnectOpts {
            max_conns: Some(2),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect postgres");

    run_migrations_for_gear(&db, GEAR_NAME, Migrator::migrations())
        .await
        .expect("run migrations");

    Service::from_config(
        ValueRepo::new(Arc::new(DBProvider::<StoreError>::new(db))),
        &PostgresCredStorePluginConfig::default(),
    )
    .expect("config builds")
}

#[tokio::test]
async fn a_secret_survives_a_restart_against_real_postgres() {
    let Ok(dsn) = std::env::var(DSN_ENV) else {
        eprintln!("SKIPPED: {DSN_ENV} is not set; see this file's header to run it");
        return;
    };

    // Fresh identities per run, so repeated runs against the same database
    // cannot collide or read each other's rows.
    let tenant = TenantId(Uuid::new_v4());
    let other_tenant = TenantId(Uuid::new_v4());
    let owner = OwnerId(Uuid::new_v4());
    let second_owner = OwnerId(Uuid::new_v4());
    let key = SecretRef::new("ssh-key").expect("valid ref");

    // ── boot 1: write ──
    {
        let svc = boot(&dsn).await;
        svc.put_value(
            &tenant,
            &key,
            SecretValue::from("SSH-PRIVATE-KEY-BODY"),
            None,
        )
        .await
        .expect("put tenant");
        svc.put_value(
            &tenant,
            &key,
            SecretValue::from("OWNED-TOKEN"),
            Some(&owner),
        )
        .await
        .expect("put private");
        svc.put_value(
            &tenant,
            &key,
            SecretValue::from("OTHER-OWNER"),
            Some(&second_owner),
        )
        .await
        .expect("put second owner");
        svc.put_value(&other_tenant, &key, SecretValue::from("OTHER-TENANT"), None)
            .await
            .expect("put other tenant");
        // Dropping `svc` drops the repository, the DBProvider, the Db handle
        // and the connection pool — the process-level teardown a container
        // recreation performs.
    }

    // ── boot 2: read back through a brand-new stack ──
    {
        let svc = boot(&dsn).await;
        assert_eq!(
            svc.get_value(&tenant, &key, None)
                .await
                .expect("get")
                .expect("tenant secret survived the restart")
                .as_bytes(),
            b"SSH-PRIVATE-KEY-BODY"
        );
        assert_eq!(
            svc.get_value(&tenant, &key, Some(&owner))
                .await
                .expect("get")
                .expect("private secret survived the restart")
                .as_bytes(),
            b"OWNED-TOKEN"
        );
        assert_eq!(
            svc.get_value(&tenant, &key, Some(&second_owner))
                .await
                .expect("get")
                .expect("the second owner's secret survived too")
                .as_bytes(),
            b"OTHER-OWNER"
        );
        // Tenant isolation, on the real engine.
        assert_eq!(
            svc.get_value(&other_tenant, &key, None)
                .await
                .expect("get")
                .expect("other tenant's own row")
                .as_bytes(),
            b"OTHER-TENANT"
        );
        assert!(
            svc.get_value(&TenantId(Uuid::new_v4()), &key, None)
                .await
                .expect("get")
                .is_none(),
            "an unrelated tenant must not resolve this reference"
        );

        // Overwrite, then delete one class only.
        svc.put_value(&tenant, &key, SecretValue::from("ROTATED"), None)
            .await
            .expect("rotate");
        svc.delete_value(&tenant, &key, Some(&owner))
            .await
            .expect("delete private");
    }

    // ── the migration history is namespaced per gear ──
    //
    // Sharing the `credstore` database with the credstore gear is only safe
    // because each gear's applied-migration list lives in its own table:
    // `toolkit_migrations__postgres_credstore_plugin__<hash8>` versus
    // `toolkit_migrations__credstore__<hash8>`. Running this plugin's migration
    // set under a *second* gear name over the same database therefore has to
    // succeed — a fresh history table, and the schema statements themselves
    // idempotent (`CREATE TABLE/INDEX IF NOT EXISTS`).
    //
    // Kept inside this test rather than beside it: `CREATE TABLE IF NOT EXISTS`
    // is not atomic against a concurrent identical create in PostgreSQL (it
    // raises a `pg_type_typname_nsp_index` unique violation), so two tests
    // racing on the same history table would flake. Production runs the DB
    // phase sequentially, gear by gear.
    {
        let db = connect_db(&dsn, ConnectOpts::default())
            .await
            .expect("connect postgres");
        run_migrations_for_gear(
            &db,
            "credstore-second-history-probe",
            Migrator::migrations(),
        )
        .await
        .expect("a second gear's migration history is independent");
    }

    // ── boot 3: the rotation and the delete both stuck ──
    {
        let svc = boot(&dsn).await;
        assert_eq!(
            svc.get_value(&tenant, &key, None)
                .await
                .expect("get")
                .expect("rotated value survived")
                .as_bytes(),
            b"ROTATED"
        );
        assert!(
            svc.get_value(&tenant, &key, Some(&owner))
                .await
                .expect("get")
                .is_none(),
            "a delete must survive a restart as well as a write"
        );
        assert_eq!(
            svc.get_value(&tenant, &key, Some(&second_owner))
                .await
                .expect("get")
                .expect("the untouched owner is unaffected")
                .as_bytes(),
            b"OTHER-OWNER"
        );
    }
}
