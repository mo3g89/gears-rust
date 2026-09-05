"""pytest plugin emitting the log markers qa-runs' Argo adapter parses.

WHY A PLUGIN IN THE IMAGE AND NOT IN THE TEST REPOSITORY. The markers are the
adapter's contract, not the suite's: a repository that had to carry a
`conftest.py` hook for them could not be run by this platform without being
modified for it. Shipped with the runner and loaded with `-p` rather than by
discovery, so it is loaded even when the bundle has a `conftest.py` of its own
-- which, since 2026-08-27, it usually does (a bundle is the repository's whole
content root; see `qa-catalog/.../domain/service/bundles.rs`, `build_bundle`).
An earlier version of this paragraph gave the *absence* of a `conftest.py` as
the reason, which stopped being true when bundles started carrying one.

PATHS ARE REPORTED RELATIVE TO THE BUNDLE ROOT, NOT TO pytest's ROOTDIR. The
entrypoint runs pytest from the directory holding the suite's config file, which
may be below the bundle root, so pytest's own nodeids are relative to that
directory while the plan, the bundle and `qa_run_test_results.test_file` are all
relative to the bundle root. `QA_RUNNER_NODEID_PREFIX` (empty when the two
coincide) and `QA_RUNNER_BUNDLE_ROOT` are what `_rebase` uses to close the gap.
Getting this wrong does not fail a run -- it records rows under paths no plan
mentions, which is worse, because the run looks fine and the analytics universe
silently does not match it.

WHAT IS EMITTED, AND WHAT DELIBERATELY IS NOT. One `=== TEST_CASE: <base64
json> ===` marker per test, and nothing else. The adapter's parser also
understands a file-level `TEST_START`/`TEST_RESULT` pair, and the source system
emits one; this does not, because `qa_run_test_results` is unique on
`(tenant_id, run_id, test_file, test_name)`
(`qa-runs/.../infra/storage/entity/run_test_result.rs:8`), so a summary row
named after the FILE is a separate row from the per-test rows and inflates
every count by one per file. A run whose pytest collects nothing then produces
no observation at all -- which qa-runs already handles correctly and loudly:
`apply_completion_guard` turns a would-be success with zero results into
`failed` (`domain/service/ingest.rs:265-271`).

THE OUTCOME VOCABULARY is the parser's, mapped in
`infra/executor/argo/markers.rs`'s `status_of`: passed/failed/skipped/
xfailed/xpassed/error. Anything else is uppercased and stored verbatim, so an
unmapped word is not an error -- it is a status nothing counts.
"""

import base64
import json
import os
import sys

# See the module doc: the prefix from the bundle root down to pytest's rootdir,
# and the bundle root itself for the absolute-nodeid case. Read once, at import,
# because the entrypoint exports them before pytest starts and nothing may change
# them mid-run.
_NODEID_PREFIX = os.environ.get("QA_RUNNER_NODEID_PREFIX", "").strip("/")
_BUNDLE_ROOT = os.environ.get("QA_RUNNER_BUNDLE_ROOT", "")


def _rebase(nodeid):
    """A pytest nodeid as a path relative to the bundle root.

    Three shapes, and each is reachable:

    * absolute (`/work/tests/monitoring/test_x.py::t`) -- pytest reports the arg
      as given, and the entrypoint passes an absolute path for any test file that
      lies outside the config directory. Made relative to the bundle root.
    * prefixed -- the ordinary case: prepend the prefix.
    * no prefix -- the config directory IS the bundle root (the canary and smoke
      fixtures), and the nodeid is already right. Returned untouched, which is
      what keeps those runs byte-identical to before this function existed.
    """
    if nodeid.startswith("/"):
        if not _BUNDLE_ROOT:
            return nodeid
        try:
            return os.path.relpath(nodeid, _BUNDLE_ROOT)
        except ValueError:
            return nodeid
    if not _NODEID_PREFIX:
        return nodeid
    return "%s/%s" % (_NODEID_PREFIX, nodeid)


# The public name `collect_reporter` imports. Same function, exposed rather than
# copied: a collect-only run reports counts against the same bundle-relative
# coordinate these markers report results against, and two implementations of
# that rebasing would drift silently. `_rebase` stays the in-module name this
# module's own doc and call sites refer to.
rebase_to_bundle_root = _rebase

# A test can report three times (setup, call, teardown) and must produce ONE
# observation. Reported node ids are remembered so a teardown error after a
# recorded call outcome does not emit a second row that the unique
# (run, file, name) tuple would collapse into an arbitrary winner.
_reported = set()


def _emit(payload):
    token = base64.standard_b64encode(
        json.dumps(payload, separators=(",", ":")).encode("utf-8")
    ).decode("ascii")
    # THE LEADING NEWLINE IS LOAD-BEARING, and it was not there at first. The
    # adapter's pattern is ANCHORED -- `^=== TEST_CASE: (\S+) ===$` -- and
    # pytest's terminal reporter writes its progress line ("tests/x.py::t
    # PASSED [ 50%]") WITHOUT a trailing newline before this hook runs, so the
    # marker was emitted on the same line as `[ 50%]` and matched nothing.
    # Measured against a real pytest run, not reasoned about: every marker was
    # silently dropped and the run reported zero tests.
    #
    # Flushed per marker: the adapter FOLLOWS the pod log, and Python buffers
    # stdout when it is a pipe -- which a container's stdout is. Without this
    # every marker for a run arrives at once, at exit, and a long suite shows
    # no progress at all.
    sys.stdout.write("\n=== TEST_CASE: %s ===\n" % token)
    sys.stdout.flush()


def _first_line(value, limit=500):
    if value is None:
        return None
    text = str(value).strip()
    if not text:
        return None
    return text.splitlines()[0][:limit]


def _crash_message(report, fallback):
    """The line a human wants, not the first line of a traceback.

    `longreprtext`'s FIRST line is the source of the failing function
    (`def test_x():`) or a decorator, which says nothing -- that is what this
    function was written to stop reporting. pytest puts the useful line on
    `longrepr.reprcrash.message` ("RuntimeError: fixture blew up"); the fallback
    is the last non-empty line of the traceback text, which is where the
    exception lands when there is no `reprcrash` (a skip, or a collection error
    reported as a string).
    """
    crash = getattr(getattr(report, "longrepr", None), "reprcrash", None)
    if crash is not None and getattr(crash, "message", None):
        return _first_line(crash.message)
    text = str(fallback or "").strip()
    if not text:
        return None
    for line in reversed(text.splitlines()):
        if line.strip():
            return line.strip()[:500]
    return None


def _outcome_of(report):
    """pytest's report -> the runner's outcome word, or None to stay silent."""
    if getattr(report, "wasxfail", None) is not None:
        # An xfailed test reports outcome "skipped" at `call`; an xpassed one
        # reports "passed". Both carry `wasxfail`, which is the only thing that
        # distinguishes them from an ordinary skip or pass.
        return "xpassed" if report.outcome == "passed" else "xfailed"
    if report.when == "call":
        return report.outcome
    # setup / teardown. A skip raised in a fixture (or by
    # `pytest.mark.skip`) surfaces as a skipped SETUP report and never reaches
    # `call`, so it has to be emitted here or the test vanishes.
    if report.outcome == "skipped":
        return "skipped"
    if report.outcome == "failed":
        # Not "failed": a fixture that blows up is an ERROR in every reporting
        # vocabulary this platform has, including the column comment on
        # `qa_run_test_results.status`.
        return "error"
    return None


def pytest_runtest_logreport(report):
    outcome = _outcome_of(report)
    if outcome is None:
        return
    if report.nodeid in _reported:
        return
    _reported.add(report.nodeid)

    reason = None
    if outcome in ("failed", "error"):
        reason = _crash_message(report, report.longreprtext or report.longrepr)
    elif outcome in ("skipped", "xfailed"):
        wasxfail = getattr(report, "wasxfail", None)
        if wasxfail:
            reason = _first_line(wasxfail)
        elif isinstance(report.longrepr, tuple) and len(report.longrepr) == 3:
            # pytest's skip longrepr is (path, lineno, "Skipped: <reason>").
            reason = _first_line(report.longrepr[2])

    nodeid = _rebase(report.nodeid)
    _emit(
        {
            "nodeid": nodeid,
            # `file` is left to the adapter to derive from `nodeid` only when
            # absent; supplying it explicitly keeps a parametrised id
            # (`f.py::t[a::b]`) from being split on the wrong `::`.
            "file": nodeid.split("::")[0],
            "name": nodeid.split("::", 1)[-1],
            "outcome": outcome,
            "duration": round(getattr(report, "duration", 0.0) or 0.0, 3),
            "reason": reason,
        }
    )


def pytest_collectreport(report):
    """A file that cannot even be imported must still produce a row.

    Without this a syntax error in a test file yields a red run with no test
    results at all, which reads as "the runner never started" rather than as
    "this file is broken".
    """
    if report.outcome != "failed":
        return
    if report.nodeid in _reported:
        return
    _reported.add(report.nodeid)
    nodeid = _rebase(report.nodeid)
    _emit(
        {
            "nodeid": nodeid,
            "file": nodeid,
            "name": "collection",
            "outcome": "error",
            "duration": 0.0,
            "reason": _crash_message(report, report.longreprtext or report.longrepr),
        }
    )
