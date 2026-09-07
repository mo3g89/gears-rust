//! Tests for [`crate::infra::storage::odata`] and the paged reads it feeds.
//!
//! A sibling `_tests.rs` file rather than an inline `mod tests`, following the
//! convention the migration tests in this same directory already use — and,
//! secondarily, because the fixture below builds an [`AccessScope`] the way a
//! test may and production may not (`domain::service::unscoped_read_guard_tests`
//! exempts `_tests.rs` files by name, and only those).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sea_orm::ActiveValue;
use time::OffsetDateTime;
use toolkit_db::secure::secure_insert;
use toolkit_odata::filter::FilterField;
use toolkit_odata::{ODataQuery, ast};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{EnvironmentsRepository, VariablesRepository};
use crate::domain::service::DbProvider;
use crate::infra::storage::db::PAGE_LIMITS;
use crate::infra::storage::entity::environment;
use crate::infra::storage::entity::environment::Entity as EnvironmentEntity;
use crate::infra::storage::odata::{EnvironmentFilterField, VariableFilterField};
use crate::infra::storage::{OrmEnvironmentsRepository, OrmVariablesRepository};
use crate::test_support::inmem_db;

/// `PAGE_LIMITS.default` as a row count. `u64 -> usize` is lossless on every
/// target this ships to and the value is 200; `try_from` in a test would add a
/// `.unwrap()` per call site and say nothing.
#[allow(clippy::cast_possible_truncation)]
const DEFAULT_PAGE: usize = PAGE_LIMITS.default as usize;

/// One tenant, one connection, one repository, and `count` environments in it.
struct EnvFixture {
    db: DbProvider,
    scope: AccessScope,
    repo: OrmEnvironmentsRepository,
    tenant_id: Uuid,
}

impl EnvFixture {
    /// A fresh runner. Taken per call rather than held, because `DbConn`
    /// borrows the provider.
    fn conn(&self) -> toolkit_db::secure::DbConn<'_> {
        self.db.conn().unwrap()
    }
}

/// `count` environments in one tenant, named `env-0000`..`env-NNNN` so the
/// `(tenant_id, name)` unique index — the index the page's sort key rides —
/// orders them predictably.
async fn env_fixture_with(count: usize) -> EnvFixture {
    let db = DbProvider::new(inmem_db().await);
    let conn = db.conn().unwrap();
    let tenant_id = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant_id);
    let repo = OrmEnvironmentsRepository;

    let now = OffsetDateTime::now_utc();
    for index in 0..count {
        let model = environment::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            name: ActiveValue::Set(format!("env-{index:04}")),
            product_id: ActiveValue::Set(Uuid::from_u128(0x9001)),
            description: ActiveValue::Set(None),
            available: ActiveValue::Set(true),
            observed_version: ActiveValue::Set(None),
            observed_build: ActiveValue::Set(None),
            default_branch: ActiveValue::Set(None),
            is_default: ActiveValue::Set(false),
            version_detect_error: ActiveValue::Set(None),
            version_detected_at: ActiveValue::Set(None),
            credentials: ActiveValue::Set(serde_json::json!([])),
            observed_attrs: ActiveValue::Set(serde_json::json!({})),
            config: ActiveValue::Set(serde_json::json!({})),
            observed_base_url: ActiveValue::Set(None),
            health_state: ActiveValue::Set("unknown".to_owned()),
            health_detail: ActiveValue::Set(None),
            health_checked_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };
        secure_insert::<EnvironmentEntity>(model, &scope, &conn)
            .await
            .unwrap();
    }

    EnvFixture {
        db,
        scope,
        repo,
        tenant_id,
    }
}

/// A `$filter` naming `field`, as an already-parsed `OData` query.
///
/// Hand-built rather than parsed from a query string: the `$filter` *parser*
/// lives behind a `toolkit-odata` feature the gear's tests do not enable, and
/// what is under test here is the **field allow-list**, which is applied after
/// parsing either way.
fn filter_on(field: &str) -> ODataQuery {
    equals(field, "x")
}

/// `<field> eq '<value>'`, as an already-parsed `OData` query.
fn equals(field: &str, value: &str) -> ODataQuery {
    ODataQuery::new().with_filter(ast::Expr::Compare(
        Box::new(ast::Expr::Identifier(field.to_owned())),
        ast::CompareOperator::Eq,
        Box::new(ast::Expr::Value(ast::Value::String(value.to_owned()))),
    ))
}

/// **An unbounded list is bounded.**
///
/// `/qa/v1/environments` was `find().all()` with no limit. cpt-cf-qa-nfr-scale's
/// first number is 100 platforms, and this is the collection that number is
/// about. Review finding #55.
#[tokio::test]
async fn listing_environments_is_bounded_by_the_page_limit() {
    let f = env_fixture_with(DEFAULT_PAGE + 50).await;
    let page = f
        .repo
        .list_page(&f.conn(), &f.scope, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), DEFAULT_PAGE);
    assert!(
        page.page_info.next_cursor.is_some(),
        "a truncated page must be resumable"
    );
}

/// **An unknown `$filter` field is a 400, not a scan.**
///
/// The closed-enum discipline the other two gears' `odata.rs` already have --
/// the review confirms it as verified-clean there, and it is the property that
/// stops a filter from becoming a whole-tenant table scan.
#[tokio::test]
async fn an_unknown_filter_field_is_rejected() {
    let f = env_fixture_with(3).await;
    let err = f
        .repo
        .list_page(&f.conn(), &f.scope, &filter_on("nonexistent"))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

/// The page limits are the literals the other two gears freeze.
///
/// **This is a constant-against-literal assertion on purpose, and it is not an
/// instance of review finding #43** (which calls that shape a defect worth
/// deleting). The reason it is different here is recorded at
/// `qa-runs/src/infra/storage/db.rs:27-33`: mutating that pair to
/// `{ default: 15, max: 9999 }` once left qa-runs' entire suite green, so the
/// literals are the only thing pinning them. The same is true here — every
/// other test in this file asserts *against* `PAGE_LIMITS` rather than against
/// a number, so every one of them would follow a mutated constant. Do not
/// delete this as a tautology; it is the only assertion in the gear that a
/// change to the pair has to argue with.
#[test]
fn the_page_limits_match_the_other_two_gears() {
    assert_eq!(PAGE_LIMITS.default, 200);
    assert_eq!(PAGE_LIMITS.max, 500);
}

/// The environment page's rows are the caller's tenant's and nobody else's,
/// **with a `$filter` applied on top rather than in place of the scope**.
///
/// The type system already makes the scope unskippable — `paginate_odata`'s
/// first parameter is `SecureSelect<E, Scoped>`, so an unscoped select does not
/// compile — but "unskippable" and "actually composed with the filter" are two
/// claims, and only the second one is about behaviour.
#[tokio::test]
async fn a_filter_cannot_widen_the_page_past_the_scope() {
    let f = env_fixture_with(2).await;

    // A second tenant's environment, inserted under its own scope.
    let other_tenant = Uuid::new_v4();
    let other_scope = AccessScope::for_tenant(other_tenant);
    let now = OffsetDateTime::now_utc();
    let model = environment::ActiveModel {
        id: ActiveValue::Set(Uuid::new_v4()),
        tenant_id: ActiveValue::Set(other_tenant),
        name: ActiveValue::Set("env-0000".to_owned()),
        product_id: ActiveValue::Set(Uuid::from_u128(0x9001)),
        description: ActiveValue::Set(None),
        available: ActiveValue::Set(true),
        observed_version: ActiveValue::Set(None),
        observed_build: ActiveValue::Set(None),
        default_branch: ActiveValue::Set(None),
        is_default: ActiveValue::Set(false),
        version_detect_error: ActiveValue::Set(None),
        version_detected_at: ActiveValue::Set(None),
        credentials: ActiveValue::Set(serde_json::json!([])),
        observed_attrs: ActiveValue::Set(serde_json::json!({})),
        config: ActiveValue::Set(serde_json::json!({})),
        observed_base_url: ActiveValue::Set(None),
        health_state: ActiveValue::Set("unknown".to_owned()),
        health_detail: ActiveValue::Set(None),
        health_checked_at: ActiveValue::Set(None),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
    };
    secure_insert::<EnvironmentEntity>(model, &other_scope, &f.conn())
        .await
        .unwrap();

    // `name eq 'env-0000'` matches one row in each tenant. The caller sees one.
    let query = equals("name", "env-0000");
    let page = f
        .repo
        .list_page(&f.conn(), &f.scope, &query)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1, "a $filter must not widen the scope");
    assert_eq!(page.items[0].name, "env-0000");

    // And the row is the caller's own, not the other tenant's lookalike.
    let owned = f
        .repo
        .list_all_with_tenant(&f.conn(), &f.scope)
        .await
        .unwrap();
    assert!(owned.iter().all(|(_, tenant)| *tenant == f.tenant_id));
}

/// Both variable collections are bounded too, and page under one field set.
#[tokio::test]
async fn listing_variables_is_bounded_by_the_page_limit() {
    let f = env_fixture_with(1).await;
    let repo = OrmVariablesRepository;
    let environment_id = f
        .repo
        .list_page(&f.conn(), &f.scope, &ODataQuery::default())
        .await
        .unwrap()
        .items[0]
        .id;

    let count = DEFAULT_PAGE + 10;
    for index in 0..count {
        repo.upsert(
            &f.conn(),
            &f.scope,
            f.tenant_id,
            qa_environments_sdk::NewVariable {
                environment_id: Some(environment_id),
                name: format!("VAR_{index:04}"),
                value: "v".to_owned(),
            },
        )
        .await
        .unwrap();
    }

    let page = repo
        .list_for_environment_page(&f.conn(), &f.scope, environment_id, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), DEFAULT_PAGE);
    assert!(page.page_info.next_cursor.is_some());

    let err = repo
        .list_for_environment_page(&f.conn(), &f.scope, environment_id, &filter_on("value"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "`value` is deliberately not filterable, got {err:?}"
    );
}

/// **`FIELDS` must list every variant**, because it is what the route
/// advertises *and* what the repository translates — a variant missing from it
/// is a field that exists in the type and nowhere else, silently unfilterable.
/// Counted rather than iterated, for the reason qa-runs'
/// `every_run_field_variant_is_advertised` records: `map_field` is a total
/// exhaustive match, so a variant added to *both* is already a compile error,
/// and a variant missing from `FIELDS` is simply never iterated.
#[test]
fn every_field_variant_is_advertised() {
    /// See [`every_field_variant_is_advertised`].
    const ENVIRONMENT_FILTER_FIELD_VARIANTS: usize = 6;
    /// See [`ENVIRONMENT_FILTER_FIELD_VARIANTS`].
    const VARIABLE_FILTER_FIELD_VARIANTS: usize = 3;

    let mut names: Vec<&str> = EnvironmentFilterField::FIELDS
        .iter()
        .map(FilterField::name)
        .collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(before, names.len(), "duplicate field name: {names:?}");
    assert_eq!(names.len(), ENVIRONMENT_FILTER_FIELD_VARIANTS, "{names:?}");

    let mut names: Vec<&str> = VariableFilterField::FIELDS
        .iter()
        .map(FilterField::name)
        .collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(before, names.len(), "duplicate field name: {names:?}");
    assert_eq!(names.len(), VARIABLE_FILTER_FIELD_VARIANTS, "{names:?}");
}

/// **The physical column is `platform_id`; no wire name is.**
///
/// `entity/environment_variable.rs`'s `#[sea_orm(column_name = "platform_id")]`
/// is what keeps the two apart, and `qa-runs`' `RunFilterField::EnvironmentId`
/// records the same trap on its own table. Neither enum here advertises either
/// spelling — see [`VariableFilterField`]'s doc for why `environment_id` is a
/// plain query parameter on `/qa/v1/variables` rather than an `OData` field —
/// so this pins the absence, which is the half a reader cannot see.
#[test]
fn neither_enum_advertises_platform_id_or_environment_id() {
    for name in EnvironmentFilterField::FIELDS
        .iter()
        .map(FilterField::name)
        .chain(VariableFilterField::FIELDS.iter().map(FilterField::name))
    {
        assert_ne!(name, "platform_id");
        assert_ne!(name, "environment_id");
    }
    assert!(EnvironmentFilterField::from_name("platform_id").is_none());
    assert!(VariableFilterField::from_name("platform_id").is_none());
    assert!(VariableFilterField::from_name("environment_id").is_none());
}
