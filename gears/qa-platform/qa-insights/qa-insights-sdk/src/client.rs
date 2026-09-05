//! Object-safe client trait for inter-gear consumption via `ClientHub`.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::QaInsightsError;
use crate::models::SkipListEntry;

/// Object-safe client for the qa-insights gear (Version 1).
///
/// Registered in `ClientHub`:
/// ```ignore
/// let insights = hub.get::<dyn QaInsightsClientV1>()?;
/// ```
///
/// Sole consumer: qa-runs' launch path.
///
/// # Why there is exactly one method
///
/// This gear's surface is large — ingest, dashboard, coverage, nine analytics
/// sections, saved views, settings, the bug endpoints, notifications — and
/// almost none of it is called by another *gear*. It is called by a UI, over
/// REST. That includes the collect report, which the runner `POST`s to
/// `VHP_COLLECT_URL` and which is therefore a route rather than an SDK call.
///
/// An SDK method with no cross-gear caller is pure overhead, and it is exactly
/// the argument `cpt-cf-qa-adr-four-gear-decomposition` uses against its
/// six-gear option. Resist adding the analytics surface here.
#[async_trait]
pub trait QaInsightsClientV1: Send + Sync {
    /// Open bugs for a plan, as `(test_name, jira_key)` pairs, for the runner's
    /// `SKIP_TESTS_WITH_BUGS` variable.
    ///
    /// qa-runs calls this at launch when the run requests skip-tests-with-bugs,
    /// and renders the result into the environment. See [`SkipListEntry`] for
    /// the rendering and — importantly — for what an **empty** result must
    /// produce, which is an absent variable and not an empty one.
    ///
    /// # The plan is `(repo_id, plan_path)`, not a `plan_id`
    ///
    /// The plan's draft signature is `skip_list_for(ctx, plan_id: Uuid)`. There
    /// is no such value: plans are not persisted in this port, so no plan UUID
    /// is ever minted, and legacy's own `jira_bugs.plan_id` is a path-derived
    /// slug in a `TEXT` column (`manager/src/services/plans.rs:789-801`), not a
    /// UUID. The pair used here is the same identity
    /// `qa_catalog_sdk::UniverseTest` and `qa_runs_sdk::RunTarget::Plan` already
    /// carry, so the caller has both values in hand at launch and no lookup is
    /// added. The full argument is in `crate::models`' header, note 1.
    ///
    /// # Legacy equivalence
    ///
    /// `JiraService::get_open_bugs` (`manager/src/services/jira.rs:220-229`) —
    /// `WHERE plan_id = $1 AND status = 'Open'`. Only *open* bugs are returned.
    ///
    /// It has **two** call sites, and only one of them is this method:
    ///
    /// 1. `manager/src/routes/runs.rs:753`, inside `submit_plan_run`
    ///    (`runs.rs:533`) — the launch path, which renders the result into
    ///    `SKIP_TESTS_WITH_BUGS`. That is what this method serves.
    /// 2. `manager/src/routes/settings.rs:640`, inside `api_open_bugs`
    ///    (`:634`, routed at `routes/mod.rs:298`) — a plain read serving
    ///    `GET /api/jira/open-bugs?plan_id=…`, which falls back to
    ///    `get_all_open_bugs` when no plan is named (`settings.rs:641-643`).
    ///
    /// **Correction, 2026-08-20 (Task 8 review).** The first draft of this doc
    /// cited only site 1 and read as though it were the only caller. It is not.
    /// The conclusion it was supporting survives unchanged — site 2 is a REST
    /// read, not a launch path — but the evidence was wrong, and the second site
    /// is a **forward requirement, not a curiosity**: this gear must serve
    /// `GET /qa/v1/jira/open-bugs` itself (design §4.6, plan Task 33), so the
    /// same query needs a REST route as well as this method. Recorded here so
    /// Tasks 31-35 do not rediscover it.
    ///
    /// **Only plan runs consult the launch path**: legacy's custom-plan launch
    /// never calls `get_open_bugs`, so a custom-plan run has no skip list at
    /// all. That is preserved by this signature, which cannot express a custom
    /// plan.
    ///
    /// # Errors
    ///
    /// Returns [`QaInsightsError`] if the caller is not authorized for the
    /// tenant, or if the registry cannot be read. A plan with no bugs — or one
    /// that does not exist — is an empty list, not an error: legacy's query
    /// returns no rows for both and the launch proceeds either way.
    async fn skip_list_for(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        plan_path: &str,
    ) -> Result<Vec<SkipListEntry>, QaInsightsError>;
}
