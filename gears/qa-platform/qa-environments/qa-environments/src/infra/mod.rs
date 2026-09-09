/// The `OpenTelemetry`-backed adapter for `domain::ports::metrics` — one
/// instrument per family in `domain::metrics`, built at gear init and handed
/// to `EnvironmentsService` as an `Arc<dyn ObservationMetrics>`.
///
/// Not feature-gated: it pulls no Kubernetes client in, and a gear that
/// configures no telemetry pipeline still builds it and still emits, into the
/// process-global no-op provider. See the module's own header.
pub mod metrics;
pub mod product_plugin;
pub mod storage;

/// The only place in this crate that builds a Kubernetes client, and since
/// Task 19b the only thing that needs one: decision **D4**'s runner-`Secret`
/// writer.
///
/// Behind the non-default `runner-secret` cargo feature — renamed from
/// `platform-observation` by ruling F-19, because after Task 19 it gates a
/// `Secret` writer and nothing observational, and a feature named for work it
/// no longer does is this branch's signature defect in cargo form. See
/// ADR-0001's waiver amendment (2026-08-28), whose condition — that `domain/`
/// never learns Kubernetes exists — still holds.
#[cfg(feature = "runner-secret")]
pub mod runner_secret_errors;
#[cfg(feature = "runner-secret")]
pub mod runner_secret_writer;
