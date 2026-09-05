//! The detection suite, copied with [`super`] from
//! `qa-environments/src/domain/observation.rs`' `mod tests`.
//!
//! Every assertion is the original's, unchanged: these pin the frozen VHP
//! rules, and a test that moved and then relaxed would prove the move was
//! safe while making it unsafe. The two fixtures are the same two files, byte
//! for byte — real `sv-test` `ConfigMap` payloads measured 2026-08-28 — copied
//! into this crate's own `tests/fixtures/` so the suite resolves them relative
//! to this manifest rather than the one it came from.

use super::*;
use std::collections::BTreeMap;

fn fixture(name: &str) -> BTreeMap<String, String> {
    let raw = std::fs::read_to_string(format!("tests/fixtures/{name}.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn a_build_is_split_off_only_when_the_head_still_has_a_dot() {
    assert_eq!(
        parse_platform_version("26.5.0"),
        ("26.5".into(), Some("0".into()))
    );
    assert_eq!(parse_platform_version("1.0"), ("1.0".into(), None));
    assert_eq!(
        parse_platform_version("0.1.516"),
        ("0.1".into(), Some("516".into()))
    );
    // A non-numeric tail is part of the version, not a build.
    assert_eq!(parse_platform_version("1.2.rc1"), ("1.2.rc1".into(), None));
    assert_eq!(
        parse_platform_version("  26.5.0  "),
        ("26.5".into(), Some("0".into()))
    );
}

#[test]
fn the_real_clusters_metadata_parses_to_the_expected_version() {
    let d = detected_from_data(&fixture("core-install-metadata"), "virtuozzo", "unknown").unwrap();
    assert_eq!(d.version, "26.5");
    assert_eq!(d.build.as_deref(), Some("0"));
    assert_eq!(d.namespace, "virtuozzo");
}

#[test]
fn metadata_without_an_environment_version_is_not_a_detection() {
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
