//! Kubernetes name and path hygiene, as pure functions.
//!
//! Separate from [`super::workflow`] because these are the parts of the mapping
//! that have nothing to do with Argo: they turn strings the control plane owns
//! into strings the API server will accept, and they must be testable without
//! a cluster.
//!
//! The port is explicit that this side of the boundary owns the rule:
//! [`ExecutionNode::name`](crate::domain::ports::run_executor::ExecutionNode::name)
//! is deliberately *not* DNS-sanitised, because "that constraint came from
//! Argo task names" and "an adapter with its own naming rules sanitises on its
//! own side" (`run_executor.rs:418-421`).

use uuid::Uuid;

/// Longest name the API server accepts for a `Workflow` object.
///
/// `metadata.name` is a DNS-1123 subdomain, so the hard limit is 253; the 63
/// used here is the *label* limit, and it is the one that matters, because Argo
/// derives pod names and label values from the workflow name.
pub const MAX_NAME_LEN: usize = 63;

/// Longest label **value** the API server accepts.
pub const MAX_LABEL_VALUE_LEN: usize = 63;

/// Lower-case, keep `[a-z0-9-]`, collapse everything else to `-`, trim the
/// dashes off both ends.
///
/// The source system's `sanitize_k8s` (`manager/src/services/plans.rs:803-817`)
/// verbatim, minus its `String` round-trip. Kept identical rather than
/// improved: a node name that sanitises differently here than there would make
/// the two systems' logs and pod names diverge for the same input.
fn sanitize(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

/// Truncate to `limit` bytes without leaving a trailing `-`.
///
/// A trailing dash is not a cosmetic problem: it makes the name invalid, and
/// the API server rejects the whole submission with a 422 that says nothing
/// about which of several names was at fault.
fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    value
        .get(..limit)
        .unwrap_or(value)
        .trim_end_matches('-')
        .to_owned()
}

/// A `Workflow` object name derived from a run's operator-facing name.
///
/// `fallback` is used when the run name sanitises to nothing at all — a name
/// of only non-ASCII characters does — because an empty `metadata.name` is a
/// submission the API server rejects, and a run that cannot be *named* must
/// still be runnable.
#[must_use]
pub fn workflow_name(run_name: &str, fallback: &str) -> String {
    let slug = sanitize(run_name);
    let slug = if slug.is_empty() {
        sanitize(fallback)
    } else {
        slug
    };
    let slug = if slug.is_empty() {
        "qa-run".to_owned()
    } else {
        slug
    };
    truncate(&slug, MAX_NAME_LEN)
}

/// A DAG task / template name for one execution node.
///
/// Prefixed `n-` and suffixed with the node's ordinal, which is what makes it
/// unique: two nodes named `Repo A` and `Repo/A` sanitise to the same string,
/// and Argo rejects a DAG with two tasks of one name. The ordinal is the
/// vector index, which the port says is stable and carries no dependency
/// meaning (`run_executor.rs:404-410`).
#[must_use]
pub fn task_name(node_name: &str, index: usize) -> String {
    let slug = sanitize(node_name);
    let slug = if slug.is_empty() {
        format!("n-{index}")
    } else {
        format!("n-{index}-{slug}")
    };
    truncate(&slug, MAX_NAME_LEN)
}

/// A pod-internal volume name for one of a run's mounts.
///
/// `mount-{index}`, the same shape and for the same reason as [`task_name`]:
/// the ordinal is what makes it unique, and the port says the vector's order is
/// stable (`run_executor.rs`' `ExecutionNode` doc).
///
/// # Why not the source system's `kubeconfig`
///
/// The source system names its single volume `kubeconfig` (`argo.rs:506`)
/// because it has exactly one and knows what is in it. This port renders
/// whatever mounts a product plugin declares, so it cannot: two secret volumes
/// both named `kubeconfig` is a submission the API server rejects, and naming
/// one of them for the product's *first* credential would encode a guess.
///
/// Nothing outside the `Workflow` object observes this string. Kubernetes pairs
/// a `volumeMount` to a `volume` by name inside one pod spec, and the value an
/// operator or a deploy script does look at — the `Secret`'s name — is
/// [`secret_name`]'s, derived from the credstore reference and unchanged.
#[must_use]
pub fn mount_volume_name(index: usize) -> String {
    format!("mount-{index}")
}

/// A label value: sanitised and truncated, since a rejected label fails the
/// whole submission.
#[must_use]
pub fn label_value(value: &str) -> String {
    truncate(&sanitize(value), MAX_LABEL_VALUE_LEN)
}

/// FNV-1a 64-bit offset basis / prime — deterministic, non-cryptographic
/// fingerprint (DE0708: no non-FIPS hashers). Public Fowler–Noll–Vo spec with
/// fixed constants → identical output across Rust versions and platforms.
/// Same constants as `keycloak-idp-plugin`'s `user_facade::FNV1A_BASIS`/
/// `FNV1A_PRIME` — the precedent this construction follows, not a
/// coincidence.
const FNV1A_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_PRIME: u64 = 0x0000_0100_0000_01B3;

/// Mix `bytes` into an in-progress FNV-1a 64-bit state.
fn fnv1a_update(state: &mut u64, bytes: impl AsRef<[u8]>) {
    for &b in bytes.as_ref() {
        *state ^= u64::from(b);
        *state = state.wrapping_mul(FNV1A_PRIME);
    }
}

/// Hex characters of the FNV-1a digest kept as [`secret_name`]'s digest
/// suffix — 16, the algorithm's full 64-bit width; nothing is truncated
/// away.
///
/// # Why 64 bits is enough
///
/// The suffix's job is to make two *different* `(prefix, tenant_id,
/// reference)` tuples collide only by accident, never by construction. A
/// birthday-bound collision among the credentials one tenant can plausibly
/// register — tens, perhaps low thousands, never the billions a hash this
/// size is actually rated for — needs on the order of 2^32 candidates before
/// even even-odds appear. This is a Kubernetes object name, not a security
/// token: nobody is trying to force a collision, so the bar is "vanishingly
/// unlikely by accident," not "infeasible for an adversary," and 64 bits
/// clears it with a wide margin while leaving most of the 63-character
/// budget for the human-readable head.
const DIGEST_HEX_LEN: usize = 16;

/// A Kubernetes `Secret` name derived from a tenant and a credstore
/// reference, collision-resistant under truncation.
///
/// **No material is read.**
///
/// # The defect this replaced
///
/// The first version of this function put the tenant ahead of the reference
/// and truncated the whole concatenation, reasoning that truncating from the
/// right would always eat the reference and never the tenant. That reasoning
/// was correct and beside the point: with the default prefix and a
/// full-length tenant, only 14 characters of *reference* survive at all, and
/// every real reference sharing that many leading characters — measured in
/// production, `qa-environments-credential-ssh-private-key` and
/// `qa-environments-credential-vinfra-password` both truncate to
/// `qa-environment` — collided. Protecting the tenant from truncation
/// protected the wrong thing: what a bounded-length name derived from
/// unbounded input must protect is *distinctness*, for the whole input, not
/// for a prefix of it.
///
/// # The construction
///
/// `{readable}-{digest}`, where:
///
/// - `digest` is the [`DIGEST_HEX_LEN`]-character lower-case hex encoding of
///   the FNV-1a 64-bit fingerprint of `prefix + tenant_id + "-" + reference`
///   (the exact same concatenation this function used to truncate directly)
///   — a fixed-width function of the *entire* input, so no truncation of it
///   is possible and no two distinct inputs are expected to produce the same
///   one (see [`DIGEST_HEX_LEN`]'s own doc for the margin).
/// - `readable` is [`sanitize`]d and [`truncate`]d to whatever the budget
///   leaves after reserving `digest` and its separating dash
///   (`MAX_NAME_LEN - DIGEST_HEX_LEN - 1`) — a human debugging a
///   `FailedMount` event gets a hint, never a guarantee, and correctness
///   never depends on any of it surviving. In practice `tenant_id`'s own hex
///   digits (a [`Uuid`]'s `Display` is never itself sanitised away) mean
///   `readable` is never actually empty, but the code does not lean on
///   that: an empty `readable` — impossible today, not assumed impossible —
///   still produces `digest` alone, a valid label, rather than a leading
///   `-digest` the API server would reject.
///
/// This has no failure mode: `digest` is fixed-width and always fits inside
/// [`MAX_NAME_LEN`], so unlike the version this replaced, no input can
/// exhaust the budget before a name is derivable. There is no precondition
/// left for a caller to check first.
///
/// # Why FNV-1a, not `sha2` and not `aws-lc-rs`
///
/// This is a naming digest, not a security boundary — nothing here resists
/// an adversary, only accidental collision — so this workspace's mandated
/// crypto primitive (`aws-lc-rs`) is not the load-bearing reason to pick a
/// particular algorithm, and cryptographic strength is not the property
/// being bought. `sha2` was tried first and is wrong for a different reason:
/// Dylint's `DE0708` (`dylint.toml`'s `hasher_allowed_paths`) bans a direct
/// `sha2`/`sha1`/`md5` import outside one allow-listed file, precisely
/// because this repository already went through the exercise of removing
/// direct `sha2` usage and does not want it back (`CHANGELOG.md`: "replace
/// all direct sha2 usage with FNV-1a"). `keycloak-idp-plugin`'s
/// `user_facade::legacy_filter_hash` is the standing precedent for exactly
/// this shape of problem — a deterministic, non-cryptographic fingerprint
/// used to name/key something, not to authenticate it — and this function
/// follows it: same constants, same reasoning ("fixed constants → identical
/// output across Rust versions and platforms").
///
/// What is load-bearing for the choice of *algorithm family* (a fingerprint,
/// not a MAC or a general-purpose crypto hash): the *same* digest has to be
/// computable from bash, by an operator running
/// `provision-platform-kubeconfig-secret.sh` or `rename-qa-secrets.sh`
/// (`naming.rs`'s parity tests pin this crate's output against theirs).
/// FNV-1a is a handful of XOR-and-multiply steps over the input's bytes —
/// no library call, no dependency, nothing to confirm is installed on a
/// deploy host, in either language. The shell side (`derive_name`/`new_name`
/// in both scripts) implements it as a small loop reading each byte's
/// decimal value from `od`, byte-for-byte identical to what this function
/// does with `&[u8]` — verified by running both against the same inputs
/// during review, not assumed from the spec alone.
#[must_use]
pub fn secret_name(prefix: &str, tenant_id: Uuid, reference: &str) -> String {
    let full = format!("{prefix}{tenant_id}-{reference}");
    let mut state = FNV1A_BASIS;
    fnv1a_update(&mut state, full.as_bytes());
    let digest = format!("{state:016x}");
    let suffix = &digest[..DIGEST_HEX_LEN];

    let readable_budget = MAX_NAME_LEN - DIGEST_HEX_LEN - 1;
    let readable = truncate(&sanitize(&full), readable_budget);

    if readable.is_empty() {
        suffix.to_owned()
    } else {
        format!("{readable}-{suffix}")
    }
}

/// The source system's `normalize_test_path`
/// (`manager/src/services/plans.rs:782-787`), used on both sides of the marker
/// grammar so a file reported by the runner keys against the file the plan
/// asked for.
#[must_use]
pub fn normalize_test_path(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .replace('\\', "/")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        DIGEST_HEX_LEN, MAX_NAME_LEN, label_value, normalize_test_path, sanitize, secret_name,
        task_name, truncate, workflow_name,
    };
    use uuid::Uuid;

    /// A fixed, readable tenant used everywhere a test needs one — its
    /// 36-character form is what actually consumes most of the 63-character
    /// budget these tests are about.
    const TENANT: Uuid = uuid::uuid!("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");

    #[test]
    fn a_run_name_becomes_a_dns_1123_workflow_name() {
        assert_eq!(workflow_name("Smoke Tests-1", "x"), "smoke-tests-1");
        assert_eq!(workflow_name("smoke_tests-1", "x"), "smoke-tests-1");
        assert_eq!(workflow_name("--Trim--", "x"), "trim");
    }

    /// The case that would otherwise submit an object with an empty name.
    #[test]
    fn a_name_that_sanitises_to_nothing_falls_back_and_never_to_empty() {
        assert_eq!(
            workflow_name("...", "0f8b1c2d"),
            "0f8b1c2d",
            "the fallback is the run id, which always sanitises to something"
        );
        assert_eq!(workflow_name("...", "..."), "qa-run");
    }

    /// A truncation that left a trailing dash would be rejected by the API
    /// server, and the rejection would name the whole submission rather than
    /// the name.
    #[test]
    fn truncation_never_leaves_a_trailing_dash() {
        let long = format!("{}-tail", "a".repeat(MAX_NAME_LEN - 1));
        let name = workflow_name(&long, "x");
        assert_eq!(name.len(), MAX_NAME_LEN - 1);
        assert!(!name.ends_with('-'));
    }

    /// Two node names that sanitise identically must still produce two task
    /// names, because Argo rejects a DAG with duplicate task names — and the
    /// run would fail at submission with no per-node explanation.
    #[test]
    fn nodes_that_sanitise_alike_still_get_distinct_task_names() {
        assert_ne!(task_name("Repo A", 0), task_name("Repo/A", 1));
        assert_eq!(task_name("Repo A", 0), "n-0-repo-a");
        assert_eq!(task_name("...", 3), "n-3");
    }

    #[test]
    fn a_secret_name_is_derived_from_the_tenant_and_reference_and_carries_no_material() {
        assert_eq!(
            secret_name("qa-platform-", TENANT, "kubeconfig"),
            "qa-platform-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee-b4f2cb6a37321470"
        );
    }

    /// Two tenants naming the very same reference must not derive one
    /// `Secret` — that collision, in the shared Argo namespace, is the
    /// defect this field exists to close.
    #[test]
    fn two_tenants_sharing_a_reference_derive_distinct_names() {
        let other: Uuid = uuid::uuid!("11111111-2222-3333-4444-555555555555");
        assert_ne!(
            secret_name("qa-platform-", TENANT, "shared-reference"),
            secret_name("qa-platform-", other, "shared-reference")
        );
    }

    /// **The live collision, reproduced.** Two references sharing their
    /// first 14 sanitised characters — the exact width the old
    /// tenant-before-reference truncation left for a reference under the
    /// default prefix and a full-length tenant — must still derive distinct
    /// names. `qa-environments-credential-ssh-private-key` and
    /// `qa-environments-credential-vinfra-password` are not a contrived
    /// pair: they are (anonymised only in their common part) the two
    /// references a live VHI environment actually registered, both of which
    /// resolved to the same Secret in production and made every run mount
    /// an SSH key where a vinfra password belonged. This is the test the
    /// original derivation had no equivalent of, and would have failed had
    /// it existed.
    #[test]
    fn references_sharing_a_long_common_prefix_under_one_tenant_still_derive_distinct_names() {
        let first = "qa-environments-credential-ssh-private-key";
        let second = "qa-environments-credential-vinfra-password";
        assert_eq!(
            &first[..14],
            &second[..14],
            "premise: the shared prefix is what caused the live collision"
        );
        assert_ne!(
            secret_name("qa-platform-", TENANT, first),
            secret_name("qa-platform-", TENANT, second)
        );
    }

    /// However long the reference, the name never exceeds the limit, and it
    /// always ends in the fixed-width digest — the part that makes two
    /// different references distinct regardless of what the readable head
    /// truncated away.
    #[test]
    fn an_arbitrarily_long_reference_still_derives_a_valid_bounded_name() {
        let long_reference = "a".repeat(200);
        let name = secret_name("qa-platform-", TENANT, &long_reference);
        assert_eq!(name.len(), MAX_NAME_LEN);
        assert!(!name.ends_with('-'));
        assert_eq!(
            &name[name.len() - DIGEST_HEX_LEN..],
            "b7b5946fad492593",
            "the digest suffix, not the truncated head, is what distinctness relies on"
        );
    }

    /// There is no longer a degenerate case to refuse: the digest is
    /// fixed-width, so it always fits, and the readable head is allowed to
    /// truncate to nothing (see [`secret_name`]'s own doc) rather than the
    /// function having any precondition left to violate.
    #[test]
    fn a_prefix_that_alone_would_have_exhausted_the_old_budget_still_derives_a_name() {
        let huge_prefix = "p".repeat(MAX_NAME_LEN);
        let name = secret_name(&huge_prefix, TENANT, "reference");
        assert_eq!(name.len(), MAX_NAME_LEN);
        assert!(!name.is_empty());
    }

    #[test]
    fn label_values_and_paths_normalise_the_way_the_source_system_does() {
        assert_eq!(label_value("Smoke Tests"), "smoke-tests");
        assert_eq!(normalize_test_path("./tests/a.py"), "tests/a.py");
        assert_eq!(normalize_test_path("/tests/a.py"), "tests/a.py");
        assert_eq!(normalize_test_path("tests\\a.py"), "tests/a.py");
    }
    /// **What this pins, precisely:** that `secret_name` keeps returning these
    /// exact strings for these three references under `TENANT`. A change to
    /// `secret_name`, `sanitize` or `truncate` that alters any of them fails
    /// this test.
    ///
    /// **What this does NOT catch:** an edit to
    /// `deploy/argo/provision-platform-kubeconfig-secret.sh`'s `derive_name`.
    /// This test never reads that file and never executes it -- there is no
    /// mechanism here by which a change to the shell script could turn this
    /// test red. Despite the name, "agree with the shell derivation" is not a
    /// live, machine-checked property; it is a claim about the past.
    ///
    /// **How the right-hand sides were obtained:** a human ran
    /// `derive_name` from that script once, by hand, with `TENANT` spliced
    /// in ahead of the reference, and transcribed the output into the
    /// literals below. They are a snapshot of one run, not a computation this
    /// test performs or re-verifies.
    ///
    /// **The gap is now closed, by a script-side guard rather than by this
    /// test.** `deploy/helm/tests/check_secret_name_parity.sh`, run by `make
    /// helm-tests`, extracts `derive_name` out of the real
    /// `provision-platform-kubeconfig-secret.sh` by markers, extracts the
    /// table below (with `TENANT` and the prefix) out of this file, and runs
    /// the one against the other. So these literals are still a
    /// transcription as far as *this* test is concerned -- it computes only
    /// the left-hand side -- but they are no longer trusted on nobody's
    /// authority: the shell is executed against them on every
    /// `make helm-tests`. Change a literal here and that guard fails; change
    /// the shell and it fails too.
    #[test]
    fn secret_name_matches_a_manual_transcription_of_the_shell_scripts_output() {
        for (reference, expected) in [
            (
                "argo-proof-kubeconfig",
                "qa-platform-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee-838ca89b7ac87ce3",
            ),
            (
                "platform/9f2c.../kubeconfig",
                "qa-platform-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee-27eda498619eaca1",
            ),
            (
                "UPPER_Case",
                "qa-platform-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee-860e9a9caad412aa",
            ),
        ] {
            assert_eq!(
                secret_name("qa-platform-", TENANT, reference),
                expected,
                "reference {reference:?} must derive the same Secret name in \
                 Rust and in the shell script that creates it"
            );
        }
    }

    /// `deploy/argo/rename-qa-secrets.sh` carries **two** derivations of its
    /// own -- `old_name` (the pre-tenant rule, so it can find the Secret an
    /// operator provisioned under the old scheme) and `new_name` (this
    /// crate's current rule, so it knows what to create it as). This test's
    /// two halves pin them, but not with the same strength, and the
    /// difference matters:
    ///
    /// **The `new_name` half is a real, live check.** It calls production
    /// [`secret_name`] directly and asserts its output against the literals
    /// below, so a change to `secret_name`, `sanitize` or `truncate` fails
    /// it -- the same guarantee
    /// [`secret_name_matches_a_manual_transcription_of_the_shell_scripts_output`]
    /// gives for the other script.
    ///
    /// **The `old_name` half is not.** No production Rust function computes
    /// the pre-tenant rule any more, so it is reconstructed locally
    /// (`old_secret_name`, below) from the still-present `sanitize`/`truncate`
    /// primitives and compared against literals.
    ///
    /// **What neither half catches:** an edit to `rename-qa-secrets.sh`
    /// itself -- its `sanitize_and_truncate`, `old_name` or `new_name`
    /// functions. This test never reads or executes that script. A local
    /// Rust reconstruction that happens to agree with the shell today says
    /// nothing about whether it still will after the next edit to either.
    ///
    /// **How the literals were obtained:** both tables are a human's
    /// transcription of one run of `rename-qa-secrets.sh`'s own
    /// `sanitize_and_truncate`/`old_name`/`new_name` functions, by hand --
    /// the `old_name` table is also, deliberately, the same references and
    /// right-hand sides
    /// [`secret_name_matches_a_manual_transcription_of_the_shell_scripts_output`]
    /// pinned before this task added a tenant, since the pre-tenant rule
    /// itself did not change.
    ///
    /// **Closing the gap** -- for this script and for
    /// `provision-platform-kubeconfig-secret.sh` alike -- means actually
    /// executing the shell and comparing, which is deliberately not built
    /// here; see the other test's doc for why.
    #[test]
    fn rename_scripts_new_name_is_live_checked_old_name_is_a_transcription() {
        // `old_name`: `sanitize(prefix + reference)`, no tenant -- what an
        // operator-provisioned Secret is still named today, until this
        // script (or an operator) moves it. No *production* Rust function
        // computes this any more (every exported `secret_name` now requires
        // a tenant, by design), so it is reconstructed here from the same
        // `sanitize`/`truncate` primitives `secret_name` itself is built
        // from. That makes this a real Rust computation rather than a
        // literal asserted against itself -- but it is still checked against
        // a transcribed value, not against the shell script live; see this
        // test's own doc.
        fn old_secret_name(prefix: &str, reference: &str) -> String {
            truncate(&sanitize(&format!("{prefix}{reference}")), MAX_NAME_LEN)
        }
        for (reference, expected) in [
            ("argo-proof-kubeconfig", "qa-platform-argo-proof-kubeconfig"),
            (
                "platform/9f2c.../kubeconfig",
                "qa-platform-platform-9f2c----kubeconfig",
            ),
            ("UPPER_Case", "qa-platform-upper-case"),
        ] {
            assert_eq!(
                old_secret_name("qa-platform-", reference),
                expected,
                "reference {reference:?} must derive the same Secret name via the pre-tenant \
                 rule as rename-qa-secrets.sh's old_name derives"
            );
        }

        // `new_name`: the same digest-suffixed construction as `secret_name`
        // -- must agree with it exactly, since this is what the migrated
        // Secret's name must be for `qa-runs`' executor to mount it.
        for (reference, expected) in [
            (
                "argo-proof-kubeconfig",
                "qa-platform-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee-838ca89b7ac87ce3",
            ),
            (
                "platform/9f2c.../kubeconfig",
                "qa-platform-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee-27eda498619eaca1",
            ),
            (
                "UPPER_Case",
                "qa-platform-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee-860e9a9caad412aa",
            ),
        ] {
            assert_eq!(
                secret_name("qa-platform-", TENANT, reference),
                expected,
                "reference {reference:?} must derive the same Secret name via naming::secret_name \
                 as rename-qa-secrets.sh's new_name derives"
            );
        }
    }
}
