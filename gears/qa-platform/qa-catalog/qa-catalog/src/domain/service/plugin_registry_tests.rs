#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit and tenant-scoping tests for [`QaProductRegistry`].
//!
//! Four properties, and each of them is a way the resolver can be wrong
//! without anything else noticing:
//!
//! 1. a product bound to a registered plugin resolves to **that** plugin;
//! 2. a product naming a plugin this binary does not register fails, naming
//!    the id that failed — the deployment is missing a gear;
//! 3. a product naming no plugin fails *differently* — the row is unbound,
//!    which is a data problem, not a missing binary;
//! 4. the product read is tenant-scoped, so a product in another tenant is
//!    not resolvable at all.
//!
//! (4) runs against a real `SQLite` database through the real
//! `OrmProductsRepository`, not a double: tenant isolation is `SecureORM`
//! row-level behaviour, and a mock repository that ignores its `AccessScope`
//! would pass the test while proving nothing. The rest use a stub repository,
//! since what they test is the resolution path rather than the query.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use qa_catalog_sdk::{NewProduct, Product};
use qa_product_sdk::{
    CredentialClassification, CredentialInput, EnvironmentHandle, FieldDesc, FieldKind,
    HealthOutcome, ObservationOutcome, ObservedAttrs, PluginFailure, PluginObservation,
    QaProductPluginV1, RunAccess, RunVarContract, RunnerSpec,
};
use time::OffsetDateTime;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
use types_registry_sdk::{GtsInstance, TypesRegistryClient};
use uuid::Uuid;

use super::plugin_registry::QaProductRegistry;
use super::test_support::{PermissiveAuthZ, ctx, test_db_provider};
use crate::domain::error::DomainError;
use crate::domain::repos::ProductsRepository;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A full GTS instance id in the shape `PluginV1::build_registration`
/// composes — a plugin spec's type id with an instance segment appended.
const PLUGIN_A: &str =
    "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1";

/// A second, structurally identical id that nothing registers.
///
/// "Structurally identical" is load-bearing and was briefly untrue: this
/// constant read `acme._.other_product.v1`, an instance segment with four
/// dot-parts where a GTS segment takes five
/// (`vendor.package._.name.version`). `plugin_for` never noticed, because it
/// only ever hands the string to `ClientScope::gts_id`, which accepts any
/// string. The enumeration tests below *parse* these ids, and a malformed one
/// panics in `make_test_instance`. Fixed here rather than worked around: a
/// fixture that cannot exist as a real GTS id proves nothing about a lookup
/// keyed on real ones.
const PLUGIN_B: &str =
    "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~acme.core._.other_product.v1";

/// A plugin instance of a DIFFERENT GTS type: a credstore plugin, which is a
/// real plugin and a real GTS instance and not a product plugin.
const OTHER_TYPE_INSTANCE: &str =
    "gts.cf.toolkit.plugins.plugin.v1~cf.core.credstore.plugin.v1~cf.core._.pg_credstore.v1";

/// The credential key [`MarkerPlugin`] declares, so a test can tell *which*
/// plugin it got back rather than only that it got one.
const MARKER_KEY: &str = "marker-of-plugin-under-test";

/// An inert plugin, identifiable by its declared credential key.
struct MarkerPlugin {
    marker: String,
}

impl MarkerPlugin {
    fn arc(marker: &str) -> Arc<dyn QaProductPluginV1> {
        Arc::new(Self {
            marker: marker.to_owned(),
        })
    }
}

#[async_trait]
impl QaProductPluginV1 for MarkerPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        vec![FieldDesc {
            key: self.marker.clone(),
            label: self.marker.clone(),
            kind: FieldKind::Text,
            required: false,
            role: None,
            in_table: false,
            in_detail: true,
            help: None,
        }]
    }

    /// One observed field, keyed off the same marker.
    ///
    /// Non-empty so the catalogue endpoint's "carries BOTH schemas" property
    /// is actually assertable: with an empty observed schema, a response that
    /// dropped the observed half entirely would look identical to a correct
    /// one.
    fn observed_schema(&self) -> Vec<FieldDesc> {
        vec![FieldDesc {
            key: format!("{}-observed", self.marker),
            label: self.marker.clone(),
            kind: FieldKind::Text,
            required: false,
            role: None,
            in_table: true,
            in_detail: true,
            help: None,
        }]
    }

    async fn validate_credentials(
        &self,
        _input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        Ok(Vec::new())
    }

    async fn observe(&self, _env: &EnvironmentHandle<'_>) -> PluginObservation {
        PluginObservation {
            environment: ObservationOutcome::Detected(ObservedAttrs::default()),
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn prepare_run_access(
        &self,
        _env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        Ok(RunAccess {
            mounts: Vec::new(),
            env: Vec::new(),
            service_account: None,
        })
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        RunnerSpec::default()
    }

    fn env_contract(&self) -> RunVarContract {
        RunVarContract::default()
    }
}

/// The declared credential keys of a resolved plugin — how these tests
/// establish identity.
fn keys(plugin: &Arc<dyn QaProductPluginV1>) -> Vec<String> {
    plugin
        .credential_schema()
        .into_iter()
        .map(|f| f.key)
        .collect()
}

/// `Result::unwrap_err` is unavailable here: it needs `T: Debug`, and
/// `dyn QaProductPluginV1` deliberately is not (a blanket `Debug` on a
/// plugin object is one more surface a careless implementation could render a
/// cached credential on — see `qa_product_sdk::plugin`). This unwraps the
/// error half without asking for it.
fn err_of(result: Result<Arc<dyn QaProductPluginV1>, DomainError>) -> DomainError {
    match result {
        Ok(_) => panic!("expected the resolution to fail, but a plugin came back"),
        Err(e) => e,
    }
}

/// `plugin_instance_id` is a `&str`, not an `Option<&str>` (review finding
/// m-3). It was an `Option` whose `None` used to mean "this product names no
/// plugin"; once the model's field became a `String` that `None` silently
/// meant `PLUGIN_A` instead — the opposite of what it reads like — so a future
/// test written as `product(id, None)` to mean "unbound" would have got a
/// bound product and passed.
fn product(id: Uuid, plugin_instance_id: &str) -> Product {
    let now = OffsetDateTime::now_utc();
    Product {
        id,
        name: "vhp".to_owned(),
        key: "VHP".to_owned(),
        description: "Virtuozzo Hybrid Platform".to_owned(),
        folder: None,
        // A `String` since Task 20a -- the column is NOT NULL and the model
        // followed, so "a product naming no plugin" is no longer a state a
        // fixture can build. The tests that covered it are gone with it; what
        // remains reachable is "names one this deployment does not carry".
        plugin_instance_id: plugin_instance_id.to_owned(),
        created_at: now,
        updated_at: now,
    }
}

/// `ProductsRepository` stub holding at most one product.
///
/// Only `get` is implemented: it is the only method the registry calls, and
/// an `unimplemented!()` on the rest is what makes that claim checkable
/// rather than asserted.
struct StubProductsRepository {
    row: Option<Product>,
}

#[async_trait]
impl ProductsRepository for StubProductsRepository {
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Product>, DomainError> {
        Ok(self.row.as_ref().filter(|p| p.id == id).cloned())
    }

    async fn list<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<Product>, DomainError> {
        unimplemented!("the registry never lists products")
    }

    async fn create<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        _new: NewProduct,
    ) -> Result<Product, DomainError> {
        unimplemented!("the registry never writes")
    }

    async fn update<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
        _update: qa_catalog_sdk::ProductUpdate,
    ) -> Result<Option<Product>, DomainError> {
        unimplemented!("the registry never writes")
    }

    async fn delete<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
    ) -> Result<bool, DomainError> {
        unimplemented!("the registry never writes")
    }
}

/// A registry over one stored product and a hub holding whatever
/// `registered` names.
async fn registry_with(
    row: Option<Product>,
    registered: &[(&str, &str)],
) -> QaProductRegistry<StubProductsRepository> {
    let hub = Arc::new(ClientHub::new());
    for (instance_id, marker) in registered {
        hub.register_scoped::<dyn QaProductPluginV1>(
            ClientScope::gts_id(instance_id),
            MarkerPlugin::arc(marker),
        );
    }

    QaProductRegistry::new(
        test_db_provider().await,
        Arc::new(StubProductsRepository { row }),
        PolicyEnforcer::new(Arc::new(PermissiveAuthZ)),
        hub,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The happy path, and the only one that proves the *composition* is right:
/// the stored id is handed to `ClientScope::gts_id` unchanged, so the plugin
/// registered under that scope is the one that comes back.
#[tokio::test]
async fn plugin_for_resolves_the_plugin_the_product_names() {
    let product_id = Uuid::new_v4();
    let registry = registry_with(
        Some(product(product_id, PLUGIN_A)),
        &[(PLUGIN_A, MARKER_KEY), (PLUGIN_B, "the-other-plugin")],
    )
    .await;

    let plugin = registry
        .plugin_for(&ctx(Uuid::new_v4()), product_id)
        .await
        .expect("a product bound to a registered plugin must resolve");

    assert_eq!(
        keys(&plugin),
        vec![MARKER_KEY.to_owned()],
        "the plugin registered under the product's own id must come back -- not merely \
         some plugin, which a hub holding one entry could not distinguish"
    );
}

/// A product naming a plugin no gear in this binary registered. The id it
/// named has to survive into the error: that string is the whole diagnosis
/// ("which gear is missing?").
#[tokio::test]
async fn plugin_for_reports_the_id_of_an_unregistered_plugin() {
    let product_id = Uuid::new_v4();
    // The hub holds a *different* plugin, so this fails on the lookup rather
    // than on an empty hub.
    let registry = registry_with(
        Some(product(product_id, PLUGIN_B)),
        &[(PLUGIN_A, MARKER_KEY)],
    )
    .await;

    let err = err_of(registry.plugin_for(&ctx(Uuid::new_v4()), product_id).await);

    match err {
        DomainError::ProductPluginUnavailable {
            product_id: reported,
            instance_id,
        } => {
            assert_eq!(reported, product_id);
            assert_eq!(
                instance_id, PLUGIN_B,
                "the error must name the id that failed to resolve, which is what tells an \
                 operator which gear the deployment is missing"
            );
        }
        other => panic!("expected ProductPluginUnavailable, got {other:?}"),
    }
}

// `plugin_for_reports_a_product_that_names_no_plugin` was deleted at Task 20.
//
// It covered a product with a NULL `plugin_instance_id`, distinguished from
// the case above by `instance_id: None`. That column is `NOT NULL` since
// `m20260903_000004_plugin_instance_id_not_null` and the model followed, so
// the state is **unrepresentable** — `product()` cannot build it, and the
// `else` branch in `plugin_for` that answered it is gone.
//
// What replaced the coverage, so the property is not simply lost:
//
// * a create cannot omit the binding — `api::rest::dto`'s
//   `the_wire_requires_a_plugin_and_says_where_to_find_one`, and the type
//   itself;
// * a binding that resolves to nothing is refused at the API rather than at
//   first use — `products_tests`'
//   `creating_a_product_with_an_unregistered_plugin_is_refused`;
// * the *database* refuses a NULL —
//   `m20260903_000004`'s `after_the_migration_a_product_cannot_omit_its_plugin`.
//
// `DomainError::ProductPluginUnavailable::instance_id` is a `String`, not an
// `Option`. An earlier version of this comment kept the `Option` on the
// grounds that "the variant is also produced by `qa-environments`' port". It
// is not, and cannot be: that port returns
// `qa_environments::domain::ports::PluginUnavailable`, a different type in a
// different crate that maps into that gear's own error. This variant exists
// only in qa-catalog and has exactly one constructor, `plugin_for`, which
// always has an id. The `None` shape and the second 404 message it selected
// were dead code kept alive by a test that built the state by hand — deleted
// at the pre-Task-19 review (finding IMPORTANT-5).

/// A product id that no row matches at all is a plain `NotFound`, the same
/// answer `update_product` and `delete_product` give.
#[tokio::test]
async fn plugin_for_404s_on_a_product_that_does_not_exist() {
    let registry = registry_with(None, &[(PLUGIN_A, MARKER_KEY)]).await;
    let missing = Uuid::new_v4();

    let err = err_of(registry.plugin_for(&ctx(Uuid::new_v4()), missing).await);

    assert!(
        matches!(err, DomainError::NotFound { id } if id == missing),
        "got {err:?}"
    );
}

/// Tenant scoping, against a real database and the real repository.
///
/// The registry's read goes through the same `SecureORM` scope every other
/// product read does, so tenant B cannot resolve tenant A's product even
/// though it knows its id — resource ids are identifiers, not secrets, so
/// "knows the id" is the realistic case.
#[tokio::test]
async fn a_product_in_another_tenant_is_not_resolvable() {
    use crate::test_support::{build_plugin_registry_tenant_scoped, build_services_tenant_scoped};

    let db = crate::test_support::inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    let services = build_services_tenant_scoped(db.clone());
    let product = services
        .products
        .create_product(
            &crate::test_support::ctx(tenant_a),
            NewProduct {
                name: "vhp".to_owned(),
                key: "VHP".to_owned(),
                description: "Virtuozzo Hybrid Platform".to_owned(),
                folder: None,
                plugin_instance_id: PLUGIN_A.to_owned(),
            },
        )
        .await
        .unwrap();

    let hub = Arc::new(ClientHub::new());
    hub.register_scoped::<dyn QaProductPluginV1>(
        ClientScope::gts_id(PLUGIN_A),
        MarkerPlugin::arc(MARKER_KEY),
    );
    let registry = build_plugin_registry_tenant_scoped(db, hub);

    let resolved = registry
        .plugin_for(&crate::test_support::ctx(tenant_a), product.id)
        .await
        .expect("the owning tenant must resolve its own product's plugin");
    assert_eq!(keys(&resolved), vec![MARKER_KEY.to_owned()]);

    let err = err_of(
        registry
            .plugin_for(&crate::test_support::ctx(tenant_b), product.id)
            .await,
    );
    assert!(
        matches!(err, DomainError::NotFound { id } if id == product.id),
        "a product outside the caller's tenant must read as absent, not as a plugin \
         failure -- and certainly not resolve; got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Enumeration — `list_registered_plugins`
// ---------------------------------------------------------------------------
//
// The `ClientHub` cannot be enumerated, so the catalogue is the types-registry
// joined to the hub. Each test below is a way that join can be wrong:
//
// 1. the happy path carries BOTH schemas, and they are the registered
//    plugin's own;
// 2. an empty registry is an empty list, not an error and not a 404;
// 3. an instance of some *other* GTS type is not a product plugin — the mock
//    ignores the query by design, so this exercises the local type filter
//    that is the real safeguard;
// 4. a GTS instance with no object in the hub is omitted, not reported with
//    empty schemas that would render an empty form;
// 5. a missing types-registry is an error, not an empty list, because
//    "unreachable directory" and "no plugins" are different facts;
// 6. `vendor` comes off the GTS instance, since the trait has no such method.

/// A `GtsInstance` for `instance_id`, declaring `vendor`.
///
/// `make_test_instance` derives the type-schema chain from the id's own
/// prefix, so an id built as `TYPE_ID + segment` lands on the product-plugin
/// type by construction — which is exactly the shape
/// `PluginV1::build_registration` produces.
fn plugin_instance(instance_id: &str, vendor: &str) -> GtsInstance {
    make_test_instance(
        instance_id,
        serde_json::json!({
            "id": instance_id,
            "vendor": vendor,
            "priority": 100,
            "properties": {},
        }),
    )
}

/// A registry whose hub holds the given plugins and a types-registry mock
/// carrying the given instances.
async fn catalogue_with(
    instances: Vec<GtsInstance>,
    registered: &[(&str, &str)],
) -> QaProductRegistry<StubProductsRepository> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(Arc::new(
        MockTypesRegistryClient::new().with_instances(instances),
    ));
    for (instance_id, marker) in registered {
        hub.register_scoped::<dyn QaProductPluginV1>(
            ClientScope::gts_id(instance_id),
            MarkerPlugin::arc(marker),
        );
    }

    QaProductRegistry::new(
        test_db_provider().await,
        Arc::new(StubProductsRepository { row: None }),
        PolicyEnforcer::new(Arc::new(PermissiveAuthZ)),
        hub,
    )
}

#[tokio::test]
async fn list_registered_plugins_carries_both_schemas_of_each_plugin() {
    let registry = catalogue_with(
        vec![plugin_instance(PLUGIN_A, "virtuozzo-vhp")],
        &[(PLUGIN_A, MARKER_KEY)],
    )
    .await;

    let listed = registry
        .list_registered_plugins(&ctx(Uuid::new_v4()))
        .await
        .expect("a reachable registry must list");

    assert_eq!(listed.len(), 1, "one registered plugin, one entry");
    assert_eq!(
        listed[0].instance_id, PLUGIN_A,
        "the entry must carry the FULL GTS instance id, byte-identical to what \
         qa_products.plugin_instance_id stores -- a caller writes this back verbatim"
    );
    assert_eq!(
        listed[0]
            .credential_schema
            .iter()
            .map(|f| f.key.clone())
            .collect::<Vec<_>>(),
        vec![MARKER_KEY.to_owned()],
        "credential_schema must be THIS plugin's own, not another's"
    );
    assert_eq!(
        listed[0]
            .observed_schema
            .iter()
            .map(|f| f.key.clone())
            .collect::<Vec<_>>(),
        vec![format!("{MARKER_KEY}-observed")],
        "observed_schema must be carried too, and be THIS plugin's: the endpoint's \
         whole purpose is both schemas, and a response with only one renders half a page"
    );
}

#[tokio::test]
async fn list_registered_plugins_is_empty_not_an_error_when_none_are_registered() {
    let registry = catalogue_with(vec![], &[]).await;

    let listed = registry
        .list_registered_plugins(&ctx(Uuid::new_v4()))
        .await
        .expect("an empty registry is a legitimate answer, not a failure");

    assert!(
        listed.is_empty(),
        "a deployment with no product plugins lists none -- and the handler renders \
         that as a 200 with [], never a 404"
    );
}

#[tokio::test]
async fn list_registered_plugins_ignores_instances_of_other_gts_types() {
    // A credstore plugin instance: a real GTS instance, a real plugin, and
    // not a product plugin. `MockTypesRegistryClient` returns every stored
    // instance and ignores the query, so nothing but the local type filter
    // stands between this and the response.
    let other = make_test_instance(
        OTHER_TYPE_INSTANCE,
        serde_json::json!({ "vendor": "builtin", "priority": 10, "properties": {} }),
    );

    // Registered in the hub as well, under its own id. Without this the test
    // passes for the WRONG reason: with the type filter deleted, an instance
    // absent from the hub is skipped by the missing-object branch instead, so
    // the mutation the test exists to catch goes unnoticed. Found by running
    // exactly that mutation.
    let registry = catalogue_with(
        vec![other, plugin_instance(PLUGIN_A, "virtuozzo-vhp")],
        &[
            (PLUGIN_A, MARKER_KEY),
            (OTHER_TYPE_INSTANCE, "a-credstore-plugin-not-a-product-one"),
        ],
    )
    .await;

    let listed = registry
        .list_registered_plugins(&ctx(Uuid::new_v4()))
        .await
        .expect("a reachable registry must list");

    assert_eq!(
        listed
            .iter()
            .map(|p| p.instance_id.clone())
            .collect::<Vec<_>>(),
        vec![PLUGIN_A.to_owned()],
        "only instances of the product-plugin type may appear; a credstore plugin is \
         not a product plugin and must not reach the environment credential form"
    );
}

#[tokio::test]
async fn list_registered_plugins_omits_an_instance_with_no_object_in_the_hub() {
    // Published to the registry, never registered in the hub -- the window
    // inside a plugin gear's `init` between the two calls.
    let registry = catalogue_with(vec![plugin_instance(PLUGIN_B, "acme")], &[]).await;

    let listed = registry
        .list_registered_plugins(&ctx(Uuid::new_v4()))
        .await
        .expect("one unusable instance must not fail the whole catalogue");

    assert!(
        listed.is_empty(),
        "an instance with no plugin object cannot supply either schema, so it is \
         omitted rather than reported with empty ones -- an empty form is worse than \
         an absent plugin"
    );
}

#[tokio::test]
async fn list_registered_plugins_errors_rather_than_claiming_there_are_none() {
    // A hub with the plugin but NO types-registry client: the directory is
    // unreachable, which is not the same fact as "no plugins exist".
    let hub = Arc::new(ClientHub::new());
    hub.register_scoped::<dyn QaProductPluginV1>(
        ClientScope::gts_id(PLUGIN_A),
        MarkerPlugin::arc(MARKER_KEY),
    );
    let registry = QaProductRegistry::new(
        test_db_provider().await,
        Arc::new(StubProductsRepository { row: None }),
        PolicyEnforcer::new(Arc::new(PermissiveAuthZ)),
        hub,
    );

    let err = registry
        .list_registered_plugins(&ctx(Uuid::new_v4()))
        .await
        .expect_err(
            "an unreachable types-registry must NOT be flattened into an empty list: \
             telling an operator their plugins are gone when the directory is down \
             sends them to fix the wrong thing",
        );

    assert!(
        matches!(err, DomainError::Internal(_)),
        "expected Internal for an unreachable directory, got {err:?}"
    );
}

#[tokio::test]
async fn list_registered_plugins_takes_vendor_from_the_gts_instance() {
    let registry = catalogue_with(
        vec![plugin_instance(PLUGIN_A, "virtuozzo-vhp")],
        &[(PLUGIN_A, MARKER_KEY)],
    )
    .await;

    let listed = registry
        .list_registered_plugins(&ctx(Uuid::new_v4()))
        .await
        .expect("a reachable registry must list");

    assert_eq!(
        listed[0].vendor.as_deref(),
        Some("virtuozzo-vhp"),
        "vendor comes off the GTS instance object -- QaProductPluginV1 has no vendor \
         method, so this is the only source there is"
    );
}
