# qa-platform in-cluster deployment — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run the whole qa-platform stack inside the remote k3s cluster from a Helm chart, retiring the docker-compose deployment on that host.

**Architecture:** A new chart at `gears/qa-platform/deploy/helm/qa-platform` deploys Postgres, Keycloak, the single gears container and the UI into namespace `qa-platform`. The UI pod is the edge on `hostPort: 443`; its nginx — already the reverse proxy for `/qa/v1/` and the SSE stream — gains a `/auth/` location for Keycloak, collapsing the five-pin login chain onto **one origin**. The gears reach Argo in namespace `argo` through a ServiceAccount and `Config::infer()` instead of a kubeconfig file.

**Tech Stack:** Helm, k3s v1.36.3, Kubernetes YAML, bash, nginx (`envsubst` templating), Keycloak 26.0, Postgres 16, Argo Workflows. Rust is not expected to change.

**Spec:** `gears/qa-platform/docs/superpowers/specs/2026-08-29-in-cluster-deployment-design.md`

## Global Constraints

* **Repo:** `/home/serhii/Jelastic/projects/fabric/gears-rust`, branch `feature/qa-platform-specs`. Push only with `./push-to-fork.sh` (personal fork). **Nothing goes to `origin`.**
* **Remote:** `root@10.136.20.200`, needs the VPN. A timeout is the tunnel, not the code.
* **`PUBLIC_ORIGIN` = `https://10.136.20.200`.** `PUBLIC_HOST` = `10.136.20.200` (bare host — `gen-cert.sh` refuses anything that is not a DNS name or IPv4 literal).
* **Cluster DNS ClusterIP = `10.43.0.10`** (measured on the target cluster).
* **Namespace `qa-platform`; Helm release `qa-platform`; Argo stays in `argo`.**
* **Helm version skew is real:** local `v3.18.1`, remote `v4.1.1`. Chart `apiVersion: v2` works on both. **Never assert byte-equality on whole `helm template` output** — assert on fields extracted with `python3`/PyYAML (6.0.2 available locally).
* **Never pipe a command whose exit code you need.** `cmd | tail` reports `tail`'s status. Redirect to a file and read `$?`. This has produced false green reports on this project more than once.
* **Never `docker compose down -v`.** Plain `down`/`stop` only.
* **No kubeconfig-derived value is ever formatted** into a message, log line or DTO field — everything goes through `qa-environments/src/infra/observer/errors.rs`.
* **ADR-0001:** no `kube`/`k8s-openapi` outside `qa-environments/src/infra/observer/`, never in `domain/`.
* Toolchain: `export PATH="$HOME/.cargo/bin:$PATH"`. Never `cargo test --all-targets` at workspace level. Clippy is `-D warnings`.
* `npm run lint` is broken repo-wide and `cargo fmt --check` is red on untouched files. **Both pre-existing — not findings.**
* **Do not run the user's test suite against the VHP cluster without asking.**
* Cargo feature lists, verbatim:
  * base — `qa-platform,oidc-authn,static-authz,tenant-resolver-rg,static-credstore,postgres-credstore,platform-observation`
  * argo — the base list plus `qa-runs-argo`

---

## File Structure

**Created**

| Path | Responsibility |
|---|---|
| `deploy/helm/qa-platform/Chart.yaml` | chart metadata |
| `deploy/helm/qa-platform/values.yaml` | every knob; `publicOrigin` is the single source of the login pins |
| `deploy/helm/qa-platform/templates/_helpers.tpl` | name helpers; **`publicHost`** derived from `publicOrigin` |
| `.../templates/postgres-*.yaml` | StatefulSet, headless Service, Secret, initdb ConfigMap |
| `.../templates/certs-job.yaml` | `pre-install` hook running `gen-cert.sh` |
| `.../templates/keycloak-*.yaml` | Deployment, Service, realm ConfigMap |
| `.../templates/gears-*.yaml` | Deployment, Service, PVC, config ConfigMap, argo fragments, ServiceAccount |
| `.../templates/rbac-argo.yaml` | Role + RoleBinding **in namespace `argo`** |
| `.../templates/ui-*.yaml` | Deployment (`hostPort`), Service, nginx extra-conf ConfigMap |
| `.../templates/jobs-*.yaml` | `db-migrate` and `tenant-seed` hook Jobs |
| `deploy/cargo-features.argo` | canonical argo feature list |
| `deploy/remote/lib.sh` | shared preflight/rsync helpers |
| `deploy/remote/deploy-k8s.sh` | the k8s deploy driver |
| `deploy/remote/verify-k8s.sh` | the verification suite |
| `deploy/helm/tests/test_pins.py` | the `PUBLIC_ORIGIN` guard |
| `deploy/helm/tests/test_nginx_template.sh` | proves the compose render is unchanged |
| `deploy/helm/tests/test_features.py` | base ⊆ argo subset guard |

**Modified**

| Path | Change |
|---|---|
| `qa-platform-ui/nginx.conf` → `qa-platform-ui/default.conf.template` | `envsubst` placeholders + a second `include` seam |
| `deploy/docker/qa-platform-ui.Dockerfile` | copy the template into `/etc/nginx/templates/` |
| ~~`deploy/compose/docker-compose.yml`~~ | **Not modified.** Task 2's Dockerfile `ENV` defaults reproduce the compose stack's current effective values, so the local stack needs no compose edit and its `docker compose config` render stays byte-identical. |
| `deploy/compose/docker-compose.argo.yml` | reads the feature list from `deploy/cargo-features.argo` |

---

## Task 1: Measure the pod → node `hostPort` hairpin

The spec's §4.5 names this as the one unmeasured assumption in the design. The gears must fetch OIDC discovery over HTTPS from the UI pod's `hostPort` on their own node. Settle it before any template depends on it.

**Files:**
- Create: `deploy/helm/NOTES-hairpin.md` (a five-line record of what was measured)

- [ ] **Step 1: Prove a `hostPort` pod is reachable from another pod via the node IP**

```bash
ssh root@10.136.20.200 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl run hairpin-target --image=nginx:alpine --restart=Never \
  --overrides="{\"spec\":{\"containers\":[{\"name\":\"nginx\",\"image\":\"nginx:alpine\",\"ports\":[{\"containerPort\":80,\"hostPort\":18080}]}]}}"
kubectl wait --for=condition=Ready pod/hairpin-target --timeout=90s'
```

- [ ] **Step 2: Curl the node IP from a second pod, capturing the exit code without a pipe**

```bash
ssh root@10.136.20.200 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl run hairpin-probe --image=curlimages/curl --restart=Never --rm -i --quiet -- \
  curl -sS -o /tmp/out -w "%{http_code}" --max-time 10 http://10.136.20.200:18080/ > /tmp/hairpin.rc 2>&1
echo "exit=$?"; cat /tmp/hairpin.rc'
```

Expected: `exit=0` and `200`. Anything else means the hairpin does not work.

- [ ] **Step 3: Record the result and set the default**

Write `deploy/helm/NOTES-hairpin.md` stating the date, the command, the observed exit code and HTTP status, and the conclusion. If the hairpin **worked**, `values.yaml` will later carry `gears.hostAliases.enabled: false`. If it **failed**, `true` — the gears PodSpec gets a `hostAliases` entry mapping `10.136.20.200` to the `ui` Service ClusterIP, which leaves the URL and therefore the `IP:` SAN check unchanged.

- [ ] **Step 4: Clean up the probe pods**

```bash
ssh root@10.136.20.200 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl delete pod hairpin-target --ignore-not-found'
```

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/deploy/helm/NOTES-hairpin.md
git commit -m "docs(qa-platform): measure the pod->node hostPort hairpin before designing around it"
```

---

## Task 2: Turn `nginx.conf` into an `envsubst` template with an extra-conf seam

`qa-platform-ui/nginx.conf` is `COPY`d to `/etc/nginx/conf.d/default.conf` (Dockerfile line 60). Two values in it are deployment-specific: `resolver 127.0.0.11 valid=10s ipv6=off;` (Docker's embedded DNS) and `set $gears http://gears:8087;`. The `nginx:alpine` image runs `envsubst` over `/etc/nginx/templates/*.template` at start-up, which lets **one** file serve both deployments instead of the chart carrying a 331-line copy that will drift.

The `/auth/` location does **not** go in this file — it would change the local compose stack's behaviour. It goes in a mounted fragment, via a second `include` seam matching the pattern `include /etc/nginx/tls-conf/*.conf;` already establishes: a wildcard include that matches nothing is legal in nginx and silent.

**Files:**
- Modify: `qa-platform-ui/nginx.conf` → renamed `qa-platform-ui/default.conf.template`
- Modify: `deploy/docker/qa-platform-ui.Dockerfile:60`
- Modify: `deploy/compose/docker-compose.yml` (`ui` service `environment:`)
- Create: `deploy/helm/tests/test_nginx_template.sh`

**Interfaces:**
- Produces: three env vars every consumer must set — `NGINX_RESOLVER`, `GEARS_UPSTREAM`, `KEYCLOAK_UPSTREAM` — plus `NGINX_ENVSUBST_FILTER`. Compose sets the first two to their current effective values; the chart sets all of them.

- [ ] **Step 1: Write the failing test**

`deploy/helm/tests/test_nginx_template.sh` — renders the template with compose's values and asserts the result is **byte-identical** to the nginx.conf that shipped before this task.

```bash
#!/usr/bin/env bash
# Proves the envsubst template still renders the compose stack's nginx.conf
# EXACTLY as it was, so templatising it changed no behaviour of the local stack.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UI="$HERE/../../../qa-platform-ui"
BASELINE="$HERE/fixtures/nginx.conf.compose-baseline"

export NGINX_RESOLVER='127.0.0.11'
export GEARS_UPSTREAM='http://gears:8087'

rendered="$(mktemp)"
envsubst '${NGINX_RESOLVER} ${GEARS_UPSTREAM}' \
    < "$UI/default.conf.template" > "$rendered"
rc=$?
[ "$rc" -eq 0 ] || { echo "FAIL: envsubst exited $rc"; exit 1; }

if diff -u "$BASELINE" "$rendered"; then
    echo "PASS: template renders the compose baseline byte-identically"
else
    echo "FAIL: rendered template differs from the compose baseline (above)"
    exit 1
fi

# The nginx runtime variables must survive envsubst untouched. $sse_authorization
# is the one that matters: substituting it away silently removes the SSE auth
# bridge and the stream then fails closed with no error naming this file.
for v in '$sse_authorization' '$arg_access_token' '$http_authorization' '$uri' '$gears'; do
    if ! grep -qF -- "$v" "$rendered"; then
        echo "FAIL: nginx runtime variable $v was eaten by envsubst"
        exit 1
    fi
done
echo "PASS: nginx runtime variables survived envsubst"
```

- [ ] **Step 2: Capture the baseline, then run the test to watch it fail**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform
mkdir -p deploy/helm/tests/fixtures
cp qa-platform-ui/nginx.conf deploy/helm/tests/fixtures/nginx.conf.compose-baseline
chmod +x deploy/helm/tests/test_nginx_template.sh
./deploy/helm/tests/test_nginx_template.sh > /tmp/t2.log 2>&1; echo "exit=$?"
```

Expected: non-zero — `default.conf.template` does not exist yet.

- [ ] **Step 3: Create the template**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/qa-platform-ui
git mv nginx.conf default.conf.template
```

Then make exactly three edits to `default.conf.template`:

```nginx
    # was: resolver 127.0.0.11 valid=10s ipv6=off;
    resolver ${NGINX_RESOLVER} valid=10s ipv6=off;

    # was: set $gears http://gears:8087;
    set $gears ${GEARS_UPSTREAM};
```

and, immediately after the existing `include /etc/nginx/tls-conf/*.conf;` line, add the second seam with a comment in the file's established voice:

```nginx
    # THE SECOND SEAM, same mechanism as tls-conf above and for the same reason:
    # a wildcard include matching nothing is legal in nginx and silent. Nothing
    # in this image populates it. The Helm chart mounts Keycloak's three root
    # prefixes here -- /realms/, /resources/, /admin/ -- so the IdP is served
    # from this server block, i.e. from the SAME ORIGIN as the SPA and the API.
    # That is what collapses the login chain's five pins to one.
    #
    # NOT `/auth/`, and that is not a style choice: the SPA already routes
    # /auth/callback as its OIDC redirect target, and an nginx `location /auth/`
    # shadows it -- the browser would come back from Keycloak straight into
    # Keycloak. Serving Keycloak at its DEFAULT root path also keeps
    # PUBLIC_ISSUER_ORIGIN a bare origin, which is the only shape
    # deploy/docker/entrypoint.sh:137 accepts (it refuses to start on a path).
    #
    # The compose stack mounts nothing here, so its behaviour is unchanged.
    include /etc/nginx/extra-conf/*.conf;
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform
./deploy/helm/tests/test_nginx_template.sh > /tmp/t2.log 2>&1; echo "exit=$?"; cat /tmp/t2.log
```

Expected: `exit=0`, two PASS lines. If the diff shows only the `include` line, update the baseline fixture — that line is an intended addition — and re-run.

- [ ] **Step 5: Break-test the envsubst filter**

Temporarily change `NGINX_ENVSUBST_FILTER` handling by rendering with a bare `envsubst` (no allow-list):

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform
envsubst < qa-platform-ui/default.conf.template > /tmp/unfiltered.conf
grep -c 'sse_authorization' /tmp/unfiltered.conf > /tmp/brk.txt 2>&1; echo "exit=$?"; cat /tmp/brk.txt
```

Expected: the count **drops** versus the filtered render — proving the allow-list is doing real work and is not decorative. Record the two counts in the commit message.

- [ ] **Step 6: Point the Dockerfile at the template directory**

Replace line 60 of `deploy/docker/qa-platform-ui.Dockerfile`:

```dockerfile
# nginx:alpine runs envsubst over /etc/nginx/templates/*.template at start-up and
# writes the result to /etc/nginx/conf.d/. NGINX_ENVSUBST_FILTER is an ALLOW-LIST:
# without it envsubst also eats nginx's own runtime variables -- $sse_authorization,
# $uri, $gears -- and the SSE auth bridge disappears with no error naming this file.
COPY default.conf.template /etc/nginx/templates/default.conf.template
ENV NGINX_ENVSUBST_FILTER='^(NGINX_RESOLVER|GEARS_UPSTREAM)$'
ENV NGINX_RESOLVER=127.0.0.11
ENV GEARS_UPSTREAM=http://gears:8087
```

The `ENV` defaults reproduce the compose stack's current effective values, so `docker-compose.yml` needs no change at all and the local stack keeps working untouched.

- [ ] **Step 7: Verify the compose UI image still builds and serves**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust
docker build -f gears/qa-platform/deploy/docker/qa-platform-ui.Dockerfile \
    -t cf-gears-qa-platform-ui:t2 gears/qa-platform/qa-platform-ui > /tmp/build.log 2>&1
echo "exit=$?"; tail -5 /tmp/build.log
docker run --rm cf-gears-qa-platform-ui:t2 nginx -t > /tmp/nginxt.log 2>&1; echo "exit=$?"; cat /tmp/nginxt.log
```

Expected: both `exit=0`; `nginx -t` reports the configuration file test is successful.

- [ ] **Step 8: Commit**

```bash
git add gears/qa-platform/qa-platform-ui/default.conf.template \
        gears/qa-platform/deploy/docker/qa-platform-ui.Dockerfile \
        gears/qa-platform/deploy/helm/tests/
git commit -m "refactor(qa-platform-ui): templatise nginx.conf and add an extra-conf seam"
```

---

## Task 3: One canonical argo feature list, with a subset guard

The compose overlay's own header records that duplicating this list has already bitten once: `postgres-credstore` was added to the Dockerfile `ARG` and would have been absent from every `--argo` build while looking deployed. A k8s build step would make it a third copy.

Docker cannot read a file into an `ARG` default, so the honest fix is **two** places with an **enforced** relationship, not one place: the Dockerfile `ARG` stays the canonical *base* list, `deploy/cargo-features.argo` becomes the canonical *argo* list, and a test fails if base is not a subset of argo.

**Files:**
- Create: `deploy/cargo-features.argo`
- Create: `deploy/helm/tests/test_features.py`
- Modify: `deploy/compose/docker-compose.argo.yml`

**Interfaces:**
- Produces: `deploy/cargo-features.argo` — one line, comma-separated, no trailing newline sensitivity. Read by `docker-compose.argo.yml` (via `render-argo.sh`'s `.env`) and by `deploy-k8s.sh` (Task 11).

- [ ] **Step 1: Write the failing test**

`deploy/helm/tests/test_features.py`:

```python
"""The Dockerfile's ARG default is the canonical BASE feature list;
deploy/cargo-features.argo is the canonical ARGO list. Docker cannot read a file
into an ARG default, so the two cannot be collapsed into one -- this test enforces
the relationship instead: every base feature must appear in the argo list.

The drift this guards against has happened: `postgres-credstore` was added to the
Dockerfile ARG and would have been absent from every --argo build while looking
deployed (see docker-compose.argo.yml's header)."""
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
DEPLOY = HERE.parent.parent
DOCKERFILE = DEPLOY / "docker" / "qa-platform.Dockerfile"
ARGO_FILE = DEPLOY / "cargo-features.argo"


def base_features():
    text = DOCKERFILE.read_text()
    m = re.search(r"^ARG CARGO_FEATURES=(\S+)$", text, re.M)
    assert m, f"no `ARG CARGO_FEATURES=` line in {DOCKERFILE}"
    return [f.strip() for f in m.group(1).split(",") if f.strip()]


def argo_features():
    return [f.strip() for f in ARGO_FILE.read_text().strip().split(",") if f.strip()]


def main():
    base, argo = base_features(), argo_features()
    missing = [f for f in base if f not in argo]
    if missing:
        print(f"FAIL: base features absent from cargo-features.argo: {missing}")
        return 1
    if "qa-runs-argo" not in argo:
        print("FAIL: cargo-features.argo does not enable qa-runs-argo")
        return 1
    print(f"PASS: {len(base)} base features all present; argo list adds "
          f"{sorted(set(argo) - set(base))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform
python3 deploy/helm/tests/test_features.py > /tmp/t3.log 2>&1; echo "exit=$?"; cat /tmp/t3.log
```

Expected: non-zero, a `FileNotFoundError` for `cargo-features.argo`.

- [ ] **Step 3: Create the canonical file**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform
printf 'qa-platform,oidc-authn,static-authz,tenant-resolver-rg,static-credstore,postgres-credstore,platform-observation,qa-runs-argo\n' > deploy/cargo-features.argo
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
python3 deploy/helm/tests/test_features.py > /tmp/t3.log 2>&1; echo "exit=$?"; cat /tmp/t3.log
```

Expected: `exit=0`, `PASS: 7 base features all present; argo list adds ['qa-runs-argo']`.

- [ ] **Step 5: Break-test the guard**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform
cp deploy/cargo-features.argo /tmp/feat.bak
printf 'qa-platform,oidc-authn,qa-runs-argo\n' > deploy/cargo-features.argo
python3 deploy/helm/tests/test_features.py > /tmp/t3b.log 2>&1; echo "exit=$?"; cat /tmp/t3b.log
cp /tmp/feat.bak deploy/cargo-features.argo
```

Expected: non-zero, naming the five dropped features. Restore, then re-run to confirm green again.

- [ ] **Step 6: Point the compose overlay at the file**

In `deploy/compose/docker-compose.argo.yml`, replace the literal default with a required variable and extend the existing comment block to say where the value now comes from:

```yaml
        # THE LIST IS NO LONGER DUPLICATED HERE. render-argo.sh reads
        # deploy/cargo-features.argo and writes CARGO_FEATURES into the stack's
        # .env; deploy/helm/tests/test_features.py fails if the Dockerfile's ARG
        # base list is not a subset of that file. That is the drift this block
        # used to warn about, now enforced rather than described.
        CARGO_FEATURES: ${CARGO_FEATURES:?run deploy/compose/render-argo.sh first -- it writes CARGO_FEATURES from deploy/cargo-features.argo}
```

- [ ] **Step 7: Make `render-argo.sh` write it**

Add to `deploy/compose/render-argo.sh`, before it finishes, a block that appends `CARGO_FEATURES` to the generated `.env` it already manages, reading `../cargo-features.argo`. Then verify:

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/compose
grep -n 'CARGO_FEATURES' render-argo.sh > /tmp/t3c.log 2>&1; echo "exit=$?"; cat /tmp/t3c.log
```

Expected: the new line is present.

- [ ] **Step 8: Commit**

```bash
git add gears/qa-platform/deploy/cargo-features.argo \
        gears/qa-platform/deploy/helm/tests/test_features.py \
        gears/qa-platform/deploy/compose/docker-compose.argo.yml \
        gears/qa-platform/deploy/compose/render-argo.sh
git commit -m "refactor(qa-platform): one canonical argo feature list with a subset guard"
```

---

## Task 4: Chart skeleton and the Postgres tier

**Files:**
- Create: `deploy/helm/qa-platform/Chart.yaml`, `values.yaml`, `templates/_helpers.tpl`
- Create: `deploy/helm/qa-platform/templates/postgres-secret.yaml`, `postgres-initdb-configmap.yaml`, `postgres-statefulset.yaml`, `postgres-service.yaml`

**Interfaces:**
- Produces: `qa-platform.publicHost` helper (bare host parsed out of `.Values.publicOrigin`); Service `qa-platform-postgres:5432`; Secret `qa-platform-postgres` with keys `POSTGRES_USER`, `POSTGRES_PASSWORD`. Every later task consumes these names.

- [ ] **Step 1: Write `Chart.yaml`**

```yaml
apiVersion: v2
name: qa-platform
description: The qa-platform stack (gears, UI, Keycloak, Postgres) running in-cluster
type: application
version: 0.1.0
appVersion: "0.1.0"
```

No `dependencies:` — Argo Workflows is already installed in the `argo` namespace and this chart does not own it.

- [ ] **Step 2: Write `values.yaml`**

```yaml
# THE ONE KNOB THAT RE-POINTS THE WHOLE LOGIN CHAIN. Keycloak's KC_HOSTNAME, the
# gears' issuer_pattern, the realm's redirectUris/webOrigins, the UI bundle's
# baked-in VITE_OIDC_ISSUER and the certificate's SAN all derive from this, and
# deploy/helm/tests/test_pins.py fails if any of them stops doing so.
publicOrigin: https://10.136.20.200

# nginx `resolver` needs an ADDRESS, not a name -- the kube-dns Service ClusterIP.
# Measured on the target cluster 2026-08-29.
clusterDns: 10.43.0.10

argo:
  namespace: argo
  workflowServiceAccount: argo-workflow

images:
  gears:
    repository: cf-gears-qa-platform
    tag: latest
    pullPolicy: IfNotPresent
  ui:
    repository: cf-gears-qa-platform-ui
    tag: latest
    pullPolicy: IfNotPresent
  postgres:
    repository: postgres
    tag: "16"
  keycloak:
    repository: quay.io/keycloak/keycloak
    tag: "26.0"

postgres:
  user: qa
  password: qa
  persistence:
    size: 20Gi
    storageClass: ""

gears:
  replicaCount: 1
  port: 8087
  logLevel: info
  persistence:
    size: 20Gi
  # Set true only if Task 1 measured the pod->node hostPort hairpin as broken.
  hostAliases:
    enabled: false

ui:
  replicaCount: 1
  hostPort:
    http: 80
    https: 443

keycloak:
  # Keycloak runs at its DEFAULT root path. Do not add KC_HTTP_RELATIVE_PATH:
  # entrypoint.sh:137 refuses to start on a PUBLIC_ISSUER_ORIGIN carrying a path,
  # and the SPA already owns /auth/callback. See ui.keycloakPrefixes.
  adminUser: admin
  adminPassword: admin

# Set by deploy-k8s.sh with --set-file, from render-realm.sh's output. Keycloak
# 26.0.8 will not expand a placeholder in an import file -- measured, it aborts
# start-up -- so the realm has to arrive already rendered for this origin.
keycloakRealmJson: ""

# Only consumed when gears.hostAliases.enabled is true (i.e. only if Task 1
# measured the pod->node hostPort hairpin as broken). deploy-k8s.sh resolves the
# ui Service's ClusterIP and passes it with --set.
uiServiceClusterIP: ""
```

- [ ] **Step 3: Write `_helpers.tpl` with the host-derivation helper and a failing guard**

```yaml
{{/* The bare host out of publicOrigin. gen-cert.sh REFUSES anything that is not a
     plain DNS hostname or IPv4 literal, so this strips scheme and any port and
     fails loudly rather than writing a bad value into a certificate's SANs. */}}
{{- define "qa-platform.publicHost" -}}
{{- $o := required "publicOrigin is required" .Values.publicOrigin -}}
{{- $noScheme := regexReplaceAll "^https?://" $o "" -}}
{{- $host := regexReplaceAll ":[0-9]+$" $noScheme "" -}}
{{- if or (contains "/" $host) (eq $host "") -}}
{{- fail (printf "publicOrigin %q must be scheme://host[:port] with no path" $o) -}}
{{- end -}}
{{- $host -}}
{{- end -}}

{{/* entrypoint.sh appends `/realms/qa-platform` to PUBLIC_ISSUER_ORIGIN itself,
     so the gears get the BARE ORIGIN. This helper is the full issuer, used for
     the UI build arg and the verification checks. */}}
{{- define "qa-platform.issuer" -}}
{{- printf "%s/realms/qa-platform" .Values.publicOrigin -}}
{{- end -}}
```

- [ ] **Step 4: Write the Postgres objects**

`postgres-secret.yaml`, `postgres-service.yaml` (headless, `clusterIP: None`, port 5432), and `postgres-statefulset.yaml` with a `volumeClaimTemplate` of `.Values.postgres.persistence.size` on `.Values.postgres.persistence.storageClass`. `postgres-initdb-configmap.yaml` carries `deploy/compose/initdb/01-create-qa-platform-databases.sh` verbatim, mounted at `/docker-entrypoint-initdb.d`.

That script creates the **eight gear databases** the config names — `qa_environments`, `qa_catalog`, `qa_runs`, `qa_insights`, `settings`, `credstore`, `resource_group`, `event_broker`. The gears never create their own (`libs/toolkit-db/src/options.rs` only does `opts.database(dbname)`), so a missing one is a connection error at gear init. Keycloak needs no database here — it runs `start-dev` on embedded H2 (spec §3.3).

Add a readiness probe using `pg_isready -U <user>`, matching what compose's healthcheck does.

- [ ] **Step 5: Lint and render**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/helm
helm lint qa-platform > /tmp/t4a.log 2>&1; echo "lint exit=$?"; cat /tmp/t4a.log
helm template qa-platform qa-platform > /tmp/t4b.yaml 2>&1; echo "template exit=$?"; tail -20 /tmp/t4b.yaml
```

Expected: both `exit=0`.

- [ ] **Step 6: Verify the host helper against a bad origin**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/helm
helm template qa-platform qa-platform --set publicOrigin=https://10.136.20.200/ui > /tmp/t4c.log 2>&1
echo "exit=$?"; grep -c 'must be scheme://host' /tmp/t4c.log
```

Expected: non-zero exit, the `fail` message present. This is the break-test for the helper.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/deploy/helm/qa-platform
git commit -m "feat(qa-platform): helm chart skeleton and the postgres tier"
```

---

## Task 5: Certificates and the realm ConfigMap

**Files:**
- Create: `deploy/helm/qa-platform/templates/certs-job.yaml`, `certs-scripts-configmap.yaml`
- Create: `deploy/helm/qa-platform/templates/keycloak-realm-configmap.yaml`

**Interfaces:**
- Produces: Secret `qa-platform-tls` with keys `ca.crt`, `ca.key`, `ui.crt`, `ui.key`, `keycloak.crt`, `keycloak.key`; ConfigMap `qa-platform-realm` with key `realm-qa-platform.json`.

- [ ] **Step 1: Mount `gen-cert.sh` unchanged**

`certs-scripts-configmap.yaml` embeds `deploy/compose/keycloak-tls/gen-cert.sh` with `.Files.Get`. The script is **not edited**: it reads `TLS_DIR` (default `/tls`) and `PUBLIC_HOST`, and its comments record TLS lessons that were paid for once (notably why a single self-signed certificate used as its own trust anchor fails with `CaUsedAsEndEntity`).

It emits two leaves. Only `ca.crt`, `ui.crt` and `ui.key` get mounted anywhere; the Keycloak leaf is written into the Secret and left unused, because deleting an output from a proven script is not worth the risk.

- [ ] **Step 2: Write the pre-install hook Job**

```yaml
metadata:
  annotations:
    "helm.sh/hook": pre-install,pre-upgrade
    "helm.sh/hook-weight": "-10"
    "helm.sh/hook-delete-policy": before-hook-creation
```

The Job runs an image that has both `openssl` and `kubectl`, writes to an `emptyDir` at `/tls`, then creates the Secret with `kubectl create secret generic qa-platform-tls --from-file=/tls --dry-run=client -o yaml | kubectl apply -f -`. **Write that to a file and check `$?` rather than trusting the pipe's status** — per the global constraint, the pipeline's exit code is `kubectl apply`'s only by luck.

It needs a ServiceAccount with `create`/`patch` on `secrets` in its own namespace; grant that in this template, scoped to `qa-platform` only.

`PUBLIC_HOST` comes from `include "qa-platform.publicHost" .`

- [ ] **Step 3: Render the realm at package time**

`keycloak-realm-configmap.yaml` embeds the output of `render-realm.sh`. That script takes `PUBLIC_UI_ORIGIN` as its **first positional argument** as an origin (`http(s)://host[:port]`, no path, no trailing slash) and `OUT_DIR` as its second; it needs `jq`; and it keeps `http://localhost:8080` alongside the origin it is given.

Keycloak 26.0.8 will not expand a placeholder in an import file — measured, it aborts start-up — so the file must be rendered before it becomes a ConfigMap. `deploy-k8s.sh` (Task 11) runs `render-realm.sh "$PUBLIC_ORIGIN" <out>` and passes the result with `--set-file`.

- [ ] **Step 4: Verify the realm renders for the single origin**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/compose
./render-realm.sh 'https://10.136.20.200' /tmp/realm-out > /tmp/t5.log 2>&1; echo "exit=$?"; tail -20 /tmp/t5.log
jq -r '.clients[] | select(.clientId=="qa-platform-ui") | .redirectUris, .webOrigins' /tmp/realm-out/realm-qa-platform.json
```

Expected: `exit=0`, PASS lines, and `https://10.136.20.200` present in both lists alongside the localhost entries. Confirm no `"*"` appears in `webOrigins` — the script already fails on that, and this confirms the check ran.

- [ ] **Step 5: Confirm the workflow client secret is still pinned**

```bash
jq -r '.clients[] | select(.clientId=="qa-platform-workflow") | if .secret then "SET" else "ABSENT" end' /tmp/realm-out/realm-qa-platform.json
```

Expected: `SET`. This is what makes Keycloak's ephemeral H2 store safe across pod restarts (spec §3.3) — if this ever prints `ABSENT`, the design's premise is gone and `provision-workflow-secret.sh` must be re-run on every restart.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/deploy/helm/qa-platform/templates
git commit -m "feat(qa-platform): dev CA job and rendered realm configmap"
```

---

## Task 6: Keycloak

**Files:**
- Create: `deploy/helm/qa-platform/templates/keycloak-deployment.yaml`, `keycloak-service.yaml`

**Interfaces:**
- Produces: Service `qa-platform-keycloak:8080` (plain HTTP), serving at its **default root path**. The `/realms/`, `/resources/` and `/admin/` prefixes are proxied to it by the UI nginx in Task 8; Keycloak itself is given no relative path.

- [ ] **Step 1: Write the Deployment**

`command: ["start-dev", "--import-realm"]`, image `quay.io/keycloak/keycloak:26.0`, env:

```yaml
- name: KC_BOOTSTRAP_ADMIN_USERNAME
  value: {{ .Values.keycloak.adminUser | quote }}
- name: KC_BOOTSTRAP_ADMIN_PASSWORD
  value: {{ .Values.keycloak.adminPassword | quote }}
- name: KC_HTTP_ENABLED
  value: "true"
- name: KC_PROXY_HEADERS
  value: "xforwarded"
- name: KC_HOSTNAME
  value: {{ .Values.publicOrigin | quote }}
- name: KC_HOSTNAME_BACKCHANNEL_DYNAMIC
  value: "true"
```

`KC_HTTPS_CERTIFICATE_FILE` and `KC_HTTPS_CERTIFICATE_KEY_FILE` are **deliberately absent** — the only TLS in this deployment is the UI nginx's. Mount the realm ConfigMap at `/opt/keycloak/data/import`.

`KC_HTTP_RELATIVE_PATH` is **deliberately absent** — Keycloak stays at its default root path so `PUBLIC_ISSUER_ORIGIN` remains a bare origin (`entrypoint.sh:137` refuses to start on anything else) and so nothing shadows the SPA's own `/auth/callback` route.

Readiness probe on `/realms/qa-platform/.well-known/openid-configuration`, not `/` — the same reasoning legacy's chart records for probing a cheap endpoint.

- [ ] **Step 2: Render and check the pins land**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/helm
helm template qa-platform qa-platform > /tmp/t6.yaml 2>&1; echo "exit=$?"
grep -n 'KC_HOSTNAME\|KC_HTTP_RELATIVE_PATH\|KC_PROXY_HEADERS' -A1 /tmp/t6.yaml
```

Expected: `KC_HOSTNAME` is `https://10.136.20.200` exactly — no path, no port. No `KC_HTTPS_` key and no `KC_HTTP_RELATIVE_PATH` appears anywhere.

- [ ] **Step 3: Commit**

```bash
git add gears/qa-platform/deploy/helm/qa-platform/templates
git commit -m "feat(qa-platform): keycloak behind the UI nginx at /auth"
```

---

## Task 7: The gears, their ServiceAccount, and cross-namespace Argo RBAC

**Files:**
- Create: `deploy/helm/qa-platform/templates/gears-deployment.yaml`, `gears-service.yaml`, `gears-pvc.yaml`, `gears-config-configmap.yaml`, `gears-argo-configmaps.yaml`, `gears-serviceaccount.yaml`, `rbac-argo.yaml`

**Interfaces:**
- Produces: ServiceAccount `qa-platform-gears` in namespace `qa-platform`; Role and RoleBinding `qa-platform-gears-executor` in namespace `argo`; Service `qa-platform-gears:8087`.

**Naming note, and it matters:** the existing `qa-runs` ServiceAccount and `qa-runs-executor` Role/RoleBinding in `argo` were created with `kubectl apply` and carry **no Helm ownership metadata** (verified 2026-08-29 — only a `last-applied-configuration` annotation). Helm refuses to adopt such resources with "invalid ownership metadata". The chart therefore uses **new names**, and the old trio is deleted as an operator step in Task 13. The new name is also more accurate: the account serves all four gears in one pod, not qa-runs alone.

- [ ] **Step 1: Write the ServiceAccount and cross-namespace RBAC**

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: qa-platform-gears-executor
  namespace: {{ .Values.argo.namespace }}
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: qa-platform-gears-executor
subjects:
  - kind: ServiceAccount
    name: qa-platform-gears
    namespace: {{ .Release.Namespace }}
```

The Role's rules are `deploy/argo/qa-runs-rbac.yaml`'s, unchanged: `workflows` get/list/watch/create/patch; `pods` get/list; `pods/log` get. **No `delete`** (`ttlStrategy.secondsAfterCompletion` reclaims workflows), **no `secrets`** (the adapter emits a `secretKeyRef` and lets the kubelet resolve it — that is what keeps `run_executor.rs:85-86` true), no `configmaps`, no `cronworkflows`.

- [ ] **Step 2: Write the two Argo config fragments as ConfigMaps**

Same two anchors `entrypoint.sh` inserts at, but **without `kubeconfig_path`** — an empty path means `Config::infer()`, which picks up the projected ServiceAccount token. `entrypoint.sh` still refuses to start if `QA_RUNS_ARGO_CONFIG` is set and `QA_ENVIRONMENTS_ARGO_CONFIG` is not, and that refusal is kept deliberately: an Argo stack that submits Workflows but never writes their kubeconfig Secret fails in a way that names neither.

- [ ] **Step 3: Write the Deployment**

One container from `cf-gears-qa-platform`, `serviceAccountName: qa-platform-gears`, env `POSTGRES_HOST: qa-platform-postgres`, `POSTGRES_USER`/`POSTGRES_PASSWORD` from the Secret, `PUBLIC_ISSUER_ORIGIN: {{ include "qa-platform.issuer" . }}` minus the realm suffix as `entrypoint.sh` expects it, `QA_RUNS_ARGO_CONFIG` and `QA_ENVIRONMENTS_ARGO_CONFIG` pointing at the mounted fragments, `RUST_LOG` from `.Values.gears.logLevel`.

Mounts: the stack config ConfigMap, both fragments, the PVC at `/var/lib/cf-gears/data` (qa-catalog's clones), and `ca.crt` from the `qa-platform-tls` Secret so the gears trust the dev CA when they fetch discovery.

Gate the `hostAliases` block on `.Values.gears.hostAliases.enabled`, using what Task 1 measured:

```yaml
{{- if .Values.gears.hostAliases.enabled }}
      hostAliases:
        - ip: {{ .Values.uiServiceClusterIP | required "uiServiceClusterIP required when hostAliases.enabled" }}
          hostnames: [{{ include "qa-platform.publicHost" . | quote }}]
{{- end }}
```

- [ ] **Step 4: Render and verify the RBAC namespaces**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/helm
helm template qa-platform qa-platform --namespace qa-platform > /tmp/t7.yaml 2>&1; echo "exit=$?"
python3 - <<'PY'
import yaml
docs=[d for d in yaml.safe_load_all(open('/tmp/t7.yaml')) if d]
sa=[d for d in docs if d['kind']=='ServiceAccount' and d['metadata']['name']=='qa-platform-gears']
rb=[d for d in docs if d['kind']=='RoleBinding' and d['metadata']['name']=='qa-platform-gears-executor']
assert sa, "no gears ServiceAccount"
assert rb, "no RoleBinding"
assert rb[0]['metadata']['namespace']=='argo', rb[0]['metadata']
assert rb[0]['subjects'][0]['namespace']=='qa-platform', rb[0]['subjects']
role=[d for d in docs if d['kind']=='Role' and d['metadata']['name']=='qa-platform-gears-executor'][0]
verbs={r['resources'][0]: set(r['verbs']) for r in role['rules']}
assert 'delete' not in verbs['workflows'], "delete must not be granted"
assert 'secrets' not in verbs, "secrets must not be granted"
print("PASS: cross-namespace binding correct, no delete, no secrets")
PY
```

Expected: `exit=0` then `PASS`.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/deploy/helm/qa-platform/templates
git commit -m "feat(qa-platform): gears deployment with in-cluster argo access via ServiceAccount"
```

---

## Task 8: The UI as the edge

**Files:**
- Create: `deploy/helm/qa-platform/templates/ui-deployment.yaml`, `ui-service.yaml`, `ui-extraconf-configmap.yaml`

**Interfaces:**
- Produces: Service `qa-platform-ui:80`; a pod holding `hostPort` 80 and 443 on the node; ConfigMap `qa-platform-ui-extraconf` mounted at `/etc/nginx/extra-conf`.

- [ ] **Step 1: Write the `/auth/` fragment**

Keycloak runs at its default root path, so it owns three URL prefixes and the fragment
proxies exactly those three. It must NOT be `/auth/`: the SPA already routes `/auth/callback`
as its OIDC redirect target, and an nginx `location /auth/` shadows it.

```nginx
# Mounted into the seam Task 2 added to default.conf.template. Keycloak is served
# from THIS server block, so the SPA, the API and the IdP share ONE ORIGIN -- which
# is what removes port 8443, the second issuer and the second certificate.
#
# THREE PREFIXES, because Keycloak runs at its default root path:
#   /realms/    -- OIDC discovery, authorize, token, JWKS. The login flow itself.
#   /resources/ -- the login page's own CSS/JS. Without it the page renders bare.
#   /admin/     -- the admin API provision-workflow-secret.sh calls.
# Verified 2026-08-29 that no SPA route collides with any of the three.
#
# The X-Forwarded-* headers are not optional: Keycloak runs with
# KC_PROXY_HEADERS=xforwarded and builds its issuer and redirect URLs from them.
# Without X-Forwarded-Proto it advertises http:// and every token's `iss` stops
# matching the gears' issuer_pattern.
location ~ ^/(realms|resources|admin)/ {
    proxy_pass http://qa-platform-keycloak:8080;
    proxy_set_header Host              $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Forwarded-Host  $host;
    proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
    proxy_set_header X-Real-IP         $remote_addr;
}
```

**This is a regex location, and that matters here.** `default.conf.template` already carries a
regex location for the SSE stream, and its own comment warns that in nginx a matching regex
location beats a matching prefix location. This fragment's regex is checked against
`location /qa/v1/` (a prefix) and the SSE regex: the three prefixes above overlap neither.

The upstream is a Helm-rendered literal, not `${KEYCLOAK_UPSTREAM}` — envsubst processes only
`/etc/nginx/templates/*.template`, never a mounted fragment, so a placeholder here would reach
nginx unexpanded.

- [ ] **Step 2: Write the Deployment**

`hostPort` 80 and 443 alongside `containerPort`; env `NGINX_RESOLVER: {{ .Values.clusterDns }}` and `GEARS_UPSTREAM: http://qa-platform-gears:8087`. No `KEYCLOAK_UPSTREAM` — the fragment carries its upstream as a literal. Mount `ui-tls/tls.conf` as a ConfigMap at `/etc/nginx/tls-conf` and `ui.crt`/`ui.key` from the `qa-platform-tls` Secret — using `items:` to select only those two keys, never the whole Secret, for the reason the compose file spells out: the volume also holds `ca.key`, and `readOnly` stops writes, not reads.

- [ ] **Step 3: Render and verify the edge wiring**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/helm
helm template qa-platform qa-platform > /tmp/t8.yaml 2>&1; echo "exit=$?"
python3 - <<'PY'
import yaml
docs=[d for d in yaml.safe_load_all(open('/tmp/t8.yaml')) if d]
dep=[d for d in docs if d['kind']=='Deployment' and 'ui' in d['metadata']['name']][0]
c=dep['spec']['template']['spec']['containers'][0]
ports={p.get('hostPort') for p in c['ports']}
assert {80,443} <= ports, ports
env={e['name']:e.get('value') for e in c['env']}
assert env['NGINX_RESOLVER']=='10.43.0.10', env
vols=[v for v in c['volumeMounts']]
assert any(v['mountPath']=='/etc/nginx/extra-conf' for v in vols), vols
sec=[v for v in dep['spec']['template']['spec']['volumes'] if v.get('secret')]
keys={i['key'] for v in sec for i in v['secret'].get('items',[])}
assert 'ca.key' not in keys, f"ca.key must never be mounted into the UI pod: {keys}"
print("PASS: edge wiring correct; ca.key not mounted")
PY
```

Expected: `exit=0` then `PASS`.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/deploy/helm/qa-platform/templates
git commit -m "feat(qa-platform): UI pod as the edge on hostPort 443, serving /auth"
```

---

## Task 9: The migrate and seed hook Jobs

**Files:**
- Create: `deploy/helm/qa-platform/templates/job-db-migrate.yaml`, `job-tenant-seed.yaml`, `seed-scripts-configmap.yaml`

- [ ] **Step 1: Write `db-migrate`**

Hook `post-install,post-upgrade`, weight `0`, `hook-delete-policy: before-hook-creation`. Argv exactly as compose runs it:

```yaml
command:
  - /usr/local/bin/cf-gears-example-server
  - --config
  - /etc/cf-gears/qa-platform-stack.yaml
  - migrate
```

with the same `POSTGRES_*` env and config mount the gears Deployment uses.

- [ ] **Step 2: Write `tenant-seed`**

Hook `post-install,post-upgrade`, weight `10` — so it runs after migrate, reproducing compose's `service_completed_successfully` ordering. Image `postgres:16`, **not** the gears image: it runs `psql`, which the gears image does not carry. `deploy/compose/seed-tenant.sh` goes in a ConfigMap, entrypoint `["bash", "/scripts/seed-tenant.sh"]`, env `PGHOST`/`PGUSER`/`PGPASSWORD`.

- [ ] **Step 3: Verify ordering and images render correctly**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/helm
helm template qa-platform qa-platform > /tmp/t9.yaml 2>&1; echo "exit=$?"
python3 - <<'PY'
import yaml
docs=[d for d in yaml.safe_load_all(open('/tmp/t9.yaml')) if d]
jobs={d['metadata']['name']: d for d in docs if d['kind']=='Job'}
mig=[v for k,v in jobs.items() if 'migrate' in k][0]
seed=[v for k,v in jobs.items() if 'seed' in k][0]
w=lambda j: int(j['metadata']['annotations']['helm.sh/hook-weight'])
assert w(mig) < w(seed), (w(mig), w(seed))
assert 'postgres' in seed['spec']['template']['spec']['containers'][0]['image']
print("PASS: migrate runs before seed; seed uses the postgres image")
PY
```

Expected: `exit=0` then `PASS`.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/deploy/helm/qa-platform/templates
git commit -m "feat(qa-platform): db-migrate and tenant-seed as helm hook jobs"
```

---

## Task 10: The `PUBLIC_ORIGIN` pin guard

This is the test the spec calls the one that matters. The failure it guards against — the pins disagreeing — has cost this project multiple sessions, and a chart is the first artefact where it can be caught **before** a deploy.

**Files:**
- Create: `deploy/helm/tests/test_pins.py`

- [ ] **Step 1: Write the failing test**

```python
"""Every login pin must derive from ONE value: .Values.publicOrigin.

The pins, and what breaks when one drifts:
  * Keycloak KC_HOSTNAME      -> tokens carry an `iss` the gears reject (401)
  * gears issuer_pattern      -> same, from the other side
  * realm webOrigins          -> "Invalid parameter: redirect_uri"
  * certificate SAN           -> a browser refusal MID-REDIRECT, which reads as
                                 a broken login rather than a certificate warning
The UI bundle's VITE_OIDC_ISSUER is the fifth pin and is NOT checkable here --
vite inlines it at image build. verify-k8s.sh greps the deployed bundle for it."""
import subprocess
import sys
import pathlib
import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
ORIGIN = "https://example-pin-test.invalid"
HOST = "example-pin-test.invalid"


def render(origin):
    out = subprocess.run(
        ["helm", "template", "qa-platform", str(CHART), "--set", f"publicOrigin={origin}"],
        capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    return [d for d in yaml.safe_load_all(out.stdout) if d]


def main():
    docs = render(ORIGIN)
    failures = []

    kc = [d for d in docs if d["kind"] == "Deployment" and "keycloak" in d["metadata"]["name"]][0]
    env = {e["name"]: e.get("value") for e in kc["spec"]["template"]["spec"]["containers"][0]["env"]}
    if env.get("KC_HOSTNAME") != ORIGIN:
        failures.append(f"KC_HOSTNAME={env.get('KC_HOSTNAME')!r}, expected {ORIGIN}")
    if "KC_HTTP_RELATIVE_PATH" in env:
        failures.append("KC_HTTP_RELATIVE_PATH must not be set -- entrypoint.sh:137 refuses "
                        "a PUBLIC_ISSUER_ORIGIN with a path, and the SPA owns /auth/callback")
    if any(k.startswith("KC_HTTPS_") for k in env):
        failures.append(f"Keycloak must not terminate TLS: {[k for k in env if k.startswith('KC_HTTPS_')]}")

    certs = [d for d in docs if d["kind"] == "Job" and "cert" in d["metadata"]["name"]][0]
    cenv = {e["name"]: e.get("value")
            for e in certs["spec"]["template"]["spec"]["containers"][0]["env"]}
    if cenv.get("PUBLIC_HOST") != HOST:
        failures.append(f"cert PUBLIC_HOST={cenv.get('PUBLIC_HOST')!r}, expected {HOST}")

    gears = [d for d in docs if d["kind"] == "Deployment" and "gears" in d["metadata"]["name"]][0]
    genv = {e["name"]: e.get("value")
            for e in gears["spec"]["template"]["spec"]["containers"][0]["env"]}
    # entrypoint.sh:137 accepts ONLY a bare origin and appends /realms/qa-platform
    # itself. Anything with a path here is a refusal to start, not a 401.
    issuer = genv.get("PUBLIC_ISSUER_ORIGIN", "")
    if issuer != ORIGIN:
        failures.append(f"gears PUBLIC_ISSUER_ORIGIN={issuer!r}, expected the bare origin "
                        f"{ORIGIN} (entrypoint.sh:137 refuses any path)")

    if failures:
        for f in failures:
            print(f"FAIL: {f}")
        return 1
    print(f"PASS: all four renderable pins derive from publicOrigin={ORIGIN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2: Run it**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform
python3 deploy/helm/tests/test_pins.py > /tmp/t10.log 2>&1; echo "exit=$?"; cat /tmp/t10.log
```

Expected: `exit=0`, one PASS line. If it fails, fix the template — not the test.

- [ ] **Step 3: BREAK-TEST IT. This step is not optional.**

A guard that cannot fail is worse than no guard, and this project has shipped one before. Hard-code Keycloak's `KC_HOSTNAME` to a literal so it stops deriving from `publicOrigin`:

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/helm/qa-platform/templates
cp keycloak-deployment.yaml /tmp/kc.bak
sed -i 's|value: {{ .Values.publicOrigin | quote }}|value: "https://wrong.invalid"|' keycloak-deployment.yaml
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform
python3 deploy/helm/tests/test_pins.py > /tmp/t10b.log 2>&1; echo "exit=$?"; cat /tmp/t10b.log
cp /tmp/kc.bak deploy/helm/qa-platform/templates/keycloak-deployment.yaml
python3 deploy/helm/tests/test_pins.py > /tmp/t10c.log 2>&1; echo "restored exit=$?"
```

Expected: the middle run exits non-zero with `FAIL: KC_HOSTNAME='https://wrong.invalid', expected ...`; the restored run exits 0. **Paste both outputs into the commit message** — this is the evidence the guard works.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/deploy/helm/tests/test_pins.py
git commit -m "test(qa-platform): guard that every login pin derives from publicOrigin"
```

---

## Task 11: `lib.sh` extraction and `deploy-k8s.sh`

**Files:**
- Create: `deploy/remote/lib.sh`, `deploy/remote/deploy-k8s.sh`
- Modify: `deploy/remote/sync.sh` (source `lib.sh` instead of defining the shared helpers)

**Interfaces:**
- Produces: `deploy-k8s.sh [--target root@HOST] [--public-origin URL] [--dry-run]`, defaulting to `root@10.136.20.200` and `https://10.136.20.200`.

- [ ] **Step 1: Extract the shared helpers**

Move from `sync.sh` into `lib.sh`: `step()`, `die()`, `ssh_ro()`, `ssh_ro_script()`, `remote_sh()`, `remote_sh_expect()`, `avail_gib()`, the `SSH_OPTS` array, the rsync exclude set, and the four preflight blocks (ssh reachability, docker/compose, disk space, the rsync `--delete` path guard). `sync.sh` sources it and keeps everything compose-specific.

- [ ] **Step 2: Prove `sync.sh` is unchanged in behaviour**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/remote
bash -n sync.sh > /tmp/t11a.log 2>&1; echo "syntax exit=$?"
bash -n lib.sh >> /tmp/t11a.log 2>&1; echo "lib syntax exit=$?"
./sync.sh --dry-run > /tmp/t11b.log 2>&1; echo "dry-run exit=$?"; tail -25 /tmp/t11b.log
```

Expected: all `exit=0`, and the dry run reaches "rsync (--dry-run)" and its PASS line exactly as before.

- [ ] **Step 3: Write `deploy-k8s.sh`**

Steps, stopping at the first failure rather than half-deploying:

1. Source `lib.sh`; run the four preflights, plus new ones for `kubectl` and `helm` on the remote.
2. rsync the working tree (same exclude set; `target/` is not synced).
3. Compute `IMAGE_TAG="deploy-$(date +%Y%m%d-%H%M%S)"`.
4. Build on the remote, tagging with `$IMAGE_TAG`:
   * gears — `--build-arg CARGO_FEATURES="$(cat deploy/cargo-features.argo)"`;
   * UI — `--build-arg VITE_OIDC_ISSUER="$PUBLIC_ORIGIN/realms/qa-platform"` and `--build-arg VITE_OIDC_CLIENT_ID=qa-platform-ui` (no `/auth` segment — Keycloak is at the root path);
   * runner — via the existing `deploy/runner/build-and-import.sh`.
5. `docker save` each and `k3s ctr images import` — **no registry**, the same path `build-and-import.sh` already uses for the runner.
6. `render-realm.sh "$PUBLIC_ORIGIN" <tmp>`.
7. If `gears.hostAliases.enabled` is true, resolve the UI Service's ClusterIP first
   (`kubectl -n qa-platform get svc qa-platform-ui -o jsonpath='{.spec.clusterIP}'`) and pass
   it as `--set uiServiceClusterIP=...`; on a first install the Service does not exist yet, so
   run `helm upgrade` twice — once to create it, once with the alias. Skip entirely when
   Task 1 measured the hairpin as working.
8. `helm upgrade --install qa-platform deploy/helm/qa-platform -n qa-platform --create-namespace --set publicOrigin="$PUBLIC_ORIGIN" --set images.gears.tag="$IMAGE_TAG" --set images.ui.tag="$IMAGE_TAG" --set-file keycloakRealmJson=<rendered realm> --wait --timeout 10m`.
9. `kubectl rollout status` for all three Deployments.
10. `deploy/argo/provision-workflow-secret.sh` — after Keycloak is up, before the first run.
11. `deploy/remote/verify-k8s.sh` (Task 12).

**The image tag is not cosmetic.** With `imagePullPolicy: IfNotPresent` and a fixed tag, `helm upgrade` renders an identical PodSpec, Kubernetes correctly does nothing, and the deploy reports success while the old code keeps serving. A timestamped tag is what makes the rollout real.

- [ ] **Step 4: Syntax-check and dry-run**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/remote
chmod +x deploy-k8s.sh
bash -n deploy-k8s.sh > /tmp/t11c.log 2>&1; echo "syntax exit=$?"
./deploy-k8s.sh --dry-run > /tmp/t11d.log 2>&1; echo "dry-run exit=$?"; tail -30 /tmp/t11d.log
```

Expected: `exit=0`; the dry run prints the resolved target, origin, image tag and the helm command it *would* run, and transfers nothing.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/deploy/remote/lib.sh \
        gears/qa-platform/deploy/remote/deploy-k8s.sh \
        gears/qa-platform/deploy/remote/sync.sh
git commit -m "feat(qa-platform): k8s deploy driver, with the shared preflight extracted"
```

---

## Task 12: The verification suite

**Files:**
- Create: `deploy/remote/verify-k8s.sh`

The 42 checks `sync.sh --argo` runs are this project's most valuable deployment asset. Port their **logic**, changing only the probe.

- [ ] **Step 1: Port the existing checks**

| check | new probe |
|---|---|
| the deployed **binary** carries the feature — read as an absence AND a presence | `kubectl exec deploy/qa-platform-gears -- grep -c ...` |
| the **rendered config** selects the argo executor | `kubectl exec ... -- cat` the rendered file |
| the UI **bundle's** baked issuer (deployed bundle, not the source tree) | `kubectl exec deploy/qa-platform-ui -- grep -r ... /usr/share/nginx/html` |
| Keycloak's advertised issuer | `curl --cacert ca.crt "$PUBLIC_ORIGIN/realms/qa-platform/.well-known/openid-configuration"` |
| the realm's redirect URIs | the same Keycloak admin API call |
| credstore backend is the persistent one | unchanged API call |
| platform observation and cluster health | unchanged API calls |
| runner image is in **containerd**, not merely docker | unchanged |
| the adapter actually CONNECTED (it lists Workflows on connect) | unchanged log grep, via `kubectl logs` |

- [ ] **Step 2: Add the four new cluster checks**

```bash
step "k8s 1/4: every Deployment rolled to $IMAGE_TAG"
for d in qa-platform-gears qa-platform-ui qa-platform-keycloak; do
    got="$(kubectl -n qa-platform get deploy "$d" -o jsonpath='{.spec.template.spec.containers[0].image}')"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "FAIL: could not read $d's image"; exit 1; }
    case "$d" in
        qa-platform-keycloak) : ;;   # pinned upstream image, not built here
        *) case "$got" in
               *":$IMAGE_TAG") echo "PASS: $d runs $got" ;;
               *) echo "FAIL: $d runs $got, expected tag $IMAGE_TAG -- an unchanged PodSpec means helm upgrade did nothing and the OLD code is still serving"; exit 1 ;;
           esac ;;
    esac
done

step "k8s 2/4: PVCs Bound"
# WaitForFirstConsumer means a mis-scheduled pod shows as Pending, not failed.
kubectl -n qa-platform get pvc -o jsonpath='{range .items[*]}{.metadata.name} {.status.phase}{"\n"}{end}' > /tmp/pvc.txt
rc=$?; [ "$rc" -eq 0 ] || { echo "FAIL: could not list PVCs"; exit 1; }
if grep -qv ' Bound$' /tmp/pvc.txt; then echo "FAIL: not every PVC is Bound:"; cat /tmp/pvc.txt; exit 1; fi
echo "PASS: every PVC Bound"

step "k8s 3/4: the gears SA can submit Workflows into argo"
kubectl auth can-i create workflows.argoproj.io -n argo \
    --as=system:serviceaccount:qa-platform:qa-platform-gears > /tmp/cani.txt 2>&1
rc=$?
if [ "$rc" -ne 0 ] || ! grep -qx 'yes' /tmp/cani.txt; then
    echo "FAIL: the cross-namespace RoleBinding is not effective: $(cat /tmp/cani.txt)"; exit 1
fi
echo "PASS: qa-platform-gears can create workflows in argo"

step "k8s 4/4: /realms serves Keycloak's discovery document through the UI nginx"
curl -sS --cacert "$CA_CRT" -o /tmp/disc.json -w '%{http_code}' \
    "$PUBLIC_ORIGIN/realms/qa-platform/.well-known/openid-configuration" > /tmp/disc.code 2>&1
rc=$?
[ "$rc" -eq 0 ] || { echo "FAIL: curl exited $rc"; exit 1; }
grep -qx '200' /tmp/disc.code || { echo "FAIL: HTTP $(cat /tmp/disc.code)"; exit 1; }
iss="$(jq -r .issuer /tmp/disc.json)"
[ "$iss" = "$PUBLIC_ORIGIN/realms/qa-platform" ] \
    || { echo "FAIL: issuer is '$iss', expected '$PUBLIC_ORIGIN/realms/qa-platform'"; exit 1; }
echo "PASS: discovery served at one origin, issuer '$iss'"
```

Note every check writes to a file and reads `$?` separately. **No check may end in a pipe whose exit code is the result** — `cmd | tail` reports `tail`'s status, and that has produced false green reports on this project before.

- [ ] **Step 3: Syntax check**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/remote
chmod +x verify-k8s.sh
bash -n verify-k8s.sh > /tmp/t12.log 2>&1; echo "exit=$?"; cat /tmp/t12.log
```

Expected: `exit=0`.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/deploy/remote/verify-k8s.sh
git commit -m "feat(qa-platform): verification suite for the in-cluster deploy"
```

---

## Task 13: First real deploy, cutover and documentation

**This task changes the remote host. Confirm with the user before Step 2.**

**Files:**
- Modify: `docs/NEXT-SESSION-PROMPT.md`, `deploy/compose/README` references if any

- [ ] **Step 1: Delete the pre-Helm RBAC that would block adoption**

The `qa-runs` ServiceAccount and `qa-runs-executor` Role/RoleBinding in `argo` were created with `kubectl apply` and carry no Helm ownership metadata. The chart uses different names, so they do not collide — but they are now dead weight granting standing access.

```bash
ssh root@10.136.20.200 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl -n argo delete serviceaccount qa-runs --ignore-not-found
kubectl -n argo delete role qa-runs-executor --ignore-not-found
kubectl -n argo delete rolebinding qa-runs-executor --ignore-not-found'
```

Do this **after** the new chart is verified working, not before — it is the old stack's access path.

- [ ] **Step 2: Stop the compose stack to free ports 80/443**

```bash
ssh root@10.136.20.200 'cd /opt/gears-rust/gears/qa-platform/deploy/compose && docker compose stop'
```

**`down -v` must never appear here.** Plain `stop` leaves the volumes intact, so the old stack remains a rollback path until someone deliberately removes it.

- [ ] **Step 3: Deploy**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/remote
./deploy-k8s.sh > /tmp/deploy.log 2>&1; echo "exit=$?"; tail -60 /tmp/deploy.log
```

Expected: `exit=0` and every check PASS. A first build on a host with no layer cache is a cold build of the whole workspace and takes tens of minutes — that is expected, not a hang.

- [ ] **Step 4: Re-register the real platform**

The database starts empty (decision D2), so the eight platform registrations are gone. Re-create the one real one:

```bash
ssh root@10.136.20.200 'cd /opt/gears-rust/gears/qa-platform && ./deploy/argo/provision-platform-kubeconfig-secret.sh'
```

then register `sv-test` through the UI and confirm the Platform page shows `Healthy`, one node `sv-vhp-jele-io`, counts `1/1/1/1/0/0`. The seven stale fixtures are gone as a side effect of the fresh start — which is what item 4 of the handoff had been asking permission to do.

- [ ] **Step 5: Trust the dev CA locally and confirm login in a browser**

Import `ca.crt` (the CA, not the leaf). There is now **one** certificate to trust instead of two, and no mid-redirect refusal.

- [ ] **Step 6: Update the handoff document**

Rewrite `docs/NEXT-SESSION-PROMPT.md`: item 2 becomes done, the standing rules gain the k8s-specific ones (the image-tag trap, the RBAC namespaces), and the remaining items renumber. Record what was measured on the first real deploy, field by field, the way the cluster-health handoff does.

- [ ] **Step 7: Commit and push to the fork**

```bash
git add gears/qa-platform/docs/NEXT-SESSION-PROMPT.md
git commit -m "docs(qa-platform): record the in-cluster cutover"
cd /home/serhii/Jelastic/projects/fabric/gears-rust && ./push-to-fork.sh
```

---

## Post-implementation

Run the whole-branch review before calling this done. The last one found a defect no per-task review could see — a feature-off build fabricating an `Unreachable` status through the interaction of three separately-correct tasks — and this plan has the same shape: five templates that are each correct and must agree on one origin.

`sync.sh` is deliberately **not** deleted. Retire it in a follow-up once the k8s path has run green several times; deleting the only proven deploy path the same day an unproven one lands is not a trade worth making.
