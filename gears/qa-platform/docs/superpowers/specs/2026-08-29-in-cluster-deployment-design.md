# qa-platform in-cluster deployment — design

**Status:** awaiting approval
**Date:** 2026-08-29
**Scope:** deployment only — a Helm chart, a deploy driver and a verification suite
**Gears touched:** none of the four gears' Rust code is expected to change
**Predecessor:** [2026-08-28 cluster health](./2026-08-28-cluster-health-design.md)

---

## 1. Context

The user's request, recorded and never picked up: *"i want that our testrunner as a legacy
installed in kuber too"*. It was sequenced behind the platform-observation and cluster-health
work by their own choice.

Today the qa-platform stack runs as **docker compose on the remote host**
(`root@10.136.20.200`) and only *dispatches* work into k3s, via Argo Workflows. Legacy
(`vhp-testrunner`) runs the manager itself *inside* a cluster, from a Helm chart at
`charts/vhp-testrunner`. This design moves qa-platform to the same footing.

### What is on the target cluster today

Measured read-only on 2026-08-29:

| fact | value |
|---|---|
| node | `vhp-central-monitoring-hcitests`, single control-plane, CentOS Stream 9 |
| k3s | `v1.36.3+k3s1`, containerd `2.3.2-k3s2`, 44 h old |
| namespaces | `argo` plus the four system ones — nothing else |
| ingress classes | **none** |
| storage classes | `local-path` (default), `rancher.io/local-path`, `Delete`, `WaitForFirstConsumer` |
| k3s server args | `--disable=traefik --disable=servicelb` |
| host ports in use | 443 and 8080 (compose `ui`), 8180 and 8443 (compose `keycloak`), 5432 on loopback (compose `postgres`) — all `docker-proxy` |

Two consequences follow immediately and shape everything below. There is **no ingress
controller and no LoadBalancer implementation**, so any entry point bottoms out in a
`hostPort`, a `NodePort`, or a controller we install ourselves. And `reclaimPolicy: Delete`
means a deleted PVC destroys its data with no recovery step.

### What legacy's chart provides, and what it does not

`charts/vhp-testrunner` has: a Postgres StatefulSet, a manager Deployment with a PVC, a
ServiceAccount and Role/RoleBinding, a manager-ui Deployment with a Service and an nginx
Ingress, and `argo-workflows` as a subchart dependency.

It has **no Keycloak**. Legacy authenticates by nginx-ingress basic auth
(`templates/manager-ui-ingress-auth-secret.yaml`, an `htpasswd` Secret). qa-platform's OIDC
login chain has no counterpart there, so the largest part of this design is the part legacy
cannot be copied for.

---

## 2. Decisions taken before design

Four rulings from the user, recorded here as the design's premises:

| # | decision |
|---|---|
| D1 | **Everything moves in-cluster** — gears, UI, Postgres, Keycloak and the migrate/seed jobs. Legacy parity. Only Argo was already there. |
| D2 | **Fresh start, no data migration.** The in-cluster Postgres begins empty; `db-migrate` and `tenant-seed` re-run. The eight platform registrations and ~200 run rows are not carried over. |
| D3 | **The UI pod is the edge** — `hostPort: 443`, one origin, no ingress controller. |
| D4 | The **cargo feature list de-duplication** is in scope as a targeted improvement. |

D2's upside is worth stating: the seven stale platform fixtures that dominate the dashboard
and log a warning every ticker cycle disappear without anyone having to authorise a
deletion. Its cost is that the one real platform, `sv-test`, must be re-registered by hand.

---

## 3. Topology

Namespace `qa-platform`, Helm release `qa-platform`. Argo stays in `argo`.

### 3.1 Objects

| Object | Shape | Notes |
|---|---|---|
| `postgres` | StatefulSet, 1 replica, headless Service, `volumeClaimTemplate` on `local-path` | `deploy/compose/initdb/01-create-qa-platform-databases.sh` becomes a ConfigMap mounted at `/docker-entrypoint-initdb.d`. It creates the **eight gear databases** the config names (`qa_environments`, `qa_catalog`, `qa_runs`, `qa_insights`, `settings`, `credstore`, `resource_group`, `event_broker`) — the gears never create their own, and a missing one is a connection error at gear init. Credentials in a Secret. |
| `gears` | Deployment, 1 replica, Service `:8087` | One container running all four gears from the single binary, as compose does. PVC for `/var/lib/cf-gears/data` (qa-catalog's git clones — compose's `qa-catalog-data` volume). ConfigMap carrying `qa-platform-stack.yaml`; `deploy/docker/entrypoint.sh` keeps performing its substitutions and insertions unchanged. ServiceAccount `qa-platform-gears`. |
| `ui` | Deployment, 1 replica, `hostPort: 443` (and 80) | The edge — see §4. |
| `keycloak` | Deployment, 1 replica, Service `:8080` | **Plain HTTP, at the DEFAULT root path.** `KC_HTTP_ENABLED=true`, `KC_PROXY_HEADERS=xforwarded`, `KC_HOSTNAME=<PUBLIC_ORIGIN>`, `KC_HOSTNAME_BACKCHANNEL_DYNAMIC=true` as today. **No `KC_HTTP_RELATIVE_PATH`** — see §4.3. Realm import from a ConfigMap. Storage: see §3.3. |
| `db-migrate` | Job, Helm hook `post-install,post-upgrade`, weight 0 | `cf-gears-example-server --config ... migrate`, the same argv compose uses. |
| `tenant-seed` | Job, same hooks, weight 10 | `postgres:16` image running `seed-tenant.sh` from a ConfigMap, as compose does — the gears image has no `psql`. |
| `certs` | Job, hook `pre-install`, weight -10 | Runs `deploy/compose/keycloak-tls/gen-cert.sh` **unchanged** and writes its output into a Secret. It still emits two leaves; only `ui.crt`/`ui.key` and `ca.crt` are mounted anywhere. Editing a script whose comments record hard-won TLS lessons, to delete an unused output, is not worth the risk. |
| RBAC | ServiceAccount in `qa-platform`; Role + RoleBinding in `argo` | See §5. |
| `git-fixture` | **not in the chart**, `gitFixture.enabled: false` | It is a smoke-test fixture, not stack. Only `smoke.sh`'s fixture path calls it. |

### 3.2 Why `hostPort` and not a controller

The three candidates, with the cluster as measured:

* **UI pod on `hostPort` (chosen).** Reuses `qa-platform-ui/nginx.conf`, which is *already*
  the full reverse proxy: the SPA `try_files`, the `location /qa/v1/` proxy, and the SSE
  stream's `$sse_authorization` bridge with its `map $arg_access_token` — the piece an
  ingress would have to reproduce and could get wrong. Adds no component to a deliberately
  minimal k3s.
* **ingress-nginx.** Legacy parity, and its
  `nginx.ingress.kubernetes.io/proxy-read-timeout: "3600"` annotations exist precisely for
  SSE. But with `servicelb` disabled the controller itself needs `hostPort` or
  `hostNetwork`, so it buys a second TLS termination point in front of an nginx that already
  terminates TLS, and buys it with an extra component.
* **Re-enable Traefik.** Requires editing the k3s server unit and restarting k3s while Argo
  runs on it. Ruled out as disruption without benefit.

Nothing in the Service/port layout forecloses adding an Ingress later; that is deliberate
but is not work this design does.

### 3.3 Keycloak's own storage, and why it stays ephemeral

Compose runs Keycloak as `start-dev --import-realm` with **no `KC_DB` of any kind** — so it
is on the embedded H2 store, and `initdb/01-create-qa-platform-databases.sh` does not create
a database for it. This design keeps that arrangement rather than moving Keycloak onto the
Postgres StatefulSet.

That is safe for one measured reason: `realm-qa-platform.json` **pins the
`qa-platform-workflow` client's secret** (`secret` is set on the confidential client; the
`qa-platform-ui` client is public and has none). A pod restart therefore discards H2,
re-imports the realm, and produces the *same* client secret — so the Secret that
`provision-workflow-secret.sh` pre-provisioned in `argo` stays valid. Had Keycloak generated
that secret, every pod restart would silently invalidate it, and a Deployment restarts far
more readily than a compose container.

What is lost on restart is anything created in Keycloak at runtime and not in the import
file. That is acceptable for a QA platform and is unchanged from today's behaviour.

Moving Keycloak to `start` (production mode) on the Postgres StatefulSet is a reasonable
future step and is deliberately **not** taken here: it adds hostname, TLS and database
configuration — three new failure surfaces — to a deploy whose login chain is already the
riskiest part of the change.

### 3.4 hostPort mechanics

`hostPort: 443` is below 1024, which is not a constraint in Kubernetes, and the nginx image
already runs as root. A `hostPort` pins the pod to whichever node it lands on; with one node
that is not a limitation, and it is the same coupling `local-path` storage already imposes.

---

## 4. The login chain

### 4.1 The problem being solved

`docker-compose.yml`'s header names five pins that must agree or every login fails with a
symptom naming none of them: Keycloak's `KC_HOSTNAME`, the gears' `issuer_pattern`, the
realm's `redirectUris`/`webOrigins`, the UI bundle's baked-in `VITE_OIDC_ISSUER`, and the
certificate SANs. Today two *different* origins are involved — `https://<host>` for the UI
and `https://<host>:8443` for Keycloak — and the Keycloak certificate fails **mid-redirect**,
where it reads as a broken login rather than a certificate warning.

### 4.2 One origin

`PUBLIC_ORIGIN` — `https://10.136.20.200` — is a single value in `values.yaml` and the sole
source of:

| consumer | value |
|---|---|
| Keycloak | `KC_HOSTNAME: https://10.136.20.200` |
| gears | `PUBLIC_ISSUER_ORIGIN: https://10.136.20.200` — the **bare origin**; `entrypoint.sh` appends `/realms/qa-platform` to build `issuer_pattern` |
| realm import | `redirectUris`, `webOrigins` on that origin |
| UI image | build arg `VITE_OIDC_ISSUER=https://10.136.20.200/realms/qa-platform` |
| certificate | one leaf, SAN `IP:10.136.20.200` |

Port 8443 and the second issuer origin are gone, and the Keycloak leaf certificate is no
longer mounted or referenced — `KC_HTTPS_CERTIFICATE_FILE` and `KC_HTTPS_CERTIFICATE_KEY_FILE`
both disappear, because the only TLS in the system is the UI nginx's.

### 4.3 nginx changes

`qa-platform-ui/nginx.conf` gains exactly three modifications:

1. a new regex location serving Keycloak's two root prefixes —
   `location ~ ^/(realms|resources)/` — with `X-Forwarded-Proto`/`-Host`/`-For` set,
   matching what `KC_PROXY_HEADERS=xforwarded` expects. The upstream reaches `proxy_pass`
   through an nginx variable (`set $kc http://qa-platform-keycloak:8080; proxy_pass $kc;`),
   never as a bare literal — a literal is resolved at config-load time and nginx refuses to
   start at all (`[emerg] host not found in upstream`) if the name is not resolvable at that
   instant, which would take the cluster's only edge into CrashLoopBackOff whenever CoreDNS
   is not yet answering.

   **`/admin/` was in this list in an earlier revision and is deliberately not proxied.**
   It was justified as what `provision-workflow-secret.sh` needs; that script actually reads
   the client secret out of the realm JSON with `jq` (`provision-workflow-secret.sh:76-81`)
   and never calls Keycloak. Proxying it published the admin console and the full admin REST
   API on the application's public origin behind committed `admin`/`admin` credentials, for
   a caller that does not exist. The console is still reachable in-cluster or via
   `kubectl port-forward`.

   **It is not `/auth/`, and Keycloak is not given a relative path.** Two independent facts
   forbid it, both measured on 2026-08-29 rather than assumed:

   * `deploy/docker/entrypoint.sh:137` validates `PUBLIC_ISSUER_ORIGIN` against
     `^https?://host[:port]$` and **refuses to start** on anything carrying a path — so
     `https://10.136.20.200/auth` never boots. The guard is strict because the value is
     interpolated into the `issuer_pattern` regex, and widening it to serve a cosmetic path
     would be trading a real safety property for nothing.
   * The SPA **already routes `/auth/callback`** as its OIDC redirect target. An nginx
     `location /auth/` shadows it, so the browser returning from Keycloak lands back in
     Keycloak instead of the application.

   At the root path both problems vanish: `PUBLIC_ISSUER_ORIGIN` stays a bare origin that the
   existing guard accepts unchanged, and no Keycloak prefix collides with any SPA route
   (verified against the full route table — `/realms` and `/resources` are both free;
   `/admin` is free too but is not proxied, see above).
2. `resolver` changes from Docker's embedded DNS to the cluster's
   (`kube-dns.kube-system.svc.cluster.local`), keeping the existing pattern of a variable in
   `proxy_pass` forcing per-request resolution;
3. `set $gears http://gears:8087;` points at the gears **Service**.

The TLS seam (`include /etc/nginx/tls-conf/*.conf`) and `ui-tls/tls.conf` are reused as-is —
they are a fragment inside the existing `server` block, so HTTP and HTTPS keep sharing every
location including the SSE bridge. `tls.conf` becomes a ConfigMap and the leaf a Secret mount.

### 4.4 The build-time pin, and its guard

`VITE_OIDC_ISSUER` is inlined by vite at image build (`deploy/docker/qa-platform-ui.Dockerfile`
`ARG`/`ENV`, lines 45-48). This design keeps it build-time: the deploy script rebuilds the UI
image on the remote every deploy, so the cost is nil.

It is, however, **the one pin Helm cannot repair after the fact**. A `helm upgrade` that
changes `PUBLIC_ORIGIN` without an image rebuild produces a stack whose every other pin
agrees and whose login fails. `sync.sh`'s existing check — grep the *deployed bundle* for its
baked issuer, not the source tree — carries over verbatim as the guard.

### 4.5 Risk: the gears' discovery fetch

The gears fetch OIDC discovery over HTTPS and have no alternative:
`UrlSecurityPolicy::STRICT` is enforced in three places (the trusted-issuer `discovery_url`
at config load, `s2s_oauth.discovery_url`, and the `jwks_uri` the discovery document reports
at fetch time), and the only relaxation, `allow_insecure_http_for_tests()`, is `#[doc(hidden)]`
and reachable from no config field.

Under D3 the only HTTPS endpoint is the UI pod's `hostPort` on the node, so the gears pod
calls its own node's IP at `https://10.136.20.200/realms/qa-platform/...` — a pod → node-IP
hairpin. This normally works on k3s but is the
single assumption in this design that has not been measured.

**Verify it in the first implementation task, not at deploy time.** The fallback is cheap:
a `hostAliases` entry in the gears PodSpec mapping `10.136.20.200` to the `ui` Service's
ClusterIP. The URL is unchanged, so the certificate is still validated against the
`IP:10.136.20.200` SAN, and the hairpin disappears.

The dev CA is mounted into the gears pod from the certs Secret, exactly as compose mounts
`ca.crt` today.

---

## 5. Argo and RBAC

### 5.1 `Config::infer()`

The observation plan's Task 6 deliberately chose the config shape so that an empty
`kubeconfig_path` implies `Config::infer()`. In-cluster that resolves the projected
ServiceAccount token, so:

* the generated kubeconfig copy (`render-argo.sh`'s `k3s-kubeconfig.yaml`, whose whole
  purpose is rewriting k3s' `server: https://127.0.0.1:6443` to an address reachable from a
  container) is **not needed** for k8s deploys;
* both config fragments survive, because they *select the executor* — a concern separate from
  cluster access — but each loses its `kubeconfig_path` line. They become chart-rendered
  ConfigMaps instead of `render-argo.sh` output;
* `entrypoint.sh`'s refusal to start when `QA_RUNS_ARGO_CONFIG` is set without
  `QA_ENVIRONMENTS_ARGO_CONFIG` (2026-08-28 final review, finding I2) keeps working untouched.

**One `entrypoint.sh` change is required after all**, discovered during implementation by running
the script against a rendered fragment rather than reasoning about it. `entrypoint.sh:295-299`
carries a *second* `validate_fragment` call on the qa-environments fragment demanding a non-empty
`kubeconfig_path:` line — so "no `kubeconfig_path`" and "the container starts" cannot both be true
today, and every in-cluster gears pod would crash-loop at boot.

The Rust side needs nothing: `secret_writer.rs:94` already documents that an empty or absent path
means `Config::infer()`, and `:111` filters empty strings. The guard is the only obstacle, and its
premise is compose-specific — its own error text says `Config::infer()` finds "no service-account
token ... inside this container", which is exactly what stops being true in-cluster. The guard
therefore becomes conditional: it still refuses a missing `kubeconfig_path` when
`/var/run/secrets/kubernetes.io/serviceaccount/token` is absent, and permits it when that
projected token is present. Compose keeps the full protection; the in-cluster path is allowed.

`render-argo.sh` remains for compose. It is not deleted.

### 5.2 Cross-namespace binding

`deploy/argo/qa-runs-rbac.yaml` moves into the chart with **one** change: the ServiceAccount
lives in `qa-platform` while the Role and RoleBinding stay in `argo` (the namespace the
Workflows live in), so the RoleBinding subject carries an explicit `namespace: qa-platform`.

The verbs are unchanged, and the file's reasoning still holds — `workflows`
get/list/watch/create/patch, `pods` get/list, `pods/log` get; no `delete`, no `secrets`, no
`configmaps`, no `cronworkflows`.

The **workflow pod's** identity (`argo-workflow`, created by the argo-workflows chart) is a
different account and is not this chart's concern.

### 5.3 What stays an operator step

`deploy/argo/provision-workflow-secret.sh` still runs outside the chart. It reads the
`qa-platform-workflow` client secret out of the realm JSON with `jq`
(`provision-workflow-secret.sh:76-81`) — **it needs no Keycloak admin credentials and makes
no HTTP call to Keycloak at all**; an earlier revision of this section claimed otherwise, and
that claim was the sole stated justification for proxying `/admin/` through the public
origin (see §4.3). The reason it is not a gear's job is ADR-0001's waiver condition 3 — a qa-runs process that created that Secret
would have to hold the material in memory, and `run_executor.rs:85-86`'s claim would stop
being true. The same applies to `provision-platform-kubeconfig-secret.sh`.

`deploy/runner/build-and-import.sh` is unchanged: the runner image still has to reach the
node's containerd, and already does it the right way.

### 5.4 The feature list (D4)

The `platform-observation` / `qa-runs-argo` feature list is pinned in three places today
(`apps/cf-gears-example-server/Cargo.toml`, `deploy/docker/qa-platform.Dockerfile`,
`deploy/compose/docker-compose.argo.yml`), and the compose overlay's own header records that
the duplication has already bitten once — `postgres-credstore` was added to the Dockerfile
`ARG` and would have been absent from every `--argo` build while looking deployed.

A k8s build step would make it four. It cannot honestly be reduced to one: Docker has no
syntax for an `ARG` default read from a file, so the Dockerfile must keep carrying a literal.
The fix is therefore **two places with an enforced relationship** rather than four with a
described one:

* the Dockerfile `ARG` stays the canonical **base** list;
* `deploy/cargo-features.argo` becomes the canonical **argo** list, read by the compose
  overlay (via `render-argo.sh`'s `.env`) and by the k8s deploy script;
* a test fails if the base list is not a subset of the argo list.

That is what actually catches the `postgres-credstore` class of drift, which a comment
warning about duplication did not.

---

## 6. The deploy driver

### 6.1 Why a new script

`deploy/remote/sync.sh` is 1343 lines. Its front is generic (ssh preflight in batch mode,
docker/compose checks, disk space, the rsync-`--delete` path guard, the `remote_sh` helpers,
the rsync exclude set). Its back two-thirds is compose-specific: writing the remote `.env`,
`COMPOSE_FILE`, `docker compose up -d`, and `docker exec`-based verification. A `--k8s` flag
would fork the script down its middle.

* Extract the shared front into `deploy/remote/lib.sh`, sourced by both.
* Add `deploy/remote/deploy-k8s.sh`, modelled on legacy's `scripts/testrunner-redeploy.sh`.
* **Keep `sync.sh`** until the k8s path has run green several times, then retire it in a
  follow-up. Deleting the only proven deploy path the same day an unproven one lands is not
  a trade worth making.

### 6.2 Steps

1. Preflight: ssh reachability, `docker`, `kubectl`, `helm`, disk.
2. rsync the working tree (same exclude set; `target/` is not synced).
3. Build on the remote: gears image with `CARGO_FEATURES` from `deploy/cargo-features.argo`;
   UI image with the `VITE_OIDC_ISSUER` build arg derived from `PUBLIC_ORIGIN`; runner image.
4. `k3s ctr images import` all three. **No registry** — legacy's proven path, and
   `build-and-import.sh` already does exactly this for the runner.
5. Render the realm for the single origin (Keycloak 26.0.8 will not expand a placeholder in
   an import file — measured, it aborts startup — so `render-realm.sh`'s substitution
   approach is kept, targeting one origin instead of several).
6. `helm upgrade --install`, pinning the three image tags, then wait for rollouts.
7. `provision-workflow-secret.sh`, after Keycloak is up and before the first run.
8. Verification (§7).

### 6.3 Image tags

Images are tagged `deploy-<timestamp>`, never `latest`. With `imagePullPolicy: IfNotPresent`
and a fixed tag, `helm upgrade` renders an identical PodSpec, Kubernetes correctly does
nothing, and the deploy reports success while the old code keeps serving. This is the same
reason legacy's script pins tags on every `helm upgrade`.

---

## 7. Verification

The 42 checks `sync.sh --argo` runs are this project's most valuable deployment asset and are
mostly probe-independent. They port with `docker exec` becoming `kubectl exec`:

| check | ports as |
|---|---|
| the deployed **binary** carries the feature (read as absence *and* presence) | `kubectl exec` + the same grep |
| the **rendered config** selects the argo executor | `kubectl exec cat` of the rendered file |
| the UI **bundle's** baked issuer | `kubectl exec` grep in the ui pod's `dist` |
| Keycloak's advertised issuer over the public origin | `curl` from the remote with the CA |
| the realm's redirect URIs | read out of the `qa-platform-realm` ConfigMap — the exact document Keycloak imports — **not** an admin API call, since `/admin/` is not proxied |
| credstore backend is the persistent one | unchanged API call |
| platform observation and cluster health | unchanged API calls |
| the runner image is in **containerd**, not merely docker | unchanged |
| the adapter actually connected (it lists Workflows on connect) | unchanged |

Four are new, and each covers something only the cluster can get wrong:

1. every Deployment rolled to the **new tag** — not merely `Available`, which an unchanged
   PodSpec also satisfies;
2. PVCs `Bound` — under `WaitForFirstConsumer` a mis-scheduled pod is `Pending`, not failed;
3. `kubectl auth can-i create workflows.argoproj.io -n argo
   --as=system:serviceaccount:qa-platform:qa-platform-gears` — the cross-namespace binding is
   new and silent when wrong;
4. `/realms/qa-platform/.well-known/openid-configuration` actually serves Keycloak's
   discovery document through the UI nginx, and its `issuer` equals the public origin plus
   `/realms/qa-platform`.

**The standing rule applies to every one of them:** never pipe a command whose exit code is
the result. `cmd | tail` reports `tail`'s status, and that has produced false green reports on
this project more than once. Redirect to a file and read `$?`.

---

## 8. Testing

* `helm lint`, plus golden-file `helm template` renders committed as fixtures.
* **The guard that matters:** one test asserting that the rendered Keycloak `KC_HOSTNAME`,
  the gears' `issuer_pattern`, the realm's `webOrigins` and the certificate SAN all derive
  from the same `PUBLIC_ORIGIN`. This is the failure mode that has repeatedly cost sessions,
  and a chart is the first artefact in this project's history where it can be caught before a
  deploy rather than after one.
* **Break-test that guard.** Change one of the four by hand and confirm the test goes red. A
  guard that cannot fail is worse than no guard, and this project has shipped one before.
* No Rust changes are expected. If any arise, TDD applies.
* The user's test suite is not run against the VHP cluster without asking.

---

## 9. Out of scope

Item 1 of the handoff (the unresolvable-kubeconfig state, ruled on 2026-08-29 and awaiting
its own design), item 3 (code coverage), deleting the stale platform fixtures (D2 removes
them as a side effect), leader election for `qa-environments`, multi-replica or HA anything,
cert-manager, an image registry, and an ingress controller.

## 10. Operator consequences of D2

After cutover the database is empty. The eight platform registrations are gone, including the
real `sv-test` one, whose re-creation means re-running
`deploy/argo/provision-platform-kubeconfig-secret.sh` and re-registering through the UI. The
~200 run rows are gone too — worth noting because they are also the corpus the coverage work
would eventually want, and there will not be another chance to keep them.
