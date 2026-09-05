//! The JIRA bug registry's pure core: bug rows in, open-bug views and the
//! frozen skip-list wire format out.
//!
//! [`registry`] — Task 31. No repository, no HTTP, no `async`: those arrive at
//! Tasks 34-35, which read this module's functions rather than re-deriving
//! them — Task 34's SDK skip-list provider is `skip_list_entries`' first
//! caller.
//!
//! **`render_skip_list` still has none, and this header used to claim Task 34
//! was it** (Phase C's final review, Important 3). It is not:
//! `domain::local_client::client`'s `skip_list_for` calls `skip_list_entries`
//! and returns `Vec<SkipListEntry>`, leaving the rendering to the consumer —
//! which is qa-runs' launch path. That consumer doesn't exist yet either:
//! `RunVarInputs` is the field that would carry `SKIP_TESTS_WITH_BUGS` into a
//! run's environment, as its own doc records
//! (`qa-runs/src/domain/runvars.rs`), but nothing populates it yet —
//! `SKIP_TESTS_WITH_BUGS` is one of the "four things the source system sets
//! and this cannot yet", because this gear has no config surface for them
//! (`qa-runs/src/domain/service/dispatch_spec.rs:483`). That producer's
//! absence is a recorded release-gate item (R74) rather than dead code:
//! the function is the frozen wire format an external contract pins
//! (`cpt-cf-qa-fr-migration-runner-contract`), so it ships written and tested
//! ahead of its caller deliberately.
//!
//! **Task 33 is not one of them either**: its two
//! endpoints (list the raw registry, file a bug) render `qa_insights_sdk::JiraBug`
//! rows directly and never touch the frozen `SKIP_TESTS_WITH_BUGS` wire format
//! this module owns, so `domain::service::jira` calls
//! `crate::domain::repos::JiraRepository` and
//! `crate::domain::ports::JiraClient` directly rather than through this
//! module. The persistence side — `JiraRepository`
//! (`crate::domain::repos::JiraRepository`) and its `OrmJiraRepository`
//! implementation — already exists; this module is the pure layer above it that
//! had not been written yet.

pub mod registry;
