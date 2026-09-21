"""The runner pod carries NO IdP credential, and the bundle route's signing
secret is never the committed dev literal.

# What this guard is for

`GET /qa/v1/test-bundles/{id}` used to be `.authenticated()`, and a runner pod
answered that by carrying `TEST_BUNDLE_CLIENT_SECRET` -- the confidential secret
of the `qa-platform-workflow` Keycloak client, which is `fullScopeAllowed` and
has a `tenant_id` claim hardcoded to `seedTenantId` -- and exchanging it for a
bearer token from inside the pod. A runner pod's entire job is to execute
TENANT-AUTHORED pytest, so that credential was readable by code the platform
does not own, and every authenticated route in all four gears was reachable with
it. The NetworkPolicy could not mitigate it: it deliberately allow-listed both
Keycloak and the gears API, because the exchange needed both. Separately, the
hardcoded claim was a live multi-tenancy bug -- every tenant but the seeded one
got a 404 on its own bundles.

The route is anonymous now, authorised by a per-bundle HMAC tag qa-catalog mints
and qa-runs renders into `TEST_BUNDLE_URL`'s `?sig=`.

**That was a replacement, not an addition, and this guard is what keeps it
one.** A partial revert -- restoring the credential while the signature stays --
would put the deployment back in the state the signature was built to end, and
nothing else in the tree would notice: the token would keep working, every test
would keep passing, and the credential would simply be back in the pod. So the
three env-var names, the Kubernetes Secret and the Helm values that carried them
are all asserted ABSENT, from the rendered chart and from the runner image's own
files alike.

`tests/check_runner_networkpolicy.py::check_keycloak_egress_is_gone` is the
third side of the same assertion (the egress path), and the Rust side is
`workflow.rs::no_rendered_workflow_carries_a_bundle_oidc_credential` (the
rendered Argo workflow). Each covers a surface the others cannot see.

# And the secret that replaced it must not ship as its committed literal

`bundle_download_signing_secret` is the whole access control on an anonymously
reachable route. `gears-config-configmap.yaml` rewrites the committed dev value
and `fail`s the render if it stops appearing in the config file -- but nothing
asserted the OUTPUT. This does, which is the half that matters to a cluster.
"""
import ast
import pathlib
import subprocess
import sys

import yaml

HERE = pathlib.Path(__file__).resolve().parent
CHART = HERE.parent / "qa-platform"
RUNNER = HERE.parent.parent / "runner"

# The three environment variables the Argo adapter used to push onto every
# runner container, plus the grant they were for. None may appear anywhere in a
# rendered chart or in the runner image's own files.
BANNED_IN_RENDER = [
    "TEST_BUNDLE_TOKEN_URL",
    "TEST_BUNDLE_CLIENT_ID",
    "TEST_BUNDLE_CLIENT_SECRET",
    # The Secret `workflow-oidc-secret.yaml` created in Argo's namespace.
    "qa-platform-workflow-oidc",
]

# The values that fed them. `argo.workflowClientSecret` is NOT here: the realm
# still pins it, because `deploy/remote/verify-k8s.sh` check 16 needs a
# non-browser token to prove the product-plugin catalogue is non-empty on a
# stand and the realm's only other client has direct grants disabled. That
# credential lives in the realm and in this chart; it no longer reaches a pod
# that runs tenant code, which is the property this guard is about.
BANNED_VALUES = [
    "workflowSecretName",
    "workflowSecretKey",
    "workflowClientId",
    "createWorkflowSecret",
]

# The committed dev literal `gears-config-configmap.yaml` must rewrite. Kept in
# sync with `config/qa-platform-stack.yaml` by that template's own `fail` guard.
DEV_SIGNING_LITERAL = "dev-bundle-download-signing-secret"


def render():
    """`helm template` the chart with the minimum a default install needs."""
    proc = subprocess.run(
        [
            "helm", "template", "guard", str(CHART),
            "--set", "publicOrigin=https://guard.example",
            # keycloak.adminPassword has no default (WS3 Task 3) -- any value
            # renders; this one is obviously not a real credential.
            "--set", "keycloak.adminPassword=guard-fixture-not-a-real-password",
            # Both signing secrets have no default either (2026-09-21): the
            # per-render `randAlphaNum` fallback became a pod roll on every
            # upgrade once the gears Deployment started hashing the ConfigMap.
            "--set", "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key",
            "--set", "collectReportSigningSecret=guard-fixture-not-a-real-collect-key",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.stdout, proc.stderr, proc.returncode


def _docstring_lines(source):
    """Every 1-based line number covered by a module/class/function docstring.

    Docstrings are the one place the deleted credential's NAMES are allowed to
    survive in the runner image -- the files explain at length what was removed
    and why, which is what stops the next reader restoring it. Every other
    string literal stays in scope, so `os.environ["TEST_BUNDLE_CLIENT_SECRET"]`
    is still caught."""
    lines = set()
    tree = ast.parse(source)
    for node in ast.walk(tree):
        if not isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef,
                                 ast.AsyncFunctionDef)):
            continue
        body = getattr(node, "body", [])
        if not body:
            continue
        first = body[0]
        if isinstance(first, ast.Expr) and isinstance(first.value, ast.Constant) \
                and isinstance(first.value.value, str):
            lines.update(range(first.lineno, (first.end_lineno or first.lineno) + 1))
    return lines


def check_render_carries_no_credential(failures):
    out, err, rc = render()
    if rc != 0:
        failures.append(f"FAIL: helm template failed:\n{err}")
        return

    for banned in BANNED_IN_RENDER:
        if banned in out:
            failures.append(
                f"FAIL: {banned!r} appears in the rendered chart. A runner pod "
                "must carry no IdP credential: it executes tenant-authored "
                "pytest, and the qa-platform-workflow client is "
                "fullScopeAllowed with a hardcoded tenant_id claim. The "
                "bundle route is anonymous and signature-authorised -- see "
                "this guard's header for why restoring the credential "
                "alongside the signature is the failure mode, not a "
                "belt-and-braces improvement.")
    if not failures:
        print(
            "PASS: no rendered object carries a bundle-download OIDC "
            f"credential ({', '.join(BANNED_IN_RENDER)} all absent)")


def check_no_workflow_secret_object(failures):
    """Belt to the string check's braces: the Secret is asserted gone as an
    OBJECT too, so a rename could not slip it past the literal above."""
    out, _, rc = render()
    if rc != 0:
        return

    for doc in yaml.safe_load_all(out):
        if not isinstance(doc, dict):
            continue
        if doc.get("kind") != "Secret":
            continue
        meta = doc.get("metadata", {})
        component = meta.get("labels", {}).get("app.kubernetes.io/component")
        if component == "argo-workflow-oidc":
            failures.append(
                "FAIL: a Secret labelled app.kubernetes.io/component="
                "argo-workflow-oidc still renders "
                f"({meta.get('namespace')}/{meta.get('name')}). That object "
                "existed only to put a confidential client secret into every "
                "runner pod's environment.")
            return

    print("PASS: no argo-workflow-oidc Secret renders")


def check_values_no_longer_declare_the_credential(failures):
    values = (CHART / "values.yaml").read_text()
    for key in BANNED_VALUES:
        # `key:` at the start of a (indented) line -- a mention inside the
        # explanatory comment block that records WHY these are gone must not
        # trip this.
        for line in values.splitlines():
            stripped = line.strip()
            if stripped.startswith("#"):
                continue
            if stripped.startswith(f"{key}:"):
                failures.append(
                    f"FAIL: values.yaml still declares argo.{key}. It fed the "
                    "runner pod's OIDC credential, which is gone; leaving the "
                    "knob invites a deployment to set it and a future template "
                    "to read it.")
                break
    if not failures:
        print(
            "PASS: values.yaml declares none of "
            f"{', '.join(BANNED_VALUES)}")


def check_runner_image_has_no_token_exchange(failures):
    """The image's own files, which `helm template` cannot see.

    `entrypoint.sh` and `fetch_bundle.py` are baked into the runner image and
    are the two places the credential was actually consumed. A chart that no
    longer supplies it plus a script that still demands it is a pod that dies
    at `required('TEST_BUNDLE_CLIENT_SECRET')` -- so these must move together."""
    for name in ("entrypoint.sh", "fetch_bundle.py"):
        path = RUNNER / name
        if not path.is_file():
            failures.append(f"FAIL: {path} does not exist")
            continue
        text = path.read_text()
        # Prose is exempt, code is not. Both files DOCUMENT the deleted
        # credential at length, and that prose is the point -- it is what stops
        # the next reader re-adding it. So `#` comments are skipped, and for
        # the Python file so are DOCSTRINGS ONLY: an ordinary string literal
        # stays in scope, because `required("TEST_BUNDLE_CLIENT_SECRET")` is
        # exactly the line this check exists to catch and it is a string.
        skip = _docstring_lines(text) if name.endswith(".py") else set()
        for number, line in enumerate(text.splitlines(), start=1):
            stripped = line.strip()
            if stripped.startswith("#") or number in skip:
                continue
            for banned in ("TEST_BUNDLE_CLIENT_SECRET", "TEST_BUNDLE_TOKEN_URL",
                           "TEST_BUNDLE_CLIENT_ID", "client_credentials"):
                if banned in stripped:
                    failures.append(
                        f"FAIL: {name}:{number} still uses {banned!r}: "
                        f"{stripped!r}. The runner image must not perform a "
                        "token exchange; the `?sig=` already on "
                        "TEST_BUNDLE_URL is the credential.")
    if not failures:
        print(
            "PASS: neither entrypoint.sh nor fetch_bundle.py performs a token "
            "exchange")


def check_the_url_is_never_logged_whole(failures):
    """`TEST_BUNDLE_URL` carries the signature, and both files echo it.

    The run view renders the pod log, so a URL printed verbatim publishes the
    tag to everyone who can read that run. Each file must strip the query
    string before printing -- `entrypoint.sh` with a `%%?*` expansion, and
    `fetch_bundle.py` through its `redacted()` helper."""
    entrypoint = (RUNNER / "entrypoint.sh").read_text()
    if "${TEST_BUNDLE_URL%%" not in entrypoint:
        failures.append(
            "FAIL: entrypoint.sh does not strip TEST_BUNDLE_URL's query string "
            "before echoing it. The `?sig=` is the access control on the "
            "bundle route and this line is rendered in the run view.")
    for number, line in enumerate(entrypoint.splitlines(), start=1):
        stripped = line.strip()
        if stripped.startswith("#"):
            continue
        if stripped.startswith("echo") and "${TEST_BUNDLE_URL" in stripped:
            failures.append(
                f"FAIL: entrypoint.sh:{number} echoes TEST_BUNDLE_URL "
                f"directly: {stripped!r}. Echo the stripped copy instead.")

    fetch = (RUNNER / "fetch_bundle.py").read_text()
    if "def redacted(url):" not in fetch:
        failures.append(
            "FAIL: fetch_bundle.py has no redacted() helper; every print and "
            "die() that names the URL must go through one.")
    for number, line in enumerate(fetch.splitlines(), start=1):
        stripped = line.strip()
        if stripped.startswith("#"):
            continue
        # `% (url` / `% url` as a formatting argument, outside redacted().
        if ("% (url" in stripped or stripped.endswith("% url")) and "redacted" not in stripped:
            failures.append(
                f"FAIL: fetch_bundle.py:{number} formats the raw url into a "
                f"message: {stripped!r}. Wrap it in redacted().")

    if not failures:
        print("PASS: neither file can print TEST_BUNDLE_URL's query string")


def check_signing_secret_is_rewritten(failures):
    """The committed dev literal must never reach a rendered cluster.

    `gears-config-configmap.yaml` fails the render if the literal stops
    appearing in the SOURCE config; this asserts the other half -- that it does
    not appear in the OUTPUT."""
    out, _, rc = render()
    if rc != 0:
        return

    if DEV_SIGNING_LITERAL in out:
        failures.append(
            f"FAIL: the committed dev literal {DEV_SIGNING_LITERAL!r} appears "
            "in the rendered chart. It is the only access control on an "
            "anonymously reachable route: anyone who knew it could forge a tag "
            "for any bundle. gears-config-configmap.yaml's fourth transform is "
            "what must rewrite it.")
        return

    if "bundle_download_signing_secret:" not in out:
        failures.append(
            "FAIL: no rendered config carries bundle_download_signing_secret "
            "at all. qa-catalog fails every bundle download closed without it, "
            "so the deployment would run no tests and report nothing but 403s "
            "in workflow pod logs.")
        return

    print(
        "PASS: bundle_download_signing_secret renders, and not as the "
        "committed dev literal")


def main():
    failures = []
    check_render_carries_no_credential(failures)
    check_no_workflow_secret_object(failures)
    check_values_no_longer_declare_the_credential(failures)
    check_runner_image_has_no_token_exchange(failures)
    check_the_url_is_never_logged_whole(failures)
    check_signing_secret_is_rewritten(failures)
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
