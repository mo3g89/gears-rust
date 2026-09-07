//! **Where a REST handler imports its error rendering from.** A re-export
//! surface; the rendering itself lives in `domain`.
//!
//! `impl From<DomainError> for CanonicalError` — the exhaustive
//! [`DomainError`](crate::domain::error::DomainError) mapping, the
//! [`RunResourceError`] type it raises and the `opaque_internal` redaction
//! helper — is in [`crate::domain::error`], next to the enum it maps, and the
//! two call-site wrappers are in [`crate::domain::error_attribution`]. Both
//! modules carry the reasoning that used to be in this header, beside the code
//! it is about.
//!
//! # Why the mapping is not here
//!
//! It was, and that put every `?` and every `other => other.into()` in a
//! *domain* module in the position of calling a function in the transport
//! layer. `domain::local_client` is an in-process call that never touches HTTP;
//! `api` is a transport over `domain` rather than the other way round. Review
//! findings #15, #16.
//!
//! **The `use` line was not the finding.** An earlier round deleted
//! `domain::error_attribution`'s `use crate::api::rest::error::...` and left the
//! edge standing, because a trait impl is resolved by coherence rather than by
//! an import: `other.into()` went on resolving into this file.
//! `crate::no_api_in_domain_tests` is a text scan over imports and could never
//! have contradicted that — it pins the imports, and the impl's location is what
//! pins the rest.
//!
//! A handler is unaffected: it imports the run resource type and the two
//! wrappers from this one module, as it always did.

pub(crate) use crate::domain::error::RunResourceError;
pub(crate) use crate::domain::error_attribution::{as_queue_error, as_schedule_error};
