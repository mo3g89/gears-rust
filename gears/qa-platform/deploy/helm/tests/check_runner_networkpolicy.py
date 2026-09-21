"""The runner pod -- the one running tenant-written test code -- is
network-isolated: default-deny ingress and egress, with a narrow egress
allow-list, selected by a label that ACTUALLY LANDS on the pod.

# The trap this guard exists to catch

A `NetworkPolicy` whose `podSelector` matches nothing fails open and looks
installed: it renders, `helm lint`s clean, and shows up in `kubectl get
netpol`, while protecting no pod at all. The label
`runner-networkpolicy.yaml`'s `podSelector` selects on has to be the EXACT
key and value qa-runs' `argo/workflow.rs` stamps onto every runner pod
template (`NETWORK_ISOLATION_LABEL` / `NETWORK_ISOLATION_LABEL_VALUE`) -- a
fact that lives in TWO files in two languages, with no shared source, on
purpose (see workflow.rs's own doc on those constants: "the pair is asserted
from both sides ... a guard that reads both from the same source proves
nothing").

So this guard does not hardcode a third copy of the expected pair. It reads
the Rust source file for the two `pub const` declarations, reads the
rendered chart's `podSelector`, and fails if they disagree. Editing either
side without the other fails THIS check, which is the property "one file per
side" is for: a values.yaml-mediated design (the pattern
`check_runner_service_account.py` uses, where both the chart and the guard
read the SAME `values.yaml` key) would make an editor of the label key in
one file silently drift from the other, because the guard would never look
at either file directly.

# What else this guards

Default-deny shape (both `policyTypes` present, `ingress` empty), the
`.Values.argo.namespace` placement (the runner pod's own namespace, not the
release namespace gears/postgres/keycloak run in -- get this wrong and the
policy selects no pod, the same silent failure as the label mismatch), and
the egress allow-list's IN-CHART-KNOWABLE entries:

  - DNS.
  - the `qa-platform-gears` Service on `.Values.gears.port` (which is
    qa-catalog's bundle route AND qa-insights' collect endpoint at once --
    one Service serves all four gears).
  - (fix round 1) the Kubernetes API server at
    `.Values.argo.apiServerClusterIP` on TCP 443, which Argo's own `wait`
    container needs to report a run's result at all -- omitting this rule
    broke every run, because rule 3's own `except` list blocks it by
    construction.
  - Keycloak, asserted ABSENT. `check_keycloak_egress_is_gone` is the
    inverse of the check that used to live here.

# The Keycloak check flipped, and that is deliberate

Fix round 3 added a fifth destination after a live-cluster run failed on it:
`deploy/runner/fetch_bundle.py` performed a `client_credentials` exchange
in-pod, against Keycloak, before it could call the then-authenticated bundle
route. This guard proved that rule was present.

The exchange is gone, and the reason is the whole point of the change that
removed it. The credential it used -- the `qa-platform-workflow` client
secret -- was deployment-wide, unexpiring and `fullScopeAllowed`, and it sat
in an environment variable of a pod whose entire job is to execute
TENANT-AUTHORED pytest. That code could read it, mint a token and call every
authenticated route in all four gears. This NetworkPolicy could not stop it:
rule 2 and rule 5 allow-listed both destinations the credential needed,
because the exchange required them.

qa-catalog's bundle route is anonymous now and authorised by a per-bundle
HMAC tag carried in `TEST_BUNDLE_URL`'s query string, so a runner pod has no
reason to reach Keycloak and no credential to present if it did. Asserting
the rule's ABSENCE is what stops a partial revert leaving the egress path
open while the token claims to have replaced the credential.

The remaining allow-list item -- the run's target environment -- is NOT
statically knowable at all (task-8-report.md has the full reasoning) and
this guard only checks that the static superset chosen for it still
excludes the CLUSTER'S OWN pod/Service CIDRs (`argo.podCidr` /
`argo.serviceCidr`), not all of RFC 1918 -- fix round 6 found that the
broader exclusion also excluded every on-prem (private-address) target
environment, which defeats this platform's actual purpose. Not that the
rule faithfully expresses "the target and nothing else", because no static
policy can.

Standalone script, like its siblings in this directory -- see the Makefile's
`helm-tests` target for why each one is invoked directly rather than through
pytest collection."""
import ipaddress
import pathlib
import re
import subprocess
import sys

import yaml

TESTS_DIR = pathlib.Path(__file__).resolve().parent
CHART = TESTS_DIR.parent / "qa-platform"
GEARS_ROOT = TESTS_DIR.parents[2]  # gears/qa-platform
WORKFLOW_RS = (
    GEARS_ROOT
    / "qa-runs"
    / "qa-runs"
    / "src"
    / "infra"
    / "executor"
    / "argo"
    / "workflow.rs"
)
ORIGIN = "https://example-runner-netpol-test.invalid"

# The two ranges that are NOT deployment-configurable and always belong in
# rule 3's `except` list regardless of cluster: loopback and link-local.
# Fix round 6: the cluster's own pod/Service CIDRs used to be hardcoded here
# too (as blanket RFC1918), which is exactly the "excludes all private space"
# design this guard now must NOT enforce -- those two are cluster-specific
# and read from values.yaml instead, in `check_target_environment_superset`,
# the same pattern `check_api_server_egress` already uses for
# `apiServerClusterIP`.
STATIC_EXCEPT_CIDRS = {
    "127.0.0.0/8",
    "169.254.0.0/16",
}


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
    `argo.namespace` / `gears.port` actually ARE rather than a number
    hardcoded a second time and free to drift from values.yaml."""
    return yaml.safe_load((CHART / "values.yaml").read_text())


def rust_isolation_label():
    """The two `pub const` values workflow.rs stamps onto every runner pod
    template, read out of the Rust source rather than imported -- this
    script has no Rust toolchain to import them WITH. A `None` return means
    the constant is missing entirely, which the caller treats as a failure
    naming the file, not a crash."""
    if not WORKFLOW_RS.is_file():
        return None, None
    text = WORKFLOW_RS.read_text()

    def const(name):
        m = re.search(rf'pub const {name}:\s*&str\s*=\s*"([^"]*)";', text)
        return m.group(1) if m else None

    return const("NETWORK_ISOLATION_LABEL"), const("NETWORK_ISOLATION_LABEL_VALUE")


def find_netpol(docs):
    return next((d for d in docs if d.get("kind") == "NetworkPolicy"
                 and d["metadata"]["name"] == "qa-platform-runner-isolation"), None)


def check_label_pair_agrees_with_rust(failures):
    """The check this whole file exists for: the workflow's stamped label and
    the policy's selected label are the same string, read from each side's
    own source, not from a shared one."""
    docs, stderr, rc = render()
    if rc != 0:
        failures.append(f"FAIL: a default render must succeed:\n{stderr}")
        return

    key, value = rust_isolation_label()
    if key is None or value is None:
        failures.append(
            f"FAIL: could not find NETWORK_ISOLATION_LABEL / "
            f"NETWORK_ISOLATION_LABEL_VALUE as `pub const ...: &str = \"...\";` "
            f"in {WORKFLOW_RS}. Either the constants were renamed/removed, or "
            "this guard's regex needs to follow -- do not weaken this to a "
            "hardcoded literal instead: that is the exact one-shared-source "
            "failure mode this guard exists to avoid.")
        return

    netpol = find_netpol(docs)
    if netpol is None:
        failures.append(
            "FAIL: no NetworkPolicy named qa-platform-runner-isolation was "
            "rendered at all.")
        return

    selector = netpol.get("spec", {}).get("podSelector", {}).get("matchLabels", {})
    if key not in selector:
        failures.append(
            f"FAIL: the rendered podSelector {selector!r} does not carry the "
            f"key {key!r} that workflow.rs's NETWORK_ISOLATION_LABEL names. "
            "A podSelector missing this key selects a different set of pods "
            "than the runner pod -- in practice, none of them: the policy "
            "fails open and looks installed.")
        return

    rendered_value = selector[key]
    if rendered_value != value:
        failures.append(
            f"FAIL: podSelector[{key!r}] = {rendered_value!r}, but "
            f"workflow.rs's NETWORK_ISOLATION_LABEL_VALUE = {value!r}. The "
            "workflow stamps one value onto the pod; the policy selects on "
            "another. Every runner pod is then unselected by this policy and "
            "every deny is a no-op.")
        return

    print(
        f"PASS: podSelector[{key!r}] = {rendered_value!r} matches "
        f"workflow.rs's NETWORK_ISOLATION_LABEL / NETWORK_ISOLATION_LABEL_VALUE "
        "exactly")


def check_namespace_and_default_deny_shape(failures):
    docs, _, rc = render()
    if rc != 0:
        return  # already reported

    vals = chart_values()
    expected_namespace = vals["argo"]["namespace"]

    netpol = find_netpol(docs)
    if netpol is None:
        return  # already reported

    rendered_namespace = netpol["metadata"].get("namespace")
    if rendered_namespace != expected_namespace:
        failures.append(
            f"FAIL: NetworkPolicy renders in namespace {rendered_namespace!r}, "
            f"expected {expected_namespace!r} (argo.namespace) -- the "
            "namespace the runner pod actually runs in. podSelector is scoped "
            "to the NetworkPolicy's own namespace, so in the wrong namespace "
            "this policy selects no pod at all.")

    spec = netpol.get("spec", {})
    policy_types = set(spec.get("policyTypes", []))
    if {"Ingress", "Egress"} - policy_types:
        failures.append(
            f"FAIL: policyTypes = {sorted(policy_types)}, expected both "
            "Ingress and Egress -- without both, Kubernetes leaves the "
            "unlisted direction unrestricted rather than denying it by "
            "default.")

    ingress = spec.get("ingress", [])
    if ingress:
        failures.append(
            f"FAIL: ingress = {ingress!r}, expected an empty list (or absent) "
            "-- the brief is explicit: \"Ingress: none.\"")

    if not failures:
        print(
            f"PASS: NetworkPolicy renders in {expected_namespace!r} with "
            "default-deny ingress and both policyTypes declared")


def _egress_targets(netpol):
    return netpol.get("spec", {}).get("egress", [])


def check_dns_and_gears_egress(failures):
    docs, _, rc = render()
    if rc != 0:
        return

    vals = chart_values()
    expected_port = vals["gears"]["port"]

    netpol = find_netpol(docs)
    if netpol is None:
        return

    egress = _egress_targets(netpol)

    def rule_matches_kube_system_dns(rule):
        ports = {(p.get("protocol"), p.get("port")) for p in rule.get("ports", [])}
        if not {("UDP", 53), ("TCP", 53)} <= ports:
            return False
        for to in rule.get("to", []):
            ns = to.get("namespaceSelector", {}).get("matchLabels", {})
            if ns.get("kubernetes.io/metadata.name") == "kube-system":
                return True
        return False

    def rule_matches_gears_service(rule):
        ports = {p.get("port") for p in rule.get("ports", [])}
        if expected_port not in ports:
            return False
        for to in rule.get("to", []):
            pod = to.get("podSelector", {}).get("matchLabels", {})
            if pod.get("app.kubernetes.io/component") == "gears":
                return True
        return False

    if not any(rule_matches_kube_system_dns(r) for r in egress):
        failures.append(
            "FAIL: no egress rule allows DNS (UDP/TCP 53 to kube-system). "
            "Every other allowed destination needs a resolvable name first.")

    if not any(rule_matches_gears_service(r) for r in egress):
        failures.append(
            f"FAIL: no egress rule allows the qa-platform-gears pod on port "
            f"{expected_port} (gears.port). This is qa-catalog's bundle "
            "route AND qa-insights' collect endpoint -- both are served by "
            "this one Service/port.")

    if not failures:
        print(
            "PASS: egress allows DNS (kube-system:53) and the gears Service "
            f"on port {expected_port}")


# keycloak-service.yaml's own header comment: "Name and port are a fixed
# interface" -- 8080 is not a values.yaml key the way gears.port is, so
# unlike that check this one is pinned to the same literal the Service
# template itself is pinned to, not read out of values.yaml. Kept after the
# Keycloak rule was deleted because `check_keycloak_egress_is_gone` now
# checks the PORT as well as the label: a rule that opened 8080 without
# naming the pod by label would still be a path to the IdP.
KEYCLOAK_PORT = 8080


def check_keycloak_egress_is_gone(failures):
    """The inverse of the check that used to be here -- see this module's
    header for why it flipped.

    A runner pod must not be able to reach Keycloak at all. The in-pod
    `client_credentials` exchange that needed this rule is gone, and so is
    the `fullScopeAllowed` client secret it used; re-adding either half
    alone is the partial revert this assertion exists to fail."""
    docs, _, rc = render()
    if rc != 0:
        return

    netpol = find_netpol(docs)
    if netpol is None:
        return

    egress = _egress_targets(netpol)

    def rule_mentions_keycloak(rule):
        for to in rule.get("to", []):
            pod = to.get("podSelector", {}).get("matchLabels", {})
            if pod.get("app.kubernetes.io/component") == "keycloak":
                return True
        return False

    offenders = [r for r in egress if rule_mentions_keycloak(r)]
    if offenders:
        failures.append(
            "FAIL: an egress rule still allows a runner pod to reach the "
            f"qa-platform-keycloak pod: {offenders!r}. That rule existed for "
            "fetch_bundle.py's in-pod client_credentials exchange, which was "
            "deleted together with the TEST_BUNDLE_CLIENT_SECRET it used -- a "
            "deployment-wide, fullScopeAllowed credential in the environment "
            "of a pod that runs tenant-authored code. The bundle route is "
            "anonymous and signature-authorised now; nothing in a runner pod "
            "has any business talking to the IdP.")
        return

    for rule in egress:
        ports = {p.get("port") for p in rule.get("ports", [])}
        if KEYCLOAK_PORT in ports:
            failures.append(
                f"FAIL: an egress rule still opens port {KEYCLOAK_PORT} "
                f"({rule!r}). Keycloak's Service port is a fixed interface, so "
                "an allow rule on it is a path to the IdP whether or not it "
                "names the pod by label.")
            return

    print(
        "PASS: no egress rule reaches Keycloak -- the in-pod token exchange "
        "and the credential it used are both gone")


def check_target_environment_superset_excludes_private_space(failures):
    """Not a claim that this expresses "the target environment" -- it
    doesn't, and can't (see task-8-report.md). Only that whatever static
    superset was chosen still keeps the CLUSTER'S OWN pod/Service networks
    out of it -- not all of RFC1918, which is the wrong thing to check since
    fix round 6: excluding every private range also excludes every on-prem
    (private-address) target environment, which is most of what this
    platform tests (VHI/VHP). `argo.podCidr`/`argo.serviceCidr` are read
    from `values.yaml`, the same pattern `check_api_server_egress` uses for
    `apiServerClusterIP` -- these are legitimately one source of truth
    (the chart's own config), unlike the isolation label."""
    docs, _, rc = render()
    if rc != 0:
        return

    vals = chart_values()
    expected_dynamic = {vals["argo"]["podCidr"], vals["argo"]["serviceCidr"]}
    expected = expected_dynamic | STATIC_EXCEPT_CIDRS

    netpol = find_netpol(docs)
    if netpol is None:
        return

    egress = _egress_targets(netpol)
    catch_all = None
    for rule in egress:
        for to in rule.get("to", []):
            block = to.get("ipBlock")
            if block and block.get("cidr") == "0.0.0.0/0":
                catch_all = block
                break

    if catch_all is None:
        # Not a failure on its own -- a deployment may legitimately choose
        # to allow NOTHING for the unexpressible target-environment case
        # (the brief permits this: "an honest ... 'this much is not [static]'
        # ... is the right answer"). Recorded so a human reads it, not
        # silently.
        print(
            "NOTE: no 0.0.0.0/0 egress rule found -- this deployment allows "
            "no additional egress for the run's target environment. That is "
            "a valid choice (see task-8-report.md) but means runs whose "
            "target is not covered by the DNS/gears rules cannot reach it.")
        return

    excepted = set(catch_all.get("except", []))
    missing = expected - excepted
    if missing:
        failures.append(
            f"FAIL: the 0.0.0.0/0 egress rule's except list is missing "
            f"{sorted(missing)}. Without excluding the cluster's own "
            "podCidr/serviceCidr specifically, the target-environment "
            "superset re-opens the cluster's own network -- Postgres, "
            "Keycloak, the gears API, and another tenant's runner pod are "
            "all reachable there again.")
        return

    # The inverse check fix round 6 exists for: excluding MORE than the
    # cluster's own two CIDRs (plus loopback/link-local) is the exact
    # regression this round fixes, so a future edit that widens this list
    # back toward blanket RFC1918 must fail here too, not just an edit that
    # narrows it. RFC1918 checked with `ipaddress`, not a prefix guess, so a
    # network like 172.20.0.0/16 (inside 172.16.0.0/12 but not matching a
    # naive "172.16."-style string prefix) is still caught.
    rfc1918 = [ipaddress.ip_network(n) for n in
               ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16")]

    def is_private(cidr):
        try:
            net = ipaddress.ip_network(cidr, strict=False)
        except ValueError:
            return False
        return any(net.overlaps(block) for block in rfc1918)

    extra_private = {cidr for cidr in excepted - expected if is_private(cidr)}
    if extra_private:
        failures.append(
            f"FAIL: the 0.0.0.0/0 egress rule's except list excludes "
            f"{sorted(extra_private)} in addition to the cluster's own "
            f"{sorted(expected_dynamic)}. Excluding private ranges beyond "
            "the cluster's own pod/Service CIDRs is fix round 6's actual "
            "regression: on-prem (private-address) target environments -- "
            "VHI/VHP management nodes among them -- become unreachable "
            "again, silently.")
        return

    print(
        f"PASS: the 0.0.0.0/0 egress rule excludes exactly the cluster's own "
        f"{sorted(expected_dynamic)} plus loopback/link-local -- private "
        "target environments outside those two ranges stay reachable")


def check_api_server_egress(failures):
    """Fix round 1: the first cut of this policy flagged, rather than
    allow-listed, the Kubernetes API server -- and rule 3's own `except`
    list blocks it by construction (it is exactly the kind of private
    address that list exists to exclude). Without its own rule, Argo's
    `wait` container cannot report a run's result and every run breaks
    (exit code 64, AFTER the run has already produced all of its output --
    see `argo.apiServerClusterIP`'s own doc in values.yaml).

    This reads the expected address from `values.yaml`, the same pattern
    `check_runner_service_account.py` uses for `workflowServiceAccount` --
    unlike the isolation label, this value genuinely has one source of
    truth (the chart's own config), so reading it from there rather than
    hardcoding it a second time is the right shape here."""
    docs, _, rc = render()
    if rc != 0:
        return

    vals = chart_values()
    expected_ip = vals["argo"]["apiServerClusterIP"]
    expected_cidr = f"{expected_ip}/32"

    netpol = find_netpol(docs)
    if netpol is None:
        return

    egress = _egress_targets(netpol)
    api_rule = None
    for rule in egress:
        for to in rule.get("to", []):
            block = to.get("ipBlock")
            if block and block.get("cidr") == expected_cidr:
                api_rule = rule
                block_found = block
                break
        if api_rule:
            break

    if api_rule is None:
        failures.append(
            f"FAIL: no egress rule allows {expected_cidr} (argo.apiServerClusterIP). "
            "Argo's own executor (the workflow pod's `wait` container) "
            "authenticates to the Kubernetes API server at this address to "
            "report a run's result -- without this rule the run executes to "
            "completion and then fails to report it, exit code 64, "
            "workflowtaskresults.argoproj.io is forbidden.")
        return

    # Defensive: a rule that grants the address but then excepts it right
    # back out (e.g. a future edit that merges this rule into rule 3's
    # ipBlock and inherits its `except` list wholesale) would look present
    # in a naive scan but grant nothing. Caught here, at render time, rather
    # than at run time as exit code 64.
    excepted = block_found.get("except", [])
    if any(expected_ip == cidr.split("/")[0] for cidr in excepted):
        failures.append(
            f"FAIL: the egress rule for {expected_cidr} carries an `except` "
            f"entry that swallows it: {excepted!r}. A future edit that widens "
            "this rule's except list (or merges it with rule 3's) must not "
            "carve the API server back out of its own allow rule.")
        return

    ports = {(p.get("protocol"), p.get("port")) for p in api_rule.get("ports", [])}
    if ("TCP", 443) not in ports:
        failures.append(
            f"FAIL: the egress rule for {expected_cidr} does not allow TCP "
            f"443 (found {sorted(ports)!r}). The in-cluster \"kubernetes\" "
            "Service is conventionally reached on 443.")
        return

    print(
        f"PASS: egress allows the Kubernetes API server at {expected_cidr} "
        "on TCP 443, and nothing excepts it back out")


def main():
    failures = []
    check_label_pair_agrees_with_rust(failures)
    check_namespace_and_default_deny_shape(failures)
    check_dns_and_gears_egress(failures)
    check_target_environment_superset_excludes_private_space(failures)
    check_api_server_egress(failures)
    check_keycloak_egress_is_gone(failures)
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
