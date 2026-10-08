#!/usr/bin/env python3
"""The Slack webhook URL's path is a credential (`/services/T.../B.../<token>`,
qa-insights' slack_oagw.rs header). It stays out of the logs only while the
libraries that carry it log at `info` or less verbose: pingora, which oagw
proxies through, prints the whole request line / header block at DEBUG and
TRACE, and no hook can override that. The same goes for the api-gateway, whose
debug lines name every request path it routes.

This guard renders the chart and reads the gears logging configuration that
actually reaches the pod -- the `logging:` section of the rendered
`qa-platform-stack.yaml` Secret, and every `RUST_LOG` in the rendered
workloads -- and fails when:
  * `default` (console or file) is more verbose than `info`, or RUST_LOG is a
    bare level more verbose than `info`;
  * a target matching `pingora*`, `oagw*`, `cf_gears_oagw*` or
    `api_gateway`/`api-gateway` is set to `debug` or `trace`, in the config or
    in a RUST_LOG directive.
Other targets (sqlx, qa_*) are deliberately out of scope: they never see the
proxied request line.

The committed default passing is not enough: `gears.logLevel` is an operator
knob that becomes RUST_LOG, and `--set gears.logLevel=debug` put the request
dumps straight back. So the chart REFUSES to render a gears.logLevel that makes
the global level, or a pingora/oagw/api-gateway target, more verbose than info
-- unless `gears.allowVerboseProxyLogs=true` says the operator accepts webhook
credentials in the logs. This guard checks that refusal both ways: every
REFUSED value fails the render naming `gears.allowVerboseProxyLogs`, every
ACCEPTED value renders, and a refused value renders once the opt-in is set.
A directive with a target and no level (`pingora`) means TRACE for that target
in tracing's EnvFilter, and is refused as such."""
import pathlib
import re
import subprocess
import sys

import yaml

CHART = pathlib.Path(__file__).resolve().parents[1] / "qa-platform"
SETS = ["publicOrigin=https://example-loglevel.invalid",
        "keycloak.adminPassword=log-fixture-not-a-real-password",
        "bundleDownloadSigningSecret=log-fixture-not-a-real-bundle-key",
        "collectReportSigningSecret=log-fixture-not-a-real-collect-key",
        "argo.workflowClientSecret=log-fixture-not-a-real-workflow-secret",
        "postgres.password=log-fixture-not-a-real-db-password"]
ORDER = {"off": 0, "error": 1, "warn": 2, "info": 3, "debug": 4, "trace": 5,
         "0": 0, "1": 1, "2": 2, "3": 3, "4": 4, "5": 5}
# The crates whose debug/trace output carries the request line. Matched by
# prefix BOTH ways, as the chart's `qa-platform.rustLog` does: EnvFilter applies
# a directive to every event whose target starts with the directive's target, so
# `ping=debug` reaches pingora, and `pingora_core` is inside pingora.
PROTECTED = ("pingora", "pingora_core", "pingora_proxy", "oagw", "cf_gears_oagw",
             "api_gateway", "api-gateway")
WHY = ("the Slack webhook path is a credential and pingora/oagw/api-gateway print request "
       "lines at debug and trace")


def base_of(target):
    """A directive target without its `[span...]` suffix and `::module` tail."""
    return str(target).strip().split("[")[0].split("::")[0]


def is_sensitive(target):
    """The directive covers a protected crate (it is a prefix of one), or sits
    inside one (one is a prefix of it). An empty base (`[span]=debug`) applies
    to every target, so it is sensitive too."""
    base = base_of(target)
    return base == "" or any(p.startswith(base) or base.startswith(p) for p in PROTECTED)


def verbosity(level):
    return ORDER.get(str(level).strip().lower(), 0)


def helm_template(literals=()):
    cmd = ["helm", "template", "qa-platform", str(CHART), "--namespace", "qa-platform"]
    for s in SETS:
        cmd += ["--set", s]
    # `--set` splits on `,`, and a RUST_LOG list is comma-separated.
    for s in literals:
        cmd += ["--set-literal", s]
    return subprocess.run(cmd, capture_output=True, text=True)


def render():
    r = helm_template()
    if r.returncode != 0:
        raise SystemExit(f"FAIL: chart does not render: {r.stderr.strip()}")
    return [d for d in yaml.safe_load_all(r.stdout) if d]


def check_logging(section, where):
    failures = []
    default = (section or {}).get("default")
    if not default:
        failures.append(f"{where}: no `logging.default` -- the default level is unstated, so it "
                        f"cannot be shown to be info or less ({WHY})")
    for target, cfg in (section or {}).items():
        for key in ("console_level", "file_level"):
            level = (cfg or {}).get(key)
            if level is None:
                continue
            if target == "default" and verbosity(level) > ORDER["info"]:
                failures.append(f"{where}: logging.default.{key}={level} is more verbose than info ({WHY})")
            elif is_sensitive(target) and verbosity(level) > ORDER["info"]:
                failures.append(f"{where}: logging.{target}.{key}={level} -- {WHY}")
    return failures


def check_rust_log(value, where):
    failures = []
    for directive in filter(None, (d.strip() for d in str(value).split(","))):
        if directive.lower() in ORDER:  # bare level
            if verbosity(directive) > ORDER["info"]:
                failures.append(f"{where}: RUST_LOG={value} sets the global level above info ({WHY})")
            continue
        target, _, level = directive.rpartition("=")
        if not target or level.lower() not in ORDER:
            # `pingora` alone: a target with no level is TRACE for it.
            target, level = directive, "trace"
        if is_sensitive(target) and verbosity(level) > ORDER["info"]:
            failures.append(f"{where}: RUST_LOG directive {directive} -- {WHY}")
    return failures


# gears.logLevel values the chart must refuse without the opt-in.
REFUSED = ["debug", "TRACE", "5", "info,pingora=debug", "warn,oagw=trace",
           "info,pingora_core::protocols=debug", "info,cf_gears_oagw=debug",
           "info,api-gateway=debug", "info,pingora", "info,[request]=debug",
           # EnvFilter enables a directive for every event whose target STARTS
           # WITH the directive's target, so a prefix of a protected crate
           # covers it: `ping` reaches pingora, `cf_gears` cf_gears_oagw, `api`
           # api_gateway, `oag` oagw.
           "info,ping=debug", "info,cf_gears=debug", "info,api=trace", "info,oag=debug"]
# ...and values it must keep accepting: other targets are out of scope.
ACCEPTED = ["info", "warn", "info,qa_runs=debug,sqlx=trace", "info,pingora=info"]
OPT_IN = "gears.allowVerboseProxyLogs"


def check_refusal():
    failures = []
    for value in REFUSED:
        r = helm_template([f"gears.logLevel={value}"])
        if r.returncode == 0:
            failures.append(f"gears.logLevel={value} rendered -- the chart must refuse it without "
                            f"{OPT_IN}=true ({WHY})")
        elif OPT_IN not in r.stderr:
            failures.append(f"gears.logLevel={value} failed, but not on the verbose-log refusal "
                            f"(no {OPT_IN} in the message): {r.stderr.strip()}")
        r = helm_template([f"gears.logLevel={value}", f"{OPT_IN}=true"])
        if r.returncode != 0:
            failures.append(f"gears.logLevel={value} with {OPT_IN}=true must render (the opt-in "
                            f"has to work): {r.stderr.strip()}")
    # The refusal lives in one helper; a template reading the value directly
    # would bypass it while another site's refusal kept this check green.
    for tpl in sorted((CHART / "templates").glob("*.yaml")):
        if ".Values.gears.logLevel" in tpl.read_text():
            failures.append(f"{tpl.name} reads .Values.gears.logLevel directly -- use "
                            "`include \"qa-platform.rustLog\" .`, which carries the refusal")
    for value in ACCEPTED:
        r = helm_template([f"gears.logLevel={value}"])
        if r.returncode != 0:
            failures.append(f"gears.logLevel={value} must render -- it leaves default/pingora/oagw/"
                            f"api-gateway at info or less: {r.stderr.strip()}")
    return failures


def check_parser():
    """This file's own RUST_LOG reader must agree with the chart's: it is what
    judges every rendered RUST_LOG, so a shape the chart refuses and this
    reader accepts is a hole in the second line of defence."""
    failures = []
    for value in REFUSED:
        if not check_rust_log(value, "parser"):
            failures.append(f"check_rust_log accepts {value!r}, which the chart refuses ({WHY})")
    for value in ACCEPTED:
        if check_rust_log(value, "parser"):
            failures.append(f"check_rust_log refuses {value!r}, which is out of scope")
    return failures


def main():
    docs = render()
    failures, configs, env_seen = check_refusal() + check_parser(), 0, 0
    for d in docs:
        name = f"{d.get('kind')}/{d.get('metadata', {}).get('name')}"
        for fname, text in ((d.get("stringData") or {}) | (d.get("data") or {})).items():
            if fname == "qa-platform-stack.yaml":
                configs += 1
                failures += check_logging(yaml.safe_load(text).get("logging"), f"{name} {fname}")
        pod = (d.get("spec") or {}).get("template") or {}
        for c in (pod.get("spec") or {}).get("containers", []):
            for e in c.get("env", []):
                if e.get("name") == "RUST_LOG":
                    env_seen += 1
                    failures += check_rust_log(e.get("value", ""), f"{name} container {c['name']}")
    if configs == 0:
        failures.append("no rendered qa-platform-stack.yaml with a logging section -- the guard would pass on nothing")
    if failures:
        for f in failures:
            print(f"FAIL: {f}")
        return 1
    print(f"OK: {configs} gears config and {env_seen} RUST_LOG env keep default/pingora/oagw/api-gateway at info or less")
    return 0


if __name__ == "__main__":
    sys.exit(main())
