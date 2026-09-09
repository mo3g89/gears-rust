//! How a run reaches its target environment: the mounts, environment
//! variables, and runner shape a plugin hands back to `qa-runs` (spec §5.2,
//! §7).
//!
//! `qa-runs`' `KubeconfigMount` generalises into [`RunAccess`] here — one
//! plugin-shaped seam instead of an inline kubeconfig block, with no other
//! change to dispatch's shape.

use std::collections::BTreeSet;
use std::fmt;

use credstore_sdk::SecretValue;

/// How a plugin wires one run into its target environment: the mounts
/// `qa-runs` must create, the extra environment variables to inject, and the
/// service account (if any) the run's pod should assume.
///
/// Deliberately **not** `Clone`: a clone of a `RunAccess` holding a
/// [`MountSpec::ConfigValue`] doubles the number of live plaintext copies of
/// that value, and nothing in the platform needs one. Task 17, which teaches
/// the Argo executor to render this, may restore `Clone` if it turns out to
/// need it — with a reason.
///
/// **Task 17 did not need `Clone`** — `qa-runs`' mock executor records
/// submissions behind an `Arc` instead — but it did need `Default`, which is
/// now derived. `RunAccess::default()` is "no access": nothing to mount, no
/// variables, no service account. That is a real state, not an absence — a run
/// with no target environment has it, and the source system mounts nothing in
/// exactly that case (`manager/src/services/argo.rs:504`) — so it is spelled
/// once here rather than re-assembled field by field at every construction
/// site that means it.
#[derive(Debug, Default)]
pub struct RunAccess {
    pub mounts: Vec<MountSpec>,
    pub env: Vec<RunVar>,
    pub service_account: Option<String>,
}

/// Where a credential or a resolved value lands inside the run's container.
///
/// `Secret` names something the *gear* already wrote to credstore — only it
/// holds the tenant-scoped `SecurityContext` that write requires. `ConfigValue`
/// carries a value the plugin resolved itself, still wrapped in `SecretValue`
/// for the trip so it stays redacted at every log point in between, the same
/// way `credstore_sdk::SecretValue` protects a value already at rest.
///
/// Not `Clone`, and not by accident: `SecretValue` itself refuses `Clone`
/// ("to prevent accidental serialization of secret data", its own doc says),
/// and cloning a `ConfigValue` through `as_bytes`/`new` would put a second
/// live copy of a plaintext credential in memory for no consumer's benefit.
pub enum MountSpec {
    Secret {
        credstore_ref: String,
        path: String,
        mode: Option<i32>,
    },
    ConfigValue {
        value: SecretValue,
        path: String,
        mode: Option<i32>,
    },
}

// Hand-written rather than derived: a derived `Debug` formats each field with
// its own `Debug` impl over an unwrapped copy of the enum's data, and while
// `SecretValue::fmt` itself already redacts, this is the enforcement point
// named in the task brief — the redaction must survive here by construction,
// not by relying on a property of a type this module doesn't own.
impl fmt::Debug for MountSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Secret {
                credstore_ref,
                path,
                mode,
            } => f
                .debug_struct("Secret")
                .field("credstore_ref", credstore_ref)
                .field("path", path)
                .field("mode", mode)
                .finish(),
            Self::ConfigValue { path, mode, .. } => f
                .debug_struct("ConfigValue")
                .field("path", path)
                .field("value", &"<redacted>")
                .field("mode", mode)
                .finish(),
        }
    }
}

/// One environment variable a plugin supplies to a run (`RunAccess::env`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunVar {
    pub name: String,
    pub value: String,
}

/// The runner shape for one product (**D11**: `RunnerSpec` varies per
/// *product*, not per environment — a per-environment image would make a
/// run's provenance unclear).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunnerSpec {
    /// `None` means "inherit the deployment-wide `qa-runs.argo.runner_image`".
    /// `qa-runs.argo.runner_image` survives as a deployment-wide default a
    /// plugin may inherit, so a plugin indifferent to its image declares
    /// nothing.
    pub image: Option<String>,
    pub command: Vec<String>,
    pub image_pull_policy: Option<String>,
}

/// A plugin's run-variable declarations: the names — beyond the platform's
/// own floor — that a run parameter may not shadow.
///
/// There is no `names` list of "names the plugin owns". Task 18 takes a
/// plugin's variables from what [`RunAccess::env`] actually returns, so a
/// parallel declaration would be a second source of truth that nothing reads
/// and nothing keeps honest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunVarContract {
    /// Names a run parameter may not override, on top of the platform floor.
    pub reserved: BTreeSet<String>,
}

impl RunVarContract {
    /// The full set of names a run parameter may not override: the
    /// platform's floor, whatever the platform passes as `floor`, **unioned**
    /// with whatever this plugin additionally reserves (**D8**).
    ///
    /// # What the floor actually contains, and what it does not
    ///
    /// Corrected at the Phase E review (finding I-2). This doc used to
    /// enumerate the floor as spec §7 does — "`TEST_FILES`,
    /// `TEST_BUNDLE_URL`, `TEST_VERSION`, `COLLECT_ONLY`, and the collect and
    /// progress URLs" — and that list is **not** what `qa-runs` passes.
    /// `qa_runs::domain::params::RESERVED_NAMES` is the source system's
    /// eleven names verbatim, and three of the six §7 names are absent from
    /// it: `COLLECT_ONLY`, `VHP_COLLECT_URL` and `VHP_PROGRESS_URL`.
    ///
    /// That absence is an **inherited parity exposure, not a regression**: the
    /// source system's own reserved list omits them too, so a run parameter
    /// named `COLLECT_ONLY` overrides the collect flag there as well.
    /// `params::RESERVED_NAMES`' SECURITY NOTE names this class of exposure
    /// and says closing it is a deliberate divergence belonging in a PRD
    /// amendment rather than a quiet edit. So this function documents the
    /// mechanism and leaves the membership to its caller — which is the honest
    /// shape anyway, since `floor` is a parameter.
    ///
    /// This can never return `self.reserved` alone. The platform owns
    /// precedence; the plugin owns names and values — a plugin that could
    /// shrink the platform's floor could let a run parameter overwrite the
    /// bundle URL.
    ///
    /// Both sides are compared case-insensitively via uppercasing, matching
    /// `qa-runs`'s existing `params::RESERVED_NAMES` behaviour: reserving
    /// `my_var` refuses `MY_VAR` too.
    #[must_use]
    pub fn union_with_floor(&self, floor: &[&str]) -> BTreeSet<String> {
        let mut union: BTreeSet<String> =
            floor.iter().map(|name| name.to_ascii_uppercase()).collect();
        union.extend(self.reserved.iter().map(|name| name.to_ascii_uppercase()));
        union
    }
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod access_tests;
