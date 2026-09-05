//! REST DTOs (serde + utoipa) for the `qa-environments` gear.
//!
//! These types never leak into the SDK or domain layers — the SDK's
//! `qa_environments_sdk` models carry no serde/utoipa (contract-layer
//! purity), and the domain layer speaks those SDK models plus
//! `DomainError`. Conversions here are the only bridge.

use std::collections::BTreeMap;

use time::OffsetDateTime;
use uuid::Uuid;

use qa_environments_sdk as sdk;

// ==================== Environment DTOs ====================

/// REST DTO for a registered target environment.
///
/// Never carries the kubeconfig document, and — since 2026-08-27, by a human
/// decision — not its **credstore reference** either. The material is stored
/// under [`SharingMode::Tenant`](credstore_sdk::SharingMode::Tenant), so any
/// tenant member holding the reference can read it back through credstore's own
/// `GET /credstore/v1/secrets/{ref}`. This DTO is returned on **every**
/// environment read and write path, i.e. to every `qa.platform` GET/LIST-authorized
/// caller, so publishing the reference handed every tenant member a working read
/// path to a kubeconfig's `client-key-data` — a client private key — the moment
/// the create form began accepting pasted documents (`EnvironmentsService`).
///
/// This is exactly `SshKeyDto`'s convention
/// (`qa-catalog/qa-catalog/src/api/rest/dto.rs`), which withholds its own
/// `credstore_ref` for the same reason and says so. The reference stays on the
/// SDK model (`qa_environments_sdk::Environment::kubeconfig_credstore_ref`)
/// for in-process consumers — `qa-runs` resolves the kubeconfig from it when it
/// builds a dispatch spec — and on the column. Only the REST projection drops
/// it. The `name` is what identifies an environment to a human.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EnvironmentDto {
    pub id: Uuid,
    pub name: String,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    pub available: bool,
    pub observed_version: Option<String>,
    pub observed_build: Option<String>,
    /// Per-environment default branch override; `null` means the repository's own
    /// default applies. See `sdk::Environment::default_branch`.
    pub default_branch: Option<String>,
    /// Whether this environment is its product's default -- what the Run and
    /// Schedule dialogs' "Default cluster" option resolves to. At most one
    /// environment per product has this set. See `sdk::Environment::is_default`.
    pub is_default: bool,
    /// The most recent **failed** observation's message, or `null` if the
    /// most recent attempt succeeded (or none has run). See
    /// `sdk::Environment::version_detect_error` — this is the field a
    /// `POST /qa/v1/environments/{id}/refresh` populates on a 200 response when
    /// only detection itself failed.
    pub version_detect_error: Option<String>,
    /// When the most recent observation attempt ran, success or failure.
    #[serde(with = "time::serde::rfc3339::option")]
    pub version_detected_at: Option<OffsetDateTime>,
    /// The most recent observation's plugin-defined attributes, keyed by the
    /// plugin's own `FieldDesc::key`. `{}` for an environment the plugin path
    /// has never observed.
    ///
    /// This is what makes an environment table renderable without the UI
    /// knowing any product: the caller pairs these values with the product's
    /// `observed_schema()` from `GET /qa/v1/product-plugins`.
    ///
    /// **Safe to publish structurally, not by review.** Two rules stand
    /// between a plugin and this field: `observed_schema()` may declare no
    /// `Secret`/`MultilineSecret` field (a boot failure if it does), and
    /// `qa_product_sdk::observation::retain_declared` drops every *undeclared*
    /// key before the column is written — so a plugin that echoed its
    /// kubeconfig into an attribute finds the attribute gone before it is
    /// stored, let alone serialised here.
    pub observed_attrs: BTreeMap<String, String>,
    /// The environment's base domain (`https://…`), as the plugin's
    /// `FieldRole::BaseUrl` attribute. `null` means never conclusively
    /// detected.
    ///
    /// Non-secret, which is why it is published here at all: it is a URL an
    /// operator already knows, not a credential. It replaced `vhp_base_url`,
    /// which Task 19 dropped.
    pub observed_base_url: Option<String>,
    /// The most recent health verdict: `ok`, `degraded`, `down` or `unknown`.
    /// `unknown` covers both "nothing has looked" and "a look failed" —
    /// [`Self::health_checked_at`] is what tells those apart, being `null`
    /// only in the first case.
    pub health_state: String,
    /// Why the most recent health read reached that state, when there is
    /// something to say. Classified text only: a fixed string chosen by
    /// failure variant, or a remote service's own message — never a formatted
    /// error and never anything derived from a credential (**D12**).
    pub health_detail: Option<String>,
    /// When the most recent health read ran, or `null` if nothing ever looked.
    #[serde(with = "time::serde::rfc3339::option")]
    pub health_checked_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<sdk::Environment> for EnvironmentDto {
    fn from(p: sdk::Environment) -> Self {
        // `p.kubeconfig_credstore_ref` is intentionally dropped here — see the
        // struct doc comment. Do not add it back.
        Self {
            id: p.id,
            name: p.name,
            product_id: Some(p.product_id),
            description: p.description,
            available: p.available,
            observed_version: p.observed_version,
            observed_build: p.observed_build,
            default_branch: p.default_branch,
            is_default: p.is_default,

            version_detect_error: p.version_detect_error,
            version_detected_at: p.version_detected_at,
            // `p.credentials` is intentionally dropped, for the same reason as
            // `p.kubeconfig_credstore_ref` above: it carries credstore
            // references, and under `SharingMode::Tenant` a reference is a
            // read path to the material. `p.config` is withheld too — it is
            // operator-set and non-secret, but no caller in this plan reads it
            // and the create/patch DTOs are where an operator's own values
            // belong.
            observed_attrs: p
                .observed_attrs
                .iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
            observed_base_url: p.observed_base_url,
            health_state: p.health_state.as_str().to_owned(),
            health_detail: p.health_detail,
            health_checked_at: p.health_checked_at,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// One submitted credential on the wire: either the document or a reference.
///
/// Externally tagged, so a body reads
/// `{"credentials": {"kubeconfig": {"material": "apiVersion: v1\n…"}}}` or
/// `{"credentials": {"kubeconfig": {"reference": "credstore://kc/staging"}}}`.
/// The tag is what makes "exactly one of a document and a reference"
/// unrepresentable rather than validated — see
/// `sdk::CredentialSubmission`, whose shape this mirrors.
///
/// `Debug` is hand-written and redacts [`Self::Material`], for
/// `CreateEnvironmentReq`'s reason: a derived `Debug` here is the 2026-08-28
/// leak with a new field name. A reference is not redacted, matching
/// `kubeconfig_credstore_ref`'s long-standing treatment in these same
/// hand-written impls — it is withheld from *responses* (`EnvironmentDto`
/// drops it) because under `SharingMode::Tenant` it is a read path to the
/// material, but the caller of a request already holds it.
#[derive(Clone)]
#[toolkit_macros::api_dto(request)]
pub enum CredentialSubmissionDto {
    /// The document itself. Written to credstore under a reference this gear
    /// generates and then owns.
    Material(String),
    /// A credstore reference the caller already holds. Never written, never
    /// deleted by this gear.
    Reference(String),
}

impl std::fmt::Debug for CredentialSubmissionDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Material(_) => f.write_str("Material([REDACTED])"),
            Self::Reference(reference) => f.debug_tuple("Reference").field(reference).finish(),
        }
    }
}

impl From<CredentialSubmissionDto> for sdk::CredentialSubmission {
    fn from(dto: CredentialSubmissionDto) -> Self {
        match dto {
            CredentialSubmissionDto::Material(document) => {
                Self::Material(sdk::CredentialMaterial::new(document))
            }
            CredentialSubmissionDto::Reference(reference) => Self::Reference(reference),
        }
    }
}

/// Turn the wire map into the SDK's, or an empty map when the key is absent.
///
/// A free function rather than a `From` impl because the orphan rule puts
/// `BTreeMap<String, _>` out of reach, and both request DTOs need it.
fn submitted_credentials(
    credentials: Option<std::collections::BTreeMap<String, CredentialSubmissionDto>>,
) -> std::collections::BTreeMap<String, sdk::CredentialSubmission> {
    credentials
        .unwrap_or_default()
        .into_iter()
        .map(|(key, submission)| (key, submission.into()))
        .collect()
}

/// REST DTO for creating a new target environment.
///
/// # Two ways to supply the kubeconfig
///
/// `kubeconfig_credstore_ref` names a secret the caller has already
/// registered; `kubeconfig` is the **document** itself, for the operator who
/// has a file to paste and no reference to name. Exactly one must be present
/// — the service rejects both, and rejects neither with the same
/// `kubeconfig_credstore_ref must not be empty` error it always gave.
/// `kubeconfig_credstore_ref` is therefore `Option<String>` where it used to
/// be a required `String`; the required-ness moved from the schema to the
/// pair-wise rule, because neither field alone can be required any more.
///
/// `Debug` is hand-written to redact `kubeconfig` (the precedent is
/// `qa-catalog`'s `CreateSshKeyReq`, which mirrors credstore's own
/// `CreateSecretRequestDto`): a kubeconfig carries `client-key-data`, i.e. a
/// client private key, so a future `debug!("{req:?}")` anywhere would
/// otherwise write private key material into the logs. The redaction makes
/// that structurally impossible rather than a convention.
#[derive(Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateEnvironmentReq {
    pub name: String,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    /// Reference to a kubeconfig already in credstore. Mutually exclusive
    /// with `kubeconfig`.
    pub kubeconfig_credstore_ref: Option<String>,
    /// Raw kubeconfig YAML. Written to credstore immediately; this gear
    /// persists only the generated reference and never returns the document.
    /// Mutually exclusive with `kubeconfig_credstore_ref`.
    pub kubeconfig: Option<String>,
    /// Credentials keyed by the product plugin's own field key, each either
    /// the document (`{"material": …}`) or a credstore reference
    /// (`{"reference": …}`). This is the plugin-shaped channel; the
    /// `kubeconfig`/`kubeconfig_credstore_ref` pair above is the pre-plugin
    /// spelling of one entry of it and is still accepted so the shipped UI
    /// keeps working. Supplying both spellings of the same field is a
    /// validation error.
    pub credentials: Option<std::collections::BTreeMap<String, CredentialSubmissionDto>>,
    /// Optional per-environment default branch override. Absent, `null`, or an
    /// empty/whitespace-only string all mean "no override"; the service
    /// normalises. See `sdk::Environment::default_branch`.
    pub default_branch: Option<String>,
    /// Make this environment its product's default -- what the Run and Schedule
    /// dialogs' "Default cluster" option resolves to. Absent or `null` means
    /// `false`. Setting it clears the flag on the product's previous default.
    /// See `sdk::Environment::is_default`.
    pub is_default: Option<bool>,
}

impl std::fmt::Debug for CreateEnvironmentReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateEnvironmentReq")
            .field("name", &self.name)
            .field("product_id", &self.product_id)
            .field("description", &self.description)
            .field("kubeconfig_credstore_ref", &self.kubeconfig_credstore_ref)
            .field(
                "kubeconfig",
                &self.kubeconfig.as_ref().map(|_| "[REDACTED]"),
            )
            .field("credentials", &self.credentials)
            .field("default_branch", &self.default_branch)
            .field("is_default", &self.is_default)
            .finish()
    }
}

/// **Fallible since Task 20b**, on Task 20a's precedent: `product_id` is
/// required, and the refusal names what to supply. `TryFrom` rather than a
/// required serde field because serde's "missing field" cannot say where a
/// value comes from, and the shipped UI cannot send it until Task 22 — so that
/// message is the one a real caller meets.
impl TryFrom<CreateEnvironmentReq> for sdk::NewEnvironment {
    type Error = crate::domain::error::DomainError;

    fn try_from(req: CreateEnvironmentReq) -> Result<Self, Self::Error> {
        let product_id =
            req.product_id
                .ok_or_else(|| crate::domain::error::DomainError::Validation {
                    field: "product_id".to_owned(),
                    message: "an environment must name the product it belongs to: it is how its \
                          plugin resolves, and without one the environment can be neither \
                          observed nor dispatched against. `GET /qa/v1/products` lists them"
                        .to_owned(),
                })?;
        Ok(Self {
            name: req.name,
            product_id,
            description: req.description,
            // The pre-plugin pair still travels on the wire until Task 22
            // retires it in the UI; `desugar_legacy_credential_pair` folds it
            // into the plugin-shaped map. Task 19 dropped the *column*, not
            // the request field.
            kubeconfig_credstore_ref: req.kubeconfig_credstore_ref,
            kubeconfig: req.kubeconfig.map(sdk::CredentialMaterial::new),
            credentials: submitted_credentials(req.credentials),
            default_branch: req.default_branch,
            // Absent means `false`: a create that does not mention the flag is not
            // asking for a default, and `NewEnvironment::is_default` is a plain `bool`
            // because a row that does not exist yet has no value to leave unchanged.
            is_default: req.is_default.unwrap_or(false),
        })
    }
}

/// REST DTO for partially updating a target environment.
///
/// `serde_with` is not a workspace dependency, so — unlike
/// `sdk::EnvironmentPatch`'s nested `Option<Option<_>>` fields — `product_id`
/// and `description` here cannot distinguish an explicit JSON `null` (meaning
/// "clear this field") from the key being absent: both deserialize to `None`
/// and are mapped to "leave unchanged" (`sdk::EnvironmentPatch`'s outer `None`).
/// There is currently no REST-exposed way to clear a previously-set
/// `product_id` or `description` back to empty; only SDK/local-client
/// callers using `sdk::EnvironmentPatch` directly can do that.
///
/// # `default_branch` is the exception, and deliberately so
///
/// It reaches all three of `sdk::EnvironmentPatch`'s states over REST without
/// `serde_with`, because the source system's "clear" signal is **not** JSON
/// `null` — it is the **empty string**. `update_environment` maps an empty or
/// whitespace-only `default_branch` to its `"__NULL__"` sentinel and thence to
/// `NULL`, while an absent field keeps the stored value
/// (`manager/src/services/platforms.rs:447-496`). So:
///
/// | request body | meaning |
/// |---|---|
/// | key absent, or `null` | leave the override unchanged |
/// | `""` (or whitespace) | clear the override; the repository default applies |
/// | `"release-9.0"` | pin to that branch |
///
/// The middle row works because `Some(String)` survives the `.map(Some)` below
/// as `Some(Some(""))`, which `EnvironmentsService::normalize_default_branch` then
/// folds to `Some(None)` — the outer `Some` carrying "the caller mentioned the
/// field" the whole way. This is not a workaround for the missing `serde_with`;
/// it is the source system's own encoding, which happens not to need it.
///
/// # `kubeconfig` mirrors the create DTO
///
/// A patch may replace the kubeconfig by naming a new reference *or* by
/// pasting a new document, and supplying both is a validation error. Absent
/// on both leaves the stored reference alone. `Debug` is hand-written for the
/// same reason as `CreateEnvironmentReq`'s.
#[derive(Clone, Default)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateEnvironmentReq {
    pub name: Option<String>,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    /// Replace the stored reference with one the caller already holds.
    /// Mutually exclusive with `kubeconfig`.
    pub kubeconfig_credstore_ref: Option<String>,
    /// Replace the kubeconfig by pasting a new document. Written to credstore
    /// immediately; only the generated reference is stored. Mutually
    /// exclusive with `kubeconfig_credstore_ref`.
    pub kubeconfig: Option<String>,
    /// Credentials keyed by the product plugin's own field key, each either
    /// the document (`{"material": …}`) or a credstore reference
    /// (`{"reference": …}`). This is the plugin-shaped channel; the
    /// `kubeconfig`/`kubeconfig_credstore_ref` pair above is the pre-plugin
    /// spelling of one entry of it and is still accepted so the shipped UI
    /// keeps working. Supplying both spellings of the same field is a
    /// validation error.
    pub credentials: Option<std::collections::BTreeMap<String, CredentialSubmissionDto>>,
    pub available: Option<bool>,
    /// Per-environment default branch override. See the struct doc's table: absent
    /// or `null` leaves it unchanged, `""` clears it, any other value pins it.
    pub default_branch: Option<String>,
    /// Absent or `null` leaves the default flag unchanged; `true` makes this the
    /// product's default (clearing the previous holder); `false` clears it on this
    /// environment. See `sdk::Environment::is_default`.
    pub is_default: Option<bool>,
}

impl std::fmt::Debug for UpdateEnvironmentReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateEnvironmentReq")
            .field("name", &self.name)
            .field("product_id", &self.product_id)
            .field("description", &self.description)
            .field("kubeconfig_credstore_ref", &self.kubeconfig_credstore_ref)
            .field(
                "kubeconfig",
                &self.kubeconfig.as_ref().map(|_| "[REDACTED]"),
            )
            .field("credentials", &self.credentials)
            .field("available", &self.available)
            .field("default_branch", &self.default_branch)
            .field("is_default", &self.is_default)
            .finish()
    }
}

impl From<UpdateEnvironmentReq> for sdk::EnvironmentPatch {
    fn from(req: UpdateEnvironmentReq) -> Self {
        Self {
            name: req.name,
            // See the struct doc comment: absent and `null` both arrive here
            // as `None` and both mean "leave unchanged"; `Some(v)` means
            // "set to v". Explicitly clearing the field via REST is not
            // supported.
            product_id: req.product_id,
            description: req.description.map(Some),
            // Still on the wire until Task 22 -- see the create conversion.
            kubeconfig_credstore_ref: req.kubeconfig_credstore_ref,
            kubeconfig: req.kubeconfig.map(sdk::CredentialMaterial::new),
            credentials: submitted_credentials(req.credentials),
            available: req.available,
            // Unlike the two above, this one *can* express "clear": an empty
            // string arrives as `Some(Some(""))` and the service normalises it
            // to `Some(None)`. See the struct doc's table.
            default_branch: req.default_branch.map(Some),
            // Two-state inside the `Option`, unlike `default_branch` above: the flag
            // is a `bool`, so `Some(false)` is already the only "clear" there is and
            // no nullable third state exists to express.
            is_default: req.is_default,
        }
    }
}

// ==================== Variable DTOs ====================

/// REST DTO for a pipeline (global) or per-environment variable.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct VariableDto {
    pub id: Uuid,
    pub environment_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

impl From<sdk::Variable> for VariableDto {
    fn from(v: sdk::Variable) -> Self {
        Self {
            id: v.id,
            environment_id: v.environment_id,
            name: v.name,
            value: v.value,
        }
    }
}

/// REST DTO for creating or updating a variable by natural key.
///
/// `environment_id = None` targets the pipeline (global) table;
/// `environment_id = Some(_)` targets that environment's variable table.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpsertVariableReq {
    pub environment_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

impl From<UpsertVariableReq> for sdk::NewVariable {
    fn from(req: UpsertVariableReq) -> Self {
        Self {
            environment_id: req.environment_id,
            name: req.name,
            value: req.value,
        }
    }
}

/// Query parameters for `GET /qa/v1/variables`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ListVariablesQuery {
    /// When given, also include this environment's variables alongside the
    /// global pipeline variables.
    #[serde(default)]
    pub environment_id: Option<Uuid>,
}

// ==================== Lease DTO ====================

/// Read-only lease view for an environment's detail page (PRD: engineers must
/// see why a run is queued/waiting). Acquire/release are SDK-only
/// operations — see `crate::api::rest::routes::environments` — so this is the
/// only lease-related REST DTO.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
#[serde(tag = "state")]
pub enum LeaseDto {
    Free,
    HeldParallel { holders: Vec<Uuid> },
    HeldExclusive { holder: Uuid },
}

impl From<sdk::LeaseState> for LeaseDto {
    fn from(s: sdk::LeaseState) -> Self {
        match s {
            sdk::LeaseState::Free => Self::Free,
            sdk::LeaseState::HeldParallel { holders } => Self::HeldParallel { holders },
            sdk::LeaseState::HeldExclusive { holder } => Self::HeldExclusive { holder },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment() -> sdk::Environment {
        let now = OffsetDateTime::now_utc();
        sdk::Environment {
            id: Uuid::new_v4(),
            name: "staging-a".to_owned(),
            product_id: Uuid::new_v4(),
            description: Some("desc".to_owned()),
            available: true,
            observed_version: Some("1.2.3".to_owned()),
            observed_build: Some("20260813".to_owned()),
            default_branch: Some("release-9.0".to_owned()),
            is_default: true,
            version_detect_error: Some("namespaces \"virtuozzo\" not found".to_owned()),
            version_detected_at: Some(now),
            // The plugin-shaped half (Task 14). Populated, and with values
            // distinct from their legacy twins above, so
            // `environment_dto_from_sdk_carries_every_field_it_publishes` can
            // tell a field read from the wrong source.
            credentials: vec![sdk::EnvironmentCredential {
                key: "kubeconfig".to_owned(),
                credstore_ref: "credstore://plugin-ref".to_owned(),
            }],
            observed_attrs: {
                let mut attrs = qa_product_sdk::observation::ObservedAttrs::default();
                attrs.set("platformVersion", "1.2");
                attrs.set("baseDomain", "https://plugin.jele.io");
                attrs
            },
            config: serde_json::json!({"vpadm_namespace": "vzt"}),
            observed_base_url: Some("https://plugin.jele.io".to_owned()),
            health_state: qa_product_sdk::observation::HealthState::Degraded,
            health_detail: Some("one node not ready".to_owned()),
            health_checked_at: Some(now),
            created_at: now,
            updated_at: now,
        }
    }

    /// Every field the DTO has is carried through, and the one field it
    /// deliberately does not have stays absent from the serialized body.
    ///
    /// The name is no longer `preserves_all_fields`: `kubeconfig_credstore_ref`
    /// is on the SDK model and is *dropped* on purpose (see the struct doc), so
    /// a name promising a total mapping would be a false claim.
    #[test]
    #[allow(
        clippy::cognitive_complexity,
        reason = "a flat list of one assertion per published field; splitting it into helper \
                  functions would not reduce what it checks, only how it reads"
    )]
    fn environment_dto_from_sdk_carries_every_field_it_publishes() {
        let p = environment();
        let dto = EnvironmentDto::from(p.clone());
        assert_eq!(dto.id, p.id);
        assert_eq!(dto.name, p.name);
        assert_eq!(dto.product_id, Some(p.product_id));
        assert_eq!(dto.description, p.description);
        assert_eq!(dto.available, p.available);
        assert_eq!(dto.observed_version, p.observed_version);
        assert_eq!(dto.observed_build, p.observed_build);
        assert_eq!(dto.default_branch, p.default_branch);
        assert_eq!(dto.version_detect_error, p.version_detect_error);
        assert_eq!(dto.version_detected_at, p.version_detected_at);
        assert_eq!(dto.created_at, p.created_at);
        assert_eq!(dto.updated_at, p.updated_at);

        // The five plugin-shaped fields this DTO does publish (Task 14).
        assert_eq!(
            dto.observed_attrs,
            std::collections::BTreeMap::from([
                ("platformVersion".to_owned(), "1.2".to_owned()),
                ("baseDomain".to_owned(), "https://plugin.jele.io".to_owned()),
            ])
        );
        assert_eq!(dto.observed_base_url, p.observed_base_url);
        assert_eq!(
            dto.health_state, "degraded",
            "health_state goes on the wire as HealthState::as_str, not as a \
             serde-renamed enum"
        );
        assert_eq!(dto.health_detail, p.health_detail);
        assert_eq!(dto.health_checked_at, p.health_checked_at);

        // The withheld fields, asserted on the wire rather than on the struct:
        // re-adding one under any spelling puts a credstore reference back in
        // the body.
        let body = serde_json::to_string(&dto).unwrap();
        // What the three negative assertions below need to mean anything: this
        // really is the serialised DTO, and it really does carry the
        // plugin-shaped fields that ARE published.
        assert!(
            body.contains("observed_attrs") && body.contains("platformVersion"),
            "the body must be this DTO's own JSON: {body}"
        );
        assert!(
            !body.contains("kubeconfig") && !body.contains(&p.credentials[0].credstore_ref),
            "EnvironmentDto must not publish the credstore reference: {body}"
        );
        assert!(
            !body.contains("credentials") && !body.contains("credstore://plugin-ref"),
            "nor the same reference in plugin shape -- `credentials` carries \
             credstore references, so it is withheld for exactly the reason \
             `kubeconfig_credstore_ref` is: {body}"
        );
        assert!(
            !body.contains("vpadm_namespace"),
            "`config` is withheld too -- non-secret, but no caller reads it and \
             an operator's own values belong on the create/patch DTOs: {body}"
        );
    }

    /// **Review finding m-3.** The externally-tagged credential wire shape is
    /// Step 2's deliverable and the contract Task 22's generated form is being
    /// written against, and nothing asserted it — the tags were right only
    /// because `api_dto(request)` happens to apply
    /// `#[serde(rename_all = "snake_case")]`. A future change to that macro's
    /// default, or a variant rename, would move a public wire contract
    /// silently.
    #[test]
    fn the_credential_wire_shape_deserialises_both_arms() {
        let body = r#"{
            "name": "prod",
            "product_id": "00000000-0000-0000-0000-000000009001",
            "credentials": {
                "kubeconfig": { "material": "apiVersion: v1\n" },
                "api_token": { "reference": "credstore://tokens/prod" }
            }
        }"#;

        let req: CreateEnvironmentReq =
            serde_json::from_str(body).expect("the documented body must deserialise");
        let new: sdk::NewEnvironment = req.try_into().expect("the documented body names a product");

        assert!(
            matches!(
                new.credentials.get("kubeconfig"),
                Some(sdk::CredentialSubmission::Material(material))
                    if material.expose() == "apiVersion: v1\n"
            ),
            "`material` must become the pasted-document arm: {:?}",
            new.credentials.get("kubeconfig")
        );
        assert!(
            matches!(
                new.credentials.get("api_token"),
                Some(sdk::CredentialSubmission::Reference(reference))
                    if reference == "credstore://tokens/prod"
            ),
            "`reference` must become the credstore-reference arm: {:?}",
            new.credentials.get("api_token")
        );
    }

    /// An unknown tag is refused rather than silently dropped: a submitted
    /// credential the gear does not understand must not read as absent.
    #[test]
    fn an_unknown_credential_tag_is_refused() {
        let body = r#"{"name":"prod","credentials":{"kubeconfig":{"secret":"x"}}}"#;
        assert!(
            serde_json::from_str::<CreateEnvironmentReq>(body).is_err(),
            "an unrecognised tag must fail deserialisation"
        );
    }

    #[test]
    fn create_environment_req_into_new_environment() {
        let req = CreateEnvironmentReq {
            name: "staging-a".to_owned(),
            product_id: Some(Uuid::new_v4()),
            description: None,
            kubeconfig_credstore_ref: Some("credstore://ref".to_owned()),
            kubeconfig: None,
            credentials: None,
            default_branch: Some("release-9.0".to_owned()),
            is_default: Some(true),
        };
        let new: sdk::NewEnvironment = req
            .clone()
            .try_into()
            .expect("this request names a product");
        assert_eq!(new.name, req.name);
        assert_eq!(Some(new.product_id), req.product_id);
        assert_eq!(new.description, req.description);
        assert_eq!(new.kubeconfig_credstore_ref, req.kubeconfig_credstore_ref);
        assert_eq!(
            new.default_branch, req.default_branch,
            "the create path is two-state, so the value passes through untouched \
             and the service normalises it"
        );
        assert!(
            new.is_default,
            "an explicit `is_default: true` must reach NewEnvironment; the flag is what \
             the dialogs' \"Default cluster\" resolves against"
        );
    }

    #[test]
    fn update_environment_req_absent_fields_map_to_leave_unchanged() {
        let req = UpdateEnvironmentReq::default();
        let patch: sdk::EnvironmentPatch = req.into();
        assert_eq!(patch.name, None);
        assert_eq!(patch.product_id, None, "None must mean 'leave unchanged'");
        assert_eq!(patch.description, None, "None must mean 'leave unchanged'");
        assert_eq!(patch.kubeconfig_credstore_ref, None);
        assert_eq!(patch.available, None);
        assert_eq!(
            patch.default_branch, None,
            "an absent default_branch must keep the stored override, matching \
             `WHEN $6 IS NULL THEN platforms_meta.default_branch`"
        );
        assert_eq!(
            patch.is_default, None,
            "an absent is_default must leave the flag alone -- a patch that renames \
             an environment must not silently demote it"
        );
    }

    /// The one thing `product_id` and `description` cannot do over REST, and
    /// `default_branch` can — because the source system's clear signal is the
    /// empty string, not JSON `null`
    /// (`manager/src/services/platforms.rs:447-454`).
    ///
    /// The outer `Some` is what carries "the caller mentioned the field"; the
    /// service's normaliser then folds the inner `Some("")` to `None`. Both
    /// halves are asserted — this one here, the other in
    /// `crate::domain::service::environments_tests`'s
    /// `a_patch_with_a_blank_value_clears_the_override_rather_than_storing_it` —
    /// because either one alone would let the clear be silently dropped.
    #[test]
    fn an_empty_default_branch_is_the_rest_encoding_of_clear_it() {
        for blank in ["", "   ", "\t\n"] {
            let req = UpdateEnvironmentReq {
                default_branch: Some(blank.to_owned()),
                ..UpdateEnvironmentReq::default()
            };
            let patch: sdk::EnvironmentPatch = req.into();
            assert_eq!(
                patch.default_branch,
                Some(Some(blank.to_owned())),
                "the DTO must preserve the outer Some so the service can tell \
                 'clear it' from 'leave it alone'; normalising to None here \
                 would drop the clear"
            );
        }
    }

    #[test]
    fn update_environment_req_some_fields_map_to_set() {
        let product_id = Uuid::new_v4();
        let req = UpdateEnvironmentReq {
            name: Some("renamed".to_owned()),
            product_id: Some(product_id),
            description: Some("new desc".to_owned()),
            kubeconfig_credstore_ref: Some("credstore://new".to_owned()),
            kubeconfig: None,
            credentials: None,
            available: Some(false),
            default_branch: Some("release-9.0".to_owned()),
            is_default: Some(true),
        };
        let patch: sdk::EnvironmentPatch = req.into();
        assert_eq!(patch.name, Some("renamed".to_owned()));
        assert_eq!(
            patch.default_branch,
            Some(Some("release-9.0".to_owned())),
            "Some(v) must mean 'pin to v', not clear"
        );
        assert_eq!(
            patch.is_default,
            Some(true),
            "the flag is two-state inside the Option: Some(true) promotes, \
             Some(false) clears, None leaves alone"
        );
        assert_eq!(
            patch.product_id,
            Some(product_id),
            "Some(v) means 'rebind to v'. There is no 'clear' since Task 20b -- \
             the column is NOT NULL, so the tri-state collapsed to two"
        );
        assert_eq!(patch.description, Some(Some("new desc".to_owned())));
        assert_eq!(
            patch.kubeconfig_credstore_ref,
            Some("credstore://new".to_owned())
        );
        assert_eq!(patch.available, Some(false));
    }

    #[test]
    fn variable_dto_from_sdk_roundtrip() {
        let v = sdk::Variable {
            id: Uuid::new_v4(),
            environment_id: Some(Uuid::new_v4()),
            name: "FOO".to_owned(),
            value: "bar".to_owned(),
        };
        let dto = VariableDto::from(v.clone());
        assert_eq!(dto.id, v.id);
        assert_eq!(dto.environment_id, v.environment_id);
        assert_eq!(dto.name, v.name);
        assert_eq!(dto.value, v.value);
    }

    #[test]
    fn upsert_variable_req_into_new_variable() {
        let req = UpsertVariableReq {
            environment_id: None,
            name: "FOO".to_owned(),
            value: "bar".to_owned(),
        };
        let new: sdk::NewVariable = req.clone().into();
        assert_eq!(new.environment_id, req.environment_id);
        assert_eq!(new.name, req.name);
        assert_eq!(new.value, req.value);
    }

    #[test]
    fn lease_dto_from_sdk_state_variants() {
        assert!(matches!(
            LeaseDto::from(sdk::LeaseState::Free),
            LeaseDto::Free
        ));

        let holder = Uuid::new_v4();
        assert!(matches!(
            LeaseDto::from(sdk::LeaseState::HeldExclusive { holder }),
            LeaseDto::HeldExclusive { holder: h } if h == holder
        ));

        let holders = vec![Uuid::new_v4(), Uuid::new_v4()];
        match LeaseDto::from(sdk::LeaseState::HeldParallel {
            holders: holders.clone(),
        }) {
            LeaseDto::HeldParallel { holders: h } => assert_eq!(h, holders),
            other => panic!("expected HeldParallel, got {other:?}"),
        }
    }

    #[test]
    fn lease_dto_serializes_with_snake_case_state_tag() {
        let json = serde_json::to_value(LeaseDto::Free).expect("serialize");
        assert_eq!(json, serde_json::json!({"state": "free"}));

        let holder = Uuid::new_v4();
        let json = serde_json::to_value(LeaseDto::HeldExclusive { holder }).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"state": "held_exclusive", "holder": holder})
        );
    }
}
