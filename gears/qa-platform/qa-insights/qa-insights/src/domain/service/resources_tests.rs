//! Each `resources::*` descriptor carries its own sibling `*_NAME` const.
//!
//! # The hole this closes
//!
//! The `*_NAME` refactor put the PDP string one hop away from
//! `ResourceType::from_static`, and nothing else in the subsystem pins the two
//! ends of that hop together. A descriptor wired to the *wrong* sibling —
//! `PLAN: ResourceType = ResourceType::from_static(BUNDLE_NAME, ...)` — sends
//! the wrong string to the PEP **and** the same wrong string to
//! `crate::gts::permissions` through
//! [`super::authz_surface::ENFORCED`], consistently. So every anti-drift guard
//! this subsystem has stays green: `authz_surface_tests.rs` reads the literal
//! the descriptor was built from, not the descriptor's identity, and
//! `permissions_tests` compares the catalog to `ENFORCED`. Between two resource
//! types with identical action sets the mistake is completely silent, and what
//! ships is a policy surface addressing the wrong table.
//!
//! One `assert_eq!` per resource type closes it, against
//! `authz_resolver_sdk::pep::ResourceType::name()` — the accessor that reads
//! back the very string `from_static` was handed
//! (`gears/system/authz-resolver/authz-resolver-sdk/src/pep/enforcer.rs:210`).
//!
//! # Why this file is per-gear and not in `authz_surface_tests.rs`
//!
//! `authz_surface_tests.rs` is byte-identical across all four QA Platform
//! gears and `authz_surface_parity_tests.rs` enforces that, so it cannot name
//! a gear's own resource types. The assertions below are exactly the
//! gear-specific half, which is why they live beside the `resources` module
//! they are about rather than inside the shared scanner.
//!
//! Review finding #1, final fix wave (Minor 1).

use super::resources;

/// Every descriptor was built from its own `*_NAME` const, not a sibling's.
#[test]
fn every_resource_descriptor_carries_its_own_name_const() {
    assert_eq!(
        resources::TEST_RESULT.name(),
        resources::TEST_RESULT_NAME,
        "resources::TEST_RESULT was built from the wrong *_NAME const",
    );
    assert_eq!(
        resources::SAVED_VIEW.name(),
        resources::SAVED_VIEW_NAME,
        "resources::SAVED_VIEW was built from the wrong *_NAME const",
    );
    assert_eq!(
        resources::JIRA_CONFIG.name(),
        resources::JIRA_CONFIG_NAME,
        "resources::JIRA_CONFIG was built from the wrong *_NAME const",
    );
    assert_eq!(
        resources::JIRA_BUG.name(),
        resources::JIRA_BUG_NAME,
        "resources::JIRA_BUG was built from the wrong *_NAME const",
    );
    assert_eq!(
        resources::NOTIFICATION_CONFIG.name(),
        resources::NOTIFICATION_CONFIG_NAME,
        "resources::NOTIFICATION_CONFIG was built from the wrong *_NAME const",
    );
}
