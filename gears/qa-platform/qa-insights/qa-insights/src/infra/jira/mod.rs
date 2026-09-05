//! The outbound JIRA adapter.
//!
//! One implementation of [`JiraClient`](crate::domain::ports::JiraClient), over
//! the Outbound API Gateway. See [`oagw_client`] for why there is no `reqwest`
//! here and never may be.

pub mod oagw_client;

pub use oagw_client::OagwJiraClient;
