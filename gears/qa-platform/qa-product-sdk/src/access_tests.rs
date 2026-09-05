use super::*;

/// **D8** expressed as a test: the platform's transport-critical floor is
/// never replaced by a plugin's own reservations, only added to.
#[test]
fn run_var_contract_reserved_is_a_set_not_a_replacement() {
    let contract = RunVarContract {
        reserved: BTreeSet::from(["MY_VAR".to_owned()]),
    };

    let union = contract.union_with_floor(&["TEST_FILES"]);

    assert!(union.contains("MY_VAR"), "got {union:?}");
    assert!(union.contains("TEST_FILES"), "got {union:?}");
}

/// Matches `params::RESERVED_NAMES`' existing uppercase-key behaviour:
/// reserving `my_var` refuses `MY_VAR` too, because both sides are compared
/// uppercased.
#[test]
fn reserved_names_compare_case_insensitively() {
    let contract = RunVarContract {
        reserved: BTreeSet::from(["my_var".to_owned()]),
    };

    let union = contract.union_with_floor(&["MY_VAR"]);

    assert_eq!(union.len(), 1, "got {union:?}");
    assert!(union.contains("MY_VAR"), "got {union:?}");
}

/// `None` means "use the deployment-wide `qa-runs.argo.runner_image`" — a
/// plugin indifferent to its image declares nothing.
#[test]
fn runner_spec_default_inherits_the_deployment_image() {
    assert_eq!(RunnerSpec::default().image, None);
}

/// `SecretValue` already redacts its own `Debug`; this pins that `MountSpec`
/// does not defeat that redaction by deriving `Debug` over an unwrapped copy.
#[test]
fn mount_spec_debug_does_not_render_a_config_value() {
    let mount = MountSpec::ConfigValue {
        value: SecretValue::from("PRIVATE"),
        path: "/etc/plugin/config".to_owned(),
        mode: None,
    };

    let rendered = format!("{mount:?}");

    assert!(!rendered.contains("PRIVATE"), "got {rendered}");
}
