# Paste this into a new session

Execute the plan at
`gears/qa-platform/docs/superpowers/plans/2026-08-31-revert-system-gear-changes.md`
using the **superpowers:subagent-driven-development** skill: a fresh subagent per
task, with a review between tasks.

## What this is

qa-platform modified two shared system gears. Both must be reverted to their
exact upstream state, with whatever qa-platform needed absorbed into
qa-platform's own code. The user has confirmed the rest of the footprint (new
crates, Cargo registration, CI, tooling) is acceptable and out of scope.

* **The plan** — the file above. 8 tasks, two independent phases.
* **The spec it argues from** — `gears/qa-platform/docs/FOOTPRINT-OUTSIDE-QA-PLATFORM.md`,
  sections 2.1 (authz) and 2.2 (event broker). Read it before Task 1.

Both files are **untracked**; commit them with the work (Task 8 Step 6 already
updates the footprint doc).

## Repository state

* Repo `/home/serhii/Jelastic/projects/fabric/gears-rust`, branch
  `feature/qa-platform-specs`.
* HEAD **was** `925240eb7` — a single squashed commit containing all qa-platform
  work — when this prompt was first written. As of 2026-09-01, Tasks 1-7 have
  landed on top of it (see "Where the session stopped" below); HEAD is now
  `c1eb7439a`. Check `git log --oneline -15` rather than trusting this line if
  time has passed since.
* Upstream base for every "revert to" comparison is **`db7660030`**.
* `pre-squash-backup-20260831` holds the pre-squash 312-commit history. Leave it
  alone.
* Working tree is otherwise clean.

## The acceptance test for the whole plan

```bash
git diff --stat db7660030 -- gears/system/authz-resolver gears/system/event-broker
```

Must print **nothing**. Anything else means the job is not done.

## Verification baselines — these must not regress

| Crate | Tests passing now |
|---|---|
| qa-runs | 890 |
| qa-environments | 173 |
| qa-insights | 716 + 4 |
| qa-catalog | 219 |
| qa-platform-ui (`npx vitest run`) | 211 |

Clippy is clean at `-D warnings` on all four gears. `npx tsc --noEmit` is clean.

Test counts **will drop** in Tasks 5 and 6 — deleted code takes its tests with
it. The plan requires diffing the test-name lists to prove every disappearance
belongs to a deleted file. A test vanishing from anywhere else means logic was
deleted along with plumbing.

## Standing rules — each of these has cost a session here

* **Never pipe a command whose exit code you need.** `cmd | tail` reports
  `tail`'s status. Redirect to a file and read `$?`. This has produced false
  green reports more than once — including a deploy that printed `exit=1` while
  the harness notification said 0. **The log is the authority, not the exit code
  and not the notification.**
* **Check the whole build, not one crate.** `cargo check -p <gear>` passing means
  nothing about the composed binary. Before any deploy:

  ```bash
  FEATS="qa-platform,oidc-authn,static-authz,tenant-resolver-rg,static-credstore,postgres-credstore,platform-observation,qa-runs-argo"
  cargo check -p cf-gears-example-server --features "$FEATS"
  ```

  A recent deploy died 7 minutes into the remote Rust build because an SDK
  change broke a *consumer* gear that a single-crate check never compiled.
* **Never edit a file a running process is reading.** Bash reads scripts
  incrementally; editing a running deploy script corrupts it mid-execution and
  the exit code afterwards is worthless.
* **Break-test every guard you add.** Mutate the code so the new test *should*
  fail and confirm it does. This caught a false claim in a test comment during
  the session that produced this plan — the assertion could not fail, and the
  comment claimed it pinned behaviour it did not.
* **No kubeconfig-derived value is ever formatted** into a message, log line or
  DTO. A measured leak once put a private key on the platform page.
* **ADR-0001:** no `kube` / `k8s-openapi` outside
  `qa-environments/src/infra/observer/`, never in `domain/`, gated behind the
  `platform-observation` feature.
* Toolchain: `export PATH="$HOME/.cargo/bin:$PATH"`. Never
  `cargo test --all-targets` at workspace level.
* `npm run lint` is broken repo-wide (no `eslint.config.js`) and
  `cargo fmt --check` is red on many untouched files. Both pre-existing — not
  findings.
* **Do not push to `origin`** (constructorfabric). Only the `fork` remote, via
  `./push-to-fork.sh`. That script deliberately never force-pushes.

## Deploying (Task 8 only)

```bash
cd gears/qa-platform
./deploy/remote/deploy-k8s.sh --target root@10.136.20.200 \
    --public-origin https://10.136.20.200 > /tmp/deploy.txt 2>&1; echo "exit=$?"
```

* Needs the **VPN**. If `10.136.20.200` times out that is the tunnel, not the code.
* **~20 minutes** — a change anywhere, including UI-only, invalidates the Docker
  `COPY` layer and forces a full cargo rebuild.
* Read `/tmp/deploy.txt`. Expect `51 PASS, 0 FAIL` and
  `VERIFY-K8S: every check above passed individually`.

## Two things in the plan most likely to be shortcut

1. **`for_bundle_gc` in Task 3 is the only write** among the elevated sites. It
   must be split into an elevated *read* plus tenant-bound deletes. If that
   proves awkward, stop and ask — do not elevate the delete.
2. **Keep `m20260818_000002_offset_store.rs` in Task 6.** The table already
   exists on the deployed database and the runner records the migration as
   applied; deleting the file makes the code disagree with the schema.

---

## Where the session stopped (updated 2026-09-01, final)

**The plan is code-complete.** 20 commits on `feature/qa-platform-specs`, `925240eb7..a8a1efd62`.
Tasks 1-7 are done and reviewed; Task 8 is done except its dev-stand verification.

### The acceptance test passes

```
git diff --stat db7660030 -- gears/system/authz-resolver gears/system/event-broker
```

produces **zero bytes**, and `git status --short gears/system/` is clean. Both shared gears are
byte-for-byte upstream again.

### What remains — Task 8, Steps 3, 4 and 5

The deploy was **blocked by the permission classifier**, not by infrastructure: the stand at
`10.136.20.200` answered ssh during the session, so the VPN was up. Nothing in this branch has
been exercised against a running system. Run the deploy in the "Deploying" section above, then:

* **Step 4** — launch a run, let it finish, wait one `reconcile_interval_seconds` (300s), and
  confirm the reconcile sweep ingested it automatically. Non-zero row count is the pass.
* **Step 5** — check the gears log for authz denials on ticker paths; expect none.

### The one deliberate deferral, and it belongs to the deploy

`config/qa-platform.yaml` (repo **root**) still ships `dispatcher_enabled: false`,
`scheduler_enabled: false` and `branch_refresh_interval_seconds: 0`. Its prose was corrected —
it no longer claims the PEP denies these tickers, because it no longer does — but the switch
values were deliberately left alone: enabling tickers that have never run in this configuration
is a behavioural change, and the deploy that would verify it was blocked. **Flipping those three
switches and proving them on the stand is the real Step 4.** That is the plan's whole win, still
un-shipped.

### Current verification baselines (these replace the table above)

| Crate | Tests |
|---|---|
| qa-runs | 854 |
| qa-environments | 173 |
| qa-insights | 705 |
| qa-catalog | 228 |
| qa-platform-ui (`npx vitest run`) | 211 |

Counts in qa-runs and qa-insights are below their pre-plan figures because deleted code took its
own tests with it; every disappearance was diffed against the deleted files and accounted for.
Clippy `-D warnings` is clean on all four gears, `npx tsc --noEmit` is clean, the composed
`cf-gears-example-server` build passes with the full feature set, and the helm guard passes.

### Two accepted residuals, recorded rather than fixed

* The `elevated.rs` exemption in all three one-seam guards is **whole-file**, so a second
  `allow_all()` added to `elevated.rs` outside the seam function would pass. Verified by
  injection. It matches the pre-existing qa-runs precedent, so this is not a regression.
* `qa-catalog/src/domain/system_actor.rs` `for_bundle_delete` accepts a nil `Uuid`, where qa-runs
  refuses it by type and qa-insights filters it. The enumeration boundary now filters nil, and the
  path fails closed regardless.

### One thing worth fixing outside this plan

The drift guard `deploy/helm/tests/test_no_system_gear_changes.py` **is not wired into CI**.
`pytest tests/` in that directory collects only that one test, and no workflow or Makefile
references the directory at all. It protects manual re-runs, not automated ones — so nothing
currently stops a future PR from reintroducing the drift this plan just removed. Wiring it was
left out deliberately: changing shared CI is exactly the class of footprint this plan exists to
reduce, and the spec marks `ci.yml` out of scope. Its `UPSTREAM_BASE = "db7660030"` pin is now
documented in the file, including what to do when upstream legitimately moves.
