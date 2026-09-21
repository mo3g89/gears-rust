use async_trait::async_trait;
use qa_environments_sdk::LeaseState;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{LeasesRepository, VersionedLease};
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::environment_lease::{
    self, Column as LeaseColumn, Entity as LeaseEntity,
};
use crate::infra::storage::mapper::{lease_to_state, state_to_columns};

/// ORM-based implementation of the `LeasesRepository` trait.
#[derive(Clone, Default)]
pub struct OrmLeasesRepository;

#[async_trait]
impl LeasesRepository for OrmLeasesRepository {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        environment_id: Uuid,
    ) -> Result<VersionedLease, DomainError> {
        let found = LeaseEntity::find()
            .filter(sea_orm::Condition::all().add(LeaseColumn::EnvironmentId.eq(environment_id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        match found {
            Some(m) => Ok(VersionedLease {
                state: lease_to_state(&m)?,
                version: m.version,
                freed_at: m.freed_at,
            }),
            // Missing row: no lease has ever been written for this environment.
            // This is a legitimate Free state, not corruption. It is also an
            // environment nothing has ever freed, so it carries no anchor —
            // `None` rather than a clock read, which would date the free
            // transition to whenever this read happened to run.
            None => Ok(VersionedLease {
                state: LeaseState::Free,
                version: 0,
                freed_at: None,
            }),
        }
    }

    async fn compare_and_set<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        environment_id: Uuid,
        expected_version: i64,
        new_state: &LeaseState,
    ) -> Result<(), DomainError> {
        let (mode, holders) = state_to_columns(new_state);
        let now = OffsetDateTime::now_utc();

        // **The anchor.** A `Free` write is a transition to free and nothing
        // else: the service writes only when the state actually changes, and
        // no acquisition can produce `Free` — `domain::lease`'s
        // `no_acquisition_can_produce_a_free_state` pins that invariant, which
        // is what lets this be an unconditional test on the new state rather
        // than a flag the caller has to remember to pass.
        //
        // Every other write leaves the column alone. It is deliberately *not*
        // cleared on acquisition: a held row keeps the instant its current
        // holder consumed, which costs nothing and means a reader never sees a
        // half-written anchor.
        let freed_at = matches!(new_state, LeaseState::Free).then_some(now);

        if expected_version == 0 {
            // No row expected yet — insert. A peer racing us between the
            // caller's read and this insert loses on the primary-key
            // unique-violation and is surfaced as a lease conflict so it
            // retries the read-decide-write loop.
            let am = environment_lease::ActiveModel {
                environment_id: ActiveValue::Set(environment_id),
                tenant_id: ActiveValue::Set(tenant_id),
                mode: ActiveValue::Set(mode),
                holders: ActiveValue::Set(holders),
                version: ActiveValue::Set(1),
                updated_at: ActiveValue::Set(now),
                freed_at: ActiveValue::Set(freed_at),
            };

            return match secure_insert::<LeaseEntity>(am, scope, runner).await {
                Ok(_) => Ok(()),
                Err(e) if e.is_unique_violation() => Err(DomainError::LeaseConflict),
                Err(e) => Err(db_err(e)),
            };
        }

        let mut update = LeaseEntity::update_many()
            .filter(
                sea_orm::Condition::all()
                    .add(LeaseColumn::EnvironmentId.eq(environment_id))
                    .add(LeaseColumn::Version.eq(expected_version)),
            )
            .secure()
            .scope_with(scope)
            .col_expr(LeaseColumn::Mode, Expr::value(mode))
            .col_expr(LeaseColumn::Holders, Expr::value(holders))
            .col_expr(LeaseColumn::Version, Expr::value(expected_version + 1))
            .col_expr(LeaseColumn::UpdatedAt, Expr::value(now));
        if let Some(at) = freed_at {
            update = update.col_expr(LeaseColumn::FreedAt, Expr::value(at));
        }

        let result = update.exec(runner).await.map_err(db_err)?;

        if result.rows_affected == 0 {
            return Err(DomainError::LeaseConflict);
        }
        Ok(())
    }
}
