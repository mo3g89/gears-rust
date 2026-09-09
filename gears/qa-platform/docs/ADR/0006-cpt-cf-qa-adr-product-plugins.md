---
status: accepted
date: 2026-09-03
---
# Product behaviour is an in-process plugin resolved per product

**ID**: `cpt-cf-qa-adr-product-plugins`

## Context and Problem Statement

QA Platform tests Virtuozzo products, and the products are not variations on one another. One is a
Kubernetes cluster. One is a management node reached over SSH. Others have their own control-plane
APIs. Each has a different credential form, a different way of being observed, a different notion
of what a test run needs in order to reach it.

If a product's behaviour lives in the gears, onboarding a product means editing
`qa-environments`, `qa-runs`, `qa-catalog` and the UI. Where should product-specific behaviour
live?

## Decision Drivers

* Adding a product should be additive — a new crate, not edits across the core.
* The environment abstraction must not *be* any one product's shape.
* The UI must render a product's credential form without knowing the product.
* The workspace already has a plugin mechanism (`ClientHub` plus a GTS identity) used by other
  subsystems.

## Considered Options

* An in-process Rust plugin trait, resolved per product from `ClientHub`
* Product behaviour as configuration data (declarative descriptors)
* Product-specific branches inside the gears, keyed on `product_key`

## Decision Outcome

Chosen option: **an in-process plugin trait**. `QaProductPluginV1`, declared in `qa-product-sdk`,
is the whole of what the platform knows about any product:

| Method | Answers |
|--------|---------|
| `credential_schema` | what an operator must supply |
| `observed_schema` | what an observation can yield |
| `validate_credentials` | is this form valid, and which fields are secret |
| `observe` | what is this environment, and is it healthy |
| `prepare_access` | what does a run need to reach it |

A plugin is a gear. It registers in `ClientHub` under `ClientScope::gts_id(&instance_id)`, and
`qa_products.plugin_instance_id` holds that id, so any gear resolves a product's behaviour by
reading the column. **No gear branches on a product key.**

Two properties of the trait are decisions in their own right:

* `observe` returns attributes **and** health together, so one client and one handshake serve both.
* `prepare_access` works from credstore references alone. It reads `credstore_ref`, never
  `resolved`, so dispatch never materialises a plaintext credential in its own process. A plugin
  that needs secret bytes to build a mount has the wrong mount: `MountSpec::Secret` names the
  reference and lets the executor resolve it.

Correspondingly, `validate_credentials` returns a classification of which submitted keys are secret
— not anything credstore-shaped. Only the gear holds the tenant-scoped `SecurityContext` that may
write credstore, and the plugin runs before that write, so it cannot know a reference to return.

### Consequences

* Good, because a new product is a new crate and a row in `qa_products`.
* Good, because the UI renders any product's form from `credential_schema` with no product
  knowledge.
* Good, because the environment abstraction stopped being "a Kubernetes cluster": VHI's environment
  is a host, and nothing in the core had to learn that.
* Good, because plugins are in-process, so there is no network hop on the observe or dispatch path.
* Bad, because the trait is a public contract: changing it is a versioned change across every
  plugin.
* Bad, because a plugin ships in the same binary, so a new product means a rebuild and redeploy —
  there is no runtime installation.
* Bad, because a misbehaving plugin shares the gear's process.

### Confirmation

* `apps/cf-gears-example-server/tests/qa_product_plugin_boot.rs` boots the server with both plugins
  and asserts each resolves under its own GTS id.
* `descriptor::validate_schemas` refuses a secret field kind in `observed_schema` at registration.
* No gear source matches a product key literal.

## Pros and Cons of the Options

### In-process plugin trait

* Good, because it is expressive enough for genuinely different products.
* Good, because it follows a mechanism the workspace already uses.
* Bad, because it is a compile-time extension point.

### Declarative descriptors

* Good, because a product could be added without a rebuild.
* Bad, because observation is not declarative: reading an install topology or shelling a CLI is
  code, and a descriptor language rich enough to express it is a programming language.

### Branches in the gears

* Good, because it is the least structure for the first product.
* Bad, because it is exactly the coupling this decision exists to remove: the second product
  touches four components.
