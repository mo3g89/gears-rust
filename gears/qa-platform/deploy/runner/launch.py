#!/usr/local/bin/python3 -I
"""The runner container's entrypoint (installed as /entrypoint.sh).

The pod's environment mixes the platform's names with every run parameter,
plugin variable and stored variable. bash reads some names before it runs a
single line of a script: BASH_ENV, with expansion. So the run script cannot
protect itself, and its credential fetch must never start under that
environment: a run variable must not choose code that runs beside the run's
credentials.

This launcher runs with python3 -I, which ignores every PYTHON* variable. It:
  1. saves the whole environment to a private file, the tenant environment
     that with_tenant_env.py applies to pytest, and only to pytest;
  2. execs bash on the run script with PLATFORM_NAMES only, and without
     startup files.

Every name in PLATFORM_NAMES except HOME and PATH is on qa-runs' reserved
floor, so no run variable can set one (params.rs,
every_name_the_runner_launcher_keeps_is_reserved). The Argo adapter appends
HOME and PATH after the shared environment, and Kubernetes keeps the last
entry of a name.
"""
import json
import os
import sys

RUN_SCRIPT = "/opt/qa-runner/run.sh"
TENANT_ENV_FILE = "/tmp/qa-runner-tenant-env.json"
PLATFORM_NAMES = (
    "COLLECT_ONLY",
    "HOME",
    "PATH",
    "QA_CREDENTIALS_TOKEN_FILE",
    "QA_CREDENTIALS_URL",
    "QA_CREDENTIAL_DIRS",
    "QA_RUNNER_COLLECT_VERBOSE",
    "QA_RUNNER_PYTEST_ARGS",
    "QA_RUNNER_WORKDIR",
    "TEST_BUNDLE_CA_CERT",
    "TEST_BUNDLE_REF",
    "TEST_BUNDLE_URL",
    "TEST_FILES",
    "VHP_COLLECT_URL",
)


def main(argv):
    script = argv[1] if len(argv) > 1 else RUN_SCRIPT
    tenant_file = argv[2] if len(argv) > 2 else TENANT_ENV_FILE
    fd = os.open(tenant_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as out:
        json.dump(dict(os.environ), out)
    os.chmod(tenant_file, 0o600)
    platform = {name: os.environ[name] for name in PLATFORM_NAMES if name in os.environ}
    platform["QA_RUNNER_TENANT_ENV"] = tenant_file
    os.execve("/bin/bash", ["bash", "--noprofile", "--norc", script], platform)


if __name__ == "__main__":
    main(sys.argv)
