# Run log persistence — design

Date: 2026-08-31
Gear: `qa-runs`
Status: approved for planning
Predecessor observations: `docs/NEXT-SESSION-PROMPT.md` (the "run logs are not
persisted" open item, whose framing this document corrects)

---

## 1. Context

### The symptom

A finished run's log pane goes empty after a while. `authentication-1` on the
in-cluster deployment at `https://10.136.20.200` shows no lines at all, while the
same run's output was watched live end-to-end at the time it ran.

### What the predecessor document got wrong, measured

`docs/NEXT-SESSION-PROMPT.md` describes the gap as "logs are live-only through
the SSE broadcaster" and proposes buffering lines in the broadcaster with a cap,
persisting on `finish`, and replacing a `stream::empty()` in the SSE route. **Two
of those three are already built.** Read from the source on 2026-08-31:

* `infra/logs/broadcast.rs:115-131` — the broadcaster already retains lines per
  run, bounded by `MAX_RETAINED_LINES_PER_RUN = 5_000`,
  `MAX_RETAINED_BYTES_PER_RUN = 512 * 1024` and `MAX_RETAINED_RUNS = 32`, with
  eviction in `Inner::retain` / `Inner::evict_retained_runs`.
* `infra/logs/broadcast.rs:139-143` — `truncation_marker(dropped)` already
  exists and is already prepended by `Inner::replay` when lines were evicted.
* `api/rest/handlers/runs.rs:276-288` — a terminal run is **already** served from
  that retained tail. The `stream::empty()` the predecessor document describes
  was removed, and the comment above the branch records why, naming the run it
  was measured on (`94978978-fa28-4650-a14d-2ce8f72dff49`, 179 lines).

The same stale premise appears in the predecessor document's item 3 (code
coverage), which argues that a finished run's stream is `stream::empty()` and so
there is no text to parse. The first half is no longer true; the conclusion
survives for a different reason, which section 8 records.

### What is genuinely missing

**The archive is memory-only.** Nothing writes any durable copy, so a run's log
is lost on either of two ordinary events:

* **Eviction.** `MAX_RETAINED_RUNS = 32`. Once 32 other runs have logged, the
  oldest retained log is dropped — `Inner::evict_retained_runs` prefers an
  unwatched run and otherwise takes the oldest by write sequence.
* **Restart.** The map is a process-local `Mutex<Inner>`. The gears pod restarted
  **3 times** before reaching Ready on the first in-cluster deploy, so this is
  not a hypothetical.

Either explains `authentication-1`. Argo's
`ttlStrategy.secondsAfterCompletion: 3600` then reclaims the pod, so there is no
second source to re-read from.

`qa_runs.log_storage_ref` exists as a column
(`m20260813_000003_initial.rs:249/522/603`) and is published in `RunDto`
(`api/rest/dto.rs:342`), but `runs_sea_repo.rs:153` sets it to `None`
unconditionally and **no writer exists anywhere in the workspace** — confirmed by
grep across `.rs`, `.ts` and `.sql`.

### What legacy does, and why it is not copied

`manager/src/services/argo.rs:1610 get_workflow_logs()` fetches the whole
workflow log on completion; `argo.rs:2621` hands it to
`run_history::upsert_run_metadata(db, run, raw_logs)`; it lands in
`run_results.raw_logs TEXT` (`manager/migrations/001_initial.sql:160`) and is
served by `routes/runs.rs:175 get_persisted_raw_logs`.

Legacy's own comment records that selecting `raw_logs` for every row of the full
history once **OOMKilled the manager**. That incident is the single most
load-bearing fact in this design, and it is a fact about the *read* path, not the
write path: the failure was a list query reaching bulk text. Section 3's D-RLP-1
is shaped to make that query impossible rather than merely discouraged.

Two further reasons not to copy legacy's mechanism, as opposed to its intent:

* legacy fetches the log **once, at completion, from the execution plane**. Ours
  already has every line in-process as it is ingested, so a fetch would be a
  second retrieval of data we are holding.
* a run here has one execution node per repository group (parity spec §3.4 rule
  5), so there is no single "the workflow log" to fetch.

### Verified feasible before designing

* `ctx.subject_tenant_id()` is the established tenant accessor and is already
  used on this exact path (`domain/service/ingest.rs:945`,
  `domain/service/launch.rs:2031`), so the writer needs no read to learn the
  tenant.
* `domain/system_actor.rs` already provides the nil-tenant-enumerate /
  per-tenant-write pair this kind of background write uses
  (`for_schedule_tick` / `for_schedule_fire`, `for_ttl_sweep` /
  `for_ttl_expiry`).
* Migrations in this gear are raw SQL with one body per dialect
  (`DatabaseBackend::Postgres | MySql | Sqlite`, `m20260813_000003_initial.rs:661-668`),
  and the cascade-test pattern with an explicit `PRAGMA foreign_keys = ON` plus a
  recorded `OFF` break-test already exists
  (`m20260813_000003_initial.rs:840-862`).
* The dispatcher ticker exists at `gear.rs:591` with
  `MissedTickBehavior::Delay`, and the log publisher is the `service::watch`
  observer task that this same tick starts — so the process that ingests lines is
  the process that ticks, by construction.

---

## 2. Scope

**In scope**

* A `qa_run_logs` table in qa-runs' own database, one row per run.
* Accumulation of every ingested log line into a per-run pending buffer, and a
  flush of that buffer to the row.
* The terminal branch of `GET /qa/v1/runs/{id}/logs` reading the durable text,
  falling back to the in-memory tail.
* Correcting the three comments that assert no archive exists.

**Out of scope, with the reason**

* **Object storage.** qa-runs has no object-storage dependency and qa-platform
  does not deploy the `file-storage` gear; adding one means a gear, an SDK, and a
  credentials story. Decided in brainstorming.
* **A per-run size cap.** Decided by the user: no cap. Section 8 records the
  residual risk.
* **An age or size sweep.** Retention is the foreign key alone. Decided by the
  user.
* **Any UI change.** The endpoint's contract is unchanged, which is what keeps
  this to a single ~20-minute remote Rust rebuild.
* **A separate non-SSE endpoint** for the archived text (legacy's
  `get_persisted_raw_logs`). Nothing needs it while the SSE route serves the same
  bytes.
* **Replaying the archive to a late joiner of a *running* run.** The archive and
  the pending buffer overlap at the last flush point, so this needs
  de-duplication it has not earned yet.
* **Writing `qa_runs.log_storage_ref`.** See D-RLP-6.
* **Coverage parsing** (predecessor item 3). This design makes durable log text
  exist, which removes one of that item's three blockers; it does not address the
  other two.

---

## 3. Decisions

### D-RLP-1 — Its own table, keyed by `run_id`, and no list query may reach it

```sql
-- AMENDED 2026-08-31 during the whole-branch review: the foreign key is
-- COMPOSITE. See "D-RLP-1a" below for why.
CREATE UNIQUE INDEX IF NOT EXISTS uq_qa_runs_id_tenant ON qa_runs(id, tenant_id);

CREATE TABLE IF NOT EXISTS qa_run_logs (
    run_id     UUID PRIMARY KEY NOT NULL,
    tenant_id  UUID NOT NULL,
    text       TEXT NOT NULL DEFAULT '',
    lines      BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_qa_run_logs_run FOREIGN KEY (run_id, tenant_id)
        REFERENCES qa_runs(id, tenant_id) ON DELETE CASCADE
);
```

Not a column on `qa_runs`: that is legacy's shape and the direct cause of its
OOM. A separate table makes the fatal query structurally impossible rather than
merely discouraged, and D-RLP-7 adds a test that fails if a join is ever
introduced.

`run_id` as the primary key rather than a surrogate `id`: there is exactly one
log per run, so the upsert needs no prior lookup and a doubled flush cannot
produce two rows. This differs from `qa_run_test_results`, which has a surrogate
key because a run has many results.

`tenant_id` mirrors `qa_run_test_results` so the SecureORM scope has a column to
filter on; the cross-tenant test in D-RLP-8 is what holds it honest.

`lines` so a consumer can learn a log's size without selecting its text — the
one piece of metadata the OOM lesson says must be cheap.

Dialect note: `TIMESTAMPTZ` for Postgres, with the MySQL and SQLite bodies
following whatever `m20260813_000003_initial.rs` already uses for the
corresponding columns rather than inventing a third convention.

### D-RLP-1a — The foreign key is composite, so a log row's tenant equals its run's

**Amended 2026-08-31 by the whole-branch review, which disproved a claim this
document relied on.** The single-column FK above was specified on the assumption
that D-RLP-5's scoping made a cross-tenant write impossible. It did not, and the
reviewer demonstrated it with a probe test rather than an argument:

* the scoped `UPDATE` correctly matches zero rows for another tenant's log row,
  so control falls through to the insert;
* `secure_insert`'s `validate_insert_scope`
  (`libs/toolkit-db/src/secure/db_ops.rs:190-197`) checks the **ActiveModel's
  own** `tenant_id` against the caller's scope — and the caller supplied that
  tenant, so the check cannot fail;
* nothing tied `qa_run_logs.tenant_id` to `qa_runs.tenant_id`.

Composed, a foreign tenant could create a run's log row on its *first* append.
The rightful tenant could then never read it, because the scoped `get_log`
filters it out, and every legitimate append afterwards failed forever on the
primary key. Each of the three tasks involved was locally correct; the invariant
that mattered was enforced nowhere.

**Never reachable in production** — the only attach path reads the tenant off
the run row (`dispatch.rs:1766-1810`), and `fan_out_log` calls the tenant-scoped
`read_run` before recording, so the recorded tenant is provably the run's. It
was fixed regardless, because `qa_run_logs` had not yet been deployed and the
constraint was therefore free; against a table with data it would not have been.

The composite FK needs a unique index on the parent's `(id, tenant_id)`.
`qa_runs` **is** deployed, but `id` is already its primary key, so the pair is
unique for any existing row by implication and the index build has nothing to
fail on. Declared table-level in all three dialect bodies — a composite key
cannot be spelled inline in any of them, which is also why the guard no longer
needs the Postgres exemption D-RLP-1's original single-column form required.

Two forward hazards, recorded rather than solved: the index build is not
`CONCURRENTLY`, so it takes an ACCESS EXCLUSIVE lock on `qa_runs` for its
duration (trivial at the ~192 rows on the remote, not at scale); and any future
migration that rebuilds `qa_runs` through SQLite's 12-step ALTER must recreate
`uq_qa_runs_id_tenant`, or every `qa_run_logs` insert begins failing with a
foreign-key mismatch at DML time rather than at DDL time.

### D-RLP-2 — A new domain port, not an extension of `LogFanout`

`domain/service/mod.rs` gains a `LogArchive` port beside `LogFanout`:

```rust
#[async_trait]
pub trait LogArchive: Send + Sync {
    /// Buffer one line for `run_id`. Synchronous and infallible, like `publish`.
    fn record(&self, tenant_id: Uuid, run_id: Uuid, line: &str);

    /// Drain `run_id`'s buffer into its row.
    async fn flush(&self, run_id: Uuid) -> Result<(), DomainError>;

    /// Drain every buffer that has pending text.
    async fn flush_due(&self) -> FlushReport;
}
```

`LogFanout` is not extended: it is a fire-and-forget fan-out port whose two
methods are synchronous, and `reap` is called from four services that have no
business acquiring a database transaction. A second port keeps each one's purpose
answerable in a sentence.

`#[async_trait]`, as `RunsRepository` uses (`domain/repos/runs_repo.rs:423`);
`LogFanout` needs none because both its methods are synchronous.

The port is a **domain** port with an **infra** implementation
(`infra/logs/archive.rs`), matching how `LogFanout` / `RunLogBroadcaster` are
already split. As with `LogFanout`, no `read` method goes on this port: the read
path needs the repository, not the accumulator (D-RLP-5).

### D-RLP-3 — The buffer is drained, not a capped tail, and is not the broadcaster's

`record` appends to a `Mutex<HashMap<Uuid, Pending>>` where `Pending` holds the
tenant, the accumulated text since the last flush, and a line count. `flush`
takes that text out.

**Reusing the broadcaster's existing `retained` map was considered and rejected.**
It would avoid a second copy of each line in memory, but `retained` is a *capped
tail*: at 512 KiB it evicts from the front. A run emitting more than 512 KiB
between two flushes would therefore lose its head *before it was ever written*,
silently. A drained buffer cannot lose a line it has not yet flushed. The cost is
that the pending buffer is unbounded between flushes, which is consistent with
the user's no-cap decision and bounded in practice by the flush interval.

`record` is called from `IngestService::fan_out_log`
(`domain/service/ingest.rs:729-739`) with **the same `[{node}] `-prefixed string**
that is handed to `LogFanout::publish`. Two consequences, both wanted: replay is
byte-identical to what live subscribers saw, and the UI's marker parser — which
was taught to strip that prefix in `5538cf972` — needs no second rule for
archived lines.

A mutex acquisition per line is added to a path that already performs one scoped
repository read per line (`fan_out_log` calls `read_run`, and the doc comment
there records that bounding that cost is an open item). The lock is not the
dominant term and this design does not make that open item worse.

### D-RLP-4 — Two flush drivers, both already existing

* **`IngestService::finish`** (`domain/service/ingest.rs:1078`) flushes that one
  run after its transaction commits — the same placement as the existing `reap`
  call, and for the same reason: the run has ended, so nothing more will arrive.
* **The dispatcher tick** (`gear.rs:591`) calls `flush_due()` each pass, logging
  the report beside the existing `"qa-runs dispatcher tick"` line.

No new task and no new leader-election question. The broadcaster's module
comment already establishes that the log publisher is the `service::watch`
observer started by the dispatcher tick, so the process holding the buffers is
the process running the tick. Under a real `LeaderElector` — today
`NoopLeaderElector` is the only impl — both would move to the leader together,
which is the existing coupling and not a new one.

**The tick is what covers the terminal paths `finish` does not.** `LogFanout::reap`
is called from four services (`ingest::finish`, `runs::retire`,
`dispatch::transition`, `launch::transition`, per the port's own doc comment),
covering cancel, failed submit, TTL expiry, control-plane timeout, orphan
recovery and a refused launch's abandon. Adding a flush to each of those four
sites was considered and rejected: the port's doc comment records that this
convention has already been got wrong once ("the fifth path already existed in
`service::launch`"), and a fifth site added later would silently not flush. A run
retired by any path simply stops producing lines, and the next tick drains what
is left.

**The accepted loss:** if the process dies between the last flush and the next
tick, up to one dispatcher interval of lines is lost. This is what "periodic
flush" means and it is the whole reason a flush-only-at-`finish` design was
rejected in brainstorming — a pod that restarts mid-run archives nothing at all
under that design, and this pod restarts.

### D-RLP-5 — Reads go through a repository, and the terminal branch prefers it

A `RunLogsRepository` port (`domain/repos/run_logs_repo.rs`) with a
`RunLogsSeaRepo` implementation (`infra/storage/run_logs_sea_repo.rs`), following
the SecureORM shape every other repository in this gear uses. Writes execute
under `system_actor::for_log_archive(tenant)`; the tenant comes from the pending
buffer, which took it from `ctx.subject_tenant_id()` at `record` time, so no read
is needed to discover it.

**One system context, not the usual pair.** `for_schedule_tick` /
`for_schedule_fire` exist because a sweep must first *enumerate* rows under a
nil-tenant context and then write per tenant. `flush_due` enumerates nothing from
the database — its work list is the in-memory buffer map, and each entry already
carries its tenant. So there is no nil-tenant read to authorize and no
`for_log_archive_sweep`, and adding one would create a nil-tenant context with
nothing to justify it.

`api/rest/handlers/runs.rs:276-288`'s terminal branch reads the row and serves
its text; when there is no row it falls back to `logs.replay(id)` exactly as it
does today. **The fallback is not vestigial** — it is what serves every run that
finished before this migration, including the ~192 finished runs already on the
remote.

The response goes through the same `sse_event` / `sanitize_line` path, so the
8 KiB per-line cap and the newline neutralisation that stops a log line forging
an SSE frame (`api/rest/sse.rs:134-165`) apply to archived lines identically. The
stream stays finite, which is what lets the UI's `EventSource` stop retrying.

### D-RLP-6 — `qa_runs.log_storage_ref` stays NULL

It is published in `RunDto` (`api/rest/dto.rs:342`) and in the UI's generated
`openapi.d.ts:4514`, it is declared `VARCHAR(2048)`, and its migration fixture is
`s3://logs/1` — every signal says "a URI a consumer may fetch". Writing
`db:qa_run_logs/{run_id}` there would publish an internal table name in a public
DTO and invite exactly that misreading. Nothing reads the field today.

Instead, the three comments that assert no archive exists are corrected:
`infra/logs/broadcast.rs:88-90`, `infra/logs/broadcast.rs:275-278`, and
`api/rest/handlers/runs.rs:264`. Each currently says "nothing in this gear writes
`log_storage_ref`", which will remain literally true and will stop being the
whole truth.

If a consumer later needs to know whether a durable log exists without opening a
stream, the honest shape is a `log_lines` count on the DTO sourced from
`qa_run_logs.lines`, not a fake URI. Not built now.

### D-RLP-7 — The list-query guard is a test, not a comment

A test asserting that the runs repository's list path does not select from
`qa_run_logs` — the OOM legacy recorded, made unrepeatable.

The required assertion: build the list query the way `RunsSeaRepo`'s list path
builds it, render its SQL, and assert the string does not mention
`qa_run_logs`. The SQL-inspection tests at
`m20260813_000003_initial.rs:765-881` already parse generated statements in this
crate and are the precedent. Its break-test is to add the join and observe red.

A source-text grep over the repository file is **not** an acceptable substitute:
it would pass while a join arrived through a shared query builder.

### D-RLP-8 — Every guard is break-tested

Project standing habit, and this gear has shipped a guard that could not fail.
For each guard in section 6, the plan carries an explicit mutation and the
observation that the test goes red under it.

---

## 4. Architecture

### 4.1 New files

| Path | Contents |
|---|---|
| `infra/storage/migrations/m20260831_000008_run_logs.rs` | `qa_run_logs`, three dialect bodies, `up`/`down`, SQL-shape and cascade tests |
| `infra/storage/entity/run_log.rs` | SeaORM entity |
| `domain/repos/run_logs_repo.rs` | `RunLogsRepository` port |
| `infra/storage/run_logs_sea_repo.rs` | SecureORM implementation |
| `infra/logs/archive.rs` | `RunLogArchive`: the pending buffers and the flush |

### 4.2 Modified files

| Path | Change |
|---|---|
| `domain/service/mod.rs` | the `LogArchive` port and `FlushReport` |
| `domain/service/ingest.rs` | `IngestService` gains an `archive` dependency; `fan_out_log` records; `finish` flushes |
| `domain/system_actor.rs` | `for_log_archive(tenant)` only |
| `infra/storage/migrations/mod.rs` | register the migration |
| `infra/storage/entity/mod.rs`, `infra/logs/mod.rs`, `domain/repos/mod.rs` | module wiring |
| `gear.rs` | build the archive, hand it to `IngestService`, call `flush_due()` in the dispatcher tick |
| `api/rest/handlers/runs.rs` | terminal branch reads the repository, falls back to `replay` |
| `infra/logs/broadcast.rs` | correct the two stale comments |

### 4.3 Data flow

```
executor adapter (follows the pod log)
   → service::watch drain
      → IngestService::ingest → fan_out_log(ctx, run_id, node, line)
           ├→ LogFanout::publish(run_id, "[node] line")     ← live SSE, unchanged
           └→ LogArchive::record(tenant, run_id, "[node] line")   ← NEW, buffers

dispatcher tick (gear.rs:591) ─→ LogArchive::flush_due()
IngestService::finish          ─→ LogArchive::flush(run_id)
                                     └→ RunLogsRepository::append(scope, run_id, text, lines)
                                           └→ UPSERT qa_run_logs

GET /qa/v1/runs/{id}/logs, run terminal
   → RunLogsRepository::get(scope, run_id)
        ├─ Some(row) → stream row.text                       ← NEW
        └─ None      → stream logs.replay(run_id)            ← today's behaviour
```

### 4.4 The append

One statement per flush. Postgres:

```sql
INSERT INTO qa_run_logs (run_id, tenant_id, text, lines, updated_at)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (run_id) DO UPDATE
   SET text = qa_run_logs.text || EXCLUDED.text,
       lines = qa_run_logs.lines + EXCLUDED.lines,
       updated_at = EXCLUDED.updated_at;
```

Append rather than rewrite, so flush cost is proportional to the new text and not
to the log's accumulated size.

The semantics above are the requirement on all three dialects — MySQL via
`ON DUPLICATE KEY UPDATE` with `CONCAT`, SQLite via its own
`ON CONFLICT DO UPDATE`. Whether the implementation reaches them through SeaORM's
upsert builder or raw SQL is the plan's call, on one condition: **the concatenation
must happen in the statement, not in Rust.** Reading the row, concatenating, and
writing it back would both re-transfer the whole log on every flush and lose a
concurrent append.

`text` is never trimmed and `lines` never decreases — there is no cap, by
decision.

---

## 5. Error handling

* **A flush that fails is logged and its text is put back.** `flush` returns
  `Err` rather than dropping the buffer, so a transient database error costs a
  delayed write and not a lost log. The buffer therefore grows until the next
  successful flush; unbounded, consistent with the no-cap decision.
* **`finish` does not fail because a flush failed.** The run's terminal state,
  its lease release and its reap are what matter; the archive is best-effort
  beside them. The flush is therefore issued after the transaction commits and
  its error is logged, never propagated — the same treatment `reap` gets.
* **No log text ever reaches an error message.** A `DomainError` from this path
  carries the run id and the failure class, never a line. Log text is the
  tenant's own, but this gear's standing rule is that bulk payloads do not get
  interpolated into errors that may be recorded or published.
* **A missing row is not an error.** `RunLogsRepository::get` returning `None` is
  the ordinary case for a pre-migration run and drives the D-RLP-5 fallback.
* **Absent and foreign stay indistinguishable.** The handler's existing
  `svc.runs.get(&ctx, id)` check runs first and is unchanged, so a foreign run is
  a 404 before the archive is consulted. The archive read is additionally scoped,
  so a defect in the handler cannot turn into a cross-tenant read on its own.

---

## 6. Testing

TDD throughout. Each item below is a guard, and D-RLP-8 requires a recorded
break-test for each.

**The symptom, reproduced**

1. **A log survives broadcaster eviction.** Ingest lines for run A, then ingest
   for `MAX_RETAINED_RUNS` further runs so A is evicted from `retained`, then read
   A's log through the terminal branch and get its lines. This is
   `authentication-1`'s exact failure and no current test covers it.
2. **A log survives a lost buffer.** Flush, drop the archive and broadcaster
   entirely, rebuild them against the same database, read the log back. Stands in
   for the pod restart.

**The write path**

3. **Flush is idempotent in the sense that matters:** two `flush` calls with one
   line recorded between them yield the row once, with that line once. A drained
   buffer must not be re-appended.
4. **A canceled run's tail reaches the row via `flush_due`, not `finish`** —
   drives the D-RLP-4 claim that the tick covers `retire`.
5. **A failed flush does not lose text:** the append fails once, the next flush
   succeeds, and the row contains everything.
6. **Archived bytes equal live bytes.** Capture what a live subscriber received
   and what the archive stored for the same ingest sequence, and assert equality
   including the `[node] ` prefix.

**Schema and isolation**

7. **Cascade:** deleting a `qa_runs` row deletes its `qa_run_logs` row, on the
   Postgres tier, plus the SQLite tier with an explicit `PRAGMA foreign_keys = ON`
   and the `OFF` break-test the existing migration tests already model.
8. **Cross-tenant:** tenant A's context cannot read or append to tenant B's
   archived log.
9. **The list-query guard** of D-RLP-7.
10. **SQL shape** across all three dialects, following the existing migration's
    inspection tests.

**Read path**

11. **No row falls back to the retained tail** — the pre-migration run case.
12. **A row with text is preferred over a non-empty retained tail**, so the
    durable copy wins rather than being shadowed.
13. **Sanitisation applies to archived lines:** an archived line containing
    `\n\nevent: run_finished` cannot forge an SSE frame. The existing
    `a_line_cannot_carry_a_second_sse_frame` test is the model, extended to the
    archive path.

Tiering follows this gear's convention: in-memory doubles for service-level
behaviour, `ingest_races_pg_tests`' Postgres tier for anything where isolation or
a real dialect is load-bearing — which is items 3, 5, 7, 8 and 10.

---

## 7. Verification plan

Local, before any deploy:

1. `cargo clippy -p cf-gears-qa-runs --all-targets -- -D warnings`
2. `cargo test -p cf-gears-qa-runs` (never `--all-targets` at workspace level)
3. the Postgres tier, by whatever gate `ingest_races_pg_tests` uses
4. every break-test in section 6 performed and its red observed

On the remote (`10.136.20.200`, VPN required), after the ~20-minute rebuild:

5. `./deploy/remote/verify-k8s.sh` still exits 0 at 28 PASS / 0 FAIL / 0 NOTE,
   read from a file rather than a pipe — this project's standing rule, having
   produced false green reports before.
6. Launch a run, watch the log pane live, confirm lines appear as they do today.
7. After it finishes, reload the run page and confirm the same lines are served.
8. `SELECT run_id, lines, length(text) FROM qa_run_logs;` — a row exists for that
   run and `lines` matches what the pane showed.
9. Restart the gears deployment, reload the same finished run's page, confirm the
   lines are still served. **This is the acceptance test**: it is the one thing
   that is impossible today.
10. Confirm a run that finished *before* the migration still shows whatever the
    in-memory tail holds, or nothing, and does not error.

Not run without asking: the user's own test suite against the VHP cluster.

---

## 8. Risks and open items

* **No per-run cap, by decision.** A test looping on output can write an
  unbounded row into the operational Postgres, and with retention being the
  foreign key alone, nothing reclaims it until the run is deleted. The user chose
  this with the risk stated. The cheapest later mitigation is a cap enforced at
  append time in `RunLogsRepository`, which needs no schema change — `lines` and
  `length(text)` are already there to measure against. Recorded here so it is a
  decision on the record and not an oversight.
* **Up to one dispatcher interval of lines is lost** if the process dies between
  flushes (D-RLP-4). Reducing the window means a shorter flush period, which is
  independent of the dispatcher's interval and could be split out later.
* **The archive is written by whichever process ticks.** Under a real
  `LeaderElector`, buffers held by a replica that loses leadership mid-run would
  never flush. This is the same constraint the broadcaster's module comment
  already documents for live streaming, stated in the conditional because
  `NoopLeaderElector` is still the only impl. Not solved here; it would be solved
  for both at once.
* **Predecessor item 3 (coverage) is unblocked by one third.** Durable log text
  will exist, so `COVERAGE_SUMMARY` markers become parseable after a run ends.
  Its other two blockers stand untouched: our pytest runner emits no coverage
  marker at all, and `qa_runs_sdk::Run` carries no product key. That item's
  design still has to answer both, and its claim that a finished run's stream is
  `stream::empty()` needs correcting whenever it is next opened.
* **`log_storage_ref` remains a column nothing writes** (D-RLP-6). It is now
  dead weight in the schema and in the published DTO. Removing it is a breaking
  DTO change and is not proposed here.
