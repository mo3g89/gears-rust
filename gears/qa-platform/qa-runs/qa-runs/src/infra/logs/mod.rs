//! Live log fan-out for the SSE endpoint Task 16 exposes.
//!
//! Infrastructure, not domain: it holds live channel handles rather than
//! values, which is the same class `domain::ports::run_executor` exempts from
//! `#[domain_model]` for [`ExecutionStream`](crate::domain::ports::run_executor::ExecutionStream)
//! and `ExecutionSink`. The domain side of the seam is
//! `domain::service::LogFanout`, the two-method trait this module implements. It
//! lives in the composition tier rather than beside any one consumer because it
//! has several — see that trait's own doc, which records why an earlier
//! placement beside `service::ingest` stopped being accurate.
//!
//! # What this replaces, and where it diverges
//!
//! The source system has no fan-out at all: its log socket **polls** Argo once a
//! second per connected client and diffs the whole log against the last string
//! it sent (`../testrunner/manager/src/routes/runs.rs:1477-1527`). That is a
//! fixed-size buffer per client and no shared state, so it cannot leak — and it
//! also cannot deliver a line the poll interval missed, and it re-fetches the
//! entire log per client per second.
//!
//! This module pushes instead, which is what an
//! [`ExecutionEvent::Log`](crate::domain::ports::run_executor::ExecutionEvent)
//! stream makes possible. The cost of pushing is that a slow consumer now has
//! somewhere to accumulate, so the channel is **bounded** and a subscriber that
//! falls behind is told so explicitly rather than silently skipped. See
//! [`RunLogBroadcaster`].

pub mod archive;
mod broadcast;

pub use archive::RunLogArchive;
pub use broadcast::{
    DEFAULT_LOG_CHANNEL_CAPACITY, LogSubscription, MAX_RETAINED_RUNS, MAX_SUBSCRIBERS_PER_RUN,
    RunLogBroadcaster, gap_marker,
};
