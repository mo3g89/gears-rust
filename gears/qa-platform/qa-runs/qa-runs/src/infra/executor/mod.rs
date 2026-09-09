//! Adapters for the [`RunExecutor`](crate::domain::ports::run_executor::RunExecutor)
//! port.
//!
//! Two, and only one of them is compiled by default.
//!
//! * [`mock`] — the p1 adapter, and the default. Deterministic, in-memory, and
//!   the thing every non-execution test in this crate runs against.
//! * [`argo`] — Argo Workflows on Kubernetes, behind the **non-default `argo`
//!   cargo feature**. It is the backend a real deployment runs on; without it
//!   every run in every
//!   deployment reports one fabricated passing test. The feature gate is what
//!   keeps `cpt-cf-qa-constraint-no-kube` true for a default build — read
//!   `docs/ADR/0001-cpt-cf-qa-adr-execution-plane.md` before widening it.
//!
//! The port itself stays frozen (`cpt-cf-qa-principle-executor-port`, DESIGN
//! §2.1): an adapter satisfies the four operations, it does not widen them.
//!
//! Which one the gear wires is [`crate::config::ExecutorKind`], read once in
//! `crate::gear`'s `init`.

#[cfg(feature = "argo")]
pub mod argo;
pub mod mock;
