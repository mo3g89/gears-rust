#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for the CAS (compare-and-swap) retry loop in `LeasesService`.
//!
//! These tests use a hand-rolled in-memory mock for `LeasesRepository` (it
//! ignores the `DBRunner` argument entirely — the trait requires one, but the
//! mock never touches a database) plus the shared `PlatformsRepository`/
//! `AuthZResolverClient` test doubles in [`super::test_support`]. The real
//! PEP flow (`PolicyEnforcer::access_scope`) runs unmodified; only the PDP
//! backend is faked.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use qa_environments_sdk::{AcquireOutcome, LeaseMode, LeaseState};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::DbProvider;
use super::leases::{CAS_MAX_RETRIES, LeasesService};
use super::test_support::{
    MockPlatformsRepository, PermissiveAuthZ, ctx, platform, test_db_provider,
};
use crate::domain::error::DomainError;
use crate::domain::repos::{LeasesRepository, VersionedLease};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// In-memory `LeasesRepository` double: `platform_id -> (state, version)`.
///
/// `compare_and_set` can be programmed to fail with `LeaseConflict` a fixed
/// number of times before succeeding, to exercise the retry loop.
#[derive(Default)]
struct MockLeasesRepository {
    rows: Mutex<HashMap<Uuid, (LeaseState, i64)>>,
    fail_cas_times: Mutex<usize>,
    cas_calls: Mutex<usize>,
}

impl MockLeasesRepository {
    fn new() -> Self {
        Self::default()
    }

    fn seeded(platform_id: Uuid, state: LeaseState, version: i64) -> Self {
        let repo = Self::new();
        repo.rows
            .lock()
            .unwrap()
            .insert(platform_id, (state, version));
        repo
    }

    /// Program the next `n` `compare_and_set` calls to fail with `LeaseConflict`.
    fn program_conflicts(&self, n: usize) {
        *self.fail_cas_times.lock().unwrap() = n;
    }

    fn cas_call_count(&self) -> usize {
        *self.cas_calls.lock().unwrap()
    }

    fn stored(&self, platform_id: Uuid) -> Option<(LeaseState, i64)> {
        self.rows.lock().unwrap().get(&platform_id).cloned()
    }
}

#[async_trait]
impl LeasesRepository for MockLeasesRepository {
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<VersionedLease, DomainError> {
        let rows = self.rows.lock().unwrap();
        Ok(match rows.get(&platform_id) {
            Some((state, version)) => VersionedLease {
                state: state.clone(),
                version: *version,
            },
            None => VersionedLease {
                state: LeaseState::Free,
                version: 0,
            },
        })
    }

    async fn compare_and_set<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        platform_id: Uuid,
        expected_version: i64,
        new_state: &LeaseState,
    ) -> Result<(), DomainError> {
        *self.cas_calls.lock().unwrap() += 1;

        {
            let mut fail = self.fail_cas_times.lock().unwrap();
            if *fail > 0 {
                *fail -= 1;
                return Err(DomainError::LeaseConflict);
            }
        }

        let mut rows = self.rows.lock().unwrap();
        let current_version = rows.get(&platform_id).map_or(0, |(_, v)| *v);
        if current_version != expected_version {
            return Err(DomainError::LeaseConflict);
        }
        rows.insert(platform_id, (new_state.clone(), expected_version + 1));
        Ok(())
    }
}

fn build_service(
    leases_repo: Arc<MockLeasesRepository>,
    platforms_repo: Arc<MockPlatformsRepository>,
    db: Arc<DbProvider>,
) -> LeasesService<MockLeasesRepository, MockPlatformsRepository> {
    let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
    LeasesService::new(db, leases_repo, platforms_repo, enforcer)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acquire_free_platform_persists_parallel_hold() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    let platforms_repo = Arc::new(MockPlatformsRepository::with_platform(platform(
        platform_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let outcome = svc
        .acquire(&ctx(tenant_id), platform_id, run_id, LeaseMode::Parallel)
        .await
        .unwrap();

    assert_eq!(outcome, AcquireOutcome::Acquired);
    assert_eq!(
        leases_repo.stored(platform_id),
        Some((
            LeaseState::HeldParallel {
                holders: vec![run_id]
            },
            1
        ))
    );
    assert_eq!(leases_repo.cas_call_count(), 1);
}

#[tokio::test]
async fn acquire_busy_platform_returns_busy_without_write() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let holder = Uuid::new_v4();
    let other_run = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        platform_id,
        LeaseState::HeldExclusive { holder },
        1,
    ));
    let platforms_repo = Arc::new(MockPlatformsRepository::with_platform(platform(
        platform_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let outcome = svc
        .acquire(&ctx(tenant_id), platform_id, other_run, LeaseMode::Parallel)
        .await
        .unwrap();

    assert_eq!(
        outcome,
        AcquireOutcome::Busy {
            current: LeaseState::HeldExclusive { holder }
        }
    );
    assert_eq!(leases_repo.cas_call_count(), 0, "Busy must not write");
    // State must be untouched.
    assert_eq!(
        leases_repo.stored(platform_id),
        Some((LeaseState::HeldExclusive { holder }, 1))
    );
}

#[tokio::test]
async fn acquire_retries_on_cas_conflict_then_succeeds() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    leases_repo.program_conflicts(2);
    let platforms_repo = Arc::new(MockPlatformsRepository::with_platform(platform(
        platform_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let outcome = svc
        .acquire(&ctx(tenant_id), platform_id, run_id, LeaseMode::Exclusive)
        .await
        .unwrap();

    assert_eq!(outcome, AcquireOutcome::Acquired);
    assert_eq!(
        leases_repo.cas_call_count(),
        3,
        "2 conflicts + 1 successful write"
    );
    assert_eq!(
        leases_repo.stored(platform_id),
        Some((LeaseState::HeldExclusive { holder: run_id }, 1))
    );
}

#[tokio::test]
async fn acquire_gives_up_after_max_retries() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    leases_repo.program_conflicts(usize::MAX);
    let platforms_repo = Arc::new(MockPlatformsRepository::with_platform(platform(
        platform_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let err = svc
        .acquire(&ctx(tenant_id), platform_id, run_id, LeaseMode::Parallel)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::LeaseConflict),
        "expected LeaseConflict, got {err:?}"
    );
    assert_eq!(leases_repo.cas_call_count(), CAS_MAX_RETRIES);
}

#[tokio::test]
async fn release_is_idempotent() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let unknown_run = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    // Nothing seeded: the repo reads back Free/version 0 for an unknown platform.
    let leases_repo = Arc::new(MockLeasesRepository::new());
    let platforms_repo = Arc::new(MockPlatformsRepository::none());
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let state = svc
        .release(&ctx(tenant_id), platform_id, unknown_run)
        .await
        .unwrap();

    assert_eq!(state, LeaseState::Free);
    assert_eq!(
        leases_repo.cas_call_count(),
        0,
        "no-op release must not write"
    );
}

#[tokio::test]
async fn release_removes_holder_and_persists() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let run1 = Uuid::new_v4();
    let run2 = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        platform_id,
        LeaseState::HeldParallel {
            holders: vec![run1, run2],
        },
        1,
    ));
    // Release doesn't touch platforms_repo; no platform is registered.
    let platforms_repo = Arc::new(MockPlatformsRepository::none());
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let state = svc
        .release(&ctx(tenant_id), platform_id, run1)
        .await
        .unwrap();

    assert_eq!(
        state,
        LeaseState::HeldParallel {
            holders: vec![run2]
        }
    );
    assert_eq!(
        leases_repo.stored(platform_id),
        Some((
            LeaseState::HeldParallel {
                holders: vec![run2]
            },
            2
        ))
    );
    assert_eq!(leases_repo.cas_call_count(), 1);
}

#[tokio::test]
async fn release_retries_on_cas_conflict() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        platform_id,
        LeaseState::HeldExclusive { holder: run_id },
        1,
    ));
    leases_repo.program_conflicts(1);
    let platforms_repo = Arc::new(MockPlatformsRepository::none());
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let state = svc
        .release(&ctx(tenant_id), platform_id, run_id)
        .await
        .unwrap();

    assert_eq!(state, LeaseState::Free);
    assert_eq!(
        leases_repo.cas_call_count(),
        2,
        "1 conflict + 1 successful write"
    );
}

#[tokio::test]
async fn acquire_idempotent_reacquire_skips_write() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    // run_id already exclusively holds the lease; re-acquiring in the same
    // mode must be a no-op (NIT fix): the decided state equals the current
    // state, so the CAS write is skipped entirely.
    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        platform_id,
        LeaseState::HeldExclusive { holder: run_id },
        1,
    ));
    let platforms_repo = Arc::new(MockPlatformsRepository::with_platform(platform(
        platform_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let outcome = svc
        .acquire(&ctx(tenant_id), platform_id, run_id, LeaseMode::Exclusive)
        .await
        .unwrap();

    assert_eq!(outcome, AcquireOutcome::Acquired);
    assert_eq!(
        leases_repo.cas_call_count(),
        0,
        "idempotent re-acquire must not write"
    );
    assert_eq!(
        leases_repo.stored(platform_id),
        Some((LeaseState::HeldExclusive { holder: run_id }, 1)),
        "version must not bump on a no-op re-acquire"
    );
}

#[tokio::test]
async fn acquire_unavailable_platform_rejected() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    let platforms_repo = Arc::new(MockPlatformsRepository::with_platform(platform(
        platform_id,
        false,
    )));
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let err = svc
        .acquire(&ctx(tenant_id), platform_id, run_id, LeaseMode::Parallel)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::PlatformUnavailable { id } if id == platform_id),
        "expected PlatformUnavailable, got {err:?}"
    );
    assert_eq!(leases_repo.cas_call_count(), 0);
}

#[tokio::test]
async fn acquire_unknown_platform_rejected() {
    let db = test_db_provider().await;
    let platform_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    let platforms_repo = Arc::new(MockPlatformsRepository::none());
    let svc = build_service(Arc::clone(&leases_repo), platforms_repo, db);

    let err = svc
        .acquire(&ctx(tenant_id), platform_id, run_id, LeaseMode::Parallel)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::PlatformNotFound { id } if id == platform_id),
        "expected PlatformNotFound, got {err:?}"
    );
    assert_eq!(leases_repo.cas_call_count(), 0);
}
