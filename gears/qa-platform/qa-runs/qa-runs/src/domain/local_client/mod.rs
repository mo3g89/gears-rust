//! In-process `QaRunsClientV1`, registered in `ClientHub` at gear init.
//!
//! The one seam other gears reach this one through - qa-insights' auto-rerun
//! is the named consumer. It lives under `domain` rather than `api` because it
//! speaks SDK models and `SecurityContext`, with no transport of its own.

mod client;

pub use client::QaRunsLocalClient;
