//! The archived-log repository port.
//!
//! # A separate trait, deliberately, and not a separate struct
//!
//! `OrmRunsRepository` implements this as well as [`RunsRepository`], because
//! `AppServices` is generic over its repository types and a fourth type
//! parameter would ripple through every service signature for no gain. What
//! matters is that the two *traits* are separate: `get_log` must not sit next
//! to `list`, or a future "list runs with their logs" convenience has
//! everything it needs to reproduce legacy's OOM.
//!
//! `no_list_query_reaches_the_log_table`
//! (`infra::storage::runs_sea_repo`) is the guard that keeps that true
//! regardless of intent.
//!
//! [`RunsRepository`]: crate::domain::repos::RunsRepository

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// One run's archived log, and enough metadata to describe it without
/// re-reading the text.
///
/// `Debug` is hand-written below and there is deliberately **no** `PartialEq`
/// or `Eq` — see that impl's own doc for both.
#[derive(Clone)]
pub struct ArchivedLog {
    pub text: String,
    pub lines: i64,
}

/// Reports `lines` and `text`'s **length**, never `text` itself.
///
/// This mirrors `infra::logs::archive::Pending`'s hand-written `Debug`, and
/// this type crosses a **wider** boundary than that one does: it is `pub`,
/// re-exported from `domain::repos`, returned from the `pub`
/// `RunsService::archived_log`, and held as a local inside
/// `api::rest::handlers::runs::stream_run_logs`, which **is**
/// `#[tracing::instrument]`ed. Adding `ret` to that attribute, or hoisting the
/// value into a parameter of a helper that later gets instrumented, would print
/// a tenant's entire log into a span — which is not hypothetical in this crate:
/// `stream_run_logs` needed `skip(svc, ctx, logs)` added to that exact
/// attribute because `RunLogBroadcaster`'s **derived** `Debug` walked its
/// `Mutex`-guarded channel map and enumerated every run streaming on the
/// replica, other tenants' included, into every child span.
///
/// # Why `PartialEq` and `Eq` are gone rather than kept
///
/// They had no user. A dead derive on a type holding bulk tenant text is not
/// inert, though: the next `assert_eq!` written against an `ArchivedLog` — or
/// against an `Option<ArchivedLog>` — renders both sides through `Debug` on
/// failure, which would have turned an unused derive into a printer. Without
/// `PartialEq` that comparison does not compile, so the two existing call
/// sites' shape is enforced rather than merely conventional:
///
/// * `assert!(x.is_none())`, not `assert_eq!(x, None)` — the former prints
///   nothing, the latter would have printed the log. Both call sites
///   (`infra::storage::run_logs_sea_repo`'s cross-tenant read test and
///   `api::rest::handlers::runs_handler_tests`'s scoped-read test) already
///   use the safe form; that was previously load-bearing and unstated.
/// * `Option::expect("…")` on the `Option<ArchivedLog>` — `expect` on an
///   `Option` prints only its own message, never the payload. (`Result::expect`
///   prints the *error*, which here is a `DomainError` and carries no log
///   text by this path's own rule.)
///
/// Assertions on the content assert on `log.text` or `log.lines` directly,
/// which is both narrower and what the tests actually mean.
impl std::fmt::Debug for ArchivedLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchivedLog")
            .field("lines", &self.lines)
            .field("text_len", &self.text.len())
            .finish()
    }
}

#[async_trait]
pub trait RunLogsRepository: Send + Sync {
    /// Concatenate `text` onto `run_id`'s archived log, creating the row if it
    /// is the first append, and add `lines` to its count.
    ///
    /// **The concatenation happens in the statement.** Reading the row,
    /// appending in Rust and writing it back would re-transfer the whole log on
    /// every flush and would lose a concurrent append.
    ///
    /// There is no size cap: user decision, 2026-08-31, risk recorded in the
    /// design's §8.
    async fn append_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
        tenant_id: Uuid,
        text: &str,
        lines: i64,
    ) -> Result<(), DomainError>;

    /// Read `run_id`'s archived log, or `None` when it has none.
    ///
    /// `None` is the ordinary answer for a run that finished before this table
    /// existed, and it is what makes the handler fall back to the broadcaster's
    /// in-memory tail rather than showing an empty pane.
    async fn get_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Option<ArchivedLog>, DomainError>;
}
