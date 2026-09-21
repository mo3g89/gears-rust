//! The test-bundle download route.
//!
//! # `GET /qa/v1/test-bundles/{id}` is registered `.anonymous().exposed()`,
//! # and the `sig` query parameter is the actual access control
//!
//! Every other route this crate registers is `.authenticated()`. This one is
//! reachable without a session on purpose, because its caller is an Argo
//! workflow pod fetching the test content it is about to execute, and that pod
//! has no user to borrow a session from.
//!
//! **It used to solve that with a credential, and that is what this shape
//! replaced.** The pod carried `TEST_BUNDLE_CLIENT_SECRET` — the confidential
//! secret of a `fullScopeAllowed` Keycloak service-account client with a
//! `tenant_id` claim hardcoded to one tenant — and performed a
//! `client_credentials` exchange inside the pod before calling this route. Two
//! consequences, both measured rather than theorised:
//!
//! * The pod's whole job is to execute **tenant-authored pytest**, so that
//!   deployment-wide, unexpiring credential was readable by tenant code, which
//!   could then call every `.authenticated()` route in all four gears for that
//!   tenant. The runner `NetworkPolicy` did not mitigate it — it allow-listed
//!   both destinations the credential needed.
//! * Because the claim was hardcoded, **every tenant but the seeded one 404'd
//!   on its own bundles.** A live multi-tenancy defect, not merely a risk.
//!
//! What replaces it is an HMAC-SHA256 tag over `(bundle_id, tenant_id)`,
//! hex-encoded, minted by `BundlesService::create_bundle` and verified by
//! `BundlesService::get_bundle_content_signed` before anything else happens.
//! It authorises exactly one bundle for exactly as long as that bundle's row
//! lives, it carries no expiry of its own, and nothing stores it. The tenant it
//! verifies under is read off the descriptor row, never asserted by the caller.
//!
//! This is the same shape, for the same reason, as qa-insights'
//! `POST /qa/v1/collect/{repo_id}` — see that module's header.
//!
//! # Why `.anonymous()` genuinely exempts this route on the address the runner
//! # dials
//!
//! `api-gateway`'s auth middleware resolves an `AuthRequirement::None` route by
//! inserting `SecurityContext::anonymous()` and running the handler **without
//! reading the `Authorization` header at all**
//! (`gears/system/api-gateway/src/middleware/auth.rs`, the
//! `AuthRequirement::None` arm). The `http` port a workflow pod reaches through
//! `bundle_base_url` is that gateway, so there is no second gate behind this
//! registration and no fallback if the signature check is wrong.

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use crate::api::rest::handlers;

const API_TAG: &str = "QA Catalog";

pub(super) fn register_bundle_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/test-bundles/{id} - Download bundle bytes.
    //
    // Binary (non-JSON) response: `text_response` is the builder's
    // custom-media-type registration (a raw media type with `schema: None`) —
    // the same call file-parser uses for its `text/markdown` stream. The name
    // says "text", but it is media-type agnostic and emits exactly the
    // schema-less `application/gzip` content block this endpoint needs.
    //
    // Bundle creation is SDK-only (qa-runs at launch); see
    // `handlers::download_bundle`.
    //
    // 403 is the signature refusal, merged: a malformed tag, a non-verifying
    // tag and an unconfigured signing secret all answer the same status with
    // the same body, so the response cannot be an oracle for which guess was
    // closer. 401 is gone -- there is no authentication on this route to fail.
    //
    // 400 is the *absent* tag, and it is a different status because a
    // different thing rejects it. `sig` is declared `required: true` and the
    // handler takes `Query<BundleDownloadQuery>` with a non-optional field, so
    // a request with no `sig` at all never reaches
    // `get_bundle_content_signed` -- the extractor refuses it first, with 400.
    // Verified live: a tampered `sig` answers 403, an omitted one answers 400.
    // This is fail-closed and correct, so it is documented rather than
    // changed; merging it into the 403 would mean accepting `Option<String>`
    // and hand-rolling the refusal, which buys an indistinguishable response
    // at the price of letting an unsigned request reach the verifier.
    router = OperationBuilder::get("/qa/v1/test-bundles/{id}")
        .operation_id("qa_catalog.download_bundle")
        .summary("Download a test bundle")
        .description(
            "Download the tar.gz bytes of an ephemeral test bundle. Anonymous: the required \
             sig query parameter is the access control, not a bearer token - it is an \
             HMAC-SHA256 tag over (bundle_id, tenant_id) this gear itself minted when the \
             bundle was built, and the request is refused with 403 if it does not verify. \
             Omitting sig entirely is a 400, not a 403: it is a required parameter, so the \
             request is rejected before the handler runs. An expired bundle reads exactly \
             like a missing one (404).",
        )
        .tag(API_TAG)
        // See this module's header. `.anonymous()` + `.exposed()` is the same
        // pair qa-insights' collect callback uses, and for the same reason: the
        // caller is a workflow pod, and the `sig` query parameter below is what
        // authorises the request.
        .anonymous()
        .exposed()
        // NO `.require_license_features::<License>([])`, and its absence is
        // enforced rather than chosen: `require_license_features` is only
        // available on an `AuthSet`/`LicenseNotSet` builder, and `.anonymous()`
        // transitions straight to `LicenseSet` -- an anonymous route cannot
        // declare a license requirement, because there is no licensed subject
        // to check one against. qa-insights' collect callback omits it for the
        // same structural reason.
        .path_param("id", "Bundle UUID")
        .query_param(
            "sig",
            true,
            "HMAC-SHA256 over (bundle_id, tenant_id), hex-encoded. This gear's own choice, \
             minted when the bundle was built and handed to the runner on TEST_BUNDLE_URL; \
             a download whose sig does not verify is refused with 403, including when this \
             deployment has no bundle_download_signing_secret configured (fail-closed). \
             Omitting it is a 400 rather than a 403 - the parameter is required, so the \
             request never reaches the verification.",
        )
        .handler(handlers::download_bundle)
        .text_response(StatusCode::OK, "Bundle tar.gz bytes", "application/gzip")
        .error_400(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
