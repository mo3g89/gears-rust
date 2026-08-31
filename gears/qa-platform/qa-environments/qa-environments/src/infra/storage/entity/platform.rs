//! `SeaORM` entity for the `qa_platforms` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_platforms")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    pub kubeconfig_credstore_ref: String,
    pub available: bool,
    /// Overwritten unconditionally on every successful observation — the
    /// cluster is authoritative for its own version. Writer:
    /// `OrmPlatformsRepository::record_observation`. Added by
    /// `m20260813_000004_observed_build`'s twin migration, but this task is
    /// the first thing that ever writes it: that migration's "no writer"
    /// module doc now carries a **Superseded 2026-08-28** note pointing back
    /// here.
    pub observed_version: Option<String>,
    /// Mirrors `observed_version` in every respect — nullable, same shape,
    /// same writer, overwritten unconditionally on success. Added by
    /// `m20260813_000004_observed_build`, whose module doc said this column
    /// had no writer; it did not until this task, and
    /// `OrmPlatformsRepository::record_observation` is now it. That doc now
    /// says so too.
    pub observed_build: Option<String>,
    /// Per-platform default branch override; `NULL` means "no override, use the
    /// repository's default". Added by
    /// `m20260814_000006_platform_default_branch`.
    ///
    /// Unlike the `observed_*` and observation columns on this entity, this one
    /// is **operator-set**, so it has a different writer: `OrmPlatformsRepository::create`
    /// and `::update`. Always `NULL` or a trimmed, non-empty string — `PlatformsService`
    /// normalises before either reaches here.
    pub default_branch: Option<String>,
    /// Whether this platform is its product's **default** — what the Run and
    /// Schedule dialogs' "Default cluster" option resolves to. Added by
    /// `m20260831_000009_platform_is_default`.
    ///
    /// Operator-set, like `default_branch`, so its writers are
    /// `OrmPlatformsRepository::create` and `::update` rather than
    /// `record_observation`. `NOT NULL`: a platform either is its product's
    /// default or is not, and there is no third state.
    ///
    /// **At most one row per (tenant, product) may be `true`.** That rule is
    /// enforced by `PlatformsService`, not by a database constraint — see the
    /// migration's module doc for why a partial unique index was rejected.
    pub is_default: bool,
    /// The platform's base domain, prefixed `https://` (e.g.
    /// `https://sv.jele.io`), recovered by comparing the gateway's
    /// externally-visible hostnames. `NULL` means "never successfully
    /// detected" — **not** "no base URL" — so `record_observation` only ever
    /// overwrites it with a *conclusive* detection; an inconclusive one (the
    /// gateway `ConfigMap` missing, unreadable, or carrying fewer than two
    /// hostnames to compare) leaves whatever is already stored untouched.
    /// Added by `m20260828_000007_platform_observation`. Writer:
    /// `OrmPlatformsRepository::record_observation`.
    pub vhp_base_url: Option<String>,
    /// The namespace the platform's core components were detected in. Unlike
    /// `vhp_base_url` and the two `observed_*` columns above, a value already
    /// stored here is **never overwritten** — only a currently-`NULL` row is
    /// filled from a detection. This is the one column of the four where the
    /// stored value, not the freshest read, wins: an operator who has set this
    /// (or a previous detection that already filled it) is assumed correct
    /// going forward. Added by `m20260828_000007_platform_observation`.
    /// Writer: `OrmPlatformsRepository::record_observation`.
    pub observed_namespace: Option<String>,
    /// The message from the most recent **failed** observation attempt, or
    /// `NULL` if the most recent attempt succeeded (or none has run yet). A
    /// human-readable string, e.g. `namespaces "virtuozzo" not found` — shown
    /// to an operator so a broken observation is diagnosable rather than
    /// silent. Cleared back to `NULL` by the next successful observation.
    /// Added by `m20260828_000007_platform_observation`. Writer:
    /// `OrmPlatformsRepository::record_observation`.
    pub version_detect_error: Option<String>,
    /// When the most recent observation attempt — success or failure — ran.
    /// `NULL` means no attempt has ever completed. Added by
    /// `m20260828_000007_platform_observation`. Writer:
    /// `OrmPlatformsRepository::record_observation`.
    pub version_detected_at: Option<OffsetDateTime>,
    /// `Healthy`|`Degraded`|`Unhealthy`|`Warning` from a successful cluster
    /// read, or `Unreachable` when the read failed. `NULL` means **never
    /// checked**, which is a different fact from `Unreachable` and is rendered
    /// differently. Added by `m20260828_000008_platform_cluster_health`.
    /// Writer: `OrmPlatformsRepository::record_observation`.
    pub cluster_status: Option<String>,
    /// Set only when `cluster_status` is `Unreachable`; `NULL` for every other
    /// status. The three sentences legacy attaches to Warning, Unhealthy and
    /// Degraded are pure functions of the status and the counts, so they are
    /// composed in the UI rather than stored — which means every non-
    /// `Unreachable` row has nothing in this column to leak. The value that
    /// does land here comes from `infra::observer::errors`, never from
    /// formatting a `kube::Error` (D-CH-5). Added by
    /// `m20260828_000008_platform_cluster_health`. Writer:
    /// `OrmPlatformsRepository::record_observation`.
    pub cluster_status_message: Option<String>,
    /// The node list as JSON. Every count the UI shows is derived from this,
    /// so no stored count can disagree with it (D-CH-2). `NULL` after a failed
    /// read, per D-CH-3: a failed read must clear the node list rather than
    /// keep an hour-old one, deliberately diverging from `version_detect_error`'s
    /// stale-but-known rule above, because a stale-but-reported-ready node is
    /// the exact false signal `Unreachable` exists to prevent. Added by
    /// `m20260828_000008_platform_cluster_health`. Writer:
    /// `OrmPlatformsRepository::record_observation`.
    pub cluster_nodes: Option<serde_json::Value>,
    /// `NULL` means the namespace list could not be read — **not** zero
    /// namespaces. Cleared alongside `cluster_nodes` on a failed read, for the
    /// same D-CH-3 reason. Added by `m20260828_000008_platform_cluster_health`.
    /// Writer: `OrmPlatformsRepository::record_observation`.
    pub cluster_namespace_count: Option<i32>,
    /// When the last cluster read was attempted, success or failure. `NULL`
    /// means no attempt has ever completed. Added by
    /// `m20260828_000008_platform_cluster_health`. Writer:
    /// `OrmPlatformsRepository::record_observation`.
    pub cluster_checked_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
