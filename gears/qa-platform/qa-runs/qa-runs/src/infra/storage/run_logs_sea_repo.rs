//! `impl RunLogsRepository for OrmRunsRepository`.
//!
//! # `CONCAT(text, $1)`, not `text || $1`
//!
//! The plan's brief for this file expected `Expr::col(..).concat(..)` to
//! render `||` on Postgres and `CONCAT` on `MySQL`. That method exists in
//! `sea-query` only as `extension::postgres::PgExpr::concat` — a
//! Postgres-specific operator. Rendered against the `MySQL` or `SQLite`
//! query builder it hits `sea-query-0.32.7`'s
//! `QueryBuilder::prepare_bin_oper_common`'s catch-all `_ => unimplemented!()`
//! arm, because neither builder overrides handling for
//! `BinOper::PgOperator`, and `MySQL`'s default `sql_mode` treats a literal
//! `||` as logical OR rather than concatenation in any case.
//!
//! **Dispatching on the backend at this call site is not an option, either.**
//! `append_log` is generic over `C: DBRunner`, and `DBRunner` deliberately
//! exposes no way to ask a connection its backend — the internal trait that
//! could yield one is not nameable outside `toolkit-db`. This is not a guess:
//! `qa-insights`'s `infra::storage::notify_sea_repo` hit the identical wall
//! (`let _ = runner.get_database_backend();` fails to compile there) and its
//! module doc records the same finding.
//!
//! So this uses `Func::cust(Alias::new("CONCAT")).arg(..).arg(..)`, a
//! `sea_query::FunctionCall` with a custom function name, which every
//! `QueryBuilder` in this dependency renders identically as
//! `CONCAT(text, $1)` regardless of backend
//! (`sea-query-0.32.7/src/backend/query_builder.rs`'s
//! `prepare_function_name_common` handles `Function::Custom` generically).
//! `CONCAT()` is a native `MySQL` function, Postgres has had a `concat()`
//! function (distinct from `||`) since 9.1, and this workspace's `SQLite` is
//! the bundled 3.46.0 amalgamation (`qa-runs`'s `toolkit-db` dependency pulls
//! `sqlx/sqlite`, which pulls `sqlx-sqlite/bundled`) — `SQLite` added a
//! built-in `concat()` scalar function in 3.44.0, so the bundled version
//! supports it. Confirmed by rendering the built expression against
//! `PostgresQueryBuilder`, `MysqlQueryBuilder` and `SqliteQueryBuilder`
//! directly (see the task report) rather than trusting the above by
//! inspection alone, and by `a_second_append_concatenates_rather_than_replacing`
//! below, which executes the statement against a real `SQLite` connection
//! rather than only rendering it.
//!
//! None of the three dialects' `NULL`-handling difference between `concat()`
//! and `||` matters here: `qa_run_logs.text` is `NOT NULL DEFAULT ''`
//! (`migrations::m20260831_000008_run_logs`), so this statement only ever
//! runs against a row that already holds a non-`NULL` string.
//!
//! # A scoped update first, an insert only if nothing matched
//!
//! `on_conflict` was the obvious shape and is not used, because the insert
//! half of an upsert takes no `AccessScope` — it would be an unscoped write.
//! This form keeps the concatenation in the statement *and* keeps the scope on
//! the path that touches an existing row. The insert half goes through
//! `secure_insert`, not a bare `ActiveModel::insert(runner)`: `DBRunner` gives
//! no accessible `ConnectionTrait` to call `.insert` against directly outside
//! `toolkit-db` (`runs_sea_repo::create` takes the same route for the same
//! reason), and `secure_insert` is what checks the inserted row's `tenant_id`
//! against the caller's scope.
//!
//! The two statements cannot race for one run: `flush` takes the pending
//! buffer out of the map before writing, so only one caller ever holds a given
//! run's text at a time (a later task's contract, not enforced here).
//!
//! # What actually refuses a foreign-scoped write, and what does not
//!
//! `.secure().scope_with(scope)` is what refuses the **update** half: a row
//! belonging to another tenant matches zero rows and the update is a no-op.
//!
//! **`secure_insert` is not the control on the insert half.**
//! `validate_insert_scope` (`libs/toolkit-db/src/secure/db_ops.rs`) compares
//! the `ActiveModel`'s own `tenant_id` against the caller's scope, and that
//! `tenant_id` is this function's own argument — so a caller that supplies a
//! tenant it is scoped for passes the check by construction. It still earns
//! its place: it is what refuses a caller that supplies *someone else's*
//! tenant id. What it cannot do is refuse a caller supplying its own tenant id
//! for a run it does not own.
//!
//! **The composite foreign key is what closes that.**
//! `(run_id, tenant_id) REFERENCES qa_runs(id, tenant_id)`
//! (`migrations::m20260831_000008_run_logs`, which carries the full mechanism
//! and why it was reachable nowhere in production) makes the insert fail in
//! the database when the supplied tenant is not the run's owner. Both halves
//! are pinned by `a_foreign_scoped_append_is_refused` below; deleting that
//! foreign key turns one of its two assertions red.
//!
//! # `sea-orm`'s `debug-print` feature would print a tenant's log text
//!
//! With `debug-print` enabled, `sea-orm` emits every statement it executes
//! through a `tracing::debug!` **with its bound parameters inlined**. This
//! crate's standing rule is that no log text reaches an error message, a log
//! line or a `Debug` rendering — `infra::logs::archive::Pending` and
//! `domain::repos::ArchivedLog` both hand-write `Debug` for exactly that
//! reason — and `append_log`'s `text` parameter is the **first** bound
//! parameter anywhere in this crate that carries bulk tenant log text. So that
//! feature stopped being merely a performance question the moment this file
//! landed: turning it on would publish tenants' log output into this service's
//! trace stream.
//!
//! It is off, and named in no `Cargo.toml` in this workspace — `sea-orm` is
//! depended on with an explicit feature list that does not include it, and
//! Cargo's feature unification cannot add one nothing asks for. Recorded here
//! because that is now load-bearing rather than incidental, and because the
//! next reader to reach for it while debugging a query will reach for it in
//! this file.

use std::collections::BTreeMap;

use async_trait::async_trait;
use sea_orm::sea_query::{Alias, Expr, Func};
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{ArchivedLog, LogPosition, LogResume, RunLogsRepository};
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::run_log::{self, Column as LogColumn, Entity as LogEntity};
use crate::infra::storage::runs_sea_repo::OrmRunsRepository;

#[async_trait]
impl RunLogsRepository for OrmRunsRepository {
    async fn append_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
        tenant_id: Uuid,
        text: &str,
        lines: i64,
    ) -> Result<(), DomainError> {
        // A SCOPED UPDATE FIRST, THEN AN INSERT IF THERE WAS NOTHING TO
        // UPDATE. See the module doc for why, and for why the "$1" bound
        // here is a `CONCAT` function call and not a `||` operator.
        let updated = LogEntity::update_many()
            .filter(Condition::all().add(Expr::col(LogColumn::RunId).eq(run_id)))
            .secure()
            .scope_with(scope)
            .col_expr(
                LogColumn::Text,
                Func::cust(Alias::new("CONCAT"))
                    .arg(Expr::col(LogColumn::Text))
                    .arg(Expr::val(text))
                    .into(),
            )
            .col_expr(LogColumn::Lines, Expr::col(LogColumn::Lines).add(lines))
            .col_expr(LogColumn::UpdatedAt, Expr::value(OffsetDateTime::now_utc()))
            .exec(runner)
            .await
            .map_err(db_err)?;

        if updated.rows_affected > 0 {
            return Ok(());
        }

        // First append for this run. `tenant_id` comes from the tenant-bound
        // system context the flush minted, never from a caller.
        let am = run_log::ActiveModel {
            run_id: ActiveValue::Set(run_id),
            tenant_id: ActiveValue::Set(tenant_id),
            text: ActiveValue::Set(text.to_owned()),
            lines: ActiveValue::Set(lines),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };
        secure_insert::<LogEntity>(am, scope, runner)
            .await
            .map_err(db_err)?;

        Ok(())
    }

    async fn get_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Option<ArchivedLog>, DomainError> {
        let found = LogEntity::find()
            .filter(LogColumn::RunId.eq(run_id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        Ok(found.map(|m| ArchivedLog {
            text: m.text,
            lines: m.lines,
        }))
    }

    async fn log_resume_positions<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<LogResume, DomainError> {
        let found = LogEntity::find()
            .filter(LogColumn::RunId.eq(run_id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        // No row: nothing archived yet, so an empty `LogResume` is the right
        // answer — see the trait doc's "first attach" case.
        let Some(model) = found else {
            return Ok(LogResume::default());
        };

        // Recover a per-node count from the interleaved text — see the trait
        // doc's "There is no per-node column" section for why this is
        // possible at all and what it costs. `str::lines` rather than
        // `split('\n')`: `record` terminates every line with exactly one
        // `\n` (`infra::logs::archive`'s module doc), so `split` would yield
        // one trailing empty string per call and `lines` does not.
        let mut counts: BTreeMap<&str, i64> = BTreeMap::new();
        for line in model.text.lines() {
            // Every archived line is `"[{node}] {line}"` by construction
            // (`domain::service::ingest::fan_out_log`), so the substring
            // between the first `[` and the first `]` is the node. A node
            // name containing `]` would truncate early here; unreachable
            // today (`ExecutionNode::name`'s one producer is
            // `format!("repo-{repo_id}")`) and merely mis-attributed, not
            // unsound, if it ever happened.
            if let Some(rest) = line.strip_prefix('[')
                && let Some(end) = rest.find(']')
            {
                *counts.entry(&rest[..end]).or_insert(0) += 1;
            }
        }

        Ok(counts
            .into_iter()
            .map(|(node, lines)| {
                (
                    node.to_owned(),
                    LogPosition {
                        lines,
                        // The row's own `updated_at` stands in for every
                        // node's `since_time` — see `LogPosition`'s doc for
                        // why that is the safe over-approximation rather
                        // than a shortcut.
                        since_time: Some(model.updated_at),
                    },
                )
            })
            .collect())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use toolkit_db::Db;
    use toolkit_db::secure::DbConn;

    use super::*;
    use crate::domain::repos::RunsRepository;
    use crate::infra::storage::test_db::{inmem_db, sample_new_run, scope};

    /// Shared setup for this file's tests.
    ///
    /// `runs_sea_repo.rs`'s own test module builds `db`/`conn`/`scope` inline
    /// in each test rather than through a struct like this one. A struct is
    /// used here instead because these tests also need a seeded parent
    /// `qa_runs` row before a log row's cascade foreign key can be satisfied,
    /// and that seeding is common to all three tests below.
    ///
    /// `conn` is a method rather than a stored field: `Db::conn()` borrows
    /// from `&Db` (`libs/toolkit-db/src/secure/db.rs:190`), so a field of
    /// that borrowed type living alongside an owned `db: Db` field would be
    /// self-referential. Calling `fx.conn()` per statement, as
    /// `runs_sea_repo.rs`'s tests call `db.conn().unwrap()` once per test,
    /// keeps the same non-owning shape this crate's other DB tests use.
    struct Fixture {
        db: Db,
        repo: OrmRunsRepository,
        tenant: Uuid,
        scope: AccessScope,
    }

    impl Fixture {
        fn conn(&self) -> DbConn<'_> {
            self.db.conn().unwrap()
        }

        /// A real `qa_runs` row under the fixture's tenant, so a log row has
        /// a parent to reference — `qa_run_logs.run_id` cascades from
        /// `qa_runs(id)` in every dialect.
        async fn seed_run(&self) -> Uuid {
            self.repo
                .create(
                    &self.conn(),
                    &self.scope,
                    self.tenant,
                    sample_new_run("log-fixture"),
                )
                .await
                .unwrap()
                .id
        }
    }

    async fn fixture() -> Fixture {
        let tenant = Uuid::new_v4();
        Fixture {
            db: inmem_db().await,
            repo: OrmRunsRepository,
            tenant,
            scope: scope(tenant),
        }
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Two appends concatenate. **This is the property the flush depends on**
    /// (spec §4.4): the concatenation happens in the statement, so a flush
    /// costs the new text and not the whole log.
    #[tokio::test]
    async fn a_second_append_concatenates_rather_than_replacing() {
        let fx = fixture().await;
        let run_id = fx.seed_run().await;

        fx.repo
            .append_log(&fx.conn(), &fx.scope, run_id, fx.tenant, "[a] one\n", 1)
            .await
            .unwrap();
        fx.repo
            .append_log(&fx.conn(), &fx.scope, run_id, fx.tenant, "[a] two\n", 1)
            .await
            .unwrap();

        let stored = fx
            .repo
            .get_log(&fx.conn(), &fx.scope, run_id)
            .await
            .unwrap()
            .expect("the row must exist after two appends");

        assert_eq!(stored.text, "[a] one\n[a] two\n");
        assert_eq!(stored.lines, 2, "line counts must add, not overwrite");
    }

    /// A run with no archived log reads as absent, not as an error. This is
    /// the ordinary case for every run that finished before this migration,
    /// and it is what drives the handler's fallback to the in-memory tail.
    #[tokio::test]
    async fn a_run_with_no_archived_log_reads_as_none() {
        let fx = fixture().await;
        let run_id = fx.seed_run().await;

        assert!(
            fx.repo
                .get_log(&fx.conn(), &fx.scope, run_id)
                .await
                .unwrap()
                .is_none(),
        );
    }

    /// Tenant A cannot read tenant B's archived log. The scope, not the
    /// handler, is what enforces this — so a defect in the handler cannot turn
    /// into a cross-tenant read on its own.
    #[tokio::test]
    async fn a_foreign_tenant_cannot_read_an_archived_log() {
        let fx = fixture().await;
        let run_id = fx.seed_run().await;
        fx.repo
            .append_log(&fx.conn(), &fx.scope, run_id, fx.tenant, "[a] secret\n", 1)
            .await
            .unwrap();

        let stranger = scope(uuid(999));

        assert!(
            fx.repo
                .get_log(&fx.conn(), &stranger, run_id)
                .await
                .unwrap()
                .is_none(),
            "another tenant's scope must not see this log",
        );
    }

    /// **A foreign-scoped `append_log` is refused, and refused by the
    /// database.** The whole-branch review, 2026-08-31, disproved the claim
    /// that this was already structurally enforced: the scoped `UPDATE`
    /// correctly matched zero rows for a run it did not own, control fell
    /// through to `secure_insert`, and `validate_insert_scope` compares the
    /// `ActiveModel`'s own `tenant_id` against the caller's scope — which the
    /// caller supplied. Probe output from that review:
    ///
    /// ```text
    /// PROBE1 foreign first append is_ok=true
    /// PROBE2 owner sees row=false
    /// PROBE3 stranger sees row=true
    /// PROBE4 owner legitimate append is_err=true (UNIQUE constraint failed)
    /// ```
    ///
    /// So a foreign tenant reaching a run's **first** append could create its
    /// log row under its own tenant and poison it permanently: the rightful
    /// tenant's scoped `get_log` filtered it out, and every legitimate append
    /// then failed forever on the `run_id` primary key. Unreachable in
    /// production — the only attach path reads the tenant off the run row —
    /// and closed anyway, while `qa_run_logs` is still undeployed.
    ///
    /// The two assertions cover the two distinct attempts, and they are
    /// refused by two different mechanisms:
    ///
    /// * a stranger supplying **its own** tenant id passes
    ///   `validate_insert_scope` and is refused by the composite foreign key
    ///   `(run_id, tenant_id) REFERENCES qa_runs(id, tenant_id)`;
    /// * a stranger supplying the **owner's** tenant id is refused by
    ///   `validate_insert_scope`, before any statement runs.
    ///
    /// **Break-tested**, both directions:
    /// * reverting the migration's key to `FOREIGN KEY (run_id) REFERENCES
    ///   qa_runs(id)` turns the first assertion red — the stranger's insert
    ///   succeeds — and then turns the third red too, because the owner's
    ///   legitimate append hits the poisoned row's primary key;
    /// * replacing `secure_insert`'s `scope` argument with
    ///   `&AccessScope::for_tenant(tenant_id)` — the closest reachable stand-in
    ///   for an unscoped insert, since `DBRunner` exposes no bare
    ///   `ConnectionTrait` to insert against — turns the second assertion
    ///   red.
    #[tokio::test]
    async fn a_foreign_scoped_append_is_refused() {
        let fx = fixture().await;
        let run_id = fx.seed_run().await;
        let stranger_tenant = uuid(999);
        let stranger = scope(stranger_tenant);

        assert!(
            fx.repo
                .append_log(
                    &fx.conn(),
                    &stranger,
                    run_id,
                    stranger_tenant,
                    "[a] poison\n",
                    1,
                )
                .await
                .is_err(),
            "a stranger must not be able to create this run's log row under its own \
             tenant - doing so makes the row invisible to its owner and every \
             legitimate append fail on the primary key forever",
        );

        assert!(
            fx.repo
                .append_log(&fx.conn(), &stranger, run_id, fx.tenant, "[a] poison\n", 1)
                .await
                .is_err(),
            "nor by naming the owner's tenant id while scoped to its own",
        );

        // And the owner's own path is unaffected: nothing was created, so the
        // first legitimate append still takes the insert branch.
        fx.repo
            .append_log(&fx.conn(), &fx.scope, run_id, fx.tenant, "[a] mine\n", 1)
            .await
            .expect("the rightful tenant's first append must still succeed");
        assert_eq!(
            fx.repo
                .get_log(&fx.conn(), &fx.scope, run_id)
                .await
                .unwrap()
                .expect("the owner's row exists")
                .text,
            "[a] mine\n",
        );
    }

    /// A run with nothing archived yet answers an empty [`LogResume`] — the
    /// case that must make a first attach read from the beginning rather than
    /// resume from a position that does not exist.
    #[tokio::test]
    async fn a_run_with_no_archived_log_has_no_resume_position() {
        let fx = fixture().await;
        let run_id = fx.seed_run().await;

        let resume = fx
            .repo
            .log_resume_positions(&fx.conn(), &fx.scope, run_id)
            .await
            .unwrap();

        assert_eq!(resume.lines_for("a"), 0);
        assert_eq!(resume.since_time_for("a"), None);
    }

    /// **The per-node count, recovered from one run's interleaved text.**
    /// `qa_run_logs` has no node column — see `log_resume_positions`'s own
    /// doc — so this is what proves the recovery is exact: two nodes'
    /// output, appended in two separate `append_log` calls the way two
    /// pods' drains would arrive, must be told apart and counted
    /// correctly, with neither node's count contaminating the other's.
    #[tokio::test]
    async fn resume_positions_are_counted_per_node_from_the_interleaved_text() {
        let fx = fixture().await;
        let run_id = fx.seed_run().await;

        fx.repo
            .append_log(
                &fx.conn(),
                &fx.scope,
                run_id,
                fx.tenant,
                "[a] one\n[b] uno\n",
                2,
            )
            .await
            .unwrap();
        fx.repo
            .append_log(&fx.conn(), &fx.scope, run_id, fx.tenant, "[a] two\n", 1)
            .await
            .unwrap();

        let resume = fx
            .repo
            .log_resume_positions(&fx.conn(), &fx.scope, run_id)
            .await
            .unwrap();

        assert_eq!(resume.lines_for("a"), 2, "node a has two of its own lines");
        assert_eq!(resume.lines_for("b"), 1, "node b has one, not three");
        assert!(resume.since_time_for("a").is_some());
        assert!(resume.since_time_for("b").is_some());
        assert_eq!(
            resume.since_time_for("a"),
            resume.since_time_for("b"),
            "the row has one updated_at for both nodes - see LogPosition's doc \
             for why that over-approximation is the safe direction",
        );
        assert_eq!(
            resume.lines_for("never-appeared"),
            0,
            "a node this run never emitted answers 0, not a missing-key panic",
        );
    }
}
