//! The collect trigger and the runner's report — `POST
//! /qa/v1/analytics/collect` and `POST /qa/v1/collect/{repo_id}`. Task 30.
//!
//! # `POST /qa/v1/collect/{repo_id}` is registered `.public()`, and it is
//! # genuinely, anonymously reachable
//!
//! Every other route this crate registers is `.authenticated()`. This one is
//! meant to be reachable without a browser session — its caller is the test
//! runner reporting back to a URL this gear itself built — and, traced
//! against the host that actually serves `qa_insights` (an in-process host,
//! `apps/cf-gears-example-server`, fronted by `api-gateway`), `.public()`
//! **does** deliver that: `api-gateway`'s own auth middleware resolves a
//! `.public()`-registered route to `AuthRequirement::None` and runs the
//! handler with `SecurityContext::anonymous()`, no bearer token checked at
//! all. See `domain::service::collect`'s header ("`.public()` genuinely
//! exempts this route") and `crate::domain::system_actor`'s (the section on
//! `for_collect_report`) for the finding in full, including why an earlier
//! revision of this file concluded the opposite from premises that were each
//! true but described a hosting shape this gear does not use.
//!
//! **Because this route is anonymously reachable, the `sig` query
//! parameter is the actual access control, not a defence behind a platform
//! fix that has yet to land.** It is an HMAC-SHA256 tag over `(repo_id,
//! branch, tenant_id)` that
//! `domain::service::collect::CollectService::record_count` verifies before
//! doing anything else. See that module's header, "Fix round 1, Critical 1",
//! for why an embedded `tenant_id` alone was a cross-tenant write and how the
//! signature closes it, and "What the tag does and does not cover" for the
//! replay and unsigned-field scope this signature does *not* protect.
//!
//! # `branch` is a query parameter here, not `{repo_id}/{branch}`
//!
//! Legacy's route shape is `/api/collect/{repo_id}/{branch}`. This gear's is
//! not, because a real branch name routinely contains `/` and a `/` inside a
//! single path segment is a router 404 — exactly the defect Task 27 shipped
//! and had to fix for `plan_id`. `domain::service::collect`'s header ("The
//! branch-in-path hazard") is the full argument; the summary this route's
//! description quotes is that this gear controls both ends of this
//! particular URL, so a query parameter needs no percent-encoding convention
//! at all for the one character (`/`) that would otherwise 404.

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};

pub(super) fn register_collect_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // POST /qa/v1/analytics/collect
    router = OperationBuilder::post("/qa/v1/analytics/collect")
        .operation_id("qa_insights.trigger_collect")
        .summary("Launch the collect job on demand")
        .description(
            "Launch a collect-only workflow for the given branch (or this deployment's default \
             branch, when none is given) against every repository the caller's universe \
             admits. A collect workflow enumerates exact test-case counts (pytest \
             --collect-only, parametrize expanded) and reports them back, which is what lets \
             the analytics overview's expected-cases number reflect parametrized tests exactly \
             rather than as a static per-file estimate. A repository that fails to launch is \
             skipped, not fatal to the request. launched counts launch calls this deployment's \
             qa-runs accepted, not repositories confirmed to be collecting - branch existence is \
             validated by qa-runs asynchronously, after this endpoint has already answered, so a \
             repository lacking the requested branch can still be counted here and separately \
             fail to ever report back. Requires the gts.cf.qa.insights.test_result.v1~/collect grant, and that \
             grant's compiled scope must constrain owner_tenant_id only - the same constraint \
             POST /qa/v1/insights/rebuild requires and for the identical reason: this operation \
             addresses no single row a narrower scope could express.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param(
            "branch",
            false,
            "Branch to collect. Blank and absent both fall back to this deployment's default \
             collect branch.",
        )
        .handler(handlers::trigger_collect)
        .json_response_with_schema::<dto::CollectTriggerOutcomeDto>(
            openapi,
            StatusCode::OK,
            "How many launch calls were accepted, and the branch actually collected",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/collect/{repo_id} — the runner's callback target. See this
    // module's header for why `.public()` is intent rather than enforcement
    // today, why `branch` is a query parameter rather than a second path
    // segment, and what `sig` actually protects in the meantime.
    OperationBuilder::post("/qa/v1/collect/{repo_id}")
        .operation_id("qa_insights.report_collect_count")
        .summary("Report an exact per-file test-case count")
        .description(
            "Called by the test runner in collect-only mode (VHP_COLLECT_URL). branch, \
             tenant_id and sig are all required query parameters: branch is the branch that \
             was collected; tenant_id and sig are values this gear itself chose when it \
             launched the workflow, echoed back rather than asserted by the caller - sig is an \
             HMAC-SHA256 tag over (repo_id, branch, tenant_id) under this deployment's signing \
             secret, and the request is refused with 403 if it does not verify (including when \
             this deployment has no signing secret configured, which fails every report \
             closed rather than accepting one under a well-known empty key). Upserts the exact \
             case count for one file, keyed on (repository, branch, file); a repeated report \
             for the same key replaces the count, it does not accumulate. A negative \
             case_count clamps to zero rather than being rejected. Both branch and test_file \
             (the latter after path normalization) must be non-blank, or the request is a 400.",
        )
        .tag(API_TAG)
        // `.public()` was split into two axes upstream and is now a deprecated
        // alias forwarding to exactly this pair; the registration is unchanged.
        .anonymous()
        .exposed()
        .path_param("repo_id", "The repository this count belongs to")
        .query_param(
            "branch",
            true,
            "The branch that was collected. Must be non-blank.",
        )
        .query_param(
            "tenant_id",
            true,
            "This gear's own choice, echoed back - not the caller's claim on its own; see sig.",
        )
        .query_param(
            "sig",
            true,
            "HMAC-SHA256 over (repo_id, branch, tenant_id), hex-encoded. This gear's own choice; \
             a report whose sig does not verify is refused with 403.",
        )
        .json_request::<dto::CollectCountReq>(openapi, "One file's exact case count")
        .handler(handlers::report_collect_count)
        .no_content_response(StatusCode::OK, "The count was recorded")
        // 404 is deliberately absent: legacy's handler answers only OK or
        // BAD_REQUEST or INTERNAL_SERVER_ERROR (`analytics.rs:2635-2640`) -
        // an unknown repo_id is stored, not rejected, exactly as legacy's
        // own `INSERT` never checks whether the repository exists (there is
        // no foreign key to check against; `domain::repos`' header states
        // why this schema has none).
        .error_400(openapi)
        // 403: a signature that does not verify, or an unconfigured signing
        // secret (fail-closed) - fix round 1, Critical 1. An ABSENT `sig` is
        // NOT among them and this comment used to say it was: the field is a
        // required `String` on `dto::CollectReportQuery`, so a request
        // carrying no `sig` is refused by the extractor as a 400 before the
        // handler is entered - the same thing the bundle-download route
        // turned out to do (see qa-catalog's routes/bundles.rs). `.error_400`
        // above already declares that status; only the prose was wrong.
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
