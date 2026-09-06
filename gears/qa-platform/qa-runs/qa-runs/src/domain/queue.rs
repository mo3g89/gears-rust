//! Per-platform run queue: admission policy and FIFO drain planning.
//!
//! Ported from `manager/src/services/run_queue.rs`. Frozen semantics — the
//! governing document is `../testrunner/docs/guides/exclusive-runs-and-the-queue.md`.
//!
//! Pure by design, for the reason the source system gives
//! (`manager/src/services/run_queue.rs:4-7`): the rules are the part worth
//! testing, and keeping them free of I/O is the only way to test them without a
//! database.
//!
//! # One deliberate adaptation
//!
//! The source system derives occupancy by unioning two independent views of
//! the same fact — live Argo workflows and unreleased DB claims — and
//! de-duplicating them by key (`manager/src/services/run_queue.rs:682-712`,
//! `merge_occupancy`). That merge exists only because Argo and the database
//! were two separate records of "what holds this platform". Here there is one
//! record: the `qa-environments` platform lease, whose `acquire`/`release` are
//! the authoritative compare-and-swap
//! (`qa-environments/src/domain/service/leases.rs:66`, `:150`). So [`Occupancy`]
//! is derived from `LeaseState` and `merge_occupancy` has no counterpart. Two
//! consequences worth stating:
//!
//! * The planners below are **advisory**. The lease CAS is the source of
//!   truth, so a row the planner selects may still lose its `acquire` and stay
//!   queued for the next tick. That is safe in the only direction that
//!   matters: the lease can refuse a dispatch the planner allowed, but it can
//!   never permit one the planner refused.
//! * The source system's fail-safe — an unreadable Argo makes the platform
//!   read as exclusively held so nothing dispatches blind
//!   (`manager/src/services/run_dispatcher.rs:237-252`, which pushes a synthetic
//!   `exclusive: true` occupant) — ports as: a failing lease read is treated
//!   as [`Occupancy::Exclusive`]. That belongs to the service that performs the
//!   read; nothing here can fail, so nothing here can fail closed.
//!
//! One guard the source system has does not survive the adaptation, and this is
//! the reason: legacy's occupancy is a `Vec<Occupant>`, so it can test
//! "an exclusive occupant *anywhere* in the list blocks" against a degradation
//! to "look at the first occupant only"
//! (`manager/src/services/run_queue.rs:1263-1267`, `:1312-1325`). [`Occupancy`]
//! is a three-variant enum with no list to scan, so that degradation is not
//! expressible and the tests have no counterpart here.

use std::collections::HashMap;

use qa_environments_sdk::LeaseState;
use time::OffsetDateTime;
use uuid::Uuid;

/// What currently holds a platform, as far as an admission decision cares.
///
/// Three states rather than a holder list because that is all the rule reads:
/// whether anything holds it, and whether that hold is exclusive. `holders` is
/// carried on the parallel arm only so the dispatcher can log it and so a
/// future rule that *counts* occupancy has the number available without a
/// second lease read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occupancy {
    /// Nothing holds the platform.
    Free,
    /// Held by one or more parallel runs.
    Parallel {
        /// How many runs hold it. Never `0` — see [`Occupancy::from_lease`].
        holders: usize,
    },
    /// Held by a single run that owns the platform outright.
    Exclusive,
}

impl Occupancy {
    /// Project a lease state onto the admission rule's view.
    ///
    /// An empty `HeldParallel` holder list reads [`Occupancy::Free`]: a lease
    /// row that exists but holds nothing must not block an incoming exclusive
    /// run. (`qa-environments` releases to `Free` rather than to an empty
    /// parallel hold — `qa-environments/src/domain/lease.rs:169-174` — so this
    /// arm is defensive. But a defensive arm that failed *open* here would be
    /// the wrong direction, and one that blocks forever is a wedged platform.)
    ///
    /// This takes a *successful* read. A lease read that **fails** must not
    /// reach here at all: map the error to [`Occupancy::Exclusive`] so nothing
    /// dispatches blind (this module's header;
    /// `manager/src/services/run_dispatcher.rs:237-252`). That is the one
    /// obligation in this module whose failure direction is the permissive one.
    #[must_use]
    pub fn from_lease(state: &LeaseState) -> Self {
        match state {
            LeaseState::Free => Self::Free,
            LeaseState::HeldParallel { holders } if holders.is_empty() => Self::Free,
            LeaseState::HeldParallel { holders } => Self::Parallel {
                holders: holders.len(),
            },
            LeaseState::HeldExclusive { .. } => Self::Exclusive,
        }
    }
}

/// The single definition of the platform exclusivity rule: may a run with this
/// exclusivity flag join a platform in this state?
///
/// Deliberately shared by the admission path and the dispatcher, exactly as in
/// the source system (`manager/src/services/run_queue.rs:14-24`). If the two
/// ever disagreed the queue would drift — a weaker dispatcher rule would start
/// a run alongside an exclusive one, a stronger one would stop the queue
/// draining.
#[must_use]
pub fn platform_admits(occupancy: Occupancy, exclusive: bool) -> bool {
    match occupancy {
        // An exclusive occupant owns the platform: nothing joins it.
        Occupancy::Exclusive => false,
        // An incoming exclusive run needs the platform to itself.
        Occupancy::Parallel { .. } => !exclusive,
        Occupancy::Free => true,
    }
}

/// Whether an incoming launch may start now or must queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// Start it inline; no queue row waits.
    Dispatch,
    /// Write a `queued` row; the dispatcher will start it.
    Queue,
}

/// Decide whether an incoming launch may start now or must queue.
///
/// Note what this function does **not** take: the global concurrency cap.
/// Exceeding `max_concurrent_runs` is a 429 at admission — today's behaviour —
/// and must not silently become a queued row
/// (`manager/src/services/run_queue.rs:30-40`, with a legacy test asserting
/// exactly that at `:1281-1286`). The caller checks the cap before calling.
///
/// The depth check is written before the occupancy check to match the source.
/// In this pure function the order is not observable — both paths return
/// [`AdmissionDecision::Queue`] — but it is load-bearing at the call site, where
/// the depth read and this decision happen under one platform lock and a
/// rejection must not pay for an occupancy read it would throw away
/// (`manager/src/services/run_queue.rs:633-655`).
#[must_use]
pub fn decide_admission(
    occupancy: Occupancy,
    queued_depth: usize,
    incoming_exclusive: bool,
) -> AdmissionDecision {
    // Strict FIFO: once anything is queued for this platform, later arrivals
    // queue behind it whatever their flag. Without it a stream of parallel runs
    // starves a queued exclusive row forever
    // (`manager/src/services/run_queue.rs:42-47`; guide lines 105-107).
    if queued_depth > 0 {
        return AdmissionDecision::Queue;
    }
    if !platform_admits(occupancy, incoming_exclusive) {
        return AdmissionDecision::Queue;
    }
    AdmissionDecision::Dispatch
}

/// A queued row, reduced to what drain planning reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueuedRow {
    /// The queue row's identifier, which is what the planner returns.
    pub id: Uuid,
    /// The run's resolved exclusivity flag.
    pub exclusive: bool,
}

/// The cluster-wide concurrency limit, as named fields.
///
/// Deliberately not `(u32, u32)`: both numbers are the same type and their
/// order would be documented rather than enforced, so `global_cap_status(max,
/// active)` would compile and silently invert the limit. Task 14 constructs
/// this pair by hand while threading the budget across platforms, which is the
/// least visible place a transposition could hide. The source system reaches
/// for the same remedy against the same hazard
/// (`manager/src/services/run_queue.rs:682-684`, `OccupancySources` — "as named
/// fields so they cannot be transposed at a call site"), as does
/// [`crate::domain::exclusivity::Tiers`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalCap {
    /// Runs already counted against the limit, cluster-wide.
    pub active: u32,
    /// The limit itself. `0` disables it. **No longer the shipped default**
    /// — `0` is kept as an explicit "unbounded" opt-out; see
    /// `crate::config::QaRunsConfig::max_concurrent_runs` for the current
    /// default and its derivation.
    pub max: u32,
}

impl GlobalCap {
    /// The same limit, with `claimed` rows spent from its budget.
    ///
    /// One dispatcher tick visits every platform that has a queue, and
    /// [`plan_dispatch_batch`]'s own `claimed.len()` counts one platform only.
    /// The caller must fold each platform's claims into `active` before moving
    /// to the next; a caller that passes the same [`GlobalCap`] to every
    /// platform gives each of them the full `max - active` budget and
    /// overshoots `max` by `(platforms with a queue - 1) x (max - active)`
    /// (`manager/src/services/run_dispatcher.rs:406-414`). Keeping that
    /// arithmetic here makes the obligation testable rather than prose.
    ///
    /// Saturating rather than plain addition: legacy adds plainly
    /// (`manager/src/services/run_dispatcher.rs:412`) and the difference is
    /// unreachable at any real run count, but a saturated cap reads as "no
    /// room" — the fail-closed direction, and better than a dispatcher tick
    /// that panics and stops the queue draining at all.
    #[must_use]
    pub fn with_claimed(self, claimed: u32) -> Self {
        Self {
            active: self.active.saturating_add(claimed),
            max: self.max,
        }
    }
}

/// Plan which queued rows a single dispatcher tick may claim, in FIFO order.
///
/// `fifo` must be ordered oldest-first. `GlobalCap::max == 0` disables the
/// limit; `active` must be the cluster's *committed* count **plus** anything
/// already claimed earlier in the same tick on other platforms — see
/// [`GlobalCap::with_claimed`], which is where that threading lives
/// (`manager/src/services/run_queue.rs:62-69`).
#[must_use]
pub fn plan_dispatch_batch(
    mut occupancy: Occupancy,
    fifo: &[QueuedRow],
    cap: Option<GlobalCap>,
) -> Vec<Uuid> {
    let mut claimed = Vec::new();

    for row in fifo {
        // Stop rather than skip ahead: a row behind a blocked one must not
        // overtake it, or a queued exclusive row starves
        // (`manager/src/services/run_queue.rs:78-82`).
        if !platform_admits(occupancy, row.exclusive) {
            break;
        }
        // Only new claims count. `active` is cluster-wide and already includes
        // this platform's occupants, so adding `occupancy` here a second time
        // would double-count them
        // (`manager/src/services/run_queue.rs:83-87`, test at `:1400-1410`).
        // Legacy nests these two conditions; a let-chain is the same test and
        // is what `clippy::collapsible_if` requires here.
        if let Some(GlobalCap { active, max }) = cap
            && max > 0
            && active as usize + claimed.len() >= max as usize
        {
            break;
        }

        claimed.push(row.id);
        // Fold the claim into occupancy so the rules apply to rows claimed
        // earlier in the same tick. This is what makes a queue of exclusive
        // rows drain one per tick (guide lines 102-104): an exclusive claim
        // becomes an exclusive occupant, and the next iteration breaks.
        //
        // The parallel arm accumulates rather than overwrites, and legacy
        // records why (`manager/src/services/run_queue.rs:90-93`): today the
        // count is indistinguishable from overwriting, because an exclusive
        // claim always ends the loop and so can only ever be the last one — but
        // accumulation is the correct primitive the moment any rule *counts*
        // occupancy rather than just testing it. The `+ 1` is unobservable;
        // this comment is the only thing standing between it and a future
        // "simplification".
        occupancy = if row.exclusive {
            Occupancy::Exclusive
        } else {
            match occupancy {
                Occupancy::Free => Occupancy::Parallel { holders: 1 },
                Occupancy::Parallel { holders } => Occupancy::Parallel {
                    holders: holders + 1,
                },
                // Unreachable: `platform_admits` already refused above.
                Occupancy::Exclusive => Occupancy::Exclusive,
            }
        };
    }

    claimed
}

/// Normalise `queue_max_depth`: `None` when the limit is disabled (`0`).
///
/// Mirrors [`global_cap_status`] so both capacity settings read the same way
/// at their call sites (`manager/src/services/run_queue.rs:810-819`).
#[must_use]
pub fn depth_limit(max_depth: u32) -> Option<u32> {
    if max_depth == 0 {
        None
    } else {
        Some(max_depth)
    }
}

/// Whether a launch must be rejected outright because the platform's queue is
/// already at `queue_max_depth`.
///
/// Deliberately separate from [`decide_admission`]: that function answers a
/// *scheduling* question — is there a row to write, and in which state —
/// whereas this is a *capacity* answer that writes no row at all. Keeping them
/// apart is what lets the caller run this check first and skip the occupancy
/// read entirely on a rejection
/// (`manager/src/services/run_queue.rs:635-655`). Note the composition
/// property: a full queue implies
/// `queued_depth > 0` for any limit of 1 or more, so a launch that would have
/// started inline on an idle platform can never be rejected by this rule
/// (`manager/src/services/run_queue.rs:821-832`).
#[must_use]
pub fn queue_is_full(queued_depth: usize, limit: Option<u32>) -> bool {
    matches!(limit, Some(limit) if queued_depth >= limit as usize)
}

/// Normalise the global concurrency setting: `None` when disabled
/// (`max_concurrent_runs == 0`, the explicit "unbounded" opt-out — no
/// longer the shipped default, see `crate::config::QaRunsConfig`), else the
/// [`GlobalCap`] (`manager/src/services/run_queue.rs:714-722`).
#[must_use]
pub fn global_cap_status(active: u32, max: u32) -> Option<GlobalCap> {
    if max == 0 {
        None
    } else {
        Some(GlobalCap { active, max })
    }
}

/// Whether the global limit currently leaves no room
/// (`manager/src/services/run_queue.rs:724-727`).
#[must_use]
pub fn cap_reached(cap: Option<GlobalCap>) -> bool {
    matches!(cap, Some(GlobalCap { active, max }) if active >= max)
}

/// The timestamp before which a `queued` row has outlived `queue_ttl_seconds`.
///
/// `None` means "do not expire anything": either expiry is switched off
/// (`ttl_seconds == 0`, the same convention as `max_concurrent_runs == 0`) or
/// the configured TTL is large enough to overflow the arithmetic. Both fail
/// **open** on purpose — a stale queued row is an operator annoyance, whereas
/// a panicking dispatcher tick stops the queue draining at all
/// (`manager/src/services/run_queue.rs:729-744`).
#[must_use]
pub fn expiry_cutoff(now: OffsetDateTime, ttl_seconds: u64) -> Option<OffsetDateTime> {
    ttl_window(ttl_seconds).and_then(|window| now.checked_sub(window))
}

/// When a queued row will hit its TTL, for display.
///
/// The mirror image of [`expiry_cutoff`] — the same `None` cases for the same
/// fail-open reason — and the reason no client ever hard-codes 7200: the TTL
/// has exactly one definition, in config
/// (`manager/src/services/run_queue.rs:746-759`).
#[must_use]
pub fn ttl_expires_at(enqueued_at: OffsetDateTime, ttl_seconds: u64) -> Option<OffsetDateTime> {
    ttl_window(ttl_seconds).and_then(|window| enqueued_at.checked_add(window))
}

/// Shared guard for both TTL helpers: `0` disables, and an unrepresentable
/// span disables rather than panicking.
///
/// Legacy needs two guards here because `chrono::Duration::try_seconds` can
/// itself reject a span (`manager/src/services/run_queue.rs:740-743`);
/// `time::Duration::seconds` accepts any `i64`, so the only guard left at this
/// level is the `u64` -> `i64` conversion. The date-range guard moves to the
/// `checked_add`/`checked_sub` in the two callers, which is where legacy's
/// second guard also lives.
fn ttl_window(ttl_seconds: u64) -> Option<time::Duration> {
    if ttl_seconds == 0 {
        return None;
    }
    i64::try_from(ttl_seconds).ok().map(time::Duration::seconds)
}

/// What a queued row is waiting for, phrased for an operator.
///
/// Strict FIFO makes the non-head case exact: anything but the head waits on
/// the rows in front of it, whatever the platform is doing. The head waits on
/// whatever still holds the platform.
///
/// `holders` carries `None` for a claim that has not produced an execution yet
/// (still mid force-sync / bundle build), which is why it is not `&[String]`:
/// an empty string there would read as a run called "".
///
/// The deliberate gap: when the head's platform has no claim at all, occupancy
/// may still be something this gear cannot name — or a lease read that failed,
/// which reads as busy by design. The answer then names no run rather than
/// fabricating one. `position` is 1-based; `0` falls into the head branch and
/// is unreachable from the listing
/// (`manager/src/services/run_queue.rs:761-808`).
///
/// **These strings are published API, not log text.** Guide line 177 documents
/// four of the five verbatim in the `blocked_by` table — it is the only place
/// the frozen guide holds literals — so changing one is a contract change, not
/// a wording change.
#[must_use]
pub fn describe_blocker(position: u32, holders: &[Option<String>]) -> String {
    if position > 1 {
        let ahead = position - 1;
        return format!(
            "waiting for {ahead} queued run{} ahead of it",
            if ahead == 1 { "" } else { "s" }
        );
    }

    match holders {
        [] => "waiting for its turn".to_owned(),
        [Some(name)] => format!("waiting for run {name}"),
        [None] => "waiting for a run that is still starting".to_owned(),
        many => {
            let names: Vec<&str> = many
                .iter()
                .map(|holder| holder.as_deref().unwrap_or("(not yet started)"))
                .collect();
            format!(
                "waiting for {} active runs ({})",
                names.len(),
                names.join(", ")
            )
        }
    }
}

/// One row's inputs to position numbering.
#[derive(Clone, Copy, Debug)]
pub struct PositionInput {
    /// The queue row's identifier — the key the returned map is built on.
    pub id: Uuid,
    /// Which platform's queue the row is in. Positions never cross platforms.
    pub platform_id: Uuid,
    /// When the row joined the queue; the primary FIFO sort key.
    pub enqueued_at: OffsetDateTime,
    /// Whether the row is in the `queued` state. Only these get a position.
    pub queued: bool,
}

/// 1-based FIFO position per platform, oldest `queued` row first.
///
/// Only `queued` rows get a position; every other state reports `None`
/// (guide line 175). Ties on `enqueued_at` break on `id`, matching the FIFO
/// `ORDER BY enqueued_at ASC, id ASC` (`manager/src/services/run_queue.rs:248`)
/// so positions and drain order cannot disagree.
///
/// Computed over the rows the caller passes — which is the rows *that
/// request returned*. The documented consequence: a truncating limit can push
/// an older queued row out of the window, collapsing the rows behind it toward
/// position 1 and turning their `blocked_by` into the vaguer head-branch text.
/// The platform-filtered listing is the reliable one
/// (`manager/src/services/run_queue.rs:435-453`; guide lines 179-184).
///
/// `u32` rather than legacy's `i64`: that width came from SQL `BIGINT` via
/// `row_number()`, and nothing here does that — positions are computed in
/// memory from rows already in hand and never stored. `u32` is what the gear
/// publishes (`qa-runs-sdk/src/models.rs:389`), so the mapper needs no
/// fallible conversion for a value this function guarantees is `>= 1`.
#[must_use]
pub fn assign_positions(rows: &[PositionInput]) -> HashMap<Uuid, u32> {
    let mut queued: Vec<&PositionInput> = rows.iter().filter(|row| row.queued).collect();
    // `platform_id` leads the sort as a *grouping* device, not an ordering
    // claim: it clusters each platform's rows so the run-length reset below
    // sees them consecutively. No caller may read anything into which platform
    // sorts first — only `enqueued_at`, then `id`, carry FIFO meaning.
    queued.sort_by(|a, b| {
        a.platform_id
            .cmp(&b.platform_id)
            .then(a.enqueued_at.cmp(&b.enqueued_at))
            .then(a.id.cmp(&b.id))
    });

    let mut positions = HashMap::new();
    let mut current: Option<Uuid> = None;
    let mut position = 0u32;
    for row in queued {
        if current != Some(row.platform_id) {
            current = Some(row.platform_id);
            position = 0;
        }
        position += 1;
        positions.insert(row.id, position);
    }
    positions
}

#[cfg(test)]
mod tests {
    use super::*;
    use qa_environments_sdk::LeaseState;
    use time::OffsetDateTime;

    /// The one place a fixture UUID is minted. Row ids, holder ids and platform
    /// ids all come from here, so a test that needs two of them distinct just
    /// picks two different `n`.
    fn id(n: u8) -> uuid::Uuid {
        uuid::Uuid::from_u128(u128::from(n))
    }

    fn row(n: u8, exclusive: bool) -> QueuedRow {
        QueuedRow {
            id: id(n),
            exclusive,
        }
    }

    fn holder(n: u8) -> uuid::Uuid {
        id(n)
    }

    /// 2026-08-13 00:00:00 UTC as a Unix timestamp.
    const DAY: i64 = 1_786_579_200;

    /// An hour of 2026-08-13 UTC, as an instant.
    ///
    /// The plan's fixtures were written with `time::macros::datetime!`, which is
    /// unavailable here: the workspace pins `time` with only the `serde`,
    /// `formatting` and `parsing` features (root `Cargo.toml:516`), and adding
    /// `macros` for tests alone is not worth a workspace-wide feature change.
    /// The instants are the plan's, arrived at by arithmetic instead.
    fn at(hour: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(DAY + hour * 3600)
            .expect("fixture instant is within the supported date range")
    }

    // ---------- Occupancy::from_lease ----------

    #[test]
    fn a_free_lease_is_unoccupied() {
        assert_eq!(Occupancy::from_lease(&LeaseState::Free), Occupancy::Free);
    }

    #[test]
    fn parallel_holders_are_a_parallel_occupancy() {
        let state = LeaseState::HeldParallel {
            holders: vec![holder(1), holder(2)],
        };
        assert_eq!(
            Occupancy::from_lease(&state),
            Occupancy::Parallel { holders: 2 }
        );
    }

    /// An empty parallel holder list is a lease row that exists but holds
    /// nothing — it must read Free, not as a zero-holder occupancy that
    /// blocks an incoming exclusive run forever.
    #[test]
    fn an_empty_parallel_holder_list_reads_free() {
        let state = LeaseState::HeldParallel {
            holders: Vec::new(),
        };
        assert_eq!(Occupancy::from_lease(&state), Occupancy::Free);
    }

    #[test]
    fn an_exclusive_hold_is_an_exclusive_occupancy() {
        let state = LeaseState::HeldExclusive { holder: holder(1) };
        assert_eq!(Occupancy::from_lease(&state), Occupancy::Exclusive);
    }

    // ---------- platform_admits ----------

    #[test]
    fn a_parallel_run_joins_a_platform_busy_with_parallel_runs() {
        assert!(platform_admits(Occupancy::Parallel { holders: 1 }, false));
    }

    #[test]
    fn a_parallel_run_starts_on_an_idle_platform() {
        assert!(platform_admits(Occupancy::Free, false));
    }

    #[test]
    fn an_exclusive_run_needs_the_platform_to_itself() {
        assert!(platform_admits(Occupancy::Free, true));
        assert!(!platform_admits(Occupancy::Parallel { holders: 1 }, true));
    }

    #[test]
    fn an_exclusive_occupant_admits_nothing() {
        assert!(!platform_admits(Occupancy::Exclusive, false));
        assert!(!platform_admits(Occupancy::Exclusive, true));
    }

    // ---------- decide_admission ----------

    #[test]
    fn an_admissible_launch_dispatches() {
        assert_eq!(
            decide_admission(Occupancy::Free, 0, false),
            AdmissionDecision::Dispatch
        );
    }

    #[test]
    fn a_blocked_launch_queues() {
        assert_eq!(
            decide_admission(Occupancy::Exclusive, 0, false),
            AdmissionDecision::Queue
        );
    }

    /// Strict FIFO: once anything is queued for a platform, later arrivals
    /// queue behind it even on an idle platform — otherwise a queued exclusive
    /// row starves behind a stream of parallel ones (`run_queue.rs:42-48`; guide
    /// lines 105-107).
    ///
    /// What this pins is the depth rule's *effect*, not the source order of the
    /// two checks: both branches return `Queue`, so no test of this pure
    /// function can distinguish depth-first from occupancy-first. The order is
    /// load-bearing one layer up, where the depth check must precede the
    /// occupancy *read* under the platform lock (`run_queue.rs:633-655`), and it
    /// is the service that can pin that.
    #[test]
    fn a_non_empty_queue_holds_back_a_launch_on_an_idle_platform() {
        assert_eq!(
            decide_admission(Occupancy::Free, 1, false),
            AdmissionDecision::Queue
        );
        assert_eq!(
            decide_admission(Occupancy::Free, 3, true),
            AdmissionDecision::Queue
        );
    }

    /// The commonest production state — a busy platform that also has a queue.
    #[test]
    fn a_queued_row_holds_back_a_new_run_on_an_already_busy_platform() {
        assert_eq!(
            decide_admission(Occupancy::Parallel { holders: 1 }, 2, false),
            AdmissionDecision::Queue
        );
    }

    // ---------- queue_is_full ----------

    #[test]
    fn an_unlimited_queue_is_never_full() {
        assert!(!queue_is_full(0, None));
        assert!(!queue_is_full(9_999, None));
    }

    #[test]
    fn a_queue_at_or_over_its_limit_is_full() {
        assert!(!queue_is_full(19, Some(20)));
        assert!(queue_is_full(20, Some(20)));
        // Reachable after an operator lowers queue_max_depth below the current
        // depth. Rejecting new launches is correct; queued rows still drain.
        assert!(queue_is_full(25, Some(20)));
    }

    /// The property that keeps the depth rule from breaking today's behaviour:
    /// with a limit of 1 an empty queue is still open, so a launch that
    /// `decide_admission` would dispatch inline is never turned into a 429. Only
    /// the second waiter is rejected (`run_queue.rs:1129-1137`).
    #[test]
    fn a_limit_of_one_rejects_the_second_waiter_but_not_the_first() {
        assert!(!queue_is_full(0, Some(1)));
        assert!(queue_is_full(1, Some(1)));
    }

    /// `depth_limit(0)` is what keeps a disabled setting from rejecting every
    /// launch with a nonsensical "0 of 0 slots used" (`run_queue.rs:1139-1146`).
    #[test]
    fn a_disabled_depth_setting_composes_to_unlimited() {
        assert!(!queue_is_full(0, depth_limit(0)));
        assert!(!queue_is_full(9_999, depth_limit(0)));
        assert_eq!(depth_limit(20), Some(20));
    }

    // ---------- global cap ----------

    #[test]
    fn a_zero_max_disables_the_global_cap() {
        assert_eq!(global_cap_status(5, 0), None);
    }

    #[test]
    fn an_enabled_cap_reports_its_numbers() {
        assert_eq!(
            global_cap_status(3, 6),
            Some(GlobalCap { active: 3, max: 6 })
        );
    }

    #[test]
    fn the_cap_is_reached_at_or_above_the_limit_and_never_when_disabled() {
        assert!(!cap_reached(Some(GlobalCap { active: 5, max: 6 })));
        assert!(cap_reached(Some(GlobalCap { active: 6, max: 6 })));
        assert!(cap_reached(Some(GlobalCap { active: 7, max: 6 })));
        assert!(!cap_reached(None));
    }

    /// The cross-platform budget arithmetic Task 14 owns, made checkable
    /// (`manager/src/services/run_dispatcher.rs:406-414`). One tick, one shared
    /// budget: platform A spends it and platform B must then find none. The
    /// last assertion is the bug the threading exists to prevent — reusing the
    /// un-threaded cap hands B the same two slots A already took.
    #[test]
    fn threading_claims_across_platforms_spends_one_shared_budget() {
        let cap = GlobalCap { active: 4, max: 6 };
        let rows = vec![row(1, false), row(2, false), row(3, false)];

        let platform_a = plan_dispatch_batch(Occupancy::Free, &rows, Some(cap));
        assert_eq!(platform_a.len(), 2, "4 active against a cap of 6 leaves 2");

        let spent = u32::try_from(platform_a.len()).expect("2 fits in u32");
        let threaded = cap.with_claimed(spent);
        assert_eq!(threaded, GlobalCap { active: 6, max: 6 });
        assert!(cap_reached(Some(threaded)));
        assert!(
            plan_dispatch_batch(Occupancy::Free, &rows, Some(threaded)).is_empty(),
            "the budget platform A spent must not be offered to platform B"
        );

        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, Some(cap)).len(),
            2,
            "without threading every platform gets the full budget and the tick \
             overshoots max by (platforms with a queue - 1) x (max - active)"
        );
    }

    // ---------- plan_dispatch_batch ----------

    #[test]
    fn drains_consecutive_parallel_rows_in_one_tick() {
        let rows = vec![row(1, false), row(2, false), row(3, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, None),
            vec![id(1), id(2), id(3)]
        );
    }

    #[test]
    fn stops_at_an_exclusive_row_and_claims_it_alone() {
        let rows = vec![row(1, true), row(2, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, None),
            vec![id(1)]
        );
    }

    /// The one rule that only holds if occupancy is extended as we go: an
    /// exclusive row claimed this tick becomes occupancy, so everything behind
    /// it waits for the next tick (`run_queue.rs:1332-1341`). A queue of
    /// exclusive rows therefore drains one per tick (guide lines 102-104).
    #[test]
    fn an_exclusive_row_claimed_this_tick_blocks_the_rows_behind_it() {
        let rows = vec![row(1, true), row(2, false), row(3, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, None),
            vec![id(1)],
            "the exclusive row is claimed alone; the two parallel rows behind it wait"
        );
    }

    #[test]
    fn claims_parallel_rows_before_an_exclusive_row_but_not_the_exclusive_one() {
        let rows = vec![row(1, false), row(2, false), row(3, true), row(4, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Free, &rows, None),
            vec![id(1), id(2)]
        );
    }

    #[test]
    fn claims_nothing_while_an_exclusive_run_still_occupies_the_platform() {
        let rows = vec![row(1, false), row(2, false)];
        assert!(plan_dispatch_batch(Occupancy::Exclusive, &rows, None).is_empty());
    }

    #[test]
    fn claims_parallel_rows_onto_a_platform_busy_with_parallel_runs() {
        let rows = vec![row(1, false)];
        assert_eq!(
            plan_dispatch_batch(Occupancy::Parallel { holders: 1 }, &rows, None),
            vec![id(1)]
        );
    }

    /// A queued exclusive head must not be overtaken by the row behind it —
    /// the drain half of "the queue is strictly FIFO" (`run_queue.rs:1364-1372`).
    #[test]
    fn holds_an_exclusive_head_while_any_run_is_still_active() {
        let rows = vec![row(1, true), row(2, false)];
        assert!(
            plan_dispatch_batch(Occupancy::Parallel { holders: 1 }, &rows, None).is_empty(),
            "a queued exclusive head must not be overtaken by the row behind it"
        );
    }

    #[test]
    fn respects_the_global_concurrency_cap() {
        let rows = vec![row(1, false), row(2, false), row(3, false)];
        // 4 active cluster-wide, cap 6 -> room for exactly 2 more.
        assert_eq!(
            plan_dispatch_batch(
                Occupancy::Free,
                &rows,
                Some(GlobalCap { active: 4, max: 6 })
            ),
            vec![id(1), id(2)]
        );
    }

    #[test]
    fn a_zero_cap_means_unlimited() {
        let rows = vec![row(1, false), row(2, false)];
        assert_eq!(
            plan_dispatch_batch(
                Occupancy::Free,
                &rows,
                Some(GlobalCap { active: 99, max: 0 })
            ),
            vec![id(1), id(2)]
        );
    }

    #[test]
    fn a_reached_cap_claims_nothing() {
        let rows = vec![row(1, false), row(2, false)];
        assert!(
            plan_dispatch_batch(
                Occupancy::Free,
                &rows,
                Some(GlobalCap { active: 6, max: 6 })
            )
            .is_empty()
        );
    }

    /// `active` is cluster-wide and already counts this platform's occupants,
    /// so occupancy must NOT be added to the cap arithmetic a second time.
    /// Without this test, a variant that adds the holder count to the budget
    /// passes every other test here (`run_queue.rs:1396-1410`).
    #[test]
    fn the_cap_counts_only_new_claims_not_existing_occupancy() {
        let rows = vec![row(1, false), row(2, false), row(3, false)];
        assert_eq!(
            plan_dispatch_batch(
                Occupancy::Parallel { holders: 1 },
                &rows,
                Some(GlobalCap { active: 4, max: 6 })
            ),
            vec![id(1), id(2)],
            "4 active cluster-wide against a cap of 6 leaves room for 2 more, \
             regardless of how many runs already occupy this platform"
        );
    }

    // ---------- TTL arithmetic ----------

    #[test]
    fn a_zero_ttl_disables_expiry() {
        assert_eq!(expiry_cutoff(at(12), 0), None);
        assert_eq!(ttl_expires_at(at(10), 0), None);
    }

    #[test]
    fn a_positive_ttl_yields_a_cutoff_in_the_past_and_a_deadline_in_the_future() {
        assert_eq!(expiry_cutoff(at(12), 7200), Some(at(10)));
        assert_eq!(ttl_expires_at(at(10), 7200), Some(at(12)));
    }

    /// An absurd TTL must disable expiry rather than panic. Failing open is
    /// the safe direction: the alternative is a panicking dispatcher tick,
    /// and a row that never expires is merely stale, not destructive
    /// (`run_queue.rs:1002-1015`).
    #[test]
    fn an_absurd_ttl_disables_expiry_rather_than_panicking() {
        let now = at(12);
        assert_eq!(expiry_cutoff(now, u64::MAX), None);
        assert_eq!(ttl_expires_at(now, u64::MAX), None);
        // A TTL that fits in i64 but not in the date range: the case the
        // checked arithmetic exists to catch. Without this the u64::MAX case
        // short-circuits earlier and the second guard is never exercised.
        assert_eq!(expiry_cutoff(now, 1_000_000_000_000_000), None);
        assert_eq!(ttl_expires_at(now, 1_000_000_000_000_000), None);
    }

    // ---------- describe_blocker ----------

    #[test]
    fn a_row_behind_others_is_blocked_by_the_rows_ahead() {
        assert_eq!(
            describe_blocker(4, &[]),
            "waiting for 3 queued runs ahead of it"
        );
        assert_eq!(
            describe_blocker(2, &[]),
            "waiting for 1 queued run ahead of it"
        );
    }

    #[test]
    fn the_head_names_the_single_run_holding_the_platform() {
        let holders = vec![Some("upgrade-7".to_owned())];
        assert_eq!(describe_blocker(1, &holders), "waiting for run upgrade-7");
    }

    #[test]
    fn the_head_lists_every_run_holding_the_platform() {
        let holders = vec![Some("smoke-1".to_owned()), Some("smoke-2".to_owned())];
        assert_eq!(
            describe_blocker(1, &holders),
            "waiting for 2 active runs (smoke-1, smoke-2)"
        );
        // A claim mid-build has no name yet; the placeholder must not be
        // mistakable for a run name. This arm is otherwise uncovered.
        let mixed = vec![Some("smoke-1".to_owned()), None];
        assert_eq!(
            describe_blocker(1, &mixed),
            "waiting for 2 active runs (smoke-1, (not yet started))"
        );
    }

    #[test]
    fn a_claim_without_a_name_yet_is_described_as_starting() {
        assert_eq!(
            describe_blocker(1, &[None]),
            "waiting for a run that is still starting"
        );
    }

    /// The honest answer when occupancy is not represented by a claim row.
    /// The text asserts nothing about the platform — an earlier legacy version
    /// said "waiting for the platform to be free", which is false in the
    /// global-cap case — and above all it never invents a run name
    /// (`run_queue.rs:1074-1084`).
    #[test]
    fn a_head_with_no_claim_names_no_run() {
        assert_eq!(describe_blocker(1, &[]), "waiting for its turn");
        // Positions at or below zero are unreachable from the listing but must
        // fall into the head branch rather than underflow.
        assert_eq!(describe_blocker(0, &[]), "waiting for its turn");
    }

    // ---------- assign_positions ----------

    /// Positions are 1-based, per platform, oldest queued row first, and only
    /// `queued` rows get one (`run_queue.rs:485-507`).
    #[test]
    fn positions_are_per_platform_and_oldest_first() {
        let plat_a = id(0xA);
        let plat_b = id(0xB);
        let rows = vec![
            PositionInput {
                id: id(3),
                platform_id: plat_a,
                enqueued_at: at(12),
                queued: true,
            },
            PositionInput {
                id: id(1),
                platform_id: plat_a,
                enqueued_at: at(10),
                queued: true,
            },
            PositionInput {
                id: id(2),
                platform_id: plat_b,
                enqueued_at: at(11),
                queued: true,
            },
            PositionInput {
                id: id(4),
                platform_id: plat_a,
                enqueued_at: at(9),
                queued: false,
            },
        ];
        let positions = assign_positions(&rows);
        assert_eq!(positions.get(&id(1)), Some(&1));
        assert_eq!(positions.get(&id(3)), Some(&2));
        assert_eq!(
            positions.get(&id(2)),
            Some(&1),
            "platform B numbers independently"
        );
        assert_eq!(
            positions.get(&id(4)),
            None,
            "a non-queued row has no position"
        );
    }

    /// Ties on `enqueued_at` break on `id`, matching the FIFO `ORDER BY`
    /// (`run_queue.rs:248`) so positions and drain order cannot disagree.
    #[test]
    fn ties_on_enqueue_time_break_on_id() {
        let plat = id(0xA);
        let rows = vec![
            PositionInput {
                id: id(2),
                platform_id: plat,
                enqueued_at: at(10),
                queued: true,
            },
            PositionInput {
                id: id(1),
                platform_id: plat,
                enqueued_at: at(10),
                queued: true,
            },
        ];
        let positions = assign_positions(&rows);
        assert_eq!(positions.get(&id(1)), Some(&1));
        assert_eq!(positions.get(&id(2)), Some(&2));
    }
}
