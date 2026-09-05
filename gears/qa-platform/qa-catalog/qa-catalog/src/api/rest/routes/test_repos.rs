use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Catalog";

pub(super) fn register_test_repo_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/test-repos - List test repositories
    router = OperationBuilder::get("/qa/v1/test-repos")
        .operation_id("qa_catalog.list_test_repos")
        .summary("List test repositories")
        .description("Retrieve all registered test repositories visible to the caller")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_test_repos)
        .json_array_response_with_schema::<dto::TestRepositoryDto>(
            openapi,
            StatusCode::OK,
            "List of test repositories",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/test-repos - Register a new test repository
    router = OperationBuilder::post("/qa/v1/test-repos")
        .operation_id("qa_catalog.create_test_repo")
        .summary("Register a new test repository")
        .description(
            "Register a git test repository (https/http remote; credentials \
             via a credstore reference only)",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateTestRepoReq>(openapi, "Repository registration data")
        .handler(handlers::create_test_repo)
        .json_response_with_schema::<dto::TestRepositoryDto>(
            openapi,
            StatusCode::CREATED,
            "Created test repository",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/test-repos/{id} - Get a test repository
    router = OperationBuilder::get("/qa/v1/test-repos/{id}")
        .operation_id("qa_catalog.get_test_repo")
        .summary("Get a test repository")
        .description("Retrieve a specific test repository by UUID")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Repository UUID")
        .handler(handlers::get_test_repo)
        .json_response_with_schema::<dto::TestRepositoryDto>(
            openapi,
            StatusCode::OK,
            "Test repository found",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/test-repos/{id} - Update a test repository
    router = OperationBuilder::put("/qa/v1/test-repos/{id}")
        .operation_id("qa_catalog.update_test_repo")
        .summary("Update a test repository")
        .description(
            "Replace a repository's mutable fields (name, url, default_branch, \
             content_root, credential reference). Changing `url` or \
             `content_root` clears the synced state and the working area, so \
             content reads report the repository as not synced until the next \
             sync",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Repository UUID")
        .json_request::<dto::UpdateTestRepoReq>(openapi, "Repository update data")
        .handler(handlers::update_test_repo)
        .json_response_with_schema::<dto::TestRepositoryDto>(
            openapi,
            StatusCode::OK,
            "Updated test repository",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        // A name colliding with another of the tenant's repositories maps to
        // AlreadyExists -> HTTP 409.
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/test-repos/{id} - Delete a test repository
    router = OperationBuilder::delete("/qa/v1/test-repos/{id}")
        .operation_id("qa_catalog.delete_test_repo")
        .summary("Delete a test repository")
        .description("Delete a test repository by UUID (cascades to its cached branches)")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Repository UUID")
        .handler(handlers::delete_test_repo)
        // 204 carries no body — `no_content_response` omits the `content`
        // block a `json_response` would wrongly advertise.
        .no_content_response(
            StatusCode::NO_CONTENT,
            "Test repository deleted successfully",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/test-repos/{id}/sync - Sync now
    router = OperationBuilder::post("/qa/v1/test-repos/{id}/sync")
        .operation_id("qa_catalog.sync_test_repo")
        .summary("Sync a test repository now")
        .description(
            "Clone-or-fetch the requested branch (the repository's default \
             branch when none is named) into its snapshot and refresh the \
             branch cache, forcing past the freshness cache; an engine \
             failure is recorded in `sync_error` on the returned repository \
             rather than failing the request",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Repository UUID")
        .query_param_typed(
            "branch",
            false,
            "Branch to sync (defaults to the repository's default branch)",
            "string",
        )
        .handler(handlers::sync_test_repo)
        .json_response_with_schema::<dto::TestRepositoryDto>(
            openapi,
            StatusCode::OK,
            "Repository after the sync attempt",
        )
        // Validation of a malformed credential_ref maps to invalid_argument.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        // BranchCacheConflict (concurrent sync) maps to Aborted → HTTP 409.
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/test-repos/{id}/branches - Cached branches
    router = OperationBuilder::get("/qa/v1/test-repos/{id}/branches")
        .operation_id("qa_catalog.list_test_repo_branches")
        .summary("List a repository's cached branches")
        .description(
            "Cached branch names (populated by sync and the branch-cache \
             refresher); never contacts the git remote",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Repository UUID")
        .handler(handlers::list_test_repo_branches)
        .json_response_with_schema::<dto::BranchListDto>(
            openapi,
            StatusCode::OK,
            "Cached branch names",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
