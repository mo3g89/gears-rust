//! One observation, ready to persist — the plugin's own outcome plus the two
//! platform-side transforms every write must have applied to it.
//!
//! # Why this type exists rather than passing `PluginObservation` around
//!
//! `qa_product_sdk::observation::retain_declared` **must** run before an
//! observation is persisted. Its own doc says so ("The gear must call this
//! before persisting an observation"), and the reason is measured: schema
//! validation constrains what a plugin *declares*, not what it puts in the map
//! it returns, so a plugin with a spotless `observed_schema()` can still
//! `attrs.set("kubeconfig_echo", <the real kubeconfig>)`. That map is
//! persisted to a JSONB column and published on `EnvironmentDto` — the
//! persist → DTO → page chain of the 2026-08-28 leak, walking straight
//! through the door schema validation exists to close.
//!
//! A call the writer has to remember to make is a call that eventually is not
//! made. So [`ObservationWrite::new`] is the **only** constructor, the fields
//! are private, and it applies `retain_declared` and `project_roles` itself.
//! There is no way to hand the repository an observation that skipped either.
//! This is the same move `qa_product_sdk::plugin::RegisteredPlugin::new` made
//! for schema validation, for the same reason and after the same finding.

use qa_product_sdk::descriptor::FieldDesc;
use qa_product_sdk::observation::{
    HealthOutcome, ObservationOutcome, ObservedAttrs, PluginObservation, RoleProjection,
    project_roles, retain_declared,
};

/// An observation with both transforms applied, as
/// [`crate::domain::repos::EnvironmentsRepository::record_observation`] takes
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationWrite {
    outcome: PluginObservation,
    attrs: ObservedAttrs,
    roles: RoleProjection,
}

impl ObservationWrite {
    /// Apply the two platform-side transforms to one plugin observation.
    ///
    /// `schema` is the plugin's own `observed_schema()`. Attributes it does
    /// not declare are dropped here and reach no column; the four
    /// role-claimed attributes are lifted out into [`Self::roles`] for the
    /// platform's indexable columns.
    ///
    /// A failed environment half carries no attributes, so it produces an
    /// empty map and an empty projection — **not** the previous cycle's. What
    /// a stale attribute map is worth is a merge rule, and merge rules belong
    /// to the repository, which is the only thing that can see what is
    /// already stored.
    #[must_use]
    pub fn new(schema: &[FieldDesc], outcome: PluginObservation) -> Self {
        let attrs = match &outcome.environment {
            ObservationOutcome::Detected(attrs) => retain_declared(schema, attrs.clone()),
            ObservationOutcome::Failed(_) => ObservedAttrs::default(),
        };
        // Over the *retained* map, deliberately: projecting first and
        // retaining second would let an undeclared key claim a role through a
        // schema entry that shares its name.
        let roles = project_roles(schema, &attrs);
        Self {
            outcome,
            attrs,
            roles,
        }
    }

    /// The declared attributes, and only those.
    #[must_use]
    pub const fn attrs(&self) -> &ObservedAttrs {
        &self.attrs
    }

    /// The four role projections: `observed_version`, `observed_build`,
    /// `observed_base_url`/`vhp_base_url`, `observed_namespace`.
    #[must_use]
    pub const fn roles(&self) -> &RoleProjection {
        &self.roles
    }

    /// The environment half of the plugin's outcome, untransformed.
    #[must_use]
    pub const fn environment(&self) -> &ObservationOutcome {
        &self.outcome.environment
    }

    /// The health half of the plugin's outcome, untransformed.
    #[must_use]
    pub const fn health(&self) -> &HealthOutcome {
        &self.outcome.health
    }
}

// `legacy_cluster_status` was deleted at branch close.
//
// It recovered legacy's four-value `cluster_status` spelling from a plugin's
// coarse `HealthState` plus its classified detail -- the one rule that made an
// empty cluster store `Warning` rather than the `Degraded` it coarsens to.
// `m20260903_000012` dropped that column, so nothing had called it since Task
// 19 except its own tests, and it was the last thing keeping
// `domain::observation` reachable (whole-branch review, I-2).

#[cfg(test)]
#[path = "observation_write_tests.rs"]
mod observation_write_tests;
