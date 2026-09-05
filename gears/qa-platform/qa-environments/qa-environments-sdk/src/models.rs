//! Transport-agnostic models for the qa-environments contract.

use std::collections::BTreeMap;

use qa_product_sdk::observation::{HealthState, ObservedAttrs};
use time::OffsetDateTime;
use uuid::Uuid;

/// A raw credential **document**, supplied by an operator who has one to paste
/// rather than a credstore reference to name.
///
/// Named for what it carries rather than for one product's credential: a
/// kubeconfig is one instance, and which fields of a submitted form are made
/// of this is the product plugin's answer
/// ([`qa_product_sdk::plugin::CredentialClassification`]), not this crate's.
/// It was `KubeconfigMaterial` until Task 18b.
///
/// # Why this is a newtype and not a `String`
///
/// A kubeconfig's `users[].user.client-key-data` is a client **private key** —
/// the same class of material as the SSH key `qa-catalog` stores
/// (`qa-catalog/src/domain/service/ssh_keys.rs`). It must reach credstore and
/// nothing else: no log line, no `#[instrument]` field, no error message, no
/// response body. That was measured, not supposed: on 2026-08-28 a formatted
/// kubeconfig error put a PEM private key on the platform page, which is the
/// incident `qa-environments/src/infra/observer/errors.rs` exists to answer.
///
/// [`Debug`] is therefore hand-written to print `[REDACTED]`, mirroring
/// `qa-catalog`'s `CreateSshKeyReq` (`qa-catalog/src/api/rest/dto.rs`) and
/// credstore's own `CreateSecretRequestDto`. Because this type — not a bare
/// `String` — is what [`NewEnvironment`] and [`EnvironmentPatch`] carry, **their**
/// derived `Debug` is redacted too, and so is that of anything holding one of
/// them. A `debug!("{new:?}")` added anywhere later cannot leak the document;
/// that is a property of the type rather than a convention someone has to
/// remember.
///
/// The material is never returned by any read path: `Environment` carries
/// only [`EnvironmentCredential::credstore_ref`] and, until Task 19,
/// [`Environment::kubeconfig_credstore_ref`].
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialMaterial(String);

impl CredentialMaterial {
    /// Wrap a raw credential document.
    #[must_use]
    pub fn new(document: String) -> Self {
        Self(document)
    }

    /// Borrow the raw document. Named `expose` (not `as_str`) so that every
    /// call site reads as a deliberate decision to handle key material.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper, yielding the raw document. Only the credstore
    /// write path should need this.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for CredentialMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialMaterial([REDACTED])")
    }
}

impl From<String> for CredentialMaterial {
    fn from(document: String) -> Self {
        Self(document)
    }
}

/// One credential a caller submitted, in the shape they supplied it.
///
/// # Why the two arms are an enum and not two fields
///
/// The pre-plugin API spelled this as `kubeconfig_credstore_ref: Option<String>`
/// beside `kubeconfig: Option<CredentialMaterial>`, with "exactly one of these
/// must be present" enforced by a validator and re-enforced, differently, on
/// the patch path. This type **cannot express** the violation, so there is
/// nothing to validate and nothing for the two paths to disagree about.
///
/// The distinction is not cosmetic: it is the ownership boundary
/// `EnvironmentsService::forget_owned_secret` tests. A [`Self::Material`] is
/// written to credstore under a reference this gear generates and therefore
/// owns and will delete; a [`Self::Reference`] names a secret that may be
/// shared with other systems and that this gear never deletes.
///
/// # The derives are inherited, not chosen
///
/// `PartialEq`/`Eq` are here because [`CredentialMaterial`] already has them
/// and [`NewEnvironment`] already derives `PartialEq` over it. That is the
/// opposite of the call [`qa_product_sdk::plugin::CredentialInput`] makes,
/// where `==` on secret bytes was refused as a timing footgun with no
/// consumer. The asymmetry is pre-existing and this type is not the place to
/// resolve it: dropping `PartialEq` here means dropping it from both request
/// models and from every test that compares one. **Nothing compares the secret
/// arm on purpose** — the comparisons are whole-request equality in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialSubmission {
    /// A pasted document, to be written to credstore under a generated
    /// reference this gear then owns.
    Material(CredentialMaterial),
    /// A credstore reference the caller already holds. Never written by this
    /// gear, and never deleted by it either.
    Reference(String),
}

/// A registered target environment (system under test).
#[derive(Clone, Debug, PartialEq)]
pub struct Environment {
    pub id: Uuid,
    pub name: String,
    /// The product in qa-catalog this environment belongs to (by ID; no
    /// cross-gear FK).
    ///
    /// **Required since Task 20b** (`m20260903_000013`, decision **D9**). It is
    /// how the plugin resolves, so an environment without one could be neither
    /// observed nor dispatched against — a row that existed only to record that
    /// it could not be used.
    pub product_id: Uuid,
    pub description: Option<String>,
    /// Operator-controlled availability toggle: an unavailable environment accepts no new leases.
    pub available: bool,
    /// Last product version observed on the environment (populated by the p2 version poller).
    pub observed_version: Option<String>,
    /// Last product **build** identifier observed on the environment, alongside
    /// [`Self::observed_version`] and populated by the same p2 version poller.
    ///
    /// Added 2026-08-13 by user decision, raised by qa-runs Task 9: the source
    /// system keeps a build alongside the version
    /// (`manager/migrations/001_initial.sql:143`, `platforms_meta.build`) and
    /// stamps every run with it (`:170`), which qa-runs snapshots onto
    /// `qa_runs.app_build` and feeds to the runner as `APP_BUILD`. Without this
    /// field that variable reaches every test empty, and
    /// `PRD.md:577`'s "environment variable names consumed by tests … are
    /// unchanged, so existing test repositories run unmodified" would be
    /// partially false.
    ///
    /// **No writer yet**, exactly like `observed_version`: the version poller is
    /// unbuilt and both are tracked together in `DECOMPOSITION.md` 2.1 so one
    /// retrofit closes both.
    pub observed_build: Option<String>,
    /// Per-environment default branch **override**: the middle tier of qa-runs'
    /// branch-resolution chain, which is `explicit → this → the repository's
    /// own default_branch` (parity spec §3.4 rule 1).
    ///
    /// `None` means "no override", not "no branch" — the repository default
    /// then applies. Always either `None` or a trimmed, non-empty string:
    /// every write path normalises, so an empty or whitespace-only value is
    /// stored as `None`.
    ///
    /// # Why this exists
    ///
    /// Added 2026-08-14 by user decision (qa-runs Task 13b), raised by Task 13.
    /// The source system has the column — `platforms_meta.default_branch TEXT`
    /// (`manager/migrations/001_initial.sql:233`) — read by **legacy's**
    /// `PlatformsService::get_platform_default_branch`, whose own doc calls it
    /// *"the per-platform default branch override (falls back to repo
    /// default)"* (`manager/src/services/platforms.rs:845-848`). Legacy's
    /// "platform" is this gear's [`Environment`]; the names in that citation
    /// are legacy's and do not rename with it. This gear
    /// shipped without it, so a launch that named no branch silently used the
    /// *repository's* default where the source system would have used the
    /// environment's — with no error and no warning for an operator who had
    /// pinned an environment to a release branch.
    ///
    /// Unlike [`Self::observed_version`] and [`Self::observed_build`], which are
    /// machine-written and therefore appear in no write DTO, this field is
    /// **operator-set**: it is present in [`NewEnvironment`], [`EnvironmentPatch`] and
    /// the REST request types. Mirroring `observed_build`'s read-only shape here
    /// would have shipped a field nothing could set.
    pub default_branch: Option<String>,
    /// Whether this environment is its product's **default** — what the Run and
    /// Schedule dialogs' "Default cluster" option resolves to.
    ///
    /// # Why this exists, and why it is not the source system's behaviour
    ///
    /// In the source system "Default cluster" meant *no platform at all* — legacy's
    /// name for what this gear calls an environment — and the
    /// runner fell through to its own in-cluster `ServiceAccount`
    /// (`manager/src/routes/runs.rs` resolves the platform context to
    /// `(None, None, None, None)`; `manager/src/services/argo.rs` mounts a
    /// kubeconfig only `if let Some(platform_name)`). That worked because the
    /// source system's manager ran **inside** the cluster under test. qa-platform
    /// runs beside the clusters it tests, so that meaning is dead here — it
    /// produced runs that dispatched and then failed every test on missing
    /// credentials. **Decision (user, 2026-08-31): the option resolves to the
    /// product's default environment instead.**
    ///
    /// **At most one environment per (tenant, product) has this set.**
    /// `EnvironmentsService` enforces it by clearing the previous holder in the same
    /// transaction, rather than a database constraint — see
    /// `m20260831_000009_platform_is_default`'s module doc for why.
    ///
    /// Operator-set, like [`Self::default_branch`], so it appears in
    /// [`NewEnvironment`], [`EnvironmentPatch`] and the REST request types.
    pub is_default: bool,
    /// The message from the most recent **failed** observation attempt, or
    /// `None` if the most recent attempt succeeded (or none has run yet).
    /// Cleared back to `None` by the next successful observation. This is a
    /// **value**, not this gear's own error: an environment whose cluster cannot
    /// be reached is a fact about the environment, and this field is what lets
    /// an operator read why (`POST /qa/v1/environments/{id}/refresh` returns
    /// HTTP 200 with this field populated, never a 5xx, when only detection
    /// itself failed).
    pub version_detect_error: Option<String>,
    /// When the most recent observation attempt — success or failure — ran.
    /// `None` means no attempt has ever completed.
    pub version_detected_at: Option<OffsetDateTime>,
    /// The environment's credentials in the shape a product plugin consumes:
    /// one entry per credential key, each naming the credstore reference the
    /// material lives behind. Empty for an environment whose credentials have
    /// never been written through the plugin path.
    ///
    /// Populated by the plugin path; the legacy column beside it
    /// ([`Self::kubeconfig_credstore_ref`]) is still authoritative until the
    /// contract migration. `m20260903_000011_environment_plugin_columns`
    /// backfills this from that field, so the two agree from the moment the
    /// migration runs.
    ///
    /// **In-process consumers only**, for [`Self::kubeconfig_credstore_ref`]'s
    /// reason and no other: under `SharingMode::Tenant` a credstore reference
    /// *is* a read path to the material, so `EnvironmentDto` drops this field
    /// exactly as it drops that one.
    pub credentials: Vec<EnvironmentCredential>,
    /// The most recent observation's plugin-defined attributes, keyed by
    /// `FieldDesc::key`. Empty for an environment the plugin path has never
    /// observed.
    ///
    /// Every key here was declared in the plugin's `observed_schema()`:
    /// `qa_product_sdk::observation::retain_declared` drops the rest before
    /// the column is written, which is what makes this map safe to render.
    /// The four role-claimed attributes are *also* projected into their own
    /// columns ([`Self::observed_version`], [`Self::observed_build`],
    /// [`Self::observed_base_url`] and [`Self::observed_namespace`]) by
    /// `project_roles`, so a run variable and a column cannot disagree about
    /// which attribute a role means.
    ///
    /// Populated by the plugin path; deliberately **not** backfilled — the
    /// keys are the plugin's to choose, and the first observation cycle
    /// writes them.
    pub observed_attrs: ObservedAttrs,
    /// The operator-set, **non-secret** half of this environment's credential
    /// fields — `EnvironmentHandle`'s `config` channel, passed to the plugin
    /// verbatim. An empty object when nothing is overridden.
    ///
    /// Kept apart from [`Self::observed_attrs`] because the two have different
    /// authors: this one a human, that one a machine. Conflating them would
    /// leave the environment page unable to say which of two values a human
    /// may correct.
    ///
    /// Populated by the plugin path;
    /// `m20260903_000011_environment_plugin_columns` backfills it from each
    /// environment's `VPADM_NAMESPACE` variable, which stays authoritative
    /// for the legacy observer until the contract migration.
    pub config: serde_json::Value,
    /// The `FieldRole::BaseUrl` projection of [`Self::observed_attrs`] —
    /// [`Self::vhp_base_url`] with the product's name taken out of it. `None`
    /// means "never conclusively detected", exactly as it does there.
    ///
    /// Populated by the plugin path; the legacy column beside it
    /// (`vhp_base_url`) is still authoritative until the contract migration,
    /// and both are written from the same observation until then.
    pub observed_base_url: Option<String>,
    /// The most recent health verdict, in the vocabulary every product can
    /// express. [`HealthState::Unknown`] both for an environment nothing has
    /// looked at and for one whose health read failed —
    /// [`Self::health_checked_at`] is what separates those two.
    ///
    /// Populated by the plugin path; the legacy [`Self::cluster`] view beside
    /// it is still authoritative until the contract migration.
    pub health_state: HealthState,
    /// Why the most recent health read reached the state it did, when there is
    /// something to say. Classified text only — never a formatted error, and
    /// never anything derived from a credential.
    ///
    /// Populated by the plugin path; `cluster.status_message` is still
    /// authoritative until the contract migration.
    pub health_detail: Option<String>,
    /// When the most recent health read ran, or `None` if **nothing ever
    /// looked** — a different fact from a [`HealthState::Unknown`] a failed
    /// read produced, and the only thing that distinguishes them.
    ///
    /// Populated by the plugin path; `cluster.checked_at` is still
    /// authoritative until the contract migration.
    pub health_checked_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// One credential an environment holds, as a product plugin sees it: a key
/// from the plugin's `credential_schema()` and the credstore reference the
/// gear wrote the material to.
///
/// **There is no `value` field, and there must never be one.** This type is
/// the persisted shape of `qa_environments.credentials`, and leaving plaintext
/// structurally unrepresentable in it is what stops a JSONB column — one that
/// a DTO could later publish — from ever holding credential material. A
/// plugin that needs the material receives it on
/// `qa_product_sdk::plugin::CredentialSlot::value`, resolved per call and
/// never stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentCredential {
    /// The plugin's own key for this credential, e.g. `kubeconfig`.
    pub key: String,
    /// Where the material is, never the material.
    pub credstore_ref: String,
}

/// One node, as the environment's own cluster reported it in its most recent
/// successful health read.
///
/// **No consumer since Task 21** (user decision U4 deleted the cluster-health
/// UI, and Task 19 dropped the five `cluster_*` columns that fed it), and the
/// gear-side type it mirrored — `domain::observation::NodeSummary` — is
/// deleted too, along with the `infra::storage::mapper` conversion. Kept for
/// now because removing a type from a published contract crate is a separate,
/// wider change than this branch's; nothing populates it (re-review, N-7).
///
/// The live equivalent is product-defined rather than Kubernetes-shaped:
/// [`Environment::observed_attrs`], keyed by the product plugin's own
/// `observed_schema()`.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSummary {
    pub name: String,
    pub control_plane: bool,
    pub ready: bool,
    pub kubelet_version: Option<String>,
    pub os_image: Option<String>,
}

/// Node counts derived, once, from a [`ClusterHealthView`]'s `nodes` —
/// server-side, in Rust — so that every consumer (this SDK's own callers, the
/// REST DTO, and ultimately the UI) renders the same numbers rather than each
/// re-deriving them and risking disagreement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeCounts {
    pub total: u32,
    pub ready: u32,
    pub control_plane: u32,
    pub ready_control_plane: u32,
    pub worker: u32,
    pub ready_worker: u32,
}

/// One environment's cluster-health reading, as of [`Self::checked_at`].
///
/// # Why this is one optional field on [`Environment`], not several
///
/// `Environment::cluster` being `None` means "no cycle has reached this
/// environment"; `Some(_)` here, even with `status == "Unreachable"`, means one
/// has. A set of flat nullable fields (`cluster_status: Option<String>` and
/// so on, each independently `None`) cannot represent that distinction: both
/// "never checked" and "checked, unreachable, nothing to show" would read as
/// all-`None`. One optional struct keeps the three states — never checked,
/// checked-and-failed, checked-and-read — distinguishable at the type level.
#[derive(Clone, Debug, PartialEq)]
pub struct ClusterHealthView {
    /// `"Healthy"`, `"Degraded"`, `"Unhealthy"`, `"Warning"` for a successful
    /// read, or `"Unreachable"` for a read that failed. `"Unreachable"` is
    /// this SDK's own name for that outcome. The gear's internal
    /// `ClusterStatus` domain type deliberately had no such variant, because
    /// unreachable is the absence of a reading rather than a property of one
    /// that succeeded — that type is deleted now, and this whole view is
    /// unpopulated (see [`NodeSummary`]'s header, re-review N-7).
    pub status: String,
    /// Populated only when `status == "Unreachable"`; `None` for every other
    /// status, which the UI composes from `status` and `counts` instead.
    /// Always text already classified by the observer, never a formatted
    /// `kube::Error`.
    pub status_message: Option<String>,
    /// Empty when `status == "Unreachable"` (D-CH-3): a failed read has
    /// nothing to report on.
    pub nodes: Vec<NodeSummary>,
    /// `None` means the namespace count was not read (an `Unreachable`
    /// status, or a read that could not list namespaces) — not that the
    /// cluster has zero namespaces.
    pub namespace_count: Option<u32>,
    /// Derived once, server-side, from `nodes`. Never re-derive this from a
    /// UI — that would risk a second implementation disagreeing with this
    /// one.
    pub counts: NodeCounts,
    /// When this reading was taken, success or failure.
    pub checked_at: OffsetDateTime,
}

/// Creation request for a target environment.
///
/// The kubeconfig arrives one of two ways — a credstore reference the caller
/// already holds ([`Self::kubeconfig_credstore_ref`]) or a pasted document
/// ([`Self::kubeconfig`]) — and exactly one of them must be present. The
/// derived `Debug` is safe because the document is wrapped in
/// [`CredentialMaterial`], whose own `Debug` redacts.
#[derive(Clone, Debug, PartialEq)]
pub struct NewEnvironment {
    pub name: String,
    /// **A plain `Uuid` since Task 20b**, on Task 20a's precedent
    /// (`NewProduct::plugin_instance_id`): a productless create was already
    /// refused at runtime by ruling F-13, and making the state
    /// unrepresentable is better than validating it. The wire's `Option`
    /// survives one layer and dies at `TryFrom<CreateEnvironmentReq>`, where
    /// the message can name what to supply — which serde's "missing field"
    /// cannot.
    pub product_id: Uuid,
    pub description: Option<String>,
    /// A credstore reference the caller already holds.
    ///
    /// **Exactly one of this and [`Self::kubeconfig`] must be supplied.**
    /// `EnvironmentsService::create_environment` enforces that: both is a validation
    /// error naming both fields, neither is the pre-existing
    /// `kubeconfig_credstore_ref must not be empty` error. An empty string is
    /// treated as "not supplied", which is what this field's required-`String`
    /// predecessor already meant (`validate_credstore_ref` rejected `""`).
    pub kubeconfig_credstore_ref: Option<String>,
    /// A raw kubeconfig **document** pasted by an operator, as the alternative
    /// to [`Self::kubeconfig_credstore_ref`].
    ///
    /// The service writes it to credstore **first** and persists only the
    /// generated reference — the ordering `qa-catalog`'s `create_ssh_key` uses
    /// and comments on. The document itself never reaches this gear's database
    /// or any response. See [`CredentialMaterial`] for why it is not a
    /// `String`.
    pub kubeconfig: Option<CredentialMaterial>,
    /// Credentials the caller submitted, keyed by the plugin's own
    /// [`FieldDesc::key`](qa_product_sdk::descriptor::FieldDesc::key).
    ///
    /// This is the plugin-shaped channel and the one Task 22's generated form
    /// uses; the legacy [`Self::kubeconfig`]/[`Self::kubeconfig_credstore_ref`]
    /// pair is still accepted so the shipped UI keeps working, and
    /// `EnvironmentsService` desugars it into one entry of this map keyed by
    /// [`sole_required_secret_key`](qa_product_sdk::descriptor::sole_required_secret_key)
    /// over the product's plugin — never by the literal `"kubeconfig"`, which
    /// is one product's name for its own credential.
    ///
    /// Which entries are secret is not this crate's judgement to make: the
    /// plugin classifies each submitted key
    /// ([`validate_credentials`](qa_product_sdk::QaProductPluginV1::validate_credentials)),
    /// secrets go to credstore and [`Environment::credentials`], and the rest
    /// go to [`Environment::config`].
    pub credentials: BTreeMap<String, CredentialSubmission>,
    /// Optional per-environment default branch override; see
    /// [`Environment::default_branch`].
    ///
    /// **Two states are enough here because there is no stored value to keep**:
    /// this struct only ever describes a row that does not exist yet, so "leave
    /// it unchanged" has no referent and `None` can mean "no override" without
    /// ambiguity. That is the whole argument, and it stands on this gear's own
    /// create semantics rather than on legacy's.
    ///
    /// It is worth saying what does **not** justify it, because an earlier version
    /// of this comment used exactly that reasoning: legacy's create path is an
    /// *upsert* that assigns `default_branch = $7` unconditionally, so there an
    /// absent value **clears** an existing row's override
    /// (`manager/src/services/platforms.rs:385-410`). That cannot be the reason
    /// two states suffice here, because **this gear's create can never reach an
    /// existing row** — it mints a fresh `Uuid::new_v4()` and a name collision
    /// becomes `EnvironmentNameExists` rather than an update
    /// (`environments_sea_repo.rs:66`, `:96-100`). Legacy's clear-on-create is
    /// unreachable in this port, so it justifies nothing.
    ///
    /// That unreachability is itself a divergence, and it is **pre-existing and
    /// deliberately left alone**: legacy `POST`ing an existing platform name with
    /// `default_branch` absent silently clears the stored override, where this gear
    /// returns a conflict. It predates this field, applies identically to
    /// `description` and `product_id`, and changing it would be a change to the
    /// gear's create contract rather than to this column. Recorded so the
    /// asymmetry with [`EnvironmentPatch::default_branch`] is not mistaken for an
    /// oversight.
    ///
    /// Whatever is supplied is trimmed, with blank normalised to `None`, by
    /// `EnvironmentsService` — as legacy's create normaliser also does
    /// (`platforms.rs:385-392`).
    pub default_branch: Option<String>,
    /// Make this environment its product's default. Defaults to `false`.
    ///
    /// Two states suffice for the same reason [`Self::default_branch`] gives: this
    /// struct only ever describes a row that does not exist yet, so "leave
    /// unchanged" has no referent. Creating an environment with `true` clears whatever
    /// environment previously held the flag for that product, which is the same
    /// service-side rule the patch path applies.
    pub is_default: bool,
}

/// Partial update; `None` = leave unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnvironmentPatch {
    pub name: Option<String>,
    /// Outer `None` = leave unchanged; `Some(None)` = clear the association;
    /// `Some(Some(id))` = set to `id`.
    /// `None` = leave the binding alone; `Some(id)` = rebind.
    ///
    /// **Two states since Task 20b, not three.** It was
    /// `Option<Option<Uuid>>` so a caller could *clear* the product; the
    /// column is `NOT NULL` now, so there is no cleared state to express.
    /// That also closes the route Critical C-1 was found through — clearing a
    /// plugin-managed row's product and then rotating a credential on it.
    pub product_id: Option<Uuid>,
    /// Outer `None` = leave unchanged; `Some(None)` = clear the description;
    /// `Some(Some(text))` = set to `text`.
    #[allow(
        clippy::option_option,
        reason = "tri-state patch field: unchanged vs. cleared vs. set"
    )]
    pub description: Option<Option<String>>,
    /// Replace the stored credstore reference with one the caller already
    /// holds. Mutually exclusive with [`Self::kubeconfig`] — supplying both is
    /// a validation error naming both fields.
    pub kubeconfig_credstore_ref: Option<String>,
    /// Replace the environment's kubeconfig by pasting a new **document**.
    ///
    /// `EnvironmentsService::update_environment` writes it to credstore first, then
    /// stores the new reference; the previous secret is removed afterwards
    /// **only when this gear generated it** (see the service for why a
    /// caller-supplied reference is left alone). See [`CredentialMaterial`].
    pub kubeconfig: Option<CredentialMaterial>,
    /// Credentials to replace, keyed by the plugin's own `FieldDesc::key`.
    ///
    /// **An absent key means "not mentioned" and supersedes nothing**; a
    /// present key replaces that credential, and the reference it displaces is
    /// forgotten only once the row carries the new one and only where this gear
    /// owned it. There is deliberately no "clear this credential" spelling: a
    /// required credential cannot be absent, and an optional one that should go
    /// away is a schema change in the plugin. See
    /// [`NewEnvironment::credentials`] for the classification rule.
    pub credentials: BTreeMap<String, CredentialSubmission>,
    pub available: Option<bool>,
    /// Outer `None` = leave unchanged; `Some(None)` = clear the override, so
    /// the repository default applies again; `Some(Some(branch))` = pin to
    /// `branch`. See [`Environment::default_branch`].
    ///
    /// # Why three states and not two
    ///
    /// The source system's patch path is genuinely three-state, via a
    /// `"__NULL__"` sentinel string: an **absent** field keeps the stored value
    /// (`WHEN $6 IS NULL THEN platforms_meta.default_branch`), an **empty or
    /// whitespace-only** one clears the column, and any other value sets the
    /// trimmed text (`manager/src/services/platforms.rs:447-496`). A plain
    /// `Option<String>` here would collapse "absent" and "explicitly emptied"
    /// into one `None` and drop the clear, so an operator who unpinned an
    /// environment would find the old branch still in force with no error and no
    /// warning — the same silent-divergence failure this field was added to
    /// fix. `Option<Option<String>>` is also already this struct's convention
    /// for `product_id` and `description`.
    ///
    /// [`NewEnvironment::default_branch`] needs only two states, for a reason of this
    /// gear's own — a row being created has no stored value to keep. See that
    /// field's doc, which also records why legacy's clear-on-create upsert is
    /// *not* the reason, being unreachable in this port.
    #[allow(
        clippy::option_option,
        reason = "tri-state patch field: unchanged vs. cleared vs. set"
    )]
    pub default_branch: Option<Option<String>>,
    /// `None` = leave unchanged; `Some(true)` = make this the product's default;
    /// `Some(false)` = clear the flag on this environment.
    ///
    /// Two states inside the `Option` and not three: the flag is a `bool` with no
    /// "unset" reading, so `Some(false)` already expresses the only clear there is.
    /// Contrast [`Self::default_branch`], where `Some(None)` is a genuine third
    /// state because the column is nullable.
    ///
    /// Setting `Some(true)` **clears the flag on the product's previous default**
    /// in the same transaction. An operator's gesture here is "make this one the
    /// default", not "fail because another already is".
    pub is_default: Option<bool>,
}

/// A name=value pair participating in run environment assembly.
///
/// `environment_id = None` → global pipeline variable;
/// `environment_id = Some(_)` → per-environment variable.
#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub id: Uuid,
    pub environment_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewVariable {
    pub environment_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

/// Environment-variable names the test runner owns. No pipeline or environment
/// variable may take one, compared **case-insensitively**.
///
/// Exactly the source system's `RESERVED_PIPELINE_VARIABLE_NAMES`
/// (`../testrunner/manager/src/routes/settings.rs:15-27`), name for name and in
/// the same order.
///
/// # Why this is enforced, and why it lives in the SDK
///
/// The source system rejects these names on **all three** environment write
/// paths — run parameters (`routes/settings.rs:153`), pipeline variables
/// (`:105-108`) and platform variables, which reach the same check indirectly
/// (`routes/platforms.rs:473` → `:505 merge_pipeline_variables` →
/// `routes/settings.rs:243` → `:108`) — all landing on one
/// `eq_ignore_ascii_case` sweep at `routes/settings.rs:80-88`.
///
/// That is not cosmetic. `RP_API_KEY` is supplied to the runner as a secret
/// **reference**, never a value (`manager/src/services/argo.rs:443-452`), and
/// the later environment tiers override earlier ones by name. A variable
/// literally named `RP_API_KEY` would therefore replace the reference with
/// operator-supplied text — which qa-runs' `RunExecutor` port
/// (`domain::ports::run_executor::RunEnv::new`) documents as its safety
/// premise. **Two gears must enforce one list for that premise to hold**:
/// qa-runs owns the run-parameter path, this gear owns the other two.
///
/// It lives in the SDK because that is the only place both can see it. qa-runs
/// depends on `qa-environments-sdk`; the reverse edge does not exist and must
/// not be created for a constant. Duplicating the list instead was rejected —
/// two lists that must stay identical, in different crates, with nothing
/// connecting them, is the drift hazard this subsystem has already found seven
/// of. qa-runs pins the pair with an equality assertion
/// (`the_two_reserved_name_lists_must_stay_identical`), which is the guard the
/// duplicate would have lacked.
///
/// **Added 2026-08-13** (qa-runs Task 11b, user decision) after Task 11's spec
/// review found this gear enforced charset and length only, leaving the
/// premise broken on two of the three paths.
pub const RESERVED_VARIABLE_NAMES: [&str; 11] = [
    "APP_BUILD",
    "APP_VERSION",
    "E2E_K8S_NAMESPACE",
    "KUBECONFIG",
    "PRODUCT_KEY",
    "RP_API_KEY",
    "RP_PROJECT",
    "SKIP_TESTS_WITH_BUGS",
    "TEST_BUNDLE_URL",
    "TEST_FILES",
    "TEST_VERSION",
];

/// How a run intends to hold an environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseMode {
    /// Co-exists with other parallel holders.
    Parallel,
    /// Sole holder; nothing else may run.
    Exclusive,
}

/// Current occupancy of an environment.
#[derive(Clone, Debug, PartialEq)]
pub enum LeaseState {
    Free,
    /// Held by one or more parallel runs.
    HeldParallel {
        holders: Vec<Uuid>,
    },
    /// Held exclusively by a single run.
    HeldExclusive {
        holder: Uuid,
    },
}

/// Result of a lease acquisition attempt.
#[derive(Clone, Debug, PartialEq)]
pub enum AcquireOutcome {
    /// The lease was granted; the run may start.
    Acquired,
    /// The environment is occupied in a conflicting mode; the caller should queue.
    Busy { current: LeaseState },
}
