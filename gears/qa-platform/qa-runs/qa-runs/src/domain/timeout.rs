//! Run timeout resolution: legacy's **three** chains, plus a clamp on the one
//! knob this port added.
//!
//! A pure core, in the shape `domain::exclusivity` already established, and here
//! for the reason `domain::service`'s header gives: *"The four pure cores decide,
//! the repositories persist … this layer is what puts them in order."* Timeout
//! resolution decides; it did not belong inside the service that orders it.
//!
//! It was moved out of `service::launch` in Task 13's third review round, and the
//! trigger was a defect rather than tidiness: **Task 15's re-run has to resolve a
//! new deadline for a new run**, and `resolve_timeout_seconds` was private to
//! `launch`. The thing that would have been duplicated is a three-branch chain
//! with two load-bearing asymmetries that take a page to explain — the exact
//! shape of legacy's own warning about the branch chain, and exactly what
//! parity spec §3.4 rule 1's "written once" discipline exists to prevent.
//!
//! Contrast the branch chain, which stays private to `launch` on purpose: dispatch
//! must read the *recorded* branch off `Run::test_version` rather than resolve it
//! again. Share the function when the decision is being made afresh; read the row
//! when it has already been made. See `service::launch::resolve_branch`.

use qa_runs_sdk::RunKind;

/// Per-kind timeout fallback, in seconds, when no plan and no configured
/// default supply one.
///
/// **Read from the source system, and the three kinds genuinely differ** — this
/// is not one chain with three constants:
///
/// * `RunKind::Plan` — the plan's own `timeout_seconds` is used directly and
///   `default_timeout_seconds` is never consulted
///   (`manager/src/services/argo.rs:539`,
///   `"activeDeadlineSeconds": plan.plan.timeout_seconds`). That field is a
///   plain `u64` with a serde default of **300**
///   (`manager/src/models.rs:11-12` and `:29-31`), so a plan run's floor is 300,
///   not 1800. qa-catalog applies the same default when it parses `plan.yaml`
///   (`qa-catalog/src/domain/parsing/plan_yaml.rs:42`, `:55-56`), so a discovered
///   plan always arrives with a value and this constant is the belt-and-braces
///   arm.
/// * `RunKind::Test` — `self.default_timeout_seconds(1800)`
///   (`manager/src/services/argo.rs:746`).
/// * `RunKind::CustomPlan` — `self.default_timeout_seconds(3600)`
///   (`manager/src/services/argo.rs:1065`).
/// * `RunKind::Collect` — **600, and it is not a fallback: it is the whole
///   chain.** The collect job synthesizes its own `TestPlanInfo` with
///   `timeout_seconds: 600` (`manager/src/services/collect.rs:106`) and submits
///   it through the plan path, which forwards a plan's own value verbatim
///   (`argo.rs:539`). So the source system reaches 600 by way of a constant it
///   wrote one line earlier, never by consulting
///   `runner_defaults.default_timeout_seconds`. [`resolve_timeout_seconds`]'s
///   arm reproduces that by consulting nothing.
#[must_use]
pub const fn kind_timeout_fallback(kind: RunKind) -> u64 {
    match kind {
        RunKind::Plan => 300,
        RunKind::Test => 1800,
        RunKind::CustomPlan => 3600,
        RunKind::Collect => 600,
    }
}

/// The ceiling a **caller-supplied** `timeout_seconds` is clamped to: seven days.
///
/// # Why the caller's value has to be clamped and not saturated
///
/// `timeout_at` is what the control-plane sweep reclaims a stuck run by, and a
/// NULL one is excluded by construction — `infra::storage::runs_sea_repo`'s
/// `list_timeout_candidates` says it in place: *"A NULL `timeout_at` compares
/// NULL and is excluded, which is the wanted reading: a run with no deadline
/// never times out."* `u64::MAX` seconds saturates to `i64::MAX`, which
/// `OffsetDateTime::checked_add` answers `None` for, so
/// `{"timeout_seconds": 18446744073709551615}` minted a run the sweep could never
/// reclaim — and an **exclusive** one holds the platform's global lease
/// (`qa_platform_leases` is keyed on a bare `platform_id`, so the lease is not
/// tenant-partitioned) until somebody cancels it by hand, blocking every later
/// run on that platform. `saturating_i64`'s own doc used to argue that saturation
/// was right because the input's "only sensible reading is 'no practical
/// deadline'"; that is the caller's reading, not the operator's.
///
/// # Why clamping breaks no ported behaviour
///
/// **The source system has no per-launch timeout knob at all.** Verified field by
/// field against all three of its launch DTOs — `SubmitPlanRunRequest`
/// (`manager/src/models.rs:1495-1515`), `SubmitSingleTestRunRequest`
/// (`:1522-1539`) and `SubmitCustomPlanRunRequest` (`:1546-1562`) — none of which
/// carries a timeout. Every legacy deadline comes from `plan.yaml`, from
/// `runner_defaults.default_timeout_seconds`, or from a per-kind constant.
/// `LaunchRequest::timeout_seconds` is this port's addition and unbounded caller
/// input, so clamping it is not a parity divergence.
///
/// # Why seven days
///
/// The ceiling has to sit above every deadline the source system can produce from
/// non-caller input and below "effectively never". The largest legacy constant is
/// the custom-plan floor of 3600 seconds (`manager/src/services/argo.rs:1065`);
/// above that only an operator-set `default_timeout_seconds` or an
/// operator-authored `plan.yaml` goes higher, and neither is clamped here. A week
/// is ~168x that floor — so no plausible suite is cut short — while still
/// guaranteeing that a run abandoned by a crashed executor is reclaimed inside
/// one, which is the property the sweep exists for.
///
/// **Task 16 owes the same check at the REST boundary**, as a 400 naming the
/// ceiling rather than a silent clamp: a caller who asks for a year should be
/// told the request was changed. This constant is the fail-safe behind it, not a
/// substitute for it.
pub const MAX_LAUNCH_TIMEOUT_SECONDS: u64 = 7 * 24 * 60 * 60;

/// The floor under a **caller-supplied** `timeout_seconds`: one second.
///
/// `Some(0)` is not "no deadline". It produced `timeout_at = Some(now)`, i.e. a
/// run whose deadline is the instant it launched, which the very next
/// control-plane tick cancels — a launch that succeeds and is then killed before
/// it can start, with nothing in the response to explain it. One second is the
/// smallest value that is still a deadline; a caller who means "do not set one"
/// omits the field, which is what `Option` is for.
pub const MIN_LAUNCH_TIMEOUT_SECONDS: u64 = 1;

/// The run's timeout, in seconds.
///
/// The chain, and **where it departs from the plan's description, with the
/// reading that produced the departure.** The plan's Step 3 asks for one unified
/// chain — *"explicit launch override -> the plan's `timeout_seconds` -> the
/// configured default -> the per-kind fallback"* — and the source system does
/// not have one. It has three:
///
/// | Kind | Source system |
/// |---|---|
/// | plan | `plan.plan.timeout_seconds`, always (`argo.rs:539`) |
/// | single test | `default_timeout_seconds(1800)`; the plan's value is **not** consulted (`argo.rs:746`) |
/// | custom plan | `composed_timeout_seconds.max(default_timeout_seconds(3600))` (`argo.rs:1065`) |
///
/// Two of those are load-bearing differences, so legacy is ported rather than
/// the unified chain:
///
/// * A **single test** takes the configured default even when the plan it
///   belongs to declares a longer one. Unifying would give a single test from a
///   two-hour plan a two-hour deadline, which the source system never grants.
/// * A **custom plan** takes the `max`, not the first-that-is-set: the
///   configured default is a *floor* there, and the source system says so
///   (`argo.rs:1063-1064`, *"Use the summed core-plan timeout when it exceeds
///   the configured default; otherwise fall back to the default as a floor"*).
///   First-wins would shorten a composed plan below its own sum.
///
/// The launch override is this gear's addition — `LaunchRequest::timeout_seconds`
/// is in the shipped SDK contract and the source system has no per-launch knob —
/// and it wins outright, which is what an override means. It is **clamped**, and
/// [`MAX_LAUNCH_TIMEOUT_SECONDS`] says why.
///
/// `configured_default` is honoured only when it is **non-zero**, matching
/// `default_timeout_seconds` (`manager/src/services/argo.rs:168-181`, the guard
/// itself at `:174`: `Ok(Some(cfg)) if cfg.default_timeout_seconds > 0`,
/// everything else falling through to the caller's fallback).
#[must_use]
pub fn resolve_timeout_seconds(
    override_seconds: Option<u64>,
    plan_seconds: Option<u64>,
    configured_default: u64,
    kind: RunKind,
) -> u64 {
    if let Some(seconds) = override_seconds {
        return seconds.clamp(MIN_LAUNCH_TIMEOUT_SECONDS, MAX_LAUNCH_TIMEOUT_SECONDS);
    }
    let configured = (configured_default > 0).then_some(configured_default);
    match kind {
        // The plan's own value first; the configured default and the 300-second
        // floor are reached only if qa-catalog ever stops defaulting it.
        RunKind::Plan => plan_seconds
            .or(configured)
            .unwrap_or_else(|| kind_timeout_fallback(kind)),
        // The plan's value is deliberately not consulted.
        RunKind::Test => configured.unwrap_or_else(|| kind_timeout_fallback(kind)),
        // The default is a floor, not a fallback.
        RunKind::CustomPlan => plan_seconds
            .unwrap_or(0)
            .max(configured.unwrap_or_else(|| kind_timeout_fallback(kind))),
        // Neither input is consulted, and both omissions are the source
        // system's. There is no plan: a collect run enumerates whatever the
        // branch holds rather than a named `plan.yaml`, and the `TestPlanInfo`
        // the collect job builds is a synthetic wrapper whose `timeout_seconds`
        // is the literal 600 (`manager/src/services/collect.rs:106`). And the
        // configured default is never reached, because that wrapper's value is
        // forwarded verbatim by the plan submit path (`argo.rs:539`) — the
        // `default_timeout_seconds` lookup belongs to the single-test and
        // custom-plan paths (`:746`, `:1065`), which collect does not take.
        //
        // `configured` is therefore deliberately unused here. Threading it in
        // "for consistency" would let an operator's global default silently
        // lengthen or shorten the hourly cycle's deadline, which the source
        // system does not permit.
        RunKind::Collect => kind_timeout_fallback(kind),
    }
}

/// Widen a `u64` second count onto the `i64` `time::Duration` takes, saturating.
///
/// **The `LaunchRequest::timeout_seconds` path can no longer reach the saturating
/// arm**, because [`resolve_timeout_seconds`] clamps that one input to
/// [`MAX_LAUNCH_TIMEOUT_SECONDS`] before returning it.
/// `an_unbounded_caller_timeout_is_clamped_to_the_ceiling` and
/// `a_clamped_caller_timeout_still_lands_inside_the_calendar` pin that.
///
/// **The other inputs are not all bounded, and an earlier version of this comment
/// said they were** — *"every other input to that function is either one of this
/// module's own constants or the configured default"*, which `plan_seconds`
/// falsifies outright: `resolve_timeout_seconds(None, Some(u64::MAX), 0,
/// RunKind::Plan)` returns `u64::MAX` and lands here. The false sentence came two
/// paragraphs before the paragraph that contradicted it, so a reader met the
/// reassurance first.
///
/// # What can still overflow, stated rather than fixed
///
/// A `plan.yaml` `timeout_seconds`. qa-catalog parses it as a plain `u64`
/// (`qa-catalog/src/domain/parsing/plan_yaml.rs:55-56`) and legacy honours it
/// unbounded (`manager/src/services/argo.rs:539`,
/// `"activeDeadlineSeconds": plan.plan.timeout_seconds`), so `RunKind::Plan`
/// forwards it and `RunKind::CustomPlan` takes it as a `max`. An absurd value
/// there still saturates, `checked_add` still answers `None`, and the run still
/// has no deadline the timeout sweep can act on.
///
/// It is left alone on purpose, and the reason is the threat model rather than
/// laziness: that value is authored by whoever can push to the test repository —
/// who can already run arbitrary destructive tests — not by a launch caller
/// holding only `qa.run`/`create`. Clamping it would be a parity divergence in a
/// chain this task did not introduce, whereas the launch override *is* the surface
/// this port added. **Recorded for the coordinator as an open item, not claimed as
/// closed.**
#[must_use]
pub fn saturating_i64(seconds: u64) -> i64 {
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! The arithmetic of the three chains and the clamp, with **every expected
    //! value read out of the source system** rather than recorded from running
    //! this code.
    //!
    //! The wiring these feed — that the resolved number actually reaches the run
    //! row's `timeout_at` — is pinned separately in
    //! `domain::service::launch`'s tests, which is the only place a whole launch
    //! can be driven. Neither set subsumes the other: these would pass if nothing
    //! ever called them, and those would pass on wrong arithmetic.
    use time::OffsetDateTime;

    use super::*;

    #[test]
    fn an_explicit_timeout_override_wins() {
        for kind in [
            RunKind::Plan,
            RunKind::Test,
            RunKind::CustomPlan,
            RunKind::Collect,
        ] {
            assert_eq!(
                resolve_timeout_seconds(Some(42), Some(7200), 900, kind),
                42,
                "{kind:?}"
            );
        }
    }

    /// `manager/src/services/argo.rs:539`: a plan run's deadline is
    /// `plan.plan.timeout_seconds`, directly.
    #[test]
    fn the_plans_timeout_is_used_when_no_override_is_given() {
        assert_eq!(
            resolve_timeout_seconds(None, Some(7200), 900, RunKind::Plan),
            7200
        );
    }

    /// **This pins a defensive arm that no production input reaches, said so rather
    /// than left to be discovered.**
    ///
    /// `resolve_timeout_seconds`' `RunKind::Plan` arm falls through to the configured
    /// default only when `plan_seconds` is `None`, and a *discovered* plan never
    /// arrives that way: qa-catalog's parser applies a serde default of 300
    /// (`qa-catalog/src/domain/parsing/plan_yaml.rs:42` for the constant, `:55-56` for
    /// the `#[serde(default)]`) and `to_sdk_plan` emits `Some(parsed.timeout_seconds)`
    /// unconditionally (`qa-catalog/src/domain/service/plans.rs:258-260`). So both this
    /// arm and the `300` arm in `a_zero_configured_default_falls_back_to_the_per_kind_default`
    /// are reachable only from a future plan source that stops defaulting the field.
    ///
    /// They are kept rather than deleted because `manager/src/services/argo.rs:168-181`
    /// is the chain they mirror and removing them would make restoring the
    /// fall-through a re-derivation. The production-reachable path through the same
    /// configured default is `RunKind::Test`, which
    /// `a_single_test_ignores_the_plans_timeout` and
    /// `the_configured_default_reaches_the_run_deadline` cover.
    #[test]
    fn the_configured_default_is_used_when_the_plan_declares_none() {
        assert_eq!(
            resolve_timeout_seconds(None, None, 900, RunKind::Plan),
            900,
            "manager/src/services/argo.rs:168-181 honours a configured default"
        );
    }

    /// `argo.rs:174` (`Ok(Some(cfg)) if cfg.default_timeout_seconds > 0`): a zero
    /// configured default is not a zero deadline, it is "unset".
    #[test]
    fn a_zero_configured_default_falls_back_to_the_per_kind_default() {
        assert_eq!(
            resolve_timeout_seconds(None, None, 0, RunKind::Plan),
            300,
            "plan.yaml's own serde default, manager/src/models.rs:11-12 declaring it \
             and :29-31 defining it (:28 is blank)"
        );
        assert_eq!(
            resolve_timeout_seconds(None, None, 0, RunKind::Test),
            1800,
            "manager/src/services/argo.rs:746"
        );
        assert_eq!(
            resolve_timeout_seconds(None, None, 0, RunKind::CustomPlan),
            3600,
            "manager/src/services/argo.rs:1065"
        );
    }

    /// `argo.rs:746`: a single test takes `default_timeout_seconds(1800)` and the
    /// plan's own value is never consulted. Unifying the three chains would give a
    /// single test from a two-hour plan a two-hour deadline, which the source system
    /// never grants.
    #[test]
    fn a_single_test_ignores_the_plans_timeout() {
        assert_eq!(
            resolve_timeout_seconds(None, Some(7200), 900, RunKind::Test),
            900
        );
        assert_eq!(
            resolve_timeout_seconds(None, Some(7200), 0, RunKind::Test),
            1800
        );
    }

    /// `argo.rs:1063-1065`: "Use the summed core-plan timeout when it exceeds the
    /// configured default; otherwise fall back to the default as a floor." The
    /// default is a **floor**, not a fallback, so first-wins would be wrong.
    #[test]
    fn a_custom_plan_takes_the_max_of_its_own_timeout_and_the_default() {
        assert_eq!(
            resolve_timeout_seconds(None, Some(7200), 900, RunKind::CustomPlan),
            7200,
            "the composed sum exceeds the default"
        );
        assert_eq!(
            resolve_timeout_seconds(None, Some(60), 900, RunKind::CustomPlan),
            900,
            "the default is a floor under a short composed sum"
        );
        assert_eq!(
            resolve_timeout_seconds(None, None, 0, RunKind::CustomPlan),
            3600
        );
    }

    /// **An unbounded caller timeout must not mint an unreclaimable run.**
    ///
    /// `LaunchRequest::timeout_seconds` is `Option<u64>` and caller-supplied, and it
    /// used to be returned outright: `u64::MAX` saturated to `i64::MAX`,
    /// `OffsetDateTime::checked_add` answered `None`, and the run got
    /// `timeout_at = NULL` — which `list_timeout_candidates` excludes by construction
    /// (*"a run with no deadline never times out"*), so the control-plane sweep could
    /// never reclaim it, and an exclusive one held the platform's global lease
    /// indefinitely.
    #[test]
    fn an_unbounded_caller_timeout_is_clamped_to_the_ceiling() {
        for kind in [
            RunKind::Plan,
            RunKind::Test,
            RunKind::CustomPlan,
            RunKind::Collect,
        ] {
            assert_eq!(
                resolve_timeout_seconds(Some(u64::MAX), Some(7200), 900, kind),
                MAX_LAUNCH_TIMEOUT_SECONDS,
                "{kind:?}"
            );
        }
        assert_eq!(
            MAX_LAUNCH_TIMEOUT_SECONDS, 604_800,
            "seven days; if this ceiling moves, say why in its doc comment"
        );
    }

    /// The property the clamp exists for, asserted directly rather than inferred from
    /// the number: the clamped value still lands inside the calendar, so `timeout_at`
    /// is `Some` and the sweep can see it.
    #[test]
    fn a_clamped_caller_timeout_still_lands_inside_the_calendar() {
        let seconds = resolve_timeout_seconds(Some(u64::MAX), None, 0, RunKind::Test);
        let deadline =
            OffsetDateTime::now_utc().checked_add(time::Duration::seconds(saturating_i64(seconds)));
        assert!(
            deadline.is_some(),
            "a NULL timeout_at is invisible to the timeout sweep; the whole point of \
             the clamp is that this is `Some`"
        );
        // And the unclamped input is what it protects against, so the fallback arm is
        // shown to be the dangerous one rather than merely asserted to be.
        assert!(
            OffsetDateTime::now_utc()
                .checked_add(time::Duration::seconds(saturating_i64(u64::MAX)))
                .is_none(),
            "unclamped, the deadline falls outside the calendar and becomes NULL"
        );
    }

    /// A collect run's deadline is legacy's synthetic 600 and consults nothing
    /// else.
    ///
    /// The collect job builds its own `TestPlanInfo` with
    /// `timeout_seconds: 600` (`manager/src/services/collect.rs:106`) and hands
    /// it to the plan submit path, which forwards a plan's value verbatim
    /// (`manager/src/services/argo.rs:539`) — so neither
    /// `runner_defaults.default_timeout_seconds` nor any plan the caller could
    /// name is ever consulted.
    ///
    /// Both non-override inputs are varied here rather than passed as `None`,
    /// because passing `None` would make the assertion true for an arm that
    /// *did* consult them. The launch override is checked separately: it wins
    /// for every kind, including this one, in
    /// `an_explicit_timeout_override_wins`.
    #[test]
    fn a_collect_run_takes_the_legacy_constant_and_ignores_both_other_inputs() {
        for (plan_seconds, configured) in [
            (None, 0),
            (None, 900),
            (Some(7200), 0),
            (Some(7200), 900),
            (Some(1), 1),
        ] {
            assert_eq!(
                resolve_timeout_seconds(None, plan_seconds, configured, RunKind::Collect),
                600,
                "plan_seconds={plan_seconds:?} configured={configured}"
            );
        }
        assert_eq!(kind_timeout_fallback(RunKind::Collect), 600);
    }

    /// `Some(0)` is not "no deadline": unclamped it produced `timeout_at = Some(now)`,
    /// so the next control-plane tick cancelled a run that had only just launched.
    #[test]
    fn a_zero_caller_timeout_is_raised_to_the_floor() {
        for kind in [
            RunKind::Plan,
            RunKind::Test,
            RunKind::CustomPlan,
            RunKind::Collect,
        ] {
            assert_eq!(
                resolve_timeout_seconds(Some(0), Some(7200), 900, kind),
                MIN_LAUNCH_TIMEOUT_SECONDS,
                "{kind:?}"
            );
        }
        assert_eq!(
            MIN_LAUNCH_TIMEOUT_SECONDS, 1,
            "the floor has to be above zero or the clamp changes nothing; one second \
             is the smallest value that is still a deadline"
        );
    }
}
