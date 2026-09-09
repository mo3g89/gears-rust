//! The gear that makes this plugin reachable.
//!
//! Everything else in this crate is a library: schemas, [`observe`](crate::observe)
//! and [`run`](crate::run) are only functions until something registers them.
//! This module is that something — it publishes a
//! `PluginV1<QaProductPluginSpecV1>` instance to the GTS types-registry and
//! registers the plugin object in the `ClientHub` under that instance id,
//! which is the pair Task 12's `QaProductRegistry` resolves a product's
//! plugin through.
//!
//! # Why the `OnceLock` holds a [`RegisteredPlugin`]
//!
//! [`RegisteredPlugin::new`] is the only constructor of that type and it runs
//! `validate_schemas` itself, so a value of this type *is* the evidence that
//! §5.1's three registration-time invariants hold — at most one field per
//! `FieldRole`, no secret kind in `observed_schema`, no key in both schemas.
//! Holding the wrapper rather than the bare plugin moves that from a
//! convention ("`init` remembers to call the checker") to a type-system fact.
//! See `PRODUCT-PLUGINS-DESIGN.md` §5.3, whose `RegisteredPlugin` row records
//! why the free-function form was withdrawn.
//!
//! A schema failure therefore leaves `init` as an error and the process does
//! not come up: a plugin whose `observed_schema` declared a secret would
//! render credential material on the environment page (§9), which is not a
//! thing to degrade gracefully around.
//!
//! # Why there are no `deps` beyond `types_registry`, and no `capabilities`
//!
//! No `deps` on the QA gears. They resolve a plugin lazily through the
//! types-registry on first use, exactly as the credstore gear resolves its
//! backend, so this gear's `init` may (and generally will) run after theirs.
//! Naming them here would impose an ordering the resolution path does not
//! need and cannot use.
//!
//! No `capabilities = [db]`. This plugin owns no tables and writes nothing:
//! every value it handles arrives on the `EnvironmentHandle` of the call
//! being served.

use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use async_trait::async_trait;
use qa_product_sdk::{QaProductPluginSpecV1, QaProductPluginV1, RegisteredPlugin};
use serde::Deserialize;
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::VhiProductPlugin;

/// The instance segment this plugin registers under.
///
/// Appended to `QaProductPluginSpecV1`'s type id by
/// [`PluginV1::build_registration`], so the full GTS instance id is
/// `gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhi_product.v1`.
///
/// The `_` segment is the vendor-neutral slot every `cf.core.*` id in this
/// workspace carries; it is part of the wire identity, so changing this
/// constant renames the plugin as far as every stored
/// `qa_products.plugin_instance_id` is concerned.
pub const INSTANCE_SEGMENT: &str = "cf.core._.vhi_product.v1";

/// Default vendor string, matched by exact string equality when a deployment
/// selects between several product plugins.
pub const DEFAULT_VENDOR: &str = "virtuozzo-vhi";

/// Default selection priority (lower wins).
pub const DEFAULT_PRIORITY: i16 = 100;

/// Plugin configuration — the same `vendor`/`priority` shape every toolkit
/// plugin takes, and nothing else.
///
/// Nothing about *what* this plugin does is configurable: the VHI credential
/// and observed schemas are frozen (see [`crate::schemas`]) and the run
/// variables are the product's, not a deployment's. Both fields exist so that
/// a deployment running more than one product plugin can say which one a
/// product resolves to, and in what order candidates are considered.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VhiProductPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,
    /// Plugin priority (lower = higher priority).
    pub priority: i16,
}

impl Default for VhiProductPluginConfig {
    fn default() -> Self {
        Self {
            vendor: DEFAULT_VENDOR.to_owned(),
            priority: DEFAULT_PRIORITY,
        }
    }
}

/// The VHI product plugin, as a gear.
#[toolkit::gear(name = "qa-vhi-product-plugin", deps = [types_registry])]
pub struct VhiProductPluginGear {
    /// The schema-validated plugin, committed only once every fallible step
    /// of `init` has succeeded.
    plugin: OnceLock<RegisteredPlugin>,
}

impl Default for VhiProductPluginGear {
    fn default() -> Self {
        Self {
            plugin: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for VhiProductPluginGear {
    /// Ordering here is deliberate and matches `postgres-credstore-plugin`'s:
    /// everything that can fail runs *before* anything shared is touched, so
    /// a failed boot leaves no `ClientHub` entry pointing at a plugin the
    /// process then refuses to finish initialising.
    ///
    /// **The GTS half of that is weaker, and only in one case.** `init` fails
    /// *before* `registry.register` for every reason except one — a second
    /// `init` on the same gear. That path publishes the instance and only then
    /// trips the `OnceLock` guard, so the types-registry keeps an instance
    /// this gear did not finish committing to. It is republished rather than
    /// newly created (`build_registration` derives the same id from the same
    /// config), and the reference gear has the identical ordering, so this is
    /// not a regression and the ordering is deliberately left alone: moving
    /// the guard above the register call would diverge from the shape every
    /// other plugin gear in this workspace uses, for a case that means the
    /// host runtime called `init` twice.
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: VhiProductPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded plugin configuration"
        );

        // Validates both declared schemas; a `SchemaError` here is a boot
        // failure by design (see this module's header).
        //
        // `Error::new` rather than `anyhow!("...: {e}")`: `SchemaError`
        // implements `std::error::Error`
        // (`qa-product-sdk/src/descriptor.rs:159`), so flattening it to a
        // string would throw away the typed source for a caller that wants to
        // match on it. The operator sees the same two facts either way —
        // anyhow's `Debug` prints the context line and then the `SchemaError`
        // beneath it as a cause, which is how a `Gear::init` error reaches a
        // terminal.
        let plugin: Arc<dyn QaProductPluginV1> = Arc::new(VhiProductPlugin);
        let registered = RegisteredPlugin::new(plugin)
            .map_err(anyhow::Error::new)
            .context("qa-vhi-product-plugin: invalid schemas")?;

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) = PluginV1::<QaProductPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor.clone(),
            cfg.priority,
        )?;

        // Publish to types-registry.
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // All fallible steps done — commit to shared state.
        let api = registered.clone().into_inner();
        self.plugin
            .set(registered)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The scope MUST equal the instance id `build_registration` derived
        // above: that is the key `QaProductRegistry` looks a product's plugin
        // up under, having read the same id out of `plugin_instance_id`.
        ctx.client_hub()
            .register_scoped::<dyn QaProductPluginV1>(ClientScope::gts_id(&instance_id), api);

        info!(instance_id = %instance_id, "VHI product plugin registered");
        Ok(())
    }
}
