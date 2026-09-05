//! Canned API-server answers shared by this crate's two test modules.
//!
//! `observe_tests` drives the reads directly and `conformance_tests` drives
//! them through `assert_no_leak`, but both need a cluster that answers, and
//! both need it to answer the same three ways: with the object, without it, or
//! by refusing.
//!
//! # What deliberately does *not* live here
//!
//! Any fixture carrying a canary. `conformance_tests` plants markers in the
//! `ConfigMap` bodies and asserts they reach no surface; `observe_tests`
//! asserts on the values those same bodies produce. One shared body would have
//! to serve both, and the first edit that suited one would silently weaken the
//! other. Everything below is canary-free by construction, which is why it can
//! be shared at all.

use qa_plugin_k8s::test_support::{StubConfigMap, api_error_body, not_found_body};

/// One canned answer: an HTTP status and a body.
pub type Answer = (u16, Vec<u8>);

/// The API server's own words when it refuses a read.
///
/// Reaches [`qa_product_sdk::observation::PluginFailure::remote_message`], the
/// one `String` sanctioned to cross the plugin boundary — which is exactly why
/// it carries no canary. A marker here would assert about the SDK's carrier
/// rather than about this plugin.
pub const REFUSED: &str = "configmaps is forbidden: User cannot list resource in API group";

/// A read the API server refuses.
pub fn refused() -> Answer {
    (403, api_error_body(403, "Forbidden", REFUSED))
}

/// A `ConfigMap` the API server answered about and does not have.
pub fn absent() -> Answer {
    (404, not_found_body("the ConfigMap was not found"))
}

/// An all-namespace scan that matched nothing.
pub fn scan_found_nothing() -> Answer {
    (200, StubConfigMap::list_body(&[]))
}
