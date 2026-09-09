//! SSH transport for QA Platform product plugins whose target is a host.
//!
//! A **connector**, not a plugin: no gear, no GTS identity, nothing resolves
//! it at runtime. Product plugins link it the way they link any dependency,
//! the same arrangement `qa-connector-k8s` has for cluster-targeted products.
//!
//! # Where the code came from
//!
//! [`agent`] is moved from `qa-catalog/src/infra/git/ssh_agent.rs`, which
//! introduced the technique on 2026-08-27 and, until this move, also owned the
//! host-key policy. Its own doc claimed that policy was expressed in exactly
//! one place; a second copy here would have made that false, so the code moved
//! rather than being duplicated, and `qa-catalog` now depends on this crate.

pub mod agent;
pub mod errors;
pub mod session;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use agent::{HOST_KEY_VERIFICATION_OPTIONS, SshAgent};
pub use errors::{SshFailure, classify};
pub use session::{DEFAULT_TIMEOUT, SshSession, SshTarget};
