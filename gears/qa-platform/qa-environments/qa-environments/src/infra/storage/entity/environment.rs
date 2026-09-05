//! `SeaORM` entity for the `qa_environments` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_environments")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    /// **`NOT NULL` since Task 20b** (`m20260903_000013`, decision D9).
    pub product_id: Uuid,
    pub description: Option<String>,
    pub available: bool,
    /// Overwritten unconditionally on every successful observation — the
    /// cluster is authoritative for its own version. Writer:
    /// `OrmEnvironmentsRepository::record_observation`. Added by
    /// `m20260813_000004_observed_build`'s twin migration, but this task is
    /// the first thing that ever writes it: that migration's "no writer"
    /// module doc now carries a **Superseded 2026-08-28** note pointing back
    /// here.
    pub observed_version: Option<String>,
    /// Mirrors `observed_version` in every respect — nullable, same shape,
    /// same writer, overwritten unconditionally on success. Added by
    /// `m20260813_000004_observed_build`, whose module doc said this column
    /// had no writer; it did not until this task, and
    /// `OrmEnvironmentsRepository::record_observation` is now it. That doc now
    /// says so too.
    pub observed_build: Option<String>,
    /// Per-environment default branch override; `NULL` means "no override, use the
    /// repository's default". Added by
    /// `m20260814_000006_platform_default_branch`.
    ///
    /// Unlike the `observed_*` and observation columns on this entity, this one
    /// is **operator-set**, so it has a different writer: `OrmEnvironmentsRepository::create`
    /// and `::update`. Always `NULL` or a trimmed, non-empty string — `EnvironmentsService`
    /// normalises before either reaches here.
    pub default_branch: Option<String>,
    /// Whether this environment is its product's **default** — what the Run and
    /// Schedule dialogs' "Default cluster" option resolves to. Added by
    /// `m20260831_000009_platform_is_default`.
    ///
    /// Operator-set, like `default_branch`, so its writers are
    /// `OrmEnvironmentsRepository::create` and `::update` rather than
    /// `record_observation`. `NOT NULL`: an environment either is its product's
    /// default or is not, and there is no third state.
    ///
    /// **At most one row per (tenant, product) may be `true`.** That rule is
    /// enforced by `EnvironmentsService`, not by a database constraint — see the
    /// migration's module doc for why a partial unique index was rejected.
    pub is_default: bool,
    /// The message from the most recent **failed** observation attempt, or
    /// `NULL` if the most recent attempt succeeded (or none has run yet). A
    /// human-readable string, e.g. `namespaces "virtuozzo" not found` — shown
    /// to an operator so a broken observation is diagnosable rather than
    /// silent. Cleared back to `NULL` by the next successful observation.
    /// Added by `m20260828_000007_platform_observation`. Writer:
    /// `OrmEnvironmentsRepository::record_observation`.
    pub version_detect_error: Option<String>,
    /// When the most recent observation attempt — success or failure — ran.
    /// `NULL` means no attempt has ever completed. Added by
    /// `m20260828_000007_platform_observation`. Writer:
    /// `OrmEnvironmentsRepository::record_observation`.
    pub version_detected_at: Option<OffsetDateTime>,
    /// The environment's credentials in **plugin shape**: a JSON array of
    /// `{"key", "credstore_ref"}` objects, decoded by
    /// [`mapper::StoredEnvironmentCredential`](crate::infra::storage::mapper::StoredEnvironmentCredential).
    /// `[]` for an environment whose credentials have never been written
    /// through the plugin path.
    ///
    /// **There is no `value` field in that shape, ever**, which is what makes
    /// this column structurally incapable of holding credential material —
    /// the rule `infra::runner_secret_errors` exists to enforce elsewhere, applied
    /// here to a schema instead. Added by
    /// `m20260903_000011_environment_plugin_columns`, which backfills it from
    /// `kubeconfig_credstore_ref`. Writer: Task 15's `record_observation`;
    /// until then the backfill is the only thing that has ever set it.
    pub credentials: serde_json::Value,
    /// The most recent observation's plugin-defined attribute map, stored as
    /// `qa_product_sdk::observation::ObservedAttrs` itself (a JSON object of
    /// string to string). `{}` for an environment the plugin path has never
    /// observed.
    ///
    /// Every key here was declared in the plugin's `observed_schema()`:
    /// `retain_declared` drops the rest **before** this column is written, so
    /// an undeclared attribute cannot reach storage and therefore cannot reach
    /// `EnvironmentDto`. Added by
    /// `m20260903_000011_environment_plugin_columns`, deliberately with no
    /// backfill — the keys are the plugin's to choose, and the first
    /// observation cycle writes them.
    pub observed_attrs: serde_json::Value,
    /// The operator-set, **non-secret** half of an environment's credential
    /// fields: `EnvironmentHandle`'s `config` channel, passed to the plugin
    /// verbatim. `{}` when nothing is overridden.
    ///
    /// Kept apart from [`Self::observed_attrs`] because the two have different
    /// authors — this one a human, that one a machine — and an environment
    /// page has to be able to say which of two values a human may correct.
    /// Added by `m20260903_000011_environment_plugin_columns`, which backfills
    /// it from each environment's `VPADM_NAMESPACE` variable; see that
    /// migration's module doc for why the case-fold belongs in a one-time
    /// backfill and nowhere else.
    pub config: serde_json::Value,
    /// The `FieldRole::BaseUrl` projection of [`Self::observed_attrs`].
    ///
    /// It replaced `vhp_base_url`, which Task 19 dropped.
    ///
    /// During Phase D both columns are written from the same observation, and
    /// `vhp_base_url` stays authoritative for existing readers; Task 19 drops
    /// it. `NULL` means "never conclusively detected", exactly as it does
    /// there. Added by `m20260903_000011_environment_plugin_columns`, which
    /// backfills it from `vhp_base_url`.
    pub observed_base_url: Option<String>,
    /// `qa_product_sdk::observation::HealthState`'s wire form — `ok`,
    /// `degraded`, `down` or `unknown`. `NOT NULL DEFAULT 'unknown'`, matching
    /// that type's `#[default]`, and read back through
    /// `HealthState::from_str_or_unknown`, which is total by design so a value
    /// written by a newer build cannot panic an older one.
    ///
    /// `unknown` is what a *failed* read stores as well as what a never-read
    /// row holds: [`Self::health_checked_at`] is what separates them. Added by
    /// `m20260903_000011_environment_plugin_columns`, which backfills it from
    /// `cluster_status` (see its module doc for the mapping table).
    pub health_state: String,
    /// Why the most recent health read reached the state it did, when there is
    /// something to say. **Classified text only** — a fixed `&'static str`
    /// chosen by variant, or the one sanctioned exception
    /// (`PluginFailure::remote_message`) — never a formatted error and never
    /// anything derived from a credential (**D12**). Added by
    /// `m20260903_000011_environment_plugin_columns`, which backfills it from
    /// `cluster_status_message`, itself already classified (D-CH-5).
    pub health_detail: Option<String>,
    /// When the most recent health read ran. `NULL` means **nothing ever
    /// looked**, which is a different fact from `health_state = 'unknown'`
    /// after a look that failed — and the only thing that distinguishes them,
    /// which is why `HealthOutcome::NotAttempted` writes no health column at
    /// all rather than stamping a time. Added by
    /// `m20260903_000011_environment_plugin_columns`, which backfills it from
    /// `cluster_checked_at`.
    pub health_checked_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
