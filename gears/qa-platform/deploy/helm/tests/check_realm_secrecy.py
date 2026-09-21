"""The realm no longer ships as a ConfigMap, and the admin password no
longer has a default.

# The trap this guard exists to catch

`keycloak-realm-secret.yaml` (a ConfigMap until this task) carries
`realm-qa-platform.json`, which pins the `qa-platform-workflow` client's
confidential secret (`argo.workflowClientSecret`) so the realm and
`workflow-oidc-secret.yaml`'s Secret agree across a restart. A ConfigMap is
readable by anything in the namespace that can read ConfigMaps -- far
wider than the RBAC most clusters put on Secrets -- so rendering that same
value into a ConfigMap defeated the one place this chart already treats it
as secret material.

Separately, `keycloak.adminPassword` used to default to the well-known
value `admin`, which is also its username: an install nobody thought to
override a default value on published the master realm's admin console
behind `admin`/`admin`.

A third trap this guard now also catches: the realm file unconditionally
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

Standalone script, like its siblings in this directory -- see the
Makefile's `helm-tests` target for why each one is invoked directly rather
than through pytest collection."""
import json
import pathlib
import subprocess
import sys

import yaml

TESTS_DIR = pathlib.Path(__file__).resolve().parent
CHART = TESTS_DIR.parent / "qa-platform"
ORIGIN = "https://example-realm-secrecy-test.invalid"
RELEASE = "realm-secrecy-test"
GENERATED_PASSWORD = "not-the-default-Tr0ub4dor&3"  # nosec: test fixture only
DEFAULT_WORKFLOW_SECRET = "qa-platform-workflow-dev-secret"
BUNDLE_SIGNING_KEY = "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key"  # nosec: test fixture only
COLLECT_SIGNING_KEY = "collectReportSigningSecret=guard-fixture-not-a-real-collect-key"  # nosec: test fixture only


def render(extra_sets=None):
    """Return (docs, stdout, stderr, returncode). Never asserts -- callers
    decide."""
    cmd = ["helm", "template", RELEASE, str(CHART),
           "--namespace", "qa-platform", "--set", f"publicOrigin={ORIGIN}",
           # The two signing secrets have no default (2026-09-21) and are not
           # what any check here is about -- they go in the BASE command so
           # that check_admin_password_unset_fails below still fails on the
           # one value it is testing, and names it.
           "--set", BUNDLE_SIGNING_KEY,
           "--set", COLLECT_SIGNING_KEY]
    for s in extra_sets or []:
        cmd += ["--set", s]
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


def main():
    failures = []
    check_no_configmap_carries_the_secret(failures)
    check_admin_password_unset_fails(failures)
    check_default_password_without_devmode_fails(failures)
    check_default_password_with_devmode_succeeds(failures)
    check_realm_hardening(failures)
    check_seed_users_gated_by_devmode(failures)
    if failures:
        for line in failures:
            print(line, file=sys.stderr)
        return 1
    print(
        "PASS: the realm renders as a Secret (no ConfigMap carries its "
        "secret), keycloak.adminPassword has no default and the fixture "
        "value is refused outside devMode, the realm/UI-client hardening "
        "(directAccessGrantsEnabled, bruteForceProtected, passwordPolicy, "
        "session limits) is present, and the seeded fixture users "
        "(admin/AdminPass1!, viewer/ViewerPass1!) only render when "
        "devMode=true.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
