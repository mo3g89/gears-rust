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
