//! `OperationBuilder` registrations under `/qa/v1/...`.
//!
//! Mirrors `qa-environments/src/api/rest/routes/mod.rs`: one module per
//! resource, each registering its operations against the `OpenAPI` registry,
//! and the per-request state layered on at the end so the registry sees the
//! route definitions before anything is bound to them.
//!
//! Routes orchestrate but hold no business logic: they delegate to
//! `handlers::*`, which call `domain::service::AppServices`.
//!
//! # There is no ingest endpoint, and its absence is load-bearing
//!
//! `IngestService::apply` is the seam a runner-facing progress endpoint would
//! land on, and this route table deliberately contains none.
//!
//! **Corrected 2026-08-15 by Task 16b step 1 — the reason has changed, the
//! absence has not.** This paragraph justified the absence by an *open* phantom
//! read between the per-test tally and the terminal write, "which no test in
//! this crate can falsify because the whole tier is in-memory `SQLite`". Both
//! halves are now false: `service::ingest` opens both of its transactions
//! `SERIALIZABLE` with a bounded retry, and
//! `domain::service::ingest_races_pg_tests` falsifies all three of the races
//! against real Postgres.
//!
//! **No endpoint is added here anyway, and that is a scope statement rather than
//! a safety one.** Wiring one was never in scope, and **nothing in this plan
//! specifies how a runner would authenticate to such a route** — the ingest path
//! reached from `service::watch` runs under `system_actor::for_result_ingest`,
//! not under a caller's own subject, so an HTTP entry point would need a
//! decision that has not been taken.
//!
//! ## Corrected 2026-08-15 by Task 16c — the reachability claim, and the reason
//!
//! This paragraph continued: *"Not reachable today because `ingest` drains one
//! stream sequentially and one stream cannot race itself - which stops being
//! true the moment a second producer exists, and an HTTP endpoint is a second
//! producer. The risk here is somebody adding the route, not somebody
//! forgetting it."* Both sentences are now wrong, and this was the **fourth**
//! copy of the first one — the other three were retracted in `service::ingest`
//! in the same task, and this one was missed because the retraction was scoped
//! to that module rather than to the claim.
//!
//! Task 16c is the second producer. `service::watch` drains
//! `RunExecutor::watch` into `IngestService::ingest`, and its registry keeps one
//! observer per run **per process** — so reachability became a *deployment*
//! property: unreachable on one replica, live on more than one, because the
//! shipped `NoopLeaderElector` makes every replica the dispatcher. See
//! `service::watch`'s header for the three consequences, which are more than the
//! verdict flip.
//!
//! **So "the risk is somebody adding the route" is no longer the whole risk:
//! nobody has to add anything; a second replica suffices.** The gate itself is
//! unchanged and still binds — an HTTP endpoint would add a producer that is not
//! even leader-gated, on top of one that is — which is why the route table
//! still has no ingest operation.

use std::sync::Arc;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};

use crate::api::rest::dto::BoundaryLimits;
use crate::infra::ConcreteAppServices;
use crate::infra::logs::RunLogBroadcaster;

mod queue;
mod runs;
mod schedules;

pub(super) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

pub(super) const API_TAG: &str = "QA Runs";

/// Register every operation, then layer the per-request state.
///
/// Three extensions, not one. `ConcreteAppServices` is the services;
/// [`BoundaryLimits`] is the configured ceiling the launch validation quotes in
/// its 400; and the **concrete** [`RunLogBroadcaster`] is what the SSE handler
/// subscribes through, because a subscription hands back an infrastructure type
/// that `domain::service::LogFanout` deliberately does not mention.
#[allow(clippy::needless_pass_by_value)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteAppServices>,
    logs: Arc<RunLogBroadcaster>,
    limits: BoundaryLimits,
) -> Router {
    router = register_operations(router, openapi);

    router
        .layer(axum::Extension(service))
        .layer(axum::Extension(logs))
        .layer(axum::Extension(limits))
}

/// The route definitions alone, with nothing bound to them.
///
/// Split out from [`register_routes`] so the registration can be driven by a
/// test that has no database, no policy decision point and no services: an
/// `OpenAPI` schema-name collision is a **startup panic**, and a startup panic
/// that only happens under `cargo run` is one `cargo test` cannot see. See
/// `routes::tests`.
pub(super) fn register_operations(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = runs::register_run_routes(router, openapi);
    router = queue::register_queue_routes(router, openapi);
    schedules::register_schedule_routes(router, openapi)
}

#[cfg(test)]
mod tests {
    use super::register_operations;
    use axum::Router;
    use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};

    /// **Registration is exercised, because registration is what panics.**
    ///
    /// `ensure_schema` aborts the process on a `Vec<_>` - utoipa names every one
    /// of them `Vec`, so registering one as a component collides with every
    /// other list response - and on any name already registered with a
    /// different definition. Neither is reachable from a handler unit test, and
    /// `cargo build` cannot see either, because component names are runtime
    /// strings. A route that panics at startup otherwise passes the whole suite.
    ///
    /// This drives the real registration against a real `OpenApiRegistryImpl`,
    /// which is the same call the gear's `RestApiCapability` will make, and then
    /// builds the document - the second half matters because some failures only
    /// surface when the paths and components are assembled.
    #[test]
    fn every_operation_registers_and_the_document_builds() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
    }

    /// Every path from the route table, named individually so a route lost to a
    /// refactor fails here rather than in a manual smoke test.
    ///
    /// Asserted by presence rather than by count: a count is a claim that goes
    /// stale the moment a route lands, and this project has been bitten by
    /// several of those - including two lines above this one, which used to
    /// say how many.
    ///
    /// **What it does not prove:** that the document is *right*. Response
    /// descriptions, the accuracy of each schema against the handler that fills
    /// it, and whether the declared content type matches what the handler
    /// actually writes are all unchecked here. The SSE route is the one where
    /// that gap has teeth - a gear in this workspace ships a published spec that
    /// misdescribes its own content type - so it is asserted below by content
    /// type as well as by path.
    #[test]
    fn the_openapi_document_lists_every_declared_path() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");

        for path in [
            "/qa/v1/runs",
            "/qa/v1/runs/{id}",
            "/qa/v1/runs/{id}/cancel",
            "/qa/v1/runs/{id}/rerun",
            "/qa/v1/runs/{id}/logs",
            "/qa/v1/queue",
            "/qa/v1/queue/{id}",
            "/qa/v1/queue/{id}/force-start",
            "/qa/v1/schedules",
            "/qa/v1/schedules/{id}",
            "/qa/v1/schedules/{id}/notifications",
        ] {
            assert!(
                doc.paths.paths.contains_key(path),
                "{path} is missing from the published document; registered: {:?}",
                doc.paths.paths.keys().collect::<Vec<_>>()
            );
        }
    }

    /// The launch endpoint really does advertise **two** success bodies at two
    /// statuses.
    ///
    /// The `OperationBuilder` typestate permits chaining a second response, but
    /// "permits" is not "did": a transcription that dropped the 202 would leave
    /// a CI caller with no documented shape for the outcome it gets most often.
    /// Two distinct statuses are also the shape that is unambiguously safe -
    /// responses are keyed by status string, so two bodies at one status would
    /// collide on a single map key.
    #[test]
    fn both_launch_shaped_endpoints_document_both_success_statuses() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        let rendered = serde_json::to_value(&doc).expect("the document must serialize");

        // Both paths, because a re-run is a launch: it takes the same two
        // outcomes and a CI caller polls on the 202 the same way. Checking only
        // `/qa/v1/runs` left `/rerun`'s `ACCEPTED` registration deletable with
        // the suite green, while the commit message asserted the contract for
        // both.
        for path in ["/qa/v1/runs", "/qa/v1/runs/{id}/rerun"] {
            let post = &rendered["paths"][path]["post"]["responses"];
            assert!(post.get("200").is_some(), "{path}: 200 missing: {post}");
            assert!(
                post.get("202").is_some(),
                "{path}: 202 queued is the launch contract's other half: {post}"
            );
        }
    }

    /// **204 means no body**, and the shape is easy to get wrong in exactly one
    /// way: `json_response_with_schema::<T>(.., NO_CONTENT, ..)` compiles and
    /// publishes a JSON body on a bodyless response. Step 3 names that
    /// regression specifically; nothing caught it until this test.
    #[test]
    fn the_bodyless_responses_advertise_no_content_type() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        let rendered = serde_json::to_value(&doc).expect("the document must serialize");

        for (path, method) in [
            ("/qa/v1/runs/{id}/cancel", "post"),
            ("/qa/v1/queue/{id}", "delete"),
            ("/qa/v1/schedules/{id}", "delete"),
        ] {
            let response = &rendered["paths"][path][method]["responses"]["204"];
            assert!(!response.is_null(), "{path}: 204 missing");
            assert!(
                response.get("content").is_none(),
                "{path}: a 204 must advertise no body: {response}"
            );
        }
    }

    /// **Every schedule operation is on the method it is documented for.**
    ///
    /// Route registration is a composition point, and this is the shape of
    /// failure it has: five handlers over two paths, all five compiling, all
    /// five unit-testable in isolation, and a transposition between - say - the
    /// `PUT` and the `POST` builder chains producing a router that registers
    /// every operation and behaves nothing like the contract. `cargo build`
    /// cannot see it, because a method is a runtime string, and neither can any
    /// test of a handler, because the handler is fine.
    ///
    /// The operation id is what is asserted rather than mere presence: two
    /// operations swapped between methods would leave both methods populated.
    #[test]
    fn every_schedule_operation_is_registered_on_the_method_it_names() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        let rendered = serde_json::to_value(&doc).expect("the document must serialize");

        for (path, method, operation_id) in [
            ("/qa/v1/schedules", "get", "qa_runs.list_schedules"),
            ("/qa/v1/schedules", "post", "qa_runs.create_schedule"),
            ("/qa/v1/schedules/{id}", "get", "qa_runs.get_schedule"),
            ("/qa/v1/schedules/{id}", "put", "qa_runs.replace_schedule"),
            ("/qa/v1/schedules/{id}", "delete", "qa_runs.delete_schedule"),
            // The notification sub-resource is a **PUT**, where the source
            // system registers a POST (`manager/src/routes/mod.rs:95-98`). The
            // divergence is deliberate and argued at
            // `dto::UpdateScheduleNotificationsReq`; asserting the method here is
            // what stops it drifting back by accident.
            (
                "/qa/v1/schedules/{id}/notifications",
                "put",
                "qa_runs.update_schedule_notifications",
            ),
        ] {
            let operation = &rendered["paths"][path][method];
            assert!(
                !operation.is_null(),
                "{method} {path} is not registered; the path has: {:?}",
                rendered["paths"][path]
            );
            assert_eq!(
                operation["operationId"], operation_id,
                "{method} {path} registered the wrong operation"
            );
        }

        // The negative half: a full replace must not also be reachable as a
        // partial one. `serde_with` is absent, so a PATCH on this resource
        // cannot express the tri-state it would have to - see `NewScheduleReq`.
        assert!(
            rendered["paths"]["/qa/v1/schedules/{id}"]
                .get("patch")
                .is_none(),
            "the schedule edit is a replace, and a PATCH beside it would be a second \
             contract nothing implements"
        );

        // The same, for the notification sub-resource: it is a PUT *instead of*
        // the source system's POST, not as well as it. Registering both would
        // publish two contracts for one edit and make the divergence look
        // accidental.
        for absent in ["post", "patch"] {
            assert!(
                rendered["paths"]["/qa/v1/schedules/{id}/notifications"]
                    .get(absent)
                    .is_none(),
                "the notification edit is a PUT only: {absent} is also registered"
            );
        }
    }

    /// Both write operations really do declare a request body.
    ///
    /// A builder chain that dropped `.json_request::<_>()` registers, serves,
    /// and publishes a `POST` that documents no body at all - so a generated
    /// client would send none. It is exactly the kind of omission that survives
    /// every handler test, because the handler still has its `Json<T>`
    /// extractor.
    #[test]
    fn both_schedule_writes_publish_the_body_they_require() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        let rendered = serde_json::to_value(&doc).expect("the document must serialize");

        for (path, method) in [
            ("/qa/v1/schedules", "post"),
            ("/qa/v1/schedules/{id}", "put"),
            ("/qa/v1/schedules/{id}/notifications", "put"),
        ] {
            let body = &rendered["paths"][path][method]["requestBody"];
            assert!(!body.is_null(), "{method} {path} declares no request body");
            assert!(
                body["content"].get("application/json").is_some(),
                "{method} {path}: {body}"
            );
        }
    }

    /// The list response is published as an **array**, which is what
    /// `json_array_response_with_schema` buys.
    ///
    /// Spelling it `json_response_with_schema::<Vec<ScheduleDto>>` instead does
    /// not merely publish a worse schema: utoipa names every `Vec` `Vec`, so a
    /// second list response would clobber the first, and `ensure_schema` aborts
    /// registration rather than let it.
    ///
    /// **What this test is *not* the only guard against, corrected 2026-08-17.**
    /// It claimed `every_operation_registers_and_the_document_builds` "would
    /// catch the collision once a second array response existed", implying a
    /// precondition. There is none: `openapi_registry`'s assertion on the
    /// component name `Vec` is unconditional, so the wrong spelling fails
    /// immediately and loudly. Measured - swapping this one call reddens every
    /// test in this module, most of which predate the schedules routes
    /// entirely.
    ///
    /// So what this test uniquely contributes is the *shape*: that the published
    /// 200 is an array of `ScheduleDto` rather than merely that registration
    /// survived. Kept for that, not as the tripwire it described itself as.
    #[test]
    fn the_schedule_list_is_published_as_an_array_of_schedules() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        let rendered = serde_json::to_value(&doc).expect("the document must serialize");

        let schema = &rendered["paths"]["/qa/v1/schedules"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"];
        assert_eq!(schema["type"], "array", "{schema}");
        assert!(
            serde_json::to_string(&schema["items"])
                .expect("the item schema must serialize")
                .contains("ScheduleDto"),
            "the array's items must be schedules: {schema}"
        );
    }

    /// The SSE endpoint's declared content type is the one it actually writes.
    ///
    /// `sse_json` is what makes these agree; a route that declared
    /// `json_response_with_schema` and streamed `text/event-stream` anyway would
    /// publish a spec that lies about itself, which is a live defect in another
    /// gear in this workspace.
    #[test]
    fn the_log_stream_is_published_as_an_event_stream() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        let rendered = serde_json::to_value(&doc).expect("the document must serialize");

        let content =
            &rendered["paths"]["/qa/v1/runs/{id}/logs"]["get"]["responses"]["200"]["content"];
        assert!(
            content.get("text/event-stream").is_some(),
            "the log stream must be published as text/event-stream: {content}"
        );
        assert!(
            content.get("application/json").is_none(),
            "it must not also claim to be JSON: {content}"
        );
    }

    /// **`GET /qa/v1/queue` publishes `environment_id`, not `platform_id`, as
    /// its plain query parameter.**
    ///
    /// Important-4 of the Task 25 review, Mutation B: reverting
    /// `.query_param("environment_id", ..)` back to `"platform_id"` at
    /// `routes/queue.rs:28` survived the full suite, because nothing asserted
    /// the *registered* parameter name - only the handler's binding to
    /// [`crate::api::rest::dto::QueueQuery::environment_id`], which the
    /// mutation never touches. Under that mutation the published document
    /// would advertise a parameter the endpoint silently ignores: a Task 26
    /// caller would send `platform_id` exactly as the (wrong) document says,
    /// and get an unfiltered queue back with no error.
    #[test]
    fn the_queue_endpoint_publishes_environment_id_not_platform_id_as_its_parameter() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        let rendered = serde_json::to_value(&doc).expect("the document must serialize");

        let parameters = rendered["paths"]["/qa/v1/queue"]["get"]["parameters"]
            .as_array()
            .expect("the queue read declares its parameters as an array");
        let names: Vec<&str> = parameters
            .iter()
            .filter_map(|p| p["name"].as_str())
            .collect();
        assert!(
            names.contains(&"environment_id"),
            "the plain parameter must be published as environment_id: {names:?}"
        );
        assert!(
            !names.contains(&"platform_id"),
            "the pre-Task-25 name must not reappear: {names:?}"
        );
    }

    /// **The legacy-field traps do not leak into the published schema.**
    ///
    /// NEW-Important-1 of the Task 25 re-review: `pub(crate)` restricts Rust
    /// visibility, and does nothing to stop `utoipa` deriving a schema
    /// property from `LaunchRunReq`/`NewScheduleReq`'s `legacy_platform_id`
    /// trap field, nor to stop it publishing that field's doc as the
    /// property's description - which, before this fix, was our own internal
    /// review commentary, handed to every API consumer. `#[schema(ignore =
    /// true)]` is what actually removes the property; this is the test that
    /// would catch it coming back.
    #[test]
    fn the_legacy_field_traps_do_not_appear_in_the_published_schema() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        let rendered = serde_json::to_value(&doc).expect("the document must serialize");

        for schema_name in ["LaunchRunReq", "NewScheduleReq"] {
            let properties = &rendered["components"]["schemas"][schema_name]["properties"];
            assert!(
                properties.get("legacy_platform_id").is_none(),
                "{schema_name} must not publish its Rust field name: {properties}"
            );
            assert!(
                properties.get("platform_id").is_none(),
                "{schema_name} must not publish a platform_id property at all - it is refused, \
                 not accepted: {properties}"
            );
        }
    }

    /// **The published schema declares the closed value set, not `string`.**
    ///
    /// This is the half of Task 20 the Rust type system cannot check for
    /// itself. Retyping `RunDto::state` from `String` to
    /// `dto::RunStateDto` is what stops *this* gear inventing a state; the
    /// generated TypeScript narrowing to a literal union - so a UI `switch`
    /// missing a case fails `tsc` - depends entirely on the component
    /// schema being an `enum` and being reachable from the response bodies.
    /// A mirror enum that never got registered would leave the document
    /// saying `string` while every Rust test above stayed green.
    ///
    /// The expected spellings are `qa_runs_sdk`'s `as_str` forms, asserted in
    /// the SDK's own declaration order: `plan.yaml` (not `plan`), `canceled`
    /// with one `l` on a run and `cancelled` with two on a queue row.
    #[test]
    fn the_published_schema_declares_closed_enums_for_the_run_vocabularies() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        let rendered = serde_json::to_value(&doc).expect("the document must serialize");

        for (schema_name, expected) in [
            (
                "RunStateDto",
                vec![
                    "created",
                    "queued",
                    "dispatching",
                    "running",
                    "succeeded",
                    "failed",
                    "canceled",
                    "timed_out",
                    "expired",
                    "error",
                ],
            ),
            (
                "ExclusiveTierDto",
                vec!["launch", "plan.yaml", "test_meta", "default"],
            ),
            ("RunSourceDto", vec!["manual", "scheduled"]),
            (
                "QueueStateDto",
                vec![
                    "queued",
                    "dispatching",
                    "running",
                    "done",
                    "failed",
                    "cancelled",
                    "expired",
                ],
            ),
        ] {
            let schema = &rendered["components"]["schemas"][schema_name];
            let published: Vec<&str> = schema["enum"]
                .as_array()
                .unwrap_or_else(|| {
                    panic!("{schema_name} must publish an `enum`, not a bare string: {schema}")
                })
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect();
            assert_eq!(
                published, expected,
                "{schema_name} must publish exactly the SDK's as_str spellings"
            );
        }

        // ... and the response bodies really point at them, rather than at a
        // registered-but-unreferenced component.
        for (owner, field, target) in [
            ("RunDto", "state", "RunStateDto"),
            ("RunDto", "exclusive_tier", "ExclusiveTierDto"),
            ("RunDto", "source", "RunSourceDto"),
            ("QueueEntryDto", "state", "QueueStateDto"),
            ("QueueEntryDto", "source", "RunSourceDto"),
        ] {
            let property = &rendered["components"]["schemas"][owner]["properties"][field];
            assert_eq!(
                property["$ref"],
                serde_json::Value::String(format!("#/components/schemas/{target}")),
                "{owner}.{field} must reference {target}, not inline a string: {property}"
            );
        }
    }
}
