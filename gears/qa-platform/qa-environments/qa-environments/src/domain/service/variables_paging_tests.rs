//! What `GET /qa/v1/variables` — and therefore what a *run* — actually receives.
//!
//! Against a REAL in-memory `SQLite` database with the gear's own migrations and
//! `SeaORM` repositories, like [`super::tests_tenant_scoping`], because both
//! properties here are about what the pager does to the rows and a mock
//! repository would answer whatever it was told.
//!
//! # Why this file exists at all
//!
//! Both tests below are for defects Task 24's review found in Task 24's own
//! first cut, and both were invisible to everything else in the suite:
//!
//! 1. **The reachable set fell from 500 rows to 200**, and because the union
//!    fills pipeline-first the rows lost were the *environment-specific* ones —
//!    the tier that overrides pipeline in `qa-runs`'
//!    `domain::runvars::split_by_scope`. The consumer is
//!    `qa-runs`' `dispatch_spec`, which assembles a run's environment from this
//!    call, so the symptom was a run launched with a pipeline default where an
//!    environment override existed. Nothing in either gear's suite exercised a
//!    tenant with more than a page of variables, so nothing went red.
//! 2. **A caller-supplied `cursor` was re-applied to the second table**, keyed
//!    on `name` against `qa_pipeline_variables` and then used to filter
//!    `qa_environment_variables`, dropping every environment variable whose name
//!    sorted at or before it — with `next_cursor: null`, so the caller could not
//!    tell.
//!
//! Both are row-losing and silent, which is the class of defect a test has to
//! exist for; neither is expressible at the repository level, because both are
//! properties of how [`VariablesService::list_for_env`] *composes* two paged
//! reads.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use qa_environments_sdk::{NewEnvironment, NewVariable};
use toolkit_odata::{CursorV1, ODataQuery, SortDir};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::test_support::{build_services_tenant_scoped, ctx, inmem_db};

/// `PAGE_LIMITS.default`. Named as a literal rather than imported so this file
/// states the number the regression was *about* — a test that read the constant
/// would follow it if someone changed it, which is the whole shape review
/// finding #43 objects to and the reason `db.rs` pins the pair separately.
const PAGE_DEFAULT: usize = 200;

fn new_environment(name: &str) -> NewEnvironment {
    NewEnvironment {
        kubeconfig_credstore_ref: Some("credstore://test".to_owned()),
        credentials: std::collections::BTreeMap::new(),
        name: name.to_owned(),
        product_id: Uuid::from_u128(0x9001),
        description: None,
        kubeconfig: None,
        default_branch: None,
        is_default: false,
    }
}

/// **The union reaches the deployment's cap, not one page — and the rows past
/// the page boundary are the environment's own.**
///
/// `max_variables` is 500 (`config::QaEnvironmentsConfig::default`, and the
/// shipped YAML). The pre-paging code returned every pipeline variable plus
/// every one of the environment's and then `truncate(500)`. This asserts the
/// same reachable set through the paged path, with a tenant deliberately over
/// `PAGE_LIMITS.default`.
///
/// The `assert` that matters is the **second** one. A version of this method
/// bounded at the page returns 200 rows and passes a bare length check against
/// `>= 200`; what it cannot do is return an environment-scoped row, because
/// pipeline variables fill the page first. So the environment tier is counted,
/// not just the total.
#[tokio::test]
async fn the_union_reaches_the_configured_cap_rather_than_one_page() {
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    let environment = services
        .environments
        .create_environment(&ctx(tenant), new_environment("env-a"))
        .await
        .unwrap();

    // 220 pipeline variables: more than one page on their own, so the
    // environment tier begins strictly past the `PAGE_LIMITS.default` boundary.
    let pipeline_count = PAGE_DEFAULT + 20;
    for index in 0..pipeline_count {
        services
            .variables
            .upsert(
                &ctx(tenant),
                NewVariable {
                    environment_id: None,
                    name: format!("GLOBAL_{index:04}"),
                    value: "g".to_owned(),
                },
            )
            .await
            .unwrap();
    }

    let environment_count = 30;
    for index in 0..environment_count {
        services
            .variables
            .upsert(
                &ctx(tenant),
                NewVariable {
                    environment_id: Some(environment.id),
                    name: format!("ENV_{index:04}"),
                    value: "e".to_owned(),
                },
            )
            .await
            .unwrap();
    }

    let page = services
        .variables
        .list_for_env(&ctx(tenant), Some(environment.id), &ODataQuery::default())
        .await
        .unwrap();

    assert_eq!(
        page.items.len(),
        pipeline_count + environment_count,
        "the union must reach max_variables (500), not PAGE_LIMITS.default (200)"
    );

    // The half that a page-bounded union silently drops.
    let environment_rows = page
        .items
        .iter()
        .filter(|variable| variable.environment_id == Some(environment.id))
        .count();
    assert_eq!(
        environment_rows, environment_count,
        "environment-scoped variables are the OVERRIDING tier (qa-runs' \
         runvars::split_by_scope) and are what a pipeline-first page boundary \
         drops first -- every one of them must be present"
    );
}

/// **A `cursor` sent with `environment_id` is refused, not half-applied.**
///
/// The combination is reachable — the `OData` extractor accepts a bare `cursor`
/// and refuses only `cursor` + `$orderby`, the tiebreaker is `name` on both
/// tables so the cursor's sort-token check passes, and the filter hash matches —
/// so before this guard it produced a 200 response that had silently dropped
/// every environment variable whose name sorted at or before the token, with
/// `next_cursor: null` so the caller could not detect it.
///
/// The cursor here is minted the way a real caller gets one: from this same
/// endpoint *without* `environment_id`.
#[tokio::test]
async fn a_cursor_cannot_be_combined_with_environment_id() {
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    let environment = services
        .environments
        .create_environment(&ctx(tenant), new_environment("env-a"))
        .await
        .unwrap();

    // `ZZ_...` sorts after `AA_...`, so a `name`-keyed cursor taken past the
    // pipeline rows would have filtered the environment row out entirely.
    services
        .variables
        .upsert(
            &ctx(tenant),
            NewVariable {
                environment_id: None,
                name: "ZZ_GLOBAL".to_owned(),
                value: "g".to_owned(),
            },
        )
        .await
        .unwrap();
    services
        .variables
        .upsert(
            &ctx(tenant),
            NewVariable {
                environment_id: Some(environment.id),
                name: "AA_ENV".to_owned(),
                value: "e".to_owned(),
            },
        )
        .await
        .unwrap();

    // A cursor the endpoint itself would hand out: one pipeline row per page,
    // so the first page's `next_cursor` is keyed past `ZZ_GLOBAL`.
    let first = services
        .variables
        .list_for_env(&ctx(tenant), None, &ODataQuery::new().with_limit(1))
        .await
        .unwrap();
    let token = first
        .page_info
        .next_cursor
        .clone()
        .unwrap_or_else(|| synthetic_name_cursor("ZZ_GLOBAL"));
    let cursor = CursorV1::decode(&token).unwrap();

    let err = services
        .variables
        .list_for_env(
            &ctx(tenant),
            Some(environment.id),
            &ODataQuery::new().with_cursor(cursor),
        )
        .await
        .unwrap_err();

    match err {
        DomainError::Validation { field, .. } => assert_eq!(
            field, "cursor",
            "the 400 must name the parameter the caller can edit"
        ),
        other => panic!("expected a Validation error on `cursor`, got {other:?}"),
    }
}

/// A `name`-keyed forward cursor, for the case where the tenant's pipeline rows
/// fit in one page and the endpoint therefore hands back no `next_cursor`.
///
/// Built through [`CursorV1::encode`] rather than hand-written base64, so it is
/// a token this build genuinely accepts — the point of the test is that a
/// *valid* cursor is refused for this combination, not that a malformed one is.
fn synthetic_name_cursor(name: &str) -> String {
    CursorV1 {
        k: vec![name.to_owned()],
        o: SortDir::Asc,
        s: "+name".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    }
    .encode()
    .unwrap()
}
