"""The metrics config must reach the gears Deployment, in all of its states.

# What this guards

The four qa-platform gears declare 22 metric families -- what a live scrape
carries is whichever of them have recorded a measurement, which is fewer; see
docs/DESIGN.md 3.11, "How to reach these numbers". There are now TWO ways for
one of them to leave the process:

  * PUSH -- an OTLP periodic reader, built by libs/toolkit's
    `init_metrics_provider`, aimed at a collector the operator names. Off by
    default; `check_default` / `check_enabled` / `check_refusal` below.
  * PULL -- a Prometheus text endpoint served by libs/toolkit's
    `telemetry::scrape` on its own listener. On by default; `check_scrape`
    below.

Both are switched from one block of YAML in the gears' config file, and the
first failure this module exists to catch is that block not arriving.

The pull half DOES have a container port, and that is a reversal of what this
file used to say. It said a guard on a container port "would assert something no
code reads" and that a reader would "go looking for an endpoint that does not
exist" -- true when written, and the reason the endpoint did not exist for as
long as it did. It exists now, so the port is load-bearing and the second
failure this module catches is the four copies of its number drifting apart.

# What "arriving" means here, in three links

  1. The ConfigMap `qa-platform-gears-config` carries a `qa-platform-stack.yaml`
     key whose contents PARSE AS YAML and contain the `opentelemetry` block with
     the fields the toolkit's `OpenTelemetryConfig` declares. Parsing matters:
     `OpenTelemetryConfig` is `#[serde(deny_unknown_fields)]`, so a
     misspelt key is a boot failure rather than a silently ignored line, and
     `gears-config-configmap.yaml` reaches this block by STRING REPLACEMENT --
     a transform that lands its replacement at the wrong indentation produces a
     file that still renders and no longer loads.
  2. The gears Deployment mounts that ConfigMap at the directory holding the
     config file ENTRYPOINT.SH reads -- `/etc/cf-gears/qa-platform-stack.yaml`,
     named in full both by entrypoint.sh's `GEARS_CONFIG_FILE` default and by
     the Dockerfile's `CMD --config`, and overridden by neither the Deployment
     nor this chart. A correct ConfigMap that no container mounts is the same
     defect one step along.

     **The server itself reads a different file**, and the distinction matters
     to anyone reading this test against the stand: entrypoint.sh treats
     `/etc/cf-gears/qa-platform-stack.yaml` strictly as a read-only template,
     renders it to `/var/lib/cf-gears/.rendered-qa-platform-stack.yaml`,
     rewrites the `--config` argument to that path and execs. So this mount is
     the INPUT to the render, and `deploy/remote/verify-k8s.sh`'s step 17 reads
     the rendered output. What this test can hold is the chart's half: the
     ConfigMap arrives at the path the render reads from.
  3. Both states render: default-off unchanged from the committed file, and
     enabled-on with the operator's own collector address. The pair is the
     test -- a template that hard-coded `enabled: true` would pass an
     enabled-only check, and one whose transform silently no-ops would pass a
     default-only one.

# And the catalog itself: THREE hand-maintained copies of one list

`check_catalog` is not about the chart. It is here because this is the metrics
guard, and because the 22 series names exist in three places that nothing tied
together:

  1. the constants in each gear's `domain::metrics` -- the source of truth;
  2. the table in `docs/DESIGN.md` 3.11 -- the OPERATOR-FACING copy, the one
     someone reads to write a dashboard query, and so the copy whose drift
     costs the most;
  3. the `CATALOG` heredoc in `deploy/remote/verify-k8s.sh`'s metric-catalog
     step.

(2) and (3) are hand-maintained. Nothing checked either against (1): the stand
step ties (1) to (3), but only when somebody runs a full deploy against a
cluster, which is not a gate and cannot be one in CI. A row added to a gear and
not to the table is then invisible until an operator queries a series that is
never described, or reads a table row for a series that does not exist.

So this compares all three as SETS and diffs them naming both sides. It parses
rather than imports -- these are Rust files and this is Python -- which means
the parse itself has to be checked: an expression that silently matched nothing
would turn this into a test that passes on an empty set, which is the failure
mode a "compare two lists" test has by default. Hence the per-gear expected
counts and the non-empty assertions below.

The fourth chart case is the refusal: `enabled` true with no `endpoint` must
FAIL to render. Enabling the reader while leaving the committed loopback address is the
one combination that looks instrumented and is not -- a pod's 127.0.0.1 is the
pod, so every export fails forever into a dashboard nobody is watching.

Standalone script (a `main()` under `if __name__ == "__main__"`), like
check_pins.py and check_chart_file_sync.py beside it, and invoked directly by the
Makefile's `helm-tests` target -- `python3 -m pytest tests/` would collect zero
items from it.
"""
import pathlib
import re
import subprocess
import sys

import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
ORIGIN = "https://example-metrics-test.invalid"
CONFIGMAP = "qa-platform-gears-config"
CONFIG_KEY = "qa-platform-stack.yaml"
# The DIRECTORY entrypoint.sh renders the config FROM. Both paths that name the
# file name it in full: entrypoint.sh's
# `GEARS_CONFIG_FILE:-/etc/cf-gears/qa-platform-stack.yaml` and
# qa-platform.Dockerfile's CMD `--config /etc/cf-gears/qa-platform-stack.yaml`.
# Neither is a directory, and neither is overridden by gears-deployment.yaml --
# so this is the dirname the ConfigMap volume has to land on for the template
# entrypoint.sh reads to be this release's copy rather than the one baked into
# the image. (The server is then handed the RENDERED file under
# /var/lib/cf-gears; see this module's docstring, link 2.)
CONFIG_MOUNT = "/etc/cf-gears"
COLLECTOR = "http://otel-collector.observability.svc.cluster.local:4317"
# values.yaml's `gears.metricsPort`. Restated rather than parsed so that
# CHANGING the default is a deliberate two-file edit: this number is also the
# one deploy/remote/verify-k8s.sh reaches for on the stand and the one the
# committed config's `bind_addr` carries.
DEFAULT_METRICS_PORT = 9464

# --- the catalog's three copies -------------------------------------------
# CHART is <subsystem>/deploy/helm/qa-platform, so three parents up is the
# qa-platform subsystem root.
SUBSYSTEM = CHART.parent.parent.parent
DESIGN = SUBSYSTEM / "docs" / "DESIGN.md"
VERIFY = SUBSYSTEM / "deploy" / "remote" / "verify-k8s.sh"
# Per gear, how many families its `domain::metrics` declares. Declared so a
# parse that quietly stopped matching fails LOUDLY here instead of comparing
# three empty sets and passing. Update deliberately when a family is added.
GEAR_FAMILY_COUNTS = {
    "qa-runs": 9,
    "qa-insights": 7,
    "qa-environments": 6,
    "qa-catalog": 3,
}
# The full literal series names, as they appear in the constants. Anchored on
# the opening quote so a name mentioned in a doc comment is not picked up.
SERIES_LITERAL = re.compile(r'"(qa_[a-z0-9_]+)"')
# A catalog table row: `| `qa_x_total` | counter | ... |`
DESIGN_ROW = re.compile(r"^\|\s*`(qa_[a-z0-9_]+)`\s*\|", re.MULTILINE)
# The heredoc verify-k8s.sh compares the deployed binary against.
VERIFY_HEREDOC = re.compile(r"<<'CATALOG'\n(.*?)\nCATALOG\n", re.DOTALL)


def render(*extra):
    """Return (docs, stderr, returncode). Never asserts -- one caller wants a failure."""
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


def gears_config(docs):
    """The `opentelemetry` block as the gears will actually parse it."""
    cm = [d for d in docs
          if d["kind"] == "ConfigMap" and d["metadata"]["name"] == CONFIGMAP]
    if not cm:
        return None, f"no ConfigMap named {CONFIGMAP} was rendered"
    body = cm[0].get("data", {}).get(CONFIG_KEY)
    if body is None:
        return None, f"ConfigMap {CONFIGMAP} carries no {CONFIG_KEY} key"
    try:
        parsed = yaml.safe_load(body)
    except yaml.YAMLError as exc:
        return None, (f"{CONFIGMAP}'s {CONFIG_KEY} is not valid YAML, so the gears "
                      f"would refuse to boot: {exc}")
    otel = parsed.get("opentelemetry")
    if otel is None:
        return None, (f"{CONFIGMAP}'s {CONFIG_KEY} carries no `opentelemetry` block. "
                      "Nothing else decides whether the 22 metric families leave the "
                      "process, so every series in docs/DESIGN.md 3.11 is unreachable "
                      "and the gears report no error about it.")
    return otel, None


def check_default(failures):
    """Appends to `failures`; prints its PASS only if IT added nothing.

    The `mine` list, rather than a truth test on `failures`, is what makes that
    true: gating on the shared list works only for whichever check happens to
    run first, and this one only ran first by accident of `main`'s ordering.
    A guard against a false green must not itself print one.
    """
    mine = []
    docs, _, rc = render()
    if rc != 0:
        failures.append("FAIL: a default render must succeed")
        return
    otel, why = gears_config(docs)
    if why:
        failures.append(f"FAIL (default render): {why}")
        return
    metrics = otel.get("metrics") or {}
    if metrics.get("enabled") is not False:
        mine.append(
            "FAIL (default render): opentelemetry.metrics.enabled is "
            f"{metrics.get('enabled')!r}, expected False. A default install must "
            "not start pushing OTLP at an address nobody configured.")
    exporter = metrics.get("exporter") or {}
    if exporter.get("kind") != "otlp_grpc" or not exporter.get("endpoint"):
        mine.append(
            "FAIL (default render): opentelemetry.metrics.exporter must carry a "
            f"kind and an endpoint, got {exporter!r}. Without them the transform "
            "in gears-config-configmap.yaml has no sentinel to rewrite.")
    if (otel.get("resource") or {}).get("service_name") != "qa-platform":
        mine.append(
            "FAIL (default render): opentelemetry.resource.service_name is "
            f"{(otel.get('resource') or {}).get('service_name')!r}, expected "
            "'qa-platform'. Unset, the toolkit's default attributes every data "
            "point to `cf-gears` and two stacks pushing to one collector become "
            "indistinguishable in every query.")
    failures.extend(mine)
    if not mine:
        print("PASS: default render carries opentelemetry.metrics disabled, "
              "with an exporter to rewrite and service_name=qa-platform")


def check_enabled(failures):
    """Appends to `failures`; prints its PASS only if IT added nothing.

    See `check_default` for why the accumulator is local. This one carried the
    sharper version of the same defect: its `print` was unconditional, so a run
    that appended three failures still ended the line with `PASS:`.
    """
    mine = []
    docs, _, rc = render("--set", "opentelemetry.metrics.enabled=true",
                         "--set", f"opentelemetry.metrics.endpoint={COLLECTOR}")
    if rc != 0:
        failures.append("FAIL: enabling metrics with an endpoint must render")
        return
    otel, why = gears_config(docs)
    if why:
        failures.append(f"FAIL (enabled render): {why}")
        return
    metrics = otel.get("metrics") or {}
    if metrics.get("enabled") is not True:
        mine.append(
            "FAIL (enabled render): opentelemetry.metrics.enabled is "
            f"{metrics.get('enabled')!r} after --set enabled=true. The transform "
            "in gears-config-configmap.yaml silently did not fire; the gears would "
            "come up with the built-in no-op meter provider while helm reported "
            "success.")
    if (metrics.get("exporter") or {}).get("endpoint") != COLLECTOR:
        mine.append(
            "FAIL (enabled render): the exporter endpoint is "
            f"{(metrics.get('exporter') or {}).get('endpoint')!r}, expected "
            f"{COLLECTOR!r}. Left at the committed loopback address the reader "
            "pushes into its own pod and every export fails.")
    # Enabling metrics must not switch tracing on as a side effect: both blocks
    # carry the identical line `    enabled: false`, so a transform written with
    # a one-line sentinel would rewrite whichever it reached first.
    if (otel.get("tracing") or {}).get("enabled") is not False:
        mine.append(
            "FAIL (enabled render): opentelemetry.tracing.enabled became "
            f"{(otel.get('tracing') or {}).get('enabled')!r}. The metrics transform "
            "matched the tracing block too -- its sentinel is not unique.")
    failures.extend(mine)
    if not mine:
        print("PASS: --set opentelemetry.metrics.enabled=true reaches the gears' "
              "config with the operator's endpoint, and leaves tracing off")


def check_refusal(failures):
    _, stderr, rc = render("--set", "opentelemetry.metrics.enabled=true")
    if rc == 0:
        failures.append(
            "FAIL: enabling metrics with no endpoint rendered successfully. That "
            "ships a stack whose reader pushes into its own pod's loopback "
            "address every interval, forever, while looking instrumented.")
    elif "opentelemetry.metrics.endpoint is required" not in stderr:
        failures.append(
            "FAIL: enabling metrics with no endpoint failed for some other "
            f"reason than the intended `required`:\n{stderr}")
    else:
        print("PASS: enabling metrics without an endpoint is refused at render")


def check_mount(failures):
    docs, _, rc = render()
    if rc != 0:
        return
    deploys = [d for d in docs if d["kind"] == "Deployment"
               and d["metadata"]["name"] == "qa-platform-gears"]
    if not deploys:
        failures.append("FAIL: no Deployment named qa-platform-gears was rendered")
        return
    spec = deploys[0]["spec"]["template"]["spec"]
    volume = next((v for v in spec.get("volumes", [])
                   if (v.get("configMap") or {}).get("name") == CONFIGMAP), None)
    if volume is None:
        failures.append(
            f"FAIL: the gears Deployment mounts no volume from {CONFIGMAP}. The "
            "config the metrics block lives in never reaches the process.")
        return
    container = spec["containers"][0]
    mount = next((m for m in container.get("volumeMounts", [])
                  if m.get("name") == volume["name"]), None)
    if mount is None or mount.get("mountPath") != CONFIG_MOUNT:
        failures.append(
            f"FAIL: {CONFIGMAP} is a volume but is not mounted at {CONFIG_MOUNT} "
            f"(got {mount!r}). entrypoint.sh renders from "
            f"{CONFIG_MOUNT}/{CONFIG_KEY} -- its GEARS_CONFIG_FILE "
            "default and the Dockerfile's CMD --config both name that file, and "
            "the Deployment overrides neither -- so with the volume landing "
            "anywhere else entrypoint.sh would render the copy baked into the "
            "image instead, and the server would be handed whatever metrics "
            "setting was committed rather than the one this release asked for.")
        return
    print(f"PASS: the gears container mounts {CONFIGMAP} at {CONFIG_MOUNT}, "
          f"the directory holding the {CONFIG_MOUNT}/{CONFIG_KEY} entrypoint.sh "
          "renders the server's config from")


def constants_catalog(failures):
    """The 22 series names, read from the four gears' `domain::metrics`.

    Comment lines are stripped first: these modules discuss their own series
    names in prose, and a doc comment is not a declaration.
    """
    names = set()
    for gear, expected in sorted(GEAR_FAMILY_COUNTS.items()):
        path = SUBSYSTEM / gear / gear / "src" / "domain" / "metrics.rs"
        if not path.is_file():
            failures.append(
                f"FAIL (catalog): {path} does not exist. The gear was renamed or "
                "moved and this guard can no longer read the source of truth; "
                "fix the path rather than deleting the row.")
            continue
        code = "\n".join(
            line for line in path.read_text().splitlines()
            if not line.lstrip().startswith(("//", "*")))
        found = set(SERIES_LITERAL.findall(code))
        if len(found) != expected:
            failures.append(
                f"FAIL (catalog): {gear} declares {len(found)} series "
                f"({sorted(found)}), expected {expected}. Either a family was "
                "added or removed -- update GEAR_FAMILY_COUNTS and the DESIGN "
                "3.9 table together -- or this guard's parse stopped matching, "
                "in which case every comparison below is against a set that is "
                "quietly too small.")
        names |= found
    return names


def check_catalog(failures):
    """Constants, DESIGN 3.11's table and verify-k8s.sh's heredoc must agree."""
    constants = constants_catalog(failures)
    if not constants:
        failures.append(
            "FAIL (catalog): read ZERO series names out of the gears. Refusing "
            "to compare, because an empty source of truth makes every other "
            "copy trivially 'correct'.")
        return

    if not DESIGN.is_file():
        failures.append(f"FAIL (catalog): {DESIGN} does not exist")
        return
    design = set(DESIGN_ROW.findall(DESIGN.read_text()))

    if not VERIFY.is_file():
        failures.append(f"FAIL (catalog): {VERIFY} does not exist")
        return
    heredoc = VERIFY_HEREDOC.search(VERIFY.read_text())
    if heredoc is None:
        failures.append(
            f"FAIL (catalog): {VERIFY.name} carries no <<'CATALOG' heredoc. The "
            "stand check that compares the deployed binary against the catalog "
            "was renamed or removed; this guard cannot see its list.")
        return
    verify = {line.strip() for line in heredoc.group(1).splitlines() if line.strip()}

    for label, other in (("docs/DESIGN.md 3.11's table", design),
                         (f"{VERIFY.name}'s CATALOG heredoc", verify)):
        missing = sorted(constants - other)
        invented = sorted(other - constants)
        if missing or invented:
            failures.append(
                f"FAIL (catalog): {label} does not match the constants in the "
                f"gears' domain::metrics.\n"
                f"  declared by a gear, ABSENT there: {missing or 'none'}\n"
                f"  listed there, declared by NO gear: {invented or 'none'}\n"
                "  The first is a series an operator can never find documented; "
                "the second is a table row whose query returns nothing, forever, "
                "with no error anywhere. The constants are the source of truth.")
    if not any(f.startswith("FAIL (catalog)") for f in failures):
        print(f"PASS: all {len(constants)} catalog series names agree across the "
              "gears' constants, DESIGN 3.11's table and verify-k8s.sh's heredoc")


def scrape_invariants(docs, port):
    """Every place the scrape port and switch appear, as one dict.

    Returns `None` for a key whose object is absent, so a caller can tell
    "rendered with the wrong value" from "not rendered at all" -- the two have
    different causes and only one of them is a drift.
    """
    found = {}

    cm = [d for d in docs
          if d["kind"] == "ConfigMap" and d["metadata"]["name"] == CONFIGMAP]
    scrape = None
    if cm:
        body = cm[0].get("data", {}).get(CONFIG_KEY)
        if body:
            metrics = (yaml.safe_load(body).get("opentelemetry") or {}).get("metrics") or {}
            scrape = metrics.get("scrape")
    found["config"] = scrape

    deploys = [d for d in docs if d["kind"] == "Deployment"
               and d["metadata"]["name"] == "qa-platform-gears"]
    if deploys:
        tmpl = deploys[0]["spec"]["template"]
        container = tmpl["spec"]["containers"][0]
        found["container_port"] = next(
            (p for p in container.get("ports", []) if p.get("name") == "metrics"), None)
        found["annotations"] = {
            k: v for k, v in (tmpl["metadata"].get("annotations") or {}).items()
            if k.startswith("prometheus.io/")}
    else:
        found["container_port"] = None
        found["annotations"] = None

    svcs = [d for d in docs if d["kind"] == "Service"
            and d["metadata"]["name"] == "qa-platform-gears"]
    if svcs:
        found["service_port"] = next(
            (p for p in svcs[0]["spec"]["ports"] if p.get("name") == "metrics"), None)
    else:
        found["service_port"] = None

    found["expected_port"] = port
    return found


def check_scrape(failures):
    """The scrape endpoint is reachable by default, and its port never drifts.

    THE DEFECT THIS EXISTS FOR, stated plainly so a future reader does not
    weaken it by accident: for the whole life of this chart before the guard
    below, every one of the 22 families was reachable by nothing.
    Push was off (correctly -- it needs a collector), and no route served them.
    A load test had to stand up a throwaway OTel collector to read the stack's
    own numbers. `check_default` passed the entire time, because "push is off"
    was exactly what it asserted.

    So the first assertion here is the one that matters: a DEFAULT render makes
    metrics reachable. The rest guard the ways that can be true on paper and
    false in a cluster.

    Appends to `failures`; prints its PASS only if IT added nothing -- see
    `check_default` for why the accumulator is local.
    """
    mine = []

    docs, _, rc = render()
    if rc != 0:
        failures.append("FAIL: a default render must succeed")
        return
    got = scrape_invariants(docs, DEFAULT_METRICS_PORT)

    if (got["config"] or {}).get("enabled") is not True:
        mine.append(
            "FAIL (default render): opentelemetry.metrics.scrape.enabled is "
            f"{(got['config'] or {}).get('enabled')!r}, expected True. A default "
            "install would then serve none of the 22 metric families it declares, "
            "with push off as well -- which is the exact state this guard "
            "was added to end. If you are turning this off deliberately, the "
            "owner decision to reverse is #18.")
    if (got["config"] or {}).get("path") != "/metrics":
        mine.append(
            "FAIL (default render): the scrape path is "
            f"{(got['config'] or {}).get('path')!r}, expected '/metrics'. The pod "
            "annotation below hard-codes that path; a config that serves another "
            "one leaves every scraper on a 404.")
    if got["container_port"] is None:
        mine.append(
            "FAIL (default render): the gears container declares no port named "
            "'metrics'. gears-service.yaml resolves `targetPort: metrics` BY "
            "NAME, so without it the Service port has no backend and the "
            "endpoint never becomes ready -- silently.")
    if got["service_port"] is None:
        mine.append(
            "FAIL (default render): the gears Service publishes no port named "
            "'metrics'. The process would listen inside the pod and nothing in "
            "the cluster could reach it, which is the same unreachability one "
            "layer along.")
    elif got["service_port"].get("targetPort") != "metrics":
        mine.append(
            "FAIL (default render): the Service's metrics targetPort is "
            f"{got['service_port'].get('targetPort')!r}, expected the NAME "
            "'metrics'. A numeric targetPort is a fourth copy of the port number "
            "that nothing checks against the other three.")
    if (got["annotations"] or {}).get("prometheus.io/scrape") != "true":
        mine.append(
            "FAIL (default render): the gears pod carries no "
            "prometheus.io/scrape annotation. This cluster has no Prometheus "
            "Operator (no monitoring.coreos.com CRDs), so the annotations are "
            "the only discovery convention available; without them a scraper "
            "installed later finds nothing.")

    # THE DRIFT CASE. The port is one number in four places -- config bind_addr,
    # containerPort, Service targetPort's backing port, and the annotation.
    # Three of them read .Values.gears.metricsPort directly; the fourth is a
    # string replacement in gears-config-configmap.yaml, which is the one that
    # can silently no-op. A pod that advertises 9999 and listens on 9464 is a
    # scrape target that answers `connection refused` forever, with no error on
    # this side of it.
    moved = 9999
    docs, _, rc = render("--set", f"gears.metricsPort={moved}")
    if rc != 0:
        mine.append(f"FAIL: --set gears.metricsPort={moved} must render")
    else:
        got = scrape_invariants(docs, moved)
        want_bind = f"0.0.0.0:{moved}"
        if (got["config"] or {}).get("bind_addr") != want_bind:
            mine.append(
                "FAIL (moved-port render): the gears' config binds "
                f"{(got['config'] or {}).get('bind_addr')!r}, expected "
                f"{want_bind!r}. The fifth transform in "
                "gears-config-configmap.yaml silently did not fire, so the "
                "process listens on the committed port while the Deployment, "
                "the Service and the annotation all advertise the one the "
                "operator asked for.")
        if (got["container_port"] or {}).get("containerPort") != moved:
            mine.append(
                "FAIL (moved-port render): the container port is "
                f"{(got['container_port'] or {}).get('containerPort')!r}, "
                f"expected {moved}.")
        if (got["service_port"] or {}).get("port") != moved:
            mine.append(
                "FAIL (moved-port render): the Service port is "
                f"{(got['service_port'] or {}).get('port')!r}, expected {moved}.")
        if (got["annotations"] or {}).get("prometheus.io/port") != str(moved):
            mine.append(
                "FAIL (moved-port render): prometheus.io/port is "
                f"{(got['annotations'] or {}).get('prometheus.io/port')!r}, "
                f"expected {str(moved)!r}.")

    # THE OFF CASE. Turning the endpoint off must take the advertisement with
    # it. An annotation or a Service port left behind on a pod that is not
    # listening is a permanent `connection refused` for whatever scrapes it.
    docs, _, rc = render("--set", "opentelemetry.metrics.scrape.enabled=false")
    if rc != 0:
        mine.append("FAIL: --set opentelemetry.metrics.scrape.enabled=false must render")
    else:
        got = scrape_invariants(docs, DEFAULT_METRICS_PORT)
        if (got["config"] or {}).get("enabled") is not False:
            mine.append(
                "FAIL (scrape-off render): the gears' config still says "
                f"scrape.enabled={(got['config'] or {}).get('enabled')!r}. The "
                "sixth transform did not fire and the escape hatch is a value "
                "nothing reads.")
        for key, what in (("container_port", "container port"),
                          ("service_port", "Service port")):
            if got[key] is not None:
                mine.append(
                    f"FAIL (scrape-off render): the metrics {what} is still "
                    f"declared ({got[key]!r}) while nothing is listening.")
        if got["annotations"]:
            mine.append(
                "FAIL (scrape-off render): the pod still carries "
                f"{got['annotations']!r} while nothing is listening.")

    failures.extend(mine)
    if not mine:
        print("PASS: a default render serves /metrics on "
              f"{DEFAULT_METRICS_PORT} -- config, containerPort, Service port "
              "and prometheus.io annotations agree, move together on --set "
              "gears.metricsPort, and all disappear together when scrape is "
              "turned off")


def main():
    failures = []
    check_default(failures)
    check_enabled(failures)
    check_refusal(failures)
    check_mount(failures)
    check_scrape(failures)
    check_catalog(failures)
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
