//! Telemetry utilities for OpenTelemetry integration
//!
//! This gear provides utilities for setting up and configuring
//! OpenTelemetry tracing layers for distributed tracing.

pub mod config;
pub mod init;
/// Pull-based metrics delivery — see the module docs for why it is a separate
/// listener and why it is independent of `metrics.enabled`.
#[cfg(feature = "otel")]
pub mod scrape;
pub mod throttled_log;

pub use config::{
    Exporter, HttpOpts, LogsCorrelation, MetricsConfig, MetricsScrapeConfig, OpenTelemetryConfig,
    OpenTelemetryResource, Propagation, Sampler, TracingConfig,
};
#[cfg(feature = "otel")]
pub use init::init_tracing;
pub use init::{init_metrics_provider, shutdown_tracing};
pub use throttled_log::ThrottledLog;
