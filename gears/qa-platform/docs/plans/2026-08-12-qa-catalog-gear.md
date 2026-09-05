# qa-catalog Gear Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `qa-catalog` gear (DECOMPOSITION feature 2.2, `cpt-cf-qa-feature-catalog`): test repositories with credentialed sync + branch cache, plan discovery (`plan.yaml`), TEST_META parsing with preserved semantics, custom plans, products/versions, ephemeral test bundles, SSH key metadata.

**Architecture:** Standard ToolKit DDD-light gear pair under `gears/qa-platform/qa-catalog/`, structurally identical to the qa-environments gear (see sibling plan `2026-08-12-qa-environments-gear.md`; canonical ToolKit reference remains `examples/toolkit/users-info/`). The TDD core is the two parsers (`plan.yaml`, TEST_META) whose semantics are frozen contracts (PRD `cpt-cf-qa-fr-catalog-plan-discovery`, `cpt-cf-qa-fr-catalog-test-meta`). Repo sync and bundle blobs sit behind domain ports (`RepoSyncPort`, `BundleStore`) with the p1 implementations noted below.

**Tech Stack:** Rust, ToolKit stack (same as qa-environments), `serde_yaml` for plan.yaml, `regex` for TEST_META text scanning, `flate2`+`tar` for bundles.

**Specs:** `gears/qa-platform/docs/PRD.md` §5.1, `DESIGN.md` §3.2 qa-catalog + §3.7, `DECOMPOSITION.md` 2.2.

## ⚠️ Two flagged decisions (resolve with the team before Tasks 9–10)

1. **Git sync engine.** No git crate exists in the workspace. Direct external access from a gear's infra integration adapter has clear in-repo precedent (file-storage → S3 in `src/infra/backend/s3.rs`, oidc-authn-plugin → IdP, chat-engine → webhooks) under the GEARS.md rule "only integration/adapters talk to external components" — the gix engine in `infra/git/` is exactly that category, so no exception is needed, only a recorded decision. **This plan adds `gix`** (pure-Rust git; follow `guidelines/DEPENDENCIES.md` — license check, `deny.toml`) with ADR-0005 recording the choice (direct gix vs. git-over-oagw custom transport vs. shell-out) and noting a revisit if oagw ever grows git smart-HTTP tunneling for centralized credential injection.
2. **Bundle blob storage.** `file-storage-sdk`'s `FileStorageClientV1` is currently a placeholder trait with no operations. **This plan stores bundle blobs on the gear-local filesystem behind the `BundleStore` port**; the file-storage adapter is a follow-up when its P1 ops land (tracked in DECOMPOSITION 2.2 as a convergence note — update it when executing Task 10).

---

## File map

```
gears/qa-platform/qa-catalog/
├── qa-catalog-sdk/
│   ├── Cargo.toml
│   └── src/{lib,models,errors,client}.rs
└── qa-catalog/
    ├── Cargo.toml
    └── src/
        ├── lib.rs / config.rs / gear.rs
        ├── domain/
        │   ├── mod.rs / error.rs
        │   ├── parsing/
        │   │   ├── mod.rs
        │   │   ├── plan_yaml.rs      ← TDD core 1
        │   │   └── test_meta.rs      ← TDD core 2
        │   ├── ports/{mod,repo_sync,bundle_store}.rs
        │   ├── service/{mod,repos,plans,custom_plans,products,bundles,ssh_keys}.rs
        │   ├── repos/{mod,test_repos_repo,custom_plans_repo,products_repo,bundles_repo,ssh_keys_repo}.rs
        │   └── local_client/{mod,client}.rs
        ├── infra/
        │   ├── mod.rs
        │   ├── git/{mod,gix_sync}.rs
        │   ├── bundle_store/{mod,local_fs}.rs
        │   └── storage/  (entity/, mapper.rs, *_sea_repo.rs, migrations/)
        └── api/rest/  (dto.rs, error.rs, handlers/, routes/)
Modify: Cargo.toml (workspace), apps/cf-gears-example-server/{Cargo.toml,src/registered_gears.rs}
```

---

### Task 1: Scaffold crates (mirror qa-environments Task 1)

**Files:** `qa-catalog-sdk/Cargo.toml`, `qa-catalog-sdk/src/lib.rs`, `qa-catalog/Cargo.toml`, `qa-catalog/src/lib.rs`; modify root `Cargo.toml` members.

- [ ] **Step 1:** Create both Cargo.tomls. Take the qa-environments pair as the literal starting point (they were written first and compile), change the names/description, and add to the gear crate:

```toml
serde_yaml = { workspace = true }
regex = { workspace = true }
flate2 = { workspace = true }
tar = { workspace = true }
credstore-sdk = { workspace = true }   # ssh-key material by reference
```

(Verify each is in `[workspace.dependencies]`; add there first if missing, following `guidelines/DEPENDENCIES.md`. `serde_yaml` and `regex` are common; `flate2`/`tar` may need adding.)

- [ ] **Step 2:** Add `"gears/qa-platform/qa-catalog/qa-catalog-sdk"` and `"gears/qa-platform/qa-catalog/qa-catalog"` to workspace members.

- [ ] **Step 3:** `cargo build -p qa-catalog-sdk -p qa-catalog` → success.

- [ ] **Step 4:** Commit: `feat(qa-catalog): scaffold sdk and gear crates`.

---

### Task 2: SDK models, errors, client trait

**Files:** `qa-catalog-sdk/src/{models,errors,client,lib}.rs`

- [ ] **Step 1: models.rs** (no serde/utoipa — contract purity):

```rust
use time::OffsetDateTime;
use uuid::Uuid;

/// Registered git test repository.
#[derive(Clone, Debug, PartialEq)]
pub struct TestRepository {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    /// Subdirectory within the repo that contains test content ("" = root).
    pub content_root: String,
    /// credstore reference for the access credential (SSH key or token). None = public repo.
    pub credential_ref: Option<String>,
    pub last_synced_at: Option<OffsetDateTime>,
    pub sync_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewTestRepository {
    pub name: String,
    pub url: String,
    pub default_branch: String,
    pub content_root: String,
    pub credential_ref: Option<String>,
}

/// Three-state exclusivity: None = inherit (NOT the same as Some(false)).
/// This distinction is load-bearing — see PRD cpt-cf-qa-fr-runs-exclusivity.
pub type ExclusiveFlag = Option<bool>;

/// A plan discovered from a repository's plan.yaml (not persisted — materialized on read).
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub repo_id: Uuid,
    pub branch: String,
    /// Path of the plan.yaml within the content root.
    pub path: String,
    pub name: String,
    pub test_files: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub tags: Vec<String>,
    pub exclusive: ExclusiveFlag,
}

/// Parsed TEST_META for one test file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestFileMeta {
    pub path: String,
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub exclusive: ExclusiveFlag,
    /// JIRA issue keys referenced by the meta block (e.g. "VHP-123").
    pub bugs: Vec<String>,
}

/// User-composed persisted plan.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomPlan {
    pub id: Uuid,
    pub name: String,
    /// (repo_id, file path) pairs — may span repositories.
    pub files: Vec<(Uuid, String)>,
    pub tags: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewCustomPlan {
    pub name: String,
    pub files: Vec<(Uuid, String)>,
    pub tags: Vec<String>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Product {
    pub id: Uuid,
    pub name: String,
    pub folder: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProductVersion {
    pub id: Uuid,
    pub product_id: Uuid,
    pub version: String,
    /// repo_id → branch mapping selecting test content for this version.
    pub repo_branches: Vec<(Uuid, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SshKey {
    pub id: Uuid,
    pub name: String,
    pub credstore_ref: String,
    pub fingerprint: String,
    pub created_at: OffsetDateTime,
}

/// Descriptor of a built ephemeral bundle (blob lives in the bundle store).
#[derive(Clone, Debug, PartialEq)]
pub struct TestBundle {
    pub id: Uuid,
    /// Opaque reference into the bundle store (local path or file-storage ref).
    pub storage_ref: String,
    pub checksum_sha256: String,
    pub size_bytes: u64,
    pub expires_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
}

/// What a bundle should contain — resolved by qa-runs at launch time.
#[derive(Clone, Debug, PartialEq)]
pub struct BundleRequest {
    pub repo_id: Uuid,
    pub branch: String,
    /// Files to include (paths under content_root). Empty = whole content root.
    pub files: Vec<String>,
}
```

- [ ] **Step 2: errors.rs** — same canonical re-export pattern as qa-environments-sdk (`pub use toolkit_canonical_errors::CanonicalError as QaCatalogError;`, matching users-info-sdk's exact re-export style).

- [ ] **Step 3: client.rs** — object-safe `QaCatalogClientV1`:

```rust
use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::QaCatalogError;
use crate::models::{
    BundleRequest, CustomPlan, NewCustomPlan, NewTestRepository, Plan, Product, ProductVersion,
    SshKey, TestBundle, TestFileMeta, TestRepository,
};

/// Object-safe client for the qa-catalog gear (Version 1).
/// Primary consumer: qa-runs (plan resolution, TEST_META aggregation input,
/// bundle creation at launch).
#[async_trait]
pub trait QaCatalogClientV1: Send + Sync {
    // Repositories
    async fn list_repos(&self, ctx: &SecurityContext) -> Result<Vec<TestRepository>, QaCatalogError>;
    async fn get_repo(&self, ctx: &SecurityContext, id: Uuid) -> Result<TestRepository, QaCatalogError>;
    async fn create_repo(&self, ctx: &SecurityContext, new: NewTestRepository) -> Result<TestRepository, QaCatalogError>;
    async fn delete_repo(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaCatalogError>;
    /// Trigger a sync now; returns when the sync completes or fails.
    async fn sync_repo(&self, ctx: &SecurityContext, id: Uuid) -> Result<TestRepository, QaCatalogError>;
    /// Cached branch names for the repo (refreshed by sync / branch-cache task).
    async fn list_branches(&self, ctx: &SecurityContext, id: Uuid) -> Result<Vec<String>, QaCatalogError>;

    // Plans (discovered) + metadata
    async fn list_plans(&self, ctx: &SecurityContext, repo_id: Uuid, branch: &str) -> Result<Vec<Plan>, QaCatalogError>;
    async fn get_plan(&self, ctx: &SecurityContext, repo_id: Uuid, branch: &str, path: &str) -> Result<Plan, QaCatalogError>;
    /// Parsed TEST_META for the given files (used by qa-runs exclusivity OR-aggregation).
    async fn get_test_meta(&self, ctx: &SecurityContext, repo_id: Uuid, branch: &str, files: &[String]) -> Result<Vec<TestFileMeta>, QaCatalogError>;

    // Custom plans
    async fn list_custom_plans(&self, ctx: &SecurityContext) -> Result<Vec<CustomPlan>, QaCatalogError>;
    async fn get_custom_plan(&self, ctx: &SecurityContext, id: Uuid) -> Result<CustomPlan, QaCatalogError>;
    async fn create_custom_plan(&self, ctx: &SecurityContext, new: NewCustomPlan) -> Result<CustomPlan, QaCatalogError>;
    async fn update_custom_plan(&self, ctx: &SecurityContext, id: Uuid, new: NewCustomPlan) -> Result<CustomPlan, QaCatalogError>;
    async fn delete_custom_plan(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaCatalogError>;

    // Products
    async fn list_products(&self, ctx: &SecurityContext) -> Result<Vec<Product>, QaCatalogError>;
    async fn create_product(&self, ctx: &SecurityContext, name: String, folder: Option<String>) -> Result<Product, QaCatalogError>;
    async fn list_versions(&self, ctx: &SecurityContext, product_id: Uuid) -> Result<Vec<ProductVersion>, QaCatalogError>;
    async fn upsert_version(&self, ctx: &SecurityContext, version: ProductVersion) -> Result<ProductVersion, QaCatalogError>;

    // SSH keys
    async fn list_ssh_keys(&self, ctx: &SecurityContext) -> Result<Vec<SshKey>, QaCatalogError>;
    async fn create_ssh_key(&self, ctx: &SecurityContext, name: String, private_key_pem: String) -> Result<SshKey, QaCatalogError>;
    async fn delete_ssh_key(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaCatalogError>;

    // Bundles
    /// Build an ephemeral bundle for a run (called by qa-runs at launch).
    async fn create_bundle(&self, ctx: &SecurityContext, req: BundleRequest) -> Result<TestBundle, QaCatalogError>;
    /// Fetch bundle bytes for download serving. (Streaming variant can come later.)
    async fn get_bundle_content(&self, ctx: &SecurityContext, id: Uuid) -> Result<Vec<u8>, QaCatalogError>;
}
```

Note `create_ssh_key` takes PEM material and immediately stores it in credstore, returning only the reference + fingerprint — material never persists in this gear.

- [ ] **Step 4:** lib.rs re-exports; `cargo build -p qa-catalog-sdk`; commit `feat(qa-catalog): SDK models and client trait`.

---

### Task 3: TDD core 1 — plan.yaml parser

**Files:** `qa-catalog/src/domain/{mod.rs,parsing/mod.rs,parsing/plan_yaml.rs}`; wire `pub mod domain;` in lib.rs.

- [ ] **Step 1: Write failing tests** (inline `#[cfg(test)]` in plan_yaml.rs):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_plan() {
        let yaml = "name: smoke\ntests:\n  - test_a.py\n  - test_b.py\n";
        let p = parse_plan_yaml(yaml).unwrap();
        assert_eq!(p.name, "smoke");
        assert_eq!(p.test_files, vec!["test_a.py", "test_b.py"]);
        assert_eq!(p.timeout_seconds, None);
        assert_eq!(p.tags, Vec::<String>::new());
        assert_eq!(p.exclusive, None, "absent exclusive must be None (inherit), not Some(false)");
    }

    #[test]
    fn parses_full_plan() {
        let yaml = r"
name: upgrade
timeout_seconds: 7200
tags: [e2e, destructive]
exclusive: true
tests:
  - upgrade/test_upgrade.py
";
        let p = parse_plan_yaml(yaml).unwrap();
        assert_eq!(p.timeout_seconds, Some(7200));
        assert_eq!(p.tags, vec!["e2e", "destructive"]);
        assert_eq!(p.exclusive, Some(true));
    }

    #[test]
    fn explicit_false_is_not_inherit() {
        let p = parse_plan_yaml("name: x\nexclusive: false\ntests: [a.py]\n").unwrap();
        assert_eq!(p.exclusive, Some(false), "explicit false overrides TEST_META true downstream");
    }

    #[test]
    fn missing_name_is_error() {
        assert!(parse_plan_yaml("tests: [a.py]\n").is_err());
    }

    #[test]
    fn missing_tests_is_error() {
        assert!(parse_plan_yaml("name: x\n").is_err());
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        // Existing repos may carry extra keys; discovery must not reject them.
        let p = parse_plan_yaml("name: x\nowner: qa-team\ntests: [a.py]\n").unwrap();
        assert_eq!(p.name, "x");
    }
}
```

- [ ] **Step 2:** Run `cargo test -p qa-catalog plan_yaml` → compile failure (function missing).

- [ ] **Step 3: Implement:**

```rust
//! plan.yaml parsing — frozen contract with test authors
//! (PRD cpt-cf-qa-fr-catalog-plan-discovery).

use serde::Deserialize;

use crate::domain::error::DomainError;

/// Raw deserialization target. Unknown keys tolerated by serde default behavior.
#[derive(Debug, Deserialize)]
struct RawPlanYaml {
    name: String,
    tests: Vec<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    tags: Vec<String>,
    /// Three-state: absent → None (inherit).
    #[serde(default)]
    exclusive: Option<bool>,
}

/// Parsed plan definition, pre-SDK (repo/branch/path are attached by the caller).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedPlan {
    pub name: String,
    pub test_files: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub tags: Vec<String>,
    pub exclusive: Option<bool>,
}

pub fn parse_plan_yaml(content: &str) -> Result<ParsedPlan, DomainError> {
    let raw: RawPlanYaml =
        serde_yaml::from_str(content).map_err(|e| DomainError::PlanYamlInvalid {
            message: e.to_string(),
        })?;
    if raw.tests.is_empty() {
        return Err(DomainError::PlanYamlInvalid {
            message: "plan has no tests".into(),
        });
    }
    Ok(ParsedPlan {
        name: raw.name,
        test_files: raw.tests,
        timeout_seconds: raw.timeout_seconds,
        tags: raw.tags,
        exclusive: raw.exclusive,
    })
}
```

(`ParsedPlan` needs `#[domain_model]` — add `use toolkit_macros::domain_model;` and the attribute. Create `domain/error.rs` now with at least `PlanYamlInvalid { message }`, `TestMetaInvalid { message }`, plus the standard NotFound/Validation/Forbidden/Database variants copied structurally from the qa-environments plan Task 5.)

- [ ] **Step 4:** `cargo test -p qa-catalog plan_yaml` → 6 PASS.

- [ ] **Step 5:** Commit `feat(qa-catalog): plan.yaml parser with three-state exclusive semantics`.

---

### Task 4: TDD core 2 — TEST_META parser

**Files:** `qa-catalog/src/domain/parsing/test_meta.rs`

The semantics being frozen (source: testrunner `docs/guides/exclusive-runs-and-the-queue.md`):
- Parsed **as text, never executed**.
- Python (`True`/`False`) and JSON (`true`/`false`) literals both accepted.
- **Any** occurrence of `"exclusive": true` anywhere in the file counts, and any true occurrence **wins over** any false occurrence (deliberate: a stale commented-out `False` must not disarm a real declaration).
- If only false occurrences exist → `Some(false)`. If none → `None` (inherit).
- `title`, `tags`, and bug keys extracted from the TEST_META block when present.

- [ ] **Step 1: Write failing tests:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_meta_yields_default() {
        let m = parse_test_meta("import pytest\n\ndef test_x():\n    pass\n");
        assert_eq!(m.exclusive, None);
        assert_eq!(m.title, None);
        assert!(m.tags.is_empty());
        assert!(m.bugs.is_empty());
    }

    #[test]
    fn python_literal_true() {
        let src = r#"
TEST_META = {
    "title": "Cluster upgrade",
    "tags": ["e2e", "destructive"],
    "exclusive": True,
}
"#;
        let m = parse_test_meta(src);
        assert_eq!(m.exclusive, Some(true));
        assert_eq!(m.title.as_deref(), Some("Cluster upgrade"));
        assert_eq!(m.tags, vec!["e2e", "destructive"]);
    }

    #[test]
    fn json_literal_true() {
        let m = parse_test_meta("TEST_META = { \"exclusive\": true }");
        assert_eq!(m.exclusive, Some(true));
    }

    #[test]
    fn explicit_false_only() {
        let m = parse_test_meta("TEST_META = { \"exclusive\": False }");
        assert_eq!(m.exclusive, Some(false));
    }

    #[test]
    fn any_true_occurrence_wins_over_false() {
        // A commented-out False must not disarm; a stray True anywhere arms.
        let src = r#"
# was: "exclusive": False
TEST_META = { "exclusive": False }
# docs say to use "exclusive": True for reboot tests
"#;
        let m = parse_test_meta(src);
        assert_eq!(m.exclusive, Some(true), "any true occurrence, even in a comment, wins");
    }

    #[test]
    fn true_in_docstring_counts() {
        // Deliberate contract cost: prose mentioning it arms the file.
        let src = "\"\"\"Set \"exclusive\": True in TEST_META for destructive suites.\"\"\"";
        assert_eq!(parse_test_meta(src).exclusive, Some(true));
    }

    #[test]
    fn extracts_bug_links() {
        let src = r#"
TEST_META = {
    "title": "Storage failover",
    "bugs": ["VHP-2618", "VHP-101"],
}
"#;
        let m = parse_test_meta(src);
        assert_eq!(m.bugs, vec!["VHP-2618", "VHP-101"]);
    }
}
```

**Before finalizing the `bugs`/`title`/`tags` extraction tests, open the legacy parser** (`testrunner/manager/src/services/test_meta.rs`) and mirror its exact field names and bug-link convention (key name may be `bugs`, `jira`, or similar; the block-extraction regex bounds matter). Adjust tests to the legacy truth — the contract is "what the current system accepts", not this plan's guess. The exclusivity rules above are documented behavior and are non-negotiable.

- [ ] **Step 2:** Run → compile failure.

- [ ] **Step 3: Implement** with layered extraction:

```rust
//! TEST_META text parsing — frozen contract (PRD cpt-cf-qa-fr-catalog-test-meta).
//! The block is read as text and NEVER executed.

use regex::Regex;
use std::sync::OnceLock;
use toolkit_macros::domain_model;

/// Parsed metadata for one test file (pre-SDK; path attached by caller).
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedTestMeta {
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub exclusive: Option<bool>,
    pub bugs: Vec<String>,
}

fn exclusive_true_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""exclusive"\s*:\s*(?:True|true)"#).expect("static regex"))
}

fn exclusive_false_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""exclusive"\s*:\s*(?:False|false)"#).expect("static regex"))
}

pub fn parse_test_meta(source: &str) -> ParsedTestMeta {
    // Exclusivity: whole-file scan, any-true-wins (deliberate, see module doc).
    let exclusive = if exclusive_true_re().is_match(source) {
        Some(true)
    } else if exclusive_false_re().is_match(source) {
        Some(false)
    } else {
        None
    };

    // Title/tags/bugs: extracted from the TEST_META block only.
    let block = extract_meta_block(source);
    let (title, tags, bugs) = block
        .map(|b| (extract_string(&b, "title"), extract_string_list(&b, "tags"), extract_string_list(&b, "bugs")))
        .unwrap_or((None, Vec::new(), Vec::new()));

    ParsedTestMeta { title, tags, exclusive, bugs }
}

/// Grab the text between `TEST_META = {` and its closing `}` (first balanced brace).
fn extract_meta_block(source: &str) -> Option<String> {
    let start = source.find("TEST_META")?;
    let brace = source[start..].find('{')? + start;
    let mut depth = 0usize;
    for (i, ch) in source[brace..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(source[brace..=brace + i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_string(block: &str, key: &str) -> Option<String> {
    let re = Regex::new(&format!(r#""{key}"\s*:\s*"([^"]*)""#)).ok()?;
    re.captures(block).map(|c| c[1].to_string())
}

fn extract_string_list(block: &str, key: &str) -> Vec<String> {
    let Some(re) = Regex::new(&format!(r#""{key}"\s*:\s*\[([^\]]*)\]"#)).ok() else {
        return Vec::new();
    };
    let Some(caps) = re.captures(block) else {
        return Vec::new();
    };
    Regex::new(r#""([^"]*)""#)
        .map(|item_re| {
            item_re
                .captures_iter(&caps[1])
                .map(|c| c[1].to_string())
                .collect()
        })
        .unwrap_or_default()
}
```

(`expect` on the two static regexes: if the repo's no-expect lint flags it, switch to the `OnceLock` + `Result` pattern used elsewhere in the workspace — grep for `OnceLock<Regex>` to find the idiom.)

- [ ] **Step 4:** `cargo test -p qa-catalog test_meta` → all PASS.

- [ ] **Step 5:** Commit `feat(qa-catalog): TEST_META text parser preserving any-true-wins exclusivity`.

---

### Task 5: Domain ports — RepoSyncPort and BundleStore

**Files:** `qa-catalog/src/domain/ports/{mod,repo_sync,bundle_store}.rs`

- [ ] **Step 1: repo_sync.rs**

```rust
use async_trait::async_trait;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// Result of syncing one repository.
#[domain_model]
#[derive(Debug, Clone)]
pub struct SyncResult {
    pub branches: Vec<String>,
    /// Repo-relative paths of files under the content root, for the synced branch.
    pub head_commit: String,
}

/// Port abstracting the git engine. Implemented in infra (gix; see ADR-0005).
#[async_trait]
pub trait RepoSyncPort: Send + Sync {
    /// Clone-or-fetch the repo into the gear-local working area, checkout `branch`,
    /// and return branch inventory + head commit. `credential` is the resolved
    /// secret material (never logged), already fetched from credstore by the service.
    async fn sync(
        &self,
        url: &str,
        branch: &str,
        credential: Option<&str>,
        workdir: &std::path::Path,
    ) -> Result<SyncResult, DomainError>;

    /// List remote branches without a full sync (branch cache refresh).
    async fn list_remote_branches(
        &self,
        url: &str,
        credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError>;
}
```

- [ ] **Step 2: bundle_store.rs**

```rust
use async_trait::async_trait;

use crate::domain::error::DomainError;

/// Port abstracting bundle blob storage. p1 impl: gear-local filesystem.
/// file-storage adapter follows when FileStorageClientV1 gains operations.
#[async_trait]
pub trait BundleStore: Send + Sync {
    /// Store bundle bytes; returns an opaque storage_ref.
    async fn put(&self, bundle_id: uuid::Uuid, bytes: Vec<u8>) -> Result<String, DomainError>;
    async fn get(&self, storage_ref: &str) -> Result<Vec<u8>, DomainError>;
    async fn delete(&self, storage_ref: &str) -> Result<(), DomainError>;
}
```

- [ ] **Step 3:** Build, commit `feat(qa-catalog): repo-sync and bundle-store domain ports`.

---

### Task 6: Migration and entities

**Files:** `infra/storage/migrations/{mod,m20260812_000002_initial}.rs`, `infra/storage/entity/*.rs`

Same three-backend migration pattern as the qa-environments plan Task 6. Tables (DESIGN §3.7, all with `tenant_id`):

```sql
-- Postgres branch (write Sqlite/MySql translations as in qa-environments Task 6)
CREATE TABLE IF NOT EXISTS qa_test_repositories (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    url VARCHAR(1024) NOT NULL,
    default_branch VARCHAR(255) NOT NULL,
    content_root VARCHAR(1024) NOT NULL DEFAULT '',
    credential_ref VARCHAR(1024) NULL,
    last_synced_at TIMESTAMPTZ NULL,
    sync_error TEXT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_repos_tenant_name ON qa_test_repositories(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_repo_branches (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    repo_id UUID NOT NULL REFERENCES qa_test_repositories(id) ON DELETE CASCADE,
    name VARCHAR(512) NOT NULL,
    refreshed_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_branches_unique ON qa_repo_branches(repo_id, name);

CREATE TABLE IF NOT EXISTS qa_ssh_keys (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    credstore_ref VARCHAR(1024) NOT NULL,
    fingerprint VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_ssh_keys_tenant_name ON qa_ssh_keys(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_custom_plans (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    files JSONB NOT NULL DEFAULT '[]',
    tags JSONB NOT NULL DEFAULT '[]',
    timeout_seconds BIGINT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS qa_products (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    folder VARCHAR(255) NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_name ON qa_products(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_product_versions (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    product_id UUID NOT NULL REFERENCES qa_products(id) ON DELETE CASCADE,
    version VARCHAR(255) NOT NULL,
    repo_branches JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_versions_unique ON qa_product_versions(product_id, version);

CREATE TABLE IF NOT EXISTS qa_test_bundles (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    storage_ref VARCHAR(2048) NOT NULL,
    checksum_sha256 VARCHAR(64) NOT NULL,
    size_bytes BIGINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_bundles_expiry ON qa_test_bundles(expires_at);
```

- [ ] **Step 1:** Write the migration (all three backends) and one entity file per table with `#[derive(DeriveEntityModel, Scopable)]` + `#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]`, exactly like the qa-environments entities (JSONB columns → `pub files: Json`).

- [ ] **Step 2:** Build; commit `feat(qa-catalog): schema migration and entities` (with Task 7 if preferred as one build unit).

---

### Task 7: Repos traits + ORM impls + mapper

**Files:** `domain/repos/*.rs`, `infra/storage/{mapper.rs,*_sea_repo.rs}`

- [ ] **Step 1:** One repo trait per aggregate, `DBRunner`-generic like qa-environments Task 7 and users-info:
  - `TestReposRepository`: get/list/create/delete/update_sync_state (writes `last_synced_at`/`sync_error`), `replace_branches(repo_id, Vec<String>)`, `list_branches(repo_id)`
  - `CustomPlansRepository`: get/list/create/update/delete (files/tags as JSON columns, mapped to `Vec<(Uuid, String)>` in mapper)
  - `ProductsRepository`: products CRUD + versions list/upsert
  - `SshKeysRepository`: list/create/delete
  - `BundlesRepository`: create/get/delete_expired(now) → Vec<TestBundle> (returning deleted rows so the service can delete blobs from the store)

- [ ] **Step 2:** `mapper.rs` — entity↔SDK conversions including JSON round-trips:

```rust
pub fn custom_plan_files_from_json(v: &serde_json::Value) -> Vec<(uuid::Uuid, String)> {
    serde_json::from_value(v.clone()).unwrap_or_default()
}
pub fn custom_plan_files_to_json(files: &[(uuid::Uuid, String)]) -> serde_json::Value {
    serde_json::to_value(files).unwrap_or_else(|_| serde_json::json!([]))
}
// analogous helpers for tags and product-version repo_branches
```

- [ ] **Step 3:** ORM impls using the exact SecureORM idioms already established in the compiled qa-environments repos (that gear is now the nearest in-tree reference alongside users-info).

- [ ] **Step 4:** Build; commit `feat(qa-catalog): repositories and secure ORM implementations`.

---

### Task 8: Domain services

**Files:** `domain/service/{mod,repos,plans,custom_plans,products,bundles,ssh_keys}.rs`

Same `AppServices` + `resources`/`actions` + PEP structure as qa-environments Task 8. Resource types: `qa.test_repo`, `qa.plan`, `qa.custom_plan`, `qa.product`, `qa.ssh_key`, `qa.bundle`.

Service specifics beyond generic CRUD (each with the PEP → scope → repo skeleton):

- [ ] **Step 1: `repos.rs`** — `sync_repo`: resolve credential from credstore (`credential_ref` → `CredStoreClientV1::get`), call `RepoSyncPort::sync` into `workdir = cfg.repos_dir/<repo_id>`, then `replace_branches` + `update_sync_state` (success or error string). `list_branches` reads the cache table only.

- [ ] **Step 2: `plans.rs`** — `list_plans(repo_id, branch)`: walk the synced workdir's content root for `plan.yaml`/`*.plan.yaml` files (match the legacy discovery glob — check `testrunner/manager/src/services/plans.rs` for the exact pattern), parse each with `parse_plan_yaml`, attach repo/branch/path, skip-and-log invalid files (one bad plan must not hide the rest). `get_test_meta(files)`: read each file from the workdir, `parse_test_meta`, attach paths. Unsynced repo/branch → `DomainError::RepoNotSynced` (add the variant).

- [ ] **Step 3: `custom_plans.rs`, `products.rs`** — CRUD with `Validation` on empty names; version upsert keyed on (product_id, version).

- [ ] **Step 4: `ssh_keys.rs`** — `create`: compute the fingerprint (SHA256 of the public key derived from PEM — if deriving the public key requires a new dependency, store SHA256 of the PEM as the fingerprint and document it), write material to credstore under a generated `SecretRef`, persist metadata with the returned ref. `delete`: remove credstore secret first, then the row.

- [ ] **Step 5: `bundles.rs`** — `create_bundle(req)`: read the requested files from the synced workdir (whole content root when `files` is empty), build tar.gz in memory (`tar` + `flate2`), compute SHA256, `BundleStore::put`, persist descriptor with `expires_at = now + cfg.bundle_ttl`. `get_bundle_content(id)`: descriptor lookup (expired → NotFound) + `BundleStore::get`. `purge_expired()`: repo delete_expired + store deletes — called by the lifecycle task (Task 10).

- [ ] **Step 6:** Unit tests with mock ports (in-memory `RepoSyncPort` writing fixture files into a tempdir, in-memory `BundleStore` HashMap):

```text
- sync_repo_updates_branch_cache_and_timestamp
- sync_repo_records_error_string_on_engine_failure
- list_plans_parses_fixtures_and_skips_invalid (fixture dir: 2 valid plan.yaml + 1 broken → 2 plans)
- get_test_meta_attaches_paths
- create_bundle_roundtrip (create → get_bundle_content → untar → same fixture bytes; checksum matches)
- expired_bundle_is_not_served
- ssh_key_material_never_in_db (create key; assert row contains only credstore ref + fingerprint)
```

Write each in full.

- [ ] **Step 7:** `cargo test -p qa-catalog` → parser + service tests PASS. Commit `feat(qa-catalog): domain services with PEP, sync, discovery, and bundles`.

---

### Task 9: Infra adapters — gix sync engine and local-fs bundle store

**Files:** `infra/git/{mod,gix_sync}.rs`, `infra/bundle_store/{mod,local_fs}.rs`, ADR `gears/qa-platform/docs/ADR/0005-cpt-cf-qa-adr-git-egress.md`

- [ ] **Step 1: Write ADR-0005** (git sync engine) using the repo ADR template: context (git protocol vs. oagw's HTTP-centric egress; infra-adapter precedent — file-storage S3 backend, oidc-authn-plugin), options (gix direct in infra adapter / gix-over-oagw custom transport / shell-out git), decision gix-direct-in-adapter per precedent, consequences (deny.toml + license check for gix; JIRA/SMTP still via oagw; revisit if oagw grows git tunneling). Commit it. **Confirm flagged decision #2 (bundle storage) with the team if not already settled.**

- [ ] **Step 2:** Add `gix` to workspace deps per `guidelines/DEPENDENCIES.md` (license check against `deny.toml`; run `cargo deny check` after adding).

- [ ] **Step 3: `gix_sync.rs`** — implement `RepoSyncPort`: `gix::prepare_clone` / fetch-existing into `workdir`, checkout branch, enumerate remote branches from refs; map errors to `DomainError::SyncFailed { message }` (add variant). HTTPS token credential via gix's authentication hooks; SSH-key auth if gix supports it in the pinned version — otherwise document token-only for p1 in the ADR.

- [ ] **Step 4: `local_fs.rs`** — `BundleStore` over `cfg.bundles_dir`: `put` writes `<dir>/<bundle_id>.tar.gz` (storage_ref = that path), `get` reads, `delete` removes; fs errors → `DomainError::Database`? No — add `DomainError::Storage(String)` and map bundle-store errors there (update Task 8's error mapping accordingly).

- [ ] **Step 5:** Integration-style test behind the `integration` feature: sync a local fixture git repo (create one in a tempdir with `gix` itself in the test), list branches, discover plans end-to-end.

- [ ] **Step 6:** Build + test + commit `feat(qa-catalog): gix sync engine and local-fs bundle store (ADR-0005)`.

---

### Task 10: REST layer, lifecycle tasks, gear bootstrap, registration

**Files:** `api/rest/*` (dto/error/handlers/routes), `config.rs`, `gear.rs`, `domain/local_client/*`, server registration.

- [ ] **Step 1: config.rs**

```rust
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QaCatalogConfig {
    /// Working directory for synced repositories.
    pub repos_dir: String,
    /// Directory for bundle blobs (local-fs BundleStore).
    pub bundles_dir: String,
    /// Bundle time-to-live in seconds.
    pub bundle_ttl_seconds: u64,
    /// Branch cache refresh interval in seconds (0 disables the background task).
    pub branch_refresh_interval_seconds: u64,
}

impl Default for QaCatalogConfig {
    fn default() -> Self {
        Self {
            repos_dir: "./data/qa-catalog/repos".into(),
            bundles_dir: "./data/qa-catalog/bundles".into(),
            bundle_ttl_seconds: 3600,
            branch_refresh_interval_seconds: 900,
        }
    }
}
```

- [ ] **Step 2: REST endpoints** (OperationBuilder, `.authenticated()`, canonical errors; DTOs with serde+utoipa and full `From` impls, following the established qa-environments Task 9 shape):

| Method | Path | Handler |
|--------|------|---------|
| GET/POST | `/qa/v1/test-repos` | list/create |
| GET/DELETE | `/qa/v1/test-repos/{id}` | get/delete |
| POST | `/qa/v1/test-repos/{id}/sync` | sync now |
| GET | `/qa/v1/test-repos/{id}/branches` | cached branches |
| GET | `/qa/v1/plans?repo_id&branch` | discovered plans |
| GET/POST | `/qa/v1/custom-plans` + `/{id}` CRUD | custom plans |
| GET/POST | `/qa/v1/products` (+ `/qa/v1/products/{id}/versions` GET/PUT) | products/versions |
| GET | `/qa/v1/product-folders` | distinct folders |
| GET/POST/DELETE | `/qa/v1/ssh-keys` (+ `/{id}`) | keys (POST body: name + PEM; response: id/name/fingerprint/ref — never PEM) |
| GET | `/qa/v1/test-bundles/{id}` | bundle download (binary response `application/gzip`; check OperationBuilder's non-JSON response registration — see how file-parser or api-gateway serve binary/asset responses and mirror) |

- [ ] **Step 3: Lifecycle background tasks** — read `docs/toolkit_unified_system/08_lifecycle_stateful_tasks.md` first; register two cancellable tasks in the gear's stateful capability: branch-cache refresher (every `branch_refresh_interval_seconds`, iterate repos per tenant scope of a system context — copy the system-context idiom the docs prescribe for background jobs) and bundle GC (every 300s, `purge_expired`). Cluster-wide leader election is NOT needed here per DESIGN (idempotent refresh); note that in a comment.

- [ ] **Step 4: gear.rs** — `#[toolkit::gear(name = "qa-catalog", deps = [authz_resolver, credstore], capabilities = [db, rest, stateful])]` (verify the exact dep token for credstore from how other gears declare it — grep `deps = [` across `gears/`); init resolves `AuthZResolverClient` + `CredStoreClientV1` from ClientHub, builds ports (gix engine, local-fs store), services, registers `QaCatalogClientV1` local client (write all delegations).

- [ ] **Step 5:** Register in example server: extend the `qa-platform` feature with `dep:qa-catalog`, add `#[cfg(feature = "qa-platform")] use qa_catalog as _;`.

- [ ] **Step 6:** Full verify: `cargo build -p qa-catalog && cargo clippy -p qa-catalog -- -D warnings && cargo test -p qa-catalog && cargo gears lint --dylint`, then `cargo run -p cf-gears-example-server --features qa-platform -- --list-gears` shows both qa gears; `/openapi.json` contains `/qa/v1/test-repos`.

- [ ] **Step 7:** Tenant-scoping tests (port `test_support` pattern from qa-environments Task 12: repo/custom-plan/product invisible across tenants; PDP-deny on create).

- [ ] **Step 8:** Commit `feat(qa-catalog): REST layer, lifecycle tasks, gear bootstrap, server registration`.

---

## Plan self-review notes (already applied)

- **Spec coverage**: `fr-catalog-repos` → T2/6/7/8/9/10; `fr-catalog-branch-cache` → T7/8/10 (interval task); `fr-catalog-ssh-keys` → T2/6/8 (material in credstore only, test enforces); `fr-catalog-plan-discovery` → T3/8; `fr-catalog-test-meta` → T4; `fr-catalog-custom-plans` → T6–8/10; `fr-catalog-products` → T6–8/10; `fr-catalog-bundles` → T5/8/9/10 (GC = expiry requirement).
- **Deliberate scope notes**: bundle blobs on local fs behind the port (file-storage SDK has no ops yet — flagged decision #2); git egress exception ADR required before Task 9 (flagged decision #1); OData paging deferred on catalog collections for p1 (small collections; qa-runs/analytics are where OData matters — PRD's standard-conventions requirement is met at the analytics surface, revisit if catalog lists grow).
- **Legacy-truth checkpoints**: TEST_META field names/bug-key convention (Task 4 Step 1) and plan-file discovery glob (Task 8 Step 2) must be verified against `testrunner/manager/src/services/{test_meta,plans}.rs` before those tests are finalized.
- **Type consistency**: `ParsedPlan`/`ParsedTestMeta` (domain) vs SDK `Plan`/`TestFileMeta` — services attach repo/branch/path when converting; `ExclusiveFlag = Option<bool>` used consistently; `BundleRequest.files` empty = whole content root in both trait doc and service impl.
```
