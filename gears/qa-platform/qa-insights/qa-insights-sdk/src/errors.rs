//! Canonical error envelope for the qa-insights contract.
//!
//! # Why this file is one line, and not the enum the plan asked for
//!
//! The plan's Task 8 Step 2 says to define `QaInsightsError` "mirroring
//! `QaEnvironmentsError`'s variant set — copy its shape, then delete the variants
//! that have no counterpart here and add `IngestConflict` and
//! `UnsupportedEgress`". That instruction rests on a misreading of the sibling:
//! `QaEnvironmentsError` **has no variant set of its own**. It is a one-line
//! re-export of `toolkit_canonical_errors::CanonicalError`
//! (`qa-environments-sdk/src/errors.rs`), and so are `QaRunsError`
//! (`qa-runs-sdk/src/errors.rs`) and `QaCatalogError`
//! (`qa-catalog-sdk/src/errors.rs`). All three shipped SDKs are identical here.
//! Copying the sibling's shape therefore *is* this line; inventing an enum would
//! have been the divergence.
//!
//! Gear-specific error conditions live in the **gear** crate's
//! `domain/error.rs` as a `#[domain_model] DomainError`, which is then mapped
//! onto a `CanonicalError` at the boundary — see
//! `qa-environments/src/domain/error.rs` and `qa-catalog/src/domain/error.rs`.
//! That is where the plan's two new conditions belong, and Task 9 creates the
//! file. Recorded here so the next reader does not go looking for them in the
//! SDK:
//!
//! * **`IngestConflict`** — a concurrent write to the same `(run_id, test_file,
//!   test_name)` triple lost the delete-then-insert race that makes ingest
//!   idempotent (design §4.4). Maps to [`CanonicalError::Aborted`]: the caller
//!   may retry, and the reconciler will in any case.
//! * **`UnsupportedEgress`** — a notification was routed to a channel this
//!   deployment has no adapter for. Maps to [`CanonicalError::Unimplemented`].
//!
//! [`CanonicalError::Aborted`]: toolkit_canonical_errors::CanonicalError::Aborted
//! [`CanonicalError::Unimplemented`]: toolkit_canonical_errors::CanonicalError::Unimplemented

pub use toolkit_canonical_errors::CanonicalError as QaInsightsError;
