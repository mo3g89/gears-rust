//! Shared test doubles for the `domain::service` unit tests
//! (`leases_tests`, `variables_tests`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::models::{EvaluationRequest, EvaluationResponse};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError};
use qa_environments_sdk::{NewPlatform, PlatformPatch, TargetPlatform};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::DbProvider;
use crate::domain::error::DomainError;
use crate::domain::repos::PlatformsRepository;

/// Build a `SecurityContext` for `tenant_id` with a fresh random subject.
pub(super) fn ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

/// Build a `TargetPlatform` fixture with the given id/availability.
pub(super) fn platform(id: Uuid, available: bool) -> TargetPlatform {
    let now = OffsetDateTime::now_utc();
    TargetPlatform {
        id,
        name: "test-platform".to_owned(),
        product_id: None,
        description: None,
        kubeconfig_credstore_ref: "credstore://test".to_owned(),
        available,
        observed_version: None,
        observed_build: None,
        default_branch: None,
        is_default: false,
        vhp_base_url: None,
        observed_namespace: None,
        version_detect_error: None,
        version_detected_at: None,
        // This fixture predates the platform-observation work; nothing here
        // has ever been observed.
        cluster: None,
        created_at: now,
        updated_at: now,
    }
}

/// The mocks in this module never touch the database — the `DBRunner`
/// argument is ignored by every mock method — so a real `Db` handle is only
/// needed to satisfy `Arc<DbProvider>` fields and produce a `DbConn` value to
/// pass through. No migrations are required.
pub(super) async fn test_db_provider() -> Arc<DbProvider> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("failed to connect to in-memory sqlite");
    Arc::new(DBProvider::<DbError>::new(db))
}

/// `PlatformsRepository` double returning a single configurable platform (or
/// `None`). Only `get` is exercised by the service-layer unit tests; `get`
/// respects the requested id (returns `None` for any other id).
pub(super) struct MockPlatformsRepository {
    platform: Option<TargetPlatform>,
}

impl MockPlatformsRepository {
    pub(super) fn with_platform(platform: TargetPlatform) -> Self {
        Self {
            platform: Some(platform),
        }
    }

    pub(super) fn none() -> Self {
        Self { platform: None }
    }
}

#[async_trait]
impl PlatformsRepository for MockPlatformsRepository {
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TargetPlatform>, DomainError> {
        Ok(self.platform.as_ref().filter(|p| p.id == id).cloned())
    }

    async fn list<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<TargetPlatform>, DomainError> {
        unimplemented!("not exercised by the service-layer unit tests")
    }

    async fn list_all_with_tenant<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<(TargetPlatform, Uuid)>, DomainError> {
        unimplemented!("not exercised by the service-layer unit tests")
    }

    async fn create<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        _new: NewPlatform,
        _kubeconfig_credstore_ref: String,
    ) -> Result<TargetPlatform, DomainError> {
        unimplemented!("not exercised by the service-layer unit tests")
    }

    async fn update<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
        _patch: PlatformPatch,
    ) -> Result<Option<TargetPlatform>, DomainError> {
        unimplemented!("not exercised by the service-layer unit tests")
    }

    async fn clear_default_for_product<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _product_id: Uuid,
        _except_id: Uuid,
    ) -> Result<u64, DomainError> {
        // Returns 0 rather than `unimplemented!()`: the service calls this on any
        // create/update that sets the flag, so panicking here would fail tests that
        // are about something else entirely. The rule itself is covered against the
        // real repository, where the row state can actually be observed.
        Ok(0)
    }

    async fn delete<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
    ) -> Result<bool, DomainError> {
        unimplemented!("not exercised by the service-layer unit tests")
    }

    async fn record_observation<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
        _observation: &crate::domain::ports::PlatformObservation,
    ) -> Result<(), DomainError> {
        unimplemented!("not exercised by the service-layer unit tests")
    }
}

/// Shared decision logic for the `AuthZ` test doubles below: always grants
/// access, returning a tenant `IN` constraint derived from the subject's
/// tenant (mirrors a real PDP's default tenant-isolation policy). The mocks
/// in this crate's test suite ignore the resulting `AccessScope` entirely,
/// but the PEP flow still needs a well-formed PDP response to compile one.
///
/// Defined once in `crate::test_support` and reused here (rather than
/// duplicated) so this mock-repository tier and the DB-backed
/// `tests_tenant_scoping` tier can't drift apart on what "permissive" means.
pub(super) use crate::test_support::permissive_response;

/// Permissive `AuthZ` resolver: always grants access (mirrors `users-info`'s
/// `test_support::MockAuthZResolver`). Use [`RecordingAuthZ`] in
/// `variables_tests` instead when a test needs to assert *which* action/
/// resource-id the service requested.
pub(super) struct PermissiveAuthZ;

#[async_trait]
impl AuthZResolverClient for PermissiveAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(permissive_response(&request))
    }
}
