# qa-runs Gear Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `qa-runs` gear — DECOMPOSITION features **2.3** (Run Orchestration Core, `cpt-cf-qa-feature-runs-core`) and **2.4** (Schedules, `cpt-cf-qa-feature-schedules`): launch validation, three-tier exclusivity resolution, per-platform FIFO queue with admission limits, dispatcher with crash recovery, run state machine, environment assembly, cancel/re-run, control-plane timeout, lifecycle events, SSE logs, and cron schedules fired exactly once — all against a **mock `RunExecutor`**.

**Architecture:** Standard ToolKit DDD-light gear pair under `gears/qa-platform/qa-runs/`, structurally identical to the two shipped gears (`qa-environments`, `qa-catalog`). Four pure TDD cores hold the frozen semantics (exclusivity precedence, queue admission/FIFO planning, run state machine + phase derivation, cron due-tick computation) and are written test-first before any I/O exists. Execution sits behind the `RunExecutor` domain port with a deterministic mock adapter (ADR-0001); the real serverless adapter is feature 2.7 and is out of scope. Occupancy is **not** re-derived locally: `qa-environments`' lease is the authoritative occupancy oracle, replacing legacy's "Argo listing ∪ DB claims" merge.

**Tech Stack:** Rust, ToolKit stack (`toolkit`, `toolkit-db` SecureORM, `toolkit-security`, `authz_resolver_sdk::PolicyEnforcer`, `OperationBuilder`, SeaORM + `sea_orm_migration`), `qa-catalog-sdk` + `qa-environments-sdk` via `ClientHub`, `event-broker-sdk` for lifecycle events, `cluster-sdk` for leader election, `cron` for schedule evaluation, `tokio` + `tokio-util::CancellationToken` for lifecycle tasks.

**Specs:** `gears/qa-platform/docs/PRD.md` §5.2; `DESIGN.md` §3.1 (state machine), §3.2 (`cpt-cf-qa-component-runs`), §3.3, §3.6, §3.7; `DECOMPOSITION.md` 2.3 + 2.4; `ADR/0001-cpt-cf-qa-adr-serverless-execution.md`, `ADR/0002-cpt-cf-qa-adr-structured-events.md`; parity spec `docs/superpowers/specs/2026-08-12-qa-platform-legacy-parity-design.md` §3.4.

**Frozen semantics:** `../testrunner/docs/guides/exclusive-runs-and-the-queue.md`. Every rule taken from that guide carries an explicit legacy-verification step in the task that implements it (see "Legacy-verification protocol" below).

---

## Plan shape: one plan, two phases

**Phase A — Run Orchestration Core (2.3), Tasks 1–16.** Ships a working launch → queue → dispatch → mock-execute → ingest path.

**Phase B — Schedules (2.4), Tasks 17–20.** Adds schedule CRUD, the cron evaluator, and the leader-elected firing task.

**Why one plan, two phases** (the prompt left this to the plan author): schedules have no independent value — a schedule's only action is to call the launch path, and PRD `cpt-cf-qa-fr-runs-schedules` makes "the same internal run-creation path as manual launches" the requirement itself, so Phase B is a thin adapter onto Phase A's `LaunchService`. Splitting it into a separate plan document would duplicate the whole file map, the verification gate, and every legacy-verification citation for the sake of four tasks. The two phases are still **separately squashable deliverables** (see "Commit discipline"), so reviewability is preserved without the duplication.

---

## Decisions taken before writing this plan

Four spec-vs-legacy conflicts were checked against `../testrunner` and resolved with the user. The governing principle stated by the user: **preserve legacy behavior; adapt only the implementation to the gear architecture.** Each resolution below names the spec claim and the legacy truth, per the protocol.

### D1 — Queue model: port legacy's seven states and three limits; amend DESIGN

| | |
|---|---|
| **Spec claims** | `DESIGN.md:581` — `run_queue.state` is `queued / dispatched / canceled`. `DESIGN.md:580` — `position BIGSERIAL` stores FIFO order. Nothing in PRD or DESIGN mentions a queue TTL, a per-platform depth limit, or a cluster-wide concurrency cap. |
| **Legacy does** | Seven states: `queued`, `dispatching`, `running`, `done`, `failed`, `cancelled`, `expired` (`run_queue.rs:104` `CLAIM_STATES`, `:281-421` the transitions, guide lines 88–97). **No stored position** — FIFO is `ORDER BY enqueued_at ASC, id ASC` (`run_queue.rs:243-257`) and `queue_position` is computed per request over the returned window (`run_queue.rs:485-507`, guide lines 171–184). Three limits, all in the guide's "Limits" table (lines 125–135): `max_concurrent_runs` → 429 (`run_queue.rs:879-891`), `queue_ttl_seconds` → TTL sweep to `expired` **with a mandatory alert** (`run_queue.rs:370-401`, `run_dispatcher.rs:495-566`, guide line 96), `queue_max_depth` → 429 (`run_queue.rs:830-832`). |
| **Deliberate?** | Yes. Every one of the seven states is documented in a frozen user-facing guide with its own row, and the two 429 causes are enumerated as "exactly two" (guide line 74, 240–242). This is specified behavior, not accident. |
| **Resolution** | **(a) Port legacy.** DESIGN §3.7's three-state list is a sketch that was never reconciled; Task 1 amends it. |

### D2 — Run phase: any SKIPPED test downgrades a Succeeded run to Failed; ported as-is

| | |
|---|---|
| **Spec claims** | `DESIGN.md:240` — the state machine is `… → (succeeded \| failed \| canceled \| timed_out \| error)` with no downgrade rule. |
| **Legacy does** | `argo.rs:2170-2191` `derive_phase_from_result_flags`: `if (has_failed \|\| has_skipped) && matches!(base_phase, "Succeeded" \| "Skipped") { "Failed" }`. `has_skipped` is any test result with status `SKIPPED` (`argo.rs:2197-2199`). Because legacy also feeds `SKIP_TESTS_WITH_BUGS` to the runner (`argo.rs:476-481`), a run that skipped known-broken tests reads as **Failed**. |
| **Deliberate?** | Yes — the code comment states the intent explicitly: "a skipped test means the run didn't fully execute, so it must never read as passing either" (`argo.rs:2180-2181`). The rule is also idempotent by construction (only `Succeeded`/`Skipped` are ever downgraded), which is a designed property, not a side effect. |
| **Resolution** | **(a) Port legacy, as-is,** including the JIRA-skip interaction. Task 1 amends DESIGN §3.1 to record the rule. |

### D3 — Reserved control variables: exactly the eleven legacy names, plus legacy's size caps

The user asked whether the `VHP_PROGRESS_URL` exposure was a legacy behavior or a new design decision. **Checked: it is legacy behavior.**

| | |
|---|---|
| **Spec claims** | `PRD.md:301` — the reserved list is exactly `APP_BUILD, APP_VERSION, E2E_K8S_NAMESPACE, KUBECONFIG, PRODUCT_KEY, RP_API_KEY, RP_PROJECT, SKIP_TESTS_WITH_BUGS, TEST_BUNDLE_URL, TEST_FILES, TEST_VERSION`, rejected case-insensitively; names match `^[A-Za-z_][A-Za-z0-9_]*$`; unique; at most 50. |
| **Legacy does** | `routes/settings.rs:15-27` — **the same eleven names, verbatim and in the same order.** `VHP_PROGRESS_URL` (the per-test progress callback, `argo.rs:438-441`) and `E2E_VHP_BASE_URL` (`argo.rs:493-496`) are **not** in the list, and `append_run_parameters` → `append_platform_variables` (`argo.rs:~`) does retain-then-push, so a run parameter of either name **does** replace the platform-supplied value. Legacy additionally enforces three caps the PRD omits (**corrected 2026-08-13 during Task 8: these were called "undocumented" here and in the PRD amendment. They are not** — the frozen guide `../testrunner/docs/guides/run-parameters.md:40-42` states all three. What it gets wrong is the *unit*: it says name length is "At most **128** characters" where the code measures **bytes** (`String::len`). The two agree for every name that survives the charset check and diverge only in the pre-charset window, which is exactly where the byte-cap test lives): `MAX_RUN_PARAMETERS = 50` (matches the PRD), `MAX_RUN_PARAMETER_NAME_LEN = 128`, `MAX_RUN_PARAMETER_VALUE_LEN = 8 * 1024` (`routes/settings.rs:32-34`, enforced at `:118-151`). |
| **Resolution** | **Preserve legacy.** The reserved set stays at exactly eleven names — the ingestion-callback variable remains overridable, matching the source system. The two size caps **are** legacy behavior that the PRD failed to record, so they are ported and PRD `cpt-cf-qa-fr-runs-params` is amended in Task 1 to state them (narrowing the requirement, keeping the FR id). A `SECURITY NOTE` comment at the reserved-list definition records the unreserved-callback exposure as inherited parity so the next reader does not "fix" it silently. |

### D4 — Snapshot read race: at parity with legacy; bounded retry, no new snapshot machinery

The user asked the same question here. **Checked: the race is legacy behavior.**

| | |
|---|---|
| **DECOMPOSITION claims** | `DECOMPOSITION.md:126` (corrected 2026-08-14; `:125` is the `sync_repo` follow-up and this wrong number propagated into shipped code at `dispatch.rs:803`) — reads do not serialize against snapshot rewrites; "The source system had the same coarseness over its single working copy, so this is at-parity." |
| **Legacy does** | Confirmed. Legacy has the *same two-tier lock structure* qa-catalog has — `sync_locks: HashMap<String, Arc<Mutex<()>>>` keyed `"{repo_id}::{branch}"` plus a per-repo lock keyed `"{repo_id}::*repo*"` (`services/test_repos.rs:30, 543-558`) — and the locks are taken **only by writers** (`:405-406`, `:452-456`). Readers — `plans::find_plan_in_repository_root` and `test_bundles::create_bundle_from_checkout`, both called from `submit_plan_run` (`routes/runs.rs:662-722`) — walk the checkout with no lock at all. Two concurrent launches on the same `(repo, branch)` race in legacy exactly as they would here. |
| **One honest difference in degree** | Legacy updates a branch directory via `git worktree` (`services/test_repos.rs:463-467`, `sync_repository_worktree`), which rewrites files in place; qa-catalog's `gix_sync.rs` **clears** the snapshot before rewriting it. Same class of race, but legacy's window exposes a *partial* read while qa-catalog's also exposes an *empty* one. |
| **Resolution** | **Accept at parity.** No generation-numbered snapshot directories and no reader lock — both would be new design, and the user's principle rules them out. qa-runs closes only the widened part: the launch path performs **one bounded retry** when discovery returns empty immediately after its own force-sync (Task 10, Step 5), which reduces the exposure to legacy's. DECOMPOSITION 2.2's follow-up stays open with the named future fix; Task 1 appends the parity finding and this decision to it. |

### Decisions inherited, not re-litigated

From the continuation prompt and ADRs 0001–0005: four domain gears + qa-ui; `qa-*` crate naming; full Fabric tenancy; execution behind `RunExecutor` with a mock in p1 (ADR-0001); structured events via event-broker SDK (ADR-0002); branch `feature/qa-platform-specs`, local, one squashed commit per deliverable.

### Two decisions this plan takes on its own authority (flagged for review)

- **`plan.yaml`'s `validation` flag is re-homed here.** DECOMPOSITION:131 and :346 note it is the one unported `plan.yaml` field with a live legacy consumer — it stamps a `vhp-tests/validation` annotation (**written** at `argo.rs:628-630`, `if plan.plan.validation { annotations.insert(...) }`; `argo.rs:2289-2292` is where it is **read back** into `is_validation`. Corrected 2026-08-14; verified — cite the write site when claiming something is stamped). Its natural home is a run classification. Task 5 adds `is_validation: bool` to the run record, derived from the plan's `validation` bool OR'd with a case-insensitive `validation` **tag** (legacy: `services/plans.rs`). **Caveat: qa-catalog does not currently parse `validation`.** Task 2 therefore adds it to `qa-catalog`'s `plan.yaml` parser and to `sdk::Plan`. If the reviewer prefers to defer this, drop Task 2 Step 4 and Task 5's `is_validation` field together and record the deferral in DECOMPOSITION 2.3.
- **`git_plans` runs are not a run kind.** Legacy `RunIntent` has four variants (**`manager/src/models.rs:1571-1592`** — `services/run_dispatcher.rs:184-208` is a `match` body in `submit_unqueued`, not the enum; corrected 2026-08-14, verified): `Plan`, `Test`, `CustomPlan`, `GitPlan`. `GitPlan` is the second, unported plan reader that DECOMPOSITION:153 explicitly dispositions as out of scope. qa-runs therefore has **three** run kinds. Consequence recorded in Task 5: legacy's `custom_plan_rerun_kind` three-way discrimination (`routes/runs.rs:107-118`) collapses to a two-way one, and legacy's `Unresolvable` rerun 404 (`routes/runs.rs:1043-1055`) becomes unreachable — do not port it.

---

## Legacy-verification protocol (applies to every task below)

qa-catalog's plan put legacy checkpoints on two of its three frozen contracts and drifted in precisely the third. Every task in this plan that ports behavior carries a step in this exact shape:

> **Legacy check:** read `<file:line>`, confirm `<specific rule>`, cite the `file:line` in a code comment next to the logic it justifies. If legacy differs from this task's description, **stop and report** — spec claim with `file:line`, legacy behavior with `file:line`, whether legacy looks deliberate (migration comment / guide / VHP ticket), and which of (a) port legacy (b) keep spec + amend the claim (c) deliberate divergence — then ask with an `AskUserQuestion` menu. Do not resolve it yourself.

**Verify the prose, not just its citations** (added 2026-08-13, after Task 8). This subsystem treats documentation as specification, so a false doc comment is a defect rather than a nit — and **no amount of mutation testing can find one.** Break-testing proves an assertion discriminates; it says nothing about whether the sentence justifying that assertion is true. Task 8 shipped a paragraph in which every `file:line` resolved correctly and the claim was still false, because the premise rested on `slugify` while the code path ran through `default_base` — plus a citation to a legacy call site that does not exist, which actively pointed the next implementer at the one value that breaks the invariant. So: for each **load-bearing claim**, re-derive it from the source rather than checking that its citation resolves. The two questions are different, and only the first one is being asked today.

**Why the pass fails when it fails, diagnosed by Task 12.** Its two falsified guarantees came from *writing the guarantee you were aiming for rather than the one the code delivers* — and in both cases the aim was ambitious, which is exactly when the gap opens. Its own comparison is the useful one: `OwnedRunId`'s limit statement is accurate **because its author went looking for the hole after building the thing**. Task 12 did that for `normalize_status`, whose prose held, and not for `TenantScoped`, which it wrote last and defended immediately. So the pass is not a proofread — it is a search for the counter-example, run *after* the design feels finished and *especially* on the part you are proudest of. A claim that cannot be tested must say so.

**The shape these false claims take, named by Task 10 after five of them.** Every one was *a sentence written while reasoning about a neighbouring thing that had just been verified* — the FK checked and the PK assumed, a two-scope design described after a token replaced it, `DISTINCT` reasoned about from the wrong select list, a guide citation read backwards, a trim reported that was never applied. **The citations all resolved; the claims did not follow from them.** So the pass is cheapest run on your own prose immediately after writing it, while the neighbouring fact is still in view — and it must ask "what would make this false?", never "does this citation still say what it said". Note this applies to *handoff notes and review relays* as much as to code comments: three of the five were written by the coordinator while relaying a finding rather than while reading the code, and handoff notes are what the next task actually reads.

**Make that pass adversarial, and give it to a different reader.** Task 8's implementer found the refinement that makes the rule work: in all three of its false premises the disproving source line had *already been read* — one paragraph asserted "`default_base` is taken unslugified" and "`name_base` can never emit `_`" in the same file, 130 lines apart, in the same sitting. The information was never missing; the step that asks *"does this claim survive the other things I know?"* was. Re-reading a citation confirms it forever; only asking **"what input would falsify this claim, and does that input exist?"** finds the hole. It is break-testing applied to sentences — a guard is proven only by trying to break it. And since the author's base rate on catching their own false premises was zero for three, this pass belongs to the spec or quality reviewer, who must state explicitly which load-bearing claims they attempted to falsify and how.

**Check composition against the consuming task, not just the function against legacy** (added 2026-08-13, after Task 7). The pure cores are *designed* to be composed by callers three to eight tasks away, and a composition can be wrong while every function in it is individually correct and every mutation individually caught. Task 7 shipped `reconcile_recorded_state` and `can_transition`, both correct, both fully mutation-covered — yet the pipeline Task 15 specifies (`derive → reconcile → conditional update`) turns a duplicate completion event into an HTTP 409 instead of a no-op, because a terminal state legitimately refuses even a self-transition. **No unit test inside a pure module can reach that defect.** So: every implementer of a pure core must read the consuming task's pipeline and document the composition rules its functions require; every reviewer must check that pipeline explicitly rather than only the module.

Two further rules, both from qa-catalog's post-mortem:

- **A passing test is not parity evidence.** Every assertion's expected value for ported logic must come from *reading legacy*, never from running the new code and recording what it did.
- **Concurrency fixes are verified by breaking them.** For every regression test on a race, temporarily remove the fix, watch the test fail, restore it. "The test passes" is not evidence.

---

## Architecture mapping: legacy → gear

Read this before Task 5. It is the single largest source of "the plan says X but legacy says Y" confusion, because the *semantics* are ported unchanged while the *mechanism* is entirely replaced.

| Legacy mechanism | qa-runs equivalent | Note |
|---|---|---|
| Argo `Workflow` object **plus the `run_results` table** = system of record for a run | `qa_runs` row | **Corrected 2026-08-13 (Task 4 execution).** This row previously read "Legacy has no `runs` table; nothing to port." **That is false.** Legacy keeps a `run_results` table read as `PersistedRunRow` (`manager/src/services/run_history.rs:10-48`) and merges it with the live Workflow in `overlay_persisted_with_live` (`:216`). The migration says why outright: *"Persisted because the Argo Workflow object is collected after its TTL while reruns happen much later"* (`manager/migrations/001_initial.sql:306-310`). So the run record is **two halves**, and `parse_workflow` (`argo.rs:2236-2319`) is only the live half. The field inventory must be taken from **both**; `PersistedRunRow` adds `created_at`, `run_parameters`, and `raw_logs` beyond the Workflow, and is the reason `app_version`/`app_build` are snapshotted rather than re-derived. `cpt-cf-qa-principle-db-first-state` still requires the single `qa_runs` row — the *conclusion* survives, only the premise was wrong. |
| Argo `CronWorkflow` = system of record for a schedule | `qa_schedules` + `qa_schedule_ticks` | Net-new. Legacy delegates cron evaluation to Argo entirely; the whole delete-and-recreate dance (`routes/schedules.rs:674-745`) is an Argo artifact that disappears. |
| `run_queue` table | `qa_run_queue` | Direct port, seven states (D1). |
| Occupancy = `list_workflows()` ∪ unreleased claims, merged by key (`run_queue.rs:657-712`, `run_dispatcher.rs:221-254`) | `qa-environments` **lease** is the authoritative oracle; `acquire_lease` → `Acquired`/`Busy` *is* the admission decision | The merge exists in legacy only because Argo and the DB were two independent views of the same fact. Here there is one view. `LeaseState` (`qa-environments-sdk::LeaseState`) maps to a local `Occupancy` for the pure planners. |
| "Unreadable Argo ⇒ platform reads as busy" (`run_dispatcher.rs:237-252`) | A failing `get_lease`/`acquire_lease` ⇒ treat as busy, queue | Port the fail-safe direction verbatim. |
| `activeDeadlineSeconds` on the workflow spec (`argo.rs:539`) | Control-plane `timeout_at` column + dispatcher sweep | **Deliberate divergence, already sanctioned by the spec**: PRD `cpt-cf-qa-fr-runs-timeout` requires control-plane enforcement because "enforcement cannot rely on the execution backend alone" (**`PRD.md:404`** — corrected 2026-08-14; `:376` is a different line inside `cpt-cf-qa-fr-runs-execute`. Task 14 cited `:404` correctly, so the code is right and the plan was wrong). Not a legacy conflict. |
| `vhp-tests/*` annotations | `qa_runs` columns | The annotation *codec* (`exclusivity.rs:52-79`) still ports: `true`/`false`/`auto` tri-state on the schedule's stored choice. |
| Slack notifications (`services/notifications.rs`) | Out of scope — feature 2.8 | Except the **mandatory `expired` queue alert** (guide line 96), which the guide makes non-optional. Task 12 emits it as a `qa.run.queue_expired` lifecycle event plus a WARN log carrying every field legacy's `QueueEventContext` carries; 2.8 later subscribes. |

---

## File map

```
gears/qa-platform/qa-runs/
├── qa-runs-sdk/
│   ├── Cargo.toml
│   └── src/{lib,models,errors,client}.rs
└── qa-runs/
    ├── Cargo.toml
    └── src/
        ├── lib.rs / config.rs / gear.rs
        ├── domain/
        │   ├── mod.rs / error.rs / system_actor.rs
        │   ├── exclusivity.rs          ← TDD core 1 (pure)
        │   ├── queue.rs                ← TDD core 2 (pure)
        │   ├── state_machine.rs        ← TDD core 3 (pure)
        │   ├── cron.rs                 ← TDD core 4 (pure, Phase B)
        │   ├── env_assembly.rs         ← pure (Task 11)
        │   ├── params.rs               ← pure (Task 9)
        │   ├── naming.rs               ← pure (Task 9)
        │   ├── ports/{mod,run_executor,event_publisher}.rs
        │   ├── repos/{mod,runs_repo,queue_repo,schedules_repo}.rs
        │   ├── service/{mod,launch,admission,dispatch,runs,ingest,schedules}.rs
        │   └── local_client/{mod,client}.rs
        ├── infra/
        │   ├── mod.rs
        │   ├── executor/{mod,mock}.rs
        │   ├── events/{mod,payloads,publisher}.rs
        │   ├── logs/{mod,broadcast}.rs
        │   └── storage/  (entity/, mapper.rs, *_sea_repo.rs, migrations/)
        └── api/rest/  (dto.rs, error.rs, handlers/, routes/)

Modify:
  Cargo.toml                                                   (workspace members + deps)
  apps/cf-gears-example-server/Cargo.toml                      (qa-platform feature)
  apps/cf-gears-example-server/src/registered_gears.rs         (use qa_runs as _;)
  apps/cf-gears-example-server/config/qa-platform.yaml          (qa-runs config block)
  gears/qa-platform/docs/{PRD,DESIGN,DECOMPOSITION}.md         (Task 1 amendments)
  gears/qa-platform/qa-catalog/qa-catalog-sdk/src/{client,models}.rs   (Task 2)
  gears/qa-platform/qa-catalog/qa-catalog/src/domain/local_client/client.rs (Task 2)
  gears/qa-platform/qa-catalog/qa-catalog/src/domain/parsing/plan_yaml.rs   (Task 2)
```

**Task-ownership discipline.** Every task below names the files it owns. An implementer must not edit a file owned by a later task, even to fix a compile error — the expected remaining errors are listed per task. If a task cannot be completed without touching a file it does not own, **stop and report**; do not reach into it. (Three of the parity pass's most useful findings came from implementers refusing exactly this.)

---

## Verification gate

Run after every task:

```bash
cargo build -p qa-runs -p qa-runs-sdk
cargo clippy -p qa-runs -p qa-runs-sdk --all-targets -- -D warnings
cargo test -p qa-runs
cargo fmt --check -p qa-runs -p qa-runs-sdk
```

From Task 16 on, additionally:

```bash
cargo build -p cf-gears-example-server --features qa-platform
```

`cargo gears lint --dylint` **only if the CLI is installed** — it was absent for both shipped gears and for the parity pass (`error: no such command: gears`; neither `cargo-gears` nor `cargo-dylint` present, `xtask` has no lint subcommand). Check with `cargo gears --version`; do **not** install it. If absent, state plainly in the task report that the architecture lints — including DE0309 `#[domain_model]` coverage — are **unverified**, and never report them as passing.

**The code blocks in this plan do not pass this workspace's lint gate as written** (found during Task 5, 2026-08-13). Two known classes, both of which fail `cargo clippy --all-targets -- -D warnings`:

- **`"x".to_string()` is denied.** The root `Cargo.toml:231` sets `str_to_string = "deny"`. This plan contains **42 occurrences** across Tasks 5, 6, 8, and 10 — Task 5 alone produced 21 clippy errors. Write `.to_owned()`. This is mechanical; do it as you transcribe rather than reporting it as a finding.
- **Some blocks are not `rustfmt`-clean** (e.g. a `vec![…]` that must break across lines). Run `cargo fmt` rather than hand-matching the plan's layout.

Treat every code block here as *semantically* authoritative and *syntactically* provisional. Where the two conflict, the lint gate wins — but if a lint would require changing what the code **does**, stop and report instead of quietly altering behavior to satisfy it.

**On expected outputs:** every verification step below states an expected output. Where the expectation is a prediction rather than something observed, it is marked **(unverified prediction)** — lesson 25 from the parity plan, which asserted compiler behavior it had not tested and was wrong. In particular: **for schema work `cargo test` is the signal and `cargo build` proves nothing**, because SeaORM entities are hand-written structs whose table/column names are runtime strings.

## Commit discipline

One commit per task, conventional-commit subject, so each task is independently reviewable. At the end of Phase A **and** at the end of Phase B, squash that phase into a single commit on `feature/qa-platform-specs` (the established pattern — five deliverables, one squashed commit each; the parity work was 34 commits squashed on request). Never run two implementers concurrently, and never let the controller commit while a subagent may be running `git commit --amend`.

---

# Phase A — Run Orchestration Core (2.3)

---

### Task 1: Spec reconciliation and status housekeeping (docs only)

Do this **first**. Three of the four decisions above require a spec amendment (protocol step 4: when a spec claim turns out false, amend the spec *in the same change*, never work around it), and the status checkboxes currently contradict the code in both directions.

**Files:**
- Modify: `gears/qa-platform/docs/DESIGN.md`
- Modify: `gears/qa-platform/docs/PRD.md`
- Modify: `gears/qa-platform/docs/DECOMPOSITION.md`

**Owns:** these three files only. No Rust in this task.

- [ ] **Step 1: Amend `DESIGN.md` §3.7's `run_queue` description (D1).** Replace the `run_queue` entry in the qa-runs paragraph (`DESIGN.md:568`) and the example table (`:574-583`) so both state the seven-state vocabulary and the absence of a stored position. Add this paragraph immediately after the example table:

```markdown
**Amended 2026-08-13 (qa-runs plan, decision D1).** An earlier draft of this
section listed `run_queue.state` as `queued / dispatched / canceled` and a
`position BIGSERIAL` column. Both were sketches that were never reconciled
against the source system. The shipped vocabulary is the frozen one from
`../testrunner/docs/guides/exclusive-runs-and-the-queue.md` (lines 88-97):
`queued`, `dispatching`, `running`, `done`, `failed`, `cancelled`, `expired`.
Two of these carry the load: a row in `dispatching` or `running` is a *claim*
that holds the platform (`manager/src/services/run_queue.rs:104`), and
`expired` is the TTL terminal state that guarantees a queued run can never
disappear silently (guide line 96 — its alert is mandatory).

There is **no stored position**. FIFO order is `ORDER BY enqueued_at ASC,
id ASC` (`run_queue.rs:243-257`), and `queue_position` / `blocked_by` /
`ttl_expires_at` are computed per request over the rows that request
returned (`run_queue.rs:485-533`) — with the documented consequence that a
truncating `limit` understates them, so the platform-filtered listing is the
reliable one (guide lines 179-184).

Three admission limits also belong here and were absent from every spec
document. All three are operator settings with `0` meaning "disabled":
`max_concurrent_runs` (cluster-wide; 429 at admission, never a queued row —
`run_queue.rs:34-54, 879-891`), `queue_ttl_seconds` (per queued row; default
7200 — `models.rs:767-769`, arithmetic in `run_queue.rs:736-759`), and
`queue_max_depth` (per platform; default 20 — `models.rs:775-777`; 429 at
`run_queue.rs:830-832`). They are what make `cpt-cf-qa-fr-runs-launch`'s
"rejected with the limit that was hit" a closed set of exactly two causes.
```

Then extend the `run_queue` column list at `DESIGN.md:568` to: `run_queue` (platform_id, run_id, run_kind, target_ref, source, exclusive, state, execution_ref, error, enqueued_at, dispatched_at, finished_at).

- [ ] **Step 2: Amend `DESIGN.md` §3.1's run state machine (D2).** After the state-machine line (`DESIGN.md:240`) add:

```markdown
**Phase derivation is not the raw executor phase (amended 2026-08-13,
qa-runs plan decision D2).** A run's reported terminal phase is derived from
the executor's phase *plus* the ingested per-test results, and the derivation
downgrades: a run whose executor phase is `succeeded` is reported `failed`
if **any** test result is `failed`, `error`, **or `skipped`**
(`manager/src/services/argo.rs:2170-2201`). The skip arm is deliberate —
"a skipped test means the run didn't fully execute, so it must never read as
passing either" (`argo.rs:2180-2181`) — and it is ported as-is, including its
consequence: because the skip-list (`SKIP_TESTS_WITH_BUGS`,
`argo.rs:476-481`) makes the runner skip tests with open bugs, a run that
skipped only known-broken tests reports `failed`. The rule is idempotent by
construction: only `succeeded` is ever downgraded, so re-deriving a
persisted phase is a no-op.
```

- [ ] **Step 3: Amend PRD `cpt-cf-qa-fr-runs-params` (D3).** Append to the requirement text at `PRD.md:301`, after "at most 50 parameters are accepted":

```markdown
, each parameter name at most 128 characters and each value at most 8 KiB.
```

and add a bullet under it:

```markdown
- **Amended 2026-08-13 (qa-runs plan, decision D3)**: the two size caps were
  omitted from this requirement's first draft but are enforced by the source
  system (`manager/src/routes/settings.rs:32-34`, applied at `:118-151`), so
  they are part of the ported contract rather than a new constraint. The
  reserved-name list is unchanged and complete: it matches
  `RESERVED_PIPELINE_VARIABLE_NAMES` (`routes/settings.rs:15-27`) name for
  name. Note what that list deliberately does **not** cover: the runner's
  result-callback URL (`VHP_PROGRESS_URL`, `argo.rs:438-441`) and
  `E2E_VHP_BASE_URL` are not reserved in the source system, so a launch
  parameter can replace either. Carried forward as inherited parity; if it is
  ever closed, it is a deliberate divergence and belongs here as one.
```

- [ ] **Step 4: Append the D4 finding to `DECOMPOSITION.md` 2.2's "Reads do not serialize against snapshot rewrites" follow-up** (**`DECOMPOSITION.md:126`** — corrected 2026-08-14). Add:

```markdown
    **Verified at-parity 2026-08-13 (qa-runs plan, decision D4).** The source
    system has the same two-tier writer-only lock structure — `sync_locks`
    keyed `"{repo_id}::{branch}"` plus a per-repo lock keyed
    `"{repo_id}::*repo*"` (`manager/src/services/test_repos.rs:30, 543-558`),
    taken only by writers (`:405-406, 452-456`) — while its readers
    (`plans::find_plan_in_repository_root` and
    `test_bundles::create_bundle_from_checkout`, both called from
    `routes/runs.rs:662-722`) walk the checkout unlocked. So two concurrent
    launches on one `(repo, branch)` race there exactly as they do here, and
    neither generation-numbered snapshots nor a reader lock is a parity
    requirement — both would be new design. One honest difference in degree:
    the source system updates a branch directory via `git worktree`
    (`test_repos.rs:463-467`), rewriting in place, whereas `gix_sync.rs` clears
    before rewriting — so this gear additionally exposes a transient *empty*
    read where legacy exposes only a partial one. qa-runs closes just that
    widened part with one bounded retry when post-force-sync discovery comes
    back empty; the named fix for the residual (generation-numbered snapshot
    directories with an atomic pointer swap) stays recorded here, unbuilt.
```

- [ ] **Step 5: Fix the status markers that contradict the code.** Derive from code, not from checkboxes. Set to `[x]`:
  - `DECOMPOSITION.md:36` (`cpt-cf-qa-feature-environments`) and `:73` (`cpt-cf-qa-feature-catalog`)
  - `DECOMPOSITION.md:51-53` (env platforms / variables / lease — already `[x]` in PRD, inconsistent here)
  - `DECOMPOSITION.md:89-96` (all eight catalog FRs — shipped and reviewed)
  - `DECOMPOSITION.md:65` (`cpt-cf-qa-component-environments`) and `:100` (`cpt-cf-qa-component-catalog`)
  - `PRD.md` §5.1's eight catalog FR checkboxes (shipped)

  Leave `cpt-cf-qa-fr-env-version-poll` `[ ]` — the poller genuinely does not exist (the gear has no `stateful` capability and nothing writes `observed_version`). Add a one-line note under `DECOMPOSITION.md:44` recording that: `**Not built**: the version poller and the `qa.platform.version_changed` schema were claimed by this feature's scope but never implemented — the gear declares no `stateful` capability and `observed_version` has no writer. Retrofit when 2.7 lands.`

- [ ] **Step 6: Record the event-vocabulary owner** (open decision #1 from the continuation prompt: `cpt-cf-qa-interface-events` is p1 with zero implementation and no owner; DECOMPOSITION:222 makes it a prerequisite for 2.5, but 2.1 claimed a slice of it and did not build it). Add to `DECOMPOSITION.md` 2.3's **Scope** list (`:145-149`):

```markdown
  - **Owns the subsystem's event vocabulary.** `cpt-cf-qa-interface-events` is
    p1 with no owner: 2.1 claimed the `qa.platform.version_changed` schema in
    its scope and did not build it, and this feature's "event publication via
    event-broker SDK" line assumed the vocabulary already existed. qa-runs
    defines the whole vocabulary (it is the dominant publisher — every event in
    DESIGN §3.3's table but one), and `qa.platform.version_changed` is
    retrofitted into it when the poller is built.
```

- [ ] **Step 7: Verify.** No build to run. Confirm the diff touches only the three doc files:

```bash
git diff --stat
```

Expected: exactly `docs/DESIGN.md`, `docs/PRD.md`, `docs/DECOMPOSITION.md`, no other paths.

- [ ] **Step 8: Commit.**

```bash
git add gears/qa-platform/docs/PRD.md gears/qa-platform/docs/DESIGN.md gears/qa-platform/docs/DECOMPOSITION.md
git commit -m "docs(qa-platform): reconcile queue model, phase derivation, and param caps against legacy

Amends three spec claims the qa-runs legacy-verification pass found false or
incomplete (decisions D1-D4), records the D4 at-parity finding on the catalog
snapshot race, assigns the event vocabulary to 2.3, and corrects the status
markers for the two shipped gears."
```

---

### Task 2: Widen the qa-catalog SDK for the launch path

Two tracked qa-catalog follow-ups are qa-runs' problem (DECOMPOSITION 2.2), and the launch path cannot be written without the first: `QaCatalogClientV1::sync_repo(ctx, id)` takes no branch and no force flag, but parity spec §3.4 step 4 requires force-syncing a *named* branch before `create_bundle`. The gear's service and REST layers already support both (`?branch=`, always force) — only the trait was left alone because changing it with no consumer would have been speculative. qa-runs is the consumer.

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog-sdk/src/client.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog-sdk/src/models.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/local_client/client.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/parsing/plan_yaml.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans.rs` (only if `validation` needs threading; see Step 4)

**Owns:** the files above. Does **not** own anything under `gears/qa-platform/qa-runs/` (does not exist yet).

**Expected remaining errors after this task:** none. This task must leave `cargo build -p qa-catalog -p qa-catalog-sdk` green — it is a widening of a trait plus one parser field, with the existing REST callers updated.

- [ ] **Step 1: Legacy check — force-sync semantics.** Read `../testrunner/manager/src/services/test_repos.rs:487-511` (`force_sync`) and `routes/runs.rs:647-656` (its launch-path call site). Confirm all three: (i) force-sync takes an *optional* branch and falls back to the repository's `default_branch` when `None` (`test_repos.rs:498-502`); (ii) "force" means dropping the freshness recency marker so the TTL guard cannot return the cached checkout (`:503-508`); (iii) the launch path always force-syncs — it never consults freshness (`routes/runs.rs:649`). Cite `test_repos.rs:498-508` in the doc comment written in Step 2. **If legacy differs, stop and report per the protocol.**

- [ ] **Step 2: Add a params struct and widen the trait.** A params struct rather than two more positional arguments, matching the trait's dominant idiom (`create_bundle(ctx, req: BundleRequest)`) and naming the otherwise-opaque bare `true` at the call site.

**Corrected 2026-08-13 during execution.** This step originally justified the struct by claiming `Option<String>` and `bool` are "silently transposable at a call site — the same footgun DECOMPOSITION 2.2 records against `update_product`". That is **false**, and the code review caught it: rustc rejects the transposition outright (`error[E0308]: arguments to this function are incorrect … help: swap these arguments`). The `update_product` precedent is real but does not transfer — three adjacent `String`s are mutually substitutable; a `(Option<String>, bool)` pair is not. The struct is still the right call for the reasons above; only the reasoning was wrong. The doc comment below has been rewritten accordingly, because this subsystem treats doc rationale as load-bearing and a wrong justification on a public SDK surface will be cited to justify a params struct somewhere it genuinely does not apply.

In `qa-catalog-sdk/src/models.rs`, append:

```rust
/// What to sync, and how hard.
///
/// A struct rather than two positional arguments on
/// [`crate::client::QaCatalogClientV1::sync_repo`]: it matches this trait's
/// dominant idiom (`create_bundle(ctx, req: BundleRequest)`), it names the
/// bare `true` that `sync_repo(ctx, id, branch, true)` leaves opaque at a
/// call site, and a third sync knob can be added without a breaking
/// signature change. DECOMPOSITION 2.2 sanctions either shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncRequest {
    /// Branch to materialize. `None` uses the repository's `default_branch`,
    /// matching the source system (`manager/src/services/test_repos.rs:498-502`).
    pub branch: Option<String>,
    /// Bypass the freshness TTL. The launch path always sets this: the source
    /// system's launch drops the recency marker before syncing so a cached
    /// checkout cannot be returned (`test_repos.rs:503-508`, called from
    /// `routes/runs.rs:649`).
    pub force: bool,
}
```

Re-export it from `qa-catalog-sdk/src/lib.rs` alongside the other models. In `qa-catalog-sdk/src/client.rs`, replace the `sync_repo` declaration:

```rust
    /// Trigger a sync now; returns when the sync completes or fails.
    ///
    /// `req.branch` selects which branch's content snapshot is materialized
    /// (`None` = the repository's `default_branch`); `req.force` bypasses the
    /// in-memory freshness TTL. qa-runs' launch path always passes an
    /// explicit branch with `force: true` (parity spec §3.4 step 4).
    async fn sync_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: SyncRequest,
    ) -> Result<TestRepository, QaCatalogError>;
```

and add `SyncRequest` to the `use crate::models::{…}` list.

- [ ] **Step 3: Update the local client delegation.** In `qa-catalog/src/domain/local_client/client.rs`, change the `sync_repo` impl to forward `req.branch.as_deref()` and `req.force` to whatever the service's existing signature is. **Do not guess that signature** — open `qa-catalog/src/domain/service/repos.rs`, find the sync entry point the REST handler calls, and match it exactly.

**Resolved on execution 2026-08-13: the conditional below did not apply.** The service is already `sync_repo(ctx, id, branch: &str, force: bool)` (`repos.rs:336`) and honors `force` for real (`:374-378`, re-checked under the lock at `:403-405`), so both fields forward and **the gap comment was correctly not added**. It spells "no branch named" as `""`, so the delegation is `req.branch.as_deref().unwrap_or("")`. One behavioral consequence to carry forward: the previous SDK impl hardcoded `force = true`, so `sync_repo` no longer unconditionally forces — a `SyncRequest::default()` may now be served from a fresh snapshot. There were no other SDK callers at the time, and qa-runs' launch path must therefore pass `force: true` **explicitly** (Task 13). The hypothetical below is retained only as a record of what was checked.

If the service takes only a branch and always forces (which is what DECOMPOSITION 2.2 says the REST layer does), then `req.force == false` has no service-level expression: in that case pass the branch through and add this comment at the delegation, so the gap is on record rather than silently swallowed:

```rust
// `req.force` is not yet expressible: the service's sync always force-syncs
// (DECOMPOSITION 2.2 — "REST sync takes an optional `?branch=` and always
// force-syncs"). Accepting the flag now keeps the SDK contract honest for
// qa-runs' launch path and for a future freshness-respecting caller; a
// `force: false` request currently still force-syncs, which is the safe
// direction (a redundant fetch, never a stale read).
```

- [ ] **Step 4: Add `validation` to the `plan.yaml` parser** (this plan's own flagged decision — drop this step *and* Task 5's `is_validation` field together if the reviewer defers it). **Legacy check:** read `../testrunner/manager/src/services/plans.rs` and confirm the exact rule — `validation` is a plain bool OR'd with a case-insensitive `validation` **tag** (DECOMPOSITION:131 records this; verify it in the source and note the `file:line`). Then in `qa-catalog/src/domain/parsing/plan_yaml.rs` add `validation: bool` to the raw deserialization struct with `#[serde(default)]`, and to `ParsedPlan`, computing it as `raw.validation || raw.tags.iter().any(|t| t.trim().eq_ignore_ascii_case("validation"))`. **Corrected 2026-08-13 during execution:** this formula originally omitted the `.trim()`. Legacy is `tag.trim().eq_ignore_ascii_case("validation")` (`plans.rs:35-39`, verified), and the difference is observable for a quoted YAML scalar such as `tags: ['  validation  ']`, where the YAML parser does not strip the padding. Hoist it to a `let validation = …` before the struct literal, as legacy does at `plans.rs:35` — building it inline moves `raw.tags` before the flag can read it. Add the field to `sdk::Plan` and to the mapper that builds it. **Deliberately not added to REST `PlanDto`** — DECOMPOSITION 2.2 already records `PlanDto` omitting `product_id` on the same "no REST consumer needs it yet" grounds, and the only consumer here is qa-runs over the SDK.

- [ ] **Step 5: Write the failing test first, for Step 4.** In `plan_yaml.rs`'s existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn validation_flag_is_read_from_the_bool() {
        let p = parse_plan_yaml("name: v\nvalidation: true\ntests: [a.py]\n").unwrap();
        assert!(p.validation);
    }

    #[test]
    fn validation_flag_is_read_from_a_case_insensitive_tag() {
        // Legacy ORs the bool with a `validation` tag, case-insensitively
        // (manager/src/services/plans.rs — cite the verified line here).
        let p = parse_plan_yaml("name: v\ntags: [E2E, Validation]\ntests: [a.py]\n").unwrap();
        assert!(p.validation, "a `validation` tag must arm the flag regardless of case");
    }

    #[test]
    fn absent_validation_is_false() {
        let p = parse_plan_yaml("name: v\ntests: [a.py]\n").unwrap();
        assert!(!p.validation);
    }
```

- [ ] **Step 6: Run the new tests before implementing.**

```bash
cargo test -p qa-catalog validation
```

Expected: compile failure — `no field `validation` on type `ParsedPlan``. **(unverified prediction** — it may instead fail as three assertion failures if `ParsedPlan` derives `Default` and the field is added before the test is run; either way the tests must be red before Step 4's implementation lands.**)**

- [ ] **Step 7: Verify the whole catalog gear.**

```bash
cargo build -p qa-catalog -p qa-catalog-sdk
cargo test -p qa-catalog
cargo clippy -p qa-catalog -p qa-catalog-sdk --all-targets -- -D warnings
```

Expected: build green. **Observed on execution 2026-08-13:** the pre-change count is **148** lib tests from a plain `cargo test -p qa-catalog` (the `gix_sync_integration` and `multi_branch` suites need `--features integration` and `git` on PATH, and report 0 without it), and the post-change count is **152** — 148 + 4, the fourth test being the `.trim()` case this step's formula originally missed. The prediction this line previously carried ("151 pre-change, 154 total") was wrong on both halves and self-inconsistent; it is replaced here with the measured values.

- [ ] **Step 8: Commit.**

```bash
git add gears/qa-platform/qa-catalog
git commit -m "feat(qa-catalog): branch+force sync params and plan.yaml validation flag

Widens QaCatalogClientV1::sync_repo with a SyncRequest params struct so
qa-runs' launch path can force-sync a named branch before create_bundle
(parity spec §3.4 step 4, DECOMPOSITION 2.2 follow-up), and parses
plan.yaml's `validation` flag — the one unported plan.yaml field with a live
legacy consumer — so qa-runs can re-home it as a run classification."
```

---

### Task 3: Scaffold the qa-runs crate pair

**Files:**
- Create: `gears/qa-platform/qa-runs/qa-runs-sdk/Cargo.toml`, `qa-runs-sdk/src/lib.rs`
- Create: `gears/qa-platform/qa-runs/qa-runs/Cargo.toml`, `qa-runs/src/lib.rs`
- Modify: root `Cargo.toml` (workspace members, and `[workspace.dependencies]` if `cron` is absent)

**Owns:** the four created files and the root `Cargo.toml`.

- [ ] **Step 1: Copy the qa-catalog pair as the literal starting point.** `gears/qa-platform/qa-catalog/{qa-catalog-sdk,qa-catalog}/Cargo.toml` compile today and carry the correct workspace-dependency spellings. Copy them, rename `qa-catalog` → `qa-runs` throughout, then adjust the gear crate's dependency list to:

```toml
# Cross-gear SDKs this gear consumes (ClientHub).
# Intra-subsystem siblings go by relative path, matching qa-catalog's own
# `qa-catalog-sdk = { path = "../qa-catalog-sdk" }` — neither has a
# [workspace.dependencies] entry, so `{ workspace = true }` would not resolve.
qa-catalog-sdk = { path = "../../qa-catalog/qa-catalog-sdk" }
qa-environments-sdk = { path = "../../qa-environments/qa-environments-sdk" }
event-broker-sdk = { workspace = true }   # requires a new entry — see Step 2
cluster-sdk = { workspace = true }
# Cron evaluation (Phase B).
cron = { workspace = true }               # requires a new entry — see Step 2
```

**Corrected 2026-08-13 before dispatch.** This block originally spelled all five `{ workspace = true }`. Verified against the root `Cargo.toml`: only `cluster-sdk` resolves that way today (`Cargo.toml:309`, aliasing `cf-gears-cluster-sdk`).

and **remove** from the copy the dependencies qa-catalog needs and qa-runs does not: `gix`, `flate2`, `tar`, `regex`, `credstore-sdk`. qa-runs never touches git, never builds an archive, never parses test content (qa-catalog does all of that behind its SDK), and never resolves a secret — it passes the kubeconfig *reference* through to the executor (DESIGN §3.5: "the executor adapter passes the reference to the execution plane").

- [ ] **Step 2: Add missing workspace dependencies.** Check each of the five above against `[workspace.dependencies]` in the root `Cargo.toml`:

```bash
grep -n 'event-broker-sdk\|cluster-sdk\|^cron\|qa-catalog-sdk\|qa-environments-sdk' Cargo.toml
```

**Measured 2026-08-13, before dispatch — do not re-derive, but do re-confirm:**

| Dependency | State in the root `Cargo.toml` | Action |
|---|---|---|
| `cluster-sdk` | Entry present at `:309`, aliasing `cf-gears-cluster-sdk` | Use `{ workspace = true }` as-is |
| `qa-catalog-sdk` | Member at `:63`, **no** `[workspace.dependencies]` entry | Relative path (Step 1) |
| `qa-environments-sdk` | Member at `:61`, **no** entry | Relative path (Step 1) |
| `event-broker-sdk` | Member at `:120`, **no** entry, and **no gear depends on it yet** (the only reference is a `TODO(broker)` in `gears/bss/ledger/ledger/Cargo.toml:64`) | Add an entry, aliased like `cluster-sdk` — the crate is `cf-gears-event-broker-sdk`, so a bare `event-broker-sdk = { … }` needs `package = "cf-gears-event-broker-sdk"` |
| `cron` | Absent from both `Cargo.toml` and `deny.toml` | Add per the process below |

Being the first consumer of `event-broker-sdk` is worth pausing on: nothing in the workspace exercises it, so its API is unproven here. If its surface does not match what Task 12 assumes, **stop and report** rather than bending the event design around it.

`cron` add it by checking its license against `deny.toml`'s allow-list, then running `cargo deny check`. **Corrected 2026-08-13:** this step cited `guidelines/DEPENDENCIES.md` as the process; that file is 52 lines about YAML and lock choices and documents no dependency-addition process at all — nothing in `guidelines/`, `CLAUDE.md`, or `.claude/` does. The check itself is still right, only the citation was wrong. Note `cargo-deny` may not be installed. If the license check fails, **stop and report** — a cron parser is replaceable (`saffron`, or a hand-rolled five-field evaluator), and the choice is a dependency-policy decision, not an implementation one.

- [ ] **Step 3: Add the workspace members.** In root `Cargo.toml`, next to the existing qa-platform entries:

```toml
    "gears/qa-platform/qa-runs/qa-runs-sdk",
    "gears/qa-platform/qa-runs/qa-runs",
```

- [ ] **Step 4: Minimal `lib.rs` for each.** SDK:

```rust
//! qa-runs SDK: transport-agnostic contract for the run orchestrator.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-runs`). Contract purity: this crate must stay
//! free of `serde`, `utoipa`, and `http` — enforced by review and the
//! per-task grep, NOT by a lint: `de0101_no_serde_in_contract` and
//! `de0102_no_toschema_in_contract` are in `Gears.toml`'s dylint skip list.

mod client;
mod errors;
mod models;

pub use client::QaRunsClientV1;
pub use errors::QaRunsError;
```

(`models`/`errors`/`client` land in Task 4; for this task create them as empty files with a `//!` doc line each so the `pub mod` declarations resolve.) Gear:

```rust
//! qa-runs gear: launch validation, exclusivity resolution, per-platform FIFO
//! queue, dispatcher with crash recovery, run state machine, environment
//! assembly, cancellation/re-run, timeout enforcement, SSE logs, lifecycle
//! events, and cron schedules.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-runs`).

pub mod api;
pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;

pub use gear::QaRuns;

#[cfg(test)]
mod test_support;
```

Since `api`/`config`/`domain`/`gear`/`infra` do not exist yet, **comment out all five `pub mod` lines, the `pub use`, and `#[cfg(test)] mod test_support;`** in this task with a `// Task N:` marker naming the task that uncomments each.

**Corrected 2026-08-13 during execution.** This step originally assigned `config` → Task 15, `infra` → Task 8, `api` → Task 15, and omitted `test_support` from the list entirely. Three of those five were wrong, checked against the tasks' own **Files** blocks: Task 8 creates only `domain/{params,naming,env_assembly}.rs` and never touches `lib.rs`; Task 15 creates only `domain/service/{ingest,runs}.rs` and `infra/logs/`. And `test_support.rs` does not exist either, so leaving it uncommented breaks `cargo test`. The correct owners, as shipped:

| Item | Uncommented by |
|---|---|
| `domain` | Task 5 |
| `infra` | Task 9 |
| `api`, `config`, `gear`, `pub use`, `test_support` | Task 16 |

No stale `// Task N:` marker may survive the final review, so each must be removed by its named task (Task 16's Files block already says "remove every `// Task N:` marker").

- [ ] **Step 5: Verify.**

```bash
cargo build -p qa-runs-sdk -p qa-runs
```

Expected: `Compiling qa-runs-sdk`, `Compiling qa-runs`, `Finished` — two library crates with no items. If `cargo deny check` was run in Step 2, it must also pass.

- [ ] **Step 6: Commit.** `git commit -m "feat(qa-runs): scaffold sdk and gear crates"`

---

### Task 4: SDK models, errors, client trait

**Files:**
- Create: `qa-runs-sdk/src/{models,errors,client}.rs` (replacing the Task 3 stubs)
- Modify: `qa-runs-sdk/src/lib.rs` (re-exports)

**Owns:** `qa-runs-sdk/src/**` only.

The client trait is the contract `qa-insights` will call for auto-rerun (DESIGN §3.4: "the only back-edge, and it goes through the public SDK contract") and the surface the schedule firing task uses. Keep it minimal and launch-shaped.

- [ ] **Step 1: Legacy check — the run record's field inventory.** `parse_workflow` (`../testrunner/manager/src/services/argo.rs:2236-2319`) is the exhaustive list of what the source system records per run, because the Workflow object *is* its run record. Read it and confirm every field below has a legacy origin, and that nothing load-bearing is missing. In particular confirm: `exclusive` is read through the tri-state annotation codec (`:2315-2317` → `exclusivity.rs:52-58`), `is_validation` is `annotations["vhp-tests/validation"] == "true"` (`:2289-2292`), `run_source` normalizes to `manual`/`scheduled` (`:2293-2296`), and `parameters` is deliberately **empty** from the workflow because the persisted row supplies it (`:2312-2314`). Cite `argo.rs:2236-2319` in `models.rs`'s module doc. **If a field in the list below has no legacy counterpart, stop and report before inventing it.**

- [ ] **Step 2: `models.rs`.** No `serde`, no `utoipa`, no `http`.

```rust
//! Transport-agnostic models for the qa-runs contract.
//!
//! Field inventory derived from the source system's run record, which is the
//! Argo Workflow object itself — see `parse_workflow`
//! (`manager/src/services/argo.rs:2236-2319`) for the authoritative list of
//! what a run carries. This gear persists the same facts in a `qa_runs` row
//! (`cpt-cf-qa-principle-db-first-state`). **Corrected 2026-08-13 (Task 4):**
//! this said "there is no legacy table to port, only a legacy field set".
//! There is one — `run_results`, read as `PersistedRunRow`
//! (`manager/src/services/run_history.rs:10-48`) and merged with the live
//! Workflow by `overlay_persisted_with_live` (`:216`), because the Workflow is
//! GC'd after its TTL while re-runs happen much later
//! (`migrations/001_initial.sql:306-310`). The run record is therefore two
//! halves and the field inventory must be taken from both.

use time::OffsetDateTime;
use uuid::Uuid;

/// What a launch targets. Three kinds, not the source system's four: its
/// `RunIntent::GitPlan` (`manager/src/services/run_dispatcher.rs:195-207`)
/// reads plans through a second, overlapping plan reader
/// (`manager/src/services/git_plans.rs`) that DECOMPOSITION:153 dispositions
/// as unported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunTarget {
    /// A plan discovered from a repository's `plan.yaml`.
    Plan {
        repo_id: Uuid,
        /// Path of the plan.yaml within the repository's content root.
        path: String,
    },
    /// A single test file within a discovered plan. A single-test run
    /// considers only that one file, and no tag filter applies to it
    /// (`manager/src/services/exclusivity.rs:214-223`).
    Test {
        repo_id: Uuid,
        path: String,
        test_file: String,
    },
    /// A persisted user-composed plan; its files may span repositories.
    CustomPlan { id: Uuid },
}

impl RunTarget {
    /// Stable discriminant recorded on the run and the queue row. Mirrors the
    /// source system's `vhp-tests/run-kind` annotation vocabulary
    /// (`manager/src/services/argo.rs:2245-2256`).
    #[must_use]
    pub fn kind(&self) -> RunKind {
        match self {
            Self::Plan { .. } => RunKind::Plan,
            Self::Test { .. } => RunKind::Test,
            Self::CustomPlan { .. } => RunKind::CustomPlan,
        }
    }
}

/// The `run_kind` discriminant, as persisted and reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Plan,
    Test,
    CustomPlan,
}

impl RunKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Test => "test",
            Self::CustomPlan => "custom_plan",
        }
    }
}

/// Who asked. Normalized to exactly these two, as the source system does
/// (`manager/src/services/argo.rs:2293-2296`,
/// `manager/src/routes/runs.rs:595-603`: anything that is not `scheduled` is
/// `manual`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunSource {
    Manual,
    Scheduled,
}

impl RunSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }
}

/// One `name=value` launch parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunParameter {
    pub name: String,
    pub value: String,
}

/// A launch request. One shape for every caller — manual, CI, scheduled, and
/// auto-rerun all go through it, which is the invariant
/// `cpt-cf-qa-fr-runs-schedules` states as a requirement.
#[derive(Clone, Debug, PartialEq)]
pub struct LaunchRequest {
    pub target: RunTarget,
    /// `None` = a run with no target platform. Such a run is never queued and
    /// never blocks anything (guide line 76).
    pub platform_id: Option<Uuid>,
    /// Test-content branch. Resolution order when absent: platform
    /// `default_branch`, then repository `default_branch` (parity spec §3.4
    /// step 1; `manager/src/services/exclusivity.rs:297-312` plus
    /// `manager/src/routes/runs.rs:639-644`).
    pub branch: Option<String>,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub parameters: Vec<RunParameter>,
    /// The launch exclusivity tier: three-state. `None` means "inherit",
    /// which is **not** `Some(false)` — that distinction is the whole reason
    /// the upper tiers are `Option<bool>` (guide lines 35-45).
    pub exclusive: Option<bool>,
    /// Per-run timeout override, seconds. `None` falls back to the plan's
    /// `timeout_seconds`, then the configured default.
    pub timeout_seconds: Option<u64>,
    pub source: RunSource,
    /// Set when a schedule produced this launch.
    pub schedule_id: Option<Uuid>,
}

/// A run's lifecycle state. Terminal states are the last six.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Created,
    Queued,
    Dispatching,
    Running,
    Succeeded,
    Failed,
    Canceled,
    TimedOut,
    /// The run's queue row hit `queue_ttl_seconds` and was swept before it ever
    /// started. **Added 2026-08-13 by user decision**, raised by Task 7: the
    /// transition table gave `Queued` exactly one terminal exit, `Canceled`,
    /// and nothing said what a TTL-expired run becomes. The source system never
    /// faced the question — it expires the *queue row* to `'expired'`
    /// (`manager/src/services/run_queue.rs:370-378`) and has no run row for a
    /// launch that never started. Reusing `Canceled` would make the TTL sweep
    /// indistinguishable from an operator cancel; reusing `TimedOut` would
    /// conflate a queue-wait clock with `cpt-cf-qa-fr-runs-timeout`'s
    /// execution deadline. The guide already makes the `expired` alert
    /// mandatory (guide line 96), so an operator sees that word — the run must
    /// not say something else.
    Expired,
    Error,
}

/// The authoritative run record.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub id: Uuid,
    /// Human-facing run name, `{slug}-{n}`. Load-bearing beyond display: the
    /// queue's `blocked_by` text names the run holding a platform (guide line
    /// 177), so runs need a stable short name, not just a UUID.
    pub name: String,
    pub target: RunTarget,
    pub platform_id: Option<Uuid>,
    /// Branch actually resolved and executed against; also recorded as the
    /// run's test version (`manager/src/routes/custom_plans.rs:960`,
    /// `routes/runs.rs:645`).
    pub test_version: Option<String>,
    /// Platform application version, **snapshotted at launch** — legacy sets
    /// `app_version = platform_version` (`manager/src/routes/runs.rs:594`) and
    /// persists it on `run_results` rather than re-deriving it, because the
    /// Workflow is GC'd while re-runs happen much later
    /// (`migrations/001_initial.sql:306-310`). Re-deriving from `platform_id`
    /// would let a platform upgrade silently change a queued run's or a
    /// re-run's `APP_VERSION`, breaking both reproducibility and PRD:577's
    /// "environment variable names consumed by tests are unchanged" contract.
    /// Feeds the `APP_VERSION` static runner variable in Task 8c's tier 1.
    /// **Added 2026-08-13 by user decision during Task 4** — the original
    /// model set omitted both fields, found by Task 4's backward inventory.
    pub app_version: Option<String>,
    /// Platform build identifier, snapshotted at launch alongside
    /// [`Self::app_version`]. Feeds `APP_BUILD`.
    pub app_build: Option<String>,
    pub state: RunState,
    /// The exclusivity decision actually made, plus which tier made it. Both
    /// are recorded because the source system **logs** the tier so an operator
    /// can always tell *why* a run became exclusive (`run_dispatcher.rs:92-99`,
    /// its only consumer). **Corrected 2026-08-13 (Task 4):** this previously
    /// said "logs *and annotates*" — legacy annotates only the boolean
    /// (`argo.rs:612, 918, 1342`), never the tier. Tier vocabulary itself:
    /// `manager/src/services/exclusivity.rs:13-14`.
    ///
    /// **Named `resolved_exclusive`, not `exclusive` — corrected 2026-08-13
    /// (Task 4 review).** This is the *resolved* decision, while
    /// [`LaunchRequest::exclusive`] is an `Option<bool>` tri-state request.
    /// Under the old name the obvious re-run transcription
    /// `exclusive: Some(run.exclusive)` type-checked and was **wrong**: it
    /// pins a stored `false` onto the launch tier, which outranks everything
    /// and suppresses a `TEST_META`/`plan.yaml` declaration added since — i.e.
    /// relaunches a since-marked-destructive test in parallel on a shared
    /// platform. DESIGN.md:582 already names the column `resolved_exclusive`;
    /// the SDK field was the outlier. `QueueEntry.exclusive` keeps the short
    /// name — DESIGN's `run_queue` column list spells it that way.
    pub resolved_exclusive: bool,
    pub exclusive_tier: ExclusiveTier,
    /// Whether this run is a validation run — `plan.yaml`'s `validation` bool
    /// OR'd with a case-insensitive `validation` tag. Stamped as an annotation
    /// by the source system (`manager/src/services/argo.rs:2289-2292`).
    pub is_validation: bool,
    pub parameters: Vec<RunParameter>,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub source: RunSource,
    pub schedule_id: Option<Uuid>,
    /// Bundle descriptors backing this run, one per repository group (parity
    /// spec §3.4 step 5: one execution node per group).
    pub bundle_ids: Vec<Uuid>,
    /// Opaque handle from the `RunExecutor`; `None` until dispatch succeeds.
    pub execution_ref: Option<String>,
    /// Archived-log pointer, populated on completion (p2 with 2.7).
    pub log_storage_ref: Option<String>,
    /// Deadline the control plane enforces (`cpt-cf-qa-fr-runs-timeout`).
    pub timeout_at: Option<OffsetDateTime>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    /// Terminal failure reason, operator-facing.
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Which tier supplied a run's exclusivity flag. Ported verbatim from
/// `manager/src/services/exclusivity.rs:15-37` — including the distinction
/// between `TestMeta` with a `false` answer ("files were read and all said
/// parallel") and `Default` ("nobody had an opinion"), which the source
/// system has an explicit test for
/// (`exclusivity.rs:656-664`, assertion at :658: the log must not read `default`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExclusiveTier {
    Launch,
    Plan,
    TestMeta,
    Default,
}

impl ExclusiveTier {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Plan => "plan.yaml",
            Self::TestMeta => "test_meta",
            Self::Default => "default",
        }
    }
}

/// Per-run outcome counts, updated incrementally as results arrive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunResult {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub in_progress: usize,
    pub total: usize,
}

/// The three-outcome launch contract (`cpt-cf-qa-fr-runs-launch`; guide lines
/// 68-76). Load-bearing for both UI and CI callers, so it is an enum rather
/// than a `Run` with a nullable queue id.
#[derive(Clone, Debug, PartialEq)]
pub enum LaunchOutcome {
    /// Started immediately → HTTP 200.
    Started { run: Run },
    /// Queued; it will start on its own, no further call needed → HTTP 202.
    Queued { run_id: Uuid, queue_id: Uuid },
}

/// A queue row as reported by the read endpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueEntry {
    pub id: Uuid,
    pub run_id: Uuid,
    pub platform_id: Uuid,
    pub run_kind: RunKind,
    pub source: RunSource,
    pub exclusive: bool,
    pub state: QueueState,
    pub error: Option<String>,
    pub enqueued_at: OffsetDateTime,
    pub dispatched_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    /// 1-based position among this platform's `queued` rows, oldest first;
    /// `None` for any other state. Computed per request over the rows that
    /// request returned, so a truncating limit understates it — use the
    /// platform-filtered call (guide lines 171-184).
    pub queue_position: Option<i64>,
    /// When the TTL sweep will expire this row. `None` unless `queued`, and
    /// `None` when `queue_ttl_seconds` is 0.
    pub ttl_expires_at: Option<OffsetDateTime>,
    /// Plain-text reason it has not started (guide line 177).
    pub blocked_by: Option<String>,
}

/// The seven queue states (guide lines 88-96; decision D1). `Dispatching`
/// and `Running` are the two that hold a claim on the platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueState {
    Queued,
    Dispatching,
    Running,
    Done,
    Failed,
    Cancelled,
    Expired,
}

/// A cron schedule (Phase B).
#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    pub id: Uuid,
    pub name: String,
    pub target: RunTarget,
    pub platform_id: Option<Uuid>,
    pub branch: Option<String>,
    /// Five-field cron expression.
    pub cron: String,
    /// The stored exclusivity choice, delivered into the launch as the
    /// **launch** tier when the schedule fires (guide lines 62-65). Serialized
    /// `true` / `false` / `auto` — always written, including for `None`,
    /// because an absent value must not mean both "inherit" and "parallel"
    /// (`manager/src/services/exclusivity.rs:67-79`).
    pub exclusive_choice: Option<bool>,
    pub enabled: bool,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub parameters: Vec<RunParameter>,
    /// Latest due time this schedule has fired for.
    pub last_fired_tick: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Creation/replace payload for a schedule.
#[derive(Clone, Debug, PartialEq)]
pub struct NewSchedule {
    pub name: String,
    pub target: RunTarget,
    pub platform_id: Option<Uuid>,
    pub branch: Option<String>,
    pub cron: String,
    pub exclusive_choice: Option<bool>,
    pub enabled: bool,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub parameters: Vec<RunParameter>,
}
```

- [ ] **Step 3: `errors.rs`.** Same canonical re-export as both sibling SDKs — open `qa-catalog-sdk/src/errors.rs` and mirror it exactly (`pub use toolkit_canonical_errors::CanonicalError as QaRunsError;` plus whatever doc comment shape it uses).

- [ ] **Step 4: `client.rs`.**

```rust
//! Object-safe client trait for inter-gear consumption via `ClientHub`.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::QaRunsError;
use crate::models::{
    LaunchOutcome, LaunchRequest, NewSchedule, QueueEntry, Run, RunResult, Schedule,
};

/// Object-safe client for the qa-runs gear (Version 1).
///
/// Registered in `ClientHub`:
/// ```ignore
/// let runs = hub.get::<dyn QaRunsClientV1>()?;
/// ```
///
/// Primary consumer: qa-insights' auto-rerun (DESIGN §3.4 — the one back-edge
/// in the subsystem, taken deliberately through this public contract).
#[async_trait]
pub trait QaRunsClientV1: Send + Sync {
    /// The single run-creation path. Manual, CI, scheduled, and auto-rerun
    /// launches all enter here, which is what keeps scheduled and manual runs
    /// indistinguishable downstream (`cpt-cf-qa-fr-runs-schedules`).
    ///
    /// Returns `LaunchOutcome::Started` or `::Queued`. The third outcome —
    /// rejected by a limit — is an `Err` carrying the limit that was hit
    /// (`resource_exhausted`), because there is no run to return.
    async fn launch(
        &self,
        ctx: &SecurityContext,
        req: LaunchRequest,
    ) -> Result<LaunchOutcome, QaRunsError>;

    async fn get_run(&self, ctx: &SecurityContext, id: Uuid) -> Result<Run, QaRunsError>;

    /// Runs, newest first. `limit` bounds the result — **added 2026-08-13
    /// (Task 4 review)** for symmetry with [`list_queue`](Self::list_queue):
    /// `qa_runs` grows strictly faster than the queue and never drains, so an
    /// unbounded inter-gear call would materialize every run ever executed.
    /// REST-layer paging and OData are separate and unaffected (Task 16).
    async fn list_runs(
        &self,
        ctx: &SecurityContext,
        limit: u32,
    ) -> Result<Vec<Run>, QaRunsError>;

    async fn get_run_result(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<RunResult, QaRunsError>;

    /// Cancel a queued or executing run. Idempotent on an already-terminal run.
    async fn cancel_run(&self, ctx: &SecurityContext, id: Uuid) -> Result<Run, QaRunsError>;

    /// Re-run a completed run with its original parameters, re-validated at
    /// re-run time (`cpt-cf-qa-fr-runs-cancel-rerun`).
    ///
    /// Exclusivity is inherited **upward only**: a run recorded exclusive
    /// re-runs exclusive, while a run recorded parallel is re-resolved from
    /// the current tiers, so a test marked destructive since the original run
    /// correctly becomes exclusive (guide lines 205-208;
    /// `manager/src/routes/runs.rs:958-967`).
    async fn rerun(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<LaunchOutcome, QaRunsError>;

    // ==================== Queue ====================

    /// Queue rows, newest first, all states. `platform_id` filters to one
    /// platform — the reliable call, since positions and blockers are computed
    /// over the returned window (guide lines 179-184).
    async fn list_queue(
        &self,
        ctx: &SecurityContext,
        platform_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<QueueEntry>, QaRunsError>;

    /// Drop a `queued` row before it starts. Fails `Aborted` when the row is
    /// no longer `queued` — a `dispatching`/`running` row holds a claim an
    /// in-flight execution depends on, and dropping it would let a new run be
    /// admitted beside an exclusive one
    /// (`manager/src/services/run_queue.rs:403-421`). Cancel a started run
    /// through [`cancel_run`](Self::cancel_run) instead.
    async fn cancel_queued(&self, ctx: &SecurityContext, queue_id: Uuid)
    -> Result<(), QaRunsError>;

    /// Start a queued row now, ignoring what occupies the platform —
    /// including an in-flight exclusive run. Deliberately does **not**
    /// override `max_concurrent_runs`: overriding a platform is a testing
    /// decision an operator may want, while overriding cluster capacity can
    /// wedge the whole execution plane for everyone (guide lines 116-120).
    async fn force_start_queued(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
    ) -> Result<Run, QaRunsError>;

    // ==================== Schedules (Phase B) ====================

    async fn list_schedules(&self, ctx: &SecurityContext) -> Result<Vec<Schedule>, QaRunsError>;

    async fn get_schedule(&self, ctx: &SecurityContext, id: Uuid)
    -> Result<Schedule, QaRunsError>;

    async fn create_schedule(
        &self,
        ctx: &SecurityContext,
        new: NewSchedule,
    ) -> Result<Schedule, QaRunsError>;

    async fn update_schedule(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        new: NewSchedule,
    ) -> Result<Schedule, QaRunsError>;

    async fn delete_schedule(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaRunsError>;
}
```

- [ ] **Step 5: `lib.rs` re-exports.** Mirror `qa-catalog-sdk/src/lib.rs`'s style: re-export every public model, the error alias, and the trait.

- [ ] **Step 6: Verify.**

```bash
cargo build -p qa-runs-sdk
cargo clippy -p qa-runs-sdk --all-targets -- -D warnings
grep -rn 'serde\|utoipa\|^use http' gears/qa-platform/qa-runs/qa-runs-sdk/src/ | grep -v '^\s*[^:]*:[0-9]*://'
```

Expected: build and clippy green; the `grep` prints **nothing** — contract purity (SDK crates stay free of `serde`/`utoipa`/`http`; **no CI lint enforces this** — the two `de010x_no_*_in_contract` lints are in `Gears.toml`'s dylint skip list, so this grep is the only real gate).

**Corrected 2026-08-13 (Task 4):** the grep originally had no `//` filter and therefore **could never print nothing**, because `lib.rs`'s own module doc names all three crates while stating the rule. The `| grep -v` clause excludes doc-comment lines so the gate can actually pass. Verify the filter is doing that and not masking a real hit — check the unfiltered output by eye once.

- [ ] **Step 7: Commit.** `git commit -m "feat(qa-runs): SDK models, errors, and client trait"`

---

### Task 5: TDD core 1 — exclusivity resolution

**This is the highest-value TDD core in the subsystem.** It is pure — no I/O, no database, no fixture tree — for exactly the reason the source system gives: "The rule lives in pure functions because this crate has no test database and no fixture tree: the async half below is plan lookup and file reads, and the part worth testing is which tier wins" (`../testrunner/manager/src/services/exclusivity.rs:5-7`).

**Files:**
- Create: `qa-runs/src/domain/mod.rs`, `qa-runs/src/domain/error.rs`, `qa-runs/src/domain/exclusivity.rs`
- Modify: `qa-runs/src/lib.rs` (uncomment `pub mod domain;`, remove that `// Task 5:` marker)

**Owns:** the three created files plus the one uncommented line in `lib.rs`. Does not own `domain/queue.rs` (Task 6) or `domain/state_machine.rs` (Task 7).

**Expected remaining errors after this task:** none — this task is self-contained and must leave the build green.

- [ ] **Step 1: Legacy check — read the whole of `exclusivity.rs` and `test_meta.rs` before writing a line.** Specifically confirm each of these seven rules and record its `file:line` in the code comment beside the logic it justifies. **If any differs, stop and report per the protocol.**

  1. Precedence is `launch ?? plan.yaml ?? test_meta ?? false`, first tier with an opinion wins, and `false` **is** an opinion — `exclusivity.rs:120-143`.
  2. `aggregate_test_meta(&[])` is `None`; a non-empty set is `Some(OR)` — so "files were read and all said parallel" is `Some(false)`, **not** `None` — `exclusivity.rs:152-158`, with tests at `:744-756`.
  3. A file dropped by the run's tag filter contributes **nothing** (`None`), not `false` — `exclusivity.rs:163-173`, test at `:760-767`.
  4. `tags_admit` is deliberately asymmetric: an untagged file is **admitted** by an exclude-only filter and **rejected** by an include filter — fail-open for exclude, fail-closed for include — `test_meta.rs:92-108`, tests at `:209-216`. All three tag inputs are trimmed and lowercased on both sides — `test_meta.rs:93-102`, test at `:225-230`.
  5. A single-test run considers only that one file and **no tag filter applies** — `exclusivity.rs:207-224` (passes `&[], &[]`), guide line 49.
  6. Resolution **never fails a launch**: an unresolvable plan resolves parallel with a warning, and the launch fails later at dispatch for the real reason — `exclusivity.rs:176-179, 234-249`, guide lines 215-216.
  7. A custom plan combines its nested plans with `combine_nested`, whose reported tier names the **strongest source that actually contributed** — including the fourth branch where no `plan.yaml` spoke anywhere and every scanned file said parallel, which must report `TestMeta` and not `Default` — `exclusivity.rs:474-488`, test at `:656-664`.

- [ ] **Step 2: Legacy check — the `Option<bool>` mapping (DECOMPOSITION 2.2's flagged reconciliation).** The source system's per-file parser returns `bool` (`test_meta.rs:42-54` — a missing key is `false`), so `file_declares_exclusive` yields `Some(false)` for an admitted file with no declaration (`exclusivity.rs:172`). qa-catalog's `TestFileMeta::exclusive` is `Option<bool>` where `None` means the key was absent. Confirm both, then implement the mapping as: **an admitted file contributes `meta.exclusive.unwrap_or(false)`; only a file that was not read at all, or was dropped by the filter, contributes nothing.** This is the reconciliation DECOMPOSITION:148 flags for `cpt-cf-qa-fr-runs-exclusivity` — get it wrong the naive way (`None` ⇒ no opinion) and a run whose files were all read and all said parallel is misattributed to tier `Default`, which the source system has an explicit test forbidding.

- [ ] **Step 3: Write the failing tests.** In `qa-runs/src/domain/exclusivity.rs`, inline `#[cfg(test)] mod tests`. Every expected value below was read out of legacy, not out of a run of the new code.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn meta(path: &str, tags: &[&str], exclusive: Option<bool>) -> FileMeta {
        FileMeta {
            path: path.to_string(),
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            exclusive,
        }
    }

    // ---------- resolve_precedence: the cascade ----------

    #[test]
    fn launch_true_wins_over_plan_false() {
        let r = resolve_precedence(Tiers {
            launch: Some(true),
            plan: Some(false),
            test_meta: Some(false),
        });
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Launch);
    }

    /// The point of `Option<bool>` at the upper tiers: an operator can run an
    /// exclusive-marked suite in parallel tonight without editing test files
    /// (guide lines 38-41).
    #[test]
    fn launch_false_wins_over_exclusive_tests() {
        let r = resolve_precedence(Tiers {
            launch: Some(false),
            test_meta: Some(true),
            ..Tiers::default()
        });
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Launch);
    }

    #[test]
    fn plan_yaml_wins_when_there_is_no_launch_override() {
        let r = resolve_precedence(Tiers {
            plan: Some(true),
            test_meta: Some(false),
            ..Tiers::default()
        });
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);
    }

    #[test]
    fn plan_yaml_false_beats_an_exclusive_test() {
        let r = resolve_precedence(Tiers {
            plan: Some(false),
            test_meta: Some(true),
            ..Tiers::default()
        });
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);
    }

    #[test]
    fn test_meta_decides_when_the_upper_tiers_are_silent() {
        let r = resolve_precedence(Tiers {
            test_meta: Some(true),
            ..Tiers::default()
        });
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    /// The tier reconciliation DECOMPOSITION 2.2 flags: an explicit
    /// all-parallel answer from TEST_META must report the TestMeta tier, not
    /// Default. Legacy asserts this directly (exclusivity.rs:722-730).
    #[test]
    fn test_meta_false_still_reports_the_test_meta_tier() {
        let r = resolve_precedence(Tiers {
            test_meta: Some(false),
            ..Tiers::default()
        });
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    #[test]
    fn nothing_declared_defaults_to_parallel() {
        let r = resolve_precedence(Tiers::default());
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Default);
    }

    // ---------- aggregate_test_meta ----------

    /// No file was read (a plan whose tests are missing from the snapshot), so
    /// the tier has no opinion — it must not masquerade as an explicit
    /// "parallel", or a plan.yaml-less exclusive suite looks deliberately
    /// parallel in the logs (exclusivity.rs:739-746).
    #[test]
    fn aggregate_of_an_empty_file_set_has_no_opinion() {
        assert_eq!(aggregate_test_meta(&[]), None);
    }

    #[test]
    fn aggregate_is_an_or_over_files() {
        assert_eq!(aggregate_test_meta(&[false, true, false]), Some(true));
    }

    #[test]
    fn aggregate_of_all_parallel_files_is_an_explicit_false() {
        assert_eq!(aggregate_test_meta(&[false, false]), Some(false));
    }

    // ---------- file_declares_exclusive: the tag-filter asymmetry ----------

    /// A `destructive` test this run's exclude_tags drops is not going to
    /// execute, so it must not make the run exclusive (exclusivity.rs:759-767).
    #[test]
    fn a_file_dropped_by_the_exclude_filter_does_not_contribute() {
        let f = meta("a.py", &["destructive"], Some(true));
        assert_eq!(
            file_declares_exclusive(&f, &[], &["destructive".to_string()]),
            None
        );
    }

    #[test]
    fn a_file_kept_by_the_include_filter_contributes_its_flag() {
        let f = meta("a.py", &["e2e"], Some(true));
        assert_eq!(
            file_declares_exclusive(&f, &["e2e".to_string()], &[]),
            Some(true)
        );
    }

    /// The Option<bool> -> bool reconciliation: a file that WAS read and
    /// declared nothing contributes an explicit `false`, because legacy's
    /// per-file parser returns `bool` and a missing key is false
    /// (test_meta.rs:42-54, exclusivity.rs:172). Without this, an all-quiet
    /// plan is misattributed to tier Default.
    #[test]
    fn a_read_file_with_no_declaration_contributes_false_not_nothing() {
        let f = meta("a.py", &["e2e"], None);
        assert_eq!(
            file_declares_exclusive(&f, &[], &[]),
            Some(false),
            "a read file always votes; only an unread or filtered-out file abstains"
        );
    }

    #[test]
    fn a_file_declaring_parallel_contributes_false() {
        let f = meta("a.py", &[], Some(false));
        assert_eq!(file_declares_exclusive(&f, &[], &[]), Some(false));
    }

    // ---------- tags_admit: the deliberate asymmetry ----------

    #[test]
    fn an_empty_filter_admits_everything() {
        assert!(tags_admit(&["e2e".to_string()], &[], &[]));
        assert!(tags_admit(&[], &[], &[]));
    }

    #[test]
    fn include_filter_keeps_only_matching_tags() {
        let tags = vec!["e2e".to_string(), "destructive".to_string()];
        assert!(tags_admit(&tags, &["e2e".to_string()], &[]));
        assert!(!tags_admit(&tags, &["smoke".to_string()], &[]));
    }

    /// The asymmetry, stated as two assertions so it is a decision rather than
    /// an accident: an untagged file cannot prove membership, so an include
    /// filter drops it (fail-closed), while an exclude-only filter admits it
    /// (fail-open). test_meta.rs:89-91, :214-215.
    #[test]
    fn an_untagged_file_is_dropped_by_include_and_admitted_by_exclude() {
        assert!(
            !tags_admit(&[], &["smoke".to_string()], &[]),
            "fail-closed for include: an untagged file cannot prove membership"
        );
        assert!(
            tags_admit(&[], &[], &["destructive".to_string()]),
            "fail-open for exclude: an untagged file matches no exclusion"
        );
    }

    #[test]
    fn exclude_filter_drops_matching_tags() {
        let tags = vec!["e2e".to_string(), "destructive".to_string()];
        assert!(!tags_admit(&tags, &[], &["destructive".to_string()]));
        assert!(tags_admit(&tags, &[], &["flaky".to_string()]));
    }

    #[test]
    fn filters_are_trimmed_and_case_insensitive_on_both_sides() {
        let tags = vec!["Destructive".to_string()];
        assert!(!tags_admit(&tags, &[], &["DESTRUCTIVE".to_string()]));
        assert!(tags_admit(&tags, &[" destructive ".to_string()], &[]));
    }

    /// Both sides normalize, so an entry that is only whitespace is dropped
    /// rather than becoming an unmatchable filter (test_meta.rs:93-99).
    #[test]
    fn blank_filter_entries_are_ignored() {
        let tags = vec!["e2e".to_string()];
        assert!(
            tags_admit(&tags, &["   ".to_string()], &[]),
            "a whitespace-only include entry must not reject everything"
        );
    }

    // ---------- resolve_plan_tier: plan.yaml short-circuits ----------

    /// plan.yaml outranks TEST_META, so a declared plan flag decides without
    /// any file being consulted (exclusivity.rs:342-348).
    #[test]
    fn a_declared_plan_flag_short_circuits_the_file_scan() {
        let files = vec![meta("a.py", &[], Some(true))];
        let r = resolve_plan_tier(Some(false), &files, &[], &[]);
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);
    }

    #[test]
    fn an_absent_plan_flag_falls_through_to_the_files() {
        let files = vec![meta("a.py", &[], Some(false)), meta("b.py", &[], Some(true))];
        let r = resolve_plan_tier(None, &files, &[], &[]);
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    /// Every file filtered out => nobody voted => Default, and critically NOT
    /// an explicit parallel. Legacy reaches the same state through
    /// aggregate_test_meta(&[]) (exclusivity.rs:440-449 keeps this quiet when
    /// the filter legitimately excluded everything).
    #[test]
    fn a_plan_whose_every_file_is_filtered_out_resolves_default() {
        let files = vec![meta("a.py", &["destructive"], Some(true))];
        let r = resolve_plan_tier(None, &files, &[], &["destructive".to_string()]);
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Default);
    }

    /// One destructive file among many makes the whole run exclusive
    /// (guide line 47).
    #[test]
    fn one_exclusive_file_among_fifty_makes_the_run_exclusive() {
        let mut files: Vec<FileMeta> = (0..49)
            .map(|i| meta(&format!("t{i}.py"), &["smoke"], Some(false)))
            .collect();
        files.push(meta("upgrade.py", &["destructive"], Some(true)));
        let r = resolve_plan_tier(None, &files, &[], &[]);
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    // ---------- combine_nested: custom plans ----------

    #[test]
    fn a_nested_plan_yaml_that_says_exclusive_makes_the_custom_plan_exclusive() {
        let r = combine_nested(Some(true), Some(false));
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);
    }

    #[test]
    fn a_nested_exclusive_test_makes_the_custom_plan_exclusive() {
        let r = combine_nested(Some(false), Some(true));
        assert!(r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    #[test]
    fn a_custom_plan_of_parallel_plans_stays_parallel() {
        let r = combine_nested(Some(false), None);
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Plan);

        let r = combine_nested(None, None);
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::Default);
    }

    /// The fourth cascade branch: no plan.yaml anywhere, and every scanned
    /// file said parallel. The answer is false, but it came from TEST_META
    /// rather than from nobody having an opinion — the log must not read
    /// `default` (exclusivity.rs:656-664, which is an explicit legacy test).
    #[test]
    fn a_custom_plan_whose_tests_all_declare_parallel_reports_the_test_meta_tier() {
        let r = combine_nested(None, Some(false));
        assert!(!r.exclusive);
        assert_eq!(r.tier, ExclusiveTier::TestMeta);
    }

    // ---------- the launch tier short-circuits everything ----------

    /// A launch-level override wins outright, so no file work is done for it
    /// (exclusivity.rs:181-187). Asserted here as "the answer does not depend
    /// on the files at all".
    #[test]
    fn a_launch_override_ignores_every_lower_tier_input() {
        let files = vec![meta("a.py", &[], Some(true))];
        let with_files = resolve(Some(false), Some(true), &files, &[], &[]);
        let without_files = resolve(Some(false), Some(true), &[], &[], &[]);
        assert_eq!(with_files, without_files);
        assert!(!with_files.exclusive);
        assert_eq!(with_files.tier, ExclusiveTier::Launch);
    }
}
```

- [ ] **Step 4: Run the tests to confirm they fail.**

```bash
cargo test -p qa-runs exclusivity
```

Expected: **compile failure**, `cannot find function `resolve_precedence` in this scope` and similar for every function and for `FileMeta`/`Tiers`/`Resolution`. Not assertion failures — the module has no implementation yet.

- [ ] **Step 5: Implement.**

```rust
//! Effective exclusivity of a launch: `launch ?? plan.yaml ?? OR(TEST_META
//! over the files that will run) ?? parallel`.
//!
//! Ported from `manager/src/services/exclusivity.rs` and the tag-admission
//! half of `manager/src/services/test_meta.rs`. Frozen semantics — the
//! governing document is `../testrunner/docs/guides/exclusive-runs-and-the-queue.md`.
//!
//! Pure by design, for the reason the source system gives
//! (`exclusivity.rs:5-7`): the async half is plan lookup and file reads, and
//! the part worth testing is which tier wins. Everything here takes already-
//! fetched data, so the whole contract is unit-testable with no database and
//! no fixture tree. qa-catalog supplies the inputs (per-plan and per-file
//! flags plus tags) and deliberately does not interpret them
//! (DESIGN §3.2, qa-catalog "Responsibility boundaries").

use qa_runs_sdk::ExclusiveTier;

/// One file's exclusivity-relevant metadata, as qa-catalog returns it.
///
/// A local projection of `qa_catalog_sdk::TestFileMeta` rather than the SDK
/// type itself: this module must stay callable from unit tests without
/// constructing the catalog's full model, and the projection documents
/// exactly which three fields the rule depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMeta {
    pub path: String,
    pub tags: Vec<String>,
    /// Three-state as the catalog parses it: `None` = the key was absent.
    pub exclusive: Option<bool>,
}

/// The three tiers that can have an opinion, as named fields.
///
/// Named rather than three positional `Option<bool>`s because the arguments
/// are mutually indistinguishable to the compiler: transposing `launch` and
/// `plan` silently defeats an operator's override *and* mislabels the tier in
/// the log. Same reasoning the source system records
/// (`manager/src/services/exclusivity.rs:88-95`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tiers {
    /// The launch request's override. For a scheduled run this *is* the
    /// schedule's stored choice, delivered through the launch request — so by
    /// the time resolution runs, a schedule's choice is `launch` and the two
    /// can never disagree (`exclusivity.rs:113-119`).
    pub launch: Option<bool>,
    /// `plan.yaml`'s declaration.
    pub plan: Option<bool>,
    /// The OR over the in-scope files' TEST_META, or `None` when no file
    /// contributed at all.
    pub test_meta: Option<bool>,
}

/// The effective flag plus where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub exclusive: bool,
    pub tier: ExclusiveTier,
}

/// `launch ?? plan.yaml ?? test_meta ?? false`. The first tier with an opinion
/// wins, and `false` **is** an opinion — that is the whole reason the upper
/// tiers are `Option<bool>`. With plain `bool` the rule "first one that is set
/// wins" is unimplementable, because unset and `false` collide and exclusivity
/// could only ever escalate (`manager/src/services/exclusivity.rs:108-119`).
#[must_use]
pub fn resolve_precedence(tiers: Tiers) -> Resolution {
    if let Some(exclusive) = tiers.launch {
        return Resolution { exclusive, tier: ExclusiveTier::Launch };
    }
    if let Some(exclusive) = tiers.plan {
        return Resolution { exclusive, tier: ExclusiveTier::Plan };
    }
    if let Some(exclusive) = tiers.test_meta {
        return Resolution { exclusive, tier: ExclusiveTier::TestMeta };
    }
    Resolution { exclusive: false, tier: ExclusiveTier::Default }
}

/// Aggregate the TEST_META tier over a file set: `None` when no file voted,
/// else the OR.
///
/// OR rather than "first wins" because per file `exclusive: False` honestly
/// means "I do not need the platform to myself", not "forbid exclusivity for
/// the whole run" — one destructive test makes the suite destructive
/// (`manager/src/services/exclusivity.rs:145-158`; guide line 47).
#[must_use]
pub fn aggregate_test_meta(flags: &[bool]) -> Option<bool> {
    if flags.is_empty() {
        None
    } else {
        Some(flags.iter().any(|flag| *flag))
    }
}

/// Whether a file's tags satisfy an include/exclude filter.
///
/// Ported verbatim from `manager/src/services/test_meta.rs:92-108`, asymmetry
/// included and **on purpose**: a file with no tags is admitted by an
/// exclude-only filter and rejected by an include filter, because it cannot
/// prove membership. Fail-open for exclude, fail-closed for include. All three
/// inputs are trimmed and lowercased here so no caller can normalize one side
/// and forget the other (`test_meta.rs:87`).
#[must_use]
pub fn tags_admit(tags: &[String], include_tags: &[String], exclude_tags: &[String]) -> bool {
    fn normalize(values: &[String]) -> Vec<String> {
        values
            .iter()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect()
    }
    let tags = normalize(tags);
    let included = normalize(include_tags);
    let excluded = normalize(exclude_tags);

    if !included.is_empty() && !tags.iter().any(|tag| included.contains(tag)) {
        return false;
    }
    !tags.iter().any(|tag| excluded.contains(tag))
}

/// One file's contribution to the TEST_META tier, or `None` when the run's tag
/// filter excludes it — the guide's "all its test files are considered *after*
/// tag filtering" (`manager/src/services/exclusivity.rs:160-173`).
///
/// The `Option<bool>` -> `bool` reconciliation flagged in DECOMPOSITION 2.2
/// lives on the last line. The source system's per-file parser returns `bool`,
/// where a missing `exclusive` key is `false` (`test_meta.rs:42-54`), so an
/// admitted file **always votes** and votes `false` when it declared nothing.
/// Only an unread file (never reaches here) or a filtered-out one abstains.
/// Reading catalog's `None` as "no opinion" instead would misattribute a run
/// whose files were all read and all parallel to tier `Default`, which the
/// source system has an explicit test forbidding (`exclusivity.rs:656-664`).
#[must_use]
pub fn file_declares_exclusive(
    file: &FileMeta,
    include_tags: &[String],
    exclude_tags: &[String],
) -> Option<bool> {
    if !tags_admit(&file.tags, include_tags, exclude_tags) {
        return None;
    }
    Some(file.exclusive.unwrap_or(false))
}

/// Resolve one standard plan: its `plan.yaml` tier, falling through to
/// TEST_META over the files this run will execute.
///
/// `plan_flag` short-circuits: `plan.yaml` outranks TEST_META, so a declared
/// flag decides without any file being consulted
/// (`manager/src/services/exclusivity.rs:342-348`).
///
/// For a single-test run, pass a one-element `files` and **empty** tag
/// filters: a single-test run considers only that file and no filter applies
/// (`exclusivity.rs:207-224`; guide line 49).
#[must_use]
pub fn resolve_plan_tier(
    plan_flag: Option<bool>,
    files: &[FileMeta],
    include_tags: &[String],
    exclude_tags: &[String],
) -> Resolution {
    if let Some(declared) = plan_flag {
        return resolve_precedence(Tiers { plan: Some(declared), ..Tiers::default() });
    }
    let flags: Vec<bool> = files
        .iter()
        .filter_map(|file| file_declares_exclusive(file, include_tags, exclude_tags))
        .collect();
    resolve_precedence(Tiers {
        test_meta: aggregate_test_meta(&flags),
        ..Tiers::default()
    })
}

/// Combine the nested plans' answers for a custom-plan run: a custom plan is
/// one run, and its flag is the OR over the plans it composes.
///
/// Within one nested plan, `plan.yaml` outranks TEST_META (that happens in
/// [`resolve_plan_tier`]). Across plans the aggregate is an OR, and the
/// reported tier names the **strongest source that actually contributed** —
/// including the fourth branch, where nothing declared a plan flag and every
/// scanned file said parallel: that reports `TestMeta`, not `Default`
/// (`manager/src/services/exclusivity.rs:468-488`).
#[must_use]
pub fn combine_nested(plan_tier: Option<bool>, test_meta_tier: Option<bool>) -> Resolution {
    let exclusive = plan_tier.unwrap_or(false) || test_meta_tier.unwrap_or(false);
    let tier = if plan_tier == Some(true) {
        ExclusiveTier::Plan
    } else if test_meta_tier == Some(true) {
        ExclusiveTier::TestMeta
    } else if plan_tier.is_some() {
        ExclusiveTier::Plan
    } else if test_meta_tier.is_some() {
        ExclusiveTier::TestMeta
    } else {
        ExclusiveTier::Default
    };
    Resolution { exclusive, tier }
}

/// The whole rule in one call, for a single plan.
///
/// Separate from [`resolve_plan_tier`] so the launch-tier short-circuit is
/// visible and testable: a launch override wins outright and does no work on
/// the lower tiers at all (`manager/src/services/exclusivity.rs:181-187`),
/// which is why the same override must produce the same answer whether or not
/// any file metadata was fetched.
#[must_use]
pub fn resolve(
    launch: Option<bool>,
    plan_flag: Option<bool>,
    files: &[FileMeta],
    include_tags: &[String],
    exclude_tags: &[String],
) -> Resolution {
    if let Some(exclusive) = launch {
        return Resolution { exclusive, tier: ExclusiveTier::Launch };
    }
    resolve_plan_tier(plan_flag, files, include_tags, exclude_tags)
}
```

`domain/mod.rs` for this task:

```rust
//! Domain layer: pure rules, ports, repository traits, and services.

pub mod error;
pub mod exclusivity;
```

`domain/error.rs` — start it now with only what this task needs, and let later tasks extend it. Copy the structural shape from `qa-environments/src/domain/error.rs` (`#[domain_model]`, `thiserror::Error`, the two `From` impls for `toolkit_db::DbError` and `authz_resolver_sdk::EnforcerError` with their `#[allow(unknown_lints, de1302_error_from_to_string)]`), with these variants:

```rust
    #[error("run {id} not found")]
    RunNotFound { id: Uuid },

    #[error("queue row {id} not found")]
    QueueRowNotFound { id: Uuid },

    #[error("validation failed on {field}: {message}")]
    Validation { field: String, message: String },

    #[error("access denied")]
    Forbidden,

    #[error("database error: {0}")]
    Database(String),

    #[error("internal error: {0}")]
    Internal(String),
```

- [ ] **Step 6: Run the tests.**

```bash
cargo test -p qa-runs exclusivity
```

Expected: **29 passed; 0 failed** — Step 3 defines exactly 29 `#[test]` functions. Count them; if the run reports fewer, a test was dropped in transcription.

- [ ] **Step 7: Full gate.**

```bash
cargo build -p qa-runs && cargo clippy -p qa-runs --all-targets -- -D warnings && cargo fmt --check -p qa-runs
```

Expected: all green. Note `resolve_plan_tier`'s `#[must_use]` and the `pub` surface — services are `pub(crate)` in this gear family (avoids `missing_errors_doc` on the public surface), but these pure functions are genuinely part of the domain's testable surface and stay `pub` within the crate's `domain` module. If clippy demands `pub(crate)`, apply it and keep the tests in-module.

- [ ] **Step 8: Commit.** `git commit -m "feat(qa-runs): exclusivity resolution with ported tier and tag-filter semantics"`

---

### Task 6: TDD core 2 — queue admission and FIFO dispatch planning

> **Two interface corrections applied 2026-08-13 after this task's code review. Both are about the module's *shape*, not its rules.**
>
> 1. **Positions are `u32`, not `i64`.** This section originally specified `assign_positions -> HashMap<Uuid, i64>` and `describe_blocker(position: i64, …)`, and Task 6 correctly followed it — but Task 4's review had already narrowed the SDK to `queue_position: Option<u32>` (`qa-runs-sdk/src/models.rs:389`) and **that change was never propagated here**, so the gear contradicted its own contract. The `i64` came from legacy's SQL `BIGINT`; nothing in this gear does a `row_number()` — positions are computed in memory from rows already returned and never stored. Leaving it would force Task 16's mapper into a `u32::try_from` with an error path for a value guaranteed `>= 1`, under two `deny` lints (`cast_possible_truncation`, `cast_sign_loss`).
> 2. **The global cap is a named struct, not a `(u32, u32)` tuple.** `global_cap_status`, `cap_reached`, and `plan_dispatch_batch` all thread two same-typed `u32`s in a documented order, so `global_cap_status(max, active)` compiles and silently inverts the cap. Task 14 must *construct* that pair by hand when threading the budget across platforms — the call site where a transposition is likeliest and least visible. Use `pub struct GlobalCap { pub active: u32, pub max: u32 }` with a `with_claimed(self, n: u32) -> Self` helper, so the cross-platform budget arithmetic lives here and is testable rather than being a prose obligation on Task 14. This codebase has ruled on this hazard twice already — `Tiers` in `exclusivity.rs:55-61` and legacy's own `OccupancySources` (`run_queue.rs:682-690`, "as named fields so they cannot be transposed at a call site"). The tuple is legacy's older code, not its considered position.

Pure, for the same reason as Task 5 and stated in the source: "The two policy functions (`decide_admission`, `plan_dispatch_batch`) are deliberately pure: this crate is binary-only with no test database and no Argo fixture, so keeping the rules free of I/O is the only way they can be unit-tested" (`../testrunner/manager/src/services/run_queue.rs:4-7`).

**Files:**
- Create: `qa-runs/src/domain/queue.rs`
- Modify: `qa-runs/src/domain/mod.rs` (add `pub mod queue;`)

**Owns:** those two. Does not own `domain/state_machine.rs` (Task 7) or any service (Tasks 10–13).

**Expected remaining errors after this task:** none.

- [ ] **Step 1: Legacy check — read all of `run_queue.rs`'s pure half plus `run_dispatcher.rs`'s occupancy helpers.** Confirm each rule and cite it. **If any differs, stop and report.**

  1. `platform_admits`: an exclusive occupant means nothing joins; an incoming exclusive run needs the platform empty — `run_queue.rs:20-24`.
  2. `decide_admission` checks `queued_depth > 0` **before** occupancy, and that ordering is load-bearing: strict FIFO, so once anything is queued for a platform every later arrival queues behind it, whatever its flag — otherwise a stream of parallel runs starves a queued exclusive row forever — `run_queue.rs:42-51`, guide lines 105-107.
  3. `decide_admission` **ignores** the global cap: exceeding `max_concurrent_runs` is a 429 at admission, never a silently-queued row — `run_queue.rs:30-40, 1281-1286`.
  4. The depth check happens **inside the same lock as the depth read**, and **before** the occupancy read, because a rejection writes no row — `run_queue.rs:633-655`.
  5. `plan_dispatch_batch` **stops rather than skips** at a blocked row (a row behind a blocked one must not overtake it) and **accumulates** claims into occupancy as it goes, so an exclusive row claimed this tick blocks everything behind it — `run_queue.rs:77-98`, tests at `:1332-1341`.
  6. The cap arithmetic counts **only new claims**, never existing occupancy, because `active` is already cluster-wide — `run_queue.rs:83-87`, test at `:1400-1410`.
  7. `queue_is_full` is deliberately separate from `decide_admission`, and a limit of 1 rejects the *second* waiter but never the first — so a launch that would have started inline on an idle platform can never be rejected by the depth rule — `run_queue.rs:821-832`, test at `:1133-1137`.
  8. Both TTL helpers fail **open** on a zero or unusable value (`None` = do not expire), because a stale queued row is an annoyance whereas a panicking dispatcher tick stops the queue draining at all — `run_queue.rs:729-759`, tests at `:1005-1015`.
  9. `describe_blocker`'s five arms and their exact strings — `run_queue.rs:778-808`.

- [ ] **Step 2: Legacy check — how occupancy is obtained, and what replaces the merge.** Read `run_queue.rs:682-712` (`merge_occupancy`, `OccupancySources`) and `run_dispatcher.rs:221-254` (`argo_occupants`, `argo_occupancy_for_platform`). Confirm the merge exists because Argo and the DB are two independent views of one fact, and that the fail-safe on an unreadable Argo is a **synthetic exclusive occupant** so nothing dispatches blind (`run_dispatcher.rs:237-252`). Then confirm the mapping this plan takes: `qa-environments`' lease is a single authoritative view, so `merge_occupancy` has no counterpart, and `LeaseState` maps to the local `Occupancy` below. **This is the largest legacy→gear adaptation in the plan — if the reviewer disagrees that the lease subsumes the merge, stop and report before implementing.**

- [ ] **Step 3: Write the failing tests.** In `qa-runs/src/domain/queue.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use qa_environments_sdk::LeaseState;
    use time::macros::datetime;

    fn row(n: u8, exclusive: bool) -> QueuedRow {
        QueuedRow { id: uuid::Uuid::from_u128(u128::from(n)), exclusive }
    }

    fn holder(n: u8) -> uuid::Uuid {
        uuid::Uuid::from_u128(u128::from(n))
    }

    // ---------- Occupancy::from_lease ----------

    #[test]
    fn a_free_lease_is_unoccupied() {
        assert_eq!(Occupancy::from_lease(&LeaseState::Free), Occupancy::Free);
    }

    #[test]
    fn parallel_holders_are_a_parallel_occupancy() {
        let state = LeaseState::HeldParallel { holders: vec![holder(1), holder(2)] };
        assert_eq!(Occupancy::from_lease(&state), Occupancy::Parallel { holders: 2 });
    }

    /// An empty parallel holder list is a lease row that exists but holds
    /// nothing — it must read Free, not as a zero-holder occupancy that
    /// blocks an incoming exclusive run forever.
    #[test]
    fn an_empty_parallel_holder_list_reads_free() {
        let state = LeaseState::HeldParallel { holders: Vec::new() };
        assert_eq!(Occupancy::from_lease(&state), Occupancy::Free);
    }

    #[test]
    fn an_exclusive_hold_is_an_exclusive_occupancy() {
        let state = LeaseState::HeldExclusive { holder: holder(1) };
        assert_eq!(Occupancy::from_lease(&state), Occupancy::Exclusive);
    }

    // ---------- platform_admits ----------

    #[test]
    fn a_parallel_run_joins_a_platform_busy_with_parallel_runs() {
        assert!(platform_admits(Occupancy::Parallel { holders: 1 }, false));
    }

    #[test]
    fn a_parallel_run_starts_on_an_idle_platform() {
        assert!(platform_admits(Occupancy::Free, false));
    }

    #[test]
    fn an_exclusive_run_needs_the_platform_to_itself() {
        assert!(platform_admits(Occupancy::Free, true));
        assert!(!platform_admits(Occupancy::Parallel { holders: 1 }, true));
    }

    #[test]
    fn an_exclusive_occupant_admits_nothing() {
        assert!(!platform_admits(Occupancy::Exclusive, false));
        assert!(!platform_admits(Occupancy::Exclusive, true));
    }

    // ---------- decide_admission ----------

    #[test]
    fn an_admissible_launch_dispatches() {
        assert_eq!(
            decide_admission(Occupancy::Free, 0, false),
            AdmissionDecision::Dispatch
        );
    }

    #[test]
    fn a_blocked_launch_queues() {
        assert_eq!(
            decide_admission(Occupancy::Exclusive, 0, false),
            AdmissionDecision::Queue
        );
    }

    /// Strict FIFO: once anything is queued for a platform, later arrivals
    /// queue behind it even on an idle platform — otherwise a queued exclusive
    /// row starves behind a stream of parallel ones (run_queue.rs:42-47, guide
    /// lines 105-107). This pins that the depth rule *exists*: delete it and an
    /// idle platform dispatches past its own queue.
    ///
    /// **Corrected 2026-08-13 during execution.** This doc previously claimed
    /// the pair "pins that ordering" — that the depth check precedes the
    /// occupancy check. **No test of this pure function can pin that**, and the
    /// implementer proved it: both branches return `Queue`, so the function is
    /// observationally `Queue if depth > 0 || !admits`, and swapping the two
    /// `if`s leaves the whole suite green. The ordering is only real at the
    /// service call site, where it must happen inside the platform lock and
    /// *before* the lease read (`run_queue.rs:633-655`) — otherwise two
    /// concurrent launches against a one-slot queue both see room. That is
    /// Task 10's obligation, not a property this module can hold.
    #[test]
    fn a_non_empty_queue_holds_back_a_launch_on_an_idle_platform() {
        assert_eq!(
            decide_admission(Occupancy::Free, 1, false),
            AdmissionDecision::Queue
        );
        assert_eq!(
            decide_admission(Occupancy::Free, 3, true),
            AdmissionDecision::Queue
        );
    }

    /// The commonest production state — a busy platform that also has a queue.
    #[test]
    fn a_queued_row_holds_back_a_new_run_on_an_already_busy_platform() {
        assert_eq!(
            decide_admission(Occupancy::Parallel { holders: 1 }, 2, false),
            AdmissionDecision::Queue
        );
    }

    // ---------- queue_is_full ----------

    #[test]
    fn an_unlimited_queue_is_never_full() {
        assert!(!queue_is_full(0, None));
        assert!(!queue_is_full(9_999, None));
    }

    #[test]
    fn a_queue_at_or_over_its_limit_is_full() {
        assert!(!queue_is_full(19, Some(20)));
        assert!(queue_is_full(20, Some(20)));
        // Reachable after an operator lowers queue_max_depth below the current
        // depth. Rejecting new launches is correct; queued rows still drain.
        assert!(queue_is_full(25, Some(20)));
    }

    /// The property that keeps the depth rule from breaking today's behaviour:
    /// with a limit of 1 an empty queue is still open, so a launch that
    /// decide_admission would dispatch inline is never turned into a 429. Only
    /// the second waiter is rejected (run_queue.rs:1129-1137).
    #[test]
    fn a_limit_of_one_rejects_the_second_waiter_but_not_the_first() {
        assert!(!queue_is_full(0, Some(1)));
        assert!(queue_is_full(1, Some(1)));
    }

    /// depth_limit(0) is what keeps a disabled setting from rejecting every
    /// launch with a nonsensical "0 of 0 slots used" (run_queue.rs:1139-1146).
    #[test]
    fn a_disabled_depth_setting_composes_to_unlimited() {
        assert!(!queue_is_full(0, depth_limit(0)));
        assert!(!queue_is_full(9_999, depth_limit(0)));
        assert_eq!(depth_limit(20), Some(20));
    }

    // ---------- global cap ----------

    #[test]
    fn a_zero_max_disables_the_global_cap() {
        assert_eq!(global_cap_status(5, 0), None);
    }

    #[test]
    fn the_cap_reports_its_numbers_and_its_reached_state() {
        assert_eq!(global_cap_status(3, 6), Some((3, 6)));
        assert!(!cap_reached(Some((5, 6))));
        assert!(cap_reached(Some((6, 6))));
        assert!(cap_reached(Some((7, 6))));
        assert!(!cap_reached(None));
    }

    // ---------- plan_dispatch_batch ----------

    #[test]
    fn drains_consecutive_parallel_rows_in_one_tick() {
        let rows = vec![row(1, false), row(2, false), row(3, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, None),
            vec![row(1, false).id, row(2, false).id, row(3, false).id]
        );
    }

    #[test]
    fn stops_at_an_exclusive_row_and_claims_it_alone() {
        let rows = vec![row(1, true), row(2, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, None),
            vec![row(1, true).id]
        );
    }

    /// The one rule that only holds if occupancy is extended as we go: an
    /// exclusive row claimed this tick becomes occupancy, so everything behind
    /// it waits for the next tick (run_queue.rs:1332-1341). A queue of
    /// exclusive rows therefore drains one per tick (guide lines 102-104).
    #[test]
    fn an_exclusive_row_claimed_this_tick_blocks_the_rows_behind_it() {
        let rows = vec![row(1, true), row(2, false), row(3, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, None),
            vec![row(1, true).id],
            "the exclusive row is claimed alone; the two parallel rows behind it wait"
        );
    }

    #[test]
    fn claims_parallel_rows_before_an_exclusive_row_but_not_the_exclusive_one() {
        let rows = vec![row(1, false), row(2, false), row(3, true), row(4, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, None),
            vec![row(1, false).id, row(2, false).id]
        );
    }

    #[test]
    fn claims_nothing_while_an_exclusive_run_still_occupies_the_platform() {
        let rows = vec![row(1, false), row(2, false)];
        assert!(plan_dispatch_batch(Occupancy::Exclusive, &rows, None).is_empty());
    }

    #[test]
    fn claims_parallel_rows_onto_a_platform_busy_with_parallel_runs() {
        let rows = vec![row(1, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Parallel { holders: 1 }, &rows, None),
            vec![row(1, false).id]
        );
    }

    /// A queued exclusive head must not be overtaken by the row behind it —
    /// the drain half of "the queue is strictly FIFO" (run_queue.rs:1364-1372).
    #[test]
    fn holds_an_exclusive_head_while_any_run_is_still_active() {
        let rows = vec![row(1, true), row(2, false)];
        assert!(
            plan_dispatch_batch(Occupancy::Parallel { holders: 1 }, &rows, None).is_empty(),
            "a queued exclusive head must not be overtaken by the row behind it"
        );
    }

    #[test]
    fn respects_the_global_concurrency_cap() {
        let rows = vec![row(1, false), row(2, false), row(3, false)];
        // 4 active cluster-wide, cap 6 -> room for exactly 2 more.
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, Some((4, 6))),
            vec![row(1, false).id, row(2, false).id]
        );
    }

    #[test]
    fn a_zero_cap_means_unlimited_and_a_reached_cap_claims_nothing() {
        let rows = vec![row(1, false), row(2, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, Some((99, 0))),
            vec![row(1, false).id, row(2, false).id]
        );
        assert!(plan_dispatch_batch(Occupancy::Free, &rows, Some((6, 6))).is_empty());
    }

    /// `active` is cluster-wide and already counts this platform's occupants,
    /// so occupancy must NOT be added to the cap arithmetic a second time.
    /// Without this test, a variant that adds the holder count to the budget
    /// passes every other test here (run_queue.rs:1396-1410).
    #[test]
    fn the_cap_counts_only_new_claims_not_existing_occupancy() {
        let rows = vec![row(1, false), row(2, false), row(3, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Parallel { holders: 1 }, &rows, Some((4, 6))),
            vec![row(1, false).id, row(2, false).id],
            "4 active cluster-wide against a cap of 6 leaves room for 2 more, \
             regardless of how many runs already occupy this platform"
        );
    }

    // ---------- TTL arithmetic ----------

    #[test]
    fn a_zero_ttl_disables_expiry() {
        assert_eq!(expiry_cutoff(datetime!(2026-08-13 12:00 UTC), 0), None);
        assert_eq!(ttl_expires_at(datetime!(2026-08-13 10:00 UTC), 0), None);
    }

    #[test]
    fn a_positive_ttl_yields_a_cutoff_in_the_past_and_a_deadline_in_the_future() {
        assert_eq!(
            expiry_cutoff(datetime!(2026-08-13 12:00 UTC), 7200),
            Some(datetime!(2026-08-13 10:00 UTC))
        );
        assert_eq!(
            ttl_expires_at(datetime!(2026-08-13 10:00 UTC), 7200),
            Some(datetime!(2026-08-13 12:00 UTC))
        );
    }

    /// An absurd TTL must disable expiry rather than panic. Failing open is
    /// the safe direction: the alternative is a panicking dispatcher tick,
    /// and a row that never expires is merely stale, not destructive
    /// (run_queue.rs:1002-1015).
    #[test]
    fn an_absurd_ttl_disables_expiry_rather_than_panicking() {
        let now = datetime!(2026-08-13 12:00 UTC);
        assert_eq!(expiry_cutoff(now, u64::MAX), None);
        assert_eq!(ttl_expires_at(now, u64::MAX), None);
        // A TTL that fits in i64 but not in the date range: the case the
        // checked arithmetic exists to catch. Without this the u64::MAX case
        // short-circuits earlier and the second guard is never exercised.
        assert_eq!(expiry_cutoff(now, 1_000_000_000_000_000), None);
        assert_eq!(ttl_expires_at(now, 1_000_000_000_000_000), None);
    }

    // ---------- describe_blocker ----------

    #[test]
    fn a_row_behind_others_is_blocked_by_the_rows_ahead() {
        assert_eq!(describe_blocker(4, &[]), "waiting for 3 queued runs ahead of it");
        assert_eq!(describe_blocker(2, &[]), "waiting for 1 queued run ahead of it");
    }

    #[test]
    fn the_head_names_the_single_run_holding_the_platform() {
        let holders = vec![Some("upgrade-7".to_string())];
        assert_eq!(describe_blocker(1, &holders), "waiting for run upgrade-7");
    }

    #[test]
    fn the_head_lists_every_run_holding_the_platform() {
        let holders = vec![Some("smoke-1".to_string()), Some("smoke-2".to_string())];
        assert_eq!(
            describe_blocker(1, &holders),
            "waiting for 2 active runs (smoke-1, smoke-2)"
        );
        // A claim mid-build has no name yet; the placeholder must not be
        // mistakable for a run name. This arm is otherwise uncovered.
        let mixed = vec![Some("smoke-1".to_string()), None];
        assert_eq!(
            describe_blocker(1, &mixed),
            "waiting for 2 active runs (smoke-1, (not yet started))"
        );
    }

    #[test]
    fn a_claim_without_a_name_yet_is_described_as_starting() {
        assert_eq!(
            describe_blocker(1, &[None]),
            "waiting for a run that is still starting"
        );
    }

    /// The honest answer when occupancy is not represented by a claim row.
    /// The text asserts nothing about the platform — an earlier legacy version
    /// said "waiting for the platform to be free", which is false in the
    /// global-cap case — and above all it never invents a run name
    /// (run_queue.rs:1074-1084).
    #[test]
    fn a_head_with_no_claim_names_no_run() {
        assert_eq!(describe_blocker(1, &[]), "waiting for its turn");
        // Positions at or below zero are unreachable from the listing but must
        // fall into the head branch rather than underflow.
        assert_eq!(describe_blocker(0, &[]), "waiting for its turn");
    }

    // ---------- assign_positions ----------

    /// Positions are 1-based, per platform, oldest queued row first, and only
    /// `queued` rows get one (run_queue.rs:485-507).
    #[test]
    fn positions_are_per_platform_and_oldest_first() {
        let plat_a = uuid::Uuid::from_u128(0xA);
        let plat_b = uuid::Uuid::from_u128(0xB);
        let rows = vec![
            PositionInput { id: row(3, false).id, platform_id: plat_a, enqueued_at: datetime!(2026-08-13 12:00 UTC), queued: true },
            PositionInput { id: row(1, false).id, platform_id: plat_a, enqueued_at: datetime!(2026-08-13 10:00 UTC), queued: true },
            PositionInput { id: row(2, false).id, platform_id: plat_b, enqueued_at: datetime!(2026-08-13 11:00 UTC), queued: true },
            PositionInput { id: row(4, false).id, platform_id: plat_a, enqueued_at: datetime!(2026-08-13 09:00 UTC), queued: false },
        ];
        let positions = assign_positions(&rows);
        assert_eq!(positions.get(&row(1, false).id), Some(&1));
        assert_eq!(positions.get(&row(3, false).id), Some(&2));
        assert_eq!(positions.get(&row(2, false).id), Some(&1), "platform B numbers independently");
        assert_eq!(
            positions.get(&row(4, false).id),
            None,
            "a non-queued row has no position"
        );
    }

    /// Ties on enqueued_at break on id, matching the FIFO ORDER BY
    /// (run_queue.rs:248) so positions and drain order cannot disagree.
    #[test]
    fn ties_on_enqueue_time_break_on_id() {
        let plat = uuid::Uuid::from_u128(0xA);
        let same = datetime!(2026-08-13 10:00 UTC);
        let rows = vec![
            PositionInput { id: row(2, false).id, platform_id: plat, enqueued_at: same, queued: true },
            PositionInput { id: row(1, false).id, platform_id: plat, enqueued_at: same, queued: true },
        ];
        let positions = assign_positions(&rows);
        assert_eq!(positions.get(&row(1, false).id), Some(&1));
        assert_eq!(positions.get(&row(2, false).id), Some(&2));
    }
}
```

- [ ] **Step 4: Run to confirm failure.**

```bash
cargo test -p qa-runs queue
```

Expected: compile failure — `cannot find type `Occupancy` in this scope` plus one error per missing item.

- [ ] **Step 5: Implement.** Full module:

```rust
//! Per-platform run queue: admission policy and FIFO drain planning.
//!
//! Ported from `manager/src/services/run_queue.rs`. Frozen semantics — the
//! governing document is `../testrunner/docs/guides/exclusive-runs-and-the-queue.md`.
//!
//! Pure by design, for the reason the source system gives
//! (`run_queue.rs:4-7`): the rules are the part worth testing, and keeping
//! them free of I/O is the only way to test them without a database.
//!
//! # One deliberate adaptation
//!
//! The source system derives occupancy by unioning two independent views of
//! the same fact — live Argo workflows and unreleased DB claims — and
//! de-duplicating them by key (`run_queue.rs:682-712`, `merge_occupancy`).
//! That merge exists only because Argo and the database were two separate
//! records of "what holds this platform". Here there is one record: the
//! `qa-environments` platform lease, whose `acquire`/`release` are the
//! authoritative compare-and-swap (`qa-environments/src/domain/service/leases.rs`).
//! So [`Occupancy`] is derived from `LeaseState` and `merge_occupancy` has no
//! counterpart. Two consequences worth stating:
//!
//! * The planners below are **advisory**. The lease CAS is the source of
//!   truth, so a row the planner selects may still lose its `acquire` and stay
//!   queued for the next tick. That is safe in the only direction that
//!   matters: the lease can refuse a dispatch the planner allowed, but it can
//!   never permit one the planner refused.
//! * The source system's fail-safe — an unreadable Argo makes the platform
//!   read as exclusively held so nothing dispatches blind
//!   (`run_dispatcher.rs:237-252`) — ports as: a failing lease read is treated
//!   as [`Occupancy::Exclusive`]. See `service::admission`.

use std::collections::HashMap;

use qa_environments_sdk::LeaseState;
use time::OffsetDateTime;
use uuid::Uuid;

/// What currently holds a platform, as far as an admission decision cares.
///
/// Three states rather than a holder list because that is all the rule reads:
/// whether anything holds it, and whether that hold is exclusive. `holders` is
/// carried on the parallel arm only so the dispatcher can log it and so a
/// future rule that *counts* occupancy has the number available without a
/// second lease read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occupancy {
    Free,
    Parallel { holders: usize },
    Exclusive,
}

impl Occupancy {
    /// Project a lease state onto the admission rule's view.
    ///
    /// An empty `HeldParallel` holder list reads [`Occupancy::Free`]: a lease
    /// row that exists but holds nothing must not block an incoming exclusive
    /// run. (`qa-environments` releases to `Free` rather than to an empty
    /// parallel hold, so this arm is defensive — but a defensive arm that
    /// fails *open* here would be the wrong direction, and one that blocks
    /// forever is a wedged platform.)
    #[must_use]
    pub fn from_lease(state: &LeaseState) -> Self {
        match state {
            LeaseState::Free => Self::Free,
            LeaseState::HeldParallel { holders } if holders.is_empty() => Self::Free,
            LeaseState::HeldParallel { holders } => Self::Parallel { holders: holders.len() },
            LeaseState::HeldExclusive { .. } => Self::Exclusive,
        }
    }
}

/// The single definition of the platform exclusivity rule: may a run with this
/// exclusivity flag join a platform in this state?
///
/// Deliberately shared by the admission path and the dispatcher, exactly as in
/// the source system (`manager/src/services/run_queue.rs:14-24`). If the two
/// ever disagreed the queue would drift — a weaker dispatcher rule would start
/// a run alongside an exclusive one, a stronger one would stop the queue
/// draining.
#[must_use]
pub fn platform_admits(occupancy: Occupancy, exclusive: bool) -> bool {
    match occupancy {
        // An exclusive occupant owns the platform: nothing joins it.
        Occupancy::Exclusive => false,
        // An incoming exclusive run needs the platform to itself.
        Occupancy::Parallel { .. } => !exclusive,
        Occupancy::Free => true,
    }
}

/// Whether an incoming launch may start now or must queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionDecision {
    Dispatch,
    Queue,
}

/// Decide whether an incoming launch may start now or must queue.
///
/// Note what this function does **not** take: the global concurrency cap.
/// Exceeding `max_concurrent_runs` is a 429 at admission — today's behaviour —
/// and must not silently become a queued row
/// (`manager/src/services/run_queue.rs:30-40`, with a legacy test asserting
/// exactly that at `:1281-1286`). The caller checks the cap before calling.
#[must_use]
pub fn decide_admission(
    occupancy: Occupancy,
    queued_depth: usize,
    incoming_exclusive: bool,
) -> AdmissionDecision {
    // Strict FIFO, and this check comes FIRST on purpose: once anything is
    // queued for this platform, later arrivals queue behind it whatever their
    // flag. Without it a stream of parallel runs starves a queued exclusive
    // row forever (`run_queue.rs:42-48`; guide lines 105-107).
    if queued_depth > 0 {
        return AdmissionDecision::Queue;
    }
    if !platform_admits(occupancy, incoming_exclusive) {
        return AdmissionDecision::Queue;
    }
    AdmissionDecision::Dispatch
}

/// A queued row, reduced to what drain planning reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueuedRow {
    pub id: Uuid,
    pub exclusive: bool,
}

/// Plan which queued rows a single dispatcher tick may claim, in FIFO order.
///
/// `fifo` must be ordered oldest-first. `cap` is `(active, max)` where
/// `max == 0` disables the limit; `active` must be the cluster's *committed*
/// count **plus** anything already claimed earlier in the same tick on other
/// platforms — the caller threads that budget across platforms, because
/// `claimed.len()` here counts this platform only. A caller that passes the
/// same `cap` to every platform overshoots `max` by
/// `(platforms with a queue - 1) x (max - active)`
/// (`manager/src/services/run_queue.rs:60-69`, and the threading at
/// `run_dispatcher.rs:406-414`).
#[must_use]
pub fn plan_dispatch_batch(
    occupancy: Occupancy,
    fifo: &[QueuedRow],
    cap: Option<(u32, u32)>,
) -> Vec<Uuid> {
    let mut occupancy = occupancy;
    let mut claimed = Vec::new();

    for row in fifo {
        // Stop rather than skip ahead: a row behind a blocked one must not
        // overtake it, or a queued exclusive row starves
        // (`run_queue.rs:78-82`).
        if !platform_admits(occupancy, row.exclusive) {
            break;
        }
        if let Some((active, max)) = cap {
            if max > 0 && active as usize + claimed.len() >= max as usize {
                break;
            }
        }

        claimed.push(row.id);
        // Fold the claim into occupancy so the rules apply to rows claimed
        // earlier in the same tick. This is what makes a queue of exclusive
        // rows drain one per tick (guide lines 102-104): an exclusive claim
        // becomes an exclusive occupant, and the next iteration breaks.
        // **Corrected 2026-08-13 during execution.** This was written as a
        // tuple match `match (occupancy, row.exclusive)`, which does not
        // build here: its `(_, true)` and `(Occupancy::Exclusive, false)`
        // arms have identical bodies, so `clippy::match_same_arms` fails the
        // gate under `-D warnings`. Verified by restoring the tuple form and
        // re-running. The split below is behaviorally identical — the tuple
        // form passes all 38 tests — and is lint-forced, not a style choice.
        occupancy = if row.exclusive {
            Occupancy::Exclusive
        } else {
            match occupancy {
                Occupancy::Free => Occupancy::Parallel { holders: 1 },
                Occupancy::Parallel { holders } => {
                    Occupancy::Parallel { holders: holders + 1 }
                }
                // Unreachable: platform_admits already refused above.
                Occupancy::Exclusive => Occupancy::Exclusive,
            }
        };
    }

    claimed
}

/// Normalise `queue_max_depth`: `None` when the limit is disabled (`0`).
///
/// Mirrors [`global_cap_status`] so both capacity settings read the same way
/// at their call sites (`manager/src/services/run_queue.rs:810-819`).
#[must_use]
pub fn depth_limit(max_depth: u32) -> Option<u32> {
    if max_depth == 0 { None } else { Some(max_depth) }
}

/// Whether a launch must be rejected outright because the platform's queue is
/// already at `queue_max_depth`.
///
/// Deliberately separate from [`decide_admission`]: that function answers a
/// *scheduling* question whose depth-before-occupancy ordering is a
/// load-bearing invariant, whereas this is a *capacity* answer that writes no
/// row at all. Note the composition property: a full queue implies
/// `queued_depth > 0` for any limit of 1 or more, so a launch that would have
/// started inline on an idle platform can never be rejected by this rule
/// (`manager/src/services/run_queue.rs:821-832`).
#[must_use]
pub fn queue_is_full(queued_depth: usize, limit: Option<u32>) -> bool {
    matches!(limit, Some(limit) if queued_depth >= limit as usize)
}

/// Normalise the global concurrency setting: `None` when disabled
/// (`max_concurrent_runs == 0`, the shipped default), else `(active, max)`.
#[must_use]
pub fn global_cap_status(active: u32, max: u32) -> Option<(u32, u32)> {
    if max == 0 { None } else { Some((active, max)) }
}

/// Whether the global limit currently leaves no room.
#[must_use]
pub fn cap_reached(cap: Option<(u32, u32)>) -> bool {
    matches!(cap, Some((active, max)) if active >= max)
}

/// The timestamp before which a `queued` row has outlived `queue_ttl_seconds`.
///
/// `None` means "do not expire anything": either expiry is switched off
/// (`ttl_seconds == 0`, the same convention as `max_concurrent_runs == 0`) or
/// the configured TTL is large enough to overflow the arithmetic. Both fail
/// **open** on purpose — a stale queued row is an operator annoyance, whereas
/// a panicking dispatcher tick stops the queue draining at all
/// (`manager/src/services/run_queue.rs:729-744`).
#[must_use]
pub fn expiry_cutoff(now: OffsetDateTime, ttl_seconds: u64) -> Option<OffsetDateTime> {
    ttl_window(ttl_seconds).and_then(|window| now.checked_sub(window))
}

/// When a queued row will hit its TTL, for display.
///
/// The mirror image of [`expiry_cutoff`] — the same `None` cases for the same
/// fail-open reason — and the reason no client ever hard-codes 7200: the TTL
/// has exactly one definition, in config
/// (`manager/src/services/run_queue.rs:746-759`).
#[must_use]
pub fn ttl_expires_at(enqueued_at: OffsetDateTime, ttl_seconds: u64) -> Option<OffsetDateTime> {
    ttl_window(ttl_seconds).and_then(|window| enqueued_at.checked_add(window))
}

/// Shared guard for both TTL helpers: `0` disables, and an unrepresentable
/// span disables rather than panicking.
fn ttl_window(ttl_seconds: u64) -> Option<time::Duration> {
    if ttl_seconds == 0 {
        return None;
    }
    i64::try_from(ttl_seconds).ok().map(time::Duration::seconds)
}

/// What a queued row is waiting for, phrased for an operator.
///
/// Strict FIFO makes the non-head case exact: anything but the head waits on
/// the rows in front of it, whatever the platform is doing. The head waits on
/// whatever still holds the platform.
///
/// `holders` carries `None` for a claim that has not produced an execution yet
/// (still mid force-sync / bundle build), which is why it is not `&[String]`:
/// an empty string there would read as a run called "".
///
/// The deliberate gap: when the head's platform has no claim at all, occupancy
/// may still be something this gear cannot name — or a lease read that failed,
/// which reads as busy by design. The answer then names no run rather than
/// fabricating one. `position` is 1-based; 0 and below fall into the head
/// branch and are unreachable from the listing
/// (`manager/src/services/run_queue.rs:761-808`).
#[must_use]
pub fn describe_blocker(position: u32, holders: &[Option<String>]) -> String {
    if position > 1 {
        let ahead = position - 1;
        return format!(
            "waiting for {ahead} queued run{} ahead of it",
            if ahead == 1 { "" } else { "s" }
        );
    }

    match holders {
        [] => "waiting for its turn".to_string(),
        [Some(name)] => format!("waiting for run {name}"),
        [None] => "waiting for a run that is still starting".to_string(),
        many => {
            let names: Vec<String> = many
                .iter()
                .map(|holder| {
                    holder.clone().unwrap_or_else(|| "(not yet started)".to_string())
                })
                .collect();
            format!("waiting for {} active runs ({})", names.len(), names.join(", "))
        }
    }
}

/// One row's inputs to position numbering.
#[derive(Clone, Copy, Debug)]
pub struct PositionInput {
    pub id: Uuid,
    pub platform_id: Uuid,
    pub enqueued_at: OffsetDateTime,
    pub queued: bool,
}

/// 1-based FIFO position per platform, oldest `queued` row first.
///
/// Only `queued` rows get a position; every other state reports `None`
/// (guide line 175). Ties on `enqueued_at` break on `id`, matching the FIFO
/// `ORDER BY enqueued_at ASC, id ASC` (`run_queue.rs:248`) so positions and
/// drain order cannot disagree.
///
/// Computed over the rows the caller passes — which is the rows *that
/// request returned*. The documented consequence: a truncating limit can push
/// an older queued row out of the window, collapsing the rows behind it toward
/// position 1 and turning their `blocked_by` into the vaguer head-branch text.
/// The platform-filtered listing is the reliable one
/// (`run_queue.rs:435-453`; guide lines 179-184).
#[must_use]
pub fn assign_positions(rows: &[PositionInput]) -> HashMap<Uuid, u32> {
    let mut queued: Vec<&PositionInput> = rows.iter().filter(|row| row.queued).collect();
    queued.sort_by(|a, b| {
        a.platform_id
            .cmp(&b.platform_id)
            .then(a.enqueued_at.cmp(&b.enqueued_at))
            .then(a.id.cmp(&b.id))
    });

    let mut positions = HashMap::new();
    let mut current: Option<Uuid> = None;
    let mut position = 0i64;
    for row in queued {
        if current != Some(row.platform_id) {
            current = Some(row.platform_id);
            position = 0;
        }
        position += 1;
        positions.insert(row.id, position);
    }
    positions
}
```

Add `pub mod queue;` to `domain/mod.rs`.

- [ ] **Step 6: Run the tests.**

```bash
cargo test -p qa-runs queue
```

Expected: **38 passed; 0 failed** — Step 3 defines exactly 38 `#[test]` functions. `time::macros::datetime!` requires the `macros` feature on `time` — if it is not enabled in the workspace, the tests fail to compile with `could not find `macros` in `time``; in that case build the fixtures with `OffsetDateTime::from_unix_timestamp(...)` rather than adding a feature flag for tests alone, and keep the same instants.

- [ ] **Step 7: Full gate + commit.**

```bash
cargo build -p qa-runs && cargo clippy -p qa-runs --all-targets -- -D warnings && cargo fmt --check -p qa-runs
```

`git commit -m "feat(qa-runs): queue admission and FIFO dispatch planning ported from legacy"`

---

### Task 7: TDD core 3 — run state machine, phase derivation, crash recovery

> **`RunState::Expired` added 2026-08-13 by user decision, raised by this task.** The transition table below gives `Queued` exactly one terminal exit, `Canceled`, and nothing said what a TTL-expired run becomes — legacy never faced it, having no run row for a launch that never started. Two consequences here: add the edge **`Queued → Expired`** (and nothing else gains one — a run that has reached `Dispatching` is no longer waiting on the queue), and `Expired` joins the terminal set, so `TERMINAL_STATES`, `is_terminal`, and the no-terminal-self-transition rule all cover it. The SDK variant, its `as_str()` string `"expired"`, and the mapper pair are amended into Tasks 4 and 10.
>
> **`Dispatching → Succeeded` added by the implementer 2026-08-15 during Task 16, ratified by the user 2026-08-15.** The other four exits from `Dispatching` — `Failed`, `Error`, `Canceled`, `TimedOut` — were already legal, so a run that skipped `Running` could record every verdict **except** the good one. Reachable, not theoretical: `service::dispatch`'s `record_started` deliberately swallows a failed `Dispatching → Running` write (ported from `manager/src/services/run_dispatcher.rs:32-44`, so that a caller retry cannot double-submit), so one transient database failure leaves a live run in `Dispatching`, and its green completion was then refused and later reclaimed as `TimedOut`. `Expired` is still **not** legal from `Dispatching`, correctly. The edge and its full argument live at `domain/state_machine.rs`'s `can_transition` doc; the gate on it is the executor's own `ExecutionEvent::Finished { Succeeded }`, **not** the ingested per-test counts — `derive_terminal_state(ExecutorOutcome::Succeeded, RunResult::default(), None)` returns `Succeeded` on zero counts, pinned by `no_node_failure_and_no_results_succeeds`.
>
> **`RunResult.in_progress` stays unread by `derive_terminal_state` — user decision, at parity.** A run whose executor reports `succeeded` with `in_progress > 0` is reported `Succeeded` on incomplete evidence. Legacy's derivation never sees an in-progress case, so a rule here would be net-new behavior against `cpt-cf-qa-principle-semantics-parity`, and the case is likely unreachable (a `succeeded` executor phase means every node finished, so lingering `PENDING` results would indicate an ingest bug rather than a real state). **Recorded as a known gap, not built.** If Task 11's ingest work shows the case is reachable, that is the moment to revisit.

Three rule families, one module. The state machine itself is **net-new** — but for a narrower reason than this line originally gave, **corrected 2026-08-13 (Task 4 execution)**: it said "legacy had no `runs` table", which is false. Legacy keeps `run_results` (`PersistedRunRow`, `manager/src/services/run_history.rs:10-48`, merged with the live Workflow by `overlay_persisted_with_live` at `:216`). What legacy has no counterpart for is a **transition guard**: `run_results` is written by upserts that record whatever phase Argo last reported, with no legal-transition check anywhere, so there is no guard to port even though there is a table. **Phase derivation and crash recovery are ported verbatim** and are where the traps are.

**Files:**
- Create: `qa-runs/src/domain/state_machine.rs`
- Modify: `qa-runs/src/domain/mod.rs` (add `pub mod state_machine;`)

**Owns:** those two.

**Expected remaining errors after this task:** none.

- [ ] **Step 1: Legacy check — phase derivation (decision D2).** Read `../testrunner/manager/src/services/argo.rs:2160-2227`. Confirm all five and cite each:

  1. `derive_phase_from_result_flags`: `(has_failed || has_skipped) && matches!(base_phase, "Succeeded" | "Skipped")` ⇒ `"Failed"`, else `base_phase` unchanged — `:2186-2190`.
  2. `has_failed` is `FAILED` **or** `ERROR`; `has_skipped` is `SKIPPED` — `:2194-2199`.
  3. The rule is **idempotent**: only `Succeeded`/`Skipped` are ever downgraded, so re-deriving an already-derived phase is a no-op — `:2183-2185`.
  4. `dag_nodes_failed` exists because parsed results alone do not cover a node that died before emitting any result — a bundle it could not download, an image it could not pull, its own timeout — and the workflow phase hides such a failure whenever the node is not a leaf — `:2218-2227`.
  5. `effective_base_phase`: when the live phase is `Succeeded`/`Skipped` but the persisted phase is `Failed`/`Error`, the **persisted** one wins — `:2160-2168`.

  Rules 4 and 5 are the ones a naive port drops. Both survive the Argo removal in changed form: (4) becomes "an execution that reported a terminal failure with zero test results is `failed`, not `succeeded`", and (5) becomes "a re-derivation never upgrades a run that was already recorded failed". Implement both.

- [ ] **Step 2: Legacy check — crash recovery.** Read `run_queue.rs:349-361` and `run_dispatcher.rs:417-477`. Confirm and cite:

  1. `fail_orphaned_dispatching` has **no age predicate** and is therefore **boot-only** — never call it from a tick — `run_queue.rs:349-355` (its doc says exactly this) and `run_dispatcher.rs:423-426`.
  2. The tick's reconciler has an `orphan_timeout` **age guard**, and that guard is load-bearing: a row sits in `dispatching` with no execution reference for the whole sync + bundle-build window, which is minutes. Failing such a row early abandons a launch still in progress **and momentarily releases its claim — exactly when a second run could be admitted alongside an exclusive one** — `run_dispatcher.rs:419-426`.
  3. A claim whose execution is terminal or gone is released to `done`. This is what stops a stuck execution blocking the queue forever — `run_dispatcher.rs:446-457`.
  4. `elapsed_seconds` floors at 0, because `since` comes from the database clock and `now` from the process clock, so small negative skew must read "just now" rather than wrapping into a huge age — `run_dispatcher.rs:479-487`.

- [ ] **Step 3: Write the failing tests.**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ---------- transitions ----------

    #[test]
    fn the_happy_path_is_legal_end_to_end() {
        for (from, to) in [
            (RunState::Created, RunState::Dispatching),
            (RunState::Dispatching, RunState::Running),
            (RunState::Running, RunState::Succeeded),
        ] {
            assert!(can_transition(from, to), "{from:?} -> {to:?} must be legal");
        }
    }

    #[test]
    fn the_queued_path_is_legal_end_to_end() {
        for (from, to) in [
            (RunState::Created, RunState::Queued),
            (RunState::Queued, RunState::Dispatching),
            (RunState::Dispatching, RunState::Running),
            (RunState::Running, RunState::Failed),
        ] {
            assert!(can_transition(from, to), "{from:?} -> {to:?} must be legal");
        }
    }

    /// A dispatch that fails before an execution exists goes straight to a
    /// terminal state without ever running — the legacy `mark_failed` on a
    /// failed submit (manager/src/services/run_dispatcher.rs:47-58).
    #[test]
    fn dispatching_may_fail_without_running() {
        assert!(can_transition(RunState::Dispatching, RunState::Error));
        assert!(can_transition(RunState::Dispatching, RunState::Failed));
    }

    /// Cancel is reachable from every non-terminal state: a queued row is
    /// dropped, an executing run is cancelled through the executor
    /// (cpt-cf-qa-fr-runs-cancel-rerun).
    #[test]
    fn cancel_is_reachable_from_every_non_terminal_state() {
        for from in [
            RunState::Created,
            RunState::Queued,
            RunState::Dispatching,
            RunState::Running,
        ] {
            assert!(
                can_transition(from, RunState::Canceled),
                "{from:?} must be cancellable"
            );
        }
    }

    /// Control-plane timeout enforcement only applies once something is
    /// executing or about to (cpt-cf-qa-fr-runs-timeout).
    #[test]
    fn timeout_is_reachable_only_from_dispatching_and_running() {
        assert!(can_transition(RunState::Dispatching, RunState::TimedOut));
        assert!(can_transition(RunState::Running, RunState::TimedOut));
        assert!(!can_transition(RunState::Created, RunState::TimedOut));
        assert!(!can_transition(RunState::Queued, RunState::TimedOut));
    }

    #[test]
    fn a_queued_run_cannot_start_running_without_being_dispatched() {
        assert!(!can_transition(RunState::Queued, RunState::Running));
    }

    #[test]
    fn a_run_cannot_go_backwards() {
        assert!(!can_transition(RunState::Running, RunState::Queued));
        assert!(!can_transition(RunState::Dispatching, RunState::Created));
        assert!(!can_transition(RunState::Queued, RunState::Created));
    }

    /// Terminal means terminal — including into itself, so a duplicate
    /// completion event is rejected by the guard rather than silently
    /// rewriting timestamps.
    #[test]
    fn no_transition_leaves_a_terminal_state() {
        for from in TERMINAL_STATES {
            for to in [
                RunState::Created,
                RunState::Queued,
                RunState::Dispatching,
                RunState::Running,
                RunState::Succeeded,
                RunState::Failed,
                RunState::Canceled,
                RunState::TimedOut,
                RunState::Error,
            ] {
                assert!(
                    !can_transition(from, to),
                    "{from:?} is terminal but allowed a transition to {to:?}"
                );
            }
        }
    }

    #[test]
    fn is_terminal_agrees_with_the_terminal_set() {
        for state in TERMINAL_STATES {
            assert!(is_terminal(state), "{state:?} must be terminal");
        }
        for state in [
            RunState::Created,
            RunState::Queued,
            RunState::Dispatching,
            RunState::Running,
        ] {
            assert!(!is_terminal(state), "{state:?} must not be terminal");
        }
    }

    // ---------- phase derivation (decision D2) ----------

    fn counts(passed: usize, failed: usize, skipped: usize) -> ResultCounts {
        ResultCounts { passed, failed, skipped }
    }

    #[test]
    fn a_clean_run_succeeds() {
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, counts(10, 0, 0), None),
            RunState::Succeeded
        );
    }

    #[test]
    fn a_failed_test_downgrades_a_successful_execution() {
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, counts(9, 1, 0), None),
            RunState::Failed
        );
    }

    /// Decision D2, ported as-is: any SKIPPED test downgrades a Succeeded run
    /// to Failed, because "a skipped test means the run didn't fully execute"
    /// (manager/src/services/argo.rs:2180-2181). This includes the case where
    /// the skip came from the run's own open-bug skip-list — that interaction
    /// is inherited parity, not a bug to fix here.
    #[test]
    fn a_skipped_test_downgrades_a_successful_execution() {
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, counts(9, 0, 1), None),
            RunState::Failed,
            "any skipped test makes the run Failed (decision D2, argo.rs:2186)"
        );
    }

    /// Only Succeeded is ever downgraded, so re-deriving an already-derived
    /// answer is a no-op (argo.rs:2183-2185). Asserted by feeding the rule its
    /// own output.
    #[test]
    fn derivation_is_idempotent() {
        let once = derive_terminal_state(ExecutorOutcome::Succeeded, counts(0, 1, 0), None);
        let twice = derive_terminal_state(ExecutorOutcome::Failed, counts(0, 1, 0), None);
        assert_eq!(once, RunState::Failed);
        assert_eq!(twice, RunState::Failed);
    }

    /// A non-success outcome is never upgraded by clean results.
    #[test]
    fn clean_results_never_upgrade_a_failed_execution() {
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Failed, counts(10, 0, 0), None),
            RunState::Failed
        );
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Errored, counts(10, 0, 0), None),
            RunState::Error
        );
    }

    #[test]
    fn cancel_and_timeout_outcomes_map_straight_through() {
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Canceled, counts(3, 0, 0), None),
            RunState::Canceled
        );
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::TimedOut, counts(3, 0, 0), None),
            RunState::TimedOut
        );
    }

    /// Legacy rule 4: parsed results alone do not cover a node that died
    /// before emitting any result — a bundle it could not download, an image
    /// it could not pull. Zero results plus a reported node failure is Failed,
    /// not Succeeded (argo.rs:2218-2227).
    #[test]
    fn a_node_failure_with_no_results_at_all_is_a_failure() {
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, counts(0, 0, 0), Some(true)),
            RunState::Failed,
            "an execution node that died before emitting any result must not read as passing"
        );
    }

    /// And the converse: no node failure and no results is a successful run of
    /// nothing, which is what an all-filtered-out tag selection produces.
    #[test]
    fn no_node_failure_and_no_results_succeeds() {
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, counts(0, 0, 0), Some(false)),
            RunState::Succeeded
        );
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, counts(0, 0, 0), None),
            RunState::Succeeded
        );
    }

    /// Legacy rule 5 (effective_base_phase, argo.rs:2160-2168): a run already
    /// recorded as failed is never upgraded by a later re-derivation that sees
    /// a clean executor outcome.
    #[test]
    fn a_recorded_failure_is_never_upgraded_by_re_derivation() {
        assert_eq!(
            reconcile_recorded_state(RunState::Failed, RunState::Succeeded),
            RunState::Failed
        );
        assert_eq!(
            reconcile_recorded_state(RunState::Error, RunState::Succeeded),
            RunState::Error
        );
    }

    #[test]
    fn re_derivation_may_downgrade_a_recorded_success() {
        assert_eq!(
            reconcile_recorded_state(RunState::Succeeded, RunState::Failed),
            RunState::Failed
        );
    }

    #[test]
    fn re_derivation_leaves_agreeing_states_alone() {
        assert_eq!(
            reconcile_recorded_state(RunState::Succeeded, RunState::Succeeded),
            RunState::Succeeded
        );
        assert_eq!(
            reconcile_recorded_state(RunState::Canceled, RunState::Succeeded),
            RunState::Canceled,
            "a cancel is a recorded fact, not a phase to re-derive"
        );
    }

    // ---------- crash recovery ----------

    /// The age guard is load-bearing: a row sits in `dispatching` with no
    /// execution reference for the whole sync + bundle-build window, which is
    /// minutes. Failing it early abandons a live launch AND momentarily
    /// releases its claim — exactly when a second run could be admitted
    /// alongside an exclusive one (run_dispatcher.rs:419-426).
    #[test]
    fn a_young_claim_with_no_execution_is_left_alone() {
        assert_eq!(
            reconcile_claim(ClaimObservation {
                has_execution: false,
                execution_active: false,
                age_seconds: 30,
            }, 600),
            ClaimAction::Keep
        );
    }

    #[test]
    fn a_claim_with_no_execution_past_the_orphan_timeout_is_failed() {
        assert_eq!(
            reconcile_claim(ClaimObservation {
                has_execution: false,
                execution_active: false,
                age_seconds: 600,
            }, 600),
            ClaimAction::FailOrphaned
        );
    }

    #[test]
    fn a_claim_whose_execution_is_still_active_is_left_alone() {
        assert_eq!(
            reconcile_claim(ClaimObservation {
                has_execution: true,
                execution_active: true,
                age_seconds: 100_000,
            }, 600),
            ClaimAction::Keep,
            "age never fails a claim whose execution is demonstrably alive"
        );
    }

    /// Terminal or vanished — either way the platform is free. This is what
    /// stops a stuck execution blocking the queue forever
    /// (run_dispatcher.rs:446-457).
    #[test]
    fn a_claim_whose_execution_is_gone_is_released() {
        assert_eq!(
            reconcile_claim(ClaimObservation {
                has_execution: true,
                execution_active: false,
                age_seconds: 10,
            }, 600),
            ClaimAction::Release
        );
    }

    /// Boot recovery has NO age guard and is a different rule from the tick's:
    /// with the process just restarted, a `dispatching` row with no execution
    /// reference is definitively gone (run_queue.rs:349-355). Calling this from
    /// a tick would fail every launch mid-build.
    #[test]
    fn boot_recovery_fails_every_execution_less_dispatching_row_regardless_of_age() {
        for age in [0, 1, 30, 100_000] {
            assert_eq!(
                boot_recovery_action(ClaimObservation {
                    has_execution: false,
                    execution_active: false,
                    age_seconds: age,
                }),
                ClaimAction::FailOrphaned,
                "boot recovery must not consult age (age = {age})"
            );
        }
    }

    #[test]
    fn boot_recovery_leaves_a_row_that_has_an_execution_to_the_tick() {
        assert_eq!(
            boot_recovery_action(ClaimObservation {
                has_execution: true,
                execution_active: false,
                age_seconds: 0,
            }),
            ClaimAction::Keep,
            "a row with an execution reference is the tick reconciler's business, \
             because only it knows whether that execution is still alive"
        );
    }

    // ---------- elapsed_seconds ----------

    #[test]
    fn elapsed_seconds_measures_the_gap() {
        let now = time::macros::datetime!(2026-08-13 12:00 UTC);
        let since = time::macros::datetime!(2026-08-13 10:30 UTC);
        assert_eq!(elapsed_seconds(now, since), 5400);
    }

    /// `since` comes from the database clock and `now` from this process's, so
    /// a small negative skew is normal and must read "just now" rather than
    /// wrapping into a huge age in the log (run_dispatcher.rs:479-487).
    #[test]
    fn elapsed_seconds_floors_clock_skew_at_zero() {
        let now = time::macros::datetime!(2026-08-13 12:00 UTC);
        let future = time::macros::datetime!(2026-08-13 12:00:05 UTC);
        assert_eq!(elapsed_seconds(now, future), 0);
    }
}
```

- [ ] **Step 4: Run to confirm failure.**

```bash
cargo test -p qa-runs state_machine
```

Expected: compile failure — `cannot find function `can_transition``, `cannot find type `ExecutorOutcome``, and one error per missing item.

- [ ] **Step 5: Implement.**

```rust
//! Run state machine, terminal-phase derivation, and crash-recovery rules.
//!
//! Three rule families with different provenance, kept in one module because
//! they are the only three that decide what state a run is in:
//!
//! * **Transitions** are net-new — but not because the source system lacked a
//!   run table. It has one: `run_results`, read as `PersistedRunRow`
//!   (`manager/src/services/run_history.rs:10-48`) and merged with the live
//!   Argo Workflow by `overlay_persisted_with_live` (`:216`), because the
//!   Workflow is garbage-collected after its TTL while re-runs happen much
//!   later (`manager/migrations/001_initial.sql:306-310`). What it lacks is a
//!   **transition guard**: that row is written by upserts recording whatever
//!   phase Argo last reported, with no legal-transition check anywhere, so
//!   there is nothing to port even though there is a table.
//!   `cpt-cf-qa-principle-db-first-state` makes the row authoritative here,
//!   which means it needs one.
//! * **Phase derivation** is ported verbatim from
//!   `manager/src/services/argo.rs:2160-2227`, including the rule that any
//!   skipped test downgrades a successful run (plan decision D2, recorded in
//!   DESIGN §3.1).
//! * **Crash recovery** is ported from `manager/src/services/run_queue.rs:349-361`
//!   and `manager/src/services/run_dispatcher.rs:417-487`, including the
//!   boot-only / tick-only split that is easy to collapse and dangerous to.
//!
//! Pure: every function takes already-observed facts. The dispatcher does the
//! observing (`service::dispatch`).

use qa_runs_sdk::RunState;
use time::OffsetDateTime;

/// The five states from which no transition is legal.
///
/// A named constant so [`is_terminal`] and the transition guard cannot drift
/// apart, and so the exhaustiveness test can iterate it.
pub const TERMINAL_STATES: [RunState; 5] = [
    RunState::Succeeded,
    RunState::Failed,
    RunState::Canceled,
    RunState::TimedOut,
    RunState::Error,
];

/// Whether a run has reached a state it can never leave.
#[must_use]
pub fn is_terminal(state: RunState) -> bool {
    matches!(
        state,
        RunState::Succeeded
            | RunState::Failed
            | RunState::Canceled
            | RunState::TimedOut
            | RunState::Error
    )
}

/// Whether `from -> to` is a legal transition.
///
/// Exhaustive by `match` on purpose: a new [`RunState`] variant must fail
/// compilation here rather than silently inherit a permissive default. The
/// same reason the gear's error mapping has no catch-all arm.
///
/// Two guards worth naming:
/// * **Nothing leaves a terminal state, including into itself.** A duplicate
///   completion event is therefore rejected by this guard rather than silently
///   rewriting `finished_at`.
/// * **`Queued -> Running` is illegal.** A queued row must pass through
///   `Dispatching`, because that is the state that holds the platform claim
///   while the sync and bundle build run (`run_queue.rs:159-172` inserts an
///   admitted launch directly as `dispatching`, "which is what makes the row a
///   claim before the caller submits").
#[must_use]
pub fn can_transition(from: RunState, to: RunState) -> bool {
    if is_terminal(from) {
        return false;
    }
    match (from, to) {
        // Admission decided: start now, or wait.
        (RunState::Created, RunState::Queued | RunState::Dispatching) => true,
        // The dispatcher claimed a queued row.
        (RunState::Queued, RunState::Dispatching) => true,
        // The executor accepted the run.
        (RunState::Dispatching, RunState::Running) => true,
        // A dispatch can fail before any execution exists — the submit itself
        // failed, or a restart interrupted it
        // (`manager/src/services/run_dispatcher.rs:47-58`).
        (RunState::Dispatching, RunState::Failed | RunState::Error) => true,
        // Execution finished, one way or another.
        (RunState::Running, RunState::Succeeded | RunState::Failed | RunState::Error) => true,
        // Cancellation reaches every non-terminal state
        // (`cpt-cf-qa-fr-runs-cancel-rerun`).
        (
            RunState::Created | RunState::Queued | RunState::Dispatching | RunState::Running,
            RunState::Canceled,
        ) => true,
        // Control-plane timeout applies only once something is executing or
        // about to (`cpt-cf-qa-fr-runs-timeout`). A queued run is bounded by
        // `queue_ttl_seconds` instead, which expires the queue row rather than
        // the run.
        (RunState::Dispatching | RunState::Running, RunState::TimedOut) => true,
        _ => false,
    }
}

/// How the executor says an execution ended.
///
/// The `RunExecutor` port's terminal vocabulary, deliberately narrower than
/// [`RunState`]: the executor reports what *it* observed, and this module
/// decides what the *run* is, which is not the same answer (that gap is the
/// whole point of `derive_terminal_state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutorOutcome {
    Succeeded,
    Failed,
    Errored,
    Canceled,
    TimedOut,
}

/// Per-status result counts for one run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResultCounts {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// The run's terminal state, from the executor's outcome plus the ingested
/// results.
///
/// Ported from `manager/src/services/argo.rs:2170-2227`. **A green executor
/// phase is not sufficient evidence that a run passed** — the results get the
/// final word — and there are two independent reasons:
///
/// 1. **Results say otherwise.** Any `failed` or `error` result, **and any
///    `skipped` result**, downgrades a `Succeeded` outcome to `Failed`
///    (`argo.rs:2186`). The skip arm is deliberate: "a skipped test means the
///    run didn't fully execute, so it must never read as passing either"
///    (`:2180-2181`). Plan decision D2 ports it as-is, including its
///    consequence — the open-bug skip-list makes the runner skip tests, so a
///    run that skipped only known-broken tests reports `failed`.
/// 2. **A node died before reporting anything.** `node_failure` covers what
///    the results cannot: an execution node that could not download its
///    bundle, could not pull its image, or hit its own timeout leaves no
///    failed rows behind, and in the source system the workflow phase hid that
///    failure whenever the node was not a leaf (`argo.rs:2218-2227`,
///    `dag_nodes_failed`). `None` means the executor does not report per-node
///    detail — the mock does not — and is treated as "no known node failure".
///
/// The rule is idempotent: only `Succeeded` is ever downgraded, so feeding
/// this function its own output changes nothing (`argo.rs:2183-2185`).
#[must_use]
pub fn derive_terminal_state(
    outcome: ExecutorOutcome,
    counts: ResultCounts,
    node_failure: Option<bool>,
) -> RunState {
    let base = match outcome {
        ExecutorOutcome::Succeeded => RunState::Succeeded,
        ExecutorOutcome::Failed => RunState::Failed,
        ExecutorOutcome::Errored => RunState::Error,
        ExecutorOutcome::Canceled => RunState::Canceled,
        ExecutorOutcome::TimedOut => RunState::TimedOut,
    };
    if base != RunState::Succeeded {
        return base;
    }
    let results_say_no = counts.failed > 0 || counts.skipped > 0;
    if results_say_no || node_failure == Some(true) {
        return RunState::Failed;
    }
    RunState::Succeeded
}

/// Reconcile a freshly derived state against the one already recorded.
///
/// Ported from `effective_base_phase` (`manager/src/services/argo.rs:2160-2168`):
/// a run already recorded as failed is **never upgraded** by a later
/// re-derivation that happens to see a clean outcome. Two shapes of that
/// matter here — a late-arriving result event, and a re-read after the
/// executor has forgotten the execution.
///
/// Extended past legacy in one respect, deliberately: `Canceled` and
/// `TimedOut` are recorded *decisions* rather than derived phases, so they also
/// win over any re-derivation. Legacy had no equivalent (an Argo terminate
/// simply produced a `Failed` workflow), so this is a divergence the new
/// state vocabulary forces — recorded here rather than left implicit.
#[must_use]
pub fn reconcile_recorded_state(recorded: RunState, derived: RunState) -> RunState {
    match recorded {
        RunState::Failed
        | RunState::Error
        | RunState::Canceled
        | RunState::TimedOut => recorded,
        _ => derived,
    }
}

/// What the dispatcher observed about one claim row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimObservation {
    /// Whether the row carries an `execution_ref` yet.
    pub has_execution: bool,
    /// Whether the executor still lists that execution as active. Meaningless
    /// when `has_execution` is false.
    pub execution_active: bool,
    /// Seconds since the row was claimed (or enqueued, if it was never
    /// dispatched) — see [`elapsed_seconds`].
    pub age_seconds: u64,
}

/// What to do with a claim row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimAction {
    /// Leave it alone.
    Keep,
    /// Its execution is terminal or gone: release the claim so the platform
    /// frees and the queue drains.
    Release,
    /// It was left mid-dispatch and will never produce an execution: fail it.
    FailOrphaned,
}

/// Per-tick claim reconciliation.
///
/// Ported from `manager/src/services/run_dispatcher.rs:427-477`. The
/// `orphan_timeout` age guard is **load-bearing**, and this is the comment to
/// read before ever removing it: a row sits in `dispatching` with no execution
/// reference for the whole force-sync + bundle-build window, which is minutes.
/// Failing such a row early abandons a launch that is still in progress — and
/// momentarily releases its claim, which is exactly when a second run could be
/// admitted alongside an exclusive one (`run_dispatcher.rs:419-426`).
///
/// Note the ordering: an execution that is demonstrably alive is kept
/// regardless of age. Age only decides the fate of a row that has *no*
/// execution to point at.
#[must_use]
pub fn reconcile_claim(observation: ClaimObservation, orphan_timeout_seconds: u64) -> ClaimAction {
    if observation.has_execution {
        if observation.execution_active {
            ClaimAction::Keep
        } else {
            // Terminal or vanished — either way the platform is free. This is
            // what stops a stuck execution blocking the queue forever
            // (`run_dispatcher.rs:448-456`).
            ClaimAction::Release
        }
    } else if observation.age_seconds >= orphan_timeout_seconds {
        ClaimAction::FailOrphaned
    } else {
        ClaimAction::Keep
    }
}

/// Boot-time claim recovery — a **different rule** from [`reconcile_claim`],
/// and the two must never be collapsed.
///
/// Ported from `RunQueueService::fail_orphaned_dispatching`
/// (`manager/src/services/run_queue.rs:349-361`), whose own doc records why it
/// has no age predicate and is therefore boot-only: with the process just
/// restarted, a `dispatching` row with no execution reference is definitively
/// gone — the task that was building it died with the previous process. Calling
/// this from a tick would fail every launch that is merely mid-build.
///
/// A row that *does* carry an execution reference is left to the tick
/// reconciler, because only it knows whether that execution is still alive.
#[must_use]
pub fn boot_recovery_action(observation: ClaimObservation) -> ClaimAction {
    if observation.has_execution {
        ClaimAction::Keep
    } else {
        ClaimAction::FailOrphaned
    }
}

/// Seconds elapsed since `since`, floored at 0.
///
/// Floored because `since` comes from the database clock while `now` comes from
/// this process's: small negative skew must read "just now", not wrap into a
/// huge age. Shared by the claim reconciler and the TTL sweep so the two report
/// ages the same way (`manager/src/services/run_dispatcher.rs:479-487`).
#[must_use]
pub fn elapsed_seconds(now: OffsetDateTime, since: OffsetDateTime) -> u64 {
    let seconds = (now - since).whole_seconds();
    u64::try_from(seconds.max(0)).unwrap_or(0)
}
```

Add `pub mod state_machine;` to `domain/mod.rs`.

- [ ] **Step 6: Run the tests.**

```bash
cargo test -p qa-runs state_machine
```

Expected: **28 passed; 0 failed** — Step 3 defines exactly 28 `#[test]` functions. If `time::macros::datetime!` is unavailable (see Task 6 Step 6), build the two `elapsed_seconds` fixtures with `OffsetDateTime::from_unix_timestamp` keeping the same 5400-second gap and 5-second skew.

- [ ] **Step 7: Full gate + commit.**

```bash
cargo build -p qa-runs && cargo clippy -p qa-runs --all-targets -- -D warnings && cargo fmt --check -p qa-runs
```

`git commit -m "feat(qa-runs): run state machine, phase derivation, and crash-recovery rules"`

---

### Task 8: Pure helpers — parameter validation, run naming, environment assembly

Three more pure modules, batched because each is small and they have no interdependencies. (Batching trivial adjacent work into one dispatch is proportionate; running two implementers concurrently is not.) Three commits, one per module.

**Files:**
- Create: `qa-runs/src/domain/params.rs`, `qa-runs/src/domain/naming.rs`, `qa-runs/src/domain/env_assembly.rs`
- Modify: `qa-runs/src/domain/mod.rs` (three `pub mod` lines), `qa-runs/src/domain/error.rs` (add the variants named below)

**Owns:** those five files.

**Expected remaining errors after this task:** none.

#### 8a — Parameter validation (`params.rs`)

- [ ] **Step 1: Legacy check.** Read `../testrunner/manager/src/routes/settings.rs:14-195`. Confirm and cite:
  1. The reserved list is exactly the eleven names at `:15-27`, matched **case-insensitively** (`:80-83`).
  2. Name charset: first char ASCII alphabetic or `_`, rest ASCII alphanumeric or `_` (`:36-44`) — the PRD's `^[A-Za-z_][A-Za-z0-9_]*$`.
  3. Duplicates are detected on the **uppercased** name, so `Foo` and `FOO` collide (`:93-94`).
  4. Caps: 50 parameters, name ≤ 128 chars, value ≤ 8192 bytes (`:32-34`, applied `:118-151`) — decision D3.
  5. Empty name is rejected before the charset check, with its own message (`:63-68`).
  6. Normalization **trims names only** (never values), forces `secure` off because run parameters are never secret, and drops rows that are blank in *both* fields so a stray empty UI row does not fail validation (`:183-195`).
  7. The count cap is checked **before** the per-parameter loop, so 60 oversized parameters report "too many" rather than the first size violation (`:118-127`).

- [ ] **Step 2: Write the failing tests.**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, value: &str) -> RunParameter {
        RunParameter { name: name.to_string(), value: value.to_string() }
    }

    #[test]
    fn a_valid_set_passes() {
        assert!(validate(&[p("FOO", "1"), p("_bar2", "x")]).is_ok());
        assert!(validate(&[]).is_ok());
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let err = validate(&[p("", "1")]).unwrap_err();
        assert!(matches!(err, ParamError::EmptyName));
    }

    #[test]
    fn a_name_must_start_with_a_letter_or_underscore() {
        for bad in ["1FOO", "-FOO", " FOO", "FOO-BAR", "FOO.BAR", "FÖÖ"] {
            let err = validate(&[p(bad, "1")]).unwrap_err();
            assert!(
                matches!(err, ParamError::InvalidName { .. }),
                "{bad} must be rejected as an invalid name, got {err:?}"
            );
        }
    }

    /// The eleven reserved names, rejected case-insensitively. Enumerated in
    /// full rather than sampled: the list is a frozen contract with the runner
    /// (routes/settings.rs:15-27) and a missing entry is a silent hole.
    #[test]
    fn every_reserved_name_is_rejected_case_insensitively() {
        for reserved in RESERVED_NAMES {
            for spelling in [
                reserved.to_string(),
                reserved.to_ascii_lowercase(),
                format!("{}{}", &reserved[..1], reserved[1..].to_ascii_lowercase()),
            ] {
                let err = validate(&[p(&spelling, "1")]).unwrap_err();
                assert!(
                    matches!(err, ParamError::Reserved { .. }),
                    "{spelling} must be rejected as reserved, got {err:?}"
                );
            }
        }
    }

    /// Exactly eleven, and exactly these. Pins the list itself so a future
    /// addition is a deliberate spec amendment rather than a quiet edit
    /// (decision D3).
    #[test]
    fn the_reserved_list_is_the_eleven_legacy_names() {
        assert_eq!(
            RESERVED_NAMES,
            [
                "APP_BUILD",
                "APP_VERSION",
                "E2E_K8S_NAMESPACE",
                "KUBECONFIG",
                "PRODUCT_KEY",
                "RP_API_KEY",
                "RP_PROJECT",
                "SKIP_TESTS_WITH_BUGS",
                "TEST_BUNDLE_URL",
                "TEST_FILES",
                "TEST_VERSION",
            ]
        );
    }

    /// Duplicates collide on the uppercased name (routes/settings.rs:93-94),
    /// so differently-cased spellings of one variable are a conflict.
    #[test]
    fn duplicates_collide_case_insensitively() {
        let err = validate(&[p("FOO", "1"), p("foo", "2")]).unwrap_err();
        assert!(matches!(err, ParamError::Duplicate { .. }));
    }

    #[test]
    fn at_most_fifty_parameters_are_accepted() {
        let fifty: Vec<RunParameter> = (0..50).map(|i| p(&format!("P{i}"), "v")).collect();
        assert!(validate(&fifty).is_ok());

        let fifty_one: Vec<RunParameter> = (0..51).map(|i| p(&format!("P{i}"), "v")).collect();
        let err = validate(&fifty_one).unwrap_err();
        assert!(matches!(err, ParamError::TooMany { count: 51, .. }));
    }

    /// The count cap is checked before the per-parameter loop, so an
    /// over-count set of oversized parameters reports "too many" and not the
    /// first size violation (routes/settings.rs:118-127).
    #[test]
    fn the_count_cap_is_reported_before_any_size_violation() {
        let huge = "x".repeat(MAX_VALUE_LEN + 1);
        let many_huge: Vec<RunParameter> =
            (0..51).map(|i| p(&format!("P{i}"), &huge)).collect();
        let err = validate(&many_huge).unwrap_err();
        assert!(
            matches!(err, ParamError::TooMany { .. }),
            "count is checked first, got {err:?}"
        );
    }

    #[test]
    fn a_name_longer_than_128_chars_is_rejected() {
        let ok = "A".repeat(MAX_NAME_LEN);
        assert!(validate(&[p(&ok, "v")]).is_ok());
        let too_long = "A".repeat(MAX_NAME_LEN + 1);
        let err = validate(&[p(&too_long, "v")]).unwrap_err();
        assert!(matches!(err, ParamError::NameTooLong { .. }));
    }

    #[test]
    fn a_value_larger_than_eight_kib_is_rejected() {
        let ok = "x".repeat(MAX_VALUE_LEN);
        assert!(validate(&[p("BIG", &ok)]).is_ok());
        let too_big = "x".repeat(MAX_VALUE_LEN + 1);
        let err = validate(&[p("BIG", &too_big)]).unwrap_err();
        assert!(matches!(err, ParamError::ValueTooLarge { .. }));
    }

    /// Size caps are byte counts, not char counts — a multi-byte value must
    /// be measured the way the source system measures it (`.len()` on a
    /// String, routes/settings.rs:140).
    #[test]
    fn value_size_is_measured_in_bytes() {
        // 'é' is two bytes in UTF-8, so MAX_VALUE_LEN/2 + 1 of them overflow.
        let value = "é".repeat(MAX_VALUE_LEN / 2 + 1);
        assert!(validate(&[p("BIG", &value)]).is_err());
    }

    // ---------- normalization ----------

    #[test]
    fn normalization_trims_names_but_not_values() {
        let out = normalize(vec![p("  FOO  ", "  spaced  ")]);
        assert_eq!(out, vec![p("FOO", "  spaced  ")]);
    }

    /// A stray row that is blank in BOTH fields is dropped so an empty UI row
    /// does not fail validation; a row blank in only one is kept so it fails
    /// validation loudly (routes/settings.rs:193).
    #[test]
    fn normalization_drops_only_fully_blank_rows() {
        let out = normalize(vec![p("", ""), p("", "orphan-value"), p("FOO", "")]);
        assert_eq!(out, vec![p("", "orphan-value"), p("FOO", "")]);
    }

    /// Validation runs on the normalized list, so trimming is what makes a
    /// padded name acceptable — and what makes an all-whitespace name an
    /// empty-name error rather than a charset error.
    #[test]
    fn an_all_whitespace_name_normalizes_to_an_empty_name_error() {
        let out = normalize(vec![p("   ", "v")]);
        let err = validate(&out).unwrap_err();
        assert!(matches!(err, ParamError::EmptyName));
    }
}
```

- [ ] **Step 3: Run to confirm failure.** `cargo test -p qa-runs params` → compile failure (`cannot find function `validate``, `cannot find value `RESERVED_NAMES``, …).

- [ ] **Step 4: Implement.**

```rust
//! Launch-parameter validation and normalization.
//!
//! Ported from `manager/src/routes/settings.rs:14-195`
//! (`validate_run_parameters`, `validate_variable_list`,
//! `normalize_run_parameters`). Frozen contract:
//! `cpt-cf-qa-fr-runs-params` — "Violations MUST fail the launch with a
//! message naming the offending parameter", so every error variant below
//! carries the name.
//!
//! Pure, and applied at **both** entry points the requirement names: launch
//! and re-run. Re-validating a replayed set is cheap defence-in-depth against
//! a stale row and against a reserved list that has grown since the original
//! launch (`manager/src/routes/runs.rs:920-923`).

use qa_runs_sdk::RunParameter;

/// Names the runner owns; a launch parameter may not shadow any of them.
///
/// Exactly the source system's `RESERVED_PIPELINE_VARIABLE_NAMES`
/// (`manager/src/routes/settings.rs:15-27`), name for name and in the same
/// order, and exactly what `cpt-cf-qa-fr-runs-params` enumerates.
///
/// SECURITY NOTE — inherited parity, verified 2026-08-13 (plan decision D3).
/// This list deliberately does **not** cover the runner's result-callback URL
/// (the source system's `VHP_PROGRESS_URL`, `manager/src/services/argo.rs:438-441`)
/// or `E2E_VHP_BASE_URL`, and the source system's parameter merge does a
/// retain-then-push, so a launch parameter of either name **replaces** the
/// platform-supplied value there. That exposure is carried forward on purpose:
/// the goal is to preserve the source system's behavior and adapt only the
/// architecture. Closing it is a deliberate divergence and belongs in the PRD
/// amendment block for `cpt-cf-qa-fr-runs-params`, not in a quiet edit here.
pub const RESERVED_NAMES: [&str; 11] = [
    "APP_BUILD",
    "APP_VERSION",
    "E2E_K8S_NAMESPACE",
    "KUBECONFIG",
    "PRODUCT_KEY",
    "RP_API_KEY",
    "RP_PROJECT",
    "SKIP_TESTS_WITH_BUGS",
    "TEST_BUNDLE_URL",
    "TEST_FILES",
    "TEST_VERSION",
];

/// Caps bounding what one launch can carry. `MAX_PARAMETERS` is stated by
/// `cpt-cf-qa-fr-runs-params`; the two size caps are enforced by the source
/// system (`manager/src/routes/settings.rs:32-34`) and were added to that
/// requirement by the plan's D3 amendment.
pub const MAX_PARAMETERS: usize = 50;
pub const MAX_NAME_LEN: usize = 128;
pub const MAX_VALUE_LEN: usize = 8 * 1024;

/// Why a parameter set was rejected. Every variant names the offender, which
/// is what `cpt-cf-qa-fr-runs-params` requires of the message.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParamError {
    #[error("parameter name cannot be empty")]
    EmptyName,

    #[error(
        "parameter '{name}' must start with a letter or '_' and contain only letters, \
         digits, or '_'"
    )]
    InvalidName { name: String },

    #[error("parameter '{name}' is reserved by the test runner and cannot be overridden")]
    Reserved { name: String },

    #[error("parameter '{name}' is defined more than once")]
    Duplicate { name: String },

    #[error("too many parameters: {count} (max {max})")]
    TooMany { count: usize, max: usize },

    #[error("parameter name is too long ({len} chars, max {max})")]
    NameTooLong { len: usize, max: usize },

    #[error("value for parameter '{name}' is too large ({len} bytes, max {max})")]
    ValueTooLarge { name: String, len: usize, max: usize },
}

/// Whether a name is a legal environment-variable identifier:
/// `^[A-Za-z_][A-Za-z0-9_]*$` (`manager/src/routes/settings.rs:36-44`).
/// Hand-written rather than a regex — it is two predicates, and the source
/// system writes it the same way.
#[must_use]
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Trim names, force-drop secrecy, and drop fully-blank rows.
///
/// Three rules, each ported (`manager/src/routes/settings.rs:180-195`):
/// names are trimmed and **values are not** (a value's leading space may be
/// meaningful); a run parameter is never secret, so there is no `secure` field
/// to carry; and a row blank in *both* fields is dropped so a stray empty
/// editor row does not fail validation — while a row blank in only one is kept
/// so it fails loudly.
#[must_use]
pub fn normalize(parameters: Vec<RunParameter>) -> Vec<RunParameter> {
    parameters
        .into_iter()
        .map(|parameter| RunParameter {
            name: parameter.name.trim().to_string(),
            value: parameter.value,
        })
        .filter(|parameter| !(parameter.name.is_empty() && parameter.value.is_empty()))
        .collect()
}

/// Validate a normalized parameter set.
///
/// Order matters and is ported: the **count** cap is checked before the
/// per-parameter loop, so an over-count set of oversized parameters reports
/// "too many" rather than the first size violation
/// (`manager/src/routes/settings.rs:118-127`). Sizes are then checked per
/// parameter, and only afterwards the name rules — matching the source
/// system's two-pass shape (`:129-153`).
///
/// # Errors
/// [`ParamError`] naming the offending parameter.
pub fn validate(parameters: &[RunParameter]) -> Result<(), ParamError> {
    if parameters.len() > MAX_PARAMETERS {
        return Err(ParamError::TooMany {
            count: parameters.len(),
            max: MAX_PARAMETERS,
        });
    }

    for parameter in parameters {
        // Byte lengths, as the source system measures them (`String::len`).
        if parameter.name.len() > MAX_NAME_LEN {
            return Err(ParamError::NameTooLong {
                len: parameter.name.len(),
                max: MAX_NAME_LEN,
            });
        }
        if parameter.value.len() > MAX_VALUE_LEN {
            return Err(ParamError::ValueTooLarge {
                name: parameter.name.clone(),
                len: parameter.value.len(),
                max: MAX_VALUE_LEN,
            });
        }
    }

    let mut seen = std::collections::HashSet::new();
    for parameter in parameters {
        if parameter.name.is_empty() {
            return Err(ParamError::EmptyName);
        }
        if !is_valid_name(&parameter.name) {
            return Err(ParamError::InvalidName {
                name: parameter.name.clone(),
            });
        }
        if RESERVED_NAMES
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(&parameter.name))
        {
            return Err(ParamError::Reserved {
                name: parameter.name.clone(),
            });
        }
        // Dedupe on the uppercased name, so `Foo` and `FOO` collide
        // (`manager/src/routes/settings.rs:93-94`).
        if !seen.insert(parameter.name.to_ascii_uppercase()) {
            return Err(ParamError::Duplicate {
                name: parameter.name.clone(),
            });
        }
    }

    Ok(())
}
```

Add to `domain/error.rs`:

```rust
    #[error("invalid launch parameters: {0}")]
    InvalidParameters(#[from] crate::domain::params::ParamError),
```

- [ ] **Step 5: Verify.** `cargo test -p qa-runs params` → **14 passed; 0 failed** (Step 2 defines exactly 14 tests). Then the full gate. Commit: `feat(qa-runs): launch-parameter validation with ported reserved names and caps`.

#### 8b — Run naming (`naming.rs`)

- [ ] **Step 1: Legacy check.** Read `workflow_name_base` (`../testrunner/manager/src/services/argo.rs`, the function located by `grep -n 'fn workflow_name_base' services/argo.rs`) and the `slugify_k8s_name` it calls, plus `run_history::next_sequence_number_from_names`. Confirm and cite: the 48-char prefix cap and *why* (`-{N}` must still fit a 63-char name limit); the fallback chain primary → fallback → default base; the `-test-plan` / `-plan` suffix compaction and its order (`-test-plan` first, else `-plan`); the `-` trimming after both the compaction and the truncation; and the final empty-check that falls back to the default base. **The 63-char limit is a Kubernetes constraint that no longer applies** — record that, keep the cap anyway (see the implementation note), and do not silently widen it.

- [ ] **Step 2: Write the failing tests.**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_name_slugifies() {
        assert_eq!(name_base("Smoke Tests", None, "plan"), "smoke-tests");
    }

    #[test]
    fn non_alphanumerics_collapse_to_single_hyphens() {
        assert_eq!(name_base("A//B__C  D", None, "plan"), "a-b__c-d");
    }

    #[test]
    fn a_blank_primary_falls_back_then_defaults() {
        assert_eq!(name_base("", Some("plan-id-7"), "plan"), "plan-id-7");
        assert_eq!(name_base("!!!", Some("!!!"), "plan"), "plan");
        assert_eq!(name_base("", None, "plan"), "plan");
    }

    /// Tail-word compaction, and its order: `-test-plan` is stripped in
    /// preference to `-plan` (services/argo.rs, workflow_name_base).
    #[test]
    fn common_tail_words_are_compacted() {
        assert_eq!(name_base("Upgrade Test Plan", None, "plan"), "upgrade");
        assert_eq!(name_base("Upgrade Plan", None, "plan"), "upgrade");
        assert_eq!(
            name_base("Test Plan", None, "plan"),
            "plan",
            "stripping the whole slug must fall back to the default base"
        );
    }

    #[test]
    fn a_long_name_is_truncated_to_the_prefix_cap_and_trimmed() {
        let long = "a".repeat(MAX_PREFIX_LEN + 20);
        assert_eq!(name_base(&long, None, "plan").len(), MAX_PREFIX_LEN);

        // Truncation must not leave a trailing hyphen.
        let hyphen_at_cap = format!("{}-{}", "b".repeat(MAX_PREFIX_LEN - 1), "c".repeat(10));
        let out = name_base(&hyphen_at_cap, None, "plan");
        assert!(!out.ends_with('-'), "got {out:?}");
        assert_eq!(out.len(), MAX_PREFIX_LEN - 1);
    }

    #[test]
    fn leading_and_trailing_hyphens_are_trimmed() {
        assert_eq!(name_base("--smoke--", None, "plan"), "smoke");
    }

    // ---------- sequence numbers ----------

    #[test]
    fn the_first_run_of_a_prefix_is_one() {
        assert_eq!(next_sequence("smoke", std::iter::empty()), 1);
    }

    #[test]
    fn the_next_number_is_one_past_the_highest_seen() {
        let existing = ["smoke-1", "smoke-3", "smoke-2"];
        assert_eq!(next_sequence("smoke", existing.into_iter()), 4);
    }

    /// Only exact `{prefix}-{digits}` names count. A different prefix that
    /// merely starts with this one must not bump the sequence, or `smoke` and
    /// `smoke-slow` share a counter.
    #[test]
    fn a_longer_prefix_does_not_bump_this_ones_sequence() {
        let existing = ["smoke-slow-9", "smoke-2"];
        assert_eq!(next_sequence("smoke", existing.into_iter()), 3);
    }

    #[test]
    fn non_numeric_suffixes_are_ignored() {
        let existing = ["smoke-abc", "smoke-", "smoke", "smoke-2x", "smoke-2"];
        assert_eq!(next_sequence("smoke", existing.into_iter()), 3);
    }

    #[test]
    fn run_name_joins_the_base_and_the_number() {
        assert_eq!(run_name("smoke", 7), "smoke-7");
    }
}
```

- [ ] **Step 3: Run to confirm failure**, then implement:

```rust
//! Human-facing run names: `{slug}-{n}`.
//!
//! Ported from `workflow_name_base` + `slugify_k8s_name`
//! (`manager/src/services/argo.rs`) and
//! `run_history::next_sequence_number_from_names`.
//!
//! Why a run needs a short name at all, given it has a UUID: the queue's
//! `blocked_by` text names the run holding a platform ("waiting for run
//! am-validation-smoke-17", guide line 177), and every operator-facing surface
//! in the source system refers to runs this way. A UUID there would be
//! unreadable.
//!
//! # The prefix cap is now arbitrary, and kept anyway
//!
//! The source system's 48-character cap exists so that `-{N}` still fits
//! Kubernetes' 63-character object-name limit. This gear creates no Kubernetes
//! objects, so that constraint is gone — but the cap is retained rather than
//! widened, because run names are compared, logged, and rendered in tables
//! throughout the source system's UI, and changing their length distribution
//! is a visible parity change with no requirement asking for it.

/// Longest slug a run name's prefix may have. See the module docs for why this
/// number is 48 and why it stays 48.
pub const MAX_PREFIX_LEN: usize = 48;

/// Lowercase, ASCII-alphanumeric-or-`_`, with every other run of characters
/// collapsed to a single `-`.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_separator = false;
    // **Corrected 2026-08-13 during execution.** This loop was written over
    // `input.chars()` with an allow-predicate of
    // `ch.is_ascii_alphanumeric() || ch == '_'`. Both halves were wrong against
    // legacy (`manager/src/services/argo.rs:2514-2515`), which iterates
    // `value.to_lowercase().chars()` and allows **only** `is_ascii_alphanumeric()`
    // — no `_` arm. Two observable consequences: an underscore is a *separator*,
    // so `name_base("A__B", …)` is `a-b` and not `a__b`; and the lowercasing is
    // Unicode-aware over the whole string rather than a per-character ASCII fold,
    // so `U+0130` expands to `i` plus a combining mark and contributes a real `i`.
    for ch in input.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_separator && !out.is_empty() {
                out.push('-');
            }
            pending_separator = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_separator = true;
        }
    }
    out
}

/// The run-name prefix for a target: slugified `primary`, falling back to
/// `fallback` and then to `default_base`.
///
/// Ported behaviour, in order (`manager/src/services/argo.rs`,
/// `workflow_name_base`): slugify the primary; if empty, slugify the fallback;
/// if still empty, use the default base. Then compact a common tail word —
/// `-test-plan` in preference to `-plan` — trim hyphens, truncate to
/// [`MAX_PREFIX_LEN`], trim hyphens again (truncation can expose one), and if
/// the result is empty fall back to the default base.
#[must_use]
pub fn name_base(primary: &str, fallback: Option<&str>, default_base: &str) -> String {
    let mut slug = slugify(primary);
    if slug.is_empty() {
        if let Some(fallback) = fallback {
            slug = slugify(fallback);
        }
    }
    if slug.is_empty() {
        slug = default_base.to_string();
    }

    if let Some(stripped) = slug.strip_suffix("-test-plan") {
        slug = stripped.to_string();
    } else if let Some(stripped) = slug.strip_suffix("-plan") {
        slug = stripped.to_string();
    }
    slug = slug.trim_matches('-').to_string();

    if slug.len() > MAX_PREFIX_LEN {
        slug.truncate(MAX_PREFIX_LEN);
        slug = slug.trim_matches('-').to_string();
    }

    if slug.is_empty() {
        default_base.to_string()
    } else {
        slug
    }
}

/// One past the highest `{prefix}-{digits}` sequence among `existing`.
///
/// Only an exact `{prefix}-{digits}` match counts: a longer prefix that merely
/// starts with this one (`smoke-slow-9` against prefix `smoke`) must not bump
/// this prefix's counter, or two plans share a sequence.
#[must_use]
pub fn next_sequence<'a>(prefix: &str, existing: impl Iterator<Item = &'a str>) -> u64 {
    let mut highest = 0u64;
    for name in existing {
        let Some(suffix) = name.strip_prefix(prefix).and_then(|rest| rest.strip_prefix('-')) else {
            continue;
        };
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(number) = suffix.parse::<u64>() {
            highest = highest.max(number);
        }
    }
    highest + 1
}

/// `{base}-{number}`.
#[must_use]
pub fn run_name(base: &str, number: u64) -> String {
    format!("{base}-{number}")
}
```

- [ ] **Step 4: Verify.** `cargo test -p qa-runs naming` → **11 passed; 0 failed**. **(unverified prediction on two assertions:** `non_alphanumerics_collapse_to_single_hyphens` and `a_long_name_is_truncated_to_the_prefix_cap_and_trimmed` depend on `slugify`'s exact collapsing behaviour, which is reimplemented here rather than read line-by-line from `slugify_k8s_name`. **Read that function in Step 1 and correct both the implementation and these two expected values to match it** — the expected values must come from legacy, not from running this code.**)** Commit: `feat(qa-runs): run naming and sequence numbering ported from legacy`.

#### 8c — Environment assembly (`env_assembly.rs`)

- [ ] **Step 1: Legacy check.** Read `../testrunner/manager/src/services/argo.rs:424-521` (the env build in `submit_workflow`) plus `append_pipeline_variables`, `append_platform_variables`, `append_run_parameters` (locate with `grep -n 'fn append_' services/argo.rs`). Confirm and cite **five** things — the last three are the ones the PRD's clean four-tier sentence hides:

  1. The tier order is: static runner variables (`TEST_FILES`, the progress URL, `RP_*`, `APP_VERSION`, `APP_BUILD`, `E2E_K8S_NAMESPACE`, `PRODUCT_KEY`, `SKIP_TESTS_WITH_BUGS`, the repo/bundle vars) → pipeline variables → platform variables → run parameters (`:436-499`). **`APP_VERSION`/`APP_BUILD` read the run's own snapshotted `app_version`/`app_build` columns, not a live lookup against the platform** — user decision 2026-08-13, see the note at the head of Task 9.
  2. `append_platform_variables` and `append_run_parameters` **retain-then-push**: they remove any existing entry of the same name before pushing, i.e. a true override. `append_run_parameters` is literally `append_platform_variables`.
  3. `append_pipeline_variables` **only extends** — it does *not* remove a same-named earlier entry, so a pipeline variable colliding with a static runner variable leaves **two** entries in the list. In Kubernetes the later entry wins at container start, so the observable behaviour is still "last wins"; assembling into a map here is therefore behaviour-preserving, not a change. **Confirm this reading before relying on it** — if pipeline variables are observably *not* overriding in the source system, that is a conflict and you must stop and report.
  4. `E2E_VHP_BASE_URL` is pushed from platform metadata **after** the platform variables (`:493-496`, with the comment "Platform metadata should win over generic pipeline variables when both are present"), so it overrides a platform variable of that name but is still overridden by a run parameter.
  5. `KUBECONFIG` is pushed **last of all**, after the run parameters (`:504-521`), so it wins outright — which is consistent, since it is reserved and therefore unreachable from a parameter anyway.

- [ ] **Step 2: Write the failing tests.**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn v(name: &str, value: &str) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    fn inputs() -> EnvInputs {
        EnvInputs {
            statics: vec![v("TEST_FILES", "a.py,b.py")],
            pipeline: vec![],
            platform: vec![],
            platform_base_url: None,
            parameters: vec![],
            kubeconfig_path: None,
        }
    }

    #[test]
    fn statics_alone_pass_through() {
        let env = assemble(inputs());
        assert_eq!(env.get("TEST_FILES").map(String::as_str), Some("a.py,b.py"));
        assert_eq!(env.len(), 1);
    }

    /// The four-tier chain, each tier overriding the one before it
    /// (cpt-cf-qa-fr-runs-env-assembly; services/argo.rs:436-499).
    #[test]
    fn each_tier_overrides_the_one_before_it() {
        let mut i = inputs();
        i.statics.push(v("SHARED", "static"));
        let env = assemble(i.clone());
        assert_eq!(env.get("SHARED").map(String::as_str), Some("static"));

        i.pipeline.push(v("SHARED", "pipeline"));
        let env = assemble(i.clone());
        assert_eq!(env.get("SHARED").map(String::as_str), Some("pipeline"));

        i.platform.push(v("SHARED", "platform"));
        let env = assemble(i.clone());
        assert_eq!(env.get("SHARED").map(String::as_str), Some("platform"));

        i.parameters.push(v("SHARED", "param"));
        let env = assemble(i);
        assert_eq!(
            env.get("SHARED").map(String::as_str),
            Some("param"),
            "per-launch parameters are the most specific value for this run"
        );
    }

    /// Legacy tier 4: platform metadata's base URL is pushed AFTER the
    /// platform variables, so it beats a platform variable of the same name
    /// (services/argo.rs:493-496) — but a run parameter still beats it.
    #[test]
    fn the_platform_base_url_overrides_a_platform_variable_but_not_a_parameter() {
        let mut i = inputs();
        i.platform.push(v("E2E_VHP_BASE_URL", "from-variable"));
        i.platform_base_url = Some("from-metadata".to_string());
        let env = assemble(i.clone());
        assert_eq!(
            env.get("E2E_VHP_BASE_URL").map(String::as_str),
            Some("from-metadata")
        );

        i.parameters.push(v("E2E_VHP_BASE_URL", "from-parameter"));
        let env = assemble(i);
        assert_eq!(
            env.get("E2E_VHP_BASE_URL").map(String::as_str),
            Some("from-parameter"),
            "E2E_VHP_BASE_URL is not reserved, so a parameter overrides it (decision D3)"
        );
    }

    /// KUBECONFIG is pushed last of all and wins outright
    /// (services/argo.rs:504-521). Reserved, so a parameter cannot reach it —
    /// but the ordering is asserted here rather than left to the reserved-list
    /// check, so removing that check cannot silently break this too.
    #[test]
    fn the_kubeconfig_path_wins_outright() {
        let mut i = inputs();
        i.parameters.push(v("KUBECONFIG", "attacker"));
        i.kubeconfig_path = Some("/.kube/kubeconfig".to_string());
        let env = assemble(i);
        assert_eq!(
            env.get("KUBECONFIG").map(String::as_str),
            Some("/.kube/kubeconfig")
        );
    }

    #[test]
    fn a_run_without_a_platform_gets_no_kubeconfig_entry() {
        let env = assemble(inputs());
        assert!(!env.contains_key("KUBECONFIG"));
        assert!(!env.contains_key("E2E_VHP_BASE_URL"));
    }

    /// Names are matched exactly, not case-insensitively: the source system's
    /// retain compares `existing["name"] != variable.name` with no case
    /// folding. Two differently-cased spellings therefore coexist in the
    /// environment — which is why the DUPLICATE check in `params` folds case,
    /// so a launch can never create that state through parameters.
    #[test]
    fn assembly_matches_names_exactly() {
        let mut i = inputs();
        i.platform.push(v("Shared", "mixed"));
        i.parameters.push(v("SHARED", "upper"));
        let env = assemble(i);
        assert_eq!(env.get("Shared").map(String::as_str), Some("mixed"));
        assert_eq!(env.get("SHARED").map(String::as_str), Some("upper"));
    }

    /// Within one tier, a later entry wins — the tier's own list order is
    /// preserved rather than being an arbitrary map insertion.
    #[test]
    fn within_a_tier_the_last_entry_wins() {
        let mut i = inputs();
        i.platform.push(v("DUP", "first"));
        i.platform.push(v("DUP", "second"));
        let env = assemble(i);
        assert_eq!(env.get("DUP").map(String::as_str), Some("second"));
    }
}
```

- [ ] **Step 3: Run to confirm failure**, then implement:

```rust
//! Run environment assembly.
//!
//! `cpt-cf-qa-fr-runs-env-assembly`: static runner variables → pipeline
//! variables → platform variables → run parameters, where later entries
//! override earlier ones of the same name.
//!
//! Ported from the env build in `submit_workflow`
//! (`manager/src/services/argo.rs:424-521`). Two details the requirement's
//! one-sentence version omits, both verified against the source and both
//! preserved:
//!
//! * **A fifth position between platform variables and run parameters.** The
//!   platform's own base URL is pushed *after* the platform variables
//!   (`argo.rs:493-496` — "Platform metadata should win over generic pipeline
//!   variables when both are present"), so it overrides a platform variable of
//!   that name while still losing to a run parameter. It is not a reserved
//!   name (plan decision D3), so that last part is reachable.
//! * **`KUBECONFIG` is pushed last of all**, after the run parameters
//!   (`argo.rs:504-521`), so it wins outright. Reserved, hence unreachable
//!   from a parameter — but the ordering is what actually enforces it.
//!
//! One representational change, behaviour-preserving: the source system builds
//! a *list* of `{name, value}` entries, and its pipeline tier only appends
//! (`append_pipeline_variables`) where the platform and parameter tiers
//! retain-then-push. A pipeline variable colliding with a static one therefore
//! leaves two list entries — and Kubernetes resolves duplicates by taking the
//! later one, so the observable result is "last wins" either way. Assembling
//! into a map here produces the same environment with no duplicate entries to
//! resolve.

use std::collections::BTreeMap;

/// The tiers, in precedence order. A struct rather than positional arguments
/// because five `Vec<(String, String)>`s in a row are mutually transposable
/// and transposing two of them silently inverts the precedence rule this
/// module exists to enforce.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvInputs {
    /// Runner control variables the control plane sets itself: `TEST_FILES`,
    /// the result-callback URL, `RP_*`, `APP_VERSION`, `APP_BUILD`,
    /// `E2E_K8S_NAMESPACE`, `PRODUCT_KEY`, `SKIP_TESTS_WITH_BUGS`, and the
    /// bundle reference. Lowest precedence, and every name here is reserved
    /// (`domain::params::RESERVED_NAMES`) except the callback URL — see the
    /// SECURITY NOTE there.
    pub statics: Vec<(String, String)>,
    /// Global pipeline variables (qa-environments, `platform_id = None`).
    pub pipeline: Vec<(String, String)>,
    /// Per-platform variables (qa-environments, `platform_id = Some(_)`).
    pub platform: Vec<(String, String)>,
    /// The platform's own base URL, from platform metadata. Sits between the
    /// platform variables and the run parameters — see the module docs.
    pub platform_base_url: Option<String>,
    /// Per-launch parameters. Highest precedence except `KUBECONFIG`.
    pub parameters: Vec<(String, String)>,
    /// Where the executor will mount the kubeconfig. `None` for a run with no
    /// target platform. Applied last of all.
    pub kubeconfig_path: Option<String>,
}

/// Environment variable name for the platform base URL. Named here because
/// this is the only place its precedence position is expressed.
const PLATFORM_BASE_URL_VAR: &str = "E2E_VHP_BASE_URL";

/// Environment variable naming the mounted kubeconfig.
const KUBECONFIG_VAR: &str = "KUBECONFIG";

/// Assemble the run's environment.
///
/// `BTreeMap` rather than `HashMap` so the result is deterministically ordered
/// — the environment is logged, diffed between a run and its re-run, and
/// asserted on in tests, and a nondeterministic order makes all three worse.
///
/// Names are matched **exactly**, with no case folding, because the source
/// system's override compares names verbatim. Two differently-cased spellings
/// therefore coexist. That is safe only because
/// [`crate::domain::params::validate`] folds case when detecting duplicates,
/// so a launch cannot create the state through parameters — the two rules are
/// a pair, and neither is safe to relax alone.
#[must_use]
pub fn assemble(inputs: EnvInputs) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for tier in [inputs.statics, inputs.pipeline, inputs.platform] {
        for (name, value) in tier {
            env.insert(name, value);
        }
    }
    // **Corrected 2026-08-13 during execution.** This was an unguarded
    // `if let Some(base_url)`. Legacy filters the blank case first —
    // `e2e_vhp_base_url.filter(|v| !v.trim().is_empty())`
    // (`manager/src/services/argo.rs:493`) — so an unset or whitespace-only
    // metadata field is *absent*, not an empty override. Without the filter a
    // blank field writes `E2E_VHP_BASE_URL=""` at a tier that outranks the
    // platform variables, clobbering a platform-supplied value with nothing.
    // Note the asymmetry is deliberate: `kubeconfig_path` below has no such
    // guard in legacy either, because it is a control-plane constant rather
    // than operator-supplied metadata.
    if let Some(base_url) = inputs.platform_base_url.filter(|v| !v.trim().is_empty()) {
        env.insert(PLATFORM_BASE_URL_VAR.to_string(), base_url);
    }
    for (name, value) in inputs.parameters {
        env.insert(name, value);
    }
    if let Some(path) = inputs.kubeconfig_path {
        env.insert(KUBECONFIG_VAR.to_string(), path);
    }
    env
}
```

- [ ] **Step 4: Verify.** `cargo test -p qa-runs env_assembly` → **7 passed; 0 failed**. Then the full gate. Commit: `feat(qa-runs): environment assembly with the ported precedence chain`.

- [ ] **Step 5: Batch close-out.** Confirm all three modules are wired in `domain/mod.rs` and the whole suite is green:

```bash
cargo test -p qa-runs
```

Expected: **29 (exclusivity) + 38 (queue) + 28 (state_machine) + 14 (params) + 11 (naming) + 7 (env_assembly) = 127 passed; 0 failed.**

---

### Task 9: Migration and entities

> **Added 2026-08-13 by user decision (raised by Task 4's backward field inventory).** The `qa_runs` table and its entity must carry **`app_version`** and **`app_build`**, both nullable text, snapshotted at launch from the target platform — legacy sets `app_version = platform_version` (`manager/src/routes/runs.rs:594`) and persists it on `run_results` rather than re-deriving it, because the Workflow is GC'd while re-runs happen much later (`migrations/001_initial.sql:306-310`). Re-deriving from `platform_id` would let a platform upgrade silently change a queued run's or a re-run's `APP_VERSION`, breaking reproducibility and PRD:577's preserved-env-var contract. These two columns are absent from the column list below; **add them.** Task 10's mapper and Task 8c's tier-1 static variables must carry them through.

**Files:**
- Create: `qa-runs/src/infra/mod.rs`, `qa-runs/src/infra/storage/mod.rs`, `qa-runs/src/infra/storage/db.rs`
- Create: `qa-runs/src/infra/storage/migrations/{mod.rs,m20260813_000003_initial.rs}`
- Create: `qa-runs/src/infra/storage/entity/{mod.rs,run.rs,run_queue.rs,run_test_result.rs}`
- Modify: `qa-runs/src/lib.rs` (uncomment `pub mod infra;`, remove that `// Task 9:` marker — **corrected 2026-08-13**: this line previously said `// Task 8:`, but Task 8 never touches `lib.rs`, so Task 3 correctly wrote the marker as `// Task 9:`)
- Modify: `gears/qa-platform/docs/DESIGN.md` (§3.7 amendment, Step 2)

**Owns:** all of the above. Does **not** own `infra/storage/mapper.rs` or any `*_sea_repo.rs` (Task 10), and does not own the schedules tables (Task 17 adds a second migration).

**Expected remaining errors after this task:** none. Entities are standalone structs; nothing references them yet.

**Read first:** `qa-environments/src/infra/storage/migrations/m20260812_000001_initial.rs` (the three-dialect DDL-blob shape and the deliberate `qa_platform_leases` column-shape deviation, which is the precedent for documenting one) and `qa-catalog/src/infra/storage/migrations/m20260812_000002_initial.rs` (the tenant-prefixed-unique-index reasoning and the MySQL/InnoDB key-width arithmetic, both written out for implementers).

- [ ] **Step 1: Legacy check — the per-test result shape and its dedupe strategy.** Read `../testrunner/manager/src/routes/runs.rs:1110-1188` (`ProgressPayload` and the `progress` handler) and `:120-173` (`PersistedTestResultRow`, `get_persisted_test_results`). Confirm and cite:
  1. The per-test row carries `test_name`, optional `test_file`, `status`, optional `duration`, optional `launch_id` (the ReportPortal launch link), optional `jira_key` — `:1110-1122`, `:121-129`.
  2. Idempotency is achieved by **delete-then-insert** on `(run_id, test_name, COALESCE(test_file, ''))`, not by a unique index — `:1153-1185`.
  3. `duration` is stored as **text**, not a number, and the source system deliberately keeps extended duration text (there is a test named `parse_test_results_keeps_extended_duration_text`, `argo.rs:3279`) — so do **not** "improve" it into an integer millisecond column without checking what the runner actually emits.
  4. An event arriving before the run row exists is **dropped with a 202**, and reconciled later — `:1146-1149`.

  Rule 2 is also what keeps the schema legal on MySQL: a unique index over `(tenant_id, run_id, test_file, test_name)` would be `36*4 + 36*4 + 1024*4 + 512*4 = 6432` bytes under `utf8mb4`, well past InnoDB's 3072-byte limit. Legacy's delete-then-insert is both the parity behaviour and the shape that fits. Record that in the migration comment.

- [ ] **Step 2: Amend `DESIGN.md` §3.7 for the third table.** The qa-runs table list (`DESIGN.md:568`) names only `runs`, `run_queue`, `schedules`, `schedule_ticks` — it has no per-test table, yet `cpt-cf-qa-fr-runs-results-ingest` requires the system to "persist run **and per-test** results incrementally as events arrive". Insights' `test_results` cannot serve that: it is populated from events and `cpt-cf-qa-principle-async-insights` forbids the run path depending on it. Append to the qa-runs paragraph:

```markdown
`run_test_results` (run_id, test_file, test_name, status, duration, launch_id,
jira_key) — **added 2026-08-13 (qa-runs plan, Task 9).** An earlier draft of
this list omitted a per-test table, but `cpt-cf-qa-fr-runs-results-ingest`
requires per-test results to be persisted incrementally, and
`cpt-cf-qa-principle-async-insights` forbids the run path from reading
qa-insights to get them. So qa-runs owns the authoritative per-test rows (the
`RunResult` entity of §3.1) and qa-insights builds its analytical
`test_results` from the published events — two tables with different owners,
write patterns, and lifetimes, which is the same split the source system had
between `run_results`/`test_results` in the manager and its analytics views.
Deduplication is delete-then-insert on `(run_id, test_name, test_file)`
(`manager/src/routes/runs.rs:1153-1185`) rather than a unique index, which is
also what keeps the key inside InnoDB's 3072-byte limit.
```

- [ ] **Step 3: Write the migration.** One `#[derive(DeriveMigrationName)] pub struct Migration` with a backend `match` producing one `execute_unprepared` DDL blob per dialect, exactly as both sibling gears do. Postgres branch:

```sql
CREATE TABLE IF NOT EXISTS qa_runs (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    run_kind VARCHAR(16) NOT NULL,
    -- Target, flattened. A discriminated column set rather than one JSON blob:
    -- the dispatcher filters on `target_repo_id` and the UI groups by it, and
    -- a JSON blob would make both an unindexable scan. `run_kind` says which
    -- columns are meaningful; the mapper enforces that and fails closed on an
    -- inconsistent row (never a permissive default — a run whose target
    -- silently decoded to nothing would execute nothing).
    target_repo_id UUID NULL,
    target_path VARCHAR(1024) NULL,
    target_test_file VARCHAR(1024) NULL,
    target_custom_plan_id UUID NULL,
    platform_id UUID NULL,
    test_version VARCHAR(512) NULL,
    state VARCHAR(16) NOT NULL,
    exclusive BOOLEAN NOT NULL,
    exclusive_tier VARCHAR(16) NOT NULL,
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
-- system context (see `domain::system_actor`), so the enumeration wants the
-- state leading. Non-unique, so this is not a cross-tenant channel.
CREATE INDEX IF NOT EXISTS idx_qa_runs_state_timeout ON qa_runs(state, timeout_at);
CREATE INDEX IF NOT EXISTS idx_qa_runs_tenant_platform_state ON qa_runs(tenant_id, platform_id, state);
CREATE INDEX IF NOT EXISTS idx_qa_runs_tenant_schedule ON qa_runs(tenant_id, schedule_id);

CREATE TABLE IF NOT EXISTS qa_run_queue (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    platform_id UUID NOT NULL,
    run_id UUID NOT NULL REFERENCES qa_runs(id) ON DELETE CASCADE,
    run_kind VARCHAR(16) NOT NULL,
    source VARCHAR(16) NOT NULL,
    exclusive BOOLEAN NOT NULL,
    -- Seven states: queued / dispatching / running / done / failed /
    -- cancelled / expired. Frozen vocabulary — see DESIGN §3.7's amendment and
    -- `../testrunner/docs/guides/exclusive-runs-and-the-queue.md` lines 88-97.
    -- `dispatching` and `running` are the two that hold a claim on the
    -- platform (`manager/src/services/run_queue.rs:104`).
    state VARCHAR(16) NOT NULL,
    execution_ref VARCHAR(512) NULL,
    error TEXT NULL,
    enqueued_at TIMESTAMPTZ NOT NULL,
    dispatched_at TIMESTAMPTZ NULL,
    finished_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
-- Tenant-prefixed even though `run_id` is already globally unique per run: a
-- tenant-blind unique index on a child table is a cross-tenant channel even
-- when every query is correctly scoped, because resource UUIDs are
-- identifiers, not secrets. A caller who learns another tenant's `run_id`
-- could insert a row of its own tenant referencing it — the insert passes
-- tenant validation, stays invisible to both tenants' scoped reads, and yet
-- permanently collides with the victim's writes while the unique-violation
-- error itself reports whether the victim's row exists. See DESIGN §3.7's
-- "Every unique index is tenant-prefixed" paragraph, which spells this out.
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_run_queue_tenant_run ON qa_run_queue(tenant_id, run_id);
-- The dispatch path: FIFO within a platform. Column order mirrors the query's
-- ORDER BY (`run_queue.rs:243-257`) so the index serves both the filter and
-- the sort.
CREATE INDEX IF NOT EXISTS idx_qa_run_queue_fifo
    ON qa_run_queue(tenant_id, platform_id, state, enqueued_at, id);
-- The TTL sweep and claim reconciliation, both cross-tenant enumerations.
CREATE INDEX IF NOT EXISTS idx_qa_run_queue_state_enqueued ON qa_run_queue(state, enqueued_at);

CREATE TABLE IF NOT EXISTS qa_run_test_results (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    run_id UUID NOT NULL REFERENCES qa_runs(id) ON DELETE CASCADE,
    test_file VARCHAR(1024) NOT NULL DEFAULT '',
    test_name VARCHAR(512) NOT NULL,
    status VARCHAR(16) NOT NULL,
    -- Text, not a number. The source system stores the runner's duration
    -- string verbatim and deliberately keeps extended forms
    -- (`manager/src/services/argo.rs`, `parse_test_results_keeps_extended_duration_text`).
    duration VARCHAR(64) NULL,
    launch_id VARCHAR(255) NULL,
    jira_key VARCHAR(64) NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
-- Deliberately NOT unique on (tenant_id, run_id, test_file, test_name), for
-- two reasons that point the same way. (1) Parity: the source system
-- deduplicates by delete-then-insert on that tuple
-- (`manager/src/routes/runs.rs:1153-1185`), and the ingest service does the
-- same. (2) Key width: that tuple is 36*4 + 36*4 + 1024*4 + 512*4 = 6432 bytes
-- under utf8mb4, well past InnoDB's 3072-byte index limit, so the index could
-- not be created on MySQL without prefix lengths anyway.
CREATE INDEX IF NOT EXISTS idx_qa_run_test_results_run ON qa_run_test_results(tenant_id, run_id);
```

MySQL branch: `UUID` → `VARCHAR(36)`, `TIMESTAMPTZ` → `TIMESTAMP`, `JSONB` → `JSON`, unique indexes as inline `UNIQUE KEY`, FKs as named `CONSTRAINT ... FOREIGN KEY`, and non-unique indexes as inline `KEY` — copy the exact shape from `qa-environments`' MySQL branch. **On the JSON defaults:** MySQL 8.0.13+ accepts the parenthesised expression form, so write `parameters JSON NOT NULL DEFAULT ('[]')` — do **not** introduce a cross-dialect asymmetry here. (The parity plan claimed MySQL `TEXT` columns cannot carry a default and documented an asymmetry to accommodate it; that is true of *literal* defaults only, and the very same migration already used the expression form two tables below. The asymmetry was unnecessary and was removed — lesson 27.) SQLite branch: `UUID`/`TIMESTAMPTZ` → `TEXT`, `JSONB` → `TEXT`, `BOOLEAN` → `INTEGER`, separate `CREATE INDEX` statements.

Verify the three-dialect key widths before committing: on MySQL, `idx_qa_run_queue_fifo` is `36*4 + 36*4 + 16*4 + 4 + 36*4 = 644` bytes — fine. `idx_qa_runs_tenant_name` is `36*4 + 255*4 = 1164` — fine.

- [ ] **Step 4: Write the entities.** One file per table, `#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]` with `#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]`, mirroring `qa-environments/src/infra/storage/entity/platform.rs`. JSON columns are `pub parameters: Json`. `#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)] pub enum Relation {}` and `impl ActiveModelBehavior for ActiveModel {}` on each — the sibling gears declare no relations and join explicitly, so do the same.

- [ ] **Step 5: `db.rs` and the module wiring.** Copy `qa-environments/src/infra/storage/db.rs` verbatim (it is the `DbProvider` re-parameterisation helper) and adapt the error type to this gear's `DomainError`. `migrations/mod.rs` gets the `Migrator` with the single migration; `infra/storage/mod.rs` re-exports the entity module and (later) the repositories; `infra/mod.rs` declares `pub mod storage;`.

- [ ] **Step 6: Verify — and note what proves nothing here.**

```bash
cargo build -p qa-runs
cargo test -p qa-runs
```

Expected: build green; **127 passed** (unchanged — this task adds no tests). **`cargo build` proves nothing about the schema**: SeaORM entities are hand-written structs whose table and column names are runtime strings, so nothing links them to the DDL at compile time. A column-name mismatch is a *runtime* failure. The schema is not actually exercised until Task 10's DB-backed repository tests run — that is where a typo surfaces, and it is why those tests are not optional.

- [ ] **Step 7: Commit.** `git commit -m "feat(qa-runs): schema migration and entities for runs, queue, and per-test results"`

---

### Task 9b: Widen qa-environments with `observed_build` (added 2026-08-13 by user decision)

Raised by Task 9. `qa_runs.app_build` had **no upstream source**: legacy stamps every run with `platforms_meta.build` (`manager/migrations/001_initial.sql:143`, copied at `:170`), but the shipped `qa_environments_sdk::TargetPlatform` exposes only `observed_version`. Left alone, `APP_BUILD` reaches every test empty, and PRD:577 — "environment variable names consumed by tests … are unchanged, so existing test repositories run unmodified" — becomes partially false.

**Decision: add `observed_build` to qa-environments**, mirroring `observed_version` exactly.

**Files (all in `gears/qa-platform/qa-environments/`):**
- `qa-environments-sdk/src/models.rs` — `TargetPlatform::observed_build: Option<String>`, mirroring `observed_version`
- the gear's migration — a new migration file, **not** an edit to the shipped one
- the gear's entity and mapper — the field threaded through
- whatever REST DTO surfaces `observed_version`, if any — mirror it there too

**Owns:** those files only. Does **not** touch qa-runs, qa-catalog, or any other gear.

- [ ] **Step 1: Mirror, do not invent.** `observed_build` must match `observed_version` in nullability, width, mapper treatment, and REST exposure. Read how `observed_version` is declared, migrated, mapped and (if at all) exposed, and follow it. Where the two differ, that difference is a finding — report it rather than choosing.
- [ ] **Step 2: A new migration file.** The shipped migration has run; editing it is not an option. Follow the subsystem's numbering and the gear's own migration conventions.
- [ ] **Step 3: Verify the schema, not the build.** `cargo build` proves nothing about a SeaORM entity — table and column names are runtime strings. Exercise the new column through a real INSERT/SELECT round-trip, and break-test it by renaming the column in the DDL only and confirming a test goes red.
- [ ] **Step 4: Record the writer gap.** `observed_build` will have **no writer**, exactly as `observed_version` has none — the version poller is unbuilt and tracked in `DECOMPOSITION.md` 2.1 as *"the version poller and the `qa.platform.version_changed` schema were claimed by this feature's scope but never implemented."* Add `observed_build` to that record so one retrofit closes both. Do **not** build the poller.
- [ ] **Step 5: Verify.** `cargo build -p qa-environments -p qa-environments-sdk`, `cargo clippy … --all-targets -- -D warnings`, `cargo test -p qa-environments`, `cargo fmt --check`. The gear shipped with **46 tests**; report the new count and confirm none regressed.

---

### Task 10: Repository traits, SeaORM implementations, and mapper

**Files:**
- Create: `qa-runs/src/domain/repos/{mod.rs,runs_repo.rs,queue_repo.rs}`
- Create: `qa-runs/src/infra/storage/{mapper.rs,runs_sea_repo.rs,queue_sea_repo.rs}`
- Modify: `qa-runs/src/domain/mod.rs`, `qa-runs/src/infra/storage/mod.rs`, `qa-runs/src/domain/error.rs`

**Owns:** the above. Does not own `domain/service/**` (Tasks 13–15) or `domain/repos/schedules_repo.rs` (Task 17).

**Read first:** `qa-environments/src/domain/repos/leases_repo.rs` + `infra/storage/leases_sea_repo.rs` (the closest shape — a `DBRunner`-generic trait plus a compare-and-set), and `qa-catalog/src/infra/storage/test_repos_sea_repo.rs` (the richest SecureORM usage, including unique-violation mapping).

**Non-negotiables from the review lessons, applied throughout this task:**

1. **Filter-first ordering** in SecureORM chains: `.filter(...)` **before** `.secure().scope_with(scope)`.
2. **No unscoped queries, ever.** `AccessScope::allow_all()` is banned in production paths — a probe using it created a cross-tenant existence oracle (403-vs-404 discrimination).
3. **Unique violations** use `toolkit_db::secure::ScopeError::is_unique_violation()` mapped to a domain already-exists error. Never string-match; never let one surface as `Database` (500).
4. **Fail closed on corrupt state.** The mapper returns `Err(DomainError::Internal)` on an unrecognised enum string or corrupt JSON — never a permissive default. `unwrap_or_default()` on a JSON column is a finding: a run whose parameter list quietly decoded to empty would execute with the wrong environment, and one whose target decoded to nothing would execute nothing.
5. **`secure_insert` validates only the `tenant_id` column** — it cannot verify that a referenced parent belongs to that tenant. So the *service* must resolve the parent (`qa_runs.id` for a queue row) under a properly-derived scope first; the repository does not and cannot.

- [ ] **Step 1: `mapper.rs` — the enum and JSON codecs, fail-closed.** These are the highest-risk lines in the layer, so they get their own tests.

```rust
//! Entity <-> SDK conversions.
//!
//! Every decoder here **fails closed**: an unrecognised state string or a
//! JSON column that does not parse returns `DomainError::Internal`, never a
//! default. A permissive default is not a smaller bug here, it is a larger
//! one — a run whose `parameters` quietly decoded to `[]` would execute with
//! the wrong environment, and one whose target decoded to `None` would
//! execute nothing at all while reporting success.

use qa_runs_sdk::{ExclusiveTier, QueueState, RunKind, RunSource, RunState, RunTarget};

use crate::domain::error::DomainError;

pub(crate) fn run_state_to_str(state: RunState) -> &'static str {
    match state {
        RunState::Created => "created",
        RunState::Queued => "queued",
        RunState::Dispatching => "dispatching",
        RunState::Running => "running",
        RunState::Succeeded => "succeeded",
        RunState::Failed => "failed",
        RunState::Canceled => "canceled",
        RunState::TimedOut => "timed_out",
        RunState::Expired => "expired",
        RunState::Error => "error",
    }
}

pub(crate) fn run_state_from_str(raw: &str) -> Result<RunState, DomainError> {
    match raw {
        "created" => Ok(RunState::Created),
        "queued" => Ok(RunState::Queued),
        "dispatching" => Ok(RunState::Dispatching),
        "running" => Ok(RunState::Running),
        "succeeded" => Ok(RunState::Succeeded),
        "failed" => Ok(RunState::Failed),
        "canceled" => Ok(RunState::Canceled),
        "timed_out" => Ok(RunState::TimedOut),
        "expired" => Ok(RunState::Expired),
        "error" => Ok(RunState::Error),
        other => Err(DomainError::CorruptState {
            what: "run.state",
            value: other.to_string(),
        }),
    }
}
```

**Amended 2026-08-13 (Task 4 review): do not write a `_to_str` body for any enum that already has `as_str()` in the SDK.** Task 4 ships `as_str()` on all five (`RunKind`, `RunSource`, `RunState`, `ExclusiveTier`, `QueueState`), so each `_to_str` here would duplicate a function three thousand lines away and give the two spellings of cancel*ed* two independent homes. Keep only the `_from_str` half in this mapper and delegate encoding to `as_str()`. The round-trip test below still works unchanged.

Write the `_from_str` half for `QueueState` (the seven frozen names: `queued`, `dispatching`, `running`, `done`, `failed`, `cancelled`, `expired` — note **`cancelled`** with two `l`s for the queue row, matching the guide, while the *run* state is `canceled` with one; they are different vocabularies and the mapper must not "fix" either), `RunKind` (`plan`/`test`/`custom_plan`), `RunSource` (`manual`/`scheduled`), and `ExclusiveTier` (`launch`/`plan.yaml`/`test_meta`/`default` — the strings the source system logs, `exclusivity.rs:29-36`).

Then the target codec and the JSON codecs:

```rust
/// Encode a target into the four flattened columns.
pub(crate) fn target_to_columns(
    target: &RunTarget,
) -> (Option<uuid::Uuid>, Option<String>, Option<String>, Option<uuid::Uuid>) {
    match target {
        RunTarget::Plan { repo_id, path } => (Some(*repo_id), Some(path.clone()), None, None),
        RunTarget::Test { repo_id, path, test_file } => {
            (Some(*repo_id), Some(path.clone()), Some(test_file.clone()), None)
        }
        RunTarget::CustomPlan { id } => (None, None, None, Some(*id)),
    }
}

/// Decode a target from `run_kind` plus the four columns.
///
/// `run_kind` is the discriminant; a row whose columns disagree with it is
/// corrupt and rejected. Reconstructing a partial target instead would launch
/// a run against the wrong content.
pub(crate) fn target_from_columns(
    run_kind: RunKind,
    repo_id: Option<uuid::Uuid>,
    path: Option<String>,
    test_file: Option<String>,
    custom_plan_id: Option<uuid::Uuid>,
) -> Result<RunTarget, DomainError> {
    match (run_kind, repo_id, path, test_file, custom_plan_id) {
        (RunKind::Plan, Some(repo_id), Some(path), None, None) => {
            Ok(RunTarget::Plan { repo_id, path })
        }
        (RunKind::Test, Some(repo_id), Some(path), Some(test_file), None) => {
            Ok(RunTarget::Test { repo_id, path, test_file })
        }
        (RunKind::CustomPlan, None, None, None, Some(id)) => Ok(RunTarget::CustomPlan { id }),
        _ => Err(DomainError::CorruptState {
            what: "run.target",
            value: format!("run_kind={}", run_kind.as_str()),
        }),
    }
}

/// Decode a JSON column, failing closed.
pub(crate) fn json_from_column<T: serde::de::DeserializeOwned>(
    what: &'static str,
    value: &sea_orm::entity::prelude::Json,
) -> Result<T, DomainError> {
    serde_json::from_value(value.clone()).map_err(|error| DomainError::CorruptState {
        what,
        value: error.to_string(),
    })
}
```

Add to `domain/error.rs`:

```rust
    #[error("corrupt persisted state in {what}: {value}")]
    CorruptState { what: &'static str, value: String },

    #[error("run '{name}' already exists")]
    RunNameExists { name: String },

    #[error("run {run_id} already has a queue row")]
    QueueRowExists { run_id: Uuid },

    #[error("run {id} is in state {state} and cannot {action}")]
    IllegalTransition { id: Uuid, state: String, action: String },

    #[error("queue row {id} is {state}, not queued")]
    QueueRowNotQueued { id: Uuid, state: String },
```

- [ ] **Step 2: Write the mapper's failing tests** (pure, so they run without a database):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Every state round-trips, and the set is exhaustive: a new variant that
    /// the decoder does not know fails this test rather than reaching
    /// production as a corrupt-state error.
    #[test]
    fn every_run_state_round_trips() {
        for state in [
            RunState::Created, RunState::Queued, RunState::Dispatching,
            RunState::Running, RunState::Succeeded, RunState::Failed,
            RunState::Canceled, RunState::TimedOut, RunState::Error,
        ] {
            let encoded = run_state_to_str(state);
            assert_eq!(run_state_from_str(encoded).unwrap(), state, "{encoded}");
        }
    }

    #[test]
    fn every_queue_state_round_trips() {
        for state in [
            QueueState::Queued, QueueState::Dispatching, QueueState::Running,
            QueueState::Done, QueueState::Failed, QueueState::Cancelled,
            QueueState::Expired,
        ] {
            let encoded = queue_state_to_str(state);
            assert_eq!(queue_state_from_str(encoded).unwrap(), state, "{encoded}");
        }
    }

    /// The two vocabularies differ by one letter and must not be unified: the
    /// queue row's frozen spelling is `cancelled` (guide line 95) while the
    /// run's is `canceled`. A mapper that "fixed" either would silently break
    /// every persisted row of the other kind.
    #[test]
    fn the_run_and_queue_cancel_spellings_are_deliberately_different() {
        assert_eq!(run_state_to_str(RunState::Canceled), "canceled");
        assert_eq!(queue_state_to_str(QueueState::Cancelled), "cancelled");
        assert!(run_state_from_str("cancelled").is_err());
        assert!(queue_state_from_str("canceled").is_err());
    }

    /// The tier strings are what the source system logs, so an operator
    /// grepping old and new logs sees the same tokens (exclusivity.rs:29-36).
    #[test]
    fn exclusive_tier_strings_match_the_legacy_log_tokens() {
        assert_eq!(exclusive_tier_to_str(ExclusiveTier::Launch), "launch");
        assert_eq!(exclusive_tier_to_str(ExclusiveTier::Plan), "plan.yaml");
        assert_eq!(exclusive_tier_to_str(ExclusiveTier::TestMeta), "test_meta");
        assert_eq!(exclusive_tier_to_str(ExclusiveTier::Default), "default");
        for tier in [ExclusiveTier::Launch, ExclusiveTier::Plan, ExclusiveTier::TestMeta, ExclusiveTier::Default] {
            assert_eq!(exclusive_tier_from_str(exclusive_tier_to_str(tier)).unwrap(), tier);
        }
    }

    #[test]
    fn an_unknown_state_string_fails_closed() {
        let err = run_state_from_str("Succeeded").unwrap_err();
        assert!(
            matches!(err, DomainError::CorruptState { what: "run.state", .. }),
            "an unrecognised value must be an error, never a default; got {err:?}"
        );
        assert!(run_state_from_str("").is_err());
    }

    #[test]
    fn every_target_round_trips() {
        let repo = uuid::Uuid::from_u128(1);
        let plan = uuid::Uuid::from_u128(2);
        for target in [
            RunTarget::Plan { repo_id: repo, path: "plans/smoke.yaml".into() },
            RunTarget::Test {
                repo_id: repo,
                path: "plans/smoke.yaml".into(),
                test_file: "tests/test_a.py".into(),
            },
            RunTarget::CustomPlan { id: plan },
        ] {
            let (r, p, t, c) = target_to_columns(&target);
            assert_eq!(
                target_from_columns(target.kind(), r, p, t, c).unwrap(),
                target
            );
        }
    }

    /// Columns that disagree with `run_kind` are corrupt, not partially
    /// usable. Reconstructing a partial target would launch a run against the
    /// wrong content.
    #[test]
    fn a_target_inconsistent_with_its_run_kind_fails_closed() {
        let repo = uuid::Uuid::from_u128(1);
        // Plan kind with no path.
        assert!(target_from_columns(RunKind::Plan, Some(repo), None, None, None).is_err());
        // Test kind with no test file.
        assert!(
            target_from_columns(RunKind::Test, Some(repo), Some("p".into()), None, None).is_err()
        );
        // Custom-plan kind carrying repo columns.
        assert!(
            target_from_columns(
                RunKind::CustomPlan,
                Some(repo),
                None,
                None,
                Some(uuid::Uuid::from_u128(2))
            )
            .is_err()
        );
        // Plan kind with a custom-plan id as well.
        assert!(
            target_from_columns(
                RunKind::Plan,
                Some(repo),
                Some("p".into()),
                None,
                Some(uuid::Uuid::from_u128(2))
            )
            .is_err()
        );
    }

    #[test]
    fn a_corrupt_json_column_fails_closed() {
        let bad = serde_json::json!({"not": "an array"});
        let out: Result<Vec<String>, _> = json_from_column("run.include_tags", &bad);
        assert!(
            matches!(out, Err(DomainError::CorruptState { what: "run.include_tags", .. })),
            "a JSON column that does not parse must be an error, never unwrap_or_default()"
        );
    }

    #[test]
    fn a_well_formed_json_column_decodes() {
        let good = serde_json::json!(["e2e", "smoke"]);
        let out: Vec<String> = json_from_column("run.include_tags", &good).unwrap();
        assert_eq!(out, vec!["e2e".to_string(), "smoke".to_string()]);
    }
}
```

Run them before implementing: `cargo test -p qa-runs mapper` → compile failure. Then implement and re-run → **9 passed**.

- [ ] **Step 3: `runs_repo.rs` — the trait.** `DBRunner`-generic, mirroring `qa-environments/src/domain/repos/leases_repo.rs`'s exact generic bounds and `AccessScope` threading. Methods:

| Method | Purpose |
|---|---|
| `create(conn, scope, tenant_id, run)` | Insert. Unique violation on `(tenant_id, name)` → `RunNameExists`. |
| `get(conn, scope, id) -> Option<Run>` | Scoped read. |
| `get_by_name(conn, scope, name) -> Option<Run>` | The `blocked_by` text and the operator surfaces address runs by name. |
| `list(conn, scope) -> Vec<Run>` | OData applied at the API layer, not here. |
| `update_state(conn, scope, id, from, to, patch)` | **Conditional on the current state** — the `WHERE state = $from` guard is what makes the transition atomic; a zero-row update means someone else moved it, and the service maps that to `IllegalTransition`. This is the repository's half of the state machine and must not be a blind `UPDATE`. |
| `set_execution_ref(conn, scope, id, execution_ref)` | Recorded separately from the state change, because dispatch records the reference *then* transitions. |
| `add_result_counts(conn, scope, id, delta)` | Incremental, so concurrent result events do not lose counts — a read-modify-write here would. |
| `list_timeout_candidates(conn, scope, now) -> Vec<(Uuid, Uuid)>` | `(run_id, tenant_id)` for runs in `dispatching`/`running` past `timeout_at`. **Returns the tenant so the caller can mint a per-tenant system context for each write** (review lesson 6). |
| `upsert_test_result(conn, scope, tenant_id, run_id, result)` | Delete-then-insert on `(run_id, test_name, test_file)`, matching legacy (`routes/runs.rs:1153-1185`). |
| `list_test_results(conn, scope, run_id) -> Vec<TestResultRow>` | For the run-detail response. |

- [ ] **Step 4: `queue_repo.rs` — the trait.** Same shape. Methods, each named after the legacy function it ports so the mapping is greppable:

| Method | Legacy origin |
|---|---|
| `insert(conn, scope, tenant_id, row)` | `run_queue.rs:162-200` — an admitted launch is inserted **directly in `dispatching`**, with `dispatched_at` set; that is what makes the row a claim *before* the caller submits. |
| `queued_depth(conn, scope, platform_id)` | `:232-240` |
| `queued_rows(conn, scope, platform_id) -> Vec<QueuedRow>` | `:243-257` — `ORDER BY enqueued_at ASC, id ASC` |
| `claims_for_platform(conn, scope, platform_id) -> Vec<ClaimRow>` | `:203-229` — rows in `dispatching`/`running` |
| `platforms_with_queued_rows(conn, scope) -> Vec<(Uuid, Uuid)>` | `:260-267` — returns `(platform_id, tenant_id)` for the cross-tenant enumeration |
| `mark_dispatching(conn, scope, id) -> bool` | `:281-291` — `WHERE id = $1 AND state = 'queued'`; `false` means it was no longer queued, "a cheap guard against double dispatch" |
| `mark_running(conn, scope, id, execution_ref)` | `:294-303` |
| `mark_failed(conn, scope, id, error)` | `:306-317` |
| `mark_done(conn, scope, id)` | `:320-326` |
| `cancel_queued(conn, scope, id, reason) -> bool` | `:410-421` — the `AND state = 'queued'` guard is the **safety property**, not an optimisation |
| `all_claims(conn, scope) -> Vec<ClaimAge>` | `:329-347` — carries `dispatched_at.unwrap_or(enqueued_at)` as the age basis, plus `tenant_id` |
| `expire_queued_before(conn, scope, cutoff, reason) -> Vec<ExpiredRow>` | `:370-401` — **`UPDATE ... RETURNING`**, so a row cannot be expired without the caller holding the data its mandatory alert needs. Restricted to `state = 'queued'` on purpose: such a row holds no claim, so expiring it cannot release a platform a live execution still owns. |
| `fail_orphaned_dispatching(conn, scope, reason) -> u64` | `:351-361` — **no age predicate, boot-only.** Put that in the doc comment, in those words. |
| `list_for_read(conn, scope, platform_id, limit) -> Vec<QueueRow>` | `:454-483` — newest first, all states |
| `row_status(conn, scope, id) -> Option<RowStatus>` | `:426-433` — named fields, never a `(String, String)`: the two are the same type and transposing them at a call site would take the wrong lock |

- [ ] **Step 5: `RowStatus` and friends as named structs, not tuples.** The source system makes this point three times (`run_queue.rs:136-147`, `:682-693`, `exclusivity.rs:88-95`): a pair of same-typed fields is a transposition hazard the compiler cannot catch. Every multi-field return in this layer is a named struct with doc comments on each field. Copy that reasoning into the doc comments.

- [ ] **Step 6: Implement the two SeaORM repositories.** Use the idioms already compiled in `qa-catalog/src/infra/storage/test_repos_sea_repo.rs`. For `update_state`'s conditional update, the shape is a filtered `Entity::update_many()` with `.filter(Column::Id.eq(id)).filter(Column::State.eq(from_str))` **before** `.secure().scope_with(scope)`, returning `rows_affected == 1`. **Do not guess the SecureORM method names** — read the sibling file and match it exactly.

- [ ] **Step 7: DB-backed repository tests.** This is where the schema is actually exercised (Task 9 Step 6). Mirror `qa-catalog/src/domain/service/tests_tenant_scoping.rs`'s harness and `test_support.rs`. **Mocks must carry real tenant ids** — a nil-tenant test double masked exactly this class of bug in qa-catalog until a DB-backed test caught it. Tests:

```text
- a_run_round_trips_through_the_database        (every column, including the JSON ones and every enum)
- a_duplicate_run_name_in_one_tenant_conflicts (is_unique_violation -> RunNameExists, not Database)
- the_same_run_name_in_two_tenants_is_fine     (the unique index is tenant-prefixed)
- a_run_is_invisible_to_another_tenant         (scoped read returns None, not Forbidden)
- update_state_succeeds_only_from_the_expected_state (guard returns false on a stale `from`)
- add_result_counts_accumulates_across_calls   (two +1s make 2 — a read-modify-write would lose one)
- upsert_test_result_replaces_the_prior_row_for_the_same_test
- upsert_test_result_keeps_rows_for_different_test_files
- queued_rows_are_fifo_by_enqueue_then_id
- claims_for_platform_returns_only_dispatching_and_running
- mark_dispatching_is_false_for_a_row_that_is_no_longer_queued
- cancel_queued_refuses_a_dispatching_row      (the safety property, asserted directly)
- expire_queued_before_returns_what_it_expired_and_touches_no_claim
- fail_orphaned_dispatching_ignores_rows_that_have_an_execution_ref
- platforms_with_queued_rows_carries_each_row_s_tenant
- a_queue_row_referencing_another_tenants_run_is_rejected_by_the_service_not_the_repo
```

Write each in full. The last one documents the boundary explicitly: `secure_insert` validates only the `tenant_id` column and **cannot** verify that the referenced `run_id` belongs to that tenant, so the test asserts the repository *accepts* it and records that the ownership precheck is the service's job (Tasks 13–15). Without this test the gap looks like a repository bug and someone "fixes" it in the wrong layer.

- [ ] **Step 8: Verify.**

```bash
cargo test -p qa-runs
cargo clippy -p qa-runs --all-targets -- -D warnings
```

Expected: 127 (pure) + 9 (mapper) + 16 (DB-backed) = **152 passed; 0 failed**. If any DB-backed test fails on a missing column, that is Task 9's DDL and the entity disagreeing — fix the DDL, **not** the entity, unless the entity is the one that is wrong; either way say which in the task report.

- [ ] **Step 9: Commit.** `git commit -m "feat(qa-runs): repository traits, secure ORM implementations, and fail-closed mapper"`

---

### Task 11: The `RunExecutor` port and its mock adapter

ADR-0001 makes execution a port with a mock in p1 and the serverless adapter in feature 2.7. The port's shape is the thing 2.7 has to satisfy, so it is designed here **from the legacy execution contract**, not from what the mock finds convenient.

**Files:**
- Create: `qa-runs/src/domain/ports/{mod.rs,run_executor.rs}`
- Create: `qa-runs/src/infra/executor/{mod.rs,mock.rs}`
- Modify: `qa-runs/src/domain/mod.rs`, `qa-runs/src/infra/mod.rs`, `qa-runs/src/domain/error.rs`

**Owns:** the above. Does not own `infra/events/**` (Task 12).

- [ ] **Step 1: Legacy check — what a submission actually carries, and how state comes back.** Read `../testrunner/manager/src/services/argo.rs:397-560` (`submit_workflow`: the argument list is the execution contract) and the terminate path (`grep -n 'fn terminate_workflow' services/argo.rs`). Confirm and cite:
  1. A submission carries: the test file list, the assembled environment, the kubeconfig **secret reference** (mounted as a volume, `:504-521` — never the material), a timeout, and the run's identifying annotations. Everything else in that argument list is annotation metadata.
  2. `activeDeadlineSeconds` is set from the plan's timeout (`:539`) — so the executor is *also* told the timeout, even though the control plane now enforces it independently (`cpt-cf-qa-fr-runs-timeout`). Both: the executor bound is a backstop, the control-plane sweep is the guarantee.
  3. There is exactly **one execution node per repository group** (`routes/custom_plans.rs:752-764`, where each `DagNodeSpec` is pushed with `bundle_url: Some(config.bundle_url.clone())`), and a single group yields a single node with no synthetic DAG (parity spec §3.4 step 5). **The per-node bundle field is `DagNodeSpec.bundle_url: Option<String>` (`manager/src/models.rs:502`, doc'd `:493-495`: "per-node so a custom plan that spans multiple git repositories can give each pod the checkout for *its* repo"). Do not cite `services/argo.rs:64-66` for this** — that is `RepoRunConfig::bundle_url`, a single `String` *per submission*, which argues the opposite of per-node. (Corrected 2026-08-14; verified.)
  4. Cancellation is a terminate call by execution name, and it is fire-and-forget from the control plane's point of view — the state change arrives through the normal observation path.

- [ ] **Step 2: `run_executor.rs`.**

```rust
//! The execution-plane port (`cpt-cf-qa-principle-executor-port`, ADR-0001).
//!
//! Shaped from the source system's submission contract
//! (`manager/src/services/argo.rs:397-560`), not from what the p1 mock finds
//! convenient — feature 2.7 has to satisfy this trait against
//! serverless-runtime, and a port shaped around a mock is a port that has to
//! be redesigned then.
//!
//! Three operations, matching ADR-0001's mapping: `start` -> workflow
//! invocation, `watch` -> execution event/log stream, `cancel` -> workflow
//! cancellation.

use async_trait::async_trait;
use std::collections::BTreeMap;

use crate::domain::error::DomainError;
use crate::domain::state_machine::ExecutorOutcome;

/// One execution node. There is exactly one per repository group, each
/// carrying its own bundle reference — a single group yields a single node and
/// no synthetic DAG (parity spec §3.4 step 5;
/// `manager/src/routes/custom_plans.rs:696-712`, and note
/// `manager/src/services/argo.rs:64-66` where `bundle_url` is a single
/// `String`, which is what makes one-bundle-per-node the source contract
/// rather than a simplification).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionNode {
    /// Stable node label, used in logs and in the per-node failure report.
    pub name: String,
    /// Where the node fetches its test content.
    pub bundle_ref: String,
    /// Test files this node runs, in discovery order.
    pub test_files: Vec<String>,
}

/// Everything the execution plane needs to run one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSpec {
    /// The run's id, so the executor can correlate its events back.
    pub run_id: uuid::Uuid,
    /// The run's human-facing name, for the execution's own labelling.
    pub run_name: String,
    pub nodes: Vec<ExecutionNode>,
    /// The fully assembled environment (`domain::env_assembly`).
    pub env: BTreeMap<String, String>,
    /// credstore **reference** to the target platform's kubeconfig. Never the
    /// material: the control plane passes the reference and the execution plane
    /// resolves it (DESIGN §3.5; the source system mounts it as a secret
    /// volume, `manager/src/services/argo.rs:504-521`).
    pub kubeconfig_credstore_ref: Option<String>,
    /// Executor-side deadline. A **backstop**, not the guarantee: the source
    /// system relies on it alone (`activeDeadlineSeconds`,
    /// `manager/src/services/argo.rs:539`), whereas
    /// `cpt-cf-qa-fr-runs-timeout` requires the control plane to enforce the
    /// timeout itself because "enforcement cannot rely on the execution
    /// backend alone". Both are set.
    pub timeout_seconds: u64,
}

/// One observation of an execution's progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionEvent {
    /// The execution has begun.
    Started,
    /// A test reported a result.
    TestResult {
        node: String,
        test_file: String,
        test_name: String,
        /// `passed` / `failed` / `skipped` / `error` / `pending` / `running` —
        /// the runner's vocabulary, uppercased in the source system
        /// (`manager/src/services/argo.rs:2194-2199`). Normalised on ingest.
        status: String,
        /// The runner's duration string, kept verbatim — the source system
        /// stores extended forms as text, not as a number.
        duration: Option<String>,
        /// ReportPortal launch link, when the runner reports one.
        launch_id: Option<String>,
    },
    /// A chunk of log output, for the SSE fan-out.
    Log { node: String, line: String },
    /// The execution reached a terminal state.
    Finished {
        outcome: ExecutorOutcome,
        /// Whether any node ended in a failed state. `None` when the executor
        /// reports no per-node detail. Feeds
        /// `state_machine::derive_terminal_state`'s `node_failure` argument —
        /// which exists because a node that dies before emitting any result
        /// (bundle it cannot fetch, image it cannot pull) leaves no failed rows
        /// behind (`manager/src/services/argo.rs:2218-2227`).
        node_failure: Option<bool>,
        /// Operator-facing reason, when the executor gives one.
        message: Option<String>,
    },
}

/// A handle on a started execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartedExecution {
    /// Opaque executor-side reference, persisted as `runs.execution_ref`.
    pub execution_ref: String,
}

/// The execution plane, behind a port.
#[async_trait]
pub trait RunExecutor: Send + Sync {
    /// Submit a run. Returns as soon as the execution is accepted; progress
    /// arrives through [`watch`](Self::watch).
    ///
    /// # Errors
    /// [`DomainError::ExecutorFailed`] when submission itself fails. The
    /// caller records the queue row `failed` and the run terminal — a failed
    /// submit must release the platform claim, or the queue stops draining
    /// (`manager/src/services/run_dispatcher.rs:47-58`).
    async fn start(&self, spec: RunSpec) -> Result<StartedExecution, DomainError>;

    /// Observe an execution. The stream ends after
    /// [`ExecutionEvent::Finished`], or when the execution can no longer be
    /// found.
    ///
    /// Re-attachable by design (ADR-0001, "stream re-attach"): calling this
    /// again after a control-plane restart must resume observation of a still
    /// running execution rather than error. The mock satisfies this trivially;
    /// 2.7 must satisfy it for real, which is why it is in the contract now.
    ///
    /// # Errors
    /// [`DomainError::ExecutorFailed`] when the executor cannot be reached.
    /// Note the fail-safe direction this implies for the caller: an
    /// unreadable executor must **not** be read as "the execution is gone",
    /// because releasing the claim would let a run start beside an exclusive
    /// one (`manager/src/services/run_dispatcher.rs:237-252`).
    async fn watch(
        &self,
        execution_ref: &str,
    ) -> Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = ExecutionEvent> + Send>>,
        DomainError,
    >;

    /// Request cancellation. Idempotent; cancelling an already-terminal
    /// execution succeeds. The resulting state change arrives through
    /// [`watch`](Self::watch) like any other, so this returns as soon as the
    /// request is accepted.
    ///
    /// # Errors
    /// [`DomainError::ExecutorFailed`] when the request cannot be delivered.
    async fn cancel(&self, execution_ref: &str) -> Result<(), DomainError>;

    /// Execution references the executor currently considers active.
    ///
    /// Used by claim reconciliation (`state_machine::reconcile_claim`), which
    /// needs "is this still alive?" for many executions at once — the source
    /// system uses a single `list_workflows()` per tick for exactly this
    /// (`manager/src/services/run_dispatcher.rs:353-375`) rather than one call
    /// per claim.
    ///
    /// # Errors
    /// [`DomainError::ExecutorFailed`]. The dispatcher **skips the tick** on
    /// this error rather than treating the result as empty — an empty answer
    /// would release every claim at once (`run_dispatcher.rs:353-359`).
    async fn list_active(&self) -> Result<Vec<String>, DomainError>;
}
```

Add to `domain/error.rs`:

```rust
    #[error("execution plane error: {0}")]
    ExecutorFailed(String),
```

- [ ] **Step 3: `mock.rs` — a deterministic mock.** Requirements, in order of importance:
  1. **Deterministic.** No wall-clock sleeps that tests depend on, no `rand`. A configurable script of events per run.
  2. **Honours `cancel`.** A cancelled execution's stream ends with `Finished { outcome: Canceled, .. }`, so the cancel path is exercised end to end without 2.7.
  3. **Re-attachable.** `watch` called twice on one `execution_ref` both yield the full remaining event sequence, so the crash-recovery path is testable.
  4. **Records what it was given.** Tests assert on the `RunSpec` it received — that is how env assembly, bundle grouping, and the kubeconfig-reference-not-material rule get verified end to end.
  5. **Injectable failures.** `start` and `list_active` can be made to fail, so the fail-safe directions in Task 14 have something to fail against.

```rust
//! Deterministic in-memory `RunExecutor` (ADR-0001's p1 adapter).
//!
//! Not a stub: this is the adapter every non-execution test runs against, so
//! it is the thing that makes the launch path, the dispatcher, cancellation,
//! and crash recovery verifiable before serverless-runtime exists — which, as
//! of 2026-08-13, is a docs-only gear with zero `.rs` files, so "before" is
//! doing real work here.
//!
//! Deliberately deterministic: a scripted event sequence per run, no sleeps a
//! test can race, and no randomness. A mock that needed `tokio::time` to
//! settle would make every dispatcher test flaky.
```

Implement with an `Arc<Mutex<MockState>>` holding: `scripts: HashMap<Uuid, Vec<ExecutionEvent>>` (keyed by `run_id`, defaulting to a passing single-test script), `submitted: Vec<RunSpec>`, `active: HashSet<String>`, `cancelled: HashSet<String>`, `fail_start: Option<String>`, `fail_list_active: Option<String>`. Expose `pub(crate)` test-only setters for the scripts and the failure injections, and a `submitted()` accessor. `watch` returns `futures::stream::iter` over the script, with the cancel check applied when the stream is created **and** the `Finished` event rewritten to `Canceled` if `cancel` was called.

- [ ] **Step 4: Tests for the mock itself** — a mock nobody tests is a mock that lies:

```text
- start_records_the_spec_and_returns_a_reference
- start_reports_the_injected_failure
- watch_replays_the_scripted_events_in_order
- watch_is_re_attachable_and_replays_from_the_beginning
- cancel_makes_the_stream_finish_as_canceled
- cancel_is_idempotent_on_an_already_cancelled_execution
- cancel_of_an_unknown_reference_succeeds          (idempotency, per the trait doc)
- list_active_reports_started_executions
- list_active_reports_the_injected_failure
```

Write each in full.

- [ ] **Step 5: Verify.** `cargo test -p qa-runs executor` → **9 passed**. Full gate. Commit: `feat(qa-runs): RunExecutor port and deterministic mock adapter`.

---

### Task 11b: Port the reserved-name check to qa-environments (added 2026-08-13 by user decision)

Raised by Task 11's spec review. Legacy rejects the eleven reserved control-variable names on **all three** env write paths — run parameters (`routes/settings.rs:80-83`), pipeline variables (`:243` → `:108` → `:80-83`), and platform variables, which reach the same check indirectly (`routes/platforms.rs:473` → `:505 merge_pipeline_variables` → `settings.rs:243`). **That is what makes "a literal wins over a same-named secret binding" safe**, which is the precedence `RunEnv::new` ports and Task 14 will inherit.

**The port covers one path.** `RESERVED_NAMES` has exactly one consumer (`qa-runs/src/domain/params.rs:329`), and `qa_environments::VariableService::validate_name` enforces charset and length only — zero reserved-name checks exist anywhere in that gear. So an operator with variable-write permission can create a platform variable named `RP_API_KEY`, and that literal replaces the credstore reference. Nothing is exploitable today (no writer yet); Task 14 inherits the premise.

**Decision: port the check**, restoring the premise and rejecting at the write, where the operator gets an actionable error.

**The constraint that makes this awkward, and must be solved rather than ignored:** qa-runs owns `RESERVED_NAMES` and **depends on** qa-environments — so qa-environments cannot import it. Duplicating the list is the obvious move and the obvious drift hazard: two lists that must stay identical, in different crates, with nothing connecting them.

- [ ] **Step 1: Verify the three legacy paths yourself** before porting anything, especially the third — `platforms.rs:505` reaching `settings.rs:243` indirectly is the one that would break the argument if it were wrong.
- [ ] **Step 2: Choose the mechanism and defend it.** Options include duplicating the list in qa-environments, exporting it from `qa-environments-sdk` and having qa-runs consume *that*, or a shared location. **Whatever you choose must come with a drift guard** — a test that fails if the two lists diverge. A cross-crate assertion can live in qa-runs, which already depends on `qa-environments-sdk`. A duplicated list with no such test is not acceptable: this subsystem has found seven guards that pinned nothing, and an unpinned duplicated constant is the eighth waiting to happen.
- [ ] **Step 3: Reject case-insensitively**, as legacy does (`settings.rs:80-83`), and map to the gear's existing validation error so the operator sees which name was refused.
- [ ] **Step 4: Break-verify.** Confirm a variable named `rp_api_key` is refused, that the refusal is case-insensitive, and that the drift guard actually fails when one list changes. Use sha256-compare and three-point (baseline green → mutant red → restored green).
- [ ] **Step 5: Correct Task 11's doc.** `run_executor.rs:278-289` currently asserts the port is complete ("**Ported as** `crate::domain::params::RESERVED_NAMES`"). Once this lands it is true; until then it is false. Update it to name both enforcement sites.
- [ ] **Step 6: Verify.** qa-environments shipped Task 9b at **58 tests**; report the new count and confirm none regressed. Gate both packages with per-command exit codes.

---

### Task 12: The subsystem event vocabulary and its publisher

> **A third status rule, and a decision this task must make explicitly — from Task 10, 2026-08-13.** The security review asked for `trim().to_uppercase()` normalization at ingest. Task 10 took **only the length cap** and declined the uppercase (**corrected 2026-08-13**: an earlier version of this note said it took the trim as well. It did not — `normalize_test_status` truncates and nothing else, so **the trim is also this task's call**, and a runner status of `"PASSED\n"` currently lands in no counter bucket), because **legacy's two writers disagree**: the log-parse mapper uppercases (`manager/src/services/argo.rs:2940`) while the live progress endpoint binds the runner's string **raw** (`manager/src/routes/runs.rs:1174`) — and the counter projection matches uppercase only (`plans.rs:188-192`). So **in legacy, a lowercase `passed` arriving on the progress path counts toward none of the four categorised counters** — it still counts toward `total`, which is a bare `COUNT(*)` at `plans.rs:192`. (Corrected 2026-08-13: an earlier version said "counts toward nothing", which overstates it.) Uppercasing here would silently change that arithmetic under a parity mandate; not uppercasing preserves a quirk that is arguably a legacy bug. Task 10 correctly refused to decide it from the repository layer. **This task owns the call — make it explicitly and record which behavior you chose and why.** Task 10 also truncates the status to 16 chars at the write (by `char`, WARN-logged) so the open set can never be rejected by the column width.

> **Two per-test-status rules found by Task 9, 2026-08-13 — both invert what you would assume from the neighbouring column.**
>
> 1. **The per-test `status` set is OPEN, and its mapper must NOT fail closed.** This is the opposite of `qa_runs.state` twenty lines away in the same migration. Legacy writes unvalidated runner text straight through (`manager/src/routes/runs.rs:1115` → `:1169`) and maps unknown pytest outcomes with `other => return other.to_uppercase()` (`manager/src/services/argo.rs:2932-2943`, verified). A ninth value is a **runner change, not corruption** — rejecting it drops a real result. The eight known values are `PASSED`, `FAILED`, `ERROR`, `SKIPPED`, `PENDING`, `RUNNING`, `XFAIL`, `XPASS`.
> 2. **The four counters do not sum to `total`.** `plans.rs:188-192` filters `passed` on `PASSED`, `failed` on `FAILED|ERROR`, `skipped` on `SKIPPED`, `in_progress` on `PENDING|RUNNING` — and `total` is a bare `COUNT(*)`. `XFAIL` and `XPASS` therefore feed **`total` alone**. Legacy counts them separately in analytics (`analytics.rs:1359-1360`) and never folded them in. An implementer who assumes the four sum will "fix" this by adding them to `passed`, which changes what a run reports.

qa-runs owns the whole vocabulary (Task 1 Step 6 recorded that in DECOMPOSITION 2.3). `cpt-cf-qa-interface-events` is p1 with zero implementation today, and `cpt-cf-qa-principle-async-insights` makes the entire insights architecture depend on it — so this is a prerequisite for feature 2.5, not a nice-to-have.

**Files:**
- Create: `qa-runs/src/domain/ports/event_publisher.rs`
- Create: `qa-runs/src/infra/events/{mod.rs,payloads.rs,publisher.rs}`
- Modify: `qa-runs/src/domain/ports/mod.rs`, `qa-runs/src/infra/mod.rs`

**Owns:** the above.

- [ ] **Step 1: Read the SDK before designing the payloads.** `gears/system/event-broker/event-broker-sdk/src/typed_event.rs` (the `TypedEvent` trait: `TYPE_ID`, `TOPIC`, `SUBJECT_TYPE`, `SOURCE`, `subject()`, and the optional `partition_key`/`tenant_id`) and `gears/system/event-broker/event-broker-sdk/tests/producer/direct.rs` (the `Producer::builder()` shape). Note the trait's own warning on `partition_key`: prefer an authenticated, normalized identifier the producer controls, because MurmurHash3 is non-cryptographic and adversarial keys can hot-spot a partition. **Use the run id, never a user-supplied name.**

  Also check `gears/bss/ledger/ledger/src/infra/events/publisher.rs`: it is a *parked* publisher whose module docs say the broker "is not yet available in gears-rust". That comment is **stale** — `event-broker-sdk` is a workspace member (`Cargo.toml:120`). Verify that for yourself before relying on it; if the crate genuinely cannot be depended on, **stop and report**, because a parked publisher would leave `cpt-cf-qa-interface-events` at zero implementation for a second gear in a row and that is a scope decision, not an implementation one.

- [ ] **Step 2: Define the vocabulary.** DESIGN §3.3's table lists five qa-runs events; the queue needs one more (the guide makes the `expired` alert mandatory, and 2.8 will subscribe to it rather than qa-runs growing a Slack client). Seven in total:

| Event | Subject | Payload |
|---|---|---|
| `qa.run.created` | run id | run id, name, kind, target, platform, exclusive + tier, source, schedule id |
| `qa.run.queued` | run id | run id, queue id, platform id, exclusive, queue position |
| `qa.run.started` | run id | run id, execution ref, started_at |
| `qa.run.finished` | run id | run id, terminal state, counts, started/finished, error |
| `qa.run.canceled` | run id | run id, prior state, who (subject id) |
| `qa.test.result` | run id | run id, node, test file, test name, status, duration |
| `qa.run.queue_expired` | queue id | queue id, run id, platform id, kind, source, exclusive, waited seconds |

The last one carries **every field** the source system's `QueueEventContext` carries (`../testrunner/manager/src/services/run_dispatcher.rs:550-563`) — read that struct and match it, because the alert's whole purpose is that a queued run cannot disappear silently, and a field the alert cannot report is a field the operator cannot act on.

Constants follow the SDK's GTS spelling. One topic for the subsystem, `gts.cf.core.events.topic.v1~qa.runs.v1`, with per-event `TYPE_ID`s — a single topic keeps insights' subscription to one consumer group and keeps run events ordered per partition, which matters because `created` before `queued` before `started` is the ordering insights rebuilds from. **Verify the GTS id format against the SDK's own constants** rather than copying the shape from the test file's `example.*` ids.

- [ ] **Step 3: The port.** Domain must not depend on the broker SDK (no infra types in a domain signature), so the domain sees a trait:

```rust
//! Lifecycle-event publication, behind a port
//! (`cpt-cf-qa-fr-runs-events`, ADR-0002).
//!
//! A port rather than a direct `event_broker_sdk` dependency because
//! `event_broker_sdk` is infra: no domain signature may carry one of its types
//! (the architecture lints enforce this, and `#[domain_model]` coverage is
//! DE0309). It also makes the "does publishing block the run path?" question
//! answerable in one place — see the failure policy below.

use async_trait::async_trait;

use crate::domain::error::DomainError;

/// A lifecycle event, in domain terms. One variant per published event; the
/// infra adapter maps each to its `TypedEvent`.
///
/// Every variant leads with `run_id`: it is the subject and the partition key
/// for all seven, which is what keeps one run's events ordered within a
/// partition. Insights rebuilds `created -> queued -> started -> finished`
/// ordering from that guarantee, so it is a contract, not an implementation
/// detail.
#[derive(Clone, Debug, PartialEq)]
pub enum LifecycleEvent {
    RunCreated {
        run_id: Uuid,
        run_name: String,
        run_kind: RunKind,
        target: RunTarget,
        platform_id: Option<Uuid>,
        exclusive: bool,
        exclusive_tier: ExclusiveTier,
        source: RunSource,
        schedule_id: Option<Uuid>,
    },
    RunQueued {
        run_id: Uuid,
        queue_id: Uuid,
        platform_id: Uuid,
        exclusive: bool,
        /// FIFO position at the moment of enqueue. Informational: it is
        /// recomputed per read and renumbers as rows ahead drain
        /// (guide line 123).
        queue_position: Option<i64>,
    },
    RunStarted {
        run_id: Uuid,
        execution_ref: String,
        started_at: OffsetDateTime,
    },
    RunFinished {
        run_id: Uuid,
        state: RunState,
        passed: usize,
        failed: usize,
        skipped: usize,
        total: usize,
        started_at: Option<OffsetDateTime>,
        finished_at: OffsetDateTime,
        error: Option<String>,
    },
    RunCanceled {
        run_id: Uuid,
        /// The state the run was in when cancellation was requested — the
        /// difference between dropping a queued row and terminating a live
        /// execution, which is what an auditor needs.
        prior_state: RunState,
        /// Who asked, from the `SecurityContext`.
        requested_by: Uuid,
    },
    TestResult {
        run_id: Uuid,
        node: String,
        test_file: String,
        test_name: String,
        /// Normalised on ingest to `passed` / `failed` / `skipped` / `error` /
        /// `pending` / `running`.
        status: String,
        duration: Option<String>,
    },
    /// The mandatory expiry alert. A queued run must never disappear silently
    /// (guide line 96), and since qa-runs owns no notification channel, the
    /// guarantee is met by publishing this plus a WARN log; feature 2.8
    /// subscribes and delivers it.
    QueueExpired {
        run_id: Uuid,
        queue_id: Uuid,
        platform_id: Uuid,
        run_kind: RunKind,
        source: RunSource,
        exclusive: bool,
        /// How long it waited before expiring, floored at 0 for clock skew
        /// (`domain::state_machine::elapsed_seconds`).
        waited_seconds: u64,
        /// The TTL it outlived, so the alert names the knob that produced it.
        ttl_seconds: u64,
    },
}

#[async_trait]
pub trait EventPublisher: Send + Sync {
    /// Publish one lifecycle event.
    ///
    /// **Failure policy: publication never fails the run path.** A broker
    /// outage must not fail a launch or wedge the dispatcher, because
    /// `cpt-cf-qa-principle-async-insights` says insights never blocks the run
    /// path and an event is an insights input. So the caller logs and
    /// continues, and this signature returns `Result` only so the adapter can
    /// report *why* — never so a caller can propagate it.
    ///
    /// The cost is accepted and stated: a dropped event is a gap in insights
    /// that `cpt-cf-qa-feature-insights-foundation`'s rebuild-from-runs
    /// procedure exists to close. That rebuild is what makes at-most-once
    /// publication tolerable here; if it is ever dropped from 2.5's scope,
    /// this policy must be revisited and a transactional outbox used instead
    /// (the shape `gears/bss/ledger` documents for its own events).
    ///
    /// # Errors
    /// [`DomainError::Internal`] describing the publication failure.
    async fn publish(&self, event: LifecycleEvent) -> Result<(), DomainError>;
}
```

The `use` list for that module: `async_trait::async_trait`, `time::OffsetDateTime`, `uuid::Uuid`, `qa_runs_sdk::{ExclusiveTier, RunKind, RunSource, RunState, RunTarget}`, and `crate::domain::error::DomainError`. Note `RunTarget` on `RunCreated`: insights needs to know *what* was run, and re-deriving it from three nullable id fields at the consumer would duplicate this gear's mapper.

- [ ] **Step 4: The adapter.** `infra/events/payloads.rs` holds the `serde`-derived structs with their `TypedEvent` impls; `publisher.rs` holds `BrokerEventPublisher` wrapping the `ClientHub`-resolved `dyn EventBrokerApi` plus a `Producer`, and a `NoopEventPublisher` for tests and for a deployment with no broker. Build the producer **once at `init`**, not per publish — `prepare_all()` is a network round trip.

- [ ] **Step 5: Tests.**

```text
- every_lifecycle_event_maps_to_a_distinct_type_id   (a copy-paste TYPE_ID collision is otherwise invisible)
- every_event_subjects_on_an_id_never_on_a_name      (the partition-key warning in the SDK trait docs)
- the_queue_expired_payload_carries_every_field_the_alert_needs
- the_noop_publisher_reports_success_and_publishes_nothing
- a_publish_failure_is_returned_not_panicked
```

The first two are cheap and catch the two mistakes that are impossible to see in review. Write each in full.

- [ ] **Step 6: Verify.** `cargo test -p qa-runs events` → **5 passed**. Full gate. Commit: `feat(qa-runs): subsystem lifecycle event vocabulary and broker publisher`.

---

### Task 13: The launch service — resolution, grouping, and the six-step launch contract

> **`OwnedRunId` — an API Task 10 introduced that this task and Tasks 14/15 must adopt. Added 2026-08-13.**
>
> Task 9's security review found the FK is a cross-tenant existence oracle: `run_id REFERENCES qa_runs(id)` has no tenant component, so an insert carrying a guessed `run_id` **succeeds if that run exists in any tenant and fails if it does not** — the response itself discriminates. The plan said the repository "does not and cannot" do the precheck. Task 10 closed it **structurally instead of by prose**: `OwnedRunId` is a newtype whose field is private to `domain::repos::runs_repo`, and its only constructor is `RunsRepository::resolve_owned` — a *provided* trait method, so an implementor overriding it still cannot mint a token except by delegating. It does a tenant-scoped `get` and returns `RunNotFound` for absent and foreign alike.
>
> `QueueRepository::insert`, `upsert_test_result` and `list_test_results` take the **token**, not a `Uuid`. The token carries the id it verified, so "resolve A, write about B" is also unrepresentable, and `domain::service` is a sibling module that cannot construct one.
>
> **Consequence for this task and 14/15: you cannot pass a bare `Uuid` to those three methods.** Call `resolve_owned` first. That is not friction — it is the precheck, and making it structural is why the obligation cannot be forgotten the way a doc comment can. **The rule that makes the token mean anything — do not skip this.** A token proves only that *some* `RunsRepository::get` answered `Some` for this id under this scope; it is exactly as trustworthy as the repository that minted it, and `resolve_owned`'s provided body trusts `get`, a **required** method. A service-layer double whose `get` returns `Some` for convenience therefore mints tokens freely, and every service test then "proves" a precheck it never exercised. **So a `RunsRepository` double's `get` MUST apply tenant scoping to its fixture**, returning `None` when the scope does not admit the row's tenant — one line, `scope.contains_uuid(OWNER_TENANT_ID, row.tenant_id)`. Task 10 considered sealing the trait so only the ORM impl could satisfy it and rejected that, since it would block the very service unit tests the doubles exist for: constrain the doubles, not the trait.
>
> Task 10 also pinned the boundary lesson in `the_token_is_what_stops_a_cross_tenant_run_id_not_the_foreign_key`, which asserts that `secure_insert` validates only the row's own `tenant_id` — so nobody "fixes" this in the wrong layer.

The parity spec's §3.4 is six numbered rules and **each one is a legacy-derived rule that gets its own citation**. This is the task the continuation prompt singles out.

> **Two obligations this task inherits from Task 5, added 2026-08-13 by its code review — neither was recorded anywhere before.**
>
> 1. **Only pass files that were actually read** into `resolve_plan_tier`. An admitted file always votes, so a *default* `FileMeta` votes `Some(false)`: it turns a `Default` resolution into a `TestMeta`-false one, and if the unreadable file was the destructive one, its vote is replaced by a parallel vote. `qa_catalog_sdk::TestFileMeta` derives `Default`, so `FileMeta { path, ..from(meta) }` for an unfetchable file is an easy and silent mistake in the dangerous direction. **Omit unreadable files; never substitute a default.**
> 2. **Port legacy's unreadable-files warning, which lives here and not in the pure core.** `manager/src/services/exclusivity.rs:440-449` fires `tracing::warn!` when `unreadable > 0 && flags.is_empty()` — deliberately *not* "every file was unreadable", so the mixed case is covered while a legitimately all-filtered-out run stays quiet. Its own comment — at **`:433-439`**, not in the `:440-449` predicate block (corrected 2026-08-14; verified) — calls this "the one failure mode here that an operator must see": a destructive test silently losing its exclusivity guarantee. The pure module cannot emit it (no I/O, and it has no unreadable-count input), so if this task does not carry it, nothing does.

**Files:**
- Create: `qa-runs/src/domain/service/{mod.rs,launch.rs}`
- Create: `qa-runs/src/domain/service/launch_tests.rs`
- Create: `qa-runs/src/domain/service/test_support.rs`
- Create: `qa-runs/src/domain/system_actor.rs`
- Modify: `qa-runs/src/domain/mod.rs`, `qa-runs/src/domain/error.rs`

**Owns:** the above. Does **not** own `service/admission.rs` (Task 14), `service/dispatch.rs` (Task 14), `service/ingest.rs` or `service/runs.rs` (Task 15).

**Expected remaining errors after this task:** none — `launch.rs` calls into admission through a small trait it defines itself (see Step 6), which Task 14 implements. That keeps this task compiling standalone and keeps the two reviewable separately.

- [ ] **Step 1: Legacy check — all six launch-path rules.** Read, in this order, and cite each in the code beside the logic it justifies. **If any differs from the description here, stop and report.**

  1. **Branch resolution: explicit request → platform `default_branch` → repository `default_branch`.** `manager/src/services/exclusivity.rs:297-312` (`effective_branch`: explicit trimmed-non-empty, else the platform default, trimmed and non-empty-filtered, else `None`), then `manager/src/routes/runs.rs:639-644` (the repository default fills the `None`). Note the source system's own warning at `exclusivity.rs:290-296`: this chain is **duplicated** across three route files and the duplication is load-bearing and unenforced — "if one copy's trimming or precedence drifts, this scan reads a different branch's files than the run will execute". **Write it once here.** That single-definition fix is the whole reason this rule is worth a task step.
  2. **Group the custom plan's test entries by `repo_id`.** `manager/src/routes/custom_plans.rs:647-780`; the grouping itself is `:652-658` (`BTreeMap` declared `:652`, iterated `:654-658`). **Read the comment at `:647-651` first** — it records the production bug the grouping fixed, and it is the best available argument for why the grouping matters. **Note the shape difference:** legacy has no `CustomPlan.files` — it is `pub tests: Vec<CustomPlanTest>` (`manager/src/models.rs:508-527`) where `CustomPlanTest` is `{ plan_id, test_file }` (`:484-487`), and the grouping keys off `plan_repo_ids.get(&test.plan_id)` rather than a `repo_id` on the entry. `files` is *this port's* shape; the mapping from `plan_id` → `repo_id` is a step legacy performs that the port must account for. (Corrected 2026-08-14; verified.)
  3. **Guard: more than one repo group and no explicit branch → `FailedPrecondition`, naming the constraint.** `manager/src/routes/custom_plans.rs:673-687`. **Two distinct strings live here, do not conflate them:** the sentence *"Mixed multi-repo plans must pin one explicit branch so every checkout resolves the same ref and results register under that Version target"* is the **source comment** at `:671-672`, i.e. the *rationale*. The **actual operator-facing message** is `:681-685`: *"Mixed custom plans that span multiple repositories require an explicit branch. Select a branch that exists in every contributing repository."* — it names no "Version target" and carries **no group count**. So `AmbiguousBranch`'s `groups` field is a deliberate improvement over legacy, not parity; say so rather than citing the comment as the message. (Corrected 2026-08-14; verified.)
  4. **Per group: force-sync the branch, then `create_bundle`.** `manager/src/routes/runs.rs:647-656` (force-sync) and `:714-727` (bundle), with the concurrent per-repo build at `custom_plans.rs:696-712`.
  5. **One execution node per group,** each carrying its own bundle reference; a single group yields a single node and no synthetic DAG. The per-node field is `DagNodeSpec.bundle_url: Option<String>` (`manager/src/models.rs:502`, doc'd `:493-495`), pushed per group at `routes/custom_plans.rs:752-764`. `routes/custom_plans.rs:931-937` is `build_repo_run_config`, which is indeed called once per repo but builds a `RepoRunConfig` (bundle URL + `test_version`), **not** a node. **Do not cite `services/argo.rs:64-66`** — `RepoRunConfig::bundle_url` is a single `String` *per submission* and argues the opposite of per-node.

     **Also port the local-tests wrinkle:** non-repo ("local") tests get their own **bundle-less** node (`routes/custom_plans.rs:773-780`, `bundle_url: None`, `test_version: None`) so they execute from the runner image's built-in `/test_plans` "rather than being dropped from the run entirely" (its own comment, `:770-772`). "One node per repo group" is therefore *groups + at most one local node*. (Corrected 2026-08-14; verified.)
  6. **Record `test_version` as the branch label.** `manager/src/routes/custom_plans.rs:960` and `routes/runs.rs:645` (`let test_version = Some(branch.clone());`). Note the direction: the branch label **is** the recorded test version — there is no version→branch mapping, and looking for one is the mistake DECOMPOSITION:122 records as a withdrawn requirement.

- [ ] **Step 2: Legacy check — what resolution must *not* do.** Read `exclusivity.rs:394-402` and `:176-179`. Confirm: the exclusivity scan **does not sync** ("admission must stay cheap — the sync and the bundle build are what dispatch is for"), so a `TEST_META` change pushed between the last sync and this launch is not seen; that is a documented limitation, and the decision actually made is recorded on the run. And resolution **never fails a launch**. Both port directly: the launch service resolves exclusivity from whatever the catalog can serve *without* forcing a sync, and force-syncs only in the dispatch phase.

  This ordering also matters for correctness, not just cost: the admission decision (and therefore the queue's FIFO position) must be made before minutes of I/O, or two concurrent launches serialise behind each other's bundle builds.

- [ ] **Step 3: Legacy check — timeout resolution.** Read `argo.rs:168-181` (`default_timeout_seconds`) and its three call sites (`:539` plan runs use `plan.plan.timeout_seconds`; `:746` single tests fall back to 1800; `:1065` custom plans use `composed_timeout_seconds.max(default_timeout_seconds(3600))`). Confirm the shape: a configured default that is only used when it is `> 0`, per-kind fallbacks of 1800 and 3600 seconds, and a custom plan taking the **max** of its composed sum and the default. Port the chain as: explicit launch override → the plan's `timeout_seconds` → the configured default → the per-kind fallback. **If the per-kind fallbacks differ from 1800/3600, use what you read and say so.**

- [ ] **Step 4: `system_actor.rs`.** Copy the structure of `qa-catalog/src/domain/system_actor.rs` exactly — a hand-picked stable actor UUID, a `qa_runs.system` subject type, one named factory per legitimate call site, each logging under a `qa_runs.system_actor` target, plus the module doc explaining that these contexts do **not** bypass the PEP. Factories needed (each used by exactly one call site, which is what makes a new one a review magnet):

```text
for_dispatch_enumeration()      -> nil tenant   (cross-tenant "which platforms have queued rows")
for_dispatch(tenant_id)         -> tenant-bound (claiming and dispatching one platform's rows)
for_claim_reconciliation()      -> nil tenant   (cross-tenant claim listing)
for_claim_release(tenant_id)    -> tenant-bound (releasing one claim)
for_ttl_sweep()                 -> nil tenant   (cross-tenant expiry enumeration)
for_ttl_expiry(tenant_id)       -> tenant-bound (expiring one tenant's rows + its alert)
for_timeout_sweep()             -> nil tenant   (cross-tenant timeout candidates)
for_timeout_enforcement(tenant_id) -> tenant-bound (cancelling one run)
for_schedule_tick()             -> nil tenant   (Phase B: cross-tenant due-schedule enumeration)
for_schedule_fire(tenant_id)    -> tenant-bound (Phase B: launching one schedule's run)
```

The nil/tenant-bound split is review lesson 6 and it is not cosmetic: **a nil-tenant context produces zero constraints and, because the gear requires constraints, is *denied*** — so "platform-scoped" only works when the PDP returns a covering constraint set. Every *write* is therefore bound to the row's own tenant, and the nil-tenant factories are enumeration-only. Include the same test block qa-catalog has (stable subject id, subject type, tenant binding) covering all ten factories.

- [ ] **Step 5: `service/mod.rs` — the DI container.** Mirror `qa-catalog/src/domain/service/mod.rs`: a generic `AppServices<R, Q>` over the two repository traits, a `ServiceDeps` struct carrying `db`, `authz`, the catalog and environments SDK clients, the `RunExecutor`, the `EventPublisher`, and the config values. `pub(crate)` on `AppServices` and every service (avoids `missing_errors_doc` on the public surface). Declare the `resources` and `actions` constant modules here: resource types `qa.run`, `qa.queue_entry`, `qa.schedule`; actions `create`, `get`, `list`, `cancel`, `rerun`, `dispatch`, `force_start`.

- [ ] **Step 6: The admission seam.** `launch.rs` needs "decide and record under the platform lock" but must not contain it (Task 14 owns that, and it is where the concurrency lives). Define the seam here:

```rust
/// Decide-and-record, under the platform's admission lock.
///
/// A trait rather than a direct call so the launch path is testable without a
/// lock registry or a lease, and so the two halves review separately: this
/// module owns *what a run is*, `service::admission` owns *whether it may
/// start now*, and that boundary is exactly where the source system's
/// per-platform mutex sits (`manager/src/services/run_queue.rs:566-588`).
#[async_trait]
pub(crate) trait Admitter: Send + Sync {
    /// # Errors
    /// `DomainError::QueueFull` / `DomainError::ConcurrencyLimit` for the two
    /// 429 causes; otherwise a persistence or lease failure.
    async fn admit(
        &self,
        ctx: &SecurityContext,
        run: &Run,
    ) -> Result<Admission, DomainError>;
}

/// The result of admitting one launch.
///
/// An enum rather than a struct because the queue-row id only exists when a row
/// was written, and a struct would have needed a sentinel
/// (`manager/src/services/run_queue.rs:590-606`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Row inserted `dispatching`; the caller dispatches inline and answers 200.
    Dispatch { queue_id: Uuid },
    /// Row inserted `queued`; the caller answers 202.
    Queued { queue_id: Uuid, position: Option<i64> },
    /// No platform, so nothing to coordinate — never queued, never occupancy
    /// (guide line 76). Dispatch inline with no queue row at all.
    Unqueued,
}
```

Add to `domain/error.rs` — the two 429 causes, kept as **distinct variants** because the guide says there are exactly two and each must name its own knob (guide lines 240-242):

```rust
    #[error("run queue for platform {platform_id} is full: {queued} of {limit} slots used")]
    QueueFull { platform_id: Uuid, queued: usize, limit: u32 },

    #[error("max concurrent runs limit reached ({limit})")]
    ConcurrencyLimit { limit: u32 },

    #[error(
        "custom plan {plan_id} spans {groups} repositories, so a branch must be given \
         explicitly: no single repository default applies"
    )]
    AmbiguousBranch { plan_id: Uuid, groups: usize },

    #[error("catalog error: {0}")]
    Catalog(String),

    #[error("environments error: {0}")]
    Environments(String),
```

- [ ] **Step 7: Implement `launch.rs`.** The public entry is one method; its structure is the six rules in order. Key requirements, each traceable to a review lesson or a legacy citation:

  - **PEP before every repository call, one scope per resource type.** Never reuse a scope compiled for `qa.run` on a query against the queue table. Derive `qa.run`/`create` for the run insert, and let the admitter derive its own.
  - **Ownership precheck before any existence probe.** The launch reads a platform (via the environments SDK) and a plan or custom plan (via the catalog SDK); both are cross-gear calls that carry the caller's `SecurityContext`, so tenancy is enforced on the far side. Do **not** add a local existence probe on top — that is where qa-catalog's cross-tenant 403-vs-404 oracle came from.
  - **Validate parameters first.** `params::normalize` then `params::validate`, before any I/O: a launch that will be rejected must not force-sync anything.
  - **Resolve the branch once,** via a single private `resolve_branch` fn, and note in its doc comment that the source system duplicated this across three files and warned about the drift (rule 1).
  - **Never let resolution fail the launch.** A catalog error while gathering exclusivity inputs logs a warning and resolves parallel (`Tiers::default()`), exactly as legacy does — the launch then fails later at dispatch for the real reason, if at all.
  - **Group, then guard.** Group the target's files by `repo_id`; if `groups.len() > 1` and no explicit branch was given, return `AmbiguousBranch` (which maps to `FailedPrecondition` → **HTTP 400**, not 409 — review lesson 11).
  - **Do not sync or build bundles here.** Rules 4 and 5 belong to the dispatch phase (Step 2's finding). `launch` creates the run row, resolves exclusivity, and admits; `service::dispatch` does the I/O.
  - **Record the resolved facts on the run before admitting:** `exclusive`, `exclusive_tier`, `test_version` (the branch label, rule 6), `is_validation`, `timeout_at`, and the run name.
  - **Publish `qa.run.created`, then admit, then publish `qa.run.queued` or dispatch inline.** Publication failures log and continue (Task 12's stated policy).

- [ ] **Step 8: Unit tests with mock ports** (`launch_tests.rs`, `#[path]`-included like `qa-catalog`'s `*_tests.rs` files). `test_support.rs` provides in-memory catalog and environments clients plus a `RecordingAdmitter`. **Every mock carries a real tenant id** — a nil-tenant double is what hid the qa-catalog background-write bug.

```text
Branch resolution (rule 1)
- an_explicit_branch_wins_over_both_defaults
- a_blank_explicit_branch_falls_through_to_the_platform_default
- the_platform_default_wins_over_the_repository_default
- a_blank_platform_default_falls_through_to_the_repository_default
- a_run_with_no_platform_uses_the_repository_default
Grouping and the guard (rules 2, 3)
- a_single_repo_custom_plan_produces_one_group
- a_multi_repo_custom_plan_with_an_explicit_branch_produces_one_group_per_repo
- a_multi_repo_custom_plan_without_an_explicit_branch_is_a_failed_precondition
- the_ambiguous_branch_error_names_the_constraint_and_the_group_count
test_version (rule 6)
- the_resolved_branch_label_is_recorded_as_the_test_version
Exclusivity wiring
- a_plan_flag_decides_without_the_catalog_being_asked_for_test_meta
- an_exclusive_test_file_makes_the_run_exclusive_and_reports_the_test_meta_tier
- a_launch_override_of_false_beats_an_exclusive_test_file
- files_dropped_by_the_exclude_filter_do_not_make_the_run_exclusive
- a_catalog_failure_while_resolving_exclusivity_resolves_parallel_and_still_launches
- exclusivity_resolution_never_force_syncs
Parameters
- a_reserved_parameter_name_fails_the_launch_before_any_catalog_call
- a_replayed_parameter_set_is_re_validated
Timeout
- an_explicit_timeout_override_wins
- the_plans_timeout_is_used_when_no_override_is_given
- the_configured_default_is_used_when_the_plan_declares_none
- a_zero_configured_default_falls_back_to_the_per_kind_default
Outcomes
- an_admissible_launch_returns_started
- a_blocked_launch_returns_queued_with_its_queue_id
- a_platformless_launch_is_never_queued
- a_full_queue_surfaces_the_queue_full_error_naming_the_limit
- the_concurrency_limit_surfaces_its_own_error_naming_its_own_limit
- the_run_row_records_the_resolved_exclusivity_tier
- a_broker_failure_does_not_fail_the_launch
```

Write each in full.

- [ ] **Step 9: Verify.** `cargo test -p qa-runs launch`. **Step 8's list names 29 tests, not the 31 this step originally predicted — the plan contradicted itself (5+4+1+6+2+4+7 = 29). Both numbers were wrong about the tree: the actual count is 51 launch tests and 327 lib + 1 integration + 1 ignored, all green.** (Reconciled 2026-08-14 against the shipped commit; the original "207 total" prediction was flagged unverified and was indeed wrong.) Assert that all 29 named tests exist; treat any surplus as requiring a justification, not as a pass.

- [ ] **Step 10: Commit.** `git commit -m "feat(qa-runs): launch service implementing the six-step launch contract"`

---

### Task 13b: Add `default_branch` to qa-environments (added 2026-08-14 by user decision)

Raised by Task 13. Parity spec §3.4 rule 1 resolves a run's branch as **explicit → the target platform's `default_branch` → the repository's `default_branch`**. Legacy has the middle tier: `platforms_meta.default_branch` (`manager/migrations/001_initial.sql:233`). **Citation corrected 2026-08-14 — the original `manager/src/routes/platforms.rs:846-848` does not exist; that file is 521 lines long.** The real sites, all in `manager/src/services/platforms.rs`, verified:

- **read**: `:845-848`, `get_platform_default_branch` — *"Get the per-platform default branch override (falls back to repo default)"*, which is rule 1's middle tier stated in legacy's own words. (**Span corrected again 2026-08-14.** The function body is `:846-848`; the sentence quoted here is the doc comment at `:845`, so a bullet that quotes it must include `:845`. Both earlier readings were half right, which is why the shipped code cites `:845-848`.)
- **write A** (`:385-410`, full upsert): `default_branch.and_then(|v| { trim; if empty { None } else { Some(trimmed) } })`, then `ON CONFLICT ... default_branch = $7` — **unconditional**, so `None` *clears* the column.
- **write B** (`:447-496`, the patch path): a **three-state `__NULL__` sentinel**. `None` (field absent) → `WHEN $6 IS NULL THEN platforms_meta.default_branch`, i.e. **keep**; `Some("")`/whitespace → `"__NULL__"` → **set NULL**; `Some(v)` → the **trimmed** value. The shipped `qa_environments_sdk::TargetPlatform` has no such field — `DESIGN.md:662`'s column list dropped it with no recorded decision.

Task 13 implemented the **full** three-tier chain and tested it; `platform_default_branch` returns `None` today, and `platforms_are_not_available_yet` is written to fail the moment the field lands. So this task is the field, not the chain.

Without it a launch with no explicit branch silently uses the repository default where legacy would have used the platform's — a behaviour change an operator who pinned a platform to a release branch would notice, with no error and no warning.

**Files:** in `gears/qa-platform/qa-environments/` — the SDK model, a **new** migration, the entity, the mapper, and the **two** REST request types (`CreatePlatformReq` *and* `UpdatePlatformReq`; the singular "the REST request type" below is wrong and was echoed into the shipped code). Unlike `observed_build`, this field is **operator-set**, so it must be writable, not just readable. **Plus three files in `gears/qa-platform/qa-runs/`** per Step 4's inversion — the "all in qa-environments" this line originally claimed stopped being true on 2026-08-14, as did the section title.

- [ ] **Step 1: Verify the legacy semantics first.** The coordinator pre-verified these on 2026-08-14 (the three bullets above) because the plan's original citation did not exist — **re-verify them rather than trusting this relay**, then answer the three questions the step was asking:

  - **Trimmed?** Yes, on both write paths.
  - **Empty stored or NULL?** Normalised to **NULL**, on both.
  - **What happens on update?** *This is the part the plan previously did not capture and the part most likely to be got wrong.* The two paths **disagree by design**: the full upsert overwrites unconditionally (absent ⇒ clear), while the patch path is genuinely **three-state** (absent ⇒ keep, empty ⇒ clear, value ⇒ set).

  Consequence for Task 13's `resolve_branch`: because both writers already trim and NULL-out empty, the stored value is always either NULL or a trimmed non-empty string, so `resolve_branch`'s own trim-and-filter is **defensively redundant, not contradictory** — the two places do not disagree. Say that explicitly; do not "simplify" either side on the strength of the other.
- [ ] **Step 2: Mirror `observed_build`'s shape where it applies** (nullable, new migration file, entity, mapper) **but diverge deliberately where it does not**: `observed_build` is machine-written and absent from every write DTO; this one belongs in `NewPlatform`, `PlatformPatch` and the REST request type. State that divergence rather than copying the read-only shape by habit.

  **`PlatformPatch` needs three states, and a plain `Option<String>` gives only two.** Per Step 1's write B, legacy's patch distinguishes *absent* (keep the stored branch) from *explicitly emptied* (clear it), and `Option<String>` collapses those into one `None`. Whatever `PlatformPatch` does for the other nullable fields, check it against this: if the existing patch shape cannot express "clear it", then either adopt the shape that can (e.g. `Option<Option<String>>`, or the sentinel the codebase already uses) **or** record the dropped capability as a deliberate, stated divergence from legacy. **Do not let the two-state default silently decide it** — an operator who clears the field and finds the old branch still pinned gets no error and no warning, which is the same failure class this whole task exists to fix. Check how the shipped qa-environments patch path handles the other nullable columns before choosing.
- [ ] **Step 3: Verify the schema, not the build.** `cargo build` proves nothing about a SeaORM entity. Round-trip the column through a real INSERT/SELECT, and break-test by renaming it in the DDL only.
- [ ] **Step 4: Wire qa-runs in the same commit. This step was inverted on 2026-08-14 — read why.**

  It originally said *"Do not touch qa-runs; leave the wiring to a Task 13 follow-up."* **That is now impossible.** Task 13's code-quality review found the intended tripwire, `platform_defaults_are_not_available_yet`, was **tautological**: `platform_default_branch` was `const fn -> Option<&str> { None }`, so it asserted `None == None` and would have stayed green no matter what the SDK gained, and `launch_tests.rs:218`'s claim that "when the field lands, this test is what fails" was false. Task 13's fix round 2 (`a2dc6e29`) repaired it the way the review proposed — `platform_default_branch` now **destructures `TargetPlatform` exhaustively**, every binding `_`-prefixed rather than `..`, "because `..` is precisely what would make this silent again."

  **Consequence: the moment you add `default_branch` to the SDK model, `qa-runs` stops compiling** with `E0027: missing field`, pointing at `platform_default_branch`. That is the tripwire working exactly as designed — and it means a qa-environments-only commit would leave the workspace non-compiling. So:

  - **This task owns the minimal qa-runs wiring too, in the same commit**: add the field to the destructuring, return it from `platform_default_branch`, confirm `resolve_branch`'s middle tier now reads it, and **delete `platform_defaults_are_not_available_yet`** (its comment says so itself).
  - Keep the qa-runs change **minimal** — the three-tier chain and its tests already exist and are already correct (Task 13 implemented and tested the full chain). You are connecting a source, not writing a rule.
  - The now-live middle tier means Task 13's `the_platform_default_wins_over_the_repository_default` and `a_blank_platform_default_falls_through_to_the_repository_default` stop being hypothetical. **Verify they still pass against a real platform value**, and check whether either was written in a way that only passed because the tier returned `None`.
  - Gate **all four packages** (`qa-environments`, `qa-environments-sdk`, `qa-runs`, `qa-runs-sdk`), since you are now touching both gears.
- [ ] **Step 5: Verify.** qa-environments shipped at **61 tests**; report the new count and confirm none regressed. Gate **all four packages** — `qa-environments`, `qa-environments-sdk`, `qa-runs`, `qa-runs-sdk` — with per-command exit codes checked individually. (This step said "both packages" until 2026-08-14; it was not updated when `cfda6dc4` inverted Step 4 to include qa-runs, and contradicted Step 4's own last bullet.)

---

### Task 13c: Restore the custom-plan `plan.yaml` exclusivity tier (added 2026-08-14 by user decision)

Raised by Task 13's spec review; the user chose on 2026-08-14 to **widen the SDK rather than accept the gap**.

**The divergence being closed.** A custom plan composed of a plan whose `plan.yaml` declares `exclusive: true`, with every test file silent, resolves **exclusive / `tier=Plan`** in legacy and **parallel / `tier=TestMeta`** here. A destructive plan silently loses its platform-to-itself guarantee — no error, no warning. Same failure class as Task 11b (reserved names) and Task 13b (`default_branch`): a tier of a resolution chain with no source in the ported model.

**Root cause.** `qa_catalog_sdk::CustomPlan.files` is `Vec<(Uuid, String)>` — `(repo_id, path)` pairs (`qa-catalog/qa-catalog-sdk/src/models.rs:96-105`, doc'd `:99`). Legacy's equivalent is `Vec<CustomPlanTest>` where `CustomPlanTest` is `{ plan_id, test_file }` (`manager/src/models.rs:508-527`, `:484-487`). **The port dropped the nested-plan reference**, the only key by which a nested plan's `plan.yaml` can be found. Consequence already recorded in `launch.rs`: `exclusivity::combine_nested` has **zero production callers**.

> **The field is `plan_path: Option<String>`, NOT `plan_id: Option<Uuid>`. This section said `plan_id` until 2026-08-14 and that type is unimplementable in this port** — corrected after execution proved it. Legacy has a `plan_id` because it persists plans; **this port does not**. `qa_catalog_sdk::Plan` is *"not persisted — materialized on read"*, there is **no `qa_plans` table** anywhere in `m20260812_000002_initial` (verified: zero occurrences), and `get_plan` is keyed `(repo_id, branch, path)`. A `Uuid` here would reference nothing. The nested plan is identified by its **path inside the entry's existing `repo_id`**, which is also why this adds no new cross-resource reference and no new tenancy surface.

**What legacy actually does** — `manager/src/services/exclusivity.rs:530-570`. Read it in full; it is this task's specification:

1. `group_tests_by_plan(&plan.tests)` groups entries **by `plan_id`**, not by repo. **The port must key on `(repo_id, plan_path)`, not on the path alone** — and this is the *fourth* defect in this paragraph of exactly the same family, so read it before transplanting anything else from legacy's shape. Legacy's `plan_id` is unique across the whole plans directory **and carries its repository with it**; a `plan.yaml` *path* is unique only within one repository. Keying on `plan_path` alone merges two repositories' `plans/smoke.yaml` into one group and scans each one's files against the other's repository. The shipped implementation gets this right and documents it (`launch.rs:449-455`), with `each_nested_plan_is_resolved_in_its_own_repository` registering the same path in two repos with opposite declarations. **The lesson, since it has now cost four defects here: when a legacy identifier is replaced, check what the original identifier was carrying implicitly, not just what it was called.**
2. Per nested plan: if `info.plan.exclusive` is `Some(declared)` → `plan_flags`. **Else** `scan_test_meta(state, info, &files, &[], &[], repo)` → `test_meta_flags`. Note the two empty slices: **a custom plan has no tag filter of its own**, as its own comment says. Do not thread the launch's include/exclude here.
3. The scan's repo is `info.repo_id` — **the nested plan's** repository, not the entry's. A custom plan can span repositories and each nested plan's `TEST_META` lives in its own.
4. `unresolved` counts nested plans that could not be resolved and fires its own `tracing::warn!`, with the reason inline: *"say plainly when the answer rests on partial data, so 'this custom plan resolved parallel' is never mistaken for 'every plan it composes was checked and declared parallel'."* **Port this warning** — same operator-visibility obligation as Task 13's unreadable-files warning, for the same reason.
5. The two flag vectors go to `combine_nested`.

- [ ] **Step 1: The payload-shape problem — solve this before writing anything else.** `qa_custom_plans.files` is a **JSONB** column (`qa-catalog/src/infra/storage/migrations/m20260812_000002_initial.rs:115`, and again in the MySQL and Postgres blobs at ~`:187` and ~`:267`), decoded by `uuid_pairs_from_json` → `serde_json::from_value::<Vec<(Uuid, String)>>` (`qa-catalog/src/infra/storage/mapper.rs:119-126`). Therefore:

  - **No DDL change is needed.** But the reason stated here until 2026-08-14 — *"the column is already JSONB in all three dialects"* — is **false**: it is `JSONB` on Postgres (`:115`), `JSON` on MySQL (`:195`), and plain **`TEXT`** on SQLite (`:271`), which is the only one any test executes. The *conclusion* survives (all three store the payload as text and need no width or type change), but do not repeat the premise. **Do not add a migration for the column.**
  - **But every existing row holds the two-element shape**, and a naive widening makes `from_value` fail, surfacing as `DomainError::Internal("corrupt qa_custom_plans.files …")` — a fail-closed break of every stored custom plan.
  - **`cargo build` proves nothing here.** This is a *serialized payload*, exactly the case the legacy-verification protocol names. Only a test that decodes a **stored old-shape row** is signal.

  Choose and justify one: a struct entry with `#[serde(default)] plan_id: Option<Uuid>`; an untagged enum accepting both shapes; or a data migration rewriting stored rows. **Name the rejected alternatives.** Whichever you pick, write a test that decodes a literal old-shape payload (`[["<uuid>","path.py"]]`) and asserts it still loads, and break-test it by feeding the new shape to the old decoder. **Beware the known harness bug: `perl -0p` without `/g` patches only the first dialect blob** — there are three.

- [ ] **Step 2: Widen the SDK model.** `CustomPlan.files` and `NewCustomPlan.files` (`models.rs:96-113`). `plan_id` is **operator-supplied**, so unlike `observed_build` it must reach the write path and the REST DTOs (`api/rest/dto.rs:207`, `:216`). Mirror Task 13b's lesson: state the read-only-vs-writable divergence rather than copying a shape by habit.

- [ ] **Step 3: Thread it through qa-catalog.** Mapper (`mapper.rs:76`), the SeaORM repo (`custom_plans_sea_repo.rs`), the service (`domain/service/custom_plans.rs`), the REST handlers (`api/rest/handlers/custom_plans.rs`). **Security rules apply:** no unscoped queries, PEP before every repo call, one scope per resource type, filter-first, unique violations via `ScopeError::is_unique_violation()`. Do not widen any unique index without a tenant prefix.

- [ ] **Step 4: Consume it in qa-runs — the actual fix.** In `launch.rs`, replace the union-of-all-files resolution with legacy's shape: group by **`(repo_id, plan_path)`** — *not* `plan_path` alone; see the warning below — consult each nested plan's `exclusive`, fall back to a per-plan `TEST_META` scan **with no tag filter**, count unresolved plans and warn, then call **`exclusivity::combine_nested`** — dead in production since Task 12, and the function this task exists to revive. **Preserve Task 13's obligation: only pass files that were actually read**; a default `FileMeta` votes `Some(false)` and turns `Default` into `TestMeta`-false (`exclusivity.rs:184`). That guard is break-test-proven; do not regress it.

- [ ] **Step 5: Delete the divergence table** and the "zero production callers" note on `combine_nested`. (The citation here said `launch.rs:548-591` until 2026-08-14; the table was actually at `:500-542`, and `548-591` was unrelated `plan_timeout_seconds`/`scan_include` documentation. **Locate it, don't trust a range.**) The review names that table as this task's **acceptance criterion**: if any part of it is still true, keep exactly that part and say which.

  **Outcome, recorded 2026-08-14:** two of the three rows became false; **one survives, narrowed** — an entry whose `plan_path` is `None` still yields no plan tier and still resolves from `TEST_META` alone. Every row stored before the field existed decodes to exactly that, so **the fix is not retroactive, and no backfill can make it so**: recovering `plan_path` means guessing which `plan.yaml` lists a given file when several legitimately may, and guessing wrong in the permissive direction is the exact failure this field exists to prevent.

- [ ] **Step 6: Verify.** qa-catalog is at **153 tests** (the plan and the continuation prompt both said 154; measured 2026-08-14); qa-runs at **340 lib + 1 integration** as of `535a7bb9`. Report real counts; predict nothing. Gate with **per-command exit codes checked individually**:

```bash
cargo build  -p qa-catalog -p qa-catalog-sdk -p qa-runs -p qa-runs-sdk
cargo clippy -p qa-catalog -p qa-catalog-sdk -p qa-runs -p qa-runs-sdk --all-targets -- -D warnings
cargo test   -p qa-catalog -p qa-runs
cargo fmt --check -p qa-catalog -p qa-catalog-sdk -p qa-runs -p qa-runs-sdk
```

  Break-test three-point with sha256 (baseline green, mutant red, restored byte-identical and green) and **verify the failure kind by hand** — a compile error banked as "guard live" is how the eighth harness bug nearly landed, and a *green* mutant is not automatically a harness bug (Task 13's quality review found a real inert guard that way).

**Note:** this is the first task since Task 2 to modify **shipped qa-catalog**. Its 153 tests are the regression surface. Remember qa-catalog's own post-mortem lesson: *legacy-truth checkpoints must cover every frozen contract, not just the ones the plan names.*

---

### Task 13d: Close qa-environments' storage-mapper coverage class (added 2026-08-14 by coordinator decision)

**Do this before Task 14.** Task 14 is the concurrency core and reads and writes platform leases through the very functions this task covers.

Found by Task 13b's focused code-quality review, which was asked whether the mapper fix it had just landed closed the hole **or moved it**. It closed one instance and found the class open twice more, in the same file. **These are pre-existing coverage gaps in shipped qa-environments, not defects introduced by 13b** — the code writes correctly today; nothing *proves* it keeps doing so. This is the third appearance of the pattern the reviews now call **"structurally unable to detect"** rather than vacuous: the test is not wrong, its wiring cannot see the failure.

`infra/storage/mapper.rs` is the only conversion module in the gear with functions that have **no test at all**. For contrast, `api/rest/dto.rs` has a test per conversion, all six.

- [ ] **Step 1: The lease round trip — the one that matters.** Mutating `state_to_columns` (`infra/storage/mapper.rs:105`) to write `LeaseState::HeldExclusive` as `mode: "parallel"` leaves the gear **76 passed, 0 failed**. Trace the consequence before you write the test, so the test's name can carry it: `lease_to_state` (`:84`) maps `("parallel", [h])` → `HeldParallel { [h] }`, and `decide_acquire` (`domain/lease.rs:34-43`) grants `HeldParallel + Parallel → Acquired`. So a second run **joins a platform another run holds exclusively** — which is precisely the answer `lease_to_state`'s own doc calls *"the single most dangerous answer this function can give"*, and for which the **read** half carries four tests and a fix-history section. The **write** half is asserted by nothing. `state_to_columns` is on the live write path (`infra/storage/leases_sea_repo.rs:63`, feeding both write branches at `:74-75` and `:95-96`).

  Write a `state_to_columns` → `lease_to_state` round-trip test over **all three** variants.

  **Do not repeat this plan's own mistake about *why* the gap survived.** This section originally said the mock-based `leases_tests`/`variables_tests` are the reason, quoting *"never touch the database"* (`domain/service/test_support.rs:48-51` — note **two** `test_support.rs` files exist in this gear; it is the one under `domain/service/`). That is incomplete and was corrected by execution: **`tests_tenant_scoping` is DB-backed and does write lease rows through `state_to_columns`.** It cannot see the mode because it asserts *downstream effects* — `delete_platform`'s guard is `!matches!(lease.state, LeaseState::Free)` (`domain/service/platforms.rs:200`), so an exclusive hold written as parallel is still not `Free`, still blocks the delete, and still releases cleanly. Measured contrast: writing `HeldParallel` as `"free"` reddens `lease_scoped_by_tenant`, and an empty holder list for `"exclusive"` reddens `delete_leased_platform_blocked_until_release`. **Exclusive-as-parallel is the only substitution that is neither corrupt nor visibly free, which is exactly why it survived.**

- [ ] **Step 2: The variable mappers.** `platform_var_to_sdk` (`mapper.rs:29`) and `pipeline_var_to_sdk` (`:40`) have no test. Mutating `platform_var_to_sdk` to `value: String::new()` leaves the gear green — every platform and pipeline variable could come back with an empty value unnoticed. Cover both, and note the one asymmetry that distinguishes them (`platform_id: Some(..)` vs `None`), since that is the field a copy-paste between them would get wrong.

- [ ] **Step 3: Make the guard's own claim true.** `mapper.rs:163` asserts *"replacing **any one of the ten reads** below with a literal turns this red."* **False**: replacing `name: m.name` with a literal leaves the gear green. It holds for the five *nullable* reads and fails for the six non-nullable ones, because `platform_to_sdk_preserves_absent_optionals` builds its row with `..full_platform_row()` and so shares every non-optional value with the populated fixture. **Prefer making the sentence true** — give the absent-optionals row its own distinct `name` / `kubeconfig_credstore_ref` / `available` / `id` / timestamps instead of `..full_platform_row()`, about three lines — over narrowing the claim to the nullable five. Break-test every one of the ten afterwards and report the matrix.

- [ ] **Step 4: Verify.** qa-environments is at **77 tests** (61 on ship → 72 → 76 → 77). Report the real count. Gate `qa-environments` and `qa-environments-sdk` with **per-command exit codes checked individually**. Every new test must be break-tested three-point with sha256 (baseline green, mutant red, restored byte-identical and green), with the failure **kind** verified by hand — a green mutant here is the *finding*, not a harness artefact, which is exactly how this task's contents were discovered.

**Do not** widen scope into qa-runs or into `domain/lease.rs`'s decision logic: the read half and the decision logic are already well covered. This task is the **write** half and the two untested conversions.

---

### Task 14: Admission and dispatch — the concurrency core

This is where every concurrency lesson from the parity pass applies, and where a mistake is a cross-tenant or cross-run correctness bug rather than a wrong number on a page.

**Files:**
- Create: `qa-runs/src/domain/service/{admission.rs,dispatch.rs}`
- Create: `qa-runs/src/domain/service/{admission_tests.rs,dispatch_tests.rs}`
- Modify: `qa-runs/src/domain/service/mod.rs`, `qa-runs/src/domain/error.rs`

**Owns:** the above.

- [ ] **Step 1: Legacy check — the lock's exact scope, which is the whole game.** Read `run_queue.rs:566-588` (`PlatformLocks`), `:608-680` (`admit`), and `run_dispatcher.rs:572-639` (`drain_platform`). Confirm and cite **five** things:
  1. Admission is serialised **per platform**, because two concurrent launches would otherwise both observe an idle platform and both start — `run_queue.rs:566-572`.
  2. The lock is held only across **the decision and the row insert** — **never** across the submit, which includes a repo sync and a bundle build and "would otherwise serialise every launch on a platform for minutes" — `run_queue.rs:570-572`, and the dispatcher's mirror at `run_dispatcher.rs:618-620`.
  3. The depth check is inside the lock and **before** the occupancy read — `run_queue.rs:633-655`.
  4. The dispatcher claims rows **inside** the lock "so a concurrent launch sees these as occupancy", and dispatches **outside** it — `run_dispatcher.rs:606-620`.
  5. Locks are per platform and different platforms never block each other, and the source system has two tests pinning exactly that, including a **bounded** one that fails loudly rather than hanging CI — `run_queue.rs:1412-1449`.

- [ ] **Step 2: Legacy check — the tick's ordering, which is also load-bearing.** Read `run_dispatcher.rs:344-415` (`run_dispatch_cycle`). Confirm and cite:
  1. **The TTL sweep runs first, before every early return.** Not merely before the cap check: the two early returns fire in exactly the situations where queued rows pile up unnoticed — a cluster at `max_concurrent_runs`, and an unreadable executor (during which occupancy fails safe to busy so every launch queues and nothing drains). "The ticket requires that a run never disappear silently, so the sweep must not be downstream of either" — `run_dispatcher.rs:345-351`, `:495-508`.
  2. The tick **skips entirely** if the executor listing fails — it does not treat the result as empty — `:353-359`.
  3. Claims are read **after** reconciliation so released ones are not counted — `:376-377`.
  4. The global budget is **threaded across platforms**, because the batch planner counts one platform only — `:406-414`.
  5. Each tick runs in its **own task** so a panic costs one tick rather than the whole dispatcher — and the reason is stated: an unsupervised loop that dies leaves admission queueing rows nothing will ever start, and because admission is strict FIFO the first such row blocks every later launch on that platform indefinitely — `:321-340`.
  6. The interval is **15 s** in legacy and the reason is given: because an admissible run dispatches inline, tick latency only ever delays runs that are already queued — `:306-311`, floored at 5 s at `:313`.

     **This cross-check FIRED and is now resolved — do not re-open it.** `cpt-cf-qa-nfr-dispatch-latency` (PRD:622) requires a queued run to start within **10 s p95** of its platform becoming free; a 15 s sweep gives p95 ≈ 14 s and **cannot meet it**. DESIGN originally prescribed a release-notification wake with the sweep as fallback, but **no such notification exists** in this gear or anywhere in this plan, so Task 14 implemented only the fallback. **User decision, 2026-08-14: poll at 5 s** — legacy's own documented floor — giving p95 ≈ 4.75 s and meeting the NFR with no new mechanism. DESIGN's row and DECOMPOSITION 2.3's follow-up register are both amended. Task 16 owns `dispatcher_interval_seconds` and **must default it to 5**, not 15. Legacy's argument for 15 s is about the common case (an admissible run dispatches inline); the NFR measures the p95, which is the queued case.

- [ ] **Step 3: Implement `admission.rs`.**

  - A `PlatformLocks` registry: `Arc<Mutex<HashMap<Uuid, Arc<Mutex<()>>>>>`, keyed by platform id. Copy the source system's shape (`run_queue.rs:573-588`) — an outer lock to hand out per-platform locks, held only for the lookup.
  - `admit()`: acquire the platform lock → read `queued_depth` → **check the depth limit first** (a rejection writes no row, so do not pay for an occupancy read you will discard) → read occupancy → `queue::decide_admission` → insert the row (`dispatching` with `dispatched_at`, or `queued`) → release.
  - **Occupancy comes from the lease, and a failed read is `Occupancy::Exclusive`.** This is the fail-safe direction ported from `run_dispatcher.rs:237-252`: an unreadable oracle means we assume the platform is busy, so a run queues rather than trampling an exclusive run. Log it at ERROR with the platform id, as legacy does.
  - **The lease is acquired inside the lock, for a `Dispatch` decision only.** `acquire_lease(ctx, platform_id, run_id, mode)` returning `Busy` overrides the planner and the row is inserted `queued` instead — the lease CAS is the source of truth and the planner is advisory (see `domain/queue.rs`'s module docs). Assert this ordering in a test.
  - **The global cap is checked before the lock**, not inside it: it is cluster-wide, so serialising it per platform buys nothing, and legacy checks it at the top of `launch` (`run_dispatcher.rs:80-81`).
  - **Ownership precheck before the child insert.** `secure_insert` validates only the `tenant_id` column and cannot verify that the referenced `run_id` belongs to that tenant (Task 10 Step 7's documented boundary). Resolve the run under a properly-derived `qa.run`/`get` scope first, then insert the queue row. Cite the DESIGN §3.7 paragraph.

- [ ] **Step 4: Implement `dispatch.rs`.** Two entry points and one tick.

  - `dispatch_one(ctx, queue_id, run_id)` — the single path from a claimed row to a started execution, used by **both** the inline admission path and the tick, so there is one submission path with no bypass branch (`run_dispatcher.rs:1-6`). It does the expensive work: resolve the branch's content by force-syncing each repo group (`sync_repo` with `SyncRequest { branch, force: true }` — Task 2's widened trait), `create_bundle` per group, build the `ExecutionNode` list, assemble the environment, `RunExecutor::start`, then `set_execution_ref` + `mark_running` + transition the run to `Running` + publish `qa.run.started`.
  - **The bounded retry for decision D4** goes here: if discovery for a group returns **empty** immediately after this call's own force-sync, retry that group's read **once**. Comment it with the D4 reasoning and the DECOMPOSITION 2.2 cross-reference, and make it exactly one retry — a loop would turn a genuinely empty plan into a hang.
  - **A failed submit must release the claim.** `mark_failed` on the queue row and a terminal state on the run. Legacy's comment on the inverse case is worth porting too: if the submit *succeeded* but recording it failed, returning `Err` would be a lie and could trigger a caller retry that double-submits — so log at ERROR and return `Ok`, accepting a stale claim that the next tick's reconciliation releases (`run_dispatcher.rs:31-45`).
  - `run_tick()` — in the legacy order, which is not negotiable: **(1)** TTL sweep, **(2)** executor `list_active` (skip the whole tick on error), **(3)** claim reconciliation, **(4)** re-read claims and compute the committed active count, **(5)** global-cap check, **(6)** enumerate platforms with queued rows, **(7)** drain each with a threaded budget. Then the two additions this gear needs and legacy did not have: **(8)** the control-plane timeout sweep, and **(9)** nothing else — resist adding anything to the tick that the guide does not require.
  - **Timeout enforcement** (`cpt-cf-qa-fr-runs-timeout`): `list_timeout_candidates` returns `(run_id, tenant_id)`; for each, mint `system_actor::for_timeout_enforcement(tenant_id)`, call `RunExecutor::cancel`, transition to `TimedOut`, release the lease and the claim. **Per-tenant context per write** — review lesson 6.
  - **Every background write is bound to the row's own tenant.** Enumerate with a nil-tenant factory, write with a tenant-bound one. A nil-tenant write is *denied*, so getting this wrong makes the dispatcher inert rather than wrong — which is worse than it sounds, because it fails quietly.

- [ ] **Step 5: The dev-deployment caveat, decided now rather than discovered later.** The reference dev stack's `static-authz` plugin derives its decision purely from the resolved tenant and denies a nil tenant outright; its config is only `{vendor, priority}`, so there is no per-subject grant to express. **Every gear system actor is therefore denied in the reference deployment.** qa-catalog handled this by disabling its refresher in the dev config and letting the GC log one actionable warning per pass — but a GC being inert is a much smaller deal than **the dispatcher being inert**, which means queued runs never start and, because admission is strict FIFO, the first queued row blocks every later launch on that platform.

  Two options, and this plan takes the second:
  - Disable the dispatcher in the dev config, as qa-catalog disabled its refresher. **Rejected**: it makes the reference deployment's launch path silently one-shot — a run that queues never starts, with no error anywhere.
  - **Chosen:** keep the dispatcher enabled and add a `log_task_failure`-style helper that maps `DomainError::Forbidden` to a single actionable WARN per pass naming the remedy (grant the `qa_runs.system` subject the scopes the tick needs, or set `dispatcher_enabled: false`), exactly as `qa-catalog/src/gear.rs:400-419` does. Plus a `dispatcher_enabled` config knob (Task 16) so an operator can turn it off deliberately rather than by accident.

  **Whether the dev stack should instead grow a policy plugin that can grant a system subject is a real decision and it is not this plan's to make** — record it as a follow-up in DECOMPOSITION 2.3's tracked-follow-ups register (add that subsection; 2.3 has none yet) and raise it with the reviewer.

- [ ] **Step 6: The concurrency tests, written by breaking the fix.** For each race below: write the test, **temporarily remove the fix, watch the test fail, restore it**, and state in the task report that you did. "The test passes" is not evidence.

```text
Locking
- the_same_platform_maps_to_the_same_lock                     (Arc::ptr_eq)
- different_platforms_do_not_share_a_lock                     (!Arc::ptr_eq)
- holding_one_platforms_lock_does_not_block_another           (tokio::time::timeout — bounded, so a
                                                               global lock FAILS rather than hangs CI)
- two_concurrent_exclusive_launches_produce_one_start_and_one_queued
- the_lock_is_released_before_the_submit                      (a submit that blocks on a barrier must
                                                               not prevent a second admission)
Fail-safe directions
- an_unreadable_lease_makes_the_platform_read_as_busy         (queue, never dispatch)
- a_busy_lease_overrides_an_admit_decision_from_the_planner   (the CAS is the source of truth)
- a_failed_executor_listing_skips_the_whole_tick              (no claim is released)
- a_failed_submit_releases_the_claim_and_fails_the_run
- a_submit_that_succeeded_but_could_not_be_recorded_returns_ok (and leaves a claim for reconciliation)
Tick ordering
- the_ttl_sweep_runs_before_the_cap_check                     (rows expire while the cluster is full)
- the_ttl_sweep_runs_even_when_the_executor_listing_fails     (the case that needs it most)
- claims_are_counted_after_reconciliation_not_before
- the_global_budget_is_threaded_across_platforms              (two platforms, cap 6, active 4 -> 2 total,
                                                               not 2 each — the overshoot legacy warns about)
- an_exclusive_queued_row_drains_one_per_tick
- a_parallel_queue_drains_in_one_tick
Crash recovery
- boot_recovery_fails_rows_left_mid_dispatch
- boot_recovery_leaves_rows_that_have_an_execution_ref
- a_young_mid_build_row_survives_a_tick                       (the orphan-timeout guard; removing it
                                                               momentarily releases a claim, which is
                                                               when a run could start beside an exclusive one)
- a_reconciled_claim_releases_its_lease_and_frees_the_platform
Tenancy in background writes
- the_dispatcher_writes_under_the_rows_own_tenant             (a nil-tenant write is denied)
- a_forbidden_system_actor_logs_once_per_pass_and_does_not_kill_the_ticker
- the_timeout_sweep_cancels_under_the_runs_own_tenant
```

- [ ] **Step 7: Verify.** Run the two filters **separately** — `cargo test -p qa-runs admission` and `cargo test -p qa-runs dispatch` — or `cargo test -p qa-runs -- admission dispatch`. (**Corrected 2026-08-14:** the single-invocation form this step originally gave, `cargo test -p qa-runs admission dispatch`, does **not run** — cargo accepts one positional filter and errors with `unexpected argument 'dispatch' found`. A reviewer following it literally saw an error, not a count.) The predicted **25** was wrong by more than 2×: the real figure is **56** new tests (20 admission, 36 dispatch). Report real counts. Report explicitly which of the concurrency tests you verified by removing the fix.

- [ ] **Step 8: Commit.** Two commits — `feat(qa-runs): per-platform admission with lease-backed occupancy` and `feat(qa-runs): dispatcher tick with crash recovery, TTL sweep, and timeout enforcement`.

---

### Task 15: Ingest, cancel, re-run, and the queue operator actions

> **A completion guard this task must decide on — raised by Task 9's security review, 2026-08-13.** The `qa_runs` count columns are `NOT NULL DEFAULT 0`, and `derive_terminal_state` reads only `failed > 0 || skipped > 0`. So an all-zero count row plus a `Succeeded` executor outcome yields `Succeeded` — **a run whose result ingest was dropped, delayed, or never delivered is byte-identical to one that passed cleanly.** That sits directly against the invariant `state_machine.rs` states in its own words: *"a run whose work never ran must never read as passing."*
>
> Legacy could neither have this bug nor fix it: it stored no counts and recomputed them per query, so "no rows" and "no ingest" were one fact there too. This is therefore net-new exposure created by denormalizing the counts, not a parity gap.
>
> Task 9 investigated a schema-level fix and **recommends against one**, with the reasoning: nullable counts (`NULL` = not ingested, `0` = ingested and empty) would require changing `qa_runs_sdk::RunResult`'s five `usize` fields — a shipped SDK contract *and* a shipped pure domain module that consumes it by value — and a `results_ingested_at` column would be schema no consuming task knows to write or read. Both were judged worse than the defect.
>
> **The recommendation is a completion guard here:** refuse to record `Succeeded` for `total == 0` against a non-empty plan. It needs no new column and sits exactly where the invariant is stated. Take it, or make the case for the alternative — but do not leave the case unhandled.

> **A composition trap this task will hit at runtime — found by Task 7's code review, 2026-08-13.** The ingest pipeline is written here as `derive_terminal_state → reconcile_recorded_state → conditional update_state`, and Task 10 maps a zero-row conditional update to `IllegalTransition`. But `reconcile_recorded_state(Failed, Succeeded) == Failed`, and `can_transition(Failed, Failed) == false` — terminal states refuse *every* transition including self, deliberately, so a duplicate completion event cannot rewrite `finished_at`. Composed naively, **a late or duplicate `Finished` event on an already-terminal run yields `IllegalTransition` (HTTP 409) instead of the silent no-op that is obviously correct**, and executors retry.
>
> The rule: **when `reconcile_recorded_state` returns a value equal to the recorded state, there is nothing to write — no-op and return success. Never feed that pair to `can_transition`.** "Rejected" is right at the guard layer and wrong at the ingest layer. `can_transition`'s own doc says "a duplicate completion event is therefore rejected by this guard rather than silently rewriting `finished_at`", which is true of the guard and must not be read as the ingest contract.
>
> **Two domain functions this task and Task 13 must use rather than reimplement**, neither of which the plan previously named: `can_transition` is the **mandatory** pre-check before any `update_state` — writing the update directly bypasses the whole state machine — and `is_terminal` is what makes cancelling an already-terminal run idempotent rather than an error.

**Files:**
- Create: `qa-runs/src/domain/service/{ingest.rs,runs.rs,ingest_tests.rs,runs_tests.rs}`
- Create: `qa-runs/src/infra/logs/{mod.rs,broadcast.rs}`
- Modify: `qa-runs/src/domain/service/mod.rs`, `qa-runs/src/domain/error.rs`

**Owns:** the above.

- [ ] **Step 1: Legacy check — re-run's exclusivity inheritance, which is the subtle one.** Read `manager/src/routes/runs.rs:958-967` (and the identical comment repeated at `:1008-1018` and `:1071-1081` — legacy has it three times because three intents needed it). Confirm the rule and *why*: **inherit upward only.** `Some(true)` replays the original decision, which is the point of re-run. A stored `Some(false)` is deliberately **not** pinned, because it would land on the launch tier — which outranks everything — and so would suppress a `TEST_META` or `plan.yaml` declaration added since the original run, relaunching as parallel a test that has since been marked destructive. Letting `false` fall through re-resolves it from the current tiers, so the only drift is an unnecessary exclusive run (throughput) rather than a corrupted platform. The implementation is `run.resolved_exclusive.then_some(true)` — **corrected 2026-08-13 (Task 4 review)**: this said `run.exclusive.filter(|&e| e)`, an `Option<bool>` method carried over from legacy's annotation codec, but the shipped `Run.resolved_exclusive` is a plain `bool`. No information is lost; the upward-only rule needs only the resolved boolean. Guide lines 205-208 state the same rule from the user's side.

- [ ] **Step 2: Legacy check — the rest of re-run and cancel.** Read `routes/runs.rs:904-1088` and `:1092-1106`. Confirm: parameters are **re-validated** at re-run ("cheap defense-in-depth against stale/tampered rows and a reserved list that may have grown since", `:920-923`); the original branch is reused from `source_ref` falling back to `test_version` (`:927-933`); a run missing plan metadata is a `BadRequest` (`:910-918`); and stopping a started run is a separate operation from cancelling a queued row (`:1092-1106` vs `run_queue.rs:403-421`). Then read `run_queue.rs:403-421` once more and cite the guard in the code: the `state = 'queued'` condition is **the safety property** — a `dispatching`/`running` row holds a claim an in-flight execution depends on, and dropping it would let a new run be admitted beside an exclusive one.

- [ ] **Step 3: Legacy check — force start's deliberate asymmetry.** Guide lines 116-120: force start ignores what occupies the platform, **including an in-flight exclusive run**, but does **not** override `max_concurrent_runs` — that still answers 429 and leaves the row queued. "The asymmetry is intentional: overriding a platform is a testing decision you may want to make, while overriding cluster capacity can wedge the whole namespace for everyone." Port it, and put that sentence in the code comment.

- [ ] **Step 4: Implement `ingest.rs`.** Consumes an `ExecutionEvent` stream for one run and, per event: normalise the status; `upsert_test_result` (delete-then-insert, the legacy dedupe); `add_result_counts` with the **delta** (never a read-modify-write — concurrent events would lose counts); fan the log line out to the SSE broadcaster; publish `qa.test.result`. On `Finished`: `derive_terminal_state(outcome, counts, node_failure)` → `reconcile_recorded_state(recorded, derived)` → conditional `update_state` → release the lease → `mark_done` on the queue row → publish `qa.run.finished`.

  **Note the ordering of the last three.** Release the lease *before* marking the queue row done. (**Corrected 2026-08-14 (Task 15 + its spec review):** this said "only if the row is the lease holder's record", which is not a reason — it names a condition and stops. The real reason is crash recovery: row-first then a crash leaves a lease nothing can find, because `all_claims` no longer returns the row; lease-first then a crash leaves a claim reconciliation reclaims. Same conclusion, sound argument. `ingest.rs`'s `finish` doc carries the corrected version.) Get this wrong and the platform stays held. Assert the resulting invariant directly in a test: *after a run reaches a terminal state, the platform's lease no longer names it*.

- [ ] **Step 5: Implement `infra/logs/broadcast.rs`.** A `tokio::sync::broadcast` channel per active run, in a `HashMap<Uuid, Sender<String>>` behind a lock, with a bounded capacity and lagged-receiver handling that sends an explicit gap marker rather than silently dropping lines. Reap a run's channel when it terminates. **Bounded, not unbounded**: an unbounded channel with a slow SSE client is a memory leak per run, and the source system's WebSocket handler polls a fixed-size log rather than buffering (`routes/runs.rs:1477-1527`).

- [ ] **Step 6: Implement `runs.rs`** — `get`, `list`, `get_result`, `cancel`, `rerun`, `cancel_queued`, `force_start`. Each with PEP → scope → repo, one scope per resource type. `cancel` branches on the run's current state: a queued run drops its row and releases nothing (it holds nothing); a dispatching/running run is cancelled through the executor and **releases nothing yet**. (**Corrected 2026-08-14 (Task 15 + its spec review):** this said the live-cancel branch "releases its lease", which is wrong and was not implemented. At cancel time the executor has been *asked* to stop, not observed to stop; releasing then frees the platform under a winding-down execution — the same hazard `cancel_queued`'s `state = 'queued'` guard exists for one table over. Legacy's `stop_run` (`routes/runs.rs:1092-1106`) writes no state and releases nothing, so this is also the parity behaviour. The lease is released on *evidence*, by ingest's `Finished` branch and by the tick's reconciliation; the review traced both drains — `reconcile_claims` has no terminal-run guard and keys off `list_active` ceasing to list the execution, and the no-`execution_ref` sub-case drains through `fail_orphan`, whose `release_lease` is unconditional. Exposure is bounded by one tick, in the fail-safe direction.) `rerun` rebuilds a `LaunchRequest` from the stored run and calls the launch service — **one creation path**, which is what keeps a re-run indistinguishable downstream.

- [ ] **Step 7: Tests.**

```text
Re-run
- a_run_recorded_exclusive_reruns_exclusive
- a_run_recorded_parallel_is_re_resolved_not_pinned_parallel   (the upward-only rule; a test marked
                                                                destructive since must become exclusive)
- rerun_re_validates_the_replayed_parameters
- rerun_reuses_the_original_branch
- rerun_of_a_run_missing_its_recorded_branch_is_a_validation_error
                                                                (renamed 2026-08-14: `RunTarget` is total in
                                                                 this port, so "missing its target" is
                                                                 unrepresentable; the field that can be
                                                                 missing is `test_version`)
- rerun_goes_through_the_same_launch_path                       (asserted on the recording admitter)
Cancel
- cancelling_a_queued_run_drops_its_row_and_never_calls_the_executor
- cancelling_a_running_run_calls_the_executor_and_releases_the_lease
- cancelling_a_terminal_run_is_idempotent
- cancel_publishes_the_prior_state
Queue operator actions
- cancel_queued_refuses_a_dispatching_row_with_a_conflict
- cancel_queued_of_an_unknown_row_is_a_not_found
- force_start_ignores_an_exclusive_occupant
- force_start_does_not_override_the_concurrency_limit           (the deliberate asymmetry)
- force_start_refuses_a_row_that_is_no_longer_queued
Ingest
- results_are_upserted_idempotently_for_a_repeated_test
- counts_accumulate_from_deltas_across_concurrent_events
- a_finished_event_derives_the_terminal_state_from_the_counts
- a_skipped_result_makes_the_run_failed                          (decision D2, end to end)
- a_late_result_after_a_terminal_state_does_not_reopen_the_run
- a_terminal_run_no_longer_holds_its_platforms_lease             (the invariant from Step 4)
- an_event_for_an_unknown_run_is_a_not_found_rather_than_legacys_202
                                                                (**corrected 2026-08-14 (Task 15 + its spec
                                                                 review).** The citation is real but the
                                                                 inference does not port. Legacy's 202 sits
                                                                 above `// Run row not persisted yet; poller
                                                                 will create it and reconcile.` — its row is
                                                                 written by the poller *after* the fact. Here
                                                                 Task 13's `create` writes the row at launch,
                                                                 so that precondition is gone, and dropping
                                                                 silently would accept another tenant's event
                                                                 and answer success. Legacy is single-tenant
                                                                 and cannot be the oracle for this.)
Logs
- a_subscriber_receives_lines_published_after_it_subscribed
- a_lagging_subscriber_gets_an_explicit_gap_marker_not_silence
- a_terminated_runs_channel_is_reaped
```

- [ ] **Step 8: Verify.** `cargo test -p qa-runs` → the whole suite. The predicted **+27** was wrong by more than 2×, the same miss shape as Tasks 13 and 14: the real figure is **+64** (424 → 488 lib tests, plus the unchanged 1 integration and 1 ignored doctest). **Report real counts; never predict them.** Full gate. Commit as two: `feat(qa-runs): incremental result ingestion and SSE log fan-out` and `feat(qa-runs): cancellation, re-run, and queue operator actions`.

---

### Task 16: REST layer, config, gear bootstrap, registration, and the Phase A close-out

**Files:**
- Create: `qa-runs/src/api/{mod.rs,rest/mod.rs,rest/dto.rs,rest/error.rs}`, `rest/handlers/{mod.rs,runs.rs,queue.rs}`, `rest/routes/{mod.rs,runs.rs,queue.rs}`
- Create: `qa-runs/src/config.rs`, `qa-runs/src/gear.rs`, `qa-runs/src/domain/local_client/{mod.rs,client.rs}`, `qa-runs/src/test_support.rs`
- Create: `qa-runs/src/domain/service/tests_tenant_scoping.rs`
- Modify: `qa-runs/src/lib.rs` (uncomment the remaining `pub mod`s and the `pub use`; **remove every `// Task N:` marker** — no stale marker may survive)
- Modify: `apps/cf-gears-example-server/Cargo.toml`, `apps/cf-gears-example-server/src/registered_gears.rs`, `config/qa-platform.yaml` (**workspace root** — corrected 2026-08-15; `apps/cf-gears-example-server/config/` does not exist and never did)

**Owns:** the above. Does not own the schedule routes (Task 20).

- [ ] **Step 1: `config.rs`.** Mirror `qa-catalog/src/config.rs`'s shape (`#[serde(default, deny_unknown_fields)]` plus a hand-written `Default`). **Every knob must be enforced or deleted** — a declared-but-unused config field is a review finding — and every interval needs a sane floor, because `1` meant a one-second ticker doing live network calls against every repository in every tenant in qa-catalog.

```rust
pub struct QaRunsConfig {
    /// Whether the dispatcher ticker runs at all. Default `true`.
    ///
    /// Exists so an operator can disable the tick deliberately rather than by
    /// accident: the reference dev stack cannot grant a gear system actor, so
    /// the tick is inert there and logs one actionable WARN per pass. Required
    /// by the Task 14 Step 5 cross-check (line 5542) and DECOMPOSITION 2.3.
    pub dispatcher_enabled: bool,
    /// Dispatcher tick interval, seconds. `0` disables the dispatcher.
    ///
    /// **Default 5, not legacy's 15** — user decision 2026-08-14, resolved in
    /// full at the Task 14 Step 5 cross-check (line 5518). Do not re-litigate.
    /// `cpt-cf-qa-nfr-dispatch-latency` demands 10 s p95 for a queued run to
    /// start once its platform frees; a 15 s sweep gives p95 ~14 s and cannot
    /// meet it, and the release-notification wake DESIGN originally paired with
    /// the 15 s sweep is **not built** in this port.
    ///
    /// Legacy's own rationale for 15 is true and does not apply: it argues the
    /// *common* case, where an admissible run dispatches inline so tick latency
    /// only ever delays already-queued runs (`run_dispatcher.rs:306-311`, the
    /// doc comment reading "15 s rather than the 2-5 s an always-queue design
    /// would need"). The NFR measures the p95, which is exactly that queued
    /// case. 5 is legacy's own floor (`run_dispatcher.rs:313`,
    /// `interval_seconds.max(5)`), so this is its floor promoted to default,
    /// not a new number.
    ///
    /// The one cost legacy names that does carry over is per-tick expense —
    /// its `list_workflows()` is an Argo call; here the analogue is the tick's
    /// own reads, and `all_claims` runs **twice per tick**, uncapped and
    /// cross-tenant. Tripling the tick rate is only affordable once Step 1b's
    /// caps land, which is why both are in this task.
    pub dispatcher_interval_seconds: u64,
    /// Seconds a queue row may sit in `dispatching` with no execution
    /// reference before the tick fails it. Must comfortably exceed a
    /// force-sync plus a bundle build — minutes, not seconds. Failing a row
    /// early abandons a live launch and momentarily releases its claim.
    pub orphan_timeout_seconds: u64,
    /// Queue TTL, seconds. `0` disables expiry (guide's Limits table).
    pub queue_ttl_seconds: u64,
    /// Per-platform queue depth. `0` = unlimited.
    pub queue_max_depth: u32,
    /// Cluster-wide concurrent-run cap. `0` = unlimited.
    pub max_concurrent_runs: u32,
    /// Fallback run timeout when neither the launch nor the plan gives one.
    pub default_timeout_seconds: u64,
    /// Ceiling on any resolved timeout, so a plan cannot pin a platform
    /// indefinitely (`cpt-cf-qa-fr-runs-timeout`'s "configurable default
    /// ceiling").
    pub max_timeout_seconds: u64,
    /// SSE log buffer per run, in lines. Bounded on purpose.
    pub log_buffer_lines: usize,
}
```

Defaults, named rather than positional so a field added mid-struct cannot silently shift them: `dispatcher_enabled: true`, `dispatcher_interval_seconds: 5` (**not 15** — see the field's doc comment and line 5518), `orphan_timeout_seconds: 600`, `queue_ttl_seconds: 7200`, `queue_max_depth: 20`, `max_concurrent_runs: 0`, `default_timeout_seconds: 3600`, `max_timeout_seconds: 86_400`, `log_buffer_lines: DEFAULT_LOG_CHANNEL_CAPACITY` (**256, corrected 2026-08-15 — this list previously said 1024**).

**Why 256 and not 1024, decided 2026-08-15.** Two independent defaults for one quantity is the same duplication bug as the two timeout ceilings, so one has to point at the other; the question was only which survives. 256 wins because it is the one with a derivation attached (`infra/logs/broadcast.rs:10-47`: a subscriber stalling for one dispatcher tick at ~15 lines/second loses nothing), where 1024 appeared in this list with no argument at all.

**The trap to avoid when reading that constant now.** Stage 3 added `MAX_LINE_BYTES` (8 KiB, `api/rest/sse.rs:39`), and it is tempting to conclude the byte-unboundedness `broadcast.rs` warns about is closed. **It is not.** `sanitize_line` is applied on the **render** path in `api/rest/sse.rs`; `RunLogBroadcaster` buffers the raw `line: String` as published. So `MAX_LINE_BYTES` bounds the *emitted frame*, not the *resident buffer*, and `broadcast.rs`'s standing conclusion is unchanged: the capacity is a multiplier on an unbounded quantity, and **the ingestion-side cap still belongs to the executor adapter (feature 2.7)**. Do not let the two constants be read as one solved problem.

Floors: clamp `dispatcher_interval_seconds` to ≥ 5 when non-zero (legacy does exactly this, `run_dispatcher.rs:313`, `interval_seconds.max(5)`) and `orphan_timeout_seconds` to ≥ 60, each with a one-time warning naming both the configured and effective values — copy `qa-catalog/src/gear.rs:55-66`'s `effective_*` helper shape and its unit tests (`0` stays `0`; sub-floor clamps; at-or-above is verbatim).

**Two Step 1 outcomes that diverge from this plan's text, both accepted (2026-08-15):**

- **The log-assertion dev-dependency is `tracing-test`, not `tracing-subscriber`.** `tracing-test` is the workspace idiom for asserting log output from `#[cfg(test)]` unit tests inside a lib (account-management, cluster, toolkit itself); `tracing-subscriber` plus a hand-rolled layer is the idiom for `tests/`-directory integration tests (api-gateway). Everywhere this plan and the handoffs say "add `tracing-subscriber` as a dev-dependency", read `tracing-test`.
- **The clamp helpers live in `config.rs`, not `gear.rs`.** This plan's Step 1 says to copy qa-catalog's `gear.rs` placement, but `gear.rs` does not exist until stage 4, so following it would have shipped a knob nothing enforces for two stages. **Stage 4 must not add a second copy in `gear.rs`.**

  **The accessors are the contract, but they do not divide the way an earlier revision of this bullet said.** It claimed the gear "reads each once at init and stores the result" — true of the two **cadences** (`effective_dispatcher_interval_seconds`, `effective_orphan_timeout_seconds`), which is what `config.rs`'s own doc says, and **wrong for `effective_max_timeout_seconds`** — but the sentence that replaced it was wrong too, and it reached shipped code, so both are recorded here.

  **Corrected 2026-08-15 (second time).** The claim "`effective_max_timeout_seconds` is a per-request boundary read; caching it at init would be the bug" is **false**. It has one production caller, `impl From<&QaRunsConfig> for BoundaryLimits`, which `init` invokes **once**; the resulting `u64` is stored on the runtime and layered as a `Copy` extension. It is read once and frozen at init, and that is fine — a config change needs a restart regardless.

  **The real distinction is mundane, and stating it as an architectural rule is what caused the damage.** The two cadences and this ceiling differ only in *which value object carries them to which consumer*: the cadences go to the ticker, the ceiling goes to the REST boundary. That is one sentence. Written as a rule about caching, it produced a `gear.rs` doc comment asserting the value is "a per-request read, deliberately absent here" while sitting five lines above the struct field that holds it — false three times over, and traceable to this paragraph.

  **What actually matters, and is the only rule worth stating:** every read goes through the `effective_*` accessor rather than the raw field. Nothing structural enforces it — the fields are `pub`, so `self.config.dispatcher_interval_seconds` compiles and silently bypasses the floor. Treat it as an unenforced convention and say so wherever it is relied on.
- **`max_timeout_seconds` had two unreconciled ceilings.** The config default is 86 400; `domain::timeout::MAX_LAUNCH_TIMEOUT_SECONDS` is 604 800 (`timeout.rs:97`). An operator raising the knob above the constant would have got exactly the silent clamp the boundary rejection exists to prevent. `effective_max_timeout_seconds()` now takes the lower of the two, so **the number named in the 400 is the number actually enforced**.

- [ ] **Step 2: DTOs and error mapping.** DTOs use `#[toolkit_macros::api_dto(request|response)]`, never raw derives. `serde_with` is not in the workspace, so tri-state patch fields flatten to plain `Option` with documented null-equals-absent semantics. Error mapping in `rest/error.rs` mirrors `qa-environments/src/api/rest/error.rs`: `#[resource_error(gts_id!("cf.qa.runs.run.v1~"))]` etc., **exhaustive `match` with no catch-all arm** so a new `DomainError` variant fails compilation. Mappings that matter:

| DomainError | Canonical | HTTP |
|---|---|---|
| `RunNotFound` / `QueueRowNotFound` | `not_found` | 404 |
| `RunNameExists` / `QueueRowExists` | `already_exists` | 409 |
| `QueueRowNotQueued` / `IllegalTransition` | `aborted` | 409 |
| `AmbiguousBranch` | `failed_precondition` | **400** |
| `QueueFull` / `ConcurrencyLimit` | `resource_exhausted` | **429** |
| `InvalidParameters` / `Validation` | `invalid_argument` | 400 |
| `CorruptState` / `Internal` / `Database` / `ExecutorFailed` | `internal` | 500 |
| `Catalog` / `Environments` | `internal` | 500 |
| `Forbidden` | `permission_denied` | 403 |

**Table corrected 2026-08-15.** It omitted `Catalog` and `Environments` entirely, and `DomainError` has **18** variants (an earlier revision of this plan and one handoff both said 17). Because Step 2 mandates an exhaustive `match` with no catch-all, the omission would have surfaced as a compile error rather than a silent gap — but the mapping still had to be decided, and the decision is **500, not 503**: this layer cannot distinguish "the plan does not resolve" from "the sibling gear is down", and answering 503 to the first invites a retry that can never succeed.

**Correction to this correction, 2026-08-15.** The paragraph above originally ended by saying `disclosable()` "is the authority on which text may reach a body", and used that to argue the table need not be maintained. **All three reviewers falsified that**, and the sentence was itself the drift vector — this is the documented shape where a correction's own supporting prose carries the next over-claim.

What is actually true: `disclosable()` (in **`domain/error.rs:298-329`** — not `api/rest/error.rs`, which is what the surrounding paragraph is about, and the bare filename was ambiguous) is the *intended* authority, but the API layer **restates** the classification in three unconnected places — the or-pattern of the internal arm, the leak test's hand-written variant array, and the module doc comments — and **nothing ties any of them to it**. Falsifying input: move `Self::Validation` to `disclosable()`'s `=> false` arm and every REST error test stays green while the body still carries the field name and the ceiling. Deleting one entry from the leak-test array is likewise green.

So: **maintain this table, and treat the mapping as duplicated until a drift test exists that enumerates all 18 variants and asserts the rendered body agrees with `disclosable()` in both directions.** Until that test lands, no document may claim this file "cannot drift".

`FailedPrecondition` renders **400** — register `error_400`, not `error_409`. Unit-test every mapping with `status_code()` assertions, **including negative assertions that driver/engine text never reaches the client** (assert the 500 body does not contain the raw `Database(..)` string).

- [ ] **Step 3: Routes.** `OperationBuilder`, `.authenticated()`, `.require_license_features::<License>([])`, canonical errors, mirroring `qa-environments/src/api/rest/routes/platforms.rs` line for line.

| Method | Path | Notes |
|---|---|---|
| POST | `/qa/v1/runs` | 200 started / **202 queued** / 429 limit. Register `error_400`, `error_401`, `error_403`, `error_404`, `error_429`, `error_500`. **Multi-status gate resolved 2026-08-15 — the builder supports it and the contract stands.** `OperationBuilder` is a typestate machine: the first response method moves `Missing → Present` (`operation_builder.rs:1152`), and a second same-named set on the `Present` impl returns `Self` (`:1371`), so 2xx responses chain without limit and may carry **different DTO types per status**. Precedent to mirror: `gears/bss/ledger/ledger/src/api/rest/recognition.rs:140`, which registers `RecognitionRunResponse` at 200 and `RecognitionRunQueuedResponse` at 202. Its handler returns `Result<Response, CanonicalError>` and branches on the domain outcome (`:425`) — a typed `Json<T>` return cannot express two bodies. **Caveat, unresolved and worth avoiding:** OpenAPI keys responses by status *string* (`openapi_registry.rs:266`), so two responses at the *same* status collide on one map key with unread utoipa merge semantics. Distinct statuses are unambiguously fine; do not register two bodies at one status. |
| GET | `/qa/v1/runs` | **Superseded by Step 4 — ship `json_response_with_schema::<toolkit_odata::Page<RunDto>>()`.** This row originally said `json_array_response_with_schema::<RunDto>()`, which was right before OData was wired and contradicts Step 4 once it is; both rows of this table were left stale until 2026-08-15. The hazard it names is still real and still applies: **never** `json_response_with_schema::<Vec<T>>()`, because every `Vec<_>` collapses to one OpenAPI component name and the second registration aborts startup. `Page<T>` is a distinct named component, so it is safe where `Vec<T>` is not. |
| GET | `/qa/v1/runs/{id}` | Detail including incremental results. |
| POST | `/qa/v1/runs/{id}/cancel` | `.no_content_response()` for 204 — never `.json_response(NO_CONTENT, …)`, which advertises a JSON body on a bodyless response. |
| POST | `/qa/v1/runs/{id}/rerun` | Same two-outcome shape as launch. |
| GET | `/qa/v1/runs/{id}/logs` | **SSE. Gate resolved 2026-08-15 — do not re-derive.** `OperationBuilder::sse_json::<T>()` is a first-class builder method (`libs/toolkit/src/api/operation_builder.rs:1303`; it hardcodes status 200 and emits `text/event-stream`, which `openapi_registry.rs:241` treats as schema-bearing). **Copy mini-chat**, `gears/mini-chat/mini-chat/src/api/rest/routes/messages.rs:39` — the only production gear using it. Its handler returns the erased `axum::response::Response`, not a typed `Json<T>`, precisely because pre-stream validation must still answer canonical JSON errors while the success path answers SSE (`handlers/messages.rs:46`, body built at `:141` with `Sse::new(relay).keep_alive(...)`). **Do NOT copy chat-engine**: it streams `text/event-stream` while declaring `json_response_with_schema` (`routes/mod.rs:344`, header set by hand at `api/rest/mod.rs:107`), so its published OpenAPI is wrong about its own content type. |
| GET | `/qa/v1/queue` | `?platform_id=&limit=` — limit defaults to 200, clamped 1–500 (the guide's "Read the queue" section). **"Array response" superseded by Step 4: ship `Page<QueueEntryDto>`**, same reasoning as the runs row. **Where the guide's numbers are actually enforced is the thing to pin:** the legacy `limit` parameter's literals are one enforcement point, and `PAGE_LIMITS` (`infra/storage/db.rs`) is the other — a request with no `limit` takes its page size from `PAGE_LIMITS.default`, and `$top` is clamped **solely** by `PAGE_LIMITS.max`. Assert both against the guide's literals, not against each other. |
| DELETE | `/qa/v1/queue/{id}` | Cancel a queued row. 409 when not queued. |
| POST | `/qa/v1/queue/{id}/force-start` | The asymmetry from Task 15 Step 3. |

**Keep response DTOs and their OpenAPI descriptions in sync** — qa-catalog removed a field for a real reason and left two doc comments still advertising it, one of them live in the published spec, inviting someone to "fix the bug" by adding it back.

- [ ] **Step 4: OData on the runs collection.** DESIGN §3.3 puts OData on the qa-runs and qa-insights collections (it is deferred only on catalog's). `cpt-cf-qa-nfr-scale` allocates paging here. Wire it on `GET /qa/v1/runs` and `GET /qa/v1/queue`.

  **Gate resolved 2026-08-15: OData exists and is used by nine gears — do not re-derive.** Mirror **mini-chat**, which is the cleanest SecureORM path:
  - Route: `.with_odata_filter::<ChatCursorField>()` (`gears/mini-chat/mini-chat/src/api/rest/routes/chats.rs:36`), response typed `toolkit_odata::Page<T>`. The extension trait `OperationBuilderODataExt` (`operation_builder.rs:305`) also offers `with_odata_orderby::<T>()` and `with_odata_select()`; `usage-collector/.../routes/usage_records.rs:50` uses all three.
  - Handler: the `OData(query)` extractor (`toolkit::api::odata`, `libs/toolkit/src/api/odata.rs:272`), which already enforces hard caps — `MAX_FILTER_LEN` 8 KiB, `MAX_NODES` 2000, `MAX_SELECT_FIELDS` 100 (`odata.rs:24-29`).
  - Service: resolve the `AccessScope` from the PEP first, then hand the untouched `ODataQuery` to the repository (`chat_service.rs:157`).
  - Repository: `paginate_odata::<F, M, ...>` (`libs/toolkit-db/src/odata/sea_orm_filter.rs:577`). **Its first parameter is `SecureSelect<E, Scoped>`, so it is uncallable on an unscoped select** — the tenancy boundary holds independently of whatever filter the caller supplies. The `F: FilterField` generic is the field allow-list, and passing the *same* `F` to `with_odata_filter` is what stops the advertised fields and the SQL-translatable fields drifting apart.

  **This step also discharges two items that were tracked separately**, and they should be closed here rather than deferred: the "pagination is owed" note on `RunsService::list`, and the guide's `limit` contract on the queue read (defaults 200, clamped 1–500). Use `LimitCfg` for the clamp rather than hand-rolling it.

  **Two things found during stage 1 that this step must handle (2026-08-15):**

  1. **`GET /qa/v1/queue` has no service method to call.** It is specified with an *optional* `platform_id`, but `RunsService::queue_window` is private and requires a platform (`runs.rs:1194`), and there is no public queue read anywhere on the service. A new public method is needed and **is not in Task 16's file list as written** — add it under `domain/service/runs.rs` and say so in the commit.
  2. **Pagination will never bound `RunsRepository::list`, and the endpoint was never its main exposure.** `launch::create_run` calls it on **every launch**, up to `NAME_ATTEMPTS` times, to compute a name sequence, then filters in memory. So capping it at the API tier addresses the smaller of two callers. The remedy is a prefix-filtered read — the source system's own `LIKE '{base}-%'` — which is safe because `next_sequence` re-filters. **That is deliberately not in this task**: it changes the launch path's naming and uniqueness behaviour and needs its own review. Tracked in §8 of the handoff as a follow-up.

**What stage 3 (`d9e3c1c2`) already landed that Step 5 must consume rather than re-create (2026-08-15):**

1. **`ConcreteAppServices` exists, at `crate::infra::ConcreteAppServices`.** Step 5 below places it in `gear.rs`, but the REST handlers need it and they were written first, so it landed in infra — correctly, on the merits: the alias names `OrmRunsRepository`, which the domain layer may not. **Stage 4 must `use` it, never declare a second one.** The instruction is right; the reason first given here was not, and is corrected: two aliases for the *same* instantiation are the same type and cannot diverge. The real hazard is a second alias that **diverges later**, and because these values travel as axum `Extension`s the failure is not a compile error at all — it is a **runtime extraction miss**, in a handler, on a live request. That is strictly worse than the compile-time story originally written here, and it is the composition class both TDD and mutation testing are blind to.
2. **`register_routes` takes three extensions**, and `init` must layer all three: `ConcreteAppServices`, the concrete `RunLogBroadcaster`, and `BoundaryLimits`. The broadcaster is separate because `LogFanout` is the *publishing* port and has no `subscribe` — a subscription returns `LogSubscription`, an infra type the domain seam correctly refuses to name. **The `Arc` layered here must be the same one `ServiceDeps::logs` holds**; a second broadcaster leaves every subscriber silent and nothing else notices.
3. **`RunsDeps` gained `queue_ttl_seconds`** (only that one limit; the other two stay with admission), because `ttl_expires_at` is a derived column of the queue view.
4. Minor, but they cost time to rediscover: `no_content_response` takes **two** arguments, `(StatusCode, description)`. `toolkit-odata` and `futures` were not qa-runs dependencies and are now.

- [ ] **Step 5: `gear.rs`.** `#[toolkit::gear(name = "qa-runs", deps = [authz_resolver], capabilities = [db, rest, stateful], lifecycle(entry = "serve", stop_timeout = "30s"))]`. **Dep-token gate: RESOLVED WRONG on 2026-08-15, corrected the same day by a real boot. The answer is `deps = [authz_resolver, qa_catalog, qa_environments]`.**

  The earlier resolution said "`deps = [authz_resolver]` as written is correct; do not add the QA SDKs". Every fact it cited is true and the **inference was wrong**, because none of it addresses **ordering**. `ClientHub::get` at init finds only what another gear has *already registered*, and `deps` is exactly what places that gear earlier in the topological sort. Measured, not argued — the first boot produced:

```text
topo = [..., "api-gateway", "qa-runs", "qa-environments", ..., "qa-catalog"]
Error: initialization failed for gear 'qa-runs'
Caused by: failed to get qa-catalog client: client not found:
           type=dyn qa_catalog_sdk::client::QaCatalogClientV1
```

  The clue was inside the original evidence and was read backwards: qa-catalog **declares** `credstore` *and also* resolves it through `ClientHub`. That was cited as proof the token is unnecessary. It is proof of the opposite — the token buys ordering, the `ClientHub` call buys the handle, and a consumer needs both. The cost is a Cargo dependency on each **gear crate** (not its SDK) plus a `[workspace.metadata.cargo-shear] ignored` entry each, which the paragraph below already predicted for the add-a-token case.

  **The general lesson, which is why this is written up rather than silently corrected:** a survey that answers "what does the tree contain" does not answer "what does this gear need". The scout question was framed as the former, so a correct census produced a wrong conclusion, and the coordinator stamped it "settled". Facts were never the failure; the inference from them was, and marking it settled removed the prompt to re-check it.

  The mechanics below remain accurate. `deps` takes **gear crate identifiers**, snake_case, converted to gear names by `_`→`-` (`libs/toolkit-macros/src/lib.rs:486-500`; the parser at `:305-320` rejects anything that is not a bare ident). There is no SDK token and no cross-gear-client token: `qa_catalog`, `qa_environments` and every `*_sdk` ident appear in **no** `deps` list anywhere in the tree. Cross-gear clients come from `ClientHub` regardless of whether a token is declared — qa-catalog resolves its two that way (`qa-catalog/src/gear.rs:132-139`), and bss-ledger, which *does* declare `account_management`, still resolves through `ClientHub` (`ledger/src/module.rs:1077`). qa-catalog publishes its own client with no reciprocal declaration, commented "primary consumer: qa-runs" (`gear.rs:203-207`).

  Two consequences if a token is ever added: it requires a Cargo dependency on the **gear crate itself**, not its SDK (qa-catalog spells this out at `qa-catalog/Cargo.toml:15-19`), and the crate must also be added to `[workspace.metadata.cargo-shear] ignored` in the workspace `Cargo.toml`, because the macro's re-exports are invisible to `cargo-shear` and CI's `shear` job fails otherwise. `init` resolves `AuthZResolverClient`, `dyn QaCatalogClientV1`, `dyn QaEnvironmentsClientV1`, and `dyn EventBrokerApi` from `ClientHub`, builds the mock executor and the broadcast log hub, wires `AppServices`, and registers `QaRunsLocalClient` under `dyn QaRunsClientV1`.

  `serve` hosts **one** ticker (the dispatcher) under a child `CancellationToken`, supervised exactly as `qa-catalog/src/gear.rs:424-464` supervises its two — a premature exit is a panic and must surface as an error, not as silent idling. **Leader election:** unlike qa-catalog's idempotent jobs, the dispatcher **claims** rows, so concurrent replicas would double-dispatch. The source system's own admission-correctness note says as much: it "assumes a single manager replica" (guide line 233). **Gate fired 2026-08-15. The API shape *does* permit wrapping a ticker; its semantics do not deliver what this instruction claims.** Evidence, so the next reader can check rather than trust:

  - `cluster_sdk::LeaderElectionV1` is real (`gears/system/cluster/cluster-sdk/src/leader/facade.rs:92`, `elect(name) -> LeaderWatch`) and its own docs name "gate each short, idempotent iteration on the cached `LeaderWatch::is_leader` snapshot" as consumer pattern 1 (`facade.rs:28-44`). So a ticker can be wrapped.
  - **But `is_leader()` is explicitly advisory** (`leader/watch.rs:245-254`): *"**Advisory — do NOT use for correctness-critical mutual exclusion.** The snapshot lags backend truth by up to one renewal interval … and up to a full TTL under partition. Workloads where two simultaneous writers would corrupt state must combine the reactive pattern with `DistributedLockV1::try_lock` or `ClusterCacheV1::compare_and_swap`."* A row-claiming dispatcher is exactly that workload.
  - **It has no working caller.** The only product reference is event-broker, whose `resolve()` is `todo!("resolve cluster-sdk facades once a cluster-gear profile is bound")` (`event-broker/src/domain/cluster.rs:24-45`), and whose reaper worker is a shell. No `impl ClusterProfile` exists outside the cluster gear's own tests and examples, and `Gears.toml` binds no cluster profile. qa-runs would be the first product gear to define a profile marker and the first to call `.resolve()` in non-test code.
  - **The pattern that does wrap tickers in-tree today** is a hand-rolled trait, `LeaderElector::run_role(role, cancel, work_fn)` (`chat-engine/src/infra/leader/mod.rs:28-67`), with two live callers (`chat-engine/src/module.rs:176-199`, `mini-chat/src/infra/workers/orphan_watchdog.rs:59-99`), a `NoopLeaderElector` that simply runs the work, and a feature-gated `K8sLeaseElector`. It is duplicated per-gear infra — chat-engine and mini-chat each hold their own copy — and nothing in `libs/` exports it.

  **User decision, 2026-08-15: port chat-engine's `LeaderElector`. Do not re-litigate.** Copy the trait, `work_fn`, and `NoopLeaderElector` into `qa-runs/src/infra/leader/`, following `chat-engine/src/infra/leader/mod.rs` and selecting the implementation as `chat-engine/src/module.rs:278-296` does. Reasons, in order: it is the only in-tree pattern that actually wraps a periodic loop and it has two live callers; `NoopLeaderElector` runs the work unchanged, so the reference dev stack keeps booting; and it does not make qa-runs the first product gear to depend on an API whose only other consumer is a `todo!()`. The cost is accepted knowingly — this is a **third** copy of per-gear infra that belongs in `libs/`. Say so in the module doc comment and name the two existing copies, so whoever lifts it later can find them. `cluster-sdk` stays a declared-but-unused dependency of qa-runs; leave it, and note in the same comment that it is the intended long-term home once a `ClusterProfile` is bound.

  **Whichever mechanism is chosen, the doc comment must not say leader election makes the dispatcher safe on two replicas.** It does not, and §5's standing decision already says so: admission is N-writer, leader election covers the ticker only, and closing it needs a DB advisory lock on `platform_id`. Leader election here reduces duplicate work and gives `enforce_global_cap` a single ticker-side evaluator; it is not the serialization point. Writing the stronger claim would be exactly the false-guarantee defect this project counts as a bug.

- [ ] **Step 6: Registration.** Extend the existing feature in `apps/cf-gears-example-server/Cargo.toml`:

```toml
qa-platform = ["dep:qa-environments", "dep:qa-catalog", "dep:qa-runs"]
```

and add the dependency itself, matching the sibling entries' nested path shape (`qa-runs = { path = "../../gears/qa-platform/qa-runs/qa-runs", optional = true }` — the crate lives one level down from the subsystem directory).

Add `#[cfg(feature = "qa-platform")] use qa_runs as _;` to `src/registered_gears.rs`, and a `qa-runs:` block to **`config/qa-platform.yaml` at the workspace root**, with a `database:` sub-block on `sqlite_qa_platform` mirroring its two siblings, plus a `qa_runs:` entry in the `logging:` section (both siblings have one).

**Set `dispatcher_enabled: false` in this dev config**, with a comment carrying the reason. The code default stays `true`; this is the dev stack alone. Precedent is directly adjacent: qa-catalog sets `branch_refresh_interval_seconds: 0` in this same file and explains, in the comment block above it, that the static-authz plugin denies the gear's nil-tenant system actor outright, so the task can only fail closed and WARN. qa-catalog's bundle GC shows the alternative — its cadence is a `const`, so it cannot be switched off and "keeps ticking and keeps logging that one WARN". At a 5 s tick that is a WARN every five seconds, which is why the knob exists. Note in the comment that this is what makes the dev dispatcher inert, and point at DECOMPOSITION 2.3 for the open decision about a policy plugin that could grant a system subject.

- [ ] **Step 7: Tenant-scoping and security tests.** Port `qa-catalog/src/domain/service/tests_tenant_scoping.rs`'s harness. **Mocks carry real tenant ids.**

```text
- a_run_is_invisible_to_another_tenant
- a_queue_row_is_invisible_to_another_tenant
- launching_against_another_tenants_platform_is_not_found_not_forbidden
- launching_against_another_tenants_custom_plan_is_not_found
- a_pdp_denial_on_create_fails_the_launch_closed
- cancelling_another_tenants_run_is_not_found
- force_starting_another_tenants_queue_row_is_not_found
- the_same_run_name_in_two_tenants_does_not_collide
- a_queue_row_cannot_be_created_against_another_tenants_run   (the service's ownership precheck —
                                                               the boundary Task 10 Step 7 documented)
- no_production_path_uses_allow_all
```

The last one is a `grep`-shaped test or, better, a build-time check: assert `AccessScope::allow_all()` appears in no non-test module. Do it however the workspace already does; if there is no precedent, make it a `#[test]` that reads the crate's own sources. It exists because a probe using `allow_all()` created a cross-tenant existence oracle in qa-environments.

- [ ] **Step 8: Full verification gate.**

```bash
cargo build -p qa-runs -p qa-runs-sdk
cargo clippy -p qa-runs -p qa-runs-sdk --all-targets -- -D warnings
cargo test -p qa-runs
cargo fmt --check -p qa-runs -p qa-runs-sdk
cargo build -p cf-gears-example-server --features qa-platform
cargo gears --version   # check only; do NOT install
```

Expected: all green. Report the real test total. If `cargo gears` is absent (it was for both shipped gears and the parity pass), say plainly that the architecture lints — **including DE0309 `#[domain_model]` coverage** — are **unverified** and must be run in CI. Do not report them as passing.

- [ ] **Step 9: Live smoke, verified properly.** `--list-gears` only echoes the YAML config and is **not** proof of registration.

**Feature list corrected 2026-08-15.** `--features qa-platform` alone links no authn, authz, tenant-resolver or credstore plugin, while `config/qa-platform.yaml` configures four of them by name. The dump commands exit before boot so they tolerate it, but the server run does not. Use the feature list from the config file's own usage line (`config/qa-platform.yaml:8`), which is how both shipped gears were smoked. Run from the workspace root — the `--config` path is relative:

```bash
cargo run -p cf-gears-example-server \
  --features qa-platform,static-authn,static-authz,single-tenant,static-credstore \
  -- --config config/qa-platform.yaml --dump-gears-config-yaml

cargo run -p cf-gears-example-server \
  --features qa-platform,static-authn,static-authz,single-tenant,static-credstore \
  -- --config config/qa-platform.yaml run
```

`run` is the default subcommand (`main.rs:107`, `cli.command.unwrap_or(Commands::Run)`), so naming it is optional; it is spelled out here to match the config file's usage line.

Confirm from the registry-backed dump that `qa-runs` is present; then from the boot log that its **migrations ran** and its **routes registered**; then:

```bash
curl -s localhost:PORT/openapi.json | jq '.paths | keys | map(select(startswith("/qa/v1/runs") or startswith("/qa/v1/queue")))'
```

Expected: the eight paths from Step 3. Use the port the boot log reports.

- [ ] **Step 10: Hygiene sweep before committing.**
  - No stale `// Task N:` markers anywhere — grep for them: `grep -rn 'allow(dead_code)\|// Task [0-9]' gears/qa-platform/qa-runs/`. **Corrected 2026-08-13:** this said `TODO(Task N)`, which can never match — `\bTODO\b` is a hard CI failure via `.github/forbidden-words.json`, so Task 3 correctly used `// Task N:` instead.
  - Every `#[allow(dead_code)]` is per-item, carries a `reason`, and names the task that removes it; none should survive Phase A except for the schedule surfaces Phase B fills.
  - **`dead_code` cannot see an unused *trait method*.** qa-catalog shipped ~52 lines of dead repository code that only a human reading the call graph caught. Walk every method on `RunsRepository`, `QueueRepository`, `RunExecutor`, and `EventPublisher` and confirm each has a real caller; delete or justify the ones that do not.
  - Services and `AppServices` are `pub(crate)`.
  - SDK crates still free of `serde`/`utoipa`/`http`; no `sea_orm`/`axum`/broker type in any domain signature.

- [ ] **Step 11: Commit.** `git commit -m "feat(qa-runs): REST layer, SSE logs, gear bootstrap, and server registration"`. **The Phase A squash moves to the end of Task 16b** (added 2026-08-15), which is now what closes Phase A.

---

### Task 16c: Nothing drives `IngestService` — the run lifecycle does not close (added 2026-08-15)

**Found by deleting the `dead_code` tripwire in Task 16 stage 4, which is precisely what that attribute existed to reveal.** `domain/mod.rs`'s `#[cfg_attr(not(test), allow(dead_code))]` had masked it for the whole of Phase A.

**The gap:** `IngestService` is unreachable from production. `apply` has no caller, and `RunExecutor::watch` has **no non-test caller** either. So a run this gear dispatches receives no results at all and reaches a terminal state **only via the timeout sweep** — every run eventually times out regardless of what the executor did. Every per-test row, every verdict derivation, and the whole of Task 15's ingest path is live code with no production entry point.

This is not a defect in Task 15 or 16; both built what their plans specified. **No task in this plan was ever given the job of driving the watcher**, which is why nothing caught it until the tripwire came off.

**Deliberately not fixed in Task 16.** Closing it needs a per-run watcher seam on the dispatch path *and* an answer for re-attaching watchers after a restart — a live `watch` stream does not survive a process bounce, and the runs it was watching are exactly the ones boot recovery must not fail. That is a subsystem, it has no plan text, and inventing one inside a REST-layer task is how the composition defects in this project happened.

**The design was settled 2026-08-15 in a coordinator design pass, with the two shape decisions ratified by the user.** What follows is the ratified shape and the evidence behind it. **Every factual claim below was read off the tree by the coordinator and is therefore suspect — verify each one before relying on it, and report what is wrong rather than working around it.** Three claims in the original four-step sketch were already falsified this way; two of them are retracted in place below.

#### The shape

**A `RunWatcher` port, mirroring `launch::InlineDispatcher` one-for-one.** `AppServices`' own `ingest` field already names the closure: *"a seam on the dispatch path that the composition root implements by spawning a task to drain `RunExecutor::watch` into `IngestService::apply`"* (`domain/service/mod.rs`, the `ingest` field's doc). `DispatchService` cannot call `IngestService` directly without a service-to-service dependency, and it does not have to: `launch::InlineDispatcher` is exactly this pattern already — a domain-layer port that `DispatchService` implements and `AppServices::new` wires with a default (`domain/service/mod.rs`, `let dispatcher = deps.dispatcher.unwrap_or_else(..)`). Follow it exactly, including the injectable override, which is what makes the seam fakeable without a database.

**Attachment has ONE caller and one registry — corrected 2026-08-15, re-ratified by the user the same day.** The registry is keyed by run id and **idempotent**: asking twice for the same run attaches once. This is the structural closure `LogWiring` used against the two-broadcaster defect (`gear.rs`) — the double-attach must not be *spellable*, not merely untested. The single caller is **the leader-gated re-attach sweep**.

> **Retracted: the two-caller shape, and both reasons given for it.** This section originally ratified two callers — `record_started` for immediacy, plus the sweep — and justified the first with *"for a platformless run this is the only attach path that exists — see below"*. **That sentence was false when written, and the text it pointed at is what falsifies it**: the retraction two paragraphs below establishes that the re-attach source is the run table, which finds platformless runs perfectly well. The pointer aimed at the paragraph that kills the claim. `a_platformless_run_reaches_a_terminal_state_from_its_finished_event` now proves the sweep reaches them.
>
> The implementer then found the load-bearing objection, which all three reviewers verified independently and none could falsify: **`record_started` cannot mint a correct ingest context.** `qa_runs_sdk::Run` carries no `tenant_id` — the spec review enumerated all twenty-six fields — and `IngestService::record_one_result` writes `ctx.subject_tenant_id()` into `qa_run_test_results.tenant_id`. No alternative tenant source exists at that call site: `OwnedRunId` is a bare `Uuid` newtype, `ClaimRow` carries no tenant, and `RunsRepository::get` returns the tenant-less SDK `Run`. Supplying one would need a new repository read — which is precisely the work the sweep already does. `TimeoutCandidate`'s own doc had already recorded the general form of this: the tenant travels separately because a caller holding only a run id "would have nothing to mint the second context from".
>
> **State the premise precisely.** It is *not* that `record_started` never has the right tenant — on the tick path and the inline-launch path it is equal by construction, and only force start can diverge. It is that it has **no reliably correct tenant and no discriminator to tell which case it is in**, since `record_started` sees `Some(queue_id)` on both the tick and force-start paths. The conclusion is unchanged; the categorical version is falsifiable and must not be written.
>
> **Two consequences.** The plan's derived "gate caller 1 on holding the dispatcher role" machinery has nothing left to gate — the only attach site is already inside `run_tick`, inside `run_role`. And the leader gate, which this section correctly notes gates nothing under the shipped elector, is nonetheless now the gate on the *only* attach site.

**Watchers run only where the dispatcher ticker runs — user decision 2026-08-15.** Two consequences, both to be written where a reader meets them rather than left to be discovered:

- **SSE `/logs` streams only on the replica running the dispatcher — *once a real elector is deployed*.** `RunLogBroadcaster` is a per-process channel map (`infra/logs/broadcast.rs`) and nothing replicates between them, so under a real elector a subscriber on another replica gets a 200 and silence. **Corrected 2026-08-15: this bullet originally stated that unconditionally, and unconditionally it is false.** Under the shipped `NoopLeaderElector` — the only `impl LeaderElector` in the crate — `run_role` calls its work unconditionally, so *every* replica runs the tick, attaches observers, and publishes into its own broadcaster, which is the same one its own router subscribes to. There is no "other replica" that is not also the dispatcher, and today a subscriber on any replica does get lines. **The whole of this section's own warning applies to this bullet: do not describe the gate as if it currently gates anything** — a rule this bullet broke while stating it. `runs_repo.rs`'s treatment of the structurally identical point is the model to copy.
- **Under `NoopLeaderElector` — still the only implementation shipped — "leader-only" is vacuous, so N replicas means N watchers per run.** That is the *same* pre-existing hazard `infra/leader/mod.rs` already documents for `recover_after_boot` and the claim-scan cursor, with the same fix (a real elector, chat-engine's per §5), not a new one. Do not describe the gate as if it currently gates anything.

> **Retracted: the derived role-check machinery.** This paragraph read: *"`record_started` is reached from the inline launch path, which is not leader-gated … so caller 1 can fire on a non-leader replica. Gate it on holding the dispatcher role."* The premise is true and remains true — `infra/leader/mod.rs` is explicit that admission is N-writer and no election gates the launch path. The **machinery is obsolete**, because caller 1 no longer exists. Recorded rather than deleted because the premise is the reason a future revision must not reintroduce an inline attach casually. The paragraph also invited a cheaper resolution that kept one producer per run; the implementer found one, and it was to delete the caller.

#### Two retractions from the original sketch

**Retracted: "`recover_after_boot` fails claims a restart left mid-dispatch, on the premise that nothing is watching them. That premise is about to change."** It does not, and the premise does not change. `recover_window` classifies `execution_ref: None` as `ClaimExecution::Absent` and everything else as `Active`, and only `Absent` reaches `ClaimAction::FailOrphaned` (`domain/service/dispatch.rs`, `recover_window`). Rows carrying a reference — the only rows a watcher could ever attach to, since `watch` takes an `ExecutionRef` — are already left to the tick reconciler. **Boot recovery and the watcher do not interact.** Confirm this before building around it.

**Retracted: that re-attachment can ride the tick's claim reconciliation.** It cannot, and this is the finding that shapes the task. `AdmissionService` answers `Admission::Unqueued` for a run with no platform — *"A run with no platform is never queued and never occupancy — no row, no lease, no lock"* (`domain/service/admission.rs`, the `Unqueued` arm) — and `reconcile_claims` scans **queue** rows via `QueueRepository::all_claims`. A platformless run therefore never appears in reconciliation, so a claim-scan-driven re-attach would leave exactly those runs on the timeout-only path this task exists to close.

**So the re-attach source is the run table, not the queue.** Its precedent is `RunsRepository::list_timeout_candidates`, which is the same shape and whose doc carries the reasoning to inherit: cross-tenant, windowed, ordered by `id` **and deliberately not by the interesting timestamp**, with a rotating cursor advanced by the caller, issued under a nil-tenant enumeration identity and followed by one scoped read per candidate. `list_timeout_candidates` itself is **not** the read — it filters to overdue runs, and a healthy running run is not overdue.

- [x] **Step 1: the `RunWatcher` port and its registry.** ~~The port in `domain/ports/`, the registry and the spawning implementation in `infra/`.~~ **Retracted 2026-08-15 — this contradicted the same section's own directive to mirror `launch::InlineDispatcher` one-for-one.** That trait is declared in `domain/service/launch.rs` and implemented by `DispatchService`, a *domain* service; it is not a `domain/ports/` + `infra/` split at all. The two texts could not both be followed, and the implementer correctly followed the shape section over the step. Delivered in `domain::service::watch`, for the independent reason that `AppServices::new` is the only point where the ingest service exists and the dispatch service does not yet, and it may not name an infra type. Bind the watcher's lifetime to the run, never to the request that launched it. State what happens when the stream ends without a `Finished` event, and what happens when `watch` errors — `RunExecutor`'s own trait doc is contract here: an empty stream is *"nothing more to say"*, **never** *"this failed"*, and a `watch` error means nothing is known. Neither may retire a run.

- [x] **Step 2: the re-attach sweep.** A new `RunsRepository` read over non-terminal runs holding an `execution_ref`, following `list_timeout_candidates`' shape — and **inheriting its starvation argument, not just its signature**: say in the new method's own doc what a rotating window costs *this* pass, the way that one does. Cross-tenant enumeration identity for the scan; a scoped read per candidate; `system_actor` needs the constructor pair for it and does not have one today. Runs it into the leader-gated ticker beside `recover_after_boot`.

- [x] **Step 3: the security surface.** A new cross-tenant enumerating read is the highest-risk thing this task adds. PEP before the repository call, one scope per resource type, filter-first; the write path per candidate is scoped to that candidate's tenant, never to the enumeration identity — `recover_window`'s per-tenant grouping is the pattern. **`AccessScope::allow_all()` stays banned** and `no_production_path_uses_allow_all` must still pass.

- [x] **Step 4: close the timeout-only path.** A test that fails if the watcher is not wired: a run reaching a terminal state from `ExecutionEvent::Finished` rather than from its deadline. **`init` has no test and this task does not have to give it one**, but the seam it introduces must be constructible without a database and four cross-gear clients — that is the whole reason it is a port with an injectable override. Cover both shapes: a platform-bound run and an `Unqueued` one.

- [x] **Step 5: state what is not proven.** At minimum: that the leader gate gates nothing under the shipped elector; that SSE logs are replica-local; that re-attachment against a *real* executor whose `watch` must resume mid-execution is unproven here, since `MockRunExecutor` satisfies the re-attach contract by replaying (`domain/ports/run_executor.rs`, `watch`'s "re-attachable by design" paragraph) — the mock cannot falsify a real resume, and saying so is the deliverable.

**User decision 2026-08-15 — the executor's `Finished { message }` is accepted at parity, ungated.** It reaches `qa_runs.error` and the `RunFinished` event payload verbatim, with no `DomainError::recorded_text()` gate, and **Task 16c is what makes that path live for the first time**. Accepted because it is deliberate and documented at the port, it matches the source system (`argo.rs:2280-2283`), and the text concerns the tenant's own run — infrastructure-detail disclosure to the owning tenant, not cross-tenant. **Record the asymmetry rather than leaving it to be rediscovered:** `DomainError::ExecutorFailed` is classified **non-disclosable** on the stated grounds that it "wraps the execution plane's vocabulary", so the same system's text is redacted through the error wrapper and passed verbatim through the event payload. That is this subsystem's "payload → wrapper" oracle-move shape, accepted with eyes open.

**Does not own** the three races in Task 16b, and must not close them incidentally. **16b's Step 1 gate is a constraint on this task**: the completion's phantom read stays unreachable only while **one** producer feeds each run. If any shape here would give a run two concurrent producers, stop and report it rather than absorbing 16b's work.

**Tracked follow-up, not owned by this task: a doc-citation resolution test.** This task shipped an invented test identifier — the second instance in this project, in the same kind of sentence both times — and the two corrections for it were *themselves* false, one claiming a rustdoc link is compiler-resolved (it is not; `broken_intra_doc_links` is a rustdoc lint and `cargo doc` does not build `#[cfg(test)]` modules) and one claiming the `const _PRECEDENT` binding makes the citation checked (it pins the function's existence, not the doc text beside it — falsified by changing only the doc link). Both reviewers independently proposed the same remedy and it is this crate's own established idiom: one source-reading `#[test]` on the shape of `system_actor.rs`'s `every_factory_in_this_module_is_classified`, using `include_str!` to extract identifier-shaped tokens from doc comments and assert each resolves to a `fn` in the tree. It would have caught both historical instances. **Not built here** — it is crate-wide hygiene, not the run lifecycle, and inventing it inside this task is the scope mistake this plan keeps recording.

**Until this lands, the honest statement of what Phase A delivers is: runs launch, queue, dispatch, and time out.** Do not describe the subsystem as end-to-end.

---

### Task 16b: The three multi-connection races SQLite cannot falsify (added 2026-08-15 by user decision)

**Why this is its own task.** Each item below is a race between two database connections. The whole test tier is in-memory SQLite, which serializes writers, so **none of these can be falsified where every other test in this crate runs** — and this project counts a test that passes either way as a defect, not as coverage. Closing them means building integration-tier tests against a real dialect. ~~and **no gear in this workspace has a falsified retry branch yet**, so this task builds that capability first and uses it three times.~~ **Retracted 2026-08-15 by the coordinator, measured against the tree: the integration-tier capability already exists and this task follows it rather than building it.** That is a different kind of work from wiring REST handlers, with different reviewers and a different failure mode, which is why it is not folded into Task 16.

**The precedent, verified by reading the files:**

- `gears/system/account-management/account-management/tests/coord_lease_integration_pg.rs` — a real-Postgres suite behind `#![cfg(feature = "integration")]`, with `tests/common/mod.rs`'s `pg::bring_up_postgres` standing the container up via `testcontainers`. It drives concurrent `acquire` against a path whose transaction is `TxConfig::serializable()` + `transaction_with_retry`, and asserts exactly one winner with `LeaseHeld` for every loser.
- `gears/system/cluster/plugins/postgres-cluster-plugin/` — a whole Layer 3 testcontainers suite, with the `integration` feature declared so a default `cargo test` never needs Docker, and `testcontainers`/`testcontainers-modules` listed as optional deps under a `cargo-shear` ignore.
- `gears/system/resource-group/resource-group/src/domain/group_service.rs` — four **production** call sites using `db.transaction_with_retry(TxConfig::serializable(), …)`.
- `libs/toolkit-db/src/contention.rs` — already classifies the retry-worthy errors for both Postgres and MySQL, and already records that matching SQLSTATE alone is insufficient because sqlx surfaces some serialization failures without the numeric code.

**What is genuinely unverified, and stays the implementer's first question:** whether any of those retry branches is *falsified* — that is, whether any test goes red when the retry is removed. The coordinator read their assertions, not a mutation of them. So the honest claim is that **the harness capability exists and is the precedent to copy; a falsified retry branch may still be this task's to build first.** Do not repeat the retracted sentence, and do not upgrade this one either — measure it.

`libs/toolkit-db/src/secure/db.rs` has the idiom, `Db::transaction_with_retry` and `transaction_with_retry_max`; `libs/toolkit-db/tests/retry_helper_sqlite.rs` exercises the budget. Start there, then copy the AM suite's shape.

**The shipped dialect is Postgres, not MySQL — corrected 2026-08-15.** Every gear in this workspace links `toolkit-db` with `features = ["sqlite", "pg"]` or `["sqlite"]`; **no gear anywhere enables the `mysql` feature**, which exists in `libs/toolkit-db/Cargo.toml` and is dead. `config/qa-platform.yaml` uses SQLite. So the integration tier targets **Postgres**, and any isolation reasoning must be `READ COMMITTED`'s. This contradicts a doc comment in the code — see Step 1.

**Owns:** the three closures below and whatever integration-test harness they need. Does **not** own anything in Task 16's file list.

- [x] **Step 1: the completion's phantom read.** `IngestService`'s `finish` tallies the per-test rows, derives the verdict, and writes the terminal state in one transaction — but nothing under `READ COMMITTED` stops the row set growing between the tally and the write, so a result committing in that window leaves a `Succeeded` run with a `FAILED` row stored. `record_one_result` carries the trace and the costs. ~~**Not reachable today**, because `ingest` drains one stream sequentially and one stream cannot race itself; it becomes reachable the moment a second producer exists.~~ **Retracted 2026-08-15: Task 16c is that second producer, and the exposure is wider than this step's single sentence.** Reachability is now a deployment property, not a code property — **unreachable on one replica, live on more than one.** `reattach_watchers` is leader-gated, but `NoopLeaderElector` is the only `impl LeaderElector` in the crate and its `run_role` calls the work unconditionally, `WatchRegistry` is a per-process `HashSet` that consults no database, and `list_watch_candidates` is neither claim-gated nor leader-gated — so it returns the same rows to every replica and each opens its own observer.

  **Three consequences, not one.** The verdict flip this step describes is the *least* of them:

  1. **Duplicate per-test rows.** `qa_run_test_results` has **no unique index** on `(tenant_id, run_id, test_file, test_name)` — the migration records uniqueness as *"an application invariant, not a constraint"*, held solely by delete-then-insert inside one transaction. Two producers in two **processes** are two transactions, so duplicates are reachable under any isolation weaker than serializable. This is not cosmetic: `finish` derives the verdict **from these rows**, so duplicates feed the verdict directly.
  2. **Counter double-count.** ~~`add_result_counts` applies a delta computed from a `previous` read~~ — **the mechanism as written names the wrong function, corrected 2026-08-15; the conclusion is unchanged.** `add_result_counts` is *not* a read-modify-write: `infra/storage/runs_sea_repo.rs` builds a server-side `col = col + n` via `col_expr`/`clamped_increment`, and its own comment says why — *"A read-modify-write in this process would lose one of two concurrent result events."* That write is atomic and is not the defect. **The racing read is one layer up**, in `record_one_result`: it reads `previous` via `list_test_results` filtered to the test's `(test_file, test_name)`, computes `counter_delta(previous, &new.status)`, and only then calls `add_result_counts`. Two producers that both read `previous = None` both compute `+1` and both apply it atomically — the counter reaches 2 for one logical test, surfaced by `GET /runs/{id}/result`. **An implementer sent to `add_result_counts` would find correct code and report no defect**, which is why this correction matters.
  3. The completion's phantom read, as originally described.

  **The gate itself is unchanged and was honoured** — Task 16c added no ingest route. But this step's stated rationale, *"the default is already correct — the risk is someone adding one, not someone forgetting to"*, no longer covers the exposure: **nobody has to add anything; a second replica suffices.** Whichever closure this step takes must be assessed against all three.

  **A false premise sits in the code this step must read — `domain/service/ingest.rs`, the isolation paragraph.** It asserts *"`REPEATABLE READ` on MySQL/InnoDB, which this gear ships (the migration's `MYSQL_UP` arm)"*. **The gear does not ship MySQL.** `qa-runs/Cargo.toml` links `toolkit-db` with `features = ["sqlite", "pg"]`, and no gear in the workspace enables `mysql`. The `MYSQL_UP` arm exists because **every** migration here carries three dialect blobs by convention — that is not evidence of what is deployed, and the inference from one to the other is the defect. This is a bug report, not a wording problem: **the paragraph's conclusions are reached partly through the REPEATABLE READ branch, so re-derive them under `READ COMMITTED` rather than editing the label.** The paragraph also says an earlier revision "got the premise wrong by asserting one of them for both" — so this is the *second* wrong premise in the same paragraph. Per the standing rule on a paragraph wrong twice running, the next wrong revision means delete it.

  **A third closure exists and the coordinator assessed it as expensive, not as rejected — the choice remains this step's.** A unique index on `(tenant_id, run_id, test_file, test_name)` would close consequence 1 at the storage layer rather than the isolation layer. The migration rejects it on an **InnoDB 3072-byte** limit, which is a MySQL argument and therefore no longer load-bearing. It would additionally contradict the migration test `a_repeated_per_test_tuple_is_accepted`, which pins the constraint's *absence*. Cost this properly before choosing it; do not report the InnoDB reason as if it still applied.

> **Two corrections to the paragraph above, both to coordinator claims, 2026-08-15.**
>
> 1. ~~the tuple "**also exceeds Postgres's btree index-entry limit**", so the rejection survives on the shipped dialect~~ — **UNVERIFIED and withdrawn as a stated fact.** The coordinator asserted it without measuring; the implementer explicitly declined to repeat it, correctly. It may well be true — the tuple is wide — but **nobody has measured it, and it must not be cited as a reason until someone does.**
> 2. The replacement rationale that surfaced during the work — *"legacy dedupes by delete-then-insert, so the schema must accept a repeated tuple"* — is a **non-sequitur**, falsified in review. Delete followed by unconditional insert never leaves two rows carrying the tuple, so it is fully compatible with a unique index; neither legacy statement would break under one. The absence of the index is why delete-then-insert was chosen, not a consequence of it.
>
> **Net: there is currently NO surviving positive reason for the missing index** — one was dialect-dead, one was unmeasured, one was invalid. That is the honest state and it is where a future revision must start. This paragraph has now been wrong twice; a third wrong revision means delete it rather than write a fourth.

  Two candidate closures, and the choice is this task's to make and record: a locking read taken by **both** transactions before their reads, or `SERIALIZABLE` with a retry on SQLSTATE `40001`. **The coordinator's recommendation, which the implementer is invited to overturn with evidence, is `SERIALIZABLE` + retry** — it is the workspace's established idiom (four production call sites in `resource-group`, plus AM's lease path), `toolkit-db` already ships both the retry helper and the 40001 classifier, and the locking-read alternative has a known dead end recorded below. Whichever is taken **must arrive with an integration test against a real dialect**. Note the Task 15 finding that settles one wrong turn already taken here: `RunsRepository::get` is a plain scoped `SELECT`, so a non-locking read under `READ COMMITTED` serializes nothing and `FOR UPDATE` on it would not have closed this.

  **Gate:** `IngestService::apply` must not be wired to an HTTP endpoint until this closes. Task 16's route table deliberately contains no ingest endpoint, so the default is already correct — the risk is someone adding one, not someone forgetting to.

- [x] **Step 2: `enforce_global_cap` is read-then-act with no coordination.** It is `pub(in crate::domain::service)` with two callers — launch and force start — and nothing pins that both take the same path; contrast `admission_and_dispatch_share_one_platform_lock_registry`, which does pin its analogue. N operators across N replicas all read under the cap and all proceed. **This is the one cap the frozen guide says must not be overridden** (`exclusive-runs-and-the-queue.md:117`: force start "does **not** override `max_concurrent_runs`"), so the gap contradicts a frozen promise rather than merely being weak. Add the coordination and a guard that pins both callers to the one path.

- [x] **Step 3: the counter correction's read-modify-write.** ~~`stored` is read on one connection and the corrective delta applied on another, leaving the columns at `tallied + d`; a transaction does not fix it.~~ **The two-connection premise is false — corrected 2026-08-15, and verify this before building on either version.** `reconcile_counts` is a module-private free function taking `tx`, called from exactly one place: inside `finish`'s transaction. Its `get_result` read, its `list_test_results` tally, and its corrective `add_result_counts` all run **on that one transaction**. So "one connection … another" does not describe the code. What survives is the skew, by a different route: the correction's delta is computed from a `stored` value that a **concurrent producer's** committed `add_result_counts` can invalidate between the read and the correcting write, since `finish` sets no `TxConfig` and inherits the server default. The columns therefore remain best-effort. The verdict does not depend on this — `reconcile_counts` returns `tallied` regardless — so the columns are **best-effort, not convergent**. Either make them convergent or state the weaker guarantee where a reader of the columns will see it. Do not write a test that passes either way.

- [x] **Step 4: gate, then squash Phase A.** Full gate per §9, then squash Tasks 1–16b into one commit on `feature/qa-platform-specs`.

#### Task 16b closed 2026-08-16 — what shipped, and what did not

**Counts, re-measured by the coordinator rather than relayed:** qa-runs **646 lib + 1 integration + 2 ignored doctests**; **651** under `--features integration --lib`; `make test-qa-runs-pg` **652/652**. Siblings unchanged at qa-environments **81**, qa-catalog **162**. Full gate green, exit codes read individually.

**Step 1** closed the three-fold exposure with `SERIALIZABLE` + a bounded retry. `Db::transaction_with_retry` was **unusable**: it requires `Fn(&E) -> Option<&sea_orm::DbErr>` and `DomainError::Database` holds a `String`, because `db_err` takes `impl Display`. `resource-group` and `account-management` can use the helper only because they keep the `DbErr`. So the retry loop is local, delegating classification to `toolkit_db::contention`. A real-Postgres tier sits behind `--features integration`.

**Step 2** replaced the read-then-act cap with a `CapSlot` reserved by one `fetch_update` and held to the submit. Three alternatives rejected **by consequence**: a lock around the *check* closes nothing (the second caller reads the same listing and passes the same check — an advisory-lock-only fix would have been a fake fix); a lock spanning check-to-submit serialises every launch cluster-wide across a repo sync and a bundle build, which `run_queue.rs:569-572` already refuses for the weaker per-platform case; counting committed claims leaves the platformless arm open, since that arm writes no queue row.

**Step 3 reversed its own prediction by measuring.** The columns are **convergent at the completion's commit**, not best-effort, and the mechanism is *not* the one this step named: the correcting write was always safe, because `add_result_counts` is a server-side `col = col + n` and a delta preserves a concurrent increment. What needs the escalation is the **pair of reads** — `get_result` can miss a commit that `list_test_results` then sees. The guarantee and its two residuals are stated on `RunsRepository::get_result`, where a reader of the columns meets them.

**Two additions authorised by the user, outside this plan's text:**

1. **The Postgres tier is wired to CI** (`make test-qa-runs-pg`, in the `integration` job beside the three sibling suites). It was discovered that `qa-runs` appeared **nowhere** in `Makefile` or `.github/workflows/`, so the only guards able to falsify the escalation ran on a developer's machine and nowhere else.
2. **A doc-citation resolution test was built** (§7's tracked item, deferral overridden) after the class reached five shipped instances. **It found a sixth before landing.** Measured and load-bearing for the decision: a dangling link inside a `#[cfg(test)]` module is reported *not at all* by `cargo doc --document-private-items`, while the same link on a non-test private item **is** — so rustdoc cannot subsume it. **Its own limits, stated because they are real:** qualified paths (`Self::foo`) are **not** checked and are the majority of link targets; the bare-token rule uses a four-underscore threshold that catches test-name citations and not function-name citations; and the file-path instance is out of scope.

**Not proven, and none of it is hedging:**

- **The cap gate's cross-thread atomicity is unproven** — the test runtime is single-threaded, and replacing the `fetch_update` with a load and a store leaves every cap test green. The counting is falsified; the atomicity is not.
- **A second replica has a second counter.** The gate is an in-process `AtomicU32`. Closing it needs durable reservations and a schema change. The frozen guide already scopes admission correctness to `replicaCount: 1` (guide lines 232-233), so this is a documented limitation rather than a new hole.
- **The dispatcher tick keeps its own claim-based budget**, so a tick and a launch can still pass the cap jointly. That separation predates this task.
- **One `make test-qa-runs-pg` failure was observed and never diagnosed.** Twenty-plus clean runs stand against it and nobody can name the failing test; one apparent reproduction turned out to be a reviewer's own `shutil.copy2` mtime bug leaving cargo on a stale mutant binary. `--retries 1` was added, matching `test-cluster-pg`. **It is mitigated, not understood**, and two reviewers separately verified a real regression still fails both attempts and exits non-zero. If it recurs, the next step is a nextest test group serialising the pg tier.
- **`cargo gears lint --dylint` and `cargo-shear` are not installed**, so DE0309 and the shear-ignore entries are **UNVERIFIED** — not passing.
- **Step 1's last two commits were never reviewed.** The user stopped the review iteration after three rounds as disproportionate, which it was: across those rounds no reviewer ever found a defect in the closure itself — every blocking finding was prose, a manifest comment, a commit message, or a bug in an artifact review had itself requested. Recorded as a deliberate trade, not an oversight.

**A coordinator instruction was refused, and the refusal was right.** Step 2 was told to ship with a real-Postgres falsifying test. `enforce_global_cap` is `executor.list_active()` plus one `fetch_update` — **no database on the defect's path or the fix's** — so a Postgres harness would have exercised byte-identical in-process code and passed either way, which this project counts as a defect rather than coverage. The falsifying test was built where the mechanism actually races, using an executor double that yields to force the overlap. Spec-compliance adjudicated the refusal correct and noted the "integration test against a real dialect" requirement belongs to **Step 1**, not Step 2.

---

# Phase B — Schedules (2.4)

**What is and is not ported here.** Legacy delegates cron evaluation to Argo `CronWorkflow` objects entirely: there is no cron evaluator, no `schedules` table, and no tick table anywhere in `manager/src` (`routes/schedules.rs` is CRUD over Kubernetes objects, and `parse_cron_workflow` at `argo.rs:2321-2360` reads them back). So the *machinery* is net-new, demanded by `cpt-cf-qa-principle-db-first-state` and `cpt-cf-qa-nfr-scheduler-exactly-once`. What legacy does supply, and what must be ported exactly:

- **The stored exclusivity choice is a tri-state, always written.** `true` / `false` / `auto`, and `auto` is written explicitly rather than omitted — because an absent annotation would mean both "inherit" and "parallel" in already-deployed objects, "a distinction that then cannot be recovered" (`exclusivity.rs:60-79`, `format_exclusive_annotation`). The codec pairs with its parser and the two live together for that reason.
- **The choice arrives at resolution as the `launch` tier.** Not a fourth tier: "a CronWorkflow delivers its choice through the trigger request's `exclusive` parameter... so by the time admission runs a schedule's choice *is* `launch`" — which also means the two can never disagree (`exclusivity.rs:113-119`, guide lines 62-65).
- **Firing uses the same run-creation path as a manual launch.** `cpt-cf-qa-fr-runs-schedules` makes this the requirement itself, and legacy achieves it by having the CronWorkflow's trigger POST to the very same launch endpoints.
- **One legacy quirk deliberately does not port.** Editing a schedule in legacy means delete-and-recreate the CronWorkflow, because a `CronWorkflow` cannot change its embedded trigger script in place (`routes/schedules.rs:674-745`), with all the consequences legacy documents: the suspended state must be fetched and restored by hand or every edit silently resumes a paused schedule (`:717-733`), and a recreate that fails leaves the schedule permanently gone with no rollback (`:826-832`). A database row is simply updated. **Do not port the dance, and do not port `ensure_schedule_plan_exists`'s pre-delete check** — it exists only to narrow that window.

---

### Task 17: Schedules schema, entities, and repository

**Files:**
- Create: `qa-runs/src/infra/storage/migrations/m20260813_000004_schedules.rs`
- Create: `qa-runs/src/infra/storage/entity/{schedule.rs,schedule_tick.rs}`
- Create: `qa-runs/src/domain/repos/schedules_repo.rs`
- Create: `qa-runs/src/infra/storage/schedules_sea_repo.rs`
- Modify: `migrations/mod.rs`, `entity/mod.rs`, `domain/repos/mod.rs`, `infra/storage/mod.rs`, `infra/storage/mapper.rs`, `domain/error.rs`

**Owns:** the above. A **second migration file**, not an edit to the first: migrations are append-only, and Phase A's is already applied wherever Phase A ran.

- [ ] **Step 1: The DDL.** Postgres branch:

```sql
CREATE TABLE IF NOT EXISTS qa_schedules (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    run_kind VARCHAR(16) NOT NULL,
    target_repo_id UUID NULL,
    target_path VARCHAR(1024) NULL,
    target_test_file VARCHAR(1024) NULL,
    target_custom_plan_id UUID NULL,
    platform_id UUID NULL,
    branch VARCHAR(512) NULL,
    cron VARCHAR(255) NOT NULL,
    -- Tri-state, ALWAYS written: 'true' | 'false' | 'auto'. Never NULL, and
    -- never omitted for the inherit case — an absent value would mean both
    -- "inherit" and "parallel", a distinction that cannot be recovered later
    -- (`manager/src/services/exclusivity.rs:60-79`). NOT NULL is the schema
    -- making that impossible rather than a convention hoping for it.
    exclusive_choice VARCHAR(8) NOT NULL DEFAULT 'auto',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    include_tags JSONB NOT NULL DEFAULT '[]',
    exclude_tags JSONB NOT NULL DEFAULT '[]',
    parameters JSONB NOT NULL DEFAULT '[]',
    last_fired_tick TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_schedules_tenant_name ON qa_schedules(tenant_id, name);
-- The evaluator's enumeration: enabled schedules across tenants.
CREATE INDEX IF NOT EXISTS idx_qa_schedules_enabled ON qa_schedules(enabled, last_fired_tick);
-- Residual recorded 2026-08-17, found by Task 17's code-quality review: this
-- index buys less than its name suggests, and shipped anyway. `list_enabled`
-- issues `WHERE enabled = true ORDER BY id`, so `last_fired_tick` is in no
-- predicate and no sort, the row is not covered, and the `ORDER BY id` forces
-- a sort whether or not the index is used — while a leading `enabled = true`
-- matching most rows is the classic shape a planner answers with a sequential
-- scan. The partial form that would help (`ON qa_schedules(id) WHERE enabled`)
-- is unavailable: DESIGN §3.7 already rejected partial indexes for this
-- subsystem because a `WHERE` predicate has no MySQL equivalent and one column
-- list must serve three dialects. Kept because schedules are few and the write
-- amplification is negligible; the doc comments must not claim a read benefit
-- the query shape cannot collect.

CREATE TABLE IF NOT EXISTS qa_schedule_ticks (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    schedule_id UUID NOT NULL REFERENCES qa_schedules(id) ON DELETE CASCADE,
    due_at TIMESTAMPTZ NOT NULL,
    claimed_by VARCHAR(255) NOT NULL,
    claimed_at TIMESTAMPTZ NOT NULL,
    -- The run this tick produced. NULL means the claim succeeded but the
    -- launch has not completed yet, or failed — the claim is deliberately
    -- durable either way, because a retried launch would violate
    -- exactly-once for a destructive run.
    run_id UUID NULL,
    error TEXT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
-- THE exactly-once mechanism (`cpt-cf-qa-nfr-scheduler-exactly-once`). The
-- claim is a UNIQUE constraint, not a convention: a second instance racing for
-- the same due time loses the insert and never launches. DESIGN §3.7 states
-- this explicitly — "`schedule_ticks` UNIQUE(tenant_id, schedule_id, due_at)
-- makes the exactly-once claim a constraint, not a convention" — and it is
-- tenant-prefixed for the same reason every unique index in this subsystem is
-- (DESIGN §3.7, "Every unique index is tenant-prefixed").
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_schedule_ticks_claim
    ON qa_schedule_ticks(tenant_id, schedule_id, due_at);
```

MySQL/SQLite branches as in Task 9, expression-form JSON defaults on MySQL. MySQL key width for the claim index: `36*4 + 36*4 + 4 = 292` bytes — fine.

- [ ] **Step 2: Entities**, same `Scopable`/`#[secure(...)]` shape as Task 9. `qa_schedule_ticks` has no `updated_at`: a tick row is immutable after its claim except for `run_id`/`error`, which are written once by the claiming instance — document that deviation in the migration comment, as `qa-environments` documents `qa_platform_leases`' deviation and DESIGN §3.7 documents qa-catalog's two.

- [ ] **Step 3: Mapper additions.** `exclusive_choice` codec — `Some(true) -> "true"`, `Some(false) -> "false"`, `None -> "auto"`, and the inverse with **fail-closed** decoding (an unrecognised value is `CorruptState`, never silently `auto`; conflating them is the exact mistake `exclusivity.rs:66-72` exists to prevent). Reuse the Task 9 target codec for the schedule's four target columns.

- [ ] **Step 4: `schedules_repo.rs`.** `create` / `get` / `get_by_name` / `list` / `update` / `delete`, plus the two the evaluator needs:
  - `list_enabled(conn, scope) -> Vec<(Schedule, Uuid)>` — schedule plus its `tenant_id`, for the cross-tenant enumeration (review lesson 6: enumerate carrying the tenant, then write under a per-tenant context).
  - `claim_tick(conn, scope, tenant_id, schedule_id, due_at, claimed_by) -> Result<Option<Uuid>, DomainError>` — insert the claim row; **`Ok(None)` on a unique violation** (someone else claimed it), `Ok(Some(tick_id))` on success. Use `ScopeError::is_unique_violation()`; never string-match, and **never let this surface as `Database` (500)** — a lost race is the normal, expected path on every non-leader instance and on every leader failover.
  - `record_tick_outcome(conn, scope, tick_id, run_id, error)`.

- [ ] **Step 5: Mapper and DB-backed tests.**

```text
- the_exclusive_choice_tri_state_round_trips             (all three, both directions)
- an_unknown_exclusive_choice_fails_closed               (never silently 'auto')
- a_schedule_round_trips_through_the_database
- a_duplicate_schedule_name_in_one_tenant_conflicts
- the_same_schedule_name_in_two_tenants_is_fine
- claiming_a_tick_twice_returns_none_the_second_time     (the unique violation, mapped — not a 500)
- two_tenants_may_claim_the_same_schedule_id_and_due_at  (only if that is reachable; if the FK makes it
                                                          unreachable, assert that instead and say so)
- list_enabled_carries_each_schedules_tenant
- a_disabled_schedule_is_not_enumerated
- deleting_a_schedule_cascades_its_ticks
```

- [ ] **Step 6: Verify.** `cargo test -p qa-runs schedule` → **10 passed**. Full gate. Commit: `feat(qa-runs): schedules schema, entities, and the exactly-once tick claim`.

---

### Task 18: TDD core 4 — cron evaluation and due-tick computation

Pure, like the other three cores, and for the sharpest reason yet: **exactly-once firing is untestable against a wall clock.** The rule has to be "given a schedule, a last-fired mark, and a `now`, which due times are outstanding?" — a pure function over an injected `now`.

**Files:**
- Create: `qa-runs/src/domain/cron.rs`
- Modify: `qa-runs/src/domain/mod.rs`, `qa-runs/src/domain/error.rs`

**Owns:** the above.

- [ ] **Step 1: Decide the catch-up policy explicitly, and record it.** This is the one behavioural question Phase B has to answer that legacy cannot: Argo's `CronWorkflow` has `concurrencyPolicy` and `startingDeadlineSeconds`, and legacy sets neither (check `create_cron_workflow` in `argo.rs` and confirm — **if it does set them, port those values instead of deciding here, and say so**). With them unset, Argo's default is to skip missed schedules rather than back-fill.

  **This plan chooses: fire at most one tick per evaluation, the most recent due time, and never back-fill.** Rationale, in the code comment: a destructive exclusive run is the worst possible thing to back-fill — a control plane down for six hours would, on restart, enqueue six hours of hourly cluster-upgrade runs onto one platform, and because the queue is strict FIFO they would drain one per tick with everything else stuck behind them. Skipping is recoverable (an operator relaunches); back-filling is not.

  > **Retracted 2026-08-17, found by Task 17's code-quality review.** This step originally ended: *"The skipped due times are still recorded as claimed-and-skipped tick rows, so 'why did my 03:00 run not happen?' has an answer."* **Nothing in this plan delivers that**, and it was wrong twice over. Task 19 Step 2 claims only the one `due_at` that `next_due` returns, so no tick row is ever written for a skipped occurrence; and Task 17's shipped repository surface is write-only — `claim_tick` and `record_tick_outcome` with no read — so even the rows that *do* exist, including the `error` text of a failed launch, cannot be read back by any method or any route in Task 20's table. **The honest statement is that a skipped occurrence leaves no record an operator can query.** `skipped_since` below still computes the skipped times, and that is worth having, but computing them in memory is not recording them. Closing this properly needs a scoped tick read and somewhere to surface it; both are tracked as a follow-up under Task 20 Step 3 rather than invented here.

  **The interlock this choice creates, which is not obvious from either side.** Because `next_due` returns the most recent occurrence at or before `now` and never back-fills, a claim that is won and then orphaned — the process dies before `record_tick_outcome` — self-heals at the next occurrence. An "earliest outstanding occurrence" evaluator would instead wedge that schedule permanently, because no method in Task 17's surface can advance the cursor past a lost claim. **Task 18 owes Task 17 this semantics, not merely a convenient one.**

  Record this in DECOMPOSITION 2.4 as a scope note in the same change (protocol step 4).

- [ ] **Step 2: Write the failing tests.**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn hourly() -> &'static str { "0 * * * *" }

    #[test]
    fn a_valid_expression_parses() {
        assert!(parse_cron(hourly()).is_ok());
        assert!(parse_cron("*/15 * * * *").is_ok());
        assert!(parse_cron("30 2 * * 1").is_ok());
    }

    #[test]
    fn an_invalid_expression_is_a_validation_error() {
        for bad in ["", "not a cron", "* * * *", "99 * * * *", "* * * * * * *"] {
            assert!(parse_cron(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    /// A schedule that has never fired: its first outstanding tick is the most
    /// recent due time at or before `now`, not the next one in the future.
    #[test]
    fn a_never_fired_schedule_is_due_at_the_most_recent_past_occurrence() {
        let due = next_due(hourly(), None, datetime!(2026-08-13 12:30 UTC)).unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 12:00 UTC)));
    }

    #[test]
    fn a_schedule_fired_at_its_latest_due_time_is_not_due_again() {
        let due = next_due(
            hourly(),
            Some(datetime!(2026-08-13 12:00 UTC)),
            datetime!(2026-08-13 12:30 UTC),
        )
        .unwrap();
        assert_eq!(due, None);
    }

    #[test]
    fn a_schedule_becomes_due_again_at_the_next_occurrence() {
        let due = next_due(
            hourly(),
            Some(datetime!(2026-08-13 12:00 UTC)),
            datetime!(2026-08-13 13:00 UTC),
        )
        .unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 13:00 UTC)));
    }

    /// The catch-up policy (Step 1): after a long outage only the MOST RECENT
    /// due time fires. Back-filling six hours of an hourly destructive suite
    /// onto one platform is the failure this prevents.
    #[test]
    fn a_long_outage_fires_only_the_most_recent_due_time() {
        let due = next_due(
            hourly(),
            Some(datetime!(2026-08-13 06:00 UTC)),
            datetime!(2026-08-13 12:30 UTC),
        )
        .unwrap();
        assert_eq!(
            due,
            Some(datetime!(2026-08-13 12:00 UTC)),
            "skipped occurrences are never back-filled"
        );
    }

    /// And the skipped ones are enumerable, so an operator can be told which
    /// runs did not happen rather than being left to infer it.
    #[test]
    fn skipped_occurrences_are_reportable() {
        let skipped = skipped_since(
            hourly(),
            datetime!(2026-08-13 06:00 UTC),
            datetime!(2026-08-13 12:30 UTC),
        )
        .unwrap();
        assert_eq!(
            skipped,
            vec![
                datetime!(2026-08-13 07:00 UTC),
                datetime!(2026-08-13 08:00 UTC),
                datetime!(2026-08-13 09:00 UTC),
                datetime!(2026-08-13 10:00 UTC),
                datetime!(2026-08-13 11:00 UTC),
            ],
            "the most recent due time is fired, not skipped, so it is excluded"
        );
    }

    /// `now` exactly on an occurrence boundary is due, not pending — otherwise
    /// a tick that lands precisely on the minute silently waits a full period.
    #[test]
    fn an_occurrence_exactly_at_now_is_due() {
        let due = next_due(hourly(), None, datetime!(2026-08-13 12:00 UTC)).unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 12:00 UTC)));
    }

    /// Evaluation is idempotent for a fixed (schedule, last_fired, now): the
    /// evaluator runs on every instance and the claim is what deduplicates, so
    /// this function must never depend on hidden state.
    ///
    /// **This test does not pin what its comment claims — plan defect found
    /// 2026-08-17 by Task 18's code-quality review, and recorded here because
    /// the body is the kind of thing that gets copied into the next plan.**
    /// Two calls microseconds apart with identical arguments cannot detect a
    /// clock read. Demonstrated rather than argued: a mutant shifting `now` by
    /// an hour on odd wall-clock seconds passed this test **eight runs out of
    /// eight**. The requirement in the comment is right; the body cannot test
    /// it. What discriminates is evaluating instants decades apart in one
    /// test, so a hidden clock read biases one of them — as Task 18 shipped
    /// alongside this one. Note even that has a ceiling of 4/8 against a
    /// coin-flip mutant, because half its executions *are* the correct
    /// function; the honest thing is to state the ceiling, not to claim the
    /// guard is complete.
    #[test]
    fn evaluation_is_pure_and_repeatable() {
        let args = (hourly(), Some(datetime!(2026-08-13 11:00 UTC)), datetime!(2026-08-13 12:30 UTC));
        let first = next_due(args.0, args.1, args.2).unwrap();
        let second = next_due(args.0, args.1, args.2).unwrap();
        assert_eq!(first, second);
    }

    /// A schedule whose last_fired mark is in the future — a clock stepping
    /// backwards, or a restored backup — must not fire, and must not panic.
    #[test]
    fn a_last_fired_mark_in_the_future_yields_nothing() {
        let due = next_due(
            hourly(),
            Some(datetime!(2026-08-14 12:00 UTC)),
            datetime!(2026-08-13 12:30 UTC),
        )
        .unwrap();
        assert_eq!(due, None);
    }

    /// Everything is UTC. A schedule is not given a timezone here, so this
    /// pins that decision as a test rather than leaving it to be discovered.
    #[test]
    fn all_evaluation_is_utc() {
        let due = next_due("0 0 * * *", None, datetime!(2026-08-13 00:30 UTC)).unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 00:00 UTC)));
    }
}
```

- [ ] **Step 3: Run to confirm failure**, then implement `parse_cron`, `next_due`, and `skipped_since` over the `cron` crate. Module doc must state: five-field expressions, UTC only, at-most-one-tick-per-evaluation with the Step 1 rationale, and that the function is pure because the claim — not the evaluator — is what makes firing exactly-once. Add `DomainError::InvalidCron { expression: String, message: String }`.

  **A note on the `cron` crate's field count:** many Rust cron crates expect six or seven fields (with seconds and/or years), not five. **Check what the pinned version accepts before writing `parse_cron`**, and if it is not five-field, normalise the input (prepend `0` for seconds) and say so in the doc comment — the five-field form is what `cpt-cf-qa-fr-runs-schedules` and every legacy schedule use.

- [ ] **Step 4: Verify.** `cargo test -p qa-runs cron` → **12 passed**. **(unverified prediction on `an_invalid_expression_is_a_validation_error`** — which of those five strings the pinned crate rejects depends on its strictness. Run it, and if the crate accepts one, either tighten `parse_cron` yourself or drop that case from the list and say which.**)** Full gate. Commit: `feat(qa-runs): cron evaluation with an explicit no-back-fill policy`.

---

### Task 19: Schedule service and the leader-elected firing task

**Files:**
- Create: `qa-runs/src/domain/service/{schedules.rs,schedules_tests.rs}`
- Modify: `qa-runs/src/domain/service/mod.rs`, `qa-runs/src/gear.rs` (second ticker), `qa-runs/src/config.rs` (two knobs), `qa-runs/src/domain/system_actor.rs` (the two Phase B factories, already declared in Task 13)

**Owns:** the above.

- [ ] **Step 1: Implement the service.** CRUD with PEP → scope → repo, `Validation` on an empty name and on an unparseable cron (validate the expression at **create and update**, not only at fire time — a schedule that cannot be parsed should fail loudly when it is written, not silently never fire). `update` is a plain row update: none of legacy's delete-and-recreate consequences apply (see the Phase B preamble), and in particular **`enabled` survives an edit by construction** rather than by the hand-rolled save-and-restore legacy needs (`routes/schedules.rs:717-733`).

- [ ] **Step 2: Implement `fire_due_schedules()`.** The tick, in order:
  1. Enumerate enabled schedules under `system_actor::for_schedule_tick()` (nil tenant, enumeration only), each carrying its `tenant_id`.
  2. Per schedule: `cron::next_due(cron, last_fired_tick, now)`. `None` → nothing to do.
  3. `Some(due_at)` → mint `system_actor::for_schedule_fire(tenant_id)` and `claim_tick(...)`. **`Ok(None)` means another instance won the race: log at DEBUG and move on.** This is the normal path on failover, not an error.
  4. On a successful claim: build a `LaunchRequest` from the schedule — **`exclusive: schedule.exclusive_choice` goes into the `launch` field**, because a schedule's choice *is* the launch tier (`exclusivity.rs:113-119`) — and call the **same** `LaunchService::launch`. One creation path.
  5. `record_tick_outcome(tick_id, run_id, error)` and update `last_fired_tick`, then publish `qa.schedule.fired`.
  6. A launch failure is recorded on the tick row and **does not retry**: the claim is durable on purpose. Comment it — a retried launch is a second destructive run, which is precisely what exactly-once exists to prevent. (Legacy has the same posture in a different corner: a JIRA auto-rerun rejected with 429 "is dropped permanently and not retried", guide lines 225-227.)

- [ ] **Step 3: The `qa.schedule.fired` event.** DESIGN §3.3's table lists it; Task 12 defined seven events and this is the eighth. Add `LifecycleEvent::ScheduleFired { schedule_id, due_at, run_id }` and its payload, subjecting on the **schedule** id (this is the one event whose subject is not a run — it is about the schedule, and a consumer watching one schedule's history wants them grouped). Extend Task 12's `every_lifecycle_event_maps_to_a_distinct_type_id` test to cover it.

- [ ] **Step 4: The second ticker, under leader election.** Add to `gear.rs`'s `serve` alongside the dispatcher. **The scheduler is the clearer of the two leader-election cases**: `cpt-cf-qa-nfr-scheduler-exactly-once` names it as a requirement with "Verification Method: Integration tests with multiple concurrent scheduler instances" (PRD §5.2). Note in the doc comment that the tick claim is the *primary* guarantee and leadership is defence in depth — the claim holds even with every replica evaluating, which is exactly what makes the failover test writable.

  Config: `schedule_interval_seconds` (default 60, floor 10) and `scheduler_enabled` (default true). Both enforced, both in `config/qa-platform.yaml` (Task 20 Step 3).

- [ ] **Step 5: Tests.** The multi-instance ones are the point.

```text
CRUD
- an_unparseable_cron_is_rejected_at_create
- an_unparseable_cron_is_rejected_at_update
- an_empty_name_is_rejected
- a_disabled_schedule_never_fires
- editing_a_schedule_preserves_its_enabled_state          (the legacy quirk that does not port)
Firing
- a_due_schedule_fires_through_the_launch_service         (asserted on the recording admitter — the same
                                                            path a manual launch takes)
- the_stored_exclusivity_choice_arrives_as_the_launch_tier
- an_auto_choice_arrives_as_none_and_is_inherited
- an_explicit_false_choice_suppresses_an_exclusive_test_file
- a_schedule_that_is_not_due_does_not_fire
- last_fired_tick_advances_after_a_successful_fire
- the_produced_run_records_its_schedule_id_and_scheduled_source
Exactly-once (cpt-cf-qa-nfr-scheduler-exactly-once)
- two_instances_evaluating_the_same_due_time_produce_exactly_one_run
- the_losing_instance_treats_the_lost_claim_as_normal_not_an_error
- a_failover_mid_fire_does_not_produce_a_second_run       (claim durable, launch not retried)
- a_launch_failure_is_recorded_on_the_tick_and_not_retried
- a_long_outage_fires_once_not_once_per_missed_occurrence (end to end, through the service)
Tenancy
- a_schedule_fires_under_its_own_tenant                    (a nil-tenant fire is denied)
- another_tenants_schedule_is_invisible
```

`two_instances_evaluating_the_same_due_time_produce_exactly_one_run` must be **deterministic**, not timing-dependent: run both evaluations sequentially against the same store and assert the second's claim returns `None` and no second launch reached the admitter. Then **verify it by breaking the fix** — drop the unique index (or bypass `claim_tick`) and watch two runs appear.

- [ ] **Step 6: Verify.** `cargo test -p qa-runs schedule` → **+20**. Full gate. Report which exactly-once test you verified by breaking. Commit: `feat(qa-runs): schedule service and leader-elected exactly-once firing`.

---

### Task 20: Schedule REST, registration, and the Phase B close-out

**Files:**
- Create: `qa-runs/src/api/rest/handlers/schedules.rs`, `qa-runs/src/api/rest/routes/schedules.rs`
- Modify: `qa-runs/src/api/rest/{dto.rs,error.rs}`, `handlers/mod.rs`, `routes/mod.rs`, `qa-runs/src/domain/local_client/client.rs`, `config/qa-platform.yaml` (**corrected 2026-08-17: at the workspace root, not under `apps/cf-gears-example-server/`, which holds no such file — the path this plan gave does not exist, and the plan's own smoke command already pointed at the real one**)
- Modify: `gears/qa-platform/docs/DECOMPOSITION.md` (status + the Task 18 scope note)

**Owns:** the above.

- [ ] **Step 1: Routes.**

| Method | Path | Notes |
|---|---|---|
| GET | `/qa/v1/schedules` | `json_array_response_with_schema::<ScheduleDto>()` |
| POST | `/qa/v1/schedules` | 201; `error_400` (invalid cron), `error_409` (duplicate name) |
| GET | `/qa/v1/schedules/{id}` | |
| PUT | `/qa/v1/schedules/{id}` | Full replace, matching `NewSchedule`. Not PATCH: `serde_with` is absent, so a tri-state patch on `exclusive_choice` — itself already a tri-state — would need `Option<Option<Option<bool>>>`. A full replace with documented semantics is the honest shape, and it matches how the source system's edit form behaves anyway. |
| DELETE | `/qa/v1/schedules/{id}` | `.no_content_response()` |

`exclusive_choice` on the wire is the string `"true"` / `"false"` / `"auto"`, **not** a nullable boolean — the same three-token vocabulary the source system stores, so an operator reading either system sees the same values, and so `null` can never be mistaken for `false`. Document that on the DTO field, and register the decode failure as `error_400`.

- [ ] **Step 2: Complete the local client.** Every `QaRunsClientV1` method now has an implementation; delete the last `#[allow(dead_code)]` markers from Phase A and confirm none remain:

```bash
grep -rn 'dead_code' gears/qa-platform/qa-runs/
```

Expected: only per-item attributes that carry a written `reason` and are not "wire this later" markers.

> **The grep this step originally gave was broken in both directions — corrected 2026-08-17 after Task 20 ran it.** It read `grep -rn 'allow(dead_code)\|// Task [0-9]' …` and expected **no output**. It returned 25 lines and could never have returned none: 23 were historical `// Task N` prose scattered through the gear since Task 5, because the pattern matches inside `///` doc comments as well as `//` ones, and deleting them would mean deleting correct history. **The serious half is the false negative: the single-line `allow(dead_code)` pattern would not have matched the very attribute this step exists to find**, because Task 19 wrote it multi-line with a `reason =`. A check that fails noisily on what does not matter while staying silent on what does is worse than no check.
>
> **The verification that actually works is the compiler.** Removing the attribute and letting `cargo clippy --all-targets -- -D warnings` decide passes only if every CRUD method genuinely has a caller — which is the property this step is about. The grep above is a supplement, not the proof.

- [ ] **Step 3: Config and docs.** Add `schedule_interval_seconds` and `scheduler_enabled` to `config/qa-platform.yaml`. In `DECOMPOSITION.md`: set 2.3's and 2.4's feature and requirement checkboxes to `[x]` for what shipped; add 2.3's **new** "Tracked follow-ups" subsection (it has none today) carrying the items this plan deferred; and add 2.4 the Task 18 catch-up scope note.

  Tracked follow-ups to record under 2.3, each with the reason it was deferred rather than as a bare bullet:
  - **The dev-deployment system-actor grant** (Task 14 Step 5) — the dispatcher is inert under `static-authz`, mitigated by an actionable WARN per pass. Whether the dev stack grows a policy plugin that can grant a system subject is a deployment decision, not this gear's.
  - **`qa.platform.version_changed` is still unowned.** qa-runs now owns the vocabulary, but the poller that would publish that event does not exist (2.1's unbuilt scope, recorded in Task 1 Step 5). Retrofit when 2.7 lands.
  - **Archived logs are p2.** `cpt-cf-qa-fr-runs-logs` needs file-storage for the archive half; `log_storage_ref` exists and stays NULL until 2.7. The live half (SSE) ships.
  - **Real execution is 2.7, and 2.7 is blocked behind an unbuilt gear.** `gears/serverless-runtime/` contains zero `.rs` files — it is docs-only, with a DECOMPOSITION whose single feature is an unbuilt `gear-scaffold`. So the ~2.5k blocked LOC can be *replaced* without waiting but cannot be *validated* until that gear exists. Any schedule for 2.7 must include serverless-runtime's own build.
  - **`git_plans` remains unported** (this plan's second flagged decision) — so legacy's three-way rerun discrimination collapses to two-way and its `Unresolvable` 404 is unreachable here.
  - **A schedule's tick history is unreadable, so a skipped or failed occurrence leaves no answer an operator can query** (added 2026-08-17, from Task 17's code-quality review). `qa_schedule_ticks` is written by `claim_tick` and `record_tick_outcome` and read by nothing: the `error` column recording why a launch failed has no reader, and an orphaned claim — won, then the process died before the outcome was recorded — leaves `run_id` and `error` NULL with no way to enumerate such rows. Closing it needs a scoped `list_ticks(scope, schedule_id, limit)` on the repository and a `GET /qa/v1/schedules/{id}/ticks` to surface it. Deferred rather than folded into Task 20 because the tick row's operator-facing shape is a product question this plan never asked — in particular whether a skipped occurrence should be claimed at all, which Task 18's retraction leaves open.
  - **The `serde-saphyr` vs `serde_yaml 0.9` parity risk is inherited, not closed.** qa-runs consumes `plan.yaml` parse results through the catalog SDK, so DECOMPOSITION 2.2's unverified-engine-swap risk now has a *runtime* consumer: a duplicate key or a YAML-1.1 `yes` parsed differently changes which tests a run executes. The cheap mitigation is still a shared fixture corpus in qa-catalog's tests.

- [ ] **Step 4: Full verification gate + live smoke.** Repeat Task 16 Steps 8–9, plus:

```bash
curl -s localhost:PORT/openapi.json | jq '.paths | keys | map(select(startswith("/qa/v1/schedules")))'
```

Expected: `/qa/v1/schedules` and `/qa/v1/schedules/{id}`.

- [ ] **Step 5: The final whole-gear review and the independent spec-coverage audit.** **Neither is optional.** Per-task reviews structurally cannot catch "the plan under-delivered against the PRD": qa-catalog's audit found two FR verbs that were never built and three silent regressions against a frozen contract — *after ten clean task reviews*.

  Run **two separate** reviews with different briefs:
  1. **Whole-gear review** — security, concurrency, error mapping, tenancy, hygiene across the finished gear.
  2. **Independent spec-coverage audit** — a reviewer who has **not** seen this plan, given only the PRD/DESIGN/DECOMPOSITION requirement ids and the frozen guide, asked: for each requirement, point at the code that satisfies it, and for each rule in `exclusive-runs-and-the-queue.md`, point at the test that pins it. Anything it cannot find is a finding. Give it the "Spec coverage" table below **only after** it reports, as a cross-check — never before, or it will grade the table instead of the code.

- [ ] **Step 6: Squash Phase B** into one commit on `feature/qa-platform-specs`.

---

## Spec coverage

Withheld from the Step 5 auditor until after it reports.

| Requirement | Tasks | Note |
|---|---|---|
| `cpt-cf-qa-fr-runs-launch` | 4, 13, 14, 16 | Three outcomes: 200 / 202 / 429 with the limit named. Branch resolution and `test_version` are parity spec §3.4 rules 1 and 6. |
| `cpt-cf-qa-fr-runs-params` | 8a, 13, 15 | Eleven reserved names + the two caps D3 added to the PRD. Re-validated at re-run. |
| `cpt-cf-qa-fr-runs-env-assembly` | 8c, 14 | Four tiers plus the platform-metadata position legacy has and the PRD omitted. |
| `cpt-cf-qa-fr-runs-exclusivity` | 5, 13 | The whole cascade, the tag-filter asymmetry, and the DECOMPOSITION 2.2 tier reconciliation. |
| `cpt-cf-qa-fr-runs-queue` | 6, 9, 10, 14, 15 | Seven states, FIFO, TTL, depth + concurrency limits (D1). Platformless runs never queued. |
| `cpt-cf-qa-fr-runs-dispatch` | 7, 10, 14 | Boot recovery vs tick reconciliation kept distinct; the orphan-timeout guard. |
| `cpt-cf-qa-fr-runs-cancel-rerun` | 15 | Upward-only exclusivity inheritance; re-validation. |
| `cpt-cf-qa-fr-runs-timeout` | 8b(timeout chain in 13), 14, 16 | Control-plane enforcement + executor backstop + the config ceiling. |
| `cpt-cf-qa-fr-runs-events` | 12, 19 | Eight events; qa-runs owns the vocabulary (Task 1 Step 6). |
| `cpt-cf-qa-fr-runs-schedules` | 17, 18, 19, 20 | One creation path; tri-state choice as the launch tier. |
| `cpt-cf-qa-nfr-scheduler-exactly-once` | 17, 19 | UNIQUE(tenant, schedule, due_at) as a constraint; multi-instance tests. |
| `cpt-cf-qa-nfr-dispatch-latency` | 14, 16 | 15 s tick; cross-checked against the NFR in Task 14 Step 2.6. |
| `cpt-cf-qa-nfr-scale` | 16 | OData + paging on the runs and queue collections (Task 16 Step 4). |
| `cpt-cf-qa-principle-semantics-parity` | 5, 6, 7, 8 | The four pure cores, all written test-first from legacy. |
| `cpt-cf-qa-principle-db-first-state` | 9, 10 | `qa_runs` is authoritative; net-new, nothing to port. |
| `cpt-cf-qa-principle-executor-port` | 11 | Port shaped from the legacy submission contract, mock in p1. |
| `cpt-cf-qa-seq-launch` | 13, 14, 15 | DESIGN §3.6's sequence, end to end against the mock. |
| **Deferred, recorded** | 20 Step 3 | `fr-runs-execute`, `fr-runs-results-ingest` (real executor), `fr-runs-logs` (archive half) are 2.7. |

## Plan self-review notes (already applied)

- **Spec coverage:** every p1 requirement in DECOMPOSITION 2.3 and 2.4 has a task; the three p2 ones are 2.7's and are recorded as deferred rather than silently dropped.
- **Legacy-truth checkpoints on *every* frozen contract, not just the named ones.** qa-catalog's plan mandated checkpoints for TEST_META and the discovery glob but not for the `plan.yaml` parser — and that is precisely where it drifted, with one regression *pinned by a test* that enshrined it. Every rule in `exclusive-runs-and-the-queue.md` is covered by a numbered legacy check: precedence and the tag asymmetry (Task 5 Step 1), admission/FIFO/limits/TTL/blocker text (Task 6 Step 1), phase derivation and recovery (Task 7 Steps 1–2), the reserved list and caps (Task 8a Step 1), env precedence (Task 8c Step 1), the six launch rules (Task 13 Step 1), the lock's scope and the tick's ordering (Task 14 Steps 1–2), re-run inheritance and force-start's asymmetry (Task 15 Steps 1–3), the tri-state codec (Phase B preamble + Task 17 Step 3).
- **Expected-output lines are per command and marked when predicted.** Six are flagged **(unverified prediction)** — Task 2 Steps 6–7, Task 8b Step 4, Task 13 Step 9, Task 18 Step 4 — because they depend on a crate's strictness or on a count the implementer must read first. A wrong expected output is worse than none: it teaches the implementer to distrust the plan or, worse, to "fix" something already right (four of the parity plan's expected-output claims were wrong).
- **File lists are compiler-derived where it matters, and stated as owned.** Two parity-plan tasks had incomplete file lists that surfaced as mid-execution surprises. Tasks 9, 10, 16, 17, 20 each name the `mod.rs` files they modify, which is where those omissions happened.
- **No struct is retyped for replacement.** The parity plan's Task 3 replaced a whole entity struct and silently omitted `Product.name`, dropping a NOT NULL column with a unique index; the implementer's honest report caught it. Here Task 2 gives *additive* instructions plus an expected-final-field checklist for `plan_yaml.rs`, and Task 12's enum is given in full rather than sketched.
- **Task ordering checked for mutual dependencies.** Parity Tasks 11 and 12 were ordered so neither could be verified first. Every adjacent pair here is verifiable alone: the four pure cores need nothing; Task 9's schema is only *exercised* by Task 10, which is why Task 9's verification step says so explicitly instead of claiming a green build proves the schema; Task 13 compiles standalone because it defines the `Admitter` seam that Task 14 implements.
- **No compiler behaviour is predicted untested.** Task 9 Step 6 states outright that `cargo build` proves nothing about a schema, because SeaORM entities are hand-written structs whose table and column names are runtime strings — the exact claim the parity plan got wrong.
- **Type consistency:** `Occupancy` (local projection) vs `LeaseState` (SDK) — converted only in `Occupancy::from_lease`. `ExclusiveTier` and `RunState` live in the SDK and are used unqualified everywhere. `FileMeta` (local) vs `qa_catalog_sdk::TestFileMeta` — projected in the launch service, and the `Option<bool>` → `bool` reconciliation happens in exactly one place (`file_declares_exclusive`). The two cancel spellings (`canceled` for a run, `cancelled` for a queue row) are deliberate and pinned by a test so neither gets "fixed".
- **Known gap, stated rather than hidden:** the plan cannot verify the `OperationBuilder` multi-success-status registration (Task 16 Step 3), the SSE response registration (Step 3), the OData helper (Step 4), or the `cluster-sdk` leader-election shape (Step 5) without reading code this plan did not open. Each of those four steps therefore says **stop and report** rather than guessing an API — which is the correct failure mode for a plan, not a defect in one.

## Execution handoff

Plan complete and saved to `gears/qa-platform/docs/plans/2026-08-13-qa-runs-gear.md`.

Recommended: **subagent-driven development**, exactly as the two shipped gears and the parity pass were built — a fresh implementer per task, a spec-compliance review after each, an **additional security-hardening review** for every security-sensitive slice (Tasks 9, 10, 13, 14, 15, 16 — persistence, services, anything touching authorization, secrets, or client-supplied paths), fix rounds returned to the *same* implementer so context is preserved, and the two independent reviews at the end (Task 20 Step 5).

Every dispatch must name: the files the task owns, the files later tasks own, and the exact errors expected to remain. And it must invite pushback explicitly — "if legacy disagrees with this task's description, stop and report" — because a subagent that refuses an instruction is working correctly. Three of the parity pass's most useful findings came from exactly that: one proved a stated verification premise was impossible, one caught a dropped struct field by reporting honestly what it had removed, and one escalated rather than reaching into a file it did not own.

