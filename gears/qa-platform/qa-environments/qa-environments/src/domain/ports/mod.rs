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
mod runner_secret;

pub use product_plugin::{PluginUnavailable, ProductPluginPort};
// `NoopRunnerSecretWriter` is constructed by `gear.rs` only in a build
// *without* the `runner-secret` feature; with the feature on, this crate's own
// tests are its only callers. Review finding #38 made `domain` `pub(crate)`,
// which is what turned that into a visible `unused_imports` — while the module
// was `pub`, the re-export was public API and could never be unused. Kept
// re-exported rather than cfg-split, so `crate::domain::ports` names both
// halves of the port in every build.
#[cfg_attr(
    feature = "runner-secret",
    allow(
        unused_imports,
        reason = "used by gear.rs only in a build without `runner-secret`, and by this \
                  crate's tests in every build"
    )
)]
pub use runner_secret::{NoopRunnerSecretWriter, RunnerSecretWriter};
