//! REST API route definitions - `OpenAPI` and Axum routing.
//!
//! ## Architecture
//!
//! Routes are organized by resource:
//! - `environments` - target environment endpoints (5 CRUD + 1 read-only lease view)
//! - `variables` - variable endpoints (list, upsert, delete)
//!
//! Lease acquire/release are intentionally **not** registered here — they
//! are SDK-only operations for the qa-runs dispatcher (see
//! `routes::environments` and `handlers::environments::get_environment_lease` for the
//! rationale).
//!
//! ## Layering
//!
//! Routes orchestrate but don't contain business logic: they delegate to
//! `handlers::*`, which in turn call `domain::service::AppServices`.

use std::sync::Arc;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};

use crate::gear::ConcreteAppServices;

mod environments;
mod variables;

pub(super) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Register all routes for the `qa-environments` gear.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteAppServices>,
) -> Router {
    router = environments::register_environment_routes(router, openapi);
    router = variables::register_variable_routes(router, openapi);

    router.layer(axum::Extension(service))
}

#[cfg(test)]
mod tests {
    //! **The two collection endpoints publish a page, not a bare array.**
    //!
    //! This is the branch's only breaking wire change: `GET /qa/v1/environments`
    //! and `GET /qa/v1/variables` used to answer `T[]` and now answer
    //! `Page_T` — `{ items, page_info }` — and both gained `$filter`/`$orderby`.
    //! Every consumer of the generated `openapi.d.ts` reads the shape from the
    //! built document, so the document is what has to be pinned; a hand-edited
    //! `.d.ts` entry that agreed with nothing would look exactly as correct.
    //!
    //! `qa-runs`' `the_published_schema_declares_closed_enums_for_the_run_vocabularies`
    //! and `qa-insights`' `the_published_schema_declares_closed_enums_for_both_scopes`
    //! are the shape this follows: build the real `OpenApiRegistryImpl`
    //! document, then assert the component *and* the `$ref` that reaches it.
    //! Asserting only that `Page_EnvironmentDto` exists would pass while the
    //! response went on being an inline array, which is precisely the
    //! regression this gear had no test for.
    //!
    //! Compared as `serde_json::Value`, never as text: `serde_json`'s
    //! `preserve_order` feature is enabled workspace-wide, so a textual
    //! comparison passes per-package and fails under `make test-no-macros`.

    use axum::Router;
    use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};

    use super::{environments, variables};

    /// The real document, built the way `RestApiCapability` builds it.
    fn published_document() -> serde_json::Value {
        let openapi = OpenApiRegistryImpl::new();
        let mut router = Router::new();
        router = environments::register_environment_routes(router, &openapi);
        let _router = variables::register_variable_routes(router, &openapi);
        let doc = openapi
            .build_openapi(&OpenApiInfo::default())
            .expect("the OpenAPI document must build");
        serde_json::to_value(&doc).expect("the document must serialize")
    }

    /// A `$ref` value, as it appears in the document.
    fn schema_ref(name: &str) -> serde_json::Value {
        serde_json::Value::String(format!("#/components/schemas/{name}"))
    }

    /// **Both collections answer `Page_T`, and `Page_T` really wraps `T`.**
    ///
    /// Three assertions per endpoint, because each one alone can be satisfied
    /// by a broken document:
    ///
    /// 1. the `200` body is `$ref: Page_T` — a regression to `T[]` inlines an
    ///    `array` here and fails;
    /// 2. `Page_T` is registered and its `items` is an array of `$ref: T` —
    ///    `Page<T>`'s hand-written `ToSchema` is what registers `T` beside it
    ///    (the derive omits the generic and leaves a dangling `$ref`), and its
    ///    `page_info` points at `PageInfo`;
    /// 3. `T` itself is present, so neither `$ref` dangles.
    ///
    /// The component name is `Page_` + `T::name()`, formatted by
    /// `libs/toolkit-odata/src/page.rs:59`; it is spelled out here rather than
    /// computed so that a change to that formatting is a failure rather than a
    /// silently renamed assertion.
    #[test]
    fn the_published_schema_declares_a_page_for_both_collections() {
        let rendered = published_document();

        for (path, page, item, description) in [
            (
                "/qa/v1/environments",
                "Page_EnvironmentDto",
                "EnvironmentDto",
                "One page of target environments",
            ),
            (
                "/qa/v1/variables",
                "Page_VariableDto",
                "VariableDto",
                "One page of variables",
            ),
        ] {
            let ok = &rendered["paths"][path]["get"]["responses"]["200"];
            let body = &ok["content"]["application/json"]["schema"];
            assert_eq!(
                body["$ref"],
                schema_ref(page),
                "GET {path} must answer {page}, not an inline array: {body}"
            );
            assert_eq!(
                ok["description"],
                serde_json::Value::String(description.to_owned()),
                "GET {path}'s 200 description names the page: {ok}"
            );

            let page_schema = &rendered["components"]["schemas"][page];
            assert_eq!(
                page_schema["type"],
                serde_json::Value::String("object".to_owned()),
                "{page} must be registered as an object: {page_schema}"
            );
            assert_eq!(
                page_schema["properties"]["items"]["type"],
                serde_json::Value::String("array".to_owned()),
                "{page}.items must be an array: {page_schema}"
            );
            assert_eq!(
                page_schema["properties"]["items"]["items"]["$ref"],
                schema_ref(item),
                "{page}.items must be an array of {item}: {page_schema}"
            );
            assert_eq!(
                page_schema["properties"]["page_info"]["$ref"],
                schema_ref("PageInfo"),
                "{page}.page_info must reference PageInfo: {page_schema}"
            );
            assert_eq!(
                page_schema["required"],
                serde_json::json!(["items", "page_info"]),
                "both halves of {page} are required: {page_schema}"
            );

            for referenced in [item, "PageInfo"] {
                assert!(
                    rendered["components"]["schemas"]
                        .get(referenced)
                        .is_some_and(|schema| !schema.is_null()),
                    "{page} references {referenced}, so it must be registered too"
                );
            }
        }
    }

    /// **Both collections advertise `$filter` and `$orderby` as query
    /// parameters.**
    ///
    /// The other half of the same wire change. `with_odata_filter::<T>()` and
    /// `with_odata_orderby::<T>()` are what write them, and both take the
    /// repository's own `FilterField` enum — so a parameter that vanished from
    /// the document would mean the endpoint had stopped accepting the query it
    /// documents, with nothing else in this crate to notice.
    ///
    /// The advertised field lists are deliberately *not* asserted: they are
    /// generated from `EnvironmentFilterField`/`VariableFilterField`, so
    /// restating them here would only duplicate the enums.
    #[test]
    fn both_collections_declare_their_odata_parameters() {
        let rendered = published_document();

        for path in ["/qa/v1/environments", "/qa/v1/variables"] {
            let parameters = rendered["paths"][path]["get"]["parameters"]
                .as_array()
                .unwrap_or_else(|| panic!("GET {path} must declare query parameters"))
                .clone();

            for wanted in ["$filter", "$orderby"] {
                let parameter = parameters
                    .iter()
                    .find(|parameter| parameter["name"] == serde_json::json!(wanted))
                    .unwrap_or_else(|| {
                        panic!("GET {path} must advertise {wanted}: {parameters:?}")
                    });
                assert_eq!(
                    parameter["in"],
                    serde_json::json!("query"),
                    "{wanted} on GET {path} is a query parameter: {parameter}"
                );
                assert_eq!(
                    parameter["required"],
                    serde_json::json!(false),
                    "{wanted} on GET {path} is optional: {parameter}"
                );
            }
        }
    }
}
