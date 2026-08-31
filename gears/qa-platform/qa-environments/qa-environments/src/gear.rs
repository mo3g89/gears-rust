//! Composition root: `#[toolkit::gear]` bootstrap, `Gear::init`, the
//! `DatabaseCapability`/`RestApiCapability` implementations, and the stateful
//! lifecycle entry (`serve`) hosting the background observation ticker
//! (Task 8).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use sea_orm_migration::MigrationTrait;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use toolkit_db::DbError;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

#[cfg(feature = "platform-observation")]
use std::future::Future;
#[cfg(feature = "platform-observation")]
use std::time::Duration;
#[cfg(feature = "platform-observation")]
use tokio::task::JoinSet;
#[cfg(feature = "platform-observation")]
use tracing::{error, warn};

use authz_resolver_sdk::AuthZResolverClient;
use credstore_sdk::CredStoreClientV1;
use qa_environments_sdk::QaEnvironmentsClientV1;

use crate::api::rest::routes;
use crate::config::QaEnvironmentsConfig;
use crate::domain::local_client::QaEnvironmentsLocalClient;
use crate::domain::ports::PlatformObserver;
#[cfg(not(feature = "platform-observation"))]
use crate::domain::ports::NoopObserver;
use crate::domain::service::AppServices;
use crate::infra::storage::{OrmLeasesRepository, OrmPlatformsRepository, OrmVariablesRepository};

/// Build the [`PlatformObserver`] collaborator: a real Kubernetes-backed
/// `KubeObserver` when this build carries the `platform-observation` cargo
/// feature, or [`NoopObserver`] otherwise — the same build-time selection
/// this crate already uses everywhere that feature gates a collaborator (see
/// `Cargo.toml`'s feature stanza). Named entirely in terms of the **port**
/// (`PlatformObserver`), never a concrete Kubernetes type, so a default build
/// pulls none of `infra::observer::kube_observer`'s `kube`/`k8s-openapi`
/// dependency in through this function's signature.
#[cfg(feature = "platform-observation")]
fn build_observer(cfg: &QaEnvironmentsConfig) -> Arc<dyn PlatformObserver> {
    Arc::new(crate::infra::observer::KubeObserver::new(
        cfg.argo.kubeconfig_path.clone(),
        cfg.argo.namespace.clone(),
        cfg.argo.secret_prefix.clone(),
        cfg.argo.secret_key.clone(),
    ))
}

#[cfg(not(feature = "platform-observation"))]
fn build_observer(_cfg: &QaEnvironmentsConfig) -> Arc<dyn PlatformObserver> {
    Arc::new(NoopObserver)
}

/// Concrete instantiation of the generic `AppServices<P, V, L>` DI container,
/// wired to the `SeaORM`-backed repositories. Used by the REST handlers
/// (`api::rest::handlers`) and routes (`api::rest::routes`) to name the
/// `Extension<Arc<ConcreteAppServices>>` type they extract.
pub(crate) type ConcreteAppServices =
    AppServices<OrmPlatformsRepository, OrmVariablesRepository, OrmLeasesRepository>;

/// Main gear struct with DDD-light layout and proper `ClientHub` integration.
#[toolkit::gear(
    name = "qa-environments",
    deps = [authz_resolver, credstore],
    capabilities = [db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s")
)]
pub struct QaEnvironments {
    // Keep the domain service behind OnceLock for set-once access.
    service: OnceLock<Arc<ConcreteAppServices>>,
    /// The observation ticker's own config, read once at `init` and consumed
    /// by `serve` when it decides whether to spawn the ticker. Only exists in
    /// a build carrying the `platform-observation` cargo feature — see
    /// `QaEnvironmentsConfig::observation`'s doc for why that field itself is
    /// feature-gated.
    #[cfg(feature = "platform-observation")]
    observation: OnceLock<crate::config::ObservationConfig>,
}

impl Default for QaEnvironments {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
            #[cfg(feature = "platform-observation")]
            observation: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for QaEnvironments {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: QaEnvironmentsConfig = ctx.config_or_default()?;
        debug!(
            "Loaded qa-environments config: max_variables={}",
            cfg.max_variables
        );

        // Acquire DB capability (secure wrapper, no DbHandle exposed to gears)
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        // Cross-gear clients from ClientHub.
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;
        // A pasted kubeconfig is written here before any row is created, so
        // credstore is a hard dependency of the create/update paths — declared
        // in `deps` above exactly as qa-catalog declares it for SSH keys.
        let credstore = ctx
            .client_hub()
            .get::<dyn CredStoreClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get credstore client: {e}"))?;

        let observer = build_observer(&cfg);

        #[cfg(feature = "platform-observation")]
        self.observation
            .set(cfg.observation.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let services = Arc::new(AppServices::new(
            Arc::new(OrmPlatformsRepository),
            Arc::new(OrmVariablesRepository),
            Arc::new(OrmLeasesRepository),
            db,
            authz,
            credstore,
            observer,
            cfg.max_variables,
        ));

        self.service
            .set(services.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Register local client under the SDK trait for transport-agnostic
        // (in-process, `ClientHub`-mediated) consumption by other gears.
        ctx.client_hub()
            .register::<dyn QaEnvironmentsClientV1>(Arc::new(QaEnvironmentsLocalClient::new(
                services,
            )));

        Ok(())
    }
}

impl DatabaseCapability for QaEnvironments {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing qa-environments database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for QaEnvironments {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering qa-environments REST routes");

        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();

        let router = routes::register_routes(router, openapi, service);

        info!("qa-environments REST routes registered successfully");
        Ok(router)
    }
}

// ---------------------------------------------------------------------------
// Lifecycle (stateful capability): the background observation ticker
// ---------------------------------------------------------------------------

impl QaEnvironments {
    /// Lifecycle entry (`stateful` capability). Spawns the background
    /// observation ticker (Task 8) when this build carries the
    /// `platform-observation` cargo feature *and* the feature has not been
    /// switched off at runtime (`qa-environments.observation.enabled`).
    ///
    /// # Two independent "off" switches, and each one says which it is
    ///
    /// A deployment that expects platforms to refresh on their own but got
    /// neither the cargo feature nor the config flag would otherwise have no
    /// way to tell "not built in" from "built in, but disabled" apart — both
    /// look like a gear that silently never refreshes anything. So each has
    /// its own log line: this method's own branch when the feature was never
    /// compiled in, and `observation.enabled == false`'s branch below when it
    /// was but the config says not to run it.
    ///
    /// # Errors
    ///
    /// Returns `Err` only when the ticker exits before it was cancelled —
    /// whether it panicked, was aborted, or returned of its own accord.
    /// Cooperative shutdown returns `Ok(())`. See [`supervise`]'s doc for why
    /// that distinction matters here exactly as it does in `qa-runs` and
    /// `qa-insights`, whose `Tickers`/`Ticker`/`supervise` shape this
    /// duplicates rather than reinvents.
    pub(crate) async fn serve(self: Arc<Self>, cancel: CancellationToken) -> anyhow::Result<()> {
        let Some(services) = self.service.get().cloned() else {
            info!("qa-environments: serve() with no service (uninitialized); idling until cancelled");
            cancel.cancelled().await;
            return Ok(());
        };

        self.serve_with_services(services, cancel).await
    }

    /// The feature-off half of [`Self::serve`]: this build has no
    /// `PlatformObserver` beyond [`crate::domain::ports::NoopObserver`], so
    /// there is no ticker to spawn — only a single log line saying so, then
    /// idling until shutdown.
    #[cfg(not(feature = "platform-observation"))]
    async fn serve_with_services(
        &self,
        _services: Arc<ConcreteAppServices>,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        info!(
            "qa-environments: built without the `platform-observation` cargo feature; the \
             background observation ticker does not exist in this build, and no platform \
             will ever refresh on its own - use POST /qa/v1/platforms/{{id}}/refresh, or \
             rebuild with the feature enabled"
        );
        cancel.cancelled().await;
        Ok(())
    }

    /// The feature-on half of [`Self::serve`]: spawns the observation ticker
    /// unless `qa-environments.observation.enabled` says not to, logging
    /// which of the two applies.
    #[cfg(feature = "platform-observation")]
    #[allow(
        clippy::cognitive_complexity,
        reason = "one log line per branch (enabled/disabled/nothing-enabled), and the \
                  branches are the point - same diagnosis as qa-runs'/qa-insights' `serve`"
    )]
    async fn serve_with_services(
        &self,
        services: Arc<ConcreteAppServices>,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        let observation = self.observation.get().cloned().unwrap_or_else(|| {
            warn!(
                "qa-environments: serve() reached with no observation config recorded \
                 (init() did not run, or ran before this field was added); using the \
                 compiled-in default rather than refusing to start"
            );
            crate::config::ObservationConfig::default()
        });

        let tasks = cancel.child_token();
        let mut tickers = Tickers::default();

        if observation.enabled {
            let interval_seconds = observation.effective_poll_interval_seconds();
            let role = tickers.spawn(Self::observation_ticker(&services, interval_seconds, &tasks));
            info!(interval_seconds, role, "qa-environments observation ticker started");
        } else {
            info!(
                "qa-environments: observation ticker disabled by config \
                 (qa-environments.observation.enabled=false); no platform will refresh on its \
                 own until this is switched back on or POST /qa/v1/platforms/{{id}}/refresh is \
                 called"
            );
        }

        if tickers.is_empty() {
            info!("qa-environments: no lifecycle ticker is enabled; idling until cancelled");
            cancel.cancelled().await;
            return Ok(());
        }

        supervise(cancel, &tasks, tickers).await
    }

    /// Spawn the observation ticker: list every platform, observe each
    /// against its own cluster, persist the outcome, and self-heal its
    /// kubeconfig `Secret` — one cycle per `interval_seconds`, forever, until
    /// cancelled. The cycle's body
    /// ([`crate::domain::service::PlatformsService::run_observation_cycle`],
    /// reached through `services.platforms`) owns "one platform's failure
    /// never aborts the cycle for the others"; this loop owns only the
    /// timing and the shutdown path.
    ///
    /// No leader election, unlike `qa-runs`' and `qa-insights`' tickers: every
    /// unit of work here is idempotent by construction —
    /// `PlatformsRepository::record_observation`'s merge rules converge
    /// regardless of which replica writes them, and the self-heal's
    /// server-side apply converges the same `Secret` to the same content no
    /// matter how many replicas apply it. Two replicas both observing every
    /// platform every cycle is redundant work, not a correctness hazard, so
    /// there is nothing here for an elector to buy.
    #[cfg(feature = "platform-observation")]
    fn observation_ticker(
        services: &Arc<ConcreteAppServices>,
        interval_seconds: u64,
        tasks: &CancellationToken,
    ) -> Ticker<impl Future<Output = ()> + Send + 'static> {
        // Bound once, so the value `Tickers` files this task under is the
        // same value the started-log line above already printed — see
        // `Tickers`' own doc for why the role travels with the loop rather
        // than being supplied beside it.
        let role = OBSERVATION_ROLE;
        let services = Arc::clone(services);
        let period = Duration::from_secs(interval_seconds);
        let cancel = tasks.clone();

        let run = async move {
            let mut ticker = tokio::time::interval(period);
            // A tick missed because the previous cycle ran long is not made
            // up in a burst that re-observes every platform several times
            // over — matches `qa-runs`' dispatcher/scheduler and
            // `qa-insights`' three tickers.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("qa-environments observation ticker stopping (shutdown)");
                        return;
                    }
                    _ = ticker.tick() => {
                        let report = services.platforms.run_observation_cycle().await;
                        debug!(
                            attempted = report.attempted,
                            observed = report.observed,
                            failed = report.failed,
                            "qa-environments observation cycle"
                        );
                    }
                }
            }
        };
        Ticker { role, run }
    }
}

/// The role name the observation ticker holds — echoed in its started-log
/// line and filed under it in [`Tickers`], so an operator grepping for a
/// stuck ticker finds both.
#[cfg(feature = "platform-observation")]
const OBSERVATION_ROLE: &str = "qa-environments-observation";

/// A ticker that is ready to spawn: the role it holds, and the loop that holds
/// it.
///
/// Copied from `qa-runs`' and `qa-insights`' `gear.rs` (their own `Ticker`
/// doc has the full argument for why the role travels *with* the loop rather
/// than being supplied beside it — a panicking ticker must still be
/// nameable, and the first two attempts at this in `qa-runs` each lost that
/// on one arm or the other). Only one ticker exists in this gear today, but
/// the shape stays because it is the shape [`supervise`] is written against.
#[cfg(feature = "platform-observation")]
struct Ticker<F> {
    role: &'static str,
    run: F,
}

/// The spawned tickers, and which role each spawned task is.
///
/// See `qa-runs`' `gear.rs`' `Tickers` doc for the full "why" — the role has
/// to come from the task id (via [`Self::spawn`]'s bookkeeping), not from the
/// future's own output, so a panicking ticker is still named on the `Err`
/// arm of a join.
#[cfg(feature = "platform-observation")]
#[derive(Default)]
struct Tickers {
    set: JoinSet<()>,
    roles: Vec<(tokio::task::Id, &'static str)>,
}

#[cfg(feature = "platform-observation")]
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

    /// The next ticker to finish, with the role it was, or `None` when the
    /// set is empty.
    async fn join_next(&mut self) -> Option<(&'static str, Result<(), tokio::task::JoinError>)> {
        match self.set.join_next_with_id().await {
            None => None,
            Some(Ok((id, ()))) => Some((self.role_of(id), Ok(()))),
            Some(Err(error)) => Some((self.role_of(error.id()), Err(error))),
        }
    }

    /// The role that task is, or a placeholder.
    ///
    /// No path in this module reaches the placeholder: [`Self::spawn`] is the
    /// only thing that inserts into [`Self::set`], and it records the pair
    /// every time.
    fn role_of(&self, id: tokio::task::Id) -> &'static str {
        self.roles
            .iter()
            .find(|(task, _)| *task == id)
            .map_or("unidentified", |(_, role)| *role)
    }
}

/// Wait for shutdown, or surface a premature exit by the ticker.
///
/// The ticker's loop returns only on cancellation, so a join before `cancel`
/// fires means it panicked, was aborted, or returned on its own — all three
/// are the same fault to an operator and all three are reported.
///
/// An empty set makes this return `Ok(())` at once rather than waiting for
/// cancellation, which is why [`QaEnvironments::serve`] handles "nothing
/// enabled" itself instead of passing an empty set here.
#[cfg(feature = "platform-observation")]
#[allow(
    clippy::cognitive_complexity,
    reason = "one log line per shutdown path, and the paths are the point - copied from \
              qa-runs'/qa-insights' `supervise`, which carries the same allow for the same \
              reason"
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
                Ok(()) => debug!(role, "qa-environments ticker drained"),
                Err(e) => error!(
                    role,
                    error = %e,
                    "qa-environments ticker join failed during shutdown",
                ),
            }
        }
        return Ok(());
    };

    match outcome {
        Ok(()) => Err(anyhow::anyhow!("qa-environments {role} task exited unexpectedly")),
        Err(join_err) => Err(anyhow::anyhow!(
            "qa-environments {role} task panicked or was aborted: {join_err}"
        )),
    }
}
