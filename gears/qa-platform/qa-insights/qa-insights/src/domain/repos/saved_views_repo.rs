//! Stored analytics filter sets.

use async_trait::async_trait;
use qa_insights_sdk::{NewSavedView, SavedView, SavedViewScope};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::analytics::PlanRef;
use crate::domain::error::DomainError;

/// The tuple `idx_qa_analytics_saved_views_unique` keys on, minus `plan_key`.
///
/// `plan_key` is absent on purpose: the implementation derives it from
/// [`Self::plan`], and accepting a caller-supplied one would let a probe
/// disagree with what a write would produce — which is precisely the failure
/// mode obligation #2 is about.
///
/// **The one borrowed type in this module, and the choice is deliberate.**
/// Every other supporting type here owns its fields, because every other one is
/// *stored*: [`NewLogEntry`](super::NewLogEntry) and
/// [`NotificationClaim`](super::NotificationClaim) become rows. This one is a
/// read probe that lives for the duration of one call and is never retained, so
/// borrowing costs the caller nothing and saves cloning a name and a path on a
/// lookup that happens before every saved-view write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedViewKey<'a> {
    pub owner_id: Uuid,
    pub scope: SavedViewScope,
    /// `Some` exactly when [`Self::scope`] is [`SavedViewScope::Plan`].
    pub plan: Option<&'a PlanRef>,
    pub name: &'a str,
}

/// Persistence for `qa_analytics_saved_views`.
///
/// # Every method here has to keep `plan_key` true
///
/// This is obligation #2 of the schema, and the only silent-correctness failure
/// mode in it. Legacy's uniqueness is a *functional* index over
/// `COALESCE(plan_id, '')` (`001_initial.sql:194-195`); this schema materializes
/// the coalesced value as a column because `SQLite` and `MySQL` do not both
/// support functional indexes, and **nothing in the database checks that the
/// column agrees with `repo_id`/`plan_path`**. A writer that forgets it gets the
/// `''` default, which silently collides a plan-scoped view with the owner's
/// global view of the same name.
///
/// The rule, for [`create`](SavedViewsRepository::create) and
/// [`update`](SavedViewsRepository::update) alike: `""` when the plan identity
/// is absent, otherwise `"<repo_id>/<plan_path>"`.
///
/// # What the coalesce actually buys
///
/// Not "a global and a plan-scoped view of the same name may coexist" — those
/// two rows already differ in `scope`. The load-bearing effect is the opposite
/// one: in SQL a `NULL` never equals another `NULL`, so *without* the coalesce
/// one owner could create unlimited global views all called "My View". The
/// coalesce collapses every global view onto the key `""` and makes them
/// collide, which is what [`DomainError::SavedViewNameExists`] reports.
#[async_trait]
pub trait SavedViewsRepository: Send + Sync {
    /// The caller's views at one scope.
    ///
    /// `plan` is required when `scope` is [`SavedViewScope::Plan`] and must be
    /// `None` otherwise — legacy 400s on a plan-scoped request with no plan
    /// (`manager/src/routes/analytics.rs:530-540`). The two arguments are
    /// separate rather than one `Option<PlanRef>` because the scope is also
    /// stored and filtered on: a plan-scoped list must not return global views.
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        scope: SavedViewScope,
        plan: Option<&PlanRef>,
    ) -> Result<Vec<SavedView>, DomainError>;

    /// Store a new view owned by `owner_id`.
    ///
    /// The owner comes from the request identity and never from the body, which
    /// is why it is a parameter and not a field of [`NewSavedView`] — legacy
    /// takes it from a header and this port takes it from `SecurityContext`.
    ///
    /// A collision on `idx_qa_analytics_saved_views_unique` is
    /// [`DomainError::SavedViewNameExists`], not a `Database` error.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        tenant_id: Uuid,
        owner_id: Uuid,
        view: NewSavedView,
    ) -> Result<SavedView, DomainError>;

    /// Replace an existing view's scope, plan, name and query.
    ///
    /// Rewrites `plan_key` too — see this trait's header. An update that moves
    /// a view between scopes and leaves the old key behind is the failure that
    /// obligation is about.
    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        id: Uuid,
        view: NewSavedView,
    ) -> Result<Option<SavedView>, DomainError>;

    /// Delete a view. `false` when no row matched the caller's scope.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Look up a view by the tuple its unique index keys on.
    ///
    /// Used by the service *before* it decides whether an upsert is a create or
    /// an update, so the right action can be authorized. Callers must pass a
    /// real PEP-derived scope, tenant-bound to the requesting caller — never
    /// `AccessScope::allow_all()`, which this crate's production code
    /// constructs at exactly one call site
    /// (`domain::elevated::enumeration_scope`, the nil-tenant ticker
    /// enumeration) and this is not it — because this probe's result drives the
    /// next authorization decision; and
    /// implementations must bind the lookup to `tenant_id` as well as to the
    /// scope, so it cannot be used as a cross-tenant existence oracle.
    ///
    /// The key is the index's own columns minus `plan_key`; see
    /// [`SavedViewKey`].
    async fn find_by_natural_key<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        tenant_id: Uuid,
        key: SavedViewKey<'_>,
    ) -> Result<Option<SavedView>, DomainError>;
}
