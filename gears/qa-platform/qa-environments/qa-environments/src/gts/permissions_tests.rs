//! Unit tests for qa-environments' GTS permission catalog.
//!
//! Five properties: every instance is registered in inventory, the id set
//! matches exactly, every hand-written expected id matches the id the naming
//! rule derives from its own registered `(resource_type, action)` pair, the
//! catalog's `(resource_type, action)` pairs equal
//! `domain::service::authz_surface::ENFORCED` in **both** directions, and the
//! catalog names the PDP's `qa.platform` rather than the aggregate's
//! `qa.environment`.
//!
//! The fourth is the one this catalog exists for. A pair enforced with no
//! catalog entry is an action no role can grant -- the caller is refused and
//! no grant can fix it. A catalog entry with no enforcement is a grant that
//! authorizes nothing, which is worse than absent, because it reads as
//! coverage. Review finding #1.

use toolkit_gts::{GTS_ID_PREFIX, InventoryInstance, gts_id};

const PERMISSION_TYPE_ID: &str = gts_id!("cf.toolkit.authz.permission.v1~");
const INSTANCE_SUFFIX_PREFIX: &str = "cf.qa.environments.";

/// Every qa-environments permission instance id -- one per enforced
/// `(resource_type, action)` pair.
///
/// Hand-written on purpose: it is the second copy, and the point of a second
/// copy is that changing the catalog without meaning to fails here.
/// `expected_ids_match_the_derivation_rule` below pins each of these to the
/// id the naming rule derives from its own registered pair, so a typo in
/// either this list or `permissions.rs` fails loudly rather than silently
/// agreeing with itself.
const EXPECTED_PERMISSION_IDS: &[&str] = &[
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.lease_acquire.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.lease_get.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.lease_release.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_create.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_delete.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_get.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_list.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_update.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_create.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_delete.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_get.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_list.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_update.v1"),
];

fn qa_environments_permission_instances() -> Vec<&'static InventoryInstance> {
    toolkit_gts::inventory::iter::<InventoryInstance>
        .into_iter()
        .filter(|e| {
            e.instance_id.starts_with(PERMISSION_TYPE_ID)
                && e.instance_id[PERMISSION_TYPE_ID.len()..].starts_with(INSTANCE_SUFFIX_PREFIX)
        })
        .collect()
}

/// Reads the `(resource_type, action)` pair back out of a registered
/// instance's serialized payload.
fn payload_pair(entry: &InventoryInstance) -> (String, String) {
    let payload = (entry.payload_fn)();
    let field = |name: &str| {
        payload
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("instance {} lacks a string `{name}`", entry.instance_id))
            .to_owned()
    };
    (field("resource_type"), field("action"))
}

/// The id the naming rule in `permissions.rs`'s header derives from a
/// `(resource_type, action)` pair: `pep_entity` is the entity token of the
/// resource type's GTS id — `cf.qa.environments.lease.v1~` → `lease`.
///
/// Two strips, not one: `gts_id!` prepends `GTS_ID_PREFIX` (`"gts."`) at
/// compile time, so the constant holds `gts.cf.qa.environments.<entity>.v1~`. It is
/// stripped first and **not** re-added in the `format!`, because
/// `PERMISSION_TYPE_ID` already carries its own single copy of it.
///
/// Was a bare `qa.` prefix strip until the labels became GTS type ids (WS0). The
/// rule's **output is unchanged** — `cf.qa.environments.platform_create.v1`
/// and its siblings are the ids this still derives.
fn derive_instance_id(resource_type: &str, action: &str) -> String {
    let pep_entity = resource_type
        .strip_prefix(GTS_ID_PREFIX)
        .and_then(|rest| rest.strip_prefix(INSTANCE_SUFFIX_PREFIX))
        .and_then(|rest| rest.strip_suffix(".v1~"))
        .unwrap_or_else(|| {
            panic!("resource type {resource_type} is not a cf.qa.environments.<entity>.v1~ id")
        });
    format!("{PERMISSION_TYPE_ID}{INSTANCE_SUFFIX_PREFIX}{pep_entity}_{action}.v1")
}

#[test]
fn all_qa_environments_permissions_are_registered_in_inventory() {
    let entries = qa_environments_permission_instances();
    assert_eq!(
        entries.len(),
        EXPECTED_PERMISSION_IDS.len(),
        "expected {} instances, found {}: {:?}",
        EXPECTED_PERMISSION_IDS.len(),
        entries.len(),
        entries.iter().map(|e| e.instance_id).collect::<Vec<_>>()
    );
    for entry in &entries {
        assert_eq!(
            entry.type_id, PERMISSION_TYPE_ID,
            "instance {} derived wrong type_id",
            entry.instance_id
        );
    }
}

#[test]
fn the_inventory_covers_every_expected_id() {
    let found: std::collections::BTreeSet<_> = qa_environments_permission_instances()
        .iter()
        .map(|e| e.instance_id)
        .collect();
    for expected in EXPECTED_PERMISSION_IDS {
        assert!(
            found.contains(expected),
            "missing catalog instance: {expected}"
        );
    }
    assert_eq!(
        found.len(),
        EXPECTED_PERMISSION_IDS.len(),
        "inventory contains qa-environments permission ids not in the expected set"
    );
}

/// **Every hand-written expected id matches what the naming rule derives
/// from its own registered `(resource_type, action)` pair.**
///
/// `EXPECTED_PERMISSION_IDS` is deliberately hand-written rather than
/// generated (so an accidental catalog change fails here rather than passing
/// silently), which means it can drift from `permissions.rs` in a way that
/// still agrees on the *set* of ids — e.g. a copy-pasted id paired with the
/// wrong action. This test recomputes the id from what is actually
/// registered and catches that.
#[test]
fn expected_ids_match_the_derivation_rule() {
    let instances = qa_environments_permission_instances();
    for &expected in EXPECTED_PERMISSION_IDS {
        let entry = instances
            .iter()
            .find(|e| e.instance_id == expected)
            .unwrap_or_else(|| panic!("expected id {expected} has no registered instance"));
        let (resource_type, action) = payload_pair(entry);
        let derived = derive_instance_id(&resource_type, &action);
        assert_eq!(
            expected, derived,
            "expected id {expected} does not match the id derived from its own \
             (resource_type, action) = ({resource_type}, {action}) pair"
        );
    }
}

/// **The catalog and the enforced set are the same set.**
///
/// Both directions, because the two failures are different and both are
/// silent.
#[test]
fn the_catalog_matches_the_enforced_surface() {
    let cataloged: std::collections::BTreeSet<(String, String)> =
        qa_environments_permission_instances()
            .iter()
            .map(|e| payload_pair(e))
            .collect();
    let enforced: std::collections::BTreeSet<(String, String)> =
        crate::domain::service::authz_surface::ENFORCED
            .iter()
            .map(|&(resource_type, action)| (resource_type.to_owned(), action.to_owned()))
            .collect();

    let ungrantable: Vec<_> = enforced.difference(&cataloged).collect();
    assert!(
        ungrantable.is_empty(),
        "these pairs are enforced but not in the catalog, so no role can grant \
         them: {ungrantable:#?}"
    );

    let unenforced: Vec<_> = cataloged.difference(&enforced).collect();
    assert!(
        unenforced.is_empty(),
        "these pairs are in the catalog but enforced nowhere, so granting them \
         authorizes nothing: {unenforced:#?}"
    );
}

/// **The catalog names the PDP's `qa.platform`, not the aggregate's
/// `qa.environment`.**
///
/// The aggregate this gear manages was renamed `TargetPlatform` ->
/// `Environment` across the Rust identifiers, the REST wire, the routes and
/// the UI. The PDP resource string was deliberately left as `qa.platform`
/// (`domain::service::mod`'s `resources` doc explains why), and this
/// catalog's `resource_type` values come from that same string, not from the
/// aggregate's Rust name. A catalog generated from the aggregate's name would
/// emit `qa.environment`, match no policy, grant nothing, and look entirely
/// correct while doing it. Review finding #1.
#[test]
fn the_catalog_names_qa_platform_not_qa_environment() {
    let types: std::collections::BTreeSet<_> = qa_environments_permission_instances()
        .iter()
        .map(|e| payload_pair(e).0)
        .collect();
    let expected = format!("{GTS_ID_PREFIX}cf.qa.environments.platform.v1~");
    assert!(
        types.contains(&expected),
        "the environments resource type on the wire to the PDP is {expected}; found {types:?}"
    );
    let forbidden = format!("{GTS_ID_PREFIX}cf.qa.environments.environment.v1~");
    assert!(
        !types.contains(&forbidden),
        "{forbidden} matches no policy in any deployment; found {types:?}"
    );
}

/// Every authz label must be a structurally valid, concrete GTS **type** id
/// (type ids end `~`) and must have a stub type-schema in the inventory
/// `types-registry::init()` registers at boot. A label that is neither is a
/// permission no role definition can name (review finding 1).
#[test]
fn every_authz_label_is_a_registrable_gts_type() {
    use crate::domain::service::resources;

    let registered: Vec<String> = toolkit_gts::all_inventory_type_schemas()
        .expect("inventory type schemas are well-formed JSON")
        .iter()
        .filter_map(|schema| schema["$id"].as_str().map(str::to_owned))
        .collect();

    for label in [
        resources::PLATFORM_NAME,
        resources::VARIABLE_NAME,
        resources::LEASE_NAME,
    ] {
        assert!(
            ::gts::GtsId::try_new(label).is_ok(),
            "label {label} is not a structurally valid GTS id"
        );
        assert!(
            label.ends_with('~'),
            "label {label} must be a concrete type id, not a bare string"
        );
        let wanted = format!("gts://{label}");
        assert!(
            registered.iter().any(|id| id == &wanted),
            "label {label} has no registered type schema; registered: {registered:?}"
        );
    }
}
