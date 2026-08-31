//! The JIRA bug registry.

use async_trait::async_trait;
use qa_insights_sdk::{JiraBug, JiraConfig, JiraPollerConfig, NewJiraBug};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::analytics::PlanRef;
use crate::domain::error::DomainError;

/// Persistence for `qa_jira_bugs`.
///
/// This is the gear's *own* registry of bugs filed against tests, distinct from
/// `qa_test_results.jira_key`, which is whatever reference the runner happened
/// to report on a file.
///
/// # The two configuration singletons live here too, decided by Task 32
///
/// This header used to say `qa_jira_config` and `qa_jira_poller_config` had
/// entities and no trait method, and that Task 32 — the JIRA settings surface —
/// should decide whether they belong here or on a settings repository of their
/// own. **They belong here**, and the deciding argument is
/// [`NotifyRepository`](super::NotifyRepository): it already covers three tables
/// — the send-once claims, the audit log *and* `qa_notification_config` — so a
/// repository owning both a domain area's rows and that area's per-tenant
/// settings is this gear's established shape rather than a new one. A
/// `JiraSettingsRepository` would have been a second trait over two singleton
/// tables in the same `qa_jira_*` family, a second unit struct in
/// `infra::storage`, and a sixth type parameter on
/// [`AppServices`](crate::domain::service::AppServices) — machinery bought with
/// nothing to spend it on.
///
/// So the trait is ten methods: six about bugs and four about settings, with
/// the two halves separated below. The two halves have no shared statement and
/// no shared scope; nothing about the split is a transaction boundary.
///
/// **Task 35 must not add a poller-config reader of its own** —
/// [`Self::get_poller_config`] is that reader, and it exists in this commit
/// specifically so the poller consumes it rather than growing a parallel one.
///
/// # Controller ruling R86 — every statement whose effect another tenant's row
/// # could change takes an explicit `tenant_id`
///
/// A standing rule for this crate, not only this trait, added after the
/// identical defect was found and fixed three times running: `get_config`
/// (Task 32's original design), its own fix round, and
/// [`Self::find_unclosed_for_test`] (Task 33's fix round 1, Critical 1). A
/// scope over `OWNER_TENANT_ID` may legitimately span several tenants
/// (`ScopeFilter::In`, `ScopeFilter::InTenantSubtree` — a parent-tenant grant
/// is a supported shape, not a hypothetical), and every `.one()` read in this
/// trait's `infra::storage` implementation carries no `ORDER BY`. There is no
/// case in this gear where "whichever in-scope row the engine hands back
/// first" is the right answer to give a caller, so every such read takes
/// `tenant_id: Uuid`, calls `validate_tenant_in_scope(tenant_id, scope)` and
/// adds an explicit `tenant_id` equality predicate — [`Self::get_config`] and
/// [`Self::find_unclosed_for_test`] are this trait's two examples; any new
/// singleton-shaped or dedupe-shaped `.one()` read added to this trait, or to
/// any other repository in this crate, must be checked against this rule
/// before it ships.
///
/// ## The rule was written as "`.one()`", and that spelling had a hole
///
/// **Phase C's final review found two more instances of R86 in this file and
/// neither of them was a `.one()`.** The rule's *reason* — a compiled scope is
/// not a tenant pin, and `refuse_scope_beyond_tenant` deliberately exempts the
/// very scope shapes that span tenants — has nothing to do with how many rows a
/// statement touches:
///
/// * [`Self::resolve_bug`] was an unpinned `update_many`. A `jira_key` collides
///   across tenants by design (see [`Self::find_by_key`]), so one tenant's
///   poller pass resolved **every** in-scope tenant's row carrying that key. A
///   write that hits N rows instead of one is strictly worse than a read that
///   returns the wrong one, and a rule phrased about `.one()` does not mention
///   writes at all.
/// * [`Self::list_open`] and [`Self::list_open_for_plan`] were unpinned `.all()`
///   reads. A listing that honours a wide scope is legitimate *in general* —
///   that is what a parent-tenant grant is for — but both of these feed
///   single-tenant consumers:
///   [`JiraPollerService::poll_once`](crate::domain::service::jira_poller::JiraPollerService::poll_once),
///   whose own contract is "one tenant-scoped pass", and the SDK skip-list
///   provider, which renders the pairs it gets into one tenant's runner
///   environment.
///
/// So the rule, as it applies to this trait: **a method takes `tenant_id: Uuid`,
/// validates it against the scope and carries a `tenant_id` predicate unless its
/// result legitimately spans every tenant the scope admits *and* every one of
/// its consumers wants that.** All ten methods here now do.
#[async_trait]
pub trait JiraRepository: Send + Sync {
    /// Every open bug in the tenant.
    ///
    /// Legacy `get_all_open_bugs` (`manager/src/services/jira.rs:232-241`),
    /// whose predicate is `status = 'Open'` — a literal string match, not
    /// `resolved_at IS NULL`. The two can disagree: a bug the poller has
    /// resolved has both a `resolved_at` *and* a `status` of `'Resolved'`, but
    /// `status` is free JIRA workflow text and an instance may report something
    /// else entirely. Legacy keys on `status`, so this does too.
    ///
    /// # `tenant_id` is a parameter even though this returns a `Vec` — R86,
    /// # Phase C's final review, Critical 1b
    ///
    /// "In the tenant" is this method's *first sentence*, and until that review
    /// the statement did not say so: the `.all()` carried no `tenant_id`
    /// predicate, so under a scope spanning several tenants
    /// (`ScopeFilter::In`, `ScopeFilter::InTenantSubtree`) it enumerated every
    /// in-scope tenant's open bugs.
    ///
    /// A wide `.all()` is not a defect everywhere — a listing endpoint honouring
    /// a parent-tenant grant is the shape that grant exists for. It is a defect
    /// *here*, because of who reads it:
    /// [`JiraPollerService::poll_once`](crate::domain::service::jira_poller::JiraPollerService::poll_once)
    /// is the only consumer besides `GET /qa/v1/jira/open-bugs`, and its own
    /// module doc opens with "one tenant-scoped pass". An unpinned listing made
    /// that false in a way no reader of the poller could see: the pass checked
    /// another tenant's `jira_key` against **its own** `qa_jira_config` — which
    /// *is* tenant-pinned, so genuinely the wrong JIRA instance — and could
    /// launch a rerun for the other tenant's `repo_id` under its own context.
    ///
    /// The narrowing is deliberate and it does reach the HTTP listing:
    /// `GET /qa/v1/jira/open-bugs` now answers with the caller's own tenant's
    /// bugs under a parent-tenant grant rather than the subtree's. That matches
    /// what this method's own first line, and
    /// [`JiraService::open_bugs`](crate::domain::service::jira::JiraService::open_bugs)'
    /// own doc ("both absent lists every open bug in the tenant"), have always
    /// claimed; legacy is single-tenant, so there is no legacy breadth being
    /// dropped.
    async fn list_open<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<JiraBug>, DomainError>;

    /// Every open bug filed against one plan.
    ///
    /// Legacy `get_open_bugs(plan_id)` (`jira.rs:220-229`), which feeds the
    /// launch path's `SKIP_TESTS_WITH_BUGS` assembly. Legacy's `plan_id` is a
    /// path-derived slug matched as an opaque string; this port keys on the
    /// `(repo_id, plan_path)` pair, which is lossless where the slug is not.
    ///
    /// An empty result means the environment variable is **absent**, not empty
    /// — legacy guards that twice (`runs.rs:753-761` and `argo.rs:477-479`) and
    /// a test repository can tell the difference. The rendering belongs to the
    /// caller; see `qa_insights_sdk::SkipListEntry`.
    ///
    /// # `tenant_id` is a parameter — R86, Phase C's final review, Critical 1b
    ///
    /// [`Self::list_open`]'s reason, with a second consumer that makes it
    /// sharper: this is the read behind
    /// `qa_insights_sdk::QaInsightsClientV1::skip_list_for`, whose pairs qa-runs
    /// renders into one run's `SKIP_TESTS_WITH_BUGS` environment variable. An
    /// unpinned `.all()` under a parent-tenant grant would put a *child*
    /// tenant's `test_name:JIRA-KEY` pairs into the parent's runner
    /// environment — a cross-tenant read of registry data leaving this gear on
    /// a wire the frozen format
    /// (`cpt-cf-qa-fr-migration-runner-contract`) gives no way to filter.
    async fn list_open_for_plan<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        plan: &PlanRef,
    ) -> Result<Vec<JiraBug>, DomainError>;

    /// Register a bug, or leave an existing one untouched.
    ///
    /// Legacy is `INSERT ... ON CONFLICT (jira_key) DO NOTHING`
    /// (`jira.rs:206-215`), and the `DO NOTHING` is the behaviour, not an
    /// optimisation: re-filing an existing key must not overwrite the summary,
    /// the status or a `resolved_at` the poller has already written. The
    /// conflict target here is `idx_qa_jira_bugs_tenant_key`, which is
    /// `(tenant_id, jira_key)` — legacy's index is global, and a JIRA key is
    /// not a secret, so without the tenant prefix one tenant filing `VHP-1`
    /// would permanently deny it to every other tenant and learn from the error
    /// whether someone else had filed it first.
    ///
    /// Returns the row that ended up stored, existing or new.
    async fn upsert_bug<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        bug: NewJiraBug,
    ) -> Result<JiraBug, DomainError>;

    /// The local re-file dedupe probe: any bug filed against this test that
    /// is not `'Closed'`, first match, no tiebreak.
    ///
    /// Legacy `create_or_find_issue`'s own local check — **a third status
    /// predicate**, distinct from both [`Self::list_open`]'s `status = 'Open'`
    /// and [`Self::resolve_bug`]'s `status = 'Resolved'` write: `SELECT
    /// jira_key FROM jira_bugs WHERE test_name = $1 AND status != 'Closed'
    /// LIMIT 1` (`manager/src/services/jira.rs:43-49`), with no `ORDER BY`
    /// under that `LIMIT 1` — ported verbatim, including the absent ordering,
    /// which is why this method's own contract makes no promise about *which*
    /// row it returns when more than one matches.
    ///
    /// # Why `!= 'Closed'` and not `list_open`'s `= 'Open'` — controller ruling
    /// # R80, decided by Task 33
    ///
    /// The two predicates disagree on exactly one class of row: a bug the
    /// poller has resolved (`status = 'Resolved'`, `'Resolved' != 'Closed'`
    /// but `'Resolved' != 'Open'`). Task 33 ports legacy's own predicate
    /// rather than reusing `list_open`'s, because this probe answers a
    /// different question than the open-bug lists do: it exists to stop this
    /// gear from filing a **second JIRA issue** for a test whose first issue
    /// JIRA has not yet finished with, and a bug this gear has marked
    /// `'Resolved'` is one whose JIRA issue reached
    /// [`StatusCategory::DONE`](crate::domain::ports::jira_client::StatusCategory::DONE)
    /// — the poller only ever calls [`Self::resolve_bug`] after
    /// [`StatusCategory::is_resolved`](crate::domain::ports::jira_client::StatusCategory::is_resolved)
    /// says so — but "done" on JIRA's own three-value reduction is not the
    /// same fact as the *ticket* having reached its own `Closed` state; an
    /// instance's workflow may hold a resolved issue open for verification
    /// before a human closes it. Re-filing against a ticket the tenant's own
    /// process has not finished with would be the wrong failure mode to
    /// introduce while porting a dedupe rule.
    ///
    /// **The user-visible consequence, stated because it is real and not
    /// merely theoretical**: once the poller resolves a bug, `GET
    /// /qa/v1/jira/open-bugs` and the runner's skip list (`status = 'Open'`)
    /// both stop showing it — the test is no longer suppressed and will run
    /// again. If it fails again before the ticket is actually closed in
    /// JIRA, `POST /qa/v1/jira/bugs` will answer `created: false` with the
    /// **same, already-resolved** `jira_key` rather than filing a fresh
    /// issue, because this probe still matches the `'Resolved'` row. That is
    /// legacy's own behaviour inherited unchanged, not a divergence
    /// introduced here — a divergence would have been switching this probe to
    /// `list_open`'s predicate, which this ruling declines to do.
    ///
    /// # `tenant_id` is a parameter, and it is not redundant with `scope` —
    /// # controller ruling R86, fix round 1, Critical 1
    ///
    /// The identical shape [`Self::get_config`]'s own doc states: a scope over
    /// `OWNER_TENANT_ID` may legitimately span several tenants
    /// (`ScopeFilter::In`, `ScopeFilter::InTenantSubtree` — a parent-tenant
    /// grant is a supported shape, not a hypothetical), and this method's
    /// `.one()` carries no `ORDER BY`. Without a `tenant_id` predicate a
    /// multi-tenant scope makes the probe return **whichever in-scope row the
    /// engine hands back first**, which for this method is worse than
    /// [`Self::get_config`]'s version of the same bug: a foreign tenant's
    /// `jira_key` would be handed back to the caller with `created: false`
    /// (a cross-tenant read of registry data), the caller's own failing test
    /// would never be filed at all, and — on the miss path, since
    /// [`crate::domain::service::jira::JiraService::file_bugs`] passes the
    /// same `tenant_id` to [`Self::upsert_bug`] — a bug filed for one tenant's
    /// run could land registered under a different, wider-scoped tenant id
    /// entirely, invisible to the first tenant's own
    /// [`Self::list_open_for_plan`].
    ///
    /// The tenant is validated against the scope first, so this cannot be used
    /// to probe a tenant the caller has no grant for.
    async fn find_unclosed_for_test<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        test_name: &str,
    ) -> Result<Option<JiraBug>, DomainError>;

    /// Mark a bug resolved: `status = 'Resolved'` and `resolved_at = at`.
    ///
    /// Legacy `resolve_bug` (`jira.rs:243-251`). **The resolution is recorded
    /// whether or not auto-rerun is on** — legacy calls this at
    /// `manager/src/services/jira_poller.rs:59`, *before* the
    /// `auto_rerun_on_resolve` gate at `:61-63`. Turning the switch off must not
    /// stop bugs closing.
    ///
    /// `at` is the caller's rather than the repository's `now()`, so the poller
    /// stamps every bug in one pass with one instant. `false` when no open bug
    /// with that key was visible to the caller.
    ///
    /// # `tenant_id` is a parameter, and this is the R86 instance that showed
    /// # the rule's wording was too narrow — Phase C's final review, Critical 1
    ///
    /// This is an `update_many`, not a `.one()`, which is exactly why five fix
    /// rounds of an R86 sweep phrased around `.one()` walked past it. Legacy's
    /// statement is `WHERE jira_key = $1` and nothing else (`jira.rs:245`) —
    /// correct for a single-tenant service and a cross-tenant write here.
    ///
    /// The premise is [`Self::find_by_key`]'s, verbatim: two tenants filing
    /// against the same or different JIRA instances can each produce
    /// `VHP-2618`, and `idx_qa_jira_bugs_tenant_key` is `(tenant_id, jira_key)`
    /// *because* that collision is expected. So without a `tenant_id`
    /// predicate, tenant P's poller pass — granted `qa.jira_bug/update` over
    /// `InTenantSubtree(P)` and told by P's JIRA that `VHP-1` is done — set
    /// `status = 'Resolved'` and `resolved_at` on child tenant C's `VHP-1` row
    /// too. C's bug is against a different instance and still open there, so:
    /// C's test leaves C's skip list and starts running again; C's own poller
    /// pass never re-evaluates it, because the row is no longer `'Open'` and
    /// [`Self::list_open`] is what that pass reads; and
    /// `POST /qa/v1/jira/bugs` answers `created: false` for C against a bug
    /// nobody resolved, because [`Self::find_unclosed_for_test`]'s
    /// `!= 'Closed'` probe still matches it.
    ///
    /// The tenant is validated against the scope first, so this cannot be used
    /// to resolve a bug belonging to a tenant the caller has no grant for.
    async fn resolve_bug<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        jira_key: &str,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// Look up a bug by its JIRA key within the tenant.
    ///
    /// # `tenant_id` is a parameter, and it is not redundant with `scope` —
    /// # controller ruling R89, fix round 2
    ///
    /// [`Self::find_unclosed_for_test`]'s reason, restated because this is the
    /// second time this exact defect was found in this file: a scope over
    /// `OWNER_TENANT_ID` may legitimately span several tenants
    /// (`ScopeFilter::In`, `ScopeFilter::InTenantSubtree`), and this method's
    /// `.one()` carries no `ORDER BY`. `jira_key` is not a secret and is not
    /// scoped to this gear's own tenants — two tenants filing against the
    /// same or different JIRA instances can each produce `VHP-2618` — and
    /// `idx_qa_jira_bugs_tenant_key` is `(tenant_id, jira_key)` precisely
    /// *because* that collision is expected, per [`Self::upsert_bug`]'s own
    /// doc ("one tenant filing `VHP-1` would permanently deny it to every
    /// other tenant"). Without a `tenant_id` predicate here, a multi-tenant
    /// scope can make this method return a **different** tenant's row for the
    /// identical key — and [`Self::upsert_bug`]'s own re-read is this
    /// method's most consequential caller: it is the value `upsert_bug`
    /// returns to a caller who just wrote (or thought they wrote) under their
    /// own `tenant_id`.
    ///
    /// The tenant is validated against the scope first, so this cannot be used
    /// to look up a tenant the caller has no grant for.
    async fn find_by_key<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        jira_key: &str,
    ) -> Result<Option<JiraBug>, DomainError>;

    // -- The two configuration singletons ------------------------------------

    /// The tenant's JIRA connection settings, or `None` when never saved.
    ///
    /// `None` is distinct from a row at its column defaults, exactly as
    /// [`NotifyRepository::get_config`](super::NotifyRepository::get_config)
    /// documents: a tenant that has never opened the settings page has no row,
    /// and legacy's equivalent is a missing `settings` key, which its
    /// `api_get_jira` answers with a hand-built all-empty document
    /// (`manager/src/routes/settings.rs:262-269`). Substituting that default
    /// belongs to the service, so that a caller can still tell "unconfigured"
    /// from "configured to blanks".
    ///
    /// Returns [`JiraConfig::api_token_credstore_ref`] and **never token
    /// material** — obligation #3 of the schema. Legacy masks the token to
    /// `"********"` on read (`settings.rs:254-259`); this column holds a
    /// credential-store reference, so there is nothing to mask.
    ///
    /// # `tenant_id` is a parameter, and it is not redundant with `scope`
    ///
    /// **Added in fix round 1, finding 4.** `.one()` over a compiled scope is
    /// unambiguous only when that scope pins exactly *one* tenant, and for this
    /// resource it need not: [`OWNER_TENANT_ID`](toolkit_security::pep_properties::OWNER_TENANT_ID)
    /// is the declared property and both `ScopeFilter::In` and
    /// `ScopeFilter::InTenantSubtree` compile against it
    /// (`libs/toolkit-security/src/access_scope.rs:169-198`), so a parent-tenant
    /// grant is a supported shape rather than a hypothetical. Under one, a
    /// `.one()` with no tenant predicate returns an **arbitrary** in-scope
    /// tenant's row — and `save_jira_config`'s read-then-write would then carry
    /// another tenant's credential reference onto this tenant's config.
    ///
    /// [`NotifyRepository::get_config`](super::NotifyRepository::get_config) had
    /// the same shape and no such parameter through Task 37 — deliberately not
    /// fixed here (controller ruling R83): it was Task 38's file. **Task 38
    /// closed it**, with the identical `tenant_id` parameter and equality
    /// predicate; that method's own doc carries the argument rather than
    /// repeating it here a second time.
    ///
    /// The tenant is validated against the scope first, so this cannot be used to
    /// read a tenant the caller has no grant for.
    async fn get_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<JiraConfig>, DomainError>;

    /// Create or replace the tenant's JIRA connection settings.
    ///
    /// One row per tenant, enforced by `idx_qa_jira_config_tenant`. Returns
    /// `()` rather than the stored config for
    /// [`NotifyRepository::save_config`](super::NotifyRepository::save_config)'s
    /// reason: [`JiraConfig`] has no `id` and no timestamps, so the repository
    /// mints nothing a caller could not already see.
    async fn save_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        config: JiraConfig,
    ) -> Result<(), DomainError>;

    /// The tenant's poller cadence and auto-rerun switch, or `None` when never
    /// saved.
    ///
    /// `None` again means "no row", and again the default is the service's to
    /// apply — legacy's reader is `get_or_default::<JiraPollerConfig>("jira_poller")`
    /// (`manager/src/routes/settings.rs:513-517`), whose default is 300 seconds
    /// and auto-rerun on (`manager/src/models.rs:1422-1429`).
    ///
    /// # The `i64` → `u64` widening, and where the clamp is not
    ///
    /// The column is `BIGINT` and
    /// [`JiraPollerConfig::poll_interval_seconds`] is `u64`
    /// (`infra::storage::entity::jira_poller_config`'s own doc names Task 32 as
    /// the owner of both). The widening is
    /// `infra::storage::mapper::jira_poller_config_to_sdk`'s and it **fails
    /// closed** on a negative value rather than wrapping — a wrapped negative
    /// would be an interval of some hundreds of millions of years, which is a
    /// poller that silently never polls.
    ///
    /// Legacy's `.max(1)` (`manager/src/services/jira_poller.rs:26`) is *not*
    /// applied here and not in the mapper: a repository that clamped would make
    /// the settings screen show a value the tenant did not save, and the
    /// zero-is-a-hot-loop hazard belongs to whoever sleeps. It lives on
    /// [`crate::domain::service::jira::JiraService::poller_config`].
    /// `tenant_id` is explicit for [`Self::get_config`]'s reason — the `.one()`
    /// needs a single-tenant predicate that a multi-tenant scope does not supply.
    async fn get_poller_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<JiraPollerConfig>, DomainError>;

    /// Create or replace the tenant's poller settings.
    ///
    /// One row per tenant, enforced by `idx_qa_jira_poller_config_tenant`.
    async fn save_poller_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        config: JiraPollerConfig,
    ) -> Result<(), DomainError>;
}
