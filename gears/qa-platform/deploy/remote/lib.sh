# Shared front end for gears/qa-platform/deploy/remote/*.sh drivers --
# sync.sh (the docker-compose stack) and deploy-k8s.sh (the k3s/Helm stack).
# SOURCED, not executed (`source lib.sh` / `. lib.sh`); it defines functions
# and a handful of arrays/defaults and runs nothing on its own.
#
# WHAT LIVES HERE, and why it is safe to share between two drivers whose
# back ends (compose vs. Helm) have nothing in common: everything below is
# about REACHING the remote host and proving a REMOTE_PATH mirror is safe to
# rsync into -- neither of which depends on what gets deployed once the
# mirror exists. It was extracted out of sync.sh (Task 3-10's only proven
# deploy path) verbatim where possible, specifically so sync.sh's own
# behaviour does not change by one byte; see that script's own comments for
# the reasoning behind each piece, which is not repeated here.
#
# THE CALLING CONTRACT for remote_sh / remote_sh_expect / ssh_ro_script:
# unlike the single-script version this was extracted from (which closed
# over sync.sh's own eight compose-specific globals -- PUBLIC_HOST, MARKER,
# POSTGRES_HOST_IP, PUBLIC_ISSUER_ORIGIN, PUBLIC_UI_ORIGIN, COMPOSE_FILE_LIST,
# SMOKE_TERMINAL_TIMEOUT, REMOTE_PATH -- baked into every call), these three
# functions now forward EXPLICIT positional arguments: whatever the caller
# passes after the label (and, for remote_sh_expect, the sentinel) becomes
# the remote script's "$1", "$2", ... A caller with no values to forward
# (the docker-install body below, for instance) simply passes none. This is
# what lets deploy-k8s.sh use the same three functions with its own,
# unrelated set of values (REMOTE_PATH, PUBLIC_ORIGIN, IMAGE_TAG, ...)
# without adopting sync.sh's compose vocabulary.
#
# THE CALLER MUST HAVE SET, before calling anything below: REMOTE_TARGET
# (ssh destination), REMOTE_PATH (mirror directory, used only in die()
# messages here), and DRY_RUN (true/false). preflight_disk_space and
# preflight_rsync_path_guard also read MIN_DOCKER_GIB / MIN_PATH_GIB, with
# the same defaults sync.sh has always used, applied here so a caller that
# does not care can leave them unset.

# A floor, not a measurement -- see sync.sh's own comment on this number for
# the derivation. Overridable because it is a guess, and shared so both
# drivers guard against the same cold-build cost.
MIN_DOCKER_GIB="${MIN_DOCKER_GIB:-40}"
MIN_PATH_GIB="${MIN_PATH_GIB:-2}"

# Written into REMOTE_PATH so a later run -- by either driver -- can
# recognise its own work before running rsync --delete. Shared between
# sync.sh and deploy-k8s.sh on purpose: both mirror the SAME repository into
# the SAME kind of directory, and a mirror one of them created is exactly as
# safe for the other to rsync --delete into.
MARKER="${MARKER:-.gears-rust-remote-sync}"

# Batch mode is not optional: without it a host that has lost its key prompts
# for a password and the calling script hangs forever inside a step that
# reports nothing. ConnectTimeout bounds the unreachable-host case the same
# way.
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout="${SSH_CONNECT_TIMEOUT:-10}")

# The repo's own .dockerignore set (target, **/target, *.rlib, .venv,
# **/__pycache__, *.pyc, node_modules, dist, .build, .git, .gitignore,
# .github), proven sufficient because the local `docker compose build` /
# `docker build` succeed with exactly these paths absent from the build
# context -- including .git, so neither build can depend on git metadata.
#
# Bare names (no slash) match at any depth in rsync, so `target` alone
# already covers `**/target`; both are listed to keep this set diffable
# against .dockerignore.
#
# COMPOSE-SPECIFIC EXCLUDES ARE NOT HERE. sync.sh additionally excludes its
# own generated compose `.env` and `.generated` directory; that is a
# compose-only concern (deploy-k8s.sh never writes either) and stays in
# sync.sh, appended to this array rather than folded into it.
COMMON_RSYNC_EXCLUDES=(
    target "**/target" "*.rlib"
    .venv "**/.venv" "**/__pycache__" "*.pyc"
    node_modules dist .build
    .git .gitignore .github
)

# ---------------------------------------------------------------- helpers --
# PROG_NAME is the caller's own error-message prefix (sync.sh uses "sync",
# deploy-k8s.sh uses "deploy-k8s"), set by the caller before sourcing this
# file or before the first die()/step() call. A plain BASH_SOURCE[1] lookup
# would not work here: die() is called both directly by the caller AND from
# inside this file's own preflight_* functions, and in the latter case
# BASH_SOURCE[1] names lib.sh itself, not the top-level script -- so the
# prefix is an explicit variable rather than something inferred from the
# call stack.
PROG_NAME="${PROG_NAME:-remote}"
step() { printf '\n=== %s ===\n' "$*"; }
die()  { echo "$PROG_NAME: $*" >&2; exit 1; }

# Read-only remote command. Runs even under --dry-run, on purpose: knowing
# whether the host is reachable and already has Docker (or kubectl/helm) is
# exactly what a dry run is for, and none of these calls change anything.
# shellcheck disable=SC2029  # SC2029 warns that an unescaped `$var` in an ssh
# command line expands on the CLIENT side. That is exactly why every call
# site single-quotes its command string: nothing is expanded here, the
# remote shell is the only expander. Keep it that way -- if a call site ever
# needs a local value, pass it through ssh_ro_script/remote_sh's explicit
# positional arguments instead of interpolating it into the command.
ssh_ro() { ssh "${SSH_OPTS[@]}" "$REMOTE_TARGET" "$@"; }

# Same, for a read-only probe long enough to want a heredoc. The body is a
# QUOTED heredoc (<<'EOF') run by `bash -s` at the call site, with "$@" here
# becoming that script's positional parameters -- nothing in the heredoc is
# expanded locally, which is the difference between a script a reviewer can
# read and four-deep `\"\$(...)\"` escaping.
ssh_ro_script() {
    ssh "${SSH_OPTS[@]}" "$REMOTE_TARGET" bash -s -- "$@"
}

# A mutating remote step. Body comes from stdin and is run by `bash -s` on
# the remote with "$@" (everything after LABEL) as its positional
# parameters -- passed as arguments rather than interpolated into the text,
# so a value containing a shell metacharacter cannot become code, and so a
# quoted heredoc (which expands nothing locally) can still reach them. Under
# --dry-run the body is printed and not run.
#
# EVERY FAILURE IS NAMED. Without the `|| die`, a failed remote build, `up
# -d`/`helm upgrade` or verify aborted the calling script through `set -e`
# carrying only the remote's own stderr and no line saying which step it
# came from. A remote step that dies anonymously is exactly how a real
# deploy's `up -d` conflict got mistaken for a successful run on this
# project before.
remote_sh() {
    local label="$1" body
    shift
    body="$(cat)"
    if $DRY_RUN; then
        printf '\n--- [dry-run] would run on %s: %s ---\n%s\n--- end ---\n' \
            "$REMOTE_TARGET" "$label" "$body"
        return 0
    fi
    printf '%s\n' "$body" | ssh "${SSH_OPTS[@]}" "$REMOTE_TARGET" bash -s -- "$@" \
        || die "remote step failed: $label -- on $REMOTE_TARGET, in $REMOTE_PATH. The remote's own output is above this line; this line names which step produced it. Nothing after this step ran, so the remote is left as that step left it."
}

# A remote step whose SUCCESS MUST BE PROVED, not merely not-denied.
#
# WHY THIS EXISTS, and it is a bug that already happened rather than a
# precaution. `remote_sh` pipes its body into `bash -s` on the remote, so the
# script IS the remote shell's stdin -- and a command that forwards its own
# stdin (`docker compose exec -T` without `</dev/null` is the one measured
# here) reads the rest of the script and throws it away. Measured
# 2026-08-27 against this exact stack: a body whose second line was
# `docker compose exec -T gears cat .../ca.crt` printed line A, copied the
# certificate, and then EXITED 0 without running lines B and C. The whole
# VERIFY block after that -- seven checks -- had been silently skipped, and
# the deploy reported success.
#
# The direct fix is `</dev/null` on every command that could forward stdin,
# and that is done at each call site. This function is the fix for the
# CLASS: the body must print a sentinel line as its last act, and a step
# that exits 0 without printing it is a failure. Any future command that
# eats the script is then loud instead of invisible.
#
# The cost is that output is captured and printed at the end rather than
# streamed. Acceptable for the bounded verify-style steps this is used for,
# and nowhere else.
remote_sh_expect() {
    local label="$1" sentinel="$2" body out rc
    shift 2
    body="$(cat)"
    if $DRY_RUN; then
        printf '\n--- [dry-run] would run on %s: %s (must print %q) ---\n%s\n--- end ---\n' \
            "$REMOTE_TARGET" "$label" "$sentinel" "$body"
        return 0
    fi
    # 2>&1 so the FAIL lines the bodies write to stderr are interleaved with
    # their PASS lines in the order they happened, which is how they are
    # meant to be read.
    # `|| rc=$?` and not a bare assignment followed by `rc=$?`: under
    # `set -e` a failing command substitution inside a plain assignment
    # aborts the function right there, so the `rc` line would never run and
    # the diagnostic below would never be reached.
    rc=0
    out="$(printf '%s\n' "$body" | ssh "${SSH_OPTS[@]}" "$REMOTE_TARGET" bash -s -- "$@" 2>&1)" || rc=$?
    printf '%s\n' "$out"
    if [[ "$rc" -ne 0 ]]; then
        die "remote step failed: $label -- on $REMOTE_TARGET, in $REMOTE_PATH (exit $rc). Its output is above this line."
    fi
    if ! grep -qF -- "$sentinel" <<<"$out"; then
        die "remote step '$label' exited 0 but never printed its sentinel line '$sentinel'. That means it stopped early WITHOUT failing -- the shape of bug this function exists to catch: a command in the body consumed the rest of the script from stdin (see this function's header, and put '</dev/null' on it). Do not read the absence of FAIL lines above as success."
    fi
}

# ============================================================== preflight ==
# Four checks, always run in this order by both drivers (each names its own
# "N/M" in the label it passes in, since the two scripts preflight a
# different number of things). Stop at the first failure: none of these
# mutate anything but the docker-install fallback in
# preflight_docker, so a caller can treat "the function returned" as
# "safe to keep going".

# Preflight: ssh reachability (batch mode, no password prompt).
preflight_ssh_reachable() {
    local label="$1"
    step "$label"
    if ssh_ro true 2>/dev/null; then
        echo "PASS: ssh $REMOTE_TARGET answers in batch mode -- key auth works, no prompt"
    else
        die "cannot ssh to '$REMOTE_TARGET' in batch mode. Either the host is unreachable or key auth is not set up (batch mode refuses to fall back to a password prompt, which would hang here). Try: ssh -v $REMOTE_TARGET true"
    fi
}

# Preflight: the docker ENGINE on the remote, installed if absent.
#
# DOCKER ONLY -- deliberately NOT `docker compose`. There is no registry in
# this deployment: deploy-k8s.sh builds each image with `docker build` and
# hands it to the node's containerd with `docker save | ctr images import -`,
# so the engine is a hard requirement. The compose CLI plugin is not, and this
# function must never ask for it again. It once did, because sync.sh -- the
# compose back end that shared this preflight -- really ran `docker compose`,
# and installing both in one idempotent step left a host ready for either
# driver. sync.sh is gone; deploy-k8s.sh is the only caller left and never
# invokes compose (its header forbids it). All the shared check bought after
# that was a node fully ready for the Helm stack failing preflight 2/8 over a
# plugin nothing calls, then being sent to a package manager it may not have.
preflight_docker() {
    local label="$1"
    step "$label"
    if ssh_ro 'docker version --format "{{.Server.Version}}" >/dev/null 2>&1'; then
        echo "PASS: docker $(ssh_ro 'docker version --format "{{.Server.Version}}"') already present -- install skipped"
    else
        echo "the docker engine is missing or not running; installing"
        remote_sh "install docker-ce (idempotent)" <<'INSTALL'
set -euo pipefail
if docker version >/dev/null 2>&1; then
    echo "PASS: docker became available before the install ran -- nothing to do"
    exit 0
fi
# RHEL-family only. Guessing a package manager on an unknown distro would
# install the wrong thing and report success; naming the requirement is more
# useful than that.
if ! command -v dnf >/dev/null 2>&1; then
    echo "install: no dnf on this host -- install Docker Engine yourself, then re-run. Nothing else is required (no compose plugin, no Rust toolchain)." >&2
    exit 1
fi
dnf -y install dnf-plugins-core
# Two spellings because the subcommand was renamed between dnf4 and dnf5;
# both are idempotent, and the fallback writes the same repo file by hand.
if ! dnf config-manager --add-repo https://download.docker.com/linux/centos/docker-ce.repo 2>/dev/null; then
    curl -fsSL -o /etc/yum.repos.d/docker-ce.repo https://download.docker.com/linux/centos/docker-ce.repo
fi
dnf -y install docker-ce docker-ce-cli containerd.io
systemctl enable --now docker
docker version --format "installed docker {{.Server.Version}}"
INSTALL
    fi
}

# Preflight: disk space on the remote (docker root + the mirror path's
# filesystem). $1 is REMOTE_PATH; MIN_DOCKER_GIB / MIN_PATH_GIB are the
# module-level defaults above, overridable by the caller's environment.
preflight_disk_space() {
    local label="$1" remote_path="$2"
    step "$label"
    if $DRY_RUN && ! ssh_ro 'docker info --format "{{.DockerRootDir}}" >/dev/null 2>&1'; then
        echo "SKIP: docker not installed yet, so its storage directory cannot be measured (dry run)"
        return 0
    fi
    # `df -PB G` (POSIX output, GiB blocks) then field 4 = available. Measured
    # on the docker root because that, not the mirror, is where a cold Rust
    # build's intermediate layers land.
    local docker_root docker_gib path_gib
    read -r docker_root docker_gib path_gib <<<"$(ssh_ro_script "$remote_path" <<'PROBE'
set -euo pipefail
root=$(docker info --format '{{.DockerRootDir}}')
# The mirror may not exist yet, and df cannot measure a path that is not
# there; walk up to the nearest existing ancestor, which is on the same
# filesystem the mirror will be created on.
p="$1"
while [ ! -d "$p" ] && [ "$p" != / ]; do p=$(dirname "$p"); done
# -P for POSIX single-line output (a long device name otherwise wraps and
# breaks the field numbering), -BG for GiB blocks; field 4 is "available".
avail_gib() { df -PBG "$1" | awk 'NR==2{gsub(/G/,"",$4); print $4}'; }
printf '%s %s %s\n' "$root" "$(avail_gib "$root")" "$(avail_gib "$p")"
PROBE
)"
    [[ "$docker_gib" =~ ^[0-9]+$ ]] || die "could not read available space on the remote's docker root '$docker_root' (got '$docker_gib')"
    [[ "$path_gib"   =~ ^[0-9]+$ ]] || die "could not read available space for '$remote_path' (got '$path_gib')"
    (( docker_gib >= MIN_DOCKER_GIB )) \
        || die "docker storage '$docker_root' has ${docker_gib} GiB free, below the ${MIN_DOCKER_GIB} GiB floor a cold Rust build needs. Free space or raise MIN_DOCKER_GIB if you know better."
    (( path_gib >= MIN_PATH_GIB )) \
        || die "the filesystem for '$remote_path' has ${path_gib} GiB free, below the ${MIN_PATH_GIB} GiB floor. The mirror is ~70 MB but the build reads from it."
    echo "PASS: docker storage '$docker_root' has ${docker_gib} GiB free (floor ${MIN_DOCKER_GIB}); '$remote_path' filesystem has ${path_gib} GiB free (floor ${MIN_PATH_GIB})"
}

# Preflight: REMOTE_PATH is safe for rsync --delete. --delete is right for a
# dedicated mirror and catastrophic for anything else, so the directory has
# to prove it is this tooling's own before it is used: either it does not
# exist, or it is empty, or it carries the marker file a previous run (by
# either driver) wrote. Anything else is refused rather than emptied. $1 is
# REMOTE_PATH, $2 is MARKER.
preflight_rsync_path_guard() {
    local label="$1" remote_path="$2" marker="$3" path_state
    step "$label"
    path_state="$(ssh_ro_script "$remote_path" "$marker" <<'STATE'
set -euo pipefail
p="$1"
if   [ ! -e "$p" ];                              then echo absent
elif [ ! -d "$p" ];                              then echo notdir
elif [ -f "$p/$2" ];                             then echo marked
elif [ -z "$(ls -A "$p" 2>/dev/null)" ];         then echo empty
else                                                  echo foreign
fi
STATE
)"
    case "$path_state" in
        absent) echo "PASS: '$remote_path' does not exist yet -- it will be created and marked" ;;
        empty)  echo "PASS: '$remote_path' exists and is empty -- safe to populate" ;;
        marked) echo "PASS: '$remote_path/$marker' is present -- this is a previous sync of this repo, safe for --delete" ;;
        notdir) die "'$remote_path' exists on the remote and is not a directory -- refusing to touch it" ;;
        foreign) die "'$remote_path' on the remote is a non-empty directory with no '$marker' file, so it is not a previous sync of this repo. rsync --delete would erase whatever is in it. Pass --path to point somewhere else, or remove that directory yourself if you are sure." ;;
        *) die "could not determine the state of '$remote_path' (got '$path_state')" ;;
    esac
}
