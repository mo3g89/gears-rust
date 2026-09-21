//! Composition root: `#[toolkit::gear]` bootstrap, `Gear::init`, the
//! `DatabaseCapability`/`RestApiCapability` implementations, and the
//! stateful lifecycle entry (`serve`) hosting the two background tasks
//! (branch-cache refresher, bundle GC).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use sea_orm_migration::MigrationTrait;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use tracing::{debug, error, info, warn};

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use credstore_sdk::CredStoreClientV1;
use qa_catalog_sdk::{QaCatalogClientV1, QaProductPluginResolverV1};
use uuid::Uuid;

use crate::api::rest::routes;
use crate::config::QaCatalogConfig;
use crate::domain::error::DomainError;
use crate::domain::local_client::{QaCatalogLocalClient, QaProductPluginResolverLocalClient};
use crate::domain::ports::bundle_store::BundleStore;
use crate::domain::ports::repo_sync::RepoSyncPort;
use crate::domain::service::{
    AppServices, BundleDownloadSigningSecret, QaProductRegistry, ServiceDeps, SyncCache,
};
use crate::domain::system_actor;
use crate::infra::bundle_store::LocalFsBundleStore;
use crate::infra::fs::create_private_dir_all;
use crate::infra::git::GixSyncEngine;
use crate::domain::ports::metrics::{BundleDownloadMetrics, PluginResolutionMetrics};
use crate::infra::metrics::build_default_adapter;
use crate::infra::storage::{
    OrmBundlesRepository, OrmCustomPlansRepository, OrmProductsRepository, OrmSshKeysRepository,
    OrmTestReposRepository,
};

/// Bundle GC cadence (fixed at 300s; the *TTL* is configurable, the sweep
/// is not).
const BUNDLE_GC_INTERVAL: Duration = Duration::from_mins(5);

/// Floor for `branch_refresh_interval_seconds` when the refresher is enabled.
///
/// Each pass performs a live `ls-refs` (plus a credstore read for credentialed
/// repositories) against *every* repository of *every* tenant, so a
/// small configured value turns the gear into a self-inflicted load generator
/// against the git hosts. `0` still disables the task outright; any other
/// value below this floor is clamped up to it.
const MIN_BRANCH_REFRESH_INTERVAL_SECONDS: u64 = 60;

/// Resolve the effective refresher interval: `0` disables (returned as-is),
/// anything else is clamped to at least [`MIN_BRANCH_REFRESH_INTERVAL_SECONDS`]
/// with a one-time warning naming both the configured and effective values.
fn effective_branch_refresh_interval(configured: u64) -> u64 {
    if configured == 0 || configured >= MIN_BRANCH_REFRESH_INTERVAL_SECONDS {
        return configured;
    }
    warn!(
        configured_seconds = configured,
        effective_seconds = MIN_BRANCH_REFRESH_INTERVAL_SECONDS,
        "qa-catalog: branch_refresh_interval_seconds is below the supported floor; \
         clamping (each pass does a live ls-refs against every repository)"
    );
    MIN_BRANCH_REFRESH_INTERVAL_SECONDS
}

/// Concrete instantiation of the generic `AppServices<R, C, P, K, B>` DI
/// container, wired to the `SeaORM`-backed repositories. Used by the REST
/// handlers (`api::rest::handlers`), routes (`api::rest::routes`), the local
/// client (`domain::local_client`), and the lifecycle tasks below.
pub(crate) type ConcreteAppServices = AppServices<
    OrmTestReposRepository,
    OrmCustomPlansRepository,
    OrmProductsRepository,
    OrmSshKeysRepository,
    OrmBundlesRepository,
>;

/// Everything `serve` needs beyond the gear struct itself: the wired
/// services plus the config values driving the task cadences.
struct QaCatalogRuntime {
    services: Arc<ConcreteAppServices>,
    /// `QaCatalogConfig::branch_refresh_interval_seconds` as resolved by
    /// [`effective_branch_refresh_interval`] — already clamped to the
    /// supported floor (0 = refresher disabled).
    branch_refresh_interval_seconds: u64,
}

/// Main gear struct with DDD-light layout and proper `ClientHub` integration.
///
/// No `await_ready`: the background jobs must not gate request traffic, so
/// `serve` omits the `ReadySignal` argument (mirrors bss-ledger).
#[toolkit::gear(
    name = "qa-catalog",
    deps = [authz_resolver, credstore],
    capabilities = [db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s")
)]
pub struct QaCatalog {
    // Keep the runtime behind OnceLock for set-once access.
    runtime: OnceLock<Arc<QaCatalogRuntime>>,
}

impl Default for QaCatalog {
    fn default() -> Self {
        Self {
            runtime: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for QaCatalog {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: QaCatalogConfig = ctx.config_or_default()?;
        debug!(
            "Loaded qa-catalog config: repos_dir={}, bundles_dir={}, bundle_ttl_seconds={}, branch_refresh_interval_seconds={}, branch_freshness_ttl_seconds={}",
            cfg.repos_dir,
            cfg.bundles_dir,
            cfg.bundle_ttl_seconds,
            cfg.branch_refresh_interval_seconds,
            cfg.branch_freshness_ttl_seconds
        );
        // The one loud signal for an unset download signing secret. It is
        // fail-closed at request time -- `BundlesService::verify_download_
        // signature` refuses every download, so a deployment in this state
        // runs no tests at all -- and a per-request 403 in a workflow pod's
        // log is not where an operator looks. `signing_secret_is_configured`
        // is the single predicate this check and that refusal share, so the
        // two cannot drift the way qa-insights' pair once did (its boot check
        // tested `is_empty()` while the refusal had been raised to a 16-byte
        // floor, and a short secret then passed boot and failed every
        // request).
        if !crate::domain::service::bundles::signing_secret_is_configured(
            &cfg.bundle_download_signing_secret,
        ) {
            warn!(
                "bundle_download_signing_secret is empty or too short; EVERY test-bundle \
                 download will be refused with Forbidden and no run will execute a single \
                 test (fail-closed by design - an empty HMAC key is a publicly known key, \
                 not 'no protection'). Set this to a random per-deployment secret of at \
                 least 16 characters.",
            );
        }

        // Acquire DB capability, re-parameterized with DomainError so
        // transaction closures preserve domain variants (see
        // `domain::service::DbProvider`).
        let db = Arc::new(DBProvider::<DomainError>::new(ctx.db_required()?.db()));

        // Cross-gear clients from ClientHub.
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;
        let credstore = ctx
            .client_hub()
            .get::<dyn CredStoreClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get credstore client: {e}"))?;

        // Working-copy root, created up front and owner-only: gix creates
        // `<repos_dir>/<repo_id>` itself on first clone (at its own default
        // mode), so pre-creating the parent at 0700 is what keeps synced test
        // content unreadable by other local users — traversal needs the
        // execute bit on every component.
        create_private_dir_all(&cfg.repos_dir).map_err(|e| {
            anyhow::anyhow!(
                "failed to create the repositories directory '{}': {e}",
                cfg.repos_dir
            )
        })?;

        // Infra ports (ADR-0005 gix engine; local-fs bundle store, which
        // creates its own directory owner-only).
        let sync_engine: Arc<dyn RepoSyncPort> = Arc::new(GixSyncEngine);
        let bundle_store: Arc<dyn BundleStore> =
            Arc::new(LocalFsBundleStore::new(&cfg.bundles_dir).map_err(|e| {
                anyhow::anyhow!(
                    "failed to initialize bundle store at '{}': {e}",
                    cfg.bundles_dir
                )
            })?);

        let bundle_ttl = time::Duration::seconds(
            i64::try_from(cfg.bundle_ttl_seconds)
                .map_err(|_| anyhow::anyhow!("bundle_ttl_seconds out of range"))?,
        );

        // The product → plugin resolver and plugin catalogue. Built before
        // `AppServices` consumes `db`/`authz`, and shared with it by `Arc`:
        // there is exactly one per process, with two kinds of consumer. Other
        // gears reach it through the `ClientHub` registration below
        // (qa-environments' observation, qa-runs' dispatch); this gear's own
        // REST layer reaches it through the DI container, for
        // `GET /qa/v1/product-plugins`.
        //
        // Task 12 built this outside the container and said so, because at
        // that point nothing in this gear's REST layer touched a plugin.
        // Task 13's catalogue endpoint is what changed it.
        // Unconditionally, with no `metrics.enabled` branch of our own to get
        // wrong: `build_default_adapter` reads the process-global meter
        // provider, which `toolkit`'s telemetry init leaves as the built-in
        // no-op when metrics are switched off or never configured. So a
        // deployment with no pipeline builds every instrument and emits into
        // nothing, which is exactly the "silent with no adapter" posture the
        // observability constraints ask for. See `infra::metrics`' header.
        let metrics = build_default_adapter();

        let plugin_registry = Arc::new(QaProductRegistry::new(
            Arc::clone(&db),
            Arc::new(OrmProductsRepository),
            PolicyEnforcer::new(Arc::clone(&authz)),
            ctx.client_hub(),
            Some(Arc::clone(&metrics) as Arc<dyn PluginResolutionMetrics>),
        ));

        let services = Arc::new(AppServices::new(
            Arc::new(OrmTestReposRepository),
            Arc::new(OrmCustomPlansRepository),
            Arc::new(OrmProductsRepository),
            Arc::new(OrmSshKeysRepository),
            Arc::new(OrmBundlesRepository),
            Arc::clone(&plugin_registry),
            ServiceDeps {
                db,
                authz,
                credstore,
                sync_engine,
                bundle_store,
                repos_dir: PathBuf::from(&cfg.repos_dir),
                bundle_ttl,
                bundle_download_signing_secret: BundleDownloadSigningSecret(
                    cfg.bundle_download_signing_secret.clone(),
                ),
                // The SAME adapter the plugin registry holds, narrowed to the
                // one trait `BundlesService` needs. `build_default_adapter` is
                // still called exactly once -- see
                // `init_installs_exactly_one_metrics_adapter_into_the_plugin_registry`.
                bundle_download_metrics: metrics as Arc<dyn BundleDownloadMetrics>,
                sync_cache: Arc::new(SyncCache::new(Duration::from_secs(
                    cfg.branch_freshness_ttl_seconds,
                ))),
            },
        ));

        self.runtime
            .set(Arc::new(QaCatalogRuntime {
                services: Arc::clone(&services),
                branch_refresh_interval_seconds: effective_branch_refresh_interval(
                    cfg.branch_refresh_interval_seconds,
                ),
            }))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Register local client under the SDK trait for transport-agnostic
        // (in-process, `ClientHub`-mediated) consumption by other gears
        // (primary consumer: qa-runs).
        ctx.client_hub()
            .register::<dyn QaCatalogClientV1>(Arc::new(QaCatalogLocalClient::new(services)));

        // Unscoped, beside the client above: there is exactly one product →
        // plugin resolver per process. (The *plugins* are the scoped
        // registrations, one per GTS instance id, published by the plugin
        // gears themselves.) Consumers: qa-environments' observation and
        // qa-runs' dispatch.
        ctx.client_hub()
            .register::<dyn QaProductPluginResolverV1>(Arc::new(
                QaProductPluginResolverLocalClient::new(plugin_registry),
            ));

        Ok(())
    }
}

impl DatabaseCapability for QaCatalog {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing qa-catalog database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for QaCatalog {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering qa-catalog REST routes");

        let runtime = self
            .runtime
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?;

        let router = routes::register_routes(router, openapi, Arc::clone(&runtime.services));

        info!("qa-catalog REST routes registered successfully");
        Ok(router)
    }
}

// ---------------------------------------------------------------------------
// Lifecycle (stateful capability): background tasks
// ---------------------------------------------------------------------------

impl QaCatalog {
    /// Lifecycle entry (`stateful` capability). Spawns the branch-cache
    /// refresher and the bundle-GC tickers under a child of `cancel`.
    /// A tick failure is logged and the loop continues (a transient job
    /// error must not kill the gear); a panicking task cancels the other so
    /// the runtime sees an abort.
    ///
    /// **No cluster-wide leader election** — deliberately: both jobs are
    /// idempotent (a branch refresh converges to the same cache and a
    /// concurrent duplicate at worst surfaces a retryable
    /// `BranchCacheConflict`; expired-bundle deletes are idempotent), so
    /// multiple replicas running them concurrently waste a little work but
    /// never corrupt state.
    ///
    /// # Errors
    /// Returns `Err` only if a spawned ticker task panics / aborts
    /// (surfaced as a join error); cooperative cancel-token shutdown
    /// returns `Ok(())`.
    pub(crate) async fn serve(self: Arc<Self>, cancel: CancellationToken) -> anyhow::Result<()> {
        let Some(rt) = self.runtime.get().cloned() else {
            info!("qa-catalog: serve() with no runtime (uninitialized); idling until cancelled");
            cancel.cancelled().await;
            return Ok(());
        };

        // Shared child token — cancelled by either the runtime (normal
        // shutdown via `cancel`) or by the supervisor when one ticker
        // dies, so both tickers observe the same cancellation.
        let tasks = cancel.child_token();
        let (refresher, gc) = Self::spawn_tasks(&rt, &tasks);
        supervise(cancel, &tasks, refresher, gc).await
    }

    /// Spawn the tickers under `tasks`: the branch-cache refresher (unless
    /// disabled by a zero interval) and the bundle GC.
    fn spawn_tasks(
        rt: &Arc<QaCatalogRuntime>,
        tasks: &CancellationToken,
    ) -> (Option<JoinHandle<()>>, JoinHandle<()>) {
        let refresher = if rt.branch_refresh_interval_seconds == 0 {
            info!(
                "qa-catalog: branch-cache refresher disabled (branch_refresh_interval_seconds = 0)"
            );
            None
        } else {
            Some(Self::spawn_branch_refresher(Arc::clone(rt), tasks.clone()))
        };
        let gc = Self::spawn_bundle_gc(Arc::clone(rt), tasks.clone());

        info!(
            branch_refresh_interval_seconds = rt.branch_refresh_interval_seconds,
            bundle_gc_interval_seconds = BUNDLE_GC_INTERVAL.as_secs(),
            "qa-catalog background tasks started"
        );

        (refresher, gc)
    }

    /// Spawn the branch-cache refresher: every
    /// `branch_refresh_interval_seconds`, enumerate `(repo, tenant)` targets
    /// under a platform-scoped system context and refresh each repository's
    /// branch cache (bare ls-refs, no content sync) under a system context
    /// bound to that repository's tenant — the system-context idiom for
    /// background jobs (see `domain::system_actor`).
    fn spawn_branch_refresher(
        rt: Arc<QaCatalogRuntime>,
        token: CancellationToken,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut iv =
                tokio::time::interval(Duration::from_secs(rt.branch_refresh_interval_seconds));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first `interval` tick fires immediately — a startup
            // refresh is idempotent and harmless.
            loop {
                tokio::select! {
                    biased;
                    () = token.cancelled() => break,
                    _ = iv.tick() => Self::refresh_branch_caches(&rt, &token).await,
                }
            }
        })
    }

    /// One refresher pass. Failures are logged per repository and never
    /// abort the pass (let alone the ticker).
    async fn refresh_branch_caches(rt: &QaCatalogRuntime, token: &CancellationToken) {
        let enumeration_ctx = system_actor::for_branch_refresh_enumeration();
        let targets = match rt
            .services
            .repos
            .list_refresh_targets(&enumeration_ctx)
            .await
        {
            Ok(targets) => targets,
            Err(e) => {
                log_task_failure("branch-cache refresh enumeration", &e);
                return;
            }
        };

        debug!(
            repos = targets.len(),
            "qa-catalog: branch-cache refresh pass"
        );
        for target in targets {
            // Cooperative shutdown between (potentially slow) remote reads.
            if token.is_cancelled() {
                return;
            }
            Self::refresh_one(rt, target).await;
        }
    }

    /// Refresh one repository's branch cache under its tenant-bound system
    /// context; the failure is logged (WARN — one unreachable remote must not
    /// read as a gear fault), never propagated.
    async fn refresh_one(rt: &QaCatalogRuntime, target: crate::domain::repos::RefreshTarget) {
        let ctx = system_actor::for_branch_refresh(target.tenant_id);
        if let Err(e) = rt
            .services
            .repos
            .refresh_branches(&ctx, target.repo_id)
            .await
        {
            warn!(
                repo_id = %target.repo_id,
                error = %e,
                "qa-catalog: branch-cache refresh failed for repository"
            );
        }
    }

    /// Spawn the bundle GC: every [`BUNDLE_GC_INTERVAL`], enumerate tenants
    /// with expired bundles under a platform-scoped system context and purge
    /// each one's expired bundle descriptors and blobs under a system
    /// context bound to that tenant — the same enumerate-then-act-per-tenant
    /// shape [`Self::spawn_branch_refresher`] uses (see
    /// `domain::system_actor`).
    fn spawn_bundle_gc(rt: Arc<QaCatalogRuntime>, token: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(BUNDLE_GC_INTERVAL);
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    biased;
                    () = token.cancelled() => break,
                    _ = iv.tick() => Self::run_bundle_gc(&rt, &token).await,
                }
            }
        })
    }

    /// One GC pass. Failures enumerating are logged and end the pass;
    /// failures purging one tenant are logged per tenant and never abort the
    /// rest of the pass (mirrors [`Self::refresh_branch_caches`]).
    ///
    /// The enumeration is nil-tenant and elevated
    /// (`BundlesService::tenants_with_expired_bundles`, `domain::elevated`);
    /// each per-tenant purge is tenant-bound and still PEP-authorized
    /// (`BundlesService::purge_expired` under
    /// `system_actor::for_bundle_delete`). Splitting the two is what keeps
    /// `BundlesRepository::delete_expired`'s atomic select+delete inside a
    /// transaction scoped to one tenant, even though the read that finds
    /// which tenants to sweep spans every tenant. Elevating the delete itself
    /// — passing the nil-tenant enumeration scope into `purge_expired` — was
    /// considered and rejected: it would widen a *write* across every tenant
    /// in one transaction, which is exactly the escape `domain::elevated`'s
    /// "read paths only" contract exists to close.
    ///
    /// Also mirrors [`Self::refresh_branch_caches`] in stopping between
    /// tenants once `token` fires, rather than after the pass: a purge is a
    /// database round trip per tenant, so checking only at the end would have
    /// already paid for every one of them.
    async fn run_bundle_gc(rt: &QaCatalogRuntime, token: &CancellationToken) {
        let enumeration_ctx = system_actor::for_bundle_gc();
        let tenants = match rt
            .services
            .bundles
            .tenants_with_expired_bundles(&enumeration_ctx)
            .await
        {
            Ok(tenants) => tenants,
            Err(e) => {
                log_task_failure("bundle GC enumeration", &e);
                return;
            }
        };

        debug!(tenants = tenants.len(), "qa-catalog: bundle GC pass");
        let mut total_purged = 0usize;
        for tenant_id in tenants {
            if token.is_cancelled() {
                return;
            }
            total_purged += Self::purge_one_tenants_bundles(rt, tenant_id).await;
        }

        if total_purged > 0 {
            info!(
                purged = total_purged,
                "qa-catalog: bundle GC pass purged expired bundles"
            );
        }
    }

    /// Purge one tenant's expired bundles under
    /// `system_actor::for_bundle_delete(tenant_id)`; the failure is logged
    /// (never propagated), and the count is `0` on failure exactly as it is
    /// when there was nothing expired.
    async fn purge_one_tenants_bundles(rt: &QaCatalogRuntime, tenant_id: Uuid) -> usize {
        let ctx = system_actor::for_bundle_delete(tenant_id);
        match rt.services.bundles.purge_expired(&ctx).await {
            Ok(purged) => purged,
            Err(e) => {
                log_task_failure("bundle GC pass", &e);
                0
            }
        }
    }
}

/// Log a background-pass failure at the level its cause warrants.
///
/// `Forbidden` means the deployment's policy does not grant this gear's system
/// actor the scope the task needs (see `domain::system_actor`). The task is
/// then inert *by configuration*, not broken: an operator must still see it —
/// GC not running eventually fills the bundle store — but at every tick it is
/// a recurring, expected steady state, so it belongs at WARN with the remedy
/// named rather than in the ERROR stream where real faults live. Everything
/// else (DB, storage, git) is a genuine fault and stays at ERROR.
///
/// **Of this function's three callers, only the "bundle GC pass" one can
/// still produce `Forbidden`.** "branch-cache refresh enumeration" and
/// "bundle GC enumeration" both elevate through `domain::elevated` rather
/// than asking the PDP (see that module's doc), so neither reaches
/// `access_scope` at all any more and neither can be denied — a failure at
/// either call site is a database error. The per-tenant write that follows
/// each enumeration (`refresh_one`'s own `warn!`, and this function's "bundle
/// GC pass" call) is still PEP-authorized per tenant and is where a
/// deployment's missing grant actually shows up.
fn log_task_failure(pass: &str, e: &DomainError) {
    if matches!(e, DomainError::Forbidden) {
        warn!(
            error = %e,
            "qa-catalog: {pass} is not authorized; grant the qa-catalog system actor \
             (subject_type=qa_catalog.system) the scopes this task needs, or disable it"
        );
    } else {
        error!(error = %e, "qa-catalog: {pass} failed");
    }
}

/// Supervise the spawned tickers: wait for shutdown, or surface a premature
/// ticker exit (= panic / abort: the loops themselves only break on
/// cancellation) instead of idling blind until the next shutdown.
async fn supervise(
    cancel: CancellationToken,
    tasks: &CancellationToken,
    mut refresher: Option<JoinHandle<()>>,
    mut gc: JoinHandle<()>,
) -> anyhow::Result<()> {
    let premature: Option<(&'static str, Result<(), tokio::task::JoinError>)> = tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        res = &mut gc => Some(("bundle-gc", res)),
        res = join_optional(&mut refresher) => Some(("branch-cache-refresher", res)),
    };

    tasks.cancel();

    // Drain whichever tickers are still running (the prematurely-fired one,
    // if any, has already yielded its join result and must not be re-polled).
    let Some((name, res)) = premature else {
        if let Some(handle) = refresher.take() {
            log_ticker_exit("branch-cache-refresher", handle.await);
        }
        log_ticker_exit("bundle-gc", gc.await);
        return Ok(());
    };

    if name == "bundle-gc" {
        if let Some(handle) = refresher.take() {
            log_ticker_exit("branch-cache-refresher", handle.await);
        }
    } else {
        log_ticker_exit("bundle-gc", gc.await);
    }
    match res {
        Ok(()) => Err(anyhow::anyhow!(
            "qa-catalog {name} task exited unexpectedly"
        )),
        Err(join_err) => Err(anyhow::anyhow!(
            "qa-catalog {name} task panicked or was aborted: {join_err}"
        )),
    }
}

/// Await an optional ticker handle; pends forever when the task was never
/// spawned (disabled refresher) so the `select!` arm simply never fires.
async fn join_optional(handle: &mut Option<JoinHandle<()>>) -> Result<(), tokio::task::JoinError> {
    match handle.as_mut() {
        Some(h) => h.await,
        None => std::future::pending().await,
    }
}

fn log_ticker_exit(name: &str, res: Result<(), tokio::task::JoinError>) {
    match res {
        Ok(()) => debug!("qa-catalog {name} task drained"),
        Err(e) => error!(error = %e, "qa-catalog {name} task join failed during shutdown"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MIN_BRANCH_REFRESH_INTERVAL_SECONDS, QaCatalog, QaCatalogRuntime,
        effective_branch_refresh_interval,
    };
    use crate::domain::system_actor;
    use crate::test_support::{build_services_tenant_scoped, inmem_db, seed_expired_bundle};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    /// A pre-cancelled bundle GC pass purges nothing.
    ///
    /// This is the discriminating case for the check `run_bundle_gc` added
    /// (Task 18, review finding #31): with the token already cancelled
    /// *before* the loop is ever entered, a check at the top of the
    /// per-tenant loop skips the one seeded tenant with zero purges, while a
    /// check placed after `purge_one_tenants_bundles` would still purge that
    /// tenant once before ever consulting the token. A token cancelled mid-loop
    /// (as a side effect of the first purge) cannot tell the two placements
    /// apart -- see qa-environments' `a_cancelled_observation_cycle_stops_between_environments`
    /// for the same distinction, made explicit there.
    #[tokio::test]
    async fn a_pre_cancelled_bundle_gc_purges_nothing() {
        let db = inmem_db().await;
        let tenant = Uuid::new_v4();
        seed_expired_bundle(&db, tenant, -time::Duration::seconds(1)).await;

        let services = build_services_tenant_scoped(db);
        let rt = QaCatalogRuntime {
            services: services.clone(),
            branch_refresh_interval_seconds: 0,
        };

        let cancel = CancellationToken::new();
        cancel.cancel();

        QaCatalog::run_bundle_gc(&rt, &cancel).await;

        let tenants = services
            .bundles
            .tenants_with_expired_bundles(&system_actor::for_bundle_gc())
            .await
            .expect("enumeration");
        assert_eq!(
            tenants,
            vec![tenant],
            "a pre-cancelled pass must purge nothing -- the seeded expired bundle must still be \
             there"
        );
    }

    #[test]
    fn zero_still_disables_the_refresher() {
        assert_eq!(
            effective_branch_refresh_interval(0),
            0,
            "0 must stay 0 (disabled), never be clamped up into an enabled ticker"
        );
    }

    #[test]
    fn sub_floor_intervals_are_clamped_to_the_floor() {
        for configured in [1, 5, 59] {
            assert_eq!(
                effective_branch_refresh_interval(configured),
                MIN_BRANCH_REFRESH_INTERVAL_SECONDS,
                "{configured}s must clamp to the floor"
            );
        }
    }

    #[test]
    fn at_or_above_the_floor_is_honored_verbatim() {
        for configured in [MIN_BRANCH_REFRESH_INTERVAL_SECONDS, 900, 86_400] {
            assert_eq!(effective_branch_refresh_interval(configured), configured);
        }
    }

    // -----------------------------------------------------------------------
    // What `init` wires, asserted against `init`'s own source
    // -----------------------------------------------------------------------
    //
    // A source scan rather than a call, and for the reason the sibling gears'
    // equivalents exist: `init` needs a database, a `ClientHub` and two
    // resolved cross-gear clients, so standing one up here would be an
    // integration harness for a two-line property. What has to be true is
    // textual anyway -- *one* adapter is built, it reaches the registry, and no
    // runtime switch guards it.

    /// The body of `Gear::init`, comments stripped.
    ///
    /// # Two hazards, both handled rather than hoped away
    ///
    /// * **Comments are stripped first**, because `init`'s own justification
    ///   for not consulting a metrics switch says the words `metrics.enabled`,
    ///   and a naive scan would trip on the comment explaining why the thing it
    ///   looks for is absent. `no_api_in_domain_tests` documents the same trap
    ///   and solves it the same way.
    /// * **The slice ends at the first method-closing brace**, not at the end
    ///   of the file. The assertions below include a *negative* one, and an
    ///   absence assertion is only as narrow as the text it is made over: a
    ///   slice that ran on into the rest of the file would start answering
    ///   questions about code `init` does not contain.
    fn init_source() -> String {
        let source = include_str!("gear.rs");
        let start = source
            .find("async fn init(&self, ctx: &GearCtx)")
            .expect("gear.rs must declare Gear::init");
        let body = &source[start..];
        let end = body
            .find("\n    }\n")
            .expect("init must be closed by a brace at method indentation");
        body[..end]
            .lines()
            .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **`init` builds exactly one metrics adapter and hands it to the plugin
    /// registry, with no switch of its own.**
    ///
    /// Three claims, and the third is the one that is easy to get wrong later.
    /// `build_default_adapter` reads the process-global meter provider, which
    /// `toolkit`'s telemetry init leaves as the built-in no-op when metrics are
    /// off or were never configured — so a `metrics.enabled` branch here would
    /// be a second, independent switch that can disagree with the first, and
    /// the disagreement's symptom is a gear that exports nothing while the
    /// deployment believes telemetry is on.
    ///
    /// **Two consumers now, still one adapter.** The plugin registry emits
    /// plugin-resolution samples; `BundlesService` emits the anonymous
    /// download route's access-control counter. Both are handed the SAME
    /// `Arc`, narrowed to the trait each needs — so a deployment builds one
    /// meter and the two families cannot end up on different providers.
    #[test]
    fn init_installs_exactly_one_metrics_adapter_into_the_plugin_registry() {
        let init = init_source();

        assert_eq!(
            init.matches("build_default_adapter()").count(),
            1,
            "init must build the adapter once and share the Arc"
        );
        assert!(
            init.contains("Some(Arc::clone(&metrics) as Arc<dyn PluginResolutionMetrics>)"),
            "the adapter init built must reach QaProductRegistry::new, or nothing in this \
             gear emits a plugin-resolution sample"
        );
        assert!(
            init.contains("bundle_download_metrics: metrics as Arc<dyn BundleDownloadMetrics>"),
            "the SAME adapter must reach BundlesService, or the anonymous download route's \
             signature refusals -- the only operator-visible difference between 'no secret \
             configured' and 'someone is guessing' -- are counted nowhere"
        );
        assert!(
            !init.contains("metrics.enabled"),
            "init must not carry a metrics switch of its own: the global meter provider is \
             already the no-op when telemetry is off, and a second switch is a second thing \
             that can disagree"
        );
    }

    /// **The slice above covers exactly one method.**
    ///
    /// Pins the width by the property rather than by a length, so a rewritten
    /// `init` cannot silently widen the negative assertion above. `impl Gear
    /// for QaCatalog` is a *trait* impl, so no ad-hoc method can be added to
    /// it — only a method the `Gear` trait itself grows and this gear
    /// implements after `init` would widen the old slice — but that is a fact
    /// about today's trait, not a guarantee, and this is the assertion that
    /// survives it changing.
    #[test]
    fn the_init_slice_covers_exactly_one_method() {
        assert_eq!(
            init_source().matches(" fn ").count(),
            1,
            "init_source must not reach a second method declaration"
        );
    }
}
