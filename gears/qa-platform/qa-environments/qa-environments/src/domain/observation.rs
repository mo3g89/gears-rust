//! Pure detection rules, ported from `manager/src/services/platforms.rs`.
//!
//! Deliberately free of Kubernetes types: these take the `ConfigMap`'s `data`
//! map, not a `ConfigMap`, so every rule is testable in a default build with
//! no `kube` in the tree (ADR-0001 waiver amendment, 2026-08-28).

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

/// One entry of the `vp-gateway-hostnames` `ConfigMap`.
#[derive(Debug, Deserialize)]
struct GatewayResolvedHost {
    host: String,
    #[serde(default)]
    visibility: String,
}

/// The key whose value is a bare cluster IP rather than JSON, and which must
/// therefore be skipped by name before any parse is attempted.
const GATEWAY_INTERNAL_CLUSTERIP_KEY: &str = "internal-gateway-clusterip";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPlatform {
    pub version: String,
    pub build: Option<String>,
    pub raw: String,
    pub namespace: String,
    pub base_domain: Option<String>,
}

/// Split a raw `platformVersion` into a version prefix and a numeric build
/// suffix. Splits on the last `.` **only** when the head still contains a `.`
/// (so `1.0` is preserved whole) and the tail is entirely ASCII digits.
#[must_use]
pub fn parse_platform_version(raw: &str) -> (String, Option<String>) {
    let trimmed = raw.trim();
    if let Some((head, tail)) = trimmed.rsplit_once('.')
        && head.contains('.')
        && !tail.is_empty()
        && tail.chars().all(|c| c.is_ascii_digit())
    {
        return (head.to_owned(), Some(tail.to_owned()));
    }
    (trimmed.to_owned(), None)
}

/// Build a detection from a `core-install-metadata` `data` map. `None` when
/// `platformVersion` is absent or blank — which is "keep scanning", not an error.
#[must_use]
pub fn detected_from_data(
    data: &BTreeMap<String, String>,
    namespace: &str,
    fallback_ns: &str,
) -> Option<DetectedPlatform> {
    let raw = data.get("platformVersion").filter(|v| !v.trim().is_empty())?.clone();
    let namespace = if namespace.trim().is_empty() { fallback_ns } else { namespace };
    let (version, build) = parse_platform_version(&raw);
    Some(DetectedPlatform {
        version,
        build,
        raw,
        namespace: namespace.to_owned(),
        base_domain: None,
    })
}

/// Externally-visible hostnames from a `vp-gateway-hostnames` `data` map.
/// Unparseable entries are skipped rather than failing the whole read.
#[must_use]
pub fn external_hosts(data: &BTreeMap<String, String>) -> Vec<String> {
    let mut hosts = BTreeSet::new();
    for (component, raw) in data {
        if component == GATEWAY_INTERNAL_CLUSTERIP_KEY {
            continue;
        }
        match serde_json::from_str::<GatewayResolvedHost>(raw) {
            Ok(entry) if entry.visibility == "external" => {
                hosts.insert(entry.host);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(component = %component, %error, "unparseable gateway hostname entry");
            }
        }
    }
    hosts.into_iter().collect()
}

/// The domain suffix shared by every externally-visible hostname.
///
/// vpadm never exposes a component at the bare base domain — every entry
/// carries a subdomain — so the domain can only be recovered by comparing two
/// or more hostnames. `None` means **inconclusive**, never an error, and a
/// caller must not overwrite a stored value with it.
#[must_use]
pub fn compute_base_domain(hosts: &[String]) -> Option<String> {
    let distinct: BTreeSet<&str> =
        hosts.iter().map(|h| h.trim()).filter(|h| !h.is_empty()).collect();
    if distinct.len() < 2 {
        return None;
    }
    let mut iter = distinct.into_iter();
    let mut common: Vec<&str> = iter.next()?.split('.').collect();
    for host in iter {
        let labels: Vec<&str> = host.split('.').collect();
        let shared = common
            .iter()
            .rev()
            .zip(labels.iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        common = common[common.len() - shared..].to_vec();
        if common.len() < 2 {
            return None;
        }
    }
    Some(common.join("."))
}

/// One node, as the cluster reported it. Pure: nothing here is a `k8s_openapi`
/// type, so every rule below is testable in a default build with no `kube` in
/// the tree (ADR-0001 waiver amendment, 2026-08-28).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeSummary {
    pub name: String,
    pub control_plane: bool,
    pub ready: bool,
    pub kubelet_version: Option<String>,
    pub os_image: Option<String>,
}

/// One cluster read.
///
/// `namespace_count` is `Option`, not `u32`: legacy folds a failed namespace
/// list into `unwrap_or(0)`, a zero indistinguishable from a cluster that
/// really has no namespaces. `None` here means "not read".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClusterHealth {
    pub nodes: Vec<NodeSummary>,
    pub namespace_count: Option<u32>,
}

/// Legacy's four derivable statuses.
///
/// **No `Unreachable` variant on purpose.** Unreachable is not a property of a
/// cluster that was read; it is the absence of a reading, and it lives in
/// `HealthOutcome::Failed`. A variant here would be one `status()` can never
/// return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Warning,
}

impl ClusterStatus {
    /// The stored and published spelling. These strings are a wire contract
    /// with `qa_platforms.cluster_status` and with the UI's dot palette.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "Healthy",
            Self::Degraded => "Degraded",
            Self::Unhealthy => "Unhealthy",
            Self::Warning => "Warning",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeCounts {
    pub total: u32,
    pub ready: u32,
    pub control_plane: u32,
    pub ready_control_plane: u32,
    pub worker: u32,
    pub ready_worker: u32,
}

impl ClusterHealth {
    /// Counts derived, never stored (D-CH-2): they cannot disagree with the
    /// node list because there is nothing to disagree with.
    #[must_use]
    pub fn counts(&self) -> NodeCounts {
        let count = |f: &dyn Fn(&NodeSummary) -> bool| -> u32 {
            u32::try_from(self.nodes.iter().filter(|n| f(n)).count()).unwrap_or(u32::MAX)
        };
        NodeCounts {
            total: count(&|_| true),
            ready: count(&|n| n.ready),
            control_plane: count(&|n| n.control_plane),
            ready_control_plane: count(&|n| n.control_plane && n.ready),
            worker: count(&|n| !n.control_plane),
            ready_worker: count(&|n| !n.control_plane && n.ready),
        }
    }

    /// Legacy's derivation, verbatim (`manager/src/services/platforms.rs:210-226`).
    ///
    /// The `total == 0` arm must come first: with no nodes, `ready == total`
    /// is `0 == 0` and an empty cluster would otherwise report Healthy.
    #[must_use]
    pub fn status(&self) -> ClusterStatus {
        let counts = self.counts();
        if counts.total == 0 {
            ClusterStatus::Warning
        } else if counts.ready == counts.total {
            ClusterStatus::Healthy
        } else if counts.ready == 0 {
            ClusterStatus::Unhealthy
        } else {
            ClusterStatus::Degraded
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fixture(name: &str) -> BTreeMap<String, String> {
        let raw = std::fs::read_to_string(format!("tests/fixtures/{name}.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn a_build_is_split_off_only_when_the_head_still_has_a_dot() {
        assert_eq!(parse_platform_version("26.5.0"), ("26.5".into(), Some("0".into())));
        assert_eq!(parse_platform_version("1.0"), ("1.0".into(), None));
        assert_eq!(parse_platform_version("0.1.516"), ("0.1".into(), Some("516".into())));
        // A non-numeric tail is part of the version, not a build.
        assert_eq!(parse_platform_version("1.2.rc1"), ("1.2.rc1".into(), None));
        assert_eq!(parse_platform_version("  26.5.0  "), ("26.5".into(), Some("0".into())));
    }

    #[test]
    fn the_real_clusters_metadata_parses_to_the_expected_version() {
        let d = detected_from_data(&fixture("core-install-metadata"), "virtuozzo", "unknown").unwrap();
        assert_eq!(d.version, "26.5");
        assert_eq!(d.build.as_deref(), Some("0"));
        assert_eq!(d.namespace, "virtuozzo");
    }

    #[test]
    fn metadata_without_a_platform_version_is_not_a_detection() {
        let mut data = BTreeMap::new();
        data.insert("profile".to_owned(), "dev".to_owned());
        assert!(detected_from_data(&data, "virtuozzo", "unknown").is_none());
        // Present but blank is also not a detection.
        data.insert("platformVersion".to_owned(), "   ".to_owned());
        assert!(detected_from_data(&data, "virtuozzo", "unknown").is_none());
    }

    #[test]
    fn fallback_namespace_is_used_when_provided_namespace_is_empty() {
        let mut data = BTreeMap::new();
        data.insert("platformVersion".to_owned(), "26.5.0".to_owned());
        let d = detected_from_data(&data, "", "fallback-ns").unwrap();
        assert_eq!(d.namespace, "fallback-ns");
        // Whitespace-only namespace also triggers fallback.
        let d = detected_from_data(&data, "   ", "another-fallback").unwrap();
        assert_eq!(d.namespace, "another-fallback");
    }

    #[test]
    fn only_external_hosts_contribute_and_the_bare_ip_entry_is_skipped() {
        let hosts = external_hosts(&fixture("vp-gateway-hostnames"));
        assert!(hosts.contains(&"api.sv.jele.io".to_owned()));
        assert!(hosts.contains(&"app.sv.jele.io".to_owned()));
        // `internal-gateway-clusterip` holds a bare IP and would fail JSON parsing.
        assert!(!hosts.iter().any(|h| h.starts_with("10.")));
        // `core-monitoring-query` is visibility: internal-only.
        assert!(!hosts.contains(&"core-monitoring-query.sv.jele.io".to_owned()));
    }

    #[test]
    fn the_real_clusters_gateway_map_yields_the_base_domain() {
        let hosts = external_hosts(&fixture("vp-gateway-hostnames"));
        assert_eq!(compute_base_domain(&hosts), Some("sv.jele.io".to_owned()));
    }

    #[test]
    fn one_host_is_inconclusive_because_vpadm_never_exposes_the_bare_domain() {
        assert_eq!(compute_base_domain(&["api.sv.jele.io".to_owned()]), None);
        // The same host twice is still one distinct host.
        assert_eq!(
            compute_base_domain(&["api.sv.jele.io".to_owned(), "api.sv.jele.io".to_owned()]),
            None
        );
    }

    #[test]
    fn a_suffix_shorter_than_two_labels_is_inconclusive_rather_than_wrong() {
        // Zero-shared: reversed zip fails immediately on different labels.
        assert_eq!(
            compute_base_domain(&["a.example.com".to_owned(), "b.example.org".to_owned()]),
            None
        );
    }

    #[test]
    fn one_shared_label_is_also_inconclusive() {
        // One-shared: api.example.io and app.other.io share only "io".
        // The check `common.len() < 2` must reject this.
        assert_eq!(
            compute_base_domain(&["api.example.io".to_owned(), "app.other.io".to_owned()]),
            None
        );
    }
}

#[cfg(test)]
mod cluster_health_tests {
    use super::*;

    fn node(name: &str, control_plane: bool, ready: bool) -> NodeSummary {
        NodeSummary {
            name: name.to_owned(),
            control_plane,
            ready,
            kubelet_version: None,
            os_image: None,
        }
    }

    fn health(nodes: Vec<NodeSummary>) -> ClusterHealth {
        ClusterHealth { nodes, namespace_count: Some(14) }
    }

    /// The real `sv-test` shape, measured 2026-08-28: a single-node k3s whose
    /// one node is control-plane. The worker row is 0/0 and that is correct.
    #[test]
    fn a_single_control_plane_node_is_healthy_with_no_workers() {
        let h = health(vec![node("sv-vhp-jele-io", true, true)]);
        assert_eq!(h.status(), ClusterStatus::Healthy);
        let c = h.counts();
        assert_eq!((c.total, c.ready), (1, 1));
        assert_eq!((c.control_plane, c.ready_control_plane), (1, 1));
        assert_eq!((c.worker, c.ready_worker), (0, 0));
    }

    /// The boundary a naive `ready == total` gets wrong: zero equals zero, so
    /// an empty cluster would report Healthy. Legacy checks it first, and so
    /// must this.
    #[test]
    fn a_cluster_with_no_nodes_is_warning_not_healthy() {
        assert_eq!(health(vec![]).status(), ClusterStatus::Warning);
    }

    #[test]
    fn no_node_ready_is_unhealthy() {
        let h = health(vec![node("a", true, false), node("b", false, false)]);
        assert_eq!(h.status(), ClusterStatus::Unhealthy);
        assert_eq!(h.counts().ready, 0);
    }

    #[test]
    fn some_nodes_ready_is_degraded() {
        let h = health(vec![node("a", true, true), node("b", false, false)]);
        assert_eq!(h.status(), ClusterStatus::Degraded);
        let c = h.counts();
        assert_eq!((c.ready, c.total), (1, 2));
    }

    /// Readiness is counted per role, not inferred from the totals: a ready
    /// control-plane next to an unready worker must not report the worker as
    /// ready.
    #[test]
    fn readiness_is_counted_within_each_role() {
        let h = health(vec![
            node("cp1", true, true),
            node("cp2", true, false),
            node("w1", false, true),
            node("w2", false, false),
            node("w3", false, false),
        ]);
        let c = h.counts();
        assert_eq!((c.total, c.ready), (5, 2));
        assert_eq!((c.control_plane, c.ready_control_plane), (2, 1));
        assert_eq!((c.worker, c.ready_worker), (3, 1));
        assert_eq!(h.status(), ClusterStatus::Degraded);
    }

    #[test]
    fn an_unread_namespace_count_is_none_not_zero() {
        let h = ClusterHealth { nodes: vec![node("a", true, true)], namespace_count: None };
        assert_eq!(h.namespace_count, None);
        assert_eq!(h.status(), ClusterStatus::Healthy);
    }

    /// The stored string is a wire value: a rename here silently reclassifies
    /// every row already in the database.
    #[test]
    fn the_status_strings_are_the_stored_wire_values() {
        assert_eq!(ClusterStatus::Healthy.as_str(), "Healthy");
        assert_eq!(ClusterStatus::Degraded.as_str(), "Degraded");
        assert_eq!(ClusterStatus::Unhealthy.as_str(), "Unhealthy");
        assert_eq!(ClusterStatus::Warning.as_str(), "Warning");
    }

    /// A stored blob written by one version must be readable by the next.
    #[test]
    fn cluster_health_round_trips_through_json() {
        let h = health(vec![NodeSummary {
            name: "sv-vhp-jele-io".to_owned(),
            control_plane: true,
            ready: true,
            kubelet_version: Some("v1.33.4+k3s1".to_owned()),
            os_image: Some("Ubuntu 24.04.3 LTS".to_owned()),
        }]);
        let json = serde_json::to_string(&h).expect("serialising ClusterHealth");
        let back: ClusterHealth = serde_json::from_str(&json).expect("deserialising ClusterHealth");
        assert_eq!(back, h);
    }
}
