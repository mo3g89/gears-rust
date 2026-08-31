use async_trait::async_trait;
use qa_environments_sdk::{NewPlatform, PlatformPatch, TargetPlatform};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::PlatformObservation;

/// Repository trait for `TargetPlatform` persistence operations.
#[async_trait]
pub trait PlatformsRepository: Send + Sync {
    /// Find a platform by ID within the given security scope.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TargetPlatform>, DomainError>;

    /// List all platforms visible within the given security scope.
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<TargetPlatform>, DomainError>;

    /// Every platform visible within `scope`, paired with the tenant that
    /// owns it. The sole caller is the background observation ticker's
    /// per-cycle sweep (`PlatformsService::run_observation_cycle`), which
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
    /// *tenant-bound* [`toolkit_security::SecurityContext`] per platform
    /// afterwards (`crate::domain::system_actor::for_observation`) —
    /// everything the sweep does with a given platform beyond this listing
    /// goes through the ordinary PEP-scoped path, under that context.
    async fn list_all_with_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<(TargetPlatform, Uuid)>, DomainError>;

    /// Create a new platform.
    ///
    /// `kubeconfig_credstore_ref` is passed **separately** and is the value
    /// written to the column; `new.kubeconfig_credstore_ref` and
    /// `new.kubeconfig` are ignored, and `PlatformsService::create_platform`
    /// clears both before calling. That is deliberate: `NewPlatform` carries
    /// an *unresolved* kubeconfig — either a reference or the document itself
    /// — and only the service can collapse the two, because resolving a
    /// document means writing it to credstore first. Mirrors
    /// `qa-catalog`'s `SshKeysRepository::create`, which likewise takes an
    /// already-resolved `credstore_ref: String`.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewPlatform,
        kubeconfig_credstore_ref: String,
    ) -> Result<TargetPlatform, DomainError>;

    /// Apply a partial update to an existing platform.
    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        patch: PlatformPatch,
    ) -> Result<Option<TargetPlatform>, DomainError>;

    /// Clear `is_default` on every platform of `product_id` **except** `except_id`,
    /// returning how many rows were changed.
    ///
    /// This is the enforcement point for "at most one default platform per (tenant,
    /// product)". It lives in the repository rather than as a database constraint
    /// because `MySQL` has no partial unique indexes, so a schema-level rule would
    /// hold on two dialects out of three — see
    /// `m20260831_000009_platform_is_default`'s module doc.
    ///
    /// **This gear has no transaction seam** — every write goes through
    /// `self.db.conn()` and there is no `begin()` anywhere in it — so the clear and
    /// the write that sets the new default are two statements, not one unit. The
    /// ordering is what bounds the damage: `PlatformsService` calls this **first**
    /// and sets the new default second, so a failure between them leaves the
    /// product with **zero** defaults rather than two. Zero is a state the callers
    /// already handle (the dialogs fall back to "the product's only platform", and
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

    /// Delete a platform by ID.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Persist the result of one `PlatformObserver::observe` call — both the
    /// version-detection half (`observation.platform`) and the cluster-health
    /// half (`observation.health`) — applying five deliberately different
    /// merge rules against whatever is already stored — see
    /// `OrmPlatformsRepository::record_observation` for the SQL and the
    /// reasoning behind each rule.
    ///
    /// The version half keeps its original three rules (`observed_version`/
    /// `observed_build` overwritten unconditionally, `observed_namespace` kept
    /// if already set, `vhp_base_url` overwritten only by a conclusive
    /// detection). The health half adds two more: `cluster_status` and the
    /// node/namespace data overwrite unconditionally on a successful read
    /// (D-CH-2), and — the deliberate divergence from the version half's
    /// stale-but-known default — a **failed** health read clears
    /// `cluster_nodes`/`cluster_namespace_count` back to `NULL` rather than
    /// keeping a previous reading (D-CH-3). The two halves are independent
    /// and fail independently (D-CH-4): a failure in one never touches what
    /// the other half wrote in the same call.
    ///
    /// Takes `runner`/`scope` rather than a `SecurityContext`, matching every
    /// other method on this trait: policy resolution (`SecurityContext` →
    /// `AccessScope`) is a service-layer concern via `PolicyEnforcer`, and this
    /// trait's job is to accept an already-resolved scope. Called from
    /// `PlatformsService::observe_platform` (the on-demand refresh) and, since
    /// Task 8, from `PlatformsService::run_observation_cycle`'s per-platform
    /// pass through that same method — both resolve their own scope before
    /// calling here.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::PlatformNotFound` if `id` does not name a platform
    /// visible under `scope` (including "does not exist at all"), and
    /// `DomainError::Database` on any other failure.
    async fn record_observation<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        observation: &PlatformObservation,
    ) -> Result<(), DomainError>;
}
