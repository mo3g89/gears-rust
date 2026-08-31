use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{CreatePlatformReq, LeaseDto, PlatformDto, UpdatePlatformReq};
use crate::gear::ConcreteAppServices;

/// List all target platforms visible to the caller.
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_platforms(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<Vec<PlatformDto>>> {
    let platforms = svc.platforms.list_platforms(&ctx).await?;
    Ok(Json(platforms.into_iter().map(PlatformDto::from).collect()))
}

/// Get a single target platform by ID.
#[tracing::instrument(skip(svc, ctx), fields(platform.id = %id))]
pub async fn get_platform(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PlatformDto>> {
    Ok(Json(svc.platforms.get_platform(&ctx, id).await?.into()))
}

/// Register a new target platform.
#[tracing::instrument(skip(svc, ctx, req), fields(platform.name = %req.name))]
pub async fn create_platform(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<CreatePlatformReq>,
) -> ApiResult<impl IntoResponse> {
    let platform = svc.platforms.create_platform(&ctx, req.into()).await?;
    let id_str = platform.id.to_string();
    Ok(created_json(PlatformDto::from(platform), &uri, &id_str).into_response())
}

/// Partially update a target platform.
#[tracing::instrument(skip(svc, ctx, req), fields(platform.id = %id))]
pub async fn update_platform(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePlatformReq>,
) -> ApiResult<Json<PlatformDto>> {
    let platform = svc.platforms.update_platform(&ctx, id, req.into()).await?;
    Ok(Json(PlatformDto::from(platform)))
}

/// Delete a target platform. Fails with `failed_precondition` if it holds an
/// active lease (enforced by `PlatformsService::delete_platform`).
#[tracing::instrument(skip(svc, ctx), fields(platform.id = %id))]
pub async fn delete_platform(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.platforms.delete_platform(&ctx, id).await?;
    Ok(no_content().into_response())
}

/// Run one on-demand detection cycle for a platform and return the refreshed
/// row.
///
/// A detection failure (the platform's own cluster unreachable, install
/// metadata missing) is a fact about the platform: `Ok` from
/// `PlatformsService::observe_platform`, and this handler answers it with an
/// ordinary 200 body carrying `version_detect_error`. An error response is a
/// fact about the request (`DomainError::PlatformNotFound` → 404 via the
/// usual `?`/`CanonicalError` path) or a genuine fault of this service
/// (→ 5xx) — never detection itself failing.
#[tracing::instrument(skip(svc, ctx), fields(platform.id = %id))]
pub async fn refresh_platform(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PlatformDto>> {
    let platform = svc.platforms.observe_platform(&ctx, id).await?;
    Ok(Json(PlatformDto::from(platform)))
}

/// Read-only view of a platform's current lease state.
///
/// Acquire/release are deliberately **not** exposed over REST — they are
/// SDK-only operations for the qa-runs dispatcher (see
/// `qa_environments_sdk::QaEnvironmentsClientV1`). Exposing run-lifecycle
/// lease mutations here would let REST callers bypass qa-runs' queue
/// orchestration.
#[tracing::instrument(skip(svc, ctx), fields(platform.id = %id))]
pub async fn get_platform_lease(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LeaseDto>> {
    Ok(Json(svc.leases.get(&ctx, id).await?.into()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! `refresh_platform` is where the whole "detection failure is a 200,
    //! not a 5xx" contract becomes observable at the HTTP boundary: it is the
    //! one handler in this file whose underlying service method can succeed
    //! while still describing a failure. These tests call the handler
    //! function directly (no router, no transport) — exactly the pattern
    //! `api/rest/error.rs`'s own tests use for asserting `CanonicalError`'s
    //! status-code mapping — because `Ok`/`Err` here already determines the
    //! response family: `Json<_>` renders 200, and `CanonicalError` carries
    //! its own `status_code()`.
    use std::sync::Arc;

    use qa_environments_sdk::NewPlatform;
    use uuid::Uuid;

    use super::{Extension, Path, refresh_platform};
    use crate::domain::observation::DetectedPlatform;
    use crate::domain::ports::ObservationOutcome;
    use crate::domain::service::actions;
    use crate::gear::ConcreteAppServices;
    use crate::test_support::{
        RecordingAuthZ, RecordingCredStore, ScriptedObserver, build_services_full,
        build_services_tenant_scoped_with_observer, ctx, inmem_db,
    };

    fn pasted_platform(name: &str) -> NewPlatform {
        NewPlatform {
            name: name.to_owned(),
            product_id: None,
            description: None,
            kubeconfig_credstore_ref: None,
            kubeconfig: Some(qa_environments_sdk::KubeconfigMaterial::new(
                "apiVersion: v1\nkind: Config\n".to_owned(),
            )),
            default_branch: None,
            is_default: false,
        }
    }

    async fn seed_platform(services: &Arc<ConcreteAppServices>, tenant: Uuid) -> Uuid {
        services
            .platforms
            .create_platform(&ctx(tenant), pasted_platform("prod"))
            .await
            .expect("seeding the platform must succeed")
            .id
    }

    fn detected(version: &str, build: Option<&str>) -> ObservationOutcome {
        ObservationOutcome::Detected(DetectedPlatform {
            version: version.to_owned(),
            build: build.map(str::to_owned),
            raw: version.to_owned(),
            namespace: "virtuozzo".to_owned(),
            base_domain: None,
        })
    }

    /// The brief's own scenario, verbatim: a detection failure must reach the
    /// caller as HTTP 200 (an `Ok` from this handler, which axum renders as
    /// 200 via `Json<_>`'s default), carrying the message in
    /// `version_detect_error`, with the platform's last-known
    /// `observed_version` left exactly as a prior successful detection left
    /// it — never blanked by the failed attempt. 5xx is reserved for a fault
    /// of THIS service; a platform whose cluster cannot be reached is a fact
    /// about the platform, matching legacy's `api_refresh_version`
    /// (`manager/src/routes/platforms.rs:363`).
    #[tokio::test]
    async fn a_detection_failure_is_a_200_carrying_the_reason_not_a_5xx() {
        let tenant = Uuid::new_v4();
        let observer = Arc::new(ScriptedObserver::new([
            detected("26.5", Some("0")),
            ObservationOutcome::Failed("namespaces \"virtuozzo\" not found".to_owned()),
        ]));
        let services = build_services_tenant_scoped_with_observer(inmem_db().await, observer);
        let id = seed_platform(&services, tenant).await;

        // Seed a known observed_version via one successful observation first,
        // so the assertion below can tell "left alone" from "coincidentally
        // still None".
        let seeded = refresh_platform(
            Extension(ctx(tenant)),
            Extension(services.clone()),
            Path(id),
        )
        .await
        .expect("the seeding refresh must itself succeed");
        assert_eq!(seeded.0.observed_version.as_deref(), Some("26.5"));

        // The call under test: the scripted observer now reports a failure.
        let result = refresh_platform(Extension(ctx(tenant)), Extension(services.clone()), Path(id)).await;

        let dto = result
            .expect("a detection failure must be Ok (HTTP 200), not an Err (mapped to a 5xx)")
            .0;
        assert_eq!(
            dto.version_detect_error.as_deref(),
            Some("namespaces \"virtuozzo\" not found")
        );
        assert_eq!(
            dto.observed_version.as_deref(),
            Some("26.5"),
            "a failed detection must not blank the last known version"
        );
    }

    /// The other half of the brief's contract: a platform that does not
    /// exist at all is a 404, not folded into the 200-with-error shape above.
    #[tokio::test]
    async fn refreshing_an_unknown_platform_is_404() {
        let tenant = Uuid::new_v4();
        let services = build_services_tenant_scoped_with_observer(
            inmem_db().await,
            Arc::new(ScriptedObserver::new([])),
        );

        let result = refresh_platform(Extension(ctx(tenant)), Extension(services), Path(Uuid::new_v4())).await;

        let err = result.expect_err("refreshing an unknown platform must fail");
        assert_eq!(err.status_code(), 404);
    }

    /// `observe_platform` persists (`record_observation`) and makes an
    /// outbound call to the platform's own cluster, exactly the shape
    /// `update_platform`/`delete_platform` gate on their own mutating
    /// actions -- so it must authorize with `UPDATE`, not `GET`. A principal
    /// granted only `GET` on a platform must not be able to drive a
    /// live-cluster round-trip and a database write through this endpoint.
    /// Found by review; this test is what stops it silently regressing.
    /// Build a `NewPlatform` attached to `product`, optionally as its default.
    fn platform_for_product(name: &str, product: Uuid, is_default: bool) -> NewPlatform {
        NewPlatform {
            product_id: Some(product),
            is_default,
            ..pasted_platform(name)
        }
    }

    async fn services_for_default_tests() -> Arc<ConcreteAppServices> {
        build_services_full(
            inmem_db().await,
            Arc::new(RecordingAuthZ::new()),
            Arc::new(RecordingCredStore::new()),
            Arc::new(ScriptedObserver::new([])),
            crate::config::QaEnvironmentsConfig::default().max_variables,
        )
    }

    /// Promoting a platform demotes the product's previous default.
    ///
    /// This is the whole invariant behind the dialogs' "Default cluster" option: with
    /// two rows flagged, the option resolves arbitrarily, which is the quiet
    /// wrong-environment bug the flag exists to prevent. Asserted against the **real**
    /// repository rather than `MockPlatformsRepository`, whose
    /// `clear_default_for_product` is a stub returning 0 — the mock cannot observe row
    /// state, so a test there would pass with the rule deleted.
    #[tokio::test]
    async fn promoting_a_platform_demotes_the_products_previous_default() {
        let tenant = Uuid::new_v4();
        let product = Uuid::new_v4();
        let services = services_for_default_tests().await;

        let first = services
            .platforms
            .create_platform(&ctx(tenant), platform_for_product("a", product, true))
            .await
            .expect("the first create must succeed");
        assert!(first.is_default, "the first platform starts as the default");

        let second = services
            .platforms
            .create_platform(&ctx(tenant), platform_for_product("b", product, true))
            .await
            .expect("the second create must succeed");
        assert!(second.is_default);

        let reread_first = services
            .platforms
            .get_platform(&ctx(tenant), first.id)
            .await
            .expect("the first platform must still be readable");
        assert!(
            !reread_first.is_default,
            "promoting 'b' must have demoted 'a': two defaults for one product make \
             \"Default cluster\" resolve arbitrarily"
        );
    }

    /// The clear is scoped to the product, not the tenant.
    ///
    /// Without the `product_id` filter every promotion would wipe the defaults of every
    /// other product in the tenant — silently, and only visible the next time somebody
    /// used "Default cluster" on an unrelated product.
    #[tokio::test]
    async fn promoting_a_platform_leaves_another_products_default_alone() {
        let tenant = Uuid::new_v4();
        let (product_a, product_b) = (Uuid::new_v4(), Uuid::new_v4());
        let services = services_for_default_tests().await;

        let other = services
            .platforms
            .create_platform(&ctx(tenant), platform_for_product("a", product_a, true))
            .await
            .expect("create must succeed");
        services
            .platforms
            .create_platform(&ctx(tenant), platform_for_product("b", product_b, true))
            .await
            .expect("create must succeed");

        let reread = services
            .platforms
            .get_platform(&ctx(tenant), other.id)
            .await
            .expect("the other product's platform must still be readable");
        assert!(
            reread.is_default,
            "a promotion in product B must not touch product A's default"
        );
    }

    #[tokio::test]
    async fn refresh_platform_authorizes_with_update_not_get() {
        let tenant = Uuid::new_v4();
        let authz = Arc::new(RecordingAuthZ::new());
        let services = build_services_full(
            inmem_db().await,
            authz.clone(),
            Arc::new(RecordingCredStore::new()),
            Arc::new(ScriptedObserver::new([detected("26.5", Some("0"))])),
            crate::config::QaEnvironmentsConfig::default().max_variables,
        );
        let id = seed_platform(&services, tenant).await;

        let refreshed = refresh_platform(Extension(ctx(tenant)), Extension(services.clone()), Path(id))
            .await
            .expect("the refresh must succeed")
            .0;
        assert_eq!(refreshed.observed_version.as_deref(), Some("26.5"));

        assert!(
            authz.requested(actions::UPDATE, Some(id)),
            "refresh must authorize with UPDATE"
        );
        assert!(
            !authz.requested(actions::GET, Some(id)),
            "refresh must not authorize a mutating, outbound-network-calling \
             operation with the read action"
        );
    }
}
