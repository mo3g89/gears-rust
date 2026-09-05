//! Characterization test for a third-party exposure this plugin cannot fix.
//!
//! `sea-orm` annotates every driver entry point with
//! `#[instrument(level = "trace")]` (`sea-orm-1.1.20/src/driver/sqlx_postgres.rs:61`
//! and `.../sqlx_sqlite.rs:62`, and the same on every sibling method), and the
//! span it opens carries the whole `sea_orm::Statement` as a `Debug` field —
//! `values` included, i.e. every bound parameter. So with `TRACE` enabled for
//! `sea_orm::driver::*`, a stored secret's bytes DO appear in log lines, as the
//! decimal array `Vec<u8>`'s `Debug` produces.
//!
//! This plugin cannot suppress that: the value must be bound as a parameter
//! (inlining it into SQL would be strictly worse), and `sea-orm` offers no hook
//! to redact a span field. Asserting the exposure here, rather than ignoring
//! it, makes the boundary of the crate's no-leak guarantee explicit and turns a
//! future `sea-orm` change into a test failure instead of a silent shift.
//!
//! Its own test binary because the guarantee test in
//! `src/infra/storage/leak_tests.rs` installs a process-global `DEBUG`
//! subscriber, which caps `tracing`'s `LevelFilter::current()` and stops these
//! `TRACE` spans from ever opening.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::{Arc, Mutex};

use credstore_sdk::{SecretRef, SecretValue, TenantId};
use postgres_credstore_plugin::config::PostgresCredStorePluginConfig;
use postgres_credstore_plugin::domain::Service;
use postgres_credstore_plugin::infra::storage::error::StoreError;
use postgres_credstore_plugin::infra::storage::migrations::Migrator;
use postgres_credstore_plugin::infra::storage::repo::ValueRepo;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

const SENTINEL: &str = "SENTINEL-SECRET-9d41c7f2e8b6a350-DO-NOT-LOG";

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn sea_orm_trace_spans_render_bound_parameters() {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(SharedWriter(Arc::clone(&buf)))
        .with_max_level(tracing::Level::TRACE)
        .finish();
    // Global: `sea-orm`'s `#[instrument(level = "trace")]` spans, like sqlx's
    // statement logging, are gated on the process-wide `LevelFilter::current()`,
    // which only `set_global_default` raises.
    tracing::subscriber::set_global_default(subscriber).expect("install subscriber");

    let dsn = format!(
        "sqlite:file:pg_credstore_trace_{}?mode=memory&cache=shared",
        Uuid::new_v4()
    );
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");

    let svc = Service::from_config(
        ValueRepo::new(Arc::new(DBProvider::<StoreError>::new(db))),
        &PostgresCredStorePluginConfig::default(),
    )
    .expect("config builds");

    svc.put_value(
        &TenantId(Uuid::new_v4()),
        &SecretRef::new("trace-probe").expect("valid ref"),
        SecretValue::from(SENTINEL),
        None,
    )
    .await
    .expect("put");

    let log = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
    let decimal = SENTINEL
        .as_bytes()
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");

    assert!(
        log.contains(&decimal),
        "sea-orm no longer renders bound parameters in its trace spans. That is \
         an improvement, not a regression — widen the guarantee: update \
         CAPTURE_LEVEL in src/infra/storage/leak_tests.rs, this test, and the \
         README.\n{log}"
    );
}
