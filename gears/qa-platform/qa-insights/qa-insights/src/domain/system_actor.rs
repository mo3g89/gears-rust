//! qa-insights-internal "system actor" `SecurityContext` factories.
//!
//! The reconciler, the operator rebuild's per-run re-read and Phase C's two
//! pollers have no end-user `SecurityContext` to forward, but they still make
//! cross-gear reads that the far side authorizes. These factories mint the
//! stable, audit-correlatable identity those flows use: every system call
//! carries `subject_id = QA_INSIGHTS_SYSTEM_ACTOR_UUID` and
//! `subject_type = "qa_insights.system"`.
//!
//! Structure copied from `qa-runs/src/domain/system_actor.rs`, which copied it
//! from `qa-catalog`'s, which mirrors the site-specific-factory idiom of
//! `gears/system/account-management/.../domain/system_actor.rs`: one named
//! factory per legitimate call site, each logging a `tracing` line under a
//! shared target so an audit sink can enumerate the sites.
//!
//! # One factory per site, and there are six sites
//!
//! qa-runs declares twelve (`grep -c '^pub fn for_'
//! qa-runs/src/domain/system_actor.rs`, measured 2026-08-25 — an earlier
//! revision of this line said six, and fix round 1's count sweep caught it).
//! This declares six, one per flow that reaches a sibling gear (qa-runs,
//! qa-catalog, qa-environments or JIRA-over-oagw) without a caller of its own:
//!
//! | Factory | `site` | The flow |
//! |---|---|---|
//! | [`for_reconcile_sweep`] | `reconcile_sweep` | `ReconcileService::sweep`'s listing and its per-run backfills |
//! | [`for_operator_rebuild`] | `operator_rebuild` | the per-run re-read inside `POST /qa/v1/insights/rebuild` |
//! | [`for_collect_report`] | `collect_report` | `POST /qa/v1/collect/{repo_id}` — the runner's callback, Task 30 |
//! | [`for_jira_poll`] | `jira_poll` | the JIRA poller ticker's pass over one tenant's open bugs, Task 40 |
//! | [`for_collect_cycle`] | `collect_cycle` | the collect ticker's hourly cycle for one tenant, Task 40 |
//! | [`for_ticker_enumeration`] | `ticker_enumeration` | the one cross-tenant read all three tickers start from, Task 40 |
//!
//! **This section said "four sites" through Task 39 and "seven sites" from
//! Task 40 until a seventh, `for_event_ingest`, was deleted.** That factory
//! minted the identity a transactional broker consumer used for its per-event
//! reads; no deployment ever registered the client that consumer needed, so it
//! was deleted along with it (`crate::gear`'s header, "Event ingest, and why
//! there is only one path"), and `for_event_ingest` went with it — its only
//! caller was gone. `SystemActorSite` accordingly dropped its `EventIngest`
//! arm; the two remaining arms below are what a reconcile sweep and an
//! operator rebuild each need, which is the whole reason the type has more
//! than one arm at all.
//!
//! **The names are the audit trail**, which is why there is no shared
//! `for_background()`: it would make six different flows indistinguishable in
//! the one log line that exists to tell them apart.
//!
//! The rebuild's asymmetry is deliberate and unchanged: its *listing* runs under
//! the operator's own `SecurityContext` (decision 4 on
//! [`crate::domain::service::reconcile`]'s header), and only the per-run re-read
//! is a system call — so `operator_rebuild` names a read the operator's own
//! listing already authorized.
//!
//! # `for_collect_report` is not like the other three, and the difference is the
//! # point
//!
//! The first three all mint an identity for a call **this gear** makes outward,
//! under a tenant it already knows from an authenticated or event-carried
//! source. [`for_collect_report`] mints an identity for a call **inbound**: the
//! test runner posts to `POST /qa/v1/collect/{repo_id}`, a route this gear
//! registers `.public()`. There is therefore no `SecurityContext` to forward
//! and no PDP grant to compile — the same two facts a route with no caller of
//! its own always has, restated here because the *reason* is different: a
//! ticker's read has no caller because it runs on a timer; this has no caller
//! because the transport is meant to be an unauthenticated HTTP POST from a
//! workload that is not a logged-in user.
//!
//! **`.public()` genuinely makes this route anonymously reachable — see
//! `domain::service::collect`'s header, "`.public()` genuinely exempts this
//! route", for the finding traced against how this gear is actually hosted,
//! not against `security_context_middleware` in isolation.** `qa_insights`
//! has no binary of its own; it is compiled into an in-process host
//! (`apps/cf-gears-example-server`) fronted by `api-gateway`, whose own auth
//! middleware inserts `SecurityContext::anonymous()` and runs the handler for
//! any route its `GatewayRoutePolicy` resolves to `AuthRequirement::None` —
//! exactly what a `.public()`-registered `OperationSpec` produces. An earlier
//! revision of this header concluded the opposite (`401` before
//! [`for_collect_report`] is ever called) from premises that were each true
//! in isolation but did not describe the host this gear actually runs on;
//! that conclusion is withdrawn.
//!
//! What makes a tenant-bound context safe to mint at all — on a route that
//! is, right now, reachable by anyone — is **not** merely that this gear chose the
//! `tenant_id` value when it built the callback URL —
//! [`crate::domain::service::collect::CollectService`] embeds the launching
//! caller's own tenant into that URL as a query parameter, but a bare
//! embedded value is exactly as forgeable by a third party as any other
//! caller-supplied field, which fix round 1 of this task's review found as a
//! cross-tenant write. What makes it safe is that
//! `CollectService::record_count` verifies an HMAC-SHA256 signature over
//! `(repo_id, branch, tenant_id)` **before** calling this factory at all — see
//! `domain::service::collect`'s header, "Fix round 1, Critical 1", for the
//! mechanism in full. `TenantBound::new` still refuses a nil value on top of
//! that, exactly as for the other three, because a signed-but-nil tenant must
//! not mint a platform-root actor either.
//!
//! Phase C's two pollers added their own factories when they arrived, in Task
//! 40: [`for_jira_poll`] and [`for_collect_cycle`].
//!
//! # Every context this module mints is tenant-bound **except one**
//!
//! Through Task 39 this section said there was no nil-tenant factory here and
//! that there should not be one *"until something genuinely enumerates across
//! tenants"*. **Task 40 is that something**, and the condition is met rather
//! than bypassed: a ticker holds no request and no event envelope, so it cannot
//! be handed a tenant and has to ask which ones exist. qa-runs reached the same
//! wall for the same reason — its dispatcher asks "which platforms, in any
//! tenant, have queued rows?" — and answered it with the same shape.
//!
//! So there is exactly one: [`for_ticker_enumeration`]. It reads one column of
//! one table and writes nothing, and every read and write past a tenant being
//! discovered is issued under one of the five tenant-bound factories, minted
//! from that discovered tenant. That split is not a convention this module
//! hopes for: the five take a [`TenantBound`], which cannot be constructed
//! from nil, so a ticker cannot accidentally act under the enumeration's
//! authority.
//!
//! [`TenantBound`] is the type that keeps that true. It lives in its own module
//! so the rest of this file cannot bypass its constructor — a tuple struct's
//! unmarked field is private to the declaring *module*, not to the type, so
//! `TenantBound(some_uuid)` would compile anywhere in this file if it were
//! declared here.

use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Hand-picked actor UUID (trailing bytes spell `qaisys`), stable across
/// processes so audit sinks can correlate qa-insights system invocations under
/// one identity. Cannot collide with any v4 actor UUID.
///
/// The `cf04` group continues the subsystem's series: `cf01` is
/// account-management, `cf02` is qa-catalog, `cf03` is qa-runs.
pub const QA_INSIGHTS_SYSTEM_ACTOR_UUID: Uuid = uuid::uuid!("00000000-0000-cf04-0000-716169737973");

/// `subject_type` stamped on every qa-insights system-actor context.
const QA_INSIGHTS_SYSTEM_SUBJECT_TYPE: &str = "qa_insights.system";

mod tenant_bound {
    use uuid::Uuid;

    /// A tenant id that has been checked non-nil.
    ///
    /// The nil UUID is the platform-root sentinel, and a tenant-bound context
    /// built from it is a cross-tenant context wearing a tenant-bound name.
    /// This type is the structural guard: the field is private to this module,
    /// so the only way to get one is [`Self::new`], and the only way past
    /// [`Self::new`] is a non-nil id.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TenantBound(Uuid);

    impl TenantBound {
        /// `None` for the nil UUID.
        #[must_use]
        pub const fn new(tenant_id: Uuid) -> Option<Self> {
            if tenant_id.is_nil() {
                None
            } else {
                Some(Self(tenant_id))
            }
        }

        #[must_use]
        pub const fn get(self) -> Uuid {
            self.0
        }
    }
}

pub use tenant_bound::TenantBound;

/// The one `SecurityContext::builder` call in this module.
///
/// Shape taken from `qa-runs/src/domain/system_actor.rs`'s `build_inner`, and
/// for its stated reason: a single construction point is what makes "a
/// qa-insights system context is never built from an unchecked tenant id" true
/// of the whole module rather than of each factory that happens to be written
/// correctly. There is no `Option` here where qa-runs has one — this gear has no
/// cross-tenant enumeration flow and so no nil-tenant factory (see above).
///
/// # Panics
///
/// Never in practice: both required builder fields are set unconditionally
/// below.
#[allow(
    clippy::expect_used,
    reason = "both builder fields are statically set; the expect anchors the impossible-failure invariant"
)]
fn build_inner(tenant: TenantBound) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(QA_INSIGHTS_SYSTEM_ACTOR_UUID)
        .subject_type(QA_INSIGHTS_SYSTEM_SUBJECT_TYPE)
        .subject_tenant_id(tenant.get())
        .build()
        .expect("QA_INSIGHTS_SYSTEM_ACTOR_UUID + tenant_id are always present")
}

/// Emit the audit line every factory shares.
///
/// `site` is `&'static str` and every caller is a factory in this file, which is
/// the point: nothing outside can invent a site name. A `pub fn` taking a
/// `&str` would be the shared `for_background()` this module rejects, wearing a
/// parameter instead of a name.
fn log_site(site: &'static str, tenant_id: Uuid) {
    tracing::info!(
        target: "qa_insights.system_actor",
        site,
        tenant_id = %tenant_id,
        "qa-insights system actor constructed",
    );
}

/// The reconcile sweep's context, for its qa-runs listing and every backfill it
/// performs.
///
/// Bound to the tenant the ticker was asked to sweep. Named rather than shared
/// with [`for_operator_rebuild`]'s call so a `site = "reconcile_sweep"` line in
/// the audit log tells the self-healing sweep apart from an operator's
/// deliberate replay — a `for_event_ingest` factory named a third, "the broker
/// delivered an event", until it was deleted along with the consumer that was
/// its only caller (this module's header).
#[must_use]
pub fn for_reconcile_sweep(tenant: TenantBound) -> SecurityContext {
    log_site("reconcile_sweep", tenant.get());
    build_inner(tenant)
}

/// The operator rebuild's per-run re-read context.
///
/// **This is not the context the rebuild lists runs with.** That listing runs
/// under the operator's own identity, which is what keeps the endpoint from
/// being a privilege escalation; this actor only re-reads a run that listing
/// already returned. Bound to the operator's own tenant, taken from their
/// `SecurityContext` and checked non-nil by [`TenantBound`] before the endpoint
/// does anything else.
#[must_use]
pub fn for_operator_rebuild(tenant: TenantBound) -> SecurityContext {
    log_site("operator_rebuild", tenant.get());
    build_inner(tenant)
}

/// `POST /qa/v1/collect/{repo_id}`'s context — the runner's per-file exact-count
/// report.
///
/// Bound to the tenant [`crate::domain::service::collect::CollectService`]
/// embedded in the callback URL it handed the runner, **not** to a tenant this
/// request asserts about itself: there is no session on this route
/// (`.public()`, matching legacy's own unauthenticated
/// `api_collect_report`). See this module's header, "`for_collect_report` is
/// not like the other three", for why that is a materially different
/// provenance from the first three factories' and why it is still safe to mint
/// a tenant-bound actor from it.
#[must_use]
pub fn for_collect_report(tenant: TenantBound) -> SecurityContext {
    log_site("collect_report", tenant.get());
    build_inner(tenant)
}

/// The JIRA poller ticker's per-tenant context, for its outbound JIRA status
/// reads, its local resolve writes and any auto-rerun it launches.
///
/// One of the "two pollers" this module's header reserved a factory for.
/// Separate from [`for_reconcile_sweep`] for that section's stated reason: a
/// `site = "jira_poll"` line is the difference between "this gear swept for
/// finished runs" and "this gear asked JIRA whether a bug had been resolved",
/// and the second one *files nothing but launches runs*.
///
/// Bound to a tenant taken from [`for_ticker_enumeration`]'s answer, never to
/// the enumeration context itself — see that function's doc.
#[must_use]
pub fn for_jira_poll(tenant: TenantBound) -> SecurityContext {
    log_site("jira_poll", tenant.get());
    build_inner(tenant)
}

/// The collect ticker's per-tenant context, for the universe read that
/// discovers repositories and the collect launches that follow.
///
/// The second of the two pollers. **Not** [`for_collect_report`]: that one is
/// the *inbound* callback's identity, minted from an HMAC-verified tenant on an
/// anonymous route, and this one is the *outbound* cycle's. They are two
/// directions of the same feature and sharing a factory would erase the
/// distinction the header's "`for_collect_report` is not like the other three"
/// section spends a paragraph on.
#[must_use]
pub fn for_collect_cycle(tenant: TenantBound) -> SecurityContext {
    log_site("collect_cycle", tenant.get());
    build_inner(tenant)
}

/// The tickers' tenant-enumeration context: **nil tenant, cross-tenant, reads
/// only**.
///
/// # This is the module's first non-tenant-bound factory, and the section it
/// # contradicts is corrected rather than deleted
///
/// The header used to say "There is no nil-tenant factory here and there should
/// not be one **until something genuinely enumerates across tenants**". Task 40
/// is that something, on exactly the condition that sentence set: a ticker has
/// no request and no envelope, so it cannot be handed a tenant — it has to ask
/// which tenants exist before it can bind itself to one. `qa-runs` reached the
/// same point one gear earlier and answered it the same way
/// (`qa-runs/src/domain/system_actor.rs`, `for_dispatch_enumeration` and
/// `for_schedule_tick`; `domain::service::schedules`' header, "The tenant travels
/// with the schedule, and never comes from anywhere else").
///
/// # What it may and may not be used for
///
/// **It reads one thing and writes nothing.**
/// [`TenantDirectory`](crate::domain::service::tenants::TenantDirectory) is its
/// only caller and `ResultsRepository::tenants_with_results` its only read.
/// Every read and write that follows a tenant being discovered is issued under
/// [`for_reconcile_sweep`], [`for_jira_poll`] or [`for_collect_cycle`], minted
/// from *that* tenant — which is what keeps the discovered tenant and the acting
/// tenant provably the same value, and is why those three take a
/// [`TenantBound`] rather than a `Uuid`. Using this context for anything past
/// the enumeration would hand a ticker a scope that spans every tenant, which is
/// the defect class this crate has found six times (R86) in its most damaging
/// possible location.
///
/// **This context is minted for its audit line and is never handed to the
/// PDP.** `TenantDirectory` calls this factory only so the `tracing::info!` in
/// [`log_site`] fires — the log line is the whole reason the call site still
/// reads as "feeding this read" — and then discards the returned
/// `SecurityContext`, reaching for
/// [`crate::domain::elevated::enumeration_scope`] instead of
/// `PolicyEnforcer::access_scope`. See that module's doc for why a nil-tenant
/// context bypassing the PEP for a *read* is not the security regression a
/// covering *write* grant would be, and why no deployment policy grant is
/// required for this one read to work.
///
/// Nil rather than a sentinel tenant because nil *is* the platform-root sentinel
/// `toolkit_security` already understands, and [`TenantBound::new`] refuses it —
/// so the type system stops this context being mistaken for a tenant-bound one.
/// It is built through [`SecurityContext::builder`] directly rather than through
/// [`build_inner`], because that function takes a [`TenantBound`] and there is
/// deliberately no way to make one from nil.
///
/// # Panics
///
/// Never in practice: both required builder fields are set unconditionally.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "both builder fields are statically set; the expect anchors the impossible-failure \
              invariant, exactly as build_inner's does"
)]
pub fn for_ticker_enumeration() -> SecurityContext {
    log_site("ticker_enumeration", Uuid::nil());
    SecurityContext::builder()
        .subject_id(QA_INSIGHTS_SYSTEM_ACTOR_UUID)
        .subject_type(QA_INSIGHTS_SYSTEM_SUBJECT_TYPE)
        .subject_tenant_id(Uuid::nil())
        .build()
        .expect("QA_INSIGHTS_SYSTEM_ACTOR_UUID + the nil tenant are always present")
}

/// Which call site a shared background function is being run on behalf of.
///
/// [`crate::domain::service::ingest::IngestService::read_run_projection`] is
/// one function with two callers — the reconcile sweep and the operator
/// rebuild, both reached through
/// [`ReconcileService::reproject`](crate::domain::service::reconcile::ReconcileService::reproject) —
/// and the audit identity it mints is not the same for both. Passing the
/// *site* rather than a built `SecurityContext` keeps the tenant on the
/// context and the tenant of the projection provably the same value — the
/// guarantee [`TenantBound`] exists for — while still putting the choice of
/// name at the call site. Each arm delegates to the named factory, so the log
/// line is the factory's and this type adds no third name.
///
/// **A third arm, `EventIngest`, existed from Task 5a until it was deleted.**
/// It named a transactional broker consumer that called
/// [`IngestService::read_run_projection`](crate::domain::service::ingest::IngestService::read_run_projection)
/// and [`IngestService::write_run_projection`](crate::domain::service::ingest::IngestService::write_run_projection)
/// under its own transaction — the shape the two calls above were split
/// *out of* in Task 5a's fix wave, when they were one function. No deployment ever
/// registered the client that consumer needed, so it was deleted along with
/// it, and `EventIngest` went with it — this module's header carries the
/// history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemActorSite {
    /// The reconcile sweep's backfill: [`for_reconcile_sweep`].
    ReconcileSweep,
    /// The operator rebuild's replay: [`for_operator_rebuild`].
    OperatorRebuild,
}

impl SystemActorSite {
    /// Mint this site's context for `tenant`.
    #[must_use]
    pub fn context(self, tenant: TenantBound) -> SecurityContext {
        match self {
            Self::ReconcileSweep => for_reconcile_sweep(tenant),
            Self::OperatorRebuild => for_operator_rebuild(tenant),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        QA_INSIGHTS_SYSTEM_ACTOR_UUID, SystemActorSite, TenantBound, for_collect_cycle,
        for_collect_report, for_jira_poll, for_ticker_enumeration,
    };
    use uuid::Uuid;

    /// The guard the whole module rests on. A nil tenant is the platform-root
    /// sentinel, and there is no call site in this gear entitled to it.
    #[test]
    fn a_nil_tenant_cannot_be_bound() {
        assert_eq!(TenantBound::new(Uuid::nil()), None);
    }

    /// The actor id is a stable literal, not a value derived at runtime: audit
    /// correlation across processes is the entire reason it is hand-picked.
    /// Pinned against an independent literal so a typo in the constant is
    /// visible rather than tautological.
    #[test]
    fn the_actor_id_is_the_subsystems_cf04_identity() {
        assert_eq!(
            QA_INSIGHTS_SYSTEM_ACTOR_UUID,
            Uuid::parse_str("00000000-0000-cf04-0000-716169737973").unwrap()
        );
    }

    /// The collect-report factory carries the tenant embedded in the callback
    /// URL, not a tenant the request itself asserts — there is nothing on a
    /// `.public()` route to assert one from. This is the one property a unit
    /// test can pin about that provenance: whatever [`TenantBound`] this
    /// factory is handed is exactly what lands on the context.
    #[test]
    fn the_collect_report_context_carries_the_embedded_tenant() {
        let tenant = Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap();
        let ctx = for_collect_report(TenantBound::new(tenant).expect("non-nil"));

        assert_eq!(ctx.subject_tenant_id(), tenant);
        assert_eq!(ctx.subject_id(), QA_INSIGHTS_SYSTEM_ACTOR_UUID);
    }

    /// Every arm of [`SystemActorSite`] must carry the tenant it was handed.
    ///
    /// The `site` string itself is only observable through a `tracing`
    /// subscriber, so what a unit test can pin is the property whose failure
    /// would be silent and severe: an arm that dropped or substituted the tenant
    /// would read another tenant's run and write it under this one. The two
    /// arms are enumerated explicitly rather than looped, so a third site added
    /// without a thought about its tenant leaves this test unmentioning it.
    #[test]
    fn every_site_binds_the_tenant_it_was_given() {
        let tenant = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
        let bound = TenantBound::new(tenant).expect("non-nil");

        for site in [
            SystemActorSite::ReconcileSweep,
            SystemActorSite::OperatorRebuild,
        ] {
            let ctx = site.context(bound);
            assert_eq!(ctx.subject_tenant_id(), tenant, "{site:?}");
            assert_eq!(ctx.subject_id(), QA_INSIGHTS_SYSTEM_ACTOR_UUID, "{site:?}");
        }
    }

    /// Task 40's two per-tenant ticker factories carry the tenant they were
    /// handed, for the same reason the two [`SystemActorSite`] arms do: the
    /// tenant on the context is what every downstream `AccessScope` is derived
    /// from, so an arm that substituted one would read another tenant's bugs or
    /// launch another tenant's collect runs.
    ///
    /// Enumerated as a list of `(name, factory)` pairs rather than looped over an
    /// enum because these two are free functions — there is no
    /// [`SystemActorSite`] arm for either, since neither is a *shared* function
    /// with several call sites the way `read_run_projection` is.
    #[test]
    fn the_ticker_factories_bind_the_tenant_they_were_given() {
        let tenant = Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap();
        let bound = TenantBound::new(tenant).expect("non-nil");

        for (name, ctx) in [
            ("jira_poll", for_jira_poll(bound)),
            ("collect_cycle", for_collect_cycle(bound)),
        ] {
            assert_eq!(ctx.subject_tenant_id(), tenant, "{name}");
            assert_eq!(ctx.subject_id(), QA_INSIGHTS_SYSTEM_ACTOR_UUID, "{name}");
        }
    }

    /// **The one factory in this module that is deliberately not tenant-bound.**
    ///
    /// Two properties, and the second is the one that matters: the context is nil
    /// (so a scope compiled from it can span tenants, which is what makes the
    /// enumeration expressible at all) *and* that nil id cannot be turned back
    /// into a [`TenantBound`]. The second half is why a ticker cannot carry this
    /// authority into its per-tenant work — `for_jira_poll`,
    /// `for_collect_cycle` and `for_reconcile_sweep` all take a `TenantBound`,
    /// and `TenantBound::new(nil)` is `None`.
    ///
    /// The mutation this catches is the one that would matter: giving this
    /// factory a real tenant (which would make the enumeration answer one
    /// tenant's question) or giving `TenantBound` a nil-permitting constructor.
    #[test]
    fn the_enumeration_context_is_nil_and_cannot_become_tenant_bound() {
        let ctx = for_ticker_enumeration();

        assert!(ctx.subject_tenant_id().is_nil());
        assert_eq!(ctx.subject_id(), QA_INSIGHTS_SYSTEM_ACTOR_UUID);
        assert_eq!(
            TenantBound::new(ctx.subject_tenant_id()),
            None,
            "the enumeration's authority must not be convertible into a tenant-bound one"
        );
    }
}
