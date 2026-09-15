"""The Dockerfile's ARG default is the canonical BASE feature list;
deploy/cargo-features.argo is the canonical ARGO list. Docker cannot read a file
into an ARG default, so the two cannot be collapsed into one -- this test enforces
the relationship instead: every base feature must appear in the argo list.

The drift this guards against has happened: `postgres-credstore` was added to the
Dockerfile ARG and would have been absent from every --argo build while looking
deployed (see deploy/cargo-features.argo)."""
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
DEPLOY = HERE.parent.parent
DOCKERFILE = DEPLOY / "docker" / "qa-platform.Dockerfile"
ARGO_FILE = DEPLOY / "cargo-features.argo"


def base_features():
    text = DOCKERFILE.read_text()
    m = re.search(r"^ARG CARGO_FEATURES=(\S+)$", text, re.M)
    assert m, f"no `ARG CARGO_FEATURES=` line in {DOCKERFILE}"
    return [f.strip() for f in m.group(1).split(",") if f.strip()]


def argo_features():
    return [f.strip() for f in ARGO_FILE.read_text().strip().split(",") if f.strip()]


def main():
    base, argo = base_features(), argo_features()
    missing = [f for f in base if f not in argo]
    if missing:
        print(f"FAIL: base features absent from cargo-features.argo: {missing}")
        return 1
    if "qa-runs-argo" not in argo:
        print("FAIL: cargo-features.argo does not enable qa-runs-argo")
        return 1
    print(f"PASS: {len(base)} base features all present; argo list adds "
          f"{sorted(set(argo) - set(base))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
