use async_trait::async_trait;
use qa_environments_sdk::{Environment, EnvironmentCredential, EnvironmentPatch, NewEnvironment};
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::observation_write::ObservationWrite;

/// The credential half of a write, after the service has resolved every
/// submitted credential to a credstore reference.
///
/// # Why this type exists rather than more parameters
///
/// [`NewEnvironment::credentials`] and [`EnvironmentPatch::credentials`] carry
/// `CredentialSubmission`, whose `Material` arm is **plaintext**. Both structs
/// are passed to this trait, so without a resolved type in between, the
/// repository layer is one field access away from a credential document — and
/// the guard would be a comment ("must not travel any further towards the
/// repository layer") that someone has to keep true.
///
/// This type has no arm that can hold material, so the boundary is structural.
/// The service strips the submissions before calling, exactly as it already
/// cleared `new.kubeconfig`/`new.kubeconfig_credstore_ref`, and hands the
/// resolved references over here instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedCredentials {
    /// The reference belonging to the plugin's sole required secret field, or
    /// the empty string when the plugin declares no single required secret.
    ///
    /// It was the dual-write for the pre-plugin `kubeconfig_credstore_ref`
    /// column, which `qa-runs`' dispatch and `resolve_credential_slots` read
    /// until Task 19. Task 19 dropped that column together with both readers'
    /// fallback to it, and no repository persists this field any more:
    /// `credentials` below is the only stored source. It was always a lossy
    /// projection of `credentials` (one column, one reference), which is why
    /// Task 19 re-derived `credentials` from the column only for rows holding
    /// at most one credential (review finding CRITICAL-1).
    pub legacy_ref: String,
    /// The `credentials` column: one `{key, credstore_ref}` per secret the
    /// plugin classified. **Never a value** — the column is structurally
    /// incapable of holding plaintext, which is the point after the
    /// 2026-08-28 leak.
    pub credentials: Vec<EnvironmentCredential>,
    /// The `config` column: the submitted fields the plugin classified as
    /// **not** secret, merged over whatever the column already held. Operator-
    /// set and non-secret, which is why it is a column of its own rather than
    /// merged into `observed_attrs` — the environment page has to be able to
    /// say which of two values a human may correct.
    pub config: serde_json::Value,
}

/// Repository trait for `Environment` persistence operations.
#[async_trait]
pub trait EnvironmentsRepository: Send + Sync {
    /// Find an environment by ID within the given security scope.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Environment>, DomainError>;

    /// The tenant that owns environment `id`, read off its row within `scope`;
    /// `Ok(None)` when no such row is visible there.
    ///
    /// The SDK `Environment` model carries no `tenant_id`, so this is how a
    /// request path learns the tenant the observation cycle binds its system
    /// context to ([`Self::list_all_with_tenant`]'s pairing). Reading an
    /// environment's credential under that one tenant is what makes a refresh,
    /// a create or an update and the cycle read it alike, whichever tenant the
    /// request's caller belongs to.
    async fn owner_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Uuid>, DomainError>;

    /// One page of the environments visible within `scope`, ordered by name,
    /// with the caller's `OData` `$filter`/`$orderby`/`$top`/cursor applied on
    /// top of the scope.
    ///
    /// **This replaced an unbounded `find().secure().scope_with(scope).all()`**
    /// — review finding #55. `cpt-cf-qa-nfr-scale`'s first number is *100
    /// platforms*, and this is the collection that number is about, so this
    /// gear ignoring the bound while two sibling gears enforced it was the
    /// exact inversion the finding names.
    ///
    /// The `scope` is applied **before** the filter and cannot be widened by
    /// one: `paginate_odata`'s first parameter is
    /// `SecureSelect<E, Scoped>`, so an unscoped select does not type-check,
    /// and the caller's `$filter` is `AND`ed onto the tenant predicate rather
    /// than substituted for it. A `$filter` naming a field outside
    /// [`EnvironmentFilterField`](crate::infra::storage::odata::EnvironmentFilterField)
    /// is a [`DomainError::Validation`] (HTTP 400), not a scan.
    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Environment>, DomainError>;

    /// Every environment visible within `scope`, paired with the tenant that
    /// owns it. The sole caller is the background observation ticker's
    /// per-cycle sweep (`EnvironmentsService::run_observation_cycle`), which
    /// passes [`toolkit_security::AccessScope::allow_all`] — this gear's own
    /// maintenance duty rather than a caller's request, so there is no
    /// `SecurityContext`/PDP round trip to resolve a narrower one from (see
    /// that method's own doc). `allow_all` is not a bypass of `SecureORM`:
    /// it is still routed through `.secure().scope_with(scope)` like every
    /// other read on this trait, and `SecureORM`'s sealed `DBRunner` gives no
    /// way around that — it is simply the one *value* of `AccessScope` that
    /// applies no row-level filter, which `toolkit_security` itself documents
    /// as "a legitimate PDP decision with no row-level filtering. Not a
    /// bypass — it's a valid authorization outcome."
    ///
    /// The tenant travels with each row precisely so the sweep can mint a
    /// *tenant-bound* [`toolkit_security::SecurityContext`] per environment
    /// afterwards (`crate::domain::system_actor::for_observation`) —
    /// everything the sweep does with a given environment beyond this listing
    /// goes through the ordinary PEP-scoped path, under that context.
    ///
    /// # Deliberately unpaged, unlike [`Self::list_page`]
    ///
    /// Review finding #55 is about a *client-facing* collection read with no
    /// page, and [`Self::list_page`] above is where it is closed. This is the
    /// exception, and it is one on purpose: no client can ask for it (it is on
    /// no route), and its one caller needs every row — a paged sweep would
    /// either carry a cursor across ticks, silently skipping or re-observing
    /// rows created and deleted between them, or cap itself and leave the
    /// environments past the cap never observed. The implementation carries the
    /// same note beside the `.all()` itself
    /// (`infra::storage::environments_sea_repo`), so a reader who arrives at the
    /// query rather than at the trait finds it there too.
    async fn list_all_with_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<(Environment, Uuid)>, DomainError>;

    /// Create a new environment.
    ///
    /// [`PersistedCredentials`] is passed **separately** and is what reaches the
    /// credential columns (`credentials`, `config`); `new.credentials`,
    /// `new.kubeconfig_credstore_ref` and `new.kubeconfig` (the request's unresolved
    /// fields) are ignored, and `EnvironmentsService::create_environment` clears all
    /// three before calling. That is deliberate: `NewEnvironment` carries
    /// *unresolved* credentials — references, or the documents themselves — and only
    /// the service can collapse them, because resolving a document means writing it
    /// to credstore first and asking the product's plugin which fields are secret at
    /// all. Mirrors `qa-catalog`'s `SshKeysRepository::create`, which likewise takes
    /// an already-resolved `credstore_ref: String`.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewEnvironment,
        credentials: PersistedCredentials,
    ) -> Result<Environment, DomainError>;

    /// Apply a partial update to an existing environment.
    ///
    /// `credentials` is `Some` exactly when the patch replaced at least one
    /// credential, and then it is the **complete** post-update credential state, not
    /// a delta: the service merges the patch's submissions over what the row already
    /// held, because a `credentials` JSON array cannot be partially assigned. `None`
    /// leaves both credential columns (`credentials`, `config`) `Unchanged`.
    /// `patch.credentials`, `patch.kubeconfig` and `patch.kubeconfig_credstore_ref`
    /// are ignored for [`Self::create`]'s reason.
    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        patch: EnvironmentPatch,
        credentials: Option<PersistedCredentials>,
    ) -> Result<Option<Environment>, DomainError>;

    /// Clear `is_default` on every environment of `product_id` **except** `except_id`,
    /// returning how many rows were changed.
    ///
    /// This is the enforcement point for "at most one default environment per (tenant,
    /// product)". It lives in the repository rather than as a database constraint
    /// because `MySQL` has no partial unique indexes, so a schema-level rule would
    /// have held on two dialects out of three.
    ///
    /// **That reason has expired and the placement has not been re-argued** —
    /// second review, finding #110. This gear declares `POSTGRES_UP` and
    /// `SQLITE_UP` and no `MySQL` blob at all, and both of those *do* have
    /// partial unique indexes, so the dialect that made the constraint
    /// impossible is not one this gear has. What keeps the rule here is now
    /// only that moving it is a migration: a partial unique index over
    /// `(tenant_id, product_id) WHERE is_default` would have to be added with
    /// the duplicates it forbids already impossible, and this method plus the
    /// ordering described below is what makes them impossible today. Recorded
    /// as an open choice rather than presented as a settled one.
    ///
    /// (Originally recorded in the module doc of
    /// `m20260831_000009_platform_is_default`, one of the migrations folded into
    /// `migrations::m20260812_000001_initial` by the docs squash; that module
    /// doc's fuller text did not survive the fold, so the reason is restated
    /// here rather than pointed at.)
    ///
    /// **This gear has no transaction seam** — every write goes through
    /// `self.db.conn()` and there is no `begin()` anywhere in it — so the clear and
    /// the write that sets the new default are two statements, not one unit. The
    /// ordering is what bounds the damage: `EnvironmentsService` calls this **first**
    /// and sets the new default second, so a failure between them leaves the
    /// product with **zero** defaults rather than two. Zero is a state the callers
    /// already handle (the dialogs fall back to "the product's only environment", and
    /// ask the operator when it cannot resolve); two is ambiguous and would make
    /// "Default cluster" pick arbitrarily. If a transaction seam is ever added to
    /// this gear, wrap the pair and delete this paragraph.
    ///
    /// `except_id` is the row being promoted, which must not clear itself: on the
    /// create path it does not exist yet, and on the update path it is the row whose
    /// flag was just set.
    async fn clear_default_for_product<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        product_id: Uuid,
        except_id: Uuid,
    ) -> Result<u64, DomainError>;

    /// Delete an environment by ID.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Persist the result of one product-plugin `observe` call into the
    /// environment's one observation column set — see
    /// `OrmEnvironmentsRepository::record_observation` for the SQL and the
    /// reasoning behind every merge rule.
    ///
    /// # One column set since Task 19, and the dual write before it
    ///
    /// One observation writes the plugin-shaped set (`observed_attrs`,
    /// `observed_base_url`, `health_state`, `health_detail`,
    /// `health_checked_at`) and the four pre-plugin columns that survived
    /// (`observed_version`, `observed_build`, `version_detect_error`,
    /// `version_detected_at`). Through Phases D and E it also wrote a legacy
    /// set — `observed_namespace`, `vhp_base_url` and the five `cluster_*` —
    /// and that dual write was what kept Phase D revertible. Task 19 dropped
    /// the legacy set; since the drop the phase is not revertible, and the
    /// table below is the authority on how the columns that exist are merged.
    ///
    /// Three facts about the dual write were corrected at the Phase E review,
    /// because an earlier version of this paragraph claimed every legacy
    /// reader "sees exactly what it saw before" and Task 19 read this doc to
    /// decide which columns were safe to drop:
    ///
    /// * **`cluster_nodes` and `cluster_namespace_count` were CLEARED** on
    ///   every successful read, not preserved — the plugin contract keeps only
    ///   the health verdict, so nothing reaching this method carried a node
    ///   list. The node inventory has no replacement, by the owner's decision.
    /// * **`observed_namespace` stayed sticky** (filled only from `NULL`), and
    ///   review finding I-3 — that a redeployed environment ran in its new
    ///   namespace and displayed the old one — was discharged by the drop
    ///   rather than fixed: `observed_attrs`, overwritten on every detection,
    ///   is the namespace's only home.
    /// * **`qa-runs`' dispatch was no longer a legacy reader at all.** Since
    ///   Task 18 it has read `product_id`, `config`, `observed_attrs` and the
    ///   credential reference (`kubeconfig_credstore_ref` until Task 19,
    ///   `credentials` since), and neither `vhp_base_url` nor
    ///   `observed_namespace`.
    ///
    /// The merge rules for the columns that exist:
    ///
    /// | column | on a detected/checked outcome | on a failed one |
    /// | ------ | ----------------------------- | --------------- |
    /// | `observed_version`, `observed_build` | overwritten, even to `NULL` — the target is authoritative about its own version | untouched; stale-but-known beats blank |
    /// | `observed_base_url` | overwritten only by a *conclusive* value; an absent one falls through to what is stored | untouched |
    /// | `observed_attrs` | overwritten with the declared attributes | untouched, for `observed_version`'s reason |
    /// | `version_detect_error` | cleared to `NULL` | the failure's classified text |
    /// | `version_detected_at` | now | now |
    /// | `health_state` | the verdict | `unknown` |
    /// | `health_detail` | the verdict's fixed detail | the failure's classified text |
    /// | `health_checked_at` | now | now |
    ///
    /// The dropped columns' rules, for the record: `observed_namespace` was
    /// filled only if currently `NULL`; `vhp_base_url` followed
    /// `observed_base_url`; `cluster_status` held the legacy spelling of the
    /// verdict, `Unreachable` on a failure; `cluster_status_message` mirrored
    /// `health_detail`; `cluster_checked_at` mirrored `health_checked_at`; and
    /// `cluster_nodes` and `cluster_namespace_count` were **cleared** on both
    /// outcomes (a read that cannot vouch for nodes reports none).
    ///
    /// Clearing the node columns on a *successful* read too was the one
    /// legacy rule the dual write could not preserve: the plugin contract
    /// keeps only the health verdict and leaves a product's own facts in
    /// `ObservedAttrs` (`gears/qa-platform/docs/features/product-plugins.md`'s
    /// `observe` section: "The gear persists the result into
    /// `observed_version`, `observed_build`, `observed_base_url`,
    /// `observed_attrs`, `health_state`, `health_detail` and
    /// `health_checked_at`"), so nothing reaching here carries a node list.
    /// Clearing rather than keeping was that rule — an unreachable read
    /// reports no nodes — applied to an observation that cannot vouch for
    /// nodes: a stale-but-reported-ready
    /// node rendered green on a cluster nobody counted is the exact false
    /// signal `Unreachable` existed to prevent.
    ///
    /// `HealthOutcome::NotAttempted` writes **none** of the health columns —
    /// not even a checked-at — because "never checked" is the
    /// truth when nothing looked, and `health_state`'s `NOT NULL DEFAULT
    /// 'unknown'` means the row simply keeps what it had.
    ///
    /// The two halves are independent: each is matched on its own
    /// value, so a failed health read never touches what the environment half
    /// wrote in the same call, and vice versa.
    ///
    /// Takes `runner`/`scope` rather than a `SecurityContext`, matching every
    /// other method on this trait: policy resolution (`SecurityContext` →
    /// `AccessScope`) is a service-layer concern via `PolicyEnforcer`, and this
    /// trait's job is to accept an already-resolved scope. Called from
    /// `EnvironmentsService::observe_environment` (the on-demand refresh) and,
    /// since Task 8, from `EnvironmentsService::run_observation_cycle`'s
    /// per-environment pass through that same method — both resolve their own
    /// scope before calling here.
    ///
    /// `observation` is an [`ObservationWrite`], whose only constructor has
    /// already applied `retain_declared`: an undeclared attribute cannot
    /// reach this method, let alone a column.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::EnvironmentNotFound` if `id` does not name an environment
    /// visible under `scope` (including "does not exist at all"), and
    /// `DomainError::Database` on any other failure.
    async fn record_observation<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        observation: &ObservationWrite,
    ) -> Result<(), DomainError>;
}
