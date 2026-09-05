//! `OperationBuilder` registrations under `/qa/v1/...`.
//!
//! Mirrors `qa-environments/src/api/rest/routes/mod.rs`: one module per surface,
//! each registering its operations against the `OpenAPI` registry, with the
//! per-request state layered on at the end so the registry sees the route
//! definitions before anything is bound to them.
//!
//! Routes orchestrate but hold no business logic: they delegate to
//! `handlers::*`, which call [`crate::domain::service::AppServices`].
//!
//! # `register_operations` is split out, and it is split out for a test
//!
//! `OpenApiRegistry::ensure_schema` **aborts the process** on a schema-name
//! collision, and component names are runtime strings — so `cargo build` cannot
//! see one and a handler unit test cannot reach one. A route that panics at
//! startup otherwise passes the whole suite. Splitting the registration from the
//! `Extension` layering is what lets [`tests`] drive the real registration with
//! no database, no PDP and no services. Idiom copied from
//! `qa-runs/src/api/rest/routes/mod.rs`, which records the same reasoning.

use std::sync::Arc;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};

use crate::gear::ConcreteAppServices;

mod admin;
mod analytics;
mod collect;
mod collections;
mod dashboard;
mod jira;
mod saved_views;
mod settings;

pub(super) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

pub(super) const API_TAG: &str = "QA Insights";

/// Register every operation, then layer the per-request state.
///
/// **Still one extension after Task 17, unlike qa-runs' three**, and the
/// forecast that said otherwise was wrong. This doc used to promise "a
/// configured-limits extension arrives with Task 17's `OData` collections, which
/// are the first thing here with a page-size ceiling to quote". They do have a
/// ceiling, and it needs no extension: the clamp is a `const` applied inside
/// `paginate_odata` (`infra::storage::db::PAGE_LIMITS`), so nothing per-request
/// carries it and the endpoint descriptions quote the literals. qa-runs'
/// `BoundaryLimits` extension is a different thing — a *configured* launch
/// validation ceiling its 400 has to name — and reading it as a page-size
/// mechanism is what produced the forecast.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteAppServices>,
) -> Router {
    register_operations(router, openapi).layer(axum::Extension(service))
}

/// The route definitions alone, with nothing bound to them. See this module's
/// header for why this is a separate function.
pub(super) fn register_operations(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = admin::register_admin_routes(router, openapi);
    router = collections::register_collection_routes(router, openapi);
    router = dashboard::register_dashboard_routes(router, openapi);
    router = analytics::register_analytics_routes(router, openapi);
    router = saved_views::register_saved_view_routes(router, openapi);
    router = settings::register_settings_routes(router, openapi);
    router = jira::register_jira_routes(router, openapi);
    collect::register_collect_routes(router, openapi)
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};

    use super::register_operations;

    /// **Registration is exercised, because registration is what panics.**
    ///
    /// This drives the real registration against a real `OpenApiRegistryImpl` —
    /// the same call [`crate::gear::QaInsights::register_rest`] makes — and then
    /// builds the document, because some failures only surface when the paths
    /// and components are assembled.
    #[test]
    fn every_operation_registers_and_the_document_builds() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
    }

    /// The route table's paths, named individually so a route lost to a refactor
    /// fails here rather than in a manual smoke test.
    ///
    /// Asserted by presence rather than by a hardcoded count: a bare numeric
    /// literal is a claim that goes stale the moment a route lands, and this
    /// subsystem has been bitten by several of those. Task 16 wrote this as a
    /// bare `assert!` over one path because `clippy::single_element_loop` would
    /// not permit a loop over one; Task 17 added the second and third, so it is
    /// qa-runs' loop now.
    ///
    /// **Task 26 also ties the document's path *count* to this same array**,
    /// closing the exact gap that let `/qa/v1/analytics/export` register without
    /// landing here: the presence loop alone only fails when a *listed* path
    /// goes missing, and says nothing about a path that was never listed at all.
    /// The count is derived from `PATHS.len()` rather than a second literal, so
    /// the two assertions cannot drift from each other — only from the real
    /// route table, which is exactly what should fail this test.
    #[test]
    fn the_openapi_document_lists_every_registered_path() {
        const PATHS: [&str; 23] = [
            "/qa/v1/insights/rebuild",
            "/qa/v1/test-results",
            "/qa/v1/test-case-results",
            "/qa/v1/dashboard",
            "/qa/v1/dashboard/coverage",
            "/qa/v1/analytics/overview",
            "/qa/v1/analytics/build-tests",
            "/qa/v1/analytics/export",
            "/qa/v1/analytics/plan/tests",
            "/qa/v1/analytics/plan/builds",
            "/qa/v1/analytics/plan/test-history",
            "/qa/v1/analytics/views",
            "/qa/v1/analytics/views/{id}",
            "/qa/v1/analytics/collect",
            "/qa/v1/collect/{repo_id}",
            "/qa/v1/settings/jira",
            "/qa/v1/settings/jira-poller",
            "/qa/v1/jira/open-bugs",
            "/qa/v1/jira/bugs",
            "/qa/v1/settings/notifications",
            "/qa/v1/settings/notifications/test",
            "/qa/v1/settings/notifications/preview",
            "/qa/v1/settings/notifications/log",
        ];

        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");

        for path in PATHS {
            assert!(
                doc.paths.paths.contains_key(path),
                "{path} is missing from the published document; registered: {:?}",
                doc.paths.paths.keys().collect::<Vec<_>>()
            );
        }

        assert_eq!(
            doc.paths.paths.len(),
            PATHS.len(),
            "the document registers a path this array does not name — a route \
             landed without being added here; registered: {:?}",
            doc.paths.paths.keys().collect::<Vec<_>>()
        );
    }

    /// The two collections advertise a `$filter` and an `$orderby` parameter, and
    /// the field list inside each one is the *repository's* enum.
    ///
    /// This is the one thing `every_operation_registers_and_the_document_builds`
    /// cannot see: `with_odata_filter::<F>()` is a builder call that compiles
    /// whether or not it is made, so dropping it from a route would leave the
    /// collection filterable in SQL and undocumented on the wire — the exact
    /// drift `infra::storage::odata`'s header exists to prevent, in the direction
    /// no type check covers.
    ///
    /// **Asserted against the `$filter` parameter's own description, not against
    /// the rendered path item.** That distinction is load-bearing and the first
    /// version of this test got it wrong: the route *descriptions* name their
    /// filterable fields in prose, so a whole-path-item substring search passes on
    /// the prose alone and says nothing about the enum. `with_odata_filter` writes
    /// one `- <name>: <ops>` line per `FIELDS` entry into that parameter's
    /// description (`libs/toolkit/src/api/operation_builder.rs:342-366`), so the
    /// parameter is where the enum is actually observable.
    ///
    /// Each collection is checked for a field that is **its own** and for one that
    /// belongs to the other, so a route wired to the wrong enum fails either way
    /// round.
    #[test]
    fn each_collection_advertises_its_own_filterable_fields() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");

        for (path, own_field, foreign_field) in [
            ("/qa/v1/test-results", "run_finished_at", "status"),
            ("/qa/v1/test-case-results", "status", "run_finished_at"),
        ] {
            let item = doc
                .paths
                .paths
                .get(path)
                .unwrap_or_else(|| panic!("{path} must be registered"));
            let item = serde_json::to_value(item).expect("a path item serialises");

            // `$orderby` must exist; its *contents* are the next test's
            // subject, because they are not the accepted set.
            assert!(
                parameter_description(&item, "$orderby").is_some(),
                "{path} does not advertise $orderby",
            );

            let description = parameter_description(&item, "$filter")
                .unwrap_or_else(|| panic!("{path} does not advertise $filter"));
            assert!(
                description.contains(own_field),
                "{path}'s $filter omits {own_field}, so the route is wired to the other \
                 collection's field enum: {description}",
            );
            assert!(
                !description.contains(foreign_field),
                "{path}'s $filter names {foreign_field}, which belongs to the other \
                 collection: {description}",
            );
        }
    }

    /// **Each endpoint description names every field its own enum admits.**
    ///
    /// The descriptions repeat the field lists in prose, because a caller reading
    /// the summary should not have to open the `$filter` parameter to find out what
    /// is filterable. Prose duplicating a type is prose that goes stale: adding a
    /// variant to `TestResultsField` would leave a filterable field documented
    /// nowhere a human reads, and nothing else in this crate would notice.
    ///
    /// Checked in one direction only — every enum field appears in the prose — and
    /// that is deliberate: the reverse ("the prose names nothing else") cannot be
    /// asserted against free text without false positives, since the descriptions
    /// legitimately contain words like `created_at` in order to say it is *not*
    /// available.
    #[test]
    fn each_description_names_every_field_its_enum_admits() {
        use toolkit_odata::filter::FilterField;

        use crate::infra::storage::odata::{TestCaseResultsField, TestResultsField};

        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");

        let names: Vec<(&str, Vec<&str>)> = vec![
            (
                "/qa/v1/test-results",
                TestResultsField::FIELDS
                    .iter()
                    .map(FilterField::name)
                    .collect(),
            ),
            (
                "/qa/v1/test-case-results",
                TestCaseResultsField::FIELDS
                    .iter()
                    .map(FilterField::name)
                    .collect(),
            ),
        ];

        for (path, fields) in names {
            let item = doc
                .paths
                .paths
                .get(path)
                .unwrap_or_else(|| panic!("{path} must be registered"));
            let item = serde_json::to_value(item).expect("a path item serialises");
            let description =
                operation_description(&item).expect("the operation has a description");

            for field in fields {
                assert!(
                    description.contains(field),
                    "{path}'s description does not mention the filterable field {field}",
                );
            }
        }
    }

    /// **The published `$orderby` list is wider than the accepted one, and the
    /// module header's "one type, two consumers" claim does not cover it.**
    ///
    /// Measured 2026-08-20. `with_odata_orderby::<T>()` writes an `asc`/`desc`
    /// line for **every** `T::FIELDS` entry and never consults
    /// `FieldToColumn::is_orderable`
    /// (`libs/toolkit/src/api/operation_builder.rs:389-424`), while
    /// `paginate_odata` rejects a non-orderable key as `InvalidOrderByField`
    /// (`libs/toolkit-db/src/odata/sea_orm_filter.rs:699-705`). So
    /// `/qa/v1/test-results` publishes `run_finished_at asc|desc` and answers 400
    /// to it — the one field
    /// `infra::storage::odata::TestResultsODataMapper::is_orderable` excludes.
    ///
    /// Pinned rather than worked around. `infra::storage::odata`'s header
    /// enumerates the three ways out and why none is taken — including the one
    /// that needs no change outside this crate (a second, orderable-only
    /// `FilterField` enum for `with_odata_orderby`), whose cost is two enums over
    /// one table kept in agreement by hand. The compensating control is the
    /// endpoint's own description, which names the sortable fields and says this
    /// one is filterable only; this test is what keeps that paragraph honest.
    ///
    /// **It turns red when the underlying behaviour changes, and that is not a
    /// `toolkit` patch away.** `with_odata_orderby`'s bound is `T: FilterField`,
    /// which is `toolkit-odata`'s trait, while `is_orderable` sits on
    /// `FieldToColumn` in **toolkit-db** (`sea_orm_filter.rs:103`). `toolkit` does
    /// depend on `toolkit-db`, but only optionally and behind its `db` feature
    /// (`libs/toolkit/Cargo.toml:57`, `:97`), and `api::operation_builder` is not
    /// feature-gated (`libs/toolkit/src/api/mod.rs:12`) — so consulting
    /// `is_orderable` there means either entangling the unconditional route
    /// builder with a feature-gated crate, or lifting `is_orderable` up to
    /// `FilterField`, plus a second type parameter at every call site in the
    /// workspace. A cross-crate API change, not an imminent fix; do not read this
    /// test as waiting on one.
    #[test]
    fn the_orderby_parameter_over_advertises_the_nullable_instant() {
        let openapi = OpenApiRegistryImpl::new();
        let _router = register_operations(Router::new(), &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");

        let item = doc
            .paths
            .paths
            .get("/qa/v1/test-results")
            .expect("the collection is registered");
        let item = serde_json::to_value(item).expect("a path item serialises");

        let orderby = parameter_description(&item, "$orderby").expect("$orderby is advertised");
        assert!(
            orderby.contains("run_finished_at"),
            "if the published $orderby list no longer names run_finished_at, toolkit has \
             learned about is_orderable and the endpoint description's caveat can go: \
             {orderby}",
        );

        let description = operation_description(&item).expect("the operation has a description");
        assert!(
            description.contains("filtered but not sorted"),
            "the endpoint description is the only warning a caller gets about the \
             over-advertised sort key; it must say so: {description}",
        );
    }

    /// The `description` of the *operation* itself, i.e. the `OperationBuilder`
    /// prose, as distinct from a parameter's.
    ///
    /// Found by taking the description that sits beside an `operationId`, which is
    /// what distinguishes the operation object from the parameter objects nested
    /// inside it.
    fn operation_description(node: &serde_json::Value) -> Option<String> {
        match node {
            serde_json::Value::Object(map) => {
                if map.contains_key("operationId") {
                    return map
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                }
                map.values().find_map(operation_description)
            }
            serde_json::Value::Array(items) => items.iter().find_map(operation_description),
            _ => None,
        }
    }

    /// The `description` of the query parameter called `name`, wherever it sits
    /// in a serialized `OpenAPI` path item.
    ///
    /// Walks the JSON rather than naming `get`/`post` and `parameters`: the
    /// document is `utoipa`'s shape, not this gear's, and a search that does not
    /// depend on it cannot break when that shape moves. A missing parameter comes
    /// back as `None` so the caller can say which route lacked it.
    fn parameter_description(node: &serde_json::Value, name: &str) -> Option<String> {
        match node {
            serde_json::Value::Object(map) => {
                if map.get("name").and_then(serde_json::Value::as_str) == Some(name) {
                    return Some(
                        map.get("description")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    );
                }
                map.values().find_map(|v| parameter_description(v, name))
            }
            serde_json::Value::Array(items) => {
                items.iter().find_map(|v| parameter_description(v, name))
            }
            _ => None,
        }
    }
}
