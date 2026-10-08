#!/usr/bin/env bash
# Proves the runner keeps `-v` when an operator sets QA_RUNNER_PYTEST_ARGS,
# word-splits that variable into separate argv entries, and never lets a
# glob in it expand against the working directory (finding #66).
#
# Before this, `EXTRA=(${QA_RUNNER_PYTEST_ARGS})` REPLACED the default
# `(-v)`, so `-x` alone silently dropped per-test lines from the archived log.
#
# Like test_collect_exit_code.sh, this extracts the block out of the REAL
# entrypoint.sh by line markers and evals it; a marker that stops matching
# fails loudly instead of testing stale text.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENTRYPOINT="$HERE/../../runner/entrypoint.sh"
[[ -f "$ENTRYPOINT" ]] || { echo "FAIL: $ENTRYPOINT not found"; exit 1; }

start_line=$(grep -nF 'declare -a EXTRA=(-v)' "$ENTRYPOINT" | head -1 | cut -d: -f1)
end_line=""
if [[ -n "$start_line" ]]; then
    rel=$(tail -n "+$start_line" "$ENTRYPOINT" | grep -n '^fi$' | head -1 | cut -d: -f1)
    [[ -n "$rel" ]] && end_line=$((start_line + rel - 1))
fi
if [[ -z "$start_line" || -z "$end_line" ]]; then
    echo "FAIL: could not locate the EXTRA block in $ENTRYPOINT (markers moved -- update this test)"
    exit 1
fi
block="$(sed -n "${start_line},${end_line}p" "$ENTRYPOINT")"
for sentinel in 'QA_RUNNER_PYTEST_ARGS' 'set -f' 'set +f'; do
    grep -qF -- "$sentinel" <<<"$block" || { echo "FAIL: extracted block lacks '$sentinel'"; exit 1; }
done

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT
# A file a stray glob would pick up.
: > "$WORKDIR/stray_test.py"

failures=0
# Prints EXTRA one element per line, then whether noglob is still on.
run_block() {
    (
        cd "$WORKDIR"
        if [[ "$1" == "<unset>" ]]; then unset QA_RUNNER_PYTEST_ARGS; else QA_RUNNER_PYTEST_ARGS="$1"; fi
        eval "$block"
        printf '%s\n' "${EXTRA[@]}"
        [[ "$-" == *f* ]] && echo "NOGLOB-LEFT-ON" || true
    )
}
check() {
    local name="$1" input="$2" want="$3" got
    got="$(run_block "$input")"
    if [[ "$got" == "$want" ]]; then
        echo "PASS: $name"
    else
        echo "FAIL: $name -- want:"; sed 's/^/    /' <<<"$want"
        echo "  got:"; sed 's/^/    /' <<<"$got"
        failures=$((failures + 1))
    fi
}

check "unset keeps the default -v" "<unset>" "-v"
check "empty keeps the default -v" "" "-v"
check "operator words follow -v, split into separate entries" "-x --tb=short" $'-v\n-x\n--tb=short'
check "a glob stays literal (set -f) and noglob is switched back off" "-x *" $'-v\n-x\n*'

if [[ "$failures" -gt 0 ]]; then
    echo "FAIL: $failures runner pytest-args check(s) failed"
    exit 1
fi
echo "PASS: QA_RUNNER_PYTEST_ARGS appends to -v, word-splits, and never globs"
