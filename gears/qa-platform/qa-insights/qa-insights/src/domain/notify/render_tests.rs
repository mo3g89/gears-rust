//! Tests for the notification rendering core (Task 37 brief, Step 0/1).
//!
//! # R97: the brief's `the_preview_renders_exactly_what_the_send_renders`
//!
//! The brief pins `render_scheduled_run(&ctx).rendered_message ==
//! preview_scheduled_run(&ctx)`, citing legacy's two call sites
//! (`notifications.rs:388` and `:431`) as evidence the paths can drift. But
//! both legacy call sites already route through **one** private function,
//! `build_scheduled_run_slack_message` (`:862-878`) — they were never two
//! renderers. This port keeps that shape explicitly:
//! [`super::preview_scheduled_run`] calls [`super::render_scheduled_run`] and
//! nothing else (see `render.rs`'s doc comment on
//! [`super::preview_scheduled_run`]). Under that design the brief's assertion
//! cannot fail — there is only one function's output to compare against
//! itself — so shipping it as a test would assert nothing. Per R97 it is
//! replaced below by
//! [`every_section_status_icon_and_fallback_survive_the_shared_renderer`],
//! which exercises the property that actually has content: every section,
//! `status_icon`, and the fallback text all come through non-trivially for
//! every one of the six tokens.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qa_insights_sdk::{NotificationConfig, ScheduledRunSlackTemplate, ScheduledRunSlackTemplates};
use qa_runs_sdk::SLACK_NOTIFICATION_EVENTS;

use super::{
    RenderedScheduledRunMessage, RunCompletedRenderContext, ScheduledRunRenderContext,
    preview_scheduled_run, render_run_completed, render_scheduled_run,
};

/// A context with every optional field populated, so a template exercising
/// every placeholder has something non-default to show. Mirrors legacy's
/// `sample_run_for_event` (`notifications.rs:1503-1575`) without the
/// per-event variation — tests that care about one field vary it themselves.
fn full_context() -> ScheduledRunRenderContext {
    ScheduledRunRenderContext {
        run_name: "nightly-smoke-142".to_owned(),
        plan_id: "vhp/smoke".to_owned(),
        phase: "Failed".to_owned(),
        run_source: "scheduled".to_owned(),
        platform: Some("vp-nightly-1".to_owned()),
        product_key: Some("QA".to_owned()),
        app_version: Some("8.0.1".to_owned()),
        app_build: Some("1024".to_owned()),
        test_version: Some("main".to_owned()),
        schedule_id: Some("nightly".to_owned()),
        repo_name: Some("qa-e2e".to_owned()),
        source_ref: Some("main".to_owned()),
        source_ref_kind: Some("branch".to_owned()),
        started_at: Some("2026-04-07T01:01:00Z".to_owned()),
        finished_at: Some("2026-04-07T01:11:02Z".to_owned()),
        duration: Some("10m 2s".to_owned()),
        message: Some("2 test failures detected".to_owned()),
        result_statuses: vec![
            "PASSED".to_owned(),
            "FAILED".to_owned(),
            "PASSED".to_owned(),
            "ERROR".to_owned(),
            "SKIPPED".to_owned(),
        ],
    }
}

/// A context with every optional field absent — the minimum a caller could
/// supply — used to pin `None`-section and default-fallback behaviour.
fn minimal_context() -> ScheduledRunRenderContext {
    ScheduledRunRenderContext {
        run_name: "bare-run".to_owned(),
        plan_id: "plan/bare".to_owned(),
        phase: "Pending".to_owned(),
        run_source: "scheduled".to_owned(),
        result_statuses: Vec::new(),
        ..ScheduledRunRenderContext::default()
    }
}

fn enabled_template() -> ScheduledRunSlackTemplate {
    ScheduledRunSlackTemplate {
        enabled: true,
        ..ScheduledRunSlackTemplate::default()
    }
}

/// Every one of the six templates enabled, with default text — the state a
/// tenant is in immediately after turning scheduled-run Slack alerts on
/// without customizing any wording.
fn config_with_every_template_enabled() -> NotificationConfig {
    let template = enabled_template();
    NotificationConfig {
        scheduled_run_slack_templates: ScheduledRunSlackTemplates {
            pending: template.clone(),
            in_progress: template.clone(),
            succeeded: template.clone(),
            failed: template.clone(),
            error: template.clone(),
            skipped: template,
        },
        ..NotificationConfig::default()
    }
}

fn render_ok(
    config: &NotificationConfig,
    token: &str,
    ctx: &ScheduledRunRenderContext,
) -> RenderedScheduledRunMessage {
    render_scheduled_run(config, token, ctx).unwrap_or_else(|| panic!("expected {token} to render"))
}

/// The brief's own test (Step 1), unmodified: blocks always carry a
/// non-empty fallback, because a Slack client that cannot render blocks
/// falls back to it, and an empty fallback shows the operator nothing at
/// all.
#[test]
fn every_block_message_carries_a_non_empty_fallback() {
    let config = config_with_every_template_enabled();
    let message = render_ok(&config, "failed", &full_context());

    assert!(!message.fallback_text.trim().is_empty());
    assert!(!message.blocks.is_empty());
}

/// R97's replacement for the brief's tautological preview/send test: the
/// property that has actual content is that every section, the `status_icon`
/// placeholder, and the fallback all render non-trivially, for every one of
/// the six tokens — not just the one the brief's example happened to use.
/// [`preview_scheduled_run`] is exercised too, since under this module's
/// design it is [`render_scheduled_run`] under a second name and this is the
/// only property left worth pinning about that relationship: it never
/// returns `None` when the send path wouldn't.
#[test]
fn every_section_status_icon_and_fallback_survive_the_shared_renderer() {
    let config = config_with_every_template_enabled();
    let ctx = full_context();

    for token in SLACK_NOTIFICATION_EVENTS {
        let sent = render_scheduled_run(&config, token, &ctx)
            .unwrap_or_else(|| panic!("{token} should render"));
        let previewed = preview_scheduled_run(&config, token, &ctx)
            .unwrap_or_else(|| panic!("{token} should preview"));
        assert_eq!(sent, previewed, "{token}: preview and send must agree");

        assert!(
            !sent.fallback_text.trim().is_empty(),
            "{token}: fallback must not be empty"
        );
        assert!(!sent.blocks.is_empty(), "{token}: blocks must not be empty");
        // The default header template is `{{status_icon}} \`{{run_name}}\` —
        // *{{status_headline}}*`; every token has a distinct default icon
        // (`status_defaults`), so it must show up in the header block.
        let header = sent.blocks[0]["text"]["text"]
            .as_str()
            .expect("header block has text");
        assert!(
            header.contains(':'),
            "{token}: status_icon (a `:name:` emoji) must appear in the header"
        );
        assert!(
            header.contains(&ctx.run_name),
            "{token}: header must name the run"
        );
    }
}

/// Each of the six tokens resolves to *its own* template — not, for example,
/// always the same one regardless of which token was asked for. Distinct,
/// recognizable text per token proves `template_for(token)` is actually
/// consulted per call rather than a fixed field being read every time.
#[test]
fn each_event_token_resolves_to_its_own_template() {
    let mut config = config_with_every_template_enabled();
    config.scheduled_run_slack_templates.pending.header = Some("PENDING-MARKER".to_owned());
    config.scheduled_run_slack_templates.in_progress.header = Some("IN-PROGRESS-MARKER".to_owned());
    config.scheduled_run_slack_templates.succeeded.header = Some("SUCCEEDED-MARKER".to_owned());
    config.scheduled_run_slack_templates.failed.header = Some("FAILED-MARKER".to_owned());
    config.scheduled_run_slack_templates.error.header = Some("ERROR-MARKER".to_owned());
    config.scheduled_run_slack_templates.skipped.header = Some("SKIPPED-MARKER".to_owned());

    let ctx = full_context();
    let expectations = [
        ("pending", "PENDING-MARKER"),
        ("in_progress", "IN-PROGRESS-MARKER"),
        ("succeeded", "SUCCEEDED-MARKER"),
        ("failed", "FAILED-MARKER"),
        ("error", "ERROR-MARKER"),
        ("skipped", "SKIPPED-MARKER"),
    ];
    for (token, marker) in expectations {
        let rendered = render_ok(&config, token, &ctx);
        let header = rendered.blocks[0]["text"]["text"].as_str().unwrap();
        assert_eq!(
            header, marker,
            "token {token} must resolve to its own template, not another one's"
        );
    }
}

/// A token outside `SLACK_NOTIFICATION_EVENTS` — including a case-folded
/// miss, Step 0's specific warning — renders nothing, mirroring
/// `ScheduledRunSlackTemplates::template_for`'s own partiality.
#[test]
fn an_unrecognized_token_renders_nothing() {
    let config = config_with_every_template_enabled();
    let ctx = full_context();

    assert!(render_scheduled_run(&config, "unknown", &ctx).is_none());
    assert!(render_scheduled_run(&config, "InProgress", &ctx).is_none());
}

/// A `None` section: the field it's gated on is absent, so the whole
/// conditional section around it disappears, rather than leaving an empty
/// placeholder behind. `message` is absent here and the default body
/// template is `{{#if message}}>{{message}}\n\n{{/if}}{{#if run_url}}...`,
/// so with no `manager_ui_base_url` either, the body renders as nothing at
/// all.
#[test]
fn a_none_section_disappears_rather_than_rendering_empty() {
    let config = config_with_every_template_enabled();
    let ctx = minimal_context();

    let rendered = render_ok(&config, "pending", &ctx);
    assert_eq!(
        rendered.rendered_message, "",
        "body has no message and no run_url, so it renders empty"
    );
    assert!(
        !rendered.rendered_message.contains("{{"),
        "no stray placeholder should leak through"
    );
}

/// An unknown placeholder — one the replacement table has no entry for — is
/// left in the output verbatim, because substitution is a plain
/// `.replace()` over a fixed table, not an error on an unrecognized token.
#[test]
fn an_unknown_placeholder_is_left_verbatim() {
    let mut config = config_with_every_template_enabled();
    config.scheduled_run_slack_templates.failed.footer =
        Some("{{not_a_real_placeholder}}".to_owned());

    let rendered = render_ok(&config, "failed", &full_context());
    let footer = rendered.blocks.last().unwrap()["elements"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(footer, "{{not_a_real_placeholder}}");
}

/// `manager_ui_base_url` becomes `<base>/runs/<percent-encoded run name>`,
/// with every trailing slash stripped (`trim_end_matches('/')` strips all of
/// them, not just one — legacy's own behaviour, ported as-is) and the run
/// name percent-encoded.
#[test]
fn manager_ui_base_url_builds_the_run_link_and_strips_every_trailing_slash() {
    let mut config = config_with_every_template_enabled();
    config.manager_ui_base_url = "https://qa.example.test///".to_owned();

    let mut ctx = full_context();
    ctx.run_name = "nightly smoke#142".to_owned();
    ctx.message = None;

    let rendered = render_ok(&config, "failed", &ctx);
    assert_eq!(
        rendered.rendered_message,
        "<https://qa.example.test/runs/nightly%20smoke%23142|Open run>"
    );
}

/// A blank `manager_ui_base_url` means there is nothing to link to: `run_url`
/// is `None`, and the default body template's `{{#if run_url}}` section
/// disappears rather than emitting an empty link.
#[test]
fn a_blank_manager_ui_base_url_produces_no_run_link() {
    let config = config_with_every_template_enabled();
    let mut ctx = full_context();
    ctx.message = None;

    let rendered = render_ok(&config, "failed", &ctx);
    assert_eq!(rendered.rendered_message, "");
}

/// A custom `status_icon` override reaches the header, exactly where legacy
/// puts it (`{{status_icon}}` inside the header template) — not a block of
/// its own.
#[test]
fn a_status_icon_override_reaches_the_header() {
    let mut config = config_with_every_template_enabled();
    config.scheduled_run_slack_templates.failed.status_icon = Some(":boom:".to_owned());

    let rendered = render_ok(&config, "failed", &full_context());
    let header = rendered.blocks[0]["text"]["text"].as_str().unwrap();
    assert!(header.contains(":boom:"));
}

/// A stored body template equal to the pre-`run_url` default is migrated to
/// the current default rather than kept verbatim — legacy's
/// `scheduled_run_template_body` migration shim (`notifications.rs:1444-1448`).
#[test]
fn the_old_default_body_template_is_migrated_to_the_current_one() {
    let mut config = config_with_every_template_enabled();
    config.scheduled_run_slack_templates.failed.body =
        Some("{{#if message}}>{{message}}{{/if}}".to_owned());
    config.manager_ui_base_url = "https://qa.example.test".to_owned();

    let mut ctx = full_context();
    ctx.message = None;

    let rendered = render_ok(&config, "failed", &ctx);
    assert!(
        rendered.rendered_message.contains("Open run"),
        "the migrated body should carry the run_url addition the old default lacked"
    );
}

/// `count_results` treats `ERROR` as a failure, exactly as legacy's does —
/// visible here through the `{{results_failed}}` placeholder in the default
/// `results` template.
#[test]
fn error_status_counts_as_a_failure_in_the_results_section() {
    let config = config_with_every_template_enabled();
    let ctx = full_context(); // 2 PASSED, 1 FAILED, 1 ERROR, 1 SKIPPED

    let rendered = render_ok(&config, "failed", &ctx);
    let results = rendered.blocks[2]["text"]["text"].as_str().unwrap();
    assert!(
        results.contains(":x: 2"),
        "FAILED and ERROR together must count as 2 failures, got: {results}"
    );
}

/// The fallback text never repeats the `results` section's own rendered
/// text (with its emoji and wording) — it appends the raw counts
/// independently. This is legacy's own choice (`render.rs`'s module doc,
/// "`fallback_text`"), verified here by asserting the results section's
/// distinctive marker is absent from the fallback even though it is present
/// in the blocks.
#[test]
fn fallback_text_never_repeats_the_results_sections_own_wording() {
    let mut config = config_with_every_template_enabled();
    config.scheduled_run_slack_templates.failed.results = Some("RESULTS-SECTION-MARKER".to_owned());

    let rendered = render_ok(&config, "failed", &full_context());
    let results_block = rendered.blocks[2]["text"]["text"].as_str().unwrap();
    assert_eq!(results_block, "RESULTS-SECTION-MARKER");
    assert!(
        !rendered.fallback_text.contains("RESULTS-SECTION-MARKER"),
        "fallback must not echo the results section's own text: {}",
        rendered.fallback_text
    );
    assert!(
        rendered
            .fallback_text
            .contains("passed 2, failed 2, skipped 1"),
        "fallback must still carry the raw counts independently: {}",
        rendered.fallback_text
    );
}

/// R98: legacy's generic completion alert derives its headline from the
/// result counts (`notifications.rs:191-192,222-228`) — `FAILED` beats
/// `SUCCEEDED` beats `COMPLETED`, and the same rendered `text` becomes both
/// the (non-templated) Slack message and the email body; only the subject
/// differs between the two channels.
#[test]
fn run_completed_email_headline_reflects_the_result_counts() {
    let mut ctx = RunCompletedRenderContext {
        run_name: "nightly-smoke-142".to_owned(),
        plan_id: "vhp/smoke".to_owned(),
        platform: Some("vp-nightly-1".to_owned()),
        product_key: Some("QA".to_owned()),
        result_statuses: vec!["PASSED".to_owned(), "FAILED".to_owned()],
    };
    let failed = render_run_completed(&ctx);
    assert!(failed.text.contains("FAILED"));
    assert!(failed.email_subject.contains("FAILED"));
    assert!(failed.email_subject.contains("nightly-smoke-142"));

    ctx.result_statuses = vec!["PASSED".to_owned(), "PASSED".to_owned()];
    let succeeded = render_run_completed(&ctx);
    assert!(succeeded.text.contains("SUCCEEDED"));

    ctx.result_statuses = Vec::new();
    let completed = render_run_completed(&ctx);
    assert!(completed.text.contains("COMPLETED"));
}

/// R99: the email subject keeps legacy's literal `"VHP test run"` branding
/// (`notifications.rs:328`: `format!("VHP test run {} {}", run.name,
/// headline)`) verbatim — an earlier draft of this renderer rebranded it to
/// `"QA run"` unilaterally, and the controller reverted that: rebranding is a
/// real but open product question spanning this string and
/// `infra::jira::oagw_client`'s `"[VHP] Test Failed: ..."` JIRA summary
/// (`oagw_client.rs:789`), not a call a rendering task gets to make alone.
/// The prior test only checked `.contains(...)` on both halves, which cannot
/// fail on a rebrand that keeps the run name and headline — this asserts the
/// exact string so a future rebrand attempt fails a test instead of shipping
/// silently.
#[test]
fn run_completed_email_subject_keeps_the_legacy_vhp_branding() {
    let ctx = RunCompletedRenderContext {
        run_name: "nightly-smoke-142".to_owned(),
        plan_id: "vhp/smoke".to_owned(),
        platform: Some("vp-nightly-1".to_owned()),
        product_key: Some("QA".to_owned()),
        result_statuses: vec!["PASSED".to_owned(), "FAILED".to_owned()],
    };
    let rendered = render_run_completed(&ctx);
    assert_eq!(
        rendered.email_subject,
        "VHP test run nightly-smoke-142 FAILED"
    );
}

/// R98: the rendered text carries the plan id and, when present, the
/// platform and product key, joined by `" · "`, plus the passed/failed/
/// skipped counts on their own line — legacy's `text` format
/// (`notifications.rs:230-245`).
#[test]
fn run_completed_text_carries_plan_platform_product_and_counts() {
    let ctx = RunCompletedRenderContext {
        run_name: "nightly-smoke-142".to_owned(),
        plan_id: "vhp/smoke".to_owned(),
        platform: Some("vp-nightly-1".to_owned()),
        product_key: Some("QA".to_owned()),
        result_statuses: vec![
            "PASSED".to_owned(),
            "FAILED".to_owned(),
            "SKIPPED".to_owned(),
        ],
    };
    let rendered = render_run_completed(&ctx);

    assert!(rendered.text.starts_with(
        "nightly-smoke-142 \u{2014} FAILED \u{b7} vhp/smoke \u{b7} vp-nightly-1 \u{b7} QA"
    ));
    assert!(rendered.text.contains("passed 1, failed 1, skipped 1"));
}

/// R98: an absent platform or product key is simply omitted from the joined
/// info clause, not rendered as a placeholder or a `"-"` filler — legacy only
/// ever pushes present values onto `info_parts` (`notifications.rs:230-236`).
#[test]
fn run_completed_text_omits_absent_platform_and_product_key() {
    let ctx = RunCompletedRenderContext {
        run_name: "bare-run".to_owned(),
        plan_id: "plan/bare".to_owned(),
        platform: None,
        product_key: None,
        result_statuses: vec!["PASSED".to_owned()],
    };
    let rendered = render_run_completed(&ctx);

    assert!(
        rendered
            .text
            .starts_with("bare-run \u{2014} SUCCEEDED \u{b7} plan/bare\n")
    );
}
