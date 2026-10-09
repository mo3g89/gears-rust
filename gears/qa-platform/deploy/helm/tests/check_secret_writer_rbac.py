"""The D4 runner-Secret writer authenticates as its own ServiceAccount, and the
gears pod's ambient ServiceAccount cannot touch a Secret in the Argo namespace.

# What changed, and what this guards (finding #94)

qa-environments' runner-credential writer
(`qa-environments/src/infra/runner_secret_writer.rs`) server-side-applies one
Secret per stored credential into `argo.namespace`. That needs `create` and
`patch` on `secrets`, namespace-wide: the names are derived per tenant and per
credential at runtime, so `resourceNames` cannot enumerate them. The grant used
to sit on `qa-platform-gears-executor`, bound to `qa-platform-gears` -- the SA
every gear in the pod runs as, qa-runs included, whose Argo access (workflows,
pods, pods/log) never needs a Secret.

The grant now belongs to a single-purpose ServiceAccount,
`qa-platform-secret-writer`. Its token comes from a legacy
`kubernetes.io/service-account-token` Secret (a projected token is always the
pod's OWN account, so it cannot be used for a second one), mounted into the
gears container, and a chart-rendered kubeconfig points qa-environments'
`argo.kubeconfig_path` at it. qa-runs keeps `Config::infer()`.

Each check below is a way this could quietly regress:
  1. No Role or ClusterRole bound to `qa-platform-gears` grants ANY verb on
     `secrets` -- moving the grant and leaving a copy behind is not a move.
  2. The Role(s) bound to `qa-platform-secret-writer` grant exactly
     `create` + `patch` on core `secrets`, and nothing else at all.
  3. That binding lives in `argo.namespace` and its subject names the RELEASE
     namespace explicitly (the guard renders into a non-default namespace so a
     hardcoded one fails).
  4. The ServiceAccount exists in the release namespace with
     `automountServiceAccountToken: false` (no pod runs AS it), and its token
     Secret has type `kubernetes.io/service-account-token`, the
     `kubernetes.io/service-account.name` annotation naming it, and no
     `data`/`stringData` -- the token controller fills those, and a rendered
     value would be a manifest-borne credential Helm fights on every upgrade.
  5. The qa-environments fragment's `kubeconfig_path` is a path the gears
     container actually mounts from a ConfigMap key that exists; that
     kubeconfig's `tokenFile` and `certificate-authority` are files the gears
     container actually mounts from the token Secret, and it carries no
     inline `token`, and its `server` is the in-cluster API address. The
     qa-runs fragment carries NO `kubeconfig_path` (qa-runs stays on the pod's
     own token), and the gears pod runs as `qa-platform-gears`. The token
     volume is `optional: true`, so revoking the token does not hold every
     gear in ContainerCreating.
  6. (Fix round 1.) Where `admissionregistration.k8s.io/v1/
     ValidatingAdmissionPolicy` is served, a policy and a Deny binding
     restrict the writer's identity to Opaque Secrets named with the runner
     prefix -- the exact CEL expressions are pinned, and the prefix equals the
     qa-environments fragment's `argo.secret_prefix`. Where the API is not
     served, neither object renders (it would fail the install). The prefix
     itself must survive `secret_name`'s sanitise/truncate unchanged, or a
     legitimate name would not start with it and the policy would deny the
     writer's own writes.
  7. (2026-10-08 re-verification of #94.) The chart refuses to render when
     `argo.namespace` equals the release namespace. The policy admits the
     writer to every Opaque Secret named with the runner prefix, and every
     Secret this chart owns carries that prefix too, so a shared namespace
     would let the writer overwrite them (the realm, the gears config).

Standalone script, like its siblings in this directory -- see the Makefile's
`helm-tests` target for why each one is invoked directly."""
import pathlib
import posixpath
import subprocess
import sys

import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
# Deliberately NOT "qa-platform": a subject namespace hardcoded to the usual
# release namespace would pass a render into that namespace.
RELEASE_NAMESPACE = "qa-guard-secret-writer"
GEARS_SA = "qa-platform-gears"
WRITER_SA = "qa-platform-secret-writer"
WRITER_TOKEN_SECRET = "qa-platform-secret-writer-token"
QA_ENV_CONFIGMAP = "qa-platform-gears-argo-qa-environments"
QA_ENV_FRAGMENT_KEY = "qa-environments-argo.yaml"


VAP_API = "admissionregistration.k8s.io/v1/ValidatingAdmissionPolicy"
VAP_NAME = f"qa-platform-secret-writer-guard-{RELEASE_NAMESPACE}"
IN_CLUSTER_SERVER = "https://kubernetes.default.svc"
# runner_secret_writer.rs: MAX_SECRET_NAME_LEN - DIGEST_HEX_LEN - 1.
READABLE_BUDGET = 63 - 16 - 1


def render(*extra):
    out = subprocess.run(
        ["helm", "template", "qa-platform", str(CHART),
         "--namespace", RELEASE_NAMESPACE, *extra,
         "--set", "publicOrigin=https://example-secret-writer-test.invalid",
         "--set", "keycloak.adminPassword=guard-fixture-not-a-real-password",
         "--set", "argo.workflowClientSecret=guard-fixture-not-a-real-workflow-secret",  # nosec: test fixture only
         "--set", "postgres.password=guard-fixture-not-a-real-db-password",  # nosec: test fixture only
         "--set", "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key",
         "--set", "collectReportSigningSecret=guard-fixture-not-a-real-collect-key"],
        capture_output=True, text=True)
    if out.returncode != 0:
        return None, out.stderr
    return [d for d in yaml.safe_load_all(out.stdout) if d], ""


def argo_namespace():
    return yaml.safe_load((CHART / "values.yaml").read_text())["argo"]["namespace"]


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


def check_gears_sa_has_no_secrets(docs, failures):
    for b, r in roles_bound_to(docs, GEARS_SA, RELEASE_NAMESPACE):
        for rule in r.get("rules") or []:
            if touches_secrets(rule):
                failures.append(
                    f"FAIL: {r['kind']} {r['metadata']['name']} (bound to "
                    f"{GEARS_SA} by {b['metadata']['name']}) grants "
                    f"{rule.get('verbs')} on secrets. The Argo-namespace Secret "
                    f"grant belongs to {WRITER_SA} alone; every gear in the pod "
                    f"runs as {GEARS_SA}.")
    if not failures:
        print(f"PASS: no Role bound to {GEARS_SA} grants anything on secrets")


def check_writer_role(docs, failures):
    before = len(failures)
    argo_ns = argo_namespace()
    pairs = roles_bound_to(docs, WRITER_SA, RELEASE_NAMESPACE)
    if not pairs:
        failures.append(
            f"FAIL: no RoleBinding subjects ServiceAccount {WRITER_SA} in "
            f"namespace {RELEASE_NAMESPACE!r} (the release namespace) -- the "
            "D4 writer would be Forbidden on every cycle.")
        return
    granted = []
    for b, r in pairs:
        if b["kind"] != "RoleBinding" or ns_of(b) != argo_ns:
            failures.append(
                f"FAIL: {b['kind']} {b['metadata']['name']} binding {WRITER_SA} "
                f"is in {ns_of(b)!r}; expected a RoleBinding in {argo_ns!r} "
                "(argo.namespace) and nothing wider.")
        for rule in r.get("rules") or []:
            granted.append((tuple(sorted(rule.get("apiGroups") or [])),
                            tuple(sorted(rule.get("resources") or [])),
                            tuple(sorted(rule.get("verbs") or [])),
                            tuple(sorted(rule.get("resourceNames") or []))))
    expected = [(("",), ("secrets",), ("create", "patch"), ())]
    if sorted(set(granted)) != expected:
        failures.append(
            f"FAIL: the Role(s) bound to {WRITER_SA} grant {sorted(set(granted))}; "
            f"expected exactly {expected} -- create+patch on core secrets and "
            "nothing else.")
    if len(failures) == before:
        print(f"PASS: {WRITER_SA} is bound in {argo_ns!r} to exactly create+patch on secrets")


def check_sa_and_token(docs, failures):
    before = len(failures)
    sas = find(docs, "ServiceAccount", WRITER_SA)
    if not sas:
        failures.append(f"FAIL: no ServiceAccount {WRITER_SA} was rendered")
    else:
        sa = sas[0]
        if ns_of(sa) != RELEASE_NAMESPACE:
            failures.append(f"FAIL: ServiceAccount {WRITER_SA} renders in {ns_of(sa)!r}, not the release namespace")
        if sa.get("automountServiceAccountToken") is not False:
            failures.append(
                f"FAIL: ServiceAccount {WRITER_SA} must set "
                "automountServiceAccountToken: false -- no pod runs as it; its "
                "token reaches the gears only through the mounted token Secret.")
    secrets = find(docs, "Secret", WRITER_TOKEN_SECRET)
    if not secrets:
        failures.append(f"FAIL: no Secret {WRITER_TOKEN_SECRET} was rendered")
    else:
        s = secrets[0]
        if s.get("type") != "kubernetes.io/service-account-token":
            failures.append(f"FAIL: Secret {WRITER_TOKEN_SECRET} has type {s.get('type')!r}, expected kubernetes.io/service-account-token")
        ann = (s["metadata"].get("annotations") or {}).get("kubernetes.io/service-account.name")
        if ann != WRITER_SA:
            failures.append(f"FAIL: Secret {WRITER_TOKEN_SECRET}'s kubernetes.io/service-account.name annotation is {ann!r}, expected {WRITER_SA!r}")
        if ns_of(s) != RELEASE_NAMESPACE:
            failures.append(f"FAIL: Secret {WRITER_TOKEN_SECRET} renders in {ns_of(s)!r}, not the release namespace")
        if s.get("data") or s.get("stringData"):
            failures.append(
                f"FAIL: Secret {WRITER_TOKEN_SECRET} renders data/stringData. The "
                "token controller populates it; a rendered value is a credential "
                "in the manifest and is overwritten on every upgrade.")
    if len(failures) == before:
        print(f"PASS: {WRITER_SA} (no automount) and its token Secret {WRITER_TOKEN_SECRET} render correctly")


def gears_container_mounts(docs):
    dep = next(d for d in docs if d["kind"] == "Deployment" and d["metadata"]["name"] == "qa-platform-gears")
    spec = dep["spec"]["template"]["spec"]
    gears = next(c for c in spec["containers"] if c["name"] == "gears")
    volumes = {v["name"]: v for v in spec.get("volumes") or []}
    return gears.get("volumeMounts") or [], volumes


def configmap_file(docs, path, mounts, volumes):
    """The ConfigMap text the gears container sees at `path`, or None."""
    for m in mounts:
        vol = volumes.get(m["name"], {})
        if "configMap" not in vol:
            continue
        cms = find(docs, "ConfigMap", vol["configMap"]["name"])
        if not cms:
            continue
        data = cms[0].get("data") or {}
        if m.get("subPath"):
            if m["mountPath"] == path and m["subPath"] in data:
                return data[m["subPath"]]
        elif posixpath.dirname(path) == m["mountPath"].rstrip("/"):
            return data.get(posixpath.basename(path))
    return None


def secret_file_mounted(path, mounts, volumes):
    """True when `path` is a file of the writer token Secret in the gears container."""
    for m in mounts:
        vol = volumes.get(m["name"], {})
        sec = vol.get("secret")
        if not sec or sec.get("secretName") != WRITER_TOKEN_SECRET or m.get("subPath"):
            continue
        if posixpath.dirname(path) != m["mountPath"].rstrip("/"):
            continue
        items = sec.get("items")
        names = {i["path"] for i in items} if items else {"token", "ca.crt", "namespace"}
        if posixpath.basename(path) in names:
            return True
    return False


def check_kubeconfig_wiring(docs, failures):
    before = len(failures)
    mounts, volumes = gears_container_mounts(docs)
    cm = find(docs, "ConfigMap", QA_ENV_CONFIGMAP)
    if not cm:
        failures.append(f"FAIL: no ConfigMap {QA_ENV_CONFIGMAP} was rendered")
        return
    fragment = yaml.safe_load(cm[0]["data"][QA_ENV_FRAGMENT_KEY]) or {}
    path = (fragment.get("argo") or {}).get("kubeconfig_path")
    if not path:
        failures.append(
            "FAIL: the qa-environments fragment carries no argo.kubeconfig_path, "
            f"so the D4 writer falls back to Config::infer() -- {GEARS_SA}'s token, "
            "which must not (and after this change does not) hold the Secret grant.")
        return
    text = configmap_file(docs, path, mounts, volumes)
    if text is None:
        failures.append(
            f"FAIL: argo.kubeconfig_path={path!r} is not a file the gears "
            "container mounts from a rendered ConfigMap key.")
        return
    kc = yaml.safe_load(text) or {}
    users = kc.get("users") or []
    clusters = kc.get("clusters") or []
    ctx_name = kc.get("current-context")
    ctx = next((c["context"] for c in kc.get("contexts") or [] if c.get("name") == ctx_name), None)
    if ctx is None:
        failures.append(f"FAIL: the writer kubeconfig's current-context {ctx_name!r} names no context")
        return
    user = next((u.get("user") or {} for u in users if u.get("name") == ctx.get("user")), None)
    cluster = next((c.get("cluster") or {} for c in clusters if c.get("name") == ctx.get("cluster")), None)
    if user is None or cluster is None:
        failures.append("FAIL: the writer kubeconfig's context names a user or cluster it does not define")
        return
    if cluster.get("server") != IN_CLUSTER_SERVER:
        failures.append(
            f"FAIL: the writer kubeconfig's server is {cluster.get('server')!r}, "
            f"expected {IN_CLUSTER_SERVER!r} -- the token is this cluster's.")
    if user.get("token"):
        failures.append("FAIL: the writer kubeconfig carries an inline token; it must read tokenFile")
    for key, value in (("user.tokenFile", user.get("tokenFile")),
                       ("cluster.certificate-authority", cluster.get("certificate-authority"))):
        if not value or not secret_file_mounted(value, mounts, volumes):
            failures.append(
                f"FAIL: the writer kubeconfig's {key}={value!r} is not a file the "
                f"gears container mounts from Secret {WRITER_TOKEN_SECRET}.")
    for m in mounts:
        vol = volumes.get(m["name"], {})
        if (vol.get("secret") or {}).get("secretName") == WRITER_TOKEN_SECRET \
                and vol["secret"].get("optional") is not True:
            failures.append(
                f"FAIL: the {WRITER_TOKEN_SECRET} volume is not optional: true. "
                "With strategy Recreate, deleting that Secret (how its token is "
                "revoked) would hold every gear in ContainerCreating on the next "
                "restart, not just fail the D4 writer.")
    if len(failures) == before:
        print(f"PASS: argo.kubeconfig_path={path} is mounted, its server is in-cluster, and its tokenFile/CA come from an optional {WRITER_TOKEN_SECRET} volume")


def check_qa_runs_stays_on_the_pod_identity(docs, failures):
    dep = next(d for d in docs if d["kind"] == "Deployment" and d["metadata"]["name"] == "qa-platform-gears")
    sa = dep["spec"]["template"]["spec"].get("serviceAccountName")
    if sa != GEARS_SA:
        failures.append(f"FAIL: the gears pod runs as {sa!r}, expected {GEARS_SA!r}")
    cm = find(docs, "ConfigMap", "qa-platform-gears-argo-qa-runs")
    fragment = yaml.safe_load(cm[0]["data"]["qa-runs-argo.yaml"]) if cm else {}
    if "kubeconfig_path" in ((fragment or {}).get("argo") or {}):
        failures.append(
            "FAIL: the qa-runs fragment carries argo.kubeconfig_path. qa-runs "
            f"stays on Config::infer() -- {GEARS_SA}'s projected token -- and "
            "must never be pointed at the secret writer's kubeconfig.")
    if not failures:
        print(f"PASS: the gears pod runs as {GEARS_SA} and qa-runs carries no kubeconfig_path")


def _sanitize(value):
    out = "".join(c if (c.isascii() and c.isalnum()) or c == "-" else "-" for c in value.lower())
    return out.strip("-")


def _truncate(value, limit):
    return value if len(value) <= limit else value[:limit].rstrip("-")


def fragment_prefix(docs):
    cm = find(docs, "ConfigMap", QA_ENV_CONFIGMAP)
    fragment = yaml.safe_load(cm[0]["data"][QA_ENV_FRAGMENT_KEY]) or {}
    return (fragment.get("argo") or {}).get("secret_prefix")


def check_admission_policy(docs, failures):
    before = len(failures)
    if find(docs, "ValidatingAdmissionPolicy", VAP_NAME) or \
            find(docs, "ValidatingAdmissionPolicyBinding", VAP_NAME):
        failures.append(
            f"FAIL: {VAP_NAME} renders although {VAP_API} is not among the "
            "API versions -- an install on a cluster without it would fail.")
    served, stderr = render("--api-versions", VAP_API)
    if served is None:
        failures.append(f"FAIL: the render with --api-versions {VAP_API} failed:\n{stderr}")
        return
    prefix = fragment_prefix(served)
    if not prefix:
        failures.append("FAIL: the qa-environments fragment renders no argo.secret_prefix")
        return
    # Every name `secret_name` derives starts with `prefix` only if sanitise
    # and truncate leave it intact (runner_secret_writer.rs).
    # A sample full input with the shortest possible tenant/reference tail;
    # longer tails only push truncation further right.
    sample = prefix + "00000000-0000-0000-0000-000000000000-x"
    if _sanitize(prefix + "0") != prefix + "0" or len(prefix) >= READABLE_BUDGET \
            or not _truncate(_sanitize(sample), READABLE_BUDGET).startswith(prefix):
        failures.append(
            f"FAIL: argo.secret_prefix {prefix!r} does not survive secret_name's "
            f"sanitise/truncate (lower-case [a-z0-9-], no leading '-', under "
            f"{READABLE_BUDGET} bytes), so some legitimate names would not start "
            "with it and the admission policy would deny the writer's own writes.")
    vaps = find(served, "ValidatingAdmissionPolicy", VAP_NAME)
    bindings = find(served, "ValidatingAdmissionPolicyBinding", VAP_NAME)
    if not vaps or not bindings:
        failures.append(f"FAIL: with {VAP_API} served, {VAP_NAME} (policy and binding) must render")
        return
    spec = vaps[0]["spec"]
    argo_ns = argo_namespace()
    ns_sel = {"matchLabels": {"kubernetes.io/metadata.name": argo_ns}}
    expected_rules = [{"apiGroups": [""], "apiVersions": ["v1"],
                       "operations": ["CREATE", "UPDATE"], "resources": ["secrets"]}]
    mc = spec.get("matchConstraints") or {}
    if mc.get("namespaceSelector") != ns_sel or mc.get("resourceRules") != expected_rules:
        failures.append(
            f"FAIL: {VAP_NAME} must match exactly CREATE/UPDATE of core v1 secrets "
            f"in {argo_ns!r}; got {mc!r}")
    expected_match = [f'request.userInfo.username == "system:serviceaccount:{RELEASE_NAMESPACE}:{WRITER_SA}"']
    if [c.get("expression") for c in spec.get("matchConditions") or []] != expected_match:
        failures.append(f"FAIL: {VAP_NAME}'s matchConditions must be exactly {expected_match}")
    expected_validations = ['!has(object.type) || object.type == "Opaque"',
                            f'object.metadata.name.startsWith("{prefix}")']
    if [v.get("expression") for v in spec.get("validations") or []] != expected_validations:
        failures.append(f"FAIL: {VAP_NAME}'s validations must be exactly {expected_validations}")
    if spec.get("failurePolicy") != "Fail":
        failures.append(f"FAIL: {VAP_NAME} must set failurePolicy: Fail")
    b = bindings[0]["spec"]
    if b.get("policyName") != VAP_NAME or b.get("validationActions") != ["Deny"] \
            or (b.get("matchResources") or {}).get("namespaceSelector") != ns_sel:
        failures.append(f"FAIL: binding {VAP_NAME} must name the policy, Deny, and select {argo_ns!r}; got {b!r}")
    if len(failures) == before:
        print(f"PASS: {VAP_NAME} renders only where {VAP_API} is served, and restricts {WRITER_SA} to Opaque Secrets named {prefix}*")


def check_same_namespace_refused(docs, failures):
    # The admission policy admits the writer to every Opaque Secret named
    # with the runner prefix, and every chart-owned Secret in the release
    # namespace carries that prefix too. The two namespaces must differ.
    refused, stderr = render("--set", f"argo.namespace={RELEASE_NAMESPACE}")
    if refused is not None:
        failures.append(
            "FAIL: the chart must refuse argo.namespace equal to the release "
            "namespace -- the secret writer could then overwrite the chart's "
            "own qa-platform-* Secrets")
    elif "argo.namespace" not in stderr:
        failures.append(f"FAIL: the refusal must name argo.namespace; got:\n{stderr}")
    else:
        print("PASS: argo.namespace equal to the release namespace is refused")


def main():
    docs, stderr = render()
    if docs is None:
        print(f"FAIL: the render must succeed:\n{stderr}")
        return 1
    failures = []
    for check in (check_gears_sa_has_no_secrets, check_writer_role,
                  check_sa_and_token, check_kubeconfig_wiring,
                  check_qa_runs_stays_on_the_pod_identity, check_admission_policy,
                  check_same_namespace_refused):
        mine = []
        check(docs, mine)
        failures += mine
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
