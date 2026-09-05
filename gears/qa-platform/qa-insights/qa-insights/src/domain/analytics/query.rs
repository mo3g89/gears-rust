//! The overview endpoint's query string, validated and normalized.
//!
//! `normalize_overview_query` (`manager/src/routes/analytics.rs:2392-2431`),
//! [`parse_scope`] (`:2104-2113`), [`parse_group`] (`:2115-2127`) and the one
//! validation `api_build_tests` adds on top of them (`:373-376`).
//!
//! # Every rejection here is a **400**, and the status code is the point
//!
//! Legacy returns `(StatusCode, String)` and every one of the **six** rejections
//! below is `StatusCode::BAD_REQUEST`. A 400 that legacy returns and this gear
//! does not is a behaviour change the SPA notices — it renders the message — so
//! the messages are ported verbatim, not paraphrased.
//!
//! Six and not five: `product_id`, `version`, `scope`, `group_by` and the
//! plan-scope rule are [`normalize_overview_query`]'s, and the sixth is
//! [`normalize_build`]'s — `api_build_tests`' own check, which runs *before*
//! that function rather than inside it. Counting only the five is easy and this
//! header did it; that function's `# Errors` says five and is right, because it
//! is scoped to that function.
//!
//! The status code is not spelled in this module, and that is the crate's
//! existing split rather than an omission: [`DomainError::Validation`] is mapped
//! to `invalid_argument` — HTTP **400**, with a field violation naming the
//! parameter — by the single `From<DomainError> for CanonicalError` in
//! [`crate::api::rest::error`], and `a_refused_window_is_400_invalid_argument`
//! there pins that arm. So the rule lives here where a test can reach it without
//! a router, and the mapping lives there where it is stated once for the whole
//! gear. [`super::super::service::reconcile`]'s rebuild window is the precedent:
//! same variant, same arm, and until this module it was the *only* raiser — a
//! claim that arm's own comment made and this module falsifies.
//!
//! `field` is the **query-parameter name** in every case, which is what makes the
//! canonical field violation useful to a client; `message` is legacy's string.
//!
//! # What is deliberately *not* resolved here
//!
//! [`NormalizedOverviewQuery::product_id`] and
//! [`NormalizedOverviewQuery::plan_id`] stay the opaque strings legacy's route
//! carries. Legacy resolves the first through its own product registry
//! (`analytics.rs:391-395`) and matches the second against `r.plan_id`, and
//! **this schema has neither column**: VHP-319 deleted the product-version model
//! and a plan's identity here is the `(repo_id, plan_path)` pair
//! ([`super::UniverseFilter`]' header carries that in full, as the plan's carried
//! item 1). Turning these two strings into a
//! [`CatalogReader::list_universe`](crate::domain::ports::CatalogReader::list_universe)
//! argument and a [`super::PlanRef`] is the job of the task that turns a request
//! into a read, and it is not this one. **Task 25b is that task**, and the answer
//! is in [`crate::domain::service::analytics`]: `product_uuid` parses the first
//! (and is the seventh rejection, the only one not here), `narrow_to_plan`
//! resolves the second against the universe's `plan_path`, and `universe_filter`
//! derives the row predicate from what is left.
//!
//! **Keeping `product_id` a `String` is what preserves the 400.** A `Uuid` field
//! would move the rejection into the deserializer, which answers with its own
//! message and its own shape — so `""` would stop being *"`product_id` is
//! required"* and start being whatever the extractor says about a malformed UUID.
//! That is exactly the kind of silent status/message drift this module exists to
//! prevent, and it is why the type is not "improved" here.
//!
//! # The two day counts are clamped **twice**, and both clamps are legacy's
//!
//! `clamp_days(query.days_heatmap.unwrap_or(7) as usize, 1, 30)` (`:2426`) and
//! `clamp_days(query.days_trend.unwrap_or(90) as usize, 7, 365)` (`:2427`) here,
//! and again inside `build_heatmap` (`:1452`) and `build_trend` (`:1496`). Both
//! are ported: [`super::aggregates::heatmap_days`] and
//! [`super::aggregates::trend_days`] are the second pair and this module calls
//! them for the first, which is what those two functions' headers say this task
//! would do. **The defaults live here** — `7` and `90` — because the `Option`
//! they default is a *query* parameter and nothing in a pure fold can see whether
//! the caller asked.
//!
//! # Where the group vocabulary comes from
//!
//! [`parse_group`] returns [`super::aggregates::GroupBy`], which Task 23 already
//! declared as legacy's four variants for
//! [`super::aggregates::apply_universe_group_filter`]. A second enum here would
//! be two spellings of one vocabulary; [`Scope`] is new because nothing in the
//! pure folds ever sees it — the scope selects *rows*, which is a read predicate.

use crate::domain::analytics::aggregates::{GroupBy, heatmap_days, trend_days};
use crate::domain::error::DomainError;

/// Which executions the overview is about.
///
/// Legacy's `Scope` (`analytics.rs:306-310`), two variants, two variants.
///
/// No `Default`, unlike [`GroupBy`]: legacy's `scope` query parameter is a
/// **required** `String` with no fallback (`:2408` parses it unconditionally,
/// where `:2409` defaults the group to `"none"`), so a default value would be a
/// value no request can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every plan the product's universe contains.
    All,
    /// One plan — and [`NormalizedOverviewQuery::plan_id`] is then required.
    Plan,
}

/// The overview endpoint's nine query parameters, as they arrive.
///
/// Legacy's `AnalyticsOverviewQuery` (`analytics.rs:19-33`), nine fields, nine
/// fields. Untrusted and unvalidated: [`normalize_overview_query`] is what turns
/// one of these into a [`NormalizedOverviewQuery`].
///
/// A domain type rather than the REST tier's request DTO, so that the rules below
/// are testable without a router — the split this module's header describes. The
/// DTO that deserializes it is the transport layer's and maps onto this field for
/// field.
#[derive(Clone, Debug, Default)]
pub struct OverviewQuery {
    /// Required, non-blank. Opaque here — see this module's header.
    pub product_id: String,
    /// Required, non-blank. Legacy's `app_version` predicate, i.e.
    /// [`super::UniverseFilter::product_version`].
    pub version: String,
    /// Required. `"all"` or `"plan"`, case-insensitively — [`parse_scope`].
    pub scope: String,
    /// Required **when `scope` is `plan`** and ignored otherwise.
    pub plan_id: Option<String>,
    /// Absent, blank or whitespace all mean *every* branch — the opposite of
    /// what `None` means on the universe side. [`super::UniverseFilter::branch`]
    /// carries the asymmetry.
    pub branch: Option<String>,
    /// Defaults to `7`, clamped to `[1, 30]`.
    pub days_heatmap: Option<u32>,
    /// Defaults to `90`, clamped to `[7, 365]`.
    pub days_trend: Option<u32>,
    /// Defaults to `"none"` — [`parse_group`].
    pub group_by: Option<String>,
    /// Blank narrows nothing, which is legacy's behaviour rather than an empty
    /// result; [`super::aggregates::apply_universe_group_filter`] records it.
    pub group_value: Option<String>,
}

/// An [`OverviewQuery`] that passed every rule.
///
/// Legacy's `NormalizedOverviewQuery` (`analytics.rs:2377-2390`), nine fields,
/// nine fields — with the two day counts already `usize` and already clamped,
/// which is the whole reason the type is distinct from the input.
#[derive(Clone, Debug)]
pub struct NormalizedOverviewQuery {
    /// Trimmed and non-empty. Still opaque — this module's header says why it is
    /// not a `Uuid`.
    pub product_id: String,
    /// Trimmed and non-empty.
    pub version: String,
    /// Parsed by [`parse_scope`]. **Always the caller's** — [`Scope`] has no
    /// `Default` because legacy's `scope` parameter is required, so there is no
    /// value here that means "the caller did not say".
    pub scope: Scope,
    /// Trimmed, blanks dropped. Guaranteed `Some` when [`Self::scope`] is
    /// [`Scope::Plan`], and *not* guaranteed absent otherwise — legacy carries a
    /// `plan_id` through an `all` scope and simply never reads it.
    pub plan_id: Option<String>,
    /// Trimmed, blanks dropped. `None` means every branch.
    pub branch: Option<String>,
    /// Already in `[1, 30]`.
    pub days_heatmap: usize,
    /// Already in `[7, 365]`.
    pub days_trend: usize,
    /// Parsed by [`parse_group`], defaulting to [`GroupBy::None`] when the
    /// parameter is **absent**. A [`GroupBy::None`] here therefore does not
    /// distinguish "no grouping asked for" from "`?group_by=none`" — and it is
    /// never the result of a *blank* parameter, which is a 400.
    pub group_by: GroupBy,
    /// Trimmed, blanks dropped.
    pub group_value: Option<String>,
}

/// Trim, and read blank as absent.
///
/// `normalize_optional` (`analytics.rs:2070-2075`) verbatim. Applied to
/// `plan_id`, `branch` and `group_value` and to none of the three required
/// parameters, which get their own rejection instead.
fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// `"all"` or `"plan"`, trimmed and case-insensitive.
///
/// `parse_scope` (`analytics.rs:2104-2113`). Anything else is a **400** carrying
/// legacy's message verbatim, `"scope must be 'all' or 'plan'"` — quoted string
/// included, because the SPA renders it.
///
/// `to_ascii_lowercase` and not `to_lowercase`: legacy's is the ASCII one
/// (`:2105`), so a Turkish dotless `ı` does not fold into `i` and `"PLAN"` in a
/// Turkish locale still parses. Same substitution the rest of this phase makes.
///
/// # Errors
///
/// [`DomainError::Validation`] on `scope`.
pub fn parse_scope(value: &str) -> Result<Scope, DomainError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "all" => Ok(Scope::All),
        "plan" => Ok(Scope::Plan),
        _ => Err(DomainError::Validation {
            field: "scope".to_owned(),
            message: "scope must be 'all' or 'plan'".to_owned(),
        }),
    }
}

/// One of the four groupings, trimmed and case-insensitive, defaulting to
/// [`GroupBy::None`].
///
/// `parse_group` (`analytics.rs:2115-2127`). **`None` and `Some("")` both mean
/// `none`** rather than being rejected: legacy's `value.unwrap_or("none").trim()`
/// (`:2116`) turns an absent parameter into the literal, and a blank one trims to
/// `""` — which is *not* one of the four arms and therefore **is** rejected.
/// That asymmetry is legacy's: `?group_by=` (present and empty) is a 400 where
/// omitting the parameter is not. Ported as-is and pinned, because "an empty
/// parameter is the same as no parameter" is the natural assumption and it is
/// wrong here.
///
/// # Errors
///
/// [`DomainError::Validation`] on `group_by`. The message is **not** legacy's
/// verbatim: the fourth grouping is spelled `environment` here rather than
/// legacy's `platform` (`:2115-2127` still says `platform` — see this crate's
/// own `TargetPlatform` -> `Environment` rename), a deliberate divergence from
/// legacy and not parity.
pub fn parse_group(value: Option<&str>) -> Result<GroupBy, DomainError> {
    match value.unwrap_or("none").trim().to_ascii_lowercase().as_str() {
        "none" => Ok(GroupBy::None),
        "component" => Ok(GroupBy::Component),
        "tag" => Ok(GroupBy::Tag),
        "environment" => Ok(GroupBy::Environment),
        _ => Err(DomainError::Validation {
            field: "group_by".to_owned(),
            message: "group_by must be one of: none, component, tag, environment".to_owned(),
        }),
    }
}

/// Every rule the overview's query string is subject to, in legacy's order.
///
/// `normalize_overview_query` (`analytics.rs:2392-2431`).
///
/// # The order of the checks is observable
///
/// A request that is wrong in two ways gets the **first** message, and legacy's
/// order is `product_id` (`:2395-2401`), `version` (`:2403-2406`), `scope`
/// (`:2408`), `group_by` (`:2409`), then the plan-scope rule (`:2413-2418`). So a
/// blank `product_id` with a nonsense `scope` answers *"`product_id` is
/// required"*, and reordering the checks changes what a client is told to fix
/// without changing the status code — which is why
/// `the_first_failing_rule_is_the_one_reported` pins the pair rather than each
/// rule alone.
///
/// Note what is **not** in that order: `group_value` and `branch` are normalized
/// (`:2410`, `:2425`) and never rejected, and `plan_id` is normalized at `:2412`
/// *before* the rule at `:2413` reads it.
///
/// # Errors
///
/// [`DomainError::Validation`] naming the offending parameter, which the REST
/// tier renders as a 400 — see this module's header. Five rejections, one per
/// legacy `Err`.
pub fn normalize_overview_query(
    query: &OverviewQuery,
) -> Result<NormalizedOverviewQuery, DomainError> {
    let product_id = query.product_id.trim();
    if product_id.is_empty() {
        return Err(DomainError::Validation {
            field: "product_id".to_owned(),
            message: "product_id is required".to_owned(),
        });
    }

    let version = query.version.trim();
    if version.is_empty() {
        return Err(DomainError::Validation {
            field: "version".to_owned(),
            message: "version is required".to_owned(),
        });
    }

    let scope = parse_scope(query.scope.as_str())?;
    let group_by = parse_group(query.group_by.as_deref())?;
    let group_value = normalize_optional(query.group_value.as_deref());

    let plan_id = normalize_optional(query.plan_id.as_deref());
    if scope == Scope::Plan && plan_id.is_none() {
        return Err(DomainError::Validation {
            field: "plan_id".to_owned(),
            message: "plan_id is required when scope=plan".to_owned(),
        });
    }

    Ok(NormalizedOverviewQuery {
        product_id: product_id.to_owned(),
        version: version.to_owned(),
        scope,
        plan_id,
        branch: normalize_optional(query.branch.as_deref()),
        // The **first** of legacy's two clamps; `heatmap_days` and `trend_days`
        // are the second, applied inside the folds. Both are ported — see this
        // module's header, and those two functions', which named this task.
        days_heatmap: heatmap_days(day_count(query.days_heatmap, 7)),
        days_trend: trend_days(day_count(query.days_trend, 90)),
        group_by,
        group_value,
    })
}

/// A day-count query parameter with its default applied, as a `usize`.
///
/// Legacy writes `query.days_heatmap.unwrap_or(7) as usize` (`:2426`). `as` is
/// denied in this workspace, and the fallible conversion needs an answer for the
/// case it cannot reach: `u32` exceeds `usize` only on a 16-bit target, and
/// **the saturation is unobservable regardless** because both callers clamp the
/// result to at most `365` immediately. `usize::MAX` rather than the default,
/// because saturating upward keeps the clamp's direction — an absurd request
/// lands on the ceiling, which is what `clamp` would have done with the real
/// value.
fn day_count(value: Option<u32>, default: u32) -> usize {
    usize::try_from(value.unwrap_or(default)).unwrap_or(usize::MAX)
}

/// The `build` parameter of the build-tests drill-down, trimmed and required.
///
/// `api_build_tests` (`analytics.rs:373-376`) — the one rejection that is **not**
/// part of [`normalize_overview_query`], and it runs **before** it (`:373`
/// precedes the `normalize_overview_query` call at `:379`). So a request to that
/// endpoint with a blank `build` *and* a blank `product_id` is told about the
/// build, which is the reverse of what reading `normalize_overview_query` alone
/// would suggest.
///
/// The endpoint reuses **seven** of the overview's nine parameters —
/// `AnalyticsBuildTestsQuery` is `:51-60`, eight fields, those seven plus
/// `build` — and supplies the other two itself as `days_heatmap: None,
/// days_trend: None` (`:385-386`). The drill-down draws no chart, so [`heatmap_days`]'
/// and [`trend_days`]' defaults are applied and then unused.
///
/// (This paragraph said "eight parameters" and cited `:2383-2384` and `:384-385`.
/// All three were wrong: `:2383-2384` is a doc comment on
/// `NormalizedOverviewQuery::branch`, `:384` is `branch: query.branch`, and the
/// call site is `:379` rather than `:378`, which is blank. Recorded because
/// off-by-one citations into a 2700-line file are this port's most repeated
/// defect.)
///
/// **`UNKNOWN_BUILD` is not applied here.** A blank build is refused, not
/// collapsed: [`super::universe::collapse_build`] is the *row* side of that
/// vocabulary (a stored build that is absent renders under
/// [`super::universe::UNKNOWN_BUILD`]), and the two must not be confused —
/// collapsing the query parameter would silently turn "you forgot the build" into
/// "show me the tests with no build".
///
/// # Errors
///
/// [`DomainError::Validation`] on `build`, with legacy's message
/// `"build is required"`.
pub fn normalize_build(build: &str) -> Result<String, DomainError> {
    let build = build.trim();
    if build.is_empty() {
        return Err(DomainError::Validation {
            field: "build".to_owned(),
            message: "build is required".to_owned(),
        });
    }
    Ok(build.to_owned())
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod query_tests;
