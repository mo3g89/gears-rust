//! The plugin SPI implementation.
//!
//! Like the in-memory plugin, this is a pure per-tenant value store: it
//! ignores the security context (the gear has already authorized the request
//! and resolved tenant/owner) and keys purely on `(tenant_id, key, owner_id)`.

use async_trait::async_trait;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV1, OwnerId, SecretRef, SecretValue, TenantId,
};
use toolkit_security::SecurityContext;

use super::service::Service;

#[async_trait]
impl CredStorePluginClientV1 for Service {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        self.get_value(tenant_id, key, owner_id).await
    }

    async fn put(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        value: SecretValue,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError> {
        self.put_value(tenant_id, key, value, owner_id).await
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError> {
        self.delete_value(tenant_id, key, owner_id).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
