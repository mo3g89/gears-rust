//! Target environments (systems under test) service.
//!
//! # The kubeconfig is secret material, and this module is where it stays
//!
//! An environment's kubeconfig can arrive two ways: as a credstore **reference**
//! the caller already holds, or as a pasted **document**
//! ([`CredentialMaterial`]). A kubeconfig's `users[].user.client-key-data` is
//! a client private key, so a pasted document is treated exactly as
//! `qa-catalog` treats a pasted SSH private key
//! (`qa-catalog/src/domain/service/ssh_keys.rs`): it goes to credstore
//! **first**, and only the generated reference reaches
//! `qa_environments.kubeconfig_credstore_ref`.
//!
//! **The invariant is that the material never leaves the gear — not that it
//! is never read.** [`Self::observe_environment`] is the first read path that
//! resolves a reference back into the material: it fetches the stored
//! kubeconfig from credstore and hands it to the environment's product plugin
//! ([`Self::observe_through_plugin`]), which builds whatever client it needs
//! from it to read the environment's own systems. That is still true to the original promise, which was always
//! about what a *caller* can get back, not about whether this process may
//! look at bytes it already owns: no DTO, no REST response, no log line, and
//! no error message ever carries the document or the client built from it —
//! see `environments_kubeconfig_tests`'
//! `the_document_never_reaches_a_log_line_a_debug_rendering_or_a_response_body`
//! for the read path's half of that proof, and
//! `infra::runner_secret_writer`'s
//! `the_kubeconfig_material_never_reaches_an_error_message` for D4's write.
//!
//! Three things keep it from leaving, and none of them is a convention
//! someone has to remember:
//!
//! * [`CredentialMaterial`]'s `Debug` prints `[REDACTED]`, so the derived
//!   `Debug` of [`NewEnvironment`]/[`EnvironmentPatch`] — and of anything holding
//!   one — is redacted too;
//! * every `#[instrument]` below skips the argument carrying it (`new`,
//!   `patch`) or the response carrying the resolved material
//!   (`observe_environment`'s internal fetch never logs the `SecretValue` it
//!   gets back, which itself redacts in any `Debug`/`Display` formatting);
//! * the resolved value handed to the repository layer is a `String`
//!   **reference**, and the material is cleared out of the struct that
//!   travels there — and the same is true one step further in now:
//!   the plugin's own `observe` signature takes the material and returns
//!   `qa_product_sdk::PluginObservation`, a plain value with no
//!   kubeconfig-shaped field anywhere in it, so the boundary between "has the
//!   material" and "reports what it found" is a type, not a promise.

use std::sync::Arc;

use credstore_sdk::{
    CredStoreClientV1, CredStoreError, SecretRef, SecretValue, SharingMode, WritePrecondition,
};
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use tracing::{debug, info, instrument, warn};

use crate::domain::error::DomainError;
use crate::domain::ports::RunnerSecretWriter;
use crate::domain::repos::{EnvironmentsRepository, LeasesRepository};
use crate::domain::service::DbProvider;
use crate::domain::system_actor;
use authz_resolver_sdk::PolicyEnforcer;
use qa_product_sdk::QaProductPluginV1;
// `sole_required_secret_key` moved into the SDK at Task 18, because `qa-runs`
// needs the identical derivation for `prepare_run_access` and two private
// copies of one rule is the coupling class this branch keeps finding. Every
// test in this module that called the local function still calls this one.
// The plugin contract's `ObservationOutcome`/`HealthOutcome` collide by name
// with this gear's own (`crate::domain::ports`), which this file also uses --
// `NoopRunnerSecretWriter`'s outcome, and the doubles in the tests. Different types with
// different shapes: the local `Failed` carries a `String`, the SDK's a
// `PluginFailure`. Aliased rather than shadowing, so no match arm in this file
// is ambiguous to a reader.
use qa_product_sdk::observation::{
    FailureClass, HealthOutcome as PluginHealthOutcome,
    ObservationOutcome as PluginObservationOutcome, PluginFailure, PluginObservation,
};
use qa_product_sdk::plugin::{CredentialSlot, EnvironmentHandle};

use crate::domain::observation_write::ObservationWrite;
use crate::domain::ports::ProductPluginPort;

use super::{actions, resources};
use qa_environments_sdk::{
    CredentialMaterial, Environment, EnvironmentPatch, LeaseState, NewEnvironment,
};
use std::collections::BTreeMap;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Task 18b's plugin-driven credential write path.
///
/// A **child** module rather than a sibling of this one, and that is load-
/// bearing: it needs `self.credstore` and `self.product_plugins`, which are
/// private to this module. A child sees them; a sibling would have forced
/// those fields to `pub(super)` and widened the surface for every module under
/// `domain::service`.
#[path = "environment_credentials.rs"]
pub(super) mod credentials;

const MAX_NAME_LEN: usize = 255;

/// Prefix of every credstore reference **this gear generates** for a pasted
/// credential, shaped like `qa-catalog`'s `qa-catalog-ssh-key-{uuid}`.
///
/// It is also the ownership marker: a reference carrying it was minted here
/// from a document this gear was handed, so this gear is responsible for
/// deleting it. A reference *without* it was supplied by a caller, may be
/// shared with other systems, and is never deleted by this gear — see
/// [`EnvironmentsService::forget_owned_secret`].
///
/// # It was `qa-environments-kubeconfig-` until Task 18b
///
/// The old prefix named one product's credential in the gear whose whole
/// purpose is to stop doing that. **Both prefixes still mark ownership**,
/// because every reference minted before Task 18b carries the old one and
/// ownership decides whether a rotation deletes the superseded secret — see
/// [`credentials::is_generated_ref`], which is the only place either prefix
/// should be tested.
pub(super) const GENERATED_CREDENTIAL_REF_PREFIX: &str = "qa-environments-credential-";

// Task 18b removed `KubeconfigSource` and `StoredKubeconfig` from here.
//
// They expressed "which of the two mutually-exclusive kubeconfig inputs did
// the caller supply" and "the reference that reaches the database, plus
// whether this gear minted it" -- for exactly one credential. Both are now
// per-key facts of a form that may carry *n*: the first is
// `qa_environments_sdk::CredentialSubmission`, whose two arms make "exactly
// one of" unrepresentable rather than validated, and the second is
// `credentials::StoredCredentials`, which carries the minted and superseded
// references as lists. Keeping either alongside the new path would have split
// secret ownership across two mechanisms (ruling F-8).

/// Maximum `default_branch` length, **in characters**, matching both
/// `qa_environments.default_branch VARCHAR(512)` and the
/// `qa_runs.test_version VARCHAR(512)` column this value is resolved into — see
/// `validate_default_branch`, and `m20260814_000006_platform_default_branch`'s
/// module doc for why the sink's width is the anchor rather than any convention
/// in this table.
const MAX_DEFAULT_BRANCH_CHARS: usize = 512;

/// Target environments (systems under test) service.
///
/// # Design
///
/// Services acquire database connections internally via `DBProvider`. Callers
/// call service methods with business parameters only - no DB objects.
#[domain_model]
pub struct EnvironmentsService<P: EnvironmentsRepository, L: LeasesRepository> {
    db: Arc<DbProvider>,
    repo: Arc<P>,
    leases_repo: Arc<L>,
    credstore: Arc<dyn CredStoreClientV1>,
    /// D4's runner-`Secret` writer, and **only** that: an environment is
    /// observed through its product's plugin
    /// ([`Self::observe_through_plugin`]), and Task 19 deleted the observation
    /// half of the old port outright. What survives is
    /// [`RunnerSecretWriter::ensure_kubeconfig_secret`], called from create,
    /// from update and from the ticker's self-heal — see ruling F-19 for why
    /// deleting this half too would have left every workflow run hanging on
    /// `FailedMount`.
    observer: Arc<dyn RunnerSecretWriter>,
    /// Turns an environment's product into the plugin that observes it.
    /// Resolved lazily from the `ClientHub` per call — see
    /// `infra::product_plugin` for why this gear must not fail to boot when
    /// `qa-catalog` is absent.
    product_plugins: Arc<dyn ProductPluginPort>,
    policy_enforcer: PolicyEnforcer,
}

/// What a caller is told when an environment's kubeconfig cannot be resolved.
///
/// # Fixed text, and why it must stay fixed
///
/// This lands in `qa_environments.version_detect_error`, which is published on
/// `EnvironmentDto` to every `qa.platform` GET/LIST-authorized caller and rendered
/// on the environment page. The `DomainError` it replaces reads
/// `"environment {id}'s kubeconfig secret ({credstore_ref}) was not found in
/// credstore"` -- and **`credstore_ref` is precisely the value that is banned
/// from `EnvironmentDto`**: under `SharingMode::Tenant` the reference *is* a read
/// path to the kubeconfig, which is why `From<Environment>` drops it and
/// carries a "do not add it back" comment. Formatting that error into this
/// column would publish the reference by the back door.
///
/// The other arm of the same `Result` is worse still: `map_credstore_error`
/// wraps whatever the credstore client's `Display` produced, which is not a
/// bounded string this crate controls.
///
/// So neither is interpolated. This is the same rule
/// `infra::runner_secret_errors` implements for kubeconfig-derived failures, applied
/// one layer up, where the value at risk is the reference rather than the
/// material. It says what an operator can act on without naming the secret.
const KUBECONFIG_UNRESOLVED: &str = "this environment's kubeconfig could not be read from the credential store, so no \
     observation was attempted: the stored reference names no secret, or the credential \
     store refused the read. Re-save the environment with its kubeconfig to provision it.";

/// What an environment is told when its product's plugin declares no single
/// required secret field for the one legacy credential reference to bind to.
///
/// Unreachable for a product whose plugin declares exactly one required secret
/// (every plugin in this tree today), and for any environment whose
/// `credentials` column is populated. See
/// [`EnvironmentsService::resolve_credential_slots`] for why guessing is worse
/// than saying so.
const LEGACY_CREDENTIAL_UNBINDABLE: &str = "this environment still holds its credential reference in the pre-plugin column, and its \
     product's plugin does not declare exactly one required secret field for that reference to \
     belong to. Re-save the environment's credentials so each one is stored under its own key.";

impl<P: EnvironmentsRepository, L: LeasesRepository> EnvironmentsService<P, L> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<P>,
        leases_repo: Arc<L>,
        credstore: Arc<dyn CredStoreClientV1>,
        observer: Arc<dyn RunnerSecretWriter>,
        product_plugins: Arc<dyn ProductPluginPort>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repo,
            leases_repo,
            credstore,
            observer,
            product_plugins,
            policy_enforcer,
        }
    }
}

// Business logic methods
impl<P: EnvironmentsRepository, L: LeasesRepository> EnvironmentsService<P, L> {
    #[instrument(skip(self, ctx), fields(environment_id = %id))]
    pub async fn get_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Environment, DomainError> {
        debug!("Getting environment by id");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::GET, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        self.repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::EnvironmentNotFound { id })
    }

    /// One page of the environments the caller may list, ordered by name.
    ///
    /// # The scope is resolved before the filter, and that ordering is the point
    ///
    /// The `AccessScope` is compiled from the PDP's decision *here*, and the
    /// repository composes the caller's `OData` query on top of it — never
    /// instead of it. So a `$filter` can only ever narrow what this returns; it
    /// cannot widen it, and the type system enforces that rather than this
    /// comment (`EnvironmentsRepository::list_page` takes an already-scoped
    /// select). Same ordering, same reason, as `qa-runs`' `RunsService::list`.
    ///
    /// **This used to be an unbounded read** — review finding #55, and
    /// `cpt-cf-qa-nfr-scale`'s first number (*100 platforms*) is about exactly
    /// this collection.
    #[instrument(skip(self, ctx, query))]
    pub async fn list_environments(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Environment>, DomainError> {
        debug!("Listing environments");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::LIST, None)
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let page = self.repo.list_page(&conn, &scope, query).await?;

        debug!("Successfully listed {} environments", page.items.len());
        Ok(page)
    }

    /// Register an environment from either a credstore reference the caller holds
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
    pub async fn create_environment(
        &self,
        ctx: &SecurityContext,
        new: NewEnvironment,
    ) -> Result<Environment, DomainError> {
        info!("Creating new environment");

        Self::validate_name(&new.name)?;
        // Normalise before validating, so the length check sees exactly what
        // will be stored rather than the caller's untrimmed text.
        let new = NewEnvironment {
            default_branch: Self::normalize_default_branch(new.default_branch),
            ..new
        };
        Self::validate_default_branch(new.default_branch.as_deref())?;

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::CREATE, None)
            .await?;

        // Everything that can reject the request has now run, so nothing
        // rejected ever reaches credstore. Task 18b keeps that ordering: the
        // plugin's verdict on the submitted form is also taken before any
        // write, inside `store_submitted_credentials`.
        let stored = self.resolve_new_credentials(ctx, &new).await?;

        // The submissions are resolved above and **must not travel any further
        // towards the repository layer** — `CredentialSubmission::Material` is
        // plaintext. `PersistedCredentials` is what goes on; it has no arm
        // that can hold material (ruling F-9).
        let new = NewEnvironment {
            kubeconfig: None,
            credentials: BTreeMap::new(),
            ..new
        };

        let conn = self.db.conn().map_err(DomainError::from)?;

        let tenant_id = ctx.subject_tenant_id();

        // "At most one default per product". Cleared BEFORE the create, not after:
        // this gear has no transaction seam, and a failure between the two
        // statements should leave the product with zero defaults (recoverable, and
        // the dialogs fall back) rather than two (ambiguous). See
        // `EnvironmentsRepository::clear_default_for_product`.
        //
        // `Uuid::nil()` as the exclusion: the row being created has no id yet and
        // cannot be among the rows to clear, so there is nothing to exclude — and a
        // nil id matches no environment, every id being `Uuid::new_v4()`.
        if new.is_default {
            let product_id = new.product_id;
            self.repo
                .clear_default_for_product(&conn, &scope, product_id, Uuid::nil())
                .await?;
        }

        let created = self
            .repo
            .create(&conn, &scope, tenant_id, new, stored.persisted.clone())
            .await;

        match created {
            Ok(environment) => {
                info!(
                    "Successfully created environment with id={}",
                    environment.id
                );
                // Decision D4, on the path that needs it most: without this a
                // UI-created environment has no runner `Secret` until the next
                // ticker cycle (default 300s), which IS the ~5-minute
                // `FailedMount` window D4 exists to eliminate. Logged and
                // swallowed - see `materialise_runner_secret`'s doc for why a
                // failure here must not undo the create.
                self.materialise_runner_secret(ctx, &environment, "create")
                    .await;
                Ok(environment)
            }
            Err(db_err) => {
                // Don't orphan the secrets we just wrote when the row fails
                // (the realistic cause is a duplicate name): best-effort
                // cleanup, then propagate the original error —
                // `create_ssh_key`'s compensating delete, for its reasons.
                //
                // `minted` holds only what this call wrote, so a
                // caller-supplied reference is never touched: it may name a
                // secret shared with other systems, and this gear did not mint
                // it. Every minted reference is deleted, not just the first —
                // a plugin may declare several secret fields.
                self.forget_minted_secrets(ctx, &stored.minted).await;
                Err(db_err)
            }
        }
    }

    /// Apply a partial update, optionally replacing the kubeconfig.
    ///
    /// A pasted replacement follows the same order as create — new secret
    /// first, then the row — so a failed credstore write leaves the environment
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
    #[instrument(skip(self, ctx, patch), fields(environment_id = %id))]
    pub async fn update_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: EnvironmentPatch,
    ) -> Result<Environment, DomainError> {
        info!("Updating environment");

        if let Some(ref name) = patch.name {
            Self::validate_name(name)?;
        }
        // Fold the pre-plugin pair into the plugin-shaped map, so exactly one
        // mechanism handles n >= 1 credentials from here on (ruling F-8). The
        // pair's own rules are preserved verbatim by
        // `desugar_legacy_credential_pair`, including the two that were
        // measured: an empty `kubeconfig_credstore_ref` counts as **not
        // supplied**, and naming the field with an empty value is still the
        // `must not be empty` rejection rather than "leave it alone".
        let patch = EnvironmentPatch {
            // Normalise inside the `Some`, never across it: the outer layer
            // carries "the caller mentioned this field at all" and collapsing
            // it here would silently turn a clear into a no-op — the exact
            // failure this field was added to fix, one level up.
            default_branch: patch.default_branch.map(Self::normalize_default_branch),
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

        // Resolve the row before writing anything, so the displaced references
        // are known (they may need deleting) and a missing environment costs no
        // credstore write at all.
        //
        // This read happens for a **new reference** as much as for a paste, and
        // that is the whole point. It used to happen only for a paste, and the
        // consequence was measured: `create(pasted)` then
        // `update(kubeconfig_credstore_ref: "team-a-prod-cluster")` swapped the
        // column and left `qa-environments-kubeconfig-…` sitting in credstore
        // with nothing naming it — the orphan `delete_ssh_key` exists to avoid,
        // and one this gear's own `delete_environment` already avoids. A caller
        // *replacing* a credential is replacing it either way; how they spell
        // the replacement does not change who owns the secret left behind.
        //
        // `stored` is `Some` exactly when the patch replaced at least one
        // credential — the condition both cleanups and the D4 write below read.
        let stored = self
            .resolve_patch_credentials(ctx, &scope, &conn, id, &patch)
            .await?;
        let replaced_credential = stored.is_some();

        // The submissions must not travel any further towards the repository
        // layer; only `PersistedCredentials` goes on (ruling F-9).
        let patch = EnvironmentPatch {
            kubeconfig: None,
            credentials: BTreeMap::new(),
            ..patch
        };

        // "At most one default per product", the update-path half. Ordered before
        // the write for the reason `clear_default_for_product`'s doc gives: with no
        // transaction seam in this gear, zero defaults is recoverable and two is
        // ambiguous.
        //
        // The product to clear within is the one the row will *end up* in, so an
        // explicit `product_id` in this same patch wins over the stored value —
        // otherwise a patch that moves an environment and makes it default in one call
        // would clear the defaults of the product it is leaving.
        //
        // `Some(None)` (the patch clears the product association) and a stored
        // `None` both leave nothing to clear. A default flag on a product-less
        // environment is inert rather than rejected: resolution filters candidates by
        // `product_id`, so such a row is never a candidate for anything.
        if patch.is_default == Some(true) {
            let product_id = match patch.product_id {
                Some(explicit) => explicit,
                None => {
                    self.repo
                        .get(&conn, &scope, id)
                        .await?
                        .ok_or(DomainError::EnvironmentNotFound { id })?
                        .product_id
                }
            };
            self.repo
                .clear_default_for_product(&conn, &scope, product_id, id)
                .await?;
        }

        let persisted = stored.as_ref().map(|stored| stored.persisted.clone());
        let minted: &[String] = stored.as_ref().map_or(&[], |stored| &stored.minted);

        let updated = match self.repo.update(&conn, &scope, id, patch, persisted).await {
            Ok(Some(updated)) => updated,
            Ok(None) => {
                self.forget_minted_secrets(ctx, minted).await;
                return Err(DomainError::EnvironmentNotFound { id });
            }
            Err(e) => {
                self.forget_minted_secrets(ctx, minted).await;
                return Err(e);
            }
        };

        // The row now points at whatever the caller asked for, so a
        // *superseded* reference is unreachable through this gear: drop each
        // one that was ours to drop.
        //
        // `superseded` already excludes a reference the row **ended up with**,
        // and that exclusion is load-bearing rather than defensive tidiness: a
        // patch may name the reference already stored (a no-op rotation, or a
        // retry), and deleting that would destroy the secret the row still
        // points at — an environment silently broken by a request that changed
        // nothing. See `merge_credentials`, which is where the comparison
        // happens now that it is per key.
        if let Some(ref stored) = stored {
            for previous in &stored.superseded {
                let generated = credentials::is_generated_ref(previous);
                self.forget_owned_secret(ctx, previous, generated).await;
            }
        }

        // Decision D4, the other half of what `create_environment` does: a
        // rotated kubeconfig whose `Secret` still carries the OLD material is
        // worse than a missing one, because the runner mounts it happily and
        // then fails against the environment's cluster. Only on the branch that
        // actually replaced the kubeconfig: a patch that renamed the environment
        // changes neither the `Secret`'s name (derived from the credstore
        // reference) nor its contents, so there is nothing to converge.
        if replaced_credential {
            self.materialise_runner_secret(ctx, &updated, "update")
                .await;
        }

        info!("Successfully updated environment");
        Ok(updated)
    }

    #[instrument(skip(self, ctx), fields(environment_id = %id))]
    pub async fn delete_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        info!("Deleting environment");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::DELETE, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // Precondition: an environment holding an active lease cannot be deleted.
        // The lease table is a different resource type than PLATFORM, so it
        // needs its own PEP-derived scope rather than reusing the DELETE
        // scope compiled for the environment read/write above.
        let lease_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::LEASE, actions::GET, Some(id))
            .await?;
        let lease = self.leases_repo.get(&conn, &lease_scope, id).await?;
        if !matches!(lease.state, LeaseState::Free) {
            return Err(DomainError::EnvironmentLeased { id });
        }

        // Resolve the row for its references before deleting it. Every
        // gear-owned secret has to go too, or deleting the environment leaves
        // secrets in credstore that nothing can ever name again — the orphan
        // `delete_ssh_key` exists to avoid. Reading first also means a missing
        // environment costs no credstore call.
        //
        // **All of them, not just the legacy column's** (review finding I-4).
        // Ownership and cleanup became n-ary on create and on update at Task
        // 18b and this method was left 1-ary, so the first plugin declaring
        // two secret fields orphaned one on every delete. It has no live
        // effect today — every plugin in this tree declares one — but the code
        // path is generic, and after Task 19 drops the legacy column a 1-ary
        // delete would clean up **nothing at all**.
        //
        // The same preference the two readers apply: the plugin-shaped column
        // when it is populated, the pre-plugin one as the fallback. No plugin
        // **The legacy fallback went with the column** (ruling F-2, Task 19):
        // `credentials` is the only source now, and `m20260903_000012` gave
        // every row that had a legacy reference an entry in it. A row with an
        // empty `credentials` owns nothing this gear minted, so there is
        // nothing to forget.
        let existing_refs = match self.repo.get(&conn, &scope, id).await? {
            Some(environment) => environment
                .credentials
                .into_iter()
                .map(|credential| credential.credstore_ref)
                .collect(),
            None => Vec::new(),
        };

        let deleted = self.repo.delete(&conn, &scope, id).await?;

        if !deleted {
            return Err(DomainError::EnvironmentNotFound { id });
        }

        // Row first, then the secret — the opposite of `delete_ssh_key`, and
        // deliberately so. There, the reference is the row's whole payload and
        // a dangling one is worse than a briefly orphaned row. Here the
        // reference may be **caller-supplied and shared**, so deleting it
        // before the row is committed could destroy a secret another environment
        // (or another system) still uses while this delete goes on to fail its
        // precondition. `forget_owned_secret` deletes nothing this gear did
        // not mint.
        for previous in &existing_refs {
            let generated = credentials::is_generated_ref(previous);
            self.forget_owned_secret(ctx, previous, generated).await;
        }

        info!("Successfully deleted environment");
        Ok(())
    }

    /// Run one on-demand detection cycle against the environment's own cluster
    /// and persist the outcome, returning the refreshed row. Backs
    /// `POST /qa/v1/environments/{id}/refresh`.
    ///
    /// # A detection failure is not an `Err`
    ///
    /// The environment's cluster being unreachable, or its vpadm install
    /// metadata being missing or malformed, is a **fact about the environment**,
    /// not a fault of this service. Such a failure is persisted into
    /// `version_detect_error` via
    /// [`EnvironmentsRepository::record_observation`] and returned inside the
    /// `Ok` row exactly like a successful detection — matching legacy's
    /// `api_refresh_version` (`manager/src/routes/platforms.rs:363`), which
    /// reserves a non-2xx response for a genuine server-side fault and
    /// answers every detection attempt, successful or not, with 200. The REST
    /// handler (`api::rest::handlers::environments::refresh_environment`) relies on
    /// that: it maps `Ok` straight to a 200 body.
    ///
    /// # What *is* an `Err`
    ///
    /// The environment not existing (`DomainError::EnvironmentNotFound`, → 404),
    /// the caller lacking access, or — the case new to this method — the
    /// stored `kubeconfig_credstore_ref` failing to resolve back to material
    /// through credstore. That last one is this gear's own bookkeeping
    /// failing (a dangling or unreadable reference), not a fact about the
    /// environment's cluster, so it propagates as a genuine `DomainError` rather
    /// than being folded into the `ObservationOutcome` an operator reads.
    #[instrument(skip(self, ctx), fields(environment_id = %id))]
    pub async fn observe_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Environment, DomainError> {
        info!("Observing environment");

        // UPDATE, not GET: this method persists (`record_observation`) and
        // makes an outbound call to the environment's own cluster, exactly the
        // shape `update_environment`/`delete_environment` above gate on their own
        // mutating actions. A principal granted only GET must not be able to
        // drive a live-cluster round-trip and a database write through this
        // endpoint.
        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::UPDATE, Some(id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let environment = self
            .repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::EnvironmentNotFound { id })?;

        let observation = self.observe_through_plugin(ctx, &environment).await;

        self.repo
            .record_observation(&conn, &scope, id, &observation)
            .await?;

        info!("Successfully observed environment");

        self.repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::EnvironmentNotFound { id })
    }

    /// Observe this environment through its product's plugin.
    ///
    /// The chain: resolve the plugin, resolve its credential slots out of
    /// credstore, hand it an `EnvironmentHandle`, and pass what it returns
    /// through [`ObservationWrite::new`] — which applies `retain_declared`
    /// and `project_roles`, so nothing undeclared can reach a column.
    ///
    /// # Every upstream failure is a recorded observation, not an error
    ///
    /// Four things can go wrong before the plugin is ever called: the
    /// environment names no product, the product names no resolvable plugin,
    /// no resolver is registered at all, or a credential cannot be read back
    /// out of credstore. **All four return `Ok`**, with a `Failed`
    /// environment half carrying fixed text and `NotAttempted` health.
    ///
    /// That is not leniency, it is a measured lesson. Before the kubeconfig
    /// case was handled this way, an environment whose kubeconfig could not be
    /// resolved failed *upstream* of the observer, so `record_observation` was
    /// never reached and NOTHING was written: the UI read "not yet observed"
    /// forever while the real reason repeated in the log every ticker cycle.
    /// Seven of eight environments sat in exactly that state on the remote on
    /// 2026-08-29 (cluster-health spec section 8). A row that says why it is
    /// blank is the whole point of persisting a failure as a *value*.
    ///
    /// `NotAttempted` rather than a failed health read, in all four cases, for
    /// the reason `HealthOutcome::NotAttempted`'s own doc gives: a failed
    /// health read persists `cluster_status = "Unreachable"` — a claim that
    /// somebody's target could not be reached — and nothing here contacted
    /// anything to be entitled to it.
    ///
    /// `#[instrument]` skips nothing on purpose — there is nothing to skip:
    /// neither the credstore response nor the plugin's return value is ever
    /// passed to a tracing macro, so no field carries credential material for
    /// a skip to guard.
    async fn observe_through_plugin(
        &self,
        ctx: &SecurityContext,
        environment: &Environment,
    ) -> ObservationWrite {
        // `NoProduct` is no longer reachable from a row: Task 20b made the
        // column `NOT NULL`. The variant stays on `PluginUnavailable` because
        // the port's own contract still declares it, and qa-catalog's resolver
        // can still answer it.
        let product_id = environment.product_id;

        let plugin = match self.product_plugins.plugin_for(ctx, product_id).await {
            Ok(plugin) => plugin,
            // The port has already logged which of its three causes this was;
            // this gear persists the fixed text.
            Err(unavailable) => return Self::not_observed(unavailable.detail()),
        };

        let slots = match self
            .resolve_credential_slots(ctx, environment, &plugin)
            .await
        {
            Ok(slots) => slots,
            Err(detail) => return Self::not_observed(detail),
        };

        // `None`, not an empty map: the plugin contract distinguishes "never
        // observed" from "observed and found nothing", and
        // `EnvironmentHandle::observed`'s own doc says both observation and
        // dispatch legitimately reach environments in the first state.
        let observed =
            (!environment.observed_attrs.is_empty()).then_some(&environment.observed_attrs);
        let handle = EnvironmentHandle {
            slots: &slots,
            // Verbatim, per ruling D-9. Synthesising this from the variables
            // table would put one product's variable name in the gear whose
            // whole purpose is to stop naming that product; the one-time
            // backfill in `m20260903_000011_environment_plugin_columns` is
            // where that mapping lives instead.
            config: &environment.config,
            observed,
        };

        let schema = plugin.observed_schema();
        ObservationWrite::new(&schema, plugin.observe(&handle).await)
    }

    /// The observation an environment gets when nothing was able to look at
    /// it: a classified environment-half failure, and no health reading at
    /// all.
    fn not_observed(detail: &'static str) -> ObservationWrite {
        ObservationWrite::new(
            // No schema, because no plugin produced attributes — and a
            // `Failed` half carries none either way.
            &[],
            PluginObservation {
                environment: PluginObservationOutcome::Failed(PluginFailure::classified(
                    FailureClass::Internal,
                    detail,
                )),
                health: PluginHealthOutcome::NotAttempted,
            },
        )
    }

    /// Resolve this environment's credential into a slot the plugin can read.
    ///
    /// [`CredentialSlot::resolved`], not `reference_only`: `observe` is the
    /// one method the contract expects to be handed plaintext, because
    /// reaching a target means authenticating to it. Dispatch is the opposite
    /// case and uses `reference_only` (Task 18).
    ///
    /// # Two sources, and the plugin-shaped one wins (ruling F-2)
    ///
    /// `Environment::credentials` — the plugin-shaped column
    /// `m20260903_000011_environment_plugin_columns` added — is **preferred
    /// when non-empty**, and the pre-plugin `kubeconfig_credstore_ref` is the
    /// fallback when it is empty. Which is which changed at Task 18b, and the
    /// reason it was the other way round until then is worth keeping, because
    /// it is what the fallback is still for.
    ///
    /// Before Task 18b that column was a one-time snapshot with **no writer**:
    /// `m20260903_000011` filled it from `kubeconfig_credstore_ref` for every
    /// row that existed, `create` left it empty, and `update` left it alone.
    /// So for an environment whose kubeconfig had been replaced since the
    /// migration ran it named a **superseded** reference — one this gear may
    /// have deleted from credstore outright, if it minted it
    /// (`forget_owned_secret`). Reading it would have given a plugin either a
    /// hard failure that never self-heals or, worse, *stale material*: the
    /// exact failure decision D4 exists to prevent.
    ///
    /// Task 18b gave it a writer. Every create and every credential-bearing
    /// update now maintains it through the product's own plugin
    /// (`credentials::store_submitted_credentials`), so for any row written
    /// since, it is current by construction — and it is the only one of the
    /// two that can hold more than one credential.
    ///
    /// **The fallback still matters, and only for rows written before Task
    /// 18b**: those still carry `credentials = []` and a live legacy
    /// reference. Task 19 re-derives the column from the legacy one — as an
    /// unconditional overwrite, which is what repairs a *stale* snapshot as
    /// well as an empty one — and then drops both the column and this
    /// fallback.
    ///
    /// # Where the key comes from, on the fallback path
    ///
    /// The legacy column holds a reference and no key, and a plugin looks its
    /// credentials up *by* key. The key is therefore taken from the plugin
    /// itself: the sole field it declares both required and secret. For a
    /// Kubernetes-shaped product that resolves to `kubeconfig`; for the first
    /// product with two required secrets it resolves to nothing, and the
    /// environment records that it cannot be observed rather than guessing
    /// which of the two the single legacy column meant. Deriving it beats a
    /// literal for a second reason as well: a plugin that renames its
    /// credential field renames the slot in the same release, with nothing in
    /// this gear to update.
    ///
    /// On the preferred path the key is stored beside the reference, so no
    /// derivation is needed and a plugin with *two* required secrets is
    /// resolvable — which is why [`LEGACY_CREDENTIAL_UNBINDABLE`] is now
    /// unreachable for a row with a populated `credentials`. That sentence was
    /// in this constant's doc before Task 18b and was false when written,
    /// because nothing read the column; Task 18b made it true rather than
    /// deleting it.
    ///
    /// # Errors
    ///
    /// Fixed text, never a formatted error: a credstore failure's own message
    /// can name the reference, and the reference is banned from
    /// `EnvironmentDto` because under `SharingMode::Tenant` it *is* a read
    /// path to the material.
    pub(super) async fn resolve_credential_slots(
        &self,
        ctx: &SecurityContext,
        environment: &Environment,
        plugin: &Arc<dyn QaProductPluginV1>,
    ) -> Result<Vec<CredentialSlot>, &'static str> {
        // The preferred source carries its own keys and may carry several
        // credentials; the fallback carries one reference and no key, so the
        // plugin has to supply that. See the doc above for why the preference
        // is this way round since Task 18b.
        // **One source since Task 19.** The legacy column and this method's
        // fallback to it were dropped together (ruling F-2); `credentials` is
        // where an environment's credential references live, and
        // `m20260903_000012` populated it for every row that had one.
        let _ = plugin;
        let stored: Vec<(String, String)> = environment
            .credentials
            .iter()
            .map(|credential| (credential.key.clone(), credential.credstore_ref.clone()))
            .collect();

        let mut slots = Vec::with_capacity(stored.len());
        for (key, credstore_ref) in stored {
            // Every slot is resolved, and one that cannot be resolved fails
            // the whole observation rather than being dropped: `observe` is
            // documented as the one method whose caller has resolved the slots
            // it needs, and handing a plugin a partial credential set means it
            // reports a product-level failure for what is really a credstore
            // problem an operator can fix.
            let material = self
                .fetch_credential_material(ctx, Some(environment.id), &credstore_ref)
                .await
                .map_err(|error| {
                    // `debug!`, not `warn!`: the ticker's own loop already logs
                    // one warning per failing environment per cycle, and this
                    // is the same fact one layer down.
                    tracing::debug!(
                        environment_id = %environment.id,
                        credential_key = %key,
                        %error,
                        "a credential could not be resolved; recording it as a failed observation"
                    );
                    KUBECONFIG_UNRESOLVED
                })?;
            slots.push(CredentialSlot::resolved(key, credstore_ref, material));
        }

        Ok(slots)
    }

    /// Resolve `credstore_ref` back into the bytes it names.
    ///
    /// Shared by [`Self::resolve_credential_slots`] and D4's `Secret` write
    /// ([`Self::materialise_runner_secret`], reached from create, from update
    /// and from the ticker's self-heal), which needs the same material for
    /// [`RunnerSecretWriter::ensure_kubeconfig_secret`]. It is a second
    /// credstore round trip rather than a shared one, which is an acceptable
    /// cost on a create/update request and on a step that otherwise runs on a
    /// multi-minute cadence.
    /// `environment_id` appears only in these two `Internal` messages, so a
    /// `None` costs the operator the row id and nothing else. **Both call
    /// sites pass `Some`**: Task 18b briefly had a third that read a submitted
    /// reference on a create, and ruling F-4's reversal deleted it — the write
    /// path resolves no credential material at all.
    ///
    /// # Both messages name the reference, and every caller must know it
    ///
    /// A reference is a read path to the material under `SharingMode::Tenant`,
    /// which is why `EnvironmentDto` withholds it and why
    /// [`KUBECONFIG_UNRESOLVED`] exists. **No caller may propagate these
    /// errors verbatim to a response**: the observation path maps them to
    /// `KUBECONFIG_UNRESOLVED`, and Task 18b's write path maps them to fixed
    /// validation text.
    pub(super) async fn fetch_credential_material(
        &self,
        ctx: &SecurityContext,
        environment_id: Option<Uuid>,
        credstore_ref: &str,
    ) -> Result<SecretValue, DomainError> {
        let subject = environment_id.map_or_else(
            || "a submitted credential reference".to_owned(),
            |id| format!("environment {id}'s stored credential reference"),
        );
        let secret_ref = SecretRef::new(credstore_ref.to_owned()).map_err(|e| {
            DomainError::Internal(format!("{subject} is not a valid SecretRef: {e}"))
        })?;

        let response = self
            .credstore
            .get(ctx, &secret_ref)
            .await
            .map_err(map_credstore_error)?
            .ok_or_else(|| {
                DomainError::Internal(format!(
                    "{subject} ({credstore_ref}) was not found in credstore"
                ))
            })?;

        Ok(response.value)
    }

    /// One self-heal pass over **every** environment this gear knows about,
    /// across every tenant: observe each against its own cluster, persist the
    /// outcome, and re-apply its kubeconfig `Secret` in the Argo cluster. This
    /// is the ticker's per-cycle body — see `crate::gear`'s `observation_ticker`
    /// for the supervised loop that calls it, and that module's `Tickers` doc
    /// for why the ticker's *role* is recorded at spawn time rather than read
    /// back from this method: the same reasoning applies here as there, one
    /// level up.
    ///
    /// # One environment's failure never aborts the cycle for the others
    ///
    /// Every environment gets its own `Result`, matched individually and logged
    /// on failure — a credstore miss, a PEP denial, an unreachable cluster, a
    /// `409` from the self-heal are all just one more warning line, never a
    /// `return` that would strand every environment after the one that failed.
    /// A panic partway through (which [`crate::gear`]'s supervisor reports,
    /// exactly as it would for any other ticker) is one way this stops short of
    /// every environment it started with; `cancel` firing mid-pass is the
    /// other, and it is not a failure mode — see the next section.
    ///
    /// # A cancelled cycle returns its partial report, not an error
    ///
    /// `cancel` is checked at the *top* of the per-environment loop, before
    /// that iteration's round trip to its cluster — checking after would have
    /// already spent the cost a shutdown exists to cut off, which at
    /// `cpt-cf-qa-nfr-scale`'s 100 environments is the entire shutdown budget.
    /// Review finding #32.
    ///
    /// Stopping early returns [`ObservationCycleReport`] exactly as it stands,
    /// counting only what this call actually attempted — not an error, because
    /// a cancelled cycle did the work it did, and the ticker's next tick (or
    /// the next process start, if the whole gear is shutting down) re-sweeps
    /// every environment from scratch. A short `attempted` is therefore
    /// ambiguous by design between "the gear has this many environments" and
    /// "this call was cut short" — the caller does not need to tell them
    /// apart, since both are handled by the same next pass, but a reader
    /// inferring the environment population from one report's `attempted`
    /// would be wrong to.
    ///
    /// # No `SecurityContext` in, one out per environment
    ///
    /// Listing every environment across every tenant is this gear's own
    /// maintenance duty, not a caller's request — see
    /// [`crate::domain::repos::EnvironmentsRepository::list_all_with_tenant`]'s
    /// doc for why it is read under `AccessScope::allow_all()` rather than a
    /// scope resolved from any `SecurityContext`. Everything this method
    /// does with a given environment *afterwards* — resolving its product's
    /// plugin, resolving its credentials, persisting what was observed — goes
    /// through the ordinary PEP-scoped path
    /// ([`Self::observe_environment`]/[`Self::observe_through_plugin`]), under
    /// a context bound to that environment's own tenant
    /// ([`system_actor::for_observation`]). That now includes a read in
    /// *another* gear: `qa-catalog` resolves the product's plugin under this
    /// same context, so a deployment whose `AuthZ` policy is authored per
    /// resource type must grant this gear's system actor `qa.product` `get`
    /// as well as its own `qa.platform` `update`. Under `static-authz-plugin`
    /// no policy work is needed — a tenant-bound context is granted by tenant,
    /// whatever the resource type.
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the per-environment branches (nil tenant, observe Err, self-heal \
                  Err), each of which owes an operator a distinct log line - same diagnosis as \
                  qa-runs' `service::dispatch::run_tick`/`reconcile_claims`"
    )]
    pub async fn run_observation_cycle(
        &self,
        cancel: &CancellationToken,
    ) -> ObservationCycleReport {
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
        //   ticker's own enumeration step - "which environments exist across every
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
        //   resolving an environment's product plugin, resolving its
        //   credentials, persisting its outcome - runs under a real, tenant-bound
        //   `SecurityContext` through the ordinary PEP-scoped path
        //   (`Self::observe_environment`), exactly as `list_all_with_tenant`'s own
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
        // `EnvironmentsRepository` uses - `allow_all()` is the one `AccessScope`
        // value that applies no row-level filter, not a different code path.
        let scope = toolkit_security::AccessScope::allow_all();

        let environments = match self.repo.list_all_with_tenant(&conn, &scope).await {
            Ok(environments) => environments,
            Err(error) => {
                warn!(
                    %error,
                    "qa-environments observation cycle: failed to list environments; skipping \
                     this cycle"
                );
                return report;
            }
        };

        for (environment, tenant_id) in environments {
            // Checked before the round trip, not after: each iteration below
            // contacts that environment's own cluster, so a check placed
            // after the work would have already spent the very cost a
            // shutdown is trying to cut off. Review finding #32.
            if cancel.is_cancelled() {
                info!(
                    attempted = report.attempted,
                    "qa-environments observation cycle stopping early (shutdown)"
                );
                return report;
            }

            report.attempted += 1;

            if tenant_id.is_nil() {
                warn!(
                    environment_id = %environment.id,
                    environment_name = %environment.name,
                    "qa-environments observation cycle: this environment carries a nil tenant id; \
                     skipping it rather than observing it under an unscoped identity"
                );
                report.failed += 1;
                continue;
            }

            let ctx = system_actor::for_observation(tenant_id);

            match self.observe_environment(&ctx, environment.id).await {
                Ok(_) => report.observed += 1,
                Err(error) => {
                    warn!(
                        environment_id = %environment.id,
                        environment_name = %environment.name,
                        %error,
                        "qa-environments observation cycle: observing this environment failed; \
                         continuing with the rest"
                    );
                    report.failed += 1;
                }
            }

            self.self_heal_kubeconfig_secret(&ctx, &environment).await;
        }

        report
    }

    /// The ticker's per-cycle call into [`Self::materialise_runner_secret`],
    /// made **independently of whether this cycle's detection succeeded** —
    /// the self-heal decision D4 exists for
    /// (`crate::infra::runner_secret_writer`'s module doc): a pod stuck on
    /// `FailedMount` needs the `Secret` fixed regardless of what the
    /// environment's own cluster answered this cycle.
    ///
    /// It is a named wrapper rather than a direct call so the ticker's own
    /// body reads as what it is (`observe`, then `self-heal`), and so the
    /// `origin` tag every log line carries is written once here rather than
    /// spelled at the call site.
    async fn self_heal_kubeconfig_secret(&self, ctx: &SecurityContext, environment: &Environment) {
        self.materialise_runner_secret(ctx, environment, "observation cycle self-heal")
            .await;
    }

    /// Decision D4's single write path: resolve `environment`'s kubeconfig out of
    /// credstore and ensure the `Secret` a runner pod mounts exists in the
    /// Argo cluster and carries that material.
    ///
    /// `origin` names which of the three callers asked — `create`, `update`,
    /// or `observation cycle self-heal` — and is attached to every log line,
    /// because "this environment's Secret could not be written" reads very
    /// differently depending on whether it happened once at create time or is
    /// repeating every cycle.
    ///
    /// # Called from all three places, deliberately
    ///
    /// The design (§4.6) says "on create, on update, and as a self-heal", and
    /// until 2026-08-28 only the self-heal existed: the final whole-branch
    /// review found the create/update half had been described in the plan
    /// (Task 6, Step 3) but never assigned to a task. The consequence was
    /// precisely the symptom D4 exists to remove — an environment created in the
    /// UI sitting `Pending` for up to `poll_interval_seconds` and then failing
    /// on `FailedMount` with no explanation.
    ///
    /// # A failure here never rolls anything back
    ///
    /// Logged and swallowed, exactly as [`Self::forget_owned_secret`] treats
    /// its own partial failure. An environment whose row exists and whose `Secret`
    /// is missing is *recoverable* — the ticker re-applies it on the next
    /// cycle, which is what the self-heal is for — while an environment that
    /// silently vanished because a write into a *different* cluster failed is
    /// not. That asymmetry is the whole reason this returns `()`: there is no
    /// `Err` for a caller to be tempted to unwind on.
    ///
    /// # The known `409` interaction
    ///
    /// `crate::infra::runner_secret_writer` applies without `force`, on
    /// purpose: forcing would let this writer stomp a `Secret` an operator
    /// deliberately created by hand (`kubectl create`, or the provisioning
    /// shell script) rather than through server-side apply. When that
    /// happens, the writer's own error text already names the `Secret`, the
    /// namespace, and what an operator should do about it (delete the
    /// hand-made `Secret` so this writer can own it) — see
    /// `runner_secret_writer::describe_apply_failure`. This method's job is only to
    /// make sure that text reaches the log **with the environment attached**,
    /// so a self-heal that repeats every cycle reads as "environment X's Secret
    /// needs an operator" rather than an anonymous, repeating error.
    /// The credstore reference a runner `Secret` is written from, or `None`
    /// when this environment stores no credential at all.
    ///
    /// **From `credentials`** — Task 19 dropped the legacy column this used to
    /// read. There is exactly one credential a runner `Secret` can be made
    /// from, and it is the same one both credential readers key.
    fn runner_secret_reference(environment: &Environment) -> Option<String> {
        environment
            .credentials
            .first()
            .map(|credential| credential.credstore_ref.clone())
    }

    /// Resolve the material for a runner `Secret`, logging and returning
    /// `None` when it cannot be read.
    ///
    /// Split out of [`Self::materialise_runner_secret`] purely to keep that
    /// function under clippy's cognitive-complexity gate: Task 19 added a third
    /// early exit there (an environment that stores no credential at all), and
    /// three log-and-return arms in one function is over the line.
    async fn runner_secret_material(
        &self,
        ctx: &SecurityContext,
        environment: &Environment,
        credstore_ref: &str,
        origin: &'static str,
    ) -> Option<credstore_sdk::SecretValue> {
        match self
            .fetch_credential_material(ctx, Some(environment.id), credstore_ref)
            .await
        {
            Ok(material) => Some(material),
            Err(error) => {
                warn!(
                    environment_id = %environment.id,
                    environment_name = %environment.name,
                    origin,
                    %error,
                    "qa-environments: could not resolve this environment's credential, so its \
                     runner Secret (D4) was left untouched; the observation ticker will try \
                     again on its next cycle"
                );
                None
            }
        }
    }

    async fn materialise_runner_secret(
        &self,
        ctx: &SecurityContext,
        environment: &Environment,
        origin: &'static str,
    ) {
        // **The reference now comes from `credentials`** — Task 19 dropped the
        // legacy column this used to read. There is exactly one credential a
        // runner `Secret` can be made from, and it is the same one both
        // credential readers key: the plugin's sole required secret. An
        // environment whose `credentials` is empty has nothing to materialise,
        // which is the honest answer rather than an error, for this method's
        // documented reason: a runner `Secret` that cannot be written must not
        // fail the operation that triggered it.
        let Some(credstore_ref) = Self::runner_secret_reference(environment) else {
            debug!(
                environment_id = %environment.id,
                origin,
                "qa-environments: this environment stores no credential, so there is no \
                 runner Secret (D4) to write"
            );
            return;
        };

        let Some(material) = self
            .runner_secret_material(ctx, environment, &credstore_ref, origin)
            .await
        else {
            return;
        };

        if let Err(message) = self
            .observer
            .ensure_kubeconfig_secret(&credstore_ref, &material)
            .await
        {
            warn!(
                environment_id = %environment.id,
                environment_name = %environment.name,
                origin,
                error = %message,
                "qa-environments: failed to apply this environment's runner kubeconfig Secret \
                 (D4); a run dispatched before the observation ticker's next cycle repairs \
                 it will hang on FailedMount"
            );
        }
    }

    // `resolve_kubeconfig_source` and `store_kubeconfig_source` were removed
    // by Task 18b. `credentials::store_legacy_pair` is what folds the
    // pre-plugin pair into one reference now, and it is reached only on the
    // productless branch; every other path goes through the plugin. See the
    // note where `KubeconfigSource` used to be.

    /// The "both supplied" rejection. Names **both** fields, because the
    /// caller has to know which one to drop and `DomainError::Validation`
    /// carries only a single `field`.
    ///
    /// Still reachable after Task 18b, from
    /// `credentials::desugar_legacy_credential_pair` and from
    /// `credentials::store_legacy_pair`: the pre-plugin pair is still accepted
    /// on the wire, so its own rules are still enforced, verbatim.
    pub(super) fn both_kubeconfig_fields_error() -> DomainError {
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
    ///
    /// This is the platform's floor, not the plugin's rule: a plugin decides
    /// whether a field is *required* and whether its content is well-formed
    /// (`validate_credentials`), and whitespace is refused before either
    /// question is asked, because storing it means minting a secret whose
    /// value is nothing.
    pub(super) fn validate_material(
        material: CredentialMaterial,
    ) -> Result<CredentialMaterial, DomainError> {
        if material.expose().trim().is_empty() {
            return Err(DomainError::Validation {
                field: "kubeconfig".to_owned(),
                message: "must not be empty".to_owned(),
            });
        }
        Ok(material)
    }

    /// Write a pasted document to credstore under a freshly generated
    /// reference and return that reference — the only value that goes on to
    /// the database.
    ///
    /// `SharingMode::Tenant` matches `create_ssh_key`; the caveats that mode
    /// carries are documented at length in `ssh_keys.rs`'s module header and
    /// apply here identically.
    pub(super) async fn write_generated_secret(
        &self,
        ctx: &SecurityContext,
        material: CredentialMaterial,
    ) -> Result<String, DomainError> {
        let raw_ref = format!(
            "{GENERATED_CREDENTIAL_REF_PREFIX}{}",
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
    /// reference this gear minted (either prefix — see
    /// [`credentials::is_generated_ref`]) is ours to remove. A
    /// caller-supplied reference such as `credstore://team-a/prod-cluster` may
    /// name a secret shared with other environments or other systems entirely, so
    /// deleting it on our own initiative would destroy someone else's
    /// credential. `create_ssh_key` needs no such test because its references
    /// are *always* generated — there is no way to hand it one.
    ///
    /// Failures are logged, never propagated: every call site has already
    /// committed the outcome the caller asked for, and turning a cleanup
    /// failure into an error would report a success as a failure.
    pub(super) async fn forget_owned_secret(
        &self,
        ctx: &SecurityContext,
        raw_ref: &str,
        generated: bool,
    ) {
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
                warn!(error = %e, "Failed to remove the environment's kubeconfig secret from credstore");
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
    /// justified by a claimed `qa_environments` "convention" that does not exist:
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
    /// the column — legacy's `create_platform` at
    /// `manager/src/services/platforms.rs:385-392` and its `update_platform` via the
    /// `"__NULL__"` sentinel at `:447-454` — so the stored value is always
    /// either `NULL` or a trimmed, non-empty string. Normalising here, in the
    /// service, rather than at the REST boundary means SDK and local-client
    /// callers get the same guarantee as HTTP ones; the repository layer is
    /// then free to write whatever it is handed.
    ///
    /// A consequence worth stating because it looks like a contradiction and is
    /// not: qa-runs' `resolve_branch` **also** trims and filters this value
    /// (`qa-runs/src/domain/service/launch.rs`, rule 1's middle tier). Given
    /// this normaliser that filter can never fire on a stored environment, so it is
    /// **defensively redundant, not disagreeing** — the source system carries the
    /// same redundancy for the same reason (`exclusivity.rs:310-311` filters a
    /// value `environments.rs` already normalised). Neither side should be
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

/// Outcome counters for one [`EnvironmentsService::run_observation_cycle`] pass —
/// how many environments the ticker's `debug!` cycle-summary line, and this
/// module's own tests, can report on without re-deriving them from logs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObservationCycleReport {
    /// Every environment the cycle attempted, whether or not it succeeded.
    ///
    /// A cancelled cycle (`run_observation_cycle`'s doc) returns before
    /// visiting the rest, so this can be smaller than the gear's true
    /// environment count — it is not a census, only a count of what this one
    /// call got to.
    pub attempted: u32,
    /// Environments for which [`EnvironmentsService::observe_environment`] returned
    /// `Ok` — a detection *outcome* was persisted, successful or not (see
    /// that method's own doc on why a detection failure is still `Ok`).
    pub observed: u32,
    /// Environments for which [`EnvironmentsService::observe_environment`] itself
    /// returned `Err`, or which were skipped outright (a nil tenant id).
    pub failed: u32,
}
