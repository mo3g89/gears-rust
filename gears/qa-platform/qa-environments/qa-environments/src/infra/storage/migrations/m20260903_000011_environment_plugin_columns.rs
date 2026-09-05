//! Adds the seven plugin-shaped columns an observation writes once a product
//! plugin, rather than this gear's own Kubernetes observer, produces it:
//! `credentials`, `observed_attrs`, `config`, `observed_base_url`,
//! `health_state`, `health_detail` and `health_checked_at`.
//!
//! **Added 2026-09-04, product-plugins plan Task 14 (spec §6.2).** This is the
//! *expand* half of an expand/contract pair: every legacy column stays, keeps
//! its value, and keeps its writer, so a build without the plugin path reads
//! exactly what it read before. Task 19 is the contract half and drops
//! `kubeconfig_credstore_ref`, `vhp_base_url`, `observed_namespace` and the
//! five `cluster_*` columns.
//!
//! Nothing writes these columns yet — Task 15 replaces the observer call
//! inside the observation cycle and becomes their writer, dual-writing the
//! legacy columns in the same `record_observation`. What this migration
//! guarantees is that the plugin path has somewhere to write *and* that a
//! deployment upgraded today already carries the legacy facts in the new
//! shape, so the two halves can be compared before the old ones are dropped.
//!
//! ## The seven columns, and which of them the plan did not name
//!
//! | column | type | why |
//! | ------ | ---- | --- |
//! | `credentials` | JSON array, `NOT NULL DEFAULT '[]'` | `[{"key":…,"credstore_ref":…}]` — the credstore references a plugin's `CredentialSlot`s are built from. **There is no `value` field, ever**: the column is structurally incapable of holding plaintext, which is the point after the 2026-08-28 leak. |
//! | `observed_attrs` | JSON object, `NOT NULL DEFAULT '{}'` | the plugin's opaque observed map, `qa_product_sdk::observation::ObservedAttrs` verbatim. |
//! | `config` | JSON object, `NOT NULL DEFAULT '{}'` | operator-set, non-secret: `EnvironmentHandle`'s third channel. |
//! | `observed_base_url` | `TEXT NULL` | the `FieldRole::BaseUrl` projection — `vhp_base_url` with the product's name taken out of it. |
//! | `health_state` | `TEXT NOT NULL DEFAULT 'unknown'` | `qa_product_sdk::observation::HealthState`'s wire form. |
//! | `health_detail` | `TEXT NULL` | classified text only, never a formatted error (**D12**). |
//! | `health_checked_at` | timestamp, `NULL` | `NULL` means *nothing ever looked*, which is a different fact from `health_state = 'unknown'` after a failed look. |
//!
//! `observed_version` and `observed_build` already exist and are reused
//! unchanged: Task 15 writes them from the `Version`/`Build` role projections,
//! so no schema change is needed for either.
//!
//! **`config` is the plan's own list plus one (ruling D-9).** The plan named
//! six columns, and with only those six Task 15 cannot construct an
//! `EnvironmentHandle` at all: its `config` channel is where a plugin reads
//! its non-secret credential fields, `qa-vhp-product-plugin` reads
//! `vpadm_namespace` out of exactly that channel (`observe.rs`'
//! `vpadm_namespace`, keyed by `schemas::VPADM_NAMESPACE_KEY`), and no column
//! held that value in plugin shape. `CredentialClassification::config` exists
//! in the SDK precisely to route such a field there.
//!
//! ## The backfills, and the one product-specific literal in them
//!
//! * `credentials` ← `[{"key":"kubeconfig","credstore_ref":<kubeconfig_credstore_ref>}]`.
//! * `observed_base_url` ← `vhp_base_url`, verbatim, `NULL` included.
//! * `health_state`/`health_detail`/`health_checked_at` ← the `cluster_*`
//!   columns, mapped as the table below.
//! * `config` ← `{"vpadm_namespace": <trimmed VPADM_NAMESPACE variable>}` for
//!   the environments that have such a variable, and `'{}'` for every other.
//!
//! ### Two notes on the `config` backfill
//!
//! **It is tenant-scoped as well as environment-scoped.** The correlated
//! subquery matches `v.tenant_id = qa_environments.tenant_id` on top of
//! `v.platform_id = qa_environments.id`. The unique index
//! `idx_qa_environment_vars_tenant_unique` already makes a
//! (tenant, environment, name) triple unique, so the extra predicate changes
//! nothing for well-formed data — it is there so that a row whose `tenant_id`
//! disagrees with its environment's cannot have its value copied into another
//! tenant's `config`, which is the same reasoning
//! `m20260813_000005_tenant_scoped_variable_index` applied to the index
//! itself.
//!
//! **SQL `trim` and Rust `trim` are not the same function, and here that is
//! inert.** All three engines' `TRIM`/`trim` strip spaces only, while
//! `str::trim` strips all Unicode whitespace — so a variable whose value is a
//! single tab is "blank" to `pick_vpadm_namespace` but not to this backfill,
//! which would store `{"vpadm_namespace": "\t"}`. Nothing goes wrong at read
//! time: `qa-vhp-product-plugin`'s `observe::vpadm_namespace` applies
//! `str::trim` and the same non-empty filter to whatever it finds in `config`,
//! so such a value falls back to `virtuozzo` exactly as the variable did.
//! Widening the SQL to match `str::trim` would mean naming every whitespace
//! codepoint in three dialects to fix a case the reader already handles.
//!
//! Two of those carry a literal from one product's plugin — the credential
//! key `"kubeconfig"` (`qa-vhp-product-plugin`'s `schemas::KUBECONFIG_KEY`)
//! and the config key `"vpadm_namespace"` (`schemas::VPADM_NAMESPACE_KEY`).
//! **A one-time data backfill is the one place that is legitimate**, and it is
//! deliberately not done anywhere else: hardcoding `"vpadm_namespace"` inside
//! this gear's runtime would put a VHP literal in the crate whose entire
//! purpose is to stop naming VHP, and generalising the
//! `VPADM_NAMESPACE` → `vpadm_namespace` case-fold into a rule would be a
//! landmine for the next product, whose variables are spelled differently.
//! `qa-environments` cannot depend on a product plugin to borrow the constants
//! (that edge points the other way), so the strings are written out and cited.
//!
//! ### `cluster_status` → `health_state`
//!
//! | `cluster_status` | `health_state` | why |
//! | ---------------- | -------------- | --- |
//! | `Healthy` | `ok` | |
//! | `Degraded` | `degraded` | |
//! | `Warning` | `degraded` | the mapping Phase C already chose for this pair (`qa-vhp-product-plugin`'s health read). |
//! | `Unhealthy` | `down` | something looked and found the target unhealthy — `HealthState::Down`'s own definition. |
//! | `Unreachable` | `unknown` | the read *failed*, so nothing is known about the target. `Down` would assert a verdict no cluster gave. |
//! | `NULL` | `unknown` | never checked. |
//!
//! `Unreachable` and never-checked therefore land on the same `health_state`,
//! and that is not a lost distinction: `health_checked_at` keeps it, being
//! backfilled from `cluster_checked_at` and left `NULL` for a row that was
//! never checked. This is the same split Task 15 must write —
//! `HealthOutcome::NotAttempted` writes no health column at all, while a
//! `HealthOutcome::Failed` writes a state, a detail and a check time — so the
//! backfilled rows and the plugin-written ones agree on what `unknown` means.
//!
//! `health_detail` takes `cluster_status_message`, which is `NULL` for every
//! status except `Unreachable` and carries text already classified by
//! `infra::runner_secret_errors` rather than a formatted `kube::Error` (D-CH-5).
//! So the backfill cannot move an unclassified string into the new column.
//!
//! ## Two blobs per dialect: schema, then backfill
//!
//! Every other migration here is one `execute_unprepared` blob per dialect.
//! This one is two ([`schema_for`] and [`backfill_for`]), for a testing reason
//! that is worth the extra constant: `migrated_db()` applies every migration
//! to an *empty* table, so a backfill inside the single blob would run over
//! zero rows and no test could observe it. Split, a test can populate a row
//! whose new columns are at their defaults — byte for byte the state this
//! migration finds — and run the backfill half against it. The
//! `up()`-really-runs-it half is covered separately, by a test that stops the
//! migrator one migration short, inserts a legacy row and then applies this
//! migration whole.
//!
//! ## Why raw SQL per backend rather than `Table::alter()`
//!
//! The convention of this directory, and here it is load-bearing rather than
//! stylistic: the three dialects disagree about JSON defaults.
//!
//! * **Postgres** takes `JSONB NOT NULL DEFAULT '[]'` directly.
//! * **`MySQL`** rejects a *literal* default on a `JSON` column and accepts
//!   only the expression form, `DEFAULT ('[]')` (`MySQL` >= 8.0.13;
//!   `MariaDB` >= 10.2.1, where `JSON` is `LONGTEXT` plus a `json_valid`
//!   check). Written as an expression it is valid on both.
//! * **`SQLite`** has no JSON type at all, so the three JSON columns are
//!   `TEXT` and the JSON round-trips through them as text — the same
//!   accommodation `m20260828_000008_platform_cluster_health` makes for
//!   `cluster_nodes` and `m20260828_000007_platform_observation` for
//!   `version_detected_at`.
//!
//! The backfills diverge too: each engine builds the JSON with its own
//! constructor (`jsonb_build_object`, `JSON_OBJECT`, `json_object`) rather
//! than by string concatenation, so a credstore reference containing a quote
//! cannot produce a malformed document.
//!
//! ## JSON normalisation, and what "byte for byte" can mean here
//!
//! Only `SQLite` stores the bytes it was given: Postgres' `jsonb` and
//! `MySQL`'s `JSON` both normalise on write — key order and whitespace are
//! theirs, not ours. So a cross-engine byte-identity claim would be false, and
//! the property the tests actually pin is the one that matters:
//! `serde_json::to_string` of the Rust type that reads this column produces
//! exactly what `SQLITE_BACKFILL` writes, field names and all. Deserialisation
//! is insensitive to order and whitespace, so agreeing on the field *names* is
//! what makes all three engines readable by the same code.
//!
//! ## No `IF NOT EXISTS` on the `ALTER`
//!
//! For the reason every earlier migration in this directory records: `SQLite`
//! rejects `ALTER TABLE … ADD COLUMN IF NOT EXISTS` outright and `MySQL`'s
//! grammar has no such clause, so all three dialect statements are bare and
//! the migration runner's apply-once guarantee is the actual guard.
//!
//! ## A partial failure, per engine
//!
//! `MySQL` DDL auto-commits per statement, so — exactly as
//! `m20260903_000010_rename_platform_tables` records for its own blob — a
//! failure part-way through leaves the added columns in place with the
//! backfill unapplied, and re-running fails on the first `ALTER`. The manual
//! recovery is to issue only [`backfill_for`]'s statements and let the runner
//! record the migration. Postgres runs a multi-statement
//! `execute_unprepared` as one implicit transaction, so it is all-or-nothing
//! per blob.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_SCHEMA: &str = r"
ALTER TABLE qa_environments ADD COLUMN credentials JSONB NOT NULL DEFAULT '[]';
ALTER TABLE qa_environments ADD COLUMN observed_attrs JSONB NOT NULL DEFAULT '{}';
ALTER TABLE qa_environments ADD COLUMN config JSONB NOT NULL DEFAULT '{}';
ALTER TABLE qa_environments ADD COLUMN observed_base_url TEXT NULL;
ALTER TABLE qa_environments ADD COLUMN health_state TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE qa_environments ADD COLUMN health_detail TEXT NULL;
ALTER TABLE qa_environments ADD COLUMN health_checked_at TIMESTAMPTZ NULL;
";

const MYSQL_SCHEMA: &str = r"
ALTER TABLE qa_environments ADD COLUMN credentials JSON NOT NULL DEFAULT ('[]');
ALTER TABLE qa_environments ADD COLUMN observed_attrs JSON NOT NULL DEFAULT ('{}');
ALTER TABLE qa_environments ADD COLUMN config JSON NOT NULL DEFAULT ('{}');
ALTER TABLE qa_environments ADD COLUMN observed_base_url TEXT NULL;
ALTER TABLE qa_environments ADD COLUMN health_state TEXT NOT NULL DEFAULT ('unknown');
ALTER TABLE qa_environments ADD COLUMN health_detail TEXT NULL;
ALTER TABLE qa_environments ADD COLUMN health_checked_at TIMESTAMP NULL;
";

const SQLITE_SCHEMA: &str = r"
ALTER TABLE qa_environments ADD COLUMN credentials TEXT NOT NULL DEFAULT '[]';
ALTER TABLE qa_environments ADD COLUMN observed_attrs TEXT NOT NULL DEFAULT '{}';
ALTER TABLE qa_environments ADD COLUMN config TEXT NOT NULL DEFAULT '{}';
ALTER TABLE qa_environments ADD COLUMN observed_base_url TEXT NULL;
ALTER TABLE qa_environments ADD COLUMN health_state TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE qa_environments ADD COLUMN health_detail TEXT NULL;
ALTER TABLE qa_environments ADD COLUMN health_checked_at TEXT NULL;
";

const POSTGRES_BACKFILL: &str = r"
UPDATE qa_environments
   SET credentials = jsonb_build_array(
           jsonb_build_object('key', 'kubeconfig', 'credstore_ref', kubeconfig_credstore_ref)
       )
 WHERE credentials = '[]'::jsonb;

UPDATE qa_environments
   SET observed_base_url = vhp_base_url
 WHERE observed_base_url IS NULL;

UPDATE qa_environments
   SET health_state = CASE cluster_status
                          WHEN 'Healthy' THEN 'ok'
                          WHEN 'Degraded' THEN 'degraded'
                          WHEN 'Warning' THEN 'degraded'
                          WHEN 'Unhealthy' THEN 'down'
                          ELSE 'unknown'
                      END,
       health_detail = cluster_status_message,
       health_checked_at = cluster_checked_at
 WHERE cluster_status IS NOT NULL
   AND health_state = 'unknown'
   AND health_checked_at IS NULL;

UPDATE qa_environments
   SET config = (
           SELECT jsonb_build_object('vpadm_namespace', trim(v.value))
             FROM qa_environment_variables v
            WHERE v.platform_id = qa_environments.id
              AND v.tenant_id = qa_environments.tenant_id
              AND v.name = 'VPADM_NAMESPACE'
              AND trim(v.value) <> ''
            LIMIT 1
       )
 WHERE EXISTS (
           SELECT 1
             FROM qa_environment_variables v
            WHERE v.platform_id = qa_environments.id
              AND v.tenant_id = qa_environments.tenant_id
              AND v.name = 'VPADM_NAMESPACE'
              AND trim(v.value) <> ''
       )
   AND config = '{}'::jsonb;
";

const MYSQL_BACKFILL: &str = r"
UPDATE qa_environments
   SET credentials = JSON_ARRAY(
           JSON_OBJECT('key', 'kubeconfig', 'credstore_ref', kubeconfig_credstore_ref)
       )
 WHERE JSON_LENGTH(credentials) = 0;

UPDATE qa_environments
   SET observed_base_url = vhp_base_url
 WHERE observed_base_url IS NULL;

UPDATE qa_environments
   SET health_state = CASE cluster_status
                          WHEN 'Healthy' THEN 'ok'
                          WHEN 'Degraded' THEN 'degraded'
                          WHEN 'Warning' THEN 'degraded'
                          WHEN 'Unhealthy' THEN 'down'
                          ELSE 'unknown'
                      END,
       health_detail = cluster_status_message,
       health_checked_at = cluster_checked_at
 WHERE cluster_status IS NOT NULL
   AND health_state = 'unknown'
   AND health_checked_at IS NULL;

UPDATE qa_environments
   SET config = (
           SELECT JSON_OBJECT('vpadm_namespace', TRIM(v.value))
             FROM qa_environment_variables v
            WHERE v.platform_id = qa_environments.id
              AND v.tenant_id = qa_environments.tenant_id
              AND v.name = 'VPADM_NAMESPACE'
              AND TRIM(v.value) <> ''
            LIMIT 1
       )
 WHERE EXISTS (
           SELECT 1
             FROM qa_environment_variables v
            WHERE v.platform_id = qa_environments.id
              AND v.tenant_id = qa_environments.tenant_id
              AND v.name = 'VPADM_NAMESPACE'
              AND TRIM(v.value) <> ''
       )
   AND JSON_LENGTH(config) = 0;
";

const SQLITE_BACKFILL: &str = r"
UPDATE qa_environments
   SET credentials = json_array(
           json_object('key', 'kubeconfig', 'credstore_ref', kubeconfig_credstore_ref)
       )
 WHERE json_array_length(credentials) = 0;

UPDATE qa_environments
   SET observed_base_url = vhp_base_url
 WHERE observed_base_url IS NULL;

UPDATE qa_environments
   SET health_state = CASE cluster_status
                          WHEN 'Healthy' THEN 'ok'
                          WHEN 'Degraded' THEN 'degraded'
                          WHEN 'Warning' THEN 'degraded'
                          WHEN 'Unhealthy' THEN 'down'
                          ELSE 'unknown'
                      END,
       health_detail = cluster_status_message,
       health_checked_at = cluster_checked_at
 WHERE cluster_status IS NOT NULL
   AND health_state = 'unknown'
   AND health_checked_at IS NULL;

UPDATE qa_environments
   SET config = (
           SELECT json_object('vpadm_namespace', trim(v.value))
             FROM qa_environment_variables v
            WHERE v.platform_id = qa_environments.id
              AND v.tenant_id = qa_environments.tenant_id
              AND v.name = 'VPADM_NAMESPACE'
              AND trim(v.value) <> ''
            LIMIT 1
       )
 WHERE EXISTS (
           SELECT 1
             FROM qa_environment_variables v
            WHERE v.platform_id = qa_environments.id
              AND v.tenant_id = qa_environments.tenant_id
              AND v.name = 'VPADM_NAMESPACE'
              AND trim(v.value) <> ''
       )
   AND config = '{}';
";

/// Pick the `ADD COLUMN` blob for a backend.
///
/// Extracted from `up()` so it can be tested directly, following
/// `m20260828_000007_platform_observation::sql_for` — inline in the `match`,
/// nothing could reach it, and swapping two adjacent arms is exactly the
/// mistake that guard was added to catch there.
const fn schema_for(backend: sea_orm::DatabaseBackend) -> &'static str {
    match backend {
        sea_orm::DatabaseBackend::Postgres => POSTGRES_SCHEMA,
        sea_orm::DatabaseBackend::MySql => MYSQL_SCHEMA,
        sea_orm::DatabaseBackend::Sqlite => SQLITE_SCHEMA,
    }
}

/// Pick the backfill blob for a backend. See [`schema_for`] for why these are
/// separate constants, and the module doc for why they are separate blobs.
const fn backfill_for(backend: sea_orm::DatabaseBackend) -> &'static str {
    match backend {
        sea_orm::DatabaseBackend::Postgres => POSTGRES_BACKFILL,
        sea_orm::DatabaseBackend::MySql => MYSQL_BACKFILL,
        sea_orm::DatabaseBackend::Sqlite => SQLITE_BACKFILL,
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = manager.get_database_backend();
        conn.execute_unprepared(schema_for(backend)).await?;
        conn.execute_unprepared(backfill_for(backend)).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE qa_environments DROP COLUMN credentials;
             ALTER TABLE qa_environments DROP COLUMN observed_attrs;
             ALTER TABLE qa_environments DROP COLUMN config;
             ALTER TABLE qa_environments DROP COLUMN observed_base_url;
             ALTER TABLE qa_environments DROP COLUMN health_state;
             ALTER TABLE qa_environments DROP COLUMN health_detail;
             ALTER TABLE qa_environments DROP COLUMN health_checked_at;",
        )
        .await?;
        Ok(())
    }
}

/// Schema and backfill tests for the seven added columns.
///
/// **`cargo build` proves nothing about a `SeaORM` entity**: its table and
/// column names are runtime strings, so `environment::Model::config`
/// compiling says nothing about whether a column of that name exists. Only a
/// query does.
///
/// Every fixture writes **non-default** values for all seven columns before
/// asserting a round trip, per this subsystem's Task 9b lesson: a fixture that
/// only ever writes the default cannot catch a value silently dropped on the
/// way out.
///
/// `clippy::disallowed_methods` is allowed for the same narrow reason as in
/// the migrations this one follows: a schema test needs a raw connection, and
/// the `SecureORM` wrappers deliberately expose none. Nothing here is a
/// production path.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{
        ActiveModelTrait, ActiveValue, ConnectOptions, ConnectionTrait, Database, DatabaseBackend,
        DatabaseConnection, EntityTrait, Statement,
    };
    use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::{environment, environment_variable};
    use crate::infra::storage::mapper::StoredEnvironmentCredential;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
    }

    async fn connect() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        Database::connect(opts)
            .await
            .expect("failed to connect to in-memory sqlite database")
    }

    /// In-memory `SQLite` with **every** registered migration applied in
    /// order, which is also what proves `Migrator::migrations()` lists the new
    /// one: an unregistered migration leaves the columns absent and every
    /// assertion below fails.
    async fn migrated_db() -> DatabaseConnection {
        let conn = connect().await;
        let manager = SchemaManager::new(&conn);
        // **Every migration except the contract one.** Task 19's
        // `m20260903_000012` drops eight columns, so a test that ran the whole
        // list would assert against a schema its own subject no longer has --
        // while stopping at *this* migration would cut off the later
        // `m20260903_000010` rename these tests' table names depend on. Stop
        // immediately before the drop, which is the last schema state in which
        // the legacy columns and the modern names coexist.
        for migration in super::super::Migrator::migrations() {
            if sea_orm_migration::MigrationName::name(&*migration)
                == "m20260903_000012_drop_legacy_platform_columns"
            {
                break;
            }
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-environments migrations");
        }
        conn
    }

    /// Every registered migration **except this one**, so a row can be
    /// inserted in the shape this migration finds in a real deployment.
    ///
    /// Stops on the migration name rather than on a count, so inserting
    /// another migration between `000010` and this one cannot silently turn
    /// this into "all migrations".
    async fn db_one_migration_short() -> DatabaseConnection {
        let conn = connect().await;
        let manager = SchemaManager::new(&conn);
        let this = sea_orm_migration::MigrationName::name(&super::Migration);
        let mut applied_any = false;
        for migration in super::super::Migrator::migrations() {
            if migration.name() == this {
                assert!(
                    applied_any,
                    "this migration must not be the first one registered"
                );
                return conn;
            }
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-environments migrations");
            applied_any = true;
        }
        panic!("this migration is not registered in Migrator::migrations()");
    }

    /// A row with every legacy observation column populated and every column
    /// this migration adds left at its default — byte for byte the state the
    /// backfill finds.
    /// The legacy half of a pre-`000011` row, planted with raw SQL.
    ///
    /// **Not the entity.** Task 19's `m20260903_000012` dropped
    /// `kubeconfig_credstore_ref`, `vhp_base_url` and the five `cluster_*`
    /// columns, and `environment::Model` lost the fields in the same commit --
    /// but this migration's whole subject is backfilling *from* those columns,
    /// so the fixture has to be able to set them. Reads still go through the
    /// entity: every column this migration ADDS survives Task 19.
    ///
    /// `plugin_columns` lets a test override the plugin-shaped half; omit a
    /// column to let the schema's own `DEFAULT` apply, which is what
    /// `the_four_not_null_columns_apply_their_declared_defaults` measures.
    async fn plant_legacy(conn: &DatabaseConnection, plugin_columns: &[(&str, String)]) {
        use super::super::legacy_row::{text, uuid_lit};
        // The very instant the assertions compare against, formatted the way
        // `SQLite` stores a timestamp -- a fixture that wrote "some plausible
        // date" would make every `checked_at` assertion below meaningless.
        let stamp = now()
            .format(&time::format_description::well_known::Rfc3339)
            .expect("the fixture timestamp must format");
        let stamp = stamp.as_str();
        let mut columns: Vec<(&str, String)> = vec![
            ("id", uuid_lit(uuid(1))),
            ("tenant_id", uuid_lit(uuid(2))),
            ("name", text("staging-a")),
            ("product_id", uuid_lit(uuid(3))),
            ("description", text("desc")),
            ("kubeconfig_credstore_ref", text("credstore://kc/staging-a")),
            ("available", "1".to_owned()),
            ("observed_version", text("26.5")),
            ("observed_build", text("1471")),
            ("is_default", "0".to_owned()),
            ("version_detected_at", text(stamp)),
            ("vhp_base_url", text("https://sv.jele.io")),
            ("cluster_status", text("Healthy")),
            ("cluster_status_message", text("classified text")),
            ("cluster_checked_at", text(stamp)),
            ("created_at", text(stamp)),
            ("updated_at", text(stamp)),
        ];
        // Overrides REPLACE by name rather than appending: naming a column
        // twice in one INSERT is a syntax error, and a fixture that silently
        // ignored an override would make the test that passed it meaningless.
        for (name, value) in plugin_columns {
            match columns.iter_mut().find(|(existing, _)| existing == name) {
                Some(slot) => slot.1 = value.clone(),
                None => columns.push((*name, value.clone())),
            }
        }
        super::super::legacy_row::insert(conn, "qa_environments", &columns).await;
    }

    /// A SECOND environment, so a test can prove one row's override does not
    /// reach another's.
    async fn plant_other(conn: &DatabaseConnection) {
        use super::super::legacy_row::{insert, text, uuid_lit};
        const STAMP: &str = "2026-09-03 12:00:00+00:00";
        insert(
            conn,
            "qa_environments",
            &[
                ("id", uuid_lit(uuid(99))),
                ("tenant_id", uuid_lit(uuid(2))),
                ("name", text("staging-b")),
                ("product_id", uuid_lit(uuid(3))),
                ("kubeconfig_credstore_ref", text("credstore://kc/staging-b")),
                ("available", "1".to_owned()),
                ("is_default", "0".to_owned()),
                ("created_at", text(STAMP)),
                ("updated_at", text(STAMP)),
            ],
        )
        .await;
    }

    fn vpadm_namespace_variable(value: &str) -> environment_variable::ActiveModel {
        environment_variable::ActiveModel {
            id: ActiveValue::Set(uuid(10)),
            tenant_id: ActiveValue::Set(uuid(2)),
            environment_id: ActiveValue::Set(uuid(1)),
            name: ActiveValue::Set("VPADM_NAMESPACE".to_owned()),
            value: ActiveValue::Set(value.to_owned()),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    /// Run the backfill half against rows that are already there. See the
    /// module doc: `migrated_db()` migrates an empty table, so this is the
    /// only way a test can watch the backfill do anything.
    async fn run_backfill(conn: &DatabaseConnection) {
        conn.execute_unprepared(super::backfill_for(DatabaseBackend::Sqlite))
            .await
            .expect("the sqlite backfill must apply");
    }

    async fn reload(conn: &DatabaseConnection) -> environment::Model {
        environment::Entity::find_by_id(uuid(1))
            .one(conn)
            .await
            .unwrap()
            .expect("the environment row must read back")
    }

    /// The raw text `SQLite` holds for one column of the single fixture row,
    /// which is what makes a byte-level assertion possible at all —
    /// `serde_json::Value` would have normalised whatever was stored.
    ///
    /// No `WHERE`: how `SeaORM` encodes a `Uuid` for `SQLite` is its business,
    /// and every caller inserts exactly one row.
    async fn raw_text(conn: &DatabaseConnection, column: &str) -> String {
        conn.query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!("SELECT {column} AS c FROM qa_environments"),
        ))
        .await
        .unwrap()
        .expect("the environment row must read back")
        .try_get::<String>("", "c")
        .expect("the column must hold text")
    }

    #[tokio::test]
    async fn all_seven_columns_round_trip_through_the_migrated_schema() {
        let conn = migrated_db().await;

        plant_legacy(
            &conn,
            &[
                (
                    "credentials",
                    super::super::legacy_row::text(
                        r#"[{"key":"kubeconfig","credstore_ref":"credstore://kc/staging-a"}]"#,
                    ),
                ),
                (
                    "observed_attrs",
                    super::super::legacy_row::text(r#"{"platformVersion":"26.5","build":"1471"}"#),
                ),
                (
                    "config",
                    super::super::legacy_row::text(r#"{"vpadm_namespace":"vzt"}"#),
                ),
                (
                    "observed_base_url",
                    super::super::legacy_row::text("https://sv.jele.io"),
                ),
                ("health_state", super::super::legacy_row::text("degraded")),
                (
                    "health_detail",
                    super::super::legacy_row::text("one node not ready"),
                ),
                (
                    "health_checked_at",
                    super::super::legacy_row::text(
                        &now()
                            .format(&time::format_description::well_known::Rfc3339)
                            .expect("the fixture timestamp must format"),
                    ),
                ),
            ],
        )
        .await;

        let stored = reload(&conn).await;
        assert_eq!(
            stored.credentials,
            serde_json::json!([
                {"key": "kubeconfig", "credstore_ref": "credstore://kc/staging-a"}
            ]),
            "credentials must survive the round trip"
        );
        assert_eq!(
            stored.observed_attrs,
            serde_json::json!({"platformVersion": "26.5", "build": "1471"}),
            "observed_attrs must survive the round trip"
        );
        assert_eq!(
            stored.config,
            serde_json::json!({"vpadm_namespace": "vzt"}),
            "config must survive the round trip"
        );
        assert_eq!(
            stored.observed_base_url.as_deref(),
            Some("https://sv.jele.io"),
            "observed_base_url must survive the round trip"
        );
        assert_eq!(stored.health_state, "degraded");
        assert_eq!(stored.health_detail.as_deref(), Some("one node not ready"));
        assert_eq!(stored.health_checked_at, Some(now()));
        assert_eq!(
            stored.observed_version.as_deref(),
            Some("26.5"),
            "and the columns added before it must be unaffected by the ALTER"
        );
    }

    /// The three JSON columns and `health_state` are `NOT NULL DEFAULT`, so a
    /// row that names none of them must come back with the declared default
    /// rather than failing the insert.
    #[tokio::test]
    async fn the_four_not_null_columns_apply_their_declared_defaults() {
        let conn = migrated_db().await;

        // No plugin-shaped column named at all, so every DEFAULT applies.
        plant_legacy(&conn, &[]).await;

        let stored = reload(&conn).await;
        assert_eq!(stored.credentials, serde_json::json!([]));
        assert_eq!(stored.observed_attrs, serde_json::json!({}));
        assert_eq!(stored.config, serde_json::json!({}));
        assert_eq!(
            stored.health_state, "unknown",
            "the DEFAULT must be HealthState::default()'s wire form"
        );
    }

    #[tokio::test]
    async fn the_three_nullable_columns_are_nullable() {
        let conn = migrated_db().await;

        plant_legacy(
            &conn,
            &[
                ("observed_base_url", "NULL".to_owned()),
                ("health_detail", "NULL".to_owned()),
                ("health_checked_at", "NULL".to_owned()),
            ],
        )
        .await;

        let stored = reload(&conn).await;
        assert_eq!(stored.observed_base_url, None);
        assert_eq!(stored.health_detail, None);
        assert_eq!(stored.health_checked_at, None);
    }

    /// All four backfills at once, against one representative pre-existing
    /// row: a kubeconfig reference, a base URL, a cluster status with a check
    /// time, and a `VPADM_NAMESPACE` variable.
    #[tokio::test]
    async fn the_backfill_maps_a_representative_pre_existing_row() {
        let conn = migrated_db().await;
        plant_legacy(&conn, &[]).await;
        vpadm_namespace_variable("  vzt  ")
            .insert(&conn)
            .await
            .unwrap();

        run_backfill(&conn).await;

        let stored = reload(&conn).await;
        assert_eq!(
            stored.credentials,
            serde_json::json!([
                {"key": "kubeconfig", "credstore_ref": "credstore://kc/staging-a"}
            ]),
            "credentials must be backfilled from kubeconfig_credstore_ref"
        );
        assert_eq!(
            stored.observed_base_url.as_deref(),
            Some("https://sv.jele.io"),
            "observed_base_url must be backfilled from vhp_base_url"
        );
        assert_eq!(
            stored.health_state, "ok",
            "cluster_status Healthy must map to HealthState::Ok's wire form"
        );
        assert_eq!(
            stored.health_checked_at,
            Some(now()),
            "health_checked_at must be backfilled from cluster_checked_at, which \
             is what keeps 'checked and unreadable' distinct from 'never checked'"
        );
        assert_eq!(
            stored.config,
            serde_json::json!({"vpadm_namespace": "vzt"}),
            "config must carry the TRIMMED variable value under the plugin's \
             lowercase key"
        );
        assert_eq!(
            stored.observed_attrs,
            serde_json::json!({}),
            "observed_attrs is deliberately NOT backfilled: the attribute keys \
             are the plugin's, and the first cycle writes them"
        );
        // **The legacy half is asserted through raw SQL, not the entity.**
        // Task 19's `m20260903_000012` drops these three columns and the model
        // lost the fields in the same commit -- but this migration is the
        // *expand* half and its contract is precisely that it drops nothing, so
        // the assertion has to survive even though the type cannot express it.
        for (column, expected) in [
            ("kubeconfig_credstore_ref", "credstore://kc/staging-a"),
            ("vhp_base_url", "https://sv.jele.io"),
            ("cluster_status", "Healthy"),
        ] {
            let value: Option<String> = conn
                .query_one(Statement::from_string(
                    conn.get_database_backend(),
                    format!(
                        "SELECT {column} FROM qa_environments WHERE id = {};",
                        super::super::legacy_row::uuid_lit(uuid(1))
                    ),
                ))
                .await
                .unwrap()
                .expect("the environment row must read back")
                .try_get_by_index(0)
                .expect("the column must be readable as text");
            assert_eq!(
                value.as_deref(),
                Some(expected),
                "{column} must survive -- this is the expand half and it drops nothing"
            );
        }
    }

    /// The credentials blob the migration writes must be **exactly** what
    /// `serde_json` writes for the type that reads the column back. Compared
    /// against `SQLite`'s stored bytes rather than a hand-typed twin, which
    /// could drift from the SQL and still pass.
    ///
    /// Postgres' `jsonb` and `MySQL`'s `JSON` normalise key order and
    /// whitespace on write, so this byte-level form is `SQLite`'s; what all
    /// three share, and what this really pins, is the two **field names**.
    #[tokio::test]
    async fn the_credentials_backfill_is_byte_identical_to_what_serde_writes() {
        let conn = migrated_db().await;
        plant_legacy(&conn, &[]).await;

        run_backfill(&conn).await;

        let stored_bytes = raw_text(&conn, "credentials").await;
        let expected = serde_json::to_string(&vec![StoredEnvironmentCredential {
            key: "kubeconfig".to_owned(),
            credstore_ref: "credstore://kc/staging-a".to_owned(),
        }])
        .unwrap();
        assert_eq!(
            stored_bytes, expected,
            "the migration's JSON and the codec's JSON must agree, field names \
             and all"
        );

        let decoded: Vec<StoredEnvironmentCredential> =
            serde_json::from_str(&stored_bytes).expect("the migration's own bytes must decode");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].key, "kubeconfig");
        assert_eq!(decoded[0].credstore_ref, "credstore://kc/staging-a");
    }

    /// A present-but-blank override does not count as set, any more than an
    /// absent one does. That rule is legacy's
    /// (`manager/src/services/platforms.rs`' `pick_vpadm_namespace`), it is
    /// implemented in both this gear's `pick_vpadm_namespace` and the plugin's
    /// `observe::vpadm_namespace`, and it has to survive the migration or an
    /// environment whose variable is whitespace would suddenly resolve to `""`
    /// instead of `virtuozzo`.
    #[tokio::test]
    async fn a_blank_vpadm_namespace_override_does_not_count_as_set() {
        let conn = migrated_db().await;
        plant_legacy(&conn, &[]).await;
        vpadm_namespace_variable("   ").insert(&conn).await.unwrap();

        run_backfill(&conn).await;

        assert_eq!(
            reload(&conn).await.config,
            serde_json::json!({}),
            "a whitespace-only override must leave config empty, NOT write \
             {{\"vpadm_namespace\": \"\"}}"
        );
    }

    #[tokio::test]
    async fn an_absent_vpadm_namespace_override_leaves_the_default() {
        let conn = migrated_db().await;
        plant_legacy(&conn, &[]).await;

        run_backfill(&conn).await;

        assert_eq!(reload(&conn).await.config, serde_json::json!({}));
    }

    /// A variable of some other name must not be mistaken for the override.
    #[tokio::test]
    async fn another_variable_is_not_the_vpadm_namespace_override() {
        let conn = migrated_db().await;
        plant_legacy(&conn, &[]).await;
        let am = environment_variable::ActiveModel {
            name: ActiveValue::Set("SOME_OTHER_VAR".to_owned()),
            ..vpadm_namespace_variable("vzt")
        };
        am.insert(&conn).await.unwrap();

        run_backfill(&conn).await;

        assert_eq!(reload(&conn).await.config, serde_json::json!({}));
    }

    /// Another environment's override must not leak into this row: the
    /// correlated subquery is joined on `platform_id`, and dropping that
    /// predicate is the mistake this catches.
    #[tokio::test]
    async fn a_different_environments_override_does_not_reach_this_row() {
        let conn = migrated_db().await;
        plant_legacy(&conn, &[]).await;
        plant_other(&conn).await;
        let am = environment_variable::ActiveModel {
            environment_id: ActiveValue::Set(uuid(99)),
            ..vpadm_namespace_variable("only-for-staging-b")
        };
        am.insert(&conn).await.unwrap();

        run_backfill(&conn).await;

        assert_eq!(
            reload(&conn).await.config,
            serde_json::json!({}),
            "staging-a has no override of its own"
        );
        let other = environment::Entity::find_by_id(uuid(99))
            .one(&conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            other.config,
            serde_json::json!({"vpadm_namespace": "only-for-staging-b"}),
            "and staging-b has exactly its own"
        );
    }

    /// A variable row whose tenant disagrees with its environment's must not
    /// be copied into that environment's `config`. Well-formed data cannot
    /// produce this row -- the tenant-scoped unique index and every write path
    /// agree -- which is exactly why the predicate that excludes it needs a
    /// test of its own: nothing else in the suite would notice it going
    /// missing.
    #[tokio::test]
    async fn a_variable_belonging_to_another_tenant_is_not_read_into_config() {
        let conn = migrated_db().await;
        plant_legacy(&conn, &[]).await;
        let am = environment_variable::ActiveModel {
            tenant_id: ActiveValue::Set(uuid(999)),
            ..vpadm_namespace_variable("other-tenants-ns")
        };
        am.insert(&conn).await.unwrap();

        run_backfill(&conn).await;

        assert_eq!(
            reload(&conn).await.config,
            serde_json::json!({}),
            "the variable names this environment but not its tenant, so it must \
             not reach config"
        );
    }

    /// One row per source value, including `NULL`. The mapping table is in the
    /// module doc; this is what holds it.
    #[tokio::test]
    async fn every_cluster_status_maps_to_a_health_state() {
        for (index, (cluster_status, expected)) in [
            (Some("Healthy"), "ok"),
            (Some("Degraded"), "degraded"),
            (Some("Warning"), "degraded"),
            (Some("Unhealthy"), "down"),
            (Some("Unreachable"), "unknown"),
            (None, "unknown"),
        ]
        .into_iter()
        .enumerate()
        {
            let conn = migrated_db().await;
            plant_legacy(
                &conn,
                &[(
                    "cluster_status",
                    cluster_status
                        .map_or_else(|| "NULL".to_owned(), super::super::legacy_row::text),
                )],
            )
            .await;

            run_backfill(&conn).await;

            let stored = reload(&conn).await;
            assert_eq!(
                stored.health_state, expected,
                "case {index}: cluster_status {cluster_status:?} must map to {expected}"
            );
            if cluster_status.is_none() {
                assert_eq!(
                    stored.health_checked_at, None,
                    "a row no cycle ever reached must keep health_checked_at NULL -- \
                     that is the only thing separating 'never checked' from a failed check"
                );
                assert_eq!(stored.health_detail, None);
            } else {
                assert_eq!(
                    stored.health_checked_at,
                    Some(now()),
                    "a row a cycle did reach must carry its check time"
                );
                assert_eq!(
                    stored.health_detail.as_deref(),
                    Some("classified text"),
                    "health_detail takes cluster_status_message, which carries \
                     text already classified by infra::runner_secret_errors (D-CH-5)"
                );
            }
        }
    }

    /// Re-running the backfill must not overwrite a value the plugin path has
    /// since written.
    ///
    /// This migration's own module doc prescribes hand-running
    /// [`super::backfill_for`]'s statements as the `MySQL` partial-failure
    /// recovery, and Task 15 is the writer of these columns — so an unguarded
    /// re-run months later would replace a fresh observation with
    /// legacy-derived values. The guard is a `WHERE` on each filled column;
    /// this is the property, asserted rather than the SQL shape.
    #[tokio::test]
    async fn re_running_the_backfill_leaves_plugin_written_values_alone() {
        let conn = migrated_db().await;
        plant_legacy(&conn, &[]).await;
        vpadm_namespace_variable("vzt").insert(&conn).await.unwrap();

        run_backfill(&conn).await;

        // What Task 15's `record_observation` would have written since.
        let observed = environment::ActiveModel {
            id: ActiveValue::Unchanged(uuid(1)),
            credentials: ActiveValue::Set(serde_json::json!([
                {"key": "kubeconfig", "credstore_ref": "credstore://kc/rotated"}
            ])),
            config: ActiveValue::Set(serde_json::json!({"vpadm_namespace": "operator-set"})),
            observed_base_url: ActiveValue::Set(Some("https://rotated.jele.io".to_owned())),
            health_state: ActiveValue::Set("degraded".to_owned()),
            health_detail: ActiveValue::Set(Some("one node not ready".to_owned())),
            ..Default::default()
        };
        observed.update(&conn).await.unwrap();

        run_backfill(&conn).await;

        let stored = reload(&conn).await;
        assert_eq!(
            stored.credentials,
            serde_json::json!([
                {"key": "kubeconfig", "credstore_ref": "credstore://kc/rotated"}
            ]),
            "a re-run must not replace a written credential with the legacy one"
        );
        assert_eq!(
            stored.observed_base_url.as_deref(),
            Some("https://rotated.jele.io")
        );
        assert_eq!(stored.health_state, "degraded");
        assert_eq!(stored.health_detail.as_deref(), Some("one node not ready"));
        assert_eq!(
            stored.config,
            serde_json::json!({"vpadm_namespace": "operator-set"}),
            "nor an operator's own config with the variable's value"
        );
    }

    /// `up()` really applies the backfill, and really applies it **after** the
    /// `ALTER`, to a row that existed before the migration ran.
    ///
    /// The other backfill tests re-run [`super::backfill_for`] by hand against
    /// an already-migrated database, which cannot see an `up()` that forgot to
    /// call it. This one stops the migrator one migration short, inserts a
    /// legacy row with raw SQL (the entity has the new columns and could not
    /// address the old schema), and then applies this migration whole.
    #[tokio::test]
    async fn up_backfills_a_row_that_existed_before_the_migration() {
        let conn = db_one_migration_short().await;
        conn.execute_unprepared(
            "INSERT INTO qa_environments
                 (id, tenant_id, name, product_id, description,
                  kubeconfig_credstore_ref, available, is_default,
                  vhp_base_url, cluster_status, cluster_status_message,
                  created_at, updated_at)
             VALUES
                 ('env-1', 'tenant-1', 'staging-a', NULL, NULL,
                  'credstore://kc/legacy', 1, 0,
                  'https://legacy.jele.io', 'Unreachable', 'connect failed',
                  '2026-08-14 12:00:00', '2026-08-14 12:00:00');
             INSERT INTO qa_environment_variables
                 (id, tenant_id, platform_id, name, value, created_at, updated_at)
             VALUES
                 ('var-1', 'tenant-1', 'env-1', 'VPADM_NAMESPACE', ' legacy-ns ',
                  '2026-08-14 12:00:00', '2026-08-14 12:00:00');",
        )
        .await
        .expect("the legacy fixture must insert against the pre-migration schema");

        let manager = SchemaManager::new(&conn);
        MigrationTrait::up(&super::Migration, &manager)
            .await
            .expect("up() must apply");

        // Read raw: the fixture's uuid and timestamp columns hold text this
        // entity would refuse to parse, and the new columns are what is under
        // test.
        let row = conn
            .query_one(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT credentials, observed_attrs, config, observed_base_url,
                        health_state, health_detail
                   FROM qa_environments WHERE id = 'env-1'",
            ))
            .await
            .unwrap()
            .expect("the legacy row must still be there");
        assert_eq!(
            row.try_get::<String>("", "credentials").unwrap(),
            r#"[{"key":"kubeconfig","credstore_ref":"credstore://kc/legacy"}]"#,
            "up() must run the backfill, not just the ALTER"
        );
        assert_eq!(row.try_get::<String>("", "observed_attrs").unwrap(), "{}");
        assert_eq!(
            row.try_get::<String>("", "config").unwrap(),
            r#"{"vpadm_namespace":"legacy-ns"}"#
        );
        assert_eq!(
            row.try_get::<String>("", "observed_base_url").unwrap(),
            "https://legacy.jele.io"
        );
        assert_eq!(
            row.try_get::<String>("", "health_state").unwrap(),
            "unknown"
        );
        assert_eq!(
            row.try_get::<String>("", "health_detail").unwrap(),
            "connect failed"
        );
    }

    /// `down()` has to actually drop all seven columns, or a rollback leaves a
    /// schema the previous release's entity cannot read.
    #[tokio::test]
    async fn the_down_migration_drops_all_seven_columns() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        for column in [
            "credentials",
            "observed_attrs",
            "config",
            "observed_base_url",
            "health_state",
            "health_detail",
            "health_checked_at",
        ] {
            let result = conn
                .execute_unprepared(&format!("SELECT {column} FROM qa_environments"))
                .await;
            assert!(
                result.is_err(),
                "{column} must be gone after down(), but the SELECT succeeded"
            );
        }
        conn.execute_unprepared(
            "SELECT kubeconfig_credstore_ref, vhp_base_url, cluster_status FROM qa_environments",
        )
        .await
        .expect("down() must not touch the legacy columns it expanded alongside");
    }

    /// `schema_for` and `backfill_for` map each backend to its own statements
    /// -- nothing else calls them, and the suite only ever runs migrations
    /// against `SQLite`, so without this guard a swapped `Postgres`/`MySql`
    /// arm is invisible to every other test in this module.
    #[test]
    #[allow(
        clippy::cognitive_complexity,
        reason = "a flat list of one assertion per (dialect, column) pair -- the shape a \
                  guard over three dialect blobs needs, and the shape the sibling \
                  migrations' equivalent guards have. Splitting it into helpers would not \
                  reduce what it checks, only how it reads. Its own comments record the two \
                  mutations that survived the narrower version."
    )]
    fn each_backend_gets_statements_of_the_right_column_type() {
        assert_eq!(
            super::schema_for(DatabaseBackend::Postgres),
            super::POSTGRES_SCHEMA
        );
        assert_eq!(
            super::schema_for(DatabaseBackend::MySql),
            super::MYSQL_SCHEMA
        );
        assert_eq!(
            super::schema_for(DatabaseBackend::Sqlite),
            super::SQLITE_SCHEMA
        );
        assert_eq!(
            super::backfill_for(DatabaseBackend::Postgres),
            super::POSTGRES_BACKFILL
        );
        assert_eq!(
            super::backfill_for(DatabaseBackend::MySql),
            super::MYSQL_BACKFILL
        );
        assert_eq!(
            super::backfill_for(DatabaseBackend::Sqlite),
            super::SQLITE_BACKFILL
        );

        // Every JSON column, per dialect -- not just the first one. Pinning
        // `credentials` alone left two mutations alive through the whole
        // suite: a literal `observed_attrs JSON NOT NULL DEFAULT '{}'` in the
        // MySQL blob (exactly the MySQL-invalid form this migration exists to
        // avoid) and an `observed_attrs JSONB` in the SQLite one, which SQLite
        // accepts as an unknown type name with NUMERIC affinity while every
        // round-trip test still passes.
        let postgres = super::schema_for(DatabaseBackend::Postgres);
        for (column, default) in [
            ("credentials", "'[]'"),
            ("observed_attrs", "'{}'"),
            ("config", "'{}'"),
        ] {
            assert!(
                postgres.contains(&format!("{column} JSONB NOT NULL DEFAULT {default}")),
                "Postgres must get JSONB with a literal default for {column}"
            );
        }
        assert!(
            postgres.contains("health_checked_at TIMESTAMPTZ"),
            "Postgres must get TIMESTAMPTZ, or the stored check time loses its \
             timezone"
        );

        let mysql = super::schema_for(DatabaseBackend::MySql);
        for (column, default) in [
            ("credentials", "('[]')"),
            ("observed_attrs", "('{}')"),
            ("config", "('{}')"),
        ] {
            assert!(
                mysql.contains(&format!("{column} JSON NOT NULL DEFAULT {default}")),
                "MySQL rejects a literal default on a JSON column and accepts \
                 only the parenthesised expression form -- {column}"
            );
        }
        assert!(!mysql.contains("JSONB"), "MySQL has no JSONB type");
        assert!(
            mysql.contains("health_state TEXT NOT NULL DEFAULT ('unknown')"),
            "and the same expression form for a TEXT default, which MySQL also \
             refuses as a literal"
        );
        assert!(
            mysql.contains("health_checked_at TIMESTAMP NULL"),
            "MySQL must get the bare TIMESTAMP statement"
        );
        assert!(!mysql.contains("TIMESTAMPTZ"));

        let sqlite = super::schema_for(DatabaseBackend::Sqlite);
        for column in [
            "credentials TEXT NOT NULL DEFAULT '[]'",
            "observed_attrs TEXT NOT NULL DEFAULT '{}'",
            "config TEXT NOT NULL DEFAULT '{}'",
        ] {
            assert!(
                sqlite.contains(column),
                "SQLite has no JSON type, so all three JSON columns are TEXT: {column}"
            );
        }
        assert!(
            !sqlite.contains("JSON"),
            "SQLite must get no server JSON type -- and the test for it has to be \
             `JSON`, not `JSON `: SQLite accepts `JSONB` as an unknown type name \
             with NUMERIC affinity, so a trailing space let that through"
        );
        assert!(
            sqlite.contains("health_checked_at TEXT NULL"),
            "SQLite has no native temporal type either"
        );
        assert!(!sqlite.contains("TIMESTAMP"));

        // Every backfill statement is guarded, so re-running one -- which this
        // migration's own MySQL partial-failure recovery prescribes -- cannot
        // overwrite a value the plugin path has since written.
        for backend in [
            DatabaseBackend::Postgres,
            DatabaseBackend::MySql,
            DatabaseBackend::Sqlite,
        ] {
            let backfill = super::backfill_for(backend);
            assert_eq!(
                backfill.matches("UPDATE qa_environments").count(),
                4,
                "{backend:?}: four UPDATEs -- credentials, observed_base_url, \
                 health, config"
            );
            assert_eq!(
                backfill.matches("\n WHERE ").count(),
                4,
                "{backend:?}: and a WHERE guard on every one of them"
            );
        }

        // Each engine builds the JSON with its own constructor rather than by
        // string concatenation, so a credstore reference containing a quote
        // cannot produce a malformed document.
        assert!(super::backfill_for(DatabaseBackend::Postgres).contains("jsonb_build_object"));
        assert!(super::backfill_for(DatabaseBackend::MySql).contains("JSON_OBJECT"));
        assert!(super::backfill_for(DatabaseBackend::Sqlite).contains("json_object"));
    }
}
