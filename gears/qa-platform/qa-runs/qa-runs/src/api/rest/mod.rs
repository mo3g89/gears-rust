//! REST transport: DTOs, canonical error mapping, handlers, and routes.
//!
//! ## Layering
//!
//! - `dto` — wire types (serde + utoipa) plus the validation the boundary owes
//!   before a request reaches a service. Never used by the SDK or the domain
//!   layer; the conversions here are the only bridge.
//! - `error` — `From<DomainError> for CanonicalError`, the single place that
//!   decides HTTP-visible error shape, and the single place that decides what
//!   text a caller is allowed to see.
//! - `handlers` — thin request/response glue. No business logic, no DB, no PEP.
//! - `routes` — `OperationBuilder` registrations under `/qa/v1/...`.
//!
//! - `sse` — the framing the live-log endpoint applies to another system's
//!   bytes before they reach an operator's browser.

pub mod dto;
pub mod error;
pub(crate) mod handlers;
pub(crate) mod routes;
pub mod sse;

/// The gear's route definitions with nothing bound to them, re-exported so the
/// `qa-platform-openapi` generator can register them into an `OpenAPI`
/// registry without an `AppServices`. `routes` itself stays `pub(crate)`.
pub use routes::register_operations;
