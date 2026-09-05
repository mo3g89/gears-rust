# QA Platform review remediation — core (Phases 1–4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the four CI tiers that exist but run nowhere, then fix the
security, fail-closed and lifecycle findings from the QA Platform review that
survive the product-plugin rework.

**Architecture:** CI gates land first so every later fix arrives against a CI
that can falsify it. Then three code phases in dependency order: security and
fail-closed correctness (Phase 2), IO error classification (Phase 3), and
lifecycle/resource bounds (Phase 4) — where the log-replay duplication (#50) is
fixed *before* the archive is capped (#22), because capping first truncates
duplicated text instead of stopping the duplication.

**Tech Stack:** Rust 2024 (`cf-gears-toolkit`, `sea-orm`, `axum`, `tokio`,
`kube`), `cargo-nextest`, GitHub Actions, nginx, Helm + pytest.

**Spec:** `gears/qa-platform/docs/superpowers/specs/2026-09-05-review-remediation-design.md`

**Companion plans:** Phases 5, 6, 9 →
`2026-09-05-review-remediation-quality.md`; Phase 7 →
`2026-09-05-qa-permission-catalog.md`; Phase 8 →
`2026-09-05-qa-observability.md`. Execute this plan first: Phase 1's gates are
what verify the others.

## Global Constraints

- **Branch:** all work lands on one branch off `feature/qa-product-plugins`,
  one commit per task. Never commit to `main`.
- **The PDP resource string for environments is `"qa.platform"`, not
  `"qa.environment"`.** The rework renamed the aggregate and deliberately kept
  the string (`qa-environments/src/domain/service/mod.rs:90-99`). Never change
  it, and never derive it from a type name.
- **Every fix in Phases 2–4 is TDD:** write the test that asserts the *current
  wrong* behaviour, run it red, fix, run it green. A fix with no red run is not
  done.
- **Run commands are `--lib` for in-lib integration tiers** (`qa-runs`,
  `qa-insights`), `--test` for `qa-catalog`'s, which live in `tests/`.
- **No new `unwrap`/`expect` outside `#[cfg(test)]`.** The workspace lints on
  it; tests carry `#[allow(clippy::unwrap_used, clippy::expect_used)]` at the
  module level, matching the existing handler-test modules.
- **Doc comments carry the reason, not the restatement.** This codebase's
  convention is that a non-obvious decision is explained at the point it is
  made, with the finding or source line cited. Match it.
- **`make clippy` must stay green** (`cargo clippy --workspace --all-targets
  --all-features -D warnings`, plus `cargo hack clippy --each-feature`).

---

# Phase 1 — Verification wiring

Findings #47, #48 (argo + `runner-secret` halves), #49, #52.

Nothing in this phase changes shipped behaviour. Every task ends by *proving the
new gate can fail* — a gate that has never gone red has not been shown to be a
gate.

---

### Task 1: Postgres and git integration tiers for qa-insights and qa-catalog

**Finding:** #47.

Today `grep -n 'qa-catalog\|qa-insights\|qa-environments' Makefile` returns
nothing but the `test-qa-runs-pg` comment block. 13 `#[cfg(feature =
"integration")]` tests in qa-insights (five are the Postgres dialect guards over
the analytics `GROUP BY` / `COUNT(DISTINCT)` reads) and 6 in qa-catalog (the only
real-git-transport and two-tier-lock coverage) are invoked by no target and no
job.

**Files:**
- Modify: `Makefile:494` (`.PHONY` line), after `Makefile:565` (new targets), `Makefile:935` (`ci:`)
- Modify: `.github/workflows/ci.yml:332` (the `integration` job's step list)

**Interfaces:**
- Produces: `make test-qa-insights-pg`, `make test-qa-catalog-git` — used by
  Task 2's sibling target and by `ci:`.

- [ ] **Step 1: Add both targets to the Makefile**

Insert after the `test-qa-runs-pg` target (`Makefile:565`):

```makefile
## Run qa-insights' real-Postgres integration tier.
## Five of these are dialect guards over the analytics reads: Postgres refuses a
## selected column that is not grouped or aggregated, where SQLite picks an
## arbitrary row and says nothing — so a grouping key dropped from
## `grouped_status_counts` passes the SQLite tier and fails at runtime on a
## deployment. The gear ships to Postgres in the Helm chart, so this is the tier
## that can falsify that change.
## `--lib` because the tests need `pub(crate)` services and `#[cfg(test)]`
## fixtures, exactly as qa-runs' do.
test-qa-insights-pg: install-tools
	cargo nextest run -p qa-insights --features integration --lib --retries 1

## Run qa-catalog's real-git-transport integration tier.
## `tests/multi_branch.rs` (4) and `tests/gix_sync_integration.rs` (2) are the
## only coverage of the real git transport, the multi-branch snapshot layout,
## and concurrent syncs through the two-tier locks. They clone a local fixture
## repo through the real transport, which spawns `git upload-pack`, so a `git`
## binary must be on PATH. These live in `tests/`, not in-lib, so the flag is
## `--test`, not `--lib`.
test-qa-catalog-git: install-tools
	@command -v git >/dev/null || (echo "git is required for test-qa-catalog-git" && exit 1)
	cargo nextest run -p qa-catalog --features integration --tests
```

Add both to the `.PHONY` line at `Makefile:494`:

```makefile
.PHONY: test test-no-macros test-macros test-sqlite test-pg test-mysql test-db test-users-info-pg test-usage-collector-pg test-cluster-pg test-fips test-qa-runs-pg test-qa-insights-pg test-qa-catalog-git
```

- [ ] **Step 2: Verify both targets actually select tests**

```bash
make test-qa-insights-pg
make test-qa-catalog-git
```

Expected: qa-insights runs 13 tests, qa-catalog runs 6. **If either reports
"0 tests run", the target is wrong — stop and fix it before continuing.** A
target that selects nothing is the exact defect this task exists to remove.

- [ ] **Step 3: Prove the qa-insights gate can fail**

Temporarily delete one `group_by` call in
`qa-insights/qa-insights/src/infra/storage/results_sea_repo.rs`'s
`grouped_status_counts`, then:

```bash
make test-qa-insights-pg
```

Expected: FAIL, with a Postgres error about a selected column that is not
grouped or aggregated. Restore the line and re-run to confirm green.

- [ ] **Step 4: Prove the qa-catalog gate can fail**

Temporarily change the branch name asserted in
`qa-catalog/qa-catalog/tests/multi_branch.rs` to a branch the fixture does not
have, then:

```bash
make test-qa-catalog-git
```

Expected: FAIL. Restore and re-run green.

- [ ] **Step 5: Wire both into the `integration` CI job**

In `.github/workflows/ci.yml`, immediately after the existing
`Test qa-runs with Postgres (integration)` step (`:331-332`):

```yaml
      - name: Test qa-insights with Postgres (integration)
        run: make test-qa-insights-pg

      - name: Test qa-catalog with the real git transport (integration)
        run: make test-qa-catalog-git
```

- [ ] **Step 6: Wire both into `ci:`**

`Makefile:935` becomes:

```makefile
ci: fmt clippy test-no-macros test-macros test-db deny test-users-info-pg test-usage-collector-pg test-qa-runs-pg test-qa-insights-pg test-qa-catalog-git lychee gts-docs dylint
```

- [ ] **Step 7: Commit**

```bash
git add Makefile .github/workflows/ci.yml
git commit -m "ci(qa-platform): run qa-insights' and qa-catalog's integration tiers

Both tiers existed and were invoked by no target and no job. 13 tests in
qa-insights -- five of them the Postgres dialect guards over the analytics
GROUP BY reads -- and 6 in qa-catalog, the only real-git-transport and
two-tier-lock coverage, ran nowhere. Review finding #47."
```

---

### Task 2: A test target for the two feature-gated adapters the image ships

**Finding:** #48, narrowed. The review's `platform-observation` half is already
resolved — the rework moved the kube observer into `plugins/qa-plugin-k8s`,
which has no feature gate and is an unconditional workspace member, so its 169
tests now run under `cargo nextest run --workspace`. What survives is `argo`
(39 tests under `qa-runs/src/infra/executor/argo/`) and `runner-secret` (13
under `qa-environments/src/infra/runner_secret_*`). Both features ship:
`deploy/cargo-features.argo` names `runner-secret,qa-runs-argo`.

CI's `test` job runs `make test-no-macros` and nothing else, and that is
`cargo nextest run --workspace --exclude …` — per-crate **default** features.
`make clippy` does pass `--all-features`, so this code is compiled and linted;
linting is not running.

**Files:**
- Modify: `Makefile:494` (`.PHONY`), after the Task 1 targets, `Makefile:935` (`ci:`)
- Modify: `.github/workflows/ci.yml` (`integration` job)

**Interfaces:**
- Consumes: nothing from Task 1 beyond placement convention.
- Produces: `make test-qa-platform-features`.

- [ ] **Step 1: Add the target**

```makefile
## Run the unit tier of the two feature-gated adapters the shipped image
## enables (`deploy/cargo-features.argo`: runner-secret, qa-runs-argo).
## `make test-no-macros` is `cargo nextest run --workspace` with per-crate
## DEFAULT features, so `#[cfg(feature = "argo")]` and
## `#[cfg(feature = "runner-secret")]` code is neither compiled nor run there.
## 39 tests under qa-runs::infra::executor::argo and 13 under
## qa-environments' runner-secret writer are what this adds.
## `--lib` only: `qa-runs/tests/argo_cluster.rs`'s 5 are `#[ignore]`d and want
## a live cluster.
test-qa-platform-features: install-tools
	cargo nextest run -p qa-runs --features argo --lib
	cargo nextest run -p qa-environments --features runner-secret --lib
```

Add `test-qa-platform-features` to the `.PHONY` line.

- [ ] **Step 2: Verify it selects the gated tests**

```bash
make test-qa-platform-features
```

Expected: the qa-runs invocation reports **more** tests than
`cargo nextest run -p qa-runs --lib` does. Confirm the delta:

```bash
cargo nextest run -p qa-runs --lib --no-run 2>&1 | tail -2
cargo nextest run -p qa-runs --features argo --lib --no-run 2>&1 | tail -2
```

Expected: the second is 39 higher. If it is not, the feature name is wrong.

- [ ] **Step 3: Prove the gate can fail**

Temporarily break an assertion in
`qa-runs/qa-runs/src/infra/executor/argo/naming.rs`'s tests, run
`make test-qa-platform-features`, confirm FAIL, restore, confirm green.

- [ ] **Step 4: Wire into CI and `ci:`**

In the `integration` job, after Task 1's two steps:

```yaml
      - name: Test qa-platform feature-gated adapters (argo, runner-secret)
        run: make test-qa-platform-features
```

And append `test-qa-platform-features` to `ci:`.

- [ ] **Step 5: Commit**

```bash
git add Makefile .github/workflows/ci.yml
git commit -m "ci(qa-platform): run the feature-gated adapters the image ships

deploy/cargo-features.argo names runner-secret and qa-runs-argo, and CI's only
test command is a default-feature workspace run -- so 39 argo tests and 13
runner-secret tests were compiled by clippy --all-features and executed by
nothing. Review finding #48; its platform-observation half is already resolved
by the plugin rework."
```

---

### Task 3: A CI gate for the UI

**Finding:** #49. Three independent confirmations that the UI has no gate of any
kind: (a) `grep -rn 'npm\|vitest\|tsc\|ui-lint' .github/workflows/` has one hit
and it is the docs site; (b) `ci.yml:43-82`'s `changes` filter declares
`rust|fips|dylint|deps|ci` with no `**/*.tsx` output, so a PR touching only
`qa-platform-ui/**` sets every output false and `clippy`, `test` and
`integration` all skip; (c) `codeql.yml:70-77` analyses `actions`, `python`,
`rust` with `javascript-typescript` left commented at `:78-81`.

The branch adds ~134 UI source files and a 211-test vitest suite. `ui-test`'s
own comment (`Makefile:453-457`) says *"a suite reachable only through `npx
vitest` is a suite no later task and no CI job will run"* — the same failure one
level up, at the runner rather than the target.

**Files:**
- Modify: `.github/workflows/ci.yml:43-82` (filter outputs), plus a new job
- Modify: `.github/workflows/codeql.yml:70-77`

**Interfaces:**
- Consumes: the existing `make ui-lint ui-test ui-build` targets
  (`Makefile:450-465`). `ui-contract` is deliberately **not** wired — it needs a
  live gears stack on `localhost:8087`, as its own comment says.

- [ ] **Step 1: Add the `ui` filter output**

In `ci.yml`, add to the `outputs:` block (after `:54`):

```yaml
      ui:     ${{ steps.filter.outputs.ui }}
```

and to the `filters:` block (after the `ci:` entry at `:81-82`):

```yaml
            ui:
              - 'gears/qa-platform/qa-platform-ui/**'
              - 'Makefile'
```

`Makefile` is in the filter because the `ui-*` targets live there.

- [ ] **Step 2: Add the job**

Add after the `test` job:

```yaml
  ui:
    name: QA Platform UI
    needs: changes
    if: >-
      !cancelled()
      && (needs.changes.result != 'success'
      || needs.changes.outputs.ui == 'true'
      || needs.changes.outputs.ci == 'true'
      || github.event_name == 'workflow_dispatch')
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
        with:
          persist-credentials: false

      - name: Set up Node
        uses: actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020 # v4.4.0
        with:
          node-version: 22
          cache: npm
          cache-dependency-path: gears/qa-platform/qa-platform-ui/package-lock.json

      # `ui-contract` is deliberately absent: it regenerates types from a live
      # /openapi.json and needs the compose stack up on localhost:8087, which
      # this job has no gears to serve.
      - name: Lint, test and build the UI
        run: make ui-lint ui-test ui-build
```

- [ ] **Step 3: Verify the job passes locally**

```bash
make ui-lint ui-test ui-build
```

Expected: lint clean, 211 vitest tests pass, build succeeds.

- [ ] **Step 4: Prove the gate can fail**

Introduce a type error in a `.tsx` — e.g. in
`gears/qa-platform/qa-platform-ui/src/pages/RunsPage.tsx`, assign a `number` to
a `string` prop — then:

```bash
make ui-build
```

Expected: FAIL with a `tsc` error. Revert.

- [ ] **Step 5: Add `javascript-typescript` to CodeQL**

In `codeql.yml`, add to the `matrix.include` list (before the commented block at
`:78`):

```yaml
        - language: javascript-typescript
          build-mode: none
```

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml .github/workflows/codeql.yml
git commit -m "ci(qa-platform): gate the UI

A PR touching only qa-platform-ui set every 'changes' output false, so clippy,
test and integration all skipped and nothing ran the 211-test vitest suite.
Adds a ui filter output, a job running make ui-lint ui-test ui-build, and
javascript-typescript to the CodeQL matrix. ui-contract stays out: it needs a
live gears stack. Review finding #49."
```

---

### Task 4: Run the Helm tests

**Finding:** #52. `deploy/helm/tests/` holds `test_chart_file_sync.py`,
`test_features.py`, `test_no_environment_hardcode.py`,
`test_no_system_gear_changes.py`, `test_pins.py` and `test_nginx_template.sh`,
and no Makefile target and no workflow invokes any of them. FOOTPRINT §2 says of
one of them *"nothing in CI or any Makefile invokes it, so it protects a
deliberate re-run, not an automated one"* — true of all six. They need no
cluster (`helm template` plus file reads), so the `lint` job can hold them.

`test_no_system_gear_changes.py` is the guard FOOTPRINT names for the reverted
system-gear changes; Task 10 of this plan also depends on
`test_nginx_template.sh` being runnable.

**Files:**
- Modify: `Makefile` (new target + `.PHONY`)
- Modify: `.github/workflows/ci.yml:577-600` (the `lint` job)

**Interfaces:**
- Produces: `make helm-tests`, consumed by Task 10's verification.

- [ ] **Step 1: Add the target**

```makefile
.PHONY: helm-tests

## Run the qa-platform Helm chart guards. No cluster needed -- these are
## `helm template` plus file reads -- so the `lint` job holds them.
## Includes `test_no_system_gear_changes.py`, the guard FOOTPRINT names for the
## reverted system-gear changes, and `test_nginx_template.sh`, which is what
## proves the SSE access-log redaction.
helm-tests:
	@command -v helm >/dev/null || (echo "helm is required for helm-tests" && exit 1)
	cd gears/qa-platform/deploy/helm && python3 -m pytest tests/ -q
	bash gears/qa-platform/deploy/helm/tests/test_nginx_template.sh
```

- [ ] **Step 2: Verify it runs and selects tests**

```bash
make helm-tests
```

Expected: five pytest modules collect and pass, then the shell script passes.
**If pytest reports "no tests ran", stop and fix the path.**

- [ ] **Step 3: Prove the gate can fail**

Temporarily change a pinned image tag in
`gears/qa-platform/deploy/helm/qa-platform/values.yaml`, run `make helm-tests`,
confirm `test_pins.py` FAILs, revert, confirm green.

- [ ] **Step 4: Wire into the `lint` job and `ci:`**

Add to `ci.yml`'s `lint` job, after the checkout step:

```yaml
      - name: Set up Python
        uses: actions/setup-python@a26af69be951a213d495a4c3e4e4022e16d87065 # v5.6.0
        with:
          python-version: '3.12'

      - name: Install Helm
        uses: azure/setup-helm@b9e51907a09c216f16ebe8536097933489208112 # v4.3.0

      - name: Install pytest
        run: python3 -m pip install --quiet pytest pyyaml

      - name: Run the qa-platform Helm chart guards
        run: make helm-tests
```

Append `helm-tests` to `ci:`.

- [ ] **Step 5: Commit**

```bash
git add Makefile .github/workflows/ci.yml
git commit -m "ci(qa-platform): run the Helm chart guards

Six guards in deploy/helm/tests/ were invoked by no target and no workflow,
including test_no_system_gear_changes.py -- the guard FOOTPRINT names for the
reverted system-gear changes. They need no cluster, so the lint job holds them.
Review finding #52."
```

---

# Phase 2 — Security and fail-closed correctness

Findings #2, #3, #23, #24, #28, #29, #40, #51, #56.

---

### Task 5: Stop publishing `credential_ref` on the test-repository read DTO

**Finding:** #2 (HIGH, `RUST-SEC-001`).

`TestRepositoryDto` publishes `credential_ref`
(`qa-catalog/qa-catalog/src/api/rest/dto.rs:36`), so any caller who can LIST or
GET a test repository learns the credstore reference for its git credentials.
The field's own doc says the secret material is never returned, which is true
and beside the point: the reference is what a caller redeems.

This is not a new convention. `SshKeyDto` drops `credstore_ref` with a comment
saying not to add it back (`dto.rs:528-529`), and `PlatformDto` does the same
for `kubeconfig_credstore_ref`
(`qa-environments/src/api/rest/dto.rs:107,123`). `TestRepositoryDto` is the
outlier.

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/api/rest/dto.rs:24-59` (the struct and its `From`), `:581,599` (the test)
- Modify: `gears/qa-platform/qa-platform-ui/src/api/generated/openapi.d.ts` (regenerated, not hand-edited — see Step 5)

**Interfaces:**
- Produces: `TestRepositoryDto` with no `credential_ref` field. **Create and
  update requests keep theirs** — `CreateTestRepositoryReq.credential_ref`
  (`dto.rs:84`) and `UpdateTestRepositoryReq.credential_ref` (`:126`) are how a
  caller supplies it and are unchanged.

- [ ] **Step 1: Write the failing test**

Replace the existing `credential_ref` assertion in `dto.rs` (around `:599`) —
it currently asserts the leak — with a test that asserts the field is absent
from the serialized body:

```rust
/// **`credential_ref` must not appear on the read DTO.**
///
/// It names a credstore entry holding the repository's git credentials, and a
/// LIST or GET caller can redeem it. `SshKeyDto` (`:528`) and `PlatformDto`
/// (`qa-environments/.../dto.rs:107`) both drop their credstore ref for the
/// same reason and both say so; this DTO was the outlier. Review finding #2.
///
/// Asserted against the serialized JSON rather than the struct, because the
/// struct not having the field is what a compiler enforces and the wire not
/// carrying it is what an operator cares about.
#[test]
fn the_read_dto_does_not_publish_the_credential_reference() {
    let r = sdk::TestRepository {
        credential_ref: Some("qa-cred".to_owned()),
        ..repository_fixture()
    };
    let body = serde_json::to_string(&TestRepositoryDto::from(r)).unwrap();
    assert!(
        !body.contains("credential_ref"),
        "the read DTO must not publish credential_ref; body was {body}"
    );
    assert!(
        !body.contains("qa-cred"),
        "the read DTO must not publish the reference value; body was {body}"
    );
}
```

If `repository_fixture()` does not exist, lift the struct literal currently at
`dto.rs:575-585` into one — it is already used by two tests.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo nextest run -p qa-catalog the_read_dto_does_not_publish_the_credential_reference
```

Expected: FAIL — the body contains `credential_ref` and `qa-cred`.

- [ ] **Step 3: Remove the field**

In `dto.rs`, delete `:33-36` (the doc comment and the field) from
`TestRepositoryDto`, and delete `credential_ref: r.credential_ref,` from the
`From<sdk::TestRepository>` impl (`:57`). Add to the struct's doc comment:

```rust
/// A test repository as published over REST.
///
/// **`credential_ref` is intentionally absent.** It names the credstore entry
/// holding this repository's git credentials, and a LIST or GET caller can
/// redeem a reference it has been handed. `SshKeyDto` and qa-environments'
/// `PlatformDto` drop their credstore refs for the same reason. Do not add it
/// back. The reference stays on the SDK model
/// (`qa_catalog_sdk::TestRepository::credential_ref`), which is where the sync
/// path reads it. Review finding #2.
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo nextest run -p qa-catalog --lib
```

Expected: PASS, and no other qa-catalog test regresses. Fix any test that
asserted the old shape by deleting the assertion, **not** by re-adding the
field.

- [ ] **Step 5: Regenerate and check the UI wire types**

```bash
grep -rn 'credential_ref' gears/qa-platform/qa-platform-ui/src/
```

If the generated `openapi.d.ts` or any UI code reads `credential_ref` off a
`TestRepositoryDto`, remove the read — the UI never renders a credstore
reference, and if it does, that is a second instance of the same finding. Do not
regenerate via `make ui-contract` here (it needs a live stack); hand-edit the
generated type's `TestRepositoryDto` entry to drop the property and note in the
commit that `make ui-contract` should be re-run against a live stack before
release.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-catalog gears/qa-platform/qa-platform-ui
git commit -m "fix(qa-catalog)!: stop publishing credential_ref on TestRepositoryDto

A LIST or GET caller received the credstore reference for the repository's git
credentials. SshKeyDto and qa-environments' PlatformDto already drop their
credstore refs and say not to add them back; this DTO was the outlier. Create
and update requests keep the field -- that is how a caller supplies it.

Wire-visible change: run make ui-contract against a live stack before release.
Review finding #2."
```

---

### Task 6: A PDP compile failure is a server fault, not a denial

**Finding:** #3 (HIGH, `RUST-ERR-001` / `Z1-1`), ×4 gears.

`EnforcerError::CompileFailed` is mapped to `DomainError::Forbidden` in all four
gears, so a scope that will not compile answers **403** — telling an operator
they lack a permission when the truth is that the policy engine is broken. The
gears already classify the two differently for logging (`log_enforcer_error`
logs `Denied` at DEBUG and `CompileFailed` at ERROR, and its doc explains why);
only the status mapping is wrong.

The rework widened this: `QaProductRegistry::plugin_for`
(`qa-catalog/src/domain/service/plugin_registry.rs:188`) resolves through the
same `From`, so a compile fault during plugin resolution now also reads as "you
may not use this product's plugin".

Behaviour is unchanged in the direction that matters — the request still fails
closed. Only the status and the message change.

**Files:**
- Modify: `qa-catalog/qa-catalog/src/domain/error.rs:117-127`
- Modify: `qa-environments/qa-environments/src/domain/error.rs:71-80`
- Modify: `qa-insights/qa-insights/src/domain/error.rs:333-342`
- Modify: `qa-runs/qa-runs/src/domain/error.rs:403-412`
- Modify: `qa-insights/qa-insights/src/domain/service/mod.rs:627` (a doc comment that states the old mapping)

**Interfaces:**
- Produces: `From<EnforcerError> for DomainError` mapping `Denied` →
  `Forbidden`, `CompileFailed` → `Internal`, `EvaluationFailed` → `Internal`.

- [ ] **Step 1: Write the failing test (qa-catalog first)**

Add to `qa-catalog/qa-catalog/src/domain/error.rs`'s test module (create one if
absent, with `#[allow(clippy::unwrap_used, clippy::expect_used)]`):

```rust
/// **A scope that will not compile is a server fault, not a denial.**
///
/// `CompileFailed` means the PDP could not build an `AccessScope` — a broken
/// policy document, a resolver that answered with something unparseable. The
/// request must still fail closed, and it does, but answering 403 tells an
/// operator they lack a permission when what they lack is a working policy
/// engine. `log_enforcer_error` already logs this at ERROR and `Denied` at
/// DEBUG; the status mapping was the half that disagreed.
///
/// Review finding #3, found in all four gears.
#[test]
fn a_scope_compile_failure_is_internal_not_forbidden() {
    let e = authz_resolver_sdk::EnforcerError::CompileFailed(
        "unparseable policy document".to_owned(),
    );
    assert!(
        matches!(DomainError::from(e), DomainError::Internal(_)),
        "CompileFailed must map to Internal (500), not Forbidden (403)"
    );
}

/// The other half of the same rule: a genuine denial is still a denial.
#[test]
fn a_denial_is_still_forbidden() {
    let e = authz_resolver_sdk::EnforcerError::Denied {
        reason: "no matching grant".to_owned(),
    };
    assert!(matches!(DomainError::from(e), DomainError::Forbidden));
}
```

**Check `EnforcerError`'s actual variant shapes before writing this** —
`CompileFailed`'s payload and `Denied`'s fields must match the SDK:

```bash
grep -rn 'enum EnforcerError' -A 20 $(cargo metadata --format-version 1 \
  | python3 -c "import json,sys;print([p['manifest_path'] for p in json.load(sys.stdin)['packages'] if p['name']=='authz-resolver-sdk'][0])" \
  | xargs dirname)/src
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo nextest run -p qa-catalog a_scope_compile_failure_is_internal_not_forbidden
```

Expected: FAIL — the value is `Forbidden`.

- [ ] **Step 3: Fix the mapping in qa-catalog**

`qa-catalog/qa-catalog/src/domain/error.rs:117-127` becomes:

```rust
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        log_enforcer_error(&e);
        match e {
            // A denial is the routine, expected outcome and the only one that
            // is about the caller.
            authz_resolver_sdk::EnforcerError::Denied { .. } => Self::Forbidden,
            // A scope that will not compile, and a resolver that failed to
            // answer, are both faults in the policy engine. They still fail
            // closed -- no scope is produced, so no row is reachable -- but
            // they are 500s, not 403s: telling an operator they lack a
            // permission when the PDP is broken sends them to the wrong
            // system. `log_enforcer_error` already draws this exact line for
            // log levels. Review finding #3.
            authz_resolver_sdk::EnforcerError::CompileFailed(err) => {
                Self::Internal(format!("authorization scope compilation failed: {err}"))
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(err) => {
                Self::Internal(err.to_string())
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo nextest run -p qa-catalog --lib
```

Expected: PASS. If a test asserted a 403 from a compile failure, update it — the
403 was the defect.

- [ ] **Step 5: Repeat for the other three gears**

Apply the identical change and the identical pair of tests to:

- `qa-environments/qa-environments/src/domain/error.rs:76`
- `qa-insights/qa-insights/src/domain/error.rs:338`
- `qa-runs/qa-runs/src/domain/error.rs:408`

Each gear gets its own copy of both tests — the mapping is per-gear and a shared
test would not catch one gear drifting.

Then fix the stale doc comment at
`qa-insights/qa-insights/src/domain/service/mod.rs:627`, which currently says
`EnforcerError::CompileFailed` becomes `DomainError::Forbidden`.

- [ ] **Step 6: Verify the REST layer renders 500, not 403**

```bash
cargo nextest run -p qa-catalog -p qa-environments -p qa-insights -p qa-runs --lib
```

Expected: PASS. Confirm no gear's `api/rest/error.rs` special-cases
`Internal` in a way that would disclose the message — `qa-runs`'
`opaque_internal` set (`api/rest/error.rs:523-528`) already covers `Internal`,
which is what keeps the policy text server-side.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform
git commit -m "fix(qa-platform): a PDP compile failure is 500, not 403

EnforcerError::CompileFailed mapped to Forbidden in all four gears, so a policy
document the engine cannot compile told the caller they lacked a permission.
The request still fails closed either way; only the status was lying.
log_enforcer_error already drew this line for log levels.

The plugin rework widened it: QaProductRegistry::plugin_for resolves through
the same From, so a compile fault during plugin resolution read as 'you may not
use this product's plugin'. Review finding #3."
```

---

### Task 7: Drive the public collect endpoint

**Finding:** #23 (HIGH, `RUST-TEST-001` / `Z6-1`).

`POST /qa/v1/collect/{repo_id}` is `.public()` and authenticates with an HMAC
over `(repo_id, branch, tenant_id)`. The verify itself is sound and the review
confirms it: `domain/service/collect.rs:654-665` uses
`aws_lc_rs::hmac::verify` (constant-time), fails closed on an empty signing
secret *before* touching `aws_lc_rs` at all, and mints the system actor only
after the check passes.

What is missing is anything that would notice if it stopped being sound.
`handlers/collect.rs`' test module has two tests and both are about query-string
decoding.

**Files:**
- Create: `gears/qa-platform/qa-insights/qa-insights/src/api/rest/handlers/collect_handler_tests.rs`
- Modify: `gears/qa-platform/qa-insights/qa-insights/src/api/rest/handlers/collect.rs` (add the `#[path]` include)

**Interfaces:**
- Consumes: `crate::domain::service::test_support::{ctx, FakeRuns}` and whatever
  fixture builds a `ConcreteAppServices` — mirror
  `qa-runs/src/api/rest/handlers/queue_handler_tests.rs`'s `Fleet`, which is the
  established shape (`handlers::schedules`' `handler_tests` set the precedent).
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Write the three failing tests**

Create `collect_handler_tests.rs`:

```rust
//! `POST /qa/v1/collect/{repo_id}` driven over real services.
//!
//! This route is `.public()` and authenticates with an HMAC over
//! `(repo_id, branch, tenant_id)`. The verification is sound —
//! `domain::service::collect`'s `verify_signature` is constant-time, fails
//! closed on an empty signing secret before touching `aws_lc_rs`, and mints
//! the system actor only after the check passes. Nothing drove it.
//!
//! Three paths, one test each: a nil tenant is refused before any signature
//! work, a bad signature is refused **and writes nothing**, and a correctly
//! signed report reaches the repository. Review finding #23.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use toolkit::api::canonical_prelude::{IntoResponse, Json};
use uuid::Uuid;

use super::report_collect_count;
use crate::api::rest::dto::{CollectCountReq, CollectReportQuery};

const REPO: Uuid = Uuid::from_u128(0x0C01_0000_0000_0001);
const TENANT: Uuid = Uuid::from_u128(0x0C01_0000_0000_0002);
const BRANCH: &str = "main";
const SIGNING_SECRET: &str = "test-collect-secret";

async fn rendered(response: axum::response::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// **A nil tenant is refused before any signature work.**
///
/// `TenantBound::new` is what rejects it, and the refusal is a 400 naming the
/// field — not a 403, because the caller supplied a malformed request rather
/// than a wrong credential.
#[tokio::test]
async fn a_nil_tenant_id_is_a_400_naming_the_field() {
    let fleet = collect_fleet(SIGNING_SECRET).await;
    let query = CollectReportQuery {
        tenant_id: Uuid::nil(),
        branch: BRANCH.to_owned(),
        sig: sign(REPO, BRANCH, Uuid::nil(), SIGNING_SECRET),
    };
    let response = report_collect_count(
        Extension(Arc::clone(&fleet.services)),
        Path(REPO),
        Query(query),
        Json(CollectCountReq { test_file: "a.py".to_owned(), case_count: 3 }),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(status, 400, "a nil tenant must be a 400; body was {body}");
    assert!(
        body.contains("tenant_id"),
        "the refusal must name the field; body was {body}"
    );
}

/// **A bad signature is refused AND writes nothing.**
///
/// The second half is the one that matters and the one a status-only assertion
/// would miss: a handler that wrote first and checked afterwards would pass a
/// 403 assertion and still have persisted the row.
#[tokio::test]
async fn a_bad_signature_is_refused_and_writes_nothing() {
    let fleet = collect_fleet(SIGNING_SECRET).await;
    let query = CollectReportQuery {
        tenant_id: TENANT,
        branch: BRANCH.to_owned(),
        sig: "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
    };
    let response = report_collect_count(
        Extension(Arc::clone(&fleet.services)),
        Path(REPO),
        Query(query),
        Json(CollectCountReq { test_file: "a.py".to_owned(), case_count: 3 }),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(status, 403, "a bad signature must be a 403; body was {body}");
    assert_eq!(
        fleet.recorded_counts().len(),
        0,
        "a refused report must not have written a count row"
    );
}

/// **A correctly signed report reaches the repository.**
///
/// The happy path, so the two refusals above are shown to be refusals of
/// something that otherwise works — without this, both could pass against a
/// handler that refuses everything.
#[tokio::test]
async fn a_correctly_signed_report_is_recorded() {
    let fleet = collect_fleet(SIGNING_SECRET).await;
    let query = CollectReportQuery {
        tenant_id: TENANT,
        branch: BRANCH.to_owned(),
        sig: sign(REPO, BRANCH, TENANT, SIGNING_SECRET),
    };
    let response = report_collect_count(
        Extension(Arc::clone(&fleet.services)),
        Path(REPO),
        Query(query),
        Json(CollectCountReq { test_file: "a.py".to_owned(), case_count: 3 }),
    )
    .await
    .into_response();

    let (status, body) = rendered(response).await;
    assert_eq!(status, 200, "a signed report must be accepted; body was {body}");
    let recorded = fleet.recorded_counts();
    assert_eq!(recorded.len(), 1, "the count must have been written");
    assert_eq!(recorded[0].case_count, 3);
}
```

**Two helpers this needs and you must write against the real code, not guess:**

- `collect_fleet(secret)` — builds `ConcreteAppServices` with the collect
  signing secret set and an inspectable results repository. Model on
  `qa-runs/src/api/rest/handlers/queue_handler_tests.rs`'s `Fleet`; read
  `qa-insights/src/domain/service/test_support.rs` for the fixtures that
  already exist there.
- `sign(repo_id, branch, tenant_id, secret)` — must produce the *same* bytes
  `CollectService::verify_signature` checks. Read
  `qa-insights/src/domain/service/collect.rs:654-665` and reproduce its message
  construction exactly. **Do not** call the production signing function if one
  exists in the same module — a test that signs with the code it verifies
  proves only self-consistency. If the only way to sign is the production
  helper, assert the message layout separately.

- [ ] **Step 2: Wire the module in and run the tests to verify they fail**

At the bottom of `handlers/collect.rs`, beside the existing `mod tests`:

```rust
#[cfg(test)]
#[path = "collect_handler_tests.rs"]
mod collect_handler_tests;
```

```bash
cargo nextest run -p qa-insights collect_handler_tests
```

Expected: compile errors first (the helpers do not exist), then FAIL. **These
tests are expected to pass once the helpers are right — this task adds coverage,
it does not fix a defect.** If `a_bad_signature_is_refused_and_writes_nothing`
fails on the *write* assertion rather than compiling, you have found a real
defect: stop, report it, and fix it before moving on.

- [ ] **Step 3: Make them pass**

Implement the two helpers. No production change is expected.

- [ ] **Step 4: Run the whole gear**

```bash
cargo nextest run -p qa-insights --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-insights
git commit -m "test(qa-insights): drive the public collect endpoint

POST /qa/v1/collect/{repo_id} is .public() and authenticates with an HMAC, and
nothing drove it -- the handler's only two tests were about query-string
decoding. Three paths now: a nil tenant is a 400 naming the field, a bad
signature is a 403 that writes nothing, and a signed report is recorded.

The write assertion is the load-bearing one: a handler that wrote before
checking would pass a status-only test. Review finding #23."
```

---

### Task 8: Two missing error-attribution tests

**Findings:** #24 and #40.

`api/rest/error.rs`'s `match` is exhaustive over `DomainError` variants, so the
compiler guarantees every variant has a status — and says nothing about which of
the gear's resource types an *endpoint* names. The review notes the same defect
was found in six places across three reviews for exactly that reason. The
saved-view and JIRA wrappers have these tests; `as_notification_error`
(`error.rs:367`) and `as_saved_view_error` do not have a handler-level one.

**Files:**
- Create: `gears/qa-platform/qa-insights/qa-insights/src/api/rest/handlers/settings_handler_tests.rs`
- Create: `gears/qa-platform/qa-insights/qa-insights/src/api/rest/handlers/saved_views_handler_tests.rs`
- Modify: `handlers/settings.rs` and `handlers/saved_views.rs` (`#[path]` includes)

**Interfaces:**
- Consumes: the `collect_fleet`-style services fixture from Task 7 — if it is
  general enough, lift it into `test_support`; if not, write a sibling.
- Produces: nothing.

- [ ] **Step 1: Write the failing tests**

`settings_handler_tests.rs`:

```rust
//! The notification-config handlers driven over real services, for the one
//! property nothing else checks: **a refusal names `qa.notification_config`**.
//!
//! `api::rest::error`'s match is exhaustive over `DomainError` *variants*, so
//! the compiler guarantees every variant has a status and says nothing about
//! which resource type an endpoint attributes a refusal to. A 403 is the most
//! likely error on a fresh deployment — a policy engine not yet taught
//! `qa.notification_config` refuses everything — and it carries no field, so
//! the resource type is the only actionable thing in the body.
//!
//! Same shape as the saved-view and JIRA wrappers' existing tests.
//! Review finding #24.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use uuid::Uuid;

/// The resource type the notification endpoints must attribute a refusal to.
const NOTIFICATION_GTS: &str = "cf.qa.insights.notification_config.v1~";

const TENANT: Uuid = Uuid::from_u128(0x0E01_0000_0000_0001);

#[tokio::test]
async fn a_denied_notification_read_names_the_notification_config() {
    let fleet = denying_fleet().await;
    let (status, body) = get_notification_config_rendered(&fleet, TENANT).await;
    assert_eq!(status, 403, "body was {body}");
    assert!(
        body.contains(NOTIFICATION_GTS),
        "a denial must name qa.notification_config, not another resource type; \
         body was {body}"
    );
}

#[tokio::test]
async fn a_denied_notification_write_names_the_notification_config() {
    let fleet = denying_fleet().await;
    let (status, body) = put_notification_config_rendered(&fleet, TENANT).await;
    assert_eq!(status, 403, "body was {body}");
    assert!(body.contains(NOTIFICATION_GTS), "body was {body}");
}
```

`saved_views_handler_tests.rs` follows the same shape for `list` and `create`,
asserting the body names `qa.saved_view`:

```rust
//! The saved-view handlers driven over real services. Same property, same
//! reason as `settings_handler_tests`: `as_saved_view_error` re-attributes a
//! refusal to `qa.saved_view` and nothing drove it through a handler.
//! Review finding #40.
#![allow(clippy::unwrap_used, clippy::expect_used)]

const SAVED_VIEW_GTS: &str = "cf.qa.insights.saved_view.v1~";

#[tokio::test]
async fn a_denied_saved_view_list_names_the_saved_view() { /* as above */ }

#[tokio::test]
async fn a_denied_saved_view_create_names_the_saved_view() { /* as above */ }
```

**Confirm the two GTS id strings against the code before asserting them** —
read `as_notification_error` (`error.rs:367`) and `as_saved_view_error` and use
the ids they actually emit. A test asserting a guessed id is worse than no test.

- [ ] **Step 2: Run to verify they fail**

```bash
cargo nextest run -p qa-insights settings_handler_tests saved_views_handler_tests
```

Expected: compile errors, then FAIL if either wrapper names the wrong type. If
both already name the right type, the tests pass on first green run — that is
the expected outcome, and the tests are still the deliverable.

- [ ] **Step 3: Make them pass**

Write `denying_fleet()` (a services fixture whose policy enforcer denies) and
the two `*_rendered` helpers. Fix any wrapper that names the wrong resource
type.

- [ ] **Step 4: Run the gear**

```bash
cargo nextest run -p qa-insights --lib
```

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-insights
git commit -m "test(qa-insights): pin the notification and saved-view refusal attribution

api::rest::error's match is exhaustive over DomainError variants, so the
compiler pins every status and nothing pins which resource type an endpoint
names in a refusal -- the defect three prior reviews found in six places.
as_notification_error and as_saved_view_error had no handler-level test.
Review findings #24 and #40."
```

---

### Task 9: Three silent drops

**Findings:** #28, #29, #56.

Three places where a failure is swallowed and the caller is told nothing went
wrong. Grouped because they are one defect shape and one commit's worth of work.

**Files:**
- Modify: `qa-insights/qa-insights/src/infra/jira/oagw_client.rs:601`
- Modify: `qa-environments/qa-environments/src/infra/storage/environments_sea_repo.rs:419`
- Modify: `qa-insights/qa-insights/src/gear.rs:1228,1251`

**Interfaces:**
- Produces: no signature changes.

- [ ] **Step 1: Write the failing test for #28**

`oagw_client.rs:601` is `serde_json::from_slice(&body).ok()?` — a JIRA 200 whose
body is not JSON is dropped with no log, and the poller then silently re-files
the bug. The HTTP-error arms directly above it both `warn!`.

```rust
/// **A 200 with a non-JSON body is logged, not silently dropped.**
///
/// The HTTP-error arms above warn and return `None`; this arm returned `None`
/// with nothing written down, and the caller's next act is to re-file a bug it
/// believes is still open. An operator debugging a duplicate-bug storm had no
/// line to grep for. Review finding #28.
#[tokio::test]
async fn a_non_json_success_body_is_warned_about() {
    let logs = capture_logs();
    let client = client_returning(200, b"<html>gateway error</html>");
    assert!(client.fetch_issue_status("VHP-1").await.is_none());
    assert!(
        logs.contains("could not be parsed"),
        "a dropped 200 body must leave a log line; captured: {logs}"
    );
}
```

Use whatever log-capture harness qa-insights already has — check
`qa-product-sdk/src/testing.rs`'s `assert_no_leak`/`Canary` and
`qa-insights`' own test modules for the established one. If none exists, assert
on the returned value plus a `#[must_use]`-style structural change instead, and
say so in the test doc rather than inventing a harness.

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p qa-insights a_non_json_success_body_is_warned_about
```

Expected: FAIL — no log line.

- [ ] **Step 3: Fix #28**

```rust
        let document: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(document) => document,
            Err(error) => {
                // Same treatment as the HTTP-error arms above. A 200 whose body
                // is not JSON is a gateway or proxy answering in place of JIRA;
                // dropping it silently makes the caller re-file a bug it thinks
                // is still open, with nothing in the log to explain the
                // duplicate. Review finding #28.
                warn!(%error, "JIRA answered 200 with a body that could not be parsed as JSON");
                return None;
            }
        };
```

- [ ] **Step 4: Fix #29**

`environments_sea_repo.rs:419` currently answers
`serde_json::to_value(observation.attrs()).unwrap_or_else(|_| json!({}))` and
continues to write a `Checked` status — recording "we looked and saw nothing"
when what happened is "we could not serialize what we saw".

`ObservedAttrs` is a `BTreeMap<String, String>`, so `to_value` cannot in fact
fail today. That makes this a latent trap rather than a live bug, and the fix is
to make the impossible case loud instead of silent:

```rust
            PluginObservationOutcome::Detected(_) => {
                let roles = observation.roles();
                // `ObservedAttrs` is a `BTreeMap<String, String>`, so this
                // cannot fail today. It is matched rather than defaulted
                // because the alternative wrote an empty attribute map
                // *together with* a `Checked` status -- recording "we looked
                // and saw nothing" for what is actually "we could not
                // serialize what we saw". If the map's value type ever widens,
                // this must skip the health write, not invent one.
                // Review finding #29.
                let attrs = match serde_json::to_value(observation.attrs()) {
                    Ok(attrs) => attrs,
                    Err(error) => {
                        warn!(
                            environment_id = %id,
                            %error,
                            "qa-environments: observed attributes could not be serialized; \
                             skipping this health write rather than recording an empty one"
                        );
                        return Ok(());
                    }
                };
```

- [ ] **Step 5: Fix #56**

`qa-insights/src/gear.rs:1228` and `:1251` are
`for tenant in tenants_for(services, ROLE).await.unwrap_or_default()`. A failed
tenant-directory read becomes an empty list, so the ticker logs nothing and does
nothing — a gear whose background work has silently stopped is indistinguishable
from one with no tenants.

```rust
    // A failed directory read is not "this deployment has no tenants". Skipping
    // the cycle with a log is recoverable -- the next tick retries -- while
    // `unwrap_or_default()` made a stopped ticker look identical to an idle
    // one. Review finding #56.
    let tenants = match tenants_for(services, ROLE_JIRA_POLLER).await {
        Ok(tenants) => tenants,
        Err(error) => {
            warn!(
                %error,
                role = ROLE_JIRA_POLLER,
                "qa-insights: could not enumerate tenants; skipping this pass"
            );
            return;
        }
    };
    for tenant in tenants {
```

Apply the same to the collect ticker at `:1251` with `ROLE_COLLECT`, and check
whether the reconcile ticker has the same call — if it does, fix it too.

- [ ] **Step 6: Run all three gears' tests**

```bash
cargo nextest run -p qa-insights -p qa-environments --lib
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform
git commit -m "fix(qa-platform): three silent drops

- A JIRA 200 with a non-JSON body was dropped with no log, then the bug was
  silently re-filed. Now warns, like the HTTP-error arms beside it (#28).
- A to_value failure wrote empty attrs together with a Checked status --
  'we looked and saw nothing' for 'we could not serialize what we saw'.
  Cannot fail today since ObservedAttrs is a BTreeMap<String, String>, so this
  closes a latent trap rather than a live bug (#29).
- tenants_for(...).unwrap_or_default() turned a failed directory read into an
  empty tenant list, so qa-insights' tickers stopped silently. Found during
  validation of the review against the rework; filed as #56."
```

---

### Task 10: Keep bearer tokens out of the UI access log

**Finding:** #51 (`Z11-1`).

`qa-platform-ui/default.conf.template:107-111` maps `$arg_access_token` onto an
`Authorization` header for `location ~ ^/qa/v1/runs/[^/]+/logs$` (`:269`). The
bridge is necessary and careful — `EventSource` cannot set headers, the gears
read a credential only from `AUTHORIZATION`, it falls through to
`$http_authorization` when there is no parameter, and it fails closed when there
is neither. The review verifies all of that and confirms no other location names
`$sse_authorization`.

What is missing is the log side: the file contains no `access_log` and no
`log_format` (`grep -n 'access_log\|log_format'` → nothing), so nginx's default
`combined` writes the full request line and the token with it. The file measures
this itself at `:80-86` — 60 such lines in `docker compose logs ui` "and only
ever grows". In the Helm deployment those lines go wherever the cluster ships
container stdout. The 5-minute `accessTokenLifespan` bounds the window, not the
disclosure.

**Files:**
- Modify: `gears/qa-platform/qa-platform-ui/default.conf.template` (near `:100` for the format, inside the block at `:269`)
- Modify: `gears/qa-platform/deploy/helm/tests/test_nginx_template.sh`

**Interfaces:**
- Produces: a `log_format sse_no_query` used by exactly one `access_log`.

- [ ] **Step 1: Write the failing check**

Add to `deploy/helm/tests/test_nginx_template.sh`:

```bash
# The SSE location must not log the query string: `?access_token=eyJ...` is a
# live bearer credential and nginx's default `combined` format writes the whole
# request line. Review finding #51.
grep -q 'log_format sse_no_query' "$CONF" \
  || { echo "FAIL: no sse_no_query log_format"; exit 1; }
grep -q 'access_log .* sse_no_query' "$CONF" \
  || { echo "FAIL: sse_no_query is defined but never applied"; exit 1; }
# And it must be applied inside the SSE location only -- a server-level
# access_log would change every route's log shape, which is not what this is.
awk '/location ~ \^\/qa\/v1\/runs/,/^    }/' "$CONF" | grep -q 'access_log .* sse_no_query' \
  || { echo "FAIL: sse_no_query is not applied inside the SSE location"; exit 1; }
```

Adjust `$CONF` to whatever variable that script already uses for the rendered
template.

- [ ] **Step 2: Run to verify it fails**

```bash
make helm-tests
```

Expected: FAIL with `no sse_no_query log_format`. (Task 4 is what makes this
runnable — if `make helm-tests` does not exist, do Task 4 first.)

- [ ] **Step 3: Add the log format**

In `default.conf.template`, beside the existing `map` block (around `:100`):

```nginx
# The SSE log-line format: `$uri` rather than `$request`, so the path is logged
# and the query string is not.
#
# The bridge below maps `?access_token=<jwt>` onto an `Authorization` header
# because `EventSource` cannot set headers and the gears read a credential only
# from `AUTHORIZATION`. That transport is by design (CONTRACT-DIFF 12.5). What
# was not by design is nginx's default `combined` format writing the whole
# request line -- token included -- to stdout, which in the Helm deployment goes
# wherever the cluster ships container logs. Measured at 60 such lines while
# this file's own comment at :80-86 was being written.
#
# Applied inside the SSE location ONLY. A server-level `access_log` here would
# silently change the log shape of every other route. Review finding #51.
log_format sse_no_query '$remote_addr - $remote_user [$time_local] '
                        '"$request_method $uri $server_protocol" '
                        '$status $body_bytes_sent "$http_referer" "$http_user_agent"';
```

- [ ] **Step 4: Apply it inside the one location**

Inside `location ~ ^/qa/v1/runs/[^/]+/logs$` (`:269`), as its first directive:

```nginx
        access_log /var/log/nginx/access.log sse_no_query;
```

- [ ] **Step 5: Run to verify it passes**

```bash
make helm-tests
```

Expected: PASS.

- [ ] **Step 6: Verify against a running container**

```bash
cd gears/qa-platform/deploy/compose && docker compose up -d
curl -s "localhost:8080/qa/v1/runs/00000000-0000-0000-0000-000000000000/logs?access_token=eyJtest" >/dev/null
docker compose logs ui | grep -c 'access_token=' || true
```

Expected: `0`. Confirm the SSE route still works — the bridge must be
untouched:

```bash
docker compose logs ui | grep 'qa/v1/runs' | tail -1
```

Expected: a line showing the path with no query string.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-platform-ui gears/qa-platform/deploy/helm
git commit -m "fix(qa-platform-ui): keep SSE bearer tokens out of the access log

default.conf.template set no access_log and no log_format, so nginx's default
combined format wrote the full request line -- ?access_token=eyJ... included --
for the one route that carries a token in the query string. The file measured
60 such lines itself. In the Helm deployment they go wherever the cluster ships
container stdout.

A log_format logging \$uri rather than \$request, applied with access_log inside
that one location. The bridge, the fail-closed behaviour and the one-route blast
radius are untouched. Guarded by test_nginx_template.sh. Review finding #51."
```

---

# Phase 3 — IO error classification

Findings #6, #7, #8, #9, #26, plus two sites found during validation
(`plans.rs:389`, `:850`).

**The shared rule:** a filesystem call answers "absent" only for
`std::io::ErrorKind::NotFound`. Every other kind — `PermissionDenied`,
`NotADirectory`, an IO error mid-read — becomes `Internal` or `SyncFailed`
carrying the original as `#[source]`. `qa-catalog`'s own
`domain/service/repos.rs` already does exactly this at `:297` and `:336`; this
phase applies that gear's existing rule to the six sites that do not follow it.

---

### Task 11: Only `NotFound` means the file is absent

**Findings:** #6, #7, #8, #26, plus `plans.rs:389` and `:850`.

| Site | Today | After |
|---|---|---|
| `plans.rs:183` | any `read_to_string` failure → `PlanNotFound` (404) | NotFound → `PlanNotFound`; else `Internal` |
| `plans.rs:230` | any failure → `FileNotFound` | same rule |
| `plans.rs:389` | `let Ok(content) = … else` | same rule |
| `plans.rs:629,630` | `canonicalize` failure → `RepoNotSynced` | `Internal` when the path exists |
| `plans.rs:649` | any `canonicalize` failure → `Ok(None)` | `Ok(None)` only on NotFound |
| `plans.rs:850` | `.is_ok_and(…)` | same rule |

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans.rs` (six sites)
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/error.rs` (if a `SyncFailed`-shaped variant with a source is needed)
- Test: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans_tests.rs`

**Interfaces:**
- Produces: `fn io_is_absent(error: &std::io::Error) -> bool` — a private helper
  in `plans.rs`, used by all six sites. Task 12 does **not** consume it (that is
  a cross-gear call, not a filesystem read).

- [ ] **Step 1: Write the failing test**

The reliable way to produce a non-`NotFound` IO error in a test is a
`PermissionDenied` on a `chmod 000` file. On CI running as root that does not
deny, so gate it:

```rust
/// **A file that exists but cannot be read is not a missing plan.**
///
/// `get_plan` mapped every `read_to_string` failure to `PlanNotFound`, which
/// the REST layer renders 404. An operator whose snapshot directory has wrong
/// permissions was told the plan does not exist, and went looking in the
/// repository instead of at the filesystem. `domain::service::repos` already
/// draws this line correctly at `:297` and `:336`; this file did not.
/// Review finding #6.
///
/// Skipped when running as root, which cannot be denied by mode bits.
#[tokio::test]
async fn an_unreadable_plan_file_is_internal_not_not_found() {
    if nix_is_root() {
        return;
    }
    let f = fixture_with_synced_repo().await;
    let plan = f.snapshot_dir().join("plan.yaml");
    std::fs::write(&plan, "name: x\n").unwrap();
    std::fs::set_permissions(&plan, std::os::unix::fs::PermissionsExt::from_mode(0o000))
        .unwrap();

    let err = f.svc.get_plan(&ctx(f.tenant), f.repo_id, "main", "plan.yaml")
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Internal(_)),
        "an EACCES on an existing plan must not be PlanNotFound; got {err:?}"
    );
}

/// The other half: a genuinely absent file is still a 404.
#[tokio::test]
async fn a_genuinely_absent_plan_is_still_not_found() {
    let f = fixture_with_synced_repo().await;
    let err = f.svc.get_plan(&ctx(f.tenant), f.repo_id, "main", "nope.yaml")
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::PlanNotFound { .. }), "got {err:?}");
}
```

Write the same pair for `get_test_meta` (#7) and for `resolve_under_root`'s
`Ok(None)` (#8). Both halves matter: without the second, the fix could
regress every 404 into a 500 and still pass.

- [ ] **Step 2: Run to verify they fail**

```bash
cargo nextest run -p qa-catalog an_unreadable_plan_file_is_internal_not_not_found
```

Expected: FAIL — the error is `PlanNotFound`.

- [ ] **Step 3: Add the shared helper**

In `plans.rs`, near the other private helpers:

```rust
/// Whether an IO error means "this path is not there" as opposed to "this path
/// could not be read".
///
/// The distinction is load-bearing in this module: the not-there answers are
/// `PlanNotFound` / `FileNotFound` / `Ok(None)`, which the REST layer renders
/// 404 and which the exclusivity tier reads as "this file has no opinion". An
/// `EACCES`, a `NotADirectory` or a mid-read IO failure answered the same way,
/// so a misconfigured snapshot directory presented as a missing plan and an
/// unreadable test file silently dropped its exclusivity vote.
///
/// `domain::service::repos` already draws this line at `:297` and `:336`. This
/// is the same rule, named once so the six sites below cannot drift.
/// Review findings #6, #7, #8, #26.
fn io_is_absent(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}
```

- [ ] **Step 4: Apply it at all six sites**

`plans.rs:181-186` (#6):

```rust
        let source = match tokio::fs::read_to_string(&file).await {
            Ok(source) => source,
            Err(error) if io_is_absent(&error) => {
                return Err(DomainError::PlanNotFound {
                    repo_id,
                    branch: branch.to_owned(),
                    path: path.to_owned(),
                });
            }
            Err(error) => {
                return Err(DomainError::Internal(format!(
                    "plan '{path}' in repository {repo_id} branch '{branch}' could not be \
                     read: {error}"
                )));
            }
        };
```

`plans.rs:228-231` (#7): same shape, producing `FileNotFound { path }` on the
absent arm and `Internal` otherwise.

`plans.rs:389`: replace the `let Ok(content) = … else` with an explicit `match`
on the same rule; the `else` branch keeps whatever it did for the absent case
and gains an `Internal`/`warn!` arm for the rest.

`plans.rs:629-630` (#26): a `canonicalize` failure on the workdir or the content
root currently answers `RepoNotSynced`. That is right when the path is absent
and wrong otherwise:

```rust
    let canonical_workdir = match workdir.canonicalize() {
        Ok(path) => path,
        Err(error) if io_is_absent(&error) => return Err(not_synced()),
        Err(error) => {
            return Err(DomainError::Internal(format!(
                "repository {repo_id} working directory exists but could not be resolved: {error}"
            )));
        }
    };
```

and the same for `canonical_root`.

`plans.rs:649` (#8):

```rust
    let canonical = match joined.canonicalize() {
        Ok(canonical) => canonical,
        // Only "not there" is `Ok(None)`. An `EACCES` here used to read as
        // "this file does not exist", which the caller turns into a 404 and
        // the exclusivity tier turns into a dropped vote.
        Err(error) if io_is_absent(&error) => return Ok(None),
        Err(error) => {
            return Err(DomainError::Internal(format!(
                "path '{}' could not be resolved: {error}",
                joined.display()
            )));
        }
    };
```

`plans.rs:850`: `std::fs::read_to_string(path).is_ok_and(|c| c.contains("TEST_META"))`
becomes an explicit match that logs and propagates a non-absent failure rather
than answering `false`.

- [ ] **Step 5: Run to verify they pass**

```bash
cargo nextest run -p qa-catalog --lib
make test-qa-catalog-git
```

Expected: PASS on both. The git tier matters here — these paths are what it
exercises.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-catalog
git commit -m "fix(qa-catalog): only ErrorKind::NotFound means the file is absent

Six sites in plans.rs answered every IO failure as 'not there': an EACCES on a
snapshot directory presented as a missing plan (404), and an unreadable test
file silently dropped its exclusivity vote. domain::service::repos already drew
this line correctly at :297 and :336 -- this applies the gear's own rule to the
six sites that did not follow it, named once as io_is_absent so they cannot
drift.

Two of the six were found while validating the review against the rework
(plans.rs:389 and :850) and are not in the original findings.
Review findings #6, #7, #8, #26."
```

---

### Task 12: An unauthorized TEST_META read must not resolve a suite as parallel

**Finding:** #9 (HIGH, `RUST-NO-002`). **This is the highest-consequence fix in
the plan.**

`qa-runs/src/domain/service/launch.rs:2014-2021` — the per-file `TEST_META`
retry ends in `_ => unreadable += 1`. A `Forbidden`, a database failure and an
`Internal` are all counted as "this file has no opinion about exclusivity". The
module's own doc already names the cost (`:1920-1929`, "the fallback cannot tell
'one file is missing' from 'the catalog is down'"), and its *what must never
happen here* paragraph (`:1942-1951`) is about the same hazard from the other
side.

Omitting an unreadable file is correct — the source system does it
(`manager/src/services/exclusivity.rs:411-432`) and it keeps the readable files
voting. Silently omitting an **unauthorized or failed** read is not, because the
resolution it produces is **parallel**: a destructive suite losing its
platform-to-itself guarantee, which is exactly what this module's operator
warning exists for.

**Files:**
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/launch.rs:2005-2022`
- Test: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/launch_tests.rs`

**Interfaces:**
- Consumes: `QaCatalogError`'s variants — read them before writing the match;
  the discriminator you need is "is this a per-file not-found or something
  else".
- Produces: `gather_group_meta` returns `Result<(Vec<FileMeta>, usize), DomainError>`
  instead of `(Vec<FileMeta>, usize)`. `gather_file_meta` changes the same way,
  and its one caller must propagate.

- [ ] **Step 1: Write the failing test**

```rust
/// **A `Forbidden` from the catalog must not resolve an exclusive suite as
/// parallel.**
///
/// The per-file `TEST_META` fallback counted every failure as "unreadable",
/// and an unreadable file casts no vote. So a policy that denies this gear's
/// system actor the catalog read turned a suite whose files declare
/// `exclusive: True` into a **parallel** run -- a destructive test losing its
/// platform-to-itself guarantee, silently, on a deployment whose only fault is
/// a missing grant.
///
/// Omitting a genuinely *missing* file is correct and stays
/// (`a_missing_file_is_omitted_and_the_rest_still_vote` below). This is about
/// the other failures. Review finding #9.
#[tokio::test]
async fn a_forbidden_test_meta_read_fails_the_launch_rather_than_going_parallel() {
    let f = launch_fixture().await;
    f.catalog.deny_test_meta();          // every get_test_meta answers Forbidden
    f.catalog.declare_exclusive("destructive.py");

    let err = f.svc
        .launch(&ctx(f.tenant), exclusive_suite_request(&["destructive.py"]))
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "a denied catalog read must surface, not silently resolve parallel; got {err:?}"
    );
}

/// The half that must not regress: a genuinely missing file is still omitted,
/// and the files that *are* readable still vote.
///
/// Without this, the fix above could be "propagate everything", which would
/// break the source system's behaviour on an input it handles correctly
/// (`manager/src/services/exclusivity.rs:411-432`).
#[tokio::test]
async fn a_missing_file_is_omitted_and_the_rest_still_vote() {
    let f = launch_fixture().await;
    f.catalog.declare_missing("gone.py");
    f.catalog.declare_exclusive("destructive.py");

    let run = f.svc
        .launch(&ctx(f.tenant), exclusive_suite_request(&["gone.py", "destructive.py"]))
        .await
        .unwrap();

    assert!(
        run.resolved_exclusive,
        "the readable file declared exclusive: True, so the suite must be exclusive"
    );
}
```

- [ ] **Step 2: Run to verify the first fails and the second passes**

```bash
cargo nextest run -p qa-runs a_forbidden_test_meta_read_fails_the_launch \
  a_missing_file_is_omitted_and_the_rest_still_vote
```

Expected: the first FAILs (the launch succeeds and resolves parallel), the
second PASSes. **If the second fails, stop** — the fixture is wrong and the
first test's result cannot be trusted.

- [ ] **Step 3: Fix the match arm**

```rust
                for path in &files {
                    let single = self
                        .catalog
                        .get_test_meta(ctx, repo_id, branch, std::slice::from_ref(path))
                        .await;
                    match single {
                        Ok(one) if !one.is_empty() => {
                            metas.extend(one.into_iter().map(file_meta));
                        }
                        // The file is genuinely absent, or the catalog answered
                        // with nothing for it. It contributes no vote and is
                        // counted, never defaulted -- this is the source
                        // system's behaviour (`exclusivity.rs:411-432`) and it
                        // is correct.
                        Ok(_) | Err(QaCatalogError::NotFound { .. }) => unreadable += 1,
                        // Everything else -- a denial, a database failure, an
                        // internal error -- is NOT "this file has no opinion".
                        // Counting it as unreadable resolved a suite whose
                        // files declare `exclusive: True` as **parallel**: a
                        // destructive test losing its platform-to-itself
                        // guarantee because a grant was missing. That is the
                        // dangerous direction, and it fails closed now.
                        // Review finding #9.
                        Err(error) => {
                            warn!(
                                %repo_id, branch = %branch, path = %path, %error,
                                "exclusivity: the TEST_META read failed for a reason other than \
                                 the file being absent; failing the launch rather than resolving \
                                 it parallel",
                            );
                            return Err(error.into());
                        }
                    }
                }
```

**Read `QaCatalogError`'s real variants first** and match the not-found one by
its actual name. If the SDK has no per-file not-found variant, the correct fix
is to add one — a `Result` that cannot distinguish "absent" from "denied" is the
underlying defect, and the review's `RUST-ERR-001` items are the same shape.
Say so in the commit if you take that route.

- [ ] **Step 4: Propagate the signature change**

`gather_group_meta` and `gather_file_meta` now return `Result<_, DomainError>`.
Update their callers — there are two, and the module doc at `:1974-1979`
explains why (the custom-plan path groups by nested plan and cannot iterate
`facts.groups`). Both must propagate, not swallow.

- [ ] **Step 5: Run to verify both pass**

```bash
cargo nextest run -p qa-runs --lib
make test-qa-runs-pg
```

Expected: PASS on both.

- [ ] **Step 6: Update the module doc**

The paragraph at `:1919-1929` says the fallback "cannot tell 'one file is
missing' from 'the catalog is down'". That is now false. Rewrite it to say what
the code does: an absent file is omitted, everything else fails the launch, and
the `1 + N` outage cost is now bounded by the first non-absent error rather than
paid in full.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "fix(qa-runs): a denied TEST_META read must not resolve a suite as parallel

The per-file exclusivity fallback counted every failure as 'unreadable', and an
unreadable file casts no vote -- so a policy that denies this gear's system
actor the catalog read turned a suite whose files declare exclusive: True into
a parallel run. A destructive test losing its platform-to-itself guarantee,
silently, because a grant was missing.

An absent file is still omitted: that is the source system's behaviour on an
input it handles correctly, and the second test pins it. Everything else now
fails the launch closed. Review finding #9."
```

---

# Phase 4 — Lifecycle and resource bounds

Findings #50, #22, #30, #18, #19, #20, #21, #31, #32, #33.

**#50 is first and the ordering is the design decision.** Capping the archive
(#22) before fixing the replay duplication (#50) would truncate duplicated text
instead of stopping the duplication.

---

### Task 13: A watcher re-attach must not re-append the whole log

**Finding:** #50 (HIGH, `Z10-1`).

The chain, each link measured:

1. `domain/service/dispatch.rs:1722` `reattach_watchers` runs on every 5 s tick
   and attaches any live run with a non-null `execution_ref` that is not
   currently watched.
2. `domain/service/watch.rs:470` the registry slot is taken on `attach` and
   released by `AttachedSlot`'s `Drop` (`:289`) when the observer task ends — so
   an observer that ended for *any* reason is re-attached on the next tick.
3. `infra/executor/argo/watch.rs:251` a fresh `Watcher` starts with
   `drained: HashSet::new()`, and `:435-438` opens each pod log with
   `LogParams { container, follow: true, ..default() }` — **no `since_time`, no
   `since_seconds`, no `tail_lines`**. Every pod's log is re-read from byte 0.
4. Each replayed line becomes `ExecutionEvent::Log` →
   `domain/service/ingest.rs:905` `archive.record(...)` →
   `infra/storage/run_logs_sea_repo.rs:138` `CONCAT(text, $1)` with
   `lines = lines + n`.
5. `domain/repos/run_logs_repo.rs:92,107` is the whole trait — `append_log` and
   `get_log`. No truncate, no replace, no offset, so nothing can undo step 4.

Replay is idempotent everywhere else in ingest — `upsert_test_result` replaces
the row, the five counters are tallied from stored rows rather than incremented
(`ingest.rs:552`) — which is why no existing test is red. Three trigger paths,
none exotic: a process restart (which `cpt-cf-qa-nfr-run-duration` *requires*
re-attach for, on 8-hour runs), a transient API-server error
(`argo/watch.rs:286`), and an ingest failure (`watch.rs:443`).

**Files:**
- Modify: `qa-runs/qa-runs/src/domain/ports/run_executor.rs` (the `watch` signature)
- Modify: `qa-runs/qa-runs/src/infra/executor/argo/watch.rs:245-260,430-446`
- Modify: `qa-runs/qa-runs/src/domain/repos/run_logs_repo.rs` (a per-node offset read)
- Modify: `qa-runs/qa-runs/src/infra/storage/run_logs_sea_repo.rs`
- Modify: `qa-runs/qa-runs/src/domain/service/watch.rs` (pass the offset at attach)
- Test: `qa-runs/qa-runs/src/domain/service/watch_tests.rs`

**Interfaces:**
- Produces: `RunExecutor::watch(&self, target: &WatchTarget, resume: LogResume)`
  where `LogResume` carries a per-`(run_id, node)` position. Task 14 (`#22`) and
  Task 15 (`#30`) both build on the archive path this touches; do them in order.

- [ ] **Step 1: Write the failing test**

```rust
/// **A re-attach must not append the run's whole log a second time.**
///
/// `reattach_watchers` runs on every 5 s tick and re-attaches any live run
/// whose observer ended -- a process restart, a transient API-server error, an
/// ingest failure. The Argo watcher opens each pod log with no `since_time`
/// and no `tail_lines`, so it re-reads from byte 0, and `append_log` is a
/// `CONCAT`. `RunLogsRepository` has no truncate, replace or offset, so nothing
/// can undo it.
///
/// Replay is idempotent everywhere else in ingest -- `upsert_test_result`
/// replaces the row, the counters are tallied from stored rows -- which is why
/// nothing else is red. The archive is the only append-only sink in the path.
///
/// `MockRunExecutor` already replays from the beginning deliberately
/// (`dispatch.rs:1771-1778`), so this test needs no new double.
/// Review finding #50.
#[tokio::test]
async fn a_reattach_does_not_duplicate_the_archived_log() {
    let f = watch_fixture().await;
    let run = f.live_run_with_execution_ref().await;

    f.attach_and_drain(run).await;
    let after_first = f.archived_log(run).await;
    assert!(after_first.lines > 0, "the first observation must have archived something");

    // The observer ends; the slot is freed by `AttachedSlot`'s Drop, and the
    // next dispatcher tick re-attaches.
    f.drop_the_slot(run).await;
    f.attach_and_drain(run).await;
    let after_second = f.archived_log(run).await;

    assert_eq!(
        after_second.lines, after_first.lines,
        "a re-attach must not re-append the log; it grew from {} to {} lines",
        after_first.lines, after_second.lines
    );
    assert_eq!(
        after_second.text, after_first.text,
        "a re-attach must not change the archived text"
    );
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p qa-runs a_reattach_does_not_duplicate_the_archived_log
```

Expected: FAIL — `after_second.lines` is roughly double `after_first.lines`.
**This red run is the deliverable's proof; record the actual numbers in the
commit message.**

- [ ] **Step 3: Add the resume position to the repository**

`RunLogsRepository` gains a read that answers where a run's archive currently
ends, per node:

```rust
    /// Where this run's archive currently ends, per node.
    ///
    /// The executor uses it to resume a log read rather than replaying from
    /// byte 0. Answering an empty map is correct for a run that has archived
    /// nothing yet -- the caller then reads from the beginning, which is what
    /// a first attach wants.
    async fn log_resume_positions<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<LogResume, DomainError>;
```

`LogResume` is a domain type — a `BTreeMap<String, LogPosition>` keyed by node,
where `LogPosition` carries the last archived line's timestamp (for
`since_time`) and the count archived for that node (for `tail_lines` when no
timestamp is available). Put it beside `ArchivedLog`.

- [ ] **Step 4: Thread it through the port**

`RunExecutor::watch` takes the resume position. `domain/service/watch.rs`'s
`attach` reads it before spawning and passes it in. In the Argo adapter:

```rust
        let params = LogParams {
            container: Some(MAIN_CONTAINER.to_owned()),
            follow: true,
            // Resume, do not replay. A fresh `Watcher` starts with an empty
            // `drained` set and this used to be `..LogParams::default()`, so
            // every re-attach re-read each pod's log from byte 0 and
            // `append_log`'s CONCAT wrote it all again. Replay is idempotent
            // everywhere else in ingest -- `upsert_test_result` replaces the
            // row, the counters are tallied from stored rows -- and the archive
            // was the one append-only sink. Review finding #50.
            since_time: resume.since_time_for(node),
            tail_lines: resume.tail_lines_for(node),
            ..LogParams::default()
        };
```

`MockRunExecutor` must honour the resume position too — the review notes it
"satisfies [the port] by replaying from the beginning, which is stronger, so
nothing in this crate can falsify an adapter that drops the gap"
(`dispatch.rs:1771-1778`). Keep that property for the *gap* direction while
making the duplication direction falsifiable: the mock should replay from the
resume point, and a second test should assert it still does not drop lines.

- [ ] **Step 5: Run to verify it passes**

```bash
cargo nextest run -p qa-runs --lib
make test-qa-runs-pg
make test-qa-platform-features
```

Expected: PASS on all three. The feature target matters — the Argo adapter's 39
tests are behind `argo` and Task 2 is what makes them run.

- [ ] **Step 6: Update the citation at `dispatch.rs:1771-1778`**

That paragraph currently says the mock replays from the beginning and that
nothing can falsify an adapter dropping the gap. Rewrite it to say what is now
true in both directions.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "fix(qa-runs): resume the log read instead of replaying it

reattach_watchers re-attaches on every 5 s tick any live run whose observer
ended -- a process restart, a transient API-server error, an ingest failure.
The Argo watcher opened each pod log with no since_time and no tail_lines, so
it re-read from byte 0, and append_log is a CONCAT with no truncate, replace or
offset anywhere in RunLogsRepository. cpt-cf-qa-nfr-run-duration *requires*
re-attach on startup for 8-hour runs.

Replay is idempotent everywhere else in ingest -- upsert_test_result replaces
the row, the counters are tallied from stored rows -- which is why nothing was
red. The archive was the only append-only sink in the path.

RunExecutor::watch now carries a per-(run_id, node) resume position.
Review finding #50."
```

---

### Task 14: Cap the per-run archive buffer

**Finding:** #22 (HIGH, `RUST-PERF-001`). **Depends on Task 13.**

`archive.rs:309-319`'s `record()` is uncapped. The module header (`:11-13`)
defends that against design §8's no-cap decision, "bounded in practice by the
flush period". That argument is sound for a log read once; it was not sound
while #50 made a flapping API server re-append the whole log every 5 s. With
Task 13 landed, the cap becomes a bound on a genuinely large run rather than a
truncation of duplicated text — which is why it comes second.

**Files:**
- Modify: `qa-runs/qa-runs/src/infra/logs/archive.rs:1-30` (header), `:309-319`
- Test: `qa-runs/qa-runs/src/infra/logs/archive_tests.rs`

**Interfaces:**
- Consumes: Task 13's resume behaviour (the cap is only correct once replay is
  fixed).
- Produces: no signature change.

- [ ] **Step 1: Write the failing test**

```rust
/// **A single run's pending buffer is bounded.**
///
/// `record` appended without limit between flushes. The module header defends
/// that as design §8's no-cap decision "bounded in practice by the flush
/// period" -- true for a log read once, and it was the second half of #50's
/// growth while a re-attach re-appended the whole log every 5 s. That is fixed;
/// this bounds the remaining case, a genuinely enormous run.
///
/// The front is dropped, not the tail, and a marker records it -- consistent
/// with `infra::logs::broadcast`'s `truncation_marker`. Review finding #22.
#[test]
fn the_pending_buffer_is_capped_per_run() {
    let archive = archive_for_test();
    let run = Uuid::new_v4();
    let tenant = Uuid::new_v4();

    let line = "x".repeat(1024);
    for _ in 0..(MAX_PENDING_BYTES_PER_RUN / 1024 + 64) {
        archive.record(tenant, run, &line);
    }

    let pending = archive.pending_len(run);
    assert!(
        pending <= MAX_PENDING_BYTES_PER_RUN,
        "the pending buffer must stay within {MAX_PENDING_BYTES_PER_RUN} bytes, was {pending}"
    );
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p qa-runs the_pending_buffer_is_capped_per_run
```

Expected: FAIL.

- [ ] **Step 3: Add the cap**

Define it in `archive.rs`, beside `record`:

```rust
/// The most one run's un-flushed buffer may hold.
///
/// The **same budget** as `infra::logs::broadcast`'s
/// `MAX_RETAINED_BYTES_PER_RUN`, and a **separate constant** because it bounds
/// a different buffer: the broadcaster retains what a late SSE reader can still
/// be shown, this holds what has not yet reached the database. They are equal
/// because there is no reason for a run to be allowed more of one than the
/// other, and named apart so changing one does not silently change the other.
const MAX_PENDING_BYTES_PER_RUN: usize = 512 * 1024;
```

Drop from the **front** with a marker, matching `broadcast.rs`'s
`truncation_marker(dropped)` — the head of a long log is the part a reader is
least likely to still need, and the marker is what makes the loss recorded
rather than silent.

- [ ] **Step 4: Run to verify it passes and update the header**

```bash
cargo nextest run -p qa-runs --lib
```

Rewrite `archive.rs:1-13`. It currently argues *against* a cap ("A run emitting
more than that between two flushes would lose its head before it was ever
written, silently"). That objection is real and the fix answers it — the marker
is what makes the loss not silent. Say so.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "fix(qa-runs): bound the per-run pending archive buffer

record() appended without limit between flushes. The module header defended
that as design §8's no-cap decision, bounded in practice by the flush period --
sound for a log read once, and not sound while a re-attach re-appended the whole
log every 5 s. That half is fixed (see the preceding commit), so this bounds
what remains: a genuinely enormous run.

The front is dropped with a marker, same shape and same constant as
infra::logs::broadcast, so the loss is recorded rather than silent -- which is
the objection the old header raised. Review finding #22."
```

---

### Task 15: Truncate a log line before it reaches the broadcaster

**Finding:** #30 (MEDIUM, `RUST-PERF-001`).

`argo/watch.rs:459`'s `emit_line` passes the line through untruncated.
Truncation happens only on the read side, at `api/rest/sse.rs:141`
(`MAX_LINE_BYTES = 8 * 1024`, with character-boundary handling and a marker that
names the cap). So a 2 MB line is carried in full through the broadcaster and
into the archive, and only shrinks when a reader asks for it.

**Files:**
- Modify: `qa-runs/qa-runs/src/infra/executor/argo/watch.rs:454-465`
- Test: the argo watcher's existing test module (runs under `--features argo`)

**Interfaces:**
- Consumes: `crate::api::rest::sse::MAX_LINE_BYTES` (`sse.rs:65`, `8 * 1024`)
  and `sanitize_line` (`sse.rs:~138`), which already cuts on a character
  boundary and appends a marker naming the cap (`sse.rs:141-167`). If
  `sanitize_line` is private to `sse`, promote it to `pub(crate)` rather than
  writing a second one — two truncation rules that must agree is the drift this
  codebase's own conventions warn about.
- Produces: `TRUNCATION_MARKER_MAX` — an upper bound on the marker
  `sanitize_line` appends, so the test below can assert a total length. Derive
  it from that function's own format string rather than guessing a number; if
  the marker's length is already fixed, a `const` beside `MAX_LINE_BYTES` is the
  right home.

- [ ] **Step 1: Write the failing test**

```rust
/// **A pathological log line is truncated before it enters the broadcaster.**
///
/// Truncation lived only on the read side (`api::rest::sse`), so a 2 MB line
/// was carried in full through the broadcaster and into the archive and only
/// shrank when a reader asked. One such line per node is hundreds of megabytes
/// of resident memory for output no reader can ever receive in full.
///
/// The same cap and the same helper as the read side, deliberately: two
/// truncation rules that must agree is a drift this crate has been bitten by.
/// Review finding #30.
#[tokio::test]
async fn a_pathological_line_is_truncated_before_it_is_emitted() {
    let sink = RecordingSink::new();
    let watcher = watcher_with(sink.clone());
    watcher.emit_line(&mut MarkerParser::new(), "node-1", "y".repeat(MAX_LINE_BYTES * 4)).await;

    let emitted = sink.log_lines();
    assert_eq!(emitted.len(), 1);
    assert!(
        emitted[0].len() <= MAX_LINE_BYTES + TRUNCATION_MARKER_MAX,
        "the emitted line must be capped, was {} bytes",
        emitted[0].len()
    );
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p qa-runs --features argo --lib a_pathological_line_is_truncated
```

Expected: FAIL.

- [ ] **Step 3: Truncate in `emit_line`**

- [ ] **Step 4: Run to verify it passes**

```bash
make test-qa-platform-features
```

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "fix(qa-runs): truncate a log line before it reaches the broadcaster

Truncation lived only on the SSE read side, so a 2 MB line was carried in full
through the broadcaster and into the archive and shrank only when a reader
asked. Same cap and same helper as api::rest::sse -- two truncation rules that
must agree is drift this crate has been bitten by. Review finding #30."
```

---

### Task 16: The observer must stop when the gear stops, and must be bounded

**Findings:** #18 (`TOOLKIT-LIFE-001`) and #19 (`RUST-NO-004`), both HIGH.

`domain/service/watch.rs:481` spawns the observer detached with no
`CancellationToken` and no `JoinHandle`. The module's argument for detaching is
correct and stays — `:455-465` explains that binding the task to the caller
would mean a dispatcher pass either awaiting an eight-hour execution or
cancelling the observation when the pass ended, and `cpt-cf-qa-nfr-run-duration`
puts eight hours on the run, not on the tick. What is missing is the third
option: the *gear's* shutdown, which is neither the caller nor the run.

And `max_concurrent_runs` defaults to `0` (`config.rs:514`), so nothing bounds
how many observers exist at once.

**Files:**
- Modify: `qa-runs/qa-runs/src/domain/service/watch.rs:455-490`
- Modify: `qa-runs/qa-runs/src/config.rs:191,514`
- Test: `qa-runs/qa-runs/src/domain/service/watch_tests.rs`

**Interfaces:**
- Consumes: the gear's `CancellationToken`, already threaded to the dispatcher
  ticker in `gear.rs`.
- Produces: `WatchRegistry::attach` takes a `CancellationToken`; the registry
  holds `JoinHandle`s and exposes `shutdown()` awaiting them.

- [ ] **Step 1: Write the failing tests**

```rust
/// **Cancelling the gear ends every live observer.**
///
/// The observer is spawned detached, correctly: binding it to the dispatcher
/// tick would mean a pass either awaiting an eight-hour execution or
/// cancelling the observation when the pass ended, and
/// cpt-cf-qa-nfr-run-duration puts eight hours on the run, not the tick. What
/// was missing is the third lifetime -- the gear's. A dropped runtime took the
/// observers with it, which is a shutdown by process death rather than by
/// design. Review finding #18.
#[tokio::test]
async fn cancelling_the_gear_ends_every_live_observer() {
    let f = watch_fixture().await;
    let cancel = CancellationToken::new();
    f.attach_with(cancel.clone(), f.live_run().await);
    assert_eq!(f.live_observers(), 1);

    cancel.cancel();
    f.registry.shutdown().await;

    assert_eq!(f.live_observers(), 0, "shutdown must join every observer");
}

/// **The number of live observers is bounded.**
///
/// `max_concurrent_runs` defaults to 0, which is not a bound -- one
/// unsupervised spawn per live run, with nothing between the dispatcher and
/// the runtime. Review finding #19.
#[tokio::test]
async fn the_observer_count_is_bounded_by_max_concurrent_runs() {
    let f = watch_fixture_with_limit(2).await;
    for _ in 0..5 {
        f.attach(f.live_run().await);
    }
    assert!(f.live_observers() <= 2, "was {}", f.live_observers());
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo nextest run -p qa-runs cancelling_the_gear_ends_every_live_observer \
  the_observer_count_is_bounded_by_max_concurrent_runs
```

Expected: both FAIL.

- [ ] **Step 3: Thread the token and keep the handles**

```rust
        let executor = Arc::clone(&self.executor);
        let ingest = Arc::clone(&self.ingest);
        let cancel = self.cancel.child_token();
        let handle = tokio::spawn(async move {
            // Moved in so it is dropped -- and the run freed for a later
            // attempt -- however this task ends, including a panic.
            let _slot = slot;
            tokio::select! {
                // The gear's shutdown. This is a third lifetime, distinct from
                // the caller's (which would cancel an 8-hour run at the end of
                // a 5 s tick) and from the run's (which is what the detached
                // spawn is right about). Review finding #18.
                () = cancel.cancelled() => {
                    info!(run_id = %target.run_id, "observation stopping (shutdown)");
                }
                () = drain(executor.as_ref(), ingest.as_ref(), target) => {}
            }
        });
        self.handles.lock().push(handle);
```

- [ ] **Step 4: Change the default and document it**

`config.rs:514`'s `max_concurrent_runs: 0` becomes a real default. Pick it from
what the dispatcher can sustain and **write the reasoning in the field's doc at
`:191`** — this codebase does not accept a bare number. Keep `0` accepted as an
explicit "unbounded" opt-out if any deployment relies on it, and say so.

- [ ] **Step 5: Run to verify they pass**

```bash
cargo nextest run -p qa-runs --lib
make test-qa-runs-pg
```

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "fix(qa-runs): bound the observers and stop them on shutdown

The observer was spawned detached with no CancellationToken and no JoinHandle.
Detaching is right and stays -- binding it to the dispatcher tick would cancel
an 8-hour observation at the end of a 5 s pass. What was missing is the gear's
own lifetime, which is neither the caller's nor the run's: a dropped runtime
took every observer with it, a shutdown by process death rather than by design.

max_concurrent_runs also defaulted to 0, which is not a bound.
Review findings #18 and #19."
```

---

### Task 17: The Argo watcher and its log follow must be cancellable

**Findings:** #20 (`TOOLKIT-LIFE-001`) and #21 (`RUST-ASYNC-001`), both HIGH.

`argo/watch.rs:253` is `tokio::spawn(async move { watcher.run().await })` with
no token and no handle, and `grep CancellationToken argo/watch.rs` is zero hits.
`:401`'s kube log follow has neither a timeout nor a cancel, so a wedged
API-server connection holds the task indefinitely.

**Files:**
- Modify: `qa-runs/qa-runs/src/infra/executor/argo/watch.rs:245-260,395-446`
- Test: the argo test module (`--features argo`)

**Interfaces:**
- Consumes: Task 16's token plumbing — `RunExecutor::watch` already carries a
  resume position from Task 13; add the token beside it rather than inventing a
  second channel.

- [ ] **Step 1: Write the failing tests**

One that cancels a running watcher and asserts `run()` returns; one that points
the follow at a stub server which accepts and never writes, and asserts the
follow gives up within the deadline rather than hanging. `qa-plugin-k8s`'s
`test_support.rs` has a loopback stub API server (`:413,464`) — read it before
writing a new one.

- [ ] **Step 2: Run to verify they fail**

```bash
cargo nextest run -p qa-runs --features argo --lib
```

Expected: the cancel test FAILs; the follow test HANGS (nextest will time it
out — that is the failure).

- [ ] **Step 3: Thread the token into `start()` and `follow()`**

`select!` on the token at both the watcher loop and the log stream, plus a
deadline on the follow. Put the deadline in `ArgoExecutorConfig` with its
reasoning in the field's doc, not as a literal.

- [ ] **Step 4: Run to verify they pass**

```bash
make test-qa-platform-features
```

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "fix(qa-runs): make the Argo watcher and its log follow cancellable

grep CancellationToken over infra/executor/argo/watch.rs was zero hits: the
watcher was spawned with no token and no JoinHandle, and the kube log follow had
neither a timeout nor a cancel, so a wedged API-server connection held the task
indefinitely. Review findings #20 and #21."
```

---

### Task 18: Per-tenant loops must see the shutdown token

**Findings:** #31, #32, #33 (MEDIUM, `TOOLKIT-LIFE-001`).

Each ticker's `select!` already holds a token; the loop *bodies* do not see it,
so a shutdown waits for the whole pass.

- `qa-catalog/src/gear.rs:457` — `run_bundle_gc`'s per-tenant purge loop.
- `qa-environments/src/domain/service/environments.rs:1077` —
  `run_observation_cycle`'s per-environment loop, where each iteration is a
  network round trip to that environment's own cluster. At the NFR's 100
  environments this is the entire shutdown budget.
- `qa-insights/src/gear.rs:1228,1251` — the JIRA and collect tenant loops (Task
  9 already restructured how these obtain their tenant list; this adds the
  cancel check inside).

**Files:**
- Modify: `qa-catalog/qa-catalog/src/gear.rs:440-467`
- Modify: `qa-environments/qa-environments/src/domain/service/environments.rs:998,1077` and `gear.rs:315-355`
- Modify: `qa-insights/qa-insights/src/gear.rs:1220-1270`

**Interfaces:**
- Produces: `run_observation_cycle(&self, cancel: &CancellationToken)` —
  qa-environments' ticker (`gear.rs:343`) is the only caller.
- Consumes: Task 9's restructured tenant reads in qa-insights.

- [ ] **Step 1: Write the failing test (qa-environments, the worst case)**

```rust
/// **A shutdown does not wait for every environment to be observed.**
///
/// `run_observation_cycle` takes no token and its per-environment loop cannot
/// be interrupted. Each iteration is a network round trip to that
/// environment's own cluster, so at cpt-cf-qa-nfr-scale's 100 environments a
/// shutdown waits for 100 round trips -- the whole budget, spent after the
/// operator asked it to stop. The ticker's select! already holds a token; the
/// loop body could not see it. Review finding #32.
#[tokio::test]
async fn a_cancelled_observation_cycle_stops_between_environments() {
    let f = observation_fixture_with(20).await;   // 20 environments
    let cancel = CancellationToken::new();
    f.plugin.cancel_after_first_observation(cancel.clone());

    let report = f.svc.run_observation_cycle(&cancel).await;

    assert!(
        report.attempted < 20,
        "a cancelled cycle must stop early; it attempted all {} of them",
        report.attempted
    );
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p qa-environments a_cancelled_observation_cycle_stops_between_environments
```

Expected: FAIL — `attempted` is 20.

- [ ] **Step 3: Add the check to all four loops**

In each, at the top of the loop body:

```rust
        if cancel.is_cancelled() {
            info!(
                attempted = report.attempted,
                "qa-environments observation cycle stopping early (shutdown)"
            );
            return report;
        }
```

Return the partial report rather than an error — a cancelled cycle did the work
it did, and the next process start re-runs it.

- [ ] **Step 4: Run to verify all four gears pass**

```bash
cargo nextest run -p qa-catalog -p qa-environments -p qa-insights --lib
```

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform
git commit -m "fix(qa-platform): let per-tenant loops see the shutdown token

Each ticker's select! already held a token and none of the loop bodies could
see it, so a shutdown waited for the whole pass. qa-environments is the worst
case: each iteration is a network round trip to that environment's own cluster,
so at cpt-cf-qa-nfr-scale's 100 environments a shutdown waited for 100 round
trips after the operator asked it to stop.

A cancelled cycle returns its partial report rather than an error -- it did the
work it did, and the next process start re-runs it.
Review findings #31, #32, #33."
```

---

## Phase completion

After Task 18:

```bash
make fmt clippy
make test-no-macros
make test-qa-runs-pg test-qa-insights-pg test-qa-catalog-git test-qa-platform-features
make helm-tests
make ui-lint ui-test ui-build
```

All must be green. Then proceed to `2026-09-05-review-remediation-quality.md`.
