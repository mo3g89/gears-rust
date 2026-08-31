use std::sync::Arc;

use toolkit_macros::domain_model;
use tracing::{debug, info, instrument};

use crate::domain::error::DomainError;
use crate::domain::repos::{PlatformsRepository, VariablesRepository};
use crate::domain::service::DbProvider;
use authz_resolver_sdk::PolicyEnforcer;

use super::{actions, resources};
use qa_environments_sdk::{NewVariable, RESERVED_VARIABLE_NAMES, Variable};
use toolkit_security::SecurityContext;
use uuid::Uuid;

const MAX_NAME_LEN: usize = 255;
const MAX_VALUE_BYTES: usize = 64 * 1024;

/// Pipeline (global) and per-platform variable service.
#[domain_model]
pub struct VariablesService<V: VariablesRepository, P: PlatformsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<V>,
    platforms_repo: Arc<P>,
    policy_enforcer: PolicyEnforcer,
    /// Cap on the number of variables `list_for_env` returns (see
    /// `QaEnvironmentsConfig::max_variables`). Applied after merging pipeline
    /// and platform variables — see `list_for_env`.
    max_variables: usize,
}

impl<V: VariablesRepository, P: PlatformsRepository> VariablesService<V, P> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<V>,
        platforms_repo: Arc<P>,
        policy_enforcer: PolicyEnforcer,
        max_variables: usize,
    ) -> Self {
        Self {
            db,
            repo,
            platforms_repo,
            policy_enforcer,
            max_variables,
        }
    }
}

// Business logic methods
impl<V: VariablesRepository, P: PlatformsRepository> VariablesService<V, P> {
    /// List all pipeline (global) variables, plus platform variables when
    /// `platform_id` is given. Both variable reads share the same VARIABLE
    /// PEP scope; the platform existence precheck uses its own PLATFORM scope.
    #[instrument(skip(self, ctx))]
    pub async fn list_for_env(
        &self,
        ctx: &SecurityContext,
        platform_id: Option<Uuid>,
    ) -> Result<Vec<Variable>, DomainError> {
        debug!("Listing variables for environment");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::VARIABLE, actions::LIST, None)
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let mut vars = self.repo.list_pipeline(&conn, &scope).await?;

        if let Some(platform_id) = platform_id {
            // Tenancy precheck: the platform must exist within the caller's
            // scope. PLATFORM is a different resource type/table than
            // VARIABLE, so this read needs its own PEP-derived scope rather
            // than reusing the VARIABLE/LIST `scope` above.
            let platform_scope = self
                .policy_enforcer
                .access_scope(ctx, &resources::PLATFORM, actions::GET, Some(platform_id))
                .await?;
            self.platforms_repo
                .get(&conn, &platform_scope, platform_id)
                .await?
                .ok_or(DomainError::PlatformNotFound { id: platform_id })?;

            let platform_vars = self
                .repo
                .list_for_platform(&conn, &scope, platform_id)
                .await?;
            vars.extend(platform_vars);
        }

        // Cap the merged result at `max_variables`. Pipeline (global)
        // variables are appended first and platform variables second, so
        // truncating here always drops platform-specific variables before
        // ever dropping a pipeline variable when the combined set exceeds
        // the configured limit.
        vars.truncate(self.max_variables);

        debug!("Successfully listed {} variables", vars.len());
        Ok(vars)
    }

    /// Insert or update a variable. `var.platform_id == None` targets the
    /// pipeline (global) table; `Some(_)` targets a specific platform.
    #[instrument(skip(self, ctx, var), fields(name = %var.name))]
    pub async fn upsert(
        &self,
        ctx: &SecurityContext,
        var: NewVariable,
    ) -> Result<Variable, DomainError> {
        info!("Upserting variable");

        Self::validate_name(&var.name)?;
        Self::validate_value(&var.value)?;

        let conn = self.db.conn().map_err(DomainError::from)?;
        let tenant_id = ctx.subject_tenant_id();

        // Tenancy precheck FIRST: a foreign/cross-tenant platform_id must
        // 404 before the natural-key probe below ever runs. Doing this after
        // the probe would let a caller learn whether a variable exists on a
        // platform_id it can't even see. PLATFORM is a different resource
        // type/table than VARIABLE, so this read needs its own PEP-derived
        // scope rather than reusing the VARIABLE scope derived below.
        if let Some(platform_id) = var.platform_id {
            let platform_scope = self
                .policy_enforcer
                .access_scope(ctx, &resources::PLATFORM, actions::GET, Some(platform_id))
                .await?;
            self.platforms_repo
                .get(&conn, &platform_scope, platform_id)
                .await?
                .ok_or(DomainError::PlatformNotFound { id: platform_id })?;
        }

        // Determine whether this write is a CREATE or an UPDATE *before*
        // requesting the CREATE/UPDATE PEP scope: authorizing a CREATE for
        // what turns out to be an update (or vice versa) would request the
        // wrong action. The natural-key probe itself is NOT unscoped — no
        // code path in this crate is allowed to query with
        // `AccessScope::allow_all()` off of unauthenticated/unauthorized
        // input. It uses a real PEP-derived VARIABLE/GET scope, and the
        // per-platform lookup is additionally bound to the caller's own
        // `tenant_id` in the repo (see `find_platform_var`), so the probe
        // can only ever observe rows the caller is authorized to see.
        let probe_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::VARIABLE, actions::GET, None)
            .await?;
        let existing = self
            .repo
            .find_by_natural_key(&conn, &probe_scope, tenant_id, var.platform_id, &var.name)
            .await?;

        let (action, resource_id) = match &existing {
            Some(existing) => (actions::UPDATE, Some(existing.id)),
            None => (actions::CREATE, None),
        };

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::VARIABLE, action, resource_id)
            .await?;

        let variable = self.repo.upsert(&conn, &scope, tenant_id, var).await?;

        info!("Successfully upserted variable with id={}", variable.id);
        Ok(variable)
    }

    #[instrument(skip(self, ctx), fields(variable_id = %id))]
    pub async fn delete(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        info!("Deleting variable");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::VARIABLE, actions::DELETE, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let deleted = self.repo.delete(&conn, &scope, id).await?;

        if !deleted {
            return Err(DomainError::VariableNotFound { id });
        }

        info!("Successfully deleted variable");
        Ok(())
    }

    /// Validate a variable name: shape (`^[A-Za-z_][A-Za-z0-9_]*$`), length,
    /// and the refusal of names the test runner owns
    /// ([`RESERVED_VARIABLE_NAMES`], added 2026-08-13 by Task 11b).
    ///
    /// Implemented with `chars()` rather than a `regex` dependency — the
    /// charset rule is a trivial first-char/rest-chars check.
    ///
    /// The length cap measures `str::len`, i.e. **bytes**, while the message it
    /// produces says "characters". The two agree for every name that survives
    /// the charset check, which is ASCII-only, and can differ only for input
    /// that is rejected anyway. The user-facing wording is left as it was
    /// rather than changed in passing; the discrepancy is noted so the next
    /// reader does not have to re-derive it.
    ///
    /// The checks run in the source system's order — empty, then charset, then
    /// reserved (`../testrunner/manager/src/routes/settings.rs:63-88`) — but
    /// **the order is unobservable here, and no test can pin it**. Every
    /// reserved name is itself shape-valid, so "malformed *and* reserved" is an
    /// empty set and either order produces the same message for every input.
    ///
    /// Recorded rather than left implicit because the obvious ordering test
    /// cannot fail: an earlier revision of this doc claimed the order decided
    /// which message a malformed reserved name got, and shipped a test using
    /// `"RP API KEY"` — a name that is not reserved at all, since spaces are
    /// not underscores. Moving the reserved check to the top left it green
    /// (found 2026-08-13 by this task's own break-test). What *is* pinned, by
    /// `upsert_refuses_every_reserved_name_on_both_paths` asserting the message
    /// names the offending variable, is that every reserved name reaches this
    /// refusal rather than being swallowed by the charset check — which is the
    /// property that makes the order moot. If a reserved name that is not
    /// shape-valid is ever added, that test fails and the order starts to
    /// matter.
    fn validate_name(name: &str) -> Result<(), DomainError> {
        let invalid = || DomainError::Validation {
            field: "name".to_owned(),
            message: "must match ^[A-Za-z_][A-Za-z0-9_]*$ and be at most 255 characters".to_owned(),
        };

        if name.is_empty() || name.len() > MAX_NAME_LEN {
            return Err(invalid());
        }

        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return Err(invalid());
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return Err(invalid());
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(invalid());
        }

        Self::validate_not_reserved(name)?;

        Ok(())
    }

    /// Refuse a name the test runner owns
    /// ([`RESERVED_VARIABLE_NAMES`] — read that constant for why this matters
    /// beyond tidiness; the short version is that `RP_API_KEY` reaches the
    /// runner as a secret *reference*, and a variable of that name would
    /// replace it with operator-supplied text).
    ///
    /// **Case-insensitive**, matching the source system's
    /// `eq_ignore_ascii_case` sweep (`routes/settings.rs:80-88`). Environment
    /// lookup in the runner's shell is case-sensitive, so `rp_api_key` would
    /// not in fact shadow anything — but the *intent* to shadow a control
    /// variable is what is refused, and the lowercase spelling expresses it
    /// just as well. qa-runs refuses run parameters on identical reasoning
    /// (`domain::params::validate`), and the two must not drift apart.
    ///
    /// Rejected at the write rather than filtered at assembly: the operator
    /// gets an error naming the variable they just typed, instead of a value
    /// that silently never reaches a run. The message leads with the noun —
    /// "variable '{name}' is reserved …" — matching the source system's
    /// "Variable '{}' is reserved …" (`routes/settings.rs:84-88`), because
    /// qa-runs raises a near-identical message for run *parameters* and an
    /// operator reading a log needs to know which of the two refused them.
    fn validate_not_reserved(name: &str) -> Result<(), DomainError> {
        if RESERVED_VARIABLE_NAMES
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(name))
        {
            return Err(DomainError::Validation {
                field: "name".to_owned(),
                message: format!(
                    "variable '{name}' is reserved by the test runner and cannot be overridden"
                ),
            });
        }
        Ok(())
    }

    fn validate_value(value: &str) -> Result<(), DomainError> {
        if value.len() > MAX_VALUE_BYTES {
            return Err(DomainError::Validation {
                field: "value".to_owned(),
                message: format!("must not exceed {MAX_VALUE_BYTES} bytes"),
            });
        }
        Ok(())
    }
}
