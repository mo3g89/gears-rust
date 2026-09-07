//! Database error conversion helpers.

use toolkit_db::odata::sea_orm_filter::LimitCfg;
use toolkit_odata::Error as ODataError;

use crate::domain::error::DomainError;

/// Default and maximum page size shared by both paginated reads.
///
/// The numbers are the frozen guide's queue contract - *"`limit` defaults to
/// 200 and is clamped to 1-500"* - applied to the runs collection as well.
/// The guide says nothing about the runs listing, so this is a decision rather
/// than a port: one page-size rule across a subsystem's collections is easier
/// for a client to hold than two, and there is no argument for the runs history
/// tolerating a larger page than the queue does.
///
/// `LimitCfg` rather than a hand-rolled clamp, so the floor-of-1 and the
/// ceiling live in `toolkit-db` where every other paginated gear gets them
/// (`libs/toolkit-db/src/odata/sea_orm_filter.rs`, `clamp_limit`).
///
/// # This is where the guide's contract is actually enforced
///
/// `api::rest::dto`'s `QUEUE_LIMIT_*` constants govern only the **legacy
/// `limit` query parameter**. A bare `GET /qa/v1/queue` sends neither `limit`
/// nor `$top`, so its page size comes from `PAGE_LIMITS.default`; a
/// `$top=5000` is clamped solely by `PAGE_LIMITS.max`. The literals are
/// therefore pinned here as well as there - see
/// `tests::the_page_limits_are_the_literals_the_guide_freezes`. They were not,
/// and mutating this pair to `{ default: 15, max: 9999 }` left the whole suite
/// green while `dto.rs` claimed "if one moves, the failing test says which
/// contract changed".
pub const PAGE_LIMITS: LimitCfg = LimitCfg {
    default: 200,
    max: 500,
};

/// Classify an `OData` failure as the caller's mistake or the server's.
///
/// The distinction is the whole reason this is not a blanket
/// [`DomainError::Database`]: a mistyped `$filter` field, a cursor from a
/// different sort order, or a limit outside the configured range are all things
/// the caller sent and can fix, and answering 500 to them would report a client
/// error as a server fault and hide it from anyone reading error rates.
///
/// `Db` and `ParsingUnavailable` are the two that are genuinely not the
/// caller's: the first is a driver failure, the second means the deployment was
/// built without `OData` parsing at all. Both are redacted by the mapping in
/// `api::rest::error`, so neither leaks.
///
/// The `match` **in this function** is exhaustive with no `_` arm, so a new
/// `toolkit_odata::Error` variant is a compile error here rather than silently
/// classified.
///
/// [`odata_field_of`] just below has a `_` arm and is deliberately not
/// exhaustive: it picks a parameter name for a message, where a wrong guess
/// costs a slightly vague 400 rather than a misclassification. Stated because
/// the two sit adjacent and the boast above reads as covering both.
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
        // driver error to a `String` by the time it reaches here (review
        // finding #25 — `DomainError::database` is the sourceless
        // constructor, and this is one of the two places that is the honest
        // answer rather than a shortcut).
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

/// Whether a failed transaction failed *because* something else was writing the
/// same rows, and so can be retried.
///
/// Lives here rather than beside its caller because it is the only place in this
/// gear outside `infra::storage` that would otherwise need `sea_orm` types, and
/// this module's own header reserves those for the infrastructure layer.
///
/// # The error is re-wrapped, not inspected, and that widens the classification
///
/// [`toolkit_db::contention::is_retryable_contention`] takes a `DbErr` and
/// classifies it by the text that `DbErr` renders — for Postgres the SQLSTATE
/// and the `"could not serialize access"` / `"deadlock detected"` wordings,
/// because sqlx surfaces some serialization failures without the numeric code.
/// This function classifies on [`DomainError::Database`]'s `message` — the
/// rendered text — and re-wraps it as a `DbErr::Query` to hand to that
/// library function.
///
/// **Review finding #25 changed what is available here but deliberately not
/// what this function does.** [`db_err`] now keeps the `sea_orm::DbErr` as
/// `.source()`, so a `downcast_ref::<sea_orm::DbErr>()` would recover the
/// typed error and let the real variant dispatch apply. That is a *behaviour*
/// change — it would narrow which errors are retried, which is exactly the
/// widening the next paragraph describes — so it is not made here, where the
/// task was to stop discarding the source. It is now possible, which it was
/// not before; `a_database_error_is_classified_by_text_alone` still pins
/// today's answer.
///
/// **What that costs, stated because the delegation is only partial.**
/// `toolkit_db::contention` matches on `DbErr::Exec | DbErr::Query` and answers
/// `false` for every other variant. Re-wrapping everything as `Query` bypasses
/// that variant filter, so the *patterns* are delegated but the *variant
/// dispatch* is not: any [`DomainError::Database`] whose text happens to contain
/// one of those markers is treated as retryable regardless of where it came
/// from — including [`odata_err`]'s `ODataError::Db` arm, which never held a
/// `DbErr` at all. `a_database_error_is_classified_by_text_alone` pins this.
///
/// **Not established:** whether any caller-controlled string can reach that text.
/// A per-test `test_name` or `test_file` is written through this layer, and
/// nobody has traced whether a database error can quote one back. The blast
/// radius if it can is bounded — at most
/// [`toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS`] attempts of an operation that was
/// already failing, on the caller's own run, with no isolation weakened and no
/// cross-tenant effect — but "bounded" is not "ruled out", and it is not ruled
/// out.
///
/// **What is discharged, and only this.** That a real Postgres serialization
/// failure survives the `to_string()` chain in a form this recognises is
/// measured by exactly one test:
/// `two_concurrent_producers_of_one_test_leave_one_row_and_one_count` turns red
/// when this function is forced to `false`. The other two contended tests do
/// **not** — they still pass with a 40001 unrecognised, because their
/// assertions survive a producer that fails outright. So this is evidence about
/// the text, not about the whole suite depending on it.
///
/// A non-`Database` variant is never retryable: those are this gear's own
/// refusals, and repeating one repeats the refusal.
pub fn is_retryable_contention(error: &DomainError, db: &toolkit_db::Db) -> bool {
    let DomainError::Database { message, .. } = error else {
        return false;
    };
    toolkit_db::contention::is_retryable_contention(
        db.backend(),
        &sea_orm::DbErr::Query(sea_orm::RuntimeErr::Internal(message.clone())),
    )
}

#[cfg(test)]
mod tests {
    use super::PAGE_LIMITS;

    /// The frozen guide's queue contract, as literals: *"`limit` defaults to
    /// 200 and is clamped to 1-500"*.
    ///
    /// `LimitCfg` carries no floor field - `clamp_limit` hardcodes 1 - so the
    /// floor is asserted where it lives rather than here, by
    /// `api::rest::dto::the_queue_limit_constants_are_the_literals_the_guide_freezes`.
    #[test]
    fn the_page_limits_are_the_literals_the_guide_freezes() {
        assert_eq!(PAGE_LIMITS.default, 200);
        assert_eq!(PAGE_LIMITS.max, 500);
    }

    /// [`super::is_retryable_contention`] sees a `String`, so it classifies on
    /// text and nothing else.
    ///
    /// This pins the widening that doc describes rather than leaving it as
    /// prose: a `Database` error that never came from a `DbErr` — the shape
    /// [`super::odata_err`] builds from `ODataError::Db` — is classified
    /// retryable purely because of what its text contains. The real library
    /// call would answer `false` for such an error, because it is not a
    /// `DbErr::Exec` or `DbErr::Query`.
    ///
    /// It is pinned, not fixed. Before review finding #25 there was nothing left
    /// to dispatch on — [`super::db_err`] discarded the typed error. It now
    /// keeps it as `.source()`, so narrowing this is possible; it is a
    /// behaviour change and not part of that finding. See
    /// [`super::is_retryable_contention`]'s own doc.
    #[tokio::test]
    async fn a_database_error_is_classified_by_text_alone() {
        use crate::domain::error::DomainError;
        use crate::infra::storage::test_db::inmem_db;

        let db = inmem_db().await;

        // Not a serialization failure, and not from any `DbErr` — but the
        // marker is in the text, so it is retried.
        assert!(super::is_retryable_contention(
            &DomainError::database("(code: 5) database is locked"),
            &db,
        ));

        // Text with no contention marker is not retried.
        assert!(!super::is_retryable_contention(
            &DomainError::database("relation \"qa_runs\" does not exist"),
            &db,
        ));

        // A non-`Database` variant is never retried, whatever it says.
        assert!(!super::is_retryable_contention(
            &DomainError::Internal("(code: 5) database is locked".to_owned()),
            &db,
        ));
    }
}
