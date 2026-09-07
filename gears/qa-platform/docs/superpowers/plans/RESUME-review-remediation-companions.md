# Resume prompt — QA Platform review remediation, companion plans

Paste the **Prompt to start the new session** section below into a fresh Claude
Code session opened at `/home/serhii/Jelastic/projects/fabric/gears-rust`.
Everything else in this file is context that prompt refers to.

Written 2026-09-06, after the core plan closed. Updated 2026-09-07: the **quality
plan is complete** (see "What remains"). Delete this file when the two plans that
still remain are finished.

---

## Prompt to start the new session

> Continue the QA Platform review remediation. Its **core plan is complete**;
> three companion plans remain.
>
> Read these first, in this order:
>
> 1. `gears/qa-platform/docs/superpowers/plans/RESUME-review-remediation-companions.md`
>    — this file. It carries what the last session learned that the plans do not
>    say, and it is the only surviving record of several rulings.
> 2. `gears/qa-platform/docs/superpowers/specs/2026-09-05-review-remediation-design.md`
>    — the spec all four plans argue from. **Binding authority** when a plan and
>    a finding disagree.
> 3. The plan you are about to execute (see "What remains" below). Read it once,
>    in full, before dispatching anything.
>
> Then invoke the `superpowers:subagent-driven-development` skill and execute
> that plan task by task.
>
> Work continuously, one task at a time, without checking in between tasks.
> Only the four stop-conditions in that skill — an irreversible or destructive
> operation, a security-sensitive action, an outward-facing side effect, or a
> plan so broken that every path forward is a guess — should interrupt you.
>
> **Two things the last session got wrong that you should not repeat.** First,
> "pre-existing" is not the same as "not ours": a broken gate inside
> `gears/qa-platform/` is in scope even if this work did not break it. Second,
> on the `qa-runs` watch path a doc comment asserting an absolute has been wrong
> six times out of six — trace the mechanism against the outside world rather
> than reading the argument.

---

## State

- **Branch:** `feature/qa-review-remediation`, off `feature/qa-product-plugins` @ `a1767401f`
- **HEAD:** one squashed commit, `fix(qa-platform): remediate the QA Platform review, phases 1-4`
- **Pre-squash history:** tag `pre-squash-qa-review-remediation` — 45 commits with every
  per-task review and fix round. The branch is unpushed, so that tag is the only
  copy. Do not delete it until the branch lands.
- **Working tree:** clean. **Nothing is running.**
- **All gates green** at the squashed commit: `fmt`, `clippy`, `test-no-macros`
  (11 665), `test-qa-runs-pg` (913), `test-qa-insights-pg` (728),
  `test-qa-catalog-git` (293), `test-qa-platform-features` (253), `helm-tests`
  (6/6), `ui-test`, `ui-build`. **Keep them that way** — two of them were red
  before this work and were fixed as part of it.

### The ledger

`.superpowers/sdd/2026-09-05-review-remediation-core/progress.md` — ~2 100 lines,
39 recorded rulings, every review verdict. **It is git-ignored**, so it exists
only on this machine and vanishes if that directory is cleared. Everything below
is the part that must outlive it.

---

## What remains: 23 tasks across three plans

| Plan | Tasks | Phases | Findings |
|---|---|---|---|
| ~~`2026-09-05-review-remediation-quality.md`~~ **DONE 2026-09-07** | 19–29 | 5, 6, 9 | #10 #11 #12 #14 #15 #16 #17 #25 #27 #34 #35 #37 #38 #39 #41 #42 #43 #44 #45 #46 #53 #54 #55, and #5 |
| `2026-09-05-qa-permission-catalog.md` | 30–35 | 7 | #1 |
| `2026-09-05-qa-observability.md` | 36–41 | 8 | #4 |

**Quality is done.** Remaining order: permission catalog → observability.

### What the quality plan left behind

All nine gates were green at its final commit (`fmt`, `clippy`, `test-no-macros` 11734,
`test-qa-runs-pg` 927, `test-qa-insights-pg` 758, `test-qa-catalog-git` 299,
`test-qa-platform-features` 268, `helm-tests` 6/6, `ui-lint`/`ui-test` 237/`ui-build`).
`make ui-lint` now exists and runs — it never had before.

**Three follow-ups that need scheduling, not just noting:**

1. **#38's remainder in `qa-insights` and `qa-runs`.** `pub(crate)` on `domain`/`infra`
   landed in qa-catalog and qa-environments and was reverted in the other two, because
   the compiler surfaced 46 and 16 groups of code only their own `#[cfg(test)]` modules
   reach. Closing it means 62 delete-or-wire decisions plus ~186 mechanical
   `pub(crate)` → `pub` edits (`clippy::redundant_pub_crate` is denied repo-wide); land
   the mechanical half as its own commit first. The live exposure is that
   `qa_insights::infra::storage::entity::*` and `qa_runs::infra::storage::entity::*`
   stay nameable — the schema-pinning risk #38 was raised about — in the two gears with
   the most entities. Both `lib.rs` files carry the measurement in code.
2. **An unwired test tier.** `gears/credstore/plugins/postgres-credstore-plugin/tests/restart_survival_pg.rs`
   needs `CREDSTORE_PG_TEST_DSN` and is named in neither the `Makefile` nor any
   workflow, so it silently self-skips under `make test` and `make ci`. Same shape as
   the gaps Phase 1 existed to close, but in `gears/credstore/`. **Schedule this one
   first** — it is the only deferral that weakens a phase already marked closed, and
   the fix is to mirror an existing `test-*-pg` target.
3. **A cross-table cursor for `/qa/v1/variables`** in the `environment_id` case, where
   the body is a union of two tables and `CursorV1` has no segment discriminator. The
   union is bounded and a `cursor` sent with `environment_id` is now a 400 raised before
   any DB or PDP work, so the dangerous shape is closed; what remains is a capability.

**Three wrong line numbers, parked with their correct values** — introduced by the
final fix wave's doc sweep, inert to behaviour, and caught by no gate because
`file_citations_tests` validates paths only and `doc_citations_tests` identifiers only:

- `qa-insights/.../api/rest/handlers/saved_views_handler_tests.rs:23` cites
  `domain/error.rs:531` for `SavedViewResourceError` — 531 is the **test_result**
  `gts_id`. Correct: **538/539**.
- `qa-insights/.../api/rest/handlers/settings_handler_tests.rs:14,45` cite `:536` for
  `NotificationResourceError` — 536 is inside the previous type's doc comment.
  Correct: **543/544**.
- `qa-insights/.../infra/clients/qa_runs.rs:342` cites qa-runs `domain/error.rs:597`,
  the `struct` line; the `gts_id!` it describes is at **596**.

**Two residuals disclosed in code, both deliberate:** the JIRA poller's claim row is a
claim row and not a fencing token, so a failover window remains in which a dispossessed
holder can finish one tenant's `poll_once`; and `after_a_failed_renewal`'s TTL-elapsed
arm is unexecuted for want of a fault-injecting DB seam this gear does not have.

**One asymmetry worth a tidy-up:** qa-runs moved its canonical-mapping tests with the
code into `domain/error.rs`; qa-insights left its in `api/rest/error.rs` because they
interleave with `as_saved_view_error`/`as_notification_error`. Permitted direction,
recorded in `domain/error.rs`'s header, but the two gears now differ.

---

**Original suggested order (superseded):** quality → permission catalog → observability. The spec
(§12) says phases 1–6 are the correctness core and that 7 and 8 are each a
subsystem's worth of work that can be scheduled independently once 1–6 land.
Task 25 (`qa.plan` drops its unused `RESOURCE_ID`) has an explicit ordering
note: **it must land before Phase 7**, because the catalog is generated from
those consts.

All three were verified genuinely untouched: no `AuthzPermissionV1` exists in
any gear, there are zero non-test metrics hits across all four gears and both
plugin crates, and `ExclusiveFlag` is still `pub type ExclusiveFlag = Option<bool>`
(`qa-catalog-sdk/src/models.rs:60`).

---

## Things the next session must not rediscover the hard way

**Environment.** Every cargo dispatch needs `export PATH="$HOME/.cargo/bin:$PATH"`
first. The system `/usr/bin/cargo` is rustc 1.75.0; this repo needs 1.97.0 and
nextest. This cost the last session time on its very first task.

**The Argo tests are invisible by default.** They sit behind `--features argo`
and are not compiled by a default-feature run, so a claim that
`infra/executor/argo/` is covered must point at a test that runs under
`make test-qa-platform-features`. A test that only compiles under a default
build is not coverage of that file.

**`serde_json/preserve_order` is enabled workspace-wide.** `serde_toon_format`
← `kreuzberg` ← `gears/file-parser` ← `apps/cf-gears-example-server` turns
`serde_json::Map` from a sorted `BTreeMap` into an insertion-order `IndexMap`
in any `--workspace` build. **Any new snapshot or golden test that compares
serialized JSON as text will pass per-package and fail under
`make test-no-macros`.** Compare `serde_json::Value` instead — its map equality
is order-independent under both backings. This broke `qa-runs`' golden RunSpec
test and cost real time to diagnose. It is not QA Platform's dependency and was
deliberately not fixed here; it is worth raising with whoever owns commit
`94a05850d`.

**Task 21 will collide with a decision the core plan made.** Task 21 is "the
domain layer must not import its own REST layer" — findings #15/#16. The core
plan *added* a new instance of that shape: `infra/executor/argo/watch.rs` and
`domain/service/ingest.rs` now import `crate::api::rest::sse` for
`MAX_LINE_BYTES` and `sanitize_line_for_archive`. That was deferred to Phase 5
on purpose. **`MAX_LINE_BYTES` is no longer only an SSE read-side judgement** —
it is the input to `WRITE_SIDE_MAX_LINE_BYTES`, so it decides what reaches the
database and what every resume anchor contains. Lowering it silently disables
log-resume suppression for every live run. Its natural home is `domain::repos`
beside `flatten_log_char`. Move it as part of Task 21, and read the paragraph
at `api/rest/sse.rs` that explains the second role before touching it.

**Task 19's exclusivity enum touches code the core plan just rewrote.**
`qa-runs/src/domain/service/launch.rs` gained `classify_catalog_failure` and a
`Result`-returning `gather_group_meta`/`gather_file_meta` chain. Read the
current file; the quality plan's snippets predate it.

**The `qa-insights` `Fleet` fixture has no accessors.** `db` is a private
`Arc<DbProvider>` and `FakeRuns`/`FakeCatalog`/`FakePlatforms` are erased into
trait objects at construction with no handle retained. The core plan hit this
and could not write a cancellation test for three of `qa-insights`' ticker
loops because of it. If any companion task needs to assert through those
doubles, extending `Fleet` is the prerequisite — and it is test-infrastructure
work, not a fix to smuggle into a task.

**Phase 9's #5 decision is already made in the spec.** A claim-row for
`ROLE_JIRA_POLLER` alone; the reconciler keeps `NoopLeaderElector` and the
spec explains why the two differ. `leases_sea_repo.rs`' CAS is the pattern to
copy — the original review verified that one as sound.

---

## Rulings from the core plan that bind future work

These were decided without the user and are recorded here because the ledger is
git-ignored. The full text, with each ruling's cost-if-wrong, is in that file.

1. **`max_concurrent_runs` defaults to 50, not 0.** Derived from
   `cpt-cf-qa-nfr-scale`. `0` is kept as an explicit unbounded opt-out and is
   exposed through the Helm chart. This is deployment-visible twice over: a 429
   at launch where there was none, and an `executor.list_active()` call on the
   front of every launch, so an unreadable executor now fails launches that
   would previously have been queued. Do not quietly revert it; it closes #19.
2. **Observers are bounded at admission, not in the watch registry.** The plan
   asked for a registry cap. `domain/service/watch.rs` already argued against
   one — a registry cap must decide *which* live run goes unobserved, and an
   unobserved run can only end at its deadline. That argument stands and the
   section documenting it should not be re-litigated without engaging it.
3. **`EnforcerError::CompileFailed` is split, not blanket-mapped.**
   `ConstraintsRequiredButAbsent` is a deny per the SDK's own words and stays
   403; `AllConstraintsFailed` is a fault and is 500. Four gears, per-arm tests,
   each carrying a warning to a future reader of finding #3 not to "correct" it
   back.
4. **Finding #56 is not a defect.** It was raised during the last session's own
   validation and its premise did not survive contact with the code —
   `tenants_for` already warned unconditionally. The change was kept as a
   readability improvement and the claim withdrawn. Do not re-file it.
5. **Finding #48's counts are unreliable** — it says 5 ignored argo tests (4),
   33 tests under `executor::argo` (39), and 24 under a
   `qa-environments::infra::observer` module that no longer exists. The
   substance was right; the arithmetic was not. Treat the review's numbers as
   claims.
6. **The golden RunSpec fixture must not be re-recorded.** Its own header says
   so three times. The core plan made the *comparison* order-insensitive
   without touching the fixture, and proved the guard still catches a real
   change by perturbing a value.

---

## Deferred items the companion plans should triage

Recorded by the core plan's final review, none blocking, all still open:

- **`qa-insights` has no cancellation test** for `reconcile_pass`,
  `jira_poll_pass` or `collect_pass` — blocked on the `Fleet` accessors above.
- **A failed pod `list` reports `Complete`**, so a terminal Argo workflow can
  emit `Finished` over pods it never enumerated. Pre-existing, disclosed at
  `infra/executor/argo/watch.rs`, deliberately unchanged — fixing it puts every
  `list`-rejecting API server into a re-attach loop.
- **A run with a NULL `timeout_at`** — from an unclamped `plan.yaml`
  `timeout_seconds` that saturates — is never reclaimed by the timeout sweep,
  so a permanently wedged pod re-attaches forever. Pre-existing, documented at
  `domain/service/launch.rs`.
- **`ASSUMED_ARCHIVE_PREFIX_BYTES = 256`** is now guarded by a `debug_assert`
  plus a warn-once, but nothing enforces it in release. Today's node names are
  41 bytes.
- **The UI has no lint gate.** `make ui-lint` has never worked: no
  `eslint.config.js` has ever been committed, the project is on `eslint ^9`
  which requires flat config, and the lint script still passes the removed
  `--ext` flag. Measured: 15 errors + 1 warning across 12 of 139 files.
  Tracked in `docs/DECOMPOSITION.md` §2.6.
- **`actions/setup-python@82c7e631…` is commented `# v5.1.1` but is tag
  v5.1.0** — `.github/workflows/e2e.yml:50` and `cfs.yml:46`. A stale comment,
  not a security issue; out of scope for every plan so far.
- **`qa.notification_config` (PEP) vs `cf.qa.insights.notification.v1~`
  (REST `gts_id`)** — every other qa-insights resource aligns between the two;
  this one drops `_config`. Not renamed because the `gts_id` is the RFC-9457
  `type` clients match on, i.e. a wire contract. **Phase 7 will have to decide
  this one**, since the catalog is generated from the `resources::*` consts.
