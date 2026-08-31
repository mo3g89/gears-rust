use async_trait::async_trait;

use crate::domain::error::DomainError;

/// Port abstracting bundle blob storage. p1 impl: gear-local filesystem
/// (Task 9). A `file-storage` adapter follows when `FileStorageClientV1`
/// gains operations. Consumed by `domain::service::BundlesService`.
#[async_trait]
pub trait BundleStore: Send + Sync {
    /// Store bundle bytes; returns an opaque `storage_ref`.
    async fn put(&self, bundle_id: uuid::Uuid, bytes: Vec<u8>) -> Result<String, DomainError>;

    /// Retrieve bundle bytes previously stored under `storage_ref`.
    async fn get(&self, storage_ref: &str) -> Result<Vec<u8>, DomainError>;

    /// Delete the bundle stored under `storage_ref`.
    async fn delete(&self, storage_ref: &str) -> Result<(), DomainError>;
}
