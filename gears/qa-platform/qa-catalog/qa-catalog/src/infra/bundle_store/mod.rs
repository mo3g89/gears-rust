//! Bundle blob storage integration: the local-filesystem implementation of
//! [`crate::domain::ports::bundle_store::BundleStore`] (p1; a `file-storage`
//! adapter follows when `FileStorageClientV1` gains operations, ADR-0005).

mod local_fs;

pub use local_fs::LocalFsBundleStore;
