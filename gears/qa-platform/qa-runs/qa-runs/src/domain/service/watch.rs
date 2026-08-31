//! Attaching a result observer to a live run — the thing that drives
//! [`super::ingest::IngestService`].
//!
//! Until this module landed, `IngestService::apply` had no production caller and
//! `RunExecutor::watch` had no non-test caller, so a run this gear dispatched
//! received no results at all and reached a terminal state **only** through the
//! control-plane timeout sweep. Every per-test row, every verdict derivation and
//! the whole of the ingest path was live code with no entry point.
//!
//! # The seam, and why it is a seam rather than a call
//!
//! [`super::dispatch::DispatchService`] cannot call [`super::ingest::IngestService`]
//! directly without a service-to-service dependency, and it does not have to:
//! [`super::launch::InlineDispatcher`] is the same pattern already — a trait
//! declared in the composition tier, implemented by a sibling service, wired by
//! [`super::AppServices::new`] with an injectable override. [`RunWatcher`]
//! follows it, and the override is what makes the pass testable without a
//! database, four cross-gear clients and a real executor.
//!
//! # Why this lives in `domain::service` and not in `infra`
//!
//! Task 16c's plan text puts the registry and the spawning implementation in
//! `infra/`. It cannot go there. [`super::AppServices::new`] is the only point
//! in the program where the ingest service exists and the dispatch service does
//! not yet, so it is the only place the default can be constructed — and it is
//! in the domain layer, which may not name an infrastructure type.
//!
//! **Corrected 2026-08-15, after review, and the correction is not a
//! softening.** This paragraph's evidence sentence read *"no production module
//! under `domain/` has a `use crate::infra`, and that is the rule being kept"*.
//! Both halves are false. `domain/local_client/client.rs` has
//! `use crate::infra::ConcreteAppServices;` and is a production module —
//! `domain/mod.rs` declares it `pub mod local_client;` with no `cfg` — and
//! `domain/mod.rs` states no layering rule at all. The sentence was written to
//! support a decision already made, which is the shape this project keeps
//! producing, and it was found by two reviewers independently.
//!
//! **What is actually in the tree**, in place of the rule that was invented:
//!
//! * [`super::LogFanout`]'s doc states the rule this module obeys — *"no domain
//!   signature may name an infrastructure type"* — which is about signatures.
//! * `infra::ConcreteAppServices`' own doc states the stronger direction —
//!   *"the domain layer could not hold it … which is the dependency direction
//!   this crate's layering forbids"* — and `domain::local_client` contradicts
//!   that sentence by holding exactly that alias. Whether `local_client` is a
//!   sanctioned exception is not recorded anywhere I could find; it is named
//!   here so the next reader does not have to rediscover it.
//!
//! So the placement rests on the argument above it — the construction point —
//! and not on a layering rule this crate uniformly keeps, because it does not.
//! The plan has since retracted the `infra/` directive on an independent ground:
//! it contradicted the same section's directive to mirror
//! [`super::launch::InlineDispatcher`], which is declared in
//! `domain/service/launch.rs` and implemented by a domain service.
//!
//! The two alternatives were considered and are worse:
//!
//! * **A factory port** — a domain port whose only job is to build another
//!   domain port, with an `infra` implementation. Three types where one does the
//!   work, and the third is untestable in isolation.
//! * **Late binding** — construct the watcher in `gear.rs` after
//!   `AppServices::new` returns and hand it the ingest service then. That is
//!   *exactly* the shape `gear::LogWiring` exists to make unspellable: a wiring
//!   step that can be omitted, whose omission produces a gear that boots clean,
//!   dispatches runs, and observes nothing, with no error and no warning.
//!
//! # `tokio::spawn` in a domain module, which the cited precedent does *not*
//! cover
//!
//! What this module names is a port ([`RunExecutor`]), a domain service and a
//! domain context factory — no infrastructure type. The `tokio` dependency is
//! the part that needs an argument, and the first version of this paragraph
//! borrowed one that does not stretch: `domain::ports::run_executor` justifies
//! its `tokio::sync::mpsc` import on the grounds that *"`tokio::sync` is not
//! among [the forbidden paths], and a **channel** is a language-level primitive
//! rather than an integration"*. `tokio::spawn` is neither `tokio::sync` nor a
//! channel, and it **requires an ambient runtime**, which is precisely the
//! property that puts it in a different class. The citation resolves; it does
//! not say what it was cited for.
//!
//! The argument that does hold is narrower and is stated as such.
//! `toolkit_macros::domain_model`'s field check bans the crates `sqlx`,
//! `sea_orm`, `http`, `axum`, `hyper`, `reqwest` and `tonic` plus the path
//! prefixes `std::fs` and `tokio::fs` (`libs/toolkit-macros/src/domain_model.rs`,
//! `FORBIDDEN_CRATES` and `FORBIDDEN_PATH_PREFIXES` — it has a third list, of
//! database-specific type names, which is not relevant here). `tokio::task` is
//! on none of them, and the async runtime is not a new dependency of this
//! layer: `domain::service` is `async` throughout — every repository call,
//! policy-decision round trip and cross-gear read is `.await`ed — and
//! [`super::admission`] already holds a `tokio::sync::Mutex`. So this is a *new
//! kind* of `tokio` use in the domain layer rather than a new dependency.
//!
//! **Not "every service method here is `async`"** — the first draft of this
//! sentence said that, and it is falsified a few lines below by [`RunWatcher`],
//! whose two methods are synchronous *by contract*, as [`super::LogFanout`]'s
//! are and for the same reason. What is already async is the I/O, which is what
//! the argument needs.
//!
//! **The real cost is the ambient-runtime requirement**, which is the property
//! that makes it a different class from a channel:
//! [`SpawningRunWatcher::attach`] panics if called outside a Tokio runtime, and
//! nothing in [`RunWatcher`]'s signature says so. Its one production caller is
//! inside `run_tick`, which the gear runs on the runtime it was spawned from,
//! and the tests that reach it are `#[tokio::test]`.
//!
//! **A related claim, corrected where it lives**: `run_executor`'s paragraph
//! also says its `mpsc` import is *"the only `tokio` path in this gear's whole
//! domain layer"*. That was already false before this task —
//! `domain::service::admission` imports `tokio::sync::Mutex` for
//! [`super::admission::PlatformLocks`] — and this module adds a third. The
//! sentence is corrected in `run_executor.rs` rather than only here.
//!
//! # One producer per run
//!
//! One producer per run is no longer what keeps the ingest races closed — Task
//! 16b step 1 closed those at the isolation layer, so two producers now conflict
//! and one retries rather than corrupting the rows. It still matters for cost
//! and for duplicate work. Within a process it is structural — see [`WatchRegistry`]. Across processes
//! it is the leader election's, and **under the shipped `NoopLeaderElector` it
//! does not hold**: `run_role` calls the work unconditionally, [`WatchRegistry`]
//! is a per-process `HashSet` that consults no database, and
//! `RunsRepository::list_watch_candidates` is neither claim-gated nor
//! leader-gated, so it returns the same rows to every replica and each opens its
//! own observer. That is the same pre-existing hazard `infra::leader` already
//! documents for `recover_after_boot` and the claim-scan cursor, with the same
//! fix — a real elector.
//!
//! **What N observers cost.** The three ways this used to corrupt stored state —
//! duplicate per-test rows, a double-counted result, and the completion's
//! phantom read — were closed by Task 16b step 1, which escalated
//! `IngestService::record_one_result` and `IngestService::finish` to
//! `SERIALIZABLE`. That module carries the mechanism; this one owns saying that
//! a multi-replica deployment is what brings the second producer.
//!
//! What N observers still cost is **not** nothing:
//!
//! 1. **Duplicated work and contention.** N observers apply the same event N
//!    times, and under `SERIALIZABLE` the losers abort and retry rather than
//!    committing. `qa_run_test_results` still carries no unique index — the
//!    migration is explicit that *"the uniqueness is an application invariant,
//!    not a constraint"* — so what keeps one logical test to one row is now the
//!    isolation level rather than the schema.
//! 2. **An exhausted retry budget ends the whole observation.**
//!    `IngestService::ingest` stops on the first failing event, so a run whose
//!    observer loses too many times reaches a terminal state only through the
//!    timeout sweep. More observers means more contention means more of this,
//!    which is the sharpest reason to want a real elector.
//!
//! **There is exactly one attach path**, [`super::dispatch::DispatchService`]'s
//! leader-gated re-attachment pass. `record_started` deliberately does not
//! attach, and the reason is not latency: it has **no reliably correct tenant,
//! and no discriminator to tell which case it is in**.
//!
//! `qa_runs_sdk::Run` carries no `tenant_id`, so the only tenant available at
//! that call site is `ctx.subject_tenant_id()`, and `IngestService` stamps that
//! value into `qa_run_test_results.tenant_id`. Whether it is the run's own
//! tenant depends on which of three callers reached `dispatch_one`:
//!
//! * **The tick's drain** — correct by construction. `drain_platform` mints
//!   `system_actor::for_dispatch(tenant)` from `QueuedPlatform::tenant_id`,
//!   which `platforms_with_queued_rows` groups straight out of the queue rows;
//!   and that column and the run row's are stamped from the *same*
//!   `ctx.subject_tenant_id()` by the same launch — `admission::admit`'s
//!   `queue.insert` and `launch`'s `runs.create` — so they are equal.
//! * **An inline launch** — correct by construction, for the second half of the
//!   same reason: the run row was created under this very context.
//! * **Force start** — the one that can diverge. `RunsService` passes the
//!   operator's own context straight through, and an operator whose compiled
//!   scope admits several tenants can force-start a row that is not their own.
//!   That a scope can admit a set rather than one tenant is what
//!   `service::admission`'s `queued_depth` note records (*"under a hierarchical
//!   policy whose scope admits several tenants"*) — cited for that, and not for
//!   the neighbouring proposition about `subject_tenant_id`, which it does not
//!   state.
//!
//! **A categorical "it never has the right tenant" is false and must not be
//! written**; the first version of this paragraph wrote it. What makes the
//! narrower form still decisive is the absence of a discriminator:
//! `record_started` sees `Some(queue_id)` on the tick path *and* on the force
//! start path, so it cannot tell the two apart. It could attach safely on the
//! `None` arm alone — the platformless inline launch — which would be a partial
//! attach path that fires for one run shape and not the others, and that is
//! worse than none.
//!
//! The sweep reads the tenant off the row, which is the rule every other
//! background pass in this gear already follows.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use toolkit_macros::domain_model;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::ingest::IngestService;
use crate::domain::ports::run_executor::{ExecutionRef, RunExecutor};
use crate::domain::repos::{QueueRepository, RunsRepository};
use crate::domain::system_actor::{self, TenantBound};

/// One run to observe, with the tenant every write about it must be bound to.
///
/// [`TenantBound`] rather than a `Uuid`, so a nil tenant cannot reach the ingest
/// path at all: the type's only constructor refuses it, and a context built from
/// nil is the cross-tenant enumeration identity this gear uses for reads and
/// never for writes.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchTarget {
    pub run_id: Uuid,
    pub tenant: TenantBound,
    pub execution_ref: ExecutionRef,
}

/// Attach an observer to a run's execution.
///
/// **Both methods are synchronous and infallible, which is the contract rather
/// than an implementation detail** — the same rule [`super::LogFanout`] states,
/// arrived at the same way. A dispatcher pass must never block on, or fail
/// because of, whether an observer could be attached: the run is already
/// executing, and the pass that would have to handle the error is the one whose
/// job is to notice unobserved runs and try again next tick.
pub trait RunWatcher: Send + Sync {
    /// Observe `target`'s execution, unless this process already is.
    ///
    /// Idempotent by run id: asking twice attaches once. See [`WatchRegistry`]
    /// for why that is structural rather than a check somebody has to remember.
    fn attach(&self, target: WatchTarget);

    /// Whether this process currently has an observer on `run_id`.
    ///
    /// Exists so the re-attachment pass can skip a run before paying for a
    /// policy decision and a scoped read, which is what makes a per-tick sweep
    /// over every live run affordable. It is **advisory**: an observer can end
    /// between this answer and the [`Self::attach`] that follows, and that is
    /// harmless because `attach` re-decides under the registry's own lock.
    fn is_watching(&self, run_id: Uuid) -> bool;
}

/// The run ids this process is observing.
///
/// # The double-attach is not spellable, rather than merely untested
///
/// Two observers on one run would give `IngestService` two producers. That no
/// longer corrupts what is stored — Task 16b step 1 closed the ingest races at
/// the isolation layer — but it is still duplicated work, and every duplicate is
/// a transaction the other one has to abort and retry against. The protection is
/// not a check before the spawn — it is that **the only way to obtain the right
/// to spawn is to have won the insert**. [`Self::claim`] returns
/// `Some(AttachedSlot)` exactly once per run id, and [`SpawningRunWatcher::attach`]
/// has no path to `tokio::spawn` that does not go through it. A second caller
/// gets `None` and there is nothing for it to do with that.
///
/// The release is the slot's `Drop`, not a line at the end of the drain, so a
/// task that returns early — an unreachable executor, an ingest failure — or
/// panics still frees the run for a later attempt. A hand-written release at the
/// bottom of the drain would leak the slot on every one of those paths and the
/// run would then never be observed again, which is worse than the defect this
/// registry exists to prevent because it is silent and permanent.
#[derive(Clone, Default)]
struct WatchRegistry(Arc<Mutex<HashSet<Uuid>>>);

impl WatchRegistry {
    /// Take the right to observe `run_id`, or `None` if it is already taken.
    fn claim(&self, run_id: Uuid) -> Option<AttachedSlot> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(run_id)
            .then(|| AttachedSlot {
                run_id,
                registry: self.clone(),
            })
    }

    fn holds(&self, run_id: Uuid) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&run_id)
    }
}

/// The right to observe one run, released when the observer ends.
struct AttachedSlot {
    run_id: Uuid,
    registry: WatchRegistry,
}

impl Drop for AttachedSlot {
    fn drop(&mut self) {
        self.registry
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.run_id);
    }
}

/// The production [`RunWatcher`]: one task per observed run, draining
/// `RunExecutor::watch` into `IngestService`.
///
/// # The number of observers is **not bounded**, and that is new
///
/// Stated because it is a resource question this task introduces and nothing
/// else in the gear discusses. Before it, a live run cost the control plane no
/// in-process resources at all; now each costs a detached task holding an
/// `ExecutionStream` `mpsc` for as long as the run executes, which
/// `cpt-cf-qa-nfr-run-duration` puts at up to eight hours, plus an entry in
/// [`WatchRegistry`]'s `HashSet`.
///
/// What does *not* bound it:
///
/// * `MAX_WATCH_SCAN` bounds the **rate** of attachment — how many observers one
///   tick may start — and its own doc claims only that. It says nothing about
///   the total.
/// * `max_concurrent_runs` bounds live runs, but
///   [`crate::domain::queue::global_cap_status`] records `0` — *no cap* — as the
///   shipped default.
/// * `queue_max_depth` bounds queued rows per (scope, platform) and does not
///   reach a platformless run at all, which is never queued.
///
/// So the ceiling in a default deployment is however many runs are executing.
/// Left unbounded rather than capped: a cap here would have to decide *which*
/// live run goes unobserved, and an unobserved run is one that can only end at
/// its deadline — which is the failure this whole module exists to remove. The
/// honest form is to say the resource is unbounded and where it would be
/// bounded, not to invent a limit whose overflow behaviour is worse than the
/// resource.
pub struct SpawningRunWatcher<R, Q> {
    executor: Arc<dyn RunExecutor>,
    ingest: Arc<IngestService<R, Q>>,
    attached: WatchRegistry,
}

impl<R, Q> SpawningRunWatcher<R, Q> {
    pub(in crate::domain::service) fn new(
        executor: Arc<dyn RunExecutor>,
        ingest: Arc<IngestService<R, Q>>,
    ) -> Self {
        Self {
            executor,
            ingest,
            attached: WatchRegistry::default(),
        }
    }
}

/// Drain one execution's events into the ingest path.
///
/// # Neither ending retires the run, and that is the port's contract
///
/// `RunExecutor`'s trait doc is explicit on both, and both are easy to get
/// wrong in the same direction:
///
/// * **The stream ends with no `Finished`.** An empty or truncated stream is
///   *"nothing more to say"*, **never** *"this failed"* — it is also exactly
///   what a finished-and-forgotten execution yields. So this returns, the slot
///   is released, and the next sweep re-attaches. A run whose executor has
///   genuinely forgotten it is then re-watched once per tick until the
///   control-plane timeout sweep reclaims it, which is a bounded cost paid for
///   never inventing a verdict.
/// * **`watch` errored.** Nothing is known. Aliveness is `list_active`'s
///   question alone, and treating an unreachable executor as a dead execution is
///   precisely what would release a claim during an outage and let a second run
///   start beside an exclusive one.
///
/// An ingest failure stops the drain, because `IngestService::ingest` stops on
/// the first failing event by contract — a `Forbidden` would otherwise become a
/// wall of identical log lines, and results would keep being recorded for a run
/// whose completion can no longer be written.
///
/// # "Retires nothing" is structural, and neither ending is a *guard*
///
/// Stated precisely, because the first version of this comment read as though
/// the two arms above were guards and they are not. **Break-tested**: replacing
/// the `watch`-error arm's `return` with an empty stream — so the errored case
/// falls through into `ingest` exactly as a forgotten execution does — left
/// `an_unreachable_executor_retires_nothing` green. Nothing was pinned about
/// **run state**, because there was nothing to pin.
///
/// The first version then said the two paths were *"observationally
/// identical"*, and that is false in the one dimension that survives: the
/// mutant loses the ERROR line carrying `%error` and emits the INFO *"the
/// observation of an execution ended"* for an execution nobody could reach —
/// which actively misleads the operator reading it. This same function's
/// `cognitive_complexity` allow-reason calls exactly that load-bearing (*"three
/// different operator stories and each owes its own line"*), so the precise
/// form is: **not a guard on run state; still load-bearing for
/// observability**, and that half *is* pinned —
/// `an_unreachable_executor_is_reported_as_unreachable_not_as_ended` asserts
/// the two lines through `tracing_test`, which is what turns "the mutant
/// misleads an operator" from a claim into a failing test.
///
/// What actually holds the property is the shape of this function: it is handed
/// an executor and an ingest service and nothing else, so the only writes it can
/// cause are the ones `IngestService::apply` makes for an event it was **given**.
/// No event, no write — and neither an error nor an empty stream produces an
/// event. A future revision that took a repository, or synthesised a terminal
/// event of its own, would break that and no test here would notice, which is
/// the reason this paragraph exists rather than a `#[test]`.
/// `an_unreachable_executor_retires_nothing` is still worth having: it pins the
/// *composition* — that the slot is released and a later tick re-attaches once
/// the executor recovers — which is a property no reading of this function
/// alone gives you.
#[allow(
    clippy::cognitive_complexity,
    reason = "inflated by the `tracing` macros in each arm: the three ways an observation \
              can end - unreachable executor, ingest failure, stream over - are three \
              different operator stories and each owes its own line, and the metric counts \
              every expansion as a branch. Same diagnosis as `service::dispatch`'s \
              `record_started`"
)]
async fn drain<R, Q>(executor: &dyn RunExecutor, ingest: &IngestService<R, Q>, target: WatchTarget)
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    let mut stream = match executor.watch(&target.execution_ref).await {
        Ok(stream) => stream,
        Err(error) => {
            error!(
                run_id = %target.run_id,
                execution_ref = target.execution_ref.as_str(),
                %error,
                "could not observe an execution; nothing is known about it, so the run is \
                 left exactly as it is and the next sweep tries again",
            );
            return;
        }
    };

    let ctx = system_actor::for_result_ingest(target.tenant);
    match ingest.ingest(&ctx, target.run_id, &mut stream).await {
        Ok(()) => info!(
            run_id = %target.run_id,
            execution_ref = target.execution_ref.as_str(),
            "the observation of an execution ended",
        ),
        Err(error) => warn!(
            run_id = %target.run_id,
            execution_ref = target.execution_ref.as_str(),
            %error,
            "ingesting an execution's events failed and the observation stopped; the \
             next tick's claim reconciliation owns the claim and the timeout sweep owns \
             the run",
        ),
    }
}

impl<R, Q> RunWatcher for SpawningRunWatcher<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// # The observer's lifetime is the run's, never the caller's
    ///
    /// The task is spawned detached and holds its own `Arc`s, so it outlives the
    /// tick that started it and survives the next one. Binding it to the caller
    /// would mean a dispatcher pass either awaiting an eight-hour execution or
    /// cancelling the observation when the pass ended — and
    /// `cpt-cf-qa-nfr-run-duration` puts eight hours on the run, not on the
    /// tick.
    ///
    /// What it does *not* survive is the process: a dropped runtime takes every
    /// observer with it, which is the whole reason the re-attachment pass exists.
    fn attach(&self, target: WatchTarget) {
        // The insert is the idempotency, and it happens before the spawn: two
        // concurrent attaches cannot both reach `tokio::spawn`, because only one
        // of them is handed a slot.
        let Some(slot) = self.attached.claim(target.run_id) else {
            return;
        };
        info!(
            run_id = %target.run_id,
            execution_ref = target.execution_ref.as_str(),
            "observing an execution's results",
        );

        let executor = Arc::clone(&self.executor);
        let ingest = Arc::clone(&self.ingest);
        tokio::spawn(async move {
            // Moved in so it is dropped — and the run freed for a later attempt
            // — however this task ends, including a panic.
            let _slot = slot;
            drain(executor.as_ref(), ingest.as_ref(), target).await;
        });
    }

    fn is_watching(&self, run_id: Uuid) -> bool {
        self.attached.holds(run_id)
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
