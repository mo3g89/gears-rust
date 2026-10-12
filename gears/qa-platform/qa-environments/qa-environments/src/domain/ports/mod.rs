/// The observability metric-emission port: the label taxonomy and the trait
/// `domain::service::EnvironmentsService` emits the observation cycle through.
///
/// `pub mod` rather than the `mod` + `pub use` shape its two neighbours use.
/// The module holds a trait, three label enums and a no-op implementation, and
/// flattening six names into this file would lose the `metrics::` qualifier
/// that makes a label enum readable at a call site. qa-runs and qa-insights
/// name theirs the same way, and every call site in all three reads
/// `ports::metrics::..`.
pub mod metrics;
mod product_plugin;
mod runner_credentials;

pub use product_plugin::{PluginUnavailable, ProductPluginPort};
// `DenyingPodIdentityVerifier` is `gear.rs`'s verifier only in a build
// without `runner-credentials`; with it, this crate's tests are its only
// callers.
#[cfg_attr(
    all(not(test), feature = "runner-credentials"),
    expect(
        unused_imports,
        reason = "used by gear.rs only in a build without `runner-credentials`, and by this \
                  crate's tests in every build"
    )
)]
pub use runner_credentials::DenyingPodIdentityVerifier;
pub use runner_credentials::{
    PodIdentityRefusal, PodIdentityVerifier, RunnerPodRefusal, RunnerPodResolver, VerifiedRunnerPod,
};
