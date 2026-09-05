"""pytest plugin reporting exact collected case counts for a `COLLECT_ONLY` run.

WHY A SECOND PLUGIN AND NOT A BRANCH IN `pytest_markers`. A collect-only run
must emit no `TEST_CASE` marker at all: the adapter turns every one of them into
a row in `qa_run_test_results` (`qa-runs/.../infra/executor/argo/markers.rs`),
and a collect run has executed no test -- rows for it would be results nothing
measured. `pytest_markers` emits one from `pytest_collectreport` by design (a
file that cannot even be imported must still produce a row), which is exactly
the case a collect-only run hits most often, so the collect path loads THIS
plugin INSTEAD of that one. The source system draws the same line: its collect
branch loads only `-p vhp_case_reporter`
(`vhp-testrunner/runner/entrypoint.sh:780`).

WHAT IS EMITTED. One

    === COLLECT_COUNT: <bundle-relative file> <n> ===

per file that collected at least one case. `<file>` is bundle-root-relative --
the coordinate a plan, `qa_test_case_collect.test_file` and
`qa_run_test_results.test_file` all speak -- and `<n>` counts pytest ITEMS, so
parametrize is expanded. That expansion is the entire reason this run kind
exists: a static per-file estimate is what the catalog already has, and
`POST /qa/v1/analytics/collect`'s own description is "exact test-case counts
(pytest --collect-only, parametrize expanded)".

COUNTED HERE RATHER THAN GREPPED IN THE SHELL. The count is
`len(session.items)` grouped by file, not a `grep -c` over marker text as the
source system does (`entrypoint.sh:783`) -- so a log line that happens to look
like a marker cannot change a number, and a file that collects zero cases is
distinguishable from a file whose markers failed to print.

PER-CASE MARKERS ARE OPT-IN, a deliberate divergence from the source system,
which emits one `=== TEST_CASE_COLLECTED: <nodeid> ===` per case
unconditionally. Two reasons: nothing needs them now that the count is computed
above, and a finished run's log is PERSISTED here (`qa_run_logs`, whereas
legacy's lived in a process buffer), so ~1400 extra lines per repository per
hour is storage that a debugging aid does not justify. `pytest --collect-only
-q` already lists every collected nodeid, which is the same information.
`QA_RUNNER_COLLECT_VERBOSE=1` turns the markers back on.
"""

import os
import sys

# The one authority for bundle-relative paths, shared with the marker plugin
# rather than reimplemented: two copies of this rebasing would drift, and the
# consequence of drift is counts recorded under paths no plan mentions -- the
# failure `pytest_markers`' own doc calls out as worse than an error, because
# nothing looks broken. Imported, not registered: importing a module does not
# make pytest collect its hooks, so `pytest_markers`' `TEST_CASE` emitters stay
# inert on this path.
from pytest_markers import rebase_to_bundle_root


def _verbose():
    return (os.environ.get("QA_RUNNER_COLLECT_VERBOSE", "").strip().lower()
            not in ("", "0", "false", "no"))


def _write(line):
    # THE LEADING NEWLINE IS LOAD-BEARING, for the reason `pytest_markers._emit`
    # records: the adapter's patterns are anchored, and pytest's terminal
    # reporter leaves a line open (no trailing newline) around hook time, which
    # silently appended the marker to it and matched nothing. Flushed because a
    # container's stdout is a pipe and the adapter follows it live.
    sys.stdout.write("\n%s\n" % line)
    sys.stdout.flush()


def pytest_collection_finish(session):
    """Emit one count per collected file, once collection is complete.

    Guarded on `--collect-only` even though the entrypoint loads this plugin
    only on that path: an operator who adds `-p collect_reporter` through
    `QA_RUNNER_PYTEST_ARGS` on an ordinary run should get their tests run and
    no counts, not counts for a run that is about to execute.
    """
    if not session.config.getoption("collectonly", default=False):
        return

    counts = {}
    for item in session.items:
        # Split BEFORE rebasing: `rebase_to_bundle_root` reasons about paths,
        # and a nodeid's `::test[param]` suffix is not part of one.
        test_file = rebase_to_bundle_root(item.nodeid.split("::")[0])
        counts[test_file] = counts.get(test_file, 0) + 1
        if _verbose():
            _write("=== TEST_CASE_COLLECTED: %s ===" % item.nodeid)

    for test_file in sorted(counts):
        _write("=== COLLECT_COUNT: %s %d ===" % (test_file, counts[test_file]))
