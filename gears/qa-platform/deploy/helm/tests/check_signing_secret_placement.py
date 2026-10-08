"""Neither HMAC root ships in a ConfigMap, and both still reach the gears.

# The defect this guard closes

`qa-catalog.bundle_download_signing_secret` and
`qa-insights.collect_report_signing_secret` are the ONLY access control on two
genuinely anonymous routes -- `GET /qa/v1/test-bundles/{id}` (`.anonymous()
.exposed()`, because its caller is an Argo runner pod with no user to borrow a
session from) and `POST /qa/v1/collect/{repo_id}` (`.public()`). Both values are
already `required`, so there is no weak default and no per-render random: the
whole defect was PLACEMENT. The template that substitutes them rendered a
`kind: ConfigMap`, and a ConfigMap is readable by anything in the namespace that
can `get configmaps` -- routinely far wider RBAC than `get secrets`. Anyone with
that verb could forge a collect report for any tenant and download any bundle by
id.

`keycloak-realm-secret.yaml` had the identical defect and the identical fix (see
`check_realm_secrecy.py`): same object name, same key, `kind: ConfigMap` ->
`kind: Secret`, `data:` -> `stringData:`, and the mounting pod's volume from
`configMap:` to `secret:`. This guard is the placement half of that treatment
for the gears config.

# Why this is not the same assertion check_bundle_token.py already makes

`check_bundle_token.py::check_signing_secret_is_rewritten` asserts the committed
DEV LITERAL is gone from the render. That is a different property: it catches a
transform that silently stopped firing. It says nothing about WHICH KIND of
object the operator-supplied value lands in, and it passed throughout the entire
life of this defect.

# What this checks

1. No rendered `ConfigMap` contains either signing secret's value. The fixture
   values below are deliberately distinctive strings, so a match is a match and
   not a coincidence.
2. The object carrying the gears config is a `Secret` named
   `qa-platform-gears-config`, keyed `qa-platform-stack.yaml` under
   `stringData:`, with no `data:` block beside it.
3. BOTH values are actually present in that Secret. Without this the guard would
   pass on a render that dropped them entirely -- and dropping them is not a
   safe state that merely fails loudly at install: the gears BOOT and then refuse
   every bundle download and every collect report, which looks like a healthy
   pod.
4. Exactly one pod template reads `qa-platform-gears-config` -- the gears
   Deployment -- by volume, env or envFrom, it does so from a `secret:` volume,
   and none mounts it as a `configMap:` volume. A correct Secret that no
   container reads, beside a leftover ConfigMap that one does, is the same
   defect wearing the fix as a hat.
5. The db-migrate Job mounts `qa-platform-gears-migrate-config` instead, a
   Secret whose body is the gears config with both roots emptied and is
   otherwise byte-identical (since 2026-10-07: `migrate` runs pre_init and the
   migrations, never a gear's init, so it never reads either root).
6. Only the db-migrate Job reads `qa-platform-gears-migrate-config`: with the
   roots emptied it still carries every other credential the gears hold.
7. Both reader scans see every way a pod reads a Secret -- `secret:` and
   `projected:` volumes, env, envFrom -- in every kind that runs one, a bare
   Pod and a CronJob's nested template included; a rogue reader of each form
   is added to the render and must be reported.

Standalone script, like its siblings in this directory -- see the Makefile's
`helm-tests` target for why each one is invoked directly rather than through
pytest collection."""
import contextlib
import io
import pathlib
import subprocess
import sys

import yaml

HERE = pathlib.Path(__file__).resolve().parent
CHART = HERE.parent / "qa-platform"

CONFIG_OBJECT = "qa-platform-gears-config"
CONFIG_KEY = "qa-platform-stack.yaml"
MIGRATE_OBJECT = "qa-platform-gears-migrate-config"
GEARS_DEPLOYMENT = "qa-platform-gears"
MIGRATE_JOB = "qa-platform-db-migrate"
DEV_LITERALS = ("dev-bundle-download-signing-secret", "dev-collect-report-signing-secret")

# Distinctive on purpose: `in` against a whole rendered document is only
# meaningful if the needle cannot occur by accident.
BUNDLE_KEY = "guard-fixture-bundle-root-3f9a2c"   # nosec: test fixture only
COLLECT_KEY = "guard-fixture-collect-root-7d41be"  # nosec: test fixture only
ADMIN_PASSWORD = "guard-fixture-not-a-real-password"  # nosec: test fixture only
# `required` since the workflow-client decision of 2026-09-29; any value
# renders, and this one is obviously not a real credential (the committed dev
# literal is refused outside devMode, which is the point of that change).
WORKFLOW_SECRET = "guard-fixture-not-a-real-workflow-secret"  # nosec: test fixture only

SECRETS = {
    "qa-catalog.bundle_download_signing_secret": BUNDLE_KEY,
    "qa-insights.collect_report_signing_secret": COLLECT_KEY,
}


def render():
    """`helm template` with the minimum a default install needs."""
    cmd = [
        "helm", "template", "signing-placement", str(CHART),
        "--namespace", "qa-platform",
        "--set", "publicOrigin=https://guard.example",
        "--set", f"keycloak.adminPassword={ADMIN_PASSWORD}",
        "--set", f"argo.workflowClientSecret={WORKFLOW_SECRET}",
        "--set", "postgres.password=guard-fixture-not-a-real-db-password",  # nosec: test fixture only
        "--set", f"bundleDownloadSigningSecret={BUNDLE_KEY}",
        "--set", f"collectReportSigningSecret={COLLECT_KEY}",
    ]
    out = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if out.returncode != 0:
        return None, out.stderr
    return [d for d in yaml.safe_load_all(out.stdout) if isinstance(d, dict)], ""


def check_no_configmap_carries_either_root(failures, docs):
    for doc in docs:
        if doc.get("kind") != "ConfigMap":
            continue
        dumped = yaml.dump(doc)
        for name, value in SECRETS.items():
            if value in dumped:
                failures.append(
                    f"FAIL: ConfigMap "
                    f"{doc.get('metadata', {}).get('name')!r} carries "
                    f"{name}. It is the only access control on an anonymously "
                    "reachable route, and `get configmaps` is routinely much "
                    "wider RBAC than `get secrets` -- anyone holding it could "
                    "forge a collect report for any tenant and download any "
                    "bundle by id. Render it into a Secret, the way "
                    "keycloak-realm-secret.yaml and postgres-secret.yaml "
                    "already do.")
    if not failures:
        print("PASS: no rendered ConfigMap carries either HMAC root")


def _config_objects(docs):
    return [d for d in docs
            if d.get("metadata", {}).get("name") == CONFIG_OBJECT]


def check_the_config_object_is_a_secret(failures, docs):
    objects = _config_objects(docs)
    if len(objects) != 1:
        failures.append(
            f"FAIL: expected exactly one object named {CONFIG_OBJECT!r}, "
            f"found {len(objects)}.")
        return None
    obj = objects[0]
    if obj.get("kind") != "Secret":
        failures.append(
            f"FAIL: {CONFIG_OBJECT} is a {obj.get('kind')!r}, not a Secret. "
            "It carries both HMAC roots, so it must be a Secret -- same "
            "object name, same key, `data:` -> `stringData:`, exactly the "
            "conversion keycloak-realm-secret.yaml already went through.")
        return None
    if obj.get("data"):
        failures.append(
            f"FAIL: the {CONFIG_OBJECT} Secret carries a `data:` block beside "
            "`stringData:`. Keep the config in stringData only, the plaintext "
            "form the template already produces.")
    if CONFIG_KEY not in (obj.get("stringData") or {}):
        failures.append(
            f"FAIL: the {CONFIG_OBJECT} Secret has no "
            f"stringData[{CONFIG_KEY!r}] key. entrypoint.sh reads exactly "
            "/etc/cf-gears/qa-platform-stack.yaml, and a volume whose key is "
            "named anything else projects a file the gears never open -- they "
            "fall back to the copy baked into the image and every OIDC token "
            "fails validation.")
        return None
    print(f"PASS: {CONFIG_OBJECT} renders as a Secret keyed {CONFIG_KEY}")
    return obj


def check_both_roots_still_reach_the_gears(failures, config_object):
    """The half that stops this guard passing either way.

    A render that dropped both values would satisfy every ConfigMap assertion
    above. It is not a safe state: qa-catalog and qa-insights both fail CLOSED
    on an empty root, which means the pod boots, reports Ready, and then refuses
    every bundle download and every collect report."""
    if config_object is None:
        return
    body = config_object["stringData"][CONFIG_KEY]
    for name, value in SECRETS.items():
        if value not in body:
            failures.append(
                f"FAIL: the {CONFIG_OBJECT} Secret does not carry {name}'s "
                "value. Both gears fail CLOSED without it -- the pod boots and "
                "reports Ready, then refuses every download and every collect "
                "report, so 'the pod is Running' is not evidence. Moving the "
                "value out of the ConfigMap must not drop it.")
    if not failures:
        print("PASS: both HMAC roots still reach the gears' config")


# Every kind that runs a pod. A bare Pod (a `helm test` hook, say) reads a
# Secret as surely as a Deployment does.
POD_KINDS = ("Pod", "Deployment", "Job", "StatefulSet", "DaemonSet", "ReplicaSet", "CronJob")


def _pod_spec(doc):
    """The pod spec of any POD_KINDS document: a Pod's own `spec`, a CronJob's
    `spec.jobTemplate.spec.template.spec`, everything else's
    `spec.template.spec`."""
    spec = doc.get("spec") or {}
    if doc.get("kind") == "Pod":
        return spec
    if doc.get("kind") == "CronJob":
        spec = (spec.get("jobTemplate") or {}).get("spec") or {}
    return (spec.get("template") or {}).get("spec") or {}


def _pod_sources(doc):
    """Every Secret/ConfigMap a pod reads, by name: `secret:`/`configMap:`
    volumes, the sources of a `projected:` volume, env and envFrom."""
    spec = _pod_spec(doc)
    names = set()
    for volume in spec.get("volumes") or []:
        names.add((volume.get("secret") or {}).get("secretName"))
        names.add((volume.get("configMap") or {}).get("name"))
        for source in (volume.get("projected") or {}).get("sources") or []:
            names.add((source.get("secret") or {}).get("name"))
            names.add((source.get("configMap") or {}).get("name"))
    for c in ((spec.get("containers") or []) + (spec.get("initContainers") or [])
              + (spec.get("ephemeralContainers") or [])):
        for env in c.get("env") or []:
            src = env.get("valueFrom") or {}
            for key in ("secretKeyRef", "configMapKeyRef"):
                names.add((src.get(key) or {}).get("name"))
        for env_from in c.get("envFrom") or []:
            for key in ("secretRef", "configMapRef"):
                names.add((env_from.get(key) or {}).get("name"))
    names.discard(None)
    return names


def check_every_mount_is_a_secret_volume(failures, docs):
    """Only the gears Deployment reads the object that carries both roots, and
    it reads it from a `secret:` volume."""
    before = len(failures)
    readers = []
    for doc in docs:
        if doc.get("kind") not in POD_KINDS:
            continue
        name = doc.get("metadata", {}).get("name")
        spec = _pod_spec(doc)
        for volume in spec.get("volumes") or []:
            as_configmap = [(volume.get("configMap") or {}).get("name")] + [
                (source.get("configMap") or {}).get("name")
                for source in (volume.get("projected") or {}).get("sources") or []]
            if CONFIG_OBJECT in as_configmap:
                failures.append(
                    f"FAIL: {doc['kind']} {name!r} mounts {CONFIG_OBJECT} as a "
                    "`configMap:` volume. The object is a Secret.")
        if doc.get("kind") == "Deployment" and name == GEARS_DEPLOYMENT and not any(
                (v.get("secret") or {}).get("secretName") == CONFIG_OBJECT
                for v in spec.get("volumes") or []):
            failures.append(
                f"FAIL: Deployment/{GEARS_DEPLOYMENT} does not mount {CONFIG_OBJECT} "
                "as a `secret:` volume. A correct Secret nothing reads is not a fix.")
        if CONFIG_OBJECT in _pod_sources(doc):
            readers.append(f"{doc['kind']}/{name}")
    if readers != [f"Deployment/{GEARS_DEPLOYMENT}"]:
        failures.append(
            f"FAIL: {CONFIG_OBJECT} (both HMAC roots) is read by {readers}; exactly "
            f"Deployment/{GEARS_DEPLOYMENT} must read it. A Job that needs the gears "
            f"config mounts {MIGRATE_OBJECT}, which carries neither root.")
    elif len(failures) == before:
        print(f"PASS: only Deployment/{GEARS_DEPLOYMENT} reads {CONFIG_OBJECT}, "
              "from a secret volume")


def check_only_the_migrate_job_reads_the_migrate_config(failures, docs):
    """The migrate config is the gears config minus the two roots -- every
    other credential in it (the database DSN, the Keycloak client) is still
    there. It exists for one Job; any other reader is a pod given the gears'
    credentials through a side door."""
    readers = [f"{d['kind']}/{d.get('metadata', {}).get('name')}" for d in docs
               if d.get("kind") in POD_KINDS and MIGRATE_OBJECT in _pod_sources(d)]
    if readers != [f"Job/{MIGRATE_JOB}"]:
        failures.append(
            f"FAIL: {MIGRATE_OBJECT} is read by {readers}; exactly Job/{MIGRATE_JOB} "
            "must read it.")
    else:
        print(f"PASS: only Job/{MIGRATE_JOB} reads {MIGRATE_OBJECT}")


def check_the_reader_scan_sees_every_form(failures, docs):
    """The two reader checks above are only as good as `_pod_sources`: a mount
    it cannot see passes them. Each rogue reader below is added to the real
    render, and each must be reported -- through a `projected:` volume, from a
    bare Pod, and from a CronJob's nested template."""
    def pod(kind, name, volume):
        spec = {"containers": [{"name": "c"}], "volumes": [volume]}
        if kind == "Pod":
            return {"kind": kind, "metadata": {"name": name}, "spec": spec}
        if kind == "CronJob":
            return {"kind": kind, "metadata": {"name": name},
                    "spec": {"jobTemplate": {"spec": {"template": {"spec": spec}}}}}
        return {"kind": kind, "metadata": {"name": name},
                "spec": {"template": {"spec": spec}}}

    def projected(secret):
        return {"name": "p", "projected": {"sources": [{"secret": {"name": secret}}]}}

    def plain(secret):
        return {"name": "s", "secret": {"secretName": secret}}

    rogues = [
        (check_every_mount_is_a_secret_volume, pod("Deployment", "rogue-projected", projected(CONFIG_OBJECT))),
        (check_every_mount_is_a_secret_volume, pod("Pod", "rogue-pod", plain(CONFIG_OBJECT))),
        (check_every_mount_is_a_secret_volume, pod("CronJob", "rogue-cron", plain(CONFIG_OBJECT))),
        (check_only_the_migrate_job_reads_the_migrate_config, pod("Deployment", "rogue-projected", projected(MIGRATE_OBJECT))),
        (check_only_the_migrate_job_reads_the_migrate_config, pod("Pod", "rogue-pod", plain(MIGRATE_OBJECT))),
    ]
    before = len(failures)
    for check, rogue in rogues:
        probe = []
        # The probe's own PASS lines are not this check's; keep them quiet.
        with contextlib.redirect_stdout(io.StringIO()):
            check(probe, [*docs, rogue])
        if not any(f"{rogue['kind']}/{rogue['metadata']['name']}" in line for line in probe):
            failures.append(
                f"FAIL: {check.__name__} did not report a rogue {rogue['kind']} "
                f"{rogue['metadata']['name']!r} -- the reader scan misses that form.")
    if len(failures) == before:
        print("PASS: the reader scan reports a rogue reader through a projected "
              "volume, from a bare Pod and from a CronJob")


def check_migrate_job_gets_no_root(failures, docs, config_object):
    """The migrate Job used to mount the whole gears config, both roots
    included, and never reads either (`migrate` runs pre_init and the
    migrations, never a gear's init)."""
    before = len(failures)
    jobs = [d for d in docs if d.get("kind") == "Job"
            and d.get("metadata", {}).get("name") == MIGRATE_JOB]
    if len(jobs) != 1:
        failures.append(f"FAIL: expected one Job {MIGRATE_JOB}, found {len(jobs)}")
        return
    spec = jobs[0]["spec"]["template"]["spec"]
    mounted = {(v.get("secret") or {}).get("secretName") for v in spec.get("volumes") or []}
    if MIGRATE_OBJECT not in mounted:
        failures.append(
            f"FAIL: {MIGRATE_JOB} does not mount {MIGRATE_OBJECT}; entrypoint.sh "
            "would render the config baked into the image and migrate against localhost.")
    migrate = [d for d in docs if d.get("metadata", {}).get("name") == MIGRATE_OBJECT]
    if len(migrate) != 1 or migrate[0].get("kind") != "Secret" \
            or CONFIG_KEY not in (migrate[0].get("stringData") or {}):
        failures.append(
            f"FAIL: expected one Secret {MIGRATE_OBJECT} keyed {CONFIG_KEY} "
            "under stringData.")
        return
    body = migrate[0]["stringData"][CONFIG_KEY]
    for needle in (*SECRETS.values(), *DEV_LITERALS):
        if needle in body:
            failures.append(f"FAIL: {MIGRATE_OBJECT} carries {needle!r}.")
    if config_object is not None:
        expected = config_object["stringData"][CONFIG_KEY]
        for value in SECRETS.values():
            expected = expected.replace(value, "")
        if body != expected:
            failures.append(
                f"FAIL: {MIGRATE_OBJECT} differs from {CONFIG_OBJECT} by more than "
                "the two emptied roots -- the migrate config must come from the "
                "same pipeline (discovery_url, metrics, SMTP rewrites included).")
    if len(failures) == before:
        print(f"PASS: {MIGRATE_JOB} mounts {MIGRATE_OBJECT}, which is the gears "
              "config with both roots emptied")


def main():
    failures = []
    docs, stderr = render()
    if docs is None:
        print(f"FAIL: helm template failed:\n{stderr}", file=sys.stderr)
        return 1
    check_no_configmap_carries_either_root(failures, docs)
    config_object = check_the_config_object_is_a_secret(failures, docs)
    check_both_roots_still_reach_the_gears(failures, config_object)
    check_every_mount_is_a_secret_volume(failures, docs)
    check_only_the_migrate_job_reads_the_migrate_config(failures, docs)
    check_the_reader_scan_sees_every_form(failures, docs)
    check_migrate_job_gets_no_root(failures, docs, config_object)
    if failures:
        for line in failures:
            print(line, file=sys.stderr)
        return 1
    print("check_signing_secret_placement: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
