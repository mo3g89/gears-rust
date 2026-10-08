# Feature: VHP Product Plugin

**ID**: `cpt-cf-qa-feature-vhp-plugin`
**Crate**: `plugins/qa-vhp-product-plugin`
**Connector**: `connectors/qa-connector-k8s`

## Purpose

Supplies `QaProductPluginV1` for Virtuozzo Hybrid Platform, whose environment is a Kubernetes
cluster.

## Environment shape

A VHP environment is a cluster, identified by the kubeconfig an operator supplies. The kubeconfig
contains a client private key, so it is credential material in full: it is written to credstore and
the environment row keeps only the reference.

## Observation

`observe` reads the cluster's install topology through `qa-connector-k8s` and reports:

* the product version and build,
* the base URL tests should target,
* cluster health, derived from the state of the installation's own resources.

Version, build and base URL land in the dedicated columns; the rest of the topology lands in
`observed_attrs` under the keys `observed_schema` declares.

## Run access

`prepare_run_access` returns:

* a `MountSpec::Secret` naming the kubeconfig's credstore reference — the plugin never reads the
  bytes,
* environment bindings for the namespace and base URL, taken from the observation,
* `service_account: None` — the runner authenticates to the cluster with the mounted kubeconfig,
  not a pod identity, so `RunAccess` carries the field but this plugin never populates it.

### Run variables and which of them a run parameter cannot override

The plugin's run variables are `E2E_VHP_BASE_URL` (the observed base URL),
`VPADM_BASE_DOMAIN` (that URL's bare host), `E2E_K8S_NAMESPACE` (the namespace the install was
observed in) and `KUBECONFIG` (the path the kubeconfig is mounted at, `/.kube/kubeconfig`). These
spellings are frozen: existing test repositories read them by name.

`env_contract` reserves two of them, `E2E_K8S_NAMESPACE` and `KUBECONFIG`. Both are already in
the platform's reserved floor (PRD §5.4), so the plugin's own list adds no name to the union
today; it states them so that the refusal is the plugin's declared contract rather than a
coincidence of the floor: a run parameter of either name is refused, because each is a fact
about the target, not a knob. `E2E_VHP_BASE_URL` and `VPADM_BASE_DOMAIN` are deliberately **not**
reserved, at parity with the source system (which let a run parameter override the variable the
plugin supplies), so a run parameter of either name overrides what the plugin
supplies. Closing that is a product decision, not something to change from this plugin's side.

`qa-connector-k8s`'s secret writer is what materialises the referenced secret into the cluster the
runner executes in.

## Verification

* Plugin boot and resolution in `qa_product_plugin_boot.rs`.
* `assert_no_leak` with planted kubeconfig material.
* Connector-level tests for the Kubernetes client, its error taxonomy and the secret writer.
