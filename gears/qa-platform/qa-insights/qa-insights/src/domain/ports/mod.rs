//! Outbound ports: the interfaces this gear's domain calls, with their
//! adapters in [`crate::infra`].
//!
//! Only the ports something already calls are declared. An unused trait is dead
//! code under this workspace's `-D warnings`, and — more to the point — a port
//! with no adapter and no caller is a guess about a shape rather than a
//! contract. The plan's Task 9 file list forecasts eight; each arrives with its
//! first call site.
//!
//! * [`runs_reader`] — the qa-runs reads (Task 13, extended by Task 15).
//! * [`catalog_reader`] — the analytics universe, read from qa-catalog (Task 20;
//!   its adapter, Task 25a).
//! * [`clock`] — today, as the analytics windows anchor on it (Task 22).
//! * [`environment_reader`] — an environment's display name for the
//!   `platform_id` a row carries, read from qa-environments (Task 25a, with
//!   its adapter; first called by Task 25b).
//! * [`runs_launcher`] — the one launch this gear performs against qa-runs, a
//!   collect-only run (Task 30; its adapter lands in the same commit, on the
//!   same struct as [`runs_reader`]'s).
//!
//! * [`jira_client`] — the three outbound JIRA calls (Task 32; its adapter,
//!   [`crate::infra::jira::OagwJiraClient`], lands in the same commit, and so
//!   does its first call site, [`crate::domain::service::jira::JiraService`]).
//! * [`slack_client`] and [`mail_client`] — the two egress ports Task 38's
//!   notification service sends through. **This paragraph used to forecast
//!   both for Task 39**, which was the plan's original file assignment; the
//!   controller's R102 moved the trait definitions (and [`SendOutcome`])
//!   here, to the task that has a caller for them
//!   ([`crate::domain::service::notify::NotifyService`]), and left Task 39
//!   the two adapters — the oagw-backed Slack client and the inert mail
//!   client D10 calls for. Same correction [`catalog_reader`]'s own header
//!   already recorded once for a different port: a plan's task-number
//!   forecast is not binding once a controller ruling moves the work.
//!
//! # [`catalog_reader`] was the first port here whose *first call site* was not
//! # in its own commit
//!
//! The rule above — "each arrives with its first call site" — was honoured in
//! substance and not in letter at Task 20: the analytics universe core is the
//! consumer, it consumes this port's return type
//! (`qa_catalog_sdk::UniverseTest`) rather than the port itself, and the port was
//! declared then because Task 20 is the task that read legacy's universe walk and
//! could state the contract.
//!
//! **The adapter was parked on Task 40 and that was never viable** — Task 25 is
//! the first real `list_universe` read, so it was blocked by its own
//! prerequisite. Task 25a shipped [`crate::infra::clients::qa_catalog`] and the
//! `ClientHub` lookup with it. [`catalog_reader`]'s header carries the retraction
//! in full, including why no `#[expect(dead_code)]` is possible on a `pub` port.
//!
//! [`clock`] is the second, and it is the *milder* case: its production adapter
//! ships in the same commit ([`crate::infra::clock::SystemClock`]) and its
//! contract is one method returning a date, so what is deferred is only the
//! service that reads it (Task 25's analytics service) and the wiring that
//! builds it (Task 40). The folds Task 22 shipped take the [`Date`](time::Date)
//! the port returns rather than the port itself — that module's header carries
//! the argument, and it is the shape `domain::service::dashboard` already uses.
//!
//! **[`environment_reader`] was the third, and for one task it was the most extreme
//! of the three** — recorded here because this section is the register of
//! exceptions and a register that omits the worst entry has stopped being one.
//! Task 25a shipped the port, its production adapter
//! ([`crate::infra::clients::qa_environments`]) and a test double, and **no
//! caller in any tier**: not the folds, not a service, not a test outside the
//! adapter's own. [`catalog_reader`] at least had a consumer of its *return
//! type*; that had neither. **Task 25b closed it in the next commit**:
//! [`crate::domain::service::analytics::AnalyticsService`] resolves the platform
//! names once per overview, and `gear::init` resolves the client.
//!
//! It was nevertheless not a guess about a shape, which is the rule's actual
//! purpose. The contract was fixed by a caller that already existed and could
//! not be written over without it:
//! [`aggregates::PlatformGroupSummary`](crate::domain::analytics::aggregates::PlatformGroupSummary)
//! carries a `Uuid` where legacy's chart draws a name, and Task 23 typed it that
//! way *deliberately* so no DTO could be written over it without deciding what
//! the label is. Task 25b's analytics service is that first call site, and it
//! brought the `ClientHub` lookup and the `deps` token with it in one commit —
//! `gear.rs`' `deps` doc and `Cargo.toml`'s `qa-environments-sdk` entry carry
//! what that obligation was and why nothing in the test suite could have caught
//! it: an unregistered client is a boot failure rather than a compile error.

pub mod catalog_reader;
pub mod clock;
pub mod environment_reader;
pub mod jira_client;
pub mod mail_client;
pub mod runs_launcher;
pub mod runs_reader;
pub mod slack_client;

pub use catalog_reader::CatalogReader;
pub use clock::Clock;
pub use environment_reader::EnvironmentReader;
pub use jira_client::{
    CREDSTORE_REF_SCHEME, IssueRef, JiraClient, JiraIssue, NewIssue, StatusCategory,
    validate_credstore_ref,
};
pub use mail_client::{MailClient, MailMessage};
pub use runs_launcher::RunsLauncher;
pub use runs_reader::RunsReader;
pub use slack_client::{SlackClient, SlackMessage};

/// What one egress attempt settled on — shared by [`SlackClient::send`] and
/// [`MailClient::send`] rather than declared per port, since the two ports'
/// callers ([`crate::domain::service::notify::NotifyService`]) fold both
/// results into the same audit-log write.
///
/// # This is the R102 seam
///
/// [`Self::UnsupportedEgress`] is a **value**, not an error: an adapter with
/// nothing to send through (Task 39's inert mail client, and this port's own
/// `mail_client` module doc) reports it rather than failing. Mapping this
/// variant to the audit log's `"unsupported_egress"` outcome string is
/// [`crate::domain::service::notify`]'s job and is tested there,
/// directly against the variant-to-string conversion and not only through
/// the end-to-end send path — no other task's tests cover that mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// The message was accepted by the far side.
    Sent,
    /// This deployment has no working adapter for the channel the message
    /// was addressed to.
    UnsupportedEgress,
}
