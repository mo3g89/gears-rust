//! The Block Kit encoder — the one place in this crate that knows Slack's
//! wire shape for a message layout (review finding #17).
//!
//! # Why this is a module and not an `impl` on the domain type
//!
//! [`SlackBlock`] describes a notification's layout in this gear's own
//! vocabulary — a paragraph, a footer — and deliberately knows nothing about
//! Slack's JSON. Block Kit is Slack's format, so encoding into it is adapter
//! work, and this is the adapter side. Before finding #17 the encoding lived
//! in [`crate::domain::notify::render`] and the adapter passed the resulting
//! `serde_json::Value`s through untouched, which put the wire format in the
//! domain.
//!
//! # Two consumers, one encoder
//!
//! [`super::slack_oagw`] sends the encoded blocks to Slack. `api::rest::dto`'s
//! `NotificationPreviewDto` also encodes them, because
//! `POST /qa/v1/settings/notifications/preview`'s response contract *is* the
//! Block Kit body the tenant's Slack would receive — the preview endpoint
//! shows the wire payload, so it is a genuine second consumer of this encoder
//! rather than a layering slip. One encoder rather than two is what makes
//! "the preview shows what gets sent" a fact about the code and not a comment.
//! (`api::rest::routes::collections` reaching into `infra::storage::odata` is
//! the same direction, already established in this crate.)

use crate::domain::ports::SlackBlock;

/// Encode one block into Block Kit.
///
/// `section` and `context` are the only two shapes legacy's
/// `render_scheduled_run_slack_blocks`
/// (`manager/src/services/notifications.rs:906-947`) ever emitted, and
/// [`SlackBlock`]'s doc records why the enum has exactly those two variants.
fn encode(block: &SlackBlock) -> serde_json::Value {
    match block {
        SlackBlock::Section { text } => serde_json::json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": text }
        }),
        SlackBlock::Context { text } => serde_json::json!({
            "type": "context",
            "elements": [{ "type": "mrkdwn", "text": text }],
        }),
    }
}

/// Encode a whole message layout into the `blocks` array Slack expects.
pub fn encode_blocks(blocks: &[SlackBlock]) -> Vec<serde_json::Value> {
    blocks.iter().map(encode).collect()
}

#[cfg(test)]
#[path = "block_kit_tests.rs"]
mod tests;
