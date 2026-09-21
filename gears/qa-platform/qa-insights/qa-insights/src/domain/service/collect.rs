//! The collect trigger, the runner's report, and the (callable, not yet
//! ticked) hourly cycle — `POST /qa/v1/analytics/collect` and
//! `POST /qa/v1/collect/{repo_id}`.
//!
//! The port of legacy's `services/collect.rs` in full —
//! `launch_collect_for_repo` is `manager/src/services/collect.rs:26-152`,
//! [`CollectService::run_collect_cycle`] below is `:157-179`, and the poller loop
//! (`start_collect_poller:183-188`) is explicitly **not** ported here: the
//! brief's own scope line says so, and [`CollectService::run_collect_cycle`]
//! being callable is what Task 40 wired a leader-elected ticker onto
//! (`crate::gear`'s `collect_pass`, role `qa-insights-collect`).
//! Also `routes/analytics.rs:2614` `api_collect_report`, `:2655`
//! `api_collect_trigger`, and `:2672` `load_collect_counts` — the last of
//! which Task 29 already ported (`analytics::AnalyticsService::collect_counts`);
//! this module is the write half of the same table.
//!
//! # This is the second write path in Phase B, and its authorization answer is
//! # not [`saved_views`](super::saved_views)'s answer copied over
//!
//! [`saved_views`](super::saved_views) established the pattern for a
//! request-scoped write: compile a PDP scope, narrow it, write. **Exactly one
//! of this module's two public writes follows that pattern.**
//! [`CollectService::trigger`] is a caller sitting in a browser session
//! clicking "Collect cases" — a real end user, with a real `SecurityContext`
//! a PDP can evaluate — so it compiles a scope under
//! [`actions::COLLECT`](super::actions::COLLECT) and refuses one that is not
//! tenant-only, for [`super::refuse_scope_beyond_tenant`]'s reason: a collect
//! trigger fans out over every repository in the caller's universe, and there
//! is no per-repository predicate a narrower scope could express.
//!
//! [`CollectService::record_count`] is not that. Its caller is
//! `POST /qa/v1/collect/{repo_id}`, which is meant to be reached without a
//! browser session at all — it is the **runner**, posting back to a URL this
//! gear itself built and handed to qa-runs when
//! [`CollectService::run_collect_cycle`] launched the workflow. There is no
//! session to check against a PDP: a `SecurityContext` minted for a request
//! with no subject would be a fiction, and evaluating a policy against a
//! fictional subject is worse than not evaluating one, because it *looks*
//! like an authorization decision was made.
//!
//! # `.public()` genuinely exempts this route — traced end to end against how
//! # this gear is actually hosted, not against the middleware in isolation
//!
//! `api::rest::routes::collect` registers this route with
//! `OperationBuilder::public()`. **Fix round 1's header here concluded the
//! opposite of what is true, from premises that were individually correct:**
//! `OperationSpec.is_public` is never read by `OperationBuilder::register`;
//! nothing in this workspace inserts the `toolkit_http_middleware::PublicRoute`
//! extension outside that middleware's own tests; and an inner, route-scoped
//! layer genuinely cannot beat an outer one in tower's layering model. All
//! three are true. **The error was concluding from them that some bearer-JWT
//! middleware rejects this route** — checked, this round, against the host
//! that actually serves `qa_insights`, not against `security_context_middleware`
//! considered on its own.
//!
//! `qa_insights` has no binary of its own — only `src/lib.rs` — and is
//! compiled into an in-process host,
//! `apps/cf-gears-example-server/src/registered_gears.rs:70`
//! (`use qa_insights as _;`). That host is fronted by `api-gateway`, which
//! has **its own**, separate, fully-wired auth middleware:
//!
//! 1. `gear::register_route_policy` walks every gear's `OperationSpec` and
//!    inserts every `spec.is_public` route into `public_routes`
//!    (`gears/system/api-gateway/src/gear.rs:250-257`).
//! 2. `build_route_policy` turns `public_routes` into `PublicRouteMatcher`s
//!    inside `GatewayRoutePolicy` (`middleware/auth.rs:148-158`).
//! 3. `authn_middleware` resolves each request's `(method, path)` against
//!    that policy; a match is `AuthRequirement::None`, and that arm **inserts
//!    `SecurityContext::anonymous()` and runs the handler** — no bearer token
//!    checked, none required (`middleware/auth.rs:195-199`, read in full,
//!    not inferred from the enum variant's name).
//!
//! So on the host that actually serves this gear, `.public()` **does** what
//! its name says: an anonymous `POST /qa/v1/collect/{repo_id}` reaches
//! [`CollectService::record_count`] with no session at all, exactly as this
//! route was designed for.
//!
//! **The `security_context_middleware`/`PublicRoute` analysis was not wrong
//! on its own terms — it was answering a question about a deployment shape
//! this gear does not use.** For completeness, traced the same way: were
//! `qa_insights` ever hosted `OoP` instead, the tenant-plane middleware that
//! analysis centred on is installed only `if let Some(bearer) =
//! options.bearer_authenticator` (`libs/toolkit/src/runtime/oop_serve.rs:491-495`),
//! and the one production constructor of those options hardcodes
//! `bearer_authenticator: None`
//! (`libs/toolkit/src/bootstrap/oop.rs:669`; its own doc at `:625` says the
//! injection point "is left unset here" for the app/gear binary to supply —
//! which this crate, having none, cannot). So the middleware whose absence
//! of a `PublicRoute` inserter mattered in the original analysis is never
//! installed **at all** on that path either, for a different reason: not
//! "installed but silently bypassing `.public()` routes", but "never
//! constructed in the first place, for every route." Both paths this
//! workspace supports reach the same place — anonymous reachability — by
//! different mechanisms, and neither one is the `401` the original analysis
//! predicted.
//!
//! **Consequence for [`CollectService::record_count`]'s HMAC check, below:
//! it is not a defence behind a platform fix that has yet to land. It is the
//! actual, live access control on an endpoint that is anonymously reachable
//! today**, on the host this gear is actually deployed on. Fix round 1's
//! framing — "the platform gap is a reachability problem, and the signature
//! is what stands between the day it is fixed and a cross-tenant write" — is
//! withdrawn along with the conclusion it depended on: there is no such day
//! to wait for, because there is no gap on the path that serves this gear.
//!
//! `record_count` therefore takes the same shape
//! [`domain::service::ingest`](crate::domain::service::ingest) does for its
//! own background callers: `AccessScope::for_tenant(tenant.get())`, no PDP
//! round-trip. **Since the Phase B fix wave's Finding 5, it also takes the
//! same *parameter* shape `IngestService::read_run_projection` does** —
//! `tenant: TenantBound` in, with
//! [`crate::domain::system_actor::for_collect_report`] called *inside* the
//! function rather than handed in already built by the caller. See
//! [`CollectService::record_count`]'s own doc, "The system actor is minted
//! inside this function", for why: minting it in the handler, before
//! signature verification, audit-logged "system actor constructed" for
//! requests this endpoint went on to refuse. The tenant this context is
//! bound to still comes from the callback URL's own `tenant_id` query
//! parameter, unchanged.
//!
//! # Fix round 1, Critical 1: that parameter was trusted on its own, and that
//! # was a cross-tenant write
//!
//! **This section replaces an earlier version of this header that argued
//! `tenant_id` was "safe to trust" merely because this gear chose the value
//! when it built the URL.** That argument does not survive contact with the
//! actual request: the runner's `POST` is unauthenticated (see the next
//! section), so *any* caller who can reach the route — not only the honest
//! runner echoing back exactly what it was given — can supply an arbitrary
//! `tenant_id`, and nothing before this fix distinguished the two. The
//! `ingest` precedent this module cited is inverted for this endpoint, not
//! parallel to it:
//! [`domain::service::ingest`](crate::domain::service::ingest)'s own header
//! states the decisive premise plainly — its tenant is not caller-supplied at
//! all, but taken from a [`TenantBound`](crate::domain::system_actor::TenantBound)
//! its caller already resolved from its own ticker enumeration or its own
//! request context. Here the tenant **is** caller-supplied; "this gear chose
//! the value" is a fact
//! about the honest runner and constrains nobody else. And legacy is not a
//! defence: legacy's `INSERT` (`analytics.rs:2623-2632`) binds only `repo_id`,
//! `branch`, `test_file` — no tenant column exists, so its own unauthenticated
//! write could corrupt one dataset but could not cross a tenant boundary that
//! does not exist there. This gear's tenancy is the one thing legacy did not
//! have to protect, and shipping the report endpoint with an unauthenticated
//! `tenant_id` would have made it the one place this gear's own added
//! security boundary went unenforced.
//!
//! **The fix: the callback URL carries an HMAC-SHA256 tag over
//! `(repo_id, branch, tenant_id)`, keyed by
//! `QaInsightsConfig::collect_report_signing_secret`, and
//! [`CollectService::record_count`] verifies it — before the tenant is bound
//! to anything — as the very first thing it does.** [`CollectService::sign`]
//! computes the tag when [`CollectService::collect_url`] builds the callback
//! URL; [`CollectService::verify_signature`] recomputes it from the report's
//! own `repo_id`/`branch`/`tenant_id` and compares against the `sig` query
//! parameter using `aws_lc_rs::hmac::verify`, which is constant-time by
//! construction. A caller without the secret cannot produce a tag that
//! verifies for **any** `(repo_id, branch, tenant_id)` triple, chosen or
//! guessed — this is what actually closes the cross-tenant vector, not the
//! `tenant_id` parameter's provenance.
//!
//! **Delimiter safety of the signed string, argued rather than assumed.**
//! [`signing_payload`] joins the three fields with `|` as
//! `format!("{repo_id}|{branch}|{tenant_id}")`. `repo_id` and `tenant_id` are
//! typed `Uuid` on both the signing and the verifying side — never re-parsed
//! out of a delimited string — and `Uuid`'s `Display` is always exactly 36
//! bytes in a fixed 8-4-4-4-12 hyphenated shape that can never contain `|`.
//! So the only variable-width, attacker-influenced field is `branch`, sitting
//! between two rigid, independently-typed anchors; there is no way for a
//! `branch` value (however it is chosen) to make two *different* typed
//! triples format to the identical byte string, because the verifier never
//! re-splits the string to recover its three fields — it recomputes the
//! format from the three values it already parsed independently via `Path`
//! and `Query` extraction.
//!
//! **Fail-closed on an unconfigured or too-short secret.**
//! [`CollectService::verify_signature`] refuses immediately, before touching
//! `aws_lc_rs` at all, when `collect_report_signing_secret` is empty — an
//! empty HMAC key is not "no protection", it is a *publicly known* key, which
//! is strictly worse than refusing outright, because it looks like a working
//! defence while offering none. `gear.rs`'s `init` also warns once at startup
//! when the secret is empty (Important 3 of the same review round), which is
//! the operator-visible half of this decision; this refusal is the
//! request-visible half.
//!
//! **Fix round 2, secret hygiene:** `is_empty()` alone let a one-character or
//! whitespace-only secret count as "configured", which is weak in the same
//! way an empty secret is but was not refused the same way.
//! [`MIN_SIGNING_SECRET_LEN`] closes that with a length floor — not a proof
//! of entropy, only the one property this code can actually check — see that
//! constant's own doc for the number and why it is not the hash's block
//! size.
//!
//! **So: [`resources::TEST_RESULT`](super::resources::TEST_RESULT) /
//! [`actions::COLLECT`](super::actions::COLLECT), tenant-only, for the
//! trigger; no PEP grant of any kind for the report.** Task 29's own header
//! raised exactly this as the open question this module inherits — "a write
//! path may need a different answer from a read path" — and the answer here
//! is not merely different in *degree* from the read side's `qa.test_result`
//! reuse; it is a different *mechanism* entirely, matching [`ingest`]'s
//! write rather than [`saved_views`]'s. `qa_test_case_collect` therefore does
//! **not** need its own PEP resource type: neither of this module's writers
//! goes through the PEP at all, so there is no policy decision for a fourth
//! resource type to change the shape of. [`resources::TEST_RESULT`]'s header
//! is still correct that a *read* riding along on it (Task 29's
//! `collect_counts`) carries a hazard if a policy ever answers with a
//! `resource_id` constraint; this module does not add a second one, because
//! it asks the PEP nothing.
//!
//! # The branch-in-path hazard, and why this gear's route is not
//! # `/qa/v1/collect/{repo_id}/{branch}`
//!
//! Task 27 shipped exactly this defect and had to fix it: a real branch name
//! routinely contains `/` (`feature/VHP-123-thing` is the ordinary case, not
//! the exception — this crate's own fixtures use `feature-x` and `main`, which
//! hide the problem rather than testing it), and a `/` inside a single path
//! segment is a **router 404**, not a handler error — nothing in `axum`'s
//! routing gives a test at this crate's tier any way to see it short of
//! crossing the HTTP boundary, which is exactly what Task 27's own remedy
//! (`serde_urlencoded` driven directly, confirmed byte-identical to axum's
//! `Query` decode path) exists to approximate without that tier.
//!
//! Legacy has the identical route shape,
//! `/api/collect/{repo_id}/{branch}` (`manager/src/services/collect.rs:90`),
//! and never hits this because it never has to: legacy's *plan* id is
//! sanitized before it reaches a URL (`compose_repo_plan_id`,
//! `manager/src/services/plans.rs:789-801`), but **legacy's collect branch is
//! not** — `format!("{}/api/collect/{}/{}", base, repo.id, branch)` interpolates
//! the raw branch string, and legacy's own router would 404 on
//! `feature/VHP-123-thing` exactly as this gear's would. Legacy simply never
//! ships a repository whose branch names collide with the character its own
//! router treats as a separator having been exercised in anger — the defect is
//! latent there too, not absent, and "legacy has it" is not a reason to port
//! it: the governing principle is to preserve legacy's *behavior*, not its
//! *bugs*, and adapt the implementation to this architecture.
//!
//! **Decided here: `branch` moves to a query parameter — `POST
//! /qa/v1/collect/{repo_id}?branch=&tenant_id=&sig=` — exactly Task 27's R41
//! shape, for the same reasons and one more.** The constraint that narrows the
//! decision beyond Task 27's case: *this gear controls both ends* of this
//! particular URL. Task 27's `plan_path` is read by a human or a client
//! composing a request by hand; this callback URL is built once, here, by
//! [`CollectService::collect_url`], and handed to qa-runs, which hands it to
//! the runner verbatim — nothing ever composes it by hand and nothing needs to
//! remember a percent-encoding convention. A query value needs no
//! percent-encoding for a literal `/` at all (RFC 3986's `query` production
//! admits `/` unescaped), which is the one respect in which this hazard is
//! *milder* than Task 27's: there, `%2F` was the alternative to a query
//! parameter and was rejected because proxies routinely normalise it; here,
//! moving to a query parameter sidesteps the encoding question rather than
//! choosing an answer to it. `api::rest::handlers::collect`'s own test module
//! drives a slash-bearing branch through the exact decode path
//! `report_collect_count` uses, over `serde_urlencoded` directly — Task 27's
//! own remedy, confirmed there to be byte-identical to axum's `Query` decode.
//!
//! The alternative this module rejected: keeping `{repo_id}/{branch}` and
//! documenting `%2F`. Rejected because a documented encoding convention is
//! still a convention every future reader of this module (and every proxy
//! between the runner and this gear) has to get right, where a query
//! parameter needs no convention at all.
//!
//! # The write key, and Task 29's read key, agree because both go through
//! # [`normalize_test_path`]
//!
//! [`CollectRepository::upsert_count`](crate::domain::repos::CollectRepository::upsert_count)
//! is keyed on `(tenant_id, repo_id, branch, test_file)`, and
//! [`domain::analytics::universe::expected_cases`](crate::domain::analytics::universe::expected_cases)
//! reads it back keyed on `(repo_id, test_file)` — **without** re-normalizing
//! `test_file` at the read site (verified by reading that function's body:
//! `exact.get(&(test.repo_id, test.test_file.as_str()))` matches
//! `count.test_file.as_str()` verbatim). So the read side's correctness
//! depends entirely on the write side having already normalized the path the
//! same way qa-catalog's own universe paths are normalized. [`CollectService::record_count`]
//! closes that: it runs `test_file` through
//! [`normalize_test_path`](crate::domain::analytics::universe::normalize_test_path)
//! before it ever reaches [`CollectRepository::upsert_count`], which is
//! legacy's own order too — `api_collect_report`'s
//! `normalize_test_path(body.test_file.trim())` (`analytics.rs:2619`) runs
//! before the `INSERT`. A collect report that skipped this step would still
//! upsert successfully — normalization is not a validation step, it changes
//! no shape the database rejects — and every "expected cases" number for that
//! file would then silently render its **static** count forever, because the
//! stored key would never match [`expected_cases`]'s lookup.
//!
//! # The `case_count` clamp needs an `i64` wire field, not [`CollectCount`]'s
//! # `u32`
//!
//! [`CollectCount::case_count`] is `u32` — Task 29 confirmed it and this
//! module inherits that type for the *stored* value. Legacy's clamp is
//! `body.case_count.max(0) as i32`
//! (`manager/src/routes/analytics.rs:2632`), applied to a **signed** wire
//! field (`CollectCountPayload::case_count: i64`, `:2609`). If this gear's
//! request DTO typed the wire field `u32` to match the storage type, a
//! negative value would fail to *deserialize* — a 400 from `serde`, before
//! [`CollectService::record_count`] ever runs — which is a different failure from legacy's
//! clamp-to-zero and is exactly the trap
//! `a_collect_report_clamps_negative_counts_and_rejects_an_empty_file` exists
//! to catch. [`CollectService::record_count`] therefore takes `case_count: i64` and clamps it
//! itself; `api::rest::dto::CollectCountReq::case_count` mirrors that
//! signedness on the wire so the clamp stays reachable through the real HTTP
//! path and not only through a service method a test could call directly.
//!
//! # `launched` counts launch calls qa-runs accepted, not repositories
//! # confirmed collecting — a fix-round-1 correction
//!
//! Legacy's `launched` genuinely counts repositories that got a workflow:
//! `launch_collect_for_repo` returns `Err` *before* launching anything when
//! the branch will not sync (`services/collect.rs:42-48`) or when the branch
//! has no test files (`:78-83`), and `run_collect_cycle` increments only on
//! `Ok` (`:168-171`). [`CollectService::run_collect_cycle`] increments on
//! `Ok` from [`RunsLauncher::launch_collect`] too, but that port's own header
//! records the reason the two are not equivalent: qa-runs resolves a
//! caller-supplied branch synchronously and validates its *existence* only at
//! dispatch, which runs after `launch()` has already returned `Ok`. So for a
//! branch present in one repository and absent from a second, legacy answers
//! `launched: 1`; this gear answers `launched: 2`, and the second repository
//! launches a run that can never successfully report back.
//!
//! **`launched` therefore means "launch calls qa-runs accepted", not
//! "repositories now collecting exact counts".** Pre-checking branch
//! existence against qa-catalog before every launch was considered and
//! rejected for this fix round: it would add one `qa-catalog` round trip per
//! repository per cycle for a property qa-runs itself does not surface
//! synchronously, so a pre-check here could still disagree with what qa-runs
//! decides moments later at dispatch — the two systems would need to agree on
//! branch resolution for the check to be trustworthy, and only qa-runs' own
//! resolution is authoritative. `route/collect.rs`'s endpoint description and
//! [`CollectService::run_collect_cycle`]'s own doc both state this meaning
//! explicitly rather than leaving `launched` to imply legacy's stronger
//! guarantee.
//!
//! # Divergences from legacy, summarized
//!
//! Gathered here, `saved_views`' precedent, because this module accumulated
//! four and a scattered list is one a reviewer has to reassemble by hand:
//!
//! 1. **`branch` is a query parameter, not a second path segment** — "The
//!    branch-in-path hazard", above.
//! 2. **The report endpoint asks the PEP nothing, and is authenticated by an
//!    HMAC signature instead of a grant** — legacy has no authorization model
//!    on this route to diverge from; this gear adds one legacy never needed,
//!    because legacy has no tenant to protect. See "Fix round 1, Critical 1",
//!    above.
//! 3. **`launched` counts accepted launch calls, not confirmed collections**
//!    — immediately above.
//! 4. **A failed repository-discovery read is a `500`, not a `200` reporting
//!    zero.** Legacy's `run_collect_cycle` swallows a failed repository
//!    listing (`services/collect.rs:158-162`,
//!    `.unwrap_or_default()` → an empty `Vec` → `launched: 0` → `200`).
//!    [`CollectService::run_collect_cycle`] propagates a failed
//!    `CatalogReader::list_universe` read as an `Err` (documented on that
//!    method's own `# Errors`), for
//!    [`super::analytics::AnalyticsService::load_universe_and_rows`]'s
//!    reason: a broken qa-catalog must not render as an idle deployment with
//!    nothing to collect. Deliberate, not an oversight, and named here
//!    because it is a response-behaviour change a caller can observe.
//!
//! # The archive-repository skip is not ported, because this gear has nothing
//! # to port it onto
//!
//! Legacy rejects (`launch_collect_for_repo:38-40`) and separately skips
//! (`run_collect_cycle:165-167`) a repository whose `source_type` is
//! `"archive"` — an archive has no branch to collect, so collecting it is a
//! category error, not merely a failure. `qa_catalog_sdk::UniverseTest`,
//! this gear's only source of "which repositories exist"
//! (`CollectService::run_collect_cycle`'s own doc), carries no `source_type`
//! or archive concept at all — checked by reading the type's field list, not
//! assumed from its absence in this module. There is therefore nothing in
//! this architecture's universe to test an archive predicate against: a
//! repository qa-catalog exposes no plans for (which is what an archive
//! would look like, if this gear could see it as one) never appears in
//! [`CollectService::run_collect_cycle`]'s repository set at all, and is
//! skipped for that reason rather than for being recognised as an archive.
//! The distinction is real — a *registered, plan-bearing* archive repository,
//! if qa-catalog's data model ever grows one, would not be skipped here the
//! way legacy skips it today — but is not this task's to close: it would need
//! a `qa-catalog` concept this gear currently has no way to ask about.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use aws_lc_rs::{hkdf, hmac};
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use qa_insights_sdk::CollectCount;

use crate::domain::analytics::universe::normalize_test_path;
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{
    CollectMetrics, CollectOutcome, CollectReportOutcome, NoopMetrics,
};
use crate::domain::ports::{CatalogReader, RunsLauncher};
use crate::domain::repos::CollectRepository;
use crate::domain::service::{DbProvider, actions, emit, refuse_scope_beyond_tenant, resources};
use crate::domain::system_actor::{TenantBound, for_collect_report};

/// Trim `branch`, treat the empty result as absent — legacy's
/// `normalize_optional` (`manager/src/routes/analytics.rs:2070-2075`), ported a
/// second time here for [`crate::domain::service::saved_views::parse_scope`]'s
/// own stated reason: coupling this module to `domain::analytics::query` or
/// `domain::analytics::universe` for one shared four-line literal would be the
/// wrong dependency.
fn normalize_branch(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// [`CollectService::collect_url`]'s query-string encoding, factored out to a
/// free function (no `&self`) for one reason: `api::rest::dto`'s test module
/// can call it directly, with a hand-picked `branch`/`tenant_id`/`sig`, and
/// decode the result through the real `CollectReportQuery` — without
/// constructing a whole `CollectService` test double just to reach this
/// encoder.
///
/// # Phase B fix wave, Finding 6 — the test this exists for
///
/// `api::rest::dto::CollectReportQuery`'s own doc states "this struct's three
/// fields must agree with `collect_url`'s private `Query` type", and names
/// the consequence of a drift: "a field renamed on one side and not the
/// other silently drops the value across the query string." The test that
/// was supposed to pin that,
/// `collect_tests::the_collect_url_round_trips_a_slash_bearing_branch_through_its_query_string`,
/// declares its own **third**, local `Decoded` struct to decode into — a
/// deliberate and legitimate choice given this crate's layering rule (domain
/// code must not import `api::rest::dto`; see that test's own doc) but one
/// with a real consequence: it pins `collect_url` against a struct that
/// mirrors `CollectReportQuery` by the test author's memory, not against
/// `CollectReportQuery` itself. Renaming a field on `CollectReportQuery`
/// alone, while leaving this encoder's field names in matching lockstep with
/// a hand-updated hardcoded literal elsewhere (plausible: a developer
/// "helpfully" updates every string literal they can find that mentions the
/// old name, without realizing this encoder is a separate file they have not
/// touched), would leave every existing test green while the real route
/// 400s on every report. Verified, not asserted: reverting this function's
/// extraction and renaming `CollectReportQuery::branch`'s wire name alone (a
/// `#[serde(rename = "...")]`, keeping the Rust identifier so the crate still
/// compiles) left the *pre-fix-wave* suite at 31 of 33 passing in this
/// module's own test file's neighborhood — 2 failures, not 0, because two
/// unrelated tests in `api::rest::handlers::collect` happen to hardcode a
/// literal query string that also encodes today's field name. Renaming this
/// encoder's own field instead (leaving `CollectReportQuery` untouched) left
/// exactly one different test red — the one this function's doc opens with.
/// Neither existing test, independently, ties the two real types together;
/// [`api::rest::dto`]'s own test module now does.
#[allow(
    clippy::expect_used,
    reason = "serde_urlencoded::to_string over a struct of &str/Uuid/String fields cannot fail; \
              see collect_url's own reason (before this fix wave's extraction) for why a silent \
              fallback on this path is worse than a panic that can never trigger."
)]
pub(crate) fn encode_collect_report_query(branch: &str, tenant_id: Uuid, sig: String) -> String {
    #[derive(serde::Serialize)]
    struct Query<'a> {
        branch: &'a str,
        tenant_id: Uuid,
        sig: String,
    }

    serde_urlencoded::to_string(Query {
        branch,
        tenant_id,
        sig,
    })
    .expect("a struct of &str/Uuid/String fields always serializes as a query string")
}

/// The collect trigger, the runner's report, and the hourly cycle.
///
/// Generic over the repository for [`super::analytics::AnalyticsService`]'s
/// reason: [`CollectRepository`]'s methods are generic over the `DBRunner`
/// they run on, so the trait is not object-safe and the parameter propagates
/// to [`super::AppServices`]. [`super::AppServices::collect`] and
/// [`super::AppServices::analytics`] share the identical `C` — one
/// `OrmCollectRepository` instance, cloned twice (it is a zero-sized unit
/// struct), never two competing sources of truth over one table.
///
/// # Three `String` config values, and why they are three distinct types
///
/// Fix round 2 of this task's review found that
/// [`CollectService::new`] took `default_collect_branch`,
/// `collect_report_base_url` and `collect_report_signing_secret` as three
/// adjacent `String` parameters, and that the `#[expect(clippy::too_many_arguments)]`
/// reason on that constructor claimed "all eight parameter types are
/// distinct" while itself listing "three `String`" — a contradiction in its
/// own text. The real hazard: swapping `collect_report_base_url` and
/// `collect_report_signing_secret` at the single call site
/// (`AppServices::new`, `domain::service::mod`) compiled silently, and would
/// have published the signing secret into every collect callback URL while
/// keying the HMAC on the base URL instead — the exact class of bug Critical
/// 1's fix exists to make impossible for an *attacker* to cause, reintroduced
/// as something a *refactor* could cause instead.
///
/// [`DefaultCollectBranch`], [`CollectReportBaseUrl`] and
/// [`CollectReportSigningSecret`] close it the way the review asked: the
/// compiler rejects the transposition now, not a comment asking a future
/// editor to notice it.
pub struct CollectService<C> {
    db: Arc<DbProvider>,
    collect: C,
    /// The analytics universe, read from qa-catalog — [`run_collect_cycle`]'s
    /// source of "every repository", for the reason its own doc gives:
    /// legacy's `list_repositories(None)` has no analogue in this
    /// architecture, and the universe is the set of repositories this gear
    /// already has a reason to know about.
    catalog: Arc<dyn CatalogReader>,
    /// The one launch this gear performs against qa-runs.
    launcher: Arc<dyn RunsLauncher>,
    policy_enforcer: authz_resolver_sdk::PolicyEnforcer,
    default_collect_branch: DefaultCollectBranch,
    collect_report_base_url: CollectReportBaseUrl,
    collect_report_signing_secret: CollectReportSigningSecret,
    /// Where this service's telemetry goes. Never `Option`: [`NoopMetrics`]
    /// is the default a caller that wires nothing gets, so every emission
    /// below is unconditional and the "silent when no adapter is installed"
    /// promise is structural rather than a branch somebody has to remember.
    metrics: Arc<dyn CollectMetrics>,
    /// Latched by [`emit`] the first time an emission panics, after which this
    /// service stops calling its adapter. Per service, not global — see
    /// [`emit`]'s own doc for why a process-wide latch would be wrong both in
    /// production and under a threaded test run.
    metrics_silenced: AtomicBool,
}

/// Legacy's `DEFAULT_COLLECT_BRANCH` (`manager/src/services/collect.rs:19`),
/// substituted whenever [`CollectService::trigger`] is asked to collect
/// without naming a branch — `analytics.rs:2662`,
/// `branch.unwrap_or(DEFAULT_COLLECT_BRANCH)`. See [`CollectService`]'s own
/// header, "Three `String` config values", for why this is a type rather
/// than a bare `String`.
#[derive(Clone, Debug)]
pub struct DefaultCollectBranch(pub String);

/// `QaInsightsConfig::collect_report_base_url` — the scheme-and-host half of
/// the URL [`CollectService::collect_url`] builds. See that config field's
/// own doc for why no default is provided, and [`CollectService`]'s header
/// for why this is a type rather than a bare `String`.
#[derive(Clone, Debug)]
pub struct CollectReportBaseUrl(pub String);

/// `QaInsightsConfig::collect_report_signing_secret` — the HMAC key
/// [`CollectService::sign`] and [`CollectService::verify_signature`] share.
/// See [`CollectService`]'s header for why this is a type rather than a bare
/// `String`, and this module's header ("Fix round 1, Critical 1") for why it
/// exists at all.
///
/// `Debug` is implemented by hand below, not derived, so an accidental
/// `{:?}` anywhere in this gear's own Rust code — a panic message, a stray
/// `tracing::debug!` — cannot print the secret. This is narrower than, and
/// does not substitute for, the config-dump exposure
/// `QaInsightsConfig::collect_report_signing_secret`'s own doc records: that
/// surface serializes the *config struct*, which this type's redacted
/// `Debug` never runs through.
#[derive(Clone)]
pub struct CollectReportSigningSecret(pub String);

impl std::fmt::Debug for CollectReportSigningSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CollectReportSigningSecret")
            .field(&"<redacted>")
            .finish()
    }
}

/// The minimum accepted length (bytes) of
/// [`QaInsightsConfig::collect_report_signing_secret`] before
/// [`CollectService::verify_signature`] treats it as configured at all.
///
/// Fix round 2, secret hygiene: `verify_signature` originally tested only
/// `is_empty()`, so a one-character or whitespace-only secret counted as
/// "configured" and would have been accepted at face value — weak, but not
/// refused the way an empty secret is. 16 bytes (128 bits) is not a proof of
/// entropy (a 16-byte string of the same repeated character is exactly as
/// weak as it looks), only a floor on *length*, which is the one property
/// this code can actually check; the deployment is still responsible for
/// generating the value randomly. Chosen as the shorter of the two common
/// floors for a shared HMAC secret (128 bits) rather than the hash's own
/// block size (64 bytes for SHA-256), because this is a per-deployment
/// operational secret an operator types into configuration, not a
/// programmatically generated key.
const MIN_SIGNING_SECRET_LEN: usize = 16;

/// Whether `secret` clears [`MIN_SIGNING_SECRET_LEN`] once trimmed — the one
/// predicate [`CollectService::verify_signature`] refuses on, and the one
/// [`crate::gear`]'s boot-time check must warn on.
///
/// Phase B fix wave, Finding 1: the two call sites used to test different
/// things — this function tested `is_empty()` and the boot warning in
/// `gear::init` also tested `is_empty()`, but fix round 2 (secret hygiene)
/// raised this function's floor to sixteen characters without carrying the
/// change back to the boot warning. The result was a real gap: a
/// 1–15-character secret passed the boot check silently and then failed
/// every collect report at request time with `Forbidden` — exactly the
/// silent misconfiguration the boot warning exists to prevent. Sharing this
/// one predicate closes the gap the way a shared floor should: the two call
/// sites cannot drift again because there is only one definition of
/// "configured" to drift from.
pub(crate) fn signing_secret_is_configured(secret: &str) -> bool {
    secret.trim().len() >= MIN_SIGNING_SECRET_LEN
}

impl<C> CollectService<C>
where
    C: CollectRepository,
{
    /// `metrics` is `None` for every caller that does not measure this
    /// service — which is most of its tests — and resolves to
    /// [`NoopMetrics`]. The parameter is an `Option` rather than a required
    /// `Arc<dyn CollectMetrics>` so that the one production call site
    /// (`AppServices::new`) is the only place that has to name an adapter, and
    /// so that "no adapter" is spelled the same way at every construction
    /// site instead of each one inventing its own no-op.
    ///
    /// **No longer `const`.** `Arc::new(NoopMetrics)` is not a constant
    /// expression; nothing called this in a constant context.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "each parameter type is distinct after fix round 2's newtypes: \
                  DefaultCollectBranch, CollectReportBaseUrl and CollectReportSigningSecret \
                  exist specifically so the three-String transposition hazard that reason used \
                  to (wrongly) claim was already impossible cannot recur. Arc<DbProvider>, C, \
                  two Arc<dyn ..>, PolicyEnforcer and the metrics port are the remaining six; a \
                  params struct would still have exactly one production construction site and \
                  would unpack it again immediately, buying nothing the type system does not \
                  already enforce."
    )]
    pub fn new(
        db: Arc<DbProvider>,
        collect: C,
        catalog: Arc<dyn CatalogReader>,
        launcher: Arc<dyn RunsLauncher>,
        policy_enforcer: authz_resolver_sdk::PolicyEnforcer,
        default_collect_branch: DefaultCollectBranch,
        collect_report_base_url: CollectReportBaseUrl,
        collect_report_signing_secret: CollectReportSigningSecret,
        metrics: Option<Arc<dyn CollectMetrics>>,
    ) -> Self {
        Self {
            db,
            collect,
            catalog,
            launcher,
            policy_enforcer,
            default_collect_branch,
            collect_report_base_url,
            collect_report_signing_secret,
            metrics: metrics.unwrap_or_else(|| Arc::new(NoopMetrics)),
            metrics_silenced: AtomicBool::new(false),
        }
    }

    /// Emit one collect-report outcome, guarded by this service's own latch.
    ///
    /// **One call site, deliberately**: [`Self::record_count`], on the outcome
    /// [`Self::write_reported_count`] handed back. The refusals inside that
    /// function do not emit — each returns its label instead, and the single
    /// emission happens once above them, on whichever label came out. That is
    /// what makes "one report, one increment" a property of the shape rather
    /// than of six call sites all remembering to fire exactly once.
    ///
    /// A named method rather than the [`emit`] call spelled out there, so
    /// `record_count` reads as the request path it is and this service's latch
    /// is named in one place.
    fn report(&self, outcome: CollectReportOutcome) {
        emit(&self.metrics_silenced, || {
            self.metrics.collect_report(outcome);
        });
    }

    /// Record the exact case count the runner reports for one file —
    /// `POST /qa/v1/collect/{repo_id}`.
    ///
    /// `api_collect_report` (`manager/src/routes/analytics.rs:2614-2642`). See
    /// this module's header for why this takes a bare `tenant: TenantBound`
    /// rather than a caller's own `SecurityContext`, and why that is not a
    /// narrowing of legacy's (nonexistent) authorization on this route.
    ///
    /// # The system actor is minted *inside* this function, after signature
    /// # verification, not handed in already built — mirroring
    /// # `domain::service::ingest::IngestService::read_run_projection`'s
    /// # `site: SystemActorSite, tenant: TenantBound` shape, not
    /// # `record_count`'s own pre-fix-wave shape
    ///
    /// Phase B fix wave, Finding 5: an earlier revision took `ctx:
    /// &SecurityContext`, already built by the handler via
    /// [`crate::domain::system_actor::for_collect_report`] before this
    /// function — and therefore before [`Self::verify_signature`] — ever ran.
    /// `for_collect_report` logs `"qa-insights system actor constructed"` on
    /// every call, so an anonymous request carrying any non-nil `tenant_id`
    /// produced that audit line even when the signature check immediately
    /// afterward refused it with `Forbidden` — the log claimed attribution
    /// for a request this endpoint never actually accepted. Taking
    /// `tenant: TenantBound` instead and minting the context here, after
    /// [`Self::verify_signature`] and the two shape checks succeed, means the
    /// audit line now fires only for a report this function goes on to
    /// accept.
    ///
    /// # `signature` is checked first, before the tenant is bound to anything
    ///
    /// This is Critical 1 of fix round 1 — see this module's header in full.
    /// [`Self::verify_signature`] runs before any other guard, before the
    /// database is touched, and before the tenant is used to scope
    /// anything: a caller that cannot prove it is echoing back a URL this
    /// gear itself signed learns nothing else about why its request failed.
    ///
    /// # The three guards, and only the three guards
    ///
    /// * **The signature must verify**, over `(repo_id, branch,
    ///   tenant.get())` against `collect_report_signing_secret` —
    ///   new in fix round 1, [`DomainError::Forbidden`].
    /// * **`branch`, after trimming, must be non-blank** — new in fix round
    ///   1: the query-parameter move that closed the branch-in-path hazard
    ///   made an empty `branch` representable where legacy's path segment
    ///   could not be empty at all, and an empty-keyed row is one
    ///   [`domain::analytics::universe::expected_cases`](crate::domain::analytics::universe::expected_cases)
    ///   can never match. [`DomainError::Validation`].
    /// * **A negative `case_count` clamps to zero**, matching legacy's
    ///   `case_count.max(0)` (`:2632`) exactly — this is not a rejection.
    /// * **An empty `test_file`, after normalization, is a
    ///   [`DomainError::Validation`]**, matching legacy's `if file.is_empty()
    ///   { return StatusCode::BAD_REQUEST }` (`:2620-2622`).
    ///
    /// (Four bullets, not three — the doc heading undercounts on purpose:
    /// the signature check is stated separately above because it runs
    /// first and for a different reason than the other three, which are
    /// legacy's own guards plus the one query-parameter-shape guard fix
    /// round 1 added alongside it.)
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when `signature` does not verify, or when
    /// `collect_report_signing_secret` is unconfigured (fail-closed — see
    /// this module's header). [`DomainError::Validation`], naming `branch` or
    /// `test_file`, on either blank-after-trimming case.
    /// [`DomainError::Database`] on a storage failure.
    pub async fn record_count(
        &self,
        tenant: TenantBound,
        repo_id: Uuid,
        branch: &str,
        signature: &str,
        test_file: &str,
        case_count: i64,
    ) -> Result<(), DomainError> {
        let result = self
            .write_reported_count(tenant, repo_id, branch, signature, test_file, case_count)
            .await;
        let outcome = match &result {
            Ok(()) => CollectReportOutcome::Recorded,
            Err((outcome, _)) => *outcome,
        };
        self.report(outcome);
        result.map_err(|(_, error)| error)
    }

    /// [`Self::record_count`]'s body, returning the metric label alongside the
    /// error rather than only the error.
    ///
    /// # Why the label cannot be derived from the returned error
    ///
    /// [`Self::verify_signature`] deliberately answers all three of its
    /// refusal paths with one [`DomainError::Forbidden`], so that a caller
    /// cannot use the response as an oracle for which guess was closer — its
    /// own doc states that in as many words. That is the right answer for the
    /// *response* and it destroys exactly the distinction an operator needs:
    /// "no secret is configured in this deployment" and "somebody is guessing
    /// at this endpoint" arrive as the same value. So the classification is
    /// carried out of the refusal, beside the error, instead of being
    /// reconstructed from it — the two never disagree because there is only
    /// one of them.
    ///
    /// Splitting the function is also what keeps the accepted path counted: a
    /// single body emitting at each `return` would need an emission on the
    /// success tail too, which is exactly the one a later edit forgets.
    async fn write_reported_count(
        &self,
        tenant: TenantBound,
        repo_id: Uuid,
        branch: &str,
        signature: &str,
        test_file: &str,
        case_count: i64,
    ) -> Result<(), (CollectReportOutcome, DomainError)> {
        let tenant_id = tenant.get();
        self.verify_signature(repo_id, branch, tenant_id, signature)
            .map_err(|refusal| (CollectReportOutcome::from(refusal), DomainError::Forbidden))?;

        if branch.trim().is_empty() {
            return Err((
                CollectReportOutcome::Invalid,
                DomainError::Validation {
                    field: "branch".to_owned(),
                    message: "branch is required".to_owned(),
                },
            ));
        }

        let file = normalize_test_path(test_file.trim());
        if file.is_empty() {
            return Err((
                CollectReportOutcome::Invalid,
                DomainError::Validation {
                    field: "test_file".to_owned(),
                    message: "test_file is required".to_owned(),
                },
            ));
        }

        // Only now — signature verified, shape checked — is the system actor
        // minted. `for_collect_report` logs the audit line; minting it any
        // earlier (this function's pre-fix-wave shape did, via a `ctx` the
        // handler built before calling this at all) would have logged
        // "system actor constructed" for requests the signature check above
        // was about to refuse. Nothing below reads `_ctx` — its only purpose
        // is that log line — but the module's header still calls this the
        // context "the tenant is not caller-asserted; it is [this context's]
        // own" for the reason given there.
        let _ctx = for_collect_report(tenant);

        // No PDP round-trip — see this module's header. The tenant is not
        // caller-asserted; it comes from the callback URL this
        // gear itself built, and which the signature check above has just
        // proven this gear itself signed for this exact
        // `(repo_id, branch, tenant_id)` triple.
        let scope = AccessScope::for_tenant(tenant_id);
        let conn = self
            .db
            .conn()
            .map_err(|error| (CollectReportOutcome::Failed, error))?;

        let count = CollectCount {
            repo_id,
            branch: branch.to_owned(),
            test_file: file,
            // `.max(0)` first, then a saturating cast: legacy's own clamp is
            // `case_count.max(0) as i32` over an `i64`, and `i32::MAX` cases
            // in one file is already an impossible report; `u32::MAX` is the
            // same non-event one width wider, and clamping there rather than
            // panicking or wrapping keeps a wire value this large a stored
            // (huge, wrong) number instead of a crash.
            case_count: u32::try_from(case_count.max(0)).unwrap_or(u32::MAX),
            collected_at: OffsetDateTime::now_utc(),
        };

        self.collect
            .upsert_count(&conn, &scope, tenant_id, count)
            .await
            // A storage failure is this gear's own; nothing else reaches this
            // tail, because every caller-facing refusal returned above.
            .map_err(|error| (CollectReportOutcome::Failed, error))
    }

    /// Launch the collect job on demand — `POST /qa/v1/analytics/collect`.
    ///
    /// `api_collect_trigger` (`manager/src/routes/analytics.rs:2655-2665`).
    /// **This is the PDP-checked half** — see this module's header — and it is
    /// the only public method here that is. `branch` is
    /// [`normalize_branch`]ed and defaulted to
    /// [`Self::default_collect_branch`], exactly as legacy's
    /// `normalize_optional(query.branch.as_deref())` then
    /// `.unwrap_or(DEFAULT_COLLECT_BRANCH)` (`:2659-2662`).
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies.
    /// [`DomainError::UnsupportedScope`] when the compiled scope constrains
    /// more than the tenant — see [`super::refuse_scope_beyond_tenant`].
    /// Otherwise, whatever [`Self::run_collect_cycle`] raises.
    pub async fn trigger(
        &self,
        ctx: &SecurityContext,
        branch: Option<&str>,
    ) -> Result<(usize, String), DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_RESULT, actions::COLLECT, None)
            .await
            .map_err(DomainError::from)?;
        refuse_scope_beyond_tenant(&scope, resources::TEST_RESULT_NAME)?;

        let branch = normalize_branch(branch)
            .unwrap_or(self.default_collect_branch.0.as_str())
            .to_owned();
        let launched = self.run_collect_cycle(ctx, &branch).await?;
        Ok((launched, branch))
    }

    /// Launch a collect workflow for `branch` of every repository the
    /// caller's universe admits. Repositories that fail to launch are
    /// skipped, not fatal. Returns how many launch calls qa-runs accepted —
    /// see this module's header, "`launched` counts launch calls qa-runs
    /// accepted, not repositories confirmed collecting", before reading this
    /// as a stronger guarantee than it is.
    ///
    /// `run_collect_cycle` (`manager/src/services/collect.rs:157-179`). Legacy
    /// lists every (git) repository and launches one workflow per repository,
    /// warning and continuing past a per-repository failure
    /// (`:168-176`) — this does the same, over
    /// [`RunsLauncher::launch_collect`] rather than legacy's in-process
    /// `launch_collect_for_repo`.
    ///
    /// # No PDP grant here — callable, and called two ways
    ///
    /// This method itself asks the PEP nothing: [`Self::trigger`] compiles and
    /// checks a scope **before** calling this, and Task 40's ticker calls this
    /// directly under a system actor of its own
    /// (`system_actor::for_collect_cycle`), which — like every background path
    /// in this gear — has no PDP grant to check in the first place
    /// (`domain::service::ingest`'s header states the reason). Putting the check
    /// here would make the ticker's call either fail closed on every tick or
    /// need a second, fictional caller identity; putting it in [`Self::trigger`]
    /// keeps exactly one PDP decision per real caller.
    ///
    /// # Repository discovery is branch-independent, and that is deliberate
    ///
    /// The repository *set* comes from
    /// `CatalogReader::list_universe(ctx, None, None)` — every product, each
    /// repository's own **default** branch — not from `branch`. Legacy's
    /// analogue, `list_repositories(None)`, is unconditional in the same way:
    /// it lists every registered repository regardless of which branch is
    /// about to be collected, and the branch is checked per repository during
    /// the *launch*, not during discovery. This gear has no repository table
    /// of its own to enumerate; the universe is the set of repositories this
    /// gear already has a reason to know about, and a repository absent from
    /// every product's default-branch universe is a repository with no
    /// registered plans at all, which legacy's own bundle-building
    /// (`:56-83`, "no test files present") would fail identically for.
    ///
    /// # A launch failure is never distinguished from a missing branch
    ///
    /// [`RunsLauncher::launch_collect`]'s own header explains why: qa-runs
    /// resolves a *specific*, caller-supplied branch synchronously but
    /// validates its existence only at dispatch, so whether "this repository
    /// lacks this branch" surfaces here as an `Err` at all is not a contract
    /// this port makes. This method does not need it to: **every** launch
    /// failure is treated the same way legacy treats "no such branch" and "no
    /// test files" alike — logged, skipped, not fatal to the cycle.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] or [`DomainError::Internal`] when
    /// qa-catalog's universe read fails — the repository discovery step is not
    /// laundered into "collect nothing", for
    /// [`super::analytics::AnalyticsService::load_universe_and_rows`]'s own
    /// reason: a broken sibling must not look like an idle deployment.
    pub async fn run_collect_cycle(
        &self,
        ctx: &SecurityContext,
        branch: &str,
    ) -> Result<usize, DomainError> {
        let started = Instant::now();
        let result = self.collect_every_repository(ctx, branch).await;
        let outcome = match &result {
            Ok(_) => CollectOutcome::Completed,
            Err(error) => CollectOutcome::from(error),
        };
        // Read before the emission, so a slow adapter cannot inflate the
        // duration it is being handed.
        let elapsed = started.elapsed();
        emit(&self.metrics_silenced, || {
            self.metrics.collect_cycle(outcome, elapsed);
        });
        result
    }

    /// [`Self::run_collect_cycle`]'s body, unmeasured.
    ///
    /// The split is what makes the cycle the measured unit: the wrapper reads
    /// the clock once around this whole call, so one cycle is one sample on
    /// both instruments however many repositories it launched into, and the
    /// per-repository failures this loop swallows cannot each become their own
    /// observation.
    async fn collect_every_repository(
        &self,
        ctx: &SecurityContext,
        branch: &str,
    ) -> Result<usize, DomainError> {
        let universe = self.catalog.list_universe(ctx, None, None).await?;
        let repo_ids: BTreeSet<Uuid> = universe.iter().map(|test| test.repo_id).collect();

        // ── Unmeasured, and this is the disclosure rather than an oversight ──
        //
        // The loop below swallows every per-repository launch failure the same
        // way `domain::service::jira_poller` swallows a per-bug one: logged,
        // skipped, never fatal. The poller's version of that defect is what
        // `domain::ports::metrics::JiraBugOutcome` exists to make visible; this
        // one is still invisible. A cycle that failed to launch for every
        // repository in the universe reports `completed` on
        // `crate::domain::metrics::QA_INSIGHTS_COLLECT`, exactly like a cycle
        // that launched them all, and `launched` — the count that would tell
        // them apart — is returned to the caller and reaches no series at all.
        //
        // Deliberately not closed here. The observability plan instruments at
        // pass boundaries and names the per-bug counter as its one exception;
        // adding an eighth family unasked is a wider change than the task that
        // wrote this comment was given. **Recorded because there is no later
        // qa-insights task to inherit it** — the phase continues in other
        // gears.
        //
        // Closing it is one counter and one emission: a counter family named
        // qa_insights_collect_launch_total, with a two-value `outcome` label
        // (launched / refused), incremented in the two arms below. It needs no
        // new call site, no new label taxonomy, and no change to the cycle
        // family: the cycle stays one observation and the launches become
        // their own, which is the same split the poll and the per-bug families
        // already use.
        let mut launched = 0usize;
        for repo_id in repo_ids {
            let url = self.collect_url(repo_id, branch, ctx.subject_tenant_id());
            match self
                .launcher
                .launch_collect(ctx, repo_id, branch, &url)
                .await
            {
                Ok(()) => launched += 1,
                Err(err) => {
                    tracing::warn!(
                        %repo_id,
                        branch,
                        error = %err,
                        "collect launch failed for repository; skipping, not fatal to the cycle",
                    );
                }
            }
        }
        Ok(launched)
    }

    /// Build the URL this gear hands the runner (via qa-runs'
    /// `VHP_COLLECT_URL`, D2) to report exact case counts back to.
    ///
    /// See this module's header, "The branch-in-path hazard", for why `branch`
    /// rides a query parameter rather than a second path segment, and
    /// [`crate::domain::system_actor`]'s header for why `tenant_id` rides one
    /// too: there is no session on the route this URL targets, so the tenant
    /// the eventual report writes under has to travel some other way, and this
    /// gear is the one party in this exchange positioned to choose how.
    ///
    /// `serde_urlencoded` is the same encoder axum's `Query` extractor decodes
    /// with (`api::rest::dto::CollectReportQuery`), which is what makes a
    /// slash- or `&`-bearing branch round-trip rather than merely "usually
    /// working" — see `api::rest::handlers::collect`'s own decode test. `sig`
    /// is [`Self::sign`]'s output — see this module's header, "Fix round 1,
    /// Critical 1", for what it protects and why. The encoding itself is
    /// [`encode_collect_report_query`], factored out so `api::rest::dto`'s
    /// own test module can drive the identical, real production encoder
    /// against the real `CollectReportQuery` — see that function's doc.
    fn collect_url(&self, repo_id: Uuid, branch: &str, tenant_id: Uuid) -> String {
        let sig = self.sign(repo_id, branch, tenant_id);
        let query = encode_collect_report_query(branch, tenant_id, sig);

        format!(
            "{}/qa/v1/collect/{repo_id}?{query}",
            self.collect_report_base_url.0.trim_end_matches('/'),
        )
    }

    /// The HMAC-SHA256 tag over `(repo_id, branch, tenant_id)`, hex-encoded —
    /// [`Self::collect_url`]'s `sig` query parameter.
    ///
    /// See this module's header for the full argument: `aws_lc_rs::hmac`
    /// rather than `sha2`/`hmac` directly (Dylint `DE0708` bans a new
    /// non-allow-listed `sha2` import), and why `signing_payload`'s `|`-joined
    /// format cannot be confused across two different `(repo_id, branch,
    /// tenant_id)` triples.
    ///
    /// **Task 7: the HMAC key is `tenant_id`'s own key, HKDF-derived from
    /// `collect_report_signing_secret`** — see [`derive_signing_key`] for the
    /// construction and what it does and does not mitigate — rather than
    /// `collect_report_signing_secret` used directly as the key for every
    /// tenant: a compromised root secret still derives any tenant's key on
    /// demand, but a leaked *derived* key now verifies for the one tenant it
    /// was derived for, not for every tenant at once.
    fn sign(&self, repo_id: Uuid, branch: &str, tenant_id: Uuid) -> String {
        let key = derive_signing_key(&self.collect_report_signing_secret.0, tenant_id);
        let tag = hmac::sign(&key, signing_payload(repo_id, branch, tenant_id).as_bytes());
        hex::encode(tag.as_ref())
    }

    /// Verify [`Self::sign`]'s tag. Fails closed (never verifies) when
    /// `collect_report_signing_secret` is empty or shorter than
    /// [`MIN_SIGNING_SECRET_LEN`], and on any signature that does not decode
    /// as hex or does not match — `aws_lc_rs::hmac::verify` is constant-time
    /// (confirmed by reading `aws-lc-rs`'s own implementation, not assumed
    /// from its name), so neither arm leaks timing information about which
    /// byte first differed.
    ///
    /// # What the tag does and does not cover — stated, not left implicit
    ///
    /// The tag binds exactly `(repo_id, branch, tenant_id)` — the natural key
    /// of the row a report writes to, minus the file. It carries **no
    /// expiry and no nonce**, and `test_file`/`case_count` are **outside**
    /// the MAC entirely. Both are deliberate, not gaps this task ran out of
    /// time to close:
    ///
    /// * **No expiry or nonce**: a single collect job reports many files over
    ///   the run of one workflow, all against the one URL qa-runs was handed
    ///   at launch, and that workflow's own duration is not a fixed, known
    ///   quantity this gear could bound in advance. A signature that expired
    ///   mid-job would fail some of that job's own, legitimate reports.
    /// * **`test_file`/`case_count` outside the MAC**: the same reasoning one
    ///   level down — the runner must be free to report an unbounded number
    ///   of distinct files against one signed URL, so those two fields could
    ///   never be part of a signature computed once, before any file is
    ///   known.
    ///
    /// The consequence, stated plainly: **whoever holds one legitimately
    /// issued callback URL can write arbitrary `case_count` values for
    /// arbitrary `test_file` names under that URL's `(repo_id, branch,
    /// tenant_id)`, indefinitely** — there is no server-side revocation and
    /// no time bound. This is a narrower, and *accepted*, exposure next to
    /// the one Critical 1 closed: the triple a URL is scoped to still cannot
    /// be forged or reassigned to a different tenant or repository, only its
    /// own per-file counts can be over-reported by whoever the runner (or
    /// anything that can read the runner's own environment) is. The signed
    /// triple is exactly what this endpoint's authorization boundary is
    /// about; the per-file payload is data the boundary was never meant to
    /// bound.
    ///
    /// # Errors
    ///
    /// [`SignatureRefusal`], naming which of the three paths refused.
    ///
    /// **The caller still cannot distinguish them, and that has not
    /// changed**: [`Self::write_reported_count`] answers all three with one
    /// [`DomainError::Forbidden`], for the reason this paragraph used to give
    /// as the reason for returning that error directly — all three mean the
    /// same thing to a caller (the request may not write under the tenant it
    /// claims), and a response that distinguished them would hand an attacker
    /// a free oracle for which guess was closer. What the typed return adds is
    /// a classification for the *operator*, on the metric only; see
    /// [`SignatureRefusal`]'s own header.
    fn verify_signature(
        &self,
        repo_id: Uuid,
        branch: &str,
        tenant_id: Uuid,
        signature: &str,
    ) -> Result<(), SignatureRefusal> {
        if !signing_secret_is_configured(&self.collect_report_signing_secret.0) {
            return Err(SignatureRefusal::SecretUnconfigured);
        }
        let Ok(tag) = hex::decode(signature) else {
            return Err(SignatureRefusal::Malformed);
        };
        let key = derive_signing_key(&self.collect_report_signing_secret.0, tenant_id);
        hmac::verify(
            &key,
            signing_payload(repo_id, branch, tenant_id).as_bytes(),
            &tag,
        )
        .map_err(|_| SignatureRefusal::Mismatch)
    }
}

/// Which of [`CollectService::verify_signature`]'s three refusal paths a
/// report took.
///
/// # This exists because the response deliberately cannot say
///
/// All three become one [`DomainError::Forbidden`] at
/// [`CollectService::write_reported_count`], which is that method's whole
/// point: a response that distinguished them would hand an attacker a free
/// oracle for which guess was closer. The distinction is real and an operator
/// needs it — a deployment with no secret configured and a deployment under a
/// guessing attack are opposite incidents with opposite fixes — so it is
/// carried in this type, out of the request path and into the metric, and
/// nowhere else.
///
/// **Crate-private on purpose.** Nothing outside this crate has a use for it,
/// and its whole reason for existing is that this information must not leave
/// the process through the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignatureRefusal {
    /// `collect_report_signing_secret` is absent, or shorter than
    /// [`MIN_SIGNING_SECRET_LEN`] once trimmed. Fail-closed: this refuses
    /// every report, including a correctly computed one.
    SecretUnconfigured,
    /// The `sig` parameter is not hex, so there is no tag to compare.
    Malformed,
    /// The tag decoded and does not match this deployment's secret over the
    /// `(repo_id, branch, tenant_id)` triple presented.
    Mismatch,
}

/// The metric label for one refusal.
///
/// **Here rather than in `domain::ports::metrics`** — beside the enum it
/// projects, following the placement qa-runs' Task 36 review settled on for
/// the same shape. Two reasons beyond the dependency arrow: [`SignatureRefusal`]
/// is crate-private, so an impl written in the ports module would attach a
/// crate-private type to a public one, which is neither usable nor
/// documentable outside the crate from the module that advertises it; and the
/// "a fourth refusal path is a compile error here" guarantee only helps if it
/// is where the author adding one is already looking.
impl From<SignatureRefusal> for CollectReportOutcome {
    fn from(refusal: SignatureRefusal) -> Self {
        match refusal {
            SignatureRefusal::SecretUnconfigured => Self::SecretUnconfigured,
            SignatureRefusal::Malformed => Self::SignatureMalformed,
            SignatureRefusal::Mismatch => Self::SignatureInvalid,
        }
    }
}

/// The domain-separation label for [`derive_signing_key`]'s HKDF salt.
///
/// Public/non-secret by construction — RFC 5869 §3.1 salt never needs to be
/// secret — and fixed rather than random because there is no per-deployment
/// random value available to store or transmit here that `collect_report_
/// signing_secret` does not already provide; a fixed, versioned label is the
/// standard fallback recommended when no such value exists (`hkdf::Salt`'s
/// own doc says the same: use a real salt "when you can", else document why
/// not). `collect_report_signing_secret` supplies the extract step's actual
/// entropy (input keying material, the secret half of HKDF-Extract) — this
/// constant's only job is binding the derivation to *this* call site, so a
/// future second HKDF use elsewhere in this crate over the same or a
/// different root secret cannot collide with this one's output by accident.
/// The trailing `v1` is deliberate: changing this constant changes every
/// derived key at once, the same "never do this without a real reason"
/// warning `signing_payload`'s format carries.
const COLLECT_SIGNING_HKDF_SALT: &[u8] = b"qa-insights/collect-report-signing/v1";

/// Task 7: HKDF-derive tenant `tenant_id`'s own HMAC-SHA256 key from the one
/// root `collect_report_signing_secret` a deployment configures, so that
/// secret is no longer the HMAC key every tenant's tag is computed under.
///
/// # Why this closes the single-point-of-compromise the task names
///
/// Before this task, `collect_report_signing_secret` itself was the HMAC
/// key, for every tenant. Whoever held that one string could sign a tag for
/// *any* `(repo_id, branch, tenant_id)` triple, because `tenant_id` inside
/// the signed tuple ([`signing_payload`]) is chosen by whoever signs, not
/// authenticated by anything outside the tag itself. After this task, the
/// same leak (this one root secret) still lets an attacker derive tenant
/// `T`'s key for a `T` of their choosing — the root secret remains the one
/// thing a deployment must protect — but that is unavoidable for *any*
/// scheme that derives every tenant's key from a single configured value
/// without an out-of-band per-tenant secret store, which this task's brief
/// does not ask for (the config surface stays one value). What the
/// derivation actually buys, stated precisely: a root secret is a
/// deployment-wide operational credential an operator types into
/// configuration once; a *derived* tenant key is not that same credential,
/// so nothing downstream of this function — a log line, a metrics label, an
/// error payload, a future feature that hands a caller "their" signing
/// material for some legitimate reason this task cannot foresee — can leak a
/// value that, held alone, works for a different tenant, the way the un-derived
/// root secret itself would.
///
/// # The construction
///
/// `PRK = HKDF-Extract(salt = `[`COLLECT_SIGNING_HKDF_SALT`]`, ikm =
/// root_secret)`, then `OKM = HKDF-Expand(PRK, info = tenant_id's 16 raw
/// bytes, len = HMAC-SHA256's key length)`, both HMAC-SHA256-based
/// (`hkdf::HKDF_SHA256`) — the same primitive [`CollectService::sign`] and
/// [`CollectService::verify_signature`] already used directly, so this adds
/// no new hash algorithm, only a key-derivation step in front of the
/// existing HMAC. `tenant_id.as_bytes()` is `Uuid`'s fixed 16-byte
/// representation — fixed-width, so unlike a delimited string it needs no
/// [`signing_payload`]-style delimiter-safety argument: `expand`'s own doc
/// warns that concatenated variable-length `info` fields can collide across
/// distinct inputs, which does not apply to a single fixed-length field.
///
/// `aws_lc_rs::hmac::Key: From<hkdf::Okm<'_, hkdf::Algorithm>>` (the crate's
/// own documented pattern for "derive a key, then use it as an HMAC key" —
/// see `hkdf`'s module-level example) hands the OKM straight to `hmac::sign`
/// / `hmac::verify` without this function ever materialising the raw key
/// bytes itself.
///
/// # Why `aws_lc_rs::hkdf`, not `hkdf`/`ring`
///
/// Same reasoning as `hmac` above it: Dylint `DE0708` bans a new
/// non-allow-listed `sha2` import, and `aws-lc-rs` is this workspace's
/// mandated FIPS-validated primitive for this shape of problem
/// (`Cargo.toml`'s own comment on the `aws-lc-rs` dependency). Reaching for
/// the `hkdf` or `ring` crates instead would both reintroduce a
/// non-FIPS-validated implementation and add a dependency this workspace has
/// already decided it does not want for exactly this problem.
///
/// # Fail-closed is unaffected
///
/// This function is never the thing that decides whether a report is
/// accepted or refused — [`CollectService::verify_signature`] still checks
/// [`signing_secret_is_configured`] first and returns
/// [`SignatureRefusal::SecretUnconfigured`] before this function, or
/// `aws_lc_rs`, is ever reached. An empty or too-short root secret still
/// disables the public collect route exactly as before; this function only
/// changes *what key* a properly configured secret's HMAC runs under.
///
/// # Constant-time verification is unaffected
///
/// `hmac::verify`'s constant-time comparison (this module's header) is
/// unchanged: it still compares two 32-byte HMAC-SHA256 tags. Deriving the
/// key first does not touch that comparison, and HKDF-Expand's own
/// computation is a fixed, input-length-independent sequence of HMAC calls
/// with no data-dependent branching over secret material, so it introduces
/// no new timing channel over `tenant_id` (which is not secret; it already
/// travels in the clear as a `Path`/`Query` value) or over the root secret
/// (whose value never varies per call in a way an attacker could probe one
/// bit at a time through this function's timing, since it is the same
/// literal bytes on every call this process makes).
#[allow(
    clippy::expect_used,
    reason = "the requested OKM length is HMAC-SHA256's own fixed digest length (32 bytes), \
              never more than the 255x-digest-length cap `Prk::expand` enforces, so this can \
              never fail for this call site -- matching encode_collect_report_query's own \
              reason above for why a panic that can never trigger beats a silent fallback."
)]
fn derive_signing_key(root_secret: &str, tenant_id: Uuid) -> hmac::Key {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, COLLECT_SIGNING_HKDF_SALT);
    let prk = salt.extract(root_secret.as_bytes());
    // A named binding, not an inline `&[...]` literal: `Okm` borrows this
    // slice for its own lifetime, and an unnamed temporary array would be
    // dropped at the end of the `let okm = ...` statement, before
    // `hmac::Key::from(okm)` below gets to use it (E0716).
    let info: [&[u8]; 1] = [tenant_id.as_bytes().as_slice()];
    let okm = prk
        .expand(&info, hkdf::HKDF_SHA256.hmac_algorithm())
        .expect(
            "HKDF-Expand's requested length is HMAC-SHA256's own fixed digest \
             length (32 bytes), never more than the 255x-digest-length cap \
             `expand` enforces, so this can never fail for this call site",
        );
    hmac::Key::from(okm)
}

/// The string [`CollectService::sign`] and [`CollectService::verify_signature`]
/// both compute the HMAC over. See this module's header, "Delimiter safety of
/// the signed string", for why joining with `|` cannot let one `branch` value
/// stand in for a different `(repo_id, branch, tenant_id)` triple.
///
/// **`tenant_id` here is redundant once [`derive_signing_key`] exists — kept
/// anyway.** The key is now tenant-specific, so a tag no longer needs
/// `tenant_id` inside the signed payload to bind the tag to a tenant; the
/// key already does that. Two reasons this format is unchanged regardless:
///
/// * **Changing it invalidates every signature in flight for no gain.** Any
///   callback URL [`CollectService::collect_url`] has already handed to a
///   running qa-runs workflow carries a `sig` computed over today's payload
///   shape; that workflow can report against it for as long as it runs (this
///   module's own doc on [`CollectService::verify_signature`] — no expiry, no
///   nonce). Dropping `tenant_id` from the payload changes what every
///   in-flight tag is a tag *of*, so every one of them would stop verifying
///   the moment this shipped, with no corresponding security improvement:
///   the field's only remaining job is defense in depth (below), not the
///   thing that makes cross-tenant forgery hard.
/// * **A second, narrower binding, kept for defense in depth.** With
///   `tenant_id` still in the payload, a bug that derived the *same* key for
///   two different tenants (a hypothetical [`derive_signing_key`] regression,
///   not a property of today's construction) would still produce different
///   tags for those two tenants, because the HMAC input would differ even
///   under a shared key. That is a strictly weaker property than the key
///   derivation itself provides today, but it costs nothing to keep — the
///   field is already there, already typed, already covered by the
///   delimiter-safety argument below — so there is no reason to spend a
///   format change removing it.
fn signing_payload(repo_id: Uuid, branch: &str, tenant_id: Uuid) -> String {
    format!("{repo_id}|{branch}|{tenant_id}")
}

#[cfg(test)]
#[path = "collect_tests.rs"]
mod tests;
