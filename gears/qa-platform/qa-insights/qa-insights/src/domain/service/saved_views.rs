//! Saved-view CRUD — `GET/POST /qa/v1/analytics/views` and
//! `PUT/DELETE /qa/v1/analytics/views/{id}`.
//!
//! The port of legacy's four saved-view handlers
//! (`manager/src/routes/analytics.rs:521` `api_list_views`, `:576`
//! `api_create_view`, `:641` `api_update_view`, `:703` `api_delete_view`), and
//! **the first write path in Phase B**: every task before this one ported a
//! read pipeline, and this is the first place two callers can race and the
//! first resource in this gear whose access scope is narrower than the tenant.
//!
//! # Divergences from legacy, summarized
//!
//! Six, gathered here as an index; each is argued in full where the
//! cross-reference points, except the last two, which have no other home:
//!
//! 1. **Owner identity**: `X-Analytics-Owner` header (untrusted, arbitrary
//!    string) → `SecurityContext::subject_id` (authenticated `Uuid`). See "The
//!    owner substitution, in full", below.
//! 2. **Plan identity on the wire**: legacy's single opaque `plan_id: String`
//!    → the `(repo_id, plan_path)` pair. See "`(repo_id, plan_path)` on the
//!    wire, not legacy's single `plan_id`".
//! 3. **A plan submitted alongside `scope=all`**: legacy stores it inertly;
//!    this gear drops it, because `plan_key`'s scope-independence would
//!    otherwise make the row invisible to its own `scope=all` list. See "A
//!    plan submitted alongside `scope=all` is dropped, not stored".
//! 4. **`list`'s ordering**: added in the domain layer to *match* legacy's
//!    `ORDER BY updated_at DESC`, which the repository does not apply on its
//!    own — the one item on this list that closes a gap rather than opening
//!    one. See "`list`'s ordering, added rather than ported broken".
//! 5. **`create`'s success status**: legacy's `api_create_view` answers a bare
//!    `200 OK` with `Json<AnalyticsSavedView>` — no explicit status is set, and
//!    axum's `Json<T>` `IntoResponse` defaults to `200`. This gear answers
//!    `201 Created` with a `Location` header, matching every other create in
//!    this workspace's REST tier (`created_json`, used identically by
//!    `qa-runs`' `create_schedule`). Not argued elsewhere; stated here in
//!    full because nothing about it is forced by this gear's architecture —
//!    it is a REST-convention improvement this task chose to make, and it
//!    should not be read back into parity.
//! 6. **Database errors on create/update**: legacy's `api_create_view` and
//!    `api_update_view` both map *every* database error from their
//!    `INSERT`/`UPDATE` — a unique-constraint violation included — to a
//!    blanket `400 Bad Request` carrying the driver's own text
//!    (`manager/src/routes/analytics.rs:618-623` for create,
//!    `:675-680` for update; `format!("Failed to {create,update} saved view:
//!    {}", e)`). This gear never lets driver text reach a caller
//!    (`domain::error`'s own rule for `Database`/`Internal`/`CorruptState`)
//!    and maps a unique violation specifically to `409 SavedViewNameExists`
//!    — narrower and more honest than legacy's catch-all, and, like item 5,
//!    a pre-existing Task 12 decision this task is the first to expose at the
//!    REST boundary rather than a choice made here. Not argued elsewhere;
//!    stated here in full for the same reason as item 5.
//!
//! # The owner substitution, in full
//!
//! Legacy's `analytics_owner_id` (`:2129-2140`) reads the caller's identity
//! from a **request header**:
//!
//! ```text
//! fn analytics_owner_id(headers: &HeaderMap) -> Result<String, (StatusCode, String)> {
//!     headers
//!         .get("X-Analytics-Owner")
//!         .and_then(|value| value.to_str().ok())
//!         .map(str::trim)
//!         .filter(|value| !value.is_empty())
//!         .map(|value| value.to_string())
//!         .ok_or((StatusCode::BAD_REQUEST, "X-Analytics-Owner header is required".to_string()))
//! }
//! ```
//!
//! Three properties matter and none of them survives unchanged:
//!
//! 1. **Absent or blank is a 400**, not an anonymous or default owner — legacy
//!    has no notion of "no owner". This gear has no equivalent rejection,
//!    because [`SecurityContext::subject_id`] cannot be absent or blank: it is
//!    a `Uuid`, set by the same authentication layer that admits the request at
//!    all, and unauthenticated traffic never reaches a handler with one. The
//!    closest analogue is the platform-root sentinel (a nil `subject_tenant_id`),
//!    which is refused by [`PolicyEnforcer::access_scope`]'s
//!    `ConstraintsRequiredButAbsent` path before this service is asked
//!    anything — a 403, not legacy's 400, and for a different reason (no tenant
//!    to scope by, not no owner to scope by).
//! 2. **Trust.** Legacy's owner is *caller-supplied, unauthenticated, free
//!    text* — any client can set `X-Analytics-Owner: someone-elses-name` and
//!    both read and write as that identity, because nothing ties the header to
//!    a session. `SecurityContext::subject_id` is minted by this platform's own
//!    authentication and cannot be forged by a request header. This is a
//!    **strict security improvement** the substitution buys for free, not a
//!    liberty this task takes: legacy's own design has no session binding on
//!    this identity at all.
//! 3. **Format.** Legacy's owner is an arbitrary trimmed non-empty `String` —
//!    there is no format legacy enforces beyond non-blank, so a caller could in
//!    principle own views under `"bob"`, `"team-qa"` or an email address, all in
//!    the same `owner_id TEXT` column. `SecurityContext::subject_id` is always a
//!    `Uuid`. A deployment migrating off legacy's header scheme therefore loses
//!    nothing this gear could accept anyway — `owner_id UUID NOT NULL` is what
//!    `infra::storage::entity::saved_view` declares — but it is a real format
//!    narrowing worth naming: legacy's column could hold a value this gear's
//!    column cannot represent, and there is no migration path for such a row
//!    other than discarding or re-keying it. Nothing in this crate needs one
//!    (there is no legacy database to migrate from), so it is recorded as a
//!    finding rather than solved.
//!
//! # The substitution preserves the uniqueness semantics
//!
//! Legacy's unique index is `(owner_id, scope, COALESCE(plan_id, ''), name)`
//! (`manager/migrations/001_initial.sql:194-195`) — **per-owner**, per-scope,
//! per-plan names. The gear's index,
//! `idx_qa_analytics_saved_views_unique` on `(tenant_id, owner_id, scope,
//! plan_key, name)` (`infra::storage::migrations`, see
//! [`crate::domain::repos::SavedViewsRepository`]'s header for the
//! `plan_key`/`COALESCE` correspondence), **adds** `tenant_id` and otherwise
//! keeps every column legacy's index has, with the same per-owner grain: two
//! different `owner_id`s never collide, matching legacy exactly. The added
//! `tenant_id` column is not a narrowing of legacy's semantics — legacy has no
//! multi-tenant model at all, so every legacy row is implicitly one tenant's —
//! it is this gear's tenancy made explicit in the key that already existed.
//! **Confirmed, not assumed**: `two_global_views_may_not_share_a_name` and
//! `a_global_and_a_plan_scoped_view_may_share_a_name` in this module's test
//! file exercise the index through this service exactly as legacy's own two
//! rules would predict, and `views_are_scoped_to_their_owner` exercises the
//! per-owner grain the substitution has to preserve.
//!
//! # Every operation additionally applies [`AccessScope::ensure_owner`]
//!
//! [`resources::SAVED_VIEW`] declares [`pep_properties::OWNER_ID`] as a
//! supported property, which lets a PDP answer with an owner-narrowed scope —
//! but nothing *requires* it to. A policy that has not been taught this new
//! resource type, or one that intentionally grants a broader read (an
//! "administrator can list every saved view" policy is a legitimate thing to
//! want), would otherwise let [`SavedViewsService::list`] return every owner's
//! views under one grant, which is not what any of legacy's four handlers do:
//! legacy's `owner_id = $1` predicate is unconditional, not a policy knob.
//!
//! [`SavedViewsService::scope`] closes that gap with the same construction
//! `mini-chat`'s `ReactionService` uses for its own per-subject resource —
//! `.access_scope(..).await?.ensure_owner(ctx.subject_id())`, identically, at
//! `gears/mini-chat/mini-chat/src/domain/service/reaction_service.rs:68` and
//! `:136`. **Corrected count (Phase B fix wave, Finding 8; a prior revision
//! of this paragraph said "six more of that gear's services do the same",
//! which double-counted)**: five more of that gear's domain services call
//! `ensure_owner` the same way — `attachment_service.rs`, `chat_service.rs`,
//! `message_service.rs`, `stream_service/mod.rs` and `turn_service.rs` — plus
//! one call site that is **not** a service: the REST handler
//! `api/rest/handlers/quota.rs`, which calls `ensure_owner` directly rather
//! than through a domain service. Verified by `grep -rln ensure_owner` over
//! `gears/mini-chat/mini-chat/src` and reading each hit, not by trusting the
//! prior count. **`usage-collector`
//! is not that precedent** — an earlier revision of this paragraph cited it,
//! and it is wrong: `usage-collector/src/domain/authz.rs` declares
//! `OWNER_ID` as a resource property (`:249`, `:408`) and maps it to a column
//! at evaluation time (`:647`, `:984`), but it never calls `ensure_owner` and
//! applies no unconditional narrowing (`grep -rln ensure_owner` over this
//! workspace does not list it). It remains the right citation for *declaring*
//! `OWNER_ID` on a resource type — `mini-chat` is the one for the narrowing
//! this module actually performs.
//!
//! `.ensure_owner(ctx.subject_id())` chains onto the compiled scope before it
//! reaches a repository, on every call site in this service.
//! `AccessScope::ensure_owner`'s own doc
//! (`libs/toolkit-security/src/access_scope.rs:871-925`) states the
//! intersection semantics in full; the property that matters here is that an
//! **unconstrained** scope becomes a single `owner_id = subject` constraint —
//! not merely "narrower", but no longer unconstrained at all. That distinction
//! is what closes the write path specifically:
//! `toolkit_db::secure::db_ops::validate_insert_scope`'s first line is
//! `if scope.is_unconstrained() || A::Entity::IS_UNRESTRICTED { return Ok(()); }`
//! (`libs/toolkit-db/src/secure/db_ops.rs:68`) — a fail-open early return for
//! exactly the scope shape a not-yet-taught or misconfigured policy would
//! compile for a new resource type. After `ensure_owner`, that shape is one
//! `create` can no longer produce: the scope it hands to `secure_insert`
//! always carries a real `owner_id` filter, so `validate_insert_scope` always
//! falls through to its actual per-column check rather than short-circuiting
//! past it. A scope that already names a *different* owner becomes deny-all
//! rather than silently widening, for the same reason. This is what makes
//! legacy's unconditional `owner_id = $1` the **floor** this gear's
//! authorization can compile to, never something a permissive or
//! not-yet-configured policy can erode.
//!
//! # The collision: caught at the database, not pre-checked
//!
//! [`crate::domain::repos::SavedViewsRepository::find_by_natural_key`] exists
//! and this service does not call it from [`Self::create`] or [`Self::update`].
//! That is a deliberate reading of the brief's "decide deliberately" instruction
//! and not an oversight, for one reason: **the repository's `create` and
//! `update` already catch the database's own unique-constraint violation and
//! map it to [`DomainError::SavedViewNameExists`]**
//! (`infra::storage::saved_views_sea_repo`, both write methods,
//! `Err(e) if e.is_unique_violation() => Err(DomainError::SavedViewNameExists { name })`).
//! A `find_by_natural_key` probe run first, by contrast, opens exactly the
//! **TOCTOU window** the brief warns about — a second writer could insert
//! between the probe and this service's own insert, and the probe would have
//! reported "free" for a name that is no longer free by the time the write
//! lands. Pre-checking would not make the operation safer; it would spend a
//! second query to compute an answer that can go stale before the query that
//! matters even starts, and give a false sense that the race had been handled.
//! `qa-runs`' `ScheduleService::create` sets the precedent this follows: it
//! relies on `SchedulesRepository::create`'s own unique-violation mapping and
//! performs no natural-key probe of its own.
//!
//! **The atomicity claim holds for `create` and not, in full, for `update` —
//! stated precisely so the difference is not lost:**
//!
//! * **`create` is atomic with the write.** There is no window between the
//!   unique-violation catch and the `INSERT` it is watching, because the
//!   catch *is* that statement's own outcome. Two callers racing to create the
//!   same `(owner, scope, plan, name)` tuple both reach the database; exactly
//!   one `INSERT` wins; the loser's statement fails the unique constraint and
//!   is mapped to `SavedViewNameExists` — a `409`, never a `500`, never a
//!   silent duplicate.
//! * **`update` is a two-step scoped read-then-write, and a delete landing
//!   between the two steps is not a 404.** `saved_views_sea_repo::update`'s
//!   own existence read (`infra::storage::saved_views_sea_repo.rs:148-155`)
//!   is the *first* scoped lookup; `secure_update_with_scope` performs a
//!   *second* one before it writes
//!   (`libs/toolkit-db/src/secure/db_ops.rs:226-237`, `E::find().secure()
//!   .scope_with(scope).and_id(id)?.one(runner).await?`). If a concurrent
//!   delete lands in the gap between those two reads, the first read still
//!   sees the row, this repository proceeds into `secure_update_with_scope`,
//!   and its second read misses — which is `ScopeError::Denied("entity not
//!   found or not accessible in current security scope")`, not `Ok(None)`.
//!   That variant has no case in `update`'s `match` for "not found"; it falls
//!   into `Err(e) => Err(db_err(e))`, and `db_err` is a blanket
//!   `DomainError::Database(e.to_string())` (`infra::storage::db::db_err`) —
//!   so this service's `.ok_or(DomainError::SavedViewNotFound { id })` never
//!   runs, and the caller sees an opaque `500` where the single-statement
//!   `create` path (and a hypothetical single-statement `update`) would have
//!   answered a clean `404`. **This is real and is not fixed here**:
//!   remapping `ScopeError::Denied` inside `secure_update_with_scope` would
//!   touch Task 12's shared repository helper, and `Denied` legitimately means
//!   "denied" for callers other than this one — that decision belongs to a
//!   review of the shared helper, not to a fix inside this service. If a
//!   two-connection concurrency test is ever written for this resource, this
//!   is the case it should assert: a delete racing an update's second read,
//!   observed as `DomainError::Database` rather than `SavedViewNotFound`.
//!
//! **This cannot be exercised on the unit tier** either way:
//! `infra::storage::test_db::inmem_db` is in-memory `SQLite` behind this
//! crate's connection pool, which serializes writers, so two "concurrent"
//! calls in a unit test are actually sequential and neither race window can be
//! observed to be lost. A test that wanted to *falsify* either half of this
//! analysis would have to run on the Postgres integration tier
//! (`--features integration --lib`) with two real concurrent connections; none
//! is added here; see this task's report for why.
//!
//! [`find_by_natural_key`] remains exactly what Task 12 built it for — a
//! read-your-own-scope existence probe available to a *future* caller that
//! needs one (an upsert-by-name endpoint, say, which legacy does not have and
//! this task does not add) — and is simply not the tool Task 28's strict
//! create/update pair needs.
//!
//! [`find_by_natural_key`]: crate::domain::repos::SavedViewsRepository::find_by_natural_key
//!
//! # `(repo_id, plan_path)` on the wire, not legacy's single `plan_id`
//!
//! `qa_insights_sdk`'s module header (note 1) and
//! [`crate::domain::repos::saved_views_repo`]'s header both state why a plan is
//! `(repo_id, plan_path)` in this contract rather than the single opaque
//! `plan_id: String` legacy stores. [`SavedViewInput`] therefore carries the
//! pair as two optional fields rather than one, and [`required_plan`] enforces
//! legacy's rule ("required when `scope=plan`") plus one rule legacy's single
//! field could never need: a half-present pair (one field `Some`, the other
//! `None`) is always a `400`, never silently read as absent.
//!
//! # A plan submitted alongside `scope=all` is dropped, not stored — and this
//! # is a correction, not a straight port
//!
//! Legacy's `normalize_optional(payload.plan_id.as_deref())` runs
//! unconditionally and only the `scope == Scope::Plan` arm inspects the
//! result, so legacy genuinely **stores** a `plan_id` a caller sends alongside
//! `scope=all` without complaint. Read that literally, this service would do
//! the same — and it would be wrong, for a reason that only exists because of
//! how this schema's uniqueness is implemented rather than anything about the
//! wire contract.
//!
//! [`crate::infra::storage::mapper::plan_key`] (called `key_for` in the
//! repository) derives the fourth column of
//! `idx_qa_analytics_saved_views_unique` from **`repo_id`/`plan_path` alone,
//! never from `scope`** — its own doc states this explicitly: "reads
//! `repo_id`/`plan_path` and not `scope`: the two must agree". So a view
//! stored with `scope="all"` but a non-`None` `repo_id`/`plan_path` gets a
//! **non-empty** `plan_key`. `SavedViewsRepository::list`'s `scope=all` branch
//! filters on `plan_key = ""` (`key_for_plan(None)`), so that row would not
//! appear in its own `scope=all` list — a view a caller could create and then
//! never see again through the endpoint that is supposed to list it. Legacy
//! has no such trap: its `scope=all` list issues no `plan_id` predicate at all
//! (`api_list_views`'s `else` branch, `manager/src/routes/analytics.rs:556-563`),
//! so a stray `plan_id` on a legacy row is merely inert, never a data-loss bug.
//!
//! This gear's `plan_key` materialization (forced by `SQLite`/`MySQL` lacking
//! functional indexes; see that trait's header) removes the option of porting
//! legacy's literal permissiveness safely. [`required_plan`] therefore clears
//! any submitted plan identity when `scope` is `all`, so `plan_key` and
//! `scope` can never disagree about what a row is. **Found by this task's Step
//! 0, not by the plan** — the plan's citation of the pair
//! (`qa_insights_sdk`'s note 1) does not mention `plan_key` at all, and the
//! interaction only surfaces once the two are read together.
//!
//! # `plan_id is required when scope=plan` moved off `plan_id`
//!
//! Legacy's 400 for scope `plan` with no plan present names the field
//! `plan_id`. With the pair split into `repo_id`/`plan_path`, there is no
//! single field left to name that a caller unambiguously recognises as "the
//! plan" — [`required_plan`] attributes it to `plan_path`, the half a caller
//! is more likely to have typed by hand, and states both halves in the message
//! text.
//!
//! # `list`'s ordering, added rather than ported broken
//!
//! Legacy's `api_list_views` issues `ORDER BY updated_at DESC`
//! (`manager/src/routes/analytics.rs:542`, `:557`). Task 12's
//! `SavedViewsRepository::list` issues no `ORDER BY` at all — verified by
//! reading `infra::storage::saved_views_sea_repo::list`'s body, not assumed —
//! so a bare `repo.list(..)` call here would hand back rows in whatever order
//! the storage engine happens to produce, which is a real divergence from
//! legacy and not merely an unspecified tie-break. [`Self::list`] closes it in
//! the domain rather than in the repository: it sorts the fetched `Vec` by
//! `updated_at` descending before returning, which reproduces legacy's
//! observable order without touching a file outside this task's list. Flagged
//! here because the repository's own header does not mention the gap, and a
//! later reader diffing this service against that trait should not conclude
//! the ordering was inherited for free.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_insights_sdk::{NewSavedView, SavedView, SavedViewScope};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::analytics::PlanRef;
use crate::domain::error::DomainError;
use crate::domain::repos::SavedViewsRepository;
use crate::domain::service::{DbProvider, actions, resources};

/// The unvalidated body of a create or a replace.
///
/// Legacy's `SavedViewUpsertRequest` (`manager/src/routes/analytics.rs:69-74`),
/// with `plan_id: Option<String>` split into [`Self::repo_id`] +
/// [`Self::plan_path`] for the reason this module's header gives. A domain type
/// rather than the REST tier's request DTO, matching
/// `domain::analytics::query::OverviewQuery`'s split: the rules below are
/// testable without a router, and the DTO that deserializes the wire body maps
/// onto this field for field.
#[derive(Clone, Debug)]
pub struct SavedViewInput {
    /// `"all"` or `"plan"`, case-insensitively — [`parse_scope`]. Untrusted.
    pub scope: String,
    /// Half of the plan identity; required together with
    /// [`Self::plan_path`] exactly when [`Self::scope`] is `"plan"` — see
    /// [`required_plan`].
    pub repo_id: Option<Uuid>,
    /// The other half.
    pub plan_path: Option<String>,
    /// Untrimmed; [`validate_name`] trims and rejects blank.
    pub name: String,
    /// Verbatim JSON text — opaque, exactly as
    /// [`qa_insights_sdk::NewSavedView::query_json`] documents.
    pub query_json: String,
}

/// `"all"` or `"plan"`, trimmed and case-insensitive — legacy's `parse_scope`
/// (`manager/src/routes/analytics.rs:2104-2113`), shared by **four** call sites
/// in legacy's `analytics.rs` — three of its saved-view handlers
/// (`api_list_views` `:527`, `api_create_view` `:582`, `api_update_view` `:648`;
/// `api_delete_view` takes no scope) plus `normalize_overview_query` (`:2408`),
/// which is a helper and not a handler at all. **A prior revision said "all six
/// of legacy's analytics handlers"** (Phase C's final review, cluster A);
/// counted with `grep -n parse_scope` over that file, it is four sites and three
/// handlers. Ported a second time here because this
/// gear's saved-view scope is [`SavedViewScope`], a different type from
/// `domain::analytics::query::Scope`'s overview-only enum — coupling this
/// service to that module for one shared literal would be the wrong
/// dependency.
///
/// `to_ascii_lowercase`, not `to_lowercase`, for `domain::analytics::query::parse_scope`'s
/// own reason: legacy's fold is the ASCII one.
///
/// # Errors
///
/// [`DomainError::Validation`] on `scope`, carrying legacy's message verbatim.
fn parse_scope(value: &str) -> Result<SavedViewScope, DomainError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "all" => Ok(SavedViewScope::All),
        "plan" => Ok(SavedViewScope::Plan),
        _ => Err(DomainError::Validation {
            field: "scope".to_owned(),
            message: "scope must be 'all' or 'plan'".to_owned(),
        }),
    }
}

/// Trim `name`, reject blank.
///
/// Legacy: `let name = payload.name.trim(); if name.is_empty() { 400 }`,
/// identically at `api_create_view` (`manager/src/routes/analytics.rs:584-586`)
/// and `api_update_view` (`:649-651`).
///
/// # Errors
///
/// [`DomainError::Validation`] on `name`, carrying legacy's message verbatim
/// (`"name is required"`).
fn validate_name(name: &str) -> Result<String, DomainError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(DomainError::Validation {
            field: "name".to_owned(),
            message: "name is required".to_owned(),
        });
    }
    Ok(trimmed.to_owned())
}

/// Pair `repo_id` and `plan_path` into a [`PlanRef`], enforce legacy's
/// "required when `scope=plan`" rule on the pair, and — the one place this
/// service does not port legacy literally — drop the pair entirely when
/// `scope` is `All`. See this module's header for why the drop is a
/// correction rather than a liberty.
///
/// `plan_path` is trimmed and blank-as-absent first, matching legacy's
/// `normalize_optional` (`:2070-2075`) applied to `plan_id`. Then:
///
/// * **`scope` is `All`** → always `Ok(None)`, whatever `repo_id`/`plan_path`
///   were. A malformed half-present pair is *not* reported as an error in this
///   arm: it is about to be discarded either way, and rejecting a client for a
///   value this service never stores would be a rejection with no
///   corresponding stored-state hazard behind it.
/// * **`scope` is `Plan`, both present** → `Some(PlanRef { .. })`.
/// * **`scope` is `Plan`, both absent** → a `400` naming `plan_path` — legacy's
///   own rule, at three call sites that share one message, `"plan_id is
///   required when scope=plan"`: `api_list_views` (`analytics.rs:530-540`),
///   `api_create_view` (`:588-594`) and `api_update_view` (`:654-660`).
/// * **`scope` is `Plan`, exactly one present** → always a `400`. Legacy's
///   single `plan_id` field cannot produce this shape; it is forced by
///   splitting the pair, and rejecting it is safer than the alternative of
///   silently treating the half-present pair as absent (which
///   [`plan_key`](crate::infra::storage::mapper::plan_key) would do,
///   coalescing it into a bucket that disagrees with the caller's declared
///   `scope=plan`).
///
/// # Errors
///
/// [`DomainError::Validation`] on `plan_path`.
fn required_plan(
    scope: SavedViewScope,
    repo_id: Option<Uuid>,
    plan_path: Option<&str>,
) -> Result<Option<PlanRef>, DomainError> {
    if scope == SavedViewScope::All {
        // Dropped, not merely unchecked: storing it would make `plan_key`
        // (derived from `repo_id`/`plan_path` alone, never from `scope`)
        // disagree with a `scope=all` row's own list predicate. See this
        // module's header.
        return Ok(None);
    }

    let plan_path = plan_path
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    match (repo_id, plan_path) {
        (Some(repo_id), Some(plan_path)) => Ok(Some(PlanRef { repo_id, plan_path })),
        _ => Err(DomainError::Validation {
            field: "plan_path".to_owned(),
            message: "repo_id and plan_path are required when scope=plan".to_owned(),
        }),
    }
}

/// Build the validated [`NewSavedView`] a create or an update writes, applying
/// legacy's rule order: scope, then name, then plan.
///
/// # Errors
///
/// Whatever [`parse_scope`], [`validate_name`] or [`required_plan`] raise,
/// checked in that order — legacy's own (`analytics.rs:582-594` for create,
/// `:648-660` for update).
fn validated(input: SavedViewInput) -> Result<NewSavedView, DomainError> {
    let scope = parse_scope(&input.scope)?;
    let name = validate_name(&input.name)?;
    let plan = required_plan(scope, input.repo_id, input.plan_path.as_deref())?;
    Ok(NewSavedView {
        scope,
        repo_id: plan.as_ref().map(|p| p.repo_id),
        plan_path: plan.map(|p| p.plan_path),
        name,
        query_json: input.query_json,
    })
}

/// Stored analytics filter sets, owned per-caller — `qa_analytics_saved_views`.
///
/// See this module's header for the owner substitution, the uniqueness
/// argument and the collision/concurrency decision. Generic over the
/// repository for [`crate::domain::service::dashboard::DashboardService`]'s
/// reason: [`SavedViewsRepository`]'s methods are generic over the `DBRunner`
/// they run on, so the trait is not object-safe and the parameter propagates
/// to [`crate::domain::service::AppServices`].
pub struct SavedViewsService<S> {
    db: Arc<DbProvider>,
    views: S,
    policy_enforcer: PolicyEnforcer,
}

impl<S> SavedViewsService<S>
where
    S: SavedViewsRepository,
{
    #[must_use]
    pub const fn new(db: Arc<DbProvider>, views: S, policy_enforcer: PolicyEnforcer) -> Self {
        Self {
            db,
            views,
            policy_enforcer,
        }
    }

    /// Compile the caller's scope for one saved-view operation, then apply
    /// [`AccessScope::ensure_owner`].
    ///
    /// **Every single call site in this service goes through here** — see this
    /// module's header for why the `ensure_owner` narrowing is not optional
    /// defence in depth but the mechanism that keeps this gear's authorization
    /// at least as narrow as legacy's unconditional `owner_id = $1`.
    async fn scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::SAVED_VIEW, action, resource_id)
            .await?;
        Ok(scope.ensure_owner(ctx.subject_id()))
    }

    /// The caller's views at one scope — `GET /qa/v1/analytics/views`.
    ///
    /// `api_list_views` (`manager/src/routes/analytics.rs:521-574`). Ordered
    /// newest-`updated_at`-first; see this module's header for why that sort
    /// happens here rather than in the repository.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] on `scope` or `plan_path`, in that order.
    /// [`DomainError::Forbidden`] when the PDP denies or compiles a scope it
    /// cannot express. [`DomainError::Database`] on a query failure.
    pub async fn list(
        &self,
        ctx: &SecurityContext,
        scope: &str,
        repo_id: Option<Uuid>,
        plan_path: Option<&str>,
    ) -> Result<Vec<SavedView>, DomainError> {
        let scope = parse_scope(scope)?;
        let plan = required_plan(scope, repo_id, plan_path)?;
        let access = self.scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        let mut views = self
            .views
            .list(&conn, &access, scope, plan.as_ref())
            .await?;
        views.sort_by_key(|v| std::cmp::Reverse(v.updated_at));
        Ok(views)
    }

    /// Store a new view owned by the caller — `POST /qa/v1/analytics/views`.
    ///
    /// `api_create_view` (`manager/src/routes/analytics.rs:576-639`).
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] on `scope`, `name` or `plan_path`, in that
    /// order — legacy's own rule order.
    /// [`DomainError::SavedViewNameExists`] on a collision; see this module's
    /// header for why it is caught at the database rather than pre-checked.
    /// [`DomainError::Forbidden`] when the PDP denies or compiles a scope it
    /// cannot express. [`DomainError::Database`] on a query failure.
    pub async fn create(
        &self,
        ctx: &SecurityContext,
        input: SavedViewInput,
    ) -> Result<SavedView, DomainError> {
        let new = validated(input)?;
        let access = self.scope(ctx, actions::CREATE, None).await?;
        let conn = self.db.conn()?;
        self.views
            .create(
                &conn,
                &access,
                ctx.subject_tenant_id(),
                ctx.subject_id(),
                new,
            )
            .await
    }

    /// Replace an existing view's scope, plan, name and query —
    /// `PUT /qa/v1/analytics/views/{id}`.
    ///
    /// `api_update_view` (`manager/src/routes/analytics.rs:641-701`). A full
    /// replace, like legacy's: every caller-decidable column is taken from
    /// `input`, and there is no patch semantics on this resource in legacy
    /// either.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] on `scope`, `name` or `plan_path`.
    /// [`DomainError::SavedViewNameExists`] on a rename that collides.
    /// [`DomainError::SavedViewNotFound`] when no view in the caller's owner
    /// scope matches `id` — absent and another owner's are the same 404,
    /// legacy's own `rows_affected() == 0` answer (`:683`).
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure.
    pub async fn update(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        input: SavedViewInput,
    ) -> Result<SavedView, DomainError> {
        let new = validated(input)?;
        let access = self.scope(ctx, actions::UPDATE, Some(id)).await?;
        let conn = self.db.conn()?;
        self.views
            .update(&conn, &access, id, new)
            .await?
            .ok_or(DomainError::SavedViewNotFound { id })
    }

    /// Delete a view — `DELETE /qa/v1/analytics/views/{id}`.
    ///
    /// `api_delete_view` (`manager/src/routes/analytics.rs:703-726`). Not
    /// idempotent, matching legacy: a second delete of the same id is a 404.
    ///
    /// # Errors
    ///
    /// [`DomainError::SavedViewNotFound`] when no view in the caller's owner
    /// scope matched `id`.
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::Database`] on a query failure.
    pub async fn delete(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        let access = self.scope(ctx, actions::DELETE, Some(id)).await?;
        let conn = self.db.conn()?;
        if self.views.delete(&conn, &access, id).await? {
            Ok(())
        } else {
            Err(DomainError::SavedViewNotFound { id })
        }
    }
}

#[cfg(test)]
#[path = "saved_views_tests.rs"]
mod tests;
