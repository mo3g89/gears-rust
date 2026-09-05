//! In-process `QaInsightsClientV1`, registered in `ClientHub` at gear init.
//!
//! The one seam another gear reaches this one through — qa-runs' launch path
//! is the sole named consumer (`qa_insights_sdk::client::QaInsightsClientV1`'s
//! own doc). It lives under `domain` rather than `api` because it speaks SDK
//! models and `SecurityContext`, with no transport of its own — the same
//! placement `qa-runs/src/domain/local_client` uses for its own seam.
//!
//! **Registered by Task 40**, in `gear::QaInsights::init`, alongside the three
//! tickers — the wiring R70 reserved for that task. This module builds the
//! type; the composition root decides that a deployment exposes it.
//!
//! **Registered and unserved, deliberately.** R74: no task in this plan wires
//! qa-runs' launch path to *call* `skip_list_for`, so `SKIP_TESTS_WITH_BUGS`
//! stays reserved-with-no-producer in `qa-runs/src/domain/params.rs:101`. That is
//! a recorded release-gate item, not a gap in this module or in the
//! registration.

mod client;

pub use client::QaInsightsLocalClient;
