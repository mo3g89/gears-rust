use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{CreateSshKeyReq, SshKeyDto};
use crate::gear::ConcreteAppServices;

/// List SSH key metadata visible to the caller (never the material).
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_ssh_keys(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<Vec<SshKeyDto>>> {
    let keys = svc.ssh_keys.list_ssh_keys(&ctx).await?;
    Ok(Json(keys.into_iter().map(SshKeyDto::from).collect()))
}

/// Create an SSH key: the PEM material goes to credstore immediately; the
/// response carries `id`/`name`/`fingerprint`/`created_at` only — NEVER the PEM, and
/// never the credstore reference either (publishing the reference would hand
/// every tenant member a working read path to the material; see
/// `dto::SshKeyDto`).
#[tracing::instrument(skip(svc, ctx, req), fields(ssh_key.name = %req.name))]
pub async fn create_ssh_key(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<CreateSshKeyReq>,
) -> ApiResult<impl IntoResponse> {
    let key = svc
        .ssh_keys
        .create_ssh_key(&ctx, req.name, req.private_key_pem)
        .await?;
    let id_str = key.id.to_string();
    Ok(created_json(SshKeyDto::from(key), &uri, &id_str).into_response())
}

/// Delete an SSH key (removes the credstore secret first, then the row).
#[tracing::instrument(skip(svc, ctx), fields(ssh_key.id = %id))]
pub async fn delete_ssh_key(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.ssh_keys.delete_ssh_key(&ctx, id).await?;
    Ok(no_content().into_response())
}
