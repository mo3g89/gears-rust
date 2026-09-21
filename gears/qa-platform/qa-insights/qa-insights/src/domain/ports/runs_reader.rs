//! The qa-runs reads, behind a port.
//!
//! # Why a port and not the SDK client directly
//!
//! `qa_runs_sdk::QaRunsClientV1` has **seventeen** methods —
//! `qa-runs-sdk/src/client.rs:24-200`, counted 2026-08-21. This header said
//! nineteen from Task 13 until Task 18 and was simply wrong; the argument it
//! makes does not depend on the digit, but a wrong count next to a "counted, not
//! eyeballed" register is the kind of claim this gear has already had to fix
//! three times.
//!
//! This gear reads through **five** of them. Three sit on the ingest path — the
//! path whose tests have to be able to produce a run that finished at a chosen
//! instant with a chosen set of results — the fourth,
//! [`RunsReader::list_recent_runs`], is Task 18's dashboard reading live run
//! state, and the fifth, [`RunsReader::get_schedule_notifications`], is Task
//! 38's: the per-schedule notification settings `domain::notify::routing::route`
//! needs, which `qa_runs_sdk::Run` does not carry (only `schedule_id` does).
//! A fake of the whole client would be twelve `unimplemented!()`s around the
//! five that matter, and the next task to add a read would not be told to
//! think about it.
//!
//! # R105 — a port grows in the task that consumes it
//!
//! `get_schedule_notifications` was added by Task 38 itself, in its own fix
//! round, rather than deferred to Task 40's wiring pass. Controller ruling
//! R105 names the precedent this follows: Task 35's `RunsLauncher::launch_test`
//! and `EnvironmentReader::default_branch` both grew in the task that consumed
//! them, each with its adapter and its own test, attached to their only
//! caller. Task 38 is `route()`'s only caller in this crate, so it is the
//! task with a routing test anywhere near a schedule-settings read.
//!
//! Naming exactly the reads this gear performs also makes the cross-gear
//! surface greppable: `RunsReader` is the complete list of what qa-insights
//! asks qa-runs for, and it is four methods long.
//!
//! **It grows only for a read a test in this crate exercises.** Task 18 needed
//! one listing and added one; it did **not** add `list_queue`, even though the
//! dashboard reports a queued count, because that count comes off the same
//! `list_recent_runs` page — see [`RunsReader::list_recent_runs`].
//!
//! # Errors cross the boundary as [`DomainError`]
//!
//! `QaRunsError` is qa-runs' vocabulary, and a `not_found` from it means "the
//! run does not exist **or** is not visible to this context" — the two are
//! deliberately indistinguishable there, which is what closes the cross-tenant
//! existence oracle. The adapter is what translates; the port speaks this
//! gear's error type so no domain code has to learn a sibling's.
//!
//! # The context is the caller's, and on the ingest path that is a system actor
//!
//! Every method takes a `SecurityContext` rather than deriving one, because
//! tenancy on the far side is enforced against *that* context. The reconcile
//! sweep builds a tenant-bound system actor from the tenant it was asked to
//! sweep ([`crate::domain::system_actor`]); a request-scoped caller passes its
//! own.
//!
//! **Task 16 is the first of those, and Task 18 is the second**, and it is why
//! the sentence above was written in the conditional and is now not:
//! [`ReconcileService::rebuild`](crate::domain::service::reconcile::ReconcileService::rebuild)
//! lists under the *operator's* context, so the set of runs a rebuild can reach
//! is the set qa-runs is willing to show that operator. Its per-run re-read
//! still goes through a system actor, which is a real asymmetry with a bound: it
//! can only re-read a run the caller's own listing already returned. **That
//! actor is `system_actor::for_operator_rebuild` as of Phase A's whole-phase
//! review, not the ingest one this sentence used to name** — the sweep and the
//! rebuild had both been minting the consumer's identity, which
//! [`crate::domain::system_actor`]'s header now records in full.
//! See that method's header, decision 4.
//!
//! The adapter is [`crate::infra::clients::QaRunsReader`].

use async_trait::async_trait;
use qa_runs_sdk::{FinishedRunCursor, Run, RunTestResult, ScheduleNotificationSettings};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The reads qa-insights performs against qa-runs.
#[async_trait]
pub trait RunsReader: Send + Sync {
    /// One run's metadata.
    ///
    /// The source of every denormalized column on `qa_test_results`:
    /// `environment_id`, `product_version` (from `Run::app_version`), `app_build`,
    /// `repo_id` and `plan_path` (from `Run::target`), `branch` (from
    /// `Run::test_version`) and `run_finished_at`. Ingest reads it once per
    /// projected run rather than once per result row.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotIngested`] when qa-runs answers `not_found`, which
    /// covers both "no such run" and "not visible to `ctx`". The projection
    /// treats that as "nothing to project" rather than as a failure — see
    /// [`crate::domain::service::ingest`].
    async fn get_run(&self, ctx: &SecurityContext, run_id: Uuid) -> Result<Run, DomainError>;

    /// Every per-test row qa-runs holds for one run, in the producer's order.
    ///
    /// **The order is load-bearing and is not this port's to change.**
    /// `ResultsRepository::upsert_run_results` derives
    /// `qa_test_results.ingest_ordinal` from the position of each row in the
    /// batch it is handed, and that ordinal is the within-run half of the
    /// latest-wins tiebreak. Re-sorting these rows anywhere between here and
    /// the repository would silently invert it, with nothing failing.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotIngested`] on `not_found`, as above;
    /// [`DomainError::Internal`] for any other transport or gateway failure.
    async fn list_run_test_results(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Vec<RunTestResult>, DomainError>;

    /// Runs whose `finished_at` is at or after `since`, **oldest first**,
    /// capped at `limit`.
    ///
    /// Task 15's reconcile sweep. The gear-split's own invention: legacy reads
    /// analytics straight off the database the run path writes, so it has
    /// nothing to reconcile and there is no endpoint to port. Do not go looking
    /// for one.
    ///
    /// # Oldest-first is load-bearing, not a convenience
    ///
    /// The sweep advances a watermark as it consumes the page and stops at the
    /// first run it fails to backfill. Newest-first would make it advance past
    /// a gap it never filled, which is the one outcome the watermark exists to
    /// prevent.
    ///
    /// # The lower bound, and the keyset half of it
    ///
    /// [`FinishedRunCursor::starting_at`] is `finished_at >= at`, inclusive.
    /// An inclusive bound can re-deliver the run sitting exactly on the
    /// watermark; re-delivering one run is the cheap failure, because the
    /// backfill is idempotent. An exclusive bound would drop a run that
    /// finished in the same clock tick as the watermark, which is the
    /// expensive one. A run with a `NULL` `finished_at` — anything not yet
    /// terminal — is never returned, which is why the sweep cannot reconcile
    /// an in-progress run at all; see
    /// [`crate::domain::service::reconcile`] for what that costs.
    ///
    /// [`FinishedRunCursor::after`] is `(finished_at, id) > (at, run_id)`,
    /// strictly, under the very order this listing is sorted by. That is
    /// what lets the sweep step over a group of runs sharing one instant that
    /// is wider than its own page — the shape that stranded three days of
    /// results on the dev stand, where single instants were shared by 54, 40,
    /// 31 and 29 runs. See
    /// [`ReconcileService::sweep`](crate::domain::service::reconcile::ReconcileService)
    /// for the walk that uses it.
    ///
    /// The two halves are one parameter, and that is a guard rather than
    /// tidiness: this listing is delegated through four layers, and at each of
    /// them keeping the instant while dropping the id compiled, read like a
    /// legitimate first page, and silently restored the bound that caused the
    /// outage. [`FinishedRunCursor`] carries the whole argument.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] for any transport or gateway failure. There is
    /// no not-found case: an empty window is an empty `Vec`.
    async fn list_runs_finished_since(
        &self,
        ctx: &SecurityContext,
        cursor: FinishedRunCursor,
        limit: u32,
    ) -> Result<Vec<Run>, DomainError>;

    /// The most recent runs, **newest first**, capped at `limit`, in **every**
    /// state.
    ///
    /// Task 18's dashboard. `qa_runs_sdk::QaRunsClientV1::list_runs`
    /// (`qa-runs-sdk/src/client.rs:46`) verbatim: same order, same mandatory
    /// limit, no state predicate.
    ///
    /// # Why one unfiltered listing, and not one call per number
    ///
    /// The dashboard's total, its active count, its queued count, its active
    /// list and its recent list all come off this one page. That is legacy's own
    /// shape and legacy states the reason in a comment at
    /// `manager/src/routes/dashboard.rs:143-146`: *"Derive everything we need
    /// from the single `runs` listing … A second `list_runs_with_history` call
    /// (or a separate `list_workflows`) costs an extra kube round-trip and
    /// ~500ms of latency for no new information."* Here the equivalent cost is a
    /// cross-gear SDK call rather than a kube round-trip, which is if anything
    /// the stronger version of the same argument.
    ///
    /// In particular the queued count is **not** `QaRunsClientV1::list_queue`.
    /// That method answers with `QueueEntry` rows in a different vocabulary on a
    /// different table (`QueueState`, down to the two-`l` `Cancelled`), while
    /// what the dashboard reports is how many *runs* are in
    /// `qa_runs_sdk::RunState::Queued` — which this page already carries, and
    /// which `cpt-cf-qa-principle-db-first-state` makes qa-runs' authoritative
    /// answer.
    ///
    /// # `limit` is mandatory upstream and is not defaulted here
    ///
    /// The SDK's own doc gives the reason — *"`qa_runs` grows strictly faster
    /// than the queue and never drains, so an unbounded inter-gear call would
    /// materialize every run ever executed"* — so there is no "all runs" read to
    /// expose and the caller must own the ceiling. The consequence for the
    /// dashboard's counts is stated where the ceiling is chosen, in
    /// [`crate::domain::service::dashboard`].
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] for any transport or gateway failure, and
    /// [`DomainError::Forbidden`] when qa-runs refuses the *subject*. There is
    /// no not-found case: an empty deployment is an empty `Vec`.
    async fn list_recent_runs(
        &self,
        ctx: &SecurityContext,
        limit: u32,
    ) -> Result<Vec<Run>, DomainError>;

    /// The per-schedule notification settings for `schedule_id` — the three
    /// fields `domain::notify::routing::route` needs
    /// (`ScheduleNotificationSettings::slack_enabled`, `::slack_channel`,
    /// `::slack_events`), read off `qa_runs_sdk::Schedule` over
    /// `QaRunsClientV1::get_schedule` (`qa-runs-sdk/src/client.rs:156`).
    ///
    /// Task 38's only caller,
    /// [`crate::domain::service::notify::NotifyService::notify_run_completed`],
    /// needs this because `qa_runs_sdk::Run` carries only `schedule_id: Option<Uuid>`
    /// — the settings themselves live on the schedule, not denormalized onto
    /// the run.
    ///
    /// # `None` covers "no such schedule" **and** "not visible to `ctx`"
    ///
    /// [`Self::get_run`]'s own reason: qa-runs' `not_found` means both, and the
    /// two are deliberately indistinguishable so a cross-tenant probe learns
    /// nothing from either the answer or the timing. Unlike [`Self::get_run`],
    /// this does **not** become a `DomainError` — there is no
    /// `ScheduleNotIngested` variant, because a schedule that cannot be
    /// resolved is not this method's caller's failure the way an unresolvable
    /// *run* would be: [`NotifyService::notify_run_completed`]'s own doc
    /// decides what an absent schedule means for routing (a schedule that
    /// cannot be read is treated exactly like no schedule at all), and that
    /// decision belongs one layer up, not folded into this port's return type.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] for `PermissionDenied`/`Unauthenticated` — a
    /// decision about the **subject**, not about whether this one schedule
    /// exists, so it is not folded into `None` the way `NotFound` is.
    /// [`DomainError::Internal`] for any other transport or gateway failure.
    async fn get_schedule_notifications(
        &self,
        ctx: &SecurityContext,
        schedule_id: Uuid,
    ) -> Result<Option<ScheduleNotificationSettings>, DomainError>;
}
