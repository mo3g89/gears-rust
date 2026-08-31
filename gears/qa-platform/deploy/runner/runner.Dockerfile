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
# Pinned by digest-less tag on purpose: `python:3.12-slim` is the same base the
# source runner uses, and this image is rebuilt by hand on a dev host rather
# than by a pipeline that could pin one.
FROM python:3.12-slim

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
# kubectl tracks stable rather than a pin, matching the source system. That is a
# deliberate difference from the two pinned tools: kubectl is version-skew
# tolerant against the API server by one minor either way, and pinning it here
# would silently age against whatever cluster a platform points at.
#
# These add ~250 MB to a ~150 MB image. The alternative -- a second image for
# cluster-touching suites -- was rejected for a dev deployment with one runner
# image and no registry: `qa-runs.argo.runner_image` names exactly one.
ARG HELM_VERSION=v3.21.0
ARG ISTIO_VERSION=1.28.6
RUN set -eux; \
    curl -fsSLO "https://dl.k8s.io/release/$(curl -fsSL https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl"; \
    install -o root -g root -m 0755 kubectl /usr/local/bin/kubectl; \
    rm kubectl; \
    curl -fsSL -o helm.tar.gz "https://get.helm.sh/helm-${HELM_VERSION}-linux-amd64.tar.gz"; \
    tar -xzf helm.tar.gz; \
    install -o root -g root -m 0755 linux-amd64/helm /usr/local/bin/helm; \
    rm -rf helm.tar.gz linux-amd64; \
    curl -fsSL -o istioctl.tar.gz "https://github.com/istio/istio/releases/download/${ISTIO_VERSION}/istioctl-${ISTIO_VERSION}-linux-amd64.tar.gz"; \
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
