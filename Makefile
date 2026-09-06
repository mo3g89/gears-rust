CI := 1

# Python interpreter used by helper scripts. Defaults to python3 (Linux/macOS);
# on Windows, where `python3` is often absent, it falls back to `python`.
# Override explicitly with `make <target> PYTHON=...` if needed.
PYTHON ?= $(shell command -v python3 2>/dev/null || command -v python 2>/dev/null || echo python3)

OPENAPI_URL ?= http://127.0.0.1:8087/cf/openapi.json
OPENAPI_OUT ?= docs/api/api.json

# E2E feature set (single source of truth: config/e2e-features.txt)
E2E_FEATURES ?= $(strip $(shell cat config/e2e-features.txt 2>/dev/null))
E2E_ARGS ?= $(if $(E2E_FEATURES),--features $(E2E_FEATURES),)

# Nightly toolchain for targets that need unstable rustc flags (currently only
# `shear`, which drives -Zunpretty=expanded). This default serves local runs;
# CI overrides it via `make shear RUST_NIGHTLY=...` so the toolchain it installs
# and caches cannot drift from the one that actually compiles.
RUST_NIGHTLY ?= nightly-2026-04-16

# cargo-shear version installed by `make setup`. Pinned because an unused-dep
# verdict that disagrees with CI is worse than no local check at all.
# Keep in sync with the `Install cargo-shear` step in shear-nightly.yml.
SHEAR_VERSION ?= 1.13.1

# -------- Utility macros --------

define check_tool
    @command -v $(1) >/dev/null || (echo "ERROR: $(1) is not installed. Run 'make setup' to install required tools." && exit 1)
endef

# fips-policy relies on cargo-deny bans-check behavior only available since 0.20.0
DENY_MIN_VERSION := 0.20.0

define check_deny_version
    @DENY_VERSION=$$(cargo deny --version 2>/dev/null | awk '{print $$2}'); \
	if [ -z "$$DENY_VERSION" ] || ! $(PYTHON) -c "import sys; sys.exit(0 if tuple(map(int, '$$DENY_VERSION'.split('.'))) > tuple(map(int, '$(DENY_MIN_VERSION)'.split('.'))) else 1)" 2>/dev/null; then \
		echo "ERROR: cargo-deny > $(DENY_MIN_VERSION) is required (found: $${DENY_VERSION:-none}). Run 'cargo install --locked cargo-deny' to upgrade." && exit 1; \
	fi
endef

define check_rustup_component
    @command -v rustup >/dev/null || (echo "ERROR: rustup not installed. Install rustup or run 'make setup'." && exit 1)
	@rustup component list --installed | grep -q '^$(1)' || (echo "ERROR: $(1) component not installed. Run 'rustup component add $(1)' or 'make setup'." && exit 1)
endef

# Generic server start/stop with cleanup (cross-platform: Linux, Mac, Windows)
# Usage: $(call start_server_and_wait,<command>,<health_url>,<max_wait_seconds>)
# Args:
#   1. command - Full command to start the server (e.g., cargo run --bin server)
#   2. health_url - URL to poll for server readiness (e.g., http://localhost:8080/health)
#   3. max_wait_seconds - Maximum time to wait for server to be ready (e.g., 300)
# Returns: Sets $$SERVER_PID variable for use in the recipe
# Cleanup: Automatically kills server on EXIT/INT/TERM (normal or error)
# Features:
#   - Cross-platform: Works on Linux, Mac, and Windows (Git Bash/WSL/MSYS2)
#   - Logs server output to temp directory for debugging
#   - Exponential backoff polling (1s, 2s, 4s, 8s intervals)
#   - Detects if server dies unexpectedly during startup
#   - Graceful shutdown with SIGTERM, then SIGKILL if needed (or taskkill on Windows)
# Example:
#   @$(call start_server_and_wait,cargo run --bin my-server,http://localhost:8080/health,60); \
#   curl http://localhost:8080/api/data -o output.json
define start_server_and_wait
	TEMP_DIR=$$(if [ -n "$$TEMP" ]; then echo "$$TEMP"; elif [ -n "$$TMP" ]; then echo "$$TMP"; else echo "/tmp"; fi); \
	LOG_FILE="$$TEMP_DIR/server-$$$$.log"; \
	HEALTH_PORT=$$(echo "$(2)" | sed -E 's|.*://[^:]+:([0-9]+).*|\1|'); \
	if [ -n "$$HEALTH_PORT" ] && command -v lsof >/dev/null 2>&1; then \
		STALE_PIDS=$$(lsof -tiTCP:$$HEALTH_PORT -sTCP:LISTEN 2>/dev/null); \
		if [ -n "$$STALE_PIDS" ]; then \
			echo "Killing stale LISTEN processes on port $$HEALTH_PORT (PIDs: $$STALE_PIDS)"; \
			echo "$$STALE_PIDS" | xargs kill 2>/dev/null || true; \
			sleep 1; \
		fi; \
	fi; \
	$(1) > "$$LOG_FILE" 2>&1 & \
	SERVER_PID=$$!; \
	echo "Server started with PID: $$SERVER_PID (log: $$LOG_FILE)"; \
	is_process_running() { \
		if command -v kill >/dev/null 2>&1; then \
			kill -0 $$1 2>/dev/null; \
		elif command -v tasklist >/dev/null 2>&1; then \
			tasklist /FI "PID eq $$1" 2>NUL | grep -q "$$1"; \
		else \
			ps -p $$1 >/dev/null 2>&1; \
		fi; \
	}; \
	kill_process() { \
		PID_TO_KILL=$$1; \
		FORCE=$$2; \
		if command -v kill >/dev/null 2>&1; then \
			if [ "$$FORCE" = "force" ]; then \
				kill -9 $$PID_TO_KILL 2>/dev/null || true; \
			else \
				kill $$PID_TO_KILL 2>/dev/null || true; \
			fi; \
		elif command -v taskkill >/dev/null 2>&1; then \
			if [ "$$FORCE" = "force" ]; then \
				taskkill /PID $$PID_TO_KILL /F /T 2>NUL || true; \
			else \
				taskkill /PID $$PID_TO_KILL /T 2>NUL || true; \
			fi; \
		fi; \
	}; \
	cleanup_server() { \
		if is_process_running $$SERVER_PID; then \
			echo "Stopping server (PID $$SERVER_PID)..."; \
			kill_process $$SERVER_PID; \
			sleep 1; \
			if is_process_running $$SERVER_PID; then \
				echo "Server still running, forcing shutdown..."; \
				kill_process $$SERVER_PID force; \
				sleep 1; \
			fi; \
			wait $$SERVER_PID 2>/dev/null || true; \
			echo "Server stopped."; \
		fi; \
	}; \
	trap cleanup_server EXIT INT TERM; \
	echo "Waiting for $(2) to become ready..."; \
	ELAPSED=0; MAX_WAIT=$(3); SLEEP=1; \
	while ! curl -fsS "$(2)" -o /dev/null 2>/dev/null; do \
		if ! is_process_running $$SERVER_PID; then \
			echo "ERROR: Server process died unexpectedly. Check $$LOG_FILE"; \
			exit 1; \
		fi; \
		if [ $$ELAPSED -ge $$MAX_WAIT ]; then \
			echo "ERROR: Server did not become ready in time. Check $$LOG_FILE"; \
			exit 1; \
		fi; \
		echo "Waiting for server... ($$ELAPSED s)"; \
		sleep $$SLEEP; \
		ELAPSED=$$((ELAPSED + SLEEP)); \
		SLEEP=$$((SLEEP < 8 ? SLEEP*2 : 8)); \
	done; \
	echo "Server is ready!"
endef

# -------- Defaults --------

# Show the help message with list of commands (default target)
help:
	@$(PYTHON) tools/scripts/make_help.py Makefile


# -------- Set up --------
# Note: .setup-stamp should be added to .gitignore

.PHONY: setup

## Install all required development tools
setup: .setup-stamp

# Re-run setup whenever the tool list changes (Makefile is the source of truth),
# so developers who already have .setup-stamp pick up newly-added tools.
.setup-stamp: Makefile
	@echo "Installing required development tools..."
	rustup component add clippy
	cargo install lychee
	cargo install cargo-geiger
	cargo install cargo-deny
	cargo install cargo-gears
	cargo install cargo-fuzz
	cargo install cargo-hack
	cargo install --locked cargo-shear --version $(SHEAR_VERSION)
	cargo install gts-validator
	@if echo "$$OS" | grep -iq windows || [ -n "$$COMSPEC" ]; then \
		echo "NOTE: kani-verifier is not supported on Windows; skipping (use WSL2/Docker for Kani)."; \
		echo "Installing cargo-llvm-cov (supported on Windows; needs llvm-tools-preview)..."; \
		rustup component add llvm-tools-preview; \
		cargo install cargo-llvm-cov; \
		if ! command -v nasm >/dev/null 2>&1; then \
			echo "Installing NASM (required by aws-lc-sys on Windows)..."; \
			winget install NASM.NASM --accept-source-agreements --accept-package-agreements || \
				echo "WARNING: NASM auto-install failed. Install manually from https://www.nasm.us/"; \
		fi; \
	else \
		cargo install --locked kani-verifier && \
		cargo kani setup && \
		cargo install cargo-llvm-cov; \
	fi
	@echo "Setup complete. All tools installed."
	@touch .setup-stamp

# -------- Code safety checks --------
#
# Tool Comparison - What Each Tool Checks:
# +-------------+----------------------------------------------------------------------+
# | Tool        | Checks Performed                                                     |
# +-------------+----------------------------------------------------------------------+
# | clippy      | - Idiomatic Rust patterns (e.g., use of .iter() vs into_iter())      |
# |             | - Common mistakes (e.g., unnecessary clones, redundant closures)     |
# |             | - Performance issues (e.g., inefficient string operations)           |
# |             | - Style violations (e.g., naming conventions, formatting)            |
# |             | - Suspicious constructs (e.g., comparison to NaN, unused results)    |
# |             | - Complexity warnings (e.g., too many arguments, cognitive load)     |
# +-------------+----------------------------------------------------------------------+
# | kani        | - Memory safety proofs (buffer overflows, null pointer dereferences) |
# |             | - Arithmetic overflow/underflow in all possible execution paths      |
# |             | - Assertion violations (panics, unwrap failures)                     |
# |             | - Undefined behavior detection                                       |
# |             | - Concurrency issues (data races, deadlocks) with #[kani::proof]     |
# |             | - Custom invariants and postconditions verification                  |
# +-------------+----------------------------------------------------------------------+
# | geiger      | - Unsafe blocks in your code and dependencies                        |
# |             | - FFI (Foreign Function Interface) calls                             |
# |             | - Raw pointer dereferences                                           |
# |             | - Mutable static variables access                                    |
# |             | - Inline assembly usage                                              |
# |             | - Dependency tree visualization of unsafe code usage                 |
# +-------------+----------------------------------------------------------------------+
# | lint        | - Compiler warnings treated as errors (unused variables, imports)    |
# |             | - Dead code detection                                                |
# |             | - Type inference failures                                            |
# |             | - Deprecated API usage                                               |
# |             | - Missing documentation warnings                                     |
# |             | - Ensures clean compilation across all targets and features          |
# +-------------+----------------------------------------------------------------------+

.PHONY: fmt clippy clippy-deep lychee docs-preview kani geiger safety lint dylint dylint-list dylint-test shear gts-docs cfs-ensure cfs-repair cfs-validate cfs-validate-kits cfs-validate-kit-local cfs-spec-coverage ensure-submodules

## Verify git submodules (e.g. guidelines/DNA) are initialized; fails otherwise.
ensure-submodules:
	@if git submodule status --recursive 2>/dev/null | grep -q '^-'; then \
		echo "ERROR: Uninitialized git submodules detected. Run 'git submodule update --init --recursive'." && exit 1; \
	fi

# Check code formatting
fmt:
	$(call check_rustup_component,rustfmt)
	cargo fmt --all --check

CFS ?= cfs
CFS_PIPX_SPEC ?= git+https://github.com/constructorfabric/studio.git
export PATH := $(HOME)/.local/bin:$(PATH)

# Fast two-pass clippy used in PR CI (target: <5 min with sccache).
#
# Pass 1 — one workspace-wide all-features run.
#   Covers every crate, every target, every additive feature combination.
#   80+ "leaf" crates (gears, plugins, SDKs) use `dep:` guards only, so
#   --all-features is sufficient — no --each-feature needed.
#
# Pass 2 — cargo-hack --each-feature on the three crates with real
#   #[cfg(feature = "...")] guards (mutually-exclusive DB backends, otel,
#   fips, db). These account for >95% of all cfg-gated lines in the repo.
#   See GH issue #1574 for original motivation.
#
# Use `make clippy-deep` for the full 182-run matrix (nightly / pre-release).
CLIPPY_FLAGS := -- -D warnings -D clippy::perf
CLIPPY_HACK_CRATES := -p cf-gears-toolkit -p cf-gears-toolkit-db -p cf-gears-toolkit-http

clippy:
	$(call check_rustup_component,clippy)
	$(call check_tool,cargo-hack)
	cargo clippy --workspace --all-targets --all-features $(CLIPPY_FLAGS)
	cargo hack clippy $(CLIPPY_HACK_CRATES) --all-targets --each-feature $(CLIPPY_FLAGS)

## Full feature-matrix clippy: one pass per (crate × feature).
## ~182 runs — intended for nightly CI and pre-release validation, not PRs.
clippy-deep:
	$(call check_rustup_component,clippy)
	$(call check_tool,cargo-hack)
	cargo hack clippy --workspace --all-targets --each-feature $(CLIPPY_FLAGS)

# Run markdown checks with 'lychee'
lychee: ensure-submodules
	$(call check_tool,lychee)
	lychee --exclude-path 'docs/web-docs' docs examples guidelines gears/system/event-broker/docs

## Validate internal links in web-docs.
# The web-docs pages use Starlight route-relative links (e.g. ../foo/) that only
# resolve against the *built* site, not the markdown source — so we build the
# docs site with the local content and run lychee over the generated HTML.
WEB_DOCS_CACHE ?= .web-docs-preview
web-docs-check:
	$(call check_tool,lychee)
	@bash tools/scripts/docs-preview.sh build
	lychee --offline --root-dir '$(abspath $(WEB_DOCS_CACHE)/dist)' --exclude 'i18n' '$(WEB_DOCS_CACHE)/dist/**/*.html'

## The Kani Rust Verifier for checking safety of the code
kani:
	$(call check_tool,kani)
	cargo kani --workspace --all-features

## Run Geiger scanner for unsafe code in dependencies
geiger:
	$(call check_tool,cargo-geiger)
	cd apps/cf-gears-example-server && cargo geiger --all-features

## Check there are no compile time warnings
lint:
	RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --all-features

## Validate GTS identifiers in .md and .json files (DE0903)
# Uses gts-validator binary (install via: cargo install gts-validator)

gts-docs:
	$(call check_tool,gts-validator)
	gts-validator \
		--vendor cf,vendor,example,fabrikam \
		--exclude "target/*" \
		--exclude "docs/api/*" \
		--exclude "docs/web-docs/*" \
		--exclude "gears/chat-engine/*" \
		--exclude "**/helm/*/templates/*" \
		docs gears libs examples

# cli_smoke_tests.rs / migrate_command_tests.rs rely on CARGO_BIN_EXE_<name> being
# set at test runtime, which cargo-nextest only supports from 0.9.130 onward.
NEXTEST_MIN_VERSION := 0.9.130

install-tools:
	@NEXTEST_VERSION=$$(cargo nextest --version 2>/dev/null | awk '/^cargo-nextest/ {print $$2}'); \
	if [ -z "$$NEXTEST_VERSION" ] || ! $(PYTHON) -c "import sys; sys.exit(0 if tuple(map(int, '$$NEXTEST_VERSION'.split('.'))) >= tuple(map(int, '$(NEXTEST_MIN_VERSION)'.split('.'))) else 1)" 2>/dev/null; then \
		echo "Installing/upgrading cargo-nextest (>= $(NEXTEST_MIN_VERSION) required for CARGO_BIN_EXE_* support)..."; \
		cargo install --locked cargo-nextest; \
	fi
	@DENY_VERSION=$$(cargo deny --version 2>/dev/null | awk '{print $$2}'); \
	if [ -z "$$DENY_VERSION" ] || ! $(PYTHON) -c "import sys; sys.exit(0 if tuple(map(int, '$$DENY_VERSION'.split('.'))) > tuple(map(int, '$(DENY_MIN_VERSION)'.split('.'))) else 1)" 2>/dev/null; then \
		echo "Installing/upgrading cargo-deny (> $(DENY_MIN_VERSION) required for fips-policy)..."; \
		cargo install --locked cargo-deny; \
	fi

# Run architecture lints via cargo-gears (see Gears.toml for configuration).
dylint:
	$(call check_tool,cargo-gears)
	cargo gears lint --dylint

# Check for unused dependencies with cargo-shear.
shear:
	$(call check_tool,cargo-shear)
	cargo +$(RUST_NIGHTLY) shear --expand --deny-warnings

# Run all code safety checks
safety: clippy kani lint dylint # geiger
	@echo "OK. Rust Safety Pipeline complete"

## Validate gear folder names follow kebab-case convention
validate-gear-names:
	@$(PYTHON) tools/scripts/validate_gear_names.py

## Validate readme/license-file paths declared by publishable crates exist
check-packaging-metadata:
	@$(PYTHON) tools/scripts/check_packaging_metadata.py

# -------- Code security checks --------

.PHONY: deny fips-policy security

# Check licenses and dependencies
deny:
	$(call check_tool,cargo-deny)
	$(call check_deny_version)
	cargo deny check

## FIPS dependency-graph policy (see deny-fips.toml + ADR 0005).
## Refuses the build if any non-FIPS-validated crypto crate enters the
## --features fips dep graph. Build-time analogue of Go 1.25 fips140=only.
## Run on every PR that touches deps.
fips-policy:
	$(call check_tool,cargo-deny)
	$(call check_deny_version)
	cargo deny --config deny-fips.toml check bans

security: deny fips-policy

# -------- Studio --------

# Validate Constructor Studio artifacts (specs, code, templates).
cfs-validate: cfs-repair
	$(CFS) validate && echo "OK. Constructor Studio validation PASSED" || (echo "ERROR: Constructor Studio validation FAILED"; exit 1)

# Ensure the Constructor Studio CLI is available even when generated runtime
# files are ignored locally or absent in a clean checkout.
cfs-ensure:
	@if ! command -v $(CFS) >/dev/null 2>&1; then \
		echo "cfs not found; installing $(CFS_PIPX_SPEC) via pipx"; \
		if ! command -v pipx >/dev/null 2>&1; then \
			echo "ERROR: pipx is required before running this target"; \
			exit 1; \
		else \
			pipx install $(CFS_PIPX_SPEC); \
		fi; \
	fi
	@if ! command -v $(CFS) >/dev/null 2>&1; then \
		echo "ERROR: cfs was installed but is not on PATH"; \
		exit 1; \
	fi

## Repair ignored/generated Constructor Studio runtime files before validation.
cfs-repair: cfs-ensure
	$(CFS) init --yes

## Check Constructor Studio spec-to-code traceability coverage.
cfs-spec-coverage: cfs-repair
	$(CFS) spec-coverage --min-coverage 80

## Validate registered Constructor Studio kits.
cfs-validate-kits: cfs-repair
	$(CFS) validate-kits

## Validate the local studio-kit-gears checkout as a kit directory.
cfs-validate-kit-local: cfs-repair
	cd studio-kit-gears && $(CFS) validate-kits .

# -------- API and docs --------

.PHONY: openapi md-fabric slides web-docs-preview

# Generate OpenAPI spec from running cf-gears-example-server
openapi:
	@command -v curl >/dev/null || (echo "curl is required to generate OpenAPI spec" && exit 1)
	@cargo build --bin cf-gears-example-server $(E2E_ARGS)
	@echo "Starting cf-gears-example-server to generate OpenAPI spec..."
	@$(call start_server_and_wait,target/debug/cf-gears-example-server --config config/quickstart.yaml,$(OPENAPI_URL),300) && \
	echo "Fetching OpenAPI spec..." && \
	mkdir -p $$(dirname "$(OPENAPI_OUT)") && \
	curl -fsS "$(OPENAPI_URL)" -o "$(OPENAPI_OUT)" && \
	echo "Sorting OpenAPI JSON for deterministic ordering..." && \
	$(PYTHON) tools/scripts/sort_openapi_json.py "$(OPENAPI_OUT)" && \
	echo "OpenAPI spec saved to $(OPENAPI_OUT)"

## Generate Markdown files map
md-fabric:
	$(PYTHON) ./tools/scripts/md-fabric.py --out docs/md-fabric/md-fabric.html

## Build the slides with Marp
slides:
	@command -v npx >/dev/null || (echo "npx is required to build slides. Install Node.js or run 'npm install' from the repo root." && exit 1)
	npx marp docs/slides/[0-9]*.md --theme-set docs/slides/css/slides.css --allow-local-files

# Preview the documentation website with local docs/web-docs content.
# Clones the web docs site into .web-docs-preview/ and serves it at localhost:4321.
web-docs-preview:
	@bash tools/scripts/docs-preview.sh

# -------- qa-platform UI --------

.PHONY: ui-install ui-lint ui-test ui-build ui-contract

UI_DIR := gears/qa-platform/qa-platform-ui

## Install UI dependencies from the lockfile
ui-install:
	@command -v npm >/dev/null || (echo "npm is required for the qa-platform UI" && exit 1)
	cd $(UI_DIR) && npm ci

## Lint the UI
ui-lint: ui-install
	cd $(UI_DIR) && npm run lint

## Run the UI unit tests (vitest). Added with the first test suite in this project
## (Task 10's `src/api/adapters.test.ts`): a suite reachable only through `npx vitest` is
## a suite no later task and no CI job will run, and `ui-lint`/`ui-build` are already the
## vocabulary this repo gates the UI with. Kept separate from `ui-build` rather than made
## a prerequisite, so a type error and a failing assertion stay distinguishable.
ui-test: ui-install
	cd $(UI_DIR) && npm test

## Type-check and build the UI
ui-build: ui-install
	cd $(UI_DIR) && npm run build

## Regenerate UI types from a running gears stack and fail if they drift. Requires the compose
## stack up on localhost:8087 (`docker compose up -d` in gears/qa-platform/deploy/compose) — it
## hits a live /openapi.json, so it is not wired into a CI job that has no gears.
ui-contract: ui-install
	cd $(UI_DIR) && npm run gen:api
	git diff HEAD --exit-code -- $(UI_DIR)/src/api/generated/openapi.d.ts \
	  || (echo "UI wire types are stale: regenerate with 'make ui-contract' and commit" && exit 1)

# -------- Development and auto fix --------

.PHONY: dev dev-fmt dev-clippy dev-test

## Run tests in development mode
dev-test: install-tools
	cargo nextest run --workspace

## Auto-fix code formatting
dev-fmt:
	cargo fmt --all

## Auto-fix clippy warnings
dev-clippy:
	cargo clippy --workspace --all-targets --all-features --fix --allow-dirty

# Auto-fix formatting and clippy warnings
dev: dev-fmt dev-clippy dev-test

# -------- Tests --------

.PHONY: test test-no-macros test-macros test-sqlite test-pg test-mysql test-db test-users-info-pg test-usage-collector-pg test-cluster-pg test-fips test-qa-runs-pg test-qa-insights-pg test-qa-catalog-git test-qa-platform-features

# Run all tests
test: install-tools
	cargo nextest run --workspace

test-no-macros: install-tools
	cargo nextest run --workspace --exclude cf-gears-toolkit-macros-tests --exclude cf-gears-toolkit-db-macros

test-macros: install-tools
	cargo nextest run -p cf-gears-toolkit-db-macros
	cargo nextest run -p cf-gears-toolkit-macros-tests

## Run SQLite integration tests
test-sqlite: install-tools
	cargo nextest run -p cf-gears-toolkit-db --features sqlite,integration,preview-outbox
	cargo build -p cf-gears-toolkit-db --examples --features sqlite,preview-outbox

## Run PostgreSQL integration tests
test-pg: install-tools
	cargo nextest run -p cf-gears-toolkit-db --features pg,integration,preview-outbox

## Run MySQL integration tests
test-mysql: install-tools
	cargo nextest run -p cf-gears-toolkit-db --features mysql,integration,preview-outbox

# Run all database integration tests
test-db: test-sqlite test-pg test-mysql

## Run users-info gear integration tests
test-users-info-pg: install-tools
	cargo nextest run -p users-info --features "integration"

## Run TimescaleDB usage-collector plugin integration tests (Docker required;
## the suite spins up its own timescale/timescaledb container via testcontainers)
test-usage-collector-pg: install-tools
	cargo nextest run -p cf-gears-timescaledb-usage-collector-plugin --features postgres

## Run the Postgres cluster plugin's conformance (Layer 2) and Layer 3
## integration suites (Docker required;
## each spins up its own postgres container per test via testcontainers —
## see gears/system/cluster/plugins/postgres-cluster-plugin/docs/TESTING.md §7).
##
## `--retries 1` because the container/pool *setup* in tests/common/mod.rs is
## load-sensitive on a busy host: it already retries `Postgres::start()` itself,
## and exhausting that budget surfaces as a failure in whichever test drew the
## short straw. A genuine logic regression fails both attempts, so this absorbs
## Docker churn without masking one.
test-cluster-pg: install-tools
	cargo nextest run -p cf-postgres-cluster-plugin --features integration --retries 1

## Run the qa-runs gear's real-Postgres suite (Docker required; the fixture
## spins up its own postgres container per test via testcontainers -- see
## `infra::storage::test_db::pg_db`).
##
## These are the only tests that can falsify Task 16b step 1's SERIALIZABLE
## escalation and step 3's counter repair. The rest of the gear's tier is
## in-memory SQLite, which admits one writer at a time, so the races they cover
## cannot occur there with a fix or without one -- which is exactly why this
## target has to exist rather than relying on `make test`.
##
## `--retries 1` for the reason `test-cluster-pg` has it, arrived at the same
## way: one failure of this target was observed on a busy host and could not be
## reproduced in eighteen subsequent runs, so which test failed is not known.
## Every test here spins up its own container and the contended ones assert
## outcomes that depend on a bounded retry budget absorbing an abort, so both the
## container setup and the assertions are load-sensitive. A genuine logic
## regression fails both attempts; this absorbs a busy host without masking one.
## An earlier round decided against this on the grounds that most sibling targets
## omit it; that was decided before a failure had been seen.
test-qa-runs-pg: install-tools
	cargo nextest run -p qa-runs --features integration --retries 1

## Run qa-insights' real-Postgres integration tier.
## Five of these are dialect guards over the analytics reads: Postgres refuses a
## selected column that is not grouped or aggregated, where SQLite picks an
## arbitrary row and says nothing — so a grouping key dropped from
## `grouped_status_counts` passes the SQLite tier and fails at runtime on a
## deployment. The gear ships to Postgres in the Helm chart, so this is the tier
## that can falsify that change.
## `--lib` because the tests need `pub(crate)` services and `#[cfg(test)]`
## fixtures, exactly as qa-runs' do.
test-qa-insights-pg: install-tools
	cargo nextest run -p qa-insights --features integration --lib --retries 1

## Run qa-catalog's real-git-transport integration tier.
## `tests/multi_branch.rs` (4) and `tests/gix_sync_integration.rs` (2) are the
## only coverage of the real git transport, the multi-branch snapshot layout,
## and concurrent syncs through the two-tier locks. They clone a local fixture
## repo through the real transport, which spawns `git upload-pack`, so a `git`
## binary must be on PATH. These live in `tests/`, not in-lib, so the flag is
## `--tests` (all test-target files), not `--lib`. Not `--test`, which is
## singular and takes one target name — it would run only one of the two files.
test-qa-catalog-git: install-tools
	@command -v git >/dev/null || (echo "git is required for test-qa-catalog-git" && exit 1)
	cargo nextest run -p qa-catalog --features integration --tests

## Run the unit tier of the two feature-gated adapters the shipped image
## enables (`deploy/cargo-features.argo`: runner-secret, qa-runs-argo).
## `make test-no-macros` is `cargo nextest run --workspace` with per-crate
## DEFAULT features, so `#[cfg(feature = "argo")]` and
## `#[cfg(feature = "runner-secret")]` code is neither compiled nor run there.
## 39 tests under qa-runs::infra::executor::argo and 13 under
## qa-environments' runner-secret writer are what this adds.
## `--lib` only: `qa-runs/tests/argo_cluster.rs`'s 4 are `#[ignore]`d and want
## a live cluster.
test-qa-platform-features: install-tools
	cargo nextest run -p qa-runs --features argo --lib
	cargo nextest run -p qa-environments --features runner-secret --lib

.PHONY: helm-tests

## Run the qa-platform Helm chart guards. No cluster needed -- these are
## `helm template` plus file reads -- so the `lint` job holds them.
## Includes `test_no_system_gear_changes.py`, the guard FOOTPRINT names for the
## reverted system-gear changes, and `test_nginx_template.sh`, which is what
## proves the SSE access-log redaction.
##
## Only test_no_system_gear_changes.py is pytest-shaped (it defines a
## `test_*` function); `test_chart_file_sync.py`, `test_features.py`,
## `test_no_environment_hardcode.py` and `test_pins.py` are standalone scripts
## -- a `main()` run via `if __name__ == "__main__"` -- so
## `python3 -m pytest tests/` collects zero items from them and would silently
## skip four of the six guards. Each is invoked directly so all six actually
## run; a non-zero exit from any of them fails this target.
helm-tests:
	@command -v helm >/dev/null || (echo "helm is required for helm-tests" && exit 1)
	cd gears/qa-platform/deploy/helm && python3 -m pytest tests/ -q
	python3 gears/qa-platform/deploy/helm/tests/test_chart_file_sync.py
	python3 gears/qa-platform/deploy/helm/tests/test_features.py
	python3 gears/qa-platform/deploy/helm/tests/test_no_environment_hardcode.py
	python3 gears/qa-platform/deploy/helm/tests/test_pins.py
	bash gears/qa-platform/deploy/helm/tests/test_nginx_template.sh

## Run FIPS-mode integration tests (requires Go for aws-lc-fips-sys).
## Covers:
##   - cf-gears-toolkit         : bootstrap + init_crypto_provider dispatch
##   - cf-gears-toolkit-http    : TLS client fail-closed path (NoCryptoProvider,
##                                apply_fips_hardening, builder/client FIPS-feature
##                                test surface). See issue #1935.
##   - cf-gears-oagw           : startup validation rejects allow_http_upstream
##                                under --features fips (PR #1985).
##
## Per-package `pkg/feat` syntax is required because `bootstrap` exists only
## on `cf-gears-toolkit` and the crates have independent FIPS feature
## spaces (toolkit doesn't depend on toolkit-http; oagw forwards toolkit-http/fips
## via its own `fips` feature). Single invocation so the shared FIPS dep graph
## compiles once.
test-fips: install-tools
	cargo nextest run -p cf-gears-toolkit -p cf-gears-toolkit-http -p cf-gears-oagw \
		--features cf-gears-toolkit/bootstrap,cf-gears-toolkit/fips,cf-gears-toolkit-http/fips,cf-gears-oagw/fips

## Cross-compile gate for the Windows+FIPS path (Windows handshake
## verification is the manual runbook in cf-gears-fips-probe/README.md). Catches
## type / cfg / feature-graph regressions for `rustls-cng-crypto` and the
## dep-graph (`rustls-cng-crypto` present, `aws-lc-fips-sys` absent).
##
## Uses `cargo-xwin` because the workspace pulls transitive C deps
## (`aws-lc-sys`, `libz-ng-sys`) whose build.rs scripts need a Windows
## sysroot (`windows.h`, MSVC headers). `cargo-xwin` downloads the MSVC
## redistributable and CRT/Windows headers automatically — works on Linux,
## macOS, and Windows hosts without a pre-installed Visual Studio.
##
## Prerequisites:
##   cargo install cargo-xwin    # MSVC sysroot bridge
##   # And a cmake build driver for the cmake-based transitive C deps:
##   # macOS:  brew install ninja
##   # Linux:  apt-get install ninja-build  (or: dnf install ninja-build)
##
## Pair this with the dep-graph regression check, which needs no toolchain
## at all and runs on any host:
##   cargo tree --target x86_64-pc-windows-msvc -p cf-gears-example-server \
##       --features fips -e features | grep aws-lc-fips    # must be empty
.PHONY: check-windows-fips
check-windows-fips:
	$(call check_tool,cargo-xwin)
	$(call check_tool,ninja)
	rustup target add x86_64-pc-windows-msvc
	cargo xwin check --target x86_64-pc-windows-msvc -p cf-gears-example-server --features fips

# -------- Benchmarks --------

.PHONY: bench-pg bench-pg-profiler bench-mysql bench-mariadb bench-sqlite bench-db \
       bench-pg-longhaul bench-mysql-longhaul bench-mariadb-longhaul bench-sqlite-longhaul bench-db-longhaul

## Run outbox throughput benchmarks against PostgreSQL
bench-pg:
	cargo bench -p cf-gears-toolkit-db --features pg,preview-outbox --bench outbox_throughput -- postgres

## Run outbox throughput benchmarks against MySQL
bench-mysql:
	cargo bench -p cf-gears-toolkit-db --features mysql,preview-outbox --bench outbox_throughput -- mysql

## Run outbox throughput benchmarks against MariaDB
bench-mariadb:
	cargo bench -p cf-gears-toolkit-db --features mysql,preview-outbox --bench outbox_throughput -- mariadb

# Run outbox throughput benchmarks against SQLite
bench-sqlite:
	cargo bench -p cf-gears-toolkit-db --features sqlite,preview-outbox --bench outbox_throughput -- sqlite

## Run outbox throughput benchmarks against all database engines
bench-db: bench-pg bench-mysql bench-mariadb bench-sqlite

## Run long-haul (1M+10M) outbox benchmarks against PostgreSQL
bench-pg-longhaul:
	cargo bench -p cf-gears-toolkit-db --features pg,preview-outbox --bench outbox_throughput -- postgres_longhaul

## Run long-haul (1M+10M) outbox benchmarks against MySQL
bench-mysql-longhaul:
	cargo bench -p cf-gears-toolkit-db --features mysql,preview-outbox --bench outbox_throughput -- mysql_longhaul

## Run long-haul (1M+10M) outbox benchmarks against MariaDB
bench-mariadb-longhaul:
	cargo bench -p cf-gears-toolkit-db --features mysql,preview-outbox --bench outbox_throughput -- mariadb_longhaul

## Run long-haul (100K 1P) outbox benchmarks against SQLite
bench-sqlite-longhaul:
	cargo bench -p cf-gears-toolkit-db --features sqlite,preview-outbox --bench outbox_throughput -- sqlite_longhaul

## Run long-haul outbox benchmarks against all database engines
bench-db-longhaul: bench-pg-longhaul bench-mysql-longhaul bench-mariadb-longhaul bench-sqlite-longhaul

# -------- E2E tests --------

.PHONY: e2e e2e-local e2e-local-smoke e2e-mini-chat e2e-docker e2e-docker-smoke e2e-tr-authz e2e-usage-collector

E2E_TARGET ?=

# Run E2E tests in Docker (default)
e2e: e2e-docker

## Run E2E tests in Docker environment
e2e-docker:
	$(PYTHON) tools/scripts/ci.py e2e-docker -- $(E2E_TARGET)

## Run E2E smoke tests in Docker (only tests marked @pytest.mark.smoke)
e2e-docker-smoke:
	$(PYTHON) tools/scripts/ci.py e2e-docker -- -m smoke $(E2E_TARGET)

# Run E2E tests locally, use `make e2e-local E2E_TARGET=testing/e2e/gears/` to specify target
e2e-local:
	$(PYTHON) tools/scripts/ci.py e2e-local -- $(E2E_TARGET)

## Run RG + AuthZ barrier E2E tests with tr-authz-plugin going through TR -> RG
e2e-tr-authz:
	$(PYTHON) tools/scripts/ci.py e2e-local \
		--config config/e2e-tr-authz.yaml \
		-- -k "resource_group"

## Run E2E smoke tests locally (only tests marked @pytest.mark.smoke)
e2e-local-smoke:
	$(PYTHON) tools/scripts/ci.py e2e-local --smoke

MINI_CHAT_FEATURES = mini-chat,static-authn,static-authz,single-tenant,static-credstore
MINI_CHAT_K8S_FEATURES = $(MINI_CHAT_FEATURES),k8s

MINI_CHAT_IMAGE ?= cf-gears-mini-chat
MINI_CHAT_TAG   ?= $(shell git rev-parse --short HEAD 2>/dev/null || echo latest)

## Run mini-chat E2E tests (separate binary with mini-chat features)
e2e-mini-chat:
	cargo build --bin cf-gears-example-server --features=$(MINI_CHAT_FEATURES)
	E2E_BINARY=target/debug/cf-gears-example-server \
		$(PYTHON) -m pytest testing/e2e/gears/mini_chat/ --mode offline -vv

UC_E2E_FEATURES = usage-collector,timescaledb-usage-collector,static-tenants,static-authn,static-authz

## Run usage-collector E2E tests (dedicated binary + TimescaleDB container; Docker required)
e2e-usage-collector:
	cargo build --bin cf-gears-example-server --features=$(UC_E2E_FEATURES)
	E2E_BINARY=target/debug/cf-gears-example-server \
		$(PYTHON) -m pytest testing/e2e/gears/usage_collector/ -vv

# -------- Code coverage --------

.PHONY: coverage coverage-unit coverage-e2e-local check-prereq-e2e-local

# Generate code coverage report (unit + e2e-local tests)
coverage:
	$(call check_tool,cargo-llvm-cov)
	$(PYTHON) tools/scripts/coverage.py combined

# Generate code coverage report (unit tests only)
coverage-unit:
	$(call check_tool,cargo-llvm-cov)
	$(PYTHON) tools/scripts/coverage.py unit

## Ensure needed packages and programs installed for local e2e testing
check-prereq-e2e-local:
	$(PYTHON) tools/scripts/check_local_env.py --mode e2e-local

# Generate code coverage report (e2e-local tests only)
coverage-e2e-local: check-prereq-e2e-local
	$(call check_tool,cargo-llvm-cov)
	$(PYTHON) tools/scripts/coverage.py e2e-local

# -------- Fuzzing --------

.PHONY: fuzz fuzz-build fuzz-list fuzz-run fuzz-clean fuzz-corpus

## Check cargo-fuzz is installed (required for fuzzing)
fuzz-install:
	$(call check_tool,cargo-fuzz)

## Build all fuzz targets
fuzz-build: fuzz-install
	cargo +nightly fuzz build --fuzz-dir tools/fuzz

## List all available fuzz targets
fuzz-list: fuzz-install
	cargo +nightly fuzz list --fuzz-dir tools/fuzz

## Run a specific fuzz target (use FUZZ_TARGET=name)
## Example: make fuzz-run FUZZ_TARGET=fuzz_odata_filter FUZZ_SECONDS=60
fuzz-run: fuzz-install
	@if [ -z "$(FUZZ_TARGET)" ]; then \
		echo "ERROR: FUZZ_TARGET is required. Example: make fuzz-run FUZZ_TARGET=fuzz_odata_filter"; \
		exit 1; \
	fi
	cargo +nightly fuzz run --fuzz-dir tools/fuzz $(FUZZ_TARGET) -- -max_total_time=$(or $(FUZZ_SECONDS),60)

# Run all fuzz targets for a short time (smoke test)
fuzz: fuzz-build
	@echo "Running all fuzz targets for 30 seconds each..."
	@FAILED=0; \
	for target in $$(cargo +nightly fuzz list --fuzz-dir tools/fuzz); do \
		echo "=== Fuzzing $$target ==="; \
		cargo +nightly fuzz run --fuzz-dir tools/fuzz $$target -- -max_total_time=30 || FAILED=1; \
	done; \
	if [ $$FAILED -ne 0 ]; then \
		echo "Fuzzing found crashes. Check tools/fuzz/artifacts/ for details."; \
		exit 1; \
	fi
	@echo "Fuzzing complete. No crashes found."

## Clean fuzzing artifacts and corpus
fuzz-clean:
	rm -rf tools/fuzz/artifacts/
	rm -rf tools/fuzz/corpus/*/
	rm -rf tools/fuzz/target/

## Minimize corpus for a specific target
fuzz-corpus: fuzz-install
	@if [ -z "$(FUZZ_TARGET)" ]; then \
		echo "ERROR: FUZZ_TARGET is required. Example: make fuzz-corpus FUZZ_TARGET=fuzz_odata_filter"; \
		exit 1; \
	fi
	cargo +nightly fuzz cmin --fuzz-dir tools/fuzz $(FUZZ_TARGET)

# -------- Mini chat --------

# mini-chat targets are for running the mini-chat gear locally and in Kubernetes, with options for building Docker images and deploying with Helm.

.PHONY: mini-chat mini-chat-docker mini-chat-helm mini-chat-helm-template mini-chat-up mini-chat-down mini-chat-port-forward

# Run server with mini-chat gear
mini-chat:
	cargo run --bin cf-gears-example-server --features mini-chat,static-authn,static-authz,single-tenant,static-credstore,otel -- --config config/mini-chat.yaml run

## Build mini-chat Docker image for K8s (dev build by default, RELEASE=1 for optimized)
## On linux: builds on host (reuses local target/), then packages the binary.
## On other OS: full multi-stage Docker build with BuildKit caching.
MINI_CHAT_PROFILE = $(if $(RELEASE),release,dev)
MINI_CHAT_CARGO_RELEASE_FLAG = $(if $(RELEASE),--release,)
MINI_CHAT_TARGET_DIR = $(or $(CARGO_TARGET_DIR),target)/$(if $(RELEASE),release,debug)

mini-chat-docker:
ifeq ($(shell uname -s),Linux)
	@echo "==> Linux host: building on host, packaging into image"
	cargo build $(MINI_CHAT_CARGO_RELEASE_FLAG) --bin cf-gears-example-server --package=cf-gears-example-server \
		--features "$(MINI_CHAT_K8S_FEATURES)"
	@mkdir -p .docker-stage
	@cp $(MINI_CHAT_TARGET_DIR)/cf-gears-example-server .docker-stage/cf-gears-example-server
	DOCKER_BUILDKIT=1 docker build \
		-f gears/mini-chat/deploy/docker/mini-chat-prebuilt.Dockerfile \
		--build-arg BINARY_PATH=".docker-stage/cf-gears-example-server" \
		-t $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) .
	@rm -rf .docker-stage
else
	@echo "==> Non-linux host: full Docker build"
	DOCKER_BUILDKIT=1 docker build \
		-f gears/mini-chat/deploy/docker/mini-chat.Dockerfile \
		--build-arg CARGO_FEATURES="$(MINI_CHAT_K8S_FEATURES)" \
		--build-arg BUILD_PROFILE="$(MINI_CHAT_PROFILE)" \
		-t $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) .
endif

## Deploy mini-chat Helm chart to local K8s cluster (build + load + install)
mini-chat-helm: mini-chat-docker
	@if command -v k3s >/dev/null 2>&1; then \
		docker save $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) | sudo k3s ctr images import -; \
	elif command -v minikube >/dev/null 2>&1; then \
		minikube ssh "docker rmi -f $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) 2>/dev/null" || true; \
		minikube image load $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG); \
	else \
		echo "ERROR: k3s or minikube required"; exit 1; \
	fi
	helm upgrade --install mini-chat gears/mini-chat/deploy/helm/mini-chat/ \
		--set image.tag="$(MINI_CHAT_TAG)" \
		--set secrets.azureOpenaiApiKey="$${AZURE_OPENAI_API_KEY}" \
		--set secrets.azureOpenaiApiHost="$${AZURE_OPENAI_API_HOST}" \
		--set postgres.host="$${PG_HOST:-postgres.default.svc.cluster.local}" \
		--set postgres.password="$${PG_PASSWORD}"
	kubectl rollout restart deployment/mini-chat
	kubectl rollout status deployment/mini-chat --timeout=120s

## Render mini-chat Helm templates (dry-run)
mini-chat-helm-template:
	helm template mini-chat gears/mini-chat/deploy/helm/mini-chat/

## One-command: ensure minikube is up, deploy latest chart, port-forward
## Usage: make mini-chat-up
## If image was rebuilt (make mini-chat-docker), re-run this to pick it up.
mini-chat-up:
	@# --- 1. Ensure cluster is running ---
	@if command -v minikube >/dev/null 2>&1; then \
		STATUS=$$(minikube status -f '{{.Host}}' 2>/dev/null || true); \
		if [ "$$STATUS" != "Running" ]; then \
			echo "Starting minikube..."; \
			minikube start; \
		fi; \
	elif command -v k3s >/dev/null 2>&1; then \
		: ; \
	else \
		echo "ERROR: minikube or k3s required"; exit 1; \
	fi
	@# --- 2. Load latest image if it exists locally ---
	@if docker image inspect $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) >/dev/null 2>&1; then \
		echo "Loading image $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) into cluster..."; \
		if command -v minikube >/dev/null 2>&1; then \
			minikube ssh "docker rmi -f $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) 2>/dev/null" || true; \
			minikube image load $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG); \
		else \
			docker save $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) | sudo k3s ctr images import -; \
		fi; \
	else \
		echo "No local image found. Run 'make mini-chat-docker' first to build."; \
		exit 1; \
	fi
	@# --- 3. Helm install/upgrade ---
	@if [ -z "$${AZURE_OPENAI_API_KEY}" ] || [ -z "$${AZURE_OPENAI_API_HOST}" ]; then \
		echo "WARNING: AZURE_OPENAI_API_KEY or AZURE_OPENAI_API_HOST not set."; \
		echo "  export AZURE_OPENAI_API_KEY=... AZURE_OPENAI_API_HOST=..."; \
	fi
	helm upgrade --install mini-chat gears/mini-chat/deploy/helm/mini-chat/ \
		--set image.tag="$(MINI_CHAT_TAG)" \
		--set secrets.azureOpenaiApiKey="$${AZURE_OPENAI_API_KEY}" \
		--set secrets.azureOpenaiApiHost="$${AZURE_OPENAI_API_HOST}" \
		--set postgres.host="$${PG_HOST:-postgres.default.svc.cluster.local}" \
		--set postgres.password="$${PG_PASSWORD}"
	kubectl rollout restart deployment/mini-chat
	kubectl rollout status deployment/mini-chat --timeout=120s
	@echo ""
	@echo "mini-chat is running. In a separate terminal run:"
	@echo "  make mini-chat-port-forward"
	@echo "Then access: http://localhost:8087/cf/mini-chat"

## Persistent port-forward with auto-reconnect (run in a separate terminal)
mini-chat-port-forward:
	@echo "Port-forward: localhost:8087 -> svc/mini-chat:8087 (auto-reconnect, Ctrl+C to stop)"
	@while true; do \
		kubectl port-forward svc/mini-chat 8087:8087 2>&1 || true; \
		echo "connection lost, reconnecting in 2s..."; \
		sleep 2; \
	done

## Tear down mini-chat from the cluster
mini-chat-down:
	helm uninstall mini-chat 2>/dev/null || true
	@echo "mini-chat uninstalled"

# -------- Main targets --------

.PHONY: all check ci ci_test ci_docs build .cargo-build .split-debug quickstart example mini-chat mini-chat-docker mini-chat-helm mini-chat-helm-template mini-chat-up mini-chat-down mini-chat-port-forward

# Start server with quickstart config
quickstart:
	mkdir -p data
	cargo run --bin cf-gears-example-server -- --config config/quickstart.yaml run

# Run server with example gear
example:
	cargo run --bin cf-gears-example-server $(E2E_ARGS) -- --config config/quickstart.yaml run

## Run server with fips gear
fips:
	cargo run --bin cf-gears-example-server --features fips,static-authn,static-authz,single-tenant,static-credstore,otel -- --config config/quickstart.yaml run

## Run server with out-of-process example gear
oop-example:
	cargo build -p calculator --features oop_gear
	cargo run --bin cf-gears-example-server --features oop-example,users-info-example,static-authn,static-authz,static-tenants,static-credstore -- --config config/quickstart.yaml run

# Run all quality checks
check: .setup-stamp fmt cfs-validate clippy lychee security dylint gts-docs test

ci_test: fmt clippy

ci_docs: lychee gts-docs

# Run CI pipeline locally, requires docker
ci: fmt clippy test-no-macros test-macros test-db deny test-users-info-pg test-usage-collector-pg test-qa-runs-pg test-qa-insights-pg test-qa-catalog-git test-qa-platform-features lychee gts-docs dylint helm-tests

## Build the cf-gears-example-server release binary using a toolchain from the rust-toolchain.toml
.cargo-build:
	cargo build --release --bin cf-gears-example-server $(E2E_ARGS)

## Split debug symbols into separate artifact(s) and strip the binary.
## Requires platform tools: objcopy (Linux), dsymutil+strip (macOS).
## On Windows MSVC the PDB is already separate; no extra tools needed.
.split-debug:
	cargo xtask split-debug cf-gears-example-server

# Build the release binary, then split debug symbols.
# Use 'make cargo-build' if you don't need stripped artifacts or lack
# platform debug-splitting tools (objcopy, dsymutil).
build: .cargo-build .split-debug

# Run all necessary quality checks and tests and then build the release binary
all: build check test-sqlite e2e-local openapi
	@echo ""
	@echo "============================================================="
	@echo "  CONGRATULATIONS! All 'make all' tasks have been completed!"
	@echo ""
	@echo "  Next suggestions:"
	@echo "    - make test-db        # run full DB integration tests"
	@echo "    - make mini-chat-up   # deploy and try the mini-chat demo"
	@echo ""
	@echo "  Tip: run 'git status' to inspect changes."
	@echo "============================================================="
