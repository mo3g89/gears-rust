//! What a run needs to reach a VHP environment: the kubeconfig mount, the
//! four run variables named below, and the runner shape.
//!
//! # Where the code came from
//!
//! The four variable names, their values and their derivation rules are
//! `qa-runs`' — `qa-runs/qa-runs/src/domain/runvars.rs`, itself a port of
//! `manager/src/services/argo.rs:436-521`. Nothing here changes a spelling or
//! a rule; what changes is *who owns them*. A name only VHP's runner reads
//! belongs to VHP's plugin, and the platform keeps the precedence ladder that
//! orders them.
//!
//! **The `qa-runs` copies stay in place until Task 18**, which is the task
//! that deletes one of the two. Phase C is additive by definition — revertible
//! by deleting two directories and three lines — so removing them now would
//! break `qa-runs` for the whole of Phases C, D and E. `runvars.rs` carries a
//! comment naming this module as its duplicate and Task 18 as the expiry,
//! because a duplicated *string* is exactly the coupling a reader cannot see
//! from either side.
//!
//! # The division of labour with `qa-runs`, restated because it is easy to get wrong
//!
//! This module supplies **names and values**. It does not order them. The
//! precedence ladder — static variables → pipeline variables → environment
//! variables → the base-URL pair → run parameters → `KUBECONFIG` last of all —
//! stays in `qa-runs::domain::runvars`, with its own tests, and a plugin
//! cannot reach it. [`env_contract`] likewise adds to the platform's reserved
//! floor and can never replace it: see
//! [`RunVarContract::union_with_floor`], which unions unconditionally.
//!
//! # Where the values come from, which is neither the credential nor the config
//!
//! Three of the four are *observed* facts, read back off the environment's
//! last successful observation through
//! [`EnvironmentHandle::observed_role`] — the same projection that writes
//! `qa_environments.observed_base_url` and friends, so a run variable and the
//! column on the environment page can never disagree about which attribute a
//! role means.
//!
//! That makes "never observed" a normal shape rather than an error.
//! [`prepare_run_access`] returns `Ok` for it, with the three observed
//! variables simply **omitted** — not defaulted and not blank. Dispatch can
//! legitimately produce that shape, and so does the leak-conformance harness,
//! which drives this function with `observed: None`.
//!
//! The fourth, `KUBECONFIG`, is a control-plane constant: the path the
//! executor will mount the kubeconfig at. It is not derived from the
//! kubeconfig's *bytes*, which this function never sees — see
//! [`prepare_run_access`]'s own note on that.

use qa_product_sdk::access::{MountSpec, RunAccess, RunVar, RunVarContract, RunnerSpec};
use qa_product_sdk::descriptor::FieldRole;
use qa_product_sdk::observation::{FailureClass, PluginFailure};
use qa_product_sdk::plugin::EnvironmentHandle;

use crate::schemas::{KUBECONFIG_KEY, observed_schema};

/// Run variable carrying the environment's base URL.
///
/// Frozen spelling: existing test repositories read it by this name. Copied
/// from `qa-runs::domain::runvars::PLATFORM_BASE_URL_VAR`, which keeps its own
/// until Task 18.
///
/// **Deliberately not reserved** (parity decision D3, recorded in
/// `qa-runs::domain::params::RESERVED_NAMES`' SECURITY NOTE): a run parameter
/// of this name really does redirect the run's base URL, and that exposure is
/// carried forward on purpose rather than closed in a quiet edit here. See
/// [`env_contract`].
pub const PLATFORM_BASE_URL_VAR: &str = "E2E_VHP_BASE_URL";

/// Run variable carrying the environment's bare host, derived from
/// [`PLATFORM_BASE_URL_VAR`]. Frozen spelling; also not reserved, because it
/// shares that variable's position and precedence exactly.
pub const BASE_DOMAIN_VAR: &str = "VPADM_BASE_DOMAIN";

/// Run variable carrying the Kubernetes namespace the install was observed in
/// — what the runner's Keycloak and credstore auto-discovery reads. Frozen
/// spelling.
pub const NAMESPACE_VAR: &str = "E2E_K8S_NAMESPACE";

/// Run variable naming the mounted kubeconfig. Frozen spelling.
pub const KUBECONFIG_VAR: &str = "KUBECONFIG";

/// Where the executor mounts the kubeconfig, **and** the value of
/// [`KUBECONFIG_VAR`].
///
/// One constant, used at both sites, because the two must be the same string
/// or the run gets a `KUBECONFIG` pointing at nothing. `qa-runs`'
/// `KubeconfigMount` called that "an obligation the port cannot enforce" —
/// true there, where the mount and the variable were built by different
/// functions from different inputs. Here they are built by one function from
/// this constant, so the obligation is discharged by construction, and
/// `the_mount_path_and_the_kubeconfig_variable_are_one_string` asserts it
/// anyway: the enforcement is that the literal is written once, and a test is
/// what notices if someone writes it twice.
///
/// # Corrected at Task 18: this was `/etc/qa/kubeconfig`
///
/// The plan's Task 10 Step 3 prescribed that path and Task 10 implemented it,
/// but `qa-runs` has always mounted the kubeconfig at `/.kube/kubeconfig` —
/// the source system's own pair, a `/.kube` secret volume and a `KUBECONFIG`
/// naming the file inside it (`manager/src/services/argo.rs:514`, `:519`).
/// Task 16's golden `RunSpec` fixture caught the divergence the moment
/// dispatch started coming through this function.
///
/// **This is a value the runner sees**, not an implementation detail: a test
/// image or repository that reads `/.kube/kubeconfig` directly would break, and
/// the plan's own gate makes byte-identity of that fixture the condition for
/// the schema migration that follows. So the platform's shipped spelling wins,
/// and the plan's is the one that changed. Moving it later is one constant and
/// this comment.
pub const KUBECONFIG_PATH: &str = "/.kube/kubeconfig";

/// Owner-read-only. A kubeconfig is a credential in the run's filesystem, and
/// nothing in the runner writes to it or reads it as another user.
const KUBECONFIG_MODE: i32 = 0o400;

/// Fixed rejection for an environment that declares no kubeconfig at all.
///
/// [`FailureClass::Internal`] rather than [`FailureClass::NotFound`]: nothing
/// was reached and nothing was read. This is the environment's own credential
/// configuration being unusable, which is the meaning that variant's doc was
/// widened to carry on 2026-09-04.
const NO_KUBECONFIG: &str =
    "this environment has no kubeconfig: add one on the environment's credentials form";

/// Prepare what a run needs to reach this VHP environment.
///
/// Returns the kubeconfig as a [`MountSpec::Secret`] at [`KUBECONFIG_PATH`]
/// with mode `0o400`, plus up to four [`RunVar`]s: `KUBECONFIG` always, and
/// `E2E_K8S_NAMESPACE`, `E2E_VHP_BASE_URL` and `VPADM_BASE_DOMAIN` when the
/// environment's last observation supplied them.
///
/// # This function never reads a credential's bytes
///
/// It names the kubeconfig by [`EnvironmentHandle::credstore_ref`] and lets
/// the executor resolve it. [`EnvironmentHandle::resolved`] is not called and
/// must not be: dispatch calls this without resolving anything, precisely so
/// no plaintext kubeconfig is materialised in the dispatching process.
///
/// That has a consequence worth stating, because it looks like a missing
/// feature: **the mount is always `Secret`, never `ConfigValue`, even on the
/// call where plaintext happens to be available.** Varying the mount *variant*
/// by whether the bytes are visible is the one divergence an author could
/// believe is helpful, and `assert_no_leak` compares the two drives' output
/// shape precisely to catch it — a plugin whose output moves when plaintext
/// appears has read the plaintext.
///
/// # A never-observed environment is a normal shape
///
/// [`EnvironmentHandle::observed`] is `Option`, and `None` yields `Ok` with
/// the three observed variables omitted. Not defaulted, not blank, not an
/// error. `assert_no_leak` drives exactly this shape, and a plugin that errors
/// on it fails its own crate's leak gate.
///
/// # Errors
///
/// [`FailureClass::Internal`] when the environment declares no `kubeconfig`
/// credential — there is no reference to mount, so there is no access to
/// prepare. The text is fixed and names no submitted value.
pub fn prepare_run_access(env: &EnvironmentHandle<'_>) -> Result<RunAccess, PluginFailure> {
    let credstore_ref = env
        .credstore_ref(KUBECONFIG_KEY)
        .ok_or_else(|| PluginFailure::classified(FailureClass::Internal, NO_KUBECONFIG))?;

    Ok(RunAccess {
        mounts: vec![MountSpec::Secret {
            credstore_ref: credstore_ref.to_owned(),
            path: KUBECONFIG_PATH.to_owned(),
            mode: Some(KUBECONFIG_MODE),
        }],
        env: run_vars(env),
        // VHP's runner authenticates to the cluster with the mounted
        // kubeconfig, not with a pod identity, so there is no account for it
        // to assume. `qa-runs` names none today either.
        service_account: None,
    })
}

/// The run variables, in the order `qa-runs`' assembly pushes them.
///
/// The order is not load-bearing — `RunAccess::env` is consumed by name, and
/// `assert_no_leak` compares the two drives as a multiset for exactly that
/// reason — but it costs nothing to keep it recognisable against the function
/// this was ported from.
fn run_vars(env: &EnvironmentHandle<'_>) -> Vec<RunVar> {
    let schema = observed_schema();
    let mut vars = Vec::new();

    // `observed_role` is `project_roles`, which trims and treats a blank as
    // unset — the same guard `qa-runs`' `assemble` applies at this push site
    // (`.filter(|ns| !ns.trim().is_empty())`), applied once in the SDK
    // instead of once per call site here.
    if let Some(namespace) = env.observed_role(&schema, FieldRole::Namespace) {
        vars.push(var(NAMESPACE_VAR, namespace));
    }

    // `baseDomain` holds `https://<domain>` despite its key — it is the
    // successor to the `vhp_base_url` column, which
    // `environments_sea_repo.rs` writes as `format!("https://{domain}")`.
    // `VPADM_BASE_DOMAIN` is the bare host derived back out of it, which is
    // the direction `qa-runs` derives it today.
    if let Some(base_url) = env.observed_role(&schema, FieldRole::BaseUrl) {
        // An unparseable URL suppresses only the *derived* domain. The value
        // an operator's cluster actually reported still reaches the run: a
        // bad URL must not silently blank out `E2E_VHP_BASE_URL`.
        if let Some(domain) = base_domain_from_url(&base_url) {
            vars.push(var(BASE_DOMAIN_VAR, domain));
        }
        vars.push(var(PLATFORM_BASE_URL_VAR, base_url));
    }

    // Last, as `qa-runs` pushes it. No blank guard, because the value is this
    // module's own constant rather than anything read off an environment.
    vars.push(var(KUBECONFIG_VAR, KUBECONFIG_PATH.to_owned()));

    vars
}

/// One `name=value` entry. `RunVar`'s two fields have the same type, so every
/// construction goes through here rather than being written out inline at
/// four sites where a transposition would compile.
fn var(name: &str, value: String) -> RunVar {
    RunVar {
        name: name.to_owned(),
        value,
    }
}

/// Extracts the bare host (no scheme, port, or path) from a base URL, e.g.
/// `"https://sv.jele.io"` -> `Some("sv.jele.io")`.
///
/// Behaviour copied from `qa-runs::domain::runvars::base_domain_from_url`,
/// itself ported from `manager/src/services/argo.rs:2476`. Delegates to
/// `url::Url` rather than hand-rolled string splitting, so IPv6 literals,
/// userinfo (`user:pass@host`) and IDN hosts are handled correctly.
/// `Url::parse` requires a scheme, so a bare host (no `://`) gets a throwaway
/// `https://` prefix purely to make it parseable — the scheme itself is
/// discarded, only `host_str()` is read.
///
/// Returns `None` for blank input and for input `Url::parse` cannot make sense
/// of (e.g. `"::::"`). The caller does not propagate that as a failure; see
/// [`run_vars`].
fn base_domain_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    url::Url::parse(&with_scheme)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
}

/// VHP inherits the deployment-wide runner image.
///
/// `RunnerSpec::default()` — every field `None`/empty — which
/// [`RunnerSpec::image`]'s own doc defines as "inherit
/// `qa-runs.argo.runner_image`". That is what runs VHP's tests today, so
/// declaring an image here would be a behaviour change, not a port.
///
/// The method exists because **D11** says a runner shape varies per *product*,
/// and the products that follow VHP will need one. Indifference is expressible
/// and this is what it looks like.
#[must_use]
pub fn runner() -> RunnerSpec {
    RunnerSpec::default()
}

/// The names VHP reserves on top of the platform's transport-critical floor.
///
/// # Which names, and why exactly these two
///
/// Parity with `qa-runs::domain::params::RESERVED_NAMES` as it stands today.
/// Of its twelve names, ten are the platform's own (`TEST_FILES`,
/// `TEST_BUNDLE_URL`, `TEST_VERSION`, `APP_VERSION`, `APP_BUILD`,
/// `PRODUCT_KEY`, `RP_PROJECT`, `RP_API_KEY`, `SKIP_TESTS_WITH_BUGS`,
/// `QA_RUNNER_PYTEST_ARGS`) and stay with the platform. The two that are facts
/// about *this product's* target — its Kubernetes namespace and its mounted
/// kubeconfig — come here.
///
/// # The two that are conspicuously absent
///
/// Neither [`PLATFORM_BASE_URL_VAR`] nor [`BASE_DOMAIN_VAR`] is reserved, and
/// that is the ported behaviour rather than an omission: today's
/// `RESERVED_NAMES` does not list either, so a run parameter of either name
/// overrides the value this plugin supplies. Decision D3 carried that forward
/// deliberately, and `runvars.rs`' SECURITY NOTE says closing it belongs in a
/// PRD amendment rather than a quiet edit. Reserving them here would close it
/// quietly, from the other side.
///
/// # This can only ever add
///
/// [`RunVarContract::union_with_floor`] unions; it cannot be handed a set that
/// shrinks the floor. A plugin that could shrink it could let a run parameter
/// overwrite the bundle URL.
#[must_use]
pub fn env_contract() -> RunVarContract {
    RunVarContract {
        reserved: [NAMESPACE_VAR, KUBECONFIG_VAR]
            .into_iter()
            .map(ToOwned::to_owned)
            .collect(),
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod run_tests;
