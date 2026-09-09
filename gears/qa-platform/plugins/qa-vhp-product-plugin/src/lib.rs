//! The QA Platform product plugin for Virtuozzo Hybrid Platform.
//!
//! The first real implementation of [`QaProductPluginV1`], and the one that
//! proves the contract carries a whole product: everything the platform knew
//! about VHP — the kubeconfig field, the `vpadm` namespace, the
//! `core-install-metadata` and `vp-gateway-hostnames` `ConfigMap`s, the base
//! URL that used to be a column called `vhp_base_url` — is declared and
//! implemented in this crate and nowhere else.
//!
//! # Where the code came from
//!
//! [`detect`] is copied from `qa-environments/src/domain/observation.rs` and
//! [`observe`]'s orchestration from
//! `qa-environments/src/infra/observer/kube_observer.rs`. Both originals stay
//! in place and stay working until Task 19 removes them: Phase C must not
//! change `qa-environments`' behaviour, so the two paths run side by side
//! until the one-way door in Phase E. Each module's header names its origin
//! and says what changed on the way over.
//!
//! [`run`] is the same arrangement one gear over: its four frozen variable
//! names and the base-domain derivation are copied from
//! `qa-runs/src/domain/runvars.rs`, which keeps its copies — and its own
//! tests over the precedence ladder — until Task 18 makes dispatch read them
//! from the plugin. That module's header, and a comment beside the constants
//! in `runvars.rs`, name each other.
//!
//! Task 8 took the *other* half of those files — what a Kubernetes cluster is,
//! as opposed to what vpadm installed on it — into `qa-connector-k8s`. That
//! division is why this crate holds no `kube` types and names no `kube`
//! dependency (ADR-0001): every read below goes through
//! [`qa_connector_k8s::KubeClient`].
//!
//! # The invariant every module here maintains
//!
//! **No value derived from credential material is ever formatted.** Not
//! `Display`, not `Debug`, not into a message, a log line or a DTO. Failures
//! leave this crate as [`PluginFailure`], whose `detail` is `&'static str` and
//! therefore cannot be produced from runtime bytes; the one sanctioned
//! exception is text a *remote* sent back, which
//! [`qa_connector_k8s::classify`] puts in `remote_message`. Layer 3 —
//! `qa_product_sdk::testing::assert_no_leak` — drives this plugin with planted
//! credential material in this crate's own test suite and fails the build if
//! any of it reaches a surface. See `conformance_tests.rs`.
//!
//! # How it becomes reachable
//!
//! [`gear`] is the only part of this crate that is not a library function: it
//! publishes the GTS instance and registers the plugin in the `ClientHub`
//! under that id. Nothing *resolves* it yet — Task 12's `QaProductRegistry`
//! does that — so linking this crate into a server is additive.

pub mod detect;
pub mod gear;
pub mod observe;
pub mod run;
pub mod schemas;

use async_trait::async_trait;
use qa_product_sdk::access::{RunAccess, RunVarContract, RunnerSpec};
use qa_product_sdk::descriptor::FieldDesc;
use qa_product_sdk::observation::{ObservedAttrs, PluginFailure, PluginObservation};
use qa_product_sdk::plugin::{
    CredentialClassification, CredentialInput, EnvironmentHandle, QaProductPluginV1,
};

/// The VHP product plugin.
///
/// Stateless, and deliberately so: it caches no credential, so there is
/// nothing for [`QaProductPluginV1::health_check`] — whose signature carries
/// no credential material at all — to echo. Everything it needs arrives on the
/// [`EnvironmentHandle`] of the call being served.
#[derive(Debug, Default, Clone, Copy)]
pub struct VhpProductPlugin;

impl VhpProductPlugin {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The plugin as this crate's own tests drive it.
    ///
    /// Identical to [`Self::new`] while the plugin is stateless. It exists as
    /// its own name so the conformance test names the *shape* it is asserting
    /// about rather than a constructor whose signature may later grow
    /// deployment configuration the harness has no business supplying.
    #[cfg(test)]
    #[must_use]
    pub const fn new_for_test() -> Self {
        Self::new()
    }
}

#[async_trait]
impl QaProductPluginV1 for VhpProductPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        schemas::credential_schema()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        schemas::observed_schema()
    }

    async fn validate_credentials(
        &self,
        input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        schemas::validate_credentials(input)
    }

    async fn observe(&self, env: &EnvironmentHandle<'_>) -> PluginObservation {
        observe::observe(env).await
    }

    async fn prepare_run_access(
        &self,
        env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        run::prepare_run_access(env)
    }

    /// VHP inherits the deployment-wide runner image, so `observed` is
    /// unread: **D11** puts the runner shape on the *product*, and a
    /// per-environment image would make a run's provenance unclear. The
    /// parameter stays in the signature for the products that will use it.
    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        run::runner()
    }

    fn env_contract(&self) -> RunVarContract {
        run::env_contract()
    }
}

#[cfg(test)]
#[path = "conformance_tests.rs"]
mod conformance_tests;

// Canned API-server answers `observe_tests` and `conformance_tests` both need.
// Declared here rather than inside either because neither owns the other, and
// the module's own header records what must stay out of it.
#[cfg(test)]
mod test_fixtures;
