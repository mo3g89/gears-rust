# The qa-platform pytest runner: fetches a test bundle with a service-account
# token, unpacks it, runs pytest, and emits the log markers qa-runs' Argo
# adapter parses.
#
# WHY THIS IMAGE EXISTS RATHER THAN REUSING THE SOURCE SYSTEM'S. The source
# system's runner (`vhp-testrunner/runner/`, an 896-line entrypoint) downloads
# from `TEST_BUNDLE_URL` with NO authentication, because its manager's bundle
# route had no auth middleware at all. This deployment's route is
# `.authenticated()`, so a token has to be fetched first -- and that is the one
# behaviour the source runner cannot be configured into.
#
# WHY IT IS BUILT AND IMPORTED RATHER THAN PULLED. There is no registry in this
# deployment. `docker build` puts an image in the DOCKER daemon's store, which
# k3s' containerd cannot see; `deploy/runner/build-and-import.sh` does the
# `k3s ctr images import` step that makes it visible. `imagePullPolicy` on the
# workflow is `IfNotPresent` (qa-runs' default), which is what lets an imported
# image with no registry behind it be used at all -- `Always` would try to pull
# `docker.io/library/qa-platform-pytest-runner` and fail.
#
# Pinned by digest, with the tag beside it. `python:3.12-slim` is the same
# family the source runner uses; the exact patch release and the digest are
# recorded so two builds a week apart are the same image. The rationale and the
# bump procedure are in the PINS block further down, above the tooling install, which
# this line is part of -- one procedure for every pin in this file.
FROM python:3.12.14-slim@sha256:f77ac9e44ae96ef2c90b8053ea08c31f8be030f824196b0ae4db6d462c84e51f

# `pytest` alone. Every package added here has to exist inside an environment
# that may have no internet at build time, and the marker plugin is stdlib-only
# (`base64`, `json`, `os`, `sys`), as are the bundle fetch (`urllib`, `tarfile`,
# `ssl`) and the layout probe (`os`, `shlex`) -- specifically so this line stays
# one package long.
#
# THE SUITE'S OWN DEPENDENCIES ARE NOT BAKED IN, and that is a decision with a
# measurement behind it. `entrypoint.sh` installs the bundle's
# `requirements.txt` at run start; a pod in the `argo` namespace reached
# `https://pypi.org/simple/` in 0.47 s and installed the first real suite's ten
# requirements in 16.5 s. Baking a wheel set in instead would couple this image
# to one repository's dependency list and force an image rebuild-and-import for
# every change to it -- for a per-run cost of well under a minute on a suite
# whose own runtime is minutes. If a deployment ever has no index, the fix is a
# private index or a wheelhouse volume, not a `pip install` line here.
#
# The version is pinned: an unpinned `pip install pytest` makes the runner's
# behaviour a function of the day it was built, and the marker plugin uses
# report attributes (`longreprtext`, `wasxfail`) whose presence is a pytest
# API promise, not a guess.
RUN pip install --no-cache-dir pytest==8.3.4

# bash, because entrypoint.sh uses arrays and `[[`. `python:3.12-slim` ships
# dash as /bin/sh and no bash at all -- measured: `docker run --rm
# python:3.12-slim bash --version` exits 127. A `#!/bin/sh` rewrite was the
# alternative and was rejected: the file's array handling of TEST_FILES is what
# keeps a path with a space in it working.
#
# curl and ca-certificates are here for the cluster tooling below; git because
# the source system's runner has it in its base image and suites clone with it.
RUN apt-get update \
    && apt-get install -y --no-install-recommends bash curl ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

# CLUSTER TOOLING. The first real suite run against this image failed every
# test with
#   lib.k8s.ClusterUnreachable: kubectl get httproute -A -o json failed:
#   [Errno 2] No such file or directory: 'kubectl'
# because the suite shells out to these binaries rather than using a Python
# client. The source system installs the same three in its own runner
# (`vhp-testrunner/runner/Dockerfile`), which is the image this suite was
# written against, so the set and the pins are copied from there rather than
# chosen here.
#
# ISTIO_VERSION IS NOT FREE TO MOVE. The source Dockerfile pins it to match
# `tests/e2e/tests/lib/gateway.py:_VANILLA_ISTIOD_VERSION` in the suite's own
# repository; without `istioctl` on PATH that suite's `external_istiod_fixture`
# falls back to a bare `helm install istio/istiod` with no repo added and the
# adoption tests skip -- green, having tested nothing. If the suite's pin moves,
# this moves with it.
#
# kubectl IS PINNED, and its published SHA-256 is verified. It used to track
# `stable.txt`, which the source system does and which this file defended as
# version-skew tolerance. The skew argument is real and is kept below; the
# defect was that "whatever stable.txt says today" was then installed with no
# integrity check, so the binary was whatever the URL served.
#
# These add ~250 MB to a ~150 MB image. The alternative -- a second image for
# cluster-touching suites -- was rejected for a dev deployment with one runner
# image and no registry: `qa-runs.argo.runner_image` names exactly one.
#
# PINS: EVERY BINARY BELOW IS A VERSION PLUS A VERIFIED SHA-256, AND EVERY BASE
# IMAGE IS A TAG PLUS A DIGEST. Second review, finding #95.
#
# What drifts if they are not pinned:
#   * `stable.txt` moves whenever Kubernetes cuts a release, so kubectl -- and
#     with it the runner's behaviour against a cluster -- became a function of
#     the day the image was built, with no diff in this repository. The digest-
#     less `python:3.12-slim` (and `node:20-alpine`/`nginx:alpine` in
#     deploy/docker/qa-platform-ui.Dockerfile) drift the same way: two builds a
#     week apart are two images with one name.
#   * A version alone is not integrity. The three downloads trusted TLS to
#     dl.k8s.io, get.helm.sh and github.com and nothing else, though each
#     publishes a SHA-256 beside the artefact. A substituted or truncated
#     binary was caught only if it failed to execute.
#
# What broke: nothing yet. This closes a gap the review found; it is not the
# fix for an incident. The precedent is PDFIUM_VERSION in the root
# .cargo/config.toml, where an unpinned upstream fetch changed output on every
# open branch at once with no diff in the repo. Recorded plainly so the next
# reader does not go looking for a breakage that is not written down.
#
# Version skew, which is why kubectl used to float: kubectl is supported one
# minor either side of the API server. A pin ages, so KUBECTL_VERSION is moved
# when the cluster a platform points at moves a minor -- not when stable.txt
# does.
#
# To bump any of them (deliberately, in one commit, never one half alone):
#   kubectl: choose the version, then fetch its published checksum
#              curl -fsSL https://dl.k8s.io/release/<version>/bin/linux/amd64/kubectl.sha256
#            and change KUBECTL_VERSION and KUBECTL_SHA256 together.
#   helm:    HELM_SHA256 is in
#              https://get.helm.sh/helm-<version>-linux-amd64.tar.gz.sha256sum
#   istioctl: ISTIO_SHA256 is in
#              https://github.com/istio/istio/releases/download/<version>/istioctl-<version>-linux-amd64.tar.gz.sha256
#            (ISTIO_VERSION must still move with the suite's own pin, above.)
#   python base: pick the tag, then
#              docker buildx imagetools inspect python:<tag>
#            and replace the tag AND the top-level `Digest:` on the FROM line.
# Then run `make helm-tests` and
#   python3 gears/qa-platform/deploy/helm/tests/check_image_pins.py --online
# which re-fetches each checksum and re-resolves each tag, and finally rebuild
# and import the image (deploy/runner/build-and-import.sh). Reverting a pin
# because it is inconvenient reopens #95; raise it through this procedure.
ARG KUBECTL_VERSION=v1.37.1
ARG KUBECTL_SHA256=65691ff77eb6fa44c908b77a1082c9f092c3b9733b5cefabec0d1104890e21a8
ARG HELM_VERSION=v3.21.0
ARG ISTIO_VERSION=1.28.6
ARG HELM_SHA256=0093eb572e3d2380f094df162ddb525e219249de88957afe24cfbb19632acd36
ARG ISTIO_SHA256=e47f32c363e5fcd233a126f56d88897c9e0a92b7025c9e72deff813756c9f89e
RUN set -eux; \
    curl -fsSLO "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/amd64/kubectl"; \
    echo "${KUBECTL_SHA256}  kubectl" | sha256sum -c -; \
    install -o root -g root -m 0755 kubectl /usr/local/bin/kubectl; \
    rm kubectl; \
    curl -fsSL -o helm.tar.gz "https://get.helm.sh/helm-${HELM_VERSION}-linux-amd64.tar.gz"; \
    echo "${HELM_SHA256}  helm.tar.gz" | sha256sum -c -; \
    tar -xzf helm.tar.gz; \
    install -o root -g root -m 0755 linux-amd64/helm /usr/local/bin/helm; \
    rm -rf helm.tar.gz linux-amd64; \
    curl -fsSL -o istioctl.tar.gz "https://github.com/istio/istio/releases/download/${ISTIO_VERSION}/istioctl-${ISTIO_VERSION}-linux-amd64.tar.gz"; \
    echo "${ISTIO_SHA256}  istioctl.tar.gz" | sha256sum -c -; \
    tar -xzf istioctl.tar.gz; \
    install -o root -g root -m 0755 istioctl /usr/local/bin/istioctl; \
    rm -rf istioctl.tar.gz istioctl; \
    kubectl version --client=true; \
    helm version --short; \
    istioctl version --remote=false

WORKDIR /opt/qa-runner
COPY gears/qa-platform/deploy/runner/fetch_bundle.py /opt/qa-runner/fetch_bundle.py
COPY gears/qa-platform/deploy/runner/pytest_markers.py /opt/qa-runner/pytest_markers.py
COPY gears/qa-platform/deploy/runner/collect_reporter.py /opt/qa-runner/collect_reporter.py
COPY gears/qa-platform/deploy/runner/bundle_layout.py /opt/qa-runner/bundle_layout.py

# `/entrypoint.sh`, at the root, because that is the path
# `qa-runs.argo.runner_command` defaults to. A different path here means every
# deployment has to set that knob.
COPY gears/qa-platform/deploy/runner/entrypoint.sh /entrypoint.sh
RUN chmod 0755 /entrypoint.sh

# /work is where the bundle is unpacked and pytest's CWD. Created here rather
# than by the entrypoint so its ownership is the image's, which matters if a
# deployment ever runs this pod as a non-root user.
RUN mkdir -p /work
ENV QA_RUNNER_WORKDIR=/work

# No ENTRYPOINT/CMD that the workflow does not override: Argo sets `command`
# from `runner_command`, so anything declared here is dead weight that only
# matters when someone runs the image by hand -- which is worth supporting, so
# it is declared, and it is the same path the adapter uses.
ENTRYPOINT ["/entrypoint.sh"]
