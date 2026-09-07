#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for `ProductsService`: CRUD scope selection (which PEP action
//! and resource id each verb requests) and the PUT-style full-replace
//! semantics of `update_product`.
//!
//! `RecordingAuthZ` wraps the same permissive decision logic as
//! [`super::test_support::PermissiveAuthZ`] but additionally records every
//! `(action, resource_id)` pair the service requested.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::models::{EvaluationRequest, EvaluationResponse};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, PolicyEnforcer};
use qa_catalog_sdk::{NewProduct, Product, ProductUpdate};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, pep_properties};
use uuid::Uuid;

use super::ProductPluginPresence;
use super::products::ProductsService;
use super::test_support::{
    SelectiveGrantAuthZ, ctx, enforced_pair, permissive_response, test_db_provider,
};
use super::{actions, resources};
use crate::domain::error::DomainError;
use crate::domain::repos::ProductsRepository;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// `ProductsRepository` double: a set of rows, each tagged with the tenant
/// that owns it, mutable so the update/delete tests can observe what the
/// service persisted.
///
/// Filters `get`/`update`/`delete` on the requested `AccessScope` the way
/// `SecureORM`'s `.secure().scope_with(scope)` filters a real query: a row
/// whose owning tenant the scope does not admit is invisible, not merely
/// "found but denied". Before this, every method here ignored `_scope`
/// entirely, so a test claiming to check tenant isolation could only ever
/// prove that a *different id* 404s -- review finding #41.
#[derive(Default)]
struct MockProductsRepository {
    rows: Mutex<Vec<(Product, Uuid)>>,
}

impl MockProductsRepository {
    /// A single row, owned by `tenant_id`.
    fn with_product_in_tenant(product: Product, tenant_id: Uuid) -> Self {
        Self {
            rows: Mutex::new(vec![(product, tenant_id)]),
        }
    }

    /// Add another row, owned by `tenant_id`, alongside whatever is already
    /// stored.
    fn insert_in_tenant(&self, product: Product, tenant_id: Uuid) {
        self.rows.lock().unwrap().push((product, tenant_id));
    }

    /// The first stored row, as the service left it.
    fn stored(&self) -> Option<Product> {
        self.rows.lock().unwrap().first().map(|(p, _)| p.clone())
    }

    /// Whether `scope` admits `tenant_id` -- the same question
    /// `.secure().scope_with(scope)` answers per-row in SQL, evaluated here
    /// in memory. An unconstrained ("allow all") scope admits everything,
    /// matching a real PDP decision with no row-level filtering.
    fn scope_admits(scope: &AccessScope, tenant_id: Uuid) -> bool {
        scope.is_unconstrained() || scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_id)
    }
}

#[async_trait]
impl ProductsRepository for MockProductsRepository {
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Product>, DomainError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|(p, tenant_id)| p.id == id && Self::scope_admits(scope, *tenant_id))
            .map(|(p, _)| p.clone()))
    }

    async fn list<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<Product>, DomainError> {
        unimplemented!("not exercised by these unit tests")
    }

    async fn create<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        _new: NewProduct,
    ) -> Result<Product, DomainError> {
        unimplemented!("not exercised by these unit tests")
    }

    async fn update<C: DBRunner>(
        &self,
        _runner: &C,
        scope: &AccessScope,
        id: Uuid,
        update: ProductUpdate,
    ) -> Result<Option<Product>, DomainError> {
        let mut guard = self.rows.lock().unwrap();
        let Some((row, _)) = guard
            .iter_mut()
            .find(|(p, tenant_id)| p.id == id && Self::scope_admits(scope, *tenant_id))
        else {
            return Ok(None);
        };
        row.name = update.name;
        row.key = update.key;
        row.description = update.description;
        row.folder = update.folder;
        // Written through when named, and **left alone when absent** -- the
        // one field on `ProductUpdate` that is not full-replace. Mirrors
        // `OrmProductsRepository::update`'s own `match`, because a mock that
        // full-replaced here would keep teaching the semantics that silently
        // unbound a product's plugin on every description edit (see
        // `ProductUpdate::plugin_instance_id`'s doc). A mock that dropped the
        // field entirely would hide a service failing to pass it on, so the
        // `Some` arm still writes through.
        if let Some(instance_id) = update.plugin_instance_id {
            row.plugin_instance_id = instance_id;
        }
        row.updated_at = OffsetDateTime::now_utc();
        Ok(Some(row.clone()))
    }

    async fn delete<C: DBRunner>(
        &self,
        _runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let mut guard = self.rows.lock().unwrap();
        let before = guard.len();
        guard.retain(|(p, tenant_id)| !(p.id == id && Self::scope_admits(scope, *tenant_id)));
        Ok(guard.len() < before)
    }
}

/// Wraps [`super::test_support::permissive_response`] (always grants access)
/// while recording every `(action, resource_id)` pair requested.
#[derive(Default)]
struct RecordingAuthZ {
    requests: Mutex<Vec<(String, Option<Uuid>)>>,
}

impl RecordingAuthZ {
    fn requested(&self, action: &str, resource_id: Option<Uuid>) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(a, id)| a == action && *id == resource_id)
    }
}

#[async_trait]
impl AuthZResolverClient for RecordingAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.requests
            .lock()
            .unwrap()
            .push((request.action.name.clone(), request.resource.id));
        Ok(permissive_response(&request))
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A full GTS product-plugin instance id, in the shape
/// `PluginV1::build_registration` composes.
const PLUGIN_ID: &str =
    "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1";

fn product(id: Uuid) -> Product {
    let now = OffsetDateTime::now_utc();
    Product {
        id,
        name: "vhp".to_owned(),
        key: "VHP".to_owned(),
        description: "Virtuozzo Hybrid Platform".to_owned(),
        folder: None,
        plugin_instance_id: PLUGIN_ID.to_owned(),
        created_at: now,
        updated_at: now,
    }
}

/// A `ProductUpdate` naming `folder` and `plugin_instance_id`, with the
/// remaining fields fixed.
fn update(folder: Option<&str>, plugin_instance_id: Option<&str>) -> ProductUpdate {
    ProductUpdate {
        name: "vhi".to_owned(),
        key: "VHI".to_owned(),
        description: "Virtuozzo Hybrid Infrastructure".to_owned(),
        folder: folder.map(ToOwned::to_owned),
        plugin_instance_id: plugin_instance_id.map(ToOwned::to_owned),
    }
}

/// A [`ProductPluginPresence`] double: every id it was asked about, and a
/// fixed answer.
///
/// Recording the ids matters as much as the answer — the service must ask
/// about the value it is going to **store**, unchanged. A transformation on
/// the way in would resolve to nothing in production and report a registered
/// plugin as absent, which is the mistake `is_registered`'s doc warns about.
struct ScriptedPresence {
    registered: bool,
    asked: std::sync::Mutex<Vec<String>>,
}

impl ScriptedPresence {
    fn answering(registered: bool) -> Arc<Self> {
        Arc::new(Self {
            registered,
            asked: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl ProductPluginPresence for ScriptedPresence {
    fn is_registered(&self, instance_id: &str) -> bool {
        self.asked.lock().unwrap().push(instance_id.to_owned());
        self.registered
    }
}

async fn build_service(
    repo: Arc<MockProductsRepository>,
    authz: Arc<RecordingAuthZ>,
) -> ProductsService<MockProductsRepository> {
    build_service_with_presence(repo, authz, ScriptedPresence::answering(true)).await
}

async fn build_service_with_presence(
    repo: Arc<MockProductsRepository>,
    authz: Arc<dyn AuthZResolverClient>,
    presence: Arc<ScriptedPresence>,
) -> ProductsService<MockProductsRepository> {
    let enforcer = PolicyEnforcer::new(authz);
    let db = test_db_provider().await;
    ProductsService::new(db, repo, enforcer, presence)
}

/// [`build_service`] with a caller-supplied `AuthZ` double in place of
/// [`RecordingAuthZ`] — for the test whose subject is the *decisions* the PDP
/// returns rather than the requests the service made.
async fn build_service_with_authz(
    repo: Arc<MockProductsRepository>,
    authz: Arc<dyn AuthZResolverClient>,
) -> ProductsService<MockProductsRepository> {
    build_service_with_presence(repo, authz, ScriptedPresence::answering(true)).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// PRD `cpt-cf-qa-fr-catalog-products` ("manage"): the update verb, and the
/// PEP action it authorizes. Full replace covers `name`, `key`,
/// `description`, and `folder`.
#[tokio::test]
async fn update_product_replaces_fields_under_an_update_scope() {
    let tenant_id = Uuid::new_v4();
    let product_id = Uuid::new_v4();

    let repo = Arc::new(MockProductsRepository::with_product_in_tenant(
        product(product_id),
        tenant_id,
    ));
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(Arc::clone(&repo), Arc::clone(&authz)).await;

    let updated = svc
        .update_product(
            &ctx(tenant_id),
            product_id,
            update(Some("virt"), Some(PLUGIN_ID)),
        )
        .await
        .unwrap();

    assert_eq!(updated.name, "vhi");
    assert_eq!(updated.key, "VHI");
    assert_eq!(updated.description, "Virtuozzo Hybrid Infrastructure");
    assert_eq!(updated.folder.as_deref(), Some("virt"));
    assert_eq!(updated.plugin_instance_id, PLUGIN_ID);
    let stored = repo.stored().unwrap();
    assert_eq!(
        stored.name, "vhi",
        "the change must reach the repository, not just the return value"
    );
    assert_eq!(stored.key, "VHI");
    assert_eq!(stored.description, "Virtuozzo Hybrid Infrastructure");
    assert_eq!(
        stored.plugin_instance_id, PLUGIN_ID,
        "the plugin binding must reach the repository too -- it is the field the \
         resolver reads, and nothing else in this service would notice it being dropped"
    );
    assert!(
        authz.requested(actions::UPDATE, Some(product_id)),
        "expected an UPDATE request for the product's id; requests were: {:?}",
        authz.requests.lock().unwrap()
    );

    // Full replace for `folder`, and deliberately NOT for the binding.
    let cleared = svc
        .update_product(&ctx(tenant_id), product_id, update(None, None))
        .await
        .unwrap();
    assert_eq!(cleared.folder, None, "PUT semantics: folder is replaced");
    assert_eq!(
        cleared.plugin_instance_id, PLUGIN_ID,
        "an absent binding must LEAVE the stored one: the shipped UI cannot \
         send this field, so full replace turned every description edit into a \
         silent unbind -- and unbinding is not a state this platform wants at \
         all (D6, and Task 20 makes the column NOT NULL)"
    );
}

/// A malformed binding is a named 400 from this service, not a stored id that
/// silently resolves to nothing. The bare instance segment is the specific
/// mistake worth pinning: it is what the plan first specified as the stored
/// value, it looks entirely plausible, and `ClientScope::gts_id` accepts any
/// string.
#[tokio::test]
async fn update_product_rejects_a_bare_instance_segment_as_the_binding() {
    let tenant_id = Uuid::new_v4();
    let product_id = Uuid::new_v4();

    let repo = Arc::new(MockProductsRepository::with_product_in_tenant(
        product(product_id),
        tenant_id,
    ));
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(Arc::clone(&repo), authz).await;

    let err = svc
        .update_product(
            &ctx(tenant_id),
            product_id,
            update(None, Some("cf.core._.vhp_product.v1")),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "plugin_instance_id"),
        "got {err:?}"
    );
    assert_eq!(
        repo.stored().unwrap().plugin_instance_id,
        PLUGIN_ID,
        "a rejected update must not persist the binding"
    );
}

#[tokio::test]
async fn update_product_rejects_an_empty_name_and_404s_on_a_foreign_id() {
    let tenant_id = Uuid::new_v4();
    let product_id = Uuid::new_v4();

    let repo = Arc::new(MockProductsRepository::with_product_in_tenant(
        product(product_id),
        tenant_id,
    ));
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(Arc::clone(&repo), authz).await;

    let err = svc
        .update_product(
            &ctx(tenant_id),
            product_id,
            ProductUpdate {
                name: String::new(),
                ..update(None, None)
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "name"),
        "got {err:?}"
    );
    assert_eq!(
        repo.stored().unwrap().name,
        "vhp",
        "a rejected update must not persist"
    );

    // A row that EXISTS -- just under a different tenant -- so the 404 below
    // is genuinely scope-driven rather than a coincidence of "id absent from
    // the mock entirely".
    let foreign = Uuid::new_v4();
    repo.insert_in_tenant(product(foreign), Uuid::new_v4());
    let err = svc
        .update_product(&ctx(tenant_id), foreign, update(None, None))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == foreign),
        "a product outside the scope must 404, got {err:?}"
    );
}

#[tokio::test]
async fn delete_product_removes_the_row_and_404s_when_it_is_gone() {
    let tenant_id = Uuid::new_v4();
    let product_id = Uuid::new_v4();

    let repo = Arc::new(MockProductsRepository::with_product_in_tenant(
        product(product_id),
        tenant_id,
    ));
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(Arc::clone(&repo), Arc::clone(&authz)).await;

    svc.delete_product(&ctx(tenant_id), product_id)
        .await
        .unwrap();
    assert!(repo.stored().is_none(), "the row must be removed");
    assert!(
        authz.requested(actions::DELETE, Some(product_id)),
        "expected a DELETE request for the product's id; requests were: {:?}",
        authz.requests.lock().unwrap()
    );

    let err = svc
        .delete_product(&ctx(tenant_id), product_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == product_id),
        "a second delete must 404, got {err:?}"
    );
}

#[tokio::test]
async fn create_product_rejects_empty_name() {
    let tenant_id = Uuid::new_v4();
    let repo = Arc::new(MockProductsRepository::default());
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(repo, authz).await;

    let err = svc
        .create_product(
            &ctx(tenant_id),
            NewProduct {
                name: String::new(),
                key: "KEY".to_owned(),
                description: "description".to_owned(),
                folder: None,
                plugin_instance_id: PLUGIN_ID.to_owned(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "name"),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Task 20 Step 3: every product names a plugin, and it has to resolve
// ---------------------------------------------------------------------------

/// A valid, registered instance id, shaped like a real one.
const BOUND: &str =
    "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1";

/// **Finding FW-1's other half.** The `plugin_instance_id: None` a create used
/// to be able to build is now unrepresentable: `NewProduct::plugin_instance_id`
/// is a plain `String`.
///
/// So there is no service-level test to write here — the state is gone from
/// the type. The refusal an actual caller meets lives one layer out, where the
/// wire's `Option` dies, and its test is
/// `api::rest::dto::tests::the_wire_requires_a_plugin_and_says_where_to_find_one`.
/// `an_update_that_names_no_plugin_leaves_the_products_binding_alone` holds
/// the update half of the asymmetry against a real database.
///
/// A create naming a plugin this deployment does not register is refused **at
/// the API, not at first use** (Task 20 Step 3, ruling F-10).
///
/// Accepting it would produce a product whose every environment is silently
/// unobservable and undispatchable until somebody noticed.
#[tokio::test]
async fn creating_a_product_with_an_unregistered_plugin_is_refused() {
    let repo = Arc::new(MockProductsRepository::default());
    let authz = Arc::new(RecordingAuthZ::default());
    let presence = ScriptedPresence::answering(false);
    let svc = build_service_with_presence(Arc::clone(&repo), authz, Arc::clone(&presence)).await;

    let err = svc
        .create_product(
            &ctx(Uuid::new_v4()),
            NewProduct {
                name: "vhp".to_owned(),
                key: "VHP".to_owned(),
                description: "d".to_owned(),
                folder: None,
                plugin_instance_id: BOUND.to_owned(),
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "plugin_instance_id"),
        "got {err:?}"
    );
    assert!(
        format!("{err:?}").contains(BOUND),
        "the id is echoed so an operator who mistyped a 100-character GTS id can \
         see what arrived -- it is configuration, not credential material: {err:?}"
    );
    assert!(
        repo.stored().is_none(),
        "nothing rejected may reach the repository"
    );
}

/// The service asks about the **exact** id it is going to store.
///
/// A transformation on the way in would resolve to nothing in production and
/// report a registered plugin as absent — the mistake `is_registered`'s doc
/// warns about, and the reason the check lives beside `plugin_for` rather than
/// being re-derived.
///
/// Asserted on the *refusing* answer, so the assertion needs no repository
/// write: this mock's `create` is `unimplemented!()`, and what is under test
/// is the argument rather than the outcome. The accepting side is covered
/// end-to-end against the real repository by every DB-backed fixture, all of
/// which now bind to `FIXTURE_PLUGIN_INSTANCE_ID` and would fail if the
/// service asked about anything other than the stored value.
#[tokio::test]
async fn the_presence_check_is_asked_about_the_id_verbatim() {
    let repo = Arc::new(MockProductsRepository::default());
    let authz = Arc::new(RecordingAuthZ::default());
    let presence = ScriptedPresence::answering(false);
    let svc = build_service_with_presence(Arc::clone(&repo), authz, Arc::clone(&presence)).await;

    let outcome = svc
        .create_product(
            &ctx(Uuid::new_v4()),
            NewProduct {
                name: "vhp".to_owned(),
                key: "VHP".to_owned(),
                description: "d".to_owned(),
                folder: None,
                plugin_instance_id: BOUND.to_owned(),
            },
        )
        .await;
    // The outcome is irrelevant -- this test is about the ARGUMENT the service
    // asked with, and the refusing answer is what keeps it off the mock's
    // `unimplemented!()` create.
    assert!(outcome.is_err());

    assert_eq!(
        presence.asked(),
        vec![BOUND.to_owned()],
        "asked once, with the submitted value unchanged"
    );
}

/// A **rebind** to an unregistered plugin is refused for create's reason, and
/// `None` still reaches no check at all — it means "leave the binding alone".
#[tokio::test]
async fn a_rebind_must_resolve_but_an_absent_field_is_not_a_rebind() {
    let tenant_id = Uuid::new_v4();
    let id = Uuid::new_v4();
    let repo = Arc::new(MockProductsRepository::with_product_in_tenant(
        product(id),
        tenant_id,
    ));
    let authz = Arc::new(RecordingAuthZ::default());
    let presence = ScriptedPresence::answering(false);
    let svc = build_service_with_presence(Arc::clone(&repo), authz, Arc::clone(&presence)).await;

    let err = svc
        .update_product(
            &ctx(tenant_id),
            id,
            ProductUpdate {
                name: "vhp".to_owned(),
                key: "VHP".to_owned(),
                description: "d".to_owned(),
                folder: None,
                plugin_instance_id: Some(BOUND.to_owned()),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "plugin_instance_id"),
        "a rebind to an unregistered plugin must be refused: {err:?}"
    );

    // The same update with the field absent must not consult presence at all.
    let before = presence.asked().len();
    svc.update_product(
        &ctx(tenant_id),
        id,
        ProductUpdate {
            name: "vhp".to_owned(),
            key: "VHP".to_owned(),
            description: "renamed".to_owned(),
            folder: None,
            plugin_instance_id: None,
        },
    )
    .await
    .expect("an update that names no plugin is not a rebind and must succeed");
    assert_eq!(
        presence.asked().len(),
        before,
        "`None` means leave the binding alone (ruling D-18), so there is nothing \
         to check -- consulting presence here would refuse every ordinary edit \
         made by the shipped UI, which cannot send the field"
    );
}

/// **A valid id from another tenant is a 404, not a read.**
///
/// `MockProductsRepository` used to ignore `_scope` on every method, so a
/// test claiming to check tenant isolation could only ever prove that a
/// *different id* 404s -- an unrelated property. Here the id is real and the
/// row exists; only the scope excludes it. `ProductsService` has no bare
/// "get" (products are read via `list_products` or reached by their id
/// through `update`/`delete`), so this goes through `update_product`, the
/// same entry point `update_product_rejects_an_empty_name_and_404s_on_a_foreign_id`
/// uses for its own 404 case -- the two together are what pin the id-absent
/// and scope-absent cases apart. Review finding #41.
#[tokio::test]
async fn a_product_from_another_tenant_is_not_readable() {
    let ours = Uuid::new_v4();
    let theirs = Uuid::new_v4();
    let their_product = Uuid::new_v4();
    let repo = Arc::new(MockProductsRepository::with_product_in_tenant(
        product(their_product),
        theirs,
    ));
    let authz = Arc::new(RecordingAuthZ::default());
    let svc = build_service(Arc::clone(&repo), authz).await;

    let err = svc
        .update_product(&ctx(ours), their_product, update(None, None))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == their_product),
        "another tenant's product must be absent, not forbidden or readable; got {err:?}"
    );
}

/// **An action the caller has no grant for is denied.**
///
/// Listed by the review as mandatory and missing, and blocked on finding #1
/// for a precise reason: with no catalog there was no enumeration of grantable
/// pairs, so there was no way to grant exactly *one* of them — and "denied
/// without a grant" could not be told from "denied always".
///
/// # What this proves that `pdp_deny_blocks_create` does not
///
/// `tests_tenant_scoping::pdp_deny_blocks_create` builds its services with
/// `DenyAllAuthZ`, a double that refuses every pair. It proves a PDP refusal
/// reaches the caller as [`DomainError::Forbidden`] — and it would pass
/// unchanged against a gear that denied every request unconditionally, or one
/// whose permission catalog was empty, because nothing in it is ever
/// authorized. Here **one principal, one tenant, one service** holds a grant
/// for `(qa.product, update)` and no grant for `(qa.product, delete)`: the
/// update succeeds and the delete is refused, so the refusal is attributable
/// to the missing grant rather than to the caller, the tenant, or the fixture.
/// The positive half is the load-bearing one — without it the test passes
/// against a fixture that denies everything, which is exactly the shape of
/// denial test the review found insufficient.
///
/// Both pairs are resolved out of [`super::authz_surface::ENFORCED`] through
/// [`enforced_pair`], which panics on a pair this gear does not enforce. So
/// the denied pair is one the same principal *could* have been granted, and
/// the test is anchored to the permission catalog rather than to two
/// hand-typed strings: `gts::permissions_tests::the_catalog_matches_the_enforced_surface`
/// pins that list to the `AuthzPermissionV1` instances qa-catalog declares, in
/// both directions. Review finding #1.
#[tokio::test]
async fn an_action_without_a_grant_is_denied() {
    let tenant_id = Uuid::new_v4();
    let product_id = Uuid::new_v4();
    let repo = Arc::new(MockProductsRepository::with_product_in_tenant(
        product(product_id),
        tenant_id,
    ));

    let granted = enforced_pair(resources::PRODUCT_NAME, actions::UPDATE);
    let ungranted = enforced_pair(resources::PRODUCT_NAME, actions::DELETE);
    let svc = build_service_with_authz(
        Arc::clone(&repo),
        Arc::new(SelectiveGrantAuthZ::granting(granted.0, granted.1)),
    )
    .await;

    // ONE context, reused: `ctx` mints a fresh subject id per call, and both
    // halves have to be the same principal for the denial below to be about
    // the grant.
    let caller = ctx(tenant_id);

    svc.update_product(&caller, product_id, update(None, None))
        .await
        .unwrap_or_else(|e| panic!("the granted pair {granted:?} must be authorized: {e:?}"));

    let err = svc.delete_product(&caller, product_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::Forbidden),
        "{ungranted:?} was never granted to this principal, so it must be Forbidden; \
         `Ok` would mean one grant covered every action and `NotFound` would mean the \
         row was invisible rather than the action unauthorized. Got {err:?}"
    );
    assert!(
        repo.stored().is_some(),
        "a denied delete must not remove the row"
    );
}
