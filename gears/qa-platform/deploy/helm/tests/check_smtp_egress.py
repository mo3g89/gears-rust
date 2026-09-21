"""qa-insights' SMTP allow-list must reach the gears, in both of its states.

# What this guards, and why it is not a NetworkPolicy guard

`qaInsights.smtpAllowedHosts` is the ONLY egress control on qa-insights' mail
path. There is no NetworkPolicy behind it, deliberately:

  * This chart has one NetworkPolicy and it selects the RUNNER pod. The gears
    pod is selected by nothing, which in Kubernetes means unrestricted egress;
    the first policy with `policyTypes: [Egress]` to select it denies everything
    it does not list.
  * What it would have to list includes Postgres, Keycloak, the Argo and
    Kubernetes APIs, arbitrary git remotes (qa-catalog) and arbitrary
    tenant-supplied management nodes (qa-environments). The runner policy's own
    rule 3 already concedes that the last shape is not statically expressible
    and falls back to `0.0.0.0/0 except <cluster CIDRs>`. Any gears policy would
    have to do the same, at which point an SMTP rule inside it narrows nothing.
  * The relay address is `qa_notification_config.email_smtp_host`, a PER-TENANT
    column typed into the settings page at runtime. A NetworkPolicy matches
    CIDRs and pod labels; it has no DNS-name matcher at all.

ADR-0011 (`docs/ADR/0011-cpt-cf-qa-adr-smtp-egress.md`) carries that decision.
The consequence is that this value, travelling intact from `values.yaml` to the
gears' config file, IS the rule -- so it gets the same treatment every other
load-bearing transform in `gears-config-configmap.yaml` gets.

# The three states, and why all three are checked

  1. DEFAULT -- `smtp_allowed_hosts: []` arrives unchanged. This is the state
     in which qa-insights binds `UnsupportedMailClient` and every email
     notification FAILS with `unsupported_egress`. A template that hard-coded
     a host would pass an enabled-only check.
  2. CONFIGURED -- the operator's hosts arrive, all of them, as a YAML list the
     gears' `Vec<String>` will parse. `deny_unknown_fields` is on that config
     struct and the transform is a STRING REPLACEMENT, so a replacement landing
     at the wrong indentation produces a file that still renders and no longer
     loads. Parsing the ConfigMap body as YAML is what catches that.
  3. GUARDED -- the transform's own `fail` fires when its sentinel string
     disappears from the committed config. Without it, someone renaming the key
     in `files/qa-platform-stack.yaml` gets a chart that renders cleanly,
     reports success, and silently ignores every host an operator allow-listed.
     `check_transform_fails_without_its_sentinel` proves the guard by REMOVING
     the sentinel from a scratch copy of the chart and asserting the render
     fails -- a guard nobody has seen fail is a guard nobody knows works.

# What this cannot hold

That the running gear enforces the list. That is
`infra::notify::mail_smtp::tests::a_host_outside_the_allow_list_is_refused_without_dialling`,
in Rust, against a live mock relay. This holds the chart's half: the value an
operator sets is the value the process reads.
"""
import pathlib
import shutil
import subprocess
import sys
import tempfile

import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
ORIGIN = "https://example-smtp-test.invalid"
CONFIGMAP = "qa-platform-gears-config"
CONFIG_KEY = "qa-platform-stack.yaml"
# The committed default, and the string gears-config-configmap.yaml's seventh
# transform replaces. Restated here rather than parsed out of the template: this
# file and that one are the two halves whose agreement is the thing being
# checked, so deriving one from the other would make the check vacuous.
SENTINEL = "smtp_allowed_hosts: []"
HOSTS = ["smtp.corp.example", "Relay-Two.example"]


def render(chart=CHART, *extra):
    """Return (docs, stderr, returncode). Never asserts -- one caller wants a failure."""
    out = subprocess.run(
        ["helm", "template", "qa-platform", str(chart),
         "--namespace", "qa-platform", "--set", f"publicOrigin={ORIGIN}",
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


def gears_config(docs):
    """The gears' config file, parsed as the gears will parse it."""
    cm = [d for d in docs
          if d.get("kind") == "ConfigMap" and d["metadata"]["name"] == CONFIGMAP]
    if not cm:
        return None, f"no ConfigMap named {CONFIGMAP} was rendered"
    body = cm[0].get("data", {}).get(CONFIG_KEY)
    if body is None:
        return None, f"{CONFIGMAP} has no {CONFIG_KEY} key"
    try:
        parsed = yaml.safe_load(body)
    except yaml.YAMLError as exc:
        return None, f"{CONFIG_KEY} does not parse as YAML: {exc}"
    return parsed, None


def allowed_hosts(parsed):
    """`gears.qa-insights.config.smtp_allowed_hosts`, or a message."""
    try:
        config = parsed["gears"]["qa-insights"]["config"]
    except (KeyError, TypeError) as exc:
        return None, f"the qa-insights config block is not where it was: {exc}"
    if "smtp_allowed_hosts" not in config:
        return None, (
            "qa-insights' config has no smtp_allowed_hosts key; without it the "
            "gear binds UnsupportedMailClient and no operator setting can change that"
        )
    return config["smtp_allowed_hosts"], None


def check_default():
    """A stock install allow-lists nothing, and says so in the config."""
    docs, stderr, code = render()
    if code != 0:
        return False, f"FAIL: default render failed: {stderr}"
    parsed, err = gears_config(docs)
    if err:
        return False, f"FAIL: {err}"
    hosts, err = allowed_hosts(parsed)
    if err:
        return False, f"FAIL: {err}"
    if hosts != []:
        return False, (
            "FAIL: a default install must allow-list no SMTP relay, so that mail "
            f"fails loudly rather than reaching an unvetted host; got {hosts!r}"
        )
    return True, "OK: a default install allow-lists no SMTP relay"


def check_configured():
    """Every host the operator set arrives, verbatim and in order."""
    sets = []
    for i, host in enumerate(HOSTS):
        sets += ["--set", f"qaInsights.smtpAllowedHosts[{i}]={host}"]
    docs, stderr, code = render(CHART, *sets)
    if code != 0:
        return False, f"FAIL: configured render failed: {stderr}"
    parsed, err = gears_config(docs)
    if err:
        return False, f"FAIL: {err}"
    hosts, err = allowed_hosts(parsed)
    if err:
        return False, f"FAIL: {err}"
    if hosts != HOSTS:
        return False, (
            f"FAIL: qaInsights.smtpAllowedHosts={HOSTS!r} must reach the gears' "
            f"config unchanged -- a dropped entry is a relay an operator "
            f"allow-listed and the gear will refuse; got {hosts!r}"
        )
    return True, f"OK: {len(HOSTS)} allow-listed relays reach the gears' config"


def check_transform_fails_without_its_sentinel():
    """Breaking the rule must break the render, not pass quietly.

    A scratch copy of the chart with the committed key renamed. If the transform
    ever loses its `fail` guard, this render succeeds and an operator's
    allow-list is silently discarded -- which is the exact failure the guard
    exists for, so it is exercised rather than assumed.
    """
    with tempfile.TemporaryDirectory() as tmp:
        scratch = pathlib.Path(tmp) / "qa-platform"
        shutil.copytree(CHART, scratch)
        committed = scratch / "files" / CONFIG_KEY
        text = committed.read_text()
        if SENTINEL not in text:
            return False, (
                f"FAIL: the committed config no longer contains {SENTINEL!r}, so this "
                "check cannot break it -- the chart's seventh transform is already "
                "silently no-opping"
            )
        committed.write_text(text.replace(SENTINEL, "smtp_permitted_hosts: []"))

        _, stderr, code = render(scratch)
        if code == 0:
            return False, (
                "FAIL: the chart rendered with qa-insights' SMTP allow-list key "
                "renamed. gears-config-configmap.yaml's seventh transform must "
                "`fail` instead, or an operator's qaInsights.smtpAllowedHosts is "
                "discarded with helm reporting success"
            )
        if "smtp_allowed_hosts" not in stderr:
            return False, (
                "FAIL: the render failed, but not with a message naming the key an "
                f"operator has to fix; got: {stderr}"
            )
    return True, "OK: a renamed allow-list key fails the render, naming the key"


def main():
    ok = True
    for check in (check_default, check_configured,
                  check_transform_fails_without_its_sentinel):
        passed, message = check()
        print(message)
        ok = ok and passed
    if not ok:
        sys.exit(1)
    print("check_smtp_egress: OK")


if __name__ == "__main__":
    main()
