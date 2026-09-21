#!/usr/bin/env bash
# Verification suite for the qa-platform Helm/k3s deploy. The premise: "the
# deploy printed no errors" is not "the deploy is correct", and every check
# below exists because a specific failure once looked like success.
#
# RUNS ON THE NODE ITSELF, not over ssh. Unlike deploy-k8s.sh (which drives a
# REMOTE host from a local machine via lib.sh's remote_sh/ssh_ro),
# this script IS the thing that runs on the remote -- deploy-k8s.sh rsyncs it
# over like any other file and then, from inside its own remote_sh heredoc,
# does:
#
#   PUBLIC_ORIGIN="$2" NAMESPACE="$3" IMAGE_TAG="$4" bash "$VERIFY_SCRIPT"
#
# which is also exactly how to run this by hand on the node for a standalone
# check. `kubectl`, `curl`, `jq`, `openssl` and `base64` are assumed present
# on PATH (deploy-k8s.sh's own preflight already requires kubectl; the other
# four are standard on the target node -- checked below anyway, with a named
# refusal rather than a mid-script `command not found`).
#
# Usage (env vars):
#   PUBLIC_ORIGIN   required. scheme://host[:port] the BROWSER reaches the
#                   stack at -- same value deploy-k8s.sh's --public-origin
#                   took, no trailing slash, no path. Drives every https
#                   check below and the expected issuer/redirect strings.
#   NAMESPACE       optional, default "qa-platform" -- the Helm release's
#                   namespace.
#   ARGO_NAMESPACE  optional, default "argo" -- must equal values.yaml's
#                   argo.namespace (rbac-argo.yaml's Role/RoleBinding live
#                   there, not in NAMESPACE).
#   IMAGE_TAG       optional. The tag deploy-k8s.sh built and passed to Helm
#                   as images.gears.tag / images.ui.tag. If unset, the first
#                   new check below (every Deployment rolled to it) is
#                   SKIPPED with a NOTE, not silently passed -- see that
#                   check's own comment.
#   KUBECONFIG      optional, default /etc/rancher/k3s/k3s.yaml (this
#                   cluster's own kubeconfig path; every other script in this
#                   directory that touches k3s defaults it the same way).
#
# STOPS AT THE FIRST FAILURE. Every check below `exit 1`s the moment it fails,
# immediately after printing a `FAIL:` line naming what is wrong and what to do
# about it. The trade-off
# is real (a run that fails check 3 never learns whether check 9 would also
# have failed) but the alternative -- swallowing exit codes so every check
# can "run" regardless -- is exactly the false-green shape this project has
# already been bitten by; see the exit-code notes throughout below.
#
# EVERY CHECK BELOW WRITES TO A FILE OR CAPTURES A VALUE AND TESTS IT
# SEPARATELY -- no check ends in a pipe whose exit status is trusted as the
# result. `cmd | tail` reports tail's status, not cmd's; a bare
# `x=$(cmd 2>&1); rc=$?` under `set -e` is its own trap in a different
# shape (see below). Two idioms are used throughout, deliberately, matching
# which failure mode each guards against:
#   (a) `cmd > file 2>err || rc=$?` -- for a command whose EXIT CODE is the
#       thing being tested. Note the `|| rc=$?`, not a bare `rc=$?` on the
#       next line: under `set -e`, a plain `cmd > file` (or `x=$(cmd)`) that
#       fails aborts the script THERE, before the next line ever assigns
#       `rc`, so the custom "FAIL: ..." message this script exists to print
#       never runs -- the run just dies with cmd's raw stderr and no
#       attribution. `cmd || rc=$?` is exempt from errexit (the `||` makes it
#       so), so `rc` reliably holds cmd's real exit status and the FAIL
#       branch actually executes. (This is the same idiom lib.sh's
#       remote_sh_expect already uses for the identical reason, restated here
#       for exactly the same reason.)
#   (b) `x="$(cmd 2>/dev/null || true)"` -- for a command whose CAPTURED
#       VALUE is the thing being tested (a grep -c count, a psql query
#       result, a log line), where the command's own exit code carries no
#       information worth keeping (grep -c returns 1 on zero matches, which
#       is itself a legitimate answer, not an error). The trailing `|| true`
#       is what keeps a legitimate zero-match/empty result from aborting the
#       script via errexit; the VALUE captured in `x` is what every such
#       check actually branches on afterwards, never `$?`.
#   (c) `psql_count()` (defined below, used by the DB-fact checks) is called
#       from `if ! psql_count ...; then exit 1; fi`, NEVER via
#       `x="$(psql_count ...)"`. Fix round 1 caught a subtler version of the
#       SAME class of bug in checks 8/9/10: `${n:-0}` treats an EMPTY read
#       (the exec/query itself failed) identically to a genuine "0 rows" --
#       absent evidence read as a clean PASS (or, for check 9, a NOTE with no
#       FAIL branch at all to catch it). `psql_count` fails loudly instead of
#       defaulting a blank or non-numeric result to 0. It must be called from
#       an `if`, not a command substitution: a function's own `return 1`
#       inside `$(...)` only ends that subshell, and while the OUTER
#       assignment would still (correctly) abort the script via errexit, the
#       diagnostic path becomes indirect and harder to audit than calling it
#       where `return 1` is unambiguously exempt from errexit and the
#       `if`/`else` reads as what it is.
#   (d) `grep_count()` (defined below, used by every binary-string
#       presence/absence read), the same idea as (c) applied to `kubectl
#       exec ... grep -ac`. Review round 3 found a THIRD spelling of (c)'s
#       mistake, in a shape with no pipe at all: checks 2/9's asymmetric
#       with/without pairs did `n="$(kubectl exec ... grep -ac ... || true)"`
#       then `${n:-0}` in the comparison -- if that exec failed outright
#       (a pod restart between two sequential execs, an API hiccup), stdout
#       was empty, `${n:-0}` coerced it to a genuine-looking zero, and for
#       the pair's "must be ABSENT" half that zero silently satisfied the
#       PASS condition. `grep_count` is deliberately NOT gated on the exec's
#       own exit status the way `psql_count` is gated on psql's -- `grep -c`
#       legitimately exits 1 on zero matches, a normal answer, not a sign
#       the exec never ran. What proves the exec actually ran is stdout
#       itself: `grep -c` always prints a plain count, even "0", on a
#       completed run, so an empty/non-numeric stdout is the tell for "did
#       not complete" and is FAILed rather than defaulted to 0.
#
# THE INVARIANT THIS FILE NOW HOLDS, stated once so a future check can be
# checked against it rather than this mistake being re-found a fourth time:
# NO VARIABLE THAT FEEDS A PASS/FAIL DECISION MAY BE DERIVED WITH A DEFAULT
# (`${x:-0}`, `|| true` swallowing an exec's own result, `tail`/`head` on a
# stream whose upstream may have failed) UNLESS THAT DEFAULT IS ALSO THE
# CORRECT ANSWER WHEN THE READ ITSELF NEVER HAPPENED. Every count or value
# behind a comparison in this file now goes through a helper (`psql_count`,
# `grep_count`) that fails the check outright when the read cannot be
# proven to have happened, OR is provably safe by construction because ITS
# OWN empty/defaulted case already lands on the FAIL branch by itself (the
# "presence-required-for-PASS" shape documented at checks 5/13/15, and
# check 0's post-rollout-failure job-status reads, which can only pick a
# FAIL's wording, never its outcome). When adding a new check: if a read's
# absence could ever be mistaken for a legitimate answer that helps produce
# a PASS, it needs a real failure path before comparison, not a default.
set -euo pipefail

# ------------------------------------------------------------------ setup --
if [ -z "${PUBLIC_ORIGIN:-}" ]; then
    echo "verify-k8s: PUBLIC_ORIGIN is not set -- e.g. PUBLIC_ORIGIN=https://qa.example.com NAMESPACE=qa-platform IMAGE_TAG=deploy-... bash $0" >&2
    exit 1
fi
# Same shape deploy-k8s.sh itself enforces on this exact value, checked here
# too: a value with a trailing slash or a path would make every string
# comparison below (issuer, redirect URI) compare against the wrong thing
# with no error, only a confusing FAIL further down.
if [[ ! "$PUBLIC_ORIGIN" =~ ^https?://[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?$ ]]; then
    echo "verify-k8s: PUBLIC_ORIGIN '$PUBLIC_ORIGIN' is not a plain 'http(s)://host[:port]' origin (no trailing slash, no path) -- refusing to run checks whose string comparisons would silently be against the wrong value" >&2
    exit 1
fi
NAMESPACE="${NAMESPACE:-qa-platform}"
ARGO_NAMESPACE="${ARGO_NAMESPACE:-argo}"
: "${KUBECONFIG:=/etc/rancher/k3s/k3s.yaml}"
export KUBECONFIG

for tool in kubectl curl jq openssl base64; do
    command -v "$tool" >/dev/null 2>&1 || { echo "verify-k8s: '$tool' is not on PATH -- this script needs it" >&2; exit 1; }
done

# `if ! WORKDIR=$(...)`, not a bare `WORKDIR="$(...)"`: the same errexit trap
# this file's own header warns about applies to mktemp exactly as it does to
# kubectl -- a bare assignment that fails takes the whole script down before
# any custom message can print. Exempted here by being an `if` condition.
if ! WORKDIR="$(mktemp -d /tmp/verify-k8s.XXXXXX)"; then
    echo "verify-k8s: mktemp could not create a scratch directory under /tmp -- cannot continue" >&2
    exit 1
fi
trap 'rm -rf "$WORKDIR"' EXIT

step() { printf '\n=== %s ===\n' "$*"; }

echo "verify-k8s: PUBLIC_ORIGIN=$PUBLIC_ORIGIN NAMESPACE=$NAMESPACE ARGO_NAMESPACE=$ARGO_NAMESPACE IMAGE_TAG=${IMAGE_TAG:-<unset>}"

# ============================================================ preconditions =
step "0: kubectl rollout status -- gears/ui/keycloak settled (post-hook steady state)"
# EXPECT THE GEARS POD TO CrashLoopBackOff EARLY IN A FRESH INSTALL, AND
# RECOVER -- job-db-migrate.yaml's own header is the full account: Helm
# applies the gears Deployment before it ever runs the db-migrate/
# tenant-seed post-install hooks, so on a first install the gears pod races
# an unmigrated, unseeded database and aborts boot until both hooks
# complete, at which point the NEXT restart succeeds. A single sample of pod
# state can therefore catch a stack mid-recovery and call it broken, so this
# waits for `kubectl rollout status` to report Available rather than
# sampling once. If invoked through deploy-k8s.sh, that driver's own
# rollout-status step has typically already settled this before this script
# even runs -- these calls then return almost immediately (rollout status is
# cheap once Available), so the wait budget below costs nothing on the
# common path and only matters when this script runs standalone right after
# `helm upgrade --install` returns.
#
# THE FAIL MESSAGE MUST DISTINGUISH "STILL SETTLING" FROM "BROKEN" (Task 12's
# brief, constraint 6), because the two look identical from `kubectl get
# pods` alone (both are a CrashLoopBackOff gears pod) and only differ in
# whether the hooks this pod is racing have themselves finished yet. Read
# that back from the Jobs' own status.succeeded field -- a Job with zero
# successes so far reports NO succeeded field at all, which is why the
# comparison below treats an empty read as 0, not as an error.
#
# A THIRD OUTCOME, fixed in review round 1: a Job that has EXHAUSTED its
# backoffLimit (6, job-db-migrate.yaml/job-tenant-seed.yaml) also reports no
# `status.succeeded` field -- identical to "hasn't finished yet" by the
# `-lt 1` test alone, which would tell an operator to keep waiting for a Job
# that has already given up and will never succeed on its own. Kubernetes
# marks that case with a `Failed` status Condition, checked separately and
# BEFORE the still-settling/broken split below so it takes priority over
# both.
#
# `${migrate_succ:-0}`/`${seed_succ:-0}` BELOW ARE SAFE DESPITE THE `:-0`
# SHAPE checks 2/9 were fixed for in review round 3, and it is worth stating
# why rather than leaving that to be re-derived: this whole block only runs
# after `kubectl rollout status` has ALREADY failed, and every branch of the
# if/elif/elif/else below ends in the same `exit 1` -- an unread
# migrate_succ/seed_succ can only pick the WRONG WORDING of an already-
# certain FAIL (calling a broken deploy "still settling" or vice versa), it
# can never turn this block into a PASS. That is what makes it a different,
# lower-stakes case than checks 2/9's asymmetric with/without pairs, where an
# unread value fed a PASS/FAIL decision directly.
ROLLOUT_TIMEOUT="${ROLLOUT_TIMEOUT:-300s}"
for dep in qa-platform-gears qa-platform-ui qa-platform-keycloak; do
    if kubectl -n "$NAMESPACE" rollout status "deployment/$dep" --timeout="$ROLLOUT_TIMEOUT" > "$WORKDIR/rollout-$dep.log" 2>&1; then
        echo "PASS: deployment/$dep is Available"
    else
        cat "$WORKDIR/rollout-$dep.log" >&2
        migrate_succ="$(kubectl -n "$NAMESPACE" get job qa-platform-db-migrate -o jsonpath='{.status.succeeded}' 2>/dev/null || true)"
        seed_succ="$(kubectl -n "$NAMESPACE" get job qa-platform-tenant-seed -o jsonpath='{.status.succeeded}' 2>/dev/null || true)"
        migrate_failed="$(kubectl -n "$NAMESPACE" get job qa-platform-db-migrate -o jsonpath='{.status.conditions[?(@.type=="Failed")].status}' 2>/dev/null || true)"
        seed_failed="$(kubectl -n "$NAMESPACE" get job qa-platform-tenant-seed -o jsonpath='{.status.conditions[?(@.type=="Failed")].status}' 2>/dev/null || true)"
        if [ "$migrate_failed" = "True" ]; then
            echo "FAIL: deployment/$dep did not reach Available, and job/qa-platform-db-migrate has EXHAUSTED its backoffLimit (6) -- it will never succeed on its own. This is broken, not still settling. 'kubectl -n $NAMESPACE logs job/qa-platform-db-migrate' names the cause." >&2
        elif [ "$seed_failed" = "True" ]; then
            echo "FAIL: deployment/$dep did not reach Available, and job/qa-platform-tenant-seed has EXHAUSTED its backoffLimit (6) -- it will never succeed on its own. This is broken, not still settling. 'kubectl -n $NAMESPACE logs job/qa-platform-tenant-seed' names the cause." >&2
        elif [ "${migrate_succ:-0}" -lt 1 ] 2>/dev/null || [ "${seed_succ:-0}" -lt 1 ] 2>/dev/null; then
            echo "FAIL: deployment/$dep did not reach Available within $ROLLOUT_TIMEOUT, and the db-migrate/tenant-seed post-install hooks have NOT both completed yet (migrate succeeded=${migrate_succ:-0}, seed succeeded=${seed_succ:-0}). This looks like the STILL-SETTLING crash loop job-db-migrate.yaml documents, not a broken chart -- re-run this script once 'kubectl -n $NAMESPACE get jobs' shows both Complete." >&2
        else
            echo "FAIL: deployment/$dep did not reach Available within $ROLLOUT_TIMEOUT, but the db-migrate/tenant-seed hooks have ALREADY both completed (migrate=${migrate_succ:-0} seed=${seed_succ:-0}) -- so this is NOT the expected early crash loop, it is broken. 'kubectl -n $NAMESPACE describe deployment/$dep' and 'kubectl -n $NAMESPACE logs deployment/$dep --previous' name the cause." >&2
        fi
        exit 1
    fi
done
echo "PASS: gears, ui and keycloak Deployments are all Available"

step "1: the dev CA, extracted from the qa-platform-tls Secret (cluster-sourced, not a file on disk)"
# Every https check below needs this as --cacert's trust anchor. Read out of
# the Secret directly, NOT via `kubectl exec ... cat /etc/keycloak-tls/ca.crt`
# out of the gears container: that container may
# still be mid-CrashLoopBackOff even after step 0 above (Available only means
# the CURRENT pod passed its readiness probe, not that a previous restart
# didn't happen moments ago), and more fundamentally the Secret is the
# authoritative source certs-job.yaml wrote it into -- the gears pod's own
# copy is just a projection of the same Secret's ca.crt key.
#
# TWO SEPARATE STEPS, EACH CHECKED ON ITS OWN, per Task 12's brief
# (constraint 4): `kubectl get secret ... | base64 -d` piped straight through
# would report base64's exit status, not kubectl's, and a kubectl failure
# (wrong namespace, Secret not yet created) would feed base64 an empty
# string that decodes to an empty (but "successful") file -- silently wrong
# rather than loudly missing.
CA_B64="$WORKDIR/ca.crt.b64"
CA_CRT="$WORKDIR/ca.crt"
rc=0
kubectl -n "$NAMESPACE" get secret qa-platform-tls -o jsonpath='{.data.ca\.crt}' > "$CA_B64" 2>"$WORKDIR/ca.get.err" || rc=$?
if [ "$rc" -ne 0 ] || [ ! -s "$CA_B64" ]; then
    echo "FAIL: could not read secret/qa-platform-tls's ca.crt key in namespace $NAMESPACE (exit $rc): $(cat "$WORKDIR/ca.get.err" 2>/dev/null). certs-job.yaml is what should have created this Secret before Deployments ever start -- check 'kubectl -n $NAMESPACE get secret qa-platform-tls' and the certs-job's own logs." >&2
    exit 1
fi
rc=0
base64 -d "$CA_B64" > "$CA_CRT" 2>"$WORKDIR/ca.decode.err" || rc=$?
if [ "$rc" -ne 0 ] || [ ! -s "$CA_CRT" ]; then
    echo "FAIL: base64-decoding secret/qa-platform-tls's ca.crt produced nothing usable (exit $rc): $(cat "$WORKDIR/ca.decode.err" 2>/dev/null)" >&2
    exit 1
fi
echo "PASS: extracted the dev CA to $CA_CRT ($(openssl x509 -in "$CA_CRT" -noout -subject 2>/dev/null || echo 'subject unreadable'))"

# ====================================================== platform probes ====
# The checks below probe the platform itself rather than the chart's
# mechanics: the issuer the UI bundle was built against, the issuer the gears
# were rendered with, and whether the two agree with what Keycloak actually
# advertises. They are the ones that catch a stack which installed cleanly and
# cannot log anyone in.

# Shared by several checks below (the UI bundle's baked issuer, the gears'
# own rendered issuer_pattern) -- defined once so both compare against the
# literal same string.
want_issuer="$PUBLIC_ORIGIN/realms/qa-platform"

# Runs one `select count(*) ...`-shaped query against qa-platform-postgres
# and leaves the numeric result in PSQL_COUNT_VAL. FAILS (prints a `FAIL:`
# line and returns 1) rather than defaulting an unreadable result to 0 --
# review round 1's Critical was exactly this: checks 8/9/10 (old numbering)
# used `${n:-0}` on a value that could be EMPTY because the exec/query
# itself failed, which silently read as "legitimately zero rows" (a
# cold-start NOTE for the observation checks, or -- worse -- a clean PASS for
# the leak canary, whose entire job is to have looked). Absent evidence is a
# FAIL here, never a PASS, matching the fix already applied to the PVC check.
#
# Called from `if ! psql_count ...; then exit 1; fi` at every call site,
# NEVER via `x="$(psql_count ...)"` -- see this file's header, idiom (c).
psql_count() {
    local db="$1" sql="$2" what="$3" rc=0
    kubectl exec -n "$NAMESPACE" statefulset/qa-platform-postgres -- psql -U qa -d "$db" -tAc "$sql" \
        > "$WORKDIR/psql-count.out" 2>"$WORKDIR/psql-count.err" || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "FAIL: could not query $what (exit $rc): $(cat "$WORKDIR/psql-count.err" 2>/dev/null)" >&2
        return 1
    fi
    PSQL_COUNT_VAL="$(tr -d ' \r' < "$WORKDIR/psql-count.out")"
    case "$PSQL_COUNT_VAL" in
        ''|*[!0-9]*)
            echo "FAIL: query for $what returned '$PSQL_COUNT_VAL', not a plain count -- an unreadable result is UNVERIFIED, not zero; treating it as zero is the exact false-green shape fixed here." >&2
            return 1 ;;
    esac
}

# Runs one `grep -ac PATTERN FILE` inside a pod via `kubectl exec` and leaves
# the numeric match count in GREP_COUNT_VAL. FAILS (prints `FAIL:` and
# returns 1) if that count cannot be read as a plain non-negative integer --
# fixed in review round 3, a THIRD spelling of round 1/2's same mistake, in a
# shape no pipe-focused audit could have caught: `n_x="$(kubectl exec ...
# grep -ac ... || true)"` has no pipe in it at all. If the exec itself fails
# outright (a pod restart between two sequential execs, an API hiccup, an
# exec quota) stdout is empty, and checks 2/9 (before this fix) did
# `${n:-0}` on it -- indistinguishable from a genuine zero count, and for
# the "must be ABSENT" half of an asymmetric with/without pair, an unread
# value defaulting to 0 contributes a false PASS exactly as readily as a
# real zero would.
#
# DELIBERATELY NOT GATED ON `kubectl exec`'s OWN EXIT STATUS, unlike
# psql_count -- `grep -c` legitimately exits 1 on zero matches, which is a
# normal, expected answer here, not a sign the exec never ran; gating on
# `$? -eq 0` would make a real "count is 0" result FAIL. What DOES prove the
# exec actually reached the container and ran grep is stdout: `grep -c`
# always prints a plain count (even "0") on a completed run, so an EMPTY or
# non-numeric stdout is what a dropped connection, a missing pod, or an exec
# quota look like -- and that is exactly the case that must never be read as
# zero. `|| true` on the exec itself only suppresses errexit for grep's
# expected exit-1-on-no-match case; it is not what proves anything here.
#
# Called from `if ! grep_count ...; then exit 1; fi`, never via
# `x="$(grep_count ...)"` -- same reasoning as psql_count, this file's
# header idiom (c).
grep_count() {
    local target="$1" pattern="$2" file="$3" what="$4"
    kubectl exec -n "$NAMESPACE" "$target" -- grep -ac "$pattern" "$file" \
        > "$WORKDIR/grep-count.out" 2>"$WORKDIR/grep-count.err" || true
    GREP_COUNT_VAL="$(tr -d ' \r\n' < "$WORKDIR/grep-count.out")"
    case "$GREP_COUNT_VAL" in
        ''|*[!0-9]*)
            echo "FAIL: could not verify $what ($(cat "$WORKDIR/grep-count.err" 2>/dev/null)) -- kubectl exec produced no usable count. An empty or unreadable result means the exec itself did not complete; it is UNVERIFIED, not zero, and must never be read as zero." >&2
            return 1 ;;
    esac
}

step "2: the deployed binary carries the argo cargo feature (absence AND presence)"
# Read as an absence-and-presence pair because either alone is ambiguous:
# the "built without" bail message only exists in a #[cfg(not(feature =
# "argo"))] build, "argo executor connected" only in one with it. This chart
# runs the real Argo executor unconditionally (gears-argo-configmaps.yaml,
# gears-deployment.yaml) -- there is no "mock" mode in-cluster.
#
# BOTH READS GO THROUGH grep_count(), fixed in review round 3: this is an
# ASYMMETRIC pair (PASS needs with>=1 AND without==0), and the "without"
# half is the one an unread value could silently fake -- a bare
# `${n_without:-0}` on a failed `kubectl exec` coerces "the exec never ran"
# into "0 matches", which is exactly the false PASS an absence-proving half
# must never produce. See grep_count()'s own comment for why this is not
# gated on the exec's exit status (grep -c exiting 1 on zero matches is a
# normal, expected result here, not a failure).
if ! grep_count deploy/qa-platform-gears 'built without the .argo. cargo feature' /usr/local/bin/cf-gears-example-server "gears binary: 'built without argo' string count"; then
    exit 1
fi
n_without="$GREP_COUNT_VAL"
if ! grep_count deploy/qa-platform-gears 'argo executor connected' /usr/local/bin/cf-gears-example-server "gears binary: 'argo executor connected' string count"; then
    exit 1
fi
n_with="$GREP_COUNT_VAL"
if [ "$n_with" -ge 1 ] && [ "$n_without" -eq 0 ]; then
    echo "PASS: the deployed gears binary was built WITH the argo feature (with=$n_with without=$n_without)"
else
    echo "FAIL: the deployed binary's argo feature state is wrong (with=$n_with without=$n_without; wanted with>=1, without=0). Check deploy/cargo-features.argo and that deploy-k8s.sh built the gears image from it." >&2
    exit 1
fi

step "3: the gears' rendered config selects the argo executor"
# Belt to entrypoint.sh's own braces (it refuses to start otherwise) -- and
# the check that would catch a fragment mounted but never inserted.
if kubectl exec -n "$NAMESPACE" deploy/qa-platform-gears -- grep -qE '^      executor: argo$' /var/lib/cf-gears/.rendered-qa-platform-stack.yaml 2>/dev/null; then
    echo "PASS: the gears' rendered config selects the argo executor"
else
    echo "FAIL: the gears' rendered config does NOT contain '      executor: argo' -- entrypoint.sh's QA_RUNS_ARGO_ANCHOR insertion did not happen; check that ConfigMap qa-platform-gears-argo-qa-runs is mounted and non-empty." >&2
    exit 1
fi

step "4: the gears' rendered issuer_pattern names this exact PUBLIC_ORIGIN"
# "k8s 4/4" below tests
# what KEYCLOAK itself asserts as its issuer, and check 5 tests what the UI
# BUNDLE was built believing it is -- neither reads what the GEARS
# themselves rendered, which is the third, independent place this exact
# string has to agree and the one this check exists for.
# THE HISTORICAL BUG THIS CATCHES: an entrypoint.sh rendering defect that
# drops the scheme or port from issuer_pattern does NOT crash the pod -- it
# 401s every request, quietly, with every other check in this file (which
# probe Keycloak/the realm/the UI bundle, never the gears' own rendered
# config's issuer) still reporting green.
#
# THE DOTS ARE BACKSLASH-ESCAPED in the rendered line, because
# issuer_pattern is a regex (see entrypoint.sh) -- the rendered line reads
# `https://10\.136\.20\.200/...` and does NOT contain the literal issuer
# string; strip the backslashes before matching (verified: `case` on the raw
# line does not match, on the stripped line it does).
iss_line="$(kubectl exec -n "$NAMESPACE" deploy/qa-platform-gears -- grep -h 'issuer_pattern:' /var/lib/cf-gears/.rendered-qa-platform-stack.yaml 2>/dev/null || true)"
iss_unescaped="${iss_line//\\/}"
case "$iss_line" in
    "")
        echo "FAIL: could not read issuer_pattern from the gears' rendered /var/lib/cf-gears/.rendered-qa-platform-stack.yaml" >&2
        exit 1 ;;
    *)
        case "$iss_unescaped" in
            *"$want_issuer"*)
                echo "PASS: gears' rendered config has issuer_pattern matching '$want_issuer' ->${iss_line}" ;;
            *)
                echo "FAIL: gears' rendered issuer_pattern is not '$want_issuer' ->${iss_line}. entrypoint.sh derives this from PUBLIC_ISSUER_ORIGIN (gears-deployment.yaml sets it from .Values.publicOrigin) -- if the scheme or port is wrong, PUBLIC_ORIGIN this script was invoked with does not match what the gears Deployment actually received." >&2
                exit 1 ;;
        esac ;;
esac

step "5: the deployed UI bundle's baked-in issuer"
# VITE_OIDC_ISSUER is inlined by vite at build time -- the deployed BUNDLE,
# not the source tree, is the only place the running UI's opinion about the
# IdP can be read back.
#
# A SINGLE presence-required-for-PASS read, not an asymmetric pair (unlike
# checks 2/9) -- `${n_iss:-0}` defaulting to 0 on a failed exec still
# correctly takes the FAIL branch below, the same safe shape as checks
# 13/15. Not routed through grep_count() because there is no "absence" half
# here for an unread value to falsely satisfy.
n_iss="$(kubectl exec -n "$NAMESPACE" deploy/qa-platform-ui -- sh -c "cat /usr/share/nginx/html/assets/*.js 2>/dev/null | grep -acF '$want_issuer'" 2>/dev/null || true)"
if [ "${n_iss:-0}" -ge 1 ] 2>/dev/null; then
    echo "PASS: deployed UI bundle has '$want_issuer' baked in ($n_iss matching lines)"
else
    echo "FAIL: deployed UI bundle does NOT contain '$want_issuer' -- it was built without --build-arg VITE_OIDC_ISSUER=$want_issuer, or PUBLIC_ORIGIN here does not match what deploy-k8s.sh built the image with." >&2
    exit 1
fi

step "6: the public origin is served by the IN-CLUSTER UI pod, and answers 200"
# "k8s 4/4" below only exercises the Keycloak-proxy location block
# (ui-extraconf-configmap.yaml's `^/(realms|resources)/` regex); the
# SPA's own `location /` is a DIFFERENT block, and nothing else in this file
# fetches it over the wire -- check 5 above only reads the bundle's files via
# `kubectl exec`, never through nginx. No retry budget: nginx opens its
# listener at start and does not import a realm, so this either answers or it
# is broken.
#
# A 200 ALONE IS NOT EVIDENCE THE IN-CLUSTER UI SERVED IT. hostPort 80/443 is a
# node-wide reservation, so any other web server already holding those ports
# answers `curl $PUBLIC_ORIGIN/` with a 200 just as happily -- and every FAIL
# further down would then be read against the wrong process. deploy-k8s.sh
# preflights that 80/443 are free before installing (see its port preflight),
# and this check closes the other end: it compares the LEAF CERTIFICATE the
# origin actually presents against the ui.crt inside the cluster's own
# qa-platform-tls Secret. Those two agree only if the pod terminating this TLS
# connection is the one this chart's certs Job minted a leaf for; an unrelated
# server's certificate cannot match.
UI_CRT_B64="$WORKDIR/ui.crt.b64"
UI_CRT="$WORKDIR/ui.crt"
rc=0
kubectl -n "$NAMESPACE" get secret qa-platform-tls -o jsonpath='{.data.ui\.crt}' > "$UI_CRT_B64" 2>"$WORKDIR/uicrt.get.err" || rc=$?
if [ "$rc" -ne 0 ] || [ ! -s "$UI_CRT_B64" ]; then
    echo "FAIL: could not read secret/qa-platform-tls's ui.crt key in namespace $NAMESPACE (exit $rc): $(cat "$WORKDIR/uicrt.get.err" 2>/dev/null)" >&2
    exit 1
fi
rc=0
base64 -d "$UI_CRT_B64" > "$UI_CRT" 2>"$WORKDIR/uicrt.decode.err" || rc=$?
if [ "$rc" -ne 0 ] || [ ! -s "$UI_CRT" ]; then
    echo "FAIL: base64-decoding secret/qa-platform-tls's ui.crt produced nothing usable (exit $rc): $(cat "$WORKDIR/uicrt.decode.err" 2>/dev/null)" >&2
    exit 1
fi

# host[:port] out of PUBLIC_ORIGIN, defaulting to 443 -- the origin regex at
# the top of this file has already guaranteed the `scheme://host[:port]`
# shape, so this needs no defensive parsing beyond supplying the default port.
ORIGIN_HOSTPORT="${PUBLIC_ORIGIN#*://}"
case "$ORIGIN_HOSTPORT" in
    *:*) : ;;
    *)   ORIGIN_HOSTPORT="$ORIGIN_HOSTPORT:443" ;;
esac
rc=0
openssl s_client -connect "$ORIGIN_HOSTPORT" -CAfile "$CA_CRT" \
    < /dev/null > "$WORKDIR/s_client.out" 2>"$WORKDIR/s_client.err" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: openssl s_client could not complete a TLS handshake with $ORIGIN_HOSTPORT (exit $rc): $(tail -n3 "$WORKDIR/s_client.err" 2>/dev/null). Nothing is listening on 443, or what is listening presents a certificate that does not chain to the cluster's dev CA -- the latter is itself the 'a DIFFERENT nginx owns this port' symptom this check exists to name." >&2
    exit 1
fi
# `awk` to a file, not a pipe into openssl: the extraction and the parse are
# two steps whose exit codes are both wanted separately (this file's header).
awk '/-----BEGIN CERTIFICATE-----/{c++} c==1{print} /-----END CERTIFICATE-----/{if(c==1) exit}' \
    "$WORKDIR/s_client.out" > "$WORKDIR/served.crt"
if [ ! -s "$WORKDIR/served.crt" ]; then
    echo "FAIL: the TLS handshake with $ORIGIN_HOSTPORT succeeded but no certificate could be extracted from openssl s_client's output -- treat this as a broken check rather than a broken deployment, and inspect $WORKDIR/s_client.out by hand." >&2
    exit 1
fi
rc=0
served_fp="$(openssl x509 -in "$WORKDIR/served.crt" -noout -fingerprint -sha256 2>"$WORKDIR/fp.err")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$served_fp" ]; then
    echo "FAIL: could not fingerprint the certificate served at $ORIGIN_HOSTPORT (exit $rc): $(cat "$WORKDIR/fp.err" 2>/dev/null)" >&2
    exit 1
fi
rc=0
want_fp="$(openssl x509 -in "$UI_CRT" -noout -fingerprint -sha256 2>"$WORKDIR/fp2.err")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$want_fp" ]; then
    echo "FAIL: could not fingerprint secret/qa-platform-tls's ui.crt (exit $rc): $(cat "$WORKDIR/fp2.err" 2>/dev/null)" >&2
    exit 1
fi
if [ "$served_fp" != "$want_fp" ]; then
    echo "FAIL: $ORIGIN_HOSTPORT is served by something OTHER than this chart's UI pod. The leaf it presents is [$served_fp]; secret/qa-platform-tls's ui.crt is [$want_fp]. hostPort 80/443 is a node-wide reservation, so the likely cause is that another process on this node holds it: 'ss -ltnp' names the process. Stop it, then 'kubectl -n $NAMESPACE delete pod -l app.kubernetes.io/component=ui' so the UI pod can bind the port. Every check below that goes through \$PUBLIC_ORIGIN would otherwise be measuring the wrong process." >&2
    exit 1
fi
echo "PASS: the leaf served at $ORIGIN_HOSTPORT is byte-identical to secret/qa-platform-tls's ui.crt -- the in-cluster UI pod owns this origin"

rc=0
curl -s -o /dev/null -w '%{http_code}' --max-time 10 --cacert "$CA_CRT" "$PUBLIC_ORIGIN/" > "$WORKDIR/ui-root.code" 2>"$WORKDIR/ui-root.err" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: curl exited $rc fetching $PUBLIC_ORIGIN/: $(cat "$WORKDIR/ui-root.err" 2>/dev/null)" >&2
    exit 1
fi
ui_code="$(cat "$WORKDIR/ui-root.code")"
if [ "$ui_code" = "200" ]; then
    echo "PASS: the UI answers 200 at $PUBLIC_ORIGIN/ over TLS verified against the dev CA"
else
    echo "FAIL: $PUBLIC_ORIGIN/ answered '$ui_code' rather than 200. A TLS handshake failure (rather than a real HTTP code) means the ui leaf's SANs do not cover this host, or 443 is not published; 'kubectl -n $NAMESPACE logs deploy/qa-platform-ui' names an nginx config error in ui-tls/tls.conf." >&2
    exit 1
fi

step "6b: /qa/v1 reaches the gears THROUGH nginx -- 401, never 502"
# THE ONLY CHECK IN THIS FILE THAT EXERCISES THE API PROXY PATH END TO END,
# and the only one that can catch a wrong `clusterDns` (values.yaml:9, the
# address that becomes nginx's `resolver`). Nothing else here notices it:
# check 6 above fetches the SPA off nginx's own filesystem with no upstream
# involved, and the Keycloak checks (k8s 4/4, and the realm check) reach a
# DIFFERENT location block. If `resolver` points at an address that is not
# the cluster's kube-dns, `location /qa/v1/`'s variable `proxy_pass $gears`
# cannot resolve `qa-platform-gears` at REQUEST time and every API call
# 502s -- a stack that looks perfectly healthy from every other check in
# this file while the application is entirely non-functional.
#
# 401 IS THE PASS, AND THAT IS THE POINT. An unauthenticated
# GET /qa/v1/environments is refused by the gears themselves, so a 401 proves
# the request travelled the WHOLE path: TLS termination at nginx, the
# `location /qa/v1/` block, resolver lookup of qa-platform-gears, the
# Service, the pod, and the gears' own auth layer answering. Any 5xx --
# 502 in particular -- means the request never reached the gears at all.
# A 200 would be just as wrong as a 502 (it would mean the endpoint is
# unauthenticated), so both are failed explicitly rather than "not 502".
rc=0
curl -s -o "$WORKDIR/api.body" -w '%{http_code}' --max-time 15 --cacert "$CA_CRT" \
    "$PUBLIC_ORIGIN/qa/v1/environments" > "$WORKDIR/api.code" 2>"$WORKDIR/api.err" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: curl exited $rc fetching $PUBLIC_ORIGIN/qa/v1/environments: $(cat "$WORKDIR/api.err" 2>/dev/null)" >&2
    exit 1
fi
api_code="$(cat "$WORKDIR/api.code")"
case "$api_code" in
    401)
        echo "PASS: $PUBLIC_ORIGIN/qa/v1/environments answered 401 -- the request reached the gears through nginx's /qa/v1 proxy, so the resolver (clusterDns=$(kubectl -n "$NAMESPACE" get deploy qa-platform-ui -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="NGINX_RESOLVER")].value}' 2>/dev/null || echo '<unreadable>')) resolves qa-platform-gears" ;;
    502|503|504)
        echo "FAIL: $PUBLIC_ORIGIN/qa/v1/environments answered $api_code -- nginx could not reach the gears. The prime suspect is values.yaml's clusterDns: it becomes nginx's \`resolver\` (NGINX_RESOLVER on the ui Deployment), and \`location /qa/v1/\` uses a VARIABLE proxy_pass, which resolves the upstream name at REQUEST time through that resolver. Read the real address with 'kubectl -n kube-system get svc kube-dns -o jsonpath={.spec.clusterIP}' and compare it with the NGINX_RESOLVER env on deploy/qa-platform-ui. 'kubectl -n $NAMESPACE logs deploy/qa-platform-ui' shows the resolver error verbatim." >&2
        exit 1 ;;
    200)
        echo "FAIL: $PUBLIC_ORIGIN/qa/v1/environments answered 200 WITHOUT a bearer token. The proxy path works, but the endpoint is answering unauthenticated -- that is a worse finding than the one this check was written for, not a pass." >&2
        exit 1 ;;
    *)
        echo "FAIL: $PUBLIC_ORIGIN/qa/v1/environments answered '$api_code', expected 401. A 404 means nginx matched a different location block than \`location /qa/v1/\` (see default.conf.template's warning about the trailing-extension regex); anything else, read 'kubectl -n $NAMESPACE logs deploy/qa-platform-ui'. Body: $(head -c 300 "$WORKDIR/api.body" 2>/dev/null)" >&2
        exit 1 ;;
esac

step "7: the gears' rendered config selects the persistent credstore backend"
# THE THREE-PLACE SWITCH: the Dockerfile's ARG CARGO_FEATURES default,
# deploy/cargo-features.argo, and credstore.config.vendor in
# qa-platform-stack.yaml must all agree, or secrets silently live in a HashMap
# and die on every pod restart -- which is how this got noticed (an SSH key
# lost four times in one day). Read as a rendered-config claim AND a database fact (this check
# and the next), because either alone is ambiguous.
if kubectl exec -n "$NAMESPACE" deploy/qa-platform-gears -- grep -qE '^      vendor: "constructorfabric-postgres"$' /var/lib/cf-gears/.rendered-qa-platform-stack.yaml 2>/dev/null; then
    echo "PASS: the rendered config selects vendor constructorfabric-postgres"
else
    live_vendor="$(kubectl exec -n "$NAMESPACE" deploy/qa-platform-gears -- sh -c "sed -n '/^  credstore:/,/^  [a-z]/p' /var/lib/cf-gears/.rendered-qa-platform-stack.yaml | grep vendor" 2>/dev/null || true)"
    echo "FAIL: the rendered config does not select the persistent credstore backend (found: ${live_vendor:-<no vendor line>}). Secrets will live in a HashMap and die on the next pod restart -- set credstore.config.vendor to \"constructorfabric-postgres\" in gears/qa-platform/config/qa-platform-stack.yaml." >&2
    exit 1
fi

step "8: credstore.credstore_plugin_values exists (the persistent plugin's own migration ran)"
if ! psql_count credstore "select count(*) from information_schema.tables where table_name='credstore_plugin_values'" "credstore.credstore_plugin_values row count"; then
    exit 1
fi
n_tbl="$PSQL_COUNT_VAL"
if [ "$n_tbl" = "1" ]; then
    echo "PASS: credstore.credstore_plugin_values exists -- the persistent plugin registered and its migration ran against the real database"
else
    echo "FAIL: credstore.credstore_plugin_values does not exist (got $n_tbl matching tables, wanted 1). 'postgres-credstore' is missing from CARGO_FEATURES -- with this chart that list comes from deploy/cargo-features.argo, check it and that the image was built from it." >&2
    exit 1
fi

step "9: the deployed binary carries the VHP product plugin"
# **REWRITTEN BY TASK 19b, and the old check had to go rather than be fixed.**
# It read two strings out of the binary to prove the `platform-observation`
# cargo feature was selected. Task 19 deleted that feature's observation half:
# observation runs through the product plugin now, and the feature was renamed
# `runner-secret` for the one thing it still gates. Both strings the old check
# counted went with the code that logged them, so it had no correct outcome
# left -- delete the #[cfg] arms and it FAILS, keep the ticker and drop its
# gate (which is what happened) and it PASSES while asserting a cargo feature
# that no longer exists. A check that passes for a reason that has ceased to
# exist is worse than one that fails: the failure gets investigated, the pass
# gets trusted.
#
# What is worth checking after Task 19 is that the binary carries the thing
# observation now depends on -- the product plugin itself. Without it every
# environment records "no product plugin can observe it" and nothing refreshes,
# which is the same operator-visible symptom the old check was aimed at.
if ! grep_count deploy/qa-platform-gears 'VHP product plugin registered' /usr/local/bin/cf-gears-example-server "gears binary: 'VHP product plugin registered' string count"; then
    exit 1
fi
n_plugin="$GREP_COUNT_VAL"
if ! grep_count deploy/qa-platform-gears 'observation ticker started' /usr/local/bin/cf-gears-example-server "gears binary: 'observation ticker started' string count"; then
    exit 1
fi
n_ticker="$GREP_COUNT_VAL"
if [ "$n_plugin" -ge 1 ] && [ "$n_ticker" -ge 1 ]; then
    echo "PASS: the deployed binary carries the VHP product plugin and the observation ticker (plugin=$n_plugin ticker=$n_ticker)"
else
    echo "FAIL: the deployed binary is missing the product plugin or the observation ticker (plugin=$n_plugin ticker=$n_ticker; wanted both >=1). The plugin comes from the 'qa-platform' feature, which links qa-vhp-product-plugin in -- check deploy/cargo-features.argo and that the image was built from it." >&2
    exit 1
fi

step "9b: GET /qa/v1/product-plugins answers 200 with a non-empty catalogue -- the gear REGISTERED the VHP plugin, not merely linked it"
# STRICTLY STRONGER THAN CHECK 9 ABOVE, and that relationship is the reason
# this exists rather than being folded into it. Check 9 greps the BINARY for
# the string 'VHP product plugin registered' -- proof the plugin is compiled
# in, nothing more. It cannot see whether the running gear's own startup ever
# reached the registration call, or whether the route that serves the
# catalogue answers at all. GET /qa/v1/product-plugins is now the ONLY way
# any client -- the UI, an operator, a script -- learns a valid
# plugin_instance_id, and qa-catalog's schema and API now require one on
# every product create (qa-catalog/src/api/rest/dto.rs,
# qa-catalog/src/domain/service/validation.rs). An empty catalogue here means
# every product create is impossible and every environment goes unobserved --
# and check 9 would still PASS, because the string is still sitting in the
# binary. A user hit exactly this on a freshly wiped cluster by hand; nothing
# in this file noticed until now.
#
# READING A FAILURE HERE AGAINST CHECK 9's RESULT is the intended diagnostic:
#   check 9 PASS, this check FAIL -> linked but not answering. The plugin is
#     compiled in; something in the running gear's own startup did not
#     register it, or the route itself is broken. Read
#     'kubectl -n $NAMESPACE logs deploy/qa-platform-gears' for the
#     registration error -- do NOT go looking at cargo features, check 9
#     already cleared those.
#   check 9 FAIL too -> the build itself is missing the plugin; see check 9's
#     own FAIL text and fix that first, this check's failure is downstream of it.
#
# AUTHENTICATION -- RULING G-6, and the obvious reading ("reuse whatever the
# existing authenticated checks do") is wrong because THERE ARE NO EXISTING
# AUTHENTICATED CHECKS: every other HTTP probe in this file is deliberately
# unauthenticated (checks 6, 6b, k8s 4/4 all assert 401/200-without-a-token
# shapes on purpose). Check 16's own header records that an earlier version
# of THIS FILE acquired a MASTER-REALM ADMIN token against
# /admin/realms/qa-platform/clients with the committed admin/admin
# credentials, and was deliberately rewritten to stop -- that shape forced
# the admin console and the whole admin REST API onto the application's
# PUBLIC origin for one check's convenience, and was the proxy's only
# consumer. This check does NOT repeat that: it takes a client-credentials
# token for the qa-platform-workflow SERVICE-ACCOUNT client (confirmed in
# the chart's files/keycloak/realm-qa-platform.json: serviceAccountsEnabled=true,
# with the secret pinned from argo.workflowClientSecret) from the realm's ORDINARY token
# endpoint, /realms/qa-platform/protocol/openid-connect/token. That endpoint
# is not new surface -- it is already published and already exercised
# unauthenticated by check 6/"k8s 4/4"'s discovery-document fetch, which
# reads .token_endpoint off the exact same document this literal URL matches.
# No admin API is involved, and the realm PINS this client's secret, which is
# precisely the property keycloak-deployment.yaml's own header records the
# pin exists to guarantee across an H2 discard (Keycloak's store is
# ephemeral and re-imports the realm, secret included, on every restart).
#
# PLACED HERE, AFTER KEYCLOAK IS ALREADY KNOWN Available (step 0's rollout
# wait) and its discovery document already proven reachable (check 6/"k8s
# 4/4"), because unlike its unauthenticated neighbours this is the one check
# in the file that needs a real login flow to succeed, not just a listener
# that answers.
#
# THREE OUTCOMES, DELIBERATELY DISTINGUISHED -- a check that cannot tell them
# apart is worse than no check, per this task's own brief:
#   (a) no token at all (Keycloak/secret problem) -- FAIL naming the token
#       failure, and say so explicitly: the catalogue was NOT reached, this is
#       not evidence of an empty one.
#   (b) a token, but the catalogue route answers something other than 200 --
#       FAIL with the status and body.
#   (c) 200 with `[]` -- the real failure this check exists for.
rc=0
curl -s -o "$WORKDIR/plugin-token.json" -w '%{http_code}' --max-time 15 --cacert "$CA_CRT" \
    -X POST "$PUBLIC_ORIGIN/realms/qa-platform/protocol/openid-connect/token" \
    -H 'Content-Type: application/x-www-form-urlencoded' \
    --data-urlencode 'grant_type=client_credentials' \
    --data-urlencode 'client_id=qa-platform-workflow' \
    --data-urlencode 'client_secret=qa-platform-workflow-dev-secret' \
    > "$WORKDIR/plugin-token.code" 2>"$WORKDIR/plugin-token.err" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: curl exited $rc requesting a client-credentials token from $PUBLIC_ORIGIN/realms/qa-platform/protocol/openid-connect/token: $(cat "$WORKDIR/plugin-token.err" 2>/dev/null). The product-plugin catalogue itself was NOT reached -- this is a token-acquisition failure, not evidence of an empty catalogue." >&2
    exit 1
fi
token_code="$(cat "$WORKDIR/plugin-token.code")"
if [ "$token_code" != "200" ]; then
    echo "FAIL: the qa-platform-workflow client-credentials token request answered $token_code, not 200 (body: $(head -c 300 "$WORKDIR/plugin-token.json" 2>/dev/null)). The product-plugin catalogue itself was NOT reached. Compare the deployed realm's secret against the pinned value: kubectl -n $NAMESPACE get secret qa-platform-realm -o jsonpath='{.data.realm-qa-platform\.json}' | base64 -d | jq -r '.clients[] | select(.clientId==\"qa-platform-workflow\") | .secret, .serviceAccountsEnabled' should show qa-platform-workflow-dev-secret and true." >&2
    exit 1
fi
rc=0
plugin_token="$(jq -r '.access_token // empty' "$WORKDIR/plugin-token.json" 2>"$WORKDIR/plugin-token.jq.err")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$plugin_token" ]; then
    echo "FAIL: the token endpoint answered 200 but no usable access_token could be read from its body ($(cat "$WORKDIR/plugin-token.jq.err" 2>/dev/null); body: $(head -c 300 "$WORKDIR/plugin-token.json" 2>/dev/null)). The product-plugin catalogue itself was NOT reached." >&2
    exit 1
fi

rc=0
curl -s -o "$WORKDIR/plugins.json" -w '%{http_code}' --max-time 15 --cacert "$CA_CRT" \
    -H "Authorization: Bearer $plugin_token" \
    "$PUBLIC_ORIGIN/qa/v1/product-plugins" > "$WORKDIR/plugins.code" 2>"$WORKDIR/plugins.err" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: curl exited $rc fetching $PUBLIC_ORIGIN/qa/v1/product-plugins with a valid token: $(cat "$WORKDIR/plugins.err" 2>/dev/null)" >&2
    exit 1
fi
plugins_code="$(cat "$WORKDIR/plugins.code")"
if [ "$plugins_code" != "200" ]; then
    echo "FAIL: $PUBLIC_ORIGIN/qa/v1/product-plugins answered $plugins_code with a valid bearer token, not 200 (body: $(head -c 300 "$WORKDIR/plugins.json" 2>/dev/null)). A live token failing against the route itself, not a Keycloak problem -- 'kubectl -n $NAMESPACE logs deploy/qa-platform-gears' names the cause." >&2
    exit 1
fi
rc=0
n_plugins_reg="$(jq -r 'if type=="array" then length else "not-an-array" end' "$WORKDIR/plugins.json" 2>"$WORKDIR/plugins.jq.err")" || rc=$?
if [ "$rc" -ne 0 ] || ! [[ "$n_plugins_reg" =~ ^[0-9]+$ ]]; then
    echo "FAIL: $PUBLIC_ORIGIN/qa/v1/product-plugins answered 200 but its body could not be read as a JSON array (got '$n_plugins_reg'): $(cat "$WORKDIR/plugins.jq.err" 2>/dev/null). Body: $(head -c 300 "$WORKDIR/plugins.json" 2>/dev/null)" >&2
    exit 1
fi
if [ "$n_plugins_reg" -eq 0 ]; then
    echo "FAIL: $PUBLIC_ORIGIN/qa/v1/product-plugins answered 200 with an EMPTY array. No product plugin is registered: no product can be created (qa-catalog now requires a plugin_instance_id drawn from this exact list on every create) and no environment can be observed. Check 9 above already proved the VHP plugin is linked into the binary -- if check 9 PASSED, this is a registration-at-startup failure, not a missing feature; read 'kubectl -n $NAMESPACE logs deploy/qa-platform-gears' for the registration error, not the cargo feature list." >&2
    exit 1
fi
n_with_id="$(jq -r '[.[] | select(.instance_id != null and .instance_id != "")] | length' "$WORKDIR/plugins.json" 2>"$WORKDIR/plugins.jq2.err" || true)"
case "$n_with_id" in
    ''|*[!0-9]*)
        echo "FAIL: could not count $PUBLIC_ORIGIN/qa/v1/product-plugins entries carrying a non-empty instance_id ($(cat "$WORKDIR/plugins.jq2.err" 2>/dev/null)). Body: $(head -c 300 "$WORKDIR/plugins.json" 2>/dev/null)" >&2
        exit 1 ;;
esac
if [ "$n_with_id" -lt 1 ]; then
    echo "FAIL: $PUBLIC_ORIGIN/qa/v1/product-plugins answered 200 with $n_plugins_reg entries, but NONE carries a non-empty instance_id. instance_id is the value a product's plugin_instance_id must match verbatim (qa-catalog's ProductPluginDto) -- this is functionally the same failure as an empty catalogue, since no product can be created against any of these entries." >&2
    exit 1
fi
echo "PASS: $PUBLIC_ORIGIN/qa/v1/product-plugins answered 200 with $n_plugins_reg registered plugin(s), $n_with_id carrying a non-empty instance_id -- the gear REGISTERED the VHP plugin (cf. check 9, which only proves it is linked in) and the route every product create and environment observation now depends on actually answers"

step "10: environment observation actually ran against the real database"
# The DATABASE FACT half of check 9: proof it did not just get selected but
# actually ran -- an environment row with a non-null version_detected_at, which
# only the ticker or POST /qa/v1/environments/{id}/refresh ever writes.
# COLD START, DELIBERATELY NOT A FAILURE: a brand-new deployment has no
# qa_environments rows at all, so there is nothing to have observed yet -- a
# NOTE, not folded into a FAIL. Both queries must themselves succeed and
# return a plain count (psql_count's own job) BEFORE this cold-start logic
# ever runs -- fixed in review round 1: the previous `${n:-0}` form could not
# tell "zero rows, confirmed" apart from "the query failed and returned
# nothing", so a broken exec could silently read as a legitimate cold start.
if ! psql_count qa_environments "select count(*) from qa_environments" "qa_environments row count"; then
    exit 1
fi
n_platforms="$PSQL_COUNT_VAL"
if ! psql_count qa_environments "select count(*) from qa_environments where version_detected_at is not null" "qa_environments.version_detected_at observed-row count"; then
    exit 1
fi
n_observed="$PSQL_COUNT_VAL"
if [ "$n_platforms" -eq 0 ]; then
    echo "NOTE: qa_environments has no rows yet -- a cold-start deployment with nothing registered cannot show a non-null version_detected_at. Not treated as a failure; create an environment and re-run this check."
elif [ "$n_observed" -ge 1 ]; then
    echo "PASS: $n_observed of $n_platforms environment row(s) carry a non-null version_detected_at -- observation actually ran against a real cluster"
else
    echo "FAIL: qa_environments has $n_platforms row(s) but none has a non-null version_detected_at -- the ticker is compiled in (check 9) but no observation has ever completed. Every environment needs a product whose plugin is registered; a row with no product records that in version_detect_error. If an environment was JUST created, give the ticker's first tick a few seconds before treating this as real." >&2
    exit 1
fi

step "11: health observation actually ran"
# **REPOINTED BY TASK 19b.** This queried `cluster_status`, which Task 19
# dropped; `health_state` is the column the plugin path writes. Same
# cold-start-is-a-NOTE treatment as check 10: the ticker fires every five
# minutes, so a freshly deployed stack can legitimately show nothing observed
# yet. `psql_count` itself fails loudly if the query cannot be verified,
# rather than this check having no way to fail at all.
#
# `health_state` is NOT NULL with a default of 'unknown', so "never observed"
# is a value rather than a NULL -- which is why this counts rows that have
# moved OFF the default, not rows that are non-null.
if ! psql_count qa_environments "select count(*) from qa_environments where health_state <> 'unknown'" "qa_environments.health_state observed-row count"; then
    exit 1
fi
n_health_observed="$PSQL_COUNT_VAL"
if [ "$n_health_observed" -eq 0 ]; then
    echo "NOTE: no qa_environments row has a health_state other than 'unknown' yet -- the observation ticker runs every five minutes and may not have fired since this stack (or these environments) came up. Not treated as a failure; re-run this check shortly."
else
    echo "PASS: $n_health_observed environment row(s) carry an observed health_state -- health observation actually ran through the product plugin"
fi

step "12: the health_detail leak canary (D-CH-5)"
# **REPOINTED BY TASK 19b, NOT DELETED, AND THAT IS THE POINT.** This watched
# `cluster_status_message`, which Task 19 dropped. A canary left pointing at a
# dropped column becomes a check that can only fail, which is how a canary gets
# commented out -- so it moves to the column that now carries the same class of
# text: `health_detail`, written from a plugin's CLASSIFIED failure detail and
# never a formatted error.
#
# `observed_attrs` is watched too, and is new here: it is a JSONB map a plugin
# fills, bounded by `retain_declared`, and it reaches the environment page the
# same way -- exactly the persist -> DTO -> page chain of the 2026-08-28
# incident.
#
# A match is a FAIL, not a NOTE, cold start or not, because the whole point of
# a canary is that it should never fire. The offending value itself is never
# printed, only the count and the column name, so the canary cannot become a
# second leak. `psql_count`'s own failure path is what makes an exec/query
# failure FAIL rather than read as a clean PASS: "a security canary that goes
# green without looking is worse than no canary".
if ! psql_count qa_environments "select count(*) from qa_environments where health_detail like '%BEGIN%' or health_detail like '%PRIVATE KEY%' or observed_attrs::text like '%BEGIN%' or observed_attrs::text like '%PRIVATE KEY%'" "qa_environments health_detail/observed_attrs leak-canary count"; then
    exit 1
fi
n_leaked="$PSQL_COUNT_VAL"
if [ "$n_leaked" -eq 0 ]; then
    echo "PASS: no qa_environments.health_detail or .observed_attrs row contains BEGIN or PRIVATE KEY (query verified to have run)"
else
    echo "FAIL: $n_leaked qa_environments row(s) have BEGIN or PRIVATE KEY in health_detail or observed_attrs -- a secret is leaking from a plugin observation into the database (D-CH-5). Value withheld deliberately; inspect those columns directly against the deployed database." >&2
    exit 1
fi

step "13: the runner image is in containerd, not merely in Docker"
# The one that produces ImagePullBackOff on every workflow run, minutes in,
# naming a registry the image was never in: docker build alone does not make
# an image visible to k3s's containerd. Read runner_image out of the
# ConfigMap gears-argo-configmaps.yaml renders, not out of values.yaml directly,
# so this is checking what the CLUSTER actually has, not what the chart
# source merely intends.
rc=0
kubectl -n "$NAMESPACE" get configmap qa-platform-gears-argo-qa-runs -o jsonpath='{.data.qa-runs-argo\.yaml}' > "$WORKDIR/qa-runs-argo.yaml" 2>"$WORKDIR/cm.err" || rc=$?
if [ "$rc" -ne 0 ] || [ ! -s "$WORKDIR/qa-runs-argo.yaml" ]; then
    echo "FAIL: could not read configmap/qa-platform-gears-argo-qa-runs in namespace $NAMESPACE (exit $rc): $(cat "$WORKDIR/cm.err" 2>/dev/null)" >&2
    exit 1
fi
image="$(grep -E '^        runner_image:' "$WORKDIR/qa-runs-argo.yaml" | sed 's/.*runner_image:[[:space:]]*//' | tr -d '"' || true)"
if [ -z "$image" ]; then
    echo "FAIL: no runner_image line found in configmap/qa-platform-gears-argo-qa-runs -- gears-argo-configmaps.yaml's qa-runs fragment did not render one." >&2
    exit 1
fi
if command -v k3s >/dev/null 2>&1; then CTR=(k3s ctr); elif command -v ctr >/dev/null 2>&1; then CTR=(ctr); else
    echo "FAIL: neither 'k3s' nor 'ctr' is on PATH -- cannot check whether containerd has '$image'" >&2
    exit 1
fi
# `| head -n1` on possibly-empty input, same shape audited for check 14/15:
# safe here for the same reason check 15 is -- PASS requires `ctr_match`
# non-empty, so whether `${CTR[@]} images ls` itself failed or genuinely
# listed no matching image, the result is empty and the FAIL branch below
# fires either way.
# THE FULL `repo:tag`, NOT `${image%%:*}`. This check exists SPECIFICALLY to
# catch a tag mismatch -- the config naming qa-platform-pytest-runner:2 while
# containerd only has :1 produces ImagePullBackOff minutes into every workflow
# run, naming a registry the image was never in. Matching on the repository
# alone made that exact case PASS: any tag of the right repository satisfied
# it, which is the one thing it must not do. `grep -F` on the whole
# `repo:tag`, and a `$` anchor via grep -x on the containerd reference is not
# usable here because containerd lists fully-qualified names
# (`docker.io/library/foo:1`) whose PREFIX differs -- so this is a substring
# match on `repo:tag`, which cannot be satisfied by a different tag of the
# same repository.
ctr_match="$("${CTR[@]}" images ls -q 2>/dev/null | grep -F "$image" | head -n1 || true)"
if [ -n "$ctr_match" ]; then
    echo "PASS: containerd has image '$ctr_match' matching the configured '$image' (repository AND tag)"
else
    ctr_repo_only="$("${CTR[@]}" images ls -q 2>/dev/null | grep -F "${image%%:*}" | head -n1 || true)"
    if [ -n "$ctr_repo_only" ]; then
        echo "FAIL: containerd has NO image tagged '$image', only '$ctr_repo_only' -- the repository is right and the TAG is wrong, which is exactly the mismatch that produces ImagePullBackOff on every workflow run. Rebuild with the configured tag: 'bash deploy/runner/build-and-import.sh $image' (that script takes the full name:tag as its first argument)." >&2
    else
        echo "FAIL: containerd has no image matching '$image' at all (not even the '${image%%:*}' repository). Run deploy/runner/build-and-import.sh." >&2
    fi
    exit 1
fi

step "14: qa-environments' runner-credential Secret write into \$ARGO_NAMESPACE (D4) actually succeeds"
# RESTORED in review round 1 -- this is the check that would have caught
# that round's other Critical: rbac-argo.yaml withheld `secrets` entirely,
# which is right for qa-runs' executor (ADR-0001) but wrong for
# qa-environments, a DIFFERENT gear in the SAME pod that legitimately writes
# a Secret by design (decision D4,
# `runner_secret_writer::ensure_runner_secret` in
# qa-environments/src/infra/runner_secret_writer.rs --
# `Api::<Secret>::namespaced(client, argo_namespace)` then a server-side
# apply `patch`). A deployment whose gears hold a cluster-admin kubeconfig
# never surfaces a missing grant; with this scoped ServiceAccount the
# write is Forbidden on every cycle, and the documented symptom is every
# workflow run hanging on FailedMount while observation (checks 10/11 above)
# keeps working and the stack otherwise looks healthy -- nothing in checks
# 2/3/13/15 touches this code path at all, since qa-environments is not
# qa-runs.
#
# THERE IS DELIBERATELY NO `kubeconfig_path` ASSERTION HERE. An absent
# kubeconfig_path making both Argo clients fall back to `Config::infer()` IS
# the intended in-cluster credential path -- see gears-argo-configmaps.yaml's
# own header. What is worth checking is the RUNTIME half below: does inference
# and the Secret write actually SUCCEED.
#
# FIRST, PROVE THE TWO FAILURE STRINGS ARE STILL ALIVE IN THE BINARY, same
# idiom as check 2/9's absence-and-presence pairs: an absence check whose
# pattern has drifted from the source string passes forever and reports
# nothing, this project's own documented failure mode. Both strings are
# compiled in whether or not they are ever logged, so grepping the binary
# for them first proves the two log-absence checks below can still match
# something.
#
# BOTH reads go through grep_count() -- unlike checks 2/9 this pair is
# SYMMETRIC (both halves require >=1, neither is an "absence" half), so an
# unread value defaulting to 0 was already safe here (0 < 1 correctly FAILs
# either way; there was never a false-PASS path). Still routed through
# grep_count() rather than left as the old bare `${n:-0}` form, so this file
# has exactly ONE way to turn a binary-grep exec into a count, not two --
# see this file's header for the invariant that is the point of doing so.
if ! grep_count deploy/qa-platform-gears 'failed to infer a Kubernetes config for the Argo cluster' /usr/local/bin/cf-gears-example-server "gears binary: 'failed to infer...' D4 string count"; then
    exit 1
fi
n_infer_str="$GREP_COUNT_VAL"
if ! grep_count deploy/qa-platform-gears 'failed to apply one of this environment.s runner Secrets' /usr/local/bin/cf-gears-example-server "gears binary: 'failed to apply...' D4 string count"; then
    exit 1
fi
n_apply_str="$GREP_COUNT_VAL"
if [ "$n_infer_str" -lt 1 ] || [ "$n_apply_str" -lt 1 ]; then
    echo "FAIL: a D4 failure string is not in the deployed binary (infer=$n_infer_str apply=$n_apply_str; wanted both >=1). The wording in qa-environments' secret_writer.rs has drifted from what this check greps the log for below, so the two absence checks would pass vacuously without this guard -- update them together." >&2
    exit 1
fi

# THE LOG FETCH ITSELF MUST BE VERIFIED, SEPARATELY FROM SEARCHING IT --
# fixed in review round 2, a genuine Important this check introduced. The
# previous form (`kubectl logs ... | grep '...' | tail -n1 || true`) is safe
# ONLY for a check whose PASS branch requires the searched-for text to be
# PRESENT (check 15 below is that shape, and stays as-is -- see its own
# comment). It is NOT safe here, because THIS check's PASS branch is "NEITHER
# string was found" -- and `tail -n1` on EMPTY INPUT EXITS 0 no matter what
# fed it that empty input: a real absence of the error strings, `kubectl
# logs` itself failing (a typo'd deployment name, a namespace mismatch, an
# API-server hiccup on the logs endpoint, an evicted log buffer), all look
# identical to this pipeline -- empty stdout, exit 0 -- so it took the PASS
# branch and printed "D4 writer is reaching $ARGO_NAMESPACE and succeeding"
# having verified nothing. `pipefail` does not save this either: `tail`'s own
# exit status (0) is what the pipeline reports regardless of what `kubectl
# logs`/`grep` did upstream. Fixed by capturing `kubectl logs`' own exit
# status separately, the same `cmd > file || rc=$?` idiom used throughout
# this file (CA extraction, PVC list, `can-i`) -- FAIL if the log read itself
# did not succeed, and only search the file, now safely on disk, once that
# is confirmed. "Logs unreadable" and "logs read fine, string found" are
# reported as two distinct FAILs with different remedies, not folded
# together.
# THE LOG HALF IS VACUOUS ON A COLD STACK, AND IS NOW GATED ON THAT --
# the fourth false green found in the final review. `materialise_runner_secret`
# (qa-environments/.../environments.rs:1026-1066) runs PER REGISTERED
# ENVIRONMENT: on create, on update, and on every self-heal cycle. The cutover
# starts against an EMPTY database, so there are zero environments, the writer
# never runs, and neither failure string is ever logged -- at which point this
# check's PASS branch ("neither string appears") fires having exercised
# nothing at all and reports the D4 writer as working. Checks 10 and 11 above
# already treat this exact precondition as a NOTE rather than a pass; this one
# now does too.
#
# The STATIC half of the same concern -- can the gears' ServiceAccount even
# create/patch a Secret in $ARGO_NAMESPACE? -- does NOT need an environment to
# exist, and is checked unconditionally in "k8s 3/4" below alongside the
# workflows grant. That is what actually covers a cold stack; this check is
# the runtime confirmation once there is something to observe.
if ! psql_count qa_environments "select count(*) from qa_environments" "qa_environments row count (D4 precondition)"; then
    exit 1
fi
n_d4_platforms="$PSQL_COUNT_VAL"
if [ "$n_d4_platforms" -eq 0 ]; then
    echo "NOTE: qa_environments has no rows -- materialise_runner_secret runs per registered environment, so on a cold-start deployment the D4 writer has never run and the log CANNOT contain either failure string. Reporting PASS here would be reporting on nothing. The RBAC that this writer needs is checked statically in 'k8s 3/4' below, which works on an empty stack; re-run this check once an environment exists."
else
    rc=0
    kubectl -n "$NAMESPACE" logs deploy/qa-platform-gears > "$WORKDIR/gears-d4.log" 2>"$WORKDIR/gears-d4.err" || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "FAIL: could not read the gears' log (exit $rc): $(cat "$WORKDIR/gears-d4.err" 2>/dev/null). This check cannot tell 'D4 is working' from 'the log is unreadable' -- an unreadable log is a FAIL here, not the silent PASS it would otherwise become." >&2
        exit 1
    fi
    infer_line="$(grep 'failed to infer a Kubernetes config for the Argo cluster' "$WORKDIR/gears-d4.log" | tail -n1 || true)"
    secret_line="$(grep 'failed to apply one of this environment.s runner Secrets' "$WORKDIR/gears-d4.log" | tail -n1 || true)"
    if [ -n "$infer_line" ]; then
        echo "FAIL: the gears' log contains '$infer_line' -- qa-environments could not construct ANY Kubernetes client for the Argo cluster (the in-cluster ServiceAccount token or the API server itself is unreachable), independent of RBAC." >&2
        exit 1
    elif [ -n "$secret_line" ]; then
        echo "FAIL: the gears' log contains '$secret_line' -- qa-environments has a client but the Secret write itself is failing. Check the qa-platform-gears ServiceAccount's RBAC for 'secrets' create/patch in namespace $ARGO_NAMESPACE (rbac-argo.yaml), a missing namespace, or a hand-made Secret this writer does not own (see secret_writer.rs's own 409-conflict message)." >&2
        exit 1
    else
        echo "PASS: with $n_d4_platforms environment(s) registered, the gears' log (read successfully, $(wc -l < "$WORKDIR/gears-d4.log" | tr -d ' ') lines) carries neither the Config::infer() failure nor a failed runner-credential Secret write -- qa-environments' D4 writer is reaching $ARGO_NAMESPACE and succeeding"
    fi
fi

step "15: the gears' log shows the argo executor actually connected"
# The adapter lists Workflow during connect, so this line proves the
# in-cluster kubeconfig (Config::infer()), the API server address, the CRDs
# and the RBAC all work -- four things whose individual failures all read as
# "the gears did not start".
#
# `grep 'x' | tail -n1`, never `grep -q`, and UNLIKE CHECK 14 ABOVE (before
# its review-round-2 fix) THIS SHAPE IS SAFE HERE -- confirmed, not assumed,
# because it is worth writing down once rather than re-deriving at every
# call site: this check's PASS branch requires `argo_line` to be NON-EMPTY.
# `tail -n1` on empty input exits 0 regardless of why its input was empty --
# `kubectl logs` itself failing, or genuinely no matching line -- but EITHER
# WAY the result is an empty `argo_line`, which takes the FAIL branch below.
# There is no failure mode in which "kubectl logs could not be read" and
# "the connect line is genuinely absent" produce different (and one of them
# wrongly green) outcomes -- both correctly FAIL, only the FAIL MESSAGE does
# not (yet) distinguish the two causes. That is the opposite of check 14's
# bug, where the ABSENCE branch was the one this check reports as PASS.
argo_line="$(kubectl -n "$NAMESPACE" logs deploy/qa-platform-gears 2>/dev/null | grep 'argo executor connected' | tail -n1 || true)"
if [ -n "$argo_line" ]; then
    echo "PASS: $argo_line"
else
    echo "FAIL: the gears' log has no 'argo executor connected' line (or the log itself could not be read). 'kubectl -n $NAMESPACE logs deploy/qa-platform-gears' names the cause if the container is restarting; an unreachable Argo API server or absent CRDs both abort boot here on purpose." >&2
    exit 1
fi

step "16: the realm Keycloak imports lists the UI origin as a redirect URI"
# READ OUT OF THE qa-platform-realm Secret, NOT OUT OF THE ADMIN API --
# rewritten in the final review together with the removal of the `/admin/`
# proxy (ui-extraconf-configmap.yaml). The previous form obtained a
# master-realm admin token over the PUBLIC ORIGIN with the committed
# admin/admin credentials and called `/admin/realms/qa-platform/clients`,
# which meant the admin console and the entire admin REST API had to be
# published on the application's public origin for a verification check's
# convenience. It was the only consumer of that proxy.
#
# THE SECRET IS NOT A WEAKER SOURCE THAN THE ADMIN API HERE. It is the
# EXACT document Keycloak imports: keycloak-realm-secret.yaml renders it
# for this release's publicOrigin into secret/qa-platform-realm, and
# keycloak-deployment.yaml mounts that at /opt/keycloak/data/import for
# `--import-realm`. Reading the Secret therefore checks the same pin the
# admin API would have -- "does the realm this cluster will import name this
# origin?" -- against the cluster's own copy rather than the file on disk
# that produced it, and unlike the admin API it is answerable BEFORE any
# login flow works and on a Keycloak whose H2 store has just been discarded.
#
# WAS A CONFIGMAP UNTIL THE REALM-SECRECY REVIEW: keycloak-realm-secret.yaml
# used to be keycloak-realm-configmap.yaml, `kind: ConfigMap`, `data:` --
# converted because the realm carries the workflow client's confidential
# secret in the clear (see that file's own header). `kubectl get configmap`
# read `.data.<key>` as plain text; a Secret's `.data.<key>` is base64 no
# matter whether the template writes `data:` or `stringData:` -- Kubernetes
# stores it the same way either way -- so the read below now pipes through
# `base64 -d`. This is the one-word fix Task 3's implementer named and
# deliberately left for this task, in the same paragraph as the reminder
# that the "product-plugin catalogue" check's diagnostic hint text (a few
# hundred lines up) carries the identical `get configmap` typo and needs the
# identical fix -- fixed there too, not just here.
#
# What it does NOT prove is that the import SUCCEEDED, or that a RUNNING
# Keycloak actually reflects this Secret's current content -- a Secret
# volume mount updates on the node without restarting the pod that mounts
# it, and `--import-realm` only runs on Keycloak's own first start (H2 is
# ephemeral by design, see keycloak-deployment.yaml). So a release whose
# publicOrigin changed via `helm upgrade` while the Keycloak pod happened
# not to be replaced yet would have THIS check comparing the NEW Secret
# against the NEW $PUBLIC_ORIGIN and passing, even though the RUNNING
# Keycloak's KC_HOSTNAME -- and therefore its advertised issuer -- is still
# the old one. That blind spot is covered separately, twice, by checks that
# read the RUNNING Keycloak rather than the desired-state Secret:
# keycloak-deployment.yaml's readiness probe gates on
# /realms/qa-platform/.well-known/openid-configuration (which only answers
# after the import), and "k8s 4/4" below fetches that same document
# unauthenticated through the UI nginx and compares its `issuer` field
# against this same $PUBLIC_ORIGIN. Verified live (2026-09-18): pausing the
# Keycloak rollout, then `helm upgrade`-ing publicOrigin to a new value,
# reproduces exactly the case this paragraph describes -- this check (PASS,
# reading the already-updated Secret) and "k8s 4/4" (FAIL, reading the
# still-old running issuer) diverge exactly as predicted, and both agree
# again once the paused rollout is resumed and the new pod is Ready. So the
# split is: those two prove the realm the cluster is ACTUALLY RUNNING
# imported and advertises the right issuer, this one proves the realm
# Keycloak is CONFIGURED to import next carries the right redirect URI.
kubectl -n "$NAMESPACE" get secret qa-platform-realm \
    -o jsonpath='{.data.realm-qa-platform\.json}' 2>"$WORKDIR/realm.err" | base64 -d > "$WORKDIR/realm.json" 2>>"$WORKDIR/realm.err"
# PIPESTATUS[0], NOT $? -- $? after a pipe is base64's exit code, and
# `base64 -d` on empty input (what kubectl writes to stdout when the get
# itself fails, since its error text goes to stderr, captured separately
# above) exits 0. Reading kubectl's own status out of PIPESTATUS is what
# makes the `rc -ne 0` half of the check below actually fire on a real
# `get secret` failure, rather than leaning on the empty-file half alone.
rc="${PIPESTATUS[0]}"
if [ "$rc" -ne 0 ] || [ ! -s "$WORKDIR/realm.json" ]; then
    echo "FAIL: could not read secret/qa-platform-realm's realm-qa-platform.json key in namespace $NAMESPACE (exit $rc): $(cat "$WORKDIR/realm.err" 2>/dev/null). keycloak-realm-secret.yaml renders it from the chart's files/keycloak/realm-qa-platform.json; an empty value means that template produced nothing, or keycloakRealmJson was overridden with an empty file." >&2
    exit 1
fi
rc=0
uris="$(jq -r --arg c qa-platform-ui '.clients[] | select(.clientId==$c) | .redirectUris | join(" ")' "$WORKDIR/realm.json" 2>"$WORKDIR/realm.jq.err")" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: jq could not read the qa-platform-ui client's redirectUris out of the realm Secret (exit $rc): $(cat "$WORKDIR/realm.jq.err" 2>/dev/null). The Secret's value is not the JSON this check expects, or is not valid base64 -- confirm with 'kubectl -n $NAMESPACE get secret qa-platform-realm -o jsonpath={.data.realm-qa-platform\\.json} | base64 -d | head'." >&2
    exit 1
fi
if [ -z "$uris" ]; then
    echo "FAIL: the realm Secret has no qa-platform-ui client, or that client has an empty redirectUris list. An absent client is not an absent problem: the SPA cannot complete a login at all. Check the chart's files/keycloak/realm-qa-platform.json." >&2
    exit 1
fi
case " $uris " in
    *" $PUBLIC_ORIGIN/* "*)
        echo "PASS: the realm this cluster is configured to import lists $PUBLIC_ORIGIN/* as a qa-platform-ui redirect URI (all: [$uris])" ;;
    *)
        echo "FAIL: the realm this cluster is configured to import does NOT list $PUBLIC_ORIGIN/* for qa-platform-ui -- it lists [$uris]. The release was installed with a different publicOrigin than this script was invoked with; re-run with a matching --public-origin, or 'helm upgrade' the release with the right one. Note the realm imports only on Keycloak's FIRST start (H2 is ephemeral by design, see keycloak-deployment.yaml), so after correcting the Secret 'kubectl -n $NAMESPACE delete pod -l app.kubernetes.io/component=keycloak' is what makes the new realm take effect -- 'k8s 4/4' below is what would have caught a pod that needed exactly that and didn't get it." >&2
        exit 1 ;;
esac

step "17: the gears' rendered config carries the OpenTelemetry metrics block"
# THE ONLY SWITCH THAT DECIDES WHETHER 22 METRIC FAMILIES LEAVE THIS STACK,
# and its failure mode is silence. The four qa-platform gears declare their
# series in `domain::metrics` and obtain every instrument from the
# process-global meter provider; with `opentelemetry.metrics.enabled` false
# -- or with the block missing from the rendered config altogether -- that
# provider stays the built-in no-op, every instrument is a no-op, and the
# gears serve normally while nothing is observable. There is NO error, no log
# line and no failed probe to notice, which is exactly why this is a check.
#
# THIS STEP IS ABOUT THE PUSH HALF ONLY, and the observable fact for it on the
# node is the rendered CONFIG rather than an endpoint: an OTLP periodic reader
# (libs/toolkit's `init_metrics_provider`) either was built or was not, and
# there is nothing on this side to ask. The PULL half -- the scrape endpoint,
# which there very much is something to curl -- is step 18b below.
#
# The `enabled: false` this step reads and reports is therefore NOT "metrics
# are off": with the scrape reader attached, a real meter provider is
# installed and every instrument records. It means no collector is being
# pushed to.
#
# READ FROM /var/lib/cf-gears/.rendered-*, not from the ConfigMap: that
# rendered file is what the server actually loaded (entrypoint.sh writes it and
# then rewrites its own `--config` argument to it before exec'ing), which is the
# same reason checks 3, 4 and 7 read it rather than the ConfigMap.
#
# THAT ALONE DOES NOT PROVE THE ConfigMap WAS MOUNTED, and an earlier revision
# of this comment claimed it did. qa-platform.Dockerfile BAKES
# gears/qa-platform/config/qa-platform-stack.yaml into the image at
# /etc/cf-gears/qa-platform-stack.yaml -- exactly the path entrypoint.sh reads
# as GEARS_CONFIG_FILE. With the ConfigMap absent, or mounted somewhere else,
# entrypoint.sh renders the BAKED copy instead: the rendered file still exists,
# still carries a well-formed `opentelemetry:` block, and the committed value of
# the metrics flag is `false` -- so the else branch below would print
# "PASS: ... metrics are DISABLED (the chart default)" to an operator who ran
# `--set opentelemetry.metrics.enabled=true`, and the FAIL text about an
# unmounted ConfigMap could never be reached for that cause. A false green on
# the exact defect this step exists for.
#
# SO THE MOUNT IS PROVEN FIRST, from the rendered file itself, before the flag
# is read at all. The discriminator is `discovery_url`, and it is the only
# clean one: gears-config-configmap.yaml rewrites the committed
# `https://keycloak:8443/realms/qa-platform` to `$PUBLIC_ORIGIN/...` in ITS
# render, unconditionally and on every install (deploy-k8s.sh passes the same
# origin to `--set publicOrigin=`), and entrypoint.sh does NOT touch that key --
# it rewrites `issuer_pattern`, which is a different setting, and leaves
# `discovery_url` alone by design (see that template's own comment on why the
# two differ). So the rewritten value can only have come through the ConfigMap,
# and the committed literal can only have come from the baked image copy.
#
# ASYMMETRIC, like check 2: PASS needs the rewritten form PRESENT and the
# committed literal ABSENT. Presence alone would not catch a partial render, and
# absence alone is what an unreadable exec fakes -- which is why both halves go
# through grep_count() rather than through `${n:-0}`.
#
# ABSENCE IS A FAIL, DISABLED IS A NOTE. `enabled: false` is the chart's
# default and a legitimate deployment state -- an operator who has no
# collector has nothing to point at. A MISSING block is different: it means
# the committed config or the chart's transform lost it, and the operator who
# later sets opentelemetry.metrics.enabled=true would get a successful helm
# upgrade and no metrics.

# ---- the mount, proven before anything is read out of the block ----------
# PUBLIC_ORIGIN is validated above as `scheme://host[:port]`, so the only BRE
# metacharacter it can contain is `.`; escaping it keeps this a literal match
# rather than one where `10.136.20.200` would also match `10x136x20x200`.
origin_re="${PUBLIC_ORIGIN//./\\.}"
if ! grep_count deploy/qa-platform-gears "discovery_url: \"$origin_re/realms/qa-platform\"" \
    /var/lib/cf-gears/.rendered-qa-platform-stack.yaml \
    "rendered config: ConfigMap-rewritten discovery_url count"; then
    exit 1
fi
n_cm="$GREP_COUNT_VAL"
if ! grep_count deploy/qa-platform-gears 'discovery_url: "https://keycloak:8443/realms/qa-platform"' \
    /var/lib/cf-gears/.rendered-qa-platform-stack.yaml \
    "rendered config: committed placeholder discovery_url count"; then
    exit 1
fi
n_baked="$GREP_COUNT_VAL"
if [ "$n_cm" -ge 1 ] && [ "$n_baked" -eq 0 ]; then
    echo "PASS: the rendered config came from ConfigMap qa-platform-gears-config, not from the copy baked into the image (rewritten discovery_url lines=$n_cm, committed-placeholder lines=$n_baked)"
else
    echo "FAIL: the gears rendered their config from the copy BAKED INTO THE IMAGE, not from ConfigMap qa-platform-gears-config (rewritten discovery_url lines=$n_cm, committed-placeholder lines=$n_baked; wanted >=1 and 0). entrypoint.sh reads /etc/cf-gears/qa-platform-stack.yaml, which qa-platform.Dockerfile also bakes in, so an absent or misplaced ConfigMap mount is silent -- the pod starts and serves. Everything this step would report about opentelemetry below would then be the COMMITTED defaults rather than what this release asked for, and every OIDC token would fail validation besides. Check 'kubectl -n $NAMESPACE get deploy qa-platform-gears -o jsonpath={.spec.template.spec.volumes}' and that the volumeMount lands on /etc/cf-gears." >&2
    exit 1
fi

rc=0
kubectl exec -n "$NAMESPACE" deploy/qa-platform-gears -- \
    sed -n '/^opentelemetry:/,/^[^ #]/p' /var/lib/cf-gears/.rendered-qa-platform-stack.yaml \
    > "$WORKDIR/otel.block" 2>"$WORKDIR/otel.err" || rc=$?
if [ "$rc" -ne 0 ] || [ ! -s "$WORKDIR/otel.block" ]; then
    echo "FAIL: the gears' rendered /var/lib/cf-gears/.rendered-qa-platform-stack.yaml carries no 'opentelemetry:' block (exit $rc): $(cat "$WORKDIR/otel.err" 2>/dev/null). Every one of the 22 metric families in DESIGN 3.11 is then unreachable, silently -- the gears report nothing about it. The mount itself is already proven above, so this is the committed config or the chart's transform having lost the block: check gears/qa-platform/config/qa-platform-stack.yaml and gears-config-configmap.yaml's fourth transform." >&2
    exit 1
fi
otel_service="$(sed -n 's/^    service_name: *"\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' "$WORKDIR/otel.block" | head -1)"
otel_endpoint="$(sed -n 's/^      endpoint: *"\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' "$WORKDIR/otel.block" | head -1)"
# The LAST `enabled:` in the block is the metrics one; tracing's comes first.
# Both are read, and BOTH ARE ASSERTED BELOW -- reading tracing without
# checking it would make this comment a claim the step does not perform, which
# is the defect class this whole phase kept producing.
otel_tracing="$(sed -n 's/^    enabled: *\(.*\)$/\1/p' "$WORKDIR/otel.block" | head -1)"
otel_metrics="$(sed -n 's/^    enabled: *\(.*\)$/\1/p' "$WORKDIR/otel.block" | sed -n '2p')"
if [ -z "$otel_metrics" ]; then
    echo "FAIL: the rendered opentelemetry block has no metrics 'enabled:' line -- found only [$(tr '\n' ' ' < "$WORKDIR/otel.block")]. The block's shape changed and this check can no longer tell enabled from disabled; fix the config or this check, but do not leave it reporting on a shape that is gone." >&2
    exit 1
fi
# TRACING MUST STILL BE OFF, and this is the assertion the comment above
# promises. Nothing in this chart turns tracing on: `values.yaml` exposes only
# `opentelemetry.metrics`, and the configmap's fourth transform rewrites the
# two-line `  metrics:\n    enabled: false` sentinel. So a rendered config with
# tracing enabled means either the metrics transform matched the TRACING block
# -- both carry the identical line `    enabled: false`, which is exactly why
# the sentinel is two lines -- or someone hand-edited the committed config.
#
# Checked in BOTH states of the metrics flag, deliberately. The failure this
# catches is loudest in the disabled branch: a transform that rewrote tracing
# instead of metrics leaves metrics reading `false`, and without this check the
# step would print its cheerful "metrics are DISABLED (the chart default)" PASS
# over a stack whose operator asked for metrics and got a tracing pipeline.
# `check_metrics_config.py`'s `check_enabled` holds the same property at render
# time; this holds it against what the server actually loaded.
if [ "$otel_tracing" != "false" ]; then
    echo "FAIL: opentelemetry.tracing.enabled is '$otel_tracing', expected 'false'. No value in this chart turns tracing on, so either the metrics transform in gears-config-configmap.yaml matched the tracing block (both blocks carry the identical line '    enabled: false' -- that is why the metrics sentinel is two lines), or the committed config was hand-edited. Either way the metrics flag read from this file ('$otel_metrics') cannot be trusted to mean what it says." >&2
    exit 1
fi
if [ "$otel_service" != "qa-platform" ]; then
    echo "FAIL: opentelemetry.resource.service_name is '$otel_service', expected 'qa-platform'. Unset, the toolkit attributes every data point to 'cf-gears' and two stacks pushing into one collector are indistinguishable in every query." >&2
    exit 1
fi
if [ "$otel_metrics" = "true" ]; then
    case "$otel_endpoint" in
        ""|http://127.0.0.1:*|http://localhost:*)
            echo "FAIL: metrics are enabled but the exporter endpoint is '$otel_endpoint' -- a pod's own loopback address, where nothing is listening. The reader will push into itself every interval and log an export failure forever while every dashboard stays empty. Re-run helm with --set opentelemetry.metrics.endpoint=<collector>." >&2
            exit 1 ;;
    esac
    echo "PASS: metrics are ENABLED and push to '$otel_endpoint' (service_name=$otel_service, tracing=$otel_tracing)"
else
    echo "PASS: the metrics block is present and correctly shaped, in a config proven above to have come from the ConfigMap; OTLP PUSH is disabled (the chart default). That is not 'nothing is observable' -- step 18b below scrapes the same families off this pod's /metrics, which is on by default. Add a collector too with --set opentelemetry.metrics.enabled=true --set opentelemetry.metrics.endpoint=<collector>."
fi

step "18: the deployed binary carries exactly the metric catalog, name for name"
# THE PHASE'S CENTRAL NAMING RISK, checked against the artifact that is
# actually running. A series exported under a name nobody queries is
# indistinguishable, from inside the process, from a series that works: the
# gears emit, the collector accepts, and the dashboard is empty. The
# constants in each gear's `domain::metrics` are the full literal Prometheus
# names (no `.with_unit()` anywhere, so no suffix is added in translation),
# and each gear's `every_catalog_family_is_exported_under_its_catalog_name`
# is what ties constant to exported name -- in-process. This check closes the
# other half: that the binary on this node carries those exact strings and no
# others.
#
# A SET COMPARISON, NOT A COUNT. A count passes when one name is misspelt and
# another is duplicated. The expected list below is the catalog; a name that
# drifts shows up as a diff naming both sides.
#
# grep -oa OVER THE BINARY is the same technique check 2 and check 9 already
# use to read a build's own content. It proves the string is compiled in, not
# that an instrument was built with it -- which is precisely the half the
# in-process tests already hold. Neither alone is the whole property.
cat > "$WORKDIR/metrics.want" <<'CATALOG'
qa_catalog_bundle_download_total
qa_catalog_plugin_resolution_duration_seconds
qa_catalog_plugin_resolution_total
qa_environments_observation_cycle_duration_seconds
qa_environments_observation_cycle_total
qa_environments_observation_duration_seconds
qa_environments_observation_total
qa_environments_plugin_call_duration_seconds
qa_environments_plugin_call_total
qa_insights_collect_duration_seconds
qa_insights_collect_report_total
qa_insights_collect_total
qa_insights_jira_bug_total
qa_insights_jira_poll_duration_seconds
qa_insights_jira_poll_total
qa_insights_jira_rerun_total
qa_runs_dispatch_decision_total
qa_runs_dispatch_duration_seconds
qa_runs_dispatch_total
qa_runs_free_to_start_duration_seconds
qa_runs_free_to_start_unanchored_total
qa_runs_ingest_duration_seconds
qa_runs_ingest_total
qa_runs_queue_wait_duration_seconds
qa_runs_queue_wait_total
CATALOG
rc=0
kubectl exec -n "$NAMESPACE" deploy/qa-platform-gears -- \
    grep -oaE 'qa_(runs|insights|environments|catalog)_[a-z_]+(_total|_duration_seconds)' \
    /usr/local/bin/cf-gears-example-server \
    > "$WORKDIR/metrics.raw" 2>"$WORKDIR/metrics.err" || rc=$?
# grep exits 1 on no matches, which is a legitimate answer here (an old image)
# and is caught by the emptiness test below, not by rc. Any other exit is the
# exec itself failing, which must never be read as "no metrics".
if [ "$rc" -gt 1 ]; then
    echo "FAIL: could not read the metric names out of the deployed binary (exit $rc): $(cat "$WORKDIR/metrics.err" 2>/dev/null). An unreadable result is UNVERIFIED, not empty." >&2
    exit 1
fi
sort -u "$WORKDIR/metrics.raw" > "$WORKDIR/metrics.have"
if [ ! -s "$WORKDIR/metrics.have" ]; then
    echo "FAIL: the deployed gears binary carries NONE of the catalog series names. The running image predates the observability work, or the metric modules were compiled out. Rebuild and redeploy: deploy/remote/deploy-k8s.sh builds the image the tag in images.gears.tag names." >&2
    exit 1
fi
if diff -u "$WORKDIR/metrics.want" "$WORKDIR/metrics.have" > "$WORKDIR/metrics.diff" 2>&1; then
    echo "PASS: the deployed binary carries exactly the $(grep -c '' "$WORKDIR/metrics.want") catalog series names (DESIGN 3.11)"
else
    echo "FAIL: the deployed binary's metric names are not the catalog. '-' lines are names DESIGN 3.11 documents and the binary does not carry; '+' lines are names the binary carries and the catalog does not document. Either is the defect this check exists for -- an operator's query names one of the '-' lines and gets nothing back, forever, with no error anywhere." >&2
    cat "$WORKDIR/metrics.diff" >&2
    exit 1
fi

step "18b: the scrape endpoint ANSWERS, and a counter on it MOVES when work happens"
# CHECK 18 READS THE BINARY; THIS ONE READS THE RUNNING PROCESS. They catch
# different things and neither substitutes for the other: 18 proves the image
# was compiled with the catalog, this proves the numbers can be got out of the
# pod at all. For the whole life of this stack before the scrape endpoint,
# 18 passed while every one of those 22 families was reachable by nothing --
# push was off (it needs a collector) and no route served them.
#
# THIS CHECK DOES NOT COUNT FAMILIES, and must not be strengthened into doing
# so. 22 is what the binary DEFINES, which is what check 18 asserts; an
# instrument that has never recorded emits no data point, so a healthy endpoint
# on a freshly-rolled pod carries fewer (18 of 22, measured 2026-09-19) and the
# number rises as the stack is exercised. Asserting 22 here would fail on a
# correct deployment. What is asserted instead is that the endpoint answers and
# that a counter MOVES -- see below. A load test
# had to stand up a throwaway OTel collector to read this stack's own numbers.
#
# CURLED FROM THE NODE, AGAINST THE SERVICE ClusterIP. Three constraints meet
# here: the gears image carries no curl, wget, nc or bash (Debian trixie slim
# plus ca-certificates and openssh-client -- see qa-platform.Dockerfile), so
# there is nothing to exec INSIDE the pod; the node's own resolver does not
# serve cluster DNS, so the Service NAME does not resolve here; but a k3s node
# does route ClusterIPs. So: resolve the ClusterIP with kubectl, curl the IP.
# That also makes this a stronger check than hitting the pod IP would be --
# it proves the Service's `targetPort: metrics` actually selects a listening
# container port, which is the wiring most likely to be wrong.
#
# A STATIC RESPONSE MUST NOT PASS. Two scrapes with a real request between
# them, asserting the request counter went UP, is the whole point: a handler
# that returned a fixed blob, a stale cache, or a reader wired to the wrong
# provider would all satisfy "200 with metric-looking text". The counter used
# is the api-gateway's own `http_server_request_duration_count`, bumped by the
# 401 that check 6b already relies on -- and NOT by this check's own scrapes,
# which land on a different listener that the gateway's middleware never sees.
metrics_port="$(kubectl -n "$NAMESPACE" get svc qa-platform-gears \
    -o jsonpath='{.spec.ports[?(@.name=="metrics")].port}' 2>/dev/null || true)"
if [ -z "$metrics_port" ]; then
    echo "FAIL: the qa-platform-gears Service publishes no port named 'metrics'. Either this release was installed with --set opentelemetry.metrics.scrape.enabled=false, or the chart regressed -- deploy/helm/tests/check_metrics_config.py's check_scrape holds the render side of this." >&2
    exit 1
fi
gears_ip="$(kubectl -n "$NAMESPACE" get svc qa-platform-gears -o jsonpath='{.spec.clusterIP}')"
metrics_url="http://${gears_ip}:${metrics_port}/metrics"

scrape_metrics() {
    # $1 = output file. Fails loudly on anything but a 200; an unreadable
    # endpoint is UNVERIFIED, never "no metrics".
    _rc=0
    _code="$(curl -s -o "$1" -w '%{http_code}' --max-time 15 "$metrics_url")" || _rc=$?
    if [ "$_rc" -ne 0 ]; then
        echo "FAIL: curl exited $_rc fetching $metrics_url. The gears pod is not listening on the port its Service and its prometheus.io/port annotation advertise -- check 'kubectl -n $NAMESPACE logs deploy/qa-platform-gears | grep -i \"metrics scrape\"', which logs either the bind or the reason it failed." >&2
        exit 1
    fi
    if [ "$_code" != "200" ]; then
        echo "FAIL: $metrics_url answered $_code, expected 200. A 503 means the process built no meter provider at all (scrape.enabled false in the RENDERED config -- read /var/lib/cf-gears/.rendered-qa-platform-stack.yaml, the way check 17 does); a 404 means it is serving a different path than the annotation advertises." >&2
        exit 1
    fi
}

scrape_metrics "$WORKDIR/scrape.before"
if ! grep -q '^# TYPE ' "$WORKDIR/scrape.before"; then
    echo "FAIL: $metrics_url answered 200 but carries no '# TYPE' line, so it is not a Prometheus exposition. First 300 bytes: $(head -c 300 "$WORKDIR/scrape.before")" >&2
    exit 1
fi
counter_line() { grep -c '^http_server_request_duration_count' "$WORKDIR/$1" 2>/dev/null || true; }
if [ "$(counter_line scrape.before)" = "0" ]; then
    echo "FAIL: $metrics_url carries no http_server_request_duration_count series. That instrument is created by the api-gateway's own middleware on the SAME global meter provider the qa-platform gears use, so its absence means the scrape reader is attached to a different provider than the one the gears record into -- the exact false green this check exists for. Families present: $(grep -c '^# TYPE ' "$WORKDIR/scrape.before")" >&2
    exit 1
fi
before="$(awk '/^http_server_request_duration_count/ {s+=$NF} END {print s+0}' "$WORKDIR/scrape.before")"

# The work. A 401 is a complete request through the gateway's middleware stack
# (check 6b explains why 401 is the right answer here), and the metrics layer
# is OUTSIDE the auth layer -- see gears/system/api-gateway/src/gear.rs, which
# adds http_metrics with `layer` rather than `route_layer` precisely so
# refused requests are still counted.
curl -s -o /dev/null --max-time 15 "http://${gears_ip}:$(kubectl -n "$NAMESPACE" get svc qa-platform-gears -o jsonpath='{.spec.ports[?(@.name=="http")].port}')/qa/v1/environments" || true

scrape_metrics "$WORKDIR/scrape.after"
after="$(awk '/^http_server_request_duration_count/ {s+=$NF} END {print s+0}' "$WORKDIR/scrape.after")"
if [ "$after" -le "$before" ]; then
    echo "FAIL: http_server_request_duration_count was $before before a request and $after after it. The endpoint answers, but its numbers do not move -- a static or cached response, or a reader that is not collecting from the live provider. This is the failure a 200-and-it-looks-like-metrics check would have missed." >&2
    exit 1
fi
echo "PASS: $metrics_url answers 200 with $(grep -c '^# TYPE ' "$WORKDIR/scrape.after") metric families, and http_server_request_duration_count rose $before -> $after across one request -- the numbers are live, not a fixture"

# ========================================================= new for k8s ====
# Four checks for the cluster-shaped concerns the probes above do not touch:
# Helm's no-op-on-unchanged-PodSpec behaviour, PersistentVolumeClaims,
# cross-namespace RBAC, and a single public origin proxying Keycloak through
# the UI's nginx.

step "k8s 1/4: every Deployment rolled to \$IMAGE_TAG"
# THE IMAGE TAG IS NOT COSMETIC (deploy-k8s.sh's own header makes the same
# point): imagePullPolicy: IfNotPresent plus a FIXED tag means a re-deploy of
# unchanged config renders an IDENTICAL PodSpec, Kubernetes correctly does
# nothing, and the deploy reports success while the OLD code keeps serving.
# This is the check that would have caught that -- and the only check in this
# file for which "IMAGE_TAG was never passed in" is a legitimate reason to
# skip rather than fail: standalone invocations before Task 12's interface
# change landed had no way to supply it. A skip is announced with a NOTE,
# never silently treated as a pass.
if [ -z "${IMAGE_TAG:-}" ]; then
    echo "NOTE: IMAGE_TAG is not set -- skipping this check. Pass IMAGE_TAG=<tag> (deploy-k8s.sh already does) to verify a re-deploy actually rolled new pods rather than a no-op reconcile."
else
    for d in qa-platform-gears qa-platform-ui qa-platform-keycloak; do
        rc=0
        got="$(kubectl -n "$NAMESPACE" get deploy "$d" -o jsonpath='{.spec.template.spec.containers[0].image}' 2>"$WORKDIR/img-$d.err")" || rc=$?
        if [ "$rc" -ne 0 ]; then
            echo "FAIL: could not read $d's image: $(cat "$WORKDIR/img-$d.err" 2>/dev/null)" >&2
            exit 1
        fi
        case "$d" in
            qa-platform-keycloak) : ;;   # pinned upstream image (quay.io/keycloak/keycloak:26.0), not built by this deploy
            *) case "$got" in
                   *":$IMAGE_TAG") echo "PASS: $d runs $got" ;;
                   *) echo "FAIL: $d runs $got, expected tag $IMAGE_TAG -- an unchanged PodSpec means helm upgrade did nothing and the OLD code is still serving" >&2; exit 1 ;;
               esac ;;
        esac
    done
fi

step "k8s 2/4: PVCs Bound"
# WaitForFirstConsumer means a mis-scheduled pod shows as Pending, not
# failed, so this is checked directly rather than inferred from pod status.
# ABSENT EVIDENCE IS A FAIL, NOT A PASS: an empty PVC list would make the
# "no non-Bound line found" test below pass vacuously, so the empty case is
# checked and failed explicitly first.
rc=0
kubectl -n "$NAMESPACE" get pvc -o jsonpath='{range .items[*]}{.metadata.name} {.status.phase}{"\n"}{end}' > "$WORKDIR/pvc.txt" 2>"$WORKDIR/pvc.err" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: could not list PVCs in namespace $NAMESPACE: $(cat "$WORKDIR/pvc.err" 2>/dev/null)" >&2
    exit 1
fi
if [ ! -s "$WORKDIR/pvc.txt" ]; then
    echo "FAIL: kubectl returned zero PVCs in namespace $NAMESPACE -- expected at least qa-platform-postgres's pgdata claim and qa-platform-gears's data claim. An empty list is not evidence of health; something is missing." >&2
    exit 1
fi
if grep -qv ' Bound$' "$WORKDIR/pvc.txt"; then
    echo "FAIL: not every PVC is Bound:" >&2
    cat "$WORKDIR/pvc.txt" >&2
    exit 1
fi
echo "PASS: every PVC Bound ($(wc -l < "$WORKDIR/pvc.txt" | tr -d ' ') total)"

step "k8s 3/4: the gears SA can submit Workflows AND write runner-credential Secrets into argo"
# rbac-argo.yaml's Role/RoleBinding live in $ARGO_NAMESPACE, granting
# system:serviceaccount:$NAMESPACE:qa-platform-gears create/get/list/watch/
# patch on workflows.argoproj.io and create/patch on secrets -- this is the
# one check that actually exercises the cross-namespace RoleBinding rather
# than just reading its manifest back.
#
# THE `secrets` VERBS ARE HERE, NOT ONLY IN CHECK 14, AND THAT IS THE POINT.
# Check 14 confirms the D4 writer at RUNTIME, from the gears' log -- which
# says nothing at all until an environment has been registered, so on the cold
# stack this cutover starts from it reports a NOTE and exercises nothing.
# `kubectl auth can-i` needs no environment, no workflow and no traffic: it asks
# the API server's authorizer directly, so it is the half of the D4 RBAC
# question that is answerable on an empty database. If this fails, every
# runner-credential Secret write is Forbidden and every workflow run will
# hang on FailedMount, while observation keeps working and the stack
# otherwise looks healthy.
rc=0
kubectl auth can-i create workflows.argoproj.io -n "$ARGO_NAMESPACE" \
    --as="system:serviceaccount:$NAMESPACE:qa-platform-gears" > "$WORKDIR/cani.txt" 2>&1 || rc=$?
if [ "$rc" -ne 0 ] || ! grep -qx 'yes' "$WORKDIR/cani.txt"; then
    echo "FAIL: the cross-namespace RoleBinding is not effective for workflows: $(cat "$WORKDIR/cani.txt" 2>/dev/null)" >&2
    exit 1
fi
echo "PASS: qa-platform-gears can create workflows in namespace $ARGO_NAMESPACE"

# Both verbs, checked separately: qa-environments' secret_writer.rs does a
# server-side-apply `patch` on an existing Secret and a `create` on a new
# one, so a Role granting only one of the two fails on exactly half the
# environments (the new ones, or the updated ones) with no pattern an
# operator would spot.
for verb in create patch; do
    rc=0
    kubectl auth can-i "$verb" secrets -n "$ARGO_NAMESPACE" \
        --as="system:serviceaccount:$NAMESPACE:qa-platform-gears" > "$WORKDIR/cani-secrets-$verb.txt" 2>&1 || rc=$?
    if [ "$rc" -ne 0 ] || ! grep -qx 'yes' "$WORKDIR/cani-secrets-$verb.txt"; then
        echo "FAIL: qa-platform-gears CANNOT '$verb' secrets in namespace $ARGO_NAMESPACE: $(cat "$WORKDIR/cani-secrets-$verb.txt" 2>/dev/null). qa-environments writes each of an environment's credentials there as a Secret (decision D4, runner_secret_writer::ensure_runner_secret); without this grant every workflow run hangs on FailedMount while the rest of the stack looks healthy. rbac-argo.yaml's Role is what grants it." >&2
        exit 1
    fi
done
echo "PASS: qa-platform-gears can create AND patch secrets in namespace $ARGO_NAMESPACE (the D4 runner-credential write, checked statically -- works on a cold stack, unlike check 14's log half)"

step "k8s 4/4: /realms serves Keycloak's discovery document through the UI nginx"
# THIS CHECK ALSO COVERS "Keycloak's own advertised issuer": a deployment that
# exposed Keycloak's own https listener at a separate origin could probe it
# directly, but this chart never
# exposes Keycloak outside the cluster at all (keycloak-service.yaml is
# ClusterIP-only) -- the UI's nginx (ui-extraconf-configmap.yaml) is the
# ONLY path to it, proxying /realms and /resources to the Service (/admin is
# deliberately NOT proxied -- see ui-extraconf-configmap.yaml and check 16).
# With a single public origin, "Keycloak's advertised issuer" and "the
# discovery document served through the UI nginx" are the literal same HTTP
# request, so porting the former separately would just be this same curl
# call twice under two names.
rc=0
curl -sS --cacert "$CA_CRT" -o "$WORKDIR/disc.json" -w '%{http_code}' \
    "$PUBLIC_ORIGIN/realms/qa-platform/.well-known/openid-configuration" > "$WORKDIR/disc.code" 2>"$WORKDIR/disc.err" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: curl exited $rc: $(cat "$WORKDIR/disc.err" 2>/dev/null)" >&2
    exit 1
fi
code="$(cat "$WORKDIR/disc.code")"
if [ "$code" != "200" ]; then
    echo "FAIL: HTTP $code fetching $PUBLIC_ORIGIN/realms/qa-platform/.well-known/openid-configuration -- if this is a TLS/connection failure rather than a real HTTP code, the ui leaf's SANs or the nginx /realms proxy (ui-extraconf-configmap.yaml) are the likely cause, not Keycloak itself (step 0 already confirmed it Available)." >&2
    exit 1
fi
iss="$(jq -r .issuer "$WORKDIR/disc.json" 2>/dev/null || true)"
if [ "$iss" != "$PUBLIC_ORIGIN/realms/qa-platform" ]; then
    echo "FAIL: issuer is '$iss', expected '$PUBLIC_ORIGIN/realms/qa-platform' -- KC_HOSTNAME (values.yaml's publicOrigin) does not agree with PUBLIC_ORIGIN this script was invoked with." >&2
    exit 1
fi
echo "PASS: discovery served at one origin through the UI nginx, issuer '$iss'"

echo
echo "VERIFY-K8S: every check above passed individually"
