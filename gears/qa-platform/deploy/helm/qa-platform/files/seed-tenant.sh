#!/usr/bin/env bash
# Seed the one Resource Group row that lets this stack boot.
#
# WHY THIS EXISTS AT ALL, which is not obvious and is the whole point.
#
# Task 14 replaced single-tenant-tr-plugin with rg-tr-plugin (see the
# CARGO_FEATURES comment in gears/qa-platform/deploy/docker/qa-platform.Dockerfile
# and the `rg-tr-plugin` block in gears/qa-platform/config/qa-platform-stack.yaml).
# rg-tr-plugin answers every tenant query out of Resource Group rows: tenants
# are RG groups whose GTS type code starts with `TENANT_RG_TYPE_PATH`
# (gears/system/tenant-resolver/plugins/rg-tr-plugin/src/lib.rs:1-8), and its
# hierarchy walk filters on exactly that (its domain/service.rs:41-46). With no
# such row there is nothing to resolve.
#
# That would be merely inert if nothing asked at boot. Something does. `oagw`
# is a NON-optional dependency of cf-gears-example-server
# (apps/cf-gears-example-server/Cargo.toml, `api_egress`), so it is compiled
# into every build regardless of CARGO_FEATURES, and its `post_init` calls
# `TenantResolverClient::get_root_tenant` with a context built from
# `toolkit_security::constants::DEFAULT_TENANT_ID` and turns any error into an
# `anyhow!` (gears/system/oagw/oagw/src/gear.rs:242-252). `run_post_init_phase`
# propagates that with `?` (libs/toolkit/src/runtime/host_runtime.rs:371-377),
# so the boot ABORTS. Nothing listens on 8087, and the REST API that could
# create the missing row is the API of the server that just exited.
#
# Hence SQL rather than the obvious `curl` against
# `POST /resource-group/v1/groups`. That endpoint works fine -- it is how this
# row's exact shape was first produced and read back -- but it cannot be
# reached before the boot it is a precondition of. The ordering the compose
# file builds instead is:
#
#     postgres healthy
#       -> db-migrate      (the gears image, `migrate` subcommand: runs only
#                           pre_init + the DB migration phases, so the RG
#                           schema is created WITHOUT oagw's post_init ever
#                           running -- see libs/toolkit/src/bootstrap/run.rs:124
#                           and its `run_migration_phases` call)
#       -> tenant-seed     (this script)
#       -> gears           (boots once, cleanly)
#
# THE ID IS NOT ARBITRARY, and it is pinned for two independent reasons that
# must both hold:
#
#   1. It is the `tenant_id` claim in the tokens Keycloak issues for this realm
#      (keycloak/realm-qa-platform.json), which oidc-authn-plugin maps onto
#      `SecurityContext.subject_tenant_id`. The RG create path sets
#      `tenant_id = group.id` for a tenant-typed group
#      (gears/system/resource-group/resource-group/src/domain/group_service.rs:566-569),
#      so seeding the row under this id is what makes the tenant a request
#      resolves to and the tenant that exists as a platform object the same
#      tenant.
#
#   2. It is `toolkit_security::constants::DEFAULT_TENANT_ID`
#      (libs/toolkit-security/src/constants.rs:12) -- the value oagw's
#      bootstrap SecurityContext is built with, unconditionally, with no config
#      lever. So even if reason 1 were satisfied by some other id (by changing
#      the realm's claim instead of seeding this one), oagw would still ask for
#      DEFAULT_TENANT_ID at boot and still not find it. That is what rules out
#      "generate an id and point the realm at it" as an alternative.
#
# Keep this value, the realm's `tenant_id` user attribute, and
# DEFAULT_TENANT_ID in agreement. A mismatch does not announce itself: a
# well-formed but wrong UUID fails the same way a missing one does.
#
# IDEMPOTENT. Re-running against an already-seeded database is a no-op, so
# `docker compose up -d` on a surviving volume behaves the same as on a fresh
# one. It is also NON-DESTRUCTIVE: it never updates or deletes an existing
# row, so a real tenant tree grown through the API later is left alone.
#
# Usage: seed-tenant.sh
#
#   PGHOST / PGUSER / PGPASSWORD  Standard libpq variables, set by the compose
#                                 service. PGDATABASE is NOT used -- the
#                                 database name is fixed below, because this
#                                 script seeds exactly one gear's database and
#                                 pointing it at another would silently do
#                                 nothing.
#
#   SEED_TENANT_ID                Overridable, defaulting to the value above.
#                                 Present so a deployment that runs a different
#                                 realm can move all three values together
#                                 rather than patching this file.
#
#   SEED_TENANT_NAME              Display name for the row. Cosmetic; RG
#                                 enforces 1..255 characters.
set -euo pipefail

RG_DATABASE="resource_group"
TENANT_ID="${SEED_TENANT_ID:-00000000-df51-5b42-9538-d2b56b7ee953}"
TENANT_NAME="${SEED_TENANT_NAME:-QA Platform Root Tenant}"

# `gts.` prefix included: this is the fully-qualified GTS type path as it is
# stored and as `TENANT_RG_TYPE_PATH` expands
# (gears/system/resource-group/resource-group-sdk/src/gts.rs:89 --
# `gts_id!("cf.core.rg.type.v1~cf.core._.tenant.v1~")`). Verified by creating
# the type through `POST /types-registry/v1/types` and reading the stored
# `gts_type.schema_id` back out of Postgres.
TENANT_TYPE_CODE="gts.cf.core.rg.type.v1~cf.core._.tenant.v1~"

# `__can_be_root: true` is how the RG type table stores `can_be_root` -- there
# is no column for it; it lives inside the `metadata_schema` JSONB under that
# double-underscore key. Taken from the row `POST /types-registry/v1/types`
# with `"can_be_root": true` actually wrote, not from the migration source. It
# must be true: a root tenant has no parent, and RG rejects a parentless group
# whose type cannot be a root.
TENANT_TYPE_METADATA_SCHEMA='{"__can_be_root": true}'

psql_rg() {
    psql -v ON_ERROR_STOP=1 --no-psqlrc --dbname "$RG_DATABASE" "$@"
}

say() {
    printf 'seed-tenant: %s\n' "$*"
}

die() {
    printf 'seed-tenant: FATAL -- %s\n' "$*" >&2
    exit 1
}

# ---------------------------------------------------------------------------
# 0. The schema must already exist
# ---------------------------------------------------------------------------
# This script writes rows; it does not own the schema. The tables come from
# toolkit's migration phase, which the `db-migrate` service runs before this
# one. Checking explicitly turns an ordering regression into a message that
# names the cause, instead of three "relation does not exist" errors that read
# like a broken query.
for table in gts_type resource_group resource_group_closure; do
    exists=$(psql_rg -tAc \
        "SELECT to_regclass('public.${table}') IS NOT NULL") \
        || die "cannot query $RG_DATABASE on ${PGHOST:-<unset>} as ${PGUSER:-<unset>}"
    if [ "$exists" != "t" ]; then
        die "table '${table}' does not exist in the '${RG_DATABASE}' database.
  This script seeds rows into a schema it does not create. The schema is
  created by the gears' migration phase -- the compose 'db-migrate' service,
  which must complete before this one. Check that it ran and succeeded."
    fi
done
say "schema present in '$RG_DATABASE'"

# ---------------------------------------------------------------------------
# 1. The tenant GTS type
# ---------------------------------------------------------------------------
# `gts_type.id` is `generated always as identity`, so the id cannot be supplied
# and the group insert below has to look it up by `schema_id` rather than
# assume 1. `WHERE NOT EXISTS` rather than `ON CONFLICT`: it keeps the identity
# sequence from being consumed on every re-run, which `ON CONFLICT DO NOTHING`
# would do (Postgres evaluates the default before detecting the conflict).
psql_rg <<SQL
INSERT INTO gts_type (schema_id, metadata_schema)
SELECT '${TENANT_TYPE_CODE}'::gts_type_path, '${TENANT_TYPE_METADATA_SCHEMA}'::jsonb
WHERE NOT EXISTS (
    SELECT 1 FROM gts_type WHERE schema_id = '${TENANT_TYPE_CODE}'::gts_type_path
);
SQL
say "tenant GTS type present: $TENANT_TYPE_CODE"

# ---------------------------------------------------------------------------
# 2. The root tenant group, and 3. its closure self-row
# ---------------------------------------------------------------------------
# One transaction, because a group without its depth-0 closure row is worse
# than no group at all: `resolve_tenant` finds a tenant only via the
# `hierarchy/depth eq 0` row (rg-tr-plugin's domain/service.rs:110-124), so a
# half-seeded state would look exactly like an unseeded one while making the
# INSERT below think its work was done.
#
# `metadata` carries `status` and `self_managed` because that is where
# rg-tr-plugin reads tenant status and the isolation-barrier flag from
# (its lib.rs:6-7). `status: "active"` matters: a tenant filtered out by
# status is a tenant that does not resolve.
#
# `parent_id` is NULL -- this is the forest root, and `get_root_tenant` returns
# the context tenant itself when it has no ancestors
# (rg-tr-plugin's domain/client.rs:50-53).
psql_rg <<SQL
BEGIN;

INSERT INTO resource_group (id, parent_id, gts_type_id, name, metadata, tenant_id)
SELECT
    '${TENANT_ID}'::uuid,
    NULL,
    t.id,
    \$name\$${TENANT_NAME}\$name\$,
    '{"status": "active", "self_managed": false}'::jsonb,
    '${TENANT_ID}'::uuid
FROM gts_type t
WHERE t.schema_id = '${TENANT_TYPE_CODE}'::gts_type_path
ON CONFLICT (id) DO NOTHING;

INSERT INTO resource_group_closure (ancestor_id, descendant_id, depth)
VALUES ('${TENANT_ID}'::uuid, '${TENANT_ID}'::uuid, 0)
ON CONFLICT (ancestor_id, descendant_id) DO NOTHING;

COMMIT;
SQL

# ---------------------------------------------------------------------------
# 4. Read it back
# ---------------------------------------------------------------------------
# The gate, not a formality. Everything above is `DO NOTHING`-guarded, so a
# subtly wrong WHERE clause would insert nothing and exit 0. This asserts the
# three properties the boot actually depends on: the row exists under the
# expected id, its `tenant_id` equals its own id (the tenant-scope rule --
# group_service.rs:566-569 -- without which the request tenant and the RG
# tenant are different tenants that happen to share a database), and it is of
# the tenant type rg-tr-plugin filters on.
result=$(psql_rg -tAc "
    SELECT g.id || '|' || g.tenant_id || '|' || t.schema_id || '|' || c.depth
    FROM resource_group g
    JOIN gts_type t ON t.id = g.gts_type_id
    JOIN resource_group_closure c
        ON c.ancestor_id = g.id AND c.descendant_id = g.id
    WHERE g.id = '${TENANT_ID}'::uuid
") || die "verification query failed"

if [ -z "$result" ]; then
    die "after seeding, no tenant row with id '${TENANT_ID}' and a depth-0 closure row
  could be found. Nothing was inserted and nothing errored, which means a
  guard above matched when it should not have. The gears will not boot."
fi

IFS='|' read -r got_id got_tenant got_type got_depth <<<"$result"

[ "$got_id" = "$TENANT_ID" ] \
    || die "seeded row id is '$got_id', expected '$TENANT_ID'"
[ "$got_tenant" = "$TENANT_ID" ] \
    || die "seeded row's tenant_id is '$got_tenant' but its id is '$got_id'.
  A tenant-typed group must own its own tenant scope. Requests carrying
  tenant_id='$TENANT_ID' would resolve against a different tenant than this row."
[ "$got_type" = "$TENANT_TYPE_CODE" ] \
    || die "seeded row's type is '$got_type', expected '$TENANT_TYPE_CODE'.
  rg-tr-plugin filters on that exact code and would not see this row as a tenant."
[ "$got_depth" = "0" ] \
    || die "closure self-row depth is '$got_depth', expected 0"

say "verified root tenant: id=$got_id tenant_id=$got_tenant type=$got_type depth=$got_depth"
say "done"
