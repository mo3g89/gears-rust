//! A [`DbProvider`] wrapper that can only open `SERIALIZABLE` transactions.
//!
//! Its own module so that the field is genuinely private: Rust makes a private
//! item visible to the defining module and everything beneath it, so declaring
//! this beside the services would have left `db.0` reachable from all of them.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use toolkit_db::secure::{DbConn, DbTx, TxConfig};
use tracing::warn;

use super::DbProvider;
use crate::domain::error::DomainError;
// `DE0301` (no infra in domain) is allowed for this one import. The lint is
// right that a domain module should not name `crate::infra` -- but the
// alternative here is worse, not better: `is_retryable_contention` takes a
// `&toolkit_db::Db` because the backend decides which SQLSTATEs are
// retryable, so moving it into the domain moves a driver handle in with it and
// trades one DE0301 for a less honest one. The retry policy is domain, the
// classification of a driver error is not. Kept at the import so a second
// `crate::infra` use in this module still has to argue for itself.
//
// `clippy::useless_attribute` is a false positive here and is allowed on the
// same item. That lint fires on any `allow` attached to a `use`, because for
// the lints it knows about the diagnostic lands at the *usage* site, not the
// import. `DE0301` is a Dylint lint clippy has never heard of, and it emits at
// the `use` item itself -- verified by deleting the `allow` below, after which
// `cargo gears lint --dylint -P qa-runs` fails with `DE0301` pointing at
// exactly this line. Clippy's suggested `#![allow(..)]` would be wrong twice
// over: an inner attribute is not permitted in this position, and hoisted to
// the module header it would suppress `DE0301` for the whole file -- the thing
// the paragraph above says was deliberately not done.
#[allow(unknown_lints, clippy::useless_attribute, de0301_no_infra_in_domain)]
use crate::infra::storage::db::is_retryable_contention;

/// A [`DbProvider`] that cannot open an un-escalated transaction.
///
/// # Why the seam is a type rather than a convention
///
/// `service::ingest`'s two transactions have to be `SERIALIZABLE` or the races
/// Task 16b step 1 closed reopen, and nothing about `self.db.transaction(..)`
/// looks wrong at a call site. This wrapper exposes [`Self::with_retry`] and
/// [`Self::conn`] and deliberately **not** `transaction` or
/// `transaction_with_config`, so a holder cannot open an un-escalated one. That
/// is the shape `gear::LogWiring` used against the two-broadcaster defect.
///
/// **Checked by compiling every escape anyone proposed, plus controls.** Two
/// earlier versions of this wrapper did not hold, and neither failure was
/// visible by reading:
///
/// * declared beside the services, `self.db.0.transaction(..)` compiled from
///   `service::ingest`, because a private field is visible to the defining
///   module *and every descendant*. Fixed by making this a sibling module.
/// * with `IngestDeps.db` still a raw `Arc<DbProvider>`,
///   `deps.db.transaction(..)` compiled inside `service::ingest` — the wrapper
///   was applied on the way *in* to the service, so the un-wrapped provider was
///   still in the module. Fixed by wrapping at the construction site instead.
///
/// Seven spellings are now refused — `transaction` and `transaction_with_config`
/// (`E0599`), field access and destructuring (`E0616`, `E0532`), `db()`
/// (`E0599`), and the same two through `IngestDeps` — and two controls
/// (`self.db.conn()`, `deps.db.conn()`) still compile. The controls are the
/// point: without them a harness in which everything failed would look
/// identical to one in which everything was refused.
///
/// **What it does not close, which is most of the class.** It binds the holders
/// of *this wrapper*. Any other holder of the same `Arc<DbProvider>` can still
/// open a default transaction over `qa_runs` and `qa_run_test_results` and would
/// not be serialized against ingest's two, because `SERIALIZABLE` is a property
/// of a pair of transactions and not of one. Nothing in the type system says so,
/// and this wrapper does not change that.
pub(in crate::domain::service) struct SerializedDb(Arc<DbProvider>);

impl SerializedDb {
    /// Wrap a provider. The field stays private to this module, which is the
    /// whole mechanism: a consumer in another module cannot reach past the
    /// wrapper to the provider it holds.
    pub(in crate::domain::service) fn new(db: Arc<DbProvider>) -> Self {
        Self(db)
    }

    /// A non-transactional runner, for the reads that are not part of either
    /// escalated transaction.
    pub(in crate::domain::service) fn conn(&self) -> Result<DbConn<'_>, DomainError> {
        self.0.conn()
    }

    /// Run `body` in a `SERIALIZABLE` transaction, retrying it on a contention
    /// abort.
    ///
    /// # Why this is not `Db::transaction_with_retry`
    ///
    /// That helper is the workspace idiom and this is a copy of its loop, for
    /// one reason it cannot work around: it needs an
    /// `Fn(&E) -> Option<&sea_orm::DbErr>` extractor, and this gear's
    /// [`DomainError`] cannot supply one. `resource-group` and
    /// `account-management` keep the `DbErr` inside their error type, so their
    /// extractor is a `match` arm; `DomainError::Database` keeps a `String`,
    /// because `infra::storage::db::db_err` takes `impl Display` and stores
    /// `to_string()`.
    ///
    /// **The variant cannot simply start carrying a `DbErr`**, which is the
    /// shape of the blocker rather than its size:
    /// `infra::storage::db::odata_err` builds `DomainError::Database` from an
    /// `ODataError::Db(String)`, where no `DbErr` exists to carry. Supplying the
    /// extractor means splitting the variant, not widening it. `TODO(DE1302)` on
    /// that variant proposes a **boxed source**, which is a different change and
    /// would still not yield `Option<&sea_orm::DbErr>` without a downcast — so
    /// it does not already record this work.
    ///
    /// `body` is `FnMut` because a retry re-runs it, which is why every caller
    /// clones its captures per attempt rather than moving them once.
    ///
    /// **The budget is not injectable, and that is a known gap.**
    /// `toolkit-db` exposes `transaction_with_retry_max` precisely so a test can
    /// set the budget to 1 and watch the retry branch disappear; this loop
    /// hardcodes the constant, so falsifying that branch needs a source
    /// mutation rather than a test. Left as it is because a seam whose only
    /// caller is a test is itself untested surface, and because the branch *is*
    /// falsified — cutting the budget to 1 turns
    /// `two_concurrent_producers_of_one_test_leave_one_row_and_one_count` red
    /// (measured on the pinned `postgres:15-alpine`) — just not by anything
    /// checked in.
    ///
    /// # Why a bounded budget, and what exceeding it costs
    ///
    /// The budget is [`toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS`], shared with the
    /// helper this loop copies so the two cannot drift. It is bounded because
    /// the alternative — retrying forever — converts a persistent conflict into
    /// a stuck task holding a connection.
    ///
    /// **What an exhausted budget costs is more than the one event.**
    /// [`IngestService::ingest`] propagates with `?` and stops on the first
    /// failing event, so the abort ends the whole observation:
    /// `service::watch`'s drain logs a WARN and returns, and every later event
    /// on that stream — **including `Finished`** — is never applied. That run
    /// then reaches a terminal state only through the timeout sweep, which is
    /// the path Task 16c existed to close. Under the previous un-configured
    /// transaction nothing aborted, so this failure mode is one the escalation
    /// introduced; what it replaced was silent corruption of the stored rows.
    /// `heavy_contention_may_exhaust_the_budget_but_never_corrupts_what_is_stored`
    /// drives eight-way contention and asserts what holds either way. **How many
    /// producers actually exhaust the budget is not pinned and must not be
    /// read as pinned**: it is a timing property, it moved from 4 of 8 to 2 of 8
    /// when the container image was pinned from `postgres:11-alpine` to
    /// `15-alpine`, and at 0 of 8 nothing in the suite would notice.
    pub(in crate::domain::service) async fn with_retry<T, F>(
        &self,
        mut body: F,
    ) -> Result<T, DomainError>
    where
        T: Send + 'static,
        F: for<'a> FnMut(
                &'a DbTx<'a>,
            )
                -> Pin<Box<dyn Future<Output = Result<T, DomainError>> + Send + 'a>>
            + Send,
    {
        let mut attempt: u32 = 1;
        loop {
            let result = self
                .0
                .transaction_with_config(TxConfig::serializable(), |tx| body(tx))
                .await;
            match result {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if attempt < toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS
                        && is_retryable_contention(&error, &self.0.db())
                    {
                        warn!(
                            attempt,
                            max_attempts = toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS,
                            "retrying an ingest transaction after a contention abort",
                        );
                        attempt += 1;
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }
}
