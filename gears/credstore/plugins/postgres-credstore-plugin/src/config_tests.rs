//! Configuration deserialization tests.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::use_debug
)]

use credstore_sdk::SharingMode;

use super::{DEFAULT_VENDOR, PostgresCredStorePluginConfig};

#[test]
fn defaults_use_this_plugin_s_own_vendor() {
    let cfg = PostgresCredStorePluginConfig::default();
    assert_eq!(cfg.vendor, DEFAULT_VENDOR);
    assert_eq!(cfg.priority, 100);
    assert!(cfg.secrets.is_empty());
}

/// The default vendor must differ from the in-memory plugin's, or both plugins
/// compiled into one binary would tie on vendor and the selection would come
/// down to `priority` — the exact ambiguity the separate vendor string avoids.
#[test]
fn the_default_vendor_differs_from_the_static_plugin_s() {
    assert_ne!(DEFAULT_VENDOR, "constructorfabric");
}

#[test]
fn deserializes_an_empty_config_section() {
    let cfg: PostgresCredStorePluginConfig =
        serde_saphyr::from_str("{}").expect("empty map deserializes");
    assert_eq!(cfg.vendor, DEFAULT_VENDOR);
}

#[test]
fn deserializes_a_full_config_section() {
    let yaml = r#"
vendor: "acme"
priority: 42
secrets:
  - tenant_id: "00000000-0000-0000-0000-000000000001"
    key: "db-password"
    value: "s3cret"
    sharing: tenant
"#;
    let cfg: PostgresCredStorePluginConfig =
        serde_saphyr::from_str(yaml).expect("full config deserializes");
    assert_eq!(cfg.vendor, "acme");
    assert_eq!(cfg.priority, 42);
    assert_eq!(cfg.secrets.len(), 1);
    assert_eq!(cfg.secrets[0].resolve_sharing(), SharingMode::Tenant);
}

#[test]
fn unknown_fields_are_rejected() {
    let err = serde_saphyr::from_str::<PostgresCredStorePluginConfig>("nope: 1");
    assert!(err.is_err(), "deny_unknown_fields must reject stray keys");
}

/// A `SecretConfig`'s `Debug` must not print the value.
#[test]
fn secret_config_debug_redacts_the_value() {
    let cfg: PostgresCredStorePluginConfig = serde_saphyr::from_str(
        r#"
secrets:
  - key: "k"
    value: "SUPER-SECRET-VALUE"
"#,
    )
    .expect("deserializes");
    let rendered = format!("{:?}", cfg.secrets[0]);
    assert!(!rendered.contains("SUPER-SECRET-VALUE"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
}

/// The `config:` block this plugin's stanza ships with in
/// `gears/qa-platform/config/qa-platform-stack.yaml` and `config/qa-platform.yaml`
/// must deserialize, and must select this plugin rather than the in-memory one.
/// Pinned here so a rename of the vendor constant cannot silently orphan the
/// shipped config (`deny_unknown_fields` would catch a renamed key, but not a
/// changed vendor value).
#[test]
fn the_shipped_stack_config_block_deserializes() {
    let yaml = r#"
vendor: "constructorfabric-postgres"
priority: 100
secrets: []
"#;
    let cfg: PostgresCredStorePluginConfig =
        serde_saphyr::from_str(yaml).expect("the shipped config block deserializes");
    assert_eq!(
        cfg.vendor, DEFAULT_VENDOR,
        "the shipped vendor must be this plugin's"
    );
    assert_eq!(cfg.priority, 100);
    assert!(cfg.secrets.is_empty());
}
