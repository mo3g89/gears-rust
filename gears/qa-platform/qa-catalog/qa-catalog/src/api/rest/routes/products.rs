use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Catalog";

pub(super) fn register_product_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/products - List products
    router = OperationBuilder::get("/qa/v1/products")
        .operation_id("qa_catalog.list_products")
        .summary("List products")
        .description("Retrieve all products visible to the caller")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_products)
        .json_array_response_with_schema::<dto::ProductDto>(
            openapi,
            StatusCode::OK,
            "List of products",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/products - Create a product
    router = OperationBuilder::post("/qa/v1/products")
        .operation_id("qa_catalog.create_product")
        .summary("Create a product")
        .description(
            "Create a product with a name, key, and description, optionally placed in a folder",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateProductReq>(openapi, "Product creation data")
        .handler(handlers::create_product)
        .json_response_with_schema::<dto::ProductDto>(
            openapi,
            StatusCode::CREATED,
            "Created product",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/products/{id} - Update a product
    router = OperationBuilder::put("/qa/v1/products/{id}")
        .operation_id("qa_catalog.update_product")
        .summary("Update a product")
        .description(
            "Replace a product's name, key, description, and folder (an absent \
             folder moves it to the root)",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Product UUID")
        .json_request::<dto::UpdateProductReq>(openapi, "Product update data")
        .handler(handlers::update_product)
        .json_response_with_schema::<dto::ProductDto>(openapi, StatusCode::OK, "Updated product")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        // A name colliding with another of the tenant's products maps to
        // AlreadyExists -> HTTP 409.
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/products/{id} - Delete a product
    router = OperationBuilder::delete("/qa/v1/products/{id}")
        .operation_id("qa_catalog.delete_product")
        .summary("Delete a product")
        .description("Delete a product by UUID")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Product UUID")
        .handler(handlers::delete_product)
        // 204 carries no body — see `routes::test_repos`.
        .no_content_response(StatusCode::NO_CONTENT, "Product deleted successfully")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/product-folders - Distinct folder names
    router = OperationBuilder::get("/qa/v1/product-folders")
        .operation_id("qa_catalog.list_product_folders")
        .summary("List product folders")
        .description("Distinct non-null folder names across the caller's visible products")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_product_folders)
        .json_response_with_schema::<dto::ProductFolderListDto>(
            openapi,
            StatusCode::OK,
            "Distinct product folder names",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
