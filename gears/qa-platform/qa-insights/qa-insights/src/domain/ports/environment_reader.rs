//! The qa-environments read that turns an environment id into the label a
//! chart draws, behind a port.
//!
//! # Why this port exists at all: legacy had a name where this schema has an id
//!
//! Legacy's execution row carries `r.platform`, a display **name**, written by
//! the runner and read straight onto the group chart
//! (`manager/src/routes/analytics.rs:1085-1146`, `build_grouped_summaries`).
//! Legacy's "platform" is this gear's `Environment`; the names in that citation
//! are legacy's and do not rename with it. This architecture has no such
//! column:
//! [`ExecRow::platform_id`](crate::domain::analytics::ExecRow::platform_id) is a
//! `Uuid`, because qa-environments owns the environment and a denormalized name
//! would be a second copy of it that drifts on rename.
//!
//! Task 23 typed its fold's output
//! [`PlatformGroupSummary`](crate::domain::analytics::aggregates::PlatformGroupSummary)
//! with that `Uuid` **deliberately, so that no DTO could be written over it
//! without deciding what the label is** — the gap fails at the DTO boundary
//! rather than rendering a plausible-looking UUID at a user. This port is the
//! answer to that question, and it is the whole of the answer: ids in, names out.
//!
//! # One method, and it resolves many ids in one call
//!
//! `qa_environments_sdk::QaEnvironmentsClientV1` has **eleven** methods
//! (`qa-environments-sdk/src/client.rs:23-107`, counted 2026-08-21 — 11 lines
//! matching `^    async fn `, all inside the one trait). This gear needs **one**
//! quantity from it, so a fake of the whole client would be **ten**
//! `unimplemented!()`s around the single lookup that matters — a worse ratio than
//! [`super::runs_reader`]'s four-of-seventeen and a better one than
//! [`super::catalog_reader`]'s one-of-twenty-five. That header is the precedent
//! and the argument is the same one.
//!
//! (The count is spelled by hand, and `infra::clients`' header records that the
//! last **two** hand-spellings of the *other* trait's count were wrong — this
//! line said three, which is a miscount about a miscount. Recount rather than
//! trusting it: `grep -c '^    async fn ' qa-environments-sdk/src/client.rs`.)
//!
//! Naming exactly the read this gear performs also makes the cross-gear surface
//! greppable: [`EnvironmentReader`] is the complete list of what qa-insights asks
//! qa-environments for, and it is one method long. **It grows only for a read a
//! test in this crate exercises** — the rule [`super::runs_reader`] states.
//!
//! # It is not a per-row lookup, and the signature is what enforces that
//!
//! [`EnvironmentReader::names`] takes a **slice** of ids and answers with a map. It
//! is called once per request with the distinct ids of a whole grouped summary,
//! never once per [`ExecRow`](crate::domain::analytics::ExecRow) — the same
//! argument
//! [`CatalogReader::list_universe`](super::catalog_reader::CatalogReader::list_universe)
//! makes about plans: *"an N+1 across the SDK boundary would make the overview
//! endpoint's latency a function of plan count."* Here it would be a function of
//! row count, on a table `cpt-cf-qa-nfr-scale` sizes at 5M rows. A
//! `name(id) -> String` signature would make that mistake expressible; this one
//! does not.
//!
//! # Errors cross the boundary as [`DomainError`]
//!
//! `QaEnvironmentsError` is qa-environments' vocabulary. The adapter is what
//! translates; the port speaks this gear's error type, the same reason
//! [`super::runs_reader`] and [`super::catalog_reader`] give.
//!
//! # A second method, added by Task 35
//!
//! [`EnvironmentReader::default_branch`] is the per-environment branch override
//! the auto-rerun resolves **once** and reuses for both the plan lookup and the
//! launch — `domain::service::jira_poller`'s module doc carries the full
//! argument for why resolving it twice, or resolving the lookup against a
//! repository default instead, silently drops a rerun. It is a **single**-id
//! read rather than a batch, unlike [`EnvironmentReader::names`]: the poller resolves one
//! bug's environment at a time, so there is no window of distinct ids to
//! de-duplicate the way an analytics group chart's rows have. Controller
//! ruling R73: this port grows only for a read a test in this crate
//! exercises, and `the_branch_is_resolved_once_and_reused_for_lookup_and_launch`
//! is that test.

use std::collections::HashMap;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The reads qa-insights performs against qa-environments.
#[async_trait]
pub trait EnvironmentReader: Send + Sync {
    /// The display name of each of `ids` that names an environment visible to
    /// `ctx`.
    ///
    /// # An id that resolves to nothing is **absent from the map**
    ///
    /// That is the contract, and it is neither a drop nor an error. The caller
    /// asked about a set of ids and gets back the subset that resolved, so
    /// `map.get(&id) == None` is the answer *"this id names no environment you
    /// can see"* — which happens for an environment deleted since the run
    /// executed, and for an environment in another tenant.
    ///
    /// **The two are deliberately indistinguishable**, exactly as
    /// [`RunsReader`](super::runs_reader::RunsReader)'s `not_found` rule makes
    /// them for a run: a caller must not be able to use this read to learn that
    /// an id exists somewhere it cannot see.
    ///
    /// **What the absence renders as is not decided here.** It is the DTO's
    /// decision, like the one it makes for
    /// [`ExecRow::run_id`](crate::domain::analytics::ExecRow::run_id) → a run
    /// name. This port must not invent `"unknown"`, `"—"` or the UUID's own
    /// string: each of those is a *label*, and a label chosen here would be a
    /// wire decision made in the wrong layer and invisible to whoever changes it.
    ///
    /// `HashMap<Uuid, String>` rather than `HashMap<Uuid, Option<String>>` with
    /// every requested id present: the caller only ever looks up ids it asked
    /// about, so the extra level would distinguish "asked and unresolved" from
    /// "never asked" for a reader that cannot be in the second state.
    /// `ids.len()` against the map's length is what a caller counts if it wants
    /// to know how many did not resolve.
    ///
    /// # An empty `ids` performs no call
    ///
    /// A request whose rows carry no platform must not cost a cross-gear round
    /// trip. The adapter guarantees it and
    /// `no_ids_means_no_cross_gear_call` pins it.
    ///
    /// # Duplicates and ordering are the caller's business, not this method's
    ///
    /// A map has no order and cannot hold a duplicate key, so `ids` may be
    /// handed over unsorted and repeated with no effect on the answer. Note the
    /// consequence for the group chart: Task 23's
    /// `build_grouped_summaries` orders the platform list **by id**, and legacy
    /// orders it by name, so resolving the names through this port and re-sorting
    /// on them *changes the rendered order*. That is the correct direction —
    /// legacy's order is the one on screen today — and it is a change to expect
    /// rather than a regression to hunt.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] for any transport or gateway failure, and
    /// [`DomainError::Forbidden`] when qa-environments refuses the subject.
    ///
    /// There is no not-found case: ids naming nothing visible to `ctx` are
    /// absent from the map, so a `not_found` reaching the adapter is a contract
    /// break on the far side rather than an empty answer — the same rule
    /// [`CatalogReader::list_universe`](super::catalog_reader::CatalogReader::list_universe)
    /// states, and `infra::clients::qa_environments` carries the mapping.
    ///
    /// **A caller has to decide what a `Forbidden` here means for a payload
    /// whose other sections do not need this read.** This port does not decide
    /// it, and it deliberately does not degrade to an empty map: an operator
    /// missing the `platform:list` grant would then see a group chart labelled
    /// with nothing and no indication why.
    async fn names(
        &self,
        ctx: &SecurityContext,
        ids: &[Uuid],
    ) -> Result<HashMap<Uuid, String>, DomainError>;

    /// `environment_id`'s default-branch **override**, or `None` when it has
    /// none (or does not resolve to an environment visible to `ctx`).
    ///
    /// Legacy `PlatformsService::get_platform_default_branch`
    /// (`manager/src/services/platforms.rs:846-848`), which is one column read
    /// on the platform row — no fallback chain of its own. Legacy's "platform"
    /// is this gear's `Environment`; the names in that citation are legacy's
    /// and do not rename with it. `None` covers two legacy-distinct cases this
    /// port does not distinguish, deliberately, for [`Self::names`]'s own
    /// reason: an environment with the column unset, and an environment id
    /// naming nothing this caller can see (deleted, or another tenant's). Both
    /// mean the same thing to every caller of this method — resolve the run's
    /// branch some other way — and a caller must not be able to use this read
    /// to learn *which* of the two it got.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] for any transport or gateway failure, and
    /// [`DomainError::Forbidden`] when qa-environments refuses the subject.
    /// There is no not-found case: an unresolvable `environment_id` is
    /// `Ok(None)`, exactly like an id absent from [`Self::names`]' map.
    async fn default_branch(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
    ) -> Result<Option<String>, DomainError>;
}
