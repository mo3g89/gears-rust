//! Domain repository traits.
//!
//! Every method takes the caller-prepared `AccessScope` and a `DBRunner`
//! (`&SecureConn` or `&SecureTx`), so multi-statement operations can be run
//! inside a service-owned transaction. Implementations must never widen the
//! scope they are handed: whatever scope reaches a trait in this module —
//! PEP-compiled, or minted by the one named seam,
//! `domain::elevated::enumeration_scope`, for one of the two nil-tenant
//! lifecycle enumerations — is executed as given, never assembled or
//! loosened here. `AccessScope::allow_all()` itself appears in this gear's
//! production code only at that one seam; no implementation in this module
//! constructs one, and no query here is unscoped on its own account.

mod bundles_repo;
mod custom_plans_repo;
mod products_repo;
mod ssh_keys_repo;
mod test_repos_repo;

pub use bundles_repo::BundlesRepository;
pub use custom_plans_repo::CustomPlansRepository;
pub use products_repo::ProductsRepository;
pub use ssh_keys_repo::SshKeysRepository;
pub use test_repos_repo::{RefreshTarget, TestReposRepository};
