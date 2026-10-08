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
`gears-config-secret.yaml` substituted a per-render `randAlphaNum 32`
for each unset secret, so **every** `helm upgrade` of such a release had
already been silently rotating both keys. Each one is the only access
control on an anonymously reachable route, so a rotation invalidates
every `?sig=` minted before it: a test bundle built before the upgrade
and fetched after it gets a 403 and `fetch_bundle.py` exits 1 before a
single test runs.

MEASURED on the dev stand, and the reason this changed: after
`helm upgrade --reuse-values --set bundleDownloadSigningSecret=...`
produced revision 29, the rendered config held the new key and the gears pod
was still 38 minutes old, still signing with the old one. That second
defect is fixed too (the pod template now carries `checksum/*`
annotations, so a config change rolls the Deployment) — which makes the
rotation take effect immediately rather than at the next restart, and is
why leaving the random in place was no longer an option.

### What to do

Generate one value per secret, once, and keep them. Each must be at least 16
characters once trimmed; since 2026-10-07 a shorter value fails the render,
naming the value. `openssl rand -hex 24` gives 48.

```bash
BUNDLE_SIGNING_KEY="$(openssl rand -hex 24)"
COLLECT_SIGNING_KEY="$(openssl rand -hex 24)"
```

Pass both on the upgrade (they join `--reuse-values`' stored values, so
subsequent upgrades do not need them again):

```bash
helm upgrade qa-platform ./deploy/helm/qa-platform -n qa-platform \
  --reuse-values \
  --set-file bundleDownloadSigningSecret=<(printf '%s' "$BUNDLE_SIGNING_KEY") \
  --set-file collectReportSigningSecret=<(printf '%s' "$COLLECT_SIGNING_KEY")
```

**Do this when no run is in flight.** Any workflow already holding a
`?sig=` built with the old key fails its bundle fetch. That was true of
the old random fallback as well; the only difference is that now somebody
chose the moment.

If you want to keep the key a release is *currently* using rather than
issue a new one, read it out of the live object first — it is in the
gears config under `qa-catalog.bundle_download_signing_secret` and
`qa-insights.collect_report_signing_secret`. That object is a **Secret**
since 2026-09-29 (see the next section), so it is `get secret` and a
base64 decode, not `get configmap`:

```bash
kubectl get secret qa-platform-gears-config -n qa-platform \
  -o jsonpath='{.data.qa-platform-stack\.yaml}' | base64 -d \
  | grep -E 'bundle_download_signing_secret|collect_report_signing_secret'
```

Note that on a release that never set them this reads back whatever the
LAST render happened to generate, which is not necessarily what the
running pod is using — that is the pair of defects above, seen from the
other side.

## Also required, from 2026-09-29: `argo.workflowClientSecret`

It was the bare literal `qa-platform-workflow-dev-secret` in `values.yaml`,
with no `required` and no gate, and `deploy/remote/verify-k8s.sh` hardcoded
the same string twice. The `qa-platform-workflow` client is
`serviceAccountsEnabled` and `fullScopeAllowed`, so a client-credentials
token minted with that string carries full tenant access — and the client
sits **outside** the realm file's `{{if devMode}}` block, so every install
got it, not just dev stands.

The chart now `required`s the value, and separately refuses that literal
unless `devMode=true` — the same pair `keycloak.adminPassword` already had.

```bash
helm upgrade qa-platform ./deploy/helm/qa-platform -n qa-platform \
  --reuse-values \
  --set-file argo.workflowClientSecret=<(printf '%s' "$(openssl rand -hex 24)")
```

**Two things to know before you run it.**

First, `--reuse-values` does NOT surface this as a missing value. Helm
re-coalesces the *previous* release's chart defaults and carries them
forward as user-supplied values, so a release that never set this one
inherits the OLD chart's committed literal and the `required` never fires.
What then fires is the devMode refusal — loudly, and naming both the value
and `devMode` — on any release that is not `devMode=true`. On a `devMode`
stand the literal simply survives the upgrade, which is the intended
outcome there; set the value explicitly if you would rather it did not.
Measured on the dev stand, 2026-09-29. An upgrade **without**
`--reuse-values` fails on the `required` as you would expect.

Second, Keycloak imports a realm only on a FIRST start against an empty
database, so changing this value is not enough on its own — the pod has to
restart, at which point its ephemeral H2 store is empty and the realm
re-imports with the new secret. `keycloak-deployment.yaml` now carries a
`checksum/realm` pod annotation for exactly that, so a changed realm rolls
the pod by itself. That restart ends every Keycloak session; on a release
where that matters, pick the moment.

From 2026-10-07 the value must also be at least 16 characters once trimmed
(devMode included), and a realm supplied through `keycloakRealmJson` is held
to the same rules: the development literal anywhere in it is refused outside
devMode, and a `qa-platform-workflow` client whose `secret` is under 16
characters is refused.

## Also required, from 2026-10-07: `postgres.password`

`values.yaml` shipped `postgres.password: qa` with no refusal, so every install
that did not override it ran its database as `qa`/`qa`. The value now has no
default and `required`s one, and `qa` is refused unless `devMode=true`. The
chart has six required values: `publicOrigin`, `keycloak.adminPassword`,
`bundleDownloadSigningSecret`, `collectReportSigningSecret`,
`argo.workflowClientSecret` and `postgres.password`.

**Postgres applies this password only when it initialises an empty data
directory.** Your database keeps the password it was created with, so a new
value in the chart alone leaves the gears unable to log in. Change the role's
password inside Postgres first, then pass the same value to the upgrade:

```bash
POSTGRES_PASSWORD="$(openssl rand -hex 24)"
printf "ALTER ROLE qa PASSWORD '%s';\n" "$POSTGRES_PASSWORD" \
  | kubectl -n qa-platform exec -i qa-platform-postgres-0 -- \
      psql -U qa -d postgres -v ON_ERROR_STOP=1 -f -
helm upgrade qa-platform ./deploy/helm/qa-platform -n qa-platform \
  --reuse-values \
  --set-file postgres.password=<(printf '%s' "$POSTGRES_PASSWORD")
```

(The statement travels on `psql`'s stdin and the value reaches helm as a
`--set-file` from the `printf` builtin, so the password is not in any
process's argv. Replace `qa` with your `postgres.user` if you changed it.)
Between the `ALTER ROLE` and the upgrade, connections the gears already hold
keep working and new ones are refused; the upgrade rolls the gears pod
(`checksum/postgres-secret`) onto the new value.

Under `--reuse-values` a release that never set the value inherits the old
chart's `qa`, so the upgrade fails on the devMode refusal, naming
`postgres.password` and `devMode`; without `--reuse-values` it fails on the
`required`. A `devMode=true` stand keeps `qa` and needs nothing.

## The gears config is a Secret now, not a ConfigMap (2026-09-29)

`qa-platform-gears-config` was a `ConfigMap`. Its body carries both HMAC
roots above, and those two values are the only access control on the two
routes that are anonymous by design — so anyone in the namespace holding
`get configmaps`, routinely far wider RBAC than `get secrets`, could forge
a collect report for any tenant and download any bundle by id.

**Nothing about an upgrade changes for you.** The object keeps its name
and its `qa-platform-stack.yaml` key, the gears Deployment mounts it from
a `secret:` volume at the same `/etc/cf-gears` path, and Helm replaces the
old ConfigMap with the new Secret on the upgrade. The only thing that
changes for an operator is how you read it back — `get secret` plus
`base64 -d`, as in the snippet above.

Since 2026-10-07 the db-migrate Job mounts `qa-platform-gears-migrate-config`
instead — the same config with both roots emptied, because `migrate` never
runs the code that reads them. Helm creates it on the upgrade; nothing for
you to do.

This is the same conversion `qa-platform-realm` went through earlier, for
the same reason. `deploy/helm/tests/check_signing_secret_placement.py` is
the guard that keeps it converted.

## The runner-Secret writer has its own ServiceAccount (2026-09-30)

qa-environments' D4 runner-Secret writer now authenticates as a dedicated
ServiceAccount, `qa-platform-secret-writer`. The `qa-platform-gears-executor`
Role, bound to `qa-platform-gears`, loses its `secrets` `create`/`patch` rule
in the Argo namespace (finding #94). qa-runs keeps running as
`qa-platform-gears` and needs no Secret.

**No values change.** The upgrade creates these objects before the gears pod
restarts:
- in the release namespace, the `qa-platform-secret-writer` ServiceAccount
  and its `kubernetes.io/service-account-token` Secret
  `qa-platform-secret-writer-token`;
- in the Argo namespace, the `qa-platform-secret-writer` Role and
  RoleBinding;
- on a cluster that serves `admissionregistration.k8s.io/v1`
  ValidatingAdmissionPolicy (Kubernetes 1.30 and later), the cluster-scoped
  policy and binding `qa-platform-secret-writer-guard-<release namespace>`.
  They limit the writer to Opaque Secrets whose names start with
  `qa-platform-`. On an older cluster neither renders, and the writer keeps
  the whole namespace-wide grant (see ADR-0008, "Consequences").

The Kubernetes token controller then fills the token Secret. The token
volume is `optional: true`, so a gears pod that starts before that happens
still starts. Until the kubelet refreshes the volume (about a minute), only
the D4 writer fails, with a named `failed to build config from argo
kubeconfig` error, and the next observation cycle repairs it.

Between the old Role losing `secrets` and the new pod starting, a D4 write
from the old pod is Forbidden. The next observation cycle repairs it, the
same way it repairs any failed write.

To check it afterwards, run `deploy/remote/verify-k8s.sh`. Its `k8s 3/4`
step requires
`kubectl auth can-i create secrets -n argo --as=system:serviceaccount:<ns>:qa-platform-secret-writer`
to say `yes`, and the same question for `qa-platform-gears` to say `no`.
Where the policy API is served, it also dry-runs one allowed write and two
denied ones as the writer.

**To revoke the writer's credential, delete `qa-platform-secret-writer-token`.**
The API server stops accepting that token at once. Because the volume is
optional, the gears keep running and restarting normally, and only the D4
writer fails, with the same named error, until a new token exists. The next
`helm upgrade` recreates the Secret, and the controller fills it with a new
token.

## Third-party images are digest-pinned (2026-09-30)

`images.postgres`, `images.keycloak` and `images.kubectl` now carry a `digest`
beside the `tag`, and the chart renders `repository:tag@sha256:...`. `postgres`
moved from the floating `16` to `16.15` (the tag `16` resolved to when the
digest was taken); the same postgres image also runs the cert and seed jobs.
No migration is needed. To use a different image, set the tag and digest
together, e.g. `--set images.postgres.tag=16.16 --set images.postgres.digest=sha256:...`;
`--set images.postgres.digest=` renders the bare tag (an unpinned image, for a
cluster that mirrors images under its own tags). Also new: the gears'
`api-gateway` log target is `info`, not `debug`, because its debug lines name
every request path and the Slack webhook path is a credential.

## Chart and API changes from the 2026-09-30 closure round

**`gears.allowVerboseProxyLogs` (new value, default `false`).** The render now refuses a
`gears.logLevel` that makes the global level, or a `pingora`, `oagw`, `cf_gears_oagw` or
`api-gateway` (also spelled `api_gateway`) target, more verbose than `info`. The value is read the
way `EnvFilter` reads it: comma-separated directives, each `level`, `target=level` or a bare
`target` (which means trace); levels are case-insensitive and may be the digits 0 to 5 (4 and 5 are
debug and trace); a `[span]` suffix and a `::module` tail on a target are ignored; a directive with
no target, such as `[span]=debug`, counts as global; and targets match by prefix both ways, so
`ping=debug` (which reaches pingora) and `pingora_core=debug` (which sits inside it) are refused
too. The reason is the
Slack webhook: its credential is the URL path, and pingora's debug and trace dumps print whole
request lines, so `--set gears.logLevel=debug` would write webhook credentials into the logs. A
release that already carries such a level, through `--reuse-values` or a values file, **fails to
render** after the upgrade until the level is lowered or `--set gears.allowVerboseProxyLogs=true`
is passed. Set the opt-in only to debug the proxy path itself, knowing the logs then carry the
credentials. Other targets, such as `qa_runs=debug`, stay free.

**The Keycloak realm is rendered as JSON now.** Every placeholder in the realm file goes through
`toJson`, so a `publicOrigin`, `argo.workflowClientSecret` or seeded tenant id containing a `"` or
a `\` renders a valid realm. Before, such a value produced invalid JSON and the realm import
failed. Nothing changes for values without those characters.

**`images.keycloak.tag` is a concrete patch tag, `26.0.8`** (it tracked `26.0`), beside its digest.
Set the tag and digest together if you override it, as for the other third-party images.

**Three response fields are closed enums in the OpenAPI document.** `AnalyticsOverviewDto.group_by`
(`none`, `component`, `tag`, `environment`), `EnvironmentDto.health_state` (`ok`, `degraded`,
`down`, `unknown`) and `QueueEntryDto.run_kind` (`plan`, `test`, `custom_plan`, `collect`) were
plain strings. The wire strings are the same, so no client changes behaviour; a client generated
from `docs/openapi.json` now gets a union type instead of `string`.

**A new claim kind in `qa_run_notifications`: `run_completed_history_audit`.** A run that finished
before the notification cutoff is audited once, with one `skipped` row in `qa_notification_log`,
instead of once per sweep tick. The claim that makes it once is recorded under this kind. It is not
a channel: neither the Slack nor the email slot is taken, so moving the cutoff later still lets
such a run be announced. Expect one such row per history run.

## Run notifications start now, and Slack finally delivers (2026-09-30)

Applies to: any deployment that runs qa-insights, whatever version it upgrades from.

**What was true before.** `notify_run_completed` had no production caller until 2026-09-29
(commit `c5bcf0a24`, the reconcile sweep), so a deployment older than that **announced nothing, on
any channel, for any run**. Slack could not have delivered even then: the webhook's credential is
its URL path, which the gateway cannot inject, and the adapter only became able to deliver on
2026-09-30 (`7267e6777`). The routing that existed between `c5bcf0a24` and `4d563bb3f` (the same
day) was narrower: only scheduled runs notified, the schedule's Slack flag gated email as well as
Slack, and `notify_on_failure`/`notify_on_success` were stored and read by nothing. Only a build
from that window ever behaved that way.

**What is true now.** Every run finished after the deployment's notification cutoff is announced
over each channel the tenant has switched on, ad-hoc runs included, and email is not gated by the
schedule's Slack flag. The two outcome flags gate **both** channels
(`domain/notify/routing.rs`, `RunOutcome::notifies`): a failed run notifies if `notify_on_failure`
is set, a passing one if `notify_on_success` is, and a run that is neither if either is. Runs that
finished before the cutoff are never announced, so an upgrade does not mail history.

**Check each tenant's stored settings.** Through the settings API a tenant may have stored
`notify_on_failure = false`, which until now did nothing; it now means no failure notifications on
Slack or email. After upgrading, read each tenant's settings
(`GET /qa/v1/settings/notifications`) and confirm `notify_on_failure` and `notify_on_success` are
what the tenant wants. With both off, nothing is announced on any channel.

Migration `m20260929_000007_opt_existing_tenants_into_success_notifications` sets
`notify_on_success = true` on rows that were `false` and that were created before the migration
ran, so existing tenants hear about passing runs. It never touches `notify_on_failure`.

### Slack: what the credential-store secret must hold

The credential-store secret named by the tenant's `slack_webhook_credstore_ref` must hold the
**full** incoming-webhook URL, `https://hooks.slack.com/services/T.../B.../...`. A bare token or a
reference to a different kind of secret is refused before anything is dialled, with one message for
"absent" and "not a Slack webhook" alike. After the upgrade Slack actually delivers: each tenant
gets a no-auth `hooks.slack.com` upstream and a `POST /services` route in oagw, provisioned by the
first send.

**A foreign `hooks.slack.com` upstream is refused, not reused.** If the tenant already has an oagw
upstream aliased `hooks.slack.com` that this gear did not provision (a different endpoint, any
auth, plugins or request-header rules, or a route carrying plugins), every Slack send fails with
`the tenant's gateway route to hooks.slack.com is not the one this gear provisions ... nothing was
sent`, and the log carries a `refusing a Slack send` warning naming the upstream id. The settings
page's test send shows the same error. To clear it, delete that upstream (and its routes) through
the oagw upstream API as a principal with upstream-management rights in the tenant, or correct it
to the no-auth `https://hooks.slack.com:443` shape. The next send provisions the tenant's own
upstream again, including after an upstream is deleted while the gears keep running.

**Share the secret at tenant level.** Notifications read the webhook (and the SMTP password) as the
qa-insights system actor, not as the user who stored it, so a secret created with `private` sharing
is never found by a send. Store it with `tenant` sharing (or `shared` from a parent tenant). The
settings page's test send now reads it the same way, so it fails for a `private` secret too, with a
message that says so.

## A read of a branch syncs it on first use (2026-10-06)

Applies to: any deployment, whatever version it upgrades from. No chart value, migration or manual
step is involved.

Reading the content of a branch that has no work tree on disk (`GET /qa/v1/plans`, a single plan,
`TEST_META`, building a test bundle, and the target resolution of a run launch) no longer fails with
`400` "has no synced content" until someone calls `POST /qa/v1/test-repos/{id}/sync`. The catalog
confirms the branch on the remote with a ref listing, then syncs it without `force` and serves the
read. Behaviour to know when upgrading:

* A branch the remote does not have is `404` ("Branch '<b>' does not exist in repository <id>"). It
  never reaches the sync engine and does not set `sync_error`.
* The sync runs under the reader's own security context, so the reader needs `SYNC` on the
  repository. A user who could launch but lacks `SYNC` used to get `400` on an unsynced branch and
  now gets `403`. The same holds for every read while the repository's `sync_error` is set, since
  those reads re-sync. A branch that already has a work tree on a repository with no `sync_error`
  is served without `SYNC` and without network, as before.
* Two syncs, first reads or branch refreshes of one repository at the same time no longer fail on
  the branch cache (`409`, or an opaque `500` through qa-runs): the cache is written as an
  idempotent diff. `POST /qa/v1/test-repos/{id}/sync` no longer declares `409`.
* Branch-cache rows are now filed under the repository's owning tenant, whoever's request wrote
  them. A row of a child tenant's repository that an earlier caller scoped to a parent tenant
  filed under the parent is removed only by a sync or refresh made by a caller whose scope covers
  the parent. The periodic branch refresher is scoped to the owning (child) tenant alone, so it
  does not remove it. It is harmless to the child, whose scope does not see it. Until such a sync,
  a caller whose scope spans both tenants may see that branch name listed twice.
* The first read of a branch waits for a fetch and checkout inside the request. A remote that
  cannot be listed (unreachable, timing out, failing, or answering HTTP `403`) answers `503` and
  records nothing. At launch qa-runs reports a `503` as an opaque `500`. A credential the catalog
  cannot resolve, or one the remote refuses, is recorded in the repository's `sync_error` and
  answered `400` with that reason, as `POST /qa/v1/test-repos/{id}/sync` records it; qa-runs
  relays that `400` and its sentence. Until the 2026-10-07 change both answered `503` and recorded
  nothing. After either failure the repository is backed off for `remote_failure_backoff_seconds`
  (new key, default 30, `0` disables), per replica: reads that would sync it give the same answer
  without contacting the remote. A failure found by the periodic branch refresher starts no
  backoff. Changing the repository's credential or url, or forcing a sync
  (`POST /qa/v1/test-repos/{id}/sync`), ends the backoff at once — that is the step after fixing a
  credential.
* A launch that fails for a catalog reason now answers the catalog's status and sentence: `404` for
  a missing branch or plan, `400` for a precondition or argument refusal, `403` for a permission
  refusal. Only a catalog fault still answers an opaque `500`. A run's recorded `Error`
  carries a fixed sentence naming the refusal's category, not the catalog's own text (which can
  name the remote), and `POST /qa/v1/queue/{id}/force-start` can answer `400` or `404` for it. Schedule create and update turn
  a missing plan or branch into a `400` field validation on `target.path`, not a `404`.
* The analytics universe walk is unchanged: it skips a repository that is not synced
  (`last_synced_at` unset or `sync_error` set) or has no work tree for the selected branch, and
  never syncs it.
* `branch_freshness_ttl_seconds` now bounds the fetch of these first-read syncs as well as explicit
  non-forced ones, except while the repository's `sync_error` is set: then a read fetches its
  branch again even inside the window, and the successful sync clears the error. The explicit sync
  and run dispatch still force-sync.

## Git syncs are bounded in time and size (2026-10-07)

Applies to: any deployment. Four new qa-catalog keys, all with defaults:
`sync_timeout_seconds` (300), `ls_refs_timeout_seconds` (30), `max_fetch_bytes` (1 GiB) and
`max_checkout_bytes` (512 MiB); `0` disables each. A sync or branch listing that runs past its
deadline now fails as a timeout instead of running on, and the repository is backed off for
`remote_failure_backoff_seconds`. A branch listing that times out answers `503`, and reads inside
the backoff answer `503` without contacting the remote. A content sync that times out is recorded
in `sync_error`, so the read that ran it answers `400` with that reason, and reads inside the
backoff answer that same `400` at once, without contacting the remote or running another sync;
after the backoff the next read syncs again. A repository whose fetch or
checkout is larger than the limits now fails with a `sync_error` that names the key to raise;
raise it in the gears config and force a sync. ssh remotes without a key now run with
`BatchMode=yes` and connection timeouts too: one that relied on an interactive prompt, which could
never be answered in the pod, now fails at once instead of hanging.

## Credential references have one spelling in every gear (2026-10-07)

Applies to: any deployment. No chart value or manual step is involved.

qa-insights used to accept a `cred://` prefix on the JIRA token, Slack webhook and SMTP password
references and stripped it; qa-environments and qa-catalog never did. Now none does: a reference is
letters, digits, `_` and `-`, 1 to 255 characters, and `PUT /qa/v1/settings/jira` and
`PUT /qa/v1/settings/notifications` answer `400` naming the field for `cred://name`. Migration
`m20261007_000008_bare_credstore_refs` rewrites every stored `cred://name` to `name` on upgrade, so
the settings pages show the bare name afterwards and nothing that worked stops working. Scripts
that `PUT` these settings with a `cred://` value must drop the prefix.

## Concurrent writes no longer fail or duplicate, and a refused JIRA credential is named (2026-10-07)

Applies to: any deployment. No chart value or manual step is involved.

* Two `PUT /qa/v1/variables` requests for one new variable name at the same time both answer `200`;
  before, one answered `409`. The later value is the one stored.
* A run's results are no longer stored twice when the reconcile sweep and
  `POST /qa/v1/insights/rebuild` project it at the same time (or two gears processes do, on a
  deployment that runs more than one). Migration `m20261007_000009_run_projection_locks` adds the
  per-run lock table and deletes the older copy of every run that was already stored twice, keeping
  each run's newest batch. Dashboards and analytics over affected runs show smaller, correct counts
  after the upgrade. The chart's gears Deployment uses `strategy: Recreate` with one replica, so no
  pod on the old image runs beside the new one: the cleanup runs before the new pod serves, and from
  then on every write of a run's results takes the lock. The cleanup reads each result table once;
  measured on Postgres 15, it took 165 ms over 255,000 rows.
* A JIRA credential that JIRA refuses (`401`/`403`) is now counted as
  `qa_insights_jira_bug_total{outcome="status_check_refused"}` and logged at error, where it used to
  be `status_check_failed` like an outage. An alert on `status_check_failed` alone no longer sees a
  refused credential; add the new value. A poll pass logs the first refusal at error and every
  further refusal of the same pass at warn, so one refused credential is one error line per pass,
  not one per open bug.
* `POST /qa/v1/jira/bugs` tries the run's failed tests in `test_name` order and stops at the first
  one for which the gateway cannot reach JIRA, the call times out, or JIRA refuses the credential:
  the tenant has one JIRA endpoint and one credential, so the rest cannot succeed, and trying them
  all outlived the API gateway's 30 s request timeout (the caller saw a `504`). The tests not
  attempted are logged once at warn. JIRA refusing one issue's request does not stop the others.
  Within one attempt, a dedupe search that meets such a failure ends the attempt and no create is
  sent; a search JIRA answers with any other error still falls through to the create, as before.
  When no test was filed or found, the answer is `503` (an upstream that did not deliver, naming
  the `jira` channel and its failure class: the failure that stopped the attempts, else the first
  refused request). Before, this answer was `200` with an empty list. When a test was filed or
  found, the answer stays `200` with those entries, and each failed test is left out and logged at
  error, as before. The poller's
  status check, which has no HTTP answer of its own, classifies the same failures the same way in
  its log and its metric. A client that read an empty `200` from this endpoint as "nothing to
  file" must handle `503`.

## Repository and environment credentials are read as the gear's own identity (2026-10-07)

Applies to: a test repository whose `credential_ref`, or an environment whose credential reference,
names a secret stored with `private` sharing.

qa-catalog and qa-environments used to read such a secret as the user who triggered a sync or a
refresh, and as the gear's system actor in the background. The owner's own sync or refresh worked,
and every background branch refresh or observation failed. Both now read it as the system actor
every time, in the tenant that owns the repository or environment, so the owner's sync or refresh
fails too, with a reason naming the sharing mode. Re-store the secret with `tenant` sharing (or
`shared` from a parent tenant). SSH keys and kubeconfigs the gears store themselves use `tenant`
sharing, so for a caller in the resource's own tenant they are unaffected.

A caller in a parent tenant whose scope reaches a child tenant's repository or environment is the
exception. An SSH key it registers, or a kubeconfig it pastes into the child's environment, is
stored in the **parent's** tenant, which the system actor reading in the child's tenant cannot see.
That already failed every background branch refresh or observation; now the parent's own sync or
refresh fails too, with the same reason. Changing the sharing mode does not help: store the secret
in the owning (child) tenant — register the SSH key, or save the environment's kubeconfig, as a
user of that tenant.

A repository sync that credstore refused used to answer `403`; it is now a recorded sync failure
(`400` on a read) carrying that reason.

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
want: keep everything else, override only the values below.

**Pass `argo.workflowClientSecret` here too.** Under `--reuse-values` a release
that is not `devMode=true` inherits the OLD chart's committed literal for it,
and the render then refuses that literal (see "Also required, from 2026-09-29:
`argo.workflowClientSecret`" above), so an upgrade without it fails. The
examples below pass it.

Pass `postgres.password` too, after changing the role's password as in "Also
required, from 2026-10-07: `postgres.password`" — do that BEFORE the deletes
in the two sections above, while `qa-platform-postgres-0` is still running
(the "Full sequence" below has it in that order).

The five secrets go in with `--set-file` from a bash process substitution,
not `--set`: `--set KEY=VALUE` puts the value in helm's argv, readable by
anyone on the machine through `ps` or `/proc/<pid>/cmdline` for the life of
the upgrade. `printf` is a bash builtin, so the value reaches no process's
argv, and `--set-file` takes the bytes verbatim, as a string. Run these from
bash, not `sh`.

```bash
helm upgrade qa-platform ./deploy/helm/qa-platform -n qa-platform \
  --reuse-values \
  --set "publicOrigin=$PUBLIC_ORIGIN" \
  --set-file keycloak.adminPassword=<(printf '%s' "$NEW_ADMIN_PASSWORD") \
  --set-file bundleDownloadSigningSecret=<(printf '%s' "$BUNDLE_SIGNING_KEY") \
  --set-file collectReportSigningSecret=<(printf '%s' "$COLLECT_SIGNING_KEY") \
  --set-file argo.workflowClientSecret=<(printf '%s' "$WORKFLOW_CLIENT_SECRET") \
  --set-file postgres.password=<(printf '%s' "$POSTGRES_PASSWORD")
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
WORKFLOW_CLIENT_SECRET="$(openssl rand -hex 24)"   # see "argo.workflowClientSecret" above
POSTGRES_PASSWORD="$(openssl rand -hex 24)"         # see "postgres.password" above

printf "ALTER ROLE qa PASSWORD '%s';\n" "$POSTGRES_PASSWORD" \
  | kubectl -n qa-platform exec -i qa-platform-postgres-0 -- \
      psql -U qa -d postgres -v ON_ERROR_STOP=1 -f -
kubectl delete deployment qa-platform-gears qa-platform-ui qa-platform-keycloak -n qa-platform
kubectl delete statefulset qa-platform-postgres -n qa-platform --cascade=orphan
kubectl delete pod qa-platform-postgres-0 -n qa-platform
helm upgrade qa-platform ./deploy/helm/qa-platform -n qa-platform \
  --reuse-values \
  --set "publicOrigin=$PUBLIC_ORIGIN" \
  --set-file keycloak.adminPassword=<(printf '%s' "$NEW_ADMIN_PASSWORD") \
  --set-file bundleDownloadSigningSecret=<(printf '%s' "$BUNDLE_SIGNING_KEY") \
  --set-file collectReportSigningSecret=<(printf '%s' "$COLLECT_SIGNING_KEY") \
  --set-file argo.workflowClientSecret=<(printf '%s' "$WORKFLOW_CLIENT_SECRET") \
  --set-file postgres.password=<(printf '%s' "$POSTGRES_PASSWORD")
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
