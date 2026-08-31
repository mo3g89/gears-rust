//! REST request handlers - thin delegation to domain services.
//!
//! Handlers extract the request, call into `crate::gear::ConcreteAppServices`
//! (`domain::service::AppServices`), and convert the result to a DTO. No
//! business logic, no DB access, and no PEP calls happen here — all of that
//! lives in `crate::domain::service`.

mod platforms;
mod variables;

pub(crate) use platforms::{
    create_platform, delete_platform, get_platform, get_platform_lease, list_platforms,
    refresh_platform, update_platform,
};
pub(crate) use variables::{delete_variable, list_variables, upsert_variable};
