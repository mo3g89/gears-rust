"""The runner pod's ServiceAccount is a named, chart-provisioned identity
carrying exactly the permissions it needs to report a run's result -- not
the namespace's `default` account, not an account this chart merely assumes
some OTHER Helm release already created, and (a correction from this
guard's first cut) not an account with NO permissions either.

# What changed, and what this guards

Task 3 of the 2026-09-17 execution-plane workstream stopped
`ArgoExecutorConfig::workflow_service_account` from defaulting to `None`
(qa-runs' own executor now refuses to construct without one -- see
`infra/executor/argo/mod.rs`'s `check_service_account`), and it moved
`values.yaml`'s `argo.workflowServiceAccount` off the old assumption that a
separate `argo-workflows` Helm release had already created an account called
`argo-workflow` with its own RBAC. This chart now provisions the account
itself (`argo-runner-serviceaccount.yaml`).

# Fix round 1: "no permissions at all" broke every run

The first cut of this template shipped that account with NO Role at all, and
with `automountServiceAccountToken: false` on the ServiceAccount object. Both
looked like hardening; combined with Task 1's pod-level
`automountServiceAccountToken: false` (`argo/workflow.rs`), they meant Argo's
own executor (the workflow pod's `wait` container, not `main`) had no token
to authenticate with AND would have been forbidden even with one --
`workflowtaskresults.argoproj.io` needs `create` (measured on the dev
cluster, 2026-08-27 -- see `ArgoExecutorConfig::workflow_service_account`'s
doc -- as `exit code 64` after the run had already produced its output). So
this guard now checks for a MINIMAL grant, not an absent one: the point was
always a declared, reviewed identity instead of whatever `default` happens to
be, not an identity that cannot do the one thing it must.

Four properties, each a way this could quietly regress:
  1. A ServiceAccount named `argo.workflowServiceAccount`, rendered in
     `argo.namespace` -- the namespace `ArgoExecutorConfig.namespace` submits
     Workflows into, NOT the release namespace the gears pod itself runs in.
     Landing it in the wrong namespace is silent: Argo would fall back to
     that namespace's own `default` account with no error at all, which is
     the exact failure this task exists to close.
  2. That ServiceAccount does NOT set `automountServiceAccountToken: false`
     on the object itself -- `wait` needs the token to authenticate at all.
     (The pod-level flag in `argo/workflow.rs` is a SEPARATE, still-open
     defect this guard cannot see from the chart -- it is tracked in the
     fix-round section of task-3-report.md, not re-litigated here.)
  3. A Role granting exactly `create` and `patch` on
     `workflowtaskresults.argoproj.io`, in `argo.namespace` -- read off a
     live cluster's own argo-workflows-installed executor Role for the
     identical resource, not assumed.
  4. A RoleBinding, in the same namespace, binding that Role to that
     ServiceAccount. A Role nobody is bound to is the same silent failure in
     a new shape -- the account would still run forbidden.

A fifth check ties the account to qa-runs' own config: `gears-argo-configmaps.
yaml`'s rendered `workflow_service_account` must name the SAME account this
chart provisions and grants, not drift into naming one that does not exist or
is not the one the Role binds.

Standalone script, like its siblings in this directory -- see the Makefile's
`helm-tests` target for why each one is invoked directly rather than through
pytest collection (`python3 -m pytest tests/` collects zero items from a
module with no `test_*` function)."""
import pathlib
import subprocess
import sys

import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
ORIGIN = "https://example-runner-sa-test.invalid"

# The exact grant a live cluster's own argo-workflows chart installs for its
# executor identity against this same resource (`kubectl get role
# argo-workflows-workflow -n argo -o yaml`, checked during fix round 1) --
# the evidence this guard pins against, not a number picked defensively.
REQUIRED_API_GROUP = "argoproj.io"
REQUIRED_RESOURCE = "workflowtaskresults"
REQUIRED_VERBS = {"create", "patch"}


def render(*extra):
    """Return (docs, stderr, returncode). Never asserts -- callers decide."""
    out = subprocess.run(
        ["helm", "template", "qa-platform", str(CHART),
         "--namespace", "qa-platform", "--set", f"publicOrigin={ORIGIN}",
         # keycloak.adminPassword has no default (WS3 Task 3) -- any value
         # that is not the literal "admin" satisfies the render.
         "--set", "keycloak.adminPassword=guard-fixture-not-a-real-password",
         # Both signing secrets have no default either (2026-09-21): the
         # per-render `randAlphaNum` fallback became a pod roll on every
         # upgrade once the gears Deployment started hashing the ConfigMap.
         "--set", "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key",
         "--set", "collectReportSigningSecret=guard-fixture-not-a-real-collect-key",
         *extra],
        capture_output=True, text=True)
    if out.returncode != 0:
        return [], out.stderr, out.returncode
    return [d for d in yaml.safe_load_all(out.stdout) if d], out.stderr, 0


def chart_values():
    """The committed defaults, so this test asserts against what
    `argo.workflowServiceAccount` / `argo.namespace` actually ARE rather than
    a name hardcoded a second time and free to drift from values.yaml."""
    return yaml.safe_load((CHART / "values.yaml").read_text())


def check_serviceaccount(failures):
    docs, stderr, rc = render()
    if rc != 0:
        failures.append(f"FAIL: a default render must succeed:\n{stderr}")
        return

    vals = chart_values()
    expected_name = vals["argo"]["workflowServiceAccount"]
    expected_namespace = vals["argo"]["namespace"]

    sas = [d for d in docs if d["kind"] == "ServiceAccount"
           and d["metadata"]["name"] == expected_name]
    if not sas:
        failures.append(
            f"FAIL: no ServiceAccount named {expected_name!r} was rendered. "
            "argo.workflowServiceAccount names an account this chart must "
            "provision itself, not one it assumes some other release created.")
        return
    sa = sas[0]

    rendered_namespace = sa["metadata"].get("namespace")
    if rendered_namespace != expected_namespace:
        failures.append(
            f"FAIL: ServiceAccount {expected_name} renders in namespace "
            f"{rendered_namespace!r}, expected {expected_namespace!r} "
            "(argo.namespace) -- the namespace the Workflow, and so its pod's "
            "ServiceAccount, actually runs in. In the wrong namespace this "
            "account is simply never the one the Workflow names.")

    if sa.get("automountServiceAccountToken") is False:
        failures.append(
            f"FAIL: ServiceAccount {expected_name} sets "
            "automountServiceAccountToken: false on the object itself. Argo's "
            "own executor (the workflow pod's `wait` container) authenticates "
            "to the API server AS THIS ACCOUNT to write workflowtaskresults -- "
            "with no token mounted it cannot, regardless of what the Role "
            "grants. (Fix round 1: this was set here and broke every run.)")

    if not failures:
        print(
            f"PASS: {expected_name} renders in {expected_namespace!r} without "
            "automountServiceAccountToken: false on the object")


def check_role_and_binding(failures):
    docs, _, rc = render()
    if rc != 0:
        return  # already reported by check_serviceaccount

    vals = chart_values()
    expected_name = vals["argo"]["workflowServiceAccount"]
    expected_namespace = vals["argo"]["namespace"]

    roles = [d for d in docs if d["kind"] == "Role"
             and d["metadata"].get("namespace") == expected_namespace
             and any(REQUIRED_RESOURCE in rule.get("resources", [])
                     for rule in d.get("rules", []))]
    if not roles:
        failures.append(
            f"FAIL: no Role in {expected_namespace!r} grants anything on "
            f"{REQUIRED_RESOURCE}. Argo's executor cannot write a "
            "workflowtaskresults object without one, and every run then fails "
            "at the end with exit code 64 after producing all of its output.")
        return
    role = roles[0]

    granted = set()
    for rule in role.get("rules", []):
        if REQUIRED_RESOURCE in rule.get("resources", []) and REQUIRED_API_GROUP in rule.get("apiGroups", []):
            granted |= set(rule.get("verbs", []))
    missing = REQUIRED_VERBS - granted
    if missing:
        failures.append(
            f"FAIL: Role {role['metadata']['name']} grants {sorted(granted)} on "
            f"{REQUIRED_API_GROUP}/{REQUIRED_RESOURCE}, missing {sorted(missing)}. "
            "Both verbs are what a live cluster's own argo-workflows-installed "
            "executor Role carries for this identical resource -- see this "
            "file's module docstring.")

    bindings = [d for d in docs
                if d["kind"] == "RoleBinding"
                and d["metadata"].get("namespace") == expected_namespace
                and d.get("roleRef", {}).get("name") == role["metadata"]["name"]]
    bound_to_account = [
        b for b in bindings
        if any(s.get("kind") == "ServiceAccount" and s.get("name") == expected_name
               for s in b.get("subjects", []))
    ]
    if not bound_to_account:
        failures.append(
            f"FAIL: Role {role['metadata']['name']} exists but no RoleBinding "
            f"in {expected_namespace!r} subjects ServiceAccount {expected_name!r} "
            "to it. A Role nobody is bound to is the same silent failure this "
            "whole guard exists to catch, in a new shape.")

    if not missing and bound_to_account:
        print(
            f"PASS: Role {role['metadata']['name']} grants {sorted(granted)} on "
            f"{REQUIRED_API_GROUP}/{REQUIRED_RESOURCE} and is bound to "
            f"{expected_name!r} via {bound_to_account[0]['metadata']['name']}")


def check_config_names_the_same_account(failures):
    docs, _, rc = render()
    if rc != 0:
        return  # already reported by check_serviceaccount

    vals = chart_values()
    expected_name = vals["argo"]["workflowServiceAccount"]

    cm = next((d for d in docs if d["kind"] == "ConfigMap"
               and d["metadata"]["name"] == "qa-platform-gears-argo-qa-runs"), None)
    if cm is None:
        failures.append(
            "FAIL: no ConfigMap named qa-platform-gears-argo-qa-runs was rendered")
        return

    fragment = yaml.safe_load(cm["data"]["qa-runs-argo.yaml"])
    rendered_name = (fragment.get("argo") or {}).get("workflow_service_account")
    if rendered_name != expected_name:
        failures.append(
            "FAIL: qa-runs' rendered config names "
            f"workflow_service_account={rendered_name!r}, but this chart "
            f"provisions and grants a ServiceAccount named {expected_name!r}. "
            "qa-runs' executor refuses to construct against an unset or blank "
            "account (loud), but naming one this chart never creates or grants "
            "is a defect this guard can catch quietly instead.")
        return
    print(
        "PASS: qa-runs' rendered config names "
        f"workflow_service_account={expected_name!r}, matching the "
        "ServiceAccount this chart provisions and grants")


def main():
    failures = []
    check_serviceaccount(failures)
    check_role_and_binding(failures)
    check_config_names_the_same_account(failures)
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
