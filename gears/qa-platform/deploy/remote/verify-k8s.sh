#!/usr/bin/env bash
# Verification suite for the in-cluster (Helm/k3s) qa-platform deploy -- the
# k8s counterpart to sync.sh's own VERIFY / VERIFY-ARGO heredocs, which this
# file ports rather than reinvents. Same idea as that script: "the deploy
# printed no errors" is not "the deploy is correct", and every check below
# exists because a specific failure once looked like success.
#
# RUNS ON THE NODE ITSELF, not over ssh. Unlike sync.sh/deploy-k8s.sh (which
# drive a REMOTE host from a local machine via lib.sh's remote_sh/ssh_ro),
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
# STOPS AT THE FIRST FAILURE, deliberately unlike sync.sh's own VERIFY block
# (which accumulates `fail=1` across every check in the block and only exits
# at the very end, so one failure does not hide the rest). Here every check
# below `exit 1`s the moment it fails, immediately after printing a `FAIL:`
# line naming what is wrong and what to do about it -- the shape Task 12's
# brief specifies for the four new checks, applied uniformly. The trade-off
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
# the way sync.sh reads it out of the gears container: that container may
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

# ================================================== ported from sync.sh ====
# Same probes' LOGIC as sync.sh's VERIFY / VERIFY-ARGO heredocs
# (deploy/remote/sync.sh, from its "step Verification" on), `docker compose
# exec`/`docker compose logs` swapped for `kubectl exec`/`kubectl logs`. See
# this task's own report (task-12-report.md) for the full table of which of
# sync.sh's ~24 individual checks were ported here, which were dropped, and
# why -- the short version: checks that were regression tests for specific
# historical compose-only bugs (the qa-catalog credstore-ref string, the
# repos-table "Sync failed" string), and the workflow-Secret-exists /
# imported-realm-service-account-client checks that belong to deploy/argo/
# provision-workflow-secret.sh's own concern rather than this chart's, are
# not ported. Two are MERGED rather than dropped: the compose "Keycloak's own
# advertised issuer" check and this chart's own new "discovery served through
# the UI nginx" check become the literal same HTTP request once there is only
# one public origin (no separate PUBLIC_ISSUER_ORIGIN) -- see the "k8s 4/4"
# check near the end of this file. VERIFY-ARGO's "qa-environments reaches the
# same cluster" pair (its check 2b) is RESTORED below (check 14), not
# dropped, after review round 1 -- see that check's own comment for why the
# CONFIG half stays dropped (Config::infer() is this chart's intended path,
# not compose's broken fallback) while the RUNTIME half (does the Secret
# write actually succeed) remains exactly as meaningful in-cluster.

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
# RESTORED in review round 1 (sync.sh's own check 4). "k8s 4/4" below tests
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
# string; strip the backslashes before matching, same as sync.sh's own
# check 4 (verified there: `case` on the raw line does not match, on the
# stripped line it does).
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
# RESTORED in review round 1 (sync.sh's own check 7, "UI over HTTPS"). "k8s
# 4/4" below only exercises the Keycloak-proxy location block
# (ui-extraconf-configmap.yaml's `^/(realms|resources)/` regex); the
# SPA's own `location /` is a DIFFERENT block, and nothing else in this file
# fetches it over the wire -- check 5 above only reads the bundle's files via
# `kubectl exec`, never through nginx. No retry budget, unchanged from
# sync.sh's own reasoning: nginx opens its listener at start and does not
# import a realm, so this either answers or it is broken.
#
# A 200 ALONE IS NOT EVIDENCE THE IN-CLUSTER UI SERVED IT, and that is the
# half added in the final review. The compose stack this deployment is
# CUTTING OVER FROM binds the same node's 80/443 and serves the same SPA, so
# during the cutover window "curl $PUBLIC_ORIGIN/ -> 200" is satisfied just as
# happily by the compose nginx -- and every FAIL further down would then be
# read against the wrong process. deploy-k8s.sh now preflights that 80/443 are
# free before installing (see its port preflight), and this check closes the
# other end: it compares the LEAF CERTIFICATE the origin actually presents
# against the ui.crt inside the cluster's own qa-platform-tls Secret. Those
# two agree only if the pod terminating this TLS connection is the one this
# chart's certs Job minted a leaf for. The compose stack has its own,
# independently generated CA and leaf under deploy/compose/.generated, so its
# fingerprint cannot match.
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
    echo "FAIL: $ORIGIN_HOSTPORT is served by something OTHER than this chart's UI pod. The leaf it presents is [$served_fp]; secret/qa-platform-tls's ui.crt is [$want_fp]. The overwhelmingly likely cause during the cutover is that the docker-compose stack still holds hostPort 80/443 on this node -- 'docker compose -f gears/qa-platform/deploy/compose/docker-compose.yml ps' on the node, then stop it (NEVER with '-v') and 'kubectl -n $NAMESPACE delete pod -l app.kubernetes.io/component=ui' so the UI pod can bind the port. Every check below that goes through \$PUBLIC_ORIGIN would otherwise be measuring the wrong process." >&2
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
# GET /qa/v1/platforms is refused by the gears themselves, so a 401 proves
# the request travelled the WHOLE path: TLS termination at nginx, the
# `location /qa/v1/` block, resolver lookup of qa-platform-gears, the
# Service, the pod, and the gears' own auth layer answering. Any 5xx --
# 502 in particular -- means the request never reached the gears at all.
# A 200 would be just as wrong as a 502 (it would mean the endpoint is
# unauthenticated), so both are failed explicitly rather than "not 502".
rc=0
curl -s -o "$WORKDIR/api.body" -w '%{http_code}' --max-time 15 --cacert "$CA_CRT" \
    "$PUBLIC_ORIGIN/qa/v1/platforms" > "$WORKDIR/api.code" 2>"$WORKDIR/api.err" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: curl exited $rc fetching $PUBLIC_ORIGIN/qa/v1/platforms: $(cat "$WORKDIR/api.err" 2>/dev/null)" >&2
    exit 1
fi
api_code="$(cat "$WORKDIR/api.code")"
case "$api_code" in
    401)
        echo "PASS: $PUBLIC_ORIGIN/qa/v1/platforms answered 401 -- the request reached the gears through nginx's /qa/v1 proxy, so the resolver (clusterDns=$(kubectl -n "$NAMESPACE" get deploy qa-platform-ui -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="NGINX_RESOLVER")].value}' 2>/dev/null || echo '<unreadable>')) resolves qa-platform-gears" ;;
    502|503|504)
        echo "FAIL: $PUBLIC_ORIGIN/qa/v1/platforms answered $api_code -- nginx could not reach the gears. The prime suspect is values.yaml's clusterDns: it becomes nginx's \`resolver\` (NGINX_RESOLVER on the ui Deployment), and \`location /qa/v1/\` uses a VARIABLE proxy_pass, which resolves the upstream name at REQUEST time through that resolver. Read the real address with 'kubectl -n kube-system get svc kube-dns -o jsonpath={.spec.clusterIP}' and compare it with the NGINX_RESOLVER env on deploy/qa-platform-ui. 'kubectl -n $NAMESPACE logs deploy/qa-platform-ui' shows the resolver error verbatim." >&2
        exit 1 ;;
    200)
        echo "FAIL: $PUBLIC_ORIGIN/qa/v1/platforms answered 200 WITHOUT a bearer token. The proxy path works, but the endpoint is answering unauthenticated -- that is a worse finding than the one this check was written for, not a pass." >&2
        exit 1 ;;
    *)
        echo "FAIL: $PUBLIC_ORIGIN/qa/v1/platforms answered '$api_code', expected 401. A 404 means nginx matched a different location block than \`location /qa/v1/\` (see default.conf.template's warning about the trailing-extension regex); anything else, read 'kubectl -n $NAMESPACE logs deploy/qa-platform-ui'. Body: $(head -c 300 "$WORKDIR/api.body" 2>/dev/null)" >&2
        exit 1 ;;
esac

step "7: the gears' rendered config selects the persistent credstore backend"
# THE THREE-PLACE SWITCH, same as sync.sh's own check 8: the Dockerfile's ARG
# CARGO_FEATURES default, deploy/cargo-features.argo, and
# credstore.config.vendor in qa-platform-stack.yaml must all agree, or
# secrets silently live in a HashMap and die on every pod restart -- which is
# how this got noticed on the compose stack (an SSH key lost four times in
# one day). Read as a rendered-config claim AND a database fact (this check
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

step "9: the deployed binary was built WITH the platform-observation feature"
# Same absence-and-presence idiom as check 2, including its review-round-3
# fix: both reads go through grep_count(), never a bare
# `kubectl exec ... || true` feeding `${n:-0}` -- see check 2's own comment
# and grep_count()'s for why the "without" half specifically is the one an
# unread value could silently fake into a false PASS. Read on the BINARY,
# not the rendered app config: qa-environments' serve_with_services has two
# #[cfg(feature = "platform-observation")]-gated arms, and each arm's log
# string is compiled in whenever that arm exists in the source -- regardless
# of which branch runs at runtime -- so this tests only what CARGO_FEATURES
# selected at build time.
if ! grep_count deploy/qa-platform-gears 'built without the .platform-observation. cargo feature' /usr/local/bin/cf-gears-example-server "gears binary: 'built without platform-observation' string count"; then
    exit 1
fi
n_without_obs="$GREP_COUNT_VAL"
if ! grep_count deploy/qa-platform-gears 'observation ticker started' /usr/local/bin/cf-gears-example-server "gears binary: 'observation ticker started' string count"; then
    exit 1
fi
n_with_obs="$GREP_COUNT_VAL"
if [ "$n_with_obs" -ge 1 ] && [ "$n_without_obs" -eq 0 ]; then
    echo "PASS: the deployed binary was built WITH the platform-observation feature (with=$n_with_obs without=$n_without_obs)"
else
    echo "FAIL: the deployed binary's platform-observation feature state is wrong (with=$n_with_obs without=$n_without_obs; wanted with>=1, without=0). Check BOTH qa-platform.Dockerfile's ARG CARGO_FEATURES default and deploy/cargo-features.argo." >&2
    exit 1
fi

step "10: platform observation actually ran against the real database"
# The DATABASE FACT half of check 9: proof it did not just get selected but
# actually ran -- a platform row with a non-null version_detected_at, which
# only the ticker or POST /qa/v1/platforms/{id}/refresh ever writes.
# COLD START, DELIBERATELY NOT A FAILURE: a brand-new deployment has no
# qa_platforms rows at all, so there is nothing to have observed yet -- a
# NOTE, not folded into a FAIL. Both queries must themselves succeed and
# return a plain count (psql_count's own job) BEFORE this cold-start logic
# ever runs -- fixed in review round 1: the previous `${n:-0}` form could not
# tell "zero rows, confirmed" apart from "the query failed and returned
# nothing", so a broken exec could silently read as a legitimate cold start.
if ! psql_count qa_environments "select count(*) from qa_platforms" "qa_platforms row count"; then
    exit 1
fi
n_platforms="$PSQL_COUNT_VAL"
if ! psql_count qa_environments "select count(*) from qa_platforms where version_detected_at is not null" "qa_platforms.version_detected_at observed-row count"; then
    exit 1
fi
n_observed="$PSQL_COUNT_VAL"
if [ "$n_platforms" -eq 0 ]; then
    echo "NOTE: qa_platforms has no rows yet -- a cold-start deployment with nothing registered cannot show a non-null version_detected_at. Not treated as a failure; create a platform and re-run this check."
elif [ "$n_observed" -ge 1 ]; then
    echo "PASS: $n_observed of $n_platforms platform row(s) carry a non-null version_detected_at -- observation actually ran against a real cluster"
else
    echo "FAIL: qa_platforms has $n_platforms row(s) but none has a non-null version_detected_at -- platform-observation is selected but no observation has ever completed. If a platform was JUST created, give the ticker's first tick a few seconds before treating this as real." >&2
    exit 1
fi

step "11: cluster health observation actually ran"
# Same cold-start-is-a-NOTE treatment as check 10: the ticker fires every
# five minutes, so a freshly deployed stack can legitimately show zero rows
# with a cluster_status yet. Unlike the PREVIOUS version of this check
# (review round 1's other Critical-adjacent finding), there is now a real
# FAIL path: `psql_count` itself fails loudly if the query cannot be
# verified, rather than this check having no way to fail at all.
if ! psql_count qa_environments "select count(*) from qa_platforms where cluster_status is not null" "qa_platforms.cluster_status observed-row count"; then
    exit 1
fi
n_cluster_observed="$PSQL_COUNT_VAL"
if [ "$n_cluster_observed" -eq 0 ]; then
    echo "NOTE: no qa_platforms row carries a non-null cluster_status yet -- the observation ticker runs every five minutes and may not have fired since this stack (or these platforms) came up. Not treated as a failure; re-run this check shortly."
else
    echo "PASS: $n_cluster_observed platform row(s) carry a non-null cluster_status -- cluster health observation actually ran against a real cluster"
fi

step "12: the cluster_status_message leak canary (D-CH-5)"
# cluster_status_message is populated from a classified kube::Error, never a
# formatted one -- this is a FAIL, not a NOTE, whenever it matches, cold
# start or not, because the whole point of a canary is that it should never
# fire. The offending value itself is never printed, only the count and the
# column name, so the canary cannot become a second leak. `psql_count`'s own
# failure path is what closes review round 1's Critical here specifically:
# "a security canary that goes green without looking is worse than no
# canary" -- an exec/query failure now FAILS this check rather than reading
# as a clean PASS.
if ! psql_count qa_environments "select count(*) from qa_platforms where cluster_status_message like '%BEGIN%' or cluster_status_message like '%PRIVATE KEY%'" "qa_platforms.cluster_status_message leak-canary count"; then
    exit 1
fi
n_leaked="$PSQL_COUNT_VAL"
if [ "$n_leaked" -eq 0 ]; then
    echo "PASS: no qa_platforms.cluster_status_message row contains BEGIN or PRIVATE KEY (query verified to have run)"
else
    echo "FAIL: $n_leaked qa_platforms.cluster_status_message row(s) contain BEGIN or PRIVATE KEY -- a secret is leaking from a kubeconfig read into the database (D-CH-5). Value withheld deliberately; inspect cluster_status_message directly against the deployed database." >&2
    exit 1
fi

step "13: the runner image is in containerd, not merely in Docker"
# The one that produces ImagePullBackOff on every workflow run, minutes in,
# naming a registry the image was never in: docker build alone does not make
# an image visible to k3s's containerd. Read runner_image out of the
# ConfigMap gears-argo-configmaps.yaml renders (the chart's equivalent of
# sync.sh's ./.generated/qa-runs-argo.yaml), not out of values.yaml directly,
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

step "14: qa-environments' runner-kubeconfig Secret write into \$ARGO_NAMESPACE (D4) actually succeeds"
# RESTORED in review round 1 -- this is the check that would have caught
# that round's other Critical: rbac-argo.yaml withheld `secrets` entirely,
# which is right for qa-runs' executor (ADR-0001) but wrong for
# qa-environments, a DIFFERENT gear in the SAME pod that legitimately writes
# a Secret by design (decision D4,
# qa-environments/src/infra/observer/secret_writer.rs:174-176 --
# `Api::<Secret>::namespaced(client, argo_namespace)` then a server-side
# apply `patch`). Under compose the gears held a cluster-admin kubeconfig,
# so a missing grant never surfaced; with this scoped ServiceAccount the
# write is Forbidden on every cycle, and the documented symptom is every
# workflow run hanging on FailedMount while observation (checks 10/11 above)
# keeps working and the stack otherwise looks healthy -- nothing in checks
# 2/3/13/15 touches this code path at all, since qa-environments is not
# qa-runs.
#
# THE CONFIG HALF OF sync.sh's original check (VERIFY-ARGO 2b) STAYS
# DROPPED, deliberately, not merely forgotten: that half asserted
# qa-environments must have a `kubeconfig_path`, because under compose an
# absent one meant a broken fallback to `Config::infer()` (finding I2). In
# THIS chart, gears-argo-configmaps.yaml's own header states the opposite:
# an absent kubeconfig_path making both Argo clients fall back to
# `Config::infer()` IS the intended in-cluster credential path. Porting that
# config-half check unchanged would assert the exact opposite of what this
# chart's own design says is correct. The RUNTIME half below -- does
# inference and the Secret write actually SUCCEED -- carries no such
# assumption and remains exactly as meaningful here as it was under compose.
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
if ! grep_count deploy/qa-platform-gears 'failed to apply this platform.s runner kubeconfig Secret' /usr/local/bin/cf-gears-example-server "gears binary: 'failed to apply...' D4 string count"; then
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
# (qa-environments/.../platforms.rs:969-1008) runs PER REGISTERED PLATFORM: on
# create, on update, and on every self-heal cycle. The cutover starts against
# an EMPTY database, so there are zero platforms, the writer never runs, and
# neither failure string is ever logged -- at which point this check's PASS
# branch ("neither string appears") fires having exercised nothing at all and
# reports the D4 writer as working. Checks 10 and 11 above already treat this
# exact precondition as a NOTE rather than a pass; this one now does too.
#
# The STATIC half of the same concern -- can the gears' ServiceAccount even
# create/patch a Secret in $ARGO_NAMESPACE? -- does NOT need a platform to
# exist, and is checked unconditionally in "k8s 3/4" below alongside the
# workflows grant. That is what actually covers a cold stack; this check is
# the runtime confirmation once there is something to observe.
if ! psql_count qa_environments "select count(*) from qa_platforms" "qa_platforms row count (D4 precondition)"; then
    exit 1
fi
n_d4_platforms="$PSQL_COUNT_VAL"
if [ "$n_d4_platforms" -eq 0 ]; then
    echo "NOTE: qa_platforms has no rows -- materialise_runner_secret runs per registered platform, so on a cold-start deployment the D4 writer has never run and the log CANNOT contain either failure string. Reporting PASS here would be reporting on nothing. The RBAC that this writer needs is checked statically in 'k8s 3/4' below, which works on an empty stack; re-run this check once a platform exists."
else
    rc=0
    kubectl -n "$NAMESPACE" logs deploy/qa-platform-gears > "$WORKDIR/gears-d4.log" 2>"$WORKDIR/gears-d4.err" || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "FAIL: could not read the gears' log (exit $rc): $(cat "$WORKDIR/gears-d4.err" 2>/dev/null). This check cannot tell 'D4 is working' from 'the log is unreadable' -- an unreadable log is a FAIL here, not the silent PASS it would otherwise become." >&2
        exit 1
    fi
    infer_line="$(grep 'failed to infer a Kubernetes config for the Argo cluster' "$WORKDIR/gears-d4.log" | tail -n1 || true)"
    secret_line="$(grep 'failed to apply this platform.s runner kubeconfig Secret' "$WORKDIR/gears-d4.log" | tail -n1 || true)"
    if [ -n "$infer_line" ]; then
        echo "FAIL: the gears' log contains '$infer_line' -- qa-environments could not construct ANY Kubernetes client for the Argo cluster (the in-cluster ServiceAccount token or the API server itself is unreachable), independent of RBAC." >&2
        exit 1
    elif [ -n "$secret_line" ]; then
        echo "FAIL: the gears' log contains '$secret_line' -- qa-environments has a client but the Secret write itself is failing. Check the qa-platform-gears ServiceAccount's RBAC for 'secrets' create/patch in namespace $ARGO_NAMESPACE (rbac-argo.yaml), a missing namespace, or a hand-made Secret this writer does not own (see secret_writer.rs's own 409-conflict message)." >&2
        exit 1
    else
        echo "PASS: with $n_d4_platforms platform(s) registered, the gears' log (read successfully, $(wc -l < "$WORKDIR/gears-d4.log" | tr -d ' ') lines) carries neither the Config::infer() failure nor a failed runner-kubeconfig Secret write -- qa-environments' D4 writer is reaching $ARGO_NAMESPACE and succeeding"
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
# READ OUT OF THE qa-platform-realm ConfigMap, NOT OUT OF THE ADMIN API --
# rewritten in the final review together with the removal of the `/admin/`
# proxy (ui-extraconf-configmap.yaml). The previous form obtained a
# master-realm admin token over the PUBLIC ORIGIN with the committed
# admin/admin credentials and called `/admin/realms/qa-platform/clients`,
# which meant the admin console and the entire admin REST API had to be
# published on the application's public origin for a verification check's
# convenience. It was the only consumer of that proxy.
#
# THE CONFIGMAP IS NOT A WEAKER SOURCE THAN THE ADMIN API HERE. It is the
# EXACT document Keycloak imports: deploy-k8s.sh renders it with
# render-realm.sh for this PUBLIC_ORIGIN and hands it to Helm with
# --set-file, the chart writes it into configmap/qa-platform-realm, and
# keycloak-deployment.yaml mounts that at /opt/keycloak/data/import for
# `--import-realm`. Reading the ConfigMap therefore checks the same pin the
# admin API would have -- "does the realm this cluster will import name this
# origin?" -- against the cluster's own copy rather than the file on disk
# that produced it, and unlike the admin API it is answerable BEFORE any
# login flow works and on a Keycloak whose H2 store has just been discarded.
#
# What it does NOT prove is that the import SUCCEEDED. That is already
# covered, twice: keycloak-deployment.yaml's readiness probe gates on
# /realms/qa-platform/.well-known/openid-configuration (which only answers
# after the import), and "k8s 4/4" below fetches that same document through
# nginx and compares its issuer. So the split is: those two prove the realm
# imported, this one proves the realm that imported carries the right
# redirect URI.
rc=0
kubectl -n "$NAMESPACE" get configmap qa-platform-realm \
    -o jsonpath='{.data.realm-qa-platform\.json}' > "$WORKDIR/realm.json" 2>"$WORKDIR/realm.err" || rc=$?
if [ "$rc" -ne 0 ] || [ ! -s "$WORKDIR/realm.json" ]; then
    echo "FAIL: could not read configmap/qa-platform-realm's realm-qa-platform.json key in namespace $NAMESPACE (exit $rc): $(cat "$WORKDIR/realm.err" 2>/dev/null). deploy-k8s.sh renders it with render-realm.sh and passes it to Helm with --set-file; an empty value means that step did not run or rendered nothing." >&2
    exit 1
fi
rc=0
uris="$(jq -r --arg c qa-platform-ui '.clients[] | select(.clientId==$c) | .redirectUris | join(" ")' "$WORKDIR/realm.json" 2>"$WORKDIR/realm.jq.err")" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "FAIL: jq could not read the qa-platform-ui client's redirectUris out of the realm ConfigMap (exit $rc): $(cat "$WORKDIR/realm.jq.err" 2>/dev/null). The ConfigMap's value is not the JSON this check expects." >&2
    exit 1
fi
if [ -z "$uris" ]; then
    echo "FAIL: the realm ConfigMap has no qa-platform-ui client, or that client has an empty redirectUris list. An absent client is not an absent problem: the SPA cannot complete a login at all. Check render-realm.sh's output." >&2
    exit 1
fi
case " $uris " in
    *" $PUBLIC_ORIGIN/* "*)
        echo "PASS: the realm this cluster imports lists $PUBLIC_ORIGIN/* as a qa-platform-ui redirect URI (all: [$uris])" ;;
    *)
        echo "FAIL: the realm this cluster imports does NOT list $PUBLIC_ORIGIN/* for qa-platform-ui -- it lists [$uris]. render-realm.sh was run against a different origin than this script was invoked with; re-run deploy-k8s.sh with the right --public-origin. Note the realm imports only on Keycloak's FIRST start (H2 is ephemeral by design, see keycloak-deployment.yaml), so after correcting the ConfigMap 'kubectl -n $NAMESPACE delete pod -l app.kubernetes.io/component=keycloak' is what makes the new realm take effect." >&2
        exit 1 ;;
esac

# ========================================================= new for k8s ====
# The four checks Task 12's brief specifies by name -- nothing here exists
# for the compose stack because nothing here has a compose analogue: Helm's
# no-op-on-unchanged-PodSpec behaviour, PersistentVolumeClaims, cross-
# namespace RBAC, and a single public origin proxying Keycloak through the
# UI's nginx are all k8s-shaped concerns.

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

step "k8s 3/4: the gears SA can submit Workflows AND write runner-kubeconfig Secrets into argo"
# rbac-argo.yaml's Role/RoleBinding live in $ARGO_NAMESPACE, granting
# system:serviceaccount:$NAMESPACE:qa-platform-gears create/get/list/watch/
# patch on workflows.argoproj.io and create/patch on secrets -- this is the
# one check that actually exercises the cross-namespace RoleBinding rather
# than just reading its manifest back.
#
# THE `secrets` VERBS ARE HERE, NOT ONLY IN CHECK 14, AND THAT IS THE POINT.
# Check 14 confirms the D4 writer at RUNTIME, from the gears' log -- which
# says nothing at all until a platform has been registered, so on the cold
# stack this cutover starts from it reports a NOTE and exercises nothing.
# `kubectl auth can-i` needs no platform, no workflow and no traffic: it asks
# the API server's authorizer directly, so it is the half of the D4 RBAC
# question that is answerable on an empty database. If this fails, every
# runner-kubeconfig Secret write is Forbidden and every workflow run will
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
# platforms (the new ones, or the updated ones) with no pattern an operator
# would spot.
for verb in create patch; do
    rc=0
    kubectl auth can-i "$verb" secrets -n "$ARGO_NAMESPACE" \
        --as="system:serviceaccount:$NAMESPACE:qa-platform-gears" > "$WORKDIR/cani-secrets-$verb.txt" 2>&1 || rc=$?
    if [ "$rc" -ne 0 ] || ! grep -qx 'yes' "$WORKDIR/cani-secrets-$verb.txt"; then
        echo "FAIL: qa-platform-gears CANNOT '$verb' secrets in namespace $ARGO_NAMESPACE: $(cat "$WORKDIR/cani-secrets-$verb.txt" 2>/dev/null). qa-environments writes each platform's runner kubeconfig there (decision D4, secret_writer.rs:174-176); without this grant every workflow run hangs on FailedMount while the rest of the stack looks healthy. rbac-argo.yaml's Role is what grants it." >&2
        exit 1
    fi
done
echo "PASS: qa-platform-gears can create AND patch secrets in namespace $ARGO_NAMESPACE (the D4 runner-kubeconfig write, checked statically -- works on a cold stack, unlike check 14's log half)"

step "k8s 4/4: /realms serves Keycloak's discovery document through the UI nginx"
# THIS CHECK ALSO STANDS IN FOR sync.sh's "Keycloak's own advertised issuer"
# check (its check 5): compose could hit Keycloak's OWN https listener
# directly at a separate PUBLIC_ISSUER_ORIGIN (port 8443); this chart never
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
