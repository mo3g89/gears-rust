//! ADR-0001's containment, asserted rather than assumed.
//!
//! **The plan asked for a stronger claim than is true, and this asserts the
//! true one** (ruling F-19). Task 19 Step 4 wanted
//! `cargo tree -p qa-environments --all-features -i kube` to error — no
//! Kubernetes anywhere in this crate's graph. That is unachievable while this
//! gear writes decision D4's runner `Secret`, and the only way to make such a
//! test pass would have been to delete a production feature that
//! `deploy/remote/verify-k8s.sh` says every workflow run depends on ("every
//! workflow run hangs on `FailedMount` while the rest of the stack looks
//! healthy"). A containment test that passes because a feature was deleted is
//! worse than no test.
//!
//! So two claims, both measured:
//!
//! 1. **a default build has no Kubernetes at all** - which is what an operator
//!    deploying without `runner-secret` gets, and what ADR-0001 is really
//!    about;
//! 2. **under `--all-features`, `kube` reaches this crate directly and only
//!    through the runner-`Secret` writer** — no transitive path, so nothing
//!    else in the graph can quietly acquire one.
//!
//! # Why the default-feature form alone would prove nothing
//!
//! Measured at Task 18b: `cargo tree -p qa-environments -i kube` **already**
//! errored before Task 19, because the feature was never a default. A test
//! asserting only that would have passed before this task and after it — a
//! could-not-fail assertion, the seventh of that shape found on this plan.
//! Claim 2 is the half with content: it fails the moment anything but the
//! writer pulls `kube` in.

use std::process::Command;

/// Run `cargo tree` for this crate and return its combined output.
fn cargo_tree(extra: &[&str]) -> String {
    let mut command = Command::new(env!("CARGO"));
    command
        .args(["tree", "-p", "qa-environments", "-i", "kube"])
        .args(extra)
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    let output = command.output().expect("`cargo tree` must be runnable");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A default build carries no Kubernetes client at all.
#[test]
fn a_default_build_has_no_kubernetes_in_its_dependency_graph() {
    let output = cargo_tree(&[]);
    assert!(
        output.contains("did not match any packages"),
        "ADR-0001: a default `qa-environments` build must not have `kube` in its \
         graph at all, got:\n{output}"
    );
}

/// With every feature on, `kube` is a **direct** dependency of this crate and
/// of nothing else in its graph.
///
/// The shape `cargo tree -i` prints is the reverse graph: `kube` at the root,
/// and beneath it every crate that depends on it. Exactly one line under the
/// root means exactly one dependant.
#[test]
fn kube_reaches_this_crate_only_through_the_runner_secret_writer() {
    let output = cargo_tree(&["--all-features"]);
    let dependants: Vec<&str> = output
        .lines()
        .map(str::trim)
        // `cargo tree` prefixes each dependant with a box-drawing connector.
        // Matched by the trailing ASCII dashes rather than by the connector
        // glyph itself, because this crate denies non-ASCII literals.
        .filter(|line| line.contains("\u{2500}\u{2500} "))
        .collect();

    assert_eq!(
        dependants.len(),
        1,
        "`kube` must have exactly one dependant: this crate, for decision D4's \
         runner-Secret writer. A second one means something else acquired a \
         Kubernetes client, which is what ADR-0001 forbids. Got:\n{output}"
    );
    assert!(
        dependants[0].contains("qa-environments"),
        "and that dependant must be this crate, got:\n{output}"
    );
}
