//! An Argo Workflows adapter for the
//! [`RunExecutor`](crate::domain::ports::run_executor::RunExecutor) port.
//!
//! **Behind the non-default `argo` cargo feature**, and that is a governance
//! requirement rather than a build convenience. ADR-0001 chose the platform
//! serverless runtime specifically to remove Kubernetes from the product, and
//! its Confirmation criterion is "no `kube`/`k8s-openapi` in any qa-platform
//! crate". The waiver recorded in that ADR on 2026-08-27 permits this adapter
//! only if the dependency is optional, the port is untouched, `domain/` never
//! learns Kubernetes exists, and the mock stays the default. All four hold; the
//! `Cargo.toml` `[features]` comment carries the verification commands.
//!
//! # Where the mapping comes from
//!
//! The port was "shaped from the source system's submission contract"
//! (`run_executor.rs:3-5`), so this adapter maps the port back onto its own
//! origin, `vhp-testrunner/manager/src/services/argo.rs`. Every non-obvious
//! shape below cites the line it came from. Three things the port *added* have
//! no source to port from and are built here:
//!
//! * **`watch`** — the source system polls and scrapes; nothing streams. See
//!   [`watch`].
//! * **`Canceled` and `TimedOut`** as outcomes distinct from `Failed`. See
//!   [`watch::outcome_of`].
//! * **Re-attachability.** Structurally free here: the workflow object and its
//!   pod logs are both still in the cluster and both re-readable from byte
//!   zero, and `RunsRepository::upsert_test_result` replaces a row inside one
//!   `SERIALIZABLE` transaction, so a replay is idempotent.
//!
//! # What this first cut does not do, stated rather than left to be discovered
//!
//! 1. **It does not materialise any Kubernetes `Secret`.** A
//!    [`SecretRef`](crate::domain::ports::run_executor::SecretRef) is turned
//!    into a `secretKeyRef` or a secret volume by *name derivation*
//!    ([`naming::secret_name`]) and the kubelet does the resolving — which is
//!    exactly how the source system works (`argo.rs:504-521`) and what keeps
//!    "qa-runs never reads a secret's contents" (`run_executor.rs:85-86`)
//!    literally true of this code. It also means **a run with a platform will
//!    not start until somebody creates that Secret**: the volume is required,
//!    so the pod stays `Pending`. Who does that (qa-environments, per the
//!    scoping document's design A) is open decision D4 and is not this
//!    adapter's to take, because taking the other option — resolving credstore
//!    material here — would falsify the port's central claim.
//! 2. **It does not solve bundle download authentication.**
//!    `GET /qa/v1/test-bundles/{id}` is `.authenticated()`
//!    (`qa-catalog/.../api/rest/routes/bundles.rs:31`) where the source
//!    system's equivalent had no middleware at all. `TEST_BUNDLE_URL` is
//!    emitted when [`crate::config::ArgoExecutorConfig::bundle_base_url`] is
//!    set, and a pod fetching it today gets a 401.
//!
//!    **`s2s_oauth` is not the answer as things stand, measured rather than
//!    assumed**: the stack's config sets it, and the comment above that setting
//!    says why it cannot be used — "Nothing in this build ever calls it … It is
//!    therefore configured to be *valid*, not to be used, and no Keycloak
//!    client backs it" (`gears/qa-platform/config/qa-platform-stack.yaml:313-325`).
//!    So a pod presenting client credentials has no client to present.
//!
//!    The recommendation, consistent with (1) and with the no-material rule: a
//!    Keycloak service-account client for the runner, its secret stored in a
//!    Kubernetes `Secret`, and the workflow given a `secretKeyRef` to it — a
//!    reference the adapter derives and never resolves, the same shape as every
//!    other secret here. That needs a realm client created, which is a human's
//!    decision (D5), so it is recorded and not guessed at.
//! 3. **Per-test log slices are not carried**, because `TestObservation` has no
//!    field for them and the port is frozen. The UI's "Per-test logs are not
//!    available in this deployment" notice stays true. See [`markers`].
//! 4. **Log following is sequential across nodes.** See [`watch`].

pub mod markers;
pub mod naming;
pub mod watch;
pub mod workflow;

use std::collections::BTreeSet;

use async_trait::async_trait;
use kube::api::{
    Api, ApiResource, DynamicObject, GroupVersionKind, ListParams, Patch, PatchParams, PostParams,
};
use kube::{Client, Config};
use serde_json::json;
use tracing::{info, warn};

use crate::config::ArgoExecutorConfig;
use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{ExecutionRef, ExecutionStream, RunExecutor, RunSpec};
use crate::infra::executor::argo::workflow::{APP_LABEL, APP_LABEL_VALUE};

/// Argo phases this adapter counts as alive.
///
/// The source system's `is_active` is `Running | Pending` (`models.rs:114-116`).
/// **The empty case is added, and it is the one that matters**: a workflow the
/// API server has accepted but the controller has not yet given a `status` to
/// has no phase at all. Reading that as "not active" would have
/// [`RunExecutor::list_active`] omit a run seconds after
/// [`RunExecutor::start`] returned, and the dispatcher would release its
/// platform claim while the run was starting — the exact window in which a
/// second run gets admitted beside an exclusive one.
const ACTIVE_PHASES: [&str; 2] = ["Pending", "Running"];

/// Submission attempts before giving up on finding an unused workflow name.
///
/// Three, matching the source system's own retry budget (`argo.rs:418`).
const ATTEMPTS: usize = 3;

/// `argoproj.io/v1alpha1 Workflow`, as a dynamic resource.
///
/// A `DynamicObject` rather than generated Argo types, which is what lets this
/// adapter carry no Argo crate at all — the source system's own choice
/// (`argo.rs:17-30`).
///
/// `pub` because `tests/argo_cluster.rs` needs it to clean up after itself, and
/// an integration test compiles the library without `cfg(test)`. Harmless: it
/// returns a constant.
#[must_use]
pub fn workflow_resource() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "argoproj.io",
        "v1alpha1",
        "Workflow",
    ))
}

/// The Argo-backed execution plane.
#[derive(Clone)]
pub struct ArgoRunExecutor {
    client: Client,
    config: ArgoExecutorConfig,
}

impl std::fmt::Debug for ArgoRunExecutor {
    /// `kube::Client` is not `Debug`, and printing it would be printing
    /// credentials anyway.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArgoRunExecutor")
            .field("namespace", &self.config.namespace)
            .field("runner_image", &self.config.runner_image)
            .finish_non_exhaustive()
    }
}

impl ArgoRunExecutor {
    /// Check the two halves of bundle delivery against each other.
    ///
    /// Split out of [`Self::connect`] because it is a rule about the config and
    /// not about the cluster — and because `clippy::cognitive_complexity`
    /// denies a `connect` that carries both.
    ///
    /// # Errors
    /// When `bundle_auth` is configured without a `bundle_base_url`, or when
    /// any `bundle_auth` field is blank.
    fn check_bundle_delivery(config: &ArgoExecutorConfig) -> anyhow::Result<()> {
        // The two halves of bundle delivery, checked together because either
        // alone produces a run that executes NO TEST CONTENT and still finishes.
        // That is the failure mode worth a boot check: a pytest run over an
        // empty directory exits 5 ("no tests collected"), and a deployment
        // reading only the run's state sees a red run with no explanation, or —
        // depending on how the runner treats it — a green one with zero tests.
        let bundle_base_url = config
            .bundle_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match (bundle_base_url, config.bundle_auth.as_ref()) {
            (None, Some(_)) => anyhow::bail!(
                "qa-runs.argo.bundle_auth is configured but bundle_base_url is \
                 not: the pod would receive credentials and no URL to use them \
                 on, so it would run no tests at all"
            ),
            (Some(url), None) => warn!(
                bundle_base_url = %url,
                "qa-runs.argo.bundle_base_url is set with no bundle_auth: \
                 GET /qa/v1/test-bundles/{{id}} is authenticated, so every \
                 runner pod will get a 401 on download. Configure \
                 qa-runs.argo.bundle_auth."
            ),
            (None, None) => warn!(
                "qa-runs.argo.bundle_base_url is unset: runner pods receive \
                 TEST_BUNDLE_REF (a path on the qa-catalog gear's own \
                 filesystem) and no way to fetch the bundle"
            ),
            (Some(_), Some(auth)) => {
                if auth.token_url.trim().is_empty()
                    || auth.client_id.trim().is_empty()
                    || auth.client_secret_secret.trim().is_empty()
                    || auth.client_secret_key.trim().is_empty()
                {
                    anyhow::bail!(
                        "qa-runs.argo.bundle_auth has a blank token_url, \
                         client_id, client_secret_secret or client_secret_key"
                    );
                }
            }
        }

        Ok(())
    }

    /// Connect to the API server and check the connection.
    ///
    /// The check is deliberate: an executor that constructs happily and fails
    /// on the first dispatch turns a misconfiguration into a failed run, while
    /// this turns it into a boot failure an operator sees at once.
    ///
    /// # Errors
    /// When the kubeconfig cannot be read or built, when a client cannot be
    /// constructed from it, or when the `Workflow` collection cannot be listed
    /// — which is also the `argoproj.io` CRD presence check and the RBAC check.
    pub async fn connect(config: ArgoExecutorConfig) -> anyhow::Result<Self> {
        if config.runner_image.trim().is_empty() {
            anyhow::bail!(
                "qa-runs.argo.runner_image must be set: an unset image is an \
                 ImagePullBackOff minutes into every run"
            );
        }

        Self::check_bundle_delivery(&config)?;

        let kube_config = match config.kubeconfig_path.as_deref().map(str::trim) {
            Some(path) if !path.is_empty() => {
                let kubeconfig = kube::config::Kubeconfig::read_from(path)?;
                Config::from_custom_kubeconfig(
                    kubeconfig,
                    &kube::config::KubeConfigOptions::default(),
                )
                .await?
            }
            // In-cluster service-account credentials, else `KUBECONFIG`, else
            // `~/.kube/config` — `try_default`'s own order (`main.rs:57`).
            _ => Config::infer().await?,
        };
        let server = kube_config.cluster_url.to_string();
        let executor = Self {
            client: Client::try_from(kube_config)?,
            config,
        };

        let found = executor
            .workflows()
            .list(&ListParams::default().limit(1))
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "cannot list argoproj.io/v1alpha1 Workflow in namespace {}: {error}",
                    executor.config.namespace
                )
            })?;
        info!(
            server = %server,
            namespace = %executor.config.namespace,
            runner_image = %executor.config.runner_image,
            existing_workflows = found.items.len(),
            "argo executor connected"
        );
        Ok(executor)
    }

    /// The `Workflow` collection in the configured namespace.
    fn workflows(&self) -> Api<DynamicObject> {
        Api::namespaced_with(
            self.client.clone(),
            &self.config.namespace,
            &workflow_resource(),
        )
    }

    /// The name to submit under on attempt `attempt`.
    ///
    /// Attempt 0 is the sanitised run name, which is what an operator expects
    /// to see in a cluster console — the source system uses the run name *as*
    /// the workflow name (`argo.rs:420-422`). The later attempts exist because
    /// **`run_name` is unique per tenant, not per cluster**: two tenants can
    /// both mint `smoke-tests-1`, and the source system never had to think
    /// about that because it had one tenant. Its 409-retry loop
    /// (`argo.rs:677-686`) is kept; what it retries with is different, because
    /// qa-runs mints the name itself and the adapter may not renumber it.
    fn candidate_name(spec: &RunSpec, attempt: usize) -> String {
        let run_id = spec.run_id.simple().to_string();
        let base = naming::workflow_name(&spec.run_name, &run_id);
        match attempt {
            0 => base,
            1 => {
                let head = base.get(..base.len().min(48)).unwrap_or(&base);
                let short = run_id.get(..8).unwrap_or(&run_id);
                naming::workflow_name(&format!("{head}-{short}"), &run_id)
            }
            _ => naming::workflow_name(&format!("qa-{run_id}"), &run_id),
        }
    }
}

#[async_trait]
impl RunExecutor for ArgoRunExecutor {
    async fn start(&self, spec: RunSpec) -> Result<ExecutionRef, DomainError> {
        if spec.nodes.is_empty() {
            return Err(DomainError::ExecutorFailed(format!(
                "run {} was submitted with no execution nodes",
                spec.run_id
            )));
        }
        // Before the first submission attempt, not inside the retry loop: a
        // spec this adapter cannot render faithfully is not a transient
        // failure, and a mount it silently dropped would start a run without a
        // credential it was told to provide. See the function's own docs for
        // the two shapes and what implementing the first would require.
        workflow::reject_unrenderable_mounts(&spec)?;

        let mut last = String::new();
        for attempt in 0..ATTEMPTS {
            let name = Self::candidate_name(&spec, attempt);
            let object = workflow::build(&spec, &self.config, &name);
            let workflow: DynamicObject = serde_json::from_value(object).map_err(|error| {
                DomainError::ExecutorFailed(format!("could not build a Workflow object: {error}"))
            })?;

            match self
                .workflows()
                .create(&PostParams::default(), &workflow)
                .await
            {
                Ok(created) => {
                    let reference = created.metadata.name.unwrap_or(name);
                    info!(
                        run_id = %spec.run_id,
                        execution_ref = %reference,
                        nodes = spec.nodes.len(),
                        "submitted an argo workflow"
                    );
                    return Ok(ExecutionRef::new(reference));
                }
                Err(kube::Error::Api(api)) if api.code == 409 && attempt + 1 < ATTEMPTS => {
                    warn!(
                        run_id = %spec.run_id,
                        name = %name,
                        attempt = attempt + 1,
                        "workflow name already exists; retrying under a run-id-qualified name"
                    );
                    last = api.message;
                }
                Err(error) => {
                    return Err(DomainError::ExecutorFailed(format!(
                        "submitting workflow {name} failed: {error}"
                    )));
                }
            }
        }
        Err(DomainError::ExecutorFailed(format!(
            "could not find an unused workflow name for run {} after {ATTEMPTS} attempts: {last}",
            spec.run_id
        )))
    }

    async fn watch(&self, execution_ref: &ExecutionRef) -> Result<ExecutionStream, DomainError> {
        watch::start(self.client.clone(), self.config.clone(), execution_ref).await
    }

    async fn cancel(&self, execution_ref: &ExecutionRef) -> Result<(), DomainError> {
        // The source system's cancel, verbatim (`argo.rs:1447-1453`): a blind
        // merge patch that writes no run state. The state change arrives through
        // the normal observation path.
        let patch = json!({ "spec": { "shutdown": "Terminate" } });
        match self
            .workflows()
            .patch(
                execution_ref.as_str(),
                &PatchParams::default(),
                &Patch::Merge(patch),
            )
            .await
        {
            Ok(_) => Ok(()),
            // Idempotence, which the port requires and the source system does
            // not implement: "cancelling an already-terminal or entirely
            // unknown execution succeeds" (`run_executor.rs:816-817`). A
            // workflow Argo has already garbage-collected is exactly this case
            // and is not a failure.
            Err(kube::Error::Api(api)) if api.code == 404 => {
                info!(
                    execution_ref = %execution_ref.as_str(),
                    "cancel targeted a workflow the cluster no longer has; treating as done"
                );
                Ok(())
            }
            Err(error) => Err(DomainError::ExecutorFailed(format!(
                "cancelling {} failed: {error}",
                execution_ref.as_str()
            ))),
        }
    }

    async fn list_active(&self) -> Result<BTreeSet<ExecutionRef>, DomainError> {
        let selector = format!("{APP_LABEL}={APP_LABEL_VALUE}");
        let list = self
            .workflows()
            .list(&ListParams::default().labels(&selector))
            .await
            // Never an empty set on error: the dispatcher skips the tick on
            // `Err` and would release every claim at once on `Ok(empty)`
            // (`run_executor.rs:849-853`).
            .map_err(|error| {
                DomainError::ExecutorFailed(format!("listing active workflows failed: {error}"))
            })?;

        Ok(list
            .items
            .into_iter()
            .filter(|workflow| {
                let phase = workflow
                    .data
                    .get("status")
                    .and_then(|status| status.get("phase"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                phase.is_empty() || ACTIVE_PHASES.contains(&phase)
            })
            .filter_map(|workflow| workflow.metadata.name)
            .map(ExecutionRef::new)
            .collect())
    }
}
