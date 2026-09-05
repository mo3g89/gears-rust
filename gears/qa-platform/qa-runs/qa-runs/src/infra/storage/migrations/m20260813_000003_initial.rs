//! Initial schema: runs, the per-platform run queue, and per-test results.
//!
//! Follows the two shipped sibling gears' migration shape — a backend `match`
//! producing one `execute_unprepared` DDL blob per dialect (DESIGN §3.7,
//! "Database Schemas & Tables"). Column, index and FK order is kept identical
//! across `POSTGRES_UP`, `MYSQL_UP` and `SQLITE_UP`, because a three-way
//! eyeball diff is the only thing that catches a forgotten dialect: the tests
//! below exercise `SQLite` only.
//!
//! ## A note on how `DESIGN.md` is cited here
//!
//! **By section and heading text, not by line number.** `DESIGN.md` §3.7 has
//! drifted three times during this gear's implementation — twice from edits
//! made by this very task — and each time it silently repointed a citation at a
//! *different* rule, which is worse than no citation. A line number that is
//! right today and wrong next week is a trap; a heading is greppable forever.
//! Where a target has no heading, the line number is given together with enough
//! quoted text to re-find it. Please do not "helpfully" restore the numbers.
//!
//! ## Nothing links these names to the entities at compile time
//!
//! `SeaORM` entities are hand-written structs whose table and column names are
//! runtime strings, so a typo here — or in `entity/*.rs` — compiles cleanly and
//! fails at query time. `cargo build` is therefore *no* evidence that this file
//! and `super::super::entity` agree. The `#[cfg(test)] mod tests` at the bottom
//! is: it runs this migration against an in-memory `SQLite` database and then
//! round-trips a row through every entity, which is the only thing that
//! actually exercises all three table names and every column name.
//!
//! ## The run record is one row, not two
//!
//! The source system keeps a run in two halves and merges them on read — the
//! Argo `Workflow` object plus a persisted `run_results` row
//! (`manager/src/services/run_history.rs:10-48`, merged by
//! `overlay_persisted_with_live`, `run_history.rs:216`) — because the Workflow
//! is garbage-collected after its TTL while re-runs happen much later. So the
//! column inventory of `qa_runs` is the *union* of both halves, but there is no
//! live/persisted overlay to port: `cpt-cf-qa-principle-db-first-state` makes
//! this row authoritative on its own.
//!
//! ## `app_version` and `app_build` are snapshots, never re-derived
//!
//! Both nullable text, written once at launch from the target platform. The
//! source system does the same — `let app_version = platform_version;`
//! (`manager/src/routes/runs.rs:594`), from the platform lookup at `:579` — and
//! persists the result (`manager/migrations/001_initial.sql:56` for
//! `app_version`, `:149` for `app_build`) rather than re-reading the platform.
//! Re-deriving from `platform_id` would let a platform upgrade silently change
//! a queued run's or a re-run's `APP_VERSION`, breaking reproducibility and
//! `PRD.md:577`'s preserved-env-var contract.
//!
//! Note what legacy does *not* say: `migrations/001_initial.sql:306-310`, which
//! spells the GC rationale out ("Persisted because the Argo Workflow object is
//! collected after its TTL while reruns happen much later"), is attached to the
//! `exclusive` column, not to `app_version`. `app_version` sits in the original
//! `run_results` table with no comment at all. The rationale transfers by
//! analogy — the same GC window applies to every column of that row — but it is
//! an inference, not a quotation.
//!
//! ## Every unique index is tenant-prefixed
//!
//! The rule, and the squatting/oracle argument behind it, are stated in full
//! under this same heading in DESIGN §3.7 and repeated for implementers in
//! `qa-catalog`'s migration, which is the one repetition DESIGN designates.
//! Not repeated a third time here.
//!
//! What is specific to *this* schema: both unique indexes sit on tables whose
//! parent already carries the tenant, so both are exactly the case the rule is
//! about — `idx_qa_run_queue_tenant_run` most sharply, because `run_id` is
//! globally unique on its own and the prefix therefore looks redundant right
//! up until someone learns a victim's `run_id`.
//! `a_queue_row_is_unique_per_tenant_not_globally` is the executable form of
//! that argument.
//!
//! ## The foreign keys are tenant-blind, and that is an existence oracle
//!
//! **Corrected 2026-08-13 by the security review.** This section previously
//! argued that the tenant-prefixed unique index removes both harms and leaves
//! only "an invisible orphan row that `ON DELETE CASCADE` reaps." The row half
//! is true and the conclusion was wrong, because **the oracle was never a
//! property of the row — it is a property of the response.**
//!
//! `run_id REFERENCES qa_runs(id)` carries no tenant component, so an insert
//! with an attacker-chosen `run_id` **succeeds if that run exists in any
//! tenant** and **fails with a foreign-key violation if it does not**. That is
//! a working membership test over another tenant's run identifiers, and the
//! tenant-prefixed unique index does nothing about it — the index governs
//! collisions, the FK governs existence.
//!
//! The test module below proves both halves without meaning to:
//! `a_queue_row_is_unique_per_tenant_not_globally` asserts a squatter's insert
//! against the victim's `run_id` **succeeds**, and
//! `children_require_their_run_and_cascade_with_it` asserts an insert against a
//! nonexistent `run_id` **fails**. Those two assertions together are the
//! discriminator.
//!
//! ### The obligation this puts on every writer
//!
//! **Every write that touches a caller-supplied `run_id` must first resolve
//! that run under the caller's own scope and return not-found if it is
//! missing** — `find run by (tenant, run_id)` → `DomainError::RunNotFound`,
//! *before* the insert. Done that way the foreign key never gets to answer, and
//! it degrades to what it should be: a last-resort integrity check that fires
//! only on a bug. Skipping the precheck and letting the FK violation surface as
//! a `Database` error is the shape of the defect, not a shortcut around it.
//!
//! This is a service-layer obligation because it has to be: `secure_insert`
//! validates the `tenant_id` column of the row being written and nothing else —
//! it cannot know that a *referenced* parent belongs to the same tenant.
//!
//! ### Why the composite FK is still declined
//!
//! `(tenant_id, run_id) REFERENCES qa_runs(tenant_id, id)` would close the
//! oracle in the database, at the cost of a redundant `UNIQUE (tenant_id, id)`
//! on the parent in all three dialects. It stays declined because neither
//! shipped sibling gear uses one and because the precheck above is required
//! anyway — a caller must get `NotFound`, not a constraint error, and only the
//! service can produce that. But the residual is now written down, which is the
//! part that was missing: an undocumented residual is a finding by this
//! codebase's own standard.
//!
//! ## The queue row carries no execution handle
//!
//! Legacy's `run_queue` has a `workflow_name` column
//! (`manager/migrations/001_initial.sql:297`) and needs one: legacy keeps **no
//! run row for a queued launch**, so the queue row is the only handle on the
//! execution — `all_claims` reads `workflow_name` straight off it to decide
//! whether a claim's workflow is still alive
//! (`manager/src/services/run_queue.rs:333-337`,
//! `manager/src/services/run_dispatcher.rs:440-457`).
//!
//! That reason does not transfer. Here `qa_run_queue.run_id` points at a
//! `qa_runs` row that already owns `execution_ref`, so a column here would be a
//! second home for one execution — two places that can disagree, with nothing
//! deciding which is right. `qa_runs_sdk::QueueEntry` omits the field for
//! exactly this reason, and a column no code may write is worse than a missing
//! one: the next implementer reads it as authoritative and populates it. The
//! claim reconciler joins through `run_id` instead.
//!
//! DESIGN §3.7's qa-runs table list previously named `execution_ref` on `run_queue`; that entry was
//! removed in the same change as this migration.
//!
//! ## Obligations this schema hands to later tasks
//!
//! Five things the DDL cannot enforce and the column comments state in place.
//! Collected here so they have one findable home, because each is invisible at
//! the point where it would be violated:
//!
//! 1. **Resolve `run_id` under the caller's scope before any write that uses
//!    it** — the foreign keys are tenant-blind and answer an existence
//!    question. See "The foreign keys are tenant-blind" above.
//! 2. **Validate platform ownership at launch**, through a tenant-scoped
//!    qa-environments client. `qa_environment_leases` (renamed from
//!    `qa_platform_leases`) is keyed on a bare `platform_id`, so the lease is
//!    not tenant-partitioned. See the comment on `qa_runs.platform_id`.
//! 3. **The per-test `status` vocabulary lives only in this file.** Eight known
//!    uppercase values, an open set, and a fixed mapping onto the five
//!    counters — including that `XFAIL`/`XPASS` count toward `total` and
//!    nothing else. Ingest must match the spellings exactly and must *not*
//!    fail closed on an unknown one. See the comment on
//!    `qa_run_test_results.status`.
//! 4. **An all-zero count row is indistinguishable from a clean run.** See the
//!    `KNOWN GAP` block on the count columns for why this schema cannot close
//!    it and which two fixes can.
//! 5. **The `INTEGER`/`usize` seam belongs to the mapper**, failing closed on a
//!    negative rather than casting. See `entity/run.rs`'s note on the counters.
//!
//! ## `MySQL` key-width budget (`InnoDB`, `utf8mb4`, 3072-byte limit)
//!
//! Widest index shipped is `idx_qa_runs_tenant_name` at 36*4 + 255*4 = 1164
//! bytes. `idx_qa_run_queue_fifo` is 36*4 + 36*4 + 16*4 + 4 + 36*4 = 500 bytes
//! (`TIMESTAMP` is 4 bytes). `idx_qa_run_queue_tenant_run` and
//! `idx_qa_run_test_results_run` are 36*4 + 36*4 = 288. All fit, so no column
//! had to shrink. The one tuple that would *not* fit is the rejected per-test
//! unique index, at 6432 bytes — see below.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_runs (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    run_kind VARCHAR(16) NOT NULL,
    -- Target, flattened. A discriminated column set rather than one JSON blob:
    -- the dispatcher filters on `target_repo_id` and the UI groups by it, and a
    -- JSON blob would make both an unindexable scan. `run_kind` says which
    -- columns are meaningful; the mapper enforces that and fails closed on an
    -- inconsistent row (never a permissive default -- a run whose target
    -- silently decoded to nothing would execute nothing).
    target_repo_id UUID NULL,
    target_path VARCHAR(1024) NULL,
    target_test_file VARCHAR(1024) NULL,
    target_custom_plan_id UUID NULL,
    -- Unconstrained on purpose: the platform lives in qa-environments and
    -- DESIGN section 3.7 forbids cross-schema FKs (cross-gear references are
    -- by ID only). The obligation that replaces
    -- the missing constraint: **the launch path must verify the caller's
    -- tenant owns this platform, through a tenant-scoped qa-environments
    -- client, before persisting the row.** Nothing downstream re-checks.
    -- Why it matters more here than for a normal cross-gear id:
    -- `qa_platform_leases`' primary key is a bare `platform_id`, so the lease
    -- is **not tenant-partitioned**, and `domain/queue.rs` makes that lease the
    -- authoritative admission decision. A run row carrying another tenant's
    -- `platform_id` therefore drives its dispatcher to acquire the *global*
    -- lease on that platform and block the owning tenant's runs -- a
    -- cross-tenant denial of service reached entirely through legitimate,
    -- correctly-scoped writes to this gear's own tables.
    platform_id UUID NULL,
    test_version VARCHAR(512) NULL,
    -- Snapshotted from the target platform at launch, never re-derived. See the
    -- module header. Width matches the column they are copied from,
    -- `qa_platforms.observed_version VARCHAR(255)`.
    app_version VARCHAR(255) NULL,
    app_build VARCHAR(255) NULL,
    -- `qa_runs_sdk::RunState::as_str` is the sole encoder; ten values, longest
    -- `dispatching` (11). The mapper's decoder must reject anything else rather
    -- than fall back to a default: a corrupt state that decoded to `Created`
    -- would re-run finished work, and one that decoded to `Succeeded` would
    -- report unexecuted work as passing.
    state VARCHAR(16) NOT NULL,
    -- `resolved_exclusive`, not `exclusive`: this is the decision the
    -- exclusivity resolver reached, not the tri-state `Option<bool>` a launch
    -- requests. DESIGN section 3.7's qa-runs table list and
    -- `qa_runs_sdk::Run::resolved_exclusive` both
    -- spell it this way, and the SDK field's doc records why the short name is
    -- a trap (it makes the wrong re-run transcription type-check). The queue
    -- row keeps the short name -- different table, different vocabulary.
    resolved_exclusive BOOLEAN NOT NULL,
    exclusive_tier VARCHAR(16) NOT NULL,
    -- The only scalar default in this table, and the reason is parity: legacy
    -- declares `is_validation BOOLEAN NOT NULL DEFAULT FALSE` on `run_results`
    -- (`manager/migrations/001_initial.sql:57`), so a row written without an
    -- opinion is a non-validation run in both systems. FALSE is also the safe
    -- direction: a validation run is stamped with an extra annotation, so a
    -- wrong default omits a label rather than inventing one.
    is_validation BOOLEAN NOT NULL DEFAULT FALSE,
    parameters JSONB NOT NULL DEFAULT '[]',
    include_tags JSONB NOT NULL DEFAULT '[]',
    exclude_tags JSONB NOT NULL DEFAULT '[]',
    source VARCHAR(16) NOT NULL,
    schedule_id UUID NULL,
    bundle_ids JSONB NOT NULL DEFAULT '[]',
    execution_ref VARCHAR(512) NULL,
    log_storage_ref VARCHAR(2048) NULL,
    timeout_at TIMESTAMPTZ NULL,
    started_at TIMESTAMPTZ NULL,
    finished_at TIMESTAMPTZ NULL,
    error TEXT NULL,
    -- Denormalized counts. Net-new: the source system stores none of these and
    -- recomputes them per query as
    -- `COUNT(*) FILTER (WHERE tr.status = 'PASSED')` and friends
    -- (`manager/src/routes/plans.rs:188-192`, which produces exactly these five
    -- numbers). They are columns here because
    -- `cpt-cf-qa-fr-runs-results-ingest` requires the run's counts to be
    -- updated incrementally as events arrive, and because
    -- `domain::state_machine::derive_terminal_state` reads `failed` to decide
    -- the run's terminal state -- a rule that must not depend on an aggregate
    -- over a second table. (`skipped` no longer votes on that verdict --
    -- product owner decision, 2026-08-28, Task 10 -- but it is still one of
    -- the five counts this column set exists to update incrementally, so it
    -- stays a column for that reason alone.)
    -- `failed` is the failed-*or-errored* count, matching legacy's fold at
    -- `plans.rs:189`; there is deliberately no `error` column.
    --
    -- KNOWN GAP, recorded 2026-08-13 by the security review. `DEFAULT 0` makes
    -- a never-ingested run byte-identical to a run in which every test passed.
    -- `derive_terminal_state` reads only `failed > 0` (as of Task 10; this
    -- migration originally shipped when the rule also read `skipped > 0`, and
    -- an all-zero row triggers the gap under either reading), so an
    -- all-zero row plus a `Succeeded` executor outcome yields `Succeeded` -- a
    -- run whose ingest was dropped reports as a clean pass. That sits against
    -- the invariant `domain::state_machine`'s own test states in words: a run
    -- whose work never ran must never read as passing.
    --
    -- Not closable in this schema, and this is the argument rather than an
    -- excuse. The distinguishing fix is a nullable count set (NULL = nothing
    -- ingested, 0 = ingested and empty), but the counts surface through
    -- `qa_runs_sdk::RunResult`, whose five fields are plain `usize`, and
    -- `derive_terminal_state` consumes that type by value. Making them optional
    -- is a change to a shipped SDK contract and to a shipped pure domain
    -- module, neither of which this migration owns; doing it here would leave
    -- the schema and the contract disagreeing. The alternative -- inventing a
    -- `results_ingested_at` column -- would add a column no consuming task
    -- knows to write or read.
    --
    -- So the fix belongs to whoever owns ingest and completion, and it is one
    -- of two things: a positive record that ingest ran, or a completion guard
    -- that refuses to record `Succeeded` for a run with `total = 0` and a
    -- non-empty plan. Legacy could not have this bug and also could not have
    -- fixed it -- it stored no counts and recomputed them per query
    -- (`plans.rs:188-192`), so an empty result set and a missing ingest were
    -- the same fact
    -- there too.
    passed INTEGER NOT NULL DEFAULT 0,
    failed INTEGER NOT NULL DEFAULT 0,
    skipped INTEGER NOT NULL DEFAULT 0,
    in_progress INTEGER NOT NULL DEFAULT 0,
    total INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_runs_tenant_name ON qa_runs(tenant_id, name);
-- Timeout sweep and the active-run count. Not tenant-prefixed on purpose: the
-- dispatcher enumerates across tenants and then writes under a per-tenant
-- system context -- the pattern qa-catalog establishes in
-- `system_actor::for_branch_refresh_enumeration` (nil tenant, enumeration
-- only) followed by `system_actor::for_branch_refresh(tenant_id)` per write.
-- Cited by function name rather than line, per this module's header: those are
-- live code and drift. This gear gets its own `domain::system_actor` with the
-- dispatcher. So the enumeration wants the state leading, and being non-unique
-- this is not a cross-tenant channel.
CREATE INDEX IF NOT EXISTS idx_qa_runs_state_timeout ON qa_runs(state, timeout_at);
CREATE INDEX IF NOT EXISTS idx_qa_runs_tenant_platform_state ON qa_runs(tenant_id, platform_id, state);
CREATE INDEX IF NOT EXISTS idx_qa_runs_tenant_schedule ON qa_runs(tenant_id, schedule_id);

CREATE TABLE IF NOT EXISTS qa_run_queue (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- Same obligation as `qa_runs.platform_id`: ownership is verified at
    -- launch, not here, and the lease this id resolves to is not
    -- tenant-partitioned. See that column's comment.
    platform_id UUID NOT NULL,
    -- Tenant-blind FK: an insert with a caller-supplied `run_id` succeeds iff
    -- that run exists in *some* tenant, which is an existence oracle. The
    -- service must resolve the run under the caller's scope and return
    -- `RunNotFound` before reaching this insert, so the constraint never
    -- answers. See the module header.
    run_id UUID NOT NULL REFERENCES qa_runs(id) ON DELETE CASCADE,
    -- `run_kind`, `source` and `exclusive` are denormalized from the run so the
    -- FIFO planner needs no join, exactly as the source system's `run_queue`
    -- carries them (`manager/migrations/001_initial.sql:291`, `:294`, `:295`).
    run_kind VARCHAR(16) NOT NULL,
    source VARCHAR(16) NOT NULL,
    exclusive BOOLEAN NOT NULL,
    -- Seven states: queued / dispatching / running / done / failed / cancelled
    -- / expired. Frozen vocabulary -- see DESIGN section 3.7's decision-D1
    -- amendment and
    -- `../testrunner/docs/guides/exclusive-runs-and-the-queue.md` lines 88-96.
    -- `dispatching` and `running` are the two that hold a claim on the platform
    -- (`manager/src/services/run_queue.rs:104`, `CLAIM_STATES`).
    -- `cancelled` has two `l`s here and the *run*'s `canceled` has one; that is
    -- deliberate. This spelling is the ported one
    -- (`manager/src/services/run_queue.rs:413`, `SET state = 'cancelled'`);
    -- the run vocabulary is DESIGN's own and legacy has no run-level cancel
    -- state at all. Do not unify them.
    state VARCHAR(16) NOT NULL,
    -- No `execution_ref` here, deliberately: legacy's `workflow_name`
    -- (`001_initial.sql:297`) exists because legacy has no run row to hang it
    -- on, and that reason does not transfer. See this module's header.
    error TEXT NULL,
    -- `enqueued_at` is the domain fact: the FIFO sort key
    -- (`manager/src/services/run_queue.rs:248`) and the TTL sweep's clock
    -- (`run_queue.rs:379`, `WHERE state = 'queued' AND enqueued_at < $1`).
    -- `created_at` beside it is the house-style audit column DESIGN section
    -- 3.7 requires of every table
    -- requires; the two are equal at insert and only a future re-enqueue would
    -- separate them.
    enqueued_at TIMESTAMPTZ NOT NULL,
    dispatched_at TIMESTAMPTZ NULL,
    finished_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
-- One queue row per run. Net-new: legacy's `run_queue` has no `run_id` column
-- at all (it carries `target_id` + a serialized `intent`,
-- `manager/migrations/001_initial.sql:292-293`), so there is nothing to port --
-- the invariant is ours, and it holds because a re-run creates a *new* run row
-- rather than re-queueing the old one.
-- Tenant-prefixed even though `run_id` is already globally unique per run: see
-- this migration module's header, section `Every unique index is
-- tenant-prefixed`. Without the prefix, learning a victim's `run_id` is enough
-- to squat it.
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_run_queue_tenant_run ON qa_run_queue(tenant_id, run_id);
-- The dispatch path: FIFO within a platform. Column order mirrors the query's
-- filter and ORDER BY (`manager/src/services/run_queue.rs:243-257`:
-- `WHERE platform = $1 AND state = 'queued' ORDER BY enqueued_at ASC, id ASC`)
-- so one index serves both. `id` is in the index because it is in the sort;
-- legacy's equivalent stops at `enqueued_at`
-- (`manager/migrations/001_initial.sql:303`).
CREATE INDEX IF NOT EXISTS idx_qa_run_queue_fifo
    ON qa_run_queue(tenant_id, platform_id, state, enqueued_at, id);
-- The TTL sweep and claim reconciliation, both cross-tenant enumerations, both
-- filtering on state alone (`run_queue.rs:379`, `:335`). Legacy indexes the
-- same way (`001_initial.sql:304`, `idx_run_queue_state`).
CREATE INDEX IF NOT EXISTS idx_qa_run_queue_state_enqueued ON qa_run_queue(state, enqueued_at);

CREATE TABLE IF NOT EXISTS qa_run_test_results (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- Tenant-blind FK, same existence oracle and same obligation as
    -- `qa_run_queue.run_id`. The ingest path is the writer here, and it takes
    -- `run_id` straight from an executor-supplied event, so the tenant-scoped
    -- lookup is doing double duty: it is also what decides whether the event
    -- belongs to this tenant at all. See the module header.
    run_id UUID NOT NULL REFERENCES qa_runs(id) ON DELETE CASCADE,
    -- NOT NULL DEFAULT '' where legacy is a nullable TEXT
    -- (`manager/migrations/001_initial.sql:166`). Legacy writes NULL for a
    -- missing file (`manager/src/routes/runs.rs:1173`,
    -- `if test_file.is_empty() { None } else { .. }`) and then has to write
    -- `COALESCE(test_file, '')` at every comparison (`runs.rs:1154`).
    -- Normalizing to `''` on write makes the dedupe predicate plain equality
    -- and removes the third state (`NULL` vs `''`) that COALESCE exists to
    -- collapse.
    test_file VARCHAR(1024) NOT NULL DEFAULT '',
    test_name VARCHAR(512) NOT NULL,
    -- The one enum-shaped column in this schema whose vocabulary lives nowhere
    -- else: there is no `TestStatus` in `qa-runs-sdk`, and the per-test row has
    -- no SDK counterpart at all. So this comment is the definitive record, and
    -- Task 12's ingest must match these spellings exactly -- the counts on
    -- `qa_runs` are derived by matching on them, and a misspelling silently
    -- under-counts rather than failing.
    --
    -- Eight known values, all uppercase:
    --   PASSED FAILED ERROR SKIPPED PENDING RUNNING XFAIL XPASS
    -- Longest is 7 characters, hence VARCHAR(16).
    --
    -- How they map onto the five counters, which is the load-bearing part
    -- (`manager/src/routes/plans.rs:188-192`, the projection those counters are
    -- ported from):
    --   passed      <- PASSED
    --   failed      <- FAILED, ERROR          (the fold; see the counts comment)
    --   skipped     <- SKIPPED
    --   in_progress <- PENDING, RUNNING
    --   total       <- every row
    -- Note what that means: XFAIL and XPASS land in `total` and in **none** of
    -- the four buckets, so the four do not sum to the total on a run that has
    -- expected-failure results. Legacy counts them separately in its analytics
    -- projection (`manager/src/routes/analytics.rs:1359-1360`) and never folded
    -- them into the five. Do not correct that by adding them to `passed`.
    --
    -- **This set is open, and the mapper must NOT fail closed on it.** That is
    -- the opposite of the `qa_runs.state` rule above, and the difference is
    -- deliberate rather than an oversight. Two legacy paths put unconstrained
    -- text here: the runner's progress event carries `status: String` with no
    -- validation between the wire and the INSERT
    -- (`manager/src/routes/runs.rs:1115`, written at `:1169`), and the
    -- case-level mapper falls through to `other.to_uppercase()` for any pytest
    -- outcome outside its six-way map
    -- (`manager/src/services/argo.rs:2932-2943`). A ninth value is therefore a
    -- runner change, not corruption, and rejecting it would drop a real result.
    -- Count what is recognized; store what arrives.
    status VARCHAR(16) NOT NULL,
    -- Text, not a number, and not an integer millisecond column. The source
    -- system stores the runner's duration string verbatim and deliberately
    -- keeps extended forms: `parse_test_results_keeps_extended_duration_text`
    -- (`manager/src/services/argo.rs:3279`) pins `85.06s (0:01:25)`, which no
    -- numeric column can round-trip.
    --
    -- All three widths here narrow legacy's unbounded TEXT
    -- (`manager/migrations/001_initial.sql:167` for `duration`, `:70-71` for
    -- the other two). None of the three is indexed, so no key-width limit
    -- forces the choice and the budget is content alone. `duration`: the
    -- longest form the parser is known to emit is that 17-character string,
    -- so 64 leaves ~3x headroom on the one column that must round-trip
    -- verbatim. `launch_id`: a ReportPortal launch id, numeric in practice
    -- (`7204`); 255 matches the width this schema gives other external
    -- identifiers. `jira_key`: a project key plus a number (`VHP-2618`), well
    -- inside 64. If a runner ever emits something longer the result is an
    -- insert error, not silent corruption -- widen the column rather than
    -- trimming the value.
    duration VARCHAR(64) NULL,
    -- The ReportPortal launch link and the linked Jira issue, both optional,
    -- both carried on the runner's progress event
    -- (`manager/src/routes/runs.rs:1111-1122`, `ProgressPayload`) and both
    -- columns on legacy's `test_results` (`001_initial.sql:70-71`).
    launch_id VARCHAR(255) NULL,
    jira_key VARCHAR(64) NULL,
    -- `updated_at` is the house-style column DESIGN section 3.7 requires of
    -- every table. Under
    -- delete-then-insert dedupe it always equals `created_at`, because a row is
    -- replaced rather than updated in place.
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
-- Deliberately NOT unique on (tenant_id, run_id, test_file, test_name), for two
-- reasons that point the same way. (1) Parity: the source system deduplicates
-- by delete-then-insert on that tuple -- `DELETE FROM test_results WHERE run_id
-- = $1 AND test_name = $2 AND COALESCE(test_file, '') = $3` followed by an
-- unconditional INSERT (`manager/src/routes/runs.rs:1153-1185`) -- and the
-- ingest service does the same, so the uniqueness is an application invariant,
-- not a constraint. (2) Key width: that tuple is 36*4 + 36*4 + 1024*4 + 512*4 =
-- 6432 bytes under utf8mb4, well past InnoDB's 3072-byte limit, so the index
-- could not be created on MySQL without prefix lengths anyway.
CREATE INDEX IF NOT EXISTS idx_qa_run_test_results_run ON qa_run_test_results(tenant_id, run_id);
";

const MYSQL_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_runs (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    run_kind VARCHAR(16) NOT NULL,
    target_repo_id VARCHAR(36) NULL,
    target_path VARCHAR(1024) NULL,
    target_test_file VARCHAR(1024) NULL,
    target_custom_plan_id VARCHAR(36) NULL,
    platform_id VARCHAR(36) NULL,
    test_version VARCHAR(512) NULL,
    app_version VARCHAR(255) NULL,
    app_build VARCHAR(255) NULL,
    state VARCHAR(16) NOT NULL,
    resolved_exclusive BOOLEAN NOT NULL,
    exclusive_tier VARCHAR(16) NOT NULL,
    is_validation BOOLEAN NOT NULL DEFAULT FALSE,
    -- Expression defaults (MySQL 8.0.13+) for symmetry with the Postgres
    -- (`JSONB NOT NULL DEFAULT '[]'`) and SQLite (`TEXT NOT NULL DEFAULT '[]'`)
    -- definitions. The literal form is rejected for JSON/TEXT columns; the
    -- parenthesised form is not, so there is no cross-dialect asymmetry to
    -- document here. Application code always writes these columns explicitly.
    parameters JSON NOT NULL DEFAULT ('[]'),
    include_tags JSON NOT NULL DEFAULT ('[]'),
    exclude_tags JSON NOT NULL DEFAULT ('[]'),
    source VARCHAR(16) NOT NULL,
    schedule_id VARCHAR(36) NULL,
    bundle_ids JSON NOT NULL DEFAULT ('[]'),
    execution_ref VARCHAR(512) NULL,
    log_storage_ref VARCHAR(2048) NULL,
    timeout_at TIMESTAMP NULL,
    started_at TIMESTAMP NULL,
    finished_at TIMESTAMP NULL,
    error TEXT NULL,
    passed INTEGER NOT NULL DEFAULT 0,
    failed INTEGER NOT NULL DEFAULT 0,
    skipped INTEGER NOT NULL DEFAULT 0,
    in_progress INTEGER NOT NULL DEFAULT 0,
    total INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_runs_tenant_name (tenant_id, name),
    KEY idx_qa_runs_state_timeout (state, timeout_at),
    KEY idx_qa_runs_tenant_platform_state (tenant_id, platform_id, state),
    KEY idx_qa_runs_tenant_schedule (tenant_id, schedule_id)
);

CREATE TABLE IF NOT EXISTS qa_run_queue (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    platform_id VARCHAR(36) NOT NULL,
    run_id VARCHAR(36) NOT NULL,
    run_kind VARCHAR(16) NOT NULL,
    source VARCHAR(16) NOT NULL,
    exclusive BOOLEAN NOT NULL,
    state VARCHAR(16) NOT NULL,
    error TEXT NULL,
    enqueued_at TIMESTAMP NOT NULL,
    dispatched_at TIMESTAMP NULL,
    finished_at TIMESTAMP NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_run_queue_tenant_run (tenant_id, run_id),
    KEY idx_qa_run_queue_fifo (tenant_id, platform_id, state, enqueued_at, id),
    KEY idx_qa_run_queue_state_enqueued (state, enqueued_at),
    CONSTRAINT fk_qa_run_queue_run FOREIGN KEY (run_id) REFERENCES qa_runs(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS qa_run_test_results (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    run_id VARCHAR(36) NOT NULL,
    test_file VARCHAR(1024) NOT NULL DEFAULT '',
    test_name VARCHAR(512) NOT NULL,
    status VARCHAR(16) NOT NULL,
    duration VARCHAR(64) NULL,
    launch_id VARCHAR(255) NULL,
    jira_key VARCHAR(64) NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    KEY idx_qa_run_test_results_run (tenant_id, run_id),
    CONSTRAINT fk_qa_run_test_results_run FOREIGN KEY (run_id) REFERENCES qa_runs(id) ON DELETE CASCADE
);
";

const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_runs (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    run_kind TEXT NOT NULL,
    target_repo_id TEXT NULL,
    target_path TEXT NULL,
    target_test_file TEXT NULL,
    target_custom_plan_id TEXT NULL,
    platform_id TEXT NULL,
    test_version TEXT NULL,
    app_version TEXT NULL,
    app_build TEXT NULL,
    state TEXT NOT NULL,
    resolved_exclusive INTEGER NOT NULL,
    exclusive_tier TEXT NOT NULL,
    is_validation INTEGER NOT NULL DEFAULT 0,
    parameters TEXT NOT NULL DEFAULT '[]',
    include_tags TEXT NOT NULL DEFAULT '[]',
    exclude_tags TEXT NOT NULL DEFAULT '[]',
    source TEXT NOT NULL,
    schedule_id TEXT NULL,
    bundle_ids TEXT NOT NULL DEFAULT '[]',
    execution_ref TEXT NULL,
    log_storage_ref TEXT NULL,
    timeout_at TEXT NULL,
    started_at TEXT NULL,
    finished_at TEXT NULL,
    error TEXT NULL,
    passed INTEGER NOT NULL DEFAULT 0,
    failed INTEGER NOT NULL DEFAULT 0,
    skipped INTEGER NOT NULL DEFAULT 0,
    in_progress INTEGER NOT NULL DEFAULT 0,
    total INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_runs_tenant_name ON qa_runs(tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_qa_runs_state_timeout ON qa_runs(state, timeout_at);
CREATE INDEX IF NOT EXISTS idx_qa_runs_tenant_platform_state ON qa_runs(tenant_id, platform_id, state);
CREATE INDEX IF NOT EXISTS idx_qa_runs_tenant_schedule ON qa_runs(tenant_id, schedule_id);

CREATE TABLE IF NOT EXISTS qa_run_queue (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    platform_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    run_kind TEXT NOT NULL,
    source TEXT NOT NULL,
    exclusive INTEGER NOT NULL,
    state TEXT NOT NULL,
    error TEXT NULL,
    enqueued_at TEXT NOT NULL,
    dispatched_at TEXT NULL,
    finished_at TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (run_id) REFERENCES qa_runs(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_run_queue_tenant_run ON qa_run_queue(tenant_id, run_id);
CREATE INDEX IF NOT EXISTS idx_qa_run_queue_fifo ON qa_run_queue(tenant_id, platform_id, state, enqueued_at, id);
CREATE INDEX IF NOT EXISTS idx_qa_run_queue_state_enqueued ON qa_run_queue(state, enqueued_at);

CREATE TABLE IF NOT EXISTS qa_run_test_results (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    test_file TEXT NOT NULL DEFAULT '',
    test_name TEXT NOT NULL,
    status TEXT NOT NULL,
    duration TEXT NULL,
    launch_id TEXT NULL,
    jira_key TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (run_id) REFERENCES qa_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_qa_run_test_results_run ON qa_run_test_results(tenant_id, run_id);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::MySql => MYSQL_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = r"
DROP TABLE IF EXISTS qa_run_test_results;
DROP TABLE IF EXISTS qa_run_queue;
DROP TABLE IF EXISTS qa_runs;
        ";
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}

/// Schema tests.
///
/// These exist because **`cargo build` proves nothing about this file.** The
/// entity structs in `super::super::entity` name their table and every one of
/// their columns as runtime strings; a typo in either place compiles cleanly
/// and fails at query time. The only thing that links the two is a query, so
/// each test below issues one against the real migration running on an
/// in-memory `SQLite` database.
///
/// What they cover: all three table names, every column name (a `SeaORM`
/// `INSERT` names them all, and the `SELECT` behind `find_by_id` reads them all
/// back), both `NOT NULL DEFAULT ''` clauses, both foreign keys, and the
/// tenant-prefixing of both unique indexes. What they do **not** cover: the
/// `MYSQL_UP` and `POSTGRES_UP` blobs, neither of which *these* tests run —
/// against this tier they stay a three-way eyeball diff, and the parity tests
/// below are what compare their declarations.
///
/// **Corrected 2026-08-17**, because that sentence used to end *"which nothing
/// in this workspace executes"* and half of it was false. `POSTGRES_UP` **is**
/// executed: `infra::storage::test_db`'s `pg_db` applies this gear's real
/// `Migrator` against a Postgres container whenever the `integration` feature is
/// on, so that blob's syntax is checked — just not by the default gate.
/// `MYSQL_UP` is the half that stands: nothing on any tier runs it, and only its
/// declarations are checked. The sibling `m20260813_000004_schedules` recorded
/// the correction against this file when it landed; two tasks then declined to
/// make it here as outside their ownership, which is how a known-false sentence
/// survives.
///
/// ## Why this module talks to a raw `SeaORM` connection
///
/// `clippy::disallowed_methods` normally forbids `Select::one` and
/// `DeleteMany::exec` outside the `SecureORM` wrappers, and that rule is right
/// for every production path: it is what stops a query from escaping tenant
/// scoping. It is allowed here, and only here, because **a schema test needs a
/// raw connection and `SecureORM` deliberately exposes none.** `secure_insert`
/// and `scope_with` require an `AccessScope`, which is a policy object a
/// migration-module test has no business constructing, and `DbConn` implements
/// no `ConnectionTrait`, so `execute_unprepared` — which the
/// `NOT NULL DEFAULT ''` test needs, since no `ActiveModel` insert can omit a
/// column — is unreachable through it. Nothing here is a production path: the
/// module is `#[cfg(test)]`, and the real tenant-scoping tests arrive with the
/// repositories, which do go through `SecureORM`.
///
/// **Corrected 2026-08-13.** This paragraph previously gave two further
/// reasons, and both were wrong. It claimed the tests "must observe rows across
/// tenants" — they do not; both squat tests assert only on whether an *insert*
/// succeeds, which the scoped path could express. And it claimed
/// `PRAGMA foreign_keys = ON` was load-bearing because `toolkit-db` does not
/// issue it. See the note on that pragma in `migrated_db` below: it is a
/// no-op.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{
        ActiveModelTrait, ActiveValue, ConnectOptions, ConnectionTrait, Database,
        DatabaseConnection, EntityTrait,
    };
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::{run, run_queue, run_test_result};

    /// Deterministic fixture UUIDs. `Uuid::new_v4` would depend on a cargo
    /// feature this crate does not ask for, and would make a failure
    /// unreproducible.
    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 2026-08-13 00:00:00 UTC, the same fixture instant `domain` uses.
    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
    }

    /// Every column name of every table, per dialect, in declaration order.
    ///
    /// Strips `--` comment lines and blank lines, then takes the leading
    /// identifier of each remaining line inside a `CREATE TABLE` body, skipping
    /// the trailing constraint clauses (`UNIQUE`/`KEY`/`CONSTRAINT`/`FOREIGN`/
    /// `PRIMARY`/`INDEX`) that `MySQL` declares inline and the others do not.
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
    /// This replaces a control that had quietly stopped working. The module
    /// header says a three-way eyeball diff "is the only thing that catches a
    /// forgotten dialect" — but the blobs are now 68%, 6% and 0% comment by
    /// line, so they are no longer visually comparable and the diff cannot be
    /// performed by a human at all. Rationing the comments to restore
    /// diff-ability would trade the better artifact for the weaker control.
    /// This test makes comment density irrelevant instead.
    ///
    /// Break-verified: adding a column to one blob only is what turns this red,
    /// and nothing else in the suite notices.
    #[test]
    fn every_dialect_declares_the_same_columns_in_the_same_order() {
        let pg = columns_by_table(super::POSTGRES_UP);
        let my = columns_by_table(super::MYSQL_UP);
        let sq = columns_by_table(super::SQLITE_UP);

        assert_eq!(
            pg.iter().map(|(t, _)| t).collect::<Vec<_>>(),
            vec!["qa_runs", "qa_run_queue", "qa_run_test_results"],
            "the parser lost a table; every assertion below would then compare \
             two empty lists and pass vacuously"
        );
        assert!(
            pg.iter().all(|(_, c)| c.len() >= 10),
            "the parser produced a suspiciously short column list: {pg:?}"
        );
        assert_eq!(pg, my, "POSTGRES_UP and MYSQL_UP disagree");
        assert_eq!(pg, sq, "POSTGRES_UP and SQLITE_UP disagree");
    }

    /// In-memory `SQLite` with this gear's real migrations applied through the
    /// real [`super::super::Migrator`].
    ///
    /// `max_connections(1)` **is** load-bearing: each `SQLite` `:memory:`
    /// connection is its own database, so a larger pool would let a query land
    /// on a connection where the migration never ran.
    ///
    /// `PRAGMA foreign_keys = ON` is **not**. It is kept as an explicit
    /// statement of what the tests below depend on, but it changes nothing:
    /// the `sqlx` `SQLite` driver already enables foreign keys on every
    /// connection it opens.
    ///
    /// **Corrected 2026-08-13**, and worth reading before trusting any
    /// break-test result in this file. This doc, and the commit that introduced
    /// it, claimed that *removing* the pragma turned
    /// `children_require_their_run_and_cascade_with_it` red. It does not —
    /// deleting the statement leaves all 160 tests green, which is what the
    /// security review found and what re-running the mutation confirms. The
    /// honest break-test is `PRAGMA foreign_keys = OFF`, which does turn that
    /// test red, and which is what proves FK enforcement is real and the
    /// cascade assertions are not vacuous. The guard was fine; the evidence
    /// offered for it was false, and a break-test discipline is worth exactly
    /// what its weakest claim is worth.
    async fn migrated_db() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        let conn = Database::connect(opts)
            .await
            .expect("failed to connect to in-memory sqlite database");
        conn.execute_unprepared("PRAGMA foreign_keys = ON;")
            .await
            .expect("failed to enable sqlite foreign key enforcement");

        let manager = SchemaManager::new(&conn);
        for migration in super::super::Migrator::migrations() {
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-runs migrations");
        }
        conn
    }

    /// Every index declared by a dialect, as `(name, unique, columns)`.
    ///
    /// Two syntaxes to parse, which is the whole reason a divergence is easy to
    /// miss by eye: Postgres and `SQLite` write `CREATE [UNIQUE] INDEX ... ON
    /// t(cols)` as free-standing statements, `MySQL` writes `[UNIQUE] KEY name
    /// (cols)` inline in the `CREATE TABLE` body.
    fn indexes(ddl: &str) -> Vec<(String, bool, Vec<String>)> {
        let mut out = Vec::new();
        // Strip `--` lines *before* flattening: a comment above a statement
        // would otherwise be folded onto the front of it, and the statement
        // would stop matching its `CREATE ...` prefix. The cardinality
        // assertion below is what caught exactly that while this was written.
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
    /// The companion to the column test, and it exists because breaking the
    /// column one revealed the hole: changing an index in `POSTGRES_UP` alone
    /// left **all 162 tests green**. `every_declared_index_exists_with_the
    /// _declared_columns` runs against `SQLite` only, so it cannot see a
    /// server-dialect divergence, and the column parity test compares columns.
    /// Between them an index was the one declaration a forgotten dialect could
    /// still hide.
    ///
    /// Break-verified: dropping `tenant_id` from `idx_qa_runs_tenant_schedule`
    /// in `POSTGRES_UP` only is what turns this red.
    #[test]
    fn every_dialect_declares_the_same_indexes() {
        let pg = indexes(super::POSTGRES_UP);
        let my = indexes(super::MYSQL_UP);
        let sq = indexes(super::SQLITE_UP);

        assert_eq!(
            pg.len(),
            8,
            "expected 8 indexes; the parser found {} and every comparison below \
             would then be over the wrong set: {pg:?}",
            pg.len()
        );
        assert_eq!(pg, my, "POSTGRES_UP and MYSQL_UP declare different indexes");
        assert_eq!(
            pg, sq,
            "POSTGRES_UP and SQLITE_UP declare different indexes"
        );
    }

    /// Collect the `name` column of a one-column query.
    async fn name_column(conn: &DatabaseConnection, sql: String) -> Vec<String> {
        use sea_orm::{ConnectionTrait, Statement};
        conn.query_all(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            sql,
        ))
        .await
        .unwrap()
        .iter()
        .filter_map(|row| row.try_get::<String>("", "name").ok())
        .collect()
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

    /// Column names of one index, **in index order**, from `PRAGMA index_info`.
    async fn index_columns(conn: &DatabaseConnection, index: &str) -> Vec<String> {
        name_column(
            conn,
            format!("SELECT name FROM pragma_index_info('{index}')"),
        )
        .await
    }

    /// The six non-unique indexes were pinned by nothing at all.
    ///
    /// The two unique ones are covered by the squat tests, because a wrong
    /// column list there changes what inserts are accepted. A non-unique index
    /// changes no observable behaviour, so a typo in a name or a wrong column
    /// list left all 160 tests green — and index names and column lists are
    /// runtime strings, which is this file's own thesis about why `cargo build`
    /// proves nothing.
    ///
    /// Column *order* matters and is asserted for the FIFO index specifically:
    /// it exists to serve `WHERE tenant_id/platform_id/state ORDER BY
    /// enqueued_at, id` with one scan, and a permuted list still creates
    /// happily while serving the sort not at all.
    #[tokio::test]
    async fn every_declared_index_exists_with_the_declared_columns() {
        let conn = migrated_db().await;

        assert_eq!(
            index_names(&conn, "qa_runs").await,
            vec![
                "idx_qa_runs_state_timeout",
                "idx_qa_runs_tenant_name",
                "idx_qa_runs_tenant_platform_state",
                "idx_qa_runs_tenant_schedule",
                // Not declared by this migration. `migrated_db` runs the whole
                // `Migrator`, and `m20260831_000008_run_logs` adds this one to
                // `qa_runs` because its composite foreign key
                // `(run_id, tenant_id) REFERENCES qa_runs(id, tenant_id)` needs
                // a unique key on the parent side to reference. Listed here
                // rather than filtered out, so this assertion stays an exact
                // set: a later migration adding an index to `qa_runs` should
                // have to say so in one place.
                "uq_qa_runs_id_tenant",
            ]
        );
        assert_eq!(
            index_names(&conn, "qa_run_queue").await,
            vec![
                "idx_qa_run_queue_fifo",
                "idx_qa_run_queue_state_enqueued",
                "idx_qa_run_queue_tenant_run",
            ]
        );
        assert_eq!(
            index_names(&conn, "qa_run_test_results").await,
            vec!["idx_qa_run_test_results_run"]
        );

        for (index, columns) in [
            ("idx_qa_runs_tenant_name", vec!["tenant_id", "name"]),
            ("idx_qa_runs_state_timeout", vec!["state", "timeout_at"]),
            (
                "idx_qa_runs_tenant_platform_state",
                vec!["tenant_id", "platform_id", "state"],
            ),
            (
                "idx_qa_runs_tenant_schedule",
                vec!["tenant_id", "schedule_id"],
            ),
            ("idx_qa_run_queue_tenant_run", vec!["tenant_id", "run_id"]),
            (
                "idx_qa_run_queue_fifo",
                vec!["tenant_id", "platform_id", "state", "enqueued_at", "id"],
            ),
            (
                "idx_qa_run_queue_state_enqueued",
                vec!["state", "enqueued_at"],
            ),
            ("idx_qa_run_test_results_run", vec!["tenant_id", "run_id"]),
        ] {
            assert_eq!(
                index_columns(&conn, index).await,
                columns,
                "{index} has the wrong columns or the wrong column order"
            );
        }
    }

    /// A run `ActiveModel` with **every** column set, so the generated `INSERT`
    /// names all of them and a column this entity spells differently from the
    /// DDL fails right here.
    fn run_am(id: Uuid, tenant: Uuid, name: &str) -> run::ActiveModel {
        run::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant),
            name: ActiveValue::Set(name.to_owned()),
            run_kind: ActiveValue::Set("plan".to_owned()),
            target_repo_id: ActiveValue::Set(Some(uuid(0x10))),
            target_path: ActiveValue::Set(Some("tests/smoke/plan.yaml".to_owned())),
            target_test_file: ActiveValue::Set(None),
            target_custom_plan_id: ActiveValue::Set(None),
            target_collect_url: ActiveValue::Set(None),
            environment_id: ActiveValue::Set(Some(uuid(0x11))),
            test_version: ActiveValue::Set(Some("release/5.0".to_owned())),
            app_version: ActiveValue::Set(Some("5.0.1".to_owned())),
            app_build: ActiveValue::Set(Some("20260813".to_owned())),
            state: ActiveValue::Set("dispatching".to_owned()),
            resolved_exclusive: ActiveValue::Set(true),
            exclusive_tier: ActiveValue::Set("plan.yaml".to_owned()),
            is_validation: ActiveValue::Set(true),
            parameters: ActiveValue::Set(serde_json::json!([{"name": "A", "value": "1"}])),
            include_tags: ActiveValue::Set(serde_json::json!(["smoke"])),
            exclude_tags: ActiveValue::Set(serde_json::json!([])),
            source: ActiveValue::Set("scheduled".to_owned()),
            schedule_id: ActiveValue::Set(Some(uuid(0x12))),
            bundle_ids: ActiveValue::Set(serde_json::json!([uuid(0x13)])),
            execution_ref: ActiveValue::Set(Some("exec-1".to_owned())),
            log_storage_ref: ActiveValue::Set(Some("s3://logs/1".to_owned())),
            timeout_at: ActiveValue::Set(Some(now())),
            started_at: ActiveValue::Set(Some(now())),
            finished_at: ActiveValue::Set(None),
            error: ActiveValue::Set(Some("boom".to_owned())),
            passed: ActiveValue::Set(3),
            failed: ActiveValue::Set(2),
            skipped: ActiveValue::Set(1),
            in_progress: ActiveValue::Set(4),
            total: ActiveValue::Set(10),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    fn queue_am(id: Uuid, tenant: Uuid, run_id: Uuid) -> run_queue::ActiveModel {
        run_queue::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant),
            environment_id: ActiveValue::Set(uuid(0x11)),
            run_id: ActiveValue::Set(run_id),
            run_kind: ActiveValue::Set("custom_plan".to_owned()),
            source: ActiveValue::Set("manual".to_owned()),
            exclusive: ActiveValue::Set(false),
            state: ActiveValue::Set("queued".to_owned()),
            error: ActiveValue::Set(None),
            enqueued_at: ActiveValue::Set(now()),
            dispatched_at: ActiveValue::Set(None),
            finished_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    fn result_am(id: Uuid, tenant: Uuid, run_id: Uuid) -> run_test_result::ActiveModel {
        run_test_result::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant),
            run_id: ActiveValue::Set(run_id),
            test_file: ActiveValue::Set("tests/authn/test_error_handling.py".to_owned()),
            test_name: ActiveValue::Set("AuthN Fail-Closed Error Handling".to_owned()),
            status: ActiveValue::Set("PASSED".to_owned()),
            duration: ActiveValue::Set(Some("85.06s (0:01:25)".to_owned())),
            launch_id: ActiveValue::Set(Some("7204".to_owned())),
            jira_key: ActiveValue::Set(Some("VHP-2618".to_owned())),
            // Added by `m20260818_000005_case_fidelity`, at their column
            // defaults: nothing here asserts on them and that migration's own
            // test module owns all three. Set to the defaults rather than to
            // realistic values precisely so this fixture's meaning is unchanged.
            //
            // Named rather than `..Default::default()`, which does compile —
            // a wildcard would let a later migration's columns join this
            // literal silently, and this module's whole thesis is that a
            // column nothing names is a column nothing checks.
            nodeid: ActiveValue::Set(String::new()),
            reason: ActiveValue::Set(None),
            ticket: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    /// The whole point of this module: every table name and every column name
    /// in all three entities, exercised by a real INSERT and a real SELECT.
    #[tokio::test]
    async fn every_entity_round_trips_through_the_migrated_schema() {
        let conn = migrated_db().await;
        let tenant = uuid(1);
        let run_id = uuid(2);

        run_am(run_id, tenant, "smoke-1")
            .insert(&conn)
            .await
            .unwrap();
        queue_am(uuid(3), tenant, run_id)
            .insert(&conn)
            .await
            .unwrap();
        result_am(uuid(4), tenant, run_id)
            .insert(&conn)
            .await
            .unwrap();

        let stored = run::Entity::find_by_id(run_id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the run row must read back");
        assert_eq!(stored.name, "smoke-1");
        assert_eq!(stored.state, "dispatching");
        assert_eq!(stored.app_version.as_deref(), Some("5.0.1"));
        assert_eq!(stored.app_build.as_deref(), Some("20260813"));
        assert!(stored.resolved_exclusive);
        assert!(stored.is_validation);
        assert_eq!(stored.exclusive_tier, "plan.yaml");
        assert_eq!(stored.total, 10);
        assert_eq!(stored.include_tags, serde_json::json!(["smoke"]));
        assert_eq!(stored.timeout_at, Some(now()));
        assert_eq!(stored.finished_at, None);

        let queued = run_queue::Entity::find_by_id(uuid(3))
            .one(&conn)
            .await
            .unwrap()
            .expect("the queue row must read back");
        assert_eq!(queued.state, "queued");
        assert_eq!(queued.run_id, run_id);
        assert_eq!(queued.enqueued_at, now());

        let result = run_test_result::Entity::find_by_id(uuid(4))
            .one(&conn)
            .await
            .unwrap()
            .expect("the per-test row must read back");
        assert_eq!(
            result.duration.as_deref(),
            Some("85.06s (0:01:25)"),
            "the runner's extended duration text must survive the column verbatim \
             (`manager/src/services/argo.rs:3279`)"
        );
    }

    /// The `NOT NULL DEFAULT ''` on `test_file`, which no `ActiveModel` insert
    /// can reach because `SeaORM` always names every column. Raw SQL for that
    /// reason.
    ///
    /// `INSERT ... SELECT` off the run row rather than literal values: `SeaORM`
    /// encodes a `Uuid` as a `SQLite` blob, so a hyphenated string literal
    /// would not match the parent's `id` and the insert would die on the
    /// foreign key instead of exercising the default. Copying the parent's own
    /// columns sidesteps the encoding question entirely. The per-test row
    /// borrows the run's id as its own primary key — different table, no
    /// collision.
    #[tokio::test]
    async fn an_omitted_test_file_defaults_to_the_empty_string() {
        let conn = migrated_db().await;
        let tenant = uuid(1);
        let run_id = uuid(2);
        run_am(run_id, tenant, "smoke-1")
            .insert(&conn)
            .await
            .unwrap();

        conn.execute_unprepared(
            "INSERT INTO qa_run_test_results
                 (id, tenant_id, run_id, test_name, status, created_at, updated_at)
             SELECT r.id, r.tenant_id, r.id, 'no-file', 'PASSED', r.created_at, r.updated_at
             FROM qa_runs r",
        )
        .await
        .unwrap();

        let row = run_test_result::Entity::find_by_id(run_id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the defaulted row must read back");
        assert_eq!(
            row.test_file, "",
            "a missing test file must default to the empty string, not NULL - that is \
             what lets the dedupe predicate be a plain equality instead of legacy's \
             COALESCE(test_file, '') (`manager/src/routes/runs.rs:1154`)"
        );
    }

    /// Tenant-prefixing, stated as the property it buys: one tenant cannot deny
    /// service to another by squatting its `run_id`.
    ///
    /// This is the assertion that fails if `idx_qa_run_queue_tenant_run` ever
    /// loses its leading `tenant_id`, which is the exact defect DESIGN §3.7's "Every unique index is tenant-prefixed"
    /// is about. Verified by breaking it: with the index rewritten to
    /// `(run_id)`, the squatter's insert fails with a UNIQUE violation and this
    /// test fails.
    #[tokio::test]
    async fn a_queue_row_is_unique_per_tenant_not_globally() {
        let conn = migrated_db().await;
        let victim = uuid(1);
        let squatter = uuid(7);
        let run_id = uuid(2);
        run_am(run_id, victim, "smoke-1")
            .insert(&conn)
            .await
            .unwrap();

        queue_am(uuid(3), victim, run_id)
            .insert(&conn)
            .await
            .unwrap();
        queue_am(uuid(4), squatter, run_id)
            .insert(&conn)
            .await
            .expect(
                "a second tenant's row for the same run_id must be accepted - if the \
                 unique index were tenant-blind, learning a run_id would be enough to \
                 squat it, since resource UUIDs are identifiers and not secrets",
            );

        // ...and the index still does its job inside a tenant.
        queue_am(uuid(5), victim, run_id)
            .insert(&conn)
            .await
            .expect_err("one run may hold at most one queue row within its own tenant");
    }

    /// The same property for the run-name index: two tenants may both own a run
    /// called `smoke-1`; one tenant may not.
    #[tokio::test]
    async fn a_run_name_is_unique_per_tenant_not_globally() {
        let conn = migrated_db().await;

        run_am(uuid(2), uuid(1), "smoke-1")
            .insert(&conn)
            .await
            .unwrap();
        run_am(uuid(3), uuid(7), "smoke-1")
            .insert(&conn)
            .await
            .expect("run names are namespaced by tenant");
        run_am(uuid(4), uuid(1), "smoke-1")
            .insert(&conn)
            .await
            .expect_err("run names are unique within a tenant");
    }

    /// The deliberate *absence* of a unique index on the per-test dedupe tuple.
    ///
    /// Legacy deduplicates by delete-then-insert
    /// (`manager/src/routes/runs.rs:1153-1185`), so the database must accept a
    /// repeated tuple; a unique index here would also blow `InnoDB`'s 3072-byte
    /// key limit at 6432 bytes. If someone "hardens" the schema by adding one,
    /// this test is what tells them the ingest path relies on its absence.
    #[tokio::test]
    async fn a_repeated_per_test_tuple_is_accepted() {
        let conn = migrated_db().await;
        let tenant = uuid(1);
        let run_id = uuid(2);
        run_am(run_id, tenant, "smoke-1")
            .insert(&conn)
            .await
            .unwrap();

        result_am(uuid(4), tenant, run_id)
            .insert(&conn)
            .await
            .unwrap();
        result_am(uuid(5), tenant, run_id)
            .insert(&conn)
            .await
            .expect("dedupe is delete-then-insert in the ingest path, not a constraint");
    }

    /// Both foreign keys, in both directions: an orphan is refused, and a child
    /// goes with its parent.
    #[tokio::test]
    async fn children_require_their_run_and_cascade_with_it() {
        let conn = migrated_db().await;
        let tenant = uuid(1);
        let run_id = uuid(2);

        queue_am(uuid(3), tenant, uuid(0xdead))
            .insert(&conn)
            .await
            .expect_err("a queue row must reference an existing run");
        result_am(uuid(4), tenant, uuid(0xdead))
            .insert(&conn)
            .await
            .expect_err("a per-test row must reference an existing run");

        run_am(run_id, tenant, "smoke-1")
            .insert(&conn)
            .await
            .unwrap();
        queue_am(uuid(5), tenant, run_id)
            .insert(&conn)
            .await
            .unwrap();
        result_am(uuid(6), tenant, run_id)
            .insert(&conn)
            .await
            .unwrap();

        run::Entity::delete_by_id(run_id).exec(&conn).await.unwrap();

        assert!(
            run_queue::Entity::find_by_id(uuid(5))
                .one(&conn)
                .await
                .unwrap()
                .is_none(),
            "deleting a run must cascade to its queue row"
        );
        assert!(
            run_test_result::Entity::find_by_id(uuid(6))
                .one(&conn)
                .await
                .unwrap()
                .is_none(),
            "deleting a run must cascade to its per-test rows"
        );
    }

    /// `down()` is dead weight unless it actually drops the tables, and it has
    /// to drop them children-first or the foreign keys refuse.
    #[tokio::test]
    async fn the_down_migration_drops_every_table() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        // `super::Migration` directly, **not** a loop over
        // `Migrator::migrations()`. That loop is correct only while this gear
        // has exactly one migration: `migrations()` returns declaration order,
        // and `down()` has to run in the *reverse* of it. The moment Task 17's
        // schedules migration lands, the loop would drop these tables first and
        // then run the second migration's `down()` against a schema that no
        // longer has them — failing while naming the wrong migration. This test
        // owns this migration; the sibling gear's
        // `m20260813_000004_observed_build` does the same.
        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        for table in ["qa_runs", "qa_run_queue", "qa_run_test_results"] {
            conn.execute_unprepared(&format!("SELECT 1 FROM {table}"))
                .await
                .expect_err("every table must be gone after down()");
        }
    }
}
