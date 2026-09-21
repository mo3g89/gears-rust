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

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use time::{Duration, OffsetDateTime};
use toolkit_db::DBProvider;
use toolkit_gts::GTS_ID_PREFIX;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{ReconcileOutcome, ReconcileService};
use crate::domain::error::DomainError;
use crate::domain::ports::RunsReader;
use crate::domain::repos::{ResultsRepository, WatermarkKind, WatermarkRepository};
use crate::domain::service::ingest::IngestService;
use crate::domain::service::test_support::{
    DenyAllAuthZ, FakeRuns, RecordingAuthZ, ResourceConstrainedAuthZ, TenantScopedAuthZ,
};
use crate::domain::system_actor::TenantBound;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{inmem_db, scope};
use crate::infra::storage::watermark_sea_repo::OrmWatermarkRepository;

const TENANT: Uuid = Uuid::from_u128(0x0A);

fn tenant() -> TenantBound {
    TenantBound::new(TENANT).expect("non-nil")
}

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        .expect("an RFC3339 fixture instant")
}

struct Fixture {
    db: toolkit_db::Db,
    provider: Arc<DBProvider<DomainError>>,
    runs: Arc<FakeRuns>,
    /// The operator's own context, which only [`ReconcileService::rebuild`]
    /// takes. The sweep has no caller and derives its own system actor, so no
    /// sweep test touches this field.
    ctx: SecurityContext,
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
        let service = ReconcileService::new(
            provider.clone(),
            OrmResultsRepository,
            OrmWatermarkRepository,
            runs.clone(),
            ingest,
            PolicyEnforcer::new(authz),
            lookback,
            page_size,
        );
        Self {
            db,
            provider,
            runs,
            ctx: crate::domain::service::test_support::ctx(TENANT),
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

    async fn watermark(&self) -> Option<OffsetDateTime> {
        let conn = self.db.conn().unwrap();
        OrmWatermarkRepository
            .get(&conn, &scope(TENANT))
            .await
            .unwrap()
            .last_reconciled_finished_at
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
// Task 5a: the reprojection held a transaction across a cross-gear read
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
// Task 16: the operator rebuild
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
