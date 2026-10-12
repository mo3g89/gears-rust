#!/usr/bin/env bash
# Remove the runner-credential Secrets qa-environments' D4 writer left in the
# Argo namespace before finding #94 replaced it with pod-bound fetching.
# Nothing reads them any more. They still hold live credentials, which is
# why this script exists.
#
# Usage: cleanup-runner-secrets.sh <argo-namespace> [--delete]
#   Without --delete it only lists what it would remove.
#
# Selection: type Opaque, NO labels at all (the writer set none), and the
# writer's own name shape: "qa-platform-", the tenant UUID truncated to 34
# characters (the name had to fit 63), "-", and a 16-hex FNV-1a digest, e.g.
# qa-platform-00000000-df51-5b42-9538-d2b56b7ee9-0123456789abcdef.
# "Prefix qa-platform-, not Helm-labelled" is NOT a safe selector: the chart's
# own Secrets (qa-platform-postgres, -gears-config, -keycloak-admin, -realm,
# -tls) match it, and argo.namespace may equal the release namespace.
#
# Also selected: the orphaned workflow OIDC Secret. bf8d9c4d0 deleted the
# chart template that rendered it, but it had `helm.sh/resource-policy: keep`,
# so every upgrade leaves it behind, holding the live client secret of the
# `qa-platform-workflow` client. It is labelled, so the unlabelled rule above
# cannot see it. Its name came from argo.workflowSecretName, which was
# configurable (qa-platform-workflow-oidc was only the default), so it is
# matched by the template's two labels, whatever its name:
# app.kubernetes.io/name=qa-platform AND
# app.kubernetes.io/component=argo-workflow-oidc. verify-k8s.sh checks the
# same labels.
#
# Never selected, only listed for the operator to review and delete by hand:
# any other unlabelled Opaque Secret that is qa-platform-* or has a
# kubeconfig / qa-environment name (older writer builds used other names, e.g.
# qa-platform-qa-environments-credential-<hex>, and fixtures left
# smoke-fixture-kubeconfig and <x>-qa-environment, with no qa-platform- prefix).
#
# Refuses (exit 1) a namespace that holds the qa-platform release itself.
set -euo pipefail
ns=${1:?usage: cleanup-runner-secrets.sh <argo-namespace> [--delete]}
mode=${2:-list}
# writer_shape is defined once, in runner-secret-shape.sh, which verify-k8s.sh sources too.
# shellcheck source=runner-secret-shape.sh
. "$(dirname "${BASH_SOURCE[0]}")/runner-secret-shape.sh"

secrets=$(kubectl get secrets -n "$ns" -o json)
release=$(printf '%s' "$secrets" | jq -r '
  .items[].metadata.name
  | select(. == "qa-platform-gears-config" or startswith("sh.helm.release.v1.qa-platform."))' | head -1)
if [ -n "$release" ]; then
    echo "cleanup-runner-secrets: refusing: namespace $ns holds the qa-platform release (Secret $release). Run it against the Argo namespace only." >&2
    exit 1
fi

unlabelled_opaque='.items[]
  | select(.type == "Opaque")
  | select((.metadata.labels // {}) == {})
  | .metadata.name'
names=$(printf '%s' "$secrets" | jq -r --arg re "$writer_shape" "$unlabelled_opaque | select(test(\$re))")
oidc=$(printf '%s' "$secrets" | jq -r '
  .items[]
  | select((.metadata.labels // {})["app.kubernetes.io/name"] == "qa-platform")
  | select((.metadata.labels // {})["app.kubernetes.io/component"] == "argo-workflow-oidc")
  | .metadata.name')
names=$(printf '%s\n%s\n' "$names" "$oidc" | sed '/^$/d')
review=$(printf '%s' "$secrets" | jq -r --arg re "$writer_shape" '
  .items[] | select(.type == "Opaque") | select((.metadata.labels // {}) == {})
  | .metadata.name | select(test($re) | not)
  | select(startswith("qa-platform-") or test("kubeconfig|qa-environment"))')

if [ -n "$review" ]; then
    echo "cleanup-runner-secrets: NOT selected -- unlabelled Opaque Secrets of another name shape (qa-platform-* or a kubeconfig/qa-environment name). Older writer builds or fixtures may have left them; review them by hand and delete what is a leftover:"
    printf '%s\n' "$review" | sed 's/^/  /'
fi
if [ -z "$names" ]; then echo "cleanup-runner-secrets: nothing to remove in $ns"; exit 0; fi
printf '%s\n' "$names"
if [ "$mode" != "--delete" ]; then
    echo "cleanup-runner-secrets: $(printf '%s\n' "$names" | wc -l) Secret(s) listed; re-run with --delete to remove them"
    exit 0
fi
printf '%s\n' "$names" | xargs -r kubectl delete secret -n "$ns"
