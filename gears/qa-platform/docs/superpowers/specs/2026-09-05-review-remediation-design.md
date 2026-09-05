# QA Platform review remediation — design

Date: 2026-09-05
Gears: `qa-catalog`, `qa-environments`, `qa-insights`, `qa-runs`, `qa-platform-ui`,
plus `gears/credstore/plugins/postgres-credstore-plugin` and repo-level CI
Status: approved for planning
Source: `docs/Reviews/qa-platform-review-findings.md` (55 findings, filed against
`feature/qa-platform-specs` @ `3c14f775`)
Target: `feature/qa-product-plugins` @ `a1767401f`

---

## 1. Context

### What this document is

The review was written against `feature/qa-platform-specs`. That branch was then
reworked into `feature/qa-product-plugins`, which moved all product-specific
knowledge behind a plugin boundary — 355 files changed, +41364 / −12441 between
the two branch heads. The rework renamed the `TargetPlatform` aggregate to
`Environment`, moved each gear's `local_client` from `infra/` to `domain/`, and
added three crates that did not exist when the review was written
(`qa-product-sdk`, `plugins/qa-plugin-k8s`, `plugins/qa-vhp-product-plugin`).

So the review's file paths and line numbers can no longer be trusted on their
face, and two questions had to be answered before any fix could be planned:

1. Which findings did the rework resolve, and which did it merely relocate?
2. Did the rework reproduce any of the same defects in the new plugin layer?

Section 2 answers both, measured. Sections 3–11 are the nine phases the
remediation is broken into. Section 12 records what this design deliberately
does not do.

### Validation method

Every finding was re-checked against the working tree at `a1767401f` by reading
the cited construct, not by matching the cited line number. Where the rework
renamed or moved a file, the new location is recorded. Where a finding's
rationale was answered by a doc comment the rework added, the argument was read
and judged rather than accepted. Four such arguments were weighed: #5's is
accepted for the reconciler and rejected for the JIRA poller (§2.2); #22's is
accepted only once #50 is fixed (§6); #55's is rejected (§8); #42's is accepted
as written (§11).

---

## 2. Validation results

### 2.1 Resolved by the rework (2)

**#13 — domain health types serde into an unversioned `cluster_nodes` blob.**
Gone. The typed blob is replaced by `qa_product_sdk::ObservedAttrs`, which is
`pub struct ObservedAttrs(BTreeMap<String, String>)`
(`qa-product-sdk/src/observation.rs:16`). What reaches the column is filtered
through the plugin's own `observed_schema()` — attributes the plugin does not
declare are dropped before the write
(`qa-environments/src/domain/observation_write.rs:42-45`). There is no domain
type on the serde path any more, so the finding's defect does not exist.
Residual, not a finding: the schema itself carries no version, so a plugin that
changes an attribute's meaning without renaming it is still undetectable. Noted
for the plugin spec, not fixed here.

**#36 — `create/update_product` take four positional `String`s.**
Gone. `qa-catalog-sdk/src/client.rs:178,188` now take `NewProduct` and
`ProductUpdate`.

### 2.2 Partially addressed — the defect survives (3)

**#10 — `ExclusiveFlag = Option<bool>`.** The rework introduced the name and
stopped there: `qa-catalog-sdk/src/models.rs:60` is
`pub type ExclusiveFlag = Option<bool>;`. An alias gives a reader a word for the
three states; it gives the compiler nothing. `Inherit` and `Shared` are still
the same type, and `ExclusiveFlag::default()` is still `None`. Fixed in Phase 5
together with #11.

**#48 — feature-gated tests CI never runs.** Half resolved, by accident of the
rework rather than by intent. The `platform-observation` feature is gone; the
kube observer moved into `plugins/qa-plugin-k8s`, which carries no feature gate
and is an unconditional workspace member, so its 169 tests — including the
kubeconfig-leak guards in `errors.rs` that the review singles out — now run
under `cargo nextest run --workspace`. The `argo` half is untouched: 39 tests
under `qa-runs/src/infra/executor/argo/` are still behind
`#[cfg(feature = "argo")]` (`infra/executor/mod.rs:25`), and 13 more behind
`runner-secret` (`qa-environments/src/infra/mod.rs:14`). Both features ship —
`deploy/cargo-features.argo` names `runner-secret,qa-runs-argo`. Fixed in
Phase 1, narrowed to those two features.

**#5 — `NoopLeaderElector`.** The rework added a long argument
(`qa-insights/src/infra/leader/mod.rs:40-60`) that election here is an
optimisation, not a correctness requirement. **For `ROLE_RECONCILER` that
argument is correct and this design accepts it**: `upsert_run_results` is
delete-then-insert per run and `WatermarkRepository::advance` never moves a mark
backwards, so two concurrent sweeps converge. **It does not cover
`ROLE_JIRA_POLLER`**, whose effect is `RunsLauncher::launch_test` — a new run,
not an idempotent write. Two replicas polling the same resolved bug launch it
twice. What prevents that today is `replicaCount: 1`
(`deploy/helm/qa-platform/values.yaml:101`) and nothing else. Handled in Phase 9
as a decision with a guard, not as a rewrite.

### 2.3 Relocated by the rework — same defect, new path (7)

| # | Review path | Path at `a1767401f` |
|---|---|---|
| 15 | `qa-runs/.../local_client/client.rs:36` | `qa-runs/src/domain/local_client/client.rs:36` |
| 16 | `qa-insights/.../local_client/client.rs:73` | `qa-insights/src/domain/local_client/client.rs:73` |
| 29 | `platforms_sea_repo.rs:458` | `qa-environments/src/infra/storage/environments_sea_repo.rs:419` |
| 32 | `platforms.rs:920` | `qa-environments/src/domain/service/environments.rs:1077` |
| 39 | `qa-runs/.../local_client/client.rs:37` | `qa-runs/src/domain/local_client/client.rs:37` |
| 54 | `handlers/platforms.rs:237` | `qa-environments/src/api/rest/handlers/environments.rs:254` |
| 55 | `platforms_sea_repo.rs:44`, `/platforms` | `environments_sea_repo.rs:56`, `GET /qa/v1/environments` |

#54 is worth stating in full because the rename makes it read as fixed when it
is not: the paragraph explaining why `observe_environment` must authorize with
`UPDATE` still sits above `fn environment_for_product` (`:263`), that helper's
own one-line doc is still appended as the paragraph's last line, and
`refresh_environment_authorizes_with_update_not_get` (`:359`) still has no doc.

### 2.4 Still valid at or near the cited line (43)

Every HIGH is in this group. Spot-verified at identical line numbers:
`plans.rs:183,230,649` (#6/#7/#8), `argo/watch.rs:253,435` (#20/#21/#50),
`watch.rs:481` (#18/#19), `dto.rs:36` (#2), `error.rs:367` (#24),
`Makefile`/`ci.yml`/`codeql.yml` (#47/#49/#52), `SettingsPage.tsx` (#53).

**Total live: 53 of 55.**

### 2.5 What the rework widened, and what it added

The plugin layer is clean on the classes this review covers. Secrets travel as
`credstore_sdk::SecretValue`, and `RunAccess` deliberately refuses `Clone` so a
plaintext credential cannot be duplicated in memory
(`qa-product-sdk/src/access.rs:33`). Kube errors are classified into a
`PluginFailure` before they can cross a boundary
(`qa-plugin-k8s/src/kube_client.rs:213-217`). `PluginRegistry::plugin_for` is
PEP-gated on `PRODUCT`/`GET` with the product read scoped
(`qa-catalog/src/domain/service/plugin_registry.rs:188-197`). No new
cross-tenant path, no new secret on the wire.

Four things the rework nonetheless changed about the existing findings:

1. **#1 got bigger and gained a trap.** There are now 18 `qa.*` resource types
   and ~15 actions across the four gears, and still no `AuthzPermissionV1`
   anywhere in `gears/qa-platform`. The trap: the rename kept the PDP string as
   `"qa.platform"` on purpose, because changing it would silently change who is
   authorized for what (`qa-environments/src/domain/service/mod.rs:92-99`). A
   catalog generated from the *aggregate* name rather than from the
   `resources::*` const would emit `qa.environment` and grant nothing.

2. **#3's blast radius grew.** `plugin_for` resolves through the same
   `From<EnforcerError> for DomainError`, so a PDP compile fault during plugin
   resolution now also answers 403 — "you may not use this product's plugin"
   for what is a server fault.

3. **Two more sites in #6–#8's class**, not in the review:
   `qa-catalog/src/domain/service/plans.rs:389`
   (`let Ok(content) = std::fs::read_to_string(&resolved) else`) and `:850`
   (`std::fs::read_to_string(path).is_ok_and(...)`). Both treat any IO failure
   as "file absent".

4. **One new finding of the same shape.**
   `qa-insights/src/gear.rs:1228,1251` — `tenants_for(...).unwrap_or_default()`.
   A failed tenant-directory read becomes an empty tenant list, so the JIRA
   poller and the collect cycle log nothing and do nothing. A gear whose
   background work has silently stopped looks identical to one with no tenants.
   Filed as **#56** and fixed in Phase 2.

The three new crates inherit #4 (no metrics) and nothing else.

---

## 3. Phase 1 — Verification wiring

**Findings:** #47, #48 (argo + `runner-secret` only), #49, #52.

**Why first.** Every finding in sections 4–11 is a behaviour change to code that
ships. Four tiers of tests exist to falsify exactly this kind of change and no
target and no job invokes them. Landing the fixes first means landing them
against a CI that cannot contradict them.

**Work:**

* `test-qa-insights-pg` and `test-qa-catalog-git` in the `Makefile` beside
  `test-qa-runs-pg` (`Makefile:564`), both added to the `integration` job
  (`.github/workflows/ci.yml:331`) and to `ci:` (`Makefile:935`). This is 9
  tests in qa-insights — five of them the Postgres dialect guards over the
  analytics `GROUP BY` reads — and 6 in qa-catalog, the only coverage of the
  real git transport and the two-tier locks.
* `test-qa-platform-features`: `--features argo` and `--features runner-secret`,
  unit tier only. `qa-runs/tests/argo_cluster.rs`'s 5 are `#[ignore]`d and want
  a live cluster; they stay out.
* A `ui` output on `ci.yml`'s `changes` filter matching
  `gears/qa-platform/qa-platform-ui/**`, and a job running
  `make ui-lint ui-test ui-build`. Today a UI-only PR sets every filter output
  false and skips `clippy`, `test` and `integration` entirely.
* `javascript-typescript` added to the CodeQL matrix
  (`codeql.yml:70-77`, where it sits commented out at `:78-81`).
* A `helm-tests` target running `deploy/helm/tests/`' five pytest modules and
  `test_nginx_template.sh`, in the `lint` job — they need no cluster.

**Verification.** Each gate is proven by making it fail: delete a `group_by`
line and watch `test-qa-insights-pg` go red; break a type in a `.tsx` and open a
UI-only PR. A gate that has never failed has not been shown to be a gate.

---

## 4. Phase 2 — Security and fail-closed correctness

**Findings:** #2, #3, #23, #24, #28, #29, #40, #51, #56.

**#2 — `credential_ref` on the read DTO.** Omit it from `TestRepositoryDto`
(`qa-catalog/src/api/rest/dto.rs:36`) and from the `From<sdk::TestRepository>`
impl. It stays on `CreateTestRepositoryReq` / `UpdateTestRepositoryReq`, which
is where a caller supplies it. This is not a new convention: `SshKeyDto`
(`dto.rs:528-529`) and `PlatformDto`
(`qa-environments/src/api/rest/dto.rs:107`) both drop their credstore ref with a
comment saying not to add it back. `TestRepositoryDto` is the outlier.

**#3 — `CompileFailed` → 403.** In all four gears' `domain/error.rs`
(`qa-catalog:122`, `qa-environments:76`, `qa-insights:338`, `qa-runs:408`), map
`EnforcerError::CompileFailed` to `Internal` and leave `Forbidden` for `Denied`
alone. Behaviour is unchanged in the direction that matters — the request still
fails closed — and the status stops telling an operator that a scope that will
not compile is a permission they lack. `log_enforcer_error` already logs the two
at different levels, so the classification exists; only the mapping is wrong.

**#23 — the public collect handler is never driven.**
`qa-insights/src/api/rest/handlers/collect.rs` has two unit tests and both are
about query-string decoding. Add handler tests for the three paths that matter:
nil tenant → 400; bad signature → 403 **and no write**; correctly signed →
happy path. The verify itself is sound (`domain/service/collect.rs:654-665`,
constant-time, fail-closed on an empty secret, system actor minted after the
check) — what is missing is anything that would notice if it stopped being.

**#24, #40 — two missing error-attribution tests.** `as_notification_error`
(`api/rest/error.rs:367`) and `as_saved_view_error` have no test that the
`gts_id` they name is `qa.notification_config` / `qa.saved_view`. Same
status-plus-`gts_id` asserts the JIRA wrapper already has.

**#28 — a non-JSON 200 from JIRA is dropped silently.**
`qa-insights/src/infra/jira/oagw_client.rs:601` — `warn!` then `None`, matching
the HTTP-error arms directly above it.

**#29 — `to_value` failure writes an empty object.**
`environments_sea_repo.rs:419` currently answers `unwrap_or_else(|_| json!({}))`
and continues to a `Checked` status. Skip the health write and log instead: a
recorded observation that says "checked, nothing observed" is worse than no
observation.

**#51 — bearer tokens in the UI access log.**
`qa-platform-ui/default.conf.template` sets no `access_log` and no
`log_format`, so nginx's default `combined` writes the full request line,
`?access_token=eyJ…` included. The file measures this itself at `:80-86` — 60
lines and growing. Add a `log_format` logging `$uri` rather than `$request`,
applied with `access_log` **inside the
`location ~ ^/qa/v1/runs/[^/]+/logs$` block only**, so the bridge, the
fail-closed behaviour and the one-route blast radius are all untouched. Verify:
`docker compose logs ui | grep -c access_token=` → 0.

**#56 — `tenants_for(...).unwrap_or_default()`.** `qa-insights/src/gear.rs:1228`
and `:1251`. Log the error and skip the cycle; do not turn a failed directory
read into "this deployment has no tenants".

---

## 5. Phase 3 — IO error classification

**Findings:** #6, #7, #8, #9, #26, plus `plans.rs:389` and `:850`.

**The shared rule.** A filesystem call answers "absent" only for
`std::io::ErrorKind::NotFound`. Every other kind — `PermissionDenied`,
`NotADirectory`, an IO error mid-read — becomes `Internal` or `SyncFailed`
carrying the original as `#[source]`. `qa-catalog/src/domain/service/repos.rs`
already does exactly this at `:297` and `:336`; this phase applies that gear's
own existing rule to the six sites that do not follow it.

| Site | Today | After |
|---|---|---|
| `plans.rs:183` | any `read_to_string` failure → `PlanNotFound` (404) | NotFound → `PlanNotFound`; else Internal |
| `plans.rs:230` | any failure → `FileNotFound` | same rule |
| `plans.rs:389` | `let Ok(content) = … else` | same rule |
| `plans.rs:629,630` | `canonicalize` failure → `RepoNotSynced` | Internal/SyncFailed when the path exists |
| `plans.rs:649` | any `canonicalize` failure → `Ok(None)` | `Ok(None)` only on NotFound |
| `plans.rs:850` | `.is_ok_and(…)` | same rule |

**#9 is the dangerous one and gets its own treatment.**
`qa-runs/src/domain/service/launch.rs:2014-2021` — the per-file `TEST_META`
retry ends in `_ => unreadable += 1`, which counts a `Forbidden`, a database
failure and an `Internal` as "this file has no opinion about exclusivity". The
module's own doc (`:1920-1929`) already names what that costs, and its "what
must never happen here" paragraph (`:1943`) is about the same hazard from the
other side. An unreadable file is omitted, which is right; an *unauthorized* or
*failed* read must propagate, because the resolution it silently produces is
**parallel** — a destructive suite losing its platform-to-itself guarantee.
Retry only on `FileNotFound`; propagate the rest.

**Method.** TDD, in the strict order: a test that asserts the current wrong
behaviour, run red against the fix, then the fix. #9 in particular gets a test
that a `Forbidden` from `get_test_meta` does **not** resolve a
declared-exclusive suite as parallel.

---

## 6. Phase 4 — Lifecycle and resource bounds

**Findings:** #50, #22, #30, then #18, #19, #20, #21, #31, #32, #33.

**#50 is first, and the ordering is the design decision.**
`infra/executor/argo/watch.rs:435-438` opens every pod log with
`LogParams { container, follow: true, ..default() }` — no `since_time`, no
`since_seconds`, no `tail_lines`. A fresh `Watcher` starts with
`drained: HashSet::new()` (`:251`). So every re-attach re-reads each pod's log
from byte 0, and each replayed line reaches `run_logs_sea_repo.rs:138`'s
`CONCAT(text, $1)`. `RunLogsRepository` has no truncate, replace or offset
(`domain/repos/run_logs_repo.rs:92,107`), so nothing can undo it. Re-attach is
not exotic — `dispatch.rs:1722` re-attaches on every 5 s tick any live run whose
observer ended, and a process restart, a transient API-server error
(`argo/watch.rs:286`) and an ingest failure (`watch.rs:443`) all end one.

Replay is idempotent everywhere else in ingest — `upsert_test_result` replaces
the row, the five counters are tallied from stored rows rather than incremented
(`ingest.rs:552`) — which is why no existing test is red.

**Fix — the resumable read, not the stop-gap.** Carry a per-`(run_id, node)`
line offset into `RunExecutor::watch` and translate it to
`LogParams.since_time` / `tail_lines`. The review also offers a `replace_log`
on the first `record` of a fresh watch; this design does not take it, because it
discards the un-flushed tail of the previous observer on every re-attach — it
trades duplicated text for lost text, and lost text is the failure #50 sits
inside a slice that exists to prevent. The stop-gap is named here only so a
planner who hits a genuine blocker on the port change has a recorded fallback,
and taking it would be a scope change to raise, not a judgement call.

**Verify with the mock:** attach, drain, drop the slot, re-attach, assert
`get_log().lines` is unchanged — red today. `MockRunExecutor` already replays
from the beginning deliberately (`dispatch.rs:1771-1774`), so the test needs no
new double.

**#22 depends on #50 and is judged on merit.** `archive.rs:309-319`'s `record()`
is uncapped, and the module header (`:11-13`) defends that against design §8's
no-cap decision, bounded "in practice by the flush period". That argument is
sound for a log that is read once. It is not sound for one that a flapping API
server re-appends in full every 5 s. So: fix #50 first, and #22's cap then
becomes a bound on a genuinely-large run rather than a truncation of duplicated
text. Cap at `MAX_RETAINED_BYTES_PER_RUN`, consistent with the broadcaster.

**#30 — one 2 MB line becomes hundreds of MB.**
`argo/watch.rs:459`'s `emit_line` passes the line through untruncated;
truncation happens only on the read side at `api/rest/sse.rs:141`
(`MAX_LINE_BYTES = 8 * 1024`). Truncate to the same cap before emitting, so the
broadcaster and the archive carry what the reader will get.

**#18, #19, #20, #21 — unsupervised spawns.** `watch.rs:481` spawns the observer
detached with no `CancellationToken` and no `JoinHandle`; `argo/watch.rs:253`
does the same for the watcher; `argo/watch.rs:401`'s kube log follow has neither
timeout nor cancel. `grep CancellationToken` over `argo/watch.rs` is zero hits.
The observer's lifetime is correctly the run's and not the caller's
(`watch.rs:455-465` argues this well) — what is missing is the gear's shutdown,
which is neither. Select on the gear cancel token next to ingest; thread the
token into `start()` / `follow()`; keep `JoinHandle`s. And bound the observers:
`max_concurrent_runs` defaults to `0` (`config.rs:514`), which is not a bound.

**#31, #32, #33 — per-tenant loops that ignore cancellation.**
`qa-catalog/src/gear.rs:457` (bundle GC, per tenant),
`qa-environments/src/domain/service/environments.rs:1077` (per environment, each
a network round trip — at the NFR's 100 environments this is the whole shutdown
budget), `qa-insights/src/gear.rs:1228,1251` (three tenant loops). Each ticker's
`select!` already holds a token; the loop bodies do not see it. Pass it in and
return when cancelled.

---

## 7. Phase 5 — Types and layering

**Findings:** #10, #11, #12, #14, #15, #16, #17, #25, #34, #35, #37, #38, #39.

**One exclusivity enum, replacing three spellings.** A closed
`Inherit | Exclusive | Shared` in `qa-catalog-sdk`, re-exported by
`qa-runs-sdk`, replacing `pub type ExclusiveFlag = Option<bool>`
(`qa-catalog-sdk/models.rs:60`) on `Plan` and `TestFileMeta`, and
`LaunchRequest.exclusive: Option<bool>` (`qa-runs-sdk/models.rs:207`). The
semantics are already written down correctly in both places —
`models.rs:205-207` says "`None` means inherit, which is **not** `Some(false)`"
— so this encodes an understood rule rather than deciding a new one.
`resolved_exclusive: bool` and `QueueEntry::exclusive: bool` stay `bool`: those
are resolved decisions, not three-state inputs.

**SDK enums on the wire.** `ClusterHealthView.status: String`
(`qa-environments-sdk/models.rs:371`) becomes an enum over the five documented
values plus `Unreachable`; `RunDto.state` / `exclusive_tier` / `source`
(`qa-runs/api/rest/dto.rs:344,349,355`) and the saved-view `scope`
(`qa-insights/api/rest/dto.rs:1098` and six siblings) serialize as the SDK enums
that already exist beside them.

**Transport-agnostic error mappers.** `qa-runs/src/domain/local_client/client.rs:36`
imports `as_queue_error` / `as_schedule_error` and
`qa-insights/src/domain/local_client/client.rs:73` imports `as_jira_error` — all
three from `api::rest::error`. The rework moved these clients from `infra/` into
`domain/`, which makes the direction of the dependency worse, not better: a
domain module now reaches into the REST layer. Move the mappers out of
`api/rest` into a transport-neutral module both call. Same file, `:37`: #39's
`use crate::infra::ConcreteAppServices` becomes an alias supplied by `gear.rs`'s
composition root.

**#17 — Block Kit in a domain port.** `domain/ports/slack_client.rs:63` imports
`serde_json::Value` so the port can carry Block Kit blocks. A domain type here,
with the JSON rendering in the oagw adapter.

**#37 — `MailClient::send` has no `SecurityContext`.**
`domain/ports/mail_client.rs:83` versus `slack_client.rs:113-117`, which takes
one. Add `ctx: &SecurityContext`.

**#38 — `pub mod infra` leaks SeaORM as crate API.** All four gears'
`lib.rs` export `domain` and `infra` publicly. `pub(crate)`, with whatever the
gear genuinely needs to export named explicitly.

**#25 — `From<DbError>` keeps only `to_string()`.** Four gears; qa-catalog's
already carries the `TODO(DE1302)` and the `#[allow]` naming the fix
(`domain/error.rs:106-110`). Box the source.

**#14 — `ValueRepo` takes `self.db.conn()`.**
`gears/credstore/plugins/postgres-credstore-plugin/src/infra/storage/repo.rs:68`
and `:189`. Take `runner: &impl DBRunner` so a caller can compose it into a
transaction. This crate is new in this branch (22 files, +3063), so this is our
defect, not inherited.

---

## 8. Phase 6 — Scale

**Findings:** #27, #55.

**#55 gets the real fix, not the doc-only half.** `EnvironmentsRepository::list`
is `find().secure().scope_with(scope).all(runner)` with no limit and no filter
(`environments_sea_repo.rs:56-68`), `list_all_with_tenant` the same (`:70`), and
`grep -ril odata gears/qa-platform/qa-environments` returns zero files. The
review offers "record the carve-out" as the honest-and-cheap half. This design
declines it, because DESIGN §1.2 allocates `cpt-cf-qa-nfr-scale` to
"qa-runs + qa-insights" while that NFR's **first number is 100 platforms**,
which is a qa-environments collection and nothing else. A carve-out sentence
would document that the one gear carrying the number is the one gear that
ignores it.

So: `PAGE_LIMITS { default: 200, max: 500 }` and an `odata.rs` on
`/qa/v1/environments` and `/qa/v1/variables`, matching the shape the other two
gears already have — a closed enum of filterable fields chosen against the
table's actual indexes, so an unknown field is a 400 rather than a scan.

**The UI half, independently.**
`qa-platform-ui/src/api/hooks.ts:225` — `fetchEnvironmentDtos` is a bare
`apiGet('/environments')` outside any react-query cache, reached through
`environmentNameIndex()` (`:235`) and `resolveEnvironmentId()` (`:247`) from
inside `fetchRunDetails` (`:447`), which `useRun(name, 5000)` polls every 5 s
per open Run Detail page and `useDashboard` every 15 s. Each `EnvironmentDto`
carries its nested observation. Put it behind its own cached query with a
`staleTime`; environment names do not change at 5 s resolution.

**#27 — `qa.plan` declares an unused `RESOURCE_ID`.**
`qa-catalog/src/domain/service/mod.rs:131-134` declares it; the `PLAN` GET passes
`resource_id=None` (`plans.rs:169`). Drop it, as `qa.jira_config` already did.
Ordering note: this must land **before** Phase 7, because the catalog is
generated from these consts.

---

## 9. Phase 7 — The permission catalog

**Finding:** #1. This is the review's first entry and its largest.

**What is missing.** `grep -rln AuthzPermissionV1 gears/qa-platform` returns
nothing. The PEP asks for `qa.*` actions AM-style, and there is no catalog from
which GTS/RBAC can grant them. The request path itself is sound in shape —
`.authenticated()` → `PolicyEnforcer::access_scope` with
`require_constraints=true` → `AccessScope` → `#[secure(tenant_col)]`, verified
clean by the review — but it is not a complete AM/RMS integration without this.

**What to build.** A `gts/permissions.rs` per gear, `gts_instance!`, generated
from the same `resources::*` and `actions::*` consts the enforcement path reads.
Model: `gears/bss/ledger/ledger/src/gts/permissions.rs` and its
`permissions_tests.rs`. The surface is 18 resource types and ~15 actions:

* qa-catalog — `qa.test_repo`, `qa.plan`, `qa.custom_plan`, `qa.product`,
  `qa.ssh_key`, `qa.bundle` × get/list/create/update/delete/sync
* qa-environments — `qa.platform`, `qa.variable`, `qa.lease` ×
  get/list/create/update/delete/acquire/release
* qa-insights — `qa.test_result`, `qa.saved_view`, `qa.jira_config`,
  `qa.jira_bug`, `qa.notification_config`, `qa.jira` ×
  get/list/create/update/delete/collect/rebuild/test
* qa-runs — `qa.run`, `qa.queue_entry`, `qa.schedule` ×
  create/get/list/dispatch/cancel/rerun/force_start/update/delete/fire

**The one trap, stated so the plan cannot walk into it.** The resource type for
environments is **`"qa.platform"`**, not `"qa.environment"`. The rework renamed
the aggregate and deliberately did not rename the PDP string, because that
string is what policies are written against and changing it would silently
change who is authorized for what — the reasoning is at
`qa-environments/src/domain/service/mod.rs:90-99`. Generate from
`resources::PLATFORM`, never from the type name.

**Anti-drift test.** The catalog equals the enforced set, both directions: a
resource/action pair reachable from a handler and absent from the catalog fails,
and a catalog entry no handler enforces fails. This is what makes the catalog a
guarantee rather than a second list to keep in sync by hand.

**Grants come after, not with.** The review says "do not ship grants first" and
this design agrees.

**What this unblocks.** Three of the review's six mandatory-and-missing tests
are blocked on this and land with it: action-without-grant → denied; anti-drift
catalog == enforced set; tenant-scoped grant sees only its own subtree (needs a
real PDP fixture).

---

## 10. Phase 8 — Observability

**Finding:** #4.

`grep -rn 'metrics!\|prometheus\|opentelemetry\|histogram'` over all four gears,
both plugin crates and `qa-product-sdk` returns **zero** non-test hits. DESIGN's
p95 NFRs are therefore unobservable — not badly observed, unobservable.

**Pattern to follow:** `gears/system/account-management`, which carries both a
`domain/metrics.rs` and an `infra/metrics.rs` with tests beside each. The
workspace already has the dependency: `libs/toolkit` has `otel` in its default
features (`Cargo.toml:28`).

**RED (rate, errors, duration) on the five paths the NFRs name:** dispatch,
ingest, collect, the JIRA poll, and observation. The three new crates inherit
the same gap and get the same treatment for their plugin-boundary calls.

Last of the substantive phases because it is additive: it changes no behaviour,
and it is most useful once the paths it measures are the fixed ones.

---

## 11. Phase 9 — Decisions and cleanups

**Findings:** #5, #41, #42, #43, #44, #45, #46, #53, #54.

**#5 — judged on merit, per section 2.2.** The reconciler's argument stands and
is not touched. The JIRA poller's does not: its effect is a launch.

**Decision: a claim-row for `ROLE_JIRA_POLLER` alone.** The alternative —
pinning `replicaCount: 1` with a `deploy/helm/tests/` assertion and writing the
constraint into DESIGN — is cheaper and true today, and it is rejected because
it makes a correctness property depend on a chart value that a future scale-out
would silently break. The claim-row is small: the poller already runs as a
tenant-scoped pass, so the row is `(role, tenant, holder, expires_at)` with a
conditional update, and `leases_sea_repo.rs`' CAS is the pattern to copy — the
review verifies that one as sound. The other two roles keep
`NoopLeaderElector`, and `infra/leader/mod.rs`'s reconciler paragraph gains a
sentence saying the poller is the exception and why.

**Test:** two pollers, one rerun — the review's own named missing test.

**#41 — a fake isolation test.** `products_tests.rs`'s
`MockProductsRepository` ignores `_scope` on every method (`:57,72,80,90,120`),
so `"a product outside the scope must 404"` (`:381`) actually asserts that a
different **id** 404s. Use a repo that honours the scope, or drop the isolation
claim from the message. The former; the assertion is worth having.

**#42 — spelling only.** 385 `C: DBRunner` against 6 `impl DBRunner`. Unify on
touch, no churn — the review says so and it is right.

**#53 — dead UI.** `pages/SettingsPage.tsx` (144 lines) and
`pages/settings/SettingsJiraPollerPage.tsx` (76) have no importer;
`App.tsx:146` routes `jira-poller` to a `<Navigate>` and `SettingsJiraPage`
carries both configs. `hooks.ts:652`'s `useTestRecentResults` is exported and
called from nowhere, and it carries a latent trap: `/qa/v1/test-results` has no
chronological `$orderby` and defaults to `id` descending over a
`Uuid::new_v4()` primary key, so its "recent" would be a random sample. Delete
all three.

**#54 — a doc comment on the wrong item.** Move
`handlers/environments.rs:254-260` above `:359`; leave `:263`'s own line on the
helper.

**#43, #44, #45, #46 — test-quality LOWs.** Delete the constant-echo assert
(`slack_oagw_tests.rs:254`); collapse or differentiate the three identical
`message()` calls (`mail_unsupported.rs:74,87`); drop the duplicated denial loop
(`schedules_handler_tests.rs`); drop the constructor echoes after the
compact-JSON assert in `qa-insights/api/rest/dto.rs`.

---

## 12. Scope boundaries

**Delivery.** One branch off `feature/qa-product-plugins`, one reviewable commit
per phase, so the lead who filed the findings reviews each delta against them.

**Not in scope, and why:**

* **Grants.** Phase 7 ships the catalog and the anti-drift test. Authoring
  actual role grants is a policy decision for whoever owns the deployment's
  realm.
* **Lifting `LeaderElector` into `libs/`.** Four copies exist
  (`qa-runs`, `qa-insights`, `chat-engine`, `mini-chat`) and
  `qa-insights/src/infra/leader/mod.rs:16-22` names all four for whoever does
  it. A cross-gear change, not this remediation.
* **SSH host-key verification.** `[by-design]`, authorised explicitly on
  2026-08-27, recorded in ADR-0005 as amended. The review calls it the largest
  accepted risk in the diff and it remains accepted. Not reopened here.
* **Plaintext secrets in `postgres-credstore-plugin`.** `[by-design]` with its
  reason stated. #14 fixes that crate's `DBRunner` signature and nothing about
  its storage model.
* **`log_storage_ref` never written.** Design D-RLP-6, a dead field by decision.
* **Force-start overriding the lease.** `[by-design]`, gated by its own PEP
  action.
* **Attribute-schema versioning in the plugin observation path.** The residual
  named in section 2.1. Belongs to the plugin spec.
* **The review's own "follow-ups this pass did NOT cover"** — analytics
  aggregation arithmetic, the JIRA state machine, notification templating, the
  runner-side Python, the Keycloak realm JSON, and the four domain cores'
  rule-by-rule parity. Unreviewed is not the same as defective; they are out of
  scope because nothing has been filed against them.

**Stopping points.** Phases are ordered so the branch is coherent after each
one. Phases 1–4 are the correctness and security core; 5–6 are quality with real
but lower stakes; 7 and 8 are each a subsystem's worth of work and can be
scheduled independently once 1–6 have landed.
