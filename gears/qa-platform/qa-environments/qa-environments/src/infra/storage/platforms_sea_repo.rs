use async_trait::async_trait;
use qa_environments_sdk::{NewPlatform, PlatformPatch, TargetPlatform};
use sea_orm::sea_query::{Expr, Func};
use sea_orm::{ActiveValue, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
    secure_update_with_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::{HealthOutcome, ObservationOutcome, PlatformObservation};
use crate::domain::repos::PlatformsRepository;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::platform::{
    self, ActiveModel as PlatformAM, Column as PlatformColumn, Entity as PlatformEntity,
};
use crate::infra::storage::mapper::platform_to_sdk;

/// ORM-based implementation of the `PlatformsRepository` trait.
#[derive(Clone, Default)]
pub struct OrmPlatformsRepository;

#[async_trait]
impl PlatformsRepository for OrmPlatformsRepository {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TargetPlatform>, DomainError> {
        let found = PlatformEntity::find()
            .filter(sea_orm::Condition::all().add(Expr::col(PlatformColumn::Id).eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(found.map(platform_to_sdk))
    }

    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<TargetPlatform>, DomainError> {
        let rows = PlatformEntity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(platform_to_sdk).collect())
    }

    async fn list_all_with_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<(TargetPlatform, Uuid)>, DomainError> {
        let rows = PlatformEntity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let tenant_id = row.tenant_id;
                (platform_to_sdk(row), tenant_id)
            })
            .collect())
    }

    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewPlatform,
        kubeconfig_credstore_ref: String,
    ) -> Result<TargetPlatform, DomainError> {
        let now = OffsetDateTime::now_utc();
        let platform = TargetPlatform {
            id: Uuid::new_v4(),
            name: new.name,
            product_id: new.product_id,
            description: new.description,
            // The already-resolved reference, never `new.kubeconfig_*` — see
            // `PlatformsRepository::create`.
            kubeconfig_credstore_ref,
            available: true,
            observed_version: None,
            observed_build: None,
            default_branch: new.default_branch,
            is_default: new.is_default,
            // A freshly created platform has never been observed: every
            // column `record_observation` writes -- both halves -- starts
            // out `None`, mirroring the `ActiveModel` below.
            vhp_base_url: None,
            observed_namespace: None,
            version_detect_error: None,
            version_detected_at: None,
            // Compile-forced by Task 5's `TargetPlatform::cluster` field
            // (this literal must be exhaustive); a fresh platform has never
            // been observed, so `None` is the only value this can be.
            cluster: None,
            created_at: now,
            updated_at: now,
        };

        let am = PlatformAM {
            id: ActiveValue::Set(platform.id),
            tenant_id: ActiveValue::Set(tenant_id),
            name: ActiveValue::Set(platform.name.clone()),
            product_id: ActiveValue::Set(platform.product_id),
            description: ActiveValue::Set(platform.description.clone()),
            kubeconfig_credstore_ref: ActiveValue::Set(platform.kubeconfig_credstore_ref.clone()),
            available: ActiveValue::Set(platform.available),
            observed_version: ActiveValue::Set(platform.observed_version.clone()),
            observed_build: ActiveValue::Set(platform.observed_build.clone()),
            default_branch: ActiveValue::Set(platform.default_branch.clone()),
            is_default: ActiveValue::Set(platform.is_default),
            // A freshly created platform has never been observed: every
            // column `record_observation` writes -- both the version half and
            // the cluster-health half -- starts out NULL.
            vhp_base_url: ActiveValue::Set(None),
            observed_namespace: ActiveValue::Set(None),
            version_detect_error: ActiveValue::Set(None),
            version_detected_at: ActiveValue::Set(None),
            cluster_status: ActiveValue::Set(None),
            cluster_status_message: ActiveValue::Set(None),
            cluster_nodes: ActiveValue::Set(None),
            cluster_namespace_count: ActiveValue::Set(None),
            cluster_checked_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(platform.created_at),
            updated_at: ActiveValue::Set(platform.updated_at),
        };

        match secure_insert::<PlatformEntity>(am, scope, runner).await {
            Ok(_) => Ok(platform),
            Err(e) if e.is_unique_violation() => Err(DomainError::PlatformNameExists {
                name: platform.name,
            }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        patch: PlatformPatch,
    ) -> Result<Option<TargetPlatform>, DomainError> {
        let existing = PlatformEntity::find()
            .filter(sea_orm::Condition::all().add(Expr::col(PlatformColumn::Id).eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        let Some(existing) = existing else {
            return Ok(None);
        };

        let attempted_name = patch.name.clone().unwrap_or_else(|| existing.name.clone());

        let mut am: PlatformAM = platform::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            name: ActiveValue::Unchanged(existing.name.clone()),
            product_id: ActiveValue::Unchanged(existing.product_id),
            description: ActiveValue::Unchanged(existing.description.clone()),
            kubeconfig_credstore_ref: ActiveValue::Unchanged(
                existing.kubeconfig_credstore_ref.clone(),
            ),
            available: ActiveValue::Unchanged(existing.available),
            observed_version: ActiveValue::Unchanged(existing.observed_version.clone()),
            observed_build: ActiveValue::Unchanged(existing.observed_build.clone()),
            default_branch: ActiveValue::Unchanged(existing.default_branch.clone()),
            is_default: ActiveValue::Unchanged(existing.is_default),
            // None of these nine is reachable through `PlatformPatch`: they
            // are observation-only columns, written solely by
            // `record_observation`, so an operator-driven `update()` leaves
            // them exactly as they were.
            vhp_base_url: ActiveValue::Unchanged(existing.vhp_base_url.clone()),
            observed_namespace: ActiveValue::Unchanged(existing.observed_namespace.clone()),
            version_detect_error: ActiveValue::Unchanged(existing.version_detect_error.clone()),
            version_detected_at: ActiveValue::Unchanged(existing.version_detected_at),
            cluster_status: ActiveValue::Unchanged(existing.cluster_status.clone()),
            cluster_status_message: ActiveValue::Unchanged(existing.cluster_status_message.clone()),
            cluster_nodes: ActiveValue::Unchanged(existing.cluster_nodes.clone()),
            cluster_namespace_count: ActiveValue::Unchanged(existing.cluster_namespace_count),
            cluster_checked_at: ActiveValue::Unchanged(existing.cluster_checked_at),
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };

        if let Some(name) = patch.name {
            am.name = ActiveValue::Set(name);
        }
        if let Some(product_id) = patch.product_id {
            am.product_id = ActiveValue::Set(product_id);
        }
        if let Some(description) = patch.description {
            am.description = ActiveValue::Set(description);
        }
        if let Some(kubeconfig_credstore_ref) = patch.kubeconfig_credstore_ref {
            am.kubeconfig_credstore_ref = ActiveValue::Set(kubeconfig_credstore_ref);
        }
        if let Some(available) = patch.available {
            am.available = ActiveValue::Set(available);
        }
        // Tri-state, exactly like `product_id` and `description` above: the
        // outer `Some` is what distinguishes "the caller said something" from
        // "the field was absent", and the inner `Option` is written straight to
        // the nullable column, so `Some(None)` clears the override. This mirrors
        // the source system's patch path, whose `"__NULL__"` sentinel expresses
        // the same three states in SQL (`manager/src/services/platforms.rs:447-496`).
        if let Some(default_branch) = patch.default_branch {
            am.default_branch = ActiveValue::Set(default_branch);
        }
        // Two-state, unlike `default_branch` above: the flag is a `bool` with no
        // "unset" reading, so the outer `Some` alone carries the caller's intent.
        // Clearing the product's *previous* default is not done here -- that is a
        // second row, and `PlatformsService` owns the rule; see
        // `m20260831_000009_platform_is_default`.
        if let Some(is_default) = patch.is_default {
            am.is_default = ActiveValue::Set(is_default);
        }

        match secure_update_with_scope::<PlatformEntity>(am, scope, id, runner).await {
            Ok(model) => Ok(Some(platform_to_sdk(model))),
            Err(e) if e.is_unique_violation() => Err(DomainError::PlatformNameExists {
                name: attempted_name,
            }),
            Err(e) => Err(db_err(e)),
        }
    }

    /// One `UPDATE ... SET is_default = false WHERE product_id = ? AND id <> ?`.
    ///
    /// `update_many` rather than a read-then-write loop: the rule is "no other row
    /// holds the flag", which one statement expresses atomically. A loop would
    /// read a set, then write it back row by row, and a concurrent promotion
    /// landing between the two would survive the clear.
    ///
    /// Callers order this **before** the write that sets the new default; see the
    /// trait's doc for why (this gear has no transaction seam, and zero defaults is
    /// a recoverable state while two is an ambiguous one).
    ///
    /// Rows already `false` are matched and rewritten rather than filtered out.
    /// Adding `AND is_default = true` would make `rows_affected` mean "how many
    /// defaults were displaced", which is a more useful number, but it also makes
    /// the statement's correctness depend on the stored value being accurate —
    /// and this method exists precisely to repair a table that may have drifted.
    /// The count is therefore not load-bearing; no caller branches on it.
    async fn clear_default_for_product<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        product_id: Uuid,
        except_id: Uuid,
    ) -> Result<u64, DomainError> {
        let result = PlatformEntity::update_many()
            .col_expr(PlatformColumn::IsDefault, Expr::value(false))
            .filter(
                sea_orm::Condition::all()
                    .add(Expr::col(PlatformColumn::ProductId).eq(product_id))
                    .add(Expr::col(PlatformColumn::Id).ne(except_id)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        Ok(result.rows_affected)
    }

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let result = PlatformEntity::delete_many()
            .filter(sea_orm::Condition::all().add(Expr::col(PlatformColumn::Id).eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        Ok(result.rows_affected > 0)
    }

    /// Persist one observation attempt -- both the version-detection half
    /// (`observation.platform`) and the cluster-health half
    /// (`observation.health`) -- with five deliberately different merge
    /// rules, in a single `UPDATE`. The version half is ported from legacy
    /// (`manager/src/services/platforms.rs:1189-1205`); the health half is
    /// new with the cluster-health spec.
    ///
    /// * `observed_version` / `observed_build` — **overwritten
    ///   unconditionally** on success. The cluster is authoritative for its
    ///   own version, so whatever it just reported replaces whatever was
    ///   stored, including replacing a build with `NULL` if the cluster no
    ///   longer reports one.
    /// * `observed_namespace` — the opposite default: `COALESCE(observed_namespace,
    ///   $detected)` keeps a value **already set** and only fills a `NULL` one.
    ///   An operator's (or an earlier detection's) namespace is never
    ///   clobbered by a later one.
    /// * `vhp_base_url` — `COALESCE($detected_url, vhp_base_url)`, the
    ///   **reverse argument order** from `observed_namespace` above, and that
    ///   reversal is the entire point: `DetectedPlatform::base_domain` is
    ///   `None` for an *inconclusive* read (the gateway `ConfigMap` missing,
    ///   unreadable, or carrying fewer than two hostnames to compare), never
    ///   for "this platform has no base URL". Putting the detected value
    ///   first means a conclusive detection overwrites, while an inconclusive
    ///   one (`$detected_url = NULL`) falls through to whatever is already
    ///   stored and leaves it alone. Writing `NULL` unconditionally here would
    ///   erase a working URL every time the gateway map briefly failed to
    ///   read — the bug this rule exists to avoid.
    /// * `cluster_status` / `cluster_nodes` / `cluster_namespace_count` — on a
    ///   successful health read, **overwritten unconditionally**, exactly like
    ///   `observed_version` above: the cluster is authoritative about its own
    ///   nodes (D-CH-2, D-CH-1).
    ///
    /// On `Failed` (version half), `observed_version`, `observed_build` and
    /// `vhp_base_url` are not named in the `UPDATE` at all — stale-but-known
    /// beats blank — and only `version_detect_error` (the message) and
    /// `version_detected_at` (now) are written. On `Detected`,
    /// `version_detect_error` is cleared back to `NULL`: a previous failure's
    /// error must not keep showing once a later attempt has succeeded.
    ///
    /// The health half's `Failed` arm makes the **opposite** choice, and
    /// deliberately so (D-CH-3): `cluster_nodes` and `cluster_namespace_count`
    /// are CLEARED to `NULL`, not left as whatever was last read. A cluster
    /// that stopped answering is not "the same as it was a moment ago" the
    /// way a namespace-list-read glitch during version detection is --
    /// stale-but-known readiness rendered as a green dot on a cluster that is
    /// actually unreachable is the exact false signal `Unreachable` exists to
    /// prevent, so the failed read must blank what it can no longer vouch for
    /// rather than let an old reading keep speaking for it.
    ///
    /// The health half has a third arm, `NotAttempted`, which writes **none**
    /// of the five cluster columns. It is the outcome a build without the
    /// `platform-observation` feature produces, and it is deliberately not
    /// `Failed`: `Failed` persists `cluster_status = "Unreachable"`, a claim
    /// that somebody's cluster could not be reached, which a build that never
    /// opened a connection has no standing to make.
    ///
    /// `version_detected_at` is set on **both** branches, not only on failure.
    /// The brief only spells out the failure case explicitly, but the column
    /// names *the last observation attempt*, not *the last successful one* —
    /// `version_detect_error`'s clear-on-success already carries the
    /// success/failure distinction, so `version_detected_at` is free to mean
    /// "how stale is everything above" regardless of which branch ran, which
    /// is the more useful reading for an operator or a future staleness check.
    /// `cluster_checked_at` mirrors this on the health half, set on both of
    /// its branches for the identical reason.
    ///
    /// The two halves are independent (D-CH-4): each is matched on its own
    /// `observation.platform` / `observation.health` value and chained onto
    /// the same `update`, so a `Failed` health read never touches the columns
    /// the version half just wrote in the same call, and vice versa.
    ///
    /// `COALESCE` is ordinary ANSI SQL, so this one statement is identical
    /// across Postgres, `MySQL` and `SQLite` — no dialect branch is needed
    /// here, unlike the migrations that add these columns.
    ///
    /// `updated_at` is deliberately **not** touched: it tracks operator edits
    /// via `update()`, and a background observation is not one — bumping it
    /// here would make an unattended poller look like continuous manual
    /// editing to anything sorting or auditing on that column.
    ///
    /// # Errors
    ///
    /// `DomainError::PlatformNotFound` if no row matching `id` is visible
    /// under `scope` (covers both "no such platform" and "not in scope").
    async fn record_observation<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        observation: &PlatformObservation,
    ) -> Result<(), DomainError> {
        let now = OffsetDateTime::now_utc();

        let update = PlatformEntity::update_many()
            .filter(sea_orm::Condition::all().add(Expr::col(PlatformColumn::Id).eq(id)))
            .secure()
            .scope_with(scope);

        let update = match &observation.platform {
            ObservationOutcome::Detected(detected) => {
                let detected_url = detected
                    .base_domain
                    .as_ref()
                    .map(|domain| format!("https://{domain}"));
                update
                    // Cluster is authoritative: overwrite outright, even to NULL.
                    .col_expr(
                        PlatformColumn::ObservedVersion,
                        Expr::value(Some(detected.version.clone())),
                    )
                    .col_expr(
                        PlatformColumn::ObservedBuild,
                        Expr::value(detected.build.clone()),
                    )
                    // Never overwrite an already-set namespace: detected value
                    // is the COALESCE fallback, not the first argument.
                    .col_expr(
                        PlatformColumn::ObservedNamespace,
                        Func::coalesce([
                            Expr::col(PlatformColumn::ObservedNamespace).into(),
                            Expr::value(detected.namespace.clone()),
                        ])
                        .into(),
                    )
                    // A conclusive detection (Some) wins; an inconclusive one
                    // (None, i.e. SQL NULL) falls through to the stored value.
                    // Detected value comes FIRST here -- the opposite order
                    // from observed_namespace above, deliberately.
                    .col_expr(
                        PlatformColumn::VhpBaseUrl,
                        Func::coalesce([
                            Expr::value(detected_url),
                            Expr::col(PlatformColumn::VhpBaseUrl).into(),
                        ])
                        .into(),
                    )
                    .col_expr(
                        PlatformColumn::VersionDetectError,
                        Expr::value(None::<String>),
                    )
                    .col_expr(PlatformColumn::VersionDetectedAt, Expr::value(Some(now)))
            }
            ObservationOutcome::Failed(message) => update
                // observed_version / observed_build / vhp_base_url are
                // deliberately absent from this branch: stale-but-known beats
                // blank, so a failed attempt leaves them untouched.
                .col_expr(
                    PlatformColumn::VersionDetectError,
                    Expr::value(Some(message.clone())),
                )
                .col_expr(PlatformColumn::VersionDetectedAt, Expr::value(Some(now))),
        };

        let update = match &observation.health {
            HealthOutcome::Checked(health) => {
                let nodes = serde_json::to_value(&health.nodes).unwrap_or(serde_json::Value::Null);
                update
                    // The cluster is authoritative about its own nodes:
                    // overwrite outright, exactly as observed_version does.
                    .col_expr(
                        PlatformColumn::ClusterStatus,
                        Expr::value(Some(health.status().as_str().to_owned())),
                    )
                    .col_expr(PlatformColumn::ClusterStatusMessage, Expr::value(None::<String>))
                    .col_expr(PlatformColumn::ClusterNodes, Expr::value(Some(nodes)))
                    .col_expr(
                        PlatformColumn::ClusterNamespaceCount,
                        Expr::value(health.namespace_count.and_then(|n| i32::try_from(n).ok())),
                    )
                    .col_expr(PlatformColumn::ClusterCheckedAt, Expr::value(Some(now)))
            }
            HealthOutcome::Failed(message) => update
                // D-CH-3: CLEARED, not kept. Unlike the version half above,
                // stale-but-known is worse than blank here -- a cluster that
                // just failed to answer must not go on reporting last cycle's
                // readiness as if nothing had changed.
                .col_expr(
                    PlatformColumn::ClusterStatus,
                    Expr::value(Some("Unreachable".to_owned())),
                )
                .col_expr(
                    PlatformColumn::ClusterStatusMessage,
                    Expr::value(Some(message.clone())),
                )
                .col_expr(PlatformColumn::ClusterNodes, Expr::value(None::<serde_json::Value>))
                .col_expr(PlatformColumn::ClusterNamespaceCount, Expr::value(None::<i32>))
                .col_expr(PlatformColumn::ClusterCheckedAt, Expr::value(Some(now))),
            // No cluster read was attempted, so none of the five columns is
            // named in the UPDATE at all -- not even cluster_checked_at,
            // because nothing checked. The row keeps whatever it already
            // held; a row that was never checked stays never-checked (all
            // five NULL), which is what makes the UI fall back to the
            // reachability dot (D-CH-6) instead of rendering a manufactured
            // status. This is NOT the Failed arm with the writes omitted: it
            // must never clear a reading either, so a build that cannot look
            // can neither invent nor erase what an earlier one saw.
            HealthOutcome::NotAttempted => update,
        };

        let result = update.exec(runner).await.map_err(db_err)?;
        if result.rows_affected == 0 {
            return Err(DomainError::PlatformNotFound { id });
        }
        Ok(())
    }
}

/// Tests for `record_observation`'s five merge rules (three for the version
/// half, two for the cluster-health half).
///
/// # Why these assert on the raw entity, not `TargetPlatform`
///
/// `TargetPlatform` (the SDK contract type `platform_to_sdk` maps onto)
/// carries none of the columns this method writes — exposing them through
/// the SDK model is a later task's concern, not this one's. So every
/// assertion here reads the row back via `PlatformEntity::find()` directly
/// (`fetch_row` below), the same secure, scoped query `get()` runs
/// internally, just without the mapping step that would drop the columns
/// under test.
///
/// # Why most fixtures are built with two `record_observation` calls
///
/// A test that only checks "the value was written" cannot distinguish an
/// overwrite rule from a keep-if-set rule from a conclusive-wins rule — all
/// three produce the same result on a first write to a `NULL` column. Most
/// tests below therefore call `record_observation` twice: once to establish
/// a **stored** value (exactly as an earlier poll would have), and once more
/// with the outcome under test, then assert on the *difference* between the
/// two rules rather than on either write in isolation. The exception is
/// `a_failed_version_detection_leaves_a_successful_health_read_alone`, whose
/// property (the two halves of one call are independent, D-CH-4) is fully
/// exercised by a single call, so a second write would test nothing extra.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod record_observation_tests {
    use qa_environments_sdk::NewPlatform;
    use sea_orm::{EntityTrait, QueryFilter};
    use toolkit_db::secure::{DBRunner, SecureEntityExt};
    use toolkit_security::AccessScope;
    use uuid::Uuid;

    use super::{Expr, ObservationOutcome, PlatformColumn, PlatformEntity};
    use crate::domain::observation::{ClusterHealth, DetectedPlatform, NodeSummary};
    use crate::domain::ports::{HealthOutcome, PlatformObservation};
    use crate::domain::repos::PlatformsRepository;
    use crate::infra::storage::entity::platform;
    use crate::test_support::inmem_db;

    fn new_platform(name: &str) -> NewPlatform {
        NewPlatform {
            name: name.to_owned(),
            product_id: None,
            description: None,
            kubeconfig_credstore_ref: Some("credstore://test".to_owned()),
            kubeconfig: None,
            default_branch: None,
            is_default: false,
        }
    }

    /// Build a `Detected` outcome; `base_domain: None` is how a real
    /// inconclusive gateway read looks (see `compute_base_domain`), not an
    /// error -- callers below pass it explicitly to exercise that case.
    fn detected(
        version: &str,
        build: Option<&str>,
        namespace: &str,
        base_domain: Option<&str>,
    ) -> ObservationOutcome {
        ObservationOutcome::Detected(DetectedPlatform {
            version: version.to_owned(),
            build: build.map(str::to_owned),
            raw: version.to_owned(),
            namespace: namespace.to_owned(),
            base_domain: base_domain.map(str::to_owned),
        })
    }

    /// Wrap the two halves for `record_observation`'s new parameter.
    fn observation(platform: ObservationOutcome, health: HealthOutcome) -> PlatformObservation {
        PlatformObservation { platform, health }
    }

    /// A `Checked` health outcome with `count` nodes, all ready, the first of
    /// them control-plane -- the `sv-test` shape when `count == 1`.
    fn checked(count: usize, namespace_count: Option<u32>) -> HealthOutcome {
        HealthOutcome::Checked(ClusterHealth {
            nodes: (0..count)
                .map(|i| NodeSummary {
                    name: format!("node-{i}"),
                    control_plane: i == 0,
                    ready: true,
                    kubelet_version: Some("v1.33.4+k3s1".to_owned()),
                    os_image: Some("Ubuntu 24.04.3 LTS".to_owned()),
                })
                .collect(),
            namespace_count,
        })
    }

    /// Read the row back through the exact same secure, scoped query `get()`
    /// runs, but without `platform_to_sdk`'s mapping step -- which is what
    /// would drop the four columns under test.
    async fn fetch_row(conn: &impl DBRunner, scope: &AccessScope, id: Uuid) -> platform::Model {
        PlatformEntity::find()
            .filter(sea_orm::Condition::all().add(Expr::col(PlatformColumn::Id).eq(id)))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .unwrap()
            .expect("the platform row must read back")
    }

    /// Seed a fresh platform (every observation column `NULL`) and return its
    /// id, so each test starts from the same known-empty baseline.
    async fn seed_platform(
        conn: &impl DBRunner,
        scope: &AccessScope,
        tenant: Uuid,
        name: &str,
    ) -> Uuid {
        super::OrmPlatformsRepository
            .create(
                conn,
                scope,
                tenant,
                new_platform(name),
                "credstore://test".to_owned(),
            )
            .await
            .unwrap()
            .id
    }

    /// `observed_version` / `observed_build`: the cluster is authoritative, so
    /// a later detection overwrites a former one outright -- no COALESCE, no
    /// "keep what's set". This is the rule `observed_namespace` and
    /// `vhp_base_url` below are each, in their own way, NOT.
    #[tokio::test]
    async fn a_successful_detection_overwrites_version_and_build() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(detected("26.4", Some("9"), "virtuozzo", None), checked(1, Some(14))),
            )
            .await
            .unwrap();
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(1, Some(14))),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.observed_version.as_deref(), Some("26.5"));
        assert_eq!(row.observed_build.as_deref(), Some("0"));
    }

    /// `observed_namespace`: the opposite rule from `observed_version` above.
    /// A value already stored survives a later detection; only a `NULL`
    /// column is filled. Both halves are asserted here so swapping this
    /// rule for `observed_version`'s (or vice versa) would turn one of them
    /// red.
    #[tokio::test]
    async fn a_set_namespace_survives_detection_but_an_unset_one_is_filled() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);

        // Half 1: already set, must survive.
        let set_id = seed_platform(&conn, &scope, tenant, "set").await;
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                set_id,
                &observation(detected("26.5", Some("0"), "custom", None), checked(1, Some(14))),
            )
            .await
            .unwrap();
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                set_id,
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(1, Some(14))),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, set_id).await;
        assert_eq!(
            row.observed_namespace.as_deref(),
            Some("custom"),
            "a namespace already stored must survive a later detection"
        );

        // Half 2: unset, must be filled.
        let unset_id = seed_platform(&conn, &scope, tenant, "unset").await;
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                unset_id,
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(1, Some(14))),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, unset_id).await;
        assert_eq!(
            row.observed_namespace.as_deref(),
            Some("virtuozzo"),
            "an unset namespace must be filled from the first detection"
        );
    }

    /// `vhp_base_url`: the mirror image of `observed_namespace` -- and
    /// deliberately so. Here it is the STORED value that must yield, and only
    /// to a CONCLUSIVE detection; an inconclusive one (`base_domain: None`,
    /// how a missing/unreadable/too-few-hostnames gateway read looks) must
    /// leave a working URL alone rather than blank it. Swapping this rule for
    /// `observed_namespace`'s -- i.e. `COALESCE(vhp_base_url, $detected)` --
    /// would make the second half of this test fail exactly the opposite way:
    /// the URL would be overwritten by the inconclusive `NULL` instead of
    /// preserved.
    #[tokio::test]
    async fn a_conclusive_base_url_wins_and_an_inconclusive_one_preserves() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);

        // Half 1: a conclusive detection overwrites a stored URL.
        let overwritten_id = seed_platform(&conn, &scope, tenant, "overwritten").await;
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                overwritten_id,
                &observation(detected("26.5", Some("0"), "virtuozzo", Some("old.example.com")), checked(1, Some(14))),
            )
            .await
            .unwrap();
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                overwritten_id,
                &observation(detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")), checked(1, Some(14))),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, overwritten_id).await;
        assert_eq!(
            row.vhp_base_url.as_deref(),
            Some("https://sv.jele.io"),
            "a conclusive detection must overwrite a previously stored URL"
        );

        // Half 2: an inconclusive detection must NOT blank a stored URL.
        let preserved_id = seed_platform(&conn, &scope, tenant, "preserved").await;
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                preserved_id,
                &observation(detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")), checked(1, Some(14))),
            )
            .await
            .unwrap();
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                preserved_id,
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(1, Some(14))),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, preserved_id).await;
        assert_eq!(
            row.vhp_base_url.as_deref(),
            Some("https://sv.jele.io"),
            "an inconclusive detection (base_domain: None) must NOT erase a \
             working URL -- this is the rule an unreadable gateway ConfigMap \
             must never be allowed to violate"
        );
    }

    /// A failed detection must leave `observed_version`/`observed_build`
    /// untouched (stale-but-known beats blank) while still recording that the
    /// attempt happened and why.
    #[tokio::test]
    async fn a_failed_detection_keeps_the_last_known_version_and_records_why() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(1, Some(14))),
            )
            .await
            .unwrap();
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(ObservationOutcome::Failed("namespaces \"virtuozzo\" not found".to_owned()), checked(1, Some(14))),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(
            row.observed_version.as_deref(),
            Some("26.5"),
            "a failed detection must not blank the last known version"
        );
        assert_eq!(row.observed_build.as_deref(), Some("0"));
        assert_eq!(
            row.version_detect_error.as_deref(),
            Some("namespaces \"virtuozzo\" not found")
        );
        assert!(
            row.version_detected_at.is_some(),
            "the attempt must be timestamped even though it failed"
        );
    }

    /// A success must clear a previously recorded error back to `NULL` -- an
    /// operator must not keep seeing a stale failure once observation has
    /// recovered.
    #[tokio::test]
    async fn a_success_clears_a_previous_error() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(ObservationOutcome::Failed("namespaces \"virtuozzo\" not found".to_owned()), checked(1, Some(14))),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, id).await;
        assert!(
            row.version_detect_error.is_some(),
            "the error must be set first"
        );

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(1, Some(14))),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(
            row.version_detect_error, None,
            "a successful detection must clear a previously recorded error"
        );
    }

    /// A platform that does not exist (or is out of scope) must error rather
    /// than silently succeed on zero affected rows.
    #[tokio::test]
    async fn observing_a_missing_platform_is_an_error() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);

        let err = super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                Uuid::new_v4(),
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(1, Some(14))),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            crate::domain::error::DomainError::PlatformNotFound { .. }
        ));
    }

    /// D-CH-3, and the reason it diverges from the version rule above: a
    /// failed read must CLEAR the node list, not keep it. Stale readiness
    /// rendered as a green dot on a dead cluster is the exact failure
    /// `Unreachable` exists to prevent.
    #[tokio::test]
    async fn a_failed_health_read_clears_the_nodes_rather_than_keeping_them() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(2, Some(14))),
            )
            .await
            .unwrap();

        // The precondition: without this the assertions below could pass on a
        // row that never held nodes in the first place.
        let seeded = fetch_row(&conn, &scope, id).await;
        assert_eq!(seeded.cluster_status.as_deref(), Some("Healthy"));
        assert!(seeded.cluster_nodes.is_some(), "the seeded read must have stored nodes");
        assert_eq!(seeded.cluster_namespace_count, Some(14));
        let seeded_at = seeded.cluster_checked_at.expect("a stamped check time");

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", None),
                    HealthOutcome::Failed("the API server could not be reached".to_owned()),
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.cluster_status.as_deref(), Some("Unreachable"));
        assert_eq!(
            row.cluster_status_message.as_deref(),
            Some("the API server could not be reached")
        );
        assert!(
            row.cluster_nodes.is_none(),
            "D-CH-3: a failed read must clear the node list, not keep an hour-old one"
        );
        assert!(
            row.cluster_namespace_count.is_none(),
            "D-CH-3: the namespace count is part of the same reading"
        );
        assert!(
            row.cluster_checked_at.expect("still stamped") >= seeded_at,
            "a failed attempt is still an attempt and must stamp the time"
        );
    }

    /// D-CH-4: the halves are independent, so a failed health read must not
    /// disturb a version that was detected in the same call.
    #[tokio::test]
    async fn a_failed_health_read_leaves_the_observed_version_alone() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")),
                    checked(1, Some(14)),
                ),
            )
            .await
            .unwrap();
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")),
                    HealthOutcome::Failed("cannot list nodes".to_owned()),
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.observed_version.as_deref(), Some("26.5"));
        assert_eq!(row.observed_build.as_deref(), Some("0"));
        assert_eq!(row.vhp_base_url.as_deref(), Some("https://sv.jele.io"));
        assert!(row.version_detect_error.is_none());
        assert_eq!(row.cluster_status.as_deref(), Some("Unreachable"));
    }

    /// The mirror: a failed version detection must not clear a cluster read
    /// that succeeded in the same call. Without this test, "clear everything
    /// on any failure" would pass the one above.
    #[tokio::test]
    async fn a_failed_version_detection_leaves_a_successful_health_read_alone() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    ObservationOutcome::Failed("namespaces \"virtuozzo\" not found".to_owned()),
                    checked(1, Some(14)),
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(
            row.version_detect_error.as_deref(),
            Some("namespaces \"virtuozzo\" not found")
        );
        assert_eq!(row.cluster_status.as_deref(), Some("Healthy"));
        assert!(row.cluster_nodes.is_some(), "the health half succeeded and must be stored");
        assert_eq!(row.cluster_namespace_count, Some(14));
    }

    /// The health half's mirror of `a_success_clears_a_previous_error`: a
    /// successful read must clear `cluster_status_message` back to `NULL`.
    ///
    /// Scenario this guards: a cluster read fails at 10:00
    /// (`cluster_status = "Unreachable"`, message "the API server could not
    /// be reached"), then recovers at 10:05 (`Checked`, every node ready). If
    /// the message's clearing `col_expr` were ever dropped from the `Checked`
    /// arm, the row would read `Healthy` while still carrying the stale
    /// `Unreachable` message -- contradicting
    /// `entity::platform::Model::cluster_status_message`'s own invariant that
    /// the message is set only when the status is `Unreachable`, for every
    /// other status.
    #[tokio::test]
    async fn a_successful_health_read_clears_a_previous_unreachable_message() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", None),
                    HealthOutcome::Failed("the API server could not be reached".to_owned()),
                ),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.cluster_status.as_deref(), Some("Unreachable"));
        assert!(
            row.cluster_status_message.is_some(),
            "the message must be set first"
        );

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(1, Some(14))),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.cluster_status.as_deref(), Some("Healthy"));
        assert_eq!(
            row.cluster_status_message, None,
            "a successful health read must clear a previously recorded Unreachable message"
        );
    }

    /// The never-checked state -- all five cluster columns `NULL` -- is
    /// currently asserted only by the migration's own schema tests
    /// (`m20260828_000008_platform_cluster_health::tests`), never on the
    /// `create()` path this repository actually exposes. Worth its own test
    /// because never-checked versus `Unreachable` is the distinction this
    /// whole design turns on (see `cluster_status`'s own doc): a platform
    /// that has simply never been polled must not be confused with one whose
    /// last poll failed.
    #[tokio::test]
    async fn a_freshly_created_platform_has_no_cluster_health_recorded() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.cluster_status, None, "never checked, not Unreachable");
        assert_eq!(row.cluster_status_message, None);
        assert_eq!(row.cluster_nodes, None);
        assert_eq!(row.cluster_namespace_count, None);
        assert_eq!(row.cluster_checked_at, None);
    }

    /// `NotAttempted` writes NOTHING: a never-checked platform stays
    /// never-checked, all five columns `NULL`.
    ///
    /// This is the outcome a build without the `platform-observation` feature
    /// produces. It used to be `Failed`, which persists as
    /// `cluster_status = "Unreachable"` -- so one press of the un-gated
    /// Refresh button in such a build permanently marked a healthy platform
    /// dark red, contradicting D-CH-6's promise that a feature-off build
    /// falls back to the reachability dot.
    #[tokio::test]
    async fn a_not_attempted_health_outcome_leaves_a_never_checked_platform_untouched() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    ObservationOutcome::Failed("this build has no Kubernetes client".to_owned()),
                    HealthOutcome::NotAttempted,
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(
            row.cluster_status, None,
            "a build that attempted no cluster read must not claim Unreachable"
        );
        assert_eq!(row.cluster_status_message, None);
        assert_eq!(row.cluster_nodes, None);
        assert_eq!(row.cluster_namespace_count, None);
        assert_eq!(
            row.cluster_checked_at, None,
            "not even the check time: nothing checked"
        );
        // The version half is untouched by any of this and must still land.
        assert_eq!(
            row.version_detect_error.as_deref(),
            Some("this build has no Kubernetes client"),
            "the version half is an honest error field and still records"
        );
    }

    /// The other half of the same rule, and the one that stops `NotAttempted`
    /// from being a disguised clearer: after a `Checked` read has populated
    /// all five columns, a later `NotAttempted` must leave every one of them
    /// exactly as it was -- same status, same message, same nodes, same
    /// namespace count, same check time.
    ///
    /// Without this test the arm could be written as "clear everything", or
    /// as `Failed`-minus-the-writes, and the previous test would still pass
    /// because it starts from an all-`NULL` row.
    #[tokio::test]
    async fn a_not_attempted_health_outcome_preserves_an_earlier_successful_read() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", None),
                    checked(2, Some(14)),
                ),
            )
            .await
            .unwrap();

        let seeded = fetch_row(&conn, &scope, id).await;
        assert_eq!(seeded.cluster_status.as_deref(), Some("Healthy"));
        assert!(
            seeded.cluster_nodes.is_some(),
            "the seeded read must have stored nodes"
        );
        assert_eq!(seeded.cluster_namespace_count, Some(14));
        let seeded_at = seeded.cluster_checked_at.expect("a stamped check time");

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    ObservationOutcome::Failed("this build has no Kubernetes client".to_owned()),
                    HealthOutcome::NotAttempted,
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(
            row.cluster_status.as_deref(),
            Some("Healthy"),
            "NotAttempted must not overwrite a real reading with Unreachable"
        );
        assert_eq!(row.cluster_status_message, None);
        assert_eq!(
            row.cluster_nodes, seeded.cluster_nodes,
            "NotAttempted is not a clearer: nothing looked, so nothing changed"
        );
        assert_eq!(row.cluster_namespace_count, Some(14));
        assert_eq!(
            row.cluster_checked_at,
            Some(seeded_at),
            "the check time still belongs to the read that actually happened"
        );
    }
}
