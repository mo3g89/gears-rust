//! Tests for the Block Kit encoder.
//!
//! Compared as [`serde_json::Value`], never as text: `serde_json/preserve_order`
//! is enabled workspace-wide (through `serde_toon_format`), so a string
//! comparison of serialized JSON passes per-package and fails under
//! `make test-no-macros`. `Value`'s map equality is order-independent under
//! both map backings.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

/// A `Section` is legacy's `section` block with one `mrkdwn` text.
#[test]
fn a_section_encodes_as_a_mrkdwn_section_block() {
    let encoded = encode_blocks(&[SlackBlock::Section {
        text: "*Failed*".to_owned(),
    }]);

    assert_eq!(
        encoded,
        vec![serde_json::json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": "*Failed*" }
        })]
    );
}

/// A `Context` is legacy's `context` block with exactly one element — not a
/// `text` field, which is the mistake an opaque `serde_json::Value` block
/// could make silently.
#[test]
fn a_context_encodes_as_a_context_block_with_one_element() {
    let encoded = encode_blocks(&[SlackBlock::Context {
        text: "nightly".to_owned(),
    }]);

    assert_eq!(
        encoded,
        vec![serde_json::json!({
            "type": "context",
            "elements": [{ "type": "mrkdwn", "text": "nightly" }]
        })]
    );
}

/// Order is preserved and nothing is dropped — the renderer's section order is
/// the reading order of the message.
#[test]
fn blocks_encode_in_order() {
    let encoded = encode_blocks(&[
        SlackBlock::Section {
            text: "one".to_owned(),
        },
        SlackBlock::Section {
            text: "two".to_owned(),
        },
        SlackBlock::Context {
            text: "three".to_owned(),
        },
    ]);

    assert_eq!(encoded.len(), 3);
    assert_eq!(encoded[0]["text"]["text"], "one");
    assert_eq!(encoded[1]["text"]["text"], "two");
    assert_eq!(encoded[2]["elements"][0]["text"], "three");
}

/// An empty layout encodes to an empty array, which
/// [`super::super::slack_oagw`] then omits from the payload entirely.
#[test]
fn no_blocks_encode_to_no_array_entries() {
    assert!(encode_blocks(&[]).is_empty());
}
