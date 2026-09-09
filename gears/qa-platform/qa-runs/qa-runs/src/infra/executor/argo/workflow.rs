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

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::config::ArgoExecutorConfig;
use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{EnvSource, ExecutionNode, MountSpec, RunSpec};
use crate::infra::executor::argo::naming::{
    label_value, mount_volume_name, secret_name, task_name,
};

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

/// A configured string, or `None` when it is absent or only whitespace.
///
/// The value is returned **un-trimmed**: every caller either uses it verbatim
/// (an image name, a pull policy) or trims at its own site, and trimming here
/// would quietly change values this task is not allowed to change.
fn non_blank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

/// Every [`MountSpec::Secret`] in a run's access, paired with the pod-internal
/// volume name it renders as.
///
/// A [`MountSpec::ConfigValue`] is not here, and cannot reach this function:
/// [`reject_unrenderable_mounts`] refuses the submission before [`build`] is
/// called. Skipping one silently would be the dangerous outcome — a run that
/// starts with a credential it was told to mount simply absent — which is why
/// the refusal is a hard error and not a `warn!`.
fn secret_mounts(spec: &RunSpec) -> Vec<RenderedMount<'_>> {
    spec.access
        .mounts
        .iter()
        .enumerate()
        .filter_map(|(index, mount)| match mount {
            MountSpec::Secret {
                credstore_ref,
                path,
                mode,
            } => Some(RenderedMount {
                volume: mount_volume_name(index),
                credstore_ref,
                path,
                mode: *mode,
            }),
            MountSpec::ConfigValue { .. } => None,
        })
        .collect()
}

/// One [`MountSpec::Secret`] and the volume name it renders as.
///
/// A named struct rather than the tuple this started as, and not only because
/// `clippy::type_complexity` said so: `credstore_ref` and `path` are both
/// `&str`, so a tuple lets a caller read one where the other belongs and
/// produce a workflow that mounts a `Secret` named after a filesystem path.
struct RenderedMount<'a> {
    /// Pod-internal volume name; see
    /// [`mount_volume_name`](crate::infra::executor::argo::naming::mount_volume_name).
    volume: String,
    /// The credstore reference the `Secret`'s name is derived from. Never
    /// material.
    credstore_ref: &'a str,
    /// The **file** path the material must land at, inside the container.
    path: &'a str,
    /// File permissions the plugin asked for, if any.
    mode: Option<i32>,
}

/// Refuse a spec this adapter cannot render faithfully.
///
/// Two shapes, both of which would otherwise fail *later* and less legibly —
/// as a pod stuck on `FailedMount`, or as a 422 from the API server naming
/// neither the run nor the mount.
///
/// # 1. `MountSpec::ConfigValue` is not implemented, deliberately
///
/// Rendering one means **creating a Kubernetes `Secret`** holding a value the
/// plugin resolved, then mounting it — the plan's "a generated `Secret` via the
/// same path". The rendering is the easy half. The lifecycle is not: the Secret
/// must be garbage-collected with the workflow or it stays in the cluster
/// holding plaintext credential material indefinitely, and an `ownerReference`
/// needs the workflow's UID, which does not exist until after the submission
/// this function guards. Getting that wrong leaves credentials at rest in a
/// namespace nothing cleans; getting it right is a lifecycle design with no
/// consumer to validate it, because **no plugin emits a `ConfigValue` today**
/// (`qa-vhp-product-plugin`'s `prepare_run_access` returns one
/// `MountSpec::Secret` and nothing else).
///
/// So it refuses, and the refusal names what has to be built. Spec §5.3 records
/// what happened the last time this interface carried a speculative shape:
/// `VolumeSpec` was deleted because a field no task populated "invites a plugin
/// author to fill it in and be silently dropped".
///
/// # 2. Two mounts cannot share a directory
///
/// A secret volume mounts the *directory* containing the file (`split_mount_path`
/// below), so two mounts whose paths share a parent render two
/// `volumeMounts` at one `mountPath` — which the API server rejects for the
/// whole workflow. Refusing here says which two paths collided.
///
/// # Errors
///
/// [`DomainError::ExecutorFailed`] naming the run and the shape that was
/// refused — the `MountSpec` variant, or the directory two mounts collided in.
/// Never the value a `ConfigValue` carried.
pub fn reject_unrenderable_mounts(spec: &RunSpec) -> Result<(), DomainError> {
    if spec
        .access
        .mounts
        .iter()
        .any(|mount| matches!(mount, MountSpec::ConfigValue { .. }))
    {
        return Err(DomainError::ExecutorFailed(format!(
            "run {} declares a plugin-resolved mount (MountSpec::ConfigValue), which \
             this executor does not implement: rendering one requires generating a \
             Kubernetes Secret and garbage-collecting it with the workflow",
            spec.run_id
        )));
    }

    let mut directories: BTreeSet<String> = BTreeSet::new();
    for mount in secret_mounts(spec) {
        let directory = split_mount_path(mount.path).0;
        if !directories.insert(directory.clone()) {
            return Err(DomainError::ExecutorFailed(format!(
                "run {} declares two mounts under {directory}: a secret volume mounts \
                 the enclosing directory, so the two would render as one mountPath",
                spec.run_id
            )));
        }
    }
    Ok(())
}

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
    // The product's runner shape wins over the deployment's, and an absent or
    // blank field means "inherit" — `RunnerSpec::image`'s own doc defines
    // `None` that way, and a blank string is the same statement made by a
    // config file that has the key but no value. Every run today declares
    // nothing, so every run today renders the deployment's three values
    // unchanged.
    let image = non_blank(spec.runner.image.as_deref()).unwrap_or(&cfg.runner_image);
    let pull_policy =
        non_blank(spec.runner.image_pull_policy.as_deref()).unwrap_or(&cfg.image_pull_policy);
    let command: &[String] = if spec.runner.command.is_empty() {
        &cfg.runner_command
    } else {
        &spec.runner.command
    };
    let mut container = json!({
        "image": image,
        "imagePullPolicy": pull_policy,
        "command": command,
        "env": node_env(spec, node, cfg),
    });

    // The port carries each mount's **file** path so the caller can assert
    // equality with the variable that names it; a secret volume mounts a
    // **directory**. So the directory is the path's parent and `items` maps the
    // configured key onto the file name, which is what puts the material at
    // exactly the path the plugin asked for.
    let mounts: Vec<Value> = secret_mounts(spec)
        .into_iter()
        .map(|mount| {
            json!({
                "name": mount.volume,
                "mountPath": split_mount_path(mount.path).0,
                "readOnly": true,
            })
        })
        .collect();
    if !mounts.is_empty() {
        container["volumeMounts"] = json!(mounts);
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

    // The run's own account wins over the deployment's. A plugin naming one is
    // making a statement about how its runner authenticates (**D11**), which
    // the deployment default cannot know; a plugin naming none — every plugin
    // today — leaves the deployment's value exactly where it was.
    if let Some(account) = non_blank(spec.access.service_account.as_deref())
        .or_else(|| non_blank(cfg.workflow_service_account.as_deref()))
    {
        // Trimmed on the way out, which is what this line always did — the
        // deployment's value is read from a config file.
        workflow_spec["serviceAccountName"] = json!(account.trim());
    }

    let volumes: Vec<Value> = secret_mounts(spec)
        .into_iter()
        .map(|mount| {
            let (_, file) = split_mount_path(mount.path);
            let mut item = json!({ "key": cfg.secret_key, "path": file });
            // Absent unless the plugin asked for one, so a mount that says
            // nothing about permissions gets Kubernetes' default — which is
            // what the source system's kubeconfig volume gets
            // (`argo.rs:506-511` sets no mode).
            if let Some(mode) = mount.mode {
                item["mode"] = json!(mode);
            }
            // No `optional` flag, so Kubernetes' default applies and the pod
            // never starts if the Secret is absent. That is the required
            // behaviour, not an oversight: a credential that does not resolve
            // must fail the execution rather than silently target nothing
            // (`run_executor.rs`' `MountSpec` doc, `argo.rs:506-511`).
            json!({
                "name": mount.volume,
                "secret": {
                    "secretName": secret_name(&cfg.secret_name_prefix, mount.credstore_ref),
                    "items": [item],
                }
            })
        })
        .collect();
    if !volumes.is_empty() {
        workflow_spec["volumes"] = json!(volumes);
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

    use super::{APP_LABEL_VALUE, NODE_ANNOTATION, build, reject_unrenderable_mounts};
    use crate::config::{ArgoExecutorConfig, BundleAuthConfig};
    use crate::domain::ports::run_executor::{
        ExecutionNode, MountSpec, RunAccess, RunEnv, RunSpec, RunnerSpec, SecretRef,
    };

    /// A secret mount, the only variant any plugin emits today.
    fn secret_mount(credstore_ref: &str, path: &str, mode: Option<i32>) -> MountSpec {
        MountSpec::Secret {
            credstore_ref: credstore_ref.to_owned(),
            path: path.to_owned(),
            mode,
        }
    }

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
            access: RunAccess::default(),
            runner: RunnerSpec::default(),
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
    /// directory, with `items` putting the file at exactly the path the mount
    /// asked for.
    #[test]
    fn a_secret_mount_is_a_required_volume_landing_at_the_mounts_path() {
        let mut run = spec(vec![node("a")]);
        run.access.mounts = vec![secret_mount("platform/9f2c", "/.kube/kubeconfig", None)];
        let workflow = build(&run, &cfg(), "w");

        let volume = &workflow["spec"]["volumes"][0];
        assert_eq!(volume["name"], "mount-0");
        assert_eq!(volume["secret"]["secretName"], "qa-platform-platform-9f2c");
        assert_eq!(volume["secret"]["items"][0]["key"], "value");
        assert_eq!(
            volume["secret"]["items"][0]["path"], "kubeconfig",
            "the file name comes from the mount's path, so the material lands \
             where the assembled KUBECONFIG variable says it will"
        );
        assert!(
            volume["secret"]["items"][0].get("mode").is_none(),
            "the mount declared none, so Kubernetes' default applies, which is \
             what the source system's kubeconfig volume gets (argo.rs:506-511)"
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

    /// A mount's `mode` reaches the secret's `items` entry, and only when it
    /// was asked for.
    ///
    /// The value that matters is `0o400`: `qa-vhp-product-plugin` declares it,
    /// so this is the line that carries an owner-read-only kubeconfig into the
    /// pod once Task 18 dispatches through the plugin.
    #[test]
    fn a_mounts_mode_reaches_the_secret_item() {
        let mut run = spec(vec![node("a")]);
        run.access.mounts = vec![secret_mount("p/1", "/.kube/kubeconfig", Some(0o400))];
        let workflow = build(&run, &cfg(), "w");
        assert_eq!(
            workflow["spec"]["volumes"][0]["secret"]["items"][0]["mode"], 256,
            "0o400, as the decimal Kubernetes takes"
        );
    }

    /// Several mounts render several volumes, each paired to its own
    /// `volumeMount` by an ordinal name.
    ///
    /// One mount is what every run has today; the port accepts a vector because
    /// a product's credentials are the product's to declare, and a rendering
    /// that quietly handled only the first would lose the rest.
    #[test]
    fn several_secret_mounts_render_one_volume_and_one_mount_each() {
        let mut run = spec(vec![node("a")]);
        run.access.mounts = vec![
            secret_mount("p/kubeconfig", "/.kube/kubeconfig", None),
            secret_mount("p/license", "/etc/product/license.key", Some(0o400)),
        ];
        let workflow = build(&run, &cfg(), "w");

        let volumes = workflow["spec"]["volumes"].as_array().expect("volumes");
        assert_eq!(volumes.len(), 2);
        assert_eq!(volumes[0]["name"], "mount-0");
        assert_eq!(volumes[1]["name"], "mount-1");
        assert_eq!(volumes[1]["secret"]["items"][0]["path"], "license.key");

        let mounts = workflow["spec"]["templates"][0]["container"]["volumeMounts"]
            .as_array()
            .expect("volumeMounts");
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0]["name"], "mount-0");
        assert_eq!(mounts[0]["mountPath"], "/.kube");
        assert_eq!(mounts[1]["name"], "mount-1");
        assert_eq!(mounts[1]["mountPath"], "/etc/product");
    }

    /// The run's own service account wins over the deployment's; a run that
    /// names none leaves the deployment's exactly where it was.
    #[test]
    fn a_runs_service_account_wins_over_the_deployments() {
        let deployment = ArgoExecutorConfig {
            workflow_service_account: Some("  qa-runner  ".to_owned()),
            ..cfg()
        };

        let workflow = build(&spec(vec![node("a")]), &deployment, "w");
        assert_eq!(
            workflow["spec"]["serviceAccountName"], "qa-runner",
            "trimmed, which is what this line always did"
        );

        let mut run = spec(vec![node("a")]);
        run.access.service_account = Some("product-runner".to_owned());
        let workflow = build(&run, &deployment, "w");
        assert_eq!(workflow["spec"]["serviceAccountName"], "product-runner");

        let workflow = build(&spec(vec![node("a")]), &cfg(), "w");
        assert!(
            workflow["spec"].get("serviceAccountName").is_none(),
            "neither side named one, so the field is absent rather than empty"
        );
    }

    /// A product's runner shape overrides the deployment's three values, and an
    /// absent **or blank** field means "inherit" — `RunnerSpec::image`'s own
    /// definition of `None`, and the same statement a config key with no value
    /// makes.
    #[test]
    fn a_runner_spec_overrides_the_deployment_and_blank_means_inherit() {
        let mut run = spec(vec![node("a")]);
        run.runner = RunnerSpec {
            image: Some("product-runner:2".to_owned()),
            command: vec!["/run.sh".to_owned()],
            image_pull_policy: Some("Always".to_owned()),
        };
        let workflow = build(&run, &cfg(), "w");
        let container = &workflow["spec"]["templates"][0]["container"];
        assert_eq!(container["image"], "product-runner:2");
        assert_eq!(container["command"][0], "/run.sh");
        assert_eq!(container["imagePullPolicy"], "Always");

        let mut blank = spec(vec![node("a")]);
        blank.runner = RunnerSpec {
            image: Some("   ".to_owned()),
            command: Vec::new(),
            image_pull_policy: Some(String::new()),
        };
        let workflow = build(&blank, &cfg(), "w");
        let container = &workflow["spec"]["templates"][0]["container"];
        assert_eq!(container["image"], "vhp-test-runner:latest");
        assert_eq!(container["command"][0], "/entrypoint.sh");
        assert_eq!(container["imagePullPolicy"], "IfNotPresent");
    }

    /// A plugin-resolved mount is refused **before** submission, with a message
    /// naming what would have to be built.
    ///
    /// Not rendered-and-dropped: a run that started with a credential it was
    /// told to mount simply absent is the failure this refusal exists to
    /// prevent. See [`reject_unrenderable_mounts`].
    #[test]
    fn a_plugin_resolved_mount_is_refused_before_submission() {
        let mut run = spec(vec![node("a")]);
        run.access.mounts = vec![MountSpec::ConfigValue {
            value: credstore_sdk::SecretValue::new(b"not-a-real-token".to_vec()),
            path: "/etc/product/token".to_owned(),
            mode: None,
        }];

        let error = reject_unrenderable_mounts(&run).expect_err("a refusal");
        assert!(
            error.to_string().contains("MountSpec::ConfigValue"),
            "the message must name the variant, not just fail: {error}"
        );
        assert!(
            !error.to_string().contains("not-a-real-token"),
            "and it must never carry the value it refused"
        );
        assert!(
            build(&run, &cfg(), "w")["spec"].get("volumes").is_none(),
            "premise: build renders nothing for it, which is exactly why the \
             refusal has to come first"
        );
    }

    /// Two mounts under one directory are refused, because a secret volume
    /// mounts the directory: the two would render as one `mountPath` and the
    /// API server would reject the whole workflow.
    #[test]
    fn two_mounts_in_one_directory_are_refused() {
        let mut run = spec(vec![node("a")]);
        run.access.mounts = vec![
            secret_mount("p/kubeconfig", "/etc/qa/kubeconfig", None),
            secret_mount("p/token", "/etc/qa/token", None),
        ];

        let error = reject_unrenderable_mounts(&run).expect_err("a refusal");
        assert!(
            error.to_string().contains("/etc/qa"),
            "the message must name the directory that collided: {error}"
        );

        run.access.mounts = vec![
            secret_mount("p/kubeconfig", "/etc/qa/kubeconfig", None),
            secret_mount("p/token", "/etc/other/token", None),
        ];
        assert!(
            reject_unrenderable_mounts(&run).is_ok(),
            "two directories are two volumes and are fine"
        );
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
