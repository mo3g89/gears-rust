//! REST transport layer: DTOs, canonical error mapping, handlers, and routes.
//!
//! ## Layering
//!
//! - `dto` — wire types (serde + utoipa). Never used by the SDK or domain layers.
//! - `error` — `From<DomainError> for CanonicalError`, the single place that
//!   decides HTTP-visible error shape.
//! - `handlers` — thin request/response glue. No business logic, no DB, no PEP.
//! - `routes` — `OperationBuilder` registrations under `/qa/v1/...`.

pub mod dto;
pub mod error;
pub mod handlers;
pub mod routes;
