//! Canonical error mapping: [`DomainError`] to `CanonicalError`.
//!
//! Mirrors `qa-environments/src/api/rest/error.rs`, with two differences that
//! are the whole point of this module:
//!
//! * **The `match` has no catch-all arm**, so adding a [`DomainError`] variant
//!   is a compile error here rather than a silent 500. That is the same
//!   discipline `DomainError::disclosable` already applies to the redaction
//!   rule, and for the same reason: a decision made by absence is not a
//!   decision.
//! * **What a caller may read is decided by `DomainError::disclosable`, not by
//!   this file.** [`opaque_internal`] is the only way an internal-category
//!   error is built here, and it takes the domain error rather than a message
//!   so it cannot be handed pre-formatted text by mistake.
//!
//! # The disclosure rule, restated where it is enforced
//!
//! `Database`, `Catalog`, `Environments`, `ExecutorFailed`, `Internal` and
//! `CorruptState` carry text that originates in another system, in a database
//! driver, or in this gear's internals. None of it may reach an HTTP body:
//! driver text names indexes, columns and key values, and `CorruptState`
//! carries a persisted column's contents verbatim. Each of those is logged at
//! ERROR with the real cause.
//!
//! **What the client receives is the canonical internal detail**, *"An internal
//! error occurred. Please retry later."*, which
//! `toolkit-canonical-errors` supplies and this gear does not choose. Corrected
//! 2026-08-15: this module previously said the caller was "answered with
//! `OPAQUE_ERROR_TEXT`", which is not true of an **error** body - `InternalV1`'s
//! `description` is `#[serde(skip)]`, so that string never reaches one. It is
//! handed to the builder as the *diagnostic*, and the reason that still matters
//! is [`opaque_internal`].
//!
//! Scoped to error bodies deliberately: `OPAQUE_ERROR_TEXT` **does** reach
//! clients, in `RunDto.error` on an ordinary 200, because it is what the domain
//! layer recorded on the row. `api::rest::dto::RunDto::error` says so, and an
//! unqualified "never reaches a body" here would contradict it.
//!
//! # Two false claims shipped in this file, and the rule that would have caught
//! them
//!
//! Both were about the `Validation` arm, both were confident, and the second was
//! written to retract the first.
//!
//! 1. That a schedule's field violation *"cannot be fixed here"* and would need
//!    a new [`DomainError`] variant. [`ScheduleResourceError`] was already
//!    declared 200 lines away, and [`RunResourceError`]'s own visibility note
//!    already described the pattern - a refusal raised where the resource is
//!    known.
//! 2. That the empty-name refusal specifically could not be reached by a handler
//!    `map_err` *"without also re-attributing `ScheduleNameExists`, `Forbidden`,
//!    `Database`"*. The counter-example was the fall-through arm of the very
//!    function being cited, which passes every non-`Validation` through
//!    untouched.
//!
//! A third, in `handlers::schedules`, claimed the crate had no harness able to
//! build a `ConcreteAppServices`; `test_support::Fleet::instance` had been
//! returning one for a task and a half.
//!
//! **The rule, which is operational rather than a resolution to be careful: an
//! impossibility gets a compile-or-test attempt before it gets written, and if
//! the attempt succeeds the sentence becomes the negative.** Each of the three
//! took under two minutes to falsify once anybody tried. Knowing that a
//! correction is where the next false claim lands did not prevent the second
//! one; running the experiment would have.
//!
//! `disclosable()` is the predicate that decides which variants land here. The
//! two `match`es are separate, so nothing but
//! `tests::the_mapping_and_the_disclosure_rule_agree_in_both_directions` stops
//! them drifting apart - see that test for which direction each failure takes
//! and for what it still does not enforce.

use toolkit::api::canonical_prelude::*;
use uuid::Uuid;

use crate::domain::error::{DomainError, OPAQUE_ERROR_TEXT};

/// `pub(crate)` so a handler can raise a refusal this `match` cannot see
/// against the same resource type the mapping uses - one `gts_id` for runs, not
/// two that could disagree. `handlers::runs::subscriber_cap_reached` is the one
/// such refusal: the SSE subscriber cap is a boundary decision with no
/// [`DomainError`] behind it.
///
/// **Corrected 2026-08-17.** This said the visibility was for
/// `domain::local_client`'s *"Phase B refusals"*. Those are gone - Task 20
/// implemented the five schedule methods, so that module raises no error of its
/// own and no longer names this type at all.
#[resource_error(gts_id!("cf.qa.runs.run.v1~"))]
pub(crate) struct RunResourceError;

#[resource_error(gts_id!("cf.qa.runs.queue_entry.v1~"))]
struct QueueResourceError;

/// `qa_schedules`, the third resource type this gear owns.
///
/// **The vocabulary is the plan's, not `DESIGN.md`'s** — corrected here after
/// this comment shipped attributing it to DESIGN §3.7. It is not there: the
/// whole document contains no `qa.queue_entry` and no `qa.run` resource type
/// (its six `qa.run` hits are event topics), and its single `qa.schedule` is the
/// `qa.schedule.fired` *event* in §3.3's **Events** table. What §3.7 names are
/// **tables**. The three resource types are listed in the plan, and the
/// attribution was already correct three files away, in
/// `domain::service::resources`: *"The plan lists three — `qa.run`,
/// `qa.queue_entry` and `qa.schedule`"*. That module remains where the
/// vocabulary is recorded, so the names do not get invented twice.
///
/// Declared by Task 17 rather than by Task 20, which owns the schedules REST
/// layer, because the `match` below is exhaustive: adding
/// [`DomainError::ScheduleNameExists`] to the enum makes this file fail to
/// compile until the variant has an arm, and the arm needs a resource type that
/// is not `run`. A caller told a *run* already exists while creating a schedule
/// would go looking for the wrong row.
#[resource_error(gts_id!("cf.qa.runs.schedule.v1~"))]
struct ScheduleResourceError;

/// Render an error from a **schedule** operation, attributing a field violation
/// to the schedule resource rather than to the run.
///
/// # Why this exists rather than a better `From` impl
///
/// [`DomainError::Validation`] carries a field name and a message and nothing
/// that says which resource the field belongs to, so the `match` below cannot
/// tell `NewScheduleReq`'s `name` from `LaunchRunReq`'s `branch` and maps both
/// to [`RunResourceError`]. Until Task 20 that was harmless — every
/// `Validation` reaching the boundary really was a run's. It stopped being
/// harmless the moment a schedule payload could raise one, and a caller told
/// `cf.qa.runs.run.v1~` rejected their schedule's `name` goes looking for the
/// wrong row.
///
/// **The call site is where the resource is known, and it knows it
/// statically.** That is the same reasoning that makes [`RunResourceError`]
/// `pub(crate)` for `handlers::runs::subscriber_cap_reached`: a refusal the
/// exhaustive `match` cannot see is raised where the facts are, against the one
/// declared type for that resource.
///
/// **Not a field-name heuristic**, which would be the wrong fix: branching on
/// the string `"name"` would silently mis-attribute the next field added on
/// either side — and `target.custom_plan_id`, which both payloads can raise,
/// is exactly the field it would get wrong.
///
/// # Where it is applied, and why each site is sound
///
/// `handlers::schedules` wraps **both** the payload decode and the service call
/// with it:
///
/// * `decode_payload` converts a `NewScheduleReq` and nothing else, so every
///   `Validation` it can produce is a schedule's field, including the ones
///   `RunTargetDto`'s shared `TryFrom` raises. Its signature is concrete rather
///   than generic over `TryInto`, so it cannot later be reused for a run
///   payload; `LaunchRunReq` reaches the same shared `TryFrom` through the runs
///   handler, which never comes here. The two paths are disjoint by type.
/// * `ScheduleService::create` and `::update` produce exactly one `Validation`
///   between them — the empty-name refusal at
///   `domain::service::schedules`'s `validate` — which is measured rather than
///   assumed: it is the only `Validation` producer anywhere in that call graph.
///   `mapper`'s `db_i32_from_i64` is the one generic helper that could
///   theoretically appear and does not; it is reached only from
///   `runs_sea_repo`'s counter-delta write.
///
/// `domain::local_client` is the **third** site and shipped without it, which
/// is the whole reason this section exists as a list rather than as "the
/// schedule handlers". It reaches the same `ScheduleService` methods by a
/// different door, so every argument above applies to it unchanged; that door
/// simply had no `map_err` at all. `client::tests` drives all five methods.
///
/// # `Forbidden` is re-attributed too, and it is the one that matters most
///
/// A denial is the **most likely** error on a fresh deployment of these
/// endpoints — a policy engine that has not been taught `qa.schedule` yet
/// refuses every one of them — and it was pointing the operator at *run*
/// permissions. That is worse than the field violations: a 403 carries no field
/// to disambiguate it, so the resource type is the only thing in the body an
/// operator can act on.
///
/// It stays `permission_denied` with the same `ACCESS_DENIED` reason and still
/// says nothing about *what* was denied — the cross-tenant oracle every read in
/// this gear closes. Only the resource type changes.
///
/// # Everything else falls through, which is what makes the wrapper cheap
///
/// Anything that is neither a `Validation` nor a `Forbidden` is handed to the
/// ordinary mapping untouched, so wrapping a whole service call leaves
/// `ScheduleNameExists`, `ScheduleNotFound`, `Database` and `CorruptState`
/// exactly as they were — the first two already carry the schedule's own type.
/// It is also why a later variant becomes an ordinary error here rather than a
/// wrongly-attributed one.
pub(crate) fn as_schedule_error(e: DomainError) -> CanonicalError {
    match e {
        DomainError::Validation { field, message } => ScheduleResourceError::invalid_argument()
            .with_field_violation(field, message, "VALIDATION")
            .create(),
        DomainError::Forbidden => ScheduleResourceError::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create(),
        other => other.into(),
    }
}

/// Render an error from a **queue** operation, attributing it to the queue
/// entry the caller addressed rather than to the run behind it.
///
/// The third of the three wrappers, and the last one missing. `/qa/v1/queue`'s
/// three handlers used a bare `?` throughout, so every refusal they could raise
/// took whatever resource type the exhaustive `match` below happens to assign —
/// which is `run` for three of them, on endpoints whose scopes are compiled for
/// [`resources::QUEUE_ENTRY`](crate::domain::service::resources::QUEUE_ENTRY).
///
/// `addressed` is the queue id from the path, or `None` for the collection
/// endpoint. It is a parameter rather than something recovered from the error
/// because it is what the *request* named, which is the whole point: two of the
/// arms below exist to stop the body quoting an id the caller never sent.
///
/// # The four arms, and why each is one
///
/// * **`Forbidden`** — the one that matters most, for the reason
///   [`as_schedule_error`] gives at length: a 403 carries no field, a fresh
///   deployment whose policy has not been taught `qa.queue_entry` refuses all
///   three of these endpoints, and the resource type is then the only thing in
///   the body an operator can act on. It was naming `qa.run`.
/// * **`ConcurrencyLimit`** — raisable from `launch` *and* from
///   `RunsService::force_start`, so the enum variant cannot carry one resource
///   type. The default mapping keeps it run-typed, which is right for the
///   launch endpoint; `POST /queue/{id}/force-start` is the caller that has to
///   re-attribute, and the actionable content (`max_concurrent_runs`) survives
///   unchanged either way.
/// * **`RunNotFound`** — `cancel_queued` reads the row's run under the caller's
///   own `qa.run`/`cancel` scope, so a caller who may drop a queue row but may
///   not cancel its run was answered *"Run {uuid} was not found"* for a uuid
///   they never supplied. Re-pointed at the queue row they did. **This is
///   attribution, not an oracle fix**: `QueueEntryDto::run_id` means the
///   row-to-run association is already readable by anyone who can list the row.
/// * **`Validation`** — the same field-without-a-resource problem
///   [`as_schedule_error`] documents. `QueueQuery::reject_legacy_field`
///   raises one for a caller still sending `platform_id` (ruling G-4); this
///   arm is what keeps that refusal attributed to the queue entry rather than
///   silently becoming a run's.
///
/// # What is deliberately left alone
///
/// `IllegalTransition` keeps its run attribution and its run id. It is
/// reachable here only through `cancel_queued` losing a race with the
/// dispatcher, and its content — *"Run {id} is in state {state} and cannot be
/// cancelled"* — is a statement about the run, whose state is the only
/// actionable part of it. Re-pointing it at the queue row would keep the
/// wrong id and lose the reason.
///
/// Everything else falls through untouched, which is what keeps the wrapper
/// cheap: `QueueRowNotFound`, `QueueRowNotQueued` and `QueueRowExists` already
/// carry this type, and `Database`/`CorruptState` stay opaque.
pub(crate) fn as_queue_error(addressed: Option<Uuid>, e: DomainError) -> CanonicalError {
    match e {
        DomainError::Forbidden => QueueResourceError::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create(),
        DomainError::Validation { field, message } => QueueResourceError::invalid_argument()
            .with_field_violation(field, message, "VALIDATION")
            .create(),
        DomainError::ConcurrencyLimit { limit } => {
            let refusal = QueueResourceError::resource_exhausted(format!(
                "Max concurrent runs limit reached ({limit})"
            ))
            .with_quota_violation(
                "max_concurrent_runs",
                format!("the cluster-wide limit of {limit} concurrent runs is reached"),
            );
            match addressed {
                Some(id) => refusal.with_resource(id.to_string()).create(),
                None => refusal.create(),
            }
        }
        // Only re-pointed when the request named a row. Without one there is
        // nothing truer to say than what the ordinary mapping already says.
        DomainError::RunNotFound { .. } => match addressed {
            Some(id) => QueueResourceError::not_found(format!("Queue row {id} was not found"))
                .with_resource(id.to_string())
                .create(),
            None => e.into(),
        },
        other => other.into(),
    }
}

/// A 500 whose body says nothing, with the real cause logged.
///
/// Takes the [`DomainError`] rather than a string on purpose: a signature that
/// accepted a message would make it possible - and, on a busy day, likely - to
/// pass `e.to_string()`, which is exactly the leak this exists to prevent.
///
/// # Why the diagnostic is the opaque text and not the cause
///
/// The string handed to `CanonicalError::internal` becomes the error's
/// *diagnostic*, which `Problem::from_error` drops and
/// `Problem::from_error_debug` copies into `context["description"]`. The debug
/// renderer is documented "MUST NOT be used in production", but nothing in this
/// crate selects a renderer, so a later change could. Handing it
/// [`OPAQUE_ERROR_TEXT`] means even that renderer cannot leak - which
/// `tests::the_mapping_and_the_disclosure_rule_agree_in_both_directions` checks
/// under both renderings rather than trusting the warning to hold.
///
/// # It reports a classification disagreement rather than assuming there is none
///
/// A variant routed here that `disclosable()` calls safe is a contradiction
/// between two rules that are supposed to agree. This logs it at ERROR rather
/// than letting the milder of the two win silently - and the log line is
/// asserted, by `tests::a_classification_disagreement_is_reported`, because a
/// guarantee carried only by an unobserved log line is not a guarantee.
fn opaque_internal(e: &DomainError) -> CanonicalError {
    if e.disclosable() {
        tracing::error!(
            error = %e,
            "a disclosable error was mapped to an opaque 500; the mapping and \
             DomainError::disclosable disagree about this variant"
        );
    }
    tracing::error!(error = %e, "qa-runs internal error");
    CanonicalError::internal(OPAQUE_ERROR_TEXT).create()
}

impl From<DomainError> for CanonicalError {
    #[allow(
        clippy::cognitive_complexity,
        reason = "one arm per DomainError variant, each a single builder chain. The \
                  metric counts every arm as a branch, so the score tracks the size of \
                  the enum rather than the difficulty of the code. Splitting it would \
                  break the exhaustiveness that makes a new variant a compile error, \
                  which is this function's main safety property. Same diagnosis as \
                  `qa-environments/src/api/rest/error.rs`, which carries the bare allow."
    )]
    fn from(e: DomainError) -> Self {
        match &e {
            // -- 404 --------------------------------------------------------
            // Absent and foreign are indistinguishable by construction; see
            // `RunsRepository::resolve_owned`. Nothing here may add a
            // distinguishing detail.
            DomainError::RunNotFound { id } => {
                RunResourceError::not_found(format!("Run {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }
            DomainError::QueueRowNotFound { id } => {
                QueueResourceError::not_found(format!("Queue row {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }
            // Absent and foreign are indistinguishable here too; see
            // `SchedulesRepository::resolve_owned_schedule`.
            DomainError::ScheduleNotFound { id } => {
                ScheduleResourceError::not_found(format!("Schedule {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }

            // -- 409, already exists ----------------------------------------
            DomainError::RunNameExists { name } => {
                RunResourceError::already_exists(format!("Run '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }
            DomainError::ScheduleNameExists { name } => {
                ScheduleResourceError::already_exists(format!("Schedule '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }
            DomainError::QueueRowExists { run_id } => {
                QueueResourceError::already_exists(format!("Run {run_id} already has a queue row"))
                    .with_resource(run_id.to_string())
                    .create()
            }

            // -- 409, aborted -----------------------------------------------
            // Both carry a state that was observed *after* the guarded write
            // failed, so it is advisory - the row may have moved again. It is
            // rendered because a message has to name something; nothing may
            // branch on it, and in particular a client must not retry because
            // the state "looked retryable" (see `domain::repos`).
            DomainError::IllegalTransition { id, state, action } => {
                RunResourceError::aborted(format!(
                    "Run {id} is in state {} and cannot {action}",
                    state.as_str()
                ))
                .with_reason("ILLEGAL_TRANSITION")
                .with_resource(id.to_string())
                .create()
            }
            DomainError::QueueRowNotQueued { id, state } => QueueResourceError::aborted(format!(
                "Queue row {id} is {}, not queued",
                state.as_str()
            ))
            .with_reason("NOT_QUEUED")
            .with_resource(id.to_string())
            .create(),

            // -- 400, failed precondition -----------------------------------
            // `failed_precondition` renders **400**, not 409, and that is the
            // answer the source system gives too (`StatusCode::BAD_REQUEST` at
            // `manager/src/routes/custom_plans.rs:681`). Nothing about the
            // server's state conflicts: the caller fixes it by naming a branch.
            // The route registration must therefore declare `error_400` for
            // this, not `error_409`.
            DomainError::AmbiguousBranch { plan_id, groups } => {
                RunResourceError::failed_precondition()
                    .with_precondition_violation(
                        "branch",
                        format!(
                            "Custom plan {plan_id} spans {groups} repositories, so a branch \
                             must be given explicitly: no single repository default applies"
                        ),
                        "AMBIGUOUS_BRANCH",
                    )
                    .with_resource(plan_id.to_string())
                    .create()
            }

            // -- 429 --------------------------------------------------------
            // Two variants rather than one because the frozen guide gives each
            // of the launch path's 429 causes its own knob, and naming which
            // setting an operator has to change is the only actionable content
            // either message has.
            //
            // **Both are run-typed, and that is what raises them.**
            // `QueueFull` is constructed in exactly one place —
            // `service::admission`'s `admit` — which is on the launch path and
            // nowhere else, so the caller who meets it addressed `POST
            // /qa/v1/runs` or its re-run. It used to answer
            // `cf.qa.runs.queue_entry.v1~` with a **platform** uuid as the
            // resource name, so a caller who had asked about a run got a body
            // naming three different things and could act on none of them.
            // `ConcurrencyLimit` is also raisable from
            // `RunsService::force_start`, whose caller addressed a queue row;
            // that endpoint re-attributes through `as_queue_error`, because a
            // variant that two surfaces can raise cannot carry one resource
            // type of its own.
            //
            // Neither names a resource *instance*: the variants carry an
            // environment id and a cluster-wide limit respectively, and neither
            // is a run this gear could name. The environment stays in the
            // violation description, where it is a fact about the refusal
            // rather than a claim about which row the caller should go and
            // look at.
            DomainError::QueueFull {
                platform_id,
                queued,
                limit,
            } => RunResourceError::resource_exhausted(format!(
                "Run queue for environment {platform_id} is full: {queued} of {limit} slots used"
            ))
            // The subject is the *setting*, which is what an operator changes.
            .with_quota_violation(
                "queue_max_depth",
                format!("{queued} of {limit} queued rows for environment {platform_id}"),
            )
            .create(),
            DomainError::ConcurrencyLimit { limit } => RunResourceError::resource_exhausted(
                format!("Max concurrent runs limit reached ({limit})"),
            )
            .with_quota_violation(
                "max_concurrent_runs",
                format!("the cluster-wide limit of {limit} concurrent runs is reached"),
            )
            .create(),

            // -- 400, invalid argument --------------------------------------
            //
            // **The run is this arm's answer because the variant names no
            // resource, so every caller whose resource is not a run must
            // attribute its own** - `as_schedule_error` and `as_queue_error`
            // above.
            //
            // **What this arm is, and is not.** It is the default for a caller
            // that did not re-attribute; it is *not* evidence that no schedule
            // or queue field can reach it. Three surfaces owe attribution and
            // each has to discharge it separately: `handlers::schedules`,
            // `handlers::queue`, and `domain::local_client`, which is the one
            // that shipped without it and handed an in-process caller a
            // schedule's empty-name refusal as a run's. What holds the property
            // is a driven test per surface -
            // `handlers::{runs,queue,schedules}_handler_tests` and
            // `local_client::client::tests` - reading the `gts_id` out of the
            // rendered body, because nothing in the type system pairs a caller
            // with a resource type.
            //
            // This comment has been the defect three times, each time as an
            // absence claim about what "nothing" reaches; the module header
            // carries the history and the rule that would have caught all
            // three.
            DomainError::Validation { field, message } => RunResourceError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION")
                .create(),
            // A schedule's field, so `ScheduleResourceError` rather than
            // `RunResourceError` - a caller told a *run* rejected their cron
            // would look at the wrong resource. Both halves of the error are
            // rendered as the violation description: `expression` is what the
            // caller sent and `message` is this gear's own sentence about it
            // (`DomainError::InvalidCron`), and splitting them would drop one.
            DomainError::InvalidCron { .. } => ScheduleResourceError::invalid_argument()
                .with_field_violation("cron", e.to_string(), "INVALID_CRON")
                .create(),
            // The parameter's own name is inside the wrapped `ParamError`, and
            // `cpt-cf-qa-fr-runs-params` requires the failure to name it - so
            // the whole rendered error is the violation description rather than
            // being re-split into a field and a message this layer would have
            // to guess at.
            DomainError::InvalidParameters(inner) => RunResourceError::invalid_argument()
                .with_field_violation("parameters", inner.to_string(), "INVALID_PARAMETERS")
                .create(),

            // -- 403 --------------------------------------------------------
            // Never carries what was denied: that is the cross-tenant oracle
            // every read on this gear is written to close.
            DomainError::Forbidden => RunResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),

            // -- 500, opaque ------------------------------------------------
            // Exactly the variants `DomainError::disclosable` classifies as
            // unsafe to echo, in the order that method lists them, so the two
            // can be compared by reading. One or-pattern rather than an arm per
            // variant because `clippy::match_same_arms` is denied in this
            // workspace and every body here is the same call.
            //
            // `Catalog` and `Environments` are the two cross-gear reads, and
            // neither is `unavailable`/503: a failure here is as often "the plan
            // does not resolve" as "the gear is down", this layer cannot tell
            // which, and answering 503 to the first would invite a client to
            // retry something that will never succeed.
            DomainError::Database(_)
            | DomainError::Internal(_)
            | DomainError::ExecutorFailed(_)
            | DomainError::CorruptState { .. }
            | DomainError::Catalog(_)
            | DomainError::Environments(_) => opaque_internal(&e),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Every [`DomainError`] variant's category and HTTP status, plus the
    //! negative half: that no 500 body carries the text it was built from.
    //!
    //! Exhaustiveness is the compiler's job - the `match` above has no `_` arm.
    //! What these pin is the *classification*, which the compiler cannot check,
    //! and the disclosure, which is a property of the rendered body rather than
    //! of the type.

    use qa_runs_sdk::{QueueState, RunState};
    use uuid::Uuid;

    use super::{CanonicalError, DomainError};
    use crate::domain::error::OPAQUE_ERROR_TEXT;
    use toolkit::api::canonical_prelude::Problem;

    /// What the client actually receives, as JSON.
    ///
    /// **`Problem::from_error` and not `format!("{ce:?}")`.** The `Debug`
    /// rendering was what these tests asserted against until 2026-08-15, and it
    /// is the wrong artifact: it is a superset of the wire, so the negative
    /// assertions held, but a leak introduced in the real rendering path would
    /// not have shown up in it at all. `Problem` is the type that becomes the
    /// response body.
    fn wire(e: DomainError) -> (u16, String) {
        let ce: CanonicalError = e.into();
        let problem = Problem::from_error(&ce).expect("a problem must serialize");
        let status = problem.status;
        (
            status,
            serde_json::to_string(&problem).expect("a problem must serialize"),
        )
    }

    /// The wire rendering under **both** renderers, for the drift guard.
    ///
    /// The second is `Problem::from_error_debug`, which
    /// `toolkit-canonical-errors` documents as "MUST NOT be used in production"
    /// because it copies the internal diagnostic into `context["description"]`.
    /// Asserted anyway, and deliberately: nothing in this crate selects a
    /// renderer, so a later change could. Handing `opaque_internal` the opaque
    /// text as the *diagnostic* - rather than the real cause - is what makes
    /// even that renderer safe, and that is worth pinning rather than relying on
    /// nobody ever switching.
    fn both_renderings(e: DomainError) -> (u16, String, String) {
        let ce: CanonicalError = e.into();
        let problem = Problem::from_error(&ce).expect("a problem must serialize");
        let status = problem.status;
        let body = serde_json::to_string(&problem).expect("a problem must serialize");
        let debug_body = serde_json::to_string(
            &Problem::from_error_debug(&ce).expect("a problem must serialize"),
        )
        .expect("a problem must serialize");
        (status, body, debug_body)
    }

    /// Kept for the per-variant tests below, which assert one status and one
    /// message each; it renders the same wire body [`wire`] does.
    fn rendered(e: DomainError) -> (u16, String) {
        wire(e)
    }

    /// The sentinel every sample carries in every free-text field it has.
    ///
    /// A real Postgres unique-violation message, which is what
    /// `From<toolkit_db::DbError>` puts into `Database` verbatim: it names an
    /// index, a column list, and a key value that is another tenant's id.
    const SENTINEL: &str = "duplicate key value violates unique constraint \
                            \"idx_qa_run_queue_tenant_run\" DETAIL: Key \
                            (tenant_id, run_id)=(0a11ce11-dead-beef) already exists";

    /// A stable name per variant, from an **exhaustive** `match`.
    ///
    /// This is the compile-time half of the drift guard: adding a
    /// [`DomainError`] variant fails to build *here*, in the test module, which
    /// is what puts an author in front of [`samples`] at the moment they add
    /// one.
    ///
    /// **What it does not enforce**, stated because the previous version of this
    /// module claimed a completeness it did not have: nothing makes the author
    /// then *add* the sample. The compile error is a prompt, not a proof. A
    /// variant added to the enum and to this `match` but not to [`samples`] is
    /// simply untested, silently.
    fn variant_tag(e: &DomainError) -> &'static str {
        match e {
            DomainError::RunNotFound { .. } => "RunNotFound",
            DomainError::QueueRowNotFound { .. } => "QueueRowNotFound",
            DomainError::ScheduleNotFound { .. } => "ScheduleNotFound",
            DomainError::Validation { .. } => "Validation",
            DomainError::InvalidCron { .. } => "InvalidCron",
            DomainError::InvalidParameters(_) => "InvalidParameters",
            DomainError::CorruptState { .. } => "CorruptState",
            DomainError::RunNameExists { .. } => "RunNameExists",
            DomainError::ScheduleNameExists { .. } => "ScheduleNameExists",
            DomainError::QueueRowExists { .. } => "QueueRowExists",
            DomainError::IllegalTransition { .. } => "IllegalTransition",
            DomainError::QueueRowNotQueued { .. } => "QueueRowNotQueued",
            DomainError::ExecutorFailed(_) => "ExecutorFailed",
            DomainError::QueueFull { .. } => "QueueFull",
            DomainError::ConcurrencyLimit { .. } => "ConcurrencyLimit",
            DomainError::AmbiguousBranch { .. } => "AmbiguousBranch",
            DomainError::Catalog(_) => "Catalog",
            DomainError::Environments(_) => "Environments",
            DomainError::Forbidden => "Forbidden",
            DomainError::Database(_) => "Database",
            DomainError::Internal(_) => "Internal",
        }
    }

    /// One sample per variant, each carrying [`SENTINEL`] wherever it has a
    /// free-text field, plus a marker a *disclosable* variant must render.
    ///
    /// `None` for `Forbidden`, which carries nothing to disclose - the
    /// assertion for it is that it is neither a 500 nor opaque.
    fn samples() -> Vec<(DomainError, Option<String>)> {
        let id = Uuid::from_u128(0x5EED);
        vec![
            (DomainError::RunNotFound { id }, Some(id.to_string())),
            (DomainError::QueueRowNotFound { id }, Some(id.to_string())),
            (DomainError::ScheduleNotFound { id }, Some(id.to_string())),
            (
                DomainError::Validation {
                    field: "timeout_seconds".to_owned(),
                    message: "is 31536000; the maximum is 86400".to_owned(),
                },
                Some("timeout_seconds".to_owned()),
            ),
            (
                crate::domain::cron::parse_cron("99 * * * *")
                    .expect_err("minute 99 must not validate"),
                Some("99 * * * *".to_owned()),
            ),
            (
                DomainError::InvalidParameters(
                    crate::domain::params::validate(
                        &[qa_runs_sdk::RunParameter {
                            name: String::new(),
                            value: "v".to_owned(),
                        }],
                        &qa_product_sdk::access::RunVarContract::default(),
                    )
                    .expect_err("an empty parameter name must not validate"),
                ),
                Some("parameters".to_owned()),
            ),
            (
                DomainError::CorruptState {
                    what: "run.state",
                    id,
                    value: SENTINEL.to_owned(),
                },
                None,
            ),
            (
                DomainError::RunNameExists {
                    name: "smoke-7".to_owned(),
                },
                Some("smoke-7".to_owned()),
            ),
            (
                DomainError::ScheduleNameExists {
                    name: "nightly".to_owned(),
                },
                Some("nightly".to_owned()),
            ),
            (
                DomainError::QueueRowExists { run_id: id },
                Some(id.to_string()),
            ),
            (
                DomainError::IllegalTransition {
                    id,
                    state: RunState::Succeeded,
                    action: "be cancelled".to_owned(),
                },
                Some(id.to_string()),
            ),
            (
                DomainError::QueueRowNotQueued {
                    id,
                    state: QueueState::Running,
                },
                Some(id.to_string()),
            ),
            (DomainError::ExecutorFailed(SENTINEL.to_owned()), None),
            (
                DomainError::QueueFull {
                    platform_id: id,
                    queued: 20,
                    limit: 20,
                },
                Some("queue_max_depth".to_owned()),
            ),
            (
                DomainError::ConcurrencyLimit { limit: 50 },
                Some("max_concurrent_runs".to_owned()),
            ),
            (
                DomainError::AmbiguousBranch {
                    plan_id: id,
                    groups: 3,
                },
                Some(id.to_string()),
            ),
            (DomainError::Catalog(SENTINEL.to_owned()), None),
            (DomainError::Environments(SENTINEL.to_owned()), None),
            (DomainError::Forbidden, None),
            (DomainError::Database(SENTINEL.to_owned()), None),
            (DomainError::Internal(SENTINEL.to_owned()), None),
        ]
    }

    /// **The drift guard, in both directions.**
    ///
    /// `DomainError::disclosable` is the redaction rule for everything this gear
    /// writes down; this module renders the same errors for HTTP. The two are
    /// separate `match`es, so nothing but this test stops them disagreeing - and
    /// the disagreement is silent either way round. Both directions are checked:
    ///
    /// * **Classified unsafe, rendered in full** - the leak. A variant moved
    ///   into `disclosable()`'s `=> false` arm while keeping a disclosing arm
    ///   here would ship its text to clients.
    /// * **Classified safe, rendered opaque** - the inverse, and not harmless:
    ///   `queue_max_depth` and `max_concurrent_runs` are the only actionable
    ///   content their 429s have, and redacting them turns a fixable refusal
    ///   into a shrug.
    ///
    /// Falsified by moving `Validation` into `disclosable()`'s `=> false` arm -
    /// the mutation that passed every test this module had at the time.
    #[test]
    fn the_mapping_and_the_disclosure_rule_agree_in_both_directions() {
        for (error, marker) in samples() {
            let tag = variant_tag(&error);
            let disclosable = error.disclosable();
            let (status, body, debug_body) = both_renderings(error);

            if disclosable {
                assert_ne!(
                    status, 500,
                    "{tag} is classified disclosable but renders as an internal error"
                );
                if let Some(marker) = marker {
                    assert!(
                        body.contains(&marker),
                        "{tag} is classified disclosable but {marker:?} did not survive \
                         to the wire: {body}"
                    );
                }
            } else {
                assert_eq!(status, 500, "{tag} is classified undisclosable");
                for (label, rendered) in [("wire", &body), ("debug wire", &debug_body)] {
                    // The **whole** sentinel, not a hand-picked subset. The
                    // subset that shipped had dropped `"tenant_id"` - the
                    // cross-tenant marker specifically - which is exactly the
                    // substring a reviewer would most want asserted.
                    assert!(
                        !rendered.contains(SENTINEL),
                        "{tag} leaked the sentinel into the {label} body: {rendered}"
                    );
                    for leak in [
                        "duplicate key",
                        "idx_qa_run_queue_tenant_run",
                        "0a11ce11",
                        "tenant_id",
                        "run.state",
                    ] {
                        assert!(
                            !rendered.contains(leak),
                            "{tag} leaked {leak:?} into the {label} body: {rendered}"
                        );
                    }
                }
            }
        }
    }

    /// **The contradiction report, asserted rather than assumed.**
    ///
    /// `opaque_internal` logs when it is handed an error `disclosable()` calls
    /// safe. That was the only thing standing behind the word "asserts" in its
    /// doc, and nothing observed it: inverting the condition to
    /// `if !e.disclosable()` left every test this module had at the time green.
    ///
    /// A disclosable error is passed here directly - the production `match`
    /// never routes one this way, which is the point: this pins the *detector*,
    /// so that if a future mapping does route one here, the operator finds out.
    #[test]
    #[tracing_test::traced_test]
    fn a_classification_disagreement_is_reported() {
        let safe = DomainError::Forbidden;
        assert!(
            safe.disclosable(),
            "premise: this test needs an error the rule calls safe"
        );
        let _rendered = super::opaque_internal(&safe);

        assert!(
            logs_contain("the mapping and DomainError::disclosable disagree"),
            "a disagreement between the two rules must be reported, not resolved \
             silently in favour of the milder one"
        );
    }

    /// The sample list is hand-written, so at least assert it has no duplicates
    /// and that every entry is the variant its tag names.
    #[test]
    fn every_variant_is_sampled_exactly_once() {
        let mut tags: Vec<&str> = samples()
            .iter()
            .map(|(error, _)| variant_tag(error))
            .collect();
        let before = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(before, tags.len(), "a variant is sampled twice: {tags:?}");

        // **The count, which is what makes the list complete rather than
        // merely tidy.** `variant_tag` is exhaustive, so adding a `DomainError`
        // variant is already a compile error there - but nothing made the
        // author then add a *sample*, and deleting the `Database` entry (the
        // single most important leak vector, the one carrying driver text
        // verbatim) left the suite green.
        //
        // A count in a test, deliberately, where this project bans them in doc
        // comments: a doc comment's count rots silently, whereas this one fails
        // the moment the enum and the list disagree, which is the whole job.
        assert_eq!(
            tags.len(),
            DOMAIN_ERROR_VARIANTS,
            "every DomainError variant must be sampled: {tags:?}"
        );
    }

    /// How many variants `DomainError` has.
    ///
    /// Paired with the exhaustive `variant_tag` match: a new variant fails to
    /// compile there, and fails this count here, so the two together make the
    /// sample list complete in both directions.
    const DOMAIN_ERROR_VARIANTS: usize = 21;

    #[test]
    fn run_not_found_is_404() {
        let id = Uuid::from_u128(0x11);
        let ce: CanonicalError = DomainError::RunNotFound { id }.into();
        assert_eq!(ce.status_code(), 404);
        assert!(matches!(ce, CanonicalError::NotFound { .. }), "{ce:?}");
    }

    #[test]
    fn queue_row_not_found_is_404() {
        let ce: CanonicalError = DomainError::QueueRowNotFound {
            id: Uuid::from_u128(0x12),
        }
        .into();
        assert_eq!(ce.status_code(), 404);
        assert!(matches!(ce, CanonicalError::NotFound { .. }), "{ce:?}");
    }

    #[test]
    fn run_name_exists_is_409_already_exists() {
        let ce: CanonicalError = DomainError::RunNameExists {
            name: "smoke-7".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::AlreadyExists { .. }), "{ce:?}");
    }

    #[test]
    fn queue_row_exists_is_409_already_exists() {
        let ce: CanonicalError = DomainError::QueueRowExists {
            run_id: Uuid::from_u128(0x13),
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::AlreadyExists { .. }), "{ce:?}");
    }

    #[test]
    fn an_illegal_transition_is_409_aborted() {
        let ce: CanonicalError = DomainError::IllegalTransition {
            id: Uuid::from_u128(0x14),
            state: RunState::Succeeded,
            action: "be cancelled".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::Aborted { .. }), "{ce:?}");
    }

    #[test]
    fn a_row_that_left_queued_is_409_aborted() {
        let ce: CanonicalError = DomainError::QueueRowNotQueued {
            id: Uuid::from_u128(0x15),
            state: QueueState::Running,
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(matches!(ce, CanonicalError::Aborted { .. }), "{ce:?}");
    }

    /// **400, not 409.** The route table registers `error_400` for this, and
    /// the source system answers `BAD_REQUEST` too. Getting it wrong is
    /// invisible without this assertion, because both are still `Err`.
    #[test]
    fn an_ambiguous_branch_is_400_failed_precondition() {
        let ce: CanonicalError = DomainError::AmbiguousBranch {
            plan_id: Uuid::from_u128(0x16),
            groups: 3,
        }
        .into();
        assert_eq!(ce.status_code(), 400);
        assert!(
            matches!(ce, CanonicalError::FailedPrecondition { .. }),
            "{ce:?}"
        );
    }

    /// Both 429 causes, and both messages naming their knob - which is the only
    /// content an operator can act on.
    ///
    /// **Both are attributed to the run, because the launch path is what raises
    /// them.** `QueueFull` used to be queue-typed and to carry a *platform*
    /// uuid as its resource name, so a caller who had asked about a run got a
    /// body naming three different resources; it is constructible in exactly
    /// one place, `service::admission`'s `admit`, which is on the launch path.
    /// `ConcurrencyLimit` is also raisable from `force_start`, and that
    /// endpoint re-attributes through `as_queue_error` -
    /// `handlers::queue`'s `handler_tests` pins that half.
    #[test]
    fn both_capacity_refusals_are_429_and_name_their_setting() {
        let platform = Uuid::from_u128(0x17);
        let (status, body) = rendered(DomainError::QueueFull {
            platform_id: platform,
            queued: 20,
            limit: 20,
        });
        assert_eq!(status, 429);
        assert!(body.contains("queue_max_depth"), "{body}");
        assert!(
            body.contains("cf.qa.runs.run.v1~"),
            "the launch path's refusal must name the run the caller launched: {body}"
        );
        assert!(
            !body.contains("cf.qa.runs.queue_entry.v1~"),
            "and not a queue row the caller never addressed: {body}"
        );
        assert!(
            !body.contains(&format!("\"resource_name\":\"{platform}\"")),
            "an environment id is not a resource this gear owns, so it must not be \
             the body's resource name: {body}"
        );

        let (status, body) = rendered(DomainError::ConcurrencyLimit { limit: 50 });
        assert_eq!(status, 429);
        assert!(body.contains("max_concurrent_runs"), "{body}");
        assert!(body.contains("50"), "{body}");
        assert!(body.contains("cf.qa.runs.run.v1~"), "{body}");
    }

    #[test]
    fn a_validation_failure_is_400_invalid_argument_naming_its_field() {
        let (status, body) = rendered(DomainError::Validation {
            field: "timeout_seconds".to_owned(),
            message: "is 31536000; it must be between 1 and 86400 seconds".to_owned(),
        });
        assert_eq!(status, 400);
        assert!(body.contains("timeout_seconds"), "{body}");
        assert!(body.contains("86400"), "the ceiling must survive: {body}");
    }

    /// **The offending parameter's own name reaches the body**, which is what
    /// `cpt-cf-qa-fr-runs-params` requires and what this test's name claims.
    ///
    /// It did not test that. It built the sample from `ParamError::EmptyName` -
    /// the one variant that by design carries no name - and asserted only the
    /// literal `"parameters"`, which is the *field-violation key* this arm
    /// hardcodes. Measured: deleting `inner.to_string()` from the violation
    /// description, so the body named the parameter nothing at all, left it
    /// green.
    ///
    /// The sample is now a violation that has a name to lose. `EmptyName` is
    /// still covered - it is the `InvalidParameters` entry in [`samples`] - and
    /// the two are complementary: that one pins the key, this one pins the
    /// name.
    #[test]
    fn invalid_launch_parameters_are_400_and_name_the_parameter() {
        let inner = crate::domain::params::validate(
            &[qa_runs_sdk::RunParameter {
                name: "9lives".to_owned(),
                value: "v".to_owned(),
            }],
            &qa_product_sdk::access::RunVarContract::default(),
        )
        .expect_err("a name that does not start with a letter or '_' must not validate");
        assert!(
            matches!(inner, crate::domain::params::ParamError::InvalidName { .. }),
            "premise: the sample must be a variant that carries a name: {inner:?}"
        );

        let (status, body) = rendered(DomainError::InvalidParameters(inner));
        assert_eq!(status, 400);
        assert!(
            body.contains("parameters"),
            "the field-violation key: {body}"
        );
        assert!(
            body.contains("9lives"),
            "and the parameter the caller has to fix: {body}"
        );
    }

    #[test]
    fn forbidden_is_403_and_says_nothing_about_what_was_denied() {
        let (status, body) = rendered(DomainError::Forbidden);
        assert_eq!(status, 403);
        for oracle in ["tenant", "scope", "platform", "run "] {
            assert!(
                !body.to_lowercase().contains(oracle),
                "a denial must not describe what was denied ({oracle}): {body}"
            );
        }
    }

    /// The other direction, so the redaction cannot be "fixed" by redacting
    /// everything: a caller's own refusal keeps its detail, because that detail
    /// is the only thing they can act on.
    #[test]
    fn a_callers_own_refusal_is_not_redacted() {
        let (_, body) = rendered(DomainError::RunNameExists {
            name: "smoke-7".to_owned(),
        });
        assert!(body.contains("smoke-7"), "{body}");
        assert!(!body.contains(OPAQUE_ERROR_TEXT), "{body}");
    }
}
