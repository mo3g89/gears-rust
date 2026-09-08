pub mod error;
pub mod lease;
pub mod local_client;
/// The observability metric catalog: the full, literal Prometheus series
/// names this gear exports, and the naming rules that keep them queryable.
/// The traits that emit into them are [`ports::metrics`].
pub mod metrics;
// `pub mod observation;` was deleted at branch close.
//
// It held this gear's own copy of the VHP detection rules --
// `parse_platform_version`, `detected_from_data`, `DetectedPlatform`,
// `ObservationHealth`, `GatewayResolvedHost` -- 475 lines and 17 tests. Task 8
// moved the live copy to `qa-vhp-product-plugin/src/detect.rs` and Task 15 made
// that the only one the gear calls, but the module stayed reachable through
// `legacy_cluster_status`, which computed a value for the `cluster_status`
// column Task 19 dropped.
//
// So the gear was carrying a second, unreachable implementation of the very
// rules this branch exists to move OUT of the platform, kept green by its own
// seventeen tests and with nothing to warn a maintainer editing one copy about
// the other. The whole-branch review found it (I-2).
pub mod observation_write;
pub mod ports;
pub mod repos;
pub mod service;
/// The observation ticker's system-actor identity.
///
/// Compiled unconditionally since Task 19b. Its one production caller
/// (`EnvironmentsService::run_observation_cycle`) observes through the product
/// plugin, which needs no Kubernetes client, so nothing about this module is
/// feature-gated any more. That keeps it — and its tests — in a default
/// `cargo test` run, so the ADR-0001 property that this gear's domain is
/// testable with no Kubernetes in the tree still covers it.
pub mod system_actor;
