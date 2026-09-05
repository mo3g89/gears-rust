//! Domain layer: pure rules, ports, repository traits, and services.
//!
//! [`error`] arrived at Task 9; [`analytics`] and [`repos`] at Task 11;
//! [`ports`], [`service`] and [`system_actor`] at Task 13; [`jira`] at Task 31;
//! [`local_client`] at Task 34; [`notify`] at Task 36. The rest arrives on the
//! plan's schedule: the analytics cores over [`analytics::ExecRow`] in Tasks
//! 20-24, and the notification client/sender/wiring in Tasks 37-40.
//!
//! [`elevated`] is the one named exception to "every query is PEP-scoped":
//! the single seam the ticker enumeration in `service::tenants` elevates
//! through instead of asking the PDP. See its module doc.

pub mod analytics;
pub mod elevated;
pub mod error;
pub mod jira;
pub mod local_client;
pub mod notify;
pub mod ports;
pub mod repos;
pub mod service;
pub mod system_actor;
