use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Catalog";

pub(super) fn register_ssh_key_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/ssh-keys - List SSH key metadata
    router = OperationBuilder::get("/qa/v1/ssh-keys")
        .operation_id("qa_catalog.list_ssh_keys")
        .summary("List SSH keys")
        .description(
            "SSH key metadata (id, name, fingerprint, creation time) visible to \
             the caller. Never the key material, and never its credstore \
             reference either (the reference is itself a read path to the \
             material)",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_ssh_keys)
        .json_array_response_with_schema::<dto::SshKeyDto>(
            openapi,
            StatusCode::OK,
            "List of SSH key metadata",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/ssh-keys - Create an SSH key
    router = OperationBuilder::post("/qa/v1/ssh-keys")
        .operation_id("qa_catalog.create_ssh_key")
        .summary("Create an SSH key")
        .description(
            "Store the private key material in credstore and persist metadata \
             only; the response never echoes the PEM",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateSshKeyReq>(openapi, "SSH key name and PEM material")
        .handler(handlers::create_ssh_key)
        .json_response_with_schema::<dto::SshKeyDto>(
            openapi,
            StatusCode::CREATED,
            "Created SSH key metadata (no material)",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/ssh-keys/{id} - Delete an SSH key
    router = OperationBuilder::delete("/qa/v1/ssh-keys/{id}")
        .operation_id("qa_catalog.delete_ssh_key")
        .summary("Delete an SSH key")
        .description("Delete an SSH key: the credstore secret first, then the metadata row")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "SSH key UUID")
        .handler(handlers::delete_ssh_key)
        // 204 carries no body — see `routes::test_repos`.
        .no_content_response(StatusCode::NO_CONTENT, "SSH key deleted successfully")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
