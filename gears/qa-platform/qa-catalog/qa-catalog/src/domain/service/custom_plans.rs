//! User-composed persisted plan (custom plan) service.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_catalog_sdk::{CustomPlan, NewCustomPlan};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, info, instrument};
use uuid::Uuid;

use super::plans::validate_rel_path;
use super::validation::validate_name;
use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::repos::CustomPlansRepository;

/// Custom plan service.
#[domain_model]
pub struct CustomPlansService<C: CustomPlansRepository> {
    db: Arc<DbProvider>,
    repo: Arc<C>,
    policy_enforcer: PolicyEnforcer,
}

impl<C: CustomPlansRepository> CustomPlansService<C> {
    pub fn new(db: Arc<DbProvider>, repo: Arc<C>, policy_enforcer: PolicyEnforcer) -> Self {
        Self {
            db,
            repo,
            policy_enforcer,
        }
    }
}

// Business logic methods
impl<C: CustomPlansRepository> CustomPlansService<C> {
    #[instrument(skip(self, ctx))]
    pub async fn list_custom_plans(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<CustomPlan>, DomainError> {
        debug!("Listing custom plans");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::CUSTOM_PLAN, actions::LIST, None)
            .await?;

        let conn = self.db.conn()?;
        self.repo.list(&conn, &scope).await
    }

    #[instrument(skip(self, ctx), fields(custom_plan_id = %id))]
    pub async fn get_custom_plan(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<CustomPlan, DomainError> {
        debug!("Getting custom plan by id");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::CUSTOM_PLAN, actions::GET, Some(id))
            .await?;

        let conn = self.db.conn()?;
        self.repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::NotFound { id })
    }

    #[instrument(skip(self, ctx, new), fields(name = %new.name))]
    pub async fn create_custom_plan(
        &self,
        ctx: &SecurityContext,
        new: NewCustomPlan,
    ) -> Result<CustomPlan, DomainError> {
        info!("Creating custom plan");

        validate_new_custom_plan(&new)?;

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::CUSTOM_PLAN, actions::CREATE, None)
            .await?;

        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();

        let plan = self.repo.create(&conn, &scope, tenant_id, new).await?;

        info!("Successfully created custom plan with id={}", plan.id);
        Ok(plan)
    }

    /// Full-document update, matching `QaCatalogClientV1::update_custom_plan`.
    #[instrument(skip(self, ctx, new), fields(custom_plan_id = %id))]
    pub async fn update_custom_plan(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        new: NewCustomPlan,
    ) -> Result<CustomPlan, DomainError> {
        info!("Updating custom plan");

        validate_new_custom_plan(&new)?;

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::CUSTOM_PLAN, actions::UPDATE, Some(id))
            .await?;

        let conn = self.db.conn()?;
        let updated = self
            .repo
            .update(&conn, &scope, id, new)
            .await?
            .ok_or(DomainError::NotFound { id })?;

        info!("Successfully updated custom plan");
        Ok(updated)
    }

    #[instrument(skip(self, ctx), fields(custom_plan_id = %id))]
    pub async fn delete_custom_plan(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        info!("Deleting custom plan");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::CUSTOM_PLAN, actions::DELETE, Some(id))
            .await?;

        let conn = self.db.conn()?;
        let deleted = self.repo.delete(&conn, &scope, id).await?;
        if !deleted {
            return Err(DomainError::NotFound { id });
        }

        info!("Successfully deleted custom plan");
        Ok(())
    }
}

fn validate_new_custom_plan(new: &NewCustomPlan) -> Result<(), DomainError> {
    validate_name("name", &new.name)?;
    // File paths are later joined to repository content roots (bundle
    // builds), so traversal is rejected at write time as well as read time.
    for entry in &new.files {
        validate_rel_path("files", &entry.path)?;
        // `plan_path` is joined to a content root by the same rule, on the
        // *read* side: qa-runs hands it to `get_plan`, which resolves it under
        // the repository's checkout. It is operator-supplied and every bit as
        // untrusted as `path`.
        //
        // Its *presence* is enforced by the type
        // (`qa_catalog_sdk::NewCustomPlanEntry::plan_path` is a `String`), so
        // this call is about the content. `validate_rel_path` rejects the empty
        // string first (`plans.rs:281-283`), which is what stops `""` from
        // satisfying a mandatory field.
        validate_rel_path("files.plan_path", &entry.plan_path)?;
    }
    Ok(())
}
