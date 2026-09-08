//! Adds `qa_products.plugin_instance_id`, the binding from a product to the
//! product plugin that owns its behaviour, and backfills every existing row
//! to the VHP plugin.
//!
//! This is the **expand** half of an expand/contract pair
//! (`PRODUCT-PLUGINS-DESIGN.md` §11 step 5). The column is nullable here and
//! is tightened to `NOT NULL` by a later migration, once every writer sets
//! it. Nullable-then-tighten is what keeps this step revertible: a `down()`
//! that drops a nullable column leaves a schema every reader that predates
//! this migration still understands, and no row has to be invented to satisfy
//! a constraint mid-rollback.
//!
//! ## What the column stores: the **full** GTS instance id
//!
//! Not the instance *segment*. A plugin registers its `ClientHub` entry under
//! `ClientScope::gts_id(&instance_id)` where `instance_id` comes from
//! `PluginV1::<QaProductPluginSpecV1>::build_registration(INSTANCE_SEGMENT, …)`,
//! and that helper composes `GtsInstanceId::new(<P as GtsSchema>::TYPE_ID,
//! instance_segment)` — the spec's type id with the segment appended. The
//! resolver hands the stored value to `ClientScope::gts_id` **unchanged**, so
//! the stored value has to be that whole composition. The segment alone
//! resolves to nothing at all, silently: `try_get_scoped` returns `None`,
//! which reads as "no such plugin" rather than as "wrong id".
//!
//! [`VHP_PLUGIN_INSTANCE_ID`] is therefore pinned by
//! [`tests::the_backfilled_id_is_the_spec_type_id_plus_the_vhp_segment`],
//! which recomposes it from `QaProductPluginSpecV1`'s own `TYPE_ID` and the
//! segment rather than trusting the literal. A change to either half breaks
//! that test instead of orphaning every product in every deployed database —
//! and an orphaned product is invisible until someone tries to observe an
//! environment or launch a run, which is a long way from the change that
//! caused it.
//!
//! ## Why a VHP-specific literal is legitimate in a product-agnostic gear
//!
//! `qa-catalog` knows nothing else about VHP and must not. What this
//! migration writes is not behaviour, it is **data**: the one-time answer to
//! "which plugin did the rows that existed before plugins were a concept
//! belong to", and there is exactly one possible answer — before Phase C
//! there was one product implementation and it was VHP's. A data backfill
//! naming the plugin those rows already implicitly used is not a dependency
//! on that plugin; nothing in this crate's code paths reads the constant.
//!
//! ## Width: `VARCHAR(512)` on the servers, `TEXT` on `SQLite`
//!
//! Same shape and the same reasoning as qa-environments'
//! `m20260814_000006_platform_default_branch`. The value is a GTS instance id
//! — the VHP one is 88 characters — and 512 leaves room for a vendor's own
//! nesting without being unbounded. Nothing indexes this column, so there is
//! no `MySQL` key-width budget to spend: the widths here are about what a
//! well-formed id can be, not about an index.
//!
//! ## Why the backfill is one statement for all three dialects
//!
//! The `ALTER` differs by dialect (the type spellings above), so it keeps the
//! three-blob shape this subsystem's migrations use, where a three-way eyeball
//! diff is what catches a forgotten dialect. The `UPDATE` does not differ at
//! all, and duplicating it three times would create the very drift the
//! three-blob shape exists to expose. It is built once, from
//! [`VHP_PLUGIN_INSTANCE_ID`], so no dialect can be left out of the backfill
//! and no copy of the id can drift from another.
//!
//! Interpolating the id into SQL is safe here and only here: it is a
//! compile-time constant of this crate, not input.
//!
//! ## No `IF NOT EXISTS` on the `ALTER`
//!
//! For the reason qa-environments' `000004`, `000006` and `000009` all record:
//! `SQLite` rejects `ALTER TABLE … ADD COLUMN IF NOT EXISTS` outright,
//! `MySQL`'s grammar has no such clause, and the migration runner's
//! apply-once guarantee is the one that actually holds.
//!
//! ## A second migration, not an edit to the initial one
//!
//! `m20260812_000002_initial` must be assumed to have run wherever this gear
//! is deployed, and the runner records it as applied — so editing it would
//! change nothing on an existing database while diverging from what that
//! database contains. The numbering (`000003`) continues **this gear's** own
//! sequence after its `000002`, which is the numbering the product-plugins
//! plan assigns (its Task 20 follows with `000004` here while qa-environments
//! continues at `000011`). Note that the qa-platform subsystem's earlier
//! migrations describe a single sequence shared across its gears, in which
//! `000003` is qa-runs' initial; the names are still unique — the runner keys
//! on the whole name — but the number no longer orders across gears.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The full GTS instance id of the VHP product plugin: the id
/// `QaProductPluginSpecV1`'s `TYPE_ID` composes with
/// `qa_vhp_product_plugin::gear::INSTANCE_SEGMENT`.
///
/// Every row that exists when this migration runs is backfilled to it. See
/// the module docs for why the literal lives here and
/// [`tests::the_backfilled_id_is_the_spec_type_id_plus_the_vhp_segment`] for
/// what stops it from drifting.
const VHP_PLUGIN_INSTANCE_ID: &str =
    "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1";

const POSTGRES_UP: &str = r"
ALTER TABLE qa_products ADD COLUMN plugin_instance_id VARCHAR(512) NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_products ADD COLUMN plugin_instance_id VARCHAR(512) NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE qa_products ADD COLUMN plugin_instance_id TEXT NULL;
";

/// Pick the `ALTER` for a backend.
///
/// Extracted from `up()` so it can be tested, for the reason
/// `m20260814_000006_platform_default_branch::sql_for` records: this gear's
/// tests only ever run `SQLite`, so swapping two adjacent, near-identical
/// match arms leaves every test green while handing the server dialects the
/// wrong statement.
fn alter_for(backend: sea_orm::DatabaseBackend) -> Result<&'static str, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(POSTGRES_UP),
        sea_orm::DatabaseBackend::MySql => Ok(MYSQL_UP),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_UP),
        other => Err(DbErr::Migration(format!(
            "unsupported database backend: {other:?}"
        ))),
    }
}

/// The one-time backfill, identical on every dialect.
///
/// `WHERE plugin_instance_id IS NULL` rather than an unconditional `SET`:
/// the migration runner applies this once, but a re-run (a restored
/// database, a hand-run `down()` then `up()`) must not overwrite a binding an
/// operator has since changed to another plugin.
fn backfill_sql() -> String {
    format!(
        "UPDATE qa_products SET plugin_instance_id = '{VHP_PLUGIN_INSTANCE_ID}' \
         WHERE plugin_instance_id IS NULL;"
    )
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(alter_for(manager.get_database_backend())?)
            .await?;
        conn.execute_unprepared(&backfill_sql()).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("ALTER TABLE qa_products DROP COLUMN plugin_instance_id;")
            .await?;
        Ok(())
    }
}

/// Schema and backfill tests.
///
/// `cargo build` proves nothing about a `SeaORM` entity — its table and
/// column names are runtime strings — so only a query shows the column
/// exists.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use gts::GtsSchema;
    use qa_product_sdk::QaProductPluginSpecV1;
    use sea_orm::{
        ConnectOptions, ConnectionTrait, Database, DatabaseConnection, EntityTrait, Statement,
    };
    use sea_orm_migration::{MigrationName, MigrationTrait, MigratorTrait, SchemaManager};
    use uuid::Uuid;

    use super::VHP_PLUGIN_INSTANCE_ID;
    use crate::infra::storage::entity::product;
    use crate::infra::storage::mapper::product_to_sdk;

    /// `qa_vhp_product_plugin::gear::INSTANCE_SEGMENT`, restated rather than
    /// imported: `qa-catalog` is product-agnostic and does not depend on a
    /// plugin crate (nor could it — the plugin depends on nothing here, but a
    /// dependency in this direction would put VHP in every catalog build).
    /// This is the half of the id that is VHP's; the other half is derived
    /// below from the spec type this crate legitimately knows.
    const VHP_INSTANCE_SEGMENT: &str = "cf.core._.vhp_product.v1";

    /// **The load-bearing test of this migration.**
    ///
    /// The backfilled literal is only correct if it equals what
    /// `PluginV1::<QaProductPluginSpecV1>::build_registration` composes, and
    /// that is `GtsInstanceId::new(TYPE_ID, segment)` — a plain
    /// concatenation. Recomposing it here from the spec's own `TYPE_ID` means
    /// a change to the spec's type id fails this test rather than silently
    /// orphaning every product row in every deployed database.
    #[test]
    fn the_backfilled_id_is_the_spec_type_id_plus_the_vhp_segment() {
        let composed = format!(
            "{}{VHP_INSTANCE_SEGMENT}",
            <QaProductPluginSpecV1 as GtsSchema>::TYPE_ID
        );

        assert_eq!(
            VHP_PLUGIN_INSTANCE_ID, composed,
            "the backfill literal must equal QaProductPluginSpecV1's TYPE_ID with VHP's \
             instance segment appended -- that is what the plugin registers its ClientHub \
             scope under, and a stored id that differs resolves to nothing at all"
        );
        assert!(
            super::backfill_sql().contains(&composed),
            "and the statement that runs must carry that id: {}",
            super::backfill_sql()
        );
    }

    /// The composed id is a well-formed GTS *instance* id, not a type id.
    ///
    /// `ClientScope::gts_id` accepts any string, so a trailing `~` (the
    /// spelling of a type id) would key the hub on an id no plugin can ever
    /// register under, and nothing else would notice.
    #[test]
    fn the_backfilled_id_is_a_well_formed_instance_id() {
        gts::GtsInstanceId::try_new(VHP_PLUGIN_INSTANCE_ID)
            .expect("the backfilled id must parse as a GTS instance id");
    }

    fn without_comments(sql: &str) -> String {
        sql.lines()
            .map(|line| line.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// This gear's other tables. Altering a table that *exists* succeeds and
    /// fails only later, when the column turns out not to be on
    /// `qa_products`.
    const OTHER_TABLES: [&str; 4] = [
        "qa_test_repositories",
        "qa_repo_branches",
        "qa_ssh_keys",
        "qa_custom_plans",
    ];

    #[test]
    fn every_dialect_blob_adds_a_nullable_column_to_qa_products() {
        for (dialect, raw) in [
            ("POSTGRES_UP", super::POSTGRES_UP),
            ("MYSQL_UP", super::MYSQL_UP),
            ("SQLITE_UP", super::SQLITE_UP),
        ] {
            let sql = without_comments(raw);

            assert!(
                sql.contains("qa_products"),
                "{dialect} must alter qa_products; this file's shape is copied from another \
                 migration, so the table name is what a copy-paste gets wrong"
            );
            for other in OTHER_TABLES {
                assert!(!sql.contains(other), "{dialect} must not touch {other}");
            }
            assert!(
                sql.contains("ADD COLUMN plugin_instance_id"),
                "{dialect} must add plugin_instance_id; a rewrite that patched only the \
                 first blob is how two dialects drift apart unnoticed"
            );
            assert!(
                sql.contains("NULL") && !sql.contains("NOT NULL"),
                "{dialect} must declare the column NULLable: this is the expand half of an \
                 expand/contract pair and a NOT NULL here would make it irreversible"
            );
        }
    }

    /// M7's shape: swapping the `Postgres` and `Sqlite` arms leaves every
    /// test green, because every executed test runs on `SQLite`.
    #[test]
    fn each_backend_gets_the_statement_with_its_own_type_spelling() {
        use sea_orm::DatabaseBackend;

        for backend in [DatabaseBackend::Postgres, DatabaseBackend::MySql] {
            let sql = without_comments(super::alter_for(backend).expect("dispatch covers every backend this build compiles"));
            assert!(
                sql.contains("VARCHAR(512)"),
                "{backend:?} must get the bounded server type"
            );
        }

        let sqlite = without_comments(super::alter_for(DatabaseBackend::Sqlite).expect("dispatch covers every backend this build compiles"));
        assert!(
            sqlite.contains("TEXT"),
            "SQLite must get its own TEXT statement"
        );
        assert!(
            !sqlite.contains("VARCHAR"),
            "and must not get a server blob: the arms are adjacent and similar, which is \
             what makes swapping them a plausible edit"
        );
    }

    /// The backfill must not clobber a binding that is already set.
    ///
    /// Asserted on the statement text rather than on data, because there is
    /// no way to produce the interesting row: every row that exists before
    /// this migration runs predates the column, so none of them can carry a
    /// value to preserve. The `IS NULL` guard is for a *re-run* (a restored
    /// database, a hand-run `down()` then `up()`), which the runner's
    /// apply-once guarantee otherwise hides.
    #[test]
    fn the_backfill_only_touches_rows_that_name_no_plugin() {
        let sql = backfill();
        assert!(
            sql.contains("WHERE plugin_instance_id IS NULL"),
            "the backfill must be guarded, or a re-run overwrites an operator's own \
             binding: {sql}"
        );
    }

    fn backfill() -> String {
        super::backfill_sql()
    }

    async fn connect() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        Database::connect(opts)
            .await
            .expect("failed to connect to in-memory sqlite database")
    }

    /// Every migration of this gear **up to but excluding this one**, so a
    /// pre-migration row can be planted and the backfill observed.
    ///
    /// Keyed on this migration's own name rather than on "all but the last":
    /// Task 20 adds a `000004` after this one, and a positional cut would
    /// then quietly start testing the wrong migration.
    async fn db_migrated_up_to_this_one() -> DatabaseConnection {
        let conn = connect().await;
        let manager = SchemaManager::new(&conn);
        let this = MigrationName::name(&super::Migration);
        for migration in super::super::Migrator::migrations() {
            if MigrationName::name(&*migration) == this {
                return conn;
            }
            migration
                .up(&manager)
                .await
                .expect("failed to run a qa-catalog migration");
        }
        panic!("this migration is not registered in the Migrator");
    }

    async fn fully_migrated_db() -> DatabaseConnection {
        let conn = connect().await;
        let manager = SchemaManager::new(&conn);
        for migration in super::super::Migrator::migrations() {
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-catalog migrations");
        }
        conn
    }

    /// Insert a product row through raw SQL, naming only the columns that
    /// exist before this migration runs.
    ///
    /// Raw SQL on both sides of this test (insert and read-back), rather than
    /// the `SeaORM` entity: the entity carries the new column, so it cannot
    /// write a pre-migration row at all, and going through raw text for both
    /// halves keeps the test independent of how the driver encodes a `Uuid`
    /// or a timestamp on `SQLite`.
    async fn insert_legacy_product(conn: &DatabaseConnection, name: &str) {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "INSERT INTO qa_products \
                 (id, tenant_id, name, product_key, description, folder, created_at, updated_at) \
                 VALUES ('{}', '{}', '{name}', '{}', '', NULL, \
                 '2026-09-03 00:00:00+00:00', '2026-09-03 00:00:00+00:00');",
                Uuid::from_u128(1),
                Uuid::from_u128(7),
                name.to_uppercase()
            ),
        ))
        .await
        .expect("failed to insert the pre-migration product row");
    }

    /// The point of the whole migration: a product that existed before
    /// plugins were a concept comes out bound to the VHP plugin.
    #[tokio::test]
    async fn an_existing_product_is_backfilled_to_the_vhp_plugin() {
        let conn = db_migrated_up_to_this_one().await;
        insert_legacy_product(&conn, "vhp").await;

        MigrationTrait::up(&super::Migration, &SchemaManager::new(&conn))
            .await
            .expect("the migration must apply to a populated table");

        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT plugin_instance_id, name FROM qa_products WHERE name = 'vhp';",
            ))
            .await
            .unwrap()
            .expect("the product row must survive the ALTER");
        assert_eq!(
            row.try_get::<Option<String>>("", "plugin_instance_id")
                .unwrap()
                .as_deref(),
            Some(VHP_PLUGIN_INSTANCE_ID),
            "every pre-existing product must come out bound to the VHP plugin"
        );
        assert_eq!(
            row.try_get::<String>("", "name").unwrap(),
            "vhp",
            "and the row's other columns must be untouched"
        );
    }

    /// The binding has to reach the **SDK model**, which is what the resolver
    /// and every consumer outside this gear read. Dropping it in
    /// `mapper::product_to_sdk` leaves the row itself perfectly correct.
    ///
    /// The fixture writes a *non-VHP* id on purpose: the backfilled value is
    /// also what a mapper hard-coding the constant would produce.
    #[tokio::test]
    async fn the_binding_round_trips_and_reaches_the_sdk_model() {
        use sea_orm::{ActiveModelTrait, ActiveValue};
        use time::OffsetDateTime;

        let conn = fully_migrated_db().await;
        let id = Uuid::from_u128(2);
        let other_plugin =
            "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~acme._.other.v1";
        let now = OffsetDateTime::from_unix_timestamp(1_788_000_000).unwrap();

        product::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(Uuid::from_u128(7)),
            name: ActiveValue::Set("other".to_owned()),
            product_key: ActiveValue::Set("OTHER".to_owned()),
            description: ActiveValue::Set(String::new()),
            folder: ActiveValue::Set(None),
            plugin_instance_id: ActiveValue::Set(other_plugin.to_owned()),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        }
        .insert(&conn)
        .await
        .unwrap();

        let stored = product::Entity::find_by_id(id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the product row must read back");
        assert_eq!(stored.plugin_instance_id, other_plugin);
        assert_eq!(
            product_to_sdk(stored).plugin_instance_id,
            other_plugin,
            "the binding must reach the SDK model, which is what the resolver reads"
        );
    }

    #[tokio::test]
    async fn the_down_migration_drops_the_column() {
        let conn = fully_migrated_db().await;
        let manager = SchemaManager::new(&conn);

        MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        conn.execute_unprepared("SELECT plugin_instance_id FROM qa_products")
            .await
            .expect_err("plugin_instance_id must be gone after down()");
        conn.execute_unprepared("SELECT product_key FROM qa_products")
            .await
            .expect("down() must not touch the columns the initial migration declared");
    }
}
