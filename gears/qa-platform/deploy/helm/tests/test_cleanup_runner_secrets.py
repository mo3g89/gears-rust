"""deploy/argo/cleanup-runner-secrets.sh selects only the retired writer's
Secrets (finding #94 final review).

The writer named every Secret `{readable}-{digest}`: `readable` is
`qa-platform-` plus the tenant UUID truncated to fit 63 characters (34 of its
36), `digest` 16 hex characters, and it set no labels. The chart's own Secrets
(qa-platform-postgres, -gears-config, -keycloak-admin, -realm, -tls) are Opaque
and unlabelled too in some installs, and `argo.namespace` may equal the release
namespace, so the selector must be the name shape plus "no labels at all", and
the script must refuse a namespace that holds the release.

A fake `kubectl` on PATH serves a Secret list and records delete calls.
"""
import json
import os
import pathlib
import subprocess

SCRIPT = pathlib.Path(__file__).resolve().parents[2] / "argo" / "cleanup-runner-secrets.sh"

WRITER = "qa-platform-00000000-df51-5b42-9538-d2b56b7ee9-0123456789abcdef"
WRITER_2 = "qa-platform-11111111-2222-4333-8444-5555555555-fedcba9876543210"
LEGACY = "qa-platform-smoke-fixture-kubeconfig"
CHART = ["qa-platform-postgres", "qa-platform-keycloak-admin", "qa-platform-realm", "qa-platform-tls"]

FAKE_KUBECTL = r"""#!/usr/bin/env bash
if [ "$1" = get ]; then cat "$FAKE_SECRETS"; exit 0; fi
if [ "$1" = delete ]; then shift; echo "$@" >> "$FAKE_DELETES"; exit 0; fi
echo "unexpected kubectl $*" >&2; exit 2
"""


def secret(name, labels=None, type_="Opaque"):
    meta = {"name": name}
    if labels is not None:
        meta["labels"] = labels
    return {"metadata": meta, "type": type_}


def run(tmp_path, items, *args):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    kubectl = bin_dir / "kubectl"
    kubectl.write_text(FAKE_KUBECTL)
    kubectl.chmod(0o755)
    secrets = tmp_path / "secrets.json"
    secrets.write_text(json.dumps({"items": items}))
    deletes = tmp_path / "deletes.txt"
    deletes.write_text("")
    env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}",
               FAKE_SECRETS=str(secrets), FAKE_DELETES=str(deletes))
    out = subprocess.run(["bash", str(SCRIPT), "argo", *args], capture_output=True, text=True, env=env)
    return out, deletes.read_text()


def everything():
    return [
        secret(WRITER),
        secret(WRITER_2, labels={}),
        secret(LEGACY),
        secret("qa-platform-qa-environments-credential-0123456789abcdef01234567"),
        secret(WRITER.replace("0123456789abcdef", "0123456789abcdeg")),
        secret("qa-platform-99999999-df51-5b42-9538-d2b56b7ee9-aaaaaaaaaaaaaaaa", labels={"team": "x"}),
        secret("qa-platform-99999999-df51-5b42-9538-d2b56b7ee9-bbbbbbbbbbbbbbbb", type_="kubernetes.io/tls"),
        *[secret(n) for n in CHART],
        secret("argo-postgres-config"),
    ]


def test_lists_only_the_writers_name_shape(tmp_path):
    out, deletes = run(tmp_path, everything())
    assert out.returncode == 0, out.stderr
    listed = [line for line in out.stdout.splitlines() if line.startswith("qa-platform-")]
    assert sorted(listed) == sorted([WRITER, WRITER_2])
    assert deletes == ""
    assert "--delete" in out.stdout


def test_points_the_operator_at_other_qa_platform_secrets(tmp_path):
    out, _ = run(tmp_path, everything())
    assert "review" in out.stdout and "by hand" in out.stdout
    review = [line.strip() for line in out.stdout.splitlines() if line.startswith("  ")]
    assert LEGACY in review
    for name in CHART:
        assert name in review, "unlabelled chart Secrets are shown for review, never selected"


def test_deletes_only_the_writers_secrets(tmp_path):
    out, deletes = run(tmp_path, everything(), "--delete")
    assert out.returncode == 0, out.stderr
    deleted = deletes.split()
    for name in (WRITER, WRITER_2):
        assert name in deleted
    for name in [LEGACY, *CHART]:
        assert name not in deleted


def test_refuses_a_namespace_holding_the_release(tmp_path):
    for marker in (secret("qa-platform-gears-config"),
                   secret("sh.helm.release.v1.qa-platform.v7", labels={"owner": "helm"},
                          type_="helm.sh/release.v1")):
        for args in ((), ("--delete",)):
            out, deletes = run(tmp_path, [*everything(), marker], *args)
            assert out.returncode == 1, (marker, args, out.stdout)
            assert "release" in out.stderr
            assert deletes == ""


def test_nothing_to_remove(tmp_path):
    out, deletes = run(tmp_path, [secret(LEGACY)])
    assert out.returncode == 0
    assert "nothing to remove" in out.stdout
    assert deletes == ""
