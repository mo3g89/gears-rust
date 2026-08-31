#![cfg(feature = "platform-observation")]
//! The only place in this crate that builds a Kubernetes client or reads
//! from a platform's cluster.
//!
//! ADR-0001 forbids `kube`/`k8s-openapi` in any qa-platform crate; the waiver
//! amendment of 2026-08-28 permits this adapter, gated behind the
//! non-default `platform-observation` cargo feature, on the condition that
//! `domain/` never learns Kubernetes exists. It doesn't: this module calls
//! [`crate::domain::observation`]'s pure parsing rules — it never
//! reimplements them — and returns [`crate::domain::ports::platform_observer::ObservationOutcome`],
//! a plain value.
//!
//! Ported from `manager/src/services/platforms.rs`'s `detect_platform_version`
//! and `detect_base_domain` (vhp-testrunner, the legacy source of truth).

mod errors;
mod kube_observer;
mod secret_writer;

pub use kube_observer::KubeObserver;
