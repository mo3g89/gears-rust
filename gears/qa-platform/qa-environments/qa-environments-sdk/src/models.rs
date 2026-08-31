//! Transport-agnostic models for the qa-environments contract.

use time::OffsetDateTime;
use uuid::Uuid;

/// A raw kubeconfig **document**, supplied by an operator who has one to paste
/// rather than a credstore reference to name.
///
/// # Why this is a newtype and not a `String`
///
/// A kubeconfig's `users[].user.client-key-data` is a client **private key** —
/// the same class of material as the SSH key `qa-catalog` stores
/// (`qa-catalog/src/domain/service/ssh_keys.rs`). It must reach credstore and
/// nothing else: no log line, no `#[instrument]` field, no error message, no
/// response body.
///
/// [`Debug`] is therefore hand-written to print `[REDACTED]`, mirroring
/// `qa-catalog`'s `CreateSshKeyReq` (`qa-catalog/src/api/rest/dto.rs`) and
/// credstore's own `CreateSecretRequestDto`. Because this type — not a bare
/// `String` — is what [`NewPlatform`] and [`PlatformPatch`] carry, **their**
/// derived `Debug` is redacted too, and so is that of anything holding one of
/// them. A `debug!("{new:?}")` added anywhere later cannot leak the document;
/// that is a property of the type rather than a convention someone has to
/// remember.
///
/// The material is never returned by any read path: `TargetPlatform` carries
/// only [`TargetPlatform::kubeconfig_credstore_ref`].
#[derive(Clone, PartialEq, Eq)]
pub struct KubeconfigMaterial(String);

impl KubeconfigMaterial {
    /// Wrap a raw kubeconfig document.
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

impl std::fmt::Debug for KubeconfigMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KubeconfigMaterial([REDACTED])")
    }
}

impl From<String> for KubeconfigMaterial {
    fn from(document: String) -> Self {
        Self(document)
    }
}

/// A registered target platform (system under test).
#[derive(Clone, Debug, PartialEq)]
pub struct TargetPlatform {
    pub id: Uuid,
    pub name: String,
    /// Optional association to a product in qa-catalog (by ID; no cross-gear FK).
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    /// Reference to the kubeconfig secret in credstore. Never the material itself.
    ///
    /// **In-process consumers only.** The REST projection `PlatformDto`
    /// deliberately drops this field (see its doc comment): under
    /// `SharingMode::Tenant` the reference *is* a read path to the material, so
    /// publishing it to every `qa.platform` GET/LIST-authorized caller would
    /// publish a pasted private key. `qa-runs` reads it from this model, not
    /// over REST.
    pub kubeconfig_credstore_ref: String,
    /// Operator-controlled availability toggle: an unavailable platform accepts no new leases.
    pub available: bool,
    /// Last product version observed on the platform (populated by the p2 version poller).
    pub observed_version: Option<String>,
    /// Last product **build** identifier observed on the platform, alongside
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
    /// Per-platform default branch **override**: the middle tier of qa-runs'
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
    /// (`manager/migrations/001_initial.sql:233`) — read by
    /// `PlatformsService::get_platform_default_branch`, whose own doc calls it
    /// *"the per-platform default branch override (falls back to repo
    /// default)"* (`manager/src/services/platforms.rs:845-848`). This gear
    /// shipped without it, so a launch that named no branch silently used the
    /// *repository's* default where the source system would have used the
    /// platform's — with no error and no warning for an operator who had
    /// pinned a platform to a release branch.
    ///
    /// Unlike [`Self::observed_version`] and [`Self::observed_build`], which are
    /// machine-written and therefore appear in no write DTO, this field is
    /// **operator-set**: it is present in [`NewPlatform`], [`PlatformPatch`] and
    /// the REST request types. Mirroring `observed_build`'s read-only shape here
    /// would have shipped a field nothing could set.
    pub default_branch: Option<String>,
    /// Whether this platform is its product's **default** — what the Run and
    /// Schedule dialogs' "Default cluster" option resolves to.
    ///
    /// # Why this exists, and why it is not the source system's behaviour
    ///
    /// In the source system "Default cluster" meant *no platform at all*, and the
    /// runner fell through to its own in-cluster `ServiceAccount`
    /// (`manager/src/routes/runs.rs` resolves the platform context to
    /// `(None, None, None, None)`; `manager/src/services/argo.rs` mounts a
    /// kubeconfig only `if let Some(platform_name)`). That worked because the
    /// source system's manager ran **inside** the cluster under test. qa-platform
    /// runs beside the clusters it tests, so that meaning is dead here — it
    /// produced runs that dispatched and then failed every test on missing
    /// credentials. **Decision (user, 2026-08-31): the option resolves to the
    /// product's default platform instead.**
    ///
    /// **At most one platform per (tenant, product) has this set.**
    /// `PlatformsService` enforces it by clearing the previous holder in the same
    /// transaction, rather than a database constraint — see
    /// `m20260831_000009_platform_is_default`'s module doc for why.
    ///
    /// Operator-set, like [`Self::default_branch`], so it appears in
    /// [`NewPlatform`], [`PlatformPatch`] and the REST request types.
    pub is_default: bool,
    /// The platform's base domain, prefixed `https://` (e.g.
    /// `https://sv.jele.io`), recovered by comparing the gateway's
    /// externally-visible hostnames. `None` means "never successfully
    /// detected" — **not** "no base URL" — so a conclusive detection
    /// overwrites it, while an inconclusive one leaves a previously stored
    /// value alone. Written by `PlatformsService::observe_platform`, via
    /// `PlatformsRepository::record_observation`'s `vhp_base_url` merge rule.
    pub vhp_base_url: Option<String>,
    /// The namespace the platform's core components were detected in.
    /// Unlike [`Self::vhp_base_url`] and [`Self::observed_version`]/
    /// [`Self::observed_build`], a value already stored here is **never**
    /// overwritten by a later detection — only a currently-`None` value is
    /// filled. Written by `PlatformsService::observe_platform`, via the same
    /// repository method.
    pub observed_namespace: Option<String>,
    /// The message from the most recent **failed** observation attempt, or
    /// `None` if the most recent attempt succeeded (or none has run yet).
    /// Cleared back to `None` by the next successful observation. This is a
    /// **value**, not this gear's own error: a platform whose cluster cannot
    /// be reached is a fact about the platform, and this field is what lets
    /// an operator read why (`POST /qa/v1/platforms/{id}/refresh` returns
    /// HTTP 200 with this field populated, never a 5xx, when only detection
    /// itself failed).
    pub version_detect_error: Option<String>,
    /// When the most recent observation attempt — success or failure — ran.
    /// `None` means no attempt has ever completed.
    pub version_detected_at: Option<OffsetDateTime>,
    /// This platform's most recent cluster-health reading.
    ///
    /// `None` means no observation cycle has ever reached this platform yet —
    /// a fact distinct from a `Some` carrying `status: "Unreachable"`, which
    /// means a cycle reached it and could not read the cluster. See
    /// [`ClusterHealthView`]'s own doc for why collapsing those two into one
    /// `None` (the failure mode a flat set of nullable fields would have)
    /// would make the UI unable to tell them apart.
    pub cluster: Option<ClusterHealthView>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// One node, as the platform's own cluster reported it in its most recent
/// successful health read.
///
/// Field-for-field the same shape as `qa-environments`' own
/// `domain::observation::NodeSummary`, kept as a distinct type here because
/// this crate's models carry no dependency on that gear's domain module —
/// this is the transport-agnostic contract layer, and the gear crate is what
/// converts between the two (`infra::storage::mapper`).
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

/// One platform's cluster-health reading, as of [`Self::checked_at`].
///
/// # Why this is one optional field on [`TargetPlatform`], not several
///
/// `TargetPlatform::cluster` being `None` means "no cycle has reached this
/// platform"; `Some(_)` here, even with `status == "Unreachable"`, means one
/// has. A set of flat nullable fields (`cluster_status: Option<String>` and
/// so on, each independently `None`) cannot represent that distinction: both
/// "never checked" and "checked, unreachable, nothing to show" would read as
/// all-`None`. One optional struct keeps the three states — never checked,
/// checked-and-failed, checked-and-read — distinguishable at the type level.
#[derive(Clone, Debug, PartialEq)]
pub struct ClusterHealthView {
    /// `"Healthy"`, `"Degraded"`, `"Unhealthy"`, `"Warning"` for a successful
    /// read, or `"Unreachable"` for a read that failed. `"Unreachable"` is
    /// this SDK's own name for that outcome — the gear's internal
    /// `ClusterStatus` domain type deliberately has no such variant, because
    /// unreachable is the absence of a reading rather than a property of one
    /// that succeeded.
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

/// Creation request for a target platform.
///
/// The kubeconfig arrives one of two ways — a credstore reference the caller
/// already holds ([`Self::kubeconfig_credstore_ref`]) or a pasted document
/// ([`Self::kubeconfig`]) — and exactly one of them must be present. The
/// derived `Debug` is safe because the document is wrapped in
/// [`KubeconfigMaterial`], whose own `Debug` redacts.
#[derive(Clone, Debug, PartialEq)]
pub struct NewPlatform {
    pub name: String,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    /// A credstore reference the caller already holds.
    ///
    /// **Exactly one of this and [`Self::kubeconfig`] must be supplied.**
    /// `PlatformsService::create_platform` enforces that: both is a validation
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
    /// or any response. See [`KubeconfigMaterial`] for why it is not a
    /// `String`.
    pub kubeconfig: Option<KubeconfigMaterial>,
    /// Optional per-platform default branch override; see
    /// [`TargetPlatform::default_branch`].
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
    /// becomes `PlatformNameExists` rather than an update
    /// (`platforms_sea_repo.rs:66`, `:96-100`). Legacy's clear-on-create is
    /// unreachable in this port, so it justifies nothing.
    ///
    /// That unreachability is itself a divergence, and it is **pre-existing and
    /// deliberately left alone**: legacy `POST`ing an existing platform name with
    /// `default_branch` absent silently clears the stored override, where this gear
    /// returns a conflict. It predates this field, applies identically to
    /// `description` and `product_id`, and changing it would be a change to the
    /// gear's create contract rather than to this column. Recorded so the
    /// asymmetry with [`PlatformPatch::default_branch`] is not mistaken for an
    /// oversight.
    ///
    /// Whatever is supplied is trimmed, with blank normalised to `None`, by
    /// `PlatformsService` — as legacy's create normaliser also does
    /// (`platforms.rs:385-392`).
    pub default_branch: Option<String>,
    /// Make this platform its product's default. Defaults to `false`.
    ///
    /// Two states suffice for the same reason [`Self::default_branch`] gives: this
    /// struct only ever describes a row that does not exist yet, so "leave
    /// unchanged" has no referent. Creating a platform with `true` clears whatever
    /// platform previously held the flag for that product, which is the same
    /// service-side rule the patch path applies.
    pub is_default: bool,
}

/// Partial update; `None` = leave unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlatformPatch {
    pub name: Option<String>,
    /// Outer `None` = leave unchanged; `Some(None)` = clear the association;
    /// `Some(Some(id))` = set to `id`.
    #[allow(
        clippy::option_option,
        reason = "tri-state patch field: unchanged vs. cleared vs. set"
    )]
    pub product_id: Option<Option<Uuid>>,
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
    /// Replace the platform's kubeconfig by pasting a new **document**.
    ///
    /// `PlatformsService::update_platform` writes it to credstore first, then
    /// stores the new reference; the previous secret is removed afterwards
    /// **only when this gear generated it** (see the service for why a
    /// caller-supplied reference is left alone). See [`KubeconfigMaterial`].
    pub kubeconfig: Option<KubeconfigMaterial>,
    pub available: Option<bool>,
    /// Outer `None` = leave unchanged; `Some(None)` = clear the override, so
    /// the repository default applies again; `Some(Some(branch))` = pin to
    /// `branch`. See [`TargetPlatform::default_branch`].
    ///
    /// # Why three states and not two
    ///
    /// The source system's patch path is genuinely three-state, via a
    /// `"__NULL__"` sentinel string: an **absent** field keeps the stored value
    /// (`WHEN $6 IS NULL THEN platforms_meta.default_branch`), an **empty or
    /// whitespace-only** one clears the column, and any other value sets the
    /// trimmed text (`manager/src/services/platforms.rs:447-496`). A plain
    /// `Option<String>` here would collapse "absent" and "explicitly emptied"
    /// into one `None` and drop the clear, so an operator who unpinned a
    /// platform would find the old branch still in force with no error and no
    /// warning — the same silent-divergence failure this field was added to
    /// fix. `Option<Option<String>>` is also already this struct's convention
    /// for `product_id` and `description`.
    ///
    /// [`NewPlatform::default_branch`] needs only two states, for a reason of this
    /// gear's own — a row being created has no stored value to keep. See that
    /// field's doc, which also records why legacy's clear-on-create upsert is
    /// *not* the reason, being unreachable in this port.
    #[allow(
        clippy::option_option,
        reason = "tri-state patch field: unchanged vs. cleared vs. set"
    )]
    pub default_branch: Option<Option<String>>,
    /// `None` = leave unchanged; `Some(true)` = make this the product's default;
    /// `Some(false)` = clear the flag on this platform.
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
/// `platform_id = None` → global pipeline variable;
/// `platform_id = Some(_)` → per-platform variable.
#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub id: Uuid,
    pub platform_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewVariable {
    pub platform_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

/// Environment-variable names the test runner owns. No pipeline or platform
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

/// How a run intends to hold a platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseMode {
    /// Co-exists with other parallel holders.
    Parallel,
    /// Sole holder; nothing else may run.
    Exclusive,
}

/// Current occupancy of a platform.
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
    /// The platform is occupied in a conflicting mode; the caller should queue.
    Busy { current: LeaseState },
}
