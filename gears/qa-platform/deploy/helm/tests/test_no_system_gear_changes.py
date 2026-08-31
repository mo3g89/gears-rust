"""qa-platform must not modify gears/system/authz-resolver or
gears/system/event-broker.

Both were modified once and reverted -- see
docs/FOOTPRINT-OUTSIDE-QA-PLATFORM.md. authz-resolver carried a
`system_grants` config surface bolted onto static-authz-plugin; event-broker
carried a config stanza that existed only to satisfy qa-insights' now-deleted
`event_broker` dependency. This fails the build if either comes back, because
the cost of finding out at review time is a rewrite of whatever depended on
it.

This test is NOT wired into any CI workflow or Makefile -- run it by hand
(`python -m pytest tests/test_no_system_gear_changes.py -v` from this
directory) whenever a change touches `gears/system/authz-resolver` or
`gears/system/event-broker` on this branch. Nothing stops a future PR from
silently reintroducing drift until someone does.
"""
import subprocess
from pathlib import Path

# UPSTREAM_BASE is a fixed commit, not a moving target: the point in history
# where static-authz-plugin and event-broker were last known-good upstream --
# specifically, the commit immediately before qa-platform's now-reverted
# changes landed on them. It is a pin, not "current upstream", on purpose:
# the property this test checks is "identical to that known-good snapshot",
# which only a fixed commit can express. A merge-base against a local branch
# (e.g. `main`) was considered instead and rejected -- it would either read a
# `main` ref nobody fetched (silently comparing against a stale snapshot
# no more current than this pin) or require a fetch this test cannot demand
# without becoming network-dependent and flaky. A fixed SHA fails loudly and
# predictably instead.
#
# What a failure means, and what is NOT the same fix for both causes:
#
#   1. qa-platform-branch work touched one of these gears again. This is the
#      failure this test exists to catch. Fix it the way this branch fixed it
#      the first time: move whatever qa-platform needs into qa-platform's own
#      gear, behind a named seam (see any of the three `domain::elevated`
#      modules under `gears/qa-platform/*/src/domain/elevated.rs` for the
#      precedent), and leave these two gears untouched.
#   2. Upstream legitimately changed one of these gears for a reason that has
#      nothing to do with qa-platform (a real fix or feature landed on
#      `gears/system/authz-resolver` or `gears/system/event-broker`). This is
#      a false positive from this test's point of view, not drift to reverse.
#      Re-pin rather than delete the guard:
#        a. Identify the new commit to pin -- normally the upstream commit
#           that introduced the legitimate change, or the current tip of the
#           branch that owns these gears.
#        b. Run `git diff <old UPSTREAM_BASE> <candidate> -- gears/system/authz-resolver
#           gears/system/event-broker` and read every line. Confirm the diff
#           is exactly the legitimate change and carries nothing shaped like
#           a qa-platform coupling (a `system_grants` entry, a qa-platform
#           consumer-group key, anything naming a qa-platform gear). If it
#           does, that is case 1 wearing case 2's clothes -- stop and fix it
#           as case 1 instead of re-pinning over it.
#        c. Update UPSTREAM_BASE to the new commit. GUARDED does not need to
#           change for this.
#      Deleting this test is never the fix for a red run under case 2 --
#      re-pinning preserves the property; deleting removes it.
UPSTREAM_BASE = "db7660030"
GUARDED = [
    "gears/system/authz-resolver",
    "gears/system/event-broker",
]

# git diff pathspecs are resolved relative to the process's cwd, not the repo
# root -- pytest's cwd depends on where it was invoked from, so GUARDED's
# repo-root-relative paths would silently match nothing (empty diff, exit 0,
# a false PASS) unless `cwd` is pinned here explicitly.
REPO_ROOT = Path(__file__).resolve().parents[5]


def test_system_gears_are_untouched():
    for path in GUARDED:
        diff = subprocess.run(
            ["git", "diff", "--stat", UPSTREAM_BASE, "--", path],
            capture_output=True, text=True, check=True, cwd=REPO_ROOT,
        ).stdout.strip()
        assert diff == "", (
            f"{path} differs from upstream {UPSTREAM_BASE}:\n{diff}\n\n"
            "qa-platform must absorb what it needs on its own side -- see each "
            "gear's domain::elevated module for the authz precedent."
        )
