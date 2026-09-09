// Updated: 2026-04-16 by Constructor Tech
// This file is used to ensure that all gears are linked and registered via inventory
// In future we can simply DX via build.rs which will collect all crates in ./gears and generate this file.
// But for now we will manually maintain this file.
#![allow(unused_imports)]

#[cfg(feature = "oagw")]
use api_egress as _;
use api_gateway as _;
use authn_resolver as _;
use authz_resolver as _;
#[cfg(feature = "credstore")]
use credstore as _;
#[cfg(all(feature = "file-parser", not(feature = "oop-example")))]
use file_parser as _;
#[cfg(feature = "file-storage")]
use file_storage as _;
use gear_orchestrator as _;
#[cfg(feature = "github-mirror")]
use github_mirror as _;
#[cfg(feature = "grpc-hub")]
use grpc_hub as _;
use license_resolver as _;
#[cfg(feature = "nodes-registry")]
use nodes_registry as _;
#[cfg(feature = "resource-group")]
use resource_group as _;
#[cfg(all(feature = "simple-user-settings", not(feature = "oop-example")))]
use simple_user_settings as _;
use tenant_resolver as _;
use types_registry as _;

#[cfg(feature = "single-tenant")]
use single_tenant_tr_plugin as _;

#[cfg(feature = "static-tenants")]
use static_tr_plugin as _;

#[cfg(feature = "tenant-resolver-rg")]
use rg_tr_plugin as _;

#[cfg(feature = "static-authn")]
use static_authn_plugin as _;

// Mirrors the `static-authn` entry above in every respect: the Cargo
// dependency key is `oidc-authn-plugin` (renaming package
// `cf-gears-oidc-authn-plugin`), so the extern crate arrives here as
// `oidc_authn_plugin`. The plugin crate already existed and was wired into no
// binary; Task 13 added the `oidc-authn` feature, the optional dependency and
// this import so `gears.oidc-authn-plugin` in a config file can resolve at
// all. Without the import the gear never registers via inventory, so no
// plugin lands in the ClientHub and api-gateway's `init` fails with "auth is
// enabled but no AuthN Resolver client is available"
// (gears/system/api-gateway/src/gear.rs:412-415) -- the config section alone
// links nothing.
#[cfg(feature = "oidc-authn")]
use oidc_authn_plugin as _;

#[cfg(feature = "static-authz")]
use static_authz_plugin as _;

#[cfg(feature = "tr-authz")]
use tr_authz_plugin as _;

#[cfg(feature = "static-license")]
use static_license_plugin as _;

#[cfg(feature = "static-credstore")]
use static_credstore_plugin as _;

// Same `inventory` registration hook as every other plugin above: without this
// import the crate is linked but its `#[toolkit::gear]` registration never
// runs, so `gears.postgres-credstore-plugin` in a config file resolves to
// nothing -- no migration, no GTS instance, and `credstore.config.vendor`
// pointing at this plugin's vendor would fail with "no credstore plugin found
// for vendor".
#[cfg(feature = "postgres-credstore")]
use postgres_credstore_plugin as _;

// === Optional Gears ===

#[cfg(feature = "mini-chat")]
use mini_chat as _;

#[cfg(feature = "mini-chat")]
use mini_chat::infra::plugins::static_audit as _;

#[cfg(feature = "mini-chat")]
use mini_chat::infra::plugins::static_model_policy as _;

#[cfg(feature = "chat-engine")]
use chat_engine as _;

#[cfg(feature = "qa-platform")]
use qa_environments as _;

#[cfg(feature = "qa-platform")]
use qa_catalog as _;

#[cfg(feature = "qa-platform")]
use qa_runs as _;

#[cfg(feature = "qa-platform")]
use qa_insights as _;

// The VHP product plugin, and the same `inventory` hook as every plugin
// above: without this import the crate is linked but its `#[toolkit::gear]`
// registration never runs, so the `gears.qa-vhp-product-plugin` stanza in
// config/qa-platform.yaml configures nothing, no GTS instance is published
// under `cf.core._.vhp_product.v1`, and (once Task 12 lands) every product
// naming that plugin instance resolves to nothing. The config section alone
// links nothing -- see the note on `oidc_authn_plugin` above, written after
// exactly that failure.
#[cfg(feature = "qa-platform")]
use qa_vhp_product_plugin as _;

// The VHI product plugin, wired in for exactly the reason the VHP import
// above is: without it the crate is linked but its `#[toolkit::gear]`
// registration never runs, no `gears.qa-vhi-product-plugin` stanza
// configures anything, and no GTS instance is published under
// `cf.core._.vhi_product.v1`.
#[cfg(feature = "qa-platform")]
use qa_vhi_product_plugin as _;

// === Example Features ===

#[cfg(feature = "users-info-example")]
use users_info as _;

#[cfg(feature = "oop-example")]
use calculator_gateway as _;

#[cfg(feature = "oop-example")]
use calculator as _;

#[cfg(feature = "static-idp")]
use static_idp_plugin as _;

#[cfg(feature = "account-management")]
use account_management as _;

#[cfg(feature = "bss-rate-provider")]
use bss_rate_provider as _;
#[cfg(feature = "bss-rate-provider")]
use bss_rate_provider_ecb_plugin as _;
#[cfg(feature = "bss-rate-provider")]
use bss_rate_provider_http_json_plugin as _;

#[cfg(feature = "bss-ledger")]
use bss_ledger as _;

#[cfg(feature = "usage-collector")]
use usage_collector as _;

#[cfg(feature = "timescaledb-usage-collector")]
use timescaledb_usage_collector_plugin as _;

#[cfg(feature = "bss-pricing")]
use bss_pricing as _;
