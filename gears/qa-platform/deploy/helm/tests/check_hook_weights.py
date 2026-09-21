"""The tenant-seed hook must run strictly after the db-migrate hook.

# Why this is a guard and not a comment

`job-tenant-seed.yaml` seeds a Resource Group row the gears need to boot,
and `seed-tenant.sh` (its entrypoint) assumes the schema db-migrate creates
already exists -- its own step 0 checks for that explicitly rather than
dying on a bare "relation does not exist", but the check that actually
*prevents* seed-tenant.sh from running too early is Helm's own hook
ordering: both Jobs are `post-install,post-upgrade` hooks, and Helm runs
hooks of one type in ascending `helm.sh/hook-weight` order, waiting for
each to complete before starting the next. `job-db-migrate.yaml` carries
weight `"0"`, `job-tenant-seed.yaml` carries weight `"10"` -- that gap is
the entire ordering guarantee. Until now the only place that property was
recorded was prose in both templates' comments: nothing failed if a future
edit closed the gap, reversed it, or set the two Jobs to the SAME weight
(under which Helm's own docs do not promise an order at all).

# What this checks

Renders the chart and reads the `helm.sh/hook-weight` annotation back off
the two rendered Job objects -- not the template text, so a future edit
that changes one Job's weight without touching the other's comment (or
moves the annotation into a `{{ }}`-templated value) still gets caught
here. Asserts the tenant-seed Job's weight is strictly greater than the
db-migrate Job's, and that both parse as integers (Helm hook-weight
annotations are always strings; a non-numeric value would make `helm`
itself refuse to order the hooks, but this guard should say why rather
than let that surface as a cryptic conversion error).

Standalone script, like its siblings in this directory -- see the
Makefile's `helm-tests` target for why each one is invoked directly rather
than through pytest collection."""
import pathlib
import subprocess
import sys

import yaml

TESTS_DIR = pathlib.Path(__file__).resolve().parent
CHART = TESTS_DIR.parent / "qa-platform"
ORIGIN = "https://example-hook-weights-test.invalid"
RELEASE = "hook-weights-test"

DB_MIGRATE_NAME = "qa-platform-db-migrate"
TENANT_SEED_NAME = "qa-platform-tenant-seed"


def render():
    """Return (docs, stderr, returncode). Never asserts -- callers
    decide."""
    out = subprocess.run(
        ["helm", "template", RELEASE, str(CHART),
         "--namespace", "qa-platform", "--set", f"publicOrigin={ORIGIN}",
         # keycloak.adminPassword has no default (WS3 Task 3) -- any value
         # that is not the literal "admin" satisfies the render.
         "--set", "keycloak.adminPassword=guard-fixture-not-a-real-password",
         # Both signing secrets have no default either (2026-09-21): the
         # per-render `randAlphaNum` fallback became a pod roll on every
         # upgrade once the gears Deployment started hashing the ConfigMap.
         "--set", "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key",
         "--set", "collectReportSigningSecret=guard-fixture-not-a-real-collect-key"],
        capture_output=True, text=True)
    if out.returncode != 0:
        return [], out.stderr, out.returncode
    return [d for d in yaml.safe_load_all(out.stdout) if d], out.stderr, 0


def job_named(docs, name):
    """The single rendered Job object with this name, or None."""
    jobs = [
        d for d in docs
        if d.get("kind") == "Job" and d.get("metadata", {}).get("name") == name
    ]
    if len(jobs) != 1:
        return None
    return jobs[0]


def hook_weight_of(job, failures, label):
    """The job's `helm.sh/hook-weight` annotation, parsed as an int.
    Appends to `failures` and returns None on anything that stops this
    guard from proving the ordering property."""
    annotations = job.get("metadata", {}).get("annotations", {}) or {}
    raw = annotations.get("helm.sh/hook-weight")
    if raw is None:
        failures.append(
            f"FAIL: {label} carries no helm.sh/hook-weight annotation in "
            "the rendered output.")
        return None
    try:
        return int(raw)
    except (TypeError, ValueError):
        failures.append(
            f"FAIL: {label}'s helm.sh/hook-weight is {raw!r}, which does "
            "not parse as an integer. Helm hook-weight annotations are "
            "always strings, but the value itself must be numeric for "
            "Helm to order hooks by it at all.")
        return None


def check_tenant_seed_runs_after_db_migrate(failures):
    """The property this guard exists to prove: tenant-seed's rendered
    hook-weight is strictly greater than db-migrate's, read back off the
    rendered objects rather than assumed from the template text."""
    docs, stderr, rc = render()
    if rc != 0:
        failures.append(f"FAIL: a default render must succeed:\n{stderr}")
        return

    db_migrate = job_named(docs, DB_MIGRATE_NAME)
    if db_migrate is None:
        failures.append(
            f"FAIL: expected exactly one Job named {DB_MIGRATE_NAME!r} in "
            "the rendered output.")
        return

    tenant_seed = job_named(docs, TENANT_SEED_NAME)
    if tenant_seed is None:
        failures.append(
            f"FAIL: expected exactly one Job named {TENANT_SEED_NAME!r} in "
            "the rendered output.")
        return

    db_migrate_weight = hook_weight_of(
        db_migrate, failures, f"Job {DB_MIGRATE_NAME}")
    tenant_seed_weight = hook_weight_of(
        tenant_seed, failures, f"Job {TENANT_SEED_NAME}")
    if db_migrate_weight is None or tenant_seed_weight is None:
        return

    if not (tenant_seed_weight > db_migrate_weight):
        failures.append(
            f"FAIL: {TENANT_SEED_NAME}'s hook-weight ({tenant_seed_weight}) "
            f"is not strictly greater than {DB_MIGRATE_NAME}'s "
            f"({db_migrate_weight}). Helm runs post-install/post-upgrade "
            "hooks in ascending weight order and waits for each to "
            "complete before starting the next -- without a strict gap, "
            "tenant-seed is no longer guaranteed to run after migrations "
            "have created the schema seed-tenant.sh assumes exists.")


def main():
    failures = []
    check_tenant_seed_runs_after_db_migrate(failures)
    if failures:
        for line in failures:
            print(line, file=sys.stderr)
        return 1
    print(
        "PASS: qa-platform-tenant-seed's rendered hook-weight is strictly "
        "greater than qa-platform-db-migrate's, so tenant-seed always runs "
        "after migrations")
    return 0


if __name__ == "__main__":
    sys.exit(main())
