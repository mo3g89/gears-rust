#![cfg(feature = "qa-platform")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Boot wiring for the VHP product plugin.
//!
//! Three things have to line up before a product plugin is reachable, and
//! each of them fails silently on its own:
//!
//! 1. the `qa-platform` feature pulls the crate in (`Cargo.toml`);
//! 2. `registered_gears.rs` names it, which is what runs its `inventory`
//!    registration — a linked-but-unnamed crate registers no gear at all;
//! 3. its `init` publishes the GTS instance and registers the plugin object
//!    in the `ClientHub` under that same id, which is the key Task 12's
//!    `QaProductRegistry` will resolve a product's plugin through.
//!
//! This file links **the server's own** `registered_gears.rs` rather than a
//! copy of its import list, so (2) is tested as it ships: delete the
//! `use qa_vhp_product_plugin as _;` line and
//! [`exactly_one_vhp_product_plugin_gear_is_registered`] sees zero gears.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use qa_product_sdk::QaProductPluginV1;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use toolkit::ConfigProvider;
use toolkit::bootstrap::AppConfig;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::context::GearCtx;
use toolkit::registry::{GearEntry, GearRegistry};
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery, TypesRegistryClient,
};
use uuid::Uuid;

// The binary's own inventory import list, compiled into this test target so
// the gear set discovered below is the gear set the server ships.
#[path = "../src/registered_gears.rs"]
mod registered_gears;

/// The gear name in `#[toolkit::gear(name = ...)]`, and the key of the
/// `gears:` stanza in `config/qa-platform.yaml`.
const GEAR_NAME: &str = "qa-vhp-product-plugin";

/// The full GTS instance id: `QaProductPluginSpecV1`'s type id with the
/// plugin's own segment appended. Written out rather than derived so that a
/// change to either half has to be made here too — this string is the wire
/// identity every `qa_products.plugin_instance_id` will hold.
const INSTANCE_ID: &str =
    "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1";

/// The dev stack, relative to this package's root (the test's working
/// directory). Read as the gear's real `ConfigProvider`, so the stanza this
/// task added is the one `init` below is driven with.
const CONFIG_PATH: &str = "../../config/qa-platform.yaml";

const EXPECTED_VENDOR: &str = "virtuozzo-vhp";
const EXPECTED_PRIORITY: i64 = 100;

// ---------------------------------------------------------------------------
// A types-registry that records what was published to it.
//
// `MockTypesRegistryClient` from `types-registry-sdk`'s `test-util` cannot
// serve here: its `register` asserts the batch is empty (it is a read-side
// fixture, pre-populated rather than written to), and the batch is exactly
// what this test needs to see.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RecordingRegistry {
    registered: Mutex<Vec<Value>>,
}

impl RecordingRegistry {
    fn registered(&self) -> Vec<Value> {
        self.registered.lock().unwrap().clone()
    }
}

/// The GTS id carried by a registration payload, in the field-name order the
/// registry itself accepts (`$id`, `gtsId`, `id`).
fn payload_gts_id(payload: &Value) -> &str {
    ["$id", "gtsId", "id"]
        .iter()
        .find_map(|k| payload.get(*k).and_then(Value::as_str))
        .unwrap_or_else(|| panic!("registration payload carries no GTS id: {payload}"))
}

#[async_trait]
impl TypesRegistryClient for RecordingRegistry {
    async fn register(&self, entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        let results = entities
            .iter()
            .map(|e| RegisterResult::Ok {
                gts_id: payload_gts_id(e).to_owned(),
            })
            .collect();
        self.registered.lock().unwrap().extend(entities);
        Ok(results)
    }

    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn get_type_schemas_by_uuid(
        &self,
        _type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn register_instances(
        &self,
        _instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        unimplemented!("the plugin gear only calls `register`")
    }

    async fn list_instances(
        &self,
        _query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        unimplemented!("the plugin gear only calls `register`")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The whole server's gear inventory, as the runtime discovers it at boot.
fn server_gear_registry() -> GearRegistry {
    GearRegistry::discover_and_build().expect("the server's gear set must build")
}

fn dev_stack_config() -> AppConfig {
    AppConfig::load_or_default(Some(&PathBuf::from(CONFIG_PATH)))
        .expect("config/qa-platform.yaml must load")
}

/// Exactly one — not "at least one". A second `inventory` submission for the
/// same gear (a stray import, a crate linked twice under two names) would
/// leave two gears racing to publish the same GTS instance and to claim the
/// same `ClientHub` scope, and the later one would silently win.
#[test]
fn exactly_one_vhp_product_plugin_gear_is_registered() {
    let registry = server_gear_registry();

    let matching: Vec<&str> = registry
        .gears()
        .iter()
        .map(GearEntry::name)
        .filter(|name| *name == GEAR_NAME)
        .collect();

    assert_eq!(
        matching.len(),
        1,
        "expected exactly one `{GEAR_NAME}` gear in the server's inventory, found {}: {:?}",
        matching.len(),
        registry
            .gears()
            .iter()
            .map(GearEntry::name)
            .collect::<Vec<_>>()
    );
}

/// The `gears:` stanza this task added. Pinned here so deleting it, or
/// drifting either value, fails a test rather than quietly falling back to
/// the code defaults — which are the same values, and would therefore hide
/// the deletion completely.
#[test]
fn the_dev_stack_configures_the_plugin() {
    let config = dev_stack_config();

    let stanza = config
        .get_gear_config(GEAR_NAME)
        .unwrap_or_else(|| panic!("{CONFIG_PATH} has no `gears.{GEAR_NAME}` section"))
        .get("config")
        .unwrap_or_else(|| panic!("`gears.{GEAR_NAME}` has no `config` block"));

    assert_eq!(
        stanza.get("vendor").and_then(Value::as_str),
        Some(EXPECTED_VENDOR)
    );
    assert_eq!(
        stanza.get("priority").and_then(Value::as_i64),
        Some(EXPECTED_PRIORITY)
    );
}

/// `init` publishes one GTS instance and registers one plugin, under one id.
///
/// The two halves have to agree: the id the instance is published under is
/// the id the `ClientHub` scope is keyed on, because that is how a resolver
/// gets from a stored `plugin_instance_id` to a live plugin object.
#[tokio::test]
async fn the_gear_registers_the_plugin_under_its_gts_instance_id() {
    let registry = server_gear_registry();
    let gear = registry
        .get_gear(GEAR_NAME)
        .unwrap_or_else(|| panic!("`{GEAR_NAME}` must be discoverable"));

    let types_registry = Arc::new(RecordingRegistry::default());
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(types_registry.clone());
    let clients_before = hub.len();

    let ctx = GearCtx::new(
        GEAR_NAME,
        Uuid::new_v4(),
        Arc::new(dev_stack_config()),
        hub.clone(),
        CancellationToken::new(),
    );
    gear.init(&ctx).await.expect("plugin init must succeed");

    // One instance published, under the exact wire id.
    let published = types_registry.registered();
    assert_eq!(
        published.len(),
        1,
        "expected one GTS registration, got {published:?}"
    );
    assert_eq!(payload_gts_id(&published[0]), INSTANCE_ID);

    // Carrying the vendor and priority the dev stack set, which is how we
    // know `init` read the stanza rather than ignoring the provider.
    assert_eq!(
        published[0].get("vendor").and_then(Value::as_str),
        Some(EXPECTED_VENDOR)
    );
    assert_eq!(
        published[0].get("priority").and_then(Value::as_i64),
        Some(EXPECTED_PRIORITY)
    );

    // One client registered, and it resolves under that same id.
    assert_eq!(
        hub.len() - clients_before,
        1,
        "init must register exactly one client"
    );
    let resolved = hub
        .try_get_scoped::<dyn QaProductPluginV1>(&ClientScope::gts_id(INSTANCE_ID))
        .expect("the plugin must resolve under its GTS instance id");

    // …and it is the VHP plugin, not merely *something*. `assert_no_leak`'s
    // subject declares exactly these two credential fields.
    let keys: Vec<String> = resolved
        .credential_schema()
        .into_iter()
        .map(|f| f.key)
        .collect();
    assert_eq!(
        keys,
        vec!["kubeconfig".to_owned(), "vpadm_namespace".to_owned()]
    );

    // The scope is a key, not a wildcard: nothing resolves under a
    // neighbouring id. Without this the `try_get_scoped` above would pass on
    // a hub that answered every lookup.
    assert!(
        hub.try_get_scoped::<dyn QaProductPluginV1>(&ClientScope::gts_id(&format!(
            "{INSTANCE_ID}x"
        )))
        .is_none(),
        "an id that was never registered must not resolve"
    );
}
