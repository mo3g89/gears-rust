# QA Platform review remediation — quality (Phases 5, 6, 9) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the type-safety, layering, scale and cleanup findings from the
QA Platform review that survive the product-plugin rework.

**Architecture:** Three phases. Phase 5 encodes rules the code already states in
prose — a three-state exclusivity flag, SDK enums on the wire, a domain layer
that does not import its own REST layer. Phase 6 gives qa-environments the
paging and filtering the other two gears have, because the scale NFR's first
number is a qa-environments collection. Phase 9 settles the decisions the review
left open and deletes what is dead.

**Tech Stack:** Rust 2024 (`cf-gears-toolkit`, `toolkit-odata`, `sea-orm`,
`axum`), React + vitest, `cargo-nextest`.

**Spec:** `gears/qa-platform/docs/superpowers/specs/2026-09-05-review-remediation-design.md`

**Prerequisite:** `2026-09-05-review-remediation-core.md` (Phases 1–4) must be
complete. Phase 1's CI gates are what verify this plan, and Task 27 below
depends on Phase 1's `helm-tests` target.

## Global Constraints

- **Branch:** the same branch off `feature/qa-product-plugins`, one commit per
  task.
- **The PDP resource string for environments is `"qa.platform"`**, even though
  the aggregate, the REST route and the UI all say *environment*
  (`qa-environments/src/domain/service/mod.rs:90-99`). Task 24 touches the
  routes; it must not touch that string.
- **TDD**: the failing test first, run it red, then fix.
- **Wire-visible changes** (Tasks 21, 22, 24) require
  `make ui-contract` against a live compose stack before release. Note it in
  each commit; do not run it in CI, which has no gears.
- **`make clippy` stays green**, including `cargo hack clippy --each-feature`.
- **Do not rename `resolved_exclusive` or `QueueEntry::exclusive`.** Those are
  resolved decisions and correctly `bool`; only the three-state *inputs* change.

---

# Phase 5 — Types and layering

Findings #10, #11, #12, #14, #15, #16, #17, #25, #34, #35, #37, #38, #39.

---

### Task 19: One closed exclusivity enum, replacing three spellings

**Findings:** #10 and #11, both HIGH (`RUST-TYPE-001`).

The three states are already documented correctly in two places.
`qa-runs-sdk/src/models.rs:205-207`: *"The launch exclusivity tier: three-state.
`None` means 'inherit', which is **not** `Some(false)` — that distinction is the
whole reason the upper tiers are `Option<bool>`."* And
`qa-catalog-sdk/src/models.rs:102-108` describes the same rule for plans. What
neither has is a type that enforces it.

The rework introduced `pub type ExclusiveFlag = Option<bool>;`
(`qa-catalog-sdk/src/models.rs:60`) — a name for the concept, and nothing the
compiler can check. `ExclusiveFlag::default()` is still `None`, `Inherit` and
`Shared` are still the same type, and `if flag.unwrap_or(false)` still compiles.

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog-sdk/src/models.rs:60,80,89`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog-sdk/src/lib.rs:13` (re-export)
- Modify: `gears/qa-platform/qa-runs/qa-runs-sdk/src/models.rs:207`
- Modify: every call site — find them with the grep in Step 4
- Test: `qa-catalog-sdk`'s and `qa-runs-sdk`'s test modules

**Interfaces:**
- Produces:

```rust
pub enum Exclusivity { Inherit, Exclusive, Shared }
```

  on `qa_catalog_sdk`, re-exported by `qa_runs_sdk`. Used by `Plan::exclusive`,
  `TestFileMeta::exclusive` and `LaunchRequest::exclusive`. Task 20 does not
  consume it.

- [ ] **Step 1: Write the failing test**

```rust
/// **The three exclusivity states are three values, not two plus a null.**
///
/// `None` means *inherit* and `Some(false)` means *explicitly shared*, and the
/// difference decides whether a destructive suite gets the platform to itself.
/// Both SDKs documented that rule in prose and typed it as `Option<bool>`, so
/// `flag.unwrap_or(false)` -- which collapses inherit into shared -- compiled
/// everywhere. `ExclusiveFlag` gave the concept a name without giving the
/// compiler anything to check. Review findings #10 and #11.
#[test]
fn inherit_and_shared_are_distinguishable_without_convention() {
    assert_ne!(Exclusivity::Inherit, Exclusivity::Shared);
    // The trap the alias permitted: a default that silently means "shared".
    assert_eq!(Exclusivity::default(), Exclusivity::Inherit);
}

/// The wire form must not change: these values are persisted and are sent by
/// CI callers that this change does not get to break.
#[test]
fn the_wire_form_is_unchanged() {
    assert_eq!(serde_json::to_string(&Exclusivity::Inherit).unwrap(), "null");
    assert_eq!(serde_json::to_string(&Exclusivity::Exclusive).unwrap(), "true");
    assert_eq!(serde_json::to_string(&Exclusivity::Shared).unwrap(), "false");
    assert_eq!(
        serde_json::from_str::<Exclusivity>("null").unwrap(),
        Exclusivity::Inherit
    );
}
```

The second test is the constraint that shapes the implementation: this is a
**type** change, not a wire change. A stored `plan.yaml` with `exclusive: true`
and a CI caller posting `{"exclusive": null}` must both keep working.

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p qa-catalog-sdk inherit_and_shared_are_distinguishable
```

Expected: FAIL to compile — `Exclusivity` does not exist.

- [ ] **Step 3: Add the enum**

In `qa-catalog-sdk/src/models.rs`, replacing the alias at `:60`:

```rust
/// A three-state exclusivity declaration: the plan's, the test file's, or the
/// launch request's.
///
/// **`Inherit` is not `Shared`.** `Inherit` means "this tier says nothing, ask
/// the tier below"; `Shared` means "this tier says explicitly: do not take the
/// platform". Collapsing them resolves a destructive suite as parallel, which
/// is the failure `domain::exclusivity`'s operator warning exists for.
///
/// This was `Option<bool>` behind a `pub type ExclusiveFlag` alias. The alias
/// named the concept and enforced nothing: `flag.unwrap_or(false)` compiled and
/// meant "treat inherit as shared". Review findings #10 and #11.
///
/// The serde representation is deliberately unchanged from the `Option<bool>`
/// it replaces -- `null` / `true` / `false` -- because these values are
/// persisted in `plan.yaml` files and posted by CI callers. This is a type
/// change, not a wire change; `the_wire_form_is_unchanged` pins that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Exclusivity {
    #[default]
    Inherit,
    Exclusive,
    Shared,
}
```

Implement `Serialize`/`Deserialize` by hand (or via
`#[serde(from/into = "Option<bool>")]`) so the wire form is exactly the old one.
Add `From<Exclusivity> for Option<bool>` and the reverse for call sites that
still need the old shape during migration — and delete both once Step 4 is done,
so they cannot become a permanent bypass.

- [ ] **Step 4: Migrate every call site**

```bash
grep -rn 'ExclusiveFlag\|exclusive: Option<bool>\|\.exclusive\b' \
  gears/qa-platform --include=*.rs | grep -v resolved_exclusive
```

Work through every hit. **Watch for `unwrap_or(false)` and
`unwrap_or_default()` on the old type — each one is a live instance of the bug**
and should become an explicit `match` on the three variants. Note any you find
in the commit message.

`resolved_exclusive: bool` (`qa-runs-sdk/models.rs:347`) and
`QueueEntry::exclusive: bool` (`:593`) stay `bool` — they are resolved
decisions, not inputs. `LaunchRequest::exclusive` (`:207`) becomes
`Exclusivity`.

- [ ] **Step 5: Run everything**

```bash
cargo nextest run -p qa-catalog-sdk -p qa-runs-sdk -p qa-catalog -p qa-runs --lib
make test-qa-runs-pg test-qa-catalog-git
```

Expected: PASS. The exclusivity resolution tests in
`qa-runs/src/domain/exclusivity.rs` are the ones that matter most here.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform
git commit -m "refactor(qa-platform)!: a closed enum for the three exclusivity states

None meant 'inherit' and Some(false) meant 'explicitly shared', and the
difference decides whether a destructive suite gets the platform to itself.
Both SDKs stated that rule in prose and typed it as Option<bool>, so
unwrap_or(false) -- which collapses inherit into shared -- compiled everywhere.
The rework's ExclusiveFlag alias named the concept and enforced nothing.

The serde form is byte-identical: null/true/false. These values live in
plan.yaml files and are posted by CI callers, so this is a type change, not a
wire change, and the_wire_form_is_unchanged pins it.

resolved_exclusive and QueueEntry::exclusive stay bool -- they are resolved
decisions, not inputs. Review findings #10 and #11."
```

---

### Task 20: SDK enums on the wire

**Findings:** #12 (HIGH), #34 and #35 (MEDIUM).

Three DTOs publish bare strings for closed sets that already have enums beside
them:

- `ClusterHealthView.status: String`
  (`qa-environments-sdk/src/models.rs:371`) for five documented values plus
  `Unreachable`. Its own doc comments (`:372-381`) describe the value space and
  the invariants tying `status_message` and `counts` to it — invariants a type
  could hold and a `String` cannot.
- `RunDto.state` / `exclusive_tier` / `source`
  (`qa-runs/src/api/rest/dto.rs:344,349,355`, and `:556,559` on a second DTO).
- The saved-view `scope: String` (`qa-insights/src/api/rest/dto.rs:1098` and six
  siblings) while `SavedViewScope` exists.

**Files:**
- Modify: `qa-environments/qa-environments-sdk/src/models.rs:356-381`
- Modify: `qa-runs/qa-runs/src/api/rest/dto.rs:344,349,355,556,559`
- Modify: `qa-insights/qa-insights/src/api/rest/dto.rs` (seven `scope` fields)
- Modify: `qa-platform-ui/src/api/generated/openapi.d.ts` (hand-edit; see the Global Constraints note)

**Interfaces:**
- Produces: `ClusterStatus` on `qa-environments-sdk`. `RunDto`, the queue DTO
  and the saved-view DTOs serialize the existing `RunState`, `ExclusiveTier`,
  `RunSource` and `SavedViewScope`.

- [ ] **Step 1: Write the failing tests**

For each of the three, a pair: the enum round-trips through the exact wire
strings the `String` carried, and an unknown value is rejected rather than
accepted.

```rust
/// **The wire strings are unchanged; only the type is.**
///
/// `RunDto.state` was a `String` while `sdk::RunState` sat beside it, so a
/// typo'd state reached a UI that switches on the value. The persisted
/// spellings are `RunState::as_str`'s and stay exactly as they are — this is what makes
/// the change safe to ship. Review finding #34.
#[test]
fn run_dto_state_serializes_to_the_persisted_spelling() {
    let dto = RunDto { state: RunState::Succeeded, ..run_dto_fixture() };
    let body = serde_json::to_value(&dto).unwrap();
    assert_eq!(body["state"], "succeeded", "the wire spelling must not change");
}

/// The half the `String` could not give: an unknown state is a decode error,
/// not a value the UI has to defend against.
#[test]
fn an_unknown_run_state_is_rejected() {
    let mut body = serde_json::to_value(run_dto_fixture()).unwrap();
    body["state"] = serde_json::json!("halfway");
    assert!(serde_json::from_value::<RunDto>(body).is_err());
}
```

**Read each enum's existing `as_str` before asserting a spelling.**
`RunFilterField::State`'s doc (`qa-runs/src/infra/storage/odata.rs:42-46`) warns
that the persisted values are lowercase and the Rust variant names are not — a
test asserting `"Succeeded"` would be asserting the bug.

- [ ] **Step 2: Run to verify they fail**

```bash
cargo nextest run -p qa-runs -p qa-insights -p qa-environments-sdk an_unknown_
```

Expected: FAIL — the `String` accepts anything.

- [ ] **Step 3: Add `ClusterStatus` and retype the three DTOs**

`ClusterStatus` needs an `Unreachable` variant, and its doc must carry the two
invariants currently written on the `String` fields: `status_message` is
populated only when `Unreachable`, and `counts` is empty when `Unreachable`
(D-CH-3). Consider whether the enum should *hold* those — a
`Unreachable { message: String }` variant makes the invariant unbreakable — and
if you decide against it, say why in the doc.

- [ ] **Step 4: Run everything and fix the UI**

```bash
cargo nextest run -p qa-runs -p qa-insights -p qa-environments -p qa-environments-sdk --lib
grep -rn "state ===\|scope ===\|exclusive_tier" gears/qa-platform/qa-platform-ui/src/
make ui-lint ui-test ui-build
```

The UI already switches on these strings; the generated types get narrower, so
any switch missing a case now fails `tsc`. Fix those — a missing case is a
latent UI bug this change surfaces.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform
git commit -m "refactor(qa-platform)!: publish SDK enums instead of bare strings

ClusterHealthView.status was a String for five documented values plus
Unreachable, with the invariants tying status_message and counts to it written
in prose. RunDto.state/exclusive_tier/source and the saved-view scope were bare
strings while RunState, ExclusiveTier, RunSource and SavedViewScope sat beside
them.

The wire spellings are unchanged -- RunState::as_str's lowercase forms, not the
Rust variant names -- so this is a type change. What it adds is that an unknown
value is now a decode error rather than something the UI defends against, and
tsc now catches a switch missing a case.

Wire-visible: run make ui-contract against a live stack before release.
Review findings #12, #34, #35."
```

---

### Task 21: The domain layer must not import its own REST layer

**Findings:** #15, #16 (HIGH, `TOOLKIT-CORE-002`) and #39 (MEDIUM,
`RUST-MOD-001`).

The rework moved the local clients from `infra/` to `domain/`, which makes these
worse rather than better — a *domain* module now reaches into `api::rest`:

- `qa-runs/src/domain/local_client/client.rs:36` —
  `use crate::api::rest::error::{as_queue_error, as_schedule_error};`
- `qa-insights/src/domain/local_client/client.rs:73` —
  `use crate::api::rest::error::as_jira_error;`
- `qa-runs/src/domain/local_client/client.rs:37` —
  `use crate::infra::ConcreteAppServices;` (#39)

The mappers themselves are right: `as_jira_error`'s module doc explains why a
bare `Into` is wrong for this caller. What is wrong is where they live. A local
client is an in-process call that never touches HTTP, so its error attribution
must not come from the HTTP layer.

**Files:**
- Create: `qa-runs/qa-runs/src/domain/error_attribution.rs`
- Create: `qa-insights/qa-insights/src/domain/error_attribution.rs`
- Modify: `qa-runs/qa-runs/src/api/rest/error.rs`, `qa-insights/qa-insights/src/api/rest/error.rs` (re-export from the new home)
- Modify: both `domain/local_client/client.rs`
- Modify: `qa-runs/qa-runs/src/gear.rs` (the `ConcreteAppServices` alias for #39)

**Interfaces:**
- Produces: `domain::error_attribution::{as_queue_error, as_schedule_error}` and
  `domain::error_attribution::as_jira_error`. `api::rest::error` re-exports them
  so the REST handlers' imports are unchanged.

- [ ] **Step 1: Write the failing test**

An import-direction rule is best pinned structurally, not by a unit test. This
repo runs `cargo gears lint` (`.github/workflows/ci.yml:600`) — check whether
`Gears.toml` can express "no `domain` module imports `api`". If it can, add the
rule there and that is the test. If it cannot, write a source-scanning test,
which qa-runs already has precedent for (`src/lib.rs:23` `doc_citations_tests`,
`:32` `file_citations_tests`):

```rust
/// **No `domain` module imports `api`.**
///
/// The dependency runs the other way: `api` is a transport over `domain`. Two
/// local clients imported their error attribution from `api::rest::error`,
/// which the plugin rework made more visible by moving those clients from
/// `infra/` into `domain/`. A local client is an in-process call that never
/// touches HTTP; its error attribution must not come from the HTTP layer.
/// Review findings #15, #16.
#[test]
fn no_domain_module_imports_the_api_layer() {
    let offenders = scan_src("src/domain", "use crate::api");
    assert!(
        offenders.is_empty(),
        "domain modules must not import api: {offenders:#?}"
    );
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p qa-runs no_domain_module_imports_the_api_layer
```

Expected: FAIL, naming `domain/local_client/client.rs`.

- [ ] **Step 3: Move the mappers**

Move `as_queue_error` and `as_schedule_error` bodies into
`domain/error_attribution.rs`, keeping their doc comments intact — those
comments explain the attribution decision and are the reason this is not a bare
`Into`. In `api/rest/error.rs`:

```rust
// The queue and schedule attribution live in `domain::error_attribution`, not
// here: `domain::local_client` needs them and a domain module must not import
// the transport layer. Re-exported so this module stays the one place a REST
// handler imports error rendering from. Review findings #15, #16.
pub(crate) use crate::domain::error_attribution::{as_queue_error, as_schedule_error};
```

Same for `as_jira_error` in qa-insights.

- [ ] **Step 4: Fix #39**

`qa-runs/src/domain/local_client/client.rs:37`'s
`use crate::infra::ConcreteAppServices;` — the composition root should supply
the alias. Move the `ConcreteAppServices` type alias to `gear.rs` (qa-insights
already has it there: `qa-insights/src/gear.rs` is what `handlers/collect.rs:24`
imports from) and have the local client take it from there.

- [ ] **Step 5: Run to verify it passes**

```bash
cargo nextest run -p qa-runs -p qa-insights --lib
make clippy
```

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform
git commit -m "refactor(qa-platform): move error attribution out of the REST layer

Two local clients imported as_queue_error/as_schedule_error/as_jira_error from
api::rest::error. The plugin rework made this worse by moving those clients from
infra/ into domain/, so a domain module now reached into the transport layer.

The mappers themselves are right -- as_jira_error's own doc explains why a bare
Into is wrong for this caller -- and their doc comments move with them. They now
live in domain::error_attribution and api::rest::error re-exports them, so the
REST handlers' imports are unchanged. Pinned by a structural test.

Review findings #15, #16, #39."
```

---

### Task 22: Four smaller layering and API fixes

**Findings:** #17, #25, #37, #38.

Grouped: each is a small, independent change with no shared risk.

**Files:**
- Modify: `qa-insights/qa-insights/src/domain/ports/slack_client.rs:63,113` and its oagw adapter (#17)
- Modify: all four gears' `domain/error.rs` `From<DbError>` impls (#25)
- Modify: `qa-insights/qa-insights/src/domain/ports/mail_client.rs:83` and its callers (#37)
- Modify: all four gears' `src/lib.rs` (#38)

- [ ] **Step 1: #17 — a domain type for Slack messages**

`domain/ports/slack_client.rs:63` imports `serde_json::Value` so the port can
carry Block Kit blocks. Block Kit is Slack's wire format; a domain port
describing "a notification with a heading, a run summary and a link" should say
that, and the oagw adapter should render it.

Define the domain type from what the existing senders actually build — read
every construction site before designing it, and keep the rendering byte-identical
so the Slack output does not change. Add a test asserting one rendered payload
matches what the current code produces.

- [ ] **Step 2: #25 — box the `DbError` source**

`qa-catalog/src/domain/error.rs:106-114` already carries the plan:

```rust
// TODO(DE1302): `Database(String)` only stores a formatted message, so these
// `From` impls drop the source error. Extend `Database` to hold a boxed source
// so `.source()` returns the original error, then remove these allows.
```

Do exactly that in all four gears, and delete the `#[allow(unknown_lints,
de1302_error_from_to_string)]` attributes. Test: a `DbError` converted to
`DomainError` has a `.source()` that is the original.

- [ ] **Step 3: #37 — `MailClient::send` takes a `SecurityContext`**

`domain/ports/mail_client.rs:83` is
`async fn send(&self, message: &MailMessage)`, while `slack_client.rs:113-117`
takes `ctx: &SecurityContext`. Two notification ports with different
authorization stories is the asymmetry; add the parameter and thread it from
every caller.

- [ ] **Step 4: #38 — `pub(crate)` on `domain` and `infra`**

All four gears' `lib.rs` export both publicly (`qa-catalog:9,11`,
`qa-environments:8,10`, `qa-insights:69,71`, `qa-runs:11,13`), which makes
SeaORM entities and repository traits part of each crate's public API. Change to
`pub(crate)` and re-export explicitly whatever a genuine external consumer
needs. The compiler will name every one.

**If a gear's `gear.rs` or a sibling crate breaks, that break is the finding** —
list what you had to keep public in the commit message.

- [ ] **Step 5: Run everything**

```bash
cargo nextest run --workspace --exclude cf-gears-example-server
make clippy
```

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform
git commit -m "refactor(qa-platform): four layering and API fixes

- The Slack domain port carried Vec<serde_json::Value> Block Kit. A domain type
  now, rendered in the oagw adapter; the payload is byte-identical (#17).
- From<DbError> kept only to_string(), dropping the source. Boxed, and the
  four de1302_error_from_to_string allows are gone -- the TODO in qa-catalog's
  error.rs named exactly this fix (#25).
- MailClient::send omitted the SecurityContext that SlackClient::send takes.
  Two notification ports with different authorization stories (#37).
- pub mod domain / pub mod infra made SeaORM entities and repository traits
  part of all four crates' public API (#38)."
```

---

### Task 23: `ValueRepo` takes a `DBRunner`

**Finding:** #14 (HIGH, `TOOLKIT-DB-001`).

`gears/credstore/plugins/postgres-credstore-plugin/src/infra/storage/repo.rs:68`
and `:189` call `self.db.conn()?` instead of accepting a runner, so no caller
can compose these reads and writes into a transaction. Every other repository in
this subsystem takes `runner: &C` where `C: DBRunner` — the review confirms
"Domain repos take `&C: DBRunner` + `&AccessScope`" as verified-clean everywhere
else.

This crate is new on this branch (22 files, +3063), so this is our defect, not
inherited.

**Files:**
- Modify: `gears/credstore/plugins/postgres-credstore-plugin/src/infra/storage/repo.rs:53,62-68,183-189`
- Modify: its callers in the same crate
- Test: the crate's existing test module

**Interfaces:**
- Produces: `ValueRepo::{find, upsert, insert_if_absent, delete}` each taking
  `runner: &C` where `C: DBRunner`, as their first parameter after `&self`.

- [ ] **Step 1: Write the failing test**

```rust
/// **Two writes compose into one transaction.**
///
/// `ValueRepo` reached for `self.db.conn()` internally, so a caller could not
/// put two of its operations in one transaction -- a partial write on a
/// rollback path had no way to be undone. Every other repository in this
/// subsystem takes `runner: &C where C: DBRunner` for exactly this reason.
/// Review finding #14.
#[tokio::test]
async fn two_writes_roll_back_together() {
    let db = inmem_db().await;
    let repo = ValueRepo::new(Arc::clone(&db));

    let result: Result<(), _> = db
        .transaction(|txn| {
            Box::pin(async move {
                repo.upsert(txn, &key("a"), value("1")).await?;
                repo.upsert(txn, &key("b"), value("2")).await?;
                Err::<(), _>(DbError::Other("deliberate rollback".into()))
            })
        })
        .await;
    assert!(result.is_err());

    let conn = db.conn().unwrap();
    assert!(repo.find(&conn, &key("a"), None).await.unwrap().is_none());
    assert!(repo.find(&conn, &key("b"), None).await.unwrap().is_none());
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p postgres-credstore-plugin two_writes_roll_back_together
```

Expected: FAIL to compile — the methods take no runner.

- [ ] **Step 3: Change the signatures and update callers**

- [ ] **Step 4: Run everything, including the crate's own PG tier**

```bash
cargo nextest run -p postgres-credstore-plugin
```

The crate has `tests/restart_survival_pg.rs` and
`tests/sea_orm_trace_exposure.rs` — both must stay green. **If either needs a
container and is not wired into CI, that is a Phase 1-shaped finding: report it
rather than silently skipping.**

- [ ] **Step 5: Commit**

```bash
git add gears/credstore
git commit -m "fix(postgres-credstore-plugin): ValueRepo takes a DBRunner

Every method reached for self.db.conn() internally, so no caller could compose
two of them into one transaction and a partial write on a rollback path had no
way to be undone. Every other repository in this subsystem takes
runner: &C where C: DBRunner.

This crate is new on this branch, so this is ours rather than inherited.
Review finding #14."
```

---

# Phase 6 — Scale

Findings #27, #55. **#27 must land before the permission-catalog plan**, which
generates from the `resources::*` consts this task edits.

---

### Task 24: Paging and filtering for qa-environments' collections

**Finding:** #55 (MEDIUM, `Z13-1`).

`EnvironmentsRepository::list` is `find().secure().scope_with(scope).all(runner)`
with no limit and no filter (`environments_sea_repo.rs:56-68`);
`list_all_with_tenant` (`:70`) is the same; `/variables` is the same shape. And
`grep -ril odata gears/qa-platform/qa-environments` returns **zero** files —
this gear has no `odata.rs`, no `LimitCfg`, no `PAGE_LIMITS`.

The review offers "record the carve-out" as the cheap half. **This design
declines it**: DESIGN §1.2 allocates `cpt-cf-qa-nfr-scale` to *"qa-runs +
qa-insights"* and adds an explicit carve-out paragraph for qa-catalog, while
that NFR's own first number is **"100 platforms"** — a qa-environments
collection and nothing else. A carve-out sentence would document that the one
gear carrying the number is the one gear that ignores it.

**Files:**
- Create: `qa-environments/qa-environments/src/infra/storage/odata.rs`
- Modify: `qa-environments/qa-environments/src/infra/storage/db.rs` (add `PAGE_LIMITS`)
- Modify: `qa-environments/qa-environments/src/infra/storage/environments_sea_repo.rs:56`, `variables_sea_repo.rs`
- Modify: `qa-environments/qa-environments/src/domain/repos/*.rs` (the trait), `domain/service/environments.rs`
- Modify: `qa-environments/qa-environments/src/api/rest/handlers/environments.rs:16`, `routes/environments.rs:17`
- Test: a new test module beside `odata.rs`

**Interfaces:**
- Consumes: `toolkit::api::odata::{OData, ODataQuery}`, `toolkit_odata::FilterField`,
  `LimitCfg` — the same imports `qa-runs/src/infra/storage/odata.rs` uses.
- Produces: `EnvironmentFilterField`, `VariableFilterField`,
  `EnvironmentsRepository::list_page`, and `GET /qa/v1/environments` returning
  `JsonPage<EnvironmentDto>` instead of `Json<Vec<EnvironmentDto>>`.

- [ ] **Step 1: Write the failing tests**

```rust
/// **An unbounded list is bounded.**
///
/// `/qa/v1/environments` was `find().all()` with no limit. cpt-cf-qa-nfr-scale's
/// first number is 100 platforms, and this is the collection that number is
/// about. Review finding #55.
#[tokio::test]
async fn listing_environments_is_bounded_by_the_page_limit() {
    let f = env_fixture_with(PAGE_LIMITS.default as usize + 50).await;
    let page = f.repo.list_page(&f.conn, &f.scope, &ODataQuery::default()).await.unwrap();
    assert_eq!(page.items.len(), PAGE_LIMITS.default as usize);
    assert!(page.next_cursor.is_some(), "a truncated page must be resumable");
}

/// **An unknown $filter field is a 400, not a scan.**
///
/// The closed-enum discipline the other two gears' odata.rs already have -- the
/// review confirms it as verified-clean there, and it is the property that
/// stops a filter from becoming a whole-tenant table scan.
#[tokio::test]
async fn an_unknown_filter_field_is_rejected() {
    let f = env_fixture_with(3).await;
    let query = odata_query("$filter=nonexistent eq 'x'");
    let err = f.repo.list_page(&f.conn, &f.scope, &query).await.unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

/// The page limits are the literals the other two gears freeze.
///
/// qa-runs' `db.rs:27-33` records why this is pinned: mutating the pair to
/// `{ default: 15, max: 9999 }` once left the whole suite green.
#[test]
fn the_page_limits_match_the_other_two_gears() {
    assert_eq!(PAGE_LIMITS.default, 200);
    assert_eq!(PAGE_LIMITS.max, 500);
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo nextest run -p qa-environments listing_environments_is_bounded
```

Expected: FAIL to compile — `list_page` and `PAGE_LIMITS` do not exist.

- [ ] **Step 3: Add `PAGE_LIMITS` and `odata.rs`**

Copy the shape from `qa-runs/src/infra/storage/odata.rs`, including the doc
discipline: **choose the filterable field set against the table's actual
indexes and write down why each excluded field is excluded.** qa-insights'
`TestCaseResultsField` doc is the model — it explains that admitting `name`
would admit a whole-tenant scan of a 5M-row table.

Candidate fields for `EnvironmentFilterField`: `Id`, `Name`, `ProductId`,
`IsDefault`, `ObservedVersion`, `CreatedAt`. **Check
`entity/environment.rs`'s indexes before committing to this list.** Do not admit
`observed_attrs` — it is a JSON column.

Note the naming trap: the *physical* column is `platform_id`
(`entity/run.rs:39`'s `#[sea_orm(column_name = "platform_id")]` records the same
pattern in qa-runs), while the wire field name is `environment_id`. Follow
`RunFilterField::EnvironmentId`'s doc, which spells this out.

- [ ] **Step 4: Wire the handler and route**

`handlers/environments.rs:16`'s `list_environments` takes `OData(query): OData`
and returns `JsonPage<EnvironmentDto>`; `routes/environments.rs:17`'s
`OperationBuilder::get("/qa/v1/environments")` declares the OData parameters.
Follow `qa-runs/src/api/rest/handlers/runs.rs:105-115` exactly, including its
comment about the service resolving the `AccessScope` first so a `$filter`
cannot widen the result.

Repeat for `/qa/v1/variables`.

- [ ] **Step 5: Cache the UI's environment lookups**

`qa-platform-ui/src/api/hooks.ts:225`'s `fetchEnvironmentDtos` is a bare
`apiGet('/environments')` outside any react-query cache, reached through
`environmentNameIndex()` (`:235`) and `resolveEnvironmentId()` (`:247`) from
inside `fetchRunDetails` (`:447`) — which `useRun(name, 5000)` polls **every 5
seconds** per open Run Detail page, and `useDashboard` every 15 s. Each
`EnvironmentDto` carries its nested observation, so at 100 environments this is
the whole fleet's state re-sent per poll, per open tab.

Give it its own cached query with a `staleTime` — environment names do not
change at 5 s resolution. Add a vitest asserting the fetch is not repeated
within the stale window.

- [ ] **Step 6: Run everything**

```bash
cargo nextest run -p qa-environments --lib
make ui-lint ui-test ui-build
```

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform
git commit -m "feat(qa-environments)!: paging and OData on the environment collections

/qa/v1/environments and /variables were find().all() with no limit and no
filter, and grep -ril odata over the crate returned zero files. DESIGN §1.2
allocates cpt-cf-qa-nfr-scale to 'qa-runs + qa-insights' and carves out
qa-catalog by name, while that NFR's first number is 100 platforms -- a
qa-environments collection and nothing else. So the deferral was recorded
nowhere and the gear carrying the number was the gear ignoring it.

Same PAGE_LIMITS { default: 200, max: 500 } as the other two gears, and the same
closed-enum $filter discipline, so an unknown field is a 400 rather than a scan.
The physical column stays platform_id; the wire field is environment_id.

The UI half: fetchEnvironmentDtos was a bare apiGet outside react-query, reached
from fetchRunDetails, which useRun polls every 5 s per open tab -- so the whole
fleet's state was re-sent per poll. Now cached with a staleTime.

Wire-visible: run make ui-contract against a live stack before release.
Review finding #55."
```

---

### Task 25: `qa.plan` stops declaring an unused `RESOURCE_ID`

**Finding:** #27 (MEDIUM, `Z1-2`).

`qa-catalog/src/domain/service/mod.rs:131-134` declares `qa.plan` with
`pep_properties::RESOURCE_ID`, and the `PLAN` GET passes `resource_id=None`
(`plans.rs:169` — `access_scope(ctx, &resources::PLAN, actions::GET, None)`). A
declared property no call site supplies is a constraint the PDP may compile
against and nothing satisfies. `qa.jira_config` already dropped its.

**This must land before the permission-catalog plan**, which generates from
these consts.

**Files:**
- Modify: `qa-catalog/qa-catalog/src/domain/service/mod.rs:131-134`

- [ ] **Step 1: Confirm no `qa.plan` call site passes a resource id**

```bash
grep -rn 'resources::PLAN' gears/qa-platform/qa-catalog --include=*.rs
```

Every hit must pass `None` as the fourth argument. **If any passes `Some(id)`,
stop** — the finding is wrong for this branch and the declaration should stay.
Record what you found either way.

- [ ] **Step 2: Write the failing test**

```rust
/// **A declared PEP property that no call site supplies is a constraint
/// nothing can satisfy.**
///
/// `qa.plan` declared RESOURCE_ID while every `resources::PLAN` call passes
/// `None` -- plans are addressed by (repo, branch, path), not by a row id, so
/// there is no id to supply. `qa.jira_config` already dropped its for the same
/// reason. Review finding #27.
#[test]
fn qa_plan_declares_only_the_properties_its_call_sites_supply() {
    assert_eq!(
        resources::PLAN.properties(),
        &[pep_properties::OWNER_TENANT_ID],
        "qa.plan has no row id to constrain on"
    );
}
```

Check `ResourceType`'s accessor name before writing `.properties()`.

- [ ] **Step 3: Run red, drop the property, run green**

```bash
cargo nextest run -p qa-catalog qa_plan_declares_only_the_properties
```

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-catalog
git commit -m "fix(qa-catalog): qa.plan stops declaring an unused RESOURCE_ID

Every resources::PLAN call site passes resource_id=None -- plans are addressed
by (repo, branch, path), not by a row id -- so the declaration was a constraint
the PDP could compile against and nothing could satisfy. qa.jira_config already
dropped its for the same reason.

Lands before the permission catalog, which generates from these consts.
Review finding #27."
```

---

# Phase 9 — Decisions and cleanups

Findings #5, #41, #42, #43, #44, #45, #46, #53, #54.

---

### Task 26: A claim-row for the JIRA poller

**Finding:** #5 (HIGH, `RUST-CONC-001` / `Z7-1`), judged on merit.

The rework's argument (`qa-insights/src/infra/leader/mod.rs:40-60`) that
election is an optimisation is **correct for `ROLE_RECONCILER`** and this task
does not touch it: `upsert_run_results` is delete-then-insert per run and
`WatermarkRepository::advance` never moves a mark backwards, so two concurrent
sweeps converge.

It does not cover `ROLE_JIRA_POLLER`, whose effect is
`RunsLauncher::launch_test` — a new run, not an idempotent write. Two replicas
polling the same resolved bug launch it twice. What prevents that today is
`replicaCount: 1` (`deploy/helm/qa-platform/values.yaml:101`) and nothing else.

**Decision:** a claim-row for `ROLE_JIRA_POLLER` alone. Pinning
`replicaCount: 1` with a chart assertion was considered and rejected: it makes a
correctness property depend on a chart value a future scale-out would silently
break.

**Files:**
- Create: a migration adding the claim table
- Create: `qa-insights/qa-insights/src/infra/leader/claim_row.rs`
- Modify: `qa-insights/qa-insights/src/infra/leader/mod.rs:40-60,128-142,159`
- Modify: `qa-insights/qa-insights/src/gear.rs:943`

**Interfaces:**
- Consumes: `qa-environments/src/infra/storage/leases_sea_repo.rs`'s CAS pattern
  — the review verifies it as sound (guards the update on `version = expected`,
  turns the first-write race into `LeaseConflict` via the primary-key unique
  violation, and the service retries the whole read-decide-write cycle).
- Produces: `ClaimRowElector implements LeaderElector`. `NoopLeaderElector`
  stays and keeps serving `ROLE_RECONCILER` and `ROLE_COLLECT`.

- [ ] **Step 1: Write the failing test — the review's own named missing test**

```rust
/// **Two pollers, one rerun.**
///
/// The JIRA poller's effect is `RunsLauncher::launch_test` -- a new run, not an
/// idempotent write -- so two replicas polling the same resolved bug launch it
/// twice. The reconciler's "election is an optimisation" argument
/// (`infra::leader`'s header) is correct and does not extend here: nothing
/// downstream of `maybe_rerun` deduplicates.
///
/// `replicaCount: 1` is what prevents this today, and a chart value is not
/// where a correctness property belongs. Review finding #5.
#[tokio::test]
async fn two_concurrent_pollers_produce_one_rerun() {
    let shared = shared_db().await;
    let a = poller_fixture_on(&shared).await;
    let b = poller_fixture_on(&shared).await;
    a.jira.add_resolved_bug_with_new_build("VHP-2618");

    let (ra, rb) = tokio::join!(
        a.elector.run_role(ROLE_JIRA_POLLER, CancellationToken::new(), a.work()),
        b.elector.run_role(ROLE_JIRA_POLLER, CancellationToken::new(), b.work()),
    );
    ra.unwrap();
    rb.unwrap();

    assert_eq!(
        a.launcher.launches() + b.launcher.launches(),
        1,
        "exactly one of the two pollers may rerun the bug"
    );
}
```

This needs a **shared** database across the two fixtures — an in-memory SQLite
per fixture would make both win trivially. Use the `integration` tier's Postgres
container (`test-qa-insights-pg`, wired by Phase 1 Task 1) and put this test
behind `#[cfg(feature = "integration")]`, with a comment saying why it cannot
live in the unit tier. The gear's own `integration` feature doc
(`Cargo.toml`) already makes this argument for the ingest races.

- [ ] **Step 2: Run to verify it fails**

```bash
make test-qa-insights-pg
```

Expected: FAIL with 2 launches.

- [ ] **Step 3: Add the claim table and the elector**

Model the CAS on `leases_sea_repo.rs`. The claim row is
`(role, tenant_id, holder, claimed_at, expires_at)`; acquiring is a conditional
insert-or-update that succeeds only when the current row is absent or expired.

- [ ] **Step 4: Bind it to the JIRA poller only**

`gear.rs:943` selects the elector for `ROLE_JIRA_POLLER`. The other two roles
keep `NoopLeaderElector`.

- [ ] **Step 5: Update the module header**

`infra/leader/mod.rs:40-60`'s "What election buys the reconciler, and what it
does not" is correct and stays. Add a paragraph saying the poller is the
exception and why — its effect is a launch, not a converging write — so the next
reader does not apply the reconciler's argument to it, which is exactly what
this finding is.

- [ ] **Step 6: Run to verify it passes**

```bash
make test-qa-insights-pg
cargo nextest run -p qa-insights --lib
```

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-insights
git commit -m "fix(qa-insights): a claim-row for the JIRA poller

The reconciler's 'election is an optimisation' argument is correct and stays:
upsert_run_results is delete-then-insert per run and advance never moves a mark
backwards, so two sweeps converge. It does not extend to the JIRA poller, whose
effect is RunsLauncher::launch_test -- a new run, not a converging write. Two
replicas polling the same resolved bug launched it twice.

replicaCount: 1 is what prevented that, and a chart value is not where a
correctness property belongs -- a future scale-out would break it silently. The
claim-row's CAS is qa-environments' leases_sea_repo pattern, which this review
verifies as sound.

ROLE_RECONCILER and ROLE_COLLECT keep NoopLeaderElector. The module header now
says why the poller is the exception. Review finding #5."
```

---

### Task 27: A real scoped repository behind the isolation test

**Finding:** #41 (MEDIUM, `TEST-QUALITY-6`).

`qa-catalog/src/domain/service/products_tests.rs`'s
`MockProductsRepository` ignores `_scope` on every method (`:57,72,80,90,120`),
so `"a product outside the scope must 404"` (`:381`) actually asserts that a
*different id* 404s. The isolation claim in the message is not what the test
checks.

**Files:**
- Modify: `qa-catalog/qa-catalog/src/domain/service/products_tests.rs:52-130,374-382`

- [ ] **Step 1: Make the mock honour the scope**

Give `MockProductsRepository` a tenant per row and have `get`/`update`/`delete`
filter on the scope the way `.secure().scope_with(scope)` does. Then the
existing assertion at `:381` means what it says.

- [ ] **Step 2: Add the assertion that was missing**

A product belonging to *another tenant*, looked up with a valid id under a scope
that excludes it, must 404 — the cross-tenant id swap the review lists as a
mandatory-and-missing test.

```rust
/// **A valid id from another tenant is a 404, not a read.**
///
/// The previous version of this test used a random UUID and a mock that ignored
/// the scope, so it asserted "an unknown id 404s" while its message claimed
/// tenant isolation. Those are different properties and only one of them was
/// checked. Review finding #41.
#[tokio::test]
async fn a_product_from_another_tenant_is_not_readable() {
    let ours = Uuid::new_v4();
    let theirs = Uuid::new_v4();
    let their_product = Uuid::new_v4();
    let repo = Arc::new(MockProductsRepository::with_product_in_tenant(
        product(their_product),
        theirs,
    ));
    let svc = service_with(Arc::clone(&repo));

    let err = svc.get_product(&ctx(ours), their_product).await.unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == their_product),
        "another tenant's product must be absent, not forbidden or readable; got {err:?}"
    );
}
```

- [ ] **Step 3: Run red then green**

```bash
cargo nextest run -p qa-catalog products_tests
```

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-catalog
git commit -m "test(qa-catalog): make the product isolation test real

MockProductsRepository ignored _scope on every method, so 'a product outside the
scope must 404' asserted that an unknown id 404s -- a different property from
the one its message claimed. The mock now honours the scope, and a valid id
belonging to another tenant is asserted absent: the cross-tenant id swap the
review lists as mandatory and missing. Review finding #41."
```

---

### Task 28: Delete the dead UI and fix the misfiled doc comment

**Findings:** #53 and #54, both LOW.

**#53 — three dead symbols.**
`qa-platform-ui/src/pages/SettingsPage.tsx` (144 lines) has no importer — the
routes use `pages/settings/SettingsLayoutPage`.
`pages/settings/SettingsJiraPollerPage.tsx` (76 lines) has none either:
`App.tsx:146` routes `jira-poller` to a `<Navigate to="/settings/jira">` and
`SettingsJiraPage` already holds both `useJiraConfig`/`useJiraPollerConfig`
pairs. `hooks.ts:652`'s `useTestRecentResults` is exported and called from
nowhere.

`useTestRecentResults` additionally carries a latent trap worth recording in the
commit: `/qa/v1/test-results` has no chronological `$orderby`
(`qa-insights/src/infra/storage/odata.rs:377-379`, `is_orderable` excludes
`run_finished_at`) and its default order is `id` descending over a
`Uuid::new_v4()` primary key, so the hook's "recent" is a *random* sample.
`sliceRecentResults` (`adapters.ts:176`) takes the head of the page without
sorting. Nothing renders it today; a caller added later would render a random
history strip.

**#54 — a doc comment on the wrong item.**
`qa-environments/src/api/rest/handlers/environments.rs:254-260` — the paragraph
explaining that `observe_environment` must authorize with `UPDATE` rather than
`GET` (*"Found by review; this test is what stops it silently regressing"*) sits
on `fn environment_for_product` (`:263`), whose own one-line doc was appended as
the paragraph's last line. The test it belongs to,
`refresh_environment_authorizes_with_update_not_get`, is at `:359` with no doc.
The test passes; only the rationale is filed under the wrong name — which for a
finding-of-record comment is the part that matters.

**Files:**
- Delete: `qa-platform-ui/src/pages/SettingsPage.tsx`, `qa-platform-ui/src/pages/settings/SettingsJiraPollerPage.tsx`
- Modify: `qa-platform-ui/src/api/hooks.ts:652`, `qa-platform-ui/src/api/adapters.ts:176,220`
- Modify: `qa-environments/qa-environments/src/api/rest/handlers/environments.rs:254-263,359`

- [ ] **Step 1: Confirm they are dead before deleting**

```bash
cd gears/qa-platform/qa-platform-ui
grep -rn 'SettingsPage\|SettingsJiraPollerPage\|useTestRecentResults\|sliceRecentResults' src/
```

Expected: each name appears only at its own definition. **If anything else
references one, stop** — it is not dead and the finding is wrong for this
branch.

- [ ] **Step 2: Delete and build**

```bash
make ui-lint ui-test ui-build
```

Expected: PASS. `tsc` is what proves nothing referenced them.

- [ ] **Step 3: Move the doc comment**

Move `environments.rs:254-260` above `:359`, leaving `:263`'s own one-line doc
on the helper.

- [ ] **Step 4: Run**

```bash
cargo nextest run -p qa-environments --lib
```

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform
git commit -m "chore(qa-platform): delete dead UI and refile a misplaced doc comment

SettingsPage.tsx (144 lines) and settings/SettingsJiraPollerPage.tsx (76) had no
importer -- the routes use SettingsLayoutPage and jira-poller is a Navigate
redirect. A stale second settings page is the file a maintainer edits by
mistake.

useTestRecentResults was exported and called from nowhere, and carried a trap:
/qa/v1/test-results has no chronological \$orderby (is_orderable excludes
run_finished_at) and defaults to id descending over a Uuid::new_v4() primary
key, so its 'recent' was a random sample. A caller added later would have
rendered a random history strip.

The UPDATE-not-GET rationale in qa-environments' handler tests sat on the
environment_for_product helper instead of on the test it describes, which for a
finding-of-record comment is the part that matters.

Review findings #53 and #54."
```

---

### Task 29: Four test-quality cleanups

**Findings:** #43, #44, #45, #46, all LOW. **#42 is deliberately not in this
task** — the review says "unify on touch; no churn" and this plan agrees; 385
`C: DBRunner` against 6 `impl DBRunner` is a spelling difference, and a
mechanical sweep would be exactly the churn the finding warns against.

- [ ] **Step 1: #43 — delete the constant-echo assert**

`qa-insights/src/infra/notify/slack_oagw_tests.rs:254` asserts
`SlackOagwClient::REQUEST_TIMEOUT == Duration::from_secs(10)` — the constant
against itself spelled out. It cannot fail for any reason that matters, and the
hanging-gateway test already proves the timeout is applied. Delete it.

- [ ] **Step 2: #44 — three identical `message()` calls**

`qa-insights/src/infra/notify/mail_unsupported.rs:74,87` call the same
`message()` helper (`:57`) in tests asserting different things. Either give each
a distinct message so the tests are visibly about different inputs, or collapse
them into one — whichever the tests' actual intent supports. Read them first.

- [ ] **Step 3: #45 — a duplicated denial loop**

`qa-runs/src/api/rest/handlers/schedules_handler_tests.rs:422-442` duplicates
the loop in `every_handler_attributes_a_denial_to_the_schedule` (`:274`).
Remove it, or change it to exercise a path the loop does not.

- [ ] **Step 4: #46 — constructor echoes**

`qa-insights/src/api/rest/dto.rs` (near `:3946` in the review's numbering — find
it by the compact-JSON assert it follows) re-asserts each field after a
compact-JSON assertion that already covers them. Keep the JSON assert, drop the
echoes.

- [ ] **Step 5: Run and commit**

```bash
cargo nextest run -p qa-insights -p qa-runs --lib
git add gears/qa-platform
git commit -m "test(qa-platform): four test-quality cleanups

- A constant asserted against itself; the hanging-gateway test already proves
  the timeout is applied (#43).
- Three tests calling one message() helper for assertions about different
  inputs (#44).
- A denial test duplicating the loop above it (#45).
- Constructor field echoes after a compact-JSON assert that covers them (#46).

#42 is deliberately untouched: 385 'C: DBRunner' against 6 'impl DBRunner' is a
spelling difference, and the review says unify on touch, no churn."
```

---

## Phase completion

```bash
make fmt clippy
make test-no-macros
make test-qa-runs-pg test-qa-insights-pg test-qa-catalog-git test-qa-platform-features
make helm-tests ui-lint ui-test ui-build
```

Then `2026-09-05-qa-permission-catalog.md` (Phase 7) and
`2026-09-05-qa-observability.md` (Phase 8), which are independent of each other.
