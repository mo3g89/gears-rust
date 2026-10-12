"""The Workflow admission guard, the TokenReview grant, and the absence of any
Secret writer (finding #94).

# What this guards

qa-runs needs `workflows: create` in `argo.namespace`. Without a policy, that
grant lets a compromised gears process submit a Workflow that mounts any Secret
in the namespace or runs as any ServiceAccount there. The chart therefore
renders a mandatory ValidatingAdmissionPolicy
(`templates/workflow-guard-admission-policy.yaml`) that admits, from
`qa-platform-gears` only, exactly the Workflow shape qa-runs emits. Runner
credentials no longer live in Kubernetes Secrets at all: qa-environments hands a
verified runner pod its credentials over HTTP, after a TokenReview of the pod's
projected token (`templates/rbac-tokenreview.yaml`).

Each check below is a way this could quietly regress:
  1. Rendering without the ValidatingAdmissionPolicy API fails, naming the API
     and Kubernetes 1.30 -- the guard is mandatory, not capability-gated.
  2. The policy and its Deny binding exist with the intended match constraints.
  3. The CEL key allowlists equal qa-runs' ADMITTED_*_KEYS constants (the
     constants are read from workflow.rs as source text).
  4. The projected-token audience in the policy equals qa-runs' and
     qa-environments' constants.
  5. The policy pins spec.serviceAccountName to argo.workflowServiceAccount.
  6. qa-platform-gears holds `tokenreviews: create` and nothing else cluster-wide.
  7. No secret-writer object renders, and nothing bound to qa-platform-gears
     touches `secrets`.
  8. The gears executor Role still reads pods (the verifier needs it).
  9. Every shape rule is CREATE-only: the Argo controller adds spec.arguments
     and template inputs/outputs after creation, so a shape rule evaluated on
     UPDATE would deny the cancel patch. The two UPDATE rules (spec, status)
     are the exceptions, and each must be UPDATE-only.
 10. The gears may not write a Workflow's status: a CREATE carrying one is
     refused, and an UPDATE must leave it exactly as it was (the cancel
     merge-patch touches spec.shutdown only, so it leaves status equal; the
     Argo controller's own status writes are not matched by the policy).

Standalone script, like its siblings in this directory -- see the Makefile's
`helm-tests` target for why each one is invoked directly."""
import pathlib
import re
import subprocess
import sys

import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
QA_PLATFORM = pathlib.Path(__file__).resolve().parents[3]
WORKFLOW_RS = QA_PLATFORM / "qa-runs/qa-runs/src/infra/executor/argo/workflow.rs"
POD_IDENTITY_RS = QA_PLATFORM / "qa-environments/qa-environments/src/infra/pod_identity.rs"
# Deliberately NOT "qa-platform": a hardcoded namespace would pass a render into it.
RELEASE_NAMESPACE = "qa-guard-workflow"
GEARS_SA = "qa-platform-gears"
VAP_API = "admissionregistration.k8s.io/v1/ValidatingAdmissionPolicy"
VAP_NAME = f"qa-platform-workflow-guard-{RELEASE_NAMESPACE}"
TOKENREVIEW_NAME = f"qa-platform-gears-tokenreview-{RELEASE_NAMESPACE}"
UPDATE_MESSAGE = "workflow-guard: an update may only set spec.shutdown"
MSG_STATUS_CREATE = "workflow-guard: a created Workflow may not carry a status"
MSG_STATUS_UPDATE = "workflow-guard: an update may not change status"
UPDATE_MESSAGES = (UPDATE_MESSAGE, MSG_STATUS_UPDATE)

# Validation messages, one per allowlist (Step 3 of the plan keeps them unique).
MSG_SPEC = "workflow-guard: spec key not in the qa-runs allowlist"
MSG_SA = "workflow-guard: spec.serviceAccountName must be the runner ServiceAccount"
MSG_TEMPLATE = "workflow-guard: template key not in the qa-runs allowlist"
MSG_DAG = "workflow-guard: dag task key not in the qa-runs allowlist"
MSG_VOLUME = "workflow-guard: volume not in the qa-runs allowlist"


def render(*extra, with_api=True):
    api = ["--api-versions", VAP_API] if with_api else []
    out = subprocess.run(
        ["helm", "template", "qa-platform", str(CHART),
         "--namespace", RELEASE_NAMESPACE, *api, *extra,
         "--set", "publicOrigin=https://example-workflow-guard-test.invalid",
         "--set", "keycloak.adminPassword=guard-fixture-not-a-real-password",
         "--set", "argo.workflowClientSecret=guard-fixture-not-a-real-workflow-secret",  # nosec: test fixture only
         "--set", "postgres.password=guard-fixture-not-a-real-db-password",  # nosec: test fixture only
         "--set", "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key",
         "--set", "collectReportSigningSecret=guard-fixture-not-a-real-collect-key"],
        capture_output=True, text=True)
    if out.returncode != 0:
        return None, out.stderr
    return [d for d in yaml.safe_load_all(out.stdout) if d], ""


def values():
    return yaml.safe_load((CHART / "values.yaml").read_text())


def ns_of(doc):
    # `helm template` leaves metadata.namespace unset on objects that do not
    # set it; Helm installs those into the release namespace.
    return doc["metadata"].get("namespace") or RELEASE_NAMESPACE


def find(docs, kind, name):
    return [d for d in docs if d["kind"] == kind and d["metadata"]["name"] == name]


def roles_bound_to(docs, sa_name, sa_namespace):
    """Every (binding, role) pair whose subjects include that ServiceAccount."""
    pairs = []
    for b in docs:
        if b["kind"] not in ("RoleBinding", "ClusterRoleBinding"):
            continue
        hit = any(
            s.get("kind") == "ServiceAccount" and s.get("name") == sa_name
            and (s.get("namespace") or ns_of(b)) == sa_namespace
            for s in b.get("subjects") or [])
        if not hit:
            continue
        ref = b.get("roleRef", {})
        for r in docs:
            if r["kind"] != ref.get("kind") or r["metadata"]["name"] != ref.get("name"):
                continue
            if r["kind"] == "Role" and ns_of(r) != ns_of(b):
                continue
            pairs.append((b, r))
    return pairs


def touches_secrets(rule):
    groups = rule.get("apiGroups") or []
    resources = rule.get("resources") or []
    return ("" in groups or "*" in groups) and ("secrets" in resources or "*" in resources)


def policy(docs):
    vaps = find(docs, "ValidatingAdmissionPolicy", VAP_NAME)
    return vaps[0] if vaps else None


def validation(docs, message):
    vap = policy(docs)
    if vap is None:
        return None
    for v in vap["spec"].get("validations") or []:
        if v.get("message") == message:
            return v
    return None


def rs_array(name):
    text = WORKFLOW_RS.read_text()
    m = re.search(r'pub const %s: &\[&str\] = &\[(.*?)\];' % name, text, re.S)
    if not m:
        return None
    return set(re.findall(r'"([^"]+)"', m.group(1)))


def rs_audience(path, const):
    m = re.search(r'pub const %s: &str = "([^"]+)";' % const, path.read_text())
    return m.group(1) if m else None


def cel_list(expression):
    """The first `k in [...]` list literal of a CEL expression."""
    m = re.search(r"\bk in \[([^\]]*)\]", expression)
    if not m:
        return None
    return set(re.findall(r"'([^']+)'", m.group(1)))


def check_render_fails_without_the_api(docs, failures):
    refused, stderr = render(with_api=False)
    if refused is not None:
        failures.append(
            f"FAIL: the chart rendered without {VAP_API}. The Workflow guard is "
            "mandatory: gears' workflows:create is unsafe without it.")
    elif "ValidatingAdmissionPolicy" not in stderr or "1.30" not in stderr:
        failures.append(f"FAIL: the refusal must name ValidatingAdmissionPolicy and 1.30; got:\n{stderr}")
    else:
        print("PASS: the chart refuses to render without the ValidatingAdmissionPolicy API")


def check_policy_and_binding(docs, failures):
    before = len(failures)
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} was rendered")
        return
    spec = vap["spec"]
    if spec.get("failurePolicy") != "Fail":
        failures.append(f"FAIL: {VAP_NAME} must set failurePolicy: Fail")
    rules = (spec.get("matchConstraints") or {}).get("resourceRules") or [{}]
    rule = rules[0]
    if rule.get("apiGroups") != ["argoproj.io"] or rule.get("resources") != ["workflows"] \
            or rule.get("operations") != ["CREATE", "UPDATE"]:
        failures.append(f"FAIL: {VAP_NAME} must match CREATE/UPDATE of argoproj.io workflows; got {rule!r}")
    ns_sel = {"matchLabels": {"kubernetes.io/metadata.name": values()["argo"]["namespace"]}}
    if (spec.get("matchConstraints") or {}).get("namespaceSelector") != ns_sel:
        failures.append(f"FAIL: {VAP_NAME}'s namespaceSelector must be {ns_sel!r}")
    expected = [f'request.userInfo.username == "system:serviceaccount:{RELEASE_NAMESPACE}:{GEARS_SA}"']
    if [c.get("expression") for c in spec.get("matchConditions") or []] != expected:
        failures.append(f"FAIL: {VAP_NAME}'s matchConditions must be exactly {expected}")
    bindings = find(docs, "ValidatingAdmissionPolicyBinding", VAP_NAME)
    if not bindings:
        failures.append(f"FAIL: no ValidatingAdmissionPolicyBinding {VAP_NAME} was rendered")
    else:
        b = bindings[0]["spec"]
        if b.get("validationActions") != ["Deny"] or b.get("policyName") != VAP_NAME \
                or (b.get("matchResources") or {}).get("namespaceSelector") != ns_sel:
            failures.append(f"FAIL: binding {VAP_NAME} must name the policy, Deny, and select the Argo namespace; got {b!r}")
    if len(failures) == before:
        print(f"PASS: {VAP_NAME} and its Deny binding match CREATE/UPDATE of workflows from {GEARS_SA} only")


def check_allowlist_matches_qa_runs(docs, failures):
    before = len(failures)
    for const, message in (("ADMITTED_SPEC_KEYS", MSG_SPEC),
                           ("ADMITTED_TEMPLATE_KEYS", MSG_TEMPLATE),
                           ("ADMITTED_DAG_TASK_KEYS", MSG_DAG),
                           ("ADMITTED_VOLUME_KEYS", MSG_VOLUME)):
        keys = rs_array(const)
        v = validation(docs, message)
        if keys is None:
            failures.append(f"FAIL: {const} not found in {WORKFLOW_RS}")
            continue
        if v is None:
            failures.append(f"FAIL: no validation with message {message!r}")
            continue
        cel = cel_list(v["expression"])
        if cel != keys:
            failures.append(
                f"FAIL: the policy's allowlist for {message!r} is {sorted(cel or [])}; "
                f"qa-runs' {const} is {sorted(keys)}. The two must match exactly.")
    if len(failures) == before:
        print("PASS: the policy's key allowlists equal qa-runs' ADMITTED_*_KEYS")


def check_audience_matches_qa_runs(docs, failures):
    before = len(failures)
    runs = rs_audience(WORKFLOW_RS, "RUNNER_CREDENTIALS_AUDIENCE")
    envs = rs_audience(POD_IDENTITY_RS, "AUDIENCE")
    v = validation(docs, MSG_VOLUME)
    m = re.search(r"audience == '([^']+)'", v["expression"]) if v else None
    cel = m.group(1) if m else None
    if not runs or runs != envs or runs != cel:
        failures.append(
            f"FAIL: runner-credentials audience differs: qa-runs {runs!r}, "
            f"qa-environments {envs!r}, policy {cel!r}.")
    if len(failures) == before:
        print(f"PASS: the audience {runs!r} is the same in qa-runs, qa-environments and the policy")


def check_service_account_pinned(docs, failures):
    before = len(failures)
    sa = values()["argo"]["workflowServiceAccount"]
    v = validation(docs, MSG_SA)
    if v is None or f'"{sa}"' not in v["expression"]:
        failures.append(f"FAIL: the SA validation does not pin argo.workflowServiceAccount={sa!r}")
    other, stderr = render("--set", "argo.workflowServiceAccount=other-sa")
    if other is None:
        failures.append(f"FAIL: render with workflowServiceAccount=other-sa failed:\n{stderr}")
    else:
        v2 = validation(other, MSG_SA)
        if v2 is None or '"other-sa"' not in v2["expression"] or f'"{sa}"' in v2["expression"]:
            failures.append("FAIL: the SA validation does not follow argo.workflowServiceAccount")
    if len(failures) == before:
        print("PASS: spec.serviceAccountName is pinned to argo.workflowServiceAccount")


def check_tokenreview_grant(docs, failures):
    before = len(failures)
    roles = find(docs, "ClusterRole", TOKENREVIEW_NAME)
    if not roles:
        failures.append(f"FAIL: no ClusterRole {TOKENREVIEW_NAME} was rendered")
    else:
        expected = [{"apiGroups": ["authentication.k8s.io"], "resources": ["tokenreviews"], "verbs": ["create"]}]
        if roles[0].get("rules") != expected:
            failures.append(f"FAIL: ClusterRole {TOKENREVIEW_NAME} rules are {roles[0].get('rules')!r}; expected exactly {expected!r}")
    bindings = find(docs, "ClusterRoleBinding", TOKENREVIEW_NAME)
    ok = bool(bindings) and bindings[0]["roleRef"]["name"] == TOKENREVIEW_NAME and any(
        s.get("kind") == "ServiceAccount" and s.get("name") == GEARS_SA
        and s.get("namespace") == RELEASE_NAMESPACE for s in bindings[0].get("subjects") or [])
    if not ok:
        failures.append(f"FAIL: no ClusterRoleBinding binds {TOKENREVIEW_NAME} to {GEARS_SA} in {RELEASE_NAMESPACE!r}")
    if len(failures) == before:
        print(f"PASS: {GEARS_SA} holds exactly tokenreviews:create cluster-wide")


def check_no_writer_anywhere(docs, failures):
    before = len(failures)
    for d in docs:
        if "secret-writer" in d["metadata"]["name"]:
            failures.append(f"FAIL: {d['kind']} {d['metadata']['name']} renders; the Secret writer is gone")
    for b, r in roles_bound_to(docs, GEARS_SA, RELEASE_NAMESPACE):
        for rule in r.get("rules") or []:
            if touches_secrets(rule):
                failures.append(
                    f"FAIL: {r['kind']} {r['metadata']['name']} (bound to {GEARS_SA} by "
                    f"{b['metadata']['name']}) grants {rule.get('verbs')} on secrets")
    if len(failures) == before:
        print("PASS: no secret-writer object renders and nothing bound to the gears touches secrets")


def check_gears_can_still_read_pods_in_argo(docs, failures):
    roles = find(docs, "Role", "qa-platform-gears-executor")
    ok = bool(roles) and any(
        "" in (r.get("apiGroups") or []) and "pods" in (r.get("resources") or [])
        and "get" in (r.get("verbs") or []) for r in roles[0].get("rules") or [])
    if not ok:
        failures.append("FAIL: qa-platform-gears-executor no longer grants pods: get, which the pod-identity verifier needs")
    else:
        print("PASS: qa-platform-gears-executor still grants pods: get")


def check_shape_rules_are_create_only(docs, failures):
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} was rendered")
        return
    bad = []
    for v in vap["spec"].get("validations") or []:
        expression = " ".join(v["expression"].split())
        if v.get("message") in UPDATE_MESSAGES:
            if not expression.startswith("request.operation != 'UPDATE' ||"):
                bad.append(v.get("message"))
            continue
        if not expression.startswith("request.operation != 'CREATE' ||"):
            bad.append(v.get("message"))
    if bad:
        failures.append(
            f"FAIL: shape rules not CREATE-only: {bad}. The Argo controller adds "
            "spec.arguments and template inputs/outputs after creation, so these "
            "would deny the cancel patch.")
    else:
        print("PASS: every shape rule is CREATE-only")


def check_status_is_not_the_gears_to_write(docs, failures):
    before = len(failures)
    create = validation(docs, MSG_STATUS_CREATE)
    update = validation(docs, MSG_STATUS_UPDATE)
    if create is None:
        failures.append(f"FAIL: no validation with message {MSG_STATUS_CREATE!r}")
    elif " ".join(create["expression"].split()) != "request.operation != 'CREATE' || !has(object.status)":
        failures.append(f"FAIL: {MSG_STATUS_CREATE!r} must be exactly `request.operation != 'CREATE' || !has(object.status)`; got {create['expression']!r}")
    if update is None:
        failures.append(f"FAIL: no validation with message {MSG_STATUS_UPDATE!r}")
    else:
        e = " ".join(update["expression"].split())
        for needle in ("has(object.status) == has(oldObject.status)", "object.status == oldObject.status"):
            if needle not in e:
                failures.append(f"FAIL: {MSG_STATUS_UPDATE!r} must contain `{needle}`; got {e!r}")
    if len(failures) == before:
        print("PASS: the gears can neither create a Workflow with a status nor change one")


def main():
    docs, stderr = render()
    if docs is None:
        # Before the chart carries the guard the render may fail outright; the
        # checks below then all report.
        print(f"FAIL: the render with {VAP_API} served must succeed:\n{stderr}")
        return 1
    failures = []
    for check in (check_render_fails_without_the_api, check_policy_and_binding,
                  check_allowlist_matches_qa_runs, check_audience_matches_qa_runs,
                  check_service_account_pinned, check_tokenreview_grant,
                  check_no_writer_anywhere, check_gears_can_still_read_pods_in_argo,
                  check_shape_rules_are_create_only, check_status_is_not_the_gears_to_write):
        mine = []
        check(docs, mine)
        failures += mine
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
