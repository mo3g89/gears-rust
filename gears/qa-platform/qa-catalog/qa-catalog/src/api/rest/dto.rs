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

use crate::domain::error::DomainError;

// ==================== Test repository DTOs ====================

/// REST DTO for a registered git test repository.
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
    /// Reference to the access credential in credstore. The secret material
    /// itself is never returned over this or any other API. `null` = public
    /// repository.
    pub credential_ref: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_synced_at: Option<OffsetDateTime>,
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
            credential_ref: r.credential_ref,
            last_synced_at: r.last_synced_at,
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
            exclusive: p.exclusive,
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
    /// Durable short code (legacy `Product::key`).
    pub key: String,
    pub description: String,
    pub folder: Option<String>,
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
}

/// REST DTO for `PUT /qa/v1/products/{id}` — full replace of the product's
/// mutable fields (an absent `folder` moves the product back to the root).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateProductReq {
    pub name: String,
    pub key: String,
    pub description: String,
    pub folder: Option<String>,
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

    fn repo() -> sdk::TestRepository {
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
            sync_error: Some("boom".to_owned()),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn test_repository_dto_preserves_all_fields() {
        let r = repo();
        let dto = TestRepositoryDto::from(r.clone());
        assert_eq!(dto.id, r.id);
        assert_eq!(dto.product_id, r.product_id);
        assert_eq!(dto.name, r.name);
        assert_eq!(dto.url, r.url);
        assert_eq!(dto.default_branch, r.default_branch);
        assert_eq!(dto.content_root, r.content_root);
        assert_eq!(dto.credential_ref, r.credential_ref);
        assert_eq!(dto.last_synced_at, r.last_synced_at);
        assert_eq!(dto.sync_error, r.sync_error);
        assert_eq!(dto.created_at, r.created_at);
        assert_eq!(dto.updated_at, r.updated_at);
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

    #[test]
    fn plan_dto_preserves_exclusive_tri_state() {
        for exclusive in [None, Some(true), Some(false)] {
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
            assert_eq!(dto.exclusive, exclusive, "tri-state must survive");
            assert_eq!(dto.repo_id, plan.repo_id);
            assert_eq!(dto.test_files, plan.test_files);
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
    /// field mandatory on write, because legacy's `CustomPlanTest::plan_id` is
    /// non-optional and optional-on-write was itself the divergence.
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
