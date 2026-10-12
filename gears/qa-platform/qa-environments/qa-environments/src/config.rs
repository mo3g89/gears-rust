//! Typed configuration for the `qa-environments` gear.
//!
//! Typed configuration for the `qa-environments` gear.
//!
//! # Why nothing here is behind a cargo feature
//!
//! [`QaEnvironmentsConfig`] is `#[serde(default, deny_unknown_fields)]`, and
//! `deny_unknown_fields` plus a `#[cfg(feature = ...)]` field is a latent boot
//! failure: the same config file that a `runner-credentials` build accepts
//! would be *rejected outright* by a build without the feature, with a message
//! naming a field the operator did in fact write. A deployment could then not
//! be rolled back to a default binary without also editing its config.
//!
//! So **the config shape is the same in every build**: every field parses
//! everywhere, and `deny_unknown_fields` still catches a genuine typo, while
//! the feature decides only what is built. These structs name no Kubernetes
//! type (they are `String`/`bool`/`u64`), so keeping them costs a default
//! build nothing and is consistent with ADR-0001. The reader of
//! [`QaEnvironmentsConfig::argo`] is feature-gated alone
//! (`crate::gear::build_pod_identity`); in a build without
//! `runner-credentials` its fields are accepted and then ignored.
//! [`QaEnvironmentsConfig::observation`] is read in every build, and the
//! observation ticker spawns unconditionally.

use serde::Deserialize;

/// Typed configuration for the qa-environments gear (YAML section `qa-environments`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QaEnvironmentsConfig {
    /// Max variables returned per env-assembly query.
    pub max_variables: usize,

    /// Where the runner pods run: the Argo namespace and the `ServiceAccount`
    /// their tokens are verified against. Only *meaningful* in a build
    /// carrying the `runner-credentials` cargo feature
    /// (`crate::gear::build_pod_identity` is the sole reader) but present and
    /// parseable in every build. See this module's header.
    pub argo: ArgoObserverConfig,

    /// The background observation ticker's own settings — how often it runs
    /// and whether it runs at all. Read once at `Gear::init` and handed to
    /// `QaEnvironments::serve`, which spawns (or, per
    /// [`ObservationConfig::enabled`], declines to spawn) the ticker that
    /// keeps every environment's detection fresh without anyone calling
    /// `POST /qa/v1/environments/{id}/refresh`.
    ///
    /// **Read in every build since Task 19b.** It used to be feature-gated for
    /// the same reason [`Self::argo`]'s reader is: observation needed a
    /// Kubernetes client, so a ticker in a build without one had nothing to do
    /// but log a failure every cycle, and `crate::gear` declined to spawn it.
    /// Task 15 moved observation to the product plugin, which needs no such
    /// client, so every build can run the ticker and the gate had nothing left
    /// to buy.
    pub observation: ObservationConfig,
}

impl Default for QaEnvironmentsConfig {
    fn default() -> Self {
        Self {
            max_variables: 500,
            argo: ArgoObserverConfig::default(),
            observation: ObservationConfig::default(),
        }
    }
}

/// Where the runner pods run, for the runner-credential route's pod-identity
/// verification (finding #94).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArgoObserverConfig {
    /// Namespace the runner pods run in. Default `argo`, matching `qa-runs`'
    /// own default.
    pub namespace: String,
    /// The `ServiceAccount` runner pods run as in [`Self::namespace`] (finding
    /// #94). `POST /qa/v1/runner-credentials` refuses a token whose reviewed
    /// username is not `system:serviceaccount:<namespace>:<this>`. Default
    /// `qa-platform-runner`, the chart's `argo.workflowServiceAccount`. Read
    /// only in a build carrying the `runner-credentials` feature.
    pub runner_service_account: String,
}

impl Default for ArgoObserverConfig {
    fn default() -> Self {
        Self {
            namespace: "argo".to_owned(),
            runner_service_account: "qa-platform-runner".to_owned(),
        }
    }
}

/// The background observation ticker's settings: `qa-environments.observation`.
///
/// Mirrors legacy's `PlatformVersionPollerConfig`
/// (`platform_version_poller.rs`): `enabled` is the *sole* on/off switch, and
/// `poll_interval_seconds` is only ever read through
/// [`Self::effective_poll_interval_seconds`], which floors it exactly as
/// legacy's `load_config` does in both of its branches (`Ok(Some(cfg))` and
/// `Ok(None)` alike apply `.max(60)`) — never treating a low value as a second
/// way to disable the ticker. That is a deliberate difference from `qa-runs`'
/// `dispatcher_enabled`/`dispatcher_interval_seconds` pair, where `0` *also*
/// disables: this gear has one poller, not two independently-switched ones,
/// and legacy's own poller draws the same one-switch line.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObservationConfig {
    /// Whether the ticker runs at all.
    ///
    /// **Not gated on any cargo feature** — neither this struct nor the
    /// ticker spawn is. `crate::gear::QaEnvironments::serve_with_services`
    /// spawns the ticker in every build whenever this flag is `true`; a
    /// build without `runner-credentials` observes exactly the same way a
    /// build with it does. This flag is the *only* on/off switch, for an operator
    /// who wants environments to stop refreshing on their own.
    pub enabled: bool,
    /// Seconds between cycles, before flooring. Read only through
    /// [`Self::effective_poll_interval_seconds`] — never raw — so this field
    /// itself carries no guarantee about its lower bound.
    pub poll_interval_seconds: u64,
    /// Seconds one environment's `observe` may run before the platform
    /// abandons it and records a `Timeout`, on the ticker and on
    /// `POST /qa/v1/environments/{id}/refresh` alike. Read only through
    /// [`Self::effective_observe_timeout`], which floors it at one second: a
    /// zero must not disable the bound, because an unbounded plugin call is
    /// the defect this exists for. Default 300 s, equal to the default poll
    /// interval and above VHI's worst legitimate path (agent start, up to
    /// 40 s, plus four sequential commands of up to 60 s each), so no
    /// legitimate observation is cut.
    pub observe_timeout_seconds: u64,
}

impl Default for ObservationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_seconds: 300,
            observe_timeout_seconds: 300,
        }
    }
}

impl ObservationConfig {
    /// Floor below which a cycle would hammer every registered cluster.
    /// Legacy's own constant (`platform_version_poller.rs`'s `.max(60)`,
    /// applied identically whether the value came from stored settings or
    /// from `load_config`'s own `Ok(None)` default).
    const MIN_POLL_INTERVAL_SECONDS: u64 = 60;

    /// [`Self::poll_interval_seconds`], floored at
    /// [`Self::MIN_POLL_INTERVAL_SECONDS`].
    #[must_use]
    pub fn effective_poll_interval_seconds(&self) -> u64 {
        if self.poll_interval_seconds < Self::MIN_POLL_INTERVAL_SECONDS {
            tracing::warn!(
                configured_seconds = self.poll_interval_seconds,
                floor_seconds = Self::MIN_POLL_INTERVAL_SECONDS,
                "qa-environments.observation.poll_interval_seconds is below the floor legacy \
                 applies; using the floor instead of hammering every registered cluster"
            );
        }
        self.poll_interval_seconds
            .max(Self::MIN_POLL_INTERVAL_SECONDS)
    }

    /// [`Self::observe_timeout_seconds`] as a deadline, floored at one second.
    #[must_use]
    pub fn effective_observe_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.observe_timeout_seconds.max(1))
    }
}

#[cfg(test)]
mod observation_config_tests {
    use super::{ObservationConfig, QaEnvironmentsConfig};

    /// The `deny_unknown_fields` + feature-gate trap, pinned in the build
    /// where it would have fired.
    ///
    /// This test compiles and runs in a **default** build (no
    /// `runner-credentials`): the document an `--argo` deployment renders must
    /// parse everywhere, and the feature decides only what is done with it.
    #[test]
    fn a_config_naming_the_observation_fields_parses_in_every_build() {
        let yaml = "
max_variables: 500
argo:
  namespace: argo
  runner_service_account: qa-runner
observation:
  enabled: true
  poll_interval_seconds: 300
  observe_timeout_seconds: 90
";
        let cfg: QaEnvironmentsConfig = serde_yaml::from_str(yaml)
            .expect("an --argo deployment's config must parse in ANY build");
        assert_eq!(cfg.argo.namespace, "argo");
        assert_eq!(cfg.argo.runner_service_account, "qa-runner");
        assert!(cfg.observation.enabled);
        assert_eq!(cfg.observation.poll_interval_seconds, 300);
        assert_eq!(cfg.observation.observe_timeout_seconds, 90);
        assert_eq!(
            cfg.observation.effective_observe_timeout(),
            std::time::Duration::from_secs(90)
        );
    }

    /// Unset, the runner account is the chart's default one.
    #[test]
    fn the_runner_service_account_defaults_to_the_charts() {
        assert_eq!(
            QaEnvironmentsConfig::default().argo.runner_service_account,
            "qa-platform-runner"
        );
    }

    /// The property `deny_unknown_fields` is kept for: a misspelled key is a
    /// refusal, not a silently ignored line.
    #[test]
    fn a_misspelled_key_is_still_rejected() {
        let yaml = "
argo:
  namespce: argo
";
        serde_yaml::from_str::<QaEnvironmentsConfig>(yaml)
            .expect_err("a typo must still be refused rather than ignored");
    }

    #[test]
    fn the_interval_has_a_sixty_second_floor() {
        assert_eq!(
            ObservationConfig {
                enabled: true,
                poll_interval_seconds: 0,
                ..ObservationConfig::default()
            }
            .effective_poll_interval_seconds(),
            60
        );
        assert_eq!(
            ObservationConfig {
                enabled: true,
                poll_interval_seconds: 30,
                ..ObservationConfig::default()
            }
            .effective_poll_interval_seconds(),
            60
        );
        assert_eq!(
            ObservationConfig {
                enabled: true,
                poll_interval_seconds: 300,
                ..ObservationConfig::default()
            }
            .effective_poll_interval_seconds(),
            300
        );
    }

    /// The deadline's default is the default poll interval, 300 s.
    #[test]
    fn the_observe_timeout_defaults_to_three_hundred_seconds() {
        assert_eq!(
            ObservationConfig::default().effective_observe_timeout(),
            std::time::Duration::from_mins(5)
        );
    }

    /// A zero must not disable the bound: it is floored at one second.
    #[test]
    fn a_zero_observe_timeout_is_floored_at_one_second() {
        assert_eq!(
            ObservationConfig {
                observe_timeout_seconds: 0,
                ..ObservationConfig::default()
            }
            .effective_observe_timeout(),
            std::time::Duration::from_secs(1)
        );
    }
}
