//! `SeaORM` entity for the `qa_run_test_results` table.
//!
//! qa-runs owns the **authoritative** per-test rows; qa-insights builds its
//! own analytical `test_results` from the reconcile sweep's SDK reads
//! (`cpt-cf-qa-principle-async-insights` forbids the run path from reading them
//! back). Two tables, different owners, write patterns, and lifetimes.
//!
//! `(tenant_id, run_id, test_file, test_name)` is unique as an *application*
//! invariant maintained by delete-then-insert
//! (`manager/src/routes/runs.rs:1153-1185`), not by a database constraint — see
//! the migration for the second, independent reason (`InnoDB` key width).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_run_test_results")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    /// `""` when the runner reported no file, never `NULL` — the empty string
    /// is the single spelling of "absent" so the dedupe predicate is a plain
    /// equality rather than legacy's `COALESCE(test_file, '')`
    /// (`manager/src/routes/runs.rs:1154`).
    pub test_file: String,
    pub test_name: String,
    /// `PASSED` | `FAILED` | `ERROR` | `SKIPPED` | `PENDING` | `RUNNING` |
    /// `XFAIL` | `XPASS` — uppercase, and **not a closed set**. There is no
    /// `TestStatus` in the SDK because this row has no SDK counterpart; the
    /// migration's column comment is the definitive record, including which
    /// values feed which of the five run-level counters and why the mapper must
    /// *not* reject an unrecognized one. Read it before writing ingest.
    pub status: String,
    /// The runner's duration string **verbatim**, including extended forms
    /// like `85.06s (0:01:25)` (`manager/src/services/argo.rs:3279`). Do not
    /// "improve" this into a millisecond integer.
    pub duration: Option<String>,
    /// `ReportPortal` launch link.
    pub launch_id: Option<String>,
    pub jira_key: Option<String>,
    /// The pytest node identifier — `tests/test_x.py::TestC::test_m[param]` —
    /// and `""` when the executor reported none, never `NULL`, for the same
    /// single-spelling-of-absent reason as `test_file` above. Legacy's
    /// `test_case_results.nodeid`
    /// (`manager/migrations/001_initial.sql:257`), which is likewise
    /// `NOT NULL DEFAULT ''`.
    pub nodeid: String,
    /// The xfail/skip explanation the runner emitted, if any. Legacy
    /// `test_case_results.reason` (`001_initial.sql:261`). Nullable rather than
    /// `""`-defaulted because here absence is information: a case with no
    /// reason is a case the runner gave none for.
    pub reason: Option<String>,
    /// A per-case bug reference, **distinct from `jira_key`**: `jira_key` is
    /// the file-level link legacy keeps on `test_results`
    /// (`001_initial.sql:71`), `ticket` is the case-level one on
    /// `test_case_results` (`:262`). Analytics renders the latter, and only the
    /// latter, as `AnalyticsListItem::case_tickets`
    /// (`manager/src/routes/analytics.rs:132`). Do not collapse them.
    pub ticket: Option<String>,
    pub created_at: OffsetDateTime,
    /// Equal to `created_at` in practice: delete-then-insert replaces a row
    /// rather than updating it. Present as one of the house-style audit
    /// columns most tables carry, not because DESIGN mandates it centrally.
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
