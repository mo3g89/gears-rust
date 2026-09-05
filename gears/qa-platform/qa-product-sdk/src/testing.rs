//! The leak-conformance harness every QA Platform product plugin must run in
//! its own test suite (`Task 9` and `Task 10` do).
//!
//! # Why this exists
//!
//! On 2026-08-28 this subsystem had a measured leak: someone pasted a PEM
//! private key into a kubeconfig field, a serde error quoted the whole
//! offending scalar — and for a document that *is* one scalar, the offending
//! scalar is the whole document — and the key was persisted to a database
//! column, published on a DTO, and rendered on a user-facing page.
//! [`crate::observation::PluginFailure::detail`] being `Option<&'static str>`
//! is Layer 1, and [`crate::descriptor::validate_schemas`] is Layer 2 — both
//! guard the platform's own code. A product plugin is now third-party code
//! this team does not review line by line, so neither layer helps if a
//! plugin author writes `format!("{upstream_error}")` somewhere this crate
//! cannot see. [`assert_no_leak`] is Layer 3: it drives a plugin with planted
//! credential material and fails the build if that material reaches any
//! surface the plugin can return or emit.
//!
//! This generalises `postgres-credstore-plugin`'s
//! `src/infra/storage/leak_tests.rs` and `tests/sea_orm_trace_exposure.rs` —
//! the same idea (plant a sentinel, exercise every path, assert its absence
//! from every rendering), widened from one storage crate's log lines to
//! every surface a [`crate::plugin::QaProductPluginV1`] implementation can
//! put a `String` on.
//!
//! Layer 3 also *enforces* Layer 2: [`assert_no_leak`] calls
//! [`crate::descriptor::validate_schemas`] itself on the two schemas it
//! already has to fetch, so a plugin whose test suite runs this harness
//! cannot ship a schema that would fail at registration.
//!
//! # The plant is derived from what the plugin declared
//!
//! Credential material is planted under **the plugin's own
//! `credential_schema()` keys**, not under three fixed names. The first
//! version of this harness planted under `"pem"`/`"token"`/`"password"` only,
//! and that made it assert vacuously for any real plugin: VHP declares
//! `kubeconfig` and `vpadm_namespace`, so it would look up `"kubeconfig"`,
//! find nothing, return on its first line, and the harness would then assert
//! that three markers are absent from surfaces they never reached. Green, and
//! meaningless. The three fixed keys survive as *extras* so a plugin
//! declaring no credential schema at all is still probed.
//!
//! The `config` handed to the plugin is synthesised from the declared
//! **non-secret** fields for the same reason: a plugin that reads
//! `config["vpadm_namespace"]` and bails when it is absent is never actually
//! driven by a `Value::Null` configuration.
//!
//! A declared non-secret field is planted with a distinct per-key filler
//! rather than a canary marker, and that filler is *not* asserted absent. A
//! `Text` credential field is, by its own declaration, not credential
//! material — the platform renders it — and a plugin legitimately echoes one
//! into its observed attributes (VHP's `namespace` is exactly that). Asserting
//! its absence would fail a plugin for doing the right thing.
//!
//! # Why `remote_message` gets no special exemption
//!
//! [`crate::observation::PluginFailure::remote_message`] is the one `String`
//! that legitimately crosses the plugin boundary — the sanctioned exception
//! to "nothing formatted crosses this boundary" (see its own doc comment).
//! That makes it the one hole a careless plugin author will actually use: it
//! is *designed* to carry text the plugin received from elsewhere, so
//! echoing credential material into it looks, to a hurried author, exactly
//! like the feature working as intended. This harness checks every returned
//! `remote_message` on every driven call — including `health_check`'s, which
//! is easy to forget since its signature carries no credential material at
//! all, but which shares the same `&dyn QaProductPluginV1` object (and so,
//! for a stateful plugin, the same cached credential) as every other method.
//!
//! # A documented blind spot: coverage is per *driven path*, not per method
//!
//! This harness calls each method **once**, with one well-formed,
//! happy-path-shaped plant: every declared credential present, every declared
//! non-secret field present in `config`, nothing truncated, nothing invalid.
//! `prepare_run_access` is the one exception — it is driven twice, once with
//! resolved credentials and once from references alone. The reference-only
//! drive is the contractual one (see
//! [`crate::plugin::QaProductPluginV1::prepare_run_access`]) and must return
//! `Ok`; the resolved drive both harvests leak surfaces from a plugin that
//! cached plaintext during `observe` and echoes it later — the same reason
//! `health_check` is driven despite its signature carrying no credential
//! material — and gives the two-drive comparison something to compare, since
//! an access that changes when plaintext is available was built from it.
//!
//! What that leaves uncovered is exactly where the 2026-08-28 leak lived: an
//! **error branch**. The key did not escape through a successful parse, it
//! escaped through a `serde` failure that quoted the document it had just
//! failed to read. A plugin whose happy path is spotless and whose
//! `Err(_) => format!("{e}")` arm echoes the credential passes this harness
//! and leaks in production, because this harness never gives it a malformed
//! input to fail on.
//!
//! Recorded with the same honesty as the `set_default` blind spot below,
//! rather than left for a future reader to discover the hard way. Two things
//! narrow the gap and neither closes it: Layer 1 makes the *classified* half
//! of a failure structurally incapable of carrying runtime bytes, and a
//! plugin's own tests are expected to drive its parse-failure paths directly.
//! **Tasks 9 and 10:** a plugin whose parsing can fail should call
//! `assert_no_leak` and additionally assert, in its own unit tests, that its
//! error branches classify rather than format.
//!
//! # A documented blind spot: process-global level gates
//!
//! The capture below installs its subscriber with
//! `tracing::subscriber::set_default` — a **thread-local** default, not
//! `tracing::subscriber::set_global_default`. That is a deliberate trade-off,
//! not an oversight: [`assert_no_leak`] is called from many independent test
//! functions in the same test binary, and a global subscriber can be
//! installed at most once per process — the second call would leave every
//! later call capturing nothing, silently.
//!
//! The cost of that choice, recorded here with the same discipline
//! `postgres-credstore-plugin`'s `leak_tests.rs` uses for its own
//! `CAPTURE_LEVEL`, rather than left for a future reader to rediscover: some
//! libraries decide whether to emit a `tracing` event at all by checking the
//! **process-global** `tracing::level_filters::LevelFilter::current()` — a
//! value only `set_global_default` ever raises. `sqlx`'s statement logging is
//! the concrete, independently-verified example (`leak_tests.rs`'s own
//! `CAPTURE_LEVEL` note, and `sea_orm_trace_exposure.rs` in that same crate).
//! Under this harness's thread-local default, such a library's events never
//! fire, so they are silently absent from the capture here even though the
//! subscriber is "installed" for the whole call. A plugin built on a client
//! library with this behaviour — a real Kubernetes or HTTP client is exactly
//! the shape to worry about — could log a credential through such a gated
//! call site and this harness would report clean. **Tasks 9 and 10:** this
//! harness's `tracing` coverage is real for ordinary
//! `tracing::info!`/`warn!`/etc. call sites in the plugin's own code, but is
//! not a substitute for auditing a vendored client library's own logging
//! decisions.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use credstore_sdk::SecretValue;

use crate::access::{MountSpec, RunAccess, RunVarContract, RunnerSpec};
use crate::descriptor::{FieldDesc, FieldKind, validate_schemas};
use crate::observation::{HealthOutcome, ObservationOutcome, PluginFailure};
use crate::plugin::{CredentialInput, CredentialSlot, EnvironmentHandle, QaProductPluginV1};

/// Planted credential material, in the three shapes that leak differently —
/// a real key, a bearer token, and a password each reach a log line or a
/// rendered page through a different code path, so one marker is not enough
/// to cover all three.
#[derive(Clone, Debug)]
pub struct Canary {
    /// A real PEM block. This is the exact shape that leaked on 2026-08-28:
    /// serde quotes the offending scalar, and for a document that is
    /// entirely one scalar the offending scalar is the whole document.
    ///
    /// Planted under every [`FieldKind::MultilineSecret`] field the plugin
    /// declares.
    pub pem: String,
    /// A bearer token — the SaaS-plugin shape. Planted under every
    /// [`FieldKind::Secret`] field the plugin declares.
    pub token: String,
    /// A password — the appliance/admin-API shape. Planted under the fixed
    /// `password` extra key, so it probes a plugin that reads a credential
    /// this harness could not have learned about from the schema.
    pub password: String,
}

impl Canary {
    /// Shaped after VHP's own credential fields: a kubeconfig (carrying a
    /// client-certificate PEM block), a bearer token, and an admin
    /// password — the three shapes a real product plugin's credential form
    /// actually collects.
    #[must_use]
    pub fn vhp_shaped() -> Self {
        Self {
            pem: concat!(
                "-----BEGIN PRIVATE KEY-----\n",
                "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj\n",
                "CANARYKEYMATERIALTHISISNOTAREALPRIVATEKEYFORTESTINGONLYDONOTUSE\n",
                "-----END PRIVATE KEY-----\n",
            )
            .to_owned(),
            token: "Bearer canary-8f3d1a9c7b2e4056a9d3f7c1e8b2a406".to_owned(),
            password: "canary-Sup3r-Secret-P@ssphrase!".to_owned(),
        }
    }

    /// The three markers, each tagged with the name a panic message reports.
    fn markers(&self) -> [(&'static str, &str); 3] {
        [
            ("pem", self.pem.as_str()),
            ("token", self.token.as_str()),
            ("password", self.password.as_str()),
        ]
    }

    /// The marker a declared field of this `kind` is planted with, or `None`
    /// for a non-secret field — which gets a per-key filler instead, for the
    /// reason given in this module's header.
    fn marker_for(&self, kind: FieldKind) -> Option<&str> {
        match kind {
            FieldKind::MultilineSecret => Some(self.pem.as_str()),
            FieldKind::Secret => Some(self.token.as_str()),
            FieldKind::Text
            | FieldKind::Url
            | FieldKind::Int
            | FieldKind::Bool
            | FieldKind::Enum => None,
        }
    }
}

/// A `Write` sink appending to a shared buffer — the same shape
/// `postgres-credstore-plugin`'s leak tests use to capture `tracing` output
/// for inspection instead of letting it go to a real sink.
#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The only place this module renders a `Debug` representation, because
/// that rendering is exactly the surface under test — the same reasoning
/// `PluginFailure`'s own `Display` impl uses at its one unavoidable
/// `{:?}` callsite.
#[allow(clippy::use_debug)]
fn debug_of<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
}

fn captured(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    let bytes = buf.lock().unwrap_or_else(PoisonError::into_inner);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A marker's escaped rendering: the form `serde_json::to_string` and
/// `Debug` both produce for a string containing it — `\n`, `\"`, `\\`, and
/// so on for whatever control characters the marker carries (the PEM
/// marker's embedded newlines, most notably).
///
/// Checking only the raw marker misses a leak by construction: JSON and
/// `Debug` both escape control characters, so a PEM block that reaches
/// `ObservedAttrs`' serialised form never appears there as the literal bytes
/// [`Canary::pem`] holds — it appears with every `\n` replaced by the
/// two-character escape `\n`. This is the general form of the 2026-08-28
/// leak itself: what a serde error quotes is an *escaped* rendering of the
/// offending scalar, not its raw bytes.
#[allow(clippy::use_debug)]
fn escaped_variant(marker: &str) -> String {
    format!("{marker:?}").trim_matches('"').to_owned()
}

/// Every rendering of `marker` this harness checks, labelled by encoding.
///
/// Mirrors `postgres-credstore-plugin`'s `needles()` — and exists for the
/// same reason that function checks three encodings instead of one: its own
/// first version checked only raw text and missed a real leak that reached a
/// log line as `Debug`'s decimal byte-array rendering of a `Vec<u8>`.
/// [`crate::plugin::CredentialSlot::value`] is a `SecretValue`, which *is*
/// bytes — a plugin calling `.as_bytes()` and `format!("{:?}", bytes)` in a
/// parse-failure path (routine for PEM/certificate parsing) reaches this
/// encoding specifically, and the raw and escaped-string checks above do not
/// see it: the decimal digits of a byte array share no substring with the
/// marker's own text.
#[allow(clippy::use_debug)]
fn marker_variants(marker: &str) -> [(&'static str, String); 5] {
    let bytes = marker.as_bytes();
    let (hex_lower, hex_upper) = hex_renderings(bytes);
    [
        ("raw", marker.to_owned()),
        ("escaped", escaped_variant(marker)),
        ("hex, lowercase", hex_lower),
        ("hex, uppercase", hex_upper),
        ("decimal byte array", format!("{bytes:?}")),
    ]
}

/// Lowercase and uppercase hex renderings of `bytes`, in one pass.
///
/// `write!` into a `String` cannot fail — there is no I/O underneath it —
/// so the `fmt::Result` it returns is discarded with `.ok()` rather than
/// `.unwrap()`/`.expect()`, both denied in this workspace's lint config.
fn hex_renderings(bytes: &[u8]) -> (String, String) {
    use std::fmt::Write as _;
    let mut lower = String::with_capacity(bytes.len() * 2);
    let mut upper = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(lower, "{byte:02x}").ok();
        write!(upper, "{byte:02X}").ok();
    }
    (lower, upper)
}

/// Collapse every run of whitespace (including the newlines a PEM block is
/// full of) to a single space, and drop leading/trailing whitespace.
///
/// Checking only byte-identical markers is defeated by reformatting that
/// leaves the leaked content just as sensitive: trimming a trailing newline,
/// rewrapping a PEM body at a different column width, or collapsing its
/// newlines to spaces all change the bytes without changing what was
/// leaked. Applying this to *both* sides before comparing catches all three,
/// because rewrapped text differs from the original only in *which*
/// whitespace run separates two tokens, never in the tokens themselves.
fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One string a plugin put on a surface, plus whether that string may be
/// reproduced in a failure message.
///
/// # Why the second field exists
///
/// A panic message that quotes the offending text is what makes a leak
/// report actionable: `RunAccess.env RunVar.value` says *where*, and the text
/// says *what* — a path, a name, a rendered `Debug`. Exactly one surface this
/// harness scans is credential plaintext by construction rather than by
/// accident: [`MountSpec::ConfigValue`]'s value, which a plugin resolved
/// itself. Quoting that would put the very bytes this crate exists to contain
/// into a test log — the 2026-08-28 leak reproduced by the tool built to
/// catch it.
///
/// So a surface carries its own answer: text read out of a
/// [`SecretValue`] is compared but never rendered, and its report names the
/// surface and the marker instead. Naming both is enough to act on — the
/// author knows which mount leaked and which credential reached it — and the
/// value adds nothing a reader is allowed to see.
struct Surface {
    label: String,
    text: String,
    /// `true` when [`Self::text`] was read out of a [`SecretValue`], and so
    /// must never reach a message, a log line or a `Debug` rendering.
    from_secret: bool,
}

/// What a failure message prints in place of a secret-derived surface's text.
const REDACTED_SURFACE: &str = "<redacted: this surface's text is credential plaintext>";

impl Surface {
    /// A surface whose text a failure message may quote.
    fn visible(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            text: text.into(),
            from_secret: false,
        }
    }

    /// A surface whose text came out of a [`SecretValue`]: scanned, never
    /// rendered.
    fn from_secret(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            text: text.into(),
            from_secret: true,
        }
    }

    /// The text a failure message may show for this surface.
    fn quotable(&self) -> &str {
        if self.from_secret {
            REDACTED_SURFACE
        } else {
            &self.text
        }
    }
}

/// What a plugin is driven with: a submitted credential form, the credential
/// slots an environment would hold, and the environment's non-secret
/// configuration — all three derived from the plugin's own
/// `credential_schema()` so a plugin that looks a declared key up actually
/// finds it.
struct Plant {
    input: CredentialInput,
    slots: Vec<CredentialSlot>,
    config: serde_json::Value,
}

/// The value planted under a declared non-secret field: distinct per key, so
/// a plugin echoing the wrong one is still visible in a failure message, and
/// recognisable at a glance as harness-supplied.
fn filler_for(key: &str) -> String {
    format!("canary-field-{key}")
}

/// The credstore reference a planted slot carries. `prepare_run_access` is
/// contractually required to work from these alone, so they must be present
/// and plausible even for a slot this harness also resolved.
fn credstore_ref_for(key: &str) -> String {
    format!("qa/canary/{key}")
}

/// Build the plant from the plugin's declared credential schema.
///
/// Every declared field is planted under its own [`FieldDesc::key`] in *both*
/// directions — the submitted [`CredentialInput`] and the environment's
/// [`CredentialSlot`]s — because a plugin reads a credential from one on the
/// validation path and from the other on the observe/dispatch path, and a
/// harness that plants in only one of them proves only half the contract.
///
/// The three fixed keys (`pem`, `token`, `password`) are added as extras
/// unless the schema already claimed them, so a plugin declaring nothing is
/// still driven with credential material.
fn plant(schema: &[FieldDesc], canary: &Canary) -> Plant {
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    let mut config = serde_json::Map::new();

    for field in schema {
        let value = if let Some(marker) = canary.marker_for(field.kind) {
            marker.to_owned()
        } else {
            let filler = filler_for(&field.key);
            config.insert(field.key.clone(), serde_json::Value::String(filler.clone()));
            filler
        };
        values.insert(field.key.clone(), value);
    }

    for (key, marker) in canary.markers() {
        values
            .entry(key.to_owned())
            .or_insert_with(|| marker.to_owned());
    }

    let slots = values
        .iter()
        .map(|(key, value)| {
            CredentialSlot::resolved(
                key.clone(),
                credstore_ref_for(key),
                SecretValue::from(value.clone()),
            )
        })
        .collect();

    let input = CredentialInput {
        fields: values
            .into_iter()
            .map(|(key, value)| (key, SecretValue::from(value)))
            .collect(),
    };

    Plant {
        input,
        slots,
        config: serde_json::Value::Object(config),
    }
}

/// The same slots with nothing resolved — the shape `qa-runs`' dispatch
/// passes, and the only shape
/// [`crate::plugin::QaProductPluginV1::prepare_run_access`] may require.
fn reference_only(slots: &[CredentialSlot]) -> Vec<CredentialSlot> {
    slots
        .iter()
        .map(|slot| CredentialSlot::reference_only(slot.key.clone(), slot.credstore_ref.clone()))
        .collect()
}

fn push_field_descs(surfaces: &mut Vec<Surface>, schema: &str, fields: &[FieldDesc]) {
    for field in fields {
        surfaces.push(Surface::visible(
            format!("{schema} FieldDesc.key"),
            field.key.clone(),
        ));
        surfaces.push(Surface::visible(
            format!("{schema} FieldDesc.label"),
            field.label.clone(),
        ));
        if let Some(help) = &field.help {
            surfaces.push(Surface::visible(
                format!("{schema} FieldDesc.help"),
                help.clone(),
            ));
        }
    }
}

/// Pushed most-specific-first: [`PluginFailure::remote_message`] is checked
/// before the failure's own `Debug` rendering, so a leak that lands in the
/// one `String` sanctioned to cross the boundary is named for what it is,
/// rather than being reported against the coarser aggregate that happens to
/// embed it too.
fn push_failure(surfaces: &mut Vec<Surface>, ctx: &str, failure: &PluginFailure) {
    if let Some(remote) = &failure.remote_message {
        surfaces.push(Surface::visible(
            format!("{ctx} PluginFailure.remote_message"),
            remote.clone(),
        ));
    }
    surfaces.push(Surface::visible(
        format!("{ctx} PluginFailure Debug"),
        debug_of(failure),
    ));
}

fn push_run_access(surfaces: &mut Vec<Surface>, ctx: &str, access: &RunAccess) {
    for var in &access.env {
        surfaces.push(Surface::visible(
            format!("{ctx}.env RunVar.name"),
            var.name.clone(),
        ));
        surfaces.push(Surface::visible(
            format!("{ctx}.env RunVar.value"),
            var.value.clone(),
        ));
    }
    for mount in &access.mounts {
        match mount {
            MountSpec::Secret {
                credstore_ref,
                path,
                ..
            } => {
                surfaces.push(Surface::visible(
                    format!("{ctx}.mounts MountSpec::Secret.credstore_ref"),
                    credstore_ref.clone(),
                ));
                surfaces.push(Surface::visible(
                    format!("{ctx}.mounts MountSpec::Secret.path"),
                    path.clone(),
                ));
            }
            MountSpec::ConfigValue { value, path, .. } => {
                surfaces.push(Surface::visible(
                    format!("{ctx}.mounts MountSpec::ConfigValue.path"),
                    path.clone(),
                ));
                // The value itself, not only where it lands. A `ConfigValue`
                // is the sanctioned carrier for a credential the plugin
                // resolved on its own — which makes it the one mount a
                // careless plugin fills with the plaintext it just read, and
                // it reaches a container's filesystem exactly as a leak
                // through any other surface reaches a page. `MountSpec`'s
                // `Debug` redacts it, so the aggregate rendering this harness
                // also collects cannot see it: without this push the scan is
                // blind here by construction.
                //
                // Read through `SecretValue::as_bytes` and pushed as a
                // secret-derived surface, so it is compared and never
                // rendered.
                surfaces.push(Surface::from_secret(
                    format!("{ctx}.mounts MountSpec::ConfigValue.value"),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                ));
            }
        }
    }
    if let Some(service_account) = &access.service_account {
        surfaces.push(Surface::visible(
            format!("{ctx}.service_account"),
            service_account.clone(),
        ));
    }
}

fn push_runner_spec(surfaces: &mut Vec<Surface>, runner: &RunnerSpec) {
    if let Some(image) = &runner.image {
        surfaces.push(Surface::visible("RunnerSpec.image", image.clone()));
    }
    for arg in &runner.command {
        surfaces.push(Surface::visible("RunnerSpec.command", arg.clone()));
    }
    if let Some(policy) = &runner.image_pull_policy {
        surfaces.push(Surface::visible(
            "RunnerSpec.image_pull_policy",
            policy.clone(),
        ));
    }
}

fn push_run_var_contract(surfaces: &mut Vec<Surface>, contract: &RunVarContract) {
    for name in &contract.reserved {
        surfaces.push(Surface::visible("RunVarContract.reserved", name.clone()));
    }
}

/// What a divergence report prints in place of **either** side's value.
///
/// Both sides, not just the resolved one. The tempting argument for quoting
/// the reference-only side — that drive was handed no plaintext, so its value
/// cannot be derived from any — is false: the *drive* was handed none, but the
/// *plugin* was, twice, by `observe` and by the resolved
/// `prepare_run_access` earlier in the same [`drive`] against the same
/// `&dyn` object. A plugin that caches (the shape this harness reasons about
/// wherever it drives a method whose signature carries no credential) can put
/// a plaintext-derived value on either side.
///
/// What stands in front of that is the canary scan, which runs first and is
/// transform-blind past the encodings in [`marker_variants`]: a base64,
/// truncated, reversed or hashed derivation reaches this message unseen. So
/// the message names the field and says it differs, which is actionable on its
/// own — an author who needs the values runs their own plugin under a
/// debugger, where no test log is involved.
const REDACTED_DIVERGING_VALUE: &str =
    "<redacted: a diverging value may have been built from plaintext>";

/// One mount reduced to what the two drives of `prepare_run_access` must
/// agree on.
///
/// Deliberately carries no `SecretValue` bytes: a `ConfigValue`'s value is
/// scanned against the canary by [`push_run_access`] and is never compared
/// here, so a divergence can be reported in full without printing anything a
/// reader may not see. What remains — the variant, where it lands, its mode,
/// and the reference a `Secret` names — is the part a run's behaviour depends
/// on.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct MountShape {
    variant: &'static str,
    path: String,
    mode: Option<i32>,
    credstore_ref: Option<String>,
}

impl MountShape {
    fn of(mount: &MountSpec) -> Self {
        match mount {
            MountSpec::Secret {
                credstore_ref,
                path,
                mode,
            } => Self {
                variant: "MountSpec::Secret",
                path: path.clone(),
                mode: *mode,
                credstore_ref: Some(credstore_ref.clone()),
            },
            MountSpec::ConfigValue { path, mode, .. } => Self {
                variant: "MountSpec::ConfigValue",
                path: path.clone(),
                mode: *mode,
                credstore_ref: None,
            },
        }
    }

    fn describe(&self) -> String {
        let Self {
            variant,
            path,
            mode,
            credstore_ref,
        } = self;
        // Octal: a mount mode is written `0o400` everywhere it is set, and
        // the decimal `256` a plain `to_string` gives reads as a different
        // number to whoever has to match it against their own code.
        let mode = mode.map_or_else(|| "none".to_owned(), |mode| format!("{mode:#o}"));
        match credstore_ref {
            Some(reference) => {
                format!("{variant} {{ path: {path}, mode: {mode}, credstore_ref: {reference} }}")
            }
            None => format!("{variant} {{ path: {path}, mode: {mode} }}"),
        }
    }
}

/// A plugin's run variables keyed by name, so the two drives are compared as
/// sets rather than positionally: nothing in the contract fixes the order
/// `RunAccess::env` is built in, and Task 18 injects these into a pod spec by
/// name. A name emitted twice keeps both values, sorted, rather than
/// collapsing to the last one.
fn env_by_name(access: &RunAccess) -> BTreeMap<&str, Vec<&str>> {
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for var in &access.env {
        out.entry(var.name.as_str()).or_default().push(&var.value);
    }
    for values in out.values_mut() {
        values.sort_unstable();
    }
    out
}

/// Mount shapes, sorted, so mounts are compared as a multiset for the same
/// reason run variables are: order is not part of the contract.
fn mount_shapes(access: &RunAccess) -> Vec<MountShape> {
    let mut shapes: Vec<MountShape> = access.mounts.iter().map(MountShape::of).collect();
    shapes.sort_unstable();
    shapes
}

/// Every way the access built with resolved plaintext differs from the access
/// built from credstore references alone.
///
/// # Why a difference is a violation and not a preference
///
/// [`crate::plugin::QaProductPluginV1::prepare_run_access`] must work from
/// `credstore_ref` alone. A plugin whose *output* changes according to whether
/// plaintext happened to be available has therefore read the plaintext: every
/// other input to the call — the configuration, the observation, the
/// references themselves — is identical across the two drives, so nothing
/// else could have moved. This is the checkable form of the rule already on
/// the books, not a second rule: without it, a plugin that prefers plaintext
/// and degrades quietly (a dropped mount, an empty env, a placeholder value)
/// returns `Ok` from both drives and passes.
///
/// The claim is about the harness's *inputs*, which are verifiably identical
/// across the two drives: the same `config`, `observed: None` both times, and
/// slots differing only in whether [`crate::plugin::CredentialSlot::value`] is
/// populated. It is not a claim about the plugin, which is one object called
/// twice: a `prepare_run_access` that returns a nonce, a timestamp or a
/// generated run id diverges without having read anything. That plugin is
/// reported here, and correctly so for a different reason — dispatch must be
/// reproducible, so a nondeterministic `prepare_run_access` is a defect in its
/// own right.
///
/// `ConfigValue` bytes are not compared. They are scanned against the canary
/// by [`push_run_access`], and a divergence report must never print them.
/// Neither may a diverging value of any other kind: see
/// [`REDACTED_DIVERGING_VALUE`].
fn access_divergences(resolved: &RunAccess, refs_only: &RunAccess) -> Vec<String> {
    let mut found = Vec::new();

    let (resolved_env, refs_env) = (env_by_name(resolved), env_by_name(refs_only));
    for (name, values) in &resolved_env {
        match refs_env.get(name) {
            None => found.push(format!(
                "RunVar `{name}` is returned only when resolved plaintext is available"
            )),
            Some(refs_values) if refs_values != values => found.push(format!(
                "RunVar `{name}` has a different value in each drive \
                 {REDACTED_DIVERGING_VALUE}"
            )),
            Some(_) => {}
        }
    }
    for name in refs_env.keys() {
        if !resolved_env.contains_key(name) {
            found.push(format!(
                "RunVar `{name}` is returned only when resolved plaintext is absent"
            ));
        }
    }

    let (resolved_mounts, refs_mounts) = (mount_shapes(resolved), mount_shapes(refs_only));
    if resolved_mounts != refs_mounts {
        for mount in &resolved_mounts {
            if !refs_mounts.contains(mount) {
                found.push(format!(
                    "mount `{}` appears only when resolved plaintext is available",
                    mount.describe()
                ));
            }
        }
        for mount in &refs_mounts {
            if !resolved_mounts.contains(mount) {
                found.push(format!(
                    "mount `{}` appears only when resolved plaintext is absent",
                    mount.describe()
                ));
            }
        }
    }

    match (&resolved.service_account, &refs_only.service_account) {
        // Presence is structural and prints no value at all.
        (Some(_), None) => {
            found.push(
                "service_account is set only when resolved plaintext is available".to_owned(),
            );
        }
        (None, Some(_)) => {
            found.push("service_account is set only when resolved plaintext is absent".to_owned());
        }
        // Two different accounts: the same rule as a diverging run variable,
        // and for the same reason. Neither side is quoted.
        (Some(resolved_account), Some(refs_account)) if resolved_account != refs_account => {
            found.push(format!(
                "service_account differs between the drives {REDACTED_DIVERGING_VALUE}"
            ));
        }
        _ => {}
    }

    found
}

/// Drive every method of `plugin` — `credential_schema`, `observed_schema`,
/// `validate_credentials`, `observe`, `prepare_run_access`, `runner`,
/// `env_contract`, and `health_check` — with `canary` as the credential
/// material, then check every marker in `canary` against: every returned
/// string field, each returned value's `Debug` rendering, the serialised
/// [`crate::observation::ObservedAttrs`], every `RunVar` name and value,
/// every [`MountSpec`] path and mounted `ConfigValue`, and every `tracing`
/// event emitted for the
/// duration of the call. `health_check` is driven too, even though its
/// signature carries no credential material: it shares the same
/// `&dyn QaProductPluginV1` object as every other method, so a stateful
/// plugin that cached a credential in `observe` or `prepare_run_access` can
/// echo it from there. Panics naming the surface and the marker on the first
/// leak found.
///
/// The credential material is planted **under the keys the plugin itself
/// declares** in `credential_schema()` (plus three fixed extras), and the
/// `config` is synthesised from its declared non-secret fields — see this
/// module's header for why a fixed plant asserted nothing at all.
///
/// Each marker is checked in five encodings (raw, escaped-string, lowercase
/// hex, uppercase hex, decimal byte array — see [`marker_variants`]) and
/// once more with both the marker and the surface text whitespace-normalised
/// (see [`normalize_whitespace`]), so reformatting a leaked value does not
/// defeat detection.
///
/// Layer 2 is enforced here too: the plugin's two schemas are run through
/// [`validate_schemas`] before anything is driven.
///
/// # Panics
///
/// Panics if any of `canary`'s three markers (`pem`, `token`, `password`),
/// in any of the encodings above, is found on any checked surface. The
/// panic message names the surface, the marker, and the encoding, and quotes
/// the offending text — except for a surface read out of a [`SecretValue`],
/// whose text is redacted in the message (see [`Surface`]).
///
/// Also panics if the plugin's declared schemas fail [`validate_schemas`]
/// (which would be a boot failure at registration); if its
/// `prepare_run_access` fails when given credstore references alone — the
/// contract says it must work from references, unconditionally, including on
/// an environment nothing has observed yet; or if the access it returns
/// *differs* between the two drives, which is what a plugin that reads the
/// plaintext looks like from the outside (see [`access_divergences`]).
///
/// Surfaces are checked most-specific-first within each call: an itemised
/// field (a `FieldDesc.key`, a `RunVar.value`, a `remote_message`, ...) is
/// checked before that call's own aggregate `Debug` rendering, so a leak in
/// a field this harness itemises is reported against that field rather than
/// against the coarser rendering that happens to embed it too.
pub async fn assert_no_leak(plugin: &dyn QaProductPluginV1, canary: &Canary) {
    let credential_schema = plugin.credential_schema();
    let observed_schema = plugin.observed_schema();
    if let Err(err) = validate_schemas(&credential_schema, &observed_schema) {
        panic!("leak-conformance: the plugin's declared schemas are invalid: {err}");
    }

    let planted = plant(&credential_schema, canary);
    let driven = drive(plugin, &credential_schema, &observed_schema, &planted).await;
    let surfaces = driven.surfaces;

    for (marker_name, marker) in canary.markers() {
        for (encoding, variant) in marker_variants(marker) {
            for surface in &surfaces {
                let (label, shown) = (&surface.label, surface.quotable());
                assert!(
                    !surface.text.contains(variant.as_str()),
                    "leak-conformance: canary marker `{marker_name}` reached surface \
                     `{label}` (encoding: {encoding}):\n{shown}"
                );
            }
        }
    }

    for (marker_name, marker) in canary.markers() {
        let normalized_marker = normalize_whitespace(marker);
        for surface in &surfaces {
            let (label, shown) = (&surface.label, surface.quotable());
            assert!(
                !normalize_whitespace(&surface.text).contains(normalized_marker.as_str()),
                "leak-conformance: canary marker `{marker_name}` reached surface \
                 `{label}` (whitespace-normalised match):\n{shown}"
            );
        }
    }

    // Checked last, deliberately: a leak is what this harness exists to find,
    // and a plugin that reads `resolved` in `prepare_run_access` usually
    // violates both rules at once — it mounts the plaintext it just read.
    // Reporting the leak is the more useful failure.
    assert!(
        driven.refs_only_access_ok,
        "leak-conformance: prepare_run_access failed when given credstore references \
         alone. It must work from references: dispatch never resolves a credential to \
         plaintext, and requiring it to would pull plaintext into a process that today \
         never holds any. An environment nothing has observed yet is part of that \
         shape (`EnvironmentHandle::observed` is `None` here), and must not be an \
         error either."
    );

    // Last of the three, and last for the same reason the leak assertions come
    // first: a divergence is *evidence* that plaintext was read, while a canary
    // hit is the leak itself. A plugin that reads the plaintext and echoes it
    // trips both, and the leak is the more useful report. This one is checked
    // after the reference-only assertion because it has nothing to compare
    // until that drive has returned an access at all.
    assert!(
        driven.access_divergences.is_empty(),
        "leak-conformance: prepare_run_access returned different access when it was given \
         resolved plaintext than when it was given credstore references alone. Every other \
         input to the two calls is identical, so a difference means the plaintext was read, \
         and the contract is that it must work from references alone. A `prepare_run_access` \
         that is not deterministic (a nonce, a timestamp, a generated id) also trips this, \
         and that is a defect too: dispatch must be reproducible. Diverging values are \
         redacted, so run your plugin under a debugger to see them:\n  {}",
        driven.access_divergences.join("\n  ")
    );
}

/// One full drive of a plugin: every string it put on a surface, plus the one
/// contract violation this harness can only observe by driving.
struct Driven {
    surfaces: Vec<Surface>,
    /// The reference-only drive of `prepare_run_access` returned `Ok` — the
    /// contract, asserted flatly rather than inferred from a comparison with
    /// the resolved drive.
    refs_only_access_ok: bool,
    /// How the two drives' access differs, when both returned `Ok` (see
    /// [`access_divergences`]). Empty when they agree, and empty when there
    /// is nothing to compare because a drive failed — the assertion above
    /// covers the reference-only failure, and a plugin that fails *only* with
    /// plaintext in hand has not broken this rule.
    access_divergences: Vec<String>,
}

/// Call every method once with the plant installed, collecting every string
/// the plugin put on a surface — plus everything it emitted through
/// `tracing` for the duration.
async fn drive(
    plugin: &dyn QaProductPluginV1,
    credential_schema: &[FieldDesc],
    observed_schema: &[FieldDesc],
    planted: &Plant,
) -> Driven {
    let log_buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(SharedWriter(Arc::clone(&log_buf)))
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let mut surfaces: Vec<Surface> = Vec::new();

    push_field_descs(&mut surfaces, "credential_schema()", credential_schema);
    surfaces.push(Surface::visible(
        "credential_schema() Debug",
        debug_of(&credential_schema),
    ));
    push_field_descs(&mut surfaces, "observed_schema()", observed_schema);
    surfaces.push(Surface::visible(
        "observed_schema() Debug",
        debug_of(&observed_schema),
    ));

    let validated = plugin.validate_credentials(&planted.input).await;
    match &validated {
        Ok(classifications) => {
            for classification in classifications {
                surfaces.push(Surface::visible(
                    "CredentialClassification.key",
                    classification.key.clone(),
                ));
            }
        }
        Err(failure) => push_failure(&mut surfaces, "validate_credentials()", failure),
    }
    surfaces.push(Surface::visible(
        "validate_credentials() Debug",
        debug_of(&validated),
    ));

    // `observed: None` on purpose. `Option` exists because dispatch can
    // reach an environment nothing has observed yet, and that shape is the
    // one a plugin is most likely to get wrong: a plugin that turns a missing
    // observation into an `Err` cannot be dispatched onto a freshly created
    // environment. Driving the never-observed shape is what makes the
    // reference-only assertion below catch that.
    let env = EnvironmentHandle {
        slots: &planted.slots,
        config: &planted.config,
        observed: None,
    };

    let observation = plugin.observe(&env).await;
    let observed_attrs = match &observation.environment {
        ObservationOutcome::Detected(attrs) => {
            let json = serde_json::to_string(attrs)
                .unwrap_or_else(|err| format!("<ObservedAttrs failed to serialise: {err}>"));
            surfaces.push(Surface::visible("observe() ObservedAttrs JSON", json));
            Some(attrs.clone())
        }
        ObservationOutcome::Failed(failure) => {
            push_failure(&mut surfaces, "observe().environment", failure);
            None
        }
    };
    if let HealthOutcome::Failed(failure) = &observation.health {
        push_failure(&mut surfaces, "observe().health", failure);
    }
    surfaces.push(Surface::visible("observe() Debug", debug_of(&observation)));

    let run_access = plugin.prepare_run_access(&env).await;
    match &run_access {
        Ok(access) => push_run_access(&mut surfaces, "RunAccess", access),
        Err(failure) => push_failure(&mut surfaces, "prepare_run_access()", failure),
    }
    surfaces.push(Surface::visible(
        "prepare_run_access() Debug",
        debug_of(&run_access),
    ));

    // Driven a second time the way dispatch drives it: references only, no
    // plaintext anywhere in the handle. A plugin that needs the bytes to
    // build a mount cannot be dispatched without materialising a plaintext
    // kubeconfig in a process that today never touches one.
    //
    // Exactly one thing differs between this handle and the one above:
    // whether `CredentialSlot::value` is populated. Keep it that way. The
    // comparison below reads any difference in the returned access as proof
    // the plaintext was read, so a second differing input here would turn
    // that assertion into a false-positive generator.
    let refs = reference_only(&planted.slots);
    let ref_env = EnvironmentHandle {
        slots: &refs,
        config: &planted.config,
        observed: None,
    };
    let ref_access = plugin.prepare_run_access(&ref_env).await;
    match &ref_access {
        Ok(access) => push_run_access(&mut surfaces, "RunAccess [refs only]", access),
        Err(failure) => push_failure(&mut surfaces, "prepare_run_access() [refs only]", failure),
    }
    surfaces.push(Surface::visible(
        "prepare_run_access() [refs only] Debug",
        debug_of(&ref_access),
    ));
    // A flat reading of the one drive the contract talks about. Comparing the
    // two drives — `run_access.is_ok() && ref_access.is_err()` — made the rule
    // conditional on the *resolved* drive having succeeded, so a plugin whose
    // `prepare_run_access` never succeeds at all was reported clean, and the
    // trait's own doc states the requirement with no such condition.
    let refs_only_access_ok = ref_access.is_ok();
    let access_divergences = match (&run_access, &ref_access) {
        (Ok(resolved), Ok(refs_only)) => access_divergences(resolved, refs_only),
        _ => Vec::new(),
    };

    let runner = plugin.runner(observed_attrs.as_ref());
    push_runner_spec(&mut surfaces, &runner);
    surfaces.push(Surface::visible("runner() Debug", debug_of(&runner)));

    let contract = plugin.env_contract();
    push_run_var_contract(&mut surfaces, &contract);
    surfaces.push(Surface::visible(
        "env_contract() Debug",
        debug_of(&contract),
    ));

    let health = plugin.health_check().await;
    if let Err(failure) = &health {
        push_failure(&mut surfaces, "health_check()", failure);
    }
    surfaces.push(Surface::visible("health_check() Debug", debug_of(&health)));

    drop(guard);
    surfaces.push(Surface::visible("tracing events", captured(&log_buf)));
    Driven {
        surfaces,
        refs_only_access_ok,
        access_divergences,
    }
}

#[cfg(test)]
#[path = "testing_tests.rs"]
mod testing_tests;
