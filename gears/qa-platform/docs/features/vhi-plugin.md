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

The SSH transport forces two rules on the **observation path** (`qa-connector-ssh`), and the
connector owns them there. The **run path** does not follow either rule: `prepare_run_access`
(below) mounts both credentials into the run's pod as files, because the runner is a generic image
driving `ssh`/`vinfra` itself, not `qa-connector-ssh`'s in-process agent.

* **On the observation path, the private key never becomes a file.** It travels credstore → memory
  → a pipe → a short-lived `ssh-agent`. Never a file, never `argv`, never an environment variable.
  On the run path, the key is mounted at `SSH_KEY_PATH`, read-only.
* **On the observation path, a secret reaches a remote command on stdin, and nowhere else.**
  `sshd`'s `AcceptEnv` discards the environment channel, and `argv` is world-readable through
  `/proc`, so the `vinfra` password is written to the command's standard input. On the run path,
  the password is mounted at `VINFRA_PASSWORD_PATH`, also read-only, and the runner reads it from
  that file.

Host-key verification is disabled, which for this plugin means an on-path attacker who completes
the handshake is handed the `vinfra` administrator password. On the observation path, the private
key is not disclosed, because it never leaves the agent; on the run path, it is mounted into the
pod at `SSH_KEY_PATH`, mode `0o400`. This is recorded in full in
[ADR-0005](../ADR/0005-cpt-cf-qa-adr-git-egress.md).

## Run access

`prepare_run_access` returns the mounts and environment bindings a run needs to reach the node, built
from credstore references only.

### Run variables and which of them a run parameter cannot override

| Variable | Carries | Reserved |
|----------|---------|----------|
| `VHI_SSH_HOST` | the node host | yes |
| `VHI_SSH_KEY_FILE` | path of the mounted private key (`/etc/qa/vhi-ssh/id`) | yes |
| `VINFRA_PORTAL` | the portal address | yes |
| `VINFRA_PASSWORD_FILE` | path of the mounted `vinfra` password (`/etc/qa/vhi-vinfra/password`) | yes |
| `VHI_SSH_PORT`, `VHI_SSH_USER`, `VINFRA_USERNAME` | operator-tunable knobs the credential form already exposes | no |
| `E2E_VHI_BASE_URL` | the observed base URL | no |

`env_contract` reserves the first four, on top of the platform's own reserved set (PRD §5.4; none of the four is in it): a run parameter
of one of those names is refused, because each is a fact about the target and overriding one would
point the run at a different host or credential. The knobs are left open because an operator could
have typed the same value into the credential form, so an override escalates nothing.
`E2E_VHI_BASE_URL` is left open for parity with VHP's `E2E_VHP_BASE_URL`, which a run parameter may override. There is no
`VINFRA_PASSWORD` variable at all: the password reaches the run only as a mounted file.

## Verification

* Plugin boot and resolution in `qa_product_plugin_boot.rs`.
* `assert_no_leak` with a planted private key and password.
* Connector tests covering agent lifetime, authentication, session handling and error mapping,
  each asserting no credential material reaches a formatted surface.
