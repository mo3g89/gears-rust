//! [`CatalogReader`] over `qa_catalog_sdk::QaCatalogClientV1`.
//!
//! # It translates errors and nothing else
//!
//! One SDK call plus one `map_err`, and the signatures agree argument for
//! argument: `CatalogReader::list_universe` and
//! `QaCatalogClientV1::list_universe` (`qa-catalog-sdk/src/client.rs:134-139`)
//! take the same two optional arguments in the same order and return the same
//! `Vec<UniverseTest>`. That is not a coincidence to be preserved by hand —
//! [`crate::domain::ports::catalog_reader`]' header states that the port names
//! *exactly* the read this gear performs, so anything that looked like a decision
//! here would be a decision the analytics folds could not reach with a fake.
//!
//! In particular the ordering is **not** re-applied. The SDK's contract is
//! "ordered by `test_name`, matching legacy's sort (`analytics.rs:941`)"
//! (`qa-catalog-sdk/src/client.rs:132-133`), and re-sorting here would create a
//! second place where the two could disagree — the hazard
//! `infra::clients::qa_runs`' `list_runs_finished_since` spells out for its own
//! ordering guarantee.
//!
//! # The error translation is the security-relevant part, and it has **no**
//! # not-found arm
//!
//! `QaCatalogError` is `toolkit_canonical_errors::CanonicalError`
//! (`qa-catalog-sdk/src/errors.rs`), the same type `QaRunsError` aliases — so
//! this file is deliberately the *listing* half of
//! [`super::qa_runs`]'s table and nothing else. `PermissionDenied` and
//! `Unauthenticated` are decisions about the **subject** and become
//! [`DomainError::Forbidden`]; everything else becomes
//! [`DomainError::Internal`], which `domain::error`'s boundary mapping renders
//! as an opaque 500.
//!
//! **There is no `NotFound` arm, and that omission is the contract rather than a
//! gap.** [`CatalogReader::list_universe`]'s `# Errors` section says it: *"There
//! is no not-found case: `product_id` naming nothing visible to `ctx` is an empty
//! `Vec`, exactly like an unsynced repository, so this read is not an existence
//! oracle for a product id."* A `NotFound` arriving here is therefore a contract
//! break on the far side, and laundering it into `Ok(Vec::new())` would be worse
//! than a 500: the universe is the **denominator** of every Phase B number, so an
//! empty one is not an error anywhere downstream — it is zeros, a `total_tests` of
//! nothing and an overview that renders as a healthy, empty suite. That is the
//! same argument `super::qa_runs`' `on_subject` makes about an empty window, one
//! aggregation level up, and it is why this adapter has one mapping function where
//! that one has two: no read here addresses a single row.

use std::sync::Arc;

use async_trait::async_trait;
use qa_catalog_sdk::{QaCatalogClientV1, QaCatalogError, UniverseTest};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::CatalogReader;

/// The qa-catalog reads, over the `ClientHub`-resolved client.
pub struct QaCatalogReader {
    client: Arc<dyn QaCatalogClientV1>,
}

impl QaCatalogReader {
    #[must_use]
    pub const fn new(client: Arc<dyn QaCatalogClientV1>) -> Self {
        Self { client }
    }
}

/// A qa-catalog error on a read that addresses no single row.
///
/// Deliberately has **no** `NotFound` arm — this module's header carries the
/// argument, and `a_not_found_is_internal_rather_than_an_empty_universe` is what
/// fails if one is added.
fn on_subject(err: QaCatalogError) -> DomainError {
    match err {
        QaCatalogError::PermissionDenied { .. } | QaCatalogError::Unauthenticated { .. } => {
            DomainError::Forbidden
        }
        // `{err}` rather than the raw detail: `CanonicalError`'s `Display` is the
        // sibling's own rendering, and this string never reaches a client —
        // `DomainError::Internal` is mapped to an opaque 500.
        other => DomainError::Internal(format!("qa-catalog read failed: {other}")),
    }
}

#[async_trait]
impl CatalogReader for QaCatalogReader {
    async fn list_universe(
        &self,
        ctx: &SecurityContext,
        product_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<Vec<UniverseTest>, DomainError> {
        self.client
            .list_universe(ctx, product_id, branch)
            .await
            .map_err(on_subject)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! The translation table, which is the only thing in this file worth a test.
    //!
    //! Driven against the one mapping function rather than through a fake client,
    //! for the reason `infra::clients::qa_runs`' test module gives: a fake
    //! `QaCatalogClientV1` would be **twenty-four** `unimplemented!()`s — the
    //! exact cost `domain::ports::catalog_reader`'s header cites as the reason the
    //! port exists — inserted between the assertion and the decision without
    //! adding coverage.
    //!
    //! # The inputs are built the way qa-catalog builds them
    //!
    //! `CanonicalError`'s variants are `#[non_exhaustive]`, so a struct literal
    //! does not compile. [`FarSide`] is a `#[resource_error]` type over
    //! **qa-catalog's own** GTS id, so these fixtures come out of the same builder
    //! chain the real client's errors do.

    use toolkit::api::canonical_prelude::*;

    use super::on_subject;
    use crate::domain::error::DomainError;

    /// qa-catalog's plan resource, spelled exactly as
    /// `qa-catalog/src/api/rest/error.rs:21` spells it — this is the id its errors
    /// carry. The plan resource rather than one of the other five there because
    /// `list_universe` is a walk over plans; which of the six it is makes no
    /// difference to the arm under test, and picking a real one is what keeps
    /// these fixtures the same shape the far side actually sends.
    #[resource_error(gts_id!("cf.qa.catalog.plan.v1~"))]
    struct FarSide;

    fn not_found(detail: &str) -> CanonicalError {
        FarSide::not_found(detail.to_owned())
            .with_resource("some-plan")
            .create()
    }

    /// A subject-level refusal is [`DomainError::Forbidden`] and not an internal
    /// failure.
    ///
    /// `CatalogReader::list_universe`' own `# Errors` section fixes this:
    /// *"[`DomainError::Forbidden`] when qa-catalog refuses the subject"*.
    /// Folding it into `Internal` would answer an opaque 500 to an operator whose
    /// grant is simply missing — the failure `infra::clients::qa_runs` spent a
    /// task learning to make visible, and the reason that adapter has a separate
    /// arm for it.
    #[test]
    fn a_permission_denial_is_forbidden_rather_than_an_internal_failure() {
        let denied = FarSide::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create();
        assert!(matches!(on_subject(denied), DomainError::Forbidden));
    }

    /// `Unauthenticated` shares the arm: it is a decision about the **subject**,
    /// exactly as the denial above is.
    ///
    /// Built off `CanonicalError` rather than off [`FarSide`], because
    /// `#[resource_error]` generates no `unauthenticated` constructor
    /// (`toolkit-canonical-errors-macro/src/lib.rs:179-297` generates thirteen and
    /// that is not one of them); the variant is reachable only through
    /// `CanonicalError::unauthenticated` (`builder.rs:621`). So a real
    /// `Unauthenticated` from qa-catalog carries no resource type either, which is
    /// what this fixture reproduces.
    #[test]
    fn an_unauthenticated_call_is_forbidden_too() {
        let anon = CanonicalError::unauthenticated()
            .with_reason("NO_TOKEN")
            .create();
        assert!(matches!(on_subject(anon), DomainError::Forbidden));
    }

    /// **`NotFound` has no arm, and that is the load-bearing omission.**
    ///
    /// `CatalogReader::list_universe`'s contract says *"There is no not-found
    /// case: `product_id` naming nothing visible to `ctx` is an empty `Vec`"* —
    /// so a `NotFound` arriving here is a **contract break on the far side**, not
    /// an empty universe. Laundering it into `Ok(Vec::new())` would make a broken
    /// qa-catalog indistinguishable from a deployment with nothing synced, and
    /// every Phase B aggregate over an empty universe is zeros rather than an
    /// error — so the whole overview would render as a healthy, empty suite.
    /// `infra::clients::qa_runs`'
    /// `a_listing_not_found_is_internal_rather_than_an_empty_window` is the same
    /// property one gear over.
    #[test]
    fn a_not_found_is_internal_rather_than_an_empty_universe() {
        assert!(matches!(
            on_subject(not_found("no such product")),
            DomainError::Internal(_)
        ));
    }

    /// Anything else is internal, and the message names the gear so a log line is
    /// attributable. Opaque to a client — `DomainError::Internal` maps to the
    /// canonical internal detail in `domain::error`.
    #[test]
    fn any_other_failure_is_internal_and_names_the_gear() {
        let boom = CanonicalError::internal("upstream exploded").create();
        match on_subject(boom) {
            DomainError::Internal(msg) => assert!(msg.contains("qa-catalog"), "{msg}"),
            other => panic!("expected Internal, got {other:?}"),
        }
    }
}
