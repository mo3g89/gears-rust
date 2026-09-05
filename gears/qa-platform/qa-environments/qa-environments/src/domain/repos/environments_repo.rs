use async_trait::async_trait;
use qa_environments_sdk::{Environment, EnvironmentCredential, EnvironmentPatch, NewEnvironment};
use toolkit_db::secure::DBRunner;
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
    /// The value for the pre-plugin `kubeconfig_credstore_ref` column: the
    /// reference belonging to the plugin's sole required secret field.
    ///
    /// **This is the dual-write.** `qa-runs`' dispatch reads that column in
    /// production (ruling E-17), so it stays maintained until Task 19 drops
    /// it.
    ///
    /// **Two readers, and they are both in this workspace's Rust:**
    /// `EnvironmentsService::resolve_credential_slots` here and
    /// `DispatchService::plugin_dispatch` in `qa-runs`. An earlier version of
    /// this doc named `qa-insights` as a third; it is not one (review finding
    /// I-3) — its four mentions of the column are a doc citation using it as a
    /// naming precedent, two test fixtures, and a comment. **Task 19 Step 2
    /// decides what is safe to drop by enumerating this column's readers**, so
    /// a phantom reader here is exactly the wrong place to be loose.
    ///
    /// # It is a LOSSY projection of `credentials`, and Task 19 must not treat it otherwise
    ///
    /// **Review finding CRITICAL-1.** One column holds one reference, so this
    /// field can only ever carry the *sole required secret*'s. Two row shapes
    /// therefore exist that it cannot represent:
    ///
    /// * one required secret **plus any other stored secret** — `credentials`
    ///   holds *n* entries, this holds one of them;
    /// * **two or more required secrets** — `sole_required_secret_key` returns
    ///   `None`, so this is the empty string while `credentials` is perfectly
    ///   valid. That is the case the paragraph below is about: one column
    ///   cannot say which of two required secrets it holds, and both readers
    ///   refuse it rather than guess (`AMBIGUOUS_CREDENTIAL`,
    ///   `LEGACY_CREDENTIAL_UNBINDABLE`).
    ///
    /// Neither shape is reachable with the one plugin this tree ships, which
    /// declares a single required secret — so today `credentials` has exactly
    /// one entry and this field equals it. **That is an accident of there
    /// being one plugin, which is the thing this branch exists to stop being
    /// true**, and it is why re-deriving `credentials` *from* this column is
    /// safe only for the pre-plugin shape. Task 19 Step 2 restricts its
    /// overwrite to rows with at most one credential and refuses to run at all
    /// on a row that disagrees with this field, rather than silently replacing
    /// *n* bindings with one and orphaning the rest in the same migration that
    /// drops the column.
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

    /// List all environments visible within the given security scope.
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Environment>, DomainError>;

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
    async fn list_all_with_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<(Environment, Uuid)>, DomainError>;

    /// Create a new environment.
    ///
    /// [`PersistedCredentials`] is passed **separately** and is what reaches
    /// the credential columns; `new.credentials`, `new.kubeconfig_credstore_ref`
    /// and `new.kubeconfig` are ignored, and
    /// `EnvironmentsService::create_environment` clears all three before
    /// calling. That is deliberate: `NewEnvironment` carries *unresolved*
    /// credentials — references, or the documents themselves — and only the
    /// service can collapse them, because resolving a document means writing it
    /// to credstore first and asking the product's plugin which fields are
    /// secret at all. Mirrors `qa-catalog`'s `SshKeysRepository::create`, which
    /// likewise takes an already-resolved `credstore_ref: String`.
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
    /// credential, and then it is the **complete** post-update credential
    /// state, not a delta: the service merges the patch's submissions over what
    /// the row already held, because a `credentials` JSON array cannot be
    /// partially assigned. `None` leaves all three credential columns
    /// `Unchanged`. `patch.credentials`, `patch.kubeconfig` and
    /// `patch.kubeconfig_credstore_ref` are ignored for
    /// [`Self::create`]'s reason.
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
    /// hold on two dialects out of three — see
    /// `m20260831_000009_platform_is_default`'s module doc.
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

    /// Persist the result of one product-plugin `observe` call, into **both**
    /// column sets — see `OrmEnvironmentsRepository::record_observation` for
    /// the SQL and the reasoning behind every merge rule.
    ///
    /// # The dual write, and what makes Phase D revertible
    ///
    /// One observation, two sets of columns. The plugin-shaped set
    /// (`observed_attrs`, `observed_base_url`, `health_state`,
    /// `health_detail`, `health_checked_at`) is the new home; the legacy set
    /// (`observed_version`, `observed_build`, `observed_namespace`,
    /// `vhp_base_url`, `version_detect_error`, `version_detected_at` and the
    /// five `cluster_*`) keeps being written, and the table below is the
    /// authority on how.
    ///
    /// **Three exceptions, corrected at the Phase E review — the earlier
    /// version of this paragraph claimed every legacy reader "sees exactly
    /// what it saw before", and Task 19 reads this doc to decide which columns
    /// are safe to drop:**
    ///
    /// * **`cluster_nodes` and `cluster_namespace_count` are CLEARED** on every
    ///   successful read, not preserved — the plugin contract keeps only the
    ///   health verdict, so nothing reaching this method carries a node list
    ///   (ruling D-21, accepted by the user as U4). The UI's node table is
    ///   empty until Task 21 restores it from `observed_attrs`.
    /// * **`observed_namespace`'s merge rule changed** at the Phase E review
    ///   (finding I-3): a conclusive detection now wins, where before the
    ///   first detection won forever. See the table.
    /// * **`qa-runs`' dispatch is no longer a legacy reader at all.** Since
    ///   Task 18 it reads `product_id`, `kubeconfig_credstore_ref`, `config`
    ///   and `observed_attrs`, and neither `vhp_base_url` nor
    ///   `observed_namespace`.
    ///
    /// Task 19 drops the legacy set; until it does, reverting this phase costs
    /// nothing but the three items above.
    ///
    /// The merge rules, all of them:
    ///
    /// | column | on a detected/checked outcome | on a failed one |
    /// | ------ | ----------------------------- | --------------- |
    /// | `observed_version`, `observed_build` | overwritten, even to `NULL` — the target is authoritative about its own version | untouched; stale-but-known beats blank |
    /// | `observed_namespace` | filled only if currently `NULL` — a stored value wins | untouched |
    /// | `vhp_base_url`, `observed_base_url` | overwritten only by a *conclusive* value; an absent one falls through to what is stored | untouched |
    /// | `observed_attrs` | overwritten with the declared attributes | untouched, for `observed_version`'s reason |
    /// | `version_detect_error` | cleared to `NULL` | the failure's classified text |
    /// | `version_detected_at` | now | now |
    /// | `cluster_status` | the legacy spelling of the verdict | `Unreachable` |
    /// | `cluster_status_message` | `NULL` | the failure's classified text |
    /// | `cluster_nodes`, `cluster_namespace_count` | **cleared** | cleared (D-CH-3) |
    /// | `health_state` | the verdict | `unknown` |
    /// | `health_detail` | the verdict's fixed detail | the failure's classified text |
    /// | `cluster_checked_at`, `health_checked_at` | now | now |
    ///
    /// `cluster_nodes` and `cluster_namespace_count` are cleared on a
    /// *successful* read too, and that is the one legacy rule this phase
    /// cannot preserve: the plugin contract keeps only the health verdict and
    /// leaves a product's own facts in `ObservedAttrs`
    /// (`PRODUCT-PLUGINS-DESIGN.md` §5.3), so nothing reaching here carries a
    /// node list any more. Clearing rather than keeping is D-CH-3's own rule
    /// applied to an observation that cannot vouch for nodes: a
    /// stale-but-reported-ready node rendered green on a cluster nobody
    /// counted is the exact false signal `Unreachable` exists to prevent.
    ///
    /// `HealthOutcome::NotAttempted` writes **none** of the health columns —
    /// neither set, not even a checked-at — because "never checked" is the
    /// truth when nothing looked, and `health_state`'s `NOT NULL DEFAULT
    /// 'unknown'` means the row simply keeps what it had.
    ///
    /// The two halves are independent (D-CH-4): each is matched on its own
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
