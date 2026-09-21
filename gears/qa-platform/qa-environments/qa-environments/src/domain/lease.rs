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
use time::OffsetDateTime;
use uuid::Uuid;

/// Decide an acquisition attempt. Returns the outcome and the resulting state
/// (unchanged state when `Busy`). Pure — persistence is the caller's job.
///
/// # `freed_at`, and why the *rule* about it lives here
///
/// `freed_at` is the stored instant this environment last transitioned to
/// [`LeaseState::Free`] (`qa_environment_leases.freed_at`), or `None` when no
/// such transition was ever recorded. It is passed **in** rather than read
/// from a clock because whether it may be reported at all is a decision about
/// the state transition, not about persistence: it is the NFR's anchor only
/// when *this* acquisition is the one that takes the environment out of
/// `Free`. Every other arm reports `None` — a parallel join did not consume a
/// free transition, and neither did an idempotent re-acquire. Keeping that
/// rule in the pure function is what makes it testable without a database, and
/// what keeps a future arm from silently inheriting the wrong answer.
#[must_use]
pub fn decide_acquire(
    current: &LeaseState,
    freed_at: Option<OffsetDateTime>,
    run_id: Uuid,
    mode: LeaseMode,
) -> (AcquireOutcome, LeaseState) {
    match (current, mode) {
        (LeaseState::Free, LeaseMode::Parallel) => (
            AcquireOutcome::Acquired {
                became_free_at: freed_at,
            },
            LeaseState::HeldParallel {
                holders: vec![run_id],
            },
        ),
        (LeaseState::Free, LeaseMode::Exclusive) => (
            AcquireOutcome::Acquired {
                became_free_at: freed_at,
            },
            LeaseState::HeldExclusive { holder: run_id },
        ),
        (LeaseState::HeldParallel { holders }, LeaseMode::Parallel) => {
            let mut holders = holders.clone();
            if !holders.contains(&run_id) {
                holders.push(run_id);
            }
            (
                AcquireOutcome::Acquired {
                    became_free_at: None,
                },
                LeaseState::HeldParallel { holders },
            )
        }
        (LeaseState::HeldExclusive { holder }, LeaseMode::Exclusive) if *holder == run_id => (
            AcquireOutcome::Acquired {
                became_free_at: None,
            },
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

    /// A stored free instant, distinct from any clock this test could read.
    fn freed() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_789_689_600).expect("valid unix timestamp")
    }

    /// Every lease state, for the exhaustive invariants below.
    fn every_state() -> Vec<LeaseState> {
        vec![
            LeaseState::Free,
            LeaseState::HeldParallel {
                holders: vec![run(1)],
            },
            LeaseState::HeldParallel {
                holders: vec![run(1), run(2)],
            },
            LeaseState::HeldExclusive { holder: run(1) },
        ]
    }

    #[test]
    fn free_environment_grants_parallel() {
        let (outcome, state) = decide_acquire(
            &LeaseState::Free,
            Some(freed()),
            run(1),
            LeaseMode::Parallel,
        );
        assert_eq!(
            outcome,
            AcquireOutcome::Acquired {
                became_free_at: Some(freed())
            }
        );
        assert_eq!(
            state,
            LeaseState::HeldParallel {
                holders: vec![run(1)]
            }
        );
    }

    #[test]
    fn free_environment_grants_exclusive() {
        let (outcome, state) = decide_acquire(
            &LeaseState::Free,
            Some(freed()),
            run(1),
            LeaseMode::Exclusive,
        );
        assert_eq!(
            outcome,
            AcquireOutcome::Acquired {
                became_free_at: Some(freed())
            }
        );
        assert_eq!(state, LeaseState::HeldExclusive { holder: run(1) });
    }

    #[test]
    fn parallel_hold_admits_another_parallel() {
        let current = LeaseState::HeldParallel {
            holders: vec![run(1)],
        };
        let (outcome, state) = decide_acquire(&current, Some(freed()), run(2), LeaseMode::Parallel);
        assert_eq!(
            outcome,
            AcquireOutcome::Acquired {
                became_free_at: None
            },
            "joining an existing parallel hold consumed no free transition, so the \
             stored instant must not be reported as this run's anchor",
        );
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
        let (outcome, state) = decide_acquire(&current, Some(freed()), run(2), LeaseMode::Exclusive);
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
            let (outcome, state) = decide_acquire(&current, Some(freed()), run(2), mode);
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
        let (outcome, state) = decide_acquire(&current, Some(freed()), run(1), LeaseMode::Parallel);
        assert_eq!(
            outcome,
            AcquireOutcome::Acquired {
                became_free_at: None
            },
            "a re-acquire transitioned nothing, so it has no free instant to report",
        );
        assert_eq!(
            state,
            LeaseState::HeldParallel {
                holders: vec![run(1)]
            }
        );

        let current = LeaseState::HeldExclusive { holder: run(1) };
        let (outcome, state) =
            decide_acquire(&current, Some(freed()), run(1), LeaseMode::Exclusive);
        assert_eq!(
            outcome,
            AcquireOutcome::Acquired {
                became_free_at: None
            }
        );
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

    /// **The invariant `OrmLeasesRepository::compare_and_set` stamps `freed_at`
    /// on.** That writer sets the column whenever the new state is `Free`, and
    /// it is allowed to do so *unconditionally* only because no acquisition can
    /// produce `Free` — so a `Free` reaching the CAS is always a release, and
    /// always a real transition (the service skips the write when the state is
    /// unchanged).
    ///
    /// If a future arm ever returns `Free` from an acquire, this fails here
    /// rather than silently backdating an environment's anchor to the moment a
    /// run took it.
    #[test]
    fn no_acquisition_can_produce_a_free_state() {
        for state in every_state() {
            for mode in [LeaseMode::Parallel, LeaseMode::Exclusive] {
                for holder in [run(1), run(2), run(9)] {
                    let (_, next) = decide_acquire(&state, Some(freed()), holder, mode);
                    assert_ne!(
                        next,
                        LeaseState::Free,
                        "acquiring must never yield Free ({state:?}, {mode:?}, {holder:?}) \
                         -- `compare_and_set` treats a Free write as a release and stamps \
                         `freed_at` from it",
                    );
                }
            }
        }
    }

    /// **The anchor is reported by exactly the arm that consumes it.** Stated
    /// exhaustively rather than per arm, because the defect this guards is a
    /// new arm inheriting `freed_at` from a neighbour: a parallel join or a
    /// re-acquire reporting the previous holder's free instant would measure
    /// the NFR from an instant that admitted a *different* run.
    #[test]
    fn only_an_acquisition_out_of_free_reports_the_free_instant() {
        for state in every_state() {
            for mode in [LeaseMode::Parallel, LeaseMode::Exclusive] {
                for holder in [run(1), run(2), run(9)] {
                    let (outcome, _) = decide_acquire(&state, Some(freed()), holder, mode);
                    if let AcquireOutcome::Acquired { became_free_at } = outcome {
                        assert_eq!(
                            became_free_at.is_some(),
                            state == LeaseState::Free,
                            "only an acquisition that took the environment out of Free may \
                             report the free instant ({state:?}, {mode:?}, {holder:?})",
                        );
                    }
                }
            }
        }
    }

    /// An environment with no recorded transition to free reports none, even
    /// on the arm that would otherwise carry it. `None` in means `None` out —
    /// nothing here invents an instant.
    #[test]
    fn an_unrecorded_free_transition_stays_unrecorded() {
        let (outcome, _) = decide_acquire(&LeaseState::Free, None, run(1), LeaseMode::Exclusive);
        assert_eq!(
            outcome,
            AcquireOutcome::Acquired {
                became_free_at: None
            }
        );
    }

    /// The release side of the anchor: the two transitions that actually free
    /// an environment, and the one that does not. `compare_and_set` stamps
    /// `freed_at` from the first two and writes nothing at all for the third,
    /// because the service skips a write whose state is unchanged.
    #[test]
    fn only_a_transition_to_free_is_a_free_transition() {
        assert_eq!(
            decide_release(&LeaseState::HeldExclusive { holder: run(1) }, run(1)),
            LeaseState::Free,
            "an exclusive holder leaving frees the environment",
        );
        assert_eq!(
            decide_release(
                &LeaseState::HeldParallel {
                    holders: vec![run(1)]
                },
                run(1)
            ),
            LeaseState::Free,
            "the last parallel holder leaving frees the environment",
        );
        let still_held = LeaseState::HeldParallel {
            holders: vec![run(1), run(2)],
        };
        assert_ne!(
            decide_release(&still_held, run(1)),
            LeaseState::Free,
            "a parallel holder leaving while others remain does NOT free it, so no \
             anchor may be stamped -- this is the case that makes `release` and \
             `becomes free` different instants",
        );
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
