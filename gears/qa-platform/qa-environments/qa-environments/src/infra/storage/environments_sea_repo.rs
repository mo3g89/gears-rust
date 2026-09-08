use async_trait::async_trait;
use qa_environments_sdk::{Environment, EnvironmentPatch, NewEnvironment};
// The plugin contract's observation types. Its `ObservationOutcome` and
// `HealthOutcome` collide by name with this gear's own
// (`crate::domain::ports`), and they are different types with different
// shapes -- the local `Failed` carries a `String`, the SDK's a
// `PluginFailure`. So the SDK's two are aliased rather than shadowing:
// `record_observation` speaks the plugin contract, `create` and `update`
// still speak the local one, and no reader of this file has to guess which
// `Failed` a match arm means.
use qa_product_sdk::observation::{
    HealthOutcome as PluginHealthOutcome, HealthState,
    ObservationOutcome as PluginObservationOutcome, ObservedAttrs,
};
use sea_orm::sea_query::Expr;
use sea_orm::sea_query::Func;
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::paginate_odata;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
    secure_update_with_scope,
};
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use tracing::warn;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::observation_write::ObservationWrite;
use crate::domain::repos::{EnvironmentsRepository, PersistedCredentials};
use crate::infra::storage::db::{PAGE_LIMITS, db_err, odata_err};
use crate::infra::storage::entity::environment::{
    self, ActiveModel as EnvironmentAM, Column as EnvironmentColumn, Entity as EnvironmentEntity,
};
use crate::infra::storage::mapper::{credentials_to_json, environment_to_sdk};
use crate::infra::storage::odata::{
    EnvironmentFilterField, EnvironmentODataMapper, NAME_TIEBREAKER,
};

/// ORM-based implementation of the `EnvironmentsRepository` trait.
#[derive(Clone, Default)]
pub struct OrmEnvironmentsRepository;

#[async_trait]
impl EnvironmentsRepository for OrmEnvironmentsRepository {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Environment>, DomainError> {
        let found = EnvironmentEntity::find()
            .filter(sea_orm::Condition::all().add(EnvironmentColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(found.map(environment_to_sdk))
    }

    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Environment>, DomainError> {
        // `.secure().scope_with(scope)` before `paginate_odata`, and not
        // optionally: that function's first parameter is
        // `SecureSelect<E, Scoped>`, so an unscoped select does not type-check
        // and the caller's `$filter` is applied on top of the tenant predicate
        // rather than in place of it.
        let scoped = EnvironmentEntity::find().secure().scope_with(scope);

        paginate_odata::<EnvironmentFilterField, EnvironmentODataMapper, _, _, _, _>(
            scoped,
            runner,
            query,
            // `name` ascending, not `created_at` descending as qa-runs uses:
            // `idx_qa_environments_tenant_name (tenant_id, name)` makes this an
            // exact index prefix once the scope has pinned the tenant, and
            // `name` is unique per tenant so a single-key cursor is total. See
            // `infra::storage::odata`'s header.
            NAME_TIEBREAKER,
            PAGE_LIMITS,
            environment_to_sdk,
        )
        .await
        .map_err(|error| odata_err(&error))
    }

    async fn list_all_with_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<(Environment, Uuid)>, DomainError> {
        // **Unbounded by design, and the one read on this trait that is.**
        //
        // Review finding #55 is about a client-facing collection read with no
        // page; `list_page` three declarations above is where it is closed, and
        // that method's doc says so. This is the deliberate exception, recorded
        // here rather than left as an unremarked `.all()` that reads like the
        // defect the finding named:
        //
        // * **No client can ask for it.** It is not reachable from any route.
        //   The sole caller is the background observation ticker's per-cycle
        //   sweep (`EnvironmentsService::run_observation_cycle`), which is this
        //   gear's own maintenance duty; a request cannot reach it, so there is
        //   no page size for a caller to omit and no query to widen.
        // * **The caller needs every row.** The sweep exists to observe each
        //   environment once per cycle. Paging it would mean either a cursor
        //   held across ticks -- with rows created or deleted between them
        //   silently skipped or re-observed -- or a cap, which would leave
        //   environments past it never observed at all. Completeness is the
        //   whole contract, the same argument `local_client`'s `drain_pages`
        //   makes for the SDK reads.
        // * **It is still scoped.** `allow_all` is a value of `AccessScope`,
        //   not a bypass: the read goes through `.secure().scope_with(scope)`
        //   like every other one here, and the tenant travels back with each
        //   row so the sweep can mint a tenant-bound context per environment.
        //   The trait's own doc carries that reasoning at length.
        //
        // The bound that does exist is deployment scale: `cpt-cf-qa-nfr-scale`'s
        // first number is 100 platforms, which is the size of this sweep.
        let rows = EnvironmentEntity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let tenant_id = row.tenant_id;
                (environment_to_sdk(row), tenant_id)
            })
            .collect())
    }

    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewEnvironment,
        credentials: PersistedCredentials,
    ) -> Result<Environment, DomainError> {
        let now = OffsetDateTime::now_utc();
        let environment = Environment {
            id: Uuid::new_v4(),
            name: new.name,
            product_id: new.product_id,
            description: new.description,
            available: true,
            observed_version: None,
            observed_build: None,
            default_branch: new.default_branch,
            is_default: new.is_default,
            // A freshly created environment has never been observed: every
            // column `record_observation` writes starts out `None`,
            // mirroring the `ActiveModel` below.
            version_detect_error: None,
            version_detected_at: None,
            // The plugin-shaped half, added by Task 14. Every one of these is
            // at the value its column defaults to, and for the same reason the
            // legacy half above is `None`: nothing has observed this
            // environment yet.
            //
            // `credentials` and `config` are the exceptions, and they are
            // where Task 18b changed this method: both now arrive already
            // resolved in `PersistedCredentials`, because both are credential
            // facts rather than observations.
            //
            // Until Task 18b this literal was `Vec::new()` with a long comment
            // saying why: writing `[{"key": "kubeconfig", ...}]` would have put
            // one product's credential key in the gear whose whole purpose is
            // to stop naming VHP, and it would be *wrong* for the first
            // non-Kubernetes product. That reasoning was right and the answer
            // was not to guess the key here -- it is that the product's own
            // plugin classifies its fields
            // (`QaProductPluginV1::validate_credentials` ->
            // `CredentialClassification`) and the service hands the result
            // over. Nothing in this file names a credential key.
            credentials: credentials.credentials.clone(),
            observed_attrs: ObservedAttrs::default(),
            config: credentials.config.clone(),
            observed_base_url: None,
            health_state: HealthState::Unknown,
            health_detail: None,
            health_checked_at: None,
            created_at: now,
            updated_at: now,
        };

        let am = EnvironmentAM {
            id: ActiveValue::Set(environment.id),
            tenant_id: ActiveValue::Set(tenant_id),
            name: ActiveValue::Set(environment.name.clone()),
            product_id: ActiveValue::Set(environment.product_id),
            description: ActiveValue::Set(environment.description.clone()),
            available: ActiveValue::Set(environment.available),
            observed_version: ActiveValue::Set(environment.observed_version.clone()),
            observed_build: ActiveValue::Set(environment.observed_build.clone()),
            default_branch: ActiveValue::Set(environment.default_branch.clone()),
            is_default: ActiveValue::Set(environment.is_default),
            // A freshly created environment has never been observed, so every
            // column `record_observation` writes starts out NULL.
            version_detect_error: ActiveValue::Set(None),
            version_detected_at: ActiveValue::Set(None),
            // Set rather than `NotSet`, even though all four `NOT NULL`
            // columns among them have a matching database `DEFAULT`: `create`
            // returns the `Environment` it built rather than re-reading the
            // row, so the two have to be written from the same values or they
            // could drift. `credentials` is serialised from the very same
            // `Vec` the returned `Environment` carries, for that reason.
            credentials: ActiveValue::Set(credentials_to_json(&credentials.credentials)),
            observed_attrs: ActiveValue::Set(serde_json::json!({})),
            config: ActiveValue::Set(credentials.config),
            observed_base_url: ActiveValue::Set(None),
            health_state: ActiveValue::Set(HealthState::Unknown.as_str().to_owned()),
            health_detail: ActiveValue::Set(None),
            health_checked_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(environment.created_at),
            updated_at: ActiveValue::Set(environment.updated_at),
        };

        match secure_insert::<EnvironmentEntity>(am, scope, runner).await {
            Ok(_) => Ok(environment),
            Err(e) if e.is_unique_violation() => Err(DomainError::EnvironmentNameExists {
                name: environment.name,
            }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        patch: EnvironmentPatch,
        credentials: Option<PersistedCredentials>,
    ) -> Result<Option<Environment>, DomainError> {
        let existing = EnvironmentEntity::find()
            .filter(sea_orm::Condition::all().add(EnvironmentColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        let Some(existing) = existing else {
            return Ok(None);
        };

        let attempted_name = patch.name.clone().unwrap_or_else(|| existing.name.clone());

        let mut am: EnvironmentAM = environment::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            name: ActiveValue::Unchanged(existing.name.clone()),
            product_id: ActiveValue::Unchanged(existing.product_id),
            description: ActiveValue::Unchanged(existing.description.clone()),
            available: ActiveValue::Unchanged(existing.available),
            observed_version: ActiveValue::Unchanged(existing.observed_version.clone()),
            observed_build: ActiveValue::Unchanged(existing.observed_build.clone()),
            default_branch: ActiveValue::Unchanged(existing.default_branch.clone()),
            is_default: ActiveValue::Unchanged(existing.is_default),
            // Fourteen of these sixteen are observation-only columns written
            // solely by `record_observation`, and no `EnvironmentPatch` field
            // reaches any of them, so an operator-driven `update()` leaves
            // them exactly as they were.
            //
            // `credentials` and `config` are the two exceptions, and neither
            // is observation-derived. Both were `Unchanged` here until Task
            // 18b, because nothing could write them: re-deriving `credentials`
            // needs a credential *key*, and taking one from a literal would
            // name one product's credential in the gear whose whole purpose is
            // to stop doing that (ruling D-9).
            //
            // Task 18b answered that without guessing: the product's own
            // plugin classifies its submitted fields, and the service hands
            // the resolved result down as `PersistedCredentials`. So they are
            // still `Unchanged` on the branch below where the patch mentioned
            // no credential -- which is most patches -- and `Set` from that
            // value where it did. Nothing in this file names a credential key.
            //
            // The value is the **complete** post-update state rather than a
            // delta, because a JSON array cannot be partially assigned; the
            // service does the merge, where the previous row and the plugin
            // are both in hand.
            version_detect_error: ActiveValue::Unchanged(existing.version_detect_error.clone()),
            version_detected_at: ActiveValue::Unchanged(existing.version_detected_at),
            credentials: ActiveValue::Unchanged(existing.credentials.clone()),
            observed_attrs: ActiveValue::Unchanged(existing.observed_attrs.clone()),
            config: ActiveValue::Unchanged(existing.config.clone()),
            observed_base_url: ActiveValue::Unchanged(existing.observed_base_url.clone()),
            health_state: ActiveValue::Unchanged(existing.health_state.clone()),
            health_detail: ActiveValue::Unchanged(existing.health_detail.clone()),
            health_checked_at: ActiveValue::Unchanged(existing.health_checked_at),
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
        // Task 18b: the credential columns move together or not at all. The
        // legacy column is written from the same resolved value as the other
        // two (`PersistedCredentials::legacy_ref`) rather than from
        // `patch.kubeconfig_credstore_ref`, so the dual-write cannot drift --
        // the patch's own credential fields are ignored here for
        // `EnvironmentsRepository::create`'s reason.
        if let Some(credentials) = credentials {
            am.credentials = ActiveValue::Set(credentials_to_json(&credentials.credentials));
            am.config = ActiveValue::Set(credentials.config);
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
        // second row, and `EnvironmentsService` owns the rule; see
        // `m20260831_000009_platform_is_default`.
        if let Some(is_default) = patch.is_default {
            am.is_default = ActiveValue::Set(is_default);
        }

        match secure_update_with_scope::<EnvironmentEntity>(am, scope, id, runner).await {
            Ok(model) => Ok(Some(environment_to_sdk(model))),
            Err(e) if e.is_unique_violation() => Err(DomainError::EnvironmentNameExists {
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
        let result = EnvironmentEntity::update_many()
            .col_expr(EnvironmentColumn::IsDefault, Expr::value(false))
            .filter(
                sea_orm::Condition::all()
                    .add(EnvironmentColumn::ProductId.eq(product_id))
                    .add(EnvironmentColumn::Id.ne(except_id)),
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
        let result = EnvironmentEntity::delete_many()
            .filter(sea_orm::Condition::all().add(EnvironmentColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        Ok(result.rows_affected > 0)
    }

    /// Persist one observation attempt.
    ///
    /// Every merge rule, and why each is what it is, is tabulated on the trait
    /// method (`EnvironmentsRepository::record_observation`). What follows is
    /// only what is true of *this* implementation.
    ///
    /// ## One column set since Task 19, and this doc said otherwise
    ///
    /// Through Phases D and E this method dual-wrote: a legacy half
    /// (`vhp_base_url`, the five `cluster_*` columns) beside a plugin-shaped
    /// half, with three paragraphs here about keeping the two from drifting.
    /// `m20260903_000012` dropped all eight of those columns, and the method
    /// body names none of them. The paragraphs went with them.
    ///
    /// ## One `UPDATE`, chained
    ///
    /// Every projection contributes `col_expr`s to the same statement, so an
    /// observation is applied atomically or not at all — a crash cannot leave
    /// `health_state` fresh next to a stale `observed_version`, which is the
    /// failure mode two statements would have. `Func::coalesce` is ordinary
    /// ANSI SQL, so the statement is identical across Postgres, `MySQL` and
    /// `SQLite`; no dialect branch is needed here, unlike the migrations that
    /// add these columns.
    ///
    /// `updated_at` is deliberately **not** touched: it tracks operator edits
    /// via `update()`, and a background observation is not one — bumping it
    /// here would make an unattended poller look like continuous manual
    /// editing to anything sorting or auditing on that column.
    ///
    /// ## What is never written here
    ///
    /// `credentials` and `config` are credential facts, not observations: one
    /// is written by the create path (and, for rows that predate the plugin
    /// path, by `m20260903_000011`'s backfill), the other only by that same
    /// backfill so far. An observation must not touch either, and this method
    /// names neither column.
    ///
    /// # Errors
    ///
    /// `DomainError::EnvironmentNotFound` if no row matching `id` is visible
    /// under `scope` (covers both "no such environment" and "not in scope").
    async fn record_observation<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        observation: &ObservationWrite,
    ) -> Result<(), DomainError> {
        let now = OffsetDateTime::now_utc();

        let update = EnvironmentEntity::update_many()
            .filter(sea_orm::Condition::all().add(EnvironmentColumn::Id.eq(id)))
            .secure()
            .scope_with(scope);

        let update = match observation.environment() {
            PluginObservationOutcome::Detected(_) => {
                let roles = observation.roles();
                // `ObservedAttrs` is a `BTreeMap<String, String>`, so this
                // cannot fail today. It is matched rather than defaulted
                // because the alternative wrote an empty attribute map
                // *together with* a `Checked` status -- recording "we looked
                // and saw nothing" for what is actually "we could not
                // serialize what we saw". If the map's value type ever widens,
                // this must skip the health write, not invent one.
                // Review finding #29.
                //
                // This early `return Ok(())` also exits before the health-half
                // match below and before the `rows_affected == 0` check at the
                // end of this method, so a write against a nonexistent
                // environment that also hit this (currently unreachable)
                // branch would report success instead of
                // `EnvironmentNotFound`. That is what this brief's own
                // suggested fix does; noted rather than changed, since
                // reordering it is a different, un-asked-for change in
                // control flow. Review finding #29 (minor).
                let Some(attrs) = attrs_or_skip(serde_json::to_value(observation.attrs()), id)
                else {
                    return Ok(());
                };
                update
                    // Target is authoritative: overwrite outright, even to NULL.
                    .col_expr(
                        EnvironmentColumn::ObservedVersion,
                        Expr::value(roles.version.clone()),
                    )
                    .col_expr(
                        EnvironmentColumn::ObservedBuild,
                        Expr::value(roles.build.clone()),
                    )
                    // A conclusive detection (Some) wins; an inconclusive one
                    // (None, i.e. SQL NULL) falls through to the stored value.
                    //
                    // **E-23 is closed here rather than carried.** The dropped
                    // `observed_namespace` was the mirror of this rule with
                    // its arguments the other way round -- sticky, where
                    // `observed_attrs` is overwritten -- so a redeployed
                    // environment ran in its new namespace and displayed the
                    // old one. Task 19 dropped the column, and the namespace
                    // now has exactly one home: `observed_attrs`, surfaced
                    // through `FieldRole::Namespace`.
                    .col_expr(
                        EnvironmentColumn::ObservedBaseUrl,
                        Func::coalesce([
                            Expr::value(roles.base_url.clone()),
                            Expr::col(EnvironmentColumn::ObservedBaseUrl),
                        ])
                        .into(),
                    )
                    // The declared attributes, and only those:
                    // `ObservationWrite`'s constructor has already dropped
                    // everything the plugin did not declare, which is what
                    // keeps an echoed credential out of this column and off
                    // `EnvironmentDto`.
                    .col_expr(EnvironmentColumn::ObservedAttrs, Expr::value(attrs))
                    .col_expr(
                        EnvironmentColumn::VersionDetectError,
                        Expr::value(None::<String>),
                    )
                    .col_expr(EnvironmentColumn::VersionDetectedAt, Expr::value(Some(now)))
            }
            PluginObservationOutcome::Failed(failure) => update
                // observed_version / observed_build / observed_base_url /
                // observed_attrs are deliberately absent from this branch:
                // stale-but-known beats blank, so a failed attempt leaves them
                // untouched. (`vhp_base_url` was in this list until Task 19
                // dropped the column.)
                .col_expr(
                    EnvironmentColumn::VersionDetectError,
                    Expr::value(Some(failure.to_string())),
                )
                .col_expr(EnvironmentColumn::VersionDetectedAt, Expr::value(Some(now))),
        };

        let update = match observation.health() {
            PluginHealthOutcome::Checked { state, detail } => update
                .col_expr(
                    EnvironmentColumn::HealthState,
                    Expr::value(state.as_str().to_owned()),
                )
                .col_expr(
                    EnvironmentColumn::HealthDetail,
                    Expr::value(detail.map(ToOwned::to_owned)),
                )
                .col_expr(EnvironmentColumn::HealthCheckedAt, Expr::value(Some(now))),
            PluginHealthOutcome::Failed(failure) => {
                let detail = failure.to_string();
                update
                    // `unknown`, not `down`: the read failed, so nothing is
                    // known about the target. `down` would assert a verdict
                    // nobody reached -- the same distinction the legacy pair
                    // draws between `Unreachable` and `Unhealthy`, and the
                    // same one `m20260903_000011`'s backfill made.
                    .col_expr(
                        EnvironmentColumn::HealthState,
                        Expr::value(HealthState::Unknown.as_str().to_owned()),
                    )
                    .col_expr(EnvironmentColumn::HealthDetail, Expr::value(Some(detail)))
                    .col_expr(EnvironmentColumn::HealthCheckedAt, Expr::value(Some(now)))
            }
            // No read was attempted, so none of the health columns is named in
            // the UPDATE at all -- not even a checked-at, in either set,
            // because nothing checked. The row keeps whatever it already held;
            // a row that was never checked stays never-checked, which is what
            // makes the UI fall back to the reachability dot (D-CH-6) instead
            // of rendering a manufactured status. This is NOT the Failed arm
            // with the writes omitted: it must never clear a reading either,
            // so a build or a product that cannot look can neither invent nor
            // erase what an earlier one saw.
            PluginHealthOutcome::NotAttempted => update,
        };

        let result = update.exec(runner).await.map_err(db_err)?;
        if result.rows_affected == 0 {
            return Err(DomainError::EnvironmentNotFound { id });
        }
        Ok(())
    }
}

/// Turn an observed-attribute serialization outcome into either the value to
/// write, or the signal that [`OrmEnvironmentsRepository::record_observation`]
/// must skip the whole health write instead of recording an empty one.
///
/// # Why this is a free function rather than inline in the match arm
///
/// `record_observation`'s own call site can never produce the `Err` this
/// matches: `ObservedAttrs` is a `BTreeMap<String, String>`, and
/// `serde_json::to_value` on one cannot fail. Pulling the decision out into a
/// function that takes the *outcome* of that serialization, rather than the
/// value to serialize, is what lets
/// [`a_serialization_failure_skips_the_write_and_warns`] drive the skip path
/// with a real `serde_json::Error` — the one a genuinely malformed document
/// produces — instead of the call site's always-succeeds input. Review
/// finding #29.
fn attrs_or_skip(
    result: Result<serde_json::Value, serde_json::Error>,
    id: Uuid,
) -> Option<serde_json::Value> {
    match result {
        Ok(attrs) => Some(attrs),
        Err(error) => {
            warn!(
                environment_id = %id,
                %error,
                "qa-environments: observed attributes could not be serialized; skipping this \
                 health write rather than recording an empty one"
            );
            None
        }
    }
}

/// Tests for `record_observation`'s five merge rules (three for the version
/// half, two for the cluster-health half).
///
/// # Why these assert on the raw entity, not `Environment`
///
/// `Environment` (the SDK contract type `environment_to_sdk` maps onto)
/// carries none of the columns this method writes — exposing them through
/// the SDK model is a later task's concern, not this one's. So every
/// assertion here reads the row back via `EnvironmentEntity::find()` directly
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
    use qa_environments_sdk::NewEnvironment;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::{DBRunner, SecureEntityExt};
    use toolkit_security::AccessScope;
    use uuid::Uuid;

    use qa_product_sdk::observation::{
        FailureClass, HealthState, ObservedAttrs, PluginFailure, PluginObservation,
    };

    use crate::test_support::CapturedLogs;

    use super::{
        EnvironmentColumn, EnvironmentEntity, ObservationWrite, PluginHealthOutcome,
        PluginObservationOutcome, attrs_or_skip,
    };
    use crate::domain::repos::{EnvironmentsRepository, PersistedCredentials};
    use crate::infra::storage::entity::environment;
    use crate::test_support::inmem_db;

    fn new_environment(name: &str) -> NewEnvironment {
        NewEnvironment {
            credentials: std::collections::BTreeMap::new(),
            kubeconfig_credstore_ref: None,
            name: name.to_owned(),
            // Required since Task 20b.
            product_id: Uuid::from_u128(0x9001),
            description: None,
            kubeconfig: None,
            default_branch: None,
            is_default: false,
        }
    }

    /// The observed schema these fixtures declare, claiming all four roles --
    /// the shape `qa-vhp-product-plugin`'s own `observed_schema()` has. The
    /// merge rules under test are driven entirely by the *projections*, so the
    /// schema is what decides which attribute reaches which column.
    fn schema() -> Vec<qa_product_sdk::descriptor::FieldDesc> {
        use qa_product_sdk::descriptor::{FieldDesc, FieldKind, FieldRole};
        let observed = |key: &str, role: FieldRole| FieldDesc {
            key: key.to_owned(),
            label: key.to_owned(),
            kind: FieldKind::Text,
            required: false,
            role: Some(role),
            in_table: false,
            in_detail: true,
            help: None,
        };
        vec![
            observed("platformVersion", FieldRole::Version),
            observed("build", FieldRole::Build),
            observed("baseDomain", FieldRole::BaseUrl),
            observed("namespace", FieldRole::Namespace),
        ]
    }

    /// Build a `Detected` outcome from the four role-claimed attributes.
    ///
    /// `base_domain: None` is how a real inconclusive gateway read looks (the
    /// plugin omits the attribute entirely rather than sending a blank), not
    /// an error -- callers below pass it explicitly to exercise that case.
    /// `https://` is prepended here because the plugin's `baseDomain`
    /// attribute carries a full URL: it is the plugin that composes the
    /// scheme now, and the platform stores the projection verbatim.
    fn detected(
        version: &str,
        build: Option<&str>,
        namespace: &str,
        base_domain: Option<&str>,
    ) -> PluginObservationOutcome {
        let mut attrs = ObservedAttrs::default();
        attrs.set("platformVersion", version);
        if let Some(build) = build {
            attrs.set("build", build);
        }
        attrs.set("namespace", namespace);
        if let Some(domain) = base_domain {
            attrs.set("baseDomain", format!("https://{domain}"));
        }
        PluginObservationOutcome::Detected(attrs)
    }

    /// Wrap the two halves for `record_observation`'s parameter, through the
    /// constructor that applies `retain_declared` and `project_roles`.
    fn observation(
        environment: PluginObservationOutcome,
        health: PluginHealthOutcome,
    ) -> ObservationWrite {
        ObservationWrite::new(
            &schema(),
            PluginObservation {
                environment,
                health,
            },
        )
    }

    /// A healthy `Checked` outcome, carrying legacy's own status spelling in
    /// `detail` exactly as `qa-plugin-k8s` does.
    ///
    /// It takes no node count and no namespace count, and that is the
    /// signature change worth noticing: the plugin contract keeps only the
    /// verdict, so nothing reaching `record_observation` carries a node list
    /// any more. What used to be seeded through this helper is now seeded
    /// directly on the row by [`seed_legacy_node_list`], which is also a
    /// truer model of Phase D -- those rows were written by the old observer.
    const fn checked_healthy() -> PluginHealthOutcome {
        PluginHealthOutcome::Checked {
            state: HealthState::Ok,
            detail: Some("Healthy"),
        }
    }

    /// A failed environment half, classified, carrying the remote's own text
    /// in the one field sanctioned to hold it.
    ///
    /// `version_detect_error` therefore reads `<fixed detail>: <remote text>`
    /// (`PluginFailure`'s `Display`), where it used to hold the observer's
    /// single string. The remote half is what an operator acts on -- "namespaces
    /// \"virtuozzo\" not found" is the example the observer's own header
    /// argues makes a broken environment fixable -- and the fixed half is what
    /// classifies it.
    fn environment_failed(remote: &str) -> PluginObservationOutcome {
        PluginObservationOutcome::Failed(
            PluginFailure::classified(FailureClass::NotFound, "the target could not be read")
                .with_remote_message(remote.to_owned()),
        )
    }

    /// A failed health read, classified.
    fn health_failed(remote: &str) -> PluginHealthOutcome {
        PluginHealthOutcome::Failed(
            PluginFailure::classified(FailureClass::Unreachable, "the target could not be reached")
                .with_remote_message(remote.to_owned()),
        )
    }

    /// Read the row back through the exact same secure, scoped query `get()`
    /// runs, but without `environment_to_sdk`'s mapping step -- which is what
    /// would drop the four columns under test.
    async fn fetch_row(conn: &impl DBRunner, scope: &AccessScope, id: Uuid) -> environment::Model {
        EnvironmentEntity::find()
            .filter(sea_orm::Condition::all().add(EnvironmentColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .unwrap()
            .expect("the environment row must read back")
    }

    /// Seed a fresh environment (every observation column `NULL`) and return its
    /// id, so each test starts from the same known-empty baseline.
    async fn seed_environment(
        conn: &impl DBRunner,
        scope: &AccessScope,
        tenant: Uuid,
        name: &str,
    ) -> Uuid {
        super::OrmEnvironmentsRepository
            .create(
                conn,
                scope,
                tenant,
                new_environment(name),
                PersistedCredentials {
                    legacy_ref: "credstore://test".to_owned(),
                    credentials: Vec::new(),
                    config: serde_json::json!({}),
                },
            )
            .await
            .unwrap()
            .id
    }

    /// `observed_version` / `observed_build`: the plugin's reading is
    /// authoritative, so a later detection overwrites a former one outright --
    /// no COALESCE, no "keep what's set". This is the rule `observed_base_url`
    /// below is NOT: that column yields only to a *conclusive* detection.
    /// (`observed_namespace`, the other half of the old contrast, was dropped
    /// with the legacy columns by Task 19 -- re-review, N-8.)
    #[tokio::test]
    async fn a_successful_detection_overwrites_version_and_build() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_environment(&conn, &scope, tenant, "p1").await;

        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.4", Some("9"), "virtuozzo", None),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();
        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", None),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.observed_version.as_deref(), Some("26.5"));
        assert_eq!(row.observed_build.as_deref(), Some("0"));
    }

    // Six tests were deleted here by Task 19, with the columns they were
    // about: `a_set_namespace_survives_detection_but_an_unset_one_is_filled`
    // (the sticky `observed_namespace` rule -- finding E-23, discharged by the
    // drop rather than fixed, because the namespace now has exactly one home
    // in `observed_attrs`), `a_failed_health_read_clears_the_nodes_rather_
    // than_keeping_them`, `a_freshly_created_environment_has_no_cluster_health_
    // recorded`, `a_successful_health_read_clears_a_previous_unreachable_
    // message`, `a_not_attempted_health_outcome_leaves_a_never_checked_
    // environment_untouched` and `a_not_attempted_health_outcome_preserves_an_
    // earlier_successful_read`.
    //
    // What replaced their properties, so none is silently lost: the health
    // half is now `health_state`/`health_detail`/`health_checked_at`, and
    // `a_failed_health_read_leaves_the_observed_version_alone` and
    // `a_failed_version_detection_leaves_a_successful_health_read_alone`
    // (both kept, both rewritten onto those columns) are what hold the
    // independence rule the deleted three were about. The node inventory has
    // no replacement **on purpose** -- user decision U4.

    /// `observed_base_url`: the STORED value must yield, and only to a
    /// CONCLUSIVE detection; an inconclusive one (`base_domain: None`, how a
    /// missing/unreadable/too-few-hostnames gateway read looks) must leave a
    /// working URL alone rather than blank it. Inverting the rule -- i.e.
    /// `COALESCE(observed_base_url, $detected)`, which is what the deleted
    /// `observed_namespace` column used -- would make the second half of this
    /// test fail exactly the opposite way: the URL would be overwritten by the
    /// inconclusive `NULL` instead of preserved. (Named `vhp_base_url` until
    /// Task 19 dropped that column -- re-review, N-8.)
    #[tokio::test]
    async fn a_conclusive_base_url_wins_and_an_inconclusive_one_preserves() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);

        // Half 1: a conclusive detection overwrites a stored URL.
        let overwritten_id = seed_environment(&conn, &scope, tenant, "overwritten").await;
        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                overwritten_id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", Some("old.example.com")),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();
        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                overwritten_id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, overwritten_id).await;
        assert_eq!(
            row.observed_base_url.as_deref(),
            Some("https://sv.jele.io"),
            "a conclusive detection must overwrite a previously stored URL"
        );

        // Half 2: an inconclusive detection must NOT blank a stored URL.
        let preserved_id = seed_environment(&conn, &scope, tenant, "preserved").await;
        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                preserved_id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();
        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                preserved_id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", None),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, preserved_id).await;
        assert_eq!(
            row.observed_base_url.as_deref(),
            Some("https://sv.jele.io"),
            "an inconclusive detection (base_domain: None) must NOT erase a \
             working URL -- this is the rule an unreadable gateway ConfigMap \
             must never be allowed to violate. Task 19 dropped `vhp_base_url`, \
             which carried the same rule; this column is where it lives now"
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
        let id = seed_environment(&conn, &scope, tenant, "p1").await;

        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", None),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();
        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    environment_failed("namespaces \"virtuozzo\" not found"),
                    checked_healthy(),
                ),
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
            Some("the target could not be read: namespaces \"virtuozzo\" not found")
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
        let id = seed_environment(&conn, &scope, tenant, "p1").await;

        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    environment_failed("namespaces \"virtuozzo\" not found"),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, id).await;
        assert!(
            row.version_detect_error.is_some(),
            "the error must be set first"
        );

        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", None),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();
        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(
            row.version_detect_error, None,
            "a successful detection must clear a previously recorded error"
        );
    }

    /// An environment that does not exist (or is out of scope) must error rather
    /// than silently succeed on zero affected rows.
    #[tokio::test]
    async fn observing_a_missing_environment_is_an_error() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);

        let err = super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                Uuid::new_v4(),
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", None),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            crate::domain::error::DomainError::EnvironmentNotFound { .. }
        ));
    }

    /// D-CH-4: the halves are independent, so a failed health read must not
    /// disturb a version that was detected in the same call.
    #[tokio::test]
    async fn a_failed_health_read_leaves_the_observed_version_alone() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_environment(&conn, &scope, tenant, "p1").await;

        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();
        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")),
                    health_failed("cannot list nodes"),
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.observed_version.as_deref(), Some("26.5"));
        assert_eq!(row.observed_build.as_deref(), Some("0"));
        assert_eq!(row.observed_base_url.as_deref(), Some("https://sv.jele.io"));
        assert!(row.version_detect_error.is_none());
        assert_eq!(
            row.health_state, "unknown",
            "the read failed, so nothing is known -- `down` would assert a \
             verdict nobody reached"
        );
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
        let id = seed_environment(&conn, &scope, tenant, "p1").await;

        super::OrmEnvironmentsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    environment_failed("namespaces \"virtuozzo\" not found"),
                    checked_healthy(),
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(
            row.version_detect_error.as_deref(),
            Some("the target could not be read: namespaces \"virtuozzo\" not found")
        );
        assert_eq!(
            row.health_state, "ok",
            "the health half succeeded and must be stored -- this is the only \
             health column since Task 19 dropped the legacy pair"
        );
        assert_eq!(row.health_detail.as_deref(), Some("Healthy"));
    }

    /// **The skip path, exercised with a real `serde_json::Error`.** Review
    /// finding #29, and its own follow-up: the first version of this test
    /// asserted only `outcome.is_none()`, leaving the `_and_warns` half of
    /// its name unverified -- exactly the gap #28 disclosed and #29 did not.
    /// It now drives the `warn!` too, via [`crate::test_support::CapturedLogs`]
    /// (moved there from `environments_kubeconfig_tests` for this reuse).
    ///
    /// `record_observation`'s own call site can never produce this `Err`:
    /// `ObservedAttrs` is a `BTreeMap<String, String>`, and
    /// `serde_json::to_value` on one cannot fail. So this cannot be TDD'd the
    /// ordinary way -- there is no way to make the *call site* red first --
    /// and this test does not pretend otherwise. What it does honestly is
    /// drive [`attrs_or_skip`] directly with the `serde_json::Error` a
    /// genuinely malformed document produces (from a real failed parse, not a
    /// fabricated stand-in), and pin that the decision function returns
    /// `None` rather than `Some(json!({}))` -- the fix for the trap this
    /// finding is about, closed before the map's value type could ever widen
    /// enough to reach it for real.
    ///
    /// No `#[tokio::test]` needed: `attrs_or_skip` is synchronous, so a plain
    /// thread-local `tracing::subscriber::set_default` guard already covers
    /// the one call made while it is held -- there is no `.await` for the
    /// guard to need to survive. The other hazard the harness's doc comment
    /// warns about (global per-callsite `Interest` caching) does not apply
    /// either: this `warn!` callsite lives only in `attrs_or_skip`'s `Err`
    /// arm, which no other test in this crate reaches, so no other test can
    /// have cached it as `never` first. Confirmed empirically, not just
    /// argued -- see the fix report for both the solo and full-suite runs.
    #[test]
    fn a_serialization_failure_skips_the_write_and_warns() {
        let malformed = serde_json::from_str::<serde_json::Value>("{not json")
            .expect_err("deliberately malformed, to get a real serde_json::Error");

        let logs = CapturedLogs::default();
        let outcome = {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_max_level(tracing::Level::WARN)
                .with_ansi(false)
                .finish();
            let _guard = tracing::subscriber::set_default(subscriber);
            attrs_or_skip(Err(malformed), Uuid::from_u128(0x0E))
        };

        assert!(
            outcome.is_none(),
            "a serialization failure must skip the write, not answer an empty attribute map"
        );
        assert!(
            logs.text().contains("skipping this health write"),
            "a serialization failure must be logged, not silent; captured: {}",
            logs.text()
        );
    }

    /// The ordinary path: a value that serialized without issue is passed
    /// through unchanged.
    #[test]
    fn a_successful_serialization_is_passed_through() {
        let value = serde_json::json!({"namespace": "virtuozzo"});
        let outcome = attrs_or_skip(Ok(value.clone()), Uuid::from_u128(0x0E));
        assert_eq!(outcome, Some(value));
    }
}
