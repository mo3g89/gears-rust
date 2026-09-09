//! Leader election for this gear's background tickers.
//!
//! Three roles use it — [`ROLE_RECONCILER`], [`ROLE_JIRA_POLLER`] and
//! [`ROLE_COLLECT`]. **Task 40 started the tickers**, so all three are live;
//! Task 15 created this module ahead of them because the reconciler's contract
//! is "only one instance sweeps", and a contract with no place to be enforced is
//! a comment.
//!
//! They are three roles rather than one because a deployment switches the three
//! on and off independently — `reconcile_interval_seconds`,
//! `jira_poller_interval_seconds` and `collect_interval_seconds`, where `0`
//! disables. `qa-runs`' `SCHEDULER_ROLE` states the consequence of sharing one:
//! "run this ticker here but not that one" becomes unexpressible to any real
//! elector, and a leadership term for one would silently gate the others.
//!
//! # This is the fourth copy of this trait in the tree
//!
//! `qa-runs/src/infra/leader/mod.rs`, `chat-engine/src/infra/leader/mod.rs` and
//! `mini-chat/src/infra/leader/mod.rs` each hold their own. It belongs in
//! `libs/`, nothing there exports it, and lifting it is a cross-gear change no
//! task in this plan owns. All four are named here so whoever lifts it can find
//! them.
//!
//! `cluster-sdk` is the intended long-term home — `cluster_sdk::LeaderElectionV1`
//! is the real facade — but it still has no working consumer (event-broker's
//! `resolve()` is a `todo!()`, and no `ClusterProfile` is bound anywhere), and
//! its `is_leader()` is documented *"Advisory - do NOT use for
//! correctness-critical mutual exclusion"*.
//!
//! **Task 40 declared the dependency anyway**, reversing this paragraph's
//! earlier reasoning rather than quietly leaving it. It said a
//! declared-and-unused crate with a `cargo-shear` ignore was "a cost worth
//! paying when there is a consumer, and there is not one until Task 40" — Task
//! 40 is now here, [`elector`] has three callers, and the marker is what makes
//! the intended replacement discoverable from `Cargo.toml` rather than only from
//! this comment. The crate is named by no line of code, exactly as it is in
//! qa-runs, and [`NoopLeaderElector`] is still the only implementation shipped:
//! declaring it does **not** make this gear cluster-aware and nothing here
//! should be read as saying so.
//!
//! # What election buys the reconciler, and what it does not
//!
//! **It is an optimisation, not a correctness requirement, and saying otherwise
//! would be the false-guarantee defect this subsystem counts as a bug.**
//!
//! Two replicas sweeping the same tenant concurrently produce a correct
//! projection: the backfill is `ResultsRepository::upsert_run_results`, which is
//! delete-then-insert per run, and `WatermarkRepository::advance` never moves a
//! mark backwards. The worst outcome is duplicated work and, on a genuine
//! interleave, one run projected twice — the second write replacing the first
//! with identical rows.
//!
//! What election saves is that duplicated work: N replicas each making N times
//! the cross-gear reads of a sweep whose whole purpose is to be cheap. It is
//! also what keeps the `stopped_at_gap` signal meaningful, since two overlapping
//! sweeps can each report a different stopping point for the same tenant.
//!
//! The **one** place it would be a correctness requirement is a rebuild that
//! truncated before rewriting; Task 16's rebuild endpoint does not, for exactly
//! this reason. It replays run by run through `ReconcileService::reproject`,
//! which reads via `IngestService::read_run_projection` and writes via
//! `IngestService::write_run_projection` — split out of one function into these
//! two in Task 5a's fix wave (see `ReconcileService::reproject`'s own doc,
//! "The race the split introduces, and why it does not matter here", for the
//! race that split introduces and why this section's argument still holds
//! across it: the property this section leans on is the write's idempotence,
//! and the write did not change). The two tests that hold the
//! no-truncation property to that are
//! `reconcile_tests::a_rebuild_leaves_rows_outside_its_window_untouched`
//! and `..::a_rebuild_over_an_empty_window_deletes_nothing` — the second is the
//! one a range-truncating implementation could not pass.
//!
//! # The JIRA poller is the exception, and misreading that is review finding #5
//!
//! **Everything above is about the reconciler and holds for
//! [`ROLE_COLLECT`] too. It does not extend to [`ROLE_JIRA_POLLER`], and this
//! paragraph exists because it was read as though it did.**
//!
//! The argument above turns entirely on the *shape of the write*: two sweeps
//! converge because `upsert_run_results` is delete-then-insert per run and
//! `WatermarkRepository::advance` never moves a mark backwards. The poller's
//! effect is not a write of that shape. It is
//! [`RunsLauncher::launch_test`](crate::domain::ports::RunsLauncher::launch_test) —
//! a **new run**, through qa-runs' normal admission path — and nothing
//! downstream of `JiraPollerService::maybe_rerun` deduplicates one. Two
//! replicas polling the same resolved bug launch it twice, costing a platform
//! slot and a second set of results for a build that has already been tested.
//! `crate::gear`'s `jira_poller_ticker` has said so at its own doc since Task
//! 40; what it named as the missing fix — "a claim row of the kind
//! `idx_qa_run_notifications_claim` gives the notification path" — is now
//! [`ClaimRowElector`], and `qa_leader_claims` is that row.
//!
//! What had been holding the property up until then was `replicaCount: 1` in
//! `deploy/helm/qa-platform/values.yaml`, which is not where a correctness
//! property belongs: a future scale-out would break it silently, in a chart,
//! with nothing in this crate going red. Pinning the replica count with a
//! chart assertion was considered and rejected for that reason.
//!
//! So the three roles are deliberately not symmetric. [`ROLE_JIRA_POLLER`]
//! runs under [`ClaimRowElector`]; [`ROLE_RECONCILER`] and [`ROLE_COLLECT`]
//! keep [`NoopLeaderElector`], because for them the sections above are still
//! the whole truth and a claim row would buy a round trip per tick to prevent
//! nothing. Review finding #5.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

pub mod claim_row;

pub use claim_row::ClaimRowElector;

/// Boxed async work that receives a [`CancellationToken`].
///
/// The token fires when leadership is lost or the gear shuts down. The function
/// may be invoked more than once — after a re-election — which is why it is
/// `Fn` and not `FnOnce`.
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
/// **Every deployment of qa-insights behaves as though it were the leader for
/// the role this serves**, which for [`ROLE_RECONCILER`] and [`ROLE_COLLECT`]
/// is a safe default rather than a compromise — see the module header on why
/// election is an optimisation for those two.
///
/// It was the *only* implementation this gear shipped until review finding #5,
/// and that sentence used to stand here without qualification. It no longer
/// does: [`ClaimRowElector`] serves [`ROLE_JIRA_POLLER`], whose effect is a
/// launch rather than a converging write. The module header's "The JIRA poller
/// is the exception" carries the difference.
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

/// The role name the reconcile ticker holds. Task 40 passes it to
/// [`LeaderElector::run_role`].
///
/// A constant rather than a literal at the call site because a real elector
/// keys its lease on this string: a typo would silently give the ticker its own
/// uncontended role, which looks exactly like working.
pub const ROLE_RECONCILER: &str = "qa-insights-reconciler";

/// The role name the JIRA poller ticker holds. Task 40 passes it to
/// [`LeaderElector::run_role`].
///
/// Added now, beside [`ROLE_RECONCILER`], for that constant's own precedent:
/// it was added at Task 15, alongside the reconciler, well before Task 40
/// wires any ticker at all — the role name belongs with the service whose
/// role it names, not with whichever task first starts the loop.
/// `domain::service::jira_poller`'s module header names this constant as the
/// role Task 35's poller will run under.
pub const ROLE_JIRA_POLLER: &str = "qa-insights-jira-poller";

/// The role name the collect ticker holds. Task 40 passes it to
/// [`LeaderElector::run_role`].
///
/// The third and last, added by Task 40 — the task that actually starts all
/// three loops — rather than by the task that built
/// [`CollectService::run_collect_cycle`](crate::domain::service::collect::CollectService::run_collect_cycle).
/// That is a deviation from [`ROLE_JIRA_POLLER`]'s stated precedent ("the role
/// name belongs with the service whose role it names") and it is recorded rather
/// than hidden: Task 30 shipped the collect cycle as a *request*-driven
/// operation first — `POST /qa/v1/analytics/collect` is its endpoint — so at
/// that point there was no role, only a handler. The constant lands with the
/// ticker that gave it one.
///
/// **Legacy has no leadership concept for this poller at all**
/// (`manager/src/services/collect.rs:183`, `start_collect_poller`, one process
/// spawning one loop), so the role is this gear's own answer to running the same
/// hourly cycle on N replicas, not a ported name.
pub const ROLE_COLLECT: &str = "qa-insights-collect";

/// The elector [`ROLE_RECONCILER`] and [`ROLE_COLLECT`] use.
///
/// One function rather than a `Default` impl, so the choice has a place to grow
/// a feature gate without every call site changing.
///
/// **Not [`ROLE_JIRA_POLLER`]'s** — that role gets
/// [`jira_poller_elector`], and the module header's "The JIRA poller is the
/// exception" says why the three roles are deliberately not symmetric.
#[must_use]
pub fn elector() -> Arc<dyn LeaderElector> {
    Arc::new(NoopLeaderElector)
}

/// The elector [`ROLE_JIRA_POLLER`] uses.
///
/// A second function rather than a parameter on [`elector`], so that a call
/// site cannot pick the wrong one by passing the wrong role: the two are named
/// for what they serve. It takes the database because a claim row lives in one
/// — that is the whole difference between the two.
#[must_use]
pub fn jira_poller_elector(db: Arc<crate::domain::service::DbProvider>) -> Arc<dyn LeaderElector> {
    Arc::new(ClaimRowElector::new(db))
}

#[cfg(test)]
mod tests {
    use super::{LeaderElector, NoopLeaderElector, ROLE_RECONCILER, elector, work_fn};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio_util::sync::CancellationToken;

    /// The property the dev stack depends on: the no-op elector runs the work
    /// **unchanged**, so wiring a ticker through `run_role` does not make it
    /// conditional on anything.
    #[tokio::test]
    async fn the_noop_elector_runs_the_work() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ran);
        NoopLeaderElector
            .run_role(
                ROLE_RECONCILER,
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
    /// shut down cooperatively rather than being aborted mid-sweep.
    #[tokio::test]
    async fn the_cancellation_token_is_handed_to_the_work() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let observed = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&observed);

        elector()
            .run_role(
                ROLE_RECONCILER,
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

    /// An error from the work reaches the caller, so `crate::gear`'s `serve` can
    /// treat a premature exit as an error rather than as silent idling — which,
    /// since Task 40, it does: each ticker logs the error and returns, and
    /// `supervise` turns that return into `serve`'s `Err`.
    #[tokio::test]
    async fn an_error_from_the_work_is_propagated() {
        let result = NoopLeaderElector
            .run_role(
                ROLE_RECONCILER,
                CancellationToken::new(),
                work_fn(|_cancel| async { Err(anyhow::anyhow!("boom")) }),
            )
            .await;
        assert!(result.is_err());
    }
}
