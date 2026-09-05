//! Local (in-process) implementation of `QaEnvironmentsClientV1`.
//!
//! Adapts `domain::service::AppServices` to the object-safe SDK client trait
//! for registration in `ClientHub`.

mod client;

pub use client::QaEnvironmentsLocalClient;
