//! Adapters for the [`RunExecutor`](crate::domain::ports::run_executor::RunExecutor)
//! port.
//!
//! Two, and only one of them is compiled by default.
//!
//! * [`mock`] — the p1 adapter, and the default. Deterministic, in-memory, and
//!   the thing every non-execution test in this crate runs against.
//! * [`argo`] — Argo Workflows on Kubernetes, behind the **non-default `argo`
//!   cargo feature**. It exists because ADR-0001's execution slice is unstarted
//!   — `gears/serverless-runtime/` is docs-only — so without it every run in
//!   every deployment reports one fabricated passing test. Its presence is a
//!   recorded, scoped exception to that ADR and to
//!   `cpt-cf-qa-constraint-no-kube`; read the waiver
//!   (`docs/ADR/0001-cpt-cf-qa-adr-serverless-execution.md`, "Waiver,
//!   2026-08-27") before extending it.
//!
//! The port itself stays frozen (`DESIGN.md:163`). The serverless-runtime
//! adapter is still feature 2.7 and still the intended endpoint; it is the
//! reason the port was shaped from the source system's contract rather than from
//! whatever either of these two finds convenient.
//!
//! Which one the gear wires is [`crate::config::ExecutorKind`], read once in
//! `crate::gear`'s `init`.

#[cfg(feature = "argo")]
pub mod argo;
pub mod mock;
