//! The port D4's runner-`Secret` writer sits behind.
//!
//! **This is what is left of `platform_observer` after Task 19b.** That port
//! carried two unrelated methods: `observe`, which read an environment's own
//! cluster, and `ensure_runner_secret`, which writes one of an environment's
//! credentials as a Kubernetes `Secret` into the **Argo** cluster so a run can
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
    async fn ensure_runner_secret(
        &self,
        credstore_ref: &str,
        material: &SecretValue,
    ) -> Result<(), String>;

    /// The name [`Self::ensure_runner_secret`] would give the object it
    /// applies for `credstore_ref`.
    ///
    /// # Why the caller needs to ask
    ///
    /// The name is **derived**, not stored, and the derivation is lossy: the
    /// implementation that writes into Kubernetes sanitises the reference and
    /// truncates it to the longest name an API server accepts, which leaves
    /// only part of it. While an environment had exactly one credential that
    /// could not matter. With N it can: `CredentialSubmission::Reference` is
    /// a first-class, caller-supplied path, and two path-style references
    /// sharing a long enough prefix derive **one** name. The second apply
    /// then overwrites the first's material and `qa-runs` mounts that one
    /// object at both paths, with no error on either gear -- a run reading
    /// the wrong credential at the right path, silently.
    ///
    /// So the caller asks each credential's name *before* writing anything
    /// and refuses the second claimant. It compares the strings and nothing
    /// else: **this method is the whole of what `domain` knows about the
    /// naming rule**, which is what keeps ADR-0001's condition -- that
    /// `domain/` never learns Kubernetes exists -- true while still letting
    /// the collision be caught where the decision to write is made.
    ///
    /// # The default is the identity, deliberately
    ///
    /// A writer that does not transform the reference cannot collide, so
    /// "distinct references, distinct names" is the honest default and every
    /// implementation that *does* transform must say so by overriding. Test
    /// doubles inherit it; the one that exists to reproduce a collision
    /// overrides it on purpose.
    fn derived_secret_name(&self, credstore_ref: &str) -> String {
        credstore_ref.to_owned()
    }
}

/// The writer a build without the `runner-secret` feature gets.
///
/// **An error, not a silent success.** A deployment that believes it is
/// writing runner `Secret`s and is not would fail at `FailedMount` with
/// nothing in the log to explain it; this way the reason is on every cycle.
#[cfg_attr(
    feature = "runner-secret",
    allow(
        dead_code,
        reason = "`gear.rs` constructs this only in a build without `runner-secret`; with \
                  the feature on, the tests below and `test_support` are its only callers. \
                  See `domain::ports`' own note on the re-export (review finding #38)"
    )
)]
pub struct NoopRunnerSecretWriter;

#[async_trait]
impl RunnerSecretWriter for NoopRunnerSecretWriter {
    async fn ensure_runner_secret(
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

    /// The default derivation is the identity: a writer that does not
    /// transform the reference cannot make two of them collide, and one that
    /// does has to override and say so.
    #[test]
    fn the_default_derived_name_is_the_reference_itself() {
        assert_eq!(
            NoopRunnerSecretWriter.derived_secret_name("environment/9f2c/kubeconfig"),
            "environment/9f2c/kubeconfig"
        );
    }

    #[tokio::test]
    async fn the_noop_writer_is_an_error_not_a_silent_success() {
        let error = NoopRunnerSecretWriter
            .ensure_runner_secret("credstore://ref", &SecretValue::from("x".to_owned()))
            .await
            .expect_err("a build with no writer must say so");
        assert!(
            error.contains("runner-secret"),
            "the message must name the feature an operator has to turn on: {error}"
        );
    }
}
