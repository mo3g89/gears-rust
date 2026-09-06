//! qa-runs-internal "system actor" `SecurityContext` factories.
//!
//! The lifecycle background tasks — the dispatcher tick, claim reconciliation,
//! the queue TTL sweep, the control-plane timeout sweep, and (Phase B) the
//! schedule ticker — have no end-user `SecurityContext` to forward but still run
//! through the same PEP-enforced service layer as every other caller. These
//! factories mint the stable, audit-correlatable identity those flows use: every
//! system call carries `subject_id = QA_RUNS_SYSTEM_ACTOR_UUID` and
//! `subject_type = "qa_runs.system"`.
//!
//! Structure copied from `qa-catalog/src/domain/system_actor.rs`, which in turn
//! mirrors the site-specific-factory idiom of
//! `gears/system/account-management/.../domain/system_actor.rs`: one named
//! factory per legitimate call site, each logging a `tracing` line under the
//! `qa_runs.system_actor` target, so "where does qa-runs elevate to system?"
//! stays greppable and auditable. A new background flow must add a new factory
//! here — a deliberate review-magnet.
//!
//! # The nil/tenant-bound split, which is not cosmetic
//!
//! Half of these factories bind a tenant and half do not, and the difference is
//! a safety property rather than a style choice.
//!
//! **A nil-tenant context produces zero constraints, and a service method that
//! asked `PolicyEnforcer::access_scope` about one would get an empty
//! constraint set, which fails compilation and lands as
//! [`crate::domain::error::DomainError::Forbidden`] — every service method in
//! this gear calls `access_scope` (never
//! `access_scope_with(require_constraints(false))`), so a nil-tenant context
//! reaching the PEP is *denied* unless the deployment's policy returns a
//! covering constraint set for this subject.**
//!
//! That was the load-bearing claim of this module, and until Task 13's review
//! it was asserted rather than exercised for an ordinary, PEP-routed nil-tenant
//! context — `ctx(Uuid::nil())` appeared in no test.
//! `domain::service::launch`'s `a_nil_tenant_context_is_denied_and_writes_nothing`
//! still pins exactly that: an end-user nil-tenant `ctx` reaching `launch`
//! (which still calls `access_scope` like every other service method) gets
//! `Forbidden`, no run row, no dispatch.
//!
//! **It is not, any more, the story for the six nil-tenant factories below.**
//! Each is minted for exactly one caller, and — since the nil-tenant
//! enumerations were moved to `domain::elevated` — that caller no longer
//! passes the resulting context to `access_scope` at all: it hands it, unused
//! beyond the audit-logging construction below, to
//! [`crate::domain::elevated::enumeration_scope`], a named seam that elevates
//! past the PEP instead of asking it. No deployment policy grant is required
//! for these six reads to work, and the PDP is never consulted for them — see
//! that module's doc for the full argument, and for why an unscoped
//! cross-tenant *read* here is not the security regression a covering *write*
//! grant would be.
//!
//! So the nil-tenant
//! factories are **enumeration-only**: they name the cross-tenant reads a sweep
//! needs, and every *write* that follows is bound to the row's own tenant
//! through the paired tenant-bound factory, which — unlike the enumeration
//! context — is still minted, still carries a resolved tenant, and still goes
//! through the PEP exactly as any other tenant-bound call in this gear does.
//! That pairing is why the list below reads as pairs — an enumeration step and
//! the write step it feeds — rather than as unrelated names: dispatch, claim
//! reconciliation, the TTL sweep, the timeout sweep, watcher re-attachment, and
//! the schedule tick.
//!
//! ## The split is enforced by a type, at the point of construction
//!
//! [`build_inner`] — the *only* thing in this file that calls
//! `SecurityContext::builder`, verified by grep — takes `Option<TenantBound>`, and
//! `TenantBound::new` refuses [`Uuid::nil`]. So a tenant-bound context cannot be
//! built from an unchecked `Uuid` anywhere in this module, in any build profile,
//! independently of how anything is formatted. Every tenant-bound factory takes
//! `TenantBound` because it has nothing else to pass.
//!
//! That sentence is only true because `TenantBound` is declared in a **private
//! inner module** rather than here: a tuple struct's unmarked field is private to
//! its declaring *module*, so declared alongside the factories it would have left
//! `build_inner(Some(TenantBound(Uuid::nil())))` compiling. See the
//! [`tenant_bound`] module.
//!
//! Without that, `for_ttl_expiry(Uuid::nil())` compiled and produced exactly the
//! platform-root context this header calls enumeration-only — and Task 14 feeds
//! these from decoded database columns
//! (`domain::repos::ExpiredRow::tenant_id`, `ClaimAge::tenant_id`,
//! `TimeoutCandidate::tenant_id`), so a column that decoded to nil would have
//! reached them.
//!
//! **Why a type rather than a `debug_assert!`.** The escape is *not* only a
//! silently-skipped sweep row. Before the nil-tenant enumerations were moved
//! to `domain::elevated`, the clause above — "unless the deployment's policy
//! returns a covering constraint set for this subject" — was the
//! configuration the enumeration factories *required* in order to work at
//! all; it no longer is, and the Authorization note below states why.
//! What the clause still names is the risk `TenantBound` closes for a
//! *write*: without it, a nil-tenant context that reached `access_scope` for
//! a write action — rather than being refused at construction — would, in a
//! deployment where the PDP grants `qa_runs.system` a covering constraint set
//! for that action, compile to a platform-wide write scope. A `debug_assert!`
//! is inert in release, which is exactly where that matters.
//!
//! **Why the parameter type and not a test.** The first attempt at this put
//! `TenantBound` on the five public factory signatures and policed it with a
//! source-reading test (`every_tenant_bound_factory_takes_a_checked_tenant`)
//! that collected lines containing both `fn for_` and `Uuid`. That test was the
//! subsystem's **twelfth inert guard**, and it was the guard added to replace the
//! eleventh: rustfmt puts a signature past 100 columns on multiple lines, after
//! which no single line contains both needles. Verified by execution rather than
//! argued — a rustfmt-canonical two-parameter `for_timeout_enforcement(tenant_id:
//! Uuid, …)` left every test in this module **passing**, and
//! `for_timeout_enforcement(Uuid::nil(), …)` returned a context whose
//! `subject_tenant_id()` was nil. The escape was reachable and the guard was
//! silent. It is deleted; `build_inner`'s parameter type replaces it, because a
//! type cannot be defeated by a line break.
//!
//! **The hole that remains, named rather than implied.** Nothing in the type
//! system distinguishes "this factory is for enumeration" from "this factory is
//! for writes" — `build_inner(None)` is legitimate for the six nil-tenant
//! factories and would be wrong for a seventh that then performed writes. That is
//! not a type-level property and is not claimed as one; it is what
//! [`tests::every_factory_in_this_module_is_classified`] covers, by forcing every
//! `for_*` factory in this file onto exactly one of the two lists.
//!
//! # Authorization note
//!
//! **The seven tenant-bound contexts do not bypass the PEP.** Every write
//! call still asks the PDP for a decision and compiles the returned
//! constraints into an `AccessScope`, scoped to the row's resolved tenant.
//! The deployment's `AuthZ` policy must grant the `qa_runs.system` subject the
//! scopes those writes need; under a deny-all policy for that tenant the task
//! fails closed and logs — it never falls back to an unscoped query.
//!
//! **The six nil-tenant contexts do bypass the PEP, deliberately, and by
//! name.** Each is consumed only by
//! `crate::domain::elevated::enumeration_scope`, which returns
//! `AccessScope::allow_all()` directly rather than asking the PDP anything —
//! the one place in this gear's production paths that call appears, and the
//! only place it is meant to. No deployment policy grant is required, none is
//! consulted, and there is no deny-all case for these six reads to fail
//! closed against. What still fails closed is the *write* that follows each
//! one, under the tenant-bound factory above.

use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Hand-picked actor UUID (trailing bytes spell `qarsys`), stable across
/// processes so audit sinks can correlate qa-runs system invocations under one
/// identity. Cannot collide with any v4 actor UUID.
///
/// The `cf03` group continues the subsystem's series: `cf01` is
/// account-management, `cf02` is qa-catalog.
pub const QA_RUNS_SYSTEM_ACTOR_UUID: Uuid = uuid::uuid!("00000000-0000-cf03-0000-716172737973");

/// `subject_type` stamped on every qa-runs system-actor context.
const QA_RUNS_SYSTEM_SUBJECT_TYPE: &str = "qa_runs.system";

/// [`TenantBound`] lives in its own module **so that the rest of this file cannot
/// bypass its constructor.**
///
/// A tuple struct's unmarked field is private to the *module* that declares it, not
/// to the type. Declared directly in `system_actor`, `TenantBound(Uuid)` would have
/// left `build_inner(Some(TenantBound(Uuid::nil())))` compiling from any factory in
/// this file — so the header's claim that a tenant-bound context "cannot be built
/// from an unchecked `Uuid` anywhere in this module" would have been false by one
/// call, which is the exact shape of the mistake that whole paragraph exists to
/// record. Found in this round's own falsification pass, after the claim was
/// written.
///
/// This is deliberately **stronger than `OwnedRunId`**, whose field is private to
/// `domain::repos::runs_repo` and therefore reachable from the rest of that module.
mod tenant_bound {
    use toolkit_macros::domain_model;
    use uuid::Uuid;

    /// A tenant id that is known not to be [`Uuid::nil`].
    ///
    /// The parameter type of `build_inner` and so of every tenant-bound factory,
    /// which is what makes the nil/tenant-bound split a property of the type system
    /// rather than of the caller remembering. See `super`'s header for why the
    /// guarantee has to hold in release builds.
    ///
    /// The only constructor is [`TenantBound::new`], and it is fallible. A caller
    /// that reads a tenant id out of a database column therefore has to decide, in
    /// code, what to do when the column is nil — the intended answer being "log and
    /// skip this row", which is the same fail-closed outcome the PEP would have
    /// produced, but visible.
    ///
    /// # `#[domain_model]`, and why it is here
    ///
    /// The public domain types of this crate carry it: the row and token types
    /// of `domain::repos::runs_repo` and `queue_repo`, the executor port's
    /// vocabulary in `domain::ports::run_executor`, and `DomainError`. This
    /// type and `service::launch::Admission` were the only two domain value types
    /// without it, which is either correct or a DE0309 finding, and
    /// `cargo gears lint --dylint` is not installed so the gate cannot say
    /// which.
    ///
    /// **The modules are named and not counted, deliberately.** This paragraph
    /// shipped "verified by count: six / seven / twelve / one" and three of the
    /// four numbers were wrong — the real figures were 7 / 7 / 10 / 0, and a
    /// parenthetical had "corrected" the one that was already right. A count in
    /// a doc comment is a fact about a file that no build step re-reads.
    ///
    /// It is applied because the reasoning does not need the lint. The attribute is
    /// **not** about serialization — this type is never serialized. It implements
    /// `DomainModel` and **rejects infrastructure types in fields at compile time**,
    /// so the marker is accurate for a domain value type in the domain layer and the
    /// field check is a live guarantee: a future field carrying a `sea_orm` or
    /// `http` type would fail to build. Verified to compile before being applied.
    #[domain_model]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TenantBound(Uuid);

    impl TenantBound {
        /// `None` when `tenant_id` is nil.
        ///
        /// Nil is this subsystem's platform-root sentinel, not a tenant: a context
        /// carrying it is the cross-tenant enumeration identity, and using it for a
        /// write is the escape this type exists to close.
        #[must_use]
        pub fn new(tenant_id: Uuid) -> Option<Self> {
            (!tenant_id.is_nil()).then_some(Self(tenant_id))
        }

        /// The checked id.
        #[must_use]
        pub const fn get(self) -> Uuid {
            self.0
        }
    }
}

pub use tenant_bound::TenantBound;

/// Internal builder shared by every factory, and **the only call to
/// `SecurityContext::builder` in this module.**
///
/// `scope_tenant = None` falls back to the platform-root sentinel
/// ([`Uuid::nil`]) for cross-tenant enumeration flows. See this module's header
/// for why that is enumeration-only.
///
/// # The parameter type is the guard
///
/// [`TenantBound`], not `Uuid`. Because this is the single construction point,
/// that one parameter type is what makes "a tenant-bound system context is never
/// built from an unchecked id" true of the whole module — in every build profile
/// and regardless of how any signature happens to be wrapped. It replaced a
/// source-reading test that a multi-line signature defeated; the module header
/// records that failure and the residual hole this does *not* close.
///
/// # Panics
///
/// Never in practice: both required builder fields are set unconditionally
/// below.
#[allow(
    clippy::expect_used,
    reason = "both builder fields are statically set; the expect anchors the impossible-failure invariant"
)]
fn build_inner(scope_tenant: Option<TenantBound>) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(QA_RUNS_SYSTEM_ACTOR_UUID)
        .subject_type(QA_RUNS_SYSTEM_SUBJECT_TYPE)
        .subject_tenant_id(scope_tenant.map_or_else(Uuid::nil, TenantBound::get))
        .build()
        .expect("QA_RUNS_SYSTEM_ACTOR_UUID + tenant_id are always present")
}

/// Emit the audit line every factory shares. `tenant_id` is rendered as the
/// nil UUID for the enumeration-only factories, which is what they actually
/// carry.
fn log_site(site: &'static str, tenant_id: Uuid) {
    tracing::info!(
        target: "qa_runs.system_actor",
        site,
        tenant_id = %tenant_id,
        "qa-runs system actor constructed",
    );
}

/// Dispatcher tick, enumeration step — the cross-tenant "which platforms have
/// queued rows" listing (`QueueRepository::platforms_with_queued_rows`).
/// Platform-scoped (nil tenant).
#[must_use]
pub fn for_dispatch_enumeration() -> SecurityContext {
    log_site("dispatch_enumeration", Uuid::nil());
    build_inner(None)
}

/// Dispatcher tick, per-platform step: claiming and dispatching one platform's
/// rows. Tenant-bound to the platform's owning tenant, because every write it
/// makes — `mark_dispatching`, `set_execution_ref`, `update_state` — is a write
/// to that tenant's rows.
#[must_use]
pub fn for_dispatch(tenant: TenantBound) -> SecurityContext {
    log_site("dispatch", tenant.get());
    build_inner(Some(tenant))
}

/// Claim reconciliation, enumeration step — `QueueRepository::all_claims`
/// across every tenant. Platform-scoped (nil tenant).
#[must_use]
pub fn for_claim_reconciliation() -> SecurityContext {
    log_site("claim_reconciliation", Uuid::nil());
    build_inner(None)
}

/// Claim reconciliation, per-claim step: releasing one claim. Tenant-bound to
/// the claim's own tenant (`domain::repos::ClaimAge::tenant_id`, which travels
/// with the row precisely so this context can be minted).
#[must_use]
pub fn for_claim_release(tenant: TenantBound) -> SecurityContext {
    log_site("claim_release", tenant.get());
    build_inner(Some(tenant))
}

/// Queue TTL sweep, enumeration step — cross-tenant expiry candidates.
/// Platform-scoped (nil tenant).
#[must_use]
pub fn for_ttl_sweep() -> SecurityContext {
    log_site("ttl_sweep", Uuid::nil());
    build_inner(None)
}

/// Queue TTL sweep, per-row step: expiring one tenant's row and writing the
/// mandatory `run-queue row EXPIRED` alert (guide line 96).
///
/// One context per expired row, not one per sweep — a write under the
/// enumeration's covering scope would land under whichever tenant the loop
/// happened to be on rather than the row's own, so the sweep has nowhere else
/// to get the tenant from than `domain::repos::ExpiredRow::tenant_id`.
#[must_use]
pub fn for_ttl_expiry(tenant: TenantBound) -> SecurityContext {
    log_site("ttl_expiry", tenant.get());
    build_inner(Some(tenant))
}

/// Control-plane timeout sweep, enumeration step —
/// `RunsRepository::list_timeout_candidates` across every tenant.
/// Platform-scoped (nil tenant).
#[must_use]
pub fn for_timeout_sweep() -> SecurityContext {
    log_site("timeout_sweep", Uuid::nil());
    build_inner(None)
}

/// Control-plane timeout sweep, per-run step: cancelling one run. Tenant-bound
/// to `domain::repos::TimeoutCandidate::tenant_id`, which travels with the id
/// for exactly this reason.
#[must_use]
pub fn for_timeout_enforcement(tenant: TenantBound) -> SecurityContext {
    log_site("timeout_enforcement", tenant.get());
    build_inner(Some(tenant))
}

/// Watcher re-attachment, enumeration step —
/// `RunsRepository::list_watch_candidates` across every tenant.
/// Platform-scoped (nil tenant).
#[must_use]
pub fn for_watch_scan() -> SecurityContext {
    log_site("watch_scan", Uuid::nil());
    build_inner(None)
}

/// Everything one run's result observation does, bound to the run's own tenant
/// (`domain::repos::WatchCandidate::tenant_id`, which travels with the id for
/// exactly this reason): the pre-attach read that fetches the execution
/// reference, and then every write `service::ingest` makes while draining that
/// execution's events.
///
/// # One factory for two steps, and why that is not the module's rule bending
///
/// The rule is one factory per *call site*, and this is one site: attaching a
/// result observer to a run. The read and the drain are two statements of the
/// one operation, and splitting them would produce two audit labels for a
/// single decision to observe a run — which makes "where does qa-runs elevate
/// to system?" harder to answer, not easier.
///
/// **It is a write factory, and the write is not obvious from the name.**
/// `service::ingest` stamps `ctx.subject_tenant_id()` into
/// `qa_run_test_results.tenant_id` on every per-test row it stores, so a
/// context minted from anything but the run row's own tenant writes child rows
/// under the wrong tenant. That is why the watcher's context is derived from
/// the scanned row and never from whichever caller happened to dispatch the
/// run.
#[must_use]
pub fn for_result_ingest(tenant: TenantBound) -> SecurityContext {
    log_site("result_ingest", tenant.get());
    build_inner(Some(tenant))
}

/// Phase B schedule ticker, enumeration step — cross-tenant due-schedule
/// listing. Platform-scoped (nil tenant).
#[must_use]
pub fn for_schedule_tick() -> SecurityContext {
    log_site("schedule_tick", Uuid::nil());
    build_inner(None)
}

/// Phase B schedule ticker, per-schedule step: launching one schedule's run
/// through the same `LaunchService` a manual launch uses
/// (`cpt-cf-qa-fr-runs-schedules`). Tenant-bound to the schedule's own tenant.
#[must_use]
pub fn for_schedule_fire(tenant: TenantBound) -> SecurityContext {
    log_site("schedule_fire", tenant.get());
    build_inner(Some(tenant))
}

/// Flushing one run's accumulated log lines into `qa_run_logs` — a write —
/// and, since Task 13 (review finding #50), reading back that same run's
/// per-node resume position before a re-attach — a `GET`. Tenant-bound to
/// the run's own tenant either way; the two actions share this one factory
/// because both are `RunLogArchive` acting on a run's own log, not because
/// the read was folded in as an afterthought.
///
/// **There is no `for_log_archive_sweep`, and that is deliberate.**
/// `for_schedule_tick` / `for_schedule_fire` exist as a pair because a sweep
/// must first *enumerate* rows under a nil-tenant context and then write per
/// tenant. `LogArchive::flush_due` enumerates nothing from the database — its
/// work list is an in-memory map and each entry already carries its tenant. So
/// there is no nil-tenant read to authorize, and minting a nil-tenant context
/// with nothing to justify it would widen this gear's elevation surface for
/// free.
#[must_use]
pub fn for_log_archive(tenant: TenantBound) -> SecurityContext {
    log_site("log_archive", tenant.get());
    build_inner(Some(tenant))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! Pin the shared invariants every factory MUST satisfy (mirrors
    //! qa-catalog's and account-management's system-actor test blocks): stable
    //! subject id, `qa_runs.system` subject type, and correct tenant binding.
    //!
    //! # How the exhaustiveness guard actually works, and how it used to not
    //!
    //! The two lists below are hand-maintained, so a guard that only compares
    //! them against each other cannot notice a factory that was added to
    //! neither. The shipped version asserted `names.len() == 10` twice — once
    //! raw and once deduplicated — which catches a factory on *both* lists and
    //! is **vacuous** for one on neither: the vectors are unchanged, so the
    //! count is still ten. That was proven by adding an eleventh unclassified
    //! nil-tenant *write* factory and watching the suite stay green.
    //!
    //! [`every_factory_in_this_module_is_classified`] therefore reads this
    //! module's own source with `include_str!` and derives the declared set from
    //! it, which is the shape the plan proposes for `no_production_path_uses_
    //! allow_all` (plan line 5687, "a `#[test]` that reads the crate's own
    //! sources").
    //!
    //! **One hardcoded count survives, and here is what it actually does.** An
    //! earlier version of this paragraph claimed "the hardcoded `10`s are gone",
    //! which is false — one is right there in
    //! [`every_factory_in_this_module_is_classified`] — and believing it would get
    //! a live assertion deleted. (It reads `13` since the run-log-persistence
    //! Task 3 added `for_log_archive`, up from `12` after Task 16c added the
    //! watch-scan/result-ingest pair; the paragraphs above describe the version
    //! that read `10`, and the numeral is the only thing about them that changed.)
    //!
    //! The correction that was *relayed* with that finding is also wrong, so it is
    //! not repeated here: the surviving `10` is **not** "what catches a
    //! `#[cfg(test)]` attribute appearing above the factories and truncating the
    //! parse". Break-tested — with the count assertion disabled and
    //! [`source_outside_tests`] made to truncate to nothing, the **set comparison
    //! alone** still fails, because `classified` holds every label and `declared`
    //! holds none. Every "the parse went wrong" case the count catches, the set
    //! comparison catches too.
    //!
    //! What the count uniquely catches is one case, verified by break-test: a
    //! further factory that **was** correctly added to one of the two lists. Then
    //! both sets hold the same names, the set comparison agrees, and only the
    //! count objects. That is a deliberate speed bump rather than a correctness guard —
    //! it forces a third edit and, with it, a look at the enumeration/write
    //! classification, which is this module's stated "a new background flow must
    //! add a new factory here — a deliberate review-magnet". Keep it for that
    //! reason, and not for a reason it does not have.
    //!
    //! [`every_factory_in_this_module_is_classified`]: self::every_factory_in_this_module_is_classified
    use super::*;

    /// The tenant every tenant-bound assertion uses. Non-nil, which
    /// [`TenantBound`] now makes a compile-time-visible requirement rather than
    /// a convention.
    fn a_tenant() -> TenantBound {
        TenantBound::new(Uuid::from_u128(0xDEAD_BEEF_FACE_CAFE)).expect("non-nil")
    }

    /// Every nil-tenant factory, with its name. Enumeration-only: see the
    /// module header for why a nil tenant is denied for writes.
    ///
    /// The label is not decoration — it is the `site` field
    /// [`log_site`] stamps, so the classification test comparing these labels
    /// against the source's `for_*` names also pins that the audit line names
    /// the factory it came from.
    fn platform_scoped() -> Vec<(&'static str, SecurityContext)> {
        vec![
            ("dispatch_enumeration", for_dispatch_enumeration()),
            ("claim_reconciliation", for_claim_reconciliation()),
            ("ttl_sweep", for_ttl_sweep()),
            ("timeout_sweep", for_timeout_sweep()),
            ("watch_scan", for_watch_scan()),
            ("schedule_tick", for_schedule_tick()),
        ]
    }

    /// Every tenant-bound factory, applied to `tenant`. Same label rule as
    /// [`platform_scoped`].
    fn tenant_bound(tenant: TenantBound) -> Vec<(&'static str, SecurityContext)> {
        vec![
            ("dispatch", for_dispatch(tenant)),
            ("claim_release", for_claim_release(tenant)),
            ("ttl_expiry", for_ttl_expiry(tenant)),
            ("timeout_enforcement", for_timeout_enforcement(tenant)),
            ("result_ingest", for_result_ingest(tenant)),
            ("schedule_fire", for_schedule_fire(tenant)),
            ("log_archive", for_log_archive(tenant)),
        ]
    }

    /// This module's own source, truncated at the test module's attribute.
    ///
    /// Truncated for two reasons. It is semantically right — a factory is
    /// declared outside the test module or it is not a factory — and it removes
    /// the whole class of self-matching, where the parsers below would otherwise
    /// find their own `"fn for_"` needle in this file's test source and report
    /// twelve factories for ten. (The literal in this function sits *after* the
    /// real attribute, so splitting on the first occurrence is unaffected by it.)
    fn source_outside_tests() -> &'static str {
        let src = include_str!("system_actor.rs");
        src.split("#[cfg(test)]")
            .next()
            .expect("split always yields at least one part")
    }

    /// Every `for_*` factory this module declares, read out of its own source.
    ///
    /// Returns the names with the `for_` prefix removed, which is the spelling
    /// the two lists above use as labels.
    ///
    /// # What defeats this parser, stated rather than hoped
    ///
    /// * It matches `fn for_` anywhere on a non-`//` line and takes everything
    ///   up to the first `(`, so it survives any visibility (`pub`,
    ///   `pub(crate)`, bare) **and** survives rustfmt breaking a long signature:
    ///   rustfmt never breaks between a function's name and its `(`, so the name
    ///   and the paren are always on one line and stripping to the first paren
    ///   still yields the name. **Verified by execution, not by inspection** — a
    ///   sibling guard that assumed the *parameters* stayed on the signature line
    ///   was silently inert for exactly this reason, so the distinction matters:
    ///   what this parser needs on one line is `fn for_name(`, which rustfmt
    ///   guarantees; what that one needed was the whole parameter list, which
    ///   rustfmt does not.
    /// * It **is** defeated by a factory that does not begin with `for_` — that
    ///   is the module's naming convention and the thing this guard depends on —
    ///   by one declared inside a `/* */` block comment or a string literal, and
    ///   by one whose `fn` keyword and name are separated by a line break, which
    ///   rustfmt does not produce.
    fn declared_factories() -> Vec<String> {
        let mut names: Vec<String> = source_outside_tests()
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//"))
            .filter_map(|line| line.split_once("fn for_"))
            .filter_map(|(_, tail)| tail.split('(').next())
            .map(str::to_owned)
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn platform_scoped_factories_use_nil_tenant() {
        for (label, ctx) in platform_scoped() {
            assert_eq!(ctx.subject_id(), QA_RUNS_SYSTEM_ACTOR_UUID, "{label}");
            assert_eq!(
                ctx.subject_type(),
                Some(QA_RUNS_SYSTEM_SUBJECT_TYPE),
                "{label}"
            );
            assert_eq!(
                ctx.subject_tenant_id(),
                Uuid::nil(),
                "{label} enumerates across tenants and must not bind one"
            );
        }
    }

    #[test]
    fn tenant_bound_factories_carry_the_supplied_tenant() {
        let tenant = a_tenant();
        for (label, ctx) in tenant_bound(tenant) {
            assert_eq!(ctx.subject_id(), QA_RUNS_SYSTEM_ACTOR_UUID, "{label}");
            assert_eq!(
                ctx.subject_type(),
                Some(QA_RUNS_SYSTEM_SUBJECT_TYPE),
                "{label}"
            );
            assert_eq!(
                ctx.subject_tenant_id(),
                tenant.get(),
                "{label} writes rows and must be bound to the row's own tenant"
            );
        }
    }

    /// **The split is only meaningful if it covers everything, and this is what
    /// makes that a live property.** The declared set comes from the module's own
    /// source, so a factory added to neither list fails here — which the
    /// hand-maintained-count version it replaced could not do. See the test
    /// module's header.
    #[test]
    fn every_factory_in_this_module_is_classified() {
        let declared = declared_factories();
        assert_eq!(
            declared.len(),
            13,
            "the parser found {} `for_*` factories in this module's source; if a \
             factory was genuinely added, classify it below - do not raise this \
             number on its own: {declared:?}",
            declared.len(),
        );

        let tenant = a_tenant();
        let mut classified: Vec<String> = platform_scoped()
            .into_iter()
            .map(|(name, _)| name.to_owned())
            .chain(
                tenant_bound(tenant)
                    .into_iter()
                    .map(|(name, _)| name.to_owned()),
            )
            .collect();

        let mut deduped = classified.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            classified.len(),
            "a factory appears on both lists, so the enumeration/write split is \
             not disjoint: {classified:?}"
        );

        classified.sort_unstable();
        assert_eq!(
            classified, declared,
            "every `for_*` factory declared in this module must appear on \
             exactly one of the two lists; left = classified, right = declared \
             in the source"
        );
    }

    // There is deliberately **no** `every_tenant_bound_factory_takes_a_checked_tenant`
    // here. It existed, it was inert against a rustfmt-wrapped multi-line
    // signature, and the guarantee it was trying to police now lives in
    // `build_inner`'s `Option<TenantBound>` parameter — which no formatting can
    // defeat and which no test has to be kept honest about. The module header
    // records the failure and what it does not cover.

    /// `TenantBound` is what closes the nil-tenant write escape: nil is this
    /// subsystem's
    /// platform-root sentinel, so it is not a tenant and cannot become one.
    #[test]
    fn a_nil_tenant_cannot_be_bound() {
        assert!(
            TenantBound::new(Uuid::nil()).is_none(),
            "nil is the enumeration identity; binding a write to it produces \
             exactly the platform-root context this module calls read-only"
        );
        let real = Uuid::from_u128(0x0A11);
        assert_eq!(
            TenantBound::new(real).map(TenantBound::get),
            Some(real),
            "a real tenant id round-trips unchanged"
        );
    }

    /// The subject id is a stable audit identity, so it is pinned as a literal
    /// rather than only compared against itself: a rename that changed the
    /// constant would otherwise pass every assertion above.
    #[test]
    fn the_actor_uuid_is_stable() {
        assert_eq!(
            QA_RUNS_SYSTEM_ACTOR_UUID.to_string(),
            "00000000-0000-cf03-0000-716172737973"
        );
        assert_ne!(
            QA_RUNS_SYSTEM_ACTOR_UUID,
            Uuid::nil(),
            "a nil subject id would be indistinguishable from an unset one"
        );
    }
}
