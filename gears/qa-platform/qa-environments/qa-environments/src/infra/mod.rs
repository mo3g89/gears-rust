/// The `OpenTelemetry`-backed adapter for `domain::ports::metrics` — one
/// instrument per family in `domain::metrics`, built at gear init and handed
/// to `EnvironmentsService` as an `Arc<dyn ObservationMetrics>`.
///
/// Not feature-gated: it pulls no Kubernetes client in, and a gear that
/// configures no telemetry pipeline still builds it and still emits, into the
/// process-global no-op provider. See the module's own header.
pub mod metrics;
pub mod product_plugin;
/// [`crate::domain::ports::RunnerPodResolver`] over qa-runs'
/// `resolve_runner_pod`. Not feature-gated: it is a `ClientHub` lookup and
/// pulls no Kubernetes client in.
pub mod runner_pod_resolver;
pub mod storage;

/// Finding #94's `TokenReview` + `GET pod` verifier, behind the non-default
/// `runner-credentials` feature for ADR-0001's reason: it is the second
/// place this crate builds a Kubernetes client.
#[cfg(feature = "runner-credentials")]
pub mod pod_identity;
