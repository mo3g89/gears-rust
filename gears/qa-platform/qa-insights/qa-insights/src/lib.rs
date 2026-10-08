//! qa-insights gear: the QA platform's read side.
//!
//! Ingests finished run and test results from qa-runs, keeps the per-test
//! history behind the dashboard, coverage and analytics surfaces, and owns the
//! JIRA bug loop and the notification egress.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.5 `cpt-cf-qa-component-insights`). The contract lives next door in
//! `qa-insights-sdk`.
//!
//! # What exists as of Task 40 — the gear is fully wired
//!
//! The schema and its migrations, eleven `SeaORM` entities, six repositories,
//! the ingest projection, the reconcile sweep, the leader elector, the JIRA
//! loop, the notification surface, the REST tier — and, since Task 40, a
//! composition root with **no unwired gaps left**.
//!
//! **This crate answered no request until Task 16 and now answers five**, every
//! one of them registered in `api::rest::routes::register_operations`:
//!
//! | Path | Task | Surface |
//! |---|---|---|
//! | `POST /qa/v1/insights/rebuild` | 16 | the operator replay of a closed window |
//! | `GET /qa/v1/test-results` | 17 | the file-level `OData` collection |
//! | `GET /qa/v1/test-case-results` | 17 | the function-level `OData` collection |
//! | `GET /qa/v1/dashboard` | 18 | legacy's `api_dashboard` aggregate |
//! | `GET /qa/v1/dashboard/coverage` | 19 | legacy's `api_coverage` **shape**, which answers an empty array in every deployment — `domain::service::dashboard`'s header holds the two missing upstreams |
//!
//! # The runtime, as of Task 40
//!
//! `init` looks up **five** cross-gear clients in `ClientHub` — `authz-resolver`,
//! qa-runs, qa-catalog, qa-environments and oagw — and a deployment must
//! register all five. It also resolves the database and the config, builds the
//! services aggregate the transport layer and the tickers share, and registers
//! this gear's own `QaInsightsClientV1`, the skip-list provider qa-runs' launch
//! path can call.
//!
//! **A sixth, optional client lived here from Task 3a until it was deleted.**
//! `init` used to also resolve an `EventBrokerApi` for a transactional consumer,
//! tolerating a missing registration with a `WARN` rather than failing to boot.
//! No deployment ever registered that client — the `event-broker` gear is a
//! skeleton that registers none (`event-broker/src/module.rs`; handler bodies
//! land with #4346) — and qa-runs deleted the publisher that would have fed it,
//! so the consumer, the lookup and the `event-broker`/`event-broker-sdk`
//! dependencies were code that could not execute. `gear.rs`'s header carries the
//! full history.
//!
//! `serve` — the `stateful` capability's lifecycle entry — hosts the three
//! **leader-elected tickers** (`qa-insights-reconciler`,
//! `qa-insights-jira-poller`, `qa-insights-collect`), each switchable by its own
//! interval where `0` disables, all three under `enable_tickers`. The reconcile
//! ticker is this gear's **only** ingest path, and has been since no deployment
//! ever registered the broker client above.
//!
//! **Every `// wired in Task N` gap `gear.rs` carried from Task 9 is closed.**
//! One thing is shipped and unconsumed on purpose: the skip list is servable
//! and unserved (`domain::jira`'s header, "No caller exists yet"). This list
//! used to name a second, the Slack egress, bound but undeliverable, which
//! the third review pass closed: `infra::notify::slack_oagw` now resolves the
//! tenant's webhook through credstore and delivers it over oagw.
//!
//! **This list said three, and the third was the run-completed notification.**
//! `notify::NotifyService::notify_run_completed` now has a production caller:
//! `domain::service::reconcile::ReconcileService::reproject` calls it once a
//! run's projection has committed, which is the only terminal transition this
//! gear observes. The claim table is seeded at upgrade time
//! (`infra::storage::migrations::m20260929_000003_seed_run_completed_notification_claims`)
//! so the first rebuild after the deploy does not mail a deployment's whole
//! history. What is *still* unconsumed is narrower and is recorded where it
//! belongs rather than here: nothing sources a `run.queue_expired` or
//! `schedule.fired` event, so `domain::notify::routing`'s other three arms
//! have no caller — `domain::service::mod`'s header, "`notify_run_completed`
//! has a producer; the three *other* alerts still do not".

pub mod api;
pub mod config;
pub(crate) mod domain;
pub mod gear;
/// The GTS permission catalog (`AuthzPermissionV1` instances) — review
/// finding #1.
pub mod gts;
pub(crate) mod infra;

pub use gear::QaInsights;

/// Needed by `tests/ingest_idempotence.rs`, which asserts this gear's own
/// migration set is what the in-memory database under test was built from.
pub use infra::storage::migrations::Migrator;

// === `domain` and `infra` are crate-internal — review finding #38 ===
//
// Both used to be `pub mod`, which made every SeaORM entity, every repository
// trait and every service struct part of this crate's public API: a consumer
// could name `qa_insights::infra::storage::entity::*` and pin itself to this
// gear's schema. Only `gear` (and the SDK) is the contract. The precedent for
// the shape is `gears/system/oagw/oagw/src/lib.rs:16-17`; `qa-catalog` and
// `qa-environments` landed it first.
//
// The `pub use` above is the one exception the compiler named — an item the
// integration-test crate in `tests/` genuinely needs. That test compiles as a
// separate crate, so `pub(crate)` would otherwise break it, and an integration
// test is a real consumer rather than a visibility inconvenience.
//
// **It did not land here as a visibility-only change.** An earlier version of
// this comment recorded 46 newly-dead groups and said the bulk of them were
// the notification subsystem, "built, tested and *unwired*". That measurement
// is superseded: the run-completed producer landed
// (`domain::service::reconcile::ReconcileService::reproject` calls
// `NotifyService::notify_run_completed`), and re-measuring afterwards reports
// **20 groups**, not 46 — routing's and render's run-completed halves are
// reached from production now. Each of the 20 was adjudicated rather than
// allowed:
//
// * **Seven deleted, with their tests.** `JiraClient::get_issue` (a port
//   method whose own doc said it existed "so a later consumer does not have to
//   reopen the adapter" — the adapter's private `fetch_issue` still makes the
//   call, and `check_status` is still its projection);
//   `SavedViewsRepository::find_by_natural_key` and its `SavedViewKey`, a
//   probe `domain::service::saved_views` argues at length must *not* be used
//   because it opens a TOCTOU window the repository's unique-violation
//   mapping does not have; `domain::service::ingest`'s `classify_all` and
//   `ResultCounts`, whose forecast first caller reused `classify` instead;
//   `infra::leader::ClaimRowElector::holder`; and the `FakeRuns` page-cap knob
//   in `domain::service::test_support` that no test ever turned.
// * **Seven kept with a per-item `#[allow(dead_code, reason = …)]`, on
//   settled grounds.** `domain::metrics`' `COUNTERS` and `DURATIONS` and the
//   four `domain::ports::metrics` `ALL` catalogs are declared sets that are
//   their own oracles — `qa-catalog` and `qa-environments` keep theirs the
//   same way — and `domain::jira::registry::render_skip_list` is the frozen
//   `SKIP_TESTS_WITH_BUGS` wire format whose missing caller `domain::jira`'s
//   header states.
// * **Four escalated; all four have since been deleted on the owner's
//   ruling.** `domain::notify::routing`'s `Queued`, `QueueExpired` and
//   `ScheduledRun` arms, `Event::scheduled_run` and the two
//   `NotificationKind` values they key are **gone**, with the four tests that
//   existed only to cover them: nothing sources a `run.queue_expired` or a
//   `schedule.fired` event, the transactional broker consumer that would have
//   was itself deleted as dead code, and a suite that keeps passing over
//   deleted-adjacent code reports coverage of a decision nothing makes.
//   Re-wiring them is a cross-gear feature with a producer, and
//   `domain::notify::routing`'s header holds the legacy citations that work
//   would start from. The fourth, `WatermarkKind::SweptAt`, went with
//   `Watermarks::last_swept_at` and its column: the stale-in-progress sweep it
//   was to record never existed, and `m20260929_000005` drops the column.
// * **One marked `#[cfg(test)]`** — `StatusCategory::as_str`, read only where
//   a test asserts what an instance answered.
// * **One was an unused facade re-export** in `domain::ports`, narrowed to
//   what is named rather than allowed.

/// **No `domain` module imports the `api` layer.** `api` is a transport over
/// `domain`, and the dependency may not run the other way. A structural guard
/// rather than a `cargo gears lint` rule -- see the module's own header for
/// why that CLI cannot express this one. Review findings #15, #16, #39.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "no_api_in_domain_tests.rs"]
mod no_api_in_domain_tests;

/// Source-level invariants no compiler or lint states, such as a debug assertion
/// whose `?`, `.await` or `&mut` vanishes in release builds.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod source_hygiene_tests;

/// Every migration name this gear's comments cite is live, or says it was
/// folded away. One implementation shared with the other qa gears, kept in
/// `qa-runs` where it started; see its header.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../../qa-runs/qa-runs/src/migration_citations_tests.rs"]
mod migration_citations_tests;
