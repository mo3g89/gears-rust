"""The `qa-platform-tls` Secret name must agree between the chart that
mints it and the ops script that reads it back on a live cluster.

# Why this is a guard and not a comment

`certs-job.yaml`'s pre-install/pre-upgrade hook builds a Secret manifest
naming it literally `qa-platform-tls` (this chart does not run the name
through the release's fullname helper -- see that template's own
comments) and applies it with `kubectl apply --server-side`. Every
in-chart consumer (`gears-deployment.yaml`, `ui-deployment.yaml`, and
`certs-job.yaml`'s own `tls-existing` volume) mounts that same literal, so
a rename there that missed one of those three would fail LOUDLY: `helm
install`/`helm upgrade` would leave a pod stuck on a missing Secret volume,
impossible to miss.

`deploy/remote/verify-k8s.sh` is the case that would NOT fail loudly. It
is a standalone bash script, run by hand against a live cluster, entirely
outside Helm's render -- nothing connects its `kubectl get secret
qa-platform-tls` calls to the chart's own literal. If a future edit
renamed the Secret in `certs-job.yaml` (for instance, to fix it so two
releases in one namespace no longer collide on the same Secret name) and
missed this file, `verify-k8s.sh` would silently start reading a Secret
that no longer exists, or an unrelated one -- a stand-verification script
that reports FAIL for the wrong reason, or a false PASS if some other
Secret happened to share the name. Nothing but this guard connects the two.

# What this checks

Extracts the literal Secret name each file's real code depends on -- not
prose, which could drift without breaking anything -- and asserts they
agree:

1. `certs-job.yaml`'s heredoc line that writes the rendered Secret
   manifest's `name:` field.
2. Every `kubectl ... get secret <name> -o jsonpath=...` invocation in
   `verify-k8s.sh` (there are two, one per certificate leaf it reads back);
   asserts they agree with each other too, so this guard also catches the
   two `kubectl` calls in that script drifting from one another.

The failure message names both file paths so whoever breaks this knows
where to look, rather than having to rediscover both sides from a bare
string mismatch.

Standalone script, like its siblings in this directory -- see the
Makefile's `helm-tests` target for why each one is invoked directly rather
than through pytest collection."""
import pathlib
import re
import subprocess
import sys

import yaml

TESTS_DIR = pathlib.Path(__file__).resolve().parent
CHART = TESTS_DIR.parent / "qa-platform"
CERTS_JOB = CHART / "templates" / "certs-job.yaml"
VERIFY_K8S = TESTS_DIR.parent.parent / "remote" / "verify-k8s.sh"

# Matches certs-job.yaml's heredoc line building the Secret manifest:
#     '  name: qa-platform-tls' \
CERTS_JOB_NAME_RE = re.compile(r"'\s*name:\s*(\S+)'")

# Matches verify-k8s.sh's real `kubectl ... get secret <name> -o jsonpath=...`
# invocations -- not the FAIL-message prose that also mentions the name,
# which could drift on its own without breaking anything this script does.
VERIFY_K8S_GET_SECRET_RE = re.compile(
    r'kubectl\s+-n\s+"\$NAMESPACE"\s+get secret\s+(\S+)\s+-o\s+jsonpath=')


def extract_certs_job_secret_name(failures):
    """The Secret name certs-job.yaml's initContainer writes into the
    manifest it applies. Returns None (and records a failure) if the file
    is missing or the expected line is gone."""
    if not CERTS_JOB.is_file():
        failures.append(f"FAIL: {CERTS_JOB} does not exist.")
        return None
    text = CERTS_JOB.read_text()
    matches = CERTS_JOB_NAME_RE.findall(text)
    if len(matches) != 1:
        failures.append(
            f"FAIL: expected exactly one `'  name: <secret>'` line in "
            f"{CERTS_JOB}, found {len(matches)}. This guard's regex may no "
            "longer match the heredoc's shape.")
        return None
    return matches[0]


def extract_verify_k8s_secret_names(failures):
    """The Secret name(s) verify-k8s.sh's `kubectl get secret` calls
    actually use. Returns None (and records a failure) if the file is
    missing, the calls are gone, or the calls disagree with each other."""
    if not VERIFY_K8S.is_file():
        failures.append(f"FAIL: {VERIFY_K8S} does not exist.")
        return None
    text = VERIFY_K8S.read_text()
    matches = VERIFY_K8S_GET_SECRET_RE.findall(text)
    if not matches:
        failures.append(
            f"FAIL: found no `kubectl ... get secret <name> -o "
            f"jsonpath=...` invocations in {VERIFY_K8S}. This guard's "
            "regex may no longer match this script's shape.")
        return None
    unique = set(matches)
    if len(unique) != 1:
        failures.append(
            f"FAIL: {VERIFY_K8S}'s own `kubectl get secret` calls disagree "
            f"with each other: found {sorted(unique)!r} across "
            f"{len(matches)} invocations.")
        return None
    return matches[0]


def check_tls_secret_name_agrees(failures):
    """The property this guard exists to prove: the literal Secret name
    certs-job.yaml mints is the same one verify-k8s.sh reads back on a
    live cluster."""
    certs_job_name = extract_certs_job_secret_name(failures)
    verify_k8s_name = extract_verify_k8s_secret_names(failures)
    if certs_job_name is None or verify_k8s_name is None:
        return

    if certs_job_name != verify_k8s_name:
        failures.append(
            f"FAIL: the TLS Secret name disagrees between {CERTS_JOB} "
            f"(mints it as {certs_job_name!r}) and {VERIFY_K8S} (reads it "
            f"back as {verify_k8s_name!r}). These two files are not "
            "otherwise connected -- verify-k8s.sh is a standalone script "
            "run by hand against a live cluster, entirely outside Helm's "
            "render -- so nothing but this guard would catch the two "
            "drifting apart. Fix whichever one is stale so both name the "
            "same Secret.")


def certs_role_rules(failures):
    out = subprocess.run(
        ["helm", "template", "qa-platform", str(CHART),
         "--api-versions", "admissionregistration.k8s.io/v1/ValidatingAdmissionPolicy",
         "--set", "publicOrigin=https://example-tls-guard-test.invalid",
         "--set", "keycloak.adminPassword=guard-fixture-not-a-real-password",
         "--set", "argo.workflowClientSecret=guard-fixture-not-a-real-workflow-secret",  # nosec: test fixture only
         "--set", "postgres.password=guard-fixture-not-a-real-db-password",  # nosec: test fixture only
         "--set", "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key",
         "--set", "collectReportSigningSecret=guard-fixture-not-a-real-collect-key"],
        capture_output=True, text=True)
    if out.returncode != 0:
        failures.append(f"FAIL: helm template failed: {out.stderr.strip()}")
        return None
    for d in yaml.safe_load_all(out.stdout):
        if d and d.get("kind") == "Role" and \
                d["metadata"]["name"] == "qa-platform-certs":
            return d["rules"]
    failures.append("FAIL: no Role qa-platform-certs rendered.")
    return None


def check_certs_role_is_named(failures):
    """The certs hook Role survives the hook (before-hook-creation deletes it
    only on the next run), so it may patch ONLY the Secret the job writes.
    `create` cannot be narrowed (the API ignores resourceNames on create)
    and is the only verb allowed without resourceNames."""
    name = extract_certs_job_secret_name(failures)
    rules = certs_role_rules(failures)
    if name is None or rules is None:
        return
    for r in rules:
        if "secrets" not in r.get("resources", []):
            continue
        verbs = set(r["verbs"])
        names = r.get("resourceNames")
        if not names and verbs - {"create"}:
            failures.append(
                f"FAIL: Role qa-platform-certs grants {sorted(verbs)} on "
                "ALL secrets; only `create` may be unnamed.")
        if names and names != [name]:
            failures.append(
                f"FAIL: Role qa-platform-certs names {names!r}; the job "
                f"writes {name!r}.")
    if not any(r.get("resourceNames") == [name] and "patch" in r["verbs"]
               for r in rules):
        failures.append(
            f"FAIL: Role qa-platform-certs has no `patch` rule with "
            f"resourceNames == [{name!r}].")


def main():
    failures = []
    check_tls_secret_name_agrees(failures)
    check_certs_role_is_named(failures)
    if failures:
        for line in failures:
            print(line, file=sys.stderr)
        return 1
    print(
        f"PASS: the TLS Secret name agrees between {CERTS_JOB} and "
        f"{VERIFY_K8S}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
