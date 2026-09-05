//! `POST /qa/v1/analytics/collect` and `POST /qa/v1/collect/{repo_id}` —
//! legacy's `api_collect_trigger` (`manager/src/routes/analytics.rs:2655`) and
//! `api_collect_report` (`:2614`).
//!
//! See `domain::service::collect`'s header for the authorization split these
//! two handlers make visible: [`trigger_collect`] forwards the caller's own
//! `SecurityContext` exactly like every other handler in this crate;
//! [`report_collect_count`] does not have one to forward at all.

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use http::StatusCode;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::dto::{
    CollectCountReq, CollectReportQuery, CollectTriggerOutcomeDto, CollectTriggerQuery,
};
use crate::domain::error::DomainError;
use crate::domain::system_actor::TenantBound;
use crate::gear::ConcreteAppServices;

/// `POST /qa/v1/analytics/collect` — launch the collect job on demand.
///
/// `api_collect_trigger` (`manager/src/routes/analytics.rs:2655-2665`). An
/// ordinary authenticated handler: PDP-checked inside
/// [`crate::domain::service::collect::CollectService::trigger`], exactly like
/// every other write in this crate.
#[tracing::instrument(skip(svc, ctx, query))]
pub async fn trigger_collect(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<CollectTriggerQuery>,
) -> ApiResult<Json<CollectTriggerOutcomeDto>> {
    let (launched, branch) = svc.collect.trigger(&ctx, query.branch.as_deref()).await?;
    Ok(Json(CollectTriggerOutcomeDto { launched, branch }))
}

/// `POST /qa/v1/collect/{repo_id}` — the runner's per-file exact-count
/// report.
///
/// `api_collect_report` (`manager/src/routes/analytics.rs:2614-2642`).
///
/// # No `Extension<SecurityContext>` — not because nothing checks the
/// # caller, but because the check is a signature, not a session
///
/// This route is registered `.public()`
/// (`api::rest::routes::collect::register_collect_routes`) — see that
/// module's header and `domain::service::collect`'s ("`.public()` genuinely
/// exempts this route") for the finding, traced against how this gear is
/// actually hosted, that a request with no `Authorization` header reaches
/// this handler with no rejection: `api-gateway`'s own auth middleware
/// resolves a `.public()` route to an anonymous `SecurityContext` and runs
/// the handler. This handler still never extracts
/// `Extension<SecurityContext>`: there is no *meaningful* session to
/// forward even though one may be present (an anonymous one carries no
/// tenant this write could use), by design, because the caller is the test
/// runner, not a browser.
///
/// The tenant this report writes under comes from `query.tenant_id`, which
/// [`crate::domain::service::collect::CollectService::run_collect_cycle`]
/// itself embedded in this URL when it launched the workflow — but that
/// embedding is **not**, by itself, what makes it trustworthy (fix round 1's
/// Critical 1 finding: it previously was, and — because this route is
/// genuinely anonymously reachable, not merely reachable pending a platform
/// fix — that was a live, exploitable cross-tenant write, not a latent one).
/// What makes it trustworthy is `query.sig`, verified inside
/// [`CollectService::record_count`](crate::domain::service::collect::CollectService::record_count)
/// before the tenant is used for anything — see that method's own doc and
/// `domain::service::collect`'s header in full.
///
/// # The nil-`tenant_id` guard runs *before* signature verification, not
/// # after
///
/// A `tenant_id` of the nil `Uuid` is refused as a
/// [`DomainError::Validation`] rather than silently minting the platform-root
/// actor — [`TenantBound::new`]'s own guard, applied here in the handler.
/// **An earlier revision of this doc claimed the opposite ordering** — that
/// this check ran after `record_count`'s signature verification — which was
/// false: `TenantBound::new` is called on line one of this function's body,
/// before `record_count` (where verification happens) is ever invoked.
/// Corrected rather than reordered, because the ordering has no exploitable
/// consequence to fix: the nil `Uuid` is a public, compile-time constant
/// (`uuid::Uuid::nil()`), not a secret whose presence could tell an attacker
/// anything a "bad signature" response would not already tell them. Moving
/// the check after verification would cost a real property instead —
/// `TenantBound::new`'s guard is what stands between a malformed
/// `tenant_id` and
/// [`for_collect_report`](crate::domain::system_actor::for_collect_report)
/// ever being called at all, and that function's own contract is that it is
/// never given a nil tenant, checked or not.
///
/// # This span records `repo_id` but not `branch`, and does not construct the
/// # system actor itself — Phase B fix wave, Finding 5
///
/// An earlier revision recorded `branch = %query.branch` on this span and
/// built [`for_collect_report`](crate::domain::system_actor::for_collect_report)'s
/// context right here, before calling `record_count` — both **before**
/// `record_count`'s first action, [`CollectService::verify_signature`]
/// (`crate::domain::service::collect`), ever ran. That is the one anonymous
/// route on this crate's whole list: `repo_id` is a `Uuid` axum has already
/// parsed, but `branch` is unbounded attacker-supplied text, recorded on the
/// span verbatim regardless of whether the signature that follows accepts or
/// refuses the request — `handlers::saved_views::list_saved_views`'s own doc
/// gives the identical reasoning for skipping its `query` on an
/// *authenticated* route, which is the less exposed surface of the two. And
/// building the system-actor context here meant `for_collect_report` logged
/// `"qa-insights system actor constructed"` for every request with a
/// non-nil `tenant_id`, including the ones the signature check was about to
/// refuse with `Forbidden` — diluting a log whose entire purpose is
/// attribution. `branch` is simply not recorded here now; `record_count`
/// takes `tenant: TenantBound` and mints the context itself, after its own
/// signature and shape checks succeed — see that method's own doc.
///
/// [`CollectService::verify_signature`]: crate::domain::service::collect::CollectService::verify_signature
#[tracing::instrument(skip(svc, query, body), fields(repo_id = %repo_id))]
pub async fn report_collect_count(
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(repo_id): Path<Uuid>,
    Query(query): Query<CollectReportQuery>,
    Json(body): Json<CollectCountReq>,
) -> ApiResult<StatusCode> {
    let tenant = TenantBound::new(query.tenant_id).ok_or_else(|| DomainError::Validation {
        field: "tenant_id".to_owned(),
        message: "tenant_id must not be nil".to_owned(),
    })?;

    svc.collect
        .record_count(
            tenant,
            repo_id,
            &query.branch,
            &query.sig,
            &body.test_file,
            body.case_count,
        )
        .await?;
    Ok(StatusCode::OK)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! **The one HTTP-boundary-adjacent test this crate has for the
    //! branch-in-a-query-parameter decision.** Task 27's own remedy —
    //! confirmed there byte-identical to axum's `Query` decode path — was
    //! driving `serde_urlencoded` directly over the query DTO; this repeats it
    //! for [`CollectReportQuery`], the type this crate's own decode path
    //! actually uses, and drives a value with a literal `/` through it.

    use uuid::Uuid;

    use super::CollectReportQuery;

    /// A slash-bearing branch — the ordinary case
    /// (`feature/VHP-123-thing`), not the exception this crate's other
    /// fixtures (`main`, `feature-x`) happen to hide — must decode intact
    /// through the exact deserializer axum's `Query` extractor uses.
    #[test]
    fn a_slash_bearing_branch_decodes_intact_through_the_query_string() {
        let tenant = Uuid::from_u128(0x42);
        let raw = format!("branch=feature%2FVHP-123-thing&tenant_id={tenant}&sig=deadbeef");

        let decoded: CollectReportQuery =
            serde_urlencoded::from_str(&raw).expect("a slash-bearing branch must decode");

        assert_eq!(decoded.branch, "feature/VHP-123-thing");
        assert_eq!(decoded.tenant_id, tenant);
        assert_eq!(decoded.sig, "deadbeef");
    }

    /// A literal, **unescaped** `/` in a query value is legal per RFC 3986's
    /// `query` production and must decode identically to the percent-encoded
    /// form above — this is the property that makes the query-parameter shape
    /// strictly easier to get right than the rejected `{repo_id}/{branch}`
    /// path-segment alternative, which cannot express an unescaped `/` at
    /// all.
    #[test]
    fn an_unescaped_slash_in_the_query_value_decodes_the_same_way() {
        let tenant = Uuid::from_u128(0x42);
        let raw = format!("branch=feature/VHP-123-thing&tenant_id={tenant}&sig=deadbeef");

        let decoded: CollectReportQuery =
            serde_urlencoded::from_str(&raw).expect("an unescaped slash must decode");

        assert_eq!(decoded.branch, "feature/VHP-123-thing");
    }
}
