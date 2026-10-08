#!/usr/bin/env bash
# Proves no deploy-side secret reaches any process's argv (`ps`,
# /proc/<pid>/cmdline), and that deploy-k8s.sh's --dry-run prints none.
# The sibling of check_no_password_in_argv.sh, which covers the gears image's
# own entrypoint; this one covers deploy/remote/ and deploy/runner/.
#
# Same technique: fake tools early on PATH record every argument they are
# called with -- and, for an argument that names a file (`NAME@FILE`, `@FILE`,
# `--set-file KEY=FILE`), that file's mode and bytes WHILE they run -- then a
# secret-shaped marker is searched for in what they saw. Each block under test
# is extracted from the REAL script by markers, or sourced from the real
# lib.sh, never reimplemented.
#
#   1. verify-k8s.sh check 16: the workflow client secret and the bearer token
#      it buys travel to curl in mode-600 files, not argv.
#   2. lib.sh helm_secret_files: every chart secret reaches helm through
#      --set-file, in a mode-600 file, never as --set.
#   3. lib.sh remote_sh/remote_sh_expect under --dry-run print no registered
#      secret, raw or ${v@Q}-quoted.
#   4. runner entrypoint.sh report_collect: the signed collect URL (its `sig`
#      is the HMAC tag) is read from the environment, not python3's argv,
#      and no runner log line prints it unredacted.
#   5. deploy-k8s.sh passes no chart secret with --set, has no
#      --admin-password flag, and registers every chart secret for redaction.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY="$HERE/../.."
LIB="$DEPLOY/remote/lib.sh"
DEPLOY_K8S="$DEPLOY/remote/deploy-k8s.sh"
VERIFY="$DEPLOY/remote/verify-k8s.sh"
RUNNER="$DEPLOY/runner/entrypoint.sh"
for f in "$LIB" "$DEPLOY_K8S" "$VERIFY" "$RUNNER"; do
    [[ -f "$f" ]] || { echo "FAIL: $f not found"; exit 1; }
done
command -v jq >/dev/null || { echo "FAIL: jq is required (verify-k8s.sh check 16 uses it)"; exit 1; }

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT
FAKEBIN="$WORKDIR/fakebin"; mkdir -p "$FAKEBIN"
ARGV_LOG="$WORKDIR/argv.log"
FILE_LOG="$WORKDIR/files.log"
failures=0
fail() { echo "FAIL: $*"; failures=$((failures + 1)); }
pass() { echo "PASS: $*"; }
reset_logs() { : > "$ARGV_LOG"; : > "$FILE_LOG"; }

# Markers: distinctive substrings of each secret, so a partial leak is caught.
WF_SECRET='wf-secret-ZQX1-6a1f0c2e9b'                      # nosec: test fixture only
TOKEN='eyJ-bearer-ZQX2-77d3'                                # nosec: test fixture only
ADMIN_PW="adm'in pw/+= ZQX3-admin"                          # nosec: quote, space, slash on purpose
PG_PW='pg-ZQX4-0b9e'                                        # nosec: test fixture only
SIG='ZQX5-sig-91ab'                                         # nosec: test fixture only

# write_fake NAME BEHAVIOUR -- BEHAVIOUR is bash run after recording.
write_fake() {
    cat > "$FAKEBIN/$1" <<FAKE
#!/usr/bin/env bash
for a in "\$@"; do printf '%s\n' "\$a" >> "$ARGV_LOG"; done
printf -- '--\n' >> "$ARGV_LOG"
record() { printf '%s %s\n' "\$(stat -c %a "\$1")" "\$(cat "\$1")" >> "$FILE_LOG"; }
prev=""
for a in "\$@"; do
    case "\$a" in *@/*) f="\${a#*@}"; [[ -f "\$f" ]] && record "\$f" ;; esac
    if [[ "\$prev" == "--set-file" ]]; then f="\${a#*=}"; [[ -f "\$f" ]] && record "\$f"; fi
    prev="\$a"
done
$2
FAKE
    chmod +x "$FAKEBIN/$1"
}
argv_has() { grep -qF -- "$1" "$ARGV_LOG"; }
file_has() { grep -qxF -- "$1" "$FILE_LOG"; }

# ---------------------------------------------------------------- 1 --
start=$(grep -nF '# THE CLIENT SECRET IS READ FROM THE RELEASE, NOT HARDCODED HERE.' "$VERIFY" | head -1 | cut -d: -f1)
end=""
if [[ -n "$start" ]]; then
    rel=$(tail -n "+$start" "$VERIFY" | grep -nF 'echo "PASS: $PUBLIC_ORIGIN/qa/v1/product-plugins answered 200' | head -1 | cut -d: -f1)
    [[ -n "$rel" ]] && end=$((start + rel - 1))
fi
if [[ -z "$start" || -z "$end" ]]; then
    fail "could not locate verify-k8s.sh check 16's token block (markers moved)"
else
    block="$(sed -n "${start},${end}p" "$VERIFY")"
    realm_b64="$(printf '{"clients":[{"clientId":"qa-platform-workflow","secret":"%s"}]}' "$WF_SECRET" | base64 -w0)"
    write_fake kubectl "printf '%s' '$realm_b64'"
    write_fake curl "out=''; prev=''
for a in \"\$@\"; do [[ \"\$prev\" == -o ]] && out=\"\$a\"; prev=\"\$a\"; done
case \" \$* \" in
    *openid-connect/token*) printf '{\"access_token\":\"%s\"}' '$TOKEN' > \"\$out\" ;;
    *) printf '[{\"instance_id\":\"vhp\"}]' > \"\$out\" ;;
esac
printf 200"
    reset_logs
    vwork="$(mktemp -d "$WORKDIR/verify.XXXXXX")"
    rc=0
    ( set -euo pipefail
      PATH="$FAKEBIN:$PATH" WORKDIR="$vwork" NAMESPACE=qa-platform \
      PUBLIC_ORIGIN=https://guard.invalid CA_CRT="$vwork/ca.crt"
      eval "$block" ) > "$WORKDIR/verify.out" 2>&1 || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        fail "the extracted check 16 block exited $rc:"; sed 's/^/    /' "$WORKDIR/verify.out"
    fi
    argv_has "ZQX1" && fail "the workflow client secret reached curl's argv"
    argv_has "ZQX2" && fail "the bearer token reached curl's argv"
    file_has "600 $WF_SECRET" || fail "curl did not read the client secret from a mode-600 file (client_secret@FILE)"
    file_has "600 Authorization: Bearer $TOKEN" || fail "curl did not read the Authorization header from a mode-600 file (-H @FILE)"
    [[ "$failures" -eq 0 ]] && pass "check 16 sends the client secret and the bearer token from mode-600 files, never argv"
fi

# ---------------------------------------------------------------- 2 --
before=$failures
write_fake helm ":"
reset_logs
if ( set -e; PROG_NAME=guard; source "$LIB"
     helm_secret_files keycloak.adminPassword "$ADMIN_PW" postgres.password "$PG_PW"
     echo 'helm upgrade --install r c "${SECRET_SET_FILE_ARGS[@]}"'
     echo 'echo "SECRETS_DIR=$secrets_dir"' ) > "$WORKDIR/helm-body.sh" 2> "$WORKDIR/helm-body.err"; then
    # Piped into `bash -s`, exactly as remote_sh runs a body on the remote.
    PATH="$FAKEBIN:$PATH" bash -s < "$WORKDIR/helm-body.sh" > "$WORKDIR/helm.out" 2>&1 \
        || fail "the helm body exited non-zero: $(cat "$WORKDIR/helm.out")"
    argv_has "ZQX3" && fail "keycloak.adminPassword reached helm's argv"
    argv_has "ZQX4" && fail "postgres.password reached helm's argv"
    file_has "600 $ADMIN_PW" || fail "helm did not read keycloak.adminPassword from a mode-600 --set-file"
    file_has "600 $PG_PW" || fail "helm did not read postgres.password from a mode-600 --set-file"
    dir="$(sed -n 's/^SECRETS_DIR=//p' "$WORKDIR/helm.out")"
    [[ -n "$dir" && ! -e "$dir" ]] || fail "the secrets directory '$dir' survived the body's exit"
else
    fail "lib.sh has no working helm_secret_files: $(cat "$WORKDIR/helm-body.err")"
fi
[[ "$failures" -eq "$before" ]] && pass "helm_secret_files hands helm every secret as a mode-600 --set-file, removed on exit"

# ---------------------------------------------------------------- 3 --
before=$failures
out="$( { PROG_NAME=guard; source "$LIB"
        DRY_RUN=true REMOTE_TARGET=guard@host.invalid REMOTE_PATH=/nonexistent
        register_secret "$ADMIN_PW"; register_secret "$PG_PW"
        remote_sh "label" <<EOF
raw=$PG_PW
quoted=${ADMIN_PW@Q}
$(helm_secret_files keycloak.adminPassword "$ADMIN_PW" postgres.password "$PG_PW")
EOF
        remote_sh_expect "label2" "SENTINEL" <<EOF
also=${PG_PW@Q}
EOF
      } 2>&1 )" || fail "dry-run remote_sh exited non-zero: $out"
grep -qF "ZQX3" <<<"$out" && fail "--dry-run printed keycloak.adminPassword (or part of it)"
grep -qF "ZQX4" <<<"$out" && fail "--dry-run printed postgres.password (or part of it)"
grep -qF "<redacted>" <<<"$out" || fail "--dry-run printed no <redacted> marker -- the bodies were not printed at all?"
[[ "$failures" -eq "$before" ]] && pass "--dry-run prints every body with registered secrets replaced by <redacted>"

# ---------------------------------------------------------------- 4 --
before=$failures
fn_start=$(grep -nF 'report_collect() {' "$RUNNER" | head -1 | cut -d: -f1)
fn_end=""
if [[ -n "$fn_start" ]]; then
    rel=$(tail -n "+$fn_start" "$RUNNER" | grep -n '^}$' | head -1 | cut -d: -f1)
    [[ -n "$rel" ]] && fn_end=$((fn_start + rel - 1))
fi
if [[ -z "$fn_start" || -z "$fn_end" ]]; then
    fail "could not locate report_collect in $RUNNER (markers moved)"
else
    fn="$(sed -n "${fn_start},${fn_end}p" "$RUNNER")"
    write_fake python3 "cat > '$WORKDIR/collect.py'"
    reset_logs
    ( PATH="$FAKEBIN:$PATH"
      export VHP_COLLECT_URL="http://guard.invalid/qa/v1/collect/r?tenant_id=t&sig=$SIG"
      eval "$fn"; report_collect tests/test_a.py 3 ) > /dev/null 2>&1 || fail "report_collect exited non-zero under the fake python3"
    argv_has "ZQX5" && fail "the signed collect URL reached python3's argv"
    grep -qF 'os.environ["VHP_COLLECT_URL"]' "$WORKDIR/collect.py" || fail "report_collect's script does not read VHP_COLLECT_URL from the environment"
    python3 -I -c 'import ast,sys; ast.parse(open(sys.argv[1]).read())' "$WORKDIR/collect.py" || fail "report_collect's script does not parse"
fi
# Nor does any line the runner prints carry it: the pod log is rendered in
# the run view, so a misconfigured URL is echoed with its query redacted.
if grep -nE '(echo|printf).*\$\{?VHP_COLLECT_URL\b' "$RUNNER"; then
    fail "entrypoint.sh prints VHP_COLLECT_URL (lines above) -- its query carries the HMAC sig"
fi
[[ "$failures" -eq "$before" ]] && pass "report_collect reads the signed URL from the environment, not argv, and no log line prints it"

# ---------------------------------------------------------------- 5 --
before=$failures
if grep -nE -- '--set "(keycloak\.adminPassword|bundleDownloadSigningSecret|collectReportSigningSecret|argo\.workflowClientSecret|postgres\.password)=' "$DEPLOY_K8S"; then
    fail "deploy-k8s.sh passes a chart secret with --set (lines above); use helm_secret_files"
fi
n="$(grep -c '\$(helm_secret_files' "$DEPLOY_K8S" || true)"
[[ "$n" -eq 2 ]] || fail "deploy-k8s.sh calls helm_secret_files in $n heredoc(s), expected 2 (pass 1 and pass 2)"
grep -qE -- '--admin-password\)' "$DEPLOY_K8S" && grep -qE -- 'ADMIN_PASSWORD="\$\{2' "$DEPLOY_K8S" \
    && fail "deploy-k8s.sh still accepts --admin-password VALUE (the password in its own argv)"
# Case 3 proves a REGISTERED secret is redacted; this proves deploy-k8s.sh
# registers every one it hands helm, so its own --dry-run prints none.
for var in ADMIN_PASSWORD BUNDLE_SIGNING_KEY COLLECT_SIGNING_KEY WORKFLOW_CLIENT_SECRET POSTGRES_PASSWORD; do
    grep -qF -- "register_secret \"\$$var\"" "$DEPLOY_K8S" \
        || fail "deploy-k8s.sh never calls register_secret \"\$$var\" -- its --dry-run would print that secret"
done
[[ "$failures" -eq "$before" ]] && pass "deploy-k8s.sh hands helm no secret by --set and takes none as a flag"

if [[ "$failures" -gt 0 ]]; then
    echo "FAIL: $failures deploy secret-argv check(s) failed"
    exit 1
fi
echo "PASS: no deploy-side secret reaches a process's argv, and --dry-run prints none"
