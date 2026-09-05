//! Local adapter: `QaInsightsClientV1` over [`JiraService`].
//!
//! # Through the service, not straight to the repository
//!
//! [`JiraService::open_bugs`] already **is** legacy's `get_open_bugs(plan_id)`
//! call site ported: it compiles the caller's `qa.jira_bug`/`list` scope
//! (`JiraService::bug_scope`), applies controller ruling R85's "both present or
//! neither" pairing rule on `(repo_id, plan_path)`, and — since Task 33 — is
//! the exact method `qa_insights_sdk::client::QaInsightsClientV1::skip_list_for`'s
//! own doc names as this trait's twin call site
//! (`manager/src/routes/settings.rs:634-651`, the `GET /qa/v1/jira/open-bugs`
//! route). Going straight to [`JiraRepository::list_open_for_plan`] instead
//! would re-derive that scope compilation and that validation a second time,
//! with no test to keep the two copies from drifting — exactly the shape
//! `crate::domain::service::mod`'s own header warns against for a resource this
//! crate already gates once. There is also no PDP decision to skip: an
//! in-process caller gets the same `qa.jira_bug` grant check an HTTP caller
//! does, which is qa-runs' own `QaRunsLocalClient` header's point about its
//! sibling schedule methods.
//!
//! # `DomainError` becomes `QaInsightsError` through `as_jira_error`, not `Into`
//!
//! `qa_insights_sdk::errors` re-exports `toolkit_canonical_errors::CanonicalError`
//! as `QaInsightsError`, and `api::rest::error` is where `DomainError` becomes
//! one. But the blanket `From<DomainError> for CanonicalError` attributes a bare
//! `Validation` or `Forbidden` to the test-result resource by default — right
//! for the callers that predate the JIRA surface, wrong for this one. `as_jira_error`
//! is the renderer `api::rest::error`'s own header built for exactly this call
//! site's two refusal shapes (a `plan_path` pairing violation, a `qa.jira_bug`
//! denial), and `open_bugs`'s own doc names it as its caller's obligation. This
//! seam is qa-runs' `QaRunsLocalClient`'s `as_schedule_error`/`as_queue_error`
//! precedent, restated for this gear's one method: attribution is this
//! adapter's whole job, and a bare `.map_err(Into::into)` here would point a
//! caller fixing `plan_path` at `cf.qa.insights.test_result.v1~` instead of
//! `cf.qa.insights.jira_bug.v1~`.
//!
//! **Correction (Task 34 fix round 1): both halves are reachable, not just
//! `Forbidden`.** An earlier revision of this paragraph claimed
//! `skip_list_for`'s mandatory `repo_id: Uuid`/`plan_path: &str` parameters
//! closed off R85's `Validation` arm entirely, on the theory that both being
//! non-`Option` meant `open_bugs`'s pairing check could never see just one of
//! them. That is false, and mandatory-at-the-type-level never implied it:
//! `optional_plan_ref` (`domain/service/jira.rs:956-973`) trims `plan_path`
//! and treats an empty or whitespace-only string as **absent**
//! (`.filter(|value| !value.is_empty())`), so
//! `skip_list_for(ctx, repo_id, "")` calls
//! `open_bugs(ctx, Some(repo_id), Some(""))`, which `optional_plan_ref` sees
//! as `(Some(repo_id), None)` — the `_ => Err(Validation)` arm, not the
//! `(Some, Some)` one. `&str` is not "non-empty `&str`". Both refusal shapes
//! are therefore live, and `as_jira_error` attributes both to
//! `cf.qa.insights.jira_bug.v1~` — see this module's
//! `an_empty_plan_path_gets_a_jira_attributed_validation_error` and
//! `a_denied_caller_gets_a_jira_attributed_refusal` tests, one per shape.
//!
//! # `skip_list_entries`, not a second filter
//!
//! [`JiraRepository::list_open_for_plan`] already filters to `status = 'Open'`
//! in SQL, so [`crate::domain::jira::registry::skip_list_entries`] is redundant
//! against *that* query today. It is applied anyway, for the reason
//! `domain::jira::mod`'s own header states — this is the pure core's first
//! caller, and calling it here rather than re-deriving `SkipListEntry` by hand
//! is what keeps the open-bug predicate defined in exactly one place should a
//! future caller of `open_bugs` ever compose it with a differently-filtered
//! read.

use std::sync::Arc;

use async_trait::async_trait;
use qa_insights_sdk::{QaInsightsClientV1, QaInsightsError, SkipListEntry};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::error::as_jira_error;
use crate::domain::jira::registry::skip_list_entries;
use crate::domain::repos::{JiraRepository, ResultsRepository};
use crate::domain::service::jira::JiraService;

/// Local implementation of the object-safe `QaInsightsClientV1`.
///
/// Holds the one service the trait's one method needs, rather than the whole
/// `ConcreteAppServices` bundle qa-runs' sibling holds — `qa_insights_sdk::client`'s
/// own header explains why this gear's cross-gear surface is deliberately one
/// method, and a struct with four unused fields would be the wrong shape for
/// that. Task 40, which registers this under `dyn QaInsightsClientV1`
/// (`gear.rs`'s own reservation comment), has `services.jira` in hand at the
/// exact point `services` itself is constructed, so `Arc::clone(&services.jira)`
/// is the whole wiring change that task owes.
pub struct QaInsightsLocalClient<J, R> {
    jira: Arc<JiraService<J, R>>,
}

impl<J, R> QaInsightsLocalClient<J, R>
where
    J: JiraRepository + 'static,
    R: ResultsRepository + 'static,
{
    /// The `#[expect(dead_code)]` that guarded this constructor from Task 34 to
    /// Task 39 is **gone, and the compiler is what removed it**: Task 40's
    /// `ClientHub::register::<dyn QaInsightsClientV1>` in `gear.rs`'s `init` is
    /// the caller its reason named, so the expectation went unfulfilled — which
    /// under this workspace's `-D warnings` is a build failure. That is exactly
    /// the mechanism `gear.rs`' runtime doc describes those attributes for.
    #[must_use]
    pub(crate) fn new(jira: Arc<JiraService<J, R>>) -> Self {
        Self { jira }
    }
}

#[async_trait]
impl<J, R> QaInsightsClientV1 for QaInsightsLocalClient<J, R>
where
    J: JiraRepository + 'static,
    R: ResultsRepository + 'static,
{
    async fn skip_list_for(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        plan_path: &str,
    ) -> Result<Vec<SkipListEntry>, QaInsightsError> {
        let bugs = self
            .jira
            .open_bugs(ctx, Some(repo_id), Some(plan_path))
            .await
            .map_err(as_jira_error)?;
        Ok(skip_list_entries(&bugs))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Against a real [`OrmJiraRepository`] and [`OrmResultsRepository`] on
    //! in-memory `SQLite`, `jira_tests`' own fixture shape (that module's header
    //! explains why: a repository double would absorb exactly the scope and
    //! singleton semantics under test). `OrmResultsRepository` is a second,
    //! unused-here type parameter [`JiraService`] carries since Task 33; these
    //! tests instantiate it because [`QaInsightsLocalClient`] is generic over
    //! both, not because `skip_list_for` reads `qa_test_results`.

    use std::sync::Arc;

    use authz_resolver_sdk::PolicyEnforcer;
    use qa_insights_sdk::{JiraConfig, NewJiraBug, QaInsightsClientV1};
    use toolkit::api::canonical_prelude::Problem;
    use toolkit_db::DBProvider;
    use uuid::Uuid;

    use super::QaInsightsLocalClient;
    use crate::domain::error::DomainError;
    use crate::domain::ports::jira_client::{
        IssueRef, JiraClient, JiraIssue, NewIssue, StatusCategory,
    };
    use crate::domain::repos::JiraRepository;
    use crate::domain::service::jira::JiraService;
    use crate::domain::service::test_support::{DenyAllAuthZ, TenantScopedAuthZ, ctx};
    use crate::infra::storage::jira_sea_repo::OrmJiraRepository;
    use crate::infra::storage::results_sea_repo::OrmResultsRepository;
    use crate::infra::storage::test_db::{inmem_db, scope};

    const TENANT: Uuid = Uuid::from_u128(0x0C11_0000_0000_0001);

    fn plan_repo() -> Uuid {
        Uuid::from_u128(0x21)
    }

    const PLAN_PATH: &str = "plans/smoke/plan.yaml";

    /// `open_bugs`'s outbound port is unreachable from `skip_list_for` — the
    /// method never files, polls status, or reads an issue back — so every
    /// method panics rather than answering plausibly. A test that reached one
    /// would be exercising a code path this adapter does not have, and the
    /// panic is what makes that a loud failure instead of a silently wrong
    /// fixture.
    struct UnusedJiraClient;

    #[async_trait::async_trait]
    impl JiraClient for UnusedJiraClient {
        async fn create_or_find_issue(
            &self,
            _ctx: &toolkit_security::SecurityContext,
            _config: &JiraConfig,
            _issue: NewIssue,
        ) -> Result<IssueRef, DomainError> {
            unreachable!("skip_list_for never files a bug")
        }

        async fn check_status(
            &self,
            _ctx: &toolkit_security::SecurityContext,
            _config: &JiraConfig,
            _jira_key: &str,
        ) -> Result<StatusCategory, DomainError> {
            unreachable!("skip_list_for never polls JIRA status")
        }

        async fn get_issue(
            &self,
            _ctx: &toolkit_security::SecurityContext,
            _config: &JiraConfig,
            _jira_key: &str,
        ) -> Result<JiraIssue, DomainError> {
            unreachable!("skip_list_for never reads a single issue")
        }
    }

    fn new_bug(jira_key: &str, test_name: &str, repo_id: Uuid, plan_path: &str) -> NewJiraBug {
        NewJiraBug {
            jira_key: jira_key.to_owned(),
            test_name: test_name.to_owned(),
            repo_id,
            plan_path: plan_path.to_owned(),
            app_version: None,
            platform_id: None,
            summary: format!("{test_name} is failing"),
        }
    }

    struct Fixture {
        client: QaInsightsLocalClient<OrmJiraRepository, OrmResultsRepository>,
        ctx: toolkit_security::SecurityContext,
        db: Arc<DBProvider<DomainError>>,
    }

    async fn build(authz: Arc<dyn authz_resolver_sdk::AuthZResolverClient>) -> Fixture {
        let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
        let service = Arc::new(JiraService::new(
            Arc::clone(&db),
            OrmJiraRepository,
            OrmResultsRepository,
            Arc::new(UnusedJiraClient) as Arc<dyn JiraClient>,
            PolicyEnforcer::new(authz),
        ));
        Fixture {
            client: QaInsightsLocalClient::new(service),
            ctx: ctx(TENANT),
            db,
        }
    }

    async fn tenant_scoped() -> Fixture {
        build(Arc::new(TenantScopedAuthZ)).await
    }

    async fn upsert(f: &Fixture, bug: NewJiraBug) {
        let conn = f.db.conn().unwrap();
        OrmJiraRepository
            .upsert_bug(&conn, &scope(TENANT), TENANT, bug)
            .await
            .unwrap();
    }

    async fn resolve(f: &Fixture, jira_key: &str) {
        let conn = f.db.conn().unwrap();
        assert!(
            OrmJiraRepository
                .resolve_bug(
                    &conn,
                    &scope(TENANT),
                    TENANT,
                    jira_key,
                    crate::infra::storage::test_db::now(),
                )
                .await
                .unwrap(),
            "the fixture bug must exist to be resolved, or the fixture is not what the test thinks it is"
        );
    }

    /// A fixture with **no open bug against the launched plan** — one bug
    /// against it that the poller has since resolved, and one open bug against
    /// a *different* plan in the same tenant. Neither may appear.
    ///
    /// # Why not simply zero rows
    ///
    /// A fixture with no bugs at all cannot tell a correct implementation from
    /// one that dropped the `(repo_id, plan_path)` narrowing — both would
    /// return an empty `Vec` against zero rows. The open bug against the
    /// *other* plan is what closes that gap: a `skip_list_for` that called
    /// `open_bugs(ctx, None, None)`, or otherwise lost the narrowing on the
    /// way to that call, would surface it here. Break-verified below.
    ///
    /// The resolved bug against the *target* plan adds no discriminating power
    /// at this layer — [`JiraRepository::list_open_for_plan`]'s own SQL already
    /// narrows to `status = 'Open'` (Task 33's own tests pin that), so this
    /// adapter never sees a non-open row to filter in the first place, and
    /// [`crate::domain::jira::registry::skip_list_entries`] applied to an
    /// already-open-only slice is a no-op.
    /// It stays in the fixture anyway, as a realistic composed scenario rather
    /// than as a claimed mutation-catcher — do not read its presence as
    /// pinning something this test can fail on.
    async fn fixture_with_no_open_bugs() -> Fixture {
        let f = tenant_scoped().await;
        upsert(&f, new_bug("VHP-1", "test_login", plan_repo(), PLAN_PATH)).await;
        resolve(&f, "VHP-1").await;
        upsert(
            &f,
            new_bug(
                "VHP-2",
                "test_logout",
                Uuid::from_u128(0x22),
                "plans/other/plan.yaml",
            ),
        )
        .await;
        f
    }

    /// An empty skip list must produce whatever legacy produces.
    ///
    /// **Step 0 (this task's brief).** Legacy's launch path
    /// (`manager/src/routes/runs.rs:753-761`) reads:
    /// `match jira_svc.get_open_bugs(id).await { Ok(bugs) if !bugs.is_empty() => Some(...), _ => None }`
    /// — called unconditionally on every plan run submission (there is no
    /// "skip-tests-with-bugs requested" flag gating the call; `SKIP_TESTS_WITH_BUGS`
    /// in `manager/src/routes/settings.rs:23`'s `RESERVED_PIPELINE_VARIABLE_NAMES`
    /// is unrelated — it only stops an operator pipeline variable from
    /// colliding with the name). `services/argo.rs:476-481` then pushes the env
    /// var only when the joined string is non-empty:
    /// `if let Some(skip) = skip_tests_with_bugs { if !skip.is_empty() { env_vars.push(...) } }`.
    /// So an empty bug list produces `None`, and `None` produces **no**
    /// `SKIP_TESTS_WITH_BUGS` variable at all — confirming, not correcting,
    /// `qa_insights_sdk::SkipListEntry`'s own doc and this trait method's
    /// contract: an empty `Vec`, not an error, and never a `Some("")`. Cited
    /// against `vhp-testrunner` at the two locations cited above.
    #[tokio::test]
    async fn an_empty_skip_list_matches_legacy() {
        let f = fixture_with_no_open_bugs().await;

        let entries = f
            .client
            .skip_list_for(&f.ctx, plan_repo(), PLAN_PATH)
            .await
            .expect("skip list");

        assert!(entries.is_empty(), "{entries:?}");
    }

    /// The non-empty half of the same wiring: two open bugs against the
    /// launched plan, one closed bug against it, and one open bug against a
    /// different plan. Only the two open, matching-plan bugs may come back,
    /// in query order (no sort, no dedup — `domain::jira::registry`'s own
    /// header), each carrying `test_name` and `jira_key` verbatim.
    #[tokio::test]
    async fn a_non_empty_skip_list_carries_only_the_plans_open_bugs() {
        let f = tenant_scoped().await;
        upsert(&f, new_bug("VHP-10", "test_a", plan_repo(), PLAN_PATH)).await;
        upsert(&f, new_bug("VHP-11", "test_b", plan_repo(), PLAN_PATH)).await;
        upsert(&f, new_bug("VHP-12", "test_c", plan_repo(), PLAN_PATH)).await;
        resolve(&f, "VHP-12").await;
        upsert(
            &f,
            new_bug(
                "VHP-13",
                "test_d",
                Uuid::from_u128(0x23),
                "plans/other/plan.yaml",
            ),
        )
        .await;

        let entries = f
            .client
            .skip_list_for(&f.ctx, plan_repo(), PLAN_PATH)
            .await
            .expect("skip list");

        let pairs: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| (e.test_name.as_str(), e.jira_key.as_str()))
            .collect();
        assert_eq!(pairs, vec![("test_a", "VHP-10"), ("test_b", "VHP-11")]);
    }

    /// A PDP denial must name the JIRA resource, not the test result one —
    /// this module's header explains why `as_jira_error` and not a bare
    /// `.map_err(Into::into)` is what makes that true.
    ///
    /// # What makes this test able to fail
    ///
    /// Reverting `skip_list_for` to `.map_err(Into::into)?` turns this red:
    /// the blanket `From<DomainError> for CanonicalError` attributes a bare
    /// `Forbidden` to `cf.qa.insights.test_result.v1~`, which this test's
    /// second assertion refuses.
    #[tokio::test]
    async fn a_denied_caller_gets_a_jira_attributed_refusal() {
        let f = build(Arc::new(DenyAllAuthZ)).await;

        let error = f
            .client
            .skip_list_for(&f.ctx, plan_repo(), PLAN_PATH)
            .await
            .expect_err("a denied caller must not see a skip list");

        let status = error.status_code();
        let body =
            serde_json::to_string(&Problem::from_error(&error).expect("a problem must serialize"))
                .expect("a problem must serialize");

        assert_eq!(status, 403, "{body}");
        assert!(body.contains("cf.qa.insights.jira_bug.v1~"), "{body}");
        assert!(!body.contains("cf.qa.insights.test_result.v1~"), "{body}");
    }

    /// The `Validation` half of `as_jira_error`'s attribution — added in
    /// Task 34 fix round 1, correcting this module's own header, which
    /// originally (wrongly) claimed `skip_list_for`'s mandatory `plan_path:
    /// &str` parameter made this arm unreachable. `optional_plan_ref` treats
    /// an empty or whitespace-only `plan_path` as **absent**, which is
    /// exactly R85's "one without the other" shape, so `skip_list_for(ctx,
    /// repo_id, "")` reaches the same `Validation { field: "plan_path", .. }`
    /// a bare `repo_id` with no `plan_path` at all would.
    ///
    /// # What makes this test able to fail
    ///
    /// Reverting `skip_list_for` to `.map_err(QaInsightsError::from)` (the
    /// blanket, unattributed mapping — the same mutation
    /// `a_denied_caller_gets_a_jira_attributed_refusal` exercises for
    /// `Forbidden`) turns this red: the blanket `From<DomainError> for
    /// CanonicalError` attributes a bare `Validation` to
    /// `cf.qa.insights.test_result.v1~`, which this test's second and third
    /// assertions refuse. Verified by mutation, not merely asserted — see the
    /// fix-round report.
    #[tokio::test]
    async fn an_empty_plan_path_gets_a_jira_attributed_validation_error() {
        let f = tenant_scoped().await;

        let error = f
            .client
            .skip_list_for(&f.ctx, plan_repo(), "")
            .await
            .expect_err("an empty plan_path must be refused, not treated as 'no plan named'");

        let status = error.status_code();
        let body =
            serde_json::to_string(&Problem::from_error(&error).expect("a problem must serialize"))
                .expect("a problem must serialize");

        assert_eq!(status, 400, "{body}");
        assert!(body.contains("cf.qa.insights.jira_bug.v1~"), "{body}");
        assert!(!body.contains("cf.qa.insights.test_result.v1~"), "{body}");
        assert!(body.contains("plan_path"), "{body}");
    }
}
