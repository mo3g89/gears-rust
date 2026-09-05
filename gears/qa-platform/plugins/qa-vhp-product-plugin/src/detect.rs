//! Mirrors `manager/src/services/platforms.rs:1037-1149`
//! (`detect_platform_version` and `detect_base_domain`) line for line.
//!
//! Only the *rules* those two legacy functions apply are here — how a
//! `platformVersion` splits, which gateway entries count, what a shared domain
//! suffix is. The reads they wrap, and the error wording that goes with them,
//! are in [`crate::observe`], which carries the other half of this paragraph.
//! Nothing in this file produces an error string at all.
//!
//! # Where the code came from
//!
//! Copied from `qa-environments/src/domain/observation.rs`, which stays in
//! place and stays working until Task 19 removes it. A **copy**, not a move:
//! Phase C must not change `qa-environments`' behaviour, so both paths exist
//! side by side until the one-way door in Phase E.
//!
//! What came along is the VHP *install topology* half — `platformVersion`,
//! `core-install-metadata`, `vp-gateway-hostnames`, and the
//! `GATEWAY_INTERNAL_CLUSTERIP_KEY` skip. What did not is the Kubernetes
//! half (`ClusterHealth`, `NodeSummary`, `ClusterStatus`), which Task 8 lifted
//! the other way into `qa-plugin-k8s`: that is what a cluster is, not what
//! vpadm installed on it. `NodeCounts` and the `serde` derives stayed behind
//! in `qa-environments` with the readers that need them.
//!
//! These are the frozen VHP rules, tests included. This crate does not get to
//! improve them.
//!
//! Deliberately free of Kubernetes types: these take the `ConfigMap`'s `data`
//! map, not a `ConfigMap`, so every rule is testable without a cluster and
//! without `kube` anywhere in reach (ADR-0001).

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
    let raw = data
        .get("platformVersion")
        .filter(|v| !v.trim().is_empty())?
        .clone();
    let namespace = if namespace.trim().is_empty() {
        fallback_ns
    } else {
        namespace
    };
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
    let distinct: BTreeSet<&str> = hosts
        .iter()
        .map(|h| h.trim())
        .filter(|h| !h.is_empty())
        .collect();
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

#[cfg(test)]
#[path = "detect_tests.rs"]
mod detect_tests;
