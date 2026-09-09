//! Typed configuration for the `qa-insights` gear (YAML section `qa-insights`).
//!
//! # Where each knob goes
//!
//! [`crate::gear`]'s `init` deserializes this once (`ctx.config_or_default()`)
//! and distributes every value; nothing else reads it.
//!
//! **Two of these are consumed as of Task 16, and this table said otherwise
//! until its fix round.** It read "as of Task 9 it is stored and nothing more —
//! the table below is therefore a *forecast*", which is precisely the error it
//! cited qa-runs for making in the other direction: describing the state of
//! wiring in another file as something other than what it is. The forecast
//! column is now split from what is live.
//!
//! | Knob | Consumer | State |
//! |---|---|---|
//! | `reconcile_lookback_seconds` | `AppServices` → `ReconcileService`, via `gear::reconcile_lookback` | **live** (Task 16), and **clamped** — see below |
//! | `reconcile_page_size` | `AppServices` → `ReconcileService`; bounds both the sweep's page and the rebuild's window | **live** (Task 16), unclamped |
//! | `reconcile_interval_seconds` | the reconciler ticker, via `gear::Cadence::reconciler` | **live** (Task 40); `0` disables |
//! | `default_collect_branch` | `AppServices` → `analytics::AnalyticsService` and, since Task 30, `collect::CollectService`, via `ServiceDeps::default_collect_branch` | **live** (Task 29) |
//! | `collect_report_base_url` | `AppServices` → `collect::CollectService`, via `ServiceDeps::collect_report_base_url` | **live** (Task 30); `gear::init` warns once if empty (fix round 1) |
//! | `collect_report_signing_secret` | `AppServices` → `collect::CollectService`, via `ServiceDeps::collect_report_signing_secret` | **live** (Task 30 fix round 1); `gear::init` warns once if empty or shorter than `collect::MIN_SIGNING_SECRET_LEN` once trimmed (Phase B fix wave, Finding 1 — the boot check now shares `collect::signing_secret_is_configured` with the request-time refusal so the two cannot drift apart again) |
//! | `collect_interval_seconds` | the collect ticker, via `gear::Cadence::collect` | **live** (Task 40); `0` disables, and non-zero is floored at [`MIN_COLLECT_INTERVAL_SECONDS`] by [`QaInsightsConfig::effective_collect_interval_seconds`] |
//! | `jira_poller_interval_seconds` | the JIRA poller ticker, via `gear::Cadence::jira_poller` | **live** (Task 40, which also added the field); `0` disables |
//! | `enable_tickers` | all three tickers, in `gear::QaInsights::serve` | **live** (Task 40) |
//! | `max_page_size` | the analytics collection reads | forecast — Tasks 24-27 (**not** Task 17; see below). **That forecast has expired**: Tasks 24-27 have all shipped and none consumes this knob (Phase B fix wave, Finding 2) — whether and how to bound these reads is a product/NFR decision under `cpt-cf-qa-nfr-scale`, escalated to a human rather than decided here, so this field is left unwired |
//!
//! Both live values are resolved to their final form in `init` and handed to the
//! services, so nothing re-reads a knob per request.
//!
//! # There is one ceiling, and it is **not** here — read this before adding an
//! # accessor
//!
//! `reconcile_lookback_seconds` already has a ceiling: `MAX_LOOKBACK_SECONDS`
//! (ten years) and `reconcile_lookback` in [`crate::gear`], which clamp it and
//! WARN once at `init`. It lives there rather than here because that is where the
//! value stops being a number and becomes a `time::Duration` — and the reason it
//! exists at all is that the sweep computes `watermark - lookback`, where
//! `OffsetDateTime`'s `Sub` **panics** on overflow.
//!
//! **Do not add a clamping accessor for that knob here.** A second ceiling is a
//! second answer to one question, and an accessor re-emits its WARN on every call
//! — which for a ticker is every pass, the exact failure the discipline below
//! exists to avoid. Task 16 landed the ceiling and this pointer together
//! precisely so Task 40 does not read the section below, conclude that no clamp
//! exists, and add one.
//!
//! ## No `effective_*` accessors here, and the reason is not "later"
//!
//! qa-runs owns floor constants and clamping accessors in its own `config.rs`
//! ([`crate`]-external: `qa-runs/src/config.rs`,
//! `MIN_DISPATCHER_INTERVAL_SECONDS` and friends), and legacy has a floor for
//! the one knob this module shares with it — `start_collect_poller` clamps with
//! `interval_secs.max(300)` (`manager/src/services/collect.rs:184`) and the
//! environment read applies the same 300 (`manager/src/main.rs:274`, whose
//! `env_u64(name, default, min)` ends in `.max(min)`, `main.rs:24-30`).
//!
//! **Task 40 ported that floor, and this section is now about one accessor
//! rather than none.** The paragraph this replaces said the floor was "not
//! ported here yet, deliberately", because "a clamping accessor warns when it
//! clamps, and the discipline qa-runs documents around those accessors — read
//! once at init, store the result, never re-read in the serve loop — only exists
//! once there *is* a serve loop". There is one now, so
//! [`QaInsightsConfig::effective_collect_interval_seconds`] exists and
//! `gear::Cadence::collect` calls it exactly once, at `init`, storing the result
//! on the runtime. Nothing in `serve` can re-read it, and the type is what makes
//! that true rather than the discipline: `Cadence` carries a resolved `u64`, and
//! `serve` never sees a [`QaInsightsConfig`] at all.
//!
//! **There is deliberately no accessor for the other two intervals.**
//! `reconcile_interval_seconds` and `jira_poller_interval_seconds` are floored at
//! nothing, because a floor is a ported rule and neither has one to port: the
//! reconcile sweep has no legacy counterpart at all (see that field's own doc),
//! and legacy's JIRA poller clamps with `.max(1)` on the **per-tenant**
//! `JiraPollerConfig.poll_interval_seconds` (`manager/src/services/jira_poller.rs:16-31`),
//! which this gear applies where legacy applies it —
//! `JiraService::poller_config`, not here. Inventing a floor for either would be
//! this module deciding a policy no source states. `0` disables each, which is
//! the only special value any of the three has.
//!
//! **Narrowed in Task 16's fix round.** This paragraph used to say clamping as
//! such "belongs to Task 40". One clamp had landed by then, in [`crate::gear`],
//! for a knob Task 16 consumes; the collect floor landed in Task 40. Where a
//! value is clamped is settled by where it is first consumed, and this module is
//! the place for exactly one of the three.

use serde::Deserialize;

/// Typed configuration for the qa-insights gear (YAML section `qa-insights`).
///
/// `#[serde(default)]` so an absent section is the default configuration, and
/// `deny_unknown_fields` so a misspelled knob is a boot failure rather than a
/// silently ignored line — the pair the sibling gears use.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QaInsightsConfig {
    /// How often the reconciler sweeps for finished runs.
    ///
    /// **This is exactly a latency budget**: the reconcile sweep is this gear's
    /// only ingest path (`crate::gear`'s header, "Event ingest, and why there is
    /// only one path" — the transactional broker consumer this ticker used to
    /// back up was deleted once it was established that no deployment ever ran
    /// it). A finished run appears one sweep after it finishes, and a tenant
    /// with no result rows yet appears not at all until an operator replays one
    /// window through `POST /qa/v1/insights/rebuild`.
    ///
    /// **New, with no legacy counterpart.** Legacy's manager owns the runs and
    /// the results in one process and projects them in the same transaction, so
    /// it has nothing to reconcile. This gear's split from qa-runs is why a
    /// sweep exists at all: it reads qa-runs through `QaRunsReader` rather than
    /// sharing a process and a transaction with it.
    pub reconcile_interval_seconds: u64,
    /// How far back a sweep looks beyond its own watermark.
    ///
    /// Covers a run that finished before an earlier sweep's cutoff but was
    /// written after it. Also new; same reason as above.
    pub reconcile_lookback_seconds: u64,
    /// Rows per reconciler page.
    pub reconcile_page_size: u32,
    /// Legacy's `DEFAULT_COLLECT_BRANCH` (`manager/src/services/collect.rs:19`).
    ///
    /// Kept as `main` so the default expected-cases lookup lines up with what
    /// the hourly cycle writes. Legacy's own comment on that constant says the
    /// same thing in the same words (`collect.rs:15-18`): it is the branch the
    /// hourly poller collects and the one the Analytics trigger uses when it is
    /// invoked without an explicit branch.
    pub default_collect_branch: String,
    /// Base URL at which the test runner can reach **this gear's own**
    /// `POST /qa/v1/collect/{repo_id}` route — the scheme-and-host half of
    /// the collect callback [`crate::domain::service::collect::CollectService`]
    /// hands the runner through qa-runs' `VHP_COLLECT_URL` (D2).
    ///
    /// Legacy's `MANAGER_INTERNAL_URL` (`manager/src/main.rs:46`), read from
    /// the environment with a same-cluster default —
    /// `format!("http://vhp-test-manager.{NAMESPACE}.svc.cluster.local:8080",
    /// ..)` (`manager/src/main.rs:65-70`).
    ///
    /// # No default is provided here, and that is deliberate
    ///
    /// This gear does not know its own Kubernetes namespace or Service name at
    /// compile time the way legacy's single binary could compute one from its
    /// own `NAMESPACE` environment variable — inventing a guess (`"qa-insights"`
    /// in some assumed namespace) would be a plausible-looking wrong default
    /// that fails only when a runner actually tries to call back, which is the
    /// worst time to discover it. `String`'s own `Default` — `""` — is what
    /// `#[serde(default)]` falls back to for an absent field.
    ///
    /// # Corrected in fix round 1: an empty value does **not** fail loudly on
    /// # its own
    ///
    /// An earlier revision of this doc claimed a blank prefix "fails loudly at
    /// the first collect cycle" — checked against the actual call path in that
    /// review and found false. `collect_url` still builds a syntactically
    /// valid relative path (`/qa/v1/collect/{repo_id}?...`), the launch to
    /// qa-runs still returns `Ok`, `run_collect_cycle`'s `launched` count still
    /// increments, and `POST /qa/v1/analytics/collect` still answers `200`.
    /// The only party that ever sees a failure is the runner, mid-job, trying
    /// to `POST` to a URL with no scheme or host — and it has no path back to
    /// report that failure anywhere this gear's own logs would show it. That
    /// is exactly the silent-failure shape this crate's own documentation
    /// culture warns against elsewhere.
    ///
    /// **What actually makes this loud**: `gear::QaInsights::init` warns once,
    /// at startup, when this field is empty (`reconcile_lookback_seconds`'s own
    /// clamp-and-warn is the in-repo precedent for a check living at `init`
    /// rather than on a per-call accessor). That WARN is the operator-visible
    /// signal this field's absence produces; nothing about the field's own
    /// `Default` makes the failure loud by itself.
    pub collect_report_base_url: String,
    /// The HMAC-SHA256 key [`crate::domain::service::collect::CollectService::sign`]
    /// and `::verify_signature` share — fix round 1's Critical 1 fix, and (fix
    /// round 2 corrected) this deployment's **actual, live access control** on
    /// `POST /qa/v1/collect/{repo_id}`, not a defence behind a platform gap:
    /// that route is anonymously reachable on the host this gear is actually
    /// deployed on (`domain::service::collect`'s header, "`.public()`
    /// genuinely exempts this route"), so this secret is the only thing
    /// standing between any caller and a write claiming an arbitrary tenant.
    /// See `domain::service::collect`'s header for the full argument.
    ///
    /// No legacy counterpart: legacy's callback carries no tenant at all, so it
    /// has nothing analogous to authenticate.
    ///
    /// # Also no default, and the consequence is different from
    /// # `collect_report_base_url`'s
    ///
    /// An empty secret is not merely inert the way an empty base URL is
    /// syntactically wrong-but-harmless-to-others: an empty HMAC key is a
    /// *publicly known* key, so treating it as a working secret would be
    /// worse than having none. `CollectService::verify_signature` therefore
    /// refuses every report outright when this is empty **or shorter than**
    /// `collect::MIN_SIGNING_SECRET_LEN` (fail-closed, checked before any
    /// cryptography runs), rather than computing an HMAC under a key any
    /// reader of this source file already knows or a value too short to
    /// resist guessing. `gear::QaInsights::init` warns once at startup for
    /// this field too, for the identical operator-visibility reason
    /// `collect_report_base_url`'s does.
    ///
    /// # This is this gear's first secret-valued config field, and the
    /// # config-dump surface has not been taught to redact it
    ///
    /// `libs/toolkit/src/bootstrap/config/dump.rs`'s `--dump-gears-config-yaml`
    /// serializes this whole config struct, and only the database DSN's
    /// password is special-cased for redaction there — a plain `String`
    /// field with no such carve-out renders **verbatim** in that dump.
    /// `domain::service::collect::CollectReportSigningSecret`'s own hand-written
    /// `Debug` impl (fix round 2) redacts this value in this gear's *own* Rust
    /// code — a panic message, a stray `tracing::debug!` — but that type does
    /// not exist yet when this raw config value is deserialized, and the dump
    /// surface serializes the config struct directly, never through it. **Not
    /// fixed here**: patching `libs/toolkit`'s dump surface to redact an
    /// arbitrary gear-declared secret field is a shared-infrastructure change
    /// (it would need a way for a gear to *declare* which of its own fields
    /// are secret, which does not exist), out of this task's scope in the
    /// same way fix round 1's `.public()`/`PublicRoute` question was.
    /// Recorded here instead, at the field an operator would otherwise
    /// discover this about the hard way: **running
    /// `--dump-gears-config-yaml` against a deployment prints this secret in
    /// clear text**, and anyone who can run that command or read its output
    /// can therefore forge collect reports for any tenant. Treat the dump
    /// output the same way you would treat the secret itself.
    pub collect_report_signing_secret: String,
    /// Legacy's hourly collect cycle.
    ///
    /// **Citation corrected, 2026-08-20.** The plan cites
    /// `manager/src/services/collect.rs:183-188` for this default. That range
    /// is real and is the poller's body — `start_collect_poller` is at `:183`
    /// and its `run_collect_cycle` call at `:188` — but it holds neither the
    /// word "hourly" nor the number 3600. Both are elsewhere:
    ///
    /// * `collect.rs:181-182` — the doc comment, *"Hourly background poller
    ///   that re-collects exact case counts for the default branch."*
    /// * `main.rs:274` — `env_u64("COLLECT_POLLER_INTERVAL_SECONDS", 3600,
    ///   300)`, which is where the 3600 default this field copies actually
    ///   lives, together with the 300-second floor discussed in the module
    ///   header.
    pub collect_interval_seconds: u64,
    /// How often the JIRA poller ticker makes a pass over every tenant's open
    /// bugs. `0` disables the ticker.
    ///
    /// **Added by Task 40**, which is the task that starts the loop; nothing
    /// before it had a cadence to configure.
    ///
    /// # It is a process-level knob where legacy's is per-tenant, and that is a
    /// # decision
    ///
    /// Legacy's `start_jira_poller` re-reads
    /// `JiraPollerConfig.poll_interval_seconds` **inside its loop** and sleeps
    /// for that long (`manager/src/services/jira_poller.rs:16-31`), which works
    /// because legacy has exactly one tenant. This gear's poller passes over
    /// every tenant the tickers know about, so "the interval" is no longer a
    /// single tenant's property: honouring N per-tenant intervals from one loop
    /// needs N timers and a per-tenant last-pass instant, which is state no table
    /// here holds.
    ///
    /// So the *cadence* is this knob and the default is legacy's own default,
    /// 300 seconds (`manager/src/models.rs:1422-1429`,
    /// `JiraPollerConfig::default`). The per-tenant
    /// `qa_jira_poller_config.poll_interval_seconds` column keeps its meaning for
    /// everything else it gates — it is still what `PUT
    /// /qa/v1/settings/jira-poller` writes and what `JiraService::poller_config`
    /// clamps with `.max(1)` — but **no per-tenant timer reads it**, and that is
    /// stated here rather than left for a reader to infer from an idle column.
    /// Giving it one is a feature (a `last_polled_at` per tenant, or a
    /// per-tenant task), not a wiring gap.
    pub jira_poller_interval_seconds: u64,
    /// Whether this instance runs the tickers at all.
    ///
    /// An operator running a read-only replica sets this false. One switch for
    /// all three tickers; the per-ticker switches are the intervals, where `0`
    /// disables. Both halves are live as of Task 40: `gear::QaInsights::serve`
    /// reads this once and each `gear::Cadence` reads its own interval once.
    ///
    /// Legacy's nearest equivalent is per-poller and environment-driven —
    /// `COLLECT_POLLER_ENABLED`, default true (`manager/src/main.rs:273`) — so
    /// the default here is `true` for parity with a legacy deployment that sets
    /// nothing.
    pub enable_tickers: bool,
    /// Max rows any analytics query returns before paging.
    ///
    /// **Not a subsystem-shared constant, despite the plan's phrasing.** The
    /// plan's spec-coverage table describes `cpt-cf-qa-nfr-scale` as "`OData`
    /// paging with the subsystem's shared clamp"; there is no such shared
    /// clamp — grepping `gears/qa-platform` for `max_page_size`, `max_top` and
    /// `page_size` finds no sibling knob and no shared constant (verified
    /// 2026-08-20).
    ///
    /// # Task 17 reconciled this knob with the platform ceiling, and the answer
    /// # was "there is no platform ceiling"
    ///
    /// This doc used to say *"the platform's ceiling is `toolkit_odata`'s
    /// `Limits::with_max_top`, which is a different mechanism applied at the
    /// route"*, and asked Task 17 to reconcile the two. **`ODataLimits` is applied
    /// nowhere.** It is `pub use`d from `toolkit-odata`
    /// (`libs/toolkit-odata/src/lib.rs:14`) and `with_max_top` has no caller in
    /// this workspace outside its own module's tests — and it could not be in this
    /// path anyway: the `OData` extractor never constructs one
    /// (`libs/toolkit/src/api/odata.rs`, `extract_odata_query`, which validates a
    /// `$top` of zero and nothing else about the limit). Measured 2026-08-20 by
    /// grep over `libs/` and `gears/`. So there was no second number to reconcile
    /// with; the only ceiling in the `OData` path is `LimitCfg` inside
    /// `paginate_odata`.
    ///
    /// # It is therefore **not** what the two `OData` collections clamp with
    ///
    /// Task 17's collections use `infra::storage::db::PAGE_LIMITS` — a `const`
    /// 200/500, matching qa-runs, because the plan's stated convention is one
    /// page-size rule across the subsystem's collections. Making that ceiling a
    /// per-deployment knob instead would give the subsystem two rules and make the
    /// number the `OpenAPI` description quotes deployment-dependent; that
    /// constant's own doc carries the full argument.
    ///
    /// This field's *own* description is "max rows any analytics query returns
    /// before paging", and the analytics reads (Tasks 24-27) are non-`OData`
    /// aggregate endpoints with no `LimitCfg` near them. It is re-forecast to them
    /// rather than consumed by Task 17, which is a decision and not an oversight.
    ///
    /// **Phase B fix wave, Finding 2: that forecast has now expired.** Tasks
    /// 24-27 have all shipped — `overview`'s `lists`, `build-tests`,
    /// `plan/tests`, `plan/builds`, `plan/test-history` and the CSV export
    /// all ship as unbounded arrays over their read window, and none of them
    /// reads this field. Whether and how to bound them is a product/NFR
    /// decision under `cpt-cf-qa-nfr-scale`; it is being escalated to a human
    /// rather than decided in this wave, so this field stays unwired and
    /// pagination is **not** being added here.
    pub max_page_size: u32,
}

/// Legacy's floor on the collect poller's interval: five minutes.
///
/// Two independent places in legacy apply the same 300 —
/// `start_collect_poller`'s `interval_secs.max(300)`
/// (`manager/src/services/collect.rs:184`) and the environment read that feeds
/// it, `env_u64("COLLECT_POLLER_INTERVAL_SECONDS", 3600, 300)`
/// (`manager/src/main.rs:274`, whose `env_u64(name, default, min)` ends in
/// `.max(min)`, `main.rs:24-30`). Ported as one constant because they are one
/// rule.
///
/// **It floors, it does not disable.** `0` is not clamped up to 300; `0` means
/// the operator switched the ticker off, and turning that into a five-minute
/// cycle would be the config module overruling them.
pub const MIN_COLLECT_INTERVAL_SECONDS: u64 = 300;

impl QaInsightsConfig {
    /// `collect_interval_seconds`, floored at [`MIN_COLLECT_INTERVAL_SECONDS`],
    /// warning once when it floors.
    ///
    /// # Call this exactly once, at `init`
    ///
    /// It warns, and a warning re-emitted from a serve loop is a warning per
    /// tick. `gear::Cadence::collect` is the one caller and `gear::init` the one
    /// place it runs; the resolved `u64` is what reaches `serve`. This is the
    /// discipline qa-runs' `QaRunsRuntime` doc states and the reason
    /// `reconcile_lookback` lives in [`crate::gear`] rather than here — see this
    /// module's header.
    #[must_use]
    pub fn effective_collect_interval_seconds(&self) -> u64 {
        // Zero first: it is "off", not "too small". See
        // `MIN_COLLECT_INTERVAL_SECONDS`.
        if self.collect_interval_seconds == 0 {
            return 0;
        }
        if self.collect_interval_seconds < MIN_COLLECT_INTERVAL_SECONDS {
            tracing::warn!(
                configured_seconds = self.collect_interval_seconds,
                floored_to_seconds = MIN_COLLECT_INTERVAL_SECONDS,
                "collect_interval_seconds is below the floor legacy applies \
                 (manager/src/services/collect.rs:184); using the floor. Set it to 0 to switch \
                 the collect ticker off instead.",
            );
            return MIN_COLLECT_INTERVAL_SECONDS;
        }
        self.collect_interval_seconds
    }
}

impl Default for QaInsightsConfig {
    fn default() -> Self {
        Self {
            reconcile_interval_seconds: 300,
            reconcile_lookback_seconds: 3600,
            reconcile_page_size: 200,
            default_collect_branch: "main".to_owned(),
            collect_report_base_url: String::new(),
            collect_report_signing_secret: String::new(),
            collect_interval_seconds: 3600,
            jira_poller_interval_seconds: 300,
            enable_tickers: true,
            max_page_size: 200,
        }
    }
}
