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
  2. The gears Deployment mounts that ConfigMap at the directory
     `GEARS_CONFIG_FILE` defaults to. A correct ConfigMap that no container
     mounts is the same defect one step along.
  3. Both states render: default-off unchanged from the committed file, and
     enabled-on with the operator's own collector address. The pair is the
     test -- a template that hard-coded `enabled: true` would pass an
     enabled-only check, and one whose transform silently no-ops would pass a
     default-only one.

The fourth case is the refusal: `enabled` true with no `endpoint` must FAIL to
render. Enabling the reader while leaving the committed loopback address is the
one combination that looks instrumented and is not -- a pod's 127.0.0.1 is the
pod, so every export fails forever into a dashboard nobody is watching.

Standalone script (a `main()` under `if __name__ == "__main__"`), like
test_pins.py and test_chart_file_sync.py beside it, and invoked directly by the
Makefile's `helm-tests` target -- `python3 -m pytest tests/` would collect zero
items from it.
"""
import pathlib
import subprocess
import sys

import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
ORIGIN = "https://example-metrics-test.invalid"
CONFIGMAP = "qa-platform-gears-config"
CONFIG_KEY = "qa-platform-stack.yaml"
# qa-platform.Dockerfile bakes the config here and GEARS_CONFIG_FILE defaults to
# it, which is why gears-deployment.yaml sets no env override -- so this is the
# path the mount has to land on for the file to be read at all.
CONFIG_MOUNT = "/etc/cf-gears"
COLLECTOR = "http://otel-collector.observability.svc.cluster.local:4317"


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
        failures.append(
            "FAIL (default render): opentelemetry.metrics.enabled is "
            f"{metrics.get('enabled')!r}, expected False. A default install must "
            "not start pushing OTLP at an address nobody configured.")
    exporter = metrics.get("exporter") or {}
    if exporter.get("kind") != "otlp_grpc" or not exporter.get("endpoint"):
        failures.append(
            "FAIL (default render): opentelemetry.metrics.exporter must carry a "
            f"kind and an endpoint, got {exporter!r}. Without them the transform "
            "in gears-config-configmap.yaml has no sentinel to rewrite.")
    if (otel.get("resource") or {}).get("service_name") != "qa-platform":
        failures.append(
            "FAIL (default render): opentelemetry.resource.service_name is "
            f"{(otel.get('resource') or {}).get('service_name')!r}, expected "
            "'qa-platform'. Unset, the toolkit's default attributes every data "
            "point to `cf-gears` and two stacks pushing to one collector become "
            "indistinguishable in every query.")
    if not failures:
        print(f"PASS: default render carries opentelemetry.metrics disabled, "
              f"with an exporter to rewrite and service_name=qa-platform")


def check_enabled(failures):
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
        failures.append(
            "FAIL (enabled render): opentelemetry.metrics.enabled is "
            f"{metrics.get('enabled')!r} after --set enabled=true. The transform "
            "in gears-config-configmap.yaml silently did not fire; the gears would "
            "come up with the built-in no-op meter provider while helm reported "
            "success.")
    if (metrics.get("exporter") or {}).get("endpoint") != COLLECTOR:
        failures.append(
            "FAIL (enabled render): the exporter endpoint is "
            f"{(metrics.get('exporter') or {}).get('endpoint')!r}, expected "
            f"{COLLECTOR!r}. Left at the committed loopback address the reader "
            "pushes into its own pod and every export fails.")
    # Enabling metrics must not switch tracing on as a side effect: both blocks
    # carry the identical line `    enabled: false`, so a transform written with
    # a one-line sentinel would rewrite whichever it reached first.
    if (otel.get("tracing") or {}).get("enabled") is not False:
        failures.append(
            "FAIL (enabled render): opentelemetry.tracing.enabled became "
            f"{(otel.get('tracing') or {}).get('enabled')!r}. The metrics transform "
            "matched the tracing block too -- its sentinel is not unique.")
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
            f"(got {mount!r}). GEARS_CONFIG_FILE defaults to that directory and "
            "the Deployment sets no override, so the gears would read the copy "
            "baked into the image instead -- with whatever metrics setting was "
            "committed, not the one this release asked for.")
        return
    print(f"PASS: the gears container mounts {CONFIGMAP} at {CONFIG_MOUNT}, "
          "which is where GEARS_CONFIG_FILE looks")


def main():
    failures = []
    check_default(failures)
    check_enabled(failures)
    check_refusal(failures)
    check_mount(failures)
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
