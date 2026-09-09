use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{CreateProductReq, ProductDto, ProductFolderListDto, UpdateProductReq};
use crate::gear::ConcreteAppServices;

/// List all products visible to the caller.
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_products(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<Vec<ProductDto>>> {
    let products = svc.products.list_products(&ctx).await?;
    Ok(Json(products.into_iter().map(ProductDto::from).collect()))
}

/// Create a product.
#[tracing::instrument(skip(svc, ctx, req), fields(product.name = %req.name))]
pub async fn create_product(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<CreateProductReq>,
) -> ApiResult<impl IntoResponse> {
    // `try_into` since Task 20: the wire's optional `plugin_instance_id`
    // becomes a required one here, with the message that names
    // `GET /qa/v1/product-plugins`. See the `TryFrom` impl.
    let product = svc.products.create_product(&ctx, req.try_into()?).await?;
    let id_str = product.id.to_string();
    Ok(created_json(ProductDto::from(product), &uri, &id_str).into_response())
}

/// Replace a product's mutable fields.
#[tracing::instrument(skip(svc, ctx, req), fields(product.id = %id, product.name = %req.name))]
pub async fn update_product(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateProductReq>,
) -> ApiResult<Json<ProductDto>> {
    let product = svc.products.update_product(&ctx, id, req.into()).await?;
    Ok(Json(ProductDto::from(product)))
}

/// Delete a product (cascades to its versions).
#[tracing::instrument(skip(svc, ctx), fields(product.id = %id))]
pub async fn delete_product(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.products.delete_product(&ctx, id).await?;
    Ok(no_content().into_response())
}

/// Distinct non-null folder names across the caller's visible products.
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_product_folders(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<ProductFolderListDto>> {
    let folders = svc.products.list_product_folders(&ctx).await?;
    Ok(Json(ProductFolderListDto { folders }))
}
