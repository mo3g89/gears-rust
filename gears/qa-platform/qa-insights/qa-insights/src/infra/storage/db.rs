//! Storage-layer conversions the repositories share: the page-size clamp, the
//! `OData` error classification, and `db_err`.
//!
//! The file this module's own header promised. `infra::storage`'s doc has said
//! since Task 10 that *"Task 17 creates it when it has something to put in it"*
//! and that `db_err` should move here at the same time; both happen in this
//! commit. The three sibling gears all keep exactly this content in exactly this
//! file (`qa-runs/src/infra/storage/db.rs`), so a reader who knows one gear finds
//! it here.
//!
//! **What is deliberately not here:** `is_retryable_contention`. qa-runs carries
//! one, this gear's projection writes run inside the caller's transaction, and
//! nothing in this crate retries a contended transaction. A copy would be a
//! function with no call site.

use std::fmt::Display;

use toolkit_db::odata::sea_orm_filter::LimitCfg;
use toolkit_odata::Error as ODataError;

use crate::domain::error::DomainError;

/// Default and maximum page size for every `OData` collection this gear ships.
///
/// # Re-declared, not imported, and this is the whole reason
///
/// The plan's Task 17 says to *reuse* qa-runs' clamp — "one page-size rule across
/// the subsystem's collections is the stated convention" — and names
/// `qa-runs/src/infra/storage/db.rs:13-15` as the source. The `qa-runs` **gear
/// crate is already a dependency** of this one (it is a `deps` token in
/// `gear.rs`, so `Cargo.toml` names it), so the import was **compiled, not
/// merely read**:
///
/// ```text
/// use qa_runs::infra::storage::db::PAGE_LIMITS;
/// error[E0603]: module `db` is private
///     |                          ^^  ----------- constant `PAGE_LIMITS` is not
///     |                          |               publicly re-exported
///     |                          private module
/// ```
///
/// `qa-runs/src/infra/storage/mod.rs:25` declares `pub(crate) mod db`, so the
/// constant is unreachable from outside that crate. The two ways to make it
/// reachable — widening that visibility, or lifting the constant into a shared
/// crate — both edit qa-runs, which this task is not permitted to do and which is
/// the wrong trade for two `u64`s.
///
/// So the numbers are duplicated. What stops them drifting is
/// [`tests::the_page_limits_match_the_subsystem_convention`], which asserts the
/// literals and names the source line to check them against; a divergence is
/// then a review question rather than an invisible difference in behaviour
/// between two collections a client pages the same way.
///
/// # Where the numbers come from
///
/// 200 default, 500 maximum. They originate in qa-runs' frozen queue contract —
/// *"`limit` defaults to 200 and is clamped to 1-500"* — which qa-runs extended
/// to its runs listing on the grounds that one page-size rule per subsystem is
/// easier for a client to hold than two. The same argument reaches here: a client
/// paging `/qa/v1/runs` and `/qa/v1/test-results` in the same session should not
/// have to remember two ceilings.
///
/// `LimitCfg` rather than a hand-rolled clamp, so the floor-of-1 and the ceiling
/// live in `toolkit-db` where every other paginated gear gets them
/// (`libs/toolkit-db/src/odata/sea_orm_filter.rs:523-532`, `clamp_limit` — note
/// the floor is hardcoded there and is *not* a `LimitCfg` field, so there is no
/// third number to keep true here).
///
/// # It is not `QaInsightsConfig::max_page_size`, and that knob is not orphaned
///
/// `config.rs`' table forecast `max_page_size` for this task. It is not used
/// here, deliberately: making the collection ceiling a per-deployment knob would
/// give the subsystem two page-size rules — a fixed 500 in qa-runs and a
/// configurable one here — which is the opposite of the convention the plan
/// states, and it would make the number the endpoint description quotes
/// deployment-dependent. That knob's own doc calls it *"max rows any analytics
/// query returns before paging"*, and the analytics reads (Tasks 24-27) are
/// non-`OData` aggregate endpoints with no `LimitCfg` anywhere near them; it is
/// re-forecast to them in `config.rs` rather than consumed here.
pub(crate) const PAGE_LIMITS: LimitCfg = LimitCfg {
    default: 200,
    max: 500,
};

/// Classify an `OData` failure as the caller's mistake or the server's.
///
/// The distinction is the whole reason this is not a blanket
/// [`DomainError::Database`]: a mistyped `$filter` field and a cursor from a
/// different sort order are things the caller sent and can fix, and answering 500
/// to them would report a client error as a server fault and hide it from anyone
/// reading error rates.
///
/// `Db` is the one that is genuinely not the caller's — a driver failure — and it
/// is redacted by the mapping in [`crate::api::rest::error`], so it does not leak.
///
/// # Only five of these fifteen variants can reach here, and the doc used to
/// # claim one that cannot
///
/// **Corrected 2026-08-21.** This doc named "a `$top` outside the configured
/// range" as one of the caller mistakes it classifies. It is not reachable, and
/// `$top` is not even the parameter: `clamp_limit`
/// (`libs/toolkit-db/src/odata/sea_orm_filter.rs:523-532`) *silently clamps* an
/// over-large limit and floors `0` to `1`, so an out-of-range page size never
/// produces an error at all; a literal `limit=0` is refused by the extractor
/// (`libs/toolkit/src/api/odata.rs:250-252`) as its own `CanonicalError`, never
/// through here; and the wire parameter is `limit`, with no `$`
/// (`ODataParams`, `odata.rs:13-21`).
///
/// What `paginate_odata` can actually hand this function, traced to each
/// construction site: `InvalidFilter` and `InvalidOrderByField` (the field
/// allow-list and the `is_orderable` gate), `FilterMismatch` (a cursor minted
/// under a different `$filter`), `InvalidCursor` (every cursor failure inside the
/// pager funnels into this one variant), and `Db`. The other ten are either the
/// **extractor's** — `InvalidLimit`, `OrderWithCursor` and the six
/// `CursorInvalid*` variants, which `CursorV1::decode` raises before a handler is
/// entered — or have no reachable constructor on this path at all: `OrderMismatch`
/// comes only from `toolkit_odata::validate_cursor_against`, which
/// `paginate_odata_collect` does not call, and `ParsingUnavailable` is
/// constructed nowhere outside `toolkit-odata`'s own tests.
///
/// The unreachable arms are **kept anyway**, and the `match` is still exhaustive
/// with no `_` arm, so a new `toolkit_odata::Error` variant is a compile error
/// rather than a silent misclassification — and a future `toolkit-db` that starts
/// calling `validate_cursor_against` finds its answer already decided.
///
/// **Inherited verbatim from qa-runs** (`qa-runs/src/infra/storage/db.rs`,
/// `odata_err` and `odata_field_of`), *including* the `$top` sentence — so this is
/// a convention question rather than a regression here, and that gear still
/// carries the same text.
pub(crate) fn odata_err(error: &ODataError) -> DomainError {
    match error {
        ODataError::InvalidFilter(_)
        | ODataError::InvalidOrderByField(_)
        | ODataError::OrderMismatch
        | ODataError::FilterMismatch
        | ODataError::InvalidCursor
        | ODataError::InvalidLimit
        | ODataError::OrderWithCursor
        | ODataError::CursorInvalidBase64
        | ODataError::CursorInvalidJson
        | ODataError::CursorInvalidVersion
        | ODataError::CursorInvalidKeys
        | ODataError::CursorInvalidFields
        | ODataError::CursorInvalidDirection => DomainError::Validation {
            // Named for the query parameter the caller actually typed, so the
            // 400's field violation points at something they can edit.
            field: odata_field_of(error).to_owned(),
            message: error.to_string(),
        },
        ODataError::Db(text) => DomainError::Database(text.clone()),
        ODataError::ParsingUnavailable(what) => DomainError::Internal((*what).to_owned()),
    }
}

/// Which query parameter an `OData` error is about.
///
/// A `_` arm on purpose: this picks a parameter *name for a message*, where a
/// wrong guess costs a slightly vague 400 rather than a misclassification. Every
/// remaining variant is a cursor failure, and a new one is far more likely to be
/// another cursor failure than not.
///
/// **The `"$top"` arm is dead and is knowingly kept.** `InvalidLimit` cannot
/// reach [`odata_err`] from `paginate_odata` (see its doc), and if it somehow did,
/// `$top` is not a parameter this platform's extractor accepts — the wire name is
/// `limit`. Kept because deleting it would leave `InvalidLimit` falling through to
/// `"cursor"`, which is a *wrong* answer where this one is merely an unused one,
/// and because the arm is qa-runs' and diverging silently is worse than
/// inheriting a documented oddity.
fn odata_field_of(error: &ODataError) -> &'static str {
    match error {
        ODataError::InvalidFilter(_) | ODataError::FilterMismatch => "$filter",
        ODataError::InvalidOrderByField(_) | ODataError::OrderMismatch => "$orderby",
        ODataError::InvalidLimit => "$top",
        _ => "cursor",
    }
}

/// Convert any displayable storage error into [`DomainError::Database`].
///
/// Identical to the three siblings' `db.rs::db_err`, including the loss of the
/// typed error: `Database` holds a `String`. **Moved here from
/// `infra::storage::mapper` by Task 17**, which is what that module's header and
/// `infra::storage`'s both said should happen when this file came to exist — a
/// conversion helper in the entity/SDK mapper was the placement of last resort,
/// taken because founding a file for three lines was worse.
pub(crate) fn db_err(e: impl Display) -> DomainError {
    DomainError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use toolkit_odata::Error as ODataError;

    use super::{PAGE_LIMITS, db_err, odata_err};
    use crate::domain::error::DomainError;

    /// The literals, pinned, because [`PAGE_LIMITS`] is a **copy** of
    /// `qa-runs/src/infra/storage/db.rs:34-37` that the compiler cannot check
    /// against its source — that module is `pub(crate)`, so no import is
    /// possible. See [`PAGE_LIMITS`]' doc for the whole argument.
    ///
    /// `LimitCfg` carries no floor field; the floor of 1 is hardcoded in
    /// `clamp_limit` (`libs/toolkit-db/src/odata/sea_orm_filter.rs:523-532`) and
    /// is therefore not this gear's to assert.
    #[test]
    fn the_page_limits_match_the_subsystem_convention() {
        assert_eq!(PAGE_LIMITS.default, 200);
        assert_eq!(PAGE_LIMITS.max, 500);
    }

    /// A caller's mistake is a 400 naming the parameter they typed, and a driver
    /// failure is not.
    ///
    /// The two arms are asserted together because the *point* of
    /// [`odata_err`] is the boundary between them: a version that collapsed
    /// everything into `Database` would still satisfy either half alone.
    #[test]
    fn a_bad_filter_is_the_callers_and_a_driver_failure_is_not() {
        let bad_filter = odata_err(&ODataError::InvalidFilter(
            "Unknown field: reason".to_owned(),
        ));
        match bad_filter {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "$filter");
                assert!(
                    message.contains("reason"),
                    "the parser's cause must survive into the message: {message}",
                );
            }
            other => panic!("a mistyped filter is the caller's: {other:?}"),
        }

        assert!(matches!(
            odata_err(&ODataError::InvalidOrderByField("run_finished_at".to_owned())),
            DomainError::Validation { ref field, .. } if field == "$orderby"
        ));
        assert!(matches!(
            odata_err(&ODataError::InvalidCursor),
            DomainError::Validation { ref field, .. } if field == "cursor"
        ));
        assert!(matches!(
            odata_err(&ODataError::InvalidLimit),
            DomainError::Validation { ref field, .. } if field == "$top"
        ));

        assert!(matches!(
            odata_err(&ODataError::Db("connection reset".to_owned())),
            DomainError::Database(_)
        ));
        assert!(matches!(
            odata_err(&ODataError::ParsingUnavailable("built without odata")),
            DomainError::Internal(_)
        ));
    }

    /// [`db_err`] is a `Database`, whatever it was handed. Trivial, and here
    /// because the function moved files in this commit and a move is exactly when
    /// a three-line function acquires a typo.
    #[test]
    fn any_storage_error_becomes_a_database_error() {
        assert!(matches!(
            db_err("unique violation on idx_qa_saved_views_name"),
            DomainError::Database(text) if text.contains("unique violation")
        ));
    }
}
