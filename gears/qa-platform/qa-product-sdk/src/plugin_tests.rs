use super::*;
use crate::access::RunAccess;
use crate::descriptor::{FieldKind, SchemaError};
use crate::observation::{HealthOutcome, ObservationOutcome};

fn field(key: &str, kind: FieldKind) -> FieldDesc {
    FieldDesc {
        key: key.to_owned(),
        label: key.to_owned(),
        kind,
        required: false,
        role: None,
        in_table: false,
        in_detail: true,
        help: None,
    }
}

/// An observed field claiming `role`, for the projection tests below.
fn role_field(key: &str, role: FieldRole) -> FieldDesc {
    FieldDesc {
        role: Some(role),
        ..field(key, FieldKind::Text)
    }
}

/// A plugin whose schemas are whatever the test hands it, and whose every
/// method is inert.
struct SchemaOnlyPlugin {
    credential: Vec<FieldDesc>,
    observed: Vec<FieldDesc>,
}

#[async_trait]
impl QaProductPluginV1 for SchemaOnlyPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        self.credential.clone()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        self.observed.clone()
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

/// The 2026-08-28 path, at the point the credential is still plaintext: one
/// `tracing::debug!(?input)` in the gear's own handler used to render every
/// submitted secret. `SecretValue`'s `Debug` is what makes that impossible,
/// and this pins that the map's derived `Debug` does not defeat it.
#[test]
fn credential_input_debug_prints_keys_and_redacts_values() {
    let mut input = CredentialInput::default();
    input.fields.insert(
        "kubeconfig".to_owned(),
        SecretValue::from("-----BEGIN PRIVATE KEY-----"),
    );

    let rendered = format!("{input:?}");

    assert!(rendered.contains("kubeconfig"), "got {rendered}");
    assert!(!rendered.contains("BEGIN PRIVATE KEY"), "got {rendered}");
    assert!(rendered.contains("REDACTED"), "got {rendered}");
}

#[test]
fn a_registered_plugin_validates_both_schemas_on_construction() {
    let plugin = Arc::new(SchemaOnlyPlugin {
        credential: vec![field("kubeconfig", FieldKind::MultilineSecret)],
        observed: vec![field("platformVersion", FieldKind::Text)],
    });

    let registered = RegisteredPlugin::new(plugin);

    assert!(registered.is_ok(), "got {:?}", registered.err());
}

/// The whole point of the type: there is no way to hold an unvalidated
/// plugin, so a gear cannot forget to call `validate_schemas`.
#[test]
fn a_plugin_with_a_secret_in_its_observed_schema_cannot_be_registered() {
    let plugin = Arc::new(SchemaOnlyPlugin {
        credential: Vec::new(),
        observed: vec![field("kubeconfig_echo", FieldKind::MultilineSecret)],
    });

    let err = RegisteredPlugin::new(plugin).err();

    assert!(
        matches!(&err, Some(SchemaError::SecretInObservedSchema { key }) if key == "kubeconfig_echo"),
        "got {err:?}"
    );
}

#[test]
fn a_registered_plugin_derefs_to_the_plugin() {
    let plugin = Arc::new(SchemaOnlyPlugin {
        credential: vec![field("kubeconfig", FieldKind::MultilineSecret)],
        observed: Vec::new(),
    });
    let registered = RegisteredPlugin::new(plugin).ok();
    let Some(registered) = registered else {
        panic!("a valid plugin must register")
    };

    assert_eq!(registered.credential_schema().len(), 1);
    assert_eq!(registered.inner().observed_schema().len(), 0);
}

/// The dispatch-shaped handle: no plaintext anywhere, and the reference a
/// `MountSpec::Secret` needs is still there.
#[test]
fn a_reference_only_slot_yields_a_credstore_ref_and_no_value() {
    let slots = vec![CredentialSlot::reference_only(
        "kubeconfig",
        "qa/vhp/kubeconfig",
    )];
    let config = serde_json::json!({ "vpadm_namespace": "virtuozzo" });
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    assert_eq!(env.credstore_ref("kubeconfig"), Some("qa/vhp/kubeconfig"));
    assert!(env.resolved("kubeconfig").is_none());
    assert!(env.slot("nope").is_none());
}

#[test]
fn a_resolved_slot_yields_both() {
    let slots = vec![CredentialSlot::resolved(
        "kubeconfig",
        "qa/vhp/kubeconfig",
        SecretValue::from("apiVersion: v1"),
    )];
    let config = serde_json::Value::Null;
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    assert_eq!(env.credstore_ref("kubeconfig"), Some("qa/vhp/kubeconfig"));
    assert_eq!(
        env.resolved("kubeconfig").map(SecretValue::as_bytes),
        Some(b"apiVersion: v1".as_slice())
    );
}

/// A plugin can only report what it can know. `credstore_ref` is not one of
/// those things at validation time, which is why it is not on this type.
#[test]
fn a_classification_says_only_whether_a_key_is_credential_material() {
    assert_eq!(
        CredentialClassification::secret("kubeconfig"),
        CredentialClassification {
            key: "kubeconfig".to_owned(),
            is_secret: true,
        }
    );
    assert_eq!(
        CredentialClassification::config("vpadm_namespace"),
        CredentialClassification {
            key: "vpadm_namespace".to_owned(),
            is_secret: false,
        }
    );
}

/// The observation channel Task 10's `prepare_run_access` reads its
/// `E2E_VHP_BASE_URL` / `E2E_K8S_NAMESPACE` values from. The projection is
/// the platform's own `project_roles`, so a run variable and the
/// `observed_base_url` column can never disagree about which attribute a
/// role means.
#[test]
fn a_role_claimed_by_the_observed_schema_projects_out_of_the_handle() {
    let schema = vec![
        role_field("endpoint", FieldRole::BaseUrl),
        role_field("namespace", FieldRole::Namespace),
    ];
    let mut attrs = ObservedAttrs::default();
    attrs.set("endpoint", "https://vhp.example");
    attrs.set("namespace", "virtuozzo");
    let slots = Vec::new();
    let config = serde_json::Value::Null;
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: Some(&attrs),
    };

    assert_eq!(
        env.observed_role(&schema, FieldRole::BaseUrl).as_deref(),
        Some("https://vhp.example")
    );
    assert_eq!(
        env.observed_role(&schema, FieldRole::Namespace).as_deref(),
        Some("virtuozzo")
    );
}

/// The shape `Option` exists for: dispatch may reach an environment nothing
/// has observed yet. A plugin must read `None` here and still prepare access.
#[test]
fn a_never_observed_environment_projects_nothing() {
    let schema = vec![role_field("endpoint", FieldRole::BaseUrl)];
    let slots = Vec::new();
    let config = serde_json::Value::Null;
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    assert!(env.observed_role(&schema, FieldRole::BaseUrl).is_none());
}

/// Two ways a role yields nothing from an environment that *was* observed: no
/// declared field claims it, and the claiming field's attribute is blank.
/// Blank projects to `None`, never `Some("")` — an empty `APP_VERSION`
/// reaching every test is what that rule guards.
#[test]
fn an_unclaimed_role_and_a_blank_attribute_both_project_to_none() {
    let schema = vec![
        role_field("endpoint", FieldRole::BaseUrl),
        role_field("version", FieldRole::Version),
    ];
    let mut attrs = ObservedAttrs::default();
    attrs.set("version", "   ");
    let slots = Vec::new();
    let config = serde_json::Value::Null;
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: Some(&attrs),
    };

    assert!(
        env.observed_role(&schema, FieldRole::Namespace).is_none(),
        "no declared field claims Namespace"
    );
    assert!(
        env.observed_role(&schema, FieldRole::BaseUrl).is_none(),
        "the claiming field has no attribute"
    );
    assert!(
        env.observed_role(&schema, FieldRole::Version).is_none(),
        "the claiming field's attribute is blank"
    );
}
