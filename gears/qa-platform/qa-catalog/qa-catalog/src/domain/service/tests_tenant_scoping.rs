//! Tenant-scoping integration tests against a REAL in-memory `SQLite`
//! database (the gear's actual migrations + `SeaORM`-backed repositories).
//!
//! Unlike the sibling `*_tests` modules (which use hand-rolled in-memory
//! mock repositories to exercise pure service logic), these tests run the
//! full stack — `PolicyEnforcer` → `SecureORM` `.secure().scope_with(scope)`
//! → `SQLite` — to verify that tenant isolation actually holds at the row
//! level, not just in service-layer plumbing.
//!
//! See `crate::test_support` for the shared DB/AuthZ double setup, and
//! `gears/qa-platform/qa-environments/.../tests_tenant_scoping.rs` for the
//! reference pattern this suite adapts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use qa_catalog_sdk::{
    CustomPlanEntry, NewCustomPlan, NewCustomPlanEntry, NewProduct, NewTestRepository,
    ProductUpdate, TestRepositoryUpdate,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::system_actor;
use crate::test_support::{
    DenyAllAuthZ, all_branch_rows, build_services, build_services_tenant_scoped,
    build_services_tenant_scoped_at, build_services_tenant_scoped_with_credstore,
    build_services_with_branch_listing, ctx, inmem_db, seed_expired_bundle, seed_product,
    seed_raw_custom_plan_row,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

fn new_repo(name: &str, product_id: Uuid) -> NewTestRepository {
    NewTestRepository {
        product_id,
        name: name.to_owned(),
        url: "https://example.com/org/repo.git".to_owned(),
        default_branch: "main".to_owned(),
        content_root: String::new(),
        credential_ref: None,
    }
}

/// Two entries naming **different** nested plans, so every DB-backed custom-plan
/// test writes and reads back two distinct `plan_path` values and a codec that
/// dropped or hardcoded the field could not pass.
///
/// Both carry one because `plan_path` is mandatory on write. A stored entry
/// *without* one is now reachable only by decoding a pre-field row, which
/// [`a_row_stored_before_plan_path_existed_still_loads_through_the_service`]
/// covers by seeding that payload directly through the entity.
fn new_custom_plan(name: &str, repo_id: Uuid) -> NewCustomPlan {
    NewCustomPlan {
        name: name.to_owned(),
        files: vec![
            NewCustomPlanEntry {
                repo_id,
                path: "tests/a.py".to_owned(),
                plan_path: "plans/smoke.yaml".to_owned(),
            },
            NewCustomPlanEntry {
                repo_id,
                path: "tests/b.py".to_owned(),
                plan_path: "plans/nightly.yaml".to_owned(),
            },
        ],
        tags: vec![],
        timeout_seconds: None,
    }
}

/// What [`new_custom_plan`] should read back as: the same entries widened to the
/// read model, `plan_path` filled in.
fn expected_entries(name: &str, repo_id: Uuid) -> Vec<CustomPlanEntry> {
    new_custom_plan(name, repo_id)
        .files
        .into_iter()
        .map(CustomPlanEntry::from)
        .collect()
}

#[tokio::test]
async fn repo_created_in_tenant_a_invisible_to_tenant_b() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_id = seed_product(&services, &ctx(tenant_a), "product-a").await;

    let created = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("repo-a", product_id))
        .await
        .unwrap();

    let err = services
        .repos
        .get_repo(&ctx(tenant_b), created.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == created.id),
        "expected NotFound, got {err:?}"
    );

    let list_b = services.repos.list_repos(&ctx(tenant_b)).await.unwrap();
    assert!(list_b.is_empty(), "tenant B must not see tenant A's repo");

    let list_a = services.repos.list_repos(&ctx(tenant_a)).await.unwrap();
    assert_eq!(list_a.len(), 1);
    assert_eq!(list_a[0].id, created.id);

    // The branch-cache read must 404 for the foreign tenant too — it must
    // NOT read as "no branches", which would be indistinguishable from a
    // never-synced repository B is actually allowed to see.
    let err_b = services
        .repos
        .list_branches(&ctx(tenant_b), created.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err_b, DomainError::NotFound { id } if id == created.id),
        "expected NotFound on cross-tenant branch read, got {err_b:?}"
    );
}

#[tokio::test]
async fn cross_tenant_repo_delete_blocked() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_id = seed_product(&services, &ctx(tenant_a), "product-a").await;

    let repo = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("repo-a", product_id))
        .await
        .unwrap();

    let delete_err = services
        .repos
        .delete_repo(&ctx(tenant_b), repo.id)
        .await
        .unwrap_err();
    assert!(
        matches!(delete_err, DomainError::NotFound { id } if id == repo.id),
        "expected NotFound on cross-tenant delete, got {delete_err:?}"
    );

    // The row must still exist, untouched, for tenant A.
    let still_there = services
        .repos
        .get_repo(&ctx(tenant_a), repo.id)
        .await
        .unwrap();
    assert_eq!(still_there.id, repo.id);
}

/// Cross-tenant update must 404 and leave the row untouched — the same
/// guarantee `cross_tenant_repo_delete_blocked` gives the delete verb.
#[tokio::test]
async fn cross_tenant_repo_update_blocked() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_id = seed_product(&services, &ctx(tenant_a), "product-a").await;

    let repo = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("repo-a", product_id))
        .await
        .unwrap();

    let err = services
        .repos
        .update_repo(
            &ctx(tenant_b),
            repo.id,
            TestRepositoryUpdate {
                product_id,
                name: "hijacked".to_owned(),
                url: "https://evil.example.com/org/repo.git".to_owned(),
                default_branch: "main".to_owned(),
                content_root: String::new(),
                credential_ref: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == repo.id),
        "expected NotFound on cross-tenant update, got {err:?}"
    );

    let untouched = services
        .repos
        .get_repo(&ctx(tenant_a), repo.id)
        .await
        .unwrap();
    assert_eq!(
        untouched.name, "repo-a",
        "update from B must not have applied"
    );
    assert_eq!(untouched.url, "https://example.com/org/repo.git");
}

/// The unique index is `(tenant_id, name)`, so an update renaming a repository
/// onto a sibling's name is a domain conflict (HTTP 409), not a 500 — while the
/// same name in another tenant is no conflict at all.
#[tokio::test]
async fn repo_update_to_a_duplicate_name_conflicts_within_the_tenant_only() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_a = seed_product(&services, &ctx(tenant_a), "product-a").await;
    let product_b = seed_product(&services, &ctx(tenant_b), "product-b").await;

    services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("taken", product_a))
        .await
        .unwrap();
    let second = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("free", product_a))
        .await
        .unwrap();
    let other_tenant = services
        .repos
        .create_repo(&ctx(tenant_b), new_repo("b-repo", product_b))
        .await
        .unwrap();

    let err = services
        .repos
        .update_repo(
            &ctx(tenant_a),
            second.id,
            TestRepositoryUpdate {
                product_id: product_a,
                name: "taken".to_owned(),
                url: second.url.clone(),
                default_branch: second.default_branch.clone(),
                content_root: String::new(),
                credential_ref: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::RepositoryNameExists { ref name } if name == "taken"),
        "expected RepositoryNameExists, got {err:?}"
    );

    // Tenant B may take the very same name.
    let renamed = services
        .repos
        .update_repo(
            &ctx(tenant_b),
            other_tenant.id,
            TestRepositoryUpdate {
                product_id: product_b,
                name: "taken".to_owned(),
                url: other_tenant.url.clone(),
                default_branch: other_tenant.default_branch.clone(),
                content_root: String::new(),
                credential_ref: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.name, "taken");
}

#[tokio::test]
async fn custom_plan_scoped_by_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_id = seed_product(&services, &ctx(tenant_a), "product-a").await;

    let repo = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("repo-a", product_id))
        .await
        .unwrap();
    let plan = services
        .custom_plans
        .create_custom_plan(&ctx(tenant_a), new_custom_plan("plan-a", repo.id))
        .await
        .unwrap();

    let err = services
        .custom_plans
        .get_custom_plan(&ctx(tenant_b), plan.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == plan.id),
        "expected NotFound, got {err:?}"
    );

    let list_b = services
        .custom_plans
        .list_custom_plans(&ctx(tenant_b))
        .await
        .unwrap();
    assert!(list_b.is_empty(), "tenant B must not see tenant A's plan");

    // Cross-tenant update and delete must 404 and leave the row untouched.
    let update_err = services
        .custom_plans
        .update_custom_plan(
            &ctx(tenant_b),
            plan.id,
            new_custom_plan("hijacked", repo.id),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(update_err, DomainError::NotFound { id } if id == plan.id),
        "expected NotFound on cross-tenant update, got {update_err:?}"
    );

    let delete_err = services
        .custom_plans
        .delete_custom_plan(&ctx(tenant_b), plan.id)
        .await
        .unwrap_err();
    assert!(
        matches!(delete_err, DomainError::NotFound { id } if id == plan.id),
        "expected NotFound on cross-tenant delete, got {delete_err:?}"
    );

    let still_there = services
        .custom_plans
        .get_custom_plan(&ctx(tenant_a), plan.id)
        .await
        .unwrap();
    assert_eq!(
        still_there.name, "plan-a",
        "update from B must not have applied"
    );
}

/// `plan_path` survives a **real** insert, read, update and re-read through the
/// migration's own `qa_custom_plans.files` column.
///
/// The mapper's unit tests cover the codec in isolation and
/// `stored_pre_plan_path_rows_still_decode` covers the stored legacy payload;
/// neither runs a query. This one does, and it is the only thing here that
/// would catch the column being written by a path the codec does not go
/// through — the class of defect `cargo build` cannot see, because the column
/// name and the JSON shape are both runtime strings.
///
/// **The dialect limit:** `inmem_db` is `sqlite::memory:`, where the column is
/// `files TEXT NOT NULL DEFAULT '[]'`. The `MySQL` (`JSON`) and Postgres
/// (`JSONB`) definitions of the same column are asserted by inspection only —
/// no test in this gear executes them. No DDL changed here, so that limit is
/// inherited rather than introduced.
#[tokio::test]
async fn custom_plan_entries_round_trip_through_the_database() {
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_id = seed_product(&services, &ctx(tenant), "product-a").await;

    let repo = services
        .repos
        .create_repo(&ctx(tenant), new_repo("repo-a", product_id))
        .await
        .unwrap();

    let created = services
        .custom_plans
        .create_custom_plan(&ctx(tenant), new_custom_plan("plan-a", repo.id))
        .await
        .unwrap();
    assert_eq!(
        created.files,
        expected_entries("plan-a", repo.id),
        "the insert's own returned model must carry plan_path"
    );

    // A fresh read, so the assertion goes through the decoder rather than the
    // value the writer happened to still hold.
    let fetched = services
        .custom_plans
        .get_custom_plan(&ctx(tenant), created.id)
        .await
        .unwrap();
    assert_eq!(
        fetched.files,
        expected_entries("plan-a", repo.id),
        "plan_path must survive the round trip through the column"
    );

    // The update path encodes `files` separately from the insert path, so it
    // needs its own assertion — the same field written by two call sites.
    let replaced = NewCustomPlan {
        files: vec![NewCustomPlanEntry {
            repo_id: repo.id,
            path: "tests/c.py".to_owned(),
            plan_path: "plans/weekly.yaml".to_owned(),
        }],
        ..new_custom_plan("plan-a", repo.id)
    };
    services
        .custom_plans
        .update_custom_plan(&ctx(tenant), created.id, replaced.clone())
        .await
        .unwrap();

    let after_update = services
        .custom_plans
        .get_custom_plan(&ctx(tenant), created.id)
        .await
        .unwrap();
    assert_eq!(
        after_update.files,
        replaced
            .files
            .into_iter()
            .map(CustomPlanEntry::from)
            .collect::<Vec<_>>()
    );
}

/// `plan_path` is operator-supplied and is joined to a repository content root
/// on the read side, so traversal has to be rejected at write time exactly as
/// `path` is. Its *presence* is enforced by the type; this is about its content.
#[tokio::test]
async fn a_traversing_plan_path_is_rejected_at_write_time() {
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_id = seed_product(&services, &ctx(tenant), "product-a").await;

    let repo = services
        .repos
        .create_repo(&ctx(tenant), new_repo("repo-a", product_id))
        .await
        .unwrap();

    let malicious = NewCustomPlan {
        files: vec![NewCustomPlanEntry {
            repo_id: repo.id,
            path: "tests/a.py".to_owned(),
            plan_path: "../../etc/plan.yaml".to_owned(),
        }],
        ..new_custom_plan("plan-a", repo.id)
    };

    let err = services
        .custom_plans
        .create_custom_plan(&ctx(tenant), malicious)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "files.plan_path"),
        "expected a Validation error naming files.plan_path, got {err:?}"
    );
}

/// A mandatory `String` field is satisfied by `""` as far as the type system is
/// concerned, so the emptiness check has to be a real one.
///
/// `validate_rel_path` rejects empty first (`plans.rs:281-283`), which is the only
/// reason `plan_path: String` actually means "names a plan" rather than "has a
/// field". Without this the type change would be decorative.
#[tokio::test]
async fn an_empty_plan_path_is_rejected_even_though_the_type_permits_it() {
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_id = seed_product(&services, &ctx(tenant), "product-a").await;

    let repo = services
        .repos
        .create_repo(&ctx(tenant), new_repo("repo-a", product_id))
        .await
        .unwrap();

    let blank = NewCustomPlan {
        files: vec![NewCustomPlanEntry {
            repo_id: repo.id,
            path: "tests/a.py".to_owned(),
            plan_path: String::new(),
        }],
        ..new_custom_plan("plan-a", repo.id)
    };

    let err = services
        .custom_plans
        .create_custom_plan(&ctx(tenant), blank)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "files.plan_path"),
        "expected a Validation error naming files.plan_path, got {err:?}"
    );
}

/// **The end-to-end half of the payload-compatibility guarantee**, and since
/// `plan_path` became mandatory on write it is the *only* way to produce a stored
/// entry without one.
///
/// The row is seeded through the entity in the two-element array shape a pre-field
/// writer emitted, then read back through the real repository and service. The
/// mapper's `stored_pre_plan_path_rows_still_decode` covers the codec in
/// isolation; this covers the column, the entity, the decoder and the service
/// together, which is the only combination that can show the column name or the
/// stored shape being wrong at runtime.
///
/// If this reddens, every custom plan created before 2026-08-14 is unreadable and
/// every launch targeting one fails.
///
/// **Dialect limit:** `sqlite::memory:`, where the column is
/// `files TEXT NOT NULL DEFAULT '[]'`. The `MySQL` (`JSON`) and Postgres (`JSONB`)
/// definitions are asserted by inspection only. No DDL changed, so the limit is
/// inherited rather than introduced.
#[tokio::test]
async fn a_row_stored_before_plan_path_existed_still_loads_through_the_service() {
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let services = build_services_tenant_scoped(db.clone());
    let product_id = seed_product(&services, &ctx(tenant), "product-a").await;

    let repo = services
        .repos
        .create_repo(&ctx(tenant), new_repo("repo-a", product_id))
        .await
        .unwrap();

    // Exactly what `serde_json::to_value(&[(Uuid, String)])` produced before
    // `plan_path` existed — a two-element array per entry, no object keys.
    let legacy_payload = serde_json::json!([[repo.id.to_string(), "tests/legacy.py"]]);
    let plan_id =
        seed_raw_custom_plan_row(&db, tenant, "stored-before-plan-path", legacy_payload).await;

    let loaded = services
        .custom_plans
        .get_custom_plan(&ctx(tenant), plan_id)
        .await
        .expect("a row stored before plan_path existed must still load");

    assert_eq!(
        loaded.files,
        vec![CustomPlanEntry {
            repo_id: repo.id,
            path: "tests/legacy.py".to_owned(),
            plan_path: None,
        }],
        "the pre-field payload must decode to plan_path: None, not fail closed"
    );

    // And it must survive a listing too, which decodes through the same mapper
    // but a different query.
    let listed = services
        .custom_plans
        .list_custom_plans(&ctx(tenant))
        .await
        .expect("listing must not fail on a pre-plan_path row");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].files[0].plan_path, None);
}

/// **Where the cross-tenant guarantee for a custom plan's `plan_path` is actually
/// enforced.** Raised by Task 13c's security review.
///
/// `CustomPlanEntry::repo_id` has no foreign key and is not validated on write, so
/// an operator can store a custom plan naming another tenant's repository. qa-runs
/// then hands `(repo_id, branch, plan_path)` to `get_plan` while resolving
/// exclusivity, and the *only* thing stopping that from reading a foreign
/// repository's `plan.yaml` is this scope check — which also decides whether
/// `qa_runs.exclusive_tier`, a caller-readable field, can discriminate a foreign
/// plan's existence. See `qa-runs`' `resolve_nested_exclusivity`.
///
/// Asserted here rather than in qa-runs because this is where the enforcement is:
/// a qa-runs test could only assert its own double.
#[tokio::test]
async fn plan_and_test_meta_reads_are_scoped_to_the_callers_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_id = seed_product(&services, &ctx(tenant_a), "product-a").await;

    let repo = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("repo-a", product_id))
        .await
        .unwrap();

    let err = services
        .plans
        .get_plan(&ctx(tenant_b), repo.id, "main", "plans/smoke.yaml")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == repo.id),
        "a foreign tenant must not resolve a plan under another tenant's repo; got {err:?}"
    );

    let err = services
        .plans
        .get_test_meta(&ctx(tenant_b), repo.id, "main", &["tests/a.py".to_owned()])
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == repo.id),
        "the TEST_META read must be scoped the same way; got {err:?}"
    );

    // The owning tenant gets past the scope check and fails for a *content*
    // reason instead — which is what makes the assertions above about tenancy
    // rather than about the repo being unsynced for everyone.
    let own = services
        .plans
        .get_plan(&ctx(tenant_a), repo.id, "main", "plans/smoke.yaml")
        .await;
    assert!(
        !matches!(own, Err(DomainError::NotFound { .. })),
        "the owning tenant must not get NotFound for its own repo; got {own:?}"
    );
}

#[tokio::test]
async fn product_scoped_by_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    let product = services
        .products
        .create_product(
            &ctx(tenant_a),
            NewProduct {
                name: "vhi".to_owned(),
                key: "VHI".to_owned(),
                description: "Virtuozzo Hybrid Infrastructure".to_owned(),
                folder: Some("virt".to_owned()),
                // Every product names a plugin since Task 20 (D6), and the target
                // must be registered (ruling F-10).
                plugin_instance_id: crate::test_support::FIXTURE_PLUGIN_INSTANCE_ID.to_owned(),
            },
        )
        .await
        .unwrap();

    let list_b = services
        .products
        .list_products(&ctx(tenant_b))
        .await
        .unwrap();
    assert!(
        list_b.is_empty(),
        "tenant B must not see tenant A's product"
    );

    // Update and delete must 404 across the tenant boundary and leave the
    // row untouched.
    let update_err = services
        .products
        .update_product(
            &ctx(tenant_b),
            product.id,
            ProductUpdate {
                name: "hijacked".to_owned(),
                key: "HIJACKED".to_owned(),
                description: "hijacked description".to_owned(),
                folder: None,
                plugin_instance_id: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(update_err, DomainError::NotFound { id } if id == product.id),
        "expected NotFound on cross-tenant product update, got {update_err:?}"
    );

    let delete_err = services
        .products
        .delete_product(&ctx(tenant_b), product.id)
        .await
        .unwrap_err();
    assert!(
        matches!(delete_err, DomainError::NotFound { id } if id == product.id),
        "expected NotFound on cross-tenant product delete, got {delete_err:?}"
    );

    let list_a = services
        .products
        .list_products(&ctx(tenant_a))
        .await
        .unwrap();
    assert_eq!(list_a.len(), 1, "the product must survive B's attempts");
    assert_eq!(list_a[0].name, "vhi", "update from B must not have applied");
    assert_eq!(list_a[0].key, "VHI", "update from B must not have applied");
    assert_eq!(
        list_a[0].description, "Virtuozzo Hybrid Infrastructure",
        "update from B must not have applied"
    );

    // The distinct-folder projection is tenant-scoped as well.
    let folders_a = services
        .products
        .list_product_folders(&ctx(tenant_a))
        .await
        .unwrap();
    assert_eq!(folders_a, vec!["virt".to_owned()]);

    let folders_b = services
        .products
        .list_product_folders(&ctx(tenant_b))
        .await
        .unwrap();
    assert!(
        folders_b.is_empty(),
        "tenant B must not see tenant A's folders"
    );
}

#[tokio::test]
async fn ssh_key_scoped_by_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);

    let key = services
        .ssh_keys
        .create_ssh_key(
            &ctx(tenant_a),
            "deploy".to_owned(),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nx\n-----END OPENSSH PRIVATE KEY-----\n"
                .to_owned(),
        )
        .await
        .unwrap();

    let list_b = services
        .ssh_keys
        .list_ssh_keys(&ctx(tenant_b))
        .await
        .unwrap();
    assert!(list_b.is_empty(), "tenant B must not see tenant A's key");

    let delete_err = services
        .ssh_keys
        .delete_ssh_key(&ctx(tenant_b), key.id)
        .await
        .unwrap_err();
    assert!(
        matches!(delete_err, DomainError::NotFound { id } if id == key.id),
        "expected NotFound on cross-tenant delete, got {delete_err:?}"
    );

    let list_a = services
        .ssh_keys
        .list_ssh_keys(&ctx(tenant_a))
        .await
        .unwrap();
    assert_eq!(list_a.len(), 1, "the key must survive B's delete attempt");
}

#[tokio::test]
async fn pdp_deny_blocks_create() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let services = build_services(db, Arc::new(DenyAllAuthZ));

    let err = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("repo-a", Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Forbidden),
        "expected Forbidden, got {err:?}"
    );

    let err = services
        .products
        .create_product(
            &ctx(tenant_a),
            NewProduct {
                name: "vhi".to_owned(),
                key: "VHI".to_owned(),
                description: String::new(),
                folder: None,
                // Every product names a plugin since Task 20 (D6), and the target
                // must be registered (ruling F-10).
                plugin_instance_id: crate::test_support::FIXTURE_PLUGIN_INSTANCE_ID.to_owned(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Forbidden),
        "expected Forbidden, got {err:?}"
    );
}

/// **Review finding m-2.** An unauthorized caller must not learn which product
/// plugins this deployment registers.
///
/// `require_registered_plugin` queries process state, unlike the validators
/// beside it which only inspect the request. While it ran *before*
/// `access_scope`, a caller the policy denies got `Validation` for an
/// unregistered plugin id and `Forbidden` for a registered one — a two-answer
/// oracle for deployment topology, usable without any permission at all.
///
/// Both spellings must now answer `Forbidden`. `pdp_deny_blocks_create` above
/// covers the registered id; this covers the unregistered one, which is the
/// half that used to differ.
#[tokio::test]
async fn pdp_deny_does_not_reveal_whether_a_plugin_is_registered() {
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let services = build_services(db, Arc::new(DenyAllAuthZ));

    for (label, plugin_instance_id) in [
        (
            "registered",
            crate::test_support::FIXTURE_PLUGIN_INSTANCE_ID.to_owned(),
        ),
        (
            "well-formed but registered by nothing",
            // Same shape as the fixture, different instance segment, so it
            // passes the syntax validator and reaches the presence check --
            // which is the only ordering this test can measure.
            "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~acme.core._.absent.v1"
                .to_owned(),
        ),
    ] {
        let err = services
            .products
            .create_product(
                &ctx(tenant),
                NewProduct {
                    name: "vhi".to_owned(),
                    key: "VHI".to_owned(),
                    description: String::new(),
                    folder: None,
                    plugin_instance_id,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Forbidden),
            "{label}: a denied caller must get the same answer either way, or the \
             refusal itself says whether the plugin is registered here -- got {err:?}"
        );
    }
}

#[tokio::test]
async fn unique_repo_name_is_per_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped(db);
    let product_a = seed_product(&services, &ctx(tenant_a), "product-a").await;
    let product_b = seed_product(&services, &ctx(tenant_b), "product-b").await;

    // The unique index is (tenant_id, name): the same name may exist once
    // per tenant.
    services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("shared-name", product_a))
        .await
        .unwrap();
    services
        .repos
        .create_repo(&ctx(tenant_b), new_repo("shared-name", product_b))
        .await
        .unwrap();

    // A duplicate within the SAME tenant must fail.
    let err = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("shared-name", product_a))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::RepositoryNameExists { ref name } if name == "shared-name"),
        "expected RepositoryNameExists, got {err:?}"
    );
}

/// The branch-cache refresher (lifecycle task in `crate::gear`) enumerates
/// `(repo, tenant)` targets under one system context and then refreshes each
/// repository under a *second* system context bound to that repository's own
/// tenant — because `replace_branches` files the rewritten rows under
/// `ctx.subject_tenant_id()`. This is the DB-backed proof that the binding
/// survives the whole path: every `qa_repo_branches` row must carry its own
/// repository's tenant, and each tenant's scoped read must see only its own.
///
/// It drives the real `system_actor` factories rather than plain tenant
/// contexts, so the test also pins the deployment obligation the *refresh*
/// step still carries: the PDP must grant the system actor a per-tenant
/// `TEST_REPO`/`SYNC` scope for `refresh_branches` to write anything (see
/// `test_support::SystemActorGrantAuthZ`). The *enumeration* step carries no
/// such obligation any more — `list_refresh_targets` elevates through
/// `domain::elevated` instead of asking the PDP
/// ([`branch_refresh_enumeration_does_not_consult_the_policy_engine`], below,
/// pins that directly) — so `SystemActorGrantAuthZ`'s grant here is exercised
/// only by the refresh half of this test, not the listing half.
#[tokio::test]
async fn refreshed_branch_rows_carry_their_own_repo_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_with_branch_listing(
        db.clone(),
        &["main", "release/9.5"],
        vec![tenant_a, tenant_b],
    );
    let product_a = seed_product(&services, &ctx(tenant_a), "product-a").await;
    let product_b = seed_product(&services, &ctx(tenant_b), "product-b").await;

    let repo_a = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("repo-a", product_a))
        .await
        .unwrap();
    let repo_b = services
        .repos
        .create_repo(&ctx(tenant_b), new_repo("repo-b", product_b))
        .await
        .unwrap();

    // Enumeration step, under the enumeration system actor.
    let mut targets = services
        .repos
        .list_refresh_targets(&system_actor::for_branch_refresh_enumeration())
        .await
        .unwrap();
    targets.sort_by_key(|t| t.repo_id);
    let mut expected = vec![(repo_a.id, tenant_a), (repo_b.id, tenant_b)];
    expected.sort_by_key(|(id, _)| *id);
    assert_eq!(
        targets
            .iter()
            .map(|t| (t.repo_id, t.tenant_id))
            .collect::<Vec<_>>(),
        expected,
        "each target must carry its own repository's tenant"
    );

    // Refresh step, exactly as the lifecycle task does it: one tenant-bound
    // system context per target.
    for target in &targets {
        services
            .repos
            .refresh_branches(
                &system_actor::for_branch_refresh(target.tenant_id),
                target.repo_id,
            )
            .await
            .unwrap();
    }

    // Ground truth: every row's tenant_id matches its own repo's tenant.
    let rows = all_branch_rows(&db).await;
    assert_eq!(rows.len(), 4, "two repos x two branches, got {rows:?}");
    for (repo_id, tenant_id, name) in &rows {
        let expected_tenant = if *repo_id == repo_a.id {
            tenant_a
        } else {
            tenant_b
        };
        assert_eq!(
            *tenant_id, expected_tenant,
            "branch '{name}' of repo {repo_id} was filed under the wrong tenant"
        );
    }

    // ...and the scoped read path agrees: each tenant sees only its own.
    assert_eq!(
        services
            .repos
            .list_branches(&ctx(tenant_a), repo_a.id)
            .await
            .unwrap(),
        vec!["main".to_owned(), "release/9.5".to_owned()]
    );
    assert_eq!(
        services
            .repos
            .list_branches(&ctx(tenant_b), repo_b.id)
            .await
            .unwrap(),
        vec!["main".to_owned(), "release/9.5".to_owned()]
    );
    let cross = services
        .repos
        .list_branches(&ctx(tenant_b), repo_a.id)
        .await
        .unwrap_err();
    assert!(
        matches!(cross, DomainError::NotFound { id } if id == repo_a.id),
        "expected NotFound reading tenant A's branches as B, got {cross:?}"
    );
}

/// [`AuthZResolverApi`] double that records every request it is asked to
/// decide and always grants tenant-scoped access — the harness for
/// [`branch_refresh_enumeration_does_not_consult_the_policy_engine`]: a
/// request that slipped through to `evaluate` is recorded here regardless of
/// what the resulting decision happened to be.
#[derive(Default)]
struct RecordingAuthZ {
    requests: Mutex<Vec<(String, String)>>,
}

impl RecordingAuthZ {
    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl AuthZResolverApi for RecordingAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.requests
            .lock()
            .unwrap()
            .push((request.resource.resource_type, request.action.name));
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// The property this task exists to establish: `list_refresh_targets` never
/// asks the policy engine anything, even under a double that would grant it
/// nothing (an empty-constraint decision, exactly what the shipped
/// `static-authz-plugin` hands a nil-tenant subject) and would therefore have
/// made the pre-elevation enumeration fail closed.
///
/// Against the real `ReposService`/`SeaORM` stack, like
/// [`refreshed_branch_rows_carry_their_own_repo_tenant`] above, so a
/// regression that reintroduced a PDP round-trip inside `list_refresh_targets`
/// would show up here rather than only in a mock-repository unit test.
///
/// Break-tested: reverting `ReposService::list_refresh_targets` to call
/// `self.policy_enforcer.access_scope(...)` again makes this test fail with a
/// non-empty request log.
#[tokio::test]
async fn branch_refresh_enumeration_does_not_consult_the_policy_engine() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();

    // Seed one repository with an ordinary, permissive `AuthZ` double first —
    // the property under test is the enumeration's own behaviour, not
    // whether a repository can be created at all.
    let seeding_services = build_services_tenant_scoped(db.clone());
    let product_a = seed_product(&seeding_services, &ctx(tenant_a), "product-a").await;
    seeding_services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("repo-a", product_a))
        .await
        .unwrap();

    let enforcer = Arc::new(RecordingAuthZ::default());
    let services = build_services(db, Arc::clone(&enforcer) as _);

    let _outcome = services
        .repos
        .list_refresh_targets(&system_actor::for_branch_refresh_enumeration())
        .await;

    assert_eq!(
        enforcer.request_count(),
        0,
        "the branch-cache enumeration must elevate through domain::elevated, not the PEP"
    );
}

/// The bundle GC's full split, against the real `SeaORM` repository: the
/// enumeration (`BundlesService::tenants_with_expired_bundles`, elevated
/// through `domain::elevated`) finds every tenant with an expired bundle —
/// `DISTINCT`, ascending, a tenant with only a live bundle absent — and
/// looping `purge_expired` under each tenant's own
/// `system_actor::for_bundle_delete` context, exactly as
/// `crate::gear::QaCatalog::run_bundle_gc` does, removes only that tenant's
/// expired row.
///
/// This is the DB-backed proof `branch_refresh_enumeration_does_not_consult_the_policy_engine`
/// and `refreshed_branch_rows_carry_their_own_repo_tenant` are for the branch
/// refresher: a mock-repository unit test
/// (`domain::service::bundles_tests`) cannot catch a bug in the real
/// `SELECT DISTINCT ... ORDER BY` this crate's `SecureORM` layer compiles,
/// and this task added that query.
#[tokio::test]
async fn bundle_gc_enumerates_and_purges_each_tenant_under_its_own_scope() {
    let db = inmem_db().await;
    // Fixed, ascending: the enumeration's ordering is part of what this test
    // pins, so the fixtures cannot leave that to `Uuid::new_v4`'s luck.
    let tenant_a = Uuid::from_u128(0x0A);
    let tenant_b = Uuid::from_u128(0xB0);
    let tenant_live_only = Uuid::from_u128(0xC0);

    seed_expired_bundle(&db, tenant_a, -time::Duration::seconds(1)).await;
    seed_expired_bundle(&db, tenant_b, -time::Duration::seconds(1)).await;
    seed_expired_bundle(&db, tenant_live_only, time::Duration::hours(1)).await;

    let services = build_services_tenant_scoped(db.clone());

    let tenants = services
        .bundles
        .tenants_with_expired_bundles(&system_actor::for_bundle_gc())
        .await
        .unwrap();
    assert_eq!(
        tenants,
        vec![tenant_a, tenant_b],
        "both expired tenants, ascending (0x0A < 0xB0), the live-only tenant absent"
    );

    for tenant in &tenants {
        let purged = services.bundles.purge_expired(&ctx(*tenant)).await.unwrap();
        assert_eq!(purged, 1, "each tenant purges exactly its own row");
    }

    // Ground truth: no expired descriptor survives, and the live-only
    // tenant's bundle was never touched.
    let remaining = services
        .bundles
        .tenants_with_expired_bundles(&system_actor::for_bundle_gc())
        .await
        .unwrap();
    assert!(
        remaining.is_empty(),
        "every enumerated tenant was purged; got {remaining:?}"
    );
}

/// `list_universe` is a **cross-gear read**: qa-insights calls it over the SDK
/// and gets back plan names, test-file paths and `TEST_META` attributes for a
/// whole product. That makes it the widest read on this trait, so the tenancy
/// proof has to be over *real content*, not over an empty result.
///
/// So tenant A gets a genuinely non-empty universe here — a synced repository
/// with a plan and a test file on disk — and tenant B, asking for the same
/// product id, gets nothing. Without the content the assertion would pass
/// vacuously for a `list_universe` that ignored scoping entirely.
///
/// The boundary itself is the `TEST_REPO`/`LIST` `AccessScope` the walk
/// before enumerating repositories: a repository outside the caller's tenant
/// never reaches the filesystem walk, so no plan or test path can leak.
#[tokio::test]
async fn universe_reads_are_scoped_to_the_callers_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let repos_dir = tempfile::tempdir().unwrap();
    let services = build_services_tenant_scoped_at(db, repos_dir.path().to_path_buf());

    let product_id = seed_product(&services, &ctx(tenant_a), "product-a").await;
    let repo = services
        .repos
        .create_repo(&ctx(tenant_a), new_repo("repo-a", product_id))
        .await
        .unwrap();

    // Materialize the branch snapshot, then fill it with content the walk can
    // actually find.
    services
        .repos
        .sync_repo(&ctx(tenant_a), repo.id, "main", true)
        .await
        .unwrap();
    let workdir = crate::infra::git::layout::branch_workdir(repos_dir.path(), repo.id, "main");
    std::fs::create_dir_all(workdir.join("tests")).unwrap();
    std::fs::create_dir_all(workdir.join("infra")).unwrap();
    std::fs::write(
        workdir.join("infra/plan.yaml"),
        "name: secret infra suite\ntests:\n  - tests/test_secret.py\n",
    )
    .unwrap();
    std::fs::write(
        workdir.join("tests/test_secret.py"),
        "TEST_META = {'title': 'Secret failover'}\n\ndef test_secret():\n    pass\n",
    )
    .unwrap();

    let own = services
        .plans
        .list_universe(&ctx(tenant_a), Some(product_id), Some("main"))
        .await
        .unwrap();
    assert_eq!(
        own.len(),
        1,
        "the owning tenant must see its own universe, or the isolation \
         assertion below would pass vacuously; got {own:?}"
    );
    assert_eq!(own[0].test_name, "Secret failover");

    let foreign = services
        .plans
        .list_universe(&ctx(tenant_b), Some(product_id), Some("main"))
        .await
        .unwrap();
    assert!(
        foreign.is_empty(),
        "a foreign tenant must not see another tenant's plans or test files; got {foreign:?}"
    );

    // Nor by asking for every product at once — the product filter is not the
    // boundary, the repository scope is.
    let foreign_unfiltered = services
        .plans
        .list_universe(&ctx(tenant_b), None, Some("main"))
        .await
        .unwrap();
    assert!(
        foreign_unfiltered.is_empty(),
        "an unfiltered universe must still be tenant-scoped; got {foreign_unfiltered:?}"
    );
}

// ---------------------------------------------------------------------------
// SSH credential resolution is tenant-scoped (real DB, real ORM repository)
// ---------------------------------------------------------------------------

const SSH_PEM: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nkeybody\n-----END OPENSSH PRIVATE KEY-----\n";

fn ssh_repo(name: &str, product_id: Uuid, credential_ref: &str) -> NewTestRepository {
    NewTestRepository {
        product_id,
        name: name.to_owned(),
        url: "git@bitbucket.org:virtuozzocore/vhp-core.git".to_owned(),
        default_branch: "main".to_owned(),
        content_root: String::new(),
        credential_ref: Some(credential_ref.to_owned()),
    }
}

/// **An SSH `credential_ref` must not resolve across tenants.**
///
/// For an ssh remote `credential_ref` names a `qa_ssh_keys` row, so
/// `ReposService::resolve_credential` performs a row lookup before reading
/// credstore. That lookup is the new place tenant isolation has to hold, and
/// this exercises it through the real stack — `PolicyEnforcer` →
/// `.secure().scope_with(scope)` → `SQLite` — rather than through a mock
/// repository that ignores its `scope` argument.
///
/// The credstore double here resolves **any** reference to key material. That
/// is deliberate: it removes the second line of defence, so a `find_by_id`
/// that dropped `.scope_with(scope)` would make tenant A's sync *succeed*
/// with tenant B's key, and this test fails loudly instead of passing for the
/// wrong reason.
#[tokio::test]
async fn ssh_credential_ref_does_not_resolve_across_tenants() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let services = build_services_tenant_scoped_with_credstore(
        db,
        Arc::new(
            credstore_sdk::test_util::MockCredStoreClient::returning_raw_value(
                SSH_PEM.as_bytes().to_vec(),
            ),
        ),
    );

    // Tenant B owns the key.
    let key_b = services
        .ssh_keys
        .create_ssh_key(&ctx(tenant_b), "b-deploy".to_owned(), SSH_PEM.to_owned())
        .await
        .unwrap();

    // Tenant A registers a repository pointing at B's key id.
    let product_a = seed_product(&services, &ctx(tenant_a), "product-a").await;
    let repo_a = services
        .repos
        .create_repo(
            &ctx(tenant_a),
            ssh_repo("repo-a", product_a, &key_b.id.to_string()),
        )
        .await
        .unwrap();

    let synced = services
        .repos
        .sync_repo(&ctx(tenant_a), repo_a.id, "", true)
        .await
        .unwrap();

    let sync_error = synced
        .sync_error
        .expect("tenant A must not resolve tenant B's ssh key");
    assert!(
        sync_error.contains("is not registered"),
        "expected the cross-tenant lookup to fail closed, got: {sync_error}"
    );
    assert!(
        !sync_error.contains("keybody"),
        "no key material may appear in a persisted sync error: {sync_error}"
    );
}

/// The positive control for the test above: **the same arrangement inside one
/// tenant succeeds.** Without this, a `find_by_id` that returned `None`
/// unconditionally would satisfy the cross-tenant assertion while breaking
/// the feature entirely.
#[tokio::test]
async fn ssh_credential_ref_resolves_within_the_owning_tenant() {
    let db = inmem_db().await;
    let tenant_a = Uuid::new_v4();
    let services = build_services_tenant_scoped_with_credstore(
        db,
        Arc::new(
            credstore_sdk::test_util::MockCredStoreClient::returning_raw_value(
                SSH_PEM.as_bytes().to_vec(),
            ),
        ),
    );

    let key_a = services
        .ssh_keys
        .create_ssh_key(&ctx(tenant_a), "a-deploy".to_owned(), SSH_PEM.to_owned())
        .await
        .unwrap();

    let product_a = seed_product(&services, &ctx(tenant_a), "product-a").await;
    let repo_a = services
        .repos
        .create_repo(
            &ctx(tenant_a),
            ssh_repo("repo-a", product_a, &key_a.id.to_string()),
        )
        .await
        .unwrap();

    let synced = services
        .repos
        .sync_repo(&ctx(tenant_a), repo_a.id, "", true)
        .await
        .unwrap();

    assert_eq!(
        synced.sync_error, None,
        "a repository referencing its OWN tenant's ssh key must sync"
    );
}

/// The plugin binding survives an ordinary product edit — against the REAL
/// `SeaORM` repository, which is where the rule lives.
///
/// # Why this is here and not in `products_tests`
///
/// That module's `MockProductsRepository` implements the same rule, so a test
/// there proves only that the mock agrees with itself. `ProductUpdate`'s
/// `plugin_instance_id` is the one non-full-replace field on the struct and
/// the rule is one `match` arm in `OrmProductsRepository::update`; only a
/// DB-backed test can hold it.
///
/// # The failure it guards
///
/// Measured on the shipped UI: `productReqFromForm` sends
/// `{name, key, description, folder}` and cannot send `plugin_instance_id` —
/// it is generated against `docs/api/api.json`, which has no `/qa/v1` path.
/// Under the original full-replace semantics an operator editing a product's
/// description silently unbound its plugin, which stops every one of that
/// product's environments being observed and every one of its runs being
/// dispatched, and recovery meant re-entering a ~100-character GTS id no UI
/// surface offers. Unbinding is also not a state this platform wants: spec
/// decision D6 is "every product names a plugin", and the contract migration
/// makes the column `NOT NULL`.
#[tokio::test]
async fn an_update_that_names_no_plugin_leaves_the_products_binding_alone() {
    let db = inmem_db().await;
    let services = build_services_tenant_scoped(db);
    let tenant = Uuid::new_v4();
    let caller = ctx(tenant);

    // The id the `m20260903_000003_product_plugin_instance` migration's
    // backfill used to bind every existing product to, back when this
    // gear's schema still had a backfill step. Written out rather than
    // recomposed from `QaProductPluginSpecV1::TYPE_ID`, because this test is
    // about the update path, not about the id's construction. (That
    // migration, and the test that verified the id's composition, were
    // both folded into `migrations::m20260812_000002_initial` by the docs
    // squash; the fold replaced the backfill with a straight `NOT NULL`
    // column, so nothing here still needs to stay in sync with it.)
    let bound =
        "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1"
            .to_owned();
    let product = services
        .products
        .create_product(
            &caller,
            qa_catalog_sdk::NewProduct {
                name: "vhp".to_owned(),
                key: "VHP".to_owned(),
                description: "before".to_owned(),
                folder: Some("virt".to_owned()),
                plugin_instance_id: bound.clone(),
            },
        )
        .await
        .expect("the fixture product must be created");
    assert_eq!(product.plugin_instance_id, bound);

    // Exactly what the UI sends: every field it has, and no binding.
    let edited = services
        .products
        .update_product(
            &caller,
            product.id,
            qa_catalog_sdk::ProductUpdate {
                name: "vhp".to_owned(),
                key: "VHP".to_owned(),
                description: "after".to_owned(),
                folder: Some("virt".to_owned()),
                plugin_instance_id: None,
            },
        )
        .await
        .expect("the edit must succeed");

    assert_eq!(edited.description, "after", "the edit itself must land");
    assert_eq!(
        edited.plugin_instance_id, bound,
        "and the binding must survive an update that did not name it"
    );

    // Re-read from the row, because the returned value could have been
    // assembled in memory rather than read back.
    let reread = services
        .products
        .list_products(&caller)
        .await
        .expect("the products must be listable")
        .into_iter()
        .find(|candidate| candidate.id == product.id)
        .expect("the product must still exist");
    assert_eq!(reread.plugin_instance_id, bound);
    assert_eq!(reread.description, "after");
}

/// The `Some` arm of the same match: an update that **does** name a plugin
/// rebinds the product.
///
/// # Why this exists (Phase E review, finding FW-2)
///
/// `an_update_that_names_no_plugin_leaves_the_products_binding_alone` above
/// covers the `None` arm against the real repository. The `Some` arm had no
/// DB-backed coverage at all: the only rebind assertion ran against
/// `MockProductsRepository`, which asserts a hand-written copy of the rule
/// rather than the rule. The reviewer measured the consequence — mutate
/// `products_sea_repo.rs`' `Some(instance_id) => ActiveValue::Set(..)` to
/// `Unchanged`, making the binding **permanently immutable once set**, and the
/// whole `qa-catalog` suite stayed green.
///
/// So this asserts a rebind lands, against real migrations and the real
/// `OrmProductsRepository`, and re-reads the row rather than trusting the
/// returned value — which could have been assembled in memory.
///
/// The two ids differ only in their final segment, because that is what a real
/// rebind looks like: the same plugin *kind*, a different instance.
#[tokio::test]
async fn an_update_that_names_a_plugin_rebinds_the_product() {
    let db = inmem_db().await;
    let services = build_services_tenant_scoped(db);
    let tenant = Uuid::new_v4();
    let caller = ctx(tenant);

    let bound =
        "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1"
            .to_owned();
    // Both ids must be REGISTERED since Task 20: a rebind target that resolves
    // to nothing is refused (ruling F-10), so an invented string would make
    // this test assert the refusal instead of the rebind.
    let rebound = crate::test_support::FIXTURE_PLUGIN_INSTANCE_ID_B.to_owned();

    let product = services
        .products
        .create_product(
            &caller,
            qa_catalog_sdk::NewProduct {
                name: "vhp".to_owned(),
                key: "VHP".to_owned(),
                description: "before".to_owned(),
                folder: Some("virt".to_owned()),
                plugin_instance_id: bound.clone(),
            },
        )
        .await
        .expect("the fixture product must be created");
    assert_eq!(product.plugin_instance_id, bound);

    let edited = services
        .products
        .update_product(
            &caller,
            product.id,
            qa_catalog_sdk::ProductUpdate {
                name: "vhp".to_owned(),
                key: "VHP".to_owned(),
                description: "before".to_owned(),
                folder: Some("virt".to_owned()),
                plugin_instance_id: Some(rebound.clone()),
            },
        )
        .await
        .expect("the rebind must succeed");

    assert_eq!(
        edited.plugin_instance_id, rebound,
        "a named plugin must REPLACE the stored binding, not be ignored -- \
         making this column immutable once set left the suite green (FW-2)"
    );

    let reread = services
        .products
        .list_products(&caller)
        .await
        .expect("the products must be listable")
        .into_iter()
        .find(|candidate| candidate.id == product.id)
        .expect("the product must still exist");
    assert_eq!(
        reread.plugin_instance_id, rebound,
        "and the ROW must carry it, not just the returned value"
    );
}
