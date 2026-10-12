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
# Older writer builds used other names (qa-platform-<credential ref>, e.g.
# qa-platform-qa-environments-credential-<hex>). They are NOT selected; the
# script lists every other unlabelled Opaque qa-platform-* Secret for the
# operator to review and delete by hand.
#
# Refuses (exit 1) a namespace that holds the qa-platform release itself.
set -euo pipefail
ns=${1:?usage: cleanup-runner-secrets.sh <argo-namespace> [--delete]}
mode=${2:-list}
writer_shape='^qa-platform-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{10}-[0-9a-f]{16}$'

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
others=$(printf '%s' "$secrets" | jq -r --arg re "$writer_shape" \
    "$unlabelled_opaque | select(startswith(\"qa-platform-\")) | select(test(\$re) | not)")

if [ -n "$others" ]; then
    echo "cleanup-runner-secrets: NOT selected -- unlabelled Opaque qa-platform-* Secrets of another name shape. Older writer builds may have left them; review them by hand and delete what is a writer leftover:"
    printf '%s\n' "$others" | sed 's/^/  /'
fi
if [ -z "$names" ]; then echo "cleanup-runner-secrets: nothing to remove in $ns"; exit 0; fi
printf '%s\n' "$names"
if [ "$mode" != "--delete" ]; then
    echo "cleanup-runner-secrets: $(printf '%s\n' "$names" | wc -l) Secret(s) listed; re-run with --delete to remove them"
    exit 0
fi
printf '%s\n' "$names" | xargs -r kubectl delete secret -n "$ns"
