//! Infrastructure layer: storage, cross-gear clients, and the egress adapters.
//!
//! * `storage` — the schema and its migrations (Task 10); `SeaORM` entities
//!   and repositories, Tasks 11-12. Carries one migration for a table an
//!   ingest path used to own — see `storage::migrations::m20260818_000002_offset_store`'s
//!   header.
//! * `leader` — the elector the three tickers hold. Created by Task 15,
//!   because the reconciler's contract is "only one instance sweeps"; the three
//!   tickers that call it arrived at Task 40, in `crate::gear`'s `serve`.
//! * `clients` — the adapters over the sibling gears' SDK clients that
//!   implement this gear's outbound ports. Task 16, for the qa-runs reads.
//! * `clock` — the one-line adapter behind
//!   [`Clock`](crate::domain::ports::Clock), Task 22. Egress in the same sense
//!   `clients` is: reading the system clock is the thing the analytics folds are
//!   isolated from.
//!
//! * `jira` — the outbound JIRA calls behind
//!   [`JiraClient`](crate::domain::ports::JiraClient), over `oagw`. Task 32, and
//!   the first *true* egress adapter here: everything above either reads a
//!   sibling gear through its SDK client or reads the process's own clock. Its
//!   header opens with why `reqwest` does not belong in this crate.
//!
//! The plan's file map lists no `clients` submodule; `jira` and `notify` are the
//! two egress adapters it does name, and this is the same shape for an ingress
//! one. See that module's header.
//!
//! * `metrics` — the `OpenTelemetry`-backed adapter behind
//!   [`CollectMetrics`](crate::domain::ports::metrics::CollectMetrics) and
//!   [`JiraPollMetrics`](crate::domain::ports::metrics::JiraPollMetrics). Egress
//!   in the loosest sense of the three above: it writes into the process-global
//!   meter provider, which a deployment's telemetry configuration may or may
//!   not have pointed anywhere. That it is safe to build and emit through when
//!   it has not is the whole of its module header's last section.
//!
//! * `notify` — the outbound Slack and email adapters behind
//!   [`SlackClient`](crate::domain::ports::SlackClient) and
//!   [`MailClient`](crate::domain::ports::MailClient), over `oagw` and D10's
//!   inert answer respectively. Task 39.

pub mod clients;
pub mod clock;
pub mod jira;
pub mod leader;
pub mod metrics;
pub mod notify;
pub mod storage;
