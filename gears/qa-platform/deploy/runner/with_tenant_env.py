#!/usr/local/bin/python3 -I
"""Exec argv with the tenant environment launch.py saved, overlaid by this
process' own (platform) environment, so PYTHONPATH, PATH, HOME and the
QA_RUNNER_* names the run script exported win over a tenant value.

The tenant environment is applied here, at pytest, and nowhere earlier: the
credential fetch, the bundle fetch, bundle_layout.py and pip run without it
(launch.py says why). Reserved names were refused at
launch and dropped from stored variables by qa-runs (params.rs is_reserved).
"""
import json
import os
import sys


def main(argv):
    if len(argv) < 4 or argv[2] != "--":
        sys.stderr.write("usage: with_tenant_env.py <tenant-env-file> -- <argv...>\n")
        return 2
    with open(argv[1]) as source:
        env = json.load(source)
    env.update(os.environ)
    env.pop("QA_RUNNER_TENANT_ENV", None)
    os.execvpe(argv[3], argv[3:], env)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
