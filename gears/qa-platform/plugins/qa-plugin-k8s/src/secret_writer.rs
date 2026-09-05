//! Lifted from `qa-environments/src/infra/observer/secret_writer.rs`, which
//! stays in place and stays active until Task 19 removes it. This is a copy,
//! not a move: Phase C must not change `qa-environments`' behaviour, so both
//! paths exist side by side until the one-way door in Phase E.
//!
//! What changed in the copy: failures cross the boundary as
//! [`PluginFailure`] rather than `String`, so the values the original
//! interpolated into its messages — the configured kubeconfig path, the
//! `Secret`'s name and namespace — move to `tracing` fields, which is where a
//! deployment fact belongs and is a strictly narrower audience than the
//! published error the original put them in. `PluginFailure::detail` is
//! `&'static str` and cannot hold them; that is the point of the type.
//!
//! Decision D4's writer half: materialise the `Secret` a runner pod mounts.
//!
//! **This targets the Argo cluster, not the environment's.**
//! [`crate::kube_client::KubeClient::from_kubeconfig`] builds a client from an
//! *environment's* kubeconfig — the cluster under test. This module builds a
//! *different* client, from `argo_kubeconfig_path`, aimed at the cluster where
//! `qa-runs`' test-runner pods actually execute. Conflating the two would
//! write the Secret into the wrong cluster and the symptom would be identical
//! to the bug this was written for: a pod stuck `Pending` on `FailedMount`,
//! just for a different reason.
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
use qa_product_sdk::observation::{FailureClass, PluginFailure};

use crate::errors::{CLIENT_BUILD_FAILURE, INFER_FAILURE, classify, classify_kubeconfig};

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
///
/// **`qa-environments`, not this crate's name, and deliberately so.** Field
/// managership is per-object state the API server remembers: a `Secret`
/// already converged by `qa-environments`' own writer would refuse a patch
/// from a differently-named manager with the very 409 the branch below exists
/// to explain. Renaming this is a migration, not a tidy-up.
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

/// Fixed explanation for the known 409 interaction, and the one thing an
/// operator can do about it.
///
/// The original named the `Secret` and its namespace inline; a
/// [`PluginFailure`]'s `detail` cannot. Neither fact is lost: the API server's
/// own refusal ("secrets \"…\" already exists") rides along in
/// [`PluginFailure::remote_message`], and both are logged as `tracing` fields
/// at the call site.
///
/// The em dash is spelled `\u{2014}` because `clippy::non_ascii_literal` is
/// deny at workspace level and — unlike the original, where this text lived
/// inside a `format!` and was therefore not linted — this is a plain literal.
/// The runtime bytes are the original's, unchanged; only the source spelling
/// differs.
const SECRET_APPLY_CONFLICT: &str = "the Secret already exists and is not owned by this writer's field manager \
     (\"qa-environments\"), so server-side apply was refused with a 409 Conflict; this usually \
     means an operator created it by hand (kubectl create, or the provisioning shell script) \
     rather than through this self-heal \u{2014} delete it so qa-environments can take ownership on \
     the next cycle (`kubectl -n <the configured Argo namespace> delete secret <the name the \
     API server names above>`), or leave it in place if that hand-made Secret is deliberate \
     and must never be managed here";

/// Where a runner pod's kubeconfig `Secret` goes, and how to reach the
/// cluster it goes into.
///
/// The four fields are the original `KubeObserver`'s four, unchanged:
/// `kubeconfig_path` names the kubeconfig that reaches the **Argo** cluster
/// (empty or absent means `Config::infer()`, correct from inside that
/// cluster), `namespace` and `key` name where and under what key the material
/// goes, and `name_prefix` derives the `Secret`'s name so it agrees with what
/// `qa-runs`' executor mounts and what
/// `deploy/argo/provision-platform-kubeconfig-secret.sh` writes today.
pub struct SecretWriter {
    kubeconfig_path: Option<String>,
    namespace: String,
    name_prefix: String,
    key: String,
}

impl SecretWriter {
    #[must_use]
    pub const fn new(
        kubeconfig_path: Option<String>,
        namespace: String,
        name_prefix: String,
        key: String,
    ) -> Self {
        Self {
            kubeconfig_path,
            namespace,
            name_prefix,
            key,
        }
    }

    /// Decision D4: ensure the `Secret` a runner pod mounts exists and carries
    /// the current kubeconfig material.
    ///
    /// # Errors
    ///
    /// Every failure names *what* failed (bad path, unreachable API server,
    /// RBAC, non-existent namespace) as a classification and never the
    /// kubeconfig bytes themselves. See this module's `ensure_kubeconfig_secret`.
    pub async fn ensure_secret(
        &self,
        credstore_ref: &str,
        kubeconfig: &SecretValue,
    ) -> Result<(), PluginFailure> {
        ensure_kubeconfig_secret(
            self.kubeconfig_path.as_deref(),
            &self.namespace,
            &self.name_prefix,
            &self.key,
            credstore_ref,
            kubeconfig,
        )
        .await
    }
}

/// Build a client aimed at the Argo cluster.
///
/// `argo_kubeconfig_path` empty or absent means `Config::infer()` — in-cluster
/// service-account credentials, else `KUBECONFIG`, else `~/.kube/config` —
/// which is correct once these gears themselves run inside the cluster whose
/// Argo installation they are writing into.
///
/// # Every part of the failure is a literal
///
/// The *errors* are classified through [`crate::errors`] and never formatted.
/// That is not defensive tidiness: a `KubeconfigError` from reading a file
/// quotes the offending scalar of the document it was reading, so a malformed
/// Argo kubeconfig — a file holding cluster-admin credentials — would
/// otherwise print itself into the gear log on every self-heal cycle. Same
/// defect as the one measured on the environment side (see
/// [`crate::errors`]), one file over, with the log rather than the browser as
/// the destination.
///
/// The *path* is deployment configuration rather than credential material and
/// naming it is the whole diagnostic value, so it is logged as a `tracing`
/// field alongside the classification. The original interpolated it into the
/// returned message; the returned value is now a [`PluginFailure`], whose
/// `detail` is `&'static str` by construction.
async fn argo_client(argo_kubeconfig_path: Option<&str>) -> Result<Client, PluginFailure> {
    let trimmed = argo_kubeconfig_path
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let config = match trimmed {
        Some(path) => {
            let kubeconfig = Kubeconfig::read_from(path).map_err(|error| {
                let failure = classify_kubeconfig(&error);
                tracing::warn!(
                    argo_kubeconfig_path = path,
                    reason = %failure,
                    "failed to read the Argo kubeconfig"
                );
                failure
            })?;
            Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
                .await
                .map_err(|error| {
                    let failure = classify_kubeconfig(&error);
                    tracing::warn!(
                        argo_kubeconfig_path = path,
                        reason = %failure,
                        "failed to build a config from the Argo kubeconfig"
                    );
                    failure
                })?
        }
        None => Config::infer()
            .await
            .map_err(|_| PluginFailure::classified(FailureClass::Internal, INFER_FAILURE))?,
    };
    Client::try_from(config)
        .map_err(|_| PluginFailure::classified(FailureClass::Malformed, CLIENT_BUILD_FAILURE))
}

/// Decision D4: ensure the `Secret` a runner pod mounts exists in the Argo
/// cluster and carries the current kubeconfig material, converging via
/// server-side apply so create, update and every self-heal cycle agree
/// rather than conflict.
///
/// # Errors
/// Every error names what failed (bad path, unreachable API server, RBAC,
/// non-existent namespace) and never the kubeconfig bytes themselves — the
/// only runtime value any of these paths carries is the message the API
/// server itself sent back. Every library error is classified rather than
/// formatted; see [`crate::errors`].
pub(crate) async fn ensure_kubeconfig_secret(
    argo_kubeconfig_path: Option<&str>,
    argo_namespace: &str,
    secret_prefix: &str,
    secret_key: &str,
    credstore_ref: &str,
    kubeconfig: &SecretValue,
) -> Result<(), PluginFailure> {
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
    .map_err(|error| {
        let failure = describe_apply_failure(&error);
        // The `Secret`'s identity is deployment fact, not credential
        // material, and an operator needs it to act on either branch below.
        // It goes here rather than into the failure because
        // `PluginFailure::detail` is `&'static str`.
        tracing::warn!(
            secret = name,
            namespace = argo_namespace,
            reason = %failure,
            "failed to apply the runner kubeconfig Secret"
        );
        failure
    })?;

    Ok(())
}

/// Turn a failed `patch` call into an operator-actionable classification.
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
/// nothing to converge with. Left as the generic transport classification (as
/// every other failure here is), this reads on every self-heal cycle as the
/// exact same anonymous error, forever. It IS the same error every time — the
/// fix is on the operator's side, not this writer's (see
/// [`ensure_kubeconfig_secret`]'s own doc for why `force: true` is not that
/// fix) — so [`SECRET_APPLY_CONFLICT`] says as much: what happened, and the
/// one command that resolves it.
fn describe_apply_failure(error: &kube::Error) -> PluginFailure {
    if let kube::Error::Api(status) = error
        && status.is_conflict()
    {
        return PluginFailure::classified(FailureClass::Internal, SECRET_APPLY_CONFLICT)
            .with_remote_message(status.message.clone());
    }
    classify(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::RawBuffer;

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

    /// The `Secret`'s name still reaches the operator, but through the one
    /// slot sanctioned to carry runtime text: the API server's own refusal.
    /// The namespace no longer appears — it is a `tracing` field on the call
    /// site instead (see [`ensure_kubeconfig_secret`]), because
    /// `PluginFailure::detail` is `&'static str` and cannot hold it.
    #[test]
    fn a_409_conflict_names_the_secret_and_tells_the_operator_what_to_do() {
        let failure = describe_apply_failure(&conflict_error());
        let message = failure.to_string();
        assert!(
            message.contains("qa-platform-x"),
            "must name the secret so an operator knows which one to delete: {message}"
        );
        assert!(message.contains("409"), "must say it was a 409: {message}");
        assert!(
            message.to_lowercase().contains("delete"),
            "must tell the operator what to do about it: {message}"
        );
        assert_eq!(
            failure.remote_message.as_deref(),
            Some("secrets \"qa-platform-x\" already exists"),
            "the name comes from the server's own words, not from a format! of ours"
        );
        // "409" above is part of the fixed text, so it would read the same
        // against an implementation that never looked at the status. The class
        // is the part only a real 409 can produce.
        assert_eq!(failure.class, FailureClass::Internal);
    }

    /// A conflict is the one failure this function treats specially. Every
    /// other status — this test uses `404` — keeps the plain, generic
    /// classification: no false "an operator created this by hand" claim for
    /// a namespace that simply does not exist.
    #[test]
    fn a_non_conflict_failure_keeps_the_generic_message() {
        let error = kube::Error::Api(
            kube::core::Status::failure("namespaces \"argo\" not found", "NotFound")
                .with_code(404)
                .boxed(),
        );
        let failure = describe_apply_failure(&error);
        let message = failure.to_string();
        assert!(
            !message.contains("field manager"),
            "a 404 must not get the conflict-specific operator guidance: {message}"
        );
        assert_eq!(failure.class, FailureClass::NotFound);
    }

    /// Run `body` with every `tracing` byte captured on this thread.
    async fn capturing_logs<F, T>(body: F) -> (T, String)
    where
        F: Future<Output = T>,
    {
        let buffer = RawBuffer::new();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let value = body.await;
        drop(guard);
        let captured = buffer.captured();
        (value, captured)
    }

    /// A malformed path never reaches the network, so this is safe to run
    /// with no cluster available — the point being made is that the failure
    /// is classified rather than a panic, and that the path an operator has
    /// to fix still reaches them.
    ///
    /// The path moved from the returned message to a `tracing` field when the
    /// return type became [`PluginFailure`]; this test follows it there
    /// rather than dropping the assertion.
    #[tokio::test]
    async fn a_missing_argo_kubeconfig_file_is_a_named_failure_not_a_panic() {
        const PATH: &str = "/nonexistent/path/to/argo-kubeconfig.yaml";
        let (result, captured) = capturing_logs(ensure_kubeconfig_secret(
            Some(PATH),
            "argo",
            "qa-platform-",
            "value",
            "environment/does-not-matter/kubeconfig",
            &SecretValue::from("irrelevant".to_owned()),
        ))
        .await;

        let failure = result.expect_err("a nonexistent kubeconfig path must fail, not succeed");
        assert_eq!(failure.detail, Some("it could not be read"));
        assert_eq!(failure.class, FailureClass::Internal);
        assert!(
            captured.contains(PATH),
            "the path that could not be read must still reach an operator, got:\n{captured}"
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
        let (result, captured) = capturing_logs(ensure_kubeconfig_secret(
            Some("/nonexistent/path/to/argo-kubeconfig.yaml"),
            "argo",
            "qa-platform-",
            "value",
            "environment/does-not-matter/kubeconfig",
            &SecretValue::from(material),
        ))
        .await;

        let failure = result.expect_err("a nonexistent kubeconfig path must fail, not succeed");
        let error = failure.to_string();
        assert!(
            !error.contains(CANARY),
            "the failure must never carry the kubeconfig material, got: {error}"
        );
        assert!(
            !captured.contains(CANARY),
            "the material must never reach a log line, got:\n{captured}"
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
        let dir = std::env::temp_dir().join(format!("qa-plugin-k8s-writer-{}", std::process::id()));
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

        let (result, captured) = capturing_logs(ensure_kubeconfig_secret(
            Some(&path_text),
            "argo",
            "qa-platform-",
            "value",
            "environment/does-not-matter/kubeconfig",
            &SecretValue::from("irrelevant".to_owned()),
        ))
        .await;

        // Best-effort cleanup; a leftover temp file is not a test failure,
        // and the assertions below are what this test is about.
        drop(std::fs::remove_file(&path));
        let failure =
            result.expect_err("a document that is not a kubeconfig must fail, not succeed");
        let error = failure.to_string();
        assert!(
            !error.contains(CANARY),
            "the Argo kubeconfig's own contents must never reach the failure, got: {error}"
        );
        assert!(
            !error.contains("BEGIN"),
            "PEM material must never reach the failure, got: {error}"
        );
        assert!(
            error.contains("does not have a kubeconfig's shape"),
            "the failure must still say what is wrong with the file, got: {error}"
        );
        assert!(
            captured.contains(&path_text),
            "the failure must still name the path an operator has to fix, got:\n{captured}"
        );
        assert!(
            !captured.contains(CANARY) && !captured.contains("BEGIN"),
            "the log line naming the path must not carry the file's contents, got:\n{captured}"
        );
    }
}
