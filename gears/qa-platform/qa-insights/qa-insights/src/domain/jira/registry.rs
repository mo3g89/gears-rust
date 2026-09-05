//! The frozen skip-list wire format, and the open-bug filter that feeds it.
//!
//! Two pure functions, composed in the order legacy composes them
//! (`manager/src/routes/runs.rs:750-761`): [`skip_list_entries`] narrows a
//! tenant's bug rows down to the open ones and pairs each with its test name,
//! and [`render_skip_list`] joins those pairs into the exact string legacy
//! writes to the `SKIP_TESTS_WITH_BUGS` environment variable
//! (`manager/src/services/argo.rs:476-481`).
//!
//! # The open predicate is `status == "Open"`, not `resolved_at.is_none()`
//!
//! Legacy's `get_open_bugs`/`get_all_open_bugs`
//! (`manager/src/services/jira.rs:220-241`) filter in SQL with
//! `status = 'Open'` — a literal string match. `resolve_bug` (`jira.rs:243-251`)
//! writes `status = 'Resolved'` *and* `resolved_at = NOW()` together, but
//! `status` is free JIRA workflow text (`qa_insights_sdk::JiraBug::status`'s own
//! doc says as much, citing `check_jira_status`, `jira.rs:254`), so an instance
//! could in principle report a value the poller has not yet turned into
//! `'Resolved'` while `resolved_at` stays unset — or vice versa, if a caller
//! ever set one without the other. Legacy keys on `status` alone, so
//! [`skip_list_entries`] does too.
//!
//! # The wire format is frozen (`cpt-cf-qa-fr-migration-runner-contract`)
//!
//! `bugs.iter().map(|b| format!("{}:{}", b.test_name, b.jira_key)).join(",")`
//! (`manager/src/routes/runs.rs:755-758`): a colon between name and key, a comma
//! between pairs, no surrounding whitespace, **no sort and no dedup** — the
//! order is whatever the caller's bug list arrived in (legacy's own query
//! carries no `ORDER BY`, `jira.rs:220-229`), and two open bugs against the same
//! test name both appear, unmerged.
//!
//! # An empty list is not this module's problem
//!
//! Legacy turns zero rows into an *absent* environment variable, not an empty
//! string: the launch path only sets `skip_tests` at all when
//! `Ok(bugs) if !bugs.is_empty()` (`runs.rs:753-761`), and the env-var push is
//! separately gated on `!skip.is_empty()` (`argo.rs:477-479`).
//! `qa_insights_sdk::SkipListEntry`'s doc already states this as the contract
//! and assigns the `None`-when-empty decision to whoever renders the
//! environment variable — this module only renders whatever pairs it is given,
//! empty or not.
//!
//! **That renderer is qa-runs' launch path, not Task 34** (Phase C's final
//! review, Important 3; this paragraph named Task 34). Task 34's SDK provider,
//! `domain::local_client::client`'s `skip_list_for`, calls [`skip_list_entries`]
//! and hands back `Vec<SkipListEntry>`; it never calls [`render_skip_list`], and
//! a repo-wide search finds no caller of that function outside
//! `registry_tests`. See `domain::jira`'s own header for why it ships ahead of
//! its producer (R74) rather than being removed.

use qa_insights_sdk::{JiraBug, SkipListEntry};

/// Legacy's open-bug predicate, ported: `status == "Open"`
/// (`manager/src/services/jira.rs:220-241`), not `resolved_at.is_none()`. See
/// this module's header for why the two fields can disagree.
const STATUS_OPEN: &str = "Open";

/// Narrow a tenant's bug rows to the open ones, as `(test_name, jira_key)`
/// pairs.
///
/// Order is preserved from `bugs`, and nothing here deduplicates: two open bugs
/// against the same test both survive. See this module's header for the
/// `status`-not-`resolved_at` predicate and the no-sort/no-dedup rule, both
/// ported from legacy.
#[must_use]
pub fn skip_list_entries(bugs: &[JiraBug]) -> Vec<SkipListEntry> {
    bugs.iter()
        .filter(|bug| bug.status == STATUS_OPEN)
        .map(|bug| SkipListEntry {
            test_name: bug.test_name.clone(),
            jira_key: bug.jira_key.clone(),
        })
        .collect()
}

/// Render the frozen `SKIP_TESTS_WITH_BUGS` wire format.
///
/// `manager/src/routes/runs.rs:755-758`: `test_name:JIRA-KEY` pairs joined by
/// `,`. No sort, no dedup, no surrounding whitespace — ported exactly, because a
/// spacing or ordering change is a contract break for every test repository
/// that reads the variable (`cpt-cf-qa-fr-migration-runner-contract`).
#[must_use]
pub fn render_skip_list(entries: &[SkipListEntry]) -> String {
    entries
        .iter()
        .map(|entry| format!("{}:{}", entry.test_name, entry.jira_key))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
