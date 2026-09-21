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
//! Three things are shipped-and-unconsumed by deliberate, recorded decision
//! rather than by oversight, and each is a release-gate item with its own
//! home: the Slack egress is bound but undeliverable (R107 —
//! `infra::notify::slack_oagw`'s "Finding B"), the skip list is servable and
//! unserved (R74 — `domain::local_client`), and nothing yet routes a
//! `run.canceled` / `run.queue_expired` / `schedule.fired` event into
//! `notify::NotifyService::notify_run_completed` now that the transactional
//! broker consumer that would have owned that routing was deleted as dead
//! code (`domain::service::mod`'s header, "Still to come, at the end of
//! Phase C").

pub mod api;
pub mod config;
pub mod domain;
pub mod gear;
/// The GTS permission catalog (`AuthzPermissionV1` instances) — review
/// finding #1.
pub mod gts;
pub mod infra;

pub use gear::QaInsights;

// === `domain` and `infra` stay `pub` here — review finding #38, measured ===
//
// Finding #38 asks for `pub(crate) mod domain` / `pub(crate) mod infra` in all
// four gears, so SeaORM entities and repository traits stop being part of the
// crate's public API. It landed that way in `qa-catalog` and
// `qa-environments`. **It does not land here as a visibility-only change**,
// which is what the finding is scoped to.
//
// Measured rather than guessed: the change was made, the compiler run, and
// then reverted. With both modules `pub(crate)`, this crate reports 46 groups
// of newly-dead code — items nothing outside its own `#[cfg(test)]` modules
// reaches, which `pub mod` was keeping the compiler quiet about.
//
// The bulk of it is the notification subsystem — `domain::notify::routing`,
// `domain::notify::render`'s run-completed half, and the
// `NotifyService::notify_run_completed` path they serve — which this module's
// own header above already records as built, tested and *unwired*: no event
// source routes into it yet.
//
// None of that is a visibility question. Each item is a decision — delete it
// and the tests that cover it, or wire the feature.
//
// **Two ways of not making that decision were weighed and rejected.** The
// blunt one is an `#[allow(dead_code)]` over a whole subsystem: it trades a
// real signal for a green build, and it goes on hiding the next dead thing to
// land there. The sharp one is per-item `#[expect(dead_code, reason = "…")]`,
// which is already how this repo records a deliberately-unused item
// (`infra::storage::entity::mod`, `api::rest::dto`) and which keeps the
// signal, because `expect` starts warning the moment the item stops being
// dead. It was rejected here for one reason only: it is not a way to *defer*
// the decision. A `reason` written on each of these items records an
// adjudication nobody has made, and reads to the next reader as though one
// had. Where the follow-up's answer turns out to be "keep, deliberately
// unused", `#[expect(dead_code, reason = "…")]` is exactly what should land —
// it is that task's likely output, not a substitute for doing it.
//
// Making the modules private also turns every `pub(crate)` item inside them
// into a `clippy::redundant_pub_crate` error (83 sites here), which is denied
// repo-wide; that part is mechanical, the dead code is not.
//
// Left as its own task, with the count above as the size estimate.

/// **No `domain` module imports the `api` layer.** `api` is a transport over
/// `domain`, and the dependency may not run the other way. A structural guard
/// rather than a `cargo gears lint` rule -- see the module's own header for
/// why that CLI cannot express this one. Review findings #15, #16, #39.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "no_api_in_domain_tests.rs"]
mod no_api_in_domain_tests;
