//! The one place this gear elevates past the policy engine.
//!
//! # Why this exists
//!
//! Six of this gear's system-actor contexts are **nil-tenant**
//! (`system_actor::for_dispatch_enumeration`, `for_claim_reconciliation`,
//! `for_ttl_sweep`, `for_timeout_sweep`, `for_watch_scan`,
//! `for_schedule_tick`). A ticker holds no request, so it has no tenant to take
//! one from, and the sweep's whole job is to find work across every tenant.
//!
//! `static-authz-plugin` denies a nil-tenant request — correctly, since granting
//! unrestricted access to an unauthenticated caller is exactly what it is there
//! to stop. qa-platform previously discharged that denial by adding a
//! `system_grants` list to that shared plugin. That change has been reverted; the
//! elevation now lives here, in the gear that needs it, where it is one function
//! and one audit point instead of a config surface on a system gear every other
//! deployment also links.
//!
//! # Why this is not a security regression
//!
//! The elevation is **not reachable from a request**. Every caller is a ticker
//! that this gear spawns itself, and every one of them uses the scope only for a
//! cross-tenant **read** that enumerates which tenants have work. The writes that
//! follow are re-scoped per row under `system_actor`'s *tenant-bound* factories
//! (`for_dispatch`, `for_ttl_expiry`, `for_timeout_enforcement`,
//! `for_schedule_fire`), which is the pairing `system_actor`'s own module doc
//! already describes.
//!
//! This mirrors `account-management`, which does the same thing for the same
//! reason and says so: its hierarchy read port *"centralizes the
//! `AccessScope::allow_all()` trust elevation at a single named call site so this
//! gear no longer carries that concern"*
//! (`gears/system/account-management/account-management/src/tr_plugin/queries.rs`).

use toolkit_security::AccessScope;

/// The unrestricted scope a nil-tenant sweep enumerates with.
///
/// **Read paths only.** Pass a tenant-bound scope to anything that writes.
#[must_use]
pub fn enumeration_scope() -> AccessScope {
    AccessScope::allow_all()
}

#[cfg(test)]
mod tests {
    use super::enumeration_scope;

    /// The seam returns an unrestricted scope.
    ///
    /// Asserted through the secure-ORM's own predicate on the scope rather than a
    /// `Debug` string: the string is not a contract and would pass with a scope
    /// that merely *prints* like allow-all.
    #[test]
    fn the_enumeration_scope_is_unrestricted() {
        assert!(
            enumeration_scope().is_unconstrained(),
            "the sweep seam must be unrestricted; a clamped scope silently returns \
             only one tenant's rows and the sweep then does nothing for every other"
        );
    }
}
