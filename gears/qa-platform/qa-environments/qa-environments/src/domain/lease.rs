//! Pure lease decision logic.
//!
//! Semantics (PRD `cpt-cf-qa-fr-runs-queue`, `cpt-cf-qa-fr-env-lease`):
//! - free + parallel      → acquired (parallel hold, 1 holder)
//! - free + exclusive     → acquired (exclusive hold)
//! - parallel + parallel  → acquired (holder appended)
//! - parallel + exclusive → busy
//! - exclusive + anything → busy (except same-run re-acquire)
//! - release removes the run; last holder out → free
//! - release is idempotent (unknown run → state unchanged)

use qa_environments_sdk::{AcquireOutcome, LeaseMode, LeaseState};
use uuid::Uuid;

/// Decide an acquisition attempt. Returns the outcome and the resulting state
/// (unchanged state when `Busy`). Pure — persistence is the caller's job.
#[must_use]
pub fn decide_acquire(
    current: &LeaseState,
    run_id: Uuid,
    mode: LeaseMode,
) -> (AcquireOutcome, LeaseState) {
    match (current, mode) {
        (LeaseState::Free, LeaseMode::Parallel) => (
            AcquireOutcome::Acquired,
            LeaseState::HeldParallel {
                holders: vec![run_id],
            },
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
            (
                AcquireOutcome::Acquired,
                LeaseState::HeldParallel { holders },
            )
        }
        (LeaseState::HeldExclusive { holder }, LeaseMode::Exclusive) if *holder == run_id => (
            AcquireOutcome::Acquired,
            LeaseState::HeldExclusive { holder: run_id },
        ),
        (state, _) => (
            AcquireOutcome::Busy {
                current: state.clone(),
            },
            state.clone(),
        ),
    }
}

/// Decide a release. Idempotent: unknown run leaves the state unchanged.
#[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn free_environment_grants_parallel() {
        let (outcome, state) = decide_acquire(&LeaseState::Free, run(1), LeaseMode::Parallel);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(
            state,
            LeaseState::HeldParallel {
                holders: vec![run(1)]
            }
        );
    }

    #[test]
    fn free_environment_grants_exclusive() {
        let (outcome, state) = decide_acquire(&LeaseState::Free, run(1), LeaseMode::Exclusive);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(state, LeaseState::HeldExclusive { holder: run(1) });
    }

    #[test]
    fn parallel_hold_admits_another_parallel() {
        let current = LeaseState::HeldParallel {
            holders: vec![run(1)],
        };
        let (outcome, state) = decide_acquire(&current, run(2), LeaseMode::Parallel);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(
            state,
            LeaseState::HeldParallel {
                holders: vec![run(1), run(2)]
            }
        );
    }

    #[test]
    fn parallel_hold_rejects_exclusive() {
        let current = LeaseState::HeldParallel {
            holders: vec![run(1)],
        };
        let (outcome, state) = decide_acquire(&current, run(2), LeaseMode::Exclusive);
        assert_eq!(
            outcome,
            AcquireOutcome::Busy {
                current: current.clone()
            }
        );
        assert_eq!(state, current, "state must not change on Busy");
    }

    #[test]
    fn exclusive_hold_rejects_parallel_and_exclusive() {
        let current = LeaseState::HeldExclusive { holder: run(1) };
        for mode in [LeaseMode::Parallel, LeaseMode::Exclusive] {
            let (outcome, state) = decide_acquire(&current, run(2), mode);
            assert_eq!(
                outcome,
                AcquireOutcome::Busy {
                    current: current.clone()
                }
            );
            assert_eq!(state, current);
        }
    }

    #[test]
    fn acquire_is_idempotent_for_same_run() {
        // Re-acquiring by the same run (dispatcher retry after crash) must not duplicate holders.
        let current = LeaseState::HeldParallel {
            holders: vec![run(1)],
        };
        let (outcome, state) = decide_acquire(&current, run(1), LeaseMode::Parallel);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(
            state,
            LeaseState::HeldParallel {
                holders: vec![run(1)]
            }
        );

        let current = LeaseState::HeldExclusive { holder: run(1) };
        let (outcome, state) = decide_acquire(&current, run(1), LeaseMode::Exclusive);
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert_eq!(state, LeaseState::HeldExclusive { holder: run(1) });
    }

    #[test]
    fn release_last_parallel_holder_frees() {
        let current = LeaseState::HeldParallel {
            holders: vec![run(1)],
        };
        assert_eq!(decide_release(&current, run(1)), LeaseState::Free);
    }

    #[test]
    fn release_one_of_many_keeps_parallel() {
        let current = LeaseState::HeldParallel {
            holders: vec![run(1), run(2)],
        };
        assert_eq!(
            decide_release(&current, run(1)),
            LeaseState::HeldParallel {
                holders: vec![run(2)]
            }
        );
    }

    #[test]
    fn release_exclusive_frees() {
        let current = LeaseState::HeldExclusive { holder: run(1) };
        assert_eq!(decide_release(&current, run(1)), LeaseState::Free);
    }

    #[test]
    fn release_unknown_run_is_noop() {
        let current = LeaseState::HeldParallel {
            holders: vec![run(1)],
        };
        assert_eq!(decide_release(&current, run(9)), current);
        assert_eq!(decide_release(&LeaseState::Free, run(9)), LeaseState::Free);
        let excl = LeaseState::HeldExclusive { holder: run(1) };
        assert_eq!(decide_release(&excl, run(9)), excl);
    }
}
