//! Tests for the reconcile sweep and for Task 16's operator rebuild.
//!
//! These run against the **real** repositories on in-memory `SQLite` and the
//! real transaction provider — only qa-runs and the PDP are doubles. The
//! properties under test are all about what lands in the database and where the
//! watermark ends up, so a repository double would have been testing the test.
//!
//! The `AuthZ` double is [`TenantScopedAuthZ`], which returns a real
//! `owner_tenant_id IN [tenant]` constraint, so the scope the rebuild compiles
//! from the PDP is applied against the real database rather than ignored — see
//! `a_rebuild_for_one_tenant_writes_nothing_visible_to_another`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use qa_insights_sdk::NotificationConfig;
use qa_runs_sdk::ScheduleNotificationSettings;
use time::{Duration, OffsetDateTime};
use toolkit_db::DBProvider;
use toolkit_gts::GTS_ID_PREFIX;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{ReconcileOutcome, ReconcileService};
use crate::domain::error::DomainError;
use crate::domain::ports::{
    MAX_FINISHED_RUNS_PAGE, MailClient, MailMessage, RunsReader, SendOutcome, SlackClient,
    SlackMessage,
};
use crate::domain::repos::{
    NotifyRepository, ResultsRepository, SweepCursor, WatermarkKind, WatermarkRepository,
};
use crate::domain::service::ingest::IngestService;
use crate::domain::service::notify::{NotifyService, RunCompletionNotifier};
use crate::domain::service::test_support::{
    DenyAllAuthZ, FakeRuns, RecordingAuthZ, ResourceConstrainedAuthZ, TenantScopedAuthZ,
    finished_run, test_row,
};
use crate::domain::system_actor::TenantBound;
use crate::gear::{Stall, TenantProgress, stall_of};
use crate::infra::storage::notify_sea_repo::OrmNotifyRepository;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{
    apply_pending_migrations, inmem_db, inmem_db_before_the_notification_backfill, scope,
};
use crate::infra::storage::watermark_sea_repo::OrmWatermarkRepository;

const TENANT: Uuid = Uuid::from_u128(0x0A);

/// A `reconcile_page_size` qa-runs will not serve — see
/// [`MAX_FINISHED_RUNS_PAGE`]. 800 rather than `MAX_FINISHED_RUNS_PAGE + 1`, so
/// the fixture reads as a number an operator would plausibly type while tuning
/// for throughput.
const ABOVE_THE_CAP: u32 = 800;

fn tenant() -> TenantBound {
    TenantBound::new(TENANT).expect("non-nil")
}

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        .expect("an RFC3339 fixture instant")
}

/// A [`RunCompletionNotifier`] double that records the run id of every call,
/// in order.
///
/// The count is what the producer tests assert on, rather than a log line:
/// "the sweep emitted something notification-shaped" and "the sweep asked the
/// notification service to notify this run" are different claims, and only the
/// second one is the feature.
///
/// Answers `Ok(())` always. The real method never propagates a *send* failure
/// either — it audits and swallows — so a double that failed would be
/// modelling a shape production does not have.
#[derive(Default)]
struct CountingNotifier {
    notified: Mutex<Vec<Uuid>>,
}

impl CountingNotifier {
    fn notified(&self) -> Vec<Uuid> {
        self.notified.lock().unwrap().clone()
    }
}

#[async_trait]
impl RunCompletionNotifier for CountingNotifier {
    async fn notify_run_completed(
        &self,
        _ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<(), DomainError> {
        self.notified.lock().unwrap().push(run_id);
        Ok(())
    }
}

struct Fixture {
    db: toolkit_db::Db,
    provider: Arc<DBProvider<DomainError>>,
    runs: Arc<FakeRuns>,
    /// The operator's own context, which only [`ReconcileService::rebuild`]
    /// takes. The sweep has no caller and derives its own system actor, so no
    /// sweep test touches this field.
    ctx: SecurityContext,
    /// What [`ReconcileService::reproject`] asked to notify, per run.
    notifier: Arc<CountingNotifier>,
    service: ReconcileService<OrmResultsRepository, OrmWatermarkRepository>,
}

impl Fixture {
    async fn with_authz(
        lookback: Duration,
        page_size: u32,
        authz: Arc<dyn AuthZResolverApi>,
    ) -> Self {
        let db = inmem_db().await;
        let provider = Arc::new(DBProvider::<DomainError>::new(db.clone()));
        let runs = Arc::new(FakeRuns::default());
        let ingest = Arc::new(IngestService::new(OrmResultsRepository, runs.clone()));
        let notifier = Arc::new(CountingNotifier::default());
        let service = ReconcileService::new(
            provider.clone(),
            OrmResultsRepository,
            OrmWatermarkRepository,
            runs.clone(),
            ingest,
            Arc::clone(&notifier) as Arc<dyn RunCompletionNotifier>,
            PolicyEnforcer::new(authz),
            lookback,
            page_size,
        );
        Self {
            db,
            provider,
            runs,
            ctx: crate::domain::service::test_support::ctx(TENANT),
            notifier,
            service,
        }
    }

    async fn with_lookback(lookback: Duration, page_size: u32) -> Self {
        Self::with_authz(lookback, page_size, Arc::new(TenantScopedAuthZ)).await
    }

    async fn new() -> Self {
        Self::with_lookback(Duration::hours(1), 200).await
    }

    async fn results_for(&self, run_id: Uuid) -> Vec<qa_insights_sdk::TestResultRecord> {
        let conn = self.db.conn().unwrap();
        OrmResultsRepository
            .list_by_run(&conn, &scope(TENANT), run_id)
            .await
            .unwrap()
    }

    /// Every run [`ReconcileService::reproject`] asked to notify, in order.
    fn notified_runs(&self) -> Vec<Uuid> {
        self.notifier.notified()
    }

    async fn watermark(&self) -> Option<OffsetDateTime> {
        let conn = self.db.conn().unwrap();
        OrmWatermarkRepository
            .get(&conn, &scope(TENANT))
            .await
            .unwrap()
            .last_reconciled_finished_at
    }

    /// A second `ReconcileService` over **this fixture's** database and
    /// qa-runs, differing only in its lookback.
    ///
    /// Stands in for a second replica. Under `NoopLeaderElector` every replica
    /// is the leader, so two of them sweep one tenant against one watermark
    /// row — and during a rolling deploy that changed
    /// `reconcile_lookback_seconds`, or one that halted part way, the two carry
    /// *different* lookbacks and therefore derive different floors.
    fn sibling_with_lookback(
        &self,
        lookback: Duration,
        page_size: u32,
    ) -> ReconcileService<OrmResultsRepository, OrmWatermarkRepository> {
        ReconcileService::new(
            self.provider.clone(),
            OrmResultsRepository,
            OrmWatermarkRepository,
            self.runs.clone(),
            Arc::new(IngestService::new(OrmResultsRepository, self.runs.clone())),
            Arc::clone(&self.notifier) as Arc<dyn RunCompletionNotifier>,
            PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
            lookback,
            page_size,
        )
    }

    /// The within-window resume point as the database holds it.
    ///
    /// Read through the repository rather than asserted on a log line: the
    /// whole of finding #122's fix is that something *durable* survives a pass
    /// that ran out of budget, so the assertion has to be about the row.
    async fn sweep_cursor(&self) -> Option<SweepCursor> {
        let conn = self.db.conn().unwrap();
        OrmWatermarkRepository
            .get(&conn, &scope(TENANT))
            .await
            .unwrap()
            .sweep_cursor
    }

    /// Write the cursor directly, standing in for the *other* replica.
    ///
    /// Under `NoopLeaderElector` every replica is the leader and two of them
    /// share this one row, so "a cursor written by someone else landed on top
    /// of mine" is a production state, not a contrivance. A second
    /// `ReconcileService` over the same database would only ever produce
    /// *sequential* passes, which cannot express it.
    async fn write_sweep_cursor(&self, cursor: Option<SweepCursor>) {
        self.provider
            .transaction(move |tx| {
                Box::pin(async move {
                    OrmWatermarkRepository
                        .set_sweep_cursor(tx, &scope(TENANT), cursor)
                        .await
                })
            })
            .await
            .unwrap();
    }

    async fn set_watermark(&self, instant: OffsetDateTime) {
        self.provider
            .transaction(move |tx| {
                Box::pin(async move {
                    OrmWatermarkRepository
                        .advance(
                            tx,
                            &scope(TENANT),
                            TENANT,
                            WatermarkKind::ReconciledFinishedAt,
                            instant,
                        )
                        .await
                })
            })
            .await
            .unwrap();
    }
}

/// The sweep's core property: a run this gear never separately learned about is
/// fully backfilled by the next sweep regardless. This is the property that
/// makes the reconcile sweep this gear's ingest path rather than merely a
/// backstop for one.
#[tokio::test]
async fn a_run_whose_events_were_never_delivered_is_backfilled() {
    let f = Fixture::new().await;
    let ghost = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 3);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(f.results_for(ghost).await.len(), 3);
    assert_eq!(outcome.backfilled, 1);
    assert!(!outcome.stopped_at_gap);
    assert_eq!(
        outcome.stopped_at_run, None,
        "nothing failed, so there is no run to name"
    );
}

/// The watermark advances only past runs actually ingested. A sweep that
/// advanced on the page boundary would strand a run whose backfill failed.
#[tokio::test]
async fn a_failed_backfill_does_not_advance_the_watermark() {
    let f = Fixture::new().await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 1);
    f.runs.fail_result_reads(true);

    let before = f.watermark().await;
    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the sweep itself succeeds; a per-run gap is an outcome, not an error");

    assert_eq!(
        f.watermark().await,
        before,
        "watermark must not move past a gap"
    );
    assert!(outcome.stopped_at_gap);
    assert_eq!(outcome.backfilled, 0);
}

/// A run the finished-since listing reports that has vanished by the time
/// `get_run` is asked for it by id — deleted, or never really visible to that
/// read — does not stop the sweep, and the sweep still backfills the run
/// after it in the same page.
///
/// This is `IngestService::read_run_projection`'s
/// `DomainError::RunNotIngested => Ok(None)` arm's only exercise: the deleted
/// event-consumer test `an_event_for_a_vanished_run_is_acknowledged` used to
/// drive it and nothing replaced that coverage when the consumer was deleted
/// as dead code. Verified by temporarily changing that arm to `Err(other)`
/// instead of `Ok(None)`: this test fails with `stopped_at_gap: true`, no
/// results for the run after the vanished one, and a watermark that never
/// reaches it — which is also what an operator would see on every subsequent
/// sweep, forever, if that regression shipped, signalled only by
/// `stopped_at_gap` with no test going red.
#[tokio::test]
async fn a_run_the_listing_reports_but_get_run_cannot_see_does_not_stop_the_sweep() {
    let f = Fixture::new().await;
    let _vanished = f
        .runs
        .add_finished_run_that_vanishes(at("2026-08-18T10:00:00Z"));
    let after = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:05:00Z"), 1);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the sweep itself succeeds; a vanished run is an outcome, not an error");

    assert!(!outcome.stopped_at_gap, "{outcome:?}");
    assert_eq!(outcome.stopped_at_run, None);
    assert_eq!(
        f.results_for(after).await.len(),
        1,
        "the run after the vanished one must still be backfilled - the sweep did not stop"
    );
    assert_eq!(
        f.watermark().await,
        Some(at("2026-08-18T10:05:00Z")),
        "the watermark must advance past the vanished run, not stall on it"
    );
}

/// The lookback exists because a run can be written after a later sweep's
/// cutoff has already passed. Sweeping strictly from the watermark would miss
/// it.
#[tokio::test]
async fn the_sweep_starts_a_lookback_before_the_watermark() {
    let f = Fixture::with_lookback(Duration::hours(1), 200).await;
    f.set_watermark(at("2026-08-18T12:00:00Z")).await;
    let late = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:30:00Z"), 1);

    f.service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(f.results_for(late).await.len(), 1);
}

/// The other half of the lookback: a run *older* than the window is not
/// re-read. Without this, the previous test would also pass for a sweep that
/// ignored the watermark entirely and always started from the epoch.
#[tokio::test]
async fn a_run_older_than_the_lookback_window_is_left_alone() {
    let f = Fixture::with_lookback(Duration::hours(1), 200).await;
    f.set_watermark(at("2026-08-18T12:00:00Z")).await;
    let ancient = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert!(
        f.results_for(ancient).await.is_empty(),
        "a run three hours behind a one-hour lookback is outside the window"
    );
    assert_eq!(outcome.scanned, 0);
}

/// **The wedge that stopped three days of ingest, and the only assertion that
/// sees it.** `page_size` runs inside the lookback window must not be able to
/// pin the watermark to where it already is.
///
/// The floor is `mark - lookback`, *behind* the mark, and the listing is capped
/// at `page_size` and oldest-first. So when `page_size` runs finished inside
/// `[mark - lookback, mark]`, the page ends on the mark, `advance` is monotonic
/// and no-ops, and the next sweep computes the identical floor and reads the
/// identical page — forever, with no error and a healthy-looking `scanned`.
///
/// Here that is four runs in the hour behind a mark of 12:00 with
/// `page_size = 4`. Against the single-page sweep this reproduces the stand
/// exactly: the watermark stays at 12:00 and the two runs after it are never
/// ingested, on this pass or on any later one. What makes it a *silent* failure
/// — and so what makes this test necessary — is that the sweep still returns
/// `Ok`, still reports `stopped_at_gap: false`, and still counts a page's worth
/// of `scanned`.
#[tokio::test]
async fn a_lookback_window_holding_a_full_page_does_not_pin_the_watermark() {
    let f = Fixture::with_lookback(Duration::hours(1), 4).await;
    f.set_watermark(at("2026-08-18T12:00:00Z")).await;

    // Exactly `page_size` runs in `[mark - lookback, mark]`: the page the sweep
    // reads first cannot reach past the mark.
    for instant in [
        "2026-08-18T11:15:00Z",
        "2026-08-18T11:30:00Z",
        "2026-08-18T11:45:00Z",
        "2026-08-18T12:00:00Z",
    ] {
        f.runs.add_finished_run_with_results(at(instant), 1);
    }

    // The runs the wedge strands. On the stand these were three days of them.
    let after_a = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:10:00Z"), 1);
    let after_b = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:20:00Z"), 1);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert!(
        !outcome.stopped_at_gap,
        "the wedge is not a gap; a sweep that reports one is failing a different way"
    );
    assert_eq!(
        f.results_for(after_a).await.len(),
        1,
        "a run past a saturated lookback window must still be ingested"
    );
    assert_eq!(
        f.results_for(after_b).await.len(),
        1,
        "and so must the one after it"
    );
    assert_eq!(
        f.watermark().await,
        Some(at("2026-08-18T12:20:00Z")),
        "the mark must land on the newest run this pass accounted for, not stay \
         where a full lookback window pinned it"
    );
}

/// **#121, and it is the 2026-09-18 outage reachable from configuration alone.**
///
/// `a_lookback_window_holding_a_full_page_does_not_pin_the_watermark` above is
/// this wedge at a page size the far side honours. This is the same wedge
/// re-opened by a page size it does **not**: qa-runs caps this listing at
/// [`MAX_FINISHED_RUNS_PAGE`] (`runs_sea_repo::sweep_limit`) and says nothing
/// about having cut the page, so a sweep configured above that cap gets a full
/// page back, measures `500 >= 800`, and concludes it has caught up.
///
/// Every consequence of that conclusion is silent. The walk stops after one
/// page; the page sits entirely inside `[mark - lookback, mark]`, so the
/// advance is a monotonic no-op; nothing after the mark is ever read; and
/// because `caught_up` is `true`, `crate::gear::stall_of` — which requires
/// `!caught_up` — refuses to escalate. No `WARN`, no `ERROR`, no
/// `stopped_at_gap`, and a healthy-looking `scanned` on every tick, forever.
///
/// The fixture is the stand's shape at the smallest size that can express it:
/// exactly [`MAX_FINISHED_RUNS_PAGE`] runs in the hour behind a mark of 12:00,
/// a `reconcile_page_size` of 800, and two runs past the mark to be stranded.
/// It cannot be scaled down — the defect *is* the relationship between the
/// configured page and the port's cap, so the port's real cap is the number
/// that has to be in the fixture.
#[tokio::test]
async fn a_page_size_above_the_port_cap_does_not_wedge_the_walk() {
    let f = Fixture::with_lookback(Duration::hours(1), ABOVE_THE_CAP).await;
    let mark = at("2026-08-18T12:00:00Z");
    f.set_watermark(mark).await;

    // A full page's worth of runs, all inside `[mark - lookback, mark]`, so the
    // page the sweep reads first cannot reach past the mark.
    let window_start = at("2026-08-18T11:00:00Z");
    for n in 1..=i64::from(MAX_FINISHED_RUNS_PAGE) {
        f.runs
            .add_finished_run_with_results(window_start + Duration::seconds(n), 0);
    }

    // The runs the wedge strands. On the stand these were three days of them.
    let after_a = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:10:00Z"), 1);
    let after_b = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:20:00Z"), 1);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert!(
        !outcome.stopped_at_gap,
        "the wedge is not a gap; a sweep that reports one is failing a different way"
    );
    assert_eq!(
        f.results_for(after_a).await.len(),
        1,
        "a run past a page the port truncated must still be ingested: asked for \
         {ABOVE_THE_CAP} and a short-looking page is what froze the projection for \
         three days"
    );
    assert_eq!(
        f.results_for(after_b).await.len(),
        1,
        "and so must the one after it"
    );
    assert_eq!(
        f.watermark().await,
        Some(at("2026-08-18T12:20:00Z")),
        "the mark must land on the newest run this pass accounted for, not stay \
         where a truncated page pinned it"
    );
}

/// **One pass over a window wider than its budget: short of the mark, and it
/// says so and says where it stopped.**
///
/// The sweep starts at `floor = mark - lookback`, deliberately behind the
/// mark, so a lookback window holding more than
/// [`MAX_PAGES_PER_SWEEP`](super::MAX_PAGES_PER_SWEEP) `* page_size` runs lets
/// a pass spend its whole budget without ever reading past the mark. Every
/// page is real work; the advance is monotonic, so handing it an instant
/// behind the mark succeeds and changes nothing *on this pass*. The window
/// drains across ticks through the resume cursor this pass leaves —
/// `a_window_wider_than_the_page_budget_drains_across_consecutive_ticks` is
/// the next tick — and this test was named "pins the mark" until finding
/// #122's fix made that untrue of any tick but this one.
///
/// **What is asserted is the shape one such pass hands
/// [`report_reconcile_outcome`](crate::gear)**: an unmoved `watermark_at`,
/// `caught_up` false, and a resume cursor inside the window. `gear::stall_of`
/// reads those three — the mark, `caught_up`, and the cursor's rise against
/// its high-water — so this is the reachable single-pass input its ladder is
/// driven with; whether consecutive passes of it escalate depends on whether
/// the cursor rises, which the drain and replica-wedge tests below assert.
///
/// The fixture is the smallest that can express it: `page_size = 2`, so 50
/// pages consume 100 runs, and 110 runs sit inside the window. Finite and
/// small on purpose — the defect is the ratio of the budget to the window, not
/// the size of either.
///
/// **The run count is a literal and the budget is pinned beside it**, rather
/// than the count being computed from
/// [`MAX_PAGES_PER_SWEEP`](super::MAX_PAGES_PER_SWEEP). A fixture sized from
/// the constant grows with it and can never fail: raising the budget to 500
/// raised the window to 1,010 and the test still passed. That is the shape of
/// guard this branch has shipped five of. Pinning the constant instead makes a
/// change to it a loud failure on the next line, which is the honest answer —
/// the fixture's arithmetic only holds for this value.
#[tokio::test]
async fn a_window_wider_than_the_page_budget_stops_short_and_says_it_did_not_catch_up() {
    assert_eq!(
        super::MAX_PAGES_PER_SWEEP,
        50,
        "this fixture's 110 runs are sized against a budget of 50 pages of 2; \
         re-derive the count by hand for the new budget rather than computing \
         it from the constant, which would make this test vacuous"
    );
    let page_size = 2_u32;
    let f = Fixture::with_lookback(Duration::hours(1), page_size).await;
    let mark = at("2026-08-18T12:00:00Z");
    f.set_watermark(mark).await;

    // 110 runs inside `[mark - lookback, mark]` against a budget of 100, so the
    // pass cannot reach the mark, let alone anything past it.
    let window_start = at("2026-08-18T11:00:00Z");
    for n in 1..=110 {
        f.runs
            .add_finished_run_with_results(window_start + Duration::seconds(n), 0);
    }
    // The run this pass cannot reach; the next tick does.
    let stranded = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:10:00Z"), 1);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("a budget-exhausted sweep is not an error; that is the whole finding");

    assert!(
        !outcome.caught_up,
        "the walk stopped with the window unexhausted, so it has not caught up, \
         and `gear::stall_of` will not count a pass as a stall without this"
    );
    assert_eq!(
        outcome.watermark_at,
        Some(mark),
        "the mark stays where it was on this pass: every page it spent sits at \
         or behind it, and `WatermarkRepository::advance` is monotonic"
    );
    assert_eq!(
        f.watermark().await,
        Some(mark),
        "and the stored mark agrees; what carries this pass' progress to the \
         next tick is the cursor asserted below"
    );
    assert_eq!(
        outcome.resume_from, None,
        "and it names no resume point *on the wire*: `resume_from` is a \
         rebuild's field. What the sweep leaves instead is the durable \
         `SweepCursor` asserted below"
    );
    assert!(
        f.results_for(stranded).await.is_empty(),
        "the cost within one pass: a run past the window is not reached by it. \
         Which tick reaches it is \
         `a_window_wider_than_the_page_budget_drains_across_consecutive_ticks`"
    );
    // **The half of this that changed with finding #122's fix.** This assertion
    // used to read "there is nothing durable left behind by a wedged pass but
    // the alarm", and that was the finding: the next tick re-derived the same
    // floor and repeated this pass exactly, forever. The pass still ends here
    // and still says so; what is new is that it says where it stopped.
    let cursor = f.sweep_cursor().await.expect(
        "a pass that spent its budget must leave a resume point, or the next tick \
         re-derives this same floor and this test is the wedge again",
    );
    assert_eq!(
        cursor.window_floor,
        mark - Duration::hours(1),
        "the cursor records the floor this pass walked from; `first_page_of` \
         honours it only for a reader whose floor lies inside `[floor, at]`"
    );
    assert!(
        cursor.at > window_start && cursor.at < mark,
        "and it stands inside that window, where this pass ran out of budget: \
         got {}",
        cursor.at
    );
    assert!(
        !outcome.stopped_at_gap,
        "this is not a gap; a sweep reporting one here is failing a different way"
    );
}

/// **Finding #122, eliminated rather than alarmed.** The pass above ends short
/// of the mark and can do nothing else; this is the assertion that the *next*
/// tick continues it instead of repeating it, and that the window drains.
///
/// Same fixture as the test above and deliberately so — 110 runs inside
/// `[mark - lookback, mark]` against a budget of 50 pages of 2, so the first
/// pass spends every page behind its own mark, where
/// `WatermarkRepository::advance` is a monotonic no-op. Before the resume
/// cursor the second tick derived the identical floor, read the identical 100
/// runs and made the identical no-op; the run past the mark was stranded for as
/// long as the window stood, which on the dev stand was three days and five
/// redeploys.
///
/// **The run count is a literal and the budget is pinned beside it**, for the
/// reason the test above states at length: a fixture sized from
/// [`MAX_PAGES_PER_SWEEP`](super::MAX_PAGES_PER_SWEEP) grows with it and can
/// never fail.
#[tokio::test]
async fn a_window_wider_than_the_page_budget_drains_across_consecutive_ticks() {
    assert_eq!(
        super::MAX_PAGES_PER_SWEEP,
        50,
        "this fixture's 110 runs are sized against a budget of 50 pages of 2; \
         re-derive the count by hand for the new budget rather than computing \
         it from the constant, which would make this test vacuous"
    );
    let page_size = 2_u32;
    let f = Fixture::with_lookback(Duration::hours(1), page_size).await;
    let mark = at("2026-08-18T12:00:00Z");
    f.set_watermark(mark).await;

    // 110 runs inside `[mark - lookback, mark]` against a budget of 100.
    let window_start = at("2026-08-18T11:00:00Z");
    for n in 1..=110_i64 {
        f.runs
            .add_finished_run_with_results(window_start + Duration::seconds(n), 0);
    }
    // The run the wedge used to strand for good.
    let past_the_mark = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:10:00Z"), 1);

    // ---- tick one: the budget goes entirely behind the mark ---------------
    let first = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("a budget-exhausted sweep is not an error");

    assert!(
        !first.caught_up,
        "the window is not exhausted after one pass"
    );
    assert_eq!(
        f.watermark().await,
        Some(mark),
        "and the mark cannot have moved: every page this pass spent sits behind it"
    );
    assert!(
        f.results_for(past_the_mark).await.is_empty(),
        "one pass cannot reach past the window; that is the budget, not the wedge"
    );
    let cursor = f
        .sweep_cursor()
        .await
        .expect("the pass must have recorded where it stopped");
    assert_eq!(
        cursor.window_floor, window_start,
        "the cursor is scoped to `mark - lookback`, the window this pass walked"
    );
    assert!(
        cursor.at > window_start && cursor.at < mark,
        "and it stands inside that window: got {}",
        cursor.at
    );

    // ---- tick two: it resumes, drains, and reaches past the mark ----------
    let second = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the resuming sweep succeeds");

    assert!(
        second.caught_up,
        "the second pass finishes the window and catches up; without the resume \
         cursor it re-derives the same floor and reads the same 100 runs forever"
    );
    assert_eq!(
        f.results_for(past_the_mark).await.len(),
        1,
        "the run the wedge stranded is ingested on the very next tick"
    );
    assert_eq!(
        f.watermark().await,
        Some(at("2026-08-18T12:10:00Z")),
        "and the mark finally moves past the window that pinned it"
    );
    assert_eq!(
        f.sweep_cursor().await,
        None,
        "a pass that caught up erases its cursor: the next tick is back to \
         `mark - lookback`, which is what keeps this from being a second watermark"
    );
}

/// **The lookback still does its job, and this is the run that proves it.**
///
/// The floor sits behind the mark because a run can be *written* with a
/// `finished_at` earlier than one already swept. A resume cursor that simply
/// moved forward would skip exactly those runs — so the cursor is erased by any
/// pass that catches up, and the tick after a drain finishes is back to
/// `mark - lookback` and re-reads the whole lookback window.
///
/// The run here arrives **behind** a live cursor, mid-drain, and the test
/// asserts both halves honestly: the pass that resumes past it does *not* see
/// it (a cursor that pretended otherwise would be lying), and the pass after
/// the drain finishes does.
///
/// A two-hour lookback against a window that ends at 11:55 is not padding: the
/// mark lands on 12:10 when the drain completes, so a one-hour lookback would
/// put the floor at 11:10 and the late run genuinely outside the window this
/// tenant re-reads. That is the lookback's own limit, unchanged by the cursor,
/// and the fixture is sized to test the cursor rather than to re-test it.
#[tokio::test]
async fn a_run_written_behind_the_resume_cursor_is_still_ingested_once_the_window_drains() {
    assert_eq!(
        super::MAX_PAGES_PER_SWEEP,
        50,
        "this fixture's 110 runs are sized against a budget of 50 pages of 2; \
         re-derive the count by hand for the new budget rather than computing \
         it from the constant"
    );
    let page_size = 2_u32;
    let f = Fixture::with_lookback(Duration::hours(2), page_size).await;
    let mark = at("2026-08-18T12:00:00Z");
    f.set_watermark(mark).await;

    // 110 runs spread across the window rather than bunched at its start, so
    // that the floor derived after the drain still covers the middle of it.
    let window_start = at("2026-08-18T10:05:00Z");
    for n in 1..=110_i64 {
        f.runs
            .add_finished_run_with_results(window_start + Duration::minutes(n), 0);
    }
    let past_the_mark = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:10:00Z"), 1);

    // ---- tick one: 100 of the 110, leaving a cursor at 11:45 -------------
    f.service
        .reconcile_once(tenant())
        .await
        .expect("a budget-exhausted sweep is not an error");
    let cursor = f
        .sweep_cursor()
        .await
        .expect("the pass must have recorded where it stopped");
    assert_eq!(
        cursor.at,
        at("2026-08-18T11:45:00Z"),
        "100 runs at one-minute spacing from 10:06 ends on 11:45"
    );

    // qa-runs writes a run *behind* the live cursor. This is the case the
    // lookback exists for and the one a forward-only resume point destroys.
    let late = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:30:30Z"), 1);

    // ---- tick two: it resumes past the late run, and says so by not ------
    // ---- ingesting it -----------------------------------------------------
    let second = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the resuming sweep succeeds");
    assert!(second.caught_up, "the second pass drains the window");
    assert!(
        f.results_for(late).await.is_empty(),
        "the resuming pass starts strictly after its cursor, so it genuinely \
         steps over a run written behind it, and the fix would be dishonest if it \
         claimed otherwise"
    );
    assert_eq!(
        f.sweep_cursor().await,
        None,
        "and the pass that caught up erased the cursor, which is what puts the \
         next tick back on `mark - lookback`"
    );

    // ---- tick three: the re-derived floor finds it ------------------------
    f.service
        .reconcile_once(tenant())
        .await
        .expect("the third sweep succeeds");
    assert_eq!(
        f.results_for(late).await.len(),
        1,
        "a run written behind the cursor is picked up by the next pass that \
         derives the floor from the mark and the lookback. A cursor that \
         survived a caught-up pass would have stranded it for good"
    );
    assert_eq!(
        f.results_for(past_the_mark).await.len(),
        1,
        "and the run past the mark stays ingested"
    );
}

/// **A caught-up pass erases the cursor even when it inherited one.**
///
/// `a_window_wider_than_the_page_budget_drains_across_consecutive_ticks`
/// asserts the same erase at the end of a real drain, where the mark also
/// moves. This isolates it: the cursor is handed to the sweep directly, the
/// window is small enough to finish in one page, and the only thing under test
/// is that catching up clears the row.
///
/// Without the erase the cursor is a second watermark — it only ever moves
/// forward, the lookback is never re-walked, and every late-written run behind
/// it is lost.
#[tokio::test]
async fn a_pass_that_catches_up_erases_a_cursor_it_inherited() {
    let f = Fixture::with_lookback(Duration::hours(1), 200).await;
    let mark = at("2026-08-18T12:00:00Z");
    f.set_watermark(mark).await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T11:30:00Z"), 1);

    f.write_sweep_cursor(Some(SweepCursor {
        window_floor: at("2026-08-18T11:00:00Z"),
        at: at("2026-08-18T11:20:00Z"),
        run_id: Uuid::from_u128(0xBEEF),
    }))
    .await;

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert!(outcome.caught_up, "one short page is a caught-up walk");
    assert_eq!(
        f.sweep_cursor().await,
        None,
        "catching up must clear the row; a cursor that survived would make the \
         next tick skip the window behind it and the lookback would be dead"
    );
}

/// **A service constructed with a page size of `0` still walks** — finding
/// #122's residual C, at the type rather than the configuration.
///
/// `QaInsightsConfig::effective_reconcile_page_size` raises a configured `0`
/// and warns; `ReconcileService::new` floors what it stores, so no constructed
/// service can hold one. Unfloored, every listing asks for zero rows, comes
/// back empty, and the sweep reports itself caught up having read nothing.
#[tokio::test]
async fn a_service_constructed_with_a_page_size_of_zero_still_walks() {
    let f = Fixture::with_lookback(Duration::hours(1), 0).await;
    let runs: Vec<Uuid> = [10, 20, 30]
        .into_iter()
        .map(|minute| {
            f.runs
                .add_finished_run_with_results(at(&format!("2026-08-18T09:{minute:02}:00Z")), 1)
        })
        .collect();

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(outcome.scanned, 3, "one run per page, three pages");
    for run in runs {
        assert_eq!(f.results_for(run).await.len(), 1);
    }
    assert!(outcome.caught_up);
}

/// **A cursor from a *later* floor than this pass' is discarded**, and this is
/// the half of the honour rule that protects a wider lookback.
///
/// `first_page_of` honours a stored cursor only when its `window_floor` is at or
/// before the floor this pass derives. A cursor `{F_w, K}` says "every run in
/// `[F_w, K]` was consumed" — nothing about the band before `F_w`. A reader
/// whose floor is *earlier* (a replica carrying a wider
/// `reconcile_lookback_seconds` mid-rollout) needs exactly that band re-read,
/// because it is the late-arrival band its wider lookback exists for.
/// Honouring the narrower replica's cursor would step over it.
///
/// Stored `{floor 11:30, at 11:40}`; this pass' floor is 11:00; the run at
/// 11:10 sits in the band only this reader covers.
#[tokio::test]
async fn a_cursor_from_a_later_floor_than_this_pass_is_discarded() {
    let f = Fixture::with_lookback(Duration::hours(1), 200).await;
    f.set_watermark(at("2026-08-18T12:00:00Z")).await;
    let in_the_wider_band = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:10:00Z"), 1);

    f.write_sweep_cursor(Some(SweepCursor {
        // Later than this pass' `mark - lookback`, which is 11:00.
        window_floor: at("2026-08-18T11:30:00Z"),
        at: at("2026-08-18T11:40:00Z"),
        run_id: Uuid::from_u128(0xBEEF),
    }))
    .await;

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(
        f.results_for(in_the_wider_band).await.len(),
        1,
        "the stored cursor vouches only for `[11:30, 11:40]`, so a pass whose \
         floor is 11:00 must walk from 11:00 and read the run at 11:10"
    );
    assert!(
        outcome.caught_up,
        "one short page from the floor is caught up"
    );
}

/// **A cursor survives a floor that moved forward**, which is finding #122's
/// residual A.
///
/// During a drain the mark moves once the walk passes it, and the next tick
/// derives a later floor. The old rule (`window_floor == floor`) discarded the
/// cursor on exactly that tick and re-walked the lookback from the new floor —
/// work the previous pass had already done. The rule now is
/// `window_floor <= floor && at >= floor`: the stored cursor says every run in
/// `[window_floor, at]` was consumed, and `[floor, at]` is a subset of that, so
/// resuming strictly after `at` skips nothing the stored pass did not consume.
///
/// Stored `{floor 11:00, at 11:40}`; lookback 30 minutes against a mark of
/// 12:00 puts this pass' floor at 11:30. The run at 11:35 is inside `[11:30,
/// 11:40]`, which the cursor vouches for: the pass must not list it again. (It
/// was never really ingested here — the fixture writes the cursor directly —
/// which is what lets the assertion tell "skipped" from "re-read".)
#[tokio::test]
async fn a_cursor_inside_a_window_whose_floor_moved_forward_is_honoured() {
    let f = Fixture::with_lookback(Duration::minutes(30), 200).await;
    f.set_watermark(at("2026-08-18T12:00:00Z")).await;
    let covered = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:35:00Z"), 1);
    let after_the_cursor = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:50:00Z"), 1);

    f.write_sweep_cursor(Some(SweepCursor {
        window_floor: at("2026-08-18T11:00:00Z"),
        at: at("2026-08-18T11:40:00Z"),
        run_id: Uuid::from_u128(0xBEEF),
    }))
    .await;

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(
        outcome.scanned, 1,
        "the pass resumes strictly after 11:40, so the only run it lists is 11:50"
    );
    assert_eq!(f.results_for(after_the_cursor).await.len(), 1);
    assert!(
        f.results_for(covered).await.is_empty(),
        "11:35 lies inside the range the stored cursor vouches for, so the pass \
         must not re-walk from its own floor to reach it"
    );
    assert!(outcome.caught_up);
    assert_eq!(f.sweep_cursor().await, None, "and catching up erases it");
}

/// **A cursor below a forward-moved floor starts the walk at the floor.**
///
/// The other bound of the honour rule: `at >= floor`. A stored cursor at 10:30
/// with a pass floor of 11:30 names a position this pass' window does not
/// contain, and resuming "after 10:30" would re-read runs outside the window
/// rather than skip any — harmless, but not the rule. The walk starts at the
/// floor, and the run at 11:10 (behind the floor) is left alone.
#[tokio::test]
async fn a_cursor_below_a_forward_moved_floor_starts_at_the_floor() {
    let f = Fixture::with_lookback(Duration::minutes(30), 200).await;
    f.set_watermark(at("2026-08-18T12:00:00Z")).await;
    let behind_the_floor = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:10:00Z"), 1);
    let inside = [
        f.runs
            .add_finished_run_with_results(at("2026-08-18T11:35:00Z"), 1),
        f.runs
            .add_finished_run_with_results(at("2026-08-18T11:50:00Z"), 1),
    ];

    f.write_sweep_cursor(Some(SweepCursor {
        window_floor: at("2026-08-18T10:00:00Z"),
        at: at("2026-08-18T10:30:00Z"),
        run_id: Uuid::from_u128(0xBEEF),
    }))
    .await;

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(outcome.scanned, 2, "the walk starts at the 11:30 floor");
    for run in inside {
        assert_eq!(f.results_for(run).await.len(), 1);
    }
    assert!(
        f.results_for(behind_the_floor).await.is_empty(),
        "a run behind the floor is outside the window whatever the cursor says"
    );
}

/// **A drain that advances the mark does not re-walk the lookback**, which is
/// what residual A cost before it was fixed.
///
/// 210 runs every 30 seconds from 11:00:30, against a mark of 12:00 and a
/// one-hour lookback: 120 of them behind the mark, 90 past it. A budget of 50
/// pages of 2 reads 100 runs a pass. Pass one ends at 11:50 behind the mark;
/// pass two resumes and ends at 12:40, moving the mark there; pass three
/// derives a floor of 11:40. Under the old equality rule that floor discarded
/// the cursor and the pass re-walked from 11:40 — 131 runs, more than a
/// budget, so the drain took extra ticks re-reading runs it had already
/// consumed. Under the honour rule pass three resumes after 12:40 and reads
/// the last ten.
///
/// The assertion is on listings: one walk over 210 runs at a page of 2 is 105
/// full pages plus one empty page that says "caught up" — 106 — and a drain
/// that re-walked any part of the lookback costs more. Literal count, budget
/// pinned beside it.
#[tokio::test]
async fn a_drain_that_advances_the_mark_does_not_rewalk_the_lookback() {
    assert_eq!(
        super::MAX_PAGES_PER_SWEEP,
        50,
        "this fixture's 210 runs are sized against a budget of 50 pages of 2; \
         re-derive the count and the listing bound by hand for the new budget"
    );
    let page_size = 2_u32;
    let f = Fixture::with_lookback(Duration::hours(1), page_size).await;
    f.set_watermark(at("2026-08-18T12:00:00Z")).await;

    let window_start = at("2026-08-18T11:00:00Z");
    let runs: Vec<Uuid> = (1..=210_i64)
        .map(|n| {
            f.runs
                .add_finished_run_with_results(window_start + Duration::seconds(30 * n), 1)
        })
        .collect();

    // Finite: a drain that has not caught up in six ticks is a failure, not a
    // reason to keep going.
    let mut caught_up = false;
    for _ in 0..6 {
        let outcome = f
            .service
            .reconcile_once(tenant())
            .await
            .expect("a draining sweep is not an error");
        if outcome.caught_up {
            caught_up = true;
            break;
        }
    }

    assert!(caught_up, "the drain must finish within six ticks");
    for (n, run) in runs.iter().enumerate() {
        assert_eq!(f.results_for(*run).await.len(), 1, "run {n} ingested");
    }
    assert_eq!(f.watermark().await, Some(at("2026-08-18T12:45:00Z")));
    assert!(
        f.runs.listings() <= 106,
        "one walk over 210 runs in pages of 2 is 105 pages and a closing empty \
         one; {} listings means some tick re-walked ground an earlier pass had \
         already consumed",
        f.runs.listings()
    );
}

/// **The cursor never names a run the pass did not consume**, which is the
/// invariant the whole concurrency argument rests on.
///
/// `SweepCursor`'s doc says two replicas sharing this row cannot skip a run
/// because every value either of them writes is a position its writer fully
/// walked to. That is only true while a gap stops the cursor exactly where it
/// stops the watermark, and this is the assertion that holds it there: the run
/// that fails is 11:30 and the cursor must stand on 11:20.
///
/// A cursor written one run further on would strand 11:30 on every later tick
/// — the same permanent loss the sweep's stop-at-the-first-failure rule exists
/// to prevent for the mark, reintroduced through a second column.
#[tokio::test]
async fn a_pass_that_stops_at_a_gap_leaves_its_cursor_before_the_gap() {
    let f = Fixture::with_lookback(Duration::hours(1), 4).await;
    let mark = at("2026-08-18T12:00:00Z");
    f.set_watermark(mark).await;

    f.runs
        .add_finished_run_with_results(at("2026-08-18T11:10:00Z"), 1);
    let before_the_gap = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:20:00Z"), 1);
    let gap = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:30:00Z"), 1);
    let after_the_gap = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:40:00Z"), 1);
    f.runs.fail_result_reads_for(gap);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("a per-run failure is not an error");

    assert!(outcome.stopped_at_gap);
    assert_eq!(outcome.stopped_at_run, Some(gap));
    let cursor = f
        .sweep_cursor()
        .await
        .expect("a pass that stopped short records where it stopped");
    assert_eq!(
        cursor.at,
        at("2026-08-18T11:20:00Z"),
        "the cursor stands on the last run consumed, never on the one that failed"
    );
    assert_eq!(cursor.run_id, before_the_gap);
    assert!(
        f.results_for(after_the_gap).await.is_empty(),
        "and nothing past the gap was consumed"
    );

    // The failure clears; the next tick must re-reach the run it stopped on.
    f.runs.fail_result_reads_for(Uuid::from_u128(0xDEAD));
    f.service
        .reconcile_once(tenant())
        .await
        .expect("the resuming sweep succeeds");

    assert_eq!(
        f.results_for(gap).await.len(),
        1,
        "the run that failed is re-reached, because the cursor stopped before it"
    );
    assert_eq!(f.results_for(after_the_gap).await.len(), 1);
    assert_eq!(
        f.sweep_cursor().await,
        None,
        "and the pass that finished the window cleared the cursor"
    );
}

/// **Two replicas share one cursor row: a rewind costs work, never coverage.**
///
/// Leader election in this gear is an optimisation rather than mutual
/// exclusion, and under the shipped `NoopLeaderElector` every replica is the
/// leader — so the write is last-writer-wins with no monotonic predicate. The
/// slower replica's write landing on top of the faster one's is the interleaving
/// that matters, and [`Fixture::write_sweep_cursor`] is it: the row is moved
/// **backwards**, to run 50 of a window the first pass had walked to run 100 of.
///
/// The first assertion is the load-bearing one and it is about the repository
/// rather than the sweep: **the rewind must land**. Adding a
/// `WHERE sweep_cursor_at < at` predicate to `set_sweep_cursor` is the tempting
/// "safety" change, and it would silently turn this cursor into a second
/// monotonic watermark — the exact thing that kills the lookback. This line is
/// what goes red if anyone makes it.
#[tokio::test]
async fn a_resume_cursor_rewound_by_another_replica_skips_nothing() {
    assert_eq!(
        super::MAX_PAGES_PER_SWEEP,
        50,
        "this fixture's 110 runs are sized against a budget of 50 pages of 2"
    );
    let page_size = 2_u32;
    let f = Fixture::with_lookback(Duration::hours(1), page_size).await;
    let mark = at("2026-08-18T12:00:00Z");
    f.set_watermark(mark).await;

    let window_start = at("2026-08-18T11:00:00Z");
    let window: Vec<Uuid> = (1..=110_i64)
        .map(|n| {
            f.runs
                .add_finished_run_with_results(window_start + Duration::seconds(n), 1)
        })
        .collect();
    let past_the_mark = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:10:00Z"), 1);

    f.service
        .reconcile_once(tenant())
        .await
        .expect("a budget-exhausted sweep is not an error");
    // Run 105 is past where one pass' budget reaches, so nothing but a
    // resuming pass can have ingested it.
    assert!(f.results_for(window[104]).await.is_empty());

    // The other replica's pass, older and shorter, writing last.
    let rewound = SweepCursor {
        window_floor: window_start,
        at: window_start + Duration::seconds(50),
        run_id: window[49],
    };
    f.write_sweep_cursor(Some(rewound)).await;
    assert_eq!(
        f.sweep_cursor().await,
        Some(rewound),
        "the cursor write must be last-writer-wins. A monotonic predicate here \
         would make this a second watermark and kill the lookback"
    );

    let second = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the resuming sweep succeeds");

    assert!(
        second.caught_up,
        "resuming from run 50 leaves 60 window runs plus one past the mark, \
         which is well inside one pass' budget"
    );
    assert_eq!(
        f.results_for(window[104]).await.len(),
        1,
        "a rewound cursor re-walks ground the other replica already covered; it \
         must not let a run between the two positions go unread"
    );
    assert_eq!(
        f.results_for(past_the_mark).await.len(),
        1,
        "and the walk still reaches past the mark"
    );
    assert_eq!(
        f.sweep_cursor().await,
        None,
        "the caught-up pass cleared it"
    );
}

/// **A replica wedge on mixed lookbacks still escalates**, and it is the wedge
/// `gear::stall_of`'s cursor test must not read as a drain.
///
/// Two replicas sweep one tenant — under `NoopLeaderElector` every replica is
/// the leader — carrying **different lookbacks**, which a rolling deploy that
/// changed `reconcile_lookback_seconds` produces for as long as it is rolling,
/// and a halted rollout produces indefinitely. Replica A's floor is 11:00,
/// replica B's 10:00.
///
/// This replaces `a_wedge_every_pass_reads_as_its_own_progress_still_escalates`,
/// whose fixture put every run inside *both* windows. Under the honour rule
/// (`first_page_of`: stored floor at or before this pass' floor, stored
/// position at or after it) that fixture converges — A honours B's cursor once
/// B's walk has passed 11:00 — so it no longer wedged, and a test that asserts
/// a wedge on a fixture that drains would be pinning a false alarm.
///
/// The wedge that *does* survive the rule is this one: B's older band
/// `[10:00, 11:00)` holds 120 runs, more than one pass' budget of 50 pages of
/// 2, so B's cursor always stands below A's floor. A discards it (`at < floor`)
/// and re-walks from 11:00; B discards A's (`window_floor` later than its own)
/// and re-walks from 10:00. Each pass does real work, the mark never moves,
/// the run past the mark is never reached — and **each replica's own ticker
/// sees the same cursor on every one of its passes**, because each replica
/// writes the same position every time. That constant cursor is what keeps
/// `stall_of`'s high-water test from reading this as a drain, and the stall
/// ladder below is driven per replica, exactly as each process' own
/// `report_reconcile_outcome` would drive it.
///
/// Finite and small: 231 runs, eight alternating ticks.
#[tokio::test]
async fn a_replica_wedge_on_mixed_lookbacks_still_escalates() {
    assert_eq!(
        super::MAX_PAGES_PER_SWEEP,
        50,
        "this fixture's 120-run older band is sized against a budget of 50 pages \
         of 2; re-derive the count by hand for the new budget rather than \
         computing it from the constant"
    );
    let page_size = 2_u32;
    // Replica A's lookback; the fixture's own service is A.
    let f = Fixture::with_lookback(Duration::hours(1), page_size).await;
    // Replica B's, mid-rollout. Its floor is 10:00 where A's is 11:00.
    let replica_b = f.sibling_with_lookback(Duration::hours(2), page_size);

    let mark = at("2026-08-18T12:00:00Z");
    f.set_watermark(mark).await;

    // 120 runs in `[10:00, 11:00)`: only B's window, and wider than B's budget.
    let older_band = at("2026-08-18T10:00:00Z");
    for n in 1..=120_i64 {
        f.runs
            .add_finished_run_with_results(older_band + Duration::seconds(20 * n), 0);
    }
    // 110 runs in `[11:00, 12:00]`: both windows, wider than A's budget.
    let window_start = at("2026-08-18T11:00:00Z");
    for n in 1..=110_i64 {
        f.runs
            .add_finished_run_with_results(window_start + Duration::seconds(n), 0);
    }
    let past_the_mark = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:10:00Z"), 1);

    // Eight alternating ticks, four per replica: one to record, three to reach
    // `WEDGED_PASSES_BEFORE_ERROR`. Finite.
    let mut by_replica: [Vec<ReconcileOutcome>; 2] = [Vec::new(), Vec::new()];
    for tick in 0..8 {
        let outcome = if tick % 2 == 0 {
            f.service.reconcile_once(tenant()).await
        } else {
            replica_b.reconcile_once(tenant()).await
        }
        .expect("neither replica errors; that is what makes this silent");
        by_replica[tick % 2].push(outcome);
    }

    for (replica, outcomes) in ["A", "B"].into_iter().zip(&by_replica) {
        for (n, outcome) in outcomes.iter().enumerate() {
            assert!(
                outcome.scanned > 0 && outcome.backfilled > 0,
                "replica {replica} pass {n} did real work: {outcome:?}"
            );
            assert!(
                !outcome.caught_up,
                "replica {replica} pass {n} stopped short"
            );
            assert_eq!(
                outcome.watermark_at,
                Some(mark),
                "replica {replica} pass {n} left the mark where it found it"
            );
        }
        // **The shape the high-water test reads.** Each replica re-walks from
        // its own floor every time, so it writes the same cursor every time.
        let cursors: Vec<_> = outcomes.iter().map(|o| o.sweep_cursor_at).collect();
        assert!(
            cursors[0].is_some() && cursors.iter().all(|at| *at == cursors[0]),
            "replica {replica} must leave the same cursor on every pass: got {cursors:?}"
        );
    }
    assert!(
        by_replica[1][0].sweep_cursor_at < Some(window_start),
        "premise: B's cursor stands below A's floor, which is why A discards it"
    );
    assert!(
        f.results_for(past_the_mark).await.is_empty(),
        "and the run past the window is never ingested: genuinely wedged"
    );

    // **And the real decision, on the real outcomes, per replica.** Each
    // process keeps its own `TenantProgress` and sees only its own passes, so
    // each replica's four outcomes are fed through `stall_of` and folded with
    // `TenantProgress::observe` exactly as its own `report_reconcile_outcome`
    // would: the first records, the next three must all be stalls — which is
    // `WEDGED_PASSES_BEFORE_ERROR`, the ERROR.
    for (replica, outcomes) in ["A", "B"].into_iter().zip(&by_replica) {
        let mut progress: Option<TenantProgress> = None;
        let mut stalls = Vec::new();
        for outcome in outcomes {
            stalls.push(stall_of(outcome, progress));
            progress
                .get_or_insert_with(TenantProgress::default)
                .observe(outcome);
        }
        assert_eq!(
            stalls,
            [
                None,
                Some(Stall::MarkStandingStill),
                Some(Stall::MarkStandingStill),
                Some(Stall::MarkStandingStill),
            ],
            "replica {replica}: a wedge in which every pass does real work must \
             still be counted as a stall on every pass after the first; a \
             cursor test that read this as a drain would answer `None` and \
             report the 2026-09-18 outage as health"
        );
    }
}

/// **A healthy drain moves the cursor forward, and so does not escalate.**
///
/// A large window draining and a wedge both show an unmoved mark with
/// `caught_up` false, and until finding #122's residual B both reached the
/// `ERROR`. What tells them apart is
/// [`ReconcileOutcome::sweep_cursor_at`](super::ReconcileOutcome::sweep_cursor_at):
/// **it moves forward on every pass of a drain**, and stands still on every
/// pass of the replica wedge above. `gear::stall_of` now reads it through
/// `TenantProgress::cursor_high_water`, so this drain is not a stall; the wedge
/// above still is.
///
/// 210 runs against a budget of 50 pages of 2, so there are two consecutive
/// *partial* passes to compare before the third catches up. Literal count,
/// constant pinned beside it.
#[tokio::test]
async fn a_draining_window_moves_the_cursor_the_operator_reads_forward() {
    assert_eq!(
        super::MAX_PAGES_PER_SWEEP,
        50,
        "this fixture's 210 runs are sized against a budget of 50 pages of 2, \
         so that two consecutive passes are both partial; re-derive the count \
         by hand for the new budget"
    );
    let page_size = 2_u32;
    let f = Fixture::with_lookback(Duration::hours(2), page_size).await;
    let mark = at("2026-08-18T12:00:00Z");
    f.set_watermark(mark).await;

    let window_start = at("2026-08-18T10:05:00Z");
    for n in 1..=210_i64 {
        f.runs
            .add_finished_run_with_results(window_start + Duration::seconds(n), 0);
    }
    let past_the_mark = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:10:00Z"), 1);

    let first = f.service.reconcile_once(tenant()).await.expect("pass one");
    let second = f.service.reconcile_once(tenant()).await.expect("pass two");

    // Both passes look identical to `stall_of`: unmoved mark, not caught up.
    for (n, outcome) in [&first, &second].into_iter().enumerate() {
        assert_eq!(outcome.watermark_at, Some(mark), "pass {n}");
        assert!(!outcome.caught_up, "pass {n}");
    }
    // What `report_reconcile_outcome` records after pass one, by the rule it
    // uses.
    let mut after_first = TenantProgress::default();
    after_first.observe(&first);
    assert_eq!(
        stall_of(&second, Some(after_first)),
        None,
        "a healthy drain is not a stall: its cursor rises past the high-water \
         under an unmoved mark. This asserted `MarkStandingStill` until finding \
         #122's residual B, when the drain was a known false positive"
    );

    // And here is what separates it from the wedge: the field the high-water
    // is built from, and the one the stall `WARN` and `ERROR` carry.
    let (Some(a), Some(b)) = (first.sweep_cursor_at, second.sweep_cursor_at) else {
        panic!(
            "both partial passes must report a cursor: {:?} then {:?}",
            first.sweep_cursor_at, second.sweep_cursor_at
        );
    };
    assert!(
        b > a,
        "the cursor an operator reads must move FORWARD between consecutive \
         passes of a drain; got {a} then {b}"
    );

    let third = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("pass three");
    assert!(third.caught_up, "and the window drains");
    assert_eq!(
        third.sweep_cursor_at, None,
        "the catching-up pass erases it, which is the operator's signal that \
         the drain is over"
    );
    assert_eq!(f.results_for(past_the_mark).await.len(), 1);
}

/// **The window the paging fix could only shout about, drained.**
///
/// `page_size` runs — more than `page_size`, here — sharing one identical
/// `finished_at`. Under the instant-only cursor this was the sweep's one
/// remaining un-walkable shape: the page's newest instant is that instant, so
/// the next page is the same page, the walk stops, and everything newer is
/// stranded for good. The sweep logged an `ERROR` and gave up, which was an
/// improvement on spinning and no help at all to the data.
///
/// The fixture is the real burst's shape at test scale: six runs on one
/// instant with a page size of four, so the tie group is strictly wider than a
/// page and cannot be drained by luck. (The 2026-09-18 burst held single
/// instants shared by 54, 40, 31 and 29 runs against a `page_size` of 200; the
/// ratio is what matters, not the size.)
///
/// Three assertions, and the third is the one the outage is about: every tied
/// run is consumed, the run *after* the tie group is projected, and the mark
/// lands past the whole group.
#[tokio::test]
async fn a_tie_group_larger_than_a_page_is_drained_rather_than_stranding_what_follows() {
    let f = Fixture::with_lookback(Duration::hours(1), 4).await;
    let tied = at("2026-08-18T12:00:00Z");
    let tie_group: Vec<_> = (0..6)
        .map(|_| f.runs.add_finished_run_with_results(tied, 1))
        .collect();
    let after_the_group = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:30:00Z"), 1);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(
        outcome.backfilled, 7,
        "six runs on one instant plus the one behind them, all in one pass"
    );
    for run in tie_group {
        assert_eq!(
            f.results_for(run).await.len(),
            1,
            "every run in a tie group wider than the page must be projected"
        );
    }
    assert_eq!(
        f.results_for(after_the_group).await.len(),
        1,
        "and so must the run behind it: under an instant-only cursor this one was \
         unreachable at this page size, by any number of passes"
    );
    assert_eq!(
        f.watermark().await,
        Some(at("2026-08-18T12:30:00Z")),
        "the mark must step over the whole tie group, not stop on it"
    );
    assert!(
        outcome.caught_up,
        "and the walk ended because qa-runs had nothing further, not because it \
         could not step"
    );
}

/// The tie group is walked **one page at a time**, not by quietly asking for a
/// bigger page.
///
/// The operational escape the old `ERROR` suggested was raising
/// `reconcile_page_size` above the group's size. This pins that the fix is the
/// cursor instead: seven runs at a page size of four is two full pages and a
/// short one, so three listings — and a sweep that had started ignoring
/// `page_size` would show one.
#[tokio::test]
async fn draining_a_tie_group_still_respects_the_page_size() {
    let f = Fixture::with_lookback(Duration::hours(1), 4).await;
    let tied = at("2026-08-18T12:00:00Z");
    for _ in 0..6 {
        f.runs.add_finished_run_with_results(tied, 1);
    }
    f.runs
        .add_finished_run_with_results(at("2026-08-18T12:30:00Z"), 1);

    f.service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(
        f.runs.listings(),
        2,
        "seven runs at a page size of four is one full page and a short second that \
         ends the walk, and no run is listed twice, because a keyset resume is strict"
    );
}

/// **The stop-at-the-gap rule, and the only test that can see it.** The page is
/// oldest-first; a failure in the middle must stop the sweep rather than skip
/// the run and carry on.
///
/// A sweep that skipped the failure would backfill the third run, advance the
/// watermark to *its* instant, and put the second run permanently behind the
/// next sweep's floor. The assertion that matters is the watermark: it must sit
/// on the first run, not the third.
#[tokio::test]
async fn a_gap_in_the_middle_of_a_page_stops_the_sweep_there() {
    let f = Fixture::new().await;
    let first = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 1);
    let broken = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T11:00:00Z"), 1);
    let after = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T12:00:00Z"), 1);
    f.runs.fail_result_reads_for(broken);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(
        f.results_for(first).await.len(),
        1,
        "the run before the gap lands"
    );
    assert!(f.results_for(broken).await.is_empty());
    assert!(
        f.results_for(after).await.is_empty(),
        "the sweep must not jump the gap"
    );
    assert_eq!(
        f.watermark().await,
        Some(at("2026-08-18T10:00:00Z")),
        "the watermark stops on the last run before the gap, not on the newest in the page"
    );
    assert!(outcome.stopped_at_gap);
    assert_eq!(
        outcome.stopped_at_run,
        Some(broken),
        "the outcome must name the run that wedged the sweep, not just that one did: it is \
         the only thing an operator can act on and the only thing Task 40's alert can carry"
    );
    assert_eq!(outcome.backfilled, 1);
}

/// The newest run in a page must not be re-projected on every sweep.
///
/// `ingested_run_ids_between`'s window is half-open, and the newest run's
/// `finished_at` *is* the upper bound the plan specified — so the naive
/// `[floor, page_max)` probe reports it as missing forever. The slack on the
/// upper bound is what fixes it, and this is the test that fails without it.
#[tokio::test]
async fn the_newest_run_in_a_page_is_not_rebackfilled_on_the_next_sweep() {
    let f = Fixture::new().await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 2);

    let first = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");
    assert_eq!(first.backfilled, 1);

    let second = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(
        second.backfilled, 0,
        "the run is already ingested; a second backfill means the diff's upper bound \
         excluded the newest run in the page"
    );
    assert_eq!(
        second.scanned, 1,
        "it is still examined, just not re-projected"
    );
}

/// A second sweep over the same window is a no-op, which is what makes running
/// this on a 300-second ticker cheap. Together with the test above this pins
/// both halves: the diff finds it, and re-running changes nothing.
#[tokio::test]
async fn a_second_sweep_over_the_same_window_backfills_nothing_and_changes_nothing() {
    let f = Fixture::new().await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 3);

    f.service.reconcile_once(tenant()).await.unwrap();
    let after_first: Vec<_> = f
        .results_for(run)
        .await
        .into_iter()
        .map(|r| (r.test_file, r.test_name, r.status))
        .collect();
    let reads_after_first = f.runs.result_reads();

    f.service.reconcile_once(tenant()).await.unwrap();
    let after_second: Vec<_> = f
        .results_for(run)
        .await
        .into_iter()
        .map(|r| (r.test_file, r.test_name, r.status))
        .collect();

    assert_eq!(after_first, after_second);
    assert_eq!(after_first.len(), 3);
    assert_eq!(
        f.runs.result_reads(),
        reads_after_first,
        "an already-ingested run must not be re-read from qa-runs at all"
    );
}

/// A sweep pages forward until it catches up, and *both* halves of that are
/// properties under test: it catches up **in one pass**, and the work is still
/// bounded — by `MAX_PAGES_PER_SWEEP` pages rather than by a single one.
///
/// **This test used to assert the opposite**, and the assertion it used to make
/// was the bug. It read `scanned == 2` ("the page size is the bound") and
/// expected four ticks to walk five runs, on the reasoning that one page per
/// tick is what keeps the first-ever sweep — whose floor is the epoch — from
/// reading all of history at once. The bound was real; stopping *unconditionally*
/// after one page was not, because the floor is `mark - lookback` and so a page
/// that fills up before it reaches the mark leaves the mark exactly where it
/// was. See [`super::ReconcileService::sweep`] for the three days of silent
/// stall that followed from it, and
/// `a_lookback_window_holding_a_full_page_does_not_pin_the_watermark` for the
/// direct reproduction.
///
/// The first-ever-sweep concern the old assertion was protecting is still
/// protected, and still by `page_size` — just at
/// `MAX_PAGES_PER_SWEEP * page_size` per tick instead of `page_size`, which the
/// listing count below pins as a paged walk rather than one unbounded read.
#[tokio::test]
async fn a_sweep_pages_forward_until_it_catches_up() {
    let f = Fixture::with_lookback(Duration::ZERO, 2).await;
    for hour in 10..15 {
        f.runs
            .add_finished_run_with_results(at(&format!("2026-08-18T{hour}:00:00Z")), 1);
    }

    let first = f.service.reconcile_once(tenant()).await.unwrap();

    assert_eq!(first.scanned, 5, "every run behind the mark, in one pass");
    assert_eq!(first.backfilled, 5);
    assert_eq!(
        f.watermark().await,
        Some(at("2026-08-18T14:00:00Z")),
        "the mark lands on the newest run the pass accounted for"
    );
    assert_eq!(
        f.runs.listings(),
        3,
        "a paged walk, not one unbounded read: two full pages of two and a short \
         third that ends it. This was FIVE while the cursor was a bare instant and \
         the port's bound was inclusive, so every page re-read its predecessor's \
         last run; a keyset resume is strict, so it does not"
    );

    // Caught up. The next tick is one short page that moves nothing.
    let second = f.service.reconcile_once(tenant()).await.unwrap();
    assert_eq!(
        second.backfilled, 0,
        "the second sweep re-examines the watermark run (inclusive lower bound) and \
         finds it already ingested"
    );
    assert_eq!(f.watermark().await, Some(at("2026-08-18T14:00:00Z")));
}

/// An empty window is a successful no-op that moves nothing. The ticker runs
/// this every 300 seconds on a system where nothing has finished, so it is the
/// common case.
#[tokio::test]
async fn an_empty_window_is_a_no_op() {
    let f = Fixture::new().await;

    let outcome = f.service.reconcile_once(tenant()).await.unwrap();

    assert_eq!(
        outcome,
        ReconcileOutcome {
            // **Not `default()`.** An empty listing is the sweep having read
            // everything there is, and `caught_up` is the field
            // `crate::gear::report_reconcile_outcome` uses to tell that from a
            // walk that stopped with more to read. A sweep over an empty window
            // reporting `caught_up: false` would put every idle tenant one
            // comparison away from the stall alarm.
            caught_up: true,
            ..ReconcileOutcome::default()
        }
    );
    assert_eq!(f.watermark().await, None, "no runs means no mark");
}

/// qa-runs being unreachable is an **error**, not a quiet no-op. The sweep did
/// nothing and the caller has to be able to tell that apart from "nothing to
/// do" — otherwise a total qa-runs outage looks exactly like a healthy idle
/// system.
#[tokio::test]
async fn a_listing_failure_is_an_error_rather_than_an_empty_sweep() {
    let f = Fixture::new().await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 1);
    f.runs.fail_listing(true);

    let err = f
        .service
        .reconcile_once(tenant())
        .await
        .expect_err("an unreachable qa-runs is an error");

    assert!(matches!(err, DomainError::Internal(_)));
    assert_eq!(f.watermark().await, None);
}

/// **#123.** A listing that fails on a *later* page must not throw away the
/// pages before it.
///
/// The sweep walks forward across pages, and each page it consumes is real
/// work: rows written, runs accounted for. Until this test the walk propagated
/// a page-three failure with `?`, which returned **before** the single
/// `advance_watermark` at the end of the sweep — so pages one and two were
/// written to `qa_test_results`, the mark stayed where it began, and the next
/// tick re-walked exactly the same ground. For as long as the failure lasted
/// (a qa-runs outage is measured in minutes at best) every tick did the same
/// two pages of work and kept none of it, and the pass after the failure
/// cleared started again from the original floor.
///
/// This is the base's shape only because the base had one page. The walk that
/// replaced it introduced the discard, which is why this is one of the three
/// findings in the reconcile cluster that are this project's own.
///
/// Five runs at a page size of two is two full pages and a third listing;
/// failing that third is a failure with two pages' worth of progress already
/// standing behind it.
#[tokio::test]
async fn an_error_on_a_later_page_keeps_the_progress_already_earned() {
    let f = Fixture::with_lookback(Duration::ZERO, 2).await;
    for hour in 10..15 {
        f.runs
            .add_finished_run_with_results(at(&format!("2026-08-18T{hour}:00:00Z")), 1);
    }
    f.runs.fail_listing_on_page(3);

    let err = f
        .service
        .reconcile_once(tenant())
        .await
        .expect_err("the third listing fails");

    assert!(
        matches!(err, DomainError::Internal(_)),
        "the failure is still propagated, not swallowed: {err:?}"
    );
    assert_eq!(
        f.watermark().await,
        Some(at("2026-08-18T13:00:00Z")),
        "pages one and two were consumed and their rows written; the mark must \
         carry that progress so the next tick resumes rather than repeating it"
    );
    assert_eq!(
        f.runs.listings(),
        3,
        "premise: two full pages and the failing third"
    );
}

/// A listing that fails on the **first** page still moves nothing, which is the
/// half of #123 that must not regress while the other half is fixed.
///
/// `a_listing_failure_is_an_error_rather_than_an_empty_sweep` above asserts the
/// error; this asserts the mark, at the one boundary where "keep the progress
/// you earned" has no progress to keep. A sweep that advanced here would be
/// advancing past runs it never read.
#[tokio::test]
async fn an_error_on_the_first_page_moves_nothing() {
    let f = Fixture::with_lookback(Duration::ZERO, 2).await;
    for hour in 10..15 {
        f.runs
            .add_finished_run_with_results(at(&format!("2026-08-18T{hour}:00:00Z")), 1);
    }
    f.runs.fail_listing_on_page(1);

    f.service
        .reconcile_once(tenant())
        .await
        .expect_err("the first listing fails");

    assert_eq!(
        f.watermark().await,
        None,
        "nothing was consumed, so there is nothing to record"
    );
}

/// A backfilled run's rows carry the run's denormalized columns — because the
/// sweep's write is `IngestService::write_run_projection`, the same function
/// the rebuild endpoint uses and the only one either ever has.
///
/// The property this defends is the one that would only break after the sweep
/// and the rebuild diverged into two separate projections: a divergence like
/// that would be invisible until an operator reached for a rebuild and got a
/// different answer than the sweep had already written.
#[tokio::test]
async fn the_backfill_writes_the_same_denormalized_columns_the_event_path_would() {
    let f = Fixture::new().await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 1);

    f.service.reconcile_once(tenant()).await.unwrap();

    let rows = f.results_for(run).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].product_version.as_deref(), Some("9.1.0"));
    assert_eq!(rows[0].app_build.as_deref(), Some("9.1.0-4412"));
    assert_eq!(rows[0].branch.as_deref(), Some("main"));
    assert_eq!(rows[0].plan_path.as_deref(), Some("plans/smoke.yaml"));
    assert_eq!(rows[0].run_finished_at, Some(at("2026-08-18T10:00:00Z")));
}

/// Another tenant's sweep cannot see, and cannot write, this tenant's rows.
///
/// The backfill scopes on the tenant it was handed, and `AccessScope::for_tenant`
/// is what puts that in the `WHERE` clause. A sweep that leaked would be a
/// cross-tenant write on a background path nobody is watching.
#[tokio::test]
async fn a_sweep_for_one_tenant_writes_nothing_visible_to_another() {
    let f = Fixture::new().await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 2);

    f.service.reconcile_once(tenant()).await.unwrap();

    let other = Uuid::from_u128(0x0B);
    let conn = f.db.conn().unwrap();
    let seen = OrmResultsRepository
        .list_by_run(&conn, &scope(other), run)
        .await
        .unwrap();

    assert!(seen.is_empty(), "the other tenant must see nothing");
    assert_eq!(f.results_for(run).await.len(), 2);
}

// ---------------------------------------------------------------------------
// The reprojection held a transaction across a cross-gear read
// ---------------------------------------------------------------------------

/// **Pins the mechanism, independent of the fix.** `FakeRuns::trip_guard_via`
/// exists to be indistinguishable from the real local client's own read
/// (`qa-runs/src/domain/service/runs.rs:321`, `self.db.conn()?`) — this test
/// is the evidence, and it is unaffected by whether `reconcile.rs`'s ordering
/// bug is fixed: it drives the double directly, against a transaction it
/// opens itself, and checks the *exact* message
/// `toolkit_db`'s transaction-bypass guard produces
/// (`libs/toolkit-db/src/secure/db.rs:10-21`,
/// `DbError::ConnRequestedInsideTx`). Task 5a's brief, Step 2, asks for
/// exactly this: "confirm it fails with the guard error rather than
/// something incidental."
#[tokio::test]
async fn trip_guard_via_reproduces_the_transaction_bypass_guards_own_message() {
    let db = inmem_db().await;
    let runs = FakeRuns::default();
    runs.trip_guard_via(db.clone());
    let run_id = runs.add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);
    let provider = DBProvider::<DomainError>::new(db);

    let err = provider
        .transaction(move |_tx| {
            Box::pin(async move {
                runs.get_run(&crate::domain::service::test_support::ctx(TENANT), run_id)
                    .await
            })
        })
        .await
        .expect_err("a nested db.conn() inside an open transaction must trip the guard");

    let message = err.to_string();
    assert!(
        message.contains("Cannot create non-transactional connection inside an active transaction"),
        "expected the transaction-bypass guard's own message, got: {message}"
    );
}

/// **Task 5a's failing test.** Before the fix, `reproject()` opened its own
/// transaction and called `IngestService::reproject_run` *inside* it, and
/// that function's first act is a cross-gear read into qa-runs. In a
/// single-binary deployment the qa-runs client `ClientHub` resolves is
/// qa-runs' own in-process service, and its `get`
/// (`qa-runs/src/domain/service/runs.rs:321`) calls **qa-runs' own**
/// `db.conn()` — a different `Db` instance than this gear's, but
/// `toolkit_db`'s transaction-bypass guard is task-local rather than
/// instance-local (`libs/toolkit-db/src/secure/db.rs:10-21`), so that nested
/// call failed with `DbError::ConnRequestedInsideTx` on every run, in every
/// sweep and every rebuild — deterministically, matching production's
/// `{"scanned":2,"replayed":0,"stopped_at_gap":true}`.
///
/// Before the fix this assertion fails with `backfilled: 0` and
/// `stopped_at_gap: true`, from exactly the error the test above pins — not
/// from a listing failure, a denial, or anything else this fixture could
/// produce, since nothing else here is configured to fail.
///
/// `FakeRuns::trip_guard_via` is what makes the double reproduce that nested
/// `db.conn()` rather than answer from memory: every other test in this file
/// — and every test that predates this one — leaves it unset and cannot see
/// this bug, which is exactly why it shipped invisibly.
#[tokio::test]
async fn a_rebuild_backfills_every_run_even_when_qa_runs_reads_would_trip_the_transaction_guard() {
    let f = Fixture::new().await;
    let first = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);
    let second = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:30:00Z"), 1);
    f.runs.trip_guard_via(f.db.clone());

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(outcome.scanned, 2);
    assert_eq!(outcome.backfilled, 2);
    assert!(!outcome.stopped_at_gap, "{outcome:?}");
    assert_eq!(f.results_for(first).await.len(), 1);
    assert_eq!(f.results_for(second).await.len(), 1);
}

/// The same property against `sweep()` — Task 5a's brief calls this out
/// separately: `sweep()` shares [`ReconcileService::reproject`] with
/// `rebuild()`, so it has been failing identically on every tick since boot,
/// in this and every other single-binary deployment, and nothing surfaced it
/// because the failure is logged and swallowed per-run
/// (`ReconcileOutcome::stopped_at_gap`, never propagated as an error the
/// ticker would notice).
#[tokio::test]
async fn a_sweep_backfills_a_run_even_when_qa_runs_reads_would_trip_the_transaction_guard() {
    let f = Fixture::new().await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 1);
    f.runs.trip_guard_via(f.db.clone());

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the sweep itself succeeds; a per-run gap is an outcome, not an error");

    assert_eq!(outcome.backfilled, 1, "{outcome:?}");
    assert!(!outcome.stopped_at_gap, "{outcome:?}");
    assert_eq!(f.results_for(run).await.len(), 1);
}

// ---------------------------------------------------------------------------
// The operator rebuild
// ---------------------------------------------------------------------------

/// A rebuild replays a closed time range regardless of the watermark, and
/// leaves the watermark alone: it is an operator tool for a known-bad window,
/// not a reset.
///
/// **This is the falsifiable form of "does not read and does not write the
/// watermark", and both halves are load-bearing.** The run sits at 09:00 with
/// the mark at 20:00, so a rebuild that derived its floor from the mark the way
/// [`ReconcileService::reconcile_once`] does would compute 19:00 and replay
/// nothing; and the final assertion is what stops the mark being written.
#[tokio::test]
async fn a_rebuild_replays_the_range_without_touching_the_watermark() {
    let f = Fixture::new().await;
    f.set_watermark(at("2026-08-18T20:00:00Z")).await;
    let old = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 2);

    f.service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(f.results_for(old).await.len(), 2);
    assert_eq!(f.watermark().await, Some(at("2026-08-18T20:00:00Z")));
}

/// The same property against an *unset* mark, which is the other way the
/// previous test could pass for the wrong reason: a rebuild that wrote the
/// watermark like the sweep does would leave one behind here.
#[tokio::test]
async fn a_rebuild_never_creates_a_watermark() {
    let f = Fixture::new().await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(f.watermark().await, None, "a rebuild writes no mark at all");
    assert_eq!(
        outcome.watermark_at, None,
        "`None` here is the endpoint's contract, not a placeholder"
    );
    assert_eq!(outcome.backfilled, 1);
}

/// **A rebuild re-projects a run that is already projected, and the sweep does
/// not.** That difference is the entire reason the endpoint exists: the window
/// is known-bad, so "already ingested" is not evidence of anything.
///
/// Observed through `FakeRuns::result_reads`, because the rows themselves are
/// identical either way — an idempotent rewrite is indistinguishable from a skip
/// by looking at the table.
#[tokio::test]
async fn a_rebuild_re_reads_a_run_the_sweep_would_have_skipped() {
    let f = Fixture::new().await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 2);

    f.service.reconcile_once(tenant()).await.unwrap();
    let reads_after_sweep = f.runs.result_reads();

    // A second sweep is a no-op — `a_second_sweep_over_the_same_window_...`
    // pins that. This is the contrast.
    f.service.reconcile_once(tenant()).await.unwrap();
    assert_eq!(
        f.runs.result_reads(),
        reads_after_sweep,
        "the sweep skips an already-ingested run"
    );

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(
        f.runs.result_reads(),
        reads_after_sweep + 1,
        "the rebuild re-reads it: an existing projection is what it is there to replace"
    );
    assert_eq!(outcome.backfilled, 1);
    assert_eq!(f.results_for(run).await.len(), 2);
}

/// The window is `[from, to)`. A run sitting exactly on the upper bound belongs
/// to the *next* window, so an operator splitting a bad day at one instant
/// replays every run once rather than replaying the boundary run twice.
#[tokio::test]
async fn the_rebuild_window_excludes_a_run_sitting_on_the_upper_bound() {
    let f = Fixture::new().await;
    let inside = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);
    let on_bound = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 1);

    let first = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(f.results_for(inside).await.len(), 1);
    assert!(
        f.results_for(on_bound).await.is_empty(),
        "the upper bound is exclusive, matching ingested_run_ids_between's window"
    );
    assert_eq!(first.scanned, 1);

    // The adjoining window picks it up, which is the property half-openness
    // buys: two windows sharing an endpoint neither skip a run nor replay one.
    let second = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T10:00:00Z"),
            at("2026-08-18T12:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(f.results_for(on_bound).await.len(), 1);
    assert_eq!(second.scanned, 1, "and it is replayed exactly once");
}

/// **The no-truncate constraint.** A rebuild deletes nothing it does not
/// immediately rewrite, so rows outside its window survive it.
///
/// This is carried obligation 4 out of Task 15: leader election in this gear is
/// an optimisation, not mutual exclusion, and a range delete would have made
/// election a correctness requirement.
#[tokio::test]
async fn a_rebuild_leaves_rows_outside_its_window_untouched() {
    let f = Fixture::new().await;
    let inside = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 2);
    let outside = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T15:00:00Z"), 3);

    // Both land first, through the sweep.
    f.service.reconcile_once(tenant()).await.unwrap();
    assert_eq!(f.results_for(outside).await.len(), 3);

    f.service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(f.results_for(inside).await.len(), 2);
    assert_eq!(
        f.results_for(outside).await.len(),
        3,
        "a rebuild is not a range delete"
    );
}

/// The other half of the same constraint: an **empty** window deletes nothing.
///
/// A range-truncating implementation would pass the test above (its window holds
/// a run it rewrites) and fail this one, so this is the case that separates them.
#[tokio::test]
async fn a_rebuild_over_an_empty_window_deletes_nothing() {
    let f = Fixture::new().await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 2);
    f.service.reconcile_once(tenant()).await.unwrap();

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T11:00:00Z"),
            at("2026-08-18T12:00:00Z"),
        )
        .await
        .expect("an empty window is a successful no-op");

    assert_eq!(
        outcome,
        ReconcileOutcome {
            // **Not `default()`.** An empty window is a window the rebuild
            // walked to its end, and `caught_up` — `complete` on the wire — is
            // the field an operator branches on. A no-op that reported itself
            // incomplete would send them round the resume loop forever.
            caught_up: true,
            ..ReconcileOutcome::default()
        }
    );
    assert_eq!(
        f.results_for(run).await.len(),
        2,
        "nothing was in the window, so nothing may be removed from it"
    );
}

/// **The action string is a security surface, and this is its only witness.**
///
/// Nothing else in this crate fails if `actions::REBUILD` is replaced by
/// `actions::GET` or the resource type by another gear's, and no test in this
/// workspace evaluates a real policy. Pinned against independent literals rather
/// than against the constants, so a typo inside the constant is visible instead
/// of tautological.
#[tokio::test]
async fn a_rebuild_asks_the_pdp_for_rebuild_on_the_test_result_resource() {
    let authz = Arc::new(RecordingAuthZ::default());
    let f = Fixture::with_authz(Duration::hours(1), 200, authz.clone()).await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);

    f.service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(
        authz.asked(),
        vec![(
            format!("{GTS_ID_PREFIX}cf.qa.insights.test_result.v1~"),
            "rebuild".to_owned()
        )],
        "one decision, for the action actually being performed"
    );
}

/// A PDP deny is [`DomainError::Forbidden`], and nothing is written.
///
/// The ordering matters as much as the verdict: the scope is compiled **before**
/// qa-runs is listed, so a denied caller cannot use this endpoint to learn which
/// runs exist in a window.
#[tokio::test]
async fn a_denied_rebuild_is_forbidden_and_reads_nothing() {
    let f = Fixture::with_authz(Duration::hours(1), 200, Arc::new(DenyAllAuthZ)).await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);

    let err = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect_err("a denied caller may not rebuild");

    assert!(matches!(err, DomainError::Forbidden));
    assert!(f.results_for(run).await.is_empty());
    assert_eq!(
        f.runs.listings(),
        0,
        "the PDP is asked before qa-runs is listed, so a denied caller cannot use this \
         endpoint to learn which runs exist in a window"
    );
}

/// A caller carrying the nil tenant is the platform-root sentinel, and there is
/// no cross-tenant rebuild. Refused before the PDP is asked and before anything
/// is read.
#[tokio::test]
async fn a_caller_with_no_tenant_cannot_rebuild() {
    let f = Fixture::new().await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);
    let rootish = crate::domain::service::test_support::ctx(Uuid::nil());

    let err = f
        .service
        .rebuild(
            &rootish,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect_err("the nil tenant is the platform-root sentinel");

    assert!(matches!(err, DomainError::Forbidden));
    assert_eq!(
        f.runs.listings(),
        0,
        "refused before anything is read, and before the PDP is even asked"
    );
}

/// An inverted window, and an empty-by-equality one, are both refused rather
/// than silently doing nothing. Under a half-open upper bound `from == to`
/// cannot match a run, so an operator who typed one instant twice made a mistake
/// worth answering.
#[tokio::test]
async fn a_window_that_is_not_strictly_forward_is_refused() {
    let f = Fixture::new().await;

    for (from, to) in [
        ("2026-08-18T10:00:00Z", "2026-08-18T08:00:00Z"),
        ("2026-08-18T10:00:00Z", "2026-08-18T10:00:00Z"),
    ] {
        let err = f
            .service
            .rebuild(&f.ctx, at(from), at(to))
            .await
            .expect_err("a window must be strictly forward");
        assert!(
            matches!(&err, DomainError::Validation { field, .. } if field == "to"),
            "got {err:?}"
        );
    }
    assert_eq!(
        f.runs.listings(),
        0,
        "a refused window is refused before qa-runs is touched"
    );
}

/// A run the rebuild cannot re-project stops it, and says so. Re-running the
/// same window once qa-runs recovers is idempotent and total, so stopping loses
/// nothing — and it hands the operator one unambiguous signal instead of a
/// partial success to reason about.
#[tokio::test]
async fn a_failure_part_way_through_stops_the_rebuild() {
    let f = Fixture::new().await;
    let first = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);
    let broken = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:30:00Z"), 1);
    let after = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:45:00Z"), 1);
    f.runs.fail_result_reads_for(broken);

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("a per-run failure is an outcome, not an error");

    assert_eq!(f.results_for(first).await.len(), 1);
    assert!(f.results_for(broken).await.is_empty());
    assert!(
        f.results_for(after).await.is_empty(),
        "the rebuild stops rather than reporting a partial success"
    );
    assert!(outcome.stopped_at_gap);
    assert_eq!(
        outcome.stopped_at_run,
        Some(broken),
        "the rebuild reports which run it stopped on, the same as the sweep"
    );
    assert_eq!(outcome.backfilled, 1);
    assert_eq!(outcome.scanned, 3);
    assert!(
        !outcome.caught_up,
        "a rebuild that stopped on a run did not reach the end of its window"
    );
    assert_eq!(
        outcome.resume_from,
        Some(at("2026-08-18T09:00:00Z")),
        "the last run consumed, so the continuation reaches the failure again rather than \
         stepping over it"
    );
}

/// qa-runs being unreachable is an error, exactly as it is for the sweep: an
/// operator must be able to tell "the window held nothing" from "I could not
/// look".
#[tokio::test]
async fn a_listing_failure_makes_the_rebuild_an_error() {
    let f = Fixture::new().await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);
    f.runs.fail_listing(true);

    let err = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect_err("an unreachable qa-runs is an error");

    assert!(matches!(err, DomainError::Internal(_)));
}

/// **`page_size` is the size of a page, not the size of the job.** A window
/// holding more runs than one page is replayed in full, in pages, and says it
/// is complete.
///
/// This test shipped inverted — as
/// `the_rebuild_window_is_bounded_by_the_page_size`, asserting `scanned == 2`
/// out of four runs — and it was pinning the last page boundary in this module
/// that was a wall. The old remedy was a `WARN` telling the operator to narrow
/// the window; nothing in the response said the job was half done.
#[tokio::test]
async fn a_rebuild_window_wider_than_a_page_is_replayed_in_full() {
    let f = Fixture::with_lookback(Duration::ZERO, 2).await;
    let runs: Vec<_> = [0, 10, 20, 30]
        .into_iter()
        .map(|minute| {
            f.runs
                .add_finished_run_with_results(at(&format!("2026-08-18T09:{minute:02}:00Z")), 1)
        })
        .collect();

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(outcome.scanned, 4, "two full pages, not the first one");
    assert_eq!(outcome.backfilled, 4);
    for run in runs {
        assert_eq!(f.results_for(run).await.len(), 1);
    }
    assert!(outcome.caught_up, "the whole window was reached");
    assert_eq!(
        outcome.resume_from, None,
        "nothing left to resume from once the window is walked out"
    );
}

/// **The same wall the sweep's fix removed, on the endpoint an operator reaches
/// for when the sweep has already let them down.**
///
/// `page_size` runs sharing one `finished_at` was not merely a truncation here:
/// it was a region of history the endpoint could not reach *by any window an
/// operator could type*. Narrowing the window to that single instant returns
/// the same full page, so the advice the old `WARN` gave — "narrow the window
/// and repeat" — was unfollowable, and the runs behind the group were
/// unreachable for good. The 2026-09-18 burst contained single instants shared
/// by 54, 40, 31 and 29 runs.
///
/// Six runs on one instant at a page size of four, plus the run behind them.
#[tokio::test]
async fn a_rebuild_drains_a_tie_group_wider_than_its_page() {
    let f = Fixture::with_lookback(Duration::ZERO, 4).await;
    let tied = at("2026-08-18T09:00:00Z");
    let tie_group: Vec<_> = (0..6)
        .map(|_| f.runs.add_finished_run_with_results(tied, 1))
        .collect();
    let after_the_group = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:30:00Z"), 1);

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(outcome.scanned, 7);
    assert_eq!(outcome.backfilled, 7);
    for run in tie_group {
        assert_eq!(
            f.results_for(run).await.len(),
            1,
            "every run of a tie group wider than the page must be replayed"
        );
    }
    assert_eq!(
        f.results_for(after_the_group).await.len(),
        1,
        "and so must the run behind it: under the one-page rebuild this run was \
         unreachable at this page size, by any window"
    );
    assert!(outcome.caught_up);
}

/// The cap that remains is a **budget**, and spending it is in the answer
/// rather than in a log line.
///
/// A page size of one against `MAX_PAGES_PER_REBUILD` pages puts the ceiling at
/// ten runs; eleven runs in the window is one past it. The assertions that
/// matter are the last two: a truncated rebuild must not report itself
/// complete, and it must hand back the instant to continue from — the outage
/// established that a `WARN` nobody reads is indistinguishable from success.
#[tokio::test]
async fn a_rebuild_that_spends_its_page_budget_says_so_in_the_outcome() {
    let f = Fixture::with_lookback(Duration::ZERO, 1).await;
    for minute in 0..11 {
        f.runs
            .add_finished_run_with_results(at(&format!("2026-08-18T09:{minute:02}:00Z")), 1);
    }

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert_eq!(outcome.scanned, 10, "ten pages of one run each");
    assert_eq!(outcome.backfilled, 10);
    assert!(
        !outcome.caught_up,
        "the eleventh run was never reached, and the outcome must not claim otherwise"
    );
    assert_eq!(
        outcome.resume_from,
        Some(at("2026-08-18T09:09:00Z")),
        "the last run consumed, inclusive: posting it back as `from` re-replays that run \
         and reaches the eleventh"
    );
    assert!(!outcome.stopped_at_gap, "a budget is not a gap");

    // And the continuation finishes the job, which is what makes the field an
    // instruction rather than a diagnostic.
    let rest = f
        .service
        .rebuild(
            &f.ctx,
            outcome.resume_from.expect("a truncated pass resumes"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    assert!(rest.caught_up);
    assert_eq!(rest.resume_from, None);
}

/// **A listing that fails after the first page is a partial answer, not an
/// error** — finding #122's residual D.
///
/// Pages before the failure were replayed and their rows written. Answering
/// `Err` threw away the one thing the operator needs to continue — where to
/// resume — and made them re-replay the whole window. The rebuild now answers
/// `Ok` with `caught_up = false` and `resume_from` on the last run it fully
/// consumed, exactly as when it spends its page budget. A failure on page
/// one still observed nothing and is still an error
/// (`a_listing_failure_makes_the_rebuild_an_error`).
///
/// Five runs at a page size of two: page one replays 09:00 and 09:10, page two
/// fails.
#[tokio::test]
async fn a_rebuild_error_after_the_first_page_reports_where_to_resume() {
    let f = Fixture::with_lookback(Duration::ZERO, 2).await;
    let runs: Vec<Uuid> = [0, 10, 20, 30, 40]
        .into_iter()
        .map(|minute| {
            f.runs
                .add_finished_run_with_results(at(&format!("2026-08-18T09:{minute:02}:00Z")), 1)
        })
        .collect();
    f.runs.fail_listing_on_page(2);

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("a failure after page one is a partial answer, not an error");

    assert!(!outcome.caught_up, "the window was not walked out");
    assert_eq!(
        outcome.resume_from,
        Some(at("2026-08-18T09:10:00Z")),
        "the second run's instant: the last position page one fully consumed"
    );
    assert_eq!(outcome.scanned, 2, "page one's counts are kept");
    assert_eq!(f.results_for(runs[1]).await.len(), 1);
    assert!(f.results_for(runs[2]).await.is_empty());
    assert_eq!(
        f.runs.listings(),
        2,
        "premise: page one, then the failing page two"
    );

    // The continuation finishes the job, which is what makes it an answer.
    let rest = f
        .service
        .rebuild(
            &f.ctx,
            outcome.resume_from.expect("a partial rebuild resumes"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("qa-runs is back");
    assert!(rest.caught_up);
    for run in runs {
        assert_eq!(f.results_for(run).await.len(), 1);
    }
}

/// The PDP-compiled scope is applied against the real database, not merely
/// obtained: another tenant sees nothing a rebuild wrote.
///
/// [`TenantScopedAuthZ`] returns a real `owner_tenant_id IN [tenant]`
/// constraint, so a rebuild that ignored the compiled scope — or compiled one
/// for the wrong tenant — fails here rather than being absorbed by a mock.
#[tokio::test]
async fn a_rebuild_for_one_tenant_writes_nothing_visible_to_another() {
    let f = Fixture::new().await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 2);

    f.service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect("rebuild succeeds");

    let other = Uuid::from_u128(0x0B);
    let conn = f.db.conn().unwrap();
    let seen = OrmResultsRepository
        .list_by_run(&conn, &scope(other), run)
        .await
        .unwrap();

    assert!(seen.is_empty(), "the other tenant must see nothing");
    assert_eq!(f.results_for(run).await.len(), 2);
}

/// **The fail-open this guard exists for, falsified from the damaging side.**
///
/// `upsert_run_results` filters its per-run `DELETE`s with the full compiled
/// scope and inserts through `scope_unchecked`, a documented no-op
/// (`libs/toolkit-db/src/secure/db_ops.rs:376-385`), and the only guard between
/// them — `validate_tenant_in_scope` — checks `owner_tenant_id` alone. So under a
/// scope carrying a `resource_id` predicate the delete matches nothing while the
/// insert writes the full set.
///
/// Without `refuse_scope_beyond_tenant` this test does not merely fail to refuse:
/// the run ends up with **four** rows instead of two, silently double-counting in
/// every dashboard and analytics read Tasks 18-27 build on top. Measured by
/// removing the guard: `assertion left == right failed: left: 4, right: 2`.
///
/// The row count is therefore the assertion that matters, not the error type.
#[tokio::test]
async fn a_scope_carrying_more_than_the_tenant_is_refused_and_writes_nothing() {
    let f = Fixture::with_authz(Duration::hours(1), 200, Arc::new(ResourceConstrainedAuthZ)).await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 2);

    // Land the run first, through the sweep, which scopes with
    // `AccessScope::for_tenant` and never consults the PDP.
    f.service.reconcile_once(tenant()).await.unwrap();
    assert_eq!(f.results_for(run).await.len(), 2);
    let listings_after_sweep = f.runs.listings();

    let err = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await
        .expect_err("a scope this write path cannot execute must be refused");

    assert!(
        matches!(err, DomainError::UnsupportedScope { resource }
            if resource == format!("{GTS_ID_PREFIX}cf.qa.insights.test_result.v1~")),
        "got {err:?}"
    );
    assert_eq!(
        f.results_for(run).await.len(),
        2,
        "the projection must be untouched; 4 rows here is the duplication the guard prevents"
    );
    assert_eq!(
        f.runs.listings(),
        listings_after_sweep,
        "refused after the PDP answers and before qa-runs is listed"
    );
}

/// The guard refuses the shape, and does **not** silently repair it.
///
/// `AccessScope::tenant_only()` exists and would make the delete and the insert
/// agree — by widening the delete to the whole tenant, removing rows the PDP said
/// this subject may not touch. Exceeding a policy decision is worse than refusing
/// to act on one, so the rebuild refuses. This test is what would fail if a later
/// task reached for that repair: it asserts the call errors rather than
/// succeeding with a whole-tenant replay.
#[tokio::test]
async fn an_unexecutable_scope_is_refused_rather_than_narrowed_to_the_tenant() {
    let f = Fixture::with_authz(Duration::hours(1), 200, Arc::new(ResourceConstrainedAuthZ)).await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T09:00:00Z"), 1);

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            at("2026-08-18T08:00:00Z"),
            at("2026-08-18T10:00:00Z"),
        )
        .await;

    assert!(
        outcome.is_err(),
        "a tenant_only() repair would make this succeed, having deleted rows the policy \
         withheld: {outcome:?}"
    );
    assert_eq!(f.runs.listings(), 0, "nothing was read at all");
}

// ---------------------------------------------------------------------------
// The run-completed notification producer
// ---------------------------------------------------------------------------

/// A sweep that backfills a run asks for its completion notification.
///
/// This is the whole of the producer half: before this task
/// `NotifyService::notify_run_completed` had call sites only in its own test
/// module. Asserted on the run ids the notifier was handed rather than on a
/// count alone, so a producer that notified *some* run would still be visible
/// as the wrong one.
#[tokio::test]
async fn a_sweep_notifies_the_run_it_backfills() {
    let f = Fixture::new().await;
    let ghost = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 3);

    f.service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(f.notified_runs(), vec![ghost]);
}

/// **The one that matters.** Sweeping the same run twice notifies once.
///
/// The second sweep still *reads* the run — the assertion on `scanned` below is
/// what makes that explicit — because the lookback window deliberately reaches
/// behind the watermark. What stops the second notification is that
/// `consume_page`'s diff finds the run already projected and never reaches
/// `reproject`. Without the `scanned` assertion this test would also pass for a
/// sweep that simply stopped looking at the run, which is a different property
/// and not the one being claimed.
#[tokio::test]
async fn sweeping_the_same_run_twice_notifies_exactly_once() {
    let f = Fixture::new().await;
    let run = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 1);

    f.service
        .reconcile_once(tenant())
        .await
        .expect("the first sweep succeeds");
    let second = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the second sweep succeeds");

    assert_eq!(
        second.scanned, 1,
        "the run must still be inside the lookback window on the second pass, or this test \
         proves nothing about deduplication"
    );
    assert_eq!(second.backfilled, 0, "it was already projected");
    assert_eq!(f.notified_runs(), vec![run]);
}

/// A run whose backfill failed is not notified.
///
/// The notification says "this run's results have landed"; a run whose
/// projection never landed has nothing to say it about. This is also what pins
/// the call's *position*: a notify issued before or independently of the
/// projection write would fire here.
#[tokio::test]
async fn a_run_whose_backfill_failed_is_not_notified() {
    let f = Fixture::new().await;
    f.runs
        .add_finished_run_with_results(at("2026-08-18T10:00:00Z"), 1);
    f.runs.fail_result_reads(true);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the sweep itself succeeds; a per-run gap is an outcome, not an error");

    assert!(outcome.stopped_at_gap);
    assert_eq!(f.notified_runs(), Vec::<Uuid>::new());
}

/// A run qa-runs no longer has is not notified either — `reproject` returns on
/// the `read_run_projection` → `None` arm, before the transaction and before
/// the notify. Nothing was projected, so nothing finished.
#[tokio::test]
async fn a_vanished_run_is_not_notified() {
    let f = Fixture::new().await;
    let _vanished = f
        .runs
        .add_finished_run_that_vanishes(at("2026-08-18T10:00:00Z"));
    let real = f
        .runs
        .add_finished_run_with_results(at("2026-08-18T10:05:00Z"), 1);

    f.service
        .reconcile_once(tenant())
        .await
        .expect("sweep succeeds");

    assert_eq!(f.notified_runs(), vec![real]);
}

// ---------------------------------------------------------------------------
// The backfill guard: `m20260929_000003_seed_run_completed_notification_claims`
// ---------------------------------------------------------------------------

/// A [`SlackClient`] double that records the *text* of every message it was
/// asked to send.
///
/// The seed test asserts on real Slack egress rather than on
/// [`CountingNotifier`], because the thing under test is the claim row the
/// migration wrote — and only the real [`NotifyService`], over the real
/// `OrmNotifyRepository`, consults it. A counting notifier would be called
/// either way and would make the test pass with the migration deleted.
#[derive(Default)]
struct RecordingSlack {
    sent: Mutex<Vec<String>>,
}

impl RecordingSlack {
    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl SlackClient for RecordingSlack {
    async fn send(
        &self,
        _ctx: &SecurityContext,
        message: &SlackMessage,
    ) -> Result<SendOutcome, DomainError> {
        self.sent.lock().unwrap().push(message.text.clone());
        Ok(SendOutcome::Sent)
    }
}

/// A [`MailClient`] that is never reached: the fixture's config leaves email
/// disabled, so `routing` never asks for it. Present because
/// `NotifyService::new` requires a port, not because this test has an opinion
/// about mail.
struct UnreachedMail;

#[async_trait]
impl MailClient for UnreachedMail {
    async fn send(
        &self,
        _ctx: &SecurityContext,
        _message: &MailMessage,
    ) -> Result<SendOutcome, DomainError> {
        Err(DomainError::UnsupportedEgress {
            channel: "email".to_owned(),
        })
    }
}

/// The schedule every run in the seed fixture belongs to.
///
/// Its `slack_enabled` is what [`UpgradeFixture::set_slack_enabled`]'s tenant
/// flag is distinguished *from*: since the 2026-09-29 ruling a run with no
/// schedule routes on the tenant's settings alone
/// (`domain::notify::routing`'s header), so a fixture that wants the
/// schedule-level gate to be a real, separate gate has to register a real
/// schedule.
const SEED_SCHEDULE: Uuid = Uuid::from_u128(0x5C);

/// The reconciler wired to the **real** [`NotifyService`], over a database
/// still at the revision before the claim seed.
struct UpgradeFixture {
    db: toolkit_db::Db,
    runs: Arc<FakeRuns>,
    ctx: SecurityContext,
    slack: Arc<RecordingSlack>,
    /// Held as well as handed to the reconciler, so a test can turn a channel
    /// on *between* two passes — the operator action finding 1 is about.
    notify: Arc<NotifyService<OrmNotifyRepository>>,
    service: ReconcileService<OrmResultsRepository, OrmWatermarkRepository>,
}

impl UpgradeFixture {
    async fn before_the_upgrade() -> Self {
        let db = inmem_db_before_the_notification_backfill().await;
        let provider = Arc::new(DBProvider::<DomainError>::new(db.clone()));
        let runs = Arc::new(FakeRuns::default());
        let slack = Arc::new(RecordingSlack::default());
        let enforcer = PolicyEnforcer::new(Arc::new(TenantScopedAuthZ));
        let ctx = crate::domain::service::test_support::ctx(TENANT);

        let notify = Arc::new(NotifyService::new(
            Arc::clone(&provider),
            OrmNotifyRepository,
            enforcer.clone(),
            Arc::clone(&slack) as Arc<dyn SlackClient>,
            Arc::new(UnreachedMail) as Arc<dyn MailClient>,
            Arc::clone(&runs) as Arc<dyn RunsReader>,
        ));
        notify
            .save_config(
                &ctx,
                NotificationConfig {
                    slack_enabled: true,
                    slack_webhook_credstore_ref: "slack-hook".to_owned(),
                    // Every run this fixture adds has passing rows, and the
                    // outcome policy went live on 2026-09-29
                    // (`domain::notify::routing`'s header): at the default
                    // `notify_on_success: false` these tests would all be
                    // asserting against a run the *policy* silenced, not the
                    // cutoff or the claim they are about. Stated here so the
                    // tests below measure what they name.
                    notify_on_success: true,
                    ..NotificationConfig::default()
                },
            )
            .await
            .expect("saving the fixture's own config must not fail");
        runs.add_schedule(
            SEED_SCHEDULE,
            ScheduleNotificationSettings {
                slack_enabled: true,
                slack_channel: None,
                slack_events: Vec::new(),
            },
        );

        let ingest = Arc::new(IngestService::new(
            OrmResultsRepository,
            Arc::clone(&runs) as Arc<dyn RunsReader>,
        ));
        let service = ReconcileService::new(
            Arc::clone(&provider),
            OrmResultsRepository,
            OrmWatermarkRepository,
            Arc::clone(&runs) as Arc<dyn RunsReader>,
            ingest,
            Arc::clone(&notify) as Arc<dyn RunCompletionNotifier>,
            enforcer,
            Duration::hours(1),
            200,
        );

        Self {
            db,
            runs,
            ctx,
            slack,
            notify,
            service,
        }
    }

    /// Turn the tenant's Slack channel on or off, the way an operator does it
    /// from the settings page, leaving every other field as
    /// [`Self::before_the_upgrade`] saved it.
    async fn set_slack_enabled(&self, enabled: bool) {
        self.notify
            .save_config(
                &self.ctx,
                NotificationConfig {
                    slack_enabled: enabled,
                    slack_webhook_credstore_ref: "slack-hook".to_owned(),
                    notify_on_success: true,
                    ..NotificationConfig::default()
                },
            )
            .await
            .expect("saving the config must not fail");
    }

    /// A finished, scheduled run qa-runs knows about, with `results` passing
    /// rows.
    ///
    /// `results = 0` is the case this fixture exists for as much as any other:
    /// a run that finished and produced nothing. It leaves no row in
    /// `qa_test_results`, so `m20260929_000003` can never claim it and
    /// `ResultsRepository::ingested_run_ids_between` never reports it as
    /// ingested — which means every sweep re-projects it and reaches the
    /// producer with it. 1095 of the dev stand's 2326 finished runs are that
    /// shape.
    fn add_run_with_id(
        &self,
        id: Uuid,
        name: &str,
        finished_at: OffsetDateTime,
        results: usize,
    ) -> Uuid {
        let mut run = finished_run(id);
        run.name = name.to_owned();
        run.finished_at = Some(finished_at);
        run.schedule_id = Some(SEED_SCHEDULE);
        let rows = (0..results)
            .map(|n| test_row(id, &format!("tests/t{n}.py"), "test_ok", "PASSED", ""))
            .collect();
        self.runs.add_run(run, rows);
        id
    }

    fn add_run(&self, name: &str, finished_at: OffsetDateTime, results: usize) -> Uuid {
        self.add_run_with_id(Uuid::new_v4(), name, finished_at, results)
    }

    /// The projected rows for `run_id`, read back under the tenant's scope.
    async fn results_for(&self, run_id: Uuid) -> Vec<qa_insights_sdk::TestResultRecord> {
        let conn = self.db.conn().unwrap();
        OrmResultsRepository
            .list_by_run(&conn, &scope(TENANT), run_id)
            .await
            .unwrap()
    }

    /// This deployment's notification cutoff, as the upgrade wrote it.
    ///
    /// Every instant in the two tests below is expressed as an offset from
    /// this rather than as a calendar literal, which is what makes them
    /// deterministic: the cutoff is "whenever [`Self::upgrade`] ran", and a
    /// fixed literal would sit on one side of it or the other depending on the
    /// date the suite happens to run.
    async fn cutoff(&self) -> OffsetDateTime {
        let conn = self.db.conn().unwrap();
        OrmNotifyRepository
            .run_completed_cutoff(&conn)
            .await
            .expect("the cutoff read must not fail")
            .expect("the upgrade writes the cutoff row")
    }

    /// Write the projection a running deployment would already have held for
    /// `run_id` — the state the migration reads.
    ///
    /// Through the entity, not `upsert_run_results`: today's writer first
    /// upserts the run's `qa_run_projection_locks` row, and a database at this
    /// revision has no such table yet. The row is the one that writer stored.
    async fn already_projected(&self, run_id: Uuid, finished_at: OffsetDateTime) {
        use sea_orm::ActiveValue::Set;

        use crate::infra::storage::entity::test_result;

        let conn = self.db.conn().unwrap();
        let now = OffsetDateTime::now_utc();
        toolkit_db::secure::secure_insert::<test_result::Entity>(
            test_result::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(TENANT),
                run_id: Set(run_id),
                test_file: Set("tests/smoke.py".to_owned()),
                test_name: Set("test_ok".to_owned()),
                status: Set("PASSED".to_owned()),
                duration: Set(None),
                launch_id: Set(None),
                jira_key: Set(None),
                product_version: Set(None),
                app_build: Set(None),
                environment_id: Set(None),
                repo_id: Set(None),
                plan_path: Set(None),
                branch: Set(None),
                run_finished_at: Set(Some(finished_at)),
                run_created_at: Set(Some(finished_at - Duration::hours(1))),
                ingest_ordinal: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
            },
            &scope(TENANT),
            &conn,
        )
        .await
        .unwrap();
    }

    async fn upgrade(&self) {
        apply_pending_migrations(&self.db).await;
    }
}

/// **The claim seed, isolated.** A run this deployment had already ingested
/// before the upgrade is not notified by a later rebuild — even when its
/// `finished_at` is *after* the cutoff, so the cutoff cannot be what
/// suppressed it.
///
/// That instant is the point of the test. The cutoff covers everything that
/// finished before the upgrade, so on any ordinary pre-upgrade run the two
/// mechanisms overlap and this test could not tell which one fired. A run
/// whose recorded finish is a little *later* than the migration's own clock —
/// skew between the `job-db-migrate` Job and qa-runs' database, which nothing
/// makes zero — is the residual the cutoff genuinely cannot judge, and it is
/// exactly what `m20260929_000003`'s claim rows are still there for.
///
/// Driven through `rebuild` rather than a sweep, deliberately: a sweep skips
/// an already-projected run at `consume_page`'s diff and never reaches
/// `reproject`, so a sweep-based version would be green with the seed deleted.
/// `replay` has no diff — every run in the operator's window is re-projected,
/// which is what `ReconcileService::rebuild`'s own doc says a rebuild is for,
/// and where a mass re-announcement of history would come from.
///
/// **"The run with no seeded claim" is not the same set it was.** Both runs
/// here route to a send, so both are decided the first time they are
/// considered and this test is unchanged by the finding-1 fix. What that fix
/// changed is the class next door — a run whose routing *declined*, which also
/// has no seeded claim and which this test never covered;
/// `a_run_whose_routing_declined_is_not_announced_by_a_later_rebuild` below is
/// the one that does.
#[tokio::test]
async fn a_run_already_ingested_at_the_upgrade_is_not_notified_by_a_later_rebuild() {
    let f = UpgradeFixture::before_the_upgrade().await;

    // The projection a running deployment already held. Written before the
    // upgrade, which is what `m20260929_000003` reads.
    let already_ingested = Uuid::new_v4();
    f.already_projected(already_ingested, at("2026-08-18T10:00:00Z"))
        .await;

    f.upgrade().await;
    let cutoff = f.cutoff().await;

    // Registered with a finish instant *after* the cutoff, so only the claim
    // row can suppress it.
    f.add_run_with_id(
        already_ingested,
        "nightly-already-ingested",
        cutoff + Duration::minutes(30),
        1,
    );
    f.add_run("nightly-new", cutoff + Duration::hours(1), 1);

    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            cutoff - Duration::hours(1),
            cutoff + Duration::hours(2),
        )
        .await
        .expect("the rebuild succeeds");

    assert_eq!(
        outcome.backfilled, 2,
        "both runs must have been replayed, or nothing was suppressed - it was skipped"
    );
    let sent = f.slack.sent();
    assert_eq!(
        sent.len(),
        1,
        "exactly one message: the run with no seeded claim. Got {sent:?}"
    );
    assert!(
        sent[0].contains("nightly-new"),
        "the message is for the unclaimed run; got {sent:?}"
    );
    assert!(
        !sent[0].contains("nightly-already-ingested"),
        "a run this deployment had already ingested must never be re-announced; got {sent:?}"
    );
}

/// **Finding 1, the case the two mechanisms above never covered.** A run whose
/// routing *declined* at the time is not announced by a later rebuild either —
/// not by the claim seed (the run finished after the upgrade, so the migration
/// never saw it), not by the cutoff (it finished after that too), but by the
/// claim `notify_run_completed` now takes for a channel it decides not to send
/// on (`NotifyService::decline_run_completed_channel`).
///
/// The sequence is the operator one, in order: the tenant has Slack off, a run
/// finishes and is swept, the operator turns Slack on, and then rebuilds the
/// window — for a projection gap, which is what the endpoint is for, not to
/// re-announce anything. Before the fix that rebuild mailed the run, and up to
/// a full rebuild budget of runs like it in one call.
///
/// `slack_enabled: false` on the *tenant* while the *schedule* still notifies
/// is deliberately the **silent** skip (`routing.rs`'s header, "Slack skips:
/// audited or silent") — the one that writes no audit row
/// — because it is the state an operator is in before they configure the
/// channel for the first time, and a claim taken only on the audited skip
/// would leave exactly this case open.
///
/// Driven through `rebuild` for the reason the test above gives: a sweep would
/// skip the already-projected run at `consume_page`'s diff and never reach
/// `reproject` at all.
#[tokio::test]
async fn a_run_whose_routing_declined_is_not_announced_by_a_later_rebuild() {
    let f = UpgradeFixture::before_the_upgrade().await;
    f.upgrade().await;
    let cutoff = f.cutoff().await;

    // The channel is off when the run is swept.
    f.set_slack_enabled(false).await;
    f.add_run("nightly-declined", cutoff + Duration::minutes(30), 1);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the sweep succeeds");
    assert_eq!(
        outcome.backfilled, 1,
        "the run must have reached `reproject`, or the decision was never made at all"
    );
    assert!(
        f.slack.sent().is_empty(),
        "nothing may be sent while the channel is off; got {:?}",
        f.slack.sent()
    );

    // The operator configures the channel, then rebuilds the window.
    f.set_slack_enabled(true).await;
    let outcome = f
        .service
        .rebuild(
            &f.ctx,
            cutoff - Duration::hours(1),
            cutoff + Duration::hours(2),
        )
        .await
        .expect("the rebuild succeeds");

    assert_eq!(
        outcome.backfilled, 1,
        "the rebuild must have replayed the run - otherwise nothing was suppressed, it was \
         never looked at"
    );
    assert!(
        f.slack.sent().is_empty(),
        "enabling a channel must not retroactively announce a run that was declined while it \
         was off; got {:?}",
        f.slack.sent()
    );
}

/// **The cutoff, isolated — and the class the claim seed cannot reach.**
///
/// A run that finished before this deployment started notifying is not
/// notified, *although nothing claimed it*: it produced **zero** result rows,
/// so it is absent from `qa_test_results`, so `m20260929_000003` seeded no
/// claim for it and `ResultsRepository::ingested_run_ids_between` will never
/// call it ingested. It is re-projected on every single sweep — which is
/// precisely why it reaches the producer, and precisely why the claim seed
/// alone left 1095 of the dev stand's 2326 finished runs able to mail
/// themselves twice on the first pass after the upgrade.
///
/// Driven through a **sweep**, because for this shape of run a sweep is enough:
/// the diff never suppresses it, so `reproject` runs and the producer is
/// reached. `outcome.backfilled == 2` is the assertion that says so — without
/// it this test would also pass for a sweep that never looked at the run.
#[tokio::test]
async fn a_run_that_finished_before_the_cutoff_is_not_notified_even_with_no_claim() {
    let f = UpgradeFixture::before_the_upgrade().await;
    f.upgrade().await;
    let cutoff = f.cutoff().await;

    let ghost = f.add_run("nightly-empty", cutoff - Duration::hours(24), 0);
    f.add_run("nightly-new", cutoff + Duration::hours(1), 1);

    let outcome = f
        .service
        .reconcile_once(tenant())
        .await
        .expect("the sweep succeeds");

    assert_eq!(
        outcome.backfilled, 2,
        "both runs must have reached `reproject` - a zero-result run is never 'already \
         ingested', so the diff cannot be what suppressed it"
    );
    assert!(
        f.results_for(ghost).await.is_empty(),
        "the zero-result run really did produce no rows, which is what leaves it unclaimable"
    );

    let sent = f.slack.sent();
    assert_eq!(
        sent.len(),
        1,
        "exactly one message: the run that finished after the cutoff. Got {sent:?}"
    );
    assert!(
        sent[0].contains("nightly-new"),
        "the message is for the run after the cutoff; got {sent:?}"
    );
    assert!(
        !sent[0].contains("nightly-empty"),
        "a run older than the cutoff is history and must not be announced; got {sent:?}"
    );
}

/// **A history run is audited once, not once per sweep.** The zero-result run
/// above is re-projected on every tick for as long as it sits in the lookback
/// window, and `is_history` used to append a `skipped` row to
/// `qa_notification_log` every time — a log with no retention sweep, growing by
/// one row per tick per such run, burying the rows an operator reads it for.
/// Three sweeps here, so a guard that only stopped the second write would
/// still be caught by the third. And the run is still not announced, and its
/// two channel slots are still free, which is what `is_history`'s own doc
/// requires for an operator who later moves the cutoff.
#[tokio::test]
async fn a_history_run_is_audited_once_however_many_sweeps_reach_it() {
    let f = UpgradeFixture::before_the_upgrade().await;
    f.upgrade().await;
    let cutoff = f.cutoff().await;
    let ghost = f.add_run("nightly-empty", cutoff - Duration::hours(24), 0);

    for _ in 0..3 {
        f.service
            .reconcile_once(tenant())
            .await
            .expect("the sweep succeeds");
    }

    let history_rows: Vec<_> = f
        .notify
        .list_log(&f.ctx, Some(100))
        .await
        .expect("the log reads")
        .into_iter()
        .filter(|e| e.run_id == Some(ghost) && e.detail.contains("it is history"))
        .collect();
    assert_eq!(
        history_rows.len(),
        1,
        "three sweeps over one history run must leave one audit row, not one per sweep; \
         got {history_rows:?}"
    );
    assert!(f.slack.sent().is_empty(), "history is still not announced");

    let provider = DBProvider::<DomainError>::new(f.db.clone());
    let conn = provider.conn().unwrap();
    let held = OrmNotifyRepository
        .claimed_kinds(&conn, &scope(TENANT), TENANT, ghost, "run_completed")
        .await
        .expect("the claims read");
    assert!(
        !held
            .iter()
            .any(|k| k == "run_completed_slack" || k == "run_completed_email"),
        "the audit must not spend a channel's slot - an operator who moves the cutoff \
         needs it; held {held:?}"
    );
}

/// The positive direction, on its own so that neither test above can be
/// satisfied by a producer that has simply stopped sending.
///
/// Two runs, both after the cutoff, both unclaimed: both are announced.
#[tokio::test]
async fn runs_that_finished_after_the_cutoff_are_all_notified() {
    let f = UpgradeFixture::before_the_upgrade().await;
    f.upgrade().await;
    let cutoff = f.cutoff().await;

    f.add_run("nightly-one", cutoff + Duration::minutes(10), 1);
    f.add_run("nightly-two", cutoff + Duration::minutes(20), 1);

    f.service
        .reconcile_once(tenant())
        .await
        .expect("the sweep succeeds");

    let sent = f.slack.sent();
    assert_eq!(
        sent.len(),
        2,
        "both runs are news, not history. Got {sent:?}"
    );
    assert!(sent.iter().any(|m| m.contains("nightly-one")), "{sent:?}");
    assert!(sent.iter().any(|m| m.contains("nightly-two")), "{sent:?}");
}
