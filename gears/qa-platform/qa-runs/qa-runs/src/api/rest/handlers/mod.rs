//! Request/response glue. No business logic, no database access, no policy
//! decisions — every handler here delegates to `domain::service::AppServices`
//! and converts at the boundary.
//!
//! The one handler that is more than glue is `runs::stream_run_logs`, and its
//! extra work is all *refusal*: the authorization it must perform before
//! subscribing, and the framing it must apply to what it relays. Both are
//! documented on that function.

// `pub` items inside a `pub(crate)` module: the module declaration in
// `api::rest` is what keeps these out of the crate's public API, and repeating
// `pub(crate)` here is what `clippy::redundant_pub_crate` denies.
pub mod queue;
pub mod runs;
pub mod schedules;
