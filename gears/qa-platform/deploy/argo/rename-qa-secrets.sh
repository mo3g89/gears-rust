#!/usr/bin/env bash
# Migrate operator-provisioned runner Secrets from the pre-tenant name to the
# tenant-qualified one.
#
# WHAT CHANGED AND WHY THIS EXISTS. The Secret name used to be
# `sanitize(prefix + reference)` (the PRE-WS1 rule, `old_name` below). It
# briefly became `sanitize(prefix + tenant + "-" + reference)` truncated to
# 63 bytes, and THAT rule shipped a defect measured in production: with the
# default prefix and a full tenant, only 14 bytes of reference survive
# truncation, and two real credentials of one tenant sharing that many
# leading characters collapsed onto ONE Secret. The name is now
# `{readable}-{digest}` (`new_name` below), where `digest` is a fixed-width
# hash of the whole `(prefix, tenant, reference)` tuple and `readable` is
# whatever budget is left after it -- see `qa-runs`' `naming::secret_name`
# for the full construction and why a digest, not truncation alone, is what
# keeps two distinct tuples from colliding regardless of how long a common
# prefix they share. Every writer that self-heals -- `qa-environments`'
# `ensure_runner_secret`, on create, update, and every observation-ticker
# cycle -- re-creates its Secret under `new_name` automatically the next
# time it runs. NOTHING TO DO THERE.
#
# This script exists ONLY for Secrets an operator created BY HAND with
# `provision-platform-kubeconfig-secret.sh` (or bare `kubectl create`),
# because nothing re-derives those on its own; they sit under the old name
# until an operator (or this script) moves them.
#
# WHAT IT NEEDS AND CANNOT GUESS. The old name is a one-way, lossy hash of
# (prefix, reference): sanitize+truncate cannot be inverted, so this script
# cannot recover a Secret's reference or tenant by inspecting the Secret
# object alone. It needs a MAPPING, supplied by the operator, of every
# by-hand reference to the tenant it belongs to -- the same information the
# operator already had to know to run provision-platform-kubeconfig-secret.sh
# in the first place. A line this script cannot make sense of is REPORTED,
# never guessed at: a wrong tenant here is a run silently reading the wrong
# credential, which is the exact defect class this whole change exists to
# close.
#
# WHAT IT DOES NOT DO. It never deletes or modifies the original Secret --
# the old name keeps working (nothing stops qa-runs' executor deriving and
# mounting it under the old scheme until every caller has moved) so the
# migration is reversible by simply not switching callers over, or by
# deleting the new Secret this script created. It is idempotent: re-running
# it after a partial run only creates what is still missing.
#
# Usage:
#   rename-qa-secrets.sh <mapping-file>
#
#   <mapping-file>  One credential per line: "tenant-id,credstore-ref".
#                   Blank lines and lines starting with '#' are ignored.
#                   Leading/trailing whitespace around each field is trimmed.
#
#   NAMESPACE       Default `argo`, matching `qa-runs.argo.namespace`.
#   SECRET_PREFIX   Default `qa-platform-`, matching
#                   `qa-runs.argo.secret_name_prefix`.
#   DRY_RUN         Set to any non-empty value to print what would happen
#                   without creating anything.
set -euo pipefail

MAPPING_FILE="${1:-}"
NAMESPACE="${NAMESPACE:-argo}"
SECRET_PREFIX="${SECRET_PREFIX:-qa-platform-}"

die() { echo "rename-qa-secrets: $*" >&2; exit 1; }

[[ -n "$MAPPING_FILE" ]] || die "no mapping file given. Usage: $0 <mapping-file>. Each line is \"tenant-id,credstore-ref\" for one operator-provisioned credential."
[[ -r "$MAPPING_FILE" ]] || die "mapping file '$MAPPING_FILE' is not readable"
command -v kubectl >/dev/null 2>&1 || die "kubectl not found"
command -v python3 >/dev/null 2>&1 || die "python3 not found (needed to re-home the Secret's data under the new name)"

kubectl get namespace "$NAMESPACE" >/dev/null 2>&1 || die "namespace '$NAMESPACE' does not exist"

UUID_RE='^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
DIGEST_HEX_LEN=16

# `naming::sanitize`/`truncate` reimplemented a third time -- see
# `provision-platform-kubeconfig-secret.sh`'s own header note on why the
# duplication is the point (this script prints every name it derives, so a
# drift between here and the Rust/shell/writer versions is loud, not a
# `FailedMount` nobody can explain).
sanitize_and_truncate() {
    local raw="$1" limit="$2" out
    out="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9-]/-/g')"
    out="$(printf '%s' "$out" | sed -E 's/^-+//; s/-+$//')"
    if [[ "${#out}" -gt "$limit" ]]; then
        out="${out:0:$limit}"
        out="$(printf '%s' "$out" | sed -E 's/-+$//')"
    fi
    printf '%s' "$out"
}

# FNV-1a 64-bit, not `sha2`/`sha256sum` -- see
# `provision-platform-kubeconfig-secret.sh`'s own header note (Dylint's
# `DE0708` bans a direct `sha2` import repository-wide; this is a naming
# collision guard, not a security boundary; `keycloak-idp-plugin`'s
# `user_facade::legacy_filter_hash` is the standing precedent whose constants
# are copied verbatim below). Computed in `python3` -- already required here
# for the JSON re-homing below -- for exact, masked 64-bit arithmetic rather
# than leaning on bash's own (unspecified, if reliable in practice) integer
# width.
fnv1a_hex() {
    python3 -c '
import sys
FNV1A_BASIS = 0xcbf29ce484222325
FNV1A_PRIME = 0x100000001B3
MASK = 0xFFFFFFFFFFFFFFFF
h = FNV1A_BASIS
for byte in sys.argv[1].encode():
    h ^= byte
    h = (h * FNV1A_PRIME) & MASK
sys.stdout.write(f"{h:016x}")
' "$1"
}

# The PRE-WS1 rule, unchanged, for finding a Secret an operator provisioned
# before this migration existed. No tenant, no digest -- exactly what
# `provision-platform-kubeconfig-secret.sh` derived before this task.
old_name() { sanitize_and_truncate "${SECRET_PREFIX}${1}" 63; }

# The current rule, matching `qa-runs`' `naming::secret_name` and
# `provision-platform-kubeconfig-secret.sh`'s current `derive_name`: a
# readable head, then a fixed-width digest of the whole
# `(prefix, tenant, reference)` tuple. Args: tenant, reference.
new_name() {
    local tenant="$1" reference="$2" full digest suffix readable_budget readable
    full="${SECRET_PREFIX}${tenant}-${reference}"
    digest="$(fnv1a_hex "$full")"
    suffix="${digest:0:$DIGEST_HEX_LEN}"
    readable_budget=$((63 - DIGEST_HEX_LEN - 1))
    readable="$(sanitize_and_truncate "$full" "$readable_budget")"
    if [[ -z "$readable" ]]; then
        printf '%s' "$suffix"
    else
        printf '%s-%s' "$readable" "$suffix"
    fi
}

created=0
already_existed=0
no_old_secret=0
reported=0
line_no=0

while IFS=',' read -r tenant reference || [[ -n "${tenant:-}" ]]; do
    line_no=$((line_no + 1))
    # Trim surrounding whitespace on each field without invoking a
    # subshell per line -- this loop runs once per credential in a mapping
    # an operator hand-wrote, so a bash-builtin trim keeps it legible.
    tenant="${tenant#"${tenant%%[![:space:]]*}"}"
    tenant="${tenant%"${tenant##*[![:space:]]}"}"
    reference="${reference#"${reference%%[![:space:]]*}"}"
    reference="${reference%"${reference##*[![:space:]]}"}"

    [[ -z "$tenant" ]] && continue
    [[ "$tenant" == \#* ]] && continue

    if [[ -z "$reference" ]]; then
        echo "rename-qa-secrets: REPORTED line $line_no: '$tenant' has no reference after the comma; not guessed, skipped" >&2
        reported=$((reported + 1))
        continue
    fi
    if [[ ! "$tenant" =~ $UUID_RE ]]; then
        echo "rename-qa-secrets: REPORTED line $line_no: '$tenant' is not a valid tenant id (uuid) for reference '$reference'; not guessed, skipped" >&2
        reported=$((reported + 1))
        continue
    fi

    old="$(old_name "$reference")"
    new="$(new_name "$tenant" "$reference")"

    if ! kubectl get secret "$old" -n "$NAMESPACE" >/dev/null 2>&1; then
        echo "rename-qa-secrets: no secret/$old (reference '$reference') in namespace $NAMESPACE; nothing to migrate for this line, skipped"
        no_old_secret=$((no_old_secret + 1))
        continue
    fi

    if kubectl get secret "$new" -n "$NAMESPACE" >/dev/null 2>&1; then
        echo "rename-qa-secrets: secret/$new already exists (idempotent no-op) for reference '$reference', tenant '$tenant'"
        already_existed=$((already_existed + 1))
        continue
    fi

    if [[ -n "${DRY_RUN:-}" ]]; then
        echo "rename-qa-secrets: DRY RUN -- would create secret/$new from secret/$old (reference '$reference', tenant '$tenant')"
        continue
    fi

    # Copy the existing Secret's data under the new name via `create`, not
    # `apply`: `apply` would keep the material a second time in a
    # last-applied-configuration annotation, the same reasoning
    # `provision-platform-kubeconfig-secret.sh` states for its own write.
    # The old object's identity (resourceVersion, uid, ownerReferences,
    # creationTimestamp, status, the old name itself) is stripped rather
    # than copied -- this is a new object, not a rename in place, and the
    # original is deliberately left untouched for rollback.
    kubectl get secret "$old" -n "$NAMESPACE" -o json | python3 -c '
import json, sys
obj = json.load(sys.stdin)
obj["metadata"] = {"name": sys.argv[1], "namespace": sys.argv[2]}
obj.pop("status", None)
json.dump(obj, sys.stdout)
' "$new" "$NAMESPACE" | kubectl create -n "$NAMESPACE" -f - >/dev/null

    echo "rename-qa-secrets: created secret/$new from secret/$old (reference '$reference', tenant '$tenant') -- original left in place for rollback"
    created=$((created + 1))
done < "$MAPPING_FILE"

echo "rename-qa-secrets: done. created=$created already-existed=$already_existed no-old-secret=$no_old_secret reported=$reported"
[[ "$reported" -eq 0 ]] || die "$reported line(s) could not be migrated automatically; see REPORTED lines above and re-run after fixing the mapping file"
