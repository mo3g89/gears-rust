//! Typed configuration for the `qa-catalog` gear.
//!
//! Consumed by the gear bootstrap (`crate::gear`): the domain services take
//! `repos_dir`, `bundle_ttl_seconds`, `branch_freshness_ttl_seconds` and
//! `remote_failure_backoff_seconds` (the sync freshness cache and the failure
//! backoff), the gix sync engine takes `sync_timeout_seconds`,
//! `ls_refs_timeout_seconds`, `max_fetch_bytes` and `max_checkout_bytes`, and `bundle_download_signing_secret` (the HMAC
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
    /// How long a repository whose remote failed to list, whose credential
    /// could not be used, or whose content sync timed out or went past
    /// `max_fetch_bytes`/`max_checkout_bytes`, is backed off: a read that would
    /// sync it answers at once — 503 for a remote that could not be listed
    /// (unreachable, or a listing that timed out), 400 with the recorded
    /// reason for a credential fault, an over-budget repository or a timed-out
    /// content sync — without contacting the remote. In memory, per
    /// replica. A successful listing or sync, a forced sync, and a change of
    /// the repository's `url` or `credential_ref` end it early; the explicit
    /// sync and the branch-cache refresher are not held back by it, and the
    /// refresher, which records nothing, never starts it. `0` disables it. See DESIGN §3.3 "Branch model and the first read of a
    /// branch".
    pub remote_failure_backoff_seconds: u64,
    /// Deadline of one sync (clone or fetch, then checkout). At the deadline
    /// the work is interrupted and the sync fails as a timeout: it is
    /// recorded in `sync_error`, so the read that ran it answers 400 with
    /// that reason, and the repository is backed off for that reason — reads
    /// inside the backoff answer the same 400 at once, without contacting the
    /// remote. `0` disables it. See
    /// DESIGN §3.3 "Limits on talking to a remote".
    pub sync_timeout_seconds: u64,
    /// Deadline of one branch listing (ls-refs), which a read waits on inside
    /// its request. A listing that times out answers 503 and backs the
    /// repository off, like an unreachable remote. `0` disables it.
    pub ls_refs_timeout_seconds: u64,
    /// Most bytes one clone or fetch may add to the repository's pack
    /// directory. Over it the sync stops, the working area is removed, and
    /// the repository is recorded as too large (400) and backed off. `0`
    /// disables it.
    pub max_fetch_bytes: u64,
    /// Most bytes one branch checkout may write. Over it nothing is written,
    /// and the failure is recorded like `max_fetch_bytes`'. `0` disables it.
    pub max_checkout_bytes: u64,
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
            remote_failure_backoff_seconds: 30,
            sync_timeout_seconds: 300,
            ls_refs_timeout_seconds: 30,
            // 5 % of the chart's 20 GiB volume, which every clone, snapshot
            // and bundle shares.
            max_fetch_bytes: 1 << 30,
            // A snapshot every runner pod downloads as its bundle.
            max_checkout_bytes: 512 << 20,
            // Empty is fail-closed, not permissive: every download is refused
            // until a deployment sets this. See the field's own doc.
            bundle_download_signing_secret: String::new(),
        }
    }
}
