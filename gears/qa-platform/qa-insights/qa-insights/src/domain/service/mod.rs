//! Domain service layer — the composition tier.
//!
//! The repositories persist, the ports reach other gears, and this layer is
//! what puts them in order. Per-concern submodules, arriving on the plan's
//! schedule:
//!
//! * [`ingest`] — the projection every result row lands through, Task 13,
//!   extended by Task 14.
//! * [`reconcile`] — the self-healing sweep that keeps this gear's projection
//!   current with qa-runs, Task 15, plus Task 16's operator rebuild. It has
//!   been this gear's only ingest path since day one — see its own header.
//! * [`results`] — the two flat `OData` collections, Task 17. The first
//!   *read*-only service here, and the first that is not on the ingest path.
//! * [`dashboard`] — the dashboard aggregate, Task 18, and Task 19's coverage
//!   view beside it. The first *aggregate* read: [`results`] pages rows out
//!   unchanged, this one reduces them, and Tasks 20-27 follow its shape.
//! * [`analytics`] — the overview and its build-tests drill-down, Task 25b. It
//!   follows [`dashboard`]'s shape and is the first service here to hold a
//!   [`Clock`](crate::domain::ports::Clock) and to reach **three** systems in one
//!   request: qa-catalog for the universe, this gear's database for the rows, and
//!   qa-environments for the platform labels.
//! * [`saved_views`] — CRUD over `qa_analytics_saved_views`, Task 28. The first
//!   **write** path in Phase B and the first resource in this gear with an
//!   owner narrower than the tenant: every read and write here additionally
//!   calls [`AccessScope::ensure_owner`] on the compiled scope, so a PDP that
//!   grants a tenant-wide `qa.saved_view` scope (as [`ingest`]'s and
//!   [`dashboard`]'s do for `qa.test_result`) still narrows to the caller's own
//!   rows rather than exposing every owner's views. See that module's header
//!   for the full argument, including why this is the sanctioned substitution
//!   for legacy's `X-Analytics-Owner` header.
//! * [`collect`] — the collect trigger, the report the runner posts back, and
//!   the (callable, not yet ticked) hourly cycle, Task 30. Task 29 already
//!   shipped the *read* half of this table
//!   ([`analytics::AnalyticsService::collect_counts`]); this is the write half,
//!   and it is the **second** write path in Phase B — see that module's header
//!   for why its authorization answer is not [`saved_views`]'s answer copied
//!   over: one of its two endpoints is a system-actor callback with no PDP
//!   grant at all, which [`ingest`] is the precedent for and [`saved_views`] is
//!   not.
//! * [`jira`] — the JIRA settings surface, the outbound status read and the
//!   bug registry's list and filing path, Tasks 32-33, extended by Task 35
//!   with the two methods [`jira_poller`] consumes.
//! * [`jira_poller`] — the poller service: one tenant-scoped pass over every
//!   open bug, resolving each against JIRA and, when auto-rerun is on and a
//!   new build has appeared, launching a rerun through the normal admission
//!   path. Task 35, **ticked and wired onto [`AppServices`] by Task 40** — this
//!   said "callable and tested, not yet ticked or wired onto [`AppServices`]"
//!   until then, and that struct's field doc records the forecast being
//!   discharged. `crate::gear`'s `jira_poller_ticker` is the loop.
//! * [`notify`] — the notification service: settings CRUD, the audit log,
//!   the test/preview surfaces and the send-once path over
//!   `domain::notify::routing`/`domain::notify::render` (Tasks 36-38). It
//!   **is** a field of [`AppServices`], generic over `N: NotifyRepository`
//!   exactly like every other repository parameter here — unlike
//!   [`jira_poller`], it has a REST caller in this same task (the four
//!   `/qa/v1/settings/notifications*` routes), so it could not wait for Task
//!   40 the way a purely-ticked service can. Its two egress ports are real
//!   `ServiceDeps` fields (`slack_client`, `mail_client`), and **Task 40 bound
//!   the real adapters** (`infra::notify::SlackOagwClient`,
//!   `infra::notify::UnsupportedMailClient`) in place of the inert stand-ins
//!   `gear.rs` carried from Task 38's fix round 1 (R103).
//! * [`tenants`] — which tenants the three tickers work on, Task 40. The only
//!   service here with no request-scoped caller at all, and the only place in
//!   the crate that compiles a scope for a nil-tenant actor; its own header
//!   carries why, what it costs, and why the plan's first candidate for the
//!   question could not work.
//!
//! **Still to come, at the end of Phase C**: nothing calls
//! [`notify::NotifyService::notify_run_completed`]. The send path, its claim
//! lifecycle and its audit log all ship and are tested, but no producer routes
//! a `run.canceled` / `run.queue_expired` / `schedule.fired` event into it. A
//! transactional broker consumer's routing table skipped all three through
//! Task 40, on the grounds that Task 40's charter was binding what exists, not
//! adding a routing arm and the per-schedule settings port R104 says it would
//! need; that consumer and the event-broker dependency it needed were deleted
//! once it was established that no deployment ever registered the client it
//! needed (`crate::gear`'s header). Wiring this producer now needs a source for
//! those three events that is not a broker route, which is a design question
//! this deletion did not answer — recorded in `crate`'s own header beside the
//! other two release-gate items.
//!
//! # Two callers do not compile their `AccessScope` from the PEP, and neither
//! # is a shortcut
//!
//! Every *request-driven* caller in this subsystem compiles its `AccessScope`
//! from a `PolicyEnforcer` decision. [`ingest`] does not, and the reason is
//! stated there in full rather than here: the tenant is not caller-supplied,
//! and the PEP round-trip a system actor would make is one the platform's
//! shipped authorization plugin denies outright. [`tenants`] does not either,
//! for a related but distinct reason: its one read is not tenant-bound at
//! all — it is the cross-tenant enumeration that *finds* the tenants the
//! other two tickers then bind to — so there is no PEP decision that shape of
//! read could even ask for meaningfully. It reaches for
//! [`crate::domain::elevated::enumeration_scope`] instead of a PEP call; see
//! that module's doc, and read this section before adding a third non-PEP
//! scope anywhere in this gear.
//!
//! **Task 16 is the first request-driven caller, and it does use the PEP.**
//! [`reconcile::ReconcileService::rebuild`] compiles its scope from
//! [`PolicyEnforcer`] and hands it down, which is why
//! [`ingest::IngestService::write_run_projection`] (the write half of what
//! `reproject_run` used to be as one function — Task 5a's split, on
//! [`reconcile`]'s header) takes an `AccessScope` parameter rather than
//! building one: two callers with two different sources for the
//! same value is exactly the shape a function must not hide.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use authz_resolver_sdk::pep::ResourceType;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use time::Duration;
use toolkit_db::DBProvider;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use tracing::warn;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{CollectMetrics, JiraPollMetrics};
use crate::domain::ports::{
    CatalogReader, Clock, EnvironmentReader, JiraClient, MailClient, RunsLauncher, RunsReader,
    SlackClient,
};
use crate::domain::repos::{
    CollectRepository, JiraRepository, NotifyRepository, ResultsRepository, SavedViewsRepository,
    WatermarkRepository,
};

pub mod analytics;
/// The `(resource_type, action)` pairs this gear's PEP enforces, and the
/// distinct resource types among them. The source side of the permission
/// catalog's anti-drift test - review finding #1.
pub mod authz_surface;
pub mod collect;
pub mod dashboard;
pub mod ingest;
pub mod jira;
pub mod jira_poller;
pub mod notify;
pub mod reconcile;
pub mod results;
pub mod saved_views;
pub mod tenants;

#[cfg(test)]
pub mod test_support;

#[cfg(test)]
mod unscoped_read_guard_tests;

#[cfg(test)]
mod resources_tests;

/// Run one metric emission so that it cannot fail the path it is measuring.
///
/// Copied, contract and mechanism, from qa-runs' `domain::service::emit`.
///
/// # Why this exists when the port's contract already forbids failing
///
/// [`crate::domain::ports::metrics`] states the contract — an implementation
/// must not panic, must not block, and has no error to propagate by
/// construction — and [`crate::infra::metrics::QaInsightsMetricsMeter`]
/// satisfies it structurally: every method is one `add` or one `record` on an
/// instrument it already holds, with no `?`, no fallible lookup and no panic
/// path.
///
/// Neither of those covers the call site. Both services take
/// `Arc<dyn CollectMetrics>` / `Arc<dyn JiraPollMetrics>`, so what they hold is
/// whatever was injected: a later adapter, a different gear's adapter copied
/// across, a `debug_assert!` somebody adds inside one. "Metrics must not change
/// behaviour" is a property of the *collect and poll paths*, and a property of
/// those paths cannot be discharged by a promise written in another module — a
/// promise is exactly what a defect breaks. So the emission is guarded here,
/// where the paths are, and each call site's
/// `a_broken_metrics_adapter_does_not_fail_..` test drives a deliberately
/// panicking port through it.
///
/// # It is silent
///
/// A caught panic is dropped rather than logged. Logging here would be a log
/// line **per emission**, and on the per-bug path that is one line per open bug
/// per pass — the failure mode the observability constraints name explicitly,
/// arriving exactly when the process can least absorb it. The panic hook has
/// already run by the time control returns here, so the panic itself is not
/// invisible: it reaches stderr like any other.
///
/// [`std::panic::AssertUnwindSafe`] is sound for the reason it is normally
/// unsound: the state a panicking emission may have left inconsistent is that
/// implementation's own instrument state, and nothing in this crate ever reads
/// it back. A metric this gear cannot record is a metric this gear drops.
///
/// # It latches off, and that is the half the guard alone does not give
///
/// Catching is not enough on its own. A *persistently* broken adapter — a
/// poisoned instrument lock is the realistic shape — panics on **every** call,
/// and the default panic hook writes a line to stderr each time before control
/// returns here. So the first caught panic latches `silenced`, and every later
/// emission through that latch returns without calling anything. Deliberately
/// permanent: an emission that panicked once has no claim on being retried, the
/// alternatives (a rate limit, a backoff) are state and policy on a path whose
/// whole contract is that it changes nothing, and a metric that stops is a
/// visibly flat series — a better failure than a log flood. Nothing resets it;
/// a restart does.
///
/// **The latch is the caller's, not a global**, and each service owns one. A
/// broken `bug` emission then silences the poller and leaves the collect
/// service reporting, which is both the more useful production behaviour and
/// what keeps the `a_broken_metrics_adapter_..` tests from silencing every
/// other metric test in the binary — a global would make them do exactly that
/// under a threaded `cargo test`, where this crate's suites share a process.
///
/// # Precondition: this crate unwinds
///
/// `catch_unwind` catches nothing under `panic = "abort"`, where the first
/// panicking emission would take the process instead. The workspace sets
/// `panic = "unwind"` explicitly in `[profile.release]`, and the dev and test
/// profiles inherit the same default, so the guard is live in every profile
/// this gear is built under today. **Changing that setting silently disables
/// everything documented above**: the two `a_broken_metrics_adapter_..` tests
/// would abort rather than fail, so the suite would report a crashed binary
/// rather than a regression here.
///
/// `Relaxed` on both accesses: the latch orders nothing and guards no data. The
/// whole cost of the weakest ordering is that a racing thread may read `false`
/// once more and produce one more panic, against paying for a fence on a path
/// whose contract is that it costs nothing.
pub(in crate::domain::service) fn emit(silenced: &AtomicBool, record: impl FnOnce()) {
    if silenced.load(Ordering::Relaxed) {
        return;
    }
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(record)).is_err() {
        silenced.store(true, Ordering::Relaxed);
    }
}

/// `DB` provider alias.
///
/// Parameterized with [`DomainError`] directly, like qa-catalog's and qa-runs'
/// and unlike qa-environments': every transaction in this gear runs repository
/// calls that return `DomainError`, so an `Err` rolls the transaction back while
/// preserving the domain variant instead of flattening it to a database error.
/// `gear::init` has built the provider this way since Task 9; this alias is the
/// name the service layer uses for it.
pub(crate) type DbProvider = DBProvider<DomainError>;

/// Authorization resource types and their PEP-supported properties.
///
/// **Only the types something in this crate actually queries under are
/// declared**, which is the rule all three shipped siblings state and qa-runs
/// states most explicitly: an unused `pub(crate)` constant is dead code under
/// this workspace's `-D warnings`, and a resource type with no call site is a
/// guess about a vocabulary rather than a decision. So there was exactly one
/// through Task 16, Task 28 added the second, Task 32 the third
/// ([`resources::JIRA_CONFIG`]) and Task 33 the fourth
/// ([`resources::JIRA_BUG`]); Tasks 36-39 add `qa.notification_config` when
/// they compile their first scope.
pub(crate) mod resources {
    use super::ResourceType;
    use toolkit_security::pep_properties;

    /// `qa_test_results`, `qa_test_case_results` and, since Task 29,
    /// `qa_test_case_collect` for one read.
    ///
    /// **The first two** are one resource type over two tables, and it is not
    /// a relaxation of the one-scope-per-resource-type rule that
    /// `domain::repos` states: a case-level row has no independent lifecycle —
    /// it is written, replaced and deleted only as part of its run's
    /// projection, by the same `ResultsRepository::upsert_run_results` call,
    /// which takes exactly **one** `AccessScope` for both tables. Splitting
    /// the type would mean compiling two scopes for one indivisible write and
    /// deciding what to do when they disagree. qa-runs' `qa.schedule` covers
    /// `qa_schedules` plus `qa_schedule_ticks` on the same reasoning.
    ///
    /// **The third does not share that reasoning, and is recorded separately
    /// so the difference is not lost.** `qa_test_case_collect` has its own
    /// writer (Task 30's collect job) and no indivisible write shared with the
    /// other two; `analytics::AnalyticsService::collect_counts` reads it under
    /// this same scope only because the read is one field of the same
    /// aggregate its `case_rows_for_runs` and `list_for_universe` reads
    /// already fill under it, and declaring a resource type with exactly one
    /// caller for one field would be the "guess about a vocabulary" this
    /// module's own header argues against. `collect_counts`' doc carries the
    /// hazard this reuse creates: [`pep_properties::RESOURCE_ID`] below is
    /// declared for the first two tables' own ids, and a policy returning an
    /// id constraint would apply it to `qa_test_case_collect`'s own,
    /// unrelated id column instead — silently, not as a failure.
    ///
    /// The two properties are the pair every sibling declares: rows are
    /// tenant-owned, and a caller addresses one by id.
    pub const TEST_RESULT: ResourceType = ResourceType::from_static(
        TEST_RESULT_NAME,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// [`TEST_RESULT`]'s name as a `&'static str`.
    ///
    /// One literal, two consumers. `ResourceType::name()` borrows from the
    /// `ResourceType`, and a `const` is materialised as a temporary at each use
    /// site, so `resources::TEST_RESULT.name()` cannot produce a `&'static str`
    /// for [`super::DomainError::UnsupportedScope`] to carry. Declaring the
    /// literal here and building the descriptor from it keeps the PEP resource
    /// type and the error's `resource` field impossible to disagree about.
    pub const TEST_RESULT_NAME: &str = "qa.test_result";

    /// `qa_analytics_saved_views`. Task 28, and the **first resource type in
    /// this gear that declares [`pep_properties::OWNER_ID`]** — [`TEST_RESULT`]
    /// and `qa-runs`' `qa.schedule` both stop at tenant-plus-resource because
    /// their rows are shared within a tenant; a saved view is genuinely
    /// per-user, which is what the entity's `owner_col = "owner_id"`
    /// (`infra::storage::entity::saved_view`) declares on the persistence side.
    ///
    /// Declaring the property here is necessary but not sufficient: a
    /// deployment's policy still has to *return* an `owner_id` constraint for
    /// this resource type, or `domain::service::saved_views::SavedViewsService`
    /// would be the only thing standing between a tenant-wide grant and every
    /// owner's views. That module's `scope` helper is the compensating control —
    /// see its header for why every call there also applies
    /// [`toolkit_security::AccessScope::ensure_owner`].
    pub const SAVED_VIEW: ResourceType = ResourceType::from_static(
        SAVED_VIEW_NAME,
        &[
            pep_properties::OWNER_TENANT_ID,
            pep_properties::RESOURCE_ID,
            pep_properties::OWNER_ID,
        ],
    );

    /// [`SAVED_VIEW`]'s name as a `&'static str`, for [`TEST_RESULT_NAME`]'s
    /// reason.
    pub const SAVED_VIEW_NAME: &str = "qa.saved_view";

    /// `qa_jira_config` and `qa_jira_poller_config` — the JIRA settings
    /// singletons. Task 32.
    ///
    /// # The first resource type here that does **not** declare `RESOURCE_ID`
    ///
    /// [`TEST_RESULT`] and [`SAVED_VIEW`] both do, and so does every resource
    /// type in all three sibling gears, so this is a deliberate break in a
    /// convention rather than an omission — and it is a break the convention's
    /// own reason permits. `RESOURCE_ID` is declared so a policy can narrow a
    /// caller to particular rows *a caller can address*. Neither of these two
    /// tables has such a row: each is a per-tenant singleton, the `id` column is
    /// a surrogate that appears in no request, no response and no route, and
    /// nothing in this gear ever reads one by id. A policy returning a
    /// `resource_id` constraint for this type could therefore only ever be
    /// naming a UUID nobody can see.
    ///
    /// Declaring it anyway would not be harmless. `save_config` and
    /// `save_poller_config` reach `.scope_unchecked(scope)` for their insert —
    /// the documented no-op (`libs/toolkit-db/src/secure/db_ops.rs:376-385`) —
    /// so a compiled `resource_id` constraint would filter the *read* half of
    /// this service's read-then-write and be discarded by the write. Not
    /// declaring the property means that scope cannot compile in the first
    /// place, which closes the hole at the source instead of guarding it.
    /// [`super::refuse_scope_beyond_tenant`]'s own "rejected alternatives" list
    /// rejects dropping `RESOURCE_ID` from [`TEST_RESULT`]; that argument is
    /// about a resource whose rows genuinely are addressable by id, and it does
    /// not transfer.
    ///
    /// # `refuse_scope_beyond_tenant` was never the alternative, and the
    /// # remaining axis is closed elsewhere
    ///
    /// **Corrected in fix round 1.** An earlier revision of this doc described
    /// the `resource_id` hazard as "the shape
    /// [`super::refuse_scope_beyond_tenant`] exists to refuse", implying that
    /// guard was the alternative to not declaring the property. It is not: that
    /// function only rejects properties *other than*
    /// [`pep_properties::OWNER_TENANT_ID`] (`super::refuse_scope_beyond_tenant`,
    /// the `offending` filter), so it passes an `In` or `InTenantSubtree` tenant
    /// scope through unchanged.
    ///
    /// That is a real axis and it is **not** closed by this constant: a
    /// parent-tenant grant compiles to a scope spanning several tenants, and
    /// fix round 1's finding 4 found `save_jira_config`'s read-then-write
    /// carrying an arbitrary in-scope tenant's credential reference across
    /// because of it. It is closed where it arises instead —
    /// [`JiraRepository::get_config`](crate::domain::repos::JiraRepository::get_config)
    /// takes an explicit `tenant_id` and filters on it, and every caller passes
    /// `ctx.subject_tenant_id()`.
    pub const JIRA_CONFIG: ResourceType =
        ResourceType::from_static(JIRA_CONFIG_NAME, &[pep_properties::OWNER_TENANT_ID]);

    /// [`JIRA_CONFIG`]'s name as a `&'static str`, for [`TEST_RESULT_NAME`]'s
    /// reason.
    ///
    /// `qa.jira_config` rather than `qa.jira_settings`, and one type rather than
    /// two: the two tables are one operator-facing concern — "how this tenant
    /// talks to JIRA" — and a deployment that could grant one without the other
    /// would be able to let a subject change the poll cadence of an integration
    /// whose URL and credential reference it cannot see. [`JIRA_BUG`] is a
    /// *different* resource, declared separately below.
    pub const JIRA_CONFIG_NAME: &str = "qa.jira_config";

    /// `qa_jira_bugs` — the registry itself, as opposed to [`JIRA_CONFIG`]'s two
    /// connection-settings singletons. Task 33's first query under it:
    /// [`jira::JiraService::open_bugs`] and
    /// [`jira::JiraService::file_bugs`].
    ///
    /// # No `RESOURCE_ID`, [`JIRA_CONFIG`]'s reason and not [`TEST_RESULT`]'s
    ///
    /// Neither of Task 33's two operations addresses one bug by its own `id`:
    /// the list takes an optional `(repo_id, plan_path)` pair, and the filing
    /// path finds a bug by `test_name` (the local re-file probe) or mints one —
    /// no route, request or response on this task's two endpoints ever names a
    /// `qa_jira_bugs.id`. Declaring the property anyway would let a policy
    /// compile a `resource_id` constraint that only [`JiraRepository::upsert_bug`]'s
    /// insert could apply — filtering nothing on the caller's actual read paths
    /// — while looking, from the resource declaration alone, as though every
    /// bug-registry read were addressable by id. Task 35's poller reaches every
    /// method this resource type gates by `jira_key`, never by `id`, so nothing
    /// upstream of this commit needs the property either.
    pub const JIRA_BUG: ResourceType =
        ResourceType::from_static(JIRA_BUG_NAME, &[pep_properties::OWNER_TENANT_ID]);

    /// [`JIRA_BUG`]'s name as a `&'static str`, for [`TEST_RESULT_NAME`]'s
    /// reason.
    pub const JIRA_BUG_NAME: &str = "qa.jira_bug";

    /// `qa_notification_config`, `qa_notification_log` and
    /// `qa_run_notifications` — the tenant's notification settings, the
    /// audit trail, and the dedupe claim table. Task 38.
    ///
    /// **No `RESOURCE_ID`, [`JIRA_CONFIG`]'s reason.** All three tables are
    /// either a per-tenant singleton or rows this gear's own routes never
    /// address by id (the audit log is listed, not fetched one row at a
    /// time; the claim table has no route of its own at all) — the same
    /// shape [`JIRA_CONFIG`]'s own doc argues from, so it is not repeated
    /// here in full.
    pub const NOTIFICATION_CONFIG: ResourceType =
        ResourceType::from_static(NOTIFICATION_CONFIG_NAME, &[pep_properties::OWNER_TENANT_ID]);

    /// [`NOTIFICATION_CONFIG`]'s name as a `&'static str`, for
    /// [`TEST_RESULT_NAME`]'s reason.
    pub const NOTIFICATION_CONFIG_NAME: &str = "qa.notification_config";
}

/// Authorization actions.
///
/// # `rebuild` is the action string, and it was chosen rather than reused
///
/// Task 16's brief says to *"use the same `PolicyEnforcer` action the other
/// mutating routes in this gear use, and do not invent a new one"*. There are no
/// other routes in this gear, so there was nothing to match, and the convention
/// had to come from the siblings instead. The convention they share is: **the
/// PEP is asked for the action actually being performed, named after the
/// operation, in lowercase snake case** — qa-catalog's `sync` on
/// `qa.test_repo`, and qa-runs' `dispatch`, `cancel`, `rerun`, `force_start`
/// and `fire`. **None of those six is a CRUD verb** — the count said "five" over
/// a list of six until Task 16's fix round; the argument is unchanged and the
/// arithmetic was wrong. Each exists because the operation it names is one a
/// deployment must be able to grant, or withhold, on its own.
///
/// Two alternatives were considered and rejected:
///
/// * **`create` or `update`.** Both would ask the PDP about something the code
///   is not doing. qa-runs' `resources`/`actions` header states the rule and the
///   cost of breaking it: authorizing under the wrong verb "would take away a
///   deployment's ability to grant read-and-edit without grant-delete". Here it
///   is worse, because *nothing else* in this gear writes result rows under a
///   PEP decision at all — the event and reconcile paths use
///   `AccessScope::for_tenant` — so `create` would exist solely as a synonym for
///   `rebuild` with a less honest name.
/// * **Reusing qa-catalog's `sync`.** Semantically the closest sibling (an
///   operator-triggered re-derivation from an external source of truth), and
///   rejected because it is the wrong operation on the wrong resource: `sync`
///   means "fetch a git repository" there, and a policy author reading
///   `qa.test_result`/`sync` would have to guess.
/// # What a deployment must actually grant, written out
///
/// Recorded here because it is a deployment obligation with no home in code:
/// **nothing in this repository evaluates a policy**, so whether a production
/// PDP expresses these is unverifiable from inside the crate. qa-runs writes the
/// same list in the same place and for the same reason.
///
/// A subject that is to use `POST /qa/v1/insights/rebuild` needs:
///
/// * [`resources::TEST_RESULT`] / [`actions::REBUILD`] — and the compiled scope
///   must constrain **`owner_tenant_id` and nothing else**. That is not a
///   preference: a scope carrying any further predicate is refused by
///   [`refuse_scope_beyond_tenant`], for the measured reason recorded there and
///   on `ResultsRepository::upsert_run_results`. A policy that answers this
///   action with a `resource_id` constraint disables the endpoint rather than
///   narrowing it.
///
/// A subject that is to use `GET /qa/v1/test-results`,
/// `GET /qa/v1/test-case-results` or `GET /qa/v1/dashboard` needs
/// [`resources::TEST_RESULT`] / [`actions::LIST`] — one grant for all three, for
/// the reason [`actions::LIST`] gives — and, unlike the rebuild, **any** shape of
/// compiled scope works: a read applies the whole scope through
/// `.secure().scope_with(..)`, so a policy that narrows a subject to particular
/// rows narrows the page, or the aggregate, rather than disabling the endpoint.
/// [`refuse_scope_beyond_tenant`] is not called on those paths;
/// `domain::service::results`' and `domain::service::dashboard`'s headers state
/// the rule.
///
/// The dashboard additionally reads live run state from qa-runs under the
/// **caller's own** context, so a subject also needs whatever qa-runs requires of
/// `list_runs`. A caller qa-runs refuses gets a 403 from this gear rather than a
/// dashboard with no runs on it — `infra::clients::qa_runs`' header says why the
/// subject-level refusal is not folded into "nothing to show".
///
/// The background paths need **nothing**, and that is the other half of the
/// obligation. The reconcile sweep scopes with `AccessScope::for_tenant` and
/// never reaches the PDP, which is why it keeps working under the shipped
/// `static-authz-plugin` — see [`ingest`]'s header. A deployment that grants
/// nothing at all still ingests; it simply has no rebuild.
pub(crate) mod actions {
    /// Read a set of result rows, or an aggregate over one.
    ///
    /// Task 17's two `OData` collections over both result tables, and — from Task
    /// 18 — the dashboard. **Not a new verb**: `list` is what all three sibling
    /// gears authorize a collection read under
    /// (`qa-runs/src/domain/service/mod.rs:348`,
    /// `qa-catalog/src/domain/service/mod.rs:143`), and the reasoning Task 16 used
    /// to *choose* `rebuild` — ask the PDP for the action actually being
    /// performed — resolves to the existing name here rather than to a new one.
    ///
    /// One action for both collections; `domain::service::results`' header says
    /// why splitting them would be splitting a resource type instead.
    ///
    /// **Task 18 considered a separate `view_dashboard` and rejected it**, which
    /// is the more interesting half of this constant now. An aggregate is a
    /// reduction of exactly the rows the collections return, and a second action
    /// would let a policy give it a *different* row set — an aggregate over rows
    /// the subject may not list is an inference channel with no witness in this
    /// crate. Under one action the two are the same rows by construction.
    /// `domain::service::dashboard`'s header carries the full argument, including
    /// what a deployment gives up (summary-only access cannot be expressed) and
    /// what it would take to add it safely.
    ///
    /// **Task 33 authorizes `GET /qa/v1/jira/open-bugs` under this same
    /// constant, on [`super::resources::JIRA_BUG`] rather than
    /// [`super::resources::TEST_RESULT`].** The action is the same read verb;
    /// the resource is what changes per collection, exactly as
    /// [`super::resources::SAVED_VIEW`]'s own list already does one type over
    /// from this one.
    pub const LIST: &str = "list";

    /// Replay a closed time window from qa-runs over this gear's projection.
    ///
    /// An **operator** action, and it is separate precisely so a deployment can
    /// grant it separately: a rebuild re-reads every run in a window from
    /// qa-runs and rewrites the tenant's result rows, which is not something the
    /// grant that lets somebody read a dashboard should carry.
    pub const REBUILD: &str = "rebuild";

    /// Read one addressed resource, as opposed to a collection.
    ///
    /// Task 32's two settings reads. **Not a new verb**: `get` is what all three
    /// sibling gears authorize a single-resource read under
    /// (`qa-runs/src/domain/service/mod.rs:347`,
    /// `qa-catalog/src/domain/service/mod.rs:142`,
    /// `qa-environments/src/domain/service/mod.rs:86`), and Task 16's rule — ask
    /// the PDP for the action actually being performed — resolves to it rather
    /// than to [`LIST`]: a per-tenant settings singleton is one resource, and
    /// authorizing it under `list` would make the grant that lets somebody read
    /// a page of result rows also let them read the JIRA integration's URL and
    /// credential reference.
    pub const GET: &str = "get";

    /// Store a new saved view. Task 28, [`super::resources::SAVED_VIEW`]'s first
    /// write action — matching the CRUD verb `qa-runs`' `qa.schedule` already
    /// authorizes under (`qa-runs/src/domain/service/mod.rs:346`) rather than
    /// inventing a second vocabulary for what is, structurally, the same shape
    /// of resource. `qa-runs`' own `UPDATE`/`DELETE` for `qa.schedule` sit
    /// elsewhere in that module under the same names.
    ///
    /// **Task 33 reuses this for `POST /qa/v1/jira/bugs`, on
    /// [`super::resources::JIRA_BUG`].** The port of legacy's
    /// find-**or**-create can settle on `created: false`, which is a find and
    /// not a write at all — but the action authorizes the *request*, not its
    /// per-test outcome, and every outcome of this endpoint either inserts a
    /// row through [`crate::domain::repos::JiraRepository::upsert_bug`] or
    /// deliberately declines to (an already-registered test, or a config the
    /// tenant has not enabled) — there is no outcome that reads existing rows
    /// and stops, the way [`LIST`] does. That is the same "ask for the action
    /// actually being performed" resolution [`super::resources::SAVED_VIEW`]'s
    /// own create already made, applied to a second resource.
    pub const CREATE: &str = "create";

    /// Replace an existing saved view's scope, plan, name and query.
    pub const UPDATE: &str = "update";

    /// Delete a saved view.
    pub const DELETE: &str = "delete";

    /// Launch the collect job on demand — `POST /qa/v1/analytics/collect`,
    /// Task 30.
    ///
    /// # Why this needs its own action rather than reusing [`LIST`] or [`CREATE`]
    ///
    /// It is neither. `LIST` is "read a set of result rows, or an aggregate
    /// over one" and this writes nothing to `qa_test_results` at all — it asks
    /// qa-runs to launch workflows. `CREATE` is [`super::resources::SAVED_VIEW`]'s
    /// verb for a row this gear itself inserts; a collect trigger inserts no
    /// row either — [`super::resources::TEST_RESULT`]'s own third table,
    /// `qa_test_case_collect`, is written later, by the runner's callback,
    /// under no PEP decision at all (see [`super::collect`]'s header). So the
    /// only thing a grant on this action authorizes is "may this caller ask
    /// qa-runs to launch collect workflows for this tenant's repositories" —
    /// Task 16's own naming rule (ask the PDP for the action actually being
    /// performed) resolves to a new word, exactly as it did for [`REBUILD`].
    ///
    /// # The scope requirement is [`REBUILD`]'s, not [`LIST`]'s
    ///
    /// A collect trigger fans out over every repository the caller's universe
    /// admits and launches one qa-runs call per repository; there is no single
    /// row it addresses, so [`super::refuse_scope_beyond_tenant`] applies here
    /// for the identical reason it applies to a rebuild — a scope narrower
    /// than the tenant has no repository-level predicate this operation could
    /// honour, and a `resource_id` constraint would silently drop repositories
    /// the caller's grant does not obviously exclude rather than failing
    /// closed.
    pub const COLLECT: &str = "collect";

    /// Send a test or scheduled-run-test notification —
    /// `POST /qa/v1/settings/notifications/test`. Task 38.
    ///
    /// Not [`CREATE`]: nothing is stored (`domain::service::notify` never
    /// claims, logs or writes for a test send), and not [`GET`]/[`UPDATE`]:
    /// a test send has a real side effect on a system outside this gear
    /// (Slack, or an SMTP relay once one exists) that neither of those verbs
    /// would lead a policy author to expect. Task 16's own naming rule — ask
    /// the PDP for the action actually being performed — resolves to a new
    /// word, exactly as it did for [`REBUILD`] and [`COLLECT`].
    pub const TEST: &str = "test";
}

/// Refuse a compiled scope that constrains anything beyond the tenant.
///
/// # This is a guard over a measured fail-open, not defence in depth
///
/// `ResultsRepository::upsert_run_results` filters its two per-run `DELETE`s
/// with the **full** compiled scope (`.secure().scope_with(scope)`) and performs
/// its two bulk inserts through `.scope_unchecked(scope)`, which is a documented
/// no-op — it discards the scope and returns
/// (`libs/toolkit-db/src/secure/db_ops.rs:376-385`). The only check standing
/// between the two is `validate_tenant_in_scope`
/// (`db_ops.rs:281-298`), and it inspects **`owner_tenant_id` alone**.
///
/// So for any scope carrying a further predicate, delete and insert disagree
/// about which rows they are about: the delete matches a narrower set than the
/// insert writes, and a replay *adds* rows instead of replacing them. Both
/// result entities declare `resource_col = "id"`
/// (`infra/storage/entity/test_result.rs:25`,
/// `entity/test_case_result.rs:20`) and [`resources::TEST_RESULT`] declares
/// `RESOURCE_ID` as supported, so a PDP `resource_id` constraint **does** compile
/// and **does** reach the delete. The consequence is duplicated
/// `qa_test_results` / `qa_test_case_results` rows, silently double-counting in
/// every dashboard and analytics read Tasks 18-27 build on top.
///
/// Ruling R2's own justification — *"`upsert_run_results` already fails closed
/// via `validate_tenant_in_scope`"* — is true of the tenant and of nothing else.
/// R2 exists to let a *narrower* request-scoped scope through, and a narrower
/// scope is exactly the input that double-writes, so the guard belongs at every
/// request-scoped writer that shares `upsert_run_results`' shape: a bulk
/// insert or delete run through `.scope_unchecked(scope)` (or otherwise not
/// filtered by the *full* compiled scope on every statement it performs),
/// where a narrower-than-tenant scope can reach the table but not every
/// statement that writes to it applies the narrowing.
///
/// **Stated by shape, not by task number — corrected in the Phase B fix wave
/// (Finding 4).** This used to say "Tasks 28, 33 and 38 add three more such
/// writers, and each of them must call this before it writes." **Task 28
/// shipped and does not call this function, correctly**: saved views has no
/// bulk-insert asymmetry to guard against — every write goes through
/// `secure_insert`, `secure_update_with_scope` or `SecureDeleteExt`
/// (`infra::storage::saved_views_sea_repo`), each of which applies the full
/// compiled scope to the one row it touches, and
/// `saved_views::SavedViewsService::scope`'s
/// `.ensure_owner(ctx.subject_id())` is a stronger, structurally different
/// answer — a narrowing applied before any repository call, not a check
/// applied at write time to catch a narrowing a repository failed to honour.
/// Naming Task 28 here as an obligated future caller left a maintainer
/// auditing this guard with no way to tell "correctly does not call this"
/// from "forgot to call this" other than re-deriving the argument above.
/// Whether Tasks 33 and 38 need this guard is a question about *their* write
/// shape, to be answered when they are built, not a count fixed in advance.
///
/// **Task 33 answered it: no.** [`jira::JiraService::file_bugs`]'s only write is
/// [`JiraRepository::upsert_bug`], one row at a time through a single named
/// `ON CONFLICT` target — not a bulk statement, and not a delete paired with an
/// unequally-filtered insert. `upsert_bug` calls `validate_tenant_in_scope`
/// itself before writing, [`JiraRepository::get_config`]'s own answer to the
/// identical question, so there is no asymmetric pair of statements for a
/// narrower-than-tenant scope to make disagree.
///
/// # Why the guard is here and not in the repository
///
/// Two reasons, and the second is the decisive one. The repository cannot refuse
/// on its own behalf without changing what `upsert_run_results` means for the
/// two background callers that are already correct. And the domain is where the
/// decision is *expressible*: [`DomainError::UnsupportedScope`] carries the PEP
/// resource type, which is a `domain::service` constant and not something
/// `infra::storage` should be naming.
///
/// # Rejected alternatives
///
/// * **`scope.tenant_only()`**, which exists and would make delete and insert
///   agree. Rejected because it agrees in the *wrong direction*: it widens the
///   delete to the whole tenant, removing rows the PDP said this subject may not
///   touch. Silently exceeding a policy decision is worse than refusing to act
///   on it.
/// * **Dropping `RESOURCE_ID` from [`resources::TEST_RESULT`]** so such a
///   constraint could never compile. Rejected because all three sibling gears
///   declare both properties on every resource type, and breaking that
///   convention to route around a repository asymmetry hides the asymmetry
///   instead of naming it — leaving the next writer to rediscover it.
/// * **A doc note and no guard.** Leaves the fail-open live.
///
/// # The unconstrained and deny-all legs are unreachable today, and are covered
/// # anyway
///
/// `PolicyEnforcer::access_scope` always passes `require_constraints = true`, and
/// the compiler turns empty constraints into `ConstraintsRequiredButAbsent` and
/// total compilation failure into `AllConstraintsFailed`
/// (`authz-resolver-sdk/src/pep/compiler.rs:83-89`), both of which surface as
/// `EnforcerError::CompileFailed` and error out — never producing an
/// `AccessScope` — before reaching here: `ConstraintsRequiredButAbsent` as
/// [`DomainError::Forbidden`] (a documented deny, not a fault — the PEP asked
/// for row-level constraints and got none) and `AllConstraintsFailed` as
/// [`DomainError::Internal`] (a policy this PEP cannot compile; review finding
/// #3). **Verified, not assumed** — and both are still refused below,
/// because `validate_tenant_in_scope` returns `Ok` for an unconstrained scope
/// (`db_ops.rs:285-287`) while `scope_with` applies no filter to it, which would
/// make the per-run delete cross **every tenant**. One
/// `access_scope_with(require_constraints(false))` anywhere in a later task is
/// all it would take. **Both legs were traced to the SQL, not inferred:**
/// `build_scope_condition` returns `Condition::all()` — an empty `AND`, i.e.
/// always true — for an unconstrained scope (`libs/toolkit-db/src/secure/cond.rs:59-61`)
/// and for a constraint with no filters (`:93-94`), and `deny_all()` for a
/// deny-all scope (`:62-64`). The first two leave the delete unfiltered; the
/// third makes it match nothing while the insert still writes. All three are
/// refused below.
///
/// # Errors
///
/// [`DomainError::UnsupportedScope`], naming `resource`.
pub(crate) fn refuse_scope_beyond_tenant(
    scope: &AccessScope,
    resource: &'static str,
) -> Result<(), DomainError> {
    // An unconstrained scope filters nothing; a scope with no constraints is
    // deny-all. Neither can pair a per-run delete with a whole-tenant insert.
    if scope.is_unconstrained() || scope.constraints().is_empty() {
        warn!(
            resource,
            unconstrained = scope.is_unconstrained(),
            "the PDP compiled a scope with no tenant predicate; refusing rather than writing \
             through a delete that would not be tenant-filtered",
        );
        return Err(DomainError::UnsupportedScope { resource });
    }

    // A constraint with no filters is an unconditional access path, so it is as
    // unusable here as an unconstrained scope.
    let offending: Vec<&str> = scope
        .constraints()
        .iter()
        .flat_map(ScopeConstraint::filters)
        .map(ScopeFilter::property)
        .filter(|property| *property != pep_properties::OWNER_TENANT_ID)
        .collect();
    let has_empty_constraint = scope
        .constraints()
        .iter()
        .any(|constraint| constraint.filters().is_empty());

    if !offending.is_empty() || has_empty_constraint {
        // Logged, not returned: the property names are the actionable diagnostic
        // for whoever authors the policy, and the caller needs to know only that
        // the shape is unusable.
        warn!(
            resource,
            offending_properties = ?offending,
            has_empty_constraint,
            "the PDP compiled a scope constraining more than the tenant; this gear's projection \
             write cannot honour it (its delete is scope-filtered and its insert is not), so it \
             is refused rather than allowed to duplicate rows",
        );
        return Err(DomainError::UnsupportedScope { resource });
    }

    Ok(())
}

/// Everything [`AppServices::new`] needs beyond the repositories.
///
/// A `deps` struct rather than thirteen positional parameters, following
/// qa-catalog's `ServiceDeps`: the three shipped siblings split evenly on this,
/// and the deciding argument is that most of these fields are transposable with
/// another one of the same type. **Counted 2026-08-24, at Task 32: seven
/// `Arc<dyn …>` fields** — [`Self::authz`], [`Self::runs`], [`Self::catalog`],
/// [`Self::platforms`], [`Self::clock`], [`Self::runs_launcher`] and
/// [`Self::jira_client`] — **and three `String`s**, of which the last two were
/// so nearly transposed at Task 30 that `domain::service::collect` gave them
/// newtypes to stop it. (Four `Arc<dyn …>` until Task 25a added
/// [`Self::catalog`], six until Task 25b added [`Self::platforms`] and
/// [`Self::clock`], and the count said "five" through Task 30 while there were
/// six. The hazard this struct exists to remove keeps getting worse, which is
/// the argument holding rather than being outgrown — and the running count in
/// this paragraph keeps going stale, which is why it now carries the date it was
/// measured.)
pub(crate) struct ServiceDeps {
    pub(crate) db: Arc<DbProvider>,
    pub(crate) authz: Arc<dyn AuthZResolverApi>,
    /// The qa-runs reads, behind [`RunsReader`]. `infra::clients::QaRunsReader`
    /// in production; a fake in this layer's tests.
    pub(crate) runs: Arc<dyn RunsReader>,
    /// The qa-catalog universe, behind [`CatalogReader`].
    /// `infra::clients::QaCatalogReader` in production;
    /// [`test_support::FakeCatalog`] in this layer's tests.
    ///
    /// **Added by Task 25a**, which is what made a production implementation of
    /// that port exist at all — `domain::ports::catalog_reader`' header records
    /// why the adapter's original assignment to Task 40 could not have worked.
    /// Its one reader was [`dashboard::DashboardService`]'s
    /// `quality_vectors_pass_rate`; [`analytics::AnalyticsService`] is the
    /// second, added by Task 25b.
    pub(crate) catalog: Arc<dyn CatalogReader>,
    /// An environment's display name for the `environment_id` a row carries,
    /// resolved behind [`EnvironmentReader`].
    /// `infra::clients::QaEnvironmentsReader` in production;
    /// [`test_support::FakePlatforms`] in this layer's tests.
    ///
    /// **Added by Task 25b**, together with the `qa_environments` `deps` token
    /// and the gear-crate dependency that make the `ClientHub` lookup resolve —
    /// `gear.rs`' `deps` doc records that an unregistered client is a boot
    /// failure rather than a compile error, which is why the three land in one
    /// commit. Its only reader is [`analytics::AnalyticsService`].
    pub(crate) platforms: Arc<dyn EnvironmentReader>,
    /// Today, for the analytics windows. `infra::clock::SystemClock` in
    /// production; `test_support::FixedClock` in this layer's tests.
    ///
    /// A port rather than `OffsetDateTime::now_utc()` because the analytics
    /// output *is* a day axis — `domain::ports::clock`'s header contrasts that
    /// with the dashboard, whose tests work around the real clock instead.
    pub(crate) clock: Arc<dyn Clock>,
    /// The one launch this gear performs against qa-runs, behind
    /// [`RunsLauncher`]. `infra::clients::QaRunsReader` in production, on the
    /// same struct that implements [`RunsReader`]; a fake in
    /// `collect`'s own test module.
    ///
    /// **Added by Task 30**, [`collect::CollectService`]'s only cross-gear
    /// dependency beyond [`Self::catalog`].
    pub(crate) runs_launcher: Arc<dyn RunsLauncher>,
    /// The three outbound JIRA calls, behind [`JiraClient`].
    /// `infra::jira::OagwJiraClient` in production; a fake in
    /// [`jira`]'s own test module.
    ///
    /// **Added by Task 32.** The only field here whose adapter reaches a system
    /// outside this platform at all — `runs`, `catalog`, `platforms` and
    /// `runs_launcher` are sibling gears and `clock` is the process's own — which
    /// is why its adapter, not this struct, carries the egress argument.
    pub(crate) jira_client: Arc<dyn JiraClient>,
    /// The outbound Slack egress, behind [`SlackClient`]. Task 39's
    /// `infra::slack::SlackOagwClient` in production once it exists;
    /// `gear.rs`'s own `NeverWiredSlackClient` stand-in until then (R103) —
    /// see that type's doc for why the stand-in lives in `gear.rs` rather
    /// than in `domain::service::notify`.
    ///
    /// **Added by Task 38, fix round 1.** [`notify::NotifyService`]'s only
    /// reader.
    pub(crate) slack_client: Arc<dyn SlackClient>,
    /// The outbound email egress, behind [`MailClient`]. D10 defers the
    /// real send indefinitely, so `gear.rs`'s stand-in
    /// (`NeverWiredMailClient`) is not only a placeholder for a future
    /// adapter the way [`Self::slack_client`]'s is — see that module's own
    /// header.
    ///
    /// **Added by Task 38, fix round 1.**
    pub(crate) mail_client: Arc<dyn MailClient>,
    /// `QaInsightsConfig::reconcile_lookback_seconds`, as a duration.
    pub(crate) reconcile_lookback: Duration,
    /// `QaInsightsConfig::reconcile_page_size`.
    pub(crate) reconcile_page_size: u32,
    /// `QaInsightsConfig::default_collect_branch`, resolved here for the same
    /// reason [`Self::reconcile_lookback`] is: [`analytics::AnalyticsService`]
    /// holds it rather than re-reading the config per request.
    ///
    /// **Added by Task 29**, the field's first *behavioural* consumer —
    /// `gear.rs`'s `init` already emitted the raw config value in its `debug!`
    /// tracing line before this task, so it was not literally unread, only
    /// unused for anything the response reflects. It substitutes for a
    /// request whose `branch` is absent, exactly as legacy's
    /// `branch.unwrap_or(DEFAULT_COLLECT_BRANCH)` does (`analytics.rs:2682`).
    pub(crate) default_collect_branch: String,
    /// `QaInsightsConfig::collect_report_base_url`, resolved here for
    /// [`Self::default_collect_branch`]'s reason: [`collect::CollectService`]
    /// holds it rather than re-reading the config per cycle.
    ///
    /// **Added by Task 30.**
    pub(crate) collect_report_base_url: String,
    /// `QaInsightsConfig::collect_report_signing_secret` — the HMAC key that
    /// closes fix round 1's Critical 1 (a cross-tenant write on the collect
    /// report endpoint). See `domain::service::collect`'s header and that
    /// config field's own doc.
    ///
    /// **Added by Task 30's fix round 1.**
    pub(crate) collect_report_signing_secret: String,
    /// Where [`collect::CollectService`]'s telemetry goes.
    /// `infra::metrics::QaInsightsMetricsMeter` in production; a probe's
    /// adapter in the metric tests; `None` — which the service resolves to
    /// `ports::metrics::NoopMetrics` — everywhere else.
    ///
    /// **`Option`, and that is what "silent when no adapter is installed"
    /// means here.** It follows this struct's `runs`/`catalog` fields in kind
    /// but not in shape: those are `Arc<dyn _>` because a service with no
    /// qa-runs client cannot work, and this one is optional because a service
    /// with no metrics adapter must work *identically*.
    ///
    /// **Added by Task 38 of the observability plan.**
    pub(crate) collect_metrics: Option<Arc<dyn CollectMetrics>>,
    /// Where [`jira_poller::JiraPollerService`]'s telemetry goes. See
    /// [`Self::collect_metrics`]; `gear::init` builds one adapter and hands
    /// the same `Arc` to both.
    ///
    /// **Added by Task 38 of the observability plan.**
    pub(crate) jira_poll_metrics: Option<Arc<dyn JiraPollMetrics>>,
}

/// DI container: the services the transport layer and the background tasks
/// share.
///
/// # Generic over the repositories, not boxed
///
/// The same reason [`ingest::IngestService`] and
/// [`reconcile::ReconcileService`] are: [`ResultsRepository`]'s and
/// [`WatermarkRepository`]'s methods are generic over the `DBRunner` they run
/// on — which is what lets a caller supply a transaction — and a trait with a
/// generic method is not object-safe. So the parameters propagate up to here
/// and `gear.rs` names the one concrete instantiation. All three shipped
/// siblings put this struct in this module with these bounds
/// (`qa-environments/…/domain/service/mod.rs:103`, `qa-runs/…:574`,
/// `qa-catalog/…:158`); this follows that placement.
///
/// # Ten fields, and the field an eleventh used to be
///
/// [`Self::reconcile`] is the only service Task 16 had a reader for;
/// [`Self::results`] is Task 17's, [`Self::dashboard`] is Task 18's,
/// [`Self::analytics`] is Task 25b's, [`Self::saved_views`] is Task 28's,
/// [`Self::collect`] is Task 30's, [`Self::jira`] is Task 32's and
/// [`Self::notify`] is Task 38's.
///
/// **[`Self::jira_poller`] is Task 40's, and this section is the forecast
/// being discharged rather than a new decision.** It used to say that an
/// [`ingest::IngestService`] field "would be a field nothing reads — dead code
/// under `-D warnings`, discharged only by an `#[expect]` whose reason would be
/// 'Task 40 will need it'", that the instance existed but the field did not,
/// and that Task 40 — "which starts a transactional broker consumer and
/// therefore becomes its first reader — adds one". That consumer shipped, took
/// an `Arc` to the same `IngestService` instance from
/// `crate::gear::QaInsights::serve` rather than building a second one, and an
/// `ingest` field held it here for that reader to reach.
///
/// **The consumer was deleted once it was established that no deployment ever
/// registered the client it needed (`crate::gear`'s header), and the field
/// went with it rather than surviving with no reader.** [`Self::reconcile`]
/// holds the identical `Arc` — constructed once, in [`Self::new`], and passed
/// to `reconcile::ReconcileService::new` directly — which is the property that
/// made a shared instance necessary in the first place: two `IngestService`s
/// would be two projections over one table. That property does not need a
/// second field to hold it once there is only one reader to serve, so
/// `-D warnings` is what turned the deletion from optional cleanup into a
/// requirement, exactly as it turned the addition into one at Task 40.
/// [`jira_poller::JiraPollerService`] was the identical shape from Task 35 and
/// landed the same way, read by that task's `jira_poller_ticker` — and it
/// still has that reader, so its field stays.
///
/// [`Self::tenants`] is Task 40's too, but not an old forecast: nothing before
/// that task needed to know which tenants exist, because every other caller
/// arrives with one.
///
/// # A third repository parameter, and why it is not folded into `R`
///
/// [`SavedViewsRepository`] is its own trait over its own table, unrelated to
/// [`ResultsRepository`] — Task 28's `OrmSavedViewsRepository` is a second unit
/// struct beside `OrmResultsRepository`, not a second method on the same one.
/// `V` carries the same `Clone + 'static` bound as `R` and `W` for the same
/// reason [`Self::new`]'s doc gives, even though nothing here hands it to a
/// transaction closure yet: the bound is cheap on a unit struct and keeps this
/// struct's three repository parameters uniform rather than one of them being
/// the odd one out.
///
/// # A fourth, added by Task 29 for the same reason as the third
///
/// [`CollectRepository`] is `analytics::AnalyticsService`'s second repository —
/// [`Self::analytics`]'s doc says what it reads it for. `C` gets the same
/// `Clone + 'static` bound `V` does and for the same reason: `OrmCollectRepository`
/// is a third unit struct beside `OrmResultsRepository` and `OrmSavedViewsRepository`,
/// not a second method on either.
///
/// # And a fifth, added by Task 32 for the JIRA settings, then a use of `R`
/// # again for Task 33's bug-filing path
///
/// [`JiraRepository`] is [`jira::JiraService`]'s own repository, and Task 32
/// used it for its four *configuration* methods alone, leaving its five *bug*
/// methods uncalled. **Task 33 is the task that calls them**, and it needed a
/// second repository besides — [`ResultsRepository`], to resolve a run's plan
/// identity and per-case failure text before filing. So
/// [`jira::JiraService`] became generic over `R` too, reusing *this* struct's
/// own `R` rather than binding a second, differently-typed parameter of its
/// own: the concrete repository is the same `OrmResultsRepository` either way,
/// so a second name would only be a second word for one type.
/// `analytics::AnalyticsService<R, C>`'s two repository parameters are the
/// precedent for a service reading two repositories in one composed
/// operation, exactly as `collect_counts` reads `C` alongside `R` there. `J`
/// keeps its own `Clone + 'static` bound for the same reason as `V` and `C` —
/// `OrmJiraRepository` is a fourth unit struct beside `OrmResultsRepository`,
/// `OrmSavedViewsRepository` and `OrmCollectRepository`.
#[domain_model]
pub(crate) struct AppServices<R, W, V, C, J, N>
where
    R: ResultsRepository + Clone + 'static,
    W: WatermarkRepository + Clone + 'static,
    V: SavedViewsRepository + Clone + 'static,
    C: CollectRepository + Clone + 'static,
    J: JiraRepository + Clone + 'static,
    N: NotifyRepository + Clone + 'static,
{
    pub(crate) reconcile: Arc<reconcile::ReconcileService<R, W>>,
    /// The two `OData` collections' reads. `Arc` for the same reason
    /// [`Self::reconcile`] is one: the REST layer holds
    /// `Arc<ConcreteAppServices>` and a handler clones nothing.
    pub(crate) results: Arc<results::ResultsService<R>>,
    /// The dashboard aggregate, Task 18, and Task 19's coverage view — one
    /// service rather than a fourth field: one endpoint prefix, one authorization
    /// decision site, one place a reviewer looks for both.
    pub(crate) dashboard: Arc<dashboard::DashboardService<R>>,
    /// The analytics overview and its build-tests drill-down, Task 25b, plus
    /// Task 29's exact expected-case read over [`CollectRepository`]. One
    /// service for both, for [`Self::dashboard`]'s reason: they share a universe
    /// read, a row read, a group filter and an authorization decision, and the
    /// drill-down is a second reduction of the first's inputs.
    pub(crate) analytics: Arc<analytics::AnalyticsService<R, C>>,
    /// Saved-view CRUD, Task 28 — the first write path in Phase B and the
    /// first service in this struct generic over a repository other than
    /// [`ResultsRepository`]. See [`saved_views`]'s header for the owner-scoped
    /// authorization every one of its four operations applies.
    pub(crate) saved_views: Arc<saved_views::SavedViewsService<V>>,
    /// The collect trigger, the runner's report, and the (callable, not yet
    /// ticked) hourly cycle, Task 30. Generic over the same `C` as
    /// [`Self::analytics`]: both hold a [`CollectRepository`], and [`analytics`]
    /// reads the table this writes.
    pub(crate) collect: Arc<collect::CollectService<C>>,
    /// The JIRA settings surface and outbound status read (Task 32), plus the
    /// bug registry's list and filing path (Task 33). See [`jira`]'s header
    /// for the five defaults and clamps the settings half owns, and for why
    /// the poller (Task 35) is not here.
    pub(crate) jira: Arc<jira::JiraService<J, R>>,
    /// The notification settings surface, the audit log, and the send-once
    /// path (Task 38). Generic over `N` — a sixth repository parameter
    /// alongside `J`, [`crate::domain::service::jira::JiraService`]'s own
    /// reason: [`NotifyRepository`]'s methods are generic over the `DBRunner`
    /// they run on, so the trait is not object-safe.
    ///
    /// **Fix round 1, ruling R103.** A first draft named
    /// `crate::infra::storage::notify_sea_repo::OrmNotifyRepository` directly
    /// here instead of adding `N`, to avoid touching `gear.rs`'s
    /// `ConcreteAppServices` alias. That broke the rule the alias's own doc
    /// states — "the domain layer must not know which repository
    /// implementation a deployment uses" — for a mechanical, one-line fix
    /// `gear.rs` was always the right place for. Corrected: `N` joins `R`,
    /// `W`, `V`, `C` and `J` as a plain generic parameter, and `gear.rs`
    /// names `OrmNotifyRepository` as `ConcreteAppServices`'s sixth argument,
    /// exactly as it already names the other five.
    pub(crate) notify: Arc<notify::NotifyService<N>>,
    /// The JIRA poller's one pass, Task 35 — **a field since Task 40**.
    ///
    /// Holds [`Self::jira`]'s own `Arc`, not a second `JiraService`: the poller's
    /// resolve writes and the settings surface's reads must see one service, one
    /// enforcer and one upstream cache.
    pub(crate) jira_poller: Arc<jira_poller::JiraPollerService<J, R>>,
    /// Which tenants the three tickers work on, Task 40.
    ///
    /// The tenth field and the only service with no request-scoped caller at
    /// all — [`Self::notify`] is the eighth and [`Self::jira_poller`] the
    /// ninth: [`tenants`]'s header carries the whole design, including why the
    /// plan's own first candidate for this question could not work.
    pub(crate) tenants: Arc<tenants::TenantDirectory<R>>,
}

impl<R, W, V, C, J, N> AppServices<R, W, V, C, J, N>
where
    R: ResultsRepository + Clone + 'static,
    W: WatermarkRepository + Clone + 'static,
    V: SavedViewsRepository + Clone + 'static,
    C: CollectRepository + Clone + 'static,
    J: JiraRepository + Clone + 'static,
    N: NotifyRepository + Clone + 'static,
{
    /// Wire the services over `results`/`watermarks`/`views`/`collect`/`notify_repo`
    /// and `deps`.
    ///
    /// The repositories arrive by value rather than behind an `Arc`, unlike
    /// every sibling's constructor, and that follows from the bounds above
    /// rather than from taste: `Clone + 'static` is what
    /// [`reconcile::ReconcileService`]'s transaction closures require, both
    /// shipped repositories are unit structs, and an `Arc<OrmResultsRepository>`
    /// would be a heap allocation wrapping nothing.
    pub(crate) fn new(
        results: R,
        watermarks: W,
        views: V,
        collect: C,
        jira_repo: J,
        notify_repo: N,
        deps: ServiceDeps,
    ) -> Self {
        let enforcer = PolicyEnforcer::new(deps.authz);

        // One `IngestService`, held by the reconciler. A transactional broker
        // consumer once took an `Arc` to this same instance rather than a
        // second one being built for it; that consumer is gone, and this
        // struct's doc records why the instance stays a local rather than a
        // field with no reader.
        let ingest = Arc::new(ingest::IngestService::new(
            results.clone(),
            Arc::clone(&deps.runs),
        ));

        // Built as locals so the struct literal below can follow the field
        // declaration order (`clippy::inconsistent_struct_constructor`) while the
        // reads service still gets its clones before `results`, `enforcer` and
        // `deps.db` are moved into the reconciler.
        //
        // One `PolicyEnforcer` per service rather than one shared behind an
        // `Arc`: it is a thin handle over the `Arc<dyn AuthZResolverApi>`
        // (`PolicyEnforcer::new` takes it by value), so a clone is a refcount
        // bump and not a second client.
        let reads = Arc::new(results::ResultsService::new(
            Arc::clone(&deps.db),
            results.clone(),
            enforcer.clone(),
        ));
        let dashboard = Arc::new(dashboard::DashboardService::new(
            Arc::clone(&deps.db),
            results.clone(),
            Arc::clone(&deps.runs),
            Arc::clone(&deps.catalog),
            enforcer.clone(),
        ));
        let collect_service = Arc::new(collect::CollectService::new(
            Arc::clone(&deps.db),
            collect.clone(),
            Arc::clone(&deps.catalog),
            Arc::clone(&deps.runs_launcher),
            enforcer.clone(),
            // Fix round 2: these three were adjacent `String`s, and swapping
            // the base URL and the signing secret compiled silently -
            // publishing the secret into every callback URL and keying the
            // HMAC on the URL instead. Each is now its own type
            // (`collect::{DefaultCollectBranch, CollectReportBaseUrl,
            // CollectReportSigningSecret}`), so that transposition is a type
            // error here, not a silent one.
            collect::DefaultCollectBranch(deps.default_collect_branch.clone()),
            collect::CollectReportBaseUrl(deps.collect_report_base_url),
            collect::CollectReportSigningSecret(deps.collect_report_signing_secret),
            deps.collect_metrics,
        ));
        let analytics = Arc::new(analytics::AnalyticsService::new(
            Arc::clone(&deps.db),
            results.clone(),
            Arc::clone(&deps.catalog),
            // Cloned rather than moved since Task 40: the JIRA poller below
            // holds the same `EnvironmentReader` for its default-branch
            // resolution, and two adapters would be two upstream caches.
            Arc::clone(&deps.platforms),
            deps.clock,
            enforcer.clone(),
            collect,
            deps.default_collect_branch,
        ));
        let saved_views = Arc::new(saved_views::SavedViewsService::new(
            Arc::clone(&deps.db),
            views,
            enforcer.clone(),
        ));
        let jira_service = Arc::new(jira::JiraService::new(
            Arc::clone(&deps.db),
            jira_repo,
            results.clone(),
            deps.jira_client,
            enforcer.clone(),
        ));
        // Task 38's field, following every sibling repository's shape now
        // (fix round 1, R103): `notify_repo` arrives the same way
        // `jira_repo` does, and the two egress ports arrive from `deps`,
        // the same way `jira_client` does.
        let notify_service = Arc::new(notify::NotifyService::new(
            Arc::clone(&deps.db),
            notify_repo,
            enforcer.clone(),
            deps.slack_client,
            deps.mail_client,
            Arc::clone(&deps.runs),
        ));
        // Task 40's: the poller over the *same* `JiraService` the settings
        // surface uses, and the tenant directory over the same repository and
        // the same enforcer everything else here shares.
        let jira_poller = Arc::new(jira_poller::JiraPollerService::new(
            Arc::clone(&jira_service),
            Arc::clone(&deps.catalog),
            deps.platforms,
            Arc::clone(&deps.runs_launcher),
            deps.jira_poll_metrics,
        ));
        let tenants = Arc::new(tenants::TenantDirectory::new(
            Arc::clone(&deps.db),
            results.clone(),
            enforcer.clone(),
        ));
        let reconcile = Arc::new(reconcile::ReconcileService::new(
            deps.db,
            results,
            watermarks,
            deps.runs,
            ingest,
            enforcer,
            deps.reconcile_lookback,
            deps.reconcile_page_size,
        ));

        Self {
            reconcile,
            results: reads,
            dashboard,
            analytics,
            saved_views,
            collect: collect_service,
            jira: jira_service,
            notify: notify_service,
            jira_poller,
            tenants,
        }
    }
}
