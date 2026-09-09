//! Tests for saved-view CRUD.
//!
//! Against the **real** repository (`OrmSavedViewsRepository`) on in-memory
//! `SQLite` and the real transaction provider, with only the PDP doubled —
//! `results_tests`' and `reconcile_tests`' shape, and for their reason: the
//! property under test is whether the scope this service compiles is the scope
//! the write or the read actually runs under, and a repository double would
//! absorb exactly that. The database-level uniqueness argument itself —
//! `plan_key`, the coalesce, the collision mapping — is
//! `infra::storage::saved_views_sea_repo`'s own test module; this one tests
//! only what the service layer adds: validation order, the owner substitution,
//! and the 404/409 folding.
//!
//! The three tests named in this task's brief are the first three below, each
//! with the brief's own doc comment; the rest extend coverage the brief did not
//! spell out but the Step 0 findings call for.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use toolkit_db::DBProvider;
use uuid::Uuid;

use super::{SavedViewInput, SavedViewsService};
use crate::domain::analytics::PlanRef;
use crate::domain::error::DomainError;
use crate::domain::service::test_support::{DenyAllAuthZ, TenantScopedAuthZ, ctx};
use crate::infra::storage::saved_views_sea_repo::OrmSavedViewsRepository;
use crate::infra::storage::test_db::inmem_db;

const TENANT: Uuid = Uuid::from_u128(0x0A);

/// One plan identity, shared by every test that needs a plan-scoped view.
fn plan() -> PlanRef {
    PlanRef {
        repo_id: Uuid::from_u128(0x50),
        plan_path: "plans/smoke/plan.yaml".to_owned(),
    }
}

struct Fixture {
    service: SavedViewsService<OrmSavedViewsRepository>,
    ctx: toolkit_security::SecurityContext,
}

impl Fixture {
    async fn new() -> Self {
        let db = inmem_db().await;
        let provider = Arc::new(DBProvider::<DomainError>::new(db));
        let service = SavedViewsService::new(
            provider,
            OrmSavedViewsRepository,
            PolicyEnforcer::new(Arc::new(TenantScopedAuthZ)),
        );
        Self {
            service,
            ctx: ctx(TENANT),
        }
    }
}

/// Build a [`SavedViewInput`]. `scope` takes this test module's own
/// convenience spellings, `"global"` and `"plan"`, translated here to the wire
/// spellings `parse_scope` actually accepts (`"all"`/`"plan"`) — a readability
/// choice for this fixture, not a claim about the wire vocabulary.
fn view(name: &str, scope: &str, plan: Option<PlanRef>) -> SavedViewInput {
    let scope = match scope {
        "global" => "all",
        other => other,
    };
    SavedViewInput {
        scope: scope.to_owned(),
        repo_id: plan.as_ref().map(|p| p.repo_id),
        plan_path: plan.map(|p| p.plan_path),
        name: name.to_owned(),
        query_json: r#"{"version":"5.0.1"}"#.to_owned(),
    }
}

/// The unique key is `(owner, scope, COALESCE(plan_id, ''), name)`
/// (`manager/migrations/001_initial.sql:194-195`). A global view and a
/// plan-scoped view may share a name; two global views may not.
#[tokio::test]
async fn a_global_and_a_plan_scoped_view_may_share_a_name() {
    let f = Fixture::new().await;
    f.service
        .create(&f.ctx, view("Regressions", "global", None))
        .await
        .expect("global");
    f.service
        .create(&f.ctx, view("Regressions", "plan", Some(plan())))
        .await
        .expect("plan-scoped");
}

#[tokio::test]
async fn two_global_views_may_not_share_a_name() {
    let f = Fixture::new().await;
    f.service
        .create(&f.ctx, view("Regressions", "global", None))
        .await
        .expect("first");
    let err = f
        .service
        .create(&f.ctx, view("Regressions", "global", None))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::SavedViewNameExists { .. }));
}

/// Views are per-owner. One user's view must not appear in another's list, and
/// must not block another's name.
///
/// Two [`toolkit_security::SecurityContext`]s sharing [`TENANT`] but each
/// minted with its own random `subject_id` — `test_support::ctx`'s own doc
/// records that every call mints a fresh one, which is exactly the "two
/// owners, one tenant" shape this test needs and the brief's placeholder
/// comment asks for.
#[tokio::test]
async fn views_are_scoped_to_their_owner() {
    let f = Fixture::new().await;
    let owner_a = f.ctx;
    let owner_b = ctx(TENANT);

    f.service
        .create(&owner_a, view("My View", "global", None))
        .await
        .expect("owner A's view");
    // The same name, from a different owner: not a collision, because the
    // unique index's first column is owner_id.
    f.service
        .create(&owner_b, view("My View", "global", None))
        .await
        .expect("owner B may use the same name owner A already holds");

    let a_list = f
        .service
        .list(&owner_a, "all", None, None)
        .await
        .expect("owner A's list");
    assert_eq!(a_list.len(), 1, "owner A must see only their own view");
    assert_eq!(a_list[0].owner_id, owner_a.subject_id());

    let b_list = f
        .service
        .list(&owner_b, "all", None, None)
        .await
        .expect("owner B's list");
    assert_eq!(b_list.len(), 1, "owner B must see only their own view");
    assert_eq!(b_list[0].owner_id, owner_b.subject_id());
}

/// `list` orders newest-`updated_at`-first — legacy's `ORDER BY updated_at
/// DESC` (`manager/src/routes/analytics.rs:542`, `:557`), which
/// `SavedViewsRepository::list` does not apply on its own; see
/// `domain::service::saved_views`'s header for why the sort lives here.
///
/// **Insertion order is `[A, B]`; the assertion below is its reverse.** That
/// is deliberate and load-bearing: a fixture that created `A`, created `B`,
/// then updated `A` (an earlier draft of this test) asserts `[A, B]` again —
/// the *same* order a rowid scan or an `idx_qa_analytics_saved_views_unique`
/// walk would also produce with `SavedViewsService::list`'s sort deleted
/// entirely, so that shape passes whether or not the sort exists. Asserting
/// the reverse of insertion order is the only shape that can fail if the sort
/// is removed. Verified by hand: deleting the `sort_by_key` call in
/// `SavedViewsService::list` turns this red (`assertion failed`, order
/// `[A, B]` instead of `[B, A]`); restoring it turns it green again.
#[tokio::test]
async fn list_orders_newest_updated_at_first() {
    let f = Fixture::new().await;
    let first = f
        .service
        .create(&f.ctx, view("A", "global", None))
        .await
        .unwrap();
    let second = f
        .service
        .create(&f.ctx, view("B", "global", None))
        .await
        .unwrap();
    assert!(
        second.updated_at > first.updated_at,
        "the fixture must produce strictly increasing timestamps for this test to \
         discriminate at all: first={:?} second={:?}",
        first.updated_at,
        second.updated_at
    );

    let listed = f.service.list(&f.ctx, "all", None, None).await.unwrap();
    assert_eq!(
        listed.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![second.id, first.id],
        "the most recently created (and therefore most recently updated) view must \
         lead, which is the reverse of insertion order"
    );
}

/// `scope` must be `all` or `plan`; anything else is a 400 naming `scope`,
/// legacy's own message (`analytics.rs:2104-2113`).
#[tokio::test]
async fn an_unrecognized_scope_is_rejected_on_create_and_list() {
    let f = Fixture::new().await;

    let err = f
        .service
        .create(&f.ctx, view("Regressions", "bogus", None))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, ref message }
            if field == "scope" && message == "scope must be 'all' or 'plan'"),
        "{err:?}"
    );

    let err = f
        .service
        .list(&f.ctx, "bogus", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { ref field, .. } if field == "scope"));
}

/// A blank (or all-whitespace) name is rejected, matching legacy's
/// `payload.name.trim(); if name.is_empty()` on both create and update
/// (`analytics.rs:584-586`, `:649-651`).
#[tokio::test]
async fn a_blank_name_is_rejected_on_create_and_update() {
    let f = Fixture::new().await;

    let err = f
        .service
        .create(&f.ctx, view("   ", "global", None))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, ref message }
        if field == "name" && message == "name is required")
    );

    let existing = f
        .service
        .create(&f.ctx, view("Real Name", "global", None))
        .await
        .unwrap();
    let err = f
        .service
        .update(&f.ctx, existing.id, view("  ", "global", None))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { ref field, .. } if field == "name"));
}

/// `scope=plan` with no plan identity is a 400, on create, update and list —
/// legacy's own rule at all three call sites
/// (`analytics.rs:530-540`, `:588-594`, `:654-660`).
#[tokio::test]
async fn a_plan_scope_with_no_plan_identity_is_rejected() {
    let f = Fixture::new().await;

    let err = f
        .service
        .create(&f.ctx, view("Needs A Plan", "plan", None))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { ref field, .. } if field == "plan_path"));

    let err = f
        .service
        .list(&f.ctx, "plan", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { ref field, .. } if field == "plan_path"));
}

/// A half-present `(repo_id, plan_path)` pair under `scope=plan` — a shape
/// legacy's single `plan_id` field can never produce — is rejected rather
/// than silently coalescing into a bucket that disagrees with the declared
/// scope.
#[tokio::test]
async fn a_half_present_plan_pair_under_scope_plan_is_rejected() {
    let f = Fixture::new().await;

    let half = SavedViewInput {
        scope: "plan".to_owned(),
        repo_id: Some(Uuid::from_u128(0x99)),
        plan_path: None,
        name: "Half".to_owned(),
        query_json: "{}".to_owned(),
    };
    let err = f.service.create(&f.ctx, half).await.unwrap_err();
    assert!(matches!(err, DomainError::Validation { ref field, .. } if field == "plan_path"));
}

/// A plan submitted alongside `scope=all` is dropped, not stored — the one
/// place this service does not port legacy's literal permissiveness. See
/// `domain::service::saved_views`'s header for why storing it would make the
/// row invisible to its own `scope=all` list.
#[tokio::test]
async fn a_plan_supplied_alongside_scope_all_is_dropped_not_stored() {
    let f = Fixture::new().await;
    let stored = f
        .service
        .create(&f.ctx, view("Odd But Legal", "global", Some(plan())))
        .await
        .expect("scope=all with a plan supplied is not an error, only a no-op on the plan");
    assert_eq!(stored.repo_id, None, "the plan half must not be stored");
    assert_eq!(stored.plan_path, None);

    // And it is visible through the scope=all list it claims to belong to,
    // which is the property the drop exists to preserve.
    let listed = f.service.list(&f.ctx, "all", None, None).await.unwrap();
    assert_eq!(
        listed.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![stored.id]
    );
}

/// Updating a view's name onto one the caller already holds at the same scope
/// is the same collision a create would report, not a 500 — the repository's
/// `update` maps the unique violation exactly as its `create` does.
#[tokio::test]
async fn renaming_onto_an_existing_name_is_a_collision_not_a_crash() {
    let f = Fixture::new().await;
    f.service
        .create(&f.ctx, view("Taken", "global", None))
        .await
        .unwrap();
    let other = f
        .service
        .create(&f.ctx, view("Free", "global", None))
        .await
        .unwrap();

    let err = f
        .service
        .update(&f.ctx, other.id, view("Taken", "global", None))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::SavedViewNameExists { ref name } if name == "Taken"));
}

/// An update or a delete against an id that never existed is a 404 — legacy's
/// own `rows_affected() == 0` answer (`analytics.rs:683`, `:722`).
#[tokio::test]
async fn updating_or_deleting_an_unknown_id_is_not_found() {
    let f = Fixture::new().await;
    let missing = Uuid::from_u128(0xDEAD);

    let err = f
        .service
        .update(&f.ctx, missing, view("Whatever", "global", None))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::SavedViewNotFound { id } if id == missing));

    let err = f.service.delete(&f.ctx, missing).await.unwrap_err();
    assert!(matches!(err, DomainError::SavedViewNotFound { id } if id == missing));
}

/// **Absent and another owner's are the same 404** — the cross-owner existence
/// oracle [`DomainError::SavedViewNotFound`]'s own doc says this variant
/// closes. Owner B must not be able to update or delete owner A's view, and
/// the answer must be indistinguishable from the id never having existed.
#[tokio::test]
async fn another_owners_view_answers_not_found_not_forbidden() {
    let f = Fixture::new().await;
    let owner_b = ctx(TENANT);

    let owned_by_a = f
        .service
        .create(&f.ctx, view("Mine", "global", None))
        .await
        .unwrap();

    let err = f
        .service
        .update(&owner_b, owned_by_a.id, view("Stolen", "global", None))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::SavedViewNotFound { id } if id == owned_by_a.id));

    let err = f.service.delete(&owner_b, owned_by_a.id).await.unwrap_err();
    assert!(matches!(err, DomainError::SavedViewNotFound { id } if id == owned_by_a.id));
}

/// A denied caller is refused before any row is read or written — the same
/// order-of-operations guarantee `dashboard_tests` pins for its own aggregate,
/// applied here to the first write path in this gear.
#[tokio::test]
async fn a_denied_caller_is_refused_before_any_write() {
    let db = inmem_db().await;
    let provider = Arc::new(DBProvider::<DomainError>::new(db));
    let service = SavedViewsService::new(
        provider,
        OrmSavedViewsRepository,
        PolicyEnforcer::new(Arc::new(DenyAllAuthZ)),
    );
    let ctx = ctx(TENANT);

    let err = service
        .create(&ctx, view("Nope", "global", None))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Forbidden), "{err:?}");
}
