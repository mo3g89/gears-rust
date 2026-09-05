use async_trait::async_trait;
use qa_environments_sdk::LeaseState;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, EntityTrait, QueryFilter};
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
            .filter(
                sea_orm::Condition::all()
                    .add(Expr::col(LeaseColumn::EnvironmentId).eq(environment_id)),
            )
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        match found {
            Some(m) => Ok(VersionedLease {
                state: lease_to_state(&m)?,
                version: m.version,
            }),
            // Missing row: no lease has ever been written for this environment.
            // This is a legitimate Free state, not corruption.
            None => Ok(VersionedLease {
                state: LeaseState::Free,
                version: 0,
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
            };

            return match secure_insert::<LeaseEntity>(am, scope, runner).await {
                Ok(_) => Ok(()),
                Err(e) if e.is_unique_violation() => Err(DomainError::LeaseConflict),
                Err(e) => Err(db_err(e)),
            };
        }

        let result = LeaseEntity::update_many()
            .filter(
                sea_orm::Condition::all()
                    .add(Expr::col(LeaseColumn::EnvironmentId).eq(environment_id))
                    .add(Expr::col(LeaseColumn::Version).eq(expected_version)),
            )
            .secure()
            .scope_with(scope)
            .col_expr(LeaseColumn::Mode, Expr::value(mode))
            .col_expr(LeaseColumn::Holders, Expr::value(holders))
            .col_expr(LeaseColumn::Version, Expr::value(expected_version + 1))
            .col_expr(LeaseColumn::UpdatedAt, Expr::value(now))
            .exec(runner)
            .await
            .map_err(db_err)?;

        if result.rows_affected == 0 {
            return Err(DomainError::LeaseConflict);
        }
        Ok(())
    }
}
