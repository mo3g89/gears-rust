"""The metrics config must reach the gears Deployment, in both of its states.

# What this guards, and why it is NOT a port

The four qa-platform gears declare 22 metric families. Every one of them is
PUSHED over OTLP by a periodic reader that libs/toolkit's
`init_metrics_provider` builds; nothing in this repository serves a `/metrics`
route, no chart in it carries a `prometheus.io/scrape` annotation, and no chart
sets an `OTEL_EXPORTER_OTLP_*` variable. So the thing that decides whether a
single series ever leaves this stack is one block of YAML in the gears' config
file -- the same shape mini-chat's own configmap established -- and the failure
this test exists to catch is that block not arriving.

A guard on a container port would assert something no code reads. It would also
be worse than useless: it would establish a second convention beside the config
block, and the first person to believe it would go looking for an endpoint that
does not exist.

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
  2. the table in `docs/DESIGN.md` 3.9 -- the OPERATOR-FACING copy, the one
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
test_pins.py and test_chart_file_sync.py beside it, and invoked directly by the
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
    "qa-runs": 7,
    "qa-insights": 7,
    "qa-environments": 6,
    "qa-catalog": 2,
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
         "--namespace", "qa-platform", "--set", f"publicOrigin={ORIGIN}", *extra],
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
                      "process, so every series in docs/DESIGN.md 3.9 is unreachable "
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
    """Constants, DESIGN 3.9's table and verify-k8s.sh's heredoc must agree."""
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

    for label, other in (("docs/DESIGN.md 3.9's table", design),
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
              "gears' constants, DESIGN 3.9's table and verify-k8s.sh's heredoc")


def main():
    failures = []
    check_default(failures)
    check_enabled(failures)
    check_refusal(failures)
    check_mount(failures)
    check_catalog(failures)
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
