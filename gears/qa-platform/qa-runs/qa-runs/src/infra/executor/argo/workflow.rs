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

/// Pod label marking every runner pod as one that executes tenant-written
/// test code, so the cluster's `NetworkPolicy` can select on it.
///
/// # The contract this label is
///
/// The runner pod can otherwise reach the gears' own APIs, Postgres,
/// Keycloak and another tenant's running pod over the pod network — nothing
/// in `deploy/` stopped it. The fix is one `NetworkPolicy`
/// (`deploy/helm/qa-platform/templates/runner-networkpolicy.yaml`) whose
/// `podSelector` selects on this exact key and value. **A `NetworkPolicy`
/// whose selector matches nothing fails open and looks installed** — it
/// renders, `helm lint`s clean, and shows up in `kubectl get netpol`, while
/// protecting nothing — so this pair is asserted from *both* sides rather
/// than trusted to stay in sync by inspection:
///   - this module's `every_template_carries_the_network_isolation_label`
///     test, on the Rust side;
///   - `deploy/helm/tests/check_runner_networkpolicy.py`, on the chart side,
///     which reads the rendered policy's `podSelector` **and** greps this
///     file for these two constants, and fails if the two ever disagree.
///
/// Editing the literal value in the chart template without editing this
/// constant (or the reverse) fails that guard rather than silently
/// installing a policy that selects nothing.
///
/// # How it reaches the pod
///
/// The same route as [`NODE_ANNOTATION`]: [`template`] writes this into a
/// container template's `metadata.labels`, and Argo copies a template's
/// `metadata` (labels *and* annotations, not only the latter) onto the pod
/// it creates to run that template. A label set at the *workflow* level
/// would not do this — it would decorate the `Workflow` object
/// (`metadata.labels`, alongside [`APP_LABEL`] and [`RUN_ID_LABEL`] in
/// [`build`]) but never reach the pod a `NetworkPolicy` actually selects on.
pub const NETWORK_ISOLATION_LABEL: &str = "qa-platform/network-isolated";
/// [`NETWORK_ISOLATION_LABEL`]'s value. A literal `"true"`, not derived from
/// anything run-specific: every runner pod, regardless of run or tenant,
/// carries the same pair, because the `NetworkPolicy` this label feeds is
/// rendered once at chart-install time and cannot vary per run.
pub const NETWORK_ISOLATION_LABEL_VALUE: &str = "true";

/// Pod-internal volume name for the `/tmp` `emptyDir` every runner container
/// mounts.
///
/// Not derived from [`mount_volume_name`], which numbers *secret* mounts by
/// their index in `RunAccess::mounts` — this volume exists whether or not a
/// run declares any, so it needs a name no such index can collide with. See
/// [`build`]'s own doc on why the volume exists at all.
const TMP_VOLUME_NAME: &str = "tmp";

/// Pod-internal volume name for the `/work` `emptyDir` every runner
/// container mounts.
///
/// Fix round 4 of the 2026-09-17 network-isolation task: `runner.Dockerfile`
/// creates `/work` as root with default ownership and declares no `USER`
/// (`RUN mkdir -p /work`), and `build`'s own pod `securityContext` runs the
/// container as `cfg.run_as_user` — a non-root, non-zero uid an earlier
/// round of this very task measured against (`the_pod_is_unprivileged_but_
/// keeps_its_service_account_token`, below). That uid cannot write into a
/// root-owned `755` directory, and `/work` is exactly where
/// `entrypoint.sh`'s `QA_RUNNER_WORKDIR` unpacks the bundle and where pytest
/// runs — measured on a live cluster as `PermissionError: [Errno 13]
/// Permission denied: '/work/.gitignore'`, after the bundle had already
/// downloaded successfully (rule 5's Keycloak fix, and rule 4's API-server
/// fix, both working correctly by that point).
///
/// The fix is the same shape [`TMP_VOLUME_NAME`] already is, for the same
/// reason: an `emptyDir` the kubelet creates owned by the pod's own
/// `fsGroup`/uid rather than a path baked into the image as root. **Not** a
/// Dockerfile change — making `/work` world-writable inside an image that
/// unpacks and executes tenant-written test code is the wrong direction, and
/// this task's own hardening (`runAsNonRoot`, dropped capabilities) exists
/// to keep that image read-only where nothing needs it writable. The volume
/// starts empty, which is correct: `fetch_bundle.py` is what populates it,
/// after the mount exists.
const WORK_VOLUME_NAME: &str = "work";

/// A writable `$HOME` for the runner container — fix round 5 of the
/// 2026-09-17 network-isolation task, measured on a live cluster one layer
/// past [`WORK_VOLUME_NAME`]'s failure: with `/work` writable, the bundle
/// unpacked and `entrypoint.sh` reached `pip install -r requirements.txt`,
/// which then failed with `OSError: [Errno 13] Permission denied:
/// '/nonexistent'`.
///
/// `runner.Dockerfile` is `FROM python:3.12-slim` with no `USER` of its own,
/// so it is built to run as root; its `nobody` account (uid 65534, the same
/// uid `cfg.run_as_user` hardens the pod to) has `/nonexistent` as `$HOME` on
/// Debian, which does not exist. pip cannot write to its own install prefix
/// (`/usr/local`, root-owned) as a non-root user, so it falls back to a
/// per-user install under `$HOME/.local` — and a `$HOME` that does not exist
/// makes that fallback fail too, taking every requirement in the bundle with
/// it. Hardening the uid without giving it a writable home breaks the first
/// tool that wants one; nothing about `pip` specifically is the cause.
///
/// The fix reuses [`TMP_VOLUME_NAME`]'s mount rather than adding a third
/// volume: `/tmp` is already a writable `emptyDir` every runner container
/// mounts, and a `$HOME` needs nothing else.
const HOME_VALUE: &str = "/tmp";

/// `$PATH`, extended with the per-user install location `$HOME`'s
/// `.local/bin` a non-root `pip install` (see [`HOME_VALUE`]) falls back to.
/// pytest itself is invoked as `python3 -m pytest` (`entrypoint.sh`), so
/// nothing in this image's own test-running path needs a console script on
/// `PATH` — but the bundle's `requirements.txt` is tenant-supplied, and a
/// suite that shells out to a console script one of its own dependencies
/// installs would silently find it importable but not runnable without
/// this: pip warns, on exactly this fallback, that `$HOME/.local/bin` is
/// "not on PATH".
///
/// **A hidden coupling, and named as one** — this file's established term for
/// one, and now its only remaining instance: the other was `bundle_id()`,
/// which recovered a bundle's id by parsing `bundle_ref`'s basename and has
/// been deleted in favour of [`ExecutionNode::bundle_id`]. Here, Kubernetes'
/// `$(VAR)` substitution in a
/// container's `env` only resolves against *other entries this same list
/// declares*, never against a value the image's own `Dockerfile` sets — so
/// naming `PATH` here at all means replacing it outright, and the literal
/// tail below is `python:3.12-slim`'s own default (measured directly,
/// `docker run --rm python:3.12-slim printenv PATH`, since
/// `runner.Dockerfile` sets no `ENV PATH` of its own and so never overrides
/// the base image's). If that base image's default `PATH` ever changes —
/// a different Debian release, a different Python image entirely — this
/// constant has to change with it, silently otherwise: `kubectl`, `helm`
/// and `istioctl` all live in `/usr/local/bin`, on this literal path, not
/// found any other way.
const PATH_VALUE: &str =
    "$(HOME)/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

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
/// derivable (`argo.rs:40`) — **including the `?sig=` that authorises it**.
///
/// # The three variables that used to sit beside this one are gone
///
/// `TEST_BUNDLE_TOKEN_URL`, `TEST_BUNDLE_CLIENT_ID` and
/// `TEST_BUNDLE_CLIENT_SECRET` were emitted here so the runner could perform a
/// `client_credentials` exchange **inside the pod** and call qa-catalog's
/// then-`.authenticated()` bundle route. They are deleted, and the deletion is
/// the point of the change that added `?sig=`:
///
/// * That client secret was deployment-wide, unexpiring, `fullScopeAllowed`
///   and hardcoded to one tenant — and it sat in the environment of a process
///   tree whose whole job is running **tenant-authored pytest**. Any of that
///   code could read it, mint a token and call every `.authenticated()` route
///   in all four gears. The runner `NetworkPolicy` did not mitigate it: it
///   allow-listed both destinations the credential needed.
/// * Because the `tenant_id` claim was hardcoded, every tenant but the seeded
///   one got a 404 on its own bundles.
///
/// What replaces them is [`ExecutionNode::bundle_token`] — an HMAC tag
/// qa-catalog minted over `(bundle_id, tenant_id)`, carried in this URL's
/// query string. **This value is echoed into the pod log and rendered in the
/// run view**, so `deploy/runner/entrypoint.sh` and `fetch_bundle.py` both
/// strip the query string before printing it.
const TEST_BUNDLE_URL_VAR: &str = "TEST_BUNDLE_URL";

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
            // `optional: true` was the source system's own flag (`argo.rs:449`),
            // and the port once made it contractual: an `EnvSource::Secret`
            // that did not resolve left the variable unset and the run
            // proceeded, failing the suite the way a broken product fails —
            // the operator read a red test, not a missing credential.
            //
            // `optional: false` inverts that on purpose: the kubelet refuses
            // to start the pod, and Argo reports `CreateContainerConfigError`
            // naming the missing `Secret` in the pod's events. A pre-flight
            // check that asked the API server whether the Secret exists first
            // is not the fix available here — ADR-0008 cut `secrets` out of
            // qa-runs' RBAC by construction, so this gear cannot ask that
            // question — and the pod's own refusal is the achievable form of
            // the same outcome.
            EnvSource::Secret(reference) => json!({
                "name": name,
                "valueFrom": {
                    "secretKeyRef": {
                        "name": secret_name(&cfg.secret_name_prefix, spec.tenant_id, reference.as_str()),
                        "key": cfg.secret_key,
                        "optional": false,
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
    if let Some(base) = base {
        let base = base.trim_end_matches('/');
        // The id comes off the node, not off a parse of `bundle_ref`'s
        // basename. That parse used to live here as `bundle_id()` and its own
        // doc called it "a hidden coupling, and named as one": it assumed
        // `LocalFsBundleStore`'s `<uuid>.tar.gz` naming, so a deployment with a
        // different `BundleStore` would have broken it silently. `ExecutionNode`
        // carries the id now, which is what that doc said the right fix was.
        //
        // `bundle_token` is percent-encoding-free by construction (it is
        // `hex::encode`'s output -- `[0-9a-f]` only), so it needs no escaping
        // to survive a query string; an empty one still renders, because a
        // `?sig=` that fails to verify is the loud failure and a *missing*
        // `sig` would be a 400 from the extractor instead.
        env.push(json!({
            "name": TEST_BUNDLE_URL_VAR,
            "value": format!(
                "{base}/qa/v1/test-bundles/{}?sig={}",
                node.bundle_id, node.bundle_token,
            ),
        }));
    }
    // `HOME` then `PATH` — order matters: `PATH`'s value references `$(HOME)`,
    // and Kubernetes only resolves a `$(VAR)` reference against an entry
    // earlier in this same list, never against the image's own `Dockerfile`
    // environment. Pushed last and unconditionally, like `TEST_FILES` above,
    // so nothing a run's own `spec.env` happens to declare under either name
    // can shadow the writable home this task's hardening requires (see
    // `HOME_VALUE`'s and `PATH_VALUE`'s own docs for why each is needed).
    env.push(json!({ "name": "HOME", "value": HOME_VALUE }));
    env.push(json!({ "name": "PATH", "value": PATH_VALUE }));
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
        // The container-level half of the pod's unprivileged posture: no
        // escalation and every capability dropped. `runAsNonRoot`/`runAsUser`
        // live on the pod's own securityContext (`build`, above) and apply
        // here too; they are not repeated per container.
        "securityContext": {
            "allowPrivilegeEscalation": false,
            "capabilities": { "drop": ["ALL"] },
        },
        // Deployment-level only — see `ArgoExecutorConfig::runner_resources`'
        // own doc on why `RunnerSpec` gets no override field for this.
        // `max_concurrent_runs` bounds how many runs are admitted, not what
        // one consumes, so without this a single heavy suite could evict its
        // neighbours off the node.
        "resources": {
            "requests": {
                "cpu": cfg.runner_resources.cpu_request,
                "memory": cfg.runner_resources.memory_request,
            },
            "limits": {
                "cpu": cfg.runner_resources.cpu_limit,
                "memory": cfg.runner_resources.memory_limit,
            },
        },
    });

    // The port carries each mount's **file** path so the caller can assert
    // equality with the variable that names it; a secret volume mounts a
    // **directory**. So the directory is the path's parent and `items` maps the
    // configured key onto the file name, which is what puts the material at
    // exactly the path the plugin asked for.
    let mut mounts: Vec<Value> = secret_mounts(spec)
        .into_iter()
        .map(|mount| {
            json!({
                "name": mount.volume,
                "mountPath": split_mount_path(mount.path).0,
                "readOnly": true,
            })
        })
        .collect();
    // `/tmp` as a writable emptyDir: pip installs packages into the root
    // filesystem (`--no-cache-dir` only skips its download cache, not the
    // install target), which is why `readOnlyRootFilesystem` is not set
    // above — but the entrypoint already targets `/tmp` specifically for
    // pytest's own cache (`-o cache_dir=/tmp/pytest_cache`), so this only
    // makes that existing assumption explicit rather than leaving it to
    // whatever the image's own filesystem happens to allow.
    mounts.push(json!({ "name": TMP_VOLUME_NAME, "mountPath": "/tmp" }));
    // `/work` as a writable emptyDir — fix round 4, [`WORK_VOLUME_NAME`]'s
    // own doc has the measured failure this closes. `runner.Dockerfile`
    // creates `/work` as root with no `USER`, and the pod's non-root,
    // non-zero `run_as_user` cannot write into it otherwise.
    mounts.push(json!({ "name": WORK_VOLUME_NAME, "mountPath": "/work" }));
    container["volumeMounts"] = json!(mounts);

    json!({
        "name": task_name(&node.name, index),
        // Argo copies a template's metadata onto its pod, which is how `watch`
        // recovers the un-sanitised node name (`NODE_ANNOTATION`) and how the
        // isolation label reaches the pod for `runner-networkpolicy.yaml`'s
        // `podSelector` to select on (`NETWORK_ISOLATION_LABEL`).
        "metadata": {
            "annotations": { NODE_ANNOTATION: node.name },
            "labels": { NETWORK_ISOLATION_LABEL: NETWORK_ISOLATION_LABEL_VALUE },
        },
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
        // Unprivileged by construction: the runner image keeps kubectl, helm
        // and istioctl (the product's suites shell out to them), and what
        // makes that safe is no root and no capabilities — not the absence of
        // the pod's ServiceAccount token, which stays mounted (below).
        // `readOnlyRootFilesystem` is deliberately not set: pip installs
        // packages into the root filesystem (the bundle itself and pytest's
        // cache have their own emptyDirs, `/work` and `/tmp` — see
        // `WORK_VOLUME_NAME`'s and `TMP_VOLUME_NAME`'s own docs).
        "securityContext": {
            "runAsNonRoot": true,
            "runAsUser": cfg.run_as_user,
            // Fix round 8: the fourth field this pod's design specified
            // alongside the three above, dropped between design and code and
            // caught only on a live cluster — a Secret volume (a product
            // plugin's `MountSpec::Secret`, e.g. `qa-vhi-product-plugin`'s
            // SSH key) is root-owned unless `fsGroup` is set, and
            // `runAsUser` alone does not change that. See `fs_group`'s own
            // doc (`config.rs`) for the measurement that the mounted file's
            // declared mode does not also need to change.
            "fsGroup": cfg.fs_group,
            "seccompProfile": { "type": "RuntimeDefault" },
        },
        // No `automountServiceAccountToken: false` here: Argo's own executor
        // (the `wait` container) authenticates to the API server with this
        // same mounted token to write the pod's `workflowtaskresults` object,
        // so unmounting it does not narrow the runner's reach — it breaks
        // reporting. Measured on the dev cluster, 2026-08-27
        // (`config.rs:440-449`): with the token unmounted, the suite runs to
        // completion and then fails with exit code 64,
        // `workflowtaskresults.argoproj.io is forbidden`, because `wait`
        // could not authenticate at all. The pod's identity is constrained
        // instead by *which* ServiceAccount it runs as — a declared, minimal
        // one (`spec.serviceAccountName`, below) scoped by its own Role to
        // `create` on `workflowtaskresults.argoproj.io` and nothing else, not
        // by denying the account its token.
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

    let mut volumes: Vec<Value> = secret_mounts(spec)
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
                    "secretName": secret_name(&cfg.secret_name_prefix, spec.tenant_id, mount.credstore_ref),
                    "items": [item],
                }
            })
        })
        .collect();
    // Present on every workflow, secrets or not — every runner container
    // mounts it (`template`, above).
    volumes.push(json!({ "name": TMP_VOLUME_NAME, "emptyDir": {} }));
    // Fix round 4: same reasoning as `/tmp`, above, for `/work` —
    // `WORK_VOLUME_NAME`'s own doc has the measured failure. Starts empty;
    // `fetch_bundle.py` populates it after the mount exists.
    volumes.push(json!({ "name": WORK_VOLUME_NAME, "emptyDir": {} }));
    workflow_spec["volumes"] = json!(volumes);

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

    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{
        APP_LABEL_VALUE, NETWORK_ISOLATION_LABEL, NETWORK_ISOLATION_LABEL_VALUE, NODE_ANNOTATION,
        build, reject_unrenderable_mounts,
    };
    use crate::config::ArgoExecutorConfig;
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

    /// The bundle this module's fixture node names. A real UUID, and
    /// **deliberately unrelated to `bundle_ref`'s basename below**: the URL is
    /// now built from this field, so a test whose two values agreed would still
    /// pass if the adapter went back to parsing the path.
    const BUNDLE_ID: Uuid = uuid::uuid!("2f1c9a70-0000-4000-8000-000000000001");

    /// The hex tag qa-catalog would have minted for [`BUNDLE_ID`]. Only its
    /// shape matters here — this module never verifies it, it renders it.
    const BUNDLE_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn node(name: &str) -> ExecutionNode {
        ExecutionNode {
            name: name.to_owned(),
            bundle_ref: "/var/lib/qa-catalog/bundles/99999999-9999-4999-8999-999999999999.tar.gz"
                .to_owned(),
            bundle_id: BUNDLE_ID,
            bundle_token: BUNDLE_TOKEN.to_owned(),
            test_files: vec!["tests/test_smoke.py".to_owned()],
        }
    }

    /// The fixed tenant every golden test in this module runs under — its
    /// 36-character form is what actually consumes most of the `Secret` name
    /// budget these tests exercise, and matching `naming.rs`'s own test
    /// tenant keeps the two files' derived-name expectations comparable.
    const TENANT: Uuid = uuid::uuid!("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");

    fn spec(nodes: Vec<ExecutionNode>) -> RunSpec {
        RunSpec {
            run_id: Uuid::nil(),
            tenant_id: TENANT,
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
        // Premise changed by Task 1: the pod always carries a `/tmp` emptyDir
        // (pip installs into the root filesystem, so pytest's own cache gets
        // a dedicated writable volume rather than sharing it), so "no
        // secrets" no longer means "no volumes at all". Fix round 4 added a
        // second always-present volume, `/work` (`WORK_VOLUME_NAME`'s own
        // doc has the measured failure without it) — so "no secrets" now
        // means exactly these two, not one.
        let volumes = workflow["spec"]["volumes"].as_array().expect("volumes");
        assert_eq!(
            volumes.len(),
            2,
            "no kubeconfig means no secret volume, but /tmp and /work are \
             always present"
        );
        assert_eq!(volumes[0]["name"], "tmp");
        assert_eq!(volumes[0]["emptyDir"], json!({}));
        assert_eq!(volumes[1]["name"], "work");
        assert_eq!(volumes[1]["emptyDir"], json!({}));
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

    /// The Rust half of the two-sided assertion `NETWORK_ISOLATION_LABEL`'s
    /// own doc names: every template — and so every pod Argo creates from
    /// one — carries the exact key and value
    /// `deploy/helm/qa-platform/templates/runner-networkpolicy.yaml`'s
    /// `podSelector` selects on. If a future edit changes either constant's
    /// value without also changing that chart file's literal,
    /// `deploy/helm/tests/check_runner_networkpolicy.py` fails — not this
    /// test, which only checks that `build` actually uses the constants it
    /// claims to (asserted against the constants themselves, not a second
    /// literal, because *that* half of the contract is "does the code do
    /// what the doc says", not "do the two sides agree").
    #[test]
    fn every_template_carries_the_network_isolation_label() {
        let workflow = build(&spec(vec![node("a"), node("b")]), &cfg(), "w");
        let templates = workflow["spec"]["templates"].as_array().expect("templates");
        // Template 0 is the DAG for a two-node run and carries no metadata of
        // its own; every node template (1..) must carry the label.
        let node_templates = &templates[1..];
        assert!(
            !node_templates.is_empty(),
            "premise: this run has node templates to check"
        );
        for template in node_templates {
            assert_eq!(
                template["metadata"]["labels"][NETWORK_ISOLATION_LABEL],
                NETWORK_ISOLATION_LABEL_VALUE,
                "every runner pod must carry the isolation label, or the \
                 chart's NetworkPolicy selects nothing and fails open \
                 while looking installed"
            );
        }

        // The single-node shape too — no DAG wrapper, template 0 is the node.
        let single = build(&spec(vec![node("a")]), &cfg(), "w");
        assert_eq!(
            single["spec"]["templates"][0]["metadata"]["labels"][NETWORK_ISOLATION_LABEL],
            NETWORK_ISOLATION_LABEL_VALUE
        );
    }

    /// Fix round 5, measured on a live cluster: every runner container
    /// carries a writable `$HOME`, because `runner.Dockerfile`'s `nobody`
    /// account (the pod's hardened, non-root `run_as_user`) otherwise gets
    /// `/nonexistent` from the base image, and pip's own per-user install
    /// fallback needs somewhere real to write. `$PATH` carries that user
    /// install's `bin` directory alongside the base image's own directories
    /// (`kubectl`/`helm`/`istioctl` all live in `/usr/local/bin`), because a
    /// tenant-supplied `requirements.txt` cannot be assumed to need only
    /// importable packages and not a console script one of them installs.
    ///
    /// Checked on every node template, not just the first: `HOME_VALUE`'s
    /// own doc explains why a `$(VAR)`-style reference can only be built
    /// from entries in the same list, so a regression that silently reverted
    /// to a per-node-only push (rather than the unconditional one `node_env`
    /// makes) would otherwise slip past a single-node-only test.
    #[test]
    fn every_container_carries_a_writable_home_and_an_extended_path() {
        let workflow = build(&spec(vec![node("a"), node("b")]), &cfg(), "w");
        let templates = workflow["spec"]["templates"].as_array().expect("templates");
        let node_templates = &templates[1..];
        assert!(
            !node_templates.is_empty(),
            "premise: this run has node templates to check"
        );
        for template in node_templates {
            let env = template["container"]["env"]
                .as_array()
                .expect("an env list")
                .iter()
                .map(|entry| (entry["name"].as_str().unwrap_or_default(), entry))
                .collect::<BTreeMap<_, _>>();

            assert_eq!(
                env["HOME"]["value"], "/tmp",
                "must be the SAME emptyDir the container already mounts \
                 writable, not a path nothing backs"
            );
            let path = env["PATH"]["value"].as_str().expect("PATH is a literal");
            assert!(
                path.starts_with("$(HOME)/.local/bin:"),
                "the per-user install location must come first: {path}"
            );
            for tool_dir in ["/usr/local/bin", "/usr/bin", "/bin"] {
                assert!(
                    path.contains(tool_dir),
                    "must still carry the base image's own {tool_dir}, or \
                     kubectl/helm/istioctl (and python3 itself) stop \
                     resolving: {path}"
                );
            }
        }
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
    ///
    /// `optional` used to assert `true`, the source system's own flag
    /// (`argo.rs:449`), which the port once made contractual: an unresolvable
    /// reference left the variable unset and the run proceeded as though the
    /// product itself were broken. It now asserts `false` — the pod refuses
    /// to start and Argo reports `CreateContainerConfigError` naming the
    /// Secret, which is the achievable form of a pre-flight check: ADR-0008
    /// cut `secrets` out of qa-runs' RBAC by construction, so this gear
    /// cannot ask the API server whether the Secret exists first.
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
            secret["name"], "qa-platform-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee-3cfaa05d68d19420",
            "the reference's punctuation each becomes a dash in the readable head, and the \
             digest suffix is what actually keeps the mapping injective"
        );
        assert_eq!(secret["key"], "value");
        assert_eq!(
            secret["optional"], false,
            "an unresolvable reference must stop the pod, not leave the \
             variable unset: CreateContainerConfigError names the Secret, \
             which is the achievable substitute for the pre-flight check \
             ADR-0008 makes unavailable to this gear"
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
        assert_eq!(
            volume["secret"]["secretName"],
            "qa-platform-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee-4a90e664f3e346d1"
        );
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

        // 2 secret volumes plus the always-present /tmp and /work emptyDirs.
        let volumes = workflow["spec"]["volumes"].as_array().expect("volumes");
        assert_eq!(volumes.len(), 4);
        assert_eq!(volumes[0]["name"], "mount-0");
        assert_eq!(volumes[1]["name"], "mount-1");
        assert_eq!(volumes[1]["secret"]["items"][0]["path"], "license.key");
        assert_eq!(volumes[2]["name"], "tmp");
        assert_eq!(volumes[3]["name"], "work");

        let mounts = workflow["spec"]["templates"][0]["container"]["volumeMounts"]
            .as_array()
            .expect("volumeMounts");
        assert_eq!(mounts.len(), 4);
        assert_eq!(mounts[0]["name"], "mount-0");
        assert_eq!(mounts[0]["mountPath"], "/.kube");
        assert_eq!(mounts[1]["name"], "mount-1");
        assert_eq!(mounts[1]["mountPath"], "/etc/product");
        assert_eq!(mounts[2]["name"], "tmp");
        assert_eq!(mounts[2]["mountPath"], "/tmp");
        assert_eq!(mounts[3]["name"], "work");
        assert_eq!(mounts[3]["mountPath"], "/work");
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
        let workflow = build(&run, &cfg(), "w");
        let volumes = workflow["spec"]["volumes"].as_array().expect("volumes");
        assert_eq!(
            volumes.len(),
            2,
            "premise: build renders nothing for the refused mount -- only the \
             always-present /tmp and /work volumes -- which is exactly why \
             the refusal has to come first"
        );
        assert_eq!(volumes[0]["name"], "tmp");
        assert_eq!(volumes[1]["name"], "work");
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

    /// The bundle URL is built from `ExecutionNode::bundle_id` and carries the
    /// node's own download tag, and it is only emitted when a base URL is
    /// configured.
    ///
    /// **The fixture node's `bundle_ref` basename is a different UUID on
    /// purpose.** The adapter used to recover the id by parsing that basename —
    /// a hidden coupling to `LocalFsBundleStore`'s file naming, which a
    /// deployment with a different store would have broken silently. Making
    /// the two values disagree is what turns "the id comes off the node" from a
    /// claim into an assertion: if the parse came back, this test fails.
    #[test]
    fn a_bundle_url_is_built_from_the_nodes_id_and_carries_its_signature() {
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
            format!("http://10.0.0.1:8080/qa/v1/test-bundles/{BUNDLE_ID}?sig={BUNDLE_TOKEN}"),
            "the id is the node's own, not a parse of bundle_ref's basename, and the \
             download tag rides the query string"
        );
    }

    /// **No runner pod may ever carry an `IdP` credential again**, whatever
    /// the configuration says.
    ///
    /// This is the inverse of the test it replaces, which asserted that
    /// `TEST_BUNDLE_TOKEN_URL`, `TEST_BUNDLE_CLIENT_ID` and
    /// `TEST_BUNDLE_CLIENT_SECRET` *were* emitted. They carried the
    /// confidential secret of a `fullScopeAllowed` service-account client into
    /// the environment of a pod that runs tenant-authored pytest — see
    /// `TEST_BUNDLE_URL_VAR`'s doc for what that code could then reach, and
    /// why the per-bundle signature replaced it rather than joining it.
    ///
    /// It sweeps the **whole rendered workflow**, not just the env map, so a
    /// future revision cannot reintroduce the credential through a volume, an
    /// `envFrom` or an annotation and still pass.
    #[test]
    fn no_rendered_workflow_carries_a_bundle_oidc_credential() {
        let with_base = ArgoExecutorConfig {
            bundle_base_url: Some("http://10.0.0.1:8080/".to_owned()),
            ..cfg()
        };
        for config in [&cfg(), &with_base] {
            let workflow = build(&spec(vec![node("a")]), config, "w");
            let rendered = serde_json::to_string(&workflow).expect("the workflow serialises");
            for banned in [
                "TEST_BUNDLE_TOKEN_URL",
                "TEST_BUNDLE_CLIENT_ID",
                "TEST_BUNDLE_CLIENT_SECRET",
                "client_credentials",
            ] {
                assert!(
                    !rendered.contains(banned),
                    "{banned} must not appear anywhere in a rendered workflow: a runner pod \
                     executes tenant-authored code, and an IdP credential in its process \
                     tree is readable by that code"
                );
            }
        }
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

    /// The pod runs unprivileged but keeps its `ServiceAccount` token mounted.
    ///
    /// The runner image carries `kubectl`, `helm` and `istioctl` deliberately
    /// (`runner.Dockerfile:61-72`) — the product's suites shell out to them. What
    /// makes that safe is no root, no privilege escalation and no capabilities —
    /// **not** an unmounted token. A workflow pod has two containers, and the
    /// token is Argo's own executor's (`wait`), not just the runner's: `wait`
    /// authenticates to the API server with it to write the pod's
    /// `workflowtaskresults` object. Unmounting it does not narrow what the
    /// runner's `kubectl` can reach — the identity is what constrains that
    /// (`spec.serviceAccountName`, a declared, minimal account) — it stops
    /// `wait` from authenticating at all, and the failure mode is worse than a
    /// hardening win: measured on the dev cluster, 2026-08-27
    /// (`config.rs:440-449`), the suite runs to completion and only then fails
    /// with exit code 64, `workflowtaskresults.argoproj.io is forbidden`. A
    /// future hardening pass must not re-set this to `false`.
    #[test]
    fn the_pod_is_unprivileged_but_keeps_its_service_account_token() {
        let workflow = build(&spec(vec![node("repo-smoke")]), &cfg(), "smoke-tests-1");
        let pod = &workflow["spec"]["securityContext"];

        assert_eq!(pod["runAsNonRoot"], true, "the pod must not run as root");
        assert!(
            pod["runAsUser"].as_u64().is_some_and(|uid| uid != 0),
            "runAsUser must be set and non-zero"
        );
        assert!(
            pod["fsGroup"].as_u64().is_some_and(|gid| gid != 0),
            "fsGroup must be set and non-zero -- fix round 8: this pod's \
             design specified it alongside runAsNonRoot/runAsUser/\
             seccompProfile from the start, and its absence is exactly what \
             let a product plugin's Secret-mounted credential (e.g. \
             qa-vhi-product-plugin's SSH key) end up unreadable by this \
             pod's own non-root uid"
        );
        assert_eq!(pod["seccompProfile"]["type"], "RuntimeDefault");
        assert!(
            workflow["spec"]
                .get("automountServiceAccountToken")
                .is_none(),
            "no override here, so Kubernetes' default (mounted) applies: \
             Argo's own executor container (`wait`) needs this token to \
             report the run's result, and denying it fails the run after it \
             has already produced all of its output (exit code 64, \
             workflowtaskresults.argoproj.io is forbidden -- config.rs:440-449)"
        );

        let container = &workflow["spec"]["templates"][0]["container"];
        assert_eq!(
            container["securityContext"]["allowPrivilegeEscalation"],
            false
        );
        assert_eq!(
            container["securityContext"]["capabilities"]["drop"][0],
            "ALL"
        );
    }

    /// Every runner container declares requests and limits.
    ///
    /// `max_concurrent_runs` limits how many runs are admitted, not what they
    /// eat; without these a single tenant's heavy suite evicts its neighbours
    /// off the node.
    #[test]
    fn the_container_declares_requests_and_limits() {
        let workflow = build(&spec(vec![node("repo-smoke")]), &cfg(), "smoke-tests-1");
        let resources = &workflow["spec"]["templates"][0]["container"]["resources"];

        for (bucket, field) in [
            ("requests", "cpu"),
            ("requests", "memory"),
            ("limits", "cpu"),
            ("limits", "memory"),
        ] {
            assert!(
                resources[bucket][field]
                    .as_str()
                    .is_some_and(|v| !v.is_empty()),
                "resources.{bucket}.{field} must be set"
            );
        }
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
