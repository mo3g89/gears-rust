//! Planted-leak test: this plugin's own code must never put secret bytes in a
//! log line, at any verbosity a shipped configuration can select.
//!
//! # This test is break-tested
//!
//! A leak test that cannot fail is worse than none. This one was verified by
//! planting a real leak in [`ValueRepo::upsert`](super::repo::ValueRepo::upsert)
//! and watching it fail before being committed. The plant, the failure output,
//! and its removal are recorded in the task report.
//!
//! The first version of this test also *missed* a real leak, which is why the
//! needle list below has three encodings: see the note on `CAPTURE_LEVEL` and
//! `tests/sea_orm_trace_exposure.rs`.
//!
//! # What the net covers
//!
//! Every `tracing` event emitted anywhere in the process while the full
//! `seed`/`put`/overwrite/`get`/`delete` sequence runs, plus a storage-failure
//! path (where an error message quoting a bound parameter would surface),
//! captured at [`CAPTURE_LEVEL`]. That includes `sqlx`'s own `sqlx::query`
//! statement logging, which is the realistic way a value reaches a log file if
//! a driver ever renders bound parameters — and, per the second test in this
//! module, is exactly what happens one level higher.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::sync::{Arc, Mutex, OnceLock};

use credstore_sdk::{OwnerId, SecretRef, SecretValue, TenantId};
use uuid::Uuid;

use crate::config::{PostgresCredStorePluginConfig, SecretConfig};
use crate::domain::Service;
use crate::test_support::{connect_migrated, memory_dsn, repo_over};

/// A distinctive value that cannot occur in a log line by accident.
const SENTINEL: &str = "SENTINEL-SECRET-9d41c7f2e8b6a350-DO-NOT-LOG";

/// The verbosity the absence claim is made at.
///
/// `DEBUG`, not `TRACE`, and the distinction is load-bearing rather than
/// convenient. `sea-orm` annotates every driver entry point with
/// `#[instrument(level = "trace")]` (`sea-orm-1.1/src/driver/sqlx_postgres.rs`,
/// `.../sqlx_sqlite.rs`), and the span it opens carries the whole
/// `sea_orm::Statement` — including `values`, i.e. every bound parameter — as a
/// `Debug` field. Any event emitted inside that span therefore prints the
/// secret's bytes as a decimal array. That is third-party instrumentation this
/// plugin cannot suppress: the value has to be bound as a parameter (inlining
/// it into SQL would be far worse), and `sea-orm` offers no hook to redact a
/// span field.
///
/// So the claim this module proves is the one that is actually true and
/// actually matters: **at `DEBUG` and below — the most verbose setting any
/// shipped config selects, `logging.sqlx.console_level: debug` in
/// `qa-platform-stack.yaml` — no secret byte reaches a log line.** The
/// `TRACE`-level exposure is pinned by `tests/sea_orm_trace_exposure.rs`
/// instead of being quietly tolerated (it needs its own process: a global
/// `DEBUG` subscriber caps `LevelFilter::current()` and would stop `sea-orm`'s
/// trace spans from ever opening), and is called out in the crate `README`.
const CAPTURE_LEVEL: tracing::Level = tracing::Level::DEBUG;

/// A `Write` sink appending to a shared buffer.
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

/// Install a **process-global** capture subscriber at [`CAPTURE_LEVEL`].
///
/// Global rather than thread-local on purpose. `sqlx` decides whether to log a
/// statement with `tracing`'s *dynamic* level check
/// (`sqlx-core/src/logger.rs`: `private_tracing_dynamic_enabled!`), which
/// consults the process-wide `LevelFilter::current()` — a value only
/// `set_global_default` raises. Under a thread-local `set_default` that check
/// short-circuits and no `sqlx::query` event is ever emitted, silently removing
/// driver logging (the most realistic leak vector) from the net. Verified: the
/// positive control below asserts a `sqlx::query` event is present, and it does
/// not fire under `set_default`.
///
/// A shared buffer also collects other tests' events, which cannot weaken an
/// *absence* assertion about a sentinel no other test uses.
fn capture() -> Arc<Mutex<Vec<u8>>> {
    static BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    Arc::clone(BUF.get_or_init(|| {
        // Ignored on purpose: a second call is a no-op, and if the bridge
        // cannot be installed the tracing capture still works.
        let _bridge = tracing_log::LogTracer::init();
        let buf = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(SharedWriter(Arc::clone(&buf)))
            .with_max_level(CAPTURE_LEVEL)
            .finish();
        let _installed = tracing::subscriber::set_global_default(subscriber);
        buf
    }))
}

fn captured(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()
}

/// The three renderings a byte string realistically takes in a log line:
/// the raw text, lowercase hex, and the decimal array `Vec<u8>`'s `Debug`
/// produces (the form that slipped past the first version of this test).
fn needles() -> Vec<(&'static str, String)> {
    let bytes = SENTINEL.as_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        write!(hex, "{b:02x}").expect("writing to a String cannot fail");
    }
    let decimal = bytes
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    vec![
        ("raw text", SENTINEL.to_owned()),
        ("hex", hex),
        ("decimal byte array", decimal),
    ]
}

/// Run the full store lifecycle with a sentinel value, and a failing write.
async fn exercise_every_path(tenant: &TenantId, owner: &OwnerId, key: &SecretRef) {
    let dsn = memory_dsn();

    // Happy path, including a configured seed carrying the sentinel.
    let cfg = PostgresCredStorePluginConfig {
        secrets: vec![SecretConfig {
            tenant_id: Some(Uuid::new_v4()),
            owner_id: None,
            key: "seeded-probe".to_owned(),
            value: SENTINEL.to_owned(),
            sharing: None,
        }],
        ..Default::default()
    };
    let svc =
        Service::from_config(repo_over(connect_migrated(&dsn).await), &cfg).expect("config builds");
    svc.seed().await.expect("seed");

    svc.put_value(tenant, key, SecretValue::from(SENTINEL), None)
        .await
        .expect("put tenant");
    svc.put_value(tenant, key, SecretValue::from(SENTINEL), Some(owner))
        .await
        .expect("put private");
    // Overwrite: exercises `upsert`'s UPDATE branch as well as its INSERT.
    svc.put_value(tenant, key, SecretValue::from(SENTINEL), None)
        .await
        .expect("overwrite");
    let read_back = svc
        .get_value(tenant, key, None)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(read_back.as_bytes(), SENTINEL.as_bytes());
    svc.delete_value(tenant, key, None).await.expect("delete");

    // Failure path: a store whose table was never created. This is where an
    // ORM/driver error message quoting a bound parameter would show up, and it
    // goes through `map_store_err`, which logs at `warn`.
    let broken = Service::from_config(
        repo_over(
            toolkit_db::connect_db(
                &memory_dsn(),
                toolkit_db::ConnectOpts {
                    max_conns: Some(1),
                    min_conns: Some(1),
                    ..Default::default()
                },
            )
            .await
            .expect("connect"),
        ),
        &PostgresCredStorePluginConfig::default(),
    )
    .expect("config builds");
    let err = broken
        .put_value(tenant, key, SecretValue::from(SENTINEL), None)
        .await
        .expect_err("a put against a missing table must fail");
    // The SPI error itself must not carry the value either.
    let rendered = err.to_string();
    assert!(
        !rendered.contains(SENTINEL),
        "the SPI error leaked the secret: {rendered}"
    );
}

#[tokio::test]
async fn secret_bytes_never_reach_a_log_line() {
    let tenant = TenantId(Uuid::new_v4());
    let owner = OwnerId(Uuid::new_v4());
    let key = SecretRef::new("leak-probe").expect("valid ref");

    let buf = capture();
    tracing::info!("leak-capture-canary");

    exercise_every_path(&tenant, &owner, &key).await;

    let log = captured(&buf);

    // ── positive controls: the net is real, or the absence below is vacuous ──
    assert!(
        log.contains("leak-capture-canary"),
        "log capture is not working"
    );
    assert!(
        log.contains("credstore value store operation failed"),
        "the plugin's own warn on a storage fault was not captured, so the \
         error path is outside the net.\n{log}"
    );
    assert!(
        log.contains("sqlx::query"),
        "sqlx's statement logging was not captured, so driver logging — the \
         most realistic leak vector — is outside the net.\n{log}"
    );
    assert!(
        log.contains("credstore_plugin_values"),
        "no statement touching this plugin's table was captured.\n{log}"
    );

    // ── the claim ──
    for (form, needle) in needles() {
        assert!(
            !log.contains(&needle),
            "secret bytes reached a log line as {form}:\n{log}"
        );
    }
}
