//! Typed configuration for the `qa-catalog` gear.
//!
//! Consumed by the gear bootstrap (`crate::gear`): the domain services take
//! `repos_dir`, `bundle_ttl_seconds`, `branch_freshness_ttl_seconds`
//! (the sync freshness cache) and `bundle_download_signing_secret` (the HMAC
//! root for the anonymous bundle-download route), the local-fs `BundleStore`
//! takes `bundles_dir`, and the branch-cache refresher lifecycle task takes
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
    /// **The only access control on `GET /qa/v1/test-bundles/{id}`**, which is
    /// registered `.anonymous().exposed()`.
    ///
    /// The root secret every per-bundle download tag is HKDF-derived from —
    /// see `domain::service::bundles::derive_signing_key`. `create_bundle`
    /// signs `(bundle_id, tenant_id)` with the owning tenant's derived key and
    /// returns the tag on `TestBundle::download_sig`; the download route
    /// recomputes it and refuses, with one 403, anything that does not verify.
    ///
    /// **Fail-closed.** Empty, or shorter than
    /// `domain::service::bundles::MIN_SIGNING_SECRET_LEN` once trimmed, refuses
    /// *every* download — including a correctly computed one — rather than
    /// signing and verifying under a well-known empty key. `crate::gear`'s
    /// `init` warns once, loudly, through the same
    /// `bundles::signing_secret_is_configured` predicate the request path
    /// refuses on, so the boot check and the refusal cannot drift apart.
    ///
    /// The exact shape, the exact floor and the exact fail-closed treatment of
    /// `qa-insights`' `collect_report_signing_secret`, and for the same reason:
    /// both are the sole guard on a route that is anonymously reachable by
    /// design, because its caller is a workflow pod with no user to borrow a
    /// session from.
    ///
    /// # Rotation
    ///
    /// A single secret, with no dual-key acceptance window. Rotating it
    /// invalidates every outstanding bundle tag at once: a run dispatched
    /// before the rotation whose pod fetches after it gets a 403 and dies at
    /// `fetch_bundle.py`'s exit 1, before a single test runs. The window is
    /// bounded by `bundle_ttl_seconds`; draining the dispatch queue for that
    /// long is the zero-impact procedure, and it needs no code. That same
    /// property is the revocation mechanism this design deliberately has no
    /// table for.
    pub bundle_download_signing_secret: String,
}

impl Default for QaCatalogConfig {
    fn default() -> Self {
        Self {
            repos_dir: "./data/qa-catalog/repos".into(),
            bundles_dir: "./data/qa-catalog/bundles".into(),
            bundle_ttl_seconds: 3600,
            branch_refresh_interval_seconds: 900,
            branch_freshness_ttl_seconds: 300,
            // Empty is fail-closed, not permissive: every download is refused
            // until a deployment sets this. See the field's own doc.
            bundle_download_signing_secret: String::new(),
        }
    }
}
