//! What observing an environment yields, and how a failure crosses the plugin
//! boundary without carrying credential material with it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::descriptor::{FieldDesc, FieldRole};

/// A plugin's observed values, keyed by `FieldDesc::key`.
///
/// `BTreeMap` rather than `HashMap` so the serialised JSONB is stable: a
/// reordered blob is a spurious row update on every observation cycle.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObservedAttrs(BTreeMap<String, String>);

impl ObservedAttrs {
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// The shape of a failure, independent of the bytes that caused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClass {
    /// The target could not be reached at all.
    Unreachable,
    /// The target was reached and refused the credential.
    AuthRejected,
    /// The target was reached but the thing being read is not there.
    NotFound,
    /// Something was read and could not be understood.
    Malformed,
    Timeout,
    /// Something on the plugin's side of the wire — the plugin or its
    /// configuration, not the target.
    ///
    /// Widened from "a defect in the plugin itself" on 2026-09-04, because
    /// that had stopped being true of its live call sites: `qa-connector-k8s`
    /// classifies an unreadable local kubeconfig, an unresolvable config path,
    /// a failed `Config::infer()`, a `409 Conflict` on a hand-made `Secret`,
    /// and every API status outside the four the other variants claim, all as
    /// `Internal`. Not one of those is a plugin bug, and telling an operator
    /// that their exec credential plugin failed because of a defect in ours is
    /// worse than coarse.
    ///
    /// This is the residual bucket, and it is carrying two distinct meanings —
    /// "misconfigured" and "the plugin is broken". Splitting it needs a new
    /// variant, which was deferred to whichever task gave [`FailureClass`] a
    /// wire form, because that is when adding one stops being free.
    ///
    /// **That wire form now exists, and adding a variant is no longer free.**
    /// `qa-environments` labels a Prometheus series by this enum
    /// (`qa-environments/src/domain/ports/metrics.rs`'s `ObservationClass`,
    /// projected one-for-one with no `_` arm), so every variant here is a label
    /// value on an exported metric. The consequences of adding one, in the
    /// order they bite:
    ///
    /// * a new series appears, and a dashboard or alert written against
    ///   `internal` stops seeing the traffic that moved to the new value —
    ///   silently, because a query for a label value that no longer receives
    ///   samples returns a flat line rather than an error;
    /// * every panel and alert expression naming the old value has to be
    ///   revisited, and none of them is in this repository.
    ///
    /// The split is still the right end state — telling an operator that their
    /// exec credential plugin failed because of a defect in ours remains worse
    /// than coarse. It is now a change with a migration attached rather than a
    /// free one, and this note records the status change rather than making the
    /// split: doing it here would be doing it without the dashboards in view.
    Internal,
}

/// A failure crossing the plugin boundary.
///
/// # Why `detail` is `&'static str`
///
/// This is the enforcement point for the "Credential containment" rule in
/// `gears/qa-platform/docs/features/product-plugins.md` (no section
/// numbers in that document; `PRODUCT-PLUGINS-DESIGN.md`, cited here before
/// the docs squash, no longer exists). The rule exists because a measured
/// leak on 2026-08-28 put a PEM private key on the platform page: a serde
/// error quoted the whole offending scalar, and for a document that *is* one
/// scalar the offending scalar is the whole document.
///
/// A `String` here would let a plugin author write
/// `format!("{upstream_error}")` and reproduce that leak from inside a crate
/// this team does not review. `&'static str` cannot be produced from runtime
/// bytes without `Box::leak`, which is greppable and lintable. The rule is
/// therefore *classification, not sanitisation* — the same shape
/// `qa-environments/src/infra/observer/errors.rs` already implements, moved
/// out to where third-party plugins must obey it too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginFailure {
    pub class: FailureClass,
    /// Fixed text chosen by variant. Never derived from any value the plugin
    /// read or was given.
    pub detail: Option<&'static str>,
    /// Text the **remote** sent back — an API server's `Status.message`, an
    /// HTTP error body's `message` field.
    ///
    /// The one sanctioned exception to "nothing formatted crosses this
    /// boundary", and it is deliberate: `kube_observer`'s own header argues
    /// that "namespaces virtuozzo not found" is what makes a broken
    /// environment *fixable* rather than merely broken. A plugin must never
    /// put its own formatting here — only text it received. The
    /// `assert_no_leak` harness (Task 4) is what checks that it didn't.
    pub remote_message: Option<String>,
}

impl PluginFailure {
    #[must_use]
    pub const fn classified(class: FailureClass, detail: &'static str) -> Self {
        Self {
            class,
            detail: Some(detail),
            remote_message: None,
        }
    }

    #[must_use]
    pub const fn bare(class: FailureClass) -> Self {
        Self {
            class,
            detail: None,
            remote_message: None,
        }
    }

    #[must_use]
    pub fn with_remote_message(mut self, message: impl Into<String>) -> Self {
        self.remote_message = Some(message.into());
        self
    }
}

impl std::fmt::Display for PluginFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.detail, &self.remote_message) {
            (Some(d), Some(r)) => write!(f, "{d}: {r}"),
            (Some(d), None) => write!(f, "{d}"),
            (None, Some(r)) => write!(f, "{r}"),
            (None, None) => {
                #[allow(clippy::use_debug)]
                {
                    write!(f, "{:?}", self.class)
                }
            }
        }
    }
}

impl std::error::Error for PluginFailure {}

/// The result of one observation attempt. A failure is a **value**, not an
/// error: the message is persisted and shown, because an operator who can see
/// why it failed can fix it and one who sees a blank page cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationOutcome {
    Detected(ObservedAttrs),
    Failed(PluginFailure),
}

/// Coarse health, in a vocabulary every product can express.
///
/// "Nodes ready" is not health for a `SaaS` tenant or an appliance, so the
/// platform keeps only the verdict and the plugin puts its own facts in
/// `ObservedAttrs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ok,
    Degraded,
    Down,
    /// Nothing is known — distinct from `Down`, which means something looked
    /// and found the target unhealthy.
    #[default]
    Unknown,
}

impl HealthState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Down => "down",
            Self::Unknown => "unknown",
        }
    }

    /// Total, by design: a value written by a newer build must not panic an
    /// older one reading the same column.
    #[must_use]
    pub fn from_str_or_unknown(raw: &str) -> Self {
        match raw {
            "ok" => Self::Ok,
            "degraded" => Self::Degraded,
            "down" => Self::Down,
            _ => Self::Unknown,
        }
    }
}

/// One health read. Kept separate from [`ObservationOutcome`] because the two
/// fail independently: a credential scoped to one namespace can detect a
/// version perfectly and still be forbidden from reading cluster-wide health.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthOutcome {
    Checked {
        state: HealthState,
        detail: Option<&'static str>,
    },
    Failed(PluginFailure),
    /// Nothing was attempted, so nothing is known. The platform writes none of
    /// the health columns for this variant — not even `health_checked_at`.
    NotAttempted,
}

/// Both halves of one `observe` call. One call rather than two methods, so one
/// client and one handshake serve both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginObservation {
    pub environment: ObservationOutcome,
    pub health: HealthOutcome,
}

/// The four role-claimed attributes, lifted out for the platform's columns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleProjection {
    pub version: Option<String>,
    pub build: Option<String>,
    pub base_url: Option<String>,
    pub namespace: Option<String>,
}

/// Copy the role-claimed attributes out of a plugin's opaque map.
///
/// This is decision **D10**: observation is fully plugin-defined, and the
/// platform still gets indexable `observed_version` / `observed_build` /
/// `observed_base_url` columns and a deterministic `APP_VERSION` / `APP_BUILD`
/// for `qa-runs` to snapshot. Blank projects to `None`, never to `Some("")` —
/// an empty `APP_VERSION` reaching every test is the failure this guards.
#[must_use]
pub fn project_roles(schema: &[FieldDesc], attrs: &ObservedAttrs) -> RoleProjection {
    let mut out = RoleProjection::default();
    for field in schema {
        let Some(role) = field.role else { continue };
        let value = attrs
            .get(&field.key)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned);
        match role {
            FieldRole::Version => out.version = value,
            FieldRole::Build => out.build = value,
            FieldRole::BaseUrl => out.base_url = value,
            FieldRole::Namespace => out.namespace = value,
        }
    }
    out
}

/// Drop every attribute the plugin did not declare in its
/// `observed_schema()`.
///
/// # Why this exists
///
/// [`crate::descriptor::validate_schemas`] is layer 2, and it constrains the
/// *declaration*: no `Secret`/`MultilineSecret` kind may appear in
/// `observed_schema()`. It says nothing about the keys a plugin actually puts
/// into the [`ObservedAttrs`] it returns. A plugin with a spotless schema can
/// still `attrs.set("kubeconfig_echo", <the real kubeconfig>)`, and Task 15
/// persists that map to a JSONB column and publishes it on `EnvironmentDto` —
/// the persist → DTO → page chain of the 2026-08-28 leak, walking straight
/// through the door layer 2 exists to close.
///
/// **The gear must call this before persisting an observation** (Task 15
/// does). Declared-and-validated is then the only shape that reaches storage:
/// what is not declared cannot be rendered, because it is no longer there.
///
/// Dropping rather than rejecting is deliberate. An undeclared key is a
/// plugin bug, not an operator's, and failing the whole observation cycle
/// over one stray attribute would lose the version and health facts that were
/// read correctly in the same call.
#[must_use]
pub fn retain_declared(schema: &[FieldDesc], mut attrs: ObservedAttrs) -> ObservedAttrs {
    attrs
        .0
        .retain(|key, _| schema.iter().any(|field| field.key == *key));
    attrs
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod observation_tests;
