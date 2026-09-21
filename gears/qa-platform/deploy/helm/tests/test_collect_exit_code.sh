#!/usr/bin/env bash
# Proves the runner's collect cycle (COLLECT_ONLY=true, entrypoint.sh) fails
# CLOSED on a PARTIAL collect, not only a total one.
#
# Before WS2 Task 3, a single accepted count was enough to exit 0 even with
# 220 of 221 files refused or uncollectable -- the zero-results guard only
# catches total suppression (accepted == 0). qa-insights has no other signal
# that a collect cycle delivered less than everything, so a refused or
# uncollectable count now has to fail the workflow phase too.
#
# WHY THIS IS NOT A FULL INTEGRATION TEST. The collect branch this guards
# calls out to `python3 -m pytest` and, per accepted count, an HTTP endpoint
# qa-insights exposes -- neither reachable here without a live stand, and this
# suite runs with no cluster (see the Makefile's `helm-tests` target doc).
# What IS pure is the final decision: given accepted/refused/uncollectable
# counts, what exit code and message the branch produces. This test extracts
# exactly that block out of the real entrypoint.sh -- by line markers, not a
# reimplementation -- and drives it with synthetic counts. If entrypoint.sh's
# shape changes enough that a marker no longer matches, extraction fails
# loudly below instead of this test silently exercising stale text.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENTRYPOINT="$HERE/../../runner/entrypoint.sh"

[[ -f "$ENTRYPOINT" ]] || { echo "FAIL: $ENTRYPOINT not found"; exit 1; }

start_line=$(grep -nF 'echo "runner: collect summary:' "$ENTRYPOINT" | head -1 | cut -d: -f1)
# Searched from $start_line onward, not over the whole file: a second bare
# 4-space-indented `exit 0` inserted above the real end (in an earlier,
# unrelated block) would otherwise match first and silently truncate the
# extraction to something short of the real decision block. Exactly one such
# line exists in entrypoint.sh today, which is why this bug has not fired yet.
end_line=""
if [[ -n "$start_line" ]]; then
    relative_end_line=$(tail -n "+$start_line" "$ENTRYPOINT" | grep -n '^    exit 0$' | head -1 | cut -d: -f1)
    if [[ -n "$relative_end_line" ]]; then
        end_line=$((start_line + relative_end_line - 1))
    fi
fi

if [[ -z "$start_line" || -z "$end_line" || "$end_line" -le "$start_line" ]]; then
    echo "FAIL: could not locate the collect-summary decision block in $ENTRYPOINT (markers moved -- update this test's markers to match)"
    exit 1
fi

decision_block="$(sed -n "${start_line},${end_line}p" "$ENTRYPOINT")"

# A sentinel actually seen in the extracted text, so a marker drifting onto
# the WRONG block (e.g. matching only the "no count was accepted" branch)
# fails here instead of quietly testing something else.
for sentinel in 'collect summary' 'no count was accepted' 'exit 0'; do
    if ! grep -qF "$sentinel" <<<"$decision_block"; then
        echo "FAIL: extracted block is missing '$sentinel' -- extraction did not capture the whole decision"
        exit 1
    fi
done

# Runs the extracted block in a subshell with synthetic counts, so its `exit`
# calls end the subshell rather than this test script. FILES is only ever
# read for its length.
run_decision() {
    local accepted="$1" refused="$2" uncollectable="$3"
    (
        # shellcheck disable=SC2034  # read via ${#FILES[@]} inside the block
        FILES=(a b c)
        accepted="$accepted" refused="$refused" uncollectable="$uncollectable"
        eval "$decision_block"
    )
}

failures=0

check() {
    local name="$1" accepted="$2" refused="$3" uncollectable="$4" want_status="$5" want_text="$6"
    local stderr_file
    stderr_file="$(mktemp)"
    local status=0
    run_decision "$accepted" "$refused" "$uncollectable" 2>"$stderr_file" 1>/dev/null || status=$?
    local stderr_out
    stderr_out="$(cat "$stderr_file")"
    rm -f "$stderr_file"

    if [[ "$status" -ne "$want_status" ]]; then
        echo "FAIL: $name -- accepted=$accepted refused=$refused uncollectable=$uncollectable exited $status, wanted $want_status (stderr: $stderr_out)"
        failures=$((failures + 1))
        return
    fi
    if [[ -n "$want_text" ]] && ! grep -qF "$want_text" <<<"$stderr_out"; then
        echo "FAIL: $name -- expected stderr to mention '$want_text', got: $stderr_out"
        failures=$((failures + 1))
        return
    fi
    echo "PASS: $name"
}

check "zero accepted still fails (unchanged behaviour)" 0 0 0 1 "no count was accepted"
check "all accepted, nothing refused or uncollectable, passes" 3 0 0 0 ""
check "one refused among accepted now fails, and says refused" 5 1 0 1 "were refused"
check "one uncollectable among accepted now fails, and says uncollectable" 5 0 1 1 "uncollectable"
check "refused and uncollectable together: message names both" 5 2 3 1 "refused"
check "refused and uncollectable together: message names both (uncollectable half)" 5 2 3 1 "uncollectable"

if [[ "$failures" -gt 0 ]]; then
    echo "FAIL: $failures collect-exit-code check(s) failed"
    exit 1
fi
echo "PASS: entrypoint.sh's collect cycle exits non-zero on any refused or uncollectable count, and names which"
