use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Catalog";

pub(super) fn register_product_plugin_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/product-plugins - The product-plugin catalogue
    router = OperationBuilder::get("/qa/v1/product-plugins")
        .operation_id("qa_catalog.list_product_plugins")
        .summary("List registered product plugins")
        .description(
            "Every product plugin this deployment registers, with the two \
             field-descriptor schemas each declares: `credential_schema` \
             renders the environment credential form, and `observed_schema` \
             describes what observing an environment of that product yields \
             and which values claim a platform role. `instance_id` is the \
             value to write back to a product's `plugin_instance_id`, \
             verbatim. An empty list is a 200, not a 404",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_product_plugins)
        .json_array_response_with_schema::<dto::ProductPluginDto>(
            openapi,
            StatusCode::OK,
            "Registered product plugins and their field descriptors",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
