#!/usr/bin/env python3
"""CI's helm lint and kubeconform steps render the chart with the SAME --set
values they carry in ci.yml. When a value becomes `required`, a bare `helm lint`
still passes (lint tolerates `required`) while `helm template | kubeconform`
fails -- that shipped once (2026-09-29, argo.workflowClientSecret). This guard
replays each CI invocation's --set list through `helm template` locally.

It replays `--api-versions` too, and requires the kubeconform render (the
`helm template` invocation) to contain the ValidatingAdmissionPolicy and its
binding (finding #94). workflow-guard-admission-policy.yaml is MANDATORY: the
chart `fail`s unless the API is advertised, and a CI-side `helm template` has
no cluster to discover it from -- so without `--api-versions` the render fails
outright, and a render that omitted the policy must never be accepted."""
import pathlib
import re
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parents[5]
CI = REPO / ".github" / "workflows" / "ci.yml"
CHART = "gears/qa-platform/deploy/helm/qa-platform"


def invocations(text):
    # A `run: |` block's continuation lines joined back into one command.
    joined = re.sub(r"\\\n\s*", " ", text)
    for line in joined.splitlines():
        if re.search(r"\bhelm (template|lint)\b", line) and CHART in line:
            yield (re.search(r"\bhelm (template|lint)\b", line).group(1),
                   re.findall(r"--set(?:=|\s+)(\S+)", line),
                   re.findall(r"--api-versions(?:=|\s+)(\S+)", line))


# Kinds the kubeconform render must carry. The Workflow guard is mandatory
# (the chart refuses to render without the API), so a render that omits them
# was not given the API version and kubeconform validates less than it should.
MANDATORY_POLICY_KINDS = ("ValidatingAdmissionPolicy", "ValidatingAdmissionPolicyBinding")


def main():
    found = list(invocations(CI.read_text()))
    if len(found) < 2:
        print(f"FAIL: expected helm lint and kubeconform invocations of {CHART} in ci.yml, found {len(found)}")
        return 1
    failed = 0
    templates = [f for f in found if f[0] == "template"]
    if not templates:
        print(f"FAIL: no `helm template` invocation of {CHART} in ci.yml -- kubeconform validates nothing")
        return 1
    for verb, sets, api_versions in found:
        if not sets:
            # A bare `helm template | kubeconform` would fail to render, so a
            # zero-`--set` capture means the regex missed the flags (or CI lost
            # them) -- replaying nothing would pass on nothing.
            failed += 1
            print("FAIL: a captured CI helm invocation of the chart carries zero --set values; "
                  "the guard cannot replay it (did the flag spelling change?)")
            continue
        cmd = ["helm", "template", "qa-platform", str(REPO / CHART), "--namespace", "qa-platform"]
        for s in sets:
            cmd += ["--set", s]
        for a in api_versions:
            cmd += ["--api-versions", a]
        if verb == "lint":
            # `helm lint` has no --api-versions and tolerates the chart's
            # mandatory-API `fail` (it logs it and still exits 0), so CI's lint
            # carries no flag. The replay is a `template`, which does not
            # tolerate it, and is here only to check the --set list renders.
            cmd += ["--api-versions", "admissionregistration.k8s.io/v1/ValidatingAdmissionPolicy"]
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode != 0:
            failed += 1
            print(f"FAIL: CI's --set list {sets} does not render:\n{r.stderr.strip()}")
            continue
        if verb == "template":
            kinds = set(re.findall(r"^kind:\s*(\S+)", r.stdout, re.M))
            for kind in MANDATORY_POLICY_KINDS:
                if kind not in kinds:
                    failed += 1
                    print(f"FAIL: CI's kubeconform render has no {kind}: pass "
                          "--api-versions admissionregistration.k8s.io/v1/ValidatingAdmissionPolicy "
                          "to its `helm template`, or kubeconform never schema-checks the policy")
    if failed:
        return 1
    print(f"OK: {len(found)} CI invocations render")
    return 0


if __name__ == "__main__":
    sys.exit(main())
