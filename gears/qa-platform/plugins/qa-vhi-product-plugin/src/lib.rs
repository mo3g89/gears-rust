//! The QA Platform product plugin for Virtuozzo Hybrid Infrastructure.
//!
//! The second product plugin, and the first whose target is not a Kubernetes
//! cluster: it reaches a management node over SSH and drives the `vinfra` CLI
//! there. Everything the platform knows about VHI -- the credential form, the
//! observed facts, the run variables, the runner shape -- is declared here and
//! nowhere else.
//!
//! # The invariant every module here maintains
//!
//! **No value derived from credential material is ever formatted.** Failures
//! leave this crate as `PluginFailure`, whose `detail` is `&'static str` and
//! therefore cannot be produced from runtime bytes; the one sanctioned
//! exception is text a *remote* sent back, which travels on `remote_message`.
//!
//! `qa_connector_ssh::classify` is one writer of that field but **not the
//! only one**, which this doc claimed until 2026-09-09: [`observe`] writes it
//! directly at two sites of its own -- the `NotFound` reading of a release
//! command that exited non-zero (carrying the remote's stderr) and the
//! `Malformed` reading of a release file that would not parse (carrying the
//! whole raw `/etc/hci-release` line, which is the point: `rawRelease` is how
//! an operator sees what the node actually said). Both carry remote text and
//! neither carries anything derived from a credential, so the invariant holds
//! -- but a reader looking for every `remote_message` writer must look here
//! too, not only in the connector.
//!
//! # How it becomes reachable
//!
//! [`gear`] is the only part of this crate that is not a library function: it
//! publishes the GTS instance and registers the plugin in the `ClientHub`
//! under that id. Nothing *resolves* it yet — Task 12's `QaProductRegistry`
//! does that — so linking this crate into a server is additive.

mod config;
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

/// The VHI product plugin.
///
/// Stateless, and deliberately so: it caches no credential, so there is
/// nothing for [`QaProductPluginV1::health_check`] -- whose signature carries
/// no credential material at all -- to echo. Everything it needs arrives on
/// the [`EnvironmentHandle`] of the call being served.
#[derive(Debug, Default, Clone, Copy)]
pub struct VhiProductPlugin;

#[async_trait]
impl QaProductPluginV1 for VhiProductPlugin {
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

    /// VHI inherits the deployment-wide runner image: `paramiko` arrives
    /// through the suite's own `requirements.txt`, which the runner's
    /// entrypoint installs, so there is nothing product-specific to bake in.
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
