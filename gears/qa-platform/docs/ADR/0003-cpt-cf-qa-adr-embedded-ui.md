---
status: accepted
date: 2026-08-12
---
# UI delivery: embedded SPA gear instead of a separate static deployment

**ID**: `cpt-cf-qa-adr-embedded-ui`

## Context and Problem Statement

The source system ships its React SPA as a separate nginx container that proxies `/api/*` to the manager. QA Platform must deliver the same UI in all Fabric deployment shapes, including single-node hosts with no web infrastructure. How is the SPA packaged and served?

## Decision Drivers

* Single-node deployments must be self-contained — a separate nginx container contradicts the shape.
* The repository already has a precedent: api-gateway embeds web assets via `rust_embed` behind an `embed_elements` feature flag.
* The SPA build requires a Node toolchain, which is foreign to the cargo workspace and CI matrix.
* User decision during design review: the UI must be part of the gear subsystem.

## Considered Options

* Dedicated `qa-ui` gear embedding the built SPA via `rust_embed` (feature-gated)
* Separate static deployment (today's nginx model) behind the gateway
* Embed assets into one of the domain gears (e.g., qa-runs)

## Decision Outcome

Chosen option: "Dedicated `qa-ui` gear embedding the built SPA (feature-gated)", because it makes the UI a composable platform artifact — any host binary that includes the gear serves the UI — while the feature flag keeps pure-Rust builds green without Node, and a dedicated gear keeps domain gears free of presentation concerns.

### Consequences

* New thin gear `qa-ui`: asset routes under `/qa/ui/*` with SPA fallback to `index.html`, correct content types and cache headers; no DB, no domain logic.
* Build pipeline must produce the SPA `dist/` before compiling `qa-ui` with the embed feature (Makefile/CI target); without the feature the gear serves a "UI not embedded" placeholder.
* Host binary size grows by the asset bundle (single-digit MB); acceptable for the shapes involved.
* Kubernetes deployments may still choose a CDN/static path by disabling the feature — the ADR's alternative remains available operationally, but the embedded path is the supported default.
* The SPA itself is adapted separately (API base `/qa/v1`, platform token auth, WS→SSE for logs) per `cpt-cf-qa-fr-ui-parity`.

### Confirmation

Single-node example app serves the full UI with no external web server (e2e smoke test). CI verifies both feature states compile: with embed (after SPA build) and without (no Node in path).

## Pros and Cons of the Options

### Dedicated qa-ui gear with rust_embed

* Good, because UI availability becomes a property of composition, not deployment topology.
* Good, because it follows an existing in-repo pattern (api-gateway `embed_elements`).
* Good, because domain gears stay presentation-free.
* Neutral, because binary size grows modestly.
* Bad, because the Node build step must be orchestrated ahead of cargo in CI.

### Separate static deployment (nginx)

* Good, because it changes nothing about today's UI operations and keeps binaries small.
* Bad, because single-node shapes are not self-contained — the primary requirement fails.
* Bad, because a second deployment artifact must be versioned in lockstep with the API.

### Embed into an existing domain gear

* Good, because no new crate.
* Bad, because it welds presentation lifecycle to a domain gear and violates single-responsibility layout rules the repo lints for.

## More Information

Precedent: `gears/system/api-gateway/src/assets.rs` (`RustEmbed`, `embed_elements` feature).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-qa-fr-ui-embedded` — realized by this decision
* `cpt-cf-qa-fr-ui-parity` — constrains the adaptation scope
* `cpt-cf-qa-component-ui` — the component this decision creates
