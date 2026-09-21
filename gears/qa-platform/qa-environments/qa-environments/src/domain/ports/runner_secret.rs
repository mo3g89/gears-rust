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
use uuid::Uuid;

/// Write an environment's credential into the Argo cluster as a `Secret`.
#[async_trait]
pub trait RunnerSecretWriter: Send + Sync {
    /// Apply the `Secret` a run mounts, named after `tenant_id` and
    /// `credstore_ref`.
    ///
    /// `tenant_id` is folded into the name so two tenants naming the same
    /// (or same-prefix) reference do not derive one `Secret` in the shared
    /// Argo namespace — see [`Self::derived_secret_name`]'s own doc for the
    /// collision this closes.
    ///
    /// `Err` carries fixed operator-facing text. It is deliberately not a
    /// `DomainError`: the caller logs it and carries on, because a runner
    /// `Secret` that could not be written must not fail the create or update
    /// that triggered it.
    async fn ensure_runner_secret(
        &self,
        tenant_id: Uuid,
        credstore_ref: &str,
        material: &SecretValue,
    ) -> Result<(), String>;

    /// The name [`Self::ensure_runner_secret`] would give the object it
    /// applies for `tenant_id` and `credstore_ref`.
    ///
    /// # Why the caller needs to ask
    ///
    /// The name is **derived**, not stored, and truncation to the longest
    /// name an API server accepts is why the caller cannot just compare
    /// references itself: the implementation that writes into Kubernetes
    /// ends the name in a fixed-width digest of the whole
    /// `(prefix, tenant_id, credstore_ref)` tuple precisely so truncating
    /// the human-readable head never makes two distinct tuples collide (see
    /// the implementation's own doc for the construction and why an earlier,
    /// truncation-only version of this rule did not have that property).
    /// While an environment had exactly one credential this question could
    /// not arise. With N it can: `CredentialSubmission::Reference` is a
    /// first-class, caller-supplied path, and `domain/` has no way to know
    /// on its own whether two of them would derive the same name without
    /// asking the port that owns the rule.
    ///
    /// So the caller asks each credential's name *before* writing anything
    /// and refuses the second claimant. It compares the strings and nothing
    /// else: **this method is the whole of what `domain` knows about the
    /// naming rule**, which is what keeps ADR-0001's condition -- that
    /// `domain/` never learns Kubernetes exists -- true while still letting
    /// the collision be caught where the decision to write is made.
    ///
    /// `tenant_id` is a parameter here (rather than folded silently into some
    /// per-writer state) for the same reason it is a parameter on
    /// [`Self::ensure_runner_secret`]: every credential this is asked about
    /// belongs to the one environment `materialise_runner_secret`'s caller
    /// (`domain::service::environments`) is looking at, and that environment
    /// has exactly one tenant, so the value never varies within one loop --
    /// but the signature says so rather than leaving it an implicit,
    /// easy-to-violate assumption.
    ///
    /// # The default folds the tenant in but transforms nothing else,
    ///   deliberately
    ///
    /// A writer that does not sanitise or truncate the reference cannot
    /// reproduce the real writer's *truncation* collision (there being none
    /// to reproduce), but it can still stand in for the tenant-scoping
    /// question this method exists to answer: two credentials of one
    /// environment share one tenant, so folding `tenant_id` in and leaving
    /// `credstore_ref` untouched is injective in exactly the sense the
    /// caller needs -- within one environment's credential loop `tenant_id`
    /// is constant, so two credentials still collide under this default if
    /// and only if their references do. Every implementation that
    /// transforms the reference itself (sanitising, truncating, hashing)
    /// must still say so by overriding; test doubles that need no such
    /// transform inherit this default, and the one that exists to reproduce
    /// a collision overrides it on purpose.
    fn derived_secret_name(&self, tenant_id: Uuid, credstore_ref: &str) -> String {
        format!("{tenant_id}-{credstore_ref}")
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
        _tenant_id: Uuid,
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

    /// The default derivation folds the tenant in but transforms nothing
    /// else: two credentials of one environment (one tenant, held constant
    /// here) still collide only when their references do.
    #[test]
    fn the_default_derived_name_prepends_the_tenant_and_leaves_the_reference_alone() {
        let tenant: Uuid = uuid::uuid!("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert_eq!(
            NoopRunnerSecretWriter.derived_secret_name(tenant, "environment/9f2c/kubeconfig"),
            format!("{tenant}-environment/9f2c/kubeconfig")
        );
    }

    #[tokio::test]
    async fn the_noop_writer_is_an_error_not_a_silent_success() {
        let error = NoopRunnerSecretWriter
            .ensure_runner_secret(
                Uuid::new_v4(),
                "credstore://ref",
                &SecretValue::from("x".to_owned()),
            )
            .await
            .expect_err("a build with no writer must say so");
        assert!(
            error.contains("runner-secret"),
            "the message must name the feature an operator has to turn on: {error}"
        );
    }
}
