//! Local (in-process) implementations of this gear's SDK client traits.
//!
//! Adapts the domain layer to the object-safe SDK traits for registration in
//! `ClientHub`: `AppServices` to `QaCatalogClientV1`, and
//! `domain::service::QaProductRegistry` to `QaProductPluginResolverV1`.

mod client;
mod plugin_resolver;

pub use client::QaCatalogLocalClient;
pub use plugin_resolver::QaProductPluginResolverLocalClient;
