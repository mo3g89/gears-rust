//! qa-environments-internal "system actor" `SecurityContext` factory.
//!
//! The background observation ticker (Task 8, `crate::gear`'s
//! `observation_ticker`) has no end-user `SecurityContext` to forward, but its
//! per-platform work — resolving a platform's kubeconfig out of credstore,
//! reading its `VPADM_NAMESPACE` variable, persisting what it observed —
//! still runs through the exact same PEP-enforced service layer as every
//! other caller (`PlatformsService::observe_platform`,
//! `PlatformsService::run_observation_cycle`). This factory mints the
//! identity that work uses.
//!
//! Structure copied from `qa-runs`' and `qa-insights`' `domain::system_actor`
//! modules, which trace back to `qa-catalog`'s and, before that, the
//! site-specific-factory idiom of
//! `gears/system/account-management/.../domain/system_actor.rs`.
//!
//! # Why this module is smaller than its siblings
//!
//! `qa-runs`' and `qa-insights`' versions each hold a **pair** of factories per
//! call site: a nil-tenant *enumeration* context (for "which tenants/rows exist
//! at all", routed through `PolicyEnforcer` like everything else, and — on a
//! deployment running either shipped `AuthZ` plugin — denied by design; see
//! `qa-insights`' `domain::service::tenants` module doc) and a tenant-bound
//! *action* context minted from whatever tenant the enumeration step turned up.
//!
//! This module has no enumeration factory, because this gear's enumeration
//! step never derives its scope from a `SecurityContext` at all:
//! [`crate::domain::repos::PlatformsRepository::list_all_with_tenant`] does
//! take a `scope: &AccessScope` (`domain/repos/platforms_repo.rs`) — it is
//! still routed through `SecureORM` like every other read — but its one
//! production caller passes the ratified `AccessScope::allow_all()` literal
//! (`domain/service/platforms.rs`, inside the block-comment exception that
//! `unscoped_read_guard_tests` pins as the only one in this crate) rather
//! than a scope compiled from an identity. The reasoning is the one that
//! repository method's own doc gives: "which platforms exist" is this gear's
//! own maintenance question, not a caller's, so there is no caller to
//! authorize. That sidesteps the failure mode the paragraph above names: an
//! operator who has not granted this gear's system actor a cross-tenant PDP
//! policy still gets a working self-heal ticker, rather than one that silently
//! evaluates zero platforms forever. [`for_observation`] is this module's only
//! factory because it is the only context anything in this gear's background
//! work ever needs: one per platform, bound to the tenant
//! `list_all_with_tenant` paired that platform with.

use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Stable subject id every qa-environments system-actor context carries, so an
/// audit log can always tell this gear's own background sweep apart from an
/// end user, and so repeated cycles are recognisably the same actor rather
/// than a fresh random one each time.
///
/// The `cf05` group continues the subsystem's series: `cf01` is
/// account-management, `cf02` is qa-catalog, `cf03` is qa-runs, `cf04` is
/// qa-insights.
pub const QA_ENVIRONMENTS_SYSTEM_ACTOR_UUID: Uuid =
    uuid::uuid!("00000000-0000-cf05-0000-716165737973");

/// `subject_type` stamped on every qa-environments system-actor context.
const QA_ENVIRONMENTS_SYSTEM_SUBJECT_TYPE: &str = "qa_environments.system";

/// The observation ticker's per-platform context, bound to the tenant that
/// owns the platform being observed.
///
/// `tenant_id` must be non-nil — it comes from a stored
/// `qa_platforms.tenant_id` column via
/// [`crate::domain::repos::PlatformsRepository::list_all_with_tenant`], and
/// `run_observation_cycle` skips (logging a warning) any row whose tenant
/// somehow reads as nil rather than ever calling this with one. A nil tenant
/// here would compile to an empty constraint set under every real `AuthZ`
/// plugin in this tree and be denied outright (`PolicyEnforcer::access_scope`
/// requires constraints) — asserted in debug builds instead of silently
/// producing a context guaranteed to be refused everywhere.
///
/// # Panics
///
/// Never in practice: both required builder fields are set unconditionally
/// below.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "both builder fields are statically set; the expect anchors the \
              impossible-failure invariant, matching qa-runs'/qa-insights' \
              build_inner"
)]
pub fn for_observation(tenant_id: Uuid) -> SecurityContext {
    debug_assert!(
        !tenant_id.is_nil(),
        "for_observation must never be called with a nil tenant id; the caller \
         (run_observation_cycle) is responsible for skipping such a row first"
    );
    SecurityContext::builder()
        .subject_id(QA_ENVIRONMENTS_SYSTEM_ACTOR_UUID)
        .subject_type(QA_ENVIRONMENTS_SYSTEM_SUBJECT_TYPE)
        .subject_tenant_id(tenant_id)
        .build()
        .expect("QA_ENVIRONMENTS_SYSTEM_ACTOR_UUID + tenant_id are always present")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_observation_context_carries_the_tenant_it_was_given() {
        let tenant = Uuid::new_v4();
        let ctx = for_observation(tenant);
        assert_eq!(ctx.subject_tenant_id(), tenant);
        assert_eq!(ctx.subject_id(), QA_ENVIRONMENTS_SYSTEM_ACTOR_UUID);
    }
}
