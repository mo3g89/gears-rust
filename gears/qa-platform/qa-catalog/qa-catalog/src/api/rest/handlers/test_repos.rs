use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::Uri;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{
    BranchListDto, CreateTestRepoReq, SyncTestRepoQuery, TestRepositoryDto, UpdateTestRepoReq,
};
use crate::gear::ConcreteAppServices;

/// List all test repositories visible to the caller.
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_test_repos(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<Vec<TestRepositoryDto>>> {
    let repos = svc.repos.list_repos(&ctx).await?;
    Ok(Json(
        repos.into_iter().map(TestRepositoryDto::from).collect(),
    ))
}

/// Get a single test repository by ID.
#[tracing::instrument(skip(svc, ctx), fields(repo.id = %id))]
pub async fn get_test_repo(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<TestRepositoryDto>> {
    Ok(Json(svc.repos.get_repo(&ctx, id).await?.into()))
}

/// Register a new test repository.
#[tracing::instrument(skip(svc, ctx, req), fields(repo.name = %req.name))]
pub async fn create_test_repo(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<CreateTestRepoReq>,
) -> ApiResult<impl IntoResponse> {
    let repo = svc.repos.create_repo(&ctx, req.into()).await?;
    let id_str = repo.id.to_string();
    Ok(created_json(TestRepositoryDto::from(repo), &uri, &id_str).into_response())
}

/// Replace a test repository's mutable fields (`default_branch` included);
/// changing `url`/`content_root` clears the synced state and working area
/// (see `ReposService::update_repo`).
#[tracing::instrument(skip(svc, ctx, req), fields(repo.id = %id, repo.name = %req.name))]
pub async fn update_test_repo(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTestRepoReq>,
) -> ApiResult<Json<TestRepositoryDto>> {
    Ok(Json(
        svc.repos.update_repo(&ctx, id, req.into()).await?.into(),
    ))
}

/// Delete a test repository (cascades to its cached branches; the synced
/// working copy is cleaned up best-effort).
#[tracing::instrument(skip(svc, ctx), fields(repo.id = %id))]
pub async fn delete_test_repo(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.repos.delete_repo(&ctx, id).await?;
    Ok(no_content().into_response())
}

/// Sync the requested branch now (`branch` absent or empty = the
/// repository's default branch). An operator hitting this endpoint means
/// "fetch now", so the freshness cache is forced past. Returns the updated
/// repository whether the sync succeeded or failed — an engine failure is
/// recorded in `sync_error`, not surfaced as an HTTP error (SDK contract:
/// "returns when the sync completes or fails").
#[tracing::instrument(skip(svc, ctx, query), fields(repo.id = %id, branch = %query.branch))]
pub async fn sync_test_repo(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Query(query): Query<SyncTestRepoQuery>,
) -> ApiResult<Json<TestRepositoryDto>> {
    Ok(Json(
        svc.repos
            .sync_repo(&ctx, id, &query.branch, true)
            .await?
            .into(),
    ))
}

/// Cached branch names for the repository (populated by sync and the
/// branch-cache lifecycle task; never reads the git remote).
#[tracing::instrument(skip(svc, ctx), fields(repo.id = %id))]
pub async fn list_test_repo_branches(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BranchListDto>> {
    let branches = svc.repos.list_branches(&ctx, id).await?;
    Ok(Json(BranchListDto { branches }))
}
