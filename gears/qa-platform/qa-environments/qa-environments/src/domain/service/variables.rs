use std::sync::Arc;

use toolkit_macros::domain_model;
use tracing::{debug, info, instrument, warn};

use crate::domain::error::DomainError;
use crate::domain::repos::{EnvironmentsRepository, VariablesRepository};
use crate::domain::service::DbProvider;
use authz_resolver_sdk::PolicyEnforcer;

use super::{actions, resources};
use qa_environments_sdk::{NewVariable, RESERVED_VARIABLE_NAMES, Variable};
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

const MAX_NAME_LEN: usize = 255;
const MAX_VALUE_BYTES: usize = 64 * 1024;

/// Pipeline (global) and per-environment variable service.
#[domain_model]
pub struct VariablesService<V: VariablesRepository, P: EnvironmentsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<V>,
    environments_repo: Arc<P>,
    policy_enforcer: PolicyEnforcer,
    /// Cap on the number of variables `list_for_env` returns (see
    /// `QaEnvironmentsConfig::max_variables`). Applied after merging pipeline
    /// and environment variables — see `list_for_env`.
    max_variables: usize,
}

impl<V: VariablesRepository, P: EnvironmentsRepository> VariablesService<V, P> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<V>,
        environments_repo: Arc<P>,
        policy_enforcer: PolicyEnforcer,
        max_variables: usize,
    ) -> Self {
        Self {
            db,
            repo,
            environments_repo,
            policy_enforcer,
            max_variables,
        }
    }
}

// Business logic methods
impl<V: VariablesRepository, P: EnvironmentsRepository> VariablesService<V, P> {
    /// One page of the pipeline (global) variables, plus an environment's
    /// variables when `environment_id` is given. Both variable reads share the
    /// same VARIABLE PEP scope; the environment existence precheck uses its own
    /// PLATFORM scope.
    ///
    /// # The scope is resolved before the filter
    ///
    /// As in [`EnvironmentsService::list_environments`](super::EnvironmentsService::list_environments):
    /// the `AccessScope` comes from the PDP here and the repository composes the
    /// caller's `OData` query on top of it. A `$filter` narrows; it cannot widen.
    ///
    /// # What is paged, and what is bounded-but-not-cursored
    ///
    /// **This response is the union of two tables**, and that decides the
    /// paging contract, which is not uniform across the two cases:
    ///
    /// * **`environment_id` absent** — one table (`qa_pipeline_variables`), so
    ///   the page is an ordinary cursor page: `next_cursor` is real and
    ///   resuming from it walks the whole collection.
    /// * **`environment_id` present** — two tables. `toolkit-db`'s pager is a
    ///   single-entity pager: its cursor encodes the sort-key values of one
    ///   table's last row, and there is no room in `CursorV1` for a segment
    ///   discriminator, so a cursor handed back for the second table would be
    ///   re-applied to the first on the follow-up request and either duplicate
    ///   or skip rows. Hand-rolling a cross-table cursor is exactly the class of
    ///   code the toolkit's pager exists to remove. So in this case the union is
    ///   **bounded, not cursored** — pipeline first, then the environment's own
    ///   rows filling whatever remains — and `next_cursor` is `None`. Callers
    ///   narrow with `$filter` (or with `environment_id` itself). A `cursor`
    ///   sent *with* `environment_id` is refused rather than honoured; see
    ///   below.
    ///
    /// ## The union's bound is `max_variables`, not a page
    ///
    /// **Corrected 2026-09-07, Task 24 review finding 1.** The first version of
    /// this method bounded the union at `page_info.limit`, which for any caller
    /// not passing `limit=` is `PAGE_LIMITS.default` — **200**, where the old
    /// unpaged code returned everything and then `truncate(max_variables)`, i.e.
    /// **500** (`config.rs`'s default and the shipped YAML both say 500). That
    /// is a 300-row drop in the reachable set, and because the union fills
    /// pipeline-first the rows lost at the boundary were exactly the
    /// **environment-specific** ones — the tier that *overrides* pipeline
    /// (`qa-runs`' `domain::runvars::split_by_scope`). The caller that pays for
    /// it is not a settings screen: `qa-runs`' `dispatch_spec` assembles a run's
    /// variables from this, so a tenant with 200+ combined variables would have
    /// launched runs with the pipeline default where an environment override
    /// existed — a silent wrong-value launch, not a display defect. The union
    /// therefore asks for `max_variables` rows, and a caller's own smaller
    /// `limit` still narrows it.
    ///
    /// The pager still clamps that request to `PAGE_LIMITS.max` (500), which is
    /// exactly `max_variables`' shipped default, so at the shipped configuration
    /// the reachable set is *identical* to the pre-paging one. A deployment that
    /// raises `max_variables` **above** 500 would find the union capped at 500;
    /// that is detected and warned about below rather than left to be
    /// rediscovered.
    ///
    /// Bounding pipeline-first preserves the precedence the previous
    /// `vars.truncate(max_variables)` had and documented: a full result drops
    /// environment-specific variables before it ever drops a pipeline one.
    ///
    /// Review finding #55: before this, both halves were
    /// `find().secure().scope_with(scope).all()` with no limit, and the only
    /// bound was that silent `truncate`.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] on `cursor` when a cursor is sent together
    /// with `environment_id` — see the guard at the top of the body for why that
    /// combination cannot be answered correctly. [`DomainError::Forbidden`] when
    /// the caller may not list variables, `EnvironmentNotFound` for an
    /// environment they cannot see, or [`DomainError::Database`].
    #[instrument(skip(self, ctx, query))]
    pub async fn list_for_env(
        &self,
        ctx: &SecurityContext,
        environment_id: Option<Uuid>,
        query: &ODataQuery,
    ) -> Result<Page<Variable>, DomainError> {
        debug!("Listing variables for environment");

        // **A cursor cannot address a union, so it is refused rather than
        // half-applied.** Task 24 review finding 2: this method used to hand
        // `query.clone()` to the second read, and `clone()` carries
        // `query.cursor`. `paginate_odata_collect` applies a keyset predicate to
        // whatever entity it is given, so a `name`-keyed token minted against
        // `qa_pipeline_variables` silently dropped every environment variable
        // whose name sorted at or before it. Nothing rejected the combination on
        // the way in either -- the extractor only refuses `cursor` + `$orderby`,
        // the tiebreaker is `name` on both tables so the sort-token check
        // passed, and the filter hash matched -- so the result was missing rows
        // with `next_cursor: null`, which is precisely the shape a caller cannot
        // detect. Reachable as
        // `GET /qa/v1/variables?environment_id=...&cursor=<token from the same
        // endpoint without environment_id>`.
        //
        // Refusing is what this endpoint's own OpenAPI description already
        // promises ("cursor pagination applies only when `environment_id` is
        // absent"), and a 400 naming `cursor` is a fact the caller can act on.
        if environment_id.is_some() && query.cursor.is_some() {
            return Err(DomainError::Validation {
                field: "cursor".to_owned(),
                message: "a cursor cannot be combined with environment_id: that response is \
                          the union of two tables and a single-table cursor cannot address \
                          it. Page the pipeline variables without environment_id, or narrow \
                          with $filter."
                    .to_owned(),
            });
        }

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::VARIABLE, actions::LIST, None)
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // In the union case the bound is the deployment's `max_variables`, not
        // a page -- see this method's header for the 200-vs-500 regression that
        // correction is about. A caller's own `limit` still narrows it; it
        // cannot widen past the operator's cap.
        let pipeline_query = if environment_id.is_some() {
            let union_limit = u64::try_from(self.max_variables).unwrap_or(u64::MAX);
            query
                .clone()
                .with_limit(query.limit.map_or(union_limit, |top| top.min(union_limit)))
        } else {
            query.clone()
        };

        let mut page = self
            .repo
            .list_pipeline_page(&conn, &scope, &pipeline_query)
            .await?;

        if let Some(environment_id) = environment_id {
            // Tenancy precheck: the environment must exist within the caller's
            // scope. PLATFORM is a different resource type/table than
            // VARIABLE, so this read needs its own PEP-derived scope rather
            // than reusing the VARIABLE/LIST `scope` above.
            let environment_scope = self
                .policy_enforcer
                .access_scope(
                    ctx,
                    &resources::PLATFORM,
                    actions::GET,
                    Some(environment_id),
                )
                .await?;
            self.environments_repo
                .get(&conn, &environment_scope, environment_id)
                .await?
                .ok_or(DomainError::EnvironmentNotFound { id: environment_id })?;

            // Whatever the first half left of the union. `page_info.limit` is
            // the limit `paginate_odata` actually applied after its own clamp,
            // so the arithmetic follows the caller's `limit`, `max_variables`
            // and `PAGE_LIMITS` without this method re-deriving any of them.
            //
            // It is also how the `PAGE_LIMITS.max` clamp becomes visible: a
            // deployment whose `max_variables` exceeds the pager's ceiling gets
            // a smaller union than it configured, and says so once per call
            // rather than being discovered as missing run variables.
            let requested = pipeline_query.limit.unwrap_or(page.page_info.limit);
            if page.page_info.limit < requested {
                warn!(
                    configured = self.max_variables,
                    applied = page.page_info.limit,
                    "max_variables exceeds the storage page ceiling; the variables union is \
                     capped below the configured value"
                );
            }

            let remaining = page
                .page_info
                .limit
                .saturating_sub(page.items.len() as u64);

            // A pipeline half that already filled the union leaves no room, and
            // asking for a limit of 0 would be clamped back up to 1 by
            // `clamp_limit` -- so the read is skipped rather than issued.
            if remaining > 0 {
                // `cursor` explicitly cleared as well as bounded. The guard at
                // the top of this method already makes a cursor unreachable
                // here, and this is the belt to that pair of braces: the failure
                // mode it guards is *silently* missing rows, so it is worth two
                // lines to make the second read structurally incapable of
                // inheriting a first-table keyset predicate.
                let mut environment_query = pipeline_query.clone();
                environment_query.cursor = None;
                environment_query.limit = Some(remaining);

                let environment_page = self
                    .repo
                    .list_for_environment_page(&conn, &scope, environment_id, &environment_query)
                    .await?;
                page.items.extend(environment_page.items);
            }

            // See this method's header: a union cannot carry a single-table
            // cursor. Cleared rather than left as the pipeline half's, which
            // would resume the pipeline table and silently re-serve rows the
            // caller has already had.
            page.page_info.next_cursor = None;
            page.page_info.prev_cursor = None;
        }

        // The configured `max_variables` (default 500) still applies on top of
        // the page limit, because it is a *deployment* setting rather than a
        // request one and may be set lower than a page. Same precedence as
        // before: pipeline variables are first in `items`, so a truncation here
        // drops environment-specific variables before it ever drops a pipeline
        // variable.
        //
        // **And when it bites, the cursor goes with it.** `paginate_odata` built
        // that cursor from the *untruncated* page's last row, so leaving it
        // would hand the caller a resume point past rows this method just
        // dropped -- silently skipping them, which is worse than the truncation
        // itself. A deployment cap is not a page boundary: it is the operator
        // saying no caller sees more than N variables, and there is nothing
        // beyond it to resume to.
        if page.items.len() > self.max_variables {
            page.items.truncate(self.max_variables);
            page.page_info.next_cursor = None;
            page.page_info.prev_cursor = None;
        }

        debug!("Successfully listed {} variables", page.items.len());
        Ok(page)
    }

    /// Insert or update a variable. `var.environment_id == None` targets the
    /// pipeline (global) table; `Some(_)` targets a specific environment.
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

        // Tenancy precheck FIRST: a foreign/cross-tenant environment_id must
        // 404 before the natural-key probe below ever runs. Doing this after
        // the probe would let a caller learn whether a variable exists on an
        // environment_id it can't even see. PLATFORM is a different resource
        // type/table than VARIABLE, so this read needs its own PEP-derived
        // scope rather than reusing the VARIABLE scope derived below.
        if let Some(environment_id) = var.environment_id {
            let environment_scope = self
                .policy_enforcer
                .access_scope(
                    ctx,
                    &resources::PLATFORM,
                    actions::GET,
                    Some(environment_id),
                )
                .await?;
            self.environments_repo
                .get(&conn, &environment_scope, environment_id)
                .await?
                .ok_or(DomainError::EnvironmentNotFound { id: environment_id })?;
        }

        // Determine whether this write is a CREATE or an UPDATE *before*
        // requesting the CREATE/UPDATE PEP scope: authorizing a CREATE for
        // what turns out to be an update (or vice versa) would request the
        // wrong action. The natural-key probe itself is NOT unscoped — no
        // code path in this crate is allowed to query with
        // `AccessScope::allow_all()` off of unauthenticated/unauthorized
        // input. It uses a real PEP-derived VARIABLE/GET scope, and the
        // per-environment lookup is additionally bound to the caller's own
        // `tenant_id` in the repo (see `find_environment_var`), so the probe
        // can only ever observe rows the caller is authorized to see.
        let probe_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::VARIABLE, actions::GET, None)
            .await?;
        let existing = self
            .repo
            .find_by_natural_key(
                &conn,
                &probe_scope,
                tenant_id,
                var.environment_id,
                &var.name,
            )
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
