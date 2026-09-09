//! Drops the eight Kubernetes-shaped columns `qa_environments` no longer needs,
//! after re-deriving `credentials` from the last of them.
//!
//! **Added 2026-09-05, product-plugins plan Task 19 (spec §6.2).** This is the
//! *contract* half of the expand/contract pair `m20260903_000011` opened, and
//! it is the plan's one-way door: `kubeconfig_credstore_ref`, `vhp_base_url`,
//! `observed_namespace` and the five `cluster_*` columns go, and no `down()`
//! can put back a value that was only ever in a column this drops.
//!
//! ## The eight, and what replaced each
//!
//! | dropped | replaced by | written since |
//! | --- | --- | --- |
//! | `kubeconfig_credstore_ref` | `credentials` — `[{key, credstore_ref}]`, *n* per environment | Task 18b |
//! | `vhp_base_url` | `observed_base_url`, the `FieldRole::BaseUrl` projection | Task 15 |
//! | `observed_namespace` | `observed_attrs`, surfaced through `FieldRole::Namespace` | Task 15 |
//! | `cluster_status` | `health_state` | Task 15 |
//! | `cluster_status_message` | `health_detail` | Task 15 |
//! | `cluster_checked_at` | `health_checked_at` | Task 15 |
//! | `cluster_nodes` | nothing — **user decision U4** | — |
//! | `cluster_namespace_count` | nothing — **user decision U4** | — |
//!
//! The last two have no replacement on purpose: "nodes ready" is not health for
//! a `SaaS` tenant, and a node inventory is not something the plugin contract
//! carries. Task 21 restores node *detail* as descriptor-driven fields over
//! `observed_attrs` if a product declares any.
//!
//! **E-23 is discharged here rather than carried.** `observed_namespace` was
//! sticky where `observed_attrs` is overwritten, so a redeployed environment
//! ran in its new namespace and displayed the old one. Dropping the column
//! removes the divergence outright — which is the answer to a question the
//! code asked for one phase.
//!
//! ## The re-derivation, and why it is a partition rather than a condition list
//!
//! Ruling F-1 called this an **unconditional** overwrite of `credentials` from
//! `kubeconfig_credstore_ref`, justified by "Task 18b's dual-write means every
//! row written since 18b already agrees with it". **That justification is false
//! for any row with more than one stored credential**, and the pre-Task-19
//! review measured both shapes it breaks:
//!
//! * *n* credentials with one of them in the legacy column — an unconditional
//!   overwrite replaces *n* with **1**, deleting every other binding and
//!   orphaning its credstore secret with nothing able to name it;
//! * two or more *required* secrets — `sole_required_secret_key()` returns
//!   `None`, so the legacy column is `''` beside a valid `credentials`, and the
//!   overwrite writes an entry pointing at nothing: the credential-less row
//!   Critical C-2 was raised to prevent, produced by the statement meant to
//!   repair it.
//!
//! Neither is reachable with the one plugin this tree ships, which declares a
//! single required secret named `kubeconfig`. That is an accident of there
//! being one plugin, which is the thing this branch exists to end.
//!
//! So this migration says what happens to **every** row. Writing `N` for the
//! length of `credentials`, `K` for its single entry's key, `R` for that
//! entry's `credstore_ref` and `L` for `kubeconfig_credstore_ref`:
//!
//! | # | shape | action |
//! | --- | --- | --- |
//! | 1 | `N=0`, `L≠''` | **overwrite** — the *empty* state |
//! | 2 | `N=1`, `K='kubeconfig'`, `L≠''`, `R≠L` | **overwrite** — the *stale* state |
//! | 3 | `N=1`, `K='kubeconfig'`, `L≠''`, `R=L` | skip — already agrees |
//! | 4 | `N=0`, `L=''` | skip — nothing to derive |
//! | 5 | `N=1`, `K` present and `≠'kubeconfig'`, `L≠''`, `R=L` | skip — complete under its own key |
//!
//! Bucket 2 is what ruling F-1 exists for: `m20260903_000011`'s backfill was a
//! one-time snapshot, and a pre-18b rotation wrote `L` and **deleted** the
//! superseded `R` without touching `credentials`. A stale row cannot present
//! with `R=L`, because "stale" is defined against the legacy column, which F-1
//! makes authoritative.
//!
//! ## The halts
//!
//! Everything outside those five buckets stops the migration, naming the
//! environment ids, in the shape `m20260903_000004`'s pre-check uses in
//! `qa-catalog`. A step that cannot be undone refuses rather than guesses.
//!
//! * **H4** — `credentials` is not a JSON array. **Runs first and alone**, and
//!   that is load-bearing: `jsonb_array_length` *raises* on a Postgres scalar
//!   and SQL guarantees no evaluation order across `OR`, so folded in beside
//!   the `N`-based predicates Postgres could abort on the corrupt row before
//!   this one was considered — the opaque failure this halt exists to replace.
//!   The type check is written to be total on every backend: `jsonb_typeof` is
//!   total on Postgres and `MySQL`, and the `SQLite` arm guards `json_type`
//!   with `json_valid`, because `json_type` raises on a value that is not JSON
//!   at all. That guard is spelled `iif(json_valid(c), json_type(c) = 'array',
//!   0)` rather than `json_valid(c) AND json_type(c) = 'array'` for the same
//!   reason H4 is isolated from the `OR`: SQL guarantees no evaluation order
//!   for `AND` either, and `iif` is short-circuiting by definition, so the
//!   guard does not rest on an engine's current habit (re-review, N-9). See
//!   [`Json::is_array`]. (Finding I-8 asked Task 19 to *decide* the corrupt-blob behaviour
//!   rather than inherit `mapper.rs`' degrade-to-empty; halting and naming the
//!   row is the decision.)
//! * **H1** — `N>1`. The legacy column can name only one of them.
//! * **H2** — `N>0` beside an empty `L`. Two or more required secrets.
//! * **H6** — `N=1` and the entry has **no** `key` (absent, or JSON `null`).
//!   Checked *before* H3 so a keyless row gets its own message instead of H3's,
//!   and unconditionally on `R`: bucket 5 skips because the row is "complete
//!   under its own key", and a row with no key is not. Dropping
//!   `kubeconfig_credstore_ref` would take away the only thing naming that
//!   entry's purpose, so the migration refuses rather than silently demote it.
//!   (On `MySQL` a JSON `null` key unquotes to the *string* `'null'` rather than
//!   SQL `NULL`, so there it falls through to H3 or bucket 5 as it did before
//!   this halt existed. `SQLite` and Postgres, which this tree runs, give SQL
//!   `NULL` for both an absent key and a JSON `null` one.)
//! * **H3** — `N=1`, `K` present and `≠'kubeconfig'`, `R≠L`. The two disagree
//!   and resolving which is current needs the plugin's own key; a migration
//!   cannot call a plugin. (`R=L` is bucket 5 and needs nothing.)
//! * **H5** — a bucket-1 row exists and bucket-1 rows span more than one
//!   product. See below.
//!
//! ### H5, and why bucket 1 is the one that needed a guard
//!
//! Buckets 2, 3 and 5 are safe because their key is **read from the row**.
//! Bucket 1 has no key to read: it writes the literal `'kubeconfig'` onto a row
//! that never carried one, so it is the only place the "VHP era" premise is
//! assumed rather than enforced.
//!
//! And bucket-1 rows are still being written when this migration runs.
//! `EnvironmentsService`' plugin-unavailable and productless credential
//! branches both produce `credentials = []` beside a minted legacy reference,
//! and `qa-runs`' `plugin_dispatch` derives such a row's key **live** from
//! `sole_required_secret_key(&plugin.credential_schema())` — so those rows
//! track the current plugin today, and this overwrite is what freezes them.
//!
//! H5 enforces the premise with data the table already holds: if every bucket-1
//! row belongs to one product, that product's plugin is the only key-giver and
//! the literal cannot be wrong for it. More than one and the migration stops;
//! the operator clears the rows by re-saving those environments, which routes
//! them through the plugin path that keys them correctly.
//!
//! `COUNT(DISTINCT product_id)` **ignores `NULL`s, deliberately**: a productless
//! bucket-1 row has no plugin whose key could disagree with the literal, so it
//! must not make the count exceed one. Do not reach for `COALESCE` here.
//!
//! **What H5 does not cover, recorded rather than absorbed:** it proves *one
//! product*, not *one key-giver*. A product rebound to a different plugin
//! (ruling F-10 permits it) or a plugin renaming its sole required secret
//! between releases both defeat it invisibly. Neither is closable from this
//! gear — the only authority is `qa_products`, which belongs to `qa-catalog`.
//!
//! ## `down()` is best-effort and says so
//!
//! It re-adds the eight columns and repopulates `kubeconfig_credstore_ref` from
//! `credentials` and `vhp_base_url` from `observed_base_url`. It **cannot**
//! recover `observed_namespace` for a product whose plugin never declared a
//! namespace attribute, any `cluster_*` value (nothing has written those since
//! Task 15), or a second credential — one column cannot hold *n*.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Per-backend JSON accessors over `credentials`.
///
/// One struct rather than three sets of constants because every predicate
/// below needs all four, and pairing the wrong `length` with the wrong
/// `extract` is exactly the swap `m20260828_000007`'s `sql_for` guard was
/// added to catch.
struct Json {
    /// Whether `credentials` holds a JSON array, **without raising on a value
    /// that is not JSON at all**.
    ///
    /// `jsonb_typeof` is total on Postgres and `MySQL`, where the column type
    /// makes a non-JSON value unstorable in the first place. `SQLite` stores it
    /// as `TEXT`, so `json_type` there raises `malformed JSON` on a blob like
    /// `'not json at all'` — which would have H4 die with a type error instead
    /// of naming the row, the exact failure it exists to replace. Hence the
    /// `json_valid` guard on that arm (whole-branch review, m-1).
    is_array: &'static str,
    /// Raises on a non-array in Postgres, hence H4 running first.
    length: &'static str,
    /// The first entry's `key`.
    key: &'static str,
    /// The first entry's `credstore_ref`.
    reference: &'static str,
    /// Build a one-entry array naming `kubeconfig_credstore_ref`.
    rebuild: &'static str,
}

const POSTGRES_JSON: Json = Json {
    is_array: "jsonb_typeof(credentials) = 'array'",
    length: "jsonb_array_length(credentials)",
    key: "credentials->0->>'key'",
    reference: "credentials->0->>'credstore_ref'",
    rebuild: "jsonb_build_array(jsonb_build_object('key', 'kubeconfig', 'credstore_ref', \
              kubeconfig_credstore_ref))",
};

const MYSQL_JSON: Json = Json {
    is_array: "JSON_TYPE(credentials) = 'ARRAY'",
    length: "JSON_LENGTH(credentials)",
    key: "JSON_UNQUOTE(JSON_EXTRACT(credentials, '$[0].key'))",
    reference: "JSON_UNQUOTE(JSON_EXTRACT(credentials, '$[0].credstore_ref'))",
    rebuild: "JSON_ARRAY(JSON_OBJECT('key', 'kubeconfig', 'credstore_ref', \
              kubeconfig_credstore_ref))",
};

const SQLITE_JSON: Json = Json {
    is_array: "iif(json_valid(credentials), json_type(credentials) = 'array', 0)",
    length: "json_array_length(credentials)",
    key: "json_extract(credentials, '$[0].key')",
    reference: "json_extract(credentials, '$[0].credstore_ref')",
    rebuild: "json_array(json_object('key', 'kubeconfig', 'credstore_ref', \
              kubeconfig_credstore_ref))",
};

/// Pick the accessors for a backend.
///
/// Extracted rather than inlined in the `match`, on
/// `m20260828_000007_platform_observation::sql_for`'s precedent: inline,
/// nothing could reach it, and swapping two adjacent arms is the mistake that
/// guard exists to catch.
fn json_for(backend: DatabaseBackend) -> Result<&'static Json, DbErr> {
    match backend {
        DatabaseBackend::Postgres => Ok(&POSTGRES_JSON),
        DatabaseBackend::MySql => Ok(&MYSQL_JSON),
        DatabaseBackend::Sqlite => Ok(&SQLITE_JSON),
        other => Err(DbErr::Migration(format!(
            "unsupported database backend: {other:?}"
        ))),
    }
}

/// `L` is empty — `NULL` and `''` are the same fact to every reader.
const LEGACY_EMPTY: &str = "(kubeconfig_credstore_ref IS NULL OR kubeconfig_credstore_ref = '')";

/// The eight columns, in the order the module doc lists them.
const DROPPED: [&str; 8] = [
    "kubeconfig_credstore_ref",
    "vhp_base_url",
    "observed_namespace",
    "cluster_status",
    "cluster_status_message",
    "cluster_nodes",
    "cluster_namespace_count",
    "cluster_checked_at",
];

impl Migration {
    /// Every id matching `predicate`, as a comma-separated list, or `None`.
    ///
    /// Read through a plain `SELECT` rather than the entity, because this
    /// task removes eight of the entity's fields in the same commit and a
    /// migration that depends on the shape of today's model is a migration
    /// that stops compiling three releases later.
    async fn ids_where(
        conn: &SchemaManagerConnection<'_>,
        predicate: &str,
    ) -> Result<Option<String>, DbErr> {
        let rows = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                format!("SELECT id FROM qa_environments WHERE {predicate};"),
            ))
            .await?;

        if rows.is_empty() {
            return Ok(None);
        }
        let mut ids = Vec::with_capacity(rows.len());
        for row in &rows {
            ids.push(
                row.try_get_by_index::<uuid::Uuid>(0)
                    .map(|id| id.to_string())
                    .or_else(|_| row.try_get_by_index::<String>(0))?,
            );
        }
        Ok(Some(ids.join(", ")))
    }

    /// Refuse to run when any row is outside the five buckets.
    ///
    /// See the module doc. H4 is first and alone because `jsonb_array_length`
    /// raises on a non-array and no `OR` guarantees it would not be reached.
    async fn refuse_unrepairable_rows(
        conn: &SchemaManagerConnection<'_>,
        json: &Json,
    ) -> Result<(), DbErr> {
        let Json {
            is_array,
            length,
            key,
            reference,
            ..
        } = json;

        // H4, alone and first.
        if let Some(ids) = Self::ids_where(conn, &format!("NOT ({is_array})")).await? {
            return Err(DbErr::Custom(format!(
                "qa-environments m20260903_000012: the `credentials` column of these \
                 environments does not hold a JSON array, so this migration cannot tell \
                 what they store and will not drop the column they could be repaired \
                 from: {ids}. Re-save each environment's credentials, then re-run."
            )));
        }

        // Everything below is safe now that every row is an array.
        for (halt, predicate, remedy) in [
            (
                "H1",
                format!("{is_array} AND {length} > 1"),
                "hold more than one credential, and the column being dropped can name only \
                 one of them",
            ),
            (
                "H2",
                format!("{is_array} AND {length} > 0 AND {LEGACY_EMPTY}"),
                "hold credentials with no legacy reference beside them, which means their \
                 product's plugin declares two or more required secrets",
            ),
            (
                "H6",
                format!("{is_array} AND {length} = 1 AND {key} IS NULL"),
                "store one credential with no `key`, so dropping the legacy column would \
                 take away the only thing naming what that credential is for",
            ),
            (
                "H3",
                format!(
                    "{is_array} AND {length} = 1 AND COALESCE({key}, '') <> 'kubeconfig' \
                     AND COALESCE({reference}, '') <> COALESCE(kubeconfig_credstore_ref, '')"
                ),
                "store one credential under a key this migration cannot assume, and its \
                 reference disagrees with the legacy column; resolving which is current \
                 needs the product's plugin, and a migration cannot call one",
            ),
        ] {
            if let Some(ids) = Self::ids_where(conn, &predicate).await? {
                return Err(DbErr::Custom(format!(
                    "qa-environments m20260903_000012 ({halt}): these environments {remedy}: \
                     {ids}. Re-save each environment's credentials, then re-run."
                )));
            }
        }

        // H5. `COUNT(DISTINCT product_id)` ignores NULLs on purpose -- see the
        // module doc; a productless row has no plugin to disagree with the
        // literal, so it must not push the count past one.
        let bucket_one = format!("{is_array} AND {length} = 0 AND NOT {LEGACY_EMPTY}");
        let spread = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                format!(
                    "SELECT COUNT(DISTINCT product_id) FROM qa_environments WHERE {bucket_one};"
                ),
            ))
            .await?;
        let products: i64 = spread.map_or(Ok(0), |row| row.try_get_by_index::<i64>(0))?;
        if products > 1 {
            let ids = Self::ids_where(conn, &bucket_one)
                .await?
                .unwrap_or_default();
            return Err(DbErr::Custom(format!(
                "qa-environments m20260903_000012 (H5): these environments store a credential \
                 with no key, and they span {products} products, so this migration cannot \
                 assume the key is 'kubeconfig' for all of them: {ids}. Re-save each \
                 environment's credentials -- which stores them under the key its own product \
                 plugin declares -- then re-run."
            )));
        }

        Ok(())
    }
}

/// Buckets 1 and 2, in one statement. Buckets 3, 4 and 5 fall outside the
/// `WHERE` and are left exactly as they are.
fn rederive_for(json: &Json) -> String {
    let Json {
        is_array,
        length,
        key,
        reference,
        rebuild,
    } = json;
    format!(
        "UPDATE qa_environments
            SET credentials = {rebuild}
          WHERE {is_array}
            AND NOT {LEGACY_EMPTY}
            AND (
                  {length} = 0
               OR ({length} = 1
                   AND COALESCE({key}, '') = 'kubeconfig'
                   AND COALESCE({reference}, '') <> kubeconfig_credstore_ref)
            );"
    )
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let json = json_for(manager.get_database_backend())?;

        Self::refuse_unrepairable_rows(conn, json).await?;
        conn.execute_unprepared(&rederive_for(json)).await?;

        let drops = DROPPED
            .iter()
            .map(|column| format!("ALTER TABLE qa_environments DROP COLUMN {column};"))
            .collect::<Vec<_>>()
            .join("\n");
        conn.execute_unprepared(&drops).await?;
        Ok(())
    }

    /// Best-effort. See the module doc for what it cannot recover.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let json = json_for(manager.get_database_backend())?;
        let timestamp = match manager.get_database_backend() {
            DatabaseBackend::Postgres => "TIMESTAMPTZ",
            DatabaseBackend::MySql => "TIMESTAMP",
            DatabaseBackend::Sqlite => "TEXT",
            other => {
                return Err(DbErr::Migration(format!(
                    "unsupported database backend: {other:?}"
                )));
            }
        };
        let json_column = match manager.get_database_backend() {
            DatabaseBackend::Postgres => "JSONB",
            DatabaseBackend::MySql => "JSON",
            DatabaseBackend::Sqlite => "TEXT",
            other => {
                return Err(DbErr::Migration(format!(
                    "unsupported database backend: {other:?}"
                )));
            }
        };

        conn.execute_unprepared(&format!(
            "ALTER TABLE qa_environments ADD COLUMN kubeconfig_credstore_ref TEXT NOT NULL \
             DEFAULT '';
             ALTER TABLE qa_environments ADD COLUMN vhp_base_url TEXT NULL;
             ALTER TABLE qa_environments ADD COLUMN observed_namespace TEXT NULL;
             ALTER TABLE qa_environments ADD COLUMN cluster_status TEXT NULL;
             ALTER TABLE qa_environments ADD COLUMN cluster_status_message TEXT NULL;
             ALTER TABLE qa_environments ADD COLUMN cluster_nodes {json_column} NULL;
             ALTER TABLE qa_environments ADD COLUMN cluster_namespace_count INTEGER NULL;
             ALTER TABLE qa_environments ADD COLUMN cluster_checked_at {timestamp} NULL;"
        ))
        .await?;

        let reference = json.reference;
        let is_array = json.is_array;
        conn.execute_unprepared(&format!(
            "UPDATE qa_environments
                SET kubeconfig_credstore_ref = COALESCE({reference}, '')
              WHERE {is_array} AND {} > 0;
             UPDATE qa_environments SET vhp_base_url = observed_base_url;",
            json.length
        ))
        .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260903_000012_drop_legacy_platform_columns_tests.rs"]
mod tests;
