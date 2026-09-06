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

# The SSE location must not log the query string: `?access_token=eyJ...` is a
# live bearer credential and nginx's default `combined` format writes the whole
# request line. Review finding #51.
grep -q 'log_format sse_no_query' "$rendered" \
  || { echo "FAIL: no sse_no_query log_format"; exit 1; }
grep -q 'access_log .* sse_no_query' "$rendered" \
  || { echo "FAIL: sse_no_query is defined but never applied"; exit 1; }
# And it must be applied inside the SSE location only -- a server-level
# access_log would change every route's log shape, which is not what this is.
#
# Anchored to `^    location ~` -- the real directive, indented four spaces --
# rather than a bare `/location ~ .../` search. The template's own doc comment
# beside the log_format quotes this same location pattern in a `#`-prefixed
# line starting at column 0, so an unanchored search matches that comment
# first and opens its range there instead of at the real location, closing at
# the first unrelated `^    }` it finds (the SPA's `location /` block) -- a
# false PASS that a reviewer reproduced by moving access_log to server level
# and watching this check not notice.
awk '/^    location ~ \^\/qa\/v1\/runs/,/^    }/' "$rendered" | grep -q 'access_log .* sse_no_query' \
  || { echo "FAIL: sse_no_query is not applied inside the SSE location"; exit 1; }
echo "PASS: SSE location redacts the access-token query string from its access log"

# THE OTHER HALF OF THE SAME LEAK. `log_format` governs the access log only;
# nginx writes upstream failures to the ERROR log in a fixed format that
# carries the whole request line, token and all -- twice per failure, in
# `request:` and again in `upstream:`. A gears restart makes an `EventSource`
# reconnect against a 502 in a loop, so it recurs exactly when the deployment
# is unhealthy. `crit` drops `error`-level entries for this location only.
# Whole-branch review I3. Same anchoring as the check above, and for the same
# reason: the template's own comment quotes this location pattern at column 0.
awk '/^    location ~ \^\/qa\/v1\/runs/,/^    }/' "$rendered" | grep -qE 'error_log +[^ ]+ +crit;' \
  || { echo "FAIL: SSE location does not raise its error_log to crit -- an upstream 502 on this route writes ?access_token=eyJ... to the error log"; exit 1; }
# And ONLY inside it: a server-level `error_log ... crit` would blind every
# other route, which is a far bigger loss than this one route's upstream detail.
if grep -qE '^ {0,4}error_log +[^ ]+ +crit;' "$rendered"; then
    echo "FAIL: error_log crit appears outside the SSE location -- it must not silence the whole server"
    exit 1
fi
echo "PASS: SSE location suppresses upstream-failure error-log entries, and only that location"
