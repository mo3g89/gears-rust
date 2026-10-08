//! In-process `QaInsightsClientV1`, registered in `ClientHub` at gear init.
//!
//! The one seam another gear reaches this one through — qa-runs' launch path
//! is the sole named consumer (`qa_insights_sdk::client::QaInsightsClientV1`'s
//! own doc). It lives under `domain` rather than `api` because it speaks SDK
//! models and `SecurityContext`, with no transport of its own — the same
//! placement `qa-runs/src/domain/local_client` uses for its own seam.
//!
//! **Registered by Task 40**, in `gear::QaInsights::init`, alongside the three
//! tickers — the wiring that was reserved for that task. This module builds the
//! type; the composition root decides that a deployment exposes it.
//!
//! **Registered and unserved, deliberately.** Nothing in qa-runs' launch path
//! calls `skip_list_for` yet, so `SKIP_TESTS_WITH_BUGS` stays a reserved name
//! with no producer (`domain::jira`'s header, "No caller exists yet"). The
//! registration is complete; the caller is what is missing.

mod client;

pub use client::QaInsightsLocalClient;
