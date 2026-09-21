//! qa-catalog-internal "system actor" `SecurityContext` factories.
//!
//! The lifecycle background tasks (branch-cache refresher, bundle GC — see
//! `crate::gear`) have no end-user `SecurityContext` to forward but still
//! run through the same PEP-enforced service layer as every other caller.
//! These factories mint the stable, audit-correlatable identity those
//! flows use: every system call carries `subject_id =
//! QA_CATALOG_SYSTEM_ACTOR_UUID` and `subject_type = "qa_catalog.system"`.
//!
//! Mirrors the site-specific-factory idiom of
//! `gears/system/account-management/.../domain/system_actor.rs` (the
//! platform precedent for background-flow system contexts): one named
//! factory per legitimate call site, each logging a `tracing` line under
//! the `qa_catalog.system_actor` target, so "where does qa-catalog elevate
//! to system?" stays grep-able and auditable. A new background flow must
//! add a new factory here — a deliberate review-magnet.
//!
//! # One of these runs on a request, not on a ticker
//!
//! [`for_bundle_download`] is the exception to the sentence above: it backs
//! `GET /qa/v1/test-bundles/{id}?sig=...`, which is registered
//! `.anonymous().exposed()` because its caller is a workflow pod with no user
//! to borrow a session from. It is tenant-bound to the tenant the descriptor
//! row names — recovered from the row, never asserted by the caller — and it
//! is only reached after the caller's HMAC tag has verified against that
//! tenant's derived key. See its own doc.
//!
//! # The nil/tenant-bound split
//!
//! [`for_branch_refresh_enumeration`] and [`for_bundle_gc`] are nil-tenant:
//! each backs a lifecycle ticker's cross-tenant enumeration, and neither
//! context is ever handed to the PEP — both are consumed only by
//! `crate::domain::elevated::enumeration_scope`, which returns
//! `AccessScope::allow_all()` directly. See that module's doc for the
//! argument in full. [`for_branch_refresh`] and [`for_bundle_delete`] are
//! tenant-bound, minted per row from the enumeration's own answers
//! (`RefreshTarget::tenant_id`, and the tenant ids
//! `BundlesService::tenants_with_expired_bundles` returns), and every write
//! they authorize still goes through the PEP under that resolved tenant.
//!
//! # Authorization note
//!
//! **The two tenant-bound contexts do not bypass the PEP.** Every write call
//! still asks the PDP for a decision and compiles the returned constraints
//! into an `AccessScope`, scoped to the row's resolved tenant. The
//! deployment's `AuthZ` policy must grant the `qa_catalog.system` subject the
//! scopes those writes need (per-tenant SYNC for the refresh, per-tenant
//! DELETE for the bundle purge); under a deny-all policy for that tenant the
//! task fails closed and logs — it never falls back to an unscoped query.
//!
//! **The two nil-tenant contexts do bypass the PEP, deliberately, and by
//! name.** No deployment policy grant is required for either enumerating
//! read to work, none is consulted, and there is no deny-all case for them to
//! fail closed against. What still fails closed is the write that follows
//! each one, under the matching tenant-bound factory above.

use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Hand-picked actor UUID (trailing bytes spell `qacsys`), stable across
/// processes so audit sinks can correlate qa-catalog system invocations
/// under one identity. Cannot collide with any v4 actor UUID.
pub const QA_CATALOG_SYSTEM_ACTOR_UUID: Uuid = uuid::uuid!("00000000-0000-cf02-0000-716163737973");

/// `subject_type` stamped on every qa-catalog system-actor context.
const QA_CATALOG_SYSTEM_SUBJECT_TYPE: &str = "qa_catalog.system";

/// Internal builder shared by every factory.
///
/// `scope_tenant = None` falls back to the platform-root sentinel
/// ([`Uuid::nil`]) for platform-scoped flows (cross-tenant enumeration and
/// GC sweeps).
///
/// # Panics
///
/// Never in practice: both required builder fields are set unconditionally
/// below.
#[allow(
    clippy::expect_used,
    reason = "both builder fields are statically set; the expect anchors the impossible-failure invariant"
)]
fn build_inner(scope_tenant: Option<Uuid>) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(QA_CATALOG_SYSTEM_ACTOR_UUID)
        .subject_type(QA_CATALOG_SYSTEM_SUBJECT_TYPE)
        .subject_tenant_id(scope_tenant.unwrap_or_else(Uuid::nil))
        .build()
        .expect("QA_CATALOG_SYSTEM_ACTOR_UUID + tenant_id are always present")
}

/// Branch-cache refresher, enumeration step — the cross-tenant
/// `(repository, tenant)` target listing. Platform-scoped (nil tenant). The
/// context this factory returns is never passed to the PEP: see
/// `crate::domain::elevated::enumeration_scope`, which the enumeration reads
/// through instead, and [`for_branch_refresh`] for the tenant-bound write
/// that follows for each target.
#[must_use]
pub fn for_branch_refresh_enumeration() -> SecurityContext {
    tracing::info!(
        target: "qa_catalog.system_actor",
        site = "branch_refresh_enumeration",
        "qa-catalog system actor constructed",
    );
    build_inner(None)
}

/// Branch-cache refresher, per-repository step. Tenant-bound to the
/// repository's owning tenant so the refreshed branch rows are written
/// under the CORRECT tenant (`ReposService` derives the rows' `tenant_id`
/// from the context) and the per-repo credstore read runs tenant-scoped.
#[must_use]
pub fn for_branch_refresh(tenant_id: Uuid) -> SecurityContext {
    tracing::info!(
        target: "qa_catalog.system_actor",
        site = "branch_refresh",
        tenant_id = %tenant_id,
        "qa-catalog system actor constructed",
    );
    build_inner(Some(tenant_id))
}

/// Bundle GC sweep, enumeration step — the cross-tenant listing of tenants
/// with at least one expired bundle
/// (`BundlesService::tenants_with_expired_bundles`). Platform-scoped (nil
/// tenant). The context this factory returns is never passed to the PEP: see
/// `crate::domain::elevated::enumeration_scope`, which the enumeration reads
/// through instead, and [`for_bundle_delete`] for the tenant-bound write that
/// follows for each tenant the enumeration answers with.
///
/// **This no longer authorizes the delete itself.** Before this task, this
/// factory's context was handed straight to `BundlesService::purge_expired`,
/// which asked the PEP for a single platform-wide `qa.bundle`/`DELETE` scope
/// and ran one atomic select+delete across every tenant. Elevating that
/// context would have elevated a *write* — the one thing this seam exists to
/// avoid — so the delete now runs once per tenant, under
/// [`for_bundle_delete`], inside its own transaction; only the read that
/// finds which tenants to loop over is elevated.
#[must_use]
pub fn for_bundle_gc() -> SecurityContext {
    tracing::info!(
        target: "qa_catalog.system_actor",
        site = "bundle_gc",
        "qa-catalog system actor constructed",
    );
    build_inner(None)
}

/// Bundle GC sweep, per-tenant delete step: `BundlesService::purge_expired`
/// for one tenant's expired bundles, in its own transaction. Tenant-bound —
/// see [`for_bundle_gc`] for why the enumeration above is nil-tenant and this
/// write is not: it keeps the atomic select+delete
/// (`BundlesRepository::delete_expired`) inside a scope that spans one
/// tenant only, so the PEP grant a deployment gives this write can never
/// reach across tenants even though the enumeration that found the tenant did.
#[must_use]
pub fn for_bundle_delete(tenant_id: Uuid) -> SecurityContext {
    tracing::info!(
        target: "qa_catalog.system_actor",
        site = "bundle_delete",
        tenant_id = %tenant_id,
        "qa-catalog system actor constructed",
    );
    build_inner(Some(tenant_id))
}

/// The anonymous bundle-download route, after its signature verified:
/// `GET /qa/v1/test-bundles/{id}?sig=...`. Tenant-bound to the tenant the
/// **descriptor row** names, never to anything the caller asserted.
///
/// # The one inbound, request-driven factory in this module
///
/// Every other factory here backs a lifecycle ticker. This one runs on a
/// request, from a caller with no session at all — a workflow pod fetching the
/// test content it is about to execute. It is the structural twin of
/// qa-insights' `system_actor::for_collect_report`, which exists for exactly
/// the same reason on exactly the same kind of route.
///
/// **What makes that safe is the order of operations, not this function.**
/// `BundlesService::get_bundle_content_signed` reads the descriptor's tenant
/// first (elevated, cross-tenant, read-only — `crate::domain::elevated`),
/// verifies the caller's tag against *that* tenant's derived key, and only
/// then calls this. So the tenant on the returned context is the bundle's own,
/// and a caller that could not produce the right tag never reaches this line.
///
/// **It does not bypass the PEP.** The read that follows
/// (`BundlesService::get_bundle_content`) still asks the PDP for
/// `qa.bundle`/`GET` under this tenant and still applies the `expires_at`
/// check, which is what keeps a tag unable to reach past the one bundle it
/// names even if the derivation were wrong. A deployment whose policy denies
/// the `qa_catalog.system` subject that read fails the download closed.
#[must_use]
pub fn for_bundle_download(tenant_id: Uuid) -> SecurityContext {
    tracing::info!(
        target: "qa_catalog.system_actor",
        site = "bundle_download",
        tenant_id = %tenant_id,
        "qa-catalog system actor constructed",
    );
    build_inner(Some(tenant_id))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! Pin the shared invariants every factory MUST satisfy (mirrors the
    //! account-management system-actor test block): stable subject id,
    //! `qa_catalog.system` subject type, and correct tenant binding.
    use super::*;

    #[test]
    fn platform_scoped_factories_use_nil_tenant() {
        for (label, ctx) in [
            (
                "branch_refresh_enumeration",
                for_branch_refresh_enumeration(),
            ),
            ("bundle_gc", for_bundle_gc()),
        ] {
            assert_eq!(ctx.subject_id(), QA_CATALOG_SYSTEM_ACTOR_UUID, "{label}");
            assert_eq!(
                ctx.subject_type(),
                Some(QA_CATALOG_SYSTEM_SUBJECT_TYPE),
                "{label}"
            );
            assert_eq!(ctx.subject_tenant_id(), Uuid::nil(), "{label}");
        }
    }

    #[test]
    fn branch_refresh_factory_carries_supplied_tenant() {
        let tenant = Uuid::from_u128(0xDEAD_BEEF_FACE_CAFE);
        let ctx = for_branch_refresh(tenant);
        assert_eq!(ctx.subject_id(), QA_CATALOG_SYSTEM_ACTOR_UUID);
        assert_eq!(ctx.subject_type(), Some(QA_CATALOG_SYSTEM_SUBJECT_TYPE));
        assert_eq!(
            ctx.subject_tenant_id(),
            tenant,
            "branch rows must be written under the repository's own tenant"
        );
    }

    #[test]
    fn bundle_download_factory_carries_the_bundles_own_tenant() {
        let tenant = Uuid::from_u128(0x0BAD_C0DE_F00D_BEEF);
        let ctx = for_bundle_download(tenant);
        assert_eq!(ctx.subject_id(), QA_CATALOG_SYSTEM_ACTOR_UUID);
        assert_eq!(ctx.subject_type(), Some(QA_CATALOG_SYSTEM_SUBJECT_TYPE));
        assert_eq!(
            ctx.subject_tenant_id(),
            tenant,
            "the anonymous download must run under the tenant the DESCRIPTOR names -- a \
             nil or caller-supplied tenant here would either become the platform-root \
             sentinel or let a caller pick its own scope"
        );
        assert_ne!(
            ctx.subject_tenant_id(),
            Uuid::nil(),
            "a nil tenant here is the platform-root sentinel; this context authorises a \
             read and must never be platform-scoped"
        );
    }

    #[test]
    fn bundle_delete_factory_carries_supplied_tenant() {
        let tenant = Uuid::from_u128(0xDEAD_BEEF_FACE_CAFE);
        let ctx = for_bundle_delete(tenant);
        assert_eq!(ctx.subject_id(), QA_CATALOG_SYSTEM_ACTOR_UUID);
        assert_eq!(ctx.subject_type(), Some(QA_CATALOG_SYSTEM_SUBJECT_TYPE));
        assert_eq!(
            ctx.subject_tenant_id(),
            tenant,
            "the per-tenant purge must run under the tenant the enumeration answered with"
        );
    }
}
