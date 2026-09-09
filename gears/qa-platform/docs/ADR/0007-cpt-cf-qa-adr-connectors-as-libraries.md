---
status: accepted
date: 2026-09-08
---
# Connectors are plain libraries, not gears and not plugins

**ID**: `cpt-cf-qa-adr-connectors-as-libraries`

## Context and Problem Statement

A product plugin has to reach its environment somehow. The VHP plugin talks to a Kubernetes API;
the VHI plugin opens SSH sessions and drives a CLI. That transport code is substantial — a Kubernetes
client with its error taxonomy and secret writer, an SSH session layer with agent handling — and it
is reusable.

The workspace has a plugin mechanism close at hand, and the transports look superficially like the
same kind of thing a product plugin is. Should a connector be a gear with a GTS identity, resolved
at runtime the way a product plugin is?

## Decision Drivers

* A product plugin is a *policy* decision: which behaviour governs this product. It must be
  resolvable per product at runtime.
* A transport is not a policy decision: the VHI plugin needs SSH because VHI is reached over SSH,
  and no deployment should be able to re-bind that.
* Gears cost registration, identity, lifecycle and a stability contract.
* Connectors carry the credential-handling rules that must be auditable in one place.

## Considered Options

* Plain library crates that plugins link
* Gears with GTS identities, resolved from `ClientHub`
* Modules inside each product plugin, not shared at all

## Decision Outcome

Chosen option: **plain library crates**. `qa-connector-k8s` and `qa-connector-ssh` have no gear,
no GTS identity, and nothing resolves them at runtime. A product plugin depends on one the way it
depends on any crate.

**The distinction from a product plugin is load-bearing and is the point of this ADR.** A product
plugin exists to be swapped per product; a connector exists to be linked. Giving a connector an
identity would mean a deployment could substitute the transport underneath a plugin — which is not
a capability anyone wants, and is a capability that would have to be secured.

The split is also what lets the credential rules live in one auditable place.
`qa-connector-ssh` owns them for the SSH transport:

* A private key travels credstore → memory → a pipe → a short-lived `ssh-agent`. Never a file,
  never `argv`, never an environment variable.
* A secret reaches a remote command on **stdin**, because `sshd`'s `AcceptEnv` discards the
  environment channel and `argv` is world-readable through `/proc`.

### Consequences

* Good, because a connector is ordinary code: no registration, no lifecycle, no identity.
* Good, because the transport a plugin uses is fixed at compile time and visible in its
  `Cargo.toml`.
* Good, because credential handling for a transport is auditable in one crate.
* Good, because `qa-connector-k8s` stays out of every default build — only the VHP plugin links it,
  which is what keeps `cpt-cf-qa-constraint-no-kube` true for everything else.
* Bad, because a connector cannot be replaced without a rebuild, which is the intent but is still a
  constraint.
* Bad, because two things that look similar from outside — plugin and connector — are governed by
  different rules, and the difference has to be taught.

### Confirmation

* Neither connector crate contains a `#[toolkit::gear]` or registers anything in `ClientHub`.
* `qa-connector-k8s` appears only in `qa-vhp-product-plugin`'s dependencies;
  `qa-connector-ssh` only in `qa-vhi-product-plugin`'s.
