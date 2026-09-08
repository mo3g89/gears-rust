# Resume prompt — QA Platform review remediation, observability (Phase 8)

Paste the **Prompt to start the new session** section below into a fresh Claude
Code session opened at `/home/serhii/Jelastic/projects/fabric/gears-rust`.

Written 2026-09-07, after the permission catalog closed. **Phase 8 is the last
plan of the remediation.** Delete this file when it is finished, and delete
`RESUME-review-remediation-companions.md` with it — that file is the record for
the companion plans as a set and has nothing left to track afterwards.

---

## Prompt to start the new session

> Continue the QA Platform review remediation. The **core plan (Phases 1–4), the
> quality plan (Phases 5, 6, 9) and the permission catalog (Phase 7) are all
> complete.** Execute the **observability plan (Phase 8)** — the last one — then
> stop.
>
> Read these first, in this order:
>
> 1. `gears/qa-platform/docs/superpowers/plans/RESUME-review-remediation-companions.md`
>    — carries what the earlier sessions learned, including a section on what
>    each finished plan left behind.
> 2. `gears/qa-platform/docs/superpowers/specs/2026-09-05-review-remediation-design.md`
>    §10 — **binding authority** when the plan and a finding disagree. §12
>    records what is deliberately out of scope, and now carries a Phase 7
>    addendum on the permission catalog's unclosed residual.
> 3. `gears/qa-platform/docs/superpowers/plans/2026-09-05-qa-observability.md`
>    — Tasks 36–41, finding #4. Read it once, in full, before dispatching
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
>   @ `a1767401f`. **Unpushed.** Working tree clean.
> - **HEAD:** `f48a72701` — five commits on the branch: phases 1–4 squashed, a
>   docs commit, phases 5/6/9 squashed, a docs commit, phase 7 squashed.
> - **Pre-squash history, and the branch is unpushed, so these tags are the only
>   copies. Do not delete them until the branch lands:**
>   `pre-squash-qa-review-remediation` (43 commits, phases 1–4),
>   `pre-squash-qa-review-remediation-quality` (24 commits, phases 5/6/9),
>   `pre-squash-qa-review-remediation-permissions` (12 commits, phase 7).
> - **All gates green at HEAD**, and `make gts-docs` is now among them for the
>   first time — it had been red since the branch's own base commit:
>   `fmt` · `clippy` (workspace + `cargo hack --each-feature`) ·
>   `gts-docs` 839/0/0 · `test-no-macros` **11773** (368 skipped) ·
>   `test-qa-runs-pg` 935 · `test-qa-insights-pg` 766 ·
>   `test-qa-catalog-git` 309 · `test-qa-platform-features` 277 ·
>   `helm-tests` 6/6 · `ui-lint` + `ui-test` + `ui-build`.
>   Keep them that way. Several were red before this work and were fixed as part
>   of it.
>
> **Phase 8's prerequisite is satisfied** and it is independent of Phase 7 — the
> plan says either order works, and Phase 7 went first.
>
> ## Things that will cost you time if you rediscover them
>
> **Environment.** Every cargo dispatch needs
> `export PATH="$HOME/.cargo/bin:$PATH"` first. The system `/usr/bin/cargo` is
> rustc 1.75.0; this repo needs 1.97.0 with `cargo-nextest` and `cargo-hack`
> (both installed). Tell implementers to run builds and suites in the
> **foreground** — a backgrounded job that never reports stalls the agent's turn,
> and that has now happened three times across these sessions.
>
> **Run `make fmt` yourself, unscoped, before you call the plan done.** Two
> sessions ago the per-task runs were scoped and the branch-wide gate was red at
> the final commit across 11 files. The generalisable lesson holds and was
> confirmed again in Phase 7: per-task evidence is trustworthy in what it asserts
> and incomplete in what it omits, and the gates most often omitted are the
> global ones (`fmt`, `test-no-macros`).
>
> **`serde_json/preserve_order` is enabled workspace-wide.** Any new test that
> compares serialized JSON **as text** passes per-package and fails under
> `make test-no-macros`. Compare `serde_json::Value` — its map equality is
> order-independent under both backings. This will matter to Phase 8 if any
> metric assertion snapshots a rendered payload.
>
> **The argo and runner-secret tests are invisible by default.** They sit behind
> `--features argo` / `--features runner-secret` and are not compiled by a
> default-feature run. `make test-qa-platform-features` is the tier that runs
> them, and a claim of coverage must point at a test that runs there. Phase 8
> touches `infra/executor/argo/` indirectly through the dispatch and ingest
> paths, so this is live for Task 37.
>
> **All four qa-platform SDK crates are free of `serde`, `utoipa` and `http`**
> by a documented contract-purity rule (`qa-runs-sdk/src/lib.rs:4-7` names the
> dylint rules, which sit in `Gears.toml`'s skip list, so it is enforced by
> review rather than the compiler). Two tasks in an earlier session were planned
> assuming otherwise and had to be re-ruled mid-flight. Metrics ports belong in
> each **gear**'s `domain/ports/`, not in an SDK — but check before putting a
> derive on an SDK type.
>
> **The `qa-insights` `Fleet` fixture has no accessors.** `db` is a private
> `Arc<DbProvider>` and `FakeRuns`/`FakeCatalog`/`FakePlatforms` are erased into
> trait objects at construction with no handle retained. An earlier session could
> not write cancellation tests for three of qa-insights' ticker loops because of
> it. **Phase 8's Task 38 covers collect and the JIRA poll, both of which run in
> those loops** — if a task needs to assert an emission through those doubles,
> extending `Fleet` is the prerequisite, and it is test-infrastructure work, not
> a fix to smuggle into a task.
>
> ## Two habits this subsystem has punished, repeatedly
>
> **A gate that has never failed has not been shown to be a gate.** The quality
> plan's own named missing test passed against no leader election at all about
> half the time until an implementer built a rendezvous to make the failing case
> deterministic. Phase 7's anti-drift tests were each required to be shown
> failing, in both directions, on a deliberately perturbed catalog — and Task 30
> shipped eight negative controls. Do the same here: a metric assertion that
> passes against an uninstalled adapter proves nothing, and "emission is silent
> when no adapter is installed" is a plan constraint that makes exactly that
> failure mode easy to ship. Require every metrics test to be shown failing when
> the emission is removed.
>
> **Doc comments in this subsystem assert absolutes and turn out wrong.** The
> count on the `qa-runs` watch path is eight wrong out of eight. Phase 7 produced
> **five more false claims of its own**, caught across three separate review
> rounds — including two in the commit whose whole purpose was to correct false
> claims, and one where the "corrected" line numbers drifted three lines because
> the same commit inserted three lines above them. One claim did survive
> verification (that the public HMAC collect route has no `SecurityContext` to
> enforce against — verified true against the handler), which is the first time
> in the record. Trace every mechanism against the code rather than reading the
> argument above it, and have reviewers verify prose claims, not just code.
>
> **Corollary, worth adopting as a rule:** **cite the item by name, not the
> line.** `file_citations_tests.rs` says outright that it validates file
> existence only and never line numbers, and `doc_citations_tests` scans
> `qa-runs/src/**` only, and only tokens with four or more underscores — so
> neither guard catches a stale `:N`, and Phase 7's eight citations were
> converted to name-based form for that reason. Ten pre-existing qa-insights
> citations still point roughly 150 lines off; they were already wrong before
> that branch.
>
> ## What Phase 7 left that Phase 8 should know
>
> **A pattern worth reusing.** Phase 7's shape — a measured list of what the code
> actually does, in the gear, with a source-scanning gate that fails when a call
> site has no entry, plus a drift test pinning the artefact to that list in both
> directions — is directly applicable to a metric catalog. `domain/metrics.rs`
> naming the families and `domain/ports/metrics.rs` declaring the typed traits
> are two lists that can disagree with the emission sites, and the same
> both-directions discipline applies: a family declared and never emitted is
> dead, and an emission with no declared family is an unnamed series.
>
> **If you duplicate test infrastructure across the four gears, add a parity
> test.** Phase 7 kept four byte-identical copies of a 663-line scanner (four
> separate crates; sharing needs a new dev-dependency crate, which §12 rules out
> of scope) and added **one** parity test, hosted in a single gear, that hashes
> all four and fails on divergence. It must live outside the files it hashes and
> fail loudly rather than skip when a sibling path does not resolve. The
> subsystem's own `no_api_in_domain_tests.rs` is duplicated the same way with a
> written defence.
>
> **`clippy::redundant_pub_crate` is denied workspace-wide**, and the four gears
> differ: `domain` is `pub(crate)` in qa-catalog and qa-environments, and `pub`
> in qa-insights and qa-runs. So an item inside `domain` is `pub` in the first
> two and `pub(crate)` in the last two. It is not an inconsistency; do not
> "fix" it.
>
> **`ResourceType::name()` is not a `const fn`**, which is why every gear now
> carries `*_NAME: &str` consts beside its `ResourceType` descriptors, with 17
> assertions pinning each descriptor to its const. If Phase 8 needs a `&'static
> str` out of a const struct, that is the pattern and the reason.
>
> ## Not part of Phase 8 — do not fold these in
>
> **Phase 7's unclosed residual, recorded in spec §12.** The 71 permissions are
> catalogued and pinned to the enforced set, but no custom RBAC role can target
> them: QA's PEP resource strings are plain strings (`qa.plan`), not GTS type
> ids, so no stub type-schema can be registered for them, and
> `AuthzPermissionV1.resource_type`'s documented contract — a GTS expression — is
> breached too. Renaming the strings is precluded because policies are written
> against them. It is the same decision as authoring the grants and belongs with
> whoever owns `gears/qa-platform/deploy/realm/keycloak/`, which declares no
> roles at all. **Do not resolve this in Phase 8.**
>
> **An unwired credstore PG tier — schedule this one first if you take any of
> these on.** `gears/credstore/plugins/postgres-credstore-plugin/tests/restart_survival_pg.rs`
> needs `CREDSTORE_PG_TEST_DSN` and is named in neither the `Makefile` nor any
> workflow, so it silently self-skips under `make test` and `make ci`. It is the
> only outstanding deferral that weakens a phase already marked closed, and the
> fix is to mirror an existing `test-*-pg` target.
>
> **Three others, all still open:** finding #38's remainder in qa-insights and
> qa-runs (62 delete-or-wire decisions plus ~186 mechanical `pub(crate)` → `pub`
> edits — land the mechanical half as its own commit first); a cross-table cursor
> for `/qa/v1/variables` in the `environment_id` case; and the scanner narrowing
> named as a follow-up in spec §12 (its same-function forwarded-action shape can
> add a pair silently, live today in `variables::upsert` and harmless there
> because another call site enforces the same pair).
>
> **`SelectiveGrantAuthZ` exists only in qa-catalog**, so the other three gears
> still cannot express "holds grants, just not this one". Not blocking anything.
>
> **Three wrong doc line numbers** are parked with their correct values in
> `RESUME-review-remediation-companions.md`. If a Phase 8 task happens to open
> `qa-insights/src/api/rest/handlers/{saved_views,settings}_handler_tests.rs`,
> fix them on the way past; do not make a task of it.
>
> ## Two open questions that are the human's, not yours
>
> 1. **Whether `gears/qa-platform/docs/Reviews/` should become tracked.** The
>    subsystem's own 393-line, 55-finding review — the document this entire
>    remediation answers — is git-ignored by an explicit single-file rule and
>    exists on one machine only. Phase 7 corrected that rule's comment (it
>    claimed the file was a review of a different branch, which was false) and
>    relocated the durable records into tracked files, but deliberately left the
>    ignore in place. Raise it; do not decide it.
> 2. **Whether the branch is squashed per phase or kept granular.** Spec §12 asks
>    for one reviewable commit per phase and all four phases have been squashed
>    that way, each behind a `pre-squash-*` tag. The branch is still unpushed.

---

## Notes for whoever drives that session

**Phase 8 is additive, which changes the risk profile.** It changes no
behaviour by design — the plan makes every emission infallible and silent
without an adapter. That makes it the easiest phase to ship *looking* complete
while measuring nothing, so weight the review effort toward "does this emission
actually fire, with the labels claimed, on the path claimed" rather than toward
correctness of the surrounding code. The negative-control discipline above is
the specific defence.

**Tenant id must not be a label.** The plan states it as cardinality discipline
and as a security constraint, and in this subsystem it is the latter that bites:
a tenant id, run name, branch name or repository URL as a label value is a
disclosure into the metrics pipeline. Treat any free `&str` label in a review
diff as an Important finding, not a style note.

**Task 36 sets the vocabulary every later task emits against.** Keep it on a
capable model; 37–41 are more mechanical once the metric families and typed
label enums are right. This mirrored Phase 7 exactly, where Task 30's
measurement was the thing everything downstream was generated from.

**Re-derive; do not trust the plan's tables.** Phase 7's spec table was wrong in
two independent ways — a resource type that does not exist, and a code snippet
that could not compile — and both were caught only by measuring. The review's
counts have been unreliable in every session so far. The five paths in Phase 8's
own table are a claim; check each entry point still exists at the cited module
before building on it.
