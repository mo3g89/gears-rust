use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{CreateEnvironmentReq, EnvironmentDto, LeaseDto, UpdateEnvironmentReq};
use crate::gear::ConcreteAppServices;

/// `GET /qa/v1/environments`
///
/// One page of the target environments visible to the caller, by name.
///
/// The `OData` query is handed to the service untouched; the service resolves
/// the caller's `AccessScope` from the policy enforcer **first** and the
/// repository composes the two. A `$filter` cannot widen what this returns.
/// (Same shape and same reason as `qa-runs`' `list_runs`.)
#[tracing::instrument(skip(svc, ctx, query))]
pub async fn list_environments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<EnvironmentDto>> {
    let page = svc.environments.list_environments(&ctx, &query).await?;
    Ok(Json(page.map_items(EnvironmentDto::from)))
}

/// Get a single target environment by ID.
#[tracing::instrument(skip(svc, ctx), fields(environment.id = %id))]
pub async fn get_environment(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EnvironmentDto>> {
    Ok(Json(
        svc.environments.get_environment(&ctx, id).await?.into(),
    ))
}

/// Register a new target environment.
#[tracing::instrument(skip(svc, ctx, req), fields(environment.name = %req.name))]
pub async fn create_environment(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<CreateEnvironmentReq>,
) -> ApiResult<impl IntoResponse> {
    let environment = svc
        .environments
        .create_environment(&ctx, req.try_into()?)
        .await?;
    let id_str = environment.id.to_string();
    Ok(created_json(EnvironmentDto::from(environment), &uri, &id_str).into_response())
}

/// Partially update a target environment.
#[tracing::instrument(skip(svc, ctx, req), fields(environment.id = %id))]
pub async fn update_environment(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateEnvironmentReq>,
) -> ApiResult<Json<EnvironmentDto>> {
    let environment = svc
        .environments
        .update_environment(&ctx, id, req.into())
        .await?;
    Ok(Json(EnvironmentDto::from(environment)))
}

/// Delete a target environment. Fails with `failed_precondition` if it holds an
/// active lease (enforced by `EnvironmentsService::delete_environment`).
#[tracing::instrument(skip(svc, ctx), fields(environment.id = %id))]
pub async fn delete_environment(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.environments.delete_environment(&ctx, id).await?;
    Ok(no_content().into_response())
}

/// Run one on-demand detection cycle for an environment and return the refreshed
/// row.
///
/// A detection failure (the environment's own cluster unreachable, install
/// metadata missing) is a fact about the environment: `Ok` from
/// `EnvironmentsService::observe_environment`, and this handler answers it with an
/// ordinary 200 body carrying `version_detect_error`. An error response is a
/// fact about the request (`DomainError::EnvironmentNotFound` → 404 via the
/// usual `?`/`CanonicalError` path) or a genuine fault of this service
/// (→ 5xx) — never detection itself failing.
#[tracing::instrument(skip(svc, ctx), fields(environment.id = %id))]
pub async fn refresh_environment(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EnvironmentDto>> {
    let environment = svc.environments.observe_environment(&ctx, id).await?;
    Ok(Json(EnvironmentDto::from(environment)))
}

/// Read-only view of an environment's current lease state.
///
/// Acquire/release are deliberately **not** exposed over REST — they are
/// SDK-only operations for the qa-runs dispatcher (see
/// `qa_environments_sdk::QaEnvironmentsClientV1`). Exposing run-lifecycle
/// lease mutations here would let REST callers bypass qa-runs' queue
/// orchestration.
#[tracing::instrument(skip(svc, ctx), fields(environment.id = %id))]
pub async fn get_environment_lease(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LeaseDto>> {
    Ok(Json(svc.leases.get(&ctx, id).await?.into()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! `refresh_environment` is where the whole "detection failure is a 200,
    //! not a 5xx" contract becomes observable at the HTTP boundary: it is the
    //! one handler in this file whose underlying service method can succeed
    //! while still describing a failure. These tests call the handler
    //! function directly (no router, no transport) — exactly the pattern
    //! `api/rest/error.rs`'s own tests use for asserting `CanonicalError`'s
    //! status-code mapping — because `Ok`/`Err` here already determines the
    //! response family: `Json<_>` renders 200, and `CanonicalError` carries
    //! its own `status_code()`.
    use std::sync::Arc;

    use qa_environments_sdk::NewEnvironment;
    use uuid::Uuid;

    use super::{Extension, Path, refresh_environment};
    use crate::domain::ports::{NoopRunnerSecretWriter, ProductPluginPort};
    use crate::domain::service::actions;
    use crate::gear::ConcreteAppServices;
    use crate::test_support::{
        FixedPluginPort, RecordingAuthZ, RecordingCredStore, SequencedPlugin,
        build_services_tenant_scoped_with_plugin, build_services_with_plugin_port, ctx, inmem_db,
        plugin_detected, plugin_detection_failed,
    };

    fn pasted_environment(name: &str) -> NewEnvironment {
        NewEnvironment {
            credentials: std::collections::BTreeMap::new(),
            name: name.to_owned(),
            // Every environment here names a product: without one no plugin
            // can be resolved, which is its own case in
            // `observation_projection_tests`.
            product_id: Uuid::from_u128(0x9001),
            description: None,
            kubeconfig_credstore_ref: None,
            kubeconfig: Some(qa_environments_sdk::CredentialMaterial::new(
                "apiVersion: v1\nkind: Config\n".to_owned(),
            )),
            default_branch: None,
            is_default: false,
        }
    }

    async fn seed_environment(services: &Arc<ConcreteAppServices>, tenant: Uuid) -> Uuid {
        services
            .environments
            .create_environment(&ctx(tenant), pasted_environment("prod"))
            .await
            .expect("seeding the environment must succeed")
            .id
    }

    /// A product-plugin port whose plugin returns `outcomes` in order, one
    /// per observation.
    fn port_scripted(
        outcomes: impl IntoIterator<Item = qa_product_sdk::observation::PluginObservation>,
    ) -> Arc<dyn ProductPluginPort> {
        Arc::new(FixedPluginPort::new(Arc::new(SequencedPlugin::new(
            outcomes,
        ))))
    }

    /// The brief's own scenario, verbatim: a detection failure must reach the
    /// caller as HTTP 200 (an `Ok` from this handler, which axum renders as
    /// 200 via `Json<_>`'s default), carrying the message in
    /// `version_detect_error`, with the environment's last-known
    /// `observed_version` left exactly as a prior successful detection left
    /// it — never blanked by the failed attempt. 5xx is reserved for a fault
    /// of THIS service; an environment whose cluster cannot be reached is a fact
    /// about the environment, matching legacy's `api_refresh_version`
    /// (`manager/src/routes/platforms.rs:363`).
    #[tokio::test]
    async fn a_detection_failure_is_a_200_carrying_the_reason_not_a_5xx() {
        let tenant = Uuid::new_v4();
        let services = build_services_tenant_scoped_with_plugin(
            inmem_db().await,
            port_scripted([
                plugin_detected("26.5", Some("0")),
                plugin_detection_failed("namespaces \"virtuozzo\" not found"),
            ]),
        );
        let id = seed_environment(&services, tenant).await;

        // Seed a known observed_version via one successful observation first,
        // so the assertion below can tell "left alone" from "coincidentally
        // still None".
        let seeded = refresh_environment(
            Extension(ctx(tenant)),
            Extension(services.clone()),
            Path(id),
        )
        .await
        .expect("the seeding refresh must itself succeed");
        assert_eq!(seeded.0.observed_version.as_deref(), Some("26.5"));

        // The call under test: the scripted observer now reports a failure.
        let result = refresh_environment(
            Extension(ctx(tenant)),
            Extension(services.clone()),
            Path(id),
        )
        .await;

        let dto = result
            .expect("a detection failure must be Ok (HTTP 200), not an Err (mapped to a 5xx)")
            .0;
        assert_eq!(
            dto.version_detect_error.as_deref(),
            Some("the target could not be read: namespaces \"virtuozzo\" not found")
        );
        assert_eq!(
            dto.observed_version.as_deref(),
            Some("26.5"),
            "a failed detection must not blank the last known version"
        );
    }

    /// The other half of the brief's contract: an environment that does not
    /// exist at all is a 404, not folded into the 200-with-error shape above.
    #[tokio::test]
    async fn refreshing_an_unknown_environment_is_404() {
        let tenant = Uuid::new_v4();
        let services =
            build_services_tenant_scoped_with_plugin(inmem_db().await, port_scripted([]));

        let result = refresh_environment(
            Extension(ctx(tenant)),
            Extension(services),
            Path(Uuid::new_v4()),
        )
        .await;

        let err = result.expect_err("refreshing an unknown environment must fail");
        assert_eq!(err.status_code(), 404);
    }

    /// Build a `NewEnvironment` attached to `product`, optionally as its default.
    fn environment_for_product(name: &str, product: Uuid, is_default: bool) -> NewEnvironment {
        NewEnvironment {
            product_id: product,
            is_default,
            ..pasted_environment(name)
        }
    }

    async fn services_for_default_tests() -> Arc<ConcreteAppServices> {
        build_services_with_plugin_port(
            inmem_db().await,
            Arc::new(RecordingAuthZ::new()),
            Arc::new(RecordingCredStore::new()),
            Arc::new(NoopRunnerSecretWriter),
            port_scripted([]),
            crate::config::QaEnvironmentsConfig::default().max_variables,
        )
    }

    /// Promoting an environment demotes the product's previous default.
    ///
    /// This is the whole invariant behind the dialogs' "Default cluster" option: with
    /// two rows flagged, the option resolves arbitrarily, which is the quiet
    /// wrong-environment bug the flag exists to prevent. Asserted against the **real**
    /// repository rather than `MockEnvironmentsRepository`, whose
    /// `clear_default_for_product` is a stub returning 0 — the mock cannot observe row
    /// state, so a test there would pass with the rule deleted.
    #[tokio::test]
    async fn promoting_an_environment_demotes_the_products_previous_default() {
        let tenant = Uuid::new_v4();
        let product = Uuid::new_v4();
        let services = services_for_default_tests().await;

        let first = services
            .environments
            .create_environment(&ctx(tenant), environment_for_product("a", product, true))
            .await
            .expect("the first create must succeed");
        assert!(
            first.is_default,
            "the first environment starts as the default"
        );

        let second = services
            .environments
            .create_environment(&ctx(tenant), environment_for_product("b", product, true))
            .await
            .expect("the second create must succeed");
        assert!(second.is_default);

        let reread_first = services
            .environments
            .get_environment(&ctx(tenant), first.id)
            .await
            .expect("the first environment must still be readable");
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
    async fn promoting_an_environment_leaves_another_products_default_alone() {
        let tenant = Uuid::new_v4();
        let (product_a, product_b) = (Uuid::new_v4(), Uuid::new_v4());
        let services = services_for_default_tests().await;

        let other = services
            .environments
            .create_environment(&ctx(tenant), environment_for_product("a", product_a, true))
            .await
            .expect("create must succeed");
        services
            .environments
            .create_environment(&ctx(tenant), environment_for_product("b", product_b, true))
            .await
            .expect("create must succeed");

        let reread = services
            .environments
            .get_environment(&ctx(tenant), other.id)
            .await
            .expect("the other product's environment must still be readable");
        assert!(
            reread.is_default,
            "a promotion in product B must not touch product A's default"
        );
    }

    /// `observe_environment` persists (`record_observation`) and makes an
    /// outbound call to the environment's own cluster, exactly the shape
    /// `update_environment`/`delete_environment` gate on their own mutating
    /// actions -- so it must authorize with `UPDATE`, not `GET`. A principal
    /// granted only `GET` on an environment must not be able to drive a
    /// live-cluster round-trip and a database write through this endpoint.
    /// Found by review; this test is what stops it silently regressing.
    #[tokio::test]
    async fn refresh_environment_authorizes_with_update_not_get() {
        let tenant = Uuid::new_v4();
        let authz = Arc::new(RecordingAuthZ::new());
        let services = build_services_with_plugin_port(
            inmem_db().await,
            authz.clone(),
            Arc::new(RecordingCredStore::new()),
            Arc::new(NoopRunnerSecretWriter),
            port_scripted([plugin_detected("26.5", Some("0"))]),
            crate::config::QaEnvironmentsConfig::default().max_variables,
        );
        let id = seed_environment(&services, tenant).await;

        let refreshed = refresh_environment(
            Extension(ctx(tenant)),
            Extension(services.clone()),
            Path(id),
        )
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
