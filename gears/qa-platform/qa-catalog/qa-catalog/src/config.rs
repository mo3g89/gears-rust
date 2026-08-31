//! Typed configuration for the `qa-catalog` gear.
//!
//! Consumed by the gear bootstrap (`crate::gear`): the domain services take
//! `repos_dir`, `bundle_ttl_seconds`, and `branch_freshness_ttl_seconds`
//! (the sync freshness cache), the local-fs `BundleStore` takes
//! `bundles_dir`, and the branch-cache refresher lifecycle task takes
//! `branch_refresh_interval_seconds`.

use serde::Deserialize;

/// Typed configuration for the qa-catalog gear (YAML section `qa-catalog`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QaCatalogConfig {
    /// Working directory for synced repositories.
    pub repos_dir: String,
    /// Directory for bundle blobs (local-fs `BundleStore`).
    pub bundles_dir: String,
    /// Bundle time-to-live in seconds.
    pub bundle_ttl_seconds: u64,
    /// Branch cache refresh interval in seconds (0 disables the background task).
    pub branch_refresh_interval_seconds: u64,
    /// How long a materialized branch snapshot is trusted before a content
    /// read triggers a re-sync. `0` disables the freshness cache. The launch
    /// path force-syncs regardless.
    pub branch_freshness_ttl_seconds: u64,
}

impl Default for QaCatalogConfig {
    fn default() -> Self {
        Self {
            repos_dir: "./data/qa-catalog/repos".into(),
            bundles_dir: "./data/qa-catalog/bundles".into(),
            bundle_ttl_seconds: 3600,
            branch_refresh_interval_seconds: 900,
            branch_freshness_ttl_seconds: 300,
        }
    }
}
