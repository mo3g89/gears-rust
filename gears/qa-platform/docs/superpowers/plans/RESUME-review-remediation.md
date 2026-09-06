# Resume prompt — QA Platform review remediation

Paste the **Prompt to start the new session** section below into a fresh Claude Code
session opened at `/home/serhii/Jelastic/projects/fabric/gears-rust`. Everything
else in this file is context that prompt refers to.

Written 2026-09-06, after Task 9. Delete this file when the plan is finished.

---

## Prompt to start the new session

> Resume executing an implementation plan that is already 9 tasks in.
>
> Read these three files first, in this order:
>
> 1. `.superpowers/sdd/2026-09-05-review-remediation-core/progress.md` — the
>    ledger. It is the authority on what is done and every ruling made so far.
>    Trust it and `git log` over any assumption.
> 2. `gears/qa-platform/docs/superpowers/plans/2026-09-05-review-remediation-core.md`
>    — the plan being executed. Tasks 1–9 are complete; Tasks 10–18 remain.
> 3. `gears/qa-platform/docs/superpowers/specs/2026-09-05-review-remediation-design.md`
>    — the spec the plan argues from. It is the binding authority when the plan
>    and a finding disagree.
>
> Then invoke the `superpowers:subagent-driven-development` skill and continue
> from **Task 9's review**, which was packaged but never dispatched — see
> "Immediate next action" in
> `gears/qa-platform/docs/superpowers/plans/RESUME-review-remediation.md`.
>
> Work continuously, one task at a time, without checking in between tasks.
> Only the four stop-conditions in that skill (irreversible/destructive
> operation, security-sensitive action, outward-facing side effect, or a plan so
> broken every path is a guess) should interrupt you.

---

## Immediate next action

**Task 9 is implemented and committed but NOT reviewed.** Its review package was
generated and the reviewer was never dispatched — a controller error in the
previous session, not a failure of the work.

Nothing needs regenerating. Dispatch a task reviewer with:

- Brief: `.superpowers/sdd/2026-09-05-review-remediation-core/task-9-brief.md`
- Report: `.superpowers/sdd/2026-09-05-review-remediation-core/task-9-report.md`
- Diff: `.superpowers/sdd/2026-09-05-review-remediation-core/review-3c4fbba72..7675aa94a.diff`

Tell that reviewer three things it cannot infer:

1. **#56 is not a defect.** It was raised during the previous session's own
   validation pass and its premise did not survive contact with the code:
   `tenants_for` (`qa-insights/src/gear.rs:1085-1100`) already warns
   unconditionally, and nothing follows either ticker loop, so
   `unwrap_or_default()` and an explicit match are behaviourally identical. The
   change was kept as a readability/robustness improvement and the commit
   message was amended to say so. Do not let the reviewer re-inflate it into a
   bug fix, and do not let it flag the change as unnecessary either — the ruling
   is recorded in the ledger.
2. **#28's test cannot be red/green.** The fix adds a log line and does not
   change the return value; qa-insights has no log-capture harness and the
   implementer was told not to invent one. It verified this by stashing the fix
   and confirming the test passes either way, and documented it. That is honest,
   not a defect.
3. **#29 is unreachable at its call site** (`ObservedAttrs` is
   `BTreeMap<String, String>`, which cannot fail to serialize). The implementer
   extracted an `attrs_or_skip` helper taking a `Result` so the skip path could
   be tested with a real `serde_json::Error` — better than the brief asked for.

Then proceed to Task 10.

---

## State as of this file

- **Branch:** `feature/qa-review-remediation`, off `feature/qa-product-plugins`
- **HEAD:** `7675aa94a`
- **Working tree:** clean
- **Nothing is running.** No background agents, no pending work.

### Commits so far

| Commit | Task | What |
|---|---|---|
| `3e522d291` | — | design doc |
| `fde4b1f53` | — | the four plan documents |
| `995d1924f` | 1 | `test-qa-insights-pg` + `test-qa-catalog-git` wired into CI |
| `aaadaf09c` | — | plan-text count corrections |
| `c60c5143c` | 2 | `test-qa-platform-features` (argo, runner-secret) |
| `71c387ace` | — | `argo_cluster.rs` is 4 ignored tests, not 5 |
| `5f7bfac9c` | 3 | UI CI gate (`ui` filter + job + CodeQL) |
| `a8adcc7ee` | — | UI lint gap tracked in DECOMPOSITION 2.6 |
| `1fb04ca25` | 4 | Helm chart guards wired into the `lint` job |
| `798f3c6ce` | — | plan's Task 4 recipe + red-run proof corrected |
| `2ff057921` | 5 | `credential_ref` off `TestRepositoryDto`; `has_credential` added |
| `d5a92963c` | 6 | PDP compile failure split into a deny and a fault, ×4 gears |
| `1f5260c04` | 7 | public collect endpoint driven (4 tests) |
| `3c4fbba72` | 8 | notification + saved-view refusal attribution pinned |
| `7675aa94a` | 9 | three swallowed failures (#28, #29, #56) |

### Task status

- **Tasks 1–8:** complete, reviewed clean.
- **Task 9:** implemented and committed; **review outstanding**.
- **Tasks 10–18:** not started. Task 10's brief is already extracted at
  `.superpowers/sdd/2026-09-05-review-remediation-core/task-10-brief.md`.

---

## Things the next session must not rediscover the hard way

**Environment.** Every cargo dispatch needs
`export PATH="$HOME/.cargo/bin:$PATH"` first. The system `/usr/bin/cargo` is
rustc 1.75.0; this repo needs 1.97.0 and nextest. This cost time in Task 1.

**Task 10 has a dependency the plan does not mention.**
`deploy/helm/tests/test_nginx_template.sh` asserts `default.conf.template`
renders **byte-identically** to `deploy/helm/tests/fixtures/nginx.conf.baseline`.
Adding the `log_format` + `access_log` that Task 10 requires will fail that guard
unless the baseline is regenerated in the same commit. **Ruling already made:
regenerate it, and say so in the commit message** — it is a snapshot test, and a
deliberate reviewed change is when a snapshot moves. `envsubst` is called with an
explicit variable list (`${NGINX_RESOLVER} ${GEARS_UPSTREAM}`), so the new
format's `$remote_addr`/`$status`/`$body_bytes_sent` survive untouched, and the
script already asserts `$uri` survives.

**Tasks 13, 15, 16, 17 all edit `qa-runs`' watch path, and the plan's code
snippets for 16 and 17 were written against the pre-Task-13 tree.** Task 13 adds
a `resume` parameter to `RunExecutor::watch`. An implementer copying Task 16's or
17's snippet verbatim would drop it and silently reintroduce #50. **Ruling
already made: every dispatch for 15/16/17 must tell the implementer to read the
current file rather than trust the plan's snippet or its line numbers.**

**Task 18 depends on Task 9.** Task 9 restructured the qa-insights ticker loops,
so Task 18's cancel check goes inside the loop Task 9 left, not the one the plan
quotes.

**Task 12 (#9) is the highest-consequence change in the plan.** A denied
`TEST_META` read currently resolves a suite declaring `exclusive: True` as
**parallel** — a destructive test losing its platform-to-itself guarantee because
a grant is missing. The fix changes `gather_group_meta`'s return type and needs
`QaCatalogError` to distinguish "file absent" from "denied". If the SDK has no
such variant, the honest fix is adding one, which widens the task — the plan says
to stop and report rather than guess.

**Task 6 left a note for Task 12.** `launch_tests.rs:1972-1974`'s comment is
accurate but does not name the variant it describes. Clarify it in passing if
`launch.rs` is open anyway; do not make a separate commit for it.

---

## Deferred items (for the final whole-branch review to triage)

- **`helm-tests` has no pytest/pyyaml precondition check** where the Makefile's
  own convention (20 lines above, `test-qa-catalog-git`'s `git` check) has one.
  Stylistic. Left unfixed deliberately: it is build-recipe logic, and controller
  edits to build logic skip review.
- **The UI has no lint gate.** `make ui-lint` has never worked — no
  `eslint.config.js` has ever been committed, the project is on `eslint ^9.17.0`
  which requires flat config, and the lint script still passes the removed
  `--ext` flag. Measured scope: 15 errors + 1 warning across 12 of 139 files.
  Recorded in `gears/qa-platform/docs/DECOMPOSITION.md` §2.6 under "Tracked
  follow-ups"; the `ui` CI job runs `make ui-test ui-build` only.

## Known repo issues found in passing, deliberately not fixed

- **`actions/setup-python@82c7e631…` is commented `# v5.1.1` but is actually
  tag v5.1.0** — `.github/workflows/e2e.yml:50` and `cfs.yml:46`. The pin itself
  is a genuine commit from the genuine repo, so this is a stale comment, not a
  security issue. Out of scope: no task in this plan touches those workflows.
- **`qa.notification_config` (PEP) vs `cf.qa.insights.notification.v1~` (REST
  gts_id)** — every other resource in qa-insights aligns between the two; this
  one drops `_config`. Not renamed because the gts_id is the RFC-9457 `type`
  clients match on, i.e. a wire contract, and finding #24 only asked for a test.
  Worth revisiting if nothing consumes that string yet — it never gets cheaper.

---

## Corrections to the record

Errors made in the previous session, all already fixed in the commits above.
Listed so the next session does not re-derive them or trust the originals:

| Where | Claimed | Actual |
|---|---|---|
| Plan, Task 1 | qa-insights tier is 13 tests | **9** — counted `cfg` attribute sites, not test fns |
| Plan, Task 1 | flag is `--test` | **`--tests`** — `--test` is singular and takes one target name |
| Review #48 | `argo_cluster.rs`'s 5 ignored | **4** — the 5th grep hit is the file header's prose |
| Review #48 | 33 tests under `executor::argo` | **39** |
| Review #48 | 24 under `qa-environments::infra::observer` | module no longer exists; code is in `qa-plugin-k8s`, 169 tests, not feature-gated |
| Plan, Task 4 | `pytest tests/ -q` runs the guards | collects **1 of 5** — four are `main()` scripts |
| Plan, Task 4 | change an image tag → `test_pins.py` fails | `test_pins.py` is about login origin pins; no guard asserts on image tags |
| Plan, Task 6 | `Denied { reason: String }`, `CompileFailed(String)` | `Denied { deny_reason: Option<DenyReason> }`, `CompileFailed(ConstraintCompileError)` |
| Plan, Task 8 | gts id `cf.qa.insights.notification_config.v1~` | `cf.qa.insights.notification.v1~` |
| Finding #56 | tickers "stop silently" | they stop, but `tenants_for` already warns — **not a defect** |

---

## Rulings made so far

Full text with reasoning and cost-if-wrong is in the ledger; this is the index.

1. Later implementers read the current file, not the plan's snippet (5 file overlaps).
2. Task 7 is a coverage task; green-first is not a failed TDD cycle.
3. Fixed my own doc/comment errors directly rather than deferring (Tasks 1, 2).
4. Dropped `ui-lint` from the CI job rather than shipping a job red on every PR.
5. Recorded the UI lint gap in DECOMPOSITION rather than in the soon-to-be-deleted ledger.
6. Accepted Task 4's two deviations — the plan's recipe and its red-run proof were both wrong.
7. Added `has_credential: bool` rather than leaving the Auth column misreporting.
8. **Split `CompileFailed` into a deny (403) and a fault (500)** rather than the
   review's blanket 500 — `ConstraintsRequiredButAbsent` is documented by the SDK
   as a deny. This is the largest departure from the review so far.
9. Task 8 asserts the actual gts_id; the naming asymmetry is deferred, not fixed.
10. Task 10 regenerates the nginx baseline in the same commit.
11. Kept Task 9's #56 change but amended the commit message to stop calling it a bug fix.
