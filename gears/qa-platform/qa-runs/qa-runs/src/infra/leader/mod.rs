//! Leader election for this gear's background tickers.
//!
//! Two roles use it — `qa-runs-dispatcher` and `qa-runs-scheduler`, both named
//! in `crate::gear` — and they are separate roles rather than one, because a
//! deployment switches the two on and off independently.
//!
//! # This is the third copy of this trait in the tree
//!
//! `chat-engine/src/infra/leader/mod.rs` and `mini-chat/src/infra/leader/mod.rs`
//! each hold their own. It belongs in `libs/`, nothing there exports it, and
//! lifting it is a cross-gear change this task does not own. The two existing
//! copies are named here so whoever lifts it can find all three.
//!
//! **Path corrected 2026-08-15.** This said the second copy was
//! `mini-chat/src/infra/workers/orphan_watchdog.rs`. That file is a *caller* -
//! it does `use crate::infra::leader::{..}` - not a copy, and the error was in
//! the one sentence whose whole purpose is to let a future lifter find every
//! copy.
//!
//! `cluster-sdk` is a declared dependency of this crate and is deliberately
//! unused. It is the intended long-term home - `cluster_sdk::LeaderElectionV1`
//! is the real facade - but it has no working consumer yet (event-broker's
//! `resolve()` is a `todo!()`, and no `ClusterProfile` is bound anywhere), and
//! its `is_leader()` is documented *"Advisory - do NOT use for
//! correctness-critical mutual exclusion"*
//! (`cluster-sdk/src/leader/watch.rs`, the paragraph under that heading).
//!
//! # What election buys here, and what it does not
//!
//! **It does not make the dispatcher safe on two replicas.** Saying so would
//! be the false-guarantee defect this subsystem counts as a bug, so it is said
//! the other way round:
//!
//! * Admission is **N-writer**. Every REST launch can commit a claim inline,
//!   on any replica, and no election gates that path. Closing it needs a
//!   database advisory lock keyed on `platform_id`; nothing here is that lock.
//! * What election covers is the **tickers**: one replica running `run_tick`,
//!   so claim reconciliation, the TTL sweep and the timeout sweep are not done
//!   N times over, and the tick-side cap evaluation has a single evaluator
//!   instead of N racing ones.
//! * `recover_after_boot` is the one pass where election is not an
//!   optimisation but a correctness requirement - see the note on it in
//!   `crate::gear`.
//! * **For the scheduler it is an optimisation and nothing more**, and that is
//!   the one role whose correctness does not depend on this module at all:
//!   `domain::service::schedules` is exactly-once by way of a unique index on
//!   the tick claim, so every replica may evaluate every schedule and still
//!   produce one run. Election saves the fleet-wide enumeration being run N
//!   times; it is not what stops a schedule firing twice. `qa-runs-scheduler`
//!   is a separate role from the dispatcher's, so a real elector can place the
//!   two independently.
//!
//! One interaction worth stating because it is easy to miss: the dispatcher's
//! claim-scan cursor (`DispatchService::claim_scan_cursor`) is per-process and
//! unpersisted. Under [`NoopLeaderElector`] every replica keeps its own, so N
//! replicas sweep N independent rotations - which is not incorrect, only
//! duplicated. Under a real elector, leadership changing hands resets the
//! cursor, costing at most one extra cycle of coverage.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Boxed async work that receives a [`CancellationToken`].
///
/// The token fires when leadership is lost or the gear shuts down. The
/// function may be invoked more than once - after a re-election - which is why
/// it is `Fn` and not `FnOnce`.
pub type LeaderWorkFn = Box<
    dyn Fn(CancellationToken) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
        + Send
        + Sync,
>;

/// Wrap a closure as a [`LeaderWorkFn`].
pub fn work_fn<F, Fut>(f: F) -> LeaderWorkFn
where
    F: Fn(CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    Box::new(move |cancel| Box::pin(f(cancel)))
}

/// Run periodic work only while this process holds a named role.
#[async_trait]
pub trait LeaderElector: Send + Sync + std::fmt::Debug {
    /// Run `work` while this instance holds leadership for `role`.
    ///
    /// Returns when `cancel` fires (gear shutdown) or on an unrecoverable
    /// error.
    ///
    /// # Errors
    ///
    /// Returns an error when leadership cannot be acquired because of an
    /// infrastructure failure, and whatever `work` returns.
    async fn run_role(
        &self,
        role: &str,
        cancel: CancellationToken,
        work: LeaderWorkFn,
    ) -> anyhow::Result<()>;
}

/// Single-process elector: runs the work immediately, with no coordination.
///
/// This is the only implementation this gear ships, so **every deployment of
/// qa-runs today behaves as though it were the leader**. That is correct for
/// the single-replica deployment the source system also assumes (the frozen
/// guide's admission-correctness note says its own design "assumes a single
/// manager replica"), and it is the reason the trait exists at all: the seam is
/// here so a real elector can be dropped in without touching `serve`.
#[derive(Debug)]
pub struct NoopLeaderElector;

#[async_trait]
impl LeaderElector for NoopLeaderElector {
    async fn run_role(
        &self,
        _role: &str,
        cancel: CancellationToken,
        work: LeaderWorkFn,
    ) -> anyhow::Result<()> {
        work(cancel).await
    }
}

/// The elector this deployment uses.
///
/// One function rather than a `Default` impl, so the choice has a place to
/// grow a feature gate the way chat-engine's `create_leader_elector` does
/// without every call site changing.
#[must_use]
pub fn elector() -> Arc<dyn LeaderElector> {
    Arc::new(NoopLeaderElector)
}

#[cfg(test)]
mod tests {
    use super::{LeaderElector, NoopLeaderElector, elector, work_fn};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio_util::sync::CancellationToken;

    /// The property the dev stack depends on: the no-op elector runs the work
    /// **unchanged**, so wiring the ticker through `run_role` does not make it
    /// conditional on anything.
    #[tokio::test]
    async fn the_noop_elector_runs_the_work() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ran);
        NoopLeaderElector
            .run_role(
                "dispatcher",
                CancellationToken::new(),
                work_fn(move |_cancel| {
                    let counter = Arc::clone(&counter);
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }),
            )
            .await
            .expect("the no-op elector must not fail");
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    /// The cancellation token reaches the work, which is what lets a ticker
    /// shut down cooperatively rather than being aborted.
    #[tokio::test]
    async fn the_cancellation_token_is_handed_to_the_work() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let observed = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&observed);

        elector()
            .run_role(
                "dispatcher",
                cancel,
                work_fn(move |cancel| {
                    let counter = Arc::clone(&counter);
                    async move {
                        if cancel.is_cancelled() {
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                        Ok(())
                    }
                }),
            )
            .await
            .expect("the no-op elector must not fail");
        assert_eq!(observed.load(Ordering::SeqCst), 1);
    }

    /// An error from the work reaches the caller, so `serve` can treat a
    /// premature exit as an error rather than as silent idling.
    #[tokio::test]
    async fn an_error_from_the_work_is_propagated() {
        let result = NoopLeaderElector
            .run_role(
                "dispatcher",
                CancellationToken::new(),
                work_fn(|_cancel| async { Err(anyhow::anyhow!("boom")) }),
            )
            .await;
        assert!(result.is_err());
    }
}
