#!/usr/bin/env bash
# Renders a deploy-time copy of ./keycloak with one extra browser origin added
# to the `qa-platform-ui` client, for a stack the browser does not reach at
# `http://localhost:8080`. Point the compose file's KEYCLOAK_IMPORT_DIR at the
# output.
#
# WHY THIS SCRIPT EXISTS AT ALL, i.e. why the realm cannot be parameterised in
# place the way KC_HOSTNAME and the gears' issuer_pattern are:
#
#   * The client's `redirectUris` and `webOrigins` are literal
#     `http://localhost:8080` strings, and Keycloak will refuse the browser's
#     redirect if the origin it is redirected back to is not listed.
#   * Keycloak 26.0.8 does NOT expand `${env.VAR}` placeholders in a realm
#     import file. Measured on this exact image: a realm carrying
#     `"http://${env.PH_TEST_HOST}:8080/*"` in `redirectUris` aborts startup
#     with `ERROR: Invalid client t: A redirect URI is not a valid URI`. So the
#     obvious one-line fix -- a placeholder in the committed file -- is not a
#     fix, it is a stack that will not boot.
#   * The realm file cannot carry the target host as a second literal entry
#     either, because the target host is not known when the file is committed.
#
# That leaves generating a copy, which is what this does. It is a separate
# script rather than a one-shot compose service on the `keycloak-certs` model
# because realm import is a ONCE-PER-STACK event -- Keycloak reads
# /opt/keycloak/data/import only on a first start, against an empty database --
# so a service that re-rendered on every `up` would be doing nothing on every
# run but the first. The corollary is the trap: CHANGING THE REALM (or this
# script's output) DOES NOT AFFECT A STACK THAT HAS ALREADY STARTED KEYCLOAK
# ONCE. On a stack with an existing keycloak volume the extra origin has to be
# added through the admin API/console instead.
#
# `webOrigins` gets the concrete origin, never `"*"`: `"*"` tells Keycloak to
# send `Access-Control-Allow-Origin: *` for this client, which turns a
# deliberate two-host allowance into an any-host one.
#
# A WHOLE ORIGIN, NOT A HOST, and that is this round's change to the interface.
# An https deploy serves the UI on 443, and 443 is the DEFAULT port for https --
# so the URL is `https://10.136.20.200` with no port, and the browser's `Origin`
# header carries no port either. A host-plus-fixed-`:8080` interface cannot
# express that, and a `webOrigins` entry of `https://10.136.20.200:443` does not
# match an `Origin:` of `https://10.136.20.200`: Keycloak compares them as
# strings, so the CORS preflight on the token request fails and the login dies
# after the redirect, at the point that looks least like an origin problem.
#
# THE TENANT UUID IS ALSO RENDERED HERE, and that is this script's second job.
# The realm carries the tenant id in THREE places -- `admin`'s and `viewer`'s
# `tenant_id` user attribute, and the `qa-platform-workflow` client's hardcoded
# `tenant_id` claim -- and it has to equal seed-tenant.sh's `SEED_TENANT_ID`
# (which defaults to the same literal) and
# `toolkit_security::constants::DEFAULT_TENANT_ID`, or requests resolve against
# a tenant that owns nothing. Rather than let a deployment that moves the tenant
# hand-edit three JSON strings, this script substitutes all of them from
# $SEED_TENANT_ID -- the SAME variable name seed-tenant.sh reads, so one export
# moves both. Unset means the committed literal and a byte-identical copy.
#
# THE `qa-platform-workflow` CLIENT, and why a realm needs a machine account at
# all: a workflow pod downloading a test bundle calls
# `GET /qa/v1/test-bundles/{id}`, which is `.authenticated()`, so it needs a
# token of its own. It is a CONFIDENTIAL client with `serviceAccountsEnabled`,
# the `qa-platform-api` audience the gears require, and a HARDCODED `tenant_id`
# claim -- hardcoded because a service account has no user profile to read an
# attribute from, and oidc-authn-plugin requires the claim to parse as a UUID
# (claim_mapper.rs:103). Its secret is a committed dev fixture, in the same
# category as this realm's admin/admin password: see
# gears/qa-platform/deploy/argo/provision-workflow-secret.sh, which is what
# copies it into the Kubernetes Secret a workflow references.
#
# Usage:
#   render-realm.sh [PUBLIC_UI_ORIGIN] [OUT_DIR]
#
#   PUBLIC_UI_ORIGIN  Origin the browser reaches the UI at -- scheme, host and
#                     port if it is not the scheme's default, no trailing slash
#                     and no path. Also readable from $PUBLIC_UI_ORIGIN, and
#                     failing that derived from $PUBLIC_HOST as
#                     `http://$PUBLIC_HOST:8080` (the same defaulting chain
#                     docker-compose.yml uses). Default
#                     `http://localhost:8080`, which makes this an exact copy
#                     (see below).
#   OUT_DIR           Where to write. Also readable from $OUT_DIR. Default
#                     `<this dir>/.generated/keycloak`, which is what the
#                     compose file's KEYCLOAK_IMPORT_DIR is documented against
#                     and what deploy/remote/sync.sh uses.
#
# With the default origin the output is a byte-identical copy and no entry
# is added. That is not just an optimisation: adding it would produce a
# duplicate `http://localhost:8080/*`, and while Keycloak 26.0.8 does accept
# duplicates (measured -- a realm with the same redirect URI twice imports and
# the server starts), a generated file that differs from the committed one for
# no reason is a file nobody can diff usefully.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="$SCRIPT_DIR/keycloak"
REALM_FILE="realm-qa-platform.json"

PUBLIC_UI_ORIGIN="${1:-${PUBLIC_UI_ORIGIN:-http://${PUBLIC_HOST:-localhost}:8080}}"
OUT_DIR="${2:-${OUT_DIR:-$SCRIPT_DIR/.generated/keycloak}}"

# The tenant the realm mints in every token. Same variable name and same
# default as seed-tenant.sh's, deliberately: they must agree, and a reader who
# has set one and not the other has a stack whose tokens name a tenant with no
# rows. DEFAULT_TENANT_UUID is the literal the committed realm file carries and
# is the substitution's SOURCE, not a second copy of the setting.
DEFAULT_TENANT_UUID="00000000-df51-5b42-9538-d2b56b7ee953"
SEED_TENANT_ID="${SEED_TENANT_ID:-$DEFAULT_TENANT_UUID}"
if [[ ! "$SEED_TENANT_ID" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]]; then
    echo "render-realm: SEED_TENANT_ID='$SEED_TENANT_ID' is not a UUID -- refusing to write it into a realm file. oidc-authn-plugin runs both \`sub\` and the tenant claim through Uuid::parse_str and answers Unauthorized when either fails, so a non-UUID here 401s every request in the stack." >&2
    exit 1
fi

# The same validation, and for the same reason, as
# deploy/docker/entrypoint.sh: this value is interpolated into a file that
# grants access. It is not a regex here, so the stakes are lower, but an origin
# containing a quote or a brace would produce invalid JSON and Keycloak would
# abort on first start -- after `up -d` has already reported success.
#
# The trailing-slash rejection is not pedantry: the redirect entry below is
# `$PUBLIC_UI_ORIGIN/*`, so `https://h/` would render `https://h//*` and
# Keycloak would refuse the browser's redirect_uri with every other pin correct.
if [[ ! "$PUBLIC_UI_ORIGIN" =~ ^https?://[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?$ ]]; then
    echo "render-realm: PUBLIC_UI_ORIGIN='$PUBLIC_UI_ORIGIN' is not a plain 'http(s)://host[:port]' origin (no trailing slash, no path; host must be a DNS name or IPv4 literal) -- refusing to write it into a realm file. It is the FIRST POSITIONAL ARGUMENT and it used to be a bare host: pass 'http://<host>:8080', not '<host>'." >&2
    exit 1
fi

if [[ ! -f "$SRC_DIR/$REALM_FILE" ]]; then
    echo "render-realm: '$SRC_DIR/$REALM_FILE' not found -- refusing to run" >&2
    exit 1
fi

# `require_anchor`, copied in spirit from deploy/docker/entrypoint.sh for the
# same reason: a substitution that silently matches nothing here produces a
# realm that imports cleanly and then refuses every login with "Invalid
# parameter: redirect_uri", which is several layers from the cause. Zero
# matches means the realm changed shape; more than one means the substitution
# target is ambiguous and this script would edit the wrong client.
require_anchor() {
    local pattern="$1" label="$2" count
    # `|| true`: `grep -c` exits 1 on zero matches, which under `set -e` would
    # abort the assignment before the diagnostic below could run.
    count="$(grep -c -E "$pattern" "$SRC_DIR/$REALM_FILE" || true)"
    if [[ ! "$count" =~ ^[0-9]+$ ]]; then
        echo "render-realm: could not count '$label' lines in $SRC_DIR/$REALM_FILE (grep failed unexpectedly) -- refusing to continue" >&2
        exit 1
    fi
    if [[ "$count" -ne 1 ]]; then
        echo "render-realm: expected exactly one '$label' line in $SRC_DIR/$REALM_FILE, found $count -- refusing to continue rather than write a realm whose redirect URIs are wrong" >&2
        exit 1
    fi
}
mkdir -p "$OUT_DIR"
# Only *.json, and only in OUT_DIR: Keycloak imports EVERY *.json it finds in
# the import directory, so a stale realm left behind by an earlier run with a
# different host would be imported alongside the current one. Nothing else in
# OUT_DIR is touched, so this cannot eat an unrelated directory a caller
# pointed OUT_DIR at by mistake.
rm -f "$OUT_DIR"/*.json

# Every *.json, not just the realm: the import directory is read wholesale, so
# a second realm file added later must travel with it. Only the one named above
# is rewritten.
cp "$SRC_DIR"/*.json "$OUT_DIR"/

# jq is required rather than optional-with-a-fallback, and it is checked HERE
# rather than beside the verification it serves: both the origin block and the
# tenant block below end in a jq read-back, and discovering jq is missing after
# a realm has been written would leave an unverified file in place.
# It is present on both hosts this script runs on (measured: /usr/bin/jq here,
# jq 1.6 on the remote), and a python3 fallback would be a second parsing path
# that no run ever exercises -- the shape of bug this file's sibling
# entrypoint.sh already carries a header comment about.
if ! command -v jq >/dev/null 2>&1; then
    echo "render-realm: jq not found -- refusing to report success on a realm file that has not been parsed. Install jq (dnf -y install jq)." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 1. The browser origin
# ---------------------------------------------------------------------------
if [[ "$PUBLIC_UI_ORIGIN" == "http://localhost:8080" ]]; then
    cmp -s "$SRC_DIR/$REALM_FILE" "$OUT_DIR/$REALM_FILE" || {
        echo "render-realm: copy of $REALM_FILE differs from the source with PUBLIC_UI_ORIGIN=http://localhost:8080 -- refusing to report success" >&2
        exit 1
    }
    echo "render-realm: PUBLIC_UI_ORIGIN=http://localhost:8080 -- no origin added (copy verified byte-identical with cmp)"
else
    REDIRECT_ANCHOR='^        "http://localhost:8080/\*"$'
    ORIGIN_ANCHOR='^        "http://localhost:8080"$'
    require_anchor "$REDIRECT_ANCHOR" 'redirectUris entry "http://localhost:8080/*"'
    require_anchor "$ORIGIN_ANCHOR" 'webOrigins entry "http://localhost:8080"'

    # Line-oriented, and it appends rather than replaces, because the localhost
    # entries must keep working: the same rendered realm serves a browser on the
    # Docker host (via http://localhost:8080) and a browser elsewhere (via
    # PUBLIC_UI_ORIGIN).
    #
    # `|` as the delimiter, not `/`: both sides are URLs. `\n` in the replacement
    # is a GNU sed extension; this script runs on the deploy host, which is Linux
    # in both the local and the remote case (CentOS Stream 9 on the remote), so
    # that is a dependency this script may take. The two-space-deeper indentation
    # of the appended line matches the array it joins.
    sed \
        -e "s|$REDIRECT_ANCHOR|        \"http://localhost:8080/*\",\n        \"${PUBLIC_UI_ORIGIN}/*\"|" \
        -e "s|$ORIGIN_ANCHOR|        \"http://localhost:8080\",\n        \"${PUBLIC_UI_ORIGIN}\"|" \
        "$SRC_DIR/$REALM_FILE" > "$OUT_DIR/$REALM_FILE"

    # POST-CONDITION, not a restatement of the sed above. The anchor checks prove
    # the input had the shape expected; this proves the OUTPUT is valid JSON whose
    # client really does list both origins. sed cannot know it produced JSON, and
    # an import failure otherwise surfaces minutes later inside Keycloak's startup
    # log, after `docker compose up -d` has already exited 0.
    expected_redirects="http://localhost:8080/* ${PUBLIC_UI_ORIGIN}/*"
    expected_origins="http://localhost:8080 ${PUBLIC_UI_ORIGIN}"

    actual_redirects="$(jq -r '.clients[] | select(.clientId=="qa-platform-ui") | .redirectUris | join(" ")' "$OUT_DIR/$REALM_FILE")"
    actual_origins="$(jq -r '.clients[] | select(.clientId=="qa-platform-ui") | .webOrigins | join(" ")' "$OUT_DIR/$REALM_FILE")"

    status=0
    if [[ "$actual_redirects" == "$expected_redirects" ]]; then
        echo "PASS: qa-platform-ui redirectUris in $OUT_DIR/$REALM_FILE are [$actual_redirects]"
    else
        echo "FAIL: qa-platform-ui redirectUris are [$actual_redirects], expected [$expected_redirects]" >&2
        status=1
    fi
    if [[ "$actual_origins" == "$expected_origins" ]]; then
        echo "PASS: qa-platform-ui webOrigins in $OUT_DIR/$REALM_FILE are [$actual_origins]"
    else
        echo "FAIL: qa-platform-ui webOrigins are [$actual_origins], expected [$expected_origins]" >&2
        status=1
    fi
    if [[ " $actual_origins " == *" * "* ]]; then
        echo "FAIL: webOrigins contains \"*\", which allows any origin -- that is never what this script should produce" >&2
        status=1
    fi

    if [[ "$status" -ne 0 ]]; then
        exit "$status"
    fi
fi

# ---------------------------------------------------------------------------
# 2. The tenant UUID
# ---------------------------------------------------------------------------
# Applied to the OUTPUT file, after the origin step, so it runs on both of that
# step's branches. A global substitution of the literal, not a per-site edit:
# every occurrence in this realm is the same setting, and a per-site edit would
# have to be updated whenever a user or a client is added -- silently leaving
# the new one on the old tenant.
#
# `grep -c` on the OUTPUT before substituting, so a realm that no longer
# carries the literal (renamed, hand-edited, or already rendered) is a refusal
# rather than a no-op: an unsubstituted tenant claim does not fail to import,
# it imports and then every token names a tenant with no rows -- which reads as
# an empty database, not as a configuration error.
tenant_sites="$(grep -c -F "\"$DEFAULT_TENANT_UUID\"" "$OUT_DIR/$REALM_FILE" || true)"
if [[ ! "$tenant_sites" =~ ^[0-9]+$ ]] || [[ "$tenant_sites" -lt 1 ]]; then
    echo "render-realm: $OUT_DIR/$REALM_FILE contains no \"$DEFAULT_TENANT_UUID\" occurrence to substitute (found '${tenant_sites:-<grep failed>}') -- refusing to continue rather than write a realm whose tenant claim is not the one seed-tenant.sh seeds" >&2
    exit 1
fi

if [[ "$SEED_TENANT_ID" != "$DEFAULT_TENANT_UUID" ]]; then
    # Both sides are hex-and-dashes (validated at the top), so no sed
    # metacharacter can appear in either and no escaping is needed -- unlike
    # entrypoint.sh's credentials, whose `escape_sed_replacement` exists for
    # exactly the characters a UUID cannot contain.
    sed -i "s|\"$DEFAULT_TENANT_UUID\"|\"$SEED_TENANT_ID\"|g" "$OUT_DIR/$REALM_FILE"
fi

# Read back through jq, per site, naming each one. `-e` makes an empty result
# a non-zero exit, so a client or user that stopped carrying the claim is
# caught rather than reported as an empty string that happens not to match.
tenant_status=0
check_tenant_site() {
    local label="$1" filter="$2" got
    got="$(jq -re "$filter" "$OUT_DIR/$REALM_FILE" 2>/dev/null || true)"
    if [[ "$got" == "$SEED_TENANT_ID" ]]; then
        echo "PASS: $label tenant claim in $OUT_DIR/$REALM_FILE is $got"
    else
        echo "FAIL: $label tenant claim is '${got:-<absent>}', expected '$SEED_TENANT_ID'" >&2
        tenant_status=1
    fi
}
check_tenant_site 'user admin' '.users[] | select(.username=="admin") | .attributes.tenant_id[0]'
check_tenant_site 'user viewer' '.users[] | select(.username=="viewer") | .attributes.tenant_id[0]'
check_tenant_site 'client qa-platform-workflow' \
    '.clients[] | select(.clientId=="qa-platform-workflow") | .protocolMappers[] | select(.name=="tenant-id") | .config["claim.value"]'
if [[ "$tenant_status" -ne 0 ]]; then
    exit "$tenant_status"
fi

# The workflow client's other two pins, read back for the same reason the
# origins are: a realm that imports with `serviceAccountsEnabled: false` or
# without the audience mapper produces a `client_credentials` grant that fails
# with `unauthorized_client`, or a token the gears reject for `aud` -- neither
# of which names this file.
wf_svc="$(jq -r '.clients[] | select(.clientId=="qa-platform-workflow") | .serviceAccountsEnabled' "$OUT_DIR/$REALM_FILE")"
wf_aud="$(jq -r '.clients[] | select(.clientId=="qa-platform-workflow") | .protocolMappers[] | select(.protocolMapper=="oidc-audience-mapper") | .config["included.custom.audience"]' "$OUT_DIR/$REALM_FILE")"
wf_public="$(jq -r '.clients[] | select(.clientId=="qa-platform-workflow") | .publicClient' "$OUT_DIR/$REALM_FILE")"
if [[ "$wf_svc" == "true" && "$wf_aud" == "qa-platform-api" && "$wf_public" == "false" ]]; then
    echo "PASS: qa-platform-workflow is a confidential service-account client with audience $wf_aud"
else
    echo "FAIL: qa-platform-workflow has serviceAccountsEnabled=$wf_svc publicClient=$wf_public audience=$wf_aud, wanted true/false/qa-platform-api" >&2
    exit 1
fi

echo "render-realm: wrote $OUT_DIR (PUBLIC_UI_ORIGIN=$PUBLIC_UI_ORIGIN, SEED_TENANT_ID=$SEED_TENANT_ID). Point KEYCLOAK_IMPORT_DIR at it BEFORE keycloak's first start."
