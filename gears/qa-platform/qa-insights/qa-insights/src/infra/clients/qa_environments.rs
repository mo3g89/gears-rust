//! [`EnvironmentReader`] over `qa_environments_sdk::QaEnvironmentsClientV1`.
//!
//! # The design decision this file makes, and the evidence for it
//!
//! The SDK offers **two** environment reads and nothing batched
//! (`qa-environments-sdk/src/client.rs:26-35`; this cited `:26-37`, which runs
//! two lines into `create_environment`):
//! `get_environment(ctx, id) -> Environment` and
//! `list_environments(ctx) -> Vec<Environment>`. The port's contract is *"ids in,
//! names out, one call"*, so one of them has to be chosen and the choice is not
//! obvious: N round trips against one read of every environment in the tenant.
//!
//! **`list_environments`, once.** Four reasons, in the order they decided it:
//!
//! 1. **`get_environment` is the N+1 the sibling port refuses by name.**
//!    [`CatalogReader::list_universe`](crate::domain::ports::CatalogReader::list_universe)'s
//!    header: *"an N+1 across the SDK boundary would make the overview endpoint's
//!    latency a function of plan count."* Here it would be a function of how many
//!    distinct environments the window's rows ran on — a number this gear does not
//!    control and cannot bound, since it comes out of `qa_test_results`.
//! 2. **`list_environments` is unbounded and that is *safe here*, unlike for runs.**
//!    `QaRunsClientV1::list_runs`' `limit` is **mandatory**, and its own doc says
//!    why: *"`qa_runs` grows strictly faster than the queue and never drains, so
//!    an unbounded inter-gear call would materialize every run ever executed"*
//!    (`qa-runs-sdk/src/client.rs:42-46`). Environments are the opposite kind of
//!    table: operator-provisioned infrastructure, one row per system under test,
//!    written only by `create_environment`. It has no paging and no limit
//!    (`qa-environments/src/domain/repos/environments_repo.rs`' `list`, and the
//!    service method at `domain/service/environments.rs:230-247`, which passes the
//!    compiled scope and nothing else) — because there is nothing for it to
//!    bound. So the "one read of everything" arm is a read of tens of rows, and
//!    the comparison is one round trip against N.
//! 3. **`get_environment` forces a `NotFound` to be swallowed.** The port's contract
//!    makes an unresolvable id *absent from the map*, not an error — so a
//!    per-id implementation would have to catch `NotFound` and continue, which is
//!    exactly the laundering [`super::qa_runs`]' header spends its length arguing
//!    against: `NotFound` from a sibling covers "does not exist" **and** "not
//!    visible to you", and a loop that treats it as "skip this one" makes every
//!    other far-side failure look like a missing environment. With `list_environments`
//!    there is no `NotFound` to launder — absence is set difference, and the
//!    error mapping keeps its no-not-found arm.
//! 4. **One call is one authorization decision.** N calls are N PDP evaluations
//!    on the far side for one rendered chart.
//!
//! The cost is real and is recorded rather than hidden: the tenant's whole
//! environment list crosses the boundary to answer a question about a subset of it,
//! and a tenant with thousands of environments would make the second argument false.
//! Nothing here caps it, because a cap that silently dropped an environment would
//! silently unlabel a bar. If that day comes the fix is a batched read on the
//! **SDK** — `names_of(ids)` on qa-environments' side — and this adapter is the
//! one call site that would change.
//!
//! # It translates errors and filters, and nothing else
//!
//! No sort, no re-key, no fallback label. The map is the ids that resolved, and
//! [`EnvironmentReader::names`]' contract fixes what an absent key means and who
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
//! environments. What a *caller* does with that error — fail the payload, or render
//! the other sections without this one — is the assembling service's decision and
//! is not made here.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use qa_environments_sdk::{QaEnvironmentsClientV1, QaEnvironmentsError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::EnvironmentReader;

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

/// A qa-environments error on a read that addresses no single environment.
///
/// Deliberately has **no** `NotFound` arm. `list_environments` on a tenant with no
/// environments is an empty `Vec`, so a `NotFound` on it is a contract break on the
/// far side rather than an empty answer — and it is the arm a `get_environment`-based
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
impl EnvironmentReader for QaEnvironmentsReader {
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

        // A set, not a linear scan per environment: the listing is the tenant's
        // whole environment table and `ids` is the distinct environments of one
        // window, so the product of the two is the only quantity here that
        // could grow.
        let wanted: HashSet<Uuid> = ids.iter().copied().collect();

        Ok(self
            .client
            .list_environments(ctx)
            .await
            .map_err(on_subject)?
            .into_iter()
            // The filter is what keeps the answer the caller's *question*.
            // Returning the whole listing would compile, would look right on a
            // lookup, and would make the map's length the tenant's environment
            // count rather than "how many of my ids resolved" — the one thing
            // `EnvironmentReader::names`' doc tells a caller it can compute.
            // `platforms_the_caller_did_not_ask_about_are_not_returned` fails
            // without it.
            .filter(|environment| wanted.contains(&environment.id))
            .map(|environment| (environment.id, environment.name))
            .collect())
    }

    /// One `get_environment` call — Task 35's single-id read, unlike [`Self::names`]'
    /// batch. `NotFound` folds to `Ok(None)`, the same "absent, not an error"
    /// answer [`Self::names`] gives a deleted or foreign environment id; every
    /// other failure still takes the subject-level path.
    async fn default_branch(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
    ) -> Result<Option<String>, DomainError> {
        match self.client.get_environment(ctx, environment_id).await {
            Ok(environment) => Ok(environment.default_branch),
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
    //! it turns one read of every environment into a lookup over the ids the caller
    //! asked about, and the two properties worth pinning — **one** call rather
    //! than one per id, and **no** call at all for no ids — are only observable
    //! through a client that counts.
    //!
    //! [`CountingEnvironments`] is therefore nine `unimplemented!()`s around the
    //! two environment reads, which is exactly the cost
    //! [`crate::domain::ports::environment_reader`]' header cites (eleven methods,
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
    //! # `get_environment` is asserted **never** to be called
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
        AcquireOutcome, Environment, EnvironmentPatch, LeaseMode, LeaseState, NewEnvironment,
        NewVariable, QaEnvironmentsClientV1, QaEnvironmentsError, Variable,
    };
    use time::OffsetDateTime;
    use toolkit::api::canonical_prelude::*;
    use toolkit_security::SecurityContext;

    use super::{QaEnvironmentsReader, on_subject};
    use crate::domain::error::DomainError;
    use crate::domain::ports::EnvironmentReader;
    use std::sync::Arc;
    use uuid::Uuid;

    /// A far-side failure, built on demand. Named rather than written inline
    /// because `Mutex<Option<fn() -> QaEnvironmentsError>>` trips
    /// `clippy::type_complexity`.
    type ErrorFactory = fn() -> QaEnvironmentsError;

    /// qa-environments' platform resource, spelled exactly as
    /// `qa-environments/src/api/rest/error.rs:20` spells it — this is the id its
    /// errors carry. The GTS id keeps the name `platform` after the aggregate's
    /// rename to `Environment`, deliberately — see that file's own doc.
    #[resource_error(gts_id!("cf.qa.environments.platform.v1~"))]
    struct FarSide;

    /// An environment with `id` and `name` and nothing else distinguishing.
    fn environment(id: Uuid, name: &str) -> Environment {
        Environment {
            id,
            name: name.to_owned(),
            // Required since qa-environments' Task 20b.
            product_id: uuid::Uuid::from_u128(0x9001),
            description: None,
            available: true,
            observed_version: None,
            observed_build: None,
            default_branch: None,
            is_default: false,
            version_detect_error: None,
            version_detected_at: None,
            // Nothing has been observed through the plugin path (qa-environments
            // Task 14): every value is the one a never-observed environment holds.
            credentials: Vec::new(),
            observed_attrs: qa_environments_sdk::ObservedAttrs::default(),
            config: serde_json::json!({}),
            observed_base_url: None,
            health_state: qa_environments_sdk::HealthState::Unknown,
            health_detail: None,
            health_checked_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// `environment` with a default-branch override set — Task 35's fixture.
    fn environment_with_branch(id: Uuid, branch: &str) -> Environment {
        Environment {
            default_branch: Some(branch.to_owned()),
            is_default: false,
            ..environment(id, "with-branch")
        }
    }

    /// The two environment reads, counted; every other method of the eleven is
    /// `unimplemented!()`.
    #[derive(Default)]
    struct CountingEnvironments {
        environments: Mutex<Vec<Environment>>,
        /// [`QaEnvironmentsClientV1::list_environments`] calls.
        listings: AtomicUsize,
        /// [`QaEnvironmentsClientV1::get_environment`] calls. Asserted to stay `0`
        /// by every `names` test — [`QaEnvironmentsReader::default_branch`]'s
        /// Task 35 tests are this counter's one legitimate non-zero caller.
        gets: AtomicUsize,
        /// Makes the next read fail with this error instead of answering —
        /// [`QaEnvironmentsClientV1::list_environments`] and, since Task 35,
        /// [`QaEnvironmentsClientV1::get_environment`] too.
        fail_listing: Mutex<Option<ErrorFactory>>,
    }

    impl CountingEnvironments {
        fn with(environments: Vec<Environment>) -> Arc<Self> {
            Arc::new(Self {
                environments: Mutex::new(environments),
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
        async fn get_environment(
            &self,
            _ctx: &SecurityContext,
            id: Uuid,
        ) -> Result<Environment, QaEnvironmentsError> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            if let Some(err) = *self.fail_listing.lock().unwrap() {
                return Err(err());
            }
            self.environments
                .lock()
                .unwrap()
                .iter()
                .find(|item| item.id == id)
                .cloned()
                .ok_or_else(|| {
                    FarSide::not_found("no such environment".to_owned())
                        .with_resource(id.to_string())
                        .create()
                })
        }

        async fn list_environments(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<Vec<Environment>, QaEnvironmentsError> {
            self.listings.fetch_add(1, Ordering::SeqCst);
            if let Some(err) = *self.fail_listing.lock().unwrap() {
                return Err(err());
            }
            Ok(self.environments.lock().unwrap().clone())
        }

        async fn create_environment(
            &self,
            _ctx: &SecurityContext,
            _new: NewEnvironment,
        ) -> Result<Environment, QaEnvironmentsError> {
            unimplemented!("qa-insights performs no environment writes")
        }

        async fn update_environment(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
            _patch: EnvironmentPatch,
        ) -> Result<Environment, QaEnvironmentsError> {
            unimplemented!("qa-insights performs no environment writes")
        }

        async fn delete_environment(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), QaEnvironmentsError> {
            unimplemented!("qa-insights performs no environment writes")
        }

        async fn list_variables(
            &self,
            _ctx: &SecurityContext,
            _environment_id: Option<Uuid>,
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
            _environment_id: Uuid,
            _run_id: Uuid,
            _mode: LeaseMode,
        ) -> Result<AcquireOutcome, QaEnvironmentsError> {
            unimplemented!("qa-insights takes no leases")
        }

        async fn release_lease(
            &self,
            _ctx: &SecurityContext,
            _environment_id: Uuid,
            _run_id: Uuid,
        ) -> Result<LeaseState, QaEnvironmentsError> {
            unimplemented!("qa-insights takes no leases")
        }

        async fn get_lease(
            &self,
            _ctx: &SecurityContext,
            _environment_id: Uuid,
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
    /// **zero** `get_environment`s.
    ///
    /// `qa_environments_sdk::QaEnvironmentsClientV1` offers both shapes and
    /// nothing batched, so this is a choice — `infra::clients::qa_environments`'
    /// header carries the evidence. The counters are what make it hold: with
    /// `get_environment` the answer would be identical and the latency a function of
    /// how many environments the window's rows ran on, which is the N+1
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
            environment(a, "eu-west"),
            environment(b, "us-east"),
            environment(c, "lab-3"),
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
            "get_environment is the per-id shape this adapter exists not to use",
        );
    }

    /// No ids means no cross-gear call at all — not an empty listing.
    ///
    /// A request whose rows carry no environment (nothing has run, or every row
    /// predates the column) must not pay a round trip to be told so. The port's
    /// doc states it as contract because a reader of the map alone cannot tell an
    /// empty answer from a skipped call.
    #[tokio::test]
    async fn no_ids_means_no_cross_gear_call() {
        let client = CountingEnvironments::with(vec![environment(Uuid::new_v4(), "eu-west")]);
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
    /// An environment deleted since the run executed, and an environment in
    /// another tenant, both arrive here as "not in the listing". The port's contract makes
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
        let client = CountingEnvironments::with(vec![environment(known, "eu-west")]);
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

    /// Environments the caller did not ask about are **not** in the map.
    ///
    /// The listing reads every environment in the tenant, so the filter is what
    /// keeps the answer the caller's question. Without it `names.len()` would be
    /// the tenant's environment count rather than "how many of my ids resolved",
    /// which is the one thing the port's doc tells a caller it can compute.
    #[tokio::test]
    async fn platforms_the_caller_did_not_ask_about_are_not_returned() {
        let wanted = Uuid::new_v4();
        let client = CountingEnvironments::with(vec![
            environment(Uuid::new_v4(), "unrelated-a"),
            environment(wanted, "eu-west"),
            environment(Uuid::new_v4(), "unrelated-b"),
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
    /// make that indistinguishable from a tenant with no environments.
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
    /// `list_environments` on a tenant with no environments is an empty `Vec`, so a
    /// `NotFound` on it is a contract break on the far side. Laundering it into
    /// an empty map would label every environment bar with whatever the DTO's
    /// fallback is, while a broken qa-environments looked like a deployment that
    /// had simply not registered any.
    #[test]
    fn a_not_found_is_internal_rather_than_an_empty_map() {
        let err = FarSide::not_found("no such environment".to_owned())
            .with_resource("some-environment")
            .create();
        match on_subject(err) {
            DomainError::Internal(msg) => assert!(msg.contains("qa-environments"), "{msg}"),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // `default_branch` — Task 35
    // -----------------------------------------------------------------------

    /// The happy path: the environment's `default_branch` override crosses
    /// unchanged.
    #[tokio::test]
    async fn a_platforms_branch_override_is_returned() {
        let id = Uuid::new_v4();
        let client = CountingEnvironments::with(vec![environment_with_branch(id, "release-1.2")]);
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        let branch = reader.default_branch(&ctx(), id).await.expect("resolved");

        assert_eq!(branch.as_deref(), Some("release-1.2"));
    }

    /// No override is `None`, not an error — the common case, since most
    /// environments inherit the repository's own default.
    #[tokio::test]
    async fn a_platform_with_no_override_resolves_to_none() {
        let id = Uuid::new_v4();
        let client = CountingEnvironments::with(vec![environment(id, "no-override")]);
        let reader = QaEnvironmentsReader::new(Arc::clone(&client) as Arc<_>);

        assert_eq!(
            reader.default_branch(&ctx(), id).await.expect("resolved"),
            None
        );
    }

    /// **An environment that resolves to nothing is `Ok(None)`, exactly like an id
    /// absent from [`EnvironmentReader::names`]' map** — not an error, and not
    /// laundered from a swallowed `NotFound` the way [`Self::names`] refuses
    /// to for its own batch read. This method's whole reason to use
    /// `get_environment` rather than `list_environments` is the single-id shape, so
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
