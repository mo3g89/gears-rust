#!/usr/bin/env bash
# Build the pytest runner image and make it visible to k3s.
#
# WHY THE SECOND STEP EXISTS, because it is the step everyone forgets: `docker
# build` puts the image in the DOCKER daemon's image store. k3s runs
# containerd, with a completely separate store, and a pod referencing an image
# that only Docker has fails `ImagePullBackOff` -- with an error about a
# registry the image was never in. `k3s ctr images import` is the bridge. There
# is no registry in this deployment, so there is no `docker push` alternative.
#
# WHO RUNS THIS, AND WHEN. An operator, on the k3s node itself (both steps need
# to be local: the Docker daemon and containerd are both on that host), once
# per change to anything under deploy/runner/. `deploy/remote/sync.sh --argo`
# runs it on the remote as part of a deploy. It is not idempotent in the sense
# of being free -- it rebuilds -- but re-running it is safe and layer-cached.
#
# `imagePullPolicy: IfNotPresent` (qa-runs' default) is what lets an imported
# image be used. With `Always`, the kubelet would try to pull it from a
# registry and fail no matter what is in containerd.
#
# THE TAG MUST MATCH `qa-runs.argo.runner_image`, and it must NOT be `latest`
# with `IfNotPresent` on a node that already has an older `latest` -- the
# kubelet would keep the old one. That is why the default tag is a version, and
# why this script prints what to set the config to.
#
# Usage:
#   build-and-import.sh [IMAGE]
#
#   IMAGE   `name:tag`. Default `qa-platform-pytest-runner:1`. Also readable
#           from $IMAGE.
#   CTR     The containerd CLI to import through. Default: `k3s ctr` if a `k3s`
#           binary exists, else `ctr`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# deploy/runner -> deploy -> qa-platform -> gears -> repo root
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

IMAGE="${1:-${IMAGE:-qa-platform-pytest-runner:1}}"

die() { echo "build-and-import: $*" >&2; exit 1; }

[[ "$IMAGE" == *:* ]] || die "IMAGE='$IMAGE' has no tag. A tagless image becomes ':latest', which with imagePullPolicy=IfNotPresent means a node that already has one never picks up a rebuild."
[[ -f "$REPO_ROOT/Cargo.toml" ]] || die "'$REPO_ROOT' does not look like the repo root -- the Dockerfile COPYs paths relative to it"
command -v docker >/dev/null 2>&1 || die "docker not found. This script has to run ON the k3s node: it needs the Docker daemon to build and that node's containerd to import."

if [[ -n "${CTR:-}" ]]; then
    ctr_cmd=($CTR)
elif command -v k3s >/dev/null 2>&1; then
    ctr_cmd=(k3s ctr)
elif command -v ctr >/dev/null 2>&1; then
    ctr_cmd=(ctr)
else
    die "neither 'k3s' nor 'ctr' is on PATH, so the built image cannot be imported into containerd. On a k3s node k3s is usually at /usr/local/bin/k3s; set CTR to override (e.g. CTR='k3s ctr')."
fi

echo "=== docker build $IMAGE ==="
# The build context is the REPO ROOT, not this directory, because the
# Dockerfile's COPY paths are repo-relative -- matching qa-platform.Dockerfile's
# convention so the two files can be read the same way.
docker build \
    -f "$SCRIPT_DIR/runner.Dockerfile" \
    -t "$IMAGE" \
    "$REPO_ROOT"

echo "=== importing into containerd via ${ctr_cmd[*]} ==="
# Streamed through a pipe rather than a temp file: the image is ~250 MB and a
# node's /tmp is not always that big. `-` is `ctr images import`'s stdin form.
docker save "$IMAGE" | "${ctr_cmd[@]}" images import -

# READ IT BACK OUT OF CONTAINERD, not out of Docker. Docker having the image is
# what is already known; containerd having it is the thing this script exists to
# achieve, and `docker save | ctr import` can succeed on the pipe and still
# leave containerd with a differently-named reference (it imports whatever tags
# the tarball carries, and `docker save` writes them as
# `docker.io/library/<name>:<tag>` when the name is unqualified).
# The full `name:tag`, not `${IMAGE%%:*}`: the tag is the whole point (see
# this file's header), and matching on the repository alone is satisfied by
# a previously-imported DIFFERENT tag -- which is exactly the mismatch that
# produces ImagePullBackOff at workflow-submission time.
echo "=== containerd references matching $IMAGE ==="
listed="$("${ctr_cmd[@]}" images ls -q | grep -F "$IMAGE" || true)"
if [[ -z "$listed" ]]; then
    die "after import, containerd lists no image tagged '$IMAGE'. The import reported success, so check '${ctr_cmd[*]} images ls' by hand."
fi
printf '%s\n' "$listed"

# The kubelet resolves an unqualified image name to `docker.io/library/<name>`,
# which is exactly the reference `docker save` writes -- so the config value is
# the SHORT name, and it works because that expansion and this tarball agree.
# Stating it because the containerd listing above shows the long form, and
# copying that into the config also works but reads like a different image.
echo
echo "build-and-import: done. Set"
echo "build-and-import:   qa-runs.argo.runner_image: $IMAGE"
echo "build-and-import: and keep imagePullPolicy at IfNotPresent (qa-runs' default) -- there is no registry behind this image."
