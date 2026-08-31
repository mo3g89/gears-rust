pub mod storage;

/// The only place in this crate that builds a Kubernetes client. Behind the
/// non-default `platform-observation` cargo feature; see the module's own
/// docs and ADR-0001's waiver amendment (2026-08-28).
#[cfg(feature = "platform-observation")]
pub mod observer;
