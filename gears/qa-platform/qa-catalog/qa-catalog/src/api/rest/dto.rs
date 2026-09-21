//! REST DTOs (serde + utoipa) for the `qa-catalog` gear.
//!
//! These types never leak into the SDK or domain layers — the SDK's
//! `qa_catalog_sdk` models carry no serde/utoipa (contract-layer purity),
//! and the domain layer speaks those SDK models plus `DomainError`.
//! Conversions here are the only bridge.
//!
//! Mirrors `gears/qa-platform/qa-environments/.../api/rest/dto.rs`.

use time::OffsetDateTime;
use uuid::Uuid;

use qa_catalog_sdk as sdk;
use qa_product_sdk as sdk_plugin;

use crate::domain::error::DomainError;
use crate::domain::service::RegisteredProductPlugin;

// ==================== Test repository DTOs ====================

/// A test repository as published over REST.
///
/// **`credential_ref` is intentionally absent.** It names the credstore entry
/// holding this repository's git credentials, and a LIST or GET caller can
/// redeem a reference it has been handed. `SshKeyDto` and qa-environments'
/// `PlatformDto` drop their credstore refs for the same reason. Do not add it
/// back. The reference stays on the SDK model
/// (`qa_catalog_sdk::TestRepository::credential_ref`), which is where the sync
/// path reads it. Review finding #2.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct TestRepositoryDto {
    pub id: Uuid,
    /// Owning product.
    pub product_id: Uuid,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    /// Subdirectory within the repo that contains test content ("" = root).
    pub content_root: String,
    /// Whether an access credential is configured for this repository.
    ///
    /// A boolean, deliberately, not the `credential_ref` this field replaced: the
    /// reference names a credstore entry a LIST or GET caller could redeem, and
    /// that was the leak. Whether auth is configured is not itself sensitive — the
    /// URL already implies it — and an operator diagnosing a failed sync on a
    /// private repository needs it. `SshKeyDto` makes the same trade the same way,
    /// publishing a `fingerprint` rather than its `credstore_ref`.
    pub has_credential: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_synced_at: Option<OffsetDateTime>,
    /// Commit id the last successful sync materialized; `null` when the
    /// repository has never synced. The content revision, where
    /// `last_synced_at` is only the attempt instant — two syncs that find
    /// the same upstream tip give two `last_synced_at` values and one
    /// `head_commit`.
    pub head_commit: Option<String>,
    /// Sanitized error text of the last failed sync (`null` after a
    /// successful sync).
    pub sync_error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<sdk::TestRepository> for TestRepositoryDto {
    fn from(r: sdk::TestRepository) -> Self {
        Self {
            id: r.id,
            product_id: r.product_id,
            name: r.name,
            url: r.url,
            default_branch: r.default_branch,
            content_root: r.content_root,
            has_credential: r.credential_ref.is_some(),
            last_synced_at: r.last_synced_at,
            head_commit: r.head_commit,
            sync_error: r.sync_error,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// REST DTO for registering a new test repository.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateTestRepoReq {
    /// Owning product. Required — repo ownership attributes discovered plans
    /// to a product.
    pub product_id: Uuid,
    pub name: String,
    /// `https://`/`http://` remote only (p1 policy, ADR-0005); embedded
    /// userinfo credentials are rejected — use `credential_ref`.
    pub url: String,
    pub default_branch: String,
    /// Subdirectory within the repo that contains test content. Absent or
    /// `""` = repository root.
    #[serde(default)]
    pub content_root: String,
    /// credstore reference for the access credential. Absent/`null` = public
    /// repository.
    pub credential_ref: Option<String>,
}

impl From<CreateTestRepoReq> for sdk::NewTestRepository {
    fn from(req: CreateTestRepoReq) -> Self {
        Self {
            product_id: req.product_id,
            name: req.name,
            url: req.url,
            default_branch: req.default_branch,
            content_root: req.content_root,
            credential_ref: req.credential_ref,
        }
    }
}

/// REST DTO for `PUT /qa/v1/test-repos/{id}` — full replace of the
/// repository's mutable fields (no tri-state patch semantics: an absent
/// `credential_ref` *clears* the stored reference).
///
/// `default_branch` is mutable. It selects the branch used when a caller
/// names none; it does not identify the repository's synced content, so
/// changing it invalidates nothing already materialized. Changing `url` or
/// `content_root` clears the synced state, so content reads reject until the
/// next sync.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateTestRepoReq {
    /// Owning product. Required — repo ownership attributes discovered plans
    /// to a product.
    pub product_id: Uuid,
    pub name: String,
    /// `https://`/`http://` remote only (p1 policy, ADR-0005); embedded
    /// userinfo credentials are rejected — use `credential_ref`.
    pub url: String,
    pub default_branch: String,
    /// Subdirectory within the repo that contains test content. Absent or
    /// `""` = repository root.
    #[serde(default)]
    pub content_root: String,
    /// credstore reference for the access credential. Absent/`null` = public
    /// repository (and clears any reference previously stored).
    pub credential_ref: Option<String>,
}

impl From<UpdateTestRepoReq> for sdk::TestRepositoryUpdate {
    fn from(req: UpdateTestRepoReq) -> Self {
        Self {
            product_id: req.product_id,
            name: req.name,
            url: req.url,
            default_branch: req.default_branch,
            content_root: req.content_root,
            credential_ref: req.credential_ref,
        }
    }
}

/// Query parameters for `POST /qa/v1/test-repos/{id}/sync`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SyncTestRepoQuery {
    /// Branch to sync. Absent or `""` = the repository's default branch.
    #[serde(default)]
    pub branch: String,
}

/// Cached branch names of a repository (refreshed by sync and the
/// branch-cache lifecycle task).
///
/// Wrapped in an object (rather than a top-level `Vec<String>`) because
/// `OperationBuilder`'s array-response registration requires a named item
/// component schema, which a bare `String` is not.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct BranchListDto {
    pub branches: Vec<String>,
}

// ==================== Plan DTOs ====================

/// REST DTO for a plan discovered from a repository's `plan.yaml`
/// (materialized on read — never persisted).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PlanDto {
    pub repo_id: Uuid,
    pub branch: String,
    /// Path of the `plan.yaml` within the content root.
    pub path: String,
    pub name: String,
    pub test_files: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub tags: Vec<String>,
    /// Three-state exclusivity: `null` = inherit from the per-file
    /// `TEST_META` (NOT the same as `false`).
    pub exclusive: Option<bool>,
}

impl From<sdk::Plan> for PlanDto {
    fn from(p: sdk::Plan) -> Self {
        Self {
            repo_id: p.repo_id,
            branch: p.branch,
            path: p.path,
            name: p.name,
            test_files: p.test_files,
            timeout_seconds: p.timeout_seconds,
            tags: p.tags,
            exclusive: p.exclusive.to_option_bool(),
        }
    }
}

/// Query parameters for `GET /qa/v1/plans`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ListPlansQuery {
    /// Repository to discover plans in.
    pub repo_id: Uuid,
    /// Branch whose synced working copy is read.
    pub branch: String,
}

// ==================== Custom plan DTOs ====================

/// One test-file reference inside a custom plan.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct CustomPlanFileDto {
    pub repo_id: Uuid,
    /// File path under the repository's content root.
    pub path: String,
    /// Path of the `plan.yaml` that lists this file, in the same repository.
    ///
    /// # `Option` in the type, required on request
    ///
    /// This DTO serves both directions, and the two want different things: a
    /// **response** for a row written before the field existed must be able to say
    /// `null`, while a **request** must not be allowed to omit it (see
    /// `qa_catalog_sdk::NewCustomPlanEntry`). The type therefore stays `Option`
    /// and the requirement is enforced one step later, by
    /// `TryFrom<UpsertCustomPlanReq>`, which answers
    /// `DomainError::Validation { field: "files.plan_path", .. }` — a 400 that
    /// names the field.
    ///
    /// Rejected alternative: split into a request DTO with a plain `String` and a
    /// response DTO with an `Option`. Serde would then reject a missing key
    /// itself, which sounds stronger but moves the failure into the body
    /// extractor, whose status code and body shape are the framework's rather than
    /// this gear's — and the requirement is a *domain* rule, so it belongs with
    /// the gear's other 400s. It also doubles a two-field struct.
    ///
    /// No `#[serde(default)]` is needed for the request direction either: a missing
    /// key deserializes an `Option` to `None` unaided — `UpsertCustomPlanReq`'s own
    /// `timeout_seconds` below relies on exactly that, which is why the `tags`
    /// beside it carries `#[serde(default)]` (a `Vec` has no such handling) and it
    /// does not. Deliberately no line number: it is twenty lines down in this file
    /// and a number here would rot.
    ///
    /// **An earlier version of this comment credited a `#[serde(default)]` here
    /// with the compatibility, and that attribute was measured inert** — removing
    /// it left the whole gear green. It is gone.
    pub plan_path: Option<String>,
}

/// REST DTO for a user-composed persisted plan.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct CustomPlanDto {
    pub id: Uuid,
    pub name: String,
    /// May span repositories.
    pub files: Vec<CustomPlanFileDto>,
    pub tags: Vec<String>,
    pub timeout_seconds: Option<u64>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<sdk::CustomPlan> for CustomPlanDto {
    fn from(p: sdk::CustomPlan) -> Self {
        Self {
            id: p.id,
            name: p.name,
            files: p
                .files
                .into_iter()
                .map(|entry| CustomPlanFileDto {
                    repo_id: entry.repo_id,
                    path: entry.path,
                    plan_path: entry.plan_path,
                })
                .collect(),
            tags: p.tags,
            timeout_seconds: p.timeout_seconds,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// REST DTO for creating a custom plan, and — matching the SDK's
/// full-replace `update_custom_plan` contract — for `PUT` updates as well
/// (no tri-state patch semantics: the document is replaced wholesale).
///
/// # `plan_path` is required, and full-replace makes that wider than it looks
///
/// Because `PUT` replaces the whole document rather than patching it, an entry's
/// `plan_path` must be supplied on **every** update — including one that only
/// renames the plan or edits its tags and leaves the file list alone. There is no
/// tri-state "leave this field as it was".
///
/// That is a deliberate API break, taken on 2026-08-14 with the branch having no
/// upstream and no external clients. `an_upsert_request_omitting_plan_path_is_a_400`
/// pins it, and is the inversion of a test that previously forbade exactly this.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpsertCustomPlanReq {
    pub name: String,
    pub files: Vec<CustomPlanFileDto>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub timeout_seconds: Option<u64>,
}

impl TryFrom<UpsertCustomPlanReq> for sdk::NewCustomPlan {
    type Error = DomainError;

    /// Fallible because `plan_path` is optional in the DTO and mandatory in the
    /// write model — this is where that step happens, and where the 400 is minted.
    /// See `CustomPlanFileDto::plan_path` for why the requirement lives here
    /// rather than in serde.
    fn try_from(req: UpsertCustomPlanReq) -> Result<Self, Self::Error> {
        let files = req
            .files
            .into_iter()
            .map(|f| {
                let plan_path = f.plan_path.ok_or_else(|| DomainError::Validation {
                    field: "files.plan_path".to_owned(),
                    message: format!(
                        "every file must name the plan.yaml that lists it (missing for '{}')",
                        f.path
                    ),
                })?;
                Ok(sdk::NewCustomPlanEntry {
                    repo_id: f.repo_id,
                    path: f.path,
                    plan_path,
                })
            })
            .collect::<Result<Vec<_>, DomainError>>()?;

        Ok(Self {
            name: req.name,
            files,
            tags: req.tags,
            timeout_seconds: req.timeout_seconds,
        })
    }
}

// ==================== Product DTOs ====================

/// REST DTO for a product.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ProductDto {
    pub id: Uuid,
    pub name: String,
    /// Durable short code.
    pub key: String,
    pub description: String,
    pub folder: Option<String>,
    /// Full GTS instance id of the product plugin that owns this product's
    /// behaviour — the whole composed id, not the plugin's instance segment.
    ///
    /// **Always present since Task 20a.** The column is `NOT NULL`
    /// (`m20260903_000004`) and `From<sdk::Product>` wraps a `String`, so this
    /// field is never `null` on the wire. The `Option` survives only so the
    /// response shape does not change under clients that already parse it;
    /// Task 22 is where the UI stops needing that.
    ///
    /// It used to read "`null` while the column is still nullable … such a
    /// product has no resolvable plugin", which described a value this API can
    /// no longer return — on a public response field, in rustdoc (review
    /// finding IMPORTANT-4).
    pub plugin_instance_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<sdk::Product> for ProductDto {
    fn from(p: sdk::Product) -> Self {
        Self {
            id: p.id,
            name: p.name,
            key: p.key,
            description: p.description,
            folder: p.folder,
            plugin_instance_id: Some(p.plugin_instance_id),
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// REST DTO for creating a product.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateProductReq {
    pub name: String,
    pub key: String,
    pub description: String,
    pub folder: Option<String>,
    /// Full GTS instance id of the owning product plugin. **Required since
    /// Task 20a**: absent or `null` is a 400 from
    /// `TryFrom<CreateProductReq>` naming `GET /qa/v1/product-plugins`, and a
    /// bare instance segment, a type id, or an id no plugin registers is
    /// rejected with a 400 rather than stored as an id that resolves to
    /// nothing.
    ///
    /// It stays `Option` on the wire so the refusal can say *where to find a
    /// valid id*, which serde's "missing field" cannot. The doc used to say
    /// absence "leaves the product unbound, which is accepted only while the
    /// column is nullable" — untrue as of the commit that added the refusal
    /// twelve lines below it (review finding IMPORTANT-4).
    ///
    /// A missing key deserializes to `None` unaided — see
    /// [`CustomPlanFileDto::plan_path`] for the measurement behind not
    /// putting a `#[serde(default)]` here.
    pub plugin_instance_id: Option<String>,
}

/// `TryFrom` rather than `From` since Task 20, because
/// [`sdk::NewProduct::plugin_instance_id`] is a plain `String` and this DTO's
/// is an `Option`.
///
/// # Why the DTO keeps the `Option` when the domain type does not
///
/// **D6** says every product names a plugin, and the domain type makes that
/// unrepresentable rather than validated — which is the shape this codebase
/// prefers everywhere. But a *wire* field that is simply absent has to produce
/// an answer an operator can act on, and serde's "missing field
/// `plugin_instance_id`" does not say where a value comes from. The shipped UI
/// cannot send this field until Task 22, so that answer is the one a real
/// caller will meet (finding FW-1).
///
/// So the `Option` survives exactly one layer, and this conversion is where it
/// dies, with the message that names `GET /qa/v1/product-plugins`.
impl TryFrom<CreateProductReq> for sdk::NewProduct {
    type Error = DomainError;

    fn try_from(req: CreateProductReq) -> Result<Self, Self::Error> {
        let plugin_instance_id = req
            .plugin_instance_id
            .ok_or_else(|| DomainError::Validation {
                field: "plugin_instance_id".to_owned(),
                message:
                    "every product must name a product plugin: list the available ones at GET \
                      /qa/v1/product-plugins and supply one"
                        .to_owned(),
            })?;
        Ok(Self {
            name: req.name,
            key: req.key,
            description: req.description,
            folder: req.folder,
            plugin_instance_id,
        })
    }
}

/// REST DTO for `PUT /qa/v1/products/{id}` — full replace of the product's
/// mutable fields, with one exception: an absent `folder` moves the product
/// back to the root, while an absent `plugin_instance_id` leaves the stored
/// plugin binding alone.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateProductReq {
    pub name: String,
    pub key: String,
    pub description: String,
    pub folder: Option<String>,
    /// Full GTS instance id of the owning product plugin — see
    /// [`CreateProductReq::plugin_instance_id`]. **Not** full-replace: an
    /// absent value leaves the current binding in place rather than unbinding
    /// the product. `sdk::ProductUpdate::plugin_instance_id` carries the
    /// measurement behind that asymmetry — in short, the shipped UI cannot
    /// send this field, so full replace turned a description edit into a
    /// silent unbind.
    pub plugin_instance_id: Option<String>,
}

impl From<UpdateProductReq> for sdk::ProductUpdate {
    fn from(req: UpdateProductReq) -> Self {
        Self {
            name: req.name,
            key: req.key,
            description: req.description,
            folder: req.folder,
            plugin_instance_id: req.plugin_instance_id,
        }
    }
}

/// Distinct product folder names across the caller's visible products.
///
/// Wrapped in an object for the same reason as [`BranchListDto`].
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ProductFolderListDto {
    pub folders: Vec<String>,
}

// ==================== SSH key DTOs ====================

/// REST DTO for SSH key metadata.
///
/// Never carries the private key material, and — deliberately — not the
/// credstore reference either: the material is stored under
/// [`SharingMode::Tenant`](credstore_sdk::SharingMode::Tenant), so any
/// tenant member holding the reference can read it back through credstore's
/// own `GET /credstore/v1/secrets/{ref}`. Publishing the reference on a
/// `qa.ssh_key`/LIST-authorized endpoint would therefore have handed every
/// tenant member a working read path to another member's key. The
/// `fingerprint` is what identifies a key to a human; the reference stays on
/// the SDK model (`qa_catalog_sdk::SshKey`) for in-process consumers.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct SshKeyDto {
    pub id: Uuid,
    pub name: String,
    /// `SHA256:<hex>` fingerprint of the PEM material (p1 fallback — not the
    /// OpenSSH public-key fingerprint; see `SshKeysService`).
    pub fingerprint: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<sdk::SshKey> for SshKeyDto {
    fn from(k: sdk::SshKey) -> Self {
        // `k.credstore_ref` is intentionally dropped here — see the struct
        // doc comment. Do not add it back.
        Self {
            id: k.id,
            name: k.name,
            fingerprint: k.fingerprint,
            created_at: k.created_at,
        }
    }
}

/// REST DTO for creating an SSH key. The PEM goes straight to credstore and
/// is never echoed back (the response is [`SshKeyDto`], which carries
/// neither the material nor its credstore reference).
///
/// `Debug` is hand-written to redact `private_key_pem` (mirrors credstore's
/// own `CreateSecretRequestDto`): this type is `pub`, so a future
/// `debug!("{req:?}")` anywhere would otherwise write a private key into the
/// logs. The redaction makes that structurally impossible rather than a
/// convention.
#[derive(Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateSshKeyReq {
    pub name: String,
    /// Private key material (PEM). Written to credstore immediately; this
    /// gear never persists it (only the reference and fingerprint).
    pub private_key_pem: String,
}

impl std::fmt::Debug for CreateSshKeyReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateSshKeyReq")
            .field("name", &self.name)
            .field("private_key_pem", &"[REDACTED]")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn repository_fixture() -> sdk::TestRepository {
        let now = OffsetDateTime::now_utc();
        sdk::TestRepository {
            id: Uuid::new_v4(),
            product_id: Uuid::new_v4(),
            name: "smoke".to_owned(),
            url: "https://example.com/org/repo.git".to_owned(),
            default_branch: "main".to_owned(),
            content_root: "tests".to_owned(),
            credential_ref: Some("qa-cred".to_owned()),
            last_synced_at: Some(now),
            head_commit: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            sync_error: Some("boom".to_owned()),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn test_repository_dto_preserves_all_fields() {
        let r = repository_fixture();
        let dto = TestRepositoryDto::from(r.clone());
        assert_eq!(dto.id, r.id);
        assert_eq!(dto.product_id, r.product_id);
        assert_eq!(dto.name, r.name);
        assert_eq!(dto.url, r.url);
        assert_eq!(dto.default_branch, r.default_branch);
        assert_eq!(dto.content_root, r.content_root);
        assert_eq!(dto.last_synced_at, r.last_synced_at);
        assert_eq!(dto.sync_error, r.sync_error);
        assert_eq!(dto.created_at, r.created_at);
        assert_eq!(dto.updated_at, r.updated_at);
    }

    /// **`credential_ref` must not appear on the read DTO.**
    ///
    /// It names a credstore entry holding the repository's git credentials, and a
    /// LIST or GET caller can redeem it. `SshKeyDto` (`:528`) and `PlatformDto`
    /// (`qa-environments/.../dto.rs:107`) both drop their credstore ref for the
    /// same reason and both say so; this DTO was the outlier. Review finding #2.
    ///
    /// Asserted against the serialized JSON rather than the struct, because the
    /// struct not having the field is what a compiler enforces and the wire not
    /// carrying it is what an operator cares about.
    #[test]
    fn the_read_dto_does_not_publish_the_credential_reference() {
        let r = sdk::TestRepository {
            credential_ref: Some("qa-cred".to_owned()),
            ..repository_fixture()
        };
        let body = serde_json::to_string(&TestRepositoryDto::from(r)).unwrap();
        assert!(
            !body.contains("credential_ref"),
            "the read DTO must not publish credential_ref; body was {body}"
        );
        assert!(
            !body.contains("qa-cred"),
            "the read DTO must not publish the reference value; body was {body}"
        );
    }

    /// **`has_credential` publishes the fact, not the reference.**
    ///
    /// It replaces `credential_ref` as the wire-visible signal that a repository is
    /// authenticated: a bare boolean carries nothing a caller can redeem, unlike the
    /// reference it stands in for, which must still never appear on the wire (the
    /// leak this DTO fixed). Review finding #2.
    #[test]
    fn has_credential_reflects_whether_a_reference_is_set_without_publishing_it() {
        let with_ref = sdk::TestRepository {
            credential_ref: Some("qa-cred".to_owned()),
            ..repository_fixture()
        };
        let body = serde_json::to_string(&TestRepositoryDto::from(with_ref)).unwrap();
        assert!(
            body.contains("\"has_credential\":true"),
            "expected has_credential:true; body was {body}"
        );
        assert!(
            !body.contains("credential_ref"),
            "the read DTO must not publish credential_ref; body was {body}"
        );
        assert!(
            !body.contains("qa-cred"),
            "the read DTO must not publish the reference value; body was {body}"
        );

        let without_ref = sdk::TestRepository {
            credential_ref: None,
            ..repository_fixture()
        };
        let body = serde_json::to_string(&TestRepositoryDto::from(without_ref)).unwrap();
        assert!(
            body.contains("\"has_credential\":false"),
            "expected has_credential:false; body was {body}"
        );
    }

    #[test]
    fn create_test_repo_req_into_new_repository() {
        let req = CreateTestRepoReq {
            product_id: Uuid::new_v4(),
            name: "smoke".to_owned(),
            url: "https://example.com/org/repo.git".to_owned(),
            default_branch: "main".to_owned(),
            content_root: String::new(),
            credential_ref: None,
        };
        let new: sdk::NewTestRepository = req.clone().into();
        assert_eq!(new.product_id, req.product_id);
        assert_eq!(new.name, req.name);
        assert_eq!(new.url, req.url);
        assert_eq!(new.default_branch, req.default_branch);
        assert_eq!(new.content_root, req.content_root);
        assert_eq!(new.credential_ref, req.credential_ref);
    }

    #[test]
    fn create_test_repo_req_content_root_defaults_to_root() {
        let req: CreateTestRepoReq = serde_json::from_value(serde_json::json!({
            "product_id": Uuid::new_v4(),
            "name": "smoke",
            "url": "https://example.com/org/repo.git",
            "default_branch": "main",
        }))
        .unwrap();
        assert_eq!(req.content_root, "");
        assert_eq!(req.credential_ref, None);
    }

    /// The wire boundary for `Plan::exclusive`: `sdk::Exclusivity` carries no
    /// serde impl of its own (contract-purity rule; see that type's doc), so
    /// `PlanDto::from` is where `null`/`true`/`false` actually gets produced.
    /// This is one of the two named boundaries `Exclusivity`'s doc points at —
    /// asserted here on the rendered JSON, not just on the Rust-level
    /// `Option<bool>` field, so a `PlanDto::from` that stopped calling
    /// `to_option_bool` would fail this test even if the field happened to
    /// carry the right value some other way.
    #[test]
    fn plan_dto_preserves_exclusive_tri_state() {
        for (exclusive, wire_option, wire_json) in [
            (sdk::Exclusivity::Inherit, None, serde_json::json!(null)),
            (
                sdk::Exclusivity::Exclusive,
                Some(true),
                serde_json::json!(true),
            ),
            (
                sdk::Exclusivity::Shared,
                Some(false),
                serde_json::json!(false),
            ),
        ] {
            let plan = sdk::Plan {
                repo_id: Uuid::new_v4(),
                product_id: Uuid::new_v4(),
                branch: "main".to_owned(),
                path: "plans/smoke.yaml".to_owned(),
                name: "smoke".to_owned(),
                test_files: vec!["a.py".to_owned()],
                timeout_seconds: Some(60),
                tags: vec!["smoke".to_owned()],
                // Carried on `sdk::Plan` for qa-runs only — `PlanDto`
                // deliberately does not surface it (no REST consumer).
                validation: false,
                exclusive,
            };
            let dto = PlanDto::from(plan.clone());
            assert_eq!(dto.exclusive, wire_option, "tri-state must survive");
            assert_eq!(dto.repo_id, plan.repo_id);
            assert_eq!(dto.test_files, plan.test_files);

            let body = serde_json::to_value(&dto).unwrap();
            assert_eq!(
                body["exclusive"], wire_json,
                "the rendered JSON must be null/true/false, unchanged from the \
                 Option<bool> this type replaced"
            );
        }
    }

    /// Both entries deliberately differ in `plan_path`: `Some` and `None` in one
    /// list is what catches a conversion that hardcodes either.
    #[test]
    fn custom_plan_dto_maps_file_entries_including_plan_path() {
        let now = OffsetDateTime::now_utc();
        let repo_id = Uuid::new_v4();
        let plan = sdk::CustomPlan {
            id: Uuid::new_v4(),
            name: "mine".to_owned(),
            files: vec![
                sdk::CustomPlanEntry {
                    repo_id,
                    path: "tests/a.py".to_owned(),
                    plan_path: Some("plans/smoke.yaml".to_owned()),
                },
                sdk::CustomPlanEntry {
                    repo_id,
                    path: "tests/b.py".to_owned(),
                    plan_path: None,
                },
            ],
            tags: vec![],
            timeout_seconds: None,
            created_at: now,
            updated_at: now,
        };
        let dto = CustomPlanDto::from(plan);
        assert_eq!(dto.files.len(), 2);
        assert_eq!(dto.files[0].repo_id, repo_id);
        assert_eq!(dto.files[0].path, "tests/a.py");
        assert_eq!(dto.files[0].plan_path.as_deref(), Some("plans/smoke.yaml"));
        assert_eq!(dto.files[1].path, "tests/b.py");
        assert_eq!(dto.files[1].plan_path, None);
    }

    /// Two entries with **different** `plan_path` values, so a conversion that
    /// hardcoded or reused one could not pass.
    #[test]
    fn upsert_custom_plan_req_into_new_custom_plan() {
        let repo_id = Uuid::new_v4();
        let req = UpsertCustomPlanReq {
            name: "mine".to_owned(),
            files: vec![
                CustomPlanFileDto {
                    repo_id,
                    path: "tests/a.py".to_owned(),
                    plan_path: Some("plans/smoke.yaml".to_owned()),
                },
                CustomPlanFileDto {
                    repo_id,
                    path: "tests/b.py".to_owned(),
                    plan_path: Some("plans/nightly.yaml".to_owned()),
                },
            ],
            tags: vec!["t".to_owned()],
            timeout_seconds: Some(30),
        };
        let new = sdk::NewCustomPlan::try_from(req).expect("both entries name a plan");
        assert_eq!(new.name, "mine");
        assert_eq!(
            new.files,
            vec![
                sdk::NewCustomPlanEntry {
                    repo_id,
                    path: "tests/a.py".to_owned(),
                    plan_path: "plans/smoke.yaml".to_owned(),
                },
                sdk::NewCustomPlanEntry {
                    repo_id,
                    path: "tests/b.py".to_owned(),
                    plan_path: "plans/nightly.yaml".to_owned(),
                },
            ]
        );
        assert_eq!(new.tags, vec!["t".to_owned()]);
        assert_eq!(new.timeout_seconds, Some(30));
    }

    /// **The inversion of a test that used to forbid this.** Until 2026-08-14 an
    /// `an_upsert_request_omitting_plan_path_still_deserializes` test asserted that
    /// a body with no `plan_path` was accepted; the user then chose to make the
    /// field mandatory on write, because an entry without its plan names
    /// nothing, and optional-on-write was itself the divergence.
    ///
    /// It is inverted rather than deleted so the **API break is pinned**: the body
    /// still deserializes (the DTO field is `Option`, deliberately — see
    /// `CustomPlanFileDto::plan_path`), and the refusal happens one step later,
    /// where it can be a 400 naming the field instead of a body-extractor
    /// rejection. Both halves are asserted, because "it decodes" and "it is
    /// refused" are two different facts and only the pair describes the contract.
    ///
    /// Since `PUT` is full-replace with no tri-state patch semantics, this also
    /// means a rename or tag edit must resend every entry's `plan_path`.
    #[test]
    fn an_upsert_request_omitting_plan_path_is_a_400() {
        let repo_id = Uuid::new_v4();
        let body = serde_json::json!({
            "name": "mine",
            "files": [{"repo_id": repo_id.to_string(), "path": "tests/a.py"}],
            "timeout_seconds": null,
        });

        // Still decodes: the DTO models the field as absent-able so the refusal
        // can be the gear's own error rather than the framework's.
        let req: UpsertCustomPlanReq =
            serde_json::from_value(body).expect("the body itself is still well-formed");
        assert_eq!(req.files[0].plan_path, None);

        // And is refused, naming the field.
        let err = sdk::NewCustomPlan::try_from(req)
            .expect_err("an entry naming no plan must not reach the write model");
        match err {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "files.plan_path");
                assert!(
                    message.contains("tests/a.py"),
                    "the message must say which entry is at fault, got {message:?}"
                );
            }
            other => panic!("expected a Validation error, got {other:?}"),
        }
    }

    /// A partially-populated file list is refused too, and the error names the
    /// offending entry rather than the first one.
    ///
    /// The mixed case is the one a per-request check would get right and a
    /// `files.is_empty()`-style guard would miss.
    #[test]
    fn one_entry_omitting_plan_path_refuses_the_whole_request() {
        let repo_id = Uuid::new_v4();
        let req = UpsertCustomPlanReq {
            name: "mine".to_owned(),
            files: vec![
                CustomPlanFileDto {
                    repo_id,
                    path: "tests/ok.py".to_owned(),
                    plan_path: Some("plans/smoke.yaml".to_owned()),
                },
                CustomPlanFileDto {
                    repo_id,
                    path: "tests/missing.py".to_owned(),
                    plan_path: None,
                },
            ],
            tags: vec![],
            timeout_seconds: None,
        };

        let err = sdk::NewCustomPlan::try_from(req).expect_err("one bad entry is enough");
        match err {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "files.plan_path");
                assert!(
                    message.contains("tests/missing.py"),
                    "the message must name the offending entry, got {message:?}"
                );
            }
            other => panic!("expected a Validation error, got {other:?}"),
        }
    }

    #[test]
    fn ssh_key_dto_never_carries_pem_or_credstore_ref() {
        let k = sdk::SshKey {
            id: Uuid::new_v4(),
            name: "deploy".to_owned(),
            credstore_ref: "qa-catalog-ssh-key-deadbeef".to_owned(),
            fingerprint: "SHA256:abc".to_owned(),
            created_at: OffsetDateTime::now_utc(),
        };
        let dto = SshKeyDto::from(k);
        let json = serde_json::to_value(&dto).unwrap();
        let obj = json.as_object().unwrap();
        // Field-set lockdown: adding PEM material — or re-adding the
        // credstore reference, which is a working read path to the material
        // for any tenant member — must fail this test.
        assert_eq!(obj.len(), 4);
        for key in ["id", "name", "fingerprint", "created_at"] {
            assert!(obj.contains_key(key), "missing field {key}");
        }
        let rendered = json.to_string();
        assert!(!rendered.contains("private_key"));
        assert!(
            !rendered.contains("credstore_ref") && !rendered.contains("qa-catalog-ssh-key-"),
            "the credstore reference must never reach a REST response: {rendered}"
        );
    }

    #[test]
    fn create_ssh_key_req_debug_redacts_the_pem() {
        let req = CreateSshKeyReq {
            name: "deploy".to_owned(),
            private_key_pem: "-----BEGIN OPENSSH PRIVATE KEY-----\nSUPERSECRET\n".to_owned(),
        };
        let rendered = format!("{req:?}");
        assert!(
            !rendered.contains("SUPERSECRET") && !rendered.contains("BEGIN OPENSSH"),
            "Debug must not render key material: {rendered}"
        );
        assert!(rendered.contains("[REDACTED]"), "got {rendered}");
        assert!(rendered.contains("deploy"), "name is safe to render");
    }
}

// ==================== Test bundle DTOs ====================

/// The query string of `GET /qa/v1/test-bundles/{id}`.
///
/// One required field, and it is the route's entire access control: the route
/// is registered `.anonymous().exposed()` (see `api::rest::routes::bundles`),
/// so a request that reaches the handler has passed no authentication at all.
///
/// `sig` is **this gear's own choice, echoed back** — an HMAC-SHA256 tag over
/// `(bundle_id, tenant_id)` that `BundlesService::create_bundle` minted and
/// the Argo adapter rendered into `TEST_BUNDLE_URL`. It is a plain `String`
/// rather than a parsed byte array because a non-hex value must be refused
/// exactly like a mismatching one, on the same 403, and a deserialization
/// error here would be a 400 that told the caller which of the two it was.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BundleDownloadQuery {
    /// Required. Hex-encoded HMAC-SHA256 over `(bundle_id, tenant_id)`,
    /// verified by `BundlesService::get_bundle_content_signed` before the
    /// descriptor is read under any tenant's scope.
    pub sig: String,
}

// ==================== Product plugin DTOs ====================

/// A field's data type, mirroring `qa_product_sdk::FieldKind`.
///
/// # Why this is a mirror and not the SDK type
///
/// `#[api_dto]` adds `utoipa::ToSchema`, and every type nested in a DTO needs
/// it too. The SDK's descriptor types carry `serde` (they are persisted in
/// `observed_attrs` JSONB) but deliberately **no** utoipa: this module's own
/// header states the rule — SDK models carry no serde/utoipa and the DTO
/// layer is the only bridge — and adding an `OpenAPI` dependency to
/// `qa-product-sdk` would put a REST concern in the crate every third-party
/// product plugin links.
///
/// The variant names and their wire spellings must match the SDK's
/// `#[serde(rename_all = "snake_case")]` exactly; `product_plugin_dto_tests`
/// pins that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum FieldKindDto {
    Text,
    Url,
    Int,
    Bool,
    Enum,
    Secret,
    MultilineSecret,
}

impl From<sdk_plugin::FieldKind> for FieldKindDto {
    fn from(k: sdk_plugin::FieldKind) -> Self {
        match k {
            sdk_plugin::FieldKind::Text => Self::Text,
            sdk_plugin::FieldKind::Url => Self::Url,
            sdk_plugin::FieldKind::Int => Self::Int,
            sdk_plugin::FieldKind::Bool => Self::Bool,
            sdk_plugin::FieldKind::Enum => Self::Enum,
            sdk_plugin::FieldKind::Secret => Self::Secret,
            sdk_plugin::FieldKind::MultilineSecret => Self::MultilineSecret,
        }
    }
}

/// The platform meaning a field claims, mirroring `qa_product_sdk::FieldRole`.
///
/// There is no `Health` variant, and there is no `Health` role in the SDK
/// either: health has its own channel and its own columns, because a role is
/// a projection of a string attribute into a column while health is a closed
/// enum. See the SDK's `FieldRole` docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum FieldRoleDto {
    Version,
    Build,
    BaseUrl,
    Namespace,
}

impl From<sdk_plugin::FieldRole> for FieldRoleDto {
    fn from(r: sdk_plugin::FieldRole) -> Self {
        match r {
            sdk_plugin::FieldRole::Version => Self::Version,
            sdk_plugin::FieldRole::Build => Self::Build,
            sdk_plugin::FieldRole::BaseUrl => Self::BaseUrl,
            sdk_plugin::FieldRole::Namespace => Self::Namespace,
        }
    }
}

/// One declared field of a product plugin's credential or observation schema.
#[derive(Debug, Clone)]
// Four flags, mirroring `qa_product_sdk::FieldDesc`, which carries the same
// allow for the same reason: these are four independent declarations about one
// field, not a state machine, and a bitflags or sub-struct encoding would make
// the rendered JSON worse for the UI that consumes it.
#[allow(clippy::struct_excessive_bools)]
#[toolkit_macros::api_dto(response)]
pub struct FieldDescDto {
    /// Stable identifier, unique within its schema, and the key the value
    /// appears under in `credentials` or `observed_attrs`.
    pub key: String,
    pub label: String,
    pub kind: FieldKindDto,
    pub required: bool,
    /// `null` when the field claims no platform role.
    pub role: Option<FieldRoleDto>,
    /// Whether the field earns a column on the environments table.
    pub in_table: bool,
    /// Whether the field is shown on the environment detail page.
    pub in_detail: bool,
    pub help: Option<String>,
}

impl From<sdk_plugin::FieldDesc> for FieldDescDto {
    fn from(f: sdk_plugin::FieldDesc) -> Self {
        Self {
            key: f.key,
            label: f.label,
            kind: f.kind.into(),
            required: f.required,
            role: f.role.map(FieldRoleDto::from),
            in_table: f.in_table,
            in_detail: f.in_detail,
            help: f.help,
        }
    }
}

/// One product plugin this deployment has registered.
///
/// `credential_schema` renders the environment credential form;
/// `observed_schema` describes what observing an environment of this product
/// can yield, and which of those values claim a platform role. Tasks 21-22
/// render both.
///
/// Note what is **not** here: no failure class, no health vocabulary, and no
/// endpoint or credential value of any kind. A plugin's schemas are
/// declarations, and `observed_schema` cannot legally declare a secret field
/// at all — the SDK rejects that at registration, so nothing secret can reach
/// this response by construction.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ProductPluginDto {
    /// The full GTS instance id, byte-identical to what
    /// `qa_products.plugin_instance_id` stores. Write it back onto a product
    /// verbatim — it is not a display name and must not be transformed.
    pub instance_id: String,
    /// From the plugin's GTS instance `vendor` property; `null` if the
    /// instance declares none. A deployment running several product plugins
    /// selects between them on this string.
    pub vendor: Option<String>,
    pub credential_schema: Vec<FieldDescDto>,
    pub observed_schema: Vec<FieldDescDto>,
}

impl From<RegisteredProductPlugin> for ProductPluginDto {
    fn from(p: RegisteredProductPlugin) -> Self {
        Self {
            instance_id: p.instance_id,
            vendor: p.vendor,
            credential_schema: p
                .credential_schema
                .into_iter()
                .map(FieldDescDto::from)
                .collect(),
            observed_schema: p
                .observed_schema
                .into_iter()
                .map(FieldDescDto::from)
                .collect(),
        }
    }
}

#[cfg(test)]
mod product_plugin_dto_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::domain::service::RegisteredProductPlugin;

    fn field(
        key: &str,
        kind: sdk_plugin::FieldKind,
        role: Option<sdk_plugin::FieldRole>,
    ) -> sdk_plugin::FieldDesc {
        sdk_plugin::FieldDesc {
            key: key.to_owned(),
            label: "Label".to_owned(),
            kind,
            required: true,
            role,
            in_table: true,
            in_detail: false,
            help: Some("help".to_owned()),
        }
    }

    /// Every `FieldKind` and `FieldRole` variant must serialise to the SAME
    /// wire string the SDK's own `#[serde(rename_all = "snake_case")]`
    /// produces.
    ///
    /// This is the one property a mirror type can silently get wrong: the
    /// mirror compiles, the endpoint answers, and the UI receives a spelling
    /// it does not recognise for exactly one variant. Comparing against the
    /// SDK type's own `serde_json` output rather than against hand-typed
    /// strings means a rename in the SDK breaks this test instead of the UI.
    #[test]
    fn every_kind_and_role_spells_the_same_on_the_wire_as_the_sdk_type() {
        use sdk_plugin::{FieldKind as K, FieldRole as R};

        for kind in [
            K::Text,
            K::Url,
            K::Int,
            K::Bool,
            K::Enum,
            K::Secret,
            K::MultilineSecret,
        ] {
            assert_eq!(
                serde_json::to_value(FieldKindDto::from(kind)).unwrap(),
                serde_json::to_value(kind).unwrap(),
                "FieldKindDto must spell {kind:?} exactly as the SDK does"
            );
        }

        for role in [R::Version, R::Build, R::BaseUrl, R::Namespace] {
            assert_eq!(
                serde_json::to_value(FieldRoleDto::from(role)).unwrap(),
                serde_json::to_value(role).unwrap(),
                "FieldRoleDto must spell {role:?} exactly as the SDK does"
            );
        }
    }

    /// The response's field names, pinned. Tasks 21-22 read these.
    #[allow(unknown_lints, de0901_gts_string_pattern)] // deliberately malformed:
    // these fixtures pin how a plugin instance id is CARRIED on the wire, not
    // that it parses. `gts.a~b.c._.d.v1` / `gts.a.b.v1~c.d.v1` are exactly the
    // shapes `GtsOps::parse_id` rejects, which is the point. Same treatment as
    // `types-registry`'s `in_memory_repo` fixtures.
    #[test]
    fn the_wire_shape_is_the_documented_one() {
        let dto = ProductPluginDto::from(RegisteredProductPlugin {
            instance_id: "gts.a~b.c._.d.v1".to_owned(),
            vendor: Some("virtuozzo-vhp".to_owned()),
            credential_schema: vec![field(
                "kubeconfig",
                sdk_plugin::FieldKind::MultilineSecret,
                None,
            )],
            observed_schema: vec![field(
                "platformVersion",
                sdk_plugin::FieldKind::Text,
                Some(sdk_plugin::FieldRole::Version),
            )],
        });

        let json = serde_json::to_value(&dto).unwrap();

        assert_eq!(json["instance_id"], "gts.a~b.c._.d.v1");
        assert_eq!(json["vendor"], "virtuozzo-vhp");
        assert_eq!(json["credential_schema"][0]["key"], "kubeconfig");
        assert_eq!(json["credential_schema"][0]["kind"], "multiline_secret");
        assert_eq!(
            json["credential_schema"][0]["role"],
            serde_json::Value::Null
        );
        assert_eq!(json["observed_schema"][0]["key"], "platformVersion");
        assert_eq!(json["observed_schema"][0]["role"], "version");
        assert_eq!(json["observed_schema"][0]["in_table"], true);
        assert_eq!(json["observed_schema"][0]["in_detail"], false);
        assert_eq!(json["observed_schema"][0]["required"], true);
        assert_eq!(json["observed_schema"][0]["help"], "help");
    }

    /// A plugin declaring no vendor serialises `vendor: null`, not a missing
    /// key or an empty string: the UI distinguishes "no vendor declared" from
    /// a vendor named "".
    #[allow(unknown_lints, de0901_gts_string_pattern)] // deliberately malformed:
    // these fixtures pin how a plugin instance id is CARRIED on the wire, not
    // that it parses. `gts.a~b.c._.d.v1` / `gts.a.b.v1~c.d.v1` are exactly the
    // shapes `GtsOps::parse_id` rejects, which is the point. Same treatment as
    // `types-registry`'s `in_memory_repo` fixtures.
    #[test]
    fn an_absent_vendor_is_null() {
        let dto = ProductPluginDto::from(RegisteredProductPlugin {
            instance_id: "gts.a~b.c._.d.v1".to_owned(),
            vendor: None,
            credential_schema: vec![],
            observed_schema: vec![],
        });

        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["vendor"], serde_json::Value::Null);
        assert!(
            json.as_object().unwrap().contains_key("vendor"),
            "the key must be present and null, not omitted"
        );
    }

    /// **Finding FW-1's caller-facing half.** A create body that omits
    /// `plugin_instance_id` is refused here, with the message that says where
    /// a valid value comes from.
    ///
    /// The domain type cannot express the omission at all
    /// (`sdk::NewProduct::plugin_instance_id` is a plain `String` since Task
    /// 20), so this conversion is the only place the wire's `Option` can be
    /// answered — and it matters that the answer is good, because the shipped
    /// UI cannot send the field until Task 22 and so this is the error a real
    /// caller meets.
    #[test]
    fn the_wire_requires_a_plugin_and_says_where_to_find_one() {
        let req = CreateProductReq {
            name: "vhp".to_owned(),
            key: "VHP".to_owned(),
            description: "d".to_owned(),
            folder: None,
            plugin_instance_id: None,
        };

        let err = sdk::NewProduct::try_from(req)
            .expect_err("a product that names no plugin must be refused (D6)");

        let DomainError::Validation { field, message } = err else {
            panic!("expected a validation error, got {err:?}");
        };
        assert_eq!(field, "plugin_instance_id");
        assert!(
            message.contains("GET /qa/v1/product-plugins"),
            "the operator must be told where a valid value comes from: {message}"
        );
    }

    /// The accepting side: a named plugin survives the conversion verbatim.
    ///
    /// Verbatim matters — the id is handed to `ClientScope::gts_id` unchanged,
    /// so any transformation here would resolve to nothing and read as "no
    /// such plugin".
    #[test]
    fn a_named_plugin_reaches_the_domain_model_unchanged() {
        let id = "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1";
        let req = CreateProductReq {
            name: "vhp".to_owned(),
            key: "VHP".to_owned(),
            description: "d".to_owned(),
            folder: None,
            plugin_instance_id: Some(id.to_owned()),
        };

        let new = sdk::NewProduct::try_from(req).expect("a named plugin must be accepted");
        assert_eq!(new.plugin_instance_id, id);
    }
}
