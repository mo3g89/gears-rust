//! Adapters over the SDK clients this gear reaches other gears through.
//!
//! # Why this module exists when qa-runs has no equivalent
//!
//! qa-runs names sibling SDK traits directly in its domain signatures and needs
//! no adapter layer. This gear cannot, because it declared a **port**:
//! [`crate::domain::ports::RunsReader`] is **four** methods over a
//! **seventeen**-method client (`qa-runs-sdk/src/client.rs:24-200`, counted
//! 2026-08-21), and its header states why (a fake of the whole client would be
//! **thirteen** `unimplemented!()`s around the four that matter). A port implies
//! an adapter, and this is where the adapters live.
//!
//! **All three numbers were wrong until Task 18's fix round.** Two of them —
//! nineteen and sixteen — arrived here with this file in Task 16, copied from a
//! port header that has miscounted the trait since Task 13; the third became
//! wrong when Task 18 added `list_recent_runs` and corrected the port header and
//! the adapter's test module without noticing this file. Recorded rather than
//! quietly fixed, because this is the **third** place in the gear where the same
//! count is spelled out by hand, and the next hand-spelling of it should know
//! that the last two were wrong for two tasks and one commit respectively.
//!
//! The plan's file map has no slot for this module; `infra/jira/` and
//! `infra/notify/` — the two egress adapters it *does* name — are the
//! precedent, and the shape is the same one: a thin translation between a
//! sibling's vocabulary and this gear's, holding no logic of its own.
//!
//! * [`qa_runs`] — [`QaRunsReader`], the [`RunsReader`](crate::domain::ports::RunsReader)
//!   adapter over `qa_runs_sdk::QaRunsClientV1` (Task 16).
//! * [`qa_catalog`] — [`QaCatalogReader`], the
//!   [`CatalogReader`](crate::domain::ports::CatalogReader) adapter over
//!   `qa_catalog_sdk::QaCatalogClientV1` (**Task 25a**).
//! * [`qa_environments`] — [`QaEnvironmentsReader`], the
//!   [`PlatformReader`](crate::domain::ports::PlatformReader) adapter over
//!   `qa_environments_sdk::QaEnvironmentsClientV1` (**Task 25a**, first called
//!   and first wired by Task 25b). **This list omitted it for a whole task** —
//!   the module has existed since 25a and this header, which is the register of
//!   what crosses a gear boundary, did not say so. Third omission or miscount in
//!   this file, which is the reason the paragraph above exists.
//!
//! **This line forecast the second one for "Task 20" and it was Task 40's**, per
//! `domain::ports::catalog_reader`'s own header and the plan; neither spelling
//! survived. It landed at Task 25a, which is the task that pulled it forward
//! because it issues the first real `list_universe` read. **Task 16 did exactly
//! the same for `QaRunsClientV1`** — `gear.rs`' own `deps` doc records that
//! pull-forward and names it as the precedent — so this is the second
//! `ClientHub`-shaped obligation parked on Task 40 that the task needing the data
//! had to take instead. Recorded rather than quietly corrected, because this file
//! has now been wrong about the *owner* of both of its two entries.
//!
//! Nothing else is forecast here. The remaining egress adapters (`jira_client`,
//! `slack_client`, `mail_client`) are named on `domain::ports`' header with their
//! tasks.
//!
//! **All three of the adapters here are now resolved from the `ClientHub` in
//! `gear::init`**, and each `deps` token landed in the commit that added its
//! lookup: `qa_runs` at Task 16, `qa_catalog` at Task 25a (already in `deps` from
//! the skeleton) and `qa_environments` at Task 25b, which had to add the token
//! and the gear-crate dependency as well. `gear.rs`' `deps` doc records why that
//! coupling is not optional.

pub mod qa_catalog;
pub mod qa_environments;
pub mod qa_runs;

pub use qa_catalog::QaCatalogReader;
pub use qa_environments::QaEnvironmentsReader;
pub use qa_runs::QaRunsReader;
