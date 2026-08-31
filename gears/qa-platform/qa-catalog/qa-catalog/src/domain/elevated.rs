//! The one place this gear elevates past the policy engine.
//!
//! # Why this exists
//!
//! Two of this gear's system-actor contexts are **nil-tenant**:
//! [`system_actor::for_branch_refresh_enumeration`] and
//! [`system_actor::for_bundle_gc`]. Both back a lifecycle ticker (`crate::gear`)
//! that runs on a schedule rather than in response to a request, so neither has
//! a caller-supplied tenant to build a scope from, and each one's whole job is
//! to find candidates across every tenant before doing anything tenant-scoped.
//!
//! `static-authz-plugin` denies a nil-tenant request — correctly, since
//! granting unrestricted access to an unauthenticated caller is exactly what
//! it is there to stop. qa-platform previously discharged that denial by
//! adding a `system_grants` list to that shared plugin. That change has been
//! reverted; the elevation now lives here, in the gear that needs it, where it
//! is one function and one audit point instead of a config surface on a system
//! gear every other deployment also links.
//!
//! # Why this is not a security regression
//!
//! The elevation is **not reachable from a request**. Both callers are
//! lifecycle tasks this gear spawns itself, and each uses the scope only for a
//! cross-tenant **read**: the branch-cache refresher's `(repo, tenant)` target
//! listing, and the bundle GC's listing of tenants with expired bundles
//! (`domain::service::bundles::BundlesService::tenants_with_expired_bundles`).
//! Every *write* that follows is re-scoped per row under `system_actor`'s
//! *tenant-bound* factories — `for_branch_refresh`, minted from each target's
//! own `tenant_id`, and `for_bundle_delete`, minted from each tenant the GC
//! enumeration answered with — and each of those still goes through the PEP
//! exactly as any other tenant-bound call in this gear does. The bundle GC's
//! atomic select+delete (`BundlesRepository::delete_expired`) stays inside one
//! transaction per tenant; only the enumeration that finds which tenants have
//! expired bundles is elevated.
//!
//! This mirrors `account-management`, which does the same thing for the same
//! reason and says so: its hierarchy read port *"centralizes the
//! `AccessScope::allow_all()` trust elevation at a single named call site so
//! this gear no longer carries that concern"*
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
