//! SDK for the `qa-insights` gear: the QA platform's read side.
//!
//! `qa-insights` ingests finished run and test results from `qa-runs`, keeps the
//! per-test history behind the dashboard, coverage and analytics surfaces, and
//! owns the JIRA bug loop and the notification egress. It is the fourth and last
//! domain gear of the subsystem (see `gears/qa-platform/docs/DESIGN.md` §3.2
//! `cpt-cf-qa-component-insights`).
//!
//! Public contract: client trait, transport-agnostic models, canonical errors.
//!
//! # Contract purity
//!
//! This crate must stay free of `serde`, `utoipa` and `http`, exactly as its
//! three shipped siblings do. That is enforced by review and the per-task grep,
//! **not** by a lint: `de0101_no_serde_in_contract` and
//! `de0102_no_toschema_in_contract` are in `Gears.toml`'s dylint *skip* list
//! rather than satisfied (`qa-runs-sdk/src/lib.rs:4-7`). Wire shapes belong to
//! the gear's `api/rest` layer, which owns its own DTOs.
//!
//! # Scope of the client trait
//!
//! [`QaInsightsClientV1`] has exactly one method, because exactly one thing here
//! is called by another gear: the skip list qa-runs needs at launch. Everything
//! else this gear does — ingest, analytics, saved views, settings, the bug
//! endpoints, notifications — is REST-only, including the runner's collect
//! report, which arrives over HTTP at `VHP_COLLECT_URL` and is therefore a route
//! rather than an SDK call. An SDK method with no cross-gear caller is pure
//! overhead, which is the argument `cpt-cf-qa-adr-four-gear-decomposition` uses
//! against its six-gear option.
//!
//! The models are wider than the trait on purpose: this subsystem's gears use
//! their own SDK model types as domain types (see, for example,
//! `qa-environments/src/domain/service/platforms.rs`), so `models` is the gear's
//! vocabulary and not only its wire contract.

mod client;
mod errors;
mod models;

pub use client::QaInsightsClientV1;
pub use errors::QaInsightsError;
pub use models::{
    CollectCount, CoverageBuild, CoverageSummary, DailyStatusPoint, DashboardRun, DashboardStats,
    FailedTestCard, FlakyTestCard, JiraBug, JiraConfig, JiraPollerConfig, NewJiraBug, NewSavedView,
    NotificationConfig, NotificationLogEntry, PlatformBrief, PlatformsSummary,
    QualityVectorPassRate, RunTestTrendPoint, SavedView, SavedViewScope, ScheduledRunSlackTemplate,
    ScheduledRunSlackTemplates, SkipListEntry, TestCaseResultRecord, TestResultRecord,
};
