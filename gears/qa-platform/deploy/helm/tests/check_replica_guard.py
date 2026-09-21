"""`gears.replicaCount` above 1 must be refused, permanently.

# Why this is a guard and not a documentation note

`gears-deployment.yaml` mounts `qa-platform-gears-data`, a ReadWriteOnce PVC
(`gears-pvc.yaml`), and qa-runs' dispatcher/scheduler/schedule
referential-check tickers are gated by `infra/leader`'s shipped
implementation, `NoopLeaderElector`, under which EVERY replica believes it
is the leader. A second `gears` replica would therefore fail to mount the
RWO volume on whichever node it lands on -- and, even if it somehow ran,
would break the dispatcher's boot-recovery premise that only one process
can be mid-submit. (The scheduler itself does not need the gate: a unique
index on `qa_schedule_ticks` makes exactly-once firing hold with every
replica evaluating every schedule, which is what
`cpt-cf-qa-nfr-scheduler-exactly-once` requires -- PRD §6,
"Non-Functional Requirements".) DESIGN §3.4
(qa-runs -- Leader election) records the single-replica assumption
directly; §3.11 records what a second replica changes for the (separate)
live-log fan-out path.

This is a permanent property of the current architecture, not a bug
pending a fix, so the chart refuses to render `gears.replicaCount` above 1
at all rather than merely warning about it in a comment nobody reads
before running `--set gears.replicaCount=3`.

# What this checks

1. Rendering with `gears.replicaCount=2` fails the `helm template` call
   outright (a non-zero exit), and the failure text cites `DESIGN §3.4` so
   a reader hitting the error can find the reasoning without grepping the
   chart.
2. Rendering with the chart's default value still succeeds and produces
   exactly `replicas: 1` on the `qa-platform-gears` Deployment -- the guard
   must not be a false trip on the common path.

Standalone script, like its siblings in this directory -- see the
Makefile's `helm-tests` target for why each one is invoked directly rather
than through pytest collection."""
import pathlib
import subprocess
import sys

import yaml

TESTS_DIR = pathlib.Path(__file__).resolve().parent
CHART = TESTS_DIR.parent / "qa-platform"
ORIGIN = "https://example-replica-guard-test.invalid"
RELEASE = "replica-guard-test"


def render(extra_set=None):
    """Return (docs, stderr, returncode). Never asserts -- callers
    decide."""
    cmd = ["helm", "template", RELEASE, str(CHART),
           "--namespace", "qa-platform", "--set", f"publicOrigin={ORIGIN}",
           # keycloak.adminPassword has no default (WS3 Task 3) -- any value
           # that is not the literal "admin" satisfies the render.
           "--set", "keycloak.adminPassword=guard-fixture-not-a-real-password",
           # Both signing secrets have no default either (2026-09-21): the
           # per-render `randAlphaNum` fallback became a pod roll on every
           # upgrade once the gears Deployment started hashing the ConfigMap.
           "--set", "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key",
           "--set", "collectReportSigningSecret=guard-fixture-not-a-real-collect-key"]
    if extra_set:
        cmd += ["--set", extra_set]
    out = subprocess.run(cmd, capture_output=True, text=True)
    if out.returncode != 0:
        return [], out.stderr, out.returncode
    return [d for d in yaml.safe_load_all(out.stdout) if d], out.stderr, 0


def check_replica_count_above_one_is_refused(failures):
    """`gears.replicaCount=2` must fail the render, and the failure
    message must cite DESIGN §3.4 -- the guard exists so a reader who
    hits this error in CI or on a stand can find the reasoning without
    having to already know it."""
    _, stderr, rc = render("gears.replicaCount=2")
    if rc == 0:
        failures.append(
            "FAIL: rendering with gears.replicaCount=2 succeeded. This "
            "must be refused: qa-runs' NoopLeaderElector and the gears "
            "PVC's ReadWriteOnce access mode make a second replica unsafe "
            "by design, not pending a fix.")
        return
    if "DESIGN §3.4" not in stderr:
        failures.append(
            "FAIL: rendering with gears.replicaCount=2 failed (good), but "
            "the failure message does not cite DESIGN §3.4, so a reader "
            f"cannot find the reasoning. Got:\n{stderr}")


def check_default_still_renders_one_replica(failures):
    """The guard must not be a false trip on the ordinary path: the
    chart's default value renders successfully and produces exactly
    `replicas: 1` on the gears Deployment."""
    docs, stderr, rc = render()
    if rc != 0:
        failures.append(
            f"FAIL: a default render must succeed:\n{stderr}")
        return

    gears_deployments = [
        d for d in docs
        if d.get("kind") == "Deployment"
        and d.get("metadata", {}).get("name") == "qa-platform-gears"
    ]
    if len(gears_deployments) != 1:
        failures.append(
            "FAIL: expected exactly one Deployment named "
            f"qa-platform-gears, found {len(gears_deployments)}.")
        return

    replicas = gears_deployments[0].get("spec", {}).get("replicas")
    if replicas != 1:
        failures.append(
            f"FAIL: default render's qa-platform-gears Deployment has "
            f"replicas={replicas!r}, expected 1.")


def main():
    failures = []
    check_replica_count_above_one_is_refused(failures)
    check_default_still_renders_one_replica(failures)
    if failures:
        for line in failures:
            print(line, file=sys.stderr)
        return 1
    print(
        "PASS: gears.replicaCount above 1 is refused with a message "
        "citing DESIGN §3.4, and the default still renders replicas: 1")
    return 0


if __name__ == "__main__":
    sys.exit(main())
