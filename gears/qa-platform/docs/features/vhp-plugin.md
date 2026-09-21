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

`qa-connector-k8s`'s secret writer is what materialises the referenced secret into the cluster the
runner executes in.

## Verification

* Plugin boot and resolution in `qa_product_plugin_boot.rs`.
* `assert_no_leak` with planted kubeconfig material.
* Connector-level tests for the Kubernetes client, its error taxonomy and the secret writer.
