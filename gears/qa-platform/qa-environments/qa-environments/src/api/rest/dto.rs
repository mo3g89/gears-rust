//! REST DTOs (serde + utoipa) for the `qa-environments` gear.
//!
//! These types never leak into the SDK or domain layers — the SDK's
//! `qa_environments_sdk` models carry no serde/utoipa (contract-layer
//! purity), and the domain layer speaks those SDK models plus
//! `DomainError`. Conversions here are the only bridge.

use time::OffsetDateTime;
use uuid::Uuid;

use qa_environments_sdk as sdk;

// ==================== Platform DTOs ====================

/// REST DTO for a registered target platform.
///
/// Never carries the kubeconfig document, and — since 2026-08-27, by a human
/// decision — not its **credstore reference** either. The material is stored
/// under [`SharingMode::Tenant`](credstore_sdk::SharingMode::Tenant), so any
/// tenant member holding the reference can read it back through credstore's own
/// `GET /credstore/v1/secrets/{ref}`. This DTO is returned on **every**
/// platform read and write path, i.e. to every `qa.platform` GET/LIST-authorized
/// caller, so publishing the reference handed every tenant member a working read
/// path to a kubeconfig's `client-key-data` — a client private key — the moment
/// the create form began accepting pasted documents (`PlatformsService`).
///
/// This is exactly `SshKeyDto`'s convention
/// (`qa-catalog/qa-catalog/src/api/rest/dto.rs`), which withholds its own
/// `credstore_ref` for the same reason and says so. The reference stays on the
/// SDK model (`qa_environments_sdk::TargetPlatform::kubeconfig_credstore_ref`)
/// for in-process consumers — `qa-runs` resolves the kubeconfig from it when it
/// builds a dispatch spec — and on the column. Only the REST projection drops
/// it. The `name` is what identifies a platform to a human.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PlatformDto {
    pub id: Uuid,
    pub name: String,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    pub available: bool,
    pub observed_version: Option<String>,
    pub observed_build: Option<String>,
    /// Per-platform default branch override; `null` means the repository's own
    /// default applies. See `sdk::TargetPlatform::default_branch`.
    pub default_branch: Option<String>,
    /// Whether this platform is its product's default -- what the Run and
    /// Schedule dialogs' "Default cluster" option resolves to. At most one
    /// platform per product has this set. See `sdk::TargetPlatform::is_default`.
    pub is_default: bool,
    /// The platform's base domain (`https://…`), recovered from its gateway.
    /// `null` means never conclusively detected. See
    /// `sdk::TargetPlatform::vhp_base_url`. Non-secret: unlike
    /// `kubeconfig_credstore_ref`, publishing this never exposes credential
    /// material.
    pub vhp_base_url: Option<String>,
    /// The namespace the platform's core components were detected in. See
    /// `sdk::TargetPlatform::observed_namespace`.
    pub observed_namespace: Option<String>,
    /// The most recent **failed** observation's message, or `null` if the
    /// most recent attempt succeeded (or none has run). See
    /// `sdk::TargetPlatform::version_detect_error` — this is the field a
    /// `POST /qa/v1/platforms/{id}/refresh` populates on a 200 response when
    /// only detection itself failed.
    pub version_detect_error: Option<String>,
    /// When the most recent observation attempt ran, success or failure.
    #[serde(with = "time::serde::rfc3339::option")]
    pub version_detected_at: Option<OffsetDateTime>,
    /// This platform's most recent cluster-health reading.
    ///
    /// `null` means no observation cycle has ever reached this platform yet.
    /// A populated value with `status: "Unreachable"` means a cycle DID reach
    /// it and could not read the cluster — a different fact from `null`, and
    /// the reason this is one optional object rather than a set of flat
    /// nullable fields that would blur the two together. `status_message` is
    /// set only for `"Unreachable"` and carries text already classified by
    /// the observer (`infra::observer::errors`), never a formatted
    /// `kube::Error`.
    pub cluster: Option<ClusterHealthDto>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<sdk::TargetPlatform> for PlatformDto {
    fn from(p: sdk::TargetPlatform) -> Self {
        // `p.kubeconfig_credstore_ref` is intentionally dropped here — see the
        // struct doc comment. Do not add it back.
        Self {
            id: p.id,
            name: p.name,
            product_id: p.product_id,
            description: p.description,
            available: p.available,
            observed_version: p.observed_version,
            observed_build: p.observed_build,
            default_branch: p.default_branch,
            is_default: p.is_default,
            vhp_base_url: p.vhp_base_url,
            observed_namespace: p.observed_namespace,
            version_detect_error: p.version_detect_error,
            version_detected_at: p.version_detected_at,
            cluster: p.cluster.map(ClusterHealthDto::from),
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// REST DTO for one node, as the platform's own cluster reported it.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct NodeSummaryDto {
    pub name: String,
    pub control_plane: bool,
    pub ready: bool,
    pub kubelet_version: Option<String>,
    pub os_image: Option<String>,
}

impl From<sdk::NodeSummary> for NodeSummaryDto {
    fn from(n: sdk::NodeSummary) -> Self {
        Self {
            name: n.name,
            control_plane: n.control_plane,
            ready: n.ready,
            kubelet_version: n.kubelet_version,
            os_image: n.os_image,
        }
    }
}

/// REST DTO for node counts derived server-side (once) from a
/// [`ClusterHealthDto`]'s `nodes`. The UI renders these; it must never
/// re-derive them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct NodeCountsDto {
    pub total: u32,
    pub ready: u32,
    pub control_plane: u32,
    pub ready_control_plane: u32,
    pub worker: u32,
    pub ready_worker: u32,
}

impl From<sdk::NodeCounts> for NodeCountsDto {
    fn from(c: sdk::NodeCounts) -> Self {
        Self {
            total: c.total,
            ready: c.ready,
            control_plane: c.control_plane,
            ready_control_plane: c.ready_control_plane,
            worker: c.worker,
            ready_worker: c.ready_worker,
        }
    }
}

/// REST DTO for one platform's cluster-health reading. See
/// [`PlatformDto::cluster`]'s own doc for what its presence and its `status`
/// mean.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct ClusterHealthDto {
    pub status: String,
    pub status_message: Option<String>,
    pub nodes: Vec<NodeSummaryDto>,
    pub namespace_count: Option<u32>,
    pub counts: NodeCountsDto,
    #[serde(with = "time::serde::rfc3339")]
    pub checked_at: OffsetDateTime,
}

impl From<sdk::ClusterHealthView> for ClusterHealthDto {
    fn from(v: sdk::ClusterHealthView) -> Self {
        Self {
            status: v.status,
            status_message: v.status_message,
            nodes: v.nodes.into_iter().map(NodeSummaryDto::from).collect(),
            namespace_count: v.namespace_count,
            counts: NodeCountsDto::from(v.counts),
            checked_at: v.checked_at,
        }
    }
}

/// REST DTO for creating a new target platform.
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
pub struct CreatePlatformReq {
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
    /// Optional per-platform default branch override. Absent, `null`, or an
    /// empty/whitespace-only string all mean "no override"; the service
    /// normalises. See `sdk::TargetPlatform::default_branch`.
    pub default_branch: Option<String>,
    /// Make this platform its product's default -- what the Run and Schedule
    /// dialogs' "Default cluster" option resolves to. Absent or `null` means
    /// `false`. Setting it clears the flag on the product's previous default.
    /// See `sdk::TargetPlatform::is_default`.
    pub is_default: Option<bool>,
}

impl std::fmt::Debug for CreatePlatformReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatePlatformReq")
            .field("name", &self.name)
            .field("product_id", &self.product_id)
            .field("description", &self.description)
            .field("kubeconfig_credstore_ref", &self.kubeconfig_credstore_ref)
            .field(
                "kubeconfig",
                &self.kubeconfig.as_ref().map(|_| "[REDACTED]"),
            )
            .field("default_branch", &self.default_branch)
            .field("is_default", &self.is_default)
            .finish()
    }
}

impl From<CreatePlatformReq> for sdk::NewPlatform {
    fn from(req: CreatePlatformReq) -> Self {
        Self {
            name: req.name,
            product_id: req.product_id,
            description: req.description,
            kubeconfig_credstore_ref: req.kubeconfig_credstore_ref,
            kubeconfig: req.kubeconfig.map(sdk::KubeconfigMaterial::new),
            default_branch: req.default_branch,
            // Absent means `false`: a create that does not mention the flag is not
            // asking for a default, and `NewPlatform::is_default` is a plain `bool`
            // because a row that does not exist yet has no value to leave unchanged.
            is_default: req.is_default.unwrap_or(false),
        }
    }
}

/// REST DTO for partially updating a target platform.
///
/// `serde_with` is not a workspace dependency, so — unlike
/// `sdk::PlatformPatch`'s nested `Option<Option<_>>` fields — `product_id`
/// and `description` here cannot distinguish an explicit JSON `null` (meaning
/// "clear this field") from the key being absent: both deserialize to `None`
/// and are mapped to "leave unchanged" (`sdk::PlatformPatch`'s outer `None`).
/// There is currently no REST-exposed way to clear a previously-set
/// `product_id` or `description` back to empty; only SDK/local-client
/// callers using `sdk::PlatformPatch` directly can do that.
///
/// # `default_branch` is the exception, and deliberately so
///
/// It reaches all three of `sdk::PlatformPatch`'s states over REST without
/// `serde_with`, because the source system's "clear" signal is **not** JSON
/// `null` — it is the **empty string**. `update_platform` maps an empty or
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
/// as `Some(Some(""))`, which `PlatformsService::normalize_default_branch` then
/// folds to `Some(None)` — the outer `Some` carrying "the caller mentioned the
/// field" the whole way. This is not a workaround for the missing `serde_with`;
/// it is the source system's own encoding, which happens not to need it.
///
/// # `kubeconfig` mirrors the create DTO
///
/// A patch may replace the kubeconfig by naming a new reference *or* by
/// pasting a new document, and supplying both is a validation error. Absent
/// on both leaves the stored reference alone. `Debug` is hand-written for the
/// same reason as `CreatePlatformReq`'s.
#[derive(Clone, Default)]
#[toolkit_macros::api_dto(request)]
pub struct UpdatePlatformReq {
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
    pub available: Option<bool>,
    /// Per-platform default branch override. See the struct doc's table: absent
    /// or `null` leaves it unchanged, `""` clears it, any other value pins it.
    pub default_branch: Option<String>,
    /// Absent or `null` leaves the default flag unchanged; `true` makes this the
    /// product's default (clearing the previous holder); `false` clears it on this
    /// platform. See `sdk::TargetPlatform::is_default`.
    pub is_default: Option<bool>,
}

impl std::fmt::Debug for UpdatePlatformReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdatePlatformReq")
            .field("name", &self.name)
            .field("product_id", &self.product_id)
            .field("description", &self.description)
            .field("kubeconfig_credstore_ref", &self.kubeconfig_credstore_ref)
            .field(
                "kubeconfig",
                &self.kubeconfig.as_ref().map(|_| "[REDACTED]"),
            )
            .field("available", &self.available)
            .field("default_branch", &self.default_branch)
            .field("is_default", &self.is_default)
            .finish()
    }
}

impl From<UpdatePlatformReq> for sdk::PlatformPatch {
    fn from(req: UpdatePlatformReq) -> Self {
        Self {
            name: req.name,
            // See the struct doc comment: absent and `null` both arrive here
            // as `None` and both mean "leave unchanged"; `Some(v)` means
            // "set to v". Explicitly clearing the field via REST is not
            // supported.
            product_id: req.product_id.map(Some),
            description: req.description.map(Some),
            kubeconfig_credstore_ref: req.kubeconfig_credstore_ref,
            kubeconfig: req.kubeconfig.map(sdk::KubeconfigMaterial::new),
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

/// REST DTO for a pipeline (global) or per-platform variable.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct VariableDto {
    pub id: Uuid,
    pub platform_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

impl From<sdk::Variable> for VariableDto {
    fn from(v: sdk::Variable) -> Self {
        Self {
            id: v.id,
            platform_id: v.platform_id,
            name: v.name,
            value: v.value,
        }
    }
}

/// REST DTO for creating or updating a variable by natural key.
///
/// `platform_id = None` targets the pipeline (global) table;
/// `platform_id = Some(_)` targets that platform's variable table.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpsertVariableReq {
    pub platform_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

impl From<UpsertVariableReq> for sdk::NewVariable {
    fn from(req: UpsertVariableReq) -> Self {
        Self {
            platform_id: req.platform_id,
            name: req.name,
            value: req.value,
        }
    }
}

/// Query parameters for `GET /qa/v1/variables`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ListVariablesQuery {
    /// When given, also include this platform's variables alongside the
    /// global pipeline variables.
    #[serde(default)]
    pub platform_id: Option<Uuid>,
}

// ==================== Lease DTO ====================

/// Read-only lease view for a platform's detail page (PRD: engineers must
/// see why a run is queued/waiting). Acquire/release are SDK-only
/// operations — see `crate::api::rest::routes::platforms` — so this is the
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

    fn platform() -> sdk::TargetPlatform {
        let now = OffsetDateTime::now_utc();
        sdk::TargetPlatform {
            id: Uuid::new_v4(),
            name: "staging-a".to_owned(),
            product_id: Some(Uuid::new_v4()),
            description: Some("desc".to_owned()),
            kubeconfig_credstore_ref: "credstore://ref".to_owned(),
            available: true,
            observed_version: Some("1.2.3".to_owned()),
            observed_build: Some("20260813".to_owned()),
            default_branch: Some("release-9.0".to_owned()),
            is_default: true,
            vhp_base_url: Some("https://sv.jele.io".to_owned()),
            observed_namespace: Some("virtuozzo".to_owned()),
            version_detect_error: Some("namespaces \"virtuozzo\" not found".to_owned()),
            version_detected_at: Some(now),
            cluster: Some(sdk::ClusterHealthView {
                status: "Healthy".to_owned(),
                status_message: None,
                nodes: vec![sdk::NodeSummary {
                    name: "sv-vhp-jele-io".to_owned(),
                    control_plane: true,
                    ready: true,
                    kubelet_version: Some("v1.33.4+k3s1".to_owned()),
                    os_image: Some("Ubuntu 24.04.3 LTS".to_owned()),
                }],
                namespace_count: Some(14),
                counts: sdk::NodeCounts {
                    total: 1,
                    ready: 1,
                    control_plane: 1,
                    ready_control_plane: 1,
                    worker: 0,
                    ready_worker: 0,
                },
                checked_at: now,
            }),
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
    fn platform_dto_from_sdk_carries_every_field_it_publishes() {
        let p = platform();
        let dto = PlatformDto::from(p.clone());
        assert_eq!(dto.id, p.id);
        assert_eq!(dto.name, p.name);
        assert_eq!(dto.product_id, p.product_id);
        assert_eq!(dto.description, p.description);
        assert_eq!(dto.available, p.available);
        assert_eq!(dto.observed_version, p.observed_version);
        assert_eq!(dto.observed_build, p.observed_build);
        assert_eq!(dto.default_branch, p.default_branch);
        assert_eq!(dto.vhp_base_url, p.vhp_base_url);
        assert_eq!(dto.observed_namespace, p.observed_namespace);
        assert_eq!(dto.version_detect_error, p.version_detect_error);
        assert_eq!(dto.version_detected_at, p.version_detected_at);
        assert_eq!(dto.created_at, p.created_at);
        assert_eq!(dto.updated_at, p.updated_at);

        let cluster = dto.cluster.clone().expect("the fixture's cluster is Some");
        let source_cluster = p.cluster.clone().expect("the fixture's cluster is Some");
        assert_eq!(cluster.status, source_cluster.status);
        assert_eq!(cluster.status_message, source_cluster.status_message);
        assert_eq!(cluster.namespace_count, source_cluster.namespace_count);
        assert_eq!(cluster.checked_at, source_cluster.checked_at);
        assert_eq!(cluster.counts.total, source_cluster.counts.total);
        assert_eq!(
            cluster.counts.control_plane,
            source_cluster.counts.control_plane
        );
        assert_eq!(cluster.nodes.len(), source_cluster.nodes.len());
        assert_eq!(cluster.nodes[0].name, source_cluster.nodes[0].name);
        assert_eq!(
            cluster.nodes[0].kubelet_version,
            source_cluster.nodes[0].kubelet_version
        );

        // The withheld field, asserted on the wire rather than on the struct:
        // re-adding it under any spelling puts the reference back in the body.
        let body = serde_json::to_string(&dto).unwrap();
        assert!(
            !body.contains("kubeconfig") && !body.contains(&p.kubeconfig_credstore_ref),
            "PlatformDto must not publish the credstore reference: {body}"
        );
    }

    #[test]
    fn create_platform_req_into_new_platform() {
        let req = CreatePlatformReq {
            name: "staging-a".to_owned(),
            product_id: Some(Uuid::new_v4()),
            description: None,
            kubeconfig_credstore_ref: Some("credstore://ref".to_owned()),
            kubeconfig: None,
            default_branch: Some("release-9.0".to_owned()),
            is_default: Some(true),
        };
        let new: sdk::NewPlatform = req.clone().into();
        assert_eq!(new.name, req.name);
        assert_eq!(new.product_id, req.product_id);
        assert_eq!(new.description, req.description);
        assert_eq!(new.kubeconfig_credstore_ref, req.kubeconfig_credstore_ref);
        assert_eq!(
            new.default_branch, req.default_branch,
            "the create path is two-state, so the value passes through untouched \
             and the service normalises it"
        );
        assert!(
            new.is_default,
            "an explicit `is_default: true` must reach NewPlatform; the flag is what \
             the dialogs' \"Default cluster\" resolves against"
        );
    }

    #[test]
    fn update_platform_req_absent_fields_map_to_leave_unchanged() {
        let req = UpdatePlatformReq::default();
        let patch: sdk::PlatformPatch = req.into();
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
             a platform must not silently demote it"
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
    /// `crate::domain::service::platforms_tests`'s
    /// `a_patch_with_a_blank_value_clears_the_override_rather_than_storing_it` —
    /// because either one alone would let the clear be silently dropped.
    #[test]
    fn an_empty_default_branch_is_the_rest_encoding_of_clear_it() {
        for blank in ["", "   ", "\t\n"] {
            let req = UpdatePlatformReq {
                default_branch: Some(blank.to_owned()),
                ..UpdatePlatformReq::default()
            };
            let patch: sdk::PlatformPatch = req.into();
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
    fn update_platform_req_some_fields_map_to_set() {
        let product_id = Uuid::new_v4();
        let req = UpdatePlatformReq {
            name: Some("renamed".to_owned()),
            product_id: Some(product_id),
            description: Some("new desc".to_owned()),
            kubeconfig_credstore_ref: Some("credstore://new".to_owned()),
            kubeconfig: None,
            available: Some(false),
            default_branch: Some("release-9.0".to_owned()),
            is_default: Some(true),
        };
        let patch: sdk::PlatformPatch = req.into();
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
            Some(Some(product_id)),
            "Some(v) must mean 'set to v', not clear"
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
            platform_id: Some(Uuid::new_v4()),
            name: "FOO".to_owned(),
            value: "bar".to_owned(),
        };
        let dto = VariableDto::from(v.clone());
        assert_eq!(dto.id, v.id);
        assert_eq!(dto.platform_id, v.platform_id);
        assert_eq!(dto.name, v.name);
        assert_eq!(dto.value, v.value);
    }

    #[test]
    fn upsert_variable_req_into_new_variable() {
        let req = UpsertVariableReq {
            platform_id: None,
            name: "FOO".to_owned(),
            value: "bar".to_owned(),
        };
        let new: sdk::NewVariable = req.clone().into();
        assert_eq!(new.platform_id, req.platform_id);
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
