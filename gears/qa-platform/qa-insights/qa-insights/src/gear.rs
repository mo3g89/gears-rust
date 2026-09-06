//! Composition root: `#[toolkit::gear]` bootstrap, `Gear::init`, the
//! `DatabaseCapability`/`RestApiCapability` implementations, and the stateful
//! lifecycle entry ([`QaInsights::serve`]) hosting the three leader-elected
//! tickers.
//!
//! # What this file does
//!
//! At Task 9 `init` wired three things — the database handle, the `AuthZ`
//! enforcer and the typed config — and stored them, resolving no other client,
//! starting no task and registering no route. Every gap carried a
//! `// wired in Task N` comment naming the task that closes it, rather than an
//! unexplained `OnceLock`; Task 40's step 3 is "fill every `// wired in Task N`
//! gap left in Task 9", so those comments were the interface between the tasks
//! and not decoration.
//!
//! **Task 40 closed the last of them.** For the record of which task closed
//! what: Task 16 took `QaRunsClientV1` and the first route, Task 25a the
//! qa-catalog client, Task 25b the qa-environments client, Task 32 the oagw
//! client, and Task 40 the rest — the `QaInsightsClientV1` registration, the
//! elector, the three tickers, and the two real egress adapters that replaced
//! Task 38's inert stand-ins. Task 40 also wired an `EventBrokerApi` lookup and
//! the transactional consumer it fed; both were deleted once it was established
//! that no deployment ever registered that client and the reconcile sweep was
//! already the only ingest path that ran — see "Event ingest, and why there is
//! only one path" below.
//!
//! # The lifecycle, and what `serve` is responsible for
//!
//! The `stateful` capability and the `lifecycle(entry = "serve", ...)` clause
//! were **absent on purpose** until Task 40. This header used to say why: *"a
//! `serve` that idles is a lifecycle the runtime must supervise for no benefit,
//! and qa-runs' own `serve` documents how much care an 'idling until cancelled'
//! branch needs. Task 40 adds the capability together with the tickers it exists
//! for."* That is what happened.
//!
//! `serve` hosts the **three** leader-elected tickers — the **reconciler**, the
//! **JIRA poller** and the **collect** cycle — each under its own role
//! (`infra::leader`'s `ROLE_RECONCILER`, `ROLE_JIRA_POLLER`, `ROLE_COLLECT`) and
//! each independently switchable by its own interval, where `0` disables.
//!
//! # Event ingest, and why there is only one path
//!
//! **The event-broker dependency was removed, not reduced.** Until this change
//! `serve` also started a transactional broker consumer whenever `init` resolved
//! an `EventBrokerApi` — but the `event-broker` gear is a skeleton whose `init`
//! resolves a deployment mode and registers no client
//! (`gears/system/event-broker/event-broker/src/module.rs`, "No routes are
//! registered yet - handler bodies land with #4346"), and the only
//! `impl EventBrokerApi` in the tree was `event-broker-sdk`'s test `MockBroker`.
//! No deployment ever registered that client, qa-runs deleted the publisher that
//! would have fed it, and the reconcile sweep was already carrying every
//! deployment's event ingest — so the consumer, the `EventBrokerApi` lookup and
//! the `event-broker`/`event-broker-sdk` dependencies were code that could not
//! execute, and deleting them is not a reduction in capability.
//!
//! **The reconcile sweep is therefore the only ingest path there has ever
//! actually been**, and it stays exactly as it was: leader-elected, on its own
//! `reconcile_interval_seconds` cadence, reading qa-runs through
//! `QaRunsReader`. Stated here rather than left for an operator to discover:
//!
//! * a finished run reaches the dashboard within one
//!   `reconcile_interval_seconds` — 300 by default, and
//!   `config/qa-platform.yaml` overrides no qa-insights knob — not within
//!   seconds.
//! * the sweep needs a tenant to sweep, and it enumerates tenants from
//!   `qa_test_results` (`domain::service::tenants`, "the tenants this gear
//!   already holds results for" — chosen over the watermark scan precisely
//!   because it is not circular). On a **fresh** deployment with no rows yet, a
//!   tenant is invisible to all three tickers until somebody calls
//!   `POST /qa/v1/insights/rebuild`, which takes the operator's own context and
//!   needs no enumeration. That replay writes result rows, and from then on the
//!   sweep finds the tenant by itself.
//! * with `enable_tickers: false` there is no ingest path at all. `serve`'s
//!   "nothing to supervise" branch says so at `WARN`.
use std::future::Future;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use tracing::{debug, error, info, warn};

use authz_resolver_sdk::AuthZResolverClient;
use oagw_sdk::api::ServiceGatewayClientV1;
use qa_catalog_sdk::QaCatalogClientV1;
use qa_environments_sdk::QaEnvironmentsClientV1;
use qa_insights_sdk::QaInsightsClientV1;
use qa_runs_sdk::QaRunsClientV1;

use crate::api::rest::routes;
use crate::config::QaInsightsConfig;
use crate::domain::error::DomainError;
use crate::domain::local_client::QaInsightsLocalClient;
use crate::domain::ports::{MailClient, RunsLauncher, RunsReader, SlackClient};
use crate::domain::service::reconcile::ReconcileOutcome;
use crate::domain::service::{AppServices, ServiceDeps};
use crate::domain::system_actor::{self, TenantBound};
use crate::infra::clients::{QaCatalogReader, QaEnvironmentsReader, QaRunsReader};
use crate::infra::clock::SystemClock;
use crate::infra::jira::OagwJiraClient;
use crate::infra::leader::{
    LeaderElector, ROLE_COLLECT, ROLE_JIRA_POLLER, ROLE_RECONCILER, elector, work_fn,
};
use crate::infra::notify::{SlackOagwClient, UnsupportedMailClient};
use crate::infra::storage::collect_sea_repo::OrmCollectRepository;
use crate::infra::storage::jira_sea_repo::OrmJiraRepository;
use crate::infra::storage::notify_sea_repo::OrmNotifyRepository;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::saved_views_sea_repo::OrmSavedViewsRepository;
use crate::infra::storage::watermark_sea_repo::OrmWatermarkRepository;

/// Everything `init` resolves, in one value behind one `OnceLock`.
///
/// One struct rather than a field per dependency, following qa-runs'
/// `QaRunsRuntime`: the alternative is several `OnceLock`s that can be set
/// independently, and a gear that is half-initialised is a state nothing else
/// in this crate is written to expect.
///
/// # The per-field `#[expect(dead_code)]` attributes are gone, and that is the
/// # mechanism working
///
/// Task 9 put one above each unread field rather than a single `#[allow]` on the
/// struct, on the grounds that *"an `expect` that stops being needed is itself a
/// warning, and `-D warnings` turns that into a build failure — so the task that
/// first reads a field is **forced** to delete the attribute above it, and the
/// remaining attributes stay an accurate list of what is still unwired."*
///
/// Task 40 is that task for both of the two that were left, and it deleted both
/// because the compiler made it:
///
/// * `db` is **gone**, the same as `cfg` below, not un-`expect`ed. Its
///   `#[expect]` gave the same reason as `cfg`'s: a would-be reader whose
///   lifecycle entry receives no `GearCtx` and can only reach what `init`
///   stored. That reader was the transactional broker consumer's own
///   `db.conn()`, bypassing the services layer for a reason that consumer
///   alone needed; once it was deleted, nothing was left to read `db` as a
///   field. `QaInsightsRuntime` never keeps its own copy — the constructor
///   consumes it building `services` — and the three tickers' services still
///   need the database, reached through `services`, not through a second
///   handle kept here.
/// * `cfg` is **gone**, not un-`expect`ed, and that is the advice its own doc
///   asked Task 40 to take: *"qa-runs deliberately does **not** keep its config
///   on the runtime, resolving each knob to its final value at init and storing
///   only that, so the serve loop can never re-read a clamping accessor and
///   re-emit its warning every tick ... Keeping the whole struct here is right
///   while nothing consumes it and wrong once a ticker does."* Three tickers now
///   consume it, so the three [`Cadence`] values below are what `init` stores
///   and `serve` cannot re-read a knob at all — it never sees a
///   [`QaInsightsConfig`].
/// * `authz` went the same way at Task 16, consumed once into the
///   `PolicyEnforcer` every service shares.
struct QaInsightsRuntime {
    /// The services the REST layer and the three tickers share.
    ///
    /// Built from the transaction provider [`DBProvider`] wraps, re-parameterised
    /// with [`DomainError`] so transaction closures preserve domain variants
    /// rather than collapsing to `DbError` — the shape `qa-runs`'
    /// `domain::service::DbProvider` needs and the reason [`DomainError`]
    /// carries a `From<toolkit_db::DbError>`. Handed to the services and not
    /// kept here separately: a transactional broker consumer once needed its
    /// own `Arc<DBProvider<DomainError>>` reachable from `serve`, since a
    /// gear's lifecycle entry takes `(self: Arc<Self>, cancel:
    /// CancellationToken)` and **no `GearCtx`**; that consumer is gone
    /// (this file's header), and the three tickers reach the database only
    /// through the services they already hold.
    services: Arc<ConcreteAppServices>,
    /// Leader election for the three ticker roles. `infra::leader`'s header is
    /// explicit that this buys an *optimisation* here and not mutual exclusion.
    elector: Arc<dyn LeaderElector>,
    /// The reconcile sweep's resolved cadence.
    reconciler: Cadence,
    /// The JIRA poller's resolved cadence.
    jira_poller: Cadence,
    /// The collect cycle's resolved cadence — the one of the three with a
    /// legacy floor applied (`config::MIN_COLLECT_INTERVAL_SECONDS`).
    collect: Cadence,
    /// `default_collect_branch`, resolved here because
    /// `CollectService::run_collect_cycle` takes the branch as a parameter and
    /// the ticker has no request to take one from.
    ///
    /// The *same* value the analytics and collect services already hold, so a
    /// tenant's hourly cycle collects the branch its expected-case lookups read.
    /// Legacy's poller does the same thing with the same constant
    /// (`manager/src/services/collect.rs:19`, `DEFAULT_COLLECT_BRANCH`).
    default_collect_branch: String,
}

/// One ticker's resolved cadence: whether it runs on this replica, and how
/// often.
///
/// # Resolved at `init`, never re-read
///
/// Shape and reasoning taken from qa-runs' own `Cadence`. Both fields are
/// computed once, in `init`, from a [`QaInsightsConfig`] that is then dropped —
/// so a clamping accessor cannot re-emit its warning per tick, and `serve`
/// cannot disagree with the value `init` logged. The `runs` flag folds the two
/// switches into one answer so no branch of `serve` can consult one and forget
/// the other.
#[derive(Clone, Copy, Debug)]
struct Cadence {
    /// `enable_tickers` **and** a non-zero interval.
    runs: bool,
    /// The resolved period. Meaningful even when `runs` is false, because the
    /// "disabled" log lines report it — an operator who set `0` and one who set
    /// `enable_tickers: false` need to be told which of the two they did.
    interval_seconds: u64,
}

impl Cadence {
    /// The reconcile sweep's, from `reconcile_interval_seconds`. No floor: the
    /// sweep has no legacy counterpart to port one from — see
    /// `config`'s header, "There is deliberately no accessor for the other two
    /// intervals".
    fn reconciler(cfg: &QaInsightsConfig) -> Self {
        Self::new(cfg.enable_tickers, cfg.reconcile_interval_seconds)
    }

    /// The JIRA poller's, from `jira_poller_interval_seconds`. No floor here
    /// either, and legacy's `.max(1)` is not one: it applies to the *per-tenant*
    /// column and lives on `JiraService::poller_config`, where legacy applies it.
    fn jira_poller(cfg: &QaInsightsConfig) -> Self {
        Self::new(cfg.enable_tickers, cfg.jira_poller_interval_seconds)
    }

    /// The collect cycle's, from `effective_collect_interval_seconds` — the one
    /// accessor in `config`, carrying legacy's 300-second floor. Called here and
    /// nowhere else, exactly once per process, which is what its own doc
    /// requires.
    fn collect(cfg: &QaInsightsConfig) -> Self {
        Self::new(cfg.enable_tickers, cfg.effective_collect_interval_seconds())
    }

    /// `runs` is the conjunction, so a caller cannot check one switch and miss
    /// the other.
    const fn new(enabled: bool, interval_seconds: u64) -> Self {
        Self {
            runs: enabled && interval_seconds > 0,
            interval_seconds,
        }
    }
}

/// Concrete instantiation of the generic
/// [`AppServices<R, W, V, C, J, N>`](crate::domain::service::AppServices), wired to the
/// `SeaORM`-backed repositories.
///
/// Named here rather than in `domain::service` for the reason every sibling
/// names it in its own composition root: the domain layer must not know which
/// repository implementation a deployment uses, and this is the file whose whole
/// job is to decide. The REST handlers and routes name this alias in the
/// `Extension<Arc<ConcreteAppServices>>` they extract.
///
/// The parameters are still generic all the way up from the repositories because
/// [`ResultsRepository`](crate::domain::repos::ResultsRepository)'s,
/// [`SavedViewsRepository`](crate::domain::repos::SavedViewsRepository)'s,
/// [`CollectRepository`](crate::domain::repos::CollectRepository)'s,
/// [`JiraRepository`](crate::domain::repos::JiraRepository)'s and
/// [`NotifyRepository`](crate::domain::repos::NotifyRepository)'s methods are
/// generic over their `DBRunner` and so none of the five traits is
/// object-safe — see `AppServices`' own doc. `OrmSavedViewsRepository` is
/// Task 28's third repository parameter, added beside the two Task 9 shipped;
/// `OrmCollectRepository` is Task 29's fourth, for the exact expected-case
/// counts [`analytics::AnalyticsService`](crate::domain::service::analytics::AnalyticsService)
/// now reads alongside the universe's static ones; `OrmJiraRepository` is Task
/// 32's fifth, for the two JIRA configuration singletons.
/// `OrmNotifyRepository` is Task 38's sixth, for the notification
/// settings/log/dedupe surface — added in fix round 1 (ruling R103), which
/// moved it here from a first draft that named the concrete type inside
/// `domain::service` itself rather than touch this alias.
pub(crate) type ConcreteAppServices = AppServices<
    OrmResultsRepository,
    OrmWatermarkRepository,
    OrmSavedViewsRepository,
    OrmCollectRepository,
    OrmJiraRepository,
    OrmNotifyRepository,
>;

/// Main gear struct.
///
/// # The `deps` list is initialisation order, not just linkage
///
/// Each token expands to a hidden `pub use ::<crate>`, which both keeps the
/// dependency's `inventory::submit!` registration alive through the linker and
/// places that gear earlier in the registry's topological order
/// (`toolkit/src/registry.rs`, `build_dependency_graph`). The second effect is
/// the load-bearing one: `ClientHub::get` at init time finds only what another
/// gear has *already* registered.
///
/// All six were declared at the skeleton, when `init` resolved only
/// `authz_resolver`. The alternative — grow the list as each client appears — is
/// what qa-runs did, and it cost a boot failure that had to be diagnosed from
/// `client not found: type=dyn qa_catalog_sdk::client::QaCatalogClientV1`
/// (`qa-runs/src/gear.rs`, the measured note on its own `deps`).
///
/// **A seventh token, `event_broker`, lived here from Task 40 until it was
/// deleted.** It resolved an `EventBrokerApi` for a transactional consumer that
/// projected qa-runs' lifecycle events. No deployment ever registered that
/// client — the `event-broker` gear is a skeleton that registers none — and
/// qa-runs deleted the publisher that would have fed it, so the token, the
/// lookup and the consumer were code that could not execute. Deleting them
/// leaves the six below, all resolved in [`QaInsights::init`].
///
/// * `qa_runs` — `QaRunsClientV1`, for the run metadata ingest denormalises and
///   for the auto-rerun admission. **Live since Task 16**, which needed it for
///   the rebuild endpoint's listing (Task 35 is the auto-rerun consumer).
/// * `qa_catalog` — `QaCatalogClientV1`, for the analytics universe and the
///   static expected-case counts. **Live since Task 25a.** This bullet said the
///   lookup was Task 40's, on the grounds that Task 40 is the only later task
///   whose file list includes this file — and it closed by saying that whichever
///   task first needed a live catalog read "must either pull this lookup forward
///   — as Task 16 did for `QaRunsClientV1` — or work against a fake". Task 25a is
///   that task and it pulled it forward. **Task 29 shipped that consumer**:
///   `domain::analytics::universe::expected_cases` reads
///   `UniverseTest::static_case_count`, called from
///   `AnalyticsService::overview`. This client needed no further wiring to
///   support it — the new wiring Task 29 added below is a fourth
///   `AppServices` repository parameter, `OrmCollectRepository`, for the
///   collect job's exact count, not a second qa-catalog lookup.
/// * `qa_environments` — `QaEnvironmentsClientV1`, for the platform display
///   names the analytics group chart draws. **Live since Task 25b**, which is
///   the first task with a caller; the port's own header records why a `Uuid`
///   where legacy had a name made this port necessary at all. **Nothing in the
///   test suite covers the gap that would open if this token were dropped** — an
///   unregistered name is a `RegistryError::UnknownDependency` at boot rather
///   than a compile error, and `QaEnvironmentsReader` is constructed under
///   `#[cfg(test)]` in its own module, so the whole suite would stay green with
///   the token missing.
/// * `oagw` — `ServiceGatewayClientV1`, the outbound path for JIRA and Slack.
///   **Live since Task 32**, which resolves it for
///   [`crate::infra::jira::OagwJiraClient`]; Task 39's Slack adapter reads the
///   same client from the same lookup. This bullet forecast "Tasks 34, 38",
///   which were the task numbers the plan then gave those two adapters.
/// * `cluster` — leader election for the three tickers (Task 40).
///
/// **Each token is verified against the registered gear name.** The macro
/// derives the runtime name by replacing underscores with hyphens
/// (`toolkit-macros/src/lib.rs`, `deps_lits`), and an unregistered name is a
/// `RegistryError::UnknownDependency` at boot, not a compile error — so each was
/// checked against the `name = "..."` that gear declares:
/// `authz-resolver` (`authz-resolver/src/gear.rs:26`), `qa-runs`
/// (`qa-runs/src/gear.rs:224`), `qa-catalog` (`qa-catalog/src/gear.rs:95`),
/// `qa-environments` (`qa-environments/src/gear.rs:32`), `oagw`
/// (`oagw/src/gear.rs:42`), `cluster` (`cluster/src/gear.rs:24`). All six match;
/// none needed changing.
///
/// **The cost is real, and it is the same shape for every token here.** Each of
/// the six makes its gear mandatory in any deployment that links this one — the
/// re-export is what guarantees `UnknownDependency` cannot fire, and it
/// guarantees it by pulling the gear in. `event_broker` was the one exception to
/// that rule while it lasted: its gear was mandatory to link but its *client*
/// was not mandatory to register (`init` tolerated a missing `EventBrokerApi`
/// with a `WARN`), which is exactly the gap that let it sit unregistered in
/// every real deployment without failing a single boot. The six tokens above
/// have no such gap — a deployment that links this gear links, and needs, all
/// six.
#[toolkit::gear(
    name = "qa-insights",
    deps = [authz_resolver, qa_runs, qa_catalog, qa_environments, oagw, cluster],
    capabilities = [db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s")
)]
pub struct QaInsights {
    runtime: OnceLock<Arc<QaInsightsRuntime>>,
}

impl Default for QaInsights {
    fn default() -> Self {
        Self {
            runtime: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for QaInsights {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: QaInsightsConfig = ctx.config_or_default()?;
        debug!(
            reconcile_interval_seconds = cfg.reconcile_interval_seconds,
            reconcile_lookback_seconds = cfg.reconcile_lookback_seconds,
            reconcile_page_size = cfg.reconcile_page_size,
            default_collect_branch = %cfg.default_collect_branch,
            collect_report_base_url = %cfg.collect_report_base_url,
            collect_report_signing_secret_configured =
                crate::domain::service::collect::signing_secret_is_configured(
                    &cfg.collect_report_signing_secret
                ),
            collect_interval_seconds = cfg.collect_interval_seconds,
            enable_tickers = cfg.enable_tickers,
            max_page_size = cfg.max_page_size,
            "loaded qa-insights config"
        );
        // Fix round 1, Important 3: an empty value for either knob does not
        // fail on its own — `collect_url` still builds a syntactically valid
        // (if wrong) URL, and `CollectService::verify_signature` fails closed
        // per-request rather than at boot. Neither of those is loud enough for
        // an operator to notice without reading logs closely, so this is the
        // one loud signal: a WARN at `init`, once, matching
        // `reconcile_lookback`'s own clamp-and-warn precedent below for why the
        // check belongs here and not on a per-call accessor.
        if cfg.collect_report_base_url.is_empty() {
            warn!(
                "collect_report_base_url is empty; the collect job's callback URL will have no \
                 scheme or host, so the runner cannot report exact case counts back. Every \
                 collect cycle will still report success (a qa-runs launch, once accepted, is \
                 not aware its callback URL is malformed) - this WARN is the only place that \
                 surfaces the misconfiguration.",
            );
        }
        // Phase B fix wave, Finding 1: this used to test `is_empty()`, which
        // agreed with `CollectService::verify_signature`'s own check before
        // fix round 2 raised that check's floor to
        // `collect::MIN_SIGNING_SECRET_LEN` (16 characters, trimmed) without
        // carrying the change back here. A 1-15 character secret then passed
        // this check silently and failed every collect report at request
        // time instead — the exact silent misconfiguration this warning
        // exists to prevent. `signing_secret_is_configured` is the one
        // predicate both call sites now share, so they cannot drift again.
        if !crate::domain::service::collect::signing_secret_is_configured(
            &cfg.collect_report_signing_secret,
        ) {
            warn!(
                "collect_report_signing_secret is empty or too short; every collect report will \
                 be refused with Forbidden (fail-closed by design - an empty or short HMAC key \
                 is not a safe default). Set this to a random per-deployment secret of at least \
                 16 characters before relying on exact collected case counts.",
            );
        }

        // Re-parameterised with `DomainError` so transaction closures preserve
        // domain variants; `ctx.db_required()` hands back a
        // `DBProvider<DbError>` (`toolkit/src/context.rs:211`).
        let db = Arc::new(DBProvider::<DomainError>::new(ctx.db_required()?.db()));

        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;

        // `deps` above already orders qa-runs ahead of this gear, which is what
        // makes the lookup find a registered client rather than failing at boot —
        // see the `deps` doc. **This said "the one client this gear reads today"
        // until Task 25a**, which added the qa-catalog lookup below.
        let qa_runs = ctx
            .client_hub()
            .get::<dyn QaRunsClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get qa-runs client: {e}"))?;

        // The second client, wired by **Task 25a**. This comment forecast it for
        // Task 40, on the grounds that Task 40's Step 3 names the lookup and that
        // neither Task 20 (which needs the *types*) nor Task 29 (the static
        // counts) would need to touch *this* client lookup. That was wrong in
        // the same way the `CatalogReader` adapter's assignment was: Task 25
        // issues the first real `list_universe` read, so it needed both
        // fifteen tasks early and took them. `domain::ports::catalog_reader`'
        // header carries the retraction; Task 40 keeps the rest of the wiring
        // below. (The lookup prediction held even so: Task 29 did come back to
        // this file, but for the unrelated `OrmCollectRepository` wiring below
        // the qa-catalog lookup, not for a second one.)
        let qa_catalog = ctx
            .client_hub()
            .get::<dyn QaCatalogClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get qa-catalog client: {e}"))?;

        // The third client, wired by **Task 25b** — the first task with a caller
        // for the `EnvironmentReader` port Task 25a shipped. The `qa_environments`
        // `deps` token above landed in the same commit as this line, which is
        // what makes the lookup find a registered client instead of failing at
        // boot; the `deps` doc records why nothing in the test suite could have
        // caught the gap.
        let qa_environments = ctx
            .client_hub()
            .get::<dyn QaEnvironmentsClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get qa-environments client: {e}"))?;

        // The fourth client, wired by **Task 32** — the outbound path for the
        // JIRA calls. The `oagw` `deps` token above has ordered that gear ahead
        // of this one since the skeleton, so this lookup needed no change to
        // `deps` (unlike Task 25b's, which had to add its token in the same
        // commit); `infra::jira::OagwJiraClient` is the adapter over it.
        //
        // Task 39's Slack egress resolves the *same* client from this same
        // lookup rather than adding a second.
        let oagw = ctx
            .client_hub()
            .get::<dyn ServiceGatewayClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get oagw client: {e}"))?;

        // **Both numbers, because this file has been counting two different
        // things and never said so** (fix round 1, after a review counted the
        // `client_hub().get` call sites and got a different answer from the
        // comments). The ordinals on the four comments above — "the one client",
        // "the second", "the third", "the fourth" — count the clients named by
        // the `deps` list, and deliberately exclude the `AuthZ` enforcer, which
        // every doc in this file discusses separately because it is consumed into
        // a `PolicyEnforcer` rather than held. There are therefore **four**
        // `deps` clients (qa-runs, qa-catalog, qa-environments, oagw) and
        // **five** lookups (those four plus `AuthZResolverClient`), and a
        // deployment that links this gear has to register all five: a fifth,
        // optional `event_broker` lookup lived here from Task 40 until it was
        // deleted, once it was established that no deployment ever registered
        // that client and the reconcile sweep was already carrying every
        // deployment's event ingest. See this file's header, "Event ingest, and
        // why there is only one path".
        //
        // One `QaRunsReader`, two ports: `RunsReader` for the ingest/reconcile
        // reads and, since Task 30, `RunsLauncher` for the collect launch.
        // Both wrap the identical `Arc<dyn QaRunsClientV1>` — see that
        // struct's own header for why this is one adapter and not two.
        let qa_runs_reader = Arc::new(QaRunsReader::new(qa_runs));

        let services = Arc::new(AppServices::new(
            OrmResultsRepository,
            OrmWatermarkRepository,
            OrmSavedViewsRepository,
            OrmCollectRepository,
            OrmJiraRepository,
            OrmNotifyRepository,
            ServiceDeps {
                db,
                authz,
                runs: Arc::clone(&qa_runs_reader) as Arc<dyn RunsReader>,
                catalog: Arc::new(QaCatalogReader::new(qa_catalog)),
                platforms: Arc::new(QaEnvironmentsReader::new(qa_environments)),
                // The production clock, one line of behaviour. Held by the
                // analytics service and read once per request;
                // `domain::ports::clock`'s header carries why it is a port.
                clock: Arc::new(SystemClock),
                // Task 30's: the one launch this gear performs against
                // qa-runs.
                runs_launcher: qa_runs_reader as Arc<dyn RunsLauncher>,
                // Task 32's: the JIRA egress, over oagw. The adapter carries an
                // in-process cache of the upstreams it has provisioned, so it is
                // built once here rather than per request.
                jira_client: Arc::new(OagwJiraClient::new(Arc::clone(&oagw))),
                // **Task 40's**: the real Slack egress, over the same `oagw`
                // client the JIRA adapter above uses — which is what Task 32's
                // comment on that lookup forecast. This replaces Task 38's
                // `NeverWiredSlackClient` stand-in, whose whole doc was about
                // being replaced here.
                //
                // **R107 — this does not mean Slack notifications are
                // delivered.** `infra::notify::slack_oagw`'s module doc,
                // "Finding B", carries the argument: a Slack webhook's secret is
                // its URL *path*, oagw's plugins inject only headers, and this
                // gear declares no `credstore-sdk` and so cannot resolve the
                // reference itself. Binding the adapter is what this task owes;
                // the credential problem is an open release-gate item whose two
                // candidate fixes are both cross-gear. What binding it changes is
                // *which* of Task 38's six claim outcomes a send reaches — a
                // failed send that releases the claim and writes an
                // `OUTCOME_FAILED` audit row, instead of `UnsupportedEgress` —
                // not how many outcomes there are.
                slack_client: Arc::new(SlackOagwClient::new(oagw)) as Arc<dyn SlackClient>,
                // **Task 40's**: Task 39's real mail adapter, replacing Task
                // 38's `NeverWiredMailClient`. Behaviourally identical to it and
                // permanently so — D10 defers the SMTP send itself — which is
                // why this one is not a placeholder for a future adapter but the
                // shipped answer. `mail_unsupported.rs`' module doc carries the
                // replacement recipe for whoever eventually adds SMTP: implement
                // `MailClient`, bind it on this line, delete nothing else.
                mail_client: Arc::new(UnsupportedMailClient) as Arc<dyn MailClient>,
                // Resolved to their final values here rather than carried as
                // config into the service, so nothing re-reads a knob per
                // request.
                reconcile_lookback: reconcile_lookback(cfg.reconcile_lookback_seconds),
                reconcile_page_size: cfg.reconcile_page_size,
                // Task 29's: the collect branch a request's own `branch` falls
                // back to. See `ServiceDeps::default_collect_branch`'s doc.
                default_collect_branch: cfg.default_collect_branch.clone(),
                // Task 30's: the base URL its collect callback is built on.
                collect_report_base_url: cfg.collect_report_base_url.clone(),
                // Task 30 fix round 1's: the HMAC key that authenticates the
                // callback's tenant claim.
                collect_report_signing_secret: cfg.collect_report_signing_secret.clone(),
            },
        ));

        self.runtime
            .set(Arc::new(QaInsightsRuntime {
                services: Arc::clone(&services),
                elector: elector(),
                reconciler: Cadence::reconciler(&cfg),
                jira_poller: Cadence::jira_poller(&cfg),
                collect: Cadence::collect(&cfg),
                default_collect_branch: cfg.default_collect_branch,
            }))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // **Task 40's**, and the last `// wired in Task N` gap to close: the
        // skip-list provider qa-runs' launch path reads
        // (`qa_insights_sdk::client::QaInsightsClientV1::skip_list_for`). Task 34
        // built the adapter and left it unregistered here, under R70, for the
        // same reason `domain::local_client`'s own header gives.
        //
        // Registered **after** `self.runtime.set`, matching qa-runs'
        // `QaRunsLocalClient` registration order, so a caller that resolves this
        // client the instant it appears cannot reach a gear whose runtime is
        // still unset.
        //
        // **R74: registering the provider is not the same as it being called.**
        // No task in this plan wires qa-runs' launch path to *invoke*
        // `skip_list_for`, so at the end of Phase C the skip list is
        // servable-and-unserved and `SKIP_TESTS_WITH_BUGS` stays
        // reserved-with-no-producer in `qa-runs/src/domain/params.rs:101`. That
        // is a recorded release-gate item, not an omission here.
        ctx.client_hub()
            .register::<dyn QaInsightsClientV1>(Arc::new(QaInsightsLocalClient::new(Arc::clone(
                &services.jira,
            ))));

        Ok(())
    }
}

/// The largest lookback this gear will honour: ten years.
///
/// # Why a ceiling exists at all
///
/// `reconcile_lookback_seconds` is a `u64` and the sweep computes
/// `watermark - lookback`. `OffsetDateTime`'s `Sub` **panics** on overflow, and
/// the type only spans years -9999 to 9999, so a config carrying `u64::MAX` — a
/// typo, a unit confusion, a templating accident — would take the process down
/// on the first sweep rather than produce a wrong answer. Nothing before Task 16
/// converted this field into a `Duration`, so this is the first task with a
/// reason to bound it.
///
/// Ten years, because the value it bounds is *"how far back does a sweep look
/// beyond its own watermark"* and the failure it exists to cover is a run
/// written after a later sweep's cutoff — a lag measured in seconds. Ten years
/// is already six orders of magnitude past useful and eight thousand years short
/// of the panic.
const MAX_LOOKBACK_SECONDS: i64 = 60 * 60 * 24 * 365 * 10;

/// `reconcile_lookback_seconds` as a [`Duration`], clamped and warned about
/// once.
///
/// Warned *here* and not in an accessor on the config, which is the distinction
/// qa-runs' `QaRunsRuntime` doc draws: a clamping accessor re-emits its warning
/// on every tick that calls it, while a value resolved at `init` says it once.
fn reconcile_lookback(seconds: u64) -> Duration {
    // `try_from` can only fail above `i64::MAX`, which the clamp below would
    // catch in any case; naming `i64::MAX` as the fallback keeps the two paths
    // one decision instead of two.
    let requested = i64::try_from(seconds).unwrap_or(i64::MAX);
    if requested > MAX_LOOKBACK_SECONDS {
        warn!(
            requested_seconds = requested,
            clamped_to_seconds = MAX_LOOKBACK_SECONDS,
            "reconcile_lookback_seconds is far past anything useful; clamping. A sweep's \
             lookback covers a run written after an earlier sweep's cutoff, which is a lag \
             measured in seconds.",
        );
        return Duration::seconds(MAX_LOOKBACK_SECONDS);
    }
    Duration::seconds(requested)
}

impl DatabaseCapability for QaInsights {
    /// This gear's schema, in application order.
    ///
    /// Delegates to [`crate::infra::storage::migrations::Migrator`] rather than
    /// listing the migrations here, so the append-only ordering has exactly one
    /// home: the migration module's own tests drive the same `Migrator`, which
    /// is what makes them evidence about what the platform runs at boot.
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        info!("Providing qa-insights database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for QaInsights {
    /// Registers this gear's routes.
    ///
    /// Task 9 declared this method returning the router untouched, so that the
    /// `init`-ordering question — "is the runtime set by the time routes are
    /// registered?" — was answered once, at the skeleton, rather than here where
    /// there is also a route to debug. That answer is why the `runtime.get()`
    /// below is unchanged from a task where it asserted nothing: it was the
    /// assertion, and it turned out to hold.
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering qa-insights REST routes");

        let runtime = self
            .runtime
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?;

        let router = routes::register_routes(router, openapi, Arc::clone(&runtime.services));

        info!("qa-insights REST routes registered successfully");
        Ok(router)
    }
}

// ---------------------------------------------------------------------------
// Lifecycle (stateful capability): the three tickers
// ---------------------------------------------------------------------------

impl QaInsights {
    /// Lifecycle entry (`stateful` capability). Starts whichever tickers this
    /// deployment has switched on, under a child of `cancel`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a ticker exits before it was cancelled, whether it
    /// panicked, was aborted, or returned of its own accord. Cooperative shutdown
    /// returns `Ok(())`.
    ///
    /// The ticker distinction is the point, and qa-runs' `serve` states it for
    /// its own tickers: a ticker that has stopped and a gear that is idle look
    /// identical from outside, and one of them means the projection has stopped
    /// self-healing.
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the `tracing` macros in each early return: the four ways this \
                  function declines to start something - no runtime, one disabled ticker per \
                  role, and nothing to supervise at all - each owe an operator a distinct line \
                  saying which, and the metric counts every expansion as a branch. Same \
                  diagnosis qa-runs' own `serve` records"
    )]
    pub(crate) async fn serve(self: Arc<Self>, cancel: CancellationToken) -> anyhow::Result<()> {
        let Some(rt) = self.runtime.get().cloned() else {
            info!("qa-insights: serve() with no runtime (uninitialized); idling until cancelled");
            cancel.cancelled().await;
            return Ok(());
        };

        // A child token so the supervisor can stop the background work on its own
        // terms, and so a shutdown of the runtime reaches it either way.
        let tasks = cancel.child_token();

        let mut tickers = Tickers::default();

        if rt.reconciler.runs {
            let role = tickers.spawn(Self::reconcile_ticker(&rt, &tasks));
            info!(
                interval_seconds = rt.reconciler.interval_seconds,
                role, "qa-insights reconcile ticker started"
            );
        } else {
            // The interval is reported so an operator can tell *which* switch
            // turned this off: a non-zero value here means `enable_tickers` did.
            // Same for the other two branches below.
            info!(
                reconcile_interval_seconds = rt.reconciler.interval_seconds,
                // The consequence is the one an operator does not expect: the
                // reconcile sweep is this gear's only ingest path (this file's
                // header, "Event ingest, and why there is only one path"), so
                // with no sweep on any replica nothing projects a finished run
                // until somebody runs `POST /qa/v1/insights/rebuild` by hand.
                // (ASCII hyphens, not em dashes: `clippy::non_ascii_literal`
                // spans the whole macro invocation, comments included.)
                "qa-insights: no reconcile sweep on this replica; if no replica runs one, this \
                 gear ingests nothing until an operator replays a window through \
                 POST /qa/v1/insights/rebuild"
            );
        }

        if rt.jira_poller.runs {
            let role = tickers.spawn(Self::jira_poller_ticker(&rt, &tasks));
            info!(
                interval_seconds = rt.jira_poller.interval_seconds,
                role, "qa-insights JIRA poller ticker started"
            );
        } else {
            info!(
                jira_poller_interval_seconds = rt.jira_poller.interval_seconds,
                "qa-insights: JIRA poller disabled; a bug resolved in JIRA stays `Open` in this \
                 gear's registry, so it stays on the skip list and its test is never re-run \
                 automatically"
            );
        }

        if rt.collect.runs {
            let role = tickers.spawn(Self::collect_ticker(&rt, &tasks));
            info!(
                interval_seconds = rt.collect.interval_seconds,
                role, "qa-insights collect ticker started"
            );
        } else {
            info!(
                collect_interval_seconds = rt.collect.interval_seconds,
                "qa-insights: collect cycle disabled; expected-case counts fall back to the \
                 catalog's static numbers, which is what `analytics::expected_cases` does for \
                 every test file with no exact count"
            );
        }

        if tickers.is_empty() {
            // The one combination in which this gear ingests **nothing**: no
            // ticker to sweep for finished runs. It is a legitimate
            // configuration — a read-only replica answers every REST route from
            // rows another replica wrote — and it is indistinguishable from a
            // broken gear in a log that does not say so, hence `warn!`.
            warn!(
                "qa-insights: no ticker enabled; this replica ingests nothing and serves reads \
                 only. POST /qa/v1/insights/rebuild is the only way to project a run into this \
                 gear's database from here."
            );
            cancel.cancelled().await;
            Ok(())
        } else {
            supervise(cancel, &tasks, tickers).await
        }
    }

    /// Spawn the leader-gated reconcile ticker.
    ///
    /// # The alert obligation, which is this ticker's whole reason for existing
    /// # beyond the sweep
    ///
    /// `ReconcileOutcome::stopped_at_gap` means the projection is **frozen** for
    /// that tenant from that instant: `consume_page` breaks at the first run it
    /// cannot backfill and never advances the watermark past it — which is
    /// correct, and is what stops a failed run being stranded forever — so a run
    /// that fails *permanently* means no run finishing after it is ever
    /// backfilled for that tenant. No error is returned and no data is reported
    /// missing. `domain::service::reconcile`'s header states the same thing at
    /// length and names this ticker as the only place it can surface, because it
    /// is: the outcome is a return value with exactly one caller.
    ///
    /// So a `stopped_at_gap` is a **`WARN` naming the tenant and the run**, not a
    /// `debug!` alongside the pass counters. Legacy could not wedge this way and
    /// not because it was more careful — it had no watermark, so one unpersistable
    /// run cost exactly that run.
    ///
    /// ## A tenant wedged on consecutive passes is a louder signal, and this is
    /// ## where that decision lives
    ///
    /// The plan asked whoever implemented this to decide whether a *repeatedly*
    /// wedged tenant deserves more than a single one. **It does, and the ticker
    /// carries the history**, because nothing else can: `ReconcileOutcome` is
    /// `Copy` and stateless by design, and the alternative homes are a column
    /// (schema change, no task) or nothing (the operator greps).
    ///
    /// The history is deliberately the *smallest* thing that answers the
    /// question: a `HashMap<Uuid, u32>` of consecutive wedged passes per tenant,
    /// local to one leadership term, incremented on a `stopped_at_gap` and
    /// **removed** on a clean pass. Every wedged pass still logs at `WARN`; the
    /// count rides that line, and crossing [`WEDGED_PASSES_BEFORE_ERROR`]
    /// escalates it to `ERROR` and keeps it there. That gives an operator two
    /// distinguishable stories — "one run failed to backfill, it may be
    /// transient" and "this tenant has not advanced in a quarter of an hour" — from one
    /// grep, which a stateless `WARN` cannot.
    ///
    /// *Rejected:* a persisted counter (needs a column and turns a diagnostic
    /// into schema), and escalating on the *same* `stopped_at_run` only (a tenant
    /// wedging on a different run each pass is just as frozen — the watermark
    /// still is not moving — and keying on the run id would report that as
    /// healthy). The map is keyed on the tenant for exactly that reason.
    ///
    /// **Resetting per leadership term is a real limitation and is not hidden**:
    /// leadership changing hands restarts every tenant's count from zero, so a
    /// tenant wedged across a failover is re-reported as a first offence. Under
    /// the shipped `NoopLeaderElector` a term is a process lifetime, so in
    /// practice the reset is a restart. Persisting it is the fix and it needs the
    /// column this deliberately does not add.
    fn reconcile_ticker(
        rt: &Arc<QaInsightsRuntime>,
        tasks: &CancellationToken,
    ) -> Ticker<impl Future<Output = ()> + Send + 'static> {
        // Bound once, so the value the elector holds leadership under and the
        // value `Tickers` files this task under are the *same* one rather than
        // two spellings that could drift. qa-runs' `Ticker` doc records what the
        // two-spellings version cost.
        let role = ROLE_RECONCILER;
        let elector = Arc::clone(&rt.elector);
        let services = Arc::clone(&rt.services);
        let period = std::time::Duration::from_secs(rt.reconciler.interval_seconds);
        let cancel = tasks.clone();

        let run = async move {
            let work = work_fn(move |cancel| {
                let services = Arc::clone(&services);
                async move {
                    // Per leadership term, not per process: `run_role` may invoke
                    // this closure again after a re-election, and a fresh term is
                    // a fresh view of what is wedged. See this function's doc.
                    let mut wedged: std::collections::HashMap<uuid::Uuid, u32> =
                        std::collections::HashMap::new();
                    let mut ticker = tokio::time::interval(period);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        tokio::select! {
                            () = cancel.cancelled() => {
                                info!("qa-insights reconciler stopping (leadership lost or shutdown)");
                                return Ok(());
                            }
                            _ = ticker.tick() => {
                                reconcile_pass(&services, &mut wedged, &cancel).await;
                            }
                        }
                    }
                }
            });

            if let Err(error) = elector.run_role(role, cancel, work).await {
                error!(%error, "qa-insights reconciler exited with an error");
            }
        };
        Ticker { role, run }
    }

    /// Spawn the leader-gated JIRA poller ticker.
    ///
    /// # Leadership here is a correctness requirement, not an optimisation, and
    /// # that differs from the reconciler
    ///
    /// `infra::leader`'s header argues that election in this gear is an
    /// optimisation, and for the reconciler and the collect cycle it is: both are
    /// idempotent replays. **This ticker is not.** `JiraPollerService::rerun`
    /// *launches a run* through qa-runs' normal admission path, and two replicas
    /// polling one tenant concurrently would each see the same bug resolve and
    /// each launch a rerun — legacy's `poll_resolved_bugs` has no guard against
    /// that because legacy runs one manager.
    ///
    /// What actually bounds the damage today is the *local resolve write*: once
    /// `resolve_bug` has run, the bug leaves `open_bugs` and the next pass does
    /// not see it. That is a race, not a lock, and under the shipped
    /// `NoopLeaderElector` — where every replica is the leader — it is live.
    /// Stated rather than implied, because `infra::leader`'s blanket
    /// "optimisation" sentence would otherwise read as covering this ticker too.
    /// Closing it needs a claim row of the kind
    /// `idx_qa_run_notifications_claim` gives the notification path, which no
    /// task owns.
    fn jira_poller_ticker(
        rt: &Arc<QaInsightsRuntime>,
        tasks: &CancellationToken,
    ) -> Ticker<impl Future<Output = ()> + Send + 'static> {
        // One binding, for the reason `reconcile_ticker` gives.
        let role = ROLE_JIRA_POLLER;
        let elector = Arc::clone(&rt.elector);
        let services = Arc::clone(&rt.services);
        let period = std::time::Duration::from_secs(rt.jira_poller.interval_seconds);
        let cancel = tasks.clone();

        let run = async move {
            let work = work_fn(move |cancel| {
                let services = Arc::clone(&services);
                async move {
                    let mut ticker = tokio::time::interval(period);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        tokio::select! {
                            () = cancel.cancelled() => {
                                info!("qa-insights JIRA poller stopping (leadership lost or shutdown)");
                                return Ok(());
                            }
                            _ = ticker.tick() => {
                                jira_poll_pass(&services, &cancel).await;
                            }
                        }
                    }
                }
            });

            if let Err(error) = elector.run_role(role, cancel, work).await {
                error!(%error, "qa-insights JIRA poller exited with an error");
            }
        };
        Ticker { role, run }
    }

    /// Spawn the leader-gated collect ticker.
    ///
    /// Legacy's hourly `start_collect_poller`
    /// (`manager/src/services/collect.rs:183-188`), with two differences that are
    /// both consequences of this gear being multi-tenant and leader-elected where
    /// legacy is neither:
    ///
    /// * it iterates the tenants
    ///   [`TenantDirectory`](crate::domain::service::tenants::TenantDirectory)
    ///   reports, where legacy has one;
    /// * it **ticks first and sleeps after**, where legacy's loop sleeps first
    ///   (`collect.rs:187`, the `tokio::time::sleep` at the head of the loop
    ///   body). `tokio::time::interval`'s first tick is immediate. That is a
    ///   deliberate divergence from a cited line, and not an invention: legacy
    ///   runs the *other* poller in this pair the same way this one now runs,
    ///   under `env_bool("RUN_RESULTS_SYNC_ON_STARTUP", true)`
    ///   (`manager/src/main.rs:183`) — and for a cycle whose whole output is a
    ///   cached count, waiting an hour after a deploy to refresh it is the worse
    ///   of the two.
    fn collect_ticker(
        rt: &Arc<QaInsightsRuntime>,
        tasks: &CancellationToken,
    ) -> Ticker<impl Future<Output = ()> + Send + 'static> {
        // One binding, for the reason `reconcile_ticker` gives.
        let role = ROLE_COLLECT;
        let elector = Arc::clone(&rt.elector);
        let services = Arc::clone(&rt.services);
        let branch = rt.default_collect_branch.clone();
        let period = std::time::Duration::from_secs(rt.collect.interval_seconds);
        let cancel = tasks.clone();

        let run = async move {
            let work = work_fn(move |cancel| {
                let services = Arc::clone(&services);
                let branch = branch.clone();
                async move {
                    let mut ticker = tokio::time::interval(period);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        tokio::select! {
                            () = cancel.cancelled() => {
                                info!("qa-insights collect cycle stopping (leadership lost or shutdown)");
                                return Ok(());
                            }
                            _ = ticker.tick() => {
                                collect_pass(&services, &branch, &cancel).await;
                            }
                        }
                    }
                }
            });

            if let Err(error) = elector.run_role(role, cancel, work).await {
                error!(%error, "qa-insights collect cycle exited with an error");
            }
        };
        Ticker { role, run }
    }
}

/// Consecutive wedged passes before a tenant's `stopped_at_gap` is reported at
/// `ERROR` rather than `WARN`.
///
/// Three, so the escalation lands at three times the tick interval — fifteen
/// minutes on the default 300-second reconciler. Chosen against what the two
/// levels have to mean: a `WARN` is "one run would not backfill, it may be a
/// transient qa-runs failure and the next pass may clear it", and an `ERROR` is
/// "this tenant's projection has not advanced for long enough that nobody should
/// still be waiting". One pass is too eager for the first reading and ten would
/// put the second most of an hour after the fact.
const WEDGED_PASSES_BEFORE_ERROR: u32 = 3;

/// The tenants a pass will work on: `Some(list)` when the directory answered —
/// possibly with an empty list — and `None` when the read was refused or failed.
///
/// # The two are **not** interchangeable, and fix round 1 is why this returns an
/// # `Option`
///
/// This returned a bare `Vec` and folded the refusal into an empty one, on the
/// stated grounds that *"an empty answer and a refused one are deliberately the
/// same to the caller … returning a `Result` instead would put an identical match
/// arm in all three passes for no extra information"*. **[`reconcile_pass`] is a
/// counter-example to that rationale**: it prunes its consecutive-wedge history
/// against this answer, so a single transient PDP failure or database error
/// looked exactly like "every tenant has left the directory" and reset every
/// tenant's count to zero — indefinitely deferring the `ERROR` that
/// [`WEDGED_PASSES_BEFORE_ERROR`] exists to raise, which is the load-bearing half
/// of the alert obligation. Found by review, not by the tests.
///
/// The other two passes keep no state between passes either, so treating
/// `None` as zero tenants costs them nothing behaviourally. **They used to say
/// so by mapping it through `unwrap_or_default()`, and review finding #56 is
/// why that changed**: folding a refused enumeration into an empty `Vec` at
/// the call site made a ticker whose background work has silently stopped
/// indistinguishable, by inspection, from one with nothing to do — the same
/// shape of trap [`reconcile_pass`] was already fixed against, just without a
/// wedge count to corrupt. Both call sites now match this `Option` explicitly
/// and return without iterating on `None`, exactly as [`reconcile_pass`]
/// already did; neither adds a second `warn!` of its own, since this function
/// has already logged one by the time either sees `None`.
///
/// Not fatal to the ticker either way. The enumeration no longer asks a PDP
/// that could decline it — `domain::service::tenants::TenantDirectory::scope`
/// elevates through `domain::elevated` instead, so the only way this branch is
/// reached at all is a database error — but a transient one today may clear on
/// the next pass, which is exactly why it must not be allowed to erase a
/// signal. See `domain::service::tenants`' header for the elevation and
/// `domain::elevated`'s for why this read carries no deployment obligation any
/// more.
async fn tenants_for(
    services: &Arc<ConcreteAppServices>,
    ticker: &'static str,
) -> Option<Vec<TenantBound>> {
    match services.tenants.known_tenants().await {
        Ok(tenants) => Some(tenants),
        Err(error) => {
            warn!(
                ticker,
                %error,
                "qa-insights could not enumerate tenants; this pass does nothing"
            );
            None
        }
    }
}

/// Drop the wedge history of every tenant the directory no longer lists — and
/// **only** when the directory actually answered.
///
/// # `None` must not prune, and fix round 1 exists because it did
///
/// A map nothing prunes keeps a departed tenant wedged forever, so pruning is
/// necessary. But the first version pruned against
/// [`tenants_for`]'s answer *after* that function had folded a refused
/// enumeration into an empty `Vec` — so one transient PDP refusal or database
/// error read as "every tenant has left the directory", reset every standing
/// count to zero, and indefinitely deferred the `ERROR` that
/// [`WEDGED_PASSES_BEFORE_ERROR`] exists to raise. On a deployment where the
/// refusal is the *expected* state (see `domain::service::tenants`' header) that
/// is not a rare race; it is every pass.
///
/// So the decision has a name and one home rather than being a line inside
/// [`reconcile_pass`]: "no longer listed" and "could not be read" are different
/// facts, `Option` is what distinguishes them, and
/// [`a_refused_enumeration_does_not_clear_a_standing_wedge_count`](tests::a_refused_enumeration_does_not_clear_a_standing_wedge_count)
/// is what holds it to that.
fn prune_wedged(
    wedged: &mut std::collections::HashMap<uuid::Uuid, u32>,
    tenants: Option<&[TenantBound]>,
) {
    let Some(tenants) = tenants else {
        return;
    };
    wedged.retain(|tenant, _| tenants.iter().any(|bound| bound.get() == *tenant));
}

/// One reconcile pass over every tenant, with the wedge history the ticker
/// keeps.
///
/// A free function rather than a body inside the ticker's closure so the pass has
/// somewhere to be documented and read; `wedged` is threaded in rather than
/// captured so the closure stays the only owner of the term's state.
///
/// `cancel` is checked at the top of the per-tenant loop, before that
/// tenant's reconcile call — not after, since the call is the round trip a
/// shutdown is trying to cut off. Stopping early leaves `wedged` exactly as
/// it stood after the last tenant this call reached; the next tick resumes
/// from there, same as any other pass that ends partway through.
async fn reconcile_pass(
    services: &Arc<ConcreteAppServices>,
    wedged: &mut std::collections::HashMap<uuid::Uuid, u32>,
    cancel: &CancellationToken,
) {
    let tenants = tenants_for(services, ROLE_RECONCILER).await;
    prune_wedged(wedged, tenants.as_deref());

    let Some(tenants) = tenants else {
        return;
    };

    for tenant in tenants {
        if cancel.is_cancelled() {
            return;
        }
        match services.reconcile.reconcile_once(tenant).await {
            Ok(outcome) => report_reconcile_outcome(tenant, &outcome, wedged),
            Err(error) => warn!(
                tenant_id = %tenant.get(),
                %error,
                "qa-insights reconcile pass failed for this tenant; the others still run"
            ),
        }
    }
}

/// Log one tenant's sweep, escalating a tenant that has been wedged for
/// [`WEDGED_PASSES_BEFORE_ERROR`] consecutive passes.
///
/// Split out of [`reconcile_pass`] because it is where the obligation lives and
/// because it is the only part of the pass with a decision in it — see
/// [`QaInsights::reconcile_ticker`]'s doc for the whole argument, including what
/// the alternatives to a per-term map were and why they were rejected.
#[allow(
    clippy::cognitive_complexity,
    reason = "inflated by the three `tracing` macros, which are the whole function: a clean pass, \
              a first wedge and a persistent wedge are three different operator stories and the \
              metric counts every field of every expansion as a branch. Splitting further would \
              put one log line per function. Same diagnosis qa-runs' `supervise` records"
)]
fn report_reconcile_outcome(
    tenant: TenantBound,
    outcome: &ReconcileOutcome,
    wedged: &mut std::collections::HashMap<uuid::Uuid, u32>,
) {
    if !outcome.stopped_at_gap {
        // A clean pass clears the history, so the escalation below means
        // "consecutive" and not "ever".
        wedged.remove(&tenant.get());
        debug!(
            tenant_id = %tenant.get(),
            scanned = outcome.scanned,
            backfilled = outcome.backfilled,
            watermark_advanced_to = ?outcome.watermark_advanced_to,
            "qa-insights reconcile pass"
        );
        return;
    }

    let passes = *wedged
        .entry(tenant.get())
        .and_modify(|n| *n += 1)
        .or_insert(1);
    if passes >= WEDGED_PASSES_BEFORE_ERROR {
        error!(
            tenant_id = %tenant.get(),
            stopped_at_run = ?outcome.stopped_at_run,
            consecutive_wedged_passes = passes,
            "qa-insights reconciler has not advanced this tenant's watermark for several \
             consecutive passes: its projection is FROZEN from the named run onward and no later \
             run will be backfilled until that run can be. Replay a narrow window with POST \
             /qa/v1/insights/rebuild, and if that fails too the run needs a look in qa-runs"
        );
    } else {
        warn!(
            tenant_id = %tenant.get(),
            stopped_at_run = ?outcome.stopped_at_run,
            consecutive_wedged_passes = passes,
            scanned = outcome.scanned,
            backfilled = outcome.backfilled,
            "qa-insights reconciler stopped at a run it could not backfill; the watermark did \
             not advance past it, so nothing that finished later is projected for this tenant \
             until it can be"
        );
    }
}

/// One JIRA poller pass over every tenant.
///
/// Each tenant gets its own `system_actor::for_jira_poll` context, minted from
/// the [`TenantBound`] the directory returned — never the enumeration's own
/// nil-tenant context. `domain::service::tenants`' header carries why that split
/// is the whole safety property here.
///
/// A tenant with no JIRA configuration, or a disabled one, costs one read:
/// `poll_once` calls `JiraService::active_config` before it looks at any bug,
/// which is legacy's own first step (`jira_poller.rs:40-43`).
///
/// `cancel` is checked at the top of the per-tenant loop, before that
/// tenant's poll — not after, since `poll_once` is the round trip (JIRA
/// itself, then qa-runs for a rerun) a shutdown is trying to cut off.
/// Stopping early costs nothing beyond what any skipped tenant already
/// costs: this pass is stateless between ticks, so the next one covers it.
async fn jira_poll_pass(services: &Arc<ConcreteAppServices>, cancel: &CancellationToken) {
    // A failed directory read is not "this deployment has no tenants".
    // `unwrap_or_default()` used to fold the two together here, which made a
    // stopped ticker indistinguishable from an idle one at this call site —
    // even though [`tenants_for`] had already warned, nothing here said so.
    // Matching instead, exactly as [`reconcile_pass`] already had to, costs
    // this pass nothing: it keeps no state between passes, so returning
    // before the loop and running it zero times come to the same thing.
    // Review finding #56.
    let Some(tenants) = tenants_for(services, ROLE_JIRA_POLLER).await else {
        return;
    };
    for tenant in tenants {
        if cancel.is_cancelled() {
            return;
        }
        let ctx = system_actor::for_jira_poll(tenant);
        match services.jira_poller.poll_once(&ctx).await {
            Ok(()) => debug!(tenant_id = %tenant.get(), "qa-insights JIRA poller pass"),
            // `poll_once` swallows every per-bug failure by design, so anything
            // that reaches here is the config read or the open-bugs listing —
            // whole-tenant, and the next pass retries it.
            Err(error) => warn!(
                tenant_id = %tenant.get(),
                %error,
                "qa-insights JIRA poller pass failed for this tenant; the others still run"
            ),
        }
    }
}

/// One collect cycle over every tenant, on the default branch.
///
/// `cancel` is checked at the top of the per-tenant loop, before that
/// tenant's cycle — not after, matching [`jira_poll_pass`]:
/// `run_collect_cycle` is itself a qa-catalog round trip plus a launch per
/// repository, and a shutdown should not wait for it to finish before
/// stopping.
async fn collect_pass(
    services: &Arc<ConcreteAppServices>,
    branch: &str,
    cancel: &CancellationToken,
) {
    // Stateless between passes, exactly like [`jira_poll_pass`] — see its
    // comment. Review finding #56.
    let Some(tenants) = tenants_for(services, ROLE_COLLECT).await else {
        return;
    };
    for tenant in tenants {
        if cancel.is_cancelled() {
            return;
        }
        let ctx = system_actor::for_collect_cycle(tenant);
        match services.collect.run_collect_cycle(&ctx, branch).await {
            Ok(launched) => debug!(
                tenant_id = %tenant.get(),
                branch,
                launched,
                "qa-insights collect cycle"
            ),
            // A per-repository launch failure is already swallowed inside
            // `run_collect_cycle`; an `Err` here is the qa-catalog universe read,
            // which that method deliberately does not launder into "collect
            // nothing".
            Err(error) => warn!(
                tenant_id = %tenant.get(),
                branch,
                %error,
                "qa-insights collect cycle failed for this tenant; the others still run"
            ),
        }
    }
}

/// A ticker that is ready to spawn: the role it holds, and the loop that holds
/// it.
///
/// **The role travels *with* the loop rather than being supplied beside it**,
/// which is what keeps the two from being paired up wrongly. Copied from
/// qa-runs' own `Ticker`, whose doc records what the two earlier designs cost:
/// reading the role off the future's output loses it on the panic arm, and taking
/// it as a second argument to `spawn` let the two names be transposed while the
/// whole suite stayed green.
struct Ticker<F> {
    role: &'static str,
    run: F,
}

/// The spawned tickers, and which role each spawned task is.
///
/// `JoinError` carries `id()`, and [`JoinSet::spawn`] hands back an
/// `AbortHandle` with the same id, so recording the pair at spawn is what makes
/// the role available on **both** the `Ok` and the `Err` arm — the `Err` arm
/// being the panic, which is the one case a shared supervisor exists for.
/// qa-runs' identical type carries the measurement.
#[derive(Default)]
struct Tickers {
    set: JoinSet<()>,
    roles: Vec<(tokio::task::Id, &'static str)>,
}

impl Tickers {
    /// Spawn `ticker`, returning the role it holds so the caller can log it
    /// without naming it a second time.
    fn spawn<F: Future<Output = ()> + Send + 'static>(
        &mut self,
        ticker: Ticker<F>,
    ) -> &'static str {
        let handle = self.set.spawn(ticker.run);
        self.roles.push((handle.id(), ticker.role));
        ticker.role
    }

    fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// The next ticker to finish, with the role it was, or `None` when the set is
    /// empty.
    ///
    /// Resolving the role here rather than in [`supervise`] keeps [`Self::roles`]
    /// and [`Self::role_of`]'s invariant inside one type: the supervisor never
    /// touches [`Self::set`], so it has no way to add a task the map does not know
    /// about.
    async fn join_next(&mut self) -> Option<(&'static str, Result<(), tokio::task::JoinError>)> {
        match self.set.join_next_with_id().await {
            None => None,
            Some(Ok((id, ()))) => Some((self.role_of(id), Ok(()))),
            Some(Err(error)) => Some((self.role_of(error.id()), Err(error))),
        }
    }

    /// The role that task is, or a placeholder.
    ///
    /// **No path in this module reaches the placeholder**, because [`Self::spawn`]
    /// is the only thing that inserts into [`Self::set`] and it records the pair.
    /// That is an invariant of this type rather than a property of the id, so it
    /// is stated as such — a `self.set.spawn(..)` written anywhere in this file
    /// would break it silently.
    ///
    /// A placeholder rather than a panic because this is only ever called while
    /// reporting somebody else's failure, and a supervisor that panicked there
    /// would replace a diagnosable error with an undiagnosable one.
    fn role_of(&self, id: tokio::task::Id) -> &'static str {
        self.roles
            .iter()
            .find(|(task, _)| *task == id)
            .map_or("unidentified", |(_, role)| *role)
    }
}

/// Wait for shutdown, or surface a premature exit by **any** ticker.
///
/// Each ticker's loop returns only on cancellation, so a join before `cancel`
/// fires means it panicked, was aborted, or returned on its own — the last of
/// which happens when `LeaderElector::run_role` reports an error, which the
/// ticker logs before returning normally. All three are the same fault to an
/// operator and all three are reported.
///
/// **One supervisor over a set, rather than one per ticker.** A gear whose
/// reconciler has panicked while its collect cycle runs on is exactly as broken
/// as one with no reconciler, and half as visible; the set is what makes the
/// first exit — whichever it is — the thing that ends `serve`. [`Tickers`] is
/// what makes the error name the one that stopped, on every arm.
///
/// An empty set makes this return `Ok(())` at once rather than waiting for
/// cancellation — `JoinSet::join_next_with_id` resolves to `None` immediately
/// when nothing is spawned — which is why [`QaInsights::serve`] handles
/// "nothing enabled" itself instead of passing an empty set here: a gear with
/// every ticker switched off must still idle until shutdown, not exit at once.
#[allow(
    clippy::cognitive_complexity,
    reason = "one log line per shutdown path, and the paths are the point: drained, join-failed \
              and exited-early are three different operator stories"
)]
async fn supervise(
    cancel: CancellationToken,
    tasks: &CancellationToken,
    mut tickers: Tickers,
) -> anyhow::Result<()> {
    let premature = tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        joined = tickers.join_next() => joined,
    };

    tasks.cancel();

    let Some((role, outcome)) = premature else {
        while let Some((role, outcome)) = tickers.join_next().await {
            match outcome {
                Ok(()) => debug!(role, "qa-insights ticker drained"),
                Err(e) => error!(
                    role,
                    error = %e,
                    "qa-insights ticker join failed during shutdown",
                ),
            }
        }
        return Ok(());
    };

    match outcome {
        Ok(()) => Err(anyhow::anyhow!(
            "qa-insights {role} task exited unexpectedly"
        )),
        Err(join_err) => Err(anyhow::anyhow!(
            "qa-insights {role} task panicked or was aborted: {join_err}"
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! The decisions in this file that are not wiring.
    //!
    //! **There is still no unit test that `init` or `serve` wires correctly, and
    //! that is no longer a gap.** It used to read "a real gap rather than an
    //! oversight: `GearCtx` is constructed by the runtime and `ClientHub`
    //! resolution needs registered gears, so a composition test here would be a
    //! platform harness." Task 40 wrote that harness — as an integration target,
    //! `tests/ingest_idempotence.rs`, where it belongs: it boots this gear
    //! through the real `init`, drives the generated `Runnable` impl that
    //! `lifecycle(entry = "serve", ..)` emits, drives `POST
    //! /qa/v1/insights/rebuild` over HTTP and reads the result back the same
    //! way — the operator's ingest path, since the transactional consumer that
    //! target used to drive instead was deleted along with the event-broker
    //! dependency.
    //!
    //! It is an integration target rather than a module here for the reason its
    //! own header gives: an integration target compiles the library **without**
    //! `cfg(test)`, so it sees exactly the public surface a host sees.
    //! `api::rest::routes::tests` still covers the other half of `register_rest`
    //! — the registration that can panic at startup.
    //!
    //! `NeverWiredSlackClient`/`NeverWiredMailClient` and their two tests lived
    //! here from Task 38's fix round 1 (R103) until **Task 40 replaced both
    //! stand-ins with the real adapters** and removed them. Their never-errors
    //! contract did not go untested with them: it is `infra::notify`'s, where
    //! `UnsupportedMailClient` and `SlackOagwClient` each carry their own tests.

    use super::{
        Cadence, MAX_LOOKBACK_SECONDS, WEDGED_PASSES_BEFORE_ERROR, prune_wedged,
        reconcile_lookback, report_reconcile_outcome,
    };
    use crate::config::{MIN_COLLECT_INTERVAL_SECONDS, QaInsightsConfig};
    use crate::domain::service::reconcile::ReconcileOutcome;
    use crate::domain::system_actor::TenantBound;
    use std::collections::HashMap;
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    const TENANT_A: Uuid = Uuid::from_u128(0x0A);
    const TENANT_B: Uuid = Uuid::from_u128(0xB0);

    fn bound(id: Uuid) -> TenantBound {
        TenantBound::new(id).expect("non-nil")
    }

    fn wedged_outcome() -> ReconcileOutcome {
        ReconcileOutcome {
            scanned: 1,
            stopped_at_gap: true,
            stopped_at_run: Some(Uuid::from_u128(0xDEAD)),
            ..ReconcileOutcome::default()
        }
    }

    fn clean_outcome() -> ReconcileOutcome {
        ReconcileOutcome {
            scanned: 1,
            backfilled: 1,
            ..ReconcileOutcome::default()
        }
    }

    #[test]
    fn an_ordinary_lookback_is_carried_through_unchanged() {
        assert_eq!(reconcile_lookback(3600), Duration::hours(1));
        assert_eq!(reconcile_lookback(0), Duration::ZERO);
    }

    /// The failure this clamp exists for: an unbounded `u64` reaching
    /// `watermark - lookback`, where `OffsetDateTime`'s `Sub` panics rather than
    /// saturating. Subtracting the clamped value is the assertion — an
    /// unclamped one would take the process down here.
    #[test]
    fn an_absurd_lookback_is_clamped_to_something_a_timestamp_can_subtract() {
        let clamped = reconcile_lookback(u64::MAX);
        assert_eq!(clamped, Duration::seconds(MAX_LOOKBACK_SECONDS));

        let mark = OffsetDateTime::now_utc();
        let floor = mark - clamped;
        assert!(floor < mark);
    }

    /// **The three tickers are three independently switchable roles.** The
    /// mutation this catches is the one a single shared switch would produce:
    /// `infra::leader`'s header and qa-runs' `SCHEDULER_ROLE` both say why
    /// "run this ticker here but not that one" has to be expressible, and a
    /// `Cadence` that read the wrong field would make two of the three move
    /// together.
    #[test]
    fn each_ticker_is_switched_by_its_own_interval() {
        let cfg = QaInsightsConfig {
            reconcile_interval_seconds: 0,
            collect_interval_seconds: 0,
            jira_poller_interval_seconds: 60,
            ..QaInsightsConfig::default()
        };

        assert!(!Cadence::reconciler(&cfg).runs, "0 disables the reconciler");
        assert!(Cadence::jira_poller(&cfg).runs, "60 runs the JIRA poller");
        assert!(!Cadence::collect(&cfg).runs, "0 disables the collect cycle");
        assert_eq!(Cadence::jira_poller(&cfg).interval_seconds, 60);
    }

    /// `enable_tickers` is the master switch, and it must reach all three —
    /// including a ticker whose own interval is perfectly valid.
    #[test]
    fn the_master_switch_stops_every_ticker() {
        let cfg = QaInsightsConfig {
            enable_tickers: false,
            ..QaInsightsConfig::default()
        };

        assert!(!Cadence::reconciler(&cfg).runs);
        assert!(!Cadence::jira_poller(&cfg).runs);
        assert!(!Cadence::collect(&cfg).runs);
        // The interval survives the switch, because the "disabled" log lines
        // report it — an operator needs to know which of the two switches they
        // used.
        assert_eq!(
            Cadence::reconciler(&cfg).interval_seconds,
            QaInsightsConfig::default().reconcile_interval_seconds
        );
    }

    /// The escalation ladder: consecutive wedged passes count up, and a clean
    /// pass clears the history so "consecutive" means consecutive.
    ///
    /// Driven through `report_reconcile_outcome` — the function that owns the
    /// decision — rather than through a pass, because a pass needs the whole
    /// services graph and the ladder is what the obligation is about.
    #[test]
    fn consecutive_wedged_passes_count_up_and_a_clean_pass_clears_them() {
        let mut wedged: HashMap<Uuid, u32> = HashMap::new();

        for expected in 1..=WEDGED_PASSES_BEFORE_ERROR {
            report_reconcile_outcome(bound(TENANT_A), &wedged_outcome(), &mut wedged);
            assert_eq!(wedged.get(&TENANT_A), Some(&expected));
        }

        report_reconcile_outcome(bound(TENANT_A), &clean_outcome(), &mut wedged);
        assert_eq!(
            wedged.get(&TENANT_A),
            None,
            "a clean pass clears the history, so the ERROR means `consecutive` and not `ever`"
        );
    }

    /// **A refused enumeration must not clear a standing wedge count.**
    ///
    /// This is fix round 1's regression test. The defect: `tenants_for` folded a
    /// refused enumeration into an empty `Vec`, and `reconcile_pass` pruned
    /// against it — so one transient PDP refusal or database error read as "every
    /// tenant has left the directory" and reset every count to zero, indefinitely
    /// deferring the `ERROR` [`WEDGED_PASSES_BEFORE_ERROR`] exists to raise. On a
    /// deployment where the refusal is the expected state that is every pass, not
    /// a rare race.
    ///
    /// The sequence is the real one: two wedged passes, then a pass whose
    /// enumeration failed (`None`), then a third wedged pass. The count must
    /// reach the escalation threshold, because the tenant has in fact been wedged
    /// for three passes and nothing said otherwise. Under the old code the
    /// `None` pass emptied the map and the third pass started again at 1.
    #[test]
    fn a_refused_enumeration_does_not_clear_a_standing_wedge_count() {
        let mut wedged: HashMap<Uuid, u32> = HashMap::new();
        let listed = [bound(TENANT_A)];

        prune_wedged(&mut wedged, Some(&listed));
        report_reconcile_outcome(bound(TENANT_A), &wedged_outcome(), &mut wedged);
        prune_wedged(&mut wedged, Some(&listed));
        report_reconcile_outcome(bound(TENANT_A), &wedged_outcome(), &mut wedged);
        assert_eq!(wedged.get(&TENANT_A), Some(&2), "two wedged passes");

        // The pass whose enumeration was refused. It prunes nothing and reports
        // nothing.
        prune_wedged(&mut wedged, None);
        assert_eq!(
            wedged.get(&TENANT_A),
            Some(&2),
            "a refused enumeration is not evidence that the tenant recovered or departed"
        );

        prune_wedged(&mut wedged, Some(&listed));
        report_reconcile_outcome(bound(TENANT_A), &wedged_outcome(), &mut wedged);
        assert_eq!(
            wedged.get(&TENANT_A),
            Some(&WEDGED_PASSES_BEFORE_ERROR),
            "the third genuinely-wedged pass must reach the ERROR threshold"
        );
    }

    /// The other half of `prune_wedged`, so the fix above is not just "never
    /// prune": a tenant the directory really has stopped listing is dropped, and
    /// one it still lists is kept.
    #[test]
    fn a_tenant_the_directory_no_longer_lists_is_pruned() {
        let mut wedged: HashMap<Uuid, u32> = HashMap::new();
        report_reconcile_outcome(bound(TENANT_A), &wedged_outcome(), &mut wedged);
        report_reconcile_outcome(bound(TENANT_B), &wedged_outcome(), &mut wedged);

        prune_wedged(&mut wedged, Some(&[bound(TENANT_B)]));

        assert_eq!(wedged.get(&TENANT_A), None, "no longer listed, so dropped");
        assert_eq!(wedged.get(&TENANT_B), Some(&1), "still listed, so kept");
    }

    /// The cadence `serve` receives for the collect cycle is the **floored** one.
    ///
    /// The mutation this catches is reading `cfg.collect_interval_seconds`
    /// directly instead of the accessor: it compiles, differs only for a
    /// sub-floor configuration, and would also make the serve loop re-emit the
    /// floor warning on every tick. Same shape as qa-runs'
    /// `the_resolved_cadence_is_clamped_once_not_configured`.
    #[test]
    fn the_collect_cadence_is_floored_once_not_configured() {
        let cfg = QaInsightsConfig {
            collect_interval_seconds: 1,
            ..QaInsightsConfig::default()
        };

        let cadence = Cadence::collect(&cfg);
        assert!(cadence.runs);
        assert_eq!(
            cadence.interval_seconds, MIN_COLLECT_INTERVAL_SECONDS,
            "the raw 1 must never reach serve"
        );
    }
}
