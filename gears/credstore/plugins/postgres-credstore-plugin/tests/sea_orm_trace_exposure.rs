//! `sea-orm`'s driver trace spans must not render bound parameters.
//!
//! Under `sea-orm` 1.1 they did. Every driver entry point carries
//! `#[instrument(level = "trace")]`, and the span it opened held the whole
//! `sea_orm::Statement` as a `Debug` field -- `values` included -- so with
//! `TRACE` enabled a stored secret's bytes appeared in log lines as the decimal
//! array `Vec<u8>`'s `Debug` produces. The plugin could not suppress it: the
//! value has to be bound as a parameter, and `sea-orm` offered no hook to
//! redact a span field.
//!
//! This file was written as a *characterization* of that exposure -- asserting
//! it rather than ignoring it -- so that a future `sea-orm` change would arrive
//! as a test failure instead of a silent shift. On the 2.0 upgrade it did
//! exactly that: 2.0 renders the SQL with placeholders and no values. The
//! assertion is therefore inverted, and it is now a guarantee rather than a
//! characterization. `leak_tests.rs`' `CAPTURE_LEVEL` widened from `DEBUG` to
//! `TRACE` on the strength of it.
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
use postgres_credstore_plugin::infra::storage::store::PgValueStore;
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
async fn sea_orm_trace_spans_do_not_render_bound_parameters() {
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

    let repo = ValueRepo::new(Arc::new(DBProvider::<StoreError>::new(db)));
    let svc = Service::from_config(
        Arc::new(PgValueStore::new(repo)),
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
        !log.contains(&decimal),
        "a bound parameter's bytes reached a TRACE log line: sea-orm is \
         rendering values into its driver spans again. This is the regression \
         `leak_tests.rs` widened CAPTURE_LEVEL to TRACE on the strength of -- \
         narrow it back to DEBUG and restore the README's TRACE warning if this \
         cannot be fixed upstream.\n{log}"
    );
    assert!(
        !log.contains(SENTINEL),
        "the sentinel reached a TRACE log line in some other encoding.\n{log}"
    );
}
