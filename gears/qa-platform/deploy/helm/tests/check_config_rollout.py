"""A config change rolls the gears Deployment, and an identical upgrade does not.

# What this guard is for

A ConfigMap is not part of a Deployment's pod template. Changing one changes
nothing Kubernetes can observe, so `helm upgrade` writes the new object,
reports success, and the running pod keeps serving the values it read at
start-up. MEASURED on the dev stand 2026-09-21: setting
`bundleDownloadSigningSecret` produced revision 29 with the new value in
`qa-platform-gears-config`, and the gears pod was still 38 minutes old
afterwards, still signing bundles with the old key. Nothing anywhere said so.

`gears-deployment.yaml` now hashes each rendered config source into a
`checksum/*` pod annotation, which puts the ConfigMap's content inside the pod
template and makes a config change a rollout. This guard holds the three
properties that makes true, because each one fails silently on its own:

1. **Every config source the gears pod consumes is covered.** An annotation
   that exists but does not name the ConfigMap somebody added last week is
   worth nothing, and nothing else in the tree would notice -- the Deployment
   renders, the volume mounts, the pod runs the stale value exactly as before.

2. **Two identical renders agree.** This is the half that is easy to lose. The
   two signing secrets used to default to a per-render `randAlphaNum 32`; with
   the checksum in place, that fallback would give *every* `helm upgrade` a
   different ConfigMap and a different hash, so a no-op upgrade would roll the
   pod -- and this Deployment is `strategy: Recreate`, so that is a real
   outage, taken for nothing, on every deploy.

3. **A changed value changes the hash.** The property the whole thing exists
   for. Asserted against the one value whose staleness was actually observed.

And the two `required`s that property 2 depends on are asserted directly, so
that reinstating a default here fails loudly rather than quietly restoring
property 2's failure mode.
"""
import pathlib
import subprocess
import sys

import yaml

HERE = pathlib.Path(__file__).resolve().parent
CHART = HERE.parent / "qa-platform"
RELEASE = "rollout-guard"
ORIGIN = "https://guard-config-rollout.invalid"

# Obviously-not-real fixtures. Both signing secrets have no default; qa-catalog
# additionally refuses a bundle key shorter than 16 characters, so these are
# comfortably longer than that.
ADMIN_PASSWORD = "guard-fixture-not-a-real-password"        # nosec: test fixture
BUNDLE_KEY = "guard-fixture-not-a-real-bundle-key"          # nosec: test fixture
COLLECT_KEY = "guard-fixture-not-a-real-collect-key"        # nosec: test fixture

GEARS_DEPLOYMENT = "qa-platform-gears"

# Which annotation covers which rendered object. Written out rather than
# derived, so that mounting a new ConfigMap without hashing it is a FAILURE
# here and not an invisible gap: `check_every_config_source_is_hashed` compares
# this table against what the rendered pod actually consumes, in both
# directions.
COVERED = {
    "qa-platform-gears-config": "checksum/gears-config",
    "qa-platform-gears-argo-qa-runs": "checksum/gears-argo-config",
    "qa-platform-gears-argo-qa-environments": "checksum/gears-argo-config",
    "qa-platform-postgres": "checksum/postgres-secret",
}

# `qa-platform-tls` is mounted by the gears pod and is deliberately NOT hashed.
# It is not a template: certs-job.yaml creates and refreshes that Secret inside
# the cluster, so there is nothing for `include` to render and its contents are
# not knowable at render time. Rotating it is that Job's business.
NOT_A_TEMPLATE = {"qa-platform-tls"}


def render(extra=(), release=RELEASE):
    """`helm template` with the minimum a default install needs."""
    cmd = [
        "helm", "template", release, str(CHART),
        "--namespace", "qa-platform",
        "--set", f"publicOrigin={ORIGIN}",
        "--set", f"keycloak.adminPassword={ADMIN_PASSWORD}",
        "--set", f"bundleDownloadSigningSecret={BUNDLE_KEY}",
        "--set", f"collectReportSigningSecret={COLLECT_KEY}",
        *extra,
    ]
    out = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if out.returncode != 0:
        return [], out.stderr, out.returncode
    return [d for d in yaml.safe_load_all(out.stdout) if d], out.stderr, 0


def gears_pod_template(docs):
    for doc in docs:
        if doc.get("kind") == "Deployment" and \
                doc.get("metadata", {}).get("name") == GEARS_DEPLOYMENT:
            return doc["spec"]["template"]
    return None


def annotations(docs):
    template = gears_pod_template(docs)
    return (template or {}).get("metadata", {}).get("annotations", {}) or {}


def consumed_config_sources(template):
    """Every ConfigMap and Secret the gears pod reads, by object name.

    Volumes AND environment, because a value can reach the process either way
    and a stale one is equally invisible either way."""
    names = set()
    spec = template["spec"]
    for volume in spec.get("volumes", []) or []:
        if "configMap" in volume:
            names.add(volume["configMap"]["name"])
        if "secret" in volume:
            names.add(volume["secret"]["secretName"])
    for container in (spec.get("containers", []) or []) + \
            (spec.get("initContainers", []) or []):
        for env in container.get("env", []) or []:
            source = env.get("valueFrom") or {}
            for key in ("configMapKeyRef", "secretKeyRef"):
                if key in source:
                    names.add(source[key]["name"])
        for env_from in container.get("envFrom", []) or []:
            for key, field in (("configMapRef", "name"), ("secretRef", "name")):
                if key in env_from:
                    names.add(env_from[key][field])
    return names


def check_every_config_source_is_hashed(failures):
    docs, stderr, rc = render()
    if rc != 0:
        failures.append(f"FAIL: helm template failed:\n{stderr}")
        return
    template = gears_pod_template(docs)
    if template is None:
        failures.append(f"FAIL: no Deployment named {GEARS_DEPLOYMENT} rendered")
        return

    consumed = consumed_config_sources(template)
    annos = annotations(docs)

    uncovered = consumed - set(COVERED) - NOT_A_TEMPLATE
    if uncovered:
        failures.append(
            f"FAIL: the gears pod consumes {sorted(uncovered)} and nothing "
            "hashes it into a pod annotation. A change to it would be written "
            "by `helm upgrade`, reported as a success, and never reach the "
            "running process -- which is the defect gears-deployment.yaml's "
            "checksum annotations exist to close. Add a checksum/ line for it "
            "there and an entry to COVERED here, or add it to NOT_A_TEMPLATE "
            "with the reason.")

    stale = set(COVERED) - consumed
    if stale:
        failures.append(
            f"FAIL: COVERED names {sorted(stale)}, which the gears pod no "
            "longer consumes. Drop the entry and its checksum/ line, so this "
            "table keeps meaning what it says.")

    for name, key in sorted(COVERED.items()):
        if key not in annos:
            failures.append(
                f"FAIL: the gears pod template carries no {key!r} annotation, "
                f"so a change to {name} does not roll the Deployment.")

    if not failures:
        print(
            "PASS: every chart-rendered config source the gears pod consumes "
            f"is hashed into a pod annotation ({', '.join(sorted(set(COVERED.values())))})")


def check_two_identical_renders_agree(failures):
    """No re-randomisation: a no-op upgrade must not roll the pod.

    This Deployment is `strategy: Recreate`, so a spurious roll is a real
    outage taken for no change.

    WHAT IT CAN AND CANNOT SEE. It renders with every value this guard knows
    about set, so it catches non-determinism in anything NOT fed by one of
    them -- a `randAlphaNum` introduced for a new value, a `uuidv4`, a
    timestamp. It does NOT catch a `randAlphaNum` fallback reinstated behind
    one of the two signing secrets, because these renders supply both:
    `check_the_signing_secrets_have_no_default` is what catches that, which is
    why it exists as a separate check rather than being folded in here."""
    first, stderr, rc = render()
    if rc != 0:
        failures.append(f"FAIL: helm template failed:\n{stderr}")
        return
    second, _, rc = render()
    if rc != 0:
        return

    a = {k: v for k, v in annotations(first).items() if k.startswith("checksum/")}
    b = {k: v for k, v in annotations(second).items() if k.startswith("checksum/")}
    differing = sorted(k for k in a if a[k] != b[k])
    if differing:
        failures.append(
            f"FAIL: {differing} differ between two renders of identical "
            "values. Something in the hashed templates is non-deterministic -- "
            "`randAlphaNum`, `uuidv4` and `genCA` all are. Every `helm "
            "upgrade` would then roll the gears pod, and this Deployment is "
            "`strategy: Recreate`, so that is an outage taken for no change.")
        return
    print(f"PASS: all {len(a)} checksum annotations are identical across two renders")


def check_a_changed_value_changes_the_hash(failures):
    """The property the annotation exists for, against the value that proved it.

    `bundleDownloadSigningSecret` is the one whose staleness was observed on
    the stand: the ConfigMap held the new key and the pod kept signing with the
    old one for 38 minutes, until it was restarted by hand."""
    before, stderr, rc = render()
    if rc != 0:
        failures.append(f"FAIL: helm template failed:\n{stderr}")
        return
    after, _, rc = render(
        extra=("--set", f"bundleDownloadSigningSecret={BUNDLE_KEY}-rotated"))
    if rc != 0:
        return

    key = "checksum/gears-config"
    old = annotations(before).get(key)
    new = annotations(after).get(key)
    if old is None or new is None:
        failures.append(f"FAIL: {key} is missing from one of the two renders")
        return
    if old == new:
        failures.append(
            f"FAIL: {key} is unchanged after bundleDownloadSigningSecret was "
            "changed. The annotation is not hashing the template that carries "
            "the value, so the gears pod would keep the old signing key after "
            "the upgrade -- every bundle it mints would then fail "
            "verification once something else restarted it.")
        return
    print(f"PASS: {key} changes when a value rendered into that ConfigMap changes")


def check_the_signing_secrets_have_no_default(failures):
    """Both `required`, and the message must name the value.

    Their old per-render `randAlphaNum 32` default is what
    `check_two_identical_renders_agree` would catch; this catches the same
    regression at its source, with a message a reader can act on."""
    for value in ("bundleDownloadSigningSecret", "collectReportSigningSecret"):
        cmd = [
            "helm", "template", RELEASE, str(CHART),
            "--namespace", "qa-platform",
            "--set", f"publicOrigin={ORIGIN}",
            "--set", f"keycloak.adminPassword={ADMIN_PASSWORD}",
        ]
        other = ("collectReportSigningSecret" if value.startswith("bundle")
                 else "bundleDownloadSigningSecret")
        other_key = COLLECT_KEY if other.startswith("collect") else BUNDLE_KEY
        cmd += ["--set", f"{other}={other_key}"]
        out = subprocess.run(cmd, capture_output=True, text=True, check=False)
        if out.returncode == 0:
            failures.append(
                f"FAIL: rendering with {value} unset succeeded. It is the only "
                "access control on an anonymously reachable route and has no "
                "safe default; a per-render random would rotate it on every "
                "upgrade and, with the checksum annotation in place, roll the "
                "pod every time too.")
            continue
        if value not in out.stderr:
            failures.append(
                f"FAIL: rendering with {value} unset failed (good), but the "
                f"message does not name {value}:\n{out.stderr}")
    if not failures:
        print(
            "PASS: neither bundleDownloadSigningSecret nor "
            "collectReportSigningSecret has a default, and each refusal names "
            "the value")


def main():
    failures = []
    check_every_config_source_is_hashed(failures)
    check_two_identical_renders_agree(failures)
    check_a_changed_value_changes_the_hash(failures)
    check_the_signing_secrets_have_no_default(failures)
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
