use async_trait::async_trait;

use super::*;
use crate::access::{RunAccess, RunVar, RunVarContract, RunnerSpec};
use crate::descriptor::{FieldDesc, FieldKind, FieldRole};
use crate::observation::{
    FailureClass, HealthOutcome, HealthState, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation,
};
use crate::plugin::{
    CredentialClassification, CredentialInput, EnvironmentHandle, QaProductPluginV1,
};

/// The one leak shape a throwaway fixture plugin exercises. `None` is the
/// well-behaved plugin; every other variant plants the canary in exactly one
/// surface, so the test that drives it pins the harness to catching that
/// surface specifically.
enum LeakMode {
    None,
    RemoteMessage,
    ObservedAttrs,
    Tracing,
    RunVar,
    /// Critical-1: a plugin that calls `.as_bytes()` on a resolved
    /// credential and `format!("{:?}", bytes)` — routine in a PEM/certificate
    /// parse-failure path — reaching the decimal byte-array encoding this
    /// harness's `marker_variants` was blind to before this fix round.
    ByteDebug,
    /// Important-2: leaks through `health_check`, whose signature carries no
    /// credential material at all — only reachable because a stateful
    /// plugin shares one `&dyn QaProductPluginV1` object across every
    /// method.
    HealthCheck,
    /// Important-3: leaks the PEM marker with its newlines collapsed to
    /// single spaces, defeating a byte-identical comparison while the
    /// content stays fully sensitive.
    ReformattedPem,
    /// Promoted-from-Minor: leaks through a declared schema's `help` text —
    /// unreachable via call arguments, since `credential_schema`/
    /// `observed_schema` take none, so the fixture must hold the canary
    /// itself to exercise this surface at all.
    FieldDescHelp,
    /// The same, through a declared schema's `label` — the surface
    /// `push_field_descs` itemises alongside `key` and `help`, and the one a
    /// plugin author is most likely to have built from a stored example.
    FieldDescLabel,
}

/// A plugin whose every method is inert except the one `LeakMode` names,
/// which echoes a canary marker straight out of `self.canary` — the
/// realistic shape of a careless plugin reproducing the 2026-08-28 leak, not
/// a contrived one. Holding the canary directly (rather than reading it back
/// out of call arguments) is what lets a single fixture also exercise
/// `credential_schema`/`observed_schema`/`health_check`, none of which take
/// any credential material as an argument.
///
/// The *argument*-driven half of the contract — a plugin reading a credential
/// back out of `CredentialInput`/`EnvironmentHandle` by its own declared key
/// — is covered by [`DeclaredKeyEchoPlugin`] below, which is what pins the
/// plant itself.
struct FixturePlugin {
    mode: LeakMode,
    canary: Canary,
}

#[async_trait]
impl QaProductPluginV1 for FixturePlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        match self.mode {
            LeakMode::FieldDescHelp => vec![FieldDesc {
                key: "note".to_owned(),
                label: "Note".to_owned(),
                kind: FieldKind::Text,
                required: false,
                role: None,
                in_table: false,
                in_detail: true,
                help: Some(self.canary.token.clone()),
            }],
            LeakMode::FieldDescLabel => vec![FieldDesc {
                key: "note".to_owned(),
                label: self.canary.token.clone(),
                kind: FieldKind::Text,
                required: false,
                role: None,
                in_table: false,
                in_detail: true,
                help: None,
            }],
            LeakMode::None
            | LeakMode::RemoteMessage
            | LeakMode::ObservedAttrs
            | LeakMode::Tracing
            | LeakMode::RunVar
            | LeakMode::ByteDebug
            | LeakMode::HealthCheck
            | LeakMode::ReformattedPem => Vec::new(),
        }
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        Vec::new()
    }

    async fn validate_credentials(
        &self,
        _input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        Ok(Vec::new())
    }

    async fn observe(&self, _env: &EnvironmentHandle<'_>) -> PluginObservation {
        match self.mode {
            LeakMode::RemoteMessage => {
                return PluginObservation {
                    environment: ObservationOutcome::Failed(
                        PluginFailure::classified(
                            FailureClass::Unreachable,
                            "could not reach the target",
                        )
                        .with_remote_message(self.canary.token.clone()),
                    ),
                    health: HealthOutcome::NotAttempted,
                };
            }
            LeakMode::ObservedAttrs => {
                let mut attrs = ObservedAttrs::default();
                attrs.set("kubeconfig_echo", self.canary.pem.clone());
                return PluginObservation {
                    environment: ObservationOutcome::Detected(attrs),
                    health: HealthOutcome::NotAttempted,
                };
            }
            LeakMode::Tracing => {
                let leaked = &self.canary.password;
                tracing::info!(detail = %leaked, "observed environment");
            }
            LeakMode::None
            | LeakMode::RunVar
            | LeakMode::ByteDebug
            | LeakMode::HealthCheck
            | LeakMode::ReformattedPem
            | LeakMode::FieldDescHelp
            | LeakMode::FieldDescLabel => {}
        }
        PluginObservation {
            environment: ObservationOutcome::Failed(PluginFailure::classified(
                FailureClass::Malformed,
                "the document could not be parsed",
            )),
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn prepare_run_access(
        &self,
        _env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        let mut access = RunAccess {
            mounts: Vec::new(),
            env: Vec::new(),
            service_account: None,
        };
        match self.mode {
            LeakMode::RunVar => {
                access.env.push(RunVar {
                    name: "DEBUG_CREDENTIAL".to_owned(),
                    value: self.canary.pem.clone(),
                });
            }
            LeakMode::ByteDebug => {
                #[allow(clippy::use_debug)]
                let leaked = format!("{:?}", self.canary.pem.as_bytes());
                access.env.push(RunVar {
                    name: "DEBUG_BYTES".to_owned(),
                    value: leaked,
                });
            }
            LeakMode::ReformattedPem => {
                let collapsed = self
                    .canary
                    .pem
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                access.service_account = Some(collapsed);
            }
            LeakMode::None
            | LeakMode::RemoteMessage
            | LeakMode::ObservedAttrs
            | LeakMode::Tracing
            | LeakMode::HealthCheck
            | LeakMode::FieldDescHelp
            | LeakMode::FieldDescLabel => {}
        }
        Ok(access)
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        RunnerSpec::default()
    }

    fn env_contract(&self) -> RunVarContract {
        RunVarContract::default()
    }

    async fn health_check(&self) -> Result<HealthState, PluginFailure> {
        if matches!(self.mode, LeakMode::HealthCheck) {
            return Err(
                PluginFailure::classified(FailureClass::Internal, "health probe failed")
                    .with_remote_message(self.canary.token.clone()),
            );
        }
        Ok(HealthState::Ok)
    }
}

fn fixture(mode: LeakMode, canary: &Canary) -> FixturePlugin {
    FixturePlugin {
        mode,
        canary: canary.clone(),
    }
}

#[tokio::test]
async fn a_well_behaved_plugin_passes() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::None, &canary);
    assert_no_leak(&plugin, &canary).await;
}

#[tokio::test]
#[should_panic(
    expected = "marker `token` reached surface `observe().environment PluginFailure.remote_message`"
)]
async fn a_plugin_that_echoes_the_canary_into_remote_message_fails() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::RemoteMessage, &canary);
    assert_no_leak(&plugin, &canary).await;
}

#[tokio::test]
#[should_panic(expected = "marker `pem` reached surface `observe() ObservedAttrs JSON`")]
async fn a_plugin_that_echoes_the_canary_into_observed_attrs_fails() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::ObservedAttrs, &canary);
    assert_no_leak(&plugin, &canary).await;
}

#[tokio::test]
#[should_panic(expected = "marker `password` reached surface `tracing events`")]
async fn a_plugin_that_logs_the_canary_fails() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::Tracing, &canary);
    assert_no_leak(&plugin, &canary).await;
}

#[tokio::test]
#[should_panic(expected = "marker `pem` reached surface `RunAccess.env RunVar.value`")]
async fn a_plugin_that_returns_the_canary_in_a_run_var_fails() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::RunVar, &canary);
    assert_no_leak(&plugin, &canary).await;
}

/// Critical-1 regression test: a plugin that `Debug`-prints a resolved
/// credential's raw bytes (`SecretValue::as_bytes`) — routine in a
/// PEM/certificate parse-failure path — must be caught even though the
/// decimal byte-array text shares no substring with the marker's own raw or
/// escaped-string form.
#[tokio::test]
#[should_panic(
    expected = "marker `pem` reached surface `RunAccess.env RunVar.value` (encoding: decimal byte array)"
)]
async fn a_plugin_that_debug_prints_credential_bytes_fails() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::ByteDebug, &canary);
    assert_no_leak(&plugin, &canary).await;
}

/// Important-2 regression test: `health_check` takes no credential material
/// in its signature, but a stateful plugin shares one `&dyn
/// QaProductPluginV1` object across every method, so it can still leak
/// through `health_check`'s own `PluginFailure.remote_message`.
#[tokio::test]
#[should_panic(
    expected = "marker `token` reached surface `health_check() PluginFailure.remote_message`"
)]
async fn a_plugin_that_leaks_through_health_check_fails() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::HealthCheck, &canary);
    assert_no_leak(&plugin, &canary).await;
}

/// Important-3 regression test: collapsing the PEM's embedded newlines to
/// single spaces changes its bytes without changing what was leaked, and
/// must not defeat detection.
#[tokio::test]
#[should_panic(
    expected = "marker `pem` reached surface `RunAccess.service_account` (whitespace-normalised match)"
)]
async fn a_plugin_that_reformats_the_pem_before_leaking_it_fails() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::ReformattedPem, &canary);
    assert_no_leak(&plugin, &canary).await;
}

/// Promoted-from-Minor regression test: `credential_schema`/
/// `observed_schema` take no arguments, so the only way a real plugin leaks
/// through a `FieldDesc.help` string is a schema built from a stored example
/// that was, by mistake, a real credential — exactly what a fixture holding
/// its own canary can reproduce.
#[tokio::test]
#[should_panic(expected = "marker `token` reached surface `credential_schema() FieldDesc.help`")]
async fn a_plugin_that_leaks_the_canary_through_a_field_desc_help_fails() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::FieldDescHelp, &canary);
    assert_no_leak(&plugin, &canary).await;
}

/// `push_field_descs` itemises `key`, `label` *and* `help`; only `help` was
/// pinned by a test, so `label` could have stopped being checked with nothing
/// to notice.
#[tokio::test]
#[should_panic(expected = "marker `token` reached surface `credential_schema() FieldDesc.label`")]
async fn a_plugin_that_leaks_the_canary_through_a_field_desc_label_fails() {
    let canary = Canary::vhp_shaped();
    let plugin = fixture(LeakMode::FieldDescLabel, &canary);
    assert_no_leak(&plugin, &canary).await;
}

// ── Critical-1: the plant must come from the plugin's declared schema ─────
//
// Every `FixturePlugin` above ignores its `_input`/`_env` arguments and leaks
// from `self.canary`. That proves the *assertion* machinery works; it proves
// nothing about the *plant*. The fixtures below are the standing guard: they
// read a credential back out of a call argument, by the key they themselves
// declared, and echo it. Against the pre-fix harness — which planted under
// three hardcoded keys and passed `Value::Null` as the configuration — both
// looked up a declared key, found nothing, returned on their first line, and
// the harness reported clean.

const DECLARED_SECRET_KEY: &str = "kubeconfig";
const DECLARED_CONFIG_KEY: &str = "vpadm_namespace";

/// Shaped exactly the way Task 9's VHP plugin declares itself.
fn vhp_shaped_credential_schema() -> Vec<FieldDesc> {
    vec![
        FieldDesc {
            key: DECLARED_SECRET_KEY.to_owned(),
            label: "Kubeconfig".to_owned(),
            kind: FieldKind::MultilineSecret,
            required: true,
            role: None,
            in_table: false,
            in_detail: false,
            help: None,
        },
        FieldDesc {
            key: DECLARED_CONFIG_KEY.to_owned(),
            label: "vpadm namespace".to_owned(),
            kind: FieldKind::Text,
            required: false,
            role: None,
            in_table: false,
            in_detail: true,
            help: None,
        },
    ]
}

/// Which argument the fixture reads its declared credential back out of.
enum EchoSource {
    /// `CredentialInput` — the validation path.
    SubmittedForm,
    /// `EnvironmentHandle`'s resolved slots plus its `config` — the observe
    /// path, and the one that also pins the synthesised configuration.
    Environment,
}

struct DeclaredKeyEchoPlugin(EchoSource);

#[async_trait]
impl QaProductPluginV1 for DeclaredKeyEchoPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        vhp_shaped_credential_schema()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        Vec::new()
    }

    async fn validate_credentials(
        &self,
        input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        let Some(submitted) = input.get(DECLARED_SECRET_KEY) else {
            return Ok(Vec::new());
        };
        if matches!(self.0, EchoSource::SubmittedForm) {
            // The careless shape: the submitted bytes end up in a string the
            // platform will render.
            return Ok(vec![CredentialClassification::secret(
                String::from_utf8_lossy(submitted.as_bytes()).into_owned(),
            )]);
        }
        Ok(vec![CredentialClassification::secret(DECLARED_SECRET_KEY)])
    }

    async fn observe(&self, env: &EnvironmentHandle<'_>) -> PluginObservation {
        // A real plugin reads its non-secret configuration first and gives up
        // when it is absent — VHP reads `vpadm_namespace` exactly here. Under
        // a `Value::Null` configuration this returns before touching a
        // credential at all, which is what made the old harness vacuous.
        let Some(_namespace) = env
            .config
            .get(DECLARED_CONFIG_KEY)
            .and_then(serde_json::Value::as_str)
        else {
            return PluginObservation {
                environment: ObservationOutcome::Failed(PluginFailure::classified(
                    FailureClass::Malformed,
                    "no namespace in the environment configuration",
                )),
                health: HealthOutcome::NotAttempted,
            };
        };
        let Some(resolved) = env.resolved(DECLARED_SECRET_KEY) else {
            return PluginObservation {
                environment: ObservationOutcome::Failed(PluginFailure::classified(
                    FailureClass::NotFound,
                    "no kubeconfig credential",
                )),
                health: HealthOutcome::NotAttempted,
            };
        };
        if matches!(self.0, EchoSource::Environment) {
            return PluginObservation {
                environment: ObservationOutcome::Failed(
                    PluginFailure::classified(FailureClass::Malformed, "could not parse")
                        .with_remote_message(
                            String::from_utf8_lossy(resolved.as_bytes()).into_owned(),
                        ),
                ),
                health: HealthOutcome::NotAttempted,
            };
        }
        PluginObservation {
            environment: ObservationOutcome::Detected(ObservedAttrs::default()),
            health: HealthOutcome::NotAttempted,
        }
    }

    /// The reference-only shape the contract requires: names the secret,
    /// never reads it.
    async fn prepare_run_access(
        &self,
        env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        let Some(credstore_ref) = env.credstore_ref(DECLARED_SECRET_KEY) else {
            return Err(PluginFailure::classified(
                FailureClass::NotFound,
                "no kubeconfig credential",
            ));
        };
        Ok(RunAccess {
            mounts: vec![MountSpec::Secret {
                credstore_ref: credstore_ref.to_owned(),
                path: "/etc/qa/kubeconfig".to_owned(),
                mode: Some(0o400),
            }],
            env: Vec::new(),
            service_account: None,
        })
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        RunnerSpec::default()
    }

    fn env_contract(&self) -> RunVarContract {
        RunVarContract::default()
    }
}

/// The plant must reach `CredentialInput` under the plugin's own declared
/// key. Before the plant was derived from `credential_schema()`, this plugin
/// found nothing under `"kubeconfig"` and the harness passed.
#[tokio::test]
#[should_panic(expected = "marker `pem` reached surface `CredentialClassification.key`")]
async fn a_plugin_that_echoes_a_declared_credential_out_of_its_input_fails() {
    let canary = Canary::vhp_shaped();
    assert_no_leak(&DeclaredKeyEchoPlugin(EchoSource::SubmittedForm), &canary).await;
}

/// The same for the `EnvironmentHandle` side — and, because this fixture
/// reads `config[vpadm_namespace]` before it reads any credential, it also
/// pins the synthesised non-null configuration. Under `Value::Null` it never
/// reaches the credential at all.
#[tokio::test]
#[should_panic(
    expected = "marker `pem` reached surface `observe().environment PluginFailure.remote_message`"
)]
async fn a_plugin_that_echoes_a_declared_credential_out_of_its_environment_fails() {
    let canary = Canary::vhp_shaped();
    assert_no_leak(&DeclaredKeyEchoPlugin(EchoSource::Environment), &canary).await;
}

// ── The reference-only requirement on `prepare_run_access` ───────────────

/// A plugin that refuses to prepare access without resolved plaintext.
/// Dispatch never resolves, so this shape cannot be dispatched without
/// pulling a plaintext kubeconfig into a process that today never holds one.
///
/// It reads the plaintext and then mounts the secret *by reference*, which
/// looks redundant and is deliberate: `MountSpec::ConfigValue.value` is a
/// scanned leak surface and the leak assertions run first, so a fixture that
/// also echoed the plaintext into its mount would be reported as a leak and
/// would never reach the contract assertion this one exists to pin. That
/// (realistic, and far more common) shape has its own fixture —
/// [`ConfigValueMountPlugin`].
struct ResolvedOnlyAccessPlugin;

#[async_trait]
impl QaProductPluginV1 for ResolvedOnlyAccessPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        vhp_shaped_credential_schema()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        Vec::new()
    }

    async fn validate_credentials(
        &self,
        _input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        Ok(vec![CredentialClassification::secret(DECLARED_SECRET_KEY)])
    }

    async fn observe(&self, _env: &EnvironmentHandle<'_>) -> PluginObservation {
        PluginObservation {
            environment: ObservationOutcome::Detected(ObservedAttrs::default()),
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn prepare_run_access(
        &self,
        env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        let (Some(_resolved), Some(credstore_ref)) = (
            env.resolved(DECLARED_SECRET_KEY),
            env.credstore_ref(DECLARED_SECRET_KEY),
        ) else {
            return Err(PluginFailure::classified(
                FailureClass::NotFound,
                "the kubeconfig was not resolved",
            ));
        };
        Ok(RunAccess {
            mounts: vec![MountSpec::Secret {
                credstore_ref: credstore_ref.to_owned(),
                path: "/etc/qa/kubeconfig".to_owned(),
                mode: Some(0o400),
            }],
            env: Vec::new(),
            service_account: None,
        })
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        RunnerSpec::default()
    }

    fn env_contract(&self) -> RunVarContract {
        RunVarContract::default()
    }
}

#[tokio::test]
#[should_panic(expected = "failed when given credstore references alone")]
async fn a_plugin_whose_run_access_needs_plaintext_fails() {
    let canary = Canary::vhp_shaped();
    assert_no_leak(&ResolvedOnlyAccessPlugin, &canary).await;
}

/// A plugin that materialises the credential it resolved into a
/// `MountSpec::ConfigValue` — the variant that exists precisely to carry a
/// value a plugin resolved itself, and therefore the one a plugin author
/// reaches for without thinking of it as a rendering surface.
///
/// Given references alone it returns `Ok`, naming the secret with a
/// `MountSpec::Secret`, so it satisfies the reference-only *verdict*. It does
/// **not** satisfy the two-drive comparison: swapping the mount variant
/// according to whether plaintext was available is itself reading the
/// plaintext, so this fixture now trips two rules and reports the leak only
/// because the leak assertions run first. That ordering is deliberate and
/// this fixture is one of the things that proves it — with the
/// `ConfigValue.value` push removed it panics on the divergence instead.
///
/// Before the harness scanned `ConfigValue.value`, only the mount's `.path`
/// was checked and this plugin was reported clean while handing a container a
/// plaintext kubeconfig.
struct ConfigValueMountPlugin;

#[async_trait]
impl QaProductPluginV1 for ConfigValueMountPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        vhp_shaped_credential_schema()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        Vec::new()
    }

    async fn validate_credentials(
        &self,
        _input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        Ok(vec![CredentialClassification::secret(DECLARED_SECRET_KEY)])
    }

    async fn observe(&self, _env: &EnvironmentHandle<'_>) -> PluginObservation {
        PluginObservation {
            environment: ObservationOutcome::Detected(ObservedAttrs::default()),
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn prepare_run_access(
        &self,
        env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        if let Some(resolved) = env.resolved(DECLARED_SECRET_KEY) {
            return Ok(RunAccess {
                mounts: vec![MountSpec::ConfigValue {
                    value: SecretValue::new(resolved.as_bytes().to_vec()),
                    path: "/etc/qa/kubeconfig".to_owned(),
                    mode: Some(0o400),
                }],
                env: Vec::new(),
                service_account: None,
            });
        }
        let Some(credstore_ref) = env.credstore_ref(DECLARED_SECRET_KEY) else {
            return Err(PluginFailure::classified(
                FailureClass::NotFound,
                "no kubeconfig credential",
            ));
        };
        Ok(RunAccess {
            mounts: vec![MountSpec::Secret {
                credstore_ref: credstore_ref.to_owned(),
                path: "/etc/qa/kubeconfig".to_owned(),
                mode: Some(0o400),
            }],
            env: Vec::new(),
            service_account: None,
        })
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        RunnerSpec::default()
    }

    fn env_contract(&self) -> RunVarContract {
        RunVarContract::default()
    }
}

/// A mount is a rendering surface like any other: what a `ConfigValue`
/// carries is written into a container's filesystem, and a harness that
/// checks only where it lands proves nothing about what lands there.
#[tokio::test]
#[should_panic(
    expected = "marker `pem` reached surface `RunAccess.mounts MountSpec::ConfigValue.value`"
)]
async fn a_plugin_that_mounts_resolved_plaintext_into_a_config_value_fails() {
    let canary = Canary::vhp_shaped();
    assert_no_leak(&ConfigValueMountPlugin, &canary).await;
}

/// The report must be actionable without becoming a leak of its own: naming
/// the mount and the marker is what an author needs, and the mounted bytes
/// are what this crate exists to keep out of logs. Written against the panic
/// payload rather than as `#[should_panic(expected = ..)]` because that
/// attribute can only assert what a message *contains* — never what it must
/// not.
#[test]
fn a_leak_report_for_a_mounted_config_value_never_quotes_the_value() {
    let canary = Canary::vhp_shaped();
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
        panic!("a current-thread runtime must build")
    };

    // The hook is process-global and other tests may panic concurrently;
    // silencing it only suppresses their console output, never their result,
    // and it keeps this deliberate panic out of the test log.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(assert_no_leak(&ConfigValueMountPlugin, &canary));
    }));
    std::panic::set_hook(previous);

    let Err(payload) = unwound else {
        panic!("the harness must fail a plugin that mounts the canary")
    };
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or_default();

    assert!(
        message.contains("MountSpec::ConfigValue.value"),
        "the report names the surface: {message}"
    );
    assert!(
        message.contains("marker `pem`"),
        "the report names the marker: {message}"
    );
    assert!(
        !message.contains("BEGIN PRIVATE KEY"),
        "the report quoted the mounted plaintext: {message}"
    );
    assert!(
        !message.contains("CANARYKEYMATERIAL"),
        "the report quoted the mounted plaintext: {message}"
    );
}

/// A plugin that refuses to prepare access for an environment nothing has
/// observed yet: it reads the `BaseUrl` role for a run variable and turns its
/// absence into an error.
///
/// Dispatch reaches freshly created environments, and
/// `EnvironmentHandle::observed` is `Option` for exactly that reason, so this
/// plugin cannot be dispatched onto one. **Both** of its drives fail, which
/// is the shape the old two-call inference could not see: with
/// `run_access.is_ok()` false, `run_access.is_ok() && ref_access.is_err()`
/// was false too and the harness reported clean on a plugin whose
/// `prepare_run_access` never succeeds at all.
struct ObservationRequiredAccessPlugin;

impl ObservationRequiredAccessPlugin {
    const BASE_URL_KEY: &'static str = "endpoint";
}

#[async_trait]
impl QaProductPluginV1 for ObservationRequiredAccessPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        vhp_shaped_credential_schema()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        vec![FieldDesc {
            key: Self::BASE_URL_KEY.to_owned(),
            label: "Endpoint".to_owned(),
            kind: FieldKind::Url,
            required: false,
            role: Some(FieldRole::BaseUrl),
            in_table: true,
            in_detail: true,
            help: None,
        }]
    }

    async fn validate_credentials(
        &self,
        _input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        Ok(vec![CredentialClassification::secret(DECLARED_SECRET_KEY)])
    }

    async fn observe(&self, _env: &EnvironmentHandle<'_>) -> PluginObservation {
        PluginObservation {
            environment: ObservationOutcome::Detected(ObservedAttrs::default()),
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn prepare_run_access(
        &self,
        env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        let Some(base_url) = env.observed_role(&self.observed_schema(), FieldRole::BaseUrl) else {
            return Err(PluginFailure::classified(
                FailureClass::NotFound,
                "the environment has not been observed",
            ));
        };
        Ok(RunAccess {
            mounts: Vec::new(),
            env: vec![RunVar {
                name: "E2E_BASE_URL".to_owned(),
                value: base_url,
            }],
            service_account: None,
        })
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        RunnerSpec::default()
    }

    fn env_contract(&self) -> RunVarContract {
        RunVarContract::default()
    }
}

/// The contract is unconditional — `prepare_run_access` must work from
/// credstore references alone — so the harness asserts it rather than
/// inferring it from a comparison of two drives. A plugin that fails the
/// reference-only drive fails this harness whatever the resolved drive did.
#[tokio::test]
#[should_panic(expected = "failed when given credstore references alone")]
async fn a_plugin_that_requires_an_observation_to_prepare_access_fails() {
    let canary = Canary::vhp_shaped();
    assert_no_leak(&ObservationRequiredAccessPlugin, &canary).await;
}

/// A plugin that quietly does less when it cannot see plaintext: given
/// resolved credentials it mounts the secret and announces the cluster, and
/// given references alone it returns an empty `RunAccess` rather than an
/// error.
///
/// This is the shape the old two-call inference was built to describe and
/// could not catch, and a flat "the refs-only drive returns `Ok`" cannot
/// catch either: it *does* return `Ok`. The run it produces is simply not the
/// run the operator asked for, and the divergence is itself the evidence that
/// it read the plaintext it was told never to require.
struct QuietlyDegradingAccessPlugin;

/// The two service accounts [`QuietlyDegradingAccessPlugin`] picks between, so
/// the divergence report can be checked for their absence by name.
const RESOLVED_SERVICE_ACCOUNT: &str = "qa-runner-with-plaintext";
const REFS_ONLY_SERVICE_ACCOUNT: &str = "qa-runner-from-refs";

#[async_trait]
impl QaProductPluginV1 for QuietlyDegradingAccessPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        vhp_shaped_credential_schema()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        Vec::new()
    }

    async fn validate_credentials(
        &self,
        _input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        Ok(vec![CredentialClassification::secret(DECLARED_SECRET_KEY)])
    }

    async fn observe(&self, _env: &EnvironmentHandle<'_>) -> PluginObservation {
        PluginObservation {
            environment: ObservationOutcome::Detected(ObservedAttrs::default()),
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn prepare_run_access(
        &self,
        env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        let (Some(_resolved), Some(credstore_ref)) = (
            env.resolved(DECLARED_SECRET_KEY),
            env.credstore_ref(DECLARED_SECRET_KEY),
        ) else {
            // No plaintext, no complaint: an `Ok` carrying no mount, no
            // variables and a fallback service account, which dispatch would
            // happily run.
            return Ok(RunAccess {
                mounts: Vec::new(),
                env: Vec::new(),
                service_account: Some(REFS_ONLY_SERVICE_ACCOUNT.to_owned()),
            });
        };
        Ok(RunAccess {
            mounts: vec![MountSpec::Secret {
                credstore_ref: credstore_ref.to_owned(),
                path: "/etc/qa/kubeconfig".to_owned(),
                mode: Some(0o400),
            }],
            env: vec![RunVar {
                name: "E2E_CLUSTER".to_owned(),
                value: "reachable".to_owned(),
            }],
            service_account: Some(RESOLVED_SERVICE_ACCOUNT.to_owned()),
        })
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        RunnerSpec::default()
    }

    fn env_contract(&self) -> RunVarContract {
        RunVarContract::default()
    }
}

/// "Must work from `credstore_ref` alone" is a statement about the *output*,
/// not merely about whether the call returns. A plugin whose access changes
/// depending on whether plaintext was handed to it read the plaintext.
#[tokio::test]
#[should_panic(
    expected = "prepare_run_access returned different access when it was given resolved plaintext"
)]
async fn a_plugin_that_quietly_degrades_without_plaintext_fails() {
    let canary = Canary::vhp_shaped();
    assert_no_leak(&QuietlyDegradingAccessPlugin, &canary).await;
}

/// The divergence report is a message about values that may have been built
/// from plaintext, so it may not contain any of them — on *either* side. The
/// refs-only side is not safe by construction: the same plugin object was
/// handed plaintext twice earlier in the drive, by `observe` and by the
/// resolved `prepare_run_access`, and the canary scan that runs first cannot
/// see a base64, truncated or hashed derivation.
///
/// Read through the panic payload, since `#[should_panic]` can assert only
/// what a message contains. This is the wired-in guard for the redaction;
/// [`a_diverging_run_var_value_is_named_with_neither_side_quoted`] pins the
/// wording of the individual lines.
#[test]
fn a_divergence_report_names_the_fields_and_quotes_neither_side() {
    let canary = Canary::vhp_shaped();
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
        panic!("a current-thread runtime must build")
    };

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(assert_no_leak(&QuietlyDegradingAccessPlugin, &canary));
    }));
    std::panic::set_hook(previous);

    let Err(payload) = unwound else {
        panic!("the harness must fail a plugin whose two drives disagree")
    };
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or_default();

    assert!(
        message.contains("service_account differs between the drives"),
        "the report names the field: {message}"
    );
    assert!(
        message.contains("RunVar `E2E_CLUSTER`"),
        "the report names the variable: {message}"
    );
    assert!(
        !message.contains(RESOLVED_SERVICE_ACCOUNT),
        "the report quoted the resolved side: {message}"
    );
    assert!(
        !message.contains(REFS_ONLY_SERVICE_ACCOUNT),
        "the report quoted the reference-only side: {message}"
    );
}

// ── Layer 3 enforces layer 2 ─────────────────────────────────────────────

/// A plugin whose `observed_schema` declares a secret kind — a boot failure
/// at registration, and the harness must not let it get as far as being
/// driven.
struct SecretInObservedSchemaPlugin;

#[async_trait]
impl QaProductPluginV1 for SecretInObservedSchemaPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        Vec::new()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        vec![FieldDesc {
            key: "kubeconfig_echo".to_owned(),
            label: "Kubeconfig".to_owned(),
            kind: FieldKind::MultilineSecret,
            required: false,
            role: None,
            in_table: false,
            in_detail: true,
            help: None,
        }]
    }

    async fn validate_credentials(
        &self,
        _input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        Ok(Vec::new())
    }

    async fn observe(&self, _env: &EnvironmentHandle<'_>) -> PluginObservation {
        PluginObservation {
            environment: ObservationOutcome::Detected(ObservedAttrs::default()),
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn prepare_run_access(
        &self,
        _env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        Ok(RunAccess {
            mounts: Vec::new(),
            env: Vec::new(),
            service_account: None,
        })
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        RunnerSpec::default()
    }

    fn env_contract(&self) -> RunVarContract {
        RunVarContract::default()
    }
}

#[tokio::test]
#[should_panic(expected = "the plugin's declared schemas are invalid")]
async fn the_harness_enforces_the_schema_rules_itself() {
    let canary = Canary::vhp_shaped();
    assert_no_leak(&SecretInObservedSchemaPlugin, &canary).await;
}

// ── The two-drive comparison ─────────────────────────────────────────────

/// A run variable whose *value* moves between the drives is the subtler half
/// of the rule, and the half whose report must be careful: the divergence is
/// the claim that this value was built from plaintext, so quoting either side
/// prints the thing the claim is about — the plugin is one object that was
/// handed plaintext before both drives ran.
///
/// This calls `access_divergences` directly, so it guards the **message**, not
/// the harness wiring: it would stay green if nothing ever called the
/// comparison. The wiring is pinned by
/// [`a_plugin_that_quietly_degrades_without_plaintext_fails`] and
/// [`a_divergence_report_names_the_fields_and_quotes_neither_side`].
#[test]
fn a_diverging_run_var_value_is_named_with_neither_side_quoted() {
    let resolved = RunAccess {
        mounts: Vec::new(),
        env: vec![RunVar {
            name: "E2E_TOKEN".to_owned(),
            value: "derived-from-the-plaintext".to_owned(),
        }],
        service_account: None,
    };
    let refs_only = RunAccess {
        mounts: Vec::new(),
        env: vec![RunVar {
            name: "E2E_TOKEN".to_owned(),
            value: "placeholder".to_owned(),
        }],
        service_account: None,
    };

    let found = access_divergences(&resolved, &refs_only);

    assert_eq!(found.len(), 1, "got {found:?}");
    let report = &found[0];
    assert!(report.contains("E2E_TOKEN"), "got {report}");
    assert!(
        !report.contains("derived-from-the-plaintext"),
        "the report quoted the resolved side: {report}"
    );
    assert!(
        !report.contains("placeholder"),
        "the report quoted the reference-only side: {report}"
    );
}

/// `service_account` has two divergence shapes and neither may print a value:
/// set on one drive only (structural, so the report says which drive and
/// stops), and set differently on both (a value divergence, redacted like any
/// other). The second shape is the one that shipped quoting both sides in the
/// clear.
#[test]
fn a_diverging_service_account_is_reported_without_either_account() {
    let access = |account: Option<&str>| RunAccess {
        mounts: Vec::new(),
        env: Vec::new(),
        service_account: account.map(ToOwned::to_owned),
    };

    let differing = access_divergences(&access(Some("privileged")), &access(Some("restricted")));
    assert_eq!(differing.len(), 1, "got {differing:?}");
    assert!(
        differing[0].contains("service_account differs between the drives"),
        "got {differing:?}"
    );
    assert!(
        !differing[0].contains("privileged") && !differing[0].contains("restricted"),
        "the report quoted an account: {differing:?}"
    );

    let resolved_only = access_divergences(&access(Some("privileged")), &access(None));
    assert_eq!(
        resolved_only,
        vec!["service_account is set only when resolved plaintext is available".to_owned()]
    );

    let refs_only = access_divergences(&access(None), &access(Some("restricted")));
    assert_eq!(
        refs_only,
        vec!["service_account is set only when resolved plaintext is absent".to_owned()]
    );

    assert!(access_divergences(&access(None), &access(None)).is_empty());
}

/// Neither run variables nor mounts have a contractual order — Task 18
/// injects both by name — so two drives that emit the same access in a
/// different sequence agree.
#[test]
fn the_two_drives_may_emit_the_same_access_in_a_different_order() {
    let vars = [
        RunVar {
            name: "A".to_owned(),
            value: "1".to_owned(),
        },
        RunVar {
            name: "B".to_owned(),
            value: "2".to_owned(),
        },
    ];
    let mounts = || {
        [
            MountSpec::Secret {
                credstore_ref: "qa/canary/one".to_owned(),
                path: "/one".to_owned(),
                mode: Some(0o400),
            },
            MountSpec::Secret {
                credstore_ref: "qa/canary/two".to_owned(),
                path: "/two".to_owned(),
                mode: None,
            },
        ]
    };
    let [first_mount, second_mount] = mounts();
    let resolved = RunAccess {
        mounts: vec![first_mount, second_mount],
        env: vars.clone().into_iter().collect(),
        service_account: Some("qa-runner".to_owned()),
    };
    let [first_mount, second_mount] = mounts();
    let refs_only = RunAccess {
        mounts: vec![second_mount, first_mount],
        env: vars.into_iter().rev().collect(),
        service_account: Some("qa-runner".to_owned()),
    };

    assert!(access_divergences(&resolved, &refs_only).is_empty());
}

// ── The plant itself ─────────────────────────────────────────────────────

#[test]
fn the_plant_covers_every_declared_key_and_the_fixed_extras() {
    let canary = Canary::vhp_shaped();
    let planted = plant(&vhp_shaped_credential_schema(), &canary);

    let keys: Vec<&str> = planted.input.fields.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec![
            DECLARED_SECRET_KEY,
            "password",
            "pem",
            "token",
            DECLARED_CONFIG_KEY,
        ],
        "declared keys plus the three fixed extras"
    );

    let slot_keys: Vec<&str> = planted.slots.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(keys, slot_keys, "both directions are planted alike");
}

#[test]
fn a_declared_multiline_secret_is_planted_with_the_pem_marker() {
    let canary = Canary::vhp_shaped();
    let planted = plant(&vhp_shaped_credential_schema(), &canary);

    let submitted = planted
        .input
        .get(DECLARED_SECRET_KEY)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
    assert_eq!(submitted.as_deref(), Some(canary.pem.as_str()));

    let resolved = planted
        .slots
        .iter()
        .find(|s| s.key == DECLARED_SECRET_KEY)
        .and_then(|s| s.value.as_ref())
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
    assert_eq!(resolved.as_deref(), Some(canary.pem.as_str()));
}

/// The configuration a plugin reads must be a real object carrying the
/// declared non-secret fields, never `Value::Null` — a plugin that reads
/// `config["vpadm_namespace"]` and bails is otherwise never driven at all.
#[test]
fn the_config_is_synthesised_from_the_declared_non_secret_fields() {
    let canary = Canary::vhp_shaped();
    let planted = plant(&vhp_shaped_credential_schema(), &canary);

    assert!(planted.config.is_object(), "got {}", planted.config);
    assert!(
        planted
            .config
            .get(DECLARED_CONFIG_KEY)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|v| !v.is_empty()),
        "got {}",
        planted.config
    );
    assert!(
        planted.config.get(DECLARED_SECRET_KEY).is_none(),
        "a secret field never reaches the non-secret configuration: {}",
        planted.config
    );
}

/// A plugin declaring nothing must still be driven with credential material,
/// or the harness would report clean without having planted anything at all.
#[test]
fn a_plugin_declaring_no_credential_schema_is_still_probed() {
    let canary = Canary::vhp_shaped();
    let planted = plant(&[], &canary);
    let keys: Vec<&str> = planted.input.fields.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["password", "pem", "token"]);
}

/// Every slot carries a credstore reference whether or not it was resolved:
/// `prepare_run_access` is driven from references alone and must find one.
#[test]
fn reference_only_slots_keep_their_credstore_refs_and_drop_their_values() {
    let canary = Canary::vhp_shaped();
    let planted = plant(&vhp_shaped_credential_schema(), &canary);
    let refs = reference_only(&planted.slots);

    assert_eq!(refs.len(), planted.slots.len());
    for (slot, reference) in planted.slots.iter().zip(&refs) {
        assert_eq!(slot.key, reference.key);
        assert_eq!(slot.credstore_ref, reference.credstore_ref);
        assert!(slot.value.is_some());
        assert!(reference.value.is_none());
    }
}
