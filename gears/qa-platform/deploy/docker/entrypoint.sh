#!/usr/bin/env bash
# Entrypoint for the qa-platform gears image.
#
# gears/qa-platform/config/qa-platform-stack.yaml pins its `database.servers.pg_qa_platform`
# block to literal host/user/password values ("localhost"/"qa"/"qa") on
# purpose: `${VAR}` expansion is opt-in per config struct, and
# GlobalDatabaseConfig/DbConnConfig (libs/toolkit-db/src/config.rs) do not
# opt in, so a `${POSTGRES_HOST}`-style placeholder in that file would reach
# Postgres as a literal string, not an expanded one (see the comment at
# gears/qa-platform/config/qa-platform-stack.yaml:18-36). Inside compose the database answers
# to the `postgres` service, not `localhost`, so this script renders those
# three fields from POSTGRES_HOST/POSTGRES_USER/POSTGRES_PASSWORD into a
# *separate* writable file before the server starts, rewrites the command
# line to point at that rendered file, then execs it so it becomes PID 1 and
# receives signals directly.
#
# A FIFTH RENDER, AND IT IS AN INSERTION RATHER THAN A SUBSTITUTION: the
# TWO fragments, one mechanism. $QA_RUNS_ARGO_CONFIG selects the Argo EXECUTOR
# for qa-runs; $QA_ENVIRONMENTS_ARGO_CONFIG gives qa-environments the path to
# the same Argo cluster so decision D4's runner-credential `Secret`s can be
# written (without it that write falls back to `Config::infer()`, which finds
# nothing inside this container, and fails on every cycle while observation
# keeps working -- the 2026-08-28 review's finding I2). They are validated and
# inserted identically, at two anchors, and setting the first without the
# second is refused below.
#
# `qa-runs` Argo executor block. $QA_RUNS_ARGO_CONFIG names a YAML FRAGMENT
# file (mounted into the container) whose lines are inserted after the
# `# QA_RUNS_ARGO_ANCHOR` comment in the config template. Unset -- the default,
# and what the local stack runs -- inserts nothing, so the rendered config is
# byte-identical to what it was before this existed and qa-runs stays on the
# mock executor.
#
# WHY A FRAGMENT FILE AND NOT A DOZEN ENVIRONMENT VARIABLES. The block has ten
# fields including a nested `bundle_auth` mapping; rendering that from
# `QA_RUNS_ARGO_*` variables would put a YAML schema in a shell script and make
# every added knob a change to this file. A fragment is reviewable as the thing
# it becomes, and this script's whole job with it is to check the ONE property a
# fragment can get wrong invisibly: its indentation. YAML has no error for a
# mapping inserted at the wrong depth -- it silently becomes a sibling of the
# gear it was meant to configure, and `deny_unknown_fields` then rejects the
# whole `qa-runs` section with a message naming a field nobody wrote.
#
# CONFIG_FILE below is treated strictly as a read-only template and is never
# written to. Earlier revisions of this script rendered with `sed -i`,
# in place, on CONFIG_FILE itself: the first start rewrote its anchors and
# every subsequent start of that same container (or, under Kubernetes, a
# read-only ConfigMap-mounted ancestor directory) found zero anchors left to
# substitute and refused to start -- a config file that was actually fine
# got blamed for drift written by this script's own previous run. Rendering
# to RENDERED_CONFIG_FILE, a path outside CONFIG_FILE's directory, instead of
# in place fixes both: the template's anchor count is identical on every
# start, and nothing is ever written under wherever CONFIG_FILE happens to
# live, so this also works when that directory is mounted read-only.
#
# It fails loudly instead of silently no-op'ing: a substitution that quietly
# does nothing would leave the server pointed at "localhost" inside a
# container where nothing listens on that host, and the resulting failure
# would surface many layers away (a generic connection-refused deep in
# toolkit-db) with no clue that the real cause was here.
#
# A FOURTH FIELD IS RENDERED HERE FOR THE SAME REASON, AND IT IS NOT A
# DATABASE FIELD: `oidc-authn-plugin.config.jwt.trusted_issuers[0].
# issuer_pattern`. That value has to byte-equal the `iss` claim Keycloak puts
# in its tokens, which is the URL a BROWSER reaches Keycloak at -- so it is
# `http://localhost:8180/...` on a developer's machine and
# `http://<server>:8180/...` on a remote host. It sits in the same
# image-baked, non-`${VAR}`-expanding config file as the database block (the
# `oidc-authn-plugin` config structs do not opt into placeholder expansion
# either), so the only place it can be parameterised without editing the
# committed file is here. PUBLIC_ISSUER_ORIGIN is the knob (PUBLIC_HOST feeds
# its default); docker-compose.yml gives `KC_HOSTNAME` and the UI's
# `VITE_OIDC_ISSUER` build arg the same value, which is what keeps the ends of
# that chain in agreement.
#
# AN ORIGIN AND NOT A HOST, since PUBLIC_HOST alone used to be enough. Serving
# the stack over https moves the SCHEME and the PORT as well as the host --
# Keycloak's https listener is 8443, not 8180 -- and neither is derivable from a
# host name. An origin-shaped variable carries all three as one value, so the
# things that must byte-equal each other are configured from one string.
# PUBLIC_HOST is still read, as that variable's default
# (`http://${PUBLIC_HOST}:8180`), so a stack that sets only PUBLIC_HOST behaves
# exactly as it did before this existed.
set -euo pipefail

CONFIG_FILE="${GEARS_CONFIG_FILE:-/etc/cf-gears/qa-platform-stack.yaml}"
# Derived from CONFIG_FILE rather than its own env var, per the existing
# GEARS_CONFIG_FILE convention: the only path that ever needs to be
# configured from outside is the read-only template (e.g. to point at a
# ConfigMap mount); the rendered file is this script's own private output
# and nothing else needs to name it independently. Lives under
# /var/lib/cf-gears (WORKDIR, owned by the runtime user -- see
# qa-platform.Dockerfile) rather than next to CONFIG_FILE, so writing it never
# requires CONFIG_FILE's own directory to be writable.
RENDERED_CONFIG_FILE="/var/lib/cf-gears/.rendered-$(basename "$CONFIG_FILE")"

: "${POSTGRES_HOST:?entrypoint: POSTGRES_HOST must be set}"
: "${POSTGRES_USER:?entrypoint: POSTGRES_USER must be set}"
: "${POSTGRES_PASSWORD:?entrypoint: POSTGRES_PASSWORD must be set}"

# Defaulted, not required, unlike the three above: an unset PUBLIC_HOST must
# reproduce today's behaviour exactly, and today's behaviour is the localhost
# chain this stack has always run. The three POSTGRES_* variables get `:?`
# instead because there is no correct default for them -- "localhost" is the
# template's literal, and it is wrong inside every container.
PUBLIC_HOST="${PUBLIC_HOST:-localhost}"
# Same defaulting chain docker-compose.yml uses for KC_HOSTNAME and
# VITE_OIDC_ISSUER, spelled the same way on purpose: unset means the localhost
# http chain this stack has always run, and an https deploy sets this one
# variable (docker-compose.https.yml does).
PUBLIC_ISSUER_ORIGIN="${PUBLIC_ISSUER_ORIGIN:-http://${PUBLIC_HOST}:8180}"

# This value is interpolated into a REGEX (see `issuer_pattern` below), so an
# unvalidated one is not merely a typo risk: `.*` would widen the pattern to
# trust any issuer, which is an authentication bypass, and this is the one
# substitution in this script whose output is matched rather than compared.
# Restrict it to `scheme://host[:port]` over what a DNS hostname or an IPv4
# literal can contain -- which also happens to exclude every regex
# metacharacter and every character `escape_sed_replacement` below exists to
# defend against.
#
# ONLY THE FINAL ORIGIN IS VALIDATED, not PUBLIC_HOST separately: a bad
# PUBLIC_HOST produces a bad default origin and fails right here, and validating
# a variable that PUBLIC_ISSUER_ORIGIN has overridden would refuse a start for a
# value nothing reads.
#
# NO TRAILING SLASH AND NO PATH, because the pattern below appends
# `/realms/qa-platform`: `https://h:8443/` would render `...8443//realms/...`,
# which matches no token's `iss` and 401s everything. Rejected here rather than
# trimmed, so the operator's own `.env` is what gets fixed.
#
# IPv6 is rejected rather than half-supported: a bracketed literal
# (`http://[::1]:8180/...`) needs the brackets in the URL and needs them
# escaped in the regex, and neither this script nor the compose file's
# `${PUBLIC_HOST}:8180:8080` port strings are written for that shape. Better a
# refusal here than a pattern that silently matches nothing.
if [[ ! "$PUBLIC_ISSUER_ORIGIN" =~ ^https?://[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?$ ]]; then
    echo "entrypoint: PUBLIC_ISSUER_ORIGIN='$PUBLIC_ISSUER_ORIGIN' is not a plain 'http(s)://host[:port]' origin (no trailing slash, no path; host must be a DNS name or IPv4 literal) -- refusing to start rather than interpolate it into the issuer_pattern regex. It defaults to http://\$PUBLIC_HOST:8180, so an unset value means PUBLIC_HOST='$PUBLIC_HOST' is the thing to fix." >&2
    exit 1
fi

if [[ ! -f "$CONFIG_FILE" ]]; then
    echo "entrypoint: config file '$CONFIG_FILE' not found -- refusing to start" >&2
    exit 1
fi

# Each anchor is expected to appear exactly once, inside the
# database.servers.pg_qa_platform block. If it appears zero times (the
# config changed shape) or more than once (a substitution target became
# ambiguous), that is a config drift this script cannot safely paper over.
require_anchor() {
    local pattern="$1"
    local label="$2"
    local count
    # `grep -c` exits 1 (not 0) when it finds zero matches, even though it
    # still prints "0". Under `set -e`, a failing command substitution
    # inside a plain assignment aborts the script right there -- so without
    # `|| true` this function would exit silently on the zero-match case,
    # never reaching the check below that produces the diagnostic message.
    # Confirmed with `bash -x`: the trace stopped dead after `count=0`.
    # `grep -c` exits 0 whenever it finds at least one match (including
    # more than one), so the "found more than once" case was never at risk.
    count="$(grep -c -E "$pattern" "$CONFIG_FILE" || true)"
    # If `grep` ever errored outright (bad regex, I/O error) rather than
    # merely matching nothing, `|| true` above still leaves `count` empty,
    # not "0" -- grep prints nothing to stdout on that path. `[[ "" -ne 1 ]]`
    # is not a silent no-op: it is a bash *error* ("integer expression
    # expected"), but that error fires inside an `if` condition, and `if`
    # conditions are exempt from `set -e`, so the function would fall
    # through silently past the diagnostic below instead of stopping the
    # script. Not reachable today -- the three patterns above are fixed
    # literals -- but this function's whole job is to fail loudly, so it
    # should not itself have a silent-fallthrough path. Reject anything
    # that is not a plain non-negative integer before the numeric compare.
    if [[ ! "$count" =~ ^[0-9]+$ ]]; then
        echo "entrypoint: could not count '$label' lines in $CONFIG_FILE (grep failed unexpectedly) -- refusing to start" >&2
        exit 1
    fi
    if [[ "$count" -ne 1 ]]; then
        echo "entrypoint: expected exactly one '$label' line in $CONFIG_FILE, found $count -- refusing to start rather than risk an unrendered or ambiguous database config" >&2
        exit 1
    fi
}

require_anchor '^      host: "localhost"$' 'host: "localhost"'
require_anchor '^      user: "qa"$' 'user: "qa"'
require_anchor '^      password: "qa"$' 'password: "qa"'
# The fourth anchor, and the one whose "found 0" case is the most dangerous:
# an unrendered issuer_pattern does not fail to start, it starts and then 401s
# every token the browser presents, with the two URLs differing only in their
# host. `require_anchor` turns that into a refusal at boot.
require_anchor '^          - issuer_pattern: "http://localhost:8180/realms/qa-platform"$' \
    'issuer_pattern: "http://localhost:8180/realms/qa-platform"'

# The fifth anchor. Required unconditionally -- even when no fragment is
# mounted -- because its absence means the config template changed shape, and a
# template that has lost the anchor is one that will silently ignore
# $QA_RUNS_ARGO_CONFIG on the next deploy that sets it.
require_anchor '^      # QA_RUNS_ARGO_ANCHOR$' '# QA_RUNS_ARGO_ANCHOR'

# The sixth anchor, `gears.qa-environments.config`'s, required for exactly the
# same reason as the fifth: a template that has lost it will silently ignore
# $QA_ENVIRONMENTS_ARGO_CONFIG on the next deploy that sets it, and the symptom
# -- D4 Secret writes failing forever while observation looks healthy -- names
# nothing.
require_anchor '^      # QA_ENVIRONMENTS_ARGO_ANCHOR$' '# QA_ENVIRONMENTS_ARGO_ANCHOR'

# The fragment, validated before it is inserted. Every check here corresponds
# to a failure that is otherwise reported many layers away:
#
#   missing file          -> sed's `r` command SILENTLY inserts nothing, so the
#                            stack boots on the mock while an operator believes
#                            it is running real tests. This is the whole reason
#                            this block exists.
#   wrong indentation     -> see the header. Six spaces is the depth of
#                            `dispatcher_enabled:` inside `gears.qa-runs.config`.
#   no `executor:` line   -> a fragment that configures `argo:` without
#                            selecting it leaves the mock in place.
# Shared by both fragments, because "validated identically" is a property that
# only survives if there is one implementation of it. $1 the variable name (for
# the message), $2 the path, $3 the gear's config path (for the indentation
# message), $4 a regex the fragment must contain, $5 what that regex means.
validate_fragment() {
    local var="$1" path="$2" section="$3" required_re="$4" required_why="$5"
    local bad_indent
    if [[ ! -f "$path" ]]; then
        echo "entrypoint: $var='$path' is not a file -- refusing to start. sed's \`r\` command inserts NOTHING when its file is missing and reports no error, so the container would boot with $section unconfigured and the failure would surface many layers away." >&2
        exit 1
    fi
    if [[ ! -s "$path" ]]; then
        echo "entrypoint: $var='$path' is empty -- refusing to start, for the same reason a missing file is refused." >&2
        exit 1
    fi
    # Tabs are rejected outright: YAML forbids them as indentation, and the
    # parser's error names a line number in a file the operator did not write.
    if grep -qP '^\t| \t' "$path" 2>/dev/null; then
        echo "entrypoint: '$path' contains a tab in its indentation. YAML forbids tabs for indentation -- refusing to insert it." >&2
        exit 1
    fi
    # Every non-blank line must start with exactly six spaces followed by a
    # non-space (a key or a comment), or with more than six (a nested value).
    # `grep -vE` finds the lines that do NOT.
    bad_indent="$(grep -nvE '^( {6}[^ ]| {7,}[^ ]|[[:space:]]*$)' "$path" || true)"
    if [[ -n "$bad_indent" ]]; then
        echo "entrypoint: '$path' has lines that are not indented for insertion inside '$section' (six spaces for a top-level key of that mapping). Offending lines:" >&2
        printf '%s\n' "$bad_indent" >&2
        exit 1
    fi
    if ! grep -qE "$required_re" "$path"; then
        echo "entrypoint: '$path' has no line matching '$required_re'. $required_why -- refusing to start." >&2
        exit 1
    fi
}

QA_RUNS_ARGO_CONFIG="${QA_RUNS_ARGO_CONFIG:-}"
QA_ENVIRONMENTS_ARGO_CONFIG="${QA_ENVIRONMENTS_ARGO_CONFIG:-}"

# THE TWO ARE COUPLED, AND THE COUPLING IS REFUSED RATHER THAN GUESSED.
# An --argo deployment needs both: qa-runs to submit the Workflow, and
# qa-environments to have written the credential Secrets that Workflow's pod
# mounts. A stack with only the first boots, runs real tests, and every one of
# them hangs on FailedMount until an operator provisions the Secret by hand --
# with nothing in any log naming the missing config. The most likely way to
# arrive here is a stale docker-compose.argo.yml from before 2026-08-28, so the
# message says exactly that.
if [[ -n "$QA_RUNS_ARGO_CONFIG" && -z "$QA_ENVIRONMENTS_ARGO_CONFIG" ]]; then
    echo "entrypoint: QA_RUNS_ARGO_CONFIG is set but QA_ENVIRONMENTS_ARGO_CONFIG is not. An Argo deployment needs BOTH -- qa-runs submits the Workflow, and qa-environments writes the runner credential Secrets that Workflow's pod mounts (decision D4). With only the first, every run hangs on FailedMount and nothing says why. Under the Helm chart both come from gears-argo-configmaps.yaml, which always renders the pair -- if only one is present the ConfigMap or its volumeMounts have been edited apart. Refusing to start." >&2
    exit 1
fi

# The fragments, validated before they are inserted. Every check corresponds to
# a failure that is otherwise reported many layers away:
#
#   missing file          -> sed's `r` command SILENTLY inserts nothing, so the
#                            stack boots on the mock while an operator believes
#                            it is running real tests. This is the whole reason
#                            this block exists.
#   wrong indentation     -> see the header. Six spaces is the depth of
#                            `dispatcher_enabled:` inside `gears.qa-runs.config`
#                            and of `max_variables:` inside
#                            `gears.qa-environments.config`.
#   no `executor:` line   -> a fragment that configures `argo:` without
#                            selecting it leaves the mock in place.
#   no `kubeconfig_path:` -> a qa-environments fragment without it leaves the
#                            D4 writer on `Config::infer()`, which is the exact
#                            state this fragment exists to fix.
if [[ -n "$QA_RUNS_ARGO_CONFIG" ]]; then
    validate_fragment QA_RUNS_ARGO_CONFIG "$QA_RUNS_ARGO_CONFIG" 'gears.qa-runs.config' \
        '^      executor:[[:space:]]*argo[[:space:]]*$' \
        "A fragment that configures the argo block without selecting it leaves the MOCK executor wired, which reports one fabricated passing test per run while the deployment believes it is running real tests"
    echo "entrypoint: will insert $(grep -c '' "$QA_RUNS_ARGO_CONFIG") line(s) from '$QA_RUNS_ARGO_CONFIG' after the QA_RUNS_ARGO_ANCHOR line"
fi

# WHETHER kubeconfig_path IS REQUIRED DEPENDS ON WHERE THIS CONTAINER RUNS,
# and this is the one fact that decides it. qa-environments' D4 Secret writer
# (`argo_client` in qa-environments/src/infra/observer/secret_writer.rs) treats an
# absent/empty kubeconfig_path as "use Config::infer()", which tries
# in-cluster ServiceAccount credentials, then $KUBECONFIG, then
# ~/.kube/config, in that order. Inside a plain Docker container (compose)
# NONE of those three exist, so a fragment missing kubeconfig_path is
# unconditionally a misconfiguration there -- that is the premise the
# `validate_fragment` call below was written against, and it still holds at
# full strength for compose.
#
# Inside a Kubernetes POD, the FIRST of those three is real: the kubelet
# projects a ServiceAccount token at IN_CLUSTER_TOKEN_FILE in every pod
# (whether or not anything reads it), and gears-serviceaccount.yaml's
# `qa-platform-gears` account plus rbac-argo.yaml's RoleBinding are what make
# that token resolve to something with real RBAC in the `argo` namespace. So
# a chart-rendered fragment that deliberately omits kubeconfig_path -- to let
# Config::infer() pick up that very token, which is the whole point of
# running in-cluster rather than against a rewritten copy of the node's own
# kubeconfig -- is not the failure this check exists to catch. DO NOT DELETE
# THIS BRANCH TO "SIMPLIFY" THE GUARD BACK TO ALWAYS REQUIRING
# kubeconfig_path: that would make the Helm chart's own fragments
# (gears-argo-configmaps.yaml) refuse to boot by design.
#
# NOT OVERRIDABLE, deliberately, unlike GEARS_CONFIG_FILE/PUBLIC_HOST
# elsewhere in this script. The kubelet's projected-token path is fixed by
# Kubernetes, so no real deployment ever needs to relocate it -- an
# ordinary-looking env var that could is a standing way to disable a guard
# whose whole purpose is preventing a silent, total failure of the D4 Secret
# writer, the kind of knob that gets set during an incident and never unset.
# A test that needs this path present creates it at this literal location
# inside its own disposable container/filesystem instead of relocating the
# check.
IN_CLUSTER_TOKEN_FILE=/var/run/secrets/kubernetes.io/serviceaccount/token

if [[ -n "$QA_ENVIRONMENTS_ARGO_CONFIG" ]]; then
    validate_fragment QA_ENVIRONMENTS_ARGO_CONFIG "$QA_ENVIRONMENTS_ARGO_CONFIG" 'gears.qa-environments.config' \
        '^      # QA_ENVIRONMENTS_ARGO_FRAGMENT$' \
        "A fragment without render-argo.sh's marker line cannot be told apart from the qa-runs fragment in the rendered file, so the post-condition below could not prove it was inserted"
    if [[ -f "$IN_CLUSTER_TOKEN_FILE" ]]; then
        echo "entrypoint: '$IN_CLUSTER_TOKEN_FILE' exists -- an absent kubeconfig_path in '$QA_ENVIRONMENTS_ARGO_CONFIG' is treated as the in-cluster path (Config::infer() will use the projected ServiceAccount token), not a misconfiguration"
    else
        validate_fragment QA_ENVIRONMENTS_ARGO_CONFIG "$QA_ENVIRONMENTS_ARGO_CONFIG" 'gears.qa-environments.config' \
            '^        kubeconfig_path:[[:space:]]*[^[:space:]]' \
            "A fragment without a kubeconfig_path leaves qa-environments' D4 Secret writer on Config::infer(), which finds no service-account token ('$IN_CLUSTER_TOKEN_FILE' does not exist), no \$KUBECONFIG and no ~/.kube/config inside this container -- so every runner-credential Secret write fails, on every cycle, while observation keeps working and the stack looks healthy"
    fi
    echo "entrypoint: will insert $(grep -c '' "$QA_ENVIRONMENTS_ARGO_CONFIG") line(s) from '$QA_ENVIRONMENTS_ARGO_CONFIG' after the QA_ENVIRONMENTS_ARGO_ANCHOR line"
fi

# POSTGRES_HOST/USER/PASSWORD are interpolated into a sed replacement, and
# sed's `s/pattern/replacement/` reinterprets three characters inside
# `replacement` itself: `&` means "the whole matched line" (a credential
# containing `&` would silently replace the field with the entire line --
# exactly the kind of silent corruption this script exists to prevent, just
# arriving by a different route than the anchor check above); an
# unescaped `/` prematurely ends the replacement field, breaking the sed
# expression's own syntax; and `\` starts an escape sequence sed
# interprets on its own terms. Real deployment credentials are commonly
# base64-derived and routinely contain `/`, `+`, `&` and `=` -- the
# alphanumeric "qa"/"qa" default used in every verification run never
# exercises this path, which is exactly why it went unnoticed. Escape all
# three (backslash first, so a literal backslash in the input doesn't get
# double-escaped by the later substitutions) before building the sed
# expression, so the substituted value lands as literal text no matter
# what it contains.
escape_sed_replacement() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\//\\/}"
    s="${s//&/\\&}"
    printf '%s' "$s"
}

esc_host="$(escape_sed_replacement "$POSTGRES_HOST")"
esc_user="$(escape_sed_replacement "$POSTGRES_USER")"
esc_password="$(escape_sed_replacement "$POSTGRES_PASSWORD")"

# `issuer_pattern` entries are REGEXES, not literals -- deliberately so, and
# the reason is spelled out at gears/qa-platform/config/qa-platform-stack.yaml
# around :223 (an exact `issuer:` entry is url-validated at config load and an
# http issuer fails there; `issuer_pattern` is not validated). The consequence
# for this substitution: an IPv4 literal's dots are regex "any character"
# unless escaped, so an unescaped `10.136.20.200` would also match
# `10x136y20z200`. That is looser than it looks and it is the kind of looseness
# that never shows up in testing, because the intended host always matches too.
# Escape the dots. (`-` needs no escape outside a character class, and `:` and
# `/` -- which the origin now also contains -- are literals in a regex too;
# every other character the validation above allows is alphanumeric.)
escape_regex_literal() {
    local s="$1"
    s="${s//./\\.}"
    printf '%s' "$s"
}

# The rendered line uses SINGLE quotes where the template uses double quotes,
# and that is load-bearing rather than cosmetic. YAML processes escapes inside
# a double-quoted scalar and `\.` is not one of the escapes it knows, so
# `issuer_pattern: "http://10\.136\.20\.200:8180/..."` is not a config with a
# regex in it -- it is a YAML syntax error, and the server would refuse to
# start on a file this script wrote. Measured with PyYAML: the double-quoted
# form raises `ScannerError: found unknown escape character`, the
# single-quoted form loads as the literal `http://10\.136\.20\.200:8180/...`,
# which is exactly what the regex needs. (Doubling the backslashes to `\\.`
# inside the double quotes also works and was rejected: it makes the number of
# backslashes a reader has to count depend on which of three layers -- sed,
# YAML, regex -- they are looking at.)
#
# When the origin is the default `http://localhost:8180` this substitution
# rewrites the line to a single-quoted scalar carrying the identical string, so
# the parsed config is unchanged; that is verified by loading template and
# rendered file with a YAML parser and comparing the whole documents. Rendering
# unconditionally, rather than skipping when the origin is the default, is
# deliberate: a branch that only ever runs on the remote host is a branch no
# local run exercises, and this script's own header already carries one bug
# that hid in exactly that shape.
esc_issuer_origin="$(escape_sed_replacement "$(escape_regex_literal "$PUBLIC_ISSUER_ORIGIN")")"

# No `-i`: read CONFIG_FILE (the template, checked above and never
# mutated) and write the substituted result to RENDERED_CONFIG_FILE. This
# is what keeps every start identical -- CONFIG_FILE's anchor count is the
# same on start 1 and start 10 -- and what lets CONFIG_FILE live on a
# read-only mount (a Kubernetes ConfigMap volume) without this script
# failing to write to it.
#
# The issuer expression uses `|` as sed's delimiter, not `/`: its pattern and
# replacement are both URLs, and with `/` as the delimiter every one of those
# slashes would need escaping in the expression itself -- five per side, for no
# gain. The three database expressions keep `/` because none of their anchors
# contains one.
#
# The fragment insertion is `r`, sed's "read this file in after the matching
# line", added as its own `-e` only when a fragment was given -- so an unset
# QA_RUNS_ARGO_CONFIG produces the exact expression list this script always
# had. `r`'s filename argument runs to the end of its expression, which is why
# it cannot share an `-e` with anything.
SED_ARGS=(
    -e "s/^      host: \"localhost\"\$/      host: \"${esc_host}\"/"
    -e "s/^      user: \"qa\"\$/      user: \"${esc_user}\"/"
    -e "s/^      password: \"qa\"\$/      password: \"${esc_password}\"/"
    -e "s|^          - issuer_pattern: \"http://localhost:8180/realms/qa-platform\"\$|          - issuer_pattern: '${esc_issuer_origin}/realms/qa-platform'|"
)
if [[ -n "$QA_RUNS_ARGO_CONFIG" ]]; then
    SED_ARGS+=(-e "/^      # QA_RUNS_ARGO_ANCHOR\$/r $QA_RUNS_ARGO_CONFIG")
fi
if [[ -n "$QA_ENVIRONMENTS_ARGO_CONFIG" ]]; then
    SED_ARGS+=(-e "/^      # QA_ENVIRONMENTS_ARGO_ANCHOR\$/r $QA_ENVIRONMENTS_ARGO_CONFIG")
fi

sed "${SED_ARGS[@]}" "$CONFIG_FILE" > "$RENDERED_CONFIG_FILE"

# POST-CONDITION for the insertion, and it is not a restatement of the checks
# above: those validated the FRAGMENT, this proves the RENDERED file received
# it. sed's `r` reports nothing when its anchor does not match, and the anchor
# check ran against the template rather than against this output.
if [[ -n "$QA_RUNS_ARGO_CONFIG" ]]; then
    if grep -qE '^      executor:[[:space:]]*argo[[:space:]]*$' "$RENDERED_CONFIG_FILE"; then
        echo "entrypoint: rendered $RENDERED_CONFIG_FILE with the argo executor block inserted (executor: argo is present)"
    else
        echo "entrypoint: the argo fragment was validated but '      executor: argo' is NOT in $RENDERED_CONFIG_FILE -- the insertion did not happen. Refusing to start on the mock executor." >&2
        exit 1
    fi
fi

# The same post-condition for the second fragment, and it is not redundant with
# the first: the two insertions use two different anchors, so one can land
# while the other does not.
if [[ -n "$QA_ENVIRONMENTS_ARGO_CONFIG" ]]; then
    # The MARKER, not `kubeconfig_path:` -- the qa-runs fragment carries a line
    # of that name at the same depth, so grepping for it here would pass on the
    # qa-runs insertion alone and prove nothing about this one.
    if grep -qE '^      # QA_ENVIRONMENTS_ARGO_FRAGMENT$' "$RENDERED_CONFIG_FILE"; then
        echo "entrypoint: rendered $RENDERED_CONFIG_FILE with the qa-environments argo block inserted (QA_ENVIRONMENTS_ARGO_FRAGMENT marker is present)"
    else
        echo "entrypoint: the qa-environments argo fragment was validated but its QA_ENVIRONMENTS_ARGO_FRAGMENT marker is NOT in $RENDERED_CONFIG_FILE -- the insertion did not happen. Decision D4's Secret write would fall back to Config::infer() and fail on every cycle. Refusing to start." >&2
        exit 1
    fi
fi

# The command line (qa-platform.Dockerfile's CMD) names CONFIG_FILE literally
# (`--config /etc/cf-gears/qa-platform-stack.yaml`) because that is the one
# path this image ships as a build-time constant. Since the server must
# actually read the rendered file, not the untouched template, rewrite any
# argument that matches CONFIG_FILE exactly to RENDERED_CONFIG_FILE before
# exec'ing -- everything else in "$@" passes through unchanged, and `exec`
# (not a subshell call) still replaces this script so the server becomes
# PID 1 and receives signals directly.
args=()
for arg in "$@"; do
    if [[ "$arg" == "$CONFIG_FILE" ]]; then
        args+=("$RENDERED_CONFIG_FILE")
    else
        args+=("$arg")
    fi
done

exec "${args[@]}"
