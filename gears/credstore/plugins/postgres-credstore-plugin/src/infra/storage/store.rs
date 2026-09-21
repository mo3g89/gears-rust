//! The [`ValueStore`] adapter: the only place that decides what runs in a
//! transaction.
//!
//! [`ValueRepo`] deliberately reaches for nothing — every method takes an
//! explicit `runner: &C where C: DBRunner` (review finding #14,
//! `TOOLKIT-DB-001`), which is what lets a caller compose two of its
//! operations into one transaction. That caller is this type. It used to be
//! `domain::Service`, which meant the domain named `crate::infra` and held a
//! `DBProvider` (DE0301); the composition is an infrastructure concern, so it
//! moved here rather than being silenced there.
//!
//! Which sequences need a transaction, and why, is unchanged:
//!
//! * `upsert` — `UPDATE`-then-`INSERT` must be atomic with itself.
//! * `insert_if_absent` — the probe and the insert must be atomic against a
//!   concurrent writer.
//! * `find` / `delete` — single statements; a pooled connection is enough.

use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{OwnerId, SecretRef, TenantId};

use super::repo::{ValueDbProvider, ValueRepo};
use crate::domain::ports::{StoreFault, ValueStore};

/// The relational [`ValueStore`] this plugin ships.
pub struct PgValueStore {
    /// Handle for opening connections and transactions.
    db: Arc<ValueDbProvider>,
    /// Stateless-per-call repository over `credstore_plugin_values`.
    repo: ValueRepo,
}

impl PgValueStore {
    /// Wrap a repository, taking its provider for transaction composition.
    #[must_use]
    pub fn new(repo: ValueRepo) -> Self {
        let db = repo.provider();
        Self { db, repo }
    }
}

#[async_trait]
impl ValueStore for PgValueStore {
    async fn find(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<Option<Vec<u8>>, StoreFault> {
        let conn = self.db.conn().map_err(StoreFault::new)?;
        self.repo
            .find(&conn, tenant_id, key, owner_id)
            .await
            .map_err(StoreFault::new)
    }

    async fn upsert(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
        value: &[u8],
    ) -> Result<(), StoreFault> {
        // The closure passed to `transaction` must be usable for *any*
        // lifetime of the transaction it is handed
        // (`DBProvider::transaction`'s `for<'a> FnOnce(&'a DbTx<'a>) -> ... +
        // 'a` bound), which only a `'static` capture can satisfy -- hence
        // cloning `repo` (an `Arc` bump) and copying the arguments out before
        // the closure, rather than capturing `&self` or the parameters.
        let repo = self.repo.clone();
        let tenant_id = *tenant_id;
        let key = key.clone();
        let owner_id = owner_id.copied();
        let bytes = value.to_vec();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    repo.upsert(tx, &tenant_id, &key, owner_id.as_ref(), &bytes)
                        .await
                })
            })
            .await
            .map_err(StoreFault::new)
    }

    async fn insert_if_absent(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
        value: &[u8],
    ) -> Result<bool, StoreFault> {
        // See `upsert` for why the capture must be owned rather than borrowed.
        let repo = self.repo.clone();
        let tenant_id = *tenant_id;
        let key = key.clone();
        let owner_id = owner_id.copied();
        let bytes = value.to_vec();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    repo.insert_if_absent(tx, &tenant_id, &key, owner_id.as_ref(), &bytes)
                        .await
                })
            })
            .await
            .map_err(StoreFault::new)
    }

    async fn delete(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), StoreFault> {
        let conn = self.db.conn().map_err(StoreFault::new)?;
        self.repo
            .delete(&conn, tenant_id, key, owner_id)
            .await
            .map_err(StoreFault::new)
    }
}
