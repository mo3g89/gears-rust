//! **Where a domain error gets its resource attribution**, for the refusals
//! whose resource type the exhaustive `DomainError -> CanonicalError` mapping
//! cannot see.
//!
//! # Why this is a domain module and not part of `api::rest::error`
//!
//! It was part of it, and that put a *domain* module — `domain::local_client`,
//! moved here from `infra/` by the plugin rework — in the position of
//! importing `crate::api::rest::error` to attribute its own errors. A local
//! client is an in-process call that never touches HTTP; its error attribution
//! must not come from the HTTP layer, and `api` is a transport over `domain`
//! rather than the other way round. Review findings #15, #16.
//!
//! So the two wrappers and the two resource types they raise live here, and
//! `api::rest::error` re-exports the wrappers — a REST handler still imports
//! its error rendering from exactly one module, and the surfaces that share an
//! attribution decision share one definition of it rather than two `gts_id`s
//! that could disagree.
//!
//! # The fall-through arm, and why the exhaustive mapping had to move too
//!
//! Both wrappers end in `other => other.into()`, which resolves to
//! `impl From<DomainError> for CanonicalError`. Moving these two functions out
//! of `api::rest::error` therefore did **not**, on its own, remove the edge the
//! findings were about: the impl was still declared in the transport layer, so
//! every fall-through here was a domain call into it. The impl now lives in
//! [`crate::domain::error`], beside the enum it maps, and that is what closes
//! the direction — along with `RunResourceError`, which came with it because
//! the mapping raises it. (`handlers::runs::subscriber_cap_reached` still
//! imports that type from `api::rest::error`, which re-exports it.)
//!
//! **`crate::no_api_in_domain_tests` cannot see any of this.** It is a text
//! scan over imports; a trait impl is resolved by coherence and leaves no
//! `use` line to find. It pins the imports and nothing more, which is worth
//! stating because an earlier revision of this header cited it as the thing
//! keeping the property — a guard that could not have contradicted the claim
//! it was offered as evidence for.

use toolkit_canonical_errors::{CanonicalError, resource_error};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// `qa_queue_entries`. `pub(crate)` for `domain::error`, whose
/// exhaustive mapping raises the same type for the four queue-row variants
/// that already carry it -- one `gts_id` per resource, not two.
#[resource_error(gts_id!("cf.qa.runs.queue_entry.v1~"))]
pub(crate) struct QueueResourceError;

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
/// layer, because the boundary mapping's `match` is exhaustive: adding
/// [`DomainError::ScheduleNameExists`] to the enum makes the mapping fail to
/// compile until the variant has an arm, and the arm needs a resource type that
/// is not `run`. A caller told a *run* already exists while creating a schedule
/// would go looking for the wrong row. (The declaration moved here with the two
/// wrappers that raise it; the exhaustive mapping it also serves did not, and
/// imports it back.)
#[resource_error(gts_id!("cf.qa.runs.schedule.v1~"))]
pub(crate) struct ScheduleResourceError;

/// Render an error from a **schedule** operation, attributing a field violation
/// to the schedule resource rather than to the run.
///
/// # Why this exists rather than a better `From` impl
///
/// [`DomainError::Validation`] carries a field name and a message and nothing
/// that says which resource the field belongs to, so `domain::error`'s
/// exhaustive `match` cannot tell `NewScheduleReq`'s `name` from
/// `LaunchRunReq`'s `branch` and maps both to
/// [`RunResourceError`](crate::domain::error::RunResourceError). Until Task
/// 20 that was harmless — every
/// `Validation` reaching the boundary really was a run's. It stopped being
/// harmless the moment a schedule payload could raise one, and a caller told
/// `cf.qa.runs.run.v1~` rejected their schedule's `name` goes looking for the
/// wrong row.
///
/// **The call site is where the resource is known, and it knows it
/// statically.** That is the same reasoning that makes
/// [`RunResourceError`](crate::domain::error::RunResourceError)
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
/// took whatever resource type `domain::error`'s exhaustive `match` happens
/// to assign — which is `run` for three of them, on endpoints whose scopes are
/// compiled for
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
///   raises one for a caller still sending `environment_id` (ruling G-4); this
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
