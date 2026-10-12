"""The runner guard: no principal but the gears, the Argo workflow-controller
and the kubelets may touch qa-runs Workflows, runner pods or runner tokens.

# What this guards

The Workflow guard (`templates/workflow-guard-admission-policy.yaml`) only
constrains what `qa-platform-gears` submits. Anyone else with write rights in
`argo.namespace` could otherwise:
  - patch a live qa-runs Workflow;
  - create a Workflow or a pod that runs as the runner ServiceAccount;
  - patch a runner pod (its image, say) or add an ephemeral container to it;
  - mint a token for the runner ServiceAccount through `serviceaccounts/token`,
    bound to a live runner pod, and fetch that run's credentials with it.

`templates/runner-guard-admission-policy.yaml` closes those. Each check below is
a way it could quietly regress:
  1. The policy and its Deny binding exist, with failurePolicy: Fail, scoped to
     the Argo namespace, and NOT narrowed to one principal by matchConditions
     (the point is every principal).
  2. matchConstraints cover Workflow CREATE/UPDATE, pod CREATE/UPDATE,
     pods/ephemeralcontainers UPDATE, serviceaccounts/token CREATE and Secret
     CREATE/UPDATE.
  3. The expressions name the runner ServiceAccount, the gears, the Argo
     controller and the kubelets, and follow the values that set them.
  4. The Workflow rule also catches the runner ServiceAccount named per
     template, in templateDefaults (its serviceAccountName, executor or
     podSpecPatch), in an executor or in a podSpecPatch, and on the old object
     of an UPDATE in spec or templateDefaults.
  5. The chart refuses to render without argo.controllerServiceAccount.
  6. The garbage-collector exemption is UPDATE-only: it lets foreground and
     orphan deletes finish, and must never let anyone CREATE a runner pod.
  7. Every read of request.subResource sits behind has(request.subResource):
     the field is absent on a main-resource write, a bare read fails the
     evaluation, and failurePolicy: Fail then denies every pod write (the Argo
     controller's included). Measured on a kind cluster, 2026-10-12.
  8. Every runner-guard message is unique, and verify-k8s.sh exercises each one
     on the cluster (CEL that compiles is not CEL that denies).
  9. The identities the guard trusts are themselves guarded. The Argo
     controller's ServiceAccount (when it lives in argo.namespace) and
     qa-platform-gears (when argo.namespace is the release namespace) are
     listed in the `trustedServiceAccounts` variable, decided at render time.
     Only kubelets may mint their tokens through TokenRequest
     (`serviceaccounts/token`); no new ServiceAccount-token Secret may name
     one (check 10), and a pre-existing one keeps working until an operator
     deletes it. Only the ReplicaSet controller may
     CREATE a pod running as one of them, only the garbage collector may
     UPDATE one, and nobody may add an ephemeral container to one. Otherwise
     anyone who can create pods or tokens in that namespace becomes the
     controller and creates a runner pod with a forged ownerReference.
 10. No one may create a Secret of type kubernetes.io/service-account-token
     that names the runner ServiceAccount or a trusted one: the token
     controller would fill it with a long-lived token, outside any kubelet.
     An UPDATE of such a Secret that already named the same account (the
     token controller filling `data`) is let through. Each annotations read
     in the two token-Secret variables sits behind its own has(), so either
     variable is safe without the other.

Standalone script, like its siblings in this directory -- see the Makefile's
`helm-tests` target for why each one is invoked directly."""
import pathlib
import re
import subprocess
import sys

import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
VERIFY_K8S = pathlib.Path(__file__).resolve().parents[2] / "remote/verify-k8s.sh"
# Deliberately NOT "qa-platform": a hardcoded namespace would pass a render into it.
RELEASE_NAMESPACE = "qa-guard-runner"
VAP_API = "admissionregistration.k8s.io/v1/ValidatingAdmissionPolicy"
VAP_NAME = f"qa-platform-runner-guard-{RELEASE_NAMESPACE}"
GEARS_USER = f"system:serviceaccount:{RELEASE_NAMESPACE}:qa-platform-gears"
RS_CONTROLLER = "system:serviceaccount:kube-system:replicaset-controller"
# The ReplicaSet controller writes as its own ServiceAccount where
# kube-controller-manager runs with --use-service-account-credentials (k3s:
# read from the stand's k3s journal, 2026-10-12), as this user where it does not.
KCM_USER = "system:kube-controller-manager"
TRUSTED_VAR = "trustedServiceAccounts"

REQUIRED_RULES = {
    ("argoproj.io", "workflows", "CREATE"),
    ("argoproj.io", "workflows", "UPDATE"),
    ("", "pods", "CREATE"),
    ("", "pods", "UPDATE"),
    ("", "pods/ephemeralcontainers", "UPDATE"),
    ("", "serviceaccounts/token", "CREATE"),
    ("", "secrets", "CREATE"),
    ("", "secrets", "UPDATE"),
}


def render(*extra, with_api=True):
    api = ["--api-versions", VAP_API] if with_api else []
    out = subprocess.run(
        ["helm", "template", "qa-platform", str(CHART),
         "--namespace", RELEASE_NAMESPACE, *api, *extra,
         "--set", "publicOrigin=https://example-runner-guard-test.invalid",
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


def find(docs, kind, name):
    return [d for d in docs if d["kind"] == kind and d["metadata"]["name"] == name]


def policy(docs):
    vaps = find(docs, "ValidatingAdmissionPolicy", VAP_NAME)
    return vaps[0] if vaps else None


def all_cel(vap):
    """Every variable and validation expression, whitespace-normalised."""
    spec = vap["spec"]
    exprs = [v["expression"] for v in spec.get("variables") or []]
    exprs += [v["expression"] for v in spec.get("validations") or []]
    return " ".join(" ".join(e.split()) for e in exprs)


def controller_user(v, namespace=None):
    argo = v["argo"]
    ns = namespace or argo.get("controllerNamespace") or argo["namespace"]
    return f"system:serviceaccount:{ns}:{argo['controllerServiceAccount']}"


def check_policy_and_binding(docs, failures):
    before = len(failures)
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} was rendered")
        return
    spec = vap["spec"]
    if spec.get("failurePolicy") != "Fail":
        failures.append(f"FAIL: {VAP_NAME} must set failurePolicy: Fail")
    if spec.get("matchConditions"):
        failures.append(
            f"FAIL: {VAP_NAME} carries matchConditions {spec['matchConditions']!r}; the runner "
            "guard is about every OTHER principal, so it must not be narrowed to one")
    other_ns, stderr = render("--set", "argo.namespace=other-argo-selector-ns")
    other_vap = policy(other_ns) if other_ns else None
    if other_vap is None:
        failures.append(f"FAIL: render with argo.namespace overridden failed:\n{stderr}")
    else:
        want = {"matchLabels": {"kubernetes.io/metadata.name": "other-argo-selector-ns"}}
        if (other_vap["spec"].get("matchConstraints") or {}).get("namespaceSelector") != want:
            failures.append(f"FAIL: {VAP_NAME}'s namespaceSelector does not follow argo.namespace (want {want!r})")
        ob = find(other_ns, "ValidatingAdmissionPolicyBinding", VAP_NAME)
        if not ob or (ob[0]["spec"].get("matchResources") or {}).get("namespaceSelector") != want:
            failures.append(f"FAIL: binding {VAP_NAME}'s namespaceSelector does not follow argo.namespace")
    ns_sel = {"matchLabels": {"kubernetes.io/metadata.name": values()["argo"]["namespace"]}}
    if (spec.get("matchConstraints") or {}).get("namespaceSelector") != ns_sel:
        failures.append(f"FAIL: {VAP_NAME}'s namespaceSelector must be {ns_sel!r}")
    bindings = find(docs, "ValidatingAdmissionPolicyBinding", VAP_NAME)
    if not bindings:
        failures.append(f"FAIL: no ValidatingAdmissionPolicyBinding {VAP_NAME} was rendered")
    else:
        b = bindings[0]["spec"]
        if b.get("validationActions") != ["Deny"] or b.get("policyName") != VAP_NAME \
                or (b.get("matchResources") or {}).get("namespaceSelector") != ns_sel:
            failures.append(f"FAIL: binding {VAP_NAME} must name the policy, Deny, and select the Argo namespace; got {b!r}")
    if len(failures) == before:
        print(f"PASS: {VAP_NAME} and its Deny binding exist, fail closed and match every principal in the Argo namespace")


def check_resource_rules(docs, failures):
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} to read resourceRules from")
        return
    covered = set()
    for r in (vap["spec"].get("matchConstraints") or {}).get("resourceRules") or []:
        for g in r.get("apiGroups") or []:
            for res in r.get("resources") or []:
                for op in r.get("operations") or []:
                    covered.add((g, res, op))
    missing = REQUIRED_RULES - covered
    if missing:
        failures.append(f"FAIL: {VAP_NAME} does not match {sorted(missing)}")
    else:
        print(f"PASS: {VAP_NAME} matches Workflow and pod writes, ephemeral containers, runner token requests and Secret writes")


def check_principals_named(docs, failures):
    before = len(failures)
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} to read expressions from")
        return
    v = values()
    cel = all_cel(vap)
    for what, literal in (("the runner ServiceAccount", f"'{v['argo']['workflowServiceAccount']}'"),
                          ("the gears' username", f"'{GEARS_USER}'"),
                          ("the Argo controller's username", f"'{controller_user(v)}'"),
                          ("the kubelets' username prefix", "'system:node:'")):
        if literal not in cel:
            failures.append(f"FAIL: {VAP_NAME}'s expressions do not name {what} ({literal})")
    if len(failures) == before:
        print("PASS: the policy names the runner ServiceAccount, the gears, the Argo controller and the kubelets")


def check_follows_values(docs, failures):
    before = len(failures)
    other, stderr = render("--set", "argo.workflowServiceAccount=other-runner",
                           "--set", "argo.controllerServiceAccount=other-controller",
                           "--set", "argo.controllerNamespace=other-argo-ns")
    if other is None:
        failures.append(f"FAIL: render with other runner/controller values failed:\n{stderr}")
        return
    vap = policy(other)
    if vap is None:
        failures.append(f"FAIL: no {VAP_NAME} in the render with other runner/controller values")
        return
    cel = all_cel(vap)
    default_sa = values()["argo"]["workflowServiceAccount"]
    if "'other-runner'" not in cel or f"'{default_sa}'" in cel:
        failures.append("FAIL: the runner guard does not follow argo.workflowServiceAccount")
    if "'system:serviceaccount:other-argo-ns:other-controller'" not in cel:
        failures.append("FAIL: the runner guard does not follow argo.controllerServiceAccount/controllerNamespace")
    if len(failures) == before:
        print("PASS: the runner guard follows argo.workflowServiceAccount, controllerServiceAccount and controllerNamespace")


def check_workflow_indirections(docs, failures):
    """A Workflow names a pod's ServiceAccount in more places than spec.serviceAccountName."""
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} to read expressions from")
        return
    cel = all_cel(vap)
    missing = [n for n in ("t.serviceAccountName", "object.spec.templateDefaults",
                           "t.podSpecPatch", "object.spec.podSpecPatch",
                           "t.executor", "object.spec.executor",
                           "t.steps", "s.inline", "t.dag.tasks", "k.inline",
                           "object.spec.templateDefaults.podSpecPatch",
                           "object.spec.templateDefaults.executor",
                           "oldObject.spec.templateDefaults") if n not in cel]
    if missing:
        failures.append(
            f"FAIL: the Workflow rule does not look at {missing}; the Argo controller "
            "would create a runner-ServiceAccount pod from a foreign Workflow naming it there")
    else:
        print("PASS: the Workflow rule catches the runner ServiceAccount per template, in templateDefaults, executors and podSpecPatch")


def check_subresource_reads_are_guarded(docs, failures):
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} to read expressions from")
        return
    cel = all_cel(vap)
    reads = len(re.findall(r"request\.subResource ==", cel))
    guarded = len(re.findall(r"has\(request\.subResource\) && request\.subResource ==", cel))
    if reads == 0 or reads != guarded:
        failures.append(
            f"FAIL: {reads - guarded} read(s) of request.subResource are not behind "
            "has(request.subResource); on a main-resource write the field is absent, "
            "the evaluation errors and failurePolicy: Fail denies every pod write")
    else:
        print("PASS: every request.subResource read is guarded by has()")


def check_gc_exemption_is_update_only(docs, failures):
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} to read variables from")
        return
    gc = [v for v in vap["spec"].get("variables") or [] if "generic-garbage-collector" in v["expression"]]
    if len(gc) != 1 or not " ".join(gc[0]["expression"].split()).startswith("request.operation == 'UPDATE' && "):
        failures.append(
            "FAIL: the garbage-collector exemption must be exactly one variable starting "
            f"`request.operation == 'UPDATE' && `; got {[v['expression'] for v in gc]!r}")
    else:
        print("PASS: the garbage-collector exemption is UPDATE-only")


def check_controller_is_required(docs, failures):
    refused, stderr = render("--set", "argo.controllerServiceAccount=")
    if refused is not None:
        failures.append("FAIL: the chart rendered with an empty argo.controllerServiceAccount; "
                        "the runner guard would then exempt nobody and stop every run")
    elif "argo.controllerServiceAccount" not in stderr:
        failures.append(f"FAIL: the refusal must name argo.controllerServiceAccount; got:\n{stderr}")
    else:
        print("PASS: the chart refuses to render without argo.controllerServiceAccount")


def check_messages_exercised(docs, failures):
    before = len(failures)
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} to read messages from")
        return
    messages = [v.get("message") or "" for v in vap["spec"].get("validations") or []]
    if not messages or any(not m.startswith("runner-guard: ") for m in messages):
        failures.append(f"FAIL: every runner-guard validation needs a 'runner-guard: ' message; got {messages!r}")
    if len(set(messages)) != len(messages):
        failures.append(f"FAIL: runner-guard messages must be unique (verify-k8s greps them); got {messages!r}")
    script = VERIFY_K8S.read_text()
    for m in messages:
        if f'"{m}"' not in script:
            failures.append(f"FAIL: verify-k8s.sh has no case expecting {m!r}")
    if len(failures) == before:
        print("PASS: every runner-guard message is unique and verify-k8s.sh expects each one")


def variable(vap, name):
    for v in vap["spec"].get("variables") or []:
        if v["name"] == name:
            return " ".join(v["expression"].split())
    return None


def validation(vap, needle):
    """The one validation expression whose message contains `needle`, whitespace-normalised."""
    hits = [" ".join(v["expression"].split()) for v in vap["spec"].get("validations") or []
            if needle in (v.get("message") or "")]
    return hits[0] if len(hits) == 1 else None


def check_trusted_identities_follow_namespaces(docs, failures):
    """Which ServiceAccounts the guard trusts in argo.namespace is a render-time decision."""
    before = len(failures)
    controller = values()["argo"]["controllerServiceAccount"]
    cases = (
        ("defaults (controller in argo.namespace, gears elsewhere)", (), f"['{controller}']"),
        ("argo.namespace = the release namespace",
         ("--set", f"argo.namespace={RELEASE_NAMESPACE}"),
         f"['{controller}', 'qa-platform-gears']"),
        ("the controller in its own namespace",
         ("--set", "argo.controllerNamespace=argo-controller-elsewhere"), "[]"),
        ("argo.namespace = the release namespace, the controller elsewhere",
         ("--set", f"argo.namespace={RELEASE_NAMESPACE}",
          "--set", "argo.controllerNamespace=argo-controller-elsewhere"),
         "['qa-platform-gears']"),
    )
    for label, extra, want in cases:
        rendered, stderr = render(*extra) if extra else (docs, "")
        vap = policy(rendered) if rendered else None
        if vap is None:
            failures.append(f"FAIL: render for {label} failed:\n{stderr}")
            continue
        got = variable(vap, TRUSTED_VAR)
        if got != want:
            failures.append(f"FAIL: {TRUSTED_VAR} for {label} must be {want}; got {got!r}")
    if len(failures) == before:
        print(f"PASS: {TRUSTED_VAR} lists the controller only in argo.namespace and the gears only when argo.namespace is the release namespace")


def check_trusted_identities_are_guarded(docs, failures):
    before = len(failures)
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} to read expressions from")
        return
    pod = variable(vap, "trustedPod") or ""
    for piece in ("variables.isPod && (",
                  f"object.spec.serviceAccountName in variables.{TRUSTED_VAR}",
                  f"object.spec.serviceAccount in variables.{TRUSTED_VAR}",
                  f"oldObject.spec.serviceAccountName in variables.{TRUSTED_VAR}"):
        if piece not in pod:
            failures.append(f"FAIL: the trustedPod variable lacks {piece!r}; got {pod!r}")
    want_pod = (f"!variables.trustedPod || variables.isEphemeral || (request.operation == 'CREATE' "
                f"&& variables.user in ['{RS_CONTROLLER}', '{KCM_USER}']) || variables.gcUpdate")
    if validation(vap, "only the ReplicaSet controller may create") != want_pod:
        failures.append(
            "FAIL: pods running as a trusted ServiceAccount must be CREATEd by the ReplicaSet "
            f"controller only and UPDATEd by the garbage collector only; want {want_pod!r}, got "
            f"{validation(vap, 'only the ReplicaSet controller may create')!r}")
    if all_cel(vap).count(f"'{RS_CONTROLLER}'") != 1:
        failures.append(f"FAIL: '{RS_CONTROLLER}' must appear exactly once, in the CREATE-only pod rule")
    want_eph = "!(variables.runnerPod || variables.trustedPod) || !variables.isEphemeral"
    if validation(vap, "ephemeral container") != want_eph:
        failures.append(f"FAIL: the ephemeral-container rule must be {want_eph!r}; got "
                        f"{validation(vap, 'ephemeral container')!r}")
    tok = validation(vap, "mint tokens") or ""
    want_tok = (f"!variables.isToken || !(request.name == '{values()['argo']['workflowServiceAccount']}' "
                f"|| request.name in variables.{TRUSTED_VAR}) || (variables.user.startsWith('system:node:') "
                "&& has(request.userInfo.groups) && 'system:nodes' in request.userInfo.groups)")
    if tok != want_tok:
        failures.append(f"FAIL: the token rule must cover the trusted ServiceAccounts; want {want_tok!r}, got {tok!r}")
    if TRUSTED_VAR not in VERIFY_K8S.read_text():
        failures.append(f"FAIL: verify-k8s.sh does not read {TRUSTED_VAR} from the deployed policy")
    if len(failures) == before:
        print("PASS: trusted ServiceAccounts: kubelet-only TokenRequest tokens, ReplicaSet-controller-only pod CREATE, GC-only UPDATE, no ephemeral containers")


def check_token_secrets_are_guarded(docs, failures):
    """A ServiceAccount-token Secret is a second way to mint a guarded account's token."""
    before = len(failures)
    vap = policy(docs)
    if vap is None:
        failures.append(f"FAIL: no ValidatingAdmissionPolicy {VAP_NAME} to read expressions from")
        return
    want_secret = ("!variables.guardedTokenSecret || "
                   "(request.operation == 'UPDATE' && variables.wasGuardedTokenSecret)")
    got = validation(vap, "ServiceAccount token Secret")
    if got != want_secret:
        failures.append(f"FAIL: the token-Secret rule must be {want_secret!r}; got {got!r}")
    secret = variable(vap, "guardedTokenSecret") or ""
    for piece in ("kubernetes.io/service-account-token", "kubernetes.io/service-account.name",
                  f"variables.{TRUSTED_VAR}"):
        if piece not in secret:
            failures.append(f"FAIL: the guardedTokenSecret variable lacks {piece!r}; got {secret!r}")
    # Each variable must be safe on its own: a bare read of a missing
    # annotations map errors, and failurePolicy: Fail turns that into a deny.
    for name in ("guardedTokenSecret", "wasGuardedTokenSecret"):
        cel = variable(vap, name) or ""
        for side in ("oldObject", "object"):
            read = re.search(rf"(?<![A-Za-z]){side}\.metadata\.annotations\[", cel)
            guard = re.search(rf"(?<![A-Za-z])has\({side}\.metadata\.annotations\)", cel)
            if read and (guard is None or guard.start() > read.start()):
                failures.append(f"FAIL: {name} reads {side}.metadata.annotations without "
                                f"has({side}.metadata.annotations) before it; got {cel!r}")
    if len(failures) == before:
        print("PASS: no one may create a ServiceAccount-token Secret for the runner or a trusted ServiceAccount")


def main():
    docs, stderr = render()
    if docs is None:
        print(f"FAIL: the render with {VAP_API} served must succeed:\n{stderr}")
        return 1
    failures = []
    for check in (check_policy_and_binding, check_resource_rules, check_principals_named,
                  check_follows_values, check_workflow_indirections,
                  check_subresource_reads_are_guarded, check_gc_exemption_is_update_only,
                  check_controller_is_required, check_messages_exercised,
                  check_trusted_identities_follow_namespaces,
                  check_trusted_identities_are_guarded, check_token_secrets_are_guarded):
        mine = []
        check(docs, mine)
        failures += mine
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
