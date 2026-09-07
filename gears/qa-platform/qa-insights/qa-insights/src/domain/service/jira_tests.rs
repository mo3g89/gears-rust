//! Tests for the JIRA configuration surface and the one client pass-through.
//!
//! Against the **real** repository (`OrmJiraRepository`) on in-memory `SQLite`,
//! matching `saved_views_tests` and `collect_tests`: what is under test is
//! whether the scope and the singleton semantics the service relies on are the
//! ones the storage layer actually implements, and a repository double would
//! absorb exactly that. The JIRA instance itself is doubled — [`FakeJira`] —
//! because this module's job is the config surface, not the wire format the
//! adapter's own tests cover.
//!
//! The first two tests are this task's brief, adapted where the brief's names
//! did not survive contact with the code:
//!
//! * The brief writes `config.api_token_ref`; the contract field is
//!   `api_token_credstore_ref` (`qa-insights-sdk/src/models.rs:507`).
//! * The brief writes `f.service.poll_once(&f.ctx)`. **The poller is Task 35**
//!   and building it here would be building another task's work; the *rule* the
//!   brief pins — a disabled or absent config short-circuits silently rather
//!   than erroring (`manager/src/services/jira_poller.rs:40-43`) — is a property
//!   of the config read, which is this task's, and
//!   [`JiraService::active_config`] is where it lives.
//!   [`a_disabled_or_absent_jira_config_is_not_an_error`] pins it there.
//!
//! # Task 33 extends this file rather than starting a second one
//!
//! [`open_bugs`](JiraService::open_bugs) and [`file_bugs`](JiraService::file_bugs)
//! are two more methods of the same service, over the same fixture shape — a
//! real [`OrmJiraRepository`] and now a real
//! [`OrmResultsRepository`](crate::infra::storage::results_sea_repo::OrmResultsRepository)
//! on in-memory `SQLite`, plus [`FakeJira`], which [`FakeJira::create_or_find_issue`]
//! now actually implements rather than panicking — this task is the caller
//! Task 32 built the port for but did not exercise end to end.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use qa_insights_sdk::{JiraBug, JiraConfig, JiraPollerConfig, NewJiraBug};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{JiraConfigInput, JiraService};
use crate::domain::analytics::PlanRef;
use crate::domain::error::DomainError;
use crate::domain::ports::jira_client::{
    IssueRef, JiraClient, JiraIssue, NewIssue, StatusCategory,
};
use crate::domain::repos::{JiraRepository, NewTestCaseResult, NewTestResult, ResultsRepository};
use crate::domain::service::test_support::{
    DenyAllAuthZ, RecordingAuthZ, TenantScopedAuthZ, ctx, permissive_response,
};
use crate::infra::storage::jira_sea_repo::OrmJiraRepository;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{inmem_db, now, scope};

const TENANT: Uuid = Uuid::from_u128(0x0A);

/// The credential-store reference a tenant stores. **Not** a token: the whole
/// point of the column is that the material lives in credstore.
const TOKEN_REF: &str = "cred://qa-jira-api-token";

/// A [`JiraClient`] double that answers one scripted category, records the
/// keys it was asked about, and — since Task 33 — actually files or finds
/// issues rather than panicking.
struct FakeJira {
    category: StatusCategory,
    asked: Mutex<Vec<String>>,
    /// Every [`NewIssue`] handed to [`Self::create_or_find_issue`], in call
    /// order. What a test asserts on to see what `file_bugs` built.
    filed: Mutex<Vec<NewIssue>>,
    /// Answers for [`Self::create_or_find_issue`], consumed front-first. Empty
    /// mints `VHP-{n}` with `created: true` for the `n`th call, which is the
    /// shape most filing tests want and need not script.
    script: Mutex<VecDeque<Result<IssueRef, DomainError>>>,
}

impl FakeJira {
    fn answering(category: &str) -> Self {
        Self {
            category: StatusCategory::new(category),
            asked: Mutex::new(Vec::new()),
            filed: Mutex::new(Vec::new()),
            script: Mutex::new(VecDeque::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    /// Every issue this fake was asked to file or find, in call order.
    fn filed(&self) -> Vec<NewIssue> {
        self.filed.lock().unwrap().clone()
    }

    /// Queue the next [`Self::create_or_find_issue`] answer — for a test that
    /// needs to script the JIRA-side dedupe hit (`created: false` with no
    /// local row backing it) or a port failure.
    fn script_next(&self, result: Result<IssueRef, DomainError>) {
        self.script.lock().unwrap().push_back(result);
    }
}

#[async_trait]
impl JiraClient for FakeJira {
    async fn create_or_find_issue(
        &self,
        _: &SecurityContext,
        _: &JiraConfig,
        issue: NewIssue,
    ) -> Result<IssueRef, DomainError> {
        let mut filed = self.filed.lock().unwrap();
        filed.push(issue);
        let call_number = filed.len();
        drop(filed);

        if let Some(scripted) = self.script.lock().unwrap().pop_front() {
            return scripted;
        }
        Ok(IssueRef {
            jira_key: format!("VHP-{call_number}"),
            created: true,
        })
    }

    async fn check_status(
        &self,
        _: &SecurityContext,
        _: &JiraConfig,
        jira_key: &str,
    ) -> Result<StatusCategory, DomainError> {
        self.asked.lock().unwrap().push(jira_key.to_owned());
        Ok(self.category.clone())
    }

    async fn get_issue(
        &self,
        _: &SecurityContext,
        _: &JiraConfig,
        _: &str,
    ) -> Result<JiraIssue, DomainError> {
        unimplemented!("nothing in this task calls it")
    }
}

/// A PDP double that grants and compiles a scope over **two** tenants —
/// `owner_tenant_id IN [TENANT, OTHER]`.
///
/// The shape a parent-tenant grant produces, and the one
/// [`a_multi_tenant_scope_does_not_carry_another_tenants_credential_reference`]
/// exists for. `test_support::TenantScopedAuthZ` cannot stand in: it emits a
/// single-tenant `In`, which is precisely the case where an unpinned `.one()`
/// happens to be safe.
///
/// Local to this module rather than added to `test_support`: it is the only
/// caller, and a shared double whose whole point is a *wider* scope is an easy
/// thing to reach for by accident.
struct TwoTenantAuthZ;

#[async_trait]
impl authz_resolver_sdk::AuthZResolverClient for TwoTenantAuthZ {
    async fn evaluate(
        &self,
        _request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, authz_resolver_sdk::AuthZResolverError>
    {
        Ok(authz_resolver_sdk::EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::EvaluationResponseContext {
                constraints: vec![authz_resolver_sdk::Constraint {
                    predicates: vec![authz_resolver_sdk::Predicate::In(
                        authz_resolver_sdk::InPredicate::new(
                            toolkit_security::pep_properties::OWNER_TENANT_ID,
                            [TENANT, Uuid::from_u128(0x0B)],
                        ),
                    )],
                }],
                ..Default::default()
            },
        })
    }
}

/// A PDP double that grants every `qa.jira_bug` and `qa.jira_config` request
/// and denies everything else — in particular `qa.test_result`.
///
/// The fixture for controller ruling R87: if [`JiraService::file_bugs`] read
/// `qa_test_results`/`qa_test_case_results` under the bug scope rather than
/// compiling its own `qa.test_result` scope
/// ([`JiraService::results_scope`]), this double would never be asked about
/// `qa.test_result` at all and the whole call would wrongly succeed.
struct GrantsJiraButNotResultsAuthZ;

#[async_trait]
impl authz_resolver_sdk::AuthZResolverClient for GrantsJiraButNotResultsAuthZ {
    async fn evaluate(
        &self,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, authz_resolver_sdk::AuthZResolverError>
    {
        if request.resource.resource_type.starts_with("qa.jira") {
            Ok(permissive_response(&request))
        } else {
            Ok(authz_resolver_sdk::EvaluationResponse {
                decision: false,
                context: authz_resolver_sdk::EvaluationResponseContext::default(),
            })
        }
    }
}

/// A [`JiraRepository`] that delegates everything to [`OrmJiraRepository`]
/// except [`Self::upsert_bug`], which always fails.
///
/// The fixture for controller ruling R88: a filed-or-found issue must not be
/// discarded from [`JiraService::file_bugs`]' response just because the local
/// registration write failed after the fact — legacy's own `let _ =
/// self.track_bug(...)` treats that write as best-effort, and this double is
/// how `upsert_bug_failure_does_not_discard_a_filed_issue` forces the write to
/// fail without needing a real database fault.
#[derive(Clone, Default)]
struct FailingUpsertJiraRepository;

#[async_trait]
impl JiraRepository for FailingUpsertJiraRepository {
    async fn list_open<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<JiraBug>, DomainError> {
        OrmJiraRepository.list_open(runner, scope, tenant_id).await
    }

    async fn list_open_for_plan<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        plan: &PlanRef,
    ) -> Result<Vec<JiraBug>, DomainError> {
        OrmJiraRepository
            .list_open_for_plan(runner, scope, tenant_id, plan)
            .await
    }

    async fn upsert_bug<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        _bug: NewJiraBug,
    ) -> Result<JiraBug, DomainError> {
        Err(DomainError::database(
            "simulated local registration failure",
        ))
    }

    async fn find_unclosed_for_test<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        test_name: &str,
    ) -> Result<Option<JiraBug>, DomainError> {
        OrmJiraRepository
            .find_unclosed_for_test(runner, scope, tenant_id, test_name)
            .await
    }

    async fn resolve_bug<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        jira_key: &str,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        OrmJiraRepository
            .resolve_bug(runner, scope, tenant_id, jira_key, at)
            .await
    }

    async fn find_by_key<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        jira_key: &str,
    ) -> Result<Option<JiraBug>, DomainError> {
        OrmJiraRepository
            .find_by_key(runner, scope, tenant_id, jira_key)
            .await
    }

    async fn get_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<JiraConfig>, DomainError> {
        OrmJiraRepository.get_config(runner, scope, tenant_id).await
    }

    async fn save_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        config: JiraConfig,
    ) -> Result<(), DomainError> {
        OrmJiraRepository
            .save_config(runner, scope, tenant_id, config)
            .await
    }

    async fn get_poller_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<JiraPollerConfig>, DomainError> {
        OrmJiraRepository
            .get_poller_config(runner, scope, tenant_id)
            .await
    }

    async fn save_poller_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        config: JiraPollerConfig,
    ) -> Result<(), DomainError> {
        OrmJiraRepository
            .save_poller_config(runner, scope, tenant_id, config)
            .await
    }
}

struct Fixture {
    service: JiraService<OrmJiraRepository, OrmResultsRepository>,
    ctx: SecurityContext,
    jira: Arc<FakeJira>,
    /// The same provider the service holds, so a test can read a row back
    /// through the repository under a scope of its own choosing.
    db: Arc<DBProvider<DomainError>>,
}

/// The stored config every fixture that has one saves.
fn stored_config() -> JiraConfigInput {
    JiraConfigInput {
        url: "https://jira.example.com".to_owned(),
        project_key: "VHP".to_owned(),
        email: "qa@example.com".to_owned(),
        api_token_credstore_ref: TOKEN_REF.to_owned(),
        issue_type: Some("Bug".to_owned()),
        enabled: true,
    }
}

async fn build(authz: Arc<dyn authz_resolver_sdk::AuthZResolverClient>) -> Fixture {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let jira = Arc::new(FakeJira::answering(StatusCategory::DONE));
    let service = JiraService::new(
        Arc::clone(&db),
        OrmJiraRepository,
        OrmResultsRepository,
        Arc::clone(&jira) as Arc<dyn JiraClient>,
        PolicyEnforcer::new(authz),
    );
    Fixture {
        service,
        ctx: ctx(TENANT),
        jira,
        db,
    }
}

/// No `qa_jira_config` row at all — a tenant that has never opened the settings
/// screen.
async fn fixture_without_jira_config() -> Fixture {
    build(Arc::new(TenantScopedAuthZ)).await
}

/// One enabled `qa_jira_config` row, saved through the service's own write path
/// so the test cannot pass against a write shape production does not use.
async fn fixture_with_jira_config() -> Fixture {
    let f = fixture_without_jira_config().await;
    f.service
        .save_jira_config(&f.ctx, stored_config())
        .await
        .unwrap();
    f
}

// ---------------------------------------------------------------------------
// The brief's two
// ---------------------------------------------------------------------------

/// The token is a credstore reference here, never the material (Task 10). A
/// `GET` that returned a bearer token would be a credential-disclosure bug, and
/// legacy's masking — `"********"` substituted on read at
/// `manager/src/routes/settings.rs:254-259`, confirmed in this task's Step 0 —
/// is the floor this clears rather than meets.
///
/// # What makes this test able to fail
///
/// The fixture stores a reference and the assertion is that the read returns
/// **that reference, unchanged**. Both ways of getting it wrong go red: masking
/// the value (legacy's own behaviour, the one a porter reproduces by reflex) and
/// substituting anything else for it. The brief's own phrasing —
/// `assert!(!config.api_token_ref.contains("secret"))` — is the defect class
/// this plan has shipped before: this gear has no token material anywhere, so an
/// assertion that the result does not contain any could never fail.
#[tokio::test]
async fn reading_the_jira_config_returns_the_credstore_reference_unmasked() {
    let f = fixture_with_jira_config().await;

    let config = f.service.get_jira_config(&f.ctx).await.expect("config");

    assert_eq!(
        config.api_token_credstore_ref, TOKEN_REF,
        "the read must return the stored credstore reference verbatim - neither masked \
         (legacy's behaviour) nor replaced",
    );
    assert!(
        !config.api_token_credstore_ref.contains('*'),
        "a masked value would mean the reference had been treated as material: {}",
        config.api_token_credstore_ref,
    );
}

/// A disabled or absent config short-circuits silently rather than erroring —
/// `manager/src/services/jira_poller.rs:40-43` returns `Ok(())`.
///
/// Pinned on [`JiraService::active_config`] rather than on a `poll_once` the
/// poller task owns; this module's header records why.
#[tokio::test]
async fn a_disabled_or_absent_jira_config_is_not_an_error() {
    let absent = fixture_without_jira_config().await;
    assert_eq!(
        absent.service.active_config(&absent.ctx).await.unwrap(),
        None,
        "no row is 'nothing to do', not a failure",
    );

    let disabled = fixture_without_jira_config().await;
    disabled
        .service
        .save_jira_config(
            &disabled.ctx,
            JiraConfigInput {
                enabled: false,
                ..stored_config()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        disabled.service.active_config(&disabled.ctx).await.unwrap(),
        None,
        "a row with enabled=false is the same 'nothing to do'",
    );

    let live = fixture_with_jira_config().await;
    assert!(
        live.service
            .active_config(&live.ctx)
            .await
            .unwrap()
            .is_some(),
        "an enabled row must still be usable, or the two assertions above pass for the \
         wrong reason",
    );
}

// ---------------------------------------------------------------------------
// The config surface
// ---------------------------------------------------------------------------

/// An absent row reads as legacy's all-blank document, not as a 404.
///
/// Legacy's `api_get_jira` answers `Ok(None)` with a hand-built object whose
/// `issue_type` is `"Bug"` and whose `enabled` is `false`
/// (`manager/src/routes/settings.rs:262-269`). Applying that default is the
/// service's job, not the repository's — the repository still answers `None`, so
/// a caller that needs to tell "unconfigured" from "configured to blanks" can.
#[tokio::test]
async fn an_absent_config_reads_as_legacys_blank_document() {
    let f = fixture_without_jira_config().await;

    let config = f.service.get_jira_config(&f.ctx).await.unwrap();

    assert_eq!(
        config,
        JiraConfig {
            url: String::new(),
            project_key: String::new(),
            email: String::new(),
            api_token_credstore_ref: String::new(),
            issue_type: Some("Bug".to_owned()),
            enabled: false,
        },
    );
}

/// Every field survives a save, and a second save **replaces** rather than
/// duplicating — one row per tenant, `idx_qa_jira_config_tenant`.
///
/// A field missing from the repository's `update_columns` list keeps its old
/// value on a save that looked like it stored the whole form, which is the silent
/// half of a settings screen that appears to work.
#[tokio::test]
async fn saving_a_jira_config_round_trips_every_field_and_replaces_the_previous_row() {
    let f = fixture_with_jira_config().await;

    let second = JiraConfigInput {
        url: "https://other.example.com".to_owned(),
        project_key: "OTHER".to_owned(),
        email: "someone@example.com".to_owned(),
        api_token_credstore_ref: "cred://qa-jira-rotated".to_owned(),
        issue_type: Some("Task".to_owned()),
        enabled: false,
    };
    f.service
        .save_jira_config(&f.ctx, second.clone())
        .await
        .unwrap();

    let read = f.service.get_jira_config(&f.ctx).await.unwrap();
    assert_eq!(read.url, second.url);
    assert_eq!(read.project_key, second.project_key);
    assert_eq!(read.email, second.email);
    assert_eq!(
        read.api_token_credstore_ref, second.api_token_credstore_ref,
        "a credential repoint that did not take effect is the worst of these to miss",
    );
    assert_eq!(read.issue_type, second.issue_type);
    assert!(!read.enabled);
}

/// **A save that carries no credential reference keeps the stored one.**
///
/// This is legacy's `"********"` sentinel (`manager/src/routes/settings.rs:284-295`:
/// `if config.api_token == "********"` then reuse `existing.api_token`) ported
/// onto a value that is never masked. The mechanism legacy needs it for is the
/// same one here — a settings form that does not resend the credential must not
/// blank it — but the sentinel itself cannot survive the port, because nothing on
/// this gear's read path ever writes one. Empty takes its place.
///
/// The cost, recorded because it is a real one: the reference cannot be *cleared*
/// through this endpoint. Disabling the integration is the way to stop using it.
#[tokio::test]
async fn a_save_with_no_credential_reference_keeps_the_stored_one() {
    let f = fixture_with_jira_config().await;

    f.service
        .save_jira_config(
            &f.ctx,
            JiraConfigInput {
                project_key: "MOVED".to_owned(),
                api_token_credstore_ref: String::new(),
                ..stored_config()
            },
        )
        .await
        .unwrap();

    let read = f.service.get_jira_config(&f.ctx).await.unwrap();
    assert_eq!(
        read.api_token_credstore_ref, TOKEN_REF,
        "an omitted reference must not blank the stored credential",
    );
    assert_eq!(
        read.project_key, "MOVED",
        "the rest of the form must still have been stored, or this test would pass on a \
         save that did nothing at all",
    );
}

/// With no row saved yet there is nothing to preserve, so an empty reference
/// stores as empty rather than failing — the same `Ok(None)` fall-through legacy
/// takes (`settings.rs:290-292`, the `else` arm).
///
/// **`enabled: false`, and not `stored_config()`'s `true`** — changed in fix
/// round 1. The empty-effective-reference rule added for finding 1 refuses an
/// *enabled* integration with no credential, so this test now covers the case
/// legacy's `else` arm actually leaves reachable: a tenant filling in the form
/// before it has provisioned the secret.
/// [`an_enabled_config_needs_a_credential_reference_and_a_disabled_one_does_not`]
/// is the other half.
#[tokio::test]
async fn a_save_with_no_credential_reference_and_no_stored_row_stores_empty() {
    let f = fixture_without_jira_config().await;

    f.service
        .save_jira_config(
            &f.ctx,
            JiraConfigInput {
                api_token_credstore_ref: String::new(),
                enabled: false,
                ..stored_config()
            },
        )
        .await
        .unwrap();

    assert!(
        f.service
            .get_jira_config(&f.ctx)
            .await
            .unwrap()
            .api_token_credstore_ref
            .is_empty(),
    );
}

/// **A credential reference the credential store could not resolve is refused at
/// the `PUT`, naming its own field.**
///
/// Fix round 1, finding 1. The stored value is copied verbatim into the oagw
/// upstream's apikey auth config and reaches `SecretRef::new` after a `cred://`
/// strip; `SecretRef` accepts `[a-zA-Z0-9_-]` only, up to 255 bytes, and
/// prohibits colons outright "to prevent `ExternalID` collisions in backend
/// storage" (`gears/credstore/credstore-sdk/src/models.rs:42-83`). Unchecked, a
/// bad reference saves cleanly and fails inside oagw at *request* time, with
/// nothing tying the failure back to the save.
///
/// # What makes this test able to fail
///
/// The first case is `credstore://qa/jira/api-token` — **the exact value every
/// fixture in this task's first round used**, which is how the contract got
/// missed. The legal pair at the end is the other half: a rule that rejected
/// everything would pass the negative cases and break the feature, so both legal
/// spellings are asserted to survive.
#[tokio::test]
async fn an_unresolvable_credential_reference_is_refused_naming_its_own_field() {
    let f = fixture_without_jira_config().await;

    for bad in [
        "credstore://qa/jira/api-token",
        "qa/jira/api-token",
        "qa:jira:token",
        "has spaces",
        "cred://",
    ] {
        let err = f
            .service
            .save_jira_config(
                &f.ctx,
                JiraConfigInput {
                    api_token_credstore_ref: bad.to_owned(),
                    ..stored_config()
                },
            )
            .await
            .expect_err(bad);
        match err {
            DomainError::Validation { field, .. } => {
                assert_eq!(field, "api_token_credstore_ref", "for {bad}");
            }
            other => panic!("{bad} must be refused naming the reference, got {other:?}"),
        }
    }

    for good in ["cred://qa-jira-api-token", "qa_jira_api_token"] {
        f.service
            .save_jira_config(
                &f.ctx,
                JiraConfigInput {
                    api_token_credstore_ref: good.to_owned(),
                    ..stored_config()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("{good} is legal and must be accepted: {e:?}"));
    }
}

/// Enabling the integration without any credential reference is refused, and
/// **disabling with none is not** — the blank document
/// [`an_absent_config_reads_as_legacys_blank_document`] hands out has an empty
/// reference and `enabled: false`, so a caller must be able to `PUT` it straight
/// back.
#[tokio::test]
async fn an_enabled_config_needs_a_credential_reference_and_a_disabled_one_does_not() {
    let f = fixture_without_jira_config().await;

    let err = f
        .service
        .save_jira_config(
            &f.ctx,
            JiraConfigInput {
                api_token_credstore_ref: String::new(),
                enabled: true,
                ..stored_config()
            },
        )
        .await
        .expect_err("an enabled integration with no credential can only fail at request time");
    match err {
        DomainError::Validation { field, .. } => assert_eq!(field, "api_token_credstore_ref"),
        other => panic!("expected a validation error naming the reference, got {other:?}"),
    }

    f.service
        .save_jira_config(
            &f.ctx,
            JiraConfigInput {
                api_token_credstore_ref: String::new(),
                enabled: false,
                ..stored_config()
            },
        )
        .await
        .expect("the unconfigured document must be storable as it is handed out");
}

// ---------------------------------------------------------------------------
// Tenant isolation of the read-then-write
// ---------------------------------------------------------------------------

/// **A scope spanning two tenants must not carry one tenant's credential
/// reference onto another's config.**
///
/// Fix round 1, finding 4. `qa.jira_config` declares `OWNER_TENANT_ID`, and both
/// `ScopeFilter::In` and `ScopeFilter::InTenantSubtree` compile against it
/// (`libs/toolkit-security/src/access_scope.rs:169-198`) — a parent-tenant grant
/// is a supported shape, not a hypothetical. Under one, the repository's
/// `.one()` (which carries no `ORDER BY`) returned an arbitrary in-scope row, and
/// `save_jira_config`'s keep-the-stored-reference read would then write **another
/// tenant's credential reference** onto this tenant's config — a config that
/// authenticates to JIRA as somebody else.
///
/// `refuse_scope_beyond_tenant` would not have caught it: that guard exempts
/// `OWNER_TENANT_ID` (`domain/service/mod.rs:508-513`), so an `In`/`InTenantSubtree`
/// tenant scope passes it unchanged. This is the axis it does not cover.
///
/// # What makes this test able to fail
///
/// Only **`OTHER`** has a stored row, and the caller is `TENANT`. The compiled
/// scope admits both, so an unpinned `.one()` can return `OTHER`'s row — and on
/// `SQLite` with a single row in the table it *does*. Removing the tenant predicate
/// from `get_config` turns both assertions red.
#[tokio::test]
async fn a_multi_tenant_scope_does_not_carry_another_tenants_credential_reference() {
    const OTHER: Uuid = Uuid::from_u128(0x0B);
    let f = build(Arc::new(TwoTenantAuthZ)).await;
    let conn = f.db.conn().unwrap();

    // Only the *other* tenant is configured, with a reference of its own.
    OrmJiraRepository
        .save_config(
            &conn,
            &scope(OTHER),
            OTHER,
            JiraConfig {
                url: "https://other.example.com".to_owned(),
                project_key: "OTHER".to_owned(),
                email: "other@example.com".to_owned(),
                api_token_credstore_ref: "cred://other-tenants-secret".to_owned(),
                issue_type: None,
                enabled: true,
            },
        )
        .await
        .unwrap();

    // The read side: this caller is unconfigured and must be told so.
    let read = f.service.get_jira_config(&f.ctx).await.unwrap();
    assert_eq!(
        read.api_token_credstore_ref, "",
        "an unconfigured tenant must not be shown another tenant's credential reference",
    );
    assert_eq!(read.url, "", "nor another tenant's JIRA URL: {read:?}");

    // The write side, which is the damaging one: an omitted reference must find
    // nothing to preserve rather than preserving somebody else's.
    let err = f
        .service
        .save_jira_config(
            &f.ctx,
            JiraConfigInput {
                api_token_credstore_ref: String::new(),
                ..stored_config()
            },
        )
        .await
        .expect_err(
            "with nothing of its own stored, an enabled save with no reference has no \
             credential at all and must be refused - never silently completed with another \
             tenant's",
        );
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "api_token_credstore_ref"),
        "{err:?}",
    );

    // And nothing of the other tenant's was disturbed.
    assert_eq!(
        OrmJiraRepository
            .get_config(&conn, &scope(OTHER), OTHER)
            .await
            .unwrap()
            .map(|c| c.api_token_credstore_ref),
        Some("cred://other-tenants-secret".to_owned()),
    );
}

// ---------------------------------------------------------------------------
// The poller config
// ---------------------------------------------------------------------------

/// An absent poller row reads as legacy's defaults — 300 seconds, auto-rerun
/// **on** (`manager/src/models.rs:1422-1429`, reached through
/// `get_or_default::<JiraPollerConfig>("jira_poller")` at
/// `manager/src/routes/settings.rs:513-517`).
#[tokio::test]
async fn an_absent_poller_config_reads_as_legacys_defaults() {
    let f = fixture_without_jira_config().await;

    assert_eq!(
        f.service.poller_config(&f.ctx).await.unwrap(),
        JiraPollerConfig {
            poll_interval_seconds: 300,
            auto_rerun_on_resolve: true,
        },
    );
}

/// **Zero seconds reads back as one.**
///
/// Legacy clamps with `.max(1)` at the point it sleeps
/// (`manager/src/services/jira_poller.rs:26`), and an unclamped zero is a hot
/// loop that pins a core and hammers the JIRA instance. The clamp is in the
/// domain — not in the DDL, not in the mapper and not in the repository's write —
/// so the stored value is still what the tenant saved.
///
/// # What makes this test able to fail
///
/// It saves **zero**, which is the only input the clamp changes. A clamp test
/// whose fixture passes an in-range value cannot fail, and this plan has shipped
/// one before. The second assertion is the other half: the *stored* row is
/// unclamped, so a clamp that had leaked into the write would go red here even
/// though the read still looked right.
#[tokio::test]
async fn a_zero_poll_interval_is_clamped_to_one_second_on_read_and_not_on_write() {
    let f = fixture_without_jira_config().await;

    f.service
        .save_poller_config(
            &f.ctx,
            JiraPollerConfig {
                poll_interval_seconds: 0,
                auto_rerun_on_resolve: false,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        f.service
            .poller_config(&f.ctx)
            .await
            .unwrap()
            .poll_interval_seconds,
        1,
        "a zero interval must never reach a sleep",
    );

    let conn = f.db.conn().unwrap();
    let stored = OrmJiraRepository
        .get_poller_config(&conn, &scope(TENANT), TENANT)
        .await
        .unwrap();
    assert_eq!(
        stored.map(|c| c.poll_interval_seconds),
        Some(0),
        "the row must hold what the tenant saved; the clamp belongs to the reader that \
         sleeps, not to the write",
    );
}

/// A saved poller config round-trips, and a value the clamp does not touch is
/// returned untouched.
#[tokio::test]
async fn saving_a_poller_config_round_trips() {
    let f = fixture_without_jira_config().await;
    let config = JiraPollerConfig {
        poll_interval_seconds: 900,
        auto_rerun_on_resolve: false,
    };

    f.service.save_poller_config(&f.ctx, config).await.unwrap();

    assert_eq!(f.service.poller_config(&f.ctx).await.unwrap(), config);
}

// ---------------------------------------------------------------------------
// The one client pass-through
// ---------------------------------------------------------------------------

/// `check_status` is the method Task 35's poller calls, and it resolves the
/// tenant's config itself so the poller never handles one.
///
/// The category is JIRA's, not this gear's — the fake answers `"done"` and
/// [`StatusCategory::is_resolved`] is what the poller will branch on
/// (`manager/src/services/jira_poller.rs:57`).
#[tokio::test]
async fn check_status_resolves_the_tenants_config_and_delegates() {
    let f = fixture_with_jira_config().await;

    let category = f.service.check_status(&f.ctx, "VHP-319").await.unwrap();

    assert!(category.is_resolved());
    assert_eq!(f.jira.asked(), vec!["VHP-319".to_owned()]);
}

/// With no usable config there is nothing to ask JIRA about, and the client is
/// not called at all — legacy raises `"JIRA is not configured"` from the same
/// point (`manager/src/services/jira.rs:258-262`).
#[tokio::test]
async fn check_status_without_a_usable_config_is_not_configured_and_calls_nothing() {
    let f = fixture_without_jira_config().await;

    let err = f
        .service
        .check_status(&f.ctx, "VHP-319")
        .await
        .expect_err("an unconfigured tenant has no JIRA to ask");

    assert!(matches!(err, DomainError::JiraNotConfigured), "{err:?}");
    assert!(f.jira.asked().is_empty());
}

// ---------------------------------------------------------------------------
// Authorization and isolation
// ---------------------------------------------------------------------------

/// A PDP denial is a `Forbidden` on every one of the five operations, and none of
/// them touches the database first.
#[tokio::test]
async fn a_denied_caller_can_neither_read_nor_write_either_config() {
    let f = build(Arc::new(DenyAllAuthZ)).await;

    assert!(matches!(
        f.service.get_jira_config(&f.ctx).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        f.service.save_jira_config(&f.ctx, stored_config()).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        f.service.poller_config(&f.ctx).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        f.service
            .save_poller_config(&f.ctx, JiraPollerConfig::default())
            .await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        f.service.active_config(&f.ctx).await,
        Err(DomainError::Forbidden)
    ));
}

/// One tenant's JIRA settings are invisible to another's.
///
/// Read back under the *other* tenant's scope rather than through an
/// `allow_all()` ground-truth read, which is the rule `infra::storage::test_db`'s
/// `scope` states: a ground-truth read done that way is how a cross-tenant probe
/// gets written by accident.
#[tokio::test]
async fn another_tenants_jira_settings_are_invisible() {
    let f = fixture_with_jira_config().await;
    let other = Uuid::from_u128(0x0B);

    let conn = f.db.conn().unwrap();
    let read = OrmJiraRepository
        .get_config(&conn, &scope(other), other)
        .await
        .unwrap();

    assert_eq!(read, None);
}

// ---------------------------------------------------------------------------
// Task 33: the bug registry — `open_bugs` and `file_bugs`
// ---------------------------------------------------------------------------

/// The plan every `file_bugs` fixture files against, unless a test needs a
/// second one to prove narrowing.
fn plan_repo() -> Uuid {
    Uuid::from_u128(0x12)
}

const PLAN_PATH: &str = "plans/smoke/plan.yaml";

/// One `FAILED` file-level row, denormalized exactly the way ingest would
/// stamp it — [`crate::domain::repos::NewTestResult`]'s own doc says the eight
/// run columns arrive with the row rather than being derived, which is why
/// this helper takes the plan pair rather than defaulting it.
fn failed_result(
    test_file: &str,
    test_name: &str,
    repo_id: Uuid,
    plan_path: &str,
) -> NewTestResult {
    NewTestResult {
        test_file: test_file.to_owned(),
        test_name: test_name.to_owned(),
        status: "FAILED".to_owned(),
        duration: None,
        launch_id: None,
        jira_key: None,
        product_version: Some("5.0.1".to_owned()),
        app_build: None,
        platform_id: Some(Uuid::from_u128(0x11)),
        repo_id: Some(repo_id),
        plan_path: Some(plan_path.to_owned()),
        branch: None,
        run_finished_at: None,
        run_created_at: None,
    }
}

/// One case row, minimal — the fields R84's concatenation and its `FAILED`
/// filter actually read.
fn case(test_file: &str, nodeid: &str, status: &str, reason: Option<&str>) -> NewTestCaseResult {
    NewTestCaseResult {
        test_file: test_file.to_owned(),
        nodeid: nodeid.to_owned(),
        name: nodeid.to_owned(),
        status: status.to_owned(),
        duration: None,
        reason: reason.map(str::to_owned),
        ticket: None,
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
        summary: "s".to_owned(),
    }
}

/// The happy path: one failed test, no local or JIRA-side dedupe hit, so the
/// port mints a fresh issue and the domain service registers it locally.
///
/// Pins three things that must all be true for this to be the port's first
/// production call to succeed: the issue reaches [`FakeJira`] with the right
/// `test_name`, `file_bugs` reports `created: true` for what the port
/// reported as created, and the registry actually gained a row — not just an
/// in-memory `IssueRef` nobody persisted.
#[tokio::test]
async fn filing_a_failed_test_creates_an_issue_and_registers_it_locally() {
    let f = fixture_with_jira_config().await;
    let conn = f.db.conn().unwrap();
    let tenant_scope = scope(TENANT);
    let run_id = Uuid::from_u128(0x900);
    let file = "tests/authn/test_login.py";

    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &tenant_scope,
            TENANT,
            run_id,
            vec![failed_result(file, "AuthN Login", plan_repo(), PLAN_PATH)],
            vec![case(file, "test_login.py::test_x", "FAILED", Some("boom"))],
        )
        .await
        .unwrap();

    let filed = f.service.file_bugs(&f.ctx, run_id, None).await.unwrap();

    assert_eq!(
        filed,
        vec![IssueRef {
            jira_key: "VHP-1".to_owned(),
            created: true,
        }],
    );

    let issues = f.jira.filed();
    assert_eq!(issues.len(), 1, "the port must be called exactly once");
    assert_eq!(issues[0].test_name, "AuthN Login");
    assert_eq!(issues[0].plan, PLAN_PATH);
    assert_eq!(issues[0].run_name, run_id.to_string());

    let registered = OrmJiraRepository
        .find_by_key(&conn, &tenant_scope, TENANT, "VHP-1")
        .await
        .unwrap()
        .expect("upsert_bug must have registered the filed key");
    assert_eq!(registered.test_name, "AuthN Login");
    assert_eq!(registered.repo_id, plan_repo());
    assert_eq!(registered.plan_path, PLAN_PATH);
}

/// **R84, pinned.** The issue body concatenates every `FAILED` case's
/// `reason` for the failing file, and a `PASSED` case's `reason` — however
/// populated — must never leak in.
///
/// # What makes this test able to fail
///
/// Three cases share one file: two `FAILED` with distinct reasons and one
/// `PASSED` whose reason reads "must never appear". A concatenation that
/// filtered on nothing, or that fell back to sending every case regardless of
/// status, goes red on the third assertion; an implementation that sent an
/// empty detail regardless (the R84 alternative this task rejected) goes red
/// on the first two.
#[tokio::test]
async fn the_issue_body_concatenates_every_failed_cases_reason_and_excludes_passed_ones() {
    let f = fixture_with_jira_config().await;
    let conn = f.db.conn().unwrap();
    let tenant_scope = scope(TENANT);
    let run_id = Uuid::from_u128(0x901);
    let file = "tests/authn/test_login.py";

    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &tenant_scope,
            TENANT,
            run_id,
            vec![failed_result(file, "AuthN Login", plan_repo(), PLAN_PATH)],
            vec![
                case(
                    file,
                    "test_login.py::test_a",
                    "FAILED",
                    Some("first failure"),
                ),
                case(
                    file,
                    "test_login.py::test_b",
                    "FAILED",
                    Some("second failure"),
                ),
                case(
                    file,
                    "test_login.py::test_c",
                    "PASSED",
                    Some("must never appear"),
                ),
            ],
        )
        .await
        .unwrap();

    f.service.file_bugs(&f.ctx, run_id, None).await.unwrap();

    let logs = f.jira.filed()[0].logs.clone();
    assert!(logs.contains("first failure"), "{logs}");
    assert!(logs.contains("second failure"), "{logs}");
    assert!(
        !logs.contains("must never appear"),
        "a PASSED case's reason must not leak into the issue body: {logs}"
    );
}

/// Step 1 of legacy's `create_or_find_issue`: an already-registered test is
/// found locally and the port is never reached.
#[tokio::test]
async fn filing_an_already_registered_test_finds_it_locally_and_calls_the_port_never() {
    let f = fixture_with_jira_config().await;
    let conn = f.db.conn().unwrap();
    let tenant_scope = scope(TENANT);
    let run_id = Uuid::from_u128(0x902);
    let file = "tests/authn/test_login.py";

    OrmJiraRepository
        .upsert_bug(
            &conn,
            &tenant_scope,
            TENANT,
            new_bug("VHP-42", "AuthN Login", plan_repo(), PLAN_PATH),
        )
        .await
        .unwrap();
    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &tenant_scope,
            TENANT,
            run_id,
            vec![failed_result(file, "AuthN Login", plan_repo(), PLAN_PATH)],
            vec![],
        )
        .await
        .unwrap();

    let filed = f.service.file_bugs(&f.ctx, run_id, None).await.unwrap();

    assert_eq!(
        filed,
        vec![IssueRef {
            jira_key: "VHP-42".to_owned(),
            created: false,
        }],
    );
    assert!(
        f.jira.filed().is_empty(),
        "the local dedupe hit must short-circuit before the port is ever called",
    );
}

/// **Controller ruling R86, fix round 1, Critical 1.** A multi-tenant scope
/// must not make the local re-file probe hand back another tenant's bug.
///
/// Uses [`TwoTenantAuthZ`], not [`crate::domain::service::test_support::TenantScopedAuthZ`]
/// — the same distinction
/// `a_multi_tenant_scope_does_not_carry_another_tenants_credential_reference`'s
/// own doc makes: a single-tenant `In` is precisely the case where an
/// unpinned `.one()` happens to be safe, so only the genuine two-tenant
/// fixture can catch this.
///
/// # What makes this test able to fail
///
/// `OTHER` has a bug filed against the **same** test name `TENANT` is about
/// to fail on, under a `jira_key` `TENANT` must never see. The scope
/// [`TwoTenantAuthZ`] compiles admits both tenants, so an unpinned
/// `find_unclosed_for_test` would return `OTHER`'s row. Three assertions catch
/// three distinct failure modes: the response must **not** carry `OTHER`'s
/// key (the cross-tenant read the finding named), the response must instead
/// be a fresh, created issue (the caller's own failing test must still reach
/// the port), and `OTHER`'s original row must be completely untouched (no
/// write leaked across the tenant boundary either).
#[tokio::test]
async fn a_multi_tenant_scope_does_not_return_another_tenants_bug_from_the_local_probe() {
    const OTHER: Uuid = Uuid::from_u128(0x0B);
    let f = build(Arc::new(TwoTenantAuthZ)).await;
    f.service
        .save_jira_config(&f.ctx, stored_config())
        .await
        .unwrap();
    let conn = f.db.conn().unwrap();
    let run_id = Uuid::from_u128(0x910);
    let file = "tests/authn/test_login.py";

    // OTHER already has a bug filed against the identical test name, under a
    // key TENANT must never see or reuse.
    OrmJiraRepository
        .upsert_bug(
            &conn,
            &scope(OTHER),
            OTHER,
            new_bug("VHP-999", "AuthN Login", plan_repo(), PLAN_PATH),
        )
        .await
        .unwrap();

    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &scope(TENANT),
            TENANT,
            run_id,
            vec![failed_result(file, "AuthN Login", plan_repo(), PLAN_PATH)],
            vec![],
        )
        .await
        .unwrap();

    let filed = f.service.file_bugs(&f.ctx, run_id, None).await.unwrap();

    assert_eq!(
        filed,
        vec![IssueRef {
            jira_key: "VHP-1".to_owned(),
            created: true,
        }],
        "TENANT's own failing test must reach the port and file fresh, not be answered with \
         OTHER's key",
    );

    let others_row = OrmJiraRepository
        .find_by_key(&conn, &scope(OTHER), OTHER, "VHP-999")
        .await
        .unwrap()
        .expect("OTHER's row must still exist, untouched");
    assert_eq!(others_row.status, "Open", "{others_row:?}");
    assert_eq!(
        others_row.summary, "s",
        "OTHER's row must not have been rewritten"
    );

    let tenants_row = OrmJiraRepository
        .find_by_key(&conn, &scope(TENANT), TENANT, "VHP-1")
        .await
        .unwrap()
        .expect("the freshly filed issue must be registered under TENANT");
    assert_eq!(tenants_row.test_name, "AuthN Login");
}

/// The port's own JQL-search dedupe hit (`created: false` with **no** local
/// row behind it, unlike the local-probe hit above) still registers locally.
///
/// Legacy calls `track_bug` on *both* of `create_or_find_issue`'s dedupe
/// paths (`jira.rs:92-101` for the JIRA-side hit, `:177-187` for a created
/// issue) and not on the local-probe hit, which is already the row `track_bug`
/// would have written. This is the port-side half of that rule:
/// [`JiraRepository::upsert_bug`]'s own `ON CONFLICT DO NOTHING` makes calling
/// it here safe even though nothing was locally registered before this call —
/// `filing_an_already_registered_test_finds_it_locally_and_calls_the_port_never`
/// pins the other half, where the port must *not* be reached at all.
#[tokio::test]
async fn a_port_side_dedupe_hit_is_still_registered_locally() {
    let f = fixture_with_jira_config().await;
    let conn = f.db.conn().unwrap();
    let tenant_scope = scope(TENANT);
    let run_id = Uuid::from_u128(0x909);
    let file = "tests/authn/test_login.py";

    f.jira.script_next(Ok(IssueRef {
        jira_key: "VHP-99".to_owned(),
        created: false,
    }));

    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &tenant_scope,
            TENANT,
            run_id,
            vec![failed_result(file, "AuthN Login", plan_repo(), PLAN_PATH)],
            vec![],
        )
        .await
        .unwrap();

    let filed = f.service.file_bugs(&f.ctx, run_id, None).await.unwrap();

    assert_eq!(
        filed,
        vec![IssueRef {
            jira_key: "VHP-99".to_owned(),
            created: false,
        }],
    );

    let registered = OrmJiraRepository
        .find_by_key(&conn, &tenant_scope, TENANT, "VHP-99")
        .await
        .unwrap()
        .expect(
            "a port-side dedupe hit still calls upsert_bug, exactly as legacy's track_bug does \
             for its own JQL-hit path",
        );
    assert_eq!(registered.test_name, "AuthN Login");
}

/// **Controller ruling R88, fix round 1, Important 3.** A local registration
/// failure must not discard a filed-or-found issue from the response.
///
/// Legacy is `let _ = self.track_bug(...)` on both of `create_or_find_issue`'s
/// dedupe/create paths (`jira.rs:92-101`, `:177-187`): local tracking is
/// best-effort there, and the key is returned regardless. An earlier revision
/// of [`JiraService::file_one`] let `upsert_bug`'s error propagate instead,
/// which is *stricter* than legacy and worse on this exact path: the issue
/// the port just filed or found is real in JIRA, but propagating drops it
/// from the response and leaves no local row behind — so the next call
/// reaches the port again and duplicates it, exactly what the local dedupe
/// probe exists to prevent.
///
/// # What makes this test able to fail
///
/// [`FailingUpsertJiraRepository::upsert_bug`] always fails without touching
/// the database, so a `find_by_key` afterward can distinguish "the write
/// genuinely failed" (no row) from "the write silently succeeded anyway"
/// (a row) — the assertion is on both the response *and* the absence of the
/// row, so a fix that quietly swallowed the error into an `Ok(())` would not
/// pass this test by accident.
#[tokio::test]
async fn upsert_bug_failure_does_not_discard_a_filed_issue() {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let jira = Arc::new(FakeJira::answering(StatusCategory::DONE));
    let service = JiraService::new(
        Arc::clone(&db),
        FailingUpsertJiraRepository,
        OrmResultsRepository,
        Arc::clone(&jira) as Arc<dyn JiraClient>,
        PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
    );
    let ctx = ctx(TENANT);
    let conn = db.conn().unwrap();
    let tenant_scope = scope(TENANT);
    let run_id = Uuid::from_u128(0x913);
    let file = "tests/authn/test_login.py";

    OrmJiraRepository
        .save_config(
            &conn,
            &tenant_scope,
            TENANT,
            JiraConfig {
                url: "https://jira.example.com".to_owned(),
                project_key: "VHP".to_owned(),
                email: "qa@example.com".to_owned(),
                api_token_credstore_ref: TOKEN_REF.to_owned(),
                issue_type: Some("Bug".to_owned()),
                enabled: true,
            },
        )
        .await
        .unwrap();
    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &tenant_scope,
            TENANT,
            run_id,
            vec![failed_result(file, "AuthN Login", plan_repo(), PLAN_PATH)],
            vec![],
        )
        .await
        .unwrap();

    let filed = service.file_bugs(&ctx, run_id, None).await.unwrap();

    assert_eq!(
        filed,
        vec![IssueRef {
            jira_key: "VHP-1".to_owned(),
            created: true,
        }],
        "the issue the port filed must still be reported even though local registration failed",
    );

    let registered = OrmJiraRepository
        .find_by_key(&conn, &tenant_scope, TENANT, "VHP-1")
        .await
        .unwrap();
    assert!(
        registered.is_none(),
        "the write genuinely failed and left no row - {registered:?} - confirming the test \
         fixture, not a fix that silently swallowed the error into a success",
    );
}

/// **Controller ruling R80, pinned end to end.** The repository-level tests
/// of similar names (`infra::storage::jira_sea_repo`) pin the predicate
/// directly; this pins that `file_bugs` actually reaches it through the whole
/// filing flow — a bug the poller has resolved still deduplicates a re-file
/// locally, reusing the stale, already-resolved key, even though the same bug
/// has already left [`JiraService::open_bugs`]'s answer.
#[tokio::test]
async fn a_resolved_bug_still_blocks_the_local_refile_probe() {
    let f = fixture_with_jira_config().await;
    let conn = f.db.conn().unwrap();
    let tenant_scope = scope(TENANT);
    let run_id = Uuid::from_u128(0x903);
    let file = "tests/authn/test_login.py";

    OrmJiraRepository
        .upsert_bug(
            &conn,
            &tenant_scope,
            TENANT,
            new_bug("VHP-7", "AuthN Login", plan_repo(), PLAN_PATH),
        )
        .await
        .unwrap();
    OrmJiraRepository
        .resolve_bug(&conn, &tenant_scope, TENANT, "VHP-7", now())
        .await
        .unwrap();

    assert!(
        f.service
            .open_bugs(&f.ctx, None, None)
            .await
            .unwrap()
            .is_empty(),
        "a resolved bug must have already left the open list",
    );

    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &tenant_scope,
            TENANT,
            run_id,
            vec![failed_result(file, "AuthN Login", plan_repo(), PLAN_PATH)],
            vec![],
        )
        .await
        .unwrap();

    let filed = f.service.file_bugs(&f.ctx, run_id, None).await.unwrap();

    assert_eq!(
        filed,
        vec![IssueRef {
            jira_key: "VHP-7".to_owned(),
            created: false,
        }],
        "the stale, already-resolved key must be reused rather than a fresh issue filed",
    );
    assert!(
        f.jira.filed().is_empty(),
        "the port must never be reached once the local probe matches",
    );
}

/// A run with **no** projected rows at all is [`DomainError::RunNotIngested`],
/// not an empty list — this gear has no Argo workflow to 404 against, so an
/// unprojected run and a projected run with zero failures must stay
/// distinguishable some other way.
#[tokio::test]
async fn filing_against_an_unprojected_run_is_run_not_ingested() {
    let f = fixture_with_jira_config().await;
    let run_id = Uuid::from_u128(0x904);

    let err = f
        .service
        .file_bugs(&f.ctx, run_id, None)
        .await
        .expect_err("no rows at all must not read as zero failures");

    assert!(
        matches!(err, DomainError::RunNotIngested { run_id: got } if got == run_id),
        "{err:?}",
    );
}

/// A run whose rows exist but hold no `FAILED` test is an empty list, not an
/// error — legacy's own loop has nothing to iterate in this case.
#[tokio::test]
async fn filing_for_a_run_with_no_failures_is_an_empty_list() {
    let f = fixture_with_jira_config().await;
    let conn = f.db.conn().unwrap();
    let run_id = Uuid::from_u128(0x905);

    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &scope(TENANT),
            TENANT,
            run_id,
            vec![NewTestResult {
                status: "PASSED".to_owned(),
                ..failed_result("tests/a.py", "A", plan_repo(), PLAN_PATH)
            }],
            vec![],
        )
        .await
        .unwrap();

    let filed = f.service.file_bugs(&f.ctx, run_id, None).await.unwrap();

    assert!(filed.is_empty(), "{filed:?}");
}

/// A missing or disabled JIRA config swallows every candidate test rather
/// than failing the request — legacy's identical per-test error, logged and
/// continued (`manager/src/routes/settings.rs:624-627`), reached here through
/// one shared config read rather than one re-read per test.
#[tokio::test]
async fn filing_without_a_usable_config_is_swallowed_per_test() {
    let f = fixture_without_jira_config().await;
    let conn = f.db.conn().unwrap();
    let run_id = Uuid::from_u128(0x906);

    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &scope(TENANT),
            TENANT,
            run_id,
            vec![failed_result("tests/a.py", "A", plan_repo(), PLAN_PATH)],
            vec![],
        )
        .await
        .unwrap();

    let filed = f
        .service
        .file_bugs(&f.ctx, run_id, None)
        .await
        .expect("an unconfigured tenant is swallowed per test, not a bulk error");

    assert!(filed.is_empty());
    assert!(
        f.jira.filed().is_empty(),
        "the port must never be called for an unconfigured tenant",
    );
}

/// `test_name: Some(..)` narrows filing to that one test, matching legacy's
/// `if let Some(target) = req.test_name { if result.name != *target { continue } }`.
#[tokio::test]
async fn filing_can_be_narrowed_to_one_test_name() {
    let f = fixture_with_jira_config().await;
    let conn = f.db.conn().unwrap();
    let run_id = Uuid::from_u128(0x907);

    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &scope(TENANT),
            TENANT,
            run_id,
            vec![
                failed_result("tests/a.py", "A", plan_repo(), PLAN_PATH),
                failed_result("tests/b.py", "B", plan_repo(), PLAN_PATH),
            ],
            vec![],
        )
        .await
        .unwrap();

    let filed = f
        .service
        .file_bugs(&f.ctx, run_id, Some("B"))
        .await
        .unwrap();

    assert_eq!(filed.len(), 1);
    assert_eq!(f.jira.filed()[0].test_name, "B");
}

/// A failed test with no plan identity — `repo_id`/`plan_path` both absent, a
/// custom-plan or collect run — cannot be filed against `qa_jira_bugs`
/// (`NOT NULL` on both columns) and is swallowed like any other per-test
/// failure, never a bulk error for the rest of the run.
#[tokio::test]
async fn filing_a_test_with_no_plan_identity_is_swallowed() {
    let f = fixture_with_jira_config().await;
    let conn = f.db.conn().unwrap();
    let run_id = Uuid::from_u128(0x908);

    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &scope(TENANT),
            TENANT,
            run_id,
            vec![NewTestResult {
                repo_id: None,
                plan_path: None,
                ..failed_result("tests/a.py", "A", plan_repo(), PLAN_PATH)
            }],
            vec![],
        )
        .await
        .unwrap();

    let filed = f.service.file_bugs(&f.ctx, run_id, None).await.unwrap();

    assert!(filed.is_empty(), "{filed:?}");
    assert!(f.jira.filed().is_empty());
}

/// `open_bugs(None, None)` is legacy's `get_all_open_bugs` fallback.
#[tokio::test]
async fn open_bugs_with_no_plan_named_lists_every_open_bug() {
    let f = fixture_without_jira_config().await;
    let conn = f.db.conn().unwrap();
    let tenant_scope = scope(TENANT);
    let other_repo = Uuid::from_u128(0x13);

    OrmJiraRepository
        .upsert_bug(
            &conn,
            &tenant_scope,
            TENANT,
            new_bug("VHP-1", "A", plan_repo(), PLAN_PATH),
        )
        .await
        .unwrap();
    OrmJiraRepository
        .upsert_bug(
            &conn,
            &tenant_scope,
            TENANT,
            new_bug("VHP-2", "B", other_repo, "plans/other/plan.yaml"),
        )
        .await
        .unwrap();

    let all = f.service.open_bugs(&f.ctx, None, None).await.unwrap();

    assert_eq!(all.len(), 2, "{all:?}");
}

/// Both present narrows to exactly that plan — the pair
/// [`JiraRepository::list_open_for_plan`] and
/// `qa_insights_sdk::QaInsightsClientV1::skip_list_for` already speak, and
/// **not** the analytics drill-downs' single `plan_id` matched across every
/// repository (controller ruling R85).
#[tokio::test]
async fn open_bugs_narrows_to_the_named_plan() {
    let f = fixture_without_jira_config().await;
    let conn = f.db.conn().unwrap();
    let tenant_scope = scope(TENANT);
    let other_repo = Uuid::from_u128(0x13);

    OrmJiraRepository
        .upsert_bug(
            &conn,
            &tenant_scope,
            TENANT,
            new_bug("VHP-1", "A", plan_repo(), PLAN_PATH),
        )
        .await
        .unwrap();
    OrmJiraRepository
        .upsert_bug(
            &conn,
            &tenant_scope,
            TENANT,
            new_bug("VHP-2", "B", other_repo, PLAN_PATH),
        )
        .await
        .unwrap();

    let narrowed = f
        .service
        .open_bugs(&f.ctx, Some(plan_repo()), Some(PLAN_PATH))
        .await
        .unwrap();

    assert_eq!(
        narrowed
            .iter()
            .map(|b| b.jira_key.as_str())
            .collect::<Vec<_>>(),
        vec!["VHP-1"],
        "the other repository's identically-pathed bug must not match: {narrowed:?}",
    );
}

/// **Controller ruling R85: one without the other is a 400, on both sides.**
#[tokio::test]
async fn open_bugs_refuses_exactly_one_of_the_pair() {
    let f = fixture_without_jira_config().await;

    let repo_only = f
        .service
        .open_bugs(&f.ctx, Some(plan_repo()), None)
        .await
        .expect_err("repo_id alone must be refused");
    assert!(
        matches!(repo_only, DomainError::Validation { ref field, .. } if field == "plan_path"),
        "{repo_only:?}",
    );

    let path_only = f
        .service
        .open_bugs(&f.ctx, None, Some(PLAN_PATH))
        .await
        .expect_err("plan_path alone must be refused");
    assert!(
        matches!(path_only, DomainError::Validation { ref field, .. } if field == "plan_path"),
        "{path_only:?}",
    );
}

/// A PDP denial refuses both operations before either reaches the database —
/// `file_bugs` against a run with **no** seeded rows would answer
/// `RunNotIngested` if the scope check ran second, so `Forbidden` here is
/// itself the proof the PDP runs first.
#[tokio::test]
async fn a_denied_caller_can_neither_list_nor_file_bugs() {
    let f = build(Arc::new(DenyAllAuthZ)).await;

    assert!(matches!(
        f.service.open_bugs(&f.ctx, None, None).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        f.service.file_bugs(&f.ctx, Uuid::from_u128(1), None).await,
        Err(DomainError::Forbidden)
    ));
}

/// **Controller ruling R87, fix round 1, Important 2.** `file_bugs` reads
/// `qa_test_results`/`qa_test_case_results` under a scope compiled over
/// `qa.test_result`, separately from the `qa.jira_bug` scope that authorizes
/// the registry write — a grant of the latter alone must not be enough.
///
/// # What makes this test able to fail
///
/// [`GrantsJiraButNotResultsAuthZ`] grants every `qa.jira*` request and
/// denies everything else. If a revision of `file_bugs` reused the bug
/// scope's compiled `AccessScope` for the results read (a first draft of this
/// method did — R87), this double would never see a `qa.test_result` request
/// at all, and the call would read the run's rows and succeed instead of
/// being refused.
#[tokio::test]
async fn filing_needs_a_qa_test_result_grant_separately_from_qa_jira_bug() {
    let f = build(Arc::new(GrantsJiraButNotResultsAuthZ)).await;
    let conn = f.db.conn().unwrap();
    OrmJiraRepository
        .save_config(
            &conn,
            &scope(TENANT),
            TENANT,
            JiraConfig {
                url: "https://jira.example.com".to_owned(),
                project_key: "VHP".to_owned(),
                email: "qa@example.com".to_owned(),
                api_token_credstore_ref: TOKEN_REF.to_owned(),
                issue_type: Some("Bug".to_owned()),
                enabled: true,
            },
        )
        .await
        .unwrap();

    let err = f
        .service
        .file_bugs(&f.ctx, Uuid::from_u128(0x911), None)
        .await
        .expect_err("a qa.test_result denial must refuse filing even with qa.jira_bug granted");

    assert!(matches!(err, DomainError::Forbidden), "{err:?}");
}

/// Every resource `file_bugs` actually reads is asked about, by name — the
/// positive half of R87: `qa.jira_bug` for the registry, `qa.test_result` for
/// the two results tables, and `qa.jira_config` for the usable-config read.
/// `results.rs`'s `both_collections_authorize_under_test_result_list` is the
/// precedent this follows for the same reason its own doc gives: nothing else
/// in this crate observes the request a scope call actually sends.
#[tokio::test]
async fn file_bugs_asks_the_pdp_about_every_resource_it_reads() {
    let authz = Arc::new(RecordingAuthZ::default());
    let f = build(authz.clone()).await;
    let conn = f.db.conn().unwrap();
    OrmJiraRepository
        .save_config(
            &conn,
            &scope(TENANT),
            TENANT,
            JiraConfig {
                url: "https://jira.example.com".to_owned(),
                project_key: "VHP".to_owned(),
                email: "qa@example.com".to_owned(),
                api_token_credstore_ref: TOKEN_REF.to_owned(),
                issue_type: Some("Bug".to_owned()),
                enabled: true,
            },
        )
        .await
        .unwrap();

    let run_id = Uuid::from_u128(0x912);
    OrmResultsRepository
        .upsert_run_results(
            &conn,
            &scope(TENANT),
            TENANT,
            run_id,
            vec![failed_result("tests/a.py", "A", plan_repo(), PLAN_PATH)],
            vec![],
        )
        .await
        .unwrap();

    f.service.file_bugs(&f.ctx, run_id, None).await.unwrap();

    assert_eq!(
        authz.asked(),
        vec![
            ("qa.jira_bug".to_owned(), "create".to_owned()),
            ("qa.test_result".to_owned(), "list".to_owned()),
            ("qa.jira_config".to_owned(), "get".to_owned()),
        ],
    );
}
