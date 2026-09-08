//! qa-catalog observability metric catalog.
//!
//! One path in this gear is measured here: **product plugin resolution** —
//! `domain::service::QaProductRegistry`'s `plugin_for`, the one hop that turns
//! a product id into the plugin object that owns that product's behaviour.
//! Every observation qa-environments performs and every dispatch decision
//! qa-runs makes about a product goes through it, and before this module
//! existed its only report was two `tracing` lines.
//!
//! # Metric naming
//!
//! These constants are the **full, literal Prometheus series names** — what
//! appears in Prometheus / `VictoriaMetrics`. They bake in the suffix the
//! OTel→Prometheus translation would otherwise add: counters carry `_total`,
//! histograms carry the unit word. No `.with_unit()` hint is set on any
//! instrument, so the rendered name is identical whether the collector has
//! `add_metric_suffixes` on or off. qa-runs', qa-insights' and
//! qa-environments' `domain::metrics` document the same mechanism in their
//! module headers; this follows them.
//!
//! `counter_names_carry_the_total_suffix_and_duration_names_carry_the_unit`
//! and `every_metric_is_namespaced_to_this_gear` are the gate. They are worth
//! having because the defect they catch is invisible: a series exported under
//! a name nobody queries looks, from inside this process, exactly like a
//! series that works.
//!
//! # The plugin boundary, seen from the side that owns the binding
//!
//! This is one half of a measurement that spans two gears. The other half is
//! qa-environments' plugin-call family, which times the plugin's own round
//! trip to a target. This half times the step *before* it: deciding which
//! plugin that is.
//!
//! The two are **nested inside a third**, and the relationship is written down
//! in all three places so that nobody adds them together:
//! `qa_environments`' per-environment observation duration contains a
//! resolution measured here **plus** a credential read **plus** the plugin
//! round trip **plus** two database calls. Summing any two of the three
//! double-counts the same seconds.
//!
//! # What is deliberately *not* a label
//!
//! No product id, no tenant id, and — the one that needs saying because it is
//! right there in the resolved value — **no plugin GTS instance id**. See
//! [`crate::domain::ports::metrics`]'s header, which carries the argument and
//! also records why the plugin's *type* id is not a label either.

/// One product-plugin resolution: `QaProductRegistry::plugin_for`, from the
/// policy check to the plugin object (or to the reason there is none),
/// counted by how the resolution ended.
///
/// **Not one plugin call.** Nothing in this gear calls a plugin; this family
/// counts the lookup that hands one out. What the plugin then did with the
/// call is qa-environments' plugin-call family, whose own doc names this one.
///
/// Labelled by [`crate::domain::ports::metrics::PluginResolutionOutcome`].
pub const QA_CATALOG_PLUGIN_RESOLUTION: &str = "qa_catalog_plugin_resolution_total";

/// Wall-clock duration of one product-plugin resolution. Same label set as
/// [`QA_CATALOG_PLUGIN_RESOLUTION`], so a rate and a quantile can be read side
/// by side and a refused resolution is not mixed into the distribution of ones
/// that produced a plugin.
///
/// # What is inside the span
///
/// Everything `plugin_for` does, in order: the PEP check for `qa.product`
/// `get` (a call into the deployment's authz resolver, which may be a network
/// hop), the database connection, the tenant-scoped product read, and the
/// `ClientHub` probe under the product's stored instance id. The clock stops
/// the instant the method's result is decided, before the `debug!`/`warn!`
/// line it writes.
///
/// The `ClientHub` probe is the cheap part — a `TypeId` hash under an `RwLock`
/// read, which `domain::service::plugin_registry`'s own header calls O(1) and
/// declines to cache. So a slow sample here is the PDP or the database, and
/// that is the useful reading: this family answers *is resolution itself
/// costing anything*, which matters because it sits on the critical path of
/// every observation qa-environments performs.
///
/// # Nested inside two other measurements, and not to be added to them
///
/// A resolution driven from the observation cycle is inside
/// `qa_environments`' per-environment observation duration, which is in turn
/// inside that gear's cycle duration. It is **beside**, never inside,
/// qa-environments' plugin-call duration: that family starts after this one
/// has already returned. Adding this family to either of the qa-environments
/// durations counts the same seconds twice.
pub const QA_CATALOG_PLUGIN_RESOLUTION_DURATION: &str =
    "qa_catalog_plugin_resolution_duration_seconds";

/// Every counter family this gear exports.
///
/// Declared rather than derived, and therefore its own oracle. What makes it
/// worth having anyway is that the naming rules are properties *of the set*:
/// "every counter ends `_total`" is unstatable one constant at a time, and a
/// constant added without being listed here is a constant no rule checks.
///
/// # Read only by the tests, and that is not dead code
///
/// `domain` is `pub(crate)` in this gear (as it is in qa-environments, and
/// unlike qa-runs and qa-insights), so a catalog constant nothing outside the
/// naming tests names is genuinely unreachable and the compiler says so. The
/// allowance is on the two list constants only, and the alternative —
/// deleting them — deletes the gate: every naming rule in this catalog is a
/// property *of the set*, and a family absent from these lists is a family no
/// rule checks.
#[allow(
    dead_code,
    reason = "read by `domain::metrics::tests` and `infra::metrics::tests`; `domain` is \
              pub(crate) in this gear, so a constant with no production reader is \
              unreachable and deleting it would delete the naming gate"
)]
pub const COUNTERS: &[&str] = &[QA_CATALOG_PLUGIN_RESOLUTION];

/// Every duration histogram this gear exports. See [`COUNTERS`] for why the
/// list is declared, and for why it carries a dead-code allowance.
#[allow(dead_code, reason = "see COUNTERS")]
pub const DURATIONS: &[&str] = &[QA_CATALOG_PLUGIN_RESOLUTION_DURATION];

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metrics_tests.rs"]
mod tests;
