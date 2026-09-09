//! What a run needs to reach a VHI environment: two credential mounts, eight
//! run variables, and the runner shape.
//!
//! # This function never reads a credential's bytes
//!
//! It names both credentials by [`EnvironmentHandle::credstore_ref`] and lets
//! the executor resolve them. [`EnvironmentHandle::resolved`] is not called
//! and must not be: dispatch calls this without resolving anything, precisely
//! so no plaintext is materialised in the dispatching process -- the same
//! invariant `qa-vhp-product-plugin`'s own `run.rs` states of its kubeconfig.
//!
//! # The vinfra password travels as a file path, never as a value
//!
//! A [`RunVar`] is a plain `String` assembled in *this* process. Putting the
//! password in one would materialise plaintext here, which is exactly what
//! naming the credential by `credstore_ref` was supposed to avoid. So there
//! is no `VINFRA_PASSWORD` variable at all -- only [`VINFRA_PASSWORD_FILE_VAR`],
//! naming where the executor mounts it.
//!
//! # The two mounts must not share a parent directory
//!
//! A secret volume mounts the *directory* a secret file lives in, so two
//! mounts under one parent would render two `volumeMounts` at one
//! `mountPath`, which the Argo adapter's `reject_unrenderable_mounts` refuses
//! (`qa-runs/src/infra/executor/argo/workflow.rs:174-186`). [`SSH_KEY_PATH`]
//! and [`VINFRA_PASSWORD_PATH`] therefore live under different parents. Each
//! path is also written exactly once and used at both sites -- the mount and
//! the variable naming it -- so the pair cannot drift into a variable
//! pointing at nothing.
//!
//! # A never-observed environment is a normal shape
//!
//! [`EnvironmentHandle::observed`] is `Option`, and `None` yields `Ok` with
//! [`BASE_URL_VAR`] simply omitted -- not defaulted, not blank. Dispatch can
//! legitimately produce that shape, so [`prepare_run_access`] must not error
//! on it.
//!
//! # Where values come from
//!
//! `node_host`, `ssh_user`, `ssh_port` and `vinfra_username` come from
//! `env.config`, through the same blank-is-unset rule
//! [`crate::observe`] reads its own configuration with -- [`config_str`] and
//! [`config_port`], promoted to [`crate::config`] rather than copied, so this
//! module and `observe` can never disagree about what counts as "the operator
//! set this". `BASE_URL_VAR` comes from
//! [`EnvironmentHandle::observed_role`] against [`crate::schemas::observed_schema`].

use qa_product_sdk::access::{MountSpec, RunAccess, RunVar, RunVarContract, RunnerSpec};
use qa_product_sdk::descriptor::FieldRole;
use qa_product_sdk::observation::{FailureClass, PluginFailure};
use qa_product_sdk::plugin::EnvironmentHandle;

use crate::config::{config_port, config_str};
use crate::schemas::{
    DEFAULT_SSH_USER, DEFAULT_VINFRA_USERNAME, NODE_HOST_KEY, SSH_PRIVATE_KEY_KEY, SSH_USER_KEY,
    VINFRA_PASSWORD_KEY, VINFRA_USERNAME_KEY, observed_schema,
};

pub const SSH_HOST_VAR: &str = "VHI_SSH_HOST";
pub const SSH_PORT_VAR: &str = "VHI_SSH_PORT";
pub const SSH_USER_VAR: &str = "VHI_SSH_USER";
pub const SSH_KEY_FILE_VAR: &str = "VHI_SSH_KEY_FILE";
pub const VINFRA_PORTAL_VAR: &str = "VINFRA_PORTAL";
pub const VINFRA_USERNAME_VAR: &str = "VINFRA_USERNAME";
pub const VINFRA_PASSWORD_FILE_VAR: &str = "VINFRA_PASSWORD_FILE";
pub const BASE_URL_VAR: &str = "E2E_VHI_BASE_URL";

/// Where the executor mounts the SSH key, **and** the value of
/// [`SSH_KEY_FILE_VAR`]. One constant used at both sites, because the two
/// must be the same string or the run gets a variable pointing at nothing.
pub const SSH_KEY_PATH: &str = "/etc/qa/vhi-ssh/id";
/// Where the executor mounts the vinfra password, **and** the value of
/// [`VINFRA_PASSWORD_FILE_VAR`].
///
/// A different parent directory from [`SSH_KEY_PATH`] on purpose: a secret
/// volume mounts the *directory*, so two mounts sharing a parent render two
/// `volumeMounts` at one `mountPath`, which the API server rejects
/// (`qa-runs/src/infra/executor/argo/workflow.rs:174-186`).
pub const VINFRA_PASSWORD_PATH: &str = "/etc/qa/vhi-vinfra/password";

/// Owner-read-only. Both files are credentials in the run's filesystem, and
/// nothing in the runner writes to either or reads them as another user.
const MOUNT_MODE: i32 = 0o400;

const NO_SSH_KEY: &str =
    "this environment has no SSH key: add one on the environment's credentials form";
const NO_VINFRA_PASSWORD: &str =
    "this environment has no vinfra password: add one on the environment's credentials form";
const NO_NODE_HOST: &str = "this environment has no management node address: add one on the environment's credentials form";

/// Prepare what a run needs to reach this VHI environment.
///
/// Returns two [`MountSpec::Secret`] mounts -- the SSH key at
/// [`SSH_KEY_PATH`] and the vinfra password at [`VINFRA_PASSWORD_PATH`], both
/// mode `0o400` -- plus up to eight [`RunVar`]s: the seven derived from
/// configuration and the mount paths always, and [`BASE_URL_VAR`] when the
/// environment's last observation supplied a base URL.
///
/// See this module's own doc for the three properties this function exists
/// to hold: it never reads a credential's bytes, the vinfra password travels
/// only as a file path, and the two mounts never share a parent directory.
///
/// # Errors
///
/// [`FailureClass::Internal`] when the environment declares no SSH key, no
/// vinfra password, or no management node address -- there is nothing to
/// mount or nothing to reach, so there is no access to prepare. Each text is
/// fixed and names no submitted value.
pub fn prepare_run_access(env: &EnvironmentHandle<'_>) -> Result<RunAccess, PluginFailure> {
    let ssh_key_ref = env
        .credstore_ref(SSH_PRIVATE_KEY_KEY)
        .ok_or_else(|| PluginFailure::classified(FailureClass::Internal, NO_SSH_KEY))?;
    let vinfra_password_ref = env
        .credstore_ref(VINFRA_PASSWORD_KEY)
        .ok_or_else(|| PluginFailure::classified(FailureClass::Internal, NO_VINFRA_PASSWORD))?;
    let host = config_str(env.config, NODE_HOST_KEY)
        .ok_or_else(|| PluginFailure::classified(FailureClass::Internal, NO_NODE_HOST))?;

    Ok(RunAccess {
        mounts: vec![
            MountSpec::Secret {
                credstore_ref: ssh_key_ref.to_owned(),
                path: SSH_KEY_PATH.to_owned(),
                mode: Some(MOUNT_MODE),
            },
            MountSpec::Secret {
                credstore_ref: vinfra_password_ref.to_owned(),
                path: VINFRA_PASSWORD_PATH.to_owned(),
                mode: Some(MOUNT_MODE),
            },
        ],
        env: run_vars(env, host),
        // VHI's runner authenticates over SSH with the mounted key and to
        // vinfra with the mounted password, not with a pod identity, so
        // there is no account for it to assume.
        service_account: None,
    })
}

/// The run variables: the seven always present, plus [`BASE_URL_VAR`] when
/// the environment has been observed.
///
/// `host` is threaded in from [`prepare_run_access`]'s own successful read
/// rather than re-derived from `env.config` a second time here, so there is
/// no second place that could disagree with it (or have to re-apply its
/// blank-is-unset rule) if it were ever somehow absent.
fn run_vars(env: &EnvironmentHandle<'_>, host: &str) -> Vec<RunVar> {
    let mut vars = vec![
        var(SSH_HOST_VAR, host.to_owned()),
        var(SSH_PORT_VAR, config_port(env.config).to_string()),
        var(
            SSH_USER_VAR,
            config_str(env.config, SSH_USER_KEY)
                .unwrap_or(DEFAULT_SSH_USER)
                .to_owned(),
        ),
        var(SSH_KEY_FILE_VAR, SSH_KEY_PATH.to_owned()),
        // Same value as SSH_HOST_VAR, deliberately -- `node_host` is one
        // field used twice (see `crate::schemas::NODE_HOST_KEY`'s own doc),
        // and this is that field reaching the run under its second name.
        var(VINFRA_PORTAL_VAR, host.to_owned()),
        var(
            VINFRA_USERNAME_VAR,
            config_str(env.config, VINFRA_USERNAME_KEY)
                .unwrap_or(DEFAULT_VINFRA_USERNAME)
                .to_owned(),
        ),
        var(VINFRA_PASSWORD_FILE_VAR, VINFRA_PASSWORD_PATH.to_owned()),
    ];

    if let Some(base_url) = env.observed_role(&observed_schema(), FieldRole::BaseUrl) {
        vars.push(var(BASE_URL_VAR, base_url));
    }

    vars
}

/// One `name=value` entry. `RunVar`'s two fields have the same type, so every
/// construction goes through here rather than being written out inline at
/// several sites where a transposition would compile.
fn var(name: &str, value: String) -> RunVar {
    RunVar {
        name: name.to_owned(),
        value,
    }
}

/// VHI inherits the deployment-wide runner image.
///
/// `RunnerSpec::default()` -- every field `None`/empty -- which
/// [`RunnerSpec::image`]'s own doc defines as "inherit
/// `qa-runs.argo.runner_image`". VHI's runner drives `ssh`/`vinfra` from a
/// generic image the same way VHP's drives `kubectl`, so declaring one here
/// would be a behaviour change, not a port.
#[must_use]
pub fn runner() -> RunnerSpec {
    RunnerSpec::default()
}

/// The names VHI reserves on top of the platform's transport-critical floor:
/// the four facts about this environment's *target* -- the SSH host, the
/// mounted SSH key, the vinfra portal, and the mounted vinfra password.
///
/// # The four names left unreserved, for two different reasons
///
/// [`BASE_URL_VAR`] is parity with VHP's `E2E_VHP_BASE_URL`, which decision D3
/// deliberately leaves overridable by a run parameter -- reserving it here
/// would close that exposure quietly, from this plugin's side, rather than in
/// the PRD amendment `qa-runs::domain::params::RESERVED_NAMES`' SECURITY NOTE
/// says it belongs in.
///
/// [`SSH_PORT_VAR`], [`SSH_USER_VAR`] and [`VINFRA_USERNAME_VAR`] are a
/// different argument entirely: each is an operator-tunable knob an
/// environment's credential form already lets an operator set (or leave at
/// its default), not a fact *about* the target the way the four reserved
/// names are. A run parameter overriding one of these overrides a value the
/// operator could equally have typed into that form, so there is no
/// escalation in leaving them open the way there would be for, say,
/// [`SSH_KEY_FILE_VAR`].
///
/// # This can only ever add
///
/// [`RunVarContract::union_with_floor`] unions; it cannot be handed a set
/// that shrinks the floor.
#[must_use]
pub fn env_contract() -> RunVarContract {
    RunVarContract {
        reserved: [
            SSH_HOST_VAR,
            SSH_KEY_FILE_VAR,
            VINFRA_PORTAL_VAR,
            VINFRA_PASSWORD_FILE_VAR,
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect(),
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod run_tests;
