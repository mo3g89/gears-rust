# Feature: Product Plugins

**ID**: `cpt-cf-qa-feature-product-plugins`
**Requirement**: `cpt-cf-qa-fr-product-plugins`
**Decisions**: [ADR-0006](../ADR/0006-cpt-cf-qa-adr-product-plugins.md),
[ADR-0007](../ADR/0007-cpt-cf-qa-adr-connectors-as-libraries.md),
[ADR-0008](../ADR/0008-cpt-cf-qa-adr-credential-containment.md)

## Purpose

Everything the platform knows about a specific product lives behind one trait. The gears model
products as data and behaviour as a plugin, so adding a product is adding a crate.

## The contract

`qa-product-sdk` declares `QaProductPluginV1`:

```rust
#[async_trait]
pub trait QaProductPluginV1: Send + Sync {
    // Declaration — drives the UI and the platform's semantic bindings
    fn credential_schema(&self) -> Vec<FieldDesc>;
    fn observed_schema(&self) -> Vec<FieldDesc>;

    // Environment lifecycle (qa-environments)
    async fn validate_credentials(&self, input: &CredentialInput)
        -> Result<Vec<CredentialClassification>, PluginFailure>;
    async fn observe(&self, env: &EnvironmentHandle<'_>) -> PluginObservation;

    // Run lifecycle (qa-runs)
    async fn prepare_access(&self, env: &EnvironmentHandle<'_>)
        -> Result<AccessSpec, PluginFailure>;
}
```

### Declaration

`credential_schema` is the form an operator fills in. `observed_schema` is what an observation can
yield; `descriptor::validate_schemas` refuses a secret field kind there at registration time,
because observed fields are rendered on the environment page.

### `validate_credentials`

Returns a `CredentialClassification` per submitted key — which fields are secret — and **not**
anything credstore-shaped. Only the gear holds the tenant-scoped `SecurityContext` that may write
credstore, and the plugin runs before that write, so it cannot know a reference to return. Its job
is exactly: reject a malformed form, and say which fields must be stored as secrets.

### `observe`

Returns detected attributes **and** health in one call, so one client and one handshake serve both.
The gear persists the result into `observed_version`, `observed_build`, `observed_base_url`,
`observed_attrs`, `health_state`, `health_detail` and `health_checked_at`.

### `prepare_access`

Returns the mounts, environment bindings and service account a run needs to reach the environment.

**It must work from credstore references alone.** It reads `EnvironmentHandle::credstore_ref`,
never `EnvironmentHandle::resolved`: dispatch calls it without resolving anything, precisely so no
plaintext credential is materialised in the dispatching process. A plugin that needs secret bytes
to build a mount has the wrong mount — `MountSpec::Secret` names the reference and lets the
executor do the resolving.

Run variables whose values are *detected* rather than configured — a base URL, a namespace — come
from `EnvironmentHandle::observed_role`. That is `None` on an environment nothing has observed yet,
which dispatch can legitimately produce: **omit the variable, do not fail the call.**

## Registration and resolution

A plugin is a gear. On boot it builds a registration through
`PluginV1::<QaProductPluginSpecV1>::build_registration`, which yields a GTS instance id, and
registers itself in `ClientHub` under `ClientScope::gts_id(&instance_id)`.

`qa_products.plugin_instance_id` — `NOT NULL` — holds that same id. A gear resolves a product's
behaviour by reading the column and asking `ClientHub`. No gear branches on a product key.

```text
qa_products.plugin_instance_id ──┐
                                 ▼
                         ClientHub::resolve_scoped
                         (ClientScope::gts_id)
                                 ▼
                        dyn QaProductPluginV1
```

## Credential containment

Nothing derived from credential material is ever formatted. `PluginFailure::detail` is
`&'static str` so it cannot be built from runtime bytes; the one sanctioned exception is text a
remote sent back.

`assert_no_leak`, in the SDK's test support, drives a plugin with planted credential material and
fails the build if any of it reaches a published surface. A new plugin inherits the check by
existing.

## Adding a product

1. Create a crate under `plugins/`.
2. Implement `QaProductPluginV1`.
3. Link a connector for the transport, or none if the product speaks HTTP the plugin can use
   directly.
4. Register the gear in the server's gear list.
5. Insert a `qa_products` row whose `plugin_instance_id` is the plugin's GTS id.

No gear is edited. No UI code is written: the credential form comes from `credential_schema` and the
environment page from `observed_schema`.

## Verification

* `apps/cf-gears-example-server/tests/qa_product_plugin_boot.rs` boots the server with both shipped
  plugins and asserts each resolves under its own GTS id.
* `assert_no_leak` runs against every plugin.
* `validate_schemas` rejects a secret kind in `observed_schema`.
