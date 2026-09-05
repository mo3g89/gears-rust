//! The port D4's runner-`Secret` writer sits behind.
//!
//! **This is what is left of `platform_observer` after Task 19b.** That port
//! carried two unrelated methods: `observe`, which read an environment's own
//! cluster, and `ensure_kubeconfig_secret`, which writes an environment's
//! credential as a Kubernetes `Secret` into the **Argo** cluster so a run can
//! mount it (decision **D4**). Task 15 replaced observation with the product
//! plugin, so `observe` had no production caller; the `Secret` writer has
//! three — create, update, and the ticker's self-heal.
//!
//! # Why the writer survived a task that was written to delete it
//!
//! Task 19 Step 3 said "delete `infra/observer/` (whole directory)", and the
//! service's own doc comment said "Task 19 deletes both halves". **Both were
//! wrong about the second half**, and nothing in the plan's four-item warning
//! block had noticed: `qa-runs`' Argo executor deliberately materialises no
//! `Secret` (`infra::executor::argo`), so without this writer the pod
//! mounts a `Secret` nobody created. `deploy/remote/verify-k8s.sh` states the
//! consequence in its own words: "every workflow run hangs on `FailedMount`
//! while the rest of the stack looks healthy".
//!
//! Ruling **F-19** is therefore: keep the writer, delete the observation half,
//! and narrow ADR-0001's containment claim to what is true rather than write a
//! containment test that passes because a production feature was deleted.
//! Moving the writer to `qa-runs` was weighed and rejected — writing the
//! `Secret` needs the credential *material*, and that gear currently never sees
//! plaintext, which is a property §9 is built around.

use async_trait::async_trait;
use credstore_sdk::SecretValue;

/// Write an environment's credential into the Argo cluster as a `Secret`.
#[async_trait]
pub trait RunnerSecretWriter: Send + Sync {
    /// Apply the `Secret` a run mounts, named after `credstore_ref`.
    ///
    /// `Err` carries fixed operator-facing text. It is deliberately not a
    /// `DomainError`: the caller logs it and carries on, because a runner
    /// `Secret` that could not be written must not fail the create or update
    /// that triggered it.
    async fn ensure_kubeconfig_secret(
        &self,
        credstore_ref: &str,
        material: &SecretValue,
    ) -> Result<(), String>;
}

/// The writer a build without the `runner-secret` feature gets.
///
/// **An error, not a silent success.** A deployment that believes it is
/// writing runner `Secret`s and is not would fail at `FailedMount` with
/// nothing in the log to explain it; this way the reason is on every cycle.
pub struct NoopRunnerSecretWriter;

#[async_trait]
impl RunnerSecretWriter for NoopRunnerSecretWriter {
    async fn ensure_kubeconfig_secret(
        &self,
        _credstore_ref: &str,
        _material: &SecretValue,
    ) -> Result<(), String> {
        Err(
            "this build carries no runner-Secret writer: it was compiled without the \
             `runner-secret` cargo feature, so no run can mount an environment's \
             credential"
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_noop_writer_is_an_error_not_a_silent_success() {
        let error = NoopRunnerSecretWriter
            .ensure_kubeconfig_secret("credstore://ref", &SecretValue::from("x".to_owned()))
            .await
            .expect_err("a build with no writer must say so");
        assert!(
            error.contains("runner-secret"),
            "the message must name the feature an operator has to turn on: {error}"
        );
    }
}
