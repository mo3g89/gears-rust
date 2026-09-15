//! Infrastructure storage layer - database persistence for the qa-runs gear.
//!
//! ## Architecture
//!
//! This module contains ALL `SeaORM`-specific code and database operations:
//! - `entity/` - `SeaORM` entity definitions (runs, queue rows, per-test
//!   results, schedules, schedule tick claims)
//! - `migrations/` - Database schema migrations, append-only
//! - `mapper.rs` - Conversions between `SeaORM` models and SDK contract types
//! - `runs_sea_repo.rs` / `queue_sea_repo.rs` / `schedules_sea_repo.rs` -
//!   `SecureORM` repositories
//!
//! ## Layering Rules
//!
//! The infrastructure layer:
//! - **Contains**: ALL `SeaORM` imports and database-specific code
//! - **Uses**: `qa_runs_sdk` contract types as the domain model
//! - **Provides**: `Orm*Repository` implementations of the `domain::repos` traits

pub mod entity;
pub mod mapper;
pub mod migrations;
pub mod odata;

pub(crate) mod db;
mod queue_sea_repo;
mod run_logs_sea_repo;
mod runs_sea_repo;
mod schedules_sea_repo;

/// The contended isolation behaviour the `SQLite` tier cannot reach. Gated on
/// the `integration` feature so a default `cargo test` needs no Docker.
#[cfg(all(test, feature = "integration"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod isolation_pg_tests;

pub use queue_sea_repo::OrmQueueRepository;
pub use runs_sea_repo::OrmRunsRepository;
pub use schedules_sea_repo::OrmSchedulesRepository;

/// Shared fixtures for the DB-backed repository tests in `runs_sea_repo`,
/// `queue_sea_repo` and `schedules_sea_repo`.
///
/// **This tier exists because `cargo build` proves nothing about a query.**
/// Table and column names are runtime strings in a `SeaORM` entity, and every
/// `WHERE`, `ORDER BY` and `SET` written by the repositories is likewise only
/// checked when a statement runs. The migration module makes the same argument
/// about the schema and answers it with `SQLite`; this answers it for the
/// queries.
///
/// It lives inline in `mod.rs` rather than in its own file because Task 10 owns
/// the three files it creates and this module's declaration.
///
/// **There is no `crate::test_support` to move it to, and there will not be.**
/// This said one "arrives with the gear assembly in Task 16"; the gear assembly
/// landed and `lib.rs` records the opposite decision — the two harnesses this
/// crate needs already exist closer to what they serve, and a crate-level
/// module would have been a re-export of them. This is one of the two:
/// `domain::service::test_support` is the other, and it is what
/// `api::rest::handlers` drives a `ConcreteAppServices` from.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod test_db {
    use qa_runs_sdk::{ExclusiveTier, NewSchedule, RunParameter, RunSource, RunState, RunTarget};
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::{ConnectOpts, Db, connect_db};
    use toolkit_security::AccessScope;
    use uuid::Uuid;

    use crate::domain::repos::NewRun;

    /// The Postgres image the contended tests run against.
    ///
    /// Pinned rather than inherited from `testcontainers-modules` — see
    /// [`pg_db`] on why the server version is part of what those tests measure.
    #[cfg(feature = "integration")]
    pub const PG_IMAGE_TAG: &str = "15-alpine";

    /// Real Postgres in a container, with this gear's REAL migrations applied.
    ///
    /// # Why a second backend exists at all
    ///
    /// [`inmem_db`] admits one writer at a time, so a race *between two
    /// connections* cannot occur there with a fix or without one — and a test
    /// that passes either way is not coverage. Postgres is the shipped dialect
    /// (this crate links `toolkit-db` with `features = ["sqlite", "pg"]`), so it
    /// is what the contended tests target and whose `READ COMMITTED` default
    /// their reasoning is about.
    ///
    /// Feature-gated so a default `cargo test` neither builds `testcontainers`
    /// nor needs a Docker daemon. Shape copied from
    /// `account-management/tests/common/mod.rs`'s `pg::bring_up_postgres`.
    ///
    /// The container handle rides along in the returned value: dropping it
    /// tears the database down, so callers must hold it for as long as they
    /// hold the [`Db`].
    ///
    /// # The image tag is pinned, and that is not incidental
    ///
    /// `Postgres::default()` carries whatever tag the `testcontainers-modules`
    /// version in `Cargo.lock` happens to hardcode — which was **not** the
    /// version an earlier commit message claimed these tests ran against. The
    /// tests here depend on serializable-snapshot isolation and on `40001`
    /// being raised, so the server version is part of what they measure and
    /// must not move when a dependency is bumped.
    #[cfg(feature = "integration")]
    pub async fn pg_db() -> PgHarness {
        use testcontainers::{ContainerRequest, ImageExt, runners::AsyncRunner};
        use testcontainers_modules::postgres::Postgres;

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

        // Unlike `inmem_db`, the pool must hand out **more than one**
        // connection: the whole point here is two producers overlapping inside
        // the database, and a single-connection pool would serialise them in
        // this process instead — the suite would then pass for a reason that
        // has nothing to do with isolation.
        let db = connect_db(
            &format!("postgres://user:pass@127.0.0.1:{port}/app"),
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
            .expect("failed to run qa-runs migrations");

        PgHarness {
            db,
            _container: container,
        }
    }

    /// A live Postgres container and a pool connected to it.
    #[cfg(feature = "integration")]
    pub struct PgHarness {
        pub db: Db,
        _container: testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
    }

    #[cfg(feature = "integration")]
    async fn wait_for_tcp(port: u16) {
        use std::time::Duration;
        use tokio::net::TcpStream;
        use tokio::time::{Instant, sleep};

        let deadline = Instant::now() + Duration::from_secs(30);
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

    /// In-memory `SQLite` with this gear's REAL migrations applied.
    ///
    /// `max_conns(1)` is load-bearing: each `SQLite` `:memory:` connection is
    /// its own database, so a larger pool would let a query land on one where
    /// the migration never ran.
    pub async fn inmem_db() -> Db {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts)
            .await
            .expect("failed to connect to in-memory sqlite database");

        run_migrations_for_testing(&db, super::migrations::Migrator::migrations())
            .await
            .expect("failed to run qa-runs migrations");
        db
    }

    /// The scope a real PEP compiles for a tenant-isolated subject:
    /// `owner_tenant_id IN [tenant]`, one constraint, one predicate.
    ///
    /// Built directly rather than through `PolicyEnforcer` because Task 10 has
    /// no service layer to run one from — but it is the *same value*: qa-catalog's
    /// `test_support::permissive_response` produces exactly this constraint
    /// from a PDP decision, and `AccessScope::for_tenant` is its constructor.
    ///
    /// **This helper, and every test built over it, never uses
    /// `AccessScope::allow_all()`**: a ground-truth read done that way is how
    /// a cross-tenant probe gets written by accident. (The one place that
    /// call is legitimate is production code, at the single named seam
    /// `domain::elevated::enumeration_scope`, which exists precisely so a
    /// nil-tenant background sweep gets that trust deliberately, at one
    /// audited call site — not as an accident reached from a test.) Where
    /// these tests need ground truth about which tenant a row landed in,
    /// they read it back under the *other* tenant's scope and assert it is
    /// invisible, which is the property that actually matters.
    pub fn scope(tenant: Uuid) -> AccessScope {
        AccessScope::for_tenant(tenant)
    }

    /// 2026-08-13 00:00:00 UTC — the fixture instant `domain` and the
    /// migration tests both use.
    pub fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
    }

    /// A fully-populated launch, so an insert names every column a caller can
    /// reach. Callers override what they care about.
    ///
    /// The columns absent from [`NewRun`] are absent on purpose — each has
    /// exactly one writer and its own test. They are still exercised as
    /// *column names* by the migration module's
    /// `every_entity_round_trips_through_the_migrated_schema`, which inserts
    /// all 35 through a raw `ActiveModel`.
    pub fn sample_new_run(name: &str) -> NewRun {
        NewRun {
            name: name.to_owned(),
            target: RunTarget::Test {
                repo_id: Uuid::from_u128(0x10),
                path: "tests/smoke/plan.yaml".to_owned(),
                test_file: "tests/authn/test_a.py".to_owned(),
            },
            environment_id: Some(Uuid::from_u128(0x11)),
            test_version: Some("release/5.0".to_owned()),
            app_version: Some("5.0.1".to_owned()),
            app_build: Some("20260813".to_owned()),
            state: RunState::Created,
            resolved_exclusive: true,
            exclusive_tier: ExclusiveTier::Plan,
            is_validation: true,
            parameters: vec![RunParameter {
                name: "SKIP_TESTS_WITH_BUGS".to_owned(),
                value: "true".to_owned(),
            }],
            include_tags: vec!["smoke".to_owned(), "e2e".to_owned()],
            exclude_tags: vec!["slow".to_owned()],
            source: RunSource::Scheduled,
            schedule_id: Some(Uuid::from_u128(0x12)),
            bundle_ids: vec![Uuid::from_u128(0x13), Uuid::from_u128(0x14)],
            timeout_at: Some(now()),
        }
    }

    /// A fully-populated schedule, so an insert names every column a caller can
    /// reach. Callers override what they care about.
    ///
    /// `exclusive_choice` is `Some(false)` rather than `None` on purpose: the
    /// default fixture should be the value that is *distinguishable* from the
    /// absent case, so a test that forgets to set it cannot pass because the
    /// column happened to hold the value an omission would also produce.
    /// `every_exclusive_choice_survives_the_column` covers all three.
    pub fn sample_new_schedule(name: &str) -> NewSchedule {
        NewSchedule {
            name: name.to_owned(),
            target: RunTarget::Test {
                repo_id: Uuid::from_u128(0x10),
                path: "tests/smoke/plan.yaml".to_owned(),
                test_file: "tests/authn/test_a.py".to_owned(),
            },
            environment_id: Some(Uuid::from_u128(0x11)),
            branch: Some("release/5.0".to_owned()),
            cron: "0 3 * * *".to_owned(),
            exclusive_choice: Some(false),
            enabled: true,
            include_tags: vec!["smoke".to_owned(), "e2e".to_owned()],
            exclude_tags: vec!["slow".to_owned()],
            parameters: vec![RunParameter {
                name: "SKIP_TESTS_WITH_BUGS".to_owned(),
                value: "true".to_owned(),
            }],
        }
    }
}
