"""No specific deployment's address may be a default anywhere under deploy/.

# Why this test exists

The chart shipped with `publicOrigin: https://10.136.20.200` and
`clusterDns: 10.43.0.10` as values.yaml DEFAULTS, and `deploy-k8s.sh` defaulted
its `--target` and `--public-origin` to the same host. That made a bare
`helm install` or a bare `deploy-k8s.sh` point at one particular development
node -- building a UI bundle and a TLS certificate for someone else's hostname,
and pointing nginx's resolver at one cluster's kube-dns ClusterIP.

None of that failed at install time. It failed later, as a broken login or a 502
on every API call, with nothing in the error naming the cause.

# What is and is not a finding

Only VALUES are checked -- the right-hand side of a YAML key, and a shell
assignment's value. Prose is not: a comment explaining why a default was removed
is documentation, and one that quotes the old value is better documentation. The
scanners below strip comments before matching for exactly that reason.

`deploy/remote/*kubeconfig*.yaml` is not scanned. It is real cluster credentials,
it is covered by a `.gitignore` glob at the repo root, and it is never committed.

# Adding an address

If a new default legitimately needs an address, it is almost certainly wrong --
prefer a REQUIRED value, or discovery at run time from something the environment
already knows (the way the UI derives nginx's resolver from its own
/etc/resolv.conf). If it is genuinely right, add it to ALLOWED with the reason.
"""

import re
import sys
from pathlib import Path

DEPLOY = Path(__file__).resolve().parents[2]

# Documentation-only trees. These describe measurements taken against a specific
# cluster and are historical records, not configuration any deploy reads.
SKIP_DIRS = {".generated", ".generated-k8s", "node_modules", "__pycache__"}
SKIP_FILES = {
    "NOTES-hairpin.md",
    # This file. Its prose quotes the addresses it exists to ban, and a module
    # docstring is a string rather than a comment, so strip_comments cannot see it.
    "test_no_environment_hardcode.py",
}

# An IPv4 literal that is not a documentation/loopback/link-local address, and
# not one of the well-known cluster defaults a chart may legitimately mention.
IPV4 = re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b")

ALLOWED = {
    # Loopback and unspecified.
    "127.0.0.1", "0.0.0.0", "255.255.255.255",
    # Docker's embedded DNS. Not a deployment's address -- a fixed, documented
    # constant of the Docker runtime itself.
    "127.0.0.11",
    # RFC 5737 documentation ranges, which is what examples should use.
    # (Matched by prefix below, not listed exhaustively.)
}
ALLOWED_PREFIXES = ("192.0.2.", "198.51.100.", "203.0.113.", "127.")


def is_allowed(addr: str) -> bool:
    if addr in ALLOWED:
        return True
    if addr.startswith(ALLOWED_PREFIXES):
        return True
    # Version-like strings (1.36.3) and other non-addresses slip through IPV4
    # only if they have four parts, so a three-part version never reaches here.
    parts = addr.split(".")
    return any(int(p) > 255 for p in parts if p.isdigit())


def strip_comments(text: str, suffix: str) -> str:
    """Blank out comment bodies, keeping line structure so numbers still line up."""
    out = []
    for line in text.splitlines():
        if suffix in {".yaml", ".yml", ".sh", ".envsh", ".py", ".argo"}:
            # A '#' inside a quoted string is vanishingly rare in this tree and
            # over-stripping only ever LOSES a finding, never invents one -- so
            # the naive split is the safe direction to be wrong in.
            line = line.split("#", 1)[0]
        out.append(line)
    return "\n".join(out)


def scan() -> list[str]:
    failures = []
    for path in sorted(DEPLOY.rglob("*")):
        if not path.is_file():
            continue
        if any(part in SKIP_DIRS for part in path.parts):
            continue
        if path.name in SKIP_FILES:
            continue
        if "kubeconfig" in path.name:
            continue
        if path.suffix not in {".yaml", ".yml", ".sh", ".envsh", ".py", ".argo", ".json"}:
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            failures.append(f"FAIL: cannot read {path}: {exc}")
            continue

        code = strip_comments(text, path.suffix)
        for lineno, line in enumerate(code.splitlines(), start=1):
            for addr in IPV4.findall(line):
                if is_allowed(addr):
                    continue
                rel = path.relative_to(DEPLOY.parent)
                failures.append(
                    f"FAIL: {rel}:{lineno} carries the address {addr} as a VALUE.\n"
                    f"       {line.strip()}\n"
                    f"       A specific deployment's address must not be a default. Make it a\n"
                    f"       required value, or discover it at run time. If it is genuinely\n"
                    f"       right, add it to ALLOWED in this test with the reason."
                )
    return failures


def main() -> int:
    failures = scan()
    if failures:
        print("\n".join(failures))
        print(f"\n{len(failures)} hardcoded address(es) found under deploy/.")
        return 1
    print("PASS: no deployment-specific addresses are defaults under deploy/")
    return 0


if __name__ == "__main__":
    sys.exit(main())
