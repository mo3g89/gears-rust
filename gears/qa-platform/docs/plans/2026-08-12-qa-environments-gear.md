# qa-environments Gear Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `qa-environments` gear (DECOMPOSITION feature 2.1, `cpt-cf-qa-feature-environments`): target platform registry with credstore-referenced kubeconfigs, platform/pipeline variables, and lease state with acquire/release semantics.

**Architecture:** Standard ToolKit DDD-light gear pair (`qa-environments-sdk` + `qa-environments`) under `gears/qa-platform/qa-environments/`, mirroring the canonical reference `examples/toolkit/users-info/`. Lease semantics (the only non-CRUD domain logic) are pure functions developed test-first. Kubeconfig material lives in credstore; this gear stores only a reference string. Version-poll *probing* is out of scope (p2, needs execution plane) — only the `observed_version` column and event schema stub land here.

**Tech Stack:** Rust, ToolKit (`toolkit`, `toolkit-db` SecureORM, `toolkit-odata`, `toolkit-canonical-errors`), SeaORM, axum via OperationBuilder, `authz-resolver-sdk` PEP.

**Specs:** `gears/qa-platform/docs/PRD.md` (§5.3), `DESIGN.md` (§3.2 qa-environments, §3.7 schemas), `DECOMPOSITION.md` (2.1).

**Canonical reference:** `examples/toolkit/users-info/` — when any ToolKit API signature in this plan disagrees with what the compiler says, open the corresponding users-info file and match it; that example compiles in CI and is the source of truth.

**Branch:** work on `feature/qa-platform-specs` (already exists) or branch `feature/qa-environments` off it.

---

## File map (what gets created)

```
gears/qa-platform/qa-environments/
├── qa-environments-sdk/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs          # re-exports
│       ├── models.rs       # TargetPlatform, NewPlatform, PlatformPatch, Variable, LeaseMode, LeaseState, AcquireOutcome
│       ├── errors.rs       # QaEnvironmentsError (canonical)
│       └── client.rs       # QaEnvironmentsClientV1 trait
└── qa-environments/
    ├── Cargo.toml
    └── src/
        ├── lib.rs
        ├── config.rs       # QaEnvironmentsConfig
        ├── gear.rs         # #[toolkit::gear], init, DatabaseCapability, RestApiCapability
        ├── domain/
        │   ├── mod.rs
        │   ├── error.rs    # DomainError
        │   ├── lease.rs    # pure lease decision logic  ← TDD core
        │   ├── service/
        │   │   ├── mod.rs  # AppServices, resources, actions
        │   │   ├── platforms.rs
        │   │   ├── variables.rs
        │   │   └── leases.rs
        │   ├── repos/
        │   │   ├── mod.rs
        │   │   ├── platforms_repo.rs
        │   │   ├── variables_repo.rs
        │   │   └── leases_repo.rs
        │   └── local_client/
        │       ├── mod.rs
        │       └── client.rs
        ├── infra/
        │   ├── mod.rs
        │   └── storage/
        │       ├── mod.rs
        │       ├── entity/{mod,platform,platform_variable,pipeline_variable,platform_lease}.rs
        │       ├── mapper.rs
        │       ├── platforms_sea_repo.rs
        │       ├── variables_sea_repo.rs
        │       ├── leases_sea_repo.rs
        │       └── migrations/{mod,m20260812_000001_initial}.rs
        └── api/
            ├── mod.rs
            └── rest/
                ├── mod.rs
                ├── dto.rs
                ├── error.rs
                ├── handlers/{mod,platforms,variables,leases}.rs
                └── routes/{mod,platforms,variables,leases}.rs
Modify:
- Cargo.toml (workspace members)
- apps/cf-gears-example-server/src/registered_gears.rs
- apps/cf-gears-example-server/Cargo.toml
```

REST base path: `/qa/v1/...` (per DESIGN §3.3). Crate names: `qa-environments-sdk`, `qa-environments` (bss-style prefix, per ADR-0004).

---

### Task 1: Workspace scaffolding — two empty crates that compile

**Files:**
- Create: `gears/qa-platform/qa-environments/qa-environments-sdk/Cargo.toml`
- Create: `gears/qa-platform/qa-environments/qa-environments-sdk/src/lib.rs`
- Create: `gears/qa-platform/qa-environments/qa-environments/Cargo.toml`
- Create: `gears/qa-platform/qa-environments/qa-environments/src/lib.rs`
- Modify: `Cargo.toml` (workspace root, `members` array around line 26)

- [ ] **Step 1: Create the SDK Cargo.toml**

```toml
[package]
name = "qa-environments-sdk"
version = "0.1.0"
publish = false
edition.workspace = true
license.workspace = true
authors.workspace = true
description = "SDK for qa-environments gear: client trait, models, and error definitions"

[lints]
workspace = true

[dependencies]
uuid = { workspace = true }
time = { workspace = true }
async-trait = { workspace = true }
toolkit-canonical-errors = { workspace = true }
toolkit-gts = { workspace = true }
toolkit-security = { workspace = true }
toolkit-odata-macros = { workspace = true }
```

- [ ] **Step 2: Create SDK src/lib.rs (placeholder module doc only)**

```rust
//! SDK for the `qa-environments` gear.
//!
//! Public contract: client trait, transport-agnostic models, canonical errors.
```

- [ ] **Step 3: Create the gear Cargo.toml**

```toml
[package]
name = "qa-environments"
version = "0.1.0"
publish = false
edition.workspace = true
license.workspace = true
authors.workspace = true

[lints]
workspace = true

[dependencies]
qa-environments-sdk = { path = "../qa-environments-sdk" }

authz-resolver-sdk = { package = "cf-gears-authz-resolver-sdk", path = "../../../system/authz-resolver/authz-resolver-sdk" }
authz-resolver = { workspace = true }
credstore-sdk = { workspace = true }

anyhow = { workspace = true }
async-trait = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
inventory = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
utoipa = { workspace = true, features = ["time"] }
axum = { workspace = true, features = ["macros"] }
http = { workspace = true }
time = { workspace = true }
uuid = { workspace = true }
thiserror = { workspace = true }
sea-orm = { workspace = true }
sea-orm-migration = { workspace = true }

toolkit = { workspace = true }
toolkit-http = { workspace = true }
toolkit-db = { workspace = true, features = ["sqlite", "pg"] }
toolkit-db-macros = { workspace = true }
toolkit-security = { workspace = true }
toolkit-canonical-errors = { workspace = true, features = ["axum", "utoipa"] }
toolkit-odata = { workspace = true, features = ["with-utoipa"] }
toolkit-sdk = { workspace = true }
toolkit-macros = { workspace = true }

[dev-dependencies]
tokio-util = { workspace = true }
serde_json = { workspace = true }
```

Note: check the exact workspace-dependency names for `authz-resolver` and `credstore-sdk` in the root `Cargo.toml` `[workspace.dependencies]` table. If `credstore-sdk` is not a workspace dependency, use a `path = "../../../credstore/credstore-sdk"` dependency with the `package` name found in `gears/credstore/credstore-sdk/Cargo.toml`.

- [ ] **Step 4: Create gear src/lib.rs (module doc only for now)**

```rust
//! qa-environments gear: target platform registry, variables, and lease state.
```

- [ ] **Step 5: Add both crates to the workspace**

In root `Cargo.toml` `members`, after the `gears/file-storage/*` entries, add:

```toml
    "gears/qa-platform/qa-environments/qa-environments-sdk",
    "gears/qa-platform/qa-environments/qa-environments",
```

- [ ] **Step 6: Verify both crates compile**

Run: `cargo build -p qa-environments-sdk -p qa-environments`
Expected: `Finished` with no errors.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): scaffold sdk and gear crates in workspace"
```

---

### Task 2: SDK models and errors

**Files:**
- Create: `gears/qa-platform/qa-environments/qa-environments-sdk/src/models.rs`
- Create: `gears/qa-platform/qa-environments/qa-environments-sdk/src/errors.rs`
- Modify: `gears/qa-platform/qa-environments/qa-environments-sdk/src/lib.rs`

- [ ] **Step 1: Write models.rs**

SDK models carry no serde/utoipa (contract-layer purity — lints DE0101/DE0102 enforce this).

```rust
//! Transport-agnostic models for the qa-environments contract.

use time::OffsetDateTime;
use uuid::Uuid;

/// A registered target platform (system under test).
#[derive(Clone, Debug, PartialEq)]
pub struct TargetPlatform {
    pub id: Uuid,
    pub name: String,
    /// Optional association to a product in qa-catalog (by ID; no cross-gear FK).
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    /// Reference to the kubeconfig secret in credstore. Never the material itself.
    pub kubeconfig_credstore_ref: String,
    /// Operator-controlled availability toggle: an unavailable platform accepts no new leases.
    pub available: bool,
    /// Last product version observed on the platform (populated by the p2 version poller).
    pub observed_version: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Creation request for a target platform.
#[derive(Clone, Debug, PartialEq)]
pub struct NewPlatform {
    pub name: String,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    pub kubeconfig_credstore_ref: String,
}

/// Partial update; `None` = leave unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlatformPatch {
    pub name: Option<String>,
    pub product_id: Option<Option<Uuid>>,
    pub description: Option<Option<String>>,
    pub kubeconfig_credstore_ref: Option<String>,
    pub available: Option<bool>,
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
    HeldParallel { holders: Vec<Uuid> },
    /// Held exclusively by a single run.
    HeldExclusive { holder: Uuid },
}

/// Result of a lease acquisition attempt.
#[derive(Clone, Debug, PartialEq)]
pub enum AcquireOutcome {
    /// The lease was granted; the run may start.
    Acquired,
    /// The platform is occupied in a conflicting mode; the caller should queue.
    Busy { current: LeaseState },
}
```

- [ ] **Step 2: Write errors.rs**

```rust
//! Canonical error envelope for the qa-environments contract.

pub use toolkit_canonical_errors::CanonicalError as QaEnvironmentsError;
```

(Check how `users-info-sdk/src/lib.rs` re-exports `UsersInfoError` and mirror it exactly; if it wraps rather than aliases, wrap the same way.)

- [ ] **Step 3: Update lib.rs**

```rust
//! SDK for the `qa-environments` gear.

mod client;
mod errors;
mod models;

pub use client::QaEnvironmentsClientV1;
pub use errors::QaEnvironmentsError;
pub use models::{
    AcquireOutcome, LeaseMode, LeaseState, NewPlatform, NewVariable, PlatformPatch,
    TargetPlatform, Variable,
};
```

`client` does not exist yet — create an empty `client.rs` containing only `//! placeholder` and a commented-out export if needed to keep this step compiling, or do Steps 1–3 of Task 3 in the same change. Preferred: proceed straight to Task 3 and build once.

---

### Task 3: SDK client trait

**Files:**
- Create: `gears/qa-platform/qa-environments/qa-environments-sdk/src/client.rs`

- [ ] **Step 1: Write the client trait**

```rust
//! Object-safe client trait for inter-gear consumption via `ClientHub`.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::QaEnvironmentsError;
use crate::models::{
    AcquireOutcome, LeaseMode, LeaseState, NewPlatform, NewVariable, PlatformPatch,
    TargetPlatform, Variable,
};

/// Object-safe client for the qa-environments gear (Version 1).
///
/// Registered in `ClientHub`:
/// ```ignore
/// let envs = hub.get::<dyn QaEnvironmentsClientV1>()?;
/// ```
///
/// Primary consumer: the qa-runs dispatcher (lease operations, variable reads,
/// kubeconfig reference for `RunSpec` assembly).
#[async_trait]
pub trait QaEnvironmentsClientV1: Send + Sync {
    // ==================== Platforms ====================

    async fn get_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TargetPlatform, QaEnvironmentsError>;

    async fn list_platforms(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<TargetPlatform>, QaEnvironmentsError>;

    async fn create_platform(
        &self,
        ctx: &SecurityContext,
        new: NewPlatform,
    ) -> Result<TargetPlatform, QaEnvironmentsError>;

    async fn update_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: PlatformPatch,
    ) -> Result<TargetPlatform, QaEnvironmentsError>;

    /// Fails with `FailedPrecondition` if the platform currently holds any lease.
    async fn delete_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaEnvironmentsError>;

    // ==================== Variables ====================

    /// Variables for env assembly: global pipeline variables plus (if
    /// `platform_id` is Some) that platform's variables. Precedence is applied
    /// by the caller (qa-runs), not here.
    async fn list_variables(
        &self,
        ctx: &SecurityContext,
        platform_id: Option<Uuid>,
    ) -> Result<Vec<Variable>, QaEnvironmentsError>;

    async fn upsert_variable(
        &self,
        ctx: &SecurityContext,
        var: NewVariable,
    ) -> Result<Variable, QaEnvironmentsError>;

    async fn delete_variable(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaEnvironmentsError>;

    // ==================== Leases ====================

    /// Attempt to acquire the platform for a run. Never blocks; a conflicting
    /// hold returns `AcquireOutcome::Busy` and the caller queues.
    async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
        mode: LeaseMode,
    ) -> Result<AcquireOutcome, QaEnvironmentsError>;

    /// Release a run's hold. Idempotent: releasing a non-held run is Ok.
    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError>;

    async fn get_lease(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError>;
}
```

- [ ] **Step 2: Build the SDK**

Run: `cargo build -p qa-environments-sdk`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add gears/qa-platform/qa-environments/qa-environments-sdk
git commit -m "feat(qa-environments): SDK models, errors, and client trait"
```

---

### Task 4: Pure lease decision logic (TDD core)

This is the heart of the gear and of `cpt-cf-qa-fr-env-lease` / the queue's correctness. It is a pure function — no DB, no async.

**Files:**
- Create: `gears/qa-platform/qa-environments/qa-environments/src/domain/mod.rs`
- Create: `gears/qa-platform/qa-environments/qa-environments/src/domain/lease.rs`
- Modify: `gears/qa-platform/qa-environments/qa-environments/src/lib.rs`

- [ ] **Step 1: Wire modules**

`src/lib.rs`:

```rust
//! qa-environments gear: target platform registry, variables, and lease state.

pub mod domain;
```

`src/domain/mod.rs`:

```rust
pub mod lease;
```

- [ ] **Step 2: Write the failing tests first**

In `src/domain/lease.rs` (tests at the bottom of the file per repo convention of `*_tests.rs` or inline `#[cfg(test)]` — use inline here, the module is small):

```rust
//! Pure lease decision logic.
//!
//! Semantics (PRD `cpt-cf-qa-fr-runs-queue`, `cpt-cf-qa-fr-env-lease`):
//! - free + parallel      → acquired (parallel hold, 1 holder)
//! - free + exclusive     → acquired (exclusive hold)
//! - parallel + parallel  → acquired (holder appended)
//! - parallel + exclusive → busy
//! - exclusive + anything → busy
//! - release removes the run; last holder out → free
//! - release is idempotent (unknown run → state unchanged)

use qa_environments_sdk::{AcquireOutcome, LeaseMode, LeaseState};
use toolkit_macros::domain_model;
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    fn run(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn free_platform_grants_parallel() {
        let (outcome, state) = decide_acquire(&LeaseState::Free, run(1), LeaseMode::Parallel);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(state, LeaseState::HeldParallel { holders: vec![run(1)] });
    }

    #[test]
    fn free_platform_grants_exclusive() {
        let (outcome, state) = decide_acquire(&LeaseState::Free, run(1), LeaseMode::Exclusive);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(state, LeaseState::HeldExclusive { holder: run(1) });
    }

    #[test]
    fn parallel_hold_admits_another_parallel() {
        let current = LeaseState::HeldParallel { holders: vec![run(1)] };
        let (outcome, state) = decide_acquire(&current, run(2), LeaseMode::Parallel);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(
            state,
            LeaseState::HeldParallel { holders: vec![run(1), run(2)] }
        );
    }

    #[test]
    fn parallel_hold_rejects_exclusive() {
        let current = LeaseState::HeldParallel { holders: vec![run(1)] };
        let (outcome, state) = decide_acquire(&current, run(2), LeaseMode::Exclusive);
        assert_eq!(outcome, AcquireOutcome::Busy { current: current.clone() });
        assert_eq!(state, current, "state must not change on Busy");
    }

    #[test]
    fn exclusive_hold_rejects_parallel_and_exclusive() {
        let current = LeaseState::HeldExclusive { holder: run(1) };
        for mode in [LeaseMode::Parallel, LeaseMode::Exclusive] {
            let (outcome, state) = decide_acquire(&current, run(2), mode);
            assert_eq!(outcome, AcquireOutcome::Busy { current: current.clone() });
            assert_eq!(state, current);
        }
    }

    #[test]
    fn acquire_is_idempotent_for_same_run() {
        // Re-acquiring by the same run (dispatcher retry after crash) must not duplicate holders.
        let current = LeaseState::HeldParallel { holders: vec![run(1)] };
        let (outcome, state) = decide_acquire(&current, run(1), LeaseMode::Parallel);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(state, LeaseState::HeldParallel { holders: vec![run(1)] });

        let current = LeaseState::HeldExclusive { holder: run(1) };
        let (outcome, state) = decide_acquire(&current, run(1), LeaseMode::Exclusive);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(state, LeaseState::HeldExclusive { holder: run(1) });
    }

    #[test]
    fn release_last_parallel_holder_frees() {
        let current = LeaseState::HeldParallel { holders: vec![run(1)] };
        assert_eq!(decide_release(&current, run(1)), LeaseState::Free);
    }

    #[test]
    fn release_one_of_many_keeps_parallel() {
        let current = LeaseState::HeldParallel { holders: vec![run(1), run(2)] };
        assert_eq!(
            decide_release(&current, run(1)),
            LeaseState::HeldParallel { holders: vec![run(2)] }
        );
    }

    #[test]
    fn release_exclusive_frees() {
        let current = LeaseState::HeldExclusive { holder: run(1) };
        assert_eq!(decide_release(&current, run(1)), LeaseState::Free);
    }

    #[test]
    fn release_unknown_run_is_noop() {
        let current = LeaseState::HeldParallel { holders: vec![run(1)] };
        assert_eq!(decide_release(&current, run(9)), current);
        assert_eq!(decide_release(&LeaseState::Free, run(9)), LeaseState::Free);
        let excl = LeaseState::HeldExclusive { holder: run(1) };
        assert_eq!(decide_release(&excl, run(9)), excl);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p qa-environments lease`
Expected: compile error — `decide_acquire` / `decide_release` not found.

- [ ] **Step 4: Implement the two functions (above the tests module)**

```rust
/// Decide an acquisition attempt. Returns the outcome and the resulting state
/// (unchanged state when `Busy`). Pure — persistence is the caller's job.
#[domain_model]
pub struct LeaseDecision;

pub fn decide_acquire(
    current: &LeaseState,
    run_id: Uuid,
    mode: LeaseMode,
) -> (AcquireOutcome, LeaseState) {
    match (current, mode) {
        (LeaseState::Free, LeaseMode::Parallel) => (
            AcquireOutcome::Acquired,
            LeaseState::HeldParallel { holders: vec![run_id] },
        ),
        (LeaseState::Free, LeaseMode::Exclusive) => (
            AcquireOutcome::Acquired,
            LeaseState::HeldExclusive { holder: run_id },
        ),
        (LeaseState::HeldParallel { holders }, LeaseMode::Parallel) => {
            let mut holders = holders.clone();
            if !holders.contains(&run_id) {
                holders.push(run_id);
            }
            (AcquireOutcome::Acquired, LeaseState::HeldParallel { holders })
        }
        (LeaseState::HeldExclusive { holder }, LeaseMode::Exclusive) if *holder == run_id => (
            AcquireOutcome::Acquired,
            LeaseState::HeldExclusive { holder: run_id },
        ),
        (state, _) => (
            AcquireOutcome::Busy { current: state.clone() },
            state.clone(),
        ),
    }
}

/// Decide a release. Idempotent: unknown run leaves the state unchanged.
pub fn decide_release(current: &LeaseState, run_id: Uuid) -> LeaseState {
    match current {
        LeaseState::Free => LeaseState::Free,
        LeaseState::HeldExclusive { holder } if *holder == run_id => LeaseState::Free,
        LeaseState::HeldExclusive { holder } => LeaseState::HeldExclusive { holder: *holder },
        LeaseState::HeldParallel { holders } => {
            let holders: Vec<Uuid> = holders.iter().copied().filter(|h| *h != run_id).collect();
            if holders.is_empty() {
                LeaseState::Free
            } else {
                LeaseState::HeldParallel { holders }
            }
        }
    }
}
```

Note the `#[domain_model]` marker struct: if the DE0309 lint complains about the free functions' module, attach `#[domain_model]` per the lint's guidance; if it complains about `LeaseDecision` being unused, delete the struct — the lint only requires the attribute on non-private types, and free functions are fine.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p qa-environments lease`
Expected: 9 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-environments/qa-environments/src
git commit -m "feat(qa-environments): pure lease decision logic with full semantics tests"
```

---

### Task 5: Domain error type

**Files:**
- Create: `gears/qa-platform/qa-environments/qa-environments/src/domain/error.rs`
- Modify: `gears/qa-platform/qa-environments/qa-environments/src/domain/mod.rs`

- [ ] **Step 1: Write error.rs**

(Compare with `examples/toolkit/users-info/users-info/src/domain/error.rs` for the `Database` variant's exact payload shape and adjust if it differs.)

```rust
use thiserror::Error;
use toolkit_macros::domain_model;
use uuid::Uuid;

#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    #[error("platform {id} not found")]
    PlatformNotFound { id: Uuid },

    #[error("variable {id} not found")]
    VariableNotFound { id: Uuid },

    #[error("platform name '{name}' already exists")]
    PlatformNameExists { name: String },

    #[error("platform {id} is unavailable")]
    PlatformUnavailable { id: Uuid },

    #[error("platform {id} holds an active lease and cannot be deleted")]
    PlatformLeased { id: Uuid },

    #[error("validation failed on {field}: {message}")]
    Validation { field: String, message: String },

    #[error("concurrent lease update, retry")]
    LeaseConflict,

    #[error("access denied")]
    Forbidden,

    #[error("database error: {0}")]
    Database(String),
}
```

- [ ] **Step 2: Add `pub mod error;` to domain/mod.rs, build**

Run: `cargo build -p qa-environments`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add -A gears/qa-platform/qa-environments && git commit -m "feat(qa-environments): domain error type"
```

---

### Task 6: Migration and SeaORM entities

**Files:**
- Create: `.../qa-environments/src/infra/mod.rs`, `.../src/infra/storage/mod.rs`
- Create: `.../src/infra/storage/migrations/mod.rs`, `.../migrations/m20260812_000001_initial.rs`
- Create: `.../src/infra/storage/entity/{mod,platform,platform_variable,pipeline_variable,platform_lease}.rs`
- Modify: `src/lib.rs` (`pub mod infra;`)

Schema per DESIGN §3.7. Lease holders are a JSON array column plus a `version` integer for optimistic concurrency (realizes `LeaseConflict`).

- [ ] **Step 1: Write the migration** (`m20260812_000001_initial.rs`)

Follow the users-info migration shape (backend match + `execute_unprepared`). Postgres branch shown; write the Sqlite branch with TEXT ids/timestamps exactly as users-info does (MySql branch may mirror Postgres with VARCHAR(36) ids):

```rust
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG: &str = r"
CREATE TABLE IF NOT EXISTS qa_platforms (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    product_id UUID NULL,
    description TEXT NULL,
    kubeconfig_credstore_ref VARCHAR(1024) NOT NULL,
    available BOOLEAN NOT NULL DEFAULT TRUE,
    observed_version VARCHAR(255) NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platforms_tenant_name ON qa_platforms(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_platform_variables (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    platform_id UUID NOT NULL REFERENCES qa_platforms(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    value TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platform_vars_unique ON qa_platform_variables(platform_id, name);

CREATE TABLE IF NOT EXISTS qa_pipeline_variables (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    value TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_pipeline_vars_unique ON qa_pipeline_variables(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_platform_leases (
    platform_id UUID PRIMARY KEY NOT NULL REFERENCES qa_platforms(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL,
    mode VARCHAR(16) NOT NULL,
    holders JSONB NOT NULL DEFAULT '[]',
    version BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL
);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => PG,
            // Sqlite: same tables with TEXT ids, TEXT timestamps, TEXT holders JSON
            sea_orm::DatabaseBackend::Sqlite => SQLITE,
            sea_orm::DatabaseBackend::MySql => MYSQL,
        };
        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "DROP TABLE IF EXISTS qa_platform_leases; DROP TABLE IF EXISTS qa_pipeline_variables; DROP TABLE IF EXISTS qa_platform_variables; DROP TABLE IF EXISTS qa_platforms;",
        )
        .await?;
        Ok(())
    }
}
```

Write the `SQLITE` and `MYSQL` constants in full (translate types as users-info migration 000001 does: UUID→TEXT/VARCHAR(36), TIMESTAMPTZ→TEXT/TIMESTAMP, JSONB→TEXT/JSON, BOOLEAN→INTEGER for sqlite).

`migrations/mod.rs` (mirror users-info `migrations/mod.rs` Migrator):

```rust
use sea_orm_migration::prelude::*;

mod m20260812_000001_initial;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260812_000001_initial::Migration)]
    }
}
```

- [ ] **Step 2: Write the entities**

`entity/platform.rs`:

```rust
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_platforms")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    pub kubeconfig_credstore_ref: String,
    pub available: bool,
    pub observed_version: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

`entity/platform_variable.rs`:

```rust
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_platform_variables")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub platform_id: Uuid,
    pub name: String,
    pub value: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

`entity/pipeline_variable.rs`: identical shape minus `platform_id`, table `qa_pipeline_variables`.

`entity/platform_lease.rs`:

```rust
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_platform_leases")]
#[secure(tenant_col = "tenant_id", resource_col = "platform_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub platform_id: Uuid,
    pub tenant_id: Uuid,
    /// "free" | "parallel" | "exclusive" — denormalized from holders for indexing/display.
    pub mode: String,
    /// JSON array of holder run UUIDs.
    pub holders: Json,
    /// Optimistic-concurrency version; every write does WHERE version = read_version.
    pub version: i64,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

`entity/mod.rs`:

```rust
pub mod pipeline_variable;
pub mod platform;
pub mod platform_lease;
pub mod platform_variable;
```

`infra/mod.rs`: `pub mod storage;` — `storage/mod.rs`:

```rust
pub mod entity;
pub mod migrations;
pub mod mapper;
mod leases_sea_repo;
mod platforms_sea_repo;
mod variables_sea_repo;

pub use leases_sea_repo::OrmLeasesRepository;
pub use platforms_sea_repo::OrmPlatformsRepository;
pub use variables_sea_repo::OrmVariablesRepository;
```

(The three repo files come in Task 7; to keep this task compiling on its own, create them as empty files with a `//! placeholder` doc comment and comment out the `mod`/`pub use` lines until Task 7 — or land Tasks 6+7 as one build unit. Preferred: land 6 and 7 together, running the build at the end of Task 7.)

- [ ] **Step 3: Commit (with Task 7)**

---

### Task 7: Mapper and ORM repositories

**Files:**
- Create: `.../src/infra/storage/mapper.rs`
- Create: `.../src/infra/storage/{platforms,variables,leases}_sea_repo.rs`
- Create: `.../src/domain/repos/{mod,platforms_repo,variables_repo,leases_repo}.rs`
- Modify: `src/domain/mod.rs` (`pub mod repos;`)

Before writing, open `examples/toolkit/users-info/users-info/src/infra/storage/cities_sea_repo.rs` and copy its exact use of `DBRunner`, `.secure()`, `.scope_with(&scope)`, and error mapping — the code below is the target shape; keep the reference file's exact method-call chain if it differs.

- [ ] **Step 1: Repo traits** (`domain/repos/platforms_repo.rs`)

```rust
use async_trait::async_trait;
use qa_environments_sdk::{NewPlatform, PlatformPatch, TargetPlatform};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

#[async_trait]
pub trait PlatformsRepository: Send + Sync {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TargetPlatform>, DomainError>;

    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<TargetPlatform>, DomainError>;

    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewPlatform,
    ) -> Result<TargetPlatform, DomainError>;

    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        patch: PlatformPatch,
    ) -> Result<Option<TargetPlatform>, DomainError>;

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;
}
```

`variables_repo.rs` — trait `VariablesRepository` with `list(platform_id: Option<Uuid>)` (None → pipeline vars only... **no**: None → pipeline vars; Some → pipeline vars UNION that platform's vars is service-level composition; the repo exposes `list_pipeline`, `list_for_platform`, `upsert`, `delete`):

```rust
use async_trait::async_trait;
use qa_environments_sdk::{NewVariable, Variable};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

#[async_trait]
pub trait VariablesRepository: Send + Sync {
    async fn list_pipeline<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Variable>, DomainError>;

    async fn list_for_platform<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<Vec<Variable>, DomainError>;

    async fn upsert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        var: NewVariable,
    ) -> Result<Variable, DomainError>;

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;
}
```

`leases_repo.rs` — read + compare-and-swap write:

```rust
use async_trait::async_trait;
use qa_environments_sdk::LeaseState;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Versioned lease row for optimistic concurrency.
pub struct VersionedLease {
    pub state: LeaseState,
    pub version: i64,
}

#[async_trait]
pub trait LeasesRepository: Send + Sync {
    /// Missing row reads as Free with version 0.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<VersionedLease, DomainError>;

    /// Writes the new state iff the stored version still equals
    /// `expected_version` (insert when 0/absent). Returns `LeaseConflict`
    /// on version mismatch — callers retry the read-decide-write loop.
    async fn compare_and_set<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        platform_id: Uuid,
        expected_version: i64,
        new_state: &LeaseState,
    ) -> Result<(), DomainError>;
}
```

Add `#[domain_model]` to `VersionedLease` (non-private domain struct).

- [ ] **Step 2: mapper.rs** — entity Model ↔ SDK model conversions

```rust
use qa_environments_sdk::{LeaseState, TargetPlatform, Variable};
use uuid::Uuid;

use super::entity::{pipeline_variable, platform, platform_lease, platform_variable};

pub fn platform_to_sdk(m: platform::Model) -> TargetPlatform {
    TargetPlatform {
        id: m.id,
        name: m.name,
        product_id: m.product_id,
        description: m.description,
        kubeconfig_credstore_ref: m.kubeconfig_credstore_ref,
        available: m.available,
        observed_version: m.observed_version,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

pub fn platform_var_to_sdk(m: platform_variable::Model) -> Variable {
    Variable { id: m.id, platform_id: Some(m.platform_id), name: m.name, value: m.value }
}

pub fn pipeline_var_to_sdk(m: pipeline_variable::Model) -> Variable {
    Variable { id: m.id, platform_id: None, name: m.name, value: m.value }
}

/// holders JSON + mode column → LeaseState. Defensive: unknown mode reads as Free.
pub fn lease_to_state(m: &platform_lease::Model) -> LeaseState {
    let holders: Vec<Uuid> =
        serde_json::from_value(m.holders.clone()).unwrap_or_default();
    match (m.mode.as_str(), holders.as_slice()) {
        ("exclusive", [h, ..]) => LeaseState::HeldExclusive { holder: *h },
        ("parallel", hs) if !hs.is_empty() => LeaseState::HeldParallel { holders },
        _ => LeaseState::Free,
    }
}

/// LeaseState → (mode, holders json) column pair.
pub fn state_to_columns(state: &LeaseState) -> (String, serde_json::Value) {
    match state {
        LeaseState::Free => ("free".into(), serde_json::json!([])),
        LeaseState::HeldParallel { holders } => {
            ("parallel".into(), serde_json::to_value(holders).unwrap_or_default())
        }
        LeaseState::HeldExclusive { holder } => {
            ("exclusive".into(), serde_json::json!([holder]))
        }
    }
}
```

- [ ] **Step 3: ORM repos**

`platforms_sea_repo.rs` — implement `PlatformsRepository` with SecureORM. Model on `cities_sea_repo.rs`; shape:

```rust
use async_trait::async_trait;
use qa_environments_sdk::{NewPlatform, PlatformPatch, TargetPlatform};
use sea_orm::{ActiveModelTrait, ActiveValue};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::PlatformsRepository;
use crate::infra::storage::entity::platform;
use crate::infra::storage::mapper::platform_to_sdk;

pub struct OrmPlatformsRepository;

#[async_trait]
impl PlatformsRepository for OrmPlatformsRepository {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TargetPlatform>, DomainError> {
        // Copy the exact secure-find idiom from cities_sea_repo::get —
        // Entity::find_by_id(...).secure()... with scope applied — then:
        // Ok(model.map(platform_to_sdk))
        todo_use_reference_idiom(runner, scope, id).await
    }
    // list / create / update / delete follow the same reference idioms:
    //  - create: build ActiveModel with ActiveValue::Set for every column,
    //    id = Uuid::new_v4(), tenant_id from the passed tenant, timestamps now
    //  - update: fetch scoped, apply Some(...) patch fields, set updated_at
    //  - delete: scoped delete, Ok(rows_affected > 0)
    //  - map unique-violation DB errors on name to DomainError::PlatformNameExists
}
```

**Do not leave `todo_use_reference_idiom` in the final code** — that placeholder marks where the exact SecureORM call chain must be copied from `cities_sea_repo.rs` (it is ~10 lines per method; the chain compiles differently across toolkit-db versions, so copying from the in-repo reference is the reliable move). Every method body must be complete before this task's build step.

`variables_sea_repo.rs`: same pattern over both variable entities; `upsert` = scoped find by (platform_id/name or tenant/name) → update value, else insert.

`leases_sea_repo.rs`:

```rust
use async_trait::async_trait;
use qa_environments_sdk::LeaseState;
use sea_orm::{ActiveModelTrait, ActiveValue, ColumnTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{LeasesRepository, VersionedLease};
use crate::infra::storage::entity::platform_lease;
use crate::infra::storage::mapper::{lease_to_state, state_to_columns};

pub struct OrmLeasesRepository;

#[async_trait]
impl LeasesRepository for OrmLeasesRepository {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<VersionedLease, DomainError> {
        // scoped find_by_id on qa_platform_leases (reference idiom);
        // None → Ok(VersionedLease { state: LeaseState::Free, version: 0 })
        // Some(m) → Ok(VersionedLease { state: lease_to_state(&m), version: m.version })
    }

    async fn compare_and_set<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        platform_id: Uuid,
        expected_version: i64,
        new_state: &LeaseState,
    ) -> Result<(), DomainError> {
        let (mode, holders) = state_to_columns(new_state);
        // expected_version == 0 and no row → insert new row with version 1
        // otherwise scoped update ... .filter(platform_lease::Column::Version.eq(expected_version))
        //   setting mode/holders/version = expected_version + 1/updated_at
        // rows_affected == 0 → Err(DomainError::LeaseConflict)
    }
}
```

Complete both method bodies with the reference idioms before building.

- [ ] **Step 4: Build**

Run: `cargo build -p qa-environments`
Expected: success. Iterate on SecureORM call-chain compile errors against the reference file until clean.

- [ ] **Step 5: Commit Tasks 6+7**

```bash
git add -A gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): schema migration, entities, and secure ORM repositories"
```

---

### Task 8: Domain services with PEP authorization

**Files:**
- Create: `.../src/domain/service/{mod,platforms,variables,leases}.rs`
- Modify: `src/domain/mod.rs` (`pub mod service;`)

Study `examples/toolkit/users-info/users-info/src/domain/service/mod.rs` (resources/actions constants, `AppServices`, `DbProvider` alias) and `cities.rs` (PEP call + `db` usage in a method) first; reuse those exact idioms.

- [ ] **Step 1: service/mod.rs — resources, actions, AppServices**

```rust
use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use authz_resolver_sdk::pep::ResourceType;
use toolkit_db::{DBProvider, DbError};
use toolkit_macros::domain_model;

use crate::domain::repos::{LeasesRepository, PlatformsRepository, VariablesRepository};

mod leases;
mod platforms;
mod variables;

pub use leases::LeasesService;
pub use platforms::PlatformsService;
pub use variables::VariablesService;

/// DB provider alias (mirrors users-info).
pub type DbProvider = DBProvider<DbError>;

pub mod resources {
    use authz_resolver_sdk::pep::ResourceType;
    use toolkit_security::pep_properties;

    // Declare supported properties exactly as users-info service/mod.rs does.
    pub static PLATFORM: ResourceType = ResourceType {
        name: "qa.platform",
        supported_properties: &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    };
    pub static VARIABLE: ResourceType = ResourceType {
        name: "qa.variable",
        supported_properties: &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    };
    pub static LEASE: ResourceType = ResourceType {
        name: "qa.lease",
        supported_properties: &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    };
}
// NOTE: copy the literal ResourceType construction syntax from users-info —
// if its fields differ (e.g. builder or const fn), match it.

pub mod actions {
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const CREATE: &str = "create";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
    pub const ACQUIRE: &str = "acquire";
    pub const RELEASE: &str = "release";
}

/// Composition of all qa-environments services.
#[domain_model]
pub struct AppServices<P, V, L>
where
    P: PlatformsRepository,
    V: VariablesRepository,
    L: LeasesRepository,
{
    pub platforms: PlatformsService<P, L>,
    pub variables: VariablesService<V>,
    pub leases: LeasesService<L>,
}

impl<P, V, L> AppServices<P, V, L>
where
    P: PlatformsRepository,
    V: VariablesRepository,
    L: LeasesRepository,
{
    pub fn new(
        platforms_repo: Arc<P>,
        variables_repo: Arc<V>,
        leases_repo: Arc<L>,
        db: Arc<DbProvider>,
        authz: Arc<dyn AuthZResolverClient>,
    ) -> Self {
        let enforcer = PolicyEnforcer::new(authz); // match users-info construction exactly
        Self {
            platforms: PlatformsService::new(db.clone(), platforms_repo, leases_repo.clone(), enforcer.clone()),
            variables: VariablesService::new(db.clone(), variables_repo, enforcer.clone()),
            leases: LeasesService::new(db, leases_repo, enforcer),
        }
    }
}
```

- [ ] **Step 2: platforms.rs service**

Methods: `get_platform`, `list_platforms`, `create_platform`, `update_platform`, `delete_platform`. Every method: PEP → scope → `let db = self.db.db().await?` (copy users-info connection-acquisition idiom) → repo call. `delete_platform` first reads the lease via the leases repo and returns `DomainError::PlatformLeased` unless it is `LeaseState::Free` (realizes the SDK's documented FailedPrecondition). `create_platform` validates: non-empty `name` (≤255), non-empty `kubeconfig_credstore_ref` → `DomainError::Validation` otherwise.

- [ ] **Step 3: leases.rs service — the retry loop around the pure logic**

```rust
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_environments_sdk::{AcquireOutcome, LeaseMode, LeaseState};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::instrument;
use uuid::Uuid;

use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::lease::{decide_acquire, decide_release};
use crate::domain::repos::LeasesRepository;

const CAS_MAX_RETRIES: usize = 5;

#[domain_model]
pub struct LeasesService<L: LeasesRepository> {
    db: Arc<DbProvider>,
    repo: Arc<L>,
    policy_enforcer: PolicyEnforcer,
}

impl<L: LeasesRepository> LeasesService<L> {
    pub fn new(db: Arc<DbProvider>, repo: Arc<L>, policy_enforcer: PolicyEnforcer) -> Self {
        Self { db, repo, policy_enforcer }
    }

    #[instrument(skip(self, ctx), fields(platform_id = %platform_id, run_id = %run_id))]
    pub async fn acquire(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
        mode: LeaseMode,
    ) -> Result<AcquireOutcome, DomainError> {
        // PEP: action ACQUIRE on resources::LEASE with resource id platform_id
        // (copy the access_scope call shape from users-info cities.rs)
        let scope = /* enforcer call */;
        let conn = /* db acquisition idiom */;
        let tenant_id = /* subject tenant from ctx, as users-info does */;

        for _ in 0..CAS_MAX_RETRIES {
            let current = self.repo.get(&conn, &scope, platform_id).await?;
            let (outcome, new_state) = decide_acquire(&current.state, run_id, mode);
            if matches!(outcome, AcquireOutcome::Busy { .. }) {
                return Ok(outcome); // no write needed
            }
            match self
                .repo
                .compare_and_set(&conn, &scope, tenant_id, platform_id, current.version, &new_state)
                .await
            {
                Ok(()) => return Ok(outcome),
                Err(DomainError::LeaseConflict) => continue, // raced; re-read and retry
                Err(e) => return Err(e),
            }
        }
        Err(DomainError::LeaseConflict)
    }

    #[instrument(skip(self, ctx), fields(platform_id = %platform_id, run_id = %run_id))]
    pub async fn release(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
    ) -> Result<LeaseState, DomainError> {
        // same PEP + CAS-retry structure with decide_release; return the new state
    }

    pub async fn get(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<LeaseState, DomainError> {
        // PEP GET + repo.get → Ok(versioned.state)
    }
}
```

Complete the elided expressions with the exact users-info idioms (PEP call, `db()` acquisition, tenant extraction). `acquire` must also verify the platform exists and `available == true` — inject `Arc<P: PlatformsRepository>` if you prefer, or do it in the REST/local-client layer via `PlatformsService`; the chosen place must return `PlatformNotFound`/`PlatformUnavailable`. Simplest correct option: `LeasesService` also gets the platforms repo and checks before the CAS loop — do that, and note the availability check is a read (a platform going unavailable does not evict existing holders; it only blocks new acquisitions).

- [ ] **Step 4: variables.rs service** — `list_for_env(platform_id: Option<Uuid>)` returning pipeline vars + platform vars (both scoped), `upsert`, `delete`, with `Validation` on empty/oversized name (reuse the run-parameter charset rule `^[A-Za-z_][A-Za-z0-9_]*$` — same contract, PRD `cpt-cf-qa-fr-env-variables`).

- [ ] **Step 5: Unit tests for the CAS retry loop**

Add `service/leases_tests.rs` (or `#[cfg(test)] mod` in leases.rs) with a hand-rolled mock `LeasesRepository` (in-memory `Mutex<HashMap<Uuid, (LeaseState, i64)>>`) that can be programmed to fail `compare_and_set` with `LeaseConflict` N times before succeeding. Tests:

```text
- acquire_free_platform_persists_parallel_hold
- acquire_busy_platform_returns_busy_without_write (mock records write count == 0)
- acquire_retries_on_cas_conflict_then_succeeds (program 2 conflicts; expect Ok, 3 CAS calls)
- acquire_gives_up_after_max_retries (always conflict; expect Err(LeaseConflict))
- release_is_idempotent (release unknown run → Ok(state unchanged), CAS may be skipped or write same state)
```

Write the mock and all five tests in full; follow the mock style of `examples/toolkit/users-info/users-info/src/domain/service/tests_pdp_deny.rs` for constructing a permissive `PolicyEnforcer` test double (users-info's `test_support.rs` shows how to build a `SecurityContext` and mock AuthZ — reuse those helpers by copying the pattern, not importing the crate).

- [ ] **Step 6: Run tests**

Run: `cargo test -p qa-environments`
Expected: lease decision tests (Task 4) + CAS loop tests PASS.

- [ ] **Step 7: Commit**

```bash
git add -A gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): domain services with PEP authz and CAS lease loop"
```

---

### Task 9: REST layer — DTOs, error mapping, handlers, routes

**Files:**
- Create: `.../src/api/mod.rs` (`pub mod rest;`), `.../src/api/rest/mod.rs`
- Create: `.../src/api/rest/dto.rs`, `error.rs`
- Create: `.../src/api/rest/handlers/{mod,platforms,variables,leases}.rs`
- Create: `.../src/api/rest/routes/{mod,platforms,variables,leases}.rs`
- Modify: `src/lib.rs` (`pub mod api;`)

- [ ] **Step 1: dto.rs** — REST DTOs (serde + utoipa here, never in SDK):

```rust
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use qa_environments_sdk as sdk;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PlatformDto {
    pub id: Uuid,
    pub name: String,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    /// Reference only; secret material is never returned.
    pub kubeconfig_credstore_ref: String,
    pub available: bool,
    pub observed_version: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<sdk::TargetPlatform> for PlatformDto {
    fn from(p: sdk::TargetPlatform) -> Self {
        Self {
            id: p.id,
            name: p.name,
            product_id: p.product_id,
            description: p.description,
            kubeconfig_credstore_ref: p.kubeconfig_credstore_ref,
            available: p.available,
            observed_version: p.observed_version,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePlatformReq {
    pub name: String,
    pub product_id: Option<Uuid>,
    pub description: Option<String>,
    pub kubeconfig_credstore_ref: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdatePlatformReq {
    pub name: Option<String>,
    #[serde(default, with = "::serde_with::rust::double_option")]
    pub product_id: Option<Option<Uuid>>,
    #[serde(default, with = "::serde_with::rust::double_option")]
    pub description: Option<Option<String>>,
    pub kubeconfig_credstore_ref: Option<String>,
    pub available: Option<bool>,
}
// If serde_with is not already a workspace dependency, replace double_option
// with plain Option fields and treat null-vs-absent uniformly (document it).

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct VariableDto {
    pub id: Uuid,
    pub platform_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpsertVariableReq {
    pub platform_id: Option<Uuid>,
    pub name: String,
    pub value: String,
}

/// Lease view for the platform detail page (PRD: engineers must see why a run waits).
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
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
```

Add the corresponding `From` impls for `VariableDto` and request→SDK conversions (`CreatePlatformReq → sdk::NewPlatform`, `UpdatePlatformReq → sdk::PlatformPatch`, `UpsertVariableReq → sdk::NewVariable`) — write them out.

- [ ] **Step 2: error.rs** — `From<DomainError> for CanonicalError` using the canonical prelude, mirroring `users-info/src/api/rest/error.rs`:

```rust
use toolkit::api::canonical_prelude::*;

use crate::domain::error::DomainError;

#[resource_error(gts_id!("cf.qa.environments.platform.v1~"))]
struct PlatformResourceError;

#[resource_error(gts_id!("cf.qa.environments.variable.v1~"))]
struct VariableResourceError;

#[resource_error(gts_id!("cf.qa.environments.lease.v1~"))]
struct LeaseResourceError;

impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        match &e {
            DomainError::PlatformNotFound { id } => {
                PlatformResourceError::not_found(format!("Platform {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }
            DomainError::VariableNotFound { id } => {
                VariableResourceError::not_found(format!("Variable {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }
            DomainError::PlatformNameExists { name } => {
                PlatformResourceError::already_exists(format!("Platform '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }
            DomainError::PlatformUnavailable { id } => {
                LeaseResourceError::failed_precondition(format!(
                    "Platform {id} is unavailable for new runs"
                ))
                .create()
            }
            DomainError::PlatformLeased { id } => {
                PlatformResourceError::failed_precondition(format!(
                    "Platform {id} holds an active lease and cannot be deleted"
                ))
                .create()
            }
            DomainError::Validation { field, message } => PlatformResourceError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION")
                .create(),
            DomainError::LeaseConflict => {
                LeaseResourceError::aborted("Concurrent lease update, retry").create()
            }
            DomainError::Forbidden => PlatformResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),
            DomainError::Database(_) => {
                tracing::error!(error = ?e, "Database error occurred");
                CanonicalError::internal("An internal database error occurred").create()
            }
        }
    }
}
```

If any builder method (`failed_precondition`, `aborted`) differs, check `libs/toolkit-canonical-errors/src/` for the exact constructor names and align.

- [ ] **Step 3: handlers** — one file per resource, users-info handler shape (`Extension(ctx)`, `Extension(svc)`, Path/Json extractors, `ApiResult` from the canonical prelude). Representative (platforms.rs):

```rust
use axum::Extension;
use axum::extract::Path;
use uuid::Uuid;

use super::{ApiResult, Json, created_json, no_content};
use crate::api::rest::dto::{CreatePlatformReq, LeaseDto, PlatformDto, UpdatePlatformReq};
use crate::gear::ConcreteAppServices;
use toolkit_security::SecurityContext;

#[tracing::instrument(skip(svc, ctx))]
pub async fn list_platforms(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
) -> ApiResult<Json<Vec<PlatformDto>>> {
    let platforms = svc.platforms.list_platforms(&ctx).await?;
    Ok(Json(platforms.into_iter().map(PlatformDto::from).collect()))
}

#[tracing::instrument(skip(svc, ctx), fields(platform.id = %id))]
pub async fn get_platform(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PlatformDto>> {
    Ok(Json(svc.platforms.get_platform(&ctx, id).await?.into()))
}

// create_platform (Json<CreatePlatformReq> → 201 created_json)
// update_platform (Path + Json<UpdatePlatformReq> → 200)
// delete_platform (Path → 204 no_content)
// get_platform_lease (Path → Json<LeaseDto>)
```

Write all six bodies in full following the two shown. Match the `ApiResult`/`Json`/`created_json`/`no_content` imports to what `users-info/src/api/rest/handlers/mod.rs` re-exports from `toolkit::api::canonical_prelude` — declare the same prelude imports in our `handlers/mod.rs`.

`variables.rs`: `list_variables` (query param `platform_id` optional via `axum::extract::Query`), `upsert_variable`, `delete_variable`. `leases.rs` is view-only at REST level (`get_platform_lease`) — acquire/release are **SDK-only** operations for the qa-runs dispatcher (they exist on the trait, not as public REST endpoints; exposing run-lifecycle mutations over REST here would bypass qa-runs orchestration).

- [ ] **Step 4: routes** — OperationBuilder per endpoint under `/qa/v1/...` (users-info routes shape, `.authenticated()`, `.standard_errors()`/explicit error_4xx/500 registration, `require_license_features` if the users-info `License` pattern applies — copy `routes/mod.rs` glue including the `License` type):

| Method | Path | Handler |
|--------|------|---------|
| GET | `/qa/v1/platforms` | list_platforms |
| POST | `/qa/v1/platforms` | create_platform |
| GET | `/qa/v1/platforms/{id}` | get_platform |
| PATCH | `/qa/v1/platforms/{id}` | update_platform |
| DELETE | `/qa/v1/platforms/{id}` | delete_platform |
| GET | `/qa/v1/platforms/{id}/lease` | get_platform_lease |
| GET | `/qa/v1/variables` | list_variables |
| PUT | `/qa/v1/variables` | upsert_variable |
| DELETE | `/qa/v1/variables/{id}` | delete_variable |

`routes/mod.rs` exposes `pub(crate) fn register_routes(router, openapi, service) -> Router` that threads the service `Extension` exactly as users-info `routes/mod.rs` does.

- [ ] **Step 5: Build**

Run: `cargo build -p qa-environments`
Expected: success.

- [ ] **Step 6: Commit**

```bash
git add -A gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): REST DTOs, canonical error mapping, handlers, and routes"
```

---

### Task 10: Local client, gear bootstrap, config

**Files:**
- Create: `.../src/domain/local_client/{mod,client}.rs`
- Create: `.../src/config.rs`
- Create: `.../src/gear.rs`
- Modify: `src/lib.rs`, `src/domain/mod.rs`

- [ ] **Step 1: config.rs**

```rust
use serde::Deserialize;

/// Typed configuration for the qa-environments gear (YAML section `qa-environments`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QaEnvironmentsConfig {
    /// Max variables returned per env-assembly query.
    pub max_variables: usize,
}

impl Default for QaEnvironmentsConfig {
    fn default() -> Self {
        Self { max_variables: 500 }
    }
}
```

- [ ] **Step 2: local_client/client.rs** — implement `QaEnvironmentsClientV1` by delegating to `AppServices`, converting `DomainError → CanonicalError` (the SDK error) via the Task 9 `From` impl:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use qa_environments_sdk::{
    AcquireOutcome, LeaseMode, LeaseState, NewPlatform, NewVariable, PlatformPatch,
    QaEnvironmentsClientV1, QaEnvironmentsError, TargetPlatform, Variable,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::gear::ConcreteAppServices;

pub struct QaEnvironmentsLocalClient {
    services: Arc<ConcreteAppServices>,
}

impl QaEnvironmentsLocalClient {
    pub fn new(services: Arc<ConcreteAppServices>) -> Self {
        Self { services }
    }
}

#[async_trait]
impl QaEnvironmentsClientV1 for QaEnvironmentsLocalClient {
    async fn get_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TargetPlatform, QaEnvironmentsError> {
        self.services.platforms.get_platform(ctx, id).await.map_err(Into::into)
    }
    // ... implement every trait method the same one-line delegation way.
}
```

Write all delegations out (11 methods).

- [ ] **Step 3: gear.rs** — users-info gear.rs shape:

```rust
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use sea_orm_migration::MigrationTrait;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use toolkit_db::DbError;
use tracing::info;

use authz_resolver_sdk::AuthZResolverClient;
use qa_environments_sdk::QaEnvironmentsClientV1;

use crate::api::rest::routes;
use crate::config::QaEnvironmentsConfig;
use crate::domain::local_client::QaEnvironmentsLocalClient;
use crate::domain::service::AppServices;
use crate::infra::storage::{OrmLeasesRepository, OrmPlatformsRepository, OrmVariablesRepository};

pub(crate) type ConcreteAppServices =
    AppServices<OrmPlatformsRepository, OrmVariablesRepository, OrmLeasesRepository>;

#[toolkit::gear(
    name = "qa-environments",
    deps = [authz_resolver],
    capabilities = [db, rest]
)]
pub struct QaEnvironments {
    service: OnceLock<Arc<ConcreteAppServices>>,
}

impl Default for QaEnvironments {
    fn default() -> Self {
        Self { service: OnceLock::new() }
    }
}

#[async_trait]
impl Gear for QaEnvironments {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let _cfg: QaEnvironmentsConfig = ctx.config_or_default()?;
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;

        let services = Arc::new(AppServices::new(
            Arc::new(OrmPlatformsRepository),
            Arc::new(OrmVariablesRepository),
            Arc::new(OrmLeasesRepository),
            db,
            authz,
        ));

        self.service
            .set(services.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        ctx.client_hub()
            .register::<dyn QaEnvironmentsClientV1>(Arc::new(QaEnvironmentsLocalClient::new(
                services,
            )));
        Ok(())
    }
}

impl DatabaseCapability for QaEnvironments {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for QaEnvironments {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering qa-environments REST routes");
        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();
        Ok(routes::register_routes(router, openapi, service))
    }
}
```

- [ ] **Step 4: lib.rs final form**

```rust
//! qa-environments gear: target platform registry, variables, and lease state.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-environments`).

pub mod api;
pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;

pub use gear::QaEnvironments;
```

- [ ] **Step 5: Build + clippy**

Run: `cargo build -p qa-environments && cargo clippy -p qa-environments -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add -A gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): gear bootstrap, local client, and config"
```

---

### Task 11: Register in the example server and smoke-test

**Files:**
- Modify: `apps/cf-gears-example-server/Cargo.toml`
- Modify: `apps/cf-gears-example-server/src/registered_gears.rs`

- [ ] **Step 1: Add optional dependency + feature**

In `apps/cf-gears-example-server/Cargo.toml`, follow the `mini-chat` optional-gear pattern found there: add

```toml
qa-environments = { path = "../../gears/qa-platform/qa-environments/qa-environments", optional = true }
```

and a feature `qa-platform = ["dep:qa-environments"]` (extend this feature with the other qa gears as they land).

- [ ] **Step 2: Link in registered_gears.rs**

In the `=== Optional Gears ===` section:

```rust
#[cfg(feature = "qa-platform")]
use qa_environments as _;
```

- [ ] **Step 3: Build the server with the feature**

Run: `cargo build -p cf-gears-example-server --features qa-platform`
Expected: success.

- [ ] **Step 4: Smoke run**

Run: `cargo run -p cf-gears-example-server --features qa-platform -- --list-gears`
Expected: output includes `qa-environments`.

Then start the server with the repo's dev config (check `apps/cf-gears-example-server` README/config for the exact invocation used in CI) and verify `GET /openapi.json` contains `/qa/v1/platforms`.

- [ ] **Step 5: Commit**

```bash
git add apps/cf-gears-example-server
git commit -m "feat(qa-environments): register gear in example server behind qa-platform feature"
```

---

### Task 12: Scoping tests and final verification

**Files:**
- Create: `.../qa-environments/src/test_support.rs` (adapted from `examples/toolkit/users-info/users-info/src/test_support.rs`)
- Create: `.../qa-environments/src/domain/service/tests_tenant_scoping.rs`
- Modify: `src/lib.rs` (`#[cfg(test)] pub mod test_support;` — match how users-info gates it)

- [ ] **Step 1: Port test support**

Copy `users-info/src/test_support.rs` and adapt: in-memory SQLite DB with this gear's `Migrator`, mock AuthZ client (permissive + denying variants), `SecurityContext` builders for two tenants. Keep helper names identical to users-info so the test files read the same.

- [ ] **Step 2: Write tenant-scoping tests** (model on `users-info/src/domain/service/tests_tenant_scoping.rs`)

```text
- platform_created_in_tenant_a_invisible_to_tenant_b (create via ctx_a; get via ctx_b → PlatformNotFound;
  list via ctx_b → empty)
- variables_scoped_by_tenant (upsert pipeline var in A; list_for_env in B → empty)
- lease_scoped_by_tenant (acquire in A; get_lease in B → must NOT reveal state: expect
  the PEP/scope to yield not-found or Free per scoping semantics — assert B cannot
  observe A's holders)
- pdp_deny_blocks_create (denying AuthZ mock → create_platform returns Forbidden)
```

Write each test in full against the ported helpers.

- [ ] **Step 3: Run the full gear test suite**

Run: `cargo test -p qa-environments`
Expected: all tests pass (lease semantics, CAS loop, scoping, DTO conversions if added).

- [ ] **Step 4: Architecture lints**

Run: `cargo clippy -p qa-environments -p qa-environments-sdk -- -D warnings && cargo gears lint --dylint`
Expected: clean (contract purity DE0101/DE0102, domain-model DE0309, versioned-path lints all pass).

- [ ] **Step 5: Update GEARS.md**

Add a `QA Platform` section entry in `docs/GEARS.md` under Business Logic Gears listing the environments scenarios with `[x] p1` for what landed, linking `gears/qa-platform/docs/PRD.md` and `DESIGN.md`.

- [ ] **Step 6: Final commit**

```bash
git add -A
git commit -m "test(qa-environments): tenant scoping and PDP-deny tests; register in GEARS.md"
```

---

## Plan self-review notes (already applied)

- **Spec coverage**: `cpt-cf-qa-fr-env-platforms` → Tasks 6–11; `cpt-cf-qa-fr-env-variables` → Tasks 6–9; `cpt-cf-qa-fr-env-lease` → Tasks 4, 7, 8; `cpt-cf-qa-fr-env-version-poll` → deliberately **not** in this plan (p2, blocked on execution plane; only `observed_version` column + event stub land — DECOMPOSITION 2.1 note). Constraints `no-kube` (no kube deps anywhere) and `platform-delegation` (credstore by reference; no secret material columns) hold by construction.
- **Known deliberate deviations**: lease acquire/release are SDK-only (not REST) — rationale in Task 9 Step 3; PRD's REST table lists only the lease *view*.
- **Reference-idiom placeholders**: Tasks 7 and 8 contain two marked spots where the exact SecureORM/PEP call chains must be copied from the named users-info files. These are *look-up instructions to a specific file*, not unresolved design: the semantics, signatures, and error mapping around them are fully specified. Executors: resolve them before the task's build step; the plan fails its own rules if any survive into a commit.
- **Type consistency**: `ConcreteAppServices` = `AppServices<OrmPlatformsRepository, OrmVariablesRepository, OrmLeasesRepository>` used consistently in gear.rs, handlers, local client. SDK names (`QaEnvironmentsClientV1`, `LeaseMode`, `AcquireOutcome`) consistent across Tasks 2, 3, 8, 10.
```
