//! qa-runs SDK: transport-agnostic contract for the run orchestrator.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-runs`). Contract purity: this crate must stay
//! free of `serde`, `utoipa`, and `http` — enforced by review and the
//! per-task grep, NOT by a lint: `de0101_no_serde_in_contract` and
//! `de0102_no_toschema_in_contract` are in `Gears.toml`'s dylint skip list.

mod client;
mod errors;
mod models;

pub use client::QaRunsClientV1;
pub use errors::QaRunsError;
pub use models::{
    ExclusiveTier, LaunchOutcome, LaunchRequest, NewSchedule, QueueEntry, QueueState, Run, RunKind,
    RunParameter, RunResult, RunSource, RunState, RunTarget, RunTestResult,
    SLACK_NOTIFICATION_EVENTS, Schedule, ScheduleNotificationSettings,
};
