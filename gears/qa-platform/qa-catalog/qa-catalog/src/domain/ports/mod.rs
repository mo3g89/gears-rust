//! Domain ports: trait boundaries implemented by infra adapters.

pub mod bundle_store;
/// The plugin boundary's telemetry, from the side that owns the product ->
/// plugin binding. Its header carries the decision to instrument the caller
/// rather than the plugins, and why neither the plugin's GTS instance id nor
/// its type id is a label.
pub mod metrics;
pub mod repo_sync;
