//! Tenant-scoping integration tests against a REAL in-memory `SQLite`
//! database (the gear's actual migrations + `SeaORM`-backed repositories).
//!
//! Unlike `leases_tests`/`variables_tests` (which use hand-rolled in-memory
//! mock repositories to exercise pure service logic), these tests run the
//! full stack — `PolicyEnforcer` → `SecureORM` `.secure().scope_with(scope)`
//! → `SQLite` — to verify that tenant isolation actually holds at the row
//! level, not just in service-layer plumbing.
//!
//! See `crate::test_support` for the shared DB/AuthZ double setup, and
//! `examples/toolkit/users-info/users-info/src/domain/service/tests_tenant_scoping.rs`
//! for the reference pattern this suite adapts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use qa_environments_sdk::{
    AcquireOutcome, EnvironmentPatch, LeaseMode, LeaseState, NewEnvironment, NewVariable,
};
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::test_support::{
    DenyAllAuthZ, build_services, build_services_tenant_scoped,
    build_services_tenant_scoped_with_limit, ctx, inmem_db,
};

fn new_environment(name: &str) -> NewEnvironment {
    NewEnvironment {
        // The pre-plugin pair is still a REQUEST field until Task 22;
        // Task 19 dropped only the column.
        kubeconfig_credstore_ref: Some("credstore://test".to_owned()),
        credentials: std::collections::BTreeMap::new(),
        name: name.to_owned(),
        // A product is required to store a credential at all since Task 19
        // (ruling F-13): the key comes from the product's plugin, so a
        // productless create has nowhere to put the reference.
        product_id: Uuid::from_u128(0x9001),
        description: None,
        kubeconfig: None,
        default_branch: None,
        is_default: false,
    }
}

#[tokio::test]
async fn environment_created_in_tenant_a_invisible_to_tenant_b() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    let created = services
        .environments
        .create_environment(&ctx(tenant_a), new_environment("environment-a"))
        .await
        .unwrap();

    let err = services
        .environments
        .get_environment(&ctx(tenant_b), created.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::EnvironmentNotFound { id } if id == created.id),
        "expected EnvironmentNotFound, got {err:?}"
    );

    let list_b = services
        .environments
        .list_environments(&ctx(tenant_b), &ODataQuery::default())
        .await
        .unwrap()
        .items;
    assert!(
        list_b.is_empty(),
        "tenant B must not see tenant A's environment"
    );

    let list_a = services
        .environments
        .list_environments(&ctx(tenant_a), &ODataQuery::default())
        .await
        .unwrap()
        .items;
    assert_eq!(list_a.len(), 1);
    assert_eq!(list_a[0].id, created.id);
}

#[tokio::test]
async fn variables_scoped_by_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    services
        .variables
        .upsert(
            &ctx(tenant_a),
            NewVariable {
                environment_id: None,
                name: "GLOBAL_VAR".to_owned(),
                value: "value-a".to_owned(),
            },
        )
        .await
        .unwrap();

    let vars_b = services
        .variables
        .list_for_env(&ctx(tenant_b), None, &ODataQuery::default())
        .await
        .unwrap()
        .items;
    assert!(
        vars_b.is_empty(),
        "tenant B must not see tenant A's variable"
    );

    let vars_a = services
        .variables
        .list_for_env(&ctx(tenant_a), None, &ODataQuery::default())
        .await
        .unwrap()
        .items;
    assert_eq!(vars_a.len(), 1);
    assert_eq!(vars_a[0].name, "GLOBAL_VAR");
}

#[tokio::test]
async fn environment_variables_scoped_by_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    let environment = services
        .environments
        .create_environment(&ctx(tenant_a), new_environment("environment-a"))
        .await
        .unwrap();

    services
        .variables
        .upsert(
            &ctx(tenant_a),
            NewVariable {
                environment_id: Some(environment.id),
                name: "ENVIRONMENT_VAR".to_owned(),
                value: "value-a".to_owned(),
            },
        )
        .await
        .unwrap();

    // Cross-tenant upsert targeting A's environment must 404 via the tenancy
    // precheck — it must NOT reveal whether a variable of that name exists.
    let err = services
        .variables
        .upsert(
            &ctx(tenant_b),
            NewVariable {
                environment_id: Some(environment.id),
                name: "ENVIRONMENT_VAR".to_owned(),
                value: "value-b".to_owned(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::EnvironmentNotFound { id } if id == environment.id),
        "expected EnvironmentNotFound (tenancy precheck), got {err:?}"
    );
}

#[tokio::test]
async fn lease_scoped_by_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    let environment = services
        .environments
        .create_environment(&ctx(tenant_a), new_environment("environment-a"))
        .await
        .unwrap();

    let run_id = Uuid::new_v4();
    let outcome = services
        .leases
        .acquire(&ctx(tenant_a), environment.id, run_id, LeaseMode::Parallel)
        .await
        .unwrap();
    assert_eq!(outcome, AcquireOutcome::Acquired);

    // Tenant B must not observe tenant A's holders. The environment itself was
    // created under tenant A, so under B's tenant-scoped AccessScope the
    // environment precheck in `LeasesService::get` (see `leases.rs`) fails to
    // find it and the read 404s — it must NOT silently read as
    // `LeaseState::Free`, which would be indistinguishable from "never
    // leased" for an environment B is actually allowed to see.
    let err_b = services
        .leases
        .get(&ctx(tenant_b), environment.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err_b, DomainError::EnvironmentNotFound { id } if id == environment.id),
        "expected EnvironmentNotFound, got {err_b:?}"
    );

    // Tenant A still sees its own hold.
    let state_a = services
        .leases
        .get(&ctx(tenant_a), environment.id)
        .await
        .unwrap();
    assert_eq!(
        state_a,
        LeaseState::HeldParallel {
            holders: vec![run_id]
        }
    );
}

#[tokio::test]
async fn pdp_deny_blocks_create() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let services = build_services(db, Arc::new(DenyAllAuthZ));

    let err = services
        .environments
        .create_environment(&ctx(tenant_a), new_environment("environment-a"))
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "expected Forbidden, got {err:?}"
    );
}

#[tokio::test]
async fn cross_tenant_update_and_delete_blocked() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    let environment = services
        .environments
        .create_environment(&ctx(tenant_a), new_environment("environment-a"))
        .await
        .unwrap();

    let update_err = services
        .environments
        .update_environment(
            &ctx(tenant_b),
            environment.id,
            EnvironmentPatch {
                description: Some(Some("hijacked".to_owned())),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(update_err, DomainError::EnvironmentNotFound { id } if id == environment.id),
        "expected EnvironmentNotFound on cross-tenant update, got {update_err:?}"
    );

    let delete_err = services
        .environments
        .delete_environment(&ctx(tenant_b), environment.id)
        .await
        .unwrap_err();
    assert!(
        matches!(delete_err, DomainError::EnvironmentNotFound { id } if id == environment.id),
        "expected EnvironmentNotFound on cross-tenant delete, got {delete_err:?}"
    );

    // The row must still exist, untouched, for tenant A.
    let still_there = services
        .environments
        .get_environment(&ctx(tenant_a), environment.id)
        .await
        .unwrap();
    assert_eq!(still_there.id, environment.id);
    assert_eq!(
        still_there.description, None,
        "update from B must not have applied"
    );
}

#[tokio::test]
async fn unique_name_is_per_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    // The unique index is (tenant_id, name): the same name may exist once
    // per tenant.
    services
        .environments
        .create_environment(&ctx(tenant_a), new_environment("shared-name"))
        .await
        .unwrap();
    services
        .environments
        .create_environment(&ctx(tenant_b), new_environment("shared-name"))
        .await
        .unwrap();

    // A duplicate within the SAME tenant must fail.
    let err = services
        .environments
        .create_environment(&ctx(tenant_a), new_environment("shared-name"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::EnvironmentNameExists { ref name } if name == "shared-name"),
        "expected EnvironmentNameExists, got {err:?}"
    );
}

#[tokio::test]
async fn delete_leased_environment_blocked_until_release() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    let environment = services
        .environments
        .create_environment(&ctx(tenant_a), new_environment("environment-a"))
        .await
        .unwrap();

    let run_id = Uuid::new_v4();
    let outcome = services
        .leases
        .acquire(&ctx(tenant_a), environment.id, run_id, LeaseMode::Exclusive)
        .await
        .unwrap();
    assert_eq!(outcome, AcquireOutcome::Acquired);

    // Deletion must be blocked while the environment holds an active lease.
    let delete_err = services
        .environments
        .delete_environment(&ctx(tenant_a), environment.id)
        .await
        .unwrap_err();
    assert!(
        matches!(delete_err, DomainError::EnvironmentLeased { id } if id == environment.id),
        "expected EnvironmentLeased while held, got {delete_err:?}"
    );

    services
        .leases
        .release(&ctx(tenant_a), environment.id, run_id)
        .await
        .unwrap();

    // Once released, deletion succeeds.
    services
        .environments
        .delete_environment(&ctx(tenant_a), environment.id)
        .await
        .unwrap();

    let get_err = services
        .environments
        .get_environment(&ctx(tenant_a), environment.id)
        .await
        .unwrap_err();
    assert!(
        matches!(get_err, DomainError::EnvironmentNotFound { id } if id == environment.id),
        "expected EnvironmentNotFound after delete, got {get_err:?}"
    );
}

#[tokio::test]
async fn list_for_env_caps_at_max_variables() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let services = build_services_tenant_scoped_with_limit(db, 2);

    for name in ["VAR_A", "VAR_B", "VAR_C"] {
        services
            .variables
            .upsert(
                &ctx(tenant_a),
                NewVariable {
                    environment_id: None,
                    name: name.to_owned(),
                    value: "value".to_owned(),
                },
            )
            .await
            .unwrap();
    }

    let vars = services
        .variables
        .list_for_env(&ctx(tenant_a), None, &ODataQuery::default())
        .await
        .unwrap()
        .items;
    assert_eq!(
        vars.len(),
        2,
        "list_for_env must truncate to the configured max_variables"
    );
}
