# Resume prompt — QA Platform review remediation, permission catalog (Phase 7)

Paste the **Prompt to start the new session** section below into a fresh Claude
Code session opened at `/home/serhii/Jelastic/projects/fabric/gears-rust`.

Written 2026-09-07, after the quality plan closed. Delete this file when the
permission catalog plan is finished. `RESUME-review-remediation-companions.md`
remains the record for the two companion plans as a set.

---

## Prompt to start the new session

> Continue the QA Platform review remediation. The **core plan (Phases 1–4) and
> the quality plan (Phases 5, 6, 9) are both complete**. Execute the **permission
> catalog plan (Phase 7)**, then stop.
>
> Read these first, in this order:
>
> 1. `gears/qa-platform/docs/superpowers/plans/RESUME-review-remediation-companions.md`
>    — carries what the last two sessions learned, including a section on what
>    the quality plan left behind.
> 2. `gears/qa-platform/docs/superpowers/specs/2026-09-05-review-remediation-design.md`
>    §9 — **binding authority** when the plan and a finding disagree. §12 records
>    what is deliberately out of scope.
> 3. `gears/qa-platform/docs/superpowers/plans/2026-09-05-qa-permission-catalog.md`
>    — Tasks 30–35, finding #1. Read it once, in full, before dispatching
>    anything.
>
> Then invoke `superpowers:subagent-driven-development` and execute it task by
> task.
>
> Work continuously, one task at a time, without checking in between tasks. Only
> the four stop-conditions in that skill should interrupt you — an irreversible
> or destructive operation, a security-sensitive action, an outward-facing side
> effect, or a plan so broken every path forward is a guess.
>
> ## State
>
> - **Branch:** `feature/qa-review-remediation`, off `feature/qa-product-plugins`
>   @ `a1767401f`. Unpushed. Working tree clean.
> - **HEAD:** `3c4727849` — three commits on the branch: phases 1–4 squashed, a
>   docs commit, phases 5/6/9 squashed.
> - **Pre-squash history:** tags `pre-squash-qa-review-remediation` (43 commits,
>   phases 1–4) and `pre-squash-qa-review-remediation-quality` (24 commits,
>   phases 5/6/9). The branch is unpushed, so **these tags are the only copies**.
>   Do not delete them until the branch lands.
> - **All gates green at HEAD.** Keep them that way — several were red before
>   this work and were fixed as part of it:
>   `fmt` · `clippy` (workspace + `cargo hack --each-feature`) ·
>   `test-no-macros` 11734 · `test-qa-runs-pg` 927 · `test-qa-insights-pg` 758 ·
>   `test-qa-catalog-git` 299 · `test-qa-platform-features` 268 ·
>   `helm-tests` 6/6 · `ui-lint` + `ui-test` 237 + `ui-build`.
>   Phase 7's own completion command adds `make gts-docs`.
>
> **The prerequisite is satisfied.** Task 25 landed: `qa.plan` no longer declares
> an unused `RESOURCE_ID`. All four `resources::PLAN` call sites were verified to
> pass `None`. So the catalog generates from the corrected declaration and the
> anti-drift test never has to be edited around a fix.
>
> ## Things that will cost you time if you rediscover them
>
> **Environment.** Every cargo dispatch needs
> `export PATH="$HOME/.cargo/bin:$PATH"` first. The system `/usr/bin/cargo` is
> rustc 1.75.0; this repo needs 1.97.0 with `cargo-nextest` and `cargo-hack`
> (both installed). Tell implementers to run builds and suites in the
> **foreground** — a backgrounded job that never reports stalls the agent's turn,
> and that has happened twice.
>
> **Run `make fmt` yourself before you call the plan done.** The last session's
> per-task runs were scoped or omitted, and the branch-wide `fmt` gate was red at
> the final commit across 11 files from four different tasks. Nobody's per-task
> report was false — they simply hadn't claimed it. The generalisable lesson: the
> per-task evidence is trustworthy in what it asserts and incomplete in what it
> omits, and the gates most often omitted are the global ones (`fmt`,
> `test-no-macros`).
>
> **`serde_json/preserve_order` is enabled workspace-wide.** Any new test that
> compares serialized JSON **as text** passes per-package and fails under
> `make test-no-macros`. Compare `serde_json::Value` — its map equality is
> order-independent under both backings.
>
> **The argo and runner-secret tests are invisible by default.** They sit behind
> `--features argo` / `--features runner-secret` and are not compiled by a
> default-feature run. `make test-qa-platform-features` is the tier that runs
> them, and a claim of coverage must point at a test that runs there.
>
> **All four qa-platform SDK crates are free of `serde`, `utoipa` and `http`** by
> a documented contract-purity rule (`qa-runs-sdk/src/lib.rs:4-7` names the
> dylint rules, which sit in `Gears.toml`'s skip list, so it is enforced by
> review rather than the compiler). Two tasks last session were planned assuming
> otherwise and had to be re-ruled mid-flight. The catalog belongs in each
> **gear**, not an SDK, so this should not bite — but check before putting a
> derive on an SDK type.
>
> **A doc comment asserting an absolute on the `qa-runs` watch path has now been
> wrong eight times out of eight.** Trace the mechanism against the code rather
> than reading the argument above it.
>
> **A gate that has never failed has not been shown to be a gate.** The quality
> plan's own named missing test — "two pollers, one rerun" — passed against no
> leader election at all about half the time until an implementer noticed and
> built a rendezvous to make the failing case deterministic. Phase 7's anti-drift
> test is exactly this shape: require it be shown failing, in both directions, on
> a deliberately perturbed catalog.
>
> ## The trap the plan states, restated because it is silent when wrong
>
> The environments resource string is **`"qa.platform"`**, not
> `"qa.environment"` — the rework renamed the aggregate and deliberately kept the
> PDP string, because that string is what policies are written against
> (`qa-environments/src/domain/service/mod.rs:90-99`). Generate from
> `resources::PLATFORM`, never from a type name. A catalog emitting
> `qa.environment` grants nothing, silently, and looks correct.
>
> Related, and Phase 7 has to decide it: `qa.notification_config` (PEP) versus
> `cf.qa.insights.notification.v1~` (REST `gts_id`). Every other qa-insights
> resource aligns between the two; this one drops `_config`. It was not renamed
> because the `gts_id` is the RFC-9457 `type` clients match on — a wire contract.
>
> **Do not ship grants.** The plan ends by handing off to whoever owns the
> deployment's Keycloak realm (`gears/qa-platform/deploy/realm/keycloak/`).
>
> ## Not part of Phase 7 — do not fold these in
>
> Three follow-ups the quality plan left, recorded in the RESUME file with their
> reasoning: #38's remainder in qa-insights and qa-runs (62 delete-or-wire
> decisions), an unwired credstore PG tier that self-skips under `make ci`
> (**schedule this one first** — it is the only deferral that weakens a phase
> already marked closed), and a cross-table cursor for `/qa/v1/variables`.
>
> Three wrong doc line numbers are parked with their correct values in the RESUME
> file. If a Phase 7 task happens to open
> `qa-insights/src/api/rest/handlers/{saved_views,settings}_handler_tests.rs`,
> fix them on the way past; do not make a task of it. Note that neither
> `file_citations_tests` (paths only) nor `doc_citations_tests` (identifiers
> only) validates line numbers, which is why they survived a green run.
>
> After Phase 7, one plan remains: `2026-09-05-qa-observability.md` (Phase 8,
> #4).

---

## Two notes for whoever drives that session

**Re-derive the surface; do not trust the table.** The plan's own surface table
(18 resource types, ~15 actions) says so itself, and it is right: the last two
sessions found the review's counts unreliable more than once — finding #48's
were wrong in all three of its numbers.

**Keep Task 30 on a capable model.** Everything downstream is generated from its
output, so a wrong `(resource_type, action)` pair there produces a catalog that
grants the wrong things while every later test passes. Tasks 31–34 are more
mechanical once 30 is right.
