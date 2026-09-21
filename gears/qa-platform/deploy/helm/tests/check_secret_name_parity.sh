#!/usr/bin/env bash
# Proves the SHELL and the RUST derive the same Kubernetes `Secret` name.
#
# WHAT WAS UNCHECKED. One naming rule -- `{readable}-{digest}`, the digest
# being FNV-1a 64-bit over `prefix + tenant + "-" + reference` -- is
# implemented FOUR times:
#
#   * `qa-runs/src/infra/executor/argo/naming.rs::secret_name`
#         what the runner pod's volume actually resolves against
#   * `qa-environments/src/infra/runner_secret_writer.rs::secret_name`
#         what the self-heal cycle writes
#   * `connectors/qa-connector-k8s/src/secret_writer.rs::secret_name`
#         the connector's own copy
#   * `deploy/argo/provision-platform-kubeconfig-secret.sh::derive_name`
#     and `deploy/argo/rename-qa-secrets.sh::new_name`/`old_name`
#         the operator's manual fallback and the migration script
#
# The three Rust copies check each other: each pins a table of expected
# names and `cargo test` computes the left-hand side for real. The SHELL
# was checked by nobody. Its side of the agreement was established once, by
# hand: `naming.rs`'s own test is named
# `secret_name_matches_a_manual_transcription_of_the_shell_scripts_output`
# and its doc says outright that the literals are "a snapshot of one run,
# not a computation this test performs or re-verifies", and that closing
# the gap "means this test (or something beside it) actually executing the
# shell script and comparing its output". This is that something.
#
# Worse, the two other Rust copies cite, as their authority, a test called
# `secret_names_agree_with_the_provisioning_scripts_shell_derivation` --
# which does not exist anywhere in the workspace. `grep -rn` for it finds
# only the two doc comments that cite it. So the chain of trust for the
# shell side terminated in a name, not a check.
#
# The symptom of drift is not a failing build. It is a pod that stays
# `Pending` with a `FailedMount` event on a stand, for a Secret whose name
# differs from the one an operator created by one character.
#
# HOW THIS WORKS, and why it is not a fourth copy of the rule. Following
# `check_no_password_in_argv.sh` and `test_collect_exit_code.sh`: it
# extracts the REAL derivation functions out of the REAL shell scripts by
# line markers and `eval`s them, and it extracts the REAL expectation
# tables (and the tenant, and the prefix) out of the REAL Rust test files
# by their function names. Nothing here restates the rule or hardcodes a
# name. An edit to either side that moves a marker fails extraction
# loudly below rather than silently checking stale text.
#
# WHAT IS NOT PROVEN HERE. That the Rust implementations match their own
# tables -- that is `cargo test`'s job and it already does it. This closes
# the one remaining edge of the triangle: shell output == the table the
# Rust side is held to. Together the two mean shell == Rust.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEAR="$HERE/../../.."

PROVISION="$GEAR/deploy/argo/provision-platform-kubeconfig-secret.sh"
RENAME="$GEAR/deploy/argo/rename-qa-secrets.sh"
NAMING_RS="$GEAR/qa-runs/qa-runs/src/infra/executor/argo/naming.rs"
WRITER_RS="$GEAR/qa-environments/qa-environments/src/infra/runner_secret_writer.rs"
CONNECTOR_RS="$GEAR/connectors/qa-connector-k8s/src/secret_writer.rs"

for f in "$PROVISION" "$RENAME" "$NAMING_RS" "$WRITER_RS" "$CONNECTOR_RS"; do
    [[ -f "$f" ]] || { echo "FAIL: $f not found (a file this guard reads has moved -- update the paths here)"; exit 1; }
done

# ---------------------------------------------------------------------------
# Extract the real shell derivations, by markers, out of the real scripts.
# ---------------------------------------------------------------------------

# Print the lines of "$1" from the first line matching fixed string "$2" up
# to (and including) the first subsequent line matching fixed string "$3".
extract_block() {
    local file="$1" start_marker="$2" end_marker="$3" start rel end
    start=$(grep -nF -- "$start_marker" "$file" | head -1 | cut -d: -f1)
    [[ -n "$start" ]] || return 1
    rel=$(tail -n "+$start" "$file" | grep -nF -- "$end_marker" | head -1 | cut -d: -f1)
    [[ -n "$rel" ]] || return 1
    end=$((start + rel - 1))
    [[ "$end" -gt "$start" ]] || return 1
    sed -n "${start},${end}p" "$file"
}

# provision-platform-kubeconfig-secret.sh: DIGEST_HEX_LEN through the end of
# `derive_name`. The line that CALLS derive_name is the end marker, so the
# extracted span is exactly the derivation and none of the kubectl work.
provision_block="$(extract_block "$PROVISION" 'DIGEST_HEX_LEN=' 'SECRET_NAME="$(derive_name' || true)"
provision_block="${provision_block%SECRET_NAME=*}"
if [[ -z "$provision_block" ]]; then
    echo "FAIL: could not locate the derivation block in $PROVISION (markers moved -- update this guard's markers to match)"
    exit 1
fi

# rename-qa-secrets.sh: DIGEST_HEX_LEN through the close of `new_name`. Its
# first bare `}` at column 1 after `new_name() {` ends the function.
rename_block="$(awk '
    /^DIGEST_HEX_LEN=/ { on = 1 }
    on { print }
    /^new_name\(\) \{$/ { in_new = 1 }
    in_new && /^\}$/ { exit }
' "$RENAME")"
if [[ -z "$rename_block" ]]; then
    echo "FAIL: could not locate the derivation block in $RENAME (markers moved -- update this guard's markers to match)"
    exit 1
fi

# Sentinels actually seen in the extracted text, so a marker drifting onto the
# wrong span fails here instead of quietly testing something else.
for sentinel in 'sanitize_and_truncate' 'fnv1a_hex' 'FNV1A_PRIME' 'derive_name() {' 'readable_budget'; do
    grep -qF -- "$sentinel" <<<"$provision_block" || {
        echo "FAIL: the block extracted from $PROVISION is missing '$sentinel' -- extraction did not capture the whole derivation"
        exit 1
    }
done
for sentinel in 'sanitize_and_truncate' 'fnv1a_hex' 'FNV1A_PRIME' 'old_name() {' 'new_name() {'; do
    grep -qF -- "$sentinel" <<<"$rename_block" || {
        echo "FAIL: the block extracted from $RENAME is missing '$sentinel' -- extraction did not capture the whole derivation"
        exit 1
    }
done

# ---------------------------------------------------------------------------
# Extract the real expectation tables, by test-function name, out of the real
# Rust sources -- along with the tenant and the prefix each table is pinned
# against, so those track the source too instead of being restated here.
# ---------------------------------------------------------------------------
CASES="$(python3 - "$NAMING_RS" "$WRITER_RS" "$CONNECTOR_RS" <<'PY'
import pathlib
import re
import sys

naming_rs, writer_rs, connector_rs = (pathlib.Path(p) for p in sys.argv[1:4])

# (file, test fn, [shell fn per `for (reference, expected) in [...]` block,
#  in source order])
WANTED = [
    (naming_rs,
     "secret_name_matches_a_manual_transcription_of_the_shell_scripts_output",
     ["derive_name"]),
    (naming_rs,
     "rename_scripts_new_name_is_live_checked_old_name_is_a_transcription",
     ["old_name", "new_name"]),
    (writer_rs,
     "the_writer_the_executor_and_the_script_agree_on_every_name",
     ["derive_name"]),
    (connector_rs,
     "the_writer_the_executor_and_the_script_agree_on_every_name",
     ["derive_name"]),
]

STRING = re.compile(r'"((?:[^"\\]|\\.)*)"')
TABLE = re.compile(r"for \(reference, expected\) in \[(.*?)\n(\s*)\] \{", re.S)


def unescape(literal):
    return literal.encode("utf-8").decode("unicode_escape")


def fn_body(text, name, path):
    start = text.find(f"fn {name}(")
    if start < 0:
        sys.exit(f"FAIL: {path}: no test fn named {name} -- this guard's "
                 f"anchor moved; update it to the table's current home")
    depth, i, opened = 0, start, False
    while i < len(text):
        if text[i] == "{":
            depth += 1
            opened = True
        elif text[i] == "}":
            depth -= 1
            if opened and depth == 0:
                return text[start:i + 1]
        i += 1
    sys.exit(f"FAIL: {path}: could not find the end of fn {name}")


def tenant_of(text, path):
    found = re.search(
        r'const TENANT:\s*(?:\w+::)*\w+\s*=\s*uuid::uuid!\("([^"]+)"\)', text)
    if not found:
        sys.exit(f"FAIL: {path}: no `const TENANT: ... = uuid::uuid!(\"...\")` "
                 f"-- this guard reads the tenant from the source, not from a "
                 f"copy of it")
    return found.group(1)


tenants = set()
rows = []
for path, fn_name, shell_fns in WANTED:
    text = path.read_text(encoding="utf-8")
    tenants.add(tenant_of(text, path))
    body = fn_body(text, fn_name, path)

    # The prefix each table is pinned against, read off the assertion itself.
    prefixes = re.findall(r'(?:secret_name|old_secret_name)\("((?:[^"\\]|\\.)*)"', body)
    if not prefixes:
        sys.exit(f"FAIL: {path}: fn {fn_name} makes no `secret_name(\"<prefix>\", ...)` "
                 f"call -- this guard reads the prefix from the source")

    tables = TABLE.findall(body)
    if len(tables) != len(shell_fns):
        sys.exit(f"FAIL: {path}: fn {fn_name} has {len(tables)} expectation "
                 f"table(s), this guard expects {len(shell_fns)} "
                 f"({', '.join(shell_fns)}) -- the test changed shape")

    for (table, _indent), shell_fn, prefix in zip(tables, shell_fns, prefixes):
        literals = STRING.findall(table)
        if not literals or len(literals) % 2:
            sys.exit(f"FAIL: {path}: fn {fn_name}'s {shell_fn} table did not "
                     f"parse into (reference, expected) pairs")
        for reference, expected in zip(literals[::2], literals[1::2]):
            rows.append("\t".join([
                f"{path.name}::{fn_name}", shell_fn, prefix,
                unescape(reference), unescape(expected),
            ]))

if len(tenants) != 1:
    sys.exit("FAIL: the Rust sources disagree on TENANT: " + ", ".join(sorted(tenants)))

print(f"TENANT\t{tenants.pop()}")
print("\n".join(rows))
PY
)" || { echo "$CASES"; exit 1; }

TENANT="$(sed -n '1s/^TENANT\t//p' <<<"$CASES")"
[[ -n "$TENANT" ]] || { echo "FAIL: no tenant was extracted from the Rust sources"; exit 1; }

# ---------------------------------------------------------------------------
# Run the real shell derivations against the real Rust expectations.
# ---------------------------------------------------------------------------
derive_via_shell() {
    local shell_fn="$1" prefix="$2" tenant="$3" reference="$4"
    case "$shell_fn" in
        derive_name)
            (
                set -euo pipefail
                eval "$provision_block"
                derive_name "$prefix" "$tenant" "$reference"
            )
            ;;
        old_name | new_name)
            (
                set -euo pipefail
                SECRET_PREFIX="$prefix"
                eval "$rename_block"
                if [[ "$shell_fn" == "old_name" ]]; then
                    old_name "$reference"
                else
                    new_name "$tenant" "$reference"
                fi
            )
            ;;
        *)
            echo "FAIL: unknown shell function '$shell_fn'" >&2
            return 1
            ;;
    esac
}

failures=0
checked=0
while IFS=$'\t' read -r origin shell_fn prefix reference expected; do
    [[ "$origin" == "TENANT" || -z "$origin" ]] && continue
    got="$(derive_via_shell "$shell_fn" "$prefix" "$TENANT" "$reference")" || {
        echo "FAIL: $shell_fn exited non-zero for reference '$reference'"
        failures=$((failures + 1))
        continue
    }
    checked=$((checked + 1))
    if [[ "$got" != "$expected" ]]; then
        echo "FAIL: $shell_fn('$prefix', '$TENANT', '$reference')"
        echo "        shell: $got"
        echo "         rust: $expected   ($origin)"
        echo "      The shell and the Rust no longer derive the same Secret name."
        echo "      On a stand this is a pod that sits Pending with FailedMount,"
        echo "      not a build failure -- which is why it is checked here."
        failures=$((failures + 1))
    fi
done <<<"$CASES"

if [[ "$checked" -eq 0 ]]; then
    echo "FAIL: no cases were checked -- the extraction produced an empty table, which must not read as a pass"
    exit 1
fi

if [[ "$failures" -gt 0 ]]; then
    echo "FAIL: $failures of $checked shell/Rust Secret-name case(s) disagree"
    exit 1
fi
echo "PASS: the shell derivations and the Rust expectations agree on all $checked Secret name(s)"
