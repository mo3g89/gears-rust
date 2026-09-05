//! The queue handlers driven over a **real** `ConcreteAppServices`.
//!
//! `#[path]`-included from `handlers::queue`, the shape
//! `handlers::schedules`'s `handler_tests` established.
//!
//! # The one property this exists for
//!
//! **A queue endpoint attributes its refusals to the queue entry.** That is not
//! checked by anything else and cannot be: `api::rest::error`'s `match` is
//! exhaustive over `DomainError` *variants*, so the compiler guarantees every
//! variant has a status - and says nothing at all about which of the gear's
//! three resource types an *endpoint* should be naming. The same defect was
//! found in six places across three reviews for exactly that reason. This
//! module and `handlers::runs`'s `handler_tests` are the check, one assertion
//! per endpoint,
//! reading the `gts_id` out of the rendered body.
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use toolkit::api::canonical_prelude::IntoResponse;
use toolkit::api::odata::{OData, ODataQuery};
use uuid::Uuid;

use super::{cancel_queued, force_start, list_queue};
use crate::api::rest::dto::QueueQuery;
use crate::domain::service::test_support::{Fleet, ctx};

const TENANT: Uuid = Uuid::from_u128(0x0A11_0000_0000_0001);

/// The resource type every queue endpoint must attribute a refusal to.
const QUEUE_GTS: &str = "cf.qa.runs.queue_entry.v1~";
/// The one it was attributing them to.
const RUN_GTS: &str = "cf.qa.runs.run.v1~";

async fn rendered(response: axum::response::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// **All three queue endpoints attribute a denial to the queue entry.**
///
/// A 403 is the most likely error on a fresh deployment of these endpoints - a
/// policy engine that has not been taught `qa.queue_entry` refuses all three -
/// and it carries no field, so the resource type is the only actionable thing
/// in the body. All three were naming run permissions.
///
/// A loop rather than one case, and **break-verified per endpoint**: dropping
/// the `as_queue_error` wrapper from any one of the three handlers reintroduces
/// the defect on that endpoint alone and turns exactly this test red, while the
/// rest of the suite stays green.
#[tokio::test]
async fn every_queue_handler_attributes_a_denial_to_the_queue_entry() {
    let fleet = Fleet::denying().await;
    let services = fleet.instance();
    let id = Uuid::from_u128(0x51);

    let mut answers: Vec<(&str, axum::response::Response)> = Vec::new();

    answers.push((
        "list",
        list_queue(
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Query(QueueQuery {
                environment_id: None,
                limit: None,
                legacy_platform_id: None,
            }),
            OData(ODataQuery::new()),
        )
        .await
        .into_response(),
    ));
    answers.push((
        "cancel",
        cancel_queued(
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Path(id),
        )
        .await
        .into_response(),
    ));
    answers.push((
        "force-start",
        force_start(Extension(ctx(TENANT)), Extension(services), Path(id))
            .await
            .into_response(),
    ));

    for (handler, response) in answers {
        let (status, body) = rendered(response).await;
        assert_eq!(status, 403, "{handler}: {body}");
        assert!(
            body.contains(QUEUE_GTS),
            "{handler}: a queue endpoint's denial must name the queue entry: {body}"
        );
        assert!(
            !body.contains(RUN_GTS),
            "{handler}: and must not point at run permissions: {body}"
        );
    }
}

/// **A delete answers about the row the caller named**, not about the run
/// behind it.
///
/// `RunsService::cancel_queued` reads the row's run under the caller's own
/// `qa.run`/`cancel` scope, so a caller who may drop the row but may not cancel
/// its run raised `RunNotFound { id: run_id }` - rendered as *"Run {uuid} was
/// not found"*, run-typed, quoting a uuid the request never sent.
///
/// Driven at the wrapper rather than through the handler because reaching the
/// arm needs a policy that grants `qa.queue_entry` and refuses `qa.run` for the
/// same subject, which the fleet's decision point does not express. The
/// wrapper's *application* at each call site is what
/// [`every_queue_handler_attributes_a_denial_to_the_queue_entry`] covers, so
/// the two together pin the endpoint's behaviour.
#[test]
fn a_delete_never_quotes_the_run_behind_the_row() {
    use crate::api::rest::error::as_queue_error;
    use crate::domain::error::DomainError;
    use toolkit::api::canonical_prelude::Problem;

    let queue_id = Uuid::from_u128(0x0E01);
    let run_id = Uuid::from_u128(0x0BAD_0BAD_0BAD_0BAD);

    let error = as_queue_error(Some(queue_id), DomainError::RunNotFound { id: run_id });
    let problem = Problem::from_error(&error).expect("a problem must serialize");
    assert_eq!(problem.status, 404);
    let body = serde_json::to_string(&problem).expect("a problem must serialize");

    assert!(body.contains(QUEUE_GTS), "{body}");
    assert!(!body.contains(RUN_GTS), "{body}");
    assert!(
        body.contains(&queue_id.to_string()),
        "the body must name the row the caller addressed: {body}"
    );
    assert!(
        !body.contains(&run_id.to_string()),
        "and must not quote an id the request never sent: {body}"
    );
}

/// **Force start's 429 names the queue entry**, and keeps the knob.
///
/// `DomainError::ConcurrencyLimit` is raisable from the launch path *and* from
/// `force_start`, so the variant cannot carry one resource type: the default
/// mapping is run-typed, which is right for `POST /qa/v1/runs`, and this
/// endpoint re-attributes. The `max_concurrent_runs` subject is the only
/// actionable content either rendering has and must survive both.
#[test]
fn force_starts_capacity_refusal_names_the_queue_entry_and_keeps_its_knob() {
    use crate::api::rest::error::as_queue_error;
    use crate::domain::error::DomainError;
    use toolkit::api::canonical_prelude::{CanonicalError, Problem};

    let queue_id = Uuid::from_u128(0x0E01);
    let error = as_queue_error(Some(queue_id), DomainError::ConcurrencyLimit { limit: 50 });
    let problem = Problem::from_error(&error).expect("a problem must serialize");
    assert_eq!(problem.status, 429);
    let body = serde_json::to_string(&problem).expect("a problem must serialize");

    assert!(body.contains(QUEUE_GTS), "{body}");
    assert!(!body.contains(RUN_GTS), "{body}");
    assert!(body.contains("max_concurrent_runs"), "{body}");
    assert!(body.contains("50"), "{body}");

    // And the launch path's rendering of the same variant is still the run's,
    // which is what makes this a re-attribution rather than a correction.
    let launch: CanonicalError = DomainError::ConcurrencyLimit { limit: 50 }.into();
    let launch_body =
        serde_json::to_string(&Problem::from_error(&launch).expect("a problem must serialize"))
            .expect("a problem must serialize");
    assert!(launch_body.contains(RUN_GTS), "{launch_body}");
}
