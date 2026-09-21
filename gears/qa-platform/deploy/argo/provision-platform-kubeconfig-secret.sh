#!/usr/bin/env bash
# Materialise the Kubernetes `Secret` a workflow pod mounts as its kubeconfig,
# for one qa-environments platform.
#
# WHY THIS IS NEEDED AT ALL. `POST /qa/v1/environments` REQUIRES a kubeconfig --
# `kubeconfig_credstore_ref` must not be empty (measured: a body with only
# `name` is a 400 naming that field) -- so EVERY run carries a
# `KubeconfigMount`, and the Argo adapter turns it into a secret volume that is
# deliberately NOT optional: "a kubeconfig that does not resolve must fail the
# execution rather than silently target nothing"
# (`run_executor.rs:67-70`). A pod whose kubeconfig Secret does not exist sits
# `Pending` forever with `FailedMount`, and the run never starts.
#
# WHY IT IS A SCRIPT AND NOT SOMETHING A GEAR DOES -- decision D4, still open,
# and this script does not close it. The scoping document's two options were
# (A) somebody outside qa-runs maintains a Secret per platform, keeping
# `RunSpec`'s no-material property end to end, and (B) the adapter resolves the
# reference from credstore, which makes "qa-runs never reads a secret's
# contents" false. The adapter implements (A)'s adapter half. THIS SCRIPT IS
# (A)'s OTHER HALF DONE BY HAND, once, by an operator -- not the automatic
# per-platform reconciliation (A) eventually needs, which belongs in
# qa-environments and would give that gear a Kubernetes dependency of its own.
# Until that decision is taken, a platform created through the API needs a
# human to run this before its first run.
#
# IT DOES NOT READ CREDSTORE, and it cannot: the only backend is
# static-credstore-plugin, whose store is `HashMap`s behind an `RwLock`, and
# `POST /qa/v1/environments` with a raw `kubeconfig` writes there and never returns
# the document. So the kubeconfig comes from a FILE the operator already has --
# which is also why the reference, not the material, is what the platform row
# carries.
#
# Usage:
#   provision-platform-kubeconfig-secret.sh <tenant-id> <credstore-ref> [kubeconfig-file]
#
#   <tenant-id>        The platform's owning tenant (a UUID). The Secret NAME
#                      is derived from a digest of the tenant AND the
#                      reference together, so two tenants registering the
#                      same reference text no longer derive one Secret --
#                      see `derive_name`'s own comment below for why a digest
#                      and not a truncated concatenation. Get the tenant
#                      wrong and the script still succeeds, but at a name
#                      `qa-runs` never asks the kubelet to resolve.
#   <credstore-ref>    The platform's `kubeconfig_credstore_ref`, verbatim as it
#                      was given to POST /qa/v1/environments. The Secret NAME is
#                      derived from it, so it has to match exactly.
#   [kubeconfig-file]  Default /etc/rancher/k3s/k3s.yaml.
#
#                      A LOOPBACK `server:` IS REWRITTEN, and only a loopback
#                      one. This file is consumed by test code inside a pod,
#                      where 127.0.0.1 is the pod's own loopback and nothing is
#                      listening on it -- so a loopback address is not a choice
#                      the test author could have meant, it is a guaranteed
#                      `ClusterUnreachable` on every test in the run. Measured:
#                      the default k3s file was copied verbatim and the whole
#                      suite failed that way.
#
#                      Any other address is left ALONE, because a kubeconfig
#                      naming a real cluster is the test author's business and
#                      this script has no way to know better. Set
#                      SERVER_ADDRESS to override the substitution, or
#                      SERVER_ADDRESS=- to disable it.
#
#   NAMESPACE          Default `argo`, matching `qa-runs.argo.namespace`.
#   SECRET_PREFIX      Default `qa-platform-`, matching
#                      `qa-runs.argo.secret_name_prefix`.
#   SECRET_KEY         Default `value`, matching `qa-runs.argo.secret_key`.
set -euo pipefail

TENANT_ID="${1:-}"
REF="${2:-}"
KUBECONFIG_FILE="${3:-/etc/rancher/k3s/k3s.yaml}"
NAMESPACE="${NAMESPACE:-argo}"
SECRET_PREFIX="${SECRET_PREFIX:-qa-platform-}"
SECRET_KEY="${SECRET_KEY:-value}"
SERVER_ADDRESS="${SERVER_ADDRESS:-}"

die() { echo "provision-platform-kubeconfig-secret: $*" >&2; exit 1; }

UUID_RE='^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
[[ -n "$TENANT_ID" ]] || die "no tenant id given. Usage: $0 <tenant-id> <credstore-ref> [kubeconfig-file]. The tenant is folded into the Secret name ahead of the reference so two tenants registering the same reference text do not collide."
[[ "$TENANT_ID" =~ $UUID_RE ]] || die "tenant id '$TENANT_ID' is not a UUID; a Secret name derived from a malformed tenant is a Secret name qa-runs will never independently reconstruct"
[[ -n "$REF" ]] || die "no credstore reference given. Usage: $0 <tenant-id> <credstore-ref> [kubeconfig-file]. The reference is the platform's kubeconfig_credstore_ref as POSTed; the Secret name is derived from it, so a different spelling produces a Secret the pod will not find."
[[ -r "$KUBECONFIG_FILE" ]] || die "kubeconfig '$KUBECONFIG_FILE' is not readable"
command -v kubectl >/dev/null 2>&1 || die "kubectl not found"

# `naming::secret_name` reimplemented, and the duplication is the point of
# this comment. Two implementations of one rule is a drift risk with a
# silent symptom (a pod that cannot mount), which is why this script PRINTS
# the derived name -- compare it with `kubectl describe pod`'s FailedMount
# event if a run sits Pending.
#
# WHY A HASH SUFFIX. An earlier version of this rule put the tenant ahead of
# the reference and truncated the concatenation to 63 bytes, reasoning that
# truncating from the right always eats the reference and never the tenant.
# Measured in production: with the default prefix and a full tenant, only 14
# bytes of reference survive, and two real references sharing that many
# leading characters (`qa-environments-credential-ssh-private-key` and
# `...-vinfra-password`) both truncated to `qa-environment` and derived ONE
# Secret -- every credential of one tenant collapsed into it. A bounded-length
# name derived from unbounded input has to protect distinctness of the WHOLE
# input, not a prefix of it, so the name is now `{readable}-{digest}`: a
# human-readable head (sanitised and truncated, exactly as before, but now
# only a hint -- see naming.rs's own doc), and a fixed-width digest of the
# entire `prefix + tenant + "-" + reference` string that survives no matter
# what the head truncates away.
#
# WHY FNV-1a, NOT `sha2`/`sha256sum`. `naming::secret_name`'s own doc has the
# full reasoning; in short, this workspace's Dylint `DE0708` bans a direct
# `sha2`/`sha1`/`md5` import outside one allow-listed file (`dylint.toml`'s
# `hasher_allowed_paths`) -- this repository already tore out direct `sha2`
# usage once (`CHANGELOG.md`: "replace all direct sha2 usage with FNV-1a")
# and does not want it back, in a shell reimplementation any more than in the
# Rust it mirrors. This is a naming collision guard, not a security boundary,
# so a cryptographic hash was never required -- `keycloak-idp-plugin`'s
# `user_facade::legacy_filter_hash` is this workspace's standing precedent
# for exactly this shape of problem, and the constants below are copied from
# it verbatim (public FNV spec, fixed constants, so the output is identical
# across Rust versions and platforms -- which matters doubly here, since the
# shell and the Rust must agree).
#
# WHY IN `python3`. FNV-1a is not in `hashlib`, but this script already
# requires `python3` (the loopback-server rewrite below uses it), so
# computing the handful of XOR-and-multiply steps there costs no new tool
# and stays exact 64-bit arithmetic without depending on bash's own integer
# width (which is 64-bit on every mainstream build, but not a documented
# guarantee the way Python's arbitrary-precision integers, explicitly masked
# to 64 bits below, are).
DIGEST_HEX_LEN=16

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

derive_name() {
    local prefix="$1" tenant="$2" reference="$3" full digest suffix readable_budget readable
    full="${prefix}${tenant}-${reference}"
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

SECRET_NAME="$(derive_name "$SECRET_PREFIX" "$TENANT_ID" "$REF")"
[[ -n "$SECRET_NAME" ]] || die "the reference '$REF' with prefix '$SECRET_PREFIX' and tenant '$TENANT_ID' derived an empty Secret name, which should be impossible -- the digest suffix alone is never empty"

kubectl get namespace "$NAMESPACE" >/dev/null 2>&1 || die "namespace '$NAMESPACE' does not exist"

# Rewrite a LOOPBACK server address, and nothing else -- see the header. The
# replacement defaults to the address this node is reachable at from a pod,
# which is the same value the chart's gears-argo-configmaps.yaml derives for the gears'
# own copy; the two are independent on purpose, since a platform kubeconfig may
# legitimately name a different cluster entirely.
SOURCE_FILE="$KUBECONFIG_FILE"
current_server="$(kubectl config view --kubeconfig "$KUBECONFIG_FILE" --minify -o jsonpath='{.clusters[0].cluster.server}' 2>/dev/null || true)"

if [[ "$SERVER_ADDRESS" == "-" ]]; then
    echo "provision-platform-kubeconfig-secret: SERVER_ADDRESS=- given; leaving server '$current_server' exactly as it is"
elif [[ "$current_server" =~ ^https?://(127\.[0-9.]+|localhost|\[::1\])(:|/|$) ]]; then
    if [[ -n "$SERVER_ADDRESS" ]]; then
        new_server="$SERVER_ADDRESS"
    else
        # The node's own routable address. `kubectl get node -o jsonpath` asks
        # the cluster what it calls itself rather than guessing from an
        # interface, which is what makes this right on a host with several.
        node_ip="$(kubectl get nodes -o jsonpath='{.items[0].status.addresses[?(@.type=="InternalIP")].address}' 2>/dev/null | awk '{print $1}')"
        [[ -n "$node_ip" ]] || die "the kubeconfig's server is the loopback address '$current_server', which is the POD's own loopback once mounted and can never reach a cluster -- and this script could not read a node InternalIP to substitute. Pass SERVER_ADDRESS=https://<host>:6443 explicitly, or SERVER_ADDRESS=- to store it unchanged anyway."
        port="$(printf '%s' "$current_server" | sed -nE 's#^https?://[^:/]+:([0-9]+).*#\1#p')"
        new_server="https://${node_ip}:${port:-6443}"
    fi
    SOURCE_FILE="$(mktemp)"
    trap 'rm -f "$SOURCE_FILE"' EXIT
    # A plain string substitution of the exact server value: editing only the
    # value we just read cannot disturb any other field.
    python3 - "$KUBECONFIG_FILE" "$SOURCE_FILE" "$current_server" "$new_server" <<'PYREWRITE'
import sys
src, dst, old, new = sys.argv[1:5]
text = open(src).read()
if old not in text:
    sys.exit(f"could not find server '{old}' in {src} to rewrite")
open(dst, "w").write(text.replace(old, new))
PYREWRITE
    [[ -s "$SOURCE_FILE" ]] || die "rewriting the loopback server address produced an empty file"
    echo "provision-platform-kubeconfig-secret: rewrote server '$current_server' -> '$new_server' (a loopback address is the POD's own loopback once mounted, so it can never reach a cluster)"
elif [[ -n "$SERVER_ADDRESS" ]]; then
    die "SERVER_ADDRESS was given but the kubeconfig's server '$current_server' is not a loopback address. This script only rewrites loopback addresses; pass a kubeconfig that already names the cluster you mean."
else
    echo "provision-platform-kubeconfig-secret: server '$current_server' is not a loopback address; storing it unchanged"
fi

manifest="$(kubectl create secret generic "$SECRET_NAME" \
    --namespace "$NAMESPACE" \
    --from-file="$SECRET_KEY=$SOURCE_FILE" \
    --dry-run=client -o yaml)"

# `create|replace`, not `apply`: apply stores the whole object -- material
# included -- in a last-applied-configuration annotation, so the credential
# would be kept a second time, where nothing reads it and where
# `kubectl describe` prints it.
if kubectl get secret "$SECRET_NAME" -n "$NAMESPACE" >/dev/null 2>&1; then
    printf '%s' "$manifest" | kubectl replace -n "$NAMESPACE" -f - >/dev/null
    echo "provision-platform-kubeconfig-secret: replaced secret/$SECRET_NAME in namespace $NAMESPACE"
else
    printf '%s' "$manifest" | kubectl create -n "$NAMESPACE" -f - >/dev/null
    echo "provision-platform-kubeconfig-secret: created secret/$SECRET_NAME in namespace $NAMESPACE"
fi

got="$(kubectl get secret "$SECRET_NAME" -n "$NAMESPACE" -o "jsonpath={.data.$SECRET_KEY}" 2>/dev/null | wc -c)"
[[ "${got:-0}" -gt 0 ]] || die "read-back of secret/$SECRET_NAME key '$SECRET_KEY' found nothing. Keys present: $(kubectl get secret "$SECRET_NAME" -n "$NAMESPACE" -o jsonpath='{.data}' 2>/dev/null)"
echo "PASS: secret/$SECRET_NAME key '$SECRET_KEY' in namespace $NAMESPACE holds $got base64 bytes"
echo "provision-platform-kubeconfig-secret: derived from tenant '$TENANT_ID' and reference '$REF' with prefix '$SECRET_PREFIX'."
echo "provision-platform-kubeconfig-secret: if a run still sits Pending, compare this name with the FailedMount event in 'kubectl describe pod'."
