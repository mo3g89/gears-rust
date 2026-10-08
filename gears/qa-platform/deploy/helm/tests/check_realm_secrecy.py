"""The realm no longer ships as a ConfigMap, and the admin password no
longer has a default.

# The trap this guard exists to catch

`keycloak-realm-secret.yaml` (a ConfigMap until this task) carries
`realm-qa-platform.json`, which pins the `qa-platform-workflow` client's
confidential secret (`argo.workflowClientSecret`) so the realm's client secret is a
known value across a restart (a Secret template that once read it back into
Argo's namespace is deleted). A ConfigMap is
readable by anything in the namespace that can read ConfigMaps -- far
wider than the RBAC most clusters put on Secrets -- so rendering that same
value into a ConfigMap defeated the one place this chart already treats it
as secret material.

Separately, `keycloak.adminPassword` used to default to the well-known
value `admin`, which is also its username: an install nobody thought to
override a default value on published the master realm's admin console
behind `admin`/`admin`.

A third trap, and the reason this guard grew on 2026-09-29:
`argo.workflowClientSecret` was the bare committed literal
`qa-platform-workflow-dev-secret`, with no `required` and no gate. The
`qa-platform-workflow` client sits OUTSIDE the realm file's `{{if devMode}}`
block -- that block wraps the seeded `users` array and nothing else -- and
the client is `serviceAccountsEnabled` and `fullScopeAllowed`, so a
client-credentials token minted with that string carries full tenant access.
The string is published in this repository and was hardcoded a second time in
`deploy/remote/verify-k8s.sh`. `runner-networkpolicy.yaml`'s rule 3 opens
0.0.0.0/0 minus the cluster CIDRs, so reachability was never what kept it
shut. The value is `required` now, and the committed literal is refused
unless `devMode=true` -- the same two guards, in the same shape,
`keycloak.adminPassword` already had.

A fourth trap this guard also catches: the realm file unconditionally
seeded two APPLICATION users -- `admin`/`AdminPass1!` and
`viewer`/`ViewerPass1!` -- `enabled: true`, `temporary: false`, with no
gate at all, on every single install, dev stand or not. That is a
public-literal password in a git repository landing on a real user
account by default -- the same shape of problem as the old
`keycloak.adminPassword` default, just one level up (the realm's own
users rather than Keycloak's bootstrap admin). Those two users are now
wrapped in `{{if .Values.devMode}}...{{end}}` in
`files/keycloak/realm-qa-platform.json`, so they only render at all when
the installer has explicitly opted into a throwaway stand.

# What this checks

0. `argo.workflowClientSecret` unset fails the render, naming the value;
   the committed literal `qa-platform-workflow-dev-secret` without `devMode`
   fails and the message cites `devMode`; and the SAME render with
   `devMode=true` succeeds, so the escape hatch is proven to work rather
   than only the refusal.
1. No rendered ConfigMap contains the workflow client's secret value
   (default and an overridden one), and the object named
   `qa-platform-realm` is a Secret, not a ConfigMap, carrying the realm
   under `stringData["realm-qa-platform.json"]`.
2. Rendering with `keycloak.adminPassword` unset (the chart's own default,
   now empty) fails outright -- `required` with no default, not a fixture
   value.
3. Rendering with `keycloak.adminPassword=admin` (the historical fixture
   value) and no `devMode` set fails, citing `devMode` in the message so a
   reader knows the escape hatch exists.
4. The SAME render (`adminPassword=admin`, `devMode=true`) succeeds -- the
   escape hatch must actually work, not just the refusal.
5. A normal render (a generated, non-default password, `devMode` unset)
   succeeds, and on it: the public `qa-platform-ui` client has
   `directAccessGrantsEnabled: false`; the realm has
   `bruteForceProtected: true`, a non-empty `passwordPolicy`, and both
   `ssoSessionIdleTimeout` and `ssoSessionMaxLifespan` set.
6. `devMode` unset (the normal render from #5) seeds NO users at all --
   the realm's `users` array is empty, so the fixture `admin`/`AdminPass1!`
   and `viewer`/`ViewerPass1!` logins do not exist on a real install. The
   SAME render with `devMode=true` seeds exactly those two usernames --
   the escape hatch must still give a usable dev login.
7. `argo.workflowClientSecret` shorter than 16 characters once trimmed is
   refused in devMode and outside it, naming "at least 16"; 16 renders.
8. A `keycloakRealmJson` supplied with `--set-file` is refused when it
   carries the dev literal outside devMode -- in its raw text or, behind a
   JSON escape, as a client's decoded secret -- (naming keycloakRealmJson and
   devMode) or when its qa-platform-workflow client's secret is under 16
   characters; a well-formed one, and one with no workflow client at all,
   render verbatim.

Standalone script, like its siblings in this directory -- see the
Makefile's `helm-tests` target for why each one is invoked directly rather
than through pytest collection."""
import json
import pathlib
import subprocess
import sys
import tempfile

import yaml

TESTS_DIR = pathlib.Path(__file__).resolve().parent
CHART = TESTS_DIR.parent / "qa-platform"
ORIGIN = "https://example-realm-secrecy-test.invalid"
RELEASE = "realm-secrecy-test"
GENERATED_PASSWORD = "not-the-default-Tr0ub4dor&3"  # nosec: test fixture only
# The committed dev literal, refused outside devMode since 2026-09-29.
DEFAULT_WORKFLOW_SECRET = "qa-platform-workflow-dev-secret"
GENERATED_WORKFLOW_SECRET = "not-the-literal-9d2f4a7c"  # nosec: test fixture only
BUNDLE_SIGNING_KEY = "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key"  # nosec: test fixture only
COLLECT_SIGNING_KEY = "collectReportSigningSecret=guard-fixture-not-a-real-collect-key"  # nosec: test fixture only
POSTGRES_PASSWORD = "postgres.password=guard-fixture-not-a-real-db-password"  # nosec: test fixture only


def render(extra_sets=None, workflow=GENERATED_WORKFLOW_SECRET, literals=None,
           files=None):
    """Return (docs, stdout, stderr, returncode). Never asserts -- callers
    decide."""
    cmd = ["helm", "template", RELEASE, str(CHART),
           "--namespace", "qa-platform", "--set", f"publicOrigin={ORIGIN}",
           # The two signing secrets have no default (2026-09-21) and are not
           # what any check here is about -- they go in the BASE command so
           # that check_admin_password_unset_fails below still fails on the
           # one value it is testing, and names it.
           "--set", BUNDLE_SIGNING_KEY,
           "--set", COLLECT_SIGNING_KEY, "--set", POSTGRES_PASSWORD]
    # argo.workflowClientSecret has no default either (2026-09-29) and is the
    # subject of three checks below, so unlike the two signing secrets it is
    # OMITTABLE: a caller passes `workflow=None` to leave it out and test the
    # `required` itself. Everything else gets a generated non-literal value,
    # for the same reason the signing secrets are in the base command.
    if workflow is not None:
        cmd += ["--set", f"argo.workflowClientSecret={workflow}"]
    for s in extra_sets or []:
        cmd += ["--set", s]
    # `--set` reads `\` as an escape and drops it, so a value whose point is
    # to CARRY a backslash goes through `--set-literal`, which passes it as-is.
    for s in literals or []:
        cmd += ["--set-literal", s]
    # `--set-file` is how an operator supplies keycloakRealmJson, so the guard
    # supplies it the same way.
    for s in files or []:
        cmd += ["--set-file", s]
    out = subprocess.run(cmd, capture_output=True, text=True)
    if out.returncode != 0:
        return [], out.stdout, out.stderr, out.returncode
    docs = [d for d in yaml.safe_load_all(out.stdout) if d]
    return docs, out.stdout, out.stderr, 0


def check_no_configmap_carries_the_secret(failures):
    docs, _, stderr, rc = render(
        [f"keycloak.adminPassword={GENERATED_PASSWORD}"])
    if rc != 0:
        failures.append(
            f"FAIL: a render with an explicit non-default adminPassword "
            f"must succeed:\n{stderr}")
        return

    configmaps = [d for d in docs if d.get("kind") == "ConfigMap"]
    for cm in configmaps:
        dumped = yaml.dump(cm)
        if DEFAULT_WORKFLOW_SECRET in dumped:
            failures.append(
                f"FAIL: ConfigMap {cm['metadata']['name']!r} contains the "
                "workflow client's secret value. Secret material must "
                "never render into a ConfigMap.")
        if '"secret":' in dumped or "clientSecret" in dumped:
            failures.append(
                f"FAIL: ConfigMap {cm['metadata']['name']!r} looks like it "
                "carries client-secret material.")

    realms = [d for d in docs
              if d.get("metadata", {}).get("name") == "qa-platform-realm"]
    if len(realms) != 1:
        failures.append(
            "FAIL: expected exactly one object named qa-platform-realm, "
            f"found {len(realms)}.")
        return
    realm_obj = realms[0]
    if realm_obj.get("kind") != "Secret":
        failures.append(
            f"FAIL: qa-platform-realm is a {realm_obj.get('kind')!r}, not "
            "a Secret. The realm carries the workflow client's confidential "
            "secret and must render as a Secret.")
        return
    string_data = realm_obj.get("stringData", {})
    if "realm-qa-platform.json" not in string_data:
        failures.append(
            "FAIL: the qa-platform-realm Secret has no "
            "stringData['realm-qa-platform.json'] key.")
        return
    if realm_obj.get("data"):
        failures.append(
            "FAIL: the qa-platform-realm Secret carries a `data:` block on "
            "top of `stringData:` -- keep the realm in stringData only, "
            "the plaintext form `tpl` already produces.")


def check_admin_password_unset_fails(failures):
    _, _, stderr, rc = render()
    if rc == 0:
        failures.append(
            "FAIL: rendering with no keycloak.adminPassword set succeeded. "
            "The chart ships no default password any more -- this must be "
            "refused.")
        return
    if "keycloak.adminPassword" not in stderr:
        failures.append(
            "FAIL: rendering with adminPassword unset failed (good), but "
            f"the message does not name keycloak.adminPassword:\n{stderr}")


def check_default_password_without_devmode_fails(failures):
    _, _, stderr, rc = render(["keycloak.adminPassword=admin"])
    if rc == 0:
        failures.append(
            "FAIL: rendering with keycloak.adminPassword=admin and no "
            "devMode succeeded. The well-known fixture password must be "
            "refused outside devMode.")
        return
    if "devMode" not in stderr:
        failures.append(
            "FAIL: rendering with the default password failed (good), but "
            f"the message does not mention devMode:\n{stderr}")


def check_default_password_with_devmode_succeeds(failures):
    _, _, stderr, rc = render(
        ["keycloak.adminPassword=admin", "devMode=true"])
    if rc != 0:
        failures.append(
            "FAIL: rendering with keycloak.adminPassword=admin and "
            f"devMode=true must succeed (the escape hatch exists for "
            f"throwaway stands):\n{stderr}")


def check_workflow_secret_unset_fails(failures):
    """No default at all -- the same treatment keycloak.adminPassword got.

    It shipped as the bare literal `qa-platform-workflow-dev-secret` until
    2026-09-29. The client it belongs to is serviceAccountsEnabled and
    fullScopeAllowed, so that string was a full-tenant credential published in
    a git repository.

    RENDERED WITH `devMode=true`, WHICH IS THE WHOLE POINT OF THIS SHAPE. With
    devMode unset, a values.yaml that put the literal back as the default would
    still make this render FAIL -- on the devMode refusal below, whose message
    also names `argo.workflowClientSecret` -- and this check would pass while
    the default it exists to forbid was sitting in the chart. Measured: that is
    exactly what it did before this rewrite. Under `devMode=true` the literal
    is an acceptable value, so a render that still fails can only be failing on
    `required`, and a default of any kind makes it succeed and this check
    report it."""
    _, _, stderr, rc = render(
        [f"keycloak.adminPassword={GENERATED_PASSWORD}", "devMode=true"],
        workflow=None)
    if rc == 0:
        failures.append(
            "FAIL: rendering with no argo.workflowClientSecret set succeeded. "
            "The chart ships no default for it any more -- values.yaml must "
            "carry an empty string and keycloak-realm-secret.yaml's `required` "
            "must refuse the render.")
        return
    if "argo.workflowClientSecret is required" not in stderr:
        failures.append(
            "FAIL: rendering with argo.workflowClientSecret unset failed "
            "(good), but not on its `required` -- the message does not carry "
            f"'argo.workflowClientSecret is required':\n{stderr}")


def check_dev_workflow_secret_without_devmode_fails(failures):
    _, _, stderr, rc = render(
        [f"keycloak.adminPassword={GENERATED_PASSWORD}"],
        workflow=DEFAULT_WORKFLOW_SECRET)
    if rc == 0:
        failures.append(
            "FAIL: rendering with argo.workflowClientSecret="
            f"{DEFAULT_WORKFLOW_SECRET} and no devMode succeeded. That literal "
            "is committed in this chart and in deploy/remote/verify-k8s.sh, and "
            "the qa-platform-workflow client is serviceAccountsEnabled and "
            "fullScopeAllowed -- it must be refused outside devMode.")
        return
    if "devMode" not in stderr:
        failures.append(
            "FAIL: rendering with the committed workflow literal failed "
            f"(good), but the message does not mention devMode:\n{stderr}")


def check_dev_workflow_secret_with_devmode_succeeds(failures):
    """The escape hatch has to work, not just the refusal.

    deploy/remote/deploy-k8s.sh installs throwaway stands with devMode=true,
    and a refusal with no way past it would be a chart nobody can install on
    one."""
    _, _, stderr, rc = render(
        [f"keycloak.adminPassword={GENERATED_PASSWORD}", "devMode=true"],
        workflow=DEFAULT_WORKFLOW_SECRET)
    if rc != 0:
        failures.append(
            "FAIL: rendering with the committed workflow literal and "
            f"devMode=true must succeed (the escape hatch exists for "
            f"throwaway stands):\n{stderr}")


def check_realm_hardening(failures):
    docs, _, stderr, rc = render(
        [f"keycloak.adminPassword={GENERATED_PASSWORD}"])
    if rc != 0:
        failures.append(f"FAIL: a normal render must succeed:\n{stderr}")
        return

    realms = [d for d in docs
              if d.get("metadata", {}).get("name") == "qa-platform-realm"
              and d.get("kind") == "Secret"]
    if len(realms) != 1:
        failures.append(
            "FAIL: expected exactly one qa-platform-realm Secret, found "
            f"{len(realms)}.")
        return
    realm_json = realms[0]["stringData"]["realm-qa-platform.json"]
    realm = json.loads(realm_json)

    ui_clients = [c for c in realm.get("clients", [])
                  if c.get("clientId") == "qa-platform-ui"]
    if len(ui_clients) != 1:
        failures.append(
            "FAIL: expected exactly one qa-platform-ui client in the "
            f"rendered realm, found {len(ui_clients)}.")
    elif ui_clients[0].get("directAccessGrantsEnabled") is not False:
        failures.append(
            "FAIL: the public qa-platform-ui client must have "
            "directAccessGrantsEnabled: false -- a public client accepting "
            "a resource-owner-password grant skips every browser-flow "
            "protection (PKCE, redirect URI, origin check).")

    if realm.get("bruteForceProtected") is not True:
        failures.append(
            "FAIL: the realm must have bruteForceProtected: true.")
    if not realm.get("passwordPolicy"):
        failures.append(
            "FAIL: the realm must carry a non-empty passwordPolicy.")
    if not realm.get("ssoSessionIdleTimeout"):
        failures.append(
            "FAIL: the realm must carry ssoSessionIdleTimeout.")
    if not realm.get("ssoSessionMaxLifespan"):
        failures.append(
            "FAIL: the realm must carry ssoSessionMaxLifespan.")


def _realm_from(docs):
    realms = [d for d in docs
              if d.get("metadata", {}).get("name") == "qa-platform-realm"
              and d.get("kind") == "Secret"]
    if len(realms) != 1:
        return None
    return json.loads(realms[0]["stringData"]["realm-qa-platform.json"])


def check_seed_users_gated_by_devmode(failures):
    docs, _, stderr, rc = render([f"keycloak.adminPassword={GENERATED_PASSWORD}"])
    if rc != 0:
        failures.append(
            f"FAIL: a normal render (devMode unset) must succeed:\n{stderr}")
        return
    realm = _realm_from(docs)
    if realm is None:
        failures.append(
            "FAIL: could not find the qa-platform-realm Secret to check "
            "seeded users without devMode.")
    elif realm.get("users"):
        usernames = [u.get("username") for u in realm["users"]]
        failures.append(
            "FAIL: without devMode, the realm must seed no users at all -- "
            f"found {usernames}. The fixture admin/AdminPass1! and "
            "viewer/ViewerPass1! application users must not exist on a "
            "real install.")

    docs_dev, _, stderr_dev, rc_dev = render(
        [f"keycloak.adminPassword={GENERATED_PASSWORD}", "devMode=true"])
    if rc_dev != 0:
        failures.append(
            f"FAIL: a render with devMode=true must succeed:\n{stderr_dev}")
        return
    realm_dev = _realm_from(docs_dev)
    if realm_dev is None:
        failures.append(
            "FAIL: could not find the qa-platform-realm Secret to check "
            "seeded users with devMode=true.")
        return
    usernames_dev = {u.get("username") for u in realm_dev.get("users", [])}
    if usernames_dev != {"admin", "viewer"}:
        failures.append(
            "FAIL: with devMode=true, the realm must seed exactly the "
            f"admin and viewer fixture users -- found {usernames_dev}. "
            "The dev-stand escape hatch must still give a usable login.")


# Characters that end a JSON string or start an escape; a bare interpolation
# of any of them renders a realm Keycloak cannot import.
HOSTILE_WORKFLOW_SECRET = 'wf"quote\\back\\slash'  # nosec: test fixture only
HOSTILE_ORIGIN = 'https://ex"ample\\host.invalid'
HOSTILE_TENANT = 'tenant"quote\\back'
# 15 and 16 characters: the floor is 16 once trimmed, the same floor
# qa-catalog and qa-insights apply to their signing secrets.
SHORT_WORKFLOW_SECRET = "fifteen-chars-x"  # nosec: test fixture only
FLOOR_WORKFLOW_SECRET = "sixteen-chars-xx"  # nosec: test fixture only


def check_substituted_values_render_json_safe(failures):
    docs, _, stderr, rc = render(
        [f"keycloak.adminPassword={GENERATED_PASSWORD}", "devMode=true"],
        workflow=None,
        literals=[f"argo.workflowClientSecret={HOSTILE_WORKFLOW_SECRET}",
                  f"publicOrigin={HOSTILE_ORIGIN}",
                  f"seedTenantId={HOSTILE_TENANT}"])
    if rc != 0:
        failures.append(
            "FAIL: a render whose substituted values carry `\"` and `\\` "
            f"must succeed:\n{stderr}")
        return
    realms = [d for d in docs
              if d.get("metadata", {}).get("name") == "qa-platform-realm"
              and d.get("kind") == "Secret"]
    if len(realms) != 1:
        failures.append("FAIL: expected exactly one qa-platform-realm Secret.")
        return
    try:
        realm = json.loads(realms[0]["stringData"]["realm-qa-platform.json"])
    except json.JSONDecodeError as e:
        failures.append(
            "FAIL: the rendered realm is not valid JSON when a substituted "
            "value carries `\"` or `\\` -- interpolate with `toJson`, not "
            f"inside a literal \"...\": {e}")
        return
    wf = [c for c in realm.get("clients", [])
          if c.get("clientId") == "qa-platform-workflow"]
    if not wf or wf[0].get("secret") != HOSTILE_WORKFLOW_SECRET:
        failures.append(
            "FAIL: the workflow client's secret did not round-trip through "
            "the realm JSON unchanged.")
    ui = [c for c in realm.get("clients", [])
          if c.get("clientId") == "qa-platform-ui"]
    if not ui or f"{HOSTILE_ORIGIN}/*" not in ui[0].get("redirectUris", []) \
            or HOSTILE_ORIGIN not in ui[0].get("webOrigins", []):
        failures.append(
            "FAIL: publicOrigin did not round-trip into the UI client's "
            "redirectUris/webOrigins unchanged.")
    tenants = [u.get("attributes", {}).get("tenant_id") for u in realm.get("users", [])]
    if not tenants or any(t != [HOSTILE_TENANT] for t in tenants):
        failures.append(
            "FAIL: seedTenantId did not round-trip into every seeded user's "
            f"tenant_id attribute unchanged: {tenants}")
    claims = [m.get("config", {}).get("claim.value")
              for m in (wf[0].get("protocolMappers", []) if wf else [])
              if m.get("config", {}).get("claim.name") == "tenant_id"]
    if claims != [HOSTILE_TENANT]:
        failures.append(
            "FAIL: seedTenantId did not round-trip into the workflow client's "
            "tenant_id claim mapper.")


def check_numeric_values_stay_json_strings(failures):
    """`--set` types `42` as a number, and `toJson` of a number is `42`, not
    `"42"` -- a realm whose `secret` or `tenant_id` is a JSON number is not the
    string Keycloak and oidc-authn-plugin expect. Each placeholder is
    `toString | toJson`, so a numeric-looking value still renders a string."""
    docs, _, stderr, rc = render(
        [f"keycloak.adminPassword={GENERATED_PASSWORD}", "devMode=true",
         "seedTenantId=42"],
        workflow="1234567890123456")
    if rc != 0:
        failures.append(f"FAIL: a render with numeric-looking values must succeed:\n{stderr}")
        return
    realm = _realm_from(docs)
    if realm is None:
        failures.append("FAIL: no qa-platform-realm Secret in the numeric-value render.")
        return
    wf = [c for c in realm.get("clients", []) if c.get("clientId") == "qa-platform-workflow"]
    if not wf or wf[0].get("secret") != "1234567890123456":
        failures.append(
            "FAIL: argo.workflowClientSecret=1234567890123456 did not render as the "
            f"JSON string \"1234567890123456\": {wf[0].get('secret') if wf else None!r}")
    tenants = [u.get("attributes", {}).get("tenant_id") for u in realm.get("users", [])]
    if not tenants or any(t != ["42"] for t in tenants):
        failures.append(
            f"FAIL: seedTenantId=42 did not render as the JSON string \"42\": {tenants!r}")

    # And outside devMode: the committed-literal refusal compares the secret
    # with a string, and a numeric value must not make that comparison error.
    for extra in ([f"keycloak.adminPassword={GENERATED_PASSWORD}"],
                  ["keycloak.adminPassword=12345"]):
        _, _, stderr, rc = render(extra, workflow="1234567890123456")
        if rc != 0:
            failures.append(
                f"FAIL: numeric-looking secrets ({extra}, "
                f"argo.workflowClientSecret=1234567890123456) "
                f"without devMode must render:\n{stderr}")


def check_short_workflow_secret_fails(failures):
    """Only the exact dev literal used to be refused, so `x` rendered.

    keycloak-deployment.yaml publishes `checksum/realm` -- a sha256 of the
    rendered realm, secret included -- on the pod, readable with `get pods`,
    so a short secret can be guessed offline. The floor applies in devMode
    too: the dev literal is 31 characters, and a throwaway stand gains nothing
    from a 1-character secret."""
    for extra in ([], ["devMode=true"]):
        for value in ("x", SHORT_WORKFLOW_SECRET, "   padded-short  "):
            _, _, stderr, rc = render(
                [f"keycloak.adminPassword={GENERATED_PASSWORD}", *extra],
                workflow=value)
            if rc == 0:
                failures.append(
                    f"FAIL: argo.workflowClientSecret={value!r} ({len(value.strip())} "
                    f"characters trimmed, extra={extra}) rendered. The floor is 16.")
            elif "at least 16" not in stderr:
                failures.append(
                    f"FAIL: a short argo.workflowClientSecret was refused (good) but "
                    f"the message does not say 'at least 16':\n{stderr}")
    _, _, stderr, rc = render(
        [f"keycloak.adminPassword={GENERATED_PASSWORD}"],
        workflow=FLOOR_WORKFLOW_SECRET)
    if rc != 0:
        failures.append(
            f"FAIL: a 16-character argo.workflowClientSecret must render:\n{stderr}")


def _supplied_realm(tmpdir, name, secret, escape_secret=False):
    """A minimal realm an operator might pass with --set-file; `secret=None`
    leaves the qa-platform-workflow client out entirely. `escape_secret`
    writes the secret's first `-` as the JSON escape `\\u002d`: the same
    value once decoded, a different string to a raw-text match."""
    clients = [{"clientId": "qa-platform-ui", "publicClient": True}]
    if secret is not None:
        clients.append({"clientId": "qa-platform-workflow",
                        "serviceAccountsEnabled": True, "secret": secret})
    text = json.dumps({"realm": "qa-platform", "clients": clients})
    if escape_secret:
        encoded = json.dumps(secret)
        escaped = encoded.replace("-", "\\u002d", 1)
        assert escaped != encoded and secret not in escaped, "fixture did not escape"
        text = text.replace(encoded, escaped)
    path = pathlib.Path(tmpdir) / name
    path.write_text(text)
    return f"keycloakRealmJson={path}"


def check_supplied_realm_is_held_to_the_same_rules(failures):
    """The keycloakRealmJson branch used to take the realm verbatim and check
    nothing, so a supplied realm carrying the dev literal rendered without
    devMode (rc=0)."""
    base = [f"keycloak.adminPassword={GENERATED_PASSWORD}"]
    with tempfile.TemporaryDirectory() as tmp:
        literal = _supplied_realm(tmp, "literal.json", DEFAULT_WORKFLOW_SECRET)
        _, _, stderr, rc = render(base, files=[literal])
        if rc == 0:
            failures.append(
                "FAIL: a supplied keycloakRealmJson carrying "
                f"{DEFAULT_WORKFLOW_SECRET} rendered without devMode.")
        elif "devMode" not in stderr or "keycloakRealmJson" not in stderr:
            failures.append(
                "FAIL: the supplied-realm dev literal was refused (good) but the "
                f"message does not name keycloakRealmJson and devMode:\n{stderr}")
        _, _, stderr, rc = render([*base, "devMode=true"], files=[literal])
        if rc != 0:
            failures.append(
                "FAIL: a supplied realm carrying the dev literal must render with "
                f"devMode=true (the escape hatch):\n{stderr}")

        # The raw-text match alone is dodged by a JSON escape; the decoded
        # comparison inside the clients loop is what refuses this one.
        escaped = _supplied_realm(tmp, "escaped.json", DEFAULT_WORKFLOW_SECRET,
                                  escape_secret=True)
        _, _, stderr, rc = render(base, files=[escaped])
        if rc == 0:
            failures.append(
                "FAIL: a supplied keycloakRealmJson carrying "
                f"{DEFAULT_WORKFLOW_SECRET} as a JSON escape rendered without devMode.")
        elif "devMode" not in stderr or "keycloakRealmJson" not in stderr:
            failures.append(
                "FAIL: the JSON-escaped supplied-realm dev literal was refused "
                f"(good) but the message does not name keycloakRealmJson and devMode:\n{stderr}")
        _, _, stderr, rc = render([*base, "devMode=true"], files=[escaped])
        if rc != 0:
            failures.append(
                "FAIL: a supplied realm carrying the JSON-escaped dev literal must "
                f"render with devMode=true (the escape hatch):\n{stderr}")

        short = _supplied_realm(tmp, "short.json", SHORT_WORKFLOW_SECRET)
        _, _, stderr, rc = render([*base, "devMode=true"], files=[short])
        if rc == 0:
            failures.append(
                "FAIL: a supplied realm whose qa-platform-workflow secret is "
                "15 characters rendered.")
        elif "at least 16" not in stderr:
            failures.append(
                f"FAIL: the short supplied secret was refused without 'at least 16':\n{stderr}")

        for name, secret in (("good.json", "a-supplied-realm-secret-0123"),
                             ("noclient.json", None)):
            docs, _, stderr, rc = render(base, files=[_supplied_realm(tmp, name, secret)])
            if rc != 0:
                failures.append(f"FAIL: supplied realm {name} must render:\n{stderr}")
                continue
            realm = _realm_from(docs)
            if realm is None or realm.get("realm") != "qa-platform":
                failures.append(
                    f"FAIL: supplied realm {name} did not reach the qa-platform-realm "
                    "Secret verbatim.")


def main():
    failures = []
    check_no_configmap_carries_the_secret(failures)
    check_admin_password_unset_fails(failures)
    check_default_password_without_devmode_fails(failures)
    check_default_password_with_devmode_succeeds(failures)
    check_workflow_secret_unset_fails(failures)
    check_dev_workflow_secret_without_devmode_fails(failures)
    check_dev_workflow_secret_with_devmode_succeeds(failures)
    check_short_workflow_secret_fails(failures)
    check_supplied_realm_is_held_to_the_same_rules(failures)
    check_realm_hardening(failures)
    check_seed_users_gated_by_devmode(failures)
    check_substituted_values_render_json_safe(failures)
    check_numeric_values_stay_json_strings(failures)
    if failures:
        for line in failures:
            print(line, file=sys.stderr)
        return 1
    print(
        "PASS: the realm renders as a Secret (no ConfigMap carries its "
        "secret), keycloak.adminPassword and argo.workflowClientSecret each "
        "have no default and each refuses its committed fixture value "
        "outside devMode, argo.workflowClientSecret is at least 16 "
        "characters, a supplied keycloakRealmJson is held to the same two "
        "rules, the realm/UI-client hardening "
        "(directAccessGrantsEnabled, bruteForceProtected, passwordPolicy, "
        "session limits) is present, and the seeded fixture users "
        "(admin/AdminPass1!, viewer/ViewerPass1!) only render when "
        "devMode=true, and every value substituted into the realm "
        "renders JSON-safe.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
