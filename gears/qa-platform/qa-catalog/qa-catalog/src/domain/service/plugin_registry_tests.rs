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
        None,
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
        None,
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
        None,
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

// ---------------------------------------------------------------------------
// The plugin boundary's telemetry, from the side that owns the binding
// ---------------------------------------------------------------------------
//
// These run against the REAL adapter through `infra::metrics::probe`, not a
// mock of the port. The claim worth making is that a dashboard query finds the
// series, and a mock could only prove that `plugin_for` called a method.
//
// They live in this file rather than in one of their own because the doubles
// this resolver needs -- a stub products repository, a hub with known
// registrations, a permissive PDP -- are already here, and a second file would
// have had to copy all three or make them `pub(super)`. qa-runs and qa-insights
// put their call-site metric tests in their existing call-site test files for
// the same reason; qa-environments' are in a file of their own only because its
// metric tests needed a different tier from its service tests.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::domain::metrics::{QA_CATALOG_PLUGIN_RESOLUTION, QA_CATALOG_PLUGIN_RESOLUTION_DURATION};
use crate::domain::ports::metrics::{PluginResolutionMetrics, PluginResolutionOutcome};
use crate::infra::metrics::probe::MetricsProbe;
use crate::test_support::DenyAllAuthZ;

/// [`registry_with`] with a caller-supplied metrics adapter and a
/// caller-supplied PDP, which is the pair the assertions below vary.
async fn metered_registry(
    row: Option<Product>,
    registered: &[(&str, &str)],
    authz: Arc<dyn authz_resolver_sdk::AuthZResolverClient>,
    metrics: Arc<dyn PluginResolutionMetrics>,
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
        PolicyEnforcer::new(authz),
        hub,
        Some(metrics),
    )
}

/// A products repository whose `get` always fails the way a real outage would.
///
/// The stub above cannot produce this: its `get` is infallible by construction,
/// so `PluginResolutionOutcome::Failed` would otherwise have no call-site test
/// at all and would rest on the catalog's classification alone. qa-insights'
/// own review recorded exactly that gap for its two `Failed` values; this
/// closes it here rather than inheriting it.
struct BrokenProductsRepository;

#[async_trait]
impl ProductsRepository for BrokenProductsRepository {
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
    ) -> Result<Option<Product>, DomainError> {
        Err(DomainError::database("connection reset by peer"))
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

/// **One resolution is one observation on both instruments, under the outcome
/// that resolution actually had.**
///
/// The first thing a missing or misplaced emission breaks. The exported names
/// are printed on failure because an empty export is a different defect from a
/// wrong count — it says the pipeline saw nothing at all.
#[tokio::test]
async fn a_resolution_records_one_observation() {
    let probe = MetricsProbe::new();
    let product_id = Uuid::new_v4();
    let registry = metered_registry(
        Some(product(product_id, PLUGIN_A)),
        &[(PLUGIN_A, MARKER_KEY)],
        Arc::new(PermissiveAuthZ),
        probe.adapter(),
    )
    .await;

    registry
        .plugin_for(&ctx(Uuid::new_v4()), product_id)
        .await
        .expect("premise: the resolution must succeed, or this measures the wrong path");

    let series = probe.collect();
    assert_eq!(
        series.counter(QA_CATALOG_PLUGIN_RESOLUTION),
        1,
        "one resolution, one increment; the exported names were {:?}",
        series.names()
    );
    assert_eq!(
        series.histogram_count(QA_CATALOG_PLUGIN_RESOLUTION_DURATION),
        1,
        "and the counter and its histogram move together"
    );
    assert_eq!(
        series.counter_with(
            QA_CATALOG_PLUGIN_RESOLUTION,
            &[("outcome", PluginResolutionOutcome::Resolved.as_str())]
        ),
        1
    );
}

/// **A deployment missing a plugin gear is its own series, not a refusal.**
///
/// The value this family was worth building for. Today the state is visible
/// only as one `warn!` line per attempt, so a deployment whose products all
/// name a plugin it does not carry observes nothing and looks healthy from
/// every other angle. Folding it into `refused` would hide it behind the noise
/// of ordinary policy denials, which are the caller's business and not the
/// operator's.
#[tokio::test]
async fn a_product_naming_an_unregistered_plugin_is_counted_apart_from_a_refusal() {
    let probe = MetricsProbe::new();
    let product_id = Uuid::new_v4();
    // The product names PLUGIN_B; the hub carries only PLUGIN_A.
    let registry = metered_registry(
        Some(product(product_id, PLUGIN_B)),
        &[(PLUGIN_A, MARKER_KEY)],
        Arc::new(PermissiveAuthZ),
        probe.adapter(),
    )
    .await;

    let error = err_of(registry.plugin_for(&ctx(Uuid::new_v4()), product_id).await);
    assert!(
        matches!(error, DomainError::ProductPluginUnavailable { .. }),
        "premise: the resolution failed for the reason under test, not another one: {error:?}"
    );

    let series = probe.collect();
    assert_eq!(
        series.counter_with(
            QA_CATALOG_PLUGIN_RESOLUTION,
            &[("outcome", PluginResolutionOutcome::Unregistered.as_str())]
        ),
        1,
        "the deployment is missing the gear that registers this plugin"
    );
    assert_eq!(
        series.counter_with(
            QA_CATALOG_PLUGIN_RESOLUTION,
            &[("outcome", PluginResolutionOutcome::Refused.as_str())]
        ),
        0,
        "and that is not a refusal: nobody denied anything, and no policy change fixes it"
    );
}

/// **A product the caller cannot see is a refusal, and so is a denied read.**
///
/// Two paths, one value, and the pairing is the test: the product read is
/// tenant-scoped, so "no such product" and "not yours" are deliberately the
/// same answer (`plugin_for`'s own doc), and a PDP denial is the other way to
/// reach the same conclusion. Both are facts about the caller, and neither is
/// something an operator should be paged for.
#[tokio::test]
async fn a_product_that_cannot_be_read_is_a_refusal_not_a_failure() {
    for (what, authz, row) in [
        (
            "a product outside the caller's tenant reads as absent",
            Arc::new(PermissiveAuthZ) as Arc<dyn authz_resolver_sdk::AuthZResolverClient>,
            None,
        ),
        (
            "and a denied product read is the same kind of answer",
            Arc::new(DenyAllAuthZ) as Arc<dyn authz_resolver_sdk::AuthZResolverClient>,
            Some(product(Uuid::from_u128(0x5001), PLUGIN_A)),
        ),
    ] {
        let probe = MetricsProbe::new();
        let registry =
            metered_registry(row, &[(PLUGIN_A, MARKER_KEY)], authz, probe.adapter()).await;

        let error = err_of(
            registry
                .plugin_for(&ctx(Uuid::new_v4()), Uuid::from_u128(0x5001))
                .await,
        );
        assert!(
            matches!(error, DomainError::NotFound { .. } | DomainError::Forbidden),
            "{what}: premise failed, the error was {error:?}"
        );

        let series = probe.collect();
        assert_eq!(
            series.counter_with(
                QA_CATALOG_PLUGIN_RESOLUTION,
                &[("outcome", PluginResolutionOutcome::Refused.as_str())]
            ),
            1,
            "{what}"
        );
        assert_eq!(
            series.counter_with(
                QA_CATALOG_PLUGIN_RESOLUTION,
                &[("outcome", PluginResolutionOutcome::Failed.as_str())]
            ),
            0,
            "{what}: and it is not this gear's own failure, which is the series an alert \
             fires on"
        );
    }
}

/// **A broken database is this gear's own failure, and it is timed like any
/// other resolution.**
///
/// The one outcome that should wake somebody. The duration assertion is here
/// rather than in the happy-path test because this is the arm where an
/// implementation that emitted only on success would still pass everything
/// else: the counter would be absent, and so would the sample the p95 of a
/// failing deployment is read from.
#[tokio::test]
async fn a_broken_database_is_this_gears_own_failure() {
    let probe = MetricsProbe::new();
    let registry = QaProductRegistry::new(
        test_db_provider().await,
        Arc::new(BrokenProductsRepository),
        PolicyEnforcer::new(Arc::new(PermissiveAuthZ)),
        Arc::new(ClientHub::new()),
        Some(probe.adapter()),
    );

    let error = err_of(
        registry
            .plugin_for(&ctx(Uuid::new_v4()), Uuid::new_v4())
            .await,
    );
    assert!(
        matches!(error, DomainError::Database { .. }),
        "premise: the read really did fail: {error:?}"
    );

    let series = probe.collect();
    assert_eq!(
        series.counter_with(
            QA_CATALOG_PLUGIN_RESOLUTION,
            &[("outcome", PluginResolutionOutcome::Failed.as_str())]
        ),
        1
    );
    assert_eq!(
        series.histogram_count_with(
            QA_CATALOG_PLUGIN_RESOLUTION_DURATION,
            &[("outcome", PluginResolutionOutcome::Failed.as_str())]
        ),
        1,
        "a failed resolution is timed too: the seconds a broken PDP or database costs \
         are the seconds an operator is looking for"
    );
}

/// An adapter that panics on every call — the shape a poisoned instrument lock
/// takes — plus a count of how many times it was reached.
struct PanickingMetrics {
    calls: Arc<AtomicUsize>,
}

impl PluginResolutionMetrics for PanickingMetrics {
    fn plugin_resolution(&self, _outcome: PluginResolutionOutcome, _duration: std::time::Duration) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        panic!("the adapter is broken");
    }
}

/// **A broken adapter neither fails a resolution nor is called twice.**
///
/// Both halves of `domain::service::emit`'s contract, in one test, because they
/// fail in different ways: without `catch_unwind` the resolution panics
/// outright — and it panics *into* qa-environments' observation loop, across
/// the `ClientHub` — and without the latch every later emission panics again
/// and the default panic hook writes a line to stderr each time, which is the
/// log flood the "never log per emission" constraint is about arriving through
/// the back door.
///
/// The premise assertion matters: a resolution that failed for its own reasons
/// would satisfy "did not panic" while proving nothing about the guard.
///
/// This test prints a panic backtrace even when it passes. The default hook
/// runs before `catch_unwind` returns, so the message reaches stderr; silencing
/// it would mean installing a custom hook every other test in the process would
/// then see.
#[tokio::test]
async fn a_broken_metrics_adapter_does_not_fail_a_resolution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let product_id = Uuid::new_v4();
    let registry = metered_registry(
        Some(product(product_id, PLUGIN_A)),
        &[(PLUGIN_A, MARKER_KEY)],
        Arc::new(PermissiveAuthZ),
        Arc::new(PanickingMetrics {
            calls: Arc::clone(&calls),
        }),
    )
    .await;

    for attempt in 0..2 {
        let plugin = registry
            .plugin_for(&ctx(Uuid::new_v4()), product_id)
            .await
            .expect("a broken adapter must not fail the resolution it is measuring");
        assert_eq!(
            plugin.credential_schema()[0].key,
            MARKER_KEY,
            "attempt {attempt}: and it must resolve the right plugin, not merely survive"
        );
    }

    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the first panic latches the registry off; a second call is the log flood the \
         latch exists to prevent"
    );
}

/// **A gear with no metrics pipeline configured resolves exactly as an
/// unmetered one.**
///
/// The constraint stated as an equality rather than as an absence: the same
/// fixture is resolved twice, once through the real `build_default_adapter`
/// (with no meter provider installed anywhere in the process, which is the
/// production boot posture when telemetry is off) and once with no adapter at
/// all, and the two answers must agree.
#[tokio::test]
async fn a_resolution_with_no_pipeline_configured_behaves_exactly_as_an_unmetered_one() {
    async fn resolve(metrics: Option<Arc<dyn PluginResolutionMetrics>>) -> String {
        let product_id = Uuid::from_u128(0x6001);
        let hub = Arc::new(ClientHub::new());
        hub.register_scoped::<dyn QaProductPluginV1>(
            ClientScope::gts_id(PLUGIN_A),
            MarkerPlugin::arc(MARKER_KEY),
        );
        let registry = QaProductRegistry::new(
            test_db_provider().await,
            Arc::new(StubProductsRepository {
                row: Some(product(product_id, PLUGIN_A)),
            }),
            PolicyEnforcer::new(Arc::new(PermissiveAuthZ)),
            hub,
            metrics,
        );
        registry
            .plugin_for(&ctx(Uuid::new_v4()), product_id)
            .await
            .expect("premise: the resolution really did produce a plugin")
            .credential_schema()[0]
            .key
            .clone()
    }

    let metered = resolve(Some(crate::infra::metrics::build_default_adapter())).await;
    let unmetered = resolve(None).await;

    assert_eq!(
        metered, MARKER_KEY,
        "premise: the plugin really was resolved"
    );
    assert_eq!(
        metered, unmetered,
        "measuring a path must not change it, and the only way to state that is as an \
         equality between the measured and unmeasured answers"
    );
}

/// A products repository whose `get` takes a known, non-trivial amount of
/// wall-clock time.
///
/// Its only job is to make the *magnitude* of the recorded sample assertable:
/// every other double here answers instantly, so a call site that recorded a
/// constant — or an `Instant` taken in the wrong place — would produce a
/// plausible sample and no count-based assertion could tell.
struct SlowProductsRepository {
    row: Option<Product>,
    delay: std::time::Duration,
}

#[async_trait]
impl ProductsRepository for SlowProductsRepository {
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Product>, DomainError> {
        tokio::time::sleep(self.delay).await;
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

/// **The recorded duration really is the clock around the resolution.**
///
/// Every other assertion here is about counts and labels, and counts cannot
/// see a *value*: a call site that recorded `Duration::ZERO`, or a constant, or
/// an `Instant` taken after the work rather than before it would satisfy all of
/// them and hand a dashboard a fabricated distribution. Measured — a mutation
/// that moved the `Instant::now()` below the awaited call passed the whole
/// suite before this test existed.
///
/// Written as **two bracketing assertions rather than one equality**, because
/// an equality would be a timing test:
///
/// * the product read sleeps 150 ms, so the sample cannot be in the 10 ms
///   bucket or below — that direction is deterministic, since a sleep can only
///   overrun;
/// * and it must not be in the `(5 s, 10 s]` bucket, which an in-memory double
///   cannot honestly reach.
///
/// Between them they pin that the value tracks the call, and no more tightly
/// than that: a narrower window would start failing on a loaded machine, which
/// is how a timing assertion gets deleted.
#[tokio::test]
async fn the_recorded_resolution_duration_tracks_the_call_it_measures() {
    let delay = std::time::Duration::from_millis(150);
    let product_id = Uuid::new_v4();
    let probe = MetricsProbe::new();
    let hub = Arc::new(ClientHub::new());
    hub.register_scoped::<dyn QaProductPluginV1>(
        ClientScope::gts_id(PLUGIN_A),
        MarkerPlugin::arc(MARKER_KEY),
    );
    let registry = QaProductRegistry::new(
        test_db_provider().await,
        Arc::new(SlowProductsRepository {
            row: Some(product(product_id, PLUGIN_A)),
            delay,
        }),
        PolicyEnforcer::new(Arc::new(PermissiveAuthZ)),
        hub,
        Some(probe.adapter()),
    );

    registry
        .plugin_for(&ctx(Uuid::new_v4()), product_id)
        .await
        .expect("premise: the resolution must succeed");

    let series = probe.collect();
    assert_eq!(
        series.histogram_count(QA_CATALOG_PLUGIN_RESOLUTION_DURATION),
        1,
        "premise: exactly one resolution was timed"
    );
    // Every bucket whose upper edge is at or below 100 ms must be empty. Probed
    // edge by edge rather than through one call: `histogram_bucket_of` answers
    // for the single bucket a value falls in, so asking about one edge says
    // nothing about the buckets below it -- which is how the first version of
    // this assertion let a zero-duration mutation through. Measured.
    for edge in [0.001_f64, 0.005, 0.01, 0.025, 0.05, 0.1] {
        assert_eq!(
            series.histogram_bucket_of(QA_CATALOG_PLUGIN_RESOLUTION_DURATION, edge),
            Some(0),
            "a resolution whose product read slept for {delay:?} cannot have been measured \
             at {edge} s or less, so that bucket must be empty -- a zero or a near-zero \
             here means the clock is not around the work"
        );
    }
    assert_eq!(
        series.histogram_bucket_of(QA_CATALOG_PLUGIN_RESOLUTION_DURATION, 6.0),
        Some(0),
        "and it cannot have taken between five and ten seconds either: an in-memory \
         double does not, so a sample there is a fabricated or stale duration rather \
         than a measured one"
    );
}
