# QA Platform observability (Phase 8) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the QA Platform's p95 NFRs measurable — RED metrics on the five
paths DESIGN names, emitted through typed ports.

**Architecture:** One `domain/metrics.rs` per gear declaring the metric families
as literal Prometheus names, one `domain/ports/metrics.rs` declaring **typed**
emission traits with closed label enums, and one `infra/metrics.rs`
OpenTelemetry adapter implementing them. Modelled on
`gears/system/account-management` — specifically on the typed-port half, which
that gear documents as *"the long-term API"*, and **not** on its stringly-typed
`emit_metric` facade bridge, which it documents as *"a transitional surface"*
being removed once every call site has moved. There is no legacy call site here
to migrate, so there is nothing to transition from.

**Tech Stack:** `opentelemetry`, `opentelemetry_sdk`, `tracing-opentelemetry`
(all already in the workspace — `libs/toolkit/Cargo.toml:28` has `otel` in its
default features), `toolkit_macros::domain_model`.

**Spec:** `gears/qa-platform/docs/superpowers/specs/2026-09-05-review-remediation-design.md` §10

**Prerequisites:** `2026-09-05-review-remediation-core.md` (Phases 1–4). This
plan is independent of `2026-09-05-qa-permission-catalog.md`; either order works.

## Global Constraints

- **Metric names are the full, literal Prometheus series names**, with the
  OTel→Prometheus suffix baked in: counters carry `_total`; histograms and
  quantity gauges carry the unit word (`_seconds`, `_bytes`); gauges that are
  bare counts carry no suffix. Set no `.with_unit()` hint on any instrument.
  This is what makes the rendered name identical whether the collector has
  `add_metric_suffixes` on or off — AM's `domain/metrics.rs:24-38` explains the
  mechanism and this plan follows it exactly.
- **No label accepts a free `&str`.** Every label value is a typed enum or a
  sealed newtype with `pub const` literals. This is cardinality discipline, and
  in this subsystem it is also a *security* constraint: a tenant id, a run name,
  a branch name or a repository URL as a label value is unbounded cardinality
  **and** a disclosure into the metrics pipeline. Tenant id in particular must
  not be a label.
- **Metrics must not change behaviour.** Every emission is infallible and
  silent when no adapter is installed. A failed metric write never fails a
  request, and never logs per-emission.
- **This phase adds no `unwrap`/`expect` outside tests.**
- One commit per task, on the same branch.

---

## The five paths, and why these

DESIGN's p95 NFRs are stated over these and nothing else measures them today —
`grep -rn 'metrics!\|prometheus\|opentelemetry\|histogram'` over all four gears,
both plugin crates and `qa-product-sdk` returns **zero** non-test hits.

| Path | Gear | Entry point | NFR |
|---|---|---|---|
| Dispatch | qa-runs | `domain/service/dispatch.rs` tick | queue latency |
| Ingest | qa-runs | `domain/service/ingest.rs` | result-to-visible latency |
| Collect | qa-insights | `domain/service/collect.rs` cycle | — |
| JIRA poll | qa-insights | `domain/service/jira_poller.rs::poll_once` | — |
| Observation | qa-environments | `domain/service/environments.rs::run_observation_cycle` | `cpt-cf-qa-nfr-scale` |

The two plugin crates inherit the gap and get the same treatment for their
plugin-boundary calls (Task 40).

---

### Task 36: The metric catalog and typed ports for qa-runs

qa-runs first because it owns two of the five paths and the hardest label
taxonomy, so the shape it settles is the one the other three copy.

**Files:**
- Create: `qa-runs/qa-runs/src/domain/metrics.rs`
- Create: `qa-runs/qa-runs/src/domain/ports/metrics.rs`
- Modify: `qa-runs/qa-runs/src/domain/mod.rs`, `domain/ports/mod.rs`
- Test: `qa-runs/qa-runs/src/domain/metrics_tests.rs`

**Interfaces:**
- Produces:

```rust
// domain/metrics.rs
pub const QA_RUNS_DISPATCH: &str = "qa_runs_dispatch_total";
pub const QA_RUNS_DISPATCH_DURATION: &str = "qa_runs_dispatch_duration_seconds";
pub const QA_RUNS_INGEST: &str = "qa_runs_ingest_total";
pub const QA_RUNS_INGEST_DURATION: &str = "qa_runs_ingest_duration_seconds";

// domain/ports/metrics.rs
pub trait DispatchMetrics: Send + Sync + 'static {
    fn dispatch_pass(&self, outcome: DispatchOutcome, duration: Duration);
    fn dispatch_decision(&self, decision: DispatchDecision);
}
pub trait IngestMetrics: Send + Sync + 'static {
    fn ingest_batch(&self, outcome: IngestOutcome, duration: Duration);
}
```

  Task 37 implements both on one adapter struct. Tasks 38 and 39 declare their
  own traits in the same shape.

- [ ] **Step 1: Write the failing test**

```rust
/// **Every metric constant is the literal Prometheus series name.**
///
/// The OTel→Prometheus translation adds `_total` to counters and a unit suffix
/// to instruments carrying a `.with_unit()` hint. Baking the suffix into the
/// constant and setting no unit hint makes the rendered name identical whether
/// the collector has `add_metric_suffixes` on or off -- the mechanism
/// account-management's `domain::metrics` documents at :24-38 and the reason
/// its constants look the way they do.
///
/// A name that gets a suffix added at render time is a name that does not match
/// the dashboard query written against it.
#[test]
fn counter_names_carry_the_total_suffix_and_duration_names_carry_the_unit() {
    for name in COUNTERS {
        assert!(
            name.ends_with("_total"),
            "{name} is a counter and must carry the _total the exporter would add"
        );
    }
    for name in DURATIONS {
        assert!(
            name.ends_with("_seconds"),
            "{name} measures a duration and must carry its unit"
        );
    }
}

/// **Every metric name is prefixed with its gear.**
///
/// One Prometheus instance holds all four gears; an unprefixed
/// `dispatch_total` collides.
#[test]
fn every_metric_is_namespaced_to_this_gear() {
    for name in COUNTERS.iter().chain(DURATIONS) {
        assert!(name.starts_with("qa_runs_"), "{name} is not namespaced");
    }
}

/// **No label carries a tenant id, a run name, a branch or a URL.**
///
/// Cardinality *and* disclosure: a metrics pipeline is a second copy of
/// whatever is put in a label, exported to a system with a different audience
/// from the database. This subsystem's labels are all closed enums, so this
/// asserts the property structurally -- a free `&str` label would not compile
/// against these traits, and this test documents why that is deliberate.
#[test]
fn label_taxonomies_are_closed_sets() {
    // Each `as_str` is total over a closed enum; a value outside it is
    // unconstructible. Round-tripping every variant is what proves the set is
    // closed rather than merely narrow.
    for outcome in DispatchOutcome::ALL {
        assert!(!outcome.as_str().is_empty());
    }
    for decision in DispatchDecision::ALL {
        assert!(!decision.as_str().is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p qa-runs counter_names_carry_the_total_suffix
```

Expected: FAIL to compile — nothing exists yet.

- [ ] **Step 3: Write the catalog**

```rust
//! qa-runs observability metric catalog.
//!
//! DESIGN's p95 NFRs are stated over dispatch and ingest, and until this module
//! existed nothing measured either: `grep -rn 'metrics!|prometheus|
//! opentelemetry|histogram'` over all four qa-platform gears returned zero
//! non-test hits. The NFRs were not badly observed, they were unobservable.
//! Review finding #4.
//!
//! # Metric naming
//!
//! These constants are the **full, literal Prometheus series names** — what
//! appears in Prometheus / VictoriaMetrics. They bake in the suffix the
//! OTel→Prometheus translation would otherwise add: counters carry `_total`,
//! histograms carry the unit word. No `.with_unit()` hint is set on any
//! instrument, so the rendered name is identical whether the collector has
//! `add_metric_suffixes` on or off. `account-management`'s `domain::metrics`
//! documents this mechanism at :24-38; this follows it.
//!
//! # Why typed ports and not an `emit_metric(&str, …)` facade
//!
//! `account-management` has both. Its facade bridge is documented there as *"a
//! transitional surface"* whose typed ports are *"the long-term API"*, and it
//! exists because that gear had call sites predating the ports. This gear has
//! none: there is nothing to transition from, so it starts where AM is going.
//! See `crate::domain::ports::metrics`.
```

- [ ] **Step 4: Write the typed ports**

Follow `account-management/src/domain/ports/metrics.rs`'s four design choices,
which its header states: trait segregation (narrow traits, one adapter),
typed label values, bridging existing failure types via `From<&Failure>` rather
than duplicating the variant→string mapping, and no free `&str` labels.

For qa-runs specifically, derive `DispatchOutcome` and `IngestOutcome` from the
existing domain enums rather than inventing parallel ones — `RunState`,
`ExclusiveTier` and `DomainError`'s disclosure classification
(`domain/error.rs:363-374`) already partition the outcome space, and a second
partition that must agree with the first is drift waiting to happen.

**Do not label by tenant.** If per-tenant attribution is genuinely needed later,
that is a trace attribute, not a metric label, and it is a decision with a
disclosure question attached.

- [ ] **Step 5: Run to verify it passes**

```bash
cargo nextest run -p qa-runs --lib
```

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "feat(qa-runs): metric catalog and typed emission ports

DESIGN's p95 NFRs are stated over dispatch and ingest and nothing measured
either: grep for metrics!/prometheus/opentelemetry/histogram over all four
qa-platform gears returned zero non-test hits. The NFRs were unobservable, not
badly observed.

Names are the literal Prometheus series names with the OTel suffix baked in and
no unit hint set, so the rendered name does not depend on the collector's
add_metric_suffixes setting. Labels are closed enums derived from the existing
domain enums -- no free &str, and no tenant id: a metrics pipeline is a second
copy of whatever goes in a label, exported to a different audience than the
database.

Typed ports rather than a stringly-typed facade: account-management has both and
documents the facade as transitional and the ports as the long-term API. This
gear has no call sites to transition, so it starts there. Review finding #4."
```

---

### Task 37: The OpenTelemetry adapter and the dispatch/ingest call sites

**Files:**
- Create: `qa-runs/qa-runs/src/infra/metrics.rs`
- Create: `qa-runs/qa-runs/src/infra/metrics_tests.rs`
- Modify: `qa-runs/qa-runs/src/infra/mod.rs`, `gear.rs` (install at init)
- Modify: `qa-runs/qa-runs/src/domain/service/dispatch.rs`, `ingest.rs`
- Modify: `qa-runs/qa-runs/Cargo.toml`

**Interfaces:**
- Consumes: Task 36's traits and constants.
- Produces: `QaRunsMetricsMeter` implementing both traits;
  `build_default_adapter() -> Arc<QaRunsMetricsMeter>` (AM's
  `infra/metrics.rs:454` is the precedent for the name).

- [ ] **Step 1: Write the failing test**

```rust
/// **A dispatch pass emits exactly one counter increment and one duration.**
///
/// Driven through the real OTel SDK with an in-memory exporter, not a mock:
/// what this needs to prove is that the *rendered series* is what a dashboard
/// query will find, and a mock of the trait proves only that the call site
/// calls the trait.
#[tokio::test]
async fn a_dispatch_pass_records_one_observation() {
    let (meter, exporter) = in_memory_meter();
    let svc = dispatch_service_with_metrics(Arc::new(meter));

    svc.tick().await;

    let series = exporter.collect();
    assert_eq!(
        series.counter(QA_RUNS_DISPATCH), 1,
        "one pass, one increment; series were {series:#?}"
    );
    assert_eq!(series.histogram_count(QA_RUNS_DISPATCH_DURATION), 1);
}

/// **A failing dispatch pass still records, with a failure outcome.**
///
/// The RED half that is easy to omit: a metric that only counts successes tells
/// an operator the rate and hides the errors.
#[tokio::test]
async fn a_failing_dispatch_pass_records_a_failure_outcome() {
    let (meter, exporter) = in_memory_meter();
    let svc = dispatch_service_with_metrics(Arc::new(meter));
    svc.fail_next_pass();

    svc.tick().await;

    assert_eq!(
        exporter.collect().counter_with(QA_RUNS_DISPATCH, &[("outcome", "failed")]),
        1
    );
}

/// **A metric emission never fails a request.**
///
/// Metrics are diagnostics. An adapter that panics or errors must not take the
/// dispatch pass with it, and this pins that the emission path is infallible.
#[tokio::test]
async fn a_broken_metrics_adapter_does_not_fail_the_pass() {
    let svc = dispatch_service_with_metrics(Arc::new(PanickingMeter));
    svc.tick().await; // must not panic
}
```

`opentelemetry_sdk` is already available with its `testing` feature
(`libs/toolkit-http/Cargo.toml:116` uses it), so the in-memory exporter needs no
new dependency.

- [ ] **Step 2: Run to verify they fail**

```bash
cargo nextest run -p qa-runs a_dispatch_pass_records_one_observation
```

Expected: FAIL to compile.

- [ ] **Step 3: Write the adapter**

One struct implementing both traits, instruments built once in `new(meter,
prefix)` and held — not looked up per emission. AM's `infra/metrics.rs:61-98` is
the shape.

Make every emission infallible: no `?`, no panic path. If the third test does
not pass with a deliberately panicking meter, wrap the emission or make the
trait's contract explicit that implementations must not panic — and say which
you chose and why.

- [ ] **Step 4: Instrument dispatch and ingest**

At the boundaries, not inside the loops. `dispatch.rs`'s tick and `ingest.rs`'s
batch are the units the NFRs are stated over; per-run emissions inside them
would multiply cardinality by the run count for no dashboard's benefit.

- [ ] **Step 5: Install at gear init**

In `gear.rs`, beside the other adapter construction. **When no OTel pipeline is
configured, the gear must still start** and the emissions must be silent no-ops
— AM's `domain/metrics.rs:13-17` states that posture and this must match it. Add
a test that a gear built with no meter serves normally.

- [ ] **Step 6: Run everything**

```bash
cargo nextest run -p qa-runs --lib
make test-qa-runs-pg test-qa-platform-features
```

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "feat(qa-runs): OTel metrics adapter, dispatch and ingest instrumented

RED on the two paths DESIGN states p95 NFRs over. Instruments are built once and
held, emission is infallible, and a gear with no OTel pipeline configured starts
normally with silent no-op emissions.

Instrumented at the tick and batch boundaries, not inside the loops: those are
the units the NFRs are stated over, and a per-run emission would multiply
cardinality by the run count for no dashboard's benefit.

Driven through the real OTel SDK with an in-memory exporter rather than a trait
mock -- what needs proving is that the rendered series is what a dashboard query
finds, and a mock proves only that the call site calls the trait.
Review finding #4."
```

---

### Task 38: Collect and the JIRA poll

**Files:**
- Create: `qa-insights/qa-insights/src/domain/metrics.rs`, `domain/ports/metrics.rs`, `infra/metrics.rs` (+ tests)
- Modify: `qa-insights/qa-insights/src/domain/service/collect.rs`, `jira_poller.rs`, `gear.rs`, `Cargo.toml`

**Interfaces:**
- Consumes: Task 36's established shape. Copy it; do not redesign.
- Produces: `QA_INSIGHTS_COLLECT*`, `QA_INSIGHTS_JIRA_POLL*` and their traits.

- [ ] **Steps 1–6: As Tasks 36 and 37** (catalog → typed ports → failing test → OTel adapter → instrument the call sites → install at gear init)

Three things specific to this gear:

- **The JIRA poller's per-bug failures are swallowed by design**
  (`jira_poller.rs:107-118`: *"Every per-bug failure is logged and skipped,
  never a whole-pass error"*, which is the source system's behaviour). That
  makes a per-bug **outcome counter** the only way an operator can see them at
  all — a pass that silently skipped forty bugs currently looks identical to a
  clean one. This is the highest-value metric in the plan; give it its own
  counter with a typed failure-class label, not just a pass-level one.
- **The auto-rerun is a launch.** Count it separately from the poll. A rerun
  storm is what Task 26 of the quality plan (#5, the claim-row) exists to
  prevent, and a counter is how anyone would notice one.
- **Collect's public HMAC route** should count verification failures by class —
  a rising `signature_invalid` rate is either a misconfigured runner or an
  attack, and neither is visible today.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-insights
git commit -m "feat(qa-insights): metrics for collect and the JIRA poll

The poller's per-bug failures are swallowed by design -- logged and skipped,
never a whole-pass error, matching the source system -- so a pass that silently
skipped forty bugs looked identical to a clean one. A per-bug outcome counter
with a typed failure-class label is the only way an operator sees them.

The auto-rerun is counted separately: it is a launch, and a rerun storm is what
the JIRA poller's claim-row exists to prevent.

Collect's HMAC verification failures are counted by class -- a rising
signature_invalid rate is a misconfigured runner or an attack, and neither was
visible. Review finding #4."
```

---

### Task 39: Observation

**Files:**
- Create: `qa-environments/qa-environments/src/domain/metrics.rs`, `domain/ports/metrics.rs`, `infra/metrics.rs` (+ tests)
- Modify: `qa-environments/qa-environments/src/domain/service/environments.rs:998`, `gear.rs`, `Cargo.toml`

**Interfaces:**
- Produces: `QA_ENVIRONMENTS_OBSERVATION*` and its trait.

- [ ] **Steps 1–6: As Tasks 36 and 37** (catalog → typed ports → failing test → OTel adapter → instrument the call sites → install at gear init)

`run_observation_cycle` already returns an `ObservationCycleReport` with
`attempted` / `observed` / `failed` (`environments.rs:1077-1108`) and logs it at
`debug!`. That struct **is** the metric: emit from it rather than adding
counters inside the loop, so the log line and the series cannot disagree.

Two specifics:

- The cycle is where `cpt-cf-qa-nfr-scale`'s "100 platforms" is felt, so the
  **per-environment** duration histogram is the one that matters — a cycle
  duration alone cannot distinguish one slow cluster from a hundred slightly
  slow ones.
- Do not label by environment id or name. Label by the plugin's failure class
  (`PluginFailure`'s `FailureClass`, `qa-connector-k8s/src/errors.rs`), which is
  already a closed set and already the taxonomy the gear reasons in.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): metrics for the observation cycle

Emitted from the existing ObservationCycleReport rather than from counters
inside the loop, so the debug log line and the series cannot disagree.

The per-environment duration histogram is the one cpt-cf-qa-nfr-scale needs: a
cycle-level duration cannot tell one unreachable cluster from a hundred slightly
slow ones. Labelled by PluginFailure's FailureClass, which is already a closed
set and already the taxonomy this gear reasons in -- never by environment id or
name. Review finding #4."
```

---

### Task 40: The plugin boundary

The three crates the rework added (`qa-product-sdk`, `connectors/qa-connector-k8s`,
`plugins/qa-vhp-product-plugin`) inherit #4 and nothing else — the review's
classes are otherwise clean there.

**Files:**
- Modify: `qa-product-sdk/src/plugin.rs` (the trait, if the timing belongs there)
- Modify: `qa-environments/qa-environments/src/domain/service/environments.rs`, `qa-catalog/qa-catalog/src/domain/service/plugin_registry.rs`

**Interfaces:**
- Consumes: Task 39's `qa-environments` port for the observation side, Task 36's
  shape for qa-catalog's resolution side.

- [ ] **Step 1: Decide where the timing lives, and say why**

Two options, and this is a real design choice rather than a detail:

**(a) Instrument the callers.** `plugin_registry.rs:184`'s `plugin_for` and
`environments.rs`'s `observe_environment` time the call and emit. The plugin
crates stay metric-free, which keeps `qa-product-sdk`'s dependency surface as
narrow as its own `test-util` doc argues for.

**(b) Instrument the plugins.** Each plugin emits its own. More faithful per
plugin, at the cost of putting an observability dependency into the SDK every
plugin author must then satisfy.

**Recommendation: (a).** The measurement a dashboard needs is "how long does
*this deployment's* plugin take and how often does it fail", which the caller
can answer completely; and `qa-product-sdk`'s existing feature docs are explicit
that the crate gates its own code rather than widening the dependency graph.
Write the reasoning into the port's doc either way.

- [ ] **Step 2: Write the failing test**

```rust
/// **A plugin call is timed and its failure class is counted.**
///
/// The plugin boundary is where a QA Platform deployment meets code it does not
/// own. "The VHP plugin's detect is timing out" and "qa-environments is slow"
/// are different pages for an operator, and without this they are the same
/// series. Review finding #4.
#[tokio::test]
async fn a_plugin_observation_is_timed_and_classified() {
    let (meter, exporter) = in_memory_meter();
    let svc = env_service_with_metrics(Arc::new(meter));
    svc.plugin.fail_next(FailureClass::Unreachable);

    svc.observe_environment(&ctx(TENANT), env_id).await.ok();

    let series = exporter.collect();
    assert_eq!(
        series.counter_with(QA_ENVIRONMENTS_PLUGIN_CALL, &[("class", "unreachable")]),
        1
    );
    assert_eq!(series.histogram_count(QA_ENVIRONMENTS_PLUGIN_CALL_DURATION), 1);
}
```

- [ ] **Step 3: Run red, implement, run green**

```bash
cargo nextest run -p qa-environments -p qa-catalog --lib
```

- [ ] **Step 4: Label by plugin kind, not by instance**

The plugin's GTS type id is a closed set in any deployment; the *instance* id is
per-product and unbounded. Label by the first.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform
git commit -m "feat(qa-platform): time and classify plugin-boundary calls

The plugin boundary is where a deployment meets code it does not own, and
'the VHP plugin's detect is timing out' and 'qa-environments is slow' were the
same series.

Instrumented at the callers -- plugin_registry::plugin_for and
observe_environment -- rather than inside the plugin crates: the measurement a
dashboard needs is answerable from the caller, and qa-product-sdk's own feature
docs argue against widening its dependency graph for something the platform can
observe from outside.

Labelled by plugin GTS type id, which is a closed set, never by instance id,
which is per-product and unbounded. Review finding #4."
```

---

### Task 41: Wire the metrics into the deployment and write them down

A metric nothing scrapes is the same shape of defect as a test nothing runs —
which is Phase 1's whole subject.

**Files:**
- Modify: `gears/qa-platform/deploy/helm/qa-platform/templates/gears-deployment.yaml`, `values.yaml`
- Modify: `gears/qa-platform/deploy/helm/tests/test_features.py` (or a new guard)
- Modify: `gears/qa-platform/docs/DESIGN.md` (the metric catalog section)

- [ ] **Step 1: Confirm the exporter is reachable**

Check how other gears in this repo expose their OTel pipeline —
`gears/system/account-management`'s Helm chart or the compose stack. Follow it;
do not invent a second convention.

- [ ] **Step 2: Add a chart guard**

```python
def test_metrics_port_is_exposed():
    """A metric nothing scrapes is a metric that does not exist.

    The gears Deployment must expose the metrics port and carry the scrape
    annotations, or the whole of Phase 8 renders no series in any dashboard.
    Review finding #4.
    """
```

This runs under `make helm-tests`, wired by Phase 1 Task 4.

- [ ] **Step 3: Document the catalog in DESIGN.md**

One table: series name, type, labels, and the NFR each supports. An operator
writing a dashboard query reads this and nothing else.

- [ ] **Step 4: Verify end to end against the compose stack**

```bash
cd gears/qa-platform/deploy/compose && docker compose up -d
# drive a run, then:
curl -s localhost:<metrics-port>/metrics | grep -c '^qa_'
```

Expected: non-zero, and the names must match the constants exactly. **A
mismatch here is the `add_metric_suffixes` trap Task 36's test exists to
prevent** — if the rendered name differs from the constant, fix the constant,
not the dashboard.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform
git commit -m "feat(qa-platform): expose the metrics port and document the catalog

A metric nothing scrapes is the same defect as a test nothing runs, which is
what Phase 1 was about. The gears Deployment now exposes the metrics port with
scrape annotations, guarded by a chart test, and DESIGN.md carries the catalog:
series name, type, labels and the NFR each supports.

Verified end to end against the compose stack -- the rendered series names match
the constants exactly, which is the add_metric_suffixes trap the naming rule
exists to prevent. Review finding #4."
```

---

## Phase completion

```bash
make fmt clippy
make test-no-macros
make test-qa-runs-pg test-qa-insights-pg test-qa-catalog-git test-qa-platform-features
make helm-tests ui-lint ui-test ui-build
```

With this and the permission catalog landed, all 53 live findings from
`docs/Reviews/qa-platform-review-findings.md` are closed. Update that file's
status header to say so, naming the commit range, so the next reader sees the
current state rather than the state at filing time.
