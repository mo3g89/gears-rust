//! The qa-catalog read the analytics universe is built from, behind a port.
//!
//! # What the universe is, and why this gear cannot compute it
//!
//! Legacy builds the analytics universe inline: it lists every plan, resolves
//! each plan entry to a file in a git working copy, and parses `TEST_META` out
//! of that file (`manager/src/routes/analytics.rs:817-941`). qa-insights has no
//! checkout — ADR-0005 confines git egress to qa-catalog — so the whole walk
//! happens on the other side of a gear boundary and arrives here as a list of
//! [`UniverseTest`]. `qa_catalog_sdk::QaCatalogClientV1::list_universe`'s own
//! header states that provenance.
//!
//! The universe is the **denominator** of every Phase B number: a test that
//! exists in it and has no execution row is `NOT_RUN`, and a row that resolves
//! to nothing in it is dropped
//! ([`crate::domain::analytics::universe`] holds that join).
//!
//! # Why a port and not the SDK client
//!
//! `qa_catalog_sdk::QaCatalogClientV1` has **25** methods
//! (`qa-catalog-sdk/src/client.rs:23-236`, counted 2026-08-21 — 25 lines
//! matching `^    async fn `, all inside the one trait). This gear reads through
//! **one** of them. A fake of the whole client would be twenty-four
//! `unimplemented!()`s around the single read that matters, and the ratio is
//! worse here than for [`super::runs_reader`]'s four-of-seventeen.
//!
//! Naming exactly the read this gear performs also makes the cross-gear surface
//! greppable: [`CatalogReader`] is the complete list of what qa-insights asks
//! qa-catalog for. **It grows only for a read a test in this crate exercises**
//! — the rule [`super::runs_reader`] states and Task 18 honoured, and
//! [`CatalogReader::list_repos`] is the second read to earn a place on it.
//!
//! # `list_repos` is not `list_universe`, and the difference is the whole point
//!
//! [`list_universe`](CatalogReader::list_universe) answers "which test files
//! exist", by walking each repository's checkout on some branch — so a
//! repository that has never synced, or whose named branch has no plan on it,
//! contributes **nothing**, indistinguishably from a repository with
//! genuinely no tests. That is the correct denominator for a suite summary,
//! and the wrong test for "does this run belong to this product": a run
//! plainly exists in qa-runs regardless of whether qa-catalog has ever walked
//! its repository's checkout.
//!
//! [`list_repos`](CatalogReader::list_repos) answers a narrower, cheaper
//! question instead — which repositories exist and which product each one
//! belongs to — from `qa_test_repositories`' own rows, a plain listing with no
//! checkout walk and no dependency on sync state.
//! `domain::service::dashboard::DashboardService::stats` is its one caller,
//! for exactly the reason its own doc gives: attributing a *run* to a product
//! must not fail closed the moment a repository is merely unsynced.
//!
//! # The production adapter is [`crate::infra::clients::qa_catalog`], added by
//! # Task 25a
//!
//! **This section said the adapter was Task 40's, and that was never viable.**
//! The argument was that `gear::init` resolves clients from the hub and that no
//! service holding this port existed until Tasks 21-27 — true when it was
//! written, and it stopped being true fifteen tasks before Task 40. Task 25 is
//! the task that issues the first real `list_universe` read, so it was *blocked*
//! by its own prerequisite. Task 25a pulled the adapter forward, together with
//! the `ClientHub` lookup that constructs it; Task 40 keeps the rest of that
//! wiring (the three tickers, the elector, the oagw client and the
//! local-client registration — a transactional broker consumer was Task 40's
//! too, and it was deleted along with the event-broker dependency it needed).
//!
//! The `Vec`-backed double
//! ([`crate::domain::service::test_support::FakeCatalog`]) is still what the
//! domain tests fold, and it is still the only implementation any test in this
//! crate reaches — the adapter holds one `map_err` and its own tests drive that
//! function directly. Its header says why a fake of the whole 25-method client
//! would be worse than either.
//!
//! There is no `#[expect(dead_code)]` anywhere in this file, and that is a
//! measured fact rather than an omission: `dead_code` does not fire for a `pub`
//! item reachable from the crate root (`lib.rs` declares `pub mod domain`), so an
//! `#[expect]` here would be an *unfulfilled* expectation — itself a warning, and
//! a build failure under this workspace's `-D warnings`. `gear.rs`'s per-field
//! attributes remain the register of what is still unwired; this port is no
//! longer on it.
//!
//! # Errors cross the boundary as [`DomainError`]
//!
//! `QaCatalogError` is qa-catalog's vocabulary. The adapter is what translates;
//! the port speaks this gear's error type so no analytics core has to learn a
//! sibling's — the same reason [`super::runs_reader`] gives.

use async_trait::async_trait;
use qa_catalog_sdk::{TestRepository, UniverseTest};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The reads qa-insights performs against qa-catalog.
#[async_trait]
pub trait CatalogReader: Send + Sync {
    /// Every registered repository, with the product it belongs to.
    ///
    /// `qa_catalog_sdk::QaCatalogClientV1::list_repos` verbatim — no
    /// arguments beyond `ctx`, and no filter by product: the one caller
    /// resolves "which repositories belong to `product_id`" itself, over the
    /// whole list, because it also needs "which repositories exist at all"
    /// for nothing (there is no second caller yet to share the read with).
    ///
    /// This module's header contrasts this with [`Self::list_universe`] in
    /// full; the short version is that this is a plain table listing, so
    /// **an unsynced repository is not silently absent** — every registered
    /// repository is here, whether or not qa-catalog has ever walked its
    /// checkout.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] for any transport or gateway failure, and
    /// [`DomainError::Forbidden`] when qa-catalog refuses the subject. There
    /// is no not-found case: an empty result is a tenant with no repositories,
    /// not a refusal.
    async fn list_repos(&self, ctx: &SecurityContext) -> Result<Vec<TestRepository>, DomainError>;

    /// Every test file reachable from `product_id`'s plans on `branch`, with its
    /// `TEST_META` attributes and static case count.
    ///
    /// `qa_catalog_sdk::QaCatalogClientV1::list_universe`
    /// (`qa-catalog-sdk/src/client.rs:134-139`) verbatim: same two optional
    /// arguments, same ordering, no paging.
    ///
    /// # `branch: None` is the *default* branch, **not** every branch
    ///
    /// The asymmetry is worth stating because the row-side filter spells the
    /// same `None` the other way round. Here `None` falls back to each
    /// repository's own `default_branch`, which is legacy's no-branch-selected
    /// behaviour (`analytics.rs:810-816` chooses the plan checkout, and
    /// `qa_catalog_sdk::QaCatalogClientV1::list_universe`'s header records the
    /// fallback). On
    /// [`UniverseFilter::branch`](crate::domain::analytics::UniverseFilter::branch)
    /// `None` means *no predicate*, i.e. every branch's rows
    /// (`analytics.rs:967-970`, the `$N::text IS NULL` guard). Legacy runs both
    /// at once: with no branch selected it reads the default branch's universe
    /// and every branch's executions.
    ///
    /// # One call, not one per plan
    ///
    /// Analytics computes a whole-universe summary, so an N+1 across the SDK
    /// boundary would make the overview endpoint's latency a function of plan
    /// count. The SDK's own header makes the same argument, and it is why the
    /// projection exists at all.
    ///
    /// # A repository that is not synced contributes nothing
    ///
    /// It does not fail the call. Legacy drops such repositories the same way
    /// (`analytics.rs:861-865`), because one unsynced repository must not blank
    /// the overview for all the others. The consequence for a reader is that an
    /// **empty universe is a legal answer** and means "nothing is synced", not
    /// "nothing exists" — every Phase B aggregate over an empty universe is
    /// zeros rather than an error.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] for any transport or gateway failure, and
    /// [`DomainError::Forbidden`] when qa-catalog refuses the subject. There is
    /// no not-found case: `product_id` naming nothing visible to `ctx` is an
    /// empty `Vec`, exactly like an unsynced repository, so this read is not an
    /// existence oracle for a product id.
    async fn list_universe(
        &self,
        ctx: &SecurityContext,
        product_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<Vec<UniverseTest>, DomainError>;
}
