# Upgrading to chart 1.0.0

Applies to: upgrading a **qa-platform** release installed with chart
**< 1.0.0** to chart **1.0.0 or later**.

## What changed and why `helm upgrade` alone will not work

1.0.0 adds `app.kubernetes.io/instance: {{ .Release.Name }}` to the
`spec.selector.matchLabels` (and the matching pod-template labels) of all
four workloads: `qa-platform-gears`, `qa-platform-ui`,
`qa-platform-keycloak` (Deployments) and `qa-platform-postgres`
(StatefulSet). This is what lets two releases of this chart coexist
without one release's controller adopting the other's pods.

**A Deployment's and a StatefulSet's `spec.selector` is immutable.**
Kubernetes refuses any update that changes it, so a plain
`helm upgrade` of a pre-1.0.0 release fails on every one of these four
objects with:

```
Deployment.apps "qa-platform-gears" is invalid: spec.selector: Invalid value: ...: field is immutable
```

(or the equivalent `StatefulSet.apps "qa-platform-postgres" is invalid`).
There is no in-place upgrade. Each of the four workloads below must be
deleted and let the new chart recreate it — but **not by the same route**.

## Also new in 1.0.0: two values your old release never set are now REQUIRED

Helm 3's `helm upgrade` (no `--reset-values`) reuses the PREVIOUS release's
supplied `--set`/`-f` values for anything the new invocation does not
override. That covers `publicOrigin` if your original `helm install`
passed it — it almost certainly did, since the chart has required it for
longer than this upgrade note has existed. It does **not** cover
`keycloak.adminPassword`: that key did not exist in this chart before
1.0.0, so a pre-1.0.0 release's stored values have nothing under it, and
Helm has nothing to reuse. A bare `helm upgrade qa-platform
./deploy/helm/qa-platform -n qa-platform` therefore renders
`keycloak.adminPassword` at the new chart's own default — empty —
and `keycloak-deployment.yaml`'s `required` aborts the render:

```
execution error at (qa-platform/templates/keycloak-deployment.yaml:...): keycloak.adminPassword is required: set it explicitly, there is no default.
```

**This is exactly the failure that makes the fast, database-destroying
route (below) actually dangerous rather than merely undocumented**: the
naive fix is to run the delete commands, hit this error, and go looking
for *some* password that makes the render succeed. The two that come to
hand both make things worse:

- `--set keycloak.adminPassword=admin` (the value this chart shipped as a
  default before 1.0.0) trips the OTHER new guard —
  `keycloak-deployment.yaml:67-68` refuses the literal `admin` unless
  `devMode=true` — so the render still fails, now citing `devMode`
  instead.
- Setting `devMode=true` on a real release to get past *that* is wrong for
  a different reason: it also changes which application users
  `realm-qa-platform.json` seeds (see the fixture-login change below), and
  `devMode` is a throwaway-stand switch, not an upgrade parameter.

By the time either of those looks tempting, `gears`, `ui` and `keycloak`
are already deleted from the step below — so the failed render is not a
no-op you can retry after thinking; it is a stack with three of its four
workloads gone, refusing to come back. **Decide the real password before
you delete anything.** The commands below pass it explicitly, before the
delete step, for that reason:

```bash
NEW_ADMIN_PASSWORD="$(openssl rand -base64 24)"   # or your own; never "admin" on a real release
```

**Other things that changed and this document used to say nothing about,
so read this before you're surprised by any of them:**

- **`publicOrigin` is required.** No default was ever shipped, but if you
  are not certain your original install passed `--set publicOrigin=...`
  explicitly (rather than relying on a value that predates that
  requirement), pass it again explicitly below rather than trusting reuse.
- **The realm moved from a ConfigMap to a Secret.** The object is still
  named `qa-platform-realm` and still carries
  `realm-qa-platform.json`, but it is now `kind: Secret` with
  `stringData`, not `kind: ConfigMap` with `data`. Nothing in the upgrade
  commands below needs to change for this — Helm recreates it under the
  new chart automatically — but any tooling or runbook of yours that reads
  it with `kubectl get configmap qa-platform-realm` will now get "not
  found" and must switch to `kubectl get secret qa-platform-realm`.
- **The seeded fixture logins changed.** The realm's `admin`/`viewer`
  demo users used to have the passwords `admin`/`viewer`; they are now
  `AdminPass1!`/`ViewerPass1!` (the realm's own `passwordPolicy` no longer
  permits the old ones — Keycloak refuses to even import them). If you, or
  a script, or a bookmark, still has `admin`/`admin` saved for this
  stack's UI login, it will fail after this upgrade with no hint that the
  password itself is what changed. Additionally, those fixture users now
  only seed at all when `devMode=true` — see the next point.

## Also required, from 2026-09-21: the two signing secrets

`bundleDownloadSigningSecret` and `collectReportSigningSecret` no longer
have a default. **This applies to every upgrade, not only one from a
pre-1.0.0 release**, and it will stop an upgrade of a release that has
been running happily for months.

### What happens to a release that never set them

`helm upgrade` fails and changes nothing in the cluster:

```
Error: execution error at (qa-platform/templates/gears-deployment.yaml:...): bundleDownloadSigningSecret is required: it is the only access control on the anonymous test-bundle download route and there is no safe default. ...
```

That refusal is the point, not a regression to work around. Until now
`gears-config-configmap.yaml` substituted a per-render `randAlphaNum 32`
for each unset secret, so **every** `helm upgrade` of such a release had
already been silently rotating both keys. Each one is the only access
control on an anonymously reachable route, so a rotation invalidates
every `?sig=` minted before it: a test bundle built before the upgrade
and fetched after it gets a 403 and `fetch_bundle.py` exits 1 before a
single test runs.

MEASURED on the dev stand, and the reason this changed: after
`helm upgrade --reuse-values --set bundleDownloadSigningSecret=...`
produced revision 29, the ConfigMap held the new key and the gears pod
was still 38 minutes old, still signing with the old one. That second
defect is fixed too (the pod template now carries `checksum/*`
annotations, so a config change rolls the Deployment) — which makes the
rotation take effect immediately rather than at the next restart, and is
why leaving the random in place was no longer an option.

### What to do

Generate one value per secret, once, and keep them:

```bash
BUNDLE_SIGNING_KEY="$(openssl rand -hex 24)"
COLLECT_SIGNING_KEY="$(openssl rand -hex 24)"
```

Pass both on the upgrade (they join `--reuse-values`' stored values, so
subsequent upgrades do not need them again):

```bash
helm upgrade qa-platform ./deploy/helm/qa-platform -n qa-platform \
  --reuse-values \
  --set "bundleDownloadSigningSecret=$BUNDLE_SIGNING_KEY" \
  --set "collectReportSigningSecret=$COLLECT_SIGNING_KEY"
```

**Do this when no run is in flight.** Any workflow already holding a
`?sig=` built with the old key fails its bundle fetch. That was true of
the old random fallback as well; the only difference is that now somebody
chose the moment.

If you want to keep the key a release is *currently* using rather than
issue a new one, read it out of the live ConfigMap first — it is in the
gears config under `qa-catalog.bundle_download_signing_secret` and
`qa-insights.collect_report_signing_secret`:

```bash
kubectl get configmap qa-platform-gears-config -n qa-platform \
  -o jsonpath='{.data.qa-platform-stack\.yaml}' \
  | grep -E 'bundle_download_signing_secret|collect_report_signing_secret'
```

Note that on a release that never set them this reads back whatever the
LAST render happened to generate, which is not necessarily what the
running pod is using — that is the pair of defects above, seen from the
other side.

## READ THIS BEFORE YOU RUN ANYTHING: the fast route destroys the database

It is tempting to treat this as one downtime trade-off and delete-and-let-
`helm upgrade`-recreate all four workloads the same way. **Do not do this
to `qa-platform-postgres`.**

`postgres-statefulset.yaml`'s `volumeClaimTemplates` entry is named
`pgdata`, and Kubernetes names the PVC it generates
`pgdata-<statefulset-name>-0` — derived from the **StatefulSet's name**,
not from any label. `helm delete` (and a plain `kubectl delete
statefulset`) does **not** garbage-collect PVCs created from
`volumeClaimTemplates` — the PVC is left behind, still bound to the old
StatefulSet's identity. If the StatefulSet is deleted and recreated under
a *different* name (or if anything else about `pgdata-<name>-0`'s derived
name changes), the new StatefulSet computes a *different* PVC name,
provisions a **new, empty** volume, and the new postgres pod starts
against it. The old PVC — and every row in it — is still on disk,
unreferenced, and invisible to anyone who did not know to look for it.
**The database is gone as far as the application is concerned, and no
error is printed anywhere in this path.**

This is why the migration route below treats postgres differently from
the other three workloads. Read it fully before running any command.

## The four migration routes

### `gears`, `ui`, `keycloak` (Deployments) — delete and recreate

These are safe to delete outright. `gears` mounts
`qa-platform-gears-data`, a **standalone** `PersistentVolumeClaim`
(`gears-pvc.yaml`) referenced by name (`claimName: qa-platform-gears-data`)
— it is not derived from the Deployment's identity, has its own lifecycle,
and survives the Deployment being deleted and recreated. `ui` and
`keycloak` carry no persistent state at all (Keycloak's H2 database is
ephemeral by design — see `keycloak-deployment.yaml`'s header comment).

```bash
kubectl delete deployment qa-platform-gears qa-platform-ui qa-platform-keycloak -n qa-platform
```

### `postgres` (StatefulSet) — orphan-delete, same name, let the chart recreate it

**Do not rename it, and do not let `helm upgrade` delete it outright.**
`--cascade=orphan` deletes the StatefulSet object only — it leaves the pod
and the PVC alone — and then `helm upgrade` recreates a StatefulSet with
the **same name** (`qa-platform-postgres`), which computes the **same**
PVC name (`pgdata-qa-platform-postgres-0`) and re-binds the existing
volume. The orphaned pod is deleted separately so the new StatefulSet's
pod is the one that actually starts against the re-bound PVC.

```bash
kubectl delete statefulset qa-platform-postgres -n qa-platform --cascade=orphan
kubectl delete pod qa-platform-postgres-0 -n qa-platform   # the orphaned pod
```

### Then, once all four are handled, recreate everything under the new chart

**`--reuse-values` is required here, not optional** — without it, Helm 3
does not merge your `--set` flags onto the previous release's stored
values, it RESETS every value to the chart's own defaults and applies
only what you pass on this command line. MEASURED on the dev stand: a
bare `--set publicOrigin=... --set keycloak.adminPassword=...` (and now
the two signing secrets) with no
`--reuse-values` silently reverted `images.gears.tag`/`images.ui.tag`
from the release's actual running tag back to the chart's `latest`
default, and would just as silently drop `devMode` or any other value
you had previously set. `--reuse-values` merges the old release's values
with the new `--set` flags instead of replacing them, which is what you
want: keep everything else, override only the two values below.

```bash
helm upgrade qa-platform ./deploy/helm/qa-platform -n qa-platform \
  --reuse-values \
  --set "publicOrigin=$PUBLIC_ORIGIN" \
  --set "keycloak.adminPassword=$NEW_ADMIN_PASSWORD" \
  --set "bundleDownloadSigningSecret=$BUNDLE_SIGNING_KEY" \
  --set "collectReportSigningSecret=$COLLECT_SIGNING_KEY"
```

(Add `--set devMode=true` only if this is a throwaway dev stand and you
want the realm's fixture `admin`/`AdminPass1!` and `viewer`/`ViewerPass1!`
logins seeded — see "the seeded fixture logins changed" above. Never set
it on a release with real data or real users. If your release already
has `devMode=true` stored, `--reuse-values` keeps it without you having
to pass it again.)

This single `helm upgrade` recreates `gears`, `ui` and `keycloak` (already
deleted above) and reconciles `postgres` back into existence under its
original name, re-binding `pgdata-qa-platform-postgres-0`.

## Full sequence

```bash
PUBLIC_ORIGIN="https://your-host-or-domain"        # the value your release already uses
NEW_ADMIN_PASSWORD="$(openssl rand -base64 24)"    # or your own; never "admin" on a real release
BUNDLE_SIGNING_KEY="$(openssl rand -hex 24)"       # see "the two signing secrets" above
COLLECT_SIGNING_KEY="$(openssl rand -hex 24)"

kubectl delete deployment qa-platform-gears qa-platform-ui qa-platform-keycloak -n qa-platform
kubectl delete statefulset qa-platform-postgres -n qa-platform --cascade=orphan
kubectl delete pod qa-platform-postgres-0 -n qa-platform
helm upgrade qa-platform ./deploy/helm/qa-platform -n qa-platform \
  --reuse-values \
  --set "publicOrigin=$PUBLIC_ORIGIN" \
  --set "keycloak.adminPassword=$NEW_ADMIN_PASSWORD" \
  --set "bundleDownloadSigningSecret=$BUNDLE_SIGNING_KEY" \
  --set "collectReportSigningSecret=$COLLECT_SIGNING_KEY"
```

Expect a few minutes of downtime across all four workloads while they
reschedule — this is a single-node dev stack with no highly-available
path to protect (see `ui-deployment.yaml`'s `Recreate` strategy comment
for the same trade-off applied to `ui` alone). What must **not** happen is
data loss on postgres; the sequence above is what prevents it.

## Verifying the migration actually preserved data

Do not consider this migration proven by "the pods came back Ready."
Confirm the database itself survived:

```bash
kubectl exec -n qa-platform qa-platform-postgres-0 -- \
  psql -U "$POSTGRES_USER" -d postgres -c "SELECT count(*) FROM <a table that existed before the upgrade>;"
```

A row count of zero (or the query failing because the table does not
exist) after a "successful" upgrade means the fast route was taken by
mistake and the database was silently replaced with an empty one.
