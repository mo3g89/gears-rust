//! The golden [`RunSpec`]: exactly what today's dispatcher produces for a
//! representative VHP run, recorded **before** the product-plugin migration
//! moves where any of those values comes from.
//!
//! Product-plugin plan Task 16, the hard gate. Its whole value is that it was
//! recorded first: Tasks 17-19 replace the inline platform read in
//! [`super::dispatch_spec`] with three plugin calls, and the only evidence that
//! the replacement was *faithful* is a spec that did not move while its source
//! did.
//!
//! # Why this is an in-lib test and not `tests/golden_run_spec.rs`
//!
//! The plan names `qa-runs/qa-runs/tests/golden_run_spec.rs`, and that file
//! cannot exist: an integration test compiles the library without `cfg(test)`
//! and can only reach its public surface, while `build_spec` is a private
//! method on a service in a `pub(crate) mod` (`service::mod`'s own docs record
//! that those module declarations *are* the visibility enforcement) and every
//! double it needs — `admission::tests::fakes`, `test_support` — is
//! `#[cfg(test)]`. This crate already records the same conclusion for the same
//! reason: `Cargo.toml`'s `integration` feature comment says its tier lives
//! in-lib "because they need `pub(crate)` services and the `#[cfg(test)]`
//! fixtures". The **fixture** keeps the plan's path exactly
//! (`tests/fixtures/vhp_run_spec.json`), read through `CARGO_MANIFEST_DIR`.
//!
//! # Recording it
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo test -p qa-runs -j 6 golden_run_spec
//! ```
//!
//! That run **fails on purpose** once it has written the fixture: recording and
//! asserting in the same run would compare the file against the bytes just
//! written to it. Inspect the diff, then re-run without `UPDATE_GOLDEN`.
//!
//! **Do not re-record it to make a later task pass.** A failure here after a
//! plugin change means the plugin changed behaviour; re-recording discards the
//! only evidence the migration was faithful, and the fixture is the guard for
//! the whole of Phases E, F and G.
//!
//! # The one line that changed, and the two findings behind it
//!
//! Task 18 switched this test from the inline platform read to the **real**
//! `qa-vhp-product-plugin`, and the recorded spec moved by exactly one field:
//! the mount's `mode`, from absent to `256` (`0o400`). Every variable, every
//! value, the credstore reference, the mount path, both nodes and the runner
//! are the same bytes they were recorded as.
//!
//! Two things had to be settled for that to be true, and this test is what
//! found both:
//!
//! * **The mount path.** The plugin mounted the kubeconfig at
//!   `/etc/qa/kubeconfig` — the plan prescribed it and Task 10 implemented it —
//!   while `qa-runs` has always mounted it at `/.kube/kubeconfig`, the source
//!   system's own pair. `KUBECONFIG` is a value the *runner* reads, so the
//!   shipped spelling won and the plugin's constant changed. Nothing about this
//!   fixture moved as a result, which is the point.
//! * **The mode.** `0o400` is the plugin's, and it is kept: an owner-read-only
//!   kubeconfig is strictly better than Kubernetes' default, and it changes no
//!   variable, no value and no precedence — only the permissions of a file in
//!   the run's own filesystem. So this one field was re-recorded, deliberately,
//!   with the change named here rather than absorbed silently. **That is the
//!   only re-recording this fixture has had or may have.**
//!
//! # What is in the fixture, and what the spec deliberately leaves out
//!
//! One run exercising every branch `build_spec` has: a target environment with
//! a kubeconfig credstore reference, an observed base URL, an observed
//! namespace, one global pipeline variable, one environment-scoped variable,
//! one run parameter shadowing that variable, a file grouping spanning two
//! repositories, and a non-empty `app_version`/`app_build`.
//!
//! `TEST_FILES` and `TEST_BUNDLE_URL` are **not** environment entries, which is
//! the port's shape rather than a gap in this fixture: the source system has
//! one bundle and one file list per *node*, so `ExecutionNode`'s `test_files`
//! and `bundle_ref` carry them and the shared map would be a second copy that
//! disagrees whenever there is more than one group (`build_spec`'s own doc
//! says so). Both are pinned here — per node, where they live.
//!
//! Two honest limits, recorded rather than papered over:
//!
//! * **Both nodes carry the same `bundle_ref`**, because `FakeCatalog` mints one
//!   storage reference per build. So the fixture pins that each node has a
//!   bundle and which files went with it, not that two nodes' references
//!   differ. Nothing in Tasks 17-19 touches the grouping or the bundle build,
//!   so the guard this test exists to be is unaffected.
//! * **`timeout_seconds` is `0`** because the run carries no `timeout_at`. A
//!   deadline would be re-derived from `now()` on every run of the test, which
//!   is the one field a byte-identical fixture cannot hold. `executor_deadline`
//!   has its own unit coverage.
//! * **This gate stops at the `RunSpec`, one layer short of the artifact.**
//!   It asserts what `build_spec` handed the executor, never the Argo
//!   `Workflow` object the executor renders — and Task 17 rewrote ~440 lines of
//!   `infra::executor::argo::workflow`, where the one rendered value that did
//!   change lives (the volume name, now an ordinal). So "the fixture is
//!   byte-identical, therefore the migration was faithful" is a claim about
//!   dispatch, not about the object a cluster receives. `workflow`'s own
//!   field-by-field tests are what cover that half; added at the Phase E
//!   review (finding I-8) so the next reader does not over-trust this one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use qa_environments_sdk::{
    Environment, EnvironmentCredential, HealthState, ObservedAttrs, Variable,
};
use qa_runs_sdk::{RunParameter, RunState, RunTarget};
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::ports::run_executor::{EnvSource, MountSpec, RunSpec};
use crate::domain::service::admission::tests::fakes::{
    self, Builder, FakeCatalog, FakeEnvironments, FakeProductPlugins, FakeRuns, PLATFORM_A,
    run_fixture,
};
use crate::domain::service::test_support::{OWNER_TENANT, ctx};

/// The recorded spec. Relative to the crate root, so the plan's path is kept
/// even though the test itself moved in-lib.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/vhp_run_spec.json"
);

const GOLDEN_RUN: Uuid = Uuid::from_u128(0x1601);
const CUSTOM_PLAN: Uuid = Uuid::from_u128(0x1602);
/// Two repositories, so the grouping is observably a grouping. `ALPHA` sorts
/// below `BETA`, which is the node order `group_files_by_repo`'s `BTreeMap`
/// produces.
const REPO_ALPHA: Uuid = Uuid::from_u128(0x0B01);
const REPO_BETA: Uuid = Uuid::from_u128(0x0B02);
/// An observed VHP environment: the state the platform-observation cycle leaves
/// behind, not the never-observed default the other dispatch tests use.
///
/// `vhp_base_url` and `observed_base_url` carry the same value, and
/// `observed_attrs` carries it a third time under the key the VHP plugin's
/// `observed_schema()` declares for `FieldRole::BaseUrl`. That is not
/// redundancy in the fixture — it is what Task 15's dual-write guarantees on a
/// real row, and it is what lets Task 18 read the value from the plugin's
/// projection and still produce this same spec.
fn observed_environment() -> Environment {
    let stamp = OffsetDateTime::from_unix_timestamp(1_756_900_000).unwrap();
    let mut observed_attrs = ObservedAttrs::default();
    observed_attrs.set("baseDomain", "https://sv.jele.io");
    observed_attrs.set("namespace", "vhp-sv");
    // Deliberately **not** the run's `app_version`/`app_build`. `runvars`'
    // composition obligation 2 is that `APP_VERSION` and `APP_BUILD` come from
    // the run's own snapshotted columns rather than a live platform lookup, and
    // a fixture where the two agree cannot tell the two sources apart — a
    // dispatcher that re-derived them from the platform would leave this
    // fixture byte-identical. These are the values the platform has been
    // upgraded to since the run was launched.
    observed_attrs.set("platformVersion", "7.2.0");
    observed_attrs.set("build", "b9004");
    Environment {
        id: PLATFORM_A,
        name: "sv-staging".to_owned(),
        // The product the resolver double answers for. Task 18 resolves the
        // plugin from this field, and the plugin it resolves to is the **real**
        // `VhpProductPlugin`, not a double — see the dispatch below.
        product_id: fakes::PRODUCT,
        description: None,
        available: true,
        observed_version: Some("7.2.0".to_owned()),
        observed_build: Some("b9004".to_owned()),
        default_branch: None,
        is_default: false,
        version_detect_error: None,
        version_detected_at: Some(stamp),
        credentials: vec![EnvironmentCredential {
            key: "kubeconfig".to_owned(),
            credstore_ref: "credstore://qa/environments/sv-staging/kubeconfig".to_owned(),
        }],
        observed_attrs,
        config: json!({ "vpadm_namespace": "virtuozzo" }),
        observed_base_url: Some("https://sv.jele.io".to_owned()),
        health_state: HealthState::Ok,
        health_detail: None,
        health_checked_at: Some(stamp),
        created_at: stamp,
        updated_at: stamp,
    }
}

/// Both qa-environments tiers, one row each.
///
/// `E2E_RETRIES` is environment-scoped **and** shadowed by a run parameter
/// below, which is the ladder's most consequential rung: a parameter that
/// stopped overriding an environment variable would silently run the wrong
/// configuration.
fn variables() -> Vec<Variable> {
    vec![
        Variable {
            id: Uuid::from_u128(0x0E01),
            environment_id: None,
            name: "PIPELINE_LOG_LEVEL".to_owned(),
            value: "info".to_owned(),
        },
        Variable {
            id: Uuid::from_u128(0x0E02),
            environment_id: Some(PLATFORM_A),
            name: "E2E_RETRIES".to_owned(),
            value: "2".to_owned(),
        },
    ]
}

/// The run: a custom plan spanning two repositories, a snapshotted
/// version/build pair, and the shadowing parameter.
///
/// `app_version`/`app_build` differ from the environment's `observed_*` pair on
/// purpose — see the note in [`observed_environment`].
fn golden_run() -> qa_runs_sdk::Run {
    let mut run = run_fixture(GOLDEN_RUN, Some(PLATFORM_A), false, RunState::Dispatching);
    run.name = "vhp-golden".to_owned();
    run.target = RunTarget::CustomPlan { id: CUSTOM_PLAN };
    run.test_version = Some("main".to_owned());
    run.app_version = Some("7.1.2".to_owned());
    run.app_build = Some("b4217".to_owned());
    run.parameters = vec![RunParameter {
        name: "E2E_RETRIES".to_owned(),
        value: "5".to_owned(),
    }];
    run
}

/// The spec as JSON, keys sorted at every level.
///
/// `serde_json`'s object is a `BTreeMap` in this workspace (`preserve_order` is
/// off), and both [`RunSpec::env`] and the map behind it are already
/// `BTreeMap`s, so ordering is a property of the types rather than of this
/// function. The env entry keeps its **variant**: a literal renders as
/// `{"value": …}` and a secret binding as `{"secret": …}`, so a value silently
/// replacing a reference — the direction `RunEnv::new` documents at length —
/// changes the fixture instead of hiding in it.
///
/// # The access half is projected by *effect*, not by the port's field names
///
/// Task 17 replaces `RunSpec::kubeconfig` with a `RunAccess` carrying
/// `Vec<MountSpec>`, a `RunnerSpec` and a service account, and Task 18 changes
/// where those come from. If this function named today's fields, every one of
/// those tasks would have to edit the fixture to keep compiling — and a fixture
/// that must be rewritten during the migration cannot be evidence *about* the
/// migration.
///
/// So the three access keys below say what the execution plane will actually
/// do, in terms both the old and the new port map onto:
///
/// * `mounts` — one entry per mounted credential: variant, credstore
///   reference, container path, file mode. Before Task 17 this port's
///   `KubeconfigMount` was exactly one `secret` mount with no mode.
/// * `service_account` — the account the run's pod assumes. Today the port has
///   no channel for one, and the Argo adapter takes it from deployment config,
///   so per-run it is null.
/// * `runner` — the per-run runner override. Today there is none: every run
///   inherits `qa-runs.argo.runner_image`/`runner_command`, which is what an
///   all-empty `RunnerSpec` means.
///
/// This cuts both ways on purpose. Moving a value's *source* leaves the fixture
/// alone; changing the value — a different mount path, a stricter mode, a
/// per-run image — changes it and fails the test. `RunAccess::env` is
/// deliberately **not** projected: it is a source channel whose entire
/// observable effect is already in `env` above, where a variable the plugin
/// added, dropped or altered shows up as a changed key or value.
fn spec_as_json(spec: &RunSpec) -> Value {
    let env: Map<String, Value> = spec
        .env
        .entries()
        .iter()
        .map(|(name, source)| {
            let rendered = match source {
                EnvSource::Value(value) => json!({ "value": value }),
                EnvSource::Secret(reference) => json!({ "secret": reference.as_str() }),
            };
            (name.clone(), rendered)
        })
        .collect();
    let nodes: Vec<Value> = spec
        .nodes
        .iter()
        .map(|node| {
            json!({
                "name": node.name,
                "bundle_ref": node.bundle_ref,
                "test_files": node.test_files,
            })
        })
        .collect();
    let mounts: Vec<Value> = spec
        .access
        .mounts
        .iter()
        .map(|mount| match mount {
            MountSpec::Secret {
                credstore_ref,
                path,
                mode,
            } => json!({
                "kind": "secret",
                "credstore_ref": credstore_ref,
                "path": path,
                "mode": mode,
            }),
            // The value is **never** rendered, here least of all: this string
            // is written to a file in the repository. A `ConfigValue` is a
            // plugin-resolved credential, and what the fixture needs to pin is
            // that one was mounted, where, and with what permissions.
            MountSpec::ConfigValue { path, mode, .. } => json!({
                "kind": "config_value",
                "value": "<redacted>",
                "path": path,
                "mode": mode,
            }),
        })
        .collect();
    json!({
        "run_id": spec.run_id.to_string(),
        "run_name": spec.run_name,
        "timeout_seconds": spec.timeout_seconds,
        "nodes": nodes,
        "env": Value::Object(env),
        "mounts": mounts,
        "service_account": spec.access.service_account,
        "runner": json!({
            "image": spec.runner.image,
            "command": spec.runner.command,
            "image_pull_policy": spec.runner.image_pull_policy,
        }),
    })
}

/// A line-oriented diff of two pretty renderings, so a failure names the line
/// that moved instead of printing two 60-line blobs for a human to align.
fn line_diff(expected: &str, actual: &str) -> String {
    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    let mut report: Vec<String> = Vec::new();
    for index in 0..expected.len().max(actual.len()) {
        match (expected.get(index), actual.get(index)) {
            (Some(left), Some(right)) if left == right => {}
            (left, right) => {
                report.extend(left.map(|line| format!("-{line}")));
                report.extend(right.map(|line| format!("+{line}")));
            }
        }
    }
    report.join("\n")
}

/// Freezes the exact `RunSpec` today's code produces for a representative VHP
/// run. Tasks 17-19 move where every one of these values comes from; none of
/// them may change what it *is*.
///
/// If this test fails after a plugin change, the plugin changed behaviour.
/// Do not re-record the fixture to make it pass — that discards the only
/// evidence that the migration was faithful.
#[tokio::test]
async fn a_vhp_run_spec_is_byte_identical_to_the_recorded_fixture() {
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let catalog = Arc::new(FakeCatalog::serving(&[]));
    catalog.custom_plan_files.lock().unwrap().extend([
        (REPO_ALPHA, "tests/test_login.py".to_owned()),
        (REPO_ALPHA, "tests/test_billing.py".to_owned()),
        (REPO_BETA, "tests/api/test_tokens.py".to_owned()),
    ]);
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, golden_run())])))
        .environments(Arc::new(FakeEnvironments::serving(
            observed_environment(),
            variables(),
        )))
        // **The real plugin, not a double.** Everything this fixture claims is
        // a claim about `qa-vhp-product-plugin`'s behaviour — its four variable
        // names, its derived base domain, its mount path and mode — and a
        // double reproducing those would let the test pass by agreeing with
        // itself. The dev-dependency that makes this possible is justified in
        // this crate's `Cargo.toml`.
        .product_plugins(Arc::new(FakeProductPlugins::with(Arc::new(
            qa_vhp_product_plugin::VhpProductPlugin,
        ))))
        .catalog(catalog)
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), GOLDEN_RUN, None)
        .await
        .unwrap();

    let submitted = executor.submitted();
    assert_eq!(submitted.len(), 1, "premise: the run reached the executor");
    let actual = serde_json::to_string_pretty(&spec_as_json(&submitted[0])).unwrap();

    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(FIXTURE, format!("{actual}\n")).unwrap();
        // Recording and asserting in one run would compare the fixture against
        // the bytes just written to it -- a guaranteed pass, and the branch's
        // headline parity evidence silently turned into a no-op (Phase E
        // review, M-10). Stop here so the comparison only ever runs against a
        // fixture a human has looked at.
        panic!(
            "recorded {FIXTURE}; inspect the diff by eye, then re-run WITHOUT \
             UPDATE_GOLDEN to let this test actually compare"
        );
    }

    let expected = std::fs::read_to_string(FIXTURE).unwrap_or_else(|error| {
        panic!(
            "the golden fixture at {FIXTURE} could not be read ({error}); record it \
             with UPDATE_GOLDEN=1 and then inspect it by eye before committing"
        )
    });
    let expected = expected.trim_end();

    assert!(
        expected == actual,
        "the RunSpec no longer matches the fixture recorded before the product-plugin \
         migration. If a plugin change caused this, the plugin changed behaviour - find \
         out why. Do NOT re-record the fixture to make this pass.\n\n{}",
        line_diff(expected, &actual)
    );
}
