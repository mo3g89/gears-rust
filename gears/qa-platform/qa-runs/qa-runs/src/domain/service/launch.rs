//! The launch service: parity spec §3.4's six rules, in order.
//!
//! One public entry — [`LaunchService::launch`] — whose structure *is* the six
//! rules. Each is cited beside the logic it justifies:
//!
//! 1. **Branch resolution**, explicit request -> platform `default_branch` ->
//!    repository `default_branch` ([`resolve_branch`]).
//! 2. **Group the target's files by `repo_id`** ([`group_by_repo`]).
//! 3. **Guard**: more than one repository group and no explicit branch is a
//!    failed precondition ([`DomainError::AmbiguousBranch`]).
//! 4. **Per group: force-sync the branch, then build a bundle** — *not here*.
//! 5. **One execution node per group** — *not here*.
//! 6. **Record `test_version` as the branch label.**
//!
//! # Rules 4 and 5 are deliberately absent, and that is a correctness property
//!
//! The source system does not sync during exclusivity resolution and says why:
//! *"admission must stay cheap - the sync and the bundle build are what
//! `dispatch` is for - so a TEST_META change pushed between the last sync and
//! this launch is not seen. That is a documented limitation of the design, not
//! an oversight"* (`manager/src/services/exclusivity.rs:397-400`; the whole doc
//! comment on `scan_test_meta` is `:394-402`). Ported: this
//! module resolves exclusivity from whatever the catalog can serve **without**
//! forcing a sync, and `service::dispatch` (Task 14) owns the force-sync and the
//! per-group bundle build.
//!
//! The ordering matters for more than cost. The admission decision — and
//! therefore the queue's FIFO position — must be taken before minutes of I/O, or
//! two concurrent launches serialise behind each other's bundle builds.
//!
//! # Resolution never fails a launch
//!
//! `manager/src/services/exclusivity.rs:177-179`: *"Never fails. A plan that
//! cannot be resolved (wrong branch, deleted repo) yields the default and is
//! logged: the launch then fails for that reason at dispatch, where the error
//! belongs and where the caller already expects it."* Ported in
//! [`LaunchService::resolve_exclusivity`], which swallows catalog failures,
//! logs, and resolves parallel.
//!
//! That is a statement about the **exclusivity scan** and not about the whole
//! launch. Reading a plan in order to know *which files a run contains* is a
//! different question, and a launch whose target cannot be resolved at all
//! still fails — as it does in the source system, where the same read is a
//! `BAD_REQUEST` (`manager/src/routes/runs.rs:670-680`).
//!
//! # Composition order, and what a failure costs at each step
//!
//! | Step | On failure |
//! |---|---|
//! | validate parameters | nothing written, nothing fetched |
//! | read the platform | launch fails; no run row |
//! | read the target, group, guard | launch fails; no run row |
//! | resolve exclusivity | **cannot fail**; resolves parallel and logs |
//! | create the run row | launch fails; no run row (the insert is the write) |
//! | admit | run row is marked terminal, then the error is returned |
//! | dispatch inline | the error propagates; Task 14 owns the claim release |
//!
//! Parameters are validated **first**, before any I/O, so a launch that will be
//! rejected does not force anything to be read.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use qa_catalog_sdk::{CustomPlanEntry, QaCatalogClientV1, TestFileMeta};
use qa_environments_sdk::{QaEnvironmentsClientV1, TargetPlatform};
use qa_runs_sdk::{
    ExclusiveTier, LaunchOutcome, LaunchRequest, Run, RunKind, RunParameter, RunSource, RunState,
    RunTarget,
};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use super::admission::CapSlot;
use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::exclusivity::{self, FileMeta, Resolution};
use crate::domain::naming::{self, NameSources};
use crate::domain::params;
use crate::domain::repos::{NewRun, RunStatePatch, RunsRepository};
use crate::domain::state_machine;
use crate::domain::timeout::{resolve_timeout_seconds, saturating_i64};

// ---------------------------------------------------------------------------
// The two seams this module defines and Task 14 satisfies
// ---------------------------------------------------------------------------

/// Decide-and-record, under the platform's admission lock.
///
/// A trait rather than a direct call so the launch path is testable without a
/// lock registry or a lease, and so the two halves review separately: this
/// module owns *what a run is*, `service::admission` owns *whether it may start
/// now*, and that boundary is exactly where the source system's per-platform
/// mutex sits (`manager/src/services/run_queue.rs:566-588`).
///
/// # The obligation this seam hands to its implementor
///
/// `qa_run_queue.run_id` references `qa_runs(id)` with no tenant component, so
/// an insert carrying a guessed id succeeds if that run exists in *any* tenant
/// and fails if it does not - the response itself discriminates, which is a
/// cross-tenant existence oracle. **The implementor must resolve the run through
/// `RunsRepository::resolve_owned` under its own `qa.run`/`get` scope.**
///
/// **That precheck is already structural, and not because of this signature.**
/// `NewQueueRow::run` is an `OwnedRunId` (`domain::repos::queue_repo`, the field
/// declaration), whose only constructor is `resolve_owned` - a *provided* trait
/// method doing a tenant-scoped `get` that answers `RunNotFound` for absent and
/// foreign alike. Task 14 physically cannot build the insert payload without
/// minting a token, so the oracle is closed by a type, not by this paragraph.
///
/// What the prose adds beyond the type is only *which scope* the token must be
/// minted under, and since the admitter receives the caller's own `ctx` and
/// derives its scopes from it, widening this signature to take an `OwnedRunId`
/// would move where the token is minted rather than whether one is needed.
/// **So the signature stays `admit(ctx, run: &Run)`.** An earlier version of this
/// comment called the guard "prose, and prose is the weaker guard" and offered
/// the widened signature as the stronger alternative; that overstated the gap,
/// because `NewQueueRow::run` was already doing the work.
#[async_trait]
pub trait Admitter: Send + Sync {
    /// # Errors
    /// [`DomainError::QueueFull`] / [`DomainError::ConcurrencyLimit`] for the
    /// two 429 causes the frozen guide enumerates (guide lines 240-242);
    /// otherwise a persistence or lease failure.
    async fn admit(&self, ctx: &SecurityContext, run: &Run) -> Result<Admitted, DomainError>;

    /// Capacity for a run that does **not** go through admission at all.
    ///
    /// Answers [`Admission::Unqueued`] and takes no lock, no lease, no queue
    /// row and no cluster-capacity reservation. Its only caller is the collect
    /// route in [`LaunchService::launch`], and its only reason to exist is that
    /// [`InlineDispatcher::dispatch_inline`] takes a `&CapSlot` — the borrow
    /// that makes "the reservation outlives the submit" structural rather than
    /// documented.
    ///
    /// # Why it is a trait method rather than a `CapSlot` constructor
    ///
    /// [`CapSlot`] deliberately has **no** production constructor outside
    /// `service::admission`, and its own doc says why: substituting a fresh
    /// slot for a real one is the last spelling that still drops a reservation
    /// early, so `launch` and `runs` are given nothing to reach for. A
    /// `CapSlot::unmetered()` visible here would re-open exactly that. Minting
    /// the slot behind this seam keeps the constructor where it was, and keeps
    /// the *decision* — which runs bypass the cap — in the module that owns the
    /// cap.
    ///
    /// # Errors
    /// Infallible today. `Result` because the seam's implementor may need to
    /// read state before answering, and widening a return type later would
    /// touch every caller.
    async fn bypass(&self, ctx: &SecurityContext) -> Result<Admitted, DomainError>;
}

/// An [`Admission`], plus the cluster-capacity slot that made it legal.
///
/// The slot is the coordination `max_concurrent_runs` needs and it is *not*
/// inert: the admitter's cap check reads the executor's listing, which the
/// admitted run does not enter until the caller submits it, so the caller must
/// keep this value alive until then. Binding it and using only
/// [`Self::admission`] is enough — the drop at the end of the call is the
/// release. See `service::admission`'s `GlobalCapGate`.
///
/// Not `#[domain_model]`, unlike [`Admission`]: this is the seam's carrier for an
/// RAII guard rather than a value the domain models, and it is deliberately
/// neither `Copy` nor `Clone` so the slot cannot be duplicated.
#[derive(Debug)]
pub struct Admitted {
    /// What the admitter decided.
    pub admission: Admission,
    /// Released on drop. Private, so only `service::admission` can mint one, and
    /// read by [`LaunchService::settle`], which hands it to the submit.
    pub(in crate::domain::service) slot: CapSlot,
}

/// The result of admitting one launch.
///
/// An enum rather than a struct because the queue-row id only exists when a row
/// was written, and a struct would have needed a sentinel
/// (`manager/src/services/run_queue.rs:590-606`).
///
/// `#[domain_model]` for the same reason [`crate::domain::system_actor::TenantBound`]
/// carries it: it is a public domain type, every other one in this crate is marked,
/// and the attribute's actual job is to reject infrastructure types in fields at
/// compile time - so Task 14 cannot add a `sea_orm` or `http` type to this enum as
/// it grows. DE0309 itself remains **unverified** (no `cargo gears lint --dylint`).
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Row inserted `dispatching`; the caller dispatches inline and answers 200.
    ///
    /// **This outcome never transitions the run through `RunState::Queued`, even
    /// though a queue row exists** — and that is deliberate, so Task 14 does not
    /// re-litigate it. The frozen guide's model is that a run which starts
    /// immediately was never *queued*: the row exists because the queue table is
    /// also the claim ledger, not because the run waited. Recording `queued` here
    /// would make every consumer counting queue depth or waiting time see a run
    /// that spent zero time in the queue. The lifecycle a consumer sees is
    /// `created -> dispatching`, decided at [`LaunchService::settle`].
    Dispatch { queue_id: Uuid },
    /// Row inserted `queued`; the caller answers 202.
    Queued { queue_id: Uuid },
    /// No platform, so nothing to coordinate — never queued, never occupancy
    /// (`../testrunner/docs/guides/exclusive-runs-and-the-queue.md` line 76;
    /// `manager/src/services/run_dispatcher.rs:103-106`). Dispatch inline with
    /// no queue row at all.
    Unqueued,
}

/// Submit an admitted run to the execution plane, inline on the launch path.
///
/// **A second seam the plan does not list, and the reason it has to exist.**
/// Step 6 of the plan defines only [`Admitter`], while
/// [`Admission::Dispatch`]'s own contract says *"the caller dispatches inline
/// and answers 200"* and Task 14 specifies `dispatch_one(ctx, queue_id, run_id)`
/// as *"the single path from a claimed row to a started execution, used by both
/// the inline admission path and the tick"*. Those two statements require the
/// launch path to call into dispatch, and nothing in the plan gave it a way to.
/// Rather than reach into a module this task does not own, the seam is declared
/// here in the same shape as [`Admitter`] and Task 14 implements it on its
/// dispatch service.
///
/// `queue_id` is an `Option` because [`Admission::Unqueued`] dispatches with no
/// queue row at all — the platformless case, which has no row to mark running.
///
/// # `capacity` is a borrow, and that is the whole point of it
///
/// The submit is what puts a run in the listing the cluster-wide cap counts, so
/// the [`CapSlot`] taken at the cap check has to still be alive here or the cap
/// is read-then-act again. Passing it makes that structural rather than
/// documented: a caller cannot reach the submit having dropped it, because it has
/// nothing to pass. A **borrow** rather than a move so the implementor cannot
/// release it either — it has no ownership to release — which leaves the slot's
/// lifetime entirely with the caller that took it.
#[async_trait]
pub trait InlineDispatcher: Send + Sync {
    /// # Errors
    /// Whatever submission failed with. The implementor owns releasing the
    /// claim and moving the run to a terminal state before returning
    /// (Task 14, "a failed submit must release the claim").
    async fn dispatch_inline(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        queue_id: Option<Uuid>,
        capacity: &CapSlot,
    ) -> Result<(), DomainError>;
}

// ---------------------------------------------------------------------------
// Rule 1: branch resolution, written once
// ---------------------------------------------------------------------------

/// The branch this run will resolve against: the explicit request, else the
/// platform's default, else the repository's default (parity spec §3.4 rule 1).
///
/// Ported from `effective_branch` (`manager/src/services/exclusivity.rs:297-312`
/// — explicit trimmed and non-empty-filtered, else the platform default, trimmed
/// and non-empty-filtered, else `None`) plus the repository default that fills
/// the `None` (`manager/src/routes/runs.rs:639-644`, and the identical chain for
/// a custom plan at `manager/src/routes/custom_plans.rs:955-959`).
///
/// **Written once, on purpose.** The source system inlines this chain in three
/// route files and warns about it in place
/// (`manager/src/services/exclusivity.rs:290-296`): *"The duplication is
/// load-bearing and unenforced: if one copy's trimming or precedence drifts,
/// this scan reads a different branch's files than the run will execute."*
///
/// **This function has exactly one caller and is meant to keep exactly one.** It
/// is private to `launch`, which an earlier version of this paragraph contradicted
/// by telling "Task 14's dispatch" to call it — from a sibling module, where it is
/// not visible (`E0603`). Pointing a named caller at an unreachable definition is
/// how the drift starts, because the only ways to obey are to widen the visibility
/// or to re-derive the chain.
///
/// The resolution is not visibility. **Dispatch must not re-resolve the branch at
/// all; it reads `Run::test_version`,** which rule 6 makes the branch label
/// verbatim (`test_version: Some(facts.branch)` in `Resolved::clone_new_run`). That
/// is the stronger single-definition argument, not a weaker one: re-resolving would
/// consult the platform and repository defaults *again*, minutes later and after
/// the admission decision, so a `default_branch` edited in between would have
/// dispatch sync a different ref than the one this run's exclusivity was resolved
/// from and than the one recorded as its version. The run row **is** the record of
/// the decision, and legacy's warning is precisely about a second copy of the chain
/// disagreeing with the first.
///
/// It is an `Option<String>` on `Run` because the column is nullable; every run
/// this service creates carries `Some`, and a dispatch that finds `None` is looking
/// at a row this gear did not write and should fail rather than invent a branch.
///
/// Contrast `domain::timeout::resolve_timeout_seconds`, which *is* public: Task 15's
/// re-run has to compute a **new** deadline for a **new** run, so it needs the
/// chain, whereas dispatch needs the recorded answer. Reuse the function when the
/// decision is being made again; read the row when it has already been made.
///
/// The three copies are `manager/src/routes/runs.rs:545-552`,
/// `manager/src/routes/plans.rs:273-280` and
/// `manager/src/routes/custom_plans.rs:455-462`, each of which reads the
/// platform default and folds it into a variable it then calls
/// `requested_branch` (`runs.rs:554-566` - the fold's closure runs to `:566`, so
/// a span ending at `:562` cuts it in half). **That name is the trap**: by the
/// time the repository default is applied at `runs.rs:639-644`,
/// `requested_branch` is no longer the request's field — it already carries the
/// platform tier. Reading only `:639-644` (which the plan's rule-1 citation
/// does) makes the submit path look like a two-tier chain that disagrees with
/// the scan's three-tier one. It is not; the tiers are the same, and this
/// paragraph exists because that misreading was made and then caught.
///
/// Note that only the two *upper* tiers are trimmed and emptiness-filtered. The
/// repository default is taken as stored, which is what the source system does
/// (`runs.rs:643`, `unwrap_or(repo.repository.default_branch.as_str())` with no
/// filter): it is a required column under that gear's own validation, not
/// caller input.
///
/// # The platform tier's filter is redundant, and stays
///
/// Since 2026-08-14 qa-environments normalises `default_branch` on **both** its
/// write paths — trimming, and storing a blank as `NULL`
/// (`PlatformsService::normalize_default_branch`) — so a stored override reaching
/// [`platform_default_branch`] is already either `None` or trimmed and non-empty,
/// and the middle filter here can never fire on one. It is therefore
/// **defensively redundant, not disagreeing**: the two places do not contradict
/// each other, and neither should be "simplified" on the strength of the other.
/// The source system carries the identical redundancy for the identical reason —
/// `exclusivity.rs:310-311` filters a value `platforms.rs:385-392` and `:447-454`
/// have already normalised. What makes keeping it right rather than merely
/// harmless: the guarantee lives in another gear's *service layer*, not in a type
/// this one can see, so nothing here would break if it were relaxed. The filter
/// is what makes that impossible to notice only in production.
fn resolve_branch(
    explicit: Option<&str>,
    platform_default: Option<&str>,
    repo_default: &str,
) -> String {
    if let Some(branch) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return branch.to_owned();
    }
    if let Some(branch) = platform_default
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return branch.to_owned();
    }
    repo_default.to_owned()
}

/// The platform tier of [`resolve_branch`]: the target platform's own
/// `default_branch` override, or `None` when it has none.
///
/// The source system reads exactly this field for exactly this purpose —
/// `PlatformsService::get_platform_default_branch`, whose own doc calls it *"the
/// per-platform default branch override (falls back to repo default)"*
/// (`manager/src/services/platforms.rs:845-848`), backing
/// `platforms_meta.default_branch TEXT`
/// (`manager/migrations/001_initial.sql:233`) — and that is precisely what
/// `effective_branch` consults (`exclusivity.rs:305-311`).
///
/// # History, kept because it explains the shape below
///
/// This tier was inert until 2026-08-14: `qa_environments_sdk::TargetPlatform`
/// shipped with no `default_branch`, so a launch naming no branch fell straight
/// through to the repository default where the source system would have used the
/// platform's — silently. Task 13 found the gap, kept the tier rather than
/// deleting it (the chain is frozen behaviour), and left the guard below; Task
/// 13b added the field and wired it.
///
/// # The guard is the destructuring, and it worked
///
/// `qa_environments_sdk::TargetPlatform` is not `#[non_exhaustive]` (checked), so
/// the exhaustive destructuring — no `..` — makes adding a field a **compile
/// error in this gear, in this function**, naming the exact place to wire it.
/// That is not a prediction: adding `default_branch` produced `E0027: pattern
/// does not mention field 'default_branch'` at this line, which is how Task 13b
/// was routed here.
///
/// It replaced a test (`platform_defaults_are_not_available_yet`, now deleted)
/// that asserted `None == None` and so could only have failed *after* somebody
/// had already edited the function it tested — the wrong way round. The bindings
/// stay `_`-prefixed rather than `..` because `..` is precisely what would make
/// the next field silent.
fn platform_default_branch(platform: Option<&TargetPlatform>) -> Option<&str> {
    let platform = platform?;
    // Exhaustive on purpose: see the doc comment. A new field here is a compile
    // error rather than a silently-ignored tier.
    let TargetPlatform {
        id: _,
        name: _,
        product_id: _,
        description: _,
        kubeconfig_credstore_ref: _,
        available: _,
        observed_version: _,
        observed_build: _,
        default_branch,
        // Added 2026-08-31 by the default-platform work (qa-environments
        // `m20260831_000009_platform_is_default`). Consciously ignored, like the
        // fields below it: the flag answers "which platform does the UI's
        // 'Default cluster' option mean?", which is a question settled *before* a
        // launch reaches this gear -- by the time branch resolution runs, a
        // concrete platform has already been chosen and its identity, not its
        // defaultness, is what matters. A run launched against the default
        // platform must resolve its branch identically to the same run launched
        // by naming that platform explicitly, so reading this here would be a
        // behaviour difference with no justification.
        is_default: _,
        // Added 2026-08-28 by the platform-observation work (qa-environments
        // Task 7). This function only ever cared about `default_branch`, and
        // `vhp_base_url`/`observed_namespace` are consumed elsewhere in this
        // gear's dispatch assembly (a separate task on that same plan), not
        // here, so they are consciously ignored rather than wired.
        vhp_base_url: _,
        observed_namespace: _,
        version_detect_error: _,
        version_detected_at: _,
        // Added 2026-08-28 by the cluster-health work (qa-environments
        // Task 5). Branch resolution has nothing to do with cluster health,
        // so this is consciously ignored rather than wired, exactly like
        // `vhp_base_url`/`observed_namespace` above.
        cluster: _,
        created_at: _,
        updated_at: _,
    } = platform;
    default_branch.as_deref()
}

// ---------------------------------------------------------------------------
// Rule 2: group by repository
// ---------------------------------------------------------------------------

/// Group `(repo_id, path)` pairs by repository, deduplicating paths and keeping
/// a deterministic order (parity spec §3.4 rule 2; the grouping itself is
/// `manager/src/routes/custom_plans.rs:652-658` - the `BTreeMap` declared at
/// `:652`, iterated at `:654-658`).
///
/// The source system records why the grouping exists, in the comment directly
/// above it (`custom_plans.rs:647-651`): *"Group tests by repository so each
/// checkout gets its own bundle. A custom plan that mixes Backend + UI (or any
/// two repos) used to bail out here (`repo_ids.len() != 1` -> no
/// `TEST_BUNDLE_URL`), leaving every pod on the empty image `/test_plans` - which
/// is exactly the 'Test file not found' / 'no playwright.config.ts' failure
/// mode."* That is a production bug the grouping fixed, and it is the best
/// argument for why one group per repository is not an optimisation.
///
/// A `BTreeMap` for the same reason the source system iterates one
/// (`custom_plans.rs:652-658`, and its own note at `:723-724` - *"Iterate
/// `tests_by_repo` (a `BTreeMap`) rather than `repo_configs` (a `HashMap`) so
/// node ids are assigned in a stable, sorted order"*): the group order reaches the
/// run's `bundle_ids` and the executor's node list, and a `HashMap` would
/// reorder them between processes.
fn group_by_repo(files: &[(Uuid, String)]) -> BTreeMap<Uuid, Vec<String>> {
    let mut grouped: BTreeMap<Uuid, Vec<String>> = BTreeMap::new();
    for (repo_id, path) in files {
        let paths = grouped.entry(*repo_id).or_default();
        if !paths.contains(path) {
            paths.push(path.clone());
        }
    }
    grouped
}

/// One nested plan a custom plan composes, and the files it contributed.
///
/// This is the unit legacy's `custom_plan_tier` resolves exclusivity over — not
/// the repository group [`group_by_repo`] builds for bundling. The two groupings
/// are genuinely different and both are needed: bundling wants one checkout per
/// **repository**, exclusivity wants one `plan.yaml` per **nested plan**, and a
/// repository can hold several plans.
struct NestedPlan {
    /// The repository holding this nested plan's `plan.yaml` *and* its files.
    ///
    /// Legacy takes this from the nested plan's own `TestPlanInfo`, not from the
    /// entry — *"a custom plan can reference plans from several repositories, and
    /// `scan_test_meta` wants only the one its plan belongs to"*
    /// (`manager/src/services/exclusivity.rs:552-555`, the lookup at `:556`).
    /// Here they are necessarily the same repository, because a
    /// [`CustomPlanEntry`]'s `plan_path` is documented as living in the same
    /// repository as its `path`.
    repo_id: Uuid,
    /// The nested plan's `plan.yaml`, or `None` when the entry names none.
    ///
    /// `None` has no analogue in legacy, where `CustomPlanTest::plan_id` is
    /// mandatory — and since 2026-08-14 it has none in this port's *write* path
    /// either, so it means exactly one thing: **a row stored before the field
    /// existed**. It is **not** legacy's "unresolved" case, which is a named plan
    /// that could not be loaded and which legacy counts and warns about. `None`
    /// means no plan was ever named, so there is nothing to resolve and nothing to
    /// warn about — the files fall through to the `TEST_META` scan, which is what
    /// this gear did for every custom-plan file before `plan_path` existed.
    plan_path: Option<String>,
    /// The files this nested plan contributed, deduplicated in encounter order.
    files: Vec<String>,
}

/// What one nested plan contributed to a custom plan's exclusivity.
///
/// The three arms are legacy's three, and naming them is what keeps
/// "contributed a `false`" distinguishable from "contributed nothing" — the
/// distinction `combine_nested`'s third and fourth branches exist for, and the
/// one a `bool` return would have collapsed.
enum NestedContribution {
    /// The nested plan's `plan.yaml` declared a flag; its files were not read
    /// (`manager/src/services/exclusivity.rs:549-550`).
    Declared(bool),
    /// It declared nothing (or named no plan) and its own files voted
    /// (`exclusivity.rs:551-561`).
    Scanned(bool),
    /// It contributed no flag at all. `unresolved` is `true` only for legacy's
    /// counted case — a *named* plan that would not load (`exclusivity.rs:545`) —
    /// and `false` when the scan simply produced no vote, which legacy does not
    /// count either.
    Nothing { unresolved: bool },
}

/// Group a custom plan's entries by the nested plan each one belongs to — the
/// port of `group_tests_by_plan` (`manager/src/services/exclusivity.rs:456-466`,
/// whose doc at `:452-455` states the point: *"`CustomPlanTest` carries its own
/// `plan_id`, so a custom plan can mix tests from several plans and each group is
/// resolved against its own `plan.yaml`"*).
///
/// **Keyed on `(repo_id, plan_path)`, not `plan_path` alone.** Legacy's `plan_id`
/// is unique across the whole plans directory and carries its repository with it;
/// a `plan.yaml` path is unique only within a repository, so two repositories can
/// each hold `plans/smoke.yaml` and they are two different plans. Dropping
/// `repo_id` from the key would merge them and scan each one's files against the
/// other's repository.
///
/// A `BTreeMap` for determinism, as legacy uses one at `:457-458`: the group
/// order decides which nested plan a `TEST_META` warning names first. `None`
/// sorts before `Some` and so is a stable first group rather than an
/// order-dependent one.
fn group_by_nested_plan(files: &[CustomPlanEntry]) -> Vec<NestedPlan> {
    let mut grouped: BTreeMap<(Uuid, Option<String>), Vec<String>> = BTreeMap::new();
    for entry in files {
        let paths = grouped
            .entry((entry.repo_id, entry.plan_path.clone()))
            .or_default();
        // Deduplicated on the full path, node-id selector included, exactly as
        // legacy dedups on the full `test_file` (`exclusivity.rs:461-463`).
        if !paths.contains(&entry.path) {
            paths.push(entry.path.clone());
        }
    }
    grouped
        .into_iter()
        .map(|((repo_id, plan_path), files)| NestedPlan {
            repo_id,
            plan_path,
            files,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

/// The `default_base` a run name falls back to when nothing else slugifies.
///
/// **Must be ASCII, slug-safe, and free of `_`** — `naming::name_base` takes
/// this value *unslugified* (`manager/src/services/argo.rs:2486-2488`), so
/// whatever is here reaches the run name and from there a `LIKE '{prefix}-%'`
/// pattern in which `_` is a single-character wildcard. In particular this must
/// never be `RunKind::as_str()`, whose `CustomPlan` arm is `"custom_plan"`
/// (`qa-runs-sdk/src/models.rs:148`; the citation read `:77` until the collect
/// kind's doc comment moved the arm down the file). See `domain::naming`'s
/// composition obligation 1.
///
/// The source system has exactly two literals, `"plan"` (`argo.rs:420`) and
/// `"test"` (`argo.rs:737`); it never calls `workflow_name_base` for a custom
/// plan at all (`argo.rs:1046-1047`, which formats `custom-{plan.id}-{n}`
/// directly), so `"custom"` is this port's third literal and is unreachable in
/// practice — a custom plan's primary is `custom-{uuid}`, which always
/// slugifies non-empty.
const fn default_name_base(kind: RunKind) -> &'static str {
    match kind {
        RunKind::Plan => "plan",
        RunKind::Test => "test",
        RunKind::CustomPlan => "custom",
        // Legacy's collect submission takes the plan submit path, so its name
        // base comes from `workflow_name_base(&plan.plan.name, ..)` over the
        // synthetic plan name `format!("collect {}", repo.name)`
        // (`manager/src/services/collect.rs:104`) — which always slugifies to
        // something beginning `collect-`. This literal is the fallback for the
        // case that name cannot slugify at all, and unlike `"custom_plan"` it
        // is already ASCII, `_`-free and slug-safe, so `RunKind::as_str()`
        // would in fact have been usable here. It is still not used: the rule
        // this constant carries is "never `RunKind::as_str()`", and a rule with
        // one silent exception is not a rule.
        RunKind::Collect => "collect",
    }
}

/// How many times a name collision is retried before the launch fails.
///
/// `naming::next_sequence` is advisory: two concurrent launches on one prefix
/// read the same highest number and compute the same next one. The unique index
/// `idx_qa_runs_tenant_name` is what actually decides, and the loser retries.
/// The source system does exactly this — three attempts, the number recomputed
/// *inside* the loop (`manager/src/services/argo.rs:416-422`), retrying on the
/// create's 409 (`argo.rs:677-686`) — and **bounded**, because an unbounded
/// loop turns a genuinely stuck name into a hang.
const NAME_ATTEMPTS: u32 = 3;

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// Launch validation, resolution, and the six-step launch contract.
pub struct LaunchService<R: RunsRepository> {
    db: Arc<DbProvider>,
    runs: Arc<R>,
    catalog: Arc<dyn QaCatalogClientV1>,
    environments: Arc<dyn QaEnvironmentsClientV1>,
    /// Where a terminal transition releases the run's live log channel — see
    /// [`Self::transition`]. Added by Task 15's second fix round: `abandon`
    /// records `Canceled`, and without this field the reap was not *forgotten*
    /// here, it was **unreachable**.
    logs: Arc<dyn super::LogFanout>,
    admitter: Arc<dyn Admitter>,
    dispatcher: Arc<dyn InlineDispatcher>,
    default_timeout_seconds: u64,
    policy_enforcer: PolicyEnforcer,
}

impl<R: RunsRepository> LaunchService<R> {
    #[allow(
        clippy::too_many_arguments,
        reason = "a DI container assembled once at startup; the alternative is a second deps struct that only this constructor reads"
    )]
    pub fn new(
        db: Arc<DbProvider>,
        runs: Arc<R>,
        catalog: Arc<dyn QaCatalogClientV1>,
        environments: Arc<dyn QaEnvironmentsClientV1>,
        logs: Arc<dyn super::LogFanout>,
        admitter: Arc<dyn Admitter>,
        dispatcher: Arc<dyn InlineDispatcher>,
        default_timeout_seconds: u64,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            runs,
            catalog,
            environments,
            logs,
            admitter,
            dispatcher,
            default_timeout_seconds,
            policy_enforcer,
        }
    }

    /// The admitter, for the composition test that proves the launch path and
    /// force start reserve cluster capacity from one gate.
    #[cfg(test)]
    pub(in crate::domain::service) fn admitter(&self) -> &Arc<dyn Admitter> {
        &self.admitter
    }
}

/// Everything the target resolution settled, before the run row exists.
///
/// # Rejected alternative: return the pieces, or build [`NewRun`] directly
///
/// The three `*_target_facts` methods could have returned a tuple, or built a
/// `NewRun` straight away. Both were rejected for the same reason: the three
/// target kinds settle these six fields by **visibly different rules** — a custom
/// plan resolves exclusivity per *nested plan* and has no tag filter of its own, a
/// single test scans one file with empty filters, a plan run scans under the
/// request's filter — and a named struct is what lets those differences be read
/// side by side and commented per field. A tuple of six makes the
/// `branch`/`name_base` pair silently interchangeable — both are `String`, so
/// transposing them compiles — which is exactly the mistake `exclusivity::Tiers`
/// exists to prevent one layer down. (An earlier version of this sentence named
/// `is_validation`/`branch`; transposing a `bool` and a `String` is `error[E0308]`,
/// so that pair proved nothing.)
///
/// The exclusivity difference has since moved *into* the type:
/// [`ExclusivitySource`] is an enum precisely because "no tag filter of its own"
/// was a rule a comment could not enforce.
///
/// Building `NewRun` here was rejected because these facts are settled *before*
/// the name is known and the name is retried: the run's identity is decided in
/// `create_run`'s loop, so a `NewRun` built at this point would have to be rebuilt
/// anyway. [`Resolved::clone_new_run`] is where that happens, and it takes `&self`
/// for the same reason.
struct TargetFacts {
    /// Rule 2's output: the files that will run, by repository.
    groups: BTreeMap<Uuid, Vec<String>>,
    /// Rule 1's output, and rule 6's value.
    branch: String,
    /// What the target gives exclusivity resolution to work from.
    exclusivity: ExclusivitySource,
    /// The plan's declared `timeout_seconds`, straight from the catalog.
    ///
    /// `Option` because a *custom* plan may carry none; a discovered plan always
    /// arrives `Some`, since qa-catalog's parser applies legacy's 300-second serde
    /// default (`qa-catalog/src/domain/parsing/plan_yaml.rs:42`, `:55-56`) and
    /// `to_sdk_plan` emits it unconditionally
    /// (`qa-catalog/src/domain/service/plans.rs:258-260`).
    ///
    /// # Unbounded, and the consequence, recorded where a reader will meet it
    ///
    /// **This value is not clamped, and an absurd one produces a run the
    /// control-plane timeout sweep can never reclaim.** `domain::timeout`'s
    /// `resolve_timeout_seconds` forwards it for `RunKind::Plan` and takes it as a
    /// `max` for `RunKind::CustomPlan`; a large enough value saturates in
    /// `saturating_i64`, `OffsetDateTime::checked_add` answers `None`, and
    /// `timeout_at` lands NULL — which `list_timeout_candidates` excludes by
    /// construction (*"a run with no deadline never times out"*). If that run is
    /// **exclusive** it holds the platform's global lease until somebody cancels it
    /// by hand.
    ///
    /// Left unclamped deliberately: legacy honours `plan.plan.timeout_seconds`
    /// unbounded (`manager/src/services/argo.rs:539`) and the value is authored by
    /// whoever can push to the test repository — who can already run arbitrary
    /// destructive tests — not by a launch caller holding only `qa.run`/`create`.
    /// The caller-supplied override *is* clamped, because that surface is this
    /// port's own addition. **Open item for the coordinator**, stated here rather
    /// than only in `saturating_i64`, which is a private helper nothing on this
    /// path points at.
    plan_timeout_seconds: Option<u64>,
    /// Whether this run is a validation run.
    ///
    /// For a plan-backed target this is `qa_catalog_sdk::Plan::validation`, which
    /// the catalog has already OR'd with the case-insensitive `validation` tag
    /// rule (`manager/src/services/plans.rs:35-39`). For a custom plan the catalog
    /// does not compute it, so [`tags_declare_validation`] applies the same rule to
    /// the plan's own tags here.
    is_validation: bool,
    /// The run-name prefix, already through `naming::name_base`.
    name_base: String,
}

/// The exclusivity inputs a target settles, in the shape that target's own rule
/// needs.
///
/// # Why an enum, rather than three fields one variant leaves empty
///
/// The previous shape was `plan_flag: Option<bool>` plus `scan_include` /
/// `scan_exclude`, with a custom plan setting all three to nothing. Two of those
/// three emptinesses were load-bearing facts explained only by comment:
///
/// * *"a custom plan has no tag filter of its own"*
///   (`manager/src/services/exclusivity.rs:552-558`, which passes `&[], &[]`) —
///   the launch's include/exclude must **not** reach a custom plan's scan;
/// * a single-test run considers only its one file, so no filter applies either
///   (`exclusivity.rs:207-224`, which also passes `&[], &[]`).
///
/// The first of those is a rule a future edit could break by threading
/// `request.include_tags` through "for consistency", and no type prevented it.
/// Here [`Self::NestedPlans`] has nowhere to put a tag filter, so the rule is
/// enforced rather than described. The second is still a value, because a
/// single-test run and a plan run take the same branch and differ only in it.
enum ExclusivitySource {
    /// A `Plan` or `Test` target: one `plan.yaml`, one repository, one scan.
    SinglePlan {
        /// `plan.yaml`'s declaration. `Some` short-circuits the scan entirely
        /// (`manager/src/services/exclusivity.rs:342-348`).
        plan_flag: Option<bool>,
        /// The request's tag filter for a plan run (`exclusivity.rs:197-205`),
        /// and **empty** for a single-test run.
        include: Vec<String>,
        exclude: Vec<String>,
    },
    /// A custom plan: legacy's `custom_plan_tier`
    /// (`manager/src/services/exclusivity.rs:491-583`).
    ///
    /// One entry per nested plan, each resolved against its own `plan.yaml`
    /// before falling through to its own `TEST_META` scan, the two aggregates
    /// combined by [`exclusivity::combine_nested`]. Empty for a custom plan with
    /// no files, which resolves `Default` exactly as legacy's
    /// `combine_nested(None, None)` does.
    ///
    /// # What is left of the divergence this variant closed
    ///
    /// This field replaced a `plan_flag: None` and a table recording that a custom
    /// plan composed of a plan declaring `exclusive: true` with silent test files
    /// resolved **parallel / tier `TestMeta`** here and **exclusive / tier
    /// `Plan`** in legacy. Two of that table's three rows are now false —
    /// `a_nested_plans_declaration_makes_a_custom_plan_exclusive` pins the
    /// legacy answer, and `exclusivity::combine_nested` has a production caller
    /// again for the first time since Task 12.
    ///
    /// **One row survives, and it is now historical rather than ongoing**: for an
    /// entry whose `CustomPlanEntry::plan_path` is `None` there is still no plan
    /// tier, so that entry's files resolve from `TEST_META` alone. Since 2026-08-14
    /// the write path *requires* `plan_path`
    /// (`qa_catalog_sdk::NewCustomPlanEntry`), so **nothing reachable through the
    /// API can produce such an entry** — the only source is a row written before
    /// the field existed. Those keep the old answer until re-saved, and no backfill
    /// can fix them; `qa_catalog_sdk::CustomPlanEntry` owns that argument.
    NestedPlans(Vec<NestedPlan>),
    /// A collect target: **no tier is consulted and no file is read.**
    ///
    /// Legacy's collect submission never reaches `resolve_exclusivity` at all —
    /// it calls `ArgoService::submit_workflow` directly with `exclusive: false`
    /// (`manager/src/services/collect.rs:146-148`, whose comment is *"Collection
    /// runs never target a real platform (see the `None`s above), so there is
    /// nothing to be exclusive about"*). A variant rather than an
    /// `ExclusivitySource::SinglePlan { plan_flag: Some(false), .. }` spelling,
    /// because those are not the same statement: `Some(false)` is a *plan.yaml
    /// declaration of parallel*, which stamps [`ExclusiveTier::Plan`] and would
    /// tell an operator a `plan.yaml` decided something for a run that has no
    /// plan. This variant has nowhere to put a flag, so the tier stays
    /// [`ExclusiveTier::Default`] and the cascade is unreachable rather than
    /// short-circuited.
    None,
}

/// Everything the run row needs, once resolution is done.
///
/// # Rejected alternative: reuse [`NewRun`] and mutate its `name`
///
/// `NewRun` already holds all of this plus the name, so `resolve` could have
/// returned one and `create_run` could have overwritten `name` on each retry. It
/// was rejected because a `NewRun` whose `name` is a placeholder is a value that is
/// invalid for its whole lifetime up to the assignment, and nothing in the type
/// says which field is the placeholder — the first retry that forgot to reassign
/// would insert a run named `""` (or, worse, the previous attempt's name) and the
/// unique index would report a collision that is not one.
///
/// `Resolved` carries no name at all, so "resolved but not yet named" is
/// representable and "named" is only ever produced by
/// [`Self::clone_new_run`]. The one thing this costs is the field-by-field copy in
/// that method, which is also where every field gets its "written by" comment.
///
/// The other rejected shape was folding `Resolved` into `TargetFacts`. They are
/// separated because `TargetFacts` is what the *target* determines and `Resolved`
/// is what the *request plus the platform* determines on top of it; merging them
/// would put `parameters` and `app_version` — neither of which any target rule
/// touches — inside the struct the three per-kind resolvers each build differently.
struct Resolved {
    target: RunTarget,
    platform_id: Option<Uuid>,
    /// Rule 6: the branch label **is** the recorded test version. There is no
    /// version-to-branch mapping and looking for one is the withdrawn
    /// requirement `DECOMPOSITION.md:123`'s Withdrawn bullet records (`:122` is the
    /// adjacent *Resolved* bullet about cross-repo custom plans)
    /// (`manager/src/routes/runs.rs:645` and
    /// `manager/src/routes/custom_plans.rs:960`, both
    /// `let test_version = Some(branch.clone());`).
    test_version: String,
    app_version: Option<String>,
    app_build: Option<String>,
    exclusivity: Resolution,
    is_validation: bool,
    parameters: Vec<RunParameter>,
    include_tags: Vec<String>,
    exclude_tags: Vec<String>,
    source: RunSource,
    schedule_id: Option<Uuid>,
    timeout_at: Option<OffsetDateTime>,
    name_base: String,
}

impl Resolved {
    /// The insert payload. `state` is [`RunState::Created`]: the admission
    /// outcome is not known until the row exists, because
    /// `QueueRepository::insert` needs an `OwnedRunId` and therefore a run to
    /// resolve. `Created -> Queued` and `Created -> Dispatching` are both legal
    /// (`domain::state_machine::can_transition`) and that is what the state is
    /// for.
    ///
    /// **`bundle_ids` is empty**, because rules 4 and 5 belong to dispatch.
    ///
    /// **And there is currently no way for dispatch to fill it in.** The column
    /// is written only at insert (`infra/storage/runs_sea_repo.rs:130`), and
    /// `RunsRepository` exposes no method that updates it - `update_state`,
    /// `set_execution_ref` and `add_result_counts` are the only writes, and
    /// `NewRun`'s own "written by" table does not list `bundle_ids` at all. So
    /// rule 5's "each execution node carrying its own bundle reference" has
    /// nowhere to be recorded on the run row once dispatch builds the bundles.
    /// Task 14 needs a `set_bundle_ids` (or an extension of
    /// `set_execution_ref`) on a repository trait this task does not own.
    ///
    /// **`parameters` is the normalized list**, never the request's raw one —
    /// Task 14 assembles the environment from the *stored* parameters, so a raw
    /// `"  FOO  "` would become an environment variable literally named
    /// `"  FOO  "` and a fully-blank editor row would become `""=""`, on the
    /// first run and with no replay needed. See `domain::params::normalize`.
    ///
    /// Takes `&self` rather than `self` because the caller's bounded name-retry
    /// loop rebuilds the payload with a fresh name on each attempt, exactly as
    /// the source system recomputes the name inside its own loop
    /// (`manager/src/services/argo.rs:419-422`).
    fn clone_new_run(&self, name: String) -> NewRun {
        NewRun {
            name,
            target: self.target.clone(),
            platform_id: self.platform_id,
            test_version: Some(self.test_version.clone()),
            app_version: self.app_version.clone(),
            app_build: self.app_build.clone(),
            state: RunState::Created,
            resolved_exclusive: self.exclusivity.exclusive,
            exclusive_tier: self.exclusivity.tier,
            is_validation: self.is_validation,
            parameters: self.parameters.clone(),
            include_tags: self.include_tags.clone(),
            exclude_tags: self.exclude_tags.clone(),
            source: self.source,
            schedule_id: self.schedule_id,
            bundle_ids: Vec::new(),
            timeout_at: self.timeout_at,
        }
    }
}

/// Project a catalog `TestFileMeta` onto the pure core's [`FileMeta`].
///
/// Only ever called on a meta the catalog actually returned. Constructing one
/// for a file that could **not** be read — `FileMeta { path, ..Default }` — is
/// the silent mistake `exclusivity::resolve_plan_tier` warns about: an admitted
/// file always votes, so a default votes `Some(false)`, which turns a `Default`
/// resolution into a `TestMeta`-false one and, if the unreadable file was the
/// destructive one, replaces its vote with a parallel one.
fn file_meta(meta: TestFileMeta) -> FileMeta {
    FileMeta {
        path: meta.path,
        tags: meta.tags,
        exclusive: meta.exclusive,
    }
}

/// Whether a tag list classifies a run as a validation run: a trimmed,
/// case-insensitive `validation` tag (`manager/src/services/plans.rs:35-39`).
///
/// Used only for custom plans. A discovered plan arrives with the same rule
/// already applied, OR'd with `plan.yaml`'s `validation:` bool, because
/// qa-catalog does it at parse time (`qa_catalog_sdk::Plan::validation`).
fn tags_declare_validation(tags: &[String]) -> bool {
    tags.iter()
        .any(|tag| tag.trim().eq_ignore_ascii_case("validation"))
}

fn catalog_error(error: &qa_catalog_sdk::QaCatalogError) -> DomainError {
    DomainError::Catalog(error.to_string())
}

fn environments_error(error: &qa_environments_sdk::QaEnvironmentsError) -> DomainError {
    DomainError::Environments(error.to_string())
}

// ---------------------------------------------------------------------------
// Resolution: rules 1, 2, 3 and 6
// ---------------------------------------------------------------------------

impl<R: RunsRepository> LaunchService<R> {
    /// Compile an `AccessScope` for `qa.run`.
    ///
    /// A fresh scope per repository call, never one hoisted across an
    /// operation: the PEP is asked for the action actually about to be
    /// performed, so adding a call cannot inherit a scope compiled for
    /// something else. See `domain::service`, "One scope per resource type".
    async fn run_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Result<toolkit_security::AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::RUN, action, resource_id)
            .await?)
    }

    /// Read the target platform, which is also the **tenant-ownership check**.
    ///
    /// Not an optimisation and not decoration. `qa_platform_leases`' primary key
    /// is a bare `platform_id`, so the lease is **not** tenant-partitioned: a
    /// run row carrying another tenant's `platform_id` drives its dispatcher to
    /// acquire the *global* lease on that platform and block the owning tenant's
    /// runs. Nothing downstream re-checks — `qa_runs.platform_id` has no foreign
    /// key and `domain::repos::NewQueueRow::platform_id` says in full that it is
    /// unverifiable there. This read is where ownership is established, and it
    /// is established by qa-environments' own PEP, under the caller's context.
    ///
    /// No local existence probe is layered on top: distinguishing "exists but
    /// forbidden" from "does not exist" is exactly the cross-tenant oracle
    /// qa-catalog's review found, and the SDK call already fails closed.
    async fn read_platform(
        &self,
        ctx: &SecurityContext,
        platform_id: Option<Uuid>,
    ) -> Result<Option<TargetPlatform>, DomainError> {
        match platform_id {
            None => Ok(None),
            Some(id) => self
                .environments
                .get_platform(ctx, id)
                .await
                .map(Some)
                .map_err(|error| environments_error(&error)),
        }
    }

    /// Rule 1's repository tier, for one repository.
    async fn repo_default_branch(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
    ) -> Result<String, DomainError> {
        self.catalog
            .get_repo(ctx, repo_id)
            .await
            .map(|repo| repo.default_branch)
            .map_err(|error| catalog_error(&error))
    }

    /// Resolve a plan-backed target (`Plan` or `Test`).
    ///
    /// Both kinds read the same `plan.yaml`; they differ in which files run and
    /// in whether the request's tag filter applies to the `TEST_META` scan.
    async fn plan_target_facts(
        &self,
        ctx: &SecurityContext,
        request: &LaunchRequest,
        platform: Option<&TargetPlatform>,
        repo_id: Uuid,
        path: &str,
        only_file: Option<&str>,
    ) -> Result<TargetFacts, DomainError> {
        // Rule 1, once: explicit -> platform default -> repository default.
        let repo_default = self.repo_default_branch(ctx, repo_id).await?;
        let branch = resolve_branch(
            request.branch.as_deref(),
            platform_default_branch(platform),
            &repo_default,
        );

        let plan = self
            .catalog
            .get_plan(ctx, repo_id, &branch, path)
            .await
            .map_err(|error| catalog_error(&error))?;

        // Rule 2. A plan-backed target is one repository by construction, so
        // the grouping is a single group and rule 3's guard cannot fire.
        let files: Vec<(Uuid, String)> = match only_file {
            Some(file) => vec![(repo_id, file.to_owned())],
            None => plan
                .test_files
                .iter()
                .map(|file| (repo_id, file.clone()))
                .collect(),
        };

        let (include, exclude) = if only_file.is_some() {
            (Vec::new(), Vec::new())
        } else {
            (request.include_tags.clone(), request.exclude_tags.clone())
        };

        let name_base = match only_file {
            Some(file) => naming::name_base(NameSources {
                primary: &test_name_stem(file),
                fallback: Some(plan.name.as_str()).filter(|name| !name.trim().is_empty()),
                default_base: default_name_base(RunKind::Test),
            }),
            None => naming::name_base(NameSources {
                primary: &plan.name,
                fallback: Some(path),
                default_base: default_name_base(RunKind::Plan),
            }),
        };

        Ok(TargetFacts {
            groups: group_by_repo(&files),
            branch,
            exclusivity: ExclusivitySource::SinglePlan {
                plan_flag: plan.exclusive,
                include,
                exclude,
            },
            plan_timeout_seconds: plan.timeout_seconds,
            is_validation: plan.validation,
            name_base,
        })
    }

    /// Resolve a custom-plan target: rules 2 and 3 in full.
    async fn custom_plan_target_facts(
        &self,
        ctx: &SecurityContext,
        request: &LaunchRequest,
        platform: Option<&TargetPlatform>,
        plan_id: Uuid,
    ) -> Result<TargetFacts, DomainError> {
        let plan = self
            .catalog
            .get_custom_plan(ctx, plan_id)
            .await
            .map_err(|error| catalog_error(&error))?;

        // Rule 2 first, because rule 3's guard is a question about the grouping.
        // Bundling groups by repository; exclusivity groups by *nested plan*
        // below. Both derive from the same entries and neither is the other.
        let files: Vec<(Uuid, String)> = plan
            .files
            .iter()
            .map(|entry| (entry.repo_id, entry.path.clone()))
            .collect();
        let groups = group_by_repo(&files);

        let explicit = request
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        // Rule 3 (`manager/src/routes/custom_plans.rs:673-687`). **Two distinct
        // strings live there and they are not interchangeable.** The *rationale*
        // is the source comment at `:671-672`: "Mixed multi-repo plans must pin
        // one explicit branch so every checkout resolves the same ref and
        // results register under that Version target." The *operator-facing
        // message* is the literal at `:682-684`: "Mixed custom plans that span
        // multiple repositories require an explicit branch. Select a branch that
        // exists in every contributing repository." - which names no Version
        // target and, notably, carries **no group count**.
        //
        // So `AmbiguousBranch` matches legacy on the status (BAD_REQUEST,
        // `:681`) and on naming the constraint, and its `groups` field is a
        // deliberate improvement over legacy rather than parity: an operator
        // reading "spans 3 repositories" knows how many branches have to line up.
        if groups.len() > 1 && explicit.is_none() {
            return Err(DomainError::AmbiguousBranch {
                plan_id,
                groups: groups.len(),
            });
        }

        // Rule 1. With more than one group the guard above has already
        // guaranteed an explicit branch, so the repository tier is consulted
        // only for the single-group case — which is the one place a repository
        // default is unambiguous.
        let branch = match (explicit, groups.keys().next()) {
            (Some(branch), _) => branch.to_owned(),
            (None, Some(&repo_id)) => {
                let repo_default = self.repo_default_branch(ctx, repo_id).await?;
                resolve_branch(None, platform_default_branch(platform), &repo_default)
            }
            // An empty custom plan: no repository to ask, and no explicit
            // branch. `plan.yaml` is never read, so the only thing the branch
            // still feeds is `test_version`. Fail rather than invent one.
            (None, None) => {
                return Err(DomainError::Validation {
                    field: "branch".to_owned(),
                    message: format!(
                        "custom plan {plan_id} has no files, so no repository default branch applies"
                    ),
                });
            }
        };

        Ok(TargetFacts {
            groups,
            branch,
            // Legacy's `group_tests_by_plan`, and the whole reason
            // `CustomPlanEntry` carries `plan_path`: each nested plan is resolved
            // against its own `plan.yaml` first. Entries naming no plan land in a
            // `plan_path: None` group and fall through to the `TEST_META` scan,
            // which is what every custom-plan file did before the field existed.
            //
            // The variant takes no tag filter, because "a custom plan has no tag
            // filter of its own" (`manager/src/services/exclusivity.rs:552-558`,
            // which passes `&[], &[]`) - see `ExclusivitySource`.
            exclusivity: ExclusivitySource::NestedPlans(group_by_nested_plan(&plan.files)),
            plan_timeout_seconds: plan.timeout_seconds,
            is_validation: tags_declare_validation(&plan.tags),
            // Ported verbatim from `manager/src/services/argo.rs:1046-1047`,
            // which formats `custom-{plan.id}-{n}` and never calls
            // `workflow_name_base`. Routed through `name_base` anyway so the
            // prefix cap and the trim apply; `custom-{uuid}` is 43 characters
            // and ASCII, so it passes through unchanged.
            name_base: naming::name_base(NameSources {
                primary: &format!("custom-{plan_id}"),
                fallback: None,
                default_base: default_name_base(RunKind::CustomPlan),
            }),
        })
    }

    /// Resolve whatever the launch targets into [`TargetFacts`].
    async fn resolve_target(
        &self,
        ctx: &SecurityContext,
        request: &LaunchRequest,
        platform: Option<&TargetPlatform>,
    ) -> Result<TargetFacts, DomainError> {
        match &request.target {
            RunTarget::Plan { repo_id, path } => {
                self.plan_target_facts(ctx, request, platform, *repo_id, path, None)
                    .await
            }
            RunTarget::Test {
                repo_id,
                path,
                test_file,
            } => {
                self.plan_target_facts(ctx, request, platform, *repo_id, path, Some(test_file))
                    .await
            }
            RunTarget::CustomPlan { id } => {
                self.custom_plan_target_facts(ctx, request, platform, *id)
                    .await
            }
            RunTarget::Collect { repo_id, .. } => {
                self.collect_target_facts(ctx, request, *repo_id).await
            }
        }
    }

    /// Resolve a collect target: rule 1 and rule 6, and deliberately nothing
    /// else.
    ///
    /// # Rule 2 produces no groups here, and that is not an omission
    ///
    /// A collect run's file set is *every* test file the branch holds — legacy
    /// builds it as the union of every plan's `test_files` on that branch
    /// (`manager/src/services/collect.rs:56-75`) — which is a property of the
    /// branch at **dispatch** time, not of the launch. `TargetFacts::groups` is
    /// consumed by exactly one thing, [`Self::gather_file_meta`] for the
    /// `TEST_META` scan, and that scan is unreachable for a collect target
    /// ([`ExclusivitySource::None`]). So an empty grouping here is the accurate
    /// value rather than a placeholder, and `service::dispatch_spec` builds the
    /// real one with `list_plans`, exactly where the source system builds its.
    ///
    /// # No platform, and it is refused rather than ignored
    ///
    /// `manager/src/services/collect.rs:128-149` passes `None` for every
    /// platform-derived argument of `submit_workflow`, and says why at
    /// `:146-148`: *"Collection runs never target a real platform (see the
    /// `None`s above), so there is nothing to be exclusive about."* A collect
    /// launch carrying a `platform_id` would therefore be a shape the source
    /// system cannot produce, and accepting it silently would mount that
    /// platform's kubeconfig, snapshot its `APP_VERSION`/`APP_BUILD` and admit
    /// its variables tier into a run that only enumerates. Refused, so the
    /// caller is told rather than surprised.
    ///
    /// It is also what keeps the queue bypass honest **from both ends**:
    /// [`Self::launch`] skips admission by kind, and this refusal means there
    /// would have been nothing to queue against even if it did not.
    ///
    /// # The two omitted branch tiers
    ///
    /// [`resolve_branch`]'s platform tier is unreachable here — there is no
    /// platform to read one from — so the chain is explicit-then-repository.
    /// That matches the source system, where the collect job is invoked with a
    /// branch chosen by its caller and falls back to
    /// `DEFAULT_COLLECT_BRANCH` (`collect.rs:19`, `"main"`) in the *analytics*
    /// layer rather than by consulting a platform. This gear keeps the
    /// repository default as the last tier instead of hard-coding `main`,
    /// because that default is the same fact expressed per repository and the
    /// collect trigger (Task 30) owns the `main` policy.
    async fn collect_target_facts(
        &self,
        ctx: &SecurityContext,
        request: &LaunchRequest,
        repo_id: Uuid,
    ) -> Result<TargetFacts, DomainError> {
        if request.platform_id.is_some() {
            return Err(DomainError::Validation {
                field: "platform_id".to_owned(),
                message: "a collect run enumerates test cases and never targets a platform"
                    .to_owned(),
            });
        }

        let repo_default = self.repo_default_branch(ctx, repo_id).await?;
        let branch = resolve_branch(request.branch.as_deref(), None, &repo_default);

        Ok(TargetFacts {
            groups: BTreeMap::new(),
            branch,
            exclusivity: ExclusivitySource::None,
            // No plan, so no declared timeout. `domain::timeout`'s
            // `RunKind::Collect` arm ignores this field and the configured
            // default alike, and reaches legacy's synthetic 600
            // (`collect.rs:106`) — the citation is on that arm.
            plan_timeout_seconds: None,
            // `collect.rs:110`, `validation: false` on the synthetic plan.
            is_validation: false,
            // Legacy's synthetic plan is named `format!("collect {}", repo.name)`
            // (`collect.rs:104`) and reaches `workflow_name_base` through the
            // plan submit path. The repository *name* is not on this launch and
            // fetching one for a run name would be a catalog round trip for
            // cosmetics, so the id stands in: `collect-{uuid}` mirrors the
            // custom-plan primary (`custom-{uuid}`) and is ASCII, 44 characters,
            // and `_`-free, so `name_base` passes it through unchanged.
            name_base: naming::name_base(NameSources {
                primary: &format!("collect-{repo_id}"),
                fallback: None,
                default_base: default_name_base(RunKind::Collect),
            }),
        })
    }
}

/// The file part of a pytest node id: everything before the **first** `::`.
///
/// Ported from `test_file_part` (`manager/src/services/test_bundles.rs:310-315`;
/// `value.find("::")` then `&value[..idx]`). Its doc comment, at `:306-309`, says
/// *"Strip a pytest
/// node-id suffix (`::Class`, `::Class::method`) to get the file path.
/// Filesystem operations - existence checks, bundling, tag reads - work on the
/// file, while the full node-id is still handed to the runner so pytest selects
/// the specific class/test"*). Legacy calls it at exactly the point this module
/// calls it: `manager/src/services/exclusivity.rs:417-418`, inside
/// `scan_test_meta`'s read loop, commented *"Strip a `::Class::method`
/// selection: `TEST_META` lives in the file."*
///
/// # Why the port needs it, in the dangerous direction
///
/// qa-catalog's `get_test_meta` resolves each path with `resolve_under_root`,
/// which `canonicalize()`s it and answers `Ok(None)` — hence `FileNotFound` — for
/// anything that is not a real file (`qa-catalog/src/domain/service/plans.rs:225-226`,
/// `:371-383`; `validate_rel_path` admits `::` as an ordinary path component, so
/// the failure is a not-found rather than a rejection). Neither gear stripped
/// `::` anywhere — `grep '"::"'` across both was zero hits — so a plan whose
/// `tests:` entries carry selectors had **every** such file counted unreadable,
/// its exclusivity vote discarded, and the run resolved **parallel**: a
/// destructive test silently losing its platform-to-itself guarantee, which is
/// the exact failure mode this module's own operator warning exists for.
///
/// # Semantics matched exactly, including the edges
///
/// * Only the **first** `::` splits, so `a.py::C::m` is `a.py`. Legacy uses
///   `find`, not `rfind`; `split_once` is the same thing.
/// * A leading `::` yields the **empty string**, not the input. That then fails
///   to resolve and is counted unreadable — the same outcome legacy reaches,
///   where `resolve_test_file_path(.., "", ..)` names a directory and
///   `read_to_string` fails.
/// * Nothing is trimmed, because `test_bundles.rs:310-315` does not trim either.
///
/// # Only the `TEST_META` read is stripped — the run name deliberately is not
///
/// Legacy derives a single test's run name from the **unstripped** node id:
/// `manager/src/services/argo.rs:723-728` passes it through
/// [`normalize_test_path`], which does not touch `::`, so the name is derived from
/// the whole node id. [`test_name_stem`] matches that, so do not "fix" the name
/// path. **No test distinguishes the two here**, because for realistic node-id
/// shapes `Path::file_stem` yields the same stem either way
/// (`tests/test_b.py::TestB::test_x` has one `.`, so the stem is `test_b`); the
/// claim rests on reading `normalize_test_path`, which is said plainly rather
/// than implied.
fn test_file_part(value: &str) -> &str {
    match value.split_once("::") {
        Some((file, _)) => file,
        None => value,
    }
}

/// Legacy's `normalize_test_path`, ported verbatim
/// (`manager/src/services/argo.rs:2441-2446`): trim, drop a leading `./`, drop a
/// leading `/`, then normalize backslashes to `/`.
///
/// **The order is legacy's and it is observable**, so it is not "tidied": the
/// `replace` runs *last*, which means a leading `.\` survives the `./` strip
/// (it is not `./` until after the replace, and by then the strip has run). Any
/// other order would produce a different name for that input.
///
/// It exists for the backslash step. `test_name_stem` previously omitted this
/// whole function while its doc claimed to match legacy, and the divergence was
/// found by running both rather than by reading:
///
/// ```text
/// "tests\\test_b.py"             port "tests\\test-b"        legacy "b"
/// "ui\\specs\\test_login.spec.ts" port "ui\\specs\\test-login.spec"  legacy "login.spec"
/// ```
///
/// On a non-Windows host `\` is not a path separator, so `file_stem` sees the
/// whole thing as one filename and the directory part ends up *inside* the run
/// name. Legacy's `replace('\\', "/")` exists because backslash-bearing paths do
/// occur in practice. The consequence was confined to the run name — and thence to
/// the `LIKE '{prefix}-%'` sequence `naming::next_sequence` counts — never to which
/// tests run.
fn normalize_test_path(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .replace('\\', "/")
}

/// The primary name source for a single-test run, ported from
/// `manager/src/services/argo.rs:723-728` and `:733`: normalize the path, take the
/// file stem, replace `_` with `-`, then strip a leading `test-` and trim.
///
/// All four steps, in legacy's order. [`normalize_test_path`] records why the
/// first one is not optional.
///
/// The `_` replacement is redundant under `naming::slugify`, which treats `_`
/// as a separator anyway — it is kept because the `test-` strip is applied
/// *after* it in the source system and would otherwise miss a `test_foo.py`.
fn test_name_stem(test_file: &str) -> String {
    let normalized = normalize_test_path(test_file);
    let stem = std::path::Path::new(&normalized)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("test")
        .replace('_', "-");
    stem.trim_start_matches("test-").trim().to_owned()
}

// ---------------------------------------------------------------------------
// Exclusivity: the tier cascade, which never fails a launch
// ---------------------------------------------------------------------------

impl<R: RunsRepository> LaunchService<R> {
    /// `launch ?? plan.yaml ?? OR(TEST_META over the files that will run) ??
    /// parallel`, and **it cannot fail**.
    ///
    /// `manager/src/services/exclusivity.rs:177-179`. A catalog failure while
    /// gathering inputs logs a warning and contributes nothing; the launch then
    /// fails later at dispatch for the real reason, if at all.
    ///
    /// Two short-circuits, both ported and both observable:
    ///
    /// * a launch override wins outright and does **no** work on the lower
    ///   tiers, so the same override produces the same answer whether or not any
    ///   file metadata could be fetched (`exclusivity.rs:181-187`);
    /// * a `plan.yaml` declaration decides without any file being consulted
    ///   (`exclusivity.rs:342-348`).
    ///
    /// # Three operator warnings live on these paths, and none is asserted
    ///
    /// `resolve_single_plan_exclusivity`'s unreadable-files warning, plus
    /// `resolve_one_nested_plan`'s unresolvable-plan warning and `scan_nested_plan`'s
    /// per-nested-plan unreadable-files warning. All three re-derive from legacy and
    /// none is covered by a test.
    ///
    /// **Corrected 2026-08-15.** This used to say no test *could* cover them,
    /// because the crate had no way to capture a `tracing` event. It has one
    /// now - `tracing-test` is a dev-dependency and
    /// `service::dispatch`'s log assertions use it - so the obstacle is gone
    /// and what remains is simply that the tests were not written. Left as a
    /// statement of the gap rather than silently downgraded to a claim of
    /// coverage.
    ///
    /// # The cascade has exactly one definition, and it is not here
    ///
    /// Every branch below ends in one of the core's two whole-rule entry points —
    /// [`exclusivity::resolve`] for a single plan, [`exclusivity::combine_nested`]
    /// for a custom plan, which is the split legacy itself has
    /// (`plan_tier` vs `custom_plan_tier`). An earlier version of this method
    /// re-implemented the launch tier inline and called `resolve_plan_tier`
    /// underneath it, which gave the tier cascade two definitions that agreed only
    /// by inspection. That is the drift hazard rule 1 of this task exists to
    /// eliminate, and it is invisible to unit tests on either side.
    ///
    /// What stays here is **only the decision to skip the I/O**, plus the three
    /// operator warnings, none of which the core can express: it takes files that
    /// have already been fetched, so it cannot tell the caller not to fetch them,
    /// and it has no unreadable count. The short-circuits are observable precisely
    /// as `get_test_meta` never being called.
    async fn resolve_exclusivity(
        &self,
        ctx: &SecurityContext,
        launch_override: Option<bool>,
        facts: &TargetFacts,
    ) -> Resolution {
        match &facts.exclusivity {
            ExclusivitySource::SinglePlan {
                plan_flag,
                include,
                exclude,
            } => {
                self.resolve_single_plan_exclusivity(
                    ctx,
                    launch_override,
                    facts,
                    *plan_flag,
                    include,
                    exclude,
                )
                .await
            }
            // No I/O, no tier, no launch override — the override is not
            // consulted either, because there is nothing for it to override: a
            // collect run has no platform and holds no lease, so `exclusive`
            // has no meaning for it. `resolve(None, None, &[], &[], &[])` is the
            // core's own spelling of "nothing decided", which yields
            // `Default`/parallel.
            ExclusivitySource::None => exclusivity::resolve(None, None, &[], &[], &[]),
            ExclusivitySource::NestedPlans(nested) => {
                // The launch tier wins outright for a custom plan too: legacy
                // checks the override **before** it dispatches on the intent at
                // all (`manager/src/services/exclusivity.rs:181-187`, above the
                // `match intent` at `:189`), so `custom_plan_tier` is never
                // entered and no nested plan is read. Routed through the core so
                // the `Launch` tier is still stamped in one place.
                if launch_override.is_some() {
                    return exclusivity::resolve(launch_override, None, &[], &[], &[]);
                }
                self.resolve_nested_exclusivity(ctx, facts, nested).await
            }
        }
    }

    /// A `Plan` or `Test` target: `plan.yaml`, then `TEST_META` over the files
    /// this run will execute.
    async fn resolve_single_plan_exclusivity(
        &self,
        ctx: &SecurityContext,
        launch_override: Option<bool>,
        facts: &TargetFacts,
        plan_flag: Option<bool>,
        include: &[String],
        exclude: &[String],
    ) -> Resolution {
        if launch_override.is_some() || plan_flag.is_some() {
            // No file is fetched. Passing an empty `files` is not a
            // simplification: with an upper tier holding an opinion,
            // `resolve_plan_tier` never looks at `files` at all, which is the
            // property `a_launch_override_ignores_every_lower_tier_input` asserts
            // against the core directly. Both tiers go in the same call so the
            // launch-vs-plan precedence stays settled in the core.
            return exclusivity::resolve(launch_override, plan_flag, &[], &[], &[]);
        }

        let (files, unreadable) = self.gather_file_meta(ctx, facts).await;
        let resolution = exclusivity::resolve(None, None, &files, include, exclude);

        // Ported from `manager/src/services/exclusivity.rs:440-448`, whose own
        // comment - at `:433-439`, not inside the predicate block - calls this
        // "the one failure mode here that an operator must see: a destructive
        // test would silently lose its platform-to-itself guarantee". The pure
        // core cannot emit it - it does no I/O and has no unreadable count - so
        // it lives here or nowhere.
        //
        // **No test asserts that this line is emitted.** Mutating this whole
        // `if` to `if false` leaves the suite green, which was checked rather
        // than assumed. Corrected 2026-08-15: this used to add "and none can",
        // on the grounds that the crate had no collector; `tracing-test` is now
        // a dev-dependency, so the emission is testable and simply is not
        // tested. What *is* tested is the state that produces the warning
        // (`an_entirely_unreadable_file_set_resolves_default_not_test_meta_false`)
        // and the equivalence of the predicate to legacy's, which is argued below
        // from the two definitions rather than observed. In production `tracing`
        // is initialised, so the control does reach the operator - the gap is in
        // the tests, not in the control - and it is the mitigating control for the
        // node-id-selector failure mode (see `test_file_part`).
        //
        // Legacy's predicate is `unreadable > 0 && flags.is_empty()`. This
        // branch has already established that `plan_flag` is `None`, so
        // `resolve_plan_tier` reduced to `aggregate_test_meta(&flags)`, which
        // is `None` exactly when `flags` is empty and `None` is exactly what
        // yields `ExclusiveTier::Default`. The two predicates are therefore the
        // same predicate.
        //
        // **The granularity matches, on this path.** A `Plan` or `Test` target is
        // one repository and one `plan.yaml` by construction, so legacy's
        // "once per nested plan" and this "once per launch" are the same scan.
        // A custom plan is the case where they could differ, and
        // `resolve_nested_exclusivity` warns per nested plan exactly as legacy
        // does rather than once over the union.
        //
        // Deliberately NOT "every file was unreadable": the
        // mixed case (most files missing, the rest dropped by the tag filter)
        // is covered, while a legitimately all-filtered-out run stays quiet
        // because it leaves `unreadable` at zero.
        if unreadable > 0 && resolution.tier == ExclusiveTier::Default {
            warn!(
                unreadable,
                in_scope = facts.groups.values().map(Vec::len).sum::<usize>(),
                branch = %facts.branch,
                "exclusivity: in-scope test file(s) could not be read and none of the rest \
                 declared anything; the run resolves as parallel",
            );
        }

        resolution
    }

    /// A custom plan: `custom_plan_tier`
    /// (`manager/src/services/exclusivity.rs:491-583`), which is a different rule
    /// from a plan run's and not a special case of it.
    ///
    /// # What is ported from that range, and what is not
    ///
    /// Only `:536-582` — the grouping loop and the combine. Three earlier blocks
    /// are deliberately absent, and one of them is a trap for whoever adds the
    /// feature it belongs to:
    ///
    /// * **`:496-512`, the DAG-node union.** Legacy unions the tests hanging off a
    ///   DAG plan's `nodes` into `plan.tests` before grouping, and its comment at
    ///   `:496-497` says why in the exact terms of this task's failure class:
    ///   skipping it *"would silently resolve every DAG plan as parallel"*. This
    ///   port has no `nodes` on a custom plan, so there is nothing to union and
    ///   nothing is lost today. **Whoever adds DAG plans must add this too**, or
    ///   they reintroduce precisely the divergence `plan_path` was added to close.
    /// * **`:513`, `expand_included_plans`.** No `included_plans` here either, with
    ///   the same obligation attached if one is ever added.
    /// * **`:515-530`, the single batched `list_plans_with_repos`.** Not ported
    ///   because this port does not need it — see "The per-plan cost" below.
    /// * `:531`'s `repo_lookup` is unnecessary: a [`CustomPlanEntry`] carries its
    ///   own `repo_id`, where legacy had to recover one from the nested plan.
    ///
    /// Per nested plan, in legacy's order:
    ///
    /// 1. resolve the nested plan's `plan.yaml`. `Some(declared)` goes to
    ///    `plan_flags` and **no file is read for that plan** (`:549-550`);
    /// 2. otherwise scan that plan's own files for `TEST_META`, **with no tag
    ///    filter** — *"a custom plan has no tag filter of its own"* (`:552-558`,
    ///    which passes `&[], &[]`) — and push the aggregate to `test_meta_flags`
    ///    when any file voted (`:557-561`);
    /// 3. a nested plan that could not be resolved contributes nothing, is
    ///    counted, and is warned about individually (`:540-546`).
    ///
    /// The two aggregates then go to [`exclusivity::combine_nested`] (`:579-582`),
    /// which is the function this whole path exists to reach: within one nested
    /// plan `plan.yaml` outranks `TEST_META`, and *across* plans the answer is an
    /// OR whose tier names the strongest source that actually contributed.
    ///
    /// # Why the double aggregation is not redundant
    ///
    /// Each nested plan's scan already ORs its own files, and `combine_nested`
    /// then ORs across plans — `aggregate_test_meta` applied twice. An OR of ORs
    /// is an OR, so a flat pass over the files that *were scanned* would give the
    /// same `exclusive` bool. (Not a flat pass over every file in the custom plan:
    /// a plan whose `plan.yaml` declared something has its files skipped
    /// entirely, so those two are different inputs.) What the two levels buy is
    /// the **warnings**: the per-plan aggregate is what makes "plan A was
    /// entirely unreadable" visible while plan B votes, which a flat pass cannot
    /// distinguish from "one file of many was missing". Legacy keeps them for the
    /// same reason
    /// (the warning at `:440-448`, its reason at `:433-439`, inside
    /// `scan_test_meta` — which legacy calls once per nested plan).
    ///
    /// # The per-plan cost, in the one function whose legacy twin warns about it
    ///
    /// Legacy builds **one** `list_plans_with_repos` map for all N nested plans,
    /// and the comment at `:515-520` exists specifically to say that a per-plan
    /// lookup "would repeat that whole listing N times on every custom-plan
    /// launch". This port does look up per nested plan — so the divergence is
    /// worth naming even though it is not legacy's pathology.
    ///
    /// What it costs, stated rather than implied. Per nested plan: one `get_plan`
    /// (two PEP round trips, a scoped repository read, a path canonicalize and a
    /// file read) and, for a plan that declared nothing, one `get_test_meta`. So K
    /// plans in one repository make K `get_test_meta` calls where the previous
    /// per-repository grouping made 1.
    ///
    /// Why that is still the right trade: legacy's N repetitions were of a **full
    /// plans-directory scan plus a scan per synced repo plus a
    /// `list_repositories` query** — which is why it cached. These are *targeted*
    /// reads of one known path, bounded by the number of distinct nested plans in
    /// one custom plan, and the per-plan granularity is not incidental — it is what
    /// makes `scan_nested_plan`'s warning able to name the plan that was
    /// unreadable, and what keeps each scan in its own repository. A batched
    /// `get_plans` on the catalog client would remove the K round trips without
    /// giving up either property, and is the obvious optimisation if a custom plan
    /// ever composes enough plans for this to matter.
    ///
    /// # It still cannot fail a launch
    ///
    /// A nested plan whose `plan.yaml` will not load — wrong branch, deleted
    /// repository, another tenant's `repo_id` — warns and contributes nothing
    /// (`:540-546`, and the whole-function contract at `:177-179`). The launch
    /// then fails at dispatch for the real reason, if at all.
    ///
    /// # The cross-tenant question, and where the answer is enforced
    ///
    /// `CustomPlanEntry::repo_id` has no foreign key and is not validated at
    /// write time, so an operator **can** store a custom plan naming another
    /// tenant's repository. That was already true before `plan_path` existed and
    /// `plan_path` adds no second reference — it is a path *inside* that
    /// `repo_id`, not a key into any table.
    ///
    /// What this call newly does with a foreign `repo_id` is ask qa-catalog for a
    /// plan under it, and qa-catalog refuses: `get_plan` resolves the repository
    /// row through a `qa.test_repo`/`get` scope compiled from **the caller's**
    /// context (`qa-catalog/src/domain/service/plans.rs:157-171`, the scope at
    /// `:101-111`) and answers `NotFound`. The same is true of `get_test_meta`
    /// (`:203-215`), which this path already used.
    ///
    /// **And the response discriminates nothing** — *given that every foreign
    /// `repo_id` takes the `Err` arm.* That conditional is the actual guarantee and
    /// the half that will rot, so it is stated before the conclusion rather than
    /// after it.
    ///
    /// The conclusion: a PEP denial, a deleted repository and a plan simply not on
    /// this branch all arrive as one `QaCatalogError`, all take the `Err` arm
    /// below, and all produce the same outcome — a warning in the launcher's log, a
    /// bump to `unresolved`, and no contribution. The launch still succeeds with the
    /// same run row. The log line is visible to an operator of *this* deployment,
    /// not to the caller.
    ///
    /// ## The conditional, and what breaks it
    ///
    /// **`qa_runs.exclusive_tier` is caller-readable and it does discriminate.** It
    /// says `Plan` when a nested `plan.yaml` resolved and declared something, and
    /// `Default`/`TestMeta` when none did. So the *only* reason a foreign `repo_id`
    /// is not an existence oracle for `(repo_id, branch, plan_path)` is that
    /// `get_plan` never succeeds for one — and that rests entirely on its scope
    /// being compiled from **the caller's** `ctx`.
    ///
    /// Give `get_plan` a system-context path, a plan cache keyed without a tenant,
    /// or a "shared repository" fast path, and `exclusive_tier` becomes that oracle
    /// immediately, with nothing in this file changing. This is the shape qa-catalog's
    /// own post-mortem records three times over: the fix closed the gap one column
    /// over from where it reopened. **If you are changing how `get_plan` resolves a
    /// repository, this is the paragraph that says why you cannot widen it.**
    ///
    /// The enforcement point is asserted, in the gear that owns it:
    /// `qa-catalog`'s `plan_and_test_meta_reads_are_scoped_to_the_callers_tenant`
    /// drives `get_plan` and `get_test_meta` under a foreign tenant against a real
    /// database and pins `NotFound`. It lives there rather than here on purpose — a
    /// qa-runs test could only assert `MockCatalog`, which returns one error for
    /// every cause and so cannot tell the two apart. What *is* tested here is the
    /// arm's behaviour
    /// (`an_unresolvable_nested_plan_contributes_nothing_and_does_not_fail_the_launch`).
    async fn resolve_nested_exclusivity(
        &self,
        ctx: &SecurityContext,
        facts: &TargetFacts,
        nested: &[NestedPlan],
    ) -> Resolution {
        let mut plan_flags: Vec<bool> = Vec::new();
        let mut test_meta_flags: Vec<bool> = Vec::new();
        let mut unresolved = 0usize;

        for plan in nested {
            match self.resolve_one_nested_plan(ctx, &facts.branch, plan).await {
                NestedContribution::Declared(flag) => plan_flags.push(flag),
                NestedContribution::Scanned(flag) => test_meta_flags.push(flag),
                NestedContribution::Nothing { unresolved: true } => unresolved += 1,
                NestedContribution::Nothing { unresolved: false } => {}
            }
        }

        // Legacy's own aggregate warning (`exclusivity.rs:569-577`), whose reason
        // it states inline at `:566-568`: *"say plainly when the answer rests on
        // partial data, so 'this custom plan resolved parallel' is never mistaken
        // for 'every plan it composes was checked and declared parallel'"*. This
        // is a different statement from the per-plan warning above and neither
        // replaces the other: that one says which plan was unreadable, this one
        // says how much of the custom plan the answer is missing.
        if unresolved > 0 {
            warn!(
                unresolved,
                nested_plans = nested.len(),
                branch = %facts.branch,
                "exclusivity: nested plan(s) of this custom plan could not be resolved; its \
                 flag was computed from the rest",
            );
        }

        exclusivity::combine_nested(
            exclusivity::aggregate_test_meta(&plan_flags),
            exclusivity::aggregate_test_meta(&test_meta_flags),
        )
    }

    /// One nested plan's contribution: its `plan.yaml` first, its own files
    /// second, nothing third.
    ///
    /// Split out of [`Self::resolve_nested_exclusivity`] so the loop above reads
    /// as the three-way accumulation legacy's does (`:549-563`) and each outcome
    /// has a name. The two `Nothing` cases are deliberately one variant with a
    /// flag rather than two variants, because they are the *same* contribution
    /// and differ only in whether legacy counts them: an unresolvable plan is
    /// counted and reaches the aggregate warning, a plan whose files simply had
    /// nothing to say is not (`exclusivity.rs:545` counts only the former).
    async fn resolve_one_nested_plan(
        &self,
        ctx: &SecurityContext,
        branch: &str,
        plan: &NestedPlan,
    ) -> NestedContribution {
        // No nested plan named, so there is nothing to resolve and this is *not*
        // legacy's unresolved case - see `NestedPlan::plan_path`. The files fall
        // straight through to the scan.
        let Some(plan_path) = &plan.plan_path else {
            return self.scan_nested_plan(ctx, branch, plan).await;
        };

        match self
            .catalog
            .get_plan(ctx, plan.repo_id, branch, plan_path)
            .await
        {
            // `plan.yaml` decided; this plan's files are never read
            // (`manager/src/services/exclusivity.rs:549-550`). A plan that
            // declared nothing falls through to its own scan (`:551-561`).
            Ok(resolved) => match resolved.exclusive {
                Some(flag) => NestedContribution::Declared(flag),
                None => self.scan_nested_plan(ctx, branch, plan).await,
            },
            Err(error) => {
                warn!(
                    repo_id = %plan.repo_id,
                    plan_path = %plan_path,
                    %branch,
                    %error,
                    "exclusivity: nested plan not resolvable; it contributes nothing",
                );
                NestedContribution::Nothing { unresolved: true }
            }
        }
    }

    /// One nested plan's `TEST_META` scan: `scan_test_meta`
    /// (`manager/src/services/exclusivity.rs:403-450`) for a single nested plan.
    ///
    /// The scan's repository is the nested plan's **own**, never a union across
    /// the custom plan: a custom plan can span repositories and each nested
    /// plan's `TEST_META` lives in its own (`exclusivity.rs:552-556`).
    async fn scan_nested_plan(
        &self,
        ctx: &SecurityContext,
        branch: &str,
        plan: &NestedPlan,
    ) -> NestedContribution {
        let (files, unreadable) = self
            .gather_group_meta(ctx, plan.repo_id, branch, &plan.files)
            .await;

        // No tag filter, and there is nowhere here to get one from - see
        // `ExclusivitySource`.
        let flags: Vec<bool> = files
            .iter()
            .filter_map(|file| exclusivity::file_declares_exclusive(file, &[], &[]))
            .collect();

        if let Some(flag) = exclusivity::aggregate_test_meta(&flags) {
            return NestedContribution::Scanned(flag);
        }

        // Legacy's `unreadable > 0 && flags.is_empty()` (`exclusivity.rs:440-448`),
        // per nested plan, for the reason its comment at `:433-439` gives: a
        // destructive test would otherwise silently lose its platform-to-itself
        // guarantee.
        //
        // **This is the identical predicate, not an equivalent one.** `flags` here
        // is the same vector legacy builds, and the `None` arm of
        // `aggregate_test_meta` *is* `flags.is_empty()` by that function's
        // definition. Worth stating precisely, because no test observes the
        // emission - the crate can capture events since `tracing-test` landed,
        // but this line has no assertion - so the argument from the two
        // definitions is the only evidence there is.
        // `resolve_single_plan_exclusivity`'s copy of this warning is by contrast a
        // *re-derivation* — it tests `tier == Default` — and says so.
        if unreadable > 0 {
            warn!(
                unreadable,
                in_scope = plan.files.len(),
                repo_id = %plan.repo_id,
                plan_path = plan.plan_path.as_deref().unwrap_or("<none>"),
                %branch,
                "exclusivity: in-scope test file(s) of a nested plan could not be read and \
                 none of the rest declared anything; it contributes nothing",
            );
        }
        NestedContribution::Nothing { unresolved: false }
    }

    /// Fetch `TEST_META` for every in-scope file, **omitting the ones that could
    /// not be read** and counting them.
    ///
    /// # Why the per-file fallback exists
    ///
    /// The source system reads each file individually and skips an unreadable
    /// one, so the readable files still vote
    /// (`manager/src/services/exclusivity.rs:411-432`; the read is `:421`).
    /// qa-catalog's batch
    /// `get_test_meta` is **all-or-nothing**: it fails the whole call with
    /// `FileNotFound` on the first file it cannot read
    /// (`qa-catalog/src/domain/service/plans.rs`, the `read_to_string` arm). If
    /// a batch failure simply dropped the group, one missing file would discard
    /// every *other* file's vote in that group - and a group containing one
    /// unreadable file plus one file declaring `exclusive: True` would resolve
    /// **parallel**. That is the dangerous direction, on an input the source
    /// system handles correctly.
    ///
    /// So the batch call is attempted first (one round trip in the healthy
    /// case) and, only on failure, retried per file to isolate the unreadable
    /// ones.
    ///
    /// **The cost when the catalog is wholly unavailable, stated rather than
    /// implied.** The fallback cannot tell "one file is missing" from "the
    /// catalog is down" - both arrive as one `QaCatalogError` - so an outage
    /// costs `1 + N` failed calls per group instead of one. That is strictly
    /// worse than the source system, which reads N files from a local checkout
    /// (`manager/src/services/exclusivity.rs:412-421`) where this makes N
    /// cross-gear calls. It is bounded by the plan's own file count and only
    /// ever paid on a path that is already failing, and the alternative is
    /// resolving a destructive suite as parallel - but it is not free, and a
    /// future `get_test_meta` that reported per-file failures would remove the
    /// fallback entirely.
    ///
    /// # A pytest node selector is stripped before the read
    ///
    /// [`test_file_part`], at the same point in the loop legacy strips it
    /// (`manager/src/services/exclusivity.rs:417-418`). Duplicates that stripping
    /// creates - two selections on one file - are **not** deduplicated, which is
    /// also legacy: `group_tests_by_plan` deduplicates on the full node id
    /// (`exclusivity.rs:459-463`), so legacy reads the same file twice too, and
    /// OR-ing a multiset gives the same answer as OR-ing a set. Keeping the
    /// duplicates also keeps `unreadable` and the warning's `in_scope` counted per
    /// in-scope *entry*, matching legacy's `files.len()`.
    ///
    /// # What must never happen here
    ///
    /// An unreadable file is **omitted**, never substituted with a default
    /// [`FileMeta`]. `qa_catalog_sdk::TestFileMeta` derives `Default`, so
    /// `FileMeta { path, ..from(meta) }` is an easy slip - and an admitted file
    /// always votes, so a default one votes `Some(false)`. That turns a
    /// `Default` resolution into a `TestMeta`-false one and, if the unreadable
    /// file was the destructive one, replaces its vote with a parallel one.
    async fn gather_file_meta(
        &self,
        ctx: &SecurityContext,
        facts: &TargetFacts,
    ) -> (Vec<FileMeta>, usize) {
        let mut metas = Vec::new();
        let mut unreadable = 0usize;

        for (repo_id, paths) in &facts.groups {
            let (group_metas, group_unreadable) = self
                .gather_group_meta(ctx, *repo_id, &facts.branch, paths)
                .await;
            metas.extend(group_metas);
            unreadable += group_unreadable;
        }

        (metas, unreadable)
    }

    /// One group's half of [`Self::gather_file_meta`]: the batch-then-per-file
    /// read for a single repository and file list.
    ///
    /// Split out because the custom-plan path groups by **nested plan** rather
    /// than by repository and so cannot iterate `facts.groups`, while every word
    /// of the reasoning above — the all-or-nothing batch, the per-file fallback,
    /// the node-id strip, and above all "an unreadable file is omitted, never
    /// defaulted" — applies identically to it. Duplicating it for the second
    /// caller is exactly how the two would drift apart on the one property that
    /// must not drift.
    async fn gather_group_meta(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        paths: &[String],
    ) -> (Vec<FileMeta>, usize) {
        let mut metas = Vec::new();
        let mut unreadable = 0usize;

        // Strip a `::Class::method` selection before the read: TEST_META
        // lives in the file. See `test_file_part`.
        let files: Vec<String> = paths
            .iter()
            .map(|path| test_file_part(path).to_owned())
            .collect();
        match self
            .catalog
            .get_test_meta(ctx, repo_id, branch, &files)
            .await
        {
            Ok(batch) => metas.extend(batch.into_iter().map(file_meta)),
            Err(error) => {
                warn!(
                    %repo_id,
                    branch = %branch,
                    %error,
                    "exclusivity: batched TEST_META read failed; retrying per file so the \
                     readable files still vote",
                );
                for path in &files {
                    let single = self
                        .catalog
                        .get_test_meta(ctx, repo_id, branch, std::slice::from_ref(path))
                        .await;
                    match single {
                        Ok(one) if !one.is_empty() => {
                            metas.extend(one.into_iter().map(file_meta));
                        }
                        // Unreadable, or answered with nothing. Either way
                        // it contributes no vote and is counted, never
                        // defaulted.
                        _ => unreadable += 1,
                    }
                }
            }
        }

        (metas, unreadable)
    }
}

// ---------------------------------------------------------------------------
// Persistence and the entry point
// ---------------------------------------------------------------------------

impl<R: RunsRepository> LaunchService<R> {
    /// Insert the run under a unique name, retrying a name collision a bounded
    /// number of times.
    ///
    /// `naming::next_sequence` is advisory - two concurrent launches on one
    /// prefix compute the same next number - so the uniqueness constraint
    /// decides and the loser recomputes. Ported from
    /// `manager/src/services/argo.rs:416-422` (three attempts, the number
    /// recomputed *inside* the loop) and `:677-686` (retry on the create's 409).
    /// See [`NAME_ATTEMPTS`].
    ///
    /// # The enumeration this does, and the repository method it is missing
    ///
    /// The source system's query is `WHERE workflow_name LIKE $1` with the
    /// pattern bound as `format!("{}-%", prefix_base)` - not an inlined literal
    /// (`manager/src/services/run_history.rs:452-468`;
    /// `list_persisted_workflow_names_by_prefix`, the SQL at `:460` and the bind at
    /// `:463`). A prefilter that `next_sequence` then re-checks exactly.
    /// **`RunsRepository` has no
    /// prefix-filtered read**, so this uses the uncapped `list` and filters in
    /// memory. The answer is identical - `next_sequence` re-filters what it is
    /// given either way - but the cost is not: every launch materialises every
    /// run in scope. Recorded as a repository-layer follow-up; it is a one-method
    /// addition to a file this task does not own.
    async fn create_run(
        &self,
        ctx: &SecurityContext,
        resolved: Resolved,
    ) -> Result<Run, DomainError> {
        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();
        let base = resolved.name_base.clone();
        let mut last: Option<DomainError> = None;

        for attempt in 1..=NAME_ATTEMPTS {
            let list_scope = self.run_scope(ctx, actions::LIST, None).await?;
            let existing = self.runs.list(&conn, &list_scope).await?;
            let number = naming::next_sequence(&base, existing.iter().map(|run| run.name.as_str()));
            let name = naming::run_name(&base, number);

            let create_scope = self.run_scope(ctx, actions::CREATE, None).await?;
            match self
                .runs
                .create(
                    &conn,
                    &create_scope,
                    tenant_id,
                    resolved.clone_new_run(name.clone()),
                )
                .await
            {
                Ok(run) => return Ok(run),
                Err(error @ DomainError::RunNameExists { .. }) => {
                    warn!(
                        %name,
                        attempt,
                        max_attempts = NAME_ATTEMPTS,
                        "run name already taken; recomputing the sequence",
                    );
                    last = Some(error);
                }
                Err(error) => return Err(error),
            }
        }

        Err(last.unwrap_or_else(|| {
            DomainError::Internal("run-name retry loop produced no error".to_owned())
        }))
    }

    /// Move a run between states, guarded by the state machine.
    ///
    /// `can_transition` is the mandatory pre-check: writing `update_state`
    /// directly would bypass the state machine entirely. It is asked about the
    /// state the **repository returned**, not about a literal, so it is not a
    /// self-evidently-true guard.
    ///
    /// A terminal transition also releases the run's live log channel, for the
    /// reason `service::dispatch::transition` gives: `LogSubscription::recv`
    /// answers `None` only when the channel is dropped, so a subscriber watching
    /// a run this path retires would otherwise hold its connection open forever.
    /// The reachable case is narrow but real — [`Self::abandon`] moves a refused
    /// launch to `Canceled` while it is still `Created`, and
    /// `RunLogBroadcaster::subscribe` mints a channel for any run id, so a client
    /// that subscribed the instant the run was created is exactly the one left
    /// hanging.
    async fn transition(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        to: RunState,
        patch: RunStatePatch,
    ) -> Result<(), DomainError> {
        if !state_machine::can_transition(run.state, to) {
            return Err(DomainError::IllegalTransition {
                id: run.id,
                state: run.state,
                action: format!("become {}", to.as_str()),
            });
        }
        let scope = self.run_scope(ctx, actions::CREATE, Some(run.id)).await?;
        let conn = self.db.conn()?;
        let moved = self
            .runs
            .update_state(&conn, &scope, run.id, run.state, to, patch)
            .await?;
        if moved {
            if state_machine::is_terminal(to) {
                self.logs.reap(run.id);
            }
            Ok(())
        } else {
            // The repository reports only whether the guarded UPDATE matched;
            // the state named here is the one this launch believed, which is
            // advisory by construction (`domain::repos`, "Guarded writes return
            // `bool`"). Nothing branches on it.
            Err(DomainError::IllegalTransition {
                id: run.id,
                state: run.state,
                action: format!("become {}", to.as_str()),
            })
        }
    }

    /// Retire a run whose admission was refused.
    ///
    /// **A deliberate divergence, because the alternative is worse.** The source
    /// system has no run row at launch - it persists one only on a successful
    /// submit (`manager/src/services/argo.rs:674`, `persist_submitted_run`) - so
    /// a rejected launch leaves nothing behind there. Here the row must exist
    /// before admission, because `QueueRepository::insert` takes an
    /// `OwnedRunId` and therefore needs a run to resolve. Left alone, a refused
    /// launch would strand a row in `Created` forever: never running, never
    /// terminal, and visible in every listing.
    ///
    /// So it is moved to `Canceled` with the refusal as its `error` and
    /// `started_at: None`, which is the only terminal state `Created` can
    /// legally reach (`domain::state_machine::can_transition`) - so a consumer
    /// that saw `created` finds the run closed rather than a dangling id.
    ///
    /// Best-effort: a failure here is logged and never replaces the refusal the
    /// caller actually needs to see.
    ///
    /// # What it is allowed to write down
    ///
    /// [`DomainError::disclosable`], which lives beside the variant list in
    /// `domain::error` rather than here. The value handed to
    /// `RunStatePatch::error` is persisted to `qa_runs.error`, served verbatim
    /// by `GET /runs/{id}`, so it crosses a trust boundary and must not be
    /// whatever `Admitter::admit` happened to return — a
    /// [`DomainError::Database`] cause is `From<toolkit_db::DbError>` and carries
    /// raw driver text.
    ///
    /// Tasks 14 and 15 write the same column from their own failure paths and must
    /// use the same rule; that is why the classification is not a private helper in
    /// this file, which is where it started.
    async fn abandon(&self, ctx: &SecurityContext, run: &Run, cause: &DomainError) {
        let finished_at = OffsetDateTime::now_utc();
        if !cause.disclosable() {
            warn!(
                run_id = %run.id,
                cause = %cause,
                "admission failed for a reason the caller is not entitled to see; the run \
                 records an opaque message and this line carries the detail",
            );
        }
        let recorded_reason = cause.recorded_text();
        let patch = RunStatePatch {
            started_at: None,
            finished_at: Some(finished_at),
            error: Some(recorded_reason.clone()),
        };
        if let Err(error) = self.transition(ctx, run, RunState::Canceled, patch).await {
            warn!(
                run_id = %run.id,
                %error,
                // NOT `original_cause`: this is what was written down, which for a
                // redacted cause is the opaque text and not the cause at all. The
                // real one is on the `warn!` above.
                recorded_reason = %recorded_reason,
                "could not retire the run whose admission was refused; it stays in `created`",
            );
        }
    }

    /// Record the admission outcome on the run, then either answer 202 or
    /// dispatch inline and answer 200.
    ///
    /// # Why this matches on the pair, and not on the admission alone
    ///
    /// [`Admission::Queued`] means the admitter wrote a `qa_run_queue` row, whose
    /// `NewQueueRow::platform_id` is not nullable - there is nothing to queue
    /// against without a platform. So `(Queued, None)` should not occur.
    ///
    /// It is nevertheless matched rather than filled in with a fallback. The
    /// previous shape was `run.platform_id.unwrap_or_else(Uuid::nil)` under a
    /// comment asserting the fallback was unreachable *because of what Task 14's
    /// admitter does* - an assertion about another module, backed by a silent
    /// sentinel. And nil is not an innocent placeholder here: it is this
    /// subsystem's platform-root / cross-tenant sentinel (see
    /// `domain::system_actor`), so a consumer aggregating queue depth by platform
    /// would have attributed the run to platform-root.
    ///
    /// So the unrepresentable outcome is reported as [`DomainError::Internal`] -
    /// **returned, not asserted**, because a domain service must not panic on
    /// another module's output. It fires before the state transition, so nothing
    /// is recorded. The run stays in `created` and the queue
    /// row the admitter wrote stays `queued`, which is recoverable rather than
    /// lost **if the queue row is coherent**: Task 14's tick claims that row and
    /// `Created -> Dispatching` is a legal transition, so the run resumes. That
    /// holds only because the row carries its *own* `platform_id`, which came from
    /// the admitter rather than from this run - and the whole reason this arm exists
    /// is that those two disagreed. If the row's platform is also wrong, the tick
    /// dispatches against the wrong platform's lease and the recovery is worse than
    /// the failure. So: recoverable if the row is coherent, and if it is not, this
    /// error is the only signal anybody gets.
    ///
    /// # It takes the whole [`Admitted`], not the decision
    ///
    /// The capacity slot has to outlive the submit, and the submit happens in
    /// here. Taking the decision alone left `launch` holding the slot with
    /// nothing requiring it to keep holding it — dropping it one line early
    /// compiled and left every test green, which is how it was found. Moving the
    /// whole value in means the early drop is a use-after-move.
    async fn settle(
        &self,
        ctx: &SecurityContext,
        run: Run,
        admitted: Admitted,
    ) -> Result<LaunchOutcome, DomainError> {
        let Admitted { admission, slot } = admitted;
        match (admission, run.platform_id) {
            (Admission::Queued { queue_id, .. }, Some(_)) => {
                self.transition(ctx, &run, RunState::Queued, RunStatePatch::default())
                    .await?;
                Ok(LaunchOutcome::Queued {
                    run_id: run.id,
                    queue_id,
                })
            }
            (Admission::Queued { queue_id, .. }, None) => Err(DomainError::Internal(format!(
                "run {} was queued as {queue_id} but carries no platform; a queue row \
                 without a platform is not representable",
                run.id
            ))),
            (Admission::Dispatch { queue_id }, _) => {
                self.dispatch_and_report(ctx, run, Some(queue_id), &slot)
                    .await
            }
            (Admission::Unqueued, _) => self.dispatch_and_report(ctx, run, None, &slot).await,
        }
    }

    /// `Created -> Dispatching`, hand off to the dispatcher, then report the run
    /// as the dispatcher left it.
    ///
    /// The run is re-read afterwards under its own `qa.run`/`get` scope, because
    /// `LaunchOutcome::Started` carries a whole [`Run`] and dispatch is what
    /// writes `execution_ref` and moves the state to `running`. Returning the
    /// row as it looked at insert would report `state: created` and
    /// `execution_ref: None` for a run that is demonstrably neither. If the
    /// re-read fails or the row has vanished, the pre-dispatch row is returned
    /// and the discrepancy logged - a launch that has already started must not
    /// be reported as failed.
    async fn dispatch_and_report(
        &self,
        ctx: &SecurityContext,
        run: Run,
        queue_id: Option<Uuid>,
        capacity: &CapSlot,
    ) -> Result<LaunchOutcome, DomainError> {
        self.transition(ctx, &run, RunState::Dispatching, RunStatePatch::default())
            .await?;
        self.dispatcher
            .dispatch_inline(ctx, run.id, queue_id, capacity)
            .await?;

        let started = if let Some(fresh) = self.reread(ctx, run.id).await {
            fresh
        } else {
            warn!(
                run_id = %run.id,
                "could not re-read the run after dispatch; reporting it as launched",
            );
            run
        };
        Ok(LaunchOutcome::Started {
            run: Box::new(started),
        })
    }

    async fn reread(&self, ctx: &SecurityContext, run_id: Uuid) -> Option<Run> {
        let scope = self.run_scope(ctx, actions::GET, Some(run_id)).await.ok()?;
        let conn = self.db.conn().ok()?;
        self.runs.get(&conn, &scope, run_id).await.ok().flatten()
    }
}

impl<R: RunsRepository> LaunchService<R> {
    /// Resolve everything a run row records, without writing anything.
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        request: &LaunchRequest,
        parameters: Vec<RunParameter>,
    ) -> Result<Resolved, DomainError> {
        // The platform read is the tenant-ownership check - see
        // `read_platform`. It runs before the catalog reads so a launch aimed at
        // somebody else's platform is refused before it costs a plan lookup.
        let platform = self.read_platform(ctx, request.platform_id).await?;

        // Rules 1, 2, 3 and (through `branch`) 6.
        let facts = self.resolve_target(ctx, request, platform.as_ref()).await?;

        let exclusivity = self
            .resolve_exclusivity(ctx, request.exclusive, &facts)
            .await;

        let timeout_seconds = resolve_timeout_seconds(
            request.timeout_seconds,
            facts.plan_timeout_seconds,
            self.default_timeout_seconds,
            request.target.kind(),
        );
        let timeout_at = OffsetDateTime::now_utc()
            .checked_add(time::Duration::seconds(saturating_i64(timeout_seconds)));

        Ok(Resolved {
            target: request.target.clone(),
            platform_id: request.platform_id,
            // Rule 6: the branch label *is* the recorded test version.
            test_version: facts.branch.clone(),
            // Snapshotted from the platform at launch, never re-derived: a
            // platform upgrade must not silently change a queued run's or a
            // re-run's `APP_VERSION` (`qa_runs_sdk::Run::app_version`).
            app_version: platform.as_ref().and_then(|p| p.observed_version.clone()),
            app_build: platform.as_ref().and_then(|p| p.observed_build.clone()),
            exclusivity,
            is_validation: facts.is_validation,
            parameters,
            include_tags: request.include_tags.clone(),
            exclude_tags: request.exclude_tags.clone(),
            source: request.source,
            schedule_id: request.schedule_id,
            timeout_at,
            name_base: facts.name_base,
        })
    }

    /// Launch a run: parity spec §3.4's rules, in order.
    ///
    /// # The order, and why each step is where it is
    ///
    /// 1. **Validate the parameters**, before any I/O. `normalize` then
    ///    `validate`, in that order - "normalized" is a precondition of
    ///    `validate`, not a description of it. A launch that will be rejected
    ///    must not force anything to be read, and the **normalized** list is
    ///    what gets persisted: Task 14 assembles the environment from the
    ///    *stored* parameters, so a raw `"  FOO  "` would become a variable
    ///    literally named `"  FOO  "`.
    /// 2. **Resolve.** Platform (which is the ownership check), then the target,
    ///    grouped by repository, guarded, with the branch resolved once.
    ///    Exclusivity cannot fail this step.
    /// 3. **Create the run row**, under a bounded name retry.
    /// 4. **Admit.** A refusal retires the row (see [`Self::abandon`]) and is
    ///    returned to the caller.
    /// 5. **Settle**: record the outcome and either answer 202 or dispatch
    ///    inline and answer 200.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidParameters`] for an illegal parameter set,
    /// [`DomainError::AmbiguousBranch`] for a multi-repository custom plan with
    /// no explicit branch, [`DomainError::Environments`] /
    /// [`DomainError::Catalog`] when the platform or the target cannot be
    /// resolved, [`DomainError::QueueFull`] / [`DomainError::ConcurrencyLimit`]
    /// when admission refuses, and [`DomainError::Database`] on a persistence
    /// failure.
    #[instrument(skip(self, ctx, request), fields(kind = request.target.kind().as_str()))]
    pub async fn launch(
        &self,
        ctx: &SecurityContext,
        request: LaunchRequest,
    ) -> Result<LaunchOutcome, DomainError> {
        // Step 1, before any I/O.
        let parameters = params::normalize(request.parameters.clone());
        params::validate(&parameters)?;

        // Step 2.
        let resolved = self.resolve(ctx, &request, parameters).await?;
        let exclusivity = resolved.exclusivity;

        // Step 3.
        let run = self.create_run(ctx, resolved).await?;
        info!(
            run_id = %run.id,
            run_name = %run.name,
            exclusive = run.resolved_exclusive,
            tier = exclusivity.tier.as_str(),
            test_version = ?run.test_version,
            "run created",
        );

        // Step 4. The whole `Admitted` goes into step 5, because the capacity
        // slot inside it must outlive the submit step 5 performs.
        //
        // **A collect run does not take this step.** Legacy's collect job calls
        // `ArgoService::submit_workflow` directly rather than
        // `run_dispatcher::launch` (`manager/src/services/collect.rs:128-149`),
        // so it never meets the queue, the per-platform lease, the depth limit
        // or `max_concurrent_runs`; and `manager/src/services/argo.rs:369-372`
        // names it as one of the paths that bypass admission and pass
        // `exclusive: false` explicitly. What that buys is the property the
        // hourly cycle depends on: a busy platform cannot starve the collection
        // that keeps analytics' expected-case numbers current.
        //
        // **That comment's other half is stale and is NOT reproduced here.** It
        // reads *"the two paths that bypass admission (`collect`,
        // `jira_poller`)"*, and `jira_poller` no longer does: its own module
        // doc says the opposite, in full — *"an auto-rerun goes through
        // `run_dispatcher::launch` (VHP-2618): it is a launch like any other and
        // must be admitted, queued behind an exclusive run, and have its
        // exclusivity resolved from the tiers. Before this it called
        // `ArgoService::submit_workflow` directly, which was the one bypass of
        // the per-platform queue"* (`manager/src/services/jira_poller.rs:8-15`).
        // VHP-2618 removed that bypass; only the comment at `argo.rs:369-372`
        // was not updated. An auto-rerun (Task 15's re-run, and qa-insights'
        // Task 31) is an ordinary launch and must stay one.
        //
        // Matched on the **kind**, not on `platform_id.is_none()`. The
        // platformless arm inside `Admitter::admit` reaches the same
        // `Unqueued` outcome, and relying on it would make the bypass a
        // consequence of `collect_target_facts` refusing a platform — a
        // correctness property held up by a validation two functions away, and
        // one that would silently disappear the day that refusal is relaxed.
        let admitted = if matches!(run.target.kind(), RunKind::Collect) {
            self.admitter.bypass(ctx).await?
        } else {
            match self.admitter.admit(ctx, &run).await {
                Ok(admitted) => admitted,
                Err(error) => {
                    self.abandon(ctx, &run, &error).await;
                    return Err(error);
                }
            }
        };

        // Step 5.
        self.settle(ctx, run, admitted).await
    }
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
