//! REST request handlers - thin delegation to domain services.
//!
//! Handlers extract the request, call into `crate::gear::ConcreteAppServices`
//! (`domain::service::AppServices`), and convert the result to a DTO. No
//! business logic, no DB access, and no PEP calls happen here — all of that
//! lives in `crate::domain::service`.

mod bundles;
mod custom_plans;
mod plans;
mod products;
mod ssh_keys;
mod test_repos;

pub(crate) use bundles::download_bundle;
pub(crate) use custom_plans::{
    create_custom_plan, delete_custom_plan, get_custom_plan, list_custom_plans, update_custom_plan,
};
pub(crate) use plans::list_plans;
pub(crate) use products::{
    create_product, delete_product, list_product_folders, list_products, update_product,
};
pub(crate) use ssh_keys::{create_ssh_key, delete_ssh_key, list_ssh_keys};
pub(crate) use test_repos::{
    create_test_repo, delete_test_repo, get_test_repo, list_test_repo_branches, list_test_repos,
    sync_test_repo, update_test_repo,
};
