//! Target platforms (systems under test) service.
//!
//! # The kubeconfig is secret material, and this module is where it stays
//!
//! A platform's kubeconfig can arrive two ways: as a credstore **reference**
//! the caller already holds, or as a pasted **document**
//! ([`KubeconfigMaterial`]). A kubeconfig's `users[].user.client-key-data` is
//! a client private key, so a pasted document is treated exactly as
//! `qa-catalog` treats a pasted SSH private key
//! (`qa-catalog/src/domain/service/ssh_keys.rs`): it goes to credstore
//! **first**, and only the generated reference reaches
//! `qa_platforms.kubeconfig_credstore_ref`.
//!
//! **The invariant is that the material never leaves the gear — not that it
//! is never read.** [`Self::observe_platform`] is the first read path that
//! resolves a reference back into the material: it fetches the stored
//! kubeconfig from credstore and hands it to a [`PlatformObserver`], which
//! builds an in-process Kubernetes client from it to read the platform's own
//! cluster. That is still true to the original promise, which was always
//! about what a *caller* can get back, not about whether this process may
//! look at bytes it already owns: no DTO, no REST response, no log line, and
//! no error message ever carries the document or the client built from it —
//! see [`crate::infra::observer::kube_observer`]'s own leak tests
//! (`the_kubeconfig_never_reaches_the_log`) for the read path's half of that
//! proof.
//!
//! Three things keep it from leaving, and none of them is a convention
//! someone has to remember:
//!
//! * [`KubeconfigMaterial`]'s `Debug` prints `[REDACTED]`, so the derived
//!   `Debug` of [`NewPlatform`]/[`PlatformPatch`] — and of anything holding
//!   one — is redacted too;
//! * every `#[instrument]` below skips the argument carrying it (`new`,
//!   `patch`) or the response carrying the resolved material
//!   (`observe_platform`'s internal fetch never logs the `SecretValue` it
//!   gets back, which itself redacts in any `Debug`/`Display` formatting);
//! * the resolved value handed to the repository layer is a `String`
//!   **reference**, and the material is cleared out of the struct that
//!   travels there — and the same is true one step further in now:
//!   [`PlatformObserver::observe`]'s own signature takes the material and
//!   returns [`crate::domain::ports::ObservationOutcome`], a plain value with
//!   no kubeconfig-shaped field anywhere in it, so the boundary between "has
//!   the material" and "reports what it found" is a type, not a promise.

use std::sync::Arc;

use credstore_sdk::{
    CredStoreClientV1, CredStoreError, SecretRef, SecretValue, SharingMode, WritePrecondition,
};
use toolkit_macros::domain_model;
use tracing::{debug, info, instrument, warn};

use crate::domain::error::DomainError;
use crate::domain::ports::{
    HealthOutcome, ObservationOutcome, PlatformObservation, PlatformObserver,
};
use crate::domain::repos::{LeasesRepository, PlatformsRepository, VariablesRepository};
use crate::domain::service::DbProvider;
#[cfg(any(feature = "platform-observation", test))]
use crate::domain::system_actor;
use authz_resolver_sdk::PolicyEnforcer;

use super::{actions, resources};
use qa_environments_sdk::{
    KubeconfigMaterial, LeaseState, NewPlatform, PlatformPatch, TargetPlatform, Variable,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

const MAX_NAME_LEN: usize = 255;

/// The vpadm namespace [`Self::observe_cluster`] targets when the platform has
/// no `VPADM_NAMESPACE` override. Matches legacy's own fallback
/// (`manager/src/services/platforms.rs:1312`, `pick_vpadm_namespace`'s
/// `"virtuozzo"` default) — see [`pick_vpadm_namespace`] for the full
/// precedence rule.
const DEFAULT_VPADM_NAMESPACE: &str = "virtuozzo";

/// The per-platform variable legacy reads to override the vpadm namespace.
/// Matches `manager/src/services/platforms.rs`' `VPADM_NAMESPACE_VAR`.
const VPADM_NAMESPACE_VAR: &str = "VPADM_NAMESPACE";

/// Prefix of every credstore reference **this gear generates** for a pasted
/// kubeconfig, shaped like `qa-catalog`'s `qa-catalog-ssh-key-{uuid}`.
///
/// It is also the ownership marker: a reference carrying it was minted here
/// from a document this gear was handed, so this gear is responsible for
/// deleting it. A reference *without* it was supplied by a caller, may be
/// shared with other systems, and is never deleted by this gear — see
/// [`PlatformsService::forget_owned_secret`].
const GENERATED_KUBECONFIG_REF_PREFIX: &str = "qa-environments-kubeconfig-";

/// Which of the two mutually-exclusive kubeconfig inputs the caller supplied,
/// after validation. Constructed only by
/// [`PlatformsService::resolve_kubeconfig_source`] (create) and the equivalent
/// match in [`PlatformsService::update_platform`], so "exactly one" is
/// established before this type exists. On update it is wrapped in an `Option`,
/// whose `None` is the third case a patch has and a create does not: the
/// kubeconfig was not mentioned at all.
enum KubeconfigSource {
    /// A credstore reference the caller already holds. Nothing is written to
    /// credstore, and nothing is ever deleted from it either — see
    /// [`PlatformsService::forget_owned_secret`].
    Reference(String),
    /// A pasted document, to be written under a generated reference.
    Material(KubeconfigMaterial),
}

/// The reference that reaches the database, plus whether this gear minted it
/// (and so is responsible for cleaning it up).
struct StoredKubeconfig {
    reference: String,
    generated: bool,
}

/// Maximum `default_branch` length, **in characters**, matching both
/// `qa_platforms.default_branch VARCHAR(512)` and the
/// `qa_runs.test_version VARCHAR(512)` column this value is resolved into — see
/// `validate_default_branch`, and `m20260814_000006_platform_default_branch`'s
/// module doc for why the sink's width is the anchor rather than any convention
/// in this table.
const MAX_DEFAULT_BRANCH_CHARS: usize = 512;

/// Target platforms (systems under test) service.
///
/// # Design
///
/// Services acquire database connections internally via `DBProvider`. Callers
/// call service methods with business parameters only - no DB objects.
#[domain_model]
pub struct PlatformsService<P: PlatformsRepository, V: VariablesRepository, L: LeasesRepository> {
    db: Arc<DbProvider>,
    repo: Arc<P>,
    /// Read-only from this service's point of view: the only method that
    /// touches it is [`Self::vpadm_namespace_for`], resolving a platform's own
    /// `VPADM_NAMESPACE` override for [`Self::observe_cluster`]. Writes to
    /// per-platform variables go through `VariablesService`, not here.
    variables_repo: Arc<V>,
    leases_repo: Arc<L>,
    credstore: Arc<dyn CredStoreClientV1>,
    /// Reads a platform's own cluster to detect its version, build, namespace
    /// and base domain. A `NoopObserver` in a build without the
    /// `platform-observation` feature — see `crate::gear`'s DI wiring, which
    /// selects between the two at build time, exactly as it already does for
    /// `credstore` above.
    observer: Arc<dyn PlatformObserver>,
    policy_enforcer: PolicyEnforcer,
}

/// What a caller is told when a platform's kubeconfig cannot be resolved.
///
/// # Fixed text, and why it must stay fixed
///
/// This lands in `qa_platforms.version_detect_error`, which is published on
/// `PlatformDto` to every `qa.platform` GET/LIST-authorized caller and rendered
/// on the platform page. The `DomainError` it replaces reads
/// `"platform {id}'s kubeconfig secret ({credstore_ref}) was not found in
/// credstore"` -- and **`credstore_ref` is precisely the value that is banned
/// from `PlatformDto`**: under `SharingMode::Tenant` the reference *is* a read
/// path to the kubeconfig, which is why `From<TargetPlatform>` drops it and
/// carries a "do not add it back" comment. Formatting that error into this
/// column would publish the reference by the back door.
///
/// The other arm of the same `Result` is worse still: `map_credstore_error`
/// wraps whatever the credstore client's `Display` produced, which is not a
/// bounded string this crate controls.
///
/// So neither is interpolated. This is the same rule
/// `infra::observer::errors` implements for kubeconfig-derived failures, applied
/// one layer up, where the value at risk is the reference rather than the
/// material. It says what an operator can act on without naming the secret.
const KUBECONFIG_UNRESOLVED: &str =
    "this platform's kubeconfig could not be read from the credential store, so no \
     observation was attempted: the stored reference names no secret, or the credential \
     store refused the read. Re-save the platform with its kubeconfig to provision it.";

impl<P: PlatformsRepository, V: VariablesRepository, L: LeasesRepository> PlatformsService<P, V, L> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<P>,
        variables_repo: Arc<V>,
        leases_repo: Arc<L>,
        credstore: Arc<dyn CredStoreClientV1>,
        observer: Arc<dyn PlatformObserver>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repo,
            variables_repo,
            leases_repo,
            credstore,
            observer,
            policy_enforcer,
        }
    }
}

// Business logic methods
impl<P: PlatformsRepository, V: VariablesRepository, L: LeasesRepository> PlatformsService<P, V, L> {
    #[instrument(skip(self, ctx), fields(platform_id = %id))]
    pub async fn get_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TargetPlatform, DomainError> {
        debug!("Getting platform by id");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::GET, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        self.repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::PlatformNotFound { id })
    }

    #[instrument(skip(self, ctx))]
    pub async fn list_platforms(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<TargetPlatform>, DomainError> {
        debug!("Listing platforms");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::LIST, None)
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let platforms = self.repo.list(&conn, &scope).await?;

        debug!("Successfully listed {} platforms", platforms.len());
        Ok(platforms)
    }

    /// Register a platform from either a credstore reference the caller holds
    /// or a kubeconfig document they pasted.
    ///
    /// When a document is supplied it reaches credstore **before** any row is
    /// written, so a failed credstore write creates nothing; and when the row
    /// write then fails, the just-written secret is deleted rather than
    /// orphaned. Both halves are `create_ssh_key`'s ordering
    /// (`qa-catalog/src/domain/service/ssh_keys.rs`), for its reasons.
    ///
    /// `new` is `skip`ped by `#[instrument]` — it carries the document.
    #[instrument(skip(self, ctx, new), fields(name = %new.name))]
    pub async fn create_platform(
        &self,
        ctx: &SecurityContext,
        new: NewPlatform,
    ) -> Result<TargetPlatform, DomainError> {
        info!("Creating new platform");

        Self::validate_name(&new.name)?;
        let source = Self::resolve_kubeconfig_source(
            new.kubeconfig_credstore_ref.clone(),
            new.kubeconfig.clone(),
        )?;
        // Normalise before validating, so the length check sees exactly what
        // will be stored rather than the caller's untrimmed text.
        let new = NewPlatform {
            default_branch: Self::normalize_default_branch(new.default_branch),
            // The material is resolved above and must not travel any further
            // towards the repository layer.
            kubeconfig: None,
            kubeconfig_credstore_ref: None,
            ..new
        };
        Self::validate_default_branch(new.default_branch.as_deref())?;

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::CREATE, None)
            .await?;

        // Everything that can reject the request has now run, so nothing
        // rejected ever reaches credstore.
        let stored = self.store_kubeconfig_source(ctx, source).await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let tenant_id = ctx.subject_tenant_id();

        // "At most one default per product". Cleared BEFORE the create, not after:
        // this gear has no transaction seam, and a failure between the two
        // statements should leave the product with zero defaults (recoverable, and
        // the dialogs fall back) rather than two (ambiguous). See
        // `PlatformsRepository::clear_default_for_product`.
        //
        // `Uuid::nil()` as the exclusion: the row being created has no id yet and
        // cannot be among the rows to clear, so there is nothing to exclude — and a
        // nil id matches no platform, every id being `Uuid::new_v4()`.
        if let (true, Some(product_id)) = (new.is_default, new.product_id) {
            self.repo
                .clear_default_for_product(&conn, &scope, product_id, Uuid::nil())
                .await?;
        }

        let created = self
            .repo
            .create(&conn, &scope, tenant_id, new, stored.reference.clone())
            .await;

        match created {
            Ok(platform) => {
                info!("Successfully created platform with id={}", platform.id);
                // Decision D4, on the path that needs it most: without this a
                // UI-created platform has no runner `Secret` until the next
                // ticker cycle (default 300s), which IS the ~5-minute
                // `FailedMount` window D4 exists to eliminate. Logged and
                // swallowed - see `materialise_runner_secret`'s doc for why a
                // failure here must not undo the create.
                self.materialise_runner_secret(ctx, &platform, "create").await;
                Ok(platform)
            }
            Err(db_err) => {
                // Don't orphan a secret we just wrote when the row fails (the
                // realistic cause is a duplicate name): best-effort cleanup,
                // then propagate the original error — `create_ssh_key`'s
                // compensating delete, for its reasons.
                self.forget_owned_secret(ctx, &stored.reference, stored.generated)
                    .await;
                Err(db_err)
            }
        }
    }

    /// Apply a partial update, optionally replacing the kubeconfig.
    ///
    /// A pasted replacement follows the same order as create — new secret
    /// first, then the row — so a failed credstore write leaves the platform
    /// exactly as it was. Only once the row carries the **new** reference is
    /// the **old** secret removed, and then only if this gear generated it
    /// (see [`Self::forget_owned_secret`]). A failed row write deletes the
    /// secret just written, mirroring `create_ssh_key`'s compensating delete.
    ///
    /// A replacement spelled as a **reference** rather than a document gets the
    /// same cleanup. It did not, and the orphan that produced was measured:
    /// see the comment at the `previous_ref` read below and
    /// `patching_a_reference_over_a_generated_one_removes_the_superseded_secret`.
    ///
    /// `patch` is `skip`ped by `#[instrument]` — it can carry the document.
    #[instrument(skip(self, ctx, patch), fields(platform_id = %id))]
    pub async fn update_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: PlatformPatch,
    ) -> Result<TargetPlatform, DomainError> {
        info!("Updating platform");

        if let Some(ref name) = patch.name {
            Self::validate_name(name)?;
        }
        // An empty `kubeconfig_credstore_ref` counts as **not supplied**, which
        // is the reading `resolve_kubeconfig_source` has always given it on
        // create. Without this filter the two paths disagreed: create
        // accepted `{"kubeconfig_credstore_ref": "", "kubeconfig": "…"}` as a
        // paste and update rejected the identical payload as "both supplied".
        let named_ref = patch.kubeconfig_credstore_ref.as_deref();
        let supplied_ref = named_ref.filter(|r| !r.is_empty());

        // Only one of the two may be present. Unlike create, *neither* is
        // fine: a patch that does not mention the kubeconfig leaves it alone.
        let replacement = match (supplied_ref, patch.kubeconfig.as_ref()) {
            (Some(_), Some(_)) => return Err(Self::both_kubeconfig_fields_error()),
            (Some(credstore_ref), None) => {
                Self::validate_credstore_ref(credstore_ref)?;
                Some(KubeconfigSource::Reference(credstore_ref.to_owned()))
            }
            (None, Some(material)) => Some(KubeconfigSource::Material(Self::validate_material(
                material.clone(),
            )?)),
            (None, None) => {
                // Naming the field with an empty value is not "leave it alone":
                // that `Some("")` would reach the repository and blank
                // `kubeconfig_credstore_ref NOT NULL`, i.e. destroy a live
                // platform's only pointer to its credentials. It stays the
                // `must not be empty` rejection it has always been.
                if let Some(empty) = named_ref {
                    Self::validate_credstore_ref(empty)?;
                }
                None
            }
        };
        // Normalise inside the `Some`, never across it: the outer layer carries
        // "the caller mentioned this field at all" and collapsing it here would
        // silently turn a clear into a no-op — the exact failure this field was
        // added to fix, one level up.
        let patch = PlatformPatch {
            default_branch: patch.default_branch.map(Self::normalize_default_branch),
            // The material is resolved above; only a reference goes on.
            kubeconfig: None,
            ..patch
        };
        if let Some(ref default_branch) = patch.default_branch {
            Self::validate_default_branch(default_branch.as_deref())?;
        }

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::UPDATE, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // Resolve the row before writing anything, so the old reference is
        // known (it may need deleting) and a missing platform costs no
        // credstore write at all.
        //
        // This read happens for a **new reference** as much as for a paste, and
        // that is the whole point. It used to happen only for a paste, and the
        // consequence was measured: `create(pasted)` then
        // `update(kubeconfig_credstore_ref: "team-a-prod-cluster")` swapped the
        // column and left `qa-environments-kubeconfig-…` sitting in credstore
        // with nothing naming it — the orphan `delete_ssh_key` exists to avoid,
        // and one this gear's own `delete_platform` already avoids. A caller
        // *replacing* the kubeconfig is replacing it either way; how they spell
        // the replacement does not change who owns the secret left behind.
        let previous_ref = match replacement {
            Some(_) => Some(
                self.repo
                    .get(&conn, &scope, id)
                    .await?
                    .ok_or(DomainError::PlatformNotFound { id })?
                    .kubeconfig_credstore_ref,
            ),
            None => None,
        };

        // `previous_ref` is `Some` exactly when the patch replaced the
        // kubeconfig, by either spelling - the same condition the cleanup
        // below reads it for. Captured here because that binding is consumed
        // by the `if let Some(previous)` further down, and the D4 write at the
        // end of this method needs the same answer.
        let replaced_kubeconfig = previous_ref.is_some();

        let (patch, written_ref) = match replacement {
            Some(KubeconfigSource::Material(material)) => {
                let stored = self.write_generated_secret(ctx, material).await?;
                (
                    PlatformPatch {
                        kubeconfig_credstore_ref: Some(stored.clone()),
                        ..patch
                    },
                    Some(stored),
                )
            }
            // A caller-supplied reference is already the one `patch` carries,
            // and nothing is written to credstore for it — see
            // `forget_owned_secret` on why a reference this gear did not mint is
            // never this gear's to create or destroy.
            Some(KubeconfigSource::Reference(_)) | None => (patch, None),
        };

        // "At most one default per product", the update-path half. Ordered before
        // the write for the reason `clear_default_for_product`'s doc gives: with no
        // transaction seam in this gear, zero defaults is recoverable and two is
        // ambiguous.
        //
        // The product to clear within is the one the row will *end up* in, so an
        // explicit `product_id` in this same patch wins over the stored value —
        // otherwise a patch that moves a platform and makes it default in one call
        // would clear the defaults of the product it is leaving.
        //
        // `Some(None)` (the patch clears the product association) and a stored
        // `None` both leave nothing to clear. A default flag on a product-less
        // platform is inert rather than rejected: resolution filters candidates by
        // `product_id`, so such a row is never a candidate for anything.
        if patch.is_default == Some(true) {
            let product_id = match patch.product_id {
                Some(explicit) => explicit,
                None => {
                    self.repo
                        .get(&conn, &scope, id)
                        .await?
                        .ok_or(DomainError::PlatformNotFound { id })?
                        .product_id
                }
            };
            if let Some(product_id) = product_id {
                self.repo
                    .clear_default_for_product(&conn, &scope, product_id, id)
                    .await?;
            }
        }

        let updated = match self.repo.update(&conn, &scope, id, patch).await {
            Ok(Some(updated)) => updated,
            Ok(None) => {
                if let Some(ref written) = written_ref {
                    self.forget_owned_secret(ctx, written, true).await;
                }
                return Err(DomainError::PlatformNotFound { id });
            }
            Err(e) => {
                if let Some(ref written) = written_ref {
                    self.forget_owned_secret(ctx, written, true).await;
                }
                return Err(e);
            }
        };

        // The row now points at whatever the caller asked for, so a *superseded*
        // reference is unreachable through this gear: drop it if it was ours to
        // drop. `previous_ref` is `Some` exactly when the patch replaced the
        // kubeconfig, by either spelling.
        //
        // The comparison against the reference the row **ended up with** is
        // load-bearing, not defensive tidiness: a patch may name the reference
        // already stored (a no-op rotation, or a retry), and deleting that would
        // destroy the secret the row still points at — a platform silently
        // broken by a request that changed nothing.
        if let Some(previous) = previous_ref
            && previous != updated.kubeconfig_credstore_ref
        {
            let generated = previous.starts_with(GENERATED_KUBECONFIG_REF_PREFIX);
            self.forget_owned_secret(ctx, &previous, generated).await;
        }

        // Decision D4, the other half of what `create_platform` does: a
        // rotated kubeconfig whose `Secret` still carries the OLD material is
        // worse than a missing one, because the runner mounts it happily and
        // then fails against the platform's cluster. Only on the branch that
        // actually replaced the kubeconfig: a patch that renamed the platform
        // changes neither the `Secret`'s name (derived from the credstore
        // reference) nor its contents, so there is nothing to converge.
        if replaced_kubeconfig {
            self.materialise_runner_secret(ctx, &updated, "update").await;
        }

        info!("Successfully updated platform");
        Ok(updated)
    }

    #[instrument(skip(self, ctx), fields(platform_id = %id))]
    pub async fn delete_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        info!("Deleting platform");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::DELETE, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // Precondition: a platform holding an active lease cannot be deleted.
        // The lease table is a different resource type than PLATFORM, so it
        // needs its own PEP-derived scope rather than reusing the DELETE
        // scope compiled for the platform read/write above.
        let lease_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::LEASE, actions::GET, Some(id))
            .await?;
        let lease = self.leases_repo.get(&conn, &lease_scope, id).await?;
        if !matches!(lease.state, LeaseState::Free) {
            return Err(DomainError::PlatformLeased { id });
        }

        // Resolve the row for its reference before deleting it. A gear-owned
        // secret has to go too, or deleting the platform leaves a secret in
        // credstore that nothing can ever name again — the orphan
        // `delete_ssh_key` exists to avoid. Reading first also means a missing
        // platform costs no credstore call.
        let existing_ref = self
            .repo
            .get(&conn, &scope, id)
            .await?
            .map(|p| p.kubeconfig_credstore_ref);

        let deleted = self.repo.delete(&conn, &scope, id).await?;

        if !deleted {
            return Err(DomainError::PlatformNotFound { id });
        }

        // Row first, then the secret — the opposite of `delete_ssh_key`, and
        // deliberately so. There, the reference is the row's whole payload and
        // a dangling one is worse than a briefly orphaned row. Here the
        // reference may be **caller-supplied and shared**, so deleting it
        // before the row is committed could destroy a secret another platform
        // (or another system) still uses while this delete goes on to fail its
        // precondition. `forget_owned_secret` deletes nothing this gear did
        // not mint.
        if let Some(previous) = existing_ref {
            let generated = previous.starts_with(GENERATED_KUBECONFIG_REF_PREFIX);
            self.forget_owned_secret(ctx, &previous, generated).await;
        }

        info!("Successfully deleted platform");
        Ok(())
    }

    /// Run one on-demand detection cycle against the platform's own cluster
    /// and persist the outcome, returning the refreshed row. Backs
    /// `POST /qa/v1/platforms/{id}/refresh`.
    ///
    /// # A detection failure is not an `Err`
    ///
    /// The platform's cluster being unreachable, or its vpadm install
    /// metadata being missing or malformed, is a **fact about the platform**,
    /// not a fault of this service. Such a failure is persisted into
    /// `version_detect_error` via
    /// [`PlatformsRepository::record_observation`] and returned inside the
    /// `Ok` row exactly like a successful detection — matching legacy's
    /// `api_refresh_version` (`manager/src/routes/platforms.rs:363`), which
    /// reserves a non-2xx response for a genuine server-side fault and
    /// answers every detection attempt, successful or not, with 200. The REST
    /// handler (`api::rest::handlers::platforms::refresh_platform`) relies on
    /// that: it maps `Ok` straight to a 200 body.
    ///
    /// # What *is* an `Err`
    ///
    /// The platform not existing (`DomainError::PlatformNotFound`, → 404),
    /// the caller lacking access, or — the case new to this method — the
    /// stored `kubeconfig_credstore_ref` failing to resolve back to material
    /// through credstore. That last one is this gear's own bookkeeping
    /// failing (a dangling or unreadable reference), not a fact about the
    /// platform's cluster, so it propagates as a genuine `DomainError` rather
    /// than being folded into the `ObservationOutcome` an operator reads.
    #[instrument(skip(self, ctx), fields(platform_id = %id))]
    pub async fn observe_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TargetPlatform, DomainError> {
        info!("Observing platform");

        // UPDATE, not GET: this method persists (`record_observation`) and
        // makes an outbound call to the platform's own cluster, exactly the
        // shape `update_platform`/`delete_platform` above gate on their own
        // mutating actions. A principal granted only GET must not be able to
        // drive a live-cluster round-trip and a database write through this
        // endpoint.
        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::UPDATE, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let platform = self
            .repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::PlatformNotFound { id })?;

        let observation = self.observe_cluster(ctx, &platform).await?;

        self.repo
            .record_observation(&conn, &scope, id, &observation)
            .await?;

        info!("Successfully observed platform");

        self.repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::PlatformNotFound { id })
    }

    /// Resolve the platform's kubeconfig out of credstore and hand it to the
    /// observer, under this platform's own resolved `VPADM_NAMESPACE`.
    ///
    /// `#[instrument]` skips nothing here on purpose — there is nothing to
    /// skip: neither the credstore response nor the observer's return value
    /// is ever passed to a tracing macro, so no field carries the material
    /// for a skip to guard.
    async fn observe_cluster(
        &self,
        ctx: &SecurityContext,
        platform: &TargetPlatform,
    ) -> Result<PlatformObservation, DomainError> {
        let material = match self
            .fetch_kubeconfig_material(ctx, platform.id, &platform.kubeconfig_credstore_ref)
            .await
        {
            Ok(material) => material,
            Err(error) => {
                // Recorded as an observation outcome rather than propagated.
                // Before this, a platform whose kubeconfig could not be
                // resolved failed *upstream* of the observer, so
                // `record_observation` was never reached and NOTHING was
                // written: the UI read "not yet observed" forever while the
                // real reason repeated in the log every ticker cycle. Measured
                // on the remote 2026-08-29 -- seven of eight platforms sat in
                // exactly that state. Cluster-health spec section 8.
                //
                // `debug!`, not `warn!`: the ticker's own loop already logs
                // one warning per failing platform per cycle, and this is the
                // same fact one layer down.
                tracing::debug!(
                    platform_id = %platform.id,
                    %error,
                    "kubeconfig could not be resolved; recording it as a failed observation"
                );
                return Ok(PlatformObservation {
                    platform: ObservationOutcome::Failed(
                        KUBECONFIG_UNRESOLVED.to_owned(),
                    ),
                    // Nothing looked at a cluster, so the health half is
                    // `NotAttempted` and every `cluster_*` column stays NULL.
                    // `Failed` here would publish `cluster_status =
                    // 'Unreachable'` -- a claim about a cluster nobody
                    // contacted, which is the exact defect the final
                    // whole-branch review caught on the feature-off build.
                    health: HealthOutcome::NotAttempted,
                });
            }
        };
        let namespace = self.vpadm_namespace_for(ctx, platform.id).await?;

        // Both halves of the `PlatformObservation` reach `record_observation`
        // unchanged (`Self::observe_platform`), and from there
        // `infra::storage::mapper::platform_to_sdk` maps the persisted
        // cluster-health columns onto `TargetPlatform::cluster` for every
        // caller — SDK, REST DTO, and this method's own return value alike.
        Ok(self.observer.observe(&material, &namespace).await)
    }

    /// Resolve `credstore_ref` back into the kubeconfig bytes it names.
    ///
    /// Split out of [`Self::observe_cluster`] so D4's `Secret` write
    /// ([`Self::materialise_runner_secret`], reached from create, from update
    /// and from the ticker's self-heal) can fetch the same material for
    /// [`PlatformObserver::ensure_kubeconfig_secret`] without duplicating this
    /// resolution logic — it is a second credstore round trip rather than a
    /// shared one, which is an acceptable cost on a create/update request and
    /// on a step that otherwise runs on a multi-minute cadence.
    async fn fetch_kubeconfig_material(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        credstore_ref: &str,
    ) -> Result<SecretValue, DomainError> {
        let secret_ref = SecretRef::new(credstore_ref.to_owned()).map_err(|e| {
            DomainError::Internal(format!(
                "platform {platform_id}'s stored kubeconfig reference is not a valid SecretRef: {e}"
            ))
        })?;

        let response = self
            .credstore
            .get(ctx, &secret_ref)
            .await
            .map_err(map_credstore_error)?
            .ok_or_else(|| {
                DomainError::Internal(format!(
                    "platform {platform_id}'s kubeconfig secret ({credstore_ref}) was not found \
                     in credstore"
                ))
            })?;

        Ok(response.value)
    }

    /// This platform's own vpadm install-metadata namespace: its
    /// `VPADM_NAMESPACE` variable (trimmed, non-empty) if it has one,
    /// otherwise [`DEFAULT_VPADM_NAMESPACE`].
    ///
    /// Mirrors legacy's `pick_vpadm_namespace`/`vpadm_namespace_override`
    /// precedence exactly (`manager/src/services/platforms.rs:1298-1313`) —
    /// see [`pick_vpadm_namespace`] for the pure rule this wraps with the real
    /// PEP-scoped read. Closes the simplification Task 7 had to accept: it
    /// hardcoded [`DEFAULT_VPADM_NAMESPACE`] because no task yet owned this
    /// lookup; this one does. The observer already falls back to an
    /// all-namespace scan when the targeted lookup misses
    /// (`KubeObserver::detect`), so a wrong or absent namespace here was never
    /// fatal — just an extra list call — and stays that way.
    async fn vpadm_namespace_for(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<String, DomainError> {
        // VARIABLE, not PLATFORM: this reads `qa_platform_variables`, a
        // different resource type/table, exactly as `VariablesService::list_for_env`
        // derives its own VARIABLE scope rather than reusing a PLATFORM one.
        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::VARIABLE, actions::LIST, None)
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let vars = self
            .variables_repo
            .list_for_platform(&conn, &scope, platform_id)
            .await?;

        Ok(pick_vpadm_namespace(&vars))
    }

    /// One self-heal pass over **every** platform this gear knows about,
    /// across every tenant: observe each against its own cluster, persist the
    /// outcome, and re-apply its kubeconfig `Secret` in the Argo cluster. This
    /// is the ticker's per-cycle body — see `crate::gear`'s `observation_ticker`
    /// for the supervised loop that calls it, and that module's `Tickers` doc
    /// for why the ticker's *role* is recorded at spawn time rather than read
    /// back from this method: the same reasoning applies here as there, one
    /// level up.
    ///
    /// # One platform's failure never aborts the cycle for the others
    ///
    /// Every platform gets its own `Result`, matched individually and logged
    /// on failure — a credstore miss, a PEP denial, an unreachable cluster, a
    /// `409` from the self-heal are all just one more warning line, never a
    /// `return` that would strand every platform after the one that failed.
    /// Only a panic partway through (which [`crate::gear`]'s supervisor
    /// reports, exactly as it would for any other ticker) can stop this from
    /// visiting every platform it started with.
    ///
    /// # No `SecurityContext` in, one out per platform
    ///
    /// Listing every platform across every tenant is this gear's own
    /// maintenance duty, not a caller's request — see
    /// [`crate::domain::repos::PlatformsRepository::list_all_with_tenant`]'s
    /// doc for why it is read under `AccessScope::allow_all()` rather than a
    /// scope resolved from any `SecurityContext`. Everything this method
    /// does with a given platform *afterwards* — resolving its kubeconfig,
    /// reading its `VPADM_NAMESPACE`, persisting what was observed — goes
    /// through the ordinary PEP-scoped path
    /// ([`Self::observe_platform`]/[`Self::vpadm_namespace_for`]), under a
    /// context bound to that platform's own tenant
    /// ([`system_actor::for_observation`]).
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the per-platform branches (nil tenant, observe Err, self-heal \
                  Err), each of which owes an operator a distinct log line - same diagnosis as \
                  qa-runs' `service::dispatch::run_tick`/`reconcile_claims`"
    )]
    #[cfg(any(feature = "platform-observation", test))]
    pub(crate) async fn run_observation_cycle(&self) -> ObservationCycleReport {
        let mut report = ObservationCycleReport::default();

        let conn = match self.db.conn() {
            Ok(conn) => conn,
            Err(error) => {
                warn!(
                    %error,
                    "qa-environments observation cycle: failed to acquire a database \
                     connection; skipping this cycle"
                );
                return report;
            }
        };

        // RATIFIED EXCEPTION to the crate-wide ban on `AccessScope::allow_all()`
        // (see `no_production_path_uses_allow_all_except_the_one_ratified_ticker_enumeration`,
        // `domain::service::unscoped_read_guard_tests` — that test enforces this is
        // the ONLY call site in this crate; a second one anywhere fails CI).
        //
        // Decided 2026-08-28 by the product owner, after Task 8 review flagged
        // this as an undocumented deviation from the crate's own ban. It is not
        // undocumented anymore, and it is not a convenience:
        //
        // * Subject: a system actor with no caller. This is the observation
        //   ticker's own enumeration step - "which platforms exist across every
        //   tenant" - run on this gear's own maintenance schedule, not in
        //   response to any request. There is no `SecurityContext` to derive a
        //   narrower scope from, unauthorized or otherwise: the crate's ban
        //   (`domain::service::variables`'s header, `variables_repo.rs:36`) is
        //   worded against unscoped queries driven by unauthenticated/
        //   unauthorized *input*, and there is no input here to be unauthorized.
        // * Reason: `qa-runs`'/`qa-insights`' sibling pattern - a nil-tenant
        //   context routed through `PolicyEnforcer` for the enumeration step -
        //   is `Forbidden` by design under both shipped `AuthZ` plugins with no
        //   policy authored for this gear's system actor, which would leave this
        //   ticker silently observing nothing on every deployment that exists
        //   today. That is the opposite of Task 8's own requirement (a
        //   deployment missing the feature must be legible, not mysterious) and
        //   this exception is what avoids inheriting a second, quieter way to
        //   be inert.
        // * Scope of the exception: metadata enumeration only - this read, and
        //   nothing downstream of it. A nil `tenant_id` on any row is skipped
        //   (see the loop below) rather than fed to
        //   `system_actor::for_observation`, and every step after this one -
        //   fetching a platform's kubeconfig, reading its `VPADM_NAMESPACE`,
        //   persisting its outcome - runs under a real, tenant-bound
        //   `SecurityContext` through the ordinary PEP-scoped path
        //   (`Self::observe_platform`), exactly as `list_all_with_tenant`'s own
        //   doc describes.
        // * Prior incident, named rather than glossed over: this crate is the
        //   one `qa-runs`' own `no_production_path_uses_allow_all` guard test
        //   cites as the origin of the ban - "a probe using `allow_all()`
        //   created a cross-tenant existence oracle in qa-environments". This
        //   exception is knowingly standing next to that scar, not unaware of
        //   it: the difference this time is that nothing here answers a
        //   caller's question with cross-tenant data - the result is used only
        //   to mint per-row, tenant-bound contexts for reads/writes that are
        //   independently scoped.
        //
        // Not a bypass of `SecureORM` either way: it is still routed through
        // the identical `.secure().scope_with(scope)` call every other read on
        // `PlatformsRepository` uses - `allow_all()` is the one `AccessScope`
        // value that applies no row-level filter, not a different code path.
        let scope = toolkit_security::AccessScope::allow_all();

        let platforms = match self.repo.list_all_with_tenant(&conn, &scope).await {
            Ok(platforms) => platforms,
            Err(error) => {
                warn!(
                    %error,
                    "qa-environments observation cycle: failed to list platforms; skipping \
                     this cycle"
                );
                return report;
            }
        };

        for (platform, tenant_id) in platforms {
            report.attempted += 1;

            if tenant_id.is_nil() {
                warn!(
                    platform_id = %platform.id,
                    platform_name = %platform.name,
                    "qa-environments observation cycle: this platform carries a nil tenant id; \
                     skipping it rather than observing it under an unscoped identity"
                );
                report.failed += 1;
                continue;
            }

            let ctx = system_actor::for_observation(tenant_id);

            match self.observe_platform(&ctx, platform.id).await {
                Ok(_) => report.observed += 1,
                Err(error) => {
                    warn!(
                        platform_id = %platform.id,
                        platform_name = %platform.name,
                        %error,
                        "qa-environments observation cycle: observing this platform failed; \
                         continuing with the rest"
                    );
                    report.failed += 1;
                }
            }

            self.self_heal_kubeconfig_secret(&ctx, &platform).await;
        }

        report
    }

    /// The ticker's per-cycle call into [`Self::materialise_runner_secret`],
    /// made **independently of whether this cycle's detection succeeded** —
    /// the self-heal decision D4 exists for
    /// (`crate::infra::observer::secret_writer`'s module doc): a pod stuck on
    /// `FailedMount` needs the `Secret` fixed regardless of what the
    /// platform's own cluster answered this cycle.
    ///
    /// It is a named wrapper rather than a direct call so the ticker's own
    /// body reads as what it is (`observe`, then `self-heal`), and so the
    /// `origin` tag every log line carries is written once here rather than
    /// spelled at the call site.
    #[cfg(any(feature = "platform-observation", test))]
    async fn self_heal_kubeconfig_secret(&self, ctx: &SecurityContext, platform: &TargetPlatform) {
        self.materialise_runner_secret(ctx, platform, "observation cycle self-heal")
            .await;
    }

    /// Decision D4's single write path: resolve `platform`'s kubeconfig out of
    /// credstore and ensure the `Secret` a runner pod mounts exists in the
    /// Argo cluster and carries that material.
    ///
    /// `origin` names which of the three callers asked — `create`, `update`,
    /// or `observation cycle self-heal` — and is attached to every log line,
    /// because "this platform's Secret could not be written" reads very
    /// differently depending on whether it happened once at create time or is
    /// repeating every cycle.
    ///
    /// # Called from all three places, deliberately
    ///
    /// The design (§4.6) says "on create, on update, and as a self-heal", and
    /// until 2026-08-28 only the self-heal existed: the final whole-branch
    /// review found the create/update half had been described in the plan
    /// (Task 6, Step 3) but never assigned to a task. The consequence was
    /// precisely the symptom D4 exists to remove — a platform created in the
    /// UI sitting `Pending` for up to `poll_interval_seconds` and then failing
    /// on `FailedMount` with no explanation.
    ///
    /// # A failure here never rolls anything back
    ///
    /// Logged and swallowed, exactly as [`Self::forget_owned_secret`] treats
    /// its own partial failure. A platform whose row exists and whose `Secret`
    /// is missing is *recoverable* — the ticker re-applies it on the next
    /// cycle, which is what the self-heal is for — while a platform that
    /// silently vanished because a write into a *different* cluster failed is
    /// not. That asymmetry is the whole reason this returns `()`: there is no
    /// `Err` for a caller to be tempted to unwind on.
    ///
    /// # The known `409` interaction
    ///
    /// `crate::infra::observer::secret_writer` applies without `force`, on
    /// purpose: forcing would let this writer stomp a `Secret` an operator
    /// deliberately created by hand (`kubectl create`, or the provisioning
    /// shell script) rather than through server-side apply. When that
    /// happens, the writer's own error text already names the `Secret`, the
    /// namespace, and what an operator should do about it (delete the
    /// hand-made `Secret` so this writer can own it) — see
    /// `secret_writer::describe_apply_failure`. This method's job is only to
    /// make sure that text reaches the log **with the platform attached**,
    /// so a self-heal that repeats every cycle reads as "platform X's Secret
    /// needs an operator" rather than an anonymous, repeating error.
    async fn materialise_runner_secret(
        &self,
        ctx: &SecurityContext,
        platform: &TargetPlatform,
        origin: &'static str,
    ) {
        let material = match self
            .fetch_kubeconfig_material(ctx, platform.id, &platform.kubeconfig_credstore_ref)
            .await
        {
            Ok(material) => material,
            Err(error) => {
                warn!(
                    platform_id = %platform.id,
                    platform_name = %platform.name,
                    origin,
                    %error,
                    "qa-environments: could not resolve this platform's kubeconfig, so its \
                     runner kubeconfig Secret (D4) was left untouched; the observation \
                     ticker will try again on its next cycle"
                );
                return;
            }
        };

        if let Err(message) = self
            .observer
            .ensure_kubeconfig_secret(&platform.kubeconfig_credstore_ref, &material)
            .await
        {
            warn!(
                platform_id = %platform.id,
                platform_name = %platform.name,
                origin,
                error = %message,
                "qa-environments: failed to apply this platform's runner kubeconfig Secret \
                 (D4); a run dispatched before the observation ticker's next cycle repairs \
                 it will hang on FailedMount"
            );
        }
    }

    /// Resolve the two mutually-exclusive kubeconfig inputs into one source,
    /// or the validation error the combination deserves.
    ///
    /// An empty `kubeconfig_credstore_ref` counts as **not supplied**, which
    /// is exactly what it meant before this field became optional: it was a
    /// required `String` whose only validation was
    /// [`Self::validate_credstore_ref`]'s `is_empty` rejection. So a create
    /// carrying neither still fails with that same
    /// `kubeconfig_credstore_ref: must not be empty`.
    fn resolve_kubeconfig_source(
        credstore_ref: Option<String>,
        material: Option<KubeconfigMaterial>,
    ) -> Result<KubeconfigSource, DomainError> {
        match (credstore_ref.filter(|r| !r.is_empty()), material) {
            (Some(_), Some(_)) => Err(Self::both_kubeconfig_fields_error()),
            (None, Some(material)) => Ok(KubeconfigSource::Material(Self::validate_material(
                material,
            )?)),
            (Some(credstore_ref), None) => Ok(KubeconfigSource::Reference(credstore_ref)),
            (None, None) => Err(DomainError::Validation {
                field: "kubeconfig_credstore_ref".to_owned(),
                message: "must not be empty".to_owned(),
            }),
        }
    }

    /// The "both supplied" rejection. Names **both** fields, because the
    /// caller has to know which one to drop and `DomainError::Validation`
    /// carries only a single `field`.
    fn both_kubeconfig_fields_error() -> DomainError {
        DomainError::Validation {
            field: "kubeconfig".to_owned(),
            message: "exactly one of 'kubeconfig' and 'kubeconfig_credstore_ref' may be supplied, \
                      not both"
                .to_owned(),
        }
    }

    /// Reject a blank document. Mirrors `create_ssh_key`'s
    /// `private_key_pem.trim().is_empty()` check — and, like it, runs before
    /// anything reaches credstore.
    fn validate_material(material: KubeconfigMaterial) -> Result<KubeconfigMaterial, DomainError> {
        if material.expose().trim().is_empty() {
            return Err(DomainError::Validation {
                field: "kubeconfig".to_owned(),
                message: "must not be empty".to_owned(),
            });
        }
        Ok(material)
    }

    /// Turn a resolved source into the reference the database will hold,
    /// writing the document to credstore first when there is one.
    async fn store_kubeconfig_source(
        &self,
        ctx: &SecurityContext,
        source: KubeconfigSource,
    ) -> Result<StoredKubeconfig, DomainError> {
        match source {
            KubeconfigSource::Reference(reference) => Ok(StoredKubeconfig {
                reference,
                generated: false,
            }),
            KubeconfigSource::Material(material) => Ok(StoredKubeconfig {
                reference: self.write_generated_secret(ctx, material).await?,
                generated: true,
            }),
        }
    }

    /// Write a pasted document to credstore under a freshly generated
    /// reference and return that reference — the only value that goes on to
    /// the database.
    ///
    /// `SharingMode::Tenant` matches `create_ssh_key`; the caveats that mode
    /// carries are documented at length in `ssh_keys.rs`'s module header and
    /// apply here identically.
    async fn write_generated_secret(
        &self,
        ctx: &SecurityContext,
        material: KubeconfigMaterial,
    ) -> Result<String, DomainError> {
        let raw_ref = format!(
            "{GENERATED_KUBECONFIG_REF_PREFIX}{}",
            Uuid::new_v4().simple()
        );
        let secret_ref = SecretRef::new(raw_ref.clone())
            .map_err(|e| DomainError::Internal(format!("generated secret ref invalid: {e}")))?;
        self.credstore
            .create(
                ctx,
                &secret_ref,
                SecretValue::from(material.into_inner()),
                SharingMode::Tenant,
            )
            .await
            .map_err(map_credstore_error)?;
        Ok(raw_ref)
    }

    /// Best-effort delete of a credstore secret this gear owns.
    ///
    /// `generated` is the ownership test, not a convenience flag: only a
    /// reference this gear minted (prefix
    /// [`GENERATED_KUBECONFIG_REF_PREFIX`]) is ours to remove. A
    /// caller-supplied reference such as `credstore://team-a/prod-cluster` may
    /// name a secret shared with other platforms or other systems entirely, so
    /// deleting it on our own initiative would destroy someone else's
    /// credential. `create_ssh_key` needs no such test because its references
    /// are *always* generated — there is no way to hand it one.
    ///
    /// Failures are logged, never propagated: every call site has already
    /// committed the outcome the caller asked for, and turning a cleanup
    /// failure into an error would report a success as a failure.
    async fn forget_owned_secret(&self, ctx: &SecurityContext, raw_ref: &str, generated: bool) {
        if !generated {
            return;
        }
        let Ok(secret_ref) = SecretRef::new(raw_ref.to_owned()) else {
            warn!("Generated kubeconfig reference no longer parses; leaving the secret in place");
            return;
        };
        match self
            .credstore
            .delete(ctx, &secret_ref, WritePrecondition::Exists)
            .await
        {
            // Already gone is success for a cleanup (idempotent retry after a
            // partial failure), exactly as `delete_ssh_key` treats it.
            Ok(()) | Err(CredStoreError::NotFound | CredStoreError::Conflict) => {}
            Err(e) => {
                warn!(error = %e, "Failed to remove the platform's kubeconfig secret from credstore");
            }
        }
    }

    fn validate_name(name: &str) -> Result<(), DomainError> {
        if name.is_empty() {
            return Err(DomainError::Validation {
                field: "name".to_owned(),
                message: "must not be empty".to_owned(),
            });
        }
        if name.len() > MAX_NAME_LEN {
            return Err(DomainError::Validation {
                field: "name".to_owned(),
                message: format!("must not exceed {MAX_NAME_LEN} characters"),
            });
        }
        Ok(())
    }

    /// Reject an override too long to survive the whole path it feeds, so the
    /// failure is a named 400 here rather than a 500 in another gear later.
    ///
    /// This is a **deliberate addition, not a port**: the source column is
    /// unbounded `TEXT` (`manager/migrations/001_initial.sql:233`), so the source
    /// system needs no such check. Two things make it worth having anyway.
    ///
    /// **The bound is the downstream sink's, not this column's alone.** This
    /// value's purpose is to become a run's branch label — `resolve_branch`
    /// returns it and `qa-runs`' `launch.rs:1693` assigns it as `test_version`,
    /// landing in `qa_runs.test_version VARCHAR(512)`. So an override wider than
    /// 512 would be accepted here and then break **every launch that used it**,
    /// on an insert this gear never sees.
    /// `m20260814_000006_platform_default_branch` declares this column
    /// `VARCHAR(512)` for the same reason, and its module doc records why the
    /// sink's width is the anchor. (An earlier version of both used 255,
    /// justified by a claimed `qa_platforms` "convention" that does not exist:
    /// `description` is `TEXT` and `kubeconfig_credstore_ref` is `VARCHAR(1024)`.)
    ///
    /// **Characters, not bytes** — `MAX_DEFAULT_BRANCH_CHARS` is compared against
    /// `chars().count()`, because `VARCHAR(512)` counts characters on both server
    /// dialects. The earlier version compared `len()`, i.e. bytes, which rejected
    /// a 256-character Cyrillic branch name that both columns store perfectly
    /// well while telling the operator it exceeded a limit in "characters" that it
    /// did not. Git refs are UTF-8 and non-ASCII branch names are legal, so that
    /// was a real, reachable false rejection rather than a theoretical one.
    ///
    /// The half worth having regardless of dialect is the **silent** one: outside
    /// strict mode `MySQL` truncates rather than errors, and a truncated ref is a
    /// *different, valid-looking* branch name, so the run would sync the wrong
    /// branch with no error anywhere — the same silent-divergence class this whole
    /// field was added to fix.
    ///
    /// # What is and is not tested
    ///
    /// This function's own boundary is tested and break-tested: 513 rejected on
    /// **both** write paths, 512 accepted on **create**. The accept case is not
    /// re-exercised through the patch path because both paths call this one
    /// function, so a per-path off-by-one is not expressible; an earlier version of
    /// this sentence distributed "on both write paths" across both halves and so
    /// claimed a test that does not exist. The **database** half is not tested, and cannot be
    /// here: the gear's tests run on `SQLite`, where this column is declared
    /// `TEXT` and no width exists to enforce. That the guard is what stops the
    /// over-long write is *observed* rather than argued — disabling it makes the
    /// over-length create **succeed** and store the value in full.
    ///
    /// The Postgres error and the `MySQL` truncation are stated from those
    /// engines' documented semantics and are exercised by **no test in this
    /// repository**. No claim is made about what any git hosting provider permits.
    fn validate_default_branch(value: Option<&str>) -> Result<(), DomainError> {
        if value.is_some_and(|branch| branch.chars().count() > MAX_DEFAULT_BRANCH_CHARS) {
            return Err(DomainError::Validation {
                field: "default_branch".to_owned(),
                message: format!("must not exceed {MAX_DEFAULT_BRANCH_CHARS} characters"),
            });
        }
        Ok(())
    }

    /// Trim a supplied `default_branch` and treat an empty or whitespace-only
    /// result as "no override".
    ///
    /// Both of the source system's write paths do exactly this before touching
    /// the column — `create_platform` at
    /// `manager/src/services/platforms.rs:385-392` and `update_platform` via its
    /// `"__NULL__"` sentinel at `:447-454` — so the stored value is always
    /// either `NULL` or a trimmed, non-empty string. Normalising here, in the
    /// service, rather than at the REST boundary means SDK and local-client
    /// callers get the same guarantee as HTTP ones; the repository layer is
    /// then free to write whatever it is handed.
    ///
    /// A consequence worth stating because it looks like a contradiction and is
    /// not: qa-runs' `resolve_branch` **also** trims and filters this value
    /// (`qa-runs/src/domain/service/launch.rs`, rule 1's middle tier). Given
    /// this normaliser that filter can never fire on a stored platform, so it is
    /// **defensively redundant, not disagreeing** — the source system carries the
    /// same redundancy for the same reason (`exclusivity.rs:310-311` filters a
    /// value `platforms.rs` already normalised). Neither side should be
    /// "simplified" on the strength of the other: they are in different gears
    /// and this one's guarantee is not a type the other can see.
    fn normalize_default_branch(value: Option<String>) -> Option<String> {
        value.and_then(|branch| {
            let trimmed = branch.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        })
    }

    fn validate_credstore_ref(value: &str) -> Result<(), DomainError> {
        if value.is_empty() {
            return Err(DomainError::Validation {
                field: "kubeconfig_credstore_ref".to_owned(),
                message: "must not be empty".to_owned(),
            });
        }
        Ok(())
    }
}

/// Map a credstore failure onto this gear's error vocabulary.
///
/// `AccessDenied` is the caller's own authorization failing, so it stays a
/// 403; everything else is infrastructure and becomes a bare 500 (the
/// message is kept for the log, not the client — see `api::rest::error`).
/// Mirrors `qa-catalog`'s function of the same name in `ssh_keys.rs`.
fn map_credstore_error(e: CredStoreError) -> DomainError {
    match e {
        CredStoreError::AccessDenied => DomainError::Forbidden,
        other => DomainError::CredStore(other.to_string()),
    }
}

/// Pure precedence rule for the vpadm install-metadata namespace: the
/// platform's own [`VPADM_NAMESPACE_VAR`] variable (trimmed, non-empty) wins;
/// otherwise [`DEFAULT_VPADM_NAMESPACE`].
///
/// Mirrors legacy's `pick_vpadm_namespace`/`vpadm_namespace_override` exactly
/// (`manager/src/services/platforms.rs:1298-1313`) — including the detail
/// that a *present but blank* override does not count as "set" any more than
/// an absent one does.
fn pick_vpadm_namespace(vars: &[Variable]) -> String {
    vars.iter()
        .find(|v| v.name == VPADM_NAMESPACE_VAR)
        .map(|v| v.value.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_VPADM_NAMESPACE.to_owned())
}

/// Outcome counters for one [`PlatformsService::run_observation_cycle`] pass —
/// how many platforms the ticker's `debug!` cycle-summary line, and this
/// module's own tests, can report on without re-deriving them from logs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg(any(feature = "platform-observation", test))]
pub struct ObservationCycleReport {
    /// Every platform the cycle attempted, whether or not it succeeded.
    pub attempted: u32,
    /// Platforms for which [`PlatformsService::observe_platform`] returned
    /// `Ok` — a detection *outcome* was persisted, successful or not (see
    /// that method's own doc on why a detection failure is still `Ok`).
    pub observed: u32,
    /// Platforms for which [`PlatformsService::observe_platform`] itself
    /// returned `Err`, or which were skipped outright (a nil tenant id).
    pub failed: u32,
}
