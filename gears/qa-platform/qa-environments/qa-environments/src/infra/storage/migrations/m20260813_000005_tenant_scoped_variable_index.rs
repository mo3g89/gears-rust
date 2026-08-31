//! Rebuilds `qa_platform_variables`' unique index with the tenant leading.
//!
//! **Security fix, 2026-08-13.** The shipped index is
//! `idx_qa_platform_vars_unique (platform_id, name)`
//! (`m20260812_000001_initial.rs:38`, `:89`, `:145` — all three dialects), on a
//! table that carries `tenant_id` (`:31`, `:83`, `:137`). That is tenant-blind, and it is the
//! live instance of the one rule this subsystem's post-mortem singled out:
//! DESIGN §3.7's "Every unique index is tenant-prefixed", "Every unique index is tenant-prefixed — including on child
//! tables."
//!
//! Two concrete harms, both reachable by a caller who merely *knows* another
//! tenant's `platform_id` — and resource UUIDs are identifiers, not secrets:
//!
//! * **Squatting denial of service.** Insert a variable of your own tenant
//!   naming the victim's `platform_id` and a name the victim will want. The
//!   insert passes tenant validation, is invisible to both tenants' scoped
//!   reads, and permanently blocks the victim from creating that variable.
//! * **Existence oracle.** The unique-violation response discriminates whether
//!   the victim already has a variable of that name on that platform.
//!
//! Prefixing `tenant_id` confines such a row to the squatter's own key space,
//! where it is harmless junk.
//!
//! ## Widening a unique index can never fail on existing data
//!
//! `(platform_id, name)` unique **implies** `(tenant_id, platform_id, name)`
//! unique — adding a leading column can only split equivalence classes, never
//! merge them. So no deployed database can hold a row that violates the new
//! index, and this migration needs no data cleanup or conflict handling. The
//! reverse direction is the dangerous one, which is why `down()` below can
//! fail and says so.
//!
//! ## Why three statements, and why the extra `platform_id` index
//!
//! The FK `platform_id REFERENCES qa_platforms(id)` needs a supporting index
//! whose **leading** column is `platform_id`, and on `InnoDB` dropping the last
//! such index fails outright (errno 150). The shipped unique index happens to
//! be that index; the replacement is not, because `tenant_id` now leads. So the
//! order is: add the FK-supporting index, add the new unique index, only then
//! drop the old one.
//!
//! `idx_qa_platform_vars_platform` is created in **all three** dialects even
//! though only `MySQL` strictly requires it. Postgres and `SQLite` get a
//! cascade-delete lookup out of it, and — the reason that decides it — the
//! three blobs stay a line-for-line eyeball diff, which the initial migration's
//! own header calls the only thing that catches a forgotten dialect.
//!
//! ## The FK stays tenant-blind, and that is a documented residual
//!
//! Nothing here constrains `platform_id` to a platform of the *same* tenant, so
//! a cross-tenant *reference* remains insertable, and the FK's own response
//! still discriminates whether a given `platform_id` exists in **any** tenant.
//! The index fix removes the collision and the uniqueness oracle; it does not
//! remove the FK oracle. Closing that needs either a composite
//! `(tenant_id, platform_id)` FK — which would require a redundant
//! `UNIQUE (tenant_id, id)` on `qa_platforms` — or, the route this subsystem
//! takes, a **tenant-scoped ownership precheck in the service**: resolve the
//! platform under the caller's scope and return not-found before attempting the
//! insert, so the FK never gets to answer. The same residual and the same
//! obligation are recorded on qa-runs' two foreign keys.
//!
//! ## `MySQL` key width
//!
//! `(tenant_id, platform_id, name)` is 36*4 + 36*4 + 255*4 = 1308 bytes under
//! `utf8mb4`, inside `InnoDB`'s 3072-byte limit.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE INDEX IF NOT EXISTS idx_qa_platform_vars_platform ON qa_platform_variables(platform_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platform_vars_tenant_unique
    ON qa_platform_variables(tenant_id, platform_id, name);
DROP INDEX IF EXISTS idx_qa_platform_vars_unique;
";

const MYSQL_UP: &str = r"
-- ORDER IS LOAD-BEARING ON MYSQL, and nothing tests it: CI runs SQLite only,
-- where all three statements would succeed in any order. InnoDB refuses to drop
-- the last index whose leading column backs a foreign key (errno 150), and
-- `idx_qa_platform_vars_unique (platform_id, name)` is currently that index.
-- The replacement leads with `tenant_id`, so it cannot take over the role.
-- Add the FK-supporting index FIRST, then the new unique index, and only then
-- drop the old one. See this module's header.
ALTER TABLE qa_platform_variables ADD KEY idx_qa_platform_vars_platform (platform_id);
ALTER TABLE qa_platform_variables ADD UNIQUE KEY idx_qa_platform_vars_tenant_unique (tenant_id, platform_id, name);
ALTER TABLE qa_platform_variables DROP INDEX idx_qa_platform_vars_unique;
";

const SQLITE_UP: &str = r"
CREATE INDEX IF NOT EXISTS idx_qa_platform_vars_platform ON qa_platform_variables(platform_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platform_vars_tenant_unique
    ON qa_platform_variables(tenant_id, platform_id, name);
DROP INDEX IF EXISTS idx_qa_platform_vars_unique;
";

const POSTGRES_DOWN: &str = r"
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platform_vars_unique
    ON qa_platform_variables(platform_id, name);
DROP INDEX IF EXISTS idx_qa_platform_vars_tenant_unique;
DROP INDEX IF EXISTS idx_qa_platform_vars_platform;
";

const MYSQL_DOWN: &str = r"
-- Mirror of MYSQL_UP's ordering constraint, for the same untested reason:
-- restore the `(platform_id, name)` index FIRST so that something with
-- `platform_id` leading always backs the foreign key, and only then drop the
-- two this migration added. Dropping `idx_qa_platform_vars_platform` before
-- re-adding the old unique key would leave the FK momentarily unbacked and
-- fail.
ALTER TABLE qa_platform_variables ADD UNIQUE KEY idx_qa_platform_vars_unique (platform_id, name);
ALTER TABLE qa_platform_variables DROP INDEX idx_qa_platform_vars_tenant_unique;
ALTER TABLE qa_platform_variables DROP INDEX idx_qa_platform_vars_platform;
";

const SQLITE_DOWN: &str = r"
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platform_vars_unique
    ON qa_platform_variables(platform_id, name);
DROP INDEX IF EXISTS idx_qa_platform_vars_tenant_unique;
DROP INDEX IF EXISTS idx_qa_platform_vars_platform;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::MySql => MYSQL_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
        };
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }

    /// Narrows the index back to `(platform_id, name)`.
    ///
    /// **This direction can genuinely fail**, unlike `up()`: if any two tenants
    /// have created a variable of the same name against the same platform while
    /// the wide index was in force — which is exactly what the fix permits —
    /// the narrow index cannot be built and the rollback errors. That is the
    /// correct behaviour: silently deleting one tenant's row to make a rollback
    /// succeed would be worse than a failed rollback.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_DOWN,
            sea_orm::DatabaseBackend::MySql => MYSQL_DOWN,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN,
        };
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }
}

/// Schema tests for the tenant-scoped index.
///
/// Written as the property the index buys rather than as its column list, so
/// they fail for the reason that matters: dropping `tenant_id` from the index
/// turns `a_platform_variable_is_unique_per_tenant_not_globally` red. Verified
/// by doing exactly that.
///
/// `clippy::disallowed_methods` is allowed for the narrow reason a schema test
/// always needs it: `secure_insert` requires an `AccessScope`, which a
/// migration-module test has no business constructing, and the `SecureORM`
/// wrappers expose no raw connection. Nothing here is a production path.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{ActiveModelTrait, ActiveValue, ConnectOptions, Database, DatabaseConnection};
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::{platform, platform_variable};

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
    }

    async fn migrated_db() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        let conn = Database::connect(opts)
            .await
            .expect("failed to connect to in-memory sqlite database");
        let manager = SchemaManager::new(&conn);
        for migration in super::super::Migrator::migrations() {
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-environments migrations");
        }
        conn
    }

    async fn seed_platform(conn: &DatabaseConnection, id: Uuid, tenant: Uuid, name: &str) {
        platform::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant),
            name: ActiveValue::Set(name.to_owned()),
            product_id: ActiveValue::Set(None),
            description: ActiveValue::Set(None),
            kubeconfig_credstore_ref: ActiveValue::Set("credstore://ref".to_owned()),
            available: ActiveValue::Set(true),
            observed_version: ActiveValue::Set(None),
            observed_build: ActiveValue::Set(None),
            default_branch: ActiveValue::Set(None),
            is_default: ActiveValue::Set(false),
            vhp_base_url: ActiveValue::Set(None),
            observed_namespace: ActiveValue::Set(None),
            version_detect_error: ActiveValue::Set(None),
            version_detected_at: ActiveValue::Set(None),
            cluster_status: ActiveValue::Set(None),
            cluster_status_message: ActiveValue::Set(None),
            cluster_nodes: ActiveValue::Set(None),
            cluster_namespace_count: ActiveValue::Set(None),
            cluster_checked_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();
    }

    fn var_am(
        id: Uuid,
        tenant: Uuid,
        platform_id: Uuid,
        name: &str,
    ) -> platform_variable::ActiveModel {
        platform_variable::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant),
            platform_id: ActiveValue::Set(platform_id),
            name: ActiveValue::Set(name.to_owned()),
            value: ActiveValue::Set("v".to_owned()),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    /// The security property, stated as the attack it defeats.
    ///
    /// Before this migration the squatter's insert failed with a unique
    /// violation — which both blocked the victim permanently and reported
    /// whether the victim's row existed. It must now succeed and be confined to
    /// the squatter's own key space.
    #[tokio::test]
    async fn a_platform_variable_is_unique_per_tenant_not_globally() {
        let conn = migrated_db().await;
        let victim = uuid(1);
        let squatter = uuid(2);
        let platform_id = uuid(10);
        seed_platform(&conn, platform_id, victim, "staging-a").await;

        var_am(uuid(20), victim, platform_id, "API_TOKEN")
            .insert(&conn)
            .await
            .unwrap();
        var_am(uuid(21), squatter, platform_id, "API_TOKEN")
            .insert(&conn)
            .await
            .expect(
                "a second tenant naming the same (platform_id, name) must be accepted - \
                 under a tenant-blind index, knowing a platform_id was enough to squat \
                 every variable name on it and to learn which ones the victim had",
            );

        // ...and the index still does its job inside a tenant.
        var_am(uuid(22), victim, platform_id, "API_TOKEN")
            .insert(&conn)
            .await
            .expect_err("one tenant may not define the same variable name twice on a platform");
    }

    /// Different names on the same platform, same tenant, must still coexist —
    /// otherwise the index would be over-tightened into `(tenant_id,
    /// platform_id)` and this test is what would notice.
    #[tokio::test]
    async fn distinct_names_on_one_platform_coexist() {
        let conn = migrated_db().await;
        let tenant = uuid(1);
        let platform_id = uuid(10);
        seed_platform(&conn, platform_id, tenant, "staging-a").await;

        var_am(uuid(20), tenant, platform_id, "API_TOKEN")
            .insert(&conn)
            .await
            .unwrap();
        var_am(uuid(21), tenant, platform_id, "API_URL")
            .insert(&conn)
            .await
            .expect("two different variable names on one platform must coexist");
    }

    /// And the same name on *different* platforms of one tenant, which would
    /// break if the index were narrowed to `(tenant_id, name)`.
    #[tokio::test]
    async fn one_name_on_two_platforms_of_the_same_tenant_coexists() {
        let conn = migrated_db().await;
        let tenant = uuid(1);
        seed_platform(&conn, uuid(10), tenant, "staging-a").await;
        seed_platform(&conn, uuid(11), tenant, "staging-b").await;

        var_am(uuid(20), tenant, uuid(10), "API_TOKEN")
            .insert(&conn)
            .await
            .unwrap();
        var_am(uuid(21), tenant, uuid(11), "API_TOKEN")
            .insert(&conn)
            .await
            .expect("one variable name may exist on each of a tenant's platforms");
    }

    /// The old index has to actually be gone, not merely superseded: leaving it
    /// in place would preserve every harm this migration exists to remove,
    /// while all three tests above still passed.
    #[tokio::test]
    async fn the_tenant_blind_index_is_dropped() {
        let conn = migrated_db().await;
        let indexes = sqlite_index_names(&conn, "qa_platform_variables").await;
        assert!(
            !indexes.contains(&"idx_qa_platform_vars_unique".to_owned()),
            "the tenant-blind index must be dropped, found: {indexes:?}"
        );
        assert!(
            indexes.contains(&"idx_qa_platform_vars_tenant_unique".to_owned()),
            "the tenant-scoped index must exist, found: {indexes:?}"
        );
        assert!(
            indexes.contains(&"idx_qa_platform_vars_platform".to_owned()),
            "the FK-supporting index must exist, found: {indexes:?}"
        );
    }

    async fn sqlite_index_names(conn: &DatabaseConnection, table: &str) -> Vec<String> {
        use sea_orm::{ConnectionTrait, Statement};
        conn.query_all(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='{table}'"),
        ))
        .await
        .unwrap()
        .iter()
        .filter_map(|row| row.try_get::<String>("", "name").ok())
        .collect()
    }
}
