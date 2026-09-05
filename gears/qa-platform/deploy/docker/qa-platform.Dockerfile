# Multi-stage build for the qa-platform compose stack's gears server.
#
# Modeled on testing/docker/cyberware.Dockerfile: same pinned builder image,
# same protobuf toolchain, same workspace copies. It differs only in which
# binary/features get built and in the runtime stage's config + entrypoint,
# which this stack needs to point the server at Postgres inside compose
# (see gears/qa-platform/deploy/docker/entrypoint.sh for why that can't be done via `${VAR}`
# expansion in the config file itself).

# Stage 1: Builder
FROM rust:1.95.0-bookworm@sha256:6bb82db0878825e157664188b319c875de4f1fff5d70f5917b3a3f1974b472e4 AS builder

# Build arguments for cargo features
#
# `oidc-authn`, not `static-authn`: Phase C validates real JWTs, so
# gears/qa-platform/config/qa-platform-stack.yaml configures
# `oidc-authn-plugin` instead of `static-authn-plugin` and this image has to
# contain that plugin for the config to load. Both features existing at once
# would be harmless (each is just an optional dependency plus a `use ... as _`
# in apps/cf-gears-example-server/src/registered_gears.rs), but only one authn
# plugin is configured, so only one is compiled in.
#
# `oidc-authn` did NOT exist before Task 13. It was added to
# apps/cf-gears-example-server/Cargo.toml alongside the `static-authn` entry it
# is modelled on; the plugin crate (cf-gears-oidc-authn-plugin, lib name
# `oidc_authn_plugin`) was already in the tree but wired into no binary.
#
# `tenant-resolver-rg`, not `single-tenant`, since Task 14. Unlike `oidc-authn`
# this feature already existed (apps/cf-gears-example-server/Cargo.toml:25,
# `tenant-resolver-rg = ["dep:rg-tr-plugin"]`), so the swap is a one-word
# change here plus the matching gear section in qa-platform-stack.yaml. The
# feature name does NOT follow the `single-tenant` / `static-tenants` pattern
# of the two features beside it -- it is `tenant-resolver-rg`, not `rg-tenant`
# or `tenant-rg`; copy it from that Cargo.toml rather than guessing.
#
# READ THIS BEFORE BRINGING UP A STACK WITH AN EMPTY `resource_group` DATABASE.
# rg-tr-plugin answers every tenant query out of Resource Group rows, and oagw
# -- a NON-optional dependency of this binary, so always compiled in -- calls
# `TenantResolverClient::get_root_tenant` in its `post_init`
# (gears/system/oagw/oagw/src/gear.rs:247-252) and turns any error into an
# `anyhow!`, which `run_post_init_phase` propagates with `?`
# (libs/toolkit/src/runtime/host_runtime.rs:371-377) -- i.e. it aborts the
# boot. With no tenant row in Resource Group there is nothing to resolve, so
# the server exits instead of serving, and the only way to create that row is
# the REST API of the server that just exited. That circle is broken by
# deploy/compose/seed-tenant.sh, which the compose stack runs as a one-shot
# `tenant-seed` service; see its header for the ordering.
ARG CARGO_FEATURES=qa-platform,oidc-authn,static-authz,tenant-resolver-rg,static-credstore,postgres-credstore,runner-secret
#
# `postgres-credstore` compiles in the database-backed credstore plugin, which
# is what makes a stored secret survive this container being recreated. It is
# in the default as of 2026-08-27: without it every secret lives in a HashMap
# and dies on `docker compose up --build`, which cost the human their SSH key
# four times in one day.
#
# It is ONE OF THREE places the switch lives, and all three must agree:
#   1. this ARG default            -- the local stack and any plain build
#   2. deploy/compose/docker-compose.argo.yml's `CARGO_FEATURES:` default
#      -- that file carries its OWN COPY of this list, so a feature added
#      here and not there is silently absent from every `--argo` deploy,
#      which is how the remote is deployed
#   3. `credstore.config.vendor` in
#      gears/qa-platform/config/qa-platform-stack.yaml, which must say
#      "constructorfabric-postgres"
# Any one of the three left behind is a no-op that looks like a deploy.

# Install protobuf-compiler for prost-build, plus cmake and golang-go for two
# native crypto/compression builds that turn out to be unconditional for this
# workspace, not gated by $CARGO_FEATURES (verified with `cargo tree -p
# cf-gears-example-server -i <crate>` using no features at all -- a `-p`-scoped
# `cargo tree`, unlike the real workspace-wide `cargo build` below, does NOT
# reproduce the second one, which is why it took two build rounds to find):
#
# - cmake: cf-gears-oagw (non-optional dep of cf-gears-example-server) pulls
#   pingora-core, which pins `flate2 = { features = ["zlib-ng"], default-features
#   = false }` unconditionally -- not flate2's own default, which is the
#   pure-Rust `rust_backend` needing no native build at all. zlib-ng's
#   `libz-ng-sys` build script shells out to `cmake`.
# - golang-go: `libs/rustls-fips-shim` is a *workspace member* (root
#   Cargo.toml `members`), and Cargo unifies features for a shared dependency
#   across every member of the resolve, not just the one binary being
#   compiled -- `cargo metadata` (no `-p`, no `--features` at all) shows
#   `aws-lc-rs`'s `fips` feature active workspace-wide purely because that
#   shim exists, regardless of $CARGO_FEATURES or of toolkit-http's own
#   (otherwise-unactivated) `fips` feature gating it as optional. `fips` pulls
#   `aws-lc-fips-sys`, whose vendored AWS-LC CMakeLists (`if(FIPS) ... Building
#   AWS-LC for FIPS requires Go and Perl`) hard-requires both; minimum Go per
#   its own `cmake/go.cmake` is 1.17.13, and Debian bookworm's `golang-go`
#   (2:1.19~1, verified `go version` -> go1.19.8) clears that. Perl 5.36 is
#   already in the base image, so it needs no apt package. This FIPS pull is
#   worth a human's attention: it means every image built from this workspace
#   pays for the FIPS-validated AWS-LC native build, not only images that ask
#   for it -- out of scope to change here, so worked around rather than fixed.
# `make`/`gcc`/`g++`/`cc`/`pkg-config` were confirmed already present in the
# rust:1.95.0-bookworm base image (`docker run --rm <image> command -v ...`);
# of this block's additions, only `cmake` and `golang-go` were actually missing.
RUN apt-get update && \
    apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev cmake golang-go && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY rust-toolchain.toml ./

# Copy all workspace members (`tools/` holds two workspace members --
# gts-analyze and xtask -- so it has to come along or `cargo build` fails
# resolving the workspace before it ever gets to compiling anything).
# Note: cyberware.Dockerfile also copies `apps/gts-docs-validator`, but that
# directory does not exist in this tree (nor is it a workspace member in
# Cargo.toml any more) -- omitted here rather than copied blind.
COPY apps/cf-gears-example-server ./apps/cf-gears-example-server
COPY tools ./tools
COPY libs ./libs
COPY gears ./gears
COPY examples ./examples
# `config/` here is the repo-root config/ tree (qa-platform.yaml,
# quickstart-windows.yaml, etc.) -- distinct from
# gears/qa-platform/config/qa-platform-stack.yaml, which the stack actually
# runs on and which arrives via `COPY gears ./gears` above. This directory is
# not read by the release binary itself: the only reference to it under
# gears/qa-platform is a `#[cfg(test)]` unit test in qa-runs
# (include_str!("../../../../../config/qa-platform.yaml")), and
# `cargo build --release --bin cf-gears-example-server` does not compile test
# code. Kept anyway -- dropping it would shift this layer's cache key and the
# cargo-build layer after it, discarding an already-warm release build for no
# runtime benefit.
COPY config ./config
COPY proto ./proto

# Build the cf-gears-example-server binary in release mode with the
# qa-platform feature set (this is what actually compiles in the four
# qa-platform gears and the plugins the stack config depends on).
RUN cargo build --release --bin cf-gears-example-server --features "$CARGO_FEATURES"

# Stage 2: Runtime - must match builder's base OS
FROM debian:13.3-slim

# `ca-certificates` -- the runtime stage's first apt package, so this is the
# first RUN to install anything here; kept single-`RUN` /
# `--no-install-recommends` / `rm -rf /var/lib/apt/lists/*` to match the
# builder stage's idiom above rather than starting a second style.
#
# Without it, qa-catalog's git sync (ADR-0005, `qa-catalog/src/infra/git/gix_sync.rs`)
# fails on *every* repository, `http://` and `https://` alike, with:
#   "repository sync failed: clone failed: Could not initialize the http
#   client: builder error: unexpected error: No CA certificates were loaded
#   from the system"
# found by `deploy/compose/smoke.sh` (Task 5) actually driving a clone
# through this image rather than asserting against its shape. The mechanism
# is not "no cert store means no HTTPS": qa-catalog's gix is wired through
# `blocking-http-transport-reqwest-rust-tls` (root Cargo.toml), and that
# reqwest+rustls client fails to *build* -- before it inspects the request's
# URL scheme at all -- when `rustls-native-certs` loads zero roots from an
# empty `/etc/ssl/certs`, which is what a bare `debian:13.3-slim` has. A
# `http://`-only reader may be tempted to drop this as unnecessary; it is
# not -- the failure has nothing to do with which scheme is in use.
#
# `openssh-client` -- added 2026-08-27 with SSH git remote support (ADR-0005
# as amended, `cpt-cf-qa-adr-git-egress`). It provides the three binaries
# `qa-catalog/src/infra/git/ssh_agent.rs` executes for an `ssh://` or
# scp-like remote: `ssh-agent` (a short-lived per-sync agent), `ssh-add`
# (loads the credstore-held private key over **stdin**, so the key never
# becomes a file), and `ssh` (which gix invokes via `core.sshCommand`).
#
# This is a real, knowingly-paid cost against ADR-0005's original driver
# "no shelling out to a binary that may not exist in the container image":
# without this package an ssh sync fails at `SshAgent::start_with_key` with
# "failed to start ssh-agent". The amended ADR records why the trade was
# accepted -- the alternative was writing key material to disk, which is
# what the agent approach exists to avoid.
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates openssh-client && \
    rm -rf /var/lib/apt/lists/*

# Copy the built binary from builder stage
COPY --from=builder /build/target/release/cf-gears-example-server /usr/local/bin/cf-gears-example-server

# Copy just the qa-platform stack config so /etc/cf-gears/qa-platform-stack.yaml
# (referenced by CMD below) exists. It now lives at
# gears/qa-platform/config/qa-platform-stack.yaml, not under the repo-root
# config/ copied above -- it already arrived in the builder stage via the
# `COPY gears ./gears` line, so this pulls the one file from there rather
# than copying a whole directory. gears/qa-platform/deploy/docker/entrypoint.sh
# treats this file strictly as a read-only template: it renders the Postgres
# fields into a separate file under /var/lib/cf-gears and points the server
# at that rendered copy, without ever opening this file for writing. This
# file may therefore be mounted read-only -- e.g. a Kubernetes ConfigMap
# volume (Task 17) -- unlike an earlier revision of the entrypoint, which
# rewrote this file in place and did require it to stay writable by the
# runtime user. The `chown -R 1000:1000 /etc/cf-gears` below is no longer
# required by the entrypoint for that reason; left as-is here since changing
# runtime file ownership is a behaviour change out of scope for this comment
# fix.
COPY --from=builder /build/gears/qa-platform/config/qa-platform-stack.yaml /etc/cf-gears/qa-platform-stack.yaml

COPY gears/qa-platform/deploy/docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

EXPOSE 8087

# `-m` matters: without it, no /home/appuser is ever created, yet Docker
# still sets HOME=/home/appuser from the /etc/passwd entry useradd writes
# (verified with `getent passwd 1000` / `echo $HOME` against this image).
# server.home_dir: "~/.cf-gears" (gears/qa-platform/config/qa-platform-stack.yaml)
# resolves against $HOME via `env::home_dir()`
# (libs/toolkit/src/bootstrap/host/paths/home_dir.rs), and /home itself
# stays root-owned (0755) with no such directory in it -- appuser can't
# create it either. Without `-m` the container dies at startup with
# "Error: Failed to resolve server.home_dir ... Permission denied", found
# and initially worked around with a compose-level `HOME: /tmp` override
# now removed in favor of this proper fix.
RUN useradd -m -U -u 1000 appuser && \
    chown -R 1000:1000 /etc/cf-gears

# Working directory for the process's relative-path defaults, most notably
# qa-catalog's `repos_dir`/`bundles_dir`
# (gears/qa-platform/qa-catalog/qa-catalog/src/config.rs: "./data/qa-catalog/repos"
# and ".../bundles"), which gears/qa-platform/config/qa-platform-stack.yaml does not
# override. Without an explicit, owned WORKDIR the runtime stage's default
# CWD is `/`, root-owned, and qa-catalog's init fails with "failed to
# create the repositories directory './data/qa-catalog/repos': Permission
# denied" -- found and initially worked around with a compose-level
# `working_dir: /tmp` override now removed in favor of this proper fix.
# /var/lib/cf-gears, not /tmp: this directory ends up holding cloned git
# repositories and content bundles synced from tenant test repos, which
# belongs with other persistent application state (conventionally
# /var/lib/<app>), not under a world-writable, OS-managed scratch directory
# that other processes on the same host share and that expects to be
# cleared without warning.
#
# `data/` is created here, and not left to the process, for a reason that only
# shows up under compose: docker-compose.yml now mounts a named volume at
# /var/lib/cf-gears/data so the clones survive a container recreate. Docker
# seeds a fresh named volume from the image's content *and ownership* at the
# mount point -- and a mount point that does not exist in the image gets a
# root-owned empty directory instead, which the uid-1000 process cannot write.
# That reproduces the exact failure this WORKDIR was introduced to fix
# ("failed to create the repositories directory './data/qa-catalog/repos':
# Permission denied"), just one directory deeper. The two leaf directories are
# created too so the seeded ownership covers the paths qa-catalog actually
# opens, not only their parent.
RUN mkdir -p /var/lib/cf-gears/data/qa-catalog/repos \
             /var/lib/cf-gears/data/qa-catalog/bundles && \
    chown -R 1000:1000 /var/lib/cf-gears
WORKDIR /var/lib/cf-gears

USER 1000

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["/usr/local/bin/cf-gears-example-server", "--config", "/etc/cf-gears/qa-platform-stack.yaml", "run"]
