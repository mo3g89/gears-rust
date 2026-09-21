#!/usr/bin/env bash
# Proves POSTGRES_PASSWORD never appears in `sed`'s argv during
# entrypoint.sh's config render, and that the rendered file is written
# under a restrictive umask.
#
# Before this guard, `SED_ARGS` built the password substitution as an `-e`
# expression with the password interpolated directly into it -- so for the
# life of that one `sed` process, anything in this container's PID
# namespace could read the password straight out of `/proc/<pid>/cmdline`
# (what `ps` shows). The rendered file was also written at whatever mode
# the ambient umask allowed, which is not necessarily private.
#
# WHY THIS IS NOT A FULL INTEGRATION TEST, and why it does not merely
# inspect the script's text either. `ps` sampling a real subprocess for its
# argv is racy -- the window in which a fast `sed` invocation is actually
# running is not something a test can reliably catch mid-flight. Instead
# this intercepts the ACTUAL argv `sed` is invoked with, by putting a fake
# `sed` earlier on PATH that records every argument it is called with (and
# the permissions of anything passed via `-f`) before delegating to the
# real one. That is a stronger check than sampling `ps`: it sees every
# argument on every invocation, not whatever `ps` happened to catch.
#
# Following test_collect_exit_code.sh's approach: this extracts the render
# block out of the REAL entrypoint.sh by line markers, not a reimplemented
# copy, so a future edit that changes the shape of the render is caught
# here rather than leaving this guard exercising stale text. If a marker no
# longer matches, extraction fails loudly below instead of silently
# skipping the checks.
#
# What is NOT proven here: that the rendered file is actually unreadable by
# other users/groups inside a running pod (this fixture runs as one local
# user, so "world-readable" and "not group/other-readable" cannot differ
# from each other the way they would in a container). That is why the task
# also requires checking on the stand, from inside the pod.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENTRYPOINT="$HERE/../../docker/entrypoint.sh"

[[ -f "$ENTRYPOINT" ]] || { echo "FAIL: $ENTRYPOINT not found"; exit 1; }

start_line=$(grep -nF 'escape_sed_replacement() {' "$ENTRYPOINT" | head -1 | cut -d: -f1)
end_line=""
if [[ -n "$start_line" ]]; then
    relative_end_line=$(tail -n "+$start_line" "$ENTRYPOINT" | grep -nF 'umask "$old_umask"' | head -1 | cut -d: -f1)
    if [[ -n "$relative_end_line" ]]; then
        end_line=$((start_line + relative_end_line - 1))
    fi
fi

if [[ -z "$start_line" || -z "$end_line" || "$end_line" -le "$start_line" ]]; then
    echo "FAIL: could not locate the render block in $ENTRYPOINT (markers moved -- update this test's markers to match)"
    exit 1
fi

render_block="$(sed -n "${start_line},${end_line}p" "$ENTRYPOINT")"

# Sentinels actually seen in the extracted text, so a marker drifting onto
# the wrong span fails here instead of quietly testing something else.
for sentinel in 'esc_password' 'password_sed_script' 'SED_ARGS' 'umask 077' 'umask "$old_umask"'; do
    if ! grep -qF "$sentinel" <<<"$render_block"; then
        echo "FAIL: extracted block is missing '$sentinel' -- extraction did not capture the whole render"
        exit 1
    fi
done

WORKDIR="$(mktemp -d)"
cleanup() { rm -rf "$WORKDIR"; }
trap cleanup EXIT

REAL_SED="$(command -v sed)"

FAKE_SED_DIR="$WORKDIR/fakebin"
mkdir -p "$FAKE_SED_DIR"
ARGV_LOG="$WORKDIR/sed-argv.log"
PERM_LOG="$WORKDIR/sed-f-perms.log"
: > "$ARGV_LOG"
: > "$PERM_LOG"

cat > "$FAKE_SED_DIR/sed" <<FAKE_SED
#!/usr/bin/env bash
# Records every argument this invocation of sed was called with, and the
# permissions of anything passed via -f, before delegating to the real
# sed -- this is what the test inspects, not a re-run of ps.
{
    for a in "\$@"; do
        printf '%s\n' "\$a" >> "$ARGV_LOG"
    done
    printf -- '--\n' >> "$ARGV_LOG"
}
prev=""
for a in "\$@"; do
    if [[ "\$prev" == "-f" && -f "\$a" ]]; then
        stat -c '%a %n' "\$a" >> "$PERM_LOG" 2>/dev/null || stat -f '%Lp %N' "\$a" >> "$PERM_LOG"
    fi
    prev="\$a"
done
exec "$REAL_SED" "\$@"
FAKE_SED
chmod +x "$FAKE_SED_DIR/sed"

CONFIG_FILE="$WORKDIR/template.yaml"
RENDERED_CONFIG_FILE="$WORKDIR/rendered.yaml"
cat > "$CONFIG_FILE" <<'FIXTURE'
      host: "localhost"
      user: "qa"
      password: "qa"
          - issuer_pattern: "http://localhost:8180/realms/qa-platform"
FIXTURE

# Deliberately contains all three sed metacharacters escape_sed_replacement
# exists to handle (&, /, \), so a regression there would also show up as
# a wrong rendered value, not just a leaked argv.
SECRET='S3cr3t/Va&ue\Weird'

run_render() {
    (
        set -euo pipefail
        PATH="$FAKE_SED_DIR:$PATH"
        POSTGRES_HOST="db.example.invalid"
        POSTGRES_USER="qa_user"
        POSTGRES_PASSWORD="$SECRET"
        PUBLIC_ISSUER_ORIGIN="http://qa-platform-argv-test.example.invalid:8180"
        CONFIG_FILE="$CONFIG_FILE"
        RENDERED_CONFIG_FILE="$RENDERED_CONFIG_FILE"
        QA_RUNS_ARGO_CONFIG=""
        QA_ENVIRONMENTS_ARGO_CONFIG=""
        eval "$render_block"
    )
}

failures=0

if ! run_render; then
    echo "FAIL: the extracted render block exited non-zero"
    failures=$((failures + 1))
fi

if [[ -f "$RENDERED_CONFIG_FILE" ]] && grep -qF "$SECRET" "$RENDERED_CONFIG_FILE"; then
    echo "PASS: the password still reaches the rendered file (substitution works)"
else
    echo "FAIL: the rendered file does not contain the password -- the render is broken, not just re-plumbed"
    failures=$((failures + 1))
fi

if grep -qF "$SECRET" "$ARGV_LOG"; then
    echo "FAIL: the password appeared in sed's argv:"
    grep -F "$SECRET" "$ARGV_LOG" | sed 's/^/  /'
    failures=$((failures + 1))
else
    echo "PASS: the password never appears in any sed invocation's argv"
fi

if [[ ! -s "$PERM_LOG" ]]; then
    echo "FAIL: no -f argument was observed -- the password substitution is still going through -e (or the fake sed did not see the real invocation)"
    failures=$((failures + 1))
else
    while read -r mode name; do
        if [[ "$mode" != "600" ]]; then
            echo "FAIL: password_sed_script ($name) was mode $mode, expected 600"
            failures=$((failures + 1))
        else
            echo "PASS: password_sed_script was mode 600 when sed read it"
        fi
    done < "$PERM_LOG"
fi

if [[ -f "$RENDERED_CONFIG_FILE" ]]; then
    rendered_mode="$(stat -c '%a' "$RENDERED_CONFIG_FILE" 2>/dev/null || stat -f '%Lp' "$RENDERED_CONFIG_FILE")"
    if (( 8#$rendered_mode & 8#077 )); then
        echo "FAIL: $RENDERED_CONFIG_FILE is mode $rendered_mode -- group/other bits are set, umask 077 did not take effect"
        failures=$((failures + 1))
    else
        echo "PASS: $RENDERED_CONFIG_FILE is mode $rendered_mode -- no group/other bits"
    fi
fi

if [[ "$failures" -gt 0 ]]; then
    echo "FAIL: $failures password-argv check(s) failed"
    exit 1
fi
echo "PASS: entrypoint.sh's render never puts the password in sed's argv, and writes the rendered file under umask 077"
