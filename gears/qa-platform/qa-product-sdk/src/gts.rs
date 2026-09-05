//! GTS (Global Type System) declaration for the QA Platform product-plugin
//! spec.
//!
//! Mirrors `credstore-sdk/src/gts.rs`: the type id nests under the toolkit
//! plugin base (`gts.cf.toolkit.plugins.plugin.v1~`), the same way every
//! other toolkit plugin kind (credstore's, tenant-resolver's, ...) does.
//! Registering it is what makes `QaProductPluginSpecV1` discoverable and
//! resolvable by `types-registry` at boot, and authorizable by RBAC —
//! without this registration the plugin kind cannot be named by any role.

use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~"),
    description = "QA Platform product plugin specification",
    properties = "",
)]
pub struct QaProductPluginSpecV1;
