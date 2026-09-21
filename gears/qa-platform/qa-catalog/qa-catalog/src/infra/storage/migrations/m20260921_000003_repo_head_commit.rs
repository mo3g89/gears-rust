//! `qa_test_repositories.head_commit` — the content revision a sync
//! materialized.
//!
//! **Append-only**, per `migrations::mod`'s own header: a new column added by
//! its own migration, never an edit to `m20260812_000002_initial`.
//!
//! ## What this is for
//!
//! `infra::git::gix_sync`'s `sync_blocking` has always computed the checked-out
//! branch head (`materialize_branch`'s return) and carried it out on
//! `domain::ports::repo_sync::SyncResult::head_commit`, where nothing read it
//! — the field carried a `dead_code` allowance naming the integration test as
//! its only consumer. This column is where it lands, which makes it the
//! repository's real content revision rather than a value the sync throws
//! away, and surfaces it on the API as a side effect.
//!
//! The reason it is wanted is `domain::service::plans`' discovery cache, which
//! was keyed on `last_synced_at` — a *timestamp*, which advances on every
//! successful sync whether or not the content moved. Keyed on the revision,
//! a re-sync that finds nothing new no longer throws the filesystem walk away.
//!
//! ## Nullable, and it stays nullable
//!
//! `NULL` means "synced before this column existed, or never synced". It is
//! not backfilled, because nothing in the database knows what revision an
//! existing working copy is at — only a fresh sync does, and the next one
//! writes it. A `NOT NULL DEFAULT ''` would have invented an answer, and an
//! empty-string revision is one every un-backfilled row would share, which is
//! exactly the false cache-hit the key exists to avoid.
//!
//! `NULL` is safe as a cache key for the same reason it is honest: content
//! only changes when a sync runs, and a sync always writes a non-`NULL`
//! revision. So a `NULL` key is stable precisely as long as the content it
//! describes is.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_test_repositories ADD COLUMN IF NOT EXISTS head_commit VARCHAR(64);
";

/// `SQLite` has no `IF NOT EXISTS` on `ADD COLUMN`; the migration runner
/// applies each migration exactly once, so the bare form is the same
/// statement in practice.
const SQLITE_UP: &str = r"
ALTER TABLE qa_test_repositories ADD COLUMN head_commit VARCHAR(64);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
            other => {
                return Err(DbErr::Migration(format!(
                    "unsupported database backend: {other:?}"
                )));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("ALTER TABLE qa_test_repositories DROP COLUMN head_commit;")
            .await?;
        Ok(())
    }
}

/// Schema tests for this migration alone — see `m20260812_000002_initial`'s
/// own test module doc for why these exist (`cargo build` proves nothing
/// about a column name that only a query round-trips) and why they run
/// against `SQLite` only.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "a schema test needs a raw connection and SecureORM deliberately exposes none \
              -- see m20260812_000002_initial's own test module doc for the full argument, \
              which applies identically here"
)]
mod tests {
    use sea_orm::{ActiveModelTrait, ActiveValue, Database, EntityTrait};
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::{product, test_repository};
    use crate::infra::storage::migrations::Migrator;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_789_689_600).unwrap()
    }

    async fn migrated_db() -> sea_orm::DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("failed to connect to in-memory sqlite database");
        Migrator::up(&conn, None)
            .await
            .expect("failed to run qa-catalog migrations");
        conn
    }

    async fn seed_product(conn: &sea_orm::DatabaseConnection, tenant: Uuid) -> Uuid {
        let id = Uuid::from_u128(900);
        product::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant),
            name: ActiveValue::Set("vhp".to_owned()),
            product_key: ActiveValue::Set("vhp".to_owned()),
            description: ActiveValue::Set(String::new()),
            folder: ActiveValue::Set(None),
            plugin_instance_id: ActiveValue::Set(
                "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1"
                    .to_owned(),
            ),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();
        id
    }

    fn repo_am(
        id: Uuid,
        tenant: Uuid,
        product_id: Uuid,
        head_commit: Option<String>,
    ) -> test_repository::ActiveModel {
        test_repository::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant),
            product_id: ActiveValue::Set(product_id),
            name: ActiveValue::Set("tests".to_owned()),
            url: ActiveValue::Set("https://example.invalid/t.git".to_owned()),
            default_branch: ActiveValue::Set("main".to_owned()),
            content_root: ActiveValue::Set(String::new()),
            credential_ref: ActiveValue::Set(None),
            last_synced_at: ActiveValue::Set(Some(now())),
            head_commit: ActiveValue::Set(head_commit),
            sync_error: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    /// The column exists, is writable, and reads back — the only thing that
    /// actually exercises the name in both the DDL and `entity::test_repository`.
    #[tokio::test]
    async fn the_head_commit_round_trips() {
        let conn = migrated_db().await;
        let tenant = Uuid::from_u128(1);
        let product_id = seed_product(&conn, tenant).await;
        let id = Uuid::from_u128(2);
        let sha = "0123456789abcdef0123456789abcdef01234567";
        repo_am(id, tenant, product_id, Some(sha.to_owned()))
            .insert(&conn)
            .await
            .unwrap();

        let stored = test_repository::Entity::find_by_id(id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the repository must be readable");
        assert_eq!(stored.head_commit.as_deref(), Some(sha));
    }

    /// `NULL` is a real state and not a write error: a repository registered
    /// but never synced has no revision, and this migration invents none.
    #[tokio::test]
    async fn a_repository_with_no_revision_stores_null() {
        let conn = migrated_db().await;
        let tenant = Uuid::from_u128(1);
        let product_id = seed_product(&conn, tenant).await;
        let id = Uuid::from_u128(3);
        repo_am(id, tenant, product_id, None)
            .insert(&conn)
            .await
            .unwrap();

        let stored = test_repository::Entity::find_by_id(id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the repository must be readable");
        assert_eq!(stored.head_commit, None, "an unsynced repo has no revision");
    }
}
