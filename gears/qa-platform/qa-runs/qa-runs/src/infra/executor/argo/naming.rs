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

/// A label value: sanitised and truncated, since a rejected label fails the
/// whole submission.
#[must_use]
pub fn label_value(value: &str) -> String {
    truncate(&sanitize(value), MAX_LABEL_VALUE_LEN)
}

/// A Kubernetes `Secret` name derived from a credstore reference.
///
/// **No material is read.** A reference like `platform/9f2c.../kubeconfig`
/// becomes `{prefix}platform-9f2c----kubeconfig` — FOUR dashes, one per
/// replaced character, because runs are not collapsed; this line said three
/// until the parity test below counted them — and the kubelet resolves it —
/// or fails to, which for a kubeconfig volume is the required behaviour
/// (`run_executor.rs:68-70`).
#[must_use]
pub fn secret_name(prefix: &str, reference: &str) -> String {
    truncate(&sanitize(&format!("{prefix}{reference}")), MAX_NAME_LEN)
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
        MAX_NAME_LEN, label_value, normalize_test_path, secret_name, task_name, workflow_name,
    };

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
    fn a_secret_name_is_derived_from_the_reference_and_carries_no_material() {
        assert_eq!(
            secret_name("qa-platform-", "platform/9f2c/kubeconfig"),
            "qa-platform-platform-9f2c-kubeconfig"
        );
    }

    #[test]
    fn label_values_and_paths_normalise_the_way_the_source_system_does() {
        assert_eq!(label_value("Smoke Tests"), "smoke-tests");
        assert_eq!(normalize_test_path("./tests/a.py"), "tests/a.py");
        assert_eq!(normalize_test_path("/tests/a.py"), "tests/a.py");
        assert_eq!(normalize_test_path("tests\\a.py"), "tests/a.py");
    }
    /// Parity oracle for `deploy/argo/provision-platform-kubeconfig-secret.sh`,
    /// which reimplements this function in bash because an operator has to
    /// create the Secret this name refers to before a run can mount it.
    ///
    /// Two implementations of one naming rule is a drift risk whose symptom is
    /// silent -- a pod stuck `Pending` on `FailedMount` -- so the cases the
    /// shell version was checked against are pinned here. Each right-hand side
    /// was produced by RUNNING that script's `derive_name`, not by reading it.
    #[test]
    fn secret_names_agree_with_the_provisioning_scripts_shell_derivation() {
        for (reference, expected) in [
            ("argo-proof-kubeconfig", "qa-platform-argo-proof-kubeconfig"),
            (
                "platform/9f2c.../kubeconfig",
                "qa-platform-platform-9f2c----kubeconfig",
            ),
            ("UPPER_Case", "qa-platform-upper-case"),
        ] {
            assert_eq!(
                secret_name("qa-platform-", reference),
                expected,
                "reference {reference:?} must derive the same Secret name in \
                 Rust and in the shell script that creates it"
            );
        }
    }
}
