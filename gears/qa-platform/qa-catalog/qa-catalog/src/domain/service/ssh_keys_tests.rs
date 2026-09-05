#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for `SshKeysService`: the key material must reach credstore
//! and NEVER the database row (which carries only the reference and the
//! fingerprint).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::{
    CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef, SecretValue, SharingMode,
    WriteOptions, WritePrecondition,
};
use qa_catalog_sdk::SshKey;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::ssh_keys::SshKeysService;
use super::test_support::{PermissiveAuthZ, ctx, test_db_provider};
use crate::domain::error::DomainError;
use crate::domain::repos::SshKeysRepository;

const TEST_PEM: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA-test-material\n-----END OPENSSH PRIVATE KEY-----\n";

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// In-memory `SshKeysRepository`: records exactly what the service asked it
/// to persist, so tests can assert the row's full content. Optionally fails
/// its `create` so the compensating credstore delete can be exercised.
#[derive(Default)]
struct MockSshKeysRepository {
    rows: Mutex<Vec<SshKey>>,
    /// When set, `create` fails instead of inserting.
    failing_create: bool,
}

impl MockSshKeysRepository {
    /// A repository whose metadata write always fails (the realistic cause is
    /// a duplicate name) — the only way to reach `create_ssh_key`'s
    /// compensating credstore cleanup.
    fn failing_create() -> Self {
        Self {
            failing_create: true,
            ..Self::default()
        }
    }
}

#[async_trait]
impl SshKeysRepository for MockSshKeysRepository {
    async fn list<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<SshKey>, DomainError> {
        Ok(self.rows.lock().unwrap().clone())
    }

    async fn create<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        name: String,
        credstore_ref: String,
        fingerprint: String,
    ) -> Result<SshKey, DomainError> {
        if self.failing_create {
            return Err(DomainError::SshKeyNameExists { name });
        }
        let key = SshKey {
            id: Uuid::new_v4(),
            name,
            credstore_ref,
            fingerprint,
            created_at: OffsetDateTime::now_utc(),
        };
        self.rows.lock().unwrap().push(key.clone());
        Ok(key)
    }

    async fn find_by_id<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<SshKey>, DomainError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|k| k.id == id)
            .cloned())
    }

    async fn delete<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|k| k.id != id);
        Ok(rows.len() < before)
    }
}

/// In-memory credstore: `ref -> bytes`.
#[derive(Default)]
struct MockCredStore {
    secrets: Mutex<HashMap<String, Vec<u8>>>,
}

impl MockCredStore {
    fn secret(&self, raw_ref: &str) -> Option<Vec<u8>> {
        self.secrets.lock().unwrap().get(raw_ref).cloned()
    }

    fn len(&self) -> usize {
        self.secrets.lock().unwrap().len()
    }
}

#[async_trait]
impl CredStoreClientV1 for MockCredStore {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        let _ = key;
        unimplemented!("not exercised by the ssh-key unit tests")
    }

    async fn create_opts(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        _sharing: SharingMode,
        _opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        let mut secrets = self.secrets.lock().unwrap();
        if secrets.contains_key(key.as_ref()) {
            return Err(CredStoreError::Conflict);
        }
        secrets.insert(key.as_ref().to_owned(), value.as_bytes().to_vec());
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
        _precondition: WritePrecondition,
    ) -> Result<(), CredStoreError> {
        match self.secrets.lock().unwrap().remove(key.as_ref()) {
            Some(_) => Ok(()),
            None => Err(CredStoreError::NotFound),
        }
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

async fn build_service(
    repo: Arc<MockSshKeysRepository>,
    credstore: Arc<MockCredStore>,
) -> SshKeysService<MockSshKeysRepository> {
    let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
    let db = test_db_provider().await;
    SshKeysService::new(db, repo, credstore, enforcer)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ssh_key_material_never_in_db() {
    let tenant_id = Uuid::new_v4();
    let repo = Arc::new(MockSshKeysRepository::default());
    let credstore = Arc::new(MockCredStore::default());
    let svc = build_service(Arc::clone(&repo), Arc::clone(&credstore)).await;

    let created = svc
        .create_ssh_key(&ctx(tenant_id), "ci-key".to_owned(), TEST_PEM.to_owned())
        .await
        .unwrap();

    // The material landed in credstore under the returned reference...
    assert_eq!(
        credstore.secret(&created.credstore_ref).as_deref(),
        Some(TEST_PEM.as_bytes()),
        "the PEM must be stored in credstore under the row's reference"
    );

    // ...and the persisted row carries ONLY the reference + fingerprint.
    let rows = repo.rows.lock().unwrap().clone();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.name, "ci-key");
    assert_eq!(row.credstore_ref, created.credstore_ref);
    assert!(
        row.fingerprint.starts_with("SHA256:"),
        "fingerprint format: {}",
        row.fingerprint
    );
    // No field of the row may contain the material (or any line of it).
    for field in [&row.name, &row.credstore_ref, &row.fingerprint] {
        assert!(
            !field.contains("AAAA-test-material") && !field.contains(TEST_PEM),
            "key material leaked into a DB column: {field}"
        );
    }

    // Deterministic fingerprint (documented p1 fallback: SHA-256 of the PEM).
    let again = svc
        .create_ssh_key(&ctx(tenant_id), "ci-key-2".to_owned(), TEST_PEM.to_owned())
        .await
        .unwrap();
    assert_eq!(again.fingerprint, created.fingerprint);
    assert_ne!(
        again.credstore_ref, created.credstore_ref,
        "every key gets its own generated secret reference"
    );
}

#[tokio::test]
async fn delete_ssh_key_removes_secret_then_row() {
    let tenant_id = Uuid::new_v4();
    let repo = Arc::new(MockSshKeysRepository::default());
    let credstore = Arc::new(MockCredStore::default());
    let svc = build_service(Arc::clone(&repo), Arc::clone(&credstore)).await;

    let created = svc
        .create_ssh_key(&ctx(tenant_id), "ci-key".to_owned(), TEST_PEM.to_owned())
        .await
        .unwrap();
    assert_eq!(credstore.len(), 1);

    svc.delete_ssh_key(&ctx(tenant_id), created.id)
        .await
        .unwrap();

    assert_eq!(credstore.len(), 0, "the credstore secret must be removed");
    assert!(
        repo.rows.lock().unwrap().is_empty(),
        "the row must be removed"
    );

    // Deleting again: the row is gone → NotFound.
    let err = svc
        .delete_ssh_key(&ctx(tenant_id), created.id)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}

/// Compensating delete: the material reaches credstore *before* the metadata
/// row, so a failed row write must not leave an orphaned secret behind — the
/// gear would have no reference to it and could never clean it up.
#[tokio::test]
async fn create_ssh_key_deletes_the_secret_when_the_row_write_fails() {
    let tenant_id = Uuid::new_v4();
    let repo = Arc::new(MockSshKeysRepository::failing_create());
    let credstore = Arc::new(MockCredStore::default());
    let svc = build_service(Arc::clone(&repo), Arc::clone(&credstore)).await;

    let err = svc
        .create_ssh_key(&ctx(tenant_id), "ci-key".to_owned(), TEST_PEM.to_owned())
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::SshKeyNameExists { ref name } if name == "ci-key"),
        "the original write error must propagate, not the cleanup outcome: {err:?}"
    );
    assert_eq!(
        credstore.len(),
        0,
        "the just-written secret must be removed, leaving no orphan in credstore"
    );
    assert!(repo.rows.lock().unwrap().is_empty());
}

#[tokio::test]
async fn create_ssh_key_rejects_empty_inputs() {
    let tenant_id = Uuid::new_v4();
    let repo = Arc::new(MockSshKeysRepository::default());
    let credstore = Arc::new(MockCredStore::default());
    let svc = build_service(repo, Arc::clone(&credstore)).await;

    let err = svc
        .create_ssh_key(&ctx(tenant_id), String::new(), TEST_PEM.to_owned())
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));

    let err = svc
        .create_ssh_key(&ctx(tenant_id), "ci-key".to_owned(), "  \n".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));

    assert_eq!(
        credstore.len(),
        0,
        "rejected input must never reach credstore"
    );
}
