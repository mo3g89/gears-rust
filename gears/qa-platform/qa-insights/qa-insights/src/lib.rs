//! qa-insights gear: the QA platform's read side.
//!
//! Ingests finished run and test results from qa-runs, keeps the per-test
//! history behind the dashboard, coverage and analytics surfaces, and owns the
//! JIRA bug loop and the notification egress.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-insights`). The contract lives next door in
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
pub mod infra;

pub use gear::QaInsights;
