# qa-insights Gear Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `qa-insights` gear — DECOMPOSITION features **2.5** (Insights Foundation, `cpt-cf-qa-feature-insights-foundation`) and **2.8** (JIRA Loop & Notifications, `cpt-cf-qa-feature-jira-notifications`): transactional event ingestion of run and per-test/per-case results, a self-healing reconciler, per-test history, dashboard and coverage aggregates, the full eight-section analytics surface with saved views and expected-case collection, the JIRA bug registry with poller/skip-lists/auto-rerun, and the Slack + email notification surface — at behavioral parity with the source system.

**Architecture:** Standard ToolKit DDD-light gear pair under `gears/qa-platform/qa-insights/`, structurally identical to the three shipped gears (`qa-environments`, `qa-catalog`, `qa-runs`). Four pure domain cores hold every number the UI renders (analytics universe, analytics aggregates, JIRA registry, notification routing) and are written test-first before any I/O exists. Ingestion is a **transactional** event-broker consumer: the offset commit and the projection write share one database transaction (`LocalDbOffsetManager` + `TxSingleEventHandler`), so redelivery cannot double-count and a crash cannot skip. A leader-elected reconciler backfills anything the broker drops, because the broker has no durable backend yet. All external egress (JIRA, Slack) goes through oagw; email ships configured but unsent (D10).

**Tech Stack:** Rust, ToolKit stack (`toolkit`, `toolkit-db` SecureORM, `toolkit-security`, `authz_resolver_sdk::PolicyEnforcer`, `OperationBuilder`, SeaORM + `sea_orm_migration`), `event-broker-sdk` (consumer — a first for this repository), `qa-runs-sdk` + `qa-catalog-sdk` via `ClientHub`, `oagw-sdk` for JIRA and Slack, `cluster-sdk` for leader election, `tokio` + `tokio-util::CancellationToken` for lifecycle tasks.

**Specs:** `gears/qa-platform/docs/superpowers/specs/2026-08-18-qa-insights-design.md` (the approved design and the D1–D9 findings); `PRD.md` §5.4; `DESIGN.md` §3.2 (`cpt-cf-qa-component-insights`), §3.3, §3.4, §3.5, §3.7; `DECOMPOSITION.md` 2.5 + 2.8; `ADR/0002-cpt-cf-qa-adr-structured-events.md`, `ADR/0004-cpt-cf-qa-adr-four-gear-decomposition.md`.

**Legacy source of truth:** `../vhp-testrunner` (absolute, on the execution host: `/home/serhii/Jelastic/projects/fabric/vhp-testrunner`). Corrected 2026-08-20: this line said `../testrunner`, there is no such directory, and a Task 16 reviewer was unable to verify a legacy citation because of it. Every task below has a Step 0 that is unperformable if the tree cannot be found. Every rule ported below carries an explicit legacy-verification step in its task (see "Legacy-verification protocol").

---

## Implementation status (audited 2026-08-25)

> # ALL 40 TASKS ARE COMPLETE. THE CODE IS DONE; WHAT REMAINS IS YOURS TO DECIDE.
>
> **HEAD `7d04c8f2`, branch `feature/qa-platform-specs`, working tree clean, NOTHING PUSHED.**
> Phase C's ten tasks are each per-task reviewed with their fix loops closed, **the phase-level review
> has run** (the last review seat this branch gets), its fix wave is applied and re-reviewed, and the
> gate is controller-verified on the final HEAD: fmt clean, clippy zero diagnostics, **713 unit /
> 723 Postgres / 2 integration**, qa-runs **837** unchanged, and the whole workspace **11,286 passed /
> 0 failed**.
>
> **Read `### What Task 40 established — and the five items now waiting on a human` before you do
> anything else.** It carries the five release-gate decisions in the order they should be taken, and
> the two squashes.
>
> **Three things are NOT done, and all three are deliberate, not forgotten:**
> 1. **Both squashes** — Phase C's (Task 40 Step 6) and Phase B's, owed since 2026-08-24. Withheld from
>    every agent under ruling **R110**: each rewrites ~40 commits, and Phase B was left unsquashed at
>    your own explicit decision, so this is a call you have already taken once in the direction of
>    "not yet".
> 2. **The five release-gate items** — R112a+R112b (decide together; the cross-tenant write fix was
>    their precondition and is done), R107, R111, R74, and R95/R99 from Phase B.
> 3. **The `gear.rs` extraction** — deferred under **R114**, seam already named, first follow-up.
>
> **The single most important technical fact to carry forward:** the phase-level review found a
> **cross-tenant WRITE** (`resolve_bug`, an unpinned `update_many`) — the **seventh** instance of this
> crate's standing tenancy rule R86 and the first in write form, activated by exactly the deployment
> grant items R112a/R112b are about. It is fixed. **R86's text says `.one()` and needs restating as
> "any statement whose predicate does not include a tenant-unique key", reads and writes alike.**
> Every behavioural defect the phase review found was a **cross-task seam** — a rule set in one task
> and not carried into the task that consumed it — which is the one thing per-task reviews structurally
> cannot catch.

<details>
<summary><strong>Superseded status, kept for the record: the 2026-08-24 audit (Task 40 not yet started)</strong></summary>

### Implementation status (audited 2026-08-24)

**Phase A is complete, reviewed and squashed. PHASE B IS COMPLETE: Tasks 1–30 are done and verified, AND its whole-branch fix wave is applied (2026-08-24, `43261927`..`d7c37217`).** **PHASE C IS IN PROGRESS: Tasks 31–39 are done, each per-task reviewed and its fix loop closed. ONLY TASK 40 REMAINS**, plus Phase C's own squash and the whole-branch review. Phase B was **deliberately NOT squashed** — see the commit-discipline note below; the squash it mandates is owed, not forgotten, and now also owes absorbing the four fix-wave commits.

> **The Phase A squash happened on 2026-08-21 and every commit SHA in this document changed.**
> Task 19's Step 4 owed it and it had been outstanding for four tasks. Tasks 8–19 plus the
> whole-phase review's fix wave — 37 commits, 85 files, +28689/−146 — are now the single commit
> **`886e2419`** `feat(qa-platform): qa-insights — ingest, reconciler, history, dashboard`. Because
> Phase B was replayed on top of it, **the Phase B commits were rewritten too**, and the table below
> carries the new SHAs. The old ones are unreachable from this branch; they survive on the local
> branch `backup/pre-phase-a-squash-2026-08-21` and nowhere else.
>
> **The acceptance test was `git diff` between the pre-squash and post-squash heads being empty, and
> it was.** A squash rewrites history and must not change content; the gate was re-run afterwards
> (305 lib / 0 failed, `cf-gears-event-broker` still 9 / 0, fmt and clippy clean, tree clean).
>
> **One deliberate departure from the plan's commit-discipline section, which says Phase A squashes
> to one commit: it is two.** `39be61c4` `fix(event-broker): default the two config fields a linked
> gear cannot supply` is kept as the squash's parent rather than absorbed. It is a different gear,
> additive, and was gated on that gear's own suite (6 → 9 tests); burying a cross-gear fix inside a
> commit titled "qa-insights — ingest, reconciler, history, dashboard" makes it invisible to anyone
> bisecting an event-broker regression. Verified before deciding: that commit touches exactly one
> file and is the only Phase A commit touching that gear. The example-server registration
> (formerly `d76d3693`) *was* absorbed, and the squash's body says so.

> **RESUME AT TASK 40 — the last task in the plan.** Tasks 31–39 are complete, each per-task reviewed
> with its fix loop closed and its gate controller-verified. **Task 40 is the whole of the remaining
> wiring**: the ticker start (three roles: reconciler, JIRA poller, collect), the broker consumer, the
> elector, the oagw resolution, the local-client registration Task 34 left it, and **binding Task 39's
> two adapters in place of `gear.rs`'s `NeverWiredSlackClient` / `NeverWiredMailClient` stand-ins**.
> Its Step 3 is "fill every `// wired in Task N` gap", and those comments in `gear.rs` are the
> authoritative list. Then Phase C's squash (Task 40 Step 6), **the still-owed Phase B squash**, and
> the whole-branch review.
>
> **Read before Task 40's Step 0**, in this order: `### What Phase C's first four tasks established`,
> then `### What Task 35/36/37/38 established`. Between them they carry the standing tenant rule (R86,
> **six instances of that defect class so far, every one caught by review**), the six-outcome claim
> lifecycle Task 40 must not add a seventh arm to, and R75's substitution of
> `cargo test --workspace --lib --bins --tests` for the literal `cargo test --workspace` that cannot
> pass.
>
> **Superseded, kept for the record: RESUME AT TASK 39 — the egress adapters.** Task 38 shipped the notification service, both ports,
> the claim/release dedupe protocol, the audit log and all four settings routes. **Task 39 implements
> the two adapters against ports that already exist and whose shapes were fixed in advance by ruling
> R102** — `SendOutcome::UnsupportedEgress` is a variant, `MailClient::send` returns
> `Result<SendOutcome, DomainError>` and the inert impl never errors, and the Slack port deliberately
> declares no `REQUEST_TIMEOUT` const so `SlackOagwClient` can carry its own. **Then Task 40 is the
> whole of the remaining wiring** — the ticker start, the broker consumer, the elector, the oagw
> resolution, the local-client registration, and binding a real `SlackClient` in place of `gear.rs`'s
> `NeverWiredSlackClient` stand-in. **Read `### What Task 38 established` before Task 39's Step 0.**
>
> **Superseded, kept for the record: RESUME AT TASK 38 — the notification service, dedupe and log.** Tasks 36 and 37 shipped the two
> pure cores of the notification half: routing (`route`, `dedupe_key`, `Decision`) and rendering
> (Slack blocks + email, one renderer serving both the send and the preview). **Task 38 is where they
> meet a repository and an egress**, and it inherits two things its own brief does not say: the
> unported claim-release on send failure that ruling R9 assigned to "the task that ports the send
> path" — without it a transient Slack outage suppresses a notification permanently — and R94a's
> `slack_skip_is_audited`, which exists precisely so 38 does not re-derive the audited-versus-silent
> distinction from config. **Read `### What Task 36 established` and `### What Task 37 established`
> before its Step 0.**
>
> **Superseded, kept for the record: RESUME AT TASK 37 — message rendering.** Task 36 shipped the pure notification routing core:
> `domain::notify::routing::route` (config + event + per-schedule settings → a Slack/email
> `Decision`) and `dedupe_key` (the `(run_id, kind, event)` triple `NotificationClaim` claims on).
> **The remaining four tasks (37–40) are the rest of the notification half**, and Task 40
> additionally owns the ticker start, the consumer, the elector, the oagw wiring and the local-client
> registration that Tasks 34 and 35 deliberately left to it. **Read `### What Phase C's first four
> tasks established` below before Task 37's Step 0**, then `### What Task 35 established` and
> `### What Task 36 established` immediately after it, in that order — the last of the three records
> that three of the fifteen `NotificationsConfig` fields are dead in legacy (not just unmigrated),
> that `dedupe_key`'s kind parameter is `NotificationKind` and not the brief's `Channel` (R92), and
> that `QueueExpired` has no **per-event** toggle but still respects the tenant's master
> `slack_enabled` switch (R94) — legacy's own doc comment draws that two-axis distinction
> (`notifications.rs:654-656`), and a fix round corrected a first draft that had collapsed it into
> one axis. **Task 36 is complete**: reviewed, two fix rounds closed, gate controller-verified on
> `d54f209c`.
>
> **Superseded, kept for the record: RESUME AT TASK 36 — the notification routing core.** Tasks 31–35 shipped the whole JIRA half of Phase C: the pure
> registry core, the config surface and oagw client, the bug registry endpoints, the skip-list SDK provider, and now the
> poller with its new-build-gated auto-rerun. **The remaining five tasks (36–40) are the notification half**, and Task 40
> additionally owns the ticker start, the consumer, the elector, the oagw wiring and the local-client registration that
> Tasks 34 and 35 deliberately left to it. **Read `### What Phase C's first four tasks established` below before Task 36's
> Step 0** — it carries the rulings that bind the remaining tasks, three of which (R74, R77, R86) change what a later task
> is allowed to do — and then `### What Task 35 established` immediately after it.
>
> **Superseded, kept for the record: RESUME AT TASK 35 — the JIRA poller and auto-rerun.**
>
> **Superseded, kept for the record: Resume at Task 31 — the start of Phase C.** **Task 25 is complete in both halves** — 25a shipped the two ports and
> their adapters, `DashboardStats::quality_vectors_pass_rate` and the query parsers; **25b shipped
> the service assembly in legacy's pipeline order, the analytics REST tier and both endpoints**
> (`/qa/v1/analytics/overview` and `/qa/v1/analytics/build-tests`), and discharged the boot failure
> 25a left behind. Tasks 26–30 close Phase B. **Read `### What Tasks 26–30 inherit from Task 25b`
> before Step 0** — the four decisions there are inherited verbatim, and one of them (the read
> window) is an open product question a human has not yet ruled on. **Task 26 is done too**: the
> export endpoint ships, and its Step 0 recorded four legacy quirks that Tasks 28–30 should not
> re-derive — see `### What Task 26 found in legacy's export`. **Task 27 is done too**, and it moved
> a wire contract: see `### What Task 27 changed about plan identity on the wire`.
>
> **Task 24 is done, and its `**Files:**` line in this document is wrong — corrected under ruling
> R10 at its own heading.** Task 24 shipped the pure folds ONLY and created no REST tier: legacy's
> `api_build_tests` calls `normalize_overview_query`, `load_universe_and_rows` and
> `apply_universe_group_filter` before it reaches those folds, and all three are Task 25's, so a
> route registered in Task 24 could only stub or fail open. **Task 25 therefore CREATES
> `api/rest/{handlers,routes}/analytics.rs` and registers BOTH `/analytics/overview` and
> `/analytics/build-tests`.**
>
> **One wire change Task 24 made deliberately, and it is a drawn column.**
> `AnalyticsListItem::last_build` now renders `"unknown"` where it rendered `null`, and trimmed where
> it rendered padded. Legacy applies that collapse universally, at row construction
> (`analytics.rs:1032-1033`), so its `LatestInfo::build` is already normalized; this port had
> collapsed at only one of the field's two readers, which is a parity gap the doc claiming a single
> reader had hidden. `UNKNOWN_BUILD` and `collapse_build` now live at `domain::analytics::universe`,
> one rule for both consumers, and `LatestInfo::default()` still carries `build: None` so "no row at
> all" stays distinguishable from "a row that named no build".
>
> **Before Task 25, read `### Carried into Task 25`** — two of its three items are
> reassignments made because Task 23 discovered that **nothing in this gear implements
> `CatalogReader`**: the only impl is a test fake, so the production adapter had to be pulled forward
> from Task 40 to Task 25, which is blocked without it. Task 25 also gained a `PlatformReader` port
> and `quality_vectors_pass_rate`.
>
> **The doc-defect countermeasure below is amended by Task 24, which tripped it twice on its own
> patches.** "Anchor patches on the `fn`/`struct`/`const`/`#[test]` line" is **not sufficient**: an
> attributed item's `fn` line is not the top of the item, and anchoring there inserted a new function
> *below* `bucketize_status`' `#[must_use]` and orphaned a `///` block onto a new test. Anchor
> **above** the doc/attribute block, or replace the whole region. And the inverse has-doc check is
> **blind to struct fields** — which is exactly where Task 24's worst finding lived
> (`AnalyticsListItem::last_build` had no doc at all, and no mechanical check would ever have said
> so). **25a closed that blind spot with a real check** — a struct-fields-to-has-doc census diffed
> against the base commit — and it is worth knowing that the census's first version over-matched
> structs nested in `mod tests {` and reported 16 phantom fields, until the closing brace was
> anchored to the `struct` keyword's own indent. **25a also found a third shape: a re-grep filtered
> to `--include=*.rs` structurally cannot see `Cargo.toml`**, which is exactly where a stale
> ownership claim survived a whole round after the ledger paragraph beside it was fixed. Grep the
> whole tree, not the Rust in it.
>
> **One process finding from Tasks 23 and 23b that belongs in every remaining brief.** This crate's
> long-form doc convention carries a defect class with **no gate**: claims about symbols, counts and
> line numbers that no tool checks, because a deleted symbol in backticked prose cannot fail
> `cargo doc` (only an intra-doc link can, and this crate uses prose deliberately since rustdoc warns
> on private-item links). It has now cost four fix rounds across three tasks, in three different
> shapes: a `git checkout -- <one file>` used to revert a mutation **destroyed 29 doc edits** while
> `fmt`, `clippy` and both test tiers stayed green; a patch script anchored on the *first line of an
> existing doc comment* inserted new blocks **inside** other comments, leaving `seed` — a fixture
> other tests depend on — with no doc at all; and a grep for a guard's presence missed that the doc
> beside it was already correct. The three practices that catch them: **commit before you mutate**;
> **anchor patches on the `fn`/`struct`/`const`/`#[test]` line, never on a doc line**; and **re-grep
> for what you claim to have fixed, plus an inverse check** — diff the function→has-doc map against
> the base commit for items that *lost* a doc. That inverse check is ~20 lines and is the only one of
> the three that would have caught the misplacement class.
>
> **Task 21 was split into 21a and 21b by the controller on 2026-08-21**, and both halves are
> complete — see the table.
>
> **Two things Task 21b established that every later task depends on.** First, `qa_test_results`
> gained a **`run_created_at`** column (ruling A) and both the KPI window and the analytics reads
> now coalesce `COALESCE(run_finished_at, run_created_at)` — legacy's `COALESCE(rr.finished_at,
> rr.created_at)`. The column was necessary rather than tidy: ingest is delete-then-insert and the
> consumer re-projects a whole run on **every** result event, so the row's own `created_at` is reset
> to *now* each time, and windowing on it meant a run in progress for three days kept feeding
> `failed_24h_count`, `pass_rate_24h`'s denominator and the top of `failed_recent`. Ruling C then
> extended the same fix to `list_for_universe`, `latest_per_test` and `sort_key`'s ordering
> tiebreak, which **Tasks 22-25 all read**. Second, ruling R5's count is now **six**, not five — see
> that item.

> **Resuming at Task 21b.** Task 20 shipped the universe core Task 21a joined onto — read
> `domain::analytics::universe`'s header first, then three things:
> **`### Carried out of Tasks 13-15`** and **`### Carried into the next tasks`**, both in this
> section, which index what earlier tasks handed forward and which task owns each. The ones
> Phase B inherits are **5** out of Tasks 13-15 (the five status classifications, Tasks 20/21/24)
> and **1, 4, 6, 11, 13** out of "next tasks" (Task 24's `"unknown"` build fallback; Task 23's
> platform-id-to-name resolution; whether `list_for_universe` needs a `LIMIT`, which is the
> first analytics caller's to decide and is still open because Task 18 never called it; Tasks
> 27/31's truncation-by-provenance rule; and the ten unfilled `DashboardStats` fields, Tasks 21
> and 23). Also read the module headers of `domain::service::ingest` (the
> **five**-classification status table, which is the index every Phase B task argues from) and
> `domain::service::dashboard` (which numbers legacy computes, and the one requirement clause
> Phase A did not discharge).
>
> **Audited 2026-08-21, at the end of Phase A**, by the whole-phase review that precedes the
> squash. This section said "Tasks 1–15 are complete; Tasks 16–40 are not started" and
> "Resuming at Task 16" while Tasks 16–19 were shipped, gated and reviewed — four tasks past the
> point where it stopped being true, in the section this process treats as the authority that
> wins on conflict. Refreshed rather than patched, because it is the first thing Phase B reads.
>
> **Session end 2026-08-21, after Task 20.** Verified on `d8b7da96` by the controller on a quiet
> tree, not carried from a subagent report: `cargo fmt --all -- --check` clean, `cargo clippy -p
> qa-insights -p qa-insights-sdk --all-targets -- -D warnings` clean, `cargo test -p qa-insights
> -p qa-insights-sdk` **266 passed / 0 failed**, working tree clean, nothing pushed.
> Three things Task 20 established that later tasks depend on:
> **(1)** the brief's premise that legacy resolves a run's branch as `source_ref` falling back to
> `test_version` is **false** — qa-runs has no `source_ref` at all, `Run` carries only
> `test_version`, so there is no fallback to apply anywhere and ingest already stores the resolved
> value; **(2)** legacy's *stem* alias and its *test_name* alias are genuinely different keys in
> production (the display name comes from `fallback_test_name`, `analytics.rs:1805-1811`), which is
> the silent-`not_run` failure mode this phase is warned about, and it is now pinned; **(3)** the
> product/version mapping and the `since` bound are reassigned from Task 20 to **Task 25**, which
> is the first task that assembles a `UniverseFilter` from a request and issues the read — Tasks 21
> and 23 are pure folds with no repository call and could not own it.

Tasks 1–11 were re-verified against the code during the 2026-08-20 audit, not carried over from a
progress note — Phase 0 was executed in an earlier session and its completion was checked by
locating each deliverable in the tree. Task 12 was executed after that audit, over three review
passes, and its header records its own evidence. Each task header below carries its commit and the
evidence used.

| Phase | Tasks | State |
|---|---|---|
| **0** — cross-gear prerequisites | 1–7 | ✅ complete, squashed to `0e18503f` |
| **A** — foundation | 8–19 | ✅ complete, **squashed to `886e2419`** (parent `39be61c4`, the event-broker config fix, kept separate — see the status block). The per-task commits are gone from this branch; each task's own header below still records what it shipped and the evidence used, and the pre-squash SHAs survive on `backup/pre-phase-a-squash-2026-08-21`. What the individual rows recorded, preserved: **8–11** the SDK crate, gear skeleton, schema, entities and repository traits; **12** the SeaORM repositories over four review passes; **13** the transactional consumer, with four measured corrections in its header; **14** the projection with legacy's counter semantics; **15** the leader-elected reconciler; **16** the rebuild endpoint **and the REST tier** (the open decision below was settled as **option A**, and one of its commits was a measured fail-open in the write path, not a doc fix); **17** the two `OData` collections, including the named history-ordering parity gap Task 27 inherits; **18** the dashboard, ten of seventeen fields unfilled (ownership in carried item 13); **19** coverage **shape only**, answering an empty array in every deployment, with the requirement clause explicitly **not** discharged |
| **A** — foundation | whole-phase review | ✅ fixed — no Critical findings; eight Important and seven Minor, every one a stale or false doc register plus two behaviours that needed an owner named. The two code changes it made: `system_actor` gained `for_reconcile_sweep` and `for_operator_rebuild` with `SystemActorSite` threaded through `IngestService::reproject_run` (the sweep and the rebuild had both been emitting `site = "event_ingest"` since Task 15), and `ReconcileOutcome` gained `stopped_at_run` |
| **A** — foundation | squash | ✅ **done 2026-08-21** — `886e2419`, four tasks later than Task 19's Step 4 intended. Content verified unchanged (`git diff` pre- vs post-squash empty) and the gate re-run afterwards |
| **B** — analytics | 20 | ✅ complete (`c999b596` + `c0d42020`) — the universe core and `CatalogReader`. Alias machinery ported verbatim and verified character-by-character against legacy; two legacy quirks ported, pinned and raised rather than fixed. `d8b7da96` is a test-strength fix: three rules were pinned by tests that could not fail |
| **B** — analytics | 21a | ✅ complete (`f9399228` + `7a4d64d9`) — the pure summary/lists core; two legacy defects ported verbatim and pinned under controller ruling, and 15 tests carrying a 45-mutation sweep. `d22c9416` is a fix round: a doc contract that contradicted a passing test in the same file, plus three unfalsifiable rules |
| **B** — analytics | 21b | ✅ complete (`b6a85c12` + `49e885cd` + `9db36d59` + `b59c1873` + `7297ca52` + `6c1e8a87` + `cab77978`) — the five `DashboardStats` KPI fields and open item 7's `dashboard.rs` split. Four fix rounds: one Critical (the two pass rates could be transposed on the wire with nothing failing), then rulings A and C adding `run_created_at` and extending it to the analytics reads, then four defects the fixes themselves introduced. **Three of the four rounds were spent on prose**, not behaviour — every one a claim about a symbol or a count that no tool checks |
| **B** — analytics | 22 | ✅ complete (`99c3712a` + `0b7548d5`) — the heatmap and trend folds, the `Clock` port and its `SystemClock` adapter. Legacy clamps **twice** (`:2426-2427` over the defaults 7 and 90, then again at `:1452`/`:1496`); this task ports the second and Task 25 owns the first, and the composition is bit-identical to legacy because the bounds match and `clamp` is idempotent. The folds take `today: Date` rather than the brief's `&dyn Clock`, under controller ruling — it matches `daily_points`' precedent, keeps the cores pure, and removes a legacy incoherence (legacy reads `Utc::now()` separately in `build_heatmap`, `build_trend` and `build_flaky`, so a request crossing midnight renders a heatmap one day behind the trend). `0b7548d5` is a fix round: the clamp was pinned at the *functions* while every fold call passed an in-range `days`, so `build_trend` calling `heatmap_days` would have rendered a 30-point trend where legacy renders 90, with a green suite |
| **B** — analytics | 23 | ✅ complete (`10e333b7` + `5ef488f6` + `d689f603` + `dc6bf6f7`) — the four pure folds: flaky, the quality-vector summary, grouped summaries and the universe group filter. Legacy's **two** flaky folds are kept distinct, as they must be: the analytics one uses `build_stats_map`'s three-way split keyed on `test_file`, the dashboard one uses ruling R5's sixth classification grouped by `test_name, plan_id`. Its one fix round was **entirely doc and citation** — including a header that would have told Task 25 to narrow two folds legacy does not narrow — plus one fixture value; the reviewer called the test suite the strongest in the phase and found no Critical findings. **Two `DashboardStats` fields were declined rather than half-shipped**, and both reassignments are recorded: `flaky_tests` → 23b, `quality_vectors_pass_rate` → 25 |
| **B** — analytics | 23b | ✅ complete (`13c0a90d` + `feacbf6d` + `52a7c140`) — `DashboardStats::flaky_tests`, with legacy's reduction **in SQL** (`HAVING`, `ORDER BY LEAST(passed, failed) DESC, total DESC`, `LIMIT`) via `project_all`. **Three things the brief got wrong and legacy won on all three**: legacy *does* pick a representative file, `MAX(tr.test_file)` (`dashboard.rs:384`); the `ORDER BY` has a second key and both are `DESC` (`:395-398`), where the brief gave neither; and the field lands at `:556`. Also corrected `domain::service::ingest`'s R5 "Ported by" cell. 16 new tests over both tiers, one fix round, entirely documentation |
| **B** — analytics | 24 | ✅ complete (`1d9be272` + `469d3b9e` + `50bbd091` + `f2440db1`) — the build distribution, `latest_per_test_snapshot`, `compare_build_desc`, `build_status_rank` and the build-tests fold. **Pure folds only, no REST tier** (ruling R10). Its review found no behavioural defect and called the ports verbatim; its one fix round was the no-gate doc class again — but one of those claims ("the single place that reads `ExecRow::build`") was **hiding a live parity gap**, which the round closed in code (ruling R15). Also: a **seventh** classification row added to `domain::service::ingest`'s R5 table — `latest_per_test_snapshot`'s mapping (`:1633-1639`), which passes an unrecognized status through verbatim |
| **B** — analytics | 25a | ✅ complete (`50d98d55` + `7dfe8916` + `6dfa064f` + `231383e1` + `a5161ff3` + `e2d0cba1`) — the production `CatalogReader` adapter, a `PlatformReader` port + adapter over qa-environments, `DashboardStats::quality_vectors_pass_rate` and the six query-parser rejections with their status codes. **+42 tests.** Its review found no parity break and no dropped 400; one fix round, twelve findings, of which exactly one had behaviour behind it — a doc claiming an absent `HAVING` clause was unobservable, when it deletes a whole rendered row. Two legacy findings raised and ported verbatim rather than fixed: the **case-folding asymmetry** between the dashboard and analytics quality-vector folds, and one deliberate documented divergence (a qa-catalog failure now fails the whole dashboard where legacy warns and continues) |
| **B** — analytics | 25b | ✅ complete (`827a3307` + `73f7b9b4` + `0d8ac1e8`) — `domain/service/analytics.rs`, the analytics REST tier, and **both** endpoints registered under ruling R10. **+41 tests.** The review verified the highest-risk requirement at legacy source and it is right: the group chart and the quality-vector summary are computed over the **plan-narrowed but NOT group-narrowed** universe, above `apply_universe_group_filter`. Also discharged in one commit: the `qa_environments` `deps` token, the gear-crate dependency and the `ClientHub` lookup 25a left as a **boot failure no test could catch**. One fix round, six findings, **entirely documentation** — nine legacy citations that did not land, two of which overlapped each other, in the very commit whose purpose was fixing citations. Four decisions a human still owns are recorded under `### What Tasks 26–30 inherit from Task 25b` |
| **B** — analytics | 26 | ✅ complete (`6db82c6a` + `357d698f`) — `GET /qa/v1/analytics/export`, `domain/analytics/export.rs`, legacy's `csv_escape` with **both blind spots preserved and pinned** (a bare CR is not a quoting trigger; a leading `=`/`+`/`-`/`@` is not escaped — ruling R31, a live product/security question for the human). **+14 tests.** Its review checked **all 22 legacy citations against source and every one landed** — the first task in this plan to clear that bar — and confirmed the CSV output byte-exact against `overview_to_csv`. One fix round, whose finding with teeth was a **missing guard**: the route-table test that names every path was not updated, and stayed green because it asserted presence and not count. It now asserts both |
| **B** — analytics | 27 | ✅ complete (`cc1a67be` + `47175719`) — the three plan drill-downs (`tests`, `builds`, `test-history`) plus `ResultsRepository::list_for_plan`. **+13 tests.** Its review called Step 0 "the best legacy verification I have checked on this plan" and confirmed all four disclosed decisions rest on true premises. One fix round, whose headline was a **real functional bug no test could catch**: `plan_id` had been redefined as `plan_path` (which contains slashes) while left in a single-segment path parameter, so all three endpoints 404'd for every real plan. Legacy avoided it only because `compose_repo_plan_id` sanitizes its slug. **`plan_id` is now a query parameter on all three** (ruling R41). Second finding worth knowing: the new read's window predicate defeated the only index that could serve it, while citing the scale NFR as its justification |
| **B** — analytics | 28 | ✅ complete (`b601893a` + `ee92740a`) — saved views: the service and all four endpoints, **the first write path in Phase B and the first place two callers can collide**. **+22 tests at each tier.** Reviewed as concurrent-write code: **no authorization or ownership hole**, and the `ensure_owner` floor verified end to end — an *unconstrained* scope becomes a single `owner_id = subject` constraint, which is what closes `validate_insert_scope`'s fail-open path. Legacy's owner came from an **unauthenticated request header**; this gear's comes from `SecurityContext`, a strict security improvement. Step 0 caught a trap a literal port would have created (ruling R47) and the review found a test that passed with the behaviour it tested deleted. **First task on this plan whose citation audit matched an independent reviewer's own extraction** |
| **B** — analytics | 29 | ✅ complete (`9815d36b` + `9a619c99`) — `OverviewSummary::case_expected` is **a real number instead of `0`**, via a pure per-file precedence fold (`domain::analytics::universe::expected_cases`): the collect job's exact count wins per `(repo_id, test_file)`, `UniverseTest::static_case_count` is the fallback. **+6 tests.** Every computational claim verified at legacy source; the TDD red was genuine (`E0432: no expected_cases` — the function did not exist). Widest type-signature change in Phase B: `AnalyticsService<R>` → `<R, C>` through `AppServices` to `gear.rs`, wiring the previously-built-but-unwired `OrmCollectRepository`. One fix round, **entirely disclosure**: an undisclosed fail-open/fail-closed divergence (R54) and an unstated authorization decision with a silent-fallback hazard (R55) |
| **B** — analytics | 30 | ✅ complete (`a19ee627` + `9fd84995` + `f8a2649f` + `c8048ef2`) — exact collect: the `RunsLauncher` port, the collect service, the runner callback and the trigger. **+18 tests.** **The only task in this plan to produce a CRITICAL finding**, and it was a real one: the callback took `tenant_id` from an unauthenticated caller and bound it as the write identity, with the repository's own tenant check tautological on that path — **a live, anonymously reachable cross-tenant write**. Closed with an HMAC over `(repo_id, branch, tenant_id)`, verified constant-time, fail-closed on an unconfigured or too-short secret, checked before anything is scoped or written. Two fix rounds. The second corrected an **inverted reachability conclusion** that both the implementer and the controller had accepted from true premises |
| **C** — JIRA + notifications | 31 | ✅ complete (`311cecfc`) — the pure registry core: `skip_list_entries` on the `status = 'Open'` literal predicate, and `render_skip_list`. Reviewed clean, zero Critical, zero Important; the reviewer re-opened all five legacy citations itself. **Its Step 0 corrected the plan on where the skip list is built**: `argo.rs:477-479` is only the env-var push, double-guarded on non-empty, and the `format!`/`join(",")` construction is the caller at `routes/runs.rs:753-761` — no sort, no dedup, order is the query's, and that query has no `ORDER BY` |
| **C** — JIRA + notifications | 32 | ✅ complete (`a05dd17e` + `cff2cb12`) — the `JiraClient` port, its oagw adapter, BOTH config singletons on `JiraRepository` (ruling R71) and `GET/PUT /qa/v1/settings/jira` (ruling R69). **+45 tests**, 570 unit / 579 Postgres. **Ruling R76 was partly impossible and the task proved it rather than working around it**: oagw strips `authorization` from every proxied request unconditionally and the strip runs after passthrough, so a per-request credential is unreachable — the upstream carries oagw's `apikey` plugin pointing at the tenant's credstore reference, and the token material never enters this gear at all. One fix round, four Important findings, all addressed and re-reviewed clean: an unvalidated `SecretRef` syntax **violated by every example in the diff** (the re-grep found a fifth site the review had not named), a swallowed route-registration failure that left a tenant's JIRA egress permanently dead across a restart, a silently dropped JIRA context path recorded as a non-divergence by a comment that misstated legacy (ruling R82 — supported, not refused), and a `.one()` without a tenant predicate that could **carry a credential reference across tenants** under a parent-tenant grant (ruling R83 — scoped here; the same shape survives in `notify_sea_repo` for Task 38) |
| **C** — JIRA + notifications | 33 | ✅ complete (`d4c6b0f3` + `fd20a45a` + `077f7003`) — `GET /qa/v1/jira/open-bugs`, `POST /qa/v1/jira/bugs`, and the first production caller of `JiraClient::create_or_find_issue`. **+28 tests.** Three delegated decisions, all upheld by the review at legacy source: **R80** ported legacy's THIRD status vocabulary (`status != 'Closed'`) for the re-file probe rather than reusing `list_open`'s `status = 'Open'` — a bug this gear marked `'Resolved'` may not have reached JIRA's own `Closed` state, so a re-failure before closure answers `created:false` with the same key instead of filing fresh; **R84** the issue body is the concatenated `reason` of the failed cases, and proving it legitimate turned up that this gear's own `TestCaseResultRecord::reason` doc **undersells the column** (legacy copies `raw.reason` with no outcome filter, `argo.rs:2926,2984`); **R85** the GET takes the `(repo_id, plan_path)` pair, not the analytics `plan_id` token, so the registry and the skip list cannot disagree about which bugs exist. **200, not Task 28's 201+Location** — a 0..N list has no single resource for a `Location` to name. Two fix rounds: **one Critical — an untenanted `.one()` dedupe probe, the THIRD instance of that defect in this crate** (it leaked a foreign tenant's key AND suppressed the caller's own filing AND misfiled the insert), which produced standing ruling **R86**; then R89 for `find_by_key`, the same shape one call deeper, which this task's `file_bugs` had just turned into production code |
| **C** — JIRA + notifications | 34 | ✅ complete (`72f8a67b` + `1b708c72`) — `domain/local_client/{mod.rs,client.rs}` (ruling R70) implementing `QaInsightsClientV1::skip_list_for`, the **one method another gear calls**. **+4 tests.** It goes through `JiraService::open_bugs` rather than the repository, which the review confirmed is the right call: a direct repository call would silently skip the `bug_scope` PDP check an HTTP caller must clear. `QaInsightsLocalClient` holds `Arc<JiraService>` rather than the whole `AppServices` bundle qa-runs' sibling holds — blessed, because that client implements ~20 methods across three service families and this one implements one; **Task 40's wiring cost is one line either way** (`Arc::clone(&services.jira)`). One fix round, one Important, and it is this plan's twelfth instance of the no-gate documentation class: the module doc claimed the `Validation` arm was **unreachable** through this path because both parameters are mandatory, and told a later reader not to look for a test — but `optional_plan_ref` treats an empty `plan_path` as absent, so `skip_list_for(ctx, repo_id, "")` reaches it. Now corrected and pinned |
| **C** — JIRA + notifications | 35 | ✅ complete (`a970475c` + `4f681f6e`) — `JiraPollerService::poll_once`, the two port extensions ruling **R73** named in advance (`RunsLauncher::launch_test` and `PlatformReader::default_branch`, each with its adapter), and `GET/PUT /qa/v1/settings/jira-poller` (ruling **R91**). **+16 tests**, 618 unit / 627 Postgres. Ruling **R90** kept `gear.rs` untouched — the poller is built here and Task 40 starts its ticker, the same shape R70 set for Task 34. **R73's delegated sixth decision was resolved at Step 0**: legacy's `find_plan_test_file` greps a local git checkout, which this gear does not have (ADR-0005 confines git egress to qa-catalog), so it becomes `CatalogReader::list_universe` filtered by `(repo_id, test_name)` — justified by qa-catalog computing `test_name` from the same `TEST_TITLE`/`TEST_META` precedence legacy's `declares_test_title` uses, and legacy's plan re-resolution step drops out entirely because `JiraBug` already carries `(repo_id, plan_path)`. One fix round, two Important, zero Critical: a `find_plan_test_file` citation pointing at legacy's **call site** rather than its body — whose mandated sweep then found **four more** wrong citations across the diff — and, the one with teeth, `latest_version_for_plan` calling `list_for_plan(.., UNIX_EPOCH)`, which **disabled the very time-window guard that method's own doc calls the only thing keeping a per-plan read off an unbounded scan** on a 5M-row table, once per open bug every 300s. Now a dedicated repository method pushing `product_version IS NOT NULL`, the `ORDER BY` and `.one()` into SQL, R86-compliant, with five tests whose two-tenant case defeats the crate's SQLite index-order trap **by construction** — a full day of `run_finished_at` separation rather than a tenant-id ordinal |
| **C** — JIRA + notifications | 36 | ✅ complete (`e80c68ea` + `6bc1f465` + `d54f209c`) — `domain/notify/{mod,routing,routing_tests}.rs`: the pure routing core (`route()` and `dedupe_key()`), config + event + per-schedule settings in, a `Decision` out. **+8 tests**, 626 unit / 635 Postgres. **Ruling R92 was settled by counting, not arguing**: exactly ONE constant is ever written to `notification_kind` in legacy — `SCHEDULED_RUN_SLACK_NOTIFICATION_KIND = "scheduled_run_slack"` — and it already fuses family and channel, so the parameter is `NotificationKind`; the brief's `Channel` would have let a ScheduledRun-Slack and a RunCompleted-Slack alert for the same run+event **collide on one claim slot**. **R95**: three of the fifteen config fields are dead in legacy (zero reads outside the struct, its `Default` and one JSON snapshot) and stay dead — inventing gating legacy lacks would be a behaviour change dressed as a bug fix; **raised to the human as a PRD question, not fixed**. Two fix rounds, both on the same defect class this plan keeps hitting — a doc claim that does not survive checking — and the second time it was **driving a value**: `routing.rs`'s "`RunCompleted` and `ScheduledRun` have no silent case at all" is true of `notify_scheduled_run_status` and **false** of `notify_run_completed`, whose Slack chain (`notifications.rs:263-322`) is `if/else if/else if` with no final `else`, so `slack_enabled == false` past the `:208-219` early return logs nothing. `slack_skip_is_audited` was hardcoded `true` for that reachable state, which would have told Task 38 to write an audit row legacy never writes |
| **C** — JIRA + notifications | 37 | ✅ complete (`f9fe68f4` + `9828c8ad`) — `domain/notify/{render,render_tests}.rs`: the pure rendering core, Slack blocks and email. **+16 tests**, 642 unit / 651 Postgres. **Ruling R98 pulled email rendering into this task** — the brief's Step 0 and both its tests are Slack-only, but its own mandated commit message says "Slack block and email rendering" and Task 36 had shipped a `Decision` answering `sends_email` that would otherwise have been stranded until Task 38's send path, where it would have been written without a rendering test. **Ruling R97 was answered honestly and is the more interesting one**: the brief's `the_preview_renders_exactly_what_the_send_renders` is a real property in legacy — `:388` and `:431` are two functions that can drift — but **tautological in the port**, where one renderer serves both. The implementer built the single renderer, said plainly that the assertion cannot fail, and replaced it with a six-token property test rather than shipping the tautology as assurance. **The ~950-line template engine was challenged in review and survived**: legacy genuinely has a hand-rolled recursive `{{#if}}`/`{{#if_event}}` engine (`notifications.rs:1099-1219`), traced token-for-token — faithful porting of real complexity, not invented complexity. All five default section templates byte-verified down to the em-dash and middle-dot characters, and all eighteen status label/headline/icon values. One fix round, one Important: the email subject had been unilaterally rebranded `"VHP test run"` → `"QA run"`, reverted under **R99** for in-crate consistency with the `[VHP] Test Failed` JIRA summary that Tasks 32-33 shipped verbatim — **the rebrand itself is raised to the human as a product question, not settled here** |
| **C** — JIRA + notifications | 38 | ✅ complete (`10fd9de8` + `8d894854` + `d6c488c2` + `7ed6b7a8`) — the notification service, both egress ports, `SendOutcome`, the claim/release dedupe protocol, the audit log and all four `/qa/v1/settings/notifications*` routes. **+38 tests**, 680 unit / 689 Postgres. **The plan's largest diff** (3233 insertions, 20 files) and the only one reviewed on the most capable model. Three fix rounds, two of them *before* review because the implementer raised its own concerns rather than burying them. **R103 amended R90**: the four routes need a live service, and the choice was between touching `gear.rs` or letting the domain name a concrete infra type — the first pass took the second horn and flagged it; ruled the wrong horn, since R90's purpose was that Task 40 owns the *runtime lifecycle*, not that `gear.rs` is untouchable, and a domain layer naming `infra::storage` is a permanent break where a sixth type argument is mechanical. **R104**: the first pass rendered `run_name` as the run's UUID with every other field `Default`, calling it a documented simplification — ruled a **false green** (routes live, claim taken, log says `sent`, human receives a UUID) and removed, since `RunsReader::get_run` already existed. **R105**: the per-schedule read grew here, in the task that consumes it, on R73's precedent rather than being deferred into the wiring task. Review then found three defects in one sequence: legacy's non-empty webhook/SMTP **capability gates** ported into `send_test` but not the automatic path (a half-configured tenant burning a claim per run where legacy was silent); `UnsupportedEgress` **keeping** its claim — R100's permanent-suppression failure arrived at from the opposite direction; and a release error's `?` **swallowing the audit row for a failed send**, the one thing the log exists to record. Its citation sweep was **the first in this run to come back clean** — ~25 citations byte-verified, zero drift |
| **C** — JIRA + notifications | 39 | ✅ complete (`5d1a337d` + `4f201eac`) — `infra/notify/{mod,slack_oagw,mail_unsupported}.rs`: the Slack egress over oagw and the deliberately inert mail adapter. **+13 tests**, 693 unit / 702 Postgres. Reviewed **Approved with zero Critical and zero Important**. Implemented against ports R102 had fixed in advance, so the contract needed no negotiation. **It surfaced the plan's most consequential architectural finding, R107** — see the release-gate note below. **R108** closed a dropped parameter before it could become a security question: `SlackClient::send` had no `SecurityContext`, so the adapter proxied as `SecurityContext::anonymous()` while both call sites already held a context they never forwarded; fixed here rather than in Task 40, because once the adapter is bound an anonymous proxy stops being a signature change. The brief's `the_slack_client_bounds_every_request` is **tautological by construction** — it asserts a constant equals 10s — and the implementer said so in the test's own doc comment and added `the_bound_is_actually_applied_to_a_hanging_gateway`, whose fake gateway returns `std::future::pending()` so removing the `tokio::time::timeout` wrapper makes it hang rather than pass. `tokio` was promoted from a dev- to a real dependency for that wrapper, one task earlier than this crate's `Cargo.toml` ledger forecast |
| **C** — JIRA + notifications | 40 | ✅ complete (`05de0c32` + `1bb3322a`) — the composition root closed: the `EventBrokerApi` lookup, the transactional consumer started under the cancellation token and stopped on every exit path, **three** separately leader-elected tickers with independent cadences, the skip-list local-client registration, the two real egress adapters bound in place of the inert stand-ins, the `stateful` capability and a `lifecycle(entry = "serve")` clause, plus `tests/ingest_idempotence.rs` — this crate's **first integration target**, booting the real gear through toolkit's `GearContextBuilder`. **+20 tests**, 705 unit / 715 Postgres / 2 integration. **The plan's own tenant-enumeration option was refused as provably circular**: the reconciler is `qa_ingest_watermarks`' only writer, so scanning it to find tenants would have found none, ever — established by grepping the writers, not by argument, and replaced with `SELECT DISTINCT tenant_id FROM qa_test_results` behind a new `TenantDirectory`. **The best design in the phase came out of R86's highest-risk instance**: the tickers run with no HTTP caller, so `system_actor::TenantBound` was made a newtype in its own module whose field-privacy actually binds, with six tenant-bound factories taking `TenantBound` and the one enumeration factory unable to produce one — turning "cross-tenant authority must not leak into per-tenant work" from a convention into a **compile-time fact**. Phase A's owed `stopped_at_gap` alert obligation was discharged with the open sub-question decided (WARN naming tenant *and* run, ERROR at three consecutive passes) — and the review then found the escalation could be silently wiped by any transient enumeration failure, now fixed by distinguishing "not listed" from "could not be read". One fix round, zero Critical, three Important: a missing `cargo-shear` ignore for the `cluster-sdk` this task added, that wedge-history reset, and **four authored doc counts that contradicted the code beside them** — whose mandated sweep then found a fifth the review had not named |

Not a plan task, but shipped: the example-server registration that makes a boot test possible at
all (`apps/cf-gears-example-server`). It was its own commit `d76d3693` until the Phase A squash
absorbed it; the squash's body records it.

**Gate after Task 39 (2026-08-25, controller-verified on `4f201eac` on a quiet tree, not carried from
a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights
--lib` **693 passed / 0 failed** (1.44s), `cargo test -p qa-insights --features integration --lib`
**702 passed / 0 failed** (8.56s), `cargo test -p qa-insights-sdk` green, working tree clean, nothing
pushed. Phase C's unit-tier progression: 518 → **525** (31) → **570** (32) → **598** (33) → **602**
(34) → **618** (35) → **626** (36) → **642** (37) → **680** (38) → **693** (39).

**Gate after Task 38 (2026-08-25, controller-verified on `7ed6b7a8` on a quiet tree, not carried from
a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights
--lib` **680 passed / 0 failed** (1.44s), `cargo test -p qa-insights --features integration --lib`
**689 passed / 0 failed** (8.68s), `cargo test -p qa-insights-sdk` green, working tree clean, nothing
pushed. Phase C's unit-tier progression: 518 → **525** (31) → **570** (32) → **598** (33) → **602**
(34) → **618** (35) → **626** (36) → **642** (37) → **680** (38).

**Gate after Task 37 (2026-08-25, controller-verified on `9828c8ad` on a quiet tree, not carried from
a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights
--lib` **642 passed / 0 failed** (1.40s), `cargo test -p qa-insights --features integration --lib`
**651 passed / 0 failed** (8.60s), working tree clean, nothing pushed. Phase C's unit-tier
progression: 518 → **525** (31) → **570** (32) → **598** (33) → **602** (34) → **618** (35) →
**626** (36) → **642** (37).

**Gate after Task 36 (2026-08-25, controller-verified on `d54f209c` on a quiet tree, not carried from
a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights
--lib` **626 passed / 0 failed** (1.37s), `cargo test -p qa-insights --features integration --lib`
**635 passed / 0 failed** (10.95s), working tree clean, nothing pushed. Phase C's unit-tier
progression: 518 → **525** (31) → **570** (32) → **598** (33) → **602** (34) → **618** (35) →
**626** (36).

**Gate after Task 35 (2026-08-25, controller-verified on `4f681f6e` on a quiet tree, not carried from
a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights
--lib` **618 passed / 0 failed** (1.38s), `cargo test -p qa-insights --features integration --lib`
**627 passed / 0 failed** (8.38s), `cargo test -p qa-insights-sdk` green, working tree clean apart
from this document, nothing pushed. Phase C's unit-tier progression: 518 → **525** (31) → **570**
(32) → **598** (33) → **602** (34) → **618** (35).

**Gate after Task 34 (2026-08-24, controller-verified on `1b708c72` on a quiet tree, not carried from
a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights -p
qa-insights-sdk` **602 passed / 0 failed** (1.38s), `cargo test -p qa-insights --features integration
--lib` **611 passed / 0 failed** (8.68s), working tree clean apart from this document, nothing pushed.
The Phase C progression so far: 518 → **525** (Task 31) → **570** (Task 32, +45 over two rounds) →
**598** (Task 33, +28 over three) → **602** (Task 34) on the unit tier.

**Gate after Task 30 — THE PHASE B GATE (2026-08-24, controller-verified on `c8048ef2` on a quiet
tree, not carried from a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p
qa-insights -p qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p
qa-insights -p qa-insights-sdk` **516 passed / 0 failed**, `cargo test -p qa-insights --features
integration --lib` **525 passed / 0 failed** (16.02s), **`cargo build --workspace` clean**, working
tree clean, nothing pushed.

**Phase B whole-branch review's single fix wave (2026-08-24, `43261927`..`d7c37217`, four commits on
top of `c8048ef2`; no squash of prior Phase B commits) — full report at
`.superpowers/sdd/2026-08-18-qa-insights-gear/phase-b-fix-wave-report.md`.** One behavioural finding
(Finding 1: the boot warning and the request-time signing-secret floor tested different predicates,
so a 1–15 character secret passed boot silently and then 403'd every collect report — both now share
`collect::signing_secret_is_configured`) plus eight documentation/test findings, all verified true
before fixing (none were false premises): a double-counted citation (Finding 8), a four-tasks-stale
and mislabeled REST-tier header regenerated from the route table (Finding 3), an unqualified
"bounded read" claim corrected to name the time window rather than a page size, with no pagination
added (Finding 2), a maintainer trap naming Task 28 as an obligated future caller of a guard it
correctly does not call (Finding 4), an absolute "no serde in the domain" claim qualified to
"no wire-contract serde" at four sites (Finding 7), unbounded caller text and a false audit line on
the one anonymous route, fixed by minting the system actor after signature verification rather than
before (Finding 5), a regression test that pinned `collect_url` against a duplicate local struct
rather than the real DTO — proven with an actual red/green cycle, not asserted (Finding 6), and the
one required string query parameter (`plan_id`) that was not trimmed and 400'd like every sibling
(Finding 9). Gate after the fix wave, controller-verified on `59d3df39`: **518 passed / 0 failed** (lib+sdk),
**527 passed / 0 failed** (Postgres tier), fmt and clippy clean, `cargo build --workspace` clean.
Both tiers rose by **two**, from 516 and 525: `collect_report_query_decodes_collect_urls_real_encoder_output`
(Finding 6) and `a_blank_plan_id_is_refused_on_all_three_drilldowns` (Finding 9). *(An earlier draft
of this paragraph said "one new test" on the Postgres tier; the wave's own scoped re-review caught
the arithmetic — 525 + 2 = 527 — which is worth recording as the eleventh instance of this plan's
count-in-prose defect class, found in the paragraph whose job was to record the wave accurately.)*

**The wave's scoped re-review (2026-08-24, `ab53b5db`, over `c8048ef2..59d3df39`) verified all ten
findings ADDRESSED with no new Critical or Important breakage.** It checked by extraction rather
than acceptance where a count was involved — it re-ran the mini-chat `ensure_owner` grep itself, and
cross-checked the regenerated fifteen-paths-in-six-modules register against `PATHS` and against
`grep -n '"/qa/v1' routes/*.rs`. On the wave's highest-risk edit — restructuring `record_count`'s
signature so the system actor is minted *after* verification — it confirmed the order is
`verify_signature` → blank checks → actor → `AccessScope::for_tenant` → write, that the same
`tenant_id` covers both the signature and the write scope, and that all 13 converted call sites kept
identical assertions. On Finding 6 it reasoned through both mutation directions independently and
confirmed the new test is the only one that catches either.

**Gate after Task 29 (2026-08-24, controller-verified on `9a619c99` on a quiet tree, not carried
from a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights -p
qa-insights-sdk` **498 passed / 0 failed**, `cargo test -p qa-insights --features integration --lib`
**507 passed / 0 failed** (8.08s), working tree clean, nothing pushed.

**Gate after Task 28 (2026-08-24, controller-verified on `ee92740a` on a quiet tree, not carried
from a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights -p
qa-insights-sdk` **492 passed / 0 failed**, `cargo test -p qa-insights --features integration --lib`
**501 passed / 0 failed** (8.14s), working tree clean, nothing pushed.

**Gate after Task 27 (2026-08-24, controller-verified on `47175719` on a quiet tree, not carried
from a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights -p
qa-insights-sdk` **470 passed / 0 failed**, `cargo test -p qa-insights --features integration --lib`
**479 passed / 0 failed** (8.33s), working tree clean, nothing pushed.

**Gate after Task 26 (2026-08-24, controller-verified on `357d698f` on a quiet tree, not carried
from a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights -p
qa-insights-sdk` **457 passed / 0 failed**, `cargo test -p qa-insights --features integration --lib`
**465 passed / 0 failed** (7.62s), working tree clean, nothing pushed.

**Gate after Task 25b (2026-08-24, controller-verified on `0d8ac1e8` on a quiet tree, not carried
from a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights -p
qa-insights-sdk` **443 passed / 0 failed**, `cargo test -p qa-insights --features integration --lib`
**451 passed / 0 failed** (7.45s), working tree clean, nothing pushed.

**Note for every later controller gate on this machine:** bare `cargo` resolves to `/usr/bin/cargo`
**1.75.0**, which cannot parse this workspace (`resolver setting 3 is not valid`); the pinned
**1.97.0** that `rust-toolchain.toml` names lives at `~/.cargo/bin` and is **not on `PATH`**, and
`rustup` is not installed to shim it. Prefix every gate with
`export PATH="$HOME/.cargo/bin:$PATH"`. A run reporting that resolver error has measured the `PATH`,
not the diff.

**Gate after Task 25a (2026-08-21, controller-verified on `e2d0cba1` on a quiet tree, not carried
from a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero diagnostics), `cargo test -p qa-insights -p
qa-insights-sdk` **402 passed / 0 failed**, `cargo test -p qa-insights --features integration --lib`
**410 passed / 0 failed** (11.07s), working tree clean, nothing pushed.

**Gate after Task 24 (2026-08-21, controller-verified on `f2440db1` on a quiet tree, not carried
from a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean (zero warnings), `cargo test -p qa-insights -p
qa-insights-sdk` **360 passed / 0 failed**, `cargo test -p qa-insights --features integration --lib`
**368 passed / 0 failed** (7.29s), working tree clean, nothing pushed.

**Gate after Task 23b (2026-08-21, controller-verified on `52a7c140` on a quiet tree):**
`cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p qa-insights-sdk --all-targets
-- -D warnings` clean (zero warnings), `cargo test -p qa-insights -p qa-insights-sdk` **344 passed /
0 failed**, `cargo test -p qa-insights --features integration --lib` **352 passed / 0 failed**,
working tree clean, nothing pushed.

**Gate after Task 23 (2026-08-21, controller-verified on `dc6bf6f7` on a quiet tree):**
`cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p qa-insights-sdk --all-targets
-- -D warnings` clean (zero warnings), `cargo test -p qa-insights -p qa-insights-sdk` **329 passed /
0 failed**, `cargo test -p qa-insights --features integration --lib` **336 passed / 0 failed**,
working tree clean, nothing pushed.

**Gate after Task 22 and the Phase A squash (2026-08-21, controller-verified on `0b7548d5` on a
quiet tree):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p qa-insights-sdk
--all-targets -- -D warnings` clean (zero warnings), `cargo test -p qa-insights -p qa-insights-sdk`
**305 passed / 0 failed**, `cargo test -p qa-insights --features integration --lib` **312 passed /
0 failed**, `cargo test -p cf-gears-event-broker` **9 passed / 0 failed** (unchanged by the squash,
which is what proves the cross-gear commit survived it), working tree clean, nothing pushed.

**Gate after Task 21b (2026-08-21, controller-verified on the pre-squash `84725be1` on a quiet
tree):**
`cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p qa-insights-sdk --all-targets
-- -D warnings` clean (zero warnings), `cargo test -p qa-insights -p qa-insights-sdk` **293 passed
/ 0 failed**, `cargo test -p qa-insights --features integration --lib` **300 passed / 0 failed**
(seven Postgres containers, ~6.7s), working tree clean, nothing pushed.

**Gate after Task 21a (2026-08-21, controller-verified on `d22c9416` on a quiet tree, not carried
from a subagent report):** `cargo fmt --all -- --check` clean, `cargo clippy -p qa-insights -p
qa-insights-sdk --all-targets -- -D warnings` clean, `cargo test -p qa-insights -p qa-insights-sdk`
**281 passed / 0 failed** (266 at `0c1f4c27` + 15), working tree clean, nothing pushed.

**Gate at the end of Phase A (2026-08-21, after the whole-phase review's fix wave):** qa-insights
**247** tests (**253** with `--features integration --lib`, six Postgres containers, ~6s), zero
failures; `cargo fmt --all -- --check` and
`cargo clippy -p qa-insights -p qa-insights-sdk --all-targets -- -D warnings` clean. The review
measured **246**/**252** at `72f95434`; the fix wave added one test
(`system_actor::tests::every_site_binds_the_tenant_it_was_given`) and no test was removed or
weakened. The progression across Phase A: **21**/**23** at the Task 11 audit, **92**/**96** after
Task 12, **142**/**147** after Task 15, **247**/**253** after Task 19 and its review.

**`cargo test --workspace` cannot pass and never could**, for a reason that is not this gear's:
the doctest at `libs/toolkit/src/api/operation_builder.rs:932` references
`CORE_GLOBAL_BASE_LICENSE_FEATURE` out of scope, identically on `main`. Use
**`cargo test --workspace --lib --bins --tests`**. Tasks 19 and 40 spell the literal
`cargo test --workspace` in their gate blocks; that is the substitution to make. The **Verification
gate** section states this once, in full, with the second correction that matters — `--all-targets`
runs a 20-minute criterion bench — and is the copy to keep true if these ever disagree.

**Gate after Task 15:** qa-insights **142** tests (**147** with `--features integration --lib`,
five Postgres containers, ~5s), zero failures; `cargo fmt --all -- --check` and
`cargo clippy -p qa-insights -p qa-insights-sdk --all-targets -- -D warnings` clean under both
feature configurations. `cf-gears-event-broker` **9** (was 6), zero failures — Task 13 changed
that gear under carried obligation 2 and added the first tests its `config.rs` has ever had.
(qa-insights was **92**/**96** after Task 12 and **21**/**23** at the Task 11 audit.)
`cargo gears lint --dylint` and `cargo-shear` are **not installed** on any machine this plan has
been executed on and have **never been run, in any phase**. Do not assume they pass.

**A note on the execution machine, 2026-08-20.** Tasks 13-15 ran on a host with no rustup and
only Rust 1.75 in `/usr/bin`; the workspace pins **1.97.0** and is edition 2024. There is no
passwordless sudo, so three things were installed to get a build at all — `rustup` + 1.97.0,
`cmake` into `~/.local` (`libz-ng-sys` needs it), and `protoc` into `~/.local`
(`cargo build --workspace` needs it, though the qa-insights crates alone do not). Anyone resuming
on a fresh host should expect the same three. `docker` was present and the `integration` tier was
run on every pass.

### Open decision for Task 16 — SETTLED as option A

**Settled 2026-08-20 by Task 16 itself, and recorded here 2026-08-21** by Phase A's whole-phase
review, which found this section still presenting the choice as open four tasks after it was
made. Kept rather than deleted: the three options and their costs are the reasoning behind the
shape Phase B inherits, and Task 40's scope was reduced by this decision.

**What was taken: option A, in full.** `9a741685` shipped `ReconcileService::rebuild`, the REST
tier (`api/rest/{mod,dto,error}.rs`, `routes/`, `handlers/`), the `QaRunsClientV1`-backed
`RunsReader` adapter (`infra/clients/qa_runs.rs`), the `AppServices` aggregate, and `init` wiring
for the database, the `AuthZ` enforcer, the config and the qa-runs client. **Task 40 shrank
accordingly** and now owns: starting the consumer, the three leader-elected tickers, the elector,
the oagw client, the qa-catalog client lookup, and registering `QaInsightsLocalClient`. Tasks 17,
18 and 19 all built on that tier rather than each building their own, which is the payoff.

The "one change worth making under any of the three" was also made: `reproject_run` takes its
`AccessScope` as a parameter. Phase A's review then threaded a second parameter through the same
function for the same reason — `SystemActorSite`, because the audit identity of its qa-runs
re-read also differs per caller and it had been minting the consumer's for all three.

The original text follows.

**Task 16 cannot ship a working endpoint without pulling Task 40's composition root forward, and
nobody has decided whether it should.** Raised 2026-08-20 and left open deliberately.

This gear has no REST layer at all: `api/mod.rs` is a doc comment, there is no `AppServices`, and
`gear::init` resolves only the database and the AuthZ client. Task 16 Step 3 says "register the
route". For a registered route to *serve*, this task must also build the `QaRunsClientV1`-backed
`RunsReader` adapter, assemble a services aggregate, and wire `register_rest` — roughly eight new
files. Three ways to go:

| Option | What Task 16 ships | What it costs |
|---|---|---|
| **A — full** (recommended) | `rebuild`, the REST scaffolding, the SDK adapter, `init` wiring | Task 16 grows; Task 40 shrinks to consumer start, tickers, elector, oagw, local client |
| **B — domain only** | `ReconcileService::rebuild` + its test; route deferred | Smallest diff, but contradicts Step 3, and a route that cannot serve is worse than no route |
| **C — batch** | Skip to 17–19, do the whole REST layer once | Fewest half-built layers; leaves Task 16 open longest |

**One change worth making under any of the three.** `IngestService::reproject_run` currently
builds its own `AccessScope::for_tenant` internally. That is right for the two callers it has
(both background, both tenant-from-envelope), and wrong for the rebuild path, which has a real
request-scoped caller whose scope should come from the `PolicyEnforcer` — and whose compiled scope
may be *narrower* than the whole tenant. Make the scope a parameter, so every call site states
which one it is using; `upsert_run_results` already fails closed via `validate_tenant_in_scope` if
the two disagree.

### Carried out of Tasks 13-15

Each is stated in full on the module it belongs to; this is the index.

1. **Ingest is quadratic in a run's test count** — `domain::service::ingest`'s header. qa-runs
   publishes one `test.result` per result row and the projection re-reads the whole run per event.
   Correct and idempotent, but worse than legacy's timer-amortised bulk replace. **The remedy is a
   consumer change only** — a `TxConsumerHandler` with `ConsumerBatching`, collapsing a batch to
   its distinct run ids — and no domain code moves. **Task 40 owns it.**
2. **Two live defects in `event-broker-sdk`'s public API**, both measured against
   `postgres:15-alpine`, both worked around inside this gear and **neither reported upstream yet**
   — `m20260818_000002_offset_store`'s header has the evidence. `LOCAL_DB_OFFSET_STORE_MIGRATION_SQL`
   spells a reserved word unquoted (the table cannot be created) and declares `updated_at
   TIMESTAMP` where its own entity reads `TIMESTAMPTZ` (the row cannot be read back — writes
   succeed and every read errors, so a consumer built on it silently replays its whole topic on
   every restart). Worth filing against that crate.
3. **Which tenants does the reconcile ticker sweep?** — `domain::service::reconcile`'s header.
   `reconcile_once` takes the tenant because this gear has no registry to enumerate. The honest
   options are a `qa_ingest_watermarks` scan (only finds tenants already seen once) or a platform
   tenant directory (a new cross-gear dependency). **Task 40 owns it.**
4. **Leader election in this gear is an optimisation, not mutual exclusion** —
   `infra::leader`'s header says so rather than claiming otherwise. Two replicas sweeping one
   tenant still produce a correct projection. The one operation that would make election a
   correctness requirement is a rebuild that **truncates before rewriting**, so Task 16 must not
   do that.
5. **Legacy has six disagreeing status classifications** — the table is on
   `domain::service::ingest`'s header. Task 14 ported one (the five counters). The trap for
   **Tasks 20/21/24** is `bucketize_status` (`analytics.rs:1940-1946`), which buckets everything
   except `PASSED`/`FAILED`/`ERROR` to `NOT_RUN` — so a `SKIPPED` file is `NOT_RUN` in the
   analytics universe while it is `StatusBucket::Skipped` in the counters. Reusing Task 14's
   `classify` for the universe core would change numbers the UI already renders.

   **This ruling said "four" and the count is five.** Task 18's Step 0 measured the fifth — the
   daily trend's two-counter fold (`manager/src/routes/dashboard.rs:218-219`): `PASSED` and
   `FAILED`+`ERROR` only, no total and no skipped counter, so a `SKIPPED` row moves neither. The
   correction was recorded in Task 18's own Step 0 findings below ("Four things Step 0 found",
   item 1: "ruling R5's table of four legacy classifications is really five") and never reached
   the table on `ingest.rs`, which is the file this ruling points every Phase B task at. It now
   carries the fifth row, added by Phase A's whole-phase review. **A Phase B task consulting the
   index must learn that one legacy endpoint, `api_dashboard`, contains two of the five** — its
   per-run counters and its daily fold — and that a third row-inclusion rule lives in the same
   endpoint's 24-hour KPI window (carried item 13, **Task 21b's** — 21a is the pure fold and never
   opens that surface).
   **Amended 2026-08-21 by Task 21b, and this ruling has now been wrong twice in the same
   direction — it said "four", then "five", and the count is six.** The sixth is the KPI query's
   pass-rate **denominator** (`manager/src/routes/dashboard.rs:329`, `:337`):
   `tr.status IN ('PASSED','FAILED','ERROR')`, a three-way partition that silently excludes
   `SKIPPED` and every other spelling from the total while the numerator is `= 'PASSED'` and the
   failed counters are `IN ('FAILED','ERROR')`. Controller-verified at source before Task 21b's
   review, not carried from a report. It is genuinely none of the five: its two counters match the
   daily trend's, but no other row in the table expresses that denominator.

   **Two consequences a task consulting this index must not miss.** First, `api_dashboard` now
   contains **three** of the six — its per-run counters (`:167-178`), its daily fold (`:218-219`)
   and this denominator — on top of the 24-hour KPI window's row-inclusion rule, which is a
   *fourth* rule inside the same endpoint and is documented as carried item 13. Second, **Task 23
   inherits this sixth classification rather than deriving a seventh**: legacy's flaky fold
   (`:388`) and quality-vector fold (`:487`) use the identical denominator, and their windows
   (`:391`, `:490`) coalesce the same way over seven days — so Task 23 also inherits
   `run_created_at`, the column Task 21b added. The `ingest.rs` table's sixth row names both
   citations in its "Ported by" column so the reuse is pre-authorised.

   **Reattributed 2026-08-21, after Task 23 shipped.** The sentence above said Task 23 would reuse
   the sixth classification for both dashboard folds, and Task 23 shipped **neither** — it ported the
   *analytics* flaky fold (`analytics.rs:1655`, which uses `build_stats_map`'s three-way split, not
   this row) and correctly declined the two dashboard fields. So the sixth row's consumers are now
   **Task 23b** (`flaky_tests`, legacy `dashboard.rs:388`) and **Task 25**
   (`quality_vectors_pass_rate`, legacy `:487`). `domain::service::ingest`'s table still carries the
   old attribution in its "Ported by" cell; **Task 23b owns correcting it**, because Task 23b is what
   makes the new attribution true. Task 23 raised the staleness rather than editing a cell this
   ruling owns, which was right.

6. **`classify_all` has no caller yet**, deliberately: every aggregate is computed on read, which
   is what legacy does. **First consumer is Task 18.**
7. **The reconciler does not reconcile in-progress runs**, where legacy re-persisted live counts
   every 30s — `list_runs_finished_since` never returns a `NULL` `finished_at`. Not lost data (the
   run lands in full at `run.finished`), but **Task 18's dashboard must read active runs from
   qa-runs live**, which is what that task already specifies.

### Carried into the next tasks


1. ~~**Task 12 owns the `app_build` column** (Step 0b)~~ — **done**, in `ef878c22`. The column is
   in all three dialect blobs, on `entity::test_result`, on `qa_insights_sdk::TestResultRecord`
   and `domain::repos::NewTestResult`, and on `domain::analytics::ExecRow` as
   `build: Option<String>`. **Task 24 owns the `"unknown"` fallback**; the repository carries the
   `None` through deliberately, so "no build reported" stays distinguishable from a run that
   literally reported the string `unknown`. Who feeds it is item 9.
2. ~~**DECIDED 2026-08-20 — Task 13 fixes `event-broker`'s two missing serde defaults.**~~ —
   **done**, and the premise was only partly right; see Task 13's header for the measured
   three-row table. `#[serde(default)]` fixes an *empty* `config: {}` stanza. A missing
   `event-broker:` key still fails `GearNotFound` and a key with no `config:` still fails
   `MissingConfigSection`, both from `gear_config_required` rather than from serde, so the
   residual operator obligation is "write an empty stanza". Removing that too needs
   `ctx.config_or_default()` plus a `Default` on `EventBrokerConfig` — a change to that gear's
   operator contract, deliberately not made. `cf-gears-event-broker` went 6 → 9 tests. The
   original text follows. Linking
   qa-insights drags in `event-broker`, whose `init` reads `ctx.config()` (required, not
   `config_or_default`) while `EventBrokerConfig::{mode, default_storage_backend}` have no serde
   defaults — so any deployment linking this gear refuses to boot until an operator configures a
   broker that is still a skeleton. Measured, not reasoned. Only the dev config was patched.

   **The decision:** Task 13 adds `#[serde(default)]` to those two fields in
   `gears/system/event-broker/event-broker/src/config.rs`, as an additive change **gated on that
   gear's own test suite staying green**. Chosen over the two alternatives, which were: accept the
   operator-config obligation (every deployment linking qa-insights must configure a broker that
   does nothing yet), or drop `event_broker` from qa-insights' `deps` (which would lose the
   initialisation ordering that `deps` exists to give — see the `deps` doc in `src/gear.rs` for
   the failure qa-runs measured when its list was too short). Task 13 should run
   `cargo test -p cf-gears-event-broker` before and after and record both counts.
3. ~~**`UniverseFilter::since` and `list_counts_for`'s slice have zero consumers**~~ — both now
   measured, by `the_since_bound_applies_to_the_coalesced_timestamp` and
   `counts_are_read_for_several_repositories_in_one_call`. What is *not* measured, and is
   recorded on the method: `list_counts_for`'s empty-slice guard is defensive only on this
   dependency version — `sea-query` 0.32 already renders an empty `is_in` as a false condition,
   so removing the guard turns nothing red.
4. **Task 23 must resolve platform ids to names** — `ExecRow::platform_id` is a `Uuid`, but
   legacy's grouped summaries use the platform *name* as the UI-visible label.
5. **Phase C owes the notification claim release** (`notifications.rs:565-591`): legacy deletes
   the dedupe row when a send fails so a retry can take it. Unported, so a transient outage
   would suppress a notification permanently. Task 12 confirmed there is no
   `release_notification` on `NotifyRepository` and did not add one; the task that ports the
   send path owns the decision.
6. **One read on `ResultsRepository` is unbounded: `list_for_universe`.** It returns every row
   its filter admits, with no `LIMIT` — legacy's shape, and the method whose contract *is* "the
   rows an analytics read is about", so there is nothing to reduce. `UniverseFilter::since`
   exists so a caller can push its window into SQL; nothing forces one to. The task that adds
   the first caller (Task 18 onward) should decide whether a `LIMIT` belongs on the trait.
   `latest_per_test` and `ingested_run_ids_between` were also unbounded in `ef878c22` and are
   **not** any more — `f16cc64a` pushed both reductions into SQL via `project_all`, after the spec
   review showed the "`SecureSelect` cannot project" justification was false. **`project_all`
   (`libs/toolkit-db/src/secure/select.rs:396`) is the tool to reach for** whenever a repository
   in this gear needs a projection, `GROUP BY`, `DISTINCT` or a subquery: it hands the closure the
   already-scoped `Select`, so the scope cannot be dropped — unlike `into_inner()` (`:416`), which
   returns the raw one.
7. ~~**`qa_test_case_results` still has no reader, and its first consumer is Task 17 — not Task
   21, as Task 12 twice claimed.**~~ — **done in Task 17.** `ResultsRepository::list_case_page`
   is the reader and `infra::storage::mapper::test_case_result_to_sdk` the conversion. It turned
   out to be a ten-field infallible move with no column to decode, so the collection calls
   `paginate_odata`, **not** `paginate_odata_try` — that variant is qa-runs' because `run_to_sdk`
   really can fail. Original entry below.

   **`qa_test_case_results` still has no reader, and its first consumer is Task 17 — not Task
   21, as Task 12 twice claimed.** Task 17 registers `GET /qa/v1/test-case-results` (its Step 3),
   and in qa-runs an `OData` collection *is* a repository method calling `paginate_odata_try`
   with the mapper's `*_to_sdk` (`qa-runs/src/infra/storage/runs_sea_repo.rs:248-266`) — so Task
   17 owns the reader **and** the `TestCaseResultRecord` conversion Task 12 deferred. Task 21's
   `attach_case_summary` port is a later consumer. Deferring the conversion was still right for
   Task 12's scope; the reason given for it was wrong by four tasks. The write side is guarded by
   `a_runs_case_rows_are_written_with_every_column`, which reads the entity back through the
   secure extension.
8. ~~**Task 13 should decide the integration tier's container strategy before adding a fourth.**~~
   — **decided: keep one container per test.** Task 13 added a fifth and measured the tier at
   **5.07s** for all five started concurrently, so the contention that motivated the 30s→90s
   raise is not currently binding. Sharing also introduces a worse hazard than it removes: a
   process-wide `OnceCell` container outlives the per-test `#[tokio::test]` runtime that created
   its `testcontainers` handle. The full argument, including why the previously recorded reason
   ("these tests assert table inventories") was *not* the operative one, is on
   `infra::storage::test_db::wait_for_tcp`. Revisit when the tier is slow enough to measure. The
   original text follows.
   The tier now starts **one Postgres container per test** — four of them
   (`the_postgres_schema_executes_and_matches_sqlite`,
   `every_entity_round_trips_through_the_real_postgres_schema`,
   `a_lost_claim_inside_a_transaction_leaves_the_transaction_usable`,
   `an_advance_no_op_inside_a_transaction_leaves_the_transaction_usable`) — and `cargo test` runs
   them in parallel. The TCP readiness deadline was raised 30s→90s after one cold-cache timeout
   that surfaced in the *schema* test rather than the newly added one, which is the signature of
   contention; three warm repeats then ran in ~2.2s each. **That widened the window rather than
   removing the cause.** Sharing one container with a per-test `CREATE DATABASE` would remove it;
   the "these tests assert table inventories" objection only applies to the schema-parity test,
   and that one could keep its own database. Tasks 13-15 add more containers, so this is the last
   comfortable moment to decide. Not restructured here: it is a harness change with no bearing on
   Task 12's correctness.
9. ~~**`qa_test_results` now has two columns Task 13 must feed, and one it must not.**~~ —
   **done**, in `7255d812`. `app_build` comes off the cached `qa_runs_sdk::Run`, and
   `every_denormalized_column_comes_from_its_own_field_on_the_run` uses distinct fixture values
   per column so a transposition (both are `Option<String>` on both sides) cannot pass. The
   producer's order is preserved and pinned by `the_file_rows_keep_the_producers_order`; nothing
   sorts, dedupes or partitions the batch between the port and the repository.
10. ~~**`upsert_run_results` must be called inside the offset-committing transaction.**~~ —
    **done**, in `7255d812`. `LifecycleProjector::handle` opens one `Db::transaction_ref` and runs
    both the projection and `commit_offset_in_tx` inside it.
    `a_failed_projection_leaves_neither_rows_nor_offset_behind` is the falsifiable form: a
    commit-then-write consumer passes the idempotence test and fails that one.
11. **The truncation rule classifies by *writer*, and it should classify by *value
    provenance* — an observation for Tasks 27/31, deliberately not fixed here.**
    `mapper.rs`' header says the bug and settings writers "take operator input", which is how
    `qa_test_case_collect` slipped through the rule's earlier wording and had to be fixed in
    `45dafe9b`. The same ambiguity remains: `qa_jira_bugs.{test_name,plan_path}` are
    *producer-derived* text (the runner's test name and the plan path) written through an
    operator-facing surface, and they are untruncated. `test_name` is `VARCHAR(512)` and
    `plan_path` `VARCHAR(1024)`, so on Postgres an over-long value is `22001` on a bug-filing
    path. Left alone because deciding it means deciding whether a JIRA filing is operator input
    validated at the boundary (refuse) or producer text (truncate) — a Task 27/31 question about
    that surface, not a storage question. Whoever answers it should reword the header rule in
    terms of where the *value* came from rather than which writer carries it.
12. **`SQLite` cannot see three of this gear's properties**, so a test for any of them belongs on
    the `integration` tier from the start: a failed statement aborting a transaction (which was a
    real critical defect in `watermark_sea_repo::advance`, invisible to five `SQLite` tests),
    `VARCHAR` width enforcement (`SQLite` has type affinity, not width — every truncation test
    here asserts the non-enforcement first so it cannot become the reason nobody noticed), and any
    genuine write race, since the in-memory harness serialises every writer through its
    single connection.
13. **Six of the seventeen `DashboardStats` fields are unfilled, and Task 18 did not ship them.**
    Ruled 2026-08-21 while Task 18 was under review, and recorded here because the reasoning is
    not derivable from either task's body. **Task 21b owns `failed_recent` and the four 24-hour
    counters** (`failed_24h_count`, `failed_prev_24h_count`, `pass_rate_24h`,
    `pass_rate_prev_24h`): they are computable from `qa_test_results` alone, but legacy's KPI
    query (`manager/src/routes/dashboard.rs:317-348`) carries **no phase restriction** and windows
    on `COALESCE(finished_at, created_at)`, so it counts in-progress runs — a *third* row-inclusion
    rule inside `api_dashboard`, distinct from both the per-run counters and the daily trend, and
    it needs its own Step 0. **Task 23 owns `flaky_tests` and `quality_vectors_pass_rate`.**
    `total_plans`, `total_schedules` and `platforms_summary` need cross-gear reads no port here
    has, and stay genuinely unowned. The consequence for whoever finishes the payload:
    `GET /qa/v1/dashboard` does **not** discharge `cpt-cf-qa-fr-insights-dashboard` on its own —
    that requirement names pass rates (`docs/PRD.md:579`) — and the ten unfilled fields are
    **absent from the wire rather than emitted as zeros**, so filling one means adding a key, not
    correcting a value.

    **Amended 2026-08-21 by Task 19, and this half is not owned by anybody.** The item above
    reads as if pass rates were the only gap in that requirement. They are not: the clause's
    second bullet is a **coverage** view (`docs/PRD.md:580`), Task 19 ships that endpoint's
    *shape* and no data, and no task in this plan closes it. Two independent upstreams are
    missing — legacy parses the percentages out of Argo workflow log text
    (`manager/src/routes/dashboard.rs:606-611`, `manager/src/services/argo.rs:2718-2728`) which
    this architecture has no reader for before p2 (`qa-runs-sdk/src/models.rs:376-380`,
    `log_storage_ref`, "populated on completion (p2 with 2.7)"), and the grouping key
    `product_key` is open question 1. **Worse for whoever audits this requirement:** the PRD's
    own wording for the bullet — "which tests and plans ran against which product versions and
    platforms" — describes *execution* coverage, which `qa_test_results` could answer, while
    legacy's `api_coverage` answers *code* coverage. D1-D10 do not reconcile the two. Task 19
    ported legacy's, per D3 and per its own plan section, and raised the disagreement rather than
    settling it. **Nothing in the gear claims either question is settled**, and a task that
    inherits this item must not assume the coverage half is done.

---

</details>

## Plan shape: one plan, four phases

**Phase 0 — Cross-gear prerequisites, Tasks 1–7.** Additive changes to `qa-runs` and `qa-catalog` that features 2.5/2.8 turn out to require. Nothing in Phase A can start without them.

**Phase A — Foundation, Tasks 8–19.** Gear + SDK + schema + transactional ingest + reconciler + rebuild + per-test history + dashboard + coverage.

**Phase B — Analytics, Tasks 20–30.** The eight-section overview, export, build-tests, the three plan drill-downs, saved views, and expected-case collection (static + exact).

**Phase C — JIRA and notifications, Tasks 31–40.** Bug registry, poller, skip-lists, auto-rerun; Slack and email routing, dedupe, log.

**Why one plan, four phases.** The four share one file map, one verification gate, one legacy-citation protocol and one schema. Splitting them into four documents would duplicate all of that four times. They remain **four separately squashable deliverables** (see "Commit discipline"), so reviewability is preserved without the duplication. B and C are mutually independent once A lands and may be executed in either order or in parallel worktrees.

---

## Decisions taken before writing this plan

D1–D9 are stated in full, with their legacy citations and rejected alternatives, in
`docs/superpowers/specs/2026-08-18-qa-insights-design.md` §3. They are **not** restated here; read that section before starting Task 1. In one line each:

| | Decision |
|---|---|
| **D1** | Two result granularities restored; `qa-runs` amended additively (`nodeid`, `reason`, `ticket`) |
| **D2** | Expected-case counts split by owner: static from `qa-catalog`, exact via a `Collect` run kind in `qa-runs` |
| **D3** | The full analytics surface is ported (eleven routes, eight-section overview) |
| **D4** | Saved views keep their real key `(owner_id, scope, plan_id, name)` |
| **D5** | Slack, queue events, scheduled-run events, dedupe and the log are all ported |
| **D6** | `notification_rules` is dropped; config becomes typed per-tenant singletons |
| **D7** | Legacy query parameters on aggregates; OData only on the two flat collections |
| **D8** | Auto-rerun requires a new build, not just a resolved transition |
| **D9** | Per-schedule notification settings become `qa_schedules` columns |

### D10 — Email ships configured but unsent; the send is a port with no adapter

Discovered while writing this plan, after the spec was approved, and resolved with the user on 2026-08-18.

| | |
|---|---|
| **Spec claims** | PRD `cpt-cf-qa-fr-insights-notifications`: email "via the platform outbound gateway". PRD §11 defers convergence to a "Notifications Service" at p3. |
| **Platform truth** | oagw speaks **HTTP, SSE and WebSocket only** — `ServiceGatewayClientV1::proxy_request`'s protocol-mapping table (`gears/system/oagw/oagw-sdk/src/api.rs:141-156`) enumerates exactly those three, and `models.rs:292-299` marks gRPC "future use". There is no SMTP path. `lettre` appears nowhere in the workspace, and `gears/system/` contains no notifications gear. The requirement as written is not implementable. |
| **Legacy does** | Sends SMTP directly with `lettre` (`manager/src/services/notifications.rs:11`, `send_email` at `:133`). |
| **Resolution** | **Port everything except the socket.** The email configuration surface, the routing decisions that select email recipients, the `qa_run_notifications` dedupe rows and the `qa_notification_log` entries all ship and are all tested. `MailClient` is a domain port whose only adapter, `UnsupportedMailClient`, records outcome `unsupported_egress` in the log and returns `Ok(())`. Slack ships fully working over oagw. When the platform grows an SMTP path the adapter swaps behind the port and no domain code changes. Task 38 carries the port; Task 39 carries the adapter and the log outcome. |
| **Rejected** | A second ADR-0005-style egress exception (a real hole in `cpt-cf-qa-contract-egress` for a feature the platform has committed to solving centrally); dropping email outright (a functional regression against legacy). |
| **Amends** | `cpt-cf-qa-fr-insights-notifications` — Task 2 Step 4. |

### Decisions inherited, not re-litigated

Four domain gears + qa-ui (ADR-0004); `qa-*` crate naming; full Fabric tenancy; structured events via event-broker SDK (ADR-0002); execution behind `RunExecutor` with a mock in p1 (ADR-0001); git egress confined to qa-catalog (ADR-0005); branch `feature/qa-platform-specs`, local, one squashed commit per deliverable.

---

## Legacy-verification protocol (applies to every task below)

The governing principle, restated by the user on 2026-08-18: **preserve legacy behavior; adapt only the implementation to the gear architecture.**

Every task that ports a rule carries a **Step 0: verify against legacy**. That step is not optional and not a formality. It means:

1. Open the cited legacy file at the cited lines. Read the surrounding function, not just the line.
2. Confirm the behavior this plan claims. If the plan is wrong, **the legacy code wins** — write down what it actually does, in the task report, and implement that.
3. If legacy and a spec document disagree and the disagreement is not already covered by D1–D10, stop and raise it. Do not silently pick one.
4. Cite file and line in the test's doc comment, so the next reader can re-verify without re-deriving.

Line numbers in this plan were read on 2026-08-18 against the `../vhp-testrunner` working tree. If a citation does not land where the plan says, search for the named symbol rather than trusting the number, and correct the number in the plan as part of your task.

---

## Architecture mapping: legacy → gear

| Legacy | Gear |
|---|---|
| `routes/analytics.rs::api_overview` (eight computed sections) | `domain::service::analytics::overview` over the two pure cores |
| `routes/analytics.rs::count_test_functions:1865` | `qa-catalog` SDK projection (D2), consumed via the `CatalogReader` port |
| `routes/analytics.rs::api_export` | `domain::analytics::export` (CSV projection of the same core output) |
| `routes/analytics.rs` saved-view CRUD `:521-703` | `domain::service::saved_views` + `qa_analytics_saved_views` |
| `routes/dashboard.rs::api_dashboard:84` | `domain::service::dashboard`, reading run counts locally and active/queued over `RunsReader` |
| `routes/dashboard.rs::api_coverage:573` | `domain::service::dashboard::coverage` |
| `services/collect.rs` (hourly poller + on-demand) | `domain::service::collect` + a leader-elected ticker; launches via `RunsLauncher` (D2) |
| `services/run_results_poller.rs` (polls Argo for results) | **replaced** by the transactional event consumer + the reconciler (§ Task 12, Task 15) |
| `services/run_history.rs` | split: run-level history is qa-runs'; analytical history is `qa_test_results` |
| `services/jira.rs` | `domain::jira::registry` (pure) + `JiraClient` port over oagw |
| `services/jira_poller.rs` | `domain::service::jira_poller` + a leader-elected ticker |
| `services/notifications.rs` | `domain::notify::routing` (pure) + `SlackClient`/`MailClient` ports |
| `settings` table rows `jira`, `jira_poller`, `notifications` | typed per-tenant singleton tables (D6) |

---

## File map

Created unless marked. Paths are workspace-relative.

### Phase 0 — modified, existing gears

```
gears/qa-platform/qa-runs/qa-runs/src/infra/storage/migrations/m20260818_000005_case_fidelity.rs   (new)
gears/qa-platform/qa-runs/qa-runs/src/infra/storage/migrations/m20260818_000006_schedule_notifications.rs (new)
gears/qa-platform/qa-runs/qa-runs/src/infra/storage/migrations/mod.rs                (modify: register both)
gears/qa-platform/qa-runs/qa-runs/src/infra/storage/entity/run_test_result.rs        (modify: 3 columns)
gears/qa-platform/qa-runs/qa-runs/src/infra/storage/entity/schedule.rs               (modify: 3 columns)
gears/qa-platform/qa-runs/qa-runs/src/infra/events/payloads.rs                       (modify: TestResult fields)
gears/qa-platform/qa-runs/qa-runs/src/domain/service/ingest.rs                       (modify: carry the 3 fields)
gears/qa-platform/qa-runs/qa-runs/src/domain/ports/run_executor.rs                   (modify: collect spec)
gears/qa-platform/qa-runs/qa-runs/src/domain/service/launch.rs                       (modify: Collect run kind)
gears/qa-platform/qa-runs/qa-runs-sdk/src/models.rs                                  (modify: RunTestResult, Schedule)
gears/qa-platform/qa-runs/qa-runs-sdk/src/client.rs                                  (modify: 3 new methods)
gears/qa-platform/qa-catalog/qa-catalog/src/domain/parsing/case_count.rs             (new)
gears/qa-platform/qa-catalog/qa-catalog-sdk/src/client.rs                            (modify: universe + counts)
gears/qa-platform/qa-catalog/qa-catalog-sdk/src/models.rs                            (modify: UniverseTest)
```

### Phases A–C — the new gear

```
gears/qa-platform/qa-insights/qa-insights-sdk/Cargo.toml
gears/qa-platform/qa-insights/qa-insights-sdk/src/lib.rs
gears/qa-platform/qa-insights/qa-insights-sdk/src/client.rs
gears/qa-platform/qa-insights/qa-insights-sdk/src/models.rs
gears/qa-platform/qa-insights/qa-insights-sdk/src/errors.rs

gears/qa-platform/qa-insights/qa-insights/Cargo.toml
gears/qa-platform/qa-insights/qa-insights/src/lib.rs
gears/qa-platform/qa-insights/qa-insights/src/gear.rs
gears/qa-platform/qa-insights/qa-insights/src/config.rs
gears/qa-platform/qa-insights/qa-insights/src/test_support.rs

  domain/error.rs
  domain/mod.rs
  domain/local_client/{mod.rs,client.rs}
  domain/ports/{mod.rs,runs_reader.rs,runs_launcher.rs,catalog_reader.rs,jira_client.rs,slack_client.rs,mail_client.rs,clock.rs}
  domain/repos/{mod.rs,results_repo.rs,collect_repo.rs,saved_views_repo.rs,jira_repo.rs,notify_repo.rs,watermark_repo.rs}
  domain/analytics/{mod.rs,universe.rs,universe_tests.rs,aggregates.rs,aggregates_tests.rs,export.rs,export_tests.rs}
  domain/jira/{mod.rs,registry.rs,registry_tests.rs}
  domain/notify/{mod.rs,routing.rs,routing_tests.rs,render.rs,render_tests.rs}
  domain/service/{mod.rs,ingest.rs,ingest_tests.rs,reconcile.rs,reconcile_tests.rs,dashboard.rs,dashboard_tests.rs,
                  analytics.rs,analytics_tests.rs,saved_views.rs,saved_views_tests.rs,collect.rs,collect_tests.rs,
                  jira.rs,jira_tests.rs,jira_poller.rs,jira_poller_tests.rs,notify.rs,notify_tests.rs,
                  test_support.rs,tenant_scoping_tests.rs}

  infra/mod.rs
  infra/storage/{mod.rs,db.rs,mapper.rs,odata.rs}
  infra/storage/entity/{mod.rs,test_result.rs,test_case_result.rs,test_case_collect.rs,saved_view.rs,
                        jira_bug.rs,jira_config.rs,jira_poller_config.rs,notification_config.rs,
                        run_notification.rs,notification_log.rs,ingest_watermark.rs}
  infra/storage/migrations/{mod.rs,m20260818_000001_initial.rs}
  infra/storage/{results_sea_repo.rs,collect_sea_repo.rs,saved_views_sea_repo.rs,jira_sea_repo.rs,
                 notify_sea_repo.rs,watermark_sea_repo.rs}
  infra/events/{mod.rs,consumer.rs,payloads.rs}
  infra/jira/{mod.rs,oagw_client.rs}
  infra/notify/{mod.rs,slack_oagw.rs,mail_unsupported.rs}
  infra/leader/mod.rs

  api/mod.rs
  api/rest/{mod.rs,dto.rs,error.rs}
  api/rest/handlers/{mod.rs,dashboard.rs,analytics.rs,saved_views.rs,collect.rs,jira.rs,settings.rs,admin.rs,collections.rs}
  api/rest/routes/{mod.rs,dashboard.rs,analytics.rs,saved_views.rs,collect.rs,jira.rs,settings.rs,admin.rs,collections.rs}

  tests/ingest_idempotence.rs
```

**Why the domain splits this way.** `analytics/` holds pure computation with no service or repository in scope; `service/` holds orchestration that reads repositories and calls ports. The two analytics files are split because `universe.rs` produces one data structure (latest-status-per-test, joined with the catalog universe) and `aggregates.rs` consumes it eight different ways — they change for different reasons, and `aggregates.rs` is the file that grows as sections are added.

---

## Verification gate

Every task ends green on:

```bash
cargo fmt --all -- --check
cargo clippy -p qa-insights -p qa-insights-sdk --all-targets -- -D warnings
cargo test -p qa-insights -p qa-insights-sdk
```

Phase 0 tasks additionally, and **before** the commit:

```bash
cargo test -p qa-runs -p qa-runs-sdk -p qa-catalog -p qa-catalog-sdk
```

Expected: the qa-runs suite reports its full existing count (778 tests as of the Phase B commit `c3abe942`) plus whatever the task adds, **0 failed**. A drop in that number is a regression, not a rounding difference — find it before committing.

The Postgres tier, where a task says so:

```bash
cargo test -p qa-insights --features integration --lib
```

Requires a Docker daemon (testcontainers). Mirrors `make test-qa-runs-pg`.

Workspace-level, once per phase before the squash:

```bash
cargo build --workspace
cargo test --workspace --lib --bins --tests
```

**Two corrections to this gate, measured 2026-08-21 and binding on every later phase.** The literal
`cargo test --workspace` **cannot pass and never could**, including before Task 1: a doctest in
`libs/toolkit/src/api/operation_builder.rs:932` fails to compile (`error[E0425]: cannot find value
CORE_GLOBAL_BASE_LICENSE_FEATURE in this scope` — the snippet uses a const defined at `:66` of its
own file without a `use`). It is identical on `main` at the merge-base and this branch has never
touched `libs/toolkit/`, so it is an upstream defect, not a regression here — but a phase that
reports this gate green either did not run it or absorbed the red. Fixing it is a one-line `use`
in a crate these tasks may not modify; file it separately.
Second, `--all-targets` additionally runs `libs/toolkit-db/benches/worker_overhead.rs`, a criterion
bench that takes **20+ minutes**. It does complete (a full run reached this gear and finished at
10818 passed / 0 failed), but two agents abandoned it mid-run believing it hung. Use
`--lib --bins --tests` as written above; reach for `--all-targets` only when a bench target itself
is in question.

---

## Commit discipline

One commit per task while working; each phase squashes to one commit before review:

* Phase 0 → `feat(qa-platform): qa-insights prerequisites — case fidelity, collect kind, catalog universe`
* Phase A → `feat(qa-platform): qa-insights — ingest, reconciler, history, dashboard`
* Phase B → `feat(qa-platform): qa-insights — analytics, saved views, collect`
  **— OWED AND OUTSTANDING.** Phase B was completed on 2026-08-24 and **deliberately not squashed,
  at the user's explicit decision**, so its ~40 per-task commits remain individually bisectable.
  Phase A *was* squashed, so a reader comparing the two will otherwise assume Phase B's squash was
  forgotten. It was not. Whoever performs it should follow Phase A's precedent exactly: a backup
  branch first, and `git diff` between the pre- and post-squash heads being **empty** as the
  acceptance test.
* Phase C → `feat(qa-platform): qa-insights — JIRA loop and notifications`

Branch: `feature/qa-platform-specs`, local, no push, no PR.

---

# Phase 0 — Cross-gear prerequisites

Six of these seven tasks modify **shipped** gears. The rule for all of them: **additive only.** New nullable columns, new optional event fields, new SDK methods, new run-kind variants. Nothing existing changes shape. The existing suites staying green is the gate, and it is checked before every commit in this phase.

### Task 1: Amend the specs to record D1–D10

**STATUS: ✅ COMPLETE** — `0e18503f` (Phase 0 squash). Verified D1–D10 recorded across `PRD.md`, `DESIGN.md`, `DECOMPOSITION.md` — all ten greppable in at least two of the three.

No code. This task exists because the plan's later tasks implement behavior that four specification documents currently contradict, and a reader who finds the contradiction later cannot tell which side is intentional.

**Files:**
- Modify: `gears/qa-platform/docs/PRD.md`
- Modify: `gears/qa-platform/docs/DESIGN.md`
- Modify: `gears/qa-platform/docs/DECOMPOSITION.md`

- [x] **Step 0: Read the design spec**

Read `docs/superpowers/specs/2026-08-18-qa-insights-design.md` end to end — §2 Findings and §3 Decisions especially. Every edit below is a mechanical consequence of one of them, and making the edits without having read the findings produces text that says what changed but not why.

- [x] **Step 1: Amend PRD §5.4**

Apply, keeping every FR id unchanged (ids are referenced from DECOMPOSITION and from this plan's spec-coverage table):

* `cpt-cf-qa-fr-insights-history` — record the two granularities: per-test-file rows and per-test-case rows (`nodeid`, `reason`, `ticket`), citing `manager/migrations/001_initial.sql:65` and `:253`. (D1)
* `cpt-cf-qa-fr-insights-dashboard` — replace "dashboard aggregates … and coverage views" with the enumerated eight computed sections of the overview payload: `summary`, `lists` (passed / failed / not_run), `heatmap`, `trend`, `build_distribution`, `flaky`, `quality_vectors`, `grouped` (component / tag / platform), plus the dashboard and coverage endpoints. Cite `manager/src/routes/analytics.rs:231-248`. (D3)
* `cpt-cf-qa-fr-insights-analytics` — enumerate the endpoints (`overview`, `build-tests`, `export`, `views` CRUD, `plan/{id}/tests`, `plan/{id}/builds`, `plan/{id}/test-history`) and record the parameter split from D7: legacy parameters on aggregates, OData on the two flat collections only. (D3, D7)
* `cpt-cf-qa-fr-insights-notifications` — widen from email-on-completion to: Slack (webhook and channel, with block rendering and scheduled-run templates) and email; six scheduled-run status events; two queue events with `Expired` mandatory and un-toggleable; the dedupe table; the audit log. Then add the D10 carve-out in the same words as this plan's D10 row: the email **send** is deferred because the platform has no SMTP egress, everything else about email ships. (D5, D10)
* `cpt-cf-qa-fr-insights-auto-rerun` — add the new-build precondition, citing `manager/src/services/jira_poller.rs:61-63` (the auto-rerun toggle) and `:65-70` (the new-build check). (D8)
* **New FR** `cpt-cf-qa-fr-insights-expected-cases` — static per-file test-function counts and exact `--collect-only` counts, with the `COLLECT_ONLY` / `VHP_COLLECT_URL` runner contract named as preserved. Cite `manager/src/services/argo.rs:50-57`. (D2)

- [x] **Step 2: Amend PRD §5.2**

Record in the qa-runs requirements that a `Collect` run kind exists, bypasses admission, and carries the two collect environment variables (D2); and that schedules carry three notification settings (D9). Both are qa-runs surface that qa-insights depends on, so they belong in the runs section, not the insights section.

- [x] **Step 3: Amend DESIGN**

* §3.7 — replace the one-line qa-insights table list with the eleven tables of Task 10. Record the three new `qa_run_test_results` columns and the three new `qa_schedules` columns in the qa-runs paragraph. (D1, D2, D4, D6, D9)
* §3.3 — expand the qa-insights endpoint inventory to the full list in Task 25/27/28/30/33/38; record the OData/legacy-parameter split. (D3, D7)
* §3.4 — add a dependency row: gear `qa-catalog`, interface `qa-catalog-sdk`, purpose "plan/TEST_META universe and static case counts for analytics". State in the dependency-rules paragraph that this is a second contract-mediated edge from insights and is non-cyclic (insights depends on `qa-catalog-sdk`, not on `qa-catalog`). (Finding 4)
* §3.5 events table — record the three additive `qa.test.result` fields and state explicitly that the type id and version are unchanged, because `cpt-cf-qa-interface-events` forbids mutating a published version and adding optional fields is not a mutation. (D1)

- [x] **Step 4: Amend DECOMPOSITION 2.5 and 2.8**

Restate both entries' **Scope**, **Data** and **API** bullets to match this plan. Add to 2.5's **Depends On**: `cpt-cf-qa-feature-catalog` (universe and static counts) alongside the existing `cpt-cf-qa-feature-runs-core`. Add a note to both that Phase 0 of `docs/plans/2026-08-18-qa-insights-gear.md` amends two shipped gears, so neither feature is buildable against the gears as they stand at commit `c3abe942`.

- [x] **Step 5: Verify the docs still build their own links**

Run: `lychee --config lychee.toml gears/qa-platform/docs/` (from the workspace root)
Expected: no broken links. If `lychee` is not installed, grep instead for every `cpt-cf-qa-` id you introduced or renamed and confirm each resolves to a definition:
Run: `grep -rn "cpt-cf-qa-fr-insights-expected-cases" gears/qa-platform/docs/`
Expected: at least two hits — the PRD definition and the DECOMPOSITION reference.

- [x] **Step 6: Commit**

```bash
git add gears/qa-platform/docs/PRD.md gears/qa-platform/docs/DESIGN.md gears/qa-platform/docs/DECOMPOSITION.md
git commit -m "docs(qa-platform): amend PRD/DESIGN/DECOMPOSITION for qa-insights D1-D10"
```

---

### Task 2: qa-runs — case fidelity columns

**STATUS: ✅ COMPLETE** — `0e18503f`. Verified `m20260818_000005_case_fidelity.rs` exists; `nodeid`/`reason`/`ticket` on `qa_run_test_results` and on `entity/run_test_result.rs` (3 fields).

**Files:**
- Create: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/migrations/m20260818_000005_case_fidelity.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/migrations/mod.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/entity/run_test_result.rs`

- [x] **Step 0: Verify against legacy**

Open `manager/migrations/001_initial.sql:253-270` (`test_case_results`). Confirm the three columns this task adds — `nodeid TEXT NOT NULL DEFAULT ''`, `reason TEXT`, `ticket TEXT` — and read the table comment above it: *"Per-function test case outcomes (xfail/xpass/skip/pass/fail) parsed from the runner's TEST_CASE markers … Lets analytics aggregate per-case, not just per-file."*

Then open `manager/src/services/argo.rs:2932-2943` and confirm the case-level status mapper falls through to `other.to_uppercase()` for unrecognized pytest outcomes. This is why the existing `status` column is documented as an open set, and the same openness applies to anything you add.

Record both confirmations in the task report.

- [x] **Step 1: Write the failing migration test**

Add to the existing test module at the bottom of the new migration file. The assertion is that the three columns exist after `up` and are gone after `down` — copy the shape of the column-inventory assertions in `m20260813_000003_initial.rs` (search for `index_names(` and the table-inventory helper it uses).

```rust
#[tokio::test]
async fn case_fidelity_adds_three_nullable_columns() {
    let conn = fresh_sqlite_with_migrations().await;
    let cols = column_names(&conn, "qa_run_test_results").await;
    assert!(cols.contains(&"nodeid".to_owned()), "nodeid missing: {cols:?}");
    assert!(cols.contains(&"reason".to_owned()), "reason missing: {cols:?}");
    assert!(cols.contains(&"ticket".to_owned()), "ticket missing: {cols:?}");
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-runs case_fidelity_adds_three_nullable_columns`
Expected: FAIL — `nodeid missing: [...]`.

- [x] **Step 3: Write the migration**

Three backends, matching the raw-SQL-per-backend shape `m20260813_000003_initial.rs` already uses. All three columns are **nullable or defaulted**, which is what makes this deployable against a populated table without a backfill.

```rust
//! Adds the three case-level columns `qa_run_test_results` needs in order to
//! carry the fidelity `qa-insights` analytics is built on.
//!
//! # Why these three, and why now
//!
//! The source system keeps per-*case* outcomes in a second table,
//! `test_case_results` (`manager/migrations/001_initial.sql:253`), whose own
//! comment states the reason: *"Lets analytics aggregate per-case, not just
//! per-file."* Three of its columns have no counterpart here — `nodeid`, the
//! pytest node identifier; `reason`, the xfail/skip explanation; and `ticket`,
//! the per-case bug reference the analytics list renders as a badge
//! (`manager/src/routes/analytics.rs:112-134`, `case_tickets`).
//!
//! Without them qa-insights cannot reproduce `OverviewSummary`'s six per-case
//! counters or `AnalyticsListItem::case_status`/`case_tickets` at all. See
//! decision D1 in `docs/plans/2026-08-18-qa-insights-gear.md`.
//!
//! # Additive by construction
//!
//! `nodeid` defaults to `''` rather than being nullable, for the same reason
//! `test_file` does: one spelling of "absent" keeps every comparison a plain
//! equality. `reason` and `ticket` are genuinely optional and stay `NULL`-able.
//! No existing row changes, no existing query changes, and the ingest path
//! writes the new columns only when the executor supplies them.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &str = r"
ALTER TABLE qa_run_test_results ADD COLUMN IF NOT EXISTS nodeid VARCHAR(1024) NOT NULL DEFAULT '';
ALTER TABLE qa_run_test_results ADD COLUMN IF NOT EXISTS reason TEXT NULL;
ALTER TABLE qa_run_test_results ADD COLUMN IF NOT EXISTS ticket VARCHAR(64) NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_run_test_results ADD COLUMN nodeid VARCHAR(1024) NOT NULL DEFAULT '';
ALTER TABLE qa_run_test_results ADD COLUMN reason TEXT NULL;
ALTER TABLE qa_run_test_results ADD COLUMN ticket VARCHAR(64) NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE qa_run_test_results ADD COLUMN nodeid VARCHAR(1024) NOT NULL DEFAULT '';
ALTER TABLE qa_run_test_results ADD COLUMN reason TEXT NULL;
ALTER TABLE qa_run_test_results ADD COLUMN ticket VARCHAR(64) NULL;
";

const DOWN: &str = r"
ALTER TABLE qa_run_test_results DROP COLUMN ticket;
ALTER TABLE qa_run_test_results DROP COLUMN reason;
ALTER TABLE qa_run_test_results DROP COLUMN nodeid;
";
```

Then the `MigrationTrait` impl, dispatching on `manager.get_database_backend()` exactly as `m20260813_000003_initial.rs` does — copy that file's `up`/`down` bodies and swap the constants. Do not invent a different dispatch shape.

**MySQL has no `ADD COLUMN IF NOT EXISTS`.** That is why `MYSQL_UP` differs from `PG_UP` by exactly that clause. Re-running the migration on MySQL is the migrator's job to prevent, not the statement's.

- [x] **Step 4: Register the migration**

In `migrations/mod.rs`, add `mod m20260818_000005_case_fidelity;` and push `Box::new(m20260818_000005_case_fidelity::Migration)` as the **last** element of the `migrations()` vector. Order is the apply order; a new migration always goes last.

- [x] **Step 5: Add the three fields to the entity**

In `entity/run_test_result.rs`, after `pub jira_key: Option<String>`:

```rust
    /// The pytest node identifier (`tests/test_x.py::TestC::test_m[param]`),
    /// `""` when the executor reported none. Legacy's `test_case_results.nodeid`
    /// (`manager/migrations/001_initial.sql:258`), which is likewise
    /// `NOT NULL DEFAULT ''`.
    pub nodeid: String,
    /// The xfail/skip explanation the runner emitted, if any. Legacy
    /// `test_case_results.reason` (`:262`).
    pub reason: Option<String>,
    /// A per-case bug reference, distinct from `jira_key`: `jira_key` is the
    /// file-level link legacy keeps on `test_results` (`:70`), `ticket` is the
    /// case-level one on `test_case_results` (`:263`). Analytics renders the
    /// latter as `AnalyticsListItem::case_tickets`
    /// (`manager/src/routes/analytics.rs:133`). Do not collapse them.
    pub ticket: Option<String>,
```

- [x] **Step 6: Run the test and the full qa-runs suite**

Run: `cargo test -p qa-runs case_fidelity_adds_three_nullable_columns`
Expected: PASS.

Run: `cargo test -p qa-runs -p qa-runs-sdk`
Expected: all pass, count ≥ 778. If any existing test fails, the change was not additive — fix it before proceeding rather than adjusting the test.

- [x] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-runs/qa-runs/src/infra/storage/
git commit -m "feat(qa-runs): case-level fidelity columns on qa_run_test_results"
```

---

### Task 3: qa-runs — carry the three fields through the event and the ingest path

**STATUS: ✅ COMPLETE** — `0e18503f`. Verified `payloads.rs` `TestResult` carries all three as `Option<String>` (`:557`, `:561`, `:564`); `domain/service/ingest.rs` references them throughout.

**Files:**
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/events/payloads.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/ingest.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/ports/run_executor.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/runs_sea_repo.rs` (the `// TASK 3:` marker at `:539` — see Step 4)
- Test: `gears/qa-platform/qa-runs/qa-runs/src/infra/events/payloads.rs` (in-file test module)

- [x] **Step 0: Verify against legacy**

Open `manager/src/services/argo.rs:2932-2943`. Confirm the case-status mapper and note which pytest outcomes it maps explicitly and which fall through. Open `manager/src/routes/runs.rs:1115` and `:1169` and confirm the progress event's `status` is an unvalidated `String` written straight to the insert. Both facts are already recorded in `m20260813_000003_initial.rs:411-440`; you are confirming they still hold, because the fields you add travel the same path.

- [x] **Step 1: Write the failing payload test**

The existing module has a test that pins every event's `TYPE_ID` and one that walks all eight arms. Add one that pins the *shape* of `TestResult` — specifically that the three new fields serialize as optional and that their absence round-trips:

```rust
/// The three case-level fields are **additive**: a payload serialized without
/// them must still deserialize, because `cpt-cf-qa-interface-events` forbids
/// mutating a published version and an executor that has not been updated will
/// keep sending the old shape.
#[test]
fn test_result_case_fields_are_optional_on_the_wire() {
    let old_shape = serde_json::json!({
        "run_id": "00000000-0000-0000-0000-000000000001",
        "node": "repo-smoke",
        "test_file": "tests/test_a.py",
        "test_name": "test_a",
        "status": "PASSED",
        "duration": null,
        "launch_id": null,
        "jira_key": null,
    });
    let decoded: TestResult =
        serde_json::from_value(old_shape).expect("old payloads must still decode");
    assert_eq!(decoded.nodeid, None);
    assert_eq!(decoded.reason, None);
    assert_eq!(decoded.ticket, None);
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-runs test_result_case_fields_are_optional_on_the_wire`
Expected: FAIL — `no field 'nodeid' on type 'TestResult'`.

- [x] **Step 3: Add the fields to the payload**

In `payloads.rs`, in `pub struct TestResult`, after `pub jira_key: Option<String>`:

```rust
    /// The pytest node identifier. `#[serde(default)]` is what makes this
    /// additive rather than a version bump: an executor still emitting the
    /// pre-2026-08-18 shape decodes with `None` here instead of failing the
    /// whole event. Same for the two below.
    #[serde(default)]
    pub nodeid: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Case-level bug reference. Distinct from `jira_key`; see the entity.
    #[serde(default)]
    pub ticket: Option<String>,
```

Leave `TYPE_ID` and every constant untouched. The event stays `…run.test_result.v1`.

- [x] **Step 4: Thread the fields through the executor port and the ingest path**

In `domain/ports/run_executor.rs`, find the executor-supplied per-test result type (search for `test_name` in that file) and add the same three `Option<String>` fields with the same doc rationale.

**Before anything else in this step, open `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/runs_sea_repo.rs:539`.** Task 2 left a trap there, deliberately and with a `// TASK 3:` marker: the `upsert_test_result` write path hard-codes `nodeid: ActiveValue::Set(String::new())`. Widening `NewTestResult` to carry a nodeid produces **no compile error at that site** — the repository goes on writing `''`, every case-level nodeid is silently dropped, and no test fails. The column simply stays empty forever.

This is the one failure mode in Task 3 that nothing catches for you. Change that line to write the incoming value, and add a repository-level test that inserts a row with a non-empty nodeid and reads it back — a test at the service level passes whether or not the repository forwards the field.

In `domain/service/ingest.rs`, find where a test result is turned into a row and into a `TestResult` payload (search for `jira_key`) and carry the three new values through both. `nodeid` maps to the column's `NOT NULL DEFAULT ''`, so write `unwrap_or_default()` at the column boundary and keep the `Option` on the wire:

```rust
    nodeid: result.nodeid.clone().unwrap_or_default(),
    reason: result.reason.clone(),
    ticket: result.ticket.clone(),
```

Do **not** touch `normalize_status` or the counter classification. The five counters are derived from `status` alone and none of the three new fields feeds them.

- [x] **Step 5: Run the test and the full suite**

Run: `cargo test -p qa-runs test_result_case_fields_are_optional_on_the_wire`
Expected: PASS.

Run: `cargo test -p qa-runs -p qa-runs-sdk`
Expected: all pass, count ≥ 778 + 1.

- [x] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-runs/qa-runs/src/
git commit -m "feat(qa-runs): carry nodeid/reason/ticket through qa.test.result and ingest"
```

---

### Task 4: qa-runs-sdk — the two read methods the reconciler needs

**STATUS: ✅ COMPLETE** — `0e18503f`. Verified `qa-runs-sdk/src/client.rs` — `list_runs_finished_since` (`:81`) and `list_run_test_results` (`:101`).

The reconciler (Task 16) must answer two questions the SDK cannot currently answer: *which runs finished since a watermark* and *what were that run's per-test rows*. `list_runs(ctx, limit)` returns newest-first with no time bound, and `get_run_result` returns only the five counters (`qa-runs-sdk/src/models.rs:369-375`) — neither is sufficient.

**Files:**
- Modify: `gears/qa-platform/qa-runs/qa-runs-sdk/src/client.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs-sdk/src/models.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/local_client/client.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/repos/` (the runs repository trait) and its `infra/storage/runs_sea_repo.rs` implementation
- Test: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/runs_tests.rs`

- [x] **Step 0: Verify against legacy**

There is no legacy counterpart — legacy's analytics reads the same database directly. Confirm that: `manager/src/routes/analytics.rs` queries `test_results` and `run_results` with `sqlx` against `state.db`, the same pool the run path writes. This method pair exists *because* the gear split forbids that, not because legacy had an API. Record that in the task report so nobody looks for a legacy citation later.

- [x] **Step 1: Write the failing service test**

```rust
/// The reconciler's sweep predicate: runs whose `finished_at` is at or after a
/// watermark, oldest first, bounded. Oldest-first matters — the reconciler
/// advances its watermark as it goes, and a newest-first page would strand the
/// oldest gap forever.
#[tokio::test]
async fn list_runs_finished_since_returns_oldest_first_within_the_limit() {
    let f = fixture().await;
    let older = f.finished_run_at("2026-08-18T10:00:00Z").await;
    let newer = f.finished_run_at("2026-08-18T12:00:00Z").await;
    let _before = f.finished_run_at("2026-08-18T08:00:00Z").await;

    let found = f
        .service
        .list_runs_finished_since(&f.ctx, datetime!(2026-08-18 09:00:00 UTC), 10)
        .await
        .expect("sweep succeeds");

    assert_eq!(
        found.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![older, newer],
        "the pre-watermark run must be excluded and the order must be ascending"
    );
}
```

Use the fixture helpers already in `runs_tests.rs`; if no `finished_run_at` helper exists, add one next to the existing run-construction helpers rather than inlining setup in the test.

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-runs list_runs_finished_since_returns_oldest_first_within_the_limit`
Expected: FAIL — no method `list_runs_finished_since`.

- [x] **Step 3: Add the SDK model**

In `qa-runs-sdk/src/models.rs`, add the per-test row as a contract type. It has no `serde` derives — the SDK crate is deliberately `serde`-free (`qa-runs-sdk/src/lib.rs:4-7`).

```rust
/// One per-test row as qa-runs recorded it.
///
/// This is the **authoritative** copy. `qa-insights` builds its analytical
/// tables from the event stream and uses this type only to backfill gaps the
/// broker dropped (`docs/plans/2026-08-18-qa-insights-gear.md`, Task 16), which
/// is why it is a read-only projection with no constructor and no `New` twin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunTestResult {
    pub run_id: Uuid,
    pub test_file: String,
    pub test_name: String,
    pub status: String,
    pub duration: Option<String>,
    pub launch_id: Option<String>,
    pub jira_key: Option<String>,
    pub nodeid: String,
    pub reason: Option<String>,
    pub ticket: Option<String>,
}
```

- [x] **Step 4: Add the three trait methods**

In `qa-runs-sdk/src/client.rs`, on `QaRunsClientV1`:

```rust
    /// Runs finished at or after `since`, **oldest first**, capped at `limit`.
    ///
    /// Oldest-first is load-bearing for the only caller: qa-insights' reconciler
    /// advances a watermark as it consumes the page, so a newest-first page
    /// would let it skip past a gap it never filled. `limit` is mandatory for
    /// the same reason `list_runs`' is.
    async fn list_runs_finished_since(
        &self,
        ctx: &SecurityContext,
        since: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Run>, QaRunsError>;

    /// Every per-test row qa-runs holds for one run.
    ///
    /// Unbounded by design: the row count is bounded by the run's own test
    /// count, which the launch path already caps. Callers that want a page want
    /// the REST collection instead.
    async fn list_run_test_results(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Vec<RunTestResult>, QaRunsError>;

```

**Two methods, not three.** An earlier draft of this task added a third, `update_schedule_notifications`, and told you to "let it fail to compile until Task 6" defines `ScheduleNotificationSettings`. That was wrong and is corrected here (2026-08-18): every task in this plan ends on a green gate, and a task that knowingly leaves the workspace unbuildable makes the next task's gate meaningless — you could not tell a new breakage from the inherited one. The third method moves to **Task 6**, which defines the type it needs and can therefore ship it green in one commit.

### Task 4 header note

Both methods here exist for one consumer, qa-insights' reconciler (Task 15). Neither has a REST counterpart and neither should acquire one: the reconciler is a gear-to-gear backfill path, and exposing "give me every per-test row for this run" over HTTP would duplicate Task 17's OData collection with none of its paging.

- [x] **Step 5: Implement down the stack**

Add the repository methods (trait + `runs_sea_repo.rs`), the service methods, and the `QaRunsLocalClient` delegations. Follow `list_runs`' existing implementation exactly for scope handling — the same `AccessScope` derivation, the same `SecureSelectExt` usage. The sweep's `WHERE` clause is `finished_at >= $since AND finished_at IS NOT NULL`, ordered `finished_at ASC, id ASC`. The tiebreak on `id` is not decorative: two runs finishing inside the same clock tick would otherwise page nondeterministically and the watermark could skip one.

- [x] **Step 6: Run the test and the full suite**

Run: `cargo test -p qa-runs list_runs_finished_since_returns_oldest_first_within_the_limit`
Expected: PASS.

Run: `cargo test -p qa-runs -p qa-runs-sdk`
Expected: all pass.

- [x] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-runs/
git commit -m "feat(qa-runs-sdk): watermark sweep and per-test result read for the insights reconciler"
```

---

### Task 5: qa-runs — the `Collect` run kind

**STATUS: ✅ COMPLETE** — `0e18503f`. Verified `RunTarget::Collect { repo_id, collect_url }` and `RunKind::Collect` in `qa-runs-sdk/src/models.rs`; `COLLECT_ONLY`/`VHP_COLLECT_URL` in `env_assembly.rs`, `dispatch_spec.rs`, `dto.rs`.

**Files:**
- Modify: `gears/qa-platform/qa-runs/qa-runs-sdk/src/models.rs` (the run-kind enum)
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/launch.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/env_assembly.rs`
- Test: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/launch_tests.rs`

- [x] **Step 0: Verify against legacy**

Three things to confirm, all cited in D2:

1. `manager/src/services/argo.rs:50-57` — a collect-only submission pushes exactly two environment variables, `COLLECT_ONLY=true` and `VHP_COLLECT_URL=<url>`, and pushes `VHP_COLLECT_URL` only when a URL was supplied.
2. `manager/src/services/collect.rs:90` — the URL is built by the control plane: `format!("{}/api/collect/{}/{}", base, repo.id, branch)`. The runner posts wherever it is told, which is what lets qa-insights own the route.
3. `manager/src/services/argo.rs:369-372` — collect bypasses admission and passes `exclusive: false` explicitly. **Read the whole comment**: it also claims `jira_poller` bypasses admission, and that half is stale — `manager/src/services/jira_poller.rs:8-15` says the opposite, citing VHP-2618. Do not reproduce the jira_poller bypass (D8, Finding 8).

Also confirm from `manager/src/services/collect.rs:42-48` that the collect submission drops repositories that lack the branch, rather than failing the cycle. (`:56-70` is a different skip — it drops *files* absent on the branch from the bundle. Two skips, two levels; do not conflate them.)

- [x] **Step 1: Write the failing test**

```rust
/// A collect launch never enters the queue. Legacy submits it directly and
/// passes `exclusive: false` explicitly (`manager/src/services/argo.rs:369-372`);
/// admitting it would let a busy platform starve the hourly cycle that keeps
/// analytics' expected-case numbers current.
#[tokio::test]
async fn a_collect_launch_bypasses_admission_and_is_never_exclusive() {
    let f = fixture().await;
    let outcome = f
        .service
        .launch(&f.ctx, collect_request(f.repo_id, "main", "https://insights.example/qa/v1/collect/r/main"))
        .await
        .expect("collect launches");

    let LaunchOutcome::Started { run } = outcome else {
        panic!("a collect launch must start immediately, never queue");
    };
    assert!(!run.resolved_exclusive, "collect is always parallel");
    assert_eq!(f.queue_depth().await, 0, "nothing may be enqueued");
}

/// The two variables are the whole runner-facing contract, and their names are
/// frozen (`cpt-cf-qa-fr-migration-runner-contract`).
#[test]
fn collect_assembly_sets_exactly_the_two_legacy_variables() {
    let env = assemble_collect_env("https://insights.example/qa/v1/collect/r/main");
    assert_eq!(env.get("COLLECT_ONLY").map(String::as_str), Some("true"));
    assert_eq!(
        env.get("VHP_COLLECT_URL").map(String::as_str),
        Some("https://insights.example/qa/v1/collect/r/main")
    );
}
```

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test -p qa-runs collect`
Expected: FAIL — no `RunKind::Collect`, no `assemble_collect_env`.

- [x] **Step 3: Add the variant and the assembly**

Add `Collect` to the run-kind enum in `qa-runs-sdk/src/models.rs`, with `as_str()` returning `"collect"` — the SDK's enums render through `as_str()` and that spelling is also the persisted one (`payloads.rs` module docs). Add the launch-request shape that carries `repo_id`, `branch` and `collect_url`.

In `env_assembly.rs`, add the collect branch. It sets the two variables and **nothing else** — no `TEST_FILES`, no `TEST_BUNDLE_URL` beyond what the bundle path already supplies, no platform variables tier. Legacy's collect submission goes through `RepoRunConfig` with `collect_only: true` and does not append run parameters.

In `launch.rs`, route `RunKind::Collect` past admission: no exclusivity resolution, no queue insert, `resolved_exclusive = false`, straight to dispatch. Add a comment naming `argo.rs:369-372` and stating that the stale jira_poller half of that comment is *not* being reproduced.

- [x] **Step 4: Run the tests**

Run: `cargo test -p qa-runs collect`
Expected: PASS, both.

Run: `cargo test -p qa-runs -p qa-runs-sdk`
Expected: all pass. Pay attention to the exclusivity and queue suites — a new enum variant that any `match` handles by falling into a catch-all arm is exactly the mistake this step can make. If a `match` on run kind compiles without you having touched it, find it and check the arm is right.

- [x] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-runs/
git commit -m "feat(qa-runs): Collect run kind with the frozen COLLECT_ONLY/VHP_COLLECT_URL contract"
```

---

### Task 6: qa-runs — per-schedule notification settings

**STATUS: ✅ COMPLETE** — `0e18503f`. Verified `m20260818_000007_schedule_notifications.rs` adds the three columns; `PUT /qa/v1/schedules/{id}/notifications` registered at `api/rest/routes/mod.rs:181`.

**Files:**
- Create: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/migrations/m20260818_000006_schedule_notifications.rs`
- Modify: `migrations/mod.rs`, `entity/schedule.rs`, `qa-runs-sdk/src/models.rs`, the schedules service, and `api/rest/routes` + `handlers`
- Test: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/schedules_tests.rs`

- [x] **Step 0: Verify against legacy**

Open `manager/src/routes/schedules.rs::api_update_notifications`. Confirm the three fields — `slack_notifications_enabled`, `slack_channel`, `slack_notification_events` — and confirm the handler round-trips them by deleting and recreating the CronWorkflow, carrying every other field forward (read the `exclusive` comment: *"this endpoint edits Slack settings only, so a schedule pinned to exclusive (or to parallel) must come back pinned the same way"*).

Then confirm the delete-and-recreate is an artifact of annotation storage, not a behavior: qa-runs has a real `qa_schedules` row, so the port is a plain `UPDATE`. Record that reasoning — a reviewer who knows legacy will ask why the recreate is missing.

- [x] **Step 1: Write the failing test**

```rust
/// Editing notification settings must not disturb any other field — legacy is
/// explicit that a schedule pinned exclusive comes back pinned exclusive
/// (`manager/src/routes/schedules.rs`, the `exclusive` comment in
/// `api_update_notifications`).
#[tokio::test]
async fn updating_notification_settings_leaves_every_other_field_untouched() {
    let f = fixture().await;
    let before = f.create_schedule_exclusive_pinned().await;

    let after = f
        .service
        .update_schedule_notifications(
            &f.ctx,
            before.id,
            ScheduleNotificationSettings {
                slack_enabled: true,
                slack_channel: Some("#qa-alerts".to_owned()),
                slack_events: vec!["Failed".to_owned(), "Error".to_owned()],
            },
        )
        .await
        .expect("update succeeds");

    assert!(after.slack_notifications_enabled);
    assert_eq!(after.slack_channel.as_deref(), Some("#qa-alerts"));
    assert_eq!(after.exclusive_choice, before.exclusive_choice);
    assert_eq!(after.cron, before.cron);
    assert_eq!(after.enabled, before.enabled);
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-runs updating_notification_settings_leaves_every_other_field_untouched`
Expected: FAIL — no such method.

- [x] **Step 3: Write the migration**

Same three-backend shape as Task 2. Columns:

```sql
ALTER TABLE qa_schedules ADD COLUMN slack_notifications_enabled BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE qa_schedules ADD COLUMN slack_channel VARCHAR(255) NULL;
ALTER TABLE qa_schedules ADD COLUMN slack_notification_events JSONB NOT NULL DEFAULT '[]';
```

On MySQL, `JSONB` is `JSON` and a `JSON` column cannot carry a literal default — declare it `NOT NULL` and write `'[]'` from the entity's `ActiveModelBehavior`, or declare it nullable and treat `NULL` as empty in the mapper. Pick one, and say which in the file's module doc; do not leave both possible. The existing `include_tags`/`exclude_tags` columns on this table have the same problem and already solved it — read `m20260813_000004_schedules.rs` and do whatever it did.

- [x] **Step 4: Entity, SDK model, service, REST**

Add the three fields to `entity/schedule.rs` and to the SDK `Schedule` model. Define:

```rust
/// The three per-schedule Slack settings, ported from legacy's CronWorkflow
/// annotations (`manager/src/routes/schedules.rs::api_update_notifications`).
///
/// qa-insights reads these over the SDK when a scheduled run changes status; it
/// does not own them, because a schedule is a qa-runs aggregate and a
/// notification setting on it is a field, not a separate entity.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ScheduleNotificationSettings {
    pub slack_enabled: bool,
    pub slack_channel: Option<String>,
    /// Legacy's event vocabulary, **as serialized**: `pending`, `in_progress`,
    /// `succeeded`, `failed`, `error`, `skipped`.
    ///
    /// Corrected 2026-08-18: an earlier draft of this plan listed the Rust
    /// *variant* names (`InProgress` and friends). That is not what goes on the
    /// wire — `ScheduledRunNotificationEvent` carries
    /// `#[serde(rename_all = "snake_case")]` (`manager/src/models.rs:949-959`),
    /// legacy pins it with its own test
    /// (`scheduled_run_event_serializes_to_snake_case`, `:940-946`), and the UI
    /// declares the same six lowercase tokens
    /// (`manager-ui/src/api/types.ts:761-767`). The difference is load-bearing,
    /// not cosmetic: `InProgress` lower-cases to `inprogress`, which legacy's
    /// own parser rejects, so a settings row written the old way would be a
    /// subscription that never fires.
    ///
    /// Stored as strings rather than an enum because the SDK is `serde`-free
    /// and the column is JSON; the routing core in qa-insights parses them
    /// (Task 36) against `SLACK_NOTIFICATION_EVENTS`, the closed set both gears
    /// read.
    pub slack_events: Vec<String>,
}
```

Add the SDK method that Task 4 deliberately deferred to here, because this is the task that defines its argument type:

```rust
    /// Update the three notification settings on a schedule (D9).
    ///
    /// Deferred from Task 4 so that both the method and
    /// [`ScheduleNotificationSettings`] land in one green commit — Task 4 would
    /// otherwise have had to ship a knowingly unbuildable workspace.
    async fn update_schedule_notifications(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        settings: ScheduleNotificationSettings,
    ) -> Result<Schedule, QaRunsError>;
```

Register `PUT /qa/v1/schedules/{id}/notifications` in the schedules routes file, following the `OperationBuilder` shape of a sibling mutating route.

**PUT, though legacy is POST** (`axum::routing::post`, `routes/mod.rs:95-98`) — a deliberate adaptation, corrected 2026-08-18 after this plan briefly claimed the opposite. The frozen contract is the *test-facing* one: environment variable names, `plan.yaml`, TEST_META, per `cpt-cf-qa-fr-migration-runner-contract`. The REST surface is explicitly not frozen, and this route already changes its prefix (`/api` → `/qa/v1`) and its key (`{name}` → `{id}`), so "the SPA calls it POST" was never true of the gear. A full, idempotent replacement of a settings sub-resource is PUT, and that matches the sibling gears. PRD and DESIGN record the same reasoning.

- [x] **Step 5: Run the tests**

Run: `cargo test -p qa-runs updating_notification_settings_leaves_every_other_field_untouched`
Expected: PASS.

Run: `cargo test -p qa-runs -p qa-runs-sdk`
Expected: all pass.

- [x] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-runs/
git commit -m "feat(qa-runs): per-schedule Slack notification settings"
```

---

### Task 7: qa-catalog — the analytics universe and static case counts

**STATUS: ✅ COMPLETE** — `0e18503f`. Verified `qa-catalog-sdk` — `UniverseTest` (`models.rs:364`) with `static_case_count: u32` (`:431`); `list_universe` on the client (`:134`).

Analytics' list items carry `component`, `tags`, `plan_id`, `plan_name` and `versions`, and its quality-vector summary is computed over TEST_META `quality_vectors` (`manager/src/routes/analytics.rs:112-134`, `:217-229`). Legacy reads those off the checkout. qa-insights has no checkout, so qa-catalog projects them.

**Files:**
- Create: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/parsing/case_count.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog-sdk/src/models.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog-sdk/src/client.rs`
- Modify: the catalog plans service + local client
- Test: `case_count.rs` in-file module

- [x] **Step 0: Verify against legacy**

Open `manager/src/routes/analytics.rs:1865` (`count_test_functions`) and read the whole function, its doc comment at `:1859-1864`, and its call site at `:1882`.

**It counts two ecosystems, not one.** This was missed in an earlier draft of this plan and corrected on 2026-08-18 after the Task 1 citation audit; a counter that handles only pytest returns **zero** for every Playwright spec file, and `case_expected` silently under-reports for any repository that has them. The function is two regexes summed:

```rust
let pytest_cases = Regex::new(r#"(?m)^\s*(?:async\s+)?def\s+test\w*\s*\("#)
    .map(|re| re.find_iter(content).count()).unwrap_or(0);
let playwright_cases = Regex::new(r#"(?m)^\s*test(?:\.(?:only|skip|fixme|fail))?\s*\(\s*["'`]"#)
    .map(|re| re.find_iter(content).count()).unwrap_or(0);
pytest_cases + playwright_cases
```

Write down in the task report:

* **pytest**: `def test_*` and `async def test_*`, at any indentation (so methods count), matched at line start after optional whitespace;
* **Playwright**: `test(`, `test.only(`, `test.skip(`, `test.fixme(`, `test.fail(` — but **not** `test.describe(`, which is a suite and would double-count;
* that the Playwright regex requires a **quote** immediately after the open paren (`["'\`]`), so `test(myVar)` does not count;
* that it does **not** expand `@pytest.mark.parametrize` (doc comment `:1862-1864`), which is exactly why the static number and the exact collect number differ;
* that a failed regex compile yields `0` rather than an error — a blind spot, and one to port rather than fix.

**Port the regexes verbatim, including their blind spots.** A "corrected" counter produces different numbers than the system it replaces, which is the one outcome this task must avoid.

- [x] **Step 1: Write the failing tests from what you found**

One test per behavior confirmed in Step 0, covering **both** ecosystems.

```rust
/// Ported from `manager/src/routes/analytics.rs:1865` (`count_test_functions`),
/// which feeds `OverviewSummary::case_expected`. Parametrize is deliberately
/// **not** expanded (`:1862-1864`); the exact count comes from the collect job
/// instead (Task 30).
#[test]
fn counts_module_level_methods_and_async_pytest_functions() {
    let source = "\
import pytest

def test_alpha():
    pass

async def test_async():
    pass

def helper():
    pass

class TestGroup:
    def test_beta(self):
        pass
";
    assert_eq!(count_test_functions(source), 3, "two module-level (one async) plus one method");
}

/// Playwright specs count too. A pytest-only counter returns 0 here, and
/// `case_expected` silently under-reports for every `*.spec.ts` file in the
/// universe.
#[test]
fn counts_playwright_specs_including_modifier_variants() {
    let source = "\
import { test, expect } from '@playwright/test';

test('logs in', async ({ page }) => {});
test.only('focused', async ({ page }) => {});
test.skip('skipped', async ({ page }) => {});
test.fixme('broken', async ({ page }) => {});
test.fail('expected to fail', async ({ page }) => {});
";
    assert_eq!(count_test_functions(source), 5);
}

/// `test.describe(` is a **suite**, not a case. The legacy regex excludes it by
/// enumerating only `only|skip|fixme|fail`, and counting it would inflate every
/// Playwright file by its suite count.
#[test]
fn a_playwright_describe_block_is_not_a_case() {
    let source = "\
test.describe('suite', () => {
  test('a case', async () => {});
});
";
    assert_eq!(count_test_functions(source), 1, "the describe wrapper must not count");
}

/// The Playwright regex requires a quote straight after the open paren, so a
/// call whose title is a variable does not count. Blind spot, ported as-is.
#[test]
fn a_playwright_call_without_a_literal_title_does_not_count() {
    assert_eq!(count_test_functions("test(myTitle, async () => {});\n"), 0);
}
```

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test -p qa-catalog count_test_functions`
Expected: FAIL — function not found.

- [x] **Step 3: Port the counter**

Put it in `domain/parsing/case_count.rs` next to the existing TEST_META parser, since it consumes the same file content on the same walk. Match legacy's algorithm, not a better one. Document the parametrize blind spot in the function's doc comment with the legacy citation, so the next reader does not "fix" it.

- [x] **Step 4: Add the SDK projection**

```rust
/// One test file as the analytics universe sees it.
///
/// This is a **projection for qa-insights**, not a catalog concept: it exists
/// because analytics needs `component`/`tags`/`quality_vectors` and an expected
/// case count per file, and the gear split puts the repository content on this
/// side of the boundary (`DESIGN.md` §3.4, the qa-catalog row added for
/// qa-insights). Legacy read all of it off the checkout directly
/// (`manager/src/routes/analytics.rs:250-265`, `UniverseTest`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniverseTest {
    // CORRECTED 2026-08-18 (Task 7). An earlier draft of this list was wrong in
    // four ways, all found by verifying against legacy's own struct
    // (`manager/src/routes/analytics.rs:251-264`), which has ELEVEN fields:
    //
    //  1. `plan_id: Uuid` is unimplementable — plans are not persisted in this
    //     port (`Plan` is materialized on read; there is no plans table). And
    //     legacy's `plan_id` is not a UUID either: it is a path-derived slug
    //     (`manager/src/services/plans.rs:789-801`). `plan_path` is closer to
    //     legacy's meaning than a synthetic id would be.
    //  2. `source` values `plan`/`custom_plan`/`git_plan` were wrong — those are
    //     `RunIntent::run_kind()` values (`manager/src/models.rs:1601`), a
    //     different enum. A plan's `source` is `"repo"` or `"local"`.
    //  3. `quality_vectors` is NOT on legacy's struct; legacy folds it into a
    //     separate `quality_vectors_by_file` map (`analytics.rs:839`,
    //     `:871-882`). Keeping it on the row is the right call for the gear
    //     split — insights has no checkout — but it is an addition, not a port.
    //  4. `versions` is DEAD in legacy: written `Vec::new()` at `:918` and never
    //     touched. Kept so the response shape matches; populating it would be a
    //     behaviour change, not a fix.
    pub repo_id: Uuid,
    /// The plan's path, not an id. See correction 1 above.
    pub plan_path: String,
    pub plan_name: String,
    pub test_file: String,
    pub test_name: String,
    /// The TEST_META title, when the file declares one. **Not decorative**: it
    /// is one of the four alias sources `build_alias_map:1714` registers, so an
    /// execution row that names a test by its human title still matches its
    /// file. Omitting this field silently turns those rows into `not_run`.
    pub title_alias: Option<String>,
    pub component: Option<String>,
    pub tags: Vec<String>,
    /// An addition, not a port — see correction 3.
    pub quality_vectors: Vec<String>,
    /// `"repo"` or `"local"` — see correction 2. `SOURCE_REPO` is exported.
    pub source: String,
    /// Always empty; legacy never populates it — see correction 4.
    pub versions: Vec<String>,
    /// `count_test_functions` over the file's content, counting **both** pytest
    /// `def test_*` and Playwright `test(...)` specs. Parametrize is not
    /// expanded; the exact count lives in qa-insights' `test_case_collect`.
    pub static_case_count: u32,
}
```

And the trait method:

```rust
    /// Every test file reachable from the given product's plans on `branch`,
    /// with its TEST_META attributes and static case count.
    ///
    /// One call, not one per plan: analytics computes a whole-universe summary
    /// and an N+1 across the SDK boundary would make the overview endpoint's
    /// latency a function of plan count.
    async fn list_universe(
        &self,
        ctx: &SecurityContext,
        product_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<Vec<UniverseTest>, QaCatalogError>;
```

- [x] **Step 5: Implement it over the existing plan walk**

The catalog already walks plans and parses TEST_META. Add the projection to that walk rather than a second traversal — a second walk would double the git-checkout I/O on the hottest analytics call.

- [x] **Step 6: Run the tests**

Run: `cargo test -p qa-catalog count_test_functions`
Expected: PASS.

Run: `cargo test -p qa-catalog -p qa-catalog-sdk`
Expected: all pass.

- [x] **Step 7: Phase 0 gate — run every affected suite**

```bash
cargo fmt --all -- --check
cargo clippy -p qa-runs -p qa-runs-sdk -p qa-catalog -p qa-catalog-sdk --all-targets -- -D warnings
cargo test -p qa-runs -p qa-runs-sdk -p qa-catalog -p qa-catalog-sdk
cargo build --workspace
```

Expected: green, with the qa-runs count at or above 778 plus this phase's additions.

- [x] **Step 8: Commit and squash the phase**

```bash
git add gears/qa-platform/qa-catalog/
git commit -m "feat(qa-catalog): analytics universe projection and static case counts"
```

Then squash Tasks 1–7 into one commit titled
`feat(qa-platform): qa-insights prerequisites — case fidelity, collect kind, catalog universe`.

---

# Phase A — Foundation

Ships a gear that ingests results transactionally, heals its own gaps, and answers the dashboard and coverage pages.

### Task 8: The SDK crate

**STATUS: ✅ COMPLETE** — `d0eb4ac0` + `d7fc9f5a`. Verified `qa-insights-sdk/src/{lib,client,models,errors}.rs`; 26 contract types.

**Files:**
- Create: `gears/qa-platform/qa-insights/qa-insights-sdk/{Cargo.toml,src/lib.rs,src/client.rs,src/models.rs,src/errors.rs}`
- Modify: `Cargo.toml` (workspace members)

- [x] **Step 1: Copy the sibling's shape**

Read `qa-environments/qa-environments-sdk/` in full — all four source files and the manifest. It is the smallest of the three shipped SDKs and the one to model. Note in particular that the crate is `serde`-free by policy and that `de0101_no_serde_in_contract` is skipped in `Gears.toml`'s dylint list rather than satisfied (`qa-runs-sdk/src/lib.rs:4-7`); do not add `serde` derives to contract types here either.

- [x] **Step 2: Write the crate doc and the error type**

`lib.rs` states the gear's purpose in two sentences and links `DESIGN.md` §3.2 `cpt-cf-qa-component-insights`. `errors.rs` defines `QaInsightsError` mirroring `QaEnvironmentsError`'s variant set — copy its shape, then delete the variants that have no counterpart here and add `IngestConflict` and `UnsupportedEgress`.

- [x] **Step 3: Define the models**

`models.rs` carries: `TestResultRecord`, `TestCaseResultRecord`, `CollectCount`, `SavedView`, `NewSavedView`, `JiraBug`, `NewJiraBug`, `JiraConfig`, `JiraPollerConfig`, `NotificationConfig`, `NotificationLogEntry`, `SkipListEntry`, `DashboardStats`, `CoverageBuild`. Field-for-field from the legacy structs cited in the analytics/notification tasks below — write the citation in each type's doc comment as you go, not afterwards.

- [x] **Step 4: Define the client trait**

`QaInsightsClientV1` carries only what **other gears** need, which turns out to be exactly one method: `skip_list_for(ctx, plan_id) -> Vec<SkipListEntry>`, which qa-runs calls at launch when skip-tests-with-bugs is requested.

Everything else on this gear is REST-only, including the collect-report ingest — the runner posts it over HTTP to `VHP_COLLECT_URL`, so it is a REST route (Task 30), not an SDK method. Resist adding the analytics surface here: an SDK method with no cross-gear caller is pure overhead, which is the exact argument `cpt-cf-qa-adr-four-gear-decomposition` uses against its six-gear option.

- [x] **Step 5: Register in the workspace and build**

Run: `cargo build -p qa-insights-sdk`
Expected: compiles clean.

Run: `cargo clippy -p qa-insights-sdk --all-targets -- -D warnings`
Expected: no warnings.

- [x] **Step 6: Commit**

```bash
git add Cargo.toml gears/qa-platform/qa-insights/qa-insights-sdk/
git commit -m "feat(qa-insights-sdk): contract crate skeleton"
```

---

### Task 9: The gear skeleton

**STATUS: ✅ COMPLETE** — `da39915e`. Verified `qa-insights/src/{lib,gear,config}.rs` + `domain/error.rs`; gear boots in the example server.

**Files:**
- Create: `gears/qa-platform/qa-insights/qa-insights/{Cargo.toml,src/lib.rs,src/gear.rs,src/config.rs,src/domain/error.rs,src/domain/mod.rs,src/infra/mod.rs,src/api/mod.rs}`
- Modify: `Cargo.toml`, `Gears.toml`

- [x] **Step 1: Write the manifest**

Model on `qa-runs/qa-runs/Cargo.toml`, which is the closest in dependency shape — it is the one with `event-broker-sdk` and `cluster-sdk`. Dependencies: `toolkit`, `toolkit-db`, `toolkit-db-macros`, `toolkit-security`, `toolkit-macros`, `authz_resolver_sdk`, `event-broker-sdk`, `cluster-sdk`, `oagw-sdk`, `qa-runs-sdk` and `qa-catalog-sdk` by relative path (siblings go by path — neither has a `[workspace.dependencies]` entry, so `{ workspace = true }` would not resolve), `sea-orm`, `sea-orm-migration`, `axum`, `http`, `async-trait`, `serde`, `serde_json`, `time`, `uuid`, `thiserror`, `tracing`, `tokio`, `tokio-util`, `csv` (for the export in Task 26).

Add the `integration` feature gating `testcontainers`/`testcontainers-modules`, copying the comment block from `qa-runs/qa-runs/Cargo.toml` that explains why they are optional `[dependencies]` and not `[dev-dependencies]`. That comment is load-bearing for `cargo-shear` and re-deriving it wastes an afternoon.

- [x] **Step 2: Write `config.rs`**

```rust
//! Typed configuration for the `qa-insights` gear.

use serde::Deserialize;

/// Typed configuration for the qa-insights gear (YAML section `qa-insights`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QaInsightsConfig {
    /// How often the reconciler sweeps for runs the broker never delivered.
    /// Not a latency budget — the event stream is the fast path and this is the
    /// backstop — so minutes, not seconds.
    pub reconcile_interval_seconds: u64,
    /// How far back a sweep looks beyond its own watermark. Covers a run that
    /// finished before an earlier sweep's cutoff but was written after it.
    pub reconcile_lookback_seconds: u64,
    /// Rows per reconciler page.
    pub reconcile_page_size: u32,
    /// Legacy's `DEFAULT_COLLECT_BRANCH` (`manager/src/services/collect.rs:19`).
    /// Kept as `main` so the default expected-cases lookup lines up with what
    /// the hourly cycle writes.
    pub default_collect_branch: String,
    /// Legacy's hourly collect cycle (`manager/src/services/collect.rs:183-188`).
    pub collect_interval_seconds: u64,
    /// Whether this instance runs the tickers at all. An operator running a
    /// read-only replica sets this false.
    pub enable_tickers: bool,
    /// Max rows any analytics query returns before paging.
    pub max_page_size: u32,
}

impl Default for QaInsightsConfig {
    fn default() -> Self {
        Self {
            reconcile_interval_seconds: 300,
            reconcile_lookback_seconds: 3600,
            reconcile_page_size: 200,
            default_collect_branch: "main".to_owned(),
            collect_interval_seconds: 3600,
            enable_tickers: true,
            max_page_size: 200,
        }
    }
}
```

- [x] **Step 3: Write `domain/error.rs`**

Copy `qa-environments/src/domain/error.rs`'s shape — `#[domain_model]`, `thiserror`, the `Database`/`Internal`/`Forbidden`/`Validation` tail. Add: `RunNotIngested { run_id: Uuid }`, `SavedViewNameExists { name: String }`, `JiraNotConfigured`, `BugNotFound { key: String }`, `UnsupportedEgress { channel: String }`, and `IngestConflict`.

**The last two are owed here, not in the SDK.** Task 8's plan text asked its `errors.rs` to add `IngestConflict` and `UnsupportedEgress` to a `QaInsightsError` enum. That turned out to be the wrong home: all three shipped sibling SDKs define `errors.rs` as exactly one line — `pub use toolkit_canonical_errors::CanonicalError as …` — so inventing an enum there would have been the divergence, not the parity. Task 8 correctly kept the re-export. The two variants therefore land here, in the domain error, or they vanish; this note exists so they do not.

- [x] **Step 4: Write `gear.rs`**

Model on `qa-environments/src/gear.rs` for the `Gear`/`DatabaseCapability`/`RestApiCapability` triple, and on `qa-runs/src/gear.rs` for the stateful `serve` lifecycle that hosts tickers. Declare:

```rust
#[toolkit::gear(
    name = "qa-insights",
    deps = [authz_resolver, qa_runs, qa_catalog, oagw, event_broker, cluster],
    capabilities = [db, rest]
)]
```

At this task `init` wires only the database, authz and the config. The clients, the consumer and the tickers land in Tasks 13, 15 and 40. Leave a `// wired in Task N` comment at each gap rather than an empty `OnceLock` with no explanation.

- [x] **Step 5: Build**

Run: `cargo build -p qa-insights`
Expected: compiles clean.

- [x] **Step 6: Commit**

```bash
git add Cargo.toml Gears.toml gears/qa-platform/qa-insights/qa-insights/
git commit -m "feat(qa-insights): gear skeleton, config and domain errors"
```

---

### Task 10: The schema

**STATUS: ✅ COMPLETE** — `4631cd88`. Verified 11 tables / 18 indexes across three dialect blobs; 19 tests; verified created at boot and by a real Postgres container.

Eleven tables in one initial migration. Ten are legacy tables with tenancy and UUID keys added; one (`qa_ingest_watermarks`) is new and exists only because the broker has no durable backend.

**Files:**
- Create: `gears/qa-platform/qa-insights/qa-insights/src/infra/storage/migrations/{mod.rs,m20260818_000001_initial.rs}`
- Create: `gears/qa-platform/qa-insights/qa-insights/src/infra/storage/{mod.rs,db.rs}`

- [x] **Step 0: Verify against legacy**

Open `manager/migrations/001_initial.sql` and read every one of these definitions in full, including the comments above them:

| Line | Table |
|---|---|
| `:65` | `test_results` |
| `:78` | `jira_bugs` |
| `:183` | `analytics_saved_views` (+ the unique index at `:194`) |
| `:197` | `run_notifications` |
| `:205` | `notification_log` |
| `:253` | `test_case_results` |
| `:272` | `test_case_collect` |

For each, write down in the task report: the column set, the nullability, the indexes, and anything the comment says about *why*. Two specifically:

* `analytics_saved_views`' unique index is on `(owner_id, scope, COALESCE(plan_id, ''), name)` — the `COALESCE` is what makes a global view and a plan-scoped view of the same name coexist. Reproduce that semantics, not the literal SQL (D4).
* `run_notifications`' primary key is the composite `(workflow_name, notification_kind, event_type)` — it *is* the dedupe mechanism, not an incidental key (D5).

- [x] **Step 1: Write the failing schema test**

One test asserting the full table inventory, one asserting the index inventory. Copy the helpers from `qa-runs`' `m20260813_000003_initial.rs` (`index_names`, and the table-listing helper next to it) rather than writing new ones.

```rust
#[tokio::test]
async fn the_initial_migration_creates_every_table() {
    let conn = fresh_sqlite_with_migrations().await;
    let mut tables = table_names(&conn).await;
    tables.sort();
    assert_eq!(
        tables,
        vec![
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
        ]
    );
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-insights the_initial_migration_creates_every_table`
Expected: FAIL — no migrations.

- [x] **Step 3: Write the migration**

Three backends, raw SQL, the shape of `m20260813_000003_initial.rs`. Every table carries `id UUID PRIMARY KEY`, `tenant_id UUID NOT NULL`, `created_at`, `updated_at`. Column notes that are decisions, not transcription:

* **`qa_test_results`** — `run_id`, `test_file VARCHAR(1024) NOT NULL DEFAULT ''`, `test_name VARCHAR(512) NOT NULL`, `status VARCHAR(16) NOT NULL`, `duration VARCHAR(64) NULL`, `launch_id VARCHAR(255) NULL`, `jira_key VARCHAR(64) NULL`, `product_version VARCHAR(255) NULL`, `platform_id UUID NULL`, `plan_id UUID NULL`, `branch VARCHAR(512) NULL`, `run_finished_at TIMESTAMPTZ NULL`. The last five (`product_version`, `platform_id`, `plan_id`, `branch`, `run_finished_at`) are **denormalized from the run** on ingest. They are not in legacy's `test_results` because legacy joins `run_results` in every query; here the join would be a cross-gear call, which `cpt-cf-qa-principle-async-insights` forbids on any hot path. Denormalizing is the adaptation the gear split forces, and it is safe because a finished run's platform and version never change. Index: `(tenant_id, run_id)`, `(tenant_id, test_file, test_name)`, `(tenant_id, run_finished_at DESC)`.
* **`qa_test_case_results`** — `run_id`, `test_file`, `nodeid VARCHAR(1024) NOT NULL DEFAULT ''`, `name VARCHAR(512) NOT NULL`, `status VARCHAR(16) NOT NULL`, `duration VARCHAR(64) NULL`, `reason TEXT NULL`, `ticket VARCHAR(64) NULL`. Index: `(tenant_id, run_id)`, `(tenant_id, run_id, test_file)`, `(tenant_id, status)`. Mirrors legacy's three indexes at `:265-267`.
* **`qa_test_case_collect`** — `repo_id UUID`, `branch VARCHAR(512)`, `test_file VARCHAR(1024)`, `case_count INTEGER NOT NULL DEFAULT 0`, `collected_at TIMESTAMPTZ NOT NULL`. **Unique** on `(tenant_id, repo_id, branch, test_file)` — legacy's composite primary key becomes a unique index because the platform's standard `id UUID` primary key is non-negotiable. The upsert targets that unique index.
* **`qa_analytics_saved_views`** — `owner_id UUID NOT NULL`, `scope VARCHAR(64) NOT NULL`, `plan_id UUID NULL`, `name VARCHAR(255) NOT NULL`, `query_json JSONB NOT NULL`. Unique on `(tenant_id, owner_id, scope, plan_id_key, name)` where `plan_id_key` is a generated `VARCHAR(36) NOT NULL DEFAULT ''` mirroring legacy's `COALESCE(plan_id, '')` — SQLite and MySQL do not both support functional indexes, so the coalesced value is materialized as its own column and written by the repository. Say that in the column comment.
* **`qa_jira_bugs`** — `jira_key VARCHAR(64) NOT NULL`, `test_name VARCHAR(512) NOT NULL`, `repo_id UUID NOT NULL`, `plan_path VARCHAR(1024) NOT NULL`, `app_version VARCHAR(255) NULL`, `platform_id UUID NULL`, `status VARCHAR(64) NOT NULL DEFAULT 'Open'`, `summary TEXT NOT NULL`, `resolved_at TIMESTAMPTZ NULL`.

  **Two corrections, 2026-08-20.** An earlier draft said `plan_id UUID NULL` — there is no plan UUID in this port (Task 7), and legacy's own `jira_bugs.plan_id` is a path-derived TEXT slug (`001_initial.sql:82`, `plans.rs:789-801`), so the key is the `(repo_id, plan_path)` pair Task 8 shipped. And it said `platform VARCHAR(255)` while `qa_test_results` above says `platform_id UUID` — the same identity spelled two ways in one schema. Both are `platform_id UUID` here; legacy's `TEXT` name is an artifact of a system that had no platform ids until late (`platforms_meta.id` was added by a backfill, `001_initial.sql`'s Phase A block). Unique on `(tenant_id, jira_key)` (legacy: globally unique on `jira_key`); index on `(tenant_id, test_name, plan_id)` and `(tenant_id, status)`.

  **`auto_rerun` is deliberately NOT a column here** — corrected 2026-08-18 after the Task 1 citation audit. An earlier draft of this plan (and DESIGN §3.7's original one-line list) gave `jira_bugs` an `auto_rerun` flag. Legacy has no such column: its table stops at `resolved_at` (`001_initial.sql:78-89`), and auto-rerun is a **global** switch, `JiraPollerConfig.auto_rerun_on_resolve`, which Task 32's `qa_jira_poller_config` already carries. Adding a per-bug flag that nothing reads would be net-new design wearing a parity costume, and D8's whole point is that the rerun gate is the poller's, not the bug's. If per-bug control is ever wanted it is an additive nullable column plus an SDK field, not a redesign.
* **`qa_jira_config`**, **`qa_jira_poller_config`**, **`qa_notification_config`** — one row per tenant, enforced by a unique index on `(tenant_id)` alone. Fields exactly as the legacy config structs (`JiraConfig`, `JiraPollerConfig`, `NotificationsConfig` — fifteen fields — in `manager/src/models.rs`).

  **Two columns hold credential-equivalent material and neither may be stored in the clear.** `api_token` on `qa_jira_config` becomes `api_token_credstore_ref`, and `slack_webhook_url` on `qa_notification_config` becomes `slack_webhook_credstore_ref`. Legacy stores both verbatim; this is the one place the port deliberately diverges, because the platform has a credential store and writing a bearer secret into a gear table would fail review. A Slack incoming-webhook URL *is* the bearer secret — anyone holding it can post as the app — so it gets the same treatment as the JIRA token, and the decision is recorded here (2026-08-20) rather than left to Task 32. Note the divergence in both column comments, and make sure the GET surface returns the reference, never the material.
* **`qa_run_notifications`** — `run_id UUID NOT NULL`, `notification_kind VARCHAR(64) NOT NULL`, `event_type VARCHAR(64) NOT NULL`, `sent_at TIMESTAMPTZ NOT NULL`. Unique on `(tenant_id, run_id, notification_kind, event_type)`. This is the dedupe key; legacy keys on `workflow_name` (`001_initial.sql:197-203`) and the gear's equivalent identity is the run id.

  **This table has no SDK contract type, and that is a gap to close here.** Task 8's model list did not name one — the plan's omission, not Task 8's — but Tasks 36–38 build the whole notification idempotency story on it. Decide now whether the dedupe claim is expressed as a repository-only concern (no SDK type, `claim_notification` returning `bool`) or as a contract type, and record which. The repository-only shape is the recommendation: nothing outside this gear ever reads a dedupe row.
* **`qa_notification_log`** — `run_id UUID NULL`, `channel VARCHAR(32) NOT NULL`, `event_type VARCHAR(64) NOT NULL DEFAULT ''`, `outcome VARCHAR(32) NOT NULL`, `detail TEXT NOT NULL DEFAULT ''`. Index on `(tenant_id, created_at DESC)`, mirroring legacy's `ix_notification_log_created`.
* **`qa_ingest_watermarks`** — `last_reconciled_finished_at TIMESTAMPTZ NULL`, `last_swept_at TIMESTAMPTZ NULL`. Unique on `(tenant_id)`. New; see Task 15.

Add a module header explaining the denormalization decision and the `plan_id_key` decision, in the style of `m20260813_000003_initial.rs`'s header. Those are the two choices a future reader will otherwise mistake for sloppiness.

- [x] **Step 4: Run the tests**

Run: `cargo test -p qa-insights the_initial_migration`
Expected: PASS, both.

- [x] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/infra/
git commit -m "feat(qa-insights): initial schema"
```

---

### Task 11: Entities and repository traits

**STATUS: ✅ COMPLETE** — `8b3064d9`. Verified 11 entities (120 columns, round-tripped on SQLite + Postgres) and 6 repository traits (24 methods); 21 tests / 23 with `--features integration`.

**Files:**
- Create: `infra/storage/entity/*.rs` (eleven, one per table) and `entity/mod.rs`
- Create: `domain/repos/{mod.rs,results_repo.rs,collect_repo.rs,saved_views_repo.rs,jira_repo.rs,notify_repo.rs,watermark_repo.rs}`

- [x] **Step 1: Write the entities**

One file per table, each the shape of `qa-environments/src/infra/storage/entity/platform_variable.rs`: `DeriveEntityModel` + `Scopable` + `#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]`. The only entity that differs is `saved_view.rs`, which has an owner: use `owner_col = "owner_id"` instead of `no_owner`, because a saved view is genuinely owned and the unique index keys on the owner.

- [x] **Step 2: Write the repository traits**

Six traits, generic over `C: DBRunner` exactly as `qa-environments/src/domain/repos/variables_repo.rs` is. Method sets:

* `ResultsRepository` — `upsert_run_results` (delete-then-insert for one run, the whole point), `list_by_run`, `list_for_universe(filter) -> Vec<ExecRow>`, `ingested_run_ids_between(from, to)`, `latest_per_test(filter)`.

  `ExecRow` is this gear's name for legacy's row projection: one executed test result flattened with the run attributes analytics needs. Define it in `domain/analytics/mod.rs`, not in the repository module: it is the input type of the pure cores (Tasks 20–24), and the repository merely produces it.

  **Corrected 2026-08-20 by Task 11, which read both structs.** The field list above was wrong and the citation was incomplete. Legacy has *two* types, and this paragraph conflated them:

  * `ExecRowRaw` (`analytics.rs:321-331`) — the `sqlx` projection off `test_results JOIN run_results`. Nine fields, including `test_name` and a **nullable** `test_file`.
  * `ExecRow` (`:267-277`) — the normalized row, built from it at `:1029`. **Eight** fields: `test_file`, `status`, `build`, `platform`, `workflow_name`, `repo_id`, `ts`, `day`.

  Against the list that used to stand here: **`test_name` is not on `ExecRow`** (it is on `ExecRowRaw`, consumed by `resolve_row_test_file` at `:1750` and then dropped — analytics aggregates per *file*, and a core that grouped by test name would compute a different number with nothing failing); **`branch` is on neither** (it is a `WHERE` predicate, `:966-969`, so it belongs on the filter); and **`repo_id` is on `ExecRow` and was omitted** — though it is write-only, assigned at `:1036` and read nowhere, so it is deliberately not reproduced.

  Two consequences the plan should carry forward:

  1. **What the repository returns is the *raw* shape, not the normalized one.** Two of the three normalization steps — alias resolution and universe filtering — take the universe as input, and the universe comes from qa-catalog. So `ExecRow` here carries `test_name`, and Task 20 does the resolution exactly where legacy does. Legacy's post-resolution `ExecRow` becomes a core-internal type at Task 20.
  2. **`build` cannot be produced at all, and Task 24 needs it — `qa_test_results` has no `app_build` column.** Legacy's `ExecRow::build` is `r.app_build`. Task 10 denormalized `product_version` (legacy's `app_version`, the analytics *filter*) and **not** `app_build` (the analytics *projection*); `build_last_run_build_distribution` groups by the latter and `api_build_tests` filters on it. Task 11 left the field off `ExecRow` rather than shipping one that is always `None`, which would empty the build distribution silently on a screen that still renders. **Closing it is Task 12's job — see its Step 2.**

     (Corrected 2026-08-20 by the spec review. Task 11 first deferred this to Task 20's Step 0 and priced it at "a column plus an SDK field plus an ingest source". Both halves were wrong. The third term is **already done**: `qa_runs_sdk::Run::app_build` exists (`qa-runs-sdk/src/models.rs:316`) over a real `qa_runs.app_build VARCHAR(255) NULL` column (`m20260813_000003_initial.rs:219`), and this plan's own Task 13 already has ingest fetching the run object once per run and caching it for the batch (`:1624`) — `app_build` is simply another field on an object already in hand. And it is **not** entangled with open question 1: that question is about `product_id`/`version`/`scope` and the qa-catalog mapping VHP-319 left unresolved, whereas `app_build` is an opaque snapshot string qa-runs already denormalizes onto its own row. A settled question had been attached to an unsettled one.)
* `CollectRepository` — `upsert_count`, `list_counts_for(repo_ids: &[Uuid], branch)`.

  (Corrected 2026-08-20 by the code review. This line read `counts_for(repo_id, branch)`, and a single `repo_id` forces an N+1: legacy reads a branch's counts in **one** statement (`analytics.rs:2684`) and the analytics universe spans many repositories, so a per-repo signature issues N round trips to save reading rows the caller would have discarded. A slice avoids both — `WHERE tenant_id = ? AND branch = ? AND repo_id IN (…)` is one statement over the same index, returning exactly the rows the caller looks up, and `CollectCount` already carries `repo_id` so the caller still keys on `(repo_id, test_file)` as legacy does. Renamed to `list_*` to match the other six list-returning reads.)
* `SavedViewsRepository` — `list(scope, plan_id)`, `create`, `update`, `delete`, `find_by_natural_key`.
* `JiraRepository` — `list_open`, `list_open_for_plan`, `upsert_bug`, `resolve_bug`, `find_by_key`.
* `NotifyRepository` — `claim_notification(run_id, kind, event) -> bool`, `append_log`, `list_log`, `get_config`, `save_config`.
* `WatermarkRepository` — `get`, `advance`.

`claim_notification` returning `bool` rather than `()` is the dedupe contract: it inserts and reports whether *this* caller won. Two instances racing on the same run must produce exactly one send, and an "insert then check" pair cannot express that. Say so in the trait doc.

- [x] **Step 3: Build**

Run: `cargo build -p qa-insights`
Expected: compiles; the traits have no implementations yet, which is fine.

- [x] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/
git commit -m "feat(qa-insights): entities and repository traits"
```

---

### Task 12: SeaORM repository implementations

**STATUS: ✅ COMPLETE** — `ef878c22`, then `f16cc64a` (spec review), `45dafe9b` (code-quality review), `b6496f4a` (ordering-verification review). Verified 6 `Orm*Repository` implementations + `mapper.rs` (24 trait methods; 22 functions and 6 width constants), the `app_build` and `ingest_ordinal` columns across all three dialect blobs; **92** tests / **96** with `--features integration --lib`, 0 failures; `cargo fmt` and `cargo clippy -D warnings` clean with and without `--all-features`.

Three passes, and what each found — the pattern matters more than the count:

1. `ef878c22` — the six repositories, `mapper.rs`, and Step 0b's `app_build` column.
2. `f16cc64a` — **spec review.** Four defects, three of which the first pass had written into this plan and the trait docs *as corrections*: `SecureSelect` does support projection, `DISTINCT`, `GROUP BY` and subqueries (`project_all`, `libs/toolkit-db/src/secure/select.rs:396`), so two reads were materialising whole row sets on the tables `cpt-cf-qa-nfr-scale` targets 5M rows on; `MySQL` renders `do_nothing()` as the malformed `ON DUPLICATE KEY IGNORE`, not `INSERT IGNORE`; the first consumer of a case-row conversion is Task 17, not Task 21; plus the tiebreak divergence, escalated.
3. `45dafe9b` — **code-quality review.** One critical defect: `watermark_sea_repo::advance` poisoned a Postgres transaction on its own *supported no-op*, which is the exact hazard `notify_sea_repo`'s header documents and solves, reproduced one file over without the solution. Plus the tiebreak decision implemented (option A), and `mapper.rs`' complete absence of tests in a module whose header opens "Every decoder here fails closed" — a crate-wide grep for `CorruptState` had returned only the enum variant and doc comments.
4. `b6496f4a` — **ordering-verification review.** One correctness **regression introduced by `45dafe9b`**, and the most important thing in this record for whoever works on this next: implementing option A, I **dropped `created_at DESC` from the ordering the decision specified** and then asserted "reproduces legacy exactly" in five places. Legacy's `t.id` is a *global* SERIAL carrying two facts — ingest order across runs, parse order within one — and `ingest_ordinal` reproduces only the second. With two runs sharing a `run_finished_at`, the three-key ordering picked the *older-ingested* run's row whenever that run held the file at a higher batch position; the commit before it got that case right. Fixed by restoring the fourth key, with `a_tie_across_two_runs_is_won_by_the_later_ingested_one` — a case **no test covered**, which is why it shipped. Also fixed: two assertions this pass had introduced that could not fail (a multi-byte fixture whose truncation point landed *on* a codepoint boundary, and `is_char_boundary(s.len())`, which is true for every string), and five stale claims — three of them self-contradictions inside files that same commit had rewritten.

**Two recurring defect classes in this task, and both are worth knowing about before extending this gear.**

*The absence claim written from reading rather than from trying* — four times, three of them filed as corrections. Every such claim now carries the attempt that established it: a compile error, a rendered statement, or a mutation that stayed green. **Distrust any "X cannot be done" here that does not carry one.**

*The equivalence claim tested on one half of a two-part property* — which is what `45dafe9b`'s dropped ordering term was. The rule that catches it: **before writing that something reproduces legacy, construct the case that would distinguish them and run it.** Both places that now claim ordering equivalence state the property as two named halves with a test for each, precisely so the next reader can check them separately.

*And the sweep that both misses need:* when a question is closed, `grep` for every place that describes it as open. Three of the fourth pass's five stale claims were in files the third pass had just rewritten — re-reading what you changed is not enough, because the stale sentence is usually somewhere you did not touch.

Break-verification across the four passes: 63 mutations, **61 turn at least one named test red**. The two that turn none are recorded at the code with their measured reason — `collect_sea_repo::list_counts_for`'s empty-slice guard (`sea-query` 0.32 already renders an empty `is_in` as false) and `latest_per_test`'s SQL reduction (a *cost* property: removing it makes the answer identical, which is why it could ship as a fold). Eleven mutations that produced no red test during the passes exposed real coverage gaps, all closed and re-verified — the last two being the boundary guard in `truncate` and the cross-run ordering tie.

**Files:**
- Create: `infra/storage/{results_sea_repo.rs,collect_sea_repo.rs,saved_views_sea_repo.rs,jira_sea_repo.rs,notify_sea_repo.rs,watermark_sea_repo.rs,mapper.rs}`
- Modify (Step 0b, the `app_build` column): `infra/storage/migrations/m20260818_000001_initial.rs`, `infra/storage/entity/test_result.rs`, `domain/analytics/mod.rs`, `domain/repos/results_repo.rs`, `qa-insights-sdk/src/models.rs`
- Test: in-file `#[cfg(test)]` modules against in-memory SQLite

- [x] **Step 0: Verify against legacy**

Open `manager/src/routes/runs.rs:1153-1185`. Confirm the per-test dedupe is **delete-then-insert** on the four-column tuple, not an upsert against a unique index, and note that `test_file` is compared through `COALESCE(test_file, '')`. This gear normalizes to `''` on write (Task 10), so the predicate here is plain equality — record that difference and why it is safe.

**Verified 2026-08-20 by Task 12. The citation is exact — `:1153` is the `DELETE` and `:1185` the closing `?` of the `INSERT` — and the substance holds, with two corrections:**

1. **The tuple is *three* columns, not four:** `run_id = $1 AND test_name = $2 AND COALESCE(test_file, '') = $3` (`:1154`). There is no fourth. Legacy is single-tenant, so a reader expecting a `tenant_id` in the count will not find one; this gear's predicate adds the scope filter and is therefore genuinely four-way, which may be where the number came from.
2. **There is no unique index at all**, confirmed rather than inferred: `manager/migrations/001_initial.sql:65-76` declares `test_results` with only `idx_test_results_run` and `idx_test_results_name`, both non-unique, and `manager/migrations/` holds exactly one file. So the dedupe is entirely an application invariant on both sides.
3. **The method this task implements mirrors legacy's *per-run* writer, not this per-test one.** `manager/src/services/argo.rs:2593-2615` is `DELETE FROM test_results WHERE run_id = $1` followed by an unconditional insert of every parsed row, and `:2621-2640` does the same for `test_case_results` — which is exactly `upsert_run_results(run_id, files, cases)`. The per-test endpoint at `:1153-1185` is the live-progress path, which this gear does not have (D-series: progress arrives as a run-finished event, not per test). Both are delete-then-insert; the plan cited the narrower one.

Plain equality is safe because `qa_test_results.test_file` is `VARCHAR(1024) NOT NULL DEFAULT ''` (Task 10) and `qa_runs_sdk::RunTestResult::test_file` already collapsed absent to `''`, so "absent" has exactly one spelling on the write side and `COALESCE` has nothing left to normalise. Recorded in `results_sea_repo::upsert_run_results`' comment.

- [x] **Step 0b: Add the missing `app_build` column, before writing any mapper**

Do this before Step 1, because everything below writes the mapper that would otherwise be written and immediately rewritten.

`qa_test_results` has no `app_build`, and Task 24's build distribution cannot be computed without it (Task 11's Step 2 records the full finding). Ingest is the very next task, so if the column is not here, **every row Task 13 writes has no build and this gear acquires a backfill problem for a value that was available all along.**

Five edits, all additive:

1. `qa_test_results.app_build VARCHAR(255) NULL` in all three dialect blobs of `m20260818_000001_initial.rs`, placed next to `product_version`. Amending the initial migration rather than adding a new one is correct here **only because this gear is undeployed** — append-only has not bitten yet. The two dialect-parity tests cover the change for free; add a column comment saying it is denormalized from `qa_runs.app_build` and is the *projection* to `product_version`'s *filter*.
2. `pub app_build: Option<String>` on `entity/test_result.rs`.
3. The same field on `qa_insights_sdk::TestResultRecord` and on `domain::repos::NewTestResult`.
4. `pub build: Option<String>` on `domain::analytics::ExecRow`, with legacy's `"unknown"` fallback (`analytics.rs:1032-1033`) applied by Task 24's core, not by the repository — legacy normalizes at the consumer.
5. The mapper line, and the `list_for_universe` projection.

**Done 2026-08-20, and the five edits were seven.** Both `qa_runs` citations check out (`qa-runs-sdk/src/models.rs:316`, `m20260813_000003_initial.rs:219`), and legacy's own column is the `ALTER` at `001_initial.sql:149`, not a line of the `CREATE TABLE`. The two extra edits are the **counted claims the new column invalidated**, which the plan's list did not mention and which no test would have caught: the migration header's "### 1. **Six** run columns are copied onto every result row" (and the matching `qa_test_results` DDL comment, and `entity/test_result.rs`, and `domain/repos/results_repo.rs`) became *seven*; `entity/mod.rs`'s "**120** column names — 120 counted from the DDL" became 121; and `qa_insights_sdk::TestResultRecord`'s divergence 2, "**Five** denormalized columns", became six. The migration header's snapshot-safety argument now names `build` too, and cites the qa-runs section that already covered `app_build` by name.

- [x] **Step 1: Write the failing dedupe test**

(The snippet below passes five arguments; `ResultsRepository::upsert_run_results` takes **six** — Task 11 gave it `files` *and* `cases`, because a partial replacement leaves a run whose file-level and case-level counts disagree. The test as written is the six-argument form with an empty `cases`, plus `re_ingesting_a_run_replaces_its_case_rows_as_well` for the other half. `sqlite_with_migrations()` is `infra::storage::test_db::inmem_db()`, which returns a `toolkit_db::Db`: repository methods take `&impl DBRunner`, and `DbConn`/`DbTx` are its only two implementations because the trait is sealed inside `toolkit-db` — a repository test cannot be written against a raw `SeaORM` connection at all.)

```rust
/// Ingesting the same run's results twice must leave one row per
/// `(run_id, test_file, test_name)`. Legacy achieves this by deleting the run's
/// rows and re-inserting (`manager/src/routes/runs.rs:1153-1185`); this
/// reproduces the semantics, and it is what makes broker redelivery a no-op
/// rather than a double count.
#[tokio::test]
async fn upserting_a_runs_results_twice_leaves_one_row_per_test() {
    let db = sqlite_with_migrations().await;
    let repo = OrmResultsRepository;
    let run_id = Uuid::new_v4();

    let batch = vec![result_row(run_id, "tests/a.py", "test_a", "PASSED")];
    repo.upsert_run_results(&db, &scope(), TENANT, run_id, batch.clone()).await.unwrap();
    repo.upsert_run_results(&db, &scope(), TENANT, run_id, batch).await.unwrap();

    let rows = repo.list_by_run(&db, &scope(), run_id).await.unwrap();
    assert_eq!(rows.len(), 1, "redelivery must not duplicate: {rows:?}");
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-insights upserting_a_runs_results_twice_leaves_one_row_per_test`
Expected: FAIL — no `OrmResultsRepository`.

Observed: `error[E0432]: unresolved import ... no `OrmResultsRepository` in `infra::storage::results_sea_repo``.

- [x] **Step 3: Implement the six repositories**

Follow `qa-environments/src/infra/storage/variables_sea_repo.rs` for the `SecureSelectExt`/`SecureInsertExt` usage and the `AccessScope` threading — every query goes through the secure extension, never a bare `Entity::find()`. `mapper.rs` holds row↔SDK conversions, following `qa-runs`' `mapper.rs`, including its truncation helpers for the bounded `VARCHAR` columns.

`upsert_run_results` runs the delete and the inserts **inside the caller's transaction**, not its own — Task 14 commits the broker offset in that same transaction, and a repository that opened its own would break the atomicity that makes ingestion exactly-once.

`claim_notification` is an `INSERT … ON CONFLICT DO NOTHING` returning the affected row count as the `bool`. On MySQL that is `INSERT IGNORE`; on SQLite `INSERT OR IGNORE`. Dispatch on the backend, as the migrations do. (Note that Task 10's migration makes the MySQL arm of `up()` *refuse* — five of its indexes exceed InnoDB's 3072-byte key limit — so a MySQL branch here is symmetry with the siblings, not a reachable path.)

**Two notes on this paragraph, the first of which retracts a wrong "correction" Task 12 wrote here.**

*The backend dispatch.* It is not expressible at this seam, established by a compile attempt rather than by reading: splicing `runner.get_database_backend()` and `runner.db_engine()` into `claim_notification` produces two `error[E0599]: no method named … found for reference &'life1 C`. `DBRunner` declares no methods and the internal trait that could yield a connection is `pub(crate)` to `toolkit-db`. The information exists one layer out (`SecureConn::db_engine()` is `pub`), so this is a property of the bound the traits take, not of the toolkit.

*But the plan's instruction was right about the need, and Task 12's first answer to it was wrong.* That answer claimed `sea-query` renders `do_nothing()` as `INSERT IGNORE` on MySQL. **It does not.** Rendered on all three backends, `MysqlQueryBuilder` emits `ON DUPLICATE KEY IGNORE` — ` IGNORE` is written as the conflict *action* (`sea-query-0.32.7/src/backend/mysql/query.rs:155-180`, `DoNothing` arm at `:161-175`) after ` ON DUPLICATE KEY` has already been written by `prepare_on_conflict_keywords` (`:182-184`). That is neither `INSERT IGNORE` nor valid MySQL. Unreachable, because `up()` refuses the MySQL backend — but it *would* be a defect if MySQL were reachable: `claim_notification` would fail with a syntax error on every call and the send-once protocol would break rather than degrade. `the_do_nothing_clause_renders_per_backend_and_mysqls_is_malformed` renders all three and pins it.

What *did* need writing is obligation #4's half: SeaORM reports a `do_nothing` insert that changed nothing as `DbErr::RecordNotInserted` (`sea-orm-1.1.20/src/executor/insert.rs:351`), and that is the answer `false`, not a 500. The `do_nothing` is also **load-bearing on Postgres and not merely tidy**: a plain insert would abort the caller's transaction, so a lost claim would return the ordinary-looking `Ok(false)` while poisoning a transaction the caller is still using. `a_lost_claim_inside_a_transaction_leaves_the_transaction_usable` measures that on a real container, because `SQLite` cannot show it.

**RETRACTED — `SecureSelect` does support projection, `DISTINCT`, `GROUP BY` and subqueries.** Task 12 first wrote here, and onto two trait methods, that it "exposes only `filter`/`order_by`/`limit`/`offset`" and that "a repository cannot reach a raw connection", and shipped `latest_per_test` and `ingested_run_ids_between` as in-memory folds over the whole row set. Both claims are false. `project_all` (`libs/toolkit-db/src/secure/select.rs:396`) hands the closure the *already-scoped* `Select<E>` — its own doc-example shows `select_only().column().group_by()` — and `into_inner` (`:416`) returns the raw scoped `Select`; both sit in the same `impl` block about 120 lines below the four methods that were enumerated, and the reading stopped before them. **`qa-runs`' `queue_sea_repo::platforms_with_queued_rows` (`:302-323`) already recorded the identical wrong argument being made and corrected**, in the gear this task was told to follow.

Both methods now reduce in the database — `SELECT DISTINCT run_id` over a one-column projection, and `(test_file, latest) IN (SELECT test_file, MAX(latest) … GROUP BY test_file)` built from a clone of the scoped query so the subquery carries the caller's `AccessScope` by construction. These are the two tables `cpt-cf-qa-nfr-scale` targets 5M rows on, so "the output is small" was never a bound on the input.

*One divergence on `ResultsRepository::list_for_universe` was escalated by this pass and has since been decided and implemented* — the within-run tiebreak; see the `DECIDED` record after Step 5.

**`saved_views_sea_repo` MUST write `plan_key` on every insert and every update.** This is the one silent-correctness obligation Task 10's schema hands over, and it is invisible at the point where it would be violated. `qa_analytics_saved_views.plan_key` materializes legacy's `COALESCE(plan_id, '')` (`001_initial.sql:194-195`) because SQLite and MySQL do not both support functional indexes; it is the fourth column of `idx_qa_analytics_saved_views_unique`, and it holds `''` when the view is global (`scope = 'all'`) or `'<repo_id>/<plan_path>'` when it is plan-scoped. A writer that forgets it gets the `''` default, and a plan-scoped view then collides with the owner's *global* view of the same name — a wrong 409 with no error anywhere.

**The database cannot detect the violation, so a repository test is the only possible guard.** Add one asserting that a plan-scoped view and a global view with the same name and owner both persist *and* both come back from the list query — driving it through the repository, not through raw SQL, since the repository is the thing that has to remember. Task 10's `a_global_and_a_plan_scoped_view_may_share_a_name` proves the *schema* permits it; nothing yet proves the repository populates the column that makes it work.

Done as `a_plan_scoped_and_a_global_view_of_one_name_both_persist_and_both_list`, plus `updating_a_view_into_a_plan_scope_rewrites_its_plan_key` for the *update* half — an update that moves a view between scopes and leaves the old key behind is the same obligation on the path where forgetting it is worse. **One more test than the plan asked for, added because break-verification found the first two insufficient:** removing the `plan_key` predicate from `list` left the whole suite green, because a single plan-scoped view is separated from a global one by `scope` alone. `a_plan_scoped_list_returns_only_that_plans_views` (two plans) is what makes that predicate load-bearing.

A stored generated column would remove the obligation entirely and was considered and declined at Task 10 — the reasons are in that migration's header under "A **stored generated column** would also work, and was declined". Do not re-litigate it here without reading them.

- [x] **Step 4: Run the tests**

Run: `cargo test -p qa-insights --lib infra::storage`
Expected: all pass. Observed: 92 passed, 0 failed (96 with `--features integration --lib`).

- [x] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/infra/storage/
git commit -m "feat(qa-insights): SeaORM repositories"
```

#### DECIDED — the within-run tiebreak of `list_for_universe`

**Option A, chosen by the plan owner on 2026-08-20, and implemented in `45dafe9b`.** Escalated by the first Task 12 pass with four options; this section replaces that table, which should not be read as still open.

*What legacy does*, established not inferred: SQL orders by `COALESCE(finished_at, created_at) DESC, t.id DESC` (`analytics.rs:971`, `:1001`); the in-memory re-sort at `:1042` is `rows.sort_by(|a, b| b.ts.cmp(&a.ts))`, and `sort_by` is **stable**, so the SQL tiebreak survives it. `t.id` is a `SERIAL` and the bulk writer inserts in parse order (`argo.rs:2598-2615`). **Legacy's winner among rows of one run sharing a file is the last row the log parser produced**, consumed by `latest_per_test_snapshot`'s first-wins loop (`:1615-1632`).

*What ships*: `qa_test_results.ingest_ordinal INTEGER NOT NULL DEFAULT 0`, carrying the row's position within the batch that wrote it, with the **four-key** ordering option A specified: `sort_key DESC, created_at DESC, ingest_ordinal DESC, id DESC`.

**Both middle keys are required, and each has its own test.** `t.id` is a *global* SERIAL, so it carries two facts: across two runs sharing a timestamp the higher serial is the later-**ingested** run (reproduced by `created_at DESC`, one instant stamped per batch), and within one run it is the later row the **parser** produced (reproduced by `ingest_ordinal DESC`). The pair emulates a global serial descending; neither key does alone. `id DESC` is a fourth key only for totality.

> **`45dafe9b` shipped this as three keys, dropping `created_at`, and asserted equivalence in five places.** That regressed a case the previous commit got right: with two runs sharing a `run_finished_at`, the older-ingested run holding the file at a higher batch position won, where legacy picks the later-ingested one. Caught by the fourth review pass; fixed in `b6496f4a` with `a_tie_across_two_runs_is_won_by_the_later_ingested_one`, which no test had covered — which is exactly why it shipped. Removing either middle key now turns exactly one named test red.

Four choices inside option A, each argued at the code:

* **`NOT NULL DEFAULT 0`, not nullable.** `ORDER BY … DESC` puts `NULL`s first on Postgres and last on `SQLite`, so a nullable ordinal would make the tiebreak dialect-dependent — the one thing it exists to stop. The default is safe here in a way `plan_key`'s is not, because the writer is an exhaustive `ActiveModel` literal: a forgotten field is a compile error. It was one — adding the column broke the build at `new_result_am` until the field was wired.
* **Derived by the repository from `enumerate()`, not a field on `NewTestResult`.** The review sketched a field; the batch `Vec`'s order already *is* the producer's order, so a caller-supplied value could only be duplicated, sparse or transposed, none of which would fail to compile. Splitting one run across two calls would restart ordinals, but that is already broken for a stronger reason: the method deletes the whole run first.
* **Not on `ExecRow` and not on `TestResultRecord`.** A persistence ordering detail, same class as `updated_at`. Recorded as divergence 4 on the SDK type so its absence is deliberate rather than forgotten.
* **Not counted as a denormalized run column.** It carries no run attribute, so "seven denormalized run columns" stands and `ingest_ordinal` is named as the eighth added column with its own argument. The column *total* went 121→122 (`entity/mod.rs`).

Why the others were declined: **B** (per-row `created_at`) fabricates timestamps that `ExecRow::ts` reads for in-progress runs; **C** (a non-legacy key such as `test_name DESC`) is deterministic but diverges from legacy, which was the thing being fixed; **D** (status quo `id DESC` on a random v4 UUID) is stable per database and arbitrary across them.

**The SQL reduction in `latest_per_test` stays two-stage, and that was measured, not assumed.** Adding `MAX(ingest_ordinal)` to the group and the ordinal to the tuple compiles cleanly and is *wrong*: `MAX(ordinal)` is the maximum over every row of the file, not over the rows at `MAX(latest)`, so an older run holding a higher ordinal makes the tuple match nothing and the file vanishes from the result. `a_newer_run_wins_even_when_its_row_has_a_lower_ordinal` is the fixture that catches it. An exact single statement needs a two-level argmax — a window function over a subquery, or a correlated `NOT EXISTS` whose tenant predicate would have to be hand-written, which this method has no `tenant_id` parameter to write.

**That `git add` path is too narrow and was not used as written.** Step 0b touches `domain/analytics/mod.rs`, `domain/repos/results_repo.rs` and `qa-insights-sdk/src/models.rs`; the mapper's fail-closed decoders needed a `CorruptState` variant on `domain/error.rs` (the shape qa-runs uses, with its "who constructs what" header restated — it still said "**Nothing**"); `Cargo.toml` promotes `serde_json` from `[dev-dependencies]`, which the manifest's own ledger assigned to this task; and this plan file carries the corrections above. Everything changed was staged.

---

### Task 13: The transactional event consumer

**COMPLETE — `7255d812`, with obligation 2 in `0ad4adb3`.** qa-insights **92 -> 121** tests, 0 failed; `cargo fmt --all -- --check` and
`cargo clippy -p qa-insights -p qa-insights-sdk --all-targets -- -D warnings` clean with and
without `--features integration`. `cf-gears-event-broker` **6 -> 9**, 0 failed (obligation 2).
`cargo gears lint --dylint` and `cargo-shear` are still **not installed** and were not run.

**Four corrections to this task's own instructions, each measured:**

1. **`LOCAL_DB_OFFSET_STORE_MIGRATION_SQL` cannot be applied to Postgres.** `offset` is a
   *reserved* keyword there, so Step 4's "add the migration SQL to this gear's migrator" produces
   a migration that fails on the only dialect this gear deploys on. Measured against
   `postgres:15-alpine`: bare `offset` is `syntax error at or near "offset"`, quoted `"offset"`
   succeeds. `m20260818_000002_offset_store` therefore carries per-dialect DDL in
   `m20260818_000001_initial`'s shape, with the Postgres blob *derived from* the SDK constant in
   a test so an SDK change fails here instead of shipping an unreadable table. **This is a live
   defect in `event-broker-sdk`'s public API** — the constant is documented for exactly this use
   — and is not fixed from here, because a single quoted literal would then need a `MySQL`
   spelling. Raise it against that crate.
2. **`ConsumerGroupRef::gts(GROUP)`, which Step 4 specifies, cannot start a consumer.**
   `ensure_group` refuses it: *"consumer group GTS reference '...' must be resolved before
   startup"* (`event-broker-sdk/src/consumer/dispatcher.rs:1087`); only `Id` and `AutoAnonymous`
   are startable. The shipped spelling is `ConsumerGroupRef::existing(ConsumerGroupId::from_gts(GROUP))`,
   which `db_tx.rs` itself uses. `from_gts` is a deterministic hash, so the group identity is
   stable across processes — which is the property Step 4 wanted from `gts()`, and the reason
   `auto_anonymous` was still the wrong answer.
3. **Step 1's test needs the projection, so Task 13 ships it and Task 14 keeps the counters.**
   The test writes real rows, and carried items 9 and 10 assign this task the `app_build` feed
   and the in-transaction `upsert_run_results` call. `domain/service/ingest.rs` therefore lands
   here with the mapping, the `RunsReader` port (pulled forward from Task 15 Step 3) and
   `domain/system_actor.rs`. **Task 14 is not empty**: it still owns the five-counter
   classification and its two named tests, which nothing in this task counts.
4. **Redelivery is modelled by republishing, not by rewinding.** The mock broker exposes no
   replay control and `Db` exposes no raw connection to rewind `evbk_consumer_offsets` with. An
   at-least-once duplicate is the same thing from the consumer's side, and the property measured
   is identical: the second pass adds no row.

**Offset-manager fallback (Step 0's question): `Fallback::Earliest`.** A partition with no stored
cursor starts at the beginning of the topic. `Latest` would silently skip everything published
before this gear's first boot, which for a gear whose whole purpose is historical analytics is a
data-loss default; replaying is cheap because the projection is a replace, not an accumulate.

**Carried forward from this task:**

* **Ingest is quadratic in a run's test count, and that is worse than legacy.** qa-runs publishes
  one `test.result` per result row, and the projection re-reads the run's whole result list per
  event. Legacy polled Argo on a timer and did the same bulk replace once per poll. Correct and
  idempotent, but the amplification is real. **The remedy is a consumer change only**: a
  `TxConsumerHandler` with `ConsumerBatching`, collapsing a batch to its distinct run ids and
  committing the batch's last offset. No domain code moves. **Task 40 owns it** — that is where
  the consumer is wired for a deployment rather than for a fixture.
* **The ingest path scopes with `AccessScope::for_tenant`, not the PEP**, and the argument is on
  `domain::service::ingest`'s header: the tenant is not caller-supplied, and the shipped
  `static-authz-plugin` denies a system actor outright, so a PEP round-trip would mean an ingest
  path that does nothing in the only configuration this repository ships. `allow_all()` still
  appears nowhere in the gear. Whoever adds the second non-PEP scope should read that section
  first.
* **Obligation 2 is discharged but its premise was only partly right.** `#[serde(default)]` on
  `mode` and `default_storage_backend` fixes `config: {}`. A *missing* `event-broker:` key still
  fails `GearNotFound`, and a key with no `config:` still fails `MissingConfigSection` — both are
  `gear_config_required`, not serde. Removing those needs `ctx.config_or_default()` plus a
  `Default` on `EventBrokerConfig`, which changes that gear's operator contract, so it was left.
  The field doc carries the measured three-row table. `config/qa-platform.yaml` keeps its stanza.
* **Two of `m20260818_000001_initial`'s tests asserted the whole database's table inventory** and
  went red on the twelfth table, correctly. Both now name the tables later migrations own, in one
  constant, so they stay *exact* rather than being weakened to a prefix filter. A migration that
  adds a table adds a line there; that friction is the point.

This is the first event-broker consumer in the repository (Finding 9). Budget time for reading the SDK rather than pattern-matching a sibling, because there is no sibling.

**Files:**
- Create: `infra/events/{mod.rs,consumer.rs,payloads.rs}`
- Test: `infra/events/consumer.rs` in-file module

- [x] **Step 0: Read the SDK, then verify the topic**

Read, in order:

1. `gears/system/event-broker/event-broker-sdk/tests/consumer/single_handler.rs` — the minimal builder shape.
2. `.../tests/consumer/routed_handlers.rs` — per-event-type routing, which is what eight event types need.
3. `.../tests/consumer/db_tx.rs` — `TxSingleEventHandler`, `TxCommitHandle::commit_offset_in_tx`, `LocalDbOffsetManager`, and `LOCAL_DB_OFFSET_STORE_MIGRATION_SQL`. **This is the one to model.**

Then confirm the producer side: `qa-runs/qa-runs/src/infra/events/payloads.rs:96` for the topic constant, `:147-154` for the eight type ids, `:562` for `ALL_TYPE_IDS`. The topic is `gts.cf.core.events.topic.v1~cf.qa.runs.lifecycle.v1`.

Record in the task report which offset-manager fallback you chose and why.

- [x] **Step 1: Write the failing consumer test**

```rust
/// The projection and the offset advance in one transaction. Redelivering the
/// batch must therefore change nothing: the second delivery re-runs
/// delete-then-insert over identical rows and re-commits the same offset.
///
/// This is the property that lets qa-insights tolerate an at-least-once broker
/// without a dedupe table of its own.
#[tokio::test]
async fn a_redelivered_batch_leaves_the_projection_unchanged() {
    let f = consumer_fixture().await;
    f.publish_test_result(RUN_ID, "tests/a.py", "test_a", "PASSED").await;
    f.wait_for_ingest(1).await;

    let after_first = f.snapshot_test_results().await;
    f.redeliver_last_batch().await;
    let after_second = f.snapshot_test_results().await;

    assert_eq!(after_first, after_second, "redelivery must be a no-op");
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-insights a_redelivered_batch_leaves_the_projection_unchanged`
Expected: FAIL — no consumer.

- [x] **Step 3: Write the payload mirrors**

`infra/events/payloads.rs` holds `serde` mirrors of the eight qa-runs payloads. They are **re-declared, not imported**: `qa-runs`' payload types are `pub` but they live in a gear crate, and importing them would make qa-insights depend on `qa-runs` rather than on its contract — precisely the coupling ADR-0004 forbids. Add a module doc saying that, plus a test that pins each mirrored type id against the literal string, so a drift in the producer's constants fails here rather than silently routing nothing:

```rust
/// If qa-runs renames a type id, this fails. That is the point: a consumer that
/// silently matches nothing looks exactly like a quiet system.
#[test]
fn the_mirrored_type_ids_are_the_ones_qa_runs_publishes() {
    assert_eq!(TYPE_RUN_FINISHED, "gts.cf.core.events.type.v1~cf.qa.runs.run_finished.v1");
    assert_eq!(TYPE_TEST_RESULT, "gts.cf.core.events.type.v1~cf.qa.runs.test_result.v1");
    // ... the remaining six, all eight from qa-runs' ALL_TYPE_IDS
}
```

- [x] **Step 4: Write the consumer**

```rust
//! The broker consumer, and the reason it is transactional.
//!
//! # One transaction, two writes
//!
//! The handler is a [`TxSingleEventHandler`]: the projection write and the
//! offset commit share one database transaction
//! (`event-broker-sdk/tests/consumer/db_tx.rs` is the reference). Either both
//! land or neither does. Without that, the two orderings both lose data:
//! commit-then-write drops an event on a crash between them, and
//! write-then-commit double-applies on redelivery — survivable only because
//! delete-then-insert happens to be idempotent, which is a property of today's
//! projection and not a guarantee.
//!
//! # One consumer group, one topic
//!
//! qa-runs publishes all eight event types on a single topic and partitions
//! every run event on its run id (`qa-runs/.../payloads.rs:25-32`), so one
//! group sees one run's events in `created -> queued -> started -> finished`
//! order. Routing per type happens inside this consumer, not across groups.
```

Build it with `ConsumerBuilder::new(broker).group(ConsumerGroupRef::gts(GROUP)).topics([TOPIC]).offset_manager(LocalDbOffsetManager::new(...)).handler(...)`. Use a **named** group (`ConsumerGroupRef::gts`), not `auto_anonymous` — an anonymous group restarts from its fallback on every process start, which would re-ingest the whole topic on every deploy.

Route by `event.type_id` inside one handler rather than registering eight routed handlers: all eight write into the same transaction and the same repositories, so eight registrations would be eight copies of the transaction ceremony.

Add the offset-store migration (`LOCAL_DB_OFFSET_STORE_MIGRATION_SQL`) to this gear's migrator as a second migration file, `m20260818_000002_offset_store.rs`.

- [x] **Step 5: Run the test**

Run: `cargo test -p qa-insights a_redelivered_batch_leaves_the_projection_unchanged`
Expected: PASS.

- [x] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/infra/events/ gears/qa-platform/qa-insights/qa-insights/src/infra/storage/migrations/
git commit -m "feat(qa-insights): transactional event-broker consumer"
```

---

### Task 14: The ingest service

**COMPLETE.** qa-insights **121 -> 127** tests, 0 failed; fmt and clippy clean.

Task 13 already created both files and shipped Step 3's per-event-type
projection, for the reason recorded in its header. Task 14 therefore added the
half that was genuinely still missing: `StatusBucket`, `ResultCounts`,
`classify` and `classify_all`, with six tests — the plan's two, plus the whole
eight-value vocabulary, the all-uncounted set, case sensitivity, and the empty
set.

**Step 0 verified all three citations against `../vhp-testrunner`, 2026-08-20.**
All three land exactly where the plan says:

* `plans.rs:188-192` — the five counters, verbatim as the plan describes them.
* `analytics.rs:1359-1360` — `"XFAIL" => xfail += 1,` / `"XPASS" => xpass += 1,`.
  Counted separately, never folded into `passed`.
* `argo.rs:2170` — `derive_phase_from_result_flags`; any `SKIPPED` or
  `FAILED`/`ERROR` downgrades `Succeeded`/`Skipped` to `Failed`. qa-runs ports
  it; qa-insights does not re-derive a phase anywhere.

**Finding the plan does not mention, and Tasks 20/21/24 need it: legacy has
*four* status classifications, and they disagree.** The table is on
`domain::service::ingest`'s header. The trap is `bucketize_status`
(`analytics.rs:1940-1946`), which buckets **everything except `PASSED`,
`FAILED` and `ERROR` to `NOT_RUN`** — so a `SKIPPED` file is `NOT_RUN` in the
analytics universe while it is `StatusBucket::Skipped` here. Both are correct
for their own surface; reusing this task's `classify` for the universe core
would move every skipped test out of `NOT_RUN` and change numbers the UI
already renders.

**`classify_all` has no caller yet, and that is deliberate.** The projection
stores the runner's word verbatim and every aggregate is computed on read,
which is what legacy does — its five counters are a `COUNT(*) FILTER` in the
query that needs them, not a stored column. First consumer is Task 18.

**Files:**
- Create: `domain/service/{ingest.rs,ingest_tests.rs}`

- [x] **Step 0: Verify against legacy**

Two behaviors to confirm, both already documented in qa-runs' migration but worth re-reading at the source:

1. `manager/src/routes/plans.rs:188-192` — the five-counter projection: `passed <- PASSED`, `failed <- FAILED|ERROR`, `skipped <- SKIPPED`, `in_progress <- PENDING|RUNNING`, `total <- every row`. `XFAIL` and `XPASS` land in `total` and in none of the four.
2. `manager/src/routes/analytics.rs:1359-1360` — analytics counts `XFAIL`/`XPASS` separately and never folds them into `passed`. **Do not fold them.**

Also confirm from `manager/src/services/argo.rs:2170-2191` (`derive_phase_from_result_flags`) that any `SKIPPED` test downgrades a `Succeeded` run to `Failed`. qa-runs already ports that; qa-insights must not re-derive a different phase.

- [x] **Step 1: Write the failing classification tests**

```rust
/// The five counters, ported from `manager/src/routes/plans.rs:188-192`.
/// `XFAIL`/`XPASS` are deliberately in `total` and nowhere else — legacy counts
/// them separately (`manager/src/routes/analytics.rs:1359-1360`) and folding
/// them into `passed` would inflate every pass rate on the dashboard.
#[test]
fn xfail_and_xpass_count_toward_total_and_no_other_bucket() {
    let counts = classify_all(&["PASSED", "XFAIL", "XPASS", "FAILED"]);
    assert_eq!(counts.total, 4);
    assert_eq!(counts.passed, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.skipped, 0);
    assert_eq!(counts.in_progress, 0);
}

/// Status is an open set and the mapper must not fail closed: two legacy paths
/// write unconstrained text (`manager/src/routes/runs.rs:1115` unvalidated, and
/// `manager/src/services/argo.rs:2932-2943` falling through to
/// `other.to_uppercase()`). A ninth value is a runner change, not corruption.
#[test]
fn an_unrecognized_status_is_stored_and_counted_only_in_total() {
    let counts = classify_all(&["QUARANTINED"]);
    assert_eq!(counts.total, 1);
    assert_eq!(counts.passed + counts.failed + counts.skipped + counts.in_progress, 0);
}
```

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test -p qa-insights classify`
Expected: FAIL.

- [x] **Step 3: Write the ingest service**

The service takes a decoded event and a transaction, and projects. Per event type:

* `run.created` / `run.queued` / `run.started` — record nothing in `qa_test_results`; these carry no results. They exist for the dashboard's active/queued view, which reads qa-runs live (Task 18), so ingest ignores them apart from a trace span. Say that in a comment — a reader will otherwise assume a missing arm.
* `test.result` — one row into `qa_test_results` and, when `nodeid` is present and non-empty, one into `qa_test_case_results`.

  **`nodeid` is the discriminator, and Task 2 flagged that it is a weak one.** After Task 2's migration a file-level row and a case-level row are indistinguishable in `qa_run_test_results` — both can carry `nodeid = ''`. Legacy got the distinction for free from table identity (`test_results` vs `test_case_results`); this schema does not. Treat "non-empty `nodeid`" as "this is a case-level result", and write that rule down at the projection site, because it is a convention this schema cannot enforce. The denormalized columns (`platform_id`, `product_version`, `app_build`, `plan_id`, `branch`, `run_finished_at`) come from the run, which ingest looks up once per run and caches for the batch. **`app_build` is `qa_runs_sdk::Run::app_build` off that same cached object** — no extra call. Task 12 adds the column; this is where it gets filled, and a row written without it cannot be repaired later without a backfill.
* `run.finished` — stamp `run_finished_at` on the run's rows and advance nothing else.
* `run.canceled` / `run.queue_expired` / `schedule.fired` — no projection; consumed by Phase C's notification routing.

Classification is a pure function in this file, taking `&str` and returning a bucket — no repository, no async. That is what the Step 1 tests exercise directly.

- [x] **Step 4: Run the tests**

Run: `cargo test -p qa-insights --lib domain::service::ingest`
Expected: all pass.

- [x] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/domain/service/
git commit -m "feat(qa-insights): event projection with legacy counter semantics"
```

---

### Task 15: The reconciler

**COMPLETE.** qa-insights **127 -> 142** tests, 0 failed; fmt and clippy clean under both
feature configurations.

**Step 0, legacy read on 2026-08-20.** `run_results_poller.rs` runs **every 30s**
(`RUN_RESULTS_SYNC_INTERVAL_SECONDS`, `main.rs:182`, floored at 5s), with an immediate cycle at
startup, and each cycle lists **every workflow Argo holds** — no window, no watermark, no page
limit. `needs_persist_sync` (`:254-267`) then re-persists a run when it is `Running`/`Pending`
(always), when the persisted phase differs, when the persisted `test_count` is 0, or when there is
no row. This sweep runs every 300s over a bounded page from a windowed watermark and touches only
what is missing — strictly less work on every axis, which was the instruction. The full comparison
is on `domain::service::reconcile`'s header.

**Two legacy behaviours deliberately not ported, both recorded at the code:**

1. **In-progress runs are never reconciled**, because `list_runs_finished_since` never returns a
   run with a `NULL` `finished_at`. Legacy re-persisted live counts every 30s. The cost is a gap
   in the *in-progress* view after an outage, not lost data — the run lands in full at
   `run.finished`. Closing it needs a second unwindowed listing per pass, which is the aggression
   the instruction rules out. Task 18's dashboard already reads active runs from qa-runs live,
   which is what makes this safe.
2. **A run with genuinely zero results is re-projected on every sweep**, because
   `ingested_run_ids_between` reports runs that have *rows*. Legacy does the same thing from its
   `persisted.test_count == 0` arm, so this is parity. Bounded, and fixing it needs an
   "ingested, zero rows" marker that is a schema change no task owns.

**A defect in the plan's own Step 4, caught by a test.** Step 4 says to diff against
`ingested_run_ids_between(floor, page_max_finished_at)`. That method's window is **half-open**, so
the newest run in the page — whose `finished_at` *is* `page_max` — is excluded from the
already-ingested set and re-backfilled on **every** sweep, forever. The bound shipped is
`page_max + 1s`; an over-wide upper bound is free, because membership is only tested for page
members. `the_newest_run_in_a_page_is_not_rebackfilled_on_the_next_sweep` is the guard, and
removing the slack turns it and one other test red — measured.

**Both load-bearing rules were verified by mutation**, since the tests were written alongside the
implementation rather than before it:

| Mutation | Tests that go red |
|---|---|
| drop `DIFF_UPPER_SLACK` | `the_newest_run_in_a_page_is_not_rebackfilled_on_the_next_sweep`, `a_second_sweep_over_the_same_window_backfills_nothing_and_changes_nothing` |
| `continue` instead of `break` on a failed backfill | `a_gap_in_the_middle_of_a_page_stops_the_sweep_there` |

**Deviations from the file list, both additive:**

* `domain/ports/runs_reader.rs` already existed (Task 13); this task added its third method.
* The fake moved to `domain/service/test_support.rs`, where Step 3 asks for it, and the consumer's
  tests now use it too — one fake qa-runs, not two.
* `infra/leader/mod.rs` is the **fourth** copy of that trait in the tree (qa-runs, chat-engine,
  mini-chat). Unlike qa-runs this gear does **not** declare `cluster-sdk`: a declared-and-unused
  crate with a `cargo-shear` ignore is a cost worth paying when there is a consumer, and there is
  none until Task 40. `tokio-util` was promoted to `[dependencies]` one task earlier than the
  manifest's ledger forecast, for `CancellationToken`.

**Carried into Task 40: which tenants does the ticker sweep?** `reconcile_once` takes the tenant,
because this gear has no tenant registry — the event path learns one from an envelope and the REST
path from a caller. The honest options are a `qa_ingest_watermarks` scan (only finds tenants
already seen once) or a platform tenant directory (a new cross-gear dependency). Stated rather
than guessed at; the note is on the module header.

**Also carried: leader election is an optimisation here, not a correctness requirement**, and the
header says so rather than claiming otherwise. Two replicas sweeping one tenant produce a correct
projection — the backfill is delete-then-insert and `advance` never moves a mark backwards. What
election saves is N times the cross-gear reads. The one place it *would* be required is a rebuild
that truncated before rewriting, which Task 16 must therefore not do.

**Files:**
- Create: `domain/service/{reconcile.rs,reconcile_tests.rs}`
- Create: `domain/ports/runs_reader.rs`
- Create: `infra/leader/mod.rs`

- [x] **Step 0: Verify against legacy**

There is no legacy reconciler — legacy polled, so it had no gaps to close (`manager/src/services/run_results_poller.rs`). Read that file anyway and record its cadence and its batch shape in the task report: this task replaces it, and the replacement should not be more aggressive than the thing it replaces.

Then re-read PRD §11's event-broker risk row, which is the requirement this task satisfies.

- [x] **Step 1: Write the failing tests**

```rust
/// The sweep's core property: a run whose events were dropped entirely is fully
/// backfilled. This is what makes a non-durable broker survivable.
#[tokio::test]
async fn a_run_whose_events_were_never_delivered_is_backfilled() {
    let f = fixture().await;
    let ghost = f.runs.add_finished_run_with_results("2026-08-18T10:00:00Z", 3);

    f.service.reconcile_once(&f.ctx).await.expect("sweep succeeds");

    assert_eq!(f.results_for(ghost).await.len(), 3);
}

/// The watermark advances only past runs actually ingested. A sweep that
/// advanced on the page boundary would strand a run whose backfill failed.
#[tokio::test]
async fn a_failed_backfill_does_not_advance_the_watermark() {
    let f = fixture().await;
    f.runs.add_finished_run_with_results("2026-08-18T10:00:00Z", 1);
    f.runs.fail_next_result_read();

    let before = f.watermark().await;
    let _ = f.service.reconcile_once(&f.ctx).await;

    assert_eq!(f.watermark().await, before, "watermark must not move past a gap");
}

/// The lookback exists because a run can be written after a later sweep's
/// cutoff has already passed. Sweeping strictly from the watermark would miss it.
#[tokio::test]
async fn the_sweep_starts_a_lookback_before_the_watermark() {
    let f = fixture_with_lookback(Duration::from_secs(3600)).await;
    f.set_watermark("2026-08-18T12:00:00Z").await;
    let late = f.runs.add_finished_run_with_results("2026-08-18T11:30:00Z", 1);

    f.service.reconcile_once(&f.ctx).await.expect("sweep succeeds");

    assert_eq!(f.results_for(late).await.len(), 1);
}
```

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test -p qa-insights reconcile`
Expected: FAIL.

- [x] **Step 3: Define the `RunsReader` port and its fake**

The port names exactly the three qa-runs-sdk methods this gear reads through: `list_runs_finished_since`, `list_run_test_results`, `get_run`. A fake implementation lives in `domain/service/test_support.rs` and is what the Step 1 tests drive. Do not test against the real SDK client here — that is Task 40's integration wiring.

- [x] **Step 4: Write the reconciler**

Algorithm, per tenant:

1. Read the watermark; the sweep floor is `watermark - lookback`, or "the beginning" when unset.
2. `list_runs_finished_since(floor, page_size)` — oldest first (Task 4).
3. Diff the page's run ids against `ingested_run_ids_between(floor, page_max_finished_at)`.
4. For each missing run, `list_run_test_results` and project through the **same** ingest path Task 14 wrote. Not a second projection — a divergence between the event path and the backfill path is a bug that only appears after an outage, which is the worst possible time to find it.
5. Advance the watermark to the newest `finished_at` that was successfully ingested, and only that far.

Leader-elected: only one instance sweeps. `infra/leader/mod.rs` is a copy of `qa-runs/src/infra/leader/mod.rs` — read it first; it already handles the "no `ClusterProfile` bound yet" case that `qa-runs`' `Cargo.toml` comment describes, and re-solving that is wasted work. Role name: `qa-insights-reconciler`.

- [x] **Step 5: Run the tests**

Run: `cargo test -p qa-insights reconcile`
Expected: all three pass.

- [x] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/domain/ gears/qa-platform/qa-insights/qa-insights/src/infra/leader/
git commit -m "feat(qa-insights): leader-elected reconciler with watermark and lookback"
```

---

### Task 16: The rebuild endpoint

**Files:**
- Create: `api/rest/handlers/admin.rs`, `api/rest/routes/admin.rs`
- Modify: `domain/service/reconcile.rs`

- [x] **Step 1: Write the failing test**

```rust
/// A rebuild replays a closed time range regardless of the watermark, and
/// leaves the watermark alone: it is an operator tool for a known-bad window,
/// not a reset.
#[tokio::test]
async fn a_rebuild_replays_the_range_without_touching_the_watermark() {
    let f = fixture().await;
    f.set_watermark("2026-08-18T20:00:00Z").await;
    let old = f.runs.add_finished_run_with_results("2026-08-18T09:00:00Z", 2);

    f.service
        .rebuild(&f.ctx, datetime!(2026-08-18 08:00 UTC), datetime!(2026-08-18 10:00 UTC))
        .await
        .expect("rebuild succeeds");

    assert_eq!(f.results_for(old).await.len(), 2);
    assert_eq!(f.watermark().await, Some(datetime!(2026-08-18 20:00 UTC)));
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-insights a_rebuild_replays_the_range_without_touching_the_watermark`
Expected: FAIL.

- [x] **Step 3: Implement `rebuild` and register the route**

`POST /qa/v1/insights/rebuild` taking `{ "from": "...", "to": "..." }`. `OperationBuilder` shape as in `qa-environments/src/api/rest/routes/variables.rs`. It requires an admin scope — use the same `PolicyEnforcer` action the other mutating routes in this gear use, and do not invent a new one.

- [x] **Step 4: Run the test**

Run: `cargo test -p qa-insights a_rebuild_replays_the_range_without_touching_the_watermark`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/
git commit -m "feat(qa-insights): operator rebuild endpoint"
```

---

### Task 17: The two flat collections with OData

Per D7, OData applies here and only here — these are the two tables `cpt-cf-qa-nfr-scale`'s 5M-row target is about.

**Files:**
- Create: `infra/storage/odata.rs`, `api/rest/{handlers,routes}/collections.rs`

- [x] **Step 1: Write the failing test**

```rust
/// Only the columns that are indexed are filterable. An OData filter on an
/// unindexed column over 5M rows is a sequential scan wearing a query's
/// clothes, and `cpt-cf-qa-nfr-scale` is the requirement that forbids it.
#[test]
fn only_indexed_columns_are_filterable() {
    assert!(TestResultsField::parse("test_name").is_some());
    assert!(TestResultsField::parse("run_id").is_some());
    assert!(TestResultsField::parse("runFinishedAt").is_some());
    assert!(TestResultsField::parse("reason").is_none(), "unindexed");
}
```

All four assertions shipped verbatim, through the real API — `parse` is
`toolkit_odata::filter::FilterField::from_name`. Two things the pseudo-code did not
know, both recorded in `infra/storage/odata.rs`' header rather than worked around:

* **`runFinishedAt` and `test_name` cannot both resolve under one spelling of
  `name()`.** `from_name`'s default is `eq_ignore_ascii_case`, which does not
  bridge `_` to a capital. The field keeps its **column** name (the convention in
  every collection in this workspace, and what `api_dto`'s `rename_all =
  "snake_case"` puts on the wire) and `from_name` is overridden to also accept a
  separator-insensitive alias. Rejected: spelling the variant `runFinishedAt`,
  which advertises one camelCase field beside four snake_case ones.
* **`reason` is not a column of `qa_test_results`**, so the `"unindexed"` label
  understates it. It belongs to `qa_test_case_results` (`:499`) and is unindexed
  there too, so it is absent from both enums. The assertion holds for a stronger
  reason than the one given.
* **The test's *name* overstates the rule** and is kept because Step 2 runs it by
  name. `test_name` is the third column of `(tenant_id, test_file, test_name)`, so
  under the always-present tenant predicate it is an index **member**, not an index
  **prefix** — a `tenant + test_name` filter is a range scan over that index, not a
  seek. The module header carries the per-field prefix/member table and the
  accurate two-part rule; `the_columns_no_index_covers_are_rejected` is the
  companion test for the part the gate does buy unconditionally.

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-insights only_indexed_columns_are_filterable`
Expected: FAIL. **Actual:** `error[E0433]: cannot find module or crate
'toolkit_odata' in this scope` plus `error[E0432]: unresolved imports
'super::TestCaseResultsField', 'super::TestResultsField'` — "could not compile
`qa-insights` (lib test) due to 2 previous errors". Neither the field enums nor
the `toolkit-odata` dependency existed.

- [x] **Step 3: Implement**

Copy `qa-runs/src/infra/storage/odata.rs` wholesale and change the field enums — it already solves filter parsing, sort validation, cursor paging and the page-size clamp (`qa-runs/src/infra/storage/db.rs:34-37`: default 200, clamped 1–500). Reuse the clamp; one page-size rule across the subsystem's collections is the stated convention.

Register `GET /qa/v1/test-results` and `GET /qa/v1/test-case-results`.

**Citation corrected:** `PAGE_LIMITS` is at `db.rs:34-37`, not `:13-15` (that range is
its doc comment).

**The clamp is re-declared, not imported, and the import was compiled to prove it.**
`use qa_runs::infra::storage::db::PAGE_LIMITS;` fails with `error[E0603]: module 'db'
is private` — `qa-runs/src/infra/storage/mod.rs:25` declares `pub(crate) mod db`. The
same two literals now live in `qa-insights`' own `infra/storage/db.rs` with the
argument and the failed import recorded there. `QaInsightsConfig::max_page_size` is
**not** used for it: a per-deployment ceiling would give the subsystem two page-size
rules, which is the opposite of the convention above.

**Files beyond the plan's list.** `infra/storage/db.rs` (the clamp, `odata_err`, and
`db_err` moved out of `mapper.rs` — which `infra/storage/mod.rs` has asked Task 17 to
do since Task 10), `domain/service/results.rs` + `results_tests.rs` (a handler must not
compile a PDP scope), and the two repository methods, the `TestCaseResultRecord`
conversion and two response DTOs.

- [x] **Step 4: Run the tests and commit**

Run: `cargo test -p qa-insights --lib infra::storage::odata`
Expected: pass. **Actual: 8 passed.** Whole gate: `cargo fmt --all -- --check` clean,
`cargo clippy -p qa-insights -p qa-insights-sdk --all-targets -- -D warnings` clean,
`cargo test -p qa-insights -p qa-insights-sdk` **213 passed / 0 failed** (baseline
182), `cargo test -p qa-insights --features integration --lib` **218** (baseline 187).

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/
git commit -m "feat(qa-insights): OData collections for test results"
```

---

### Task 18: The dashboard

**Files:**
- Create: `domain/service/{dashboard.rs,dashboard_tests.rs}`, `api/rest/{handlers,routes}/dashboard.rs`

- [x] **Step 0: Verify against legacy**

Open `manager/src/routes/dashboard.rs:84` (`api_dashboard`) and read the whole function. Record:

* the `days` parameter's default and clamp — `unwrap_or(14).clamp(3, 90)`,
* that recent runs, the active/pending count, the active list and the total are **all derived from one listing**, with an explicit comment saying a second call costs a kube round-trip for no new information,
* the per-run counter SQL at `:167-178`: `COUNT(tr.id)` for total, `FILTER (WHERE status = 'PASSED')`, `FILTER (WHERE status IN ('FAILED','ERROR'))`, `FILTER (WHERE status = 'SKIPPED')`,
* that "active" is `phase == "Running" || phase == "Pending"`, and both the count and the list use that predicate, with the list capped at 10.

**All four confirmed verbatim.** `days` is `:104`; the one-listing comment is `:143-146`
and the four derivations it covers are `:147-158`; the active predicate is `:150` (count)
and `:154` (list) with `.take(10)` at `:155`.

**Citation corrected: the per-run counter SQL is `:167-178`, not `:170-190`.** `:170-173`
are the four counters, `:177` is the `GROUP BY`, `:178` closes the string; `:179-190` are
the `.bind`/`.fetch_all`/`match` around it, so the range overshot by twelve lines. Two
other numbers in this task's text land exactly and are unchanged.

**Four things Step 0 found that the plan's text does not mention, and each of them
changed the implementation:**

1. **There is a *second* status fold in the same endpoint.** The daily trend
   (`:218-219`) counts only `PASSED` and `IN ('FAILED','ERROR')` — a `SKIPPED` row moves
   neither, unlike in the per-run counters. So `api_dashboard` alone reads its rows two
   ways, and ruling R5's table of four legacy classifications is really five.
   `daily_points` reproduces the two-counter form rather than projecting the four.
2. **The daily trend's axis is `generate_series` + `LEFT JOIN`** (`:220`), so a day with
   no runs is a zero and not a gap, and `start_day = today - (days - 1)` (`:105`) makes
   the series exactly `days` points.
3. **`total_runs` is `runs.len()` over legacy's *whole* run history** (`:147`), which is
   a local `SELECT` there and is not obtainable here: `QaRunsClientV1::list_runs`'
   `limit` is mandatory (`qa-runs-sdk/src/client.rs:42-46`). Read locally as
   `COUNT(DISTINCT run_id)` over the ingested projection, per this task's own mapping row
   at `:330`, and the divergence is recorded on the repository method.
4. **Legacy's dashboard has no queued count at all, and could not have one.**
   `manager/src/services/run_queue.rs` contains **zero** references to `run_results` and a
   queued launch has no Argo Workflow, so a queued run appears in neither half of
   `list_runs_with_history` (`manager/src/services/run_history.rs:311-391`). The PRD
   nonetheless requires "active **and** queued runs" (`docs/PRD.md:579`), so
   `queued_runs` is net-new on `qa_insights_sdk::DashboardStats` — a `RunState::Queued`
   fold over the same page, not a `list_queue` call.

**The `phase` → `RunState` mapping**, which is the one real translation and had a wrong
answer available: `Pending → Dispatching`, `Running → Running`, and nothing else.
`services/argo.rs:306` stamps `Pending` on the run returned from a *successful submit*
(in `submitted_run`, `:283`) and `:2273-2275` defaults an absent Argo phase to it, so
Argo `Pending` is "submitted, not started" — which is
`qa-runs/src/domain/state_machine.rs`' `Dispatching` (`:108` for the state order, `:234`
for "The executor accepted the run"). `Created` (`:223-226`) and `Queued` are excluded,
and so are the six terminal states (`:52-59`).

- [x] **Step 1: Write the failing tests**

```rust
/// Legacy's window: default 14 days, clamped to [3, 90]
/// (`manager/src/routes/dashboard.rs`, the `days` binding in `api_dashboard`).
#[test]
fn the_day_window_defaults_to_fourteen_and_clamps_to_three_and_ninety() {
    assert_eq!(resolve_days(None), 14);
    assert_eq!(resolve_days(Some(1)), 3);
    assert_eq!(resolve_days(Some(365)), 90);
    assert_eq!(resolve_days(Some(30)), 30);
}

/// `failed` folds ERROR in; `skipped` does not. Ported from the counter SQL in
/// `api_dashboard` (`FILTER (WHERE tr.status IN ('FAILED','ERROR'))`).
#[test]
fn error_counts_as_failed_in_the_run_trend() {
    let point = run_trend_point(&["PASSED", "ERROR", "FAILED", "SKIPPED"]);
    assert_eq!(point.passed, 1);
    assert_eq!(point.failed, 2);
    assert_eq!(point.skipped, 1);
    assert_eq!(point.tests_total, 4);
}

/// The active list is capped at 10 and uses the same predicate as the count, so
/// a dashboard showing "23 active" never lists more than ten of them.
#[test]
fn the_active_list_is_capped_at_ten_and_matches_the_count_predicate() {
    let runs = runs_in_phases(&["Running"; 23]);
    let stats = summarize(&runs, 14);
    assert_eq!(stats.active_runs, 23);
    assert_eq!(stats.active_runs_list.len(), 10);
}
```

All ten assertions shipped verbatim. Two of the four helper names could not survive as
written, and both substitutions are recorded in `dashboard_tests.rs`' header:

* **`runs_in_phases(&["Running"; 23])` takes Argo phase strings**, and there is no phase
  in this architecture. `runs_in_states(&[RunState::Running; 23])` replaces it, and the
  mapping it stands on is asserted separately and exhaustively by
  `only_dispatching_and_running_runs_are_active` — one line per `RunState` variant, so a
  count would not pass if two variants swapped.
* **`summarize(&runs, 14)` took a day count the run listing does not use.** Legacy's
  listing is not windowed by `days` — every run number is derived at `:147-158`, before
  `days` is used at `:218-231` — so `summarize_runs(&runs)` takes only the runs rather
  than carrying an ignored argument.

`run_trend_point(&["PASSED", …])` is a two-line fixture over the **production** fold
(`run_trend_points`), not a re-implementation of it, so the assertion fails if the fold
is wrong rather than if the fixture is.

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test -p qa-insights dashboard`
Expected: FAIL. **Actual:** `test result: FAILED. 0 passed; 19 failed`, every one of them
`panicked at domain/service/dashboard.rs: not yet implemented: Task 18 step 3` — the
module was written signatures-first with `todo!()` bodies so the failures are the
assertions' own rather than a compile error.

- [x] **Step 3: Implement**

Run counts come from `qa_test_results` locally. Active/queued runs come from `RunsReader` — they are live run state, which insights does not own and must not cache (`cpt-cf-qa-principle-db-first-state`: qa-runs is authoritative). Register `GET /qa/v1/dashboard`.

**Files beyond the plan's list, each with a reason:**

* `domain/ports/runs_reader.rs` + `infra/clients/qa_runs.rs` — one method,
  `list_recent_runs`, under ruling R3, with the header's method counts corrected. **Two
  of those counts were false, not merely stale:** `QaRunsClientV1` has **seventeen**
  methods (`qa-runs-sdk/src/client.rs:24-200`), where the port header and the adapter's
  test module both said nineteen.
* `domain/repos/results_repo.rs` + `infra/storage/results_sea_repo.rs` — three aggregate
  reads and the `RunStatusCount` row they share. The database groups by
  `(run_id, status, run_finished_at)` and **the domain folds**, rather than legacy's
  `COUNT(*) FILTER (WHERE status = …)`: this gear already holds that vocabulary in
  `ingest::classify`, and putting it into `SeaQuery` too would be two spellings of
  `FAILED`+`ERROR` that nothing keeps in agreement. It is also what makes the mandated
  fold test drive production code, and what keeps the statements dialect-neutral — the
  day is bucketed in Rust because `DATE(x)` returns `TEXT` on `SQLite` and `date` on
  Postgres.
* `qa-insights-sdk/src/models.rs` — `DashboardStats::queued_runs`, per Step 0's finding 4.
* `domain/service/mod.rs`, `api/rest/{dto,mod}.rs` and the two `mod.rs` files — the
  `AppServices` field, the response DTOs and the registrations.

**Ruling R5 discharged: `ingest::classify` is reused, and it was verified first.** The
dashboard's four counters (`:170-173`) and the five-counter rule
(`manager/src/routes/plans.rs:188-192`) agree on every case they share — the dashboard
simply has no in-progress counter, and a `PENDING` row lands in `COUNT(tr.id)` and in
none of the three filters, which is exactly what folding `StatusBucket::InProgress` in
with `Uncounted` does. The analytics `bucketize_status`
(`manager/src/routes/analytics.rs:1940-1946`) disagrees on `SKIPPED` and is untouched.

**The PEP action is `actions::LIST`**, reused rather than a new `view_dashboard`. A
second action would let a policy give the aggregate a *different* row set from
`/qa/v1/test-results`, and an aggregate over rows the subject may not list is an
inference channel nothing in the crate would notice. `domain/service/dashboard.rs`'
header carries the argument, what a deployment gives up, and what it would take to add
`view_dashboard` safely.

**Ten of the seventeen contract fields are omitted from the wire rather than emitted as
zeros.** A `0` is indistinguishable from a measured zero and an absent key is not; adding a
key later is compatible where correcting a wrong one is not. **This endpoint therefore does
not discharge `cpt-cf-qa-fr-insights-dashboard` on its own** — that requirement
(`docs/PRD.md:575-581`) also names *pass rates*. **Corrected 2026-08-21 by Task 19:** this
paragraph said the requirement "is satisfied across Tasks 18, 19, 21 and 23", and Task 19's
Step 0 disproved the `19`. Its pass-rate half is Tasks 21b and 23; its **coverage half is not
discharged in this feature at all** — Task 19 ships the endpoint's shape and no data, for the
reasons recorded in Task 19's Step 0 block below. Ownership, as ruled after the Task 18
review:

* **Task 21** — `failed_recent` and the four 24-hour counters. Computable from
  `qa_test_results` alone, and deliberately not Task 18's: legacy's KPI query
  (`manager/src/routes/dashboard.rs:317-348`) carries **no** phase restriction and windows
  on `COALESCE(finished_at, created_at)`, so it counts rows of runs still in progress — a
  *third* row-inclusion rule inside this one endpoint, needing its own Step 0, which Task
  21 performs anyway. Folding them out of Task 18's windowed read would have silently given
  them the daily trend's rule.
* **Task 23** — `flaky_tests` and `quality_vectors_pass_rate`. **The traceability row for
  this requirement names Task 25, which is the assembler**; 23 is where they are computed.
* **Unowned** — `total_plans`, `total_schedules` and `platforms_summary`, which need a
  qa-catalog plan listing, a qa-runs schedule listing and a qa-environments port
  respectively. Raised rather than absorbed.

- [x] **Step 4: Run the tests and commit**

Run: `cargo test -p qa-insights dashboard`
Expected: all pass. **Actual:** `test result: ok. 20 passed; 0 failed` for the filter, and
`240 passed; 0 failed` for the crate (from 214 at the base commit). The Postgres tier is
`246 passed` (from 219) and includes one new test that the three aggregate statements are
accepted by a real server — the `SQLite` tier cannot see a dropped `GROUP BY` key, where
Postgres refuses one.

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/
git commit -m "feat(qa-insights): dashboard aggregates"
```

---

### Task 19: Coverage

**Files:**
- Modify: `domain/service/dashboard.rs`, `api/rest/{handlers,routes}/dashboard.rs`

- [x] **Step 0: Verify against legacy**

Open `manager/src/routes/dashboard.rs:564-573` — `CoverageBuild` and `api_coverage`. Record the exact field set and the grouping key; the endpoint takes no parameters, which is itself worth confirming rather than assuming.

(**Step 0 performed 2026-08-21. Both citations land**: `CoverageBuild` is `dashboard.rs:564-569`, `api_coverage` is `:573`, and it does take no parameters — `State(state)` only, routed bare at `manager/src/routes/mod.rs:301-304`. Field set: `product_key`, `version`, `build` = `format!("{}/{}", product_key, version)` (`:618`), `coverage` = `CoverageSummary { line_pct, branch_pct, function_pct }` (`manager/src/models.rs:1482-1486`). Grouping key: `product_key`, first survivor of a newest-first ordering, with the dedupe check *inside* the parse (`:611-621`) so a run with no marker does not consume its product's slot. A build with no coverage is **absent**, never zeros — there is no `else` on `:611`.

**What the plan did not know, and it changes the deliverable.** The percentages exist nowhere but Argo *workflow log text*: `api_coverage` fetches each run's logs (`:606-609`) and parses `=== COVERAGE_SUMMARY: {line} {branch} {function} ===` (`manager/src/services/argo.rs:2718-2728`), and legacy's schema has **no** coverage column at all (zero occurrences of `coverage` in `manager/migrations/001_initial.sql`, its only migration). This gear's sole source of run data is qa-runs, no method of whose client returns log text (`qa-runs-sdk/src/client.rs:24-200`, seventeen methods), and that is deliberate: `qa_runs_sdk::Run::log_storage_ref` is an archived-log *pointer* documented as "populated on completion (p2 with 2.7)" (`qa-runs-sdk/src/models.rs:376-380`). Neither event payload carries a log slice either. The grouping key is missing for a second, independent reason — `qa_runs_sdk::Run` has no `product_key` (open question 1, deferred to Task 20).

So the endpoint is registered with legacy's shape and answers the **empty array**, which is also legacy's answer in any deployment that does not collect coverage, and no number is folded out of `qa_test_results` to fill it. Step 1's "test asserting the grouping" is therefore unwritable — there is no grouping in this port to assert over — and the file's test-module header records that rather than inventing one. **Closing the gap is p2 work in qa-runs, not a change to this gear**; it needs log text or a parsed coverage triple on the run object plus a `product_key` mapping.)

- [x] **Step 1: Write the failing test**

Write it from the field set Step 0 produced — one test asserting the grouping, one asserting a build with no runs is represented the way legacy represents it (present with zeros, or absent — **check, do not guess**).

- [x] **Step 2: Run it, implement, run it again**

Run: `cargo test -p qa-insights coverage`
Expected: FAIL, then PASS.

Register `GET /qa/v1/dashboard/coverage`.

- [x] **Step 3: Phase A gate**

```bash
cargo fmt --all -- --check
cargo clippy -p qa-insights -p qa-insights-sdk --all-targets -- -D warnings
cargo test -p qa-insights -p qa-insights-sdk
cargo test -p qa-insights --features integration --lib
cargo build --workspace
```

Expected: green. The integration tier needs Docker; if it is unavailable, say so in the task report rather than skipping silently.

- [x] **Step 4: Commit** — `e190bc7a`, plus `72f95434` from its review round.
- [x] **Step 4b: Squash the phase** — **DONE 2026-08-21** — `886e2419`, four tasks later than intended; `git diff` pre- vs post-squash was empty and the gate was re-run afterwards. Parent `39be61c4` (the event-broker config fix) was deliberately kept separate.

```bash
git add gears/qa-platform/qa-insights/
git commit -m "feat(qa-insights): coverage view"
```

Then squash Tasks 8–19 into
`feat(qa-platform): qa-insights — ingest, reconciler, history, dashboard`.

(**Split in two 2026-08-21.** The commit landed; the squash had not, and one checkbox covering
both would have claimed a history rewrite that nobody performed. Phase A's whole-phase review
runs *before* the squash, on purpose — it is the last chance to make these registers true while
the per-task commits that justify each of them are still individually readable. Its own fix
commit is the last one to fold in.)

---

# Phase B — Analytics

Eleven tasks. Every one ports a function that produces a number a user reads, so every one is test-first against legacy's output, not against a plausible reimplementation.

**A standing instruction for this phase.** Legacy's analytics is full of small normalizations that look like noise and are not: `normalize_alias:1782`, `normalize_test_path:1965`, `infer_component_from_path:1794`, `fallback_test_name:1805`, `slugify:2142`, `compare_build_desc:2188`. Each one exists because some repository in production names something inconsistently. **Port them verbatim, including behavior you would call wrong.** If you believe one is a bug, write the test that pins the current behavior, port it, and raise the bug separately.

### Task 20: The universe core

**Files:**
- Create: `domain/analytics/{mod.rs,universe.rs,universe_tests.rs}`
- Create: `domain/ports/catalog_reader.rs`

- [x] **Step 0: Verify against legacy**

Read, in this order, and take notes on each:

* `routes/analytics.rs:805` `load_universe_and_rows` — how the universe and the execution rows are assembled and filtered.
* `:1714` `build_alias_map`, `:1750` `resolve_row_test_file`, `:1766` `add_alias`, `:1782` `normalize_alias` — the alias machinery that matches an execution row to a universe file when the two spell the path differently. This is the single most easily-missed behavior in the phase: get it wrong and tests silently show as `not_run`.
* `:1182` `build_latest_map` and `:1212` `build_stats_map` — latest-status-per-test and the per-test pass/fail/skip tallies. Note that `build_latest_map` takes the **first row it sees** per file (`:1194`, `if latest.contains_key(..) { continue }`) and relies on the query having ordered them; it does not compare timestamps itself.
* `:1940` `bucketize_status` — the three-way collapse. (`:1948` `build_status_rank` exists too, but it is used only by `api_build_tests`' sort at `:444-448`, not here — do not wire it into the latest-map.)
* `:24-27` (the `branch` field doc) — the branch filter uses `source_ref` and falls back to the legacy `test_version`.

Write the alias rules down explicitly in the task report before writing any code.

- [x] **Step 1: Write the failing tests**

One per rule found in Step 0. At minimum:

```rust
/// `normalize_alias:1782`: trim, lowercase, replace every non-alphanumeric
/// character with a space, collapse runs of whitespace, join with single
/// spaces. This is what lets `Cluster Upgrade`, `cluster_upgrade` and
/// `cluster-upgrade` all resolve to one file.
#[test]
fn alias_normalization_collapses_punctuation_and_case() {
    assert_eq!(normalize_alias("  Cluster_Upgrade--Test "), "cluster upgrade test");
    assert_eq!(normalize_alias("test_a.py"), "test a py");
    assert_eq!(normalize_alias("!!!"), "");
}

/// `build_alias_map:1714` registers four aliases per universe entry: the
/// normalized path, the normalized file **stem**, the normalized test name, and
/// the normalized TEST_META title. A row carrying only a test name resolves
/// through any of them (`resolve_row_test_file:1762-1763` — corrected from
/// `:1758-1761` by Task 20, which is the `return Some(normalized)` of the
/// *explicit-path* branch, not the alias lookup).
#[test]
fn a_row_with_only_a_test_name_resolves_through_any_of_the_four_aliases() {
    let universe = vec![universe_test_full(
        "tests/cluster/test_upgrade.py",
        "test_upgrade",
        Some("Cluster Upgrade"),
    )];
    let aliases = build_alias_map(&universe);

    for name in ["test_upgrade", "test upgrade", "Cluster Upgrade", "tests/cluster/test_upgrade.py"] {
        assert_eq!(
            resolve_row_test_file(None, name, &aliases).as_deref(),
            Some("tests/cluster/test_upgrade.py"),
            "alias {name} must resolve"
        );
    }
}

/// `add_alias:1766`: an alias claimed by two **different** files is poisoned to
/// `None` and resolves to nothing thereafter. Guessing one of the two would
/// attribute a result to the wrong test, which is worse than not attributing it.
#[test]
fn an_ambiguous_alias_resolves_to_nothing() {
    let universe = vec![
        universe_test_full("tests/a/test_smoke.py", "test_smoke", None),
        universe_test_full("tests/b/test_smoke.py", "test_smoke", None),
    ];
    let aliases = build_alias_map(&universe);
    assert_eq!(resolve_row_test_file(None, "test_smoke", &aliases), None);
}

/// `resolve_row_test_file:1755-1760` (corrected from `:1751-1757` by Task 20,
/// which is the signature rather than the branch): an explicit non-empty `test_file` wins
/// outright and is only normalized — the alias map is never consulted for it.
#[test]
fn an_explicit_test_file_bypasses_the_alias_map() {
    let aliases = build_alias_map(&[]);
    assert_eq!(
        resolve_row_test_file(Some("./tests/a.py"), "irrelevant", &aliases).as_deref(),
        Some("tests/a.py"),
    );
}

/// `build_latest_map:1194`: the **first** row per file wins, and rows outside
/// the universe are skipped entirely (`:1190`). The ordering is the query's
/// responsibility, so the repository must return newest-first.
#[test]
fn the_latest_status_is_the_first_row_and_rows_outside_the_universe_are_ignored() {
    let universe = vec![universe_test("tests/a.py")];
    let rows = vec![
        exec_row_at("tests/a.py", "FAILED", "2026-08-18T12:00:00Z"),
        exec_row_at("tests/a.py", "PASSED", "2026-08-18T09:00:00Z"),
        exec_row_at("tests/deleted.py", "PASSED", "2026-08-18T13:00:00Z"),
    ];
    let latest = build_latest_map(&universe, &rows);
    assert_eq!(latest.len(), 1);
    assert_eq!(latest["tests/a.py"].status_bucket, "FAILED");
}

/// Absent branch = all branches (`routes/analytics.rs:24-27`, "Absent = all
/// branches (unchanged behavior)"). The filter itself is trivial; the part that
/// is not is **where** the branch comes from — see the note below this block.
#[test]
fn an_absent_branch_filter_matches_every_row() {
    let rows = vec![row_on_branch("tests/a.py", Some("main")), row_on_branch("tests/b.py", None)];
    assert_eq!(filter_by_branch(&rows, Some("main")).len(), 1);
    assert_eq!(filter_by_branch(&rows, None).len(), 2, "absent = all branches");
}
```

**Where the branch comes from, and why it is not resolved here.** Legacy reads a run's branch as `source_ref` *falling back to* `test_version` at query time, because both columns live on the row it is already selecting. This gear denormalizes one resolved `branch` column onto `qa_test_results` at ingest (Task 10, Task 14), so the fallback is applied **once, on write**, and the universe core filters a single value. Two consequences to be deliberate about: the fallback belongs in Task 14's projection and needs its own test there; and rows ingested before that logic is correct cannot be fixed by a query change — they need a rebuild (Task 16). Say both in the ingest code's comment.

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test -p qa-insights --lib domain::analytics::universe`
Expected: FAIL.

- [x] **Step 3: Define `CatalogReader` and write the core**

The port is one method, `list_universe(product_id, branch) -> Vec<UniverseTest>`, delegating to Task 7's SDK call. The core takes `&[UniverseTest]` and `&[ExecRow]` and returns the joined latest-map — **pure**, no async, no repository. Its fake is a plain `Vec`.

- [x] **Step 4: Run the tests and commit**

Run: `cargo test -p qa-insights --lib domain::analytics::universe`
Expected: pass.

```bash
git add gears/qa-platform/qa-insights/qa-insights/src/domain/
git commit -m "feat(qa-insights): analytics universe core with alias resolution"
```

---

### Task 21: Summary and lists

**Files:**
- Create: `domain/analytics/{aggregates.rs,aggregates_tests.rs}`

- [x] **Step 0: Verify against legacy**

* `:1227` `build_summary` — the three file-level counters, the six case-level counters, `case_expected`, and `pct:1957`.
* `:1270` `effective_case_status` — read the doc comment: *"Worst non-passing case status (so an xfail inside an otherwise-green file wins). ERROR folds into FAILED. None when no statuses."*
* `:1288` `attach_case_data` — read its doc comment in full, especially: *"A file with no per-case rows (older runner) contributes one case of its file-level status, so totals never undercount."*
* `:1397` `build_lists` — how the three lists are populated and ordered.

**Those four bullets are Task 21a's. A fifth, the KPI query, is Task 21b's** — see
the split recorded at the end of this section.

- [x] **Step 1: Write the failing tests**

```rust
/// `manager/src/routes/analytics.rs:1288`, the `attach_case_data` doc comment:
/// a file with no per-case rows contributes one case of its file-level status,
/// "so totals never undercount". Dropping it makes `case_total` smaller than
/// `total` on any run predating case markers.
#[test]
fn a_file_without_case_rows_contributes_one_case_of_its_file_status() {
    let summary = summarize(&[file_result("tests/a.py", "PASSED")], &[]);
    assert_eq!(summary.case_total, 1);
    assert_eq!(summary.case_passed, 1);
}

/// `effective_case_status:1270`: the worst non-passing case wins, and ERROR
/// folds into FAILED.
#[test]
fn the_worst_non_passing_case_status_wins_and_error_folds_into_failed() {
    assert_eq!(effective_case_status(&["PASSED".into(), "XFAIL".into()]).as_deref(), Some("XFAIL"));
    assert_eq!(effective_case_status(&["PASSED".into(), "ERROR".into()]).as_deref(), Some("FAILED"));
    assert_eq!(effective_case_status(&[]), None);
}
```

Plus one per remaining rule from Step 0.

- [x] **Step 2: Run, implement, run**

Run: `cargo test -p qa-insights --lib domain::analytics::aggregates`
Expected: FAIL, then PASS.

- [x] **Step 3: Commit**

```bash
git commit -am "feat(qa-insights): summary and list aggregates"
```

**Split into 21a and 21b, 2026-08-21.** This task's Step 0 carried two unrelated
deliverables: the analytics-side summary/lists fold over
`domain::analytics::{universe,mod}`, and the dashboard's 24-hour KPI window over
`DashboardStats`. They share no input type, no file and no legacy function — the
first is `analytics.rs:1212-1449`, the second is `dashboard.rs:317-348` — so they
were dispatched separately.

* **Task 21a — done (this section's four ticked steps).** `domain/analytics/{aggregates.rs,aggregates_tests.rs}`:
  `build_stats_map` (`:1212`), `build_summary` (`:1227`), `pct` (`:1957`),
  `effective_case_status` (`:1270`), the pure half of `attach_case_data`
  (`:1288`), `build_lists` (`:1397`) and `sorted_versions_desc` (`:2164`), plus
  `domain::analytics::CaseRow` — the four-column case-level row legacy projects at
  `:1308-1319`, which had no type in this domain. 14 tests, all mutation-verified.
  Touches no service, no repository and no route.
* **Task 21b — the KPI bullet, still open.** Its text, unchanged:

  * **`:317-348`, the KPI query — carried into this task out of Task 18** (carried item 13). This task owns `failed_recent`, `failed_24h_count`, `failed_prev_24h_count`, `pass_rate_24h` and `pass_rate_prev_24h` on `DashboardStats`. Read that query before you port them: it has **no `phase` restriction** and windows on `COALESCE(finished_at, created_at)`, so it counts runs that have not finished — a third row-inclusion rule, disagreeing with both the per-run counters (`:167-178`) and the daily trend (`:218-219`) that Task 18 ported. Do not assume Task 18's window helpers apply; `domain::service::dashboard`'s `window_start` deliberately drops runs with no finish instant. Legacy distinguishes "no data" from "0%", which is why the two pass rates are `Option<f64>`.

  It owns `failed_recent`, `failed_24h_count`, `failed_prev_24h_count`,
  `pass_rate_24h` and `pass_rate_prev_24h` on `qa_insights_sdk::DashboardStats`,
  which means `domain/service/dashboard.rs` + `dashboard_tests.rs`,
  `qa-insights-sdk/src/models.rs`, a repository method and a handler — the whole
  vertical Task 21a deliberately does not touch.

**Three Step 0 findings Task 21a recorded, all verified against legacy:**

1. **`case_expected` is not this fold's, and legacy's own comment misnames its
   owner.** `build_summary` sets it to `0` (`:1264`) under the comment "Filled in
   by `attach_case_summary`" — **a function that does not exist**; the filling
   happens in the handler at `:769-780`, from `load_collect_counts` (`:2672`) with
   `UniverseTest::static_case_count` as the per-file fallback. That fold is
   **Task 29's** `expected_cases`. `OverviewSummary::case_expected` is therefore
   present and zero out of `domain::analytics::aggregates`, exactly as it is out of
   legacy's `build_summary`, and a test pins the zero so Task 29 does not
   double-count.
2. **`attach_case_data`'s "so totals never undercount" overstates itself.** Its
   fallback arm (`:1371-1376`) counts `PASSED` and `FAILED`; its third pattern,
   `"SKIPPED"`, **cannot match**, because it tests `LatestInfo::status_bucket`,
   which is `bucketize_status`' output and can only be one of three words. So a
   `NOT_RUN` file with no case rows contributes nothing and `case_total` is
   legitimately below `total`. Ported verbatim and pinned; not fixed, because the
   fix changes a rendered number.
3. **`build_stats_map` and `bucketize_status` are the same partition, not two.**
   `domain::analytics::universe`'s header claimed `build_stats_map` "counts a
   `SKIPPED` row as skipped rather than as not-run", implying an arithmetic
   difference. Measured at `:1218-1222` against `:1940-1946`, both match `PASSED`,
   then `FAILED | ERROR`, then a catch-all — only the *label* of the third class
   differs. That header is corrected in the same commit; the rule that genuinely
   differs is `effective_case_status`, a case-level severity pick.

---

### Task 22: Heatmap and trend

**Files:** modify `domain/analytics/{aggregates.rs,aggregates_tests.rs}`

- [x] **Step 0: Verify against legacy**

`:1451` `build_heatmap`, `:1495` `build_trend`, plus their two helpers `:2077` `recent_days` and `:2084` `clamp_days`. Record the day-window defaults and clamps for **both** `days_heatmap` and `days_trend` — they are separate parameters and may clamp differently. Record what a day with no runs renders as in each.

- [x] **Step 1: Write the failing tests**

The two clamps are **different**, which is the single most likely thing to get wrong here — `build_heatmap:1452` clamps to `[1, 30]` and `build_trend:1496` clamps to `[7, 365]`.

```rust
/// Two windows, two clamps. `build_heatmap:1452` is `clamp_days(days, 1, 30)`;
/// `build_trend:1496` is `clamp_days(days, 7, 365)`. Sharing one clamp silently
/// changes both charts.
#[test]
fn the_heatmap_and_trend_windows_clamp_differently() {
    assert_eq!(heatmap_days(0), 1);
    assert_eq!(heatmap_days(90), 30);
    assert_eq!(trend_days(1), 7);
    assert_eq!(trend_days(1000), 365);
}

/// A cell with no run for that day renders the literal `NOT_RUN`
/// (`build_heatmap:1481`, the `unwrap_or_else`), not an empty string and not a
/// missing element — the row's length must equal the day count.
#[test]
fn a_day_without_a_run_renders_not_run() {
    let heat = build_heatmap(&[universe_test("tests/a.py")], &[], 3);
    assert_eq!(heat.days.len(), 3);
    assert_eq!(heat.rows[0].values, vec!["NOT_RUN", "NOT_RUN", "NOT_RUN"]);
}

/// `bucketize_status:1940` collapses to three values: PASSED; FAILED (with ERROR
/// folded in); NOT_RUN for **everything else, including SKIPPED**. A skipped
/// test reads as not-run on both charts, which is legacy behavior and not a bug
/// to fix here.
#[test]
fn skipped_buckets_as_not_run() {
    assert_eq!(bucketize_status("PASSED"), "PASSED");
    assert_eq!(bucketize_status("ERROR"), "FAILED");
    assert_eq!(bucketize_status("SKIPPED"), "NOT_RUN");
    assert_eq!(bucketize_status("XFAIL"), "NOT_RUN");
}

/// `build_heatmap:1462` uses `or_insert_with`, so for a given (file, day) the
/// **first row encountered wins** — not the worst status, and not the latest.
/// Row order therefore matters, and a reordering of the input changes the chart.
#[test]
fn the_first_row_for_a_file_and_day_wins() {
    let rows = vec![exec_row("tests/a.py", TODAY, "PASSED"), exec_row("tests/a.py", TODAY, "FAILED")];
    let heat = build_heatmap(&[universe_test("tests/a.py")], &rows, 1);
    assert_eq!(heat.rows[0].values, vec!["PASSED"]);
}

/// The trend counts over the **whole universe** each day, so a test that never
/// ran still contributes to `not_run` (`build_trend:1519-1526`). Totals per
/// point therefore always equal the universe size.
#[test]
fn every_trend_point_totals_the_universe_size() {
    let universe = vec![universe_test("tests/a.py"), universe_test("tests/b.py")];
    let trend = build_trend(&universe, &[exec_row("tests/a.py", TODAY, "PASSED")], 7);
    let point = trend.points.last().expect("today");
    assert_eq!(point.passed + point.failed + point.not_run, 2);
    assert_eq!(point.passed, 1);
    assert_eq!(point.not_run, 1);
}
```

- [x] **Step 2: Introduce the `Clock` port**

Every test above depends on "today": `recent_days:2077` anchors its window on `Utc::now().date_naive()`, and `build_flaky:1657` computes its cutoff the same way. A core that calls the system clock directly cannot be tested deterministically — the heatmap tests would pass on 2026-08-18 and fail on a date boundary.

Create `domain/ports/clock.rs`:

```rust
/// Today, as the analytics cores see it.
///
/// A port rather than a direct `OffsetDateTime::now_utc()` because every window
/// in `domain::analytics` is anchored on it (`recent_days:2077`,
/// `build_flaky:1657`), and a core that reads the system clock cannot be pinned
/// by a test. The production adapter is one line; the test adapter is a
/// constant, and that is what makes the heatmap and flaky assertions stable.
pub trait Clock: Send + Sync {
    fn today(&self) -> Date;
}
```

Thread it into the aggregate functions as a parameter, not as struct state — the cores stay pure functions and the parameter is the only thing that makes them so.

- [x] **Step 3: Run them and watch them fail**

Run: `cargo test -p qa-insights heatmap trend bucketize`
Expected: FAIL.

- [x] **Step 4: Implement, run, commit**

`recent_days:2077` produces the window **oldest-first, ending today** — `(0..days).map(|o| today - (days - 1 - o))`. Port that ordering; the chart's x-axis depends on it.

Run: `cargo test -p qa-insights heatmap trend bucketize`
Expected: PASS.

```bash
git commit -am "feat(qa-insights): heatmap and trend aggregates"
```

---

### Carried into Task 23

Four things, **all verified at legacy source by Task 22's review rather than relayed from a report**,
and the first will silently produce wrong numbers if ignored.

1. **Legacy has *two* flaky folds and they disagree on both classification and grain. Task 23 owns
   both and must not unify them.** `analytics.rs:1655` `build_flaky` folds into `StatusStats` with
   `build_stats_map`'s three-way split (arms at `:1668-1670`). `dashboard.rs:388` is
   `COUNT(*) FILTER (WHERE tr.status IN ('PASSED','FAILED','ERROR')) AS total` — ruling R5's
   **sixth** classification — and it groups by `tr.test_name, rr.plan_id` (`:392`), where the
   analytics fold keys on `test_file`. So the two differ in classification *and* in grain, which is
   the "only the legacy dashboard groups by `test_name`" rule surfacing for the third time.
2. **`build_flaky`'s window is the *trend* clamp, not a third one.** `:1656` is
   `clamp_days(trend_days, 7, 365)` fed from `query.days_trend` at `:783`, so call Task 22's
   `trend_days`, and take the cutoff as `recent_days(today, trend_days(days))[0]` — `:1657` is
   `Utc::now().date_naive() - Duration::days(trend_days - 1)`, which is exactly that.
3. **But `build_flaky`'s filter is one-sided**: `analytics.rs:1661-1663` is
   `if row.day < cutoff { continue }`, so it admits rows dated *after* today, where Task 22's chart
   folds use a closed `HashSet` window. Implement the cutoff as `>= cutoff`, **not** as
   `window.contains(row.day)` — the two differ for any row with a future date, and a clock skew
   between the runner and this gear is enough to produce one.
4. **Task 23 inherits `run_created_at`** with the sixth classification: legacy's flaky and
   quality-vector windows (`:391`, `:490`) coalesce the same way over seven days, and Task 21b's
   ruling C is what makes the gear's own reads agree with that.

Carried in from earlier tasks and still open for this one: **platform ids are `Uuid` here and were
display names in legacy** (`ExecRow::platform_id`'s header, and "Carried into the next tasks" item
4) — `build_grouped_summaries` puts `row.platform` straight into a `BTreeSet<String>` and returns it
as the UI-visible group key (`analytics.rs:1116-1127`), so grouped summaries regress to raw UUIDs
unless something resolves ids to names, once per distinct id and outside the per-row path.

### Task 23: Flaky, quality vectors, grouped summaries

**Files:** modify `domain/analytics/{aggregates.rs,aggregates_tests.rs}`

- [x] **Step 0: Verify against legacy**

* `:1655` `build_flaky` — the pass-rate formula, the minimum-executions threshold (if any), the ordering, and whether it is bounded to N results.
* `:1047` `build_quality_vector_summary`, and `:1980` `quality_vectors_cache` + `:2016` `build_quality_vectors_by_file` — **note the cache.** Record what it is keyed on and when it invalidates. The gear gets its vectors from qa-catalog per request, so the cache does not port directly; record what you are replacing it with and why the numbers are unaffected.
* `:1085` `build_grouped_summaries` and `:1148` `apply_universe_group_filter` — the three groupings and how `group_by`/`group_value` narrows the universe. Also `:1794` `infer_component_from_path`, the component fallback when TEST_META has none.
* `:2212` `group_map_to_vec` and `:2225` `accumulate_group` — the tuple layout and accumulation order.
* **Carried into this task out of Task 18** (carried item 13): this task owns `DashboardStats::flaky_tests` and `DashboardStats::quality_vectors_pass_rate`, which `GET /qa/v1/dashboard` omits from the wire rather than reporting as zeros. Filling them means adding two keys to `api::rest::dto::DashboardStatsDto` and its `From` impl, not correcting two values. The traceability row's "Task 25" for these is the *assembler*, not their source.

- [x] **Step 1: Write the failing tests**

```rust
/// `infer_component_from_path:1794` is the fallback when TEST_META declares no
/// component: it matches the shape `tests/<component>/...` and takes the second
/// path segment. Anything else — a file at the root, or a tree not rooted at
/// `tests/` — infers nothing.
#[test]
fn the_component_falls_back_to_the_second_path_segment_under_tests() {
    assert_eq!(infer_component_from_path("tests/cluster/test_a.py").as_deref(), Some("cluster"));
    assert_eq!(infer_component_from_path("tests/test_a.py").as_deref(), Some("test_a.py"),
        "the second segment is taken verbatim, even when it is the file");
    assert_eq!(infer_component_from_path("suite/cluster/test_a.py"), None);
    assert_eq!(infer_component_from_path("test_a.py"), None);
}

/// `build_flaky:1655`, all four gates in one place:
///
/// * the window is `trend_days` clamped to `[7, 365]`, cutoff `today - (days-1)`;
/// * a test needs **at least 5 executions** (`:1690`, `if executions < 5`);
/// * its pass rate must fall **inside `[40.0, 80.0]` inclusive** (`:1694`) —
///   a test that always fails is broken, not flaky, and is deliberately excluded;
/// * `pct:1957` rounds to one decimal: `((v/total)*1000).round()/10`.
#[test]
fn flaky_requires_five_executions_and_a_pass_rate_inside_the_band() {
    // 3 of 5 passed = 60.0% — inside the band, at the execution floor.
    let flaky = build_flaky(
        &[universe_test("tests/a.py")],
        &exec_rows("tests/a.py", &["PASSED", "PASSED", "PASSED", "FAILED", "FAILED"]),
        7,
    );
    assert_eq!(flaky.len(), 1);
    assert!((flaky[0].pass_rate - 60.0).abs() < f64::EPSILON);
    assert_eq!(flaky[0].executions, 5);

    // 2 of 4 passed = 50.0%, inside the band but one execution short.
    let too_few = build_flaky(
        &[universe_test("tests/a.py")],
        &exec_rows("tests/a.py", &["PASSED", "PASSED", "FAILED", "FAILED"]),
        7,
    );
    assert!(too_few.is_empty(), "four executions is below the floor of five");

    // 0 of 5 passed = 0.0% — outside the band. Consistently broken, not flaky.
    let always_failing = build_flaky(
        &[universe_test("tests/a.py")],
        &exec_rows("tests/a.py", &["FAILED"; 5]),
        7,
    );
    assert!(always_failing.is_empty(), "0% is outside [40, 80]");
}

/// `:1704-1711`: ascending pass rate, then **descending** executions as the
/// tiebreak — the flakiest first, and among equally flaky ones the
/// best-evidenced first.
#[test]
fn flaky_sorts_by_pass_rate_then_by_evidence() {
    let flaky = build_flaky(&two_flaky_tests_universe(), &two_flaky_tests_rows(), 7);
    assert!(flaky[0].pass_rate <= flaky[1].pass_rate);
}

/// `:1697` — a test with rows but no universe entry is dropped. Rows outlive
/// deleted test files, and a flaky list naming files that no longer exist is
/// noise.
#[test]
fn a_test_absent_from_the_universe_is_not_reported_flaky() {
    let flaky = build_flaky(&[], &exec_rows("tests/gone.py", &["PASSED", "FAILED", "PASSED", "FAILED", "PASSED"]), 7);
    assert!(flaky.is_empty());
}

/// `:1668-1674`: PASSED counts as a pass, FAILED **and ERROR** as a fail, and
/// everything else — SKIPPED, XFAIL, XPASS — as skipped. All three feed the
/// execution count, so a mostly-skipped test can reach the floor.
#[test]
fn flaky_folds_error_into_fail_and_everything_else_into_skipped() {
    let flaky = build_flaky(
        &[universe_test("tests/a.py")],
        &exec_rows("tests/a.py", &["PASSED", "PASSED", "ERROR", "SKIPPED", "XFAIL"]),
        7,
    );
    assert_eq!(flaky.len(), 1);
    assert_eq!(flaky[0].pass_count, 2);
    assert_eq!(flaky[0].fail_count, 1);
    assert_eq!(flaky[0].skipped_count, 2);
    assert!((flaky[0].pass_rate - 40.0).abs() < f64::EPSILON, "2/5 = 40.0, the band's lower edge");
}
```

Plus one per remaining rule.

- [x] **Step 2–3: Run, implement, run, commit**

Run: `cargo test -p qa-insights flaky quality_vector grouped`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): flaky, quality-vector and grouped aggregates"
```

---

### Task 24: Build distribution and build-tests — ✅ DONE

**Files (as shipped, corrected under ruling R10):** modify `domain/analytics/{aggregates.rs,aggregates_tests.rs,universe.rs,universe_tests.rs,mod.rs}` and `domain/service/{ingest.rs,dashboard.rs}` (doc registers). **No REST tier** — the line below said `create api/rest/{handlers,routes}/analytics.rs` and that was unservable; **Task 25 creates those files.**

> **Five controller rulings settled this task; they are the record of what shipped, and Task 25
> inherits all five.**
>
> * **R10 — pure folds only, no REST tier.** Legacy's `api_build_tests` (`:369-455`) calls
>   `normalize_overview_query` (`:2392`), `load_universe_and_rows` (`:744`) and
>   `apply_universe_group_filter` before reaching these folds; all three are Task 25's, so a route
>   here could only stub or fail open — which Ruling R1 already settled ("a route that cannot serve
>   is a false green"). Task 25 creates the REST tier and registers **both** endpoints.
> * **R11 — the five deliverables**, and the plan's own Step 2–3 run line
>   (`cargo test -p qa-insights build_distribution build_tests`) is **not valid cargo**: one
>   positional `TESTNAME` only, so the filters must follow `--`. The Step 1 tests it gave covered
>   only `compare_build_desc`, so that filter matched nothing it asked for.
>   **And R11's stated reason was wrong**: `compare_build_desc` and `sorted_versions_desc`' inline
>   comparator **agree** on the plan's whole first test. They diverge two ways — when the longer
>   label's extra segment sorts below `"0"`, and via `sorted_versions_desc`' extra arm (`:2178`)
>   firing on segments that are numerically equal but textually different (`"2024.01.20"` vs
>   `"2024.1.15"`). Both functions ship; neither is a refactor of the other.
> * **R12 — the run identity is a `Uuid`.** Legacy's `workflow_name: String` has no counterpart
>   here, so the snapshot, the distribution and the detail item carry `run_id`/`latest_run_id`,
>   following `LatestInfo::run_id`'s precedent. **Task 25's DTO owns the label**, exactly as it owns
>   `platform_id`'s.
> * **R13 — the `"unknown"` fallback, and a seventh classification.**
>   `latest_per_test_snapshot`'s status mapping (`:1633-1639`) passes an unrecognized status through
>   **verbatim** and is a fourth distinct classification, absent from `domain::service::ingest`'s R5
>   table. It is now row seven there.
> * **R14/R15 — determinism, and the gap the doc hid.** The snapshots are returned in a
>   deterministic order (legacy's `latest.into_values()` is arbitrary and its order is observable
>   twice: the distribution's strictly-`>` `latest_run_id` scan, and `build_test_details`' stable
>   `sort_by`). R15: the collapse now lives in `domain::analytics::universe` as `collapse_build` and
>   applies at **both** readers of `ExecRow::build`, closing the `last_build` parity gap.

- [x] **Step 0: Verify against legacy**

`:1536` `build_last_run_build_distribution`, `:1615` `latest_per_test_snapshot`, `:2164` `sorted_versions_desc`, `:2188` `compare_build_desc`, and the endpoint `:369` `api_build_tests` with its query type at `:50-60`.

`compare_build_desc:2188` is a custom ordering: dot-split, then per-segment **descending numeric** compare, with a missing segment defaulting to `"0"` and any non-numeric pair falling back to a **reverse string** compare; the final tiebreak is a reverse compare of the whole string. Confirm that reading, then pin it.

- [x] **Step 1: Write the failing tests**

```rust
/// `compare_build_desc:2188`. Newest first, segment by segment, with a missing
/// segment reading as `0` — so `1.2` sorts after `1.2.1`, not before it.
#[test]
fn builds_sort_newest_first_by_numeric_segment() {
    let mut builds = vec!["1.2.1".to_owned(), "1.10.0".to_owned(), "1.2".to_owned(), "1.9.9".to_owned()];
    builds.sort_by(|a, b| compare_build_desc(a, b));
    assert_eq!(builds, vec!["1.10.0", "1.9.9", "1.2.1", "1.2"]);
}

/// A non-numeric segment falls back to a reverse **string** compare, not to an
/// error and not to zero (`:2199-2202`, the `_ =>` arm).
#[test]
fn a_non_numeric_segment_falls_back_to_reverse_string_order() {
    assert_eq!(compare_build_desc("1.rc2", "1.rc1"), std::cmp::Ordering::Less);
}

/// Equal through every segment falls through to the whole-string reverse
/// compare at `:2209`.
#[test]
fn identical_builds_compare_equal() {
    assert_eq!(compare_build_desc("2.0.0", "2.0.0"), std::cmp::Ordering::Equal);
}
```

- [x] **Step 2–3: Run, implement, commit**

Register `GET /qa/v1/analytics/build-tests` with legacy's parameter set verbatim (`product_id`, `version`, `scope`, `plan_id`, `branch`, `group_by`, `group_value`, `build`) per D7.

Run: `cargo test -p qa-insights build_distribution build_tests`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): build distribution and build-tests endpoint"
```

---

### Carried into Task 25

Three things landed on this task after the plan was written, two of them reassignments made because
Task 23 found this task is the first that can own them. All three are prerequisites for the endpoint
serving anything at all, so read them before Step 0.

1. **The production `CatalogReader` adapter, pulled forward from Task 40.** Verified 2026-08-21:
   `grep -rn "impl CatalogReader"` over the gear returns exactly **one** hit,
   `domain::service::test_support`'s `FakeCatalog`. Nothing in production implements the port, and
   `domain/ports/catalog_reader.rs`' own header still names Task 40 as the owner. **That was never
   viable**: this task issues the first real `list_universe` read, so it is blocked without the
   adapter, and Task 40 comes fifteen tasks later. Build it here — a `QaCatalogClientV1`-backed
   adapter beside `infra/clients/qa_runs.rs`, which is the shape to copy. Task 40's scope shrinks
   accordingly (it keeps the consumer start, the tickers, the elector, the oagw client and the local
   client registration).
2. **`DashboardStats::quality_vectors_pass_rate`, reassigned from Task 23** for the same reason — it
   is computed from the catalog's vectors, so it could only ever have been non-empty under test
   before item 1 exists. Legacy is `dashboard.rs:483-494`, and it uses ruling R5's **sixth**
   classification (the `PASSED`/`FAILED`/`ERROR` denominator at `:487`) windowed at `:490` over seven
   days. It is **absent from the wire**, not zero, so filling it means adding a key to
   `DashboardStatsDto` and its `From` impl.
3. **A `PlatformReader` port over qa-environments — unowned before Task 23 raised it, now this
   task's.** `ExecRow::platform_id` is a `Uuid` where legacy's `platform` was a display *name*, and
   `build_grouped_summaries` returns the platform as a UI-visible group key. Task 23 typed its
   output `PlatformGroupSummary { platform_id: Uuid, .. }` **deliberately, so that no DTO can be
   written over it without deciding what the label is** — the gap fails at the DTO boundary rather
   than rendering a plausible-looking UUID. One method (`names(ids) -> map`), resolved once per
   distinct id and outside the per-row path. **Note the consequence:** Task 23's list is ordered by
   id and legacy's is ordered by name, so resolving the names **will reorder the list** — that is
   the correct direction, and it is a rendered change to expect rather than a regression to hunt.

**And one thing Task 23's review corrected about legacy's own pipeline, because this task assembles
it and would otherwise get it backwards.** `apply_universe_group_filter` does **not** narrow
everything downstream of it. Verified at source: `build_grouped_summaries(&universe, &all_rows)` runs
at `analytics.rs:747`, **before** the filter call at `:749-750`, so the group chart is computed over
the *unfiltered* universe and rows; the quality-vector map is built inside `load_universe_and_rows`
(`:744-745`, fold at `:871-882`) and is never narrowed either. Only `build_latest_map`,
`build_stats_map`, the summary, the lists, the heatmap, the trend, the build distribution and
`build_flaky` consume `filtered_universe` + `rows_for_scope` (`:751-783`). Assembling it the other
way collapses the group chart to a single bar and shrinks `total_tests` to the selection.
`domain::analytics::aggregates`' module header carries the same table.

### Task 25: The overview endpoint

**Files (corrected under ruling R10):** create `domain/service/{analytics.rs,analytics_tests.rs}`, **create** `api/rest/{handlers,routes}/analytics.rs` (Task 24 was to have created them and could not), the production `CatalogReader` adapter under `infra/clients/`, and a `PlatformReader` port + adapter; modify `api/rest/{mod.rs,dto.rs,routes/mod.rs,handlers/mod.rs}` and `domain/service/dashboard.rs`.

> **Three controller rulings bind this task before Step 0.**
>
> * **R10 — this task registers BOTH `/qa/v1/analytics/overview` AND
>   `/qa/v1/analytics/build-tests`**, and creates the analytics REST tier. Task 24's folds are ready
>   and pure: `build_last_run_build_distribution` and
>   `build_test_details(universe, rows, build)` in `domain::analytics::aggregates`. `build-tests`
>   takes legacy's parameter set verbatim (`product_id`, `version`, `scope`, `plan_id`, `branch`,
>   `group_by`, `group_value`, `build`) per D7, and legacy 400s on an empty `build` (`:373-376`).
> * **R17 — the Step 1 test below contains an assertion that CANNOT FAIL.**
>   `assert_eq!(response.grouped.component.len(), response.grouped.component.len())` compares a
>   value to itself. Keep the test's name and its other five assertions; replace that line with one
>   that can fail — the grouped section's three keys are present and its component bars sum to the
>   **unfiltered** universe's total, which is the invariant the pipeline-order paragraph in
>   `### Carried into Task 25` says is easy to get backwards.
> * **R18 — this task was SPLIT**, as Task 21 was, and **25a is done** (see
>   `### What 25b inherits from 25a` below). **25a** was the two ports/adapters (`CatalogReader`,
>   `PlatformReader`) + `DashboardStats::quality_vectors_pass_rate` + the query parsers and their
>   status codes — everything with no pipeline in it. **25b, which is what remains**, is the service
>   assembly in legacy's exact pipeline order, the analytics REST tier and both endpoints. The
>   pipeline order is the highest-risk thing left in the phase and that is why it gets a review seat
>   that is not also reviewing an SDK adapter.
>
> **Also inherited from Task 24:** `UNKNOWN_BUILD` and `collapse_build` live at
> `domain::analytics::universe`; the DTO owns rendering `run_id` → a run name, `platform_id` → a
> platform name, and every instant → RFC-3339.

### What 25b inherits from 25a

**25a is complete and reviewed** (`2fa8bb39..e2d0cba1`, six commits, +42 tests, gate above). It
shipped the production `CatalogReader` adapter, a `PlatformReader` port and its `QaEnvironmentsReader`
adapter, `DashboardStats::quality_vectors_pass_rate`, and all six query-parser rejections with their
status codes. Four things it hands forward, and the first is the one that will bite:

1. **A boot failure nothing in the test suite catches, and 25b owes it in one commit.** 25a added
   `qa-environments-sdk` to `qa-insights/Cargo.toml` and built `QaEnvironmentsReader`, but the
   **`qa_environments` `deps` token is not declared** — `gear.rs`' `deps = [authz_resolver, qa_runs,
   qa_catalog, oagw, event_broker, cluster]` has no entry for it, there is no qa-environments *gear*
   crate dependency, and no `ClientHub::get::<dyn QaEnvironmentsClientV1>()`. `QaEnvironmentsReader`
   is constructed only inside its own test module, so **nothing compiles or tests the gap**: it
   surfaces at boot, not at build. The important non-risk, verified: `qa_catalog` *was* already in
   `deps` from the gear skeleton, so 25a's new catalog lookup is safe as shipped — the hazard is
   entirely 25b's.
2. **The parsers are done, and they are `domain::analytics::query`.** Six rejections, all 400, ported
   line-for-line in legacy's own check order (`:2395 → :2403 → :2408 → :2409 → :2413`, plus
   `api_build_tests`' empty-`build` check at `:373-376` with the message `"build is required"`).
   `the_first_failing_rule_is_the_one_reported` pins the order. 25b maps them onto HTTP, which is the
   half 25a deliberately did not do.
3. **A deliberate divergence 25b will face again, unconditionally.** A qa-catalog failure or
   `Forbidden` now fails the **whole** dashboard, where legacy warns and continues
   (`dashboard.rs:539`) — because an empty array is indistinguishable from a measured empty window.
   On the dashboard that 403 is *conditional on data*: the ported short-circuit (`dashboard.rs:498-500`)
   means the catalog is only read once some file forms a group, so one `SKIPPED` row is enough to flip
   a caller lacking the grant from 200 to 403. **The overview has no such gate, so 25b's version of
   the same divergence is unconditional.** Decide it deliberately and document it at the code.
4. **A legacy defect ported verbatim and pinned, which 25b must not "fix".** Legacy's *dashboard*
   quality-vector fold keys its aggregate on the **display** string (`dashboard.rs:520`) while its
   *analytics* fold case-folds across files (`analytics.rs:1060-1062`). So `Security` and `security`
   declared in two files are **two dashboard rows and one overview count**. Both sides are reproduced
   and both are pinned; unifying them is a one-character change that no other test catches.

Two things 25a corrected in this document's own forecasts, so they are not re-litigated: the
`CatalogReader` adapter is no longer Task 40's (that claim survived in `Cargo.toml` a whole round
after the ledger paragraph was fixed), and `domain/ports/mod.rs` now records `platform_reader` as the
third and most extreme exception to its own "only ports something already calls are declared" rule —
25b is its first caller.

- [x] **Step 0: Verify against legacy**

`:360` `api_overview`, `:728` `build_overview`, `:2392` `normalize_overview_query`, `:2104` `parse_scope`, `:2115` `parse_group`. Record the validation errors each parser returns and their status codes — a 400 that legacy returns and this gear does not is a behavior change the SPA will notice.

**Two carried items land here, both out of Task 20 (reassigned 2026-08-21).**

* **Carried item 1, the product/version mapping.** Legacy's all-scope predicate is `r.product_key = $2 OR (r.product_key IS NULL AND r.plan_id = ANY($3))` (`:992-995`, the disjunction itself at `:993-994`), and **this schema has neither column**: VHP-319 deleted the product-version model and a plan's identity here is `(repo_id, plan_path)`. `domain::analytics::UniverseFilter` therefore ships three settled predicates and no product mapping. This task is the first to turn a request into a read, so it is the first that has to answer it — and `UniverseFilter`'s own header names this task as the owner. Do not extend the struct without a call site for the new field.
* **Carried item 5, the `since` bound.** `ResultsRepository::list_for_universe` returns an unaggregated `Vec<ExecRow>` and is the read that meets `cpt-cf-qa-nfr-scale`'s 5M rows. Every window in the overview is clamped *inside* the pure folds (heatmap `[1,30]`, trend `[7,365]`, the flaky cutoff), so the bound this task must pass is the **widest** of them; `UniverseFilter::since`'s doc carries the index subtlety (`finished_only` is what makes the bare `run_finished_at` column usable, and no index covers the `COALESCE`).

- [x] **Step 1: Write the failing test**

```rust
/// The overview is eight computed sections in one payload
/// (`AnalyticsOverviewResponse`, `manager/src/routes/analytics.rs:231-248`) —
/// `summary`, `lists`, `heatmap`, `trend`, `build_distribution`, `flaky`,
/// `quality_vectors`, `grouped`. The other eight fields on that struct echo the
/// query back and are not computed. Shipping seven of the eight looks complete
/// in a screenshot and is not.
#[tokio::test]
async fn the_overview_returns_all_eight_sections() {
    let f = fixture_with_seeded_results().await;
    let response = f.service.overview(&f.ctx, overview_query()).await.expect("overview");

    assert!(response.summary.total > 0);
    assert!(!response.lists.passed.is_empty() || !response.lists.failed.is_empty() || !response.lists.not_run.is_empty());
    assert!(!response.heatmap.days.is_empty());
    assert!(!response.trend.points.is_empty());
    assert!(!response.build_distribution.is_empty());
    // flaky, quality_vectors, grouped may legitimately be empty on this
    // fixture; assert they are present and well-formed rather than non-empty.
    assert_eq!(response.grouped.component.len(), response.grouped.component.len());
    assert!(response.quality_vectors.total_tests >= response.quality_vectors.unclassified_tests);
}
```

- [x] **Step 2–3: Run, implement, run, commit**

Register `GET /qa/v1/analytics/overview` with legacy's nine parameters.

Run: `cargo test -p qa-insights the_overview_returns_all_eight_sections`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): the eight-section analytics overview"
```

---

### What Tasks 26–30 inherit from Task 25b

**Task 25b is complete and reviewed** (`e7242119..0d8ac1e8`, three commits, +41 tests, gate above).
It shipped `domain::service::analytics` — `AnalyticsService::{overview, build_tests}` — the analytics
REST tier at `api/rest/{handlers,routes}/analytics.rs`, and both endpoints. Task 26's export reads
`AnalyticsOverview` section by section, so it inherits every decision below.

**Four decisions a human still owns.** All four were taken by the controller so execution could
continue; each is documented at the code, and each is cheap to reverse now and expensive later.

1. **The read window reaches sections legacy does not window** (ruling R21, and the one with real
   product consequences). `list_for_universe` is bounded by the **widest** of the folds' internally
   clamped windows — defaulting to 90 days — because carried item 5 made it the read that must meet
   `cpt-cf-qa-nfr-scale`'s 5M rows. But legacy reads **all of history** for the summary, the lists,
   `pass_count`/`fail_count`/`total_runs`, the build distribution and the group bars, so a test whose
   only run predates the window now reads `NOT_RUN` where legacy shows its result. `days_trend` is
   the knob that widens it. **If this is wrong, the fix is a second, wider bound for the unwindowed
   sections — a service-layer change with no schema and no wire change.** Task 26 exports exactly
   these sections, so it inherits the divergence verbatim; do not "fix" it there.
2. **`/analytics/build-tests` inherits the overview's default window** (ruling R22) — always 90 days,
   because `BuildTestsQuery::into_overview` hard-codes the day counts rather than accepting
   `days_trend`, which legacy's parameter set does not have (D7). A build older than 90 days is
   therefore unreachable through the drill-down, and an overview served with `?days_trend=365` can
   draw a bar the drill-down cannot fully open. **The endpoint description states this mismatch
   outright** rather than claiming the two agree — the first review caught the original claim that
   they did. R21 and R22 stand or fall together.
3. **An unknown product is `200` with zeros, not legacy's `404`** (ruling R23). Legacy resolves
   `product_id` through a product registry (`:742`, `:401`) that this subsystem does not have, and
   `CatalogReader::list_universe` is deliberately not an existence oracle. The split shipped is:
   **malformed** id → `400 product_id must be a UUID` (a seventh rejection, and the only one not in
   `domain::analytics::query`, which keeps the field a `String` precisely to preserve rejection #1);
   **well-formed but unknown** → an empty universe → `200` of zeros. Adding the `404` later is
   additive and breaks no client.
4. **Both upstream `403`s fail the whole overview, unconditionally** (ruling R24, and brief item 3
   instructed 25b to decide it deliberately). A qa-catalog *or* qa-environments failure or `Forbidden`
   fails the entire payload rather than rendering a `200` whose zeros cannot be distinguished from a
   measured empty window — the universe is the denominator of every number in the response. The
   dashboard's version of this divergence is *conditional* on data; the overview's is not. A third,
   smaller one rides along: legacy's `attach_case_data` warns and continues on a failed per-case
   query (`:1326-1329`) and this gear propagates, because it is our own database.

**Three things 25b established in code that later tasks should use rather than rebuild.**

* **`ResultsRepository::case_rows_for_runs`** (ruling R20) — added outside the plan's file list
  because `CaseRow`'s own header assigned it here and `build_case_data(u, l, &[])` returns not zeros
  but a *synthetic fallback*, so without it the six per-case counters and every list item's
  `case_status`/`case_tickets` would have shipped wrong-but-plausible, permanently.
* **The universe is narrowed by plan but NOT by group above the filter.** `build_quality_vector_summary`
  and `build_grouped_summaries` run over the unfiltered set; everything else consumes the filtered
  one. `domain::service::analytics`' header carries the table, verified at legacy `:744-783`.
* **`case_expected` WAS `0` on the wire; Task 29 landed it.** It is now a real per-file precedence
  fold (`domain::analytics::universe::expected_cases`) — the collect job's exact count wins per
  `(repo_id, test_file)`, `UniverseTest::static_case_count` is the fallback. The deferral was
  two-part and both parts are discharged. **The CSV export still excludes it deliberately** (Task
  26's seven-metric `summary` block), and that pin was re-verified against the now-non-zero value.

**One correction to legacy's own behaviour, found by 25b's review and worth not re-deriving:**
legacy's synthetic-case fallback at `:1371-1376` has a `SKIPPED` arm that is **dead code** —
`bucketize_status` (`:1940-1946`) can only yield `PASSED`, `FAILED` or `NOT_RUN`. A `SKIPPED` file
contributes no case at all.

**And one process finding that has now fired twice on this plan, in two files, by two different
agents.** A patch script that **raises on a failed assertion and writes nothing**, followed by a
commit that succeeds anyway, silently ships a partial edit: 25b's implementer had two of three
`dto.rs` batches abort and read the third's success plus a green gate as the whole set — which is how
nine wrong citations survived the commit whose purpose was fixing citations. **The countermeasure is
not checking the script's exit code; it is re-grepping the file for what you claim to have written.**
Two mechanical checks caught six of the nine and cost one pass: citations into one block of struct
declarations must be **disjoint and monotonic** (two of them overlapped, which is impossible on its
face), and **any two citations of the same symbol must agree** (two symbols were each cited twice
with different numbers, one of them correctly in the same diff).

---

### Task 26: Export

**Files:** create `domain/analytics/{export.rs,export_tests.rs}`; modify `api/rest/{handlers,routes}/analytics.rs`

- [x] **Step 0: Verify against legacy**

`:457` `api_export` with its query at `:35-48`, `:2234` `overview_section_json`, `:2262` `overview_to_csv`, `:2369` `csv_escape`. Record the `section` vocabulary (which sections are exportable), the `format` vocabulary, and `csv_escape`'s exact quoting rule.

- [x] **Step 1: Write the failing test**

```rust
/// `csv_escape:2369`. Quote only when the value contains a comma, a double
/// quote or a newline; inside a quoted value, double the quotes. Note what is
/// **not** escaped: a carriage return alone, and a leading `=` — legacy does
/// neither, so neither does this. Port the blind spots.
#[test]
fn csv_escaping_matches_legacy_for_commas_quotes_and_newlines() {
    assert_eq!(csv_escape("plain"), "plain");
    assert_eq!(csv_escape("a,b"), "\"a,b\"");
    assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
    assert_eq!(csv_escape("trailing\r"), "trailing\r", "a bare CR is not a trigger in legacy");
}
```

- [x] **Step 2–3: Run, implement, run, commit**

Register `GET /qa/v1/analytics/export`.

Run: `cargo test -p qa-insights export csv`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): analytics export"
```

---

### What Task 26 found in legacy's export

Task 26's Step 0 recorded four things about `api_export` that this plan did not know and that its
review then confirmed at source. All four are legacy's own behaviour, all four are ported verbatim
and pinned by tests, and **none of them is a defect for a later task to fix.**

1. **The `section` vocabulary is exactly six spellings** — `summary`, `lists`, `heatmap`, `trend`,
   `flaky`, `all` (`overview_section_json`'s match at `:2238-2250`; `overview_to_csv`'s `include`
   closure at `:2265`). **Three of the overview's eight sections have no section name of their own**:
   `build_distribution`, `quality_vectors` and `grouped` are reachable **only** through `section=all`.
   Legacy's gap, deliberately not filled — adding the three arms would invent a vocabulary legacy
   never had.
2. **There is no `format` vocabulary at all.** Legacy normalizes `format` and `section` identically
   (trim, lower-case, default) but validates only `section`; `format`'s only test is a bare
   `if format == "csv"` at `:488` with no `else if` and no rejection, so `"xml"`, a typo or an absent
   value all fall through to JSON. Ported as a boolean with no error path.
3. **The two branches disagree about an unrecognized `section`.** The JSON branch validates and
   `400`s; the CSV branch never validates, so its `include` closure matches nothing and it returns
   `200` with an **empty body**. `?section=bogus` is a 400 and `?section=bogus&format=csv` is a 200 of
   nothing. Pinned rather than unified.
4. **The CSV `summary` block exports only seven metrics** — `total`, `passed`, `failed`, `not_run`
   and three percentages — and never the six per-case counters or `case_expected` that the JSON
   `summary` section renders. Pinned by a test that seeds non-zero counters specifically to prove
   they never reach the CSV.

**One open question for the human, raised by Task 26 and left open deliberately (ruling R31).**
`csv_escape` quotes on a comma, a double quote or a newline and nothing else, so a value beginning
`=`, `+`, `-` or `@` reaches a spreadsheet as a live formula. Legacy predates the mitigation, the
brief instructed that it not be hardened, and the blind spot is pinned by
`csv_escaping_does_not_guard_against_a_leading_formula_character`. **This is the first place in the
phase where preserving legacy has a plausible attacker**, and the fix — if the human wants it — is
one function and one test.

**One test-hygiene lesson worth applying to every remaining task.** Task 26 registered a route and
did not add it to `routes/mod.rs`' route-table test, **and the suite stayed green, because that test
asserted presence and not count**. It now asserts both, against the built document's own path count.
A guard that looks green while covering less than it claims is worse than no guard; when you add a
registered thing, check that the guard over that kind of thing actually fails without it.

---

### Task 27: The three plan drill-downs

**Files:** modify `domain/service/analytics.rs`, `api/rest/{handlers,routes}/analytics.rs`

- [x] **Step 0: Verify against legacy**

`:2434` `api_plan_tests`, `:2486` `api_plan_builds`, `:2530` `api_plan_test_history`. Read all three in full and record each one's parameters, response shape, ordering and any limit.

- [x] **Step 1–3: Test-first per endpoint, implement, commit**

Three endpoints, three tests minimum — one per response shape. Register `GET /qa/v1/analytics/plan/{plan_id}/tests`, `/builds`, `/test-history`.

Run: `cargo test -p qa-insights plan_tests plan_builds plan_test_history`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): plan drill-down endpoints"
```

---

### What Task 27 changed about plan identity on the wire

**Task 27 moved a wire contract, and Tasks 28–30 inherit the moved version.** The three plan
drill-downs are `GET /qa/v1/analytics/plan/{tests,builds,test-history}` with **`plan_id` as a
required query parameter** — not the path segment the brief implied and legacy uses.

**Why, because it will look like an unforced divergence otherwise.** Ruling R37 resolved `plan_id` on
these routes to `plan_path` alone: unlike the overview, these endpoints carry no product, no version
and no scope, so nothing in the request can fix the repository half. But **real `plan_path` values
contain slashes**, and a slash in a single-segment path parameter is a **router 404** — not the empty
array the endpoint documents. Legacy never hit this because `compose_repo_plan_id`
(`manager/src/services/plans.rs:789-801`) runs its id through `sanitize_k8s`, so legacy's `plan_id` is
guaranteed slash-free. Ours is not. Ruling **R41** moved it to a query parameter rather than
documenting `%2F`, because the overview already takes `?plan_id=`, because proxies routinely reject or
normalise `%2F`, and because the URL-shape change is an architectural adaptation following from the
same root cause as R37. **Nothing in the test suite caught this** — the endpoints were registered,
tested at the service tier, and 404'd for every real input.

**Three consequences for later tasks.**

1. **`plan_id` on the wire is `plan_path`, and it matches across every repository the caller's scope
   admits.** Legacy's slug composes both halves, so legacy keeps two same-path plans in different
   repositories apart and this gear merges them. This is wider than even the overview's case. The
   evidence it is the already-shipped contract: `AnalyticsListItemDto::plan_path`'s doc
   (`api/rest/dto.rs:1276-1278`) says `plan_path` is "the value `?scope=plan&plan_id=` takes" — though
   note the DTO ships `repo_id` too, so a client does receive both halves; the claim is about the
   token, not availability.
2. **The three drill-downs are windowed to 90 days where legacy reads all history** (ruling R38), and
   **there is no parameter that widens them** — they take `plan_id` and nothing else, so widening would
   mean inventing a parameter legacy does not have. This changes counter *semantics*, not just
   visibility: `total_runs`, the pass/fail counts and every build-distribution group are now 90-day
   quantities. **This is the sharpest edge of the read-window decision in the whole phase**, because a
   build distribution is exactly the view a user looks back past a quarter for.
3. **Ordering is `COALESCE(run_finished_at, run_created_at) DESC`**, not legacy's bare
   `finished_at DESC NULLS LAST` (ruling R39, extending Task 21b's ruling C) — and the window
   predicate that implements it must go through **`kpi_window(since, None)`**, never a bare
   `Expr::expr(effective_ts()).gte(...)`. The two are semantically identical, but only `kpi_window`'s
   two-bare-column form can use `idx_qa_test_results_tenant_finished`, and `qa_test_results` has **no
   index on `plan_path` at all**. Task 27 shipped the index-defeating form first, while citing the
   5M-row NFR as the reason the window existed.

**And one gap now raised in priority for the final review: this crate has no HTTP-boundary test
infrastructure at all.** Two consecutive tasks have wanted one, and Task 27's router-404 is exactly
the defect class it catches. Task 27's remedy tests `serde_urlencoded` directly, which its re-review
confirmed is byte-identical to axum's `Query` decode path — sufficient for that defect, not a
substitute for the missing tier.

---

### Task 28: Saved views

**Files:** create `domain/service/{saved_views.rs,saved_views_tests.rs}`, `api/rest/{handlers,routes}/saved_views.rs`

- [x] **Step 0: Verify against legacy**

`:521` `api_list_views`, `:576` `api_create_view`, `:641` `api_update_view`, `:703` `api_delete_view`, the request/response types at `:62-87`, the unique index at `001_initial.sql:194`, and `:2129` `analytics_owner_id`.

`analytics_owner_id` reads the owner from a **request header**. That does not port: this gear takes the owner from `SecurityContext`. Record the substitution and confirm it preserves the uniqueness semantics (per-owner, per-scope, per-plan names).

- [x] **Step 1: Write the failing tests**

```rust
/// The unique key is `(owner, scope, COALESCE(plan_id, ''), name)`
/// (`manager/migrations/001_initial.sql:194`). A global view and a plan-scoped
/// view may share a name; two global views may not.
#[tokio::test]
async fn a_global_and_a_plan_scoped_view_may_share_a_name() {
    let f = fixture().await;
    f.service.create(&f.ctx, view("Regressions", "global", None)).await.expect("global");
    f.service.create(&f.ctx, view("Regressions", "plan", Some(PLAN))).await.expect("plan-scoped");
}

#[tokio::test]
async fn two_global_views_may_not_share_a_name() {
    let f = fixture().await;
    f.service.create(&f.ctx, view("Regressions", "global", None)).await.expect("first");
    let err = f.service.create(&f.ctx, view("Regressions", "global", None)).await.unwrap_err();
    assert!(matches!(err, DomainError::SavedViewNameExists { .. }));
}

/// Views are per-owner. One user's view must not appear in another's list, and
/// must not block another's name.
#[tokio::test]
async fn views_are_scoped_to_their_owner() { /* two SecurityContexts, one tenant */ }
```

- [x] **Step 2–3: Run, implement, run, commit**

Register `GET/POST /qa/v1/analytics/views` and `PUT/DELETE /qa/v1/analytics/views/{id}`.

Run: `cargo test -p qa-insights saved_view`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): saved views"
```

---

### What Task 28 established about ownership and writes

Task 28 is the **first write path in Phase B** and the first endpoint set in this gear where one
caller can reach another's data. Four things it settled that Tasks 29–40 inherit — Phase C's JIRA
registry, notification configs and bug endpoints are all writes, so this is the section they start
from.

1. **The owner floor is `AccessScope::ensure_owner(ctx.subject_id())`, applied on top of whatever the
   PDP compiles**, and the mechanism matters: `ensure_owner`
   (`libs/toolkit-security/src/access_scope.rs:871-925`) intersects rather than replaces — deny-all
   stays deny-all, a tenant-only constraint gets the owner filter injected, a constraint naming a
   different owner is dropped, and **an *unconstrained* scope becomes a single `owner_id = subject`
   constraint**. That last case is what closes the create path, because `validate_insert_scope`'s
   fail-open early return is `scope.is_unconstrained()` (`libs/toolkit-db/src/secure/db_ops.rs:68`) —
   a state `ensure_owner` can no longer produce. The entity must declare `owner_col` for the filter to
   reach SQL. **The in-repo precedent is mini-chat** (`reaction_service.rs:68`, `:136`, and five
   sibling domain services), *not* usage-collector, which declares `OWNER_ID` but applies no
   narrowing.
2. **Legacy's owner model does not port, and the substitution is a security improvement, not a
   compromise.** `analytics_owner_id` (`manager/src/routes/analytics.rs:2129-2140`) reads
   `X-Analytics-Owner` — **unauthenticated caller-supplied free text**, so any client could read and
   write as any identity with no session binding. This gear uses `SecurityContext::subject_id()`,
   which authentication mints and a header cannot forge. Two recorded consequences: legacy's
   absent-header 400 has **no analogue** (a `subject_id` cannot be absent), and legacy's arbitrary
   string owner narrows to a `Uuid` (inert — there is no legacy database to migrate).
3. **Collisions are caught at the database, never pre-checked** (ruling R48). The repository's
   `create`/`update` map the unique violation atomically, so the catch *is* the statement's outcome;
   a `find_by_natural_key` probe would open a TOCTOU window for zero gain. `find_by_natural_key`
   exists and correctly goes unused. **But "atomic by construction" is only true of `create`**: the
   review established that `update` is **two scoped reads** — the repository's own
   (`saved_views_sea_repo.rs:148-155`) then `secure_update_with_scope`'s
   (`toolkit-db/src/secure/db_ops.rs:226-237`) — so a delete landing between them turns a clean 404
   into an opaque **500**. Rare, benign, inherited from Task 12, documented but **not fixed**:
   remapping `ScopeError::Denied` would touch a repository where that error legitimately means
   something else. **This is the case a two-connection Postgres test should assert if one is ever
   written**, and this crate still has no such test.
4. **A trap a literal port would have created** (ruling R47). Legacy stores a `plan_id` submitted
   alongside `scope=all` and it is inert there. Here it would not be: `plan_key` is materialized from
   `(repo_id, plan_path)` and **never from `scope`**, and `list`'s all-branch filters `plan_key = ""`
   — so the row would be created successfully and then be invisible in the very list meant to show
   it. The plan is dropped instead. **The general lesson for Phase C: wherever this gear materializes
   a legacy expression into a column, check every writer that can populate it inconsistently with the
   predicate that reads it.**

**Two wire divergences from legacy, both improvements, both to keep:** create returns **201** with a
`Location` header where legacy returns 200, and a name collision is **409** where legacy maps every
database error — unique violations included — to 400 with the driver's raw text.

**And one shape to be aware of rather than to change:** `/analytics/views` takes `repo_id` +
`plan_path`, while all six other analytics endpoints take a single `plan_id` query parameter matched
against the plan's path. The fields are right for their model (`qa_insights_sdk` note 1, and views
name one specific plan while the drill-downs match across repositories); the four route descriptions
now say so explicitly, because nothing else told a client the two spellings are the same plan.

---

### Task 29: Static expected-case counts

**Files:** modify `domain/analytics/universe.rs`, `domain/service/analytics.rs`

- [x] **Step 0: Verify against legacy**

`:1882` — where `count_test_functions` is called and how its result reaches `OverviewSummary::case_expected` (`:105-110`). Confirm that the static count is used when no exact collect count exists, and that `load_collect_counts:2672` supplies the exact one when it does.

> **Added 2026-08-24 under ruling R29, found by Task 25b's review.** The deferral of `case_expected`
> to this task is **wider than 25b's wire text originally admitted**, and the gap is not "no collect
> report" — it is two-part. Legacy `:769-779` uses the collect count **with
> `UniverseTest::case_count` as the per-file fallback**, at `.unwrap_or(t.case_count)` on `:777`, and
> **this gear already holds that value**: `qa_catalog_sdk::UniverseTest::static_case_count`
> (`qa-catalog-sdk/src/models.rs:431`), whose own doc cites `analytics.rs:767-779` and says the
> collect count "wins over this one where it exists". So legacy renders a **non-zero** expected count
> on a deployment that has never run a collect, where `/analytics/overview` currently renders `0`.
> **The cheaper half of this task is therefore the fallback, not the collect read** — it needs no
> new table and no new port, only the value already crossing `CatalogReader`. 25b corrected its own
> docs to say so (`api/rest/dto.rs`' `case_expected`, and both endpoint descriptions); do not read
> those as saying the fallback is unavailable.

- [x] **Step 1: Write the failing test**

```rust
/// Exact beats static. The static count does not expand parametrize
/// (`manager/src/routes/analytics.rs:105-108`), so where the collect job has
/// run, its number is the truthful one.
#[test]
fn an_exact_collect_count_overrides_the_static_count() {
    let expected = expected_cases(
        &[universe_test("tests/a.py", 3)],
        &[collect_count("tests/a.py", 11)],
    );
    assert_eq!(expected, 11);
}

#[test]
fn the_static_count_is_used_where_no_collect_count_exists() {
    let expected = expected_cases(&[universe_test("tests/a.py", 3)], &[]);
    assert_eq!(expected, 3);
}
```

- [x] **Step 2–3: Run, implement, run, commit**

Run: `cargo test -p qa-insights expected_cases`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): static expected-case counts"
```

---

### What Task 29 landed, and the two things it left open

**`case_expected` is a real number now.** The fold is
`domain::analytics::universe::expected_cases(universe, collect_counts)` — pure, `#[must_use]`,
testable without a database. Per `(repo_id, test_file)`: the collect job's exact count wins where one
exists, `qa_catalog_sdk::UniverseTest::static_case_count` is the fallback. **The precedence has a
reason, not a preference:** legacy's `count_test_functions` (`manager/src/routes/analytics.rs:1865-1873`,
doc at `:1860-1864`) is a regex count that does **not** expand `@pytest.mark.parametrize`, so the
static number is only ever a *lower bound* — which is exactly why the collector's expanded count
supersedes it.

Three mechanics later tasks should not re-derive:

* **The fold iterates the universe, never the collect map.** A universe file with no collect row falls
  back to its static count — the ordinary case today, since Task 30 has not shipped the ingest. A
  collect row naming a file no universe entry has is loaded and never looked up.
* **The collect read is unwindowed and branch-scoped**, mirroring legacy's `load_collect_counts`
  (`:2672-2693`), with the branch defaulting to `QaInsightsConfig::default_collect_branch` (`"main"`,
  matching legacy's `DEFAULT_COLLECT_BRANCH` at `services/collect.rs:19`). This gear additionally
  narrows the read to the universe's own distinct `repo_id`s, which legacy does not.
* **Legacy fills this field in the handler, not in `build_summary`** (which leaves it at zero,
  `:1264`); the gear mirrors that split by overwriting after `summarize()` returns.

**Two things left open, both for the human.**

1. **A fail-open/fail-closed divergence (ruling R54).** Legacy's collect read ends
   `.fetch_all(db).await.unwrap_or_default()` (`:2687-2689`), so a failed collect query renders the
   overview anyway with every file on its static count. **This gear propagates, so the whole
   eight-section payload 500s.** Kept deliberately: laundering would render static counts, which is
   indistinguishable from "no collect has ever run" — the same argument `case_data`'s doc already
   makes for its own read. Reversal is one `unwrap_or_default` if the human prefers legacy's
   degradation.
2. **The collect table rides the `qa.test_result` grant (ruling R55), and there is a silent
   failure mode in it.** `qa_test_case_collect` is now a **third** table under that resource type.
   Because `TEST_RESULT` declares `pep_properties::RESOURCE_ID`, a policy that returns id constraints
   would apply them to the collect table's own `id` column, match nothing, and **silently fall back to
   static counts for every file — a wrong user-visible number with no error at all.** The hazard is
   shared with the pre-existing `case_rows_for_runs` read and was deliberately not fixed here; it is
   written at the code so the whole-branch review can decide whether the collect table wants its own
   resource type. **Task 30 writes this table, so it inherits the question.**

---

### Task 30: Exact collect — trigger, report and the hourly cycle

**Files:** create `domain/service/{collect.rs,collect_tests.rs}`, `api/rest/{handlers,routes}/collect.rs`, `domain/ports/runs_launcher.rs`

- [x] **Step 0: Verify against legacy**

* `services/collect.rs` in full — `DEFAULT_COLLECT_BRANCH:19`, `launch_collect_for_repo:26`, the URL construction at `:90`, the archive-repository rejection at `:38-40`, the branch-sync-or-skip at `:42-48`, the file union at `:56-70`, and `run_collect_cycle:157` + `start_collect_poller:183-188`.
* `routes/analytics.rs:2614` `api_collect_report` — the upsert, the path normalization via `normalize_test_path`, the `case_count.max(0)` clamp, and the 400 on an empty file name.
* `routes/analytics.rs:2655` `api_collect_trigger` — the branch default and the `{ launched, branch }` response.
* `routes/analytics.rs:2672` `load_collect_counts` — the fallback when the requested branch has no counts.

- [x] **Step 1: Write the failing tests**

```rust
/// `api_collect_report:2614`: a negative count clamps to zero and an empty file
/// name is a 400. Both are cheap guards against a runner bug becoming a
/// permanently wrong "expected cases" number.
#[tokio::test]
async fn a_collect_report_clamps_negative_counts_and_rejects_an_empty_file() {
    let f = fixture().await;
    f.service.record_count(&f.ctx, REPO, "main", "tests/a.py", -5).await.expect("clamps");
    assert_eq!(f.count_for("tests/a.py").await, 0);

    let err = f.service.record_count(&f.ctx, REPO, "main", "   ", 3).await.unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

/// `launch_collect_for_repo:42-48`: a repository lacking the branch is skipped,
/// not fatal. Legacy's comment is explicit — "branch A present in repo1 but not
/// repo2 collects repo1 only".
#[tokio::test]
async fn a_repository_without_the_branch_is_skipped_not_fatal() {
    let f = fixture_with_two_repos_one_missing_the_branch().await;
    let launched = f.service.run_collect_cycle(&f.ctx, "feature-x").await.expect("cycle runs");
    assert_eq!(launched, 1);
}

/// The report is an upsert keyed on `(repo, branch, file)`: re-collecting must
/// replace, never accumulate.
#[tokio::test]
async fn re_reporting_a_file_replaces_its_count() { /* two record_count calls, assert the latter */ }
```

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test -p qa-insights collect`
Expected: FAIL.

- [x] **Step 3: Implement**

`RunsLauncher` port wraps `qa-runs-sdk`'s launch with the `Collect` run kind from Task 5. The collect URL this gear passes is its **own** report route, which is what preserves the `VHP_COLLECT_URL` contract (D2).

Register `POST /qa/v1/collect/{repo_id}/{branch}` (the runner's target — path shape mirrors legacy's `/api/collect/{repo_id}/{branch}`) and `POST /qa/v1/analytics/collect` (the trigger).

The hourly cycle is a leader-elected ticker with role `qa-insights-collect`, interval from `collect_interval_seconds`, branch from `default_collect_branch`. Wire it in Task 40 with the other tickers; here, just make `run_collect_cycle` callable.

- [x] **Step 4: Run the tests**

Run: `cargo test -p qa-insights collect`
Expected: all pass.

- [x] **Step 5: Phase B gate** — controller-verified on `c8048ef2`, then re-verified on `59d3df39` after the whole-branch fix wave: fmt clean, clippy zero diagnostics, **518 / 0** lib+sdk, **527 / 0** Postgres tier, `cargo build --workspace` clean.

```bash
cargo fmt --all -- --check
cargo clippy -p qa-insights -p qa-insights-sdk --all-targets -- -D warnings
cargo test -p qa-insights -p qa-insights-sdk
cargo build --workspace
```

- [ ] **Step 6: Commit and squash the phase** — **half done, and deliberately left unticked.**
  The commit landed (`c8048ef2`); **the Phase B squash is OWED** and is not done, at the user's
  explicit decision, so this box stays open until it is. See the commit-discipline section.

```bash
git add gears/qa-platform/qa-insights/
git commit -m "feat(qa-insights): exact collect trigger, report and cycle"
```

Then squash Tasks 20–30 into
`feat(qa-platform): qa-insights — analytics, saved views, collect`.

---

## Decisions taken on the human's behalf during Phase B, and what is still open

Phase B ran as a continuous subagent-driven session on 2026-08-24: Tasks 25b–30, each with an
implementer, a controller-verified gate, a task review and a fix loop, then one whole-branch review
with a single fix wave. Along the way the controller ruled on questions a human would otherwise have
been asked. **Every ruling is recorded inline at its task heading and in the session ledger
(`.superpowers/sdd/2026-08-18-qa-insights-gear/progress.md`, rulings R20–R67).** This section is the
short list: what a reader needs to know without reading the ledger.

### Still open — these want a human decision, not another deferral

1. **The read window changes what the numbers mean** (R21, R22, R38). Legacy reads all history for
   the summary, the lists, `pass_count`/`fail_count`/`total_runs`, the build distribution and the
   group bars; this gear bounds them to the widest fold window, defaulting to **90 days**, to meet
   `cpt-cf-qa-nfr-scale`'s 5M rows. **The three plan drill-downs are bounded too and have no
   parameter that widens them.** A build distribution is exactly the view a user looks back past a
   quarter for. Reversing it is a service-layer change — no schema, no wire change.
2. **`csv_escape` preserves legacy's formula-injection blind spot** (R31). A value beginning `=`,
   `+`, `-` or `@` reaches a spreadsheet as a live formula. Pinned by a test, faithful to legacy, and
   **the one place in the phase where preserving legacy has a plausible attacker and a real victim**
   (a spreadsheet opened from a downloaded export). One function and one test to change.
3. **`qa_test_case_collect` rides the `qa.test_result` grant** (R55), making it a third table under
   that resource type. Because that type declares `RESOURCE_ID`, a policy returning id constraints
   would apply them to the collect table's own `id`, match nothing, and **silently fall back to
   static counts with no error**. Shared with the pre-existing `case_rows_for_runs` read. No policy
   in this repository produces such a constraint today. Wants an owner, not another note.
4. **Who bounds the unbounded array endpoints.** `config.rs`'s `max_page_size` was forecast for
   "Tasks 24–27"; all four shipped without consuming it, while the phase added six endpoints that
   return unbounded arrays (`plan/tests`, `plan/builds`, `plan/test-history`, `build-tests`, the
   overview's `lists`, and the export), bounded only by the read window. `plan/test-history` is every
   run of every test in a plan over 90 days.
5. **The runner callback is unauthenticated by design and anonymously reachable today** — verified
   through `api-gateway`'s public-route policy, not assumed. Its access control is the HMAC (R60),
   not the session. That is legacy's shape; the HMAC is this gear's addition, and it is load-bearing.

### Ruled and settled, recorded so they are not re-litigated

* **R20** `ResultsRepository::case_rows_for_runs` was in scope for 25b — without it the per-case
  counters ship wrong-but-plausible. **R23** an unknown product is `200`-of-zeros, not legacy's
  `404`; this subsystem has no product registry and the catalog port is deliberately not an existence
  oracle. **R24** both upstream `403`s fail the whole overview, unconditionally, because the universe
  is the denominator of every number in the payload. **R37/R41** `plan_id` on the wire is
  `plan_path`, matched across every repository the caller's scope admits, carried as a **query
  parameter** because real plan paths contain slashes and a slash in a path segment is a router 404.
  **R39** ordering coalesces `run_finished_at` with `run_created_at`, extending Task 21b's ruling C.
  **R40** `test-history`'s outer order is sorted by `test_name`, because legacy's order is `HashMap`
  iteration — reproducing an accidental non-order is not parity. **R47** a plan submitted with
  `scope=all` is dropped rather than stored, because `plan_key` is scope-independent and storing it
  would create a row invisible in its own list. **R48** collisions are caught at the database, never
  pre-checked. **R54** a failed collect read fails the payload where legacy degrades, because
  laundering renders static counts — indistinguishable from "no collect has run".

### Two release-gate premises to confirm before merge

* **The in-place amendment of `m20260818_000001_initial` to add `run_created_at` assumes nothing has
  been deployed from the Phase A squash.** Any database created from it will silently lack the column
  (`CREATE TABLE IF NOT EXISTS`) and every analytics read will fail.
* **The Phase B squash is owed** — see the commit-discipline section.

### Upstream defects hit during Phase B, none of them this gear's to fix

`cargo test --workspace` cannot pass (a doctest in `libs/toolkit/src/api/operation_builder.rs:932`
uses a const without a `use`; identical on `main`). `libs/toolkit-db/benches/worker_overhead.rs` is a
20+ minute bench that `--all-targets` pulls in. The OpenAPI response builder cannot express two
content types at one status, so the export's CSV variant is prose-only. `security_context_middleware`
is **dead code as wired** — reachable only via `OopServeOptions::bearer_authenticator`, which the
only production constructor hardcodes to `None`. `--dump-gears-config-yaml` **prints secret-valued
config in clear text**; only the DB DSN password is redacted and there is no per-field
secret-declaration mechanism, which now matters because Task 30 added this gear's first secret.
`cargo gears lint --dylint` and `cargo-shear` have never been run in any phase — neither is installed
on this machine.

### What Phase C inherits, in one place

Read these four sections before Task 31: `### What Tasks 26–30 inherit from Task 25b`,
`### What Task 27 changed about plan identity on the wire`, **`### What Task 28 established about
ownership and writes`** (Phase C is all writes — the JIRA registry, notification configs and bug
endpoints — so this is the one that matters most), and `### What Task 29 landed, and the two things
it left open`. Task 40's scope shrank twice during Phase B: the production `CatalogReader` adapter
and the `ClientHub` lookup moved forward to 25a, so Task 40 keeps the consumer start, the tickers,
the elector, the oagw client and the local client registration. **Task 40's collect ticker depends on
nothing further** — `run_collect_cycle` is callable and uncalled, exactly as Task 30 left it.

---

# Phase C — JIRA loop and notifications

Independent of Phase B; either may run first. Ten tasks.

---

### What Phase C's first four tasks established

**Read this before Task 35's Step 0.** Tasks 31–34 ran as a subagent-driven session on 2026-08-24:
an implementer, a controller-verified gate, a task review and a fix loop each. Twenty-two rulings
were made on a human's behalf (**R68–R89**, recorded inline here and in full in
`.superpowers/sdd/2026-08-18-qa-insights-gear/progress.md`). This section is what a later task needs
without reading the ledger.

#### Rulings that change what a remaining task may do

* **R86 — a STANDING RULE for this crate, binding on every remaining task.** Every repository read
  ending in `.one()` over a scope compiled against `OWNER_TENANT_ID` must take an explicit
  `tenant_id: Uuid`, call `validate_tenant_in_scope`, and carry a `tenant_id` equality predicate.
  **This defect was found three times — Task 32's `get_config`, Task 33's `find_unclosed_for_test`,
  and `find_by_key` one call deeper — and every time by review, never by implementation.** There is
  no case in this gear where "whichever in-scope row the engine hands back first" is the right
  answer. A scope over `OWNER_TENANT_ID` may legitimately span several tenants (`ScopeFilter::In`,
  `ScopeFilter::InTenantSubtree`), and `refuse_scope_beyond_tenant` does **not** catch it because it
  exempts that very property.
* **R87 — one scope per resource type.** An endpoint that reads two resources compiles two scopes.
  Task 33 shipped `qa_test_results` reads under a scope compiled for `qa.jira_bug`; five in-crate
  precedents compile over `resources::TEST_RESULT`, and the crate states the rule in its own
  `domain::service::mod.rs`. Tasks 35 and 38 both read across resource boundaries.
* **R74 — wiring qa-runs' launch path to call `skip_list_for` is NOT in this plan and Phase C does
  not add it.** Task 34 shipped the provider and Task 40 registers it; **no task in the file map
  touches qa-runs' launch path.** At the end of Phase C the skip list is **servable and unserved** —
  `SKIP_TESTS_WITH_BUGS` stays reserved-with-no-producer in `qa-runs/src/domain/params.rs:101`.
  This is a release-gate item, not an oversight.
* **R78 — no oagw upstream reconcile in Phase C.** Task 35 must not add `update_upstream`. A rotated
  `api_token_credstore_ref` does not take effect while the JIRA host is unchanged; the remedy that
  needs no code is rotating the value *behind* the reference, which the `apikey` plugin re-reads.
* **R70 — registration of `QaInsightsLocalClient` under `dyn QaInsightsClientV1` is Task 40's**, and
  `gear.rs:407-413` reserves it. The client itself shipped in Task 34.

#### Two things a human still owns, both new this session

1. **R77 — the credstore secret for JIRA must hold `base64("email:api_token")`, and nothing enforces
   it.** oagw strips `authorization` from every proxied request **unconditionally**
   (`oagw/src/infra/proxy/headers.rs:13-18`; the strip runs *after* passthrough at `:50-53`), so a
   per-request credential is unreachable and only an upstream auth plugin can set the header. Of
   oagw's four registered plugins only `apikey` fits a static credential, and it *concatenates*
   `prefix + secret`. The token material never enters this gear — stronger than the original ruling
   asked — but the encoding is a deployment contract with no enforcement point. **The clean fix is a
   `basic_auth` plugin in oagw taking a username plus a `secret_ref`, which is a feature in another
   gear and outside this plan.** Consequence of getting it wrong: every JIRA call 401s as a gateway
   error rather than a validation error at the `PUT`. A related trap: `SecretRef` is
   `[A-Za-z0-9_-]{1,255}` with **colons explicitly prohibited**
   (`credstore-sdk/src/models.rs:42-83`), so `credstore://…`-style references are invalid — **Slack's
   reference in Tasks 38/39 will hit the same wall.**
2. **R80's consequence — a resolved bug can be re-filed against its own still-open ticket.** Legacy
   has **three** status vocabularies and Task 33 ported all three faithfully: the open-bug lists and
   the skip list use `status = 'Open'`; `resolve_bug` writes `'Resolved'` **and** `resolved_at`
   together; the re-file probe uses `status != 'Closed'`. So once the poller resolves a bug it leaves
   the skip list and the test runs again — but if it re-fails before the ticket is *closed* in JIRA,
   `POST /qa/v1/jira/bugs` answers `created: false` with the same already-resolved key rather than
   filing fresh. That is legacy's behaviour, inherited deliberately; switching predicates would have
   been the divergence.

#### Legacy facts Tasks 35–40 should not re-derive

* **The skip-list string is not built in `argo.rs`.** `argo.rs:477-479` is only the env-var push,
  double-guarded on non-empty; the `format!("{}:{}")` + `.join(",")` construction is the caller,
  `manager/src/routes/runs.rs:753-761` (map/join at `:755-758`). **No sort, no dedup**, order is
  whatever `get_open_bugs` returned, and that query has **no `ORDER BY`**. An empty list means an
  **absent** variable, not an empty one.
* **`check_jira_status` returns the `statusCategory.key`, never the workflow status** — path
  `body["fields"]["status"]["statusCategory"]["key"]` with `.unwrap_or("unknown")`
  (`services/jira.rs:279-286`). `"done"` is the resolved signal (`jira_poller.rs:57`), and the
  do-nothing `Ok(_)` catch-all is at `jira_poller.rs:80-82`, **not** near `:65` — `:61-70` is the
  auto-rerun gate.
* **`check_new_build` is a runtime query, not a config column** (`jira_poller.rs:61-70`, ruling
  R72). `auto_rerun_on_resolve` is the flag; `qa_jira_poller_config` is complete as shipped and
  **needs no migration**. Its legacy `Default` is **300 seconds / auto-rerun ON**
  (`models.rs:1422-1429`), and legacy clamps the interval with `.max(1)` while the loop **sleeps
  first**.
* **`create_or_find_issue`'s full call sequence, its two local-DB steps and every constant** are
  transcribed in `.superpowers/sdd/2026-08-18-qa-insights-gear/task-32-report.md` §2 — issue type
  `unwrap_or("Bug")`, summary `[VHP] Test Failed: {}`, the 2000-byte log cut, the
  `vhp-test-failure` label, `platform.unwrap_or("default")`, `app_version.unwrap_or("unknown")`.
* **Legacy `NotificationsConfig` has exactly fifteen fields** (`models.rs:1338-1361`) and the shipped
  `qa_notification_config` entity matches them 1:1, with two adaptations already in place
  (`slack_webhook_url` → `slack_webhook_credstore_ref`, `email_smtp_port` `u16` → `i32`). **Task 36
  needs no migration.** `ScheduledRunNotificationEvent` is the six at `models.rs:952-972` —
  `in_progress` is why case-folding is forbidden — and `QueueNotificationEvent` is exactly `Queued`
  ("Optional, off by default") and `Expired` ("Mandatory: this is the event that stops a run
  vanishing silently"); **`Expired` has no toggle and there is no column for one.**
* **This gear's own `TestCaseResultRecord::reason` doc undersells the column.** Legacy copies
  `raw.reason` with no outcome filter (`argo.rs:2926,2984`), so a `FAILED` case carries one —
  typically the assertion message. Task 33 relies on this for the filed issue's body.

#### One test-writing trap, measured

A two-tenant test in this crate can pass **regardless of its mutation**: SQLite's
`(tenant_id, jira_key)` index returns ascending-`tenant_id` order, so an un-predicated `.one()`
happens to return the right row anyway. Task 33 hit this, diagnosed it, and fixed it by choosing the
caller's tenant as the numerically **larger** UUID. Any two-tenant test in Tasks 35–40 must defeat
that ordering to be capable of failing.

**Task 35 defeated it a second way, and the second way is better.** Its multi-tenant test separates
the two tenants' rows by a full day of `run_finished_at` rather than by which UUID sorts larger, so
the assertion is **independent of the tenant-id ordinal entirely** instead of relying on a specific
one. Prefer that construction in Tasks 36–40: it cannot be quietly broken by someone regenerating a
fixture UUID.

---

### What Task 35 established

Read before Task 36's Step 0. Task 35 closed the JIRA half of Phase C; four things it settled bind
what follows.

* **R90 — the build/start split is now the standing shape for Phase C.** Task 35 built
  `JiraPollerService` and added `ROLE_JIRA_POLLER`, and did **not** touch `gear.rs`. Task 40 starts
  the ticker. This is the same shape R70 set for Task 34's local client, and Tasks 36–39 should
  assume it: build the unit, add the role constant if the unit needs one, leave the wiring to 40.
  The consequence for Task 40 is that its Step 3 gap-fill is now the *only* place the poller, the
  local client and the notification tasks become live — its scope is real, not bookkeeping.
* **The two port extensions R73 forecast are shipped**, so Tasks 36–40 consume rather than add them:
  `RunsLauncher::launch_test` (a `RunTarget::Test` launch through the **normal admission path** —
  unlike `launch_collect`, which qa-runs bypasses admission for) and
  `PlatformReader::default_branch` (single-id, `NotFound` folded to `Ok(None)`, genuine transport
  and PDP failures still `Err`).
* **`find_plan_test_file` resolves through `CatalogReader::list_universe`, not a git checkout.**
  ADR-0005 confines git egress to qa-catalog, and `UniverseTest::test_name` already carries the
  title-when-declared-else-fallback name computed from the same `TEST_TITLE`/`TEST_META` precedence
  legacy's `declares_test_title` uses. Legacy's plan re-resolution step has no analogue here because
  `JiraBug` carries `(repo_id, plan_path)` directly. Any later task tempted to reach for a checkout
  should read `jira_poller.rs`'s module doc first.
* **A new instance of this plan's recurring documentation defect, and a new instance of its
  performance one.** Task 35's fix round corrected six wrong legacy citations — two named by the
  review and **four more found only because the sweep was made mandatory and required to report its
  result either way**. Tasks 36–40 should treat "cite the symbol you actually opened, and re-verify
  every line range carried out of this document" as the default, not as a fix-round activity. The
  performance one: `list_for_plan` was called with `UNIX_EPOCH`, disabling the time window its own
  doc calls the only thing keeping a per-plan read off an unbounded scan on a 5M-row table. **When a
  repository method's doc explains why a parameter exists, passing the value that neutralises it is a
  defect even though it compiles and passes.** Tasks 38–39's notification reads run on the same
  periodic-background-job shape and should be read with that in mind.

One item is left for the whole-branch review to triage: `infra/storage/jira_sea_repo.rs`'s
pre-existing `resolve_bug` doc carries the identical off-by-one citation (`jira.rs:243-252` vs the
actual `:243-251`) that Task 35 fixed in `domain/service/jira.rs`. It is Task 32/33 territory and was
correctly left alone; the fix is one line.

---

### What Task 36 established

Read before Task 37's Step 0. Task 36 built the pure routing core; three things it settled bind what
follows.

* **Three of the fifteen `NotificationConfig` fields are dead in legacy, and stay dead in the port.**
  The brief's Step 0 claimed `notify_run_completed` (`notifications.rs:180`) consults
  `notify_on_failure`/`notify_on_success`. **It does not — a repo-wide grep of `manager/src/` for
  `notify_on_failure`, `notify_on_success` and `notify_on_schedule_completion` finds zero reads
  anywhere outside the struct definition, its `Default` impl, and one JSON snapshot literal
  (`models.rs:877-879`).** `notify_run_completed`'s actual gates are `is_scheduled_run`,
  `scheduled_completion_notifications_enabled`, `config.slack_enabled`/`schedule_allows_slack` and
  `config.email_enabled`. The routing core (`domain/notify/routing.rs`) preserves this: all three
  fields are accepted and have no effect on any `Decision`. Tasks 37-40 should not wire them to
  anything on the strength of their names; if a future task wants to implement the PRD's implied
  per-outcome filtering, that is a product decision to raise, not a bug to silently fix.
* **R92 resolved: `dedupe_key`'s middle parameter is `NotificationKind`, not `Channel`.** Every
  `notification_kind` value legacy ever writes is the single constant
  `SCHEDULED_RUN_SLACK_NOTIFICATION_KIND = "scheduled_run_slack"` (`notifications.rs:14`) — a family
  and a channel already fused into one string, matching `NotificationClaim::kind`'s own "the
  notification family" doc. `NotificationKind` (`ScheduledRunSlack`, `RunCompletedSlack`,
  `RunCompletedEmail`, `QueueSlack`) keeps that fusion; typing the parameter a bare `Channel` would
  let two different families sharing a channel collide on one claim slot. Tasks 37-40 should extend
  this enum rather than introduce a parallel `Channel` type.
* **R94: `QueueExpired` has no per-event toggle, but it still respects the tenant's master Slack
  switch.** Legacy's own doc comment on `notify_queue_event` draws a two-axis distinction
  (`notifications.rs:654-656`): *"`Expired` has no enable flag by design: the ticket makes it
  mandatory. It is still gated on Slack being configured, because there is nowhere to send
  otherwise."* "No enable flag" means no **per-event** toggle — there is no
  `run_queue_expired_slack_enabled` column the way `Queued` has
  `run_queue_queued_slack_enabled`. It does not mean bypassing `config.slack_enabled`, which still
  gates it exactly as the shared check at `notifications.rs:694-704` gates it in legacy (the
  `Queued`-only toggle check at `:685-692` does not apply to `Expired` at all). A first
  implementation round read an earlier controller dispatch too literally and had `route()` bypass
  `slack_enabled` for `QueueExpired` too; a fix round corrected it before review, on the controller's
  own re-reading of the legacy source. `route()` now gates `QueueExpired` on `config.slack_enabled`
  and nothing else — never on `run_queue_queued_slack_enabled` or any other per-event toggle, and
  never on webhook-emptiness, which stays a capability question (see `routing.rs`'s "Policy versus
  capability" section: seven of the fifteen fields — the free-form data ones — have no bearing on
  any `Decision` at all). The same legacy paragraph also distinguishes two "did not send" outcomes —
  `Queued` skipped by its own toggle is audited nowhere, everything else is — which the same fix
  round added to `Decision` as `slack_skip_is_audited` (R94a).
* **R96/R96a — `slack_skip_is_audited`'s value per event, and a warning about how it was nearly got
  wrong twice.** The review then found the same defect class one layer down: `routing.rs` claimed
  *"`RunCompleted` and `ScheduledRun` have no silent case at all"*. That is true of
  `notify_scheduled_run_status` — every early return logs (`:444,455,466,477,492`, plus the error and
  success logs: seven `log_notification` calls in total) — and **false** of `notify_run_completed`,
  whose Slack chain at `:263-322` is `if / else if / else if` with **no final `else`** and every arm
  gated on `config.slack_enabled && !webhook.is_empty()`. So `slack_enabled == false`, reached past
  the `:208-219` early return, logs nothing at all. The field had been hardcoded `true` for that arm.
  **The correct value is `!schedule_notifies || config.slack_enabled`**, and the three reachable
  states are: schedule-blocked → always audited (the `:210-217` log fires before `config.slack_enabled`
  is ever consulted); schedule-passes with Slack on → exactly one arm fires and every arm logs;
  schedule-passes with Slack off → the silent case. **The controller's own ruling first proposed
  `config.slack_enabled` alone, which is wrong** — both `NotificationConfig::default()` and
  `ScheduleNotificationSettings::default()` have `slack_enabled == false`, so the **default** state is
  schedule-blocked, audited in legacy, and the simpler formula reports it silent. The implementer
  declined the formula and said why; the re-review confirmed the deviation was correct across all
  three states. **Tasks 37-40: this field is not a boolean you can guess from one state. If you extend
  `Decision` or add an event, enumerate the reachable states against legacy before assigning it.**

Full detail, the complete fifteen-field gating map and the citation sweep (six drifted, all
corrected) are in `.superpowers/sdd/2026-08-18-qa-insights-gear/task-36-report.md`.

---

### What Task 37 established

Read before Task 38's Step 0. Task 37 built the pure rendering core; four things bind what follows.

* **One renderer serves both the send and the preview, by construction.** Legacy's preview
  (`notifications.rs:388`) and send (`:431`) are separate functions that can drift, which is why the
  brief pinned their equality with a test. In the port `preview_scheduled_run` is a direct
  pass-through to `render_scheduled_run`, so the property is structural and **the brief's test cannot
  fail** — it was replaced by a six-token property test rather than kept as false assurance. Task 38
  must not reintroduce a second rendering path for the send; if it needs a variation, it belongs
  inside the one renderer.
* **The template engine is legacy's, not an invention.** `{{#if}}` / `{{#if_event}}` are a real
  hand-rolled recursive grammar in legacy (`notifications.rs:1099-1219`), ported token-for-token:
  an unknown placeholder is left verbatim, a `None`-gated section vanishes, an unmatched open tag is
  left raw, and nesting resolves one recursion per open tag. The five default section templates and
  all eighteen status label/headline/icon values are byte-identical to legacy, **including the
  em-dash and middle-dot characters** — do not "clean up" whitespace or punctuation in them.
* **The case-folding trap has a subtle safe spot.** `event_matches` lower-cases the *tenant's*
  template argument text — what someone writes inside `{{#if_event ...}}` — but never the event token
  itself, which stays an exact match against the closed six. Any later code that resolves a token must
  keep that asymmetry.
* **R99: user-visible legacy strings stay legacy's until a human says otherwise.** The email subject
  was briefly rebranded `"VHP test run"` → `"QA run"` and reverted. The reason is consistency, not
  nostalgia: `infra/jira/oagw_client.rs:789` ships `format!("[VHP] Test Failed: {test_name}")`,
  reviewed and shipped verbatim in Tasks 32-33, so rebranding one string and not the other leaves the
  product speaking two names. **Whether to rebrand at all is an open product question for the human**,
  spanning both strings and probably others outside this crate. Tasks 38-40: do not change a
  user-visible string legacy owns, and do not resolve this question in passing.

---

### What Task 38 established

Read before Task 39's Step 0. Task 38 is where the two pure cores met a repository and an egress;
five things bind what remains.

* **The claim lifecycle has six reachable outcomes and they are now pinned by tests.** Capability gate
  fails → **no claim, no log, no send** (legacy is silent here, and the gate runs in the `if` guard
  *before* the claim, so a failed gate burns nothing); claim lost → logged as a duplicate, no send, no
  release; send succeeds → claim retained, `sent` logged, **no release** (a release here would produce
  duplicate sends); send fails → claim **released**, `failed` logged with detail, never propagated;
  `UnsupportedEgress` → claim **released** (nothing was sent, so the slot must not survive); the
  release itself failing → the log row is **still written**, never propagated. **No arm takes a claim
  and neither releases nor sends it, and no arm releases after a successful send.** Task 40 must not
  add a seventh arm without re-deriving this table.
* **`gear.rs` holds two inert stand-ins that Task 40 replaces**, `NeverWiredSlackClient` and
  `NeverWiredMailClient`, under the file's existing `// wired in Task N` convention. Task 39 ships the
  real adapters; **Task 40 binds them.** Until then every run-completed Slack send logs
  `unsupported_egress` and releases its claim, which is why the release matters before the adapter
  exists rather than after.
* **R102's seam is real and no single task's tests cross it.** Task 38 owns
  `SendOutcome::UnsupportedEgress` → the log's `"unsupported_egress"` string and tests it directly;
  Task 39 asserts the variant. The port shapes were fixed in advance so Task 39 finds no surprises:
  `MailClient::send(&self, &MailMessage) -> Result<SendOutcome, DomainError>` takes no `ctx` and the
  inert impl **never errors**, and `SlackClient` deliberately declares **no** `REQUEST_TIMEOUT` const
  so `SlackOagwClient` can carry its own as Task 39's test requires.
* **The concurrency property rests on the schema, not on a test.** `two_concurrent_attempts_produce_
  exactly_one_send` runs on in-memory SQLite, which **serializes writers with `max_conns: Some(1)`**,
  so a race cannot be exhibited there — and the Postgres-tier test that was once cited as exhibiting
  one actually runs two *sequential* claims inside a single transaction. **The race is not demonstrated
  on any tier.** What makes the code correct is `idx_qa_run_notifications_claim`'s unique index plus
  `ON CONFLICT DO NOTHING`, and the tests demonstrate that a second sequential claim loses and that the
  service acts on the boolean. Do not let a later task re-describe this as proven by test.
* **R106 pinned `list_log` to the tenant**, closing the last unpinned read on the notification path.
  That is the **sixth** instance of the R86 defect class in this crate; every one was caught by review,
  and one of them (`get_config`) was found by an implementer reading Step 0 rather than by a reviewer.
  Tasks 39-40: R86 is not a checklist item, it is the single most repeated defect here.

---

### What Task 39 established — including a RELEASE-GATE item

Read before Task 40's Step 0. **The first bullet is the one that matters and it needs a human
decision.**

* **R107 — RELEASE GATE: the Slack egress is built, bounded, tested and UNDELIVERABLE.** The adapter
  cannot deliver a real Slack message to a real tenant today, and this is architectural, not a bug in
  Task 39. **Why it differs from the JIRA path**, which does work: under R76/R77 the JIRA token never
  enters this gear at all, because oagw's `apikey` plugin injects it **into a header** from the
  tenant's credstore reference. **A Slack webhook's secret is the URL path**, and no oagw plugin
  injects a path segment. `qa-insights/Cargo.toml` declares `oagw` and `oagw-sdk` and **no
  `credstore-sdk`** (verified against the file), so the gear cannot resolve the reference itself
  either. **Two candidate fixes, both cross-gear and neither inside this plan's scope:** (i) give this
  gear a `credstore-sdk` dependency so it resolves the reference into a complete URL and hands oagw a
  full URI, or (ii) add an oagw capability that can carry a path secret. **This needs a human
  decision before release.**
  **It degrades correctly rather than dangerously**, which is why it is a gate item and not a
  stop-work: Task 38's claim lifecycle releases the claim on a failed send and writes an
  `OUTCOME_FAILED` audit row, so nothing is permanently suppressed and an operator can see the
  failures accumulate. **Task 40 must not assume Slack delivery works.** The full argument is in
  `infra/notify/slack_oagw.rs`'s module doc under "Finding B".
* **R108 — a dropped parameter, closed before it could become a security question.**
  `SlackClient::send` had no `SecurityContext`, so the adapter proxied as
  `SecurityContext::anonymous()` while **both call sites already held a context they did not
  forward**. Fixed in this task rather than deferred, on the reasoning that once Task 40 binds the
  adapter an anonymous proxy stops being a signature change. **The general rule for Task 40: a port
  signature defect found before wiring is a signature change; found after wiring it is an incident.**
* **The `tokio::time::timeout` bound is applied, not merely declared.** The brief's
  `the_slack_client_bounds_every_request` asserts a constant equals 10s and is tautological by
  construction — the same shape R97 caught in Task 37. `the_bound_is_actually_applied_to_a_hanging_
  gateway` is the real one: its fake gateway returns `std::future::pending()`, so removing the wrapper
  makes the test hang rather than pass. The 10-second value is **ported reasoning, not a guess**
  (`notifications.rs:53-64`) — a fixed bound, never derived from a tick interval.
* **`gear.rs` still holds two inert stand-ins**, `NeverWiredSlackClient` and `NeverWiredMailClient`.
  **Replacing them with `SlackOagwClient` and `UnsupportedMailClient` is Task 40's job**, and
  `mail_unsupported.rs`'s module doc carries the D10 replacement recipe for whoever eventually adds
  SMTP: implement `MailClient`, bind it in `gear.rs`, delete nothing else.

---

### Task 31: The JIRA registry core — ✅ DONE

**Files:** create `domain/jira/{mod.rs,registry.rs,registry_tests.rs}`

- [x] **Step 0: Verify against legacy**

* `services/jira.rs:220` `get_open_bugs` and `:232` `get_all_open_bugs` — the open-bug predicate.
* `services/jira.rs:243` `resolve_bug` — what "resolved" writes.
* `migrations/001_initial.sql:78-92` — the row shape, the global uniqueness on `jira_key`, and the two indexes.
* `services/argo.rs:476-481` — the skip-list wire format: a comma-separated `test_name:JIRA-KEY` list delivered as the `SKIP_TESTS_WITH_BUGS` environment variable. **The format is frozen** (`cpt-cf-qa-fr-migration-runner-contract`); record the exact separators and whether the list is sorted or deduplicated.
* `routes/settings.rs:580-631` `api_create_jira_ticket` — note that the request carries only `test_name` (`JiraCreateRequest { test_name }`), which is what lets a bug be filed from a failure without a commit to the test repository (PRD `cpt-cf-qa-fr-insights-jira`).

- [x] **Step 1: Write the failing tests**

```rust
/// The wire format is frozen (`manager/src/services/argo.rs:476-481`): a
/// comma-separated `test_name:JIRA-KEY` list. Tests consume this variable
/// directly, so a spacing or ordering change is a test-contract break, not a
/// cosmetic one.
#[test]
fn the_skip_list_renders_in_the_frozen_wire_format() {
    let rendered = render_skip_list(&[
        bug("test_upgrade", "VHP-2618"),
        bug("test_backup", "VHP-2701"),
    ]);
    // The exact expected string comes from reading `:476-481` in Step 0 —
    // including whether it sorts and whether it deduplicates.
    assert_eq!(rendered, "<from Step 0>");
}

/// Only open bugs suppress a test. A resolved bug must stop appearing the
/// moment it resolves, or a fixed test stays skipped forever.
#[test]
fn a_resolved_bug_leaves_the_skip_list() {
    let bugs = vec![open("test_a", "V-1"), resolved("test_b", "V-2")];
    assert_eq!(skip_list_entries(&bugs).len(), 1);
}
```

- [x] **Step 2–3: Run, implement, run, commit**

The core is pure: bug rows in, skip-list string and open-bug views out. No repository, no HTTP.

Run: `cargo test -p qa-insights --lib domain::jira`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): JIRA registry core and frozen skip-list format"
```

---

### Task 32: JIRA config and the oagw client — ✅ DONE

**Files:** create `domain/ports/jira_client.rs`, `infra/jira/{mod.rs,oagw_client.rs}`; modify `domain/service/jira.rs`

- [x] **Step 0: Verify against legacy**

* `services/jira.rs:33` `create_or_find_issue` — read all of it. Record the JIRA REST calls it makes, in order, and its find-before-create behavior.
* `services/jira.rs:254` `check_jira_status` — record that it returns a **status category** and that `"done"` is the resolved signal (`jira_poller.rs:57`).
* `models.rs` `JiraConfig` — `url`, `project_key`, `email`, `api_token`, `issue_type: Option<String>`, `enabled`.
* `routes/settings.rs` `api_get_jira` / `api_update_jira` — the GET/PUT surface and whether the token is masked on read.

- [x] **Step 1: Write the failing tests**

```rust
/// The token is a credstore reference here, never the material (Task 10). A GET
/// that returned a bearer token would be a credential-disclosure bug, and
/// legacy's masking behavior — confirmed in Step 0 — is the floor, not the
/// ceiling.
#[tokio::test]
async fn reading_the_jira_config_never_returns_token_material() {
    let f = fixture_with_jira_config().await;
    let config = f.service.get_jira_config(&f.ctx).await.expect("config");
    assert!(!config.api_token_ref.contains("secret"), "got: {}", config.api_token_ref);
}

/// A disabled or absent config short-circuits silently rather than erroring —
/// `manager/src/services/jira_poller.rs:40-43` returns `Ok(())`.
#[tokio::test]
async fn a_disabled_jira_config_is_not_an_error() {
    let f = fixture_without_jira_config().await;
    assert!(f.service.poll_once(&f.ctx).await.is_ok());
}
```

- [x] **Step 2–3: Run, implement, run, commit**

`JiraClient` is a port with three methods: `create_or_find_issue`, `check_status`, `get_issue`. The oagw adapter builds an `http::Request` and calls `ServiceGatewayClientV1::proxy_request` — read `gears/system/oagw/oagw-sdk/src/api.rs:136-156` for the URI convention (`/{alias}/{path_suffix}?query`). Credentials resolve from credstore by the reference stored in `qa_jira_config`.

Register `GET/PUT /qa/v1/settings/jira`.

Run: `cargo test -p qa-insights jira_config`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): JIRA config and oagw client"
```

---

### Task 33: Bug registry endpoints — ✅ DONE

**Files:** modify `domain/service/jira.rs`; create `api/rest/{handlers,routes}/jira.rs`

- [x] **Step 0: Verify against legacy**

`routes/settings.rs:580-631` (`api_create_jira_ticket`, reached at `POST /api/runs/{name}/jira`) and the open-bugs route at `routes/mod.rs:297`. Record the request shape, the response, and what happens when a bug already exists for that test.

- [x] **Step 1–3: Test-first, implement, commit**

Endpoints: `GET /qa/v1/jira/open-bugs`, `POST /qa/v1/jira/bugs` (file or link a bug against a failed test from a run view — the request carries the test name, per PRD).

Run: `cargo test -p qa-insights open_bugs`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): bug registry endpoints"
```

---

### Task 34: The skip-list SDK method — ✅ DONE

**Files:** modify `qa-insights-sdk/src/client.rs`, `domain/local_client/client.rs`

This is the one thing another gear calls: qa-runs asks for the skip list at launch when skip-tests-with-bugs is requested.

- [x] **Step 0: Verify against legacy**

`services/argo.rs:476-481` again — confirm **when** the variable is set (only when the launch requests it, or always?) and what an empty list produces (an empty variable, or no variable at all?). The difference is visible to every test that reads the variable.

- [x] **Step 1: Write the failing test**

```rust
/// An empty skip list must produce whatever legacy produces — Step 0 decides
/// whether that is an absent variable or an empty one. A test repository that
/// branches on `os.environ.get("SKIP_TESTS_WITH_BUGS")` sees the difference.
#[tokio::test]
async fn an_empty_skip_list_matches_legacy() {
    let f = fixture_with_no_open_bugs().await;
    let entries = f.client.skip_list_for(&f.ctx, PLAN).await.expect("skip list");
    assert!(entries.is_empty());
}
```

- [x] **Step 2–3: Run, implement, run, commit**

Run: `cargo test -p qa-insights skip_list`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights-sdk): skip-list provider for qa-runs"
```

---

### Task 35: The JIRA poller and auto-rerun — ✅ DONE

**Files:** create `domain/service/{jira_poller.rs,jira_poller_tests.rs}`

- [x] **Step 0: Verify against legacy**

Read `services/jira_poller.rs` end to end. Confirm all five:

1. `:16-31` — the interval comes from `JiraPollerConfig.poll_interval_seconds`, `.max(1)`, and the loop **sleeps first**.
2. `:40-43` — an absent or disabled config returns `Ok(())` without polling.
3. `:54-58` — resolution is detected by status category `"done"`, and `resolve_bug` runs **before** the rerun decision, so a resolved bug is recorded even when auto-rerun is off.
4. `:60-70` — the rerun needs **both** `auto_rerun_on_resolve` **and** `check_new_build`. (D8)
5. `:88-96` and `:100-118` — the rerun goes through the **normal admission path**, and the branch is resolved **once** and reused for both the plan lookup and the launch. Read that comment in full: resolving the plan against the repo default would search a different tree than the run executes against.

- [x] **Step 1: Write the failing tests**

```rust
/// D8: resolution alone is not enough. `manager/src/services/jira_poller.rs:65-70`
/// requires a new build too — otherwise every resolved bug reruns against the
/// same build that failed, which proves nothing and costs a platform slot.
#[tokio::test]
async fn a_resolved_bug_without_a_new_build_does_not_rerun() {
    let f = fixture_with_resolved_bug_and_no_new_build().await;
    f.service.poll_once(&f.ctx).await.expect("poll");
    assert_eq!(f.launcher.launches(), 0);
}

/// The bug is marked resolved even when auto-rerun is disabled
/// (`jira_poller.rs:54-58`: `resolve_bug` runs before the `continue`).
#[tokio::test]
async fn a_bug_resolves_even_when_auto_rerun_is_off() {
    let f = fixture_with_resolved_bug_auto_rerun_off().await;
    f.service.poll_once(&f.ctx).await.expect("poll");
    assert!(f.bug_is_resolved("V-1").await);
    assert_eq!(f.launcher.launches(), 0);
}

/// The rerun is an ordinary launch. VHP-2618 removed the one bypass that used
/// to exist (`jira_poller.rs:8-15`), and the stale comment at
/// `services/argo.rs:369-372` claiming otherwise must not be reproduced.
#[tokio::test]
async fn an_auto_rerun_goes_through_the_normal_launch_path() {
    let f = fixture_with_resolved_bug_and_new_build().await;
    f.service.poll_once(&f.ctx).await.expect("poll");
    assert_eq!(f.launcher.launches(), 1);
    assert!(!f.launcher.last_launch().bypassed_admission);
}

/// The branch is resolved once and reused (`jira_poller.rs:100-118`). Two
/// resolutions can disagree, and the failure is silent: a rerun that drops
/// because the test "does not exist" on a tree it was never going to run on.
#[tokio::test]
async fn the_branch_is_resolved_once_and_reused_for_lookup_and_launch() {
    // The platform's default branch is `release-1.2`; the test exists only
    // there, not on the repository default. Legacy resolves the branch once
    // from the platform (`jira_poller.rs:110-113`) and passes the same value to
    // the plan lookup and the launch. Resolving twice — or resolving the lookup
    // against the repository default — searches a tree the run will not execute
    // against, and the rerun is silently dropped.
    let f = fixture_with_platform_branch("release-1.2").await;
    f.catalog.add_test_on_branch_only("release-1.2", "tests/a.py", "test_a");
    f.add_resolved_bug_with_new_build("test_a", "release-1.2").await;

    f.service.poll_once(&f.ctx).await.expect("poll");

    assert_eq!(f.launcher.launches(), 1, "the rerun must not be dropped");
    assert_eq!(f.catalog.lookup_branches(), vec!["release-1.2"], "resolved once");
    assert_eq!(f.launcher.last_launch().branch.as_deref(), Some("release-1.2"));
}
```

- [x] **Step 2–3: Run, implement, run, commit**

Leader-elected, role `qa-insights-jira-poller`. Register `GET/PUT /qa/v1/settings/jira-poller`.

Run: `cargo test -p qa-insights jira_poller`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): JIRA poller with new-build-gated auto-rerun"
```

---

### Task 36: The notification routing core — ✅ DONE

**Files:** create `domain/notify/{mod.rs,routing.rs,routing_tests.rs}`

- [x] **Step 0: Verify against legacy**

* `models.rs` `NotificationsConfig` — all fifteen fields. Record which field gates which event.
* `notifications.rs:180` `notify_run_completed` — which config flags it consults (`notify_on_failure`, `notify_on_success`) and how it decides.
* `notifications.rs:431` `notify_scheduled_run_status` — the six `ScheduledRunNotificationEvent` values and the per-schedule settings that narrow them.

  **The six are `pending`, `in_progress`, `succeeded`, `failed`, `error`, `skipped`** — serialized snake_case, not the Rust variant names, pinned by legacy's own `scheduled_run_event_serializes_to_snake_case` (`manager/src/models.rs:940-946`) and by the UI's token list (`manager-ui/src/api/types.ts:761-767`). Task 6 shipped `SLACK_NOTIFICATION_EVENTS` on `qa-runs-sdk` as the closed set; parse against it rather than re-deriving the spellings, and do not case-fold — `InProgress` lower-cases to `inprogress`, which is not a member.
* `notifications.rs:657` `notify_queue_event` — and the `QueueNotificationEvent` doc comments: `Queued` is *"Optional, off by default"* gated by `run_queue_queued_slack_enabled`; `Expired` is *"Mandatory: this is the event that stops a run vanishing silently."* **`Expired` has no toggle. Do not add one.**
* `run_notifications`' composite PK at `001_initial.sql:197-203` — the dedupe key is `(workflow, kind, event)`.

**Step 0 result (see `task-36-report.md` for full detail): `notify_run_completed` does not consult
`notify_on_failure`/`notify_on_success` at all — those two fields, plus `notify_on_schedule_completion`,
are dead in legacy. Legacy's `notification_kind` is a single constant that fuses family and channel
(`SCHEDULED_RUN_SLACK_NOTIFICATION_KIND`), which is why Step 1's `dedupe_key` types its second
parameter `NotificationKind` rather than the brief's `Channel` (R92).**

- [x] **Step 1: Write the failing tests**

```rust
/// `QueueNotificationEvent::Expired` is mandatory and un-toggleable — the model
/// comment calls it "the event that stops a run vanishing silently". A config
/// with every switch off must still route it.
#[test]
fn the_expired_queue_event_routes_with_every_toggle_off() {
    let decision = route(&all_disabled_config(), &Event::QueueExpired, &no_schedule_settings());
    assert!(decision.sends_slack(), "expired has no toggle");
}

/// `Queued` is opt-in and off by default (`run_queue_queued_slack_enabled`).
#[test]
fn the_queued_event_is_off_by_default() {
    assert!(!route(&default_config(), &Event::Queued, &no_schedule_settings()).sends_slack());
}

/// The dedupe key is `(run, kind, event)` (`001_initial.sql:197-203`). Two
/// instances racing on the same run must produce exactly one send.
#[test]
fn the_dedupe_key_is_run_kind_and_event() {
    assert_eq!(
        dedupe_key(RUN, Channel::Slack, &Event::RunCompleted),
        dedupe_key(RUN, Channel::Slack, &Event::RunCompleted)
    );
    assert_ne!(
        dedupe_key(RUN, Channel::Slack, &Event::RunCompleted),
        dedupe_key(RUN, Channel::Email, &Event::RunCompleted)
    );
}
```

Plus one test per config flag found in Step 0 — fifteen fields is a table-driven test, not sixteen functions.

**Shipped as `routing_tests.rs`.** The three named tests are verbatim except `the_dedupe_key_is_run_kind_and_event`,
whose second argument became `NotificationKind::{RunCompletedSlack,RunCompletedEmail}` per R92 — the
pinned property (two different kinds over the same run+event must not collide) is unchanged. The
fourth requirement shipped as `every_notification_config_field_gates_what_step_0_found`, a 15-row
table asserting both the fully-enabled baseline and the post-mutation decision as literal booleans
(not derived by calling `route` twice and comparing) — the first draft used the latter shape and was
caught, during self-review, passing against an always-`false` `route` stub.

- [x] **Step 2–3: Run, implement, run, commit**

Pure: config + event + per-schedule settings in, a routing decision out. No client, no repository.

Run: `cargo test -p qa-insights --lib domain::notify::routing`
Expected: FAIL, then PASS. **Done — RED/GREEN evidence and two mutation checks in
`task-36-report.md`.**

```bash
git commit -am "feat(qa-insights): notification routing core"
```

---

### Task 37: Message rendering — ✅ DONE

**Files:** create `domain/notify/{render.rs,render_tests.rs}`

- [x] **Step 0: Verify against legacy**

* `notifications.rs:113` `send_slack_to_channel_with_blocks` and the `ScheduledRunSlackMessage` / `ScheduledRunSlackRenderedSections` structs at `:31-44` — the five sections (`header`, `summary`, `results`, `body`, `footer`) and the `fallback_text` that accompanies blocks.
* `notifications.rs:388` `preview_scheduled_run_message` — the preview must render the same message the send does, or the preview is a lie.
* `ScheduledRunSlackTemplatesConfig` — the template vocabulary and its placeholders.
* `notifications.rs` `ResultCounts` at `:23-29` and how counts reach the message.
* `manager_ui_base_url` — how links are built.

- [x] **Step 1: Write the failing tests**

```rust
/// The preview and the send must render identically
/// (`notifications.rs:388` vs `:431`). A preview that diverges is worse than no
/// preview, because it is trusted.
#[test]
fn the_preview_renders_exactly_what_the_send_renders() {
    let ctx = scheduled_run_context();
    assert_eq!(render_scheduled_run(&ctx).rendered_message, preview_scheduled_run(&ctx));
}

/// Blocks always carry a fallback text — a Slack client that cannot render
/// blocks shows the fallback, and an empty one shows nothing at all.
#[test]
fn every_block_message_carries_a_non_empty_fallback() {
    let message = render_scheduled_run(&scheduled_run_context());
    assert!(!message.fallback_text.trim().is_empty());
    assert!(!message.blocks.is_empty());
}
```

- [x] **Step 2–3: Run, implement, run, commit**

Run: `cargo test -p qa-insights --lib domain::notify::render`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): Slack block and email rendering"
```

---

### Task 38: The notification service, dedupe and log — ✅ DONE

**Files:** create `domain/service/{notify.rs,notify_tests.rs}`, `domain/ports/{slack_client.rs,mail_client.rs}`, `api/rest/{handlers,routes}/settings.rs`

- [x] **Step 0: Verify against legacy**

`notifications.rs:622` `get_notification_log`, `:358` `send_test_notification`, `:407` `send_scheduled_run_test_notification`, and the four settings routes at `routes/mod.rs:271-285` (`GET/PUT /api/settings/notifications`, `/test`, `/preview`, `/log`). Record the log's `outcome` vocabulary — you are about to add one value to it.

- [x] **Step 1: Write the failing tests**

```rust
/// One send per (run, kind, event), even under concurrency. `claim_notification`
/// is an insert that reports whether *this* caller won (Task 11); a
/// check-then-insert pair cannot express that.
#[tokio::test]
async fn two_concurrent_attempts_produce_exactly_one_send() {
    let f = fixture().await;
    let (a, b) = tokio::join!(
        f.service.notify_run_completed(&f.ctx, RUN),
        f.service.notify_run_completed(&f.ctx, RUN),
    );
    a.expect("first"); b.expect("second");
    assert_eq!(f.slack.sends(), 1);
}

/// Every attempt is logged, including failures — legacy records outcome and
/// detail for exactly this reason, and a silent failure is the one thing an
/// operator cannot debug.
#[tokio::test]
async fn a_failed_send_is_logged_with_its_detail() {
    let f = fixture_with_failing_slack().await;
    f.service.notify_run_completed(&f.ctx, RUN).await.expect("does not propagate");
    let log = f.log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].outcome, "failed");
    assert!(!log[0].detail.is_empty());
}

/// D10: email is configured but unsent. The routing decision, the dedupe claim
/// and the log entry all happen; only the socket is missing. When SMTP arrives,
/// this test changes and nothing else does.
#[tokio::test]
async fn an_email_send_is_recorded_as_unsupported_egress() {
    let f = fixture_with_email_enabled().await;
    f.service.notify_run_completed(&f.ctx, RUN).await.expect("does not propagate");
    let email_entries: Vec<_> = f.log().await.into_iter().filter(|e| e.channel == "email").collect();
    assert_eq!(email_entries.len(), 1);
    assert_eq!(email_entries[0].outcome, "unsupported_egress");
}
```

- [x] **Step 2–3: Run, implement, run, commit**

A send failure **never propagates** — it is logged and swallowed, matching legacy. Register `GET/PUT /qa/v1/settings/notifications`, `POST /qa/v1/settings/notifications/test`, `POST /qa/v1/settings/notifications/preview`, `GET /qa/v1/settings/notifications/log`.

Run: `cargo test -p qa-insights notify`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): notification service with dedupe and audit log"
```

---

### Task 39: The egress adapters — ✅ DONE

**Files:** create `infra/notify/{mod.rs,slack_oagw.rs,mail_unsupported.rs}`

- [x] **Step 0: Verify against legacy**

`notifications.rs:52-64` — the HTTP client's **10-second total-request timeout** and the comment explaining it: the dispatcher awaits a Slack send inside its tick for the mandatory `expired` event, and an unbounded request against a black-holing webhook host would stall the whole queue. Record that reasoning; it survives the port even though the tick is now qa-runs'.

- [x] **Step 1: Write the failing tests**

```rust
/// The 10s bound is ported reasoning, not a guess
/// (`manager/src/services/notifications.rs:52-64`). It is a fixed bound, not
/// derived from any tick interval.
#[test]
fn the_slack_client_bounds_every_request() {
    assert_eq!(SlackOagwClient::REQUEST_TIMEOUT, Duration::from_secs(10));
}

/// D10. The mail adapter is deliberately inert: it reports the outcome the log
/// records and returns Ok, so a missing SMTP path never fails a run's
/// notification pass.
#[tokio::test]
async fn the_unsupported_mail_client_reports_rather_than_fails() {
    let outcome = UnsupportedMailClient.send(&message()).await.expect("never errors");
    assert_eq!(outcome, SendOutcome::UnsupportedEgress);
}
```

- [x] **Step 2–3: Run, implement, run, commit**

The Slack adapter posts through oagw (`ServiceGatewayClientV1::proxy_request`), not through a direct `reqwest` — `cpt-cf-qa-contract-egress` permits exactly one exception and it belongs to qa-catalog's git. Put a comment saying so at the top of the file, because a future reader with a webhook URL in hand will reach for `reqwest` by reflex.

`mail_unsupported.rs` carries the D10 rationale in its module doc, including what replacing it looks like: implement `MailClient`, bind it in `gear.rs`, delete nothing else.

Run: `cargo test -p qa-insights --lib infra::notify`
Expected: FAIL, then PASS.

```bash
git commit -am "feat(qa-insights): Slack egress over oagw and the deferred mail adapter"
```

---

### Task 40: Wire the gear and close the loop — ✅ DONE

**Files:** modify `gear.rs`, `lib.rs`; create `tests/ingest_idempotence.rs`

- [x] **Step 1: Write the failing end-to-end test**

```rust
/// The whole Phase A/B/C path in one: publish a run's lifecycle, let the
/// consumer project it, and assert the dashboard and the overview both see it.
/// Every unit below this is green in isolation; this is the test that catches a
/// gear whose `init` forgot to start the consumer.
#[tokio::test]
async fn a_published_run_reaches_the_dashboard_and_the_overview() {
    let gear = booted_gear_with_broker().await;
    gear.publish_full_run_lifecycle(RUN_ID, &[("tests/a.py", "test_a", "PASSED")]).await;
    gear.wait_for_ingest(RUN_ID).await;

    let dashboard = gear.get_dashboard().await;
    assert_eq!(dashboard.total_runs, 1);

    let overview = gear.get_overview().await;
    assert_eq!(overview.summary.passed, 1);
}
```

- [x] **Step 2: Run it and watch it fail**

Run: `cargo test -p qa-insights --test ingest_idempotence`
Expected: FAIL — the consumer is not started.

- [x] **Step 3: Complete `init` and `serve`**

Fill every `// wired in Task N` gap left in Task 9:

* `ClientHub` lookups for `QaRunsClientV1`, `QaCatalogClientV1`, `ServiceGatewayClientV1`, `EventBrokerApi`.
* Register `QaInsightsLocalClient` under `dyn QaInsightsClientV1` (the skip-list provider qa-runs calls).
* Start the consumer (Task 13) under the cancellation token.
* Start three leader-elected tickers, each independently switchable by config, each with its own role name — `qa-insights-reconciler`, `qa-insights-jira-poller`, `qa-insights-collect`. Three roles, not one: `qa-runs/src/gear.rs`'s `SCHEDULER_ROLE` comment explains why sharing a role makes "run this ticker here but not that one" unexpressible, and the same reasoning applies threefold here.
* **Obligation, added 2026-08-21 by Phase A's whole-phase review: the reconciler ticker must alert on `ReconcileOutcome::stopped_at_gap`, not merely log it.** This is not a nicety. `consume_page` breaks at the first run it cannot backfill and never advances the watermark past it — which is correct, and is what stops a failed run being stranded forever — so a run that fails *permanently* (a driver error on its rows, a persistent qa-runs 500 for that id) means **no run finishing after it is ever backfilled for that tenant**. The projection is not wrong; it is frozen from that instant on. Legacy could not wedge this way because it had no watermark: it re-listed every workflow on every cycle, so one bad run cost exactly that run.

  The whole failure mode is silent by construction — no error is returned, no data is reported missing, the sweep just stops advancing — so the ticker is the only place it can be surfaced. `ReconcileOutcome::stopped_at_run` carries the run id for exactly this, and `domain::service::reconcile`'s header states the liveness consequence beside the two behaviours it already records. Whoever implements this should also decide whether a *repeatedly* wedged tenant (the same `stopped_at_run` on consecutive passes) is a louder signal than a single one; the outcome type carries no history and the ticker is where any would live.

- [x] **Step 4: Run it**

Run: `cargo test -p qa-insights --test ingest_idempotence`
Expected: PASS.

- [x] **Step 5: Phase C gate and the full workspace**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib --bins --tests
cargo test -p qa-insights --features integration --lib
cargo build --workspace
```

Expected: green throughout, with qa-runs still at or above its Phase 0 count.

> **Corrected in place, 2026-08-25, because the literal commands could not pass.** The third line said
> `cargo test --workspace`, which **cannot pass and never could** — an upstream doctest at
> `libs/toolkit/src/api/operation_builder.rs:932` fails to compile identically on `main`, and this
> branch has never touched `libs/toolkit/` (ruling R75). The `--lib --bins --tests` form above is what
> the Verification-gate section already mandates, and it is what was actually run. The second line is
> fine as written: `--all-targets` on **clippy** only compiles
> `libs/toolkit-db/benches/worker_overhead.rs`, whereas `--all-targets` on **`cargo test`** *runs* it —
> 20+ minutes, and two agents abandoned it believing it had hung.
>
> **One more thing this task's text got wrong and the implementation got right:** Step 1 names the new
> file `tests/ingest_idempotence.rs`, and what ships there is not an idempotence test — it is a
> boot-and-project end-to-end test plus a ticker-lifecycle test. The name was kept because this task's
> `**Files:**` line specifies it and nothing else references it; the plan is the half that is wrong.

- [x] **Step 6: Commit and squash the phase** — **COMMIT DONE, SQUASH DELIBERATELY WITHHELD (ruling R110).** The `git commit` half is done. The squash half is a history rewrite over ~40 commits, and the Phase B squash is owed on top of it; the plan itself records that Phase B was left unsquashed **at the user's explicit decision**, so squashing is a call a human has already taken once in the direction of "not yet". Both squashes are surfaced to the human as the last open item rather than performed by an agent.

```bash
git add gears/qa-platform/qa-insights/
git commit -m "feat(qa-insights): gear wiring and end-to-end ingest"
```

Then squash Tasks 31–40 into
`feat(qa-platform): qa-insights — JIRA loop and notifications`.

---

---

### What Task 40 established — and the five items now waiting on a human

Task 40 closed the composition root, and the phase-level review that followed it is the last review
this branch gets. Between them, seven things bind whoever picks this up.

* **R86's rule text has a hole, and the seventh instance found it.** The rule as written says every
  repository read ending in `.one()` must take a `tenant_id`, validate it against the scope, and carry
  the predicate. `resolve_bug` is an **`update_many`** — a write — and it had none of the three, sitting
  between two methods that each spend a paragraph explaining that a scope over `OWNER_TENANT_ID` may
  span tenants. Tenant P's poller pass would have resolved child tenant C's identically-keyed bug
  against a different JIRA instance; C's test leaves C's skip list, C's poller never re-evaluates it
  (no longer `Open`), and C's `POST /jira/bugs` answers `created: false` for a bug nobody resolved.
  `list_open` was unpinned too, so P's pass could `launch_test` a rerun for C's `repo_id`. Both are
  fixed. **Restate the rule as "any statement whose predicate does not include a tenant-unique key",
  reads and writes alike** — and note the fix's own test had to be built structurally rather than to
  the prescription, because `qa_jira_bugs` has no `run_finished_at` column and an `update_many` has no
  ordering trap: `AccessScope::for_tenants(vec![mine, theirs])` puts both rows inside the scope, so
  dropping the predicate is red for either tenant-id ordering and either insertion order.
* **Every behavioural defect the final review found was a cross-task seam** — a rule established in
  one task and not carried into the task that consumed it. The unpinned write (Task 33's rule, Task 35's
  caller), the unvalidated Slack reference (Task 32's `validate_credstore_ref`, Task 38's `save_config`
  — which ruling **R77 had forecast by name**), the claim path authorized by `actions::GET` (Task 35 got
  the sibling case right *with a paragraph explaining it*; Task 38 wrote `GET` with no comment). Ten
  per-task reviews cannot see any of these, by construction. **A short seam review at the JIRA/
  notification boundary — after Task 35 — would have caught three of the four at a fraction of the cost.**
* **The tickers are inert on every deployment in this tree, and that is the expected state, not a bug.**
  They enumerate tenants under a nil-tenant system actor and **both** authz plugins deny a nil-tenant
  subject. qa-runs' dispatcher enumeration is in the identical state. The code defends itself against
  being read as a wiring bug — `tenants.rs` documents it with the deployment obligation, and the
  integration test's own doc records that this is why no tick did work. Do not "fix" it in code.
* **`TenantBound` is the pattern to copy, not just a type.** A newtype in its own module so the field's
  module-privacy actually binds; six tenant-bound factories that take it; one enumeration factory that
  cannot produce it. That is what makes "cross-tenant authority must not leak into per-tenant work" a
  compile-time fact rather than a convention, and it is the right answer to R86's highest-risk instance.
* **The plan was wrong twice in this task and the implementation was right both times** — the
  tenant-enumeration option was provably circular, and Step 5's literal gate could not pass. Both are
  corrected in place above. **The plan is an argument from the spec, not an authority over it.**
* **The concurrency property still rests on the schema, not on a test**, and nothing may re-describe it
  otherwise: `idx_qa_run_notifications_claim`'s unique index plus `ON CONFLICT DO NOTHING` is what makes
  the dedupe correct. The unit tier serializes writers with `max_conns: Some(1)`, so the race is not
  demonstrated on any tier, and `notify_tests.rs` says so in its own header.
* **`gear.rs` is 1685 lines and the seam is already named** — the three ticker constructors, `Ticker`,
  `Tickers`, `supervise` and the three passes, into `gear/tickers.rs`, with `TenantDirectory` narrowed
  from `pub` to `pub(crate)` in the same edit. Three independent readers picked the identical cut.
  Deferred under ruling **R114** only because the final fix wave got exactly one review seat and moving
  ~930 lines of composition root through it alongside a cross-tenant write fix is how a mechanical
  refactor hides a real defect. **It is the first follow-up, and the cut does not need re-deriving.**

#### The five release-gate items, in the order they should be decided

1. **R112a + R112b together, and C1's fix is their precondition.** Granting this gear's system actor a
   cross-tenant `qa.test_result` list is what makes the three tickers run at all — and the same grant
   makes the JIRA poller live under `NoopLeaderElector`, which makes **every replica the leader**, with
   `rerun` launching through qa-runs' admission path bounded only by a local `resolve_bug` write:
   "a race, not a lock", in the code's own words. It needs a claim row no task owns. Decide these two as
   one thing.
2. **R107 — the Slack egress is built, bounded, tested and undeliverable.** A Slack webhook's secret is
   its URL path; oagw's plugins inject only headers; this gear has no `credstore-sdk`. It degrades
   correctly (claim released, `OUTCOME_FAILED` audit row). **Both candidate fixes are bigger than they
   look**: `proxy_request`'s URI convention is `/{alias}/{path_suffix}`, so there is no way to hand oagw
   a complete URL — the credstore route additionally needs the upstream provisioning the JIRA adapter
   spent ~250 lines on. And "undeliverable" is only true of the *intended* design: an operator can make
   Slack work today by storing the raw webhook URL in `slack_webhook_credstore_ref`. The write path now
   refuses that, which is the point — but it means a tenant whose row already holds one can no longer
   round-trip `GET` → `PUT`.
3. **R111 — `notify_run_completed` has no producer.** The consumer's routing table skips
   `run.canceled` / `run.queue_expired` / `schedule.fired`, which Tasks 36-39 assumed "Phase C" would
   wire. Ruled a behaviour change, not wiring, and deliberately not added. **Sequence it after the
   claim-path authorization fix**, which is inside the code it would switch on.
4. **R74 — worse than previously recorded.** The item said the skip list is servable-and-unserved. It is
   not just that qa-runs does not call the provider: **`render_skip_list` — the frozen wire format an
   external contract pins — has no caller anywhere in the tree.** The remaining work includes rendering,
   not only a call. `EnvInputs` is the field that would carry `SKIP_TESTS_WITH_BUGS`
   (`qa-runs/src/domain/env_assembly.rs:189`) and qa-runs itself records that it cannot set it yet
   (`dispatch_spec.rs:483`).
5. **R95 and R99, still open from Phase B**, neither silently fixed: three `NotificationConfig` fields
   are dead in legacy (a PRD gap, not a code defect), and the `VHP` → `QA` rebrand spans at least the
   email subject and the JIRA issue summary and should be decided for both at once or neither.

#### Both squashes are owed and neither was performed

Phase C's squash is this task's Step 6 and Phase B's has been owed since 2026-08-24. **Ruling R110
withheld both from every agent in this session**: each is a history rewrite over ~40 commits, and the
plan records that Phase B was left unsquashed at the user's own explicit decision — so this is a call a
human has already taken once, in the direction of "not yet". Whoever performs them should follow Phase
A's precedent exactly: a backup branch first, and **`git diff` between the pre- and post-squash heads
being empty as the acceptance test**. `backup/pre-phase-a-squash-2026-08-21` and
`backup/pre-4-commit-squash-2026-08-24` must not be deleted.

## Spec coverage

| Requirement | Tasks | Note |
|---|---|---|
| `cpt-cf-qa-fr-insights-history` | 2, 3, 10, 12, 13, 14, 17 | Both granularities (D1); transactional projection; OData collections. **One named parity gap, added 2026-08-21 by Phase A's whole-phase review: a client cannot render one test's history in chronological order.** Legacy does it in one query — `WHERE tr.test_file = $1 ORDER BY COALESCE(rr.finished_at, rr.created_at) DESC LIMIT $2` (`manager/src/routes/tests.rs:308-318`) — and `GET /qa/v1/test-results` can express the `WHERE` but no chronological `ORDER BY`: `run_finished_at` is deliberately non-orderable (the cursor codec cannot encode a `NULL`), `created_at` is in no index and so in no enum, and the default order is `id DESC` over a random v4 UUID. The rows are all there and the order is arbitrary. **The real fix is a new migration adding `(tenant_id, test_file, run_finished_at DESC)` plus an orderable key guarded on non-null** — not `COALESCE(...)`, which no index covers. **Owner: Task 27**, which ports `api_plan_test_history` (`manager/src/routes/analytics.rs:2530`, `ORDER BY t.test_name, r.finished_at DESC NULLS LAST`) and so is the first task that must produce this ordering for a surface a user reads; its fork is stated on `infra::storage::odata`'s header. Not done in the review wave that found it: an index plus a cursor-key design change is feature work. |
| `cpt-cf-qa-fr-insights-dashboard` | 18, 19 (shape only), 21, 25 | Eight computed sections (D3); legacy's day clamps and counter folds. **NOT fully discharged, corrected 2026-08-21 by Task 19:** the clause's coverage bullet (`docs/PRD.md:580`) is unowned — Task 19 registers the endpoint and it answers empty in every deployment (no log-text upstream before p2; `product_key` is open question 1), and the bullet's own wording describes *execution* coverage where legacy computes *code* coverage, which no decision reconciles. Escalated, not settled — see carried item 13. |
| `cpt-cf-qa-fr-insights-analytics` | 20–28 | Eleven routes; legacy parameters on aggregates, OData on collections (D7) |
| `cpt-cf-qa-fr-insights-expected-cases` (new, Task 1) | 7, 29, 30 | Static from qa-catalog, exact via the `Collect` run kind (D2) |
| `cpt-cf-qa-fr-insights-jira` | 31, 32, 33, 34 | Registry is the source of truth, not TEST_META; frozen skip-list format |
| `cpt-cf-qa-fr-insights-auto-rerun` | 35 | New-build gate (D8); normal admission path |
| `cpt-cf-qa-fr-insights-notifications` | 6, 36, 37, 38, 39 | Full surface (D5); per-schedule settings (D9); email deferred (D10) |
| `cpt-cf-qa-fr-insights-reportportal` | 3, 10, 14 | `launch_id` travels the event and lands on `qa_test_results`; run views render it |
| `cpt-cf-qa-principle-async-insights` | 13, 14, 15, 18 | Ingest is asynchronous; the only synchronous reads are insights→runs at query time |
| `cpt-cf-qa-nfr-scale` | 10, 17, 11/12 | Indexes on both flat tables; OData paging clamped by `infra::storage::db::PAGE_LIMITS` (200/500) with a filter allow-list gated on those indexes; **the analytics read path's window** — see the note below |
| `cpt-cf-qa-fr-migration-runner-contract` | 5, 31, 34 | `COLLECT_ONLY`, `VHP_COLLECT_URL`, `SKIP_TESTS_WITH_BUGS` all frozen |
| `cpt-cf-qa-component-insights` | 8, 9, 40 | Gear pair, composition root, three leader-elected tickers |
| **Deferred, recorded** | 39 | The email send (D10). Config, routing, dedupe and logging all ship. |

**"The subsystem's shared clamp" was a phrase, not a thing** (Task 17). There is no shared
constant and no shared knob: `qa-runs`' `PAGE_LIMITS` is `pub(crate)`, and the import fails to
compile. `qa-insights` re-declares the same 200/500 with the reason recorded on its own constant.
And `toolkit_odata`'s `ODataLimits::with_max_top`, which `config.rs` named as "the platform's
ceiling ... applied at the route", is applied **nowhere** — it has no caller in `libs/` or
`gears/` outside its own module's tests, and the `OData` extractor never constructs one. The only
ceiling in this path is `LimitCfg` inside `paginate_odata`.

## Open items carried into execution

1. **Analytics' product/version parameters.** `product_id`, `version` and `scope` presuppose legacy's product-version model, and VHP-319 deleted `product_versions` — qa-catalog's shipped schema has `products` + `repo_branches` instead. This is the largest unknown in the plan.

   **Reassigned from Task 20 to Task 25 (2026-08-21, by Task 20).** Task 20 was named because it authors `UniverseFilter`'s first consumer; it turned out not to have one. The universe core is pure — it takes `&[UniverseTest]` and `&[ExecRow]` and never sees a product, a version or a scope — so there was nothing in that task for the mapping to be recorded *against*, and answering it there would have been inventing a shape with no call site. Tasks 21-24 are pure folds over an already-fetched `Vec` for the same reason. **Task 25 is the first task that turns a request into a read**: it authors `domain/service/analytics.rs`, ports `normalize_overview_query` (`:2392`) and `parse_scope` (`:2104`), compiles the `UniverseFilter`, and passes `product_id` to `CatalogReader::list_universe`. Resolve the mapping when authoring **Task 25's Step 0** and record it there. Task 20 deliberately guessed at nothing; see its report §11.
2. **`quality_vectors_cache`** (`routes/analytics.rs:1980`). Legacy caches quality vectors process-wide. The gear reads them from qa-catalog per request. Task 23 Step 0 decides whether that is acceptable latency or whether a bounded cache ports too.
3. **Retention.** PRD §11 leaves the retention policy for results open. Unaffected by this plan; both flat tables carry the indexes a purge would need.
4. **`cargo-shear`** is not installed on the development machine, per `qa-runs/qa-runs/Cargo.toml`'s note. Whether Task 9's `[package.metadata.cargo-shear]` entries silence it is unverified until CI runs.
5. **Bounding the analytics read path** (added 2026-08-20 by the code review of Task 11). The NFR row for `cpt-cf-qa-nfr-scale` used to name Tasks 10 and 17 only — indexes, and OData paging over Task 17's two flat collections. **Neither covers `ResultsRepository::list_for_universe`, which is the read that actually meets the 5M-row target**: it returns an unaggregated `Vec<ExecRow>`, and legacy's equivalent query is bounded only by `app_version` because legacy applies every time window in memory after fetching. A requirement with no task-indexed owner is exactly the omission the end-of-phase audit exists to find, so it is written down here instead.

   Task 11 gave `UniverseFilter` a `since: Option<OffsetDateTime>` lower bound — additive, no consumers yet, and far cheaper now than after Task 12's call sites accumulate. **Task 12 owns making it reach an index**, and the subtlety is recorded on the field: the quantity being windowed and ordered is legacy's `COALESCE(run_finished_at, run_created_at)` — **`run_created_at`, since Task 21b's ruling C; this said `created_at`, which is the row's ingest instant rather than the run's, and Task 21b added the column that makes it legacy-exact** — which no index covers, but when `finished_only` is set that expression is provably just `run_finished_at` — so the query must be written with the bare column to use `idx_qa_test_results_tenant_finished`. **Task 25 owns actually passing a bound** — **reassigned from Task 20 (2026-08-21, by Task 20)**. The original reasoning ("the task that knows each surface's window") picked the wrong task: the windows are *clamped inside the pure folds* (`build_heatmap:1452` to `[1,30]`, `build_trend:1496` to `[7,365]`, Task 22) and those folds run over a `Vec` somebody else already fetched, so no task from 20 to 24 issues a `list_for_universe` call at all. The bound has to be the **widest** of the three windows, computed where the single read happens, which is Task 25's `domain/service/analytics.rs`. Task 20 built the core with no repository and no `async` and so had no call site to pass a bound from.

6. **`qa_ingest_watermarks.last_swept_at` has no consumer and no task owns one — a decision for a
   human.** Added 2026-08-21 by Phase A's whole-phase review. The column is documented as "when
   the stale-in-progress sweep last ran", and **there is no such sweep**: the reconciler
   deliberately never touches in-progress runs (`domain::service::reconcile`'s header, recorded
   behaviour 1 — `list_runs_finished_since` never returns a `NULL` `finished_at`), and Task 18
   settled the consequence the other way, by having the dashboard read active runs from qa-runs
   live. So the column, `WatermarkKind::SweptAt`, its repository support in
   `watermark_sea_repo::advance` and the three tests that exercise it
   (`an_advance_no_op_inside_a_transaction_leaves_the_transaction_usable`,
   `the_two_marks_are_independent_columns_of_one_row`, and `entity::round_trip_every_entity`) are
   shipped machinery with no forecast caller.

   Recorded honestly on `domain::repos::watermark_repo` rather than deleted: removing it means
   either editing a shipped migration (the append-only rule forbids it) or adding a second
   migration to drop a column, and neither is a review-fix action. **The decision is whether
   Phase B drops it or gives it a consumer** — and if it keeps it, the doc must say what the
   consumer is. Nothing depends on the answer, which is why it is an open item rather than a
   task.

7. **`domain/service/dashboard.rs` is 893 lines carrying a 321-line module header (measured at
   the fix wave's commit; 890/319 when the review triaged it), and the split is assigned to Task
   21b.** Triaged 2026-08-21 as "acceptable to carry" by Phase A's whole-phase review —
   deliberately *not* split in a review fix wave — and assigned rather than left to whoever
   notices. **Task 21b** is the owner because it is the first Phase B task that must edit this file
   (carried item 13 gives it `failed_recent` and the four 24-hour counters), and it will add to
   both the code and the header. Reassigned from "Task 21" to 21b when the controller split that
   task on 2026-08-21: 21a is a pure fold in `domain/analytics/` and never opens this file.

   **The preferred shape is to move the coverage transcript onto `CoverageBuildDto`.** The
   coverage material is the largest self-contained block of that header — Task 19's Step 0
   transcript, the two missing upstreams with their citations, and the execution-versus-code
   coverage disagreement — and it is about a *payload* that answers an empty array, not about the
   aggregate service. Moving it puts it where a reader of the type finds it and takes the header
   back to the numbers this service actually computes. Splitting the *service* instead was
   considered and is worse: the file's functions share `Counters` and the run listing, so a split
   along function lines would need one of them exported or duplicated.

## Execution handoff

The plan is complete. Both execution routes are supported:

1. **Subagent-driven** (recommended) — a fresh subagent per task with review between tasks. Use `superpowers:subagent-driven-development`.
2. **Inline** — batch execution with checkpoints. Use `superpowers:executing-plans`.

Either way, run Phase 0 to completion and confirm the qa-runs suite is still green before starting Phase A. Phases B and C may then proceed in either order, or in parallel worktrees via `superpowers:using-git-worktrees`.
