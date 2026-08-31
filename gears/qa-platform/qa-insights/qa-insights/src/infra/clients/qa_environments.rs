//! [`PlatformReader`] over `qa_environments_sdk::QaEnvironmentsClientV1`.
//!
//! # The design decision this file makes, and the evidence for it
//!
//! The SDK offers **two** platform reads and nothing batched
//! (`qa-environments-sdk/src/client.rs:26-35`; this cited `:26-37`, which runs
//! two lines into `create_platform`):
//! `get_platform(ctx, id) -> TargetPlatform` and
//! `list_platforms(ctx) -> Vec<TargetPlatform>`. The port's contract is *"ids in,
//! names out, one call"*, so one of them has to be chosen and the choice is not
//! obvious: N round trips against one read of every platform in the tenant.
//!
//! **`list_platforms`, once.** Four reasons, in the order they decided it:
//!
//! 1. **`get_platform` is the N+1 the sibling port refuses by name.**
//!    [`CatalogReader::list_universe`](crate::domain::ports::CatalogReader::list_universe)'s
//!    header: *"an N+1 across the SDK boundary would make the overview endpoint's
//!    latency a function of plan count."* Here it would be a function of how many
//!    distinct platforms the window's rows ran on — a number this gear does not
//!    control and cannot bound, since it comes out of `qa_test_results`.
//! 2. **`list_platforms` is unbounded and that is *safe here*, unlike for runs.**
//!    `QaRunsClientV1::list_runs`' `limit` is **mandatory**, and its own doc says
//!    why: *"`qa_runs` grows strictly faster than the queue and never drains, so
//!    an unbounded inter-gear call would materialize every run ever executed"*
//!    (`qa-runs-sdk/src/client.rs:42-46`). Platforms are the opposite kind of
//!    table: operator-provisioned infrastructure, one row per system under test,
//!    written only by `create_platform`. It has no paging and no limit
//!    (`qa-environments/src/domain/repos/platforms_repo.rs`' `list`, and the
//!    service method at `domain/service/platforms.rs:143-161`, which passes the
//!    compiled scope and nothing else) — because there is nothing for it to
//!    bound. So the "one read of everything" arm is a read of tens of rows, and
//!    the comparison is one round trip against N.
//! 3. **`get_platform` forces a `NotFound` to be swallowed.** The port's contract
//!    makes an unresolvable id *absent from the map*, not an error — so a
//!    per-id implementation would have to catch `NotFound` and continue, which is
//!    exactly the laundering [`super::qa_runs`]' header spends its length arguing
//!    against: `NotFound` from a sibling covers "does not exist" **and** "not
//!    visible to you", and a loop that treats it as "skip this one" makes every
//!    other far-side failure look like a missing platform. With `list_platforms`
//!    there is no `NotFound` to launder — absence is set difference, and the
//!    error mapping keeps its no-not-found arm.
//! 4. **One call is one authorization decision.** N calls are N PDP evaluations
//!    on the far side for one rendered chart.
//!
//! The cost is real and is recorded rather than hidden: the tenant's whole
//! platform list crosses the boundary to answer a question about a subset of it,
//! and a tenant with thousands of platforms would make the second argument false.
//! Nothing here caps it, because a cap that silently dropped a platform would
//! silently unlabel a bar. If that day comes the fix is a batched read on the
//! **SDK** — `names_of(ids)` on qa-environments' side — and this adapter is the
//! one call site that would change.
//!
//! # It translates errors and filters, and nothing else
//!
//! No sort, no re-key, no fallback label. The map is the ids that resolved, and
//! [`PlatformReader::names`]' contract fixes what an absent key means and who
//! decides how it renders (25b's DTO, exactly as for
//! [`ExecRow::run_id`](crate::domain::analytics::ExecRow::run_id)).
//!
//! # The error translation, and its missing `NotFound` arm
//!
//! `QaEnvironmentsError` is `toolkit_canonical_errors::CanonicalError`
//! (`qa-environments-sdk/src/errors.rs`), the same alias `QaRunsError` and
//! `QaCatalogError` are — so this file is the listing half of
//! [`super::qa_runs`]' table, identical in shape to
//! [`super::qa_catalog`]'s. `PermissionDenied` and `Unauthenticated` are
//! decisions about the **subject** and become [`DomainError::Forbidden`];
//! everything else, `NotFound` included, becomes [`DomainError::Internal`].
//!
//! **A `Forbidden` here fails the whole read, and does not degrade to an empty
//! map.** That is the port's decision, not this file's, and its `# Errors`
//! section carries the argument: an operator whose `platform:list` grant is
//! missing would otherwise be shown a chart whose bars are labelled with whatever
//! the DTO's fallback is, indistinguishable from a tenant that has registered no
//! platforms. What a *caller* does with that error — fail the payload, or render
//! the other sections without this one — is the assembling service's decision and
//! is not made here.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use qa_environments_sdk::{QaEnvironmentsClientV1, QaEnvironmentsError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::PlatformReader;

/// The qa-environments reads, over the `ClientHub`-resolved client.
pub struct QaEnvironmentsReader {
    client: Arc<dyn QaEnvironmentsClientV1>,
}

impl QaEnvironmentsReader {
    #[must_use]
    pub const fn new(client: Arc<dyn QaEnvironmentsClientV1>) -> Self {
        Self { client }
    }
}

/// A qa-environments error on a read that addresses no single platform.
///
/// Deliberately has **no** `NotFound` arm. `list_platforms` on a tenant with no
/// platforms is an empty `Vec`, so a `NotFound` on it is a contract break on the
/// far side rather than an empty answer — and it is the arm a `get_platform`-based
/// implementation would have been forced to swallow, which is reason 3 in this
/// module's header. `a_not_found_is_internal_rather_than_an_empty_map` is what
/// fails if one is added.
fn on_subject(err: QaEnvironmentsError) -> DomainError {
    match err {
        QaEnvironmentsError::PermissionDenied { .. }
        | QaEnvironmentsError::Unauthenticated { .. } => DomainError::Forbidden,
        // `{other}` rather than the raw detail: `CanonicalError`'s `Display` is
        // the sibling's own rendering, and this string never reaches a client —
        // `DomainError::Internal` is mapped to an opaque 500.
        other => DomainError::Internal(format!("qa-environments read failed: {other}")),
    }
}

#[async_trait]
impl PlatformReader for QaEnvironmentsReader {
    async fn names(
        &self,
        ctx: &SecurityContext,
        ids: &[Uuid],
    ) -> Result<HashMap<Uuid, String>, DomainError> {
        // No ids, no round trip. The port states this as contract rather than
        // leaving it to the far side to answer an empty question, because a
        // caller holding only the map cannot tell an empty answer from a skipped
        // call. `no_ids_means_no_cross_gear_call` pins it.
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        // A set, not a linear scan per platform: the listing is the tenant's
        // whole platform table and `ids` is the distinct platforms of one
        // window, so the product of the two is the only quantity here that
        // could grow.
        let wanted: HashSet<Uuid> = ids.iter().copied().collect();

        Ok(self
            .client
            .list_platforms(ctx)
            .await
            .map_err(on_subject)?
            .into_iter()
            // The filter is what keeps the answer the caller's *question*.
            // Returning the whole listing would compile, would look right on a
            // lookup, and would make the map's length the tenant's platform
            // count rather than "how many of my ids resolved" — the one thing
            // `PlatformReader::names`' doc tells a caller it can compute.
            // `platforms_the_caller_did_not_ask_about_are_not_returned` fails
            // without it.
            .filter(|platform| wanted.contains(&platform.id))
            .map(|platform| (platform.id, platform.name))
            .collect())
    }

    /// One `get_platform` call — Task 35's single-id read, unlike [`Self::names`]'
    /// batch. `NotFound` folds to `Ok(None)`, the same "absent, not an error"
    /// answer [`Self::names`] gives a deleted or foreign platform id; every
    /// other failure still takes the subject-level path.
    async fn default_branch(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<Option<String>, DomainError> {
        match self.client.get_platform(ctx, platform_id).await {
            Ok(platform) => Ok(platform.default_branch),
            Err(QaEnvironmentsError::NotFound { .. }) => Ok(None),
            Err(other) => Err(on_subject(other)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! This adapter has behaviour, so unlike [`super::super::qa_runs`]' it gets a
    //! client double.
    //!
    //! That module's tests drive its two mapping functions directly and argue
    //! against a fake client, because *every* method there is one call plus one
    //! `map_err` and a fake would add `unimplemented!()`s between the assertion
    //! and the decision. This adapter is the exception the argument allows for:
    //! it turns one read of every platform into a lookup over the ids the caller
    //! asked about, and the two properties worth pinning — **one** call rather
    //! than one per id, and **no** call at all for no ids — are only observable
    //! through a client that counts.
    //!
    //! [`CountingEnvironments`] is therefore nine `unimplemented!()`s around the
    //! two platform reads, which is exactly the cost
    //! [`crate::domain::ports::platform_reader`]' header cites (eleven methods,
    //! one needed). It is paid once, here, so no test in `domain::` has to.
    //!
    //! # Six tests, and there were seven
    //!
    //! `a_repeated_id_costs_one_entry_and_one_call` was folded into
    //! [`many_ids_are_resolved_in_one_call_and_never_one_per_id`] in Task 25a's
    //! first fix round: no mutation of this file reddened it that did not also
    //! redden that one, because "a repeated id is one entry" is a property of
    //! `HashMap` rather than of any code here. Its fixture survives — the
    //! surviving test now passes `[b, a, a, c]`. Recorded rather than silently
    //! deleted, because a reader who remembers the duplicate case should find out
    //! where it went.
    //!
    //! # `get_platform` is asserted **never** to be called
    //!
    //! That is this file's design decision made executable rather than only
    //! documented. `unimplemented!()` would have done it more bluntly, and a
    //! counter is better: `an_unresolvable_id_is_absent_rather_than_an_error` has
    //! to be able to distinguish "the adapter did not ask about that id" from
    //! "the adapter asked and swallowed a `NotFound`", and a panic cannot tell
    //! those apart from the outside.

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use qa_environments_sdk::{
        AcquireOutcome, LeaseMode, LeaseState, NewPlatform, NewVariable, PlatformPatch,
        QaEnvironmentsClientV1, QaEnvironmentsError, TargetPlatform, Variable,
    };
    use time::OffsetDateTime;
    use toolkit::api::canonical_prelude::*;
    use toolkit_security::SecurityContext;

    use super::{QaEnvironmentsReader, on_subject};
    use crate::domain::error::DomainError;
    use crate::domain::ports::PlatformReader;
    use std::sync::Arc;
    use uuid::Uuid;

    /// A far-side failure, built on demand. Named rather than written inline
    /// because `Mutex<Option<fn() -> QaEnvironmentsError>>` trips
    /// `clippy::type_complexity`.
    type ErrorFactory = fn() -> QaEnvironmentsError;

    /// qa-environments' platform resource, spelled exactly as
    /// `qa-environments/src/api/rest/error.rs:12` spells it — this is the id its
    /// errors carry.
    #[resource_error(gts_id!("cf.qa.environments.platform.v1~"))]
    struct FarSide;

    /// A platform with `id` and `name` and nothing else distinguishing.
    fn platform(id: Uuid, name: &str) -> TargetPlatform {
        TargetPlatform {
            id,
            name: name.to_owned(),
            product_id: None,
            description: None,
            kubeconfig_credstore_ref: "credstore://k".to_owned(),
            available: true,
            observed_version: None,
            observed_build: None,
            default_branch: None,
            is_default: false,
            // Added 2026-08-28 by the platform-observation work
            // (qa-environments Task 7); this fixture predates it and has
            // nothing to say about observation, so every one is `None`.
            vhp_base_url: None,
            observed_namespace: None,
            version_detect_error: None,
            version_detected_at: None,
            // Added 2026-08-28 by the cluster-health work (qa-environments
            // Task 5); this fixture predates it and has nothing to say about
            // cluster health, so it is `None`.
            cluster: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// `platform` with a default-branch override set — Task 35's fixture.
    fn platform_with_branch(id: Uuid, branch: &str) -> TargetPlatform {
        TargetPlatform {
            default_branch: Some(branch.to_owned()),
            is_default: false,
            ..platform(id, "with-branch")
        }
    }

    /// The two platform reads, counted; every other method of the eleven is
    /// `unimplemented!()`.
    #[derive(Default)]
    struct CountingEnvironments {
        platforms: Mutex<Vec<TargetPlatform>>,
        /// [`QaEnvironmentsClientV1::list_platforms`] calls.
        listings: AtomicUsize,
        /// [`QaEnvironmentsClientV1::get_platform`] calls. Asserted to stay `0`
        /// by every `names` test — [`QaEnvironmentsReader::default_branch`]'s
        /// Task 35 tests are this counter's one legitimate non-zero caller.
        gets: AtomicUsize,
        /// Makes the next read fail with this error instead of answering —
        /// [`QaEnvironmentsClientV1::list_platforms`] and, since Task 35,
        /// [`QaEnvironmentsClientV1::get_platform`] too.
        fail_listing: Mutex<Option<ErrorFactory>>,
    }

    impl CountingEnvironments {
        fn with(platforms: Vec<TargetPlatform>) -> Arc<Self> {
            Arc::new(Self {
                platforms: Mutex::new(platforms),
                ..Self::default()
            })
        }

        fn failing(err: ErrorFactory) -> Arc<Self> {
            Arc::new(Self {
                fail_listing: Mutex::new(Some(err)),
                ..Self::default()
            })
        }
    }

    #[async_trait::async_trait]
    impl QaEnvironmentsClientV1 for CountingEnvironments {
        async fn get_platform(
            &self,
            _ctx: &SecurityContext,
            id: Uuid,
        ) -> Result<TargetPlatform, QaEnvironmentsError> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            if let Some(err) = *self.fail_listing.lock().unwrap() {
                return Err(err());
            }
            self.platforms
                .lock()
                .unwrap()
                .iter()
                .find(|item| item.id == id)
                .cloned()
                .ok_or_else(|| {
                    FarSide::not_found("no such platform".to_owned())
                        .with_resource(id.to_string())
                        .create()
                })
        }

        async fn list_platforms(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<Vec<TargetPlatform>, QaEnvironmentsError> {
            self.listings.fetch_add(1, Ordering::SeqCst);
            if let Some(err) = *self.fail_listing.lock().unwrap() {
                return Err(err());
            }
            Ok(self.platforms.lock().unwrap().clone())
        }

        async fn create_platform(
            &self,
            _ctx: &SecurityContext,
            _new: NewPlatform,
        ) -> Result<TargetPlatform, QaEnvironmentsError> {
            unimplemented!("qa-insights performs no platform writes")
        }

        async fn update_platform(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
            _patch: PlatformPatch,
        ) -> Result<TargetPlatform, QaEnvironmentsError> {
            unimplemented!("qa-insights performs no platform writes")
        }

        async fn delete_platform(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), QaEnvironmentsError> {
            unimplemented!("qa-insights performs no platform writes")
        }

        async fn list_variables(
            &self,
            _ctx: &SecurityContext,
            _platform_id: Option<Uuid>,
        ) -> Result<Vec<Variable>, QaEnvironmentsError> {
            unimplemented!("qa-insights reads no environment variables")
        }

        async fn upsert_variable(
            &self,
            _ctx: &SecurityContext,
            _var: NewVariable,
        ) -> Result<Variable, QaEnvironmentsError> {
            unimplemented!("qa-insights reads no environment variables")
        }

        async fn delete_variable(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), QaEnvironmentsError> {
            unimplemented!("qa-insights reads no environment variables")
        }

        async fn acquire_lease(
            &self,
            _ctx: &SecurityContext,
            _platform_id: Uuid,
            _run_id: Uuid,
            _mode: LeaseMode,
        ) -> Result<AcquireOutcome, QaEnvironmentsError> {
            unimplemented!("qa-insights takes no leases")
        }

        async fn release_lease(
            &self,
            _ctx: &SecurityContext,
            _platform_id: Uuid,
            _run_id: Uuid,
        ) -> Result<LeaseState, QaEnvironmentsError> {
            unimplemented!("qa-insights takes no leases")
        }

        async fn get_lease(
            &self,
            _ctx: &SecurityContext,
            _platform_id: Uuid,
        ) -> Result<LeaseState, QaEnvironmentsError> {
            unimplemented!("qa-insights takes no leases")
        }
    }

    /// An operator context. Nothing here reads it — the double ignores it — but
    /// the port takes one, and `crate::domain::service::test_support::ctx` is the
    /// gear's one builder for it.
    fn ctx() -> SecurityContext {
        crate::domain::service::test_support::ctx(Uuid::new_v4())
    }

    /// **The design decision, as an assertion.** Three distinct ids — handed over
    /// unsorted and with one of them repeated — cost **one** cross-gear call and
    /// **zero** `get_platform`s.
    ///
    /// `qa_environments_sdk::QaEnvironmentsClientV1` offers both shapes and
    /// nothing batched, so this is a choice — `infra::clients::qa_environments`'
    /// header carries the evidence. The counters are what make it hold: with
    /// `get_platform` the answer would be identical and the latency a function of
    /// how many platforms the window's rows ran on, which is the N+1
    /// `CatalogReader::list_universe` refuses for plans.
    ///
    /// # The duplicate and the ordering are in *this* fixture on purpose
    ///
    /// They were `a_repeated_id_costs_one_entry_and_one_call`, a separate test,
    /// and it was **folded in here because no mutation of this file reddened it
    /// without also reddening this one**: a `HashMap` cannot hold a duplicate key,
    /// so "one entry per repeated id" is a property of the return type rather than
    /// of any code, and the single-call half was already asserted below. A test
    /// whose every failure mode belongs to another test is upkeep without
    /// coverage.
    ///
    /// What survives of it is the *fixture*: `[b, a, a, c]` is what a caller
    /// handing over `rows.iter().filter_map(ExecRow::platform_id)` unreduced
    /// actually passes, so this test now exercises the shape the port's doc
    /// promises ("duplicates and ordering are the caller's business") rather than
    /// a tidied one.
    #[tokio::test]
    async fn many_ids_are_resolved_in_one_call_and_never_one_per_id() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let client = CountingEnvironments::with(vec![
            platform(a, "eu-west"),
            platform(b, "us-east"),
            platform(c, "lab-3"),
        ]);
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        let names = reader.names(&ctx(), &[b, a, a, c]).await.expect("resolved");

        assert_eq!(names.get(&a).map(String::as_str), Some("eu-west"));
        assert_eq!(names.get(&b).map(String::as_str), Some("us-east"));
        assert_eq!(names.get(&c).map(String::as_str), Some("lab-3"));
        assert_eq!(names.len(), 3, "four ids, three distinct: {names:?}");
        assert_eq!(
            client.listings.load(Ordering::SeqCst),
            1,
            "four ids must cost one listing, not four lookups",
        );
        assert_eq!(
            client.gets.load(Ordering::SeqCst),
            0,
            "get_platform is the per-id shape this adapter exists not to use",
        );
    }

    /// No ids means no cross-gear call at all — not an empty listing.
    ///
    /// A request whose rows carry no platform (nothing has run, or every row
    /// predates the column) must not pay a round trip to be told so. The port's
    /// doc states it as contract because a reader of the map alone cannot tell an
    /// empty answer from a skipped call.
    #[tokio::test]
    async fn no_ids_means_no_cross_gear_call() {
        let client = CountingEnvironments::with(vec![platform(Uuid::new_v4(), "eu-west")]);
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        assert!(
            reader
                .names(&ctx(), &[])
                .await
                .expect("resolved")
                .is_empty()
        );
        assert_eq!(client.listings.load(Ordering::SeqCst), 0);
        assert_eq!(client.gets.load(Ordering::SeqCst), 0);
    }

    /// **An id that resolves to nothing is absent from the map, and the call
    /// still succeeds.**
    ///
    /// A platform deleted since the run executed, and a platform in another
    /// tenant, both arrive here as "not in the listing". The port's contract makes
    /// that the *answer*: the resolvable ids resolve, the rest are missing keys,
    /// and no label is invented for them — 25b's DTO decides what an absent name
    /// renders as, exactly as it decides `run_id`'s.
    ///
    /// The `gets` assertion is load-bearing rather than decorative: it is what
    /// distinguishes "never asked about that id" from "asked and swallowed a
    /// `NotFound`", and the second would be a laundered existence oracle.
    #[tokio::test]
    async fn an_unresolvable_id_is_absent_rather_than_an_error() {
        let known = Uuid::new_v4();
        let gone = Uuid::new_v4();
        let client = CountingEnvironments::with(vec![platform(known, "eu-west")]);
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        let names = reader
            .names(&ctx(), &[known, gone])
            .await
            .expect("resolved");

        assert_eq!(names.len(), 1, "two asked, one resolved: {names:?}");
        assert_eq!(names.get(&known).map(String::as_str), Some("eu-west"));
        assert!(
            !names.contains_key(&gone),
            "an unresolvable id must be absent, not present under an invented label",
        );
        assert_eq!(client.gets.load(Ordering::SeqCst), 0);
    }

    /// Platforms the caller did not ask about are **not** in the map.
    ///
    /// The listing reads every platform in the tenant, so the filter is what
    /// keeps the answer the caller's question. Without it `names.len()` would be
    /// the tenant's platform count rather than "how many of my ids resolved",
    /// which is the one thing the port's doc tells a caller it can compute.
    #[tokio::test]
    async fn platforms_the_caller_did_not_ask_about_are_not_returned() {
        let wanted = Uuid::new_v4();
        let client = CountingEnvironments::with(vec![
            platform(Uuid::new_v4(), "unrelated-a"),
            platform(wanted, "eu-west"),
            platform(Uuid::new_v4(), "unrelated-b"),
        ]);
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        let names = reader.names(&ctx(), &[wanted]).await.expect("resolved");

        assert_eq!(names.len(), 1, "{names:?}");
        assert_eq!(names.get(&wanted).map(String::as_str), Some("eu-west"));
    }

    /// A subject-level refusal is [`DomainError::Forbidden`] and **not** an empty
    /// map.
    ///
    /// The port's `# Errors` section states the consequence this pins: an
    /// operator missing the `platform:list` grant must not be shown a group
    /// chart labelled with nothing and no indication why. Degrading here would
    /// make that indistinguishable from a tenant with no platforms.
    #[tokio::test]
    async fn a_denial_is_forbidden_rather_than_an_unlabelled_chart() {
        let client = CountingEnvironments::failing(|| {
            FarSide::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create()
        });
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        assert!(matches!(
            reader.names(&ctx(), &[Uuid::new_v4()]).await,
            Err(DomainError::Forbidden)
        ));
    }

    /// **`NotFound` has no arm**, and that is the load-bearing omission — the
    /// same one `infra::clients::qa_catalog` carries.
    ///
    /// `list_platforms` on a tenant with no platforms is an empty `Vec`, so a
    /// `NotFound` on it is a contract break on the far side. Laundering it into
    /// an empty map would label every platform bar with whatever the DTO's
    /// fallback is, while a broken qa-environments looked like a deployment that
    /// had simply not registered any.
    #[test]
    fn a_not_found_is_internal_rather_than_an_empty_map() {
        let err = FarSide::not_found("no such platform".to_owned())
            .with_resource("some-platform")
            .create();
        match on_subject(err) {
            DomainError::Internal(msg) => assert!(msg.contains("qa-environments"), "{msg}"),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // `default_branch` — Task 35
    // -----------------------------------------------------------------------

    /// The happy path: the platform's `default_branch` override crosses
    /// unchanged.
    #[tokio::test]
    async fn a_platforms_branch_override_is_returned() {
        let id = Uuid::new_v4();
        let client = CountingEnvironments::with(vec![platform_with_branch(id, "release-1.2")]);
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        let branch = reader.default_branch(&ctx(), id).await.expect("resolved");

        assert_eq!(branch.as_deref(), Some("release-1.2"));
    }

    /// No override is `None`, not an error — the common case, since most
    /// platforms inherit the repository's own default.
    #[tokio::test]
    async fn a_platform_with_no_override_resolves_to_none() {
        let id = Uuid::new_v4();
        let client = CountingEnvironments::with(vec![platform(id, "no-override")]);
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        assert_eq!(
            reader.default_branch(&ctx(), id).await.expect("resolved"),
            None
        );
    }

    /// **A platform that resolves to nothing is `Ok(None)`, exactly like an id
    /// absent from [`PlatformReader::names`]' map** — not an error, and not
    /// laundered from a swallowed `NotFound` the way [`Self::names`] refuses
    /// to for its own batch read. This method's whole reason to use
    /// `get_platform` rather than `list_platforms` is the single-id shape, so
    /// its `NotFound` arm has to be handled here rather than avoided.
    #[tokio::test]
    async fn an_unresolvable_platform_is_none_rather_than_an_error() {
        let client = CountingEnvironments::with(vec![]);
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        assert_eq!(
            reader
                .default_branch(&ctx(), Uuid::new_v4())
                .await
                .expect("an unresolvable id is not an error"),
            None,
        );
    }

    /// A subject-level refusal is still [`DomainError::Forbidden`], not `None`
    /// — the same rule [`Self::names`] applies, restated for the single-id
    /// method.
    #[tokio::test]
    async fn a_denied_default_branch_read_is_forbidden() {
        let client = CountingEnvironments::failing(|| {
            FarSide::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create()
        });
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        assert!(matches!(
            reader.default_branch(&ctx(), Uuid::new_v4()).await,
            Err(DomainError::Forbidden)
        ));
    }
}
