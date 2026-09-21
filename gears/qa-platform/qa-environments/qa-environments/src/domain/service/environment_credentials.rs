//! The plugin-driven credential write path (Task 18b).
//!
//! # What this module is for
//!
//! Until Task 18b this gear could store exactly one credential per
//! environment, in one column, under a name it knew: `kubeconfig`. The plan's
//! whole purpose is to stop naming one product's credential, and
//! `qa_product_sdk` already had the interface for it —
//! [`QaProductPluginV1::validate_credentials`] returning
//! [`CredentialClassification`] — with **no production caller**. Only the leak
//! harness called it. This module is that caller.
//!
//! The shape is: the product's own plugin says which submitted fields are
//! credential material, secrets go to credstore and the `credentials` column,
//! everything else goes to the `config` column, and nothing in this gear names
//! a credential key.
//!
//! # One mechanism, not two (ruling F-8)
//!
//! The obvious way to add this was to keep the audited single-kubeconfig path
//! for the credential that has a column and add a second mechanism for the
//! rest. That splits secret **ownership and cleanup** across two mechanisms,
//! in the code where this branch already has both a measured leak
//! (2026-08-28) and a measured orphan (the `previous_ref` comment in
//! `update_environment`). So the legacy pair is desugared into the credentials
//! map *first*, and from there one path handles *n* ≥ 1 credentials. The
//! single-kubeconfig case is n=1.
//!
//! # The dual-write, and why it is not redundant (ruling F-1)
//!
//! Every write here also maintains `kubeconfig_credstore_ref`. That column has
//! two production readers — this gear's own
//! [`resolve_credential_slots`](super::EnvironmentsService::resolve_credential_slots)
//! and `qa-runs`' `DispatchService::plugin_dispatch` (ruling E-17) — and both
//! read it because when they were written *nothing wrote* `credentials`. Task
//! 19 drops the column; until it does, dropping the dual-write would break
//! every run against every environment.

use std::collections::BTreeMap;

use credstore_sdk::SecretValue;
use qa_environments_sdk::{
    CredentialMaterial, CredentialSubmission, Environment, EnvironmentCredential,
};
use qa_product_sdk::QaProductPluginV1;
use qa_product_sdk::descriptor::sole_required_secret_key;
use qa_product_sdk::plugin::{CredentialClassification, CredentialInput};
use std::sync::Arc;
use toolkit_security::SecurityContext;
use tracing::warn;

use super::{EnvironmentsService, GENERATED_CREDENTIAL_REF_PREFIX, LEGACY_CREDENTIAL_UNBINDABLE};
use crate::domain::error::DomainError;
use crate::domain::ports::PluginUnavailable;
use crate::domain::repos::{EnvironmentsRepository, LeasesRepository, PersistedCredentials};

/// The outcome of resolving one submitted credential form: what to persist,
/// and what to clean up on either side of the row write.
pub(super) struct StoredCredentials {
    /// What the repository writes. Carries no plaintext, by construction.
    pub(super) persisted: PersistedCredentials,
    /// References this call minted, in submission order.
    ///
    /// These are the compensating delete if the **row write fails** — the
    /// shape `qa-catalog`'s `create_ssh_key` uses and comments on. A
    /// caller-supplied reference is never in here: this gear did not mint it,
    /// so it is not this gear's to destroy.
    pub(super) minted: Vec<String>,
    /// References this write displaced, for deletion **after** the row write
    /// succeeds.
    ///
    /// Empty on create. On update it holds the previous reference of each key
    /// the patch actually replaced, and only where the new reference differs —
    /// a patch may name the reference already stored (a no-op rotation, or a
    /// retry), and deleting that would destroy the secret the row still points
    /// at. Ownership is tested at the delete, not here.
    pub(super) superseded: Vec<String>,
}

impl<P: EnvironmentsRepository, L: LeasesRepository> EnvironmentsService<P, L> {
    /// Resolve, validate, classify and store one submitted credential form.
    ///
    /// `existing` is `None` on create and the current row on update; it is
    /// what makes the update path a merge rather than a replacement, because a
    /// `credentials` JSON array cannot be partially assigned.
    ///
    /// # The order is the guarantee
    ///
    /// Everything that can reject the request runs before anything reaches
    /// credstore, which is the property `create_environment` already states
    /// and this method has to preserve: *nothing rejected ever reaches
    /// credstore*. So the plugin's verdict is taken first and the writes
    /// happen last.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] for a form the plugin rejected, for a
    /// submitted key spelled two ways, and for a reference offered for a
    /// non-secret field. [`DomainError::Internal`] for a credstore failure —
    /// never with the credstore error's own `Display`, which is not a bounded
    /// string this crate controls.
    pub(super) async fn store_submitted_credentials(
        &self,
        ctx: &SecurityContext,
        plugin: &Arc<dyn QaProductPluginV1>,
        submitted: BTreeMap<String, CredentialSubmission>,
        existing: Option<&Environment>,
    ) -> Result<StoredCredentials, DomainError> {
        let schema = plugin.credential_schema();

        // 1. The plugin's verdict on the pasted half. **This path resolves
        //    nothing** — see the module header and ruling F-4 (reversed): a
        //    credstore reference is an out-of-band binding the plugin has no
        //    opinion on and could not form one about without the bytes, and
        //    creating an environment that names a not-yet-provisioned
        //    reference is a designed, self-healing state
        //    (`KUBECONFIG_UNRESOLVED` says so, and says how to leave it).
        //
        //    So only material is validated, and a reference is classified from
        //    the declared schema below. The consequence worth stating plainly:
        //    **no credential plaintext enters this process from the write
        //    path at all.**
        let mut input = CredentialInput::default();
        for (key, submission) in &submitted {
            if let CredentialSubmission::Material(material) = submission {
                input
                    .fields
                    .insert(key.clone(), SecretValue::from(material.expose().to_owned()));
            }
        }

        // Not called for a form with nothing pasted in it: there is no form to
        // validate, and a plugin asked to check a required field it cannot see
        // would refuse a perfectly good reference-only submission.
        let mut classifications = if input.fields.is_empty() {
            Vec::new()
        } else {
            // `PluginFailure::detail` is `Option<&'static str>` — a
            // compile-time constant — which makes it the one
            // credential-shaped text in this file safe to surface verbatim.
            // Nothing is interpolated.
            plugin
                .validate_credentials(&input)
                .await
                .map_err(|failure| DomainError::Validation {
                    field: "credentials".to_owned(),
                    message: failure
                        .detail
                        .unwrap_or("this product's plugin rejected the submitted credentials")
                        .to_owned(),
                })?
        };

        // 2. Classify the reference submissions from the declared schema. A
        //    reference names a secret in the credential store, so it is only
        //    meaningful for a field the plugin declares secret; anything else
        //    is refused in `reconcile_classifications`.
        for (key, submission) in &submitted {
            if !matches!(submission, CredentialSubmission::Reference(_)) {
                continue;
            }
            if let Some(field) = schema.iter().find(|field| &field.key == key) {
                classifications.push(CredentialClassification {
                    key: key.clone(),
                    is_secret: field.kind.is_secret(),
                });
            }
        }

        // 3. Reconcile the answer against what was actually submitted, before
        //    writing anything.
        let classifications = Self::reconcile_classifications(&submitted, classifications)?;

        // 4-5, wrapped so that **any** failure after the first credstore write
        // forgets what it already minted (review finding IMPORTANT-1).
        //
        // Two things reach this: the C-2 floor, which by design runs on the
        // MERGED state and therefore cannot run before the writes; and a
        // credstore write that fails part way through an n-ary submission.
        // Before this, both dropped `minted` on the `?` and left secrets in
        // credstore with nothing able to name them — unreachable forever,
        // because the reference was never persisted anywhere. An ordinary bad
        // request reached it, and a client retrying one wrote an unbounded
        // number of them.
        //
        // `minted` is owned out here so the cleanup can see what was written
        // no matter where inside the helper the failure happened.
        let mut minted: Vec<String> = Vec::new();
        match self
            .write_classified_credentials(
                ctx,
                &schema,
                &submitted,
                classifications,
                existing,
                &mut minted,
            )
            .await
        {
            Ok((persisted, superseded)) => Ok(StoredCredentials {
                persisted,
                minted,
                superseded,
            }),
            Err(refusal) => {
                self.forget_minted_secrets(ctx, &minted).await;
                Err(refusal)
            }
        }
    }

    /// Steps 4 and 5 of [`Self::store_submitted_credentials`]: write the
    /// classified submissions, merge them over the row, and apply the floor.
    ///
    /// Split out purely so that every exit point is a single `?` inside one
    /// call the caller can compensate. `minted` is an out-parameter rather
    /// than a return value for the same reason: on the error path there is no
    /// return value, and the references written before the error are exactly
    /// what has to be cleaned up.
    async fn write_classified_credentials(
        &self,
        ctx: &SecurityContext,
        schema: &[qa_product_sdk::descriptor::FieldDesc],
        submitted: &BTreeMap<String, CredentialSubmission>,
        classifications: Vec<CredentialClassification>,
        existing: Option<&Environment>,
        minted: &mut Vec<String>,
    ) -> Result<(PersistedCredentials, Vec<String>), DomainError> {
        // 4. Write. Secrets to credstore, the rest to `config`.
        let mut credentials: Vec<EnvironmentCredential> = Vec::new();
        let mut config_fields: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

        for classification in classifications {
            // Present by construction: `reconcile_classifications` refuses a
            // classification whose key was not submitted.
            let Some(submission) = submitted.get(&classification.key) else {
                continue;
            };
            if classification.is_secret {
                let reference = match submission {
                    CredentialSubmission::Material(material) => {
                        let reference = self.write_generated_credential(ctx, material).await?;
                        minted.push(reference.clone());
                        reference
                    }
                    CredentialSubmission::Reference(reference) => reference.clone(),
                };
                credentials.push(EnvironmentCredential {
                    key: classification.key,
                    credstore_ref: reference,
                });
            } else {
                // A non-secret field's value belongs in `config`, in the
                // clear, because that is what `config` is: operator-set,
                // non-secret, and rendered on the environment page beside the
                // machine-observed values it is deliberately kept apart from.
                let CredentialSubmission::Material(material) = submission else {
                    // Refused in `reconcile_classifications`.
                    continue;
                };
                config_fields.insert(
                    classification.key,
                    serde_json::Value::String(material.expose().to_owned()),
                );
            }
        }

        // 5. Merge over what the row already held, and derive the dual-write.
        //    `sole_key` is what lets the merge attribute a pre-plugin row's
        //    single unkeyed reference to a key, so replacing it supersedes it
        //    instead of duplicating it.
        let sole_key = sole_required_secret_key(schema);
        let (credentials, superseded) =
            Self::merge_credentials(existing, credentials, submitted, sole_key.as_deref());
        let config = Self::merge_config(existing, config_fields);

        // The floor, on the MERGED state rather than the submitted map
        // (Critical C-2). On an update this is what lets a patch that rotates
        // one of two credentials pass, while a patch that would leave a
        // required one unstored does not.
        //
        // It cannot be hoisted above step 4's writes: it answers a question
        // about the merged result, and the merge needs the references step 4
        // produces. That is why the caller compensates instead.
        Self::require_declared_secrets(schema, &credentials)?;

        let legacy_ref = Self::legacy_reference(sole_key.as_deref(), &credentials);

        Ok((
            PersistedCredentials {
                legacy_ref,
                credentials,
                config,
            },
            superseded,
        ))
    }

    /// The credential half of a **create**: desugar, classify, store.
    ///
    /// # The productless branch, and why it does not refuse (ruling F-7)
    ///
    /// `product_id` is `Option<Uuid>` until Task 20, and a productless
    /// environment is creatable today — `observe_through_plugin` already
    /// refuses to *observe* one while `create_environment` happily makes one.
    /// Starting to refuse it here would move Task 20's behaviour change a task
    /// early and break a create the shipped UI can still make.
    ///
    /// So with no product there is no plugin, nothing can be classified, and
    /// the credential takes the pre-plugin path: the reference goes to
    /// `kubeconfig_credstore_ref`, `credentials` stays empty, and one `warn`
    /// says why. An empty `credentials` is exactly what
    /// [`EnvironmentsService::resolve_credential_slots`]' fallback exists to
    /// serve.
    ///
    /// **The PRODUCTLESS branch dies at Task 20b** (`product_id NOT NULL`).
    /// This comment used to add "which is why Task 20 runs before Task 19",
    /// true under ruling F-6 and false since F-12 split Task 20 and moved the
    /// environments half *after* the drop (re-review finding R3-4).
    ///
    /// **The plugin-unavailable branch below is not** — `product_id NOT NULL`
    /// says nothing about whether a product's plugin resolves, so it outlives
    /// Task 20b (review finding I-9 corrected this sentence, which used to
    /// cover both). It is safe for a different reason: `store_legacy_pair`
    /// populates the legacy column, and Task 19's re-derivation overwrites
    /// *from* that column, so a row written through it is repaired rather than
    /// stranded.
    ///
    /// **Task 19 removes that branch too, and for a reason `NOT NULL` has
    /// nothing to do with** (ruling F-13): the drop takes the legacy column,
    /// a credential can only be stored under a key, and the only source of a
    /// key is the plugin's own schema. So after Task 19 an unavailable plugin
    /// refuses the write — the behaviour I-9 reversed — and the cost is
    /// smaller than I-9 could measure, because C-1's guard had already made
    /// the update path a refusal for every plugin-shaped row.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::store_submitted_credentials`] rejects, plus the
    /// pre-plugin pair's own rules on the productless branch.
    pub(super) async fn resolve_new_credentials(
        &self,
        ctx: &SecurityContext,
        new: &qa_environments_sdk::NewEnvironment,
    ) -> Result<StoredCredentials, DomainError> {
        // The productless branch that stood here is gone with Task 20b: the
        // column is `NOT NULL` and `NewEnvironment::product_id` is a plain
        // `Uuid`, so the state is unrepresentable rather than refused. What
        // survives is the plugin-unavailable branch below, which `NOT NULL`
        // says nothing about (ruling F-13, and the asymmetry the Task 18b
        // re-review established).
        let product_id = new.product_id;
        let plugin = match self.product_plugins.plugin_for(ctx, product_id).await {
            Ok(plugin) => plugin,
            // The port has already logged which of its three causes this was.
            //
            // **This falls back rather than refusing, and the review argued
            // both ways** (finding I-9). Refusing was tried and reverted: this
            // port's whole design treats an unavailable plugin as a *recorded
            // fact about the environment* rather than a failure —
            // `PluginUnavailable::ResolverAbsent` exists for "qa-catalog is
            // not running beside this gear" — and refusing every
            // credential-bearing write while a sibling gear restarts
            // contradicts that directly. Six existing tests said so.
            //
            // What made the fallback unsafe was C-1, and that is fixed where
            // it belongs: `store_unclassified_patch` refuses outright when the
            // row already holds plugin-shaped credentials, so this branch can
            // never desynchronise the column both readers prefer.
            //
            // **How much the fallback actually preserves, stated exactly
            // (review finding IMPORTANT-3), because the sentence above
            // overstates it.** C-1's guard refuses every *update* to a row
            // with a populated `credentials` — which is every row written
            // since Task 18b. So what survives a qa-catalog outage is: a
            // create, and an update to a row that is still pre-18b-shaped.
            // Rotating a credential on a plugin-shaped row is refused, and
            // `keyed_row_needs_its_plugin_error` is where it says so.
            //
            // A create through here does leave `credentials = []` on a row
            // that HAS a product. That row is served by the readers' fallback,
            // and Task 19's re-derivation repairs it, because it is an
            // overwrite from the legacy column — which this branch does
            // populate. **Task 19 then removes this branch entirely**
            // (ruling F-13): with no legacy column there is no key to store a
            // credential under and nowhere to put it, so an unavailable plugin
            // refuses. See Phase F's header and Task 19's seventh warning
            // item.
            Err(unavailable) => {
                warn!(
                    detail = unavailable.detail(),
                    "qa-environments: this environment's product plugin is unavailable, so its \
                     credentials cannot be classified; storing the credential reference in the \
                     pre-plugin column only"
                );
                return Err(Self::refuse_unclassifiable(unavailable));
            }
        };

        let submitted = Self::desugar_legacy_credential_pair(
            new.kubeconfig_credstore_ref.clone(),
            new.kubeconfig.clone(),
            new.credentials.clone(),
            sole_required_secret_key(&plugin.credential_schema()).as_deref(),
        )?;
        // A create that mentions no credential at all, refused here as well as
        // by the floor purely so the message says what to do rather than
        // naming whichever field the plugin happens to declare first. **The
        // floor proper -- every declared required secret must end up STORED --
        // runs inside `store_submitted_credentials` over the merged result**,
        // because that is the only place the answer is known (Critical C-2:
        // counting submitted keys let a reference under a typo'd key through).
        if submitted.is_empty() {
            return Err(DomainError::Validation {
                field: "credentials".to_owned(),
                message: "an environment needs at least one credential: supply the fields this \
                          product's plugin declares"
                    .to_owned(),
            });
        }
        self.store_submitted_credentials(ctx, &plugin, submitted, None)
            .await
    }

    /// The credential half of an **update**, or `None` when the patch
    /// mentioned no credential at all — which is most patches, and which
    /// leaves all three credential columns `Unchanged`.
    ///
    /// The row is read before anything is written, so the displaced references
    /// are known and a missing environment costs no credstore write.
    ///
    /// # Errors
    ///
    /// [`DomainError::EnvironmentNotFound`] when the id names nothing, plus
    /// whatever [`Self::store_submitted_credentials`] rejects.
    pub(super) async fn resolve_patch_credentials<C: toolkit_db::secure::DBRunner>(
        &self,
        ctx: &SecurityContext,
        scope: &toolkit_security::AccessScope,
        conn: &C,
        id: uuid::Uuid,
        patch: &qa_environments_sdk::EnvironmentPatch,
    ) -> Result<Option<StoredCredentials>, DomainError> {
        // An empty `kubeconfig_credstore_ref` counts as **not supplied** — the
        // reading create has always given it — so a patch of
        // `{"kubeconfig_credstore_ref": "", "kubeconfig": "…"}` is a paste,
        // not a conflict, and `an_empty_reference_alongside_a_paste_is_a_paste
        // _on_update_as_on_create` is the test that says so.
        //
        // But naming the field with an empty value and supplying nothing else
        // is **not** "leave it alone": that `Some("")` used to reach the
        // repository and blank `kubeconfig_credstore_ref NOT NULL`, i.e.
        // destroy a live environment's only pointer to its credentials. It
        // stays the `must not be empty` rejection it has always been, and it
        // is checked before the row read so it costs no query.
        let mentions_no_credential = patch
            .kubeconfig_credstore_ref
            .as_deref()
            .is_none_or(str::is_empty)
            && patch.kubeconfig.is_none()
            && patch.credentials.is_empty();
        if mentions_no_credential {
            if let Some(named) = patch.kubeconfig_credstore_ref.as_deref() {
                Self::validate_credstore_ref(named)?;
            }
            return Ok(None);
        }

        let existing = self
            .repo
            .get(conn, scope, id)
            .await?
            .ok_or(DomainError::EnvironmentNotFound { id })?;

        // Unreachable since Task 20b -- see the create path above.
        let product_id = existing.product_id;
        let plugin = match self.product_plugins.plugin_for(ctx, product_id).await {
            Ok(plugin) => plugin,
            // Falls back for the reason on the create path above. Safe here
            // because `store_unclassified_patch` refuses a row that already
            // holds plugin-shaped credentials (Critical C-1).
            Err(unavailable) => {
                warn!(
                    detail = unavailable.detail(),
                    "qa-environments: this environment's product plugin is unavailable, so its \
                     credentials cannot be classified; updating the pre-plugin column only"
                );
                return Err(Self::refuse_unclassifiable(unavailable));
            }
        };

        let submitted = Self::desugar_legacy_credential_pair(
            patch.kubeconfig_credstore_ref.clone(),
            patch.kubeconfig.clone(),
            patch.credentials.clone(),
            sole_required_secret_key(&plugin.credential_schema()).as_deref(),
        )?;
        self.store_submitted_credentials(ctx, &plugin, submitted, Some(&existing))
            .await
            .map(Some)
    }

    /// Fold the pre-plugin `kubeconfig`/`kubeconfig_credstore_ref` pair into
    /// the plugin-shaped map, preserving every rule the pair had.
    ///
    /// # The key comes from the plugin, never from a literal
    ///
    /// `sole_key` is
    /// [`sole_required_secret_key`] over the product's schema — the same
    /// derivation rulings E-17/D-19 make for the same column in both gears
    /// that read it, and the reason a plugin that renames its credential field
    /// renames this in one release with nothing here to update. Writing
    /// `"kubeconfig"` would put one product's credential key in the gear whose
    /// whole purpose is to stop naming that product.
    ///
    /// `None` means the plugin declares no single required secret, so there is
    /// no key for the single legacy field to belong to and the pair cannot be
    /// honoured at all: [`LEGACY_CREDENTIAL_UNBINDABLE`] is the existing text
    /// for exactly that state.
    ///
    /// # A create that supplies no credential at all is the plugin's to refuse
    ///
    /// Not this function's. The plugin owns "which of my fields are required"
    /// and its rejection names its own field —
    /// `qa-vhp-product-plugin` answers with `FailureClass::Malformed` and
    /// `KUBECONFIG_REQUIRED`. That is a deliberate change from the pre-plugin
    /// `kubeconfig_credstore_ref: must not be empty`, which named one
    /// product's credential in a message every product would have seen.
    ///
    /// # Errors
    ///
    /// The pair's own "both supplied" rejection; a blank document; a blank
    /// reference; an unbindable legacy field; and the two spellings of one
    /// field colliding.
    fn desugar_legacy_credential_pair(
        legacy_ref: Option<String>,
        legacy_material: Option<CredentialMaterial>,
        mut submitted: BTreeMap<String, CredentialSubmission>,
        sole_key: Option<&str>,
    ) -> Result<BTreeMap<String, CredentialSubmission>, DomainError> {
        // An empty reference counts as **not supplied**, which is the reading
        // create has always given it. Without this the two paths disagreed:
        // create accepted `{"kubeconfig_credstore_ref": "", "kubeconfig": "…"}`
        // as a paste and update rejected the identical payload as "both
        // supplied".
        let legacy_ref = legacy_ref.filter(|reference| !reference.is_empty());

        let legacy = match (legacy_ref, legacy_material) {
            (Some(_), Some(_)) => return Err(Self::both_kubeconfig_fields_error()),
            (Some(reference), None) => {
                Self::validate_credstore_ref(&reference)?;
                Some(CredentialSubmission::Reference(reference))
            }
            (None, Some(material)) => Some(CredentialSubmission::Material(
                Self::validate_material(material)?,
            )),
            (None, None) => None,
        };

        if let Some(legacy) = legacy {
            let Some(key) = sole_key else {
                return Err(DomainError::Validation {
                    field: "kubeconfig".to_owned(),
                    message: LEGACY_CREDENTIAL_UNBINDABLE.to_owned(),
                });
            };
            // Two spellings of one field. Refused rather than resolved,
            // because either precedence is a silent surprise: the operator
            // sees one of the two values stored and no reason why.
            if submitted.contains_key(key) {
                return Err(DomainError::Validation {
                    field: key.to_owned(),
                    message: "this credential was supplied twice -- once under `credentials` and \
                              once as the legacy `kubeconfig`/`kubeconfig_credstore_ref` field. \
                              Supply it once"
                        .to_owned(),
                });
            }
            submitted.insert(key.to_owned(), legacy);
        }

        Ok(submitted)
    }

    /// **Ruling F-13.** With `kubeconfig_credstore_ref` dropped, a credential
    /// can only be stored *keyed*, and the only source of a key is the
    /// plugin's own `credential_schema()`. So a write that reaches no plugin
    /// has nowhere to put anything, and refuses.
    ///
    /// # This is the decision Task 18b deliberately reversed, and why it is
    /// # right now when it was wrong then
    ///
    /// Finding I-9 reverted a refusal here because `ProductPluginPort` treats
    /// an unavailable plugin as a *recorded fact about the environment* rather
    /// than a failure, and refusing every credential-bearing write while
    /// qa-catalog restarts contradicts that. That reasoning depended on the
    /// fallback column existing. Task 19 removed it; there is no third option.
    ///
    /// **The cost is far smaller than the reversal assumed**, and the thing
    /// that shrank it landed in the same fix wave: Critical C-1's guard already
    /// refused this path for any row holding plugin-shaped credentials, and
    /// `m20260903_000012` populates that column on every row that had a legacy
    /// reference. So on the update path this was *already* a refusal for the
    /// overwhelming majority of rows; on create there was never a row to fall
    /// back to. What changed is the message, not the availability.
    ///
    /// Both callers reach this: the productless branch (`product_id` is still
    /// `Option<Uuid>` until Task 20b) and the plugin-unavailable branch. They
    /// keep separate messages because they have separate remedies.
    fn refuse_unclassifiable(cause: PluginUnavailable) -> DomainError {
        Self::unclassifiable_credential_error(cause)
    }

    /// The refusal for a **plugin-shaped** submission that reached no plugin.
    ///
    /// Two routes, two remedies (review finding IMPORTANT-3). Before this they
    /// shared one message that told a `ResolverAbsent` caller to set a
    /// `product_id` they had already set — advice that is not merely useless
    /// but sends an operator to edit a field that is correct.
    ///
    /// `detail()` is `&'static str` by construction (see [`PluginUnavailable`]),
    /// so surfacing it interpolates nothing.
    fn unclassifiable_credential_error(cause: PluginUnavailable) -> DomainError {
        match cause {
            // Unchanged from before the split, field included: the shipped UI
            // still posts to this endpoint until Task 22, and re-pointing a
            // 400's `field` is a wire change no finding asked for (the
            // re-review's `n-3` records what one of those costs).
            PluginUnavailable::NoProduct => DomainError::Validation {
                field: "credentials".to_owned(),
                message: "plugin-shaped credentials need a product to resolve the plugin that \
                          classifies them; set `product_id`"
                    .to_owned(),
            },
            // **Transient.** `ResolverAbsent` is "qa-catalog is not running
            // beside this gear", which a retry clears once it is back.
            PluginUnavailable::ResolverAbsent => DomainError::Validation {
                field: "credentials".to_owned(),
                message: format!(
                    "this environment's credentials cannot be classified without its product \
                     plugin, so nothing was written; retry once it is available ({})",
                    PluginUnavailable::ResolverAbsent.detail()
                ),
            },
            // **NOT transient**, and telling an operator to retry a
            // configuration fault sends them round a loop that cannot
            // terminate: the plugin gear this product names is not in the
            // deployment, and only a deploy or a rebind fixes that.
            PluginUnavailable::Unresolvable => DomainError::Validation {
                field: "credentials".to_owned(),
                message: format!(
                    "this environment's credentials cannot be classified because its \
                     product's plugin is not available in this deployment, so nothing was \
                     written ({})",
                    PluginUnavailable::Unresolvable.detail()
                ),
            },
        }
    }

    /// Best-effort delete of every reference one write minted, for the
    /// compensating cleanup when the row write fails.
    ///
    /// `true` unconditionally, not [`is_generated_ref`]: everything in
    /// `minted` was written by the call that is now unwinding, so ownership is
    /// established by provenance rather than inferred from a prefix. A
    /// caller-supplied reference is never in the list.
    pub(super) async fn forget_minted_secrets(&self, ctx: &SecurityContext, minted: &[String]) {
        for reference in minted {
            self.forget_owned_secret(ctx, reference, true).await;
        }
    }

    /// Every credential field the plugin declares **required and secret** must
    /// end up stored.
    ///
    /// # This is the floor, and the first version of it was bypassable
    ///
    /// Task 18b originally refused only a create whose *submitted* map was
    /// empty. The review (Critical C-2) showed that counts the wrong thing: a
    /// submitted `Reference` under a key the plugin does not declare is
    /// dropped by [`Self::reconcile_classifications`] and never reaches
    /// `validate_credentials` at all (a reference is classified from the
    /// schema, ruling F-4 reversed), so one transposed character in a key name
    /// produced an accepted row with `credentials = []` **and**
    /// `kubeconfig_credstore_ref = ""` — a credential-less environment, which
    /// is precisely the state Task 19's warning item 5 exists to prevent and
    /// which its re-derivation cannot repair.
    ///
    /// Counting *stored* required secrets instead closes all three variants
    /// the review found, and it is a real requirement check rather than a
    /// proxy for one: the schema says which fields cannot be absent, so the
    /// gear can enforce that without naming any product's key.
    ///
    /// A plugin that declares **no** required secret field is unaffected — an
    /// empty `credentials` is legitimate for it, and the two readers already
    /// refuse to guess for that shape.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming the plugin's own missing field.
    fn require_declared_secrets(
        schema: &[qa_product_sdk::descriptor::FieldDesc],
        credentials: &[EnvironmentCredential],
    ) -> Result<(), DomainError> {
        for field in schema
            .iter()
            .filter(|field| field.required && field.kind.is_secret())
        {
            if !credentials
                .iter()
                .any(|credential| credential.key == field.key)
            {
                return Err(DomainError::Validation {
                    field: field.key.clone(),
                    message: "this product requires this credential, and the request stored no \
                              value for it: supply it under this exact key"
                        .to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Check the plugin's classifications against the form it was given.
    ///
    /// Two asymmetric rules, and the asymmetry is the plugin contract's own
    /// choice rather than this gear's:
    ///
    /// * a **submitted key the plugin did not classify is dropped**, with one
    ///   `warn` naming the key. `qa-vhp-product-plugin`'s
    ///   `validate_credentials` documents why: undeclared keys "are ignored
    ///   rather than rejected: the gear writes only what a plugin classified,
    ///   so an undeclared key reaches nothing, and rejecting the whole form
    ///   over one is a worse failure than dropping it". The `warn` is what
    ///   stops a dropped credential being silent, and it names the key —
    ///   never a value (ruling F-5);
    /// * a **classification for a key that was not submitted is refused**,
    ///   because acting on it would have this gear mint a secret out of
    ///   nothing.
    ///
    /// A reference offered for a non-secret field is refused too: `config`
    /// holds values, not credstore references, so there is nothing sensible to
    /// store.
    fn reconcile_classifications(
        submitted: &BTreeMap<String, CredentialSubmission>,
        classifications: Vec<CredentialClassification>,
    ) -> Result<Vec<CredentialClassification>, DomainError> {
        for classification in &classifications {
            if !submitted.contains_key(&classification.key) {
                // `classification.key` is a `String` the PLUGIN chose, and
                // the plugin was handed the submitted plaintext in
                // `CredentialInput`. This is the one place in this file where
                // a plugin-controlled runtime string is interpolated into an
                // operator-facing message, and the "Credential containment"
                // rule's layer 1 (`PRODUCT-PLUGINS-DESIGN.md` §9, cited here
                // before the docs squash, no longer exists; the rule itself
                // is unchanged) exists to make exactly that inexpressible
                // for *failure* text (`PluginFailure::detail` is
                // `&'static str`).
                //
                // It is bounded by layer 3 rather than by layer 1: the
                // leak-conformance harness plants a canary in the submitted
                // material and asserts it appears in no string the plugin
                // caused to be produced, this refusal included. Naming the key
                // is also the only way an operator can report the plugin bug,
                // and a key is not material. Recorded because the reasoning is
                // non-obvious and this file is otherwise strict about the rule
                // (review m-4).
                return Err(DomainError::Internal(format!(
                    "this product's plugin classified a credential field (`{}`) that the request \
                     did not submit; refusing rather than storing a credential with no value",
                    classification.key
                )));
            }
            if !classification.is_secret
                && matches!(
                    submitted.get(&classification.key),
                    Some(CredentialSubmission::Reference(_))
                )
            {
                return Err(DomainError::Validation {
                    field: classification.key.clone(),
                    message: "this field is not credential material, so it is stored as a value \
                              and cannot be given as a credential store reference"
                        .to_owned(),
                });
            }
        }

        let classified: std::collections::BTreeSet<&str> = classifications
            .iter()
            .map(|classification| classification.key.as_str())
            .collect();
        for key in submitted.keys() {
            if !classified.contains(key.as_str()) {
                warn!(
                    credential_key = %key,
                    "qa-environments: this product's plugin does not declare a credential field \
                     under this key, so the submitted value was not stored"
                );
            }
        }

        Ok(classifications)
    }

    /// Merge freshly stored credentials over whatever the row already held,
    /// and collect the references that were displaced.
    ///
    /// The result is the **complete** post-write state, because a
    /// `credentials` JSON array cannot be partially assigned. A key the
    /// request did not mention keeps its stored reference and supersedes
    /// nothing.
    ///
    /// The comparison against the reference the key **ends up with** is
    /// load-bearing rather than defensive tidiness, and it is the same rule
    /// `update_environment` already applied to the single-kubeconfig case: a
    /// request may name the reference already stored — a no-op rotation, or a
    /// retry — and deleting that would destroy the secret the row still points
    /// at, an environment silently broken by a request that changed nothing.
    fn merge_credentials(
        existing: Option<&Environment>,
        written: Vec<EnvironmentCredential>,
        submitted: &BTreeMap<String, CredentialSubmission>,
        sole_key: Option<&str>,
    ) -> (Vec<EnvironmentCredential>, Vec<String>) {
        let Some(existing) = existing else {
            return (written, Vec::new());
        };

        let stored = Self::stored_credentials(existing, sole_key);
        let mut superseded = Vec::new();
        let mut merged: Vec<EnvironmentCredential> = Vec::new();

        // Keys the row already had: replaced where the request mentioned them,
        // kept otherwise.
        for previous in &stored {
            match written
                .iter()
                .find(|credential| credential.key == previous.key)
            {
                Some(replacement) => {
                    if replacement.credstore_ref != previous.credstore_ref {
                        superseded.push(previous.credstore_ref.clone());
                    }
                    merged.push(replacement.clone());
                }
                None => {
                    // Not mentioned, or mentioned as a non-secret field -- in
                    // which case its value is in `config` now and it is not a
                    // credential at all. `submitted` distinguishes the two.
                    if submitted.contains_key(&previous.key) {
                        // Reclassified from secret to non-secret by the
                        // plugin, between releases. The stored credential is
                        // dropped from the row, so it must also be
                        // **superseded** -- otherwise the secret stays in
                        // credstore with nothing able to name it, the orphan
                        // `delete_ssh_key` exists to avoid (review m-6).
                        // Ownership is tested at the delete, as everywhere.
                        superseded.push(previous.credstore_ref.clone());
                    } else {
                        merged.push(previous.clone());
                    }
                }
            }
        }
        // Keys the row did not have.
        for credential in written {
            if !stored.iter().any(|previous| previous.key == credential.key) {
                merged.push(credential);
            }
        }

        (merged, superseded)
    }

    /// The credentials a row currently holds, preferring the plugin-shaped
    /// column and falling back to the pre-plugin one.
    ///
    /// This is the same preference [`EnvironmentsService::resolve_credential_slots`]
    /// and `qa-runs`' `plugin_dispatch` apply, and it exists in three places
    /// for one release only: Task 19 drops the fallback with the column.
    ///
    /// The fallback is a **lossy** source — it can name at most one credential
    /// — which is why it is only ever consulted when `credentials` is empty,
    /// and why Task 19's re-derivation runs the other way only on rows that
    /// have no more than one. See `PersistedCredentials::legacy_ref` (review
    /// finding CRITICAL-1).
    ///
    /// # The fallback needs the plugin to supply the key
    ///
    /// The legacy column holds a reference and **no key**, and a merge is
    /// keyed: without a key the stored reference matches no replacement, so it
    /// would be kept *beside* the new one and the row would end up naming the
    /// same credential twice. So `sole_key` is
    /// [`sole_required_secret_key`] over the plugin's schema — the same
    /// derivation rulings E-17/D-19 use for exactly this column, in both gears
    /// that read it.
    ///
    /// `None` means the plugin declares no single required secret, so the
    /// legacy reference cannot be attributed to any key. It is reported as
    /// *no stored credentials* rather than guessed at, which is the same
    /// refusal `LEGACY_CREDENTIAL_UNBINDABLE` and `AMBIGUOUS_CREDENTIAL`
    /// already make: a guess here would supersede — and delete — a secret
    /// under the wrong name.
    fn stored_credentials(
        existing: &Environment,
        sole_key: Option<&str>,
    ) -> Vec<EnvironmentCredential> {
        let _ = sole_key;
        existing.credentials.clone()
    }

    /// Merge the non-secret submitted fields over the stored `config` object.
    ///
    /// A submitted field wins; every other stored key survives. A corrupt or
    /// non-object stored `config` is replaced rather than merged into, because
    /// there is nothing to merge with and the column is `NOT NULL`.
    fn merge_config(
        existing: Option<&Environment>,
        fields: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Value {
        let mut merged = existing
            .and_then(|environment| environment.config.as_object().cloned())
            .unwrap_or_default();
        merged.extend(fields);
        serde_json::Value::Object(merged)
    }

    /// The value for the pre-plugin `kubeconfig_credstore_ref` column: the
    /// reference belonging to the plugin's sole required secret field.
    ///
    /// Empty when the plugin declares no single required secret — one column
    /// cannot say which of two required secrets it holds, and both readers
    /// already refuse that case rather than guess
    /// (`plugin_dispatch`'s `AMBIGUOUS_CREDENTIAL`, this gear's
    /// `LEGACY_CREDENTIAL_UNBINDABLE`). Empty also when that field simply was
    /// not among the submitted credentials, which a patch that replaced some
    /// *other* credential legitimately produces — and which is exactly why the
    /// merge above runs first, so this reads the post-merge state and finds the
    /// reference the row is keeping.
    fn legacy_reference(sole_key: Option<&str>, credentials: &[EnvironmentCredential]) -> String {
        let Some(key) = sole_key else {
            return String::new();
        };
        credentials
            .iter()
            .find(|credential| credential.key == key)
            .map_or_else(String::new, |credential| credential.credstore_ref.clone())
    }

    /// Write a pasted credential to credstore under a freshly generated
    /// reference and return that reference — the only value that goes on to
    /// the database.
    ///
    /// `SharingMode::Tenant` matches `create_ssh_key`; the caveats that mode
    /// carries are documented at length in `ssh_keys.rs`'s module header and
    /// apply here identically.
    async fn write_generated_credential(
        &self,
        ctx: &SecurityContext,
        material: &CredentialMaterial,
    ) -> Result<String, DomainError> {
        self.write_generated_secret(ctx, material.clone()).await
    }
}

/// Whether a credstore reference is one **this gear minted** and is therefore
/// responsible for deleting.
///
/// # Both prefixes, and why the old one cannot be dropped
///
/// Task 18b generalised the prefix from `qa-environments-kubeconfig-` to
/// [`GENERATED_CREDENTIAL_REF_PREFIX`], because the old one named one
/// product's credential in the gear whose whole purpose is to stop doing that.
/// But **every kubeconfig this gear minted before Task 18b still carries the
/// old prefix**, and ownership is not a naming convention — it is what decides
/// whether `forget_owned_secret` deletes. Testing only the new prefix would
/// make every pre-existing generated secret un-deletable, so a rotation would
/// leave it orphaned in credstore forever: the exact failure the
/// `previous_ref` orphan in `update_environment` was measured and fixed for.
///
/// A caller-supplied reference matches neither and is never this gear's to
/// destroy.
pub(super) fn is_generated_ref(raw_ref: &str) -> bool {
    raw_ref.starts_with(GENERATED_CREDENTIAL_REF_PREFIX)
        || raw_ref.starts_with(LEGACY_GENERATED_KUBECONFIG_REF_PREFIX)
}

/// The prefix this gear minted generated kubeconfig references under before
/// Task 18b. Retained **only** as an ownership marker — nothing writes it any
/// more. See [`is_generated_ref`].
pub(super) const LEGACY_GENERATED_KUBECONFIG_REF_PREFIX: &str = "qa-environments-kubeconfig-";
