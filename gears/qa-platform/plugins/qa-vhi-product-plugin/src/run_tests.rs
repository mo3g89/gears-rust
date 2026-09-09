//! The run-facing half: two mounts by reference, eight run variables, and the
//! four reserved names.
//!
//! # Fixtures, modelled on VHP's `run_tests.rs` rather than copied from it
//!
//! `handle()`, `handle_without_optional_config()` and
//! `handle_never_observed()` build an [`EnvironmentHandle`] out of
//! [`CredentialSlot::reference_only`] entries -- the shape dispatch actually
//! passes, never a resolved secret -- and a configuration object. Each owns
//! its data through a leaked `Box`, because these functions hand back the
//! handle itself (not just the `RunAccess` it produces), and an
//! `EnvironmentHandle` borrows; there is no shorter-lived owner in a fixture
//! function for it to borrow from.
//!
//! VHP's own file has no such fixtures -- it has one `access_for(observed)`
//! helper, because VHP reads no configuration at all. (This header claimed
//! they were "copied from VHP's `run_tests.rs`" until the 2026-09-09
//! whole-branch review; they never were.) What *is* taken from VHP, and is
//! the reason this file was rewritten in that review, is the shape of its
//! assertions: `value_of` returning the whole value for an `assert_eq!`
//! rather than a `contains`, `only_mount`-style tuple pins that cover the
//! reference, the path and the mode together, and a whole-set assertion that
//! no individual pin can substitute for.
//!
//! # What the four weakest pins were, and what replaced them
//!
//! Before that review: nothing tied a credstore reference to the path it is
//! mounted at (swapping the two `MountSpec::Secret` arms in `run.rs` left the
//! whole suite green, while the run got the private key at the password's
//! path); nothing read back a *configured* `ssh_user`/`ssh_port`/
//! `vinfra_username`, so an implementation ignoring `env.config` entirely
//! passed; `the_portal_and_the_ssh_host_are_the_same_value` compared two
//! `Option`s and was satisfied by `None == None`; and `E2E_VHI_BASE_URL` was
//! asserted only by its *absence*. Each is now pinned by name below.

use qa_product_sdk::access::{MountSpec, RunnerSpec};
use qa_product_sdk::observation::ObservedAttrs;
use qa_product_sdk::plugin::CredentialSlot;

use super::*;
use crate::schemas::{
    BASE_URL_KEY, NODE_HOST_KEY, SSH_PORT_KEY, SSH_PRIVATE_KEY_KEY, SSH_USER_KEY,
    VINFRA_PASSWORD_KEY, VINFRA_USERNAME_KEY,
};

/// The credential slots a dispatching process actually holds: references
/// only, never a resolved secret.
fn slots() -> Vec<CredentialSlot> {
    vec![
        CredentialSlot::reference_only(SSH_PRIVATE_KEY_KEY, "qa/vhi/ssh-key"),
        CredentialSlot::reference_only(VINFRA_PASSWORD_KEY, "qa/vhi/vinfra-password"),
    ]
}

/// The environment's last observation: just enough for
/// [`FieldRole::BaseUrl`] to project a value.
fn observed_vhi() -> ObservedAttrs {
    let mut attrs = ObservedAttrs::default();
    attrs.set(BASE_URL_KEY, "https://vhi-node.example.com:8888");
    attrs
}

/// A configuration object built from schema-key constants rather than string
/// literals typed a second time -- `serde_json::json!`'s object syntax takes
/// a literal key, not a `const`, so this is the shape dispatch's own JSON
/// takes without retyping any key.
fn config_of(pairs: &[(&str, &str)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
        map.insert(
            (*key).to_owned(),
            serde_json::Value::String((*value).to_owned()),
        );
    }
    serde_json::Value::Object(map)
}

/// A fully configured, previously observed VHI environment: both
/// credentials, a management node address, and every optional override set
/// to something other than its default.
fn handle() -> EnvironmentHandle<'static> {
    let slots: &'static [CredentialSlot] = Box::leak(slots().into_boxed_slice());
    let config: &'static serde_json::Value = Box::leak(Box::new(config_of(&[
        (NODE_HOST_KEY, "vhi-node.example.com"),
        (SSH_USER_KEY, "vhiadmin"),
        (SSH_PORT_KEY, "2222"),
        (VINFRA_USERNAME_KEY, "operator"),
    ])));
    let observed: &'static ObservedAttrs = Box::leak(Box::new(observed_vhi()));
    EnvironmentHandle {
        slots,
        config,
        observed: Some(observed),
    }
}

/// The same environment, but with every optional override left blank -- an
/// operator who accepted every default. Used to pin `root`/`22`/`admin`.
fn handle_without_optional_config() -> EnvironmentHandle<'static> {
    let slots: &'static [CredentialSlot] = Box::leak(slots().into_boxed_slice());
    let config: &'static serde_json::Value = Box::leak(Box::new(config_of(&[(
        NODE_HOST_KEY,
        "vhi-node.example.com",
    )])));
    let observed: &'static ObservedAttrs = Box::leak(Box::new(observed_vhi()));
    EnvironmentHandle {
        slots,
        config,
        observed: Some(observed),
    }
}

/// The same environment, but never observed -- a normal shape, not an error.
fn handle_never_observed() -> EnvironmentHandle<'static> {
    let slots: &'static [CredentialSlot] = Box::leak(slots().into_boxed_slice());
    let config: &'static serde_json::Value = Box::leak(Box::new(config_of(&[(
        NODE_HOST_KEY,
        "vhi-node.example.com",
    )])));
    EnvironmentHandle {
        slots,
        config,
        observed: None,
    }
}

/// The value of `name`, or `None` when the plugin did not supply it.
///
/// Whole-value by construction, as VHP's namesake is: it returns the value
/// for the caller to `assert_eq!` against, and there is no shape of call that
/// makes a substring match. It also refuses a duplicate, which no `find`-based
/// lookup can notice.
fn value_of<'a>(access: &'a RunAccess, name: &str) -> Option<&'a str> {
    let mut found = access
        .env
        .iter()
        .filter(|var| var.name == name)
        .map(|var| var.value.as_str());
    let first = found.next();
    assert!(
        found.next().is_none(),
        "`{name}` was supplied twice, so `value_of` would be reporting one of two values"
    );
    first
}

/// The two mounts as `(credstore_ref, path, mode)` triples, in the order the
/// plugin returned them.
///
/// A triple rather than three separate lookups because the *pairing* is the
/// property: two mounts and two references, each correct in isolation, can
/// still be crossed -- and a crossed pair puts the private key at the
/// password's path, which no per-field assertion notices.
fn mounts_of(access: &RunAccess) -> Vec<(&str, &str, Option<i32>)> {
    access
        .mounts
        .iter()
        .map(|mount| match mount {
            MountSpec::Secret {
                credstore_ref,
                path,
                mode,
            } => (credstore_ref.as_str(), path.as_str(), *mode),
            MountSpec::ConfigValue { .. } => panic!(
                "both credentials must be mounted by reference: a `ConfigValue` carries \
                 plaintext this function is contractually forbidden to have"
            ),
        })
        .collect()
}

#[test]
fn both_secrets_are_mounted_by_reference_never_by_value() {
    let access = prepare_run_access(&handle()).expect("ok");
    assert_eq!(access.mounts.len(), 2);
    assert!(
        access
            .mounts
            .iter()
            .all(|m| matches!(m, MountSpec::Secret { .. })),
        "a ConfigValue mount would be rejected by the Argo adapter and would also mean \
         this function had read a credential's bytes"
    );
}

#[test]
fn the_two_mounts_do_not_share_a_directory() {
    let access = prepare_run_access(&handle()).expect("ok");
    let dirs: Vec<_> = access
        .mounts
        .iter()
        .map(|m| match m {
            MountSpec::Secret { path, .. } => std::path::Path::new(path)
                .parent()
                .expect("has a parent")
                .to_owned(),
            MountSpec::ConfigValue { .. } => unreachable!("asserted above"),
        })
        .collect();
    assert_ne!(
        dirs[0], dirs[1],
        "a secret volume mounts the DIRECTORY, so two mounts under one parent render two \
         volumeMounts at one mountPath and the API server rejects the workflow"
    );
}

/// **Which credential lands at which path, pinned as a whole triple.**
///
/// Reference, path and mode together, per mount, in order. Swapping the two
/// `MountSpec::Secret` arms in `run.rs` used to leave every test in this file
/// green while the run got the SSH private key mounted where the runner looks
/// for the vinfra password, and the password where it looks for the key --
/// two credentials delivered to the wrong readers, with nothing to notice.
///
/// The literals are duplicated rather than derived from [`SSH_KEY_PATH`] and
/// [`VINFRA_PASSWORD_PATH`] on purpose: a pin that reads the constant it is
/// pinning agrees with itself however the constant changes, and these paths
/// are a contract with the pytest suite in `vhi-e2e-tests`, not an internal
/// detail. `MOUNT_MODE` is pinned the same way and for the same reason --
/// both files are credentials in the run's filesystem, and `0o400` is what
/// keeps them owner-read-only.
#[test]
fn each_credential_is_mounted_at_its_own_path_read_only() {
    let access = prepare_run_access(&handle()).expect("ok");

    assert_eq!(
        mounts_of(&access),
        vec![
            ("qa/vhi/ssh-key", "/etc/qa/vhi-ssh/id", Some(0o400)),
            (
                "qa/vhi/vinfra-password",
                "/etc/qa/vhi-vinfra/password",
                Some(0o400)
            ),
        ]
    );
}

#[test]
fn each_mount_path_is_also_the_variable_that_names_it() {
    let access = prepare_run_access(&handle()).expect("ok");
    let var = |name: &str| {
        access
            .env
            .iter()
            .find(|v| v.name == name)
            .map(|v| v.value.clone())
    };
    assert_eq!(var(SSH_KEY_FILE_VAR), Some(SSH_KEY_PATH.to_owned()));
    assert_eq!(
        var(VINFRA_PASSWORD_FILE_VAR),
        Some(VINFRA_PASSWORD_PATH.to_owned())
    );
}

#[test]
fn the_password_is_named_by_path_and_never_by_value() {
    let access = prepare_run_access(&handle()).expect("ok");
    assert!(
        !access.env.iter().any(|v| v.name == "VINFRA_PASSWORD"),
        "a RunVar is a plain String assembled in the dispatching process; putting the \
         password in one would materialise plaintext there"
    );
}

/// `node_host` is one configured field reaching the run under two names.
///
/// Both halves are asserted `Some` and against the configured literal, not
/// merely equal to each other: this test compared two `Option<String>`s until
/// the 2026-09-09 review, so `None == None` satisfied it and an
/// implementation emitting *neither* variable was green.
#[test]
fn the_portal_and_the_ssh_host_are_the_same_configured_value() {
    let access = prepare_run_access(&handle()).expect("ok");

    assert_eq!(
        value_of(&access, SSH_HOST_VAR),
        Some("vhi-node.example.com")
    );
    assert_eq!(
        value_of(&access, VINFRA_PORTAL_VAR),
        Some("vhi-node.example.com")
    );
    assert_eq!(
        value_of(&access, SSH_HOST_VAR),
        value_of(&access, VINFRA_PORTAL_VAR),
        "one field used twice: a run whose portal and ssh host disagree is reaching two hosts"
    );
}

#[test]
fn the_defaults_are_applied_when_the_config_omits_them() {
    let access = prepare_run_access(&handle_without_optional_config()).expect("ok");

    assert_eq!(value_of(&access, SSH_USER_VAR), Some("root"));
    assert_eq!(value_of(&access, SSH_PORT_VAR), Some("22"));
    assert_eq!(value_of(&access, VINFRA_USERNAME_VAR), Some("admin"));
}

/// **The other half of the same rule, and the half nothing asserted.**
///
/// `handle()` sets `ssh_user`, `ssh_port` and `vinfra_username` to values
/// other than their defaults precisely so a configured value can be read back
/// here. Until the 2026-09-09 review the only test on these three was the one
/// above, which asserts the *fallbacks* -- so an implementation that never
/// looked at `env.config` at all passed the suite, and an operator's
/// non-default SSH port would have been silently ignored on every run.
#[test]
fn a_configured_user_port_and_vinfra_username_reach_the_run() {
    let access = prepare_run_access(&handle()).expect("ok");

    assert_eq!(value_of(&access, SSH_USER_VAR), Some("vhiadmin"));
    assert_eq!(value_of(&access, SSH_PORT_VAR), Some("2222"));
    assert_eq!(value_of(&access, VINFRA_USERNAME_VAR), Some("operator"));
}

/// The observed base URL reaches the run under [`BASE_URL_VAR`], verbatim.
///
/// The absence case below was the only one pinned; "present and correct" was
/// not, so an implementation that omitted the variable on an *observed*
/// environment -- or wrote a different attribute into it -- was green.
#[test]
fn an_observed_base_url_reaches_the_run_verbatim() {
    let access = prepare_run_access(&handle()).expect("ok");

    assert_eq!(
        value_of(&access, BASE_URL_VAR),
        Some("https://vhi-node.example.com:8888")
    );
}

#[test]
fn a_never_observed_environment_is_ok_with_the_base_url_omitted() {
    let access =
        prepare_run_access(&handle_never_observed()).expect("never observed is not an error");
    assert_eq!(
        value_of(&access, BASE_URL_VAR),
        None,
        "omitted, not defaulted and not blank"
    );
}

/// **The whole set, pinned as a set: eight names and no ninth.**
///
/// VHP's `a_fully_observed_environment_yields_exactly_the_four_variables`,
/// one product over. Every individual pin above would still pass if the
/// plugin also supplied a variable nobody asked for -- including one carrying
/// something it had read out of `env.config`, or out of a credential. This is
/// the assertion that cannot: it is the only one in this file that fails on
/// an *extra* variable.
#[test]
fn a_fully_observed_environment_yields_exactly_the_eight_variables() {
    let access = prepare_run_access(&handle()).expect("ok");

    let mut supplied: Vec<(&str, &str)> = access
        .env
        .iter()
        .map(|var| (var.name.as_str(), var.value.as_str()))
        .collect();
    supplied.sort_unstable();

    assert_eq!(
        supplied,
        vec![
            ("E2E_VHI_BASE_URL", "https://vhi-node.example.com:8888"),
            ("VHI_SSH_HOST", "vhi-node.example.com"),
            ("VHI_SSH_KEY_FILE", "/etc/qa/vhi-ssh/id"),
            ("VHI_SSH_PORT", "2222"),
            ("VHI_SSH_USER", "vhiadmin"),
            ("VINFRA_PASSWORD_FILE", "/etc/qa/vhi-vinfra/password"),
            ("VINFRA_PORTAL", "vhi-node.example.com"),
            ("VINFRA_USERNAME", "operator"),
        ]
    );
}

#[test]
fn the_contract_reserves_the_four_target_facts() {
    let reserved = env_contract().reserved;
    assert!(reserved.contains(SSH_HOST_VAR));
    assert!(reserved.contains(SSH_KEY_FILE_VAR));
    assert!(reserved.contains(VINFRA_PORTAL_VAR));
    assert!(reserved.contains(VINFRA_PASSWORD_FILE_VAR));
    assert!(
        !reserved.contains(BASE_URL_VAR),
        "parity with VHP's E2E_VHP_BASE_URL, which decision D3 deliberately leaves overridable"
    );
}

#[test]
fn vhi_inherits_the_deployment_wide_runner_image() {
    assert_eq!(runner(), RunnerSpec::default());
}

// ── The three half-filled-form shapes an operator can actually produce ────

/// An environment with no SSH key reference: nothing to mount, so nothing to
/// prepare.
#[test]
fn an_environment_with_no_ssh_key_is_a_classified_failure() {
    let slots = vec![CredentialSlot::reference_only(
        VINFRA_PASSWORD_KEY,
        "qa/vhi/vinfra-password",
    )];
    let config = config_of(&[(NODE_HOST_KEY, "vhi-node.example.com")]);
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let failure = prepare_run_access(&env).expect_err("there is no SSH key to mount");

    assert_eq!(
        failure,
        PluginFailure::classified(
            FailureClass::Internal,
            "this environment has no SSH key: add one on the environment's credentials form"
        )
    );
}

/// An environment with no vinfra password reference.
#[test]
fn an_environment_with_no_vinfra_password_is_a_classified_failure() {
    let slots = vec![CredentialSlot::reference_only(
        SSH_PRIVATE_KEY_KEY,
        "qa/vhi/ssh-key",
    )];
    let config = config_of(&[(NODE_HOST_KEY, "vhi-node.example.com")]);
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let failure = prepare_run_access(&env).expect_err("there is no vinfra password to mount");

    assert_eq!(
        failure,
        PluginFailure::classified(
            FailureClass::Internal,
            "this environment has no vinfra password: add one on the environment's credentials \
             form"
        )
    );
}

/// An environment with both credentials but no configured management node
/// address.
#[test]
fn an_environment_with_no_node_host_is_a_classified_failure() {
    let slots = slots();
    let config = serde_json::Value::Null;
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let failure = prepare_run_access(&env).expect_err("there is no host to reach");

    assert_eq!(
        failure,
        PluginFailure::classified(
            FailureClass::Internal,
            "this environment has no management node address: add one on the environment's \
             credentials form"
        )
    );
}
