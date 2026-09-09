# Feature: VHI Product Plugin

**ID**: `cpt-cf-qa-feature-vhi-plugin`
**Crate**: `plugins/qa-vhi-product-plugin`
**Connector**: `connectors/qa-connector-ssh`

## Purpose

Supplies `QaProductPluginV1` for Virtuozzo Hybrid Infrastructure, whose environment is a
**management node** reached over SSH rather than a cluster reached over an API.

VHI is why the transport split exists: it is the first product whose target is a host, and the
reason `qa-connector-ssh` is a separate crate from `qa-connector-k8s`
([ADR-0007](../ADR/0007-cpt-cf-qa-adr-connectors-as-libraries.md)).

## Environment shape

An operator supplies the management node's address, an account, an SSH private key, and the
`vinfra` administrator password. The key and the password are both credential material and both
live in credstore; the environment row keeps references.

## Observation

`observe` opens a session through `qa-connector-ssh` and:

* reads the product version and build from `/etc/hci-release`,
* drives the `vinfra` CLI to enumerate the cluster's nodes,
* reports health from what those commands return.

## Credential handling

The SSH transport forces two rules, and the connector owns them:

* **The private key never becomes a file.** It travels credstore → memory → a pipe → a short-lived
  `ssh-agent`. Never a file, never `argv`, never an environment variable.
* **A secret reaches a remote command on stdin.** `sshd`'s `AcceptEnv` discards the environment
  channel, and `argv` is world-readable through `/proc`, so the `vinfra` password is written to the
  command's standard input and nowhere else.

Host-key verification is disabled, which for this plugin means an on-path attacker who completes
the handshake is handed the `vinfra` administrator password. The private key is not disclosed,
because it never leaves the agent. This is recorded in full in
[ADR-0005](../ADR/0005-cpt-cf-qa-adr-git-egress.md).

## Run access

`prepare_access` returns the mounts and environment bindings a run needs to reach the node, built
from credstore references only.

## Verification

* Plugin boot and resolution in `qa_product_plugin_boot.rs`.
* `assert_no_leak` with a planted private key and password.
* Connector tests covering agent lifetime, authentication, session handling and error mapping,
  each asserting no credential material reaches a formatted surface.
