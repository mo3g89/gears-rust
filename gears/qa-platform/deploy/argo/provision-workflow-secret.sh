#!/usr/bin/env bash
# Put the `qa-platform-workflow` Keycloak client's secret into a Kubernetes
# `Secret` that Argo workflow pods reference.
#
# WHO RUNS THIS, AND WHEN. An operator, on a host with `kubectl` pointed at the
# cluster qa-runs submits workflows to, ONCE per cluster -- after Keycloak has
# imported the realm carrying the `qa-platform-workflow` client, and BEFORE the
# first run is launched with `qa-runs.executor: argo`. It is idempotent, so
# re-running it after a realm re-import (which is when the secret can change) is
# the intended way to refresh it. `deploy/remote/sync.sh --argo` runs it on the
# remote as part of a deploy; nothing else does.
#
# WHY IT IS A SEPARATE SCRIPT AND NOT SOMETHING THE ADAPTER DOES. The Argo
# adapter never resolves secret material -- it emits a `secretKeyRef` and lets
# the kubelet resolve it. That is ADR-0001's 2026-08-27 waiver condition 3
# ("`RunSpec` continues to carry secret references, never material") and the
# port's own claim that "qa-runs never reads a secret's contents"
# (qa-runs/.../domain/ports/run_executor.rs:85-86). A qa-runs process that
# created this Secret would have to hold the client secret in memory, and that
# sentence would stop being true. So the Secret is PRE-PROVISIONED, out of
# band, by this file.
#
# WHY A CLIENT SECRET IS NEEDED AT ALL. `GET /qa/v1/test-bundles/{id}` is
# `.authenticated()` (qa-catalog/.../api/rest/routes/bundles.rs:31), so a
# workflow pod fetching its test bundle gets a 401 without a token. The
# decision taken on 2026-08-27 was a dedicated confidential service-account
# client with a hardcoded `tenant_id` claim -- see
# deploy/realm/render-realm.sh's header for what that client is and why the
# claim is hardcoded.
#
# THE VALUE IS A COMMITTED DEV FIXTURE, read out of the realm file by default.
# That is deliberate and it is the same trade this stack already makes for
# admin/admin and postgres qa/qa: the realm file has to carry a secret for the
# client to exist at all, so a second copy typed into this script would be a
# value that can drift. A deployment that wants a real secret sets
# WORKFLOW_CLIENT_SECRET here and changes the client's secret in Keycloak; the
# two must match, and nothing in this stack checks that they do except a 401 on
# the token request.
#
# Usage:
#   provision-workflow-secret.sh
#
#   NAMESPACE                 Namespace to create the Secret in. Default `argo`
#                             -- the same default as `qa-runs.argo.namespace`,
#                             because the kubelet resolves a `secretKeyRef` in
#                             the POD's namespace and the pod is the workflow's.
#   SECRET_NAME               Default `qa-platform-workflow-oidc`. Must equal
#                             `qa-runs.argo.bundle_auth.client_secret_secret`.
#   SECRET_KEY                Default `client_secret`. Must equal
#                             `qa-runs.argo.bundle_auth.client_secret_key`.
#   WORKFLOW_CLIENT_SECRET    The value. Default: read from REALM_FILE.
#   REALM_FILE                Default: the committed realm file next to this
#                             script's sibling realm directory.
#   CLIENT_ID                 Client whose secret is read from REALM_FILE.
#                             Default `qa-platform-workflow`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

NAMESPACE="${NAMESPACE:-argo}"
SECRET_NAME="${SECRET_NAME:-qa-platform-workflow-oidc}"
SECRET_KEY="${SECRET_KEY:-client_secret}"
CLIENT_ID="${CLIENT_ID:-qa-platform-workflow}"
# deploy/realm/, not the deleted deploy/compose/. This is a RELATIVE path, which
# is why moving the realm source broke it without any grep for "compose/" in this
# directory finding it -- the string here is "../compose/...", assembled at run
# time from SCRIPT_DIR.
REALM_FILE="${REALM_FILE:-$SCRIPT_DIR/../realm/keycloak/realm-qa-platform.json}"

die() { echo "provision-workflow-secret: $*" >&2; exit 1; }

command -v kubectl >/dev/null 2>&1 || die "kubectl not found. On a k3s host: export KUBECONFIG=/etc/rancher/k3s/k3s.yaml and use /usr/local/bin/kubectl."

# The value. Read from the realm rather than defaulted here, so there is one
# copy of it in the tree; see the header.
if [[ -n "${WORKFLOW_CLIENT_SECRET:-}" ]]; then
    value="$WORKFLOW_CLIENT_SECRET"
    source_desc="\$WORKFLOW_CLIENT_SECRET"
else
    command -v jq >/dev/null 2>&1 || die "jq not found, and it is needed to read the client secret out of $REALM_FILE. Install jq, or set WORKFLOW_CLIENT_SECRET."
    [[ -f "$REALM_FILE" ]] || die "realm file '$REALM_FILE' not found. Set REALM_FILE, or set WORKFLOW_CLIENT_SECRET to skip reading it."
    value="$(jq -r --arg c "$CLIENT_ID" '.clients[] | select(.clientId==$c) | .secret // empty' "$REALM_FILE")"
    [[ -n "$value" ]] || die "client '$CLIENT_ID' in '$REALM_FILE' has no \`secret\` field. A public client has no secret and cannot use the client_credentials grant -- check that the realm carries the confidential workflow client."
    source_desc="$REALM_FILE (client $CLIENT_ID)"
fi

# --from-file, not --from-literal: a literal puts the secret in this process'
# argv, where every `ps` on the host can read it. The temp file is created with
# a 0600 umask and removed on exit.
tmp="$(umask 077 && mktemp)"
trap 'rm -f "$tmp"' EXIT
printf '%s' "$value" > "$tmp"

kubectl get namespace "$NAMESPACE" >/dev/null 2>&1 \
    || die "namespace '$NAMESPACE' does not exist. It is Argo's namespace; install Argo Workflows first."

# `create ... --dry-run=client | replace` on the update path, NOT
# `apply`: `kubectl apply` records the whole object -- secret value included --
# in a `kubectl.kubernetes.io/last-applied-configuration` annotation, so the
# credential would be stored twice, once where nothing reads it and where
# `kubectl describe` prints it.
manifest="$(kubectl create secret generic "$SECRET_NAME" \
    --namespace "$NAMESPACE" \
    --from-file="$SECRET_KEY=$tmp" \
    --dry-run=client -o yaml)"

if kubectl get secret "$SECRET_NAME" -n "$NAMESPACE" >/dev/null 2>&1; then
    printf '%s' "$manifest" | kubectl replace -n "$NAMESPACE" -f - >/dev/null
    echo "provision-workflow-secret: replaced secret/$SECRET_NAME in namespace $NAMESPACE"
else
    printf '%s' "$manifest" | kubectl create -n "$NAMESPACE" -f - >/dev/null
    echo "provision-workflow-secret: created secret/$SECRET_NAME in namespace $NAMESPACE"
fi

# READ BACK AND COMPARE. Not a formality: `--from-file` names the key after the
# file unless the `key=` form is used, and getting that wrong produces a Secret
# whose key is a mktemp name -- which the pod then cannot resolve, failing at
# CreateContainerConfigError minutes later with no line naming this script.
got="$(kubectl get secret "$SECRET_NAME" -n "$NAMESPACE" -o jsonpath="{.data.$SECRET_KEY}" 2>/dev/null | base64 -d 2>/dev/null || true)"
if [[ "$got" != "$value" ]]; then
    die "read-back of secret/$SECRET_NAME key '$SECRET_KEY' in namespace $NAMESPACE does not match what was written. Keys present: $(kubectl get secret "$SECRET_NAME" -n "$NAMESPACE" -o jsonpath='{.data}' 2>/dev/null | tr ',' ' ')"
fi
echo "PASS: secret/$SECRET_NAME key '$SECRET_KEY' in namespace $NAMESPACE reads back equal to the value from $source_desc (${#value} bytes; the value itself is not printed)"
echo "provision-workflow-secret: point qa-runs at it with"
echo "provision-workflow-secret:   qa-runs.argo.bundle_auth.client_secret_secret: $SECRET_NAME"
echo "provision-workflow-secret:   qa-runs.argo.bundle_auth.client_secret_key: $SECRET_KEY"
