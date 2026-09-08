//! Kubernetes transport for QA Platform product plugins whose target is a
//! cluster.
//!
//! A **connector**, not a plugin: this crate has no gear, no GTS identity and
//! nothing resolves it at runtime. Product plugins link it the way they link
//! any dependency. Renamed from `qa-plugin-k8s` on 2026-09-08 for that reason
//! -- the old name claimed a kind it never was.
//!
//! # Why this crate exists
//!
//! ADR-0001 forbids `kube`/`k8s-openapi` in any qa-platform crate.
//! `PRODUCT-PLUGINS-DESIGN.md` §4.1 amends it, and ADR-0001's 2026-09-04
//! amendment records exactly what that bought and what it cost. The claim
//! this crate makes good on, stated precisely because the looser version of
//! it was wrong and shipped: **this is the only qa-platform crate that names
//! those types unconditionally**, and only the product plugins that actually
//! target clusters link it. It is *not* the only crate in the workspace that
//! names them — `qa-environments` does behind `runner-secret` (the feature
//! Task 19b renamed `platform-observation` to), `qa-runs` behind `argo` for as
//! long as the Argo adapter
//! lives, and outside qa-platform so do `libs/toolkit-k8s-auth`,
//! `chat-engine` and `mini-chat`.
//!
//! Containment here stops being a convention a reviewer has to remember and
//! becomes a fact about the dependency graph — which is why there is no cargo
//! feature here to turn Kubernetes off. A build that does not want it does not
//! depend on this crate. The cost of having no gate is recorded in that ADR
//! amendment and is real: there is no longer any build of the `qa-platform`
//! feature without `kube`. (There *is* one feature here, `test-support`. It
//! gates the test doubles in the `test_support` module and nothing else;
//! Kubernetes is unconditional either way. Not an intra-doc link on purpose:
//! that module does not exist in a default-feature build, so a link to it is a
//! rustdoc error in exactly the build most readers document.)
//!
//! # Where the code came from
//!
//! Every module here was lifted from `qa-environments/src/infra/observer/`,
//! which stays in place and stays active behind its `platform-observation`
//! feature until Task 19 removes it. This is a **copy**: Phase C must not
//! change `qa-environments`' behaviour, so both paths exist side by side
//! until the one-way door in Phase E. Each module's header names its origin.
//!
//! What did **not** come along: the VHP install-topology rules
//! (`core-install-metadata`, `vp-gateway-hostnames`, `platformVersion`). Those
//! are what a *product* knows, not what Kubernetes is, and Task 9 moves them
//! into `qa-vhp-product-plugin`. What is left is the mechanics that were
//! wrapped around them.
//!
//! # The invariant every module here maintains
//!
//! **No value derived from credential material is ever formatted.** Not
//! `Display`, not `Debug`, not into a message, a log line or a DTO. Failures
//! cross this crate's boundary as
//! [`qa_product_sdk::observation::PluginFailure`], whose `detail` is
//! `&'static str` and therefore cannot be produced from runtime bytes. The
//! one sanctioned exception is text a *remote* sent back, which travels in
//! `PluginFailure::remote_message`. [`errors`] records the measured leak that
//! this rule exists to prevent and the reasoning behind classification rather
//! than sanitisation; read it before changing anything here.

pub mod errors;
pub mod kube_client;
pub mod secret_writer;

// Test doubles for the API server. Public rather than `cfg(test)` because
// ADR-0001 makes them impossible to build anywhere else; the module's own
// header carries the rest of the argument, and is a plain `//` comment here
// so rustdoc resolves that header's links inside the module rather than out
// here, where `StubApiServer` is not in scope.
#[cfg(feature = "test-support")]
pub mod test_support;

pub use errors::{classify, classify_kubeconfig};
pub use kube_client::{
    ClusterHealth, ClusterStatus, ConfigMapData, KubeClient, LabelSelector, NodeSummary,
};
pub use secret_writer::SecretWriter;
