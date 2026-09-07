//! Repository-level tests: `ValueRepo` takes a runner instead of reaching for
//! one internally.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use credstore_sdk::{SecretRef, TenantId};
use toolkit_db::DbError;
use uuid::Uuid;

use crate::infra::storage::error::StoreError;
use crate::test_support::{memory_dsn, provider_and_repo};

fn sref(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid secret ref")
}

/// **Two writes compose into one transaction.**
///
/// `ValueRepo` reached for `self.db.conn()` internally, so a caller could not
/// put two of its operations in one transaction -- a partial write on a
/// rollback path had no way to be undone. Every other repository in this
/// subsystem takes `runner: &C where C: DBRunner` for exactly this reason.
/// Review finding #14.
///
/// Adapted from the brief's example to this crate's real types: `ValueRepo`'s
/// methods are tenant-scoped (`find`/`upsert`/... take a `&TenantId`), and
/// this crate's error type is `StoreError`, not a bare `DbError`, so the
/// deliberate rollback is a `StoreError` built from `DbError::Other`.
#[tokio::test]
async fn two_writes_roll_back_together() {
    let (db, repo) = provider_and_repo(&memory_dsn()).await;
    let tenant = TenantId(Uuid::new_v4());
    let key_a = sref("a");
    let key_b = sref("b");

    // `DBProvider::transaction`'s closure must be usable for any lifetime of
    // the transaction it is handed, which only a `'static` capture can
    // satisfy -- so it gets its own clone of `repo` (an `Arc` bump) and
    // clones of the keys, rather than borrowing the ones asserted on below.
    let txn_repo = repo.clone();
    let txn_key_a = key_a.clone();
    let txn_key_b = key_b.clone();
    let result: Result<(), StoreError> = db
        .transaction(move |txn| {
            Box::pin(async move {
                txn_repo
                    .upsert(txn, &tenant, &txn_key_a, None, b"1")
                    .await?;
                txn_repo
                    .upsert(txn, &tenant, &txn_key_b, None, b"2")
                    .await?;
                Err(StoreError::Db(DbError::Other(anyhow::anyhow!(
                    "deliberate rollback"
                ))))
            })
        })
        .await;
    assert!(result.is_err());

    let conn = db.conn().unwrap();
    assert!(
        repo.find(&conn, &tenant, &key_a, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.find(&conn, &tenant, &key_b, None)
            .await
            .unwrap()
            .is_none()
    );
}
