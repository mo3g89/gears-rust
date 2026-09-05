//! `SecureORM` implementation of [`CollectRepository`].

use async_trait::async_trait;
use qa_insights_sdk::CollectCount;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, Condition, EntityTrait};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureEntityExt, SecureInsertExt, SecureOnConflict, validate_tenant_in_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::CollectRepository;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::test_case_collect::{
    self, Column as CollectColumn, Entity as CollectEntity,
};
use crate::infra::storage::mapper::{
    MAX_NAME, MAX_PATH, collect_count_to_sdk, db_i32_from_u32, truncate,
};

/// ORM-based implementation of the `CollectRepository` trait.
#[derive(Clone, Default)]
pub struct OrmCollectRepository;

#[async_trait]
impl CollectRepository for OrmCollectRepository {
    /// A real upsert against `idx_qa_test_case_collect_target`, not a
    /// read-then-write.
    ///
    /// Legacy is one statement — `INSERT ... ON CONFLICT (repo_id, branch,
    /// test_file) DO UPDATE SET case_count = EXCLUDED.case_count, collected_at =
    /// EXCLUDED.collected_at` (`manager/src/routes/analytics.rs:2623-2628`) — and
    /// this is too. A probe-then-insert pair would have a race window in which
    /// two collect jobs for the same target both see no row, and one of the two
    /// inserts would then fail on the unique index; the `ON CONFLICT` makes the
    /// second a replacement instead.
    ///
    /// The conflict target is the index's own columns, `tenant_id` first: without
    /// it one tenant's collect job would overwrite another's count for the same
    /// `(repo_id, branch, test_file)`.
    ///
    /// `collected_at` is the caller's — when the collect job produced the count —
    /// and `updated_at` is this write's. Both are updated; `created_at` is not,
    /// because the row is the same row.
    async fn upsert_count<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        count: CollectCount,
    ) -> Result<(), DomainError> {
        // `scope_unchecked` below performs no validation, so the tenant check
        // has to be explicit — otherwise `tenant_id` would be a free parameter
        // and a caller could overwrite another tenant's count.
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;

        let now = OffsetDateTime::now_utc();
        let am = test_case_collect::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            repo_id: ActiveValue::Set(count.repo_id),
            // Producer text, truncated to the column, for the reason `mapper`'s
            // header gives about the ingest tables: the collect job's `test_file`
            // comes from `pytest --collect-only` with parametrize expanded, so it
            // is exactly as unconstrained as `qa_test_results.test_file`. On
            // Postgres an over-long path is `22001`, i.e. a silently lost collect
            // count on a path nobody is watching — the failure that header argues
            // against. Truncating shifts the natural key, so the count lands under
            // a *neighbouring* path rather than under none.
            branch: ActiveValue::Set(truncate(
                count.branch,
                MAX_NAME,
                "qa_test_case_collect.branch",
            )),
            test_file: ActiveValue::Set(truncate(
                count.test_file,
                MAX_PATH,
                "qa_test_case_collect.test_file",
            )),
            case_count: ActiveValue::Set(db_i32_from_u32(count.case_count, "case_count")?),
            collected_at: ActiveValue::Set(count.collected_at),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        let on_conflict = SecureOnConflict::<CollectEntity>::columns([
            CollectColumn::TenantId,
            CollectColumn::RepoId,
            CollectColumn::Branch,
            CollectColumn::TestFile,
        ])
        .update_columns([
            CollectColumn::CaseCount,
            CollectColumn::CollectedAt,
            CollectColumn::UpdatedAt,
        ])
        .map_err(db_err)?;

        CollectEntity::insert(am)
            .secure()
            .scope_unchecked(scope)
            .map_err(db_err)?
            .on_conflict(on_conflict)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// One statement over `idx_qa_test_case_collect_tenant_repo_branch`, reading
    /// exactly the rows the caller will look up.
    ///
    /// An empty `repo_ids` short-circuits to no rows **before** any statement is
    /// built, because "no repositories in scope" must never become "every
    /// repository".
    ///
    /// **Defensive rather than load-bearing on this dependency version, and that
    /// was measured rather than assumed.** Removing this guard leaves
    /// `an_empty_repository_slice_reads_nothing_rather_than_everything` green:
    /// `sea-query` 0.32 already renders an empty `is_in` as a false condition
    /// rather than the invalid `IN ()`. The guard stays because the property is a
    /// read-widening one and its correctness would otherwise rest on an
    /// undocumented builder behaviour that a version bump could change — but it
    /// is recorded here that no test currently distinguishes the two, so nothing
    /// would go red if a future edit removed it.
    async fn list_counts_for<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        repo_ids: &[Uuid],
        branch: &str,
    ) -> Result<Vec<CollectCount>, DomainError> {
        if repo_ids.is_empty() {
            return Ok(Vec::new());
        }

        let rows = CollectEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(Expr::col(CollectColumn::Branch).eq(branch))
                    .add(Expr::col(CollectColumn::RepoId).is_in(repo_ids.iter().copied())),
            )
            .all(runner)
            .await
            .map_err(db_err)?;
        rows.into_iter().map(collect_count_to_sdk).collect()
    }
}

#[cfg(test)]
mod tests {
    use qa_insights_sdk::CollectCount;
    use uuid::Uuid;

    use crate::domain::repos::CollectRepository;
    use crate::infra::storage::collect_sea_repo::OrmCollectRepository;
    use crate::infra::storage::test_db::{inmem_db, now, scope};

    fn count(repo_id: Uuid, branch: &str, file: &str, cases: u32) -> CollectCount {
        CollectCount {
            repo_id,
            branch: branch.to_owned(),
            test_file: file.to_owned(),
            case_count: cases,
            collected_at: now(),
        }
    }

    /// **The count is replaced, not appended.**
    ///
    /// Legacy's statement is `INSERT ... ON CONFLICT (repo_id, branch, test_file)
    /// DO UPDATE SET case_count = EXCLUDED.case_count, collected_at =
    /// EXCLUDED.collected_at` (`manager/src/routes/analytics.rs:2623-2628`), and
    /// the platform's mandatory `id UUID` demoted that composite key to
    /// `idx_qa_test_case_collect_target`. An implementation that inserts with a
    /// fresh `id` instead of targeting the index duplicates the count rather than
    /// replacing it, and every "expected cases" number the analytics surface
    /// shows is then inflated with nothing failing.
    #[tokio::test]
    async fn recollecting_a_target_replaces_its_count_rather_than_duplicating_it() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let repo = Uuid::from_u128(0x12);
        let ctx = scope(tenant);

        OrmCollectRepository
            .upsert_count(&conn, &ctx, tenant, count(repo, "main", "t/a.py", 7))
            .await
            .unwrap();
        OrmCollectRepository
            .upsert_count(&conn, &ctx, tenant, count(repo, "main", "t/a.py", 9))
            .await
            .unwrap();

        let rows = OrmCollectRepository
            .list_counts_for(&conn, &ctx, &[repo], "main")
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "the target must hold one row: {rows:?}");
        assert_eq!(
            rows[0].case_count, 9,
            "and it must be the newer count, not the older one"
        );
    }

    /// **Collect's producer text is truncated to fit its columns.**
    ///
    /// `test_file` and `branch` come from `pytest --collect-only` with parametrize
    /// expanded, so they are as unconstrained as `qa_test_results.test_file`. On
    /// Postgres an over-long value is `22001` — a silently lost collect count on a
    /// path nobody is watching, which is the failure `mapper`'s header argues
    /// against. `SQLite` has type affinity rather than width, so this tier cannot
    /// see the rejection; what it can see is that the stored value was shortened.
    ///
    /// Added 2026-08-20: the truncation itself was added in the same pass, after
    /// review found collect fell through *both* halves of `mapper`'s stated rule —
    /// it is neither one of the two result tables nor operator input.
    #[tokio::test]
    async fn over_long_collect_paths_are_truncated_to_fit_their_columns() {
        use crate::infra::storage::mapper::{MAX_NAME, MAX_PATH};

        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let repo = Uuid::from_u128(0x12);
        let ctx = scope(tenant);

        let long_file = "t/".to_owned() + &"a".repeat(MAX_PATH + 50);
        let long_branch = "b".repeat(MAX_NAME + 50);
        OrmCollectRepository
            .upsert_count(
                &conn,
                &ctx,
                tenant,
                count(repo, &long_branch, &long_file, 7),
            )
            .await
            .unwrap();

        let rows = OrmCollectRepository
            .list_counts_for(&conn, &ctx, &[repo], &long_branch[..MAX_NAME])
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the row must be findable under the truncated branch, which is what \
             was actually stored: {rows:?}"
        );
        assert_eq!(rows[0].test_file.chars().count(), MAX_PATH);
        assert_eq!(rows[0].branch.chars().count(), MAX_NAME);
        assert_eq!(rows[0].case_count, 7, "and the count itself is intact");
    }

    /// The read takes a slice of repositories and one branch, so a universe
    /// spanning many repositories is one statement rather than N — legacy reads a
    /// branch's counts in one (`analytics.rs:2684`).
    #[tokio::test]
    async fn counts_are_read_for_several_repositories_in_one_call() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let (a, b, c) = (
            Uuid::from_u128(0x12),
            Uuid::from_u128(0x13),
            Uuid::from_u128(0x14),
        );

        for repo in [a, b, c] {
            OrmCollectRepository
                .upsert_count(&conn, &ctx, tenant, count(repo, "main", "t/a.py", 3))
                .await
                .unwrap();
        }
        OrmCollectRepository
            .upsert_count(&conn, &ctx, tenant, count(a, "release/5.0", "t/a.py", 4))
            .await
            .unwrap();

        let rows = OrmCollectRepository
            .list_counts_for(&conn, &ctx, &[a, b], "main")
            .await
            .unwrap();
        let mut repos = rows.iter().map(|r| r.repo_id).collect::<Vec<_>>();
        repos.sort();
        assert_eq!(
            repos,
            vec![a, b],
            "only the named repositories, and only the named branch"
        );
    }

    /// An empty `repo_ids` is "no repositories in scope", never "every
    /// repository".
    ///
    /// The distinction is the whole safety of the slice signature: a caller with
    /// an empty universe has nothing to look up, and answering with every
    /// repository's counts would be the widest read this trait can perform.
    #[tokio::test]
    async fn an_empty_repository_slice_reads_nothing_rather_than_everything() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        OrmCollectRepository
            .upsert_count(
                &conn,
                &ctx,
                tenant,
                count(Uuid::from_u128(0x12), "main", "t/a.py", 3),
            )
            .await
            .unwrap();

        assert!(
            OrmCollectRepository
                .list_counts_for(&conn, &ctx, &[], "main")
                .await
                .unwrap()
                .is_empty(),
            "an empty slice must read nothing"
        );
    }

    /// One tenant's count is invisible to another, even for the same
    /// `(repo_id, branch, test_file)` — and both may store one, because
    /// `idx_qa_test_case_collect_target` is tenant-prefixed.
    #[tokio::test]
    async fn a_collect_target_is_per_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);
        let repo = Uuid::from_u128(0x12);

        OrmCollectRepository
            .upsert_count(&conn, &scope(mine), mine, count(repo, "main", "t/a.py", 3))
            .await
            .unwrap();
        OrmCollectRepository
            .upsert_count(
                &conn,
                &scope(theirs),
                theirs,
                count(repo, "main", "t/a.py", 5),
            )
            .await
            .expect("another tenant may store the same target");

        let mine_rows = OrmCollectRepository
            .list_counts_for(&conn, &scope(mine), &[repo], "main")
            .await
            .unwrap();
        assert_eq!(mine_rows.len(), 1);
        assert_eq!(
            mine_rows[0].case_count, 3,
            "and neither overwrote the other"
        );
    }
}
