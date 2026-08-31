#!/usr/bin/env bash
# Proves the envsubst template still renders the compose stack's nginx.conf
# EXACTLY as it was, so templatising it changed no behaviour of the local stack.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UI="$HERE/../../../qa-platform-ui"
BASELINE="$HERE/fixtures/nginx.conf.baseline"

export NGINX_RESOLVER='127.0.0.11'
export GEARS_UPSTREAM='http://gears:8087'

rendered="$(mktemp)"
# `|| rc=$?`, not a bare command followed by `rc=$?`. Under `set -e` (line 4)
# a failing command aborts the script immediately, so the old `rc=$?` line
# here was UNREACHABLE on failure and a no-op (rc=0) on success -- the check
# below could never fire. Same idiom, and the same fix, as the exit-status
# handling throughout deploy/remote/.
rc=0
envsubst '${NGINX_RESOLVER} ${GEARS_UPSTREAM}' \
    < "$UI/default.conf.template" > "$rendered" || rc=$?
[ "$rc" -eq 0 ] || { echo "FAIL: envsubst exited $rc"; exit 1; }

if diff -u "$BASELINE" "$rendered"; then
    echo "PASS: template renders the compose baseline byte-identically"
else
    echo "FAIL: rendered template differs from the compose baseline (above)"
    exit 1
fi

# The nginx runtime variables must survive envsubst untouched. $sse_authorization
# is the one that matters: substituting it away silently removes the SSE auth
# bridge and the stream then fails closed with no error naming this file.
for v in '$sse_authorization' '$arg_access_token' '$http_authorization' '$uri' '$gears'; do
    if ! grep -qF -- "$v" "$rendered"; then
        echo "FAIL: nginx runtime variable $v was eaten by envsubst"
        exit 1
    fi
done
echo "PASS: nginx runtime variables survived envsubst"
