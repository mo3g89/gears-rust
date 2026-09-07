//! Product → plugin resolution.
//!
//! One question, answered in one hop: *which plugin owns this product's
//! behaviour?* The binding is `qa_products.plugin_instance_id`, a column on
//! this gear's own aggregate, which is why the resolver lives here and not in
//! the two gears that consume it (`PRODUCT-PLUGINS-DESIGN.md` §3, decision
//! that products are `qa-catalog`'s aggregate).
//!
//! # Shape borrowed from `chat-engine`
//!
//! `chat-engine`'s `PluginService::resolve` is the same three lines —
//! `ClientScope::gts_id(id)`, `try_get_scoped`, map the miss to a not-found —
//! and this deliberately does not invent a second idiom for it. What differs
//! is that `chat-engine` is handed the instance id by its caller, while here
//! the caller has a *product* id and the instance id is a tenant-scoped
//! database read away. That read is the reason this method is `async`, and
//! the reason it needs a `SecurityContext`.
//!
//! # No cache
//!
//! The lookup is a `HashMap` hit on `(TypeId, ClientScope)` behind an
//! `RwLock` read, and the product read is a single indexed row. A cache here
//! would add an invalidation problem (an operator rebinding a product would
//! keep getting the old plugin) to buy nothing measurable.
//!
//! # Resolution and enumeration are two questions
//!
//! [`QaProductRegistry::plugin_for`] answers "which plugin owns this
//! product", and it does so **from a stored id alone, with no directory of
//! its own** — it never enumerates, and it must stay that way, because a
//! resolver that needs a directory fails whenever the directory does.
//!
//! [`QaProductRegistry::list_registered_plugins`] answers the different
//! question "what plugins does this deployment have", which the `ClientHub`
//! cannot answer at all: it has no enumeration API (`register`,
//! `try_get_scoped`, `len`, and nothing that iterates). The platform's own
//! answer is the **types-registry**, where every plugin gear publishes a GTS
//! instance at boot before registering its object in the hub. So enumeration
//! reads the registry and then looks each id up in the hub.
//!
//! Both live in this one type on purpose: it is the single place in
//! `qa-catalog` that knows GTS exists (design §4.1, reading 2), and splitting
//! them would make that two places.
//!
//! An earlier revision of this module doc said enumeration deliberately did
//! not exist. That was true when only `plugin_for` had a caller; Task 13's
//! `GET /qa/v1/product-plugins` is the caller that changed it.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use gts::GtsSchema;
use qa_product_sdk::{FieldDesc, QaProductPluginSpecV1, QaProductPluginV1};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use types_registry_sdk::{InstanceQuery, TypesRegistryClient};

use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::repos::ProductsRepository;

/// Whether a GTS instance id names a product plugin **this process
/// registers** (Task 20 Step 3).
///
/// # Why a one-method port rather than the registry itself
///
/// `ProductsService` needs exactly one bit before it stores a binding: does
/// this id resolve to anything? [`QaProductRegistry::plugin_for`] answers a
/// much bigger question — a tenant-scoped product read *plus* the `ClientHub`
/// lookup — and it needs a `product_id`, which a create does not have yet.
/// Handing the service the whole registry would also be circular-ish: the
/// registry is generic over [`ProductsRepository`] and holds the same repo the
/// service holds.
///
/// So the service depends on this, the registry implements it, and
/// `AppServices::new` wires the two together (ruling F-11).
///
/// # It is process-local, and that is a real limitation
///
/// `ClientHub::try_get_scoped` answers for the running binary. During a
/// rolling upgrade two replicas can disagree, so the same request can be
/// accepted by one and refused by the other until the rollout completes. All
/// four QA gears share one process and one hub, so the window is exactly that
/// upgrade — and a retry succeeds on the far side of it. Recorded rather than
/// hidden, because "the API refused something it accepted a minute ago" is a
/// confusing thing to meet without an explanation (ruling F-10).
pub trait ProductPluginPresence: Send + Sync {
    /// `true` when `instance_id` names a product plugin registered in this
    /// process, under the `ClientHub` scope the plugin registered itself with.
    fn is_registered(&self, instance_id: &str) -> bool;
}

/// Resolves a product's product plugin.
///
/// Holds an `Arc<ClientHub>` rather than borrowing one: the hub is the
/// process's registry of live clients and outlives every gear in it, and the
/// resolver is itself registered into that hub (as
/// `dyn QaProductPluginResolverV1`), so the two reference each other. That
/// cycle is between two boot singletons that live until the process exits —
/// it is not a leak that grows, and a `Weak` here would trade it for a
/// resolver that starts failing if anything ever drops the last strong
/// handle.
#[domain_model]
pub struct QaProductRegistry<P: ProductsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<P>,
    policy_enforcer: PolicyEnforcer,
    client_hub: Arc<ClientHub>,
}

/// One product plugin this deployment has, as
/// [`QaProductRegistry::list_registered_plugins`] found it.
///
/// `instance_id` is the **full** GTS instance id, byte-identical to what
/// `qa_products.plugin_instance_id` stores and to the `ClientHub` scope the
/// plugin registered under. That is what lets a caller write a value from
/// this list straight back onto a product without transforming it.
#[derive(Clone, Debug)]
pub struct RegisteredProductPlugin {
    pub(crate) instance_id: String,
    /// From the GTS instance's own `vendor` property, not from the trait —
    /// [`QaProductPluginV1`] has no `vendor` method. `PluginV1` carries it,
    /// and a deployment running several product plugins selects between them
    /// on exactly this string.
    pub(crate) vendor: Option<String>,
    pub(crate) credential_schema: Vec<FieldDesc>,
    pub(crate) observed_schema: Vec<FieldDesc>,
}

impl<P: ProductsRepository> ProductPluginPresence for QaProductRegistry<P> {
    /// The **same** lookup [`Self::plugin_for`] performs, minus the product
    /// read: `ClientScope::gts_id` of the stored value **unchanged**. Any
    /// transformation of the id here would resolve to nothing and report a
    /// registered plugin as absent, so the two must agree — which is why this
    /// lives beside `plugin_for` rather than in the service.
    fn is_registered(&self, instance_id: &str) -> bool {
        self.client_hub
            .try_get_scoped::<dyn QaProductPluginV1>(&ClientScope::gts_id(instance_id))
            .is_some()
    }
}

impl<P: ProductsRepository> QaProductRegistry<P> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<P>,
        policy_enforcer: PolicyEnforcer,
        client_hub: Arc<ClientHub>,
    ) -> Self {
        Self {
            db,
            repo,
            policy_enforcer,
            client_hub,
        }
    }

    /// The plugin bound to `product_id`.
    ///
    /// Two reads: the product's `plugin_instance_id` under a PEP-compiled
    /// `GET` scope (so a product outside the caller's tenant is absent, not
    /// merely unreadable), then an O(1) `ClientHub` lookup under
    /// `ClientScope::gts_id` of that id **unchanged** — the stored value is
    /// the full GTS instance id the plugin registered itself under, and any
    /// transformation of it here would resolve to nothing.
    ///
    /// # Errors
    ///
    /// - [`DomainError::NotFound`] when no product with `product_id` is
    ///   visible to `ctx`.
    /// - [`DomainError::ProductPluginUnavailable`] when the product names a
    ///   plugin this binary does not register. "Names no plugin" is not a
    ///   state that reaches here since Task 20a made the column `NOT NULL`,
    ///   which is why that variant no longer has a second shape (review
    ///   finding IMPORTANT-4: this list said otherwise while the comment 25
    ///   lines below said the opposite, in the same function).
    /// - [`DomainError::Forbidden`] when policy denies the product read.
    #[instrument(skip(self, ctx), fields(product_id = %product_id))]
    pub async fn plugin_for(
        &self,
        ctx: &SecurityContext,
        product_id: Uuid,
    ) -> Result<Arc<dyn QaProductPluginV1>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PRODUCT, actions::GET, Some(product_id))
            .await?;

        let conn = self.db.conn()?;
        let product = self
            .repo
            .get(&conn, &scope, product_id)
            .await?
            .ok_or(DomainError::NotFound { id: product_id })?;

        // No `Option` to unwrap since Task 20a: the column is `NOT NULL`
        // (`m20260903_000004_plugin_instance_id_not_null`) and the model
        // followed, so "this product names no plugin" is not a state that
        // reaches here any more -- and `DomainError::ProductPluginUnavailable`
        // dropped its `Option` to match (review finding IMPORTANT-5).
        let instance_id = product.plugin_instance_id;

        let scope = ClientScope::gts_id(&instance_id);
        let Some(plugin) = self
            .client_hub
            .try_get_scoped::<dyn QaProductPluginV1>(&scope)
        else {
            warn!(
                plugin_instance_id = %instance_id,
                "qa-catalog: no product plugin is registered under the product's \
                 plugin_instance_id; the deployment is missing the gear that registers it"
            );
            return Err(DomainError::ProductPluginUnavailable {
                product_id,
                instance_id,
            });
        };

        debug!(plugin_instance_id = %instance_id, "Resolved product plugin");
        Ok(plugin)
    }

    /// Every product plugin this deployment actually has, with both declared
    /// schemas.
    ///
    /// Drives `GET /qa/v1/product-plugins`, which Tasks 21-22 render the
    /// credential form and the environments table from.
    ///
    /// # Why this reads types-registry and not the `ClientHub`
    ///
    /// The hub cannot be enumerated — it exposes `register`,
    /// `try_get_scoped`, `len` and nothing that iterates, and `len` counts
    /// every interface in the process rather than plugins. The types-registry
    /// can: a plugin gear publishes its GTS instance there at boot and only
    /// then registers its object in the hub
    /// (`qa-vhp-product-plugin`'s `gear::init`), so the registry is the
    /// directory and the hub is the object store. This reads the first and
    /// looks each id up in the second.
    ///
    /// # Why the query is unfiltered and the filter is local
    ///
    /// [`InstanceQuery`] takes a GTS wildcard `pattern`, and passing
    /// `TYPE_ID` + `*` would narrow the read server-side. This passes no
    /// pattern and filters here instead, on the instance's own resolved
    /// type-schema id. The wildcard's exact matching semantics are the
    /// registry's, not this gear's; an unfiltered list plus an equality test
    /// on the type id cannot silently return the wrong set if those semantics
    /// differ from what this gear assumed, and the instance count in a
    /// process is small enough that the difference is not measurable.
    ///
    /// # A GTS instance whose object is missing from the hub is skipped
    ///
    /// It is logged at `warn!` and left out of the list, rather than reported
    /// with empty schemas. The endpoint exists to supply the two schemas a UI
    /// renders a form from; an entry that cannot supply them would render an
    /// empty form, which is worse than the plugin being absent. The window is
    /// real but narrow — inside one `init`, between the registry publish and
    /// the hub registration — and the log line names the id.
    ///
    /// # Errors
    ///
    /// - [`DomainError::Forbidden`] when policy denies the product list.
    /// - [`DomainError::Internal`] when no types-registry client is
    ///   registered in this process, or when it fails the list. Not an empty
    ///   list: "the directory is unreachable" and "the deployment has no
    ///   plugins" are different facts, and answering the first with the
    ///   second would tell an operator their plugins are gone.
    #[instrument(skip(self, ctx))]
    pub async fn list_registered_plugins(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<RegisteredProductPlugin>, DomainError> {
        // PRODUCT/LIST rather than a resource of its own: plugins are not
        // tenant-scoped (a GTS instance is process-global, exactly as
        // `TypesRegistryClient`'s own doc says), so there is no row-level
        // scope to compile here. What this gate buys is fail-closed access —
        // a caller who may not list products has no use for the catalog of
        // plugins those products bind to.
        self.policy_enforcer
            .access_scope(ctx, &resources::PRODUCT, actions::LIST, None)
            .await?;

        let registry = self
            .client_hub
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| {
                DomainError::Internal(format!(
                    "no types-registry client is registered in this process, so the \
                     product-plugin catalogue cannot be enumerated: {e}"
                ))
            })?;

        let instances = registry
            .list_instances(InstanceQuery::new())
            .await
            .map_err(|e| {
                DomainError::Internal(format!("types-registry instance list failed: {e}"))
            })?;

        let want = <QaProductPluginSpecV1 as GtsSchema>::TYPE_ID;
        let mut out = Vec::new();

        for instance in instances {
            if instance.type_schema.type_id.as_ref() != want {
                continue;
            }

            let instance_id = instance.id.to_string();
            let scope = ClientScope::gts_id(&instance_id);
            let Some(plugin) = self
                .client_hub
                .try_get_scoped::<dyn QaProductPluginV1>(&scope)
            else {
                warn!(
                    plugin_instance_id = %instance_id,
                    "qa-catalog: a product-plugin GTS instance is registered but no plugin \
                     object is in the ClientHub under its id; omitting it from the catalogue"
                );
                continue;
            };

            out.push(RegisteredProductPlugin {
                instance_id,
                vendor: instance
                    .object
                    .get("vendor")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                credential_schema: plugin.credential_schema(),
                observed_schema: plugin.observed_schema(),
            });
        }

        // Deterministic order: the endpoint feeds a rendered list, and an
        // order that varies between calls reorders a form for no reason. The
        // registry's own order is not part of its contract.
        out.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));

        debug!(count = out.len(), "Listed registered product plugins");
        Ok(out)
    }
}
