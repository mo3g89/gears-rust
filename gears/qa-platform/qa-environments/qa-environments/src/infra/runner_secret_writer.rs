//! Decision D4's writer half: materialise the `Secret` a runner pod mounts.
//!
//! **This targets the Argo cluster, not the environment's.** Until Task 19b
//! this module sat beside `KubeObserver`, which built a client from an
//! *environment's* kubeconfig — the cluster under test — and the distinction
//! between the two clients was the thing most worth stating. The observer is
//! gone (observation runs through the product plugin), so this is now the only
//! Kubernetes client in the crate, and it is aimed at the cluster where
//! `qa-runs`' test-runner pods execute. Pointing it at an environment's own
//! cluster would write the `Secret` into the wrong place, and the symptom
//! would be a pod stuck `Pending` on `FailedMount`.
//!
//! The empty-path-means-`Config::infer()` fallback is the same shape
//! `qa-runs`' `ArgoRunExecutor::connect` uses
//! (`qa-runs/src/infra/executor/argo/mod.rs:219-230`), copied rather than
//! reinvented so this code needs no change once these gears run in-cluster.

use std::collections::BTreeMap;

use credstore_sdk::SecretValue;
use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, Patch, PatchParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

use super::runner_secret_errors::{
    CLIENT_BUILD_FAILURE, INFER_FAILURE, describe_kube_error, describe_kubeconfig_error,
};

/// The `RunnerSecretWriter` a build with the `runner-secret` feature gets.
///
/// **All four fields are for the Argo cluster**, not for any environment's own
/// — `KubeObserver`, which used to carry them alongside four more for
/// observation, went with the observation half at Task 19b. `argo_namespace`
/// and `secret_key` name where and under what key the material goes, and
/// `secret_prefix` derives the `Secret`'s name so it agrees with what
/// `qa-runs`' executor mounts and what
/// `deploy/argo/provision-platform-kubeconfig-secret.sh` writes.
pub struct KubeRunnerSecretWriter {
    argo_kubeconfig_path: Option<String>,
    argo_namespace: String,
    secret_prefix: String,
    secret_key: String,
}

impl KubeRunnerSecretWriter {
    #[must_use]
    pub fn new(
        argo_kubeconfig_path: Option<String>,
        argo_namespace: String,
        secret_prefix: String,
        secret_key: String,
    ) -> Self {
        Self {
            argo_kubeconfig_path,
            argo_namespace,
            secret_prefix,
            secret_key,
        }
    }
}

#[async_trait::async_trait]
impl crate::domain::ports::RunnerSecretWriter for KubeRunnerSecretWriter {
    async fn ensure_kubeconfig_secret(
        &self,
        credstore_ref: &str,
        material: &credstore_sdk::SecretValue,
    ) -> Result<(), String> {
        ensure_kubeconfig_secret(
            self.argo_kubeconfig_path.as_deref(),
            &self.argo_namespace,
            &self.secret_prefix,
            &self.secret_key,
            credstore_ref,
            material,
        )
        .await
    }
}

/// Longest name the API server accepts for a `Secret` object.
///
/// **Duplicated, not shared, on purpose.** The exact same rule is
/// implemented twice more: `qa-runs`' `naming::secret_name`
/// (`qa-runs/src/infra/executor/argo/naming.rs`), which is what the runner
/// pod's volume actually resolves against, and
/// `deploy/argo/provision-platform-kubeconfig-secret.sh`'s `derive_name`, the
/// operator's manual fallback. A shared crate for one string derivation used
/// by two gears and a shell script is more structure than the rule deserves;
/// the parity test below (`the_writer_the_executor_and_the_script_agree_on_every_name`)
/// pins the same table `naming.rs`'s own parity oracle does, so drift between
/// any two of the three fails loudly in CI rather than silently in a
/// `FailedMount` event.
const MAX_SECRET_NAME_LEN: usize = 63;

/// Field manager name for server-side apply. Distinguishing this writer's
/// changes from any other actor's is exactly what a field manager is for,
/// and this is the identity `PatchParams::apply` requires.
const FIELD_MANAGER: &str = "qa-environments";

/// Lower-case, keep `[a-z0-9-]`, collapse everything else to `-`, trim the
/// dashes off both ends.
///
/// Verbatim copy of `qa-runs`' `naming::sanitize` and of
/// `provision-platform-kubeconfig-secret.sh`'s `derive_name` body: `tr`
/// lower-case, `sed 's/[^a-z0-9-]/-/g'`, then trim leading/trailing `-`.
fn sanitize(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

/// Truncate to `limit` bytes without leaving a trailing `-`, matching
/// `qa-runs`' `naming::truncate`.
fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    value
        .get(..limit)
        .unwrap_or(value)
        .trim_end_matches('-')
        .to_owned()
}

/// A `Secret` name derived from a credstore reference — `secret_name` in
/// `qa-runs`' `naming.rs`, reimplemented here (see [`MAX_SECRET_NAME_LEN`]'s
/// doc comment for why it is not shared).
fn secret_name(prefix: &str, reference: &str) -> String {
    truncate(
        &sanitize(&format!("{prefix}{reference}")),
        MAX_SECRET_NAME_LEN,
    )
}

/// Build a client aimed at the Argo cluster.
///
/// `argo_kubeconfig_path` empty or absent means `Config::infer()` — in-cluster
/// service-account credentials, else `KUBECONFIG`, else `~/.kube/config` —
/// which is correct once these gears themselves run inside the cluster whose
/// Argo installation they are writing into.
///
/// # Every message here is assembled from literals
///
/// The *path* is interpolated (it is deployment configuration, and naming it
/// is the whole diagnostic value); the *errors* are classified through
/// [`super::errors`] and never formatted. That is not defensive tidiness: a
/// `KubeconfigError` from reading a file quotes the offending scalar of the
/// document it was reading, so a malformed Argo kubeconfig — a file holding
/// cluster-admin credentials — would otherwise print itself into the gear log
/// on every self-heal cycle. Same defect as the one measured on the environment
/// side (see [`super::errors`]), one file over, with the log rather than the
/// browser as the destination.
async fn argo_client(argo_kubeconfig_path: Option<&str>) -> Result<Client, String> {
    let trimmed = argo_kubeconfig_path
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let config = match trimmed {
        Some(path) => {
            let kubeconfig = Kubeconfig::read_from(path).map_err(|error| {
                format!(
                    "failed to read argo kubeconfig at {path}: {}",
                    describe_kubeconfig_error(&error)
                )
            })?;
            Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
                .await
                .map_err(|error| {
                    format!(
                        "failed to build config from argo kubeconfig at {path}: {}",
                        describe_kubeconfig_error(&error)
                    )
                })?
        }
        None => Config::infer().await.map_err(|_| {
            format!("failed to infer a Kubernetes config for the Argo cluster: {INFER_FAILURE}")
        })?,
    };
    Client::try_from(config)
        .map_err(|_| format!("failed to create Argo Kubernetes client: {CLIENT_BUILD_FAILURE}"))
}

/// Decision D4: ensure the `Secret` a runner pod mounts exists in the Argo
/// cluster and carries the current kubeconfig material, converging via
/// server-side apply so create, update and every self-heal cycle agree
/// rather than conflict.
///
/// # Errors
/// Every error names what failed (bad path, unreachable API server, RBAC,
/// non-existent namespace) and never the kubeconfig bytes themselves — the
/// only value any of these paths interpolates is the configured *path*, the
/// `Secret`'s own name/namespace, and (for a rejected request) the message
/// the API server itself sent back. Every library error is classified rather
/// than formatted; see [`super::errors`].
pub(super) async fn ensure_kubeconfig_secret(
    argo_kubeconfig_path: Option<&str>,
    argo_namespace: &str,
    secret_prefix: &str,
    secret_key: &str,
    credstore_ref: &str,
    kubeconfig: &SecretValue,
) -> Result<(), String> {
    let client = argo_client(argo_kubeconfig_path).await?;
    let name = secret_name(secret_prefix, credstore_ref);

    let mut data = BTreeMap::new();
    data.insert(
        secret_key.to_owned(),
        ByteString(kubeconfig.as_bytes().to_vec()),
    );

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(argo_namespace.to_owned()),
            ..Default::default()
        },
        data: Some(data),
        type_: Some("Opaque".to_owned()),
        ..Default::default()
    };

    let api: Api<Secret> = Api::namespaced(client, argo_namespace);
    api.patch(
        &name,
        &PatchParams::apply(FIELD_MANAGER),
        &Patch::Apply(&secret),
    )
    .await
    .map_err(|error| describe_apply_failure(&name, argo_namespace, &error))?;

    Ok(())
}

/// Turn a failed `patch` call into an operator-actionable message.
///
/// # The known `409` interaction, named rather than discovered in production
///
/// This writer applies **without** `force` (deliberately — see this module's
/// header and the D4 design notes: forcing would let it silently stomp a
/// `Secret` an operator placed by hand). If a `Secret` of this name already
/// exists because it was created with `kubectl create` — or by
/// `deploy/argo/provision-platform-kubeconfig-secret.sh`, which also does not
/// server-side-apply — rather than through this writer, the API server
/// refuses the patch with a `409 Conflict`: this writer's field manager
/// (`qa-environments`) owns none of that object's fields, so apply has
/// nothing to converge with. Left as a bare `{error}` (as every other failure
/// here is), this reads on every self-heal cycle as the exact same anonymous
/// error, forever. It IS the same error every time — the fix is on the
/// operator's side, not this writer's (see [`ensure_kubeconfig_secret`]'s own
/// doc for why `force: true` is not that fix) — so the message says as much:
/// what happened, and the one command that resolves it.
fn describe_apply_failure(name: &str, namespace: &str, error: &kube::Error) -> String {
    if let kube::Error::Api(status) = error
        && status.is_conflict()
    {
        return format!(
            "secret/{name} in namespace {namespace} already exists and is not owned by this \
             writer's field manager (\"{FIELD_MANAGER}\"), so server-side apply was refused \
             with a 409 Conflict ({status}); this usually means an operator created it by hand \
             (kubectl create, or the provisioning shell script) rather than through this \
             self-heal — delete it so qa-environments can take ownership on the next cycle \
             (`kubectl -n {namespace} delete secret {name}`), or leave it in place if that \
             hand-made Secret is deliberate and must never be managed here"
        );
    }
    format!(
        "failed to apply secret/{name} in namespace {namespace}: {}",
        describe_kube_error(error)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name must equal what `qa-runs`' executor mounts and what the
    /// operator script writes. Each right-hand side is duplicated, not
    /// derived, from `qa-runs/src/infra/executor/argo/naming.rs`'s own
    /// parity-oracle test (`secret_names_agree_with_the_provisioning_scripts_shell_derivation`),
    /// which in turn was checked against
    /// `deploy/argo/provision-platform-kubeconfig-secret.sh`'s `derive_name`.
    /// Three implementations of one rule, pinned in three places, so drift
    /// between any two fails a test rather than a pod mount.
    #[test]
    fn the_writer_the_executor_and_the_script_agree_on_every_name() {
        for (reference, expected) in [
            ("argo-proof-kubeconfig", "qa-platform-argo-proof-kubeconfig"),
            (
                "environment/9f2c.../kubeconfig",
                "qa-platform-environment-9f2c----kubeconfig",
            ),
            ("UPPER_Case", "qa-platform-upper-case"),
        ] {
            assert_eq!(
                secret_name("qa-platform-", reference),
                expected,
                "reference {reference:?} must derive the same Secret name here, in \
                 qa-runs' naming.rs, and in the shell script"
            );
        }
    }

    #[test]
    fn a_reference_with_no_prefix_still_sanitises() {
        assert_eq!(secret_name("", "already-clean"), "already-clean");
    }

    /// Constructing this needs no live cluster: `kube::Error::Api` wraps a
    /// plain, `Default`-derived value, so the 409-detection branch of
    /// `describe_apply_failure` is exercised the same way any pure function
    /// would be.
    fn conflict_error() -> kube::Error {
        kube::Error::Api(
            kube::core::Status::failure("secrets \"qa-platform-x\" already exists", "Conflict")
                .with_code(409)
                .boxed(),
        )
    }

    #[test]
    fn a_409_conflict_names_the_secret_and_tells_the_operator_what_to_do() {
        let message = describe_apply_failure("qa-platform-x", "argo", &conflict_error());
        assert!(
            message.contains("qa-platform-x"),
            "must name the secret so an operator knows which one to delete: {message}"
        );
        assert!(
            message.contains("argo"),
            "must name the namespace: {message}"
        );
        assert!(message.contains("409"), "must say it was a 409: {message}");
        assert!(
            message.to_lowercase().contains("delete"),
            "must tell the operator what to do about it: {message}"
        );
    }

    /// A conflict is the one failure this function treats specially. Every
    /// other status — this test uses `404` — keeps the plain, generic
    /// message: no false "an operator created this by hand" claim for a
    /// namespace that simply does not exist.
    #[test]
    fn a_non_conflict_failure_keeps_the_generic_message() {
        let error = kube::Error::Api(
            kube::core::Status::failure("namespaces \"argo\" not found", "NotFound")
                .with_code(404)
                .boxed(),
        );
        let message = describe_apply_failure("qa-platform-x", "argo", &error);
        assert!(
            !message.contains("field manager"),
            "a 404 must not get the conflict-specific operator guidance: {message}"
        );
    }

    /// A malformed path never reaches the network, so this is safe to run
    /// with no cluster available — the point being made is that the error
    /// names the path, not the credential.
    #[tokio::test]
    async fn a_missing_argo_kubeconfig_file_is_a_named_failure_not_a_panic() {
        let result = ensure_kubeconfig_secret(
            Some("/nonexistent/path/to/argo-kubeconfig.yaml"),
            "argo",
            "qa-platform-",
            "value",
            "environment/does-not-matter/kubeconfig",
            &SecretValue::from("irrelevant".to_owned()),
        )
        .await;

        let error = result.expect_err("a nonexistent kubeconfig path must fail, not succeed");
        assert!(
            error.contains("/nonexistent/path/to/argo-kubeconfig.yaml"),
            "the error should name the path that could not be read, got: {error}"
        );
    }

    /// The canary sits directly in the kubeconfig bytes handed to
    /// `ensure_kubeconfig_secret`, unencoded, so an accidental
    /// `format!("...{kubeconfig:?}...")` or similar in an error path would
    /// actually be caught. It never reaches the Argo API — the path failure
    /// above happens first — so this needs no cluster.
    const CANARY: &str = "CANARY-6e0f2a-secret-writer-leak-probe-do-not-log-me";

    #[tokio::test]
    async fn the_kubeconfig_material_never_reaches_an_error_message() {
        let material = format!(
            "apiVersion: v1\nkind: Config\nusers:\n- name: x\n  user:\n    client-key-data: \"{CANARY}\"\n"
        );
        let result = ensure_kubeconfig_secret(
            Some("/nonexistent/path/to/argo-kubeconfig.yaml"),
            "argo",
            "qa-platform-",
            "value",
            "environment/does-not-matter/kubeconfig",
            &SecretValue::from(material),
        )
        .await;

        let error = result.expect_err("a nonexistent kubeconfig path must fail, not succeed");
        assert!(
            !error.contains(CANARY),
            "the error string must never carry the kubeconfig material, got: {error}"
        );
    }

    /// A canary in the **Argo** kubeconfig this writer reads, not in the
    /// material it writes.
    ///
    /// Both canary tests above stop at the *first* line of `argo_client` —
    /// the file does not exist, so `Kubeconfig::read_from`'s parse never
    /// runs, and neither does `from_custom_kubeconfig`. The final review
    /// named that as the same structural gap that hid C1 one module over: a
    /// canary that never reaches the branch it guards. This one writes a
    /// real, readable file whose contents are a canary-bearing scalar rather
    /// than a kubeconfig, so `Kubeconfig::read_from`'s
    /// `KubeconfigError::InvalidStructure` — the exact variant whose
    /// `Display` quotes its input — is the branch under test.
    #[tokio::test]
    async fn a_readable_but_malformed_argo_kubeconfig_is_classified_not_echoed() {
        let dir = std::env::temp_dir().join(format!("qa-env-secret-writer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("argo-kubeconfig.yaml");
        // A bare scalar: valid YAML, not a kubeconfig. serde reports
        // `invalid type: string "<the whole document>"`.
        std::fs::write(
            &path,
            format!("-----BEGIN EC PRIVATE KEY----- {CANARY} -----END EC PRIVATE KEY-----\n"),
        )
        .expect("write the fixture");
        let path_text = path.to_string_lossy().into_owned();

        let result = ensure_kubeconfig_secret(
            Some(&path_text),
            "argo",
            "qa-platform-",
            "value",
            "environment/does-not-matter/kubeconfig",
            &SecretValue::from("irrelevant".to_owned()),
        )
        .await;

        // Best-effort cleanup; a leftover temp file is not a test failure,
        // and the assertions below are what this test is about.
        drop(std::fs::remove_file(&path));
        let error = result.expect_err("a document that is not a kubeconfig must fail, not succeed");
        assert!(
            !error.contains(CANARY),
            "the Argo kubeconfig's own contents must never reach the error string, got: {error}"
        );
        assert!(
            !error.contains("BEGIN"),
            "PEM material must never reach the error string, got: {error}"
        );
        assert!(
            error.contains(&path_text),
            "the error must still name the path an operator has to fix, got: {error}"
        );
        assert!(
            error.contains("does not have a kubeconfig's shape"),
            "the error must still say what is wrong with the file, got: {error}"
        );
    }
}
