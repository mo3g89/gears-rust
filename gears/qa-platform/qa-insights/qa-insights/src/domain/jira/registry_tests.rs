//! Tests for the JIRA registry core.
//!
//! One test per pinned rule, each citing the legacy line it verifies against
//! (Task 31 brief, Step 0). [`the_skip_list_renders_in_the_frozen_wire_format`]
//! is the brief's own test, verbatim except for the expected string, which Step
//! 0 supplies by reading `manager/src/routes/runs.rs:755-758`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use time::OffsetDateTime;
use uuid::Uuid;

use qa_insights_sdk::{JiraBug, SkipListEntry};

use super::{render_skip_list, skip_list_entries};

/// A `SkipListEntry` fixture — the shape [`render_skip_list`] consumes.
fn bug(test_name: &str, jira_key: &str) -> SkipListEntry {
    SkipListEntry {
        test_name: test_name.to_owned(),
        jira_key: jira_key.to_owned(),
    }
}

/// A full `JiraBug` fixture with a chosen `status`, for the open/resolved
/// filter tests. Other fields carry arbitrary but valid values —
/// [`skip_list_entries`] does not look at them.
fn make_bug(test_name: &str, jira_key: &str, status: &str) -> JiraBug {
    JiraBug {
        id: Uuid::new_v4(),
        jira_key: jira_key.to_owned(),
        test_name: test_name.to_owned(),
        repo_id: Uuid::new_v4(),
        plan_path: "plans/smoke/plan.yaml".to_owned(),
        app_version: None,
        environment_id: None,
        status: status.to_owned(),
        summary: "summary".to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        resolved_at: None,
    }
}

/// An open bug: `status == "Open"`, `resolved_at` unset.
fn open(test_name: &str, jira_key: &str) -> JiraBug {
    make_bug(test_name, jira_key, "Open")
}

/// A resolved bug: `status == "Resolved"`, `resolved_at` set — mirroring
/// `resolve_bug`'s writing both together (`manager/src/services/jira.rs:243-251`).
fn resolved(test_name: &str, jira_key: &str) -> JiraBug {
    let mut bug = make_bug(test_name, jira_key, "Resolved");
    bug.resolved_at = Some(OffsetDateTime::UNIX_EPOCH);
    bug
}

/// The wire format is frozen (`manager/src/services/argo.rs:476-481`, whose
/// input string is assembled at `manager/src/routes/runs.rs:755-758`): a
/// comma-separated `test_name:JIRA-KEY` list. Tests consume this variable
/// directly, so a spacing or ordering change is a test-contract break, not a
/// cosmetic one.
///
/// Also pins "no sort": alphabetically `"test_backup"` precedes
/// `"test_upgrade"`, so an implementation that sorted would render the pair in
/// the opposite order and this assertion would fail.
#[test]
fn the_skip_list_renders_in_the_frozen_wire_format() {
    let rendered = render_skip_list(&[
        bug("test_upgrade", "VHP-2618"),
        bug("test_backup", "VHP-2701"),
    ]);
    assert_eq!(rendered, "test_upgrade:VHP-2618,test_backup:VHP-2701");
}

/// No dedup: two open bugs against the same test name both survive, unmerged.
/// Legacy's `get_open_bugs` query has no `DISTINCT` and no `GROUP BY`
/// (`manager/src/services/jira.rs:220-229`), so nothing here should merge them
/// either.
#[test]
fn duplicate_test_names_are_not_merged() {
    let rendered = render_skip_list(&[bug("test_a", "V-1"), bug("test_a", "V-2")]);
    assert_eq!(rendered, "test_a:V-1,test_a:V-2");
}

/// Only open bugs suppress a test. A resolved bug must stop appearing the
/// moment it resolves, or a fixed test stays skipped forever
/// (`manager/src/services/jira.rs:220-229` filters `status = 'Open'`).
#[test]
fn a_resolved_bug_leaves_the_skip_list() {
    let bugs = vec![open("test_a", "V-1"), resolved("test_b", "V-2")];
    assert_eq!(skip_list_entries(&bugs).len(), 1);
}

/// Strengthens the above: the survivor must actually be the *open* bug, not
/// merely "some" bug. A predicate inverted to `status == "Resolved"` would also
/// leave exactly one entry and pass the length-only assertion above without
/// this one.
#[test]
fn the_surviving_entry_is_the_open_one_not_the_resolved_one() {
    let bugs = vec![open("test_a", "V-1"), resolved("test_b", "V-2")];
    let entries = skip_list_entries(&bugs);
    assert_eq!(entries[0].test_name, "test_a");
    assert_eq!(entries[0].jira_key, "V-1");
}

/// Keys on `status`, not on `resolved_at`: a bug whose `status` is `"Open"`
/// still suppresses its test even with `resolved_at` (anomalously) set, and a
/// bug whose `status` is anything other than `"Open"` is excluded even with
/// `resolved_at` unset. This is what makes the two fields able to disagree, per
/// this module's header — a predicate written as `resolved_at.is_none()` would
/// get both halves of this test backwards.
#[test]
fn the_predicate_keys_on_status_not_on_resolved_at() {
    let mut anomalous_open = open("test_a", "V-1");
    anomalous_open.resolved_at = Some(OffsetDateTime::UNIX_EPOCH);

    let anomalous_closed = make_bug("test_b", "V-2", "Closed");

    let entries = skip_list_entries(&[anomalous_open, anomalous_closed]);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].test_name, "test_a");
}

/// An empty slice renders the empty string. Callers decide whether an empty
/// result means "omit the environment variable" (`qa_insights_sdk::SkipListEntry`'s
/// doc, and Task 34's job) — this module just renders what it is given.
#[test]
fn an_empty_list_renders_the_empty_string() {
    assert_eq!(render_skip_list(&[]), "");
}

/// The dual of [`a_resolved_bug_leaves_the_skip_list`]: an all-open input keeps
/// every entry, in order.
#[test]
fn all_open_bugs_all_survive_in_order() {
    let bugs = vec![open("test_a", "V-1"), open("test_b", "V-2")];
    let entries = skip_list_entries(&bugs);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].test_name, "test_a");
    assert_eq!(entries[1].test_name, "test_b");
}
