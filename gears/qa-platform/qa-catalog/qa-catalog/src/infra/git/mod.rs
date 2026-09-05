//! Git engine integration (ADR-0005
//! `cpt-cf-qa-adr-git-egress`): the gix-based implementation of
//! [`crate::domain::ports::repo_sync::RepoSyncPort`].

mod gix_sync;
pub mod layout;
mod ssh_agent;

pub use gix_sync::GixSyncEngine;
