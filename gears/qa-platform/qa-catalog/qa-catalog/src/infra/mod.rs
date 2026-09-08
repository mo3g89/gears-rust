pub mod bundle_store;
pub mod fs;
pub mod git;
/// The `OpenTelemetry` adapter for `domain::ports::metrics`, plus the in-memory
/// probe the metric tests read exported series back through.
pub mod metrics;
pub mod storage;
