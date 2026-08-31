//! Domain repository traits.
//!
//! Six traits over the eleven tables. Every method takes the caller-prepared
//! [`AccessScope`](toolkit_security::AccessScope) and a
//! [`DBRunner`](toolkit_db::secure::DBRunner) — `&DbConn` or a transaction
//! runner — so a multi-statement operation can run inside a service-owned
//! transaction. Implementations must never widen the scope they are handed:
//! whatever scope reaches a trait in this module — PEP-compiled, or minted by
//! the one named seam, `domain::elevated::enumeration_scope`, for the
//! nil-tenant ticker enumeration — is executed as given, never assembled or
//! loosened here. `AccessScope::allow_all()` itself appears in this gear's
//! production code only at that one seam; no implementation in this module
//! constructs one, and no query here is unscoped on its own account.
//!
//! # There are no foreign keys, in either direction
//!
//! `run_id`, `repo_id` and `platform_id` all name rows in *other gears'*
//! schemas, and DESIGN §3.7 forbids cross-schema foreign keys — so unlike
//! qa-runs' repositories, nothing here can lean on the database to reject a
//! write against a parent that does not exist.
//!
//! What replaces it is obligation #1 of the schema, and it lands on the
//! **service** layer rather than on these traits: *resolve every
//! caller-supplied `run_id` and `repo_id` under the caller's own scope before
//! writing a row that uses it.* Nothing in the database will catch a result row
//! written against a run that does not exist, or that belongs to another
//! tenant.
//!
//! qa-runs discharges its version of this with an `OwnedRunId` token, minted
//! only by a scoped read of its own `qa_runs` table. **That is not available
//! here and the difference is not stylistic**: the resolving read is a
//! cross-gear call to qa-runs, so a token minted in this gear would attest to
//! an answer this gear did not compute. The obligation is therefore stated on
//! `ResultsRepository::upsert_run_results` and enforced by the ingest service.
//!
//! # Three methods return `bool`, for two different reasons
//!
//! [`NotifyRepository::claim_notification`] returns `bool` because **the insert
//! is the answer**: it reports which of two racing callers won a send-once
//! claim, and no read-then-write pair can express that. Its doc carries the
//! full argument.
//!
//! [`SavedViewsRepository::delete`] and [`JiraRepository::resolve_bug`] return
//! `bool` for the ordinary reason — the statement matched a row, or it did not
//! — and, as in qa-runs, the repository deliberately does not say *why* it did
//! not. It cannot: distinguishing "gone", "never existed" and "not yours" would
//! mean guessing, and a classification that guessed would be worse than a
//! `bool` that does not.
//!
//! # Implemented by `infra::storage`, as of Task 12
//!
//! All six have exactly one implementation — `Orm*Repository` in
//! `crate::infra::storage::{results,collect,saved_views,jira,notify,watermark}_sea_repo`.
//!
//! Three of these docs carry corrected notes worth reading before building on
//! them, because in each case an earlier version of the doc was confidently
//! wrong:
//!
//! * [`ResultsRepository::list_for_universe`] — the ordering is **four** keys,
//!   `sort_key DESC, created_at DESC, ingest_ordinal DESC, id DESC`, and the
//!   middle two are each load-bearing: `created_at` reproduces the *across-run*
//!   half of legacy's global `SERIAL` tiebreak and `ingest_ordinal` the
//!   *within-run* half. This was an open question, then closed by adding the
//!   column (option A, 2026-08-20), and then briefly regressed when `45dafe9b`
//!   dropped `created_at` and claimed equivalence anyway. Do not reduce it to
//!   three keys.
//! * [`ResultsRepository::latest_per_test`] and
//!   [`ResultsRepository::ingested_run_ids_between`] — both reduce in SQL through
//!   `SecureSelect::project_all`. Task 12 first shipped them as in-memory folds
//!   and wrote, *as corrections to these traits*, that `SecureSelect` could not
//!   project, group or de-duplicate. It can. Those sentences are gone; the
//!   methods' docs record how they came to be written, because "the wrapper
//!   cannot do X" is the shape of claim this layer keeps getting wrong.
//!
//! # The two JIRA configuration singletons, placed by Task 32
//!
//! This section used to be headed "What this module does not have" and said no
//! repository read `qa_jira_config` or `qa_jira_poller_config`, leaving Task 32
//! to decide where they belonged. **[`JiraRepository`] is where they belong**,
//! and it now has nine methods: five about bugs and four about the two
//! singletons. [`NotifyRepository`] is the precedent — it already owns
//! `qa_notification_config` beside the claims and the log — and that trait's own
//! header carries the full argument against a separate settings repository.
//!
//! The consequence for Task 35 is stated there too and is repeated here because
//! this is the module a reader looking for a poller-config reader will open
//! first: [`JiraRepository::get_poller_config`] **is** that reader.

mod collect_repo;
mod jira_repo;
mod notify_repo;
mod results_repo;
mod saved_views_repo;
mod watermark_repo;

pub use collect_repo::CollectRepository;
pub use jira_repo::JiraRepository;
pub use notify_repo::{NewLogEntry, NotificationClaim, NotifyRepository};
pub use results_repo::{
    FileStatusCount, FlakyGroup, NewTestCaseResult, NewTestResult, PlanExecRow, ResultsRepository,
    RunStatusCount, StatusRowCount,
};
pub use saved_views_repo::{SavedViewKey, SavedViewsRepository};
pub use watermark_repo::{WatermarkKind, WatermarkRepository, Watermarks};
