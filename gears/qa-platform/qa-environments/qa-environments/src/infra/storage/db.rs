//! Database error conversion helpers, and the page-size contract the two
//! paginated collection reads share.

use toolkit_db::odata::sea_orm_filter::LimitCfg;
use toolkit_odata::Error as ODataError;

use crate::domain::error::DomainError;

/// Default and maximum page size for every paginated read in this gear.
///
/// **The same pair qa-runs and qa-insights freeze** (`qa-runs`'
/// `infra/storage/db.rs`), and deliberately not a third number: one page-size
/// rule across the qa-platform subsystem is easier for a client to hold than
/// three, and there is no argument for the environment registry tolerating a
/// different page than the run history does.
///
/// `LimitCfg` rather than a hand-rolled clamp, so the floor-of-1 and the
/// ceiling live in `toolkit-db` where every other paginated gear gets them
/// (`libs/toolkit-db/src/odata/sea_orm_filter.rs`, `clamp_limit`).
///
/// # Why the literals are pinned by a test as well as declared here
///
/// `infra::storage::odata_tests::the_page_limits_match_the_other_two_gears`
/// asserts these two numbers against literals — normally the shape review
/// finding #43 calls a defect. It is kept because qa-runs measured the
/// alternative: mutating its pair to `{ default: 15, max: 9999 }` left that
/// gear's **entire suite green** (`qa-runs/src/infra/storage/db.rs:27-33`).
/// Every other test in this gear asserts *against* `PAGE_LIMITS` rather than
/// against a number, so every one of them would happily follow a mutated
/// constant. Read that test's own doc before deleting it.
pub const PAGE_LIMITS: LimitCfg = LimitCfg {
    default: 200,
    max: 500,
};

/// Classify an `OData` failure as the caller's mistake or the server's.
///
/// The distinction is the whole reason this is not a blanket
/// [`DomainError::Database`]: a `$filter` naming a field outside the allow-list,
/// a cursor from a different sort order, or a limit outside the configured range
/// are all things the caller sent and can fix, and answering 500 to them would
/// report a client error as a server fault and hide it from anyone reading error
/// rates. **It is also what makes an unknown `$filter` field a 400 rather than a
/// scan** — the enum refuses it, and this is what turns that refusal into the
/// right status code (review finding #55).
///
/// `Db` and `ParsingUnavailable` are the two that are genuinely not the
/// caller's: the first is a driver failure, the second means the deployment was
/// built without `OData` parsing at all. Both are redacted by the mapping in
/// `api::rest::error`, so neither leaks.
///
/// The `match` **in this function** is exhaustive with no `_` arm, so a new
/// `toolkit_odata::Error` variant is a compile error here rather than silently
/// classified. [`odata_field_of`] just below has a `_` arm and is deliberately
/// not exhaustive: it picks a parameter name for a message, where a wrong guess
/// costs a slightly vague 400 rather than a misclassification.
pub fn odata_err(error: &ODataError) -> DomainError {
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
        // No source to keep: `toolkit_odata` has already flattened its own
        // driver error to a `String` by the time it reaches here, so
        // `DomainError::database` (the sourceless constructor) is the honest
        // answer rather than a shortcut -- cf. [`db_err`] below, which does
        // have a typed error to keep and keeps it.
        ODataError::Db(text) => DomainError::database(text.clone()),
        ODataError::ParsingUnavailable(what) => DomainError::Internal((*what).to_owned()),
    }
}

/// Which query parameter an `OData` error is about.
fn odata_field_of(error: &ODataError) -> &'static str {
    match error {
        ODataError::InvalidFilter(_) | ODataError::FilterMismatch => "$filter",
        ODataError::InvalidOrderByField(_) | ODataError::OrderMismatch => "$orderby",
        ODataError::InvalidLimit => "$top",
        _ => "cursor",
    }
}

/// Convert a storage error into [`DomainError::Database`], keeping the error
/// itself as the `.source()`.
///
/// The bound used to be `impl Display` and the body `e.to_string()`, which
/// dropped the error at the one boundary almost every storage failure in this
/// gear crosses — review finding #25 is about `From<toolkit_db::DbError>`, but
/// fixing only that would have left this helper flattening `sea_orm::DbErr`
/// (SQLSTATE and constraint name included) to a bare message. Every caller
/// already passes a `sea_orm::DbErr`, so the tighter bound cost nothing.
pub fn db_err(e: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database {
        message: e.to_string(),
        source: Some(Box::new(e)),
    }
}
