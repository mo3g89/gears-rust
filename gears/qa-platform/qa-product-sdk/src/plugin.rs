//! The contract every QA Platform product plugin implements. See
//! `gears/qa-platform/docs/features/product-plugins.md`'s "The contract"
//! section (`PRODUCT-PLUGINS-DESIGN.md` §5, cited here before the docs
//! squash, no longer exists).

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::SecretValue;

use crate::access::{RunAccess, RunVarContract, RunnerSpec};
use crate::descriptor::{FieldDesc, FieldRole, SchemaError, validate_schemas};
use crate::observation::{ObservedAttrs, PluginFailure, PluginObservation, project_roles};

/// Raw fields a submitted credential form contains, keyed by
/// [`FieldDesc::key`].
///
/// # Why the values are `SecretValue` and not `String`
///
/// This is the type that carries a freshly pasted kubeconfig from the
/// credential form into [`QaProductPluginV1::validate_credentials`] — the
/// exact path the 2026-08-28 leak travelled. With a bare `String` and a
/// derived `Debug`, one `tracing::debug!(?input)` — in a plugin *or* in the
/// gear's own handler, which no plugin-side layer covers — renders every
/// submitted secret in the clear. [`SecretValue`]'s `Debug` is `[REDACTED]`,
/// so the derived `Debug` here prints the *keys* and nothing else.
///
/// `PartialEq`/`Eq` are deliberately absent: `==` on secret bytes is a timing
/// footgun and no consumer needs it. Compare classifications, not values.
#[derive(Debug, Default)]
pub struct CredentialInput {
    pub fields: BTreeMap<String, SecretValue>,
}

impl CredentialInput {
    /// The submitted value for `key`, if the form carried one.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&SecretValue> {
        self.fields.get(key)
    }

    /// Whether the form carried a value for `key`.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }
}

/// One submitted field, classified by
/// [`QaProductPluginV1::validate_credentials`].
///
/// This carries only what the plugin can actually determine: which submitted
/// keys are credential material. It deliberately carries **no**
/// `credstore_ref` — the plugin runs *before* the gear writes to credstore,
/// so a reference is not a thing it could know. The gear→plugin direction,
/// where a reference does exist, is [`CredentialSlot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialClassification {
    /// The key the field was submitted under, matching a
    /// [`FieldDesc::key`] from [`QaProductPluginV1::credential_schema`].
    pub key: String,
    /// `true` when the gear must write this field to credstore rather than to
    /// the environment's non-secret configuration.
    pub is_secret: bool,
}

impl CredentialClassification {
    /// A field the gear must write to credstore.
    #[must_use]
    pub fn secret(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            is_secret: true,
        }
    }

    /// A field that belongs in the environment's non-secret configuration.
    #[must_use]
    pub fn config(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            is_secret: false,
        }
    }
}

/// One credential the gear holds for an environment, in the direction
/// gear → plugin: the key it belongs under, the credstore reference the gear
/// wrote it to, and its plaintext **only if the caller already resolved it**.
///
/// # Why `value` is optional
///
/// `qa-runs`' dispatch passes a *reference* today
/// (`KubeconfigMount { secret: SecretRef::new(..) }`) and never materialises
/// kubeconfig plaintext in its own process. A non-optional resolved value
/// here would force `build_spec` to resolve every credential to plaintext
/// purely to satisfy this type's shape — a change whose stated purpose is
/// tightening secret containment would end by pulling plaintext kubeconfigs
/// into a process that today never touches them.
///
/// So resolution is the *caller's* choice, per call:
/// [`QaProductPluginV1::observe`] needs plaintext and gets it;
/// [`QaProductPluginV1::prepare_run_access`] **must work from
/// `credstore_ref` alone** and must never require [`Self::value`].
#[derive(Debug)]
pub struct CredentialSlot {
    pub key: String,
    pub credstore_ref: String,
    pub value: Option<SecretValue>,
}

impl CredentialSlot {
    /// A slot the caller chose not to resolve — the shape dispatch passes.
    #[must_use]
    pub fn reference_only(key: impl Into<String>, credstore_ref: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            credstore_ref: credstore_ref.into(),
            value: None,
        }
    }

    /// A slot the caller resolved from credstore for this call.
    #[must_use]
    pub fn resolved(
        key: impl Into<String>,
        credstore_ref: impl Into<String>,
        value: SecretValue,
    ) -> Self {
        Self {
            key: key.into(),
            credstore_ref: credstore_ref.into(),
            value: Some(value),
        }
    }
}

/// What a plugin call needs to reach an environment: its credential slots,
/// its non-secret JSON configuration, and what observing it last detected.
///
/// The slots are *lazy* by construction — see [`CredentialSlot::value`]. A
/// method that only needs to name a secret (mounting it into a run) reads
/// [`Self::credstore_ref`]; a method that must read one (observing) reads
/// [`Self::resolved`] and fails cleanly when the caller did not resolve it.
///
/// The three channels are kept apart because they have three different
/// authors: credstore wrote the slots, an operator wrote the config, and the
/// plugin's own [`QaProductPluginV1::observe`] wrote the observation.
#[derive(Clone, Copy, Debug)]
pub struct EnvironmentHandle<'a> {
    pub slots: &'a [CredentialSlot],
    pub config: &'a serde_json::Value,
    /// What the environment's last successful observation detected —
    /// `qa_environments.observed_attrs`, as the plugin itself returned it.
    ///
    /// The gear sets this; a plugin only reads it, most usefully through
    /// [`Self::observed_role`]. It is what gives
    /// [`QaProductPluginV1::prepare_run_access`] a source for run variables
    /// whose values are *detected* rather than configured — a base URL, a
    /// namespace — which the slots and the config between them cannot
    /// supply.
    ///
    /// **Not merged into [`Self::config`], deliberately.** `config` is
    /// operator-set and this is machine-observed; folding one into the other
    /// would leave the environment page unable to say which of two values on
    /// it a human is allowed to correct. That distinction is the reason the
    /// `observed_attrs` column exists separately in the first place.
    ///
    /// `Option` because `prepare_run_access` can legitimately be called on an
    /// environment that has never been observed. A plugin must therefore
    /// treat a missing observation as a normal shape and still return `Ok` —
    /// the leak-conformance harness drives exactly that shape.
    pub observed: Option<&'a ObservedAttrs>,
}

impl EnvironmentHandle<'_> {
    /// The slot declared under `key`, if the environment has one.
    #[must_use]
    pub fn slot(&self, key: &str) -> Option<&CredentialSlot> {
        self.slots.iter().find(|slot| slot.key == key)
    }

    /// The credstore reference for `key`. Always available when the
    /// environment has the credential at all — this is what
    /// [`QaProductPluginV1::prepare_run_access`] uses.
    #[must_use]
    pub fn credstore_ref(&self, key: &str) -> Option<&str> {
        self.slot(key).map(|slot| slot.credstore_ref.as_str())
    }

    /// The plaintext for `key`, present only when *this* caller resolved it.
    #[must_use]
    pub fn resolved(&self, key: &str) -> Option<&SecretValue> {
        self.slot(key).and_then(|slot| slot.value.as_ref())
    }

    /// The observed value whose declared field claims `role`, projected out
    /// of [`Self::observed`].
    ///
    /// `schema` is the plugin's own [`QaProductPluginV1::observed_schema`]:
    /// a role is a property of the *declaration*, not of the attribute map,
    /// so the two are needed together. This is
    /// [`crate::observation::project_roles`] — the same projection the
    /// platform's `observed_version`/`observed_build`/`observed_base_url`
    /// columns are written from, so a run variable built here and a column
    /// rendered on the environment page can never disagree about which
    /// attribute a role means.
    ///
    /// `None` when the environment has never been observed, when no declared
    /// field claims `role`, and when the claimed attribute is absent or
    /// blank — `project_roles` treats a blank as unset, because an empty
    /// `APP_VERSION` reaching every test is the failure that rule guards.
    #[must_use]
    pub fn observed_role(&self, schema: &[FieldDesc], role: FieldRole) -> Option<String> {
        let projected = project_roles(schema, self.observed?);
        match role {
            FieldRole::Version => projected.version,
            FieldRole::Build => projected.build,
            FieldRole::BaseUrl => projected.base_url,
            FieldRole::Namespace => projected.namespace,
        }
    }
}

/// The contract every QA Platform product plugin implements.
///
/// Eight methods, grouped as they are dispatched: declaration (drives the UI
/// and the platform's semantic bindings), environment lifecycle
/// (`qa-environments`), and dispatch (`qa-runs`). Task 9–10 implement this
/// trait; Task 15 calls [`Self::observe`]; Task 18 calls
/// [`Self::prepare_run_access`], [`Self::runner`] and [`Self::env_contract`].
#[async_trait]
pub trait QaProductPluginV1: Send + Sync {
    // ── Declaration: drives the UI and the platform's semantic bindings ──
    /// The credential fields this product's environments need. Rendered as
    /// the create/edit form; checked by [`crate::descriptor::validate_schemas`]
    /// at registration.
    fn credential_schema(&self) -> Vec<FieldDesc>;
    /// The fields this product's [`Self::observe`] can yield. Rendered on the
    /// environment page, so `validate_schemas` refuses a secret kind here.
    fn observed_schema(&self) -> Vec<FieldDesc>;

    // ── Environment lifecycle (qa-environments) ──
    /// Validate a submitted credential form and say which submitted keys are
    /// credential material.
    ///
    /// Returns [`CredentialClassification`] rather than anything credstore-
    /// shaped because only the *gear* may write credstore — only it holds the
    /// tenant-scoped `SecurityContext` — and the plugin runs before that
    /// write happens. The plugin therefore cannot know a `credstore_ref`, and
    /// this signature no longer asks it to invent one. Its job is exactly:
    /// reject a malformed form, and say which of its fields must be stored as
    /// secrets.
    ///
    /// # Errors
    ///
    /// Returns [`PluginFailure`] when the submitted credential is rejected or
    /// malformed.
    async fn validate_credentials(
        &self,
        input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure>;

    /// Observe the environment this handle points at: its detected
    /// attributes and its health, in one call so one client and one
    /// handshake serve both.
    ///
    /// This is the one method whose caller is expected to have resolved the
    /// credential slots it needs ([`CredentialSlot::resolved`]).
    async fn observe(&self, env: &EnvironmentHandle<'_>) -> PluginObservation;

    /// Prepare what a run needs to reach this environment: mounts,
    /// environment variables, and a service account.
    ///
    /// **Must work from credstore references alone.** Read
    /// [`EnvironmentHandle::credstore_ref`], never
    /// [`EnvironmentHandle::resolved`]: dispatch calls this without resolving
    /// anything, precisely so no plaintext credential is ever materialised in
    /// the dispatching process. A plugin that needs the bytes of a secret to
    /// build a mount has the wrong mount — use
    /// [`crate::access::MountSpec::Secret`], which names the reference and
    /// lets the executor do the resolving.
    ///
    /// Run variables whose values are *detected* rather than configured — a
    /// base URL, a namespace — come from
    /// [`EnvironmentHandle::observed_role`]. That is `None` on an
    /// environment nothing has observed yet, which is a shape dispatch can
    /// legitimately produce: omit the variable, do not fail the call.
    ///
    /// # Errors
    ///
    /// Returns [`PluginFailure`] when access cannot be prepared — the
    /// environment declares no such credential, or its configuration is
    /// unusable.
    async fn prepare_run_access(
        &self,
        env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure>;

    // ── Dispatch (qa-runs) ──
    /// The runner image and command for a run of this product, optionally
    /// informed by the environment's last observation.
    fn runner(&self, observed: Option<&ObservedAttrs>) -> RunnerSpec;
    /// The run-variable names this plugin reserves beyond the platform's own
    /// floor (see [`RunVarContract::union_with_floor`]). The names it *owns*
    /// are not declared here — they are whatever
    /// [`Self::prepare_run_access`] actually returns in
    /// [`RunAccess::env`](crate::access::RunAccess::env).
    fn env_contract(&self) -> RunVarContract;
}

/// A plugin that has passed [`validate_schemas`] — the only door through
/// which a plugin becomes usable.
///
/// Layer 2 was a free function a gear's `init` was *supposed to remember* to
/// call. Nothing made registration go through it, and a check that can be
/// forgotten is a check that eventually is. Holding the validated plugin in a
/// distinct type moves that from a convention to a type-system fact: a
/// registry that stores `RegisteredPlugin` cannot hold a plugin whose schemas
/// were never checked, because there is no way to construct one.
///
/// [`Deref`] exposes the plugin itself, so a holder calls
/// `registered.observe(&env)` directly.
#[derive(Clone)]
pub struct RegisteredPlugin(Arc<dyn QaProductPluginV1>);

impl RegisteredPlugin {
    /// Validate both declared schemas and admit the plugin.
    ///
    /// # Errors
    ///
    /// Returns the [`SchemaError`] [`validate_schemas`] found. At
    /// registration this is a boot failure: a plugin whose `observed_schema`
    /// declares a secret would render credential material on the environment
    /// page.
    pub fn new(plugin: Arc<dyn QaProductPluginV1>) -> Result<Self, SchemaError> {
        validate_schemas(&plugin.credential_schema(), &plugin.observed_schema())?;
        Ok(Self(plugin))
    }

    /// The validated plugin, for a caller that needs the `Arc` itself.
    #[must_use]
    pub fn inner(&self) -> &Arc<dyn QaProductPluginV1> {
        &self.0
    }

    /// Give up the wrapper and keep the plugin.
    #[must_use]
    pub fn into_inner(self) -> Arc<dyn QaProductPluginV1> {
        self.0
    }
}

impl Deref for RegisteredPlugin {
    type Target = dyn QaProductPluginV1;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

// `dyn QaProductPluginV1` is not `Debug` and deliberately stays that way — a
// blanket `Debug` on a plugin object is one more surface a careless
// implementation could render a cached credential on.
impl fmt::Debug for RegisteredPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisteredPlugin").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod plugin_tests;
