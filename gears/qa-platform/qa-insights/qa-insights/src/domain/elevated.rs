//! The one place this gear elevates past the policy engine.
//!
//! # Why this exists
//!
//! One of this gear's system-actor contexts is **nil-tenant**:
//! [`system_actor::for_ticker_enumeration`]. The reconcile sweep, the JIRA
//! poller and the collect ticker are tickers — they hold no request, so unlike
//! the operator rebuild (tenant on the caller's own `SecurityContext`) they
//! have no tenant to take one from, and the one cross-tenant read that finds
//! out which tenants exist has to run before any of them can bind to one.
//!
//! `static-authz-plugin` denies a nil-tenant request — correctly, since
//! granting unrestricted access to an unauthenticated caller is exactly what
//! it is there to stop. qa-platform previously discharged that denial by
//! adding a `system_grants` list to that shared plugin. That change has been
//! reverted; the elevation now lives here, in the gear that needs it, where
//! it is one function and one audit point instead of a config surface on a
//! system gear every other deployment also links.
//!
//! # Why this is not a security regression
//!
//! The elevation is **not reachable from a request**. Its only caller is
//! [`crate::domain::service::tenants::TenantDirectory`], and it uses the
//! scope only for a cross-tenant **read** that enumerates which tenants hold
//! projected results. The writes and reads that follow — the reconcile
//! sweep's backfills, the JIRA poller's pass, the collect ticker's cycle —
//! are re-scoped per tenant under `system_actor`'s *tenant-bound* factories
//! (`for_reconcile_sweep`, `for_jira_poll`, `for_collect_cycle`), which is the
//! pairing `system_actor`'s own module doc already describes.
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
