# qa-insights: design and legacy-parity findings

**Date**: 2026-08-18
**Status**: approved (user, 2026-08-18)
**Features**: DECOMPOSITION 2.5 (`cpt-cf-qa-feature-insights-foundation`) + 2.8 (`cpt-cf-qa-feature-jira-notifications`)
**Governing principle** (restated by the user on 2026-08-18): *preserve legacy behavior; adapt only the implementation to the gear architecture.* The source system is `../testrunner`; every behavioral claim below carries its legacy citation.

## Table of contents

- [1. Context and problem](#1-context-and-problem)
- [2. Findings](#2-findings)
- [3. Decisions (D1-D9)](#3-decisions-d1-d9)
- [4. Design](#4-design)
- [5. Testing](#5-testing)
- [6. Document and requirement changes](#6-document-and-requirement-changes)
- [7. Risks and open items](#7-risks-and-open-items)

---

## 1. Context and problem

`qa-catalog`, `qa-environments` and `qa-runs` have shipped. `qa-insights` is the fourth and last domain gear: event-driven ingestion of run and test results, per-test history, dashboard and coverage aggregates, analytics with saved views, the JIRA bug loop, and run notifications.

The problem this spec exists to solve is not "how do we build the gear". It is that **PRD/DESIGN/DECOMPOSITION describe substantially less than the source system does.** The three documents allocate four tables and roughly four endpoints to insights. The source system spends ~9.8k LOC on the same territory:

| Legacy module | LOC |
|---|---|
| `manager/src/routes/analytics.rs` | 2693 |
| `manager/src/services/notifications.rs` | 1920 |
| `manager/src/services/run_history.rs` | 935 |
| `manager/src/routes/settings.rs` (JIRA + notification halves) | 773 |
| `manager/src/routes/dashboard.rs` | 626 |
| `manager/src/services/run_results_poller.rs` | 356 |
| `manager/src/services/jira_poller.rs` | 338 |
| `manager/src/services/jira.rs` | 321 |
| `manager/src/services/collect.rs` | 231 |

Building to the specs as written would ship a gear that is missing per-case result granularity, expected-case counts, five of the eight analytics endpoints, the entire Slack notification surface, and the per-schedule notification settings — none of which any spec document records as an intentional cut.

This spec therefore front-loads the parity audit, records the divergences as numbered decisions in the same format the `qa-runs` plan used for its D1-D4, and designs the gear against legacy rather than against the specs. The specs are amended, not the behavior.

---

## 2. Findings

### Finding 1 — analytics is built on per-*case* rows, which the gear architecture dropped

Legacy stores results at two granularities:

* `test_results` (`manager/migrations/001_initial.sql:65`) — one row per test *file* per run: `test_name`, `status`, `launch_id`, `jira_key`, `logs`, and (added later) `test_file`.
* `test_case_results` (`:253`) — one row per test *function* per run, parsed from the runner's `TEST_CASE` markers: `test_file`, `nodeid`, `name`, `status`, `duration`, `reason`, `ticket`. The migration comment states its purpose directly: *"Lets analytics aggregate per-case, not just per-file."*

`qa-runs` shipped a single merged table, `qa_run_test_results` (`qa-runs/qa-runs/src/infra/storage/migrations/m20260813_000003_initial.rs:387`), with columns `test_file, test_name, status, duration, launch_id, jira_key`. It has **no** `nodeid`, `reason` or `ticket`, and the `qa.test.result` event payload (`qa-runs/qa-runs/src/infra/events/payloads.rs:497-507` as of this finding — that file was later deleted when qa-runs' event publisher was found unreachable, since event-broker registers no client, and removed) carried the same reduced set — its `node` field is the *execution* node (a DAG node name such as `repo-smoke`), not a pytest nodeid.

What is unreachable without the case rows, all of it visible in the legacy response types (`routes/analytics.rs:89-110`, `:112-134`):

* `OverviewSummary.case_total / case_passed / case_failed / case_skipped / case_xfail / case_xpass`
* `AnalyticsListItem.case_status` and `AnalyticsListItem.case_tickets` — the per-file dot color and ticket badges the UI renders without a second fetch
* the xfail/xpass distinction, which `qa-runs`' own migration comment already flags as counted separately by legacy and deliberately excluded from the five run counters (`m20260813_000003_initial.rs:425-429` (same file))

### Finding 2 — "expected cases" has two sources, and neither has an owner in the gear split

Legacy computes expected-case counts two ways:

* **Static**, `count_test_functions` (`routes/analytics.rs:1865`, called at `:1882`) — counts `def test_*` in file content read straight off the on-disk checkout. Does not expand `@pytest.mark.parametrize`. Feeds `OverviewSummary.case_expected`.
* **Exact**, `test_case_collect` (`001_initial.sql:272`) — filled by collect-only runner workflows that execute `pytest --collect-only` with parametrize expanded, then POST per-file counts back. Driven by an hourly poller on the default branch `main` (`services/collect.rs:19`, `:122-129`) and by an on-demand Analytics button (`routes/analytics.rs::api_collect_trigger`).

The runner-facing contract for the exact path is two environment variables set by the control plane (`services/argo.rs:50-57`): `COLLECT_ONLY=true` and `VHP_COLLECT_URL=<url>`, where the URL is built by the control plane itself (`services/collect.rs:29-31`, `format!("{}/api/collect/{}/{}", base, repo.id, branch)`) and served at `POST /api/collect/{repo_id}/{branch}` (`routes/mod.rs:214-217`). Because the URL is control-plane-supplied, **re-homing that route onto qa-insights preserves the runner contract exactly** — the runner posts wherever it is told.

Neither source has a home in the four-gear split: insights has no repository checkout (that is qa-catalog's, per ADR-0005) and no executor (that is qa-runs').

### Finding 3 — the analytics surface is eight endpoints, not two

Registered legacy routes (`routes/mod.rs:211-249`, `:297-304`):

| Route | Handler |
|---|---|
| `GET /api/analytics/overview` | `api_overview` |
| `GET /api/analytics/build-tests` | `api_build_tests` |
| `GET /api/analytics/export` | `api_export` |
| `GET/POST /api/analytics/views`, `PUT/DELETE /api/analytics/views/{id}` | saved-view CRUD |
| `GET /api/analytics/plan/{plan_id}/tests` | `api_plan_tests` |
| `GET /api/analytics/plan/{plan_id}/builds` | `api_plan_builds` |
| `GET /api/analytics/plan/{plan_id}/test-history` | `api_plan_test_history` |
| `POST /api/analytics/collect` | `api_collect_trigger` |
| `POST /api/collect/{repo_id}/{branch}` | `api_collect_report` |
| `GET /api/dashboard`, `GET /api/dashboard/coverage` | dashboard |
| `GET /api/jira/open-bugs` | open-bug view |

And `AnalyticsOverviewResponse` (`routes/analytics.rs:231-249`) is not a summary — it is nine sections in one payload: `summary`, `lists` (passed/failed/not_run), `heatmap`, `trend`, `build_distribution`, `flaky`, `quality_vectors`, `grouped` (by component / tag / platform). PRD `cpt-cf-qa-fr-insights-dashboard` describes this as "dashboard aggregates (recent runs, pass rates, active/queued runs) and coverage views".

### Finding 4 — analytics reads the catalog universe, a dependency DESIGN §3.4 does not list

Every analytics list item carries `component`, `tags`, `plan_id`, `plan_name`, `versions` (`routes/analytics.rs:112-134`), and the quality-vector summary is computed over TEST_META `quality_vectors`. Legacy reads these from plans and TEST_META on the checkout. In the gear split those are qa-catalog's, so **qa-insights must call `qa-catalog-sdk`**. DESIGN §3.4's dependency table has rows for qa-catalog, qa-environments and qa-runs as *dependencies of qa-runs*, and one `qa-runs | qa-runs-sdk | Auto-rerun launches from qa-insights` row. There is no qa-catalog row for insights.

This does not violate `cpt-cf-qa-principle-async-insights`: that principle forbids the *run path* from reading insights, not insights from reading catalog at query time.

### Finding 5 — notifications are Slack-first, and far wider than "email on run completion"

`NotificationsConfig` (`manager/src/models.rs`) has fifteen fields: `slack_webhook_url`, `slack_channel`, `manager_ui_base_url`, `slack_enabled`, `notify_on_failure`, `notify_on_success`, `notify_on_schedule_completion`, `scheduled_run_slack_enabled`, `scheduled_run_slack_templates`, `run_queue_queued_slack_enabled`, `email_smtp_host`, `email_smtp_port`, `email_from`, `email_recipients`, `email_enabled`.

The service (`services/notifications.rs`) exposes: `send_slack` (`:97`), `send_slack_to_channel` (`:102`), `send_slack_to_channel_with_blocks` (`:113`), `send_email` (`:133`), `notify_run_completed` (`:180`), `send_test_notification` (`:358`), `preview_scheduled_run_message` (`:388`), `send_scheduled_run_test_notification` (`:407`), `notify_scheduled_run_status` (`:431`), `get_notification_log` (`:622`), `notify_queue_event` (`:657`).

Two supporting tables exist that no spec mentions: `run_notifications` (`001_initial.sql:197`), a dedupe table keyed `(workflow_name, notification_kind, event_type)`, and `notification_log` (`:205`), an audit trail surfaced at `GET /api/settings/notifications/log` (`routes/mod.rs:284-285`).

Queue events are two: `Queued` (opt-in, off by default) and `Expired` — the latter documented in the model as *"Mandatory: this is the event that stops a run vanishing silently."* Legacy sends it inline from the dispatcher tick, which is why the notification HTTP client carries a hard 10s timeout (`services/notifications.rs:52-64`).

Scheduled-run status notifications cover six events: `Pending`, `InProgress`, `Succeeded`, `Failed`, `Error`, `Skipped`.

### Finding 6 — per-schedule notification settings live on the schedule, and qa-runs has no columns for them

`PUT /api/schedules/{name}/notifications` (`routes/mod.rs:96-97`) edits three per-schedule fields — `slack_notifications_enabled`, `slack_channel`, `slack_notification_events` — which legacy persists as CronWorkflow annotations and round-trips by delete-and-recreate (`routes/schedules.rs::api_update_notifications`). In the gear split schedules are qa-runs' (`qa_schedules`, `m20260813_000004_schedules.rs`), and that table has **no notification columns**.

### Finding 7 — DESIGN's `saved_views` and `notification_rules` do not match legacy

* DESIGN §3.7 lists `saved_views (name, query JSONB)`. Legacy is `analytics_saved_views (id, owner_id, scope, plan_id, name, query_json, created_at, updated_at)` (`001_initial.sql:183`) with a unique index on `(owner_id, scope, COALESCE(plan_id, ''), name)` (`:194`). Scope and per-plan qualification are load-bearing: `GET /api/analytics/views` takes `scope` and `plan_id` as query parameters.
* DESIGN §3.7 lists `notification_rules (trigger, recipients)`. **No such table exists in legacy.** Notification configuration is a singleton row in the generic `settings` key/value table (`001_initial.sql:93`) under the key `notifications`, read through `SettingsService::get_or_default`.

### Finding 8 — auto-rerun goes through admission, and requires a new build

Two claims worth pinning because a stale comment contradicts one of them:

* `services/argo.rs:369-372` says *"the two paths that bypass admission (`collect`, `jira_poller`)"*. For `jira_poller` this is **stale and wrong**. `services/jira_poller.rs:8-15` states the opposite explicitly, citing VHP-2618: an auto-rerun *"goes through `run_dispatcher::launch` … it is a launch like any other and must be admitted, queued behind an exclusive run, and have its exclusivity resolved from the tiers."* `trigger_auto_rerun` (`:88-96`) confirms it. This matches PRD `cpt-cf-qa-fr-insights-auto-rerun`'s "using the standard launch path". No divergence.
* `collect` genuinely does bypass admission. That half of the comment is accurate.
* Auto-rerun is gated on **two** conditions, not one: `poller_config.auto_rerun_on_resolve` **and** `check_new_build(...)` (`services/jira_poller.rs:62-70`) — a resolved bug does not trigger a rerun unless a newer build exists. PRD records only the resolution transition.

### Finding 9 — insights is the platform's first event-broker consumer

`qa-runs` publishes eight event types on one topic, `gts.cf.core.events.topic.v1~cf.qa.runs.lifecycle.v1` (`payloads.rs:96`, `:147-154`, `ALL_TYPE_IDS` at `:562`). A repository-wide search for `ConsumerBuilder` / `consumer_group` / `.subscribe(` across `gears/` and `libs/` finds no gear that consumes broker events. The consumer adapter is net-new work with no in-repo precedent to copy.

PRD §11 already carries the matching risk: *"event-broker durable backend timeline — lifecycle events not durable; insights could miss events"*, mitigated by *"ingestion designed idempotent + rebuildable from qa-runs records"*. Legacy had no equivalent exposure because it polled (`services/run_results_poller.rs`).

---

## 3. Decisions (D1-D9)

Each decision names the spec claim, the legacy truth, and the resolution. All nine resolve toward legacy.

### D1 — Two result granularities are restored; qa-runs is amended additively

| | |
|---|---|
| **Spec claims** | DESIGN §3.7: qa-insights owns `test_results (run_id, file, test_id, status, duration, product_version, platform_id)`. |
| **Legacy does** | Finding 1: `test_results` (file-level) **and** `test_case_results` (case-level, with `nodeid`, `reason`, `ticket`). |
| **Resolution** | qa-insights owns both tables. `qa-runs` gains `nodeid`, `reason` and `ticket` on `qa_run_test_results` and on the `qa.test.result` payload, as an **additive v1 extension** — `cpt-cf-qa-interface-events` forbids mutating a published version, and adding optional fields does not. Alternatives rejected: a second producer for case events has no producer until feature 2.7; degrading to file-level silently changes every case-level number the UI shows. |

### D2 — Expected-case counts: static from qa-catalog, exact via a collect run kind

| | |
|---|---|
| **Spec claims** | Nothing. Neither `test_case_collect` nor `case_expected` appears in any spec document. |
| **Legacy does** | Finding 2. |
| **Resolution** | Split by owner. **Static**: `qa-catalog` exposes per-file test-function counts over its SDK — it already walks and parses those files for TEST_META, so this is a projection of work it does anyway. **Exact**: `qa-runs` gains a `Collect` run kind that sets `COLLECT_ONLY=true` and `VHP_COLLECT_URL`, bypassing admission exactly as legacy does; qa-insights triggers it over `qa-runs-sdk` (hourly leader-elected poller on the default branch, plus the on-demand trigger) and serves the report route itself, so the runner's POST target is unchanged in shape. qa-insights owns `test_case_collect`. |

### D3 — The full analytics surface is ported

| | |
|---|---|
| **Spec claims** | PRD `cpt-cf-qa-fr-insights-dashboard`: "dashboard aggregates (recent runs, pass rates, active/queued runs) and coverage views". `cpt-cf-qa-fr-insights-analytics`: "filterable, sortable analytics … and saved views". |
| **Legacy does** | Finding 3: eleven routes; a nine-section overview payload including heatmap, trend, build distribution, flaky detection, quality vectors and three groupings. |
| **Resolution** | Port all of it. Both FRs are amended to enumerate the sections and endpoints rather than gesturing at them. |

### D4 — Saved views keep their real key

| | |
|---|---|
| **Spec claims** | DESIGN §3.7: `saved_views (name, query JSONB)`. |
| **Legacy does** | Finding 7: `analytics_saved_views` with `owner_id`, `scope`, `plan_id`, unique on `(owner_id, scope, COALESCE(plan_id,''), name)`. |
| **Resolution** | Port the real shape. `owner_id` maps onto the Fabric principal from `SecurityContext`; `tenant_id` is added and joins the unique index, since two tenants must be able to hold the same view name. |

### D5 — Slack, queue events, scheduled-run events, dedupe and the log are all ported

| | |
|---|---|
| **Spec claims** | PRD `cpt-cf-qa-fr-insights-notifications`: "configurable email notifications on run completion (with status, counts, and links) via the platform outbound gateway". |
| **Legacy does** | Finding 5. |
| **Resolution** | Port the whole surface: Slack (webhook and channel forms, including block rendering and the scheduled-run templates), email, the six scheduled-run status events, the two queue events with `Expired` mandatory and un-toggleable, the `run_notifications` dedupe table and the `notification_log` audit trail with its read endpoint, plus the test-send and preview endpoints. Slack webhook egress goes through **oagw**, like SMTP — `cpt-cf-qa-contract-egress` permits no second exception, and unlike git (ADR-0005) a webhook POST is an ordinary request oagw can express. |

### D6 — `notification_rules` is dropped; config is a per-tenant singleton

| | |
|---|---|
| **Spec claims** | DESIGN §3.7: `notification_rules (trigger, recipients)`. |
| **Legacy does** | Finding 7: a singleton `settings` row under key `notifications`. There is no rules table. |
| **Resolution** | Drop the invented table. Ship typed per-tenant singleton rows — `qa_notification_config`, `qa_jira_config`, `qa_jira_poller_config` — rather than porting legacy's untyped key/value `settings` table, because a generic JSONB bag defeats the migration and validation the platform gives typed columns. This is an implementation adaptation, not a behavior change: the same fields, the same defaults, the same GET/PUT surface. Multi-tenancy is the one genuine addition — legacy's singleton becomes one row per tenant. |

### D7 — Legacy query parameters on aggregates; OData only on the flat collections

| | |
|---|---|
| **Spec claims** | DESIGN §1.2 / §3.3: OData filtering and paging on the qa-insights collections; `cpt-cf-qa-nfr-scale` names it as the mitigation for unbounded row growth. |
| **Legacy does** | Aggregate endpoints take a fixed parameter set: `product_id`, `version`, `scope`, `plan_id`, `branch`, `days_heatmap`, `days_trend`, `group_by`, `group_value` (`routes/analytics.rs:18-33`). |
| **Resolution** | Both, on different endpoints. The nine-section overview, export, build-tests and the three plan drill-downs are **computed aggregates, not collections** — OData over them is meaningless, and changing their parameters breaks the SPA. They keep legacy's parameters verbatim. OData applies to the flat, unbounded collections that `cpt-cf-qa-nfr-scale` is actually about: `test_results` and `test_case_results`. |

### D8 — Auto-rerun requires a new build

| | |
|---|---|
| **Spec claims** | PRD `cpt-cf-qa-fr-insights-auto-rerun`: "MUST optionally re-run affected tests automatically when a linked JIRA bug transitions to a resolved state". |
| **Legacy does** | Finding 8: gated on `auto_rerun_on_resolve` **and** `check_new_build`. |
| **Resolution** | Port the second condition and amend the FR. Also recorded in the plan: the `argo.rs:369-372` comment claiming `jira_poller` bypasses admission is stale — it does not, and the gear must not reproduce a bypass that legacy removed in VHP-2618. |

### D9 — Per-schedule notification settings become qa-runs columns

| | |
|---|---|
| **Spec claims** | Nothing. No document records per-schedule notification configuration. |
| **Legacy does** | Finding 6: three per-schedule fields edited at `PUT /api/schedules/{name}/notifications`. |
| **Resolution** | `qa-runs` gains `slack_notifications_enabled`, `slack_channel`, `slack_notification_events` on `qa_schedules`, exposed on `qa-runs-sdk`'s schedule model and at `PUT /qa/v1/schedules/{id}/notifications`. qa-insights reads them over the SDK when a scheduled run's status changes. Legacy's delete-and-recreate round-trip is *not* ported — it is an artifact of storing configuration in CronWorkflow annotations, and qa-runs has a real table. |

### Decisions inherited, not re-litigated

Four domain gears + qa-ui (ADR-0004); `qa-*` crate naming; full Fabric tenancy; structured events via event-broker SDK (ADR-0002); execution behind `RunExecutor` with a mock in p1 (ADR-0001); git egress confined to qa-catalog (ADR-0005); branch `feature/qa-platform-specs`, one squashed commit per deliverable.

---

## 4. Design

### 4.1 Shape

A standard ToolKit DDD-light gear pair at `gears/qa-platform/qa-insights/{qa-insights,qa-insights-sdk}`, structurally identical to the three shipped siblings: `domain/{service,ports,repos}`, `infra/{storage,events,jira,notify,leader}`, `api/rest`, migrations under `infra/storage/migrations`, SDK crate `serde`-free for contract purity.

### 4.2 Four pure domain cores

The numbers must match legacy exactly, so the logic that produces them is pure and written test-first before any I/O exists — the same discipline the qa-runs plan applied to exclusivity, queue planning, the state machine and cron.

1. **`analytics::universe`** — joins the catalog universe (test files x plans x TEST_META) with result rows to produce latest-status-per-test. Holds the branch filter with its `source_ref` -> `test_version` fallback (`routes/analytics.rs:24-27`), the not-run classification, and the rule that a file with no case rows contributes one case of its file-level status so totals never undercount (`routes/analytics.rs:1283-1288`).
2. **`analytics::aggregates`** — heatmap, trend, flaky pass-rate, quality-vector summary, grouped summaries, build distribution, and the CSV projection behind `/export`. Pure functions over the universe core's output.
3. **`jira::registry`** — bug/test identity keying (`test_name` + `plan_id`, with optional `app_version` and `platform` qualifiers), open-bug resolution, skip-list assembly as the comma-separated `test_name:JIRA-KEY` list the runner expects (`services/argo.rs:476-481`), resolved-transition detection and the new-build condition from D8.
4. **`notify::routing`** — which config field gates which event, dedupe-key derivation, Slack block and email body rendering, scheduled-run template expansion.

### 4.3 Ports and adapters

| Port | Adapter | Purpose |
|---|---|---|
| `EventStream` | `event-broker-sdk` consumer | Subscribe to the lifecycle topic, one consumer group |
| `RunsReader` | `qa-runs-sdk` | Reconciliation backfill, dashboard active/queued counts, schedule notification settings |
| `RunsLauncher` | `qa-runs-sdk` | Auto-rerun launches; collect-run triggers |
| `CatalogReader` | `qa-catalog-sdk` | Plan/TEST_META universe, static per-file case counts |
| `JiraClient` | oagw | Issue create/find, status polling |
| `SlackClient`, `MailClient` | oagw | Notification egress |
| `LeaderElection` | `cluster-sdk` | Single-instance JIRA poller, collect poller, reconciler |

The `qa-insights -> qa-runs` back-edge is the one ADR-0004 already sanctions; the new `qa-insights -> qa-catalog` edge is contract-mediated and equally non-cyclic.

### 4.4 Ingestion and gap recovery

Ingestion subscribes to `gts.cf.core.events.topic.v1~cf.qa.runs.lifecycle.v1` with a single consumer group, which is exactly what the single-topic decision in `payloads.rs:25-32` was made to enable. Per-run ordering is guaranteed by run-id partitioning, so `created -> queued -> started -> finished` arrives in order within a run.

Writes are idempotent by delete-then-insert on `(run_id, test_file, test_name)` — the same dedupe legacy performs (`routes/runs.rs:1153-1185`) — so redelivery is a no-op rather than a duplicate row.

Because the broker has no durable backend yet (Finding 9), two recovery paths exist:

* **Reconciler** — leader-elected, periodic. Lists qa-runs' runs finished since `qa_ingest_watermarks.last_reconciled_finished_at` minus a bounded lookback, diffs against ingested run ids, and backfills the difference over `qa-runs-sdk`. This is the closest analogue to legacy's polling model and makes the gear self-healing without operator attention.
* **Operator rebuild** — `POST /qa/v1/insights/rebuild` replays a time range from qa-runs, satisfying DESIGN's "rebuildable from qa-runs records" literally.

### 4.5 Data model

All tables `qa_`-prefixed, all carrying the standard `id`/`tenant_id`/`created_at`/`updated_at` set and a `ScopableEntity` mapping.

| Table | Legacy origin |
|---|---|
| `qa_test_results` | `test_results` (`001_initial.sql:65`) |
| `qa_test_case_results` | `test_case_results` (`:253`) |
| `qa_test_case_collect` | `test_case_collect` (`:272`) |
| `qa_analytics_saved_views` | `analytics_saved_views` (`:183`), key per D4 |
| `qa_jira_bugs` | `jira_bugs` (`:78`) |
| `qa_jira_config`, `qa_jira_poller_config`, `qa_notification_config` | `settings` rows, typed per D6 |
| `qa_run_notifications` | `run_notifications` (`:197`) |
| `qa_notification_log` | `notification_log` (`:205`) |
| `qa_ingest_watermarks` | none — required by 4.4 |

`qa_test_results` and `qa_test_case_results` are the two tables `cpt-cf-qa-nfr-scale`'s 5M-row target applies to, and the two that carry OData per D7. Indexes follow legacy's (`idx_test_results_name`, `idx_test_case_results_run_file`, `idx_jira_bugs_test`, `idx_jira_bugs_status`) with `tenant_id` leading.

### 4.6 API surface

Under `/qa/v1`, preserving legacy's parameters per D7: `dashboard`, `dashboard/coverage`, `analytics/overview`, `analytics/build-tests`, `analytics/export`, `analytics/views` (CRUD), `analytics/plan/{plan_id}/{tests,builds,test-history}`, `analytics/collect` (trigger), `collect/{repo_id}/{branch}` (runner report), `jira/open-bugs`, `jira/bugs` (link/file from a run view), `settings/jira`, `settings/jira-poller`, `settings/notifications` (+ `/test`, `/preview`, `/log`), `insights/rebuild`, and the two OData collections `test-results` and `test-case-results`.

### 4.7 Cross-gear amendments (Phase 0)

**qa-runs** — all additive; the shipped 778-test suite staying green is the phase gate:
* `qa_run_test_results` gains `nodeid`, `reason`, `ticket`; `qa.test.result` gains the matching optional fields (D1).
* A `Collect` run kind setting `COLLECT_ONLY` / `VHP_COLLECT_URL`, bypassing admission (D2).
* `qa_schedules` gains three notification columns, surfaced on the SDK and at `PUT /qa/v1/schedules/{id}/notifications` (D9).

**qa-catalog**:
* SDK exposure of the plan/TEST_META universe and per-file static test-function counts (D2, Finding 4).

### 4.8 Error handling

Ingestion failures log and continue rather than nacking, matching the publisher's own log-and-continue contract; the reconciler is the backstop. oagw failures on JIRA, Slack and SMTP are recorded in `qa_notification_log` with `outcome` and `detail`, exactly as legacy does, and never propagate to a caller. The notification client keeps a hard total-request timeout, per legacy's reasoning at `services/notifications.rs:52-64`. A missing or disabled JIRA config short-circuits the poller silently (`services/jira_poller.rs:40-43`).

### 4.9 Phasing

| Phase | Content |
|---|---|
| **0** | Cross-gear prerequisites (§4.7) |
| **A** | Gear + SDK + schema + event ingest + reconciler + rebuild + per-test history + dashboard + coverage |
| **B** | Analytics depth (overview's nine sections, export, build-tests, plan drill-downs), saved views, collect (static + exact) |
| **C** | JIRA registry, poller, skip-lists, auto-rerun; notifications (Slack, email, queue events, scheduled-run events, dedupe, log) |

Four separately squashable deliverables. B and C are independent of each other once A lands.

---

## 5. Testing

* **Pure cores test-first.** Every rule taken from legacy carries a legacy-verification citation in its task, per the protocol the qa-runs plan established: name the legacy file and line, state what it does, and only then write the test.
* **Numeric parity fixtures.** The overview aggregate is verified against fixture data whose expected values are computed from the legacy code path, not from the new implementation — that is the only way a subtly different flaky pass-rate or heatmap bucketing gets caught.
* **Idempotence.** Redelivering an event batch must leave `qa_test_results` and `qa_test_case_results` byte-identical.
* **Reconciler.** A run whose events were dropped entirely must be fully backfilled on the next sweep.
* **Adapter tier** against in-memory SQLite; a Postgres `integration` feature tier for ingest races and the reconciler, mirroring `make test-qa-runs-pg`.
* **Phase 0 regression gate**: `cargo test -p qa-runs` and `-p qa-catalog` green before Phase A starts.

---

## 6. Document and requirement changes

| Document | Change | Decision |
|---|---|---|
| DESIGN §3.7 | Replace the qa-insights table list with the nine tables of §4.5 | D1, D2, D4, D6 |
| DESIGN §3.7 | Record `qa_run_test_results`' three new columns and `qa_schedules`' three | D1, D9 |
| DESIGN §3.3 | Expand the qa-insights endpoint inventory to §4.6 | D3 |
| DESIGN §3.3 | Record the OData / legacy-parameter split | D7 |
| DESIGN §3.4 | Add a row: dependency gear `qa-catalog`, interface `qa-catalog-sdk`, purpose "universe and static case counts for analytics" | Finding 4 |
| DESIGN §3.5 (events) | Record the additive `qa.test.result` fields | D1 |
| PRD `cpt-cf-qa-fr-insights-dashboard` | Enumerate the nine overview sections | D3 |
| PRD `cpt-cf-qa-fr-insights-analytics` | Enumerate the endpoints; record the parameter set | D3, D7 |
| PRD `cpt-cf-qa-fr-insights-notifications` | Widen from email-on-completion to the full surface | D5 |
| PRD `cpt-cf-qa-fr-insights-auto-rerun` | Add the new-build condition | D8 |
| PRD `cpt-cf-qa-fr-insights-history` | Record the two granularities | D1 |
| PRD §5.4 | New FR for expected-case counts (static + exact collect) | D2 |
| PRD §5.2 | Record the `Collect` run kind and per-schedule notification settings | D2, D9 |
| DECOMPOSITION 2.5 | Restate scope and data to match §4.5/§4.6; add the Phase 0 dependency | D1-D4, D7 |
| DECOMPOSITION 2.8 | Restate scope to the full notification surface | D5, D6, D9 |
| ADR (new) | Consider an ADR for the reconciler, since it adds a polling loop the async-insights principle did not anticipate | §4.4 |

---

## 7. Risks and open items

| Risk | Mitigation |
|---|---|
| Touching `qa-runs`, a shipped gear with 778 tests | Phase 0 changes are additive-only (new nullable columns, new optional event fields, a new run kind); the existing suite green is the phase gate |
| No in-repo precedent for an event-broker consumer (Finding 9) | The consumer adapter gets its own task with its own integration test; the reconciler means a broken subscription degrades to delayed data rather than lost data |
| Exact collect produces nothing until feature 2.7 lands the real executor | The trigger, the run kind, the report route and the table all ship in Phase B and are exercised against the mock executor; only the real counts wait on 2.7 |
| Legacy's static case count reads the checkout directly; qa-catalog's projection may drift | The count is a pure function of file content; qa-catalog computes it during the parse it already performs, and the parity test compares against legacy's `count_test_functions` on identical fixtures |
| Analytics parameters (`product_id`, `version`, `scope`) presuppose legacy's product/version model, which VHP-319 partly deleted | Resolve against qa-catalog's shipped `products` + `repo_branches` model during Phase B task authoring; flagged here rather than guessed |
| Retention policy for `test_results` / `test_case_results` is still open (PRD §11) | Unchanged by this spec; both tables carry the indexes a future purge would need |
