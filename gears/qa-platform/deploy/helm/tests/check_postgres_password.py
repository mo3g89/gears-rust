"""postgres.password has no default, and its committed fixture `qa` is
refused outside devMode -- the pair keycloak.adminPassword and
argo.workflowClientSecret already have.

# The trap this guard exists to catch

values.yaml shipped `postgres.password: qa`, a literal published in this
repository, with no refusal: every install that did not override it ran its
database as `qa`/`qa`. That is the pre-fix shape of finding #117 (the Keycloak
admin password), one Secret over. The value is `required` with an empty default
now, and `qa` renders only with `devMode=true`.

# What this checks

1. Unset, rendered WITH devMode=true, fails on `required` and names it. devMode
   is what makes this check honest: under devMode `qa` is an acceptable value,
   so a values.yaml that put `qa` back as the default would RENDER and this
   check would report it. (check_realm_secrecy.py's
   check_workflow_secret_unset_fails explains the same trick.)
2. An explicit empty string fails the same way.
3. `qa` without devMode fails, naming postgres.password and devMode.
4. `qa` with devMode=true renders, and reaches Secret qa-platform-postgres.
5. A generated value renders without devMode and reaches that Secret verbatim.
6. A numeric-looking value renders as the same string (the comparison takes
   `toString`, or `--set postgres.password=123456789012` errors the render).

Standalone script, like its siblings -- see the Makefile's `helm-tests`."""
import pathlib
import subprocess
import sys

import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
BASE = [
    "--set", "publicOrigin=https://example-postgres-password.invalid",
    "--set", "keycloak.adminPassword=guard-fixture-not-a-real-password",  # nosec: test fixture only
    "--set", "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key",  # nosec: test fixture only
    "--set", "collectReportSigningSecret=guard-fixture-not-a-real-collect-key",  # nosec: test fixture only
    "--set", "argo.workflowClientSecret=guard-fixture-not-a-real-workflow-secret",  # nosec: test fixture only
]
GENERATED = "guard-fixture-not-a-real-db-password"  # nosec: test fixture only


def render(*extra):
    cmd = ["helm", "template", "pg-password", str(CHART),
           "--namespace", "qa-platform", *BASE, *extra]
    return subprocess.run(cmd, capture_output=True, text=True, check=False)


def postgres_password(stdout):
    for doc in yaml.safe_load_all(stdout):
        if doc and doc.get("kind") == "Secret" and \
                doc.get("metadata", {}).get("name") == "qa-platform-postgres":
            return (doc.get("stringData") or {}).get("POSTGRES_PASSWORD")
    return None


def main():
    failures = []
    for label, extra in (("unset", ()), ("empty", ("--set", "postgres.password="))):
        r = render(*extra, "--set", "devMode=true")
        if r.returncode == 0:
            failures.append(
                f"FAIL: postgres.password {label} rendered (devMode=true). It has "
                "no default any more; values.yaml must carry an empty string and "
                "postgres-secret.yaml's `required` must refuse the render.")
        elif "postgres.password is required" not in r.stderr:
            failures.append(
                f"FAIL: postgres.password {label} failed (good) but not on its "
                f"`required`:\n{r.stderr}")

    r = render("--set", "postgres.password=qa")
    if r.returncode == 0:
        failures.append(
            "FAIL: postgres.password=qa rendered without devMode. `qa` is "
            "committed in this repository and must be refused outside devMode.")
    elif "devMode" not in r.stderr or "postgres.password" not in r.stderr:
        failures.append(
            "FAIL: postgres.password=qa was refused (good) but the message does "
            f"not name postgres.password and devMode:\n{r.stderr}")

    r = render("--set", "postgres.password=qa", "--set", "devMode=true")
    if r.returncode != 0 or postgres_password(r.stdout) != "qa":
        failures.append(
            "FAIL: postgres.password=qa with devMode=true must render into "
            f"Secret qa-platform-postgres (the escape hatch):\n{r.stderr}")

    for value in (GENERATED, "123456789012"):
        r = render("--set", f"postgres.password={value}")
        got = postgres_password(r.stdout) if r.returncode == 0 else None
        if got != value:
            failures.append(
                f"FAIL: postgres.password={value} without devMode must render "
                f"verbatim into qa-platform-postgres; got {got!r}:\n{r.stderr}")

    if failures:
        for line in failures:
            print(line, file=sys.stderr)
        return 1
    print("PASS: postgres.password has no default, refuses `qa` outside devMode, "
          "permits it in devMode, and renders any other value verbatim")
    return 0


if __name__ == "__main__":
    sys.exit(main())
