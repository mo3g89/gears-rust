#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for the CAS (compare-and-swap) retry loop in `LeasesService`.
//!
//! These tests use a hand-rolled in-memory mock for `LeasesRepository` (it
//! ignores the `DBRunner` argument entirely — the trait requires one, but the
//! mock never touches a database) plus the shared `EnvironmentsRepository`/
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
    MockEnvironmentsRepository, PermissiveAuthZ, ctx, environment, test_db_provider,
};
use crate::domain::error::DomainError;
use crate::domain::repos::{LeasesRepository, VersionedLease};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// In-memory `LeasesRepository` double: `environment_id -> (state, version)`.
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

    fn seeded(environment_id: Uuid, state: LeaseState, version: i64) -> Self {
        let repo = Self::new();
        repo.rows
            .lock()
            .unwrap()
            .insert(environment_id, (state, version));
        repo
    }

    /// Program the next `n` `compare_and_set` calls to fail with `LeaseConflict`.
    fn program_conflicts(&self, n: usize) {
        *self.fail_cas_times.lock().unwrap() = n;
    }

    fn cas_call_count(&self) -> usize {
        *self.cas_calls.lock().unwrap()
    }

    fn stored(&self, environment_id: Uuid) -> Option<(LeaseState, i64)> {
        self.rows.lock().unwrap().get(&environment_id).cloned()
    }
}

#[async_trait]
impl LeasesRepository for MockLeasesRepository {
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        environment_id: Uuid,
    ) -> Result<VersionedLease, DomainError> {
        let rows = self.rows.lock().unwrap();
        Ok(match rows.get(&environment_id) {
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
        environment_id: Uuid,
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
        let current_version = rows.get(&environment_id).map_or(0, |(_, v)| *v);
        if current_version != expected_version {
            return Err(DomainError::LeaseConflict);
        }
        rows.insert(environment_id, (new_state.clone(), expected_version + 1));
        Ok(())
    }
}

fn build_service(
    leases_repo: Arc<MockLeasesRepository>,
    environments_repo: Arc<MockEnvironmentsRepository>,
    db: Arc<DbProvider>,
) -> LeasesService<MockLeasesRepository, MockEnvironmentsRepository> {
    let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
    LeasesService::new(db, leases_repo, environments_repo, enforcer)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acquire_free_environment_persists_parallel_hold() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let outcome = svc
        .acquire(&ctx(tenant_id), environment_id, run_id, LeaseMode::Parallel)
        .await
        .unwrap();

    assert_eq!(outcome, AcquireOutcome::Acquired);
    assert_eq!(
        leases_repo.stored(environment_id),
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
async fn acquire_busy_environment_returns_busy_without_write() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let holder = Uuid::new_v4();
    let other_run = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        environment_id,
        LeaseState::HeldExclusive { holder },
        1,
    ));
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let outcome = svc
        .acquire(
            &ctx(tenant_id),
            environment_id,
            other_run,
            LeaseMode::Parallel,
        )
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
        leases_repo.stored(environment_id),
        Some((LeaseState::HeldExclusive { holder }, 1))
    );
}

#[tokio::test]
async fn acquire_retries_on_cas_conflict_then_succeeds() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    leases_repo.program_conflicts(2);
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let outcome = svc
        .acquire(
            &ctx(tenant_id),
            environment_id,
            run_id,
            LeaseMode::Exclusive,
        )
        .await
        .unwrap();

    assert_eq!(outcome, AcquireOutcome::Acquired);
    assert_eq!(
        leases_repo.cas_call_count(),
        3,
        "2 conflicts + 1 successful write"
    );
    assert_eq!(
        leases_repo.stored(environment_id),
        Some((LeaseState::HeldExclusive { holder: run_id }, 1))
    );
}

#[tokio::test]
async fn acquire_gives_up_after_max_retries() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    leases_repo.program_conflicts(usize::MAX);
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let err = svc
        .acquire(&ctx(tenant_id), environment_id, run_id, LeaseMode::Parallel)
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
    let environment_id = Uuid::new_v4();
    let unknown_run = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    // Nothing seeded: the repo reads back Free/version 0 for an unknown environment.
    let leases_repo = Arc::new(MockLeasesRepository::new());
    let environments_repo = Arc::new(MockEnvironmentsRepository::none());
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let state = svc
        .release(&ctx(tenant_id), environment_id, unknown_run)
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
    let environment_id = Uuid::new_v4();
    let run1 = Uuid::new_v4();
    let run2 = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        environment_id,
        LeaseState::HeldParallel {
            holders: vec![run1, run2],
        },
        1,
    ));
    // Release doesn't touch environments_repo; no environment is registered.
    let environments_repo = Arc::new(MockEnvironmentsRepository::none());
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let state = svc
        .release(&ctx(tenant_id), environment_id, run1)
        .await
        .unwrap();

    assert_eq!(
        state,
        LeaseState::HeldParallel {
            holders: vec![run2]
        }
    );
    assert_eq!(
        leases_repo.stored(environment_id),
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
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        environment_id,
        LeaseState::HeldExclusive { holder: run_id },
        1,
    ));
    leases_repo.program_conflicts(1);
    let environments_repo = Arc::new(MockEnvironmentsRepository::none());
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let state = svc
        .release(&ctx(tenant_id), environment_id, run_id)
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
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    // run_id already exclusively holds the lease; re-acquiring in the same
    // mode must be a no-op (NIT fix): the decided state equals the current
    // state, so the CAS write is skipped entirely.
    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        environment_id,
        LeaseState::HeldExclusive { holder: run_id },
        1,
    ));
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let outcome = svc
        .acquire(
            &ctx(tenant_id),
            environment_id,
            run_id,
            LeaseMode::Exclusive,
        )
        .await
        .unwrap();

    assert_eq!(outcome, AcquireOutcome::Acquired);
    assert_eq!(
        leases_repo.cas_call_count(),
        0,
        "idempotent re-acquire must not write"
    );
    assert_eq!(
        leases_repo.stored(environment_id),
        Some((LeaseState::HeldExclusive { holder: run_id }, 1)),
        "version must not bump on a no-op re-acquire"
    );
}

#[tokio::test]
async fn acquire_unavailable_environment_rejected() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        false,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let err = svc
        .acquire(&ctx(tenant_id), environment_id, run_id, LeaseMode::Parallel)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::EnvironmentUnavailable { id } if id == environment_id),
        "expected EnvironmentUnavailable, got {err:?}"
    );
    assert_eq!(leases_repo.cas_call_count(), 0);
}

#[tokio::test]
async fn acquire_unknown_environment_rejected() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::new());
    let environments_repo = Arc::new(MockEnvironmentsRepository::none());
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let err = svc
        .acquire(&ctx(tenant_id), environment_id, run_id, LeaseMode::Parallel)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::EnvironmentNotFound { id } if id == environment_id),
        "expected EnvironmentNotFound, got {err:?}"
    );
    assert_eq!(leases_repo.cas_call_count(), 0);
}
