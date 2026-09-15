//! The whole read path, driven through a really-booted gear.
//!
//! # What this file proves that no `#[cfg(test)]` module in `src/` can
//!
//! Every unit below this is green in isolation. What was never covered is the
//! *composition root*: `gear::QaInsights::init` resolving its clients and
//! `QaInsights::serve` starting the leader-elected tickers under the
//! cancellation token. Those two are the only code in the crate a unit test
//! cannot reach — `gear.rs`'s own test module says so in as many words ("there
//! is no test that `init` wires correctly, and that is a real gap") — and they
//! are exactly what Task 40 adds. So this test boots the real gear:
//!
//! 1. the real [`Gear::init`], against a real in-memory database carrying this
//!    gear's real migrations and a real [`ClientHub`] holding five registered
//!    clients;
//! 2. the real lifecycle entry, reached through the **generated**
//!    [`toolkit::lifecycle::Runnable`] impl that `#[toolkit::gear(lifecycle(entry
//!    = "serve", ..))]` emits — the same call the runtime makes, not a
//!    hand-rolled substitute;
//! 3. the real [`RestApiCapability::register_rest`], driven over HTTP,
//!    including `POST /qa/v1/insights/rebuild` — the operator's ingest path.
//!
//! # There is no consumer here, and that is the point of this revision
//!
//! Until this change this file also drove a [`MockBroker`] publishing a run's
//! lifecycle events and asserted the transactional consumer `serve` started
//! projected them. The consumer and the `event-broker` dependency it needed are
//! gone: no deployment ever registered an `EventBrokerApi` client, qa-runs
//! deleted the publisher that would have fed it, and the reconcile sweep was
//! already carrying every deployment's real ingest — see `gear.rs`'s header,
//! "Event ingest, and why there is only one path". The two tests that used to
//! cross "broker registered or not" against "tickers on or off" collapse to one
//! axis now that there is no broker to cross against; what they proved about a
//! *missing* client no longer applies to a client this gear no longer resolves.
//!
//! What replaces the consumer-driven assertion is the ingest path that is
//! actually left: [`a_rebuilt_run_reaches_the_dashboard_and_the_overview`]
//! drives `POST /qa/v1/insights/rebuild` over HTTP against the real composition
//! root, exactly as an operator recovering a fresh or gapped tenant would, and
//! asserts the same two read surfaces the deleted test did — the dashboard and
//! the analytics overview.
//!
//! ## The reconciler is not what either test exercises
//!
//! [`TenantScopedAuthZ`] refuses the nil-tenant enumeration the reconcile ticker
//! needs to find a tenant to sweep (`domain::service::tenants`' header), so a
//! ticker's first tick logs and does nothing under this harness — the same
//! coincidence fix round 1 recorded when this file still had a consumer to keep
//! separate from the sweep. `rebuild` needs no enumeration at all: it takes the
//! tenant from the caller's own [`SecurityContext`], which is why it is usable
//! here without touching that double.
//!
//! # The two tests
//!
//! | | tickers off | tickers on |
//! |---|---|---|
//! | | [`a_rebuilt_run_reaches_the_dashboard_and_the_overview`] | [`the_lifecycle_starts_every_enabled_ticker_and_shuts_them_down_cleanly`] |
//!
//! The left column drives ingest and both read surfaces with the tickers off,
//! so nothing but the rebuild call can be responsible for what lands. The right
//! column is the **only** test that reaches `QaInsights::reconcile_ticker`,
//! `::jira_poller_ticker` and `::collect_ticker` at all, and the only one that
//! drives `supervise`'s drain path with a non-empty set; it pins that three
//! leader-gated tickers spawn without panicking and that `serve` returns `Ok`
//! on cooperative shutdown, and asserts nothing about whether a tick did work,
//! for the same reason the left column's ticker is off.
//!
//! # Why it is an integration target and not a module
//!
//! An integration target compiles the library **without** `cfg(test)`, which is
//! the property `qa-runs/qa-runs/tests/mock_executor_control_surface.rs`'s own
//! header argues for at length: everything inside `src/` is compiled *with*
//! `cfg(test)` when it is tested, so nothing in there can detect a surface that
//! only exists under it. Concretely, this file gets no
//! `domain::service::test_support` and no `infra::storage::test_db` — both are
//! `#[cfg(test)]` — so its doubles, its database and its fixtures are built here
//! from the crate's real public API and `toolkit_db`'s real public helpers.
//!
//! Three consequences worth stating rather than discovering:
//!
//! * The four cross-gear client doubles below implement the **whole** SDK trait,
//!   because a trait object cannot be partially implemented. Measured
//!   2026-08-25, so the next reader does not have to grep: **65 methods** (17
//!   qa-runs + 25 qa-catalog + 11 qa-environments + 12 oagw), of which **6** are
//!   real and **59** are `unimplemented!()`. That is the point of the shape: a
//!   wiring change that starts calling a seventh fails loudly here instead of
//!   silently reading a default. (A `grep -c 'unimplemented!('` over this file
//!   answers 60 — the sixtieth is this very sentence.)
//! * `SecurityContext` is layered onto the router by this test, because that is
//!   what the host does. In deployment `api-gateway`'s auth middleware inserts
//!   it (`domain::system_actor`'s header records the finding); `register_rest`
//!   itself has never inserted one and must not.
//! * The file is named `ingest_idempotence.rs` and tests reachability, not
//!   idempotence. The name is the plan's (`Task 40`'s `**Files:**` line) and is
//!   kept so the plan and the tree agree.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use http::{Request, StatusCode};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use qa_catalog_sdk::{QaCatalogClientV1, SOURCE_REPO, UniverseTest};
use qa_environments_sdk::{Environment, QaEnvironmentsClientV1};
use qa_runs_sdk::{
    ExclusiveTier, QaRunsClientV1, Run, RunSource, RunState, RunTarget, RunTestResult,
};
use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
use toolkit::config::ConfigProvider;
use toolkit::lifecycle::Runnable;
use toolkit::{ClientHub, Gear, GearCtx, RestApiCapability};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use toolkit_security::{SecurityContext, pep_properties};

use qa_insights::QaInsights;

// ---------------------------------------------------------------------------
// Fixture identities
// ---------------------------------------------------------------------------

const TENANT: Uuid = Uuid::from_u128(0x0A);
const RUN_ID: Uuid = Uuid::from_u128(0xBEEF);
const REPO_ID: Uuid = Uuid::from_u128(0x30);
const PLATFORM_ID: Uuid = Uuid::from_u128(0x31);
const PLAN_PATH: &str = "plans/smoke.yaml";
/// The overview's required `product_id`. This gear parses it as a `Uuid` and
/// hands it to `CatalogReader::list_universe`; the double ignores it, which is
/// the honest shape — a product filter is qa-catalog's to apply.
const PRODUCT_ID: Uuid = Uuid::from_u128(0x40);
/// The fixture run's `app_version`, and therefore the overview's required
/// `version` predicate.
const APP_VERSION: &str = "9.1.0";

// ---------------------------------------------------------------------------
// The AuthZ double
// ---------------------------------------------------------------------------

/// Grants, and compiles to a real `owner_tenant_id IN [subject_tenant]` scope.
///
/// A transcription of `domain::service::test_support::permissive_response`,
/// which this target cannot import because it is `#[cfg(test)]`. Kept
/// behaviourally identical on purpose: a scope this test compiled *differently*
/// from the one every unit test compiles would make a tenancy defect look like a
/// harness defect. `AccessScope::allow_all()` appears nowhere in this crate,
/// tests included, and this is not the file that starts.
struct TenantScopedAuthZ;

#[async_trait]
impl AuthZResolverApi for TenantScopedAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let root_id = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tc| tc.root_id)
            .or_else(|| {
                request
                    .subject
                    .properties
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            .filter(|id| !id.is_nil());

        let constraints = root_id.map_or_else(Vec::new, |id| {
            vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    [id],
                ))],
            }]
        });

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                ..Default::default()
            },
        })
    }
}

// ---------------------------------------------------------------------------
// The config provider
// ---------------------------------------------------------------------------

/// A `qa-insights` section carrying exactly one knob: `enable_tickers`.
///
/// Everything else is `QaInsightsConfig::default()`, because the struct is
/// `#[serde(default)]` — so this is still "the configuration a deployment that
/// sets nothing gets", with one switch flipped where a test needs it.
///
/// # Why the switch exists, added in fix round 1
///
/// [`a_rebuilt_run_reaches_the_dashboard_and_the_overview`] needs the rebuild
/// call to be the **only** thing that can satisfy its assertions, and the
/// reconciler reads the same qa-runs double: with the tickers on, a slow test
/// run could let a reconcile pass race the rebuild. `enable_tickers: false` is
/// what makes that structural rather than accidental.
struct TickerSwitch {
    section: serde_json::Value,
}

impl TickerSwitch {
    fn new(enable_tickers: bool) -> Self {
        // `gear_config_or_default` reads `provider.get_gear_config(name)` and then
        // its `"config"` field (`libs/toolkit/src/config.rs`, the four
        // fall-back-to-default arms), so the section has to carry that wrapper.
        Self {
            section: serde_json::json!({ "config": { "enable_tickers": enable_tickers } }),
        }
    }
}

impl ConfigProvider for TickerSwitch {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        Some(&self.section)
    }
}

// ---------------------------------------------------------------------------
// The cross-gear client doubles
// ---------------------------------------------------------------------------

/// The qa-runs double: one finished run and its result rows.
///
/// Answers `get_run`, `list_run_test_results`, `list_runs` and
/// `list_runs_finished_since`; everything else is unreachable on this path and
/// says so.
struct FakeQaRuns {
    run: Run,
    rows: Vec<RunTestResult>,
}

#[async_trait]
impl QaRunsClientV1 for FakeQaRuns {
    async fn launch(
        &self,
        _ctx: &SecurityContext,
        _req: qa_runs_sdk::LaunchRequest,
    ) -> Result<qa_runs_sdk::LaunchOutcome, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn get_run(
        &self,
        _ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Run, qa_runs_sdk::QaRunsError> {
        assert_eq!(id, self.run.id, "only the fixture run is ever read");
        Ok(self.run.clone())
    }

    async fn list_runs(
        &self,
        _ctx: &SecurityContext,
        _limit: u32,
    ) -> Result<Vec<Run>, qa_runs_sdk::QaRunsError> {
        Ok(vec![self.run.clone()])
    }

    async fn get_run_result(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<qa_runs_sdk::RunResult, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn list_runs_finished_since(
        &self,
        _ctx: &SecurityContext,
        _since: OffsetDateTime,
        _limit: u32,
    ) -> Result<Vec<Run>, qa_runs_sdk::QaRunsError> {
        Ok(vec![self.run.clone()])
    }

    async fn list_run_test_results(
        &self,
        _ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Vec<RunTestResult>, qa_runs_sdk::QaRunsError> {
        assert_eq!(run_id, self.run.id, "only the fixture run is ever read");
        Ok(self.rows.clone())
    }

    async fn cancel_run(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<Run, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn rerun(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<qa_runs_sdk::LaunchOutcome, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn list_queue(
        &self,
        _ctx: &SecurityContext,
        _environment_id: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<qa_runs_sdk::QueueEntry>, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn cancel_queued(
        &self,
        _ctx: &SecurityContext,
        _queue_id: Uuid,
    ) -> Result<(), qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn force_start_queued(
        &self,
        _ctx: &SecurityContext,
        _queue_id: Uuid,
    ) -> Result<Run, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn list_schedules(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<qa_runs_sdk::Schedule>, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn get_schedule(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<qa_runs_sdk::Schedule, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn create_schedule(
        &self,
        _ctx: &SecurityContext,
        _new: qa_runs_sdk::NewSchedule,
    ) -> Result<qa_runs_sdk::Schedule, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn update_schedule(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _new: qa_runs_sdk::NewSchedule,
    ) -> Result<qa_runs_sdk::Schedule, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn delete_schedule(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }

    async fn update_schedule_notifications(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _settings: qa_runs_sdk::ScheduleNotificationSettings,
    ) -> Result<qa_runs_sdk::Schedule, qa_runs_sdk::QaRunsError> {
        unimplemented!("not on the ingest path")
    }
}

/// The qa-catalog double: a one-test universe that matches the projected row.
struct FakeQaCatalog {
    universe: Vec<UniverseTest>,
}

#[async_trait]
impl QaCatalogClientV1 for FakeQaCatalog {
    async fn list_repos(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<qa_catalog_sdk::TestRepository>, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn get_repo(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<qa_catalog_sdk::TestRepository, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn create_repo(
        &self,
        _ctx: &SecurityContext,
        _new: qa_catalog_sdk::NewTestRepository,
    ) -> Result<qa_catalog_sdk::TestRepository, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn update_repo(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _update: qa_catalog_sdk::TestRepositoryUpdate,
    ) -> Result<qa_catalog_sdk::TestRepository, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn delete_repo(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn sync_repo(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _req: qa_catalog_sdk::SyncRequest,
    ) -> Result<qa_catalog_sdk::TestRepository, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn list_branches(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<Vec<String>, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn list_plans(
        &self,
        _ctx: &SecurityContext,
        _repo_id: Uuid,
        _branch: &str,
    ) -> Result<Vec<qa_catalog_sdk::Plan>, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn get_plan(
        &self,
        _ctx: &SecurityContext,
        _repo_id: Uuid,
        _branch: &str,
        _path: &str,
    ) -> Result<qa_catalog_sdk::Plan, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn get_test_meta(
        &self,
        _ctx: &SecurityContext,
        _repo_id: Uuid,
        _branch: &str,
        _files: &[String],
    ) -> Result<Vec<qa_catalog_sdk::TestFileMeta>, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn list_universe(
        &self,
        _ctx: &SecurityContext,
        _product_id: Option<Uuid>,
        _branch: Option<&str>,
    ) -> Result<Vec<UniverseTest>, qa_catalog_sdk::QaCatalogError> {
        Ok(self.universe.clone())
    }

    async fn list_custom_plans(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<qa_catalog_sdk::CustomPlan>, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn get_custom_plan(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<qa_catalog_sdk::CustomPlan, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn create_custom_plan(
        &self,
        _ctx: &SecurityContext,
        _new: qa_catalog_sdk::NewCustomPlan,
    ) -> Result<qa_catalog_sdk::CustomPlan, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn update_custom_plan(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _new: qa_catalog_sdk::NewCustomPlan,
    ) -> Result<qa_catalog_sdk::CustomPlan, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn delete_custom_plan(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn list_products(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<qa_catalog_sdk::Product>, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn create_product(
        &self,
        _ctx: &SecurityContext,
        _new: qa_catalog_sdk::NewProduct,
    ) -> Result<qa_catalog_sdk::Product, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn update_product(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _update: qa_catalog_sdk::ProductUpdate,
    ) -> Result<qa_catalog_sdk::Product, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn delete_product(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn list_ssh_keys(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<qa_catalog_sdk::SshKey>, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn create_ssh_key(
        &self,
        _ctx: &SecurityContext,
        _name: String,
        _private_key_pem: String,
    ) -> Result<qa_catalog_sdk::SshKey, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn delete_ssh_key(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn create_bundle(
        &self,
        _ctx: &SecurityContext,
        _req: qa_catalog_sdk::BundleRequest,
    ) -> Result<qa_catalog_sdk::TestBundle, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }

    async fn get_bundle_content(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<Vec<u8>, qa_catalog_sdk::QaCatalogError> {
        unimplemented!("not on the read path")
    }
}

/// The qa-environments double: one platform, so the overview's group chart can
/// resolve the display name `ExecRow::environment_id` stands in for.
struct FakeQaEnvironments {
    platform: Environment,
}

#[async_trait]
impl QaEnvironmentsClientV1 for FakeQaEnvironments {
    async fn get_environment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<Environment, qa_environments_sdk::QaEnvironmentsError> {
        Ok(self.platform.clone())
    }

    async fn list_environments(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<Environment>, qa_environments_sdk::QaEnvironmentsError> {
        Ok(vec![self.platform.clone()])
    }

    async fn create_environment(
        &self,
        _ctx: &SecurityContext,
        _new: qa_environments_sdk::NewEnvironment,
    ) -> Result<Environment, qa_environments_sdk::QaEnvironmentsError> {
        unimplemented!("not on the read path")
    }

    async fn update_environment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _patch: qa_environments_sdk::EnvironmentPatch,
    ) -> Result<Environment, qa_environments_sdk::QaEnvironmentsError> {
        unimplemented!("not on the read path")
    }

    async fn delete_environment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), qa_environments_sdk::QaEnvironmentsError> {
        unimplemented!("not on the read path")
    }

    async fn list_variables(
        &self,
        _ctx: &SecurityContext,
        _environment_id: Option<Uuid>,
    ) -> Result<Vec<qa_environments_sdk::Variable>, qa_environments_sdk::QaEnvironmentsError> {
        unimplemented!("not on the read path")
    }

    async fn upsert_variable(
        &self,
        _ctx: &SecurityContext,
        _var: qa_environments_sdk::NewVariable,
    ) -> Result<qa_environments_sdk::Variable, qa_environments_sdk::QaEnvironmentsError> {
        unimplemented!("not on the read path")
    }

    async fn delete_variable(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), qa_environments_sdk::QaEnvironmentsError> {
        unimplemented!("not on the read path")
    }

    async fn acquire_lease(
        &self,
        _ctx: &SecurityContext,
        _environment_id: Uuid,
        _run_id: Uuid,
        _mode: qa_environments_sdk::LeaseMode,
    ) -> Result<qa_environments_sdk::AcquireOutcome, qa_environments_sdk::QaEnvironmentsError> {
        unimplemented!("not on the read path")
    }

    async fn release_lease(
        &self,
        _ctx: &SecurityContext,
        _environment_id: Uuid,
        _run_id: Uuid,
    ) -> Result<qa_environments_sdk::LeaseState, qa_environments_sdk::QaEnvironmentsError> {
        unimplemented!("not on the read path")
    }

    async fn get_lease(
        &self,
        _ctx: &SecurityContext,
        _environment_id: Uuid,
    ) -> Result<qa_environments_sdk::LeaseState, qa_environments_sdk::QaEnvironmentsError> {
        unimplemented!("not on the read path")
    }
}

/// The oagw double.
///
/// **Never called on this path, and that is the assertion.** `init` resolves the
/// gateway for the JIRA adapter and — from Task 40 — for the Slack adapter, and
/// neither is reached by an ingest or by a dashboard read. Every method
/// `unimplemented!()` is what makes "the read path performs no egress" a fact
/// this test would notice losing.
struct FakeGateway;

#[async_trait]
impl oagw_sdk::api::ServiceGatewayClientV1 for FakeGateway {
    async fn create_upstream(
        &self,
        _ctx: SecurityContext,
        _req: oagw_sdk::models::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::models::Upstream, toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn get_upstream(
        &self,
        _ctx: SecurityContext,
        _id: Uuid,
    ) -> Result<oagw_sdk::models::Upstream, toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn list_upstreams(
        &self,
        _ctx: SecurityContext,
        _query: &oagw_sdk::models::ListQuery,
    ) -> Result<Vec<oagw_sdk::models::Upstream>, toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn update_upstream(
        &self,
        _ctx: SecurityContext,
        _id: Uuid,
        _req: oagw_sdk::models::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::models::Upstream, toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn delete_upstream(
        &self,
        _ctx: SecurityContext,
        _id: Uuid,
    ) -> Result<(), toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn create_route(
        &self,
        _ctx: SecurityContext,
        _req: oagw_sdk::models::CreateRouteRequest,
    ) -> Result<oagw_sdk::models::Route, toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn get_route(
        &self,
        _ctx: SecurityContext,
        _id: Uuid,
    ) -> Result<oagw_sdk::models::Route, toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn list_routes(
        &self,
        _ctx: SecurityContext,
        _upstream_id: Option<Uuid>,
        _query: &oagw_sdk::models::ListQuery,
    ) -> Result<Vec<oagw_sdk::models::Route>, toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn update_route(
        &self,
        _ctx: SecurityContext,
        _id: Uuid,
        _req: oagw_sdk::models::UpdateRouteRequest,
    ) -> Result<oagw_sdk::models::Route, toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn delete_route(
        &self,
        _ctx: SecurityContext,
        _id: Uuid,
    ) -> Result<(), toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }

    async fn resolve_proxy_target(
        &self,
        _ctx: SecurityContext,
        _alias: &str,
        _method: &str,
        _path: &str,
    ) -> Result<
        (oagw_sdk::models::Upstream, oagw_sdk::models::Route),
        toolkit_canonical_errors::CanonicalError,
    > {
        unimplemented!("the read path performs no egress")
    }

    async fn proxy_request(
        &self,
        _ctx: SecurityContext,
        _req: http::Request<oagw_sdk::Body>,
    ) -> Result<http::Response<oagw_sdk::Body>, toolkit_canonical_errors::CanonicalError> {
        unimplemented!("the read path performs no egress")
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The one run this test projects: finished **now**, so every analytics window —
/// all of which end today — contains it.
fn fixture_run(finished_at: OffsetDateTime) -> Run {
    Run {
        id: RUN_ID,
        name: "smoke-1".to_owned(),
        target: RunTarget::Plan {
            repo_id: REPO_ID,
            path: PLAN_PATH.to_owned(),
        },
        environment_id: Some(PLATFORM_ID),
        test_version: Some("main".to_owned()),
        app_version: Some(APP_VERSION.to_owned()),
        app_build: Some("9.1.0-4412".to_owned()),
        state: RunState::Succeeded,
        resolved_exclusive: false,
        exclusive_tier: ExclusiveTier::Default,
        is_validation: false,
        parameters: Vec::new(),
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        source: RunSource::Manual,
        schedule_id: None,
        bundle_ids: Vec::new(),
        execution_ref: Some("wf-1".to_owned()),
        log_storage_ref: None,
        timeout_at: None,
        started_at: Some(finished_at),
        finished_at: Some(finished_at),
        error: None,
        created_at: finished_at,
        updated_at: finished_at,
    }
}

fn fixture_row(file: &str, name: &str, status: &str) -> RunTestResult {
    RunTestResult {
        run_id: RUN_ID,
        test_file: file.to_owned(),
        test_name: name.to_owned(),
        status: status.to_owned(),
        duration: Some("1.5s".to_owned()),
        launch_id: None,
        jira_key: None,
        nodeid: format!("{file}::{name}"),
        reason: None,
        ticket: None,
    }
}

fn fixture_universe_test(file: &str, name: &str) -> UniverseTest {
    UniverseTest {
        repo_id: REPO_ID,
        plan_path: PLAN_PATH.to_owned(),
        plan_name: "Smoke".to_owned(),
        test_file: file.to_owned(),
        test_name: name.to_owned(),
        title_alias: Some(name.to_owned()),
        component: None,
        tags: Vec::new(),
        quality_vectors: Vec::new(),
        source: SOURCE_REPO.to_owned(),
        versions: Vec::new(),
        static_case_count: 1,
    }
}

fn fixture_platform() -> Environment {
    Environment {
        id: PLATFORM_ID,
        name: "staging".to_owned(),
        // Required since qa-environments' Task 20b.
        product_id: uuid::Uuid::from_u128(0x9001),
        description: None,
        available: true,
        observed_version: None,
        observed_build: None,
        default_branch: None,
        is_default: false,
        version_detect_error: None,
        version_detected_at: None,
        // Nothing has been observed through the plugin path (qa-environments
        // Task 14): every value is the one a never-observed environment holds.
        credentials: Vec::new(),
        observed_attrs: qa_environments_sdk::ObservedAttrs::default(),
        config: serde_json::json!({}),
        observed_base_url: None,
        health_state: qa_environments_sdk::HealthState::Unknown,
        health_detail: None,
        health_checked_at: None,
        created_at: OffsetDateTime::now_utc(),
        updated_at: OffsetDateTime::now_utc(),
    }
}

// ---------------------------------------------------------------------------
// Wire mirrors of the response bodies
// ---------------------------------------------------------------------------

// `DashboardStatsDto`, `AnalyticsOverviewDto` and `RebuildOutcomeDto` are
// `#[api_dto(response)]`, so they are `Serialize` and **not** `Deserialize` — a
// response DTO has no reason to be read back inside the gear, and this test is
// a client. So it reads the numbers the way a client reads them: off the JSON,
// by their wire names.
//
// Deliberately narrow. Mirroring the whole DTO would be a second definition of a
// large type that could drift silently; mirroring exactly the asserted fields
// makes the coupling one thing — **the wire name** — and a rename of
// `total_runs`, `summary.passed` or `replayed` fails here, which is precisely
// the breakage a client would suffer.

/// The one field of `GET /qa/v1/dashboard` this test asserts.
#[derive(serde::Deserialize)]
struct DashboardView {
    total_runs: u64,
}

/// The one field of `GET /qa/v1/analytics/overview` this test asserts.
#[derive(serde::Deserialize)]
struct OverviewView {
    summary: OverviewSummaryView,
}

#[derive(serde::Deserialize)]
struct OverviewSummaryView {
    passed: usize,
}

/// The fields of `POST /qa/v1/insights/rebuild`'s response this test asserts.
#[derive(serde::Deserialize)]
struct RebuildOutcomeView {
    replayed: usize,
    stopped_at_gap: bool,
}

// ---------------------------------------------------------------------------
// The booted gear
// ---------------------------------------------------------------------------

/// A really-booted `qa-insights`: `init` run, the lifecycle entry spawned, the
/// router built.
struct BootedGear {
    /// The fixture run's `finished_at`, exposed so a caller can build a rebuild
    /// window around it.
    finished_at: OffsetDateTime,
    router: Router,
    cancel: CancellationToken,
    served: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(results: &[(&str, &str, &str)], enable_tickers: bool) -> BootedGear {
    // The real schema, through the real `Migrator`, on the tier every unit test
    // in this crate uses. `max_conns(1)` for `infra::storage::test_db`'s reason:
    // each SQLite `:memory:` connection is its own database.
    let db = {
        use sea_orm_migration::MigratorTrait;
        let db = toolkit_db::connect_db(
            "sqlite::memory:",
            toolkit_db::ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("in-memory sqlite connects");
        toolkit_db::migration_runner::run_migrations_for_testing(
            &db,
            qa_insights::infra::storage::migrations::Migrator::migrations(),
        )
        .await
        .expect("the qa-insights migrations apply");
        db
    };

    let finished_at = OffsetDateTime::now_utc();
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn AuthZResolverApi>(Arc::new(TenantScopedAuthZ));
    hub.register::<dyn QaRunsClientV1>(Arc::new(FakeQaRuns {
        run: fixture_run(finished_at),
        rows: results
            .iter()
            .map(|(file, name, status)| fixture_row(file, name, status))
            .collect(),
    }));
    hub.register::<dyn QaCatalogClientV1>(Arc::new(FakeQaCatalog {
        universe: results
            .iter()
            .map(|(file, name, _)| fixture_universe_test(file, name))
            .collect(),
    }));
    hub.register::<dyn QaEnvironmentsClientV1>(Arc::new(FakeQaEnvironments {
        platform: fixture_platform(),
    }));
    // The fifth and last registration. `gear.rs`'s `deps` doc lists all five as
    // mandatory now that the sixth, optional `event_broker` token is gone —
    // `init` no longer tolerates a miss on any of them.
    hub.register::<dyn oagw_sdk::api::ServiceGatewayClientV1>(Arc::new(FakeGateway));

    let cancel = CancellationToken::new();
    let ctx = GearCtx::new(
        "qa-insights",
        Uuid::new_v4(),
        Arc::new(TickerSwitch::new(enable_tickers)),
        Arc::clone(&hub),
        cancel.child_token(),
    )
    .with_db(toolkit_db::DBProvider::new(db));

    let gear = Arc::new(QaInsights::default());
    gear.init(&ctx).await.expect("the gear initializes");

    // The caller identity the host would have inserted. `register_rest` never
    // inserts one, so the test layers it exactly where `api-gateway` does.
    let caller = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(TENANT)
        .build()
        .expect("a subject and a tenant are enough");

    let openapi = OpenApiRegistryImpl::new();
    let router = gear
        .register_rest(&ctx, Router::new(), &openapi)
        .expect("the routes register")
        .layer(axum::Extension(caller));
    openapi
        .build_openapi(&OpenApiInfo::default())
        .expect("the OpenAPI document builds");

    // The generated `Runnable` impl — the same entry point the runtime drives.
    let served = tokio::spawn(Runnable::run(Arc::clone(&gear), cancel.child_token()));

    BootedGear {
        finished_at,
        router,
        cancel,
        served,
    }
}

impl BootedGear {
    /// `POST /qa/v1/insights/rebuild` with a window `[from, to)`, over HTTP
    /// against the real router — the operator's own ingest path now that there
    /// is no consumer to project a run automatically.
    async fn rebuild(&self, from: OffsetDateTime, to: OffsetDateTime) -> RebuildOutcomeView {
        use time::format_description::well_known::Rfc3339;

        let body = serde_json::json!({
            "from": from.format(&Rfc3339).expect("from formats as RFC 3339"),
            "to": to.format(&Rfc3339).expect("to formats as RFC 3339"),
        });
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/qa/v1/insights/rebuild")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("the request builds"),
            )
            .await
            .expect("the router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("the body reads");
        assert_eq!(
            status,
            StatusCode::OK,
            "POST /qa/v1/insights/rebuild answered {status}: {}",
            String::from_utf8_lossy(&bytes),
        );
        serde_json::from_slice(&bytes).expect("the response body deserializes")
    }

    async fn get_dashboard(&self) -> DashboardView {
        self.get_json("/qa/v1/dashboard").await
    }

    /// `GET /qa/v1/analytics/overview` with the **three required parameters**.
    ///
    /// `product_id`, `version` and `scope` are not optional — legacy's own
    /// `AnalyticsOverviewQuery` requires them (`analytics.rs:19-33`) and
    /// `api::rest::dto::AnalyticsOverviewQuery` ports that faithfully, so a bare
    /// `GET` on this path is a `400` and not an empty overview. Discovered the
    /// hard way while writing this test, which is the sort of thing an
    /// end-to-end reach test is for.
    ///
    /// `version` is the fixture run's `app_version`, which the projection
    /// denormalises onto every row as `product_version` — so the row this test
    /// published is inside the window *and* inside the version predicate. Get
    /// that wrong and `summary.passed` is 0 with the row sitting in the table.
    async fn get_overview(&self) -> OverviewView {
        self.get_json(&format!(
            "/qa/v1/analytics/overview?product_id={PRODUCT_ID}&version={APP_VERSION}&scope=all"
        ))
        .await
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> T {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("the request builds"),
            )
            .await
            .expect("the router answers");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("the body reads");
        assert_eq!(
            status,
            StatusCode::OK,
            "GET {path} answered {status}: {}",
            String::from_utf8_lossy(&body),
        );
        serde_json::from_slice(&body).expect("the response body deserializes")
    }

    /// Cancel the lifecycle entry and assert it shut down cooperatively.
    ///
    /// `serve` returns `Err` only when a ticker exited before cancellation, so
    /// an `Err` here is a real finding and not shutdown noise.
    async fn shutdown(self) {
        self.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(10), self.served)
            .await
            .expect("serve returns within 10s of cancellation")
            .expect("the lifecycle task did not panic")
            .expect("serve returns Ok on cooperative shutdown");
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// Rebuild a window, and assert the dashboard and the overview both see the
/// run it replayed.
///
/// Every unit below this is green in isolation; this is the test that catches a
/// wiring defect between the real composition root and the operator's ingest
/// path — the only one this gear has since the consumer was deleted.
#[tokio::test]
async fn a_rebuilt_run_reaches_the_dashboard_and_the_overview() {
    const RESULTS: [(&str, &str, &str); 1] = [("tests/a.py", "test_a", "PASSED")];

    let gear = boot(&RESULTS, false).await;

    assert_eq!(
        gear.get_dashboard().await.total_runs,
        0,
        "nothing has been rebuilt yet"
    );

    let window_from = gear.finished_at - time::Duration::seconds(60);
    let window_to = gear.finished_at + time::Duration::seconds(60);
    let outcome = gear.rebuild(window_from, window_to).await;
    assert_eq!(outcome.replayed, 1, "the one fixture run was replayed");
    assert!(
        !outcome.stopped_at_gap,
        "nothing should have failed to project"
    );

    let dashboard = gear.get_dashboard().await;
    assert_eq!(dashboard.total_runs, 1);

    let overview = gear.get_overview().await;
    assert_eq!(overview.summary.passed, 1);

    gear.shutdown().await;
}

/// The lifecycle spawns all three tickers and drains them on cancellation.
///
/// Added in fix round 1 to keep what turning the tickers off in the test above
/// would otherwise have dropped: this is the **only** test that reaches
/// `QaInsights::reconcile_ticker`, `::jira_poller_ticker` and `::collect_ticker`
/// at all, and the only one that drives `supervise`'s drain path with a
/// non-empty set.
///
/// What it pins is narrow and worth stating so nobody reads more into it: three
/// leader-gated tickers spawn without panicking, and `serve` returns **`Ok`** on
/// cooperative shutdown — which is the whole contract, because `serve` returns
/// `Err` if any ticker exited before cancellation. It does **not** assert that a
/// tick did any work: under this harness's [`TenantScopedAuthZ`] the nil-tenant
/// enumeration is refused, so each first tick logs and does nothing. That is the
/// documented state of every deployment running `static-authz-plugin`
/// (`domain::service::tenants`' header), not a harness shortcoming — and it is
/// exactly why the test above cannot rely on a ticker.
#[tokio::test]
async fn the_lifecycle_starts_every_enabled_ticker_and_shuts_them_down_cleanly() {
    const RESULTS: [(&str, &str, &str); 1] = [("tests/a.py", "test_a", "PASSED")];

    let gear = boot(&RESULTS, true).await;

    // Long enough for each ticker's immediate first tick to run and settle,
    // short enough that no ticker's 300-second second tick can fire — so what
    // is being drained is three live, idle tickers.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // `shutdown` asserts both halves: the task did not panic, and `serve`
    // returned `Ok`. With a non-empty `Tickers` set that answer comes from
    // `supervise`, not from the "nothing enabled" branch.
    gear.shutdown().await;
}
