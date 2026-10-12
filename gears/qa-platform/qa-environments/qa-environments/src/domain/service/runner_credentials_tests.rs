//! `EnvironmentsService::runner_credentials` (finding #94): what a runner pod
//! is handed, and every way it is refused.
//!
//! Against a REAL in-memory `SQLite` database under [`TenantScopedAuthZ`], for
//! the reason `environments_credentials_tests` gives: the tenant boundary
//! this route leans on ends in SQL, and a mock repository would assert
//! against a hand-written copy of it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use qa_environments_sdk::{CredentialMaterial, CredentialSubmission, Environment, NewEnvironment};
use qa_product_sdk::descriptor::FieldKind;
use qa_product_sdk::observation::{
    HealthOutcome, HealthState, ObservationOutcome, ObservedAttrs, PluginObservation,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::{
    PodIdentityRefusal, PodIdentityVerifier, ProductPluginPort, RunnerPodRefusal, RunnerPodResolver,
};
use crate::gear::ConcreteAppServices;
use crate::test_support::{
    CapturedLogs, CountingRunnerPods, FixedPluginPort, HangingPodIdentity, RecordingCredStore,
    ScriptedPlugin, ScriptedPodIdentity, ScriptedRunnerPods, a_runner_pod,
    build_services_for_runner_credentials, ctx, field, vhi_shaped_credential_schema,
};

const KUBECONFIG_CANARY: &str = "KUBECONFIG-CANARY";
const SSH_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nSSH-CANARY\n";
const VINFRA_PASSWORD: &str = "VINFRA-CANARY";
const UNUSED_SECRET: &str = "UNUSED-CANARY";

/// The paths the VHP and VHI plugins mount at, spelled as
/// `qa-vhp-product-plugin`'s and `qa-vhi-product-plugin`'s `run.rs` spell them.
const KUBECONFIG_PATH: &str = "/.kube/kubeconfig";
const SSH_KEY_PATH: &str = "/etc/qa/vhi-ssh/id";
const VINFRA_PASSWORD_PATH: &str = "/etc/qa/vhi-vinfra/password";

fn product() -> Uuid {
    Uuid::from_u128(0x9001)
}

fn detected() -> PluginObservation {
    PluginObservation {
        environment: ObservationOutcome::Detected(ObservedAttrs::default()),
        health: HealthOutcome::Checked {
            state: HealthState::Ok,
            detail: None,
        },
    }
}

/// A VHP-shaped plugin that mounts `kubeconfig` where the real one does.
fn vhp_mounting() -> Arc<ScriptedPlugin> {
    Arc::new(ScriptedPlugin::vhp_shaped(detected()).mounting(&[(
        "kubeconfig",
        KUBECONFIG_PATH,
        Some(0o400),
    )]))
}

/// A VHI-shaped plugin that also declares a third secret, `unused`, which its
/// `prepare_run_access` never mounts.
fn vhi_mounting_two_of_three() -> Arc<ScriptedPlugin> {
    let mut schema = vhi_shaped_credential_schema();
    schema.push(field("unused", FieldKind::Secret, false, None));
    Arc::new(
        ScriptedPlugin::vhi_shaped(detected())
            .with_credential_schema(schema)
            .mounting(&[
                ("ssh_private_key", SSH_KEY_PATH, Some(0o400)),
                ("vinfra_password", VINFRA_PASSWORD_PATH, Some(0o400)),
            ]),
    )
}

fn port(plugin: Arc<ScriptedPlugin>) -> Arc<dyn ProductPluginPort> {
    Arc::new(FixedPluginPort::new(plugin))
}

fn pasted(value: &str) -> CredentialSubmission {
    CredentialSubmission::Material(CredentialMaterial::new(value.to_owned()))
}

async fn create(
    services: &ConcreteAppServices,
    tenant: Uuid,
    credentials: Vec<(&str, &str)>,
) -> Environment {
    services
        .environments
        .create_environment(
            &ctx(tenant),
            NewEnvironment {
                kubeconfig_credstore_ref: None,
                name: "runner-target".to_owned(),
                product_id: product(),
                description: None,
                kubeconfig: None,
                credentials: credentials
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), pasted(value)))
                    .collect(),
                default_branch: None,
                is_default: false,
            },
        )
        .await
        .unwrap()
}

/// A VHP-shaped environment holding `KUBECONFIG-CANARY`, in `tenant`, behind
/// services whose ports are the ones given. The resolver is built from the
/// environment's id, so it is a closure over it.
async fn vhp_environment(
    tenant: Uuid,
    pod_identity: Arc<dyn PodIdentityVerifier>,
    runner_pods: impl FnOnce(Uuid) -> Arc<dyn RunnerPodResolver>,
) -> (
    Arc<ConcreteAppServices>,
    Arc<RecordingCredStore>,
    Environment,
) {
    // The resolver needs the environment's id, which exists only after the
    // create; so the services are built once to create the row and the row's
    // id is scripted through a late-bound resolver.
    let late = Arc::new(LateRunnerPods::default());
    let (services, credstore) = build_services_for_runner_credentials(
        port(vhp_mounting()),
        pod_identity,
        Arc::clone(&late) as Arc<dyn RunnerPodResolver>,
    )
    .await;
    let environment = create(&services, tenant, vec![("kubeconfig", KUBECONFIG_CANARY)]).await;
    late.set(runner_pods(environment.id));
    (services, credstore, environment)
}

/// A resolver whose answer is installed after construction, so a test can
/// script the id of a row the same services created.
#[derive(Default)]
struct LateRunnerPods(std::sync::Mutex<Option<Arc<dyn RunnerPodResolver>>>);

impl LateRunnerPods {
    fn set(&self, inner: Arc<dyn RunnerPodResolver>) {
        *self.0.lock().unwrap() = Some(inner);
    }
}

#[async_trait::async_trait]
impl RunnerPodResolver for LateRunnerPods {
    async fn environment_of(
        &self,
        pod: &crate::domain::ports::VerifiedRunnerPod,
    ) -> Result<Uuid, RunnerPodRefusal> {
        let inner = self
            .0
            .lock()
            .unwrap()
            .clone()
            .expect("the test installs a resolver before calling the route");
        inner.environment_of(pod).await
    }
}

fn vouching_for(id: Uuid) -> Arc<dyn RunnerPodResolver> {
    Arc::new(ScriptedRunnerPods(Ok(id)))
}

fn refusing_runs(refusal: RunnerPodRefusal) -> impl FnOnce(Uuid) -> Arc<dyn RunnerPodResolver> {
    move |_| Arc::new(ScriptedRunnerPods(Err(refusal))) as Arc<dyn RunnerPodResolver>
}

#[tokio::test]
async fn a_verified_pod_of_a_live_run_gets_exactly_its_plugins_secret_mounts() {
    let tenant = Uuid::new_v4();
    let identity = Arc::new(ScriptedPodIdentity::proving(a_runner_pod(tenant)));
    let (services, _credstore, _environment) =
        vhp_environment(tenant, Arc::clone(&identity) as _, vouching_for).await;

    let files = services
        .environments
        .runner_credentials("tok")
        .await
        .unwrap();

    assert_eq!(files.len(), 1, "VHP mounts one secret: {files:?}");
    assert_eq!(files[0].path, KUBECONFIG_PATH);
    assert_eq!(files[0].mode, Some(0o400));
    assert_eq!(files[0].content.as_bytes(), KUBECONFIG_CANARY.as_bytes());
    assert_eq!(identity.seen(), vec!["tok".to_owned()]);
    assert!(
        !format!("{files:?}").contains(KUBECONFIG_CANARY),
        "a RunnerFile's Debug must not render its content: {files:?}"
    );
}

#[tokio::test]
async fn only_the_mounts_prepare_run_access_names_are_returned() {
    let tenant = Uuid::new_v4();
    let plugin = vhi_mounting_two_of_three();
    let late = Arc::new(LateRunnerPods::default());
    let (services, _credstore) = build_services_for_runner_credentials(
        port(Arc::clone(&plugin)),
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(tenant))),
        Arc::clone(&late) as Arc<dyn RunnerPodResolver>,
    )
    .await;
    let environment = create(
        &services,
        tenant,
        vec![
            ("node_host", "10.0.0.1"),
            ("ssh_private_key", SSH_KEY),
            ("vinfra_password", VINFRA_PASSWORD),
            ("unused", UNUSED_SECRET),
        ],
    )
    .await;
    assert!(
        environment.credentials.iter().any(|c| c.key == "unused"),
        "the fixture must hold the credential the run does not mount, or the \
         absence below proves nothing: {:?}",
        environment.credentials
    );
    late.set(vouching_for(environment.id));

    let files = services
        .environments
        .runner_credentials("tok")
        .await
        .unwrap();

    let mut got: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|f| (f.path.clone(), f.content.as_bytes().to_vec()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (SSH_KEY_PATH.to_owned(), SSH_KEY.as_bytes().to_vec()),
            (
                VINFRA_PASSWORD_PATH.to_owned(),
                VINFRA_PASSWORD.as_bytes().to_vec()
            ),
        ],
        "exactly the plugin's two mounts, each with its own bytes, and nothing for `unused`"
    );
    assert!(
        !plugin.run_access_saw_material(),
        "prepare_run_access works from references alone and is handed no material"
    );
}

#[tokio::test]
async fn an_unauthenticated_pod_is_forbidden_and_reads_nothing() {
    let tenant = Uuid::new_v4();
    let (services, credstore, _environment) = vhp_environment(
        tenant,
        Arc::new(ScriptedPodIdentity::refusing(
            PodIdentityRefusal::Unauthenticated("x"),
        )),
        vouching_for,
    )
    .await;
    let reads_before = credstore.reads();

    let result = services.environments.runner_credentials("tok").await;

    assert!(
        matches!(result, Err(DomainError::RunnerForbidden)),
        "{result:?}"
    );
    assert_eq!(
        credstore.reads(),
        reads_before,
        "nothing read from credstore"
    );
}

#[tokio::test]
async fn an_unreachable_api_server_is_unavailable() {
    let tenant = Uuid::new_v4();
    let (services, credstore, _environment) = vhp_environment(
        tenant,
        Arc::new(ScriptedPodIdentity::refusing(
            PodIdentityRefusal::Unavailable("x"),
        )),
        vouching_for,
    )
    .await;
    let reads_before = credstore.reads();

    let result = services.environments.runner_credentials("tok").await;

    assert!(
        matches!(result, Err(DomainError::RunnerCredentialsUnavailable)),
        "{result:?}"
    );
    assert_eq!(
        credstore.reads(),
        reads_before,
        "nothing read from credstore"
    );
}

#[tokio::test]
async fn a_run_qa_runs_does_not_vouch_for_is_forbidden() {
    let tenant = Uuid::new_v4();
    let (services, credstore, _environment) = vhp_environment(
        tenant,
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(tenant))),
        refusing_runs(RunnerPodRefusal::NotFound),
    )
    .await;
    let reads_before = credstore.reads();

    let result = services.environments.runner_credentials("tok").await;

    assert!(
        matches!(result, Err(DomainError::RunnerForbidden)),
        "{result:?}"
    );
    assert_eq!(
        credstore.reads(),
        reads_before,
        "nothing read from credstore"
    );
}

#[tokio::test]
async fn qa_runs_unreachable_is_unavailable() {
    let tenant = Uuid::new_v4();
    let (services, credstore, _environment) = vhp_environment(
        tenant,
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(tenant))),
        refusing_runs(RunnerPodRefusal::Unavailable("x")),
    )
    .await;
    let reads_before = credstore.reads();

    let result = services.environments.runner_credentials("tok").await;

    assert!(
        matches!(result, Err(DomainError::RunnerCredentialsUnavailable)),
        "{result:?}"
    );
    assert_eq!(
        credstore.reads(),
        reads_before,
        "nothing read from credstore"
    );
}

/// qa-runs vouches for the run and names tenant A's environment, but the pod
/// is labelled tenant B: the read runs under B's scope, so the row is not
/// there to read.
#[tokio::test]
async fn an_environment_in_another_tenant_is_forbidden() {
    let owner = Uuid::new_v4();
    let other = Uuid::new_v4();
    let (services, credstore, _environment) = vhp_environment(
        owner,
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(other))),
        vouching_for,
    )
    .await;
    let reads_before = credstore.reads();

    let result = services.environments.runner_credentials("tok").await;

    assert!(
        matches!(result, Err(DomainError::RunnerForbidden)),
        "{result:?}"
    );
    assert_eq!(
        credstore.reads(),
        reads_before,
        "nothing read from credstore"
    );
}

#[tokio::test]
async fn a_missing_credstore_entry_is_unavailable_and_names_no_reference() {
    let tenant = Uuid::new_v4();
    let (services, credstore, environment) = vhp_environment(
        tenant,
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(tenant))),
        vouching_for,
    )
    .await;
    let reference = environment.credentials[0].credstore_ref.clone();
    credstore.forget(&reference);

    let result = services.environments.runner_credentials("tok").await;

    let Err(error) = result else {
        panic!("a missing credential must refuse, got {result:?}");
    };
    assert!(
        matches!(error, DomainError::RunnerCredentialsUnavailable),
        "{error:?}"
    );
    assert!(
        !format!("{error:?}").contains(&reference) && !error.to_string().contains(&reference),
        "the error names the reference: {error:?}"
    );
}

/// Install a capturing subscriber, drive `runner_credentials` once to register
/// every callsite against it (see `CapturedLogs`' doc for why), then measure
/// a second call from a clean buffer.
async fn captured(
    services: &ConcreteAppServices,
    token: &str,
) -> (String, Result<usize, DomainError>) {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let _warmup = services.environments.runner_credentials(token).await;
    tracing::callsite::rebuild_interest_cache();
    logs.clear();

    let result = services
        .environments
        .runner_credentials(token)
        .await
        .map(|files| files.len());
    (logs.text(), result)
}

#[tokio::test]
async fn no_material_reaches_a_log_line() {
    // A distinctive token rather than the bare "tok": "tok" is a substring of
    // ordinary words a log line may carry ("token"), which would make the
    // absence assertion below fail for the wrong reason.
    const TOKEN: &str = "RUNNER-TOKEN-CANARY";
    let tenant = Uuid::new_v4();
    let (services, _credstore, environment) = vhp_environment(
        tenant,
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(tenant))),
        vouching_for,
    )
    .await;

    let (emitted, result) = captured(&services, TOKEN).await;

    assert_eq!(result.unwrap(), 1);
    assert!(
        emitted.contains("runner credentials granted"),
        "the capture is not seeing the grant, so the absences below would be vacuous: {emitted}"
    );
    assert!(
        !emitted.contains(KUBECONFIG_CANARY),
        "credential material reached a log line: {emitted}"
    );
    assert!(
        !emitted.contains(TOKEN),
        "the pod's token reached a log line: {emitted}"
    );
    assert!(
        !emitted.contains(&environment.credentials[0].credstore_ref),
        "a credstore reference reached a log line: {emitted}"
    );
}

#[tokio::test]
async fn a_denial_is_logged_with_the_pod_and_the_reason() {
    let tenant = Uuid::new_v4();
    let (services, _credstore, _environment) = vhp_environment(
        tenant,
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(tenant))),
        refusing_runs(RunnerPodRefusal::NotFound),
    )
    .await;

    let (emitted, result) = captured(&services, "tok").await;

    assert!(
        matches!(result, Err(DomainError::RunnerForbidden)),
        "{result:?}"
    );
    for expected in [
        "runner credentials denied",
        "qa-run-1-abcde-n-0-1234",
        "qa-runs does not vouch for this run",
    ] {
        assert!(
            emitted.contains(expected),
            "the denial log line must carry {expected:?}: {emitted}"
        );
    }
}

/// A plugin that names a credstore reference the environment does not hold
/// (here, one that really exists in credstore, so a read would succeed) is
/// refused before anything is read: "only the environment's credentials" is
/// checked, not trusted to the plugin.
#[tokio::test]
async fn a_mount_naming_a_reference_the_environment_does_not_hold_is_unavailable() {
    const FOREIGN_REF: &str = "foreign-environments-kubeconfig";
    const FOREIGN_MATERIAL: &str = "FOREIGN-CANARY";
    let tenant = Uuid::new_v4();
    let plugin = Arc::new(
        ScriptedPlugin::vhp_shaped(detected())
            .mounting(&[("kubeconfig", KUBECONFIG_PATH, Some(0o400))])
            .mounting_foreign_reference(FOREIGN_REF, "/etc/qa/foreign"),
    );
    let late = Arc::new(LateRunnerPods::default());
    let (services, credstore) = build_services_for_runner_credentials(
        port(plugin),
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(tenant))),
        Arc::clone(&late) as Arc<dyn RunnerPodResolver>,
    )
    .await;
    let environment = create(&services, tenant, vec![("kubeconfig", KUBECONFIG_CANARY)]).await;
    credstore.seed(FOREIGN_REF, FOREIGN_MATERIAL);
    late.set(vouching_for(environment.id));
    let reads_before = credstore.reads();

    let (emitted, result) = captured(&services, "tok").await;

    assert!(
        matches!(result, Err(DomainError::RunnerCredentialsUnavailable)),
        "{result:?}"
    );
    assert_eq!(
        credstore.reads(),
        reads_before,
        "nothing read from credstore, not even the environment's own mount"
    );
    for expected in [
        "runner credentials unavailable",
        "the plugin named a credential this environment does not hold",
    ] {
        assert!(
            emitted.contains(expected),
            "the refusal must be logged with {expected:?}: {emitted}"
        );
    }
    assert!(
        !emitted.contains(FOREIGN_REF),
        "the foreign reference reached a log line: {emitted}"
    );
}

/// A pod whose tenant label is the nil UUID is refused before qa-runs is
/// asked: no tenant owns it, so there is nothing to vouch for.
#[tokio::test]
async fn a_pod_labelled_with_the_nil_tenant_is_forbidden_before_qa_runs_is_asked() {
    let runs = Arc::new(CountingRunnerPods::default());
    let (services, credstore) = build_services_for_runner_credentials(
        port(vhp_mounting()),
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(Uuid::nil()))),
        Arc::clone(&runs) as Arc<dyn RunnerPodResolver>,
    )
    .await;

    let (emitted, result) = captured(&services, "tok").await;

    assert!(
        matches!(result, Err(DomainError::RunnerForbidden)),
        "{result:?}"
    );
    assert_eq!(
        runs.asked(),
        0,
        "qa-runs must not be asked about a nil tenant"
    );
    assert_eq!(credstore.reads(), 0, "nothing read from credstore");
    for expected in [
        "runner credentials denied",
        "qa-run-1-abcde-n-0-1234",
        "the pod's tenant label is the nil UUID",
    ] {
        assert!(
            emitted.contains(expected),
            "the denial log line must carry {expected:?}: {emitted}"
        );
    }
}

/// A verifier that never answers does not hold the request forever: the
/// whole call is bounded, and its expiry is a logged 503.
#[tokio::test]
async fn a_hanging_dependency_is_unavailable_once_the_deadline_passes() {
    let (services, credstore) = build_services_for_runner_credentials(
        port(vhp_mounting()),
        Arc::new(HangingPodIdentity),
        Arc::new(CountingRunnerPods::default()),
    )
    .await;
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    tracing::callsite::rebuild_interest_cache();
    // Paused only now: the database above was opened on the real clock.
    tokio::time::pause();
    let started = tokio::time::Instant::now();

    let result = services.environments.runner_credentials("tok").await;

    assert!(
        matches!(result, Err(DomainError::RunnerCredentialsUnavailable)),
        "{result:?}"
    );
    // The paused clock fires the timer at the deadline, rounded up to the
    // timer wheel's one-millisecond tick.
    let deadline = super::environments::runner_credentials::RUNNER_CREDENTIALS_DEADLINE;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= deadline && elapsed < deadline + std::time::Duration::from_secs(1),
        "the call ends at the deadline, not before and not never: {elapsed:?}"
    );
    assert_eq!(credstore.reads(), 0, "nothing read from credstore");
    let emitted = logs.text();
    for expected in [
        "runner credentials unavailable",
        "the request did not complete within its deadline",
    ] {
        assert!(
            emitted.contains(expected),
            "the expiry must be logged with {expected:?}: {emitted}"
        );
    }
}
