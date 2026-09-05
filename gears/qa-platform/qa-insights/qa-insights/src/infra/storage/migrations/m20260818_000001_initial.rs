//! Initial schema: results, the JIRA bug registry, saved views, the three
//! configuration singletons, the notification tables, and the ingest
//! watermarks.
//!
//! Eleven tables. Ten are legacy tables (`manager/migrations/001_initial.sql`)
//! with tenancy and UUID keys added; `qa_ingest_watermarks` is new and exists
//! because this gear's projection is built by the reconcile sweep polling
//! qa-runs on its own cadence, which needs a persisted high-water mark to
//! resume from rather than re-scanning from the beginning on every restart
//! (design §4.4, Task 15).
//!
//! Follows the three shipped sibling gears' migration shape — a backend `match`
//! producing one `execute_unprepared` DDL blob per dialect. Column, index and
//! table order is kept identical across `POSTGRES_UP`, `MYSQL_UP` and
//! `SQLITE_UP`; the two parity tests at the bottom are what enforce that, not a
//! human diff.
//!
//! ## Nothing links these names to anything at compile time
//!
//! `SeaORM` entities are hand-written structs whose table and column names are
//! runtime strings, so a typo here — or in Task 11's `entity/*.rs` — compiles
//! cleanly and fails at query time. `cargo build` is therefore *no* evidence
//! about this file. The `#[cfg(test)] mod tests` below is: it runs the real
//! `Migrator` against an in-memory `SQLite` database and asserts the table
//! inventory, the index inventory, every index's column list *and sort
//! direction*, the column defaults, and — the part that matters most — which
//! inserts the unique indexes accept and which they reject.
//!
//! One tier further, behind the `integration` feature, the same `Migrator` runs
//! against a real Postgres container: `SQLite` accepts DDL that Postgres
//! rejects, and Postgres is the dialect this gear actually deploys, so
//! agreement between the blobs is not the same as either of them running.
//!
//! ## Two denormalizations, and why they are not sloppiness
//!
//! ### 1. Eight run columns are copied onto every result row
//!
//! Eight columns carrying seven run *properties* — `repo_id` and `plan_path` are
//! one identity split across two columns, which is why the plan text and
//! `qa_insights_sdk::TestResultRecord` both say one fewer than the column count.
//! Counted here as columns, because columns are what this file declares.
//!
//! `qa_test_results` carries `product_version`, `app_build`, `platform_id`,
//! `repo_id`, `plan_path`, `branch`, `run_finished_at` and `run_created_at`,
//! none of which legacy's `test_results` has (`001_initial.sql:65-74`, plus the
//! two `ALTER`s at `:166-167`).
//!
//! **This section said "seven"/"six" until Task 21b** added `run_created_at`,
//! which is the eighth column and the seventh property. Its own DDL comment
//! carries why `created_at` could not stand in for it.
//!
//! **`ingest_ordinal` is a ninth added column and is deliberately *not* one of
//! the eight.** It carries no run attribute: it is the row's position in the
//! batch that wrote it, and it exists to reproduce legacy's `t.id` SERIAL
//! tiebreak. Counting it here would make "denormalized from the run" mean two
//! different things. Its own DDL comment carries the argument.
//!
//! Legacy does not need the eight: every analytics query joins
//! `run_results` (for instance `analytics.rs:2450`,
//! `FROM test_results t JOIN run_results r ...`).
//!
//! Here that join is a **cross-gear call**, and
//! `cpt-cf-qa-principle-async-insights` forbids one on any hot path. So the
//! run's identity is copied onto the result row at ingest. It is safe because a
//! finished run's platform, product version, build and branch never change
//! afterwards — the same argument qa-runs records for the two version columns
//! being snapshots (`m20260813_000003_initial.rs`, "`app_version` and
//! `app_build` are snapshots, never re-derived"), which covers `app_build`
//! by name.
//!
//! The cost is real and worth naming: a run whose platform is *corrected* after
//! the fact leaves stale rows here, and nothing reconciles them. Legacy has the
//! same exposure through a different door — it snapshots `run_results.platform`
//! at launch and never re-reads it either.
//!
//! ### 2. `plan_key` materializes legacy's `COALESCE(plan_id, '')`
//!
//! Legacy's saved-view uniqueness is a **functional** index:
//!
//! ```sql
//! CREATE UNIQUE INDEX ux_analytics_saved_views_owner_scope_plan_name
//!     ON analytics_saved_views(owner_id, scope, COALESCE(plan_id, ''), name);
//! ```
//! (`001_initial.sql:194-195`.)
//!
//! `SQLite` and `MySQL` do not both support functional indexes, so the
//! coalesced value is materialized as its own column, `plan_key`, written by the
//! repository on every insert and update — `saved_views_sea_repo`, through the
//! single derivation `mapper::plan_key`. The nullable
//! `repo_id`/`plan_path` pair stays as the readable representation; `plan_key`
//! exists only to be indexed.
//!
//! **What the `COALESCE` actually buys, because the plan states it the weaker
//! way.** The task text says the `COALESCE` "is what makes a global view and a
//! plan-scoped view of the same name coexist". Those two rows already differ in
//! `scope` (`"all"` vs `"plan"`, `qa_insights_sdk::SavedViewScope`), so they
//! would coexist with or without it. The load-bearing effect is the opposite
//! one: in SQL a `NULL` never equals another `NULL`, so **without** the coalesce
//! a unique index over a nullable `plan_id` would let one owner create
//! unlimited global views all named "My View". The coalesce collapses every
//! global view onto the key `''` and makes them collide. Both properties are
//! asserted below, by insert, not by reading the index definition:
//! `a_global_and_a_plan_scoped_view_may_share_a_name` and
//! `two_global_views_of_one_owner_may_not_share_a_name`.
//!
//! #### A **stored generated column** would also work, and was declined
//!
//! **This is a trade, not a forced move, and an earlier draft of this section
//! read as though it were forced.** `GENERATED ALWAYS AS (...) STORED` plus a
//! unique index over the generated column is accepted *and* indexable by all
//! three engines this file targets — `PostgreSQL` 16, `MySQL` 8 and `SQLite`
//! 3.51 — so "no functional indexes" does not close the question the way the
//! paragraph above implies on its own. It would also delete obligation #2
//! below outright, along with the only silent-correctness failure mode in this
//! schema: a writer that forgets `plan_key` gets the `''` default and quietly
//! collides a plan-scoped view with the owner's global view of the same name.
//!
//! Declined anyway, for three reasons:
//!
//! 1. **Out of remit.** Task 10 specifies the coalesced value "materialized as
//!    its own column and **written by the repository**". Generating it in the
//!    database is a different design, and picking it here would decide for
//!    Task 12 rather than with it.
//! 2. **It needs three concatenation spellings.** The generation expression is
//!    `||` on Postgres and `SQLite` and `CONCAT(...)` on `MySQL`, so the three
//!    blobs would stop being transcriptions of one another. That is the same
//!    objection this file already raises against a `CHECK` constraint
//!    (obligation #2), and the identical-across-dialects discipline is what the
//!    two parity tests rest on.
//! 3. **It pushes a constraint onto Task 11.** A `STORED` column cannot be
//!    written, so the `SeaORM` entity would have to exclude it from every
//!    insert — a rule invisible in the entity struct and enforced by nothing.
//!
//! The cost of declining is obligation #2, and it is paid in a repository test,
//! not in the schema. Recorded here so a future reader can see the alternative
//! was weighed rather than missed.
//!
//! ## Every unique index is tenant-prefixed
//!
//! The rule, and the squatting/existence-oracle argument behind it, are stated
//! in full in DESIGN §3.7 and repeated for implementers in `qa-catalog`'s
//! migration, which is the one repetition DESIGN designates. Not repeated a
//! fourth time here.
//!
//! What is specific to *this* schema: legacy is single-tenant, so **every**
//! unique key here is wider than its legacy original, and two of them are the
//! sharp case the rule is about. `qa_jira_bugs.jira_key` is globally unique in
//! legacy (`001_initial.sql:80`) and a JIRA key is not a secret; without the
//! prefix, one tenant filing `VHP-1` would permanently deny it to every other
//! tenant and learn from the error whether someone else had filed it first. Same
//! shape for `qa_run_notifications`, whose key contains a `run_id` that is
//! unique on its own. `a_bug_key_is_unique_per_tenant_not_globally` and
//! `a_notification_claim_is_per_tenant_not_global` are the executable form.
//!
//! ## No foreign keys, in either direction
//!
//! `run_id`, `repo_id` and `platform_id` all name rows in *other gears'*
//! schemas — qa-runs, qa-catalog and qa-environments respectively — and DESIGN
//! §3.7 forbids cross-schema foreign keys. There is nothing inside this schema
//! for a child to reference either: `qa_test_results` and
//! `qa_test_case_results` are siblings, not parent and child. So unlike
//! qa-runs' migration this file declares no `REFERENCES` at all, and the
//! existence-oracle residual that qa-runs has to document does not arise.
//!
//! The obligation that replaces it: **ingest must not accept a `run_id` it has
//! not resolved.** Nothing in the database will catch a result row written
//! against a run that does not exist, or that belongs to another tenant.
//!
//! ## `MySQL` key-width budget (`InnoDB`, `utf8mb4`, 3072-byte limit)
//!
//! **Five of the eighteen indexes here exceed it, which is why `up()` refuses
//! `MySQL` outright** — see "So the `MySQL` arm refuses instead of executing"
//! below. Nothing here is shipped-anyway: the blob exists, and no code path
//! hands it to an engine. Here is the arithmetic (`VARCHAR(n)` costs `4n` bytes
//! under `utf8mb4`; `TIMESTAMP` costs 4):
//!
//! | Index | Bytes | Fits |
//! |---|---|---|
//! | `idx_qa_test_results_tenant_run` | 288 | yes |
//! | `idx_qa_test_results_tenant_test` | 144+4096+2048 = **6288** | no |
//! | `idx_qa_test_results_tenant_finished` | 148 | yes |
//! | `idx_qa_test_case_results_tenant_run` | 288 | yes |
//! | `idx_qa_test_case_results_tenant_run_file` | 144+144+4096 = **4384** | no |
//! | `idx_qa_test_case_results_tenant_status` | 208 | yes |
//! | `idx_qa_test_case_collect_target` | 144+144+2048+4096 = **6432** | no |
//! | `idx_qa_test_case_collect_tenant_repo_branch` | 2336 | yes |
//! | `idx_qa_analytics_saved_views_unique` | 144+144+256+4244+1020 = **5808** | no |
//! | `idx_qa_jira_bugs_tenant_key` | 400 | yes |
//! | `idx_qa_jira_bugs_tenant_test_plan` | 144+2048+144+4096 = **6432** | no |
//! | `idx_qa_jira_bugs_tenant_status` | 400 | yes |
//! | the four singleton `(tenant_id)` uniques | 144 each | yes |
//! | `idx_qa_run_notifications_claim` | 800 | yes |
//! | `idx_qa_notification_log_tenant_created` | 148 | yes |
//!
//! Every overflowing index contains a path or a test name — `test_file`
//! `VARCHAR(1024)`, `plan_path` `VARCHAR(1024)`, `test_name` `VARCHAR(512)`.
//! `144 + 4*1024 = 4240` already exceeds 3072, so **no tenant-prefixed index
//! over a 1024-char path can fit on `InnoDB` at all**; the only ways out are
//! prefix lengths, a digest column, or truncating the data.
//!
//! Why none of those was taken:
//!
//! * **Narrowing the columns truncates real data.** A `plan.yaml` path or a
//!   parametrized `test_file` is genuinely long, and `qa-runs` — the producer
//!   of every one of these values — declares them `VARCHAR(1024)` and
//!   `VARCHAR(512)`. Narrowing here would silently drop rows qa-runs accepted.
//! * **Prefix lengths (`test_file(255)`) change the semantics — but only for
//!   two of the five.** `idx_qa_test_case_collect_target` and
//!   `idx_qa_analytics_saved_views_unique` are the over-limit indexes that
//!   *enforce* something, and there a prefix is not a formatting difference:
//!   two targets sharing a 255-char prefix would collide on `MySQL` and not
//!   elsewhere. The other three — `idx_qa_test_results_tenant_test`,
//!   `idx_qa_test_case_results_tenant_run_file` and
//!   `idx_qa_jira_bugs_tenant_test_plan` — are plain `CREATE INDEX`, where a
//!   prefix length is a pure selectivity choice with no semantic consequence,
//!   so **prefix lengths remain available for those three**. Whichever way,
//!   applying one now would make the three dialects declare different indexes,
//!   which is exactly what `every_dialect_declares_the_same_indexes` exists to
//!   forbid.
//!
//!   (Corrected 2026-08-20: this bullet said "the three *unique* indexes" and
//!   offered the strongest argument for five cases when it covers two. Counted,
//!   not read: of the five over-limit indexes exactly two are `UNIQUE`.)
//! * **A digest column** (`plan_key` as a SHA-256 hex) fits, but buys a hash
//!   dependency and an unreadable column to solve a problem no deployment has.
//!
//! ### The sibling that faced this and *could* decline
//!
//! `qa-runs` hit the identical 6432-byte tuple, with a comment saying so: it
//! declined a unique index on `(tenant_id, run_id, test_file, test_name)`
//! because the source system deduplicates by delete-then-insert — making
//! uniqueness an application invariant — **and** because "that tuple is 36*4 +
//! 36*4 + 1024*4 + 512*4 = 6432 bytes under utf8mb4, well past `InnoDB`'s
//! 3072-byte limit" (`qa-runs/.../m20260813_000003_initial.rs:475-483`).
//!
//! **On that tuple the two gears agree, not differ**: this schema declines the
//! same index for the same first reason, which is obligation 5 below, pinned by
//! `a_repeated_result_tuple_is_accepted`. (Corrected 2026-08-20; this paragraph
//! previously said qa-runs "went the other way", and a reader who checked the
//! citation would have found the opposite of the claim.)
//!
//! The precedent is cited because of where the two gears *do* diverge, and it
//! *strengthens* the argument rather than undermining it. Declining is the
//! right answer when the index enforces nothing anybody needs, and every
//! over-limit index qa-runs faced was in that position. Neither of the two
//! over-limit **unique** indexes here is: `idx_qa_test_case_collect_target` is
//! the conflict target the collect upsert runs against, and
//! `idx_qa_analytics_saved_views_unique` *is* D4. There is nothing to decline.
//!
//! ### So the `MySQL` arm refuses instead of executing
//!
//! **Measured, not assumed:** of the eleven `MYSQL_UP` blobs across the four
//! qa-platform gears, ten execute cleanly against `MySQL` 8 and only this one
//! fails — `ERROR 1071 (42000): Specified key was too long; max key length is
//! 3072 bytes`, on the first table. So `MYSQL_UP` is **not** "a declaration
//! mirror like the siblings'": theirs run, this one does not, and describing it
//! as equivalent was materially untrue.
//!
//! `up()`'s `MySql` arm therefore returns a `DbErr` naming this section, rather
//! than handing the engine DDL that will die on `ERROR 1071` with no gear name
//! and no column in the message. The constant stays — the two parity tests read
//! it, and it is the declaration of what a `MySQL` schema *would* be — but
//! nothing will execute it. `the_mysql_arm_refuses_rather_than_executing`
//! pins that.
//!
//! This costs nothing today: the gear links `toolkit-db` with
//! `features = ["sqlite", "pg"]` (see `Cargo.toml`), as all three siblings do,
//! so no build in this workspace can reach the arm. The unit tier is `SQLite`
//! and the `integration` tier is a Postgres container.
//!
//! So the residual is precisely: **whoever adds a `MySQL` backend gets a
//! refusal pointing at this section instead of a cryptic engine error.** Three
//! of the five over-limit indexes are then a mechanical fix (a prefix length on
//! a non-unique index costs nothing but selectivity); **two** need a real
//! decision — prefix lengths with the collision risk stated above, or a digest
//! column. Recorded rather than papered over: an undocumented residual is a
//! finding by this codebase's own standard.
//!
//! ## Legacy's four `test_results` indexes, and which of them survive
//!
//! Two are not reproduced, deliberately; the other two are, one of them under a
//! different name. `test_results` is the only legacy table whose index set this
//! schema does not carry across whole, which is why it is the only one with an
//! entry here.
//!
//! Not reproduced:
//!
//! * `idx_test_results_name ON test_results(test_name)` (`001_initial.sql:76`).
//!   `idx_qa_test_results_tenant_test` leads with `test_file`, so a
//!   `test_name`-only lookup is not served. Checked before dropping it: no
//!   legacy query filters `test_results` on `test_name` alone — the analytics
//!   statements group by it (`analytics.rs:2441`, `:2536`) but always under a
//!   run/plan/version predicate. If a Task 20+ query turns out to need it, it is
//!   an additive index, not a redesign.
//! * `idx_test_results_run_status_file ON test_results(run_id, status,
//!   test_file)` (`:181`). `idx_qa_test_results_tenant_run` is its prefix under
//!   tenancy; the extra two columns are a covering optimisation for a query
//!   nothing has written yet.
//!
//! Legacy's fourth index on that table, `idx_test_results_file ON
//! test_results(test_file)` (`:177`), **is** reproduced and belongs in the
//! accounting rather than in the list above: it is served by
//! `idx_qa_test_results_tenant_test`'s `(tenant_id, test_file)` prefix. That is
//! the distinction from the two entries above, whose access patterns no index
//! here covers at all. Together these four are every index legacy declares on
//! `test_results`.
//!
//! ## One legacy column is deliberately missing: `test_results.logs`
//!
//! Legacy's `test_results` has `logs TEXT` (`001_initial.sql:72`) and writes it
//! on every insert (`manager/src/services/argo.rs:2603`). There is no column
//! for it here and **that is a real parity gap, not a tidy-up.** This gear's
//! only source of test outcomes is qa-runs, and `qa_runs_sdk::RunTestResult`
//! carries no per-test log slice — qa-runs keeps whole-run `raw_logs` instead.
//! A column no writer could ever fill is the "field nothing can set" shape this
//! subsystem has already rejected once. Restoring it needs a per-test log
//! excerpt on qa-runs' event and SDK first, at which point it is one additive
//! column here. `qa_insights_sdk::TestResultRecord` records the same gap on the
//! contract side.
//!
//! ## Obligations this schema hands to later tasks
//!
//! Five things the DDL cannot enforce, collected here because each is invisible
//! at the point where it would be violated:
//!
//! 1. **Resolve every caller-supplied `run_id` and `repo_id` under the caller's
//!    own scope before writing a row that uses it.** There are no foreign keys
//!    (see above), so nothing in the database will catch a result row written
//!    against a run that does not exist or belongs to another tenant.
//! 2. **Write `plan_key` on every `qa_analytics_saved_views` insert and
//!    update.** Nothing checks that it agrees with `repo_id`/`plan_path`; a
//!    writer that forgets it gets the `''` default, which silently makes a
//!    plan-scoped view collide with the owner's global view of the same name.
//!    A `CHECK` constraint was considered and declined — string concatenation
//!    is spelled three different ways across these dialects, and the check
//!    would be a fourth thing to keep in parity.
//! 3. **The `GET` surfaces for `qa_jira_config` and `qa_notification_config`
//!    return the credstore reference, never the material.** See the next
//!    section.
//! 4. **`claim_notification` must read a unique violation on
//!    `idx_qa_run_notifications_claim` as "already sent", not as an error.**
//!    The failed insert *is* the dedupe answer; surfacing it as a `Database`
//!    error would turn every duplicate into a 500.
//! 5. **Result-row ingest must be idempotent by delete-then-insert on
//!    `(run_id, test_file, test_name)`.** There is deliberately **no** unique
//!    index on that tuple — legacy dedupes the same way
//!    (`DELETE FROM test_results WHERE run_id = $1 AND test_name = $2 AND
//!    COALESCE(test_file, '') = $3` then an unconditional `INSERT`,
//!    `manager/src/routes/runs.rs:1153-1185`), qa-runs records the identical
//!    obligation at its own index, and design §4.4 states it for this gear. So
//!    the database will happily store the same outcome twice and every count
//!    the analytics surface computes will be wrong, with nothing failing.
//!
//!    This obligation belongs *here* and not only in the repository because
//!    `qa_ingest_watermarks` — declared in this file — is what makes re-ingest
//!    **routine**: the reconcile poller replays a window of finished runs on
//!    every pass, so a non-idempotent writer double-counts on the happy path,
//!    not just after a failure.
//!
//!    `a_repeated_result_tuple_is_accepted` pins the absence of the index, so
//!    that "helpfully" adding one — which would break ingest *and* add a sixth
//!    `MySQL` failure — turns a test red instead of shipping.
//!
//! ## Two columns hold credential-equivalent material
//!
//! `qa_jira_config.api_token_credstore_ref` and
//! `qa_notification_config.slack_webhook_credstore_ref`. Legacy stores both
//! verbatim (`JiraConfig::api_token`, `NotificationsConfig::slack_webhook_url`,
//! `manager/src/models.rs:678-685` and `:1338-1362`). **This is the one place
//! the port deliberately diverges from legacy behaviour**, and both column
//! comments say so at the point of use.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
-- File-level test outcomes. Legacy `test_results` (001_initial.sql:65-74),
-- widened by the two ALTERs at :166-167 (`test_file`, `duration`), by the eight
-- denormalized run columns this gear needs -- see the module header, 'Eight run
-- columns are copied onto every result row' -- and by `ingest_ordinal`, which is
-- not one of them and carries its own argument below.
CREATE TABLE IF NOT EXISTS qa_test_results (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- The qa-runs run. No foreign key: cross-gear, see the module header.
    run_id UUID NOT NULL,
    -- Legacy's column is nullable and its readers pay for it with
    -- `COALESCE(test_file, '')` (`manager/src/routes/runs.rs:1154`).
    -- `qa_runs_sdk::RunTestResult::test_file` already collapsed absent to `''`
    -- so that 'absent' has exactly one spelling; this keeps that.
    test_file VARCHAR(1024) NOT NULL DEFAULT '',
    test_name VARCHAR(512) NOT NULL,
    -- Open, uppercase producer set -- PASSED | FAILED | ERROR | SKIPPED |
    -- PENDING | RUNNING | XFAIL | XPASS today. Deliberately not an enum and
    -- deliberately not validated: ingest must not fail closed on a spelling the
    -- runner invents. `qa_insights_sdk::TestResultRecord::status` is the
    -- contract-side statement of the same rule.
    status VARCHAR(16) NOT NULL,
    -- The runner's duration text verbatim ('1.23s'), not a number: legacy
    -- stores TEXT (:167) and the UI renders it unparsed.
    duration VARCHAR(64) NULL,
    -- ReportPortal launch link.
    launch_id VARCHAR(255) NULL,
    -- The file-level bug reference as the runner reported it. Distinct from
    -- `qa_jira_bugs`, which is this gear's own registry.
    jira_key VARCHAR(64) NULL,
    -- Denormalized from the run (legacy `run_results.app_version`, :56).
    product_version VARCHAR(255) NULL,
    -- Denormalized from `qa_runs.app_build` (`m20260813_000003_initial.rs:219`),
    -- which is legacy's `run_results.app_build` (added by the ALTER at :149).
    --
    -- **The projection to `product_version`'s filter, and that is why both are
    -- here.** Legacy filters analytics on `r.app_version`
    -- (`manager/src/routes/analytics.rs:961`, `:988`) and *groups* on
    -- `r.app_build` (`build_last_run_build_distribution`; `ExecRow::build` is
    -- assigned from it at `:1032-1033`). The two are different values and only
    -- one of them was copied over at first; a build distribution cannot be
    -- computed from a version filter, so the second is not redundant with the
    -- first.
    --
    -- Nullable, and NULL means the run named no build. Legacy's 'unknown'
    -- fallback is applied at the *consumer* -- `normalize_optional(...)` then
    -- `.unwrap_or_else(...)` at `:1032-1033` -- so this column and
    -- `domain::analytics::ExecRow::build` both keep the absence, and Task 24's
    -- core substitutes the label.
    app_build VARCHAR(255) NULL,
    -- Denormalized from the run. A UUID, not legacy's platform *name*: legacy's
    -- TEXT is an artifact of a system that had no platform ids until the Phase A
    -- backfill added `platforms_meta.id` (:218-228).
    platform_id UUID NULL,
    -- Denormalized from the run: the repository half of the plan identity.
    -- NULL for a target with no plan (a custom-plan or collect run). There is no
    -- plan UUID in this port -- legacy's own `plan_id` is a path-derived slug
    -- (`manager/src/services/plans.rs:789-801`, `compose_repo_plan_id`), so the
    -- key is the (repo_id, plan_path) pair `qa-insights-sdk` ships.
    repo_id UUID NULL,
    -- Denormalized from the run: the `plan.yaml` path half of the plan identity.
    plan_path VARCHAR(1024) NULL,
    -- Denormalized from the run, already resolved. Legacy's analytics branch
    -- filter reads `source_ref` and falls back to `test_version`
    -- (`manager/src/routes/analytics.rs:24-27`); the fallback is applied at
    -- ingest so the stored value is the effective one.
    branch VARCHAR(512) NULL,
    -- Denormalized from the run. NULL while the run is still going: legacy
    -- writes RUNNING/PENDING rows too (`manager/src/services/argo.rs:2603`).
    run_finished_at TIMESTAMPTZ NULL,
    -- Denormalized from `qa_runs.created_at`, and **the fallback half of
    -- legacy's `COALESCE(rr.finished_at, rr.created_at)`**.
    --
    -- Added by Task 21b for the same class of reason `app_build` was added by
    -- Task 12: a column that arrives one task later means writing a mapper and
    -- immediately rewriting it, and every row written in between needs a
    -- backfill. Nothing is deployed, so amending this migration is honest.
    --
    -- **Why `created_at` could not stand in for it.** Legacy's fallback is the
    -- *run's* creation instant, which is fixed for the life of the run. This
    -- table's `created_at` is the instant the *row* was written, and ingest is
    -- delete-then-insert per run, re-run every time the reconcile sweep or a
    -- rebuild re-reads it
    -- (`domain::service::ingest::IngestService::write_run_projection`), so
    -- it is reset to `now` every time a result lands. Windowing on it made
    -- an unfinished run's rows permanently 'within the last 24 hours' for
    -- as long as the run lasted -- the dashboard
    -- KPI pair counts unfinished runs by design (no phase predicate,
    -- `manager/src/routes/dashboard.rs:317-348`), so a run stuck for three days
    -- kept every FAILED row in `failed_24h_count` and every card pinned at the
    -- top of `failed_recent`. That is a wrong rendered number, not a rounding.
    --
    -- Nullable, so 'the run reported no creation instant' stays distinguishable
    -- from an epoch. `qa_runs_sdk::Run::created_at` is non-optional today, so
    -- the ingest path always fills it; the column does not assume that forever.
    run_created_at TIMESTAMPTZ NULL,
    -- **The row's position within the batch that wrote it**, and the port of
    -- legacy's `test_results.id` SERIAL used as a tiebreak.
    --
    -- Not a run attribute and not an analytics projection: it is a persistence
    -- *ordering* detail, which is why it is absent from
    -- `domain::analytics::ExecRow` and from `qa_insights_sdk::TestResultRecord`.
    --
    -- Legacy orders analytics rows `COALESCE(finished_at, created_at) DESC,
    -- t.id DESC` (`manager/src/routes/analytics.rs:971`, `:1001`) and re-sorts
    -- in memory with a *stable* sort (`:1042`), so the SQL tiebreak survives;
    -- `t.id` is a SERIAL and the bulk writer inserts in parse order
    -- (`manager/src/services/argo.rs:2598-2615`). Legacy's winner among rows of
    -- one run sharing a file is therefore the **last row the parser produced**,
    -- which `latest_per_test_snapshot`'s first-wins loop consumes (`:1615-1632`).
    -- A random UUID primary key cannot reproduce that; this column can.
    --
    -- NOT NULL DEFAULT 0 rather than nullable, deliberately: `ORDER BY ... DESC`
    -- puts NULLs first on Postgres and last on SQLite, so a nullable ordinal
    -- would make the tiebreak dialect-dependent - the one thing it exists to
    -- stop. The default is safe here in a way `plan_key`'s is not, because the
    -- writer is an exhaustive `ActiveModel` literal: a forgotten field is a
    -- compile error, not a silent 0.
    ingest_ordinal INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_test_results_tenant_run ON qa_test_results(tenant_id, run_id);
CREATE INDEX IF NOT EXISTS idx_qa_test_results_tenant_test ON qa_test_results(tenant_id, test_file, test_name);
CREATE INDEX IF NOT EXISTS idx_qa_test_results_tenant_finished ON qa_test_results(tenant_id, run_finished_at DESC);

-- Per-function test case outcomes. Legacy `test_case_results`
-- (001_initial.sql:253-264), whose own comment states the purpose: 'Per-function
-- test case outcomes (xfail/xpass/skip/pass/fail) parsed from the runner's
-- TEST_CASE markers. One row per test function per run, grouped under a file
-- (test_file). Lets analytics aggregate per-case, not just per-file.' (:250-252).
--
-- Not denormalized the way `qa_test_results` is: every case-level aggregate
-- legacy computes goes through the file-level table first, so the run columns
-- are not needed on this row and a second copy would be a second thing to keep
-- true.
CREATE TABLE IF NOT EXISTS qa_test_case_results (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    run_id UUID NOT NULL,
    -- NOT NULL with no default, exactly as legacy (:256). Unlike
    -- `qa_test_results.test_file` this one has never been nullable, so there is
    -- no absent case to collapse.
    test_file VARCHAR(1024) NOT NULL,
    -- pytest's fully-qualified node id. Legacy default '' (:257).
    nodeid VARCHAR(1024) NOT NULL DEFAULT '',
    name VARCHAR(512) NOT NULL,
    status VARCHAR(16) NOT NULL,
    duration VARCHAR(64) NULL,
    -- Why the case was skipped or xfailed, as the marker reported it.
    reason TEXT NULL,
    -- Case-level bug reference from the TEST_CASE marker; distinct from
    -- `qa_test_results.jira_key`, which is file-level.
    ticket VARCHAR(64) NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_test_case_results_tenant_run ON qa_test_case_results(tenant_id, run_id);
CREATE INDEX IF NOT EXISTS idx_qa_test_case_results_tenant_run_file ON qa_test_case_results(tenant_id, run_id, test_file);
CREATE INDEX IF NOT EXISTS idx_qa_test_case_results_tenant_status ON qa_test_case_results(tenant_id, status);

-- Exact expected test-case counts per file. Legacy `test_case_collect`
-- (001_initial.sql:272-279), whose comment states the purpose: 'Exact expected
-- test-case counts per file, produced by the collect job (pytest --collect-only,
-- parametrize expanded). Keyed by repo + branch + file so analytics can show an
-- exact 'expected cases' number without a full run.' (:269-271).
CREATE TABLE IF NOT EXISTS qa_test_case_collect (
    -- Legacy's key is the composite PRIMARY KEY (repo_id, branch, test_file)
    -- (:278). Here that becomes the unique index below, because the platform's
    -- `id UUID` primary key is non-negotiable. The upsert targets the unique
    -- index, so the dedupe behaviour is unchanged.
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    repo_id UUID NOT NULL,
    branch VARCHAR(512) NOT NULL,
    test_file VARCHAR(1024) NOT NULL,
    case_count INTEGER NOT NULL DEFAULT 0,
    -- When the collect job produced this count. Distinct from `updated_at`,
    -- which moves on any write; legacy has only this one (:277). Legacy also
    -- gives it `DEFAULT NOW()`; that half is not ported, because every
    -- timestamp in this schema is written by the application -- the platform
    -- convention the three sibling gears follow, and what keeps a row's clock
    -- the service's rather than the database server's.
    collected_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_test_case_collect_target ON qa_test_case_collect(tenant_id, repo_id, branch, test_file);
CREATE INDEX IF NOT EXISTS idx_qa_test_case_collect_tenant_repo_branch ON qa_test_case_collect(tenant_id, repo_id, branch);

-- Stored analytics filter sets. Legacy `analytics_saved_views`
-- (001_initial.sql:183-192).
CREATE TABLE IF NOT EXISTS qa_analytics_saved_views (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- Legacy's is a TEXT identifier lifted from a request header
    -- (`analytics_owner_id`); here it is the Fabric principal from
    -- `SecurityContext`.
    owner_id UUID NOT NULL,
    -- 'all' | 'plan'. Legacy validates against exactly those two spellings at
    -- the boundary and 400s on anything else
    -- (`manager/src/routes/analytics.rs:2104-2113`);
    -- `qa_insights_sdk::SavedViewScope` is the closed enum that preserves it.
    -- The column stays wide because the DDL is not the validator.
    scope VARCHAR(64) NOT NULL,
    -- The plan identity, nullable and readable. Present exactly when
    -- scope = 'plan'.
    repo_id UUID NULL,
    plan_path VARCHAR(1024) NULL,
    -- **Materialized COALESCE.** Legacy's unique index is functional --
    -- `COALESCE(plan_id, '')` (:194-195) -- and SQLite and MySQL do not both
    -- support functional indexes, so the coalesced value is its own column.
    -- Written by the repository on every insert and update: '' when
    -- repo_id/plan_path are NULL, otherwise '<repo_id>/<plan_path>'.
    -- 36 + 1 + 1024 = 1061. See the module header for what the coalesce buys.
    plan_key VARCHAR(1061) NOT NULL DEFAULT '',
    name VARCHAR(255) NOT NULL,
    -- Opaque to this gear. Legacy types it `serde_json::Value` and never
    -- inspects it: all six statements that touch the column bind or return it
    -- whole (`analytics.rs:542`, `:557`, `:606`, `:626`, `:664`, `:687`).
    query_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_analytics_saved_views_unique ON qa_analytics_saved_views(tenant_id, owner_id, scope, plan_key, name);

-- The JIRA bug registry. Legacy `jira_bugs` (001_initial.sql:78-89).
--
-- **There is no `auto_rerun` column, and that is checked, not assumed.** An
-- earlier draft of the plan and DESIGN 3.7's original one-line list both gave
-- this table one. Legacy's CREATE TABLE stops at `resolved_at` (:88) and
-- `manager/migrations/` holds exactly one file, so nothing adds the column
-- later either. Auto-rerun is a *global* switch --
-- `JiraPollerConfig::auto_rerun_on_resolve`, which `qa_jira_poller_config`
-- below carries -- and D8's point is that the rerun gate is the poller's, not
-- the bug's. If per-bug control is ever wanted it is an additive nullable
-- column plus an SDK field.
CREATE TABLE IF NOT EXISTS qa_jira_bugs (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    jira_key VARCHAR(64) NOT NULL,
    test_name VARCHAR(512) NOT NULL,
    -- Legacy's single `plan_id TEXT NOT NULL` (:82) split in two. That column is
    -- a path-derived slug, not a UUID (`compose_repo_plan_id`,
    -- `manager/src/services/plans.rs:789-801`), and there is no plan UUID in
    -- this port, so the identity is the (repo_id, plan_path) pair.
    repo_id UUID NOT NULL,
    plan_path VARCHAR(1024) NOT NULL,
    app_version VARCHAR(255) NULL,
    -- Legacy is `platform TEXT` (:84), the platform *name*; see the note on
    -- `qa_test_results.platform_id` for why this port stores the id.
    platform_id UUID NULL,
    -- Free JIRA workflow text, not a closed set. Legacy default 'Open' (:85).
    status VARCHAR(64) NOT NULL DEFAULT 'Open',
    summary TEXT NOT NULL,
    -- Set when the poller observes a resolved transition. Recorded whether or
    -- not auto-rerun is on: `resolve_bug` runs at
    -- `manager/src/services/jira_poller.rs:59`, *before* the
    -- `auto_rerun_on_resolve` gate at `:61-63`.
    resolved_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_jira_bugs_tenant_key ON qa_jira_bugs(tenant_id, jira_key);
CREATE INDEX IF NOT EXISTS idx_qa_jira_bugs_tenant_test_plan ON qa_jira_bugs(tenant_id, test_name, repo_id, plan_path);
CREATE INDEX IF NOT EXISTS idx_qa_jira_bugs_tenant_status ON qa_jira_bugs(tenant_id, status);

-- JIRA connection settings. Legacy `JiraConfig` (`manager/src/models.rs:678-685`,
-- six fields), stored as a row in the untyped key/value `settings` table
-- (001_initial.sql:93) under the key `jira`. D6 replaces the JSON bag with typed
-- columns: same fields, same GET/PUT surface, one row per tenant instead of one
-- row per installation.
CREATE TABLE IF NOT EXISTS qa_jira_config (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    url VARCHAR(1024) NOT NULL DEFAULT '',
    project_key VARCHAR(64) NOT NULL DEFAULT '',
    -- The account the token belongs to; JIRA basic auth is `email:token`.
    email VARCHAR(320) NOT NULL DEFAULT '',
    -- **Divergence from legacy, on purpose.** Legacy stores the bearer token
    -- itself (`JiraConfig::api_token`). This column holds a credential-store
    -- reference and never the material. The rename is the guard: a `String`
    -- holding an actual token cannot be assigned to a field called
    -- `api_token_credstore_ref` by accident. The GET surface returns the
    -- reference, never the token -- an obligation on Task 32's handler that this
    -- schema cannot enforce.
    api_token_credstore_ref VARCHAR(512) NOT NULL DEFAULT '',
    -- NULL uses the project's default issue type.
    issue_type VARCHAR(64) NULL,
    -- A missing or disabled config short-circuits the poller silently
    -- (`manager/src/services/jira_poller.rs:40-43`).
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_jira_config_tenant ON qa_jira_config(tenant_id);

-- JIRA poller cadence and the auto-rerun switch. Legacy `JiraPollerConfig`
-- (`manager/src/models.rs:1417-1420`, two fields), `settings` key `jira_poller`.
-- Column defaults are legacy's `Default` verbatim (`models.rs:1422-1429`).
CREATE TABLE IF NOT EXISTS qa_jira_poller_config (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- Legacy clamps with `.max(1)` and the loop sleeps first
    -- (`manager/src/services/jira_poller.rs:26-27`). BIGINT because the SDK
    -- types it `u64`; the clamp stays in the domain, not the DDL.
    poll_interval_seconds BIGINT NOT NULL DEFAULT 300,
    -- Global gate on relaunching a run when a bug resolves. Legacy needs *both*
    -- this and a new build (`jira_poller.rs:61-70`, D8). The resolution itself
    -- is recorded either way (`:59`, before the gate), so turning this off does
    -- not stop bugs closing.
    auto_rerun_on_resolve BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_jira_poller_config_tenant ON qa_jira_poller_config(tenant_id);

-- Notification routing and egress settings. Legacy `NotificationsConfig`
-- (`manager/src/models.rs:1338-1362`), `settings` key `notifications`.
--
-- **Fifteen fields, fifteen columns**, counted by parsing the struct rather than
-- by reading it. The design spec's Finding 5 says 'sixteen' and then enumerates
-- fifteen; the enumeration is right and the count is a typo. Every column
-- default below is legacy's `Default` verbatim (`models.rs:1364-1384`).
CREATE TABLE IF NOT EXISTS qa_notification_config (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- **Divergence from legacy, on purpose** -- the second of two in this
    -- schema, and decided 2026-08-20. Legacy stores the incoming-webhook URL
    -- verbatim (`NotificationsConfig::slack_webhook_url`). A Slack webhook URL
    -- is not an address that happens to be secret: possession of it *is* the
    -- authorization to post as the app, exactly like the JIRA token, so it gets
    -- the same treatment and the same rename-not-retype. The GET surface returns
    -- the reference, never the URL.
    slack_webhook_credstore_ref VARCHAR(512) NOT NULL DEFAULT '',
    slack_channel VARCHAR(255) NOT NULL DEFAULT '',
    -- Base URL used to build run links in rendered messages.
    manager_ui_base_url VARCHAR(1024) NOT NULL DEFAULT '',
    slack_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    -- The only notify gate legacy starts ON.
    notify_on_failure BOOLEAN NOT NULL DEFAULT TRUE,
    notify_on_success BOOLEAN NOT NULL DEFAULT FALSE,
    notify_on_schedule_completion BOOLEAN NOT NULL DEFAULT FALSE,
    scheduled_run_slack_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    -- Six Block Kit templates (pending, in_progress, succeeded, failed, error,
    -- skipped), each seven fields. A JSON document rather than 42 columns: the
    -- set is written and read whole, and nothing filters on it.
    scheduled_run_slack_templates JSONB NOT NULL DEFAULT '{}',
    -- Off by default because a busy platform produces a lot of queue events
    -- (legacy's own comment, `models.rs:1348-1356`). Note what has no toggle at
    -- all: the companion `expired` event, 'the one that stops a run
    -- disappearing silently'. Its absence from this table is the port of that.
    run_queue_queued_slack_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    email_smtp_host VARCHAR(255) NOT NULL DEFAULT '',
    email_smtp_port INTEGER NOT NULL DEFAULT 587,
    email_from VARCHAR(320) NOT NULL DEFAULT '',
    -- One string, not a list: legacy stores the operator's recipient line as
    -- typed and the mailer splits it. Kept as stored so a settings round-trip
    -- cannot reformat it.
    email_recipients TEXT NOT NULL DEFAULT '',
    email_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_notification_config_tenant ON qa_notification_config(tenant_id);

-- Notification dedupe claims. Legacy `run_notifications`
-- (001_initial.sql:197-203), whose PRIMARY KEY (workflow_name,
-- notification_kind, event_type) (:202) *is* the dedupe mechanism, not an
-- incidental key: an insert that violates it is the signal that this
-- notification already went out.
--
-- **This table has no SDK contract type, deliberately.** Nothing outside this
-- gear ever reads a dedupe row; the claim is a repository concern and Task 12
-- exposes it as `claim_notification(...) -> bool`. Adding a `RunNotification`
-- model to `qa-insights-sdk` would publish an internal idempotency detail as
-- contract and oblige the gear to keep its shape.
CREATE TABLE IF NOT EXISTS qa_run_notifications (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- Legacy keys on `workflow_name`; the gear's equivalent identity is the run.
    run_id UUID NOT NULL,
    notification_kind VARCHAR(64) NOT NULL,
    event_type VARCHAR(64) NOT NULL,
    sent_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_run_notifications_claim ON qa_run_notifications(tenant_id, run_id, notification_kind, event_type);

-- Notification audit trail. Legacy `notification_log` (001_initial.sql:205-213),
-- surfaced at `GET /api/settings/notifications/log`. Egress failures are
-- recorded here and never propagated to a caller (design 4.8), which makes this
-- the only place an operator can see that Slack or SMTP is broken.
CREATE TABLE IF NOT EXISTS qa_notification_log (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- Legacy's `workflow_name TEXT NOT NULL` (:208) becomes a nullable run id:
    -- some logged attempts belong to no run at all (a settings /test send),
    -- which legacy records as the empty string.
    run_id UUID NULL,
    -- 'slack' | 'email' -- the egress that was tried.
    channel VARCHAR(32) NOT NULL,
    -- NOT NULL DEFAULT '' exactly as legacy (:210): an unattributed entry is ''
    -- rather than absent.
    event_type VARCHAR(64) NOT NULL DEFAULT '',
    outcome VARCHAR(32) NOT NULL,
    -- Failure detail, '' on success (:212).
    detail TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
-- Legacy `ix_notification_log_created ON notification_log(created_at DESC)`
-- (:215-216), tenant-prefixed.
CREATE INDEX IF NOT EXISTS idx_qa_notification_log_tenant_created ON qa_notification_log(tenant_id, created_at DESC);

-- **New in this port; no legacy original.** This gear's projection is built
-- by a periodic reconcile that polls qa-runs for runs finished since a
-- persisted high-water mark, and by a sweep that retires stale in-progress
-- rows (design 4.4, Task 15). Both marks live here, one row per tenant, because
-- a mark held in memory would restart at zero on every deploy -- which is the
-- failure this table exists to prevent.
CREATE TABLE IF NOT EXISTS qa_ingest_watermarks (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- NULL means 'never reconciled': the first pass uses the configured lookback
    -- window instead. Distinct from the epoch, which would mean 'reconciled up
    -- to 1970' and would make the first pass read everything.
    last_reconciled_finished_at TIMESTAMPTZ NULL,
    last_swept_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_ingest_watermarks_tenant ON qa_ingest_watermarks(tenant_id);
";

/// The `MySQL` schema this gear *would* have, and **which `up()` refuses to
/// execute**.
///
/// Five of its indexes exceed `InnoDB`'s 3072-byte key limit, so `MySQL` 8
/// rejects the very first `CREATE TABLE` with `ERROR 1071 (42000)`. Kept
/// because the two parity tests read it and because it is the declaration of
/// what the schema would be; see this module's header,
/// "`MySQL` key-width budget (`InnoDB`, `utf8mb4`, 3072-byte limit)", for the
/// per-index arithmetic and for what a real `MySQL` port would have to decide.
///
/// Unread outside `#[cfg(test)]` for exactly that reason — `up()` no longer
/// names it. `expect` rather than `allow` so that the day something *does* read
/// it in a shipped build, this attribute is what goes red.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read only by the two dialect-parity tests; up() refuses the MySql arm"
    )
)]
const MYSQL_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_test_results (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    run_id VARCHAR(36) NOT NULL,
    test_file VARCHAR(1024) NOT NULL DEFAULT '',
    test_name VARCHAR(512) NOT NULL,
    status VARCHAR(16) NOT NULL,
    duration VARCHAR(64) NULL,
    launch_id VARCHAR(255) NULL,
    jira_key VARCHAR(64) NULL,
    product_version VARCHAR(255) NULL,
    app_build VARCHAR(255) NULL,
    platform_id VARCHAR(36) NULL,
    repo_id VARCHAR(36) NULL,
    plan_path VARCHAR(1024) NULL,
    branch VARCHAR(512) NULL,
    run_finished_at TIMESTAMP NULL,
    run_created_at TIMESTAMP NULL,
    ingest_ordinal INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    KEY idx_qa_test_results_tenant_run (tenant_id, run_id),
    KEY idx_qa_test_results_tenant_test (tenant_id, test_file, test_name),
    KEY idx_qa_test_results_tenant_finished (tenant_id, run_finished_at DESC)
);

CREATE TABLE IF NOT EXISTS qa_test_case_results (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    run_id VARCHAR(36) NOT NULL,
    test_file VARCHAR(1024) NOT NULL,
    nodeid VARCHAR(1024) NOT NULL DEFAULT '',
    name VARCHAR(512) NOT NULL,
    status VARCHAR(16) NOT NULL,
    duration VARCHAR(64) NULL,
    reason TEXT NULL,
    ticket VARCHAR(64) NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    KEY idx_qa_test_case_results_tenant_run (tenant_id, run_id),
    KEY idx_qa_test_case_results_tenant_run_file (tenant_id, run_id, test_file),
    KEY idx_qa_test_case_results_tenant_status (tenant_id, status)
);

CREATE TABLE IF NOT EXISTS qa_test_case_collect (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    repo_id VARCHAR(36) NOT NULL,
    branch VARCHAR(512) NOT NULL,
    test_file VARCHAR(1024) NOT NULL,
    case_count INTEGER NOT NULL DEFAULT 0,
    collected_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_test_case_collect_target (tenant_id, repo_id, branch, test_file),
    KEY idx_qa_test_case_collect_tenant_repo_branch (tenant_id, repo_id, branch)
);

CREATE TABLE IF NOT EXISTS qa_analytics_saved_views (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    owner_id VARCHAR(36) NOT NULL,
    scope VARCHAR(64) NOT NULL,
    repo_id VARCHAR(36) NULL,
    plan_path VARCHAR(1024) NULL,
    plan_key VARCHAR(1061) NOT NULL DEFAULT '',
    name VARCHAR(255) NOT NULL,
    query_json JSON NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_analytics_saved_views_unique (tenant_id, owner_id, scope, plan_key, name)
);

CREATE TABLE IF NOT EXISTS qa_jira_bugs (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    jira_key VARCHAR(64) NOT NULL,
    test_name VARCHAR(512) NOT NULL,
    repo_id VARCHAR(36) NOT NULL,
    plan_path VARCHAR(1024) NOT NULL,
    app_version VARCHAR(255) NULL,
    platform_id VARCHAR(36) NULL,
    status VARCHAR(64) NOT NULL DEFAULT 'Open',
    summary TEXT NOT NULL,
    resolved_at TIMESTAMP NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_jira_bugs_tenant_key (tenant_id, jira_key),
    KEY idx_qa_jira_bugs_tenant_test_plan (tenant_id, test_name, repo_id, plan_path),
    KEY idx_qa_jira_bugs_tenant_status (tenant_id, status)
);

CREATE TABLE IF NOT EXISTS qa_jira_config (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    url VARCHAR(1024) NOT NULL DEFAULT '',
    project_key VARCHAR(64) NOT NULL DEFAULT '',
    email VARCHAR(320) NOT NULL DEFAULT '',
    api_token_credstore_ref VARCHAR(512) NOT NULL DEFAULT '',
    issue_type VARCHAR(64) NULL,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_jira_config_tenant (tenant_id)
);

CREATE TABLE IF NOT EXISTS qa_jira_poller_config (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    poll_interval_seconds BIGINT NOT NULL DEFAULT 300,
    auto_rerun_on_resolve BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_jira_poller_config_tenant (tenant_id)
);

CREATE TABLE IF NOT EXISTS qa_notification_config (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    slack_webhook_credstore_ref VARCHAR(512) NOT NULL DEFAULT '',
    slack_channel VARCHAR(255) NOT NULL DEFAULT '',
    manager_ui_base_url VARCHAR(1024) NOT NULL DEFAULT '',
    slack_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    notify_on_failure BOOLEAN NOT NULL DEFAULT TRUE,
    notify_on_success BOOLEAN NOT NULL DEFAULT FALSE,
    notify_on_schedule_completion BOOLEAN NOT NULL DEFAULT FALSE,
    scheduled_run_slack_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    scheduled_run_slack_templates JSON NOT NULL DEFAULT ('{}'),
    run_queue_queued_slack_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    email_smtp_host VARCHAR(255) NOT NULL DEFAULT '',
    email_smtp_port INTEGER NOT NULL DEFAULT 587,
    email_from VARCHAR(320) NOT NULL DEFAULT '',
    -- Parenthesised expression default (MySQL 8.0.13+): the literal form is
    -- rejected on a TEXT column, so this is how the Postgres/SQLite
    -- `DEFAULT ''` is spelled here. Same for `qa_notification_log.detail`.
    email_recipients TEXT NOT NULL DEFAULT (''),
    email_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_notification_config_tenant (tenant_id)
);

CREATE TABLE IF NOT EXISTS qa_run_notifications (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    run_id VARCHAR(36) NOT NULL,
    notification_kind VARCHAR(64) NOT NULL,
    event_type VARCHAR(64) NOT NULL,
    sent_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_run_notifications_claim (tenant_id, run_id, notification_kind, event_type)
);

CREATE TABLE IF NOT EXISTS qa_notification_log (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    run_id VARCHAR(36) NULL,
    channel VARCHAR(32) NOT NULL,
    event_type VARCHAR(64) NOT NULL DEFAULT '',
    outcome VARCHAR(32) NOT NULL,
    detail TEXT NOT NULL DEFAULT (''),
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    KEY idx_qa_notification_log_tenant_created (tenant_id, created_at DESC)
);

CREATE TABLE IF NOT EXISTS qa_ingest_watermarks (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    last_reconciled_finished_at TIMESTAMP NULL,
    last_swept_at TIMESTAMP NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_ingest_watermarks_tenant (tenant_id)
);
";

const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_test_results (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    test_file TEXT NOT NULL DEFAULT '',
    test_name TEXT NOT NULL,
    status TEXT NOT NULL,
    duration TEXT NULL,
    launch_id TEXT NULL,
    jira_key TEXT NULL,
    product_version TEXT NULL,
    app_build TEXT NULL,
    platform_id TEXT NULL,
    repo_id TEXT NULL,
    plan_path TEXT NULL,
    branch TEXT NULL,
    run_finished_at TEXT NULL,
    run_created_at TEXT NULL,
    ingest_ordinal INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_test_results_tenant_run ON qa_test_results(tenant_id, run_id);
CREATE INDEX IF NOT EXISTS idx_qa_test_results_tenant_test ON qa_test_results(tenant_id, test_file, test_name);
CREATE INDEX IF NOT EXISTS idx_qa_test_results_tenant_finished ON qa_test_results(tenant_id, run_finished_at DESC);

CREATE TABLE IF NOT EXISTS qa_test_case_results (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    test_file TEXT NOT NULL,
    nodeid TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    duration TEXT NULL,
    reason TEXT NULL,
    ticket TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_test_case_results_tenant_run ON qa_test_case_results(tenant_id, run_id);
CREATE INDEX IF NOT EXISTS idx_qa_test_case_results_tenant_run_file ON qa_test_case_results(tenant_id, run_id, test_file);
CREATE INDEX IF NOT EXISTS idx_qa_test_case_results_tenant_status ON qa_test_case_results(tenant_id, status);

CREATE TABLE IF NOT EXISTS qa_test_case_collect (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    repo_id TEXT NOT NULL,
    branch TEXT NOT NULL,
    test_file TEXT NOT NULL,
    case_count INTEGER NOT NULL DEFAULT 0,
    collected_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_test_case_collect_target ON qa_test_case_collect(tenant_id, repo_id, branch, test_file);
CREATE INDEX IF NOT EXISTS idx_qa_test_case_collect_tenant_repo_branch ON qa_test_case_collect(tenant_id, repo_id, branch);

CREATE TABLE IF NOT EXISTS qa_analytics_saved_views (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    repo_id TEXT NULL,
    plan_path TEXT NULL,
    plan_key TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    query_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_analytics_saved_views_unique ON qa_analytics_saved_views(tenant_id, owner_id, scope, plan_key, name);

CREATE TABLE IF NOT EXISTS qa_jira_bugs (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    jira_key TEXT NOT NULL,
    test_name TEXT NOT NULL,
    repo_id TEXT NOT NULL,
    plan_path TEXT NOT NULL,
    app_version TEXT NULL,
    platform_id TEXT NULL,
    status TEXT NOT NULL DEFAULT 'Open',
    summary TEXT NOT NULL,
    resolved_at TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_jira_bugs_tenant_key ON qa_jira_bugs(tenant_id, jira_key);
CREATE INDEX IF NOT EXISTS idx_qa_jira_bugs_tenant_test_plan ON qa_jira_bugs(tenant_id, test_name, repo_id, plan_path);
CREATE INDEX IF NOT EXISTS idx_qa_jira_bugs_tenant_status ON qa_jira_bugs(tenant_id, status);

CREATE TABLE IF NOT EXISTS qa_jira_config (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    url TEXT NOT NULL DEFAULT '',
    project_key TEXT NOT NULL DEFAULT '',
    email TEXT NOT NULL DEFAULT '',
    api_token_credstore_ref TEXT NOT NULL DEFAULT '',
    issue_type TEXT NULL,
    enabled INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_jira_config_tenant ON qa_jira_config(tenant_id);

CREATE TABLE IF NOT EXISTS qa_jira_poller_config (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    poll_interval_seconds INTEGER NOT NULL DEFAULT 300,
    auto_rerun_on_resolve INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_jira_poller_config_tenant ON qa_jira_poller_config(tenant_id);

CREATE TABLE IF NOT EXISTS qa_notification_config (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    slack_webhook_credstore_ref TEXT NOT NULL DEFAULT '',
    slack_channel TEXT NOT NULL DEFAULT '',
    manager_ui_base_url TEXT NOT NULL DEFAULT '',
    slack_enabled INTEGER NOT NULL DEFAULT 0,
    notify_on_failure INTEGER NOT NULL DEFAULT 1,
    notify_on_success INTEGER NOT NULL DEFAULT 0,
    notify_on_schedule_completion INTEGER NOT NULL DEFAULT 0,
    scheduled_run_slack_enabled INTEGER NOT NULL DEFAULT 0,
    scheduled_run_slack_templates TEXT NOT NULL DEFAULT '{}',
    run_queue_queued_slack_enabled INTEGER NOT NULL DEFAULT 0,
    email_smtp_host TEXT NOT NULL DEFAULT '',
    email_smtp_port INTEGER NOT NULL DEFAULT 587,
    email_from TEXT NOT NULL DEFAULT '',
    email_recipients TEXT NOT NULL DEFAULT '',
    email_enabled INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_notification_config_tenant ON qa_notification_config(tenant_id);

CREATE TABLE IF NOT EXISTS qa_run_notifications (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    notification_kind TEXT NOT NULL,
    event_type TEXT NOT NULL,
    sent_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_run_notifications_claim ON qa_run_notifications(tenant_id, run_id, notification_kind, event_type);

CREATE TABLE IF NOT EXISTS qa_notification_log (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    run_id TEXT NULL,
    channel TEXT NOT NULL,
    event_type TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_notification_log_tenant_created ON qa_notification_log(tenant_id, created_at DESC);

CREATE TABLE IF NOT EXISTS qa_ingest_watermarks (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    last_reconciled_finished_at TEXT NULL,
    last_swept_at TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_ingest_watermarks_tenant ON qa_ingest_watermarks(tenant_id);
";

/// The `CREATE` blob for a backend, or a refusal.
///
/// A free function rather than a `match` inlined in [`MigrationTrait::up`] so
/// the `MySQL` refusal is reachable from a test without standing up a `MySQL`
/// connection: `the_mysql_arm_refuses_rather_than_executing` calls this
/// directly. An unexecuted error path is the same defect class as an untested
/// column name.
fn up_ddl(backend: sea_orm::DatabaseBackend) -> Result<&'static str, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(POSTGRES_UP),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_UP),
        // **Refuses rather than narrates.** Handing `MYSQL_UP` to the engine
        // produces `ERROR 1071 (42000): Specified key was too long; max key
        // length is 3072 bytes` on the first table -- a message carrying no
        // gear name, no index name and no column. Five of this schema's
        // indexes are over InnoDB's limit; three are fixable with a prefix
        // length and two need a design decision, and all of that is written
        // down in exactly one place. This error points at it, quoting the
        // header's section title verbatim so the message is greppable.
        sea_orm::DatabaseBackend::MySql => Err(DbErr::Custom(
            "qa-insights has no MySQL schema: five of its indexes exceed InnoDB's \
             3072-byte key limit. See this module's header, \"MySQL key-width budget\"."
                .to_owned(),
        )),
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        conn.execute_unprepared(up_ddl(backend)?).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        // Reverse of `up()`'s declaration order. Nothing here references
        // anything else -- there are no foreign keys in this schema, see the
        // module header -- so the order is a convention rather than a
        // constraint, kept so a future child table cannot be dropped last by
        // accident.
        let sql = r"
DROP TABLE IF EXISTS qa_ingest_watermarks;
DROP TABLE IF EXISTS qa_notification_log;
DROP TABLE IF EXISTS qa_run_notifications;
DROP TABLE IF EXISTS qa_notification_config;
DROP TABLE IF EXISTS qa_jira_poller_config;
DROP TABLE IF EXISTS qa_jira_config;
DROP TABLE IF EXISTS qa_jira_bugs;
DROP TABLE IF EXISTS qa_analytics_saved_views;
DROP TABLE IF EXISTS qa_test_case_collect;
DROP TABLE IF EXISTS qa_test_case_results;
DROP TABLE IF EXISTS qa_test_results;
        ";
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}

/// Schema tests.
///
/// These exist because **`cargo build` proves nothing about this file.** Every
/// table name, column name and index name above is a runtime string; a typo
/// compiles cleanly and fails at query time, or worse, silently accepts a write
/// it should have rejected. Each test below issues a real statement against the
/// real `Migrator` running on an in-memory `SQLite` database.
///
/// What they cover: the table inventory, the index inventory, every index's
/// column list *in index order*, the column defaults that legacy specifies, and
/// — the part `cargo build` is furthest from — which inserts each unique index
/// accepts and which it rejects.
///
/// `POSTGRES_UP` is covered too, by
/// `the_postgres_schema_executes_and_matches_sqlite`, which applies the same
/// `Migrator` to a real container through `test_db::pg_db`. That test is gated
/// on the `integration` feature, so a default `cargo test` does not run it:
/// `cargo test -p qa-insights --features integration --lib`.
///
/// What nothing covers is `MYSQL_UP`. It is never executed on any tier, by
/// anything — `up()` refuses the `MySql` backend outright — so only its
/// *declarations* are checked, by
/// `every_dialect_declares_the_same_columns_in_the_same_order` and
/// `every_dialect_declares_the_same_indexes`. That is precisely why those two
/// tests exist.
///
/// ## Why this module talks to a raw `SeaORM` connection
///
/// `clippy::disallowed_methods` normally forbids raw query execution outside
/// the `SecureORM` wrappers, and that rule is right for every production path.
/// It is allowed here, and only here, because **a schema test needs a raw
/// connection and `SecureORM` deliberately exposes none**: `execute_unprepared`
/// is what the default-value tests need, since no scoped insert can omit a
/// column. Nothing here is a production path — the module is `#[cfg(test)]`,
/// and the tenant-scoping tests that go through `SecureORM` live with the
/// repositories in `infra::storage::*_sea_repo`.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm_migration::MigratorTrait;
    use sea_orm_migration::sea_orm::{
        ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement,
    };

    /// Tables in this gear's database that **this** migration does not own.
    ///
    /// [`migrated_db`] applies the whole [`Migrator`](super::super::Migrator),
    /// not just this file, so every inventory assertion below is over the
    /// gear's entire schema. Naming the others here keeps those assertions
    /// *exact* — a stray table still fails them — while saying out loud which
    /// migration each one belongs to.
    ///
    /// * `evbk_consumer_offsets` — `m20260818_000002_offset_store`, superseded:
    ///   it held a transactional broker consumer's durable progress, and that
    ///   consumer was deleted along with the event-broker dependency it
    ///   needed. `event-broker-sdk` owned the table's shape; the gear owns the
    ///   DDL because the migration has already run on deployed databases and
    ///   cannot be edited out from under them (see that file's header).
    ///
    /// A migration that adds a table adds a line here. That is deliberate
    /// friction: the alternative is filtering the inventory down to a prefix,
    /// which would stop these tests noticing a table nobody meant to create.
    const TABLES_OWNED_BY_LATER_MIGRATIONS: [&str; 1] = ["evbk_consumer_offsets"];

    /// The eleven tables, in the order `up()` declares them.
    const TABLES: [&str; 11] = [
        "qa_test_results",
        "qa_test_case_results",
        "qa_test_case_collect",
        "qa_analytics_saved_views",
        "qa_jira_bugs",
        "qa_jira_config",
        "qa_jira_poller_config",
        "qa_notification_config",
        "qa_run_notifications",
        "qa_notification_log",
        "qa_ingest_watermarks",
    ];

    /// Deterministic fixture UUID text. Literal strings rather than `Uuid`
    /// values because every insert below is raw SQL — there are no entities to
    /// bind through until Task 11.
    fn uuid(n: u128) -> String {
        uuid::Uuid::from_u128(n).to_string()
    }

    /// 2026-08-18 00:00:00 UTC, the fixture instant.
    const TS: &str = "2026-08-18T00:00:00Z";

    // ---------------------------------------------------------------------
    // Declaration parity — the only check `MYSQL_UP` ever gets
    // ---------------------------------------------------------------------

    /// Every column name of every table, per dialect, in declaration order.
    ///
    /// Strips `--` comment lines and blank lines, then takes the leading
    /// identifier of each remaining line inside a `CREATE TABLE` body, skipping
    /// the trailing constraint clauses (`UNIQUE`/`KEY`/`CONSTRAINT`/`FOREIGN`/
    /// `PRIMARY`/`INDEX`) that `MySQL` declares inline and the others do not.
    ///
    /// Copied from `qa-runs`' `m20260813_000003_initial.rs`.
    fn columns_by_table(ddl: &str) -> Vec<(String, Vec<String>)> {
        let mut out = Vec::new();
        let mut table: Option<(String, Vec<String>)> = None;
        for line in ddl.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("CREATE TABLE IF NOT EXISTS ") {
                let name = rest.split_whitespace().next().unwrap_or_default();
                table = Some((name.to_owned(), Vec::new()));
                continue;
            }
            let Some((_, cols)) = table.as_mut() else {
                continue;
            };
            if line.starts_with(");") {
                out.push(table.take().expect("inside a table"));
                continue;
            }
            if line.is_empty() || line.starts_with("--") {
                continue;
            }
            let ident = line.split([' ', '(']).next().unwrap_or_default();
            if matches!(
                ident.to_uppercase().as_str(),
                "UNIQUE" | "KEY" | "CONSTRAINT" | "FOREIGN" | "PRIMARY" | "INDEX"
            ) {
                continue;
            }
            cols.push(ident.to_owned());
        }
        out
    }

    /// The three dialects declare the same tables with the same columns in the
    /// same order.
    ///
    /// Eleven tables across three dialects is 33 `CREATE TABLE` bodies and well
    /// past what an eyeball diff can hold, and the blobs are not even
    /// comparable by eye: `POSTGRES_UP` is mostly comment and the other two
    /// carry almost none. This test makes comment density irrelevant.
    ///
    /// A column present in one blob and absent from another is the exact defect
    /// class this whole file's header is about — it compiles, and it fails only
    /// on the dialect nobody tested.
    #[test]
    fn every_dialect_declares_the_same_columns_in_the_same_order() {
        let pg = columns_by_table(super::POSTGRES_UP);
        let my = columns_by_table(super::MYSQL_UP);
        let sq = columns_by_table(super::SQLITE_UP);

        assert_eq!(
            pg.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
            TABLES,
            "the parser lost or reordered a table; every assertion below would \
             then compare the wrong lists"
        );
        assert!(
            pg.iter().all(|(_, c)| c.len() >= 6),
            "the parser produced a suspiciously short column list: {pg:?}"
        );
        assert_eq!(pg, my, "POSTGRES_UP and MYSQL_UP disagree");
        assert_eq!(pg, sq, "POSTGRES_UP and SQLITE_UP disagree");
    }

    /// Every index declared by a dialect, as `(name, unique, columns)`.
    ///
    /// Two syntaxes to parse, which is the whole reason a divergence is easy to
    /// miss by eye: Postgres and `SQLite` write `CREATE [UNIQUE] INDEX ... ON
    /// t(cols)` as free-standing statements, `MySQL` writes `[UNIQUE] KEY name
    /// (cols)` inline in the `CREATE TABLE` body.
    ///
    /// Copied from `qa-runs`' `m20260813_000003_initial.rs`.
    fn indexes(ddl: &str) -> Vec<(String, bool, Vec<String>)> {
        let mut out = Vec::new();
        // Strip `--` lines *before* flattening: a comment above a statement
        // would otherwise be folded onto the front of it, and the statement
        // would stop matching its `CREATE ...` prefix.
        let flat = ddl
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("--"))
            .collect::<Vec<_>>()
            .join(" ");
        for stmt in flat.split(';') {
            let stmt = stmt.trim();
            let (unique, rest) =
                if let Some(r) = stmt.strip_prefix("CREATE UNIQUE INDEX IF NOT EXISTS ") {
                    (true, r)
                } else if let Some(r) = stmt.strip_prefix("CREATE INDEX IF NOT EXISTS ") {
                    (false, r)
                } else {
                    continue;
                };
            let (name, cols) = rest
                .split_once(" ON ")
                .expect("index statement has an ON clause");
            let cols = cols
                .split_once('(')
                .expect("index statement has a column list")
                .1
                .trim_end_matches(')');
            out.push((
                name.trim().to_owned(),
                unique,
                cols.split(',').map(|c| c.trim().to_owned()).collect(),
            ));
        }
        for line in ddl.lines() {
            let line = line.trim().trim_end_matches(',');
            let (unique, rest) = if let Some(r) = line.strip_prefix("UNIQUE KEY ") {
                (true, r)
            } else if let Some(r) = line.strip_prefix("KEY ") {
                (false, r)
            } else {
                continue;
            };
            let (name, cols) = rest.split_once(" (").expect("KEY clause has a column list");
            out.push((
                name.trim().to_owned(),
                unique,
                cols.trim_end_matches(')')
                    .split(',')
                    .map(|c| c.trim().to_owned())
                    .collect(),
            ));
        }
        out.sort();
        out
    }

    /// The three dialects declare the same indexes, with the same uniqueness
    /// and the same column order.
    ///
    /// The companion to the column test, and it is not redundant with it:
    /// `qa-runs` measured that changing an index in one dialect alone left
    /// every other test in its suite green, because the `SQLite`-backed index
    /// tests cannot see a server-dialect divergence and the column parity test
    /// compares columns.
    #[test]
    fn every_dialect_declares_the_same_indexes() {
        let pg = indexes(super::POSTGRES_UP);
        let my = indexes(super::MYSQL_UP);
        let sq = indexes(super::SQLITE_UP);

        assert_eq!(
            pg.len(),
            18,
            "expected 18 indexes; the parser found {} and every comparison \
             below would then be over the wrong set: {pg:?}",
            pg.len()
        );
        assert_eq!(pg, my, "POSTGRES_UP and MYSQL_UP declare different indexes");
        assert_eq!(
            pg, sq,
            "POSTGRES_UP and SQLITE_UP declare different indexes"
        );
    }

    /// Every unique index leads with `tenant_id`.
    ///
    /// The rule from the module header, asserted over the declarations rather
    /// than trusted to review. A unique index that forgets the prefix compiles,
    /// passes every single-tenant fixture in this file, and is a cross-tenant
    /// squatting and existence-oracle channel in production — the failure mode
    /// no test that only inserts as one tenant can see.
    #[test]
    fn every_unique_index_is_tenant_prefixed() {
        let unique: Vec<_> = indexes(super::POSTGRES_UP)
            .into_iter()
            .filter(|(_, unique, _)| *unique)
            .collect();
        assert_eq!(
            unique.len(),
            8,
            "expected 8 unique indexes, found {}: {unique:?}",
            unique.len()
        );
        for (name, _, cols) in unique {
            assert_eq!(
                cols.first().map(String::as_str),
                Some("tenant_id"),
                "{name} is unique but does not lead with tenant_id: {cols:?}"
            );
        }
    }

    // ---------------------------------------------------------------------
    // The migration, executed
    // ---------------------------------------------------------------------

    /// In-memory `SQLite` with this gear's real migrations applied through the
    /// real [`super::super::Migrator`].
    ///
    /// `max_connections(1)` **is** load-bearing: each `SQLite` `:memory:`
    /// connection is its own database, so a larger pool would let a query land
    /// on a connection where the migration never ran.
    async fn migrated_db() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        let conn = Database::connect(opts)
            .await
            .expect("failed to connect to in-memory sqlite database");

        let manager = sea_orm_migration::SchemaManager::new(&conn);
        for migration in super::super::Migrator::migrations() {
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-insights migrations");
        }
        conn
    }

    /// Collect the `name` column of a one-column query.
    async fn name_column(conn: &DatabaseConnection, sql: String) -> Vec<String> {
        conn.query_all(Statement::from_string(
            sea_orm_migration::sea_orm::DatabaseBackend::Sqlite,
            sql,
        ))
        .await
        .unwrap()
        .iter()
        .filter_map(|row| row.try_get::<String>("", "name").ok())
        .collect()
    }

    /// Every user table, from `SQLite`'s own catalogue.
    ///
    /// **Written here, not copied.** Task 10's instructions said to lift a
    /// table-listing helper from `qa-runs`' migration next to `index_names`;
    /// there is none there — that file has `name_column`, `index_names` and
    /// `index_columns` and nothing else. This is the missing one, built on the
    /// same `name_column`.
    ///
    /// `seaql_migrations` is filtered even though [`migrated_db`] drives
    /// `up()` directly and never creates it: a future harness that goes through
    /// `MigratorTrait::up` would, and the inventory assertion should not start
    /// failing for that reason.
    async fn table_names(conn: &DatabaseConnection) -> Vec<String> {
        name_column(
            conn,
            "SELECT name FROM sqlite_master WHERE type='table' \
             AND name NOT LIKE 'sqlite_%' AND name <> 'seaql_migrations'"
                .to_owned(),
        )
        .await
    }

    /// Index names on a table, from `SQLite`'s own catalogue. Excludes the
    /// implicit `sqlite_autoindex_*` entries that back `PRIMARY KEY`.
    async fn index_names(conn: &DatabaseConnection, table: &str) -> Vec<String> {
        let mut names = name_column(
            conn,
            format!(
                "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='{table}' \
                 AND name NOT LIKE 'sqlite_autoindex%'"
            ),
        )
        .await;
        names.sort();
        names
    }

    /// Column names of one index, **in index order and with sort direction**,
    /// from `PRAGMA index_xinfo`.
    ///
    /// `index_xinfo` rather than the `index_info` the sibling gear uses,
    /// because `index_info` does not expose sort order at all — under it,
    /// `(tenant_id, run_finished_at DESC)` and `(tenant_id, run_finished_at)`
    /// are indistinguishable. Two indexes here are declared `DESC`
    /// (`idx_qa_test_results_tenant_finished`,
    /// `idx_qa_notification_log_tenant_created`) and the descending order is
    /// the whole reason they serve their "most recent first" reads. The parity
    /// test compares the literal declarations, so dropping `DESC` from *one*
    /// dialect is caught there; dropping it from all three was caught by
    /// nothing until this helper started reading the `desc` column.
    ///
    /// `key = 1` filters the trailing rowid/PK columns `index_xinfo` appends,
    /// which `index_info` does not list.
    async fn index_columns(conn: &DatabaseConnection, index: &str) -> Vec<String> {
        conn.query_all(Statement::from_string(
            sea_orm_migration::sea_orm::DatabaseBackend::Sqlite,
            format!(
                "SELECT name || CASE desc WHEN 1 THEN ' DESC' ELSE '' END AS name \
                 FROM pragma_index_xinfo('{index}') WHERE key = 1"
            ),
        ))
        .await
        .unwrap()
        .iter()
        .filter_map(|row| row.try_get::<String>("", "name").ok())
        .collect()
    }

    /// Run a statement, returning whether it succeeded.
    ///
    /// The unique-index tests are all of the form "this second insert must be
    /// rejected", so a boolean is the whole observation.
    async fn try_exec(conn: &DatabaseConnection, sql: &str) -> bool {
        conn.execute_unprepared(sql).await.is_ok()
    }

    async fn exec(conn: &DatabaseConnection, sql: &str) {
        conn.execute_unprepared(sql)
            .await
            .unwrap_or_else(|e| panic!("statement failed: {e}\n{sql}"));
    }

    /// Read one scalar back as text, whatever its declared type.
    async fn scalar(conn: &DatabaseConnection, sql: &str) -> String {
        conn.query_one(Statement::from_string(
            sea_orm_migration::sea_orm::DatabaseBackend::Sqlite,
            sql.to_owned(),
        ))
        .await
        .unwrap()
        .expect("one row")
        .try_get::<String>("", "v")
        .unwrap()
    }

    /// The table inventory, and it is deliberately **exact** rather than a
    /// containment check: a table nobody meant to create is as much a defect as
    /// a missing one, and only an equality assertion sees it.
    ///
    /// The literals are written out rather than derived from [`TABLES`], so
    /// this is an independent oracle — a typo in that constant fails here
    /// instead of being copied into the expectation. The one derived part is
    /// [`TABLES_OWNED_BY_LATER_MIGRATIONS`], which is the list of tables this
    /// file does not own; see its doc.
    #[tokio::test]
    async fn the_initial_migration_creates_every_table() {
        let conn = migrated_db().await;
        let mut tables = table_names(&conn).await;
        tables.sort();

        let mut expected = vec![
            "qa_analytics_saved_views",
            "qa_ingest_watermarks",
            "qa_jira_bugs",
            "qa_jira_config",
            "qa_jira_poller_config",
            "qa_notification_config",
            "qa_notification_log",
            "qa_run_notifications",
            "qa_test_case_collect",
            "qa_test_case_results",
            "qa_test_results",
        ];
        expected.extend_from_slice(&TABLES_OWNED_BY_LATER_MIGRATIONS);
        expected.sort_unstable();

        assert_eq!(tables, expected);
    }

    /// The index inventory, per table.
    ///
    /// Non-unique indexes are pinned by nothing else at all: they change no
    /// observable behaviour, so a typo in a name or a missing declaration
    /// leaves every other test green. Index names are runtime strings, which is
    /// this file's own thesis about why `cargo build` proves nothing.
    #[tokio::test]
    async fn the_initial_migration_creates_every_index() {
        let conn = migrated_db().await;

        for (table, expected) in [
            (
                "qa_test_results",
                vec![
                    "idx_qa_test_results_tenant_finished",
                    "idx_qa_test_results_tenant_run",
                    "idx_qa_test_results_tenant_test",
                ],
            ),
            (
                "qa_test_case_results",
                vec![
                    "idx_qa_test_case_results_tenant_run",
                    "idx_qa_test_case_results_tenant_run_file",
                    "idx_qa_test_case_results_tenant_status",
                ],
            ),
            (
                "qa_test_case_collect",
                vec![
                    "idx_qa_test_case_collect_target",
                    "idx_qa_test_case_collect_tenant_repo_branch",
                ],
            ),
            (
                "qa_analytics_saved_views",
                vec!["idx_qa_analytics_saved_views_unique"],
            ),
            (
                "qa_jira_bugs",
                vec![
                    "idx_qa_jira_bugs_tenant_key",
                    "idx_qa_jira_bugs_tenant_status",
                    "idx_qa_jira_bugs_tenant_test_plan",
                ],
            ),
            ("qa_jira_config", vec!["idx_qa_jira_config_tenant"]),
            (
                "qa_jira_poller_config",
                vec!["idx_qa_jira_poller_config_tenant"],
            ),
            (
                "qa_notification_config",
                vec!["idx_qa_notification_config_tenant"],
            ),
            (
                "qa_run_notifications",
                vec!["idx_qa_run_notifications_claim"],
            ),
            (
                "qa_notification_log",
                vec!["idx_qa_notification_log_tenant_created"],
            ),
            (
                "qa_ingest_watermarks",
                vec!["idx_qa_ingest_watermarks_tenant"],
            ),
        ] {
            assert_eq!(
                index_names(&conn, table).await,
                expected,
                "{table} has the wrong index set"
            );
        }
    }

    /// Every declared index exists over the declared columns, **in order**.
    ///
    /// Order matters and a permuted list still creates happily while serving
    /// the intended access pattern not at all — `(tenant_id, run_id,
    /// test_file)` serves a per-run-per-file drill-down, `(test_file, run_id,
    /// tenant_id)` does not serve anything this gear asks for.
    ///
    /// Sort direction matters for the same reason and is asserted here too: the
    /// two `DESC` indexes exist to serve "most recent first" without a sort
    /// step, which an ascending index does not do.
    #[tokio::test]
    async fn every_declared_index_exists_with_the_declared_columns() {
        let conn = migrated_db().await;

        for (index, columns) in [
            (
                "idx_qa_test_results_tenant_run",
                vec!["tenant_id", "run_id"],
            ),
            (
                "idx_qa_test_results_tenant_test",
                vec!["tenant_id", "test_file", "test_name"],
            ),
            (
                "idx_qa_test_results_tenant_finished",
                vec!["tenant_id", "run_finished_at DESC"],
            ),
            (
                "idx_qa_test_case_results_tenant_run",
                vec!["tenant_id", "run_id"],
            ),
            (
                "idx_qa_test_case_results_tenant_run_file",
                vec!["tenant_id", "run_id", "test_file"],
            ),
            (
                "idx_qa_test_case_results_tenant_status",
                vec!["tenant_id", "status"],
            ),
            (
                "idx_qa_test_case_collect_target",
                vec!["tenant_id", "repo_id", "branch", "test_file"],
            ),
            (
                "idx_qa_test_case_collect_tenant_repo_branch",
                vec!["tenant_id", "repo_id", "branch"],
            ),
            (
                "idx_qa_analytics_saved_views_unique",
                vec!["tenant_id", "owner_id", "scope", "plan_key", "name"],
            ),
            ("idx_qa_jira_bugs_tenant_key", vec!["tenant_id", "jira_key"]),
            (
                "idx_qa_jira_bugs_tenant_test_plan",
                vec!["tenant_id", "test_name", "repo_id", "plan_path"],
            ),
            (
                "idx_qa_jira_bugs_tenant_status",
                vec!["tenant_id", "status"],
            ),
            ("idx_qa_jira_config_tenant", vec!["tenant_id"]),
            ("idx_qa_jira_poller_config_tenant", vec!["tenant_id"]),
            ("idx_qa_notification_config_tenant", vec!["tenant_id"]),
            (
                "idx_qa_run_notifications_claim",
                vec!["tenant_id", "run_id", "notification_kind", "event_type"],
            ),
            (
                "idx_qa_notification_log_tenant_created",
                vec!["tenant_id", "created_at DESC"],
            ),
            ("idx_qa_ingest_watermarks_tenant", vec!["tenant_id"]),
        ] {
            assert_eq!(
                index_columns(&conn, index).await,
                columns,
                "{index} has the wrong columns or the wrong column order"
            );
        }
    }

    // ---------------------------------------------------------------------
    // What the schema accepts and refuses — D4
    // ---------------------------------------------------------------------

    /// Insert a saved view. `plan_key` is passed explicitly because the
    /// repository writes it; this is the schema's contract with the writer, and
    /// `saved_views_sea_repo`'s own tests are what check the writer keeps it.
    fn saved_view(id: u128, tenant: u128, owner: u128, scope: &str, plan_key: &str) -> String {
        format!(
            "INSERT INTO qa_analytics_saved_views \
             (id, tenant_id, owner_id, scope, plan_key, name, query_json, created_at, updated_at) \
             VALUES ('{}', '{}', '{}', '{scope}', '{plan_key}', 'My View', '{{}}', '{TS}', '{TS}')",
            uuid(id),
            uuid(tenant),
            uuid(owner),
        )
    }

    /// D4, first half: one owner keeps a global view and a plan-scoped view of
    /// the same name.
    ///
    /// Proven by insert, not by reading the index definition — the point of the
    /// exercise is what a caller can now do.
    #[tokio::test]
    async fn a_global_and_a_plan_scoped_view_may_share_a_name() {
        let conn = migrated_db().await;
        exec(&conn, &saved_view(1, 0xA, 0xB, "all", "")).await;
        assert!(
            try_exec(
                &conn,
                &saved_view(
                    2,
                    0xA,
                    0xB,
                    "plan",
                    "00000000-0000-0000-0000-0000000000c0/p.yaml"
                )
            )
            .await,
            "legacy lets one owner hold a global view and a plan-scoped view of \
             the same name; this schema must too"
        );
    }

    /// D4, second half — **the property the `COALESCE` actually buys.**
    ///
    /// A unique index over a nullable `plan_id` would not catch this: SQL says
    /// `NULL != NULL`, so two global views named "My View" would both be
    /// accepted and legacy's own index would be doing nothing for the case it
    /// most obviously covers. Materializing the coalesced value as
    /// `plan_key NOT NULL DEFAULT ''` is what makes them collide.
    ///
    /// This is the assertion that fails if a future change makes `plan_key`
    /// nullable, or drops it in favour of indexing `repo_id`/`plan_path`
    /// directly — a change that would look like a simplification and would
    /// silently reopen the duplicate.
    #[tokio::test]
    async fn two_global_views_of_one_owner_may_not_share_a_name() {
        let conn = migrated_db().await;
        exec(&conn, &saved_view(1, 0xA, 0xB, "all", "")).await;
        assert!(
            !try_exec(&conn, &saved_view(2, 0xA, 0xB, "all", "")).await,
            "a second global view of the same name for the same owner must be \
             rejected; that is what COALESCE(plan_id, '') does in legacy"
        );
    }

    /// Two owners in one tenant are independent, and two tenants are
    /// independent of each other.
    ///
    /// The tenancy half is the one legacy cannot have: `tenant_id` leads the
    /// unique index, so a squatter cannot deny a victim a view name.
    #[tokio::test]
    async fn a_saved_view_name_is_unique_per_owner_and_per_tenant() {
        let conn = migrated_db().await;
        exec(&conn, &saved_view(1, 0xA, 0xB, "all", "")).await;
        assert!(
            try_exec(&conn, &saved_view(2, 0xA, 0xC, "all", "")).await,
            "a different owner in the same tenant may reuse the name"
        );
        assert!(
            try_exec(&conn, &saved_view(3, 0xF, 0xB, "all", "")).await,
            "the same owner id in a different tenant may reuse the name"
        );
    }

    // ---------------------------------------------------------------------
    // What the schema accepts and refuses — D5 dedupe
    // ---------------------------------------------------------------------

    fn claim(id: u128, tenant: u128, run: u128, kind: &str, event: &str) -> String {
        format!(
            "INSERT INTO qa_run_notifications \
             (id, tenant_id, run_id, notification_kind, event_type, sent_at, created_at, updated_at) \
             VALUES ('{}', '{}', '{}', '{kind}', '{event}', '{TS}', '{TS}', '{TS}')",
            uuid(id),
            uuid(tenant),
            uuid(run),
        )
    }

    /// The dedupe key rejects the second identical claim.
    ///
    /// This *is* the mechanism, not an incidental key: legacy makes the same
    /// tuple its `PRIMARY KEY` (`001_initial.sql:202`) and the failed insert is
    /// how the notifier learns the message already went out. Tasks 36-38 build
    /// the whole idempotency story on it, so it is asserted here where the
    /// constraint lives.
    #[tokio::test]
    async fn a_repeated_notification_claim_is_rejected() {
        let conn = migrated_db().await;
        exec(&conn, &claim(1, 0xA, 0x10, "slack", "succeeded")).await;
        assert!(
            !try_exec(&conn, &claim(2, 0xA, 0x10, "slack", "succeeded")).await,
            "the second claim on the same (run, kind, event) must be rejected"
        );
        assert!(
            try_exec(&conn, &claim(3, 0xA, 0x10, "slack", "failed")).await,
            "a different event on the same run is a different claim"
        );
        assert!(
            try_exec(&conn, &claim(4, 0xA, 0x10, "email", "succeeded")).await,
            "a different channel on the same event is a different claim"
        );
    }

    /// A claim is per tenant, even though `run_id` is unique on its own.
    ///
    /// The sharpest case for the tenant prefix in this schema: the prefix looks
    /// redundant right up until someone learns a victim's `run_id` and squats
    /// its notification claims, suppressing the victim's alerts.
    #[tokio::test]
    async fn a_notification_claim_is_per_tenant_not_global() {
        let conn = migrated_db().await;
        exec(&conn, &claim(1, 0xA, 0x10, "slack", "succeeded")).await;
        assert!(
            try_exec(&conn, &claim(2, 0xF, 0x10, "slack", "succeeded")).await,
            "another tenant's claim on the same run id must not collide"
        );
    }

    // ---------------------------------------------------------------------
    // What the schema accepts and refuses — the remaining unique keys
    // ---------------------------------------------------------------------

    fn collect_row(id: u128, tenant: u128, repo: u128, branch: &str, file: &str) -> String {
        format!(
            "INSERT INTO qa_test_case_collect \
             (id, tenant_id, repo_id, branch, test_file, case_count, collected_at, created_at, updated_at) \
             VALUES ('{}', '{}', '{}', '{branch}', '{file}', 3, '{TS}', '{TS}', '{TS}')",
            uuid(id),
            uuid(tenant),
            uuid(repo),
        )
    }

    /// Legacy's composite `PRIMARY KEY (repo_id, branch, test_file)`
    /// (`001_initial.sql:278`) survives as a unique index, which is what the
    /// collect upsert targets.
    #[tokio::test]
    async fn a_collect_target_is_unique_per_tenant() {
        let conn = migrated_db().await;
        exec(&conn, &collect_row(1, 0xA, 0x20, "main", "t/a.py")).await;
        assert!(
            !try_exec(&conn, &collect_row(2, 0xA, 0x20, "main", "t/a.py")).await,
            "a second count for the same (repo, branch, file) must be rejected \
             so the upsert has something to conflict on"
        );
        assert!(
            try_exec(&conn, &collect_row(3, 0xA, 0x20, "dev", "t/a.py")).await,
            "a different branch is a different target"
        );
        assert!(
            try_exec(&conn, &collect_row(4, 0xF, 0x20, "main", "t/a.py")).await,
            "another tenant's identical target must not collide"
        );
    }

    fn bug(id: u128, tenant: u128, key: &str) -> String {
        format!(
            "INSERT INTO qa_jira_bugs \
             (id, tenant_id, jira_key, test_name, repo_id, plan_path, summary, created_at, updated_at) \
             VALUES ('{}', '{}', '{key}', 'test_a', '{}', 'p/plan.yaml', 's', '{TS}', '{TS}')",
            uuid(id),
            uuid(tenant),
            uuid(0x30),
        )
    }

    /// Legacy's `jira_key TEXT NOT NULL UNIQUE` (`001_initial.sql:80`) is
    /// *globally* unique. Here it is unique per tenant, and that difference is
    /// the point.
    ///
    /// A JIRA key is not a secret. Left global, one tenant registering `VHP-1`
    /// would deny it to every other tenant forever, and the unique violation
    /// would report whether another tenant had already filed it.
    #[tokio::test]
    async fn a_bug_key_is_unique_per_tenant_not_globally() {
        let conn = migrated_db().await;
        exec(&conn, &bug(1, 0xA, "VHP-1")).await;
        assert!(
            !try_exec(&conn, &bug(2, 0xA, "VHP-1")).await,
            "the same tenant may register a JIRA key only once"
        );
        assert!(
            try_exec(&conn, &bug(3, 0xF, "VHP-1")).await,
            "another tenant registering the same JIRA key must be accepted"
        );
    }

    /// The **four** singleton tables hold exactly one row per tenant.
    ///
    /// Three are the configuration tables of D6 — legacy's equivalent is a
    /// single `settings` row per key for the whole installation, and the unique
    /// index on `(tenant_id)` alone is what turns that into a per-tenant
    /// singleton rather than a table anyone can append to. The fourth,
    /// `qa_ingest_watermarks`, is not configuration at all but has the same
    /// one-row-per-tenant shape and the same index, so it is covered here.
    ///
    /// (Corrected 2026-08-20: this said "three configuration tables" directly
    /// above a loop over four, and the test name carried the same error. The
    /// header's own tally at "the four singleton `(tenant_id)` uniques" was
    /// right all along.)
    #[tokio::test]
    async fn each_singleton_table_holds_one_row_per_tenant() {
        let conn = migrated_db().await;
        for table in [
            "qa_jira_config",
            "qa_jira_poller_config",
            "qa_notification_config",
            "qa_ingest_watermarks",
        ] {
            let row = |id: u128, tenant: u128| {
                format!(
                    "INSERT INTO {table} (id, tenant_id, created_at, updated_at) \
                     VALUES ('{}', '{}', '{TS}', '{TS}')",
                    uuid(id),
                    uuid(tenant),
                )
            };
            exec(&conn, &row(1, 0xA)).await;
            assert!(
                !try_exec(&conn, &row(2, 0xA)).await,
                "{table} must admit only one row per tenant"
            );
            assert!(
                try_exec(&conn, &row(3, 0xF)).await,
                "{table} must admit a row for every tenant"
            );
        }
    }

    // ---------------------------------------------------------------------
    // Defaults
    // ---------------------------------------------------------------------

    /// Every column default is legacy's, and an insert that omits the column
    /// lands on it.
    ///
    /// Asserted by omitting the columns from the `INSERT`, which no
    /// `ActiveModel` can do — a mapper that always sets every field would make
    /// these defaults unobservable and a wrong one invisible. Two are
    /// behavioural rather than cosmetic: `notify_on_failure` is the only
    /// notification gate legacy starts **on**
    /// (`manager/src/models.rs:1364-1384`), and `auto_rerun_on_resolve` starts
    /// **on** too (`:1422-1429`) — flipping either to the `derive(Default)`
    /// value would silently change who gets paged.
    #[tokio::test]
    async fn omitted_columns_land_on_legacy_defaults() {
        let conn = migrated_db().await;

        exec(
            &conn,
            &format!(
                "INSERT INTO qa_jira_poller_config (id, tenant_id, created_at, updated_at) \
                 VALUES ('{}', '{}', '{TS}', '{TS}')",
                uuid(1),
                uuid(0xA)
            ),
        )
        .await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT poll_interval_seconds || '/' || auto_rerun_on_resolve AS v \
                 FROM qa_jira_poller_config"
            )
            .await,
            "300/1",
            "legacy's JiraPollerConfig::default is 300 seconds with auto-rerun ON"
        );

        exec(
            &conn,
            &format!(
                "INSERT INTO qa_notification_config (id, tenant_id, created_at, updated_at) \
                 VALUES ('{}', '{}', '{TS}', '{TS}')",
                uuid(2),
                uuid(0xA)
            ),
        )
        .await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT slack_enabled || '/' || notify_on_failure || '/' || notify_on_success \
                 || '/' || notify_on_schedule_completion || '/' || scheduled_run_slack_enabled \
                 || '/' || run_queue_queued_slack_enabled || '/' || email_enabled \
                 || '/' || email_smtp_port || '/' || scheduled_run_slack_templates AS v \
                 FROM qa_notification_config"
            )
            .await,
            "0/1/0/0/0/0/0/587/{}",
            "notify_on_failure is the one gate legacy starts on, and SMTP port is 587"
        );

        exec(
            &conn,
            &format!(
                "INSERT INTO qa_test_results \
                 (id, tenant_id, run_id, test_name, status, created_at, updated_at) \
                 VALUES ('{}', '{}', '{}', 'test_a', 'PASSED', '{TS}', '{TS}')",
                uuid(3),
                uuid(0xA),
                uuid(0x10)
            ),
        )
        .await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT '[' || test_file || ']' AS v FROM qa_test_results"
            )
            .await,
            "[]",
            "an omitted test_file is the empty string, not NULL: legacy's column \
             is nullable and every query pays for it with COALESCE"
        );

        exec(
            &conn,
            &format!(
                "INSERT INTO qa_jira_bugs \
                 (id, tenant_id, jira_key, test_name, repo_id, plan_path, summary, created_at, updated_at) \
                 VALUES ('{}', '{}', 'VHP-9', 'test_a', '{}', 'p/plan.yaml', 's', '{TS}', '{TS}')",
                uuid(4),
                uuid(0xA),
                uuid(0x30)
            ),
        )
        .await;
        assert_eq!(
            scalar(&conn, "SELECT status AS v FROM qa_jira_bugs").await,
            "Open",
            "legacy defaults jira_bugs.status to 'Open' (001_initial.sql:85)"
        );

        exec(
            &conn,
            &format!(
                "INSERT INTO qa_notification_log \
                 (id, tenant_id, channel, outcome, created_at, updated_at) \
                 VALUES ('{}', '{}', 'slack', 'sent', '{TS}', '{TS}')",
                uuid(5),
                uuid(0xA)
            ),
        )
        .await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT '[' || event_type || '][' || detail || ']' AS v FROM qa_notification_log"
            )
            .await,
            "[][]",
            "legacy defaults both to '' (001_initial.sql:210, :212)"
        );

        exec(
            &conn,
            &format!(
                "INSERT INTO qa_test_case_results \
                 (id, tenant_id, run_id, test_file, name, status, created_at, updated_at) \
                 VALUES ('{}', '{}', '{}', 't/a.py', 'test_a', 'PASSED', '{TS}', '{TS}')",
                uuid(6),
                uuid(0xA),
                uuid(0x10)
            ),
        )
        .await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT '[' || nodeid || ']' AS v FROM qa_test_case_results"
            )
            .await,
            "[]",
            "legacy defaults nodeid to '' (001_initial.sql:257)"
        );
    }

    /// `qa_test_case_results.test_file` is `NOT NULL` with **no** default,
    /// exactly as legacy (`001_initial.sql:256`).
    ///
    /// The negative half of the test above, and the reason the two tables spell
    /// `test_file` differently: `qa_test_results` collapses absent to `''`
    /// because legacy's column there is nullable, and this one never was.
    #[tokio::test]
    async fn a_case_result_without_a_test_file_is_rejected() {
        let conn = migrated_db().await;
        assert!(
            !try_exec(
                &conn,
                &format!(
                    "INSERT INTO qa_test_case_results \
                     (id, tenant_id, run_id, name, status, created_at, updated_at) \
                     VALUES ('{}', '{}', '{}', 'test_a', 'PASSED', '{TS}', '{TS}')",
                    uuid(1),
                    uuid(0xA),
                    uuid(0x10)
                )
            )
            .await,
            "test_case_results.test_file has no default in legacy and must have \
             none here"
        );
    }

    /// `up()` refuses `MySQL` instead of handing it DDL that cannot run.
    ///
    /// Empirically, of the eleven `MYSQL_UP` blobs across the four qa-platform
    /// gears, ten execute cleanly against `MySQL` 8 and this one dies on
    /// `ERROR 1071 (42000)` at the first table. Since this gear cannot even be
    /// built with a `MySQL` backend (`toolkit-db` features are `sqlite` and
    /// `pg`), the refusal will most likely never fire — which is exactly why it
    /// needs a test: an error path nobody executes is the same defect class as
    /// a column name nobody queries.
    ///
    /// Asserts on the *content* of the message, not just that it is an error.
    /// The value of this arm is entirely in what it tells the engineer, and a
    /// refusal that said only "unsupported" would be worse than `ERROR 1071`.
    #[test]
    fn the_mysql_arm_refuses_rather_than_executing() {
        use sea_orm_migration::sea_orm::DatabaseBackend;

        assert_eq!(
            super::up_ddl(DatabaseBackend::Postgres).unwrap(),
            super::POSTGRES_UP
        );
        assert_eq!(
            super::up_ddl(DatabaseBackend::Sqlite).unwrap(),
            super::SQLITE_UP
        );

        let err = super::up_ddl(DatabaseBackend::MySql)
            .expect_err("the MySQL arm must refuse, not return DDL");
        let msg = err.to_string();
        for needle in [
            "qa-insights",
            "3072-byte key limit",
            // Quoted verbatim from the header section title, so an engineer who
            // pastes the message into a grep lands on the explanation.
            "MySQL key-width budget",
        ] {
            assert!(
                msg.contains(needle),
                "the refusal must contain {needle:?} to be actionable; got: {msg}"
            );
        }
    }

    /// **A repeated result tuple is accepted, deliberately.**
    ///
    /// There is no unique index on `(tenant_id, run_id, test_file, test_name)`
    /// and there must not be: ingest is idempotent by *delete-then-insert*,
    /// which legacy does the same way
    /// (`manager/src/routes/runs.rs:1153-1185`) and which qa-runs records at
    /// its own index. A unique index would make the re-insert half of that
    /// fail, and `qa_ingest_watermarks` makes re-ingest routine rather than
    /// exceptional — the reconcile poller replays a window of finished runs on
    /// every pass.
    ///
    /// This is a **test of an absence**, which nothing else in the file
    /// provides: adding the index would look like a tightening, would break
    /// ingest, and would add a sixth `MySQL` key-width failure. It is also the
    /// executable half of obligation 5 in the module header — the DDL cannot
    /// enforce idempotency, so all this can do is stop the wrong fix.
    #[tokio::test]
    async fn a_repeated_result_tuple_is_accepted() {
        let conn = migrated_db().await;
        let row = |id: u128| {
            format!(
                "INSERT INTO qa_test_results \
                 (id, tenant_id, run_id, test_file, test_name, status, created_at, updated_at) \
                 VALUES ('{}', '{}', '{}', 't/a.py', 'test_a', 'PASSED', '{TS}', '{TS}')",
                uuid(id),
                uuid(0xA),
                uuid(0x10),
            )
        };
        exec(&conn, &row(1)).await;
        assert!(
            try_exec(&conn, &row(2)).await,
            "the same (run, file, test) tuple must be insertable twice: ingest \
             dedupes by delete-then-insert, and a unique index here would break it"
        );
    }

    // -----------------------------------------------------------------
    // The dialect that actually ships
    // -----------------------------------------------------------------

    /// **`POSTGRES_UP` runs, and declares the same schema `SQLITE_UP` does.**
    ///
    /// Until this existed, `POSTGRES_UP` had *zero* automated verification —
    /// in a file whose thesis is that `cargo build` proves nothing about it,
    /// and for the one dialect this gear actually deploys (`toolkit-db` is
    /// linked with `sqlite` and `pg`). The parity tests do not close that hole:
    /// they prove the three blobs *agree*, not that any of them *executes*. A
    /// syntax error, a rejected default, or an index over a mistyped column in
    /// `POSTGRES_UP` alone would have shipped with the whole suite green.
    ///
    /// Asserting *equality with the `SQLite` inventories* rather than against a
    /// second hard-coded list is deliberate: a hand-copied expectation is one
    /// more thing to keep in step, and drifting it would silently weaken the
    /// test. This way `the_initial_migration_creates_every_table` remains the
    /// single place the eleven names are written down.
    ///
    /// **Residual, stated rather than left implicit:** this compares index
    /// *names*, not their column lists. Column parity on Postgres rests on
    /// `every_dialect_declares_the_same_indexes` plus the `SQLite` execution —
    /// i.e. on the declarations agreeing and one of them running. Closing it
    /// directly would mean parsing `pg_get_indexdef`, which is the tool to
    /// reach for if that ever stops being enough.
    ///
    /// Gated on `integration`, so a default `cargo test` needs no Docker:
    /// `cargo test -p qa-insights --features integration --lib`.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_postgres_schema_executes_and_matches_sqlite() {
        use sea_orm_migration::sea_orm::DatabaseBackend;

        let harness = crate::infra::storage::test_db::pg_db().await;

        // A raw SeaORM connection alongside the toolkit-db pool: `Db` exposes
        // none, and reading `pg_catalog` is the whole point here.
        let pg = Database::connect(&harness.url)
            .await
            .expect("failed to open a raw connection to the container");

        let pg_names = |sql: String| async {
            let sql = sql;
            pg.query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
                .await
                .unwrap()
                .iter()
                .filter_map(|row| row.try_get::<String>("", "name").ok())
                .collect::<Vec<_>>()
        };

        let mut pg_tables = pg_names(
            // `run_migrations_for_testing` writes its bookkeeping to
            // `toolkit_migrations___test__<hash>`, not `seaql_migrations` --
            // the platform's own runner, applied here precisely because that
            // is what production uses. Excluded by prefix because the hash
            // suffix is derived and would otherwise pin this test to it.
            "SELECT tablename AS name FROM pg_tables WHERE schemaname = 'public' \
             AND tablename NOT LIKE 'toolkit_migrations%'"
                .to_owned(),
        )
        .await;
        pg_tables.sort();

        let sqlite = migrated_db().await;
        let mut sqlite_tables = table_names(&sqlite).await;
        sqlite_tables.sort();

        assert_eq!(
            pg_tables, sqlite_tables,
            "POSTGRES_UP and SQLITE_UP produced different tables when actually run"
        );
        assert_eq!(
            pg_tables.len(),
            TABLES.len() + TABLES_OWNED_BY_LATER_MIGRATIONS.len(),
            "expected this migration's eleven tables plus the later migrations' own"
        );

        for table in TABLES {
            let mut pg_idx = pg_names(format!(
                "SELECT indexname AS name FROM pg_indexes WHERE schemaname = 'public' \
                 AND tablename = '{table}' AND indexname NOT LIKE '%_pkey'"
            ))
            .await;
            pg_idx.sort();
            // Non-vacuity: every table here declares at least one index, so an
            // empty-equals-empty comparison means the catalogue query stopped
            // matching, not that the schema is fine.
            assert!(
                !pg_idx.is_empty(),
                "{table} reported no indexes; the pg_indexes query has drifted \
                 and every comparison here would pass vacuously"
            );
            assert_eq!(
                pg_idx,
                index_names(&sqlite, table).await,
                "{table} has different indexes on Postgres than on SQLite"
            );
        }
    }

    /// `down()` is dead weight unless it actually drops the tables.
    #[tokio::test]
    async fn the_down_migration_drops_every_table() {
        let conn = migrated_db().await;
        let manager = sea_orm_migration::SchemaManager::new(&conn);

        // `super::Migration` directly, **not** a loop over
        // `Migrator::migrations()`. That loop is correct only while this gear
        // has exactly one migration: `migrations()` returns declaration order
        // and `down()` has to run in the reverse of it.
        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        // Exactly this migration's tables are gone, and exactly the later
        // migrations' tables remain — `down()` above was this file's alone.
        // Asserting "empty" was right while the gear had one migration and
        // became a false expectation the moment it had two; asserting the
        // remainder keeps the test measuring what it always meant, which is
        // that `down()` drops **its own** eleven and touches nothing else.
        let mut remaining = table_names(&conn).await;
        remaining.sort();
        let mut expected = TABLES_OWNED_BY_LATER_MIGRATIONS.to_vec();
        expected.sort_unstable();
        assert_eq!(
            remaining, expected,
            "every table this migration created must be gone after down(), and no other"
        );
    }
}
