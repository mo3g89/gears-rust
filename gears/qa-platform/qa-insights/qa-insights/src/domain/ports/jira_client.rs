//! The outbound JIRA port — Task 32.
//!
//! Three methods, because legacy has three JIRA REST interactions and no more:
//! a find-or-create ([`JiraClient::create_or_find_issue`], legacy
//! `manager/src/services/jira.rs:33-193`), a status-category read
//! ([`JiraClient::check_status`], `jira.rs:254-289`) and the single-issue read
//! that status read is a projection of ([`JiraClient::get_issue`]).
//!
//! # The config crosses the port; the credential does not
//!
//! Every method takes the tenant's [`JiraConfig`] because the JIRA instance is
//! per-tenant data, not deployment config: legacy re-reads it inside every one
//! of the three calls (`jira.rs:58-63`, `:258-262`), and the equivalent here is
//! the service reading it once and handing it down. What the config carries is
//! [`JiraConfig::api_token_credstore_ref`] — a **reference**. No method on this
//! port can be handed token material, which is the property that makes a
//! credential-disclosure bug in this direction a type error rather than a code
//! review.
//!
//! # Nothing here touches this gear's own database
//!
//! Legacy's `create_or_find_issue` interleaves three things: a local
//! `jira_bugs` dedupe query (`jira.rs:41-56`), the JIRA-side search and create,
//! and a local `track_bug` insert (`jira.rs:178-187`). Only the middle one is
//! this port's. The local registry is [`crate::domain::repos::JiraRepository`]'s
//! and the orchestration between them is Task 33's — a port that reached into
//! the gear's own tables would make the adapter untestable without a database
//! and would put the dedupe rule in the layer that speaks HTTP.

use async_trait::async_trait;
use qa_insights_sdk::JiraConfig;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;

/// The optional scheme prefix a credential-store reference may carry.
///
/// oagw's apikey auth plugin strips exactly this before handing the remainder to
/// `SecretRef::new` (`gears/system/oagw/oagw/src/infra/plugin/apikey_auth.rs:42-47`),
/// so both `cred://name` and a bare `name` are legal on the wire and mean the
/// same secret.
pub const CREDSTORE_REF_SCHEME: &str = "cred://";

/// `SecretRef`'s own length ceiling
/// (`gears/credstore/credstore-sdk/src/models.rs:59-63`).
pub const MAX_CREDSTORE_REF_LEN: usize = 255;

/// Refuse a credential-store reference oagw's credential plugin could not
/// resolve.
///
/// `field` is the name the [`DomainError::Validation`] carries, so the caller
/// sees the column they actually posted. It is a parameter rather than a
/// constant because two unrelated surfaces store a reference:
/// `qa_jira_config.api_token_credstore_ref` (this port's own) and
/// `qa_notification_config.slack_webhook_credstore_ref` (Phase C's final
/// review, Important 1 — see [`crate::domain::service::notify`]). The *rule* is
/// one rule, so it is one function; the field name is the only thing that
/// differs, and a second copy of the charset check is exactly what would drift.
///
/// # Why this is validated in this gear at all
///
/// `qa_jira_config.api_token_credstore_ref` is copied verbatim into the oagw
/// upstream's apikey auth config, where it reaches `SecretRef::new` after only a
/// `cred://` prefix strip (`apikey_auth.rs:42-47`). **`SecretRef` accepts
/// `[a-zA-Z0-9_-]` and nothing else, up to 255 bytes, and colons are prohibited
/// outright "to prevent `ExternalID` collisions in backend storage"**
/// (`gears/credstore/credstore-sdk/src/models.rs:42-83`). A reference that
/// violates that — `credstore://qa/jira/api-token`, say, which is the shape a
/// URL-flavoured guess produces — is accepted by the `PUT`, stored, and then
/// fails as `PluginError::Internal("invalid secret ref ...")` inside oagw at
/// *request* time, uncorrelated with the save that caused it.
///
/// So the syntax is checked where the operator can act on it, exactly as
/// `infra::jira::oagw_client::egress_for` checks the URL. **Duplicated
/// nowhere**, across four call sites now:
/// `domain::service::jira::JiraService::save_jira_config` calls this on the way
/// in and `infra::jira::OagwJiraClient` calls it again before it provisions, so a
/// row stored before this rule existed also fails with a named field rather than
/// as an opaque gateway error; and since Phase C's final review
/// `domain::service::notify::NotifyService::save_config` and that service's
/// `send_test` override arm call it for the Slack webhook reference, which had
/// no check at all and whose four doc claims about "never a URL" it is what
/// makes true.
///
/// **The `credstore://` spelling is not valid syntax and never was.** It carries
/// a colon and slashes, both of which this function rejects; it is refused here
/// rather than quietly stripped, because a reference oagw would resolve to
/// nothing is better refused at the `PUT` than at request time. See
/// [`CREDSTORE_REF_SCHEME`] for the one prefix that is legal.
///
/// # Errors
///
/// [`DomainError::Validation`] naming `field`. Empty is refused here; each
/// caller decides what an empty *input* means before it reaches this function —
/// the JIRA `PUT` treats it as "keep the stored reference" and refuses an empty
/// **effective** reference only when the integration is enabled, while the
/// notification `PUT` treats it as "clear the reference" and does not call this
/// at all for it. See those methods' docs.
pub fn validate_credstore_ref(field: &str, reference: &str) -> Result<(), DomainError> {
    let invalid = |why: &str| DomainError::Validation {
        field: field.to_owned(),
        // "the credential store" rather than "oagw": this function had one
        // caller when it was written (the JIRA token, which oagw resolves and
        // injects) and now has three, one of which is
        // `email_smtp_credstore_ref` -- resolved by qa-insights itself against
        // `credstore_sdk`, because SMTP cannot traverse an HTTP proxy
        // (ADR-0011). The *rule* was always `SecretRef`'s own syntax and never
        // anything of oagw's, so only the wording was ever wrong.
        message: format!(
            "the credential-store reference is not one the credential store can resolve: {why}"
        ),
    };

    let name = reference
        .strip_prefix(CREDSTORE_REF_SCHEME)
        .unwrap_or(reference);
    if name.is_empty() {
        return Err(invalid("it must not be empty"));
    }
    if name.len() > MAX_CREDSTORE_REF_LEN {
        return Err(invalid(
            "it must not exceed 255 characters after any cred:// prefix",
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(invalid(
            "it must be letters, digits, underscores and dashes only, optionally prefixed with \
             cred:// - slashes and colons in particular are rejected by the credential store",
        ));
    }
    Ok(())
}

/// The `statusCategory.key` JIRA reports for an issue.
///
/// **A category, not a status.** Legacy's `check_jira_status` reads
/// `fields.status.statusCategory.key` and nothing else
/// (`manager/src/services/jira.rs:279-286`), and the poller compares it to the
/// literal `"done"` (`manager/src/services/jira_poller.rs:57`). The workflow
/// *status* — "Resolved", "Closed", "Won't Do", whatever an instance's admin
/// named it — is free text this gear never branches on; the category is JIRA's
/// own three-valued reduction of it (`new`, `indeterminate`, `done`).
///
/// A newtype over `String` rather than an enum, deliberately: the value comes
/// off a JSON document produced by a JIRA instance this gear does not control,
/// and an enum would have to decide what to do with a fourth value. Legacy
/// answers that question by substituting the literal `"unknown"` when the field
/// is missing or not a string (`jira.rs:285`) and by treating everything that is
/// not `"done"` as "still open" (`jira_poller.rs:80-82`), so an unrecognised
/// category is already handled: it is not resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusCategory(String);

impl StatusCategory {
    /// The one category that means resolved (`jira_poller.rs:57`).
    pub const DONE: &'static str = "done";

    /// What legacy substitutes when the category is absent or not a string
    /// (`jira.rs:285`).
    pub const UNKNOWN: &'static str = "unknown";

    /// Wrap a category key as JIRA reported it. No normalisation: legacy
    /// compares the raw value, so folding case here would make this gear resolve
    /// bugs legacy leaves open.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// [`Self::UNKNOWN`], for the response shape legacy's `unwrap_or` covers.
    #[must_use]
    pub fn unknown() -> Self {
        Self::new(Self::UNKNOWN)
    }

    /// The category key verbatim.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this category is the resolved signal.
    ///
    /// The whole reason the poller reads a category at all: `jira_poller.rs:57`
    /// is `Ok(status_category) if status_category == "done"`, and every other
    /// value falls into the `Ok(_)` arm that does nothing (`:80-82`).
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        self.0 == Self::DONE
    }
}

/// The semantic content of one bug filing — no JIRA wire vocabulary.
///
/// Legacy's `create_or_find_issue` parameter list verbatim, minus the two
/// arguments that are not about the issue: it takes
/// `(test_name, plan_id, app_version, platform, run_name, logs)`
/// (`manager/src/services/jira.rs:33-40`). The summary text, the
/// `vhp-test-failure` label, the Atlassian Document Format body and the 2000-character
/// log truncation are all the adapter's — they are JIRA's document model, and a
/// domain type that carried them would be a wire type wearing a domain name.
///
/// `plan` is a display string rather than a
/// [`PlanRef`](crate::domain::analytics::PlanRef): it is rendered into prose in
/// the issue description and never matched on, so the pair would only have to be
/// flattened again inside the adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewIssue {
    /// The failing test. Both the summary and the JQL dedupe probe key on it
    /// (`jira.rs:70-74`, `:113`).
    pub test_name: String,
    /// The plan the test belongs to, as prose for the description.
    pub plan: String,
    /// `None` renders as legacy's `"unknown"` (`jira.rs:130`).
    pub app_version: Option<String>,
    /// `None` renders as legacy's `"default"` (`jira.rs:129`).
    pub platform: Option<String>,
    /// The run the failure was observed in.
    pub run_name: String,
    /// Failure output. The adapter truncates it; see [`NewIssue`]'s own doc for
    /// why the limit is not stated here.
    pub logs: String,
}

/// What a find-or-create settled on.
///
/// Legacy's `JiraCreateResponse` (`manager/src/models.rs`), and `created` is
/// load-bearing rather than informational: legacy returns `created: false` for
/// **both** dedupe paths — the local registry hit (`jira.rs:49-56`) and the
/// JIRA-side JQL hit (`:103-104`) — and `true` only for the issue it actually
/// posted (`:189-192`). Only the second of those two `false` paths is this
/// port's; the first belongs to the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueRef {
    /// The JIRA issue key, e.g. `VHP-319`.
    pub jira_key: String,
    /// `true` when this call posted the issue, `false` when it found one.
    pub created: bool,
}

/// One JIRA issue, as much of it as this gear reads.
///
/// Legacy never models this — `check_jira_status` projects the same `GET` down
/// to one string and discards the rest (`jira.rs:279-287`). The type exists
/// because the brief's port has a `get_issue`, and it carries exactly the two
/// fields the response body is read for plus the key the caller asked about, so
/// a later consumer does not have to reopen the adapter to surface a summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JiraIssue {
    pub key: String,
    /// `fields.summary`, or empty when absent — absence is not an error here for
    /// [`StatusCategory::unknown`]'s reason.
    pub summary: String,
    pub status_category: StatusCategory,
}

/// The outbound JIRA calls, behind [`crate::infra::jira::OagwJiraClient`] in
/// production.
///
/// # Errors
///
/// Every method returns [`DomainError::Internal`] for a transport or gateway
/// failure and [`DomainError::JiraNotConfigured`] when the config it is handed
/// is disabled — the second is a refusal, not a failure, and legacy makes the
/// same one (`jira.rs:65-67`, `"JIRA integration is disabled"`).
#[async_trait]
pub trait JiraClient: Send + Sync {
    /// Find an open issue for this test, or file one.
    ///
    /// Legacy `create_or_find_issue` (`manager/src/services/jira.rs:33-193`),
    /// minus its two local-database steps. Two JIRA calls at most, in this
    /// order:
    ///
    /// 1. `GET /rest/api/3/search?jql=…&maxResults=1` with
    ///    `project = {key} AND labels = "vhp-test-failure" AND summary ~ "{test}" AND status != Done`
    ///    (`jira.rs:70-82`). **A failed or non-2xx search is not an error** —
    ///    legacy guards the whole block with `if search_resp.status().is_success()`
    ///    and falls through to the create (`:86-108`), so a broken search
    ///    duplicates an issue rather than dropping a bug report.
    /// 2. `POST /rest/api/3/issue` (`jira.rs:144-162`). A non-2xx here **is** an
    ///    error (`:164-168`).
    async fn create_or_find_issue(
        &self,
        ctx: &SecurityContext,
        config: &JiraConfig,
        issue: NewIssue,
    ) -> Result<IssueRef, DomainError>;

    /// The issue's status category.
    ///
    /// Legacy `check_jira_status` (`jira.rs:254-289`), which returns the
    /// `statusCategory.key` and not the status. The poller's resolved test is
    /// [`StatusCategory::is_resolved`].
    async fn check_status(
        &self,
        ctx: &SecurityContext,
        config: &JiraConfig,
        jira_key: &str,
    ) -> Result<StatusCategory, DomainError>;

    /// The issue behind [`Self::check_status`]' one field.
    ///
    /// Same `GET /rest/api/3/issue/{key}` (`jira.rs:261-273`); this returns the
    /// summary alongside the category rather than discarding it.
    async fn get_issue(
        &self,
        ctx: &SecurityContext,
        config: &JiraConfig,
        jira_key: &str,
    ) -> Result<JiraIssue, DomainError>;
}
