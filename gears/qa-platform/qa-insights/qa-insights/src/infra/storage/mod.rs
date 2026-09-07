//! Infrastructure storage layer — database persistence for the qa-insights gear.
//!
//! ## Architecture
//!
//! This module is where ALL `SeaORM`-specific code lives:
//! - `migrations/` — the database schema, append-only.
//! - `entity/` — `SeaORM` entity definitions (Task 11).
//! - `mapper.rs` — conversions between `SeaORM` models and `qa-insights-sdk`
//!   contract types, plus the two derived values a repository must not leave to
//!   a caller (`plan_key` and the column-width truncation).
//! - `db.rs` — the page-size clamp shared by the `OData` collections, the
//!   `OData` error classification, and `db_err`.
//! - `odata.rs` — the `OData` field allow-lists and column mappings for the two
//!   flat collections.
//! - `*_sea_repo.rs` — the six `SecureORM` repositories, one per
//!   `domain::repos` trait.
//!
//! ## Layering Rules
//!
//! The infrastructure layer:
//! - **Contains**: ALL `SeaORM` imports and database-specific code.
//! - **Uses**: `qa_insights_sdk` contract types as the domain model.
//! - **Provides**: `Orm*Repository` implementations of the `domain::repos`
//!   traits.
//!
//! ## `db.rs` exists as of Task 17, and this is the note it closes
//!
//! Task 10's file list asked for `infra/storage/db.rs`. In `qa-runs` that file
//! is not connection setup — it is `OData` error conversion plus `PAGE_LIMITS`,
//! and it exists because that gear ships two `OData` collections. This gear's
//! arrived at Task 17, so this section used to say the file was deliberately
//! absent until then, and that Task 12's placement of the siblings' `db_err` in
//! [`mapper`] — rather than founding a file for three lines — was to be undone by
//! Task 17.
//!
//! **Both halves are now done**: [`db`] holds `PAGE_LIMITS`, `odata_err` and
//! `db_err`, and every repository imports the last of those from there. The one
//! thing qa-runs' copy has and this does not is `is_retryable_contention`; [`db`]
//! says why.

pub mod collect_sea_repo;
pub mod db;
pub mod entity;
pub mod jira_sea_repo;
pub mod mapper;
pub mod migrations;
pub mod notify_sea_repo;
pub mod odata;
pub mod results_sea_repo;
pub mod saved_views_sea_repo;
pub mod watermark_sea_repo;

/// Real-Postgres fixtures for the tests that `SQLite` cannot answer.
///
/// **Pulled forward from Tasks 13-15 by Task 10**, and for a narrower reason
/// than those tasks will have. They need it for ingest races; this needs it
/// because `POSTGRES_UP` — the only dialect this gear actually ships — had
/// *zero* automated verification. The two parity tests prove the three blobs
/// agree with one another; nothing proved that any of them runs, and Postgres
/// is the one where that matters. A syntax error or a rejected default in
/// `POSTGRES_UP` would have reached a deployment with the whole suite green.
///
/// Gated on the `integration` feature so a default `cargo test` needs no Docker
/// daemon:
///
/// ```text
/// cargo test -p qa-insights --features integration --lib
/// ```
///
/// Shape copied from `qa-runs/src/infra/storage/mod.rs::test_db`, pinned tag
/// and all. Tasks 13-15 extend this module rather than rebuilding it.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod test_db {
    use time::OffsetDateTime;
    use toolkit_db::secure::{AccessScope, Db};
    use uuid::Uuid;

    /// In-memory `SQLite` with this gear's REAL migrations applied through the
    /// real [`super::migrations::Migrator`], behind a `toolkit-db` pool.
    ///
    /// `max_conns(1)` **is** load-bearing, for the same reason the entity
    /// module's `migrated_db` says: each `SQLite` `:memory:` connection is its
    /// own database, so a larger pool would let a statement land on a
    /// connection where the migration never ran. It is also what makes this
    /// tier serialize every writer, which is why the ingest races of Tasks
    /// 13-15 need [`pg_db`] and cannot be falsified here.
    ///
    /// A `Db` rather than a raw `DatabaseConnection`, unlike the entity module's
    /// harness: repositories take `&impl DBRunner`, and that trait is sealed
    /// inside `toolkit-db`.
    ///
    /// **Four types implement it, not two** — `DbConn`, `DbTx`, `SecureConn` and
    /// `SecureTx` (`libs/toolkit-db/src/secure/runner.rs:51-77`); an earlier
    /// version of this comment said two, from reading half the list. The
    /// conclusion survives the correction, for a different reason than it gave:
    /// none of the four can be built from a raw `DatabaseConnection` outside
    /// `toolkit-db`, because every constructor and field is `pub(crate)`. So a
    /// repository test has to come through a `Db`, which is the point.
    pub async fn inmem_db() -> Db {
        use sea_orm_migration::MigratorTrait;
        use toolkit_db::migration_runner::run_migrations_for_testing;
        use toolkit_db::{ConnectOpts, connect_db};

        let db = connect_db(
            "sqlite::memory:",
            ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("failed to connect to in-memory sqlite database");

        run_migrations_for_testing(&db, super::migrations::Migrator::migrations())
            .await
            .expect("failed to run qa-insights migrations");
        db
    }

    /// The scope a real PEP compiles for a tenant-isolated subject:
    /// `owner_tenant_id IN [tenant]`, one constraint, one predicate.
    ///
    /// Built directly rather than through `PolicyEnforcer` because Task 12 has
    /// no service layer to run one from — but it is the *same value*, and
    /// `AccessScope::for_tenant` is its constructor. Shape and reasoning copied
    /// from `qa-runs/src/infra/storage/mod.rs::test_db::scope`.
    ///
    /// **This helper, and every test built over it, never uses
    /// `AccessScope::allow_all()`**: a ground-truth read done that way is how a
    /// cross-tenant probe gets written by accident. (The one place that call is
    /// legitimate is production code, at the single named seam
    /// `domain::elevated::enumeration_scope`, which exists precisely so the
    /// nil-tenant ticker enumeration gets that trust deliberately, at one
    /// audited call site — not as an accident reached from a test.) Where these
    /// tests need ground truth about which tenant a row landed in, they read it
    /// back under the *other* tenant's scope and assert it is invisible, which
    /// is the property that actually matters.
    pub fn scope(tenant: Uuid) -> AccessScope {
        AccessScope::for_tenant(tenant)
    }

    /// 2026-08-18 00:00:00 UTC — the fixture instant the migration and entity
    /// modules already use, so a failure anywhere in `infra::storage` names the
    /// same value.
    pub fn now() -> OffsetDateTime {
        time::macros::datetime!(2026-08-18 00:00:00 UTC)
    }

    /// The Postgres image the integration tier runs against.
    ///
    /// **Pinned, not inherited.** `Postgres::default()` carries whatever tag
    /// the `testcontainers-modules` version in `Cargo.lock` happens to
    /// hardcode, and qa-runs records that this was *not* the version an earlier
    /// commit message claimed its tests ran against. Here the server version is
    /// part of what is being measured — whether this DDL is accepted — so it
    /// must not move silently when a dependency is bumped.
    ///
    /// **Why 15 and not something newer.** For a DDL-*acceptance* test the
    /// older server is strictly the stronger check: a schema verified on 15 is
    /// verified on 16, and the reverse does not hold — a 16-only spelling would
    /// pass here and fail on any deployment still running 15. Nothing in
    /// `POSTGRES_UP` is 16-only (`TIMESTAMPTZ`, `JSONB`, `UUID` and descending
    /// index columns all long predate 15), so the older major costs nothing and
    /// buys coverage. An earlier draft pinned `16-alpine` for no stated reason.
    ///
    /// It also matches `qa-runs`' pin, which is worth having but is *not* the
    /// argument — that gear's isolation tests measure serializable-snapshot
    /// behaviour, so its version choice is load-bearing for a different reason
    /// than this one. Two constants, same value, two independent
    /// justifications; a future gear should copy the reasoning above rather
    /// than the digit.
    #[cfg(feature = "integration")]
    pub const PG_IMAGE_TAG: &str = "15-alpine";

    /// A live Postgres container, a `toolkit-db` pool onto it, and its URL.
    ///
    /// The container handle rides along: dropping it tears the database down,
    /// so callers must hold the harness for as long as they use either field.
    ///
    /// `url` is here because `Db` deliberately exposes no raw connection
    /// (`sea_internal` is `pub(crate)` to `toolkit-db`), and a *schema* test has
    /// to read the server's catalogues. See [`PgHarness::db`] for that field's
    /// consumers — a retraction that had itself gone stale used to sit here and
    /// contradict it six lines down.
    #[cfg(feature = "integration")]
    pub struct PgHarness {
        /// The pool the transaction-scoped tests drive.
        ///
        /// **The `expect(dead_code)` that used to sit here is gone**, which is
        /// exactly what its own doc said should happen: "the first task to use the
        /// field makes this attribute itself go red, which is the prompt to delete
        /// it." The first user is
        /// `notify_sea_repo::tests::a_lost_claim_inside_a_transaction_leaves_the_transaction_usable`,
        /// which needs a real Postgres because the property it measures —
        /// a failed statement aborting the transaction — does not exist on
        /// `SQLite`. `watermark_sea_repo`'s
        /// `an_advance_no_op_inside_a_transaction_leaves_the_transaction_usable`
        /// is the second, for the same reason. The ingest races of Tasks 13-15 are
        /// the next users.
        ///
        /// `max_conns(16)` above is the subtle half of this harness: it is what
        /// lets two producers overlap *inside* the database.
        pub db: toolkit_db::Db,
        pub url: String,
        _container: testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
    }

    /// Real Postgres in a container, with this gear's REAL migrations applied
    /// through the real [`super::migrations::Migrator`].
    #[cfg(feature = "integration")]
    pub async fn pg_db() -> PgHarness {
        use sea_orm_migration::MigratorTrait;
        use testcontainers::{ContainerRequest, ImageExt, runners::AsyncRunner};
        use testcontainers_modules::postgres::Postgres;
        use toolkit_db::migration_runner::run_migrations_for_testing;
        use toolkit_db::{ConnectOpts, connect_db};

        let request = ContainerRequest::from(Postgres::default())
            .with_tag(PG_IMAGE_TAG)
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app");
        let container = request.start().await.expect("postgres container starts");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("the container publishes 5432");

        wait_for_tcp(port).await;

        let url = format!("postgres://user:pass@127.0.0.1:{port}/app");

        // More than one connection, unlike the `SQLite` tier: the ingest races
        // of Tasks 13-15 need two producers overlapping *inside* the database,
        // and a single-connection pool would serialise them in this process
        // instead. Irrelevant to this task's schema test, correct for the
        // module.
        let db = connect_db(
            &url,
            ConnectOpts {
                max_conns: Some(16),
                min_conns: Some(4),
                ..Default::default()
            },
        )
        .await
        .expect("failed to connect to the postgres container");

        run_migrations_for_testing(&db, super::migrations::Migrator::migrations())
            .await
            .expect("failed to run qa-insights migrations against postgres");

        PgHarness {
            db,
            url,
            _container: container,
        }
    }

    /// A **second** pool onto a [`PgHarness`]'s database, for a test that needs
    /// two independent clients of one server.
    ///
    /// [`pg_db`] already returns a pool, and two `Db` handles cloned from it
    /// would share a connection pool — fine for most things and wrong for the
    /// one property
    /// `domain::service::jira_poller_tests::two_concurrent_pollers_produce_one_rerun`
    /// measures, which is what two *replicas* do to one database. Two pools is
    /// as close to two processes as a single-process test gets: separate
    /// connections, separate transactions, and nothing shared but the server.
    ///
    /// No migrations: [`pg_db`] has already run them against this database, and
    /// running them twice is what the runner's own bookkeeping is there to
    /// prevent, not something to rely on.
    #[cfg(feature = "integration")]
    pub async fn pg_second_pool(url: &str) -> toolkit_db::Db {
        use toolkit_db::{ConnectOpts, connect_db};

        connect_db(
            url,
            ConnectOpts {
                max_conns: Some(8),
                min_conns: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("failed to open a second pool onto the postgres container")
    }

    #[cfg(feature = "integration")]
    async fn wait_for_tcp(port: u16) {
        use std::time::Duration;
        use tokio::net::TcpStream;
        use tokio::time::{Instant, sleep};

        // 90s, where the sibling gear uses 30. The image pull happens inside
        // `start()`, so this covers only the server's own readiness — but this
        // tier starts **one container per test**, and `cargo test` runs them in
        // parallel. Several Postgres servers booting at once on a cold or
        // CPU-starved machine exceeded 30s once during Task 12: the failure
        // surfaced in the *schema* test, not in the one that had just been
        // added, which is the signature of contention rather than of a bad test.
        // The margin — not the mean — is what needed raising.
        //
        // # Task 13 was asked to decide the container strategy, and decided to
        // # keep one per test
        //
        // The plan's carried item 8 flagged that Tasks 13-15 would add more
        // containers and called this "the last comfortable moment to decide",
        // with a shared container plus a per-test `CREATE DATABASE` as the
        // alternative. Task 13 added a fifth (the offset store's Postgres
        // round-trip) and kept the strategy, for a measured reason and a named
        // hazard:
        //
        // * **Measured 2026-08-20:** the whole integration tier — five
        //   containers, started concurrently — completed in **5.23s** on the
        //   execution machine, from a warm image cache. The contention that
        //   motivated the 30s→90s raise is not currently binding, so sharing
        //   would be paying a real cost to fix a hypothetical one.
        // * **The hazard sharing introduces is worse than the one it removes.**
        //   A shared container has to live in a process-wide `OnceCell`, while
        //   every `#[tokio::test]` builds and drops its *own* runtime.
        //   `testcontainers`' handle owns background work bound to the runtime
        //   that created it, so the first test to finish would drop the runtime
        //   underneath a container the remaining tests still hold. Doing it
        //   safely means a dedicated container runtime — more harness than the
        //   contention justifies.
        //
        // Per-test databases would still have answered the "these tests assert
        // table inventories" objection, which was the reason recorded before and
        // is *not* the reason now. Revisit when the tier is slow enough to
        // measure, not before.
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the postgres container on {port}",
            );
            sleep(Duration::from_millis(200)).await;
        }
    }
}
