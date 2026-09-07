//! Turns a routed notification into the message that actually gets sent: a
//! sectioned Slack layout for [`Event::ScheduledRun`](super::routing::Event),
//! plain text and an email body for
//! [`Event::RunCompleted`](super::routing::Event) — Task 37.
//!
//! # Block Kit is not this module's format any more — review finding #17
//!
//! [`render_blocks`] used to build Slack's Block Kit JSON
//! (`serde_json::Value`s) here and hand it to the outbound adapter untouched,
//! which put Slack's wire format in the domain. It now returns
//! [`SlackBlock`]s and `infra::notify::block_kit` encodes them. The
//! `section`/`context` prose below therefore describes what each section
//! *becomes at the adapter*, which is unchanged: the encoded payload is
//! byte-for-byte what it was, pinned by
//! `infra::notify::slack_oagw`'s
//! `the_rendered_scheduled_run_payload_is_the_golden_block_kit_body`.
//!
//! Pure: no client, no repository, no `async`, matching `routing`'s own
//! purity (this module's sibling). [`routing::route`](super::routing::route)
//! answers *should this send*; this module answers *what does it say*, and
//! never re-derives a routing decision — `enabled`, `slack_events`,
//! `slack_enabled` and friends are not read here at all.
//!
//! # The five sections, `status_icon`, and how each reaches the message
//! (Step 0, brief citation `notifications.rs:31-44`)
//!
//! `qa_insights_sdk::ScheduledRunSlackTemplate` has **seven** fields, not the
//! brief's five: `enabled` (a routing concern, already spent by
//! [`routing::route`](super::routing::route)), `status_icon`, and the five
//! sections `header`, `summary`, `results`, `body`, `footer`. `status_icon`
//! does **not** get its own Slack block — legacy never gives it one either.
//! It reaches the message purely as the `status_icon` placeholder inside the
//! *header* section's default template (`default_header_template`,
//! `manager/src/models.rs:1013-1015`, which wraps the run name in Slack
//! code-span markup between the icon and an italicized headline), resolved
//! by [`status_icon_or_default`] as override-or-per-status-default exactly the
//! way `header`/`summary`/`results`/`footer` are (`scheduled_run_template_status_icon`,
//! `notifications.rs:1397-1407`). Each of the five sections becomes at most
//! one Slack block ([`render_blocks`]): `header`/`summary`/`results`/`body`
//! each a [`SlackBlock::Section`] if non-empty after rendering and trimming
//! (a `section` block once encoded), `footer` a [`SlackBlock::Context`] (a
//! `context` block). An empty (post-trim) section contributes no block at
//! all — a tenant can render a status with as few as zero blocks, and legacy
//! carries no floor, so this module doesn't invent one either.
//!
//! # `fallback_text` (`notifications.rs:31-35`, built at `:950-974`)
//!
//! Every Block Kit payload needs a plain-text fallback for a client that
//! cannot render blocks — an unrendered Slack message otherwise shows
//! nothing. Legacy's fallback is **not** "strip markup from every section":
//! it is header-or-run-name, then summary, then the **raw counts** formatted
//! independently of the `results` section's own text, then body, joined by a
//! middle-dot separator — the `results` section's rendered text (with its own emoji and
//! wording) never appears in the fallback at all. That is legacy's choice,
//! not a bug this port introduces, so [`render_fallback_text`] preserves it
//! verbatim. Markup is stripped per-part via `strip_mrkdwn`
//! (`notifications.rs:976-985`), including Slack's `<url|label>` link syntax
//! (`replace_slack_links`, `:987-1014`), turned into `"label (url)"`.
//!
//! # One renderer, not two — and what that does to the brief's first test
//! (R97)
//!
//! Legacy has two call sites that build a scheduled-run message:
//! `preview_scheduled_run_message` (`:388-404`) and `notify_scheduled_run_status`
//! (`:431-539`, the send path) — but both call the **same** private function,
//! `build_scheduled_run_slack_message` (`:862-878`). They were never two
//! renderers in the first place; the divergence risk the brief's test guards
//! against (two independent functions drifting) does not exist in legacy
//! either. [`preview_scheduled_run`] is [`render_scheduled_run`] under a
//! second name, calling it directly and nothing else — so the brief's
//! `the_preview_renders_exactly_what_the_send_renders` property cannot fail
//! under this design; see `render_tests.rs` for the replacement test and the
//! full explanation this controller ruling asked for.
//!
//! # Template placeholders and substitution (Step 0, `ScheduledRunSlackTemplatesConfig`)
//!
//! Each section template is first run through [`render_conditional`], which
//! evaluates `{{#if NAME}}...{{/if}}` (present/truthy gate, matched against a
//! fixed name list — see [`render_scheduled_run`]'s truthy-field table) and
//! `{{#if_event TOKEN}}...{{/if_event}}` (matches the current scheduled-run
//! token, `"running"` aliased to `"in_progress"` — `template_section_event_matches`,
//! `notifications.rs:1214-1219`), nesting freely. Ported from
//! `render_conditional_template_segment` (`:1112-1167`): **an open section with
//! no matching close before the template ends is left as raw, unevaluated
//! text** — that is legacy's own behaviour for a malformed template, not
//! something this port invents. After conditionals resolve, every
//! `{{token}}` still present is replaced by plain [`str::replace`] against a
//! fixed table ([`render_section`]) — **an unknown placeholder is left in the
//! output verbatim**, because `.replace()` only touches tokens it has a
//! mapping for; legacy never treats an unrecognized `{{...}}` as an error.
//! A `None` field (for example an absent `message`) is simply `false` in the
//! truthy table, so any `{{#if message}}...{{/if}}` section around it
//! disappears; every default template gates its optional placeholders this
//! way, so a `None` section renders as if the surrounding text were never
//! there, not as an empty substitution.
//!
//! # `ResultCounts` (`notifications.rs:23-29`) and how counts reach the message
//!
//! [`count_results`] folds a status list exactly as legacy's own function
//! does: `"PASSED"` → passed, `"FAILED"` **or** `"ERROR"` → failed,
//! `"SKIPPED"` → skipped, `results.len()` → total — verified against
//! `count_results` (`:1463-1479`) and its own test,
//! `count_results_treats_error_as_failure`. Counts reach the rendered message
//! two ways: as the `{{results_total}}`/`{{results_passed}}`/`{{results_failed}}`/
//! `{{results_skipped}}`/`{{results_summary}}` placeholders (used by the
//! default `results` template), and independently, as the raw
//! `"passed X, failed Y, skipped Z"` clause [`render_fallback_text`] appends
//! whenever `total > 0` — regardless of whether the `results` section itself
//! rendered anything.
//!
//! # `manager_ui_base_url` and run links (`notifications.rs:1070-1081`)
//!
//! [`run_url`] returns `None` when the base URL is blank (nothing to link
//! to), and otherwise trims exactly one trailing `/` off the base
//! (`trim_end_matches('/')`, which strips *every* trailing slash, not just
//! one — legacy's own choice, ported as-is) before appending
//! `/runs/{percent-encoded run name}`. The run name is percent-encoded by
//! [`urlencode_component`], ported verbatim from `:1057-1068`: every byte
//! that is not an ASCII alphanumeric or one of `- _ . ~` becomes `%XX`.
//!
//! # Email rendering (R98)
//!
//! Legacy's `notify_run_completed` (`:180-355`) builds one plain-text `text`
//! value (`:237-245`: run name, a headline word, plan/platform/product info,
//! then a `passed/failed/skipped` line) and sends it **verbatim as both** the
//! generic Slack alert (`:284-289`, no Block Kit — this is the
//! non-templated path, distinct from the six-token renderer above) and the
//! email body (`:328-329`); only the email `subject`
//! (`format!("VHP test run {} {}", run.name, headline)`, `:328`) differs
//! between the two channels. [`render_run_completed`] ports this — `text`
//! plays both roles, matching legacy — and, per **R99**, the subject keeps
//! legacy's literal `"VHP test run"` too. See [`render_run_completed`]'s own
//! doc for why. Every other word and the count/format logic
//! (`run_completed_headline`, ported from `:191-192,222-228`) is unchanged.
//!
//! D10 defers only the SMTP *send* to a later task, not the rendering this
//! task ships — see the plan's carried-items table
//! (`cpt-cf-qa-fr-insights-notifications`, "email deferred (D10)").

use qa_insights_sdk::{NotificationConfig, ScheduledRunSlackTemplate};

use crate::domain::ports::SlackBlock;

/// A status count fold over a list of result status strings. Ported from
/// legacy's `ResultCounts` (`notifications.rs:23-29`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ResultCounts {
    passed: usize,
    failed: usize,
    skipped: usize,
    total: usize,
}

/// Ported from `count_results` (`notifications.rs:1463-1479`): `"FAILED"`
/// and `"ERROR"` both count as failed, matching legacy's `matches!` arm.
fn count_results(statuses: &[String]) -> ResultCounts {
    ResultCounts {
        passed: statuses
            .iter()
            .filter(|status| status.as_str() == "PASSED")
            .count(),
        failed: statuses
            .iter()
            .filter(|status| matches!(status.as_str(), "FAILED" | "ERROR"))
            .count(),
        skipped: statuses
            .iter()
            .filter(|status| status.as_str() == "SKIPPED")
            .count(),
        total: statuses.len(),
    }
}

/// Everything [`render_scheduled_run`] needs about one run to fill in
/// placeholders, as flat, already-resolved data — this module never resolves
/// a repo or platform id to a display name, or a timestamp to display text;
/// that is whichever task assembles this context's job (Task 38), keeping
/// this renderer's dependency surface to plain strings only.
///
/// Field-for-field, this is legacy's `WorkflowRun` restricted to the fields
/// the scheduled-run template placeholders actually read (`notifications.rs:1221-1336`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduledRunRenderContext {
    pub run_name: String,
    pub plan_id: String,
    pub phase: String,
    pub run_source: String,
    pub platform: Option<String>,
    pub product_key: Option<String>,
    pub app_version: Option<String>,
    pub app_build: Option<String>,
    pub test_version: Option<String>,
    pub schedule_id: Option<String>,
    pub repo_name: Option<String>,
    pub source_ref: Option<String>,
    /// Truthy-gate only: legacy's own `truthy_fields` table carries this
    /// (`notifications.rs:1249-1252`) with no matching `{{source_ref_kind}}`
    /// replacement — a template can gate on `{{#if source_ref_kind}}` but
    /// cannot print its value. Preserved as-is, not an omission.
    pub source_ref_kind: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration: Option<String>,
    pub message: Option<String>,
    /// Raw result status strings (`"PASSED"`, `"FAILED"`, `"ERROR"`,
    /// `"SKIPPED"`, ...), folded by [`count_results`].
    pub result_statuses: Vec<String>,
}

/// The rendered output of one scheduled-run Slack message. Field-for-field
/// legacy's `ScheduledRunSlackMessage` (`notifications.rs:31-35`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedScheduledRunMessage {
    /// **Not** a concatenation of all five sections — legacy sets this to the
    /// rendered `body` section alone (`build_scheduled_run_slack_message`,
    /// `notifications.rs:874`), and this port preserves that exactly.
    pub rendered_message: String,
    pub fallback_text: String,
    /// The five sections that survived rendering, as [`SlackBlock`]s. Block
    /// Kit is the adapter's wire format, not this module's — review finding
    /// #17, argued in full on [`SlackBlock`]'s own doc.
    pub blocks: Vec<SlackBlock>,
}

struct RenderedSections {
    header: String,
    summary: String,
    results: String,
    body: String,
    footer: String,
}

/// The per-status defaults a template falls back to when a tenant has not
/// overridden a section. Ported from `ScheduledRunNotificationEvent::label`
/// and `default_status_icon` (`manager/src/models.rs:975-984,1018-1027`) and
/// `scheduled_run_status_headline` (`manager/src/services/notifications.rs:1022-1031`,
/// a distinct file from the other two — verified separately during Step 0's
/// citation sweep, since an earlier draft of this doc wrongly grouped all
/// three under `models.rs`). The five section defaults
/// (`header`/`summary`/`results`/`body`/`footer`) are identical across all six
/// statuses (verified by reading every match arm of each `default_*_template`
/// function, `models.rs:1013-1056`; only the icon and these two words vary),
/// so those five stay module-level constants instead of being threaded
/// through this struct.
struct StatusDefaults {
    /// `{{status}}` — Title Case, e.g. `"In progress"` (`label`,
    /// `models.rs:975-984`).
    label: &'static str,
    /// `{{status_headline}}` — short verb, e.g. `"Running"`
    /// (`scheduled_run_status_headline`, `notifications.rs:1022-1031`).
    headline: &'static str,
    default_icon: &'static str,
}

/// The six tokens' defaults, keyed by `qa_runs_sdk::SLACK_NOTIFICATION_EVENTS`'
/// spelling. `None` for anything outside that set — the same partiality as
/// [`qa_insights_sdk::ScheduledRunSlackTemplates::template_for`], which this
/// module always calls alongside this function so the two never disagree
/// about which tokens are valid.
fn status_defaults(token: &str) -> Option<StatusDefaults> {
    Some(match token {
        "pending" => StatusDefaults {
            label: "Pending",
            headline: "Queued",
            default_icon: ":hourglass_flowing_sand:",
        },
        "in_progress" => StatusDefaults {
            label: "In progress",
            headline: "Running",
            default_icon: ":large_blue_circle:",
        },
        "succeeded" => StatusDefaults {
            label: "Succeeded",
            headline: "Passed",
            default_icon: ":large_green_circle:",
        },
        "failed" => StatusDefaults {
            label: "Failed",
            headline: "Failed",
            default_icon: ":red_circle:",
        },
        "error" => StatusDefaults {
            label: "Error",
            headline: "Error",
            default_icon: ":warning:",
        },
        "skipped" => StatusDefaults {
            label: "Skipped",
            headline: "Skipped",
            default_icon: ":white_circle:",
        },
        _ => return None,
    })
}

const DEFAULT_HEADER_TEMPLATE: &str =
    "{{status_icon}} `{{run_name}}` \u{2014} *{{status_headline}}*";
const DEFAULT_SUMMARY_TEMPLATE: &str = "`{{plan_id}}`{{#if platform}}  \u{b7}  {{platform}}{{/if}}{{#if product_key}}  \u{b7}  {{product_key}}{{#if version_display}} {{version_display}}{{/if}}{{/if}}{{#if_event pending}}{{#if test_version}}  \u{b7}  {{test_version}}{{/if}}{{/if_event}}";
const DEFAULT_RESULTS_TEMPLATE: &str = "{{#if results_passed}}:white_check_mark: {{results_passed}}{{/if}}{{#if results_failed}}   :x: {{results_failed}}{{/if}}{{#if results_skipped}}   :fast_forward: {{results_skipped}}{{/if}}{{#if duration}}   :stopwatch: {{duration}}{{/if}}";
const DEFAULT_BODY_TEMPLATE: &str =
    "{{#if message}}>{{message}}\n\n{{/if}}{{#if run_url}}<{{run_url}}|Open run>{{/if}}";
const DEFAULT_FOOTER_TEMPLATE: &str = "{{#if schedule_id}}{{schedule_id}}{{/if}}{{#if repo_name}}  \u{b7}  {{repo_name}}{{#if source_ref}} @ {{source_ref}}{{/if}}{{/if}}{{#if started_at}}  \u{b7}  Started: {{started_at}}{{/if}}{{#if finished_at}}  \u{b7}  Finished: {{finished_at}}{{/if}}";

/// The *old* default body template, from before `run_url` support was added.
/// A stored template whose text still matches this verbatim is migrated to
/// the current default rather than kept — ported from `scheduled_run_template_body`'s
/// `legacy_default_body_template` comparison (`notifications.rs:1053-1055,1444-1448`).
const OLD_DEFAULT_BODY_TEMPLATE: &str = "{{#if message}}>{{message}}{{/if}}";

fn header_or_default(template: &ScheduledRunSlackTemplate) -> String {
    template
        .header
        .clone()
        .unwrap_or_else(|| DEFAULT_HEADER_TEMPLATE.to_owned())
}

fn summary_or_default(template: &ScheduledRunSlackTemplate) -> String {
    template
        .summary
        .clone()
        .unwrap_or_else(|| DEFAULT_SUMMARY_TEMPLATE.to_owned())
}

fn results_or_default(template: &ScheduledRunSlackTemplate) -> String {
    template
        .results
        .clone()
        .unwrap_or_else(|| DEFAULT_RESULTS_TEMPLATE.to_owned())
}

fn footer_or_default(template: &ScheduledRunSlackTemplate) -> String {
    template
        .footer
        .clone()
        .unwrap_or_else(|| DEFAULT_FOOTER_TEMPLATE.to_owned())
}

/// Override-or-default, plus the old-default migration (see
/// [`OLD_DEFAULT_BODY_TEMPLATE`]'s doc).
fn body_or_default(template: &ScheduledRunSlackTemplate) -> String {
    let body = template
        .body
        .clone()
        .unwrap_or_else(|| DEFAULT_BODY_TEMPLATE.to_owned());
    if body.trim() == OLD_DEFAULT_BODY_TEMPLATE {
        DEFAULT_BODY_TEMPLATE.to_owned()
    } else {
        body
    }
}

fn status_icon_or_default(template: &ScheduledRunSlackTemplate, default_icon: &str) -> String {
    template
        .status_icon
        .clone()
        .unwrap_or_else(|| default_icon.to_owned())
}

fn value_present(value: Option<&str>) -> bool {
    value
        .map(str::trim)
        .is_some_and(|candidate| !candidate.is_empty())
}

/// Ported from `display_value` (`notifications.rs:1481-1488`): the trimmed
/// value, or `"-"` for anything absent or blank.
fn display_value(value: Option<&str>) -> &str {
    let candidate = value.unwrap_or("-").trim();
    if candidate.is_empty() { "-" } else { candidate }
}

/// Ported from `format_version_value` (`notifications.rs:1034-1051`).
fn format_version_value(app_version: Option<&str>, app_build: Option<&str>) -> Option<String> {
    let version = app_version.map(str::trim).unwrap_or_default();
    let build = app_build.map(str::trim).unwrap_or_default();

    if !version.is_empty() && !build.is_empty() {
        Some(format!("{version} ({build})"))
    } else if !version.is_empty() {
        Some(version.to_owned())
    } else if !build.is_empty() {
        Some(build.to_owned())
    } else {
        None
    }
}

const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// Appends `%XX` (uppercase hex) for one byte, with no intermediate
/// allocation — equivalent to `format!("%{byte:02X}")` without triggering
/// `clippy::format_push_string`.
fn push_percent_encoded_byte(out: &mut String, byte: u8) {
    out.push('%');
    out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
    out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0F)]));
}

/// Ported verbatim from `urlencode_component` (`notifications.rs:1057-1068`).
fn urlencode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(*byte));
        } else {
            push_percent_encoded_byte(&mut out, *byte);
        }
    }
    out
}

/// Ported from `run_url` (`notifications.rs:1070-1081`). See this module's
/// header, "`manager_ui_base_url` and run links".
fn run_url(manager_ui_base_url: &str, run_name: &str) -> Option<String> {
    let base = manager_ui_base_url.trim();
    if base.is_empty() {
        return None;
    }
    Some(format!(
        "{}/runs/{}",
        base.trim_end_matches('/'),
        urlencode_component(run_name)
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    If,
    IfEvent,
}

enum Tag<'a> {
    Open {
        kind: SectionKind,
        argument: &'a str,
    },
    Close(SectionKind),
    Other,
}

/// Ported from `parse_template_section_tag` (`notifications.rs:1170-1196`).
fn parse_tag(contents: &str) -> Tag<'_> {
    if let Some(argument) = contents.strip_prefix("#if_event") {
        let normalized = argument.trim();
        if !normalized.is_empty() {
            return Tag::Open {
                kind: SectionKind::IfEvent,
                argument: normalized,
            };
        }
    }
    if let Some(argument) = contents.strip_prefix("#if") {
        let normalized = argument.trim();
        if !normalized.is_empty() {
            return Tag::Open {
                kind: SectionKind::If,
                argument: normalized,
            };
        }
    }
    match contents {
        "/if" => Tag::Close(SectionKind::If),
        "/if_event" => Tag::Close(SectionKind::IfEvent),
        _ => Tag::Other,
    }
}

/// Ported from `template_section_event_matches` (`notifications.rs:1214-1219`).
/// `token` is already one of `SLACK_NOTIFICATION_EVENTS`' canonical spellings
/// (validated before this module is ever called), so a plain equality after
/// lowercasing the argument reproduces legacy's enum re-parse without minting
/// a second closed-set matcher.
fn event_matches(argument: &str, token: &str) -> bool {
    match argument.trim().to_ascii_lowercase().as_str() {
        "running" => token == "in_progress",
        other => other == token,
    }
}

fn section_matches(
    kind: SectionKind,
    argument: &str,
    token: &str,
    truthy: &[(&str, bool)],
) -> bool {
    match kind {
        SectionKind::If => truthy
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case(argument.trim()) && *value),
        SectionKind::IfEvent => event_matches(argument, token),
    }
}

/// Ported from `render_conditional_template_sections`
/// (`notifications.rs:1099-1109`).
fn render_conditional(template: &str, token: &str, truthy: &[(&str, bool)]) -> String {
    let mut cursor = 0;
    render_segment(template, &mut cursor, None, token, truthy).0
}

/// Ported from `render_conditional_template_segment`
/// (`notifications.rs:1111-1167`). Recurses one level per open section;
/// `expected_close` is `None` only for the outermost call. Returns whether
/// the segment closed properly — see this module's header for what an
/// unmatched open section does.
fn render_segment(
    template: &str,
    cursor: &mut usize,
    expected_close: Option<SectionKind>,
    token: &str,
    truthy: &[(&str, bool)],
) -> (String, bool) {
    let mut rendered = String::new();

    while *cursor < template.len() {
        let Some(tag_start_offset) = template[*cursor..].find("{{") else {
            rendered.push_str(&template[*cursor..]);
            *cursor = template.len();
            break;
        };
        let tag_start = *cursor + tag_start_offset;
        rendered.push_str(&template[*cursor..tag_start]);

        let Some(tag_end_offset) = template[tag_start + 2..].find("}}") else {
            rendered.push_str(&template[tag_start..]);
            *cursor = template.len();
            break;
        };
        let tag_end = tag_start + 2 + tag_end_offset;
        let raw_tag = &template[tag_start..tag_end + 2];
        let tag_contents = template[tag_start + 2..tag_end].trim();
        *cursor = tag_end + 2;

        match parse_tag(tag_contents) {
            Tag::Open { kind, argument } => {
                let section_start = tag_start;
                let (section_rendered, closed) =
                    render_segment(template, cursor, Some(kind), token, truthy);
                if closed {
                    if section_matches(kind, argument, token, truthy) {
                        rendered.push_str(&section_rendered);
                    }
                } else {
                    rendered.push_str(&template[section_start..*cursor]);
                }
            }
            Tag::Close(kind) if Some(kind) == expected_close => return (rendered, true),
            Tag::Close(_) | Tag::Other => rendered.push_str(raw_tag),
        }
    }

    (rendered, expected_close.is_none())
}

/// Renders conditionals, then substitutes every known `{{token}}` via
/// [`str::replace`], then trims — ported from
/// `render_scheduled_run_template_section` (`notifications.rs:1372-1383`). An
/// unrecognized placeholder is left in the output, because `.replace()` only
/// touches tokens present in `replacements`.
fn render_section(
    template: &str,
    token: &str,
    truthy: &[(&str, bool)],
    replacements: &[(&str, String)],
) -> String {
    let mut rendered = render_conditional(template, token, truthy);
    for (placeholder, value) in replacements {
        rendered = rendered.replace(placeholder, value.as_str());
    }
    rendered.trim().to_owned()
}

/// Ported from `render_scheduled_run_slack_sections`'s `truthy_fields` table
/// (`notifications.rs:1232-1263`).
fn build_truthy_fields(
    ctx: &ScheduledRunRenderContext,
    counts: ResultCounts,
    run_url_present: bool,
    version_display_present: bool,
) -> Vec<(&'static str, bool)> {
    vec![
        ("status", true),
        ("status_icon", true),
        ("status_headline", true),
        ("run_url", run_url_present),
        ("run_name", !ctx.run_name.trim().is_empty()),
        ("plan_id", !ctx.plan_id.trim().is_empty()),
        ("phase", !ctx.phase.trim().is_empty()),
        ("platform", value_present(ctx.platform.as_deref())),
        ("product_key", value_present(ctx.product_key.as_deref())),
        ("app_version", value_present(ctx.app_version.as_deref())),
        ("app_build", value_present(ctx.app_build.as_deref())),
        ("test_version", value_present(ctx.test_version.as_deref())),
        ("run_source", !ctx.run_source.trim().is_empty()),
        ("schedule_id", value_present(ctx.schedule_id.as_deref())),
        ("repo_name", value_present(ctx.repo_name.as_deref())),
        ("source_ref", value_present(ctx.source_ref.as_deref())),
        (
            "source_ref_kind",
            value_present(ctx.source_ref_kind.as_deref()),
        ),
        ("version_display", version_display_present),
        ("started_at", value_present(ctx.started_at.as_deref())),
        ("finished_at", value_present(ctx.finished_at.as_deref())),
        ("duration", value_present(ctx.duration.as_deref())),
        ("message", value_present(ctx.message.as_deref())),
        ("results_total", counts.total > 0),
        ("results_passed", counts.passed > 0),
        ("results_failed", counts.failed > 0),
        ("results_skipped", counts.skipped > 0),
        ("results_summary", counts.total > 0),
    ]
}

/// Ported from `render_scheduled_run_slack_sections`'s `replacements` table
/// (`notifications.rs:1264-1336`).
#[allow(
    clippy::too_many_arguments,
    reason = "one parameter per legacy replacement-table input; splitting this \
              into a struct would only rename the same seven values, not reduce them"
)]
fn build_replacements(
    ctx: &ScheduledRunRenderContext,
    defaults: &StatusDefaults,
    status_icon_value: &str,
    run_link: Option<&str>,
    counts: ResultCounts,
    version_display: Option<String>,
) -> Vec<(&'static str, String)> {
    let results_summary = format!(
        "passed={}, failed={}, skipped={}, total={}",
        counts.passed, counts.failed, counts.skipped, counts.total
    );
    vec![
        ("{{status}}", defaults.label.to_owned()),
        ("{{status_icon}}", status_icon_value.to_owned()),
        ("{{run_url}}", run_link.unwrap_or_default().to_owned()),
        ("{{status_headline}}", defaults.headline.to_owned()),
        ("{{run_name}}", ctx.run_name.clone()),
        ("{{plan_id}}", ctx.plan_id.clone()),
        ("{{phase}}", ctx.phase.clone()),
        (
            "{{platform}}",
            display_value(ctx.platform.as_deref()).to_owned(),
        ),
        (
            "{{product_key}}",
            display_value(ctx.product_key.as_deref()).to_owned(),
        ),
        (
            "{{app_version}}",
            display_value(ctx.app_version.as_deref()).to_owned(),
        ),
        (
            "{{app_build}}",
            display_value(ctx.app_build.as_deref()).to_owned(),
        ),
        (
            "{{test_version}}",
            display_value(ctx.test_version.as_deref()).to_owned(),
        ),
        ("{{run_source}}", ctx.run_source.clone()),
        (
            "{{schedule_id}}",
            display_value(ctx.schedule_id.as_deref()).to_owned(),
        ),
        (
            "{{repo_name}}",
            display_value(ctx.repo_name.as_deref()).to_owned(),
        ),
        (
            "{{source_ref}}",
            display_value(ctx.source_ref.as_deref()).to_owned(),
        ),
        (
            "{{started_at}}",
            display_value(ctx.started_at.as_deref()).to_owned(),
        ),
        (
            "{{finished_at}}",
            display_value(ctx.finished_at.as_deref()).to_owned(),
        ),
        (
            "{{duration}}",
            display_value(ctx.duration.as_deref()).to_owned(),
        ),
        ("{{version_display}}", version_display.unwrap_or_default()),
        (
            "{{message}}",
            display_value(ctx.message.as_deref()).to_owned(),
        ),
        ("{{results_total}}", counts.total.to_string()),
        ("{{results_passed}}", counts.passed.to_string()),
        ("{{results_failed}}", counts.failed.to_string()),
        ("{{results_skipped}}", counts.skipped.to_string()),
        ("{{results_summary}}", results_summary),
    ]
}

/// Ported from `render_scheduled_run_slack_blocks` (`notifications.rs:906-947`).
///
/// The order, the emptiness test (`trim().is_empty()`, so a section of only
/// whitespace contributes nothing) and the section/context split are all
/// legacy's. What changed in review finding #17 is only the *type*: this used
/// to build Block Kit JSON here, in the domain, and hand it to the adapter
/// untouched. `infra::notify::block_kit` now does that last step.
fn render_blocks(sections: &RenderedSections) -> Vec<SlackBlock> {
    let mut blocks = Vec::new();

    for section in [
        &sections.header,
        &sections.summary,
        &sections.results,
        &sections.body,
    ] {
        if !section.trim().is_empty() {
            blocks.push(SlackBlock::Section {
                text: section.clone(),
            });
        }
    }
    if !sections.footer.trim().is_empty() {
        blocks.push(SlackBlock::Context {
            text: sections.footer.clone(),
        });
    }

    blocks
}

/// Ported from `replace_slack_links` (`notifications.rs:987-1014`): turns
/// `<url|label>` into `"label (url)"`, and a bare `<token>` into `token`.
fn replace_slack_links(value: &str) -> String {
    let mut output = String::new();
    let mut rest = value;

    while let Some(start) = rest.find('<') {
        output.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('>') else {
            output.push('<');
            output.push_str(after_start);
            return output;
        };
        let token = &after_start[..end];
        if let Some((url, label)) = token.split_once('|') {
            output.push_str(label.trim());
            output.push_str(" (");
            output.push_str(url.trim());
            output.push(')');
        } else {
            output.push_str(token.trim());
        }
        rest = &after_start[end + 1..];
    }

    output.push_str(rest);
    output
}

/// Ported from `strip_mrkdwn` (`notifications.rs:976-985`).
fn strip_mrkdwn(value: &str) -> String {
    replace_slack_links(value)
        .replace(['*', '_', '~', '`'], "")
        .replace('\n', " ")
}

/// Ported from `render_scheduled_run_slack_fallback_text`
/// (`notifications.rs:950-974`). See this module's header, "`fallback_text`",
/// for why the `results` section's own text never appears here.
fn render_fallback_text(
    run_name: &str,
    counts: ResultCounts,
    sections: &RenderedSections,
) -> String {
    let mut parts = Vec::new();

    if sections.header.trim().is_empty() {
        parts.push(run_name.to_owned());
    } else {
        parts.push(strip_mrkdwn(&sections.header));
    }
    if !sections.summary.trim().is_empty() {
        parts.push(strip_mrkdwn(&sections.summary));
    }
    if counts.total > 0 {
        parts.push(format!(
            "passed {}, failed {}, skipped {}",
            counts.passed, counts.failed, counts.skipped
        ));
    }
    if !sections.body.trim().is_empty() {
        parts.push(strip_mrkdwn(&sections.body));
    }

    parts.join(" \u{b7} ")
}

fn render_sections(
    template: &ScheduledRunSlackTemplate,
    defaults: &StatusDefaults,
    manager_ui_base_url: &str,
    token: &str,
    ctx: &ScheduledRunRenderContext,
    counts: ResultCounts,
) -> RenderedSections {
    let run_link = run_url(manager_ui_base_url, &ctx.run_name);
    let version_display =
        format_version_value(ctx.app_version.as_deref(), ctx.app_build.as_deref());
    let status_icon_value = status_icon_or_default(template, defaults.default_icon);

    let truthy = build_truthy_fields(ctx, counts, run_link.is_some(), version_display.is_some());
    let replacements = build_replacements(
        ctx,
        defaults,
        &status_icon_value,
        run_link.as_deref(),
        counts,
        version_display,
    );

    RenderedSections {
        header: render_section(&header_or_default(template), token, &truthy, &replacements),
        summary: render_section(&summary_or_default(template), token, &truthy, &replacements),
        results: render_section(&results_or_default(template), token, &truthy, &replacements),
        body: render_section(&body_or_default(template), token, &truthy, &replacements),
        footer: render_section(&footer_or_default(template), token, &truthy, &replacements),
    }
}

/// Renders one scheduled-run status update as a Slack Block Kit message.
///
/// `token` must be one of `qa_runs_sdk::SLACK_NOTIFICATION_EVENTS` — `None`
/// for anything outside that set, the same partiality as
/// [`qa_insights_sdk::ScheduledRunSlackTemplates::template_for`], which this
/// calls. This function never re-checks whether the event *should* send —
/// that is [`super::routing::route`]'s job; by the time this is called, that
/// decision has already been made, `enabled` included.
///
/// See this module's header for `rendered_message`'s one-section quirk,
/// `fallback_text`'s construction, and the template substitution rules.
#[must_use]
pub fn render_scheduled_run(
    config: &NotificationConfig,
    token: &str,
    ctx: &ScheduledRunRenderContext,
) -> Option<RenderedScheduledRunMessage> {
    let template = config.scheduled_run_slack_templates.template_for(token)?;
    let defaults = status_defaults(token)?;
    let counts = count_results(&ctx.result_statuses);

    let sections = render_sections(
        template,
        &defaults,
        &config.manager_ui_base_url,
        token,
        ctx,
        counts,
    );
    let blocks = render_blocks(&sections);
    let fallback_text = render_fallback_text(&ctx.run_name, counts, &sections);

    Some(RenderedScheduledRunMessage {
        rendered_message: sections.body,
        fallback_text,
        blocks,
    })
}

/// The preview path — [`render_scheduled_run`] under a second name, calling
/// it directly and nothing else. See this module's header, "One renderer,
/// not two", for why that is deliberate rather than an oversight.
#[must_use]
pub fn preview_scheduled_run(
    config: &NotificationConfig,
    token: &str,
    ctx: &ScheduledRunRenderContext,
) -> Option<RenderedScheduledRunMessage> {
    render_scheduled_run(config, token, ctx)
}

/// Everything [`render_run_completed`] needs about one run. Legacy's
/// `WorkflowRun` restricted to the fields the generic completion alert
/// actually reads (`notifications.rs:230-245`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunCompletedRenderContext {
    pub run_name: String,
    pub plan_id: String,
    pub platform: Option<String>,
    pub product_key: Option<String>,
    /// Raw result status strings, folded by [`count_results`].
    pub result_statuses: Vec<String>,
}

/// One rendered `RunCompleted` alert. `text` is legacy's single body,
/// reused for both channels — see this module's header, "Email rendering".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedRunCompletedMessage {
    /// The generic Slack alert text **and** the email body — legacy sends
    /// the same value both places (`notifications.rs:284-289,328-329`).
    pub text: String,
    pub email_subject: String,
}

/// Ported from `notify_run_completed`'s headline choice
/// (`notifications.rs:191-192,222-228`): `"FAILED"` if anything failed,
/// `"SUCCEEDED"` if every result passed and at least one did, else
/// `"COMPLETED"` (the `total == 0` and the `total > 0, failed == 0, passed == 0`
/// cases — no results at all, or only statuses that are neither passed nor
/// failed, such as `"PENDING"`/`"RUNNING"` — both fall through to this arm).
fn run_completed_headline(counts: ResultCounts) -> &'static str {
    let all_passed = counts.total > 0 && counts.failed == 0 && counts.passed > 0;
    if counts.failed > 0 {
        "FAILED"
    } else if all_passed {
        "SUCCEEDED"
    } else {
        "COMPLETED"
    }
}

/// Renders `RunCompleted`'s generic (non-templated) alert — legacy's
/// `notify_run_completed` (`notifications.rs:180-355`). See this module's
/// header, "Email rendering (R98)", for what legacy does.
///
/// **R99: the email subject keeps legacy's literal `"VHP test run"` branding,
/// unchanged.** An earlier draft of this function rebranded it to `"QA run"`
/// as a unilateral in-task call; the controller reverted that. The decisive
/// reason is in-crate consistency, not the branding call itself: this crate
/// already ships another reviewed, user-visible `"VHP"` string —
/// `infra::jira::oagw_client`'s JIRA issue summary,
/// `format!("[VHP] Test Failed: {test_name}")` (`oagw_client.rs:789`), which
/// has carried the same branding verbatim since Tasks 32-33. Rebranding only
/// this string would leave the product speaking two names. Whether to drop
/// `"VHP"` everywhere is a real, open product question — recorded by the
/// controller alongside this plan's other product-level items — but it spans
/// both strings (and probably more outside this crate), so it is not a call
/// this rendering task gets to make unilaterally. Pinned by
/// `run_completed_email_subject_keeps_the_legacy_vhp_branding`.
#[must_use]
pub fn render_run_completed(ctx: &RunCompletedRenderContext) -> RenderedRunCompletedMessage {
    let counts = count_results(&ctx.result_statuses);
    let headline = run_completed_headline(counts);

    let mut info_parts = vec![ctx.plan_id.clone()];
    if value_present(ctx.platform.as_deref()) {
        info_parts.push(display_value(ctx.platform.as_deref()).to_owned());
    }
    if value_present(ctx.product_key.as_deref()) {
        info_parts.push(display_value(ctx.product_key.as_deref()).to_owned());
    }

    let text = format!(
        "{} \u{2014} {} \u{b7} {}\npassed {}, failed {}, skipped {}",
        ctx.run_name,
        headline,
        info_parts.join(" \u{b7} "),
        counts.passed,
        counts.failed,
        counts.skipped
    );

    RenderedRunCompletedMessage {
        text,
        email_subject: format!("VHP test run {} {headline}", ctx.run_name),
    }
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod render_tests;
