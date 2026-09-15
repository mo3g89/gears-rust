#!/usr/bin/env bash
# Mirror this repository onto a remote host and install the qa-platform Helm
# chart into its k3s cluster. This is a DEVELOPMENT CONVENIENCE, not the
# install path: the chart installs on its own (see below). What this adds is
# the build+import-via-containerd dance a registry-less k3s node needs, the
# rsync that puts this checkout on it, and the post-deploy verification -- so
# one command takes a working tree to a running stand.
#
# WHAT IT DOES, in order, and it stops at the first failure rather than
# half-deploying: preflight (the four shared ones, plus kubectl and helm on
# the remote) -> rsync -> build gears/ui/runner images tagged with a
# timestamp, import all three into containerd ->
# helm upgrade --install (NO --wait, see below) -> kubectl rollout status
# for the three Deployments -> deploy/remote/verify-k8s.sh.
#
# NO RUST TOOLCHAIN IS INSTALLED OR NEEDED ON THE REMOTE:
# qa-platform.Dockerfile builds from a pinned `rust:` image with its own
# builder-stage toolchain. Docker is the only host requirement for the
# builds; k3s (already on the target node, per NOTES-hairpin.md) is the only
# additional requirement for the install.
#
# `target/` IS NOT SYNCED -- see lib.sh's COMMON_RSYNC_EXCLUDES.
#
# THE IMAGE TAG IS NOT COSMETIC. `imagePullPolicy: IfNotPresent` (both
# images.gears and images.ui default it) plus a FIXED tag means
# `helm upgrade` renders an identical PodSpec on every run, Kubernetes
# correctly does nothing (there is nothing new to roll out), and the deploy
# reports success while the OLD code keeps serving. IMAGE_TAG below is
# `deploy-<timestamp>`, computed fresh every run, and passed to Helm as
# `--set images.gears.tag=... --set images.ui.tag=...` -- that is what makes
# a re-deploy of unchanged config still roll the pods.
#
# THIS SCRIPT IS A CONVENIENCE, NOT A DEPENDENCY OF THE CHART. Everything
# the platform needs to exist is in the chart: `helm upgrade --install
# deploy/helm/qa-platform --set publicOrigin=...` installs a working stack on
# any cluster that can pull the images. What this script adds is the part a
# chart cannot do on a registry-less k3s node -- build the three images from
# this checkout and import them into containerd -- plus the rsync and the
# post-deploy verification that make a dev round-trip one command.
#
# In particular the realm and the Argo workflow Secret are NO LONGER RENDERED
# HERE. Keycloak 26.0.8 still will not expand a `${env.VAR}` placeholder in an
# import file (measured, it aborts start-up), but the substitution now happens
# at TEMPLATE time inside the chart -- see keycloak-realm-configmap.yaml and
# workflow-oidc-secret.yaml, which carry the full reasoning.
#
# THERE IS NO REGISTRY IN THIS DEPLOYMENT. `docker build` puts an image in
# the DOCKER daemon's store; k3s runs containerd, a completely separate
# store, and a pod referencing an image only Docker has fails
# ImagePullBackOff with an error naming a registry the image was never in.
# deploy/runner/build-and-import.sh already solves this for the runner image
# (`docker save $IMAGE | k3s ctr images import -`); this script follows the
# exact same path for the gears and UI images, since nothing about that
# mechanism is runner-specific.
#
# EXPECT THE GEARS POD TO CrashLoopBackOff ON A FRESH INSTALL, AND DO NOT
# TREAT IT AS FAILURE. Helm applies every normal resource (the gears
# Deployment included) before running post-install hooks, so on a first
# `helm install` the gears pod starts against an empty, unmigrated database
# with no root tenant row and aborts boot -- Kubernetes restarts it with the
# usual backoff until job-db-migrate.yaml and job-tenant-seed.yaml (both
# post-install hooks) have completed, at which point the NEXT restart
# succeeds. job-db-migrate.yaml's own header documents this end to end.
#
# THAT IS ALSO WHY THE `helm upgrade --install` BELOW HAS NO `--wait`, and
# must never be given one: `--wait` blocks on the gears Deployment reaching
# Ready BEFORE Helm runs the post-install hooks that are the only thing that
# can make it Ready. The full argument is at the HELM_ARGS array. This
# script's `kubectl rollout status` step runs AFTER `helm` returns -- i.e.
# after the hooks have completed, since hooks run synchronously as part of
# the same helm invocation -- and it is what actually blocks on the steady
# state.
#
# Usage:
#   deploy-k8s.sh [--target USER@HOST] [--public-origin URL] [--dry-run]
#
#   --target         ssh destination, e.g. root@node.example.com. REQUIRED
#                     (or set QA_PLATFORM_TARGET). No default: this used to
#                     default to one particular development node, which meant a
#                     bare invocation deployed to someone else's machine.
#   --public-origin  scheme://host[:port] the BROWSER will reach the stack
#                     at -- no path, no trailing slash. Drives Keycloak's
#                     issuer, the realm's redirect URIs, the UI bundle's
#                     baked-in issuer and the chart's TLS-adjacent pins, the
#                     REQUIRED (or set QA_PLATFORM_PUBLIC_ORIGIN). No default,
#                     for the same reason as --target: the old default baked one
#                     development host into every UI bundle and certificate this
#                     script produced.
#   --dry-run         rsync --dry-run, and print every remote command instead
#                     of running it. The read-only ssh/kubectl/helm preflight
#                     still runs, so this also answers "can I reach the host,
#                     and does it have what this needs, at all?".
set -euo pipefail

# SCRIPT_DIR / lib.sh sourced first, before any default lib.sh owns (MARKER,
# SSH_OPTS, MIN_DOCKER_GIB, MIN_PATH_GIB, and every preflight/remote_sh
# helper below) is read. This file is at gears/qa-platform/deploy/remote/,
# four levels below the repo root.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROG_NAME="deploy-k8s"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"

# ---------------------------------------------------------------- defaults --
# NO DEV-HOST DEFAULTS. Both of these are required, from the flag or from the
# environment. They named one particular development node before, so `deploy-k8s.sh`
# with no arguments rsynced to it, rebuilt on it and pointed a UI bundle and a TLS
# certificate at it -- convenient for exactly one person and a footgun for everyone
# else. `test_no_environment_hardcode.py` is what keeps them empty.
REMOTE_TARGET="${QA_PLATFORM_TARGET:-}"
# A DEDICATED MIRROR DIRECTORY, never an arbitrary path: the rsync below is
# `-a --delete`, and lib.sh's MARKER file is what lets this script recognise a
# directory as one it created and may therefore delete into. See
# `preflight_rsync_path_guard`.
REMOTE_PATH="/opt/gears-rust"
PUBLIC_ORIGIN="${QA_PLATFORM_PUBLIC_ORIGIN:-}"
DRY_RUN=false

NAMESPACE="qa-platform"
RELEASE="qa-platform"
CHART_REL="gears/qa-platform/deploy/helm/qa-platform"

# ------------------------------------------------------------------- parse --
while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)         REMOTE_TARGET="${2:?--target needs a value}"; shift 2 ;;
        --public-origin)  PUBLIC_ORIGIN="${2:?--public-origin needs a value}"; shift 2 ;;
        --dry-run)        DRY_RUN=true; shift ;;
        -h|--help)        sed -n '/^# Usage:/,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; exit 0 ;;
        *)                echo "deploy-k8s: unknown argument '$1' (try --help)" >&2; exit 2 ;;
    esac
done

# Checked before anything is transferred, rather than inside a remote heredoc
# several layers from the cause. The chart pins the same shape on the value it
# renders into the realm; this is the earlier of the two refusals.
if [[ -z "$REMOTE_TARGET" ]]; then
    echo "deploy-k8s: no target. Pass --target USER@HOST or set QA_PLATFORM_TARGET -- this script has no default node, deliberately." >&2
    exit 2
fi
if [[ -z "$PUBLIC_ORIGIN" ]]; then
    echo "deploy-k8s: no public origin. Pass --public-origin scheme://host[:port] or set QA_PLATFORM_PUBLIC_ORIGIN." >&2
    echo "deploy-k8s: it is baked into the UI bundle and the TLS certificate, so there is no safe default to guess." >&2
    exit 2
fi
if [[ ! "$PUBLIC_ORIGIN" =~ ^https?://[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?$ ]]; then
    echo "deploy-k8s: --public-origin '$PUBLIC_ORIGIN' is not a plain 'http(s)://host[:port]' origin (no trailing slash, no path) -- refusing to continue" >&2
    exit 1
fi

# --------------------------------------------------------------- repo root --
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
REQUIRED_FILES=("Cargo.toml" ".dockerignore"
                 "$CHART_REL/Chart.yaml"
                 "$CHART_REL/values.yaml"
                 "gears/qa-platform/deploy/cargo-features.argo"
                 "gears/qa-platform/deploy/docker/qa-platform.Dockerfile"
                 "gears/qa-platform/deploy/docker/qa-platform-ui.Dockerfile"
                 "gears/qa-platform/deploy/runner/build-and-import.sh")
for required in "${REQUIRED_FILES[@]}"; do
    if [[ ! -e "$REPO_ROOT/$required" ]]; then
        echo "deploy-k8s: '$REPO_ROOT' does not look like the gears-rust repo root ('$required' is missing) -- refusing to sync" >&2
        exit 1
    fi
done

# ---------------------------------------------------------------- excludes --
# lib.sh's COMMON_RSYNC_EXCLUDES, and nothing else. The rsync below is
# `-a --delete`, so anything on the REMOTE that the LOCAL tree does not have is
# removed -- which is exactly what is wanted for a mirror of a checkout, and
# why the shared exclude list (target/, node_modules/, .git/) is the whole of
# it. There is no generated remote-only state under the mirror any more: the
# realm and the Argo config fragments used to be rendered into it by helper
# scripts, and are rendered by the chart at template time now.
EXCLUDES=("${COMMON_RSYNC_EXCLUDES[@]}")
RSYNC_EXCLUDE_ARGS=()
for e in "${EXCLUDES[@]}"; do RSYNC_EXCLUDE_ARGS+=(--exclude "$e"); done

echo "deploy-k8s: $REPO_ROOT -> $REMOTE_TARGET:$REMOTE_PATH (PUBLIC_ORIGIN=$PUBLIC_ORIGIN, namespace=$NAMESPACE)"
$DRY_RUN && echo "deploy-k8s: --dry-run: nothing on the remote will be changed"

# ============================================================== preflight ==
# Two of these are READ-ONLY checks -- kubectl and helm. Unlike
# preflight_docker, this script does not install either for you.
# Installing a cluster CLI is a decision about the node, not about a deploy,
# and k3s already ships a kubectl (a shim, if the node has one, is not
# assumed here); helm has no equivalent
# reason to assume it is already there, so its absence is refused with a
# named remedy rather than silently fetched and run.
preflight_ssh_reachable "Preflight 1/8: ssh reachability (batch mode, no password prompt)"
preflight_docker "Preflight 2/8: docker on the remote"
preflight_disk_space "Preflight 3/8: disk space" "$REMOTE_PATH"
preflight_rsync_path_guard "Preflight 4/8: remote path is safe for rsync --delete" "$REMOTE_PATH" "$MARKER"

step "Preflight 5/8: kubectl on the remote"
if ssh_ro 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml; kubectl version --client >/dev/null 2>&1'; then
    echo "PASS: kubectl present on $REMOTE_TARGET ($(ssh_ro 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml; kubectl version --client --output=yaml 2>/dev/null' | grep -m1 gitVersion || echo 'version unreadable'))"
elif ssh_ro 'command -v k3s >/dev/null 2>&1'; then
    die "no 'kubectl' on \$PATH on $REMOTE_TARGET, but 'k3s' is present. k3s ships kubectl as a subcommand; expose it once with 'ln -s /usr/local/bin/k3s /usr/local/bin/kubectl' (k3s dispatches on argv[0]), then re-run this script."
else
    die "neither 'kubectl' nor 'k3s' is on \$PATH on $REMOTE_TARGET. This script installs neither -- it deploys INTO an existing k3s cluster (see NOTES-hairpin.md for the one this chart was measured against), it does not stand one up."
fi

step "Preflight 6/8: helm on the remote"
if ssh_ro 'command -v helm >/dev/null 2>&1'; then
    echo "PASS: helm present on $REMOTE_TARGET ($(ssh_ro 'helm version --short' 2>/dev/null || echo 'version unreadable'))"
else
    die "'helm' is not on \$PATH on $REMOTE_TARGET. Install it (e.g. 'curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash' on the node, or your distro's package), then re-run. This script does not install it for you -- see this file's header on why kubectl/helm are read-only checks here, unlike docker above."
fi

step "Preflight 7/8: node ports 80 and 443 are free for the UI pod's hostPort"
# ui-deployment.yaml's pod requests hostPort 80 and 443, and hostPort is a
# NODE-WIDE reservation -- so if ANYTHING already holds either port (an older
# release of this chart, a stray nginx, another project's web server) the UI
# pod never schedules. It does not fail loudly either: it sits Pending with a
# `node(s) didn't have free ports for the requested pod ports` predicate
# failure, and the ONLY symptom downstream is `kubectl rollout status` timing
# out several minutes later against a Deployment whose message names a port,
# not a cause.
#
# Worse, it is a plausibly-green failure: whatever already serves 80/443
# answers `curl $PUBLIC_ORIGIN/` with a 200, from the OLD deployment.
# verify-k8s.sh's check 6 defends against that from the other side (it compares
# the served leaf against the cluster Secret's ui.crt), but catching it BEFORE
# anything is installed is strictly better than diagnosing it afterwards.
#
# READ-ONLY, and it does NOT stop anything for you: deciding to take another
# service down is a decision about the node, exactly like the kubectl/helm
# checks above. `ss -ltn` (iproute2, present on any host running
# k3s) over `lsof`/`netstat`, and the listener list is compared for the two
# exact ports rather than grepped loosely -- ':8080' must not read as ':80'.
# `ss` MISSING MUST NOT READ AS "PORTS ARE FREE" -- an empty result from an
# absent tool is indistinguishable from an empty result from a clean node,
# which is the false-green shape this whole review round has been about. The
# probe therefore prints an explicit `PORTCHECK: ok`/`PORTCHECK: busy ...`
# sentinel and dies on anything else, rather than being read as "no output
# means fine".
PORT_PROBE="$(ssh_ro_script <<'PORTS' || true
if ! command -v ss >/dev/null 2>&1; then
    echo "PORTCHECK: no-ss"
    exit 0
fi
# `ss -Hltn` lists LISTENING TCP sockets with no header. Field 4 is
# Local Address:Port, including IPv6 forms like [::]:443 and
# *:80 -- taking the substring after the LAST colon is what makes all of
# those spellings compare equal, and comparing for equality with 80/443
# (rather than grepping) is what stops :8080 from reading as :80.
busy="$(ss -Hltn | awk '{n=split($4,a,":"); print a[n]}' | sort -u \
        | awk '$1=="80" || $1=="443"' | tr '\n' ' ')"
if [ -n "$busy" ]; then
    echo "PORTCHECK: busy $busy"
else
    echo "PORTCHECK: ok"
fi
PORTS
)"
case "$PORT_PROBE" in
    "PORTCHECK: ok") PORT_CONFLICTS="" ;;
    "PORTCHECK: no-ss")
        die "'ss' (iproute2) is not on \$PATH on $REMOTE_TARGET, so this preflight cannot tell whether ports 80/443 are free -- and 'no listeners found' from a missing tool is exactly the false green this check exists to prevent. Install iproute2 on the node, or check by hand ('lsof -nP -iTCP:80 -sTCP:LISTEN', 'lsof -nP -iTCP:443 -sTCP:LISTEN') and re-run."
        ;;
    "PORTCHECK: busy "*) PORT_CONFLICTS="${PORT_PROBE#PORTCHECK: busy }" ;;
    *)
        die "the ports 80/443 preflight returned something this script does not understand ('$PORT_PROBE'). It is READ-ONLY, so failing here costs nothing; fix the probe rather than deploying without knowing whether the UI pod's hostPort can bind."
        ;;
esac
if [[ -n "$PORT_CONFLICTS" ]]; then
    # FATAL on a real run, LOUD BUT NON-FATAL under --dry-run. A dry run
    # installs nothing, and the operator running one is precisely the person
    # surveying a node whose ports are still held on purpose -- dying here
    # would hide preflight 8 and the whole rest of the plan from exactly the
    # person who needs to see it. A REAL run still refuses, because by then the
    # conflict is not hypothetical.
    PORT_MSG="port(s) ${PORT_CONFLICTS}already have a listener on $REMOTE_TARGET, and ui-deployment.yaml's pod requests hostPort 80 AND 443. The UI pod would stay Pending with 'node(s) didn't have free ports for the requested pod ports' and this deploy would fail minutes later at 'kubectl rollout status', naming a port rather than a cause.
  REMEDY: 'ss -ltnp' on the node names the process holding the port. If it is a previous release of this chart, 'kubectl -n $NAMESPACE delete pod -l app.kubernetes.io/component=ui' releases it. If it is anything else, stop that instead -- this script will not guess."
    if $DRY_RUN; then
        echo "WOULD FAIL (--dry-run, continuing so the rest of the plan is visible): $PORT_MSG" >&2
    else
        die "$PORT_MSG"
    fi
else
    echo "PASS: neither port 80 nor 443 has a listener on $REMOTE_TARGET -- the UI pod's hostPort can bind"
fi

step "Preflight 8/8: the UI pod's nginx resolver"
# clusterDns (values.yaml) BECOMES nginx's `resolver` IN THE UI POD, and nginx
# `resolver` takes an ADDRESS, not a name -- there is no way to write "kube-dns"
# there and have it work. `location /qa/v1/` uses a VARIABLE proxy_pass, so the
# gears' name is resolved per REQUEST through that resolver. Point it at the
# wrong address and every API call 502s while the SPA, the TLS and Keycloak all
# keep working -- a stack that looks healthy and does nothing.
#
# EMPTY IS NOW THE DEFAULT AND THE RECOMMENDED SETTING. The chart used to pin one
# cluster's ClusterIP as its default, and this preflight existed to stop that pin
# being an UNVERIFIED pin. The pin is gone: the UI image derives the address from
# its own /etc/resolv.conf at start-up
# (qa-platform-ui/docker-entrypoint.d/15-resolver-from-resolv-conf.envsh), which
# is right on any cluster without being told.
#
# WHY NOT THE IMAGE'S OWN $NGINX_LOCAL_RESOLVERS. nginx:alpine ships
# docker-entrypoint.d/15-local-resolvers.envsh, which exports exactly this from
# /etc/resolv.conf when NGINX_ENTRYPOINT_LOCAL_RESOLVERS is set. Consuming it
# would mean changing default.conf.template's `resolver ${NGINX_RESOLVER}` line
# AND the image's NGINX_ENVSUBST_FILTER allow-list -- and
# deploy/helm/tests/test_nginx_template.sh holds default.conf.template against a
# recorded baseline as the guard proving templatising that config changed no
# behaviour. A separate hook sets NGINX_RESOLVER instead and leaves both the
# template and that guard untouched.
#
# So this preflight now REPORTS rather than gates when the value is empty, and
# still gates when someone has pinned one. verify-k8s.sh's check 6b closes the
# same gap after the fact either way.
CLUSTER_DNS_WANT="$(awk '/^clusterDns:/{print $2; exit}' "$REPO_ROOT/$CHART_REL/values.yaml" | tr -d '"')"
CLUSTER_DNS_GOT="$(ssh_ro 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml; kubectl -n kube-system get svc kube-dns -o jsonpath={.spec.clusterIP} 2>/dev/null' || true)"
if [[ -z "$CLUSTER_DNS_WANT" ]]; then
    if [[ -n "$CLUSTER_DNS_GOT" ]]; then
        echo "PASS: clusterDns is unset, so the UI pod will derive nginx's resolver from its own /etc/resolv.conf -- which on this cluster should be kube-dns at $CLUSTER_DNS_GOT"
    else
        echo "NOTE: clusterDns is unset, so the UI pod derives nginx's resolver from its own /etc/resolv.conf. kube-dns's ClusterIP could not be read from $REMOTE_TARGET to state what that will be; the pod's own resolv.conf is authoritative either way."
    fi
elif [[ ! "$CLUSTER_DNS_WANT" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    die "clusterDns in '$CHART_REL/values.yaml' is set to '$CLUSTER_DNS_WANT', which is not a plain IPv4 address. nginx's \`resolver\` accepts only an address. Leave it empty to let the UI pod auto-detect, or set a real ClusterIP."
elif [[ -z "$CLUSTER_DNS_GOT" ]]; then
    echo "NOTE: values.yaml pins clusterDns=$CLUSTER_DNS_WANT but kube-dns's ClusterIP could not be read from $REMOTE_TARGET, so this preflight cannot confirm it. verify-k8s.sh's check 6b will catch a mismatch after the install, as a 502 on /qa/v1 naming this value. Leaving clusterDns empty would remove the need to confirm anything."
elif [[ "$CLUSTER_DNS_GOT" != "$CLUSTER_DNS_WANT" ]]; then
    die "values.yaml pins clusterDns=$CLUSTER_DNS_WANT but this cluster's kube-dns Service has ClusterIP $CLUSTER_DNS_GOT. That value becomes nginx's \`resolver\` in the UI pod and every API call resolves through it -- deploying as-is gives a stack where the SPA loads, TLS works, Keycloak works, and EVERY API call 502s. Either set clusterDns: $CLUSTER_DNS_GOT, or (better) leave it empty and let the pod detect it."
else
    echo "PASS: values.yaml pins clusterDns=$CLUSTER_DNS_WANT, which is this cluster's kube-dns ClusterIP"
fi

# ================================================================== sync ==
step "Writing the mirror marker"
remote_sh "mkdir + marker" "$REMOTE_PATH" "$MARKER" <<'MARK'
set -euo pipefail
mkdir -p "$1"
cat > "$1/$2" <<EOF
gears-rust remote mirror, written by gears/qa-platform/deploy/remote/deploy-k8s.sh
(this driver writes and recognises its own marker; see lib.sh).
This directory is an rsync --delete mirror of a gears-rust checkout: anything
here that is not in the source tree WILL BE DELETED by the next sync. Do not
keep anything you care about in it.
last-sync: $(date -Is)
EOF
echo "PASS: wrote $1/$2"
MARK

step "rsync$($DRY_RUN && echo ' (--dry-run)')"
RSYNC_ARGS=(-a --delete --human-readable --stats "${RSYNC_EXCLUDE_ARGS[@]}"
            --exclude "/$MARKER"
            -e "ssh ${SSH_OPTS[*]}")
$DRY_RUN && RSYNC_ARGS+=(--dry-run)
rsync "${RSYNC_ARGS[@]}" "$REPO_ROOT/" "$REMOTE_TARGET:$REMOTE_PATH/" \
    || die "rsync failed -- the remote is now in an unknown state; fix the cause and re-run before building"
echo "PASS: rsync completed$($DRY_RUN && echo ' (dry run -- nothing transferred)')"

# ================================================================== tag ==
# `deploy-<UTC-ish local timestamp>`, computed once and used for BOTH images
# so a single `helm upgrade` call names one coherent build. See this file's
# header on why this can never be `latest` (or any other fixed string) with
# `imagePullPolicy: IfNotPresent`.
IMAGE_TAG="deploy-$(date +%Y%m%d-%H%M%S)"
GEARS_IMAGE="cf-gears-qa-platform:$IMAGE_TAG"
UI_IMAGE="cf-gears-qa-platform-ui:$IMAGE_TAG"
echo "deploy-k8s: IMAGE_TAG=$IMAGE_TAG (gears=$GEARS_IMAGE, ui=$UI_IMAGE)"

# =============================================================== build ==
step "Building the gears image (tag $IMAGE_TAG) and importing it into containerd"
remote_sh "docker build + ctr import: gears" "$REMOTE_PATH" "$GEARS_IMAGE" <<'BUILDGEARS'
set -euo pipefail
REPO="$1"
IMAGE="$2"
cd "$REPO"

# Same ctr-command resolution as deploy/runner/build-and-import.sh: prefer
# 'k3s ctr' (present on any k3s node), fall back to a bare 'ctr' if someone
# runs this against a plain containerd install.
if command -v k3s >/dev/null 2>&1; then
    CTR=(k3s ctr)
elif command -v ctr >/dev/null 2>&1; then
    CTR=(ctr)
else
    echo "deploy-k8s: neither 'k3s' nor 'ctr' is on PATH -- cannot import '$IMAGE' into containerd" >&2
    exit 1
fi

echo "=== docker build $IMAGE (gears) ==="
# CARGO_FEATURES from the CANONICAL argo list (deploy/cargo-features.argo),
# not the Dockerfile's own ARG default -- test_features.py is what keeps
# that file a superset of the Dockerfile default. The in-cluster deploy always
# builds the argo-featured image, so it reads the canonical list rather than
# letting the Dockerfile's default decide.
#
# READ INTO A VARIABLE FIRST, AND CHECKED, rather than
# `--build-arg CARGO_FEATURES="$(cat ...)"` inline: `set -euo pipefail` does
# NOT catch a failing command substitution nested inside another command's
# argument list -- only a bare command or the last stage of a pipeline trips
# it. A missing/unreadable cargo-features.argo would make the substitution
# expand to an empty string, `docker build` would still run, the empty value
# would override the Dockerfile's own non-empty `ARG CARGO_FEATURES` default,
# and the result is a zero-feature image that builds, imports and deploys
# without one line of output naming the cause -- the exact false-green shape
# this project has already been bitten by (see lib.sh's `remote_sh_expect`).
CARGO_FEATURES_ARGO_FILE="gears/qa-platform/deploy/cargo-features.argo"
if ! CARGO_FEATURES="$(cat "$CARGO_FEATURES_ARGO_FILE")"; then
    echo "deploy-k8s: could not read '$CARGO_FEATURES_ARGO_FILE' (cwd $(pwd)) -- refusing to build with an empty CARGO_FEATURES" >&2
    exit 1
fi
if [ -z "$CARGO_FEATURES" ]; then
    echo "deploy-k8s: '$CARGO_FEATURES_ARGO_FILE' read successfully but is empty -- refusing to build with an empty CARGO_FEATURES (the Dockerfile's own ARG default would be silently overridden with nothing)" >&2
    exit 1
fi
docker build \
    -f gears/qa-platform/deploy/docker/qa-platform.Dockerfile \
    -t "$IMAGE" \
    --build-arg CARGO_FEATURES="$CARGO_FEATURES" \
    .

echo "=== importing $IMAGE into containerd via ${CTR[*]} ==="
# Streamed through a pipe rather than a temp file, same reasoning as
# build-and-import.sh: the image is large and a node's /tmp is not always
# big enough.
docker save "$IMAGE" | "${CTR[@]}" images import -

# THE FULL `name:tag`, NOT `${IMAGE%%:*}`. This build ran specifically to put
# THIS tag into containerd, and the Deployment about to be rolled names this
# tag exactly; matching on the repository alone is satisfied by ANY previous
# deploy-* tag still sitting in containerd, so a silently failed import would
# have passed this guard and the rollout would then wait forever on an image
# that is not there.
echo "=== containerd references matching $IMAGE ==="
listed="$("${CTR[@]}" images ls -q | grep -F "$IMAGE" || true)"
if [ -z "$listed" ]; then
    echo "deploy-k8s: after import, containerd lists no image tagged '$IMAGE'. The import reported success; check '${CTR[*]} images ls' by hand." >&2
    exit 1
fi
printf '%s\n' "$listed"
echo "PASS: gears image $IMAGE built and imported into containerd"
BUILDGEARS

step "Building the UI image (tag $IMAGE_TAG) and importing it into containerd"
remote_sh "docker build + ctr import: ui" "$REMOTE_PATH" "$UI_IMAGE" "$PUBLIC_ORIGIN" <<'BUILDUI'
set -euo pipefail
REPO="$1"
IMAGE="$2"
PUBLIC_ORIGIN="$3"
cd "$REPO/gears/qa-platform/qa-platform-ui"

if command -v k3s >/dev/null 2>&1; then
    CTR=(k3s ctr)
elif command -v ctr >/dev/null 2>&1; then
    CTR=(ctr)
else
    echo "deploy-k8s: neither 'k3s' nor 'ctr' is on PATH -- cannot import '$IMAGE' into containerd" >&2
    exit 1
fi

echo "=== docker build $IMAGE (ui) ==="
# NO /auth SEGMENT: Keycloak runs at its default root path in this chart
# (keycloak-deployment.yaml does not set KC_HTTP_RELATIVE_PATH; see
# values.yaml's own comment on why not). VITE_OIDC_CLIENT_ID must equal the
# realm's qa-platform-ui client id, which the chart's realm template does not
# rename, so this is that same literal.
docker build \
    -f ../deploy/docker/qa-platform-ui.Dockerfile \
    -t "$IMAGE" \
    --build-arg VITE_OIDC_ISSUER="$PUBLIC_ORIGIN/realms/qa-platform" \
    --build-arg VITE_OIDC_CLIENT_ID=qa-platform-ui \
    .

echo "=== importing $IMAGE into containerd via ${CTR[*]} ==="
docker save "$IMAGE" | "${CTR[@]}" images import -

# THE FULL `name:tag`, NOT `${IMAGE%%:*}`. This build ran specifically to put
# THIS tag into containerd, and the Deployment about to be rolled names this
# tag exactly; matching on the repository alone is satisfied by ANY previous
# deploy-* tag still sitting in containerd, so a silently failed import would
# have passed this guard and the rollout would then wait forever on an image
# that is not there.
echo "=== containerd references matching $IMAGE ==="
listed="$("${CTR[@]}" images ls -q | grep -F "$IMAGE" || true)"
if [ -z "$listed" ]; then
    echo "deploy-k8s: after import, containerd lists no image tagged '$IMAGE'. The import reported success; check '${CTR[*]} images ls' by hand." >&2
    exit 1
fi
printf '%s\n' "$listed"
echo "PASS: UI image $IMAGE built (issuer=$PUBLIC_ORIGIN/realms/qa-platform) and imported into containerd"
BUILDUI

step "Runner image: deploy/runner/build-and-import.sh"
# Deliberately its OWN tag scheme (qa-platform-pytest-runner:1 by default,
# matching values.yaml's images.runner.tag), NOT IMAGE_TAG. Unlike the
# gears/ui Deployments, qa-runs' Argo adapter reads runner_image out of
# qa-runs' OWN config at each workflow submission -- there is no long-lived
# Pod template that needs a new tag to notice a rebuild, so timestamping it
# would only produce an ever-growing set of containerd images with no
# corresponding config change ever pointing at the new ones.
remote_sh "runner image build + import" "$REMOTE_PATH" <<'BUILDRUNNER'
set -euo pipefail
cd "$1/gears/qa-platform"
bash deploy/runner/build-and-import.sh
BUILDRUNNER

# ============================================================ hostAliases ==
# Only consumed when gears.hostAliases.enabled is true in the chart's
# values.yaml -- NOTES-hairpin.md measured the pod->node hostPort hairpin as
# WORKING on the target cluster on 2026-08-29, so the shipped default is
# false and this whole block is a no-op on that cluster today. Read
# generically here (rather than hardcoded false) so a future values.yaml
# flip is honoured without a script change: if a cluster's hairpin ever
# breaks, this is the one place that needs to already know to ask for it.
#
# Read LOCALLY, from the checkout this script is about to rsync verbatim --
# not from the remote. The remote's copy of values.yaml does not necessarily
# exist yet at this point in the script (under --dry-run it never will,
# since rsync only prints what it would transfer), and it is byte-identical
# to this local file once rsync does run, so there is nothing to gain from a
# round trip and a real failure mode (this exact read, against an
# as-yet-unsynced remote) to avoid by not doing one.
#
# `awk ... getline`, not a jq/yq read: it is a fixed two-line YAML shape
# (`  hostAliases:` immediately followed by `    enabled: <bool>`), so a
# purpose-built pattern is simpler and has one fewer tool dependency than
# parsing the whole file as YAML for one boolean.
HOSTALIASES_ENABLED="$(awk '/^  hostAliases:/{getline; gsub(/^[ \t]+enabled:[ \t]*/,""); print; exit}' \
    "$REPO_ROOT/$CHART_REL/values.yaml")"
# Validated against the two literal values this awk pattern can legitimately
# produce, rather than trusting an `== "true"` comparison downstream to fail
# safe. It would: anything other than the literal string "true" already takes
# the disabled branch below. But this flag exists SPECIFICALLY to work around
# a broken pod->node hairpin (NOTES-hairpin.md) -- silently treating a
# values.yaml shape drift (a reindent, a reordered block, a stray comment) as
# "disabled" would deploy without the workaround it was supposed to apply,
# with nothing here naming that the read itself was the failure. Refusing
# loudly on an unrecognised result is what makes that distinguishable from a
# genuine "false" in values.yaml.
case "$HOSTALIASES_ENABLED" in
    true|false) ;;
    *) die "could not read gears.hostAliases.enabled out of '$CHART_REL/values.yaml' as 'true' or 'false' (got '$HOSTALIASES_ENABLED') -- the awk pattern expects the fixed two-line shape '  hostAliases:' immediately followed by '    enabled: <bool>'; if that shape changed, update the pattern rather than deploying blind to which branch this flag is in." ;;
esac
echo "deploy-k8s: gears.hostAliases.enabled=$HOSTALIASES_ENABLED (chart default; NOTES-hairpin.md measured the hairpin working, so 'false' is expected)"

HELM_ARGS=(upgrade --install "$RELEASE" "$CHART_REL"
           --namespace "$NAMESPACE" --create-namespace
           --set "publicOrigin=$PUBLIC_ORIGIN"
           --set "images.gears.tag=$IMAGE_TAG"
           --set "images.ui.tag=$IMAGE_TAG"
           # ---------------------------------------------------------------
           # NO `--wait`. NEVER RE-ADD IT. `--timeout` stays -- it still
           # bounds the post-install HOOKS (db-migrate, tenant-seed), which
           # Helm runs synchronously inside this same invocation.
           #
           # WHY `--wait` DEADLOCKS THIS PARTICULAR CHART: `--wait` blocks
           # until every NORMAL resource is Ready BEFORE Helm runs any
           # post-install hook. The gears Deployment is a normal resource and
           # it CANNOT become Ready until db-migrate and tenant-seed have run
           # (oagw's post_init aborts boot against an unmigrated, unseeded
           # database -- job-db-migrate.yaml's header is the full account).
           # So `--wait` waits ten minutes on a Deployment that is waiting on
           # hooks that `--wait` will not let start, returns non-zero, and
           # `set -euo pipefail` kills this script -- WITH THE HOOKS NEVER
           # HAVING RUN. The database is left unmigrated and the release is
           # left in a failed state that a subsequent `helm upgrade` may
           # refuse to roll forward over.
           #
           # This is NOT fixed by a longer --timeout, by an initContainer
           # gate on the gears Deployment (same cycle, see
           # job-db-migrate.yaml), or by `--wait-for-jobs` (which only adds
           # MORE waiting to the same blocked command).
           #
           # WHAT REPLACES IT: the `kubectl rollout status` step below, which
           # runs AFTER helm returns -- i.e. after the hooks have completed --
           # and is now load-bearing rather than a second opinion. It waits
           # per-Deployment with its own timeout and fails this script if any
           # of the three does not reach Available.
           # ---------------------------------------------------------------
           --timeout 10m)

if [[ "$HOSTALIASES_ENABLED" == "true" ]]; then
    step "hostAliases.enabled=true: first helm upgrade to create the UI Service, then resolving its ClusterIP"
    # On a first install the ui Service does not exist yet -- run once WITHOUT
    # uiServiceClusterIP to create it (the gears Deployment may CrashLoopBackOff
    # for a bit regardless; see this file's header), then read the ClusterIP
    # and run again with it set. On an upgrade the Service already exists, so
    # this first call is a normal no-op reconcile for it.
    remote_sh "helm upgrade --install (pass 1: create resources, incl. the UI Service)" "$REMOTE_PATH" <<HELM1
set -euo pipefail
cd "\$1"
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
helm ${HELM_ARGS[@]@Q}
HELM1
    step "Resolving the UI Service's ClusterIP for hostAliases"
    if $DRY_RUN; then
        # Pass 1's helm upgrade above did not really run (remote_sh printed
        # it and returned), so the UI Service may not exist on the cluster
        # at all yet -- an RFC 5737 documentation-range placeholder shows the
        # SHAPE of pass 2's command without pretending to have read a real
        # ClusterIP.
        UI_CLUSTER_IP="203.0.113.1"
        echo "NOTE: --dry-run: using a placeholder ClusterIP ($UI_CLUSTER_IP) since pass 1's helm upgrade did not really run. A real run reads this back from the cluster with kubectl."
    else
        UI_CLUSTER_IP="$(ssh_ro "export KUBECONFIG=/etc/rancher/k3s/k3s.yaml; kubectl -n $NAMESPACE get svc qa-platform-ui -o jsonpath='{.spec.clusterIP}'")"
        [[ -n "$UI_CLUSTER_IP" ]] || die "could not read qa-platform-ui's ClusterIP in namespace $NAMESPACE after the first helm upgrade -- hostAliases.enabled is true but the gears Deployment needs this address to reach the UI"
        echo "deploy-k8s: qa-platform-ui ClusterIP=$UI_CLUSTER_IP"
    fi
    HELM_ARGS+=(--set "uiServiceClusterIP=$UI_CLUSTER_IP")
    step "helm upgrade --install (pass 2: with uiServiceClusterIP set)"
else
    step "helm upgrade --install qa-platform"
fi

# WITH `--wait` DROPPED (see the HELM_ARGS comment above), THE ORDERING
# QUESTION THIS BLOCK USED TO FLAG IS NO LONGER LOAD-BEARING. It asked
# whether `--wait` blocks on the Deployments reaching Ready before or after
# Helm runs the post-install hooks. It blocks BEFORE, which is precisely why
# `--wait` is gone: the gears Deployment cannot reach Ready until those hooks
# have run. Without `--wait`, Helm applies the normal resources, returns from
# applying them as soon as they are CREATED, and runs the post-install hooks
# (db-migrate then tenant-seed, by hook-weight) synchronously as part of this
# same invocation, bounded by `--timeout`.
#
# SO WHEN THIS COMMAND RETURNS 0, THE HOOKS HAVE COMPLETED AND THE GEARS POD
# MAY STILL BE MID-CRASH-LOOP. That is expected: the pod aborts boot against
# the unmigrated database until the hooks land, and self-heals on its next
# restart. The `kubectl rollout status` step below is what waits for that,
# and it is the ONLY thing that does -- it is load-bearing, not a second
# opinion.
#
# WORTH CAPTURING ON THE FIRST REAL RUN, though nothing depends on it:
# `kubectl get events -n <ns> -w` and `kubectl get pods -n <ns> -w` in a
# second ssh session, started just before this command, show the gears pod's
# CrashLoopBackOff events interleaved with the db-migrate/tenant-seed Job
# pods being Scheduled. That is the shape to expect; a helm command that
# instead sits silently for minutes means `--wait` has crept back in.
echo "deploy-k8s: helm runs WITHOUT --wait (see the comment at HELM_ARGS). The gears pod will CrashLoopBackOff while the post-install hooks run; 'kubectl rollout status' after this command is what waits for the steady state."
remote_sh "helm upgrade --install" "$REMOTE_PATH" <<HELM2
set -euo pipefail
cd "\$1"
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
echo "=== helm upgrade --install starting \$(date -Is) ==="
helm ${HELM_ARGS[@]@Q}
echo "=== helm upgrade --install returned 0 at \$(date -Is) -- post-install/post-upgrade hooks have already run to completion at this point ==="
HELM2

# ============================================================== rollout ==
# THIS STEP IS LOAD-BEARING, NOT A SECOND OPINION. `--wait` is deliberately
# absent from the helm command above (see the HELM_ARGS comment for why it
# deadlocks this chart), so this is the ONLY thing in the whole script that
# blocks on the stack actually being healthy. If it is weakened or removed,
# deploy-k8s.sh reports success the moment Helm has applied manifests and run
# its hooks, with a gears pod that may never come up.
#
# By the time this runs, helm has returned 0, which means db-migrate and
# tenant-seed BOTH completed (a failed hook Job fails the helm command, and
# `set -euo pipefail` inside remote_sh's heredoc takes the script down with
# it). What is still in flight is the gears pod's crash-loop recovery: it has
# been aborting boot against the unmigrated database for the whole duration
# of the hooks, so the kubelet's restart backoff may already be at its 300s
# cap when the hooks finish. Hence 600s here, not the 180s this step used
# when `--wait` had already settled everything before it ran.
#
# THIS TIMEOUT IS NOT THE ONLY CEILING, AND AN EARLIER VERSION OF THIS COMMENT
# GOT THE ARITHMETIC WRONG. It claimed 600s bought "two full backoff intervals
# at the 300s cap". It does not, by itself: a Deployment's
# progressDeadlineSeconds counts from its LAST PROGRESS EVENT -- i.e. from when
# Helm created it, before the hooks ran -- and `kubectl rollout status` returns
# an ERROR IMMEDIATELY when it observes Progressing=False with
# reason=ProgressDeadlineExceeded, no matter what its own --timeout says. With
# the Kubernetes default of 600s, the budget actually left by the time this
# step starts is 600s MINUS the hook duration: four minutes of hooks leaves
# about 360s, and the result is a FALSE FAIL on a stack that was seconds from
# self-healing.
#
# gears-deployment.yaml therefore sets progressDeadlineSeconds: 1800 (see its
# comment for the arithmetic), which is strictly larger than the helm --timeout
# plus this ROLLOUT_TIMEOUT, so THIS timeout is the binding constraint again --
# which is what makes the number below mean what it says. The ui and keycloak
# Deployments keep the 600s default on purpose: neither waits on a hook. If
# ROLLOUT_TIMEOUT is ever raised past 1800s, raise the Deployment's deadline
# with it or the ceiling silently moves back.
step "kubectl rollout status: gears, ui, keycloak (the only health gate -- helm ran without --wait)"
ROLLOUT_TIMEOUT="${ROLLOUT_TIMEOUT:-600s}"
remote_sh_expect "kubectl rollout status" "ROLLOUT: all three Deployments are available" "$REMOTE_PATH" "$NAMESPACE" "$ROLLOUT_TIMEOUT" <<'ROLLOUT'
set -euo pipefail
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
NS="$2"
TIMEOUT="$3"
fail=0
for dep in qa-platform-gears qa-platform-ui qa-platform-keycloak; do
    if kubectl -n "$NS" rollout status "deployment/$dep" --timeout="$TIMEOUT"; then
        echo "PASS: deployment/$dep is Available"
    else
        echo "FAIL: deployment/$dep did not reach Available within $TIMEOUT." >&2
        # WHICH KIND OF FAILURE IS IT? `rollout status` exits non-zero for two
        # very different reasons and the old message asserted only one of them.
        # It can time out on ITS OWN --timeout (the pod is still working
        # through its restart backoff), or it can return the INSTANT it sees
        # the Deployment's own progressDeadlineSeconds expire -- and in that
        # second case the wait never got anywhere near $TIMEOUT. Saying "the
        # hooks already completed, so this is NOT the expected crash loop"
        # about a ProgressDeadlineExceeded is actively misleading: the pod may
        # be recovering exactly as designed and simply have spent its deadline
        # sitting through the hook window (see this file's comment above, and
        # gears-deployment.yaml's). Read the reason back rather than guess.
        reason="$(kubectl -n "$NS" get deploy "$dep" -o jsonpath='{.status.conditions[?(@.type=="Progressing")].reason}' 2>/dev/null || true)"
        if [ "$reason" = "ProgressDeadlineExceeded" ]; then
            deadline="$(kubectl -n "$NS" get deploy "$dep" -o jsonpath='{.spec.progressDeadlineSeconds}' 2>/dev/null || true)"
            echo "  CAUSE: deployment/$dep reports Progressing=False, reason=ProgressDeadlineExceeded (spec.progressDeadlineSeconds=${deadline:-600}). 'kubectl rollout status' returns the moment it sees this, so the wait ABORTED EARLY -- it did not spend $TIMEOUT. That deadline counts from the Deployment's last progress event, i.e. from when Helm created it, BEFORE the post-install hooks ran, so the hook window is charged against it." >&2
            echo "  This may well be a pod that was about to recover, not a broken one: check 'kubectl -n $NS get pods -l app.kubernetes.io/component=${dep#qa-platform-}' and re-run 'kubectl -n $NS rollout status deployment/$dep' before concluding anything. If the deadline is genuinely too tight for this cluster, raise progressDeadlineSeconds in the chart (gears-deployment.yaml sets 1800 for exactly this reason; ui/keycloak keep the 600s default)." >&2
        else
            echo "  CAUSE: the wait ran its full $TIMEOUT without Available (Progressing reason='${reason:-<none>}'). The hooks have already run by this point (helm returned 0), so a still-crash-looping gears pod here is NOT the early crash loop job-db-migrate.yaml documents." >&2
        fi
        for job in qa-platform-db-migrate qa-platform-tenant-seed; do
            succ="$(kubectl -n "$NS" get job "$job" -o jsonpath='{.status.succeeded}' 2>/dev/null || true)"
            echo "  hook job/$job succeeded=${succ:-0} (helm returned 0, so both should read 1)" >&2
        done
        echo "  'kubectl -n $NS describe deployment/$dep', 'kubectl -n $NS logs deployment/$dep --previous' and 'kubectl -n $NS get events --sort-by=.lastTimestamp' name the cause." >&2
        fail=1
    fi
done
if [ "$fail" -ne 0 ]; then
    echo "ROLLOUT: at least one Deployment failed to become Available" >&2
    exit 1
fi
echo "ROLLOUT: all three Deployments are available"
ROLLOUT

# ============================================================== verify ==
step "deploy/remote/verify-k8s.sh (Task 12)"
# NAMED ABSENCE, NOT A CRASH: Task 12 had not landed yet when this script was
# written. A future run of this exact script picks up verify-k8s.sh the
# moment it exists on the remote (it is rsynced along with everything else
# above) -- nothing here needs editing for that to start working.
#
# IMAGE_TAG IS PASSED THROUGH, not just PUBLIC_ORIGIN/NAMESPACE: verify-k8s.sh's
# first new check (every Deployment rolled to the tag this run just built)
# needs to know what that tag WAS, and this script is the only place that
# value exists -- it is computed fresh above and never written anywhere on
# the remote. Without it, that one check has no expected value to compare
# against and has to skip itself (see verify-k8s.sh's own handling of an
# unset IMAGE_TAG).
remote_sh "verify-k8s.sh (if present)" "$REMOTE_PATH" "$PUBLIC_ORIGIN" "$NAMESPACE" "$IMAGE_TAG" <<'VERIFYK8S'
set -euo pipefail
cd "$1"
VERIFY_SCRIPT="gears/qa-platform/deploy/remote/verify-k8s.sh"
# `-e`, not a separate `-x`/`-f` pair with identical bodies: `bash
# "$VERIFY_SCRIPT"` runs it as a bash argument either way, so its own execute
# bit is irrelevant to whether this can invoke it -- only whether the file
# exists at all.
if [ -e "$VERIFY_SCRIPT" ]; then
    PUBLIC_ORIGIN="$2" NAMESPACE="$3" IMAGE_TAG="$4" bash "$VERIFY_SCRIPT"
else
    echo "NOTE: $VERIFY_SCRIPT does not exist yet (Task 12) -- skipping automated verification. Check by hand: 'kubectl -n $3 get pods', the UI at $2, and Keycloak's discovery document at $2/realms/qa-platform/.well-known/openid-configuration."
fi
VERIFYK8S

step "Done"
if $DRY_RUN; then
    echo "deploy-k8s: dry run complete. The preflight results above are real; every remote command was printed, not run."
else
    echo "deploy-k8s: stack installed as release '$RELEASE' in namespace '$NAMESPACE' on $REMOTE_TARGET"
    echo "deploy-k8s: images: gears=$GEARS_IMAGE ui=$UI_IMAGE (both imported into containerd, no registry involved)"
    echo "deploy-k8s: open the UI at $PUBLIC_ORIGIN and log in with admin/admin (dev fixture credentials from the realm file)"
    echo "deploy-k8s: watch the crash-loop-then-recover gears pod with: ssh $REMOTE_TARGET 'KUBECONFIG=/etc/rancher/k3s/k3s.yaml kubectl -n $NAMESPACE get pods -w'"
fi
