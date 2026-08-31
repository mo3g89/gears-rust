//! Domain ports — interfaces the domain owns and infrastructure satisfies.
//!
//! A port lives here, not in `infra`, because the domain defines what it needs
//! and an adapter answers it; the dependency arrow points inward. The adapter
//! itself lives under [`crate::infra`] (`infra::executor::mock` today, the
//! serverless-runtime adapter with feature 2.7).

pub mod run_executor;
