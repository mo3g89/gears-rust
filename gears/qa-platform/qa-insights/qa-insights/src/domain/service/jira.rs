//! The JIRA configuration surface — `GET/PUT /qa/v1/settings/jira` — and the
//! bug registry's list and filing path. Task 32 (the surface, the one outbound
//! status read) plus Task 33 (the registry), plus — Task 35 — the two methods
//! `domain::service::jira_poller` consumes and `GET/PUT
//! /qa/v1/settings/jira-poller`'s two handlers ([`JiraService::poller_config`],
//! [`JiraService::save_poller_config`], already Task 32's).
//!
//! The port of legacy's `api_get_jira`/`api_update_jira`
//! (`manager/src/routes/settings.rs:248-275` and `:278-305`) plus the
//! poller-config pair (`:510-542`, wired to the wire route by Task 35 —
//! [`JiraService::poller_config`]/[`JiraService::save_poller_config`] were the
//! reader and writer all along), plus — Task 33 — `api_open_bugs`
//! (`:634-651`) and `api_create_jira_ticket` (`:581-631`), plus — Task 35 —
//! [`JiraService::resolve_bug`] (`jira.rs:243-251`, the write legacy's poller
//! makes) and [`JiraService::latest_version_for_plan`]
//! (`jira_poller.rs:225-243`, its `check_new_build`).
//!
//! # The credential is a reference, and that is the whole security story
//!
//! `qa_jira_config.api_token_credstore_ref` holds a credential-store reference.
//! **Nothing on this path can return token material, because nothing on this
//! path ever holds any** — not this service, not the repository, not the DTO,
//! not even the oagw adapter, which hands the reference to `oagw`'s own apikey
//! auth plugin and lets that resolve it (`infra::jira::oagw_client`'s header).
//!
//! Legacy's floor is lower and it is worth stating exactly, because "we match
//! legacy" would be the wrong claim: legacy **stores the token** and substitutes
//! `"********"` on the way out (`settings.rs:254-259`). That is a mitigation for
//! a disclosure risk this design does not have. The one piece of it that *does*
//! port is the write half — see
//! [`JiraService::save_jira_config`]'s "an omitted reference keeps the stored one".
//!
//! # Five things this service decides that the repository deliberately does not
//!
//! 1. **An absent config reads as legacy's blank document, not as a 404.**
//!    `api_get_jira`'s `Ok(None)` arm answers a hand-built object with empty
//!    strings, `issue_type: "Bug"` and `enabled: false` (`settings.rs:262-269`).
//!    The repository still answers `None`, so a future caller that needs to tell
//!    "never configured" from "configured to blanks" can; [`JiraService::active_config`]
//!    is the caller that does not care, and it treats both as nothing to do.
//! 2. **An absent poller config reads as legacy's defaults** — 300 seconds and
//!    auto-rerun on (`manager/src/models.rs:1422-1429`, reached through
//!    `get_or_default::<JiraPollerConfig>` at `settings.rs:513-517`).
//! 3. **The `.max(1)` interval clamp lives here.** Legacy applies it where it
//!    sleeps (`manager/src/services/jira_poller.rs:26`), and controller ruling
//!    R71 puts it in the domain rather than in the DDL or the mapper. It is
//!    applied on **read** and not on write, so the stored row keeps what the
//!    tenant saved and the settings screen does not lie about it; a zero that
//!    reached a sleep would be a hot loop against a third party's API.
//! 4. **"Disabled" and "absent" are the same answer** to anything that wants to
//!    *use* JIRA. [`JiraService::active_config`] folds them, which is legacy's own
//!    short-circuit: `if config.is_none() || !config.as_ref().unwrap().enabled { return Ok(()) }`
//!    (`jira_poller.rs:40-43`) — an `Ok`, not an error.
//! 5. **An omitted credential reference preserves the stored one.**
//!    [`JiraService::save_jira_config`] carries the argument.
//!
//! # What Task 32 deliberately did not build, and what Task 33 is
//!
//! Task 32 shipped no poller (Task 35 still owns it), no bug endpoints and no
//! filing path. Its one outbound method was [`JiraService::check_status`],
//! which exists because the poller is specified to call it and because the
//! [`JiraClient`](crate::domain::ports::JiraClient) port had to arrive with a
//! call site — `domain::ports`' header states that rule.
//! [`JiraClient::create_or_find_issue`](crate::domain::ports::JiraClient::create_or_find_issue)
//! and
//! [`JiraClient::get_issue`](crate::domain::ports::JiraClient::get_issue)
//! therefore had no production caller in that commit — a weaker situation than
//! `check_status`' and not the same as an unused port, since both were already
//! exercised by the adapter's own tests against every legacy behaviour Task
//! 32's Step 0 recorded.
//!
//! **Task 33 is that caller**, and it is [`JiraService::file_bugs`] — the port
//! of legacy's `api_create_jira_ticket` (`manager/src/routes/settings.rs:581-631`),
//! reached at `POST /qa/v1/jira/bugs`. Its sibling,
//! [`JiraService::open_bugs`], ports `api_open_bugs` (`:634-651`) at
//! `GET /qa/v1/jira/open-bugs`.
//!
//! # `file_bugs` needs a second repository, and here is why
//!
//! Legacy's `api_create_jira_ticket` resolves a run's plan, app version and
//! platform by reading an Argo workflow (`state.argo.get_workflow`) and its
//! logs (`state.argo.get_workflow_logs`) — this gear has no Argo client and
//! never will (R84). What it has instead is its own event-sourced projection:
//! `qa_test_results`, already denormalized with exactly those columns per run.
//! So [`JiraService`] is generic over
//! [`ResultsRepository`](crate::domain::repos::ResultsRepository) as well as
//! [`JiraRepository`], the same shape
//! [`crate::domain::service::analytics::AnalyticsService<R, C>`] already uses to
//! read two repositories from one composed operation — extending the one
//! service in place rather than adding a second, `ResultsRepository`-only
//! service beside it on `AppServices`. `domain::service`'s own header, "And a
//! fifth, added by Task 32 ... then a use of `R` again for Task 33's
//! bug-filing path", carries why.
//!
//! # R87 — the results reads compile their own scope, fix round 1
//!
//! **A first revision of [`JiraService::file_bugs`] read `qa_test_results`
//! and `qa_test_case_results` under [`JiraService::bug_scope`]'s compiled
//! scope** — the one authorizing the registry write — reasoning that the read
//! was incidental to the filing operation, the way
//! `analytics::AnalyticsService::collect_counts` reads `qa_test_case_collect`
//! under [`resources::TEST_RESULT`] because that table has no independent
//! lifecycle of its own. Review round 1 rejected the analogy: unlike
//! `qa_test_case_collect`, the two results tables are exactly the resource
//! every *other* reader in this crate already gates
//! (`domain::service::results`, `dashboard`, `analytics`, `collect`,
//! `reconcile` all compile a [`resources::TEST_RESULT`] scope for them), and
//! reusing the bug scope here would have let `qa.jira_bug/create` alone read
//! a run's rows and ship per-case `reason` text to JIRA with no
//! `qa.test_result` grant. [`JiraService::results_scope`] is the fix: a
//! second, separately-compiled scope, over [`resources::TEST_RESULT`] /
//! [`actions::LIST`], for exactly the two calls that need it.
//!
//! # R80 — three status vocabularies, and which one `file_bugs` uses
//!
//! Legacy's `jira_bugs.status` is compared three different ways across three
//! call sites, and they disagree on one class of row. `get_open_bugs`/
//! `get_all_open_bugs` — [`JiraRepository::list_open`]/`list_open_for_plan`,
//! this gear's `GET /qa/v1/jira/open-bugs` — use `status = 'Open'`.
//! `resolve_bug` writes `status = 'Resolved'`. And `create_or_find_issue`'s own
//! local dedupe probe — [`JiraRepository::find_unclosed_for_test`], the one
//! `file_bugs` calls — uses `status != 'Closed'`. **The filing path ports
//! legacy's own predicate rather than reusing the open-bug lists'.**
//! `JiraRepository::find_unclosed_for_test`'s doc carries the full argument and
//! the user-visible consequence of the one row class where the two disagree —
//! a bug the poller has resolved still blocks a re-file, even though it no
//! longer appears in [`Self::open_bugs`] or the runner's skip list — and
//! `a_resolved_bug_still_blocks_the_local_refile_probe` (this file's tests) and
//! `infra::storage::jira_sea_repo`'s repository-level test of the same name
//! pin it from both sides.
//!
//! # R86 — the probe takes an explicit tenant, fix round 1's Critical
//!
//! **A first revision of [`JiraRepository::find_unclosed_for_test`] took no
//! `tenant_id` and relied on the compiled scope alone** — the same defect
//! this crate has now written down and fixed three times over
//! (`OrmJiraRepository::get_config`'s own doc, Task 32's original finding and
//! fix; this method, Task 33's original round and this fix). A scope over
//! `OWNER_TENANT_ID` may legitimately span several tenants
//! (`ScopeFilter::In`, `ScopeFilter::InTenantSubtree`), and this method's
//! `.one()` carries no `ORDER BY`, so an unpinned read under a multi-tenant
//! scope returns an arbitrary in-scope tenant's row. For this probe that is
//! worse than the config-read version of the same bug: a foreign tenant's
//! `jira_key` would be handed back to the caller (`created: false`), the
//! caller's *own* failing test would never reach the port at all, and — since
//! [`JiraService::file_bugs`] passes the caller's own tenant to
//! [`JiraRepository::upsert_bug`] on the miss path — a filed bug could be
//! registered under a different tenant than the run it was filed against,
//! invisible to that run's own tenant's [`JiraService::open_bugs`]. Fixed by
//! taking `tenant_id` explicit, calling `validate_tenant_in_scope` and adding
//! the equality predicate — `find_unclosed_for_test`'s own doc carries the
//! rest, and `domain::service::mod`'s header now states this as ruling R86, a
//! standing rule for every `.one()`-over-`OWNER_TENANT_ID` read in this crate,
//! not only this one.
//!
//! # R84 — the issue body's detail text, since this gear has no logs to send
//!
//! Legacy passes `&result.logs` — the captured pytest output for one test file
//! — as `create_or_find_issue`'s `logs` argument, which becomes the JIRA
//! issue's code-block detail. `qa_test_results` carries no such column, and
//! `qa_test_case_results.reason` is the nearest equivalent this gear stores —
//! populated per case by the runner's `vhp_case_reporter` plugin
//! (`manager/src/services/argo.rs:2928-2986`), not only for xfail/skip: the
//! plugin may attach a `reason` to any outcome, `FAILED` included. So
//! [`JiraService::file_bugs`] concatenates the `reason` of every `FAILED`
//! case sharing a failing file's `(run_id, test_file)` — in whatever order
//! [`ResultsRepository::case_rows_for_run`] returns them, since no ordering is
//! meaningful here and nothing about a JIRA description depends on which
//! failure is listed first — rather than sending an empty detail. The
//! alternative — always
//! empty — was rejected because it would make every filed issue equally
//! uninformative regardless of whether the runner actually reported a reason,
//! which is strictly worse than *sometimes* carrying one. The adapter still
//! truncates to legacy's 2000 bytes
//! (`infra::jira::oagw_client::MAX_LOG_CHARS`, ported by Task 32) — this
//! service does not re-implement that truncation, it only builds the
//! unbounded string the adapter is already responsible for cutting.
//!
//! # What Task 33 deliberately does not build
//!
//! No poller (Task 35 still owns `resolve_bug`'s only production caller) and
//! no SDK skip-list provider (Task 34's `QaInsightsClientV1::skip_list_for`,
//! which reads `JiraRepository::list_open_for_plan` through a different
//! surface than [`Self::open_bugs`]'s REST response).

use std::collections::HashMap;
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_insights_sdk::{JiraBug, JiraConfig, JiraPollerConfig, NewJiraBug, TestResultRecord};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::analytics::PlanRef;
use crate::domain::error::DomainError;
use crate::domain::ports::JiraClient;
use crate::domain::ports::jira_client::{
    IssueRef, NewIssue, StatusCategory, validate_credstore_ref,
};
use crate::domain::repos::{JiraRepository, ResultsRepository};
use crate::domain::service::{DbProvider, actions, resources};

/// Legacy's issue type in the all-blank document an unconfigured tenant reads
/// (`manager/src/routes/settings.rs:267`).
///
/// The same literal the adapter falls back to when `issue_type` is `None`
/// (`infra::jira::oagw_client`'s `DEFAULT_ISSUE_TYPE`, legacy `jira.rs:112`), and
/// the duplication is deliberate: one is the *placeholder a form renders* and the
/// other is the *value a request uses*, and collapsing them would couple the
/// settings screen's blank state to the filing path's fallback.
const BLANK_ISSUE_TYPE: &str = "Bug";

/// The smallest poll interval this gear will hand to a sleep. Legacy's
/// `.max(1)` (`manager/src/services/jira_poller.rs:26`).
const MIN_POLL_INTERVAL_SECONDS: u64 = 1;

/// The file-level and case-level status [`JiraService::file_bugs`] filters on.
///
/// Legacy's own literal equality — `if result.status != "FAILED" { continue }`
/// (`manager/src/routes/settings.rs:608-610`) — not the wider "did not pass"
/// this gear's status fold elsewhere uses (`domain::service::ingest::classify`,
/// which also counts `ERROR`). Ported exactly: R84's own detail-text decision
/// reuses this same literal for the case-level fold, on the reading that a
/// case worth quoting in a filed bug is one that failed the same way the
/// file-level row that triggered the filing did.
const STATUS_FAILED: &str = "FAILED";

/// The unvalidated body of a JIRA settings save.
///
/// Legacy takes `Json<JiraConfig>` directly (`settings.rs:280`). A separate
/// domain type here for [`crate::domain::service::saved_views::SavedViewInput`]'s
/// reason — the rules below are testable without a router — and for one specific
/// to this resource: the type the *read* returns and the type the *write* takes
/// are not interchangeable, because an empty
/// [`Self::api_token_credstore_ref`] means "keep what is stored" on the way in
/// and never means anything on the way out. Two names make that impossible to
/// confuse; one shared `JiraConfig` would not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JiraConfigInput {
    pub url: String,
    pub project_key: String,
    pub email: String,
    /// Empty means **keep the stored reference** — see
    /// [`JiraService::save_jira_config`]. Never token material.
    pub api_token_credstore_ref: String,
    pub issue_type: Option<String>,
    pub enabled: bool,
}

/// The tenant's JIRA settings, and the outbound calls that need them.
///
/// Generic over the repository for
/// [`crate::domain::service::saved_views::SavedViewsService`]'s reason:
/// [`JiraRepository`]'s methods are generic over the `DBRunner` they run on, so
/// the trait is not object-safe and the parameter propagates to
/// [`crate::domain::service::AppServices`].
///
/// **Generic over `R: ResultsRepository` too, since Task 33.** This module's
/// header — "`file_bugs` needs a second repository" — says why:
/// [`Self::file_bugs`] resolves a run's plan identity and per-case failure
/// text from `qa_test_results`/`qa_test_case_results`, which is
/// [`ResultsRepository`]'s table pair and not [`JiraRepository`]'s.
pub struct JiraService<J, R> {
    db: Arc<DbProvider>,
    jira: J,
    results: R,
    client: Arc<dyn JiraClient>,
    policy_enforcer: PolicyEnforcer,
}

impl<J, R> JiraService<J, R>
where
    J: JiraRepository,
    R: ResultsRepository,
{
    #[must_use]
    pub fn new(
        db: Arc<DbProvider>,
        jira: J,
        results: R,
        client: Arc<dyn JiraClient>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            jira,
            results,
            client,
            policy_enforcer,
        }
    }

    /// Compile the caller's scope for one settings operation.
    ///
    /// `resource_id` is always `None`, and it is not an omission:
    /// [`resources::JIRA_CONFIG`] declares
    /// [`OWNER_TENANT_ID`](toolkit_security::pep_properties::OWNER_TENANT_ID)
    /// and **not** `RESOURCE_ID`, so there is no id for a policy to constrain and
    /// none for a caller to pass. That constant's own doc carries why this
    /// resource breaks the siblings' both-properties convention.
    ///
    /// No [`super::refuse_scope_beyond_tenant`] call either. That guard exists
    /// for a *measured* fail-open in `ResultsRepository::upsert_run_results`,
    /// whose per-run `DELETE` is scope-filtered while its bulk insert is not, so
    /// a narrower-than-tenant scope makes the two disagree about which rows they
    /// are about. Neither write here has that shape: each is a single
    /// `INSERT … ON CONFLICT (tenant_id) DO UPDATE` over the *same* one row on
    /// both branches, guarded by `validate_tenant_in_scope`, and there is no
    /// second statement for a narrowing to be applied inconsistently across.
    /// `saved_views` reaches the same conclusion for its own writes; that
    /// function's doc records that reading its old "Tasks 28, 33 and 38 must
    /// call this" list as an obligation was the mistake.
    async fn scope(&self, ctx: &SecurityContext, action: &str) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::JIRA_CONFIG, action, None)
            .await?)
    }

    /// The tenant's JIRA settings — `GET /qa/v1/settings/jira`.
    ///
    /// `api_get_jira` (`manager/src/routes/settings.rs:248-275`). An absent row
    /// reads as the blank document legacy builds for it rather than as a 404;
    /// this module's header says why the *repository* still distinguishes them.
    ///
    /// Returns [`JiraConfig::api_token_credstore_ref`] verbatim — a reference,
    /// unmasked, because there is nothing to mask.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure.
    pub async fn get_jira_config(&self, ctx: &SecurityContext) -> Result<JiraConfig, DomainError> {
        let access = self.scope(ctx, actions::GET).await?;
        let conn = self.db.conn()?;
        Ok(self
            .jira
            .get_config(&conn, &access, ctx.subject_tenant_id())
            .await?
            .unwrap_or_else(blank_config))
    }

    /// Store the tenant's JIRA settings — `PUT /qa/v1/settings/jira`.
    ///
    /// `api_update_jira` (`manager/src/routes/settings.rs:278-305`).
    ///
    /// # An omitted credential reference keeps the stored one
    ///
    /// Legacy's write path checks `if config.api_token == "********"` and, when
    /// it matches, reuses `existing.api_token` (`settings.rs:284-295`). The
    /// mechanism exists because a settings form that renders a masked token and
    /// posts the form back must not blank the credential — and that hazard is
    /// unchanged here even though the masking is not: a form that does not
    /// resend the reference would otherwise clear it.
    ///
    /// The **sentinel** cannot survive the port, because nothing on this gear's
    /// read path ever writes one; empty takes its place. With no stored row there
    /// is nothing to preserve and empty stores as empty, which is legacy's own
    /// fall-through (`settings.rs:290-292`, the `else` arm).
    ///
    /// The cost, recorded because it is real: the reference cannot be *cleared*
    /// through this endpoint. Turning [`JiraConfigInput::enabled`] off is how a
    /// tenant stops using JIRA, and it is what every read path checks
    /// ([`Self::active_config`]).
    ///
    /// # The reference's syntax is checked here, not discovered at request time
    ///
    /// **Fix round 1, finding 1.** The stored value is copied verbatim into the
    /// oagw upstream's apikey auth config, where it reaches `SecretRef::new`
    /// after a `cred://` strip — and `SecretRef` accepts `[a-zA-Z0-9_-]` only,
    /// rejecting the slashes and colons a URL-flavoured guess like
    /// `credstore://qa/jira/api-token` carries
    /// (`gears/credstore/credstore-sdk/src/models.rs:42-83`). Unchecked, that
    /// value saves cleanly and then fails inside oagw at *request* time,
    /// uncorrelated with the `PUT` that stored it.
    /// [`validate_credstore_ref`] is the check and the `PUT` route description
    /// states the syntax; the check runs on the **effective** reference, after
    /// the keep-stored substitution above, because that is the value that will be
    /// provisioned.
    ///
    /// An empty effective reference is refused only when
    /// [`JiraConfigInput::enabled`] is true. Disabled-and-empty is exactly what
    /// [`blank_config`] returns to an unconfigured tenant, so refusing it would
    /// make the document this endpoint hands out un-`PUT`-able.
    ///
    /// # The preserving read is pinned to the caller's own tenant
    ///
    /// **Fix round 1, finding 4.** It is
    /// `get_config(.., ctx.subject_tenant_id())`, not `get_config(.., scope)`
    /// alone. A scope over `OWNER_TENANT_ID` may span several tenants
    /// (`ScopeFilter::In`, `ScopeFilter::InTenantSubtree` — a parent-tenant grant
    /// is a supported shape), and the repository's `.one()` carries no `ORDER BY`,
    /// so the unpinned read could return an arbitrary in-scope tenant's row and
    /// this method would then write **another tenant's credential reference**
    /// onto this tenant's config.
    ///
    /// Returns the stored config, so a caller need not re-read to render the form
    /// it just posted.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming `api_token_credstore_ref` for a
    /// reference the credential store could not resolve, or for an absent one on
    /// an enabled integration.
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure.
    pub async fn save_jira_config(
        &self,
        ctx: &SecurityContext,
        input: JiraConfigInput,
    ) -> Result<JiraConfig, DomainError> {
        let access = self.scope(ctx, actions::UPDATE).await?;
        let conn = self.db.conn()?;

        // Explicitly the caller's **own** tenant, never "whichever row the scope
        // happens to admit" — fix round 1, finding 4. See `JiraRepository::get_config`.
        let stored = self
            .jira
            .get_config(&conn, &access, ctx.subject_tenant_id())
            .await?;
        let api_token_credstore_ref = if input.api_token_credstore_ref.is_empty() {
            stored
                .map(|existing| existing.api_token_credstore_ref)
                .unwrap_or_default()
        } else {
            input.api_token_credstore_ref
        };

        // Validated **after** the keep-stored substitution, so the value checked
        // is the value that will be provisioned into the oagw upstream. An empty
        // effective reference is legal only while the integration is off, which
        // is the shape `blank_config` returns and therefore the shape a caller
        // may `PUT` straight back.
        if api_token_credstore_ref.is_empty() {
            if input.enabled {
                return Err(DomainError::Validation {
                    field: "api_token_credstore_ref".to_owned(),
                    message: "a credential-store reference is required to enable the JIRA \
                              integration"
                        .to_owned(),
                });
            }
        } else {
            validate_credstore_ref("api_token_credstore_ref", &api_token_credstore_ref)?;
        }

        let config = JiraConfig {
            url: input.url,
            project_key: input.project_key,
            email: input.email,
            api_token_credstore_ref,
            issue_type: input.issue_type,
            enabled: input.enabled,
        };
        self.jira
            .save_config(&conn, &access, ctx.subject_tenant_id(), config.clone())
            .await?;
        Ok(config)
    }

    /// The tenant's JIRA settings **only when they are usable**, else `None`.
    ///
    /// The one predicate every caller that wants to *talk to* JIRA should go
    /// through, and the port of legacy's poller short-circuit:
    /// `if config.is_none() || !config.as_ref().unwrap().enabled { return Ok(()) }`
    /// (`manager/src/services/jira_poller.rs:40-43`) — an `Ok`, not an error, and
    /// the two conditions folded into one answer.
    ///
    /// Task 35's poller calls this once per pass before it looks at any bug, and
    /// Task 33's filing path calls it before it files. Neither should re-derive
    /// the predicate: an `enabled` check written twice is an `enabled` check that
    /// can be forgotten once.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure. **Never** for a missing or
    /// disabled config — that is the whole point.
    pub async fn active_config(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Option<JiraConfig>, DomainError> {
        let access = self.scope(ctx, actions::GET).await?;
        let conn = self.db.conn()?;
        Ok(self
            .jira
            .get_config(&conn, &access, ctx.subject_tenant_id())
            .await?
            .filter(|config| config.enabled))
    }

    /// The tenant's poller cadence and auto-rerun switch, defaulted and clamped.
    ///
    /// `api_get_jira_poller` (`manager/src/routes/settings.rs:510-524`) reads
    /// through `get_or_default`, so an absent row is
    /// [`JiraPollerConfig::default`] — 300 seconds, auto-rerun on. The
    /// [`MIN_POLL_INTERVAL_SECONDS`] clamp is applied here and nowhere else; this
    /// module's header and controller ruling R71 carry why it is neither in the
    /// DDL nor in the mapper nor on the write.
    ///
    /// **This is the reader Task 35 consumes.** It must not add one of its own.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::CorruptState`] if the stored interval is negative.
    /// [`DomainError::Database`] on a query failure.
    pub async fn poller_config(
        &self,
        ctx: &SecurityContext,
    ) -> Result<JiraPollerConfig, DomainError> {
        let access = self.scope(ctx, actions::GET).await?;
        let conn = self.db.conn()?;
        let mut config = self
            .jira
            .get_poller_config(&conn, &access, ctx.subject_tenant_id())
            .await?
            .unwrap_or_default();
        config.poll_interval_seconds = config.poll_interval_seconds.max(MIN_POLL_INTERVAL_SECONDS);
        Ok(config)
    }

    /// Store the tenant's poller settings.
    ///
    /// `api_update_jira_poller` (`manager/src/routes/settings.rs:527-542`). The
    /// value is stored as given — see [`Self::poller_config`] for why the clamp
    /// is on the read.
    ///
    /// Returns the stored config for [`Self::save_jira_config`]'s reason.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Validation`] if the interval does not fit the column.
    /// [`DomainError::Database`] on a query failure.
    pub async fn save_poller_config(
        &self,
        ctx: &SecurityContext,
        config: JiraPollerConfig,
    ) -> Result<JiraPollerConfig, DomainError> {
        let access = self.scope(ctx, actions::UPDATE).await?;
        let conn = self.db.conn()?;
        self.jira
            .save_poller_config(&conn, &access, ctx.subject_tenant_id(), config)
            .await?;
        Ok(config)
    }

    /// One issue's status category, read from the tenant's JIRA instance.
    ///
    /// Legacy `check_jira_status` (`manager/src/services/jira.rs:254-289`),
    /// including its config resolution: legacy re-reads the config inside the
    /// call and raises `"JIRA is not configured"` when there is none
    /// (`:258-262`). This does the same, so a caller never handles a config.
    ///
    /// **The answer is a category, not a status** —
    /// [`StatusCategory::is_resolved`] is the poller's test
    /// (`manager/src/services/jira_poller.rs:57`).
    ///
    /// # Errors
    ///
    /// [`DomainError::JiraNotConfigured`] when the tenant has no config or has
    /// disabled it. Distinct from a call that failed: nothing was attempted, and
    /// [`Self::active_config`] is what a caller should use to decide whether to
    /// ask at all.
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Internal`] when the JIRA instance or the gateway failed.
    pub async fn check_status(
        &self,
        ctx: &SecurityContext,
        jira_key: &str,
    ) -> Result<StatusCategory, DomainError> {
        let config = self
            .active_config(ctx)
            .await?
            .ok_or(DomainError::JiraNotConfigured)?;
        self.client.check_status(ctx, &config, jira_key).await
    }

    /// Mark a bug resolved — Task 35's poller, the production caller
    /// [`JiraRepository::resolve_bug`]'s own doc names.
    ///
    /// **The resolution is recorded whether or not auto-rerun is on.** Legacy
    /// calls the equivalent write at `manager/src/services/jira_poller.rs:59`,
    /// *before* the `auto_rerun_on_resolve` gate at `:61-63` — turning the
    /// switch off must not stop bugs closing, and this method carries no gate
    /// of its own for the caller to route around. `at` is the caller's
    /// instant rather than one read fresh here, so a whole poll pass stamps
    /// every bug it resolves with the same value —
    /// [`JiraRepository::resolve_bug`]'s own doc states why that matters.
    ///
    /// Authorized under [`Self::bug_scope`] and `actions::UPDATE`: the same
    /// `qa_jira_bugs` resource [`Self::open_bugs`] and [`Self::file_bugs`]
    /// already compile a scope over, for a write rather than a read on it.
    ///
    /// # `ctx.subject_tenant_id()` is passed through — R86, Phase C's final
    /// # review, Critical 1
    ///
    /// The compiled scope is not the tenant pin, and this is a *write*: under a
    /// grant over `InTenantSubtree`, an unpinned
    /// [`JiraRepository::resolve_bug`] resolved every in-scope tenant's row
    /// carrying the same `jira_key`, which two tenants are expected to produce.
    /// That repository method's own doc carries the whole failure chain; the
    /// service's obligation is only to name the tenant, exactly as
    /// [`Self::latest_version_for_plan`] and [`Self::file_bugs`] already do for
    /// their own reads.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure. **Never** for a
    /// `jira_key` naming no visible open bug — see the return value.
    pub async fn resolve_bug(
        &self,
        ctx: &SecurityContext,
        jira_key: &str,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let access = self.bug_scope(ctx, actions::UPDATE).await?;
        let conn = self.db.conn()?;
        self.jira
            .resolve_bug(&conn, &access, ctx.subject_tenant_id(), jira_key, at)
            .await
    }

    /// The most recent `product_version` reported for a plan, across every
    /// ingested run that carries one — Task 35's `check_new_build` predicate.
    ///
    /// Legacy's own query, a single targeted row: `SELECT app_version FROM
    /// run_results WHERE plan_id = $1 AND app_version IS NOT NULL ORDER BY
    /// created_at DESC LIMIT 1` (`manager/src/services/jira_poller.rs:231-233`,
    /// inside `check_new_build`, `:225-243`).
    ///
    /// # Pushed into the query, not filtered in Rust — fix round 1, Important 2
    ///
    /// **A first revision of this method called
    /// [`ResultsRepository::list_for_plan`] from
    /// [`OffsetDateTime::UNIX_EPOCH`] and reduced with `.find_map` in Rust.**
    /// That method's window exists precisely so a per-plan read never scans
    /// the whole plan unbounded (its own doc, and `cpt-cf-qa-nfr-scale`'s
    /// 5M-row target); `UNIX_EPOCH` disabled that window entirely, and
    /// `list_for_plan` has no `LIMIT`, so **every row ever recorded for the
    /// plan crossed the wire** on every poll pass, for every open bug — a
    /// background job's steady-state cost, not a one-off. Rejected in favour
    /// of [`ResultsRepository::latest_version_for_plan`], which expresses
    /// legacy's own shape (`product_version IS NOT NULL`, newest first,
    /// `LIMIT 1`/`.one()`) in the query, so this method's own read is one row
    /// transferred rather than the plan's whole history — see that method's
    /// doc for why an unwindowed `.one()` is still the right shape, and for
    /// the `tenant_id` parameter R86 requires of it.
    ///
    /// Authorized under [`Self::results_scope`] — the same
    /// [`resources::TEST_RESULT`] scope [`Self::file_bugs`] compiles for the
    /// identical table, R87's rule applied to a second caller of it.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure.
    pub async fn latest_version_for_plan(
        &self,
        ctx: &SecurityContext,
        plan_path: &str,
    ) -> Result<Option<String>, DomainError> {
        let access = self.results_scope(ctx).await?;
        let conn = self.db.conn()?;
        self.results
            .latest_version_for_plan(&conn, &access, ctx.subject_tenant_id(), plan_path)
            .await
    }

    /// Compile the caller's scope for one bug-registry operation.
    ///
    /// [`Self::scope`]'s twin, over [`resources::JIRA_BUG`] rather than
    /// [`resources::JIRA_CONFIG`] — a separate resource per that constant's own
    /// doc, since a deployment may grant one without the other. `resource_id`
    /// is `None` for the identical reason `JIRA_BUG` declares no
    /// [`RESOURCE_ID`](toolkit_security::pep_properties::RESOURCE_ID): neither
    /// of this task's two operations addresses a bug by its own row id.
    async fn bug_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::JIRA_BUG, action, None)
            .await?)
    }

    /// Compile the caller's scope for the `qa_test_results`/
    /// `qa_test_case_results` reads [`Self::file_bugs`] makes on the way to
    /// filing — **fix round 1, Important 2, controller ruling R87**.
    ///
    /// A separate scope from [`Self::bug_scope`], compiled over
    /// [`resources::TEST_RESULT`] / [`actions::LIST`] rather than
    /// [`resources::JIRA_BUG`]. Every other reader of these two tables in this
    /// crate compiles its scope this way (`domain::service::results`,
    /// `dashboard`, `analytics`, `collect`, `reconcile` all do), and
    /// `domain::service::mod`'s own header states the rule this follows when
    /// it explains why `qa_test_case_collect` folds into [`resources::TEST_RESULT`]
    /// rather than getting a resource type of its own: one scope per resource
    /// type, reused only when the second table has no independent lifecycle
    /// of its own. `qa_test_results`/`qa_test_case_results` are not that case —
    /// they are the resource every other read in this gear already gates —
    /// so reusing [`Self::bug_scope`]'s compiled scope here would let a grant
    /// of `qa.jira_bug/create` alone read a run's rows and ship per-case
    /// `reason` text out to JIRA with no `qa.test_result` grant at all, and
    /// would apply the wrong filter for a deployment that scopes the two
    /// resources differently.
    async fn results_scope(&self, ctx: &SecurityContext) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_RESULT, actions::LIST, None)
            .await?)
    }

    /// Open bugs, optionally narrowed to one plan — `GET /qa/v1/jira/open-bugs`.
    ///
    /// `api_open_bugs` (`manager/src/routes/settings.rs:634-651`). Legacy takes
    /// one optional `plan_id` and falls back to `get_all_open_bugs` when it is
    /// absent (`:641-643`); this takes the `(repo_id, plan_path)` pair
    /// [`JiraRepository::list_open_for_plan`] and
    /// `qa_insights_sdk::QaInsightsClientV1::skip_list_for` already speak.
    ///
    /// Controller ruling R85: this is **not** the analytics drill-downs'
    /// single `plan_id` token, which matches a path across every repository
    /// the caller's scope admits. That is right for analytics and wrong here —
    /// a bug that suppresses a test at launch must be the same bug this
    /// endpoint lists, so the identity has to be exact, not "any repository
    /// with this path".
    ///
    /// # Both listings are pinned to the caller's own tenant — R86, Phase C's
    /// # final review, Critical 1b
    ///
    /// `ctx.subject_tenant_id()` is passed to both
    /// [`JiraRepository::list_open`] and
    /// [`JiraRepository::list_open_for_plan`], so "every open bug in the
    /// tenant" below is the statement's behaviour and not only its prose. Until
    /// that review neither read carried a `tenant_id` predicate, and a scope
    /// spanning several tenants therefore made this method enumerate the whole
    /// subtree's bugs — for the HTTP endpoint, for
    /// `QaInsightsClientV1::skip_list_for`'s runner environment, and for
    /// [`JiraPollerService::poll_once`](crate::domain::service::jira_poller::JiraPollerService::poll_once),
    /// whose contract is one tenant per pass. The two repository methods'
    /// own docs carry what each unpinned read cost.
    ///
    /// **This narrows the endpoint for a parent-tenant caller**, deliberately:
    /// a grant over `InTenantSubtree(P)` now lists P's own bugs rather than the
    /// subtree's. The alternative — pinning only the poller's call — would put
    /// the tenant pin in a different layer depending on which caller asked, and
    /// leave nothing at all forcing the next caller to choose.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming `plan_path` when exactly one of
    /// `repo_id`/`plan_path` is supplied — R85's "one without the other is a
    /// 400". Both present narrows to that plan; both absent lists every open
    /// bug in the tenant.
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure.
    pub async fn open_bugs(
        &self,
        ctx: &SecurityContext,
        repo_id: Option<Uuid>,
        plan_path: Option<&str>,
    ) -> Result<Vec<JiraBug>, DomainError> {
        let plan = optional_plan_ref(repo_id, plan_path)?;
        let access = self.bug_scope(ctx, actions::LIST).await?;
        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();
        match plan {
            Some(plan) => {
                self.jira
                    .list_open_for_plan(&conn, &access, tenant_id, &plan)
                    .await
            }
            None => self.jira.list_open(&conn, &access, tenant_id).await,
        }
    }

    /// File (or find) a JIRA bug for every failed test of a run —
    /// `POST /qa/v1/jira/bugs`.
    ///
    /// `api_create_jira_ticket` (`manager/src/routes/settings.rs:581-631`).
    /// `test_name: None` files against every `FAILED` result of the run;
    /// `Some` narrows to one. Legacy's per-test loop swallows and logs a
    /// single test's failure and continues (`:624-627`); this does the same —
    /// a partial success is still `Ok`, with fewer entries than failed tests,
    /// never a bulk error.
    ///
    /// This module's header carries the two decisions specific to this
    /// method in full: **R80** (why the local dedupe probe is
    /// [`JiraRepository::find_unclosed_for_test`] and not the open-bug lists'
    /// predicate) and **R84** (why the issue body's detail is the failing
    /// cases' concatenated `reason`, not an empty string).
    ///
    /// # What "swallowed" covers, precisely
    ///
    /// A test skipped for this reason **never appears** in the returned list —
    /// matching legacy's `continue`, not a `created: false` entry, because no
    /// `IssueRef` was ever produced for it:
    ///
    /// * No plan identity to file against (`repo_id`/`plan_path` absent on the
    ///   row — a custom-plan or collect run). Legacy's `run.plan_id` always
    ///   has a value, however synthetic; this schema's
    ///   `qa_jira_bugs.repo_id`/`plan_path` are `NOT NULL` and there is no
    ///   equivalent slug to store instead, so this is a deliberate, narrower
    ///   divergence from legacy's own always-succeeds shape.
    /// * JIRA is unconfigured or disabled ([`Self::active_config`] answers
    ///   `None`) — legacy's identical refusal (`jira.rs:65-67`), read once per
    ///   request rather than once per test as legacy's inlined check does; the
    ///   outward answer is the same empty-for-every-remaining-test result,
    ///   with one config read instead of *N* identical ones.
    /// * The port's own two calls failed — a transport or gateway error from
    ///   [`JiraClient::create_or_find_issue`].
    ///
    /// # What is never swallowed
    ///
    /// A run with **no** ingested rows at all is
    /// [`DomainError::RunNotIngested`], not an empty list — this gear has no
    /// Argo workflow to 404 against the way legacy's `state.argo.get_workflow`
    /// does, so an unprojected run and a run with zero failures would
    /// otherwise be indistinguishable. A run whose rows exist but include no
    /// `FAILED` test (or none matching a supplied `test_name`) *is* an empty
    /// list — legacy's own outcome for the identical case, since its loop
    /// simply has nothing to iterate.
    ///
    /// **Nor is a PDP denial on the config read — fix round 1, Important 4.**
    /// [`Self::active_config`] compiles its own scope over
    /// [`resources::JIRA_CONFIG`]/[`actions::GET`], read once before any
    /// per-test work starts; a subject holding `qa.jira_bug/create` but not
    /// `qa.jira_config/get` gets a bulk [`DomainError::Forbidden`] for the
    /// whole request, not a config-answers-`None`-style per-test swallow. This
    /// is distinct from "JIRA is unconfigured or disabled" above, which is
    /// [`Self::active_config`] answering `Ok(None)` rather than `Err(..)` and
    /// *is* swallowed per test.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotIngested`] when `run_id` has no projected rows.
    /// [`DomainError::Forbidden`] when the PDP denies `qa.jira_bug/create`,
    /// `qa.test_result/list` (the results read this method also makes — see
    /// [`Self::results_scope`]) or `qa.jira_config/get` (the config read
    /// [`Self::active_config`] makes before filing starts) — all three are
    /// grants this endpoint requires, not only the first.
    /// [`DomainError::Database`] on a query failure.
    pub async fn file_bugs(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        test_name: Option<&str>,
    ) -> Result<Vec<IssueRef>, DomainError> {
        let access = self.bug_scope(ctx, actions::CREATE).await?;
        // A second, separately-compiled scope for the two results tables —
        // R87. `access` above authorizes the registry write; this authorizes
        // the read of `qa_test_results`/`qa_test_case_results` this method
        // makes on the way to it, exactly like every other reader of those
        // tables in this crate.
        let results_access = self.results_scope(ctx).await?;
        let conn = self.db.conn()?;

        let rows = self
            .results
            .list_by_run(&conn, &results_access, run_id)
            .await?;
        if rows.is_empty() {
            return Err(DomainError::RunNotIngested { run_id });
        }

        let failed: Vec<TestResultRecord> = rows
            .into_iter()
            .filter(|row| row.status == STATUS_FAILED)
            .filter(|row| test_name.is_none_or(|name| row.test_name == name))
            .collect();
        if failed.is_empty() {
            return Ok(Vec::new());
        }

        let reasons = self
            .failure_detail_by_file(&conn, &results_access, run_id)
            .await?;

        // Read once, not once per test: the tenant's JIRA settings do not
        // change mid-request, and legacy's own per-call check
        // (`create_or_find_issue` re-reads on every invocation) would only
        // reproduce *N* identical log lines here for no behavioural
        // difference — see this method's own doc.
        let config = self.active_config(ctx).await?;

        let mut outcomes = Vec::with_capacity(failed.len());
        for row in &failed {
            match self
                .file_one(ctx, &access, &conn, row, config.as_ref(), &reasons)
                .await
            {
                Ok(issue) => outcomes.push(issue),
                Err(err) => {
                    tracing::error!(
                        test_name = %row.test_name,
                        run_id = %run_id,
                        error = %err,
                        "failed to file a JIRA bug for a failed test; skipping \
                         (legacy manager/src/routes/settings.rs:624-627)",
                    );
                }
            }
        }
        Ok(outcomes)
    }

    /// One failing test's local-dedupe-then-file step of [`Self::file_bugs`].
    async fn file_one<C: DBRunner>(
        &self,
        ctx: &SecurityContext,
        access: &AccessScope,
        conn: &C,
        row: &TestResultRecord,
        config: Option<&JiraConfig>,
        reasons: &HashMap<String, String>,
    ) -> Result<IssueRef, DomainError> {
        // Step 1 of legacy's `create_or_find_issue` (`jira.rs:43-56`) — R80's
        // predicate. `ctx.subject_tenant_id()` is explicit — fix round 1,
        // Critical 1 (R86) — so a multi-tenant scope cannot make this probe
        // answer with another tenant's row; it is the same tenant `upsert_bug`
        // below writes under, so the probe and the write can never disagree
        // about whose bug this is.
        if let Some(existing) = self
            .jira
            .find_unclosed_for_test(conn, access, ctx.subject_tenant_id(), &row.test_name)
            .await?
        {
            return Ok(IssueRef {
                jira_key: existing.jira_key,
                created: false,
            });
        }

        // Step 2 (`jira.rs:58-67`), pre-fetched by the caller.
        let config = config.ok_or(DomainError::JiraNotConfigured)?;

        let (repo_id, plan_path) =
            row.repo_id
                .zip(row.plan_path.clone())
                .ok_or_else(|| DomainError::Validation {
                    field: "plan_path".to_owned(),
                    message: format!(
                        "test '{}' has no plan identity in this run's projection and cannot be \
                     filed against qa_jira_bugs, whose repo_id/plan_path are not nullable",
                        row.test_name
                    ),
                })?;

        let logs = reasons.get(&row.test_file).cloned().unwrap_or_default();
        let issue = NewIssue {
            test_name: row.test_name.clone(),
            plan: plan_path.clone(),
            app_version: row.product_version.clone(),
            // No resolved display name: this gear denormalizes only
            // `platform_id` (a UUID) on `qa_test_results`, not a platform
            // name, and resolving one would mean handing this service a
            // `PlatformReader` dependency for a value that is prose in the
            // issue body only — never matched on, never stored. The port's
            // own `None` fallback (legacy's `"default"`, `jira.rs:129`)
            // already covers the absence. `platform_id` itself is not lost:
            // it goes onto `NewJiraBug::platform_id` below, which is what the
            // registry and the skip list actually need.
            platform: None,
            run_name: row.run_id.to_string(),
            logs,
        };

        // Steps 3-7 (`jira.rs:70-187`), minus the two local-DB steps that are
        // this method's, not the port's — `domain::ports::jira_client`'s own
        // header states the split.
        let result = self.client.create_or_find_issue(ctx, config, issue).await?;

        // `track_bug`, called on **both** of the port's dedupe-hit and
        // created outcomes (`jira.rs:92-101`, `:177-187`) — `upsert_bug`'s
        // `DO NOTHING` makes the first of those two calls a no-op rather than
        // a hazard.
        //
        // **Best-effort, not propagated — fix round 1, Important 3
        // (controller ruling R88).** Legacy is `let _ =
        // self.track_bug(...)` on both of these call sites: local tracking
        // failing does not undo an issue that already exists in JIRA. An
        // earlier revision of this method let the `?` propagate, which is
        // *stricter* than legacy and, on this exact error path, *worse*: the
        // issue the port just created or found is real in JIRA, but
        // propagating drops it from `file_bugs`' response and leaves no local
        // row behind, so the very next call reaches the port again and
        // duplicates it — the outcome the local probe above exists to
        // prevent. Logged at error so the miss is visible; the caller still
        // gets the key.
        if let Err(err) = self
            .jira
            .upsert_bug(
                conn,
                access,
                ctx.subject_tenant_id(),
                NewJiraBug {
                    jira_key: result.jira_key.clone(),
                    test_name: row.test_name.clone(),
                    repo_id,
                    plan_path,
                    app_version: row.product_version.clone(),
                    platform_id: row.platform_id,
                    summary: bug_summary(&row.test_name),
                },
            )
            .await
        {
            tracing::error!(
                jira_key = %result.jira_key,
                test_name = %row.test_name,
                error = %err,
                "failed to register a filed/found JIRA issue locally; the issue itself still \
                 exists (or was already found) in JIRA and is still reported to the caller \
                 (legacy manager/src/services/jira.rs:92-101, :177-187, `let _ = self.track_bug(...)`)",
            );
        }

        Ok(result)
    }

    /// The failing cases' concatenated `reason`, by `test_file` — R84.
    ///
    /// One read for the whole run rather than one per failing file: the run's
    /// case rows are already bounded (one run), and a per-file query would be
    /// *N* round trips for the same table scan.
    async fn failure_detail_by_file<C: DBRunner>(
        &self,
        conn: &C,
        access: &AccessScope,
        run_id: Uuid,
    ) -> Result<HashMap<String, String>, DomainError> {
        let cases = self.results.case_rows_for_run(conn, access, run_id).await?;
        let mut by_file: HashMap<String, Vec<String>> = HashMap::new();
        for case in cases {
            if case.status != STATUS_FAILED {
                continue;
            }
            if let Some(reason) = case.reason {
                by_file.entry(case.test_file).or_default().push(reason);
            }
        }
        Ok(by_file
            .into_iter()
            .map(|(file, reasons)| (file, reasons.join("\n\n")))
            .collect())
    }
}

/// The document an unconfigured tenant reads, field for field with legacy's
/// `Ok(None)` arm (`manager/src/routes/settings.rs:262-269`).
fn blank_config() -> JiraConfig {
    JiraConfig {
        url: String::new(),
        project_key: String::new(),
        email: String::new(),
        api_token_credstore_ref: String::new(),
        issue_type: Some(BLANK_ISSUE_TYPE.to_owned()),
        enabled: false,
    }
}

/// Pair `repo_id` and `plan_path` into a [`PlanRef`], enforcing R85's
/// "together or neither" rule on `GET /qa/v1/jira/open-bugs`.
///
/// [`crate::domain::service::saved_views::required_plan`]'s shape, without
/// that function's `scope=all` short-circuit — there is no `scope` parameter
/// here, only the pair itself, so both absent is a legal request (list every
/// open bug) rather than a case to discard.
///
/// # Errors
///
/// [`DomainError::Validation`] naming `plan_path` when exactly one of the two
/// is present.
fn optional_plan_ref(
    repo_id: Option<Uuid>,
    plan_path: Option<&str>,
) -> Result<Option<PlanRef>, DomainError> {
    let plan_path = plan_path
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    match (repo_id, plan_path) {
        (None, None) => Ok(None),
        (Some(repo_id), Some(plan_path)) => Ok(Some(PlanRef { repo_id, plan_path })),
        _ => Err(DomainError::Validation {
            field: "plan_path".to_owned(),
            message: "repo_id and plan_path must be supplied together, or not at all".to_owned(),
        }),
    }
}

/// The stored bug's summary text — the same string the adapter sends JIRA
/// as the issue summary (`infra::jira::oagw_client::summary_for`, legacy
/// `jira.rs:113` for the wording and `:99`/`:185` for both of its call sites).
///
/// Duplicated rather than shared, [`BLANK_ISSUE_TYPE`]'s reason: the domain
/// layer must not depend on the infra adapter. `pub(crate)`, not private, so
/// `infra::jira::oagw_client`'s own test module can pin the two literals
/// together — `the_stored_summary_matches_the_one_sent_to_jira` — rather than
/// leaving them to drift apart unnoticed.
pub(crate) fn bug_summary(test_name: &str) -> String {
    format!("[VHP] Test Failed: {test_name}")
}

#[cfg(test)]
#[path = "jira_tests.rs"]
mod tests;
