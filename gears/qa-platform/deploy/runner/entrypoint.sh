#!/usr/bin/env bash
# The qa-platform pytest runner's entrypoint.
#
# `/entrypoint.sh` is the path `qa-runs.argo.runner_command` defaults to, which
# is in turn the source system's hardwired entrypoint
# (`manager/src/services/argo.rs:528`) -- so this file's LOCATION is part of the
# contract with the adapter, not an implementation detail.
#
# THE CONTRACT, in both directions:
#
#   In   TEST_FILES                 comma-separated paths, relative to the
#                                   bundle's CONTENT ROOT -- which is the archive
#                                   root only when the bundle declares no
#                                   `content_root` (see "THE ARCHIVE ROOT IS NOT
#                                   THE CONTENT ROOT" below). May be empty -- the
#                                   port says an empty list is not an error
#                                   (`run_executor.rs:431-436`).
#        TEST_BUNDLE_URL            where to fetch the tests, INCLUDING the
#                                   `?sig=` that authorises the fetch. There is
#                                   no credential beside it and no token
#                                   exchange: the three TEST_BUNDLE_TOKEN_URL /
#                                   _CLIENT_ID / _CLIENT_SECRET variables this
#                                   block used to list are gone, and so is the
#                                   Keycloak round trip fetch_bundle.py made
#                                   with them. See that file's header for what
#                                   that credential could reach from inside a
#                                   pod running tenant-authored pytest, and why
#                                   a per-bundle signature replaced it rather
#                                   than joining it.
#                                   NEVER echoed whole -- see below.
#        TEST_BUNDLE_REF            qa-catalog's opaque storage_ref. Logged for
#                                   correlation; not fetchable from a pod.
#        KUBECONFIG                 present when the run targets a platform.
#                                   Passed through to pytest, untouched here.
#        COLLECT_ONLY               "true" selects the collect branch below:
#                                   count cases, execute nothing.
#        VHP_COLLECT_URL            where that branch POSTs each exact count.
#                                   Absent on an ordinary run, and (see the
#                                   branch) absent-in-effect when qa-insights
#                                   built it without an origin.
#
#   Out  `=== TEST_CASE: <base64 json> ===` per test, from
#        /opt/qa-runner/pytest_markers.py. On the collect path, no `TEST_CASE`
#        at all (a run that executed nothing has no results) and one
#        `=== COLLECT_COUNT: <file> <n> ===` per file, from
#        /opt/qa-runner/collect_reporter.py. Everything else on stdout is a
#        log line the adapter forwards verbatim.
#
# EXIT CODE. pytest's own, except that 5 ("no tests collected") is turned into
# 1: a run that collected nothing must not look like a pass, and qa-runs' own
# guard would flag it anyway (`ingest.rs:265-271`) -- agreeing with that guard
# here means the workflow's phase and the run's state say the same thing. The
# collect branch has its own rule, stated there.
set -uo pipefail

WORK="${QA_RUNNER_WORKDIR:-/work}"
RUNNER_DIR=/opt/qa-runner

echo "runner: image=qa-platform-pytest-runner pwd=$(pwd) python=$(python3 --version 2>&1)"
echo "runner: TEST_FILES=${TEST_FILES:-<unset>}"
echo "runner: TEST_BUNDLE_REF=${TEST_BUNDLE_REF:-<unset>}"
# THE URL WITHOUT ITS QUERY STRING. `?sig=` carries the HMAC tag that is the
# entire access control on the bundle route, and this line is rendered in the
# run view for anyone who can read the run -- so echoing the variable verbatim
# would publish the tag. `${VAR%%\?*}` strips from the first `?` to the end;
# the marker is appended separately so "the adapter gave us no signature at
# all" (no marker) still reads differently from "there is one and it is
# hidden", because those are different misconfigurations and this line is the
# only witness to the first.
#
# The same discipline this block already applied to the OIDC client secret,
# which used to be printed here as a byte count. That variable no longer exists
# -- see the contract block above -- so the count is gone with it.
if [[ "${TEST_BUNDLE_URL:-}" == *\?* ]]; then
    bundle_url_state="${TEST_BUNDLE_URL%%\?*}?<redacted>"
else
    bundle_url_state="${TEST_BUNDLE_URL:-<unset>}"
fi
echo "runner: TEST_BUNDLE_URL=$bundle_url_state"
echo "runner: KUBECONFIG=${KUBECONFIG:-<unset>}"

# NO BUNDLE URL IS A HARD FAILURE, not a fall back to "run whatever is in the
# image". There is nothing in the image to run, and a runner that exits 0 with
# no tests is the exact failure this whole adapter exists to end.
if [[ -z "${TEST_BUNDLE_URL:-}" ]]; then
    echo "runner: TEST_BUNDLE_URL is unset -- there is no test content to fetch. Set qa-runs.argo.bundle_base_url." >&2
    exit 1
fi

mkdir -p "$WORK"
if ! python3 "$RUNNER_DIR/fetch_bundle.py" "$WORK"; then
    echo "runner: bundle fetch failed (its own diagnostic is above) -- no tests were run" >&2
    exit 1
fi

cd "$WORK" || { echo "runner: cannot cd to $WORK" >&2; exit 1; }

# ---------------------------------------------------------------------------
# THE ARCHIVE ROOT IS NOT THE CONTENT ROOT
#
# A bundle is written under the repository-relative `content_root` it was built
# from, so the extracted tree keeps the depth the repository has. `$CONTENT` is
# that content root; `$WORK` is where the archive was unpacked. Everything below
# -- layout discovery, test-file resolution, nodeid rebasing -- is relative to
# `$CONTENT`, which is what the coordinates in a plan and in
# `qa_run_test_results.test_file` have always meant.
#
# WHY THE ARCHIVE CARRIES THE PREFIX AT ALL. A suite may locate its checkout by
# counting parents (`Path(__file__).resolve().parents[4]`), which is correct in a
# checkout and was correct in the system this replaced: it cloned the whole
# repository and merely `cd`-ed into the tests root, leaving the ancestors on
# disk. An archive rooted AT the content root deleted them, and on run
# 1a9f0eb6-5e2f-4aa5-8eec-d577ed53a88c that expression raised `IndexError` at
# conftest import time -- pytest aborted the session and all 1465 collectable
# tests recorded nothing.
#
# A MISSING MARKER IS A VALID STATE, not an error: every bundle built before the
# marker existed, and every bundle whose content root already is the checkout
# root, has none -- and for both, the archive root IS the content root.
# ---------------------------------------------------------------------------
CONTENT="$WORK"
if [[ -f "$WORK/.qa-content-root" ]]; then
    content_rel=$(tr -d '\r\n' < "$WORK/.qa-content-root")
    if [[ -n "$content_rel" ]]; then
        if [[ -d "$WORK/$content_rel" ]]; then
            CONTENT="$WORK/$content_rel"
        else
            # Fail loudly. Falling back to $WORK would run pytest against a tree
            # that is missing the very prefix the marker announced, and the run
            # would look like a collection problem in the suite.
            echo "runner: bundle declares content root '$content_rel' but $WORK/$content_rel is not a directory" >&2
            exit 1
        fi
    fi
fi

# ---------------------------------------------------------------------------
# WHERE IN THE CONTENT ROOT DOES PYTEST RUN, AND WITH WHAT
#
# The suite's `pytest.ini`, `conftest.py`, shared packages and `requirements.txt`
# arrive at whatever depth the repository keeps them -- for the first real suite
# to reach this runner, one level down in `<content>/tests/`. Running pytest at
# the content root instead produced, on run
# 94978978-fa28-4650-a14d-2ce8f72dff49: seven collection errors
# (`ModuleNotFoundError: No module named 'lib'`), an unread ini file, and 14
# `PytestUnknownMarkWarning`s.
#
# Nothing here is hardcoded to `tests` -- bundle_layout.py derives it, and a
# bundle whose content root IS the config directory resolves to it and behaves
# exactly as this runner did before.
# ---------------------------------------------------------------------------
layout=$(python3 "$RUNNER_DIR/bundle_layout.py" "$CONTENT") || {
    echo "runner: could not inspect the bundle layout -- no tests were run" >&2
    exit 1
}
eval "$layout"
echo "runner: bundle root=$WORK content=$CONTENT config_dir=$QA_CONFIG_DIR config_file=${QA_CONFIG_FILE:-<none>} requirements=${QA_REQUIREMENTS:-<none>}"

# THE REPOSITORY'S OWN DEPENDENCIES. The image ships pytest and nothing else on
# purpose (see runner.Dockerfile), so a suite that imports `httpx` or `boto3`
# fails collection unless its declared requirements are installed here.
#
# A FAILED INSTALL EXITS. It must not fall through into pytest: the symptom
# would be a wall of ModuleNotFoundError collection errors whose stated cause is
# a missing module rather than a failed install, which is precisely the
# misdiagnosis this whole change exists to end.
#
# PyPI reachability was MEASURED from a pod in this namespace before choosing a
# runtime install: `https://pypi.org/simple/pytest/` answered 200 in 0.47 s and
# the whole ten-line requirements file installed in 16.5 s. See the report for
# what the alternative (baking wheels into the image) costs.
if [[ -n "${QA_REQUIREMENTS:-}" ]]; then
    echo "runner: installing $QA_REQUIREMENTS"
    if ! python3 -m pip install --no-cache-dir --disable-pip-version-check \
        --root-user-action=ignore -r "$QA_REQUIREMENTS"; then
        echo "runner: pip install -r $QA_REQUIREMENTS FAILED -- no tests were run. Either the pod cannot reach the package index or a requirement does not resolve; pip's own diagnostic is above." >&2
        exit 1
    fi
else
    echo "runner: the bundle declares no requirements file -- running with the image's pytest only"
fi

# The file list, comma-separated, empty entries dropped. Unquoted word
# splitting on a set IFS rather than `tr`+`read -a`, so a path containing a
# space survives (it splits on commas only).
declare -a FILES=()
if [[ -n "${TEST_FILES:-}" ]]; then
    old_ifs="$IFS"
    IFS=','
    for entry in ${TEST_FILES}; do
        [[ -n "$entry" ]] && FILES+=("$entry")
    done
    IFS="$old_ifs"
fi

# An empty TEST_FILES runs whatever the bundle contained. That is qa-catalog's
# "whole content root" bundle shape, and it is the only reading of an empty
# list that runs any tests at all.
if [[ ${#FILES[@]} -eq 0 ]]; then
    echo "runner: TEST_FILES is empty -- collecting the whole bundle"
    FILES=(".")
fi

# One `=== TEST_FILE: <path> ===` marker per requested path, before pytest
# starts. Printed as the BUNDLE-ROOT-relative path -- which is the path the plan
# names and the path `qa_run_test_results.test_file` stores -- and not as the
# config-dir-relative argument pytest is about to be given. A path in TEST_FILES
# that is NOT in the bundle is visible as a marker with no results after it.
#
# The same loop re-expresses each path against the config directory, because
# that is pytest's CWD from here on. A path outside the config directory (a
# repository with tests both inside and outside it) becomes absolute rather than
# being dropped; the marker plugin rebases either shape back to bundle-relative.
declare -a ARGS=()
for file in "${FILES[@]}"; do
    # Not on the collect path: `TEST_FILE` tells the adapter which file the
    # results that FOLLOW belong to, and a collect run produces none. The
    # collect branch prints `COLLECT_FILE` instead, as the source system does.
    if [[ "${COLLECT_ONLY:-false}" != "true" ]]; then
        echo "=== TEST_FILE: ${file} ==="
    fi
    if [[ ! -e "$CONTENT/$file" && "$file" != "." ]]; then
        echo "runner: WARNING ${file} is not present in the bundle" >&2
    fi
    if [[ -z "${QA_CONFIG_PREFIX:-}" ]]; then
        ARGS+=("$file")
    elif [[ "$file" == "$QA_CONFIG_PREFIX/"* ]]; then
        ARGS+=("${file#"$QA_CONFIG_PREFIX"/}")
    elif [[ "$file" == "." ]]; then
        ARGS+=(".")
    else
        ARGS+=("$CONTENT/$file")
    fi
done

cd "$QA_CONFIG_DIR" || { echo "runner: cannot cd to $QA_CONFIG_DIR" >&2; exit 1; }

# `-p` with an absolute-ish module name needs the directory on sys.path, and
# the CWD is the config directory, not the runner dir. PYTHONPATH is the
# documented way in; `-p pytest_markers` then imports it.
#
# THE CONFIG DIRECTORY IS ON PYTHONPATH FIRST, and that is the half that fixes
# the import error rather than the `cd`. pytest adds each test module's *basedir*
# to sys.path (the first ancestor without `__init__.py`), which for
# `tests/monitoring/test_x.py` is `tests/monitoring` -- not `tests`, and so not
# the directory that contains the `lib` package the modules import. Neither the
# CWD nor rootdir is added by pytest at all.
export PYTHONPATH="$QA_CONFIG_DIR:$RUNNER_DIR${PYTHONPATH:+:$PYTHONPATH}"

# What the marker plugin needs to report bundle-relative paths: with rootdir at
# the config directory, pytest's own nodeids are config-dir-relative
# (`monitoring/test_x.py::t`) while the plan, the bundle and
# `qa_run_test_results.test_file` all speak bundle-relative
# (`tests/monitoring/test_x.py`). Rebasing in the plugin rather than in the
# adapter keeps the correlation key in one place and leaves the frozen port
# untouched.
export QA_RUNNER_NODEID_PREFIX="${QA_CONFIG_PREFIX:-}"
export QA_RUNNER_BUNDLE_ROOT="$CONTENT"

echo "runner: pytest $(python3 -m pytest --version 2>&1 | head -n1) cwd=$(pwd)"

# ---------------------------------------------------------------------------
# COLLECT-ONLY RUNS: COUNT CASES, REPORT THEM, EXECUTE NOTHING
#
# `COLLECT_ONLY=true` (and `VHP_COLLECT_URL`, when a URL was supplied) is the
# runner-facing half of qa-runs' `RunKind::Collect`
# (`qa-runs/.../domain/runvars.rs:454`), frozen by the PRD together with
# the rest of the test-facing contract. Its producer is qa-insights' hourly
# collect ticker; its purpose is the analytics "expected cases" number, which
# stays a static per-file estimate until an exact, parametrize-expanded count
# is reported back to `POST /qa/v1/collect/{repo_id}`.
#
# THIS BRANCH EXISTS IN THE SOURCE SYSTEM AND WAS NOT PORTED WITH THE REST OF
# THIS FILE (`vhp-testrunner/runner/entrypoint.sh:773-786`, `report_collect` at
# `:679-698`). Without it every collect workflow fell through to the ordinary
# path below and tried to EXECUTE the whole bundle with no platform and no
# `KUBECONFIG`. Measured on `collect-d4addf78-...-58` (2026-09-02, the 58th
# consecutive hourly failure): 1371 tests collected, none run, exit 4 from a
# suite's own precondition hook -- a hook that skips itself outright under
# `--collect-only` (`tests/openbao/conftest.py`, `config.getoption
# ("collectonly")`). `qa_test_case_collect` had never held a single row.
#
# ONE PYTEST PROCESS PER FILE, unlike the ordinary path below, which
# deliberately runs one session for the whole run. A collect CYCLE must not:
# one unimportable file in a single session takes down the counts of every
# other file with it, which is precisely what happened above. The source system
# runs a process per file for every run kind, and the conftest quoted above
# states that as a fact about its environment ("The external runner starts one
# pytest process per file").
#
# EXIT CODE. 0 when at least one count was accepted, 1 when none was -- the
# same rule the `exit 5` rewrite at the bottom of this file applies, for the
# same reason: a cycle that delivered nothing must not read as a pass. A single
# uncollectable file is a WARNING that does not fail the cycle, and NO count is
# sent for it: overwriting a real count with a zero the collection never
# established would silently shrink the analytics universe.
# ---------------------------------------------------------------------------

# POST one exact count. Prints the outcome either way -- legacy swallows every
# failure here (`|| true` around its own reporter), which is how a deployment
# can run a collect cycle hourly for days with an unset
# `collect_report_base_url` and see nothing about it anywhere.
# Args: bundle-relative test file, case count. Exit status: 0 iff accepted.
report_collect() {
    python3 - "$VHP_COLLECT_URL" "$1" "$2" <<'PY'
import json
import sys
import time
import urllib.error
import urllib.request

url, test_file, count = sys.argv[1], sys.argv[2], int(sys.argv[3])
# `CollectCountReq` (`qa-insights/.../api/rest/dto.rs:2263`): exactly these two
# fields, and the same payload legacy's reporter sends. `branch`, `tenant_id`
# and the `sig` that authorises the write all ride the URL's query string,
# which qa-insights built and this runner never parses.
payload = json.dumps({"test_file": test_file, "case_count": count}).encode("utf-8")

# ONE RETRY, INCLUDING ON 403, and that is not the usual reading of a 403.
# MEASURED on the first cycle this branch ever ran (2026-09-03, run
# collect-...-74): the collect ticker fires at gear START-UP, the first nine
# reports of the cycle came back 403 in the ten seconds after the gears process
# began serving, and replaying one of those exact URLs -- same signature, same
# payload -- a few minutes later returned 200. So the refusal was a warm-up
# race in the gears, and nine files' counts were lost to it. A permanently
# unauthorised report costs one extra request per file and still reports the
# refusal; a transient one now costs nothing at all.
ATTEMPTS = 2
BACKOFF_SECONDS = 5

for attempt in range(1, ATTEMPTS + 1):
    request = urllib.request.Request(
        url,
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            retried = " (on retry)" if attempt > 1 else ""
            print(
                "runner: collect report %s=%d -> HTTP %s%s"
                % (test_file, count, response.status, retried)
            )
            break
    except urllib.error.HTTPError as error:
        detail = (error.read() or b"")[:300].decode("utf-8", "replace").replace("\n", " ")
        failure = "refused: HTTP %s %s" % (error.code, detail)
    except Exception as error:  # noqa: BLE001 -- any transport failure is one outcome here
        failure = "failed: %r" % (error,)

    if attempt < ATTEMPTS:
        print(
            "runner: collect report %s=%d %s -- retrying in %ds"
            % (test_file, count, failure, BACKOFF_SECONDS),
            file=sys.stderr,
        )
        time.sleep(BACKOFF_SECONDS)
        continue

    print(
        "runner: WARNING collect report %s=%d %s" % (test_file, count, failure),
        file=sys.stderr,
    )
    sys.exit(1)
PY
}

if [[ "${COLLECT_ONLY:-false}" == "true" ]]; then
    echo "runner: COLLECT_ONLY=true -- collecting cases, executing nothing"
    # A URL WITH NO ORIGIN IS THE MISCONFIGURATION THIS NAMES OUT LOUD.
    # `collect_url` builds a syntactically valid RELATIVE path when qa-insights'
    # `collect_report_base_url` is empty (that field's own doc says so), and the
    # only party that ever sees the consequence is this process. So it is said
    # here, once, with the setting to change.
    if [[ -z "${VHP_COLLECT_URL:-}" ]]; then
        echo "runner: VHP_COLLECT_URL is unset -- counts will be logged below and reported to nobody." >&2
    elif [[ "$VHP_COLLECT_URL" != http://* && "$VHP_COLLECT_URL" != https://* ]]; then
        echo "runner: VHP_COLLECT_URL='$VHP_COLLECT_URL' has no scheme or host, so no count can be reported. Set qa-insights' collect_report_base_url to an origin a workflow pod can reach the gears on -- the same value qa-runs' argo.bundle_base_url carries." >&2
        VHP_COLLECT_URL=""
    fi

    accepted=0
    refused=0
    uncollectable=0
    for index in "${!FILES[@]}"; do
        # The path as the plan names it, `::node-id` selector stripped: a
        # count belongs to a FILE, and `qa_test_case_collect` is keyed on one.
        requested="${FILES[$index]%%::*}"
        echo "=== COLLECT_FILE: ${requested} ==="

        # Streamed AND captured: the adapter follows this log live, so a
        # 230-file cycle must not go silent for minutes, and the counts have to
        # be read back out of the same output.
        capture=$(mktemp)
        python3 -m pytest \
            -p collect_reporter \
            --collect-only \
            -q \
            --color=no \
            -o cache_dir=/tmp/pytest_cache \
            "${ARGS[$index]}" 2>&1 | tee "$capture"
        rc=${PIPESTATUS[0]}

        counted=0
        while IFS=$'\t' read -r counted_file counted_cases; do
            [[ -z "$counted_file" ]] && continue
            counted=1
            if [[ -z "${VHP_COLLECT_URL:-}" ]]; then
                continue
            fi
            if report_collect "$counted_file" "$counted_cases"; then
                accepted=$((accepted + 1))
            else
                refused=$((refused + 1))
            fi
        done < <(sed -n 's/^=== COLLECT_COUNT: \(.*\) \([0-9][0-9]*\) ===$/\1\t\2/p' "$capture")
        rm -f "$capture"

        if [[ "$counted" -eq 1 ]]; then
            continue
        fi
        # No count for this argument. Two different facts, and they must not be
        # reported the same way: pytest 5 is "collected nothing", which for a
        # named file is a real zero worth recording (a file whose tests were
        # deleted has to stop counting toward expected cases). Anything else is
        # a collection FAILURE, and the last count reported for that file is
        # better evidence than a zero.
        if [[ "$rc" -eq 5 && "$requested" == *.py ]]; then
            echo "runner: ${requested} collected no cases (pytest exit 5) -- reporting 0"
            if [[ -n "${VHP_COLLECT_URL:-}" ]]; then
                if report_collect "$requested" 0; then
                    accepted=$((accepted + 1))
                else
                    refused=$((refused + 1))
                fi
            fi
        else
            uncollectable=$((uncollectable + 1))
            echo "runner: WARNING ${requested} produced no count (pytest exit $rc) -- nothing reported for it, so its last known count stands" >&2
        fi
    done

    echo "runner: collect summary: arguments=${#FILES[@]} accepted=$accepted refused=$refused uncollectable=$uncollectable"
    if [[ "$accepted" -eq 0 ]]; then
        echo "runner: no count was accepted -- reporting failure, because a collect cycle that delivered nothing is not a pass" >&2
        exit 1
    fi
    # A single accepted count used to be enough to exit 0, even with 220 of
    # 221 files refused or uncollectable -- the zero-results guard above only
    # catches TOTAL suppression. A partial collect cycle is not a pass either:
    # the caller (qa-insights) has no way to tell "everything was reported"
    # from "most of it was refused" apart from this exit code.
    if [[ "$refused" -gt 0 || "$uncollectable" -gt 0 ]]; then
        # One condition, not three: whichever of refused/uncollectable fired
        # (or both) exits non-zero the same way -- only the wording of what
        # is named differs, so it is composed here rather than branched on.
        reason=""
        if [[ "$refused" -gt 0 ]]; then
            reason="$refused count(s) were refused"
        fi
        if [[ "$uncollectable" -gt 0 ]]; then
            if [[ -n "$reason" ]]; then
                reason="$reason and $uncollectable file(s) were uncollectable"
            else
                reason="$uncollectable file(s) were uncollectable"
            fi
        fi
        echo "runner: $reason -- reporting failure, because a partial collect cycle is not a pass" >&2
        exit 1
    fi
    exit 0
fi
# `-o cache_dir` because the bundle root may be read-only-ish and a stray
# `.pytest_cache` in it is noise; `-p no:randomly` is NOT passed -- no such
# plugin is installed and naming it would abort pytest.
# QA_RUNNER_PYTEST_ARGS is word-split on purpose (`-x --tb=short` has to work
# as two argv entries), which is why it is an array built from an unquoted
# expansion rather than one quoted word -- one quoted word made `-x --tb=short`
# a single argument pytest rejects. It is a RESERVED name on every writable
# tier (qa_runs::domain::params::RESERVED_NAMES, and
# qa_environments_sdk::RESERVED_VARIABLE_NAMES for pipeline/environment
# variables), and no operator override path exists: the runner pod's
# environment is built solely from RunSpec.env by the Argo adapter, with no
# envFrom, chart-level injection, or runner pod template it could arrive
# through. This variable is set, if at all, only by whatever already sits in
# this process's own environment -- pathname expansion is still disabled
# (`set -f` / `set +f`) around the unquoted expansion regardless, so a bare
# `*` or `?` in it cannot pick up stray files from the current directory.
declare -a EXTRA=(-v)
if [[ -n "${QA_RUNNER_PYTEST_ARGS:-}" ]]; then
    # shellcheck disable=SC2206  # word splitting is the point here
    set -f
    EXTRA=(${QA_RUNNER_PYTEST_ARGS})
    set +f
fi

python3 -m pytest \
    -p pytest_markers \
    --color=no \
    -o cache_dir=/tmp/pytest_cache \
    "${EXTRA[@]}" \
    "${ARGS[@]}"
status=$?

if [[ "$status" -eq 5 ]]; then
    echo "runner: pytest exited 5 (no tests collected) -- reporting failure, because a run with no results is not a pass" >&2
    status=1
fi
echo "runner: pytest exit status $status"
exit "$status"
