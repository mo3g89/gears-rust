//! [`RunSpec`] → an Argo `Workflow` object, as a pure function.
//!
//! Pure on purpose: this is the half of the adapter with the most behaviour and
//! the least need for a cluster, so it is a `serde_json::Value` builder with
//! golden tests rather than something only an end-to-end run can falsify.
//!
//! The JSON shape is the source system's, and the citations below are to the
//! lines it is copied from (`manager/src/services/argo.rs`). What is **not**
//! copied is listed in [`super`]'s docs — the `vhp-tests/*` annotation set,
//! `VHP_PROGRESS_URL`, host aliases, per-node timeouts, `depends` and the
//! parallelism cap — each because the port deliberately does not carry it.

use serde_json::{Value, json};

use crate::config::ArgoExecutorConfig;
use crate::domain::ports::run_executor::{EnvSource, ExecutionNode, RunSpec};
use crate::infra::executor::argo::naming::{label_value, secret_name, task_name};

/// Label every workflow this adapter submits carries, and the selector
/// [`list_active`](crate::domain::ports::run_executor::RunExecutor::list_active)
/// uses.
///
/// The source system's equivalent is `app=vhp-tests` (`argo.rs:639`). A
/// different value on purpose: a cluster can host both systems, and a selector
/// that matched the source system's workflows would make this gear's
/// `list_active` claim other runs are alive.
pub const APP_LABEL: &str = "app";
/// [`APP_LABEL`]'s value.
pub const APP_LABEL_VALUE: &str = "qa-runs";
/// Label carrying [`RunSpec::run_id`], for an operator correlating a workflow
/// back to a row. Not read by this adapter — the `qa_runs` row is the system of
/// record (`cpt-cf-qa-principle-db-first-state`).
pub const RUN_ID_LABEL: &str = "qa-runs/run-id";
/// Pod annotation carrying [`ExecutionNode::name`] **verbatim**.
///
/// # Why an annotation rather than a naming convention
///
/// `watch` is handed nothing but an
/// [`ExecutionRef`](crate::domain::ports::run_executor::ExecutionRef) — no
/// `RunSpec` — so on re-attach after a control-plane restart it cannot know the
/// node names the run was submitted with. The template name is a *sanitised*
/// derivative and cannot be inverted. So the un-sanitised name travels on the
/// object, and Argo copies a template's `metadata.annotations` onto its pod,
/// which is where `watch` reads it back from.
///
/// This is executor-internal correlation data, not run metadata: it is the one
/// thing the workflow object holds that the `qa_runs` row does not, which is
/// why it does not fall foul of the port's refusal of the `vhp-tests/*`
/// annotation set (`run_executor.rs:90-96`).
pub const NODE_ANNOTATION: &str = "qa-runs/node";

/// The runner control variable carrying a node's file list
/// (`argo.rs:436`).
const TEST_FILES_VAR: &str = "TEST_FILES";
/// The bundle handle, verbatim as
/// [`ExecutionNode::bundle_ref`] spells it.
///
/// Not a source-system variable: there, the bundle was always an HTTP URL
/// (`argo.rs:64-66`). Here it is qa-catalog's opaque `storage_ref`, and passing
/// it through lets a runner image that understands the new form use it without
/// the adapter having to guess a URL.
const TEST_BUNDLE_REF_VAR: &str = "TEST_BUNDLE_REF";
/// The URL form, when [`ArgoExecutorConfig::bundle_base_url`] makes one
/// derivable (`argo.rs:40`).
const TEST_BUNDLE_URL_VAR: &str = "TEST_BUNDLE_URL";

/// Token endpoint the runner posts a `client_credentials` grant to, so it can
/// present a bearer token on [`TEST_BUNDLE_URL_VAR`].
///
/// Not a source-system variable: there the bundle route had no auth middleware
/// at all (`manager/src/routes/mod.rs:156-158`). Emitted only when
/// [`crate::config::ArgoExecutorConfig::bundle_auth`] is configured.
const TEST_BUNDLE_TOKEN_URL_VAR: &str = "TEST_BUNDLE_TOKEN_URL";
/// The `client_id` for that grant — a public identifier, so a literal.
const TEST_BUNDLE_CLIENT_ID_VAR: &str = "TEST_BUNDLE_CLIENT_ID";
/// The client secret for that grant, **always** a `secretKeyRef` and never a
/// literal.
///
/// `optional` is deliberately absent (Kubernetes defaults it to `false`),
/// unlike the `EnvSource::Secret` arm above where the port makes
/// non-resolution contractual (`run_executor.rs:63-66`). A pod that starts
/// without this variable cannot download any test content, and the most likely
/// way that surfaces is an empty pytest run reported as a pass. Failing the pod
/// at `CreateContainerConfigError` names the missing `Secret` instead.
const TEST_BUNDLE_CLIENT_SECRET_VAR: &str = "TEST_BUNDLE_CLIENT_SECRET";

/// Volume name for a mounted kubeconfig, matching the source system
/// (`argo.rs:506`).
const KUBECONFIG_VOLUME: &str = "kubeconfig";

/// The bundle id qa-catalog's route takes, recovered from a `storage_ref`.
///
/// `LocalFsBundleStore` writes blobs as `<uuid>.tar.gz`
/// (`qa-catalog/.../infra/bundle_store/local_fs.rs:19-21`), so the basename
/// minus its extensions is the id. **A hidden coupling, and named as one**: a
/// deployment that swapped in a different `BundleStore` would break this
/// silently. The right fix is for the spec to carry the id, which is a change
/// to `domain/` and therefore not this adapter's to make.
fn bundle_id(bundle_ref: &str) -> Option<&str> {
    let basename = bundle_ref.rsplit('/').next()?;
    let id = basename.split('.').next()?;
    if id.is_empty() { None } else { Some(id) }
}

/// One node's environment: the shared assembled environment, then the node's
/// own runner control variables.
///
/// Node variables are appended **last**, so a same-named entry in the shared
/// environment loses. That direction is deliberate and it is not the port's
/// literal-beats-secret rule (which is settled inside `RunEnv::new`): a
/// pipeline variable called `TEST_FILES` must not be able to replace the file
/// list this node was built to run. It is also unreachable — every such name is
/// on both reserved lists (`domain::params::RESERVED_NAMES`) — so this is a
/// belt to that braces.
fn node_env(spec: &RunSpec, node: &ExecutionNode, cfg: &ArgoExecutorConfig) -> Vec<Value> {
    let mut env: Vec<Value> = spec
        .env
        .entries()
        .iter()
        .map(|(name, source)| match source {
            EnvSource::Value(value) => json!({ "name": name, "value": value }),
            // A reference, never the material — the kubelet resolves it.
            // `optional: true` is the source system's own flag (`argo.rs:449`)
            // and the port makes it contractual: an `EnvSource::Secret` that
            // does not resolve leaves the variable unset and the execution
            // proceeds (`run_executor.rs:63-66`).
            EnvSource::Secret(reference) => json!({
                "name": name,
                "valueFrom": {
                    "secretKeyRef": {
                        "name": secret_name(&cfg.secret_name_prefix, reference.as_str()),
                        "key": cfg.secret_key,
                        "optional": true,
                    }
                }
            }),
        })
        .collect();

    // Pushed unconditionally, blank included: the source system pushes
    // `TEST_FILES` even when the joined list is empty (`argo.rs:436`) where it
    // skips every neighbouring variable when blank, and the port carries that
    // asymmetry forward as "an empty list is not an error"
    // (`run_executor.rs:431-436`).
    env.push(json!({ "name": TEST_FILES_VAR, "value": node.test_files.join(",") }));
    env.push(json!({ "name": TEST_BUNDLE_REF_VAR, "value": node.bundle_ref }));
    let base = cfg
        .bundle_base_url
        .as_deref()
        .map(str::trim)
        .filter(|base| !base.is_empty());
    if let (Some(base), Some(id)) = (base, bundle_id(&node.bundle_ref)) {
        let base = base.trim_end_matches('/');
        env.push(json!({
            "name": TEST_BUNDLE_URL_VAR,
            "value": format!("{base}/qa/v1/test-bundles/{id}"),
        }));
    }
    // Emitted whether or not a URL was derivable: an image that can fetch a
    // token cannot be assumed to need `TEST_BUNDLE_URL` to have come from this
    // adapter, and suppressing the credential when the URL is absent would make
    // one misconfiguration (no `bundle_base_url`) hide the other.
    if let Some(auth) = cfg.bundle_auth.as_ref() {
        env.push(json!({ "name": TEST_BUNDLE_TOKEN_URL_VAR, "value": auth.token_url }));
        env.push(json!({ "name": TEST_BUNDLE_CLIENT_ID_VAR, "value": auth.client_id }));
        env.push(json!({
            "name": TEST_BUNDLE_CLIENT_SECRET_VAR,
            "valueFrom": {
                "secretKeyRef": {
                    "name": auth.client_secret_secret,
                    "key": auth.client_secret_key,
                }
            }
        }));
    }
    env
}

/// The container template for one node.
fn template(spec: &RunSpec, node: &ExecutionNode, index: usize, cfg: &ArgoExecutorConfig) -> Value {
    let mut container = json!({
        "image": cfg.runner_image,
        "imagePullPolicy": cfg.image_pull_policy,
        "command": cfg.runner_command,
        "env": node_env(spec, node, cfg),
    });

    if let Some(mount) = spec.kubeconfig.as_ref() {
        // The port carries the **file** path so the caller can assert equality
        // with the assembled `KUBECONFIG` variable (`run_executor.rs:356-362`);
        // a secret volume mounts a **directory**. So the directory is the
        // path's parent and `items` maps the configured key onto the file name,
        // which is what puts the material at exactly `mount.mount_path`.
        let directory = split_mount_path(&mount.mount_path).0;
        container["volumeMounts"] = json!([{
            "name": KUBECONFIG_VOLUME,
            "mountPath": directory,
            "readOnly": true,
        }]);
    }

    json!({
        "name": task_name(&node.name, index),
        // Argo copies a template's metadata onto its pod, which is how `watch`
        // recovers the un-sanitised node name. See `NODE_ANNOTATION`.
        "metadata": { "annotations": { NODE_ANNOTATION: node.name } },
        "container": container,
    })
}

/// Split a kubeconfig **file** path into the directory a secret volume mounts
/// and the file name inside it.
///
/// `/.kube/kubeconfig` → `("/.kube", "kubeconfig")`, which is the source
/// system's pair spelled as two literals (`argo.rs:514`, `:519`). A path with
/// no directory component mounts at `/` — degenerate, but the alternative is
/// rejecting a `mount_path` the port permits.
fn split_mount_path(mount_path: &str) -> (String, String) {
    match mount_path.rsplit_once('/') {
        Some(("", file)) => ("/".to_owned(), file.to_owned()),
        Some((directory, file)) => (directory.to_owned(), file.to_owned()),
        None => ("/".to_owned(), mount_path.to_owned()),
    }
}

/// Build the `Workflow` object for one run.
///
/// `name` is the already-minted `metadata.name` — see
/// [`naming::workflow_name`](crate::infra::executor::argo::naming::workflow_name)
/// — passed in rather than derived here so the submit path can retry a 409 with
/// a different name without rebuilding the argument list.
///
/// # A single node stays a single container
///
/// One node yields one template and no DAG, matching the source system
/// (`argo.rs:1268-1274`, `custom_plans.rs:784-786`). Several nodes yield a
/// `main-dag` whose tasks carry **no** `depends`: the port states nodes are
/// independent and "must not infer an order from this vector's order"
/// (`run_executor.rs:404-406`).
#[must_use]
pub fn build(spec: &RunSpec, cfg: &ArgoExecutorConfig, name: &str) -> Value {
    let templates: Vec<Value> = spec
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| template(spec, node, index, cfg))
        .collect();

    let (entrypoint, templates) = if templates.len() == 1 {
        let entrypoint = templates
            .first()
            .and_then(|first| first.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("run-tests")
            .to_owned();
        (entrypoint, templates)
    } else {
        let tasks: Vec<Value> = templates
            .iter()
            .filter_map(|node_template| node_template.get("name").and_then(Value::as_str))
            .map(|node_name| json!({ "name": node_name, "template": node_name }))
            .collect();
        let mut all = vec![json!({ "name": "main-dag", "dag": { "tasks": tasks } })];
        all.extend(templates);
        ("main-dag".to_owned(), all)
    };

    let mut workflow_spec = json!({
        "entrypoint": entrypoint,
        // A **backstop**, not the guarantee: the control-plane sweep must fire
        // first (`run_executor.rs:469-474`, `argo.rs:539`).
        "activeDeadlineSeconds": spec.timeout_seconds,
        "ttlStrategy": { "secondsAfterCompletion": cfg.workflow_ttl_seconds },
        "templates": templates,
    });

    if let Some(account) = cfg
        .workflow_service_account
        .as_deref()
        .map(str::trim)
        .filter(|account| !account.is_empty())
    {
        workflow_spec["serviceAccountName"] = json!(account);
    }

    if let Some(mount) = spec.kubeconfig.as_ref() {
        let (_, file) = split_mount_path(&mount.mount_path);
        // No `optional` flag, so Kubernetes' default applies and the pod never
        // starts if the Secret is absent. That is the required behaviour, not an
        // oversight: a kubeconfig that does not resolve must fail the execution
        // rather than silently target nothing (`run_executor.rs:67-70`,
        // `argo.rs:506-511`).
        workflow_spec["volumes"] = json!([{
            "name": KUBECONFIG_VOLUME,
            "secret": {
                "secretName": secret_name(&cfg.secret_name_prefix, mount.secret.as_str()),
                "items": [{ "key": cfg.secret_key, "path": file }],
            }
        }]);
    }

    json!({
        "apiVersion": "argoproj.io/v1alpha1",
        "kind": "Workflow",
        "metadata": {
            "name": name,
            "namespace": cfg.namespace,
            "labels": {
                APP_LABEL: APP_LABEL_VALUE,
                RUN_ID_LABEL: label_value(&spec.run_id.to_string()),
            },
        },
        "spec": workflow_spec,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;
    use uuid::Uuid;

    use super::{APP_LABEL_VALUE, NODE_ANNOTATION, build};
    use crate::config::{ArgoExecutorConfig, BundleAuthConfig};
    use crate::domain::ports::run_executor::{
        ExecutionNode, KubeconfigMount, RunEnv, RunSpec, SecretRef,
    };

    fn cfg() -> ArgoExecutorConfig {
        ArgoExecutorConfig {
            runner_image: "vhp-test-runner:latest".to_owned(),
            ..ArgoExecutorConfig::default()
        }
    }

    fn node(name: &str) -> ExecutionNode {
        ExecutionNode {
            name: name.to_owned(),
            bundle_ref: "/var/lib/qa-catalog/bundles/2f1c9a70-0000-4000-8000-000000000001.tar.gz"
                .to_owned(),
            test_files: vec!["tests/test_smoke.py".to_owned()],
        }
    }

    fn spec(nodes: Vec<ExecutionNode>) -> RunSpec {
        RunSpec {
            run_id: Uuid::nil(),
            run_name: "Smoke Tests-1".to_owned(),
            nodes,
            env: RunEnv::default(),
            kubeconfig: None,
            timeout_seconds: 1800,
        }
    }

    fn env_of(workflow: &Value, template_index: usize) -> BTreeMap<String, Value> {
        workflow["spec"]["templates"][template_index]["container"]["env"]
            .as_array()
            .expect("an env list")
            .iter()
            .map(|entry| {
                (
                    entry["name"].as_str().unwrap_or_default().to_owned(),
                    entry.clone(),
                )
            })
            .collect()
    }

    /// The shape the source system submits (`argo.rs:523-544`), asserted field
    /// by field rather than as a snapshot so a failure names the field.
    #[test]
    fn a_single_node_run_is_one_container_and_no_dag() {
        let workflow = build(&spec(vec![node("repo-smoke")]), &cfg(), "smoke-tests-1");

        assert_eq!(workflow["apiVersion"], "argoproj.io/v1alpha1");
        assert_eq!(workflow["kind"], "Workflow");
        assert_eq!(workflow["metadata"]["name"], "smoke-tests-1");
        assert_eq!(workflow["metadata"]["namespace"], "argo");
        assert_eq!(workflow["metadata"]["labels"]["app"], APP_LABEL_VALUE);
        assert_eq!(
            workflow["metadata"]["labels"]["qa-runs/run-id"],
            Uuid::nil().to_string()
        );
        assert_eq!(workflow["spec"]["activeDeadlineSeconds"], 1800);
        assert_eq!(
            workflow["spec"]["ttlStrategy"]["secondsAfterCompletion"],
            3600
        );

        let templates = workflow["spec"]["templates"].as_array().expect("templates");
        assert_eq!(templates.len(), 1, "no synthetic DAG for one node");
        assert_eq!(workflow["spec"]["entrypoint"], templates[0]["name"]);
        assert_eq!(templates[0]["container"]["image"], "vhp-test-runner:latest");
        assert_eq!(templates[0]["container"]["imagePullPolicy"], "IfNotPresent");
        assert_eq!(templates[0]["container"]["command"][0], "/entrypoint.sh");
        assert!(
            workflow["spec"].get("volumes").is_none(),
            "no kubeconfig means no volume at all (argo.rs:504)"
        );
    }

    /// The un-sanitised node name has to survive onto the pod, because `watch`
    /// is handed only an `ExecutionRef` and has no other way to recover it.
    #[test]
    fn every_template_carries_its_nodes_name_verbatim_as_a_pod_annotation() {
        let workflow = build(&spec(vec![node("Repo: Smoke/A")]), &cfg(), "w");
        assert_eq!(
            workflow["spec"]["templates"][0]["metadata"]["annotations"][NODE_ANNOTATION],
            "Repo: Smoke/A"
        );
        assert_eq!(
            workflow["spec"]["templates"][0]["name"], "n-0-repo--smoke-a",
            "while the template name itself is sanitised"
        );
    }

    /// Several nodes become a DAG whose tasks are independent. A `depends` on
    /// any task would impose an order the port says does not exist
    /// (`run_executor.rs:404-406`).
    #[test]
    fn several_nodes_become_a_dag_with_no_dependencies() {
        let workflow = build(&spec(vec![node("a"), node("b")]), &cfg(), "w");
        assert_eq!(workflow["spec"]["entrypoint"], "main-dag");

        let templates = workflow["spec"]["templates"].as_array().expect("templates");
        assert_eq!(templates.len(), 3, "the DAG template plus one per node");
        let tasks = templates[0]["dag"]["tasks"].as_array().expect("tasks");
        assert_eq!(tasks.len(), 2);
        for task in tasks {
            assert!(
                task.get("depends").is_none(),
                "nodes are independent; a depends expression would invent an order"
            );
        }
        assert_eq!(tasks[0]["name"], "n-0-a");
        assert_eq!(tasks[1]["name"], "n-1-b");
        assert!(
            workflow["spec"].get("parallelism").is_none(),
            "the port carries no parallelism cap"
        );
    }

    /// Each node gets its own file list, and nothing else does — the shared
    /// environment deliberately does not carry `TEST_FILES`
    /// (`service::dispatch_spec:476-479`).
    #[test]
    fn each_node_carries_its_own_file_list_and_bundle() {
        let mut second = node("b");
        second.test_files = vec!["tests/x.py".to_owned(), "tests/y.py".to_owned()];
        second.bundle_ref = "/bundles/other.tar.gz".to_owned();
        let workflow = build(&spec(vec![node("a"), second]), &cfg(), "w");

        // Template 0 is the DAG; nodes start at 1.
        assert_eq!(
            env_of(&workflow, 1)["TEST_FILES"]["value"],
            "tests/test_smoke.py"
        );
        assert_eq!(
            env_of(&workflow, 2)["TEST_FILES"]["value"],
            "tests/x.py,tests/y.py"
        );
        assert_eq!(
            env_of(&workflow, 2)["TEST_BUNDLE_REF"]["value"],
            "/bundles/other.tar.gz"
        );
    }

    /// "An executor that rejected an empty list would diverge from the source
    /// system on a real, if unhappy, launch" (`run_executor.rs:434-436`).
    #[test]
    fn a_node_with_no_test_files_still_gets_the_variable_blank() {
        let mut empty = node("a");
        empty.test_files.clear();
        let workflow = build(&spec(vec![empty]), &cfg(), "w");
        assert_eq!(env_of(&workflow, 0)["TEST_FILES"]["value"], "");
    }

    /// The invariant the port is built around, asserted on the actual submitted
    /// object: a secret-backed variable appears as a reference and its text
    /// never appears as a literal anywhere in the workflow.
    #[test]
    fn a_secret_arm_emits_a_reference_and_never_a_literal() {
        let mut run = spec(vec![node("a")]);
        run.env = RunEnv::new(
            BTreeMap::new(),
            [(
                "RP_API_KEY".to_owned(),
                SecretRef::new("credstore://rp/token"),
            )]
            .into_iter()
            .collect(),
        );
        let workflow = build(&run, &cfg(), "w");

        let entry = &env_of(&workflow, 0)["RP_API_KEY"];
        assert!(entry.get("value").is_none(), "no literal for a reference");
        let secret = &entry["valueFrom"]["secretKeyRef"];
        assert_eq!(
            secret["name"], "qa-platform-credstore---rp-token",
            "the reference's punctuation each becomes a dash; runs are not \
             collapsed, so the mapping stays injective"
        );
        assert_eq!(secret["key"], "value");
        assert_eq!(
            secret["optional"], true,
            "the source system's optional: true (argo.rs:449), which the port \
             makes contractual: an unresolvable reference leaves the variable unset"
        );
        assert!(
            !serde_json::to_string(&workflow)
                .expect("serialisable")
                .contains("credstore://"),
            "the reference's own text must not appear as a value anywhere either"
        );
    }

    /// The kubeconfig pair: a **required** secret volume mounted at the
    /// directory, with `items` putting the file at exactly the port's
    /// `mount_path`.
    #[test]
    fn a_kubeconfig_mount_is_a_required_volume_landing_at_the_ports_path() {
        let mut run = spec(vec![node("a")]);
        run.kubeconfig = Some(KubeconfigMount {
            secret: SecretRef::new("platform/9f2c"),
            mount_path: "/.kube/kubeconfig".to_owned(),
        });
        let workflow = build(&run, &cfg(), "w");

        let volume = &workflow["spec"]["volumes"][0];
        assert_eq!(volume["name"], "kubeconfig");
        assert_eq!(volume["secret"]["secretName"], "qa-platform-platform-9f2c");
        assert_eq!(volume["secret"]["items"][0]["key"], "value");
        assert_eq!(
            volume["secret"]["items"][0]["path"], "kubeconfig",
            "the file name comes from the port's mount_path, so the material \
             lands where the assembled KUBECONFIG variable says it will"
        );
        assert!(
            volume["secret"].get("optional").is_none(),
            "no optional flag: Kubernetes' default is required, and a \
             kubeconfig that does not resolve must stop the execution"
        );

        let mount = &workflow["spec"]["templates"][0]["container"]["volumeMounts"][0];
        assert_eq!(mount["mountPath"], "/.kube", "the directory, not the file");
        assert_eq!(mount["readOnly"], true);
    }

    /// The bundle URL is derived from the `storage_ref`'s basename, and only
    /// when a base URL is configured. Both halves are pinned because the
    /// derivation is a hidden coupling to `LocalFsBundleStore`'s file naming.
    #[test]
    fn a_bundle_url_is_derived_only_when_a_base_url_is_configured() {
        let workflow = build(&spec(vec![node("a")]), &cfg(), "w");
        assert!(
            !env_of(&workflow, 0).contains_key("TEST_BUNDLE_URL"),
            "no base URL configured, so no URL is invented"
        );

        let with_base = ArgoExecutorConfig {
            bundle_base_url: Some("http://10.0.0.1:8080/".to_owned()),
            ..cfg()
        };
        let workflow = build(&spec(vec![node("a")]), &with_base, "w");
        assert_eq!(
            env_of(&workflow, 0)["TEST_BUNDLE_URL"]["value"],
            "http://10.0.0.1:8080/qa/v1/test-bundles/2f1c9a70-0000-4000-8000-000000000001"
        );
    }

    /// Bundle authentication: the token URL and client id travel as literals,
    /// the client secret **only** as a `secretKeyRef`, and none of it appears
    /// unless it is configured.
    #[test]
    fn bundle_auth_emits_a_reference_for_the_secret_and_literals_for_the_rest() {
        let workflow = build(&spec(vec![node("a")]), &cfg(), "w");
        for absent in [
            "TEST_BUNDLE_TOKEN_URL",
            "TEST_BUNDLE_CLIENT_ID",
            "TEST_BUNDLE_CLIENT_SECRET",
        ] {
            assert!(
                !env_of(&workflow, 0).contains_key(absent),
                "{absent} must not appear when bundle_auth is unset"
            );
        }

        let with_auth = ArgoExecutorConfig {
            bundle_auth: Some(BundleAuthConfig {
                token_url: "http://idp:8180/realms/r/protocol/openid-connect/token".to_owned(),
                client_id: "qa-platform-workflow".to_owned(),
                client_secret_secret: "qa-platform-workflow-oidc".to_owned(),
                client_secret_key: "client_secret".to_owned(),
            }),
            ..cfg()
        };
        let workflow = build(&spec(vec![node("a")]), &with_auth, "w");
        let env = env_of(&workflow, 0);
        assert_eq!(
            env["TEST_BUNDLE_TOKEN_URL"]["value"],
            "http://idp:8180/realms/r/protocol/openid-connect/token"
        );
        assert_eq!(
            env["TEST_BUNDLE_CLIENT_ID"]["value"],
            "qa-platform-workflow"
        );

        let entry = &env["TEST_BUNDLE_CLIENT_SECRET"];
        assert!(
            entry.get("value").is_none(),
            "the client secret must never travel as a literal"
        );
        let reference = &entry["valueFrom"]["secretKeyRef"];
        assert_eq!(reference["name"], "qa-platform-workflow-oidc");
        assert_eq!(reference["key"], "client_secret");
        assert!(
            reference.get("optional").is_none(),
            "required on purpose: a pod with no credential downloads no tests \
             and would report an empty suite as a pass"
        );
        assert_eq!(
            reference["name"], "qa-platform-workflow-oidc",
            "the Secret is named verbatim from config, NOT through \
             secret_name_prefix: it is pre-provisioned by an operator, not \
             derived from a port SecretRef"
        );
    }

    /// Unset leaves `serviceAccountName` off entirely; set puts it on the spec.
    /// The measured failure this guards is in the field's own doc: without a
    /// suitable account the run fails *after* producing all of its output.
    #[test]
    fn the_workflow_service_account_is_written_only_when_configured() {
        let workflow = build(&spec(vec![node("a")]), &cfg(), "w");
        assert!(workflow["spec"].get("serviceAccountName").is_none());

        let with_account = ArgoExecutorConfig {
            workflow_service_account: Some("argo-workflow".to_owned()),
            ..cfg()
        };
        let workflow = build(&spec(vec![node("a")]), &with_account, "w");
        assert_eq!(workflow["spec"]["serviceAccountName"], "argo-workflow");
    }

    /// A node variable must win over a same-named entry in the shared
    /// environment: a pipeline variable called `TEST_FILES` replacing a node's
    /// file list would run the wrong tests.
    #[test]
    fn a_node_variable_wins_over_the_shared_environment() {
        let mut run = spec(vec![node("a")]);
        run.env = RunEnv::new(
            [("TEST_FILES".to_owned(), "tests/attacker.py".to_owned())]
                .into_iter()
                .collect(),
            BTreeMap::new(),
        );
        let workflow = build(&run, &cfg(), "w");
        let entries = workflow["spec"]["templates"][0]["container"]["env"]
            .as_array()
            .expect("env");
        let last = entries
            .iter()
            .rfind(|entry| entry["name"] == "TEST_FILES")
            .expect("a TEST_FILES entry");
        assert_eq!(
            last["value"], "tests/test_smoke.py",
            "the container runtime takes the last entry, and the node's is last"
        );
    }
}
