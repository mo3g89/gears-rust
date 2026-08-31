# qa-catalog Legacy Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `qa-catalog` express legacy testrunner launch semantics — arbitrary-branch content reads, and products that own repositories — by adding multi-branch working copies, adding product parity fields, and deleting the `ProductVersion` concept legacy removed in VHP-319.

**Architecture:** One `gix` clone per repository holds objects and refs at `<repos_dir>/<repo_id>/git`; each branch's content is materialized as a **plain directory** (no `.git`, no index) at `<repos_dir>/<repo_id>/branches/<branch_dir>`. Branch worktrees are read-only content snapshots, which sidesteps `gix`'s single shared index. Freshness is an in-memory TTL cache keyed `(repo_id, branch)`; the launch path force-syncs past it. Products gain `product_key`/`description` and own repositories via a required `TestRepository.product_id`.

**Tech Stack:** Rust, `gix` (pure-Rust git), SeaORM + SecureORM, `sha2`, `tokio`, axum + `OperationBuilder`, ToolKit gear framework.

**Source spec:** [`../specs/2026-08-12-qa-platform-legacy-parity-design.md`](../specs/2026-08-12-qa-platform-legacy-parity-design.md)

**Out of scope:** the qa-runs launch path (spec §3.4). That gear is not scaffolded; §3.4 is an inherited constraint, not work here.

**How spec §3.5 (error handling) is covered.** Four of its five rows — repository
not found, plan missing on the chosen branch, unpinned multi-repo plan, and
branch-sync failure surfaced to a launch — are qa-runs launch-path concerns and
travel with §3.4. What lands here is the engine half: a branch absent from the
remote must fail cleanly rather than materialize an empty snapshot (Task 19,
Step 2), and an unmaterialized branch must fail rather than read as an empty
plan list (Task 17, Step 6). The existing `SyncFailed` → 503 mapping needs no
change. Task 20 Step 4 carries the remaining rows into the continuation prompt
so they are specified where they will be built.

**Resolved before planning:** spec §6 flagged the initial-migration assumption as unverified. It is now verified — `m20260812_000002_initial.rs` has a single commit, there are no deploy/helm artifacts for qa-platform, and qa-catalog is not registered in `apps/cf-gears-example-server/src/main.rs`. The migration has never executed anywhere, so editing it in place is safe. No drop-migration is needed.

---

## File Structure

**Phase A — de-risk the unproven primitive**

| File | Responsibility |
| --- | --- |
| `qa-catalog/tests/multi_branch_spike.rs` (create) | Integration test proving `gix` can materialize two branch trees from one clone into two separate directories. Gated behind the existing `integration` feature. |

**Phase B — product parity and `ProductVersion` removal**

| File | Change |
| --- | --- |
| `src/infra/storage/migrations/m20260812_000002_initial.rs` | Reorder DDL (products before repositories, for the FK), add `product_id` to `qa_test_repositories`, add `product_key`/`description` to `qa_products`, delete `qa_product_versions` |
| `src/infra/storage/entity/product.rs` | Add `product_key`, `description` |
| `src/infra/storage/entity/test_repository.rs` | Add `product_id` |
| `src/infra/storage/entity/product_version.rs` | Delete |
| `src/infra/storage/entity/mod.rs` | Drop the `product_version` module |
| `src/infra/storage/mapper.rs` | Map new fields; delete `product_version_to_sdk` |
| `src/infra/storage/products_sea_repo.rs` | Delete version ops; carry new product fields |
| `qa-catalog-sdk/src/models.rs` | `Product` gains `key`/`description`; `TestRepository` + `NewTestRepository` gain `product_id`; delete `ProductVersion` |
| `qa-catalog-sdk/src/client.rs` | Delete `list_versions` / `upsert_version` |
| `qa-catalog-sdk/src/lib.rs` | Drop the `ProductVersion` re-export |
| `src/domain/repos/products_repo.rs` | Delete version trait methods; new product fields |
| `src/domain/service/products.rs` | Delete version methods; accept new fields |
| `src/domain/service/repos.rs` | Require `product_id` on create/update; make `default_branch` mutable |
| `src/domain/local_client/client.rs` | Drop version op implementations |
| `src/domain/error.rs` | Drop version-specific error variants if any remain unused |
| `src/api/rest/dto.rs` | Product DTOs gain fields; repo DTOs gain `product_id`; delete version DTOs |
| `src/api/rest/handlers/products.rs`, `src/api/rest/routes/products.rs` | Delete version routes/handlers |
| `src/api/rest/error.rs` | Drop version error mappings |
| `src/domain/service/plans.rs` | Stamp `product_id` on discovered plans |
| `qa-catalog-sdk/src/models.rs` (`Plan`) | Add `product_id` |

**Phase C — multi-branch working copies**

| File | Change |
| --- | --- |
| `src/infra/git/layout.rs` (create) | `branch_dir_name`, `host_dir`, `branch_workdir` — pure path helpers, unit-tested |
| `src/infra/git/mod.rs` | Export `layout` |
| `src/infra/git/gix_sync.rs` | Bare host clone + materialize a branch tree into a target directory |
| `src/domain/service/sync_cache.rs` (create) | In-memory TTL freshness cache + two-tier lock registry |
| `src/domain/service/mod.rs` | Export `sync_cache` |
| `src/domain/service/repos.rs` | `sync` takes a branch; force-sync semantics; wire the cache |
| `src/domain/service/plans.rs` | `require_synced` + `content_root_dir` become branch-aware |
| `src/test_support.rs` | Sync-port doubles updated for the new call shape |

**Phase D — documents**

| File | Change |
| --- | --- |
| `docs/PRD.md`, `docs/DESIGN.md`, `docs/DECOMPOSITION.md`, `docs/plans/2026-08-12-qa-platform-continuation-prompt.md` | Per spec §5 |

---

## Phase A — De-risk the gix primitive

### Task 1: Prove gix can materialize two branches from one clone

Spec §6 lists this as the plan's only unproven assumption. If it fails, the fallback is clone-per-branch (spec §2, rejected-alternatives) which needs no change to layout, cache, or domain code — so learning this first is worth one task.

The key facts from `gix_sync.rs` this test probes: `gix::worktree::state::checkout` takes `workdir` as a parameter **independent** of `repo` (`gix_sync.rs:305-313`), and `index.write()` writes to the repo's *shared* index (`gix_sync.rs:315-317`) — which is why branch worktrees must skip the index write.

**Files:**
- Create: `gears/qa-platform/qa-catalog/qa-catalog/tests/multi_branch_spike.rs`

- [ ] **Step 1: Write the failing test**

Create `gears/qa-platform/qa-catalog/qa-catalog/tests/multi_branch_spike.rs`:

```rust
//! Spike: prove one `gix` clone can materialize two different branches'
//! trees into two separate plain directories (no `.git`, no index write).
//!
//! This is the primitive the multi-branch working-copy design rests on
//! (spec §3.1). Gated behind `integration` like the sibling gix suite,
//! because it drives the real git transport.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

/// Build a local fixture repo with two branches holding different content.
/// Returns the repo path. Uses the `git` CLI (already required by the
/// sibling integration suite) purely to *author* the fixture.
fn fixture_repo(root: &Path) -> std::path::PathBuf {
    let repo = root.join("origin");
    std::fs::create_dir_all(&repo).unwrap();

    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("git must be on PATH");
        assert!(status.success(), "git {args:?} failed");
    };

    git(&["init", "--initial-branch=main", "--quiet"]);
    git(&["config", "user.email", "spike@example.test"]);
    git(&["config", "user.name", "Spike"]);

    std::fs::write(repo.join("marker.txt"), "from-main\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "main content"]);

    git(&["checkout", "-q", "-b", "release/5.0"]);
    std::fs::write(repo.join("marker.txt"), "from-release\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "release content"]);

    git(&["checkout", "-q", "main"]);
    repo
}

/// Materialize `branch`'s tree from `repo` into `dest`, WITHOUT writing the
/// repository index. This is the exact operation the production sync engine
/// will perform for each branch.
fn materialize(repo: &gix::Repository, branch: &str, dest: &Path) {
    let interrupt = AtomicBool::new(false);
    std::fs::create_dir_all(dest).unwrap();

    let tracking = format!("refs/remotes/origin/{branch}");
    let commit_id = repo
        .find_reference(tracking.as_str())
        .unwrap()
        .peel_to_id()
        .unwrap()
        .detach();

    let tree_id = repo
        .find_object(commit_id)
        .unwrap()
        .peel_to_tree()
        .unwrap()
        .id;
    let mut index = repo.index_from_tree(&tree_id).unwrap();
    let mut opts = repo
        .checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)
        .unwrap();
    opts.destination_is_initially_empty = true;

    let objects = repo.objects.clone().into_arc().unwrap();
    gix::worktree::state::checkout(
        &mut index,
        dest,
        objects,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &interrupt,
        opts,
    )
    .unwrap();
    // Deliberately NO `index.write(...)` — the index is shared across
    // branches, and these directories are read-only content snapshots.
}

#[test]
fn one_clone_materializes_two_branches_into_separate_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = fixture_repo(tmp.path());
    let url = format!("file://{}", origin.display());

    // One bare host clone owning objects + refs for every branch.
    let host = tmp.path().join("host");
    let interrupt = AtomicBool::new(false);
    let mut prepare = gix::prepare_clone_bare(url.as_str(), &host).unwrap();
    let (repo, _outcome) = prepare
        .fetch_only(gix::progress::Discard, &interrupt)
        .unwrap();

    // Two branches, two destinations, one object store.
    let main_dir = tmp.path().join("branches/main");
    let release_dir = tmp.path().join("branches/release-5-0");
    materialize(&repo, "main", &main_dir);
    materialize(&repo, "release/5.0", &release_dir);

    assert_eq!(
        std::fs::read_to_string(main_dir.join("marker.txt")).unwrap(),
        "from-main\n",
        "main worktree must hold main's content"
    );
    assert_eq!(
        std::fs::read_to_string(release_dir.join("marker.txt")).unwrap(),
        "from-release\n",
        "release worktree must hold release's content, not main's"
    );
    assert!(
        !main_dir.join(".git").exists(),
        "branch worktrees are plain directories, not git worktrees"
    );
}
```

- [ ] **Step 2: Run the test and observe the outcome**

```bash
cd /Users/serhii.verestun/Virtuozzo/projects/fabric/gears-rust
cargo test -p qa-catalog --features integration --test multi_branch_spike -- --nocapture
```

Expected: PASS. Two branch directories each hold their own `marker.txt`.

**If it fails**, read the failure before changing anything:
- A `prepare_clone_bare` / `fetch_only` API mismatch means the gix version differs from what this plan assumed — fix the call to match the vendored gix API (`cargo doc -p gix --open`), keeping the shape (bare clone, then fetch).
- A failure inside `gix::worktree::state::checkout` writing to a non-repo directory means approach A is not viable. **Stop and escalate**: switch to the clone-per-branch fallback (spec §2). Layout, cache, and all domain work in Phases B–D are unchanged; only Task 12 changes.

- [ ] **Step 3: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/tests/multi_branch_spike.rs
git commit -m "test(qa-catalog): prove one gix clone can materialize two branches

Spike for the multi-branch working-copy design: a bare host clone owning
objects/refs, with each branch's tree checked out into its own plain
directory and no index write (the index is shared across branches).

Gated behind the existing `integration` feature."
```

---

## Phase B — Product parity and ProductVersion removal

### Task 2: Migration — products own repositories, versions deleted

Three gotchas this task must respect:

1. **DDL order** — `qa_test_repositories` currently comes first, but it will now carry an FK to `qa_products`, so products must be created first.
2. **`key` is a MySQL reserved word.** The column is therefore named `product_key`, not `key`. The SDK field stays `key` for legacy parity; the mapper bridges the two names in one line. This avoids reserved-word quoting entirely.
3. The module doc's MySQL key-width arithmetic cites `idx_qa_versions_unique`, which is being deleted. Recompute it for the new index.

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/migrations/m20260812_000002_initial.rs`

- [ ] **Step 1: Rewrite the module doc header**

Replace lines 1-25 (the `//!` block) with:

```rust
//! Initial schema: products, test repositories/branches, SSH key metadata,
//! custom plans, and ephemeral test bundle descriptors.
//!
//! Follows the qa-environments migration shape: a backend match producing a
//! single `execute_unprepared` DDL blob per dialect (see DESIGN.md §3.7).
//! JSON array columns (`files`, `tags`) mirror the `holders` column pattern
//! used by qa-environments' `qa_platform_leases`.
//!
//! ## Products are created before repositories
//!
//! `qa_test_repositories.product_id` is a NOT NULL foreign key into
//! `qa_products` — every repository belongs to a product, which is how a
//! discovered plan is attributed to a product (legacy `enrich_plan_products`,
//! matching on repo ownership). The products table therefore has to be
//! declared first in each dialect blob.
//!
//! ## `product_key`, not `key`
//!
//! The durable short code a product is known by is stored as `product_key`.
//! The obvious name, `key`, is a reserved word in MySQL and would need
//! backtick quoting in every hand-written DDL statement below. The SDK model
//! exposes it as `Product::key` (legacy parity); `mapper::product_to_sdk`
//! bridges the two names.
//!
//! ## Every unique index is tenant-prefixed
//!
//! Including the one on the child table whose parent already carries the
//! tenant (`qa_repo_branches`). A tenant-blind unique index on a child table
//! is a cross-tenant channel even when every query is scoped: resource UUIDs
//! are identifiers, not secrets, so a caller who learns another tenant's
//! `repo_id` could insert a row of its *own* tenant referencing it — the
//! insert passes tenant validation, stays invisible to both tenants' scoped
//! reads, and yet permanently collides with the owner's writes (a squatting
//! denial of service) while the unique violation reports whether the victim's
//! row exists (an existence oracle). Prefixing `tenant_id` makes such a row
//! harmless junk in the squatter's own key space.
//!
//! `MySQL` key-width budget (`InnoDB`, `utf8mb4`, 3072-byte limit): the widest
//! of these is `idx_qa_branches_unique` at
//! 36*4 + 36*4 + 512*4 = 2336 bytes, and `idx_qa_products_tenant_key` is
//! 36*4 + 255*4 = 1164 bytes — both fit, so no column had to shrink.
```

- [ ] **Step 2: Rewrite `POSTGRES_UP`**

Replace the `POSTGRES_UP` constant (lines 33-111) with:

```rust
const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_products (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    product_key VARCHAR(255) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    folder VARCHAR(255) NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_name ON qa_products(tenant_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_key ON qa_products(tenant_id, product_key);

CREATE TABLE IF NOT EXISTS qa_test_repositories (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    product_id UUID NOT NULL REFERENCES qa_products(id) ON DELETE RESTRICT,
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
CREATE INDEX IF NOT EXISTS idx_qa_repos_product ON qa_test_repositories(tenant_id, product_id);

CREATE TABLE IF NOT EXISTS qa_repo_branches (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    repo_id UUID NOT NULL REFERENCES qa_test_repositories(id) ON DELETE CASCADE,
    name VARCHAR(512) NOT NULL,
    refreshed_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_branches_unique ON qa_repo_branches(tenant_id, repo_id, name);

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
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_custom_plans_tenant_name ON qa_custom_plans(tenant_id, name);

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
";
```

`ON DELETE RESTRICT` on `product_id` is deliberate: deleting a product that still owns repositories must fail loudly rather than cascade-delete a repository (and its branch cache) as a side effect.

- [ ] **Step 3: Rewrite `MYSQL_UP`**

Replace the `MYSQL_UP` constant with:

```rust
const MYSQL_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_products (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    product_key VARCHAR(255) NOT NULL,
    description TEXT NOT NULL DEFAULT (''),
    folder VARCHAR(255) NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_products_tenant_name (tenant_id, name),
    UNIQUE KEY idx_qa_products_tenant_key (tenant_id, product_key)
);

CREATE TABLE IF NOT EXISTS qa_test_repositories (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    product_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    url VARCHAR(1024) NOT NULL,
    default_branch VARCHAR(255) NOT NULL,
    content_root VARCHAR(1024) NOT NULL DEFAULT '',
    credential_ref VARCHAR(1024) NULL,
    last_synced_at TIMESTAMP NULL,
    sync_error TEXT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_repos_tenant_name (tenant_id, name),
    KEY idx_qa_repos_product (tenant_id, product_id),
    CONSTRAINT fk_qa_repos_product FOREIGN KEY (product_id) REFERENCES qa_products(id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS qa_repo_branches (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    repo_id VARCHAR(36) NOT NULL,
    name VARCHAR(512) NOT NULL,
    refreshed_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_branches_unique (tenant_id, repo_id, name),
    CONSTRAINT fk_qa_repo_branches_repo FOREIGN KEY (repo_id) REFERENCES qa_test_repositories(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS qa_ssh_keys (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    credstore_ref VARCHAR(1024) NOT NULL,
    fingerprint VARCHAR(255) NOT NULL,
    created_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_ssh_keys_tenant_name (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS qa_custom_plans (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    -- Expression defaults (MySQL 8.0.13+) for symmetry with the Postgres
    -- (`JSONB NOT NULL DEFAULT '[]'`) and SQLite (`TEXT NOT NULL DEFAULT
    -- '[]'`) definitions; application code always writes these columns
    -- explicitly on insert.
    files JSON NOT NULL DEFAULT ('[]'),
    tags JSON NOT NULL DEFAULT ('[]'),
    timeout_seconds BIGINT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_custom_plans_tenant_name (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS qa_test_bundles (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    storage_ref VARCHAR(2048) NOT NULL,
    checksum_sha256 VARCHAR(64) NOT NULL,
    size_bytes BIGINT NOT NULL,
    expires_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL,
    KEY idx_qa_bundles_expiry (expires_at)
);
";
```

Note the parenthesised `DEFAULT ('')` on `description`. MySQL `TEXT` columns cannot carry a *literal* default, but MySQL 8.0.13+ accepts an **expression** default in parentheses — the same form this blob already uses for `files JSON NOT NULL DEFAULT ('[]')` two tables below, so it introduces no new server-version requirement. Using it keeps all three dialects in agreement rather than leaving `description` as a documented one-off asymmetry, which matters in a file where three blobs are maintained by hand and only SQLite is exercised by tests.

- [ ] **Step 4: Rewrite `SQLITE_UP`**

Replace the `SQLITE_UP` constant with:

```rust
const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_products (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    product_key TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    folder TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_name ON qa_products(tenant_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_key ON qa_products(tenant_id, product_key);

CREATE TABLE IF NOT EXISTS qa_test_repositories (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    product_id TEXT NOT NULL,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    default_branch TEXT NOT NULL,
    content_root TEXT NOT NULL DEFAULT '',
    credential_ref TEXT NULL,
    last_synced_at TEXT NULL,
    sync_error TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (product_id) REFERENCES qa_products(id) ON DELETE RESTRICT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_repos_tenant_name ON qa_test_repositories(tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_qa_repos_product ON qa_test_repositories(tenant_id, product_id);

CREATE TABLE IF NOT EXISTS qa_repo_branches (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    repo_id TEXT NOT NULL,
    name TEXT NOT NULL,
    refreshed_at TEXT NOT NULL,
    FOREIGN KEY (repo_id) REFERENCES qa_test_repositories(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_branches_unique ON qa_repo_branches(tenant_id, repo_id, name);

CREATE TABLE IF NOT EXISTS qa_ssh_keys (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    credstore_ref TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_ssh_keys_tenant_name ON qa_ssh_keys(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_custom_plans (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    files TEXT NOT NULL DEFAULT '[]',
    tags TEXT NOT NULL DEFAULT '[]',
    timeout_seconds BIGINT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_custom_plans_tenant_name ON qa_custom_plans(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_test_bundles (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    storage_ref TEXT NOT NULL,
    checksum_sha256 TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_bundles_expiry ON qa_test_bundles(expires_at);
";
```

- [ ] **Step 5: Update `down()`**

Replace the `down()` SQL block so the drop order respects the new FK (repositories before products):

```rust
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = r"
DROP TABLE IF EXISTS qa_test_bundles;
DROP TABLE IF EXISTS qa_custom_plans;
DROP TABLE IF EXISTS qa_ssh_keys;
DROP TABLE IF EXISTS qa_repo_branches;
DROP TABLE IF EXISTS qa_test_repositories;
DROP TABLE IF EXISTS qa_products;
        ";
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
```

- [ ] **Step 6: Verify the migration runs**

The existing `test_support::inmem_db()` runs the real migrations against in-memory SQLite, so any migration syntax error surfaces in the existing suite.

**`cargo build` will SUCCEED, and that is not a mistake on your part.** SeaORM entities are
hand-written Rust structs whose table and column names are runtime strings — nothing links them
to migration DDL at compile time. So a schema-only change cannot produce a compile error. The
break is real but appears at runtime, when a migration runs against SQLite and the entities write
columns that no longer match. **For every schema task in this plan, `cargo test` is the signal and
`cargo build` proves nothing.**

```bash
cargo build -p qa-catalog 2>&1 | tail -3   # expect: Finished, no errors
cargo test -p qa-catalog --lib 2>&1 | grep -E "^test result|NOT NULL constraint failed" | sort -u
```

Expected at this point: **123 passed, 8 failed**, with every failure a `NOT NULL constraint failed`
on exactly `qa_products.product_key` or `qa_test_repositories.product_id` — fixtures still insert
rows without the new required columns. That is the intended state; Tasks 3-11 fix it. Confirm two
things: no failure mentions a **SQL syntax** error (that would mean a genuine bug in one of the
three dialect blobs), and no failure names a column other than `product_key` / `product_id`.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/migrations/m20260812_000002_initial.rs
git commit -m "feat(qa-catalog)!: products own repositories; drop product_versions

Adds a required qa_test_repositories.product_id (FK, ON DELETE RESTRICT)
so a discovered plan can be attributed to a product by repo ownership,
and adds qa_products.product_key + description. Deletes
qa_product_versions: legacy removed the version->branch mapping in
VHP-319 and drives runs by branch selection instead.

Products are now declared before repositories in each dialect blob for
the new FK. The column is product_key rather than key because key is a
MySQL reserved word.

Migration edited in place: it has never executed anywhere (qa-catalog is
not registered in the example server and has no deploy artifacts)."
```

### Task 3: Product entity gains `product_key` and `description`

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/entity/product.rs`

- [ ] **Step 1: Add the columns**

Replace the `Model` struct body so it reads:

```rust
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_products")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    /// Durable short code the product is known by (legacy `Product::key`).
    /// Stored as `product_key` because `key` is a MySQL reserved word; the
    /// SDK model exposes it as `key`.
    pub product_key: String,
    pub description: String,
    pub folder: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}
```

- [ ] **Step 2: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/entity/product.rs
git commit -m "feat(qa-catalog): add product_key + description to the product entity"
```

### Task 4: TestRepository entity gains `product_id`

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/entity/test_repository.rs`

- [ ] **Step 1: Add the column**

Insert `product_id` immediately after `tenant_id` in the `Model` struct:

```rust
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Owning product. Required — repo ownership is how a discovered plan is
    /// attributed to a product (legacy `enrich_plan_products`).
    pub product_id: Uuid,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    /// Subdirectory within the repo that contains test content ("" = root).
    pub content_root: String,
    /// credstore reference for the access credential (SSH key or token). `None` = public repo.
    pub credential_ref: Option<String>,
    pub last_synced_at: Option<OffsetDateTime>,
    pub sync_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}
```

- [ ] **Step 2: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/entity/test_repository.rs
git commit -m "feat(qa-catalog): add required product_id to the test-repository entity"
```

### Task 5: Delete the ProductVersion entity

**Files:**
- Delete: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/entity/product_version.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/entity/mod.rs`

- [ ] **Step 1: Delete the entity file**

```bash
cd /Users/serhii.verestun/Virtuozzo/projects/fabric/gears-rust
git rm gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/entity/product_version.rs
```

- [ ] **Step 2: Drop the module declaration**

In `src/infra/storage/entity/mod.rs`, remove the `product_version` module line (`pub mod product_version;` or `pub(crate) mod product_version;` — match the surrounding style).

- [ ] **Step 3: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/entity/mod.rs
git commit -m "refactor(qa-catalog): delete the product-version entity"
```

### Task 6: SDK models — new fields, ProductVersion gone

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog-sdk/src/models.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog-sdk/src/lib.rs`

- [ ] **Step 1: Update `TestRepository`**

Add `product_id` after `id`:

```rust
pub struct TestRepository {
    pub id: Uuid,
    /// Owning product. Required — repo ownership attributes discovered plans
    /// to a product.
    pub product_id: Uuid,
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
```

- [ ] **Step 2: Update `TestRepositoryUpdate`**

`default_branch` becomes mutable (spec §3.1) and `product_id` is settable:

```rust
pub struct TestRepositoryUpdate {
    pub product_id: Uuid,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    /// Subdirectory within the repo that contains test content ("" = root).
    pub content_root: String,
    /// credstore reference for the access credential. None = public repo.
    pub credential_ref: Option<String>,
}
```

- [ ] **Step 3: Add `product_id` to `NewTestRepository`**

Find `NewTestRepository` in the same file and add `pub product_id: Uuid,` as its first field, with the same doc comment as `TestRepository::product_id`.

- [ ] **Step 4: Update `Product` and delete `ProductVersion`**

```rust
pub struct Product {
    pub id: Uuid,
    pub name: String,
    /// Durable short code (legacy `Product::key`). Persisted as
    /// `qa_products.product_key`.
    pub key: String,
    pub description: String,
    pub folder: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}
```

Delete the entire `ProductVersion` struct (models.rs:107-114).

- [ ] **Step 5: Add `product_id` to `Plan`**

```rust
pub struct Plan {
    pub repo_id: Uuid,
    /// Owning product, resolved from the repository that holds this plan.
    pub product_id: Uuid,
    pub branch: String,
    /// Path of the plan.yaml within the content root.
    pub path: String,
    pub name: String,
    pub test_files: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub tags: Vec<String>,
    pub exclusive: ExclusiveFlag,
}
```

- [ ] **Step 6: Drop the `ProductVersion` re-export**

In `qa-catalog-sdk/src/lib.rs`, remove `ProductVersion` from the `pub use` list.

- [ ] **Step 7: Verify the SDK compiles standalone**

```bash
cargo build -p qa-catalog-sdk
```

Expected: FAIL — `qa-catalog-sdk/src/client.rs` still declares `list_versions` / `upsert_version` returning `ProductVersion`. Task 7 fixes it.

- [ ] **Step 8: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog-sdk/src/models.rs gears/qa-platform/qa-catalog/qa-catalog-sdk/src/lib.rs
git commit -m "feat(qa-catalog-sdk)!: product_id on repos/plans, key+description on Product

Deletes ProductVersion: legacy removed the version->branch mapping in
VHP-319. Makes default_branch mutable on TestRepositoryUpdate, which the
multi-branch working copy makes safe."
```

### Task 7: SDK client — delete the version operations

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog-sdk/src/client.rs`

- [ ] **Step 1: Delete the version methods**

Remove the `list_versions` and `upsert_version` trait methods (client.rs:163-173) and the `ProductVersion` import (client.rs:9).

- [ ] **Step 2: Fix the `delete_product` doc comment**

It currently promises cascade-to-versions behavior that no longer exists (client.rs:156-159). Replace with:

```rust
    /// Delete a product.
    ///
    /// Fails if the product still owns test repositories — the
    /// `qa_test_repositories.product_id` foreign key is `ON DELETE RESTRICT`,
    /// so repositories must be reassigned or removed first.
```

- [ ] **Step 3: Verify the SDK compiles**

```bash
cargo build -p qa-catalog-sdk
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog-sdk/src/client.rs
git commit -m "feat(qa-catalog-sdk)!: drop list_versions/upsert_version from the client trait"
```

### Task 8: Mapper — new fields, version mapper gone

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/mapper.rs`

- [ ] **Step 1: Update `repo_to_sdk`**

```rust
/// Convert a test-repository database entity to a contract model.
#[must_use]
pub fn repo_to_sdk(m: test_repository::Model) -> TestRepository {
    TestRepository {
        id: m.id,
        product_id: m.product_id,
        name: m.name,
        url: m.url,
        default_branch: m.default_branch,
        content_root: m.content_root,
        credential_ref: m.credential_ref,
        last_synced_at: m.last_synced_at,
        sync_error: m.sync_error,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}
```

- [ ] **Step 2: Update `product_to_sdk`**

This is where the column/field name bridge lives:

```rust
/// Convert a product database entity to a contract model.
///
/// The `product_key` column is exposed as `Product::key` — the column avoids
/// `key` because it is a MySQL reserved word (see the migration module docs).
#[must_use]
pub fn product_to_sdk(m: product::Model) -> Product {
    Product {
        id: m.id,
        name: m.name,
        key: m.product_key,
        description: m.description,
        folder: m.folder,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}
```

- [ ] **Step 3: Delete `product_version_to_sdk`**

Remove the whole function (mapper.rs:89 onward through its closing brace) and drop `product_version` from the `use super::entity::{…}` list on line 20. Also remove `ProductVersion` from the SDK import list at the top of the file.

- [ ] **Step 4: Update the module doc**

Line 6 references `product_versions.repo_branches` as a JSON-array column. Change the doc to list only `custom_plans.files` and `custom_plans.tags`. Do the same at mapper.rs:129 (`uuid_pairs_from_json` doc), which cites both columns — it is now used only for `custom_plans.files`.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/mapper.rs
git commit -m "refactor(qa-catalog): map product_id/product_key; drop the version mapper"
```

### Task 9: Repositories and domain services — remove version ops, carry new fields

This task spans the storage repo, the domain repo trait, and the product service, because they form one vertical slice: the trait defines the contract, the SeaORM repo implements it, and the service consumes it. Splitting them would leave the crate uncompilable between commits for no review benefit.

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/repos/products_repo.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/products_sea_repo.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/products.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/local_client/client.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/error.rs`

- [ ] **Step 1: Delete version methods from the domain repo trait**

In `src/domain/repos/products_repo.rs`, delete every trait method dealing with versions (`list_versions`, `upsert_version`, and any `delete_versions_for_product` helper). Extend the `create` and `update` signatures to carry the new fields:

```rust
    async fn create(
        &self,
        runner: &impl DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
        name: String,
        key: String,
        description: String,
        folder: Option<String>,
    ) -> Result<Product, DomainError>;

    async fn update(
        &self,
        runner: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
        name: String,
        key: String,
        description: String,
        folder: Option<String>,
    ) -> Result<Product, DomainError>;
```

Match the exact existing signature style (whether it returns `Result<_, ScopeError>` or `Result<_, DomainError>`, and whether `runner` is `&impl DBRunner`) — do not change the error type or executor convention, only the field list.

- [ ] **Step 2: Update the SeaORM repo**

In `src/infra/storage/products_sea_repo.rs`, delete the version method implementations. In `create`, set the new columns on the `ActiveModel`:

```rust
        let am = product::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            name: Set(name),
            product_key: Set(key),
            description: Set(description),
            folder: Set(folder),
            created_at: Set(now),
            updated_at: Set(now),
        };
```

In `update`, set `product_key`, `description`, `name`, `folder`, and `updated_at`. Keep the surrounding `secure_insert` / scoped-update calls exactly as they are.

- [ ] **Step 3: Update the product service**

In `src/domain/service/products.rs`:
- delete the version service methods
- thread `key` and `description` through `create_product` / `update_product`
- keep `list_product_folders` untouched (spec D9)

Update the `update_product` doc comment (products.rs:111-112), which currently says the mutable fields are `name` and `folder`:

```rust
    /// Replace a product's mutable fields (`name`, `key`, `description`,
    /// `folder`) — full replace, so `folder: None` moves the product back to
    /// the root.
```

- [ ] **Step 4: Update the local client**

In `src/domain/local_client/client.rs`, delete the `list_versions` / `upsert_version` implementations so the impl matches the trimmed trait from Task 7.

- [ ] **Step 5: Remove dead error variants**

In `src/domain/error.rs`, remove any variant that exists solely for versions (e.g. a `VersionNotFound`). Leave every variant still referenced elsewhere alone.

```bash
cargo build -p qa-catalog 2>&1 | grep -E "^(error|warning: unused)" | head -30
```

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/domain gears/qa-platform/qa-catalog/qa-catalog/src/infra/storage/products_sea_repo.rs
git commit -m "refactor(qa-catalog)!: remove product-version ops; products carry key+description"
```

### Task 10: REST surface — DTOs, handlers, routes

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/api/rest/dto.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/api/rest/handlers/products.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/api/rest/routes/products.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/api/rest/error.rs`

- [ ] **Step 1: Update product DTOs**

In `src/api/rest/dto.rs`, add the fields to the product request and response DTOs. Follow the file's existing derive set exactly — every REST DTO must have `Serialize`, `Deserialize`, and `ToSchema` (gear-creator layer rule). For example, for the response DTO:

```rust
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProductDto {
    pub id: Uuid,
    pub name: String,
    /// Durable short code for the product.
    pub key: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}
```

Match the file's actual conventions for `rename_all`, time serialization, and `skip_serializing_if` rather than copying the above verbatim — read the neighbouring DTOs first.

- [ ] **Step 2: Add `product_id` to repository DTOs**

Add `pub product_id: Uuid` to the create-repository request DTO, the update-repository request DTO, and the repository response DTO.

- [ ] **Step 3: Delete version DTOs**

Remove `ProductVersionDto` and any version request/list DTOs.

- [ ] **Step 4: Delete version handlers and routes**

In `handlers/products.rs`, delete the version handlers. In `routes/products.rs`, delete their `OperationBuilder` route registrations. Leave `list_product_folders` (handlers/products.rs:96-101) intact.

- [ ] **Step 5: Drop version error mappings**

In `src/api/rest/error.rs`, remove any match arm for a deleted version error variant. Note error.rs:185 documents an exhaustiveness test that guards status-code drift — if that test enumerates version variants, update it too.

- [ ] **Step 6: Build and fix fallout**

```bash
cargo build -p qa-catalog 2>&1 | tail -30
```

Expected: PASS, or remaining errors only in `src/test_support.rs` and test fixtures (Task 11).

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/api
git commit -m "feat(qa-catalog)!: REST surface carries product fields; version routes deleted"
```

### Task 11: Fix test fixtures for the new required field

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/test_support.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/repos_tests.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/products_tests.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans_tests.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/bundles_tests.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/tests_tenant_scoping.rs`

- [ ] **Step 1: Add a product fixture helper**

Every repository fixture now needs an owning product. Add to `src/test_support.rs`:

```rust
/// Create a product in `tenant_id` and return its id. Repository fixtures
/// need one because `qa_test_repositories.product_id` is required.
pub async fn seed_product(
    services: &ConcreteAppServices,
    ctx: &SecurityContext,
    name: &str,
) -> Uuid {
    services
        .products
        .create_product(
            ctx,
            name.to_owned(),
            name.to_uppercase(),
            format!("{name} fixture product"),
            None,
        )
        .await
        .expect("product fixture must be creatable")
        .id
}
```

Adjust the argument list to match the real `create_product` signature produced in Task 9 Step 3.

- [ ] **Step 2: Thread the product through repository fixtures**

Every place that creates a test repository must pass a `product_id`. Find them:

```bash
grep -rn "NewTestRepository" gears/qa-platform/qa-catalog/qa-catalog/src | grep -v "^.*models.rs"
```

For each, seed a product first and pass its id.

- [ ] **Step 3: Delete version tests**

In `products_tests.rs`, remove tests covering `list_versions` / `upsert_version`. Keep every product CRUD, folder, and tenant-scoping test, extending them with assertions on the new `key` / `description` fields.

- [ ] **Step 4: Run the suite**

```bash
cargo test -p qa-catalog 2>&1 | tail -30
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src
git commit -m "test(qa-catalog): fixtures seed an owning product; version tests removed"
```

### Task 12: Attribute discovered plans to their product

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans.rs`
- Test: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans_tests.rs`

`synced_content_root` already loads the repository row (plans.rs:107-111), so `product_id` costs no extra query — it just has to be returned alongside the path instead of discarded.

- [ ] **Step 1: Write the failing test**

Add to `src/domain/service/plans_tests.rs`:

```rust
#[tokio::test]
async fn discovered_plans_carry_the_owning_product() {
    let (services, ctx, tenant_id) = plans_fixture().await;
    let product_id = crate::test_support::seed_product(&services, &ctx, "vhp").await;
    let repo_id = seed_synced_repo_with_product(&services, &ctx, tenant_id, product_id).await;

    let plans = services
        .plans
        .list_plans(&ctx, repo_id, "main")
        .await
        .expect("discovery must succeed");

    assert!(!plans.is_empty(), "fixture must discover at least one plan");
    for plan in &plans {
        assert_eq!(
            plan.product_id, product_id,
            "every discovered plan must be attributed to the repository's product"
        );
    }
}
```

Reuse whatever fixture helper the existing tests in this file use to create a synced repository with plan content; add a `_with_product` variant that takes the product id rather than inventing a parallel fixture.

- [ ] **Step 2: Run it to confirm it fails**

```bash
cargo test -p qa-catalog discovered_plans_carry_the_owning_product 2>&1 | tail -20
```

Expected: FAIL — `Plan` has no field `product_id` populated by `to_sdk_plan`, so this is a compile error until Step 3.

- [ ] **Step 3: Return the product from `synced_content_root`**

Change the helper to hand back both the product id and the path:

```rust
    /// Resolve the repository (tenancy precheck under its own `TEST_REPO/GET`
    /// scope), require it synced for `branch`, and return the owning product
    /// plus the canonicalized content-root directory.
    async fn synced_content_root(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
    ) -> Result<(Uuid, PathBuf), DomainError> {
        let repo_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::GET, Some(repo_id))
            .await?;

        let conn = self.db.conn()?;
        let repo = self
            .repos_repo
            .get(&conn, &repo_scope, repo_id)
            .await?
            .ok_or(DomainError::NotFound { id: repo_id })?;

        require_synced(&repo, branch)?;
        let root = content_root_dir(&self.repos_dir, &repo, branch)?;
        Ok((repo.product_id, root))
    }
```

- [ ] **Step 4: Thread it through `to_sdk_plan`**

```rust
fn to_sdk_plan(
    repo_id: Uuid,
    product_id: Uuid,
    branch: &str,
    path: String,
    parsed: ParsedPlan,
) -> Plan {
    Plan {
        repo_id,
        product_id,
        branch: branch.to_owned(),
        path,
        name: parsed.name,
        test_files: parsed.test_files,
        // Always `Some`: the parser applies the legacy 300s default. The SDK
        // field is optional only because a *custom* plan may carry no timeout.
        timeout_seconds: Some(parsed.timeout_seconds),
        tags: parsed.tags,
        exclusive: parsed.exclusive,
    }
}
```

Update `list_plans` to destructure the new tuple and pass `product_id`:

```rust
        let (product_id, root) = self.synced_content_root(ctx, repo_id, branch).await?;

        let discovered = tokio::task::spawn_blocking(move || discover_plans(&root))
            .await
            .map_err(|e| DomainError::Internal(e.to_string()))?;

        let plans = discovered
            .into_iter()
            .map(|(path, parsed)| to_sdk_plan(repo_id, product_id, branch, path, parsed))
            .collect::<Vec<_>>();
```

Apply the same destructuring at every other `synced_content_root` call site (`get_plan`, `get_test_meta`, and the bundles service if it calls through here):

```bash
grep -rn "synced_content_root" gears/qa-platform/qa-catalog/qa-catalog/src
```

- [ ] **Step 5: Run the test**

```bash
cargo test -p qa-catalog discovered_plans_carry_the_owning_product 2>&1 | tail -20
```

Expected: PASS.

- [ ] **Step 6: Run the whole suite**

```bash
cargo test -p qa-catalog 2>&1 | tail -20
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src
git commit -m "feat(qa-catalog): attribute discovered plans to their owning product

Mirrors legacy enrich_plan_products' repo-ownership path. The repository
row is already loaded by synced_content_root, so this adds no query."
```

---

## Phase C — Multi-branch working copies

### Task 13: Branch-directory naming

Legacy's `branch_dir_name` (`test_repos.rs:1340-1366`) normalizes lossily and then appends a digest, because normalization alone collides: `release/5.0` and `release-5-0` both normalize to `release-5-0`.

**Files:**
- Create: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/git/layout.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/git/mod.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/infra/git/layout.rs`:

```rust
//! On-disk layout of the multi-branch working area.
//!
//! ```text
//! <repos_dir>/<repo_id>/git                     one clone: objects + refs
//! <repos_dir>/<repo_id>/branches/<branch_dir>   per-branch content snapshot
//! ```
//!
//! Branch snapshots are plain directories — no `.git`, no index. They are
//! read-only content materializations, which is what lets many of them share
//! one object store (a real `git worktree` would need its own index, and gix
//! writes the index into the shared repository).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Directory holding the repository's single clone (objects + refs).
#[must_use]
pub fn host_dir(repos_dir: &Path, repo_id: Uuid) -> PathBuf {
    repos_dir.join(repo_id.to_string()).join("git")
}

/// Directory holding `branch`'s materialized content.
#[must_use]
pub fn branch_workdir(repos_dir: &Path, repo_id: Uuid, branch: &str) -> PathBuf {
    repos_dir
        .join(repo_id.to_string())
        .join("branches")
        .join(branch_dir_name(branch))
}

/// Filesystem-safe directory name for `branch`.
///
/// Normalization is deliberately lossy (slashes and dots become `-`), so a
/// digest of the original name is appended: without it `release/5.0` and
/// `release-5-0` would share one directory and serve each other's content.
#[must_use]
pub fn branch_dir_name(branch: &str) -> String {
    let trimmed = branch.trim();
    let normalized = trimmed
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let base = normalized.trim_matches('-').to_owned();

    let digest = format!("{:x}", Sha256::digest(trimmed.as_bytes()));
    let suffix = &digest[..8];

    if base.is_empty() {
        suffix.to_owned()
    } else {
        format!("{base}-{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_branch_keeps_its_name_plus_digest() {
        let name = branch_dir_name("main");
        assert!(name.starts_with("main-"), "got {name}");
        assert_eq!(name.len(), "main-".len() + 8);
    }

    #[test]
    fn slashes_are_normalized() {
        assert!(branch_dir_name("release/5.0").starts_with("release-5-0-"));
    }

    #[test]
    fn lossy_normalization_does_not_collide() {
        assert_ne!(
            branch_dir_name("release/5.0"),
            branch_dir_name("release-5-0"),
            "the digest suffix must disambiguate names that normalize alike"
        );
    }

    #[test]
    fn is_deterministic() {
        assert_eq!(branch_dir_name("feature/x"), branch_dir_name("feature/x"));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(branch_dir_name("  main  "), branch_dir_name("main"));
    }

    #[test]
    fn name_that_normalizes_to_nothing_still_yields_a_directory() {
        let name = branch_dir_name("///");
        assert_eq!(name.len(), 8, "got {name}");
    }

    #[test]
    fn branch_workdir_nests_under_the_repo_id() {
        let repo_id = Uuid::nil();
        let path = branch_workdir(Path::new("/data/repos"), repo_id, "main");
        let expected_prefix = Path::new("/data/repos")
            .join(repo_id.to_string())
            .join("branches");
        assert!(path.starts_with(&expected_prefix), "got {path:?}");
    }

    #[test]
    fn host_dir_is_a_sibling_of_branches() {
        let repo_id = Uuid::nil();
        assert_eq!(
            host_dir(Path::new("/data/repos"), repo_id),
            Path::new("/data/repos").join(repo_id.to_string()).join("git")
        );
    }
}
```

- [ ] **Step 2: Register the module**

In `src/infra/git/mod.rs`, add:

```rust
pub mod layout;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p qa-catalog layout:: 2>&1 | tail -20
```

Expected: PASS (8 tests).

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/infra/git/layout.rs gears/qa-platform/qa-catalog/qa-catalog/src/infra/git/mod.rs
git commit -m "feat(qa-catalog): multi-branch working-area layout helpers

branch_dir_name appends a digest because normalization is lossy:
release/5.0 and release-5-0 would otherwise share a directory."
```

### Task 14: Sync port takes a branch destination

The port already accepts `branch` and `workdir` (`domain/ports/repo_sync.rs:36-42`), but `workdir` currently means "the repository's one working copy". It now means "where this branch's content goes", and the engine additionally needs the host-clone directory.

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/ports/repo_sync.rs`

- [ ] **Step 1: Change the port signature**

```rust
/// Port abstracting the git engine. Implemented in infra (gix; see ADR-0005,
/// Task 9). Consumed by `domain::service::ReposService`.
#[async_trait]
pub trait RepoSyncPort: Send + Sync {
    /// Clone-or-fetch the repository into `host_dir`, then materialize
    /// `branch`'s content into `branch_workdir`, and return branch inventory
    /// + head commit.
    ///
    /// `host_dir` holds one clone per repository (objects + refs) shared by
    /// every branch; `branch_workdir` is a plain content snapshot with no
    /// `.git` of its own. `credential` is the resolved secret material (never
    /// logged), already fetched from credstore by the service.
    async fn sync(
        &self,
        url: &str,
        branch: &str,
        credential: Option<&str>,
        host_dir: &std::path::Path,
        branch_workdir: &std::path::Path,
    ) -> Result<SyncResult, DomainError>;

    /// List remote branches without a full sync. Consumed by
    /// `ReposService::refresh_branches` (the branch-cache refresher
    /// lifecycle task).
    async fn list_remote_branches(
        &self,
        url: &str,
        credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError>;
}
```

- [ ] **Step 2: Build to enumerate the implementors**

```bash
cargo build -p qa-catalog 2>&1 | grep -A3 "not all trait items\|incorrect number of function parameters" | head -30
```

Expected: errors in `infra/git/gix_sync.rs` and the doubles in `src/test_support.rs`. Tasks 15 and 16 fix them.

- [ ] **Step 3: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/domain/ports/repo_sync.rs
git commit -m "refactor(qa-catalog)!: sync port takes a host dir and a branch workdir"
```

### Task 15: gix engine — shared clone, per-branch materialization

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/infra/git/gix_sync.rs`

- [ ] **Step 1: Update the module doc sync-model section**

Replace the `## Sync model (p1)` block (gix_sync.rs:4-12) with:

```rust
//! ## Sync model
//!
//! One clone per repository at `<repos_dir>/<repo_id>/git` owns the object
//! and ref store. Each branch's content is materialized into its own plain
//! directory at `<repos_dir>/<repo_id>/branches/<branch_dir>` (see
//! `infra::git::layout`). `sync` clones on first use and fetches into the
//! existing clone afterwards, then rewrites the requested branch's snapshot
//! from its tip. The snapshot is cleared and rewritten on every sync — gix's
//! checkout only writes index entries and would otherwise leave files
//! deleted upstream lying around.
//!
//! Branch snapshots are **not** git worktrees: they hold no `.git` and the
//! repository index is never written for them. That is deliberate — the
//! index lives in the shared clone, so writing it per branch would make
//! concurrent materializations of different branches clobber each other.
//! Nothing reads these directories as git repositories; they are content
//! only (plan discovery, TEST_META parsing, bundle packing).
```

- [ ] **Step 2: Rewrite the `sync` impl**

```rust
#[async_trait]
impl RepoSyncPort for GixSyncEngine {
    async fn sync(
        &self,
        url: &str,
        branch: &str,
        credential: Option<&str>,
        host_dir: &Path,
        branch_workdir: &Path,
    ) -> Result<SyncResult, DomainError> {
        let url = url.to_owned();
        let branch = branch.to_owned();
        let credential = credential.map(ToOwned::to_owned);
        let host_dir = host_dir.to_owned();
        let branch_workdir = branch_workdir.to_owned();
        tokio::task::spawn_blocking(move || {
            sync_blocking(
                &url,
                &branch,
                credential.as_deref(),
                &host_dir,
                &branch_workdir,
            )
        })
        .await
        .map_err(|e| DomainError::Internal(format!("sync task join error: {e}")))?
    }
```

Leave `list_remote_branches` unchanged.

- [ ] **Step 3: Rewrite `sync_blocking`**

```rust
/// Clone-or-fetch `url` into `host_dir`, then materialize `branch` into
/// `branch_workdir`.
///
/// A `host_dir` that is missing, unopenable, or tracking a different URL is
/// (re-)cloned from scratch — the clone is derived state, safe to discard.
/// Discarding it also invalidates every branch snapshot beneath it, so the
/// whole repository directory is cleared in that case.
fn sync_blocking(
    url: &str,
    branch: &str,
    credential: Option<&str>,
    host_dir: &Path,
    branch_workdir: &Path,
) -> Result<SyncResult, DomainError> {
    let (repo, branches) = match open_existing(url, host_dir) {
        Some(repo) => {
            let branches = fetch_existing(&repo, credential)?;
            (repo, branches)
        }
        None => {
            // The clone is unusable; drop the repository directory entirely so
            // no stale branch snapshot survives beside a fresh object store.
            if let Some(repo_root) = host_dir.parent()
                && repo_root.exists()
            {
                std::fs::remove_dir_all(repo_root).map_err(|e| DomainError::SyncFailed {
                    message: format!("failed to clear the stale working area: {e}"),
                })?;
            }
            clone_host(url, credential, host_dir)?
        }
    };

    // Membership is checked against the just-advertised refs, not the local
    // remote-tracking refs: fetch does not prune, so a stale tracking ref may
    // survive a branch deleted upstream.
    if !branches.iter().any(|b| b == branch) {
        return Err(DomainError::SyncFailed {
            message: format!("branch '{branch}' not found on the remote"),
        });
    }

    let head_commit = materialize_branch(&repo, branch, branch_workdir)?;
    Ok(SyncResult {
        branches,
        head_commit,
    })
}
```

- [ ] **Step 4: Replace `clone_and_checkout` with `clone_host`**

Delete `clone_and_checkout` (gix_sync.rs:139-172) and add:

```rust
/// Bare clone of `url` into `host_dir` — objects and refs only, no worktree.
/// Branch content is materialized separately by [`materialize_branch`].
/// Returns the opened repository and the advertised branch inventory.
fn clone_host(
    url: &str,
    credential: Option<&str>,
    host_dir: &Path,
) -> Result<(gix::Repository, Vec<String>), DomainError> {
    let interrupt = AtomicBool::new(false);
    let credential = credential.map(ToOwned::to_owned);

    std::fs::create_dir_all(host_dir).map_err(|e| DomainError::SyncFailed {
        message: format!("failed to create the clone directory: {e}"),
    })?;

    let mut prepare = gix::prepare_clone_bare(url, host_dir)
        .map_err(|e| sync_err("failed to prepare clone", &e))?
        .configure_connection(move |connection| {
            connection.set_credentials(credential_helper(credential.clone()));
            Ok(())
        });

    let (repo, outcome) = prepare
        .fetch_only(gix::progress::Discard, &interrupt)
        .map_err(|e| sync_err("clone failed", &e))?;

    let branches = remote_branch_names(&outcome.ref_map.remote_refs);
    Ok((repo, branches))
}
```

- [ ] **Step 5: Replace `fetch_and_checkout` with `fetch_existing`**

Delete `fetch_and_checkout` (gix_sync.rs:176-215) and add:

```rust
/// Fetch into the existing clone and return the advertised branch inventory.
/// Materialization is a separate step, so this touches no worktree.
fn fetch_existing(
    repo: &gix::Repository,
    credential: Option<&str>,
) -> Result<Vec<String>, DomainError> {
    let interrupt = AtomicBool::new(false);

    let remote = repo
        .find_remote(REMOTE_NAME)
        .map_err(|e| sync_err("failed to resolve the origin remote", &e))?;
    let mut connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| sync_err("failed to connect to the remote", &e))?;
    connection.set_credentials(credential_helper(credential.map(ToOwned::to_owned)));
    let outcome = connection
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(|e| sync_err("fetch negotiation failed", &e))?
        .receive(gix::progress::Discard, &interrupt)
        .map_err(|e| sync_err("fetch failed", &e))?;

    Ok(remote_branch_names(&outcome.ref_map.remote_refs))
}
```

- [ ] **Step 6: Replace `checkout_branch` / `checkout_worktree` with `materialize_branch`**

Delete `checkout_branch` (gix_sync.rs:219-245), `point_head_at` (gix_sync.rs:248-272), and `checkout_worktree` (gix_sync.rs:277-319). `point_head_at` goes because a bare clone has no meaningful HEAD to move and nothing reads it. Add:

```rust
/// Materialize `branch`'s tree from the shared clone into `dest`, returning
/// the tip commit id.
///
/// The repository index is deliberately **not** written: it belongs to the
/// shared clone, and writing it here would make concurrent materializations
/// of different branches clobber one another. `dest` is content only.
fn materialize_branch(
    repo: &gix::Repository,
    branch: &str,
    dest: &Path,
) -> Result<String, DomainError> {
    let interrupt = AtomicBool::new(false);

    let tracking_ref = format!("refs/remotes/{REMOTE_NAME}/{branch}");
    let commit_id = repo
        .find_reference(tracking_ref.as_str())
        .map_err(|e| sync_err("fetched branch has no remote-tracking ref", &e))?
        .peel_to_id()
        .map_err(|e| sync_err("failed to resolve the branch tip", &e))?
        .detach();

    let tree_id = repo
        .find_object(commit_id)
        .map_err(|e| sync_err("branch tip object missing after fetch", &e))?
        .peel_to_tree()
        .map_err(|e| sync_err("branch tip is not a treeish", &e))?
        .id;

    reset_dir(dest)?;

    let mut index = repo
        .index_from_tree(&tree_id)
        .map_err(|e| sync_err("failed to build the index from the branch tree", &e))?;
    let mut opts = repo
        .checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)
        .map_err(|e| sync_err("failed to load checkout options", &e))?;
    // `reset_dir` just recreated it empty.
    opts.destination_is_initially_empty = true;

    let objects = repo
        .objects
        .clone()
        .into_arc()
        .map_err(|e| sync_err("failed to open the object database", &e))?;
    gix::worktree::state::checkout(
        &mut index,
        dest,
        objects,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &interrupt,
        opts,
    )
    .map_err(|e| sync_err("worktree checkout failed", &e))?;

    Ok(commit_id.to_string())
}

/// Recreate `dir` empty. Branch snapshots hold no `.git`, so unlike the old
/// single-working-copy model there is nothing to preserve.
fn reset_dir(dir: &Path) -> Result<(), DomainError> {
    let err = |e: std::io::Error| sync_err("failed to reset the branch snapshot", &e);
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(err)?;
    }
    std::fs::create_dir_all(dir).map_err(err)?;
    Ok(())
}
```

Also delete `clear_worktree` (gix_sync.rs:322-339) — `reset_dir` replaces it.

- [ ] **Step 7: Point `open_existing` at the host dir**

Only its doc comment needs to change; the body is unchanged:

```rust
/// Open the existing clone, provided it tracks `url` as its `origin` fetch
/// remote. Any failure (no repo, corrupt repo, different URL) yields `None`,
/// which makes [`sync_blocking`] discard the working area and re-clone.
```

- [ ] **Step 8: Build**

```bash
cargo build -p qa-catalog 2>&1 | tail -30
```

Expected: remaining errors only in `src/test_support.rs` and `src/domain/service/repos.rs` (Tasks 16-17).

- [ ] **Step 9: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/infra/git/gix_sync.rs
git commit -m "feat(qa-catalog): shared bare clone + per-branch content materialization

One clone per repository owns objects/refs; each branch is materialized
into its own plain directory. The index is never written for a branch
snapshot — it belongs to the shared clone, so per-branch writes would
clobber each other."
```

### Task 16: Freshness cache and two-tier locking

Legacy locks at two tiers (`test_repos.rs:543-558`): per-`(repo_id, branch)` and per-repo. The per-repo tier exists because every branch materialization reads the one shared object store, and concurrent fetches race on index and ref locks. This design builds N bundles concurrently, so that tier is load-bearing, not defensive.

**Files:**
- Create: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/sync_cache.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/mod.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/domain/service/sync_cache.rs`:

```rust
//! In-memory sync bookkeeping for the multi-branch working area: a freshness
//! TTL cache and the two-tier lock registry.
//!
//! Deliberately **not** persisted, matching the source system. The launch
//! path force-syncs (evicting first), so a lost cache after a restart costs
//! at most one redundant fetch on the next browse read — never correctness.
//!
//! ## Two lock tiers
//!
//! - per-`(repo_id, branch)`: serializes syncs of the same branch
//! - per-repo: serializes *every* git mutation for a repository, because all
//!   of its branch snapshots are materialized from one shared object store
//!   and concurrent fetches race on index and ref locks
//!
//! A caller takes the repo lock for the duration of a sync; the branch lock
//! additionally collapses duplicate work on the same branch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Key identifying one materialized branch snapshot.
type BranchKey = (Uuid, String);

/// Freshness cache + lock registry for repository syncs.
#[domain_model]
pub struct SyncCache {
    ttl: Duration,
    fresh: Arc<Mutex<HashMap<BranchKey, Instant>>>,
    repo_locks: Arc<Mutex<HashMap<Uuid, Arc<Mutex<()>>>>>,
    branch_locks: Arc<Mutex<HashMap<BranchKey, Arc<Mutex<()>>>>>,
}

impl SyncCache {
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            fresh: Arc::new(Mutex::new(HashMap::new())),
            repo_locks: Arc::new(Mutex::new(HashMap::new())),
            branch_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Whether `(repo_id, branch)` was synced within the TTL. A zero TTL
    /// disables the cache entirely.
    pub async fn is_fresh(&self, repo_id: Uuid, branch: &str) -> bool {
        if self.ttl.is_zero() {
            return false;
        }
        let fresh = self.fresh.lock().await;
        fresh
            .get(&(repo_id, branch.to_owned()))
            .is_some_and(|at| at.elapsed() < self.ttl)
    }

    /// Record a successful sync of `(repo_id, branch)`.
    pub async fn mark_synced(&self, repo_id: Uuid, branch: &str) {
        let mut fresh = self.fresh.lock().await;
        fresh.insert((repo_id, branch.to_owned()), Instant::now());
    }

    /// Drop the freshness entry so the next read re-syncs. This is what
    /// "force sync" means on the launch path.
    pub async fn invalidate(&self, repo_id: Uuid, branch: &str) {
        let mut fresh = self.fresh.lock().await;
        fresh.remove(&(repo_id, branch.to_owned()));
    }

    /// Drop every freshness entry for a repository (URL or content-root
    /// change, or repository deletion).
    pub async fn invalidate_repo(&self, repo_id: Uuid) {
        let mut fresh = self.fresh.lock().await;
        fresh.retain(|(id, _), _| *id != repo_id);
    }

    /// Repository-wide git mutation lock — see the two-tier note above.
    pub async fn repo_lock(&self, repo_id: Uuid) -> Arc<Mutex<()>> {
        let mut locks = self.repo_locks.lock().await;
        Arc::clone(locks.entry(repo_id).or_insert_with(|| Arc::new(Mutex::new(()))))
    }

    /// Per-branch lock, collapsing duplicate syncs of the same branch.
    pub async fn branch_lock(&self, repo_id: Uuid, branch: &str) -> Arc<Mutex<()>> {
        let mut locks = self.branch_locks.lock().await;
        Arc::clone(
            locks
                .entry((repo_id, branch.to_owned()))
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> SyncCache {
        SyncCache::new(Duration::from_secs(300))
    }

    #[tokio::test]
    async fn unknown_branch_is_not_fresh() {
        assert!(!cache().is_fresh(Uuid::nil(), "main").await);
    }

    #[tokio::test]
    async fn marked_branch_is_fresh() {
        let c = cache();
        c.mark_synced(Uuid::nil(), "main").await;
        assert!(c.is_fresh(Uuid::nil(), "main").await);
    }

    #[tokio::test]
    async fn freshness_is_per_branch() {
        let c = cache();
        c.mark_synced(Uuid::nil(), "main").await;
        assert!(
            !c.is_fresh(Uuid::nil(), "release/5.0").await,
            "marking one branch must not make another look fresh"
        );
    }

    #[tokio::test]
    async fn invalidate_forces_a_resync() {
        let c = cache();
        c.mark_synced(Uuid::nil(), "main").await;
        c.invalidate(Uuid::nil(), "main").await;
        assert!(!c.is_fresh(Uuid::nil(), "main").await);
    }

    #[tokio::test]
    async fn invalidate_repo_clears_every_branch() {
        let c = cache();
        let repo = Uuid::new_v4();
        let other = Uuid::new_v4();
        c.mark_synced(repo, "main").await;
        c.mark_synced(repo, "dev").await;
        c.mark_synced(other, "main").await;

        c.invalidate_repo(repo).await;

        assert!(!c.is_fresh(repo, "main").await);
        assert!(!c.is_fresh(repo, "dev").await);
        assert!(
            c.is_fresh(other, "main").await,
            "another repository's freshness must survive"
        );
    }

    #[tokio::test]
    async fn zero_ttl_disables_the_cache() {
        let c = SyncCache::new(Duration::ZERO);
        c.mark_synced(Uuid::nil(), "main").await;
        assert!(!c.is_fresh(Uuid::nil(), "main").await);
    }

    #[tokio::test]
    async fn repo_lock_is_shared_per_repo_and_distinct_across_repos() {
        let c = cache();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert!(Arc::ptr_eq(&c.repo_lock(a).await, &c.repo_lock(a).await));
        assert!(!Arc::ptr_eq(&c.repo_lock(a).await, &c.repo_lock(b).await));
    }

    #[tokio::test]
    async fn branch_lock_is_distinct_per_branch() {
        let c = cache();
        let repo = Uuid::new_v4();
        assert!(Arc::ptr_eq(
            &c.branch_lock(repo, "main").await,
            &c.branch_lock(repo, "main").await
        ));
        assert!(!Arc::ptr_eq(
            &c.branch_lock(repo, "main").await,
            &c.branch_lock(repo, "dev").await
        ));
    }
}
```

- [ ] **Step 2: Register the module**

In `src/domain/service/mod.rs`, add `pub mod sync_cache;` alongside the other service modules, and re-export if that file re-exports its siblings:

```rust
pub use sync_cache::SyncCache;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p qa-catalog sync_cache:: 2>&1 | tail -20
```

Expected: PASS (8 tests).

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/sync_cache.rs gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/mod.rs
git commit -m "feat(qa-catalog): in-memory branch freshness cache + two-tier sync locks

Not persisted, matching the source system: the launch path force-syncs,
so a cache lost on restart costs one redundant fetch, never correctness."
```

### Task 17: Branch-aware `require_synced` and `content_root_dir`

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans.rs`
- Test: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans_tests.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/domain/service/plans_tests.rs`:

```rust
#[test]
fn require_synced_accepts_any_branch_once_the_repo_is_synced() {
    let repo = synced_repo_fixture("main");
    assert!(
        super::plans::require_synced(&repo, "release/5.0").is_ok(),
        "a non-default branch must be readable under the multi-branch model"
    );
}

#[test]
fn require_synced_rejects_a_repo_with_a_sync_error() {
    let mut repo = synced_repo_fixture("main");
    repo.sync_error = Some("boom".to_owned());
    assert!(matches!(
        super::plans::require_synced(&repo, "main"),
        Err(DomainError::RepoNotSynced { .. })
    ));
}

#[test]
fn require_synced_rejects_a_never_synced_repo() {
    let mut repo = synced_repo_fixture("main");
    repo.last_synced_at = None;
    assert!(matches!(
        super::plans::require_synced(&repo, "main"),
        Err(DomainError::RepoNotSynced { .. })
    ));
}
```

Add the fixture helper next to them, matching the `TestRepository` shape after Task 6:

```rust
/// A `TestRepository` that looks successfully synced, with `default_branch`.
fn synced_repo_fixture(default_branch: &str) -> TestRepository {
    let now = time::OffsetDateTime::now_utc();
    TestRepository {
        id: Uuid::new_v4(),
        product_id: Uuid::new_v4(),
        name: "fixture".to_owned(),
        url: "https://example.test/repo.git".to_owned(),
        default_branch: default_branch.to_owned(),
        content_root: String::new(),
        credential_ref: None,
        last_synced_at: Some(now),
        sync_error: None,
        created_at: now,
        updated_at: now,
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
cargo test -p qa-catalog require_synced_accepts_any_branch 2>&1 | tail -20
```

Expected: FAIL — the current predicate requires `branch == repo.default_branch`.

- [ ] **Step 3: Make `require_synced` branch-agnostic**

Replace the function (plans.rs:291-304):

```rust
/// Require the repository's working area to be usable for `branch`.
///
/// Under the multi-branch model any branch is readable once the repository
/// has synced successfully at least once — the per-branch snapshot's actual
/// presence is checked by [`content_root_dir`], which canonicalizes it.
///
/// `sync_error` is repository-scoped, not per-branch: there is no per-branch
/// state store (a deliberate parity choice — the source system's per-branch
/// cache records freshness only, never errors). So a failed sync of branch B
/// also fails reads of an otherwise-healthy branch A until the next
/// successful sync. Accepted; fixing it needs a persisted per-branch table.
pub(super) fn require_synced(repo: &TestRepository, branch: &str) -> Result<(), DomainError> {
    let synced = repo.last_synced_at.is_some() && repo.sync_error.is_none();
    if synced {
        Ok(())
    } else {
        Err(DomainError::RepoNotSynced {
            repo_id: repo.id,
            branch: branch.to_owned(),
        })
    }
}
```

- [ ] **Step 4: Make `content_root_dir` resolve under the branch snapshot**

Replace the body's workdir derivation (plans.rs:310-342):

```rust
/// Resolve and canonicalize the content-root directory for `branch`
/// (`<repos_dir>/<repo_id>/branches/<branch_dir>/<content_root>`), verifying
/// it stays inside that branch's snapshot. A missing directory (never synced
/// for this branch, or a wiped data dir) reads as
/// [`DomainError::RepoNotSynced`].
pub(super) fn content_root_dir(
    repos_dir: &Path,
    repo: &TestRepository,
    branch: &str,
) -> Result<PathBuf, DomainError> {
    let not_synced = || DomainError::RepoNotSynced {
        repo_id: repo.id,
        branch: branch.to_owned(),
    };

    let workdir = crate::infra::git::layout::branch_workdir(repos_dir, repo.id, branch);
    let root = if repo.content_root.is_empty() {
        workdir.clone()
    } else {
        // Stored value is validated on create, but re-validate on every use:
        // defense in depth against rows written by older code paths.
        validate_rel_path("content_root", &repo.content_root)?;
        workdir.join(&repo.content_root)
    };

    let canonical_workdir = workdir.canonicalize().map_err(|_| not_synced())?;
    let canonical_root = root.canonicalize().map_err(|_| not_synced())?;
    if !canonical_root.starts_with(&canonical_workdir) {
        return Err(DomainError::Validation {
            field: "content_root".to_owned(),
            message: "resolves outside the repository working directory".to_owned(),
        });
    }
    if !canonical_root.is_dir() {
        return Err(not_synced());
    }
    Ok(canonical_root)
}
```

- [ ] **Step 5: Update the module doc's branch-model section**

Replace the `## p1 branch model` block (plans.rs:41-48):

```rust
//! ## Branch model
//!
//! The sync engine keeps one clone per repository and materializes each
//! branch's content into its own snapshot directory (see
//! `infra::git::layout`). A read for `(repo, branch)` is served from that
//! branch's snapshot when the repository has synced successfully
//! (`last_synced_at` set, `sync_error` clear) and the snapshot exists on
//! disk; anything else is [`DomainError::RepoNotSynced`].
```

- [ ] **Step 6: Rewrite — do not delete — the existing single-branch assertions**

Tests written for the p1 model assert that a non-default branch yields
`RepoNotSynced`. Those assertions are now **inverted**, not obsolete: each one
was protecting a real behavior (a read must fail when content isn't there), and
the replacement must still assert that — for a branch never materialized, not
for "any branch that isn't the default". Deleting them would silently drop the
coverage.

Find them:

```bash
grep -rn "RepoNotSynced\|default_branch" gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans_tests.rs \
    gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/bundles_tests.rs \
    gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/repos_tests.rs
```

For each hit that asserts "non-default branch ⇒ `RepoNotSynced`", rewrite it to
assert "branch with no materialized snapshot ⇒ `RepoNotSynced`" — same expected
error, different cause. A test named something like
`read_of_non_default_branch_is_rejected` becomes:

```rust
#[tokio::test]
async fn read_of_a_branch_with_no_snapshot_is_rejected() {
    let (services, ctx, tenant_id) = plans_fixture().await;
    let repo_id = seed_synced_repo(&services, &ctx, tenant_id).await;

    // The repository synced successfully, but this branch was never
    // materialized, so no snapshot directory exists for it.
    let err = services
        .plans
        .list_plans(&ctx, repo_id, "never-materialized")
        .await
        .expect_err("a branch with no snapshot must not read as empty");

    assert!(
        matches!(err, DomainError::RepoNotSynced { .. }),
        "unexpected error: {err:?}"
    );
}
```

The distinction matters: reading an unmaterialized branch must fail, not return
an empty plan list. An empty list would make a mistyped branch name look like a
repository with no tests.

- [ ] **Step 7: Run the tests**

```bash
cargo test -p qa-catalog require_synced 2>&1 | tail -20
cargo test -p qa-catalog 2>&1 | tail -20
```

Expected: PASS. The three new `require_synced` unit tests pass, and every
rewritten snapshot-absence test passes.

- [ ] **Step 8: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans.rs gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/plans_tests.rs
git commit -m "feat(qa-catalog): content reads resolve per-branch snapshots

require_synced no longer pins reads to default_branch; content_root_dir
resolves under the branch snapshot directory."
```

### Task 18: Wire the service — branch argument, force-sync, locks

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/repos.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/gear.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/config.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/test_support.rs`

- [ ] **Step 1: Add the TTL config knob**

In `src/config.rs`, add a field beside the existing `branch_refresh_interval_seconds`, following its exact serde/default style:

```rust
    /// How long a materialized branch snapshot is trusted before a content
    /// read triggers a re-sync. `0` disables the freshness cache. The launch
    /// path force-syncs regardless.
    #[serde(default = "default_branch_freshness_ttl_seconds")]
    pub branch_freshness_ttl_seconds: u64,
```

```rust
const fn default_branch_freshness_ttl_seconds() -> u64 {
    300
}
```

- [ ] **Step 2: Give `ReposService` the cache**

In `src/domain/service/repos.rs`, add `sync_cache: Arc<SyncCache>` to the struct and its `new()`. Then change `sync` to take the branch and honor the cache:

```rust
    /// Sync `branch` of repository `id` into its snapshot directory.
    ///
    /// `force` skips (and evicts) the freshness cache — what the launch path
    /// uses, so a run never reads a stale snapshot. Browse reads pass
    /// `false` and may be served from a fresh snapshot without a fetch.
    #[instrument(skip(self, ctx), fields(repo_id = %id, branch = %branch, force))]
    pub async fn sync(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        branch: &str,
        force: bool,
    ) -> Result<TestRepository, DomainError> {
```

Inside, after the existing scope/precheck block that yields `repo` and before touching the engine:

```rust
        let branch = if branch.trim().is_empty() {
            repo.default_branch.clone()
        } else {
            branch.trim().to_owned()
        };

        if force {
            self.sync_cache.invalidate(id, &branch).await;
        } else if self.sync_cache.is_fresh(id, &branch).await {
            return Ok(repo);
        }

        // Two-tier lock: the repo tier serializes every git mutation against
        // the shared object store; the branch tier collapses duplicate syncs
        // of the same branch. Order is always repo-then-branch — the reverse
        // would deadlock against a concurrent caller.
        let repo_lock = self.sync_cache.repo_lock(id).await;
        let branch_lock = self.sync_cache.branch_lock(id, &branch).await;
        let _repo_guard = repo_lock.lock().await;
        let _branch_guard = branch_lock.lock().await;

        // Re-check under the lock: a concurrent caller may have just synced.
        if !force && self.sync_cache.is_fresh(id, &branch).await {
            return Ok(repo);
        }
```

Then replace the engine call:

```rust
        let host_dir = crate::infra::git::layout::host_dir(&self.repos_dir, repo.id);
        let branch_workdir =
            crate::infra::git::layout::branch_workdir(&self.repos_dir, repo.id, &branch);
        let outcome = self
            .sync_engine
            .sync(
                &repo.url,
                &branch,
                credential.as_deref(),
                &host_dir,
                &branch_workdir,
            )
            .await;

        match outcome {
            Ok(result) => {
                self.sync_cache.mark_synced(id, &branch).await;
                self.record_sync_success(&sync_scope, tenant_id, id, result.branches)
                    .await
            }
            Err(engine_err) => {
                let sanitized = sanitize_sync_error(&engine_err.to_string(), credential.as_deref());
                self.record_sync_failure(&sync_scope, &repo, sanitized)
                    .await
            }
        }
```

- [ ] **Step 3: Invalidate on mutation and delete**

In `update` (repos.rs around line 273) the URL/content-root change already wipes the working copy. Extend it to clear the whole repository directory and the cache:

```rust
        let repo_root = self.repos_dir.join(id.to_string());
        if let Err(err) = tokio::fs::remove_dir_all(&repo_root).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            warn!("failed to clear the working area for {id}: {err}");
        }
        self.sync_cache.invalidate_repo(id).await;
```

Match the existing error-handling style at that site rather than the sketch above if they differ. Do the same in the delete path.

- [ ] **Step 4: Make `default_branch` mutable**

Delete the `# Immutable default_branch` doc block (repos.rs:171-181) and replace it with:

```rust
    /// # Mutable `default_branch`
    ///
    /// `default_branch` is updatable: with per-branch snapshots the column no
    /// longer *is* the identity of the working copy's contents, it is only
    /// the branch chosen when a caller names none. Changing it therefore
    /// needs no invalidation — the previously materialized branches stay
    /// valid and readable.
```

Then include `default_branch` in the fields `update` writes, per the `TestRepositoryUpdate` shape from Task 6 Step 2.

- [ ] **Step 5: Update the callers**

```bash
grep -rn "\.sync(" gears/qa-platform/qa-catalog/qa-catalog/src --include=*.rs | grep -v "sync_engine"
```

- REST sync handler: pass the branch from the request (add an optional `branch` query/body field to its DTO, defaulting to empty) and `force: true` — an operator hitting the sync endpoint means "fetch now".
- `PlansService` / `BundlesService` read paths: they do not call `sync`; they only read. Leave them.
- `src/gear.rs`: construct `SyncCache::new(Duration::from_secs(cfg.branch_freshness_ttl_seconds))`, wrap in `Arc`, and pass to `ReposService::new`.
- `src/test_support.rs`: same, and update both `RepoSyncPort` doubles (`NoopSyncEngine` at test_support.rs:211 and the ls-refs double at test_support.rs:238) to the new five-argument `sync` signature:

```rust
    async fn sync(
        &self,
        _url: &str,
        _branch: &str,
        _credential: Option<&str>,
        _host_dir: &std::path::Path,
        _branch_workdir: &std::path::Path,
    ) -> Result<SyncResult, DomainError> {
        Err(DomainError::Internal(
            "NoopSyncEngine::sync must not be called by these tests".to_owned(),
        ))
    }
```

- [ ] **Step 6: Build and run everything**

```bash
cargo build -p qa-catalog && cargo test -p qa-catalog 2>&1 | tail -30
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/src
git commit -m "feat(qa-catalog): sync takes a branch, with force-sync and two-tier locks

Browse reads may be served from a fresh snapshot; force=true evicts and
re-syncs, which is what a launch needs. default_branch becomes mutable
now that it no longer identifies the working copy's contents."
```

### Task 19: Multi-branch integration and concurrency tests

The concurrency test is the one that fails without the per-repo lock tier, so it earns its place.

**Files:**
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/tests/multi_branch_spike.rs` (rename to `multi_branch.rs`)

- [ ] **Step 1: Rename the spike into a real suite**

```bash
cd /Users/serhii.verestun/Virtuozzo/projects/fabric/gears-rust
git mv gears/qa-platform/qa-catalog/qa-catalog/tests/multi_branch_spike.rs \
       gears/qa-platform/qa-catalog/qa-catalog/tests/multi_branch.rs
```

Update its module doc first line to `//! Multi-branch working-copy behavior of the gix sync engine.`

- [ ] **Step 2: Add a test driving the real engine per branch**

Append to `tests/multi_branch.rs`:

```rust
use qa_catalog::domain::ports::repo_sync::RepoSyncPort;
use qa_catalog::infra::git::gix_sync::GixSyncEngine;
use qa_catalog::infra::git::layout::{branch_workdir, host_dir};

#[tokio::test]
async fn engine_materializes_each_branch_into_its_own_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = fixture_repo(tmp.path());
    let url = format!("file://{}", origin.display());
    let repos_dir = tmp.path().join("repos");
    let repo_id = uuid::Uuid::new_v4();
    let engine = GixSyncEngine;

    for branch in ["main", "release/5.0"] {
        let result = engine
            .sync(
                &url,
                branch,
                None,
                &host_dir(&repos_dir, repo_id),
                &branch_workdir(&repos_dir, repo_id, branch),
            )
            .await
            .unwrap_or_else(|e| panic!("sync of {branch} failed: {e}"));
        assert!(
            result.branches.iter().any(|b| b == branch),
            "advertised inventory must include {branch}"
        );
    }

    let main_marker = branch_workdir(&repos_dir, repo_id, "main").join("marker.txt");
    let release_marker = branch_workdir(&repos_dir, repo_id, "release/5.0").join("marker.txt");
    assert_eq!(
        std::fs::read_to_string(main_marker).unwrap(),
        "from-main\n"
    );
    assert_eq!(
        std::fs::read_to_string(release_marker).unwrap(),
        "from-release\n"
    );
}

#[tokio::test]
async fn syncing_a_branch_absent_from_the_remote_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = fixture_repo(tmp.path());
    let url = format!("file://{}", origin.display());
    let repos_dir = tmp.path().join("repos");
    let repo_id = uuid::Uuid::new_v4();

    let err = GixSyncEngine
        .sync(
            &url,
            "no-such-branch",
            None,
            &host_dir(&repos_dir, repo_id),
            &branch_workdir(&repos_dir, repo_id, "no-such-branch"),
        )
        .await
        .expect_err("a missing branch must not succeed");

    assert!(
        err.to_string().contains("not found on the remote"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn concurrent_syncs_of_different_branches_do_not_corrupt_each_other() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = fixture_repo(tmp.path());
    let url = format!("file://{}", origin.display());
    let repos_dir = tmp.path().join("repos");
    let repo_id = uuid::Uuid::new_v4();

    // Serialize through the same two-tier lock the service uses; without the
    // repo tier these concurrent fetches race on the shared object store.
    let cache = std::sync::Arc::new(qa_catalog::domain::service::SyncCache::new(
        std::time::Duration::ZERO,
    ));

    let mut handles = Vec::new();
    for branch in ["main", "release/5.0", "main", "release/5.0"] {
        let url = url.clone();
        let repos_dir = repos_dir.clone();
        let cache = std::sync::Arc::clone(&cache);
        handles.push(tokio::spawn(async move {
            let repo_lock = cache.repo_lock(repo_id).await;
            let branch_lock = cache.branch_lock(repo_id, branch).await;
            let _repo_guard = repo_lock.lock().await;
            let _branch_guard = branch_lock.lock().await;
            GixSyncEngine
                .sync(
                    &url,
                    branch,
                    None,
                    &host_dir(&repos_dir, repo_id),
                    &branch_workdir(&repos_dir, repo_id, branch),
                )
                .await
                .map(|_| ())
        }));
    }

    for handle in handles {
        handle
            .await
            .expect("task must not panic")
            .expect("every concurrent sync must succeed");
    }

    assert_eq!(
        std::fs::read_to_string(branch_workdir(&repos_dir, repo_id, "main").join("marker.txt"))
            .unwrap(),
        "from-main\n"
    );
    assert_eq!(
        std::fs::read_to_string(
            branch_workdir(&repos_dir, repo_id, "release/5.0").join("marker.txt")
        )
        .unwrap(),
        "from-release\n"
    );
}
```

If `GixSyncEngine`, `layout`, or `SyncCache` are not reachable from outside the crate, make the minimum modules `pub` in `src/lib.rs` — or, if the crate deliberately exposes nothing, move these three tests into an in-crate `#[cfg(test)]` module in `src/infra/git/gix_sync.rs` gated on `feature = "integration"` and keep only the pure-gix spike in `tests/`.

- [ ] **Step 3: Run the integration suite**

```bash
cargo test -p qa-catalog --features integration --test multi_branch 2>&1 | tail -30
```

Expected: PASS (4 tests).

- [ ] **Step 4: Run everything, including clippy and the architecture lints**

```bash
cargo test -p qa-catalog
cargo clippy -p qa-catalog -- -D warnings
cargo clippy -p qa-catalog-sdk -- -D warnings
cargo gears lint --dylint
```

Expected: all clean. `cargo gears lint --dylint` enforces the gear architecture rules (including DE0309 `#[domain_model]` coverage, which `SyncCache` satisfies).

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-catalog/qa-catalog/tests
git commit -m "test(qa-catalog): multi-branch materialization, missing branch, concurrency

The concurrency case is the one that fails without the per-repo lock
tier: every branch snapshot is built from one shared object store."
```

---

## Phase D — Documents

### Task 20: Reconcile PRD, DESIGN, DECOMPOSITION, and the continuation prompt

Spec §5 is the change list. Docs come last so they describe what was actually built.

**Files:**
- Modify: `gears/qa-platform/docs/PRD.md`
- Modify: `gears/qa-platform/docs/DESIGN.md`
- Modify: `gears/qa-platform/docs/DECOMPOSITION.md`
- Modify: `gears/qa-platform/docs/plans/2026-08-12-qa-platform-continuation-prompt.md`

- [ ] **Step 1: PRD**

At `PRD.md:272-274`, rewrite `cpt-cf-qa-fr-catalog-products`:

```markdown
The system MUST manage products and product folders, and every test
repository MUST belong to a product, so discovered plans and their runs can
be attributed to a product.

- **Rationale**: Products are how the catalog, the run history, and the
  analytics surfaces are scoped. Repository ownership is the attribution
  path; the source system additionally keyed a curated
  product-version-to-branch table, which it deleted in VHP-319 in favour of
  branch selection (per-platform default plus a launch-time override).
```

At `PRD.md:212`, drop `for use in product version branch mappings and` from `cpt-cf-qa-fr-catalog-branch-cache`, leaving launch-time branch selection as the justification.

- [ ] **Step 2: DESIGN**

- `DESIGN.md:221` — delete the `Product / ProductVersion` row's version half; the entity line becomes `| Product | qa-catalog | Product taxonomy; owns test repositories |`.
- `DESIGN.md:537` — in the qa-catalog schema list, drop `product_versions (...)`, add `product_id` to `test_repositories`, and change `products (name, folder)` to `products (name, product_key, description, folder)`.
- `DESIGN.md:539` — the renames paragraph mentions `product_versions.repo_branch_map` → `repo_branches`. Append that `product_versions` was subsequently removed entirely, so readers following old references stop looking for it.
- `DESIGN.md:560` — the tenant-prefixed-index rule cites `product_versions` under `products` as one of its two examples, and gives `idx_qa_versions_unique(tenant_id, product_id, version)`. Replace that example with `repo_branches` under `test_repositories` so the rule survives, keeping `idx_qa_branches_unique(tenant_id, repo_id, name)`.
- Add a new subsection near the qa-catalog data notes recording what was built:

```markdown
**Multi-branch working copies.** One `gix` clone per repository at
`<repos_dir>/<repo_id>/git` owns objects and refs; each branch's content is
materialized into `<repos_dir>/<repo_id>/branches/<branch_dir>` (see
`qa-catalog/src/infra/git/layout.rs`). Branch snapshots are plain
directories — no `.git`, and the repository index is never written for them,
because the index belongs to the shared clone and per-branch writes would
clobber one another. `branch_dir` appends an 8-character digest because
normalization is lossy: `release/5.0` and `release-5-0` would otherwise
share a directory.

Two lock tiers guard the shared object store: per-repository (every git
mutation) and per-`(repo, branch)` (duplicate syncs of one branch), always
taken repo-then-branch. Freshness is an in-memory TTL cache
(`branch_freshness_ttl_seconds`), deliberately not persisted — launches
force-sync, so a cache lost on restart costs one redundant fetch. The
consequence to know: `sync_error` stays repository-scoped, so a failed sync
of one branch fails reads of the others until the next success.

**`folder` is not `tests_folder`.** `Product::folder` is a UI grouping label
(`None` = root). The source system's `Product.tests_folder` was a different
thing — the top-level directory attributing *local* plans, baked into the
runner image, to a product. The new design is repo-backed only, so
`tests_folder` has no equivalent and repository ownership carries all
attribution.
```

- [ ] **Step 3: DECOMPOSITION**

- `:102` — remove `ProductVersion` from the entity list.
- `:106` — remove `product_versions` from the data list; note `test_repositories` now carries `product_id`.
- `:112` — the single-branch working-copy entry is resolved. Replace it with a description of the shipped multi-branch model and remove the two knock-on rules (immutable `default_branch`, and the products consequence), both of which no longer hold.
- `:121` — resolved. Replace with: cross-repo plans are served by N single-source bundles, one execution node each, matching the source system; `BundleRequest` stayed single-source and needed no change.
- `:122`, `:123` — delete. There is no product-version→branch requirement: the source system removed it in VHP-319, so these recorded "gaps" were requirements that never existed.
- Add to the tracked-follow-ups register: **`Product` parity is partial** — `key` and `description` are carried, `tests_folder` is deliberately absent (no local plans), and per-branch `sync_error` granularity is a known coarseness with a named fix (a persisted per-branch state table).
- Add to the tracked-follow-ups register: **products are the odd one out in the SDK client trait.** `create_repo`/`update_repo` and `create_custom_plan`/`update_custom_plan` all take a parameter struct (`NewTestRepository`, `TestRepositoryUpdate`, `NewCustomPlan`), but `create_product`/`update_product` take bare positional arguments — and after this change `update_product` carries `ctx, id, name, key, description, folder`, three of them adjacent `String`s. Transposing `name`, `key`, and `description` at a call site compiles silently. A `NewProduct` / `ProductUpdate` pair would match the trait's dominant idiom and close that footgun. Deliberately not done here: it is a cross-cutting change through the SDK trait, local client, service, repository, and REST handlers, and bundling it into this parity effort would widen the diff without serving the parity goal. Surfaced by the Tasks 6-7 code-quality review.

- [ ] **Step 4: Continuation prompt**

At `2026-08-12-qa-platform-continuation-prompt.md:169-179`, replace the three gap entries with a short resolved-state note: multi-branch working copies and product ownership are built; `BundleRequest` is unchanged and correct; the version→branch entries were withdrawn as non-requirements. Then state the qa-runs launch-path contract it must implement (spec §3.4): branch-first resolution (explicit → platform default → repo default), group plan files by repo, `FailedPrecondition` for an unpinned multi-repo plan, one `create_bundle` per group, one execution node per group, and `test_version` recorded as the branch label.

- [ ] **Step 5: Verify no stale references survive**

```bash
cd /Users/serhii.verestun/Virtuozzo/projects/fabric/gears-rust
grep -rn "product_version\|ProductVersion" gears/qa-platform/ --include=*.md --include=*.rs
```

Expected: only historical mentions that are explicitly framed as removed (the DESIGN renames paragraph, the DECOMPOSITION resolved entries, and this plan plus its spec). No live schema, entity, or requirement references.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/docs
git commit -m "docs(qa-platform): reconcile PRD/DESIGN/DECOMPOSITION with legacy parity

fr-catalog-products loses the version->branch clause and its false
rationale (the source system deleted that table in VHP-319). Records the
multi-branch working-copy model, the folder vs tests_folder distinction,
and closes DECOMPOSITION follow-ups 112 and 121; 122 and 123 are
withdrawn as non-requirements."
```

---

## Final verification

- [ ] **Run the full gate**

```bash
cd /Users/serhii.verestun/Virtuozzo/projects/fabric/gears-rust
cargo build -p qa-catalog -p qa-catalog-sdk
cargo test -p qa-catalog -p qa-catalog-sdk
cargo test -p qa-catalog --features integration
cargo clippy -p qa-catalog -p qa-catalog-sdk -- -D warnings
cargo gears lint --dylint
```

Every command must pass. Report the actual output — a skipped integration run is not a pass.

- [ ] **Confirm the spec's scope boundary held**

No file under a `qa-runs` directory was created or modified. Spec §3.4 was specification only.

```bash
git diff --name-only main... | grep -i "qa-runs" || echo "clean: no qa-runs changes"
```
