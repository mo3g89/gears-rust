#!/usr/bin/env bash
# Proves the runner's own platform fetches cannot be redirected through the
# resolver's environment variables.
#
# QA_CREDENTIALS_URL, TEST_BUNDLE_URL and VHP_COLLECT_URL name
# `qa-platform-gears.<ns>.svc.cluster.local`: four dots, fewer than the pod's
# `ndots:5`, so glibc tries the search list first. glibc honours LOCALDOMAIN
# (replaces that search list), RES_OPTIONS and HOSTALIASES, and `python3 -I`
# does not clear them. A run or environment variable LOCALDOMAIN=evil.example
# would send the runner token and the credentials to an attacker's host (the
# runner NetworkPolicy allows public egress). Tenant tests may need those
# variables, so they are not reserved; only the platform's fetches drop them.
#
# Every `python3 -I` in entrypoint.sh is one of those fetches (credentials,
# bundle, collect report), except the with_tenant_env.py wrapper in front of
# pytest, which exists to hand the tenant's environment (resolver variables
# included) to the tests; only a line that starts with that wrapper's exact
# prefix is exempt. Each fetch must be `env -u LOCALDOMAIN -u RES_OPTIONS
# -u HOSTALIASES python3 -I ...`. The prefix is also run against a stub
# python3 to prove it really clears them.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENTRYPOINT="$HERE/../../runner/entrypoint.sh"
[[ -f "$ENTRYPOINT" ]] || { echo "FAIL: $ENTRYPOINT not found"; exit 1; }

PREFIX='env -u LOCALDOMAIN -u RES_OPTIONS -u HOSTALIASES python3 -I '
failures=0

# Every invocation, comments excluded, and the wrapper excluded only where a
# line STARTS with its exact prefix: a fetch that merely mentions
# with_tenant_env.py somewhere on its line is still checked.
WRAPPER='python3 -I "$RUNNER_DIR/with_tenant_env.py" "$QA_RUNNER_TENANT_ENV" -- '
calls=()
while IFS= read -r line; do
    body="${line#*:}"
    body="${body#"${body%%[![:space:]]*}"}"
    [[ "$body" == "#"* || "$body" == "$WRAPPER"* ]] && continue
    calls+=("$line")
done < <(grep -nE 'python3 -I( |$)' "$ENTRYPOINT")
for want in 'fetch_credentials.py' 'fetch_bundle.py' 'python3 -I - "$1" "$2"'; do
    if ! printf '%s\n' "${calls[@]}" | grep -qF -- "$want"; then
        echo "FAIL: no \`python3 -I\` invocation of $want in entrypoint.sh (moved? update this test)"
        failures=$((failures + 1))
    fi
done
for line in "${calls[@]}"; do
    if [[ "$line" == *"$PREFIX"* ]]; then
        echo "PASS: clears the resolver variables -- ${line#*:}"
    else
        echo "FAIL: entrypoint.sh:${line%%:*} runs python3 -I without '$PREFIX': ${line#*:}"
        failures=$((failures + 1))
    fi
done

# The prefix itself, against a stub python3 that prints what it inherited.
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT
cat > "$WORKDIR/python3" <<'EOF'
#!/bin/sh
echo "LOCALDOMAIN=${LOCALDOMAIN-<unset>} RES_OPTIONS=${RES_OPTIONS-<unset>} HOSTALIASES=${HOSTALIASES-<unset>} args=$*"
EOF
chmod +x "$WORKDIR/python3"
got=$(PATH="$WORKDIR:$PATH" LOCALDOMAIN=evil.example RES_OPTIONS=ndots:1 HOSTALIASES=/tmp/h \
    bash -c "${PREFIX}fetch.py")
want='LOCALDOMAIN=<unset> RES_OPTIONS=<unset> HOSTALIASES=<unset> args=-I fetch.py'
if [[ "$got" == "$want" ]]; then
    echo "PASS: the prefix reaches python3 with LOCALDOMAIN, RES_OPTIONS and HOSTALIASES unset"
else
    echo "FAIL: the prefix leaked a resolver variable -- want '$want', got '$got'"
    failures=$((failures + 1))
fi

if [[ "$failures" -ne 0 ]]; then
    echo "$failures failure(s)"
    exit 1
fi
