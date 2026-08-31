#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for `ProductsService`: CRUD scope selection (which PEP action
//! and resource id each verb requests) and the PUT-style full-replace
//! semantics of `update_product`.
//!
//! `RecordingAuthZ` wraps the same permissive decision logic as
//! [`super::test_support::PermissiveAuthZ`] but additionally records every
//! `(action, resource_id)` pair the service requested.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::models::{EvaluationRequest, EvaluationResponse};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, PolicyEnforcer};
use qa_catalog_sdk::Product;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::actions;
use super::products::ProductsService;
use super::test_support::{ctx, permissive_response, test_db_provider};
use crate::domain::error::DomainError;
use crate::domain::repos::ProductsRepository;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// `ProductsRepository` double: one configurable product, mutable so the
/// update/delete tests can observe what the service persisted.
#[derive(Default)]
struct MockProductsRepository {
    rows: Mutex<Option<Product>>,
}

impl MockProductsRepository {
    fn with_product(product: Product) -> Self {
        Self {
            rows: Mutex::new(Some(product)),
        }
    }

    /// The stored row, as the service left it.
    fn stored(&self) -> Option<Product> {
        self.rows.lock().unwrap().clone()
    }
}

#[async_trait]
impl ProductsRepository for MockProductsRepository {
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Product>, DomainError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .as_ref()
            .filter(|p| p.id == id)
            .cloned())
    }

    async fn list<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<Product>, DomainError> {
        unimplemented!("not exercised by these unit tests")
    }

    async fn create<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        _name: String,
        _key: String,
        _description: String,
        _folder: Option<String>,
    ) -> Result<Product, DomainError> {
        unimplemented!("not exercised by these unit tests")
    }

    async fn update<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
        name: String,
        key: String,
        description: String,
        folder: Option<String>,
    ) -> Result<Option<Product>, DomainError> {
        let mut guard = self.rows.lock().unwrap();
        let Some(row) = guard.as_mut().filter(|p| p.id == id) else {
            return Ok(None);
        };
        row.name = name;
        row.key = key;
        row.description = description;
        row.folder = folder;
        row.updated_at = OffsetDateTime::now_utc();
        Ok(Some(row.clone()))
    }

    async fn delete<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let mut guard = self.rows.lock().unwrap();
        if guard.as_ref().is_some_and(|p| p.id == id) {
            *guard = None;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Wraps [`super::test_support::permissive_response`] (always grants access)
/// while recording every `(action, resource_id)` pair requested.
#[derive(Default)]
struct RecordingAuthZ {
    requests: Mutex<Vec<(String, Option<Uuid>)>>,
}

impl RecordingAuthZ {
    fn requested(&self, action: &str, resource_id: Option<Uuid>) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(a, id)| a == action && *id == resource_id)
    }
}

#[async_trait]
impl AuthZResolverClient for RecordingAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.requests
            .lock()
            .unwrap()
            .push((request.action.name.clone(), request.resource.id));
        Ok(permissive_response(&request))
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn product(id: Uuid) -> Product {
    let now = OffsetDateTime::now_utc();
    Product {
        id,
        name: "vhp".to_owned(),
        key: "VHP".to_owned(),
        description: "Virtuozzo Hybrid Platform".to_owned(),
        folder: None,
        created_at: now,
        updated_at: now,
    }
}

async fn build_service(
    repo: Arc<MockProductsRepository>,
    authz: Arc<RecordingAuthZ>,
) -> ProductsService<MockProductsRepository> {
    let enforcer = PolicyEnforcer::new(authz);
    let db = test_db_provider().await;
    ProductsService::new(db, repo, enforcer)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// PRD `cpt-cf-qa-fr-catalog-products` ("manage"): the update verb, and the
/// PEP action it authorizes. Full replace covers `name`, `key`,
/// `description`, and `folder`.
#[tokio::test]
async fn update_product_replaces_fields_under_an_update_scope() {
    let tenant_id = Uuid::new_v4();
    let product_id = Uuid::new_v4();

    let repo = Arc::new(MockProductsRepository::with_product(product(product_id)));
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(Arc::clone(&repo), Arc::clone(&authz)).await;

    let updated = svc
        .update_product(
            &ctx(tenant_id),
            product_id,
            "vhi".to_owned(),
            "VHI".to_owned(),
            "Virtuozzo Hybrid Infrastructure".to_owned(),
            Some("virt".to_owned()),
        )
        .await
        .unwrap();

    assert_eq!(updated.name, "vhi");
    assert_eq!(updated.key, "VHI");
    assert_eq!(updated.description, "Virtuozzo Hybrid Infrastructure");
    assert_eq!(updated.folder.as_deref(), Some("virt"));
    let stored = repo.stored().unwrap();
    assert_eq!(
        stored.name, "vhi",
        "the change must reach the repository, not just the return value"
    );
    assert_eq!(stored.key, "VHI");
    assert_eq!(stored.description, "Virtuozzo Hybrid Infrastructure");
    assert!(
        authz.requested(actions::UPDATE, Some(product_id)),
        "expected an UPDATE request for the product's id; requests were: {:?}",
        authz.requests.lock().unwrap()
    );

    // Full replace: an absent folder moves the product back to the root.
    let cleared = svc
        .update_product(
            &ctx(tenant_id),
            product_id,
            "vhi".to_owned(),
            "VHI".to_owned(),
            "Virtuozzo Hybrid Infrastructure".to_owned(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(cleared.folder, None, "PUT semantics: folder is replaced");
}

#[tokio::test]
async fn update_product_rejects_an_empty_name_and_404s_on_a_foreign_id() {
    let tenant_id = Uuid::new_v4();
    let product_id = Uuid::new_v4();

    let repo = Arc::new(MockProductsRepository::with_product(product(product_id)));
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(Arc::clone(&repo), authz).await;

    let err = svc
        .update_product(
            &ctx(tenant_id),
            product_id,
            String::new(),
            "VHP".to_owned(),
            "Virtuozzo Hybrid Platform".to_owned(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "name"),
        "got {err:?}"
    );
    assert_eq!(
        repo.stored().unwrap().name,
        "vhp",
        "a rejected update must not persist"
    );

    let foreign = Uuid::new_v4();
    let err = svc
        .update_product(
            &ctx(tenant_id),
            foreign,
            "x".to_owned(),
            "X".to_owned(),
            "x".to_owned(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == foreign),
        "a product outside the scope must 404, got {err:?}"
    );
}

#[tokio::test]
async fn delete_product_removes_the_row_and_404s_when_it_is_gone() {
    let tenant_id = Uuid::new_v4();
    let product_id = Uuid::new_v4();

    let repo = Arc::new(MockProductsRepository::with_product(product(product_id)));
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(Arc::clone(&repo), Arc::clone(&authz)).await;

    svc.delete_product(&ctx(tenant_id), product_id)
        .await
        .unwrap();
    assert!(repo.stored().is_none(), "the row must be removed");
    assert!(
        authz.requested(actions::DELETE, Some(product_id)),
        "expected a DELETE request for the product's id; requests were: {:?}",
        authz.requests.lock().unwrap()
    );

    let err = svc
        .delete_product(&ctx(tenant_id), product_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == product_id),
        "a second delete must 404, got {err:?}"
    );
}

#[tokio::test]
async fn create_product_rejects_empty_name() {
    let tenant_id = Uuid::new_v4();
    let repo = Arc::new(MockProductsRepository::default());
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(repo, authz).await;

    let err = svc
        .create_product(
            &ctx(tenant_id),
            String::new(),
            "KEY".to_owned(),
            "description".to_owned(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "name"),
        "got {err:?}"
    );
}
