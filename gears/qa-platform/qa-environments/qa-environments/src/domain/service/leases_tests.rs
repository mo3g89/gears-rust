#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for the CAS (compare-and-swap) retry loop in `LeasesService`.
//!
//! These tests use a hand-rolled in-memory mock for `LeasesRepository` (it
//! ignores the `DBRunner` argument entirely — the trait requires one, but the
//! mock never touches a database) plus the shared `EnvironmentsRepository`/
//! `AuthZResolverApi` test doubles in [`super::test_support`]. The real
//! PEP flow (`PolicyEnforcer::access_scope`) runs unmodified; only the PDP
//! backend is faked.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use qa_environments_sdk::{AcquireOutcome, LeaseMode, LeaseState};
use time::OffsetDateTime;
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

/// One stored lease row, as the double models it.
#[derive(Clone, Debug)]
struct MockRow {
    state: LeaseState,
    version: i64,
    /// Mirrors `qa_environment_leases.freed_at`. The double stamps it under
    /// the same rule the real writer does — see `compare_and_set` below —
    /// because the rule is what the service's anchor behaviour rests on.
    freed_at: Option<OffsetDateTime>,
}

/// In-memory `LeasesRepository` double: `environment_id -> MockRow`.
///
/// `compare_and_set` can be programmed to fail with `LeaseConflict` a fixed
/// number of times before succeeding, to exercise the retry loop.
#[derive(Default)]
struct MockLeasesRepository {
    rows: Mutex<HashMap<Uuid, MockRow>>,
    fail_cas_times: Mutex<usize>,
    cas_calls: Mutex<usize>,
    /// What the next `Free` write stamps, so a test can assert on an exact
    /// instant instead of racing a clock.
    free_clock: Mutex<Option<OffsetDateTime>>,
}

impl MockLeasesRepository {
    fn new() -> Self {
        Self::default()
    }

    fn seeded(environment_id: Uuid, state: LeaseState, version: i64) -> Self {
        let repo = Self::new();
        repo.rows.lock().unwrap().insert(
            environment_id,
            MockRow {
                state,
                version,
                freed_at: None,
            },
        );
        repo
    }

    /// Seed a row that already carries a recorded free transition.
    fn seeded_freed_at(
        environment_id: Uuid,
        state: LeaseState,
        version: i64,
        freed_at: OffsetDateTime,
    ) -> Self {
        let repo = Self::new();
        repo.rows.lock().unwrap().insert(
            environment_id,
            MockRow {
                state,
                version,
                freed_at: Some(freed_at),
            },
        );
        repo
    }

    /// Pin what the next transition-to-free stamps.
    fn set_free_clock(&self, at: OffsetDateTime) {
        *self.free_clock.lock().unwrap() = Some(at);
    }

    fn stored_freed_at(&self, environment_id: Uuid) -> Option<OffsetDateTime> {
        self.rows
            .lock()
            .unwrap()
            .get(&environment_id)
            .and_then(|row| row.freed_at)
    }

    /// Program the next `n` `compare_and_set` calls to fail with `LeaseConflict`.
    fn program_conflicts(&self, n: usize) {
        *self.fail_cas_times.lock().unwrap() = n;
    }

    fn cas_call_count(&self) -> usize {
        *self.cas_calls.lock().unwrap()
    }

    fn stored(&self, environment_id: Uuid) -> Option<(LeaseState, i64)> {
        self.rows
            .lock()
            .unwrap()
            .get(&environment_id)
            .map(|row| (row.state.clone(), row.version))
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
            Some(row) => VersionedLease {
                state: row.state.clone(),
                version: row.version,
                freed_at: row.freed_at,
            },
            None => VersionedLease {
                state: LeaseState::Free,
                version: 0,
                freed_at: None,
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
        let current_version = rows.get(&environment_id).map_or(0, |row| row.version);
        if current_version != expected_version {
            return Err(DomainError::LeaseConflict);
        }
        // The real writer's rule, mirrored: a `Free` write is a transition to
        // free and stamps the anchor; every other write leaves it alone,
        // deliberately including an acquisition, so a held row keeps the
        // instant its current holder consumed.
        let previous = rows.get(&environment_id).and_then(|row| row.freed_at);
        let freed_at = if matches!(new_state, LeaseState::Free) {
            self.free_clock.lock().unwrap().or(previous)
        } else {
            previous
        };
        rows.insert(
            environment_id,
            MockRow {
                state: new_state.clone(),
                version: expected_version + 1,
                freed_at,
            },
        );
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

    assert!(matches!(outcome, AcquireOutcome::Acquired { .. }));
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

    assert!(matches!(outcome, AcquireOutcome::Acquired { .. }));
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

    assert!(matches!(outcome, AcquireOutcome::Acquired { .. }));
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

// ---------------------------------------------------------------------------
// The dispatch-latency anchor
//
// `cpt-cf-qa-nfr-dispatch-latency` is stated over *environment becomes free →
// queued run starts*. These pin the first of those two instants end to end
// through the service: which releases stamp it, which acquisitions receive it,
// and the two ways it must refuse to answer rather than answer wrongly.
// ---------------------------------------------------------------------------

/// A fixed instant, so an assertion names a value rather than a window.
fn freed_at_instant() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_789_689_600).expect("valid unix timestamp")
}

#[tokio::test]
async fn releasing_an_exclusive_hold_stamps_the_free_instant() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        environment_id,
        LeaseState::HeldExclusive { holder: run_id },
        1,
    ));
    leases_repo.set_free_clock(freed_at_instant());
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let state = svc
        .release(&ctx(tenant_id), environment_id, run_id)
        .await
        .unwrap();

    assert_eq!(state, LeaseState::Free);
    assert_eq!(
        leases_repo.stored_freed_at(environment_id),
        Some(freed_at_instant()),
        "a release that frees the environment must record when it became free",
    );
}

/// **The distinction the whole anchor exists for.** A parallel holder letting
/// go while another holder remains *calls* release and does **not** free the
/// environment. Stamping on the call rather than on the transition would
/// backdate the anchor to a moment the environment was still occupied, and the
/// next measurement would read the remaining holder's runtime as dispatch
/// latency.
#[tokio::test]
async fn releasing_one_of_two_parallel_holders_stamps_nothing() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let leaving = Uuid::new_v4();
    let staying = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        environment_id,
        LeaseState::HeldParallel {
            holders: vec![leaving, staying],
        },
        1,
    ));
    leases_repo.set_free_clock(freed_at_instant());
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let state = svc
        .release(&ctx(tenant_id), environment_id, leaving)
        .await
        .unwrap();

    assert_eq!(
        state,
        LeaseState::HeldParallel {
            holders: vec![staying]
        },
    );
    assert_eq!(
        leases_repo.stored_freed_at(environment_id),
        None,
        "the environment did not become free, so there is no free instant to record",
    );
}

/// The last parallel holder leaving *does* free it, and the same release path
/// therefore stamps. Together with the test above this is the pair that makes
/// "the transition, not the call" a property of the code.
#[tokio::test]
async fn releasing_the_last_parallel_holder_stamps_the_free_instant() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded(
        environment_id,
        LeaseState::HeldParallel {
            holders: vec![run_id],
        },
        1,
    ));
    leases_repo.set_free_clock(freed_at_instant());
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    svc.release(&ctx(tenant_id), environment_id, run_id)
        .await
        .unwrap();

    assert_eq!(
        leases_repo.stored_freed_at(environment_id),
        Some(freed_at_instant()),
    );
}

/// The read side: the acquisition that takes the environment out of `Free`
/// receives the stored instant. This is the hand-off the measurement is built
/// on — and it comes from the row, so a control-plane restart between the
/// release and this acquire changes nothing.
#[tokio::test]
async fn acquiring_a_free_environment_returns_the_stored_free_instant() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded_freed_at(
        environment_id,
        LeaseState::Free,
        4,
        freed_at_instant(),
    ));
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let outcome = svc
        .acquire(&ctx(tenant_id), environment_id, run_id, LeaseMode::Exclusive)
        .await
        .unwrap();

    assert_eq!(
        outcome,
        AcquireOutcome::Acquired {
            became_free_at: Some(freed_at_instant())
        },
    );
}

/// A parallel run joining an occupied environment gets **no** anchor, even
/// though the row still carries one from an earlier free. It was not admitted
/// by that transition, and reporting it would measure this run's latency from
/// an instant that admitted somebody else.
#[tokio::test]
async fn joining_a_parallel_hold_returns_no_free_instant() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let holder = Uuid::new_v4();
    let joiner = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded_freed_at(
        environment_id,
        LeaseState::HeldParallel {
            holders: vec![holder],
        },
        4,
        freed_at_instant(),
    ));
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    let outcome = svc
        .acquire(&ctx(tenant_id), environment_id, joiner, LeaseMode::Parallel)
        .await
        .unwrap();

    assert_eq!(
        outcome,
        AcquireOutcome::Acquired {
            became_free_at: None
        },
    );
}

/// An environment nobody has ever freed hands back no anchor, and specifically
/// not a clock read standing in for one. A fabricated instant is
/// indistinguishable from a real one once it is in a histogram.
#[tokio::test]
async fn acquiring_a_never_leased_environment_returns_no_free_instant() {
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
        .acquire(&ctx(tenant_id), environment_id, run_id, LeaseMode::Exclusive)
        .await
        .unwrap();

    assert_eq!(
        outcome,
        AcquireOutcome::Acquired {
            became_free_at: None
        },
    );
}

/// The anchor **survives the acquisition that consumes it**: the row still
/// carries it while held, so a second reader — a restarted control plane
/// reconciling, say — sees the same instant rather than a cleared column.
#[tokio::test]
async fn acquiring_does_not_clear_the_stored_free_instant() {
    let db = test_db_provider().await;
    let environment_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let leases_repo = Arc::new(MockLeasesRepository::seeded_freed_at(
        environment_id,
        LeaseState::Free,
        4,
        freed_at_instant(),
    ));
    let environments_repo = Arc::new(MockEnvironmentsRepository::with_environment(environment(
        environment_id,
        true,
    )));
    let svc = build_service(Arc::clone(&leases_repo), environments_repo, db);

    svc.acquire(&ctx(tenant_id), environment_id, run_id, LeaseMode::Exclusive)
        .await
        .unwrap();

    assert_eq!(
        leases_repo.stored_freed_at(environment_id),
        Some(freed_at_instant()),
        "an acquisition must not clear the anchor it consumed",
    );
}
