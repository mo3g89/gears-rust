use async_trait::async_trait;
use qa_catalog_sdk::{NewProduct, Product, ProductUpdate};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Repository trait for `Product` persistence.
#[async_trait]
pub trait ProductsRepository: Send + Sync {
    /// Find a product by ID within the given security scope.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Product>, DomainError>;

    /// List all products visible within the given security scope.
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Product>, DomainError>;

    /// Create a new product.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewProduct,
    ) -> Result<Product, DomainError>;

    /// Replace the mutable fields of an existing product. Returns `Ok(None)`
    /// when no row with `id` is visible in `scope`.
    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        update: ProductUpdate,
    ) -> Result<Option<Product>, DomainError>;

    /// Delete a product by ID.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;
}
