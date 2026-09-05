//! REST request handlers - thin delegation to domain services.
//!
//! Handlers extract the request, call into `crate::gear::ConcreteAppServices`
//! (`domain::service::AppServices`), and convert the result to a DTO. No
//! business logic, no DB access, and no PEP calls happen here — all of that
//! lives in `crate::domain::service`.

mod environments;
mod variables;

pub(crate) use environments::{
    create_environment, delete_environment, get_environment, get_environment_lease,
    list_environments, refresh_environment, update_environment,
};
pub(crate) use variables::{delete_variable, list_variables, upsert_variable};
