//! Local-filesystem [`BundleStore`]: bundle blobs as `<bundles_dir>/<bundle_id>.tar.gz`.
//!
//! The `storage_ref` returned by [`BundleStore::put`] is the blob's absolute
//! path. Refs come back through the database on `get`/`delete`, so both
//! validate — defense in depth — that the ref still resolves to a
//! `<uuid>.tar.gz` file *directly inside* the canonicalized bundles
//! directory before touching the filesystem: a tampered ref must not be
//! able to read or delete arbitrary files.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::bundle_store::BundleStore;
use crate::infra::fs::create_private_dir_all;

/// Extension every bundle blob is stored under.
const BUNDLE_EXT: &str = ".tar.gz";

/// Local-filesystem bundle blob store over the configured
/// `QaCatalogConfig::bundles_dir`.
// Constructed by the gear bootstrap (`crate::gear`, config → `new`) and
// injected into `BundlesService`.
#[derive(Debug, Clone)]
pub struct LocalFsBundleStore {
    /// Canonicalized bundles directory. Canonical form anchors the
    /// containment check in [`Self::resolve_ref`].
    dir: PathBuf,
}

impl LocalFsBundleStore {
    /// Create the store over `bundles_dir`, creating the directory
    /// owner-only (`0700` on unix) if it does not exist yet — bundle blobs
    /// carry a tenant's test content and must not be world-readable on a
    /// shared host (see [`create_private_dir_all`]).
    ///
    /// # Errors
    ///
    /// [`DomainError::Storage`] when the directory cannot be created or
    /// canonicalized.
    pub fn new(bundles_dir: impl AsRef<Path>) -> Result<Self, DomainError> {
        let bundles_dir = bundles_dir.as_ref();
        create_private_dir_all(bundles_dir).map_err(|e| {
            DomainError::Storage(format!("failed to create the bundles directory: {e}"))
        })?;
        let dir = bundles_dir.canonicalize().map_err(|e| {
            DomainError::Storage(format!("failed to canonicalize the bundles directory: {e}"))
        })?;
        Ok(Self { dir })
    }

    /// Validate that `storage_ref` names a `<uuid>.tar.gz` blob directly
    /// inside the bundles directory and return its path.
    ///
    /// Purely lexical (parent equality against the canonical dir plus a
    /// strict filename shape), so `..` segments, relative refs, and paths
    /// outside the directory are all rejected without touching the
    /// filesystem.
    fn resolve_ref(&self, storage_ref: &str) -> Result<PathBuf, DomainError> {
        let path = Path::new(storage_ref);
        let valid_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(BUNDLE_EXT))
            .is_some_and(|stem| Uuid::parse_str(stem).is_ok());
        if valid_name && path.parent() == Some(self.dir.as_path()) {
            Ok(path.to_path_buf())
        } else {
            Err(DomainError::Storage(format!(
                "storage ref '{storage_ref}' does not resolve to a bundle in the bundles directory"
            )))
        }
    }
}

#[async_trait]
impl BundleStore for LocalFsBundleStore {
    async fn put(&self, bundle_id: Uuid, bytes: Vec<u8>) -> Result<String, DomainError> {
        let path = self.dir.join(format!("{bundle_id}{BUNDLE_EXT}"));
        let storage_ref = path
            .to_str()
            .ok_or_else(|| {
                DomainError::Storage("bundles directory path is not valid UTF-8".to_owned())
            })?
            .to_owned();

        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            // Never leave a partial blob behind a failed write: the
            // descriptor is only persisted after `put` succeeds, so the
            // path would otherwise hold unreferenced garbage.
            let _removed = tokio::fs::remove_file(&path).await;
            return Err(DomainError::Storage(format!(
                "failed to write bundle {bundle_id}: {e}"
            )));
        }
        Ok(storage_ref)
    }

    async fn get(&self, storage_ref: &str) -> Result<Vec<u8>, DomainError> {
        let path = self.resolve_ref(storage_ref)?;
        tokio::fs::read(&path)
            .await
            .map_err(|e| DomainError::Storage(format!("failed to read bundle blob: {e}")))
    }

    async fn delete(&self, storage_ref: &str) -> Result<(), DomainError> {
        let path = self.resolve_ref(storage_ref)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            // Idempotent: a blob already gone (e.g. a retried GC sweep)
            // is a successful delete.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DomainError::Storage(format!(
                "failed to delete bundle blob: {e}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use uuid::Uuid;

    use super::*;

    fn store() -> (tempfile::TempDir, LocalFsBundleStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalFsBundleStore::new(dir.path().join("bundles")).unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn put_get_delete_roundtrip() {
        let (_guard, store) = store();
        let id = Uuid::new_v4();
        let payload = b"tar.gz bytes".to_vec();

        let storage_ref = store.put(id, payload.clone()).await.unwrap();
        assert!(storage_ref.ends_with(&format!("{id}.tar.gz")));

        assert_eq!(store.get(&storage_ref).await.unwrap(), payload);

        store.delete(&storage_ref).await.unwrap();
        assert!(matches!(
            store.get(&storage_ref).await,
            Err(DomainError::Storage(_))
        ));
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let (_guard, store) = store();
        let storage_ref = store.put(Uuid::new_v4(), vec![1, 2, 3]).await.unwrap();

        store.delete(&storage_ref).await.unwrap();
        store.delete(&storage_ref).await.unwrap();
    }

    #[tokio::test]
    async fn new_creates_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/bundles");
        LocalFsBundleStore::new(&nested).unwrap();
        assert!(nested.is_dir());
    }

    #[tokio::test]
    async fn get_rejects_refs_outside_the_bundles_dir() {
        let (guard, store) = store();
        let id = Uuid::new_v4();

        // A sibling of the bundles dir with a valid-looking name.
        let outside = guard.path().join(format!("{id}.tar.gz"));
        std::fs::write(&outside, b"secret").unwrap();

        let refs = [
            outside.to_str().unwrap().to_owned(),
            // Traversal out of the bundles dir back to the same file.
            format!("{}/bundles/../{id}.tar.gz", guard.path().display()),
            // Relative ref.
            format!("{id}.tar.gz"),
            "/etc/passwd".to_owned(),
        ];
        for storage_ref in refs {
            assert!(
                matches!(store.get(&storage_ref).await, Err(DomainError::Storage(_))),
                "ref must be rejected: {storage_ref}"
            );
        }
    }

    #[tokio::test]
    async fn refs_must_name_a_uuid_bundle_file() {
        let (_guard, store) = store();

        // In the right directory, but not `<uuid>.tar.gz`.
        std::fs::write(store.dir.join("notes.txt"), b"data").unwrap();
        let inside_ref = store.dir.join("notes.txt").to_str().unwrap().to_owned();
        assert!(matches!(
            store.get(&inside_ref).await,
            Err(DomainError::Storage(_))
        ));
        assert!(matches!(
            store.delete(&inside_ref).await,
            Err(DomainError::Storage(_))
        ));
    }
}
