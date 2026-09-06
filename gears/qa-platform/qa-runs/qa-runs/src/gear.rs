//! Composition root: `#[toolkit::gear]` bootstrap, `Gear::init`, the
//! `DatabaseCapability`/`RestApiCapability` implementations, and the stateful
//! lifecycle entry (`serve`) hosting the two tickers — the dispatcher and the
//! schedule firing pass.

use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use sea_orm_migration::MigrationTrait;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
#[cfg(feature = "argo")]
use tracing::warn;
use tracing::{debug, error, info};

use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use qa_catalog_sdk::QaCatalogClientV1;
use qa_environments_sdk::QaEnvironmentsClientV1;
use qa_runs_sdk::QaRunsClientV1;

use crate::api::rest::dto::BoundaryLimits;
use crate::api::rest::routes;
use crate::config::{ArgoExecutorConfig, ExecutorKind, QaRunsConfig};
use crate::domain::error::DomainError;
use crate::domain::local_client::QaRunsLocalClient;
use crate::domain::ports::run_executor::RunExecutor;
use crate::domain::service::{AppServices, LogArchive, LogFanout, QueueLimits, ServiceDeps};
use crate::infra::ConcreteAppServices;
use crate::infra::executor::mock::MockRunExecutor;
use crate::infra::leader::{LeaderElector, elector, work_fn};
use crate::infra::logs::{RunLogArchive, RunLogBroadcaster};
use crate::infra::product_plugin::HubProductPluginResolver;
use crate::infra::storage::{OrmQueueRepository, OrmRunsRepository, OrmSchedulesRepository};

/// The role name the dispatcher ticker holds.
///
/// One string, used by the elector and echoed in the ticker's log lines, so an
/// operator grepping for a stuck dispatcher finds both.
const DISPATCHER_ROLE: &str = "qa-runs-dispatcher";

/// The role name the schedule ticker holds.
///
/// **A second role, not a share of the dispatcher's.** The two tickers are
/// switched on and off independently by configuration, so a single role would
/// make "run the scheduler here but not the dispatcher" unexpressible to any
/// real elector, and a leadership term for one would silently gate the other.
const SCHEDULER_ROLE: &str = "qa-runs-scheduler";

/// The one log broadcaster, in the two shapes its two consumers need.
///
/// # Why this is a type rather than two `Arc::clone`s
///
/// `ServiceDeps::logs` takes `Arc<dyn LogFanout>` - the publishing port - and
/// the router takes the concrete `Arc<RunLogBroadcaster>`, because a
/// subscription hands back `infra::logs::LogSubscription` and the domain port
/// correctly refuses to name an infrastructure type. Two parameters, two types,
/// and **a second `RunLogBroadcaster::new(..)` for one of them type-checks
/// perfectly**. What it produces is a gear that boots clean, answers 200 on
/// `/logs`, and streams nothing ever - no error, no warning, every subscriber
/// waiting on a channel map nothing publishes into.
///
/// Mutating `init` to allocate a second one left all tests green. A test cannot
/// reach that without a database and four cross-gear clients, so the two views
/// are derived from one constructor here instead: one allocation, two
/// accessors, and [`tests::the_log_wiring_hands_out_two_views_of_one_broadcaster`]
/// pins that they really are one.
///
/// # It is not unspellable, and this used to claim it was
///
/// *"There is no way to spell the mistake"* shipped here and is false.
/// `LogWiring` makes the mistake unspellable **only inside `LogWiring`**;
/// nothing obliges `init` to use it. Break-tested rather than argued —
/// replacing `logs: logs.subscriber()` in [`QaRuns::init`] with a fresh
/// `Arc::new(RunLogBroadcaster::new(cfg.log_buffer_lines))` compiles clean and
/// leaves the whole suite passing, producing exactly the gear this type was
/// introduced to prevent: boots without a warning, answers 200 on `/logs`, and
/// streams nothing ever. Six other sites call `RunLogBroadcaster::new`
/// directly, so the constructor cannot be hidden either.
///
/// Closing it in the type system would need the runtime's field to be a handle
/// only `LogWiring` can mint, in a module `gear` cannot construct it from — and
/// even then two `LogWiring`s would still type-check, because what has to hold
/// is that *one value reaches both places*, which is a property of this
/// function body and not of any signature.
///
/// So it is a **tripwire, not a proof**:
/// [`tests::init_builds_exactly_one_log_broadcaster`] reads `init`'s own source
/// and fails if the constructor appears in it, which is the shape
/// `domain::service::tenant_scoping_tests` and `domain::system_actor` already
/// use for rules the compiler cannot state. A reader should take the guarantee
/// as "the obvious spelling is caught", and no further.
struct LogWiring(Arc<RunLogBroadcaster>);

impl LogWiring {
    fn new(capacity: usize) -> Self {
        Self(Arc::new(RunLogBroadcaster::new(capacity)))
    }

    /// The publishing half, for `ServiceDeps`.
    fn publisher(&self) -> Arc<dyn LogFanout> {
        Arc::clone(&self.0) as Arc<dyn LogFanout>
    }

    /// The subscribing half, for the SSE route.
    fn subscriber(&self) -> Arc<RunLogBroadcaster> {
        Arc::clone(&self.0)
    }
}

/// One ticker's cadence, resolved from configuration **once**.
///
/// Named constructors rather than expressions inline in `init`, so the one thing
/// `gear.rs` has to get right about the cadence knobs is testable without a
/// database, a `ClientHub` and four resolved cross-gear clients. What they pin:
/// that the value stored is the *clamped* one, and that each reads its own knob
/// and its own floor.
///
/// **The disable rule is re-derived here, and that is deliberate** — an earlier
/// wording claimed it was "folded by the config module rather than re-derived",
/// which the two bodies below plainly contradict. Each inlines
/// `enabled && interval != 0` so the clamping accessor is called once rather
/// than twice; [`Cadence::dispatcher`] carries the measurement. The cost is a
/// second home for the precedence rule, and
/// `tests::each_cadence_agrees_with_the_config_predicate_it_inlines` is what
/// keeps the two from drifting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cadence {
    runs: bool,
    interval_seconds: u64,
}

impl Cadence {
    /// Read the clamping accessor **once**, and derive the rest from the
    /// result.
    ///
    /// `QaRunsConfig::dispatcher_runs` calls
    /// `effective_dispatcher_interval_seconds` itself, so asking for both meant
    /// two calls and two identical clamp WARNs in the boot log - measured with
    /// `dispatcher_interval_seconds: 1`. Not per-tick noise, but a duplicated
    /// line is a reader wondering which of two things clamped. The `runs`
    /// predicate is inlined here rather than calling the accessor, which is the
    /// only way to have it once.
    fn dispatcher(cfg: &QaRunsConfig) -> Self {
        let interval_seconds = cfg.effective_dispatcher_interval_seconds();
        Self {
            // `QaRunsConfig::dispatcher_runs`'s rule, over the already-clamped
            // value: `enabled` is decisive, and `0` disables either way.
            runs: cfg.dispatcher_enabled && interval_seconds != 0,
            interval_seconds,
        }
    }

    /// The same, for the schedule ticker's own pair of knobs.
    ///
    /// A second constructor rather than a parameterised one: what differs is
    /// *which* accessor to call, and an argument selecting between two accessors
    /// is the transposition this shape exists to prevent — the two knobs are
    /// both `u64` seconds with different floors, so reading the dispatcher's
    /// here would compile and would silently give the scheduler a five-second
    /// cadence.
    fn scheduler(cfg: &QaRunsConfig) -> Self {
        let interval_seconds = cfg.effective_schedule_interval_seconds();
        Self {
            runs: cfg.scheduler_enabled && interval_seconds != 0,
            interval_seconds,
        }
    }
}

/// Everything `serve` needs beyond the gear struct: the wired services, the
/// elector, and the **already-clamped** cadence.
///
/// The cadence is stored, not recomputed. `QaRunsConfig`'s `effective_*`
/// accessors warn when they clamp, so calling one inside the serve loop would
/// emit a WARN every tick - which is the exact noise failure `dispatcher_enabled`
/// exists to prevent. Reading each once at init and keeping the result is the
/// caller obligation those accessors document; this struct is where it is
/// discharged.
///
/// **Not every accessor divides that way.** `effective_max_timeout_seconds` is
/// a *per-request* boundary read and is deliberately absent here: caching it
/// would be the same mistake in the other direction, freezing a ceiling that
/// the REST layer is supposed to consult per launch. It travels as
/// [`BoundaryLimits`], layered on the router.
struct QaRunsRuntime {
    services: Arc<ConcreteAppServices>,
    /// The same `Arc` `ServiceDeps::logs` holds - see [`QaRuns::init`].
    logs: Arc<RunLogBroadcaster>,
    /// The same `Arc` `ServiceDeps::archive` holds - see [`QaRuns::init`]. The
    /// dispatcher ticker drains it; `IngestService` fills it. One instance,
    /// two consumers, same reasoning as [`Self::logs`].
    archive: Arc<dyn LogArchive>,
    limits: BoundaryLimits,
    elector: Arc<dyn LeaderElector>,
    /// Both already clamped and both folded, so `serve` asks the config nothing.
    dispatcher: Cadence,
    scheduler: Cadence,
    /// The gear's own shutdown signal, handed to `AppServices::new` (via
    /// `ServiceDeps::cancel`) so the production result watcher can be built
    /// with it — see `domain::service::watch::SpawningRunWatcher`'s header on
    /// why the observer needs a third lifetime beside the caller's and the
    /// run's.
    ///
    /// **Created here, in `init`, rather than derived from `serve`'s own
    /// `cancel` parameter — the two cannot be the same token.** `init` runs
    /// before `serve`, and the watcher is built during `init`
    /// (`AppServices::new`, called from here), so whatever token it holds has
    /// to exist before `serve`'s framework-supplied `CancellationToken` does.
    /// `serve` bridges the two: it forwards its own `cancel` into this one the
    /// first time it runs, which is the only place the framework's signal and
    /// this stored one can be joined.
    shutdown: CancellationToken,
}

/// Main gear struct.
///
/// No `await_ready`: the dispatcher must not gate request traffic, so `serve`
/// omits the `ReadySignal` argument (mirrors qa-catalog and bss-ledger).
/// # `deps` is initialisation order, not just linkage
///
/// `qa_catalog` and `qa_environments` are declared because [`Self::init`]
/// resolves their clients from `ClientHub`, and `ClientHub::get` finds only
/// what another gear has **already registered**. `deps` is what places those
/// gears earlier in the topological order.
///
/// This was measured rather than reasoned: with `deps = [authz_resolver]`
/// alone the boot log ordered qa-runs before qa-catalog and init failed with
/// `client not found: type=dyn qa_catalog_sdk::client::QaCatalogClientV1`. The
/// plan had settled on the shorter list, correctly observing that cross-gear
/// clients come from `ClientHub` whether or not a token is declared - which is
/// true, and does not address ordering.
///
/// Each token costs a Cargo dependency on the **gear crate** (not its SDK) and
/// an entry in `[workspace.metadata.cargo-shear] ignored`; see this crate's
/// `Cargo.toml`.
#[toolkit::gear(
    name = "qa-runs",
    deps = [authz_resolver, qa_catalog, qa_environments],
    capabilities = [db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s")
)]
pub struct QaRuns {
    runtime: OnceLock<Arc<QaRunsRuntime>>,
}

impl Default for QaRuns {
    fn default() -> Self {
        Self {
            runtime: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for QaRuns {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: QaRunsConfig = ctx.config_or_default()?;
        debug!(
            dispatcher_enabled = cfg.dispatcher_enabled,
            dispatcher_interval_seconds = cfg.dispatcher_interval_seconds,
            scheduler_enabled = cfg.scheduler_enabled,
            schedule_interval_seconds = cfg.schedule_interval_seconds,
            orphan_timeout_seconds = cfg.orphan_timeout_seconds,
            queue_ttl_seconds = cfg.queue_ttl_seconds,
            queue_max_depth = cfg.queue_max_depth,
            max_concurrent_runs = cfg.max_concurrent_runs,
            default_timeout_seconds = cfg.default_timeout_seconds,
            max_timeout_seconds = cfg.max_timeout_seconds,
            log_buffer_lines = cfg.log_buffer_lines,
            executor = ?cfg.executor,
            "loaded qa-runs config"
        );

        // Re-parameterized with `DomainError` so transaction closures preserve
        // domain variants - see `domain::service::DbProvider`.
        let db = Arc::new(DBProvider::<DomainError>::new(ctx.db_required()?.db()));

        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;
        let catalog = ctx
            .client_hub()
            .get::<dyn QaCatalogClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get qa-catalog client: {e}"))?;
        let environments = ctx
            .client_hub()
            .get::<dyn QaEnvironmentsClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get qa-environments client: {e}"))?;

        // Resolved once, here, and stored on the runtime - never re-read in
        // the serve loop, where the clamp warning would repeat every tick.
        let dispatcher = Cadence::dispatcher(&cfg);
        let scheduler = Cadence::scheduler(&cfg);

        let logs = LogWiring::new(cfg.log_buffer_lines);
        let executor: Arc<dyn RunExecutor> = match cfg.executor {
            ExecutorKind::Mock => {
                info!(
                    executor = "mock",
                    "wiring the deterministic in-memory executor; every run will \
                     report one fabricated passing test"
                );
                Arc::new(MockRunExecutor::new())
            }
            ExecutorKind::Argo => Self::argo_executor(&cfg.argo).await?,
        };

        // Built once, here, and shared by `ServiceDeps::archive` (for
        // `IngestService`) and `QaRunsRuntime::archive` (for the dispatcher
        // tick's `flush_due`) below - allocating a second `RunLogArchive` for
        // either consumer would type-check and silently archive nothing, the
        // same hazard `LogWiring`'s own doc describes for the broadcaster;
        // see `tests::init_constructs_exactly_one_archive`.
        // `policy_enforcer` here is a second `PolicyEnforcer` instance from
        // the one `AppServices::new` builds internally from `deps.authz`,
        // not the *same* instance - fine, because `PolicyEnforcer` is a
        // stateless wrapper around the `authz` client it is handed, and two
        // instances built from the same client are interchangeable.
        let policy_enforcer = PolicyEnforcer::new(Arc::clone(&authz));
        let runs_repo = Arc::new(OrmRunsRepository);
        let archive: Arc<dyn LogArchive> = Arc::new(RunLogArchive::new(
            Arc::clone(&db),
            Arc::clone(&runs_repo),
            policy_enforcer,
        ));

        // Minted here rather than borrowed from `serve`'s own token, because
        // `serve` (and the framework-supplied `CancellationToken` it receives)
        // does not exist yet at this point in the gear's lifecycle - see
        // `QaRunsRuntime::shutdown`'s doc for how `serve` bridges the two.
        let shutdown = CancellationToken::new();

        let services = Arc::new(AppServices::new(
            runs_repo,
            Arc::new(OrmQueueRepository),
            Arc::new(OrmSchedulesRepository),
            ServiceDeps {
                db,
                authz,
                catalog,
                environments,
                // Lazy, per dispatch: an eager hub lookup here would make this
                // gear fail to BOOT in a deployment whose qa-catalog
                // initialises later or is absent, where the honest failure is
                // one run's dispatch saying so. See
                // `infra::product_plugin`'s header and ruling D-14.
                product_plugins: Arc::new(HubProductPluginResolver::new(ctx.client_hub())),
                executor,
                logs: logs.publisher(),
                archive: Arc::clone(&archive),
                // `None` wires the real admitter and the real dispatcher: the
                // overrides exist for the launch tests, and production must
                // have exactly one of each so they share a lock registry.
                admitter: None,
                dispatcher: None,
                // `None` wires the real watcher over the same `IngestService`
                // this container builds - the only place both halves exist.
                watcher: None,
                cancel: shutdown.clone(),
                default_timeout_seconds: cfg.default_timeout_seconds,
                limits: QueueLimits {
                    queue_max_depth: cfg.queue_max_depth,
                    max_concurrent_runs: cfg.max_concurrent_runs,
                    queue_ttl_seconds: cfg.queue_ttl_seconds,
                },
                orphan_timeout_seconds: cfg.effective_orphan_timeout_seconds(),
            },
        ));

        self.runtime
            .set(Arc::new(QaRunsRuntime {
                services: Arc::clone(&services),
                logs: logs.subscriber(),
                archive,
                limits: BoundaryLimits::from(&cfg),
                elector: elector(),
                dispatcher,
                scheduler,
                shutdown,
            }))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Registered under the SDK trait for transport-agnostic consumption by
        // other gears; qa-insights' auto-rerun is the named consumer.
        ctx.client_hub()
            .register::<dyn QaRunsClientV1>(Arc::new(QaRunsLocalClient::new(services)));

        Ok(())
    }
}

impl QaRuns {
    /// The Argo executor, when this binary was built with the `argo` feature.
    ///
    /// # Errors
    /// When the API server cannot be reached, the `argoproj.io` CRDs are absent,
    /// this process' credentials do not allow listing `Workflow`, or
    /// `qa-runs.argo.runner_image` is unset. All four are boot failures on
    /// purpose: an executor that constructs happily and fails on first dispatch
    /// turns a misconfiguration into a failed run hours later.
    #[cfg(feature = "argo")]
    async fn argo_executor(cfg: &ArgoExecutorConfig) -> anyhow::Result<Arc<dyn RunExecutor>> {
        warn!(
            executor = "argo",
            namespace = %cfg.namespace,
            runner_image = %cfg.runner_image,
            "wiring the Argo Workflows executor: this deployment has a Kubernetes \
             dependency, which ADR-0001 removes from the product and its \
             2026-08-27 waiver permits only behind this non-default feature"
        );
        Ok(Arc::new(
            crate::infra::executor::argo::ArgoRunExecutor::connect(cfg.clone()).await?,
        ))
    }

    /// The same selection in a build **without** the `argo` feature: a hard
    /// failure.
    ///
    /// **Not a fall back to the mock.** A deployment that asked for real tests
    /// and silently got `test_mock_default` would report fabricated passes,
    /// which is the failure the adapter exists to end — and it would be
    /// indistinguishable, from the outside, from tests that genuinely passed.
    ///
    /// # Errors
    /// Always.
    #[cfg(not(feature = "argo"))]
    #[allow(clippy::unused_async)]
    async fn argo_executor(_cfg: &ArgoExecutorConfig) -> anyhow::Result<Arc<dyn RunExecutor>> {
        anyhow::bail!(
            "qa-runs.executor is `argo`, but this binary was built without the \
             `argo` cargo feature. Rebuild with `--features argo`, or set \
             `qa-runs.executor: mock`. Falling back to the mock is deliberately \
             not offered: it would report fabricated passing tests."
        )
    }
}

impl DatabaseCapability for QaRuns {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing qa-runs database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for QaRuns {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering qa-runs REST routes");

        let runtime = self
            .runtime
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?;

        let router = routes::register_routes(
            router,
            openapi,
            Arc::clone(&runtime.services),
            Arc::clone(&runtime.logs),
            runtime.limits,
        );

        info!("qa-runs REST routes registered successfully");
        Ok(router)
    }
}

// ---------------------------------------------------------------------------
// Lifecycle (stateful capability): the dispatcher and scheduler tickers
// ---------------------------------------------------------------------------

impl QaRuns {
    /// Lifecycle entry (`stateful` capability). Hosts the tickers this
    /// deployment has switched on, under a child of `cancel`.
    ///
    /// # Errors
    ///
    /// Returns `Err` only when a ticker exits before it was cancelled — whether
    /// it panicked, was aborted, or returned of its own accord. Cooperative
    /// shutdown returns `Ok(())`. The distinction is the point: a ticker that
    /// has stopped and a gear that is idle look identical from outside, and one
    /// of them means every queued run has stopped starting.
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the `tracing` macros in each early return: the ways this \
                  function declines to start a ticker - no runtime, a disabled dispatcher, a \
                  disabled scheduler - each owe an operator a distinct line saying which, and \
                  the metric counts every expansion as a branch. Same diagnosis as \
                  `service::dispatch`'s `reconcile_claims`"
    )]
    pub(crate) async fn serve(self: Arc<Self>, cancel: CancellationToken) -> anyhow::Result<()> {
        let Some(rt) = self.runtime.get().cloned() else {
            info!("qa-runs: serve() with no runtime (uninitialized); idling until cancelled");
            cancel.cancelled().await;
            return Ok(());
        };

        // A child token so the supervisor can stop the tickers on its own
        // terms, and so a shutdown of the runtime reaches them either way.
        let tasks = cancel.child_token();

        // Bridge into the token the result watcher was already built with,
        // back in `init` - see `QaRunsRuntime::shutdown`'s doc for why the two
        // cannot be one token from the start. Bridged from `tasks`, not
        // `cancel`, and that distinction is load-bearing: `tasks` is what
        // `supervise` cancels unconditionally, on *both* of its exit paths —
        // a cooperative shutdown (where `cancel` firing cancels `tasks` too,
        // since it is `cancel`'s child) and a ticker panicking or exiting
        // before `cancel` ever fires (where `supervise` cancels `tasks`
        // directly and `cancel` stays live). Bridging from `cancel` instead
        // would leave the watcher's token — and so `rt.services.shutdown()`
        // below — waiting forever on exactly the premature-failure path this
        // function's own doc says it returns `Err` for. This has to be a
        // spawned task rather than an inline `select!` arm because nothing
        // else here polls `tasks` again after handing it to the tickers.
        tokio::spawn({
            let shutdown = rt.shutdown.clone();
            let tasks = tasks.clone();
            async move {
                tasks.cancelled().await;
                shutdown.cancel();
            }
        });
        let mut tickers = Tickers::default();

        if rt.dispatcher.runs {
            let role = tickers.spawn(Self::dispatcher_ticker(&rt, &tasks));
            info!(
                interval_seconds = rt.dispatcher.interval_seconds,
                role, "qa-runs dispatcher ticker started"
            );
        } else {
            info!(
                dispatcher_interval_seconds = rt.dispatcher.interval_seconds,
                // Both halves, because the second is the one an operator does
                // not expect: the tick is also what attaches a result observer
                // to every live run (`DispatchService::reattach_watchers`), so
                // with no ticker a run still launches and still dispatches
                // inline, receives no results at all, and can only ever end at
                // its deadline.
                "qa-runs: dispatcher disabled; queued runs will not start on their own, and \
                 no run's results are ingested - every run ends at its timeout"
            );
        }

        if rt.scheduler.runs {
            let role = tickers.spawn(Self::scheduler_ticker(&rt, &tasks));
            info!(
                interval_seconds = rt.scheduler.interval_seconds,
                role, "qa-runs schedule ticker started"
            );
        } else {
            info!(
                schedule_interval_seconds = rt.scheduler.interval_seconds,
                "qa-runs: scheduler disabled; no schedule fires on this replica. Stored \
                 schedules are untouched and their fired-through cursors do not advance, so \
                 the occurrences missed while it is off are skipped rather than caught up"
            );
        }

        if tickers.is_empty() {
            info!("qa-runs: no lifecycle ticker is enabled; idling until cancelled");
            cancel.cancelled().await;
            // No ticker ever called `attach` on this replica (only
            // `DispatchService::reattach_watchers`, driven by the dispatcher
            // ticker, does), so there is nothing for this to wait on - but
            // calling it uniformly, rather than only on the path below, means
            // "does shutdown wait for observers" has one answer instead of two.
            rt.services.shutdown().await;
            return Ok(());
        }

        let result = supervise(cancel, &tasks, tickers).await;
        // Awaited once the tickers have stopped or `supervise` has given up on
        // them, not raced against them - `reattach_watchers` only runs from
        // inside the dispatcher tick, and `tasks` (which gates it) is
        // cancelled before this line runs either way.
        //
        // **Not a claim that every observer has necessarily been spawned by
        // now**, and an earlier version of this comment said so. On the
        // premature-failure path - one ticker panics or exits before `cancel`
        // ever fires - `supervise` cancels `tasks` and returns `Err` without
        // waiting for a *surviving* ticker's in-flight tick to notice and
        // stop (`supervise`'s own body: it returns as soon as the first
        // ticker joins, rather than looping `tickers.join_next()` on that
        // path). A tick already past its own cancellation check when that
        // happens could still call `attach` after this line has started
        // running. That is exactly the race `WatchRegistry::shutdown`'s doc
        // already names and narrows rather than closes: `tasks` (and so the
        // watcher's own token, bridged above) is already cancelled by the
        // time any such attach could happen, so the new observer's
        // `tokio::select!` sees it on the first poll and ends immediately
        // rather than doing any work.
        //
        // Bounded by the framework's own `stop_timeout` racing this whole
        // function either way, not by anything in this method - see
        // `AppServices::shutdown`'s doc for what else can make it take a
        // while regardless.
        rt.services.shutdown().await;
        result
    }

    /// Spawn the leader-gated dispatcher ticker.
    ///
    /// # Boot recovery runs inside the leader gate, and that is not decoration
    ///
    /// `recover_after_boot` fails every claim it finds in `dispatching` with no
    /// execution reference, retires those runs and releases their leases -
    /// correct for rows *this cluster* left mid-submit, and destructive for
    /// rows another replica is mid-submit on right now. Running it per process
    /// would make each starting replica sabotage every other replica's
    /// in-flight launches. Inside `run_role` it runs once per leadership term,
    /// which is the closest this seam gets to once per cluster.
    ///
    /// With [`crate::infra::leader::NoopLeaderElector`] - the only
    /// implementation shipped - "once per leadership term" is "once per
    /// process", so on two replicas the hazard is live. That is a property of
    /// the elector, not of this wiring, and it is why the elector is a seam.
    ///
    /// Returns its role beside its loop, and hands that **same value** to the
    /// elector below — see [`Tickers`] for why the role is not a parameter of
    /// `spawn`.
    fn dispatcher_ticker(
        rt: &Arc<QaRunsRuntime>,
        tasks: &CancellationToken,
    ) -> Ticker<impl Future<Output = ()> + Send + 'static> {
        // Bound once, so the value the elector holds leadership under and the
        // value `Tickers` files this task under are the *same* one rather than
        // two spellings that could drift.
        let role = DISPATCHER_ROLE;
        let elector = Arc::clone(&rt.elector);
        let services = Arc::clone(&rt.services);
        let archive = Arc::clone(&rt.archive);
        let period = Duration::from_secs(rt.dispatcher.interval_seconds);
        let cancel = tasks.clone();

        let run = async move {
            let work = work_fn(move |cancel| {
                let services = Arc::clone(&services);
                let archive = Arc::clone(&archive);
                async move {
                    let boot = services.dispatch.recover_after_boot().await;
                    info!(
                        failed_orphans = boot.failed_orphans,
                        denied_passes = ?boot.denied_passes,
                        "qa-runs boot claim recovery complete"
                    );

                    let mut ticker = tokio::time::interval(period);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        tokio::select! {
                            () = cancel.cancelled() => {
                                info!("qa-runs dispatcher stopping (leadership lost or shutdown)");
                                return Ok(());
                            }
                            _ = ticker.tick() => {
                                let report = services.dispatch.run_tick().await;
                                debug!(
                                    expired = report.expired,
                                    timed_out = report.timed_out,
                                    released = report.released,
                                    failed_orphans = report.failed_orphans,
                                    claimed = report.claimed,
                                    attached = report.attached,
                                    stopped_at = ?report.stopped_at,
                                    "qa-runs dispatcher tick"
                                );

                                // The archive is flushed by the tick because
                                // the process that ingests lines is the process
                                // that ticks: the log publisher is the
                                // `service::watch` observer this tick starts.
                                // This is also what covers the terminal paths
                                // `IngestService::finish` does not — cancel,
                                // TTL expiry, control-plane timeout, orphan
                                // recovery, and a refused launch's abandon.
                                let flushed = archive.flush_due().await;
                                if flushed.runs > 0 || flushed.failed > 0 {
                                    debug!(
                                        runs = flushed.runs,
                                        lines = flushed.lines,
                                        failed = flushed.failed,
                                        "qa-runs log archive flush",
                                    );
                                }
                            }
                        }
                    }
                }
            });

            if let Err(error) = elector.run_role(role, cancel, work).await {
                error!(%error, "qa-runs dispatcher exited with an error");
            }
        };
        Ticker { role, run }
    }

    /// Spawn the leader-gated schedule firing ticker.
    ///
    /// # Leadership here is defence in depth, and the claim is the guarantee
    ///
    /// `cpt-cf-qa-nfr-scheduler-exactly-once` is discharged by
    /// `idx_qa_schedule_ticks_claim`, not by this gate:
    /// `SchedulesRepository::claim_tick` is an `INSERT` against a unique index,
    /// so **the guarantee holds with every replica evaluating every schedule**.
    /// That ordering is not a footnote — it is what makes the NFR's stated
    /// verification method ("integration tests with multiple concurrent
    /// scheduler instances", PRD §5.2) something a test can actually do:
    /// `domain::service::schedules_tests` drives two services against one store
    /// with no elector in sight, and exactly one run comes out.
    ///
    /// What the gate buys is the same thing it buys the dispatcher — one replica
    /// doing the work instead of N, so the fleet-wide enumeration
    /// (`SchedulesRepository::list_enabled`, uncapped by design) runs once per
    /// interval rather than once per replica, and the losers' DEBUG lines do not
    /// fill the log. Under the shipped
    /// [`crate::infra::leader::NoopLeaderElector`] every replica is the leader,
    /// so today it buys nothing at all and the claim is carrying the whole
    /// property. That is the intended arrangement, not a gap.
    ///
    /// **There is deliberately no boot recovery pass here**, unlike the
    /// dispatcher's. A claim orphaned by a process that died mid-fire is not
    /// reclaimable and must not be reclaimed: the run may have been created. It
    /// self-heals instead, at the next occurrence, because `domain::cron`'s
    /// `next_due` answers with the most recent occurrence and never an
    /// outstanding older one.
    fn scheduler_ticker(
        rt: &Arc<QaRunsRuntime>,
        tasks: &CancellationToken,
    ) -> Ticker<impl Future<Output = ()> + Send + 'static> {
        // One binding, for the reason `dispatcher_ticker` gives.
        let role = SCHEDULER_ROLE;
        let elector = Arc::clone(&rt.elector);
        let services = Arc::clone(&rt.services);
        let period = Duration::from_secs(rt.scheduler.interval_seconds);
        let cancel = tasks.clone();

        let run = async move {
            let work = work_fn(move |cancel| {
                let services = Arc::clone(&services);
                async move {
                    let mut ticker = tokio::time::interval(period);
                    // `Delay`, matching the dispatcher: a tick missed because
                    // the previous pass ran long must not be made up in a burst
                    // that re-enumerates the fleet several times over.
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        tokio::select! {
                            () = cancel.cancelled() => {
                                info!("qa-runs scheduler stopping (leadership lost or shutdown)");
                                return Ok(());
                            }
                            _ = ticker.tick() => {
                                let report = services.schedules.fire_due_schedules().await;
                                debug!(
                                    evaluated = report.evaluated,
                                    fired = report.fired,
                                    lost = report.lost,
                                    failed = report.failed,
                                    "qa-runs schedule tick"
                                );
                            }
                        }
                    }
                }
            });

            if let Err(error) = elector.run_role(role, cancel, work).await {
                error!(%error, "qa-runs scheduler exited with an error");
            }
        };
        Ticker { role, run }
    }
}

/// A ticker that is ready to spawn: the role it holds, and the loop that holds
/// it.
///
/// **The role travels *with* the loop rather than being supplied beside it**,
/// which is what keeps the two from being paired up wrongly. See [`Tickers`].
struct Ticker<F> {
    role: &'static str,
    run: F,
}

/// The spawned tickers, and which role each spawned task is.
///
/// # The role has to come from the task id, not from the future's output
///
/// The first version of this made each ticker future resolve to its own role
/// name and read it off the join result. That names the role on the `Ok` arm
/// and **loses it on the `Err` arm**, which is the panic — the one case the
/// whole shared supervisor exists for. Measured: one healthy ticker plus one
/// panicking one produced *"a qa-runs ticker panicked or was aborted: task 2
/// panicked with message \"boom\""*, and an operator watching half a gear die
/// was told a task number.
///
/// `JoinError` carries `id()`, and [`tokio::task::JoinSet::spawn`] hands back an
/// `AbortHandle` that carries the same id, so recording the pair at spawn is
/// what makes the role available on **both** arms.
///
/// # And the role is not spelled at the spawn site, which is the second attempt
///
/// The fix above took the role as a second argument to `spawn`, beside the
/// future — and claimed in this doc that the map was then "the single home for
/// the name". **It was not.** Each ticker also spells its own role for
/// `LeaderElector::run_role` inside its body, so the two lived at different
/// sites, and swapping only the two names at the spawn calls compiled and left
/// the whole suite green: the gear would hold the dispatcher's leadership lease
/// while reporting that the scheduler had panicked. The previous design could
/// not express that, because the role came from inside the ticker body — the
/// panic fix traded one defect for another, and `serve` is untested by design,
/// so nothing would ever have caught it.
///
/// [`Ticker`] is what closes both at once: each constructor binds its role
/// once, hands that binding to `run_role`, and returns it alongside its loop —
/// so the elector, the map and the started-log line are literally one value,
/// and **`spawn` has no role parameter to get wrong**. Each role constant is
/// read at exactly one place in this file, which is the `let role = …` in its
/// own constructor. Verified by attempting the transposition that used to
/// compile: it is now `E0061`, because the argument it swapped no longer
/// exists.
///
/// What that does *not* prevent, stated so this paragraph does not become the
/// fifth over-claim: a branch of `serve` can still call the wrong
/// constructor outright. That mislabels consistently rather than producing two
/// disagreeing names, and it is visible at the call site as a wrong function
/// name.
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

    /// The next ticker to finish, with the role it was, or `None` when the set
    /// is empty.
    ///
    /// Resolving the role here rather than in [`supervise`] is what keeps
    /// [`Self::roles`] and [`Self::role_of`]'s invariant inside one type: the
    /// supervisor never touches [`Self::set`], so it has no way to add a task
    /// the map does not know about.
    async fn join_next(&mut self) -> Option<(&'static str, Result<(), tokio::task::JoinError>)> {
        match self.set.join_next_with_id().await {
            None => None,
            Some(Ok((id, ()))) => Some((self.role_of(id), Ok(()))),
            Some(Err(error)) => Some((self.role_of(error.id()), Err(error))),
        }
    }

    /// The role that task is, or a placeholder.
    ///
    /// **No path in this module reaches the placeholder today, because
    /// [`Self::spawn`] is the only thing that inserts into [`Self::set`] and it
    /// records the pair.** That is an invariant of this type rather than a
    /// property of the id, so it is stated as such — a `self.set.spawn(..)`
    /// written anywhere in this file would break it silently.
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
/// operator and all three are reported. (An earlier version of this paragraph
/// said "a panic or an abort", omitting the third; the code always handled it.)
///
/// **One supervisor over a set, rather than one per ticker.** A gear whose
/// scheduler has panicked while its dispatcher runs on is exactly as broken as
/// one with no dispatcher, and half as visible; the set is what makes the first
/// exit — whichever it is — the thing that ends `serve`. [`Tickers`] is what
/// makes the error name the one that stopped, on every arm; this function never
/// touches the [`JoinSet`] itself, so it cannot add a task the role map does not
/// know about.
///
/// An empty set makes this return `Ok(())` at once rather than waiting for
/// cancellation, which is why [`QaRuns::serve`] handles "nothing enabled"
/// itself instead of passing an empty set here.
#[allow(
    clippy::cognitive_complexity,
    reason = "one log line per shutdown path, and the paths are the point: drained, \
              join-failed, and exited-early are three different operator stories"
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
                Ok(()) => debug!(role, "qa-runs ticker drained"),
                Err(e) => error!(
                    role,
                    error = %e,
                    "qa-runs ticker join failed during shutdown",
                ),
            }
        }
        return Ok(());
    };

    match outcome {
        Ok(()) => Err(anyhow::anyhow!("qa-runs {role} task exited unexpectedly")),
        Err(join_err) => Err(anyhow::anyhow!(
            "qa-runs {role} task panicked or was aborted: {join_err}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::QaRuns;
    use crate::infra::logs::RunLogBroadcaster;
    use std::sync::Arc;

    /// **The SSE endpoint and the publishers must share one broadcaster.**
    ///
    /// `ServiceDeps::logs` takes it as `dyn LogFanout` and the router takes the
    /// concrete type, so the two are separate parameters and a second
    /// `RunLogBroadcaster::new(..)` in `init` would type-check perfectly. What
    /// it would produce is a gear where `IngestService` publishes into one
    /// channel map and every SSE subscriber waits on another - no error, no
    /// warning, just a log stream that is permanently silent.
    ///
    /// Driven through the broadcaster's own identity rather than through `init`,
    /// which needs a database, a `ClientHub` and four resolved cross-gear
    /// clients. **What that leaves unproven is the thing itself**: this shows
    /// that publishing through a `dyn LogFanout` handle reaches a subscriber
    /// taken from the same `Arc`, and that it does *not* reach one taken from a
    /// different `Arc`. That `init` passes the same `Arc` to both is verified by
    /// reading it - `Arc::clone(&logs)` into `ServiceDeps`, `logs` into the
    /// runtime - and the sibling precedent,
    /// `admission_and_dispatch_share_one_platform_lock_registry`, has the same
    /// shape and the same limit.
    #[tokio::test]
    async fn a_subscriber_only_hears_a_publisher_that_shares_its_broadcaster() {
        use crate::domain::service::LogFanout;

        let shared = Arc::new(RunLogBroadcaster::new(8));
        let run = uuid::Uuid::from_u128(0x5E5E);

        let mut listener = shared
            .subscribe(run)
            .expect("under the per-run subscriber cap");
        let publisher: Arc<dyn LogFanout> = Arc::clone(&shared) as Arc<dyn LogFanout>;
        publisher.publish(run, "from the shared hub".to_owned());
        assert_eq!(
            listener.recv().await.as_deref(),
            Some("from the shared hub"),
            "one Arc, two views: the publish must reach the subscriber"
        );

        // The failure mode, made concrete: a second broadcaster is a second
        // channel map, and its publishes go nowhere this subscriber can see.
        let second: Arc<dyn LogFanout> = Arc::new(RunLogBroadcaster::new(8)) as Arc<dyn LogFanout>;
        second.publish(run, "from a second hub".to_owned());
        publisher.publish(run, "sentinel".to_owned());
        assert_eq!(
            listener.recv().await.as_deref(),
            Some("sentinel"),
            "the second hub's line must not appear; the next line seen is the shared hub's"
        );
    }

    /// The cadence stored for `serve` is the **clamped** one.
    ///
    /// The mutation this catches is storing `cfg.dispatcher_interval_seconds`
    /// instead of the accessor: it compiles, and it differs only for a
    /// sub-floor configuration - where it would also make the serve loop re-emit
    /// the clamp warning on every tick.
    #[test]
    #[tracing_test::traced_test]
    fn the_resolved_cadence_is_clamped_once_not_configured() {
        let cfg = crate::config::QaRunsConfig {
            dispatcher_interval_seconds: 1,
            ..crate::config::QaRunsConfig::default()
        };
        let cadence = super::Cadence::dispatcher(&cfg);
        assert_eq!(
            cadence.interval_seconds,
            crate::config::MIN_DISPATCHER_INTERVAL_SECONDS,
            "the raw 1 must never reach serve"
        );
        assert!(cadence.runs);

        // **Once**, not twice. Asking the config for both `dispatcher_runs()`
        // and the interval called the clamping accessor twice and logged the
        // same WARN twice at boot.
        logs_assert(|lines: &[&str]| {
            let clamps = lines
                .iter()
                .filter(|line| line.contains("dispatcher_interval_seconds is below"))
                .count();
            if clamps == 1 {
                Ok(())
            } else {
                Err(format!("expected exactly one clamp warning, saw {clamps}"))
            }
        });
    }

    /// Both ways of switching the ticker off reach `serve` as one answer, so
    /// the lifecycle entry does not re-derive a precedence the config module
    /// already settled.
    #[test]
    fn either_disable_reaches_serve_as_a_single_answer() {
        for cfg in [
            crate::config::QaRunsConfig {
                dispatcher_enabled: false,
                ..crate::config::QaRunsConfig::default()
            },
            crate::config::QaRunsConfig {
                dispatcher_interval_seconds: 0,
                ..crate::config::QaRunsConfig::default()
            },
        ] {
            assert!(!super::Cadence::dispatcher(&cfg).runs);
        }
        assert!(
            super::Cadence::dispatcher(&crate::config::QaRunsConfig::default()).runs,
            "the shipped default runs a dispatcher"
        );
    }

    /// **The two cadences must not be resolved from each other's knobs.**
    ///
    /// The mutation this catches is the one the two-constructor shape exists to
    /// prevent: `Cadence::scheduler` reading `dispatcher_enabled` or
    /// `effective_dispatcher_interval_seconds`. Both compile — same types, same
    /// struct — and either is invisible to a test that configures the two knobs
    /// alike. This configures them to disagree in both fields at once.
    #[test]
    fn each_ticker_resolves_from_its_own_knobs() {
        let cfg = crate::config::QaRunsConfig {
            dispatcher_enabled: false,
            dispatcher_interval_seconds: 900,
            scheduler_enabled: true,
            schedule_interval_seconds: 120,
            ..crate::config::QaRunsConfig::default()
        };

        let dispatcher = super::Cadence::dispatcher(&cfg);
        let scheduler = super::Cadence::scheduler(&cfg);

        assert!(!dispatcher.runs, "the dispatcher is switched off here");
        assert_eq!(dispatcher.interval_seconds, 900);
        assert!(scheduler.runs, "and the scheduler is not");
        assert_eq!(
            scheduler.interval_seconds, 120,
            "reading the dispatcher's interval here would answer 900"
        );
    }

    /// And the shipped default starts both, which is what a deployment that
    /// configures nothing gets.
    #[test]
    fn the_shipped_default_runs_both_tickers() {
        let cfg = crate::config::QaRunsConfig::default();
        assert!(super::Cadence::dispatcher(&cfg).runs);
        assert!(super::Cadence::scheduler(&cfg).runs);
    }

    /// **The inlined disable rule and the config predicate must agree.**
    ///
    /// [`super::Cadence`] re-derives `enabled && interval != 0` rather than
    /// calling `dispatcher_runs()`/`scheduler_runs()`, so that the clamping
    /// accessor is read once — see that type's doc. The price is two homes for
    /// one rule, and until this test nothing noticed them diverging: the config
    /// predicates have **no production caller at all**, so a change to either
    /// side alone was green.
    ///
    /// Driven over the whole cross product of the four knobs rather than a
    /// hand-picked case, because the interesting disagreements are at the
    /// combinations — a sub-floor interval, which clamps to non-zero and so
    /// *runs*, and a zero one, which does not.
    #[test]
    fn each_cadence_agrees_with_the_config_predicate_it_inlines() {
        for enabled in [true, false] {
            for interval in [0, 1, 5, 10, 900] {
                let dispatcher = crate::config::QaRunsConfig {
                    dispatcher_enabled: enabled,
                    dispatcher_interval_seconds: interval,
                    ..crate::config::QaRunsConfig::default()
                };
                assert_eq!(
                    super::Cadence::dispatcher(&dispatcher).runs,
                    dispatcher.dispatcher_runs(),
                    "dispatcher disagreed at enabled={enabled} interval={interval}"
                );

                let scheduler = crate::config::QaRunsConfig {
                    scheduler_enabled: enabled,
                    schedule_interval_seconds: interval,
                    ..crate::config::QaRunsConfig::default()
                };
                assert_eq!(
                    super::Cadence::scheduler(&scheduler).runs,
                    scheduler.scheduler_runs(),
                    "scheduler disagreed at enabled={enabled} interval={interval}"
                );
            }
        }
    }

    /// **`LogWiring`'s two views are one allocation.**
    ///
    /// The property `init` depends on, tested at the seam that now owns it -
    /// which is the point of extracting the type: before it, this was
    /// `Arc::clone` twice in a function no test can construct, and allocating a
    /// second broadcaster for one of the two was green.
    #[tokio::test]
    async fn the_log_wiring_hands_out_two_views_of_one_broadcaster() {
        let wiring = super::LogWiring::new(8);
        let run = uuid::Uuid::from_u128(0x1109);

        let mut listener = wiring
            .subscriber()
            .subscribe(run)
            .expect("under the per-run subscriber cap");
        wiring.publisher().publish(run, "one hub".to_owned());

        assert_eq!(
            listener.recv().await.as_deref(),
            Some("one hub"),
            "a line published through the port must reach a subscriber taken \
             from the concrete view"
        );
    }

    /// `Gear::init`'s own source, isolated from the rest of `gear.rs`.
    ///
    /// Extracted once both source-scanning composition tests below needed the
    /// same slice: `init` is the only item in `impl Gear for QaRuns`, so the
    /// block's closing brace at column zero is the end of the body.
    fn init_source() -> &'static str {
        let src = include_str!("gear.rs");
        let start = src
            .find("    async fn init(")
            .expect("gear.rs declares Gear::init");
        let tail = &src[start..];
        let end = tail
            .find("\n}\n")
            .expect("the impl block containing init is closed");
        &tail[..end]
    }

    /// `QaRuns::dispatcher_ticker`'s own source, isolated the same way
    /// [`init_source`] isolates `init`'s.
    ///
    /// Bounded by the *next* function's declaration rather than by a
    /// column-zero brace: `dispatcher_ticker` is one of several items in its
    /// `impl` block, so the block's closing brace is far past the end of this
    /// one.
    fn dispatcher_ticker_source() -> &'static str {
        let src = include_str!("gear.rs");
        let start = src
            .find("    fn dispatcher_ticker(")
            .expect("gear.rs declares dispatcher_ticker");
        let tail = &src[start..];
        let end = tail
            .find("\n    /// Spawn the leader-gated schedule firing ticker.")
            .expect("scheduler_ticker still follows dispatcher_ticker");
        &tail[..end]
    }

    /// `QaRuns::serve`'s own source, isolated the same way [`init_source`]
    /// isolates `init`'s — nothing can drive `serve` either, without a real
    /// runtime and a real ticker, so reading it is what is left for the two
    /// properties below.
    fn serve_source() -> &'static str {
        let src = include_str!("gear.rs");
        let start = src
            .find("    pub(crate) async fn serve(")
            .expect("gear.rs declares serve");
        let tail = &src[start..];
        let end = tail
            .find("\n    /// Spawn the leader-gated dispatcher ticker.")
            .expect("dispatcher_ticker still follows serve");
        &tail[..end]
    }

    /// **`serve` bridges the framework's `cancel` into the token the watcher
    /// was built with, and waits for the watcher's shutdown before
    /// returning.** Review finding #18: an observer's cancellation source has
    /// to reach it from *somewhere*, and `init` builds the watcher before
    /// `serve`'s own token exists at all — see `QaRunsRuntime::shutdown`'s
    /// doc. Nothing exercises this end to end (that needs the framework's own
    /// runtime), so this is the tripwire: a `serve` that stopped bridging or
    /// stopped awaiting the shutdown would still compile, still pass every
    /// other test in this file, and would silently restore "a dropped runtime
    /// is the only thing that ends an observer" — which is the exact defect
    /// this task closes.
    #[test]
    fn serve_bridges_shutdown_to_the_watcher_and_awaits_it() {
        let body = serve_source();

        assert!(
            body.contains("rt.shutdown"),
            "serve must reach the token the watcher was built with in init: {body}"
        );
        assert!(
            body.contains(".cancel()"),
            "and actually cancel it once the framework's own signal fires, not just \
             read it"
        );
        assert!(
            body.contains("rt.services.shutdown()"),
            "and await the production watcher's shutdown before returning, or a \
             caller has no way to know an observer might still be unwinding: {body}"
        );

        // The bridge specifically, isolated from every other `cancel.cancelled()`
        // in this function (the two idle-until-cancelled early returns
        // legitimately await `cancel` directly). Fix round 1, Important 3:
        // the first version of this test passed with the bridge wired to
        // `cancel.cancelled()` instead of `tasks.cancelled()` - precisely the
        // bug self-review caught, and re-introducing it leaves the other
        // three assertions above green because `rt.shutdown`, `.cancel()` and
        // `rt.services.shutdown()` are all still present. Only this pins the
        // fix itself: `tasks`, not `cancel`, is what `supervise` cancels
        // unconditionally on every exit path, including a ticker panicking
        // before `cancel` ever fires - see the bridge's own comment.
        let spawn_start = body
            .find("tokio::spawn({")
            .expect("serve still spawns the bridge task");
        let bridge = &body[spawn_start..];
        let spawn_end = bridge
            .find("});")
            .expect("the bridge's tokio::spawn block is closed");
        let bridge = &bridge[..spawn_end];

        assert!(
            bridge.contains("tasks.cancelled()"),
            "the bridge must await `tasks`, not `cancel` directly, or the \
             premature-ticker-failure path never wakes it: {bridge}"
        );
        assert!(
            !bridge.contains("cancel.cancelled()"),
            "the bridge awaiting `cancel` directly is the exact regression this \
             test exists to catch: {bridge}"
        );
        assert!(
            bridge.contains("shutdown.cancel()"),
            "and it must actually cancel the watcher's token, not just observe \
             `tasks`: {bridge}"
        );
    }

    /// **`init` allocates exactly one broadcaster, and takes both views from
    /// it.**
    ///
    /// The tripwire [`super::LogWiring`]'s doc describes. `LogWiring` cannot
    /// make the second allocation unspellable - measured: substituting
    /// `Arc::new(RunLogBroadcaster::new(cfg.log_buffer_lines))` for
    /// `logs.subscriber()` compiles and leaves the suite green - and `init`
    /// needs a database, a `ClientHub` and four resolved cross-gear clients, so
    /// nothing can drive it. Reading its source is what is left.
    ///
    /// What it catches is the obvious spelling and only that: an aliased
    /// constructor, a helper that allocates one, or a second `LogWiring` would
    /// all pass. Break-verified with the substitution above.
    #[test]
    fn init_builds_exactly_one_log_broadcaster() {
        let body = init_source();

        assert!(
            !body.contains("RunLogBroadcaster::new"),
            "init must take both log views from one `LogWiring`; allocating a \
             broadcaster here gives the SSE route a channel map nothing publishes \
             into, with no error and no warning: {body}"
        );
        assert!(
            body.contains("LogWiring::new("),
            "premise: this reads the wrong region if init no longer builds the wiring"
        );
        for view in ["logs.publisher()", "logs.subscriber()"] {
            assert!(
                body.contains(view),
                "both views must come from the wiring; {view} is missing"
            );
        }
    }

    /// **A second `RunLogArchive::new` in `init` would type-check and would
    /// silently archive nothing** — the ingest service would buffer into one
    /// instance while the dispatcher tick drained another. This is the same
    /// hazard [`init_builds_exactly_one_log_broadcaster`] above guards for the
    /// broadcaster, and it is guarded the same way: by reading `init`'s
    /// source for a second construction.
    ///
    /// **The two premise assertions are what the count alone cannot say.**
    /// `matches("RunLogArchive::new").count() == 1` says one instance is
    /// built; it says nothing about that instance reaching **both**
    /// consumers, which is the property this test's own message claims
    /// ("construct the archive once and share the `Arc`"). The sibling
    /// [`init_builds_exactly_one_log_broadcaster`] states its equivalent
    /// explicitly — `LogWiring::new(` plus both `logs.publisher()` and
    /// `logs.subscriber()` — and this test asserted parity with it in prose
    /// while asserting none of it in code.
    ///
    /// What is *not* claimed: that deleting a consumer's `archive` field
    /// would otherwise slip through. It would not compile —
    /// `ServiceDeps::archive` and `QaRunsRuntime::archive` are both mandatory
    /// fields (Task 5 made the former so deliberately), so an omission is
    /// `error[E0063]`. And substituting a second construction for either is
    /// what the count assertion above catches. The premises cover the
    /// remaining case, which is the same one the sibling's cover: the
    /// wiring being re-spelled so that this source scan no longer describes
    /// it.
    ///
    /// **Break-verified** by renaming `init`'s `archive` binding to
    /// `log_archive` throughout — which compiles, keeps the count at exactly
    /// 1, and left this test green before: it now goes red on the
    /// `ServiceDeps` premise.
    #[test]
    fn init_constructs_exactly_one_archive() {
        let body = init_source();
        assert_eq!(
            body.matches("RunLogArchive::new").count(),
            1,
            "init must construct the archive once and share the Arc",
        );
        assert!(
            body.contains("archive: Arc::clone(&archive)"),
            "premise: `ServiceDeps` must take the archive from that one binding, or \
             `IngestService` is buffering into an instance nothing drains: {body}"
        );
        assert!(
            body.contains("archive,\n"),
            "premise: `QaRunsRuntime` must take the same binding, or the dispatcher \
             tick is draining an instance nothing buffers into: {body}"
        );
    }

    /// **The dispatcher tick is what covers the terminal paths
    /// `IngestService::finish` does not** — D-RLP-4's central claim, and
    /// nothing else on this branch asserted the call site exists.
    ///
    /// A source scan for the same reason [`init_builds_exactly_one_log_broadcaster`]
    /// is one: the tick body needs a database, a `ClientHub`, four resolved
    /// cross-gear clients and a `LeaderElector` before it will run a single
    /// pass, so no test can drive it. Deleting `archive.flush_due()` from that
    /// loop leaves the crate compiling and every other test green, while a
    /// cancelled, timed-out, expired or orphan-recovered run's tail is never
    /// written — which is the whole reason the flush is not attached to
    /// `LogFanout::reap`'s four call sites instead.
    ///
    /// **Break-verified** by deleting the `let flushed = archive.flush_due()
    /// .await;` line: this goes red, and it is the only test that does.
    #[test]
    fn the_dispatcher_tick_flushes_the_archive() {
        let body = dispatcher_ticker_source();
        assert!(
            body.contains("ticker.tick()"),
            "premise: this reads the wrong region if the ticker loop moved: {body}"
        );
        assert!(
            body.contains("archive.flush_due()"),
            "the dispatcher tick must flush the log archive, or every run that ends \
             by a path other than IngestService::finish loses its tail: {body}"
        );
    }

    /// A ticker under `role` that runs until the shared token fires, as a real
    /// one does.
    ///
    /// The future is `'static` because the token is **cloned** into it rather
    /// than borrowed — which is what a spawned task needs, and what the real
    /// tickers do with the same token.
    fn healthy(
        role: &'static str,
        tasks: &tokio_util::sync::CancellationToken,
    ) -> super::Ticker<impl std::future::Future<Output = ()> + Send + 'static> {
        let child = tasks.clone();
        super::Ticker {
            role,
            run: async move { child.cancelled().await },
        }
    }

    /// A ticker under `role` that ends the moment it is polled.
    fn stops_at_once(role: &'static str) -> super::Ticker<impl std::future::Future<Output = ()>> {
        super::Ticker {
            role,
            run: async {},
        }
    }

    /// **`supervise` reports a premature ticker exit as an error, and names it.**
    ///
    /// Reporting it as `Ok` is precisely the silent idling `serve`'s doc
    /// forbids: a gear with no dispatcher looks identical to a healthy idle one
    /// from outside. Mutating the `Err` arm to `Ok(())` was green until this
    /// test.
    #[tokio::test]
    async fn a_ticker_that_exits_before_cancellation_is_an_error() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let tasks = cancel.child_token();
        let mut tickers = super::Tickers::default();
        // Returns immediately: the shape a ticker whose `run_role` errored has.
        tickers.spawn(stops_at_once("the-role"));

        let result = super::supervise(cancel, &tasks, tickers).await;

        let error = result.expect_err("a ticker that stopped on its own must surface");
        assert!(
            error.to_string().contains("the-role"),
            "the error must name which ticker stopped: {error}"
        );
    }

    /// **A panicking ticker is named too, and that is the arm the role used to
    /// be lost on.**
    ///
    /// The role used to be the future's own output, which exists only on the
    /// `Ok` arm. A panic joins as `JoinError`, so the operator got the task
    /// number — *"task 2 panicked with message \"boom\""* — for the one failure
    /// the shared supervisor exists to report. Neither sibling test covers it:
    /// both spawn futures that exit `Ok`.
    ///
    /// The healthy ticker is here so the id lookup has more than one entry to
    /// choose between; with a single ticker any lookup, including a wrong one,
    /// would answer correctly.
    #[tokio::test]
    async fn a_panicking_ticker_is_named_in_the_error() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let tasks = cancel.child_token();
        let mut tickers = super::Tickers::default();
        tickers.spawn(healthy("the-healthy-one", &tasks));
        tickers.spawn(super::Ticker {
            role: "the-panicking-one",
            run: async { panic!("boom") },
        });

        let error = super::supervise(cancel, &tasks, tickers)
            .await
            .expect_err("a panicked ticker must surface");

        assert!(
            error.to_string().contains("the-panicking-one"),
            "a panic must name the role, not just the task id: {error}"
        );
        assert!(
            !error.to_string().contains("the-healthy-one"),
            "and it must not name the one still running: {error}"
        );
    }

    /// **A surviving ticker does not mask a stopped one.**
    ///
    /// This is the property one supervisor per ticker would not have. The
    /// healthy ticker below runs until cancellation, exactly as a real one does,
    /// so a supervisor that waited for *all* tickers — or that watched only the
    /// first — would block here rather than reporting.
    #[tokio::test]
    async fn one_ticker_stopping_ends_serve_even_though_the_other_is_healthy() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let tasks = cancel.child_token();
        let mut tickers = super::Tickers::default();
        tickers.spawn(healthy("healthy", &tasks));
        tickers.spawn(stops_at_once("stopped"));

        let error = super::supervise(cancel, &tasks, tickers)
            .await
            .expect_err("a half-dead gear must not look healthy");
        assert!(
            error.to_string().contains("stopped"),
            "and it must name the one that stopped, not the one that did not: {error}"
        );
    }

    /// The other half: a cancelled shutdown drains **every** ticker and returns
    /// `Ok`, so a normal stop is not reported as a fault.
    #[tokio::test]
    async fn a_cancelled_shutdown_drains_cleanly() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let tasks = cancel.child_token();
        let mut tickers = super::Tickers::default();
        for role in ["one", "two"] {
            tickers.spawn(healthy(role, &tasks));
        }

        cancel.cancel();
        super::supervise(cancel, &tasks, tickers)
            .await
            .expect("a cooperative shutdown is not an error");
    }

    /// The gear name the macro derives **and** the key the dev config uses,
    /// which must match exactly: a typo in either makes the gear silently take
    /// its defaults, with no error anywhere.
    ///
    /// The config is read with `include_str!` rather than asserted from memory,
    /// following `domain::system_actor`'s precedent for source-reading tests.
    /// It pins the *dev* config only - a deployment's own file is not in this
    /// repository and nothing here can check it.
    #[test]
    fn the_gear_name_matches_the_config_section() {
        assert_eq!(QaRuns::MODULE_NAME, "qa-runs");

        let config = include_str!("../../../../../config/qa-platform.yaml");
        assert!(
            config.contains("\n  qa-runs:\n"),
            "the dev config must carry a section keyed by the gear's own name"
        );
    }
}
