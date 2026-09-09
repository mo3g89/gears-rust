//! Typed configuration for the `qa-environments` gear.
//!
//! # Why nothing here is behind a cargo feature
//!
//! [`QaEnvironmentsConfig`] is `#[serde(default, deny_unknown_fields)]`, and
//! `deny_unknown_fields` plus a `#[cfg(feature = …)]` field is a latent boot
//! failure: the same config file that a `runner-secret` build accepts
//! is *rejected outright* by a build without the feature, with a message
//! naming a field the operator did in fact write and that the product does in
//! fact have. A deployment cannot then be rolled back to a default binary
//! without also editing its config — which is exactly the moment nobody wants
//! to be editing config.
//!
//! The 2026-08-28 final review flagged that combination, latent only because
//! the shipped stack rendered `qa-environments: config: {}`. It is not latent
//! any more: `config/qa-platform-stack.yaml` now carries an anchor under that
//! key and an `--argo` deployment inserts `argo.kubeconfig_path` there.
//!
//! The resolution keeps both properties instead of trading one for the other.
//! **The config shape is the same in every build** — every field parses
//! everywhere, so a config file is portable across binaries and
//! `deny_unknown_fields` still catches a genuine typo — while **the feature
//! still decides what is built**: these structs name no Kubernetes type (they
//! are `String`/`bool`/`u64`), so keeping them costs a default build nothing
//! and breaks no part of ADR-0001's waiver. What is feature-gated is the
//! *reader*: `crate::gear::build_observer` and the ticker spawn, which are the
//! things that actually need `kube`.
//!
//! The one consequence, made explicit rather than left to be discovered: in a
//! build without `runner-secret` these fields are accepted and then
//! ignored. `crate::gear` already logs once at startup saying the observation
//! feature is absent, which is the signal that covers it.

use serde::Deserialize;

/// Typed configuration for the qa-environments gear (YAML section `qa-environments`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QaEnvironmentsConfig {
    /// Max variables returned per env-assembly query.
    pub max_variables: usize,

    /// How to reach the **Argo** cluster that
    /// `KubeRunnerSecretWriter::ensure_runner_secret` (D4) writes each runner
    /// `Secret` into — a different cluster from any environment's own. Only
    /// *meaningful* in a build carrying
    /// the `runner-secret` cargo feature — `crate::gear::build_runner_secret_writer`
    /// is the sole reader — but present and parseable in every build, so one
    /// config file works against either binary. See this module's header.
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

/// Connection details for the Argo cluster D4's `Secret` writer targets.
/// Mirrors `qa-runs`' `ArgoExecutorConfig` shape (`qa-runs/src/config.rs`),
/// deliberately: both gears reach the same Argo cluster from the same
/// deployment shapes (docker-compose with a mounted kubeconfig, or in-cluster
/// with ambient service-account credentials), so there is no reason for the
/// two knobs to disagree on what "unset" means.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArgoObserverConfig {
    /// Path to a kubeconfig naming the Argo cluster. `None` (the default)
    /// falls back to ambient credentials — in-cluster service-account
    /// credentials, or `KUBECONFIG`/`~/.kube/config` — via `Config::infer()`,
    /// which is correct once qa-platform runs inside the cluster the way
    /// legacy does.
    pub kubeconfig_path: Option<String>,
    /// Namespace the runner's credential `Secret`s are written into — one per
    /// stored credential, since an environment's plugin may declare more than
    /// one. Default `argo`, matching `qa-runs`' own default and
    /// `deploy/argo/provision-platform-kubeconfig-secret.sh`.
    pub namespace: String,
    /// Prefix of the generated `Secret`'s name, before the sanitised
    /// credstore reference. Default `qa-platform-` — see
    /// `infra::runner_secret_writer`'s parity-oracle test module, which
    /// pins this string against `qa-runs`' `argo/naming.rs` and the
    /// provisioning script.
    pub secret_prefix: String,
    /// Key under which a credential's bytes are stored in the `Secret`'s
    /// `data` map. Default `value`.
    pub secret_key: String,
}

impl Default for ArgoObserverConfig {
    fn default() -> Self {
        Self {
            kubeconfig_path: None,
            namespace: "argo".to_owned(),
            secret_prefix: "qa-platform-".to_owned(),
            secret_key: "value".to_owned(),
        }
    }
}

/// The background observation ticker's settings (Task 8): `qa-environments.observation`.
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
    /// Whether the ticker runs at all. Independent of the cargo feature this
    /// struct is itself gated on: that feature decides whether the ticker
    /// *exists* (`crate::gear::QaEnvironments::serve` never spawns it at all
    /// in a build without `runner-secret`, logging once at startup to
    /// say so); this flag decides whether an operator who compiled the
    /// feature in still wants it running.
    pub enabled: bool,
    /// Seconds between cycles, before flooring. Read only through
    /// [`Self::effective_poll_interval_seconds`] — never raw — so this field
    /// itself carries no guarantee about its lower bound.
    pub poll_interval_seconds: u64,
}

impl Default for ObservationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_seconds: 300,
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
}

#[cfg(test)]
mod observation_config_tests {
    use super::{ObservationConfig, QaEnvironmentsConfig};

    /// Legacy clamps with `.max(60)` in both branches of its config load
    /// (`platform_version_poller.rs`'s `load_config`). A 1-second poll would
    /// hammer every registered cluster.
    /// The `deny_unknown_fields` + feature-gate trap, pinned in the build
    /// where it would have fired.
    ///
    /// This test compiles and runs in a **default** build (no
    /// `runner-secret`). Before 2026-08-28 the two fields it names
    /// existed only under the feature, so this exact document — the one an
    /// `--argo` deployment renders — made a default binary fail
    /// deserialization at boot with a message naming a field the operator had
    /// correctly written. Now it parses everywhere, and the feature decides
    /// only what is *done* with it.
    ///
    /// If a future change re-gates either field, this test stops compiling in
    /// the default build rather than the failure being discovered on a
    /// rollback.
    #[test]
    fn a_config_naming_the_observation_fields_parses_in_every_build() {
        let yaml = "
max_variables: 500
argo:
  kubeconfig_path: /etc/qa-platform/k3s-kubeconfig.yaml
  namespace: argo
observation:
  enabled: true
  poll_interval_seconds: 300
";
        let cfg: QaEnvironmentsConfig = serde_yaml::from_str(yaml)
            .expect("an --argo deployment's config must parse in ANY build");
        assert_eq!(
            cfg.argo.kubeconfig_path.as_deref(),
            Some("/etc/qa-platform/k3s-kubeconfig.yaml")
        );
        assert_eq!(cfg.argo.namespace, "argo");
        assert!(cfg.observation.enabled);
        assert_eq!(cfg.observation.poll_interval_seconds, 300);
        // The defaults still fill in what the document does not name, so
        // `deny_unknown_fields` is doing typo-catching and nothing else.
        assert_eq!(cfg.argo.secret_prefix, "qa-platform-");
        assert_eq!(cfg.argo.secret_key, "value");
    }

    /// The property `deny_unknown_fields` is kept for: a misspelled key is a
    /// refusal, not a silently ignored line.
    #[test]
    fn a_misspelled_key_is_still_rejected() {
        let yaml = "
argo:
  kubeconfig_paht: /etc/qa-platform/k3s-kubeconfig.yaml
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
            }
            .effective_poll_interval_seconds(),
            60
        );
        assert_eq!(
            ObservationConfig {
                enabled: true,
                poll_interval_seconds: 30,
            }
            .effective_poll_interval_seconds(),
            60
        );
        assert_eq!(
            ObservationConfig {
                enabled: true,
                poll_interval_seconds: 300,
            }
            .effective_poll_interval_seconds(),
            300
        );
    }
}
