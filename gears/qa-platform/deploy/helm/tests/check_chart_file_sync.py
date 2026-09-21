"""Every file the chart COPIES from outside its own tree must match its source.

Helm's `.Files` is scoped to the chart's directory and refuses `../`, so a
template that embeds a file living elsewhere in the repo can only read a copy
kept inside the chart. Two copies of one file is the shape that already caused a
real incident here (see the commit before this test's first version, which
replaced an unguarded second copy of the argo feature list with
check_features.py's subset check), so each such pair gets a row below.

# Most of this test's rows are gone, and that is the point

It used to carry four pairs sourced from `deploy/compose/`: the initdb script,
keycloak-tls/gen-cert.sh, ui-tls/tls.conf and seed-tenant.sh. The compose stack
has been deleted, so the chart's copies are now the ONLY copies -- canonical, not
duplicates -- and there is nothing left to drift from. A row asserting a file
matches a deleted source is not a weaker guard, it is a broken one.

What remains is the one pair that never came from compose: the gears' stack
config, whose source of truth is gears/qa-platform/config/qa-platform-stack.yaml
and which hits the same `.Files.Get` boundary. Add a row whenever a template
starts embedding another out-of-tree file.

Originally test_initdb_sync.py, covering only the initdb script."""
import difflib
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
DEPLOY = HERE.parent.parent
CONFIG = DEPLOY.parent / "config"
CHART_FILES = DEPLOY / "helm" / "qa-platform" / "files"

# (source of truth, chart copy under helm/qa-platform/files/).
PAIRS = [
    (
        CONFIG / "qa-platform-stack.yaml",
        CHART_FILES / "qa-platform-stack.yaml",
    ),
]


def check_pair(source, chart_copy):
    """Returns (ok, message)."""
    if not source.is_file():
        return False, f"FAIL: source {source} does not exist"
    if not chart_copy.is_file():
        return False, f"FAIL: chart copy {chart_copy} does not exist"
    source_text = source.read_text()
    chart_text = chart_copy.read_text()
    if source_text != chart_text:
        diff = "".join(
            difflib.unified_diff(
                source_text.splitlines(keepends=True),
                chart_text.splitlines(keepends=True),
                fromfile=str(source),
                tofile=str(chart_copy),
            )
        )
        return False, (
            f"FAIL: chart's copy of {chart_copy.name} has drifted from "
            f"{source}:\n{diff}"
        )
    return True, (
        f"PASS: {chart_copy} is byte-identical to {source} "
        f"({len(source_text)} bytes)"
    )


def main():
    ok = True
    for source, chart_copy in PAIRS:
        pair_ok, message = check_pair(source, chart_copy)
        print(message)
        ok = ok and pair_ok
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
