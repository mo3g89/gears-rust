//! qa-environments' link-time GTS content.
//!
//! Everything declared here reaches `types-registry` automatically through the
//! process-wide `toolkit-gts` inventory — no registration code in
//! `crate::gear` is needed. One file per content kind keeps this directory
//! navigable (permissions today).

pub mod permissions;
