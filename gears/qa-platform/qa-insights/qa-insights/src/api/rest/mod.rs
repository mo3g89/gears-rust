//! REST transport layer: DTOs, canonical error mapping, handlers, and routes.
//!
//! ## Layering
//!
//! - [`dto`] — wire types (`serde` + `utoipa`). Never used by the SDK or the
//!   domain layer; `qa-insights-sdk` is deliberately `serde`-free, so the
//!   conversions here are the only bridge.
//! - [`error`] — `From<DomainError> for CanonicalError`, the single place that
//!   decides HTTP-visible error shape.
//! - [`handlers`] — thin request/response glue. No business logic, no database,
//!   no PEP.
//! - [`routes`] — `OperationBuilder` registrations under `/qa/v1/...`.
//!
//! Shape copied from `qa-environments/src/api/rest/mod.rs`, which is the
//! smallest of the three shipped siblings and therefore the clearest template.
//!
//! ## What is here at HEAD (Task 38)
//!
//! **Twenty-three paths in eight route modules** — regenerated from
//! `routes::mod::tests::the_openapi_document_lists_every_registered_path`'s
//! `PATHS` array (the same array a test ties to the live `OpenAPI` document's
//! path count, so it cannot go stale the way this paragraph itself once did)
//! and from which [`routes`] module registers which path. Corrected in the
//! Phase B fix wave (Finding 3): this used to say "Seven endpoints in four
//! modules" under a "What is here at Task 25b" heading — four tasks stale —
//! and named Task 27 as "the saved-view surfaces", which is Task 28's; Task
//! 27 is the plan drill-downs.
//!
//! * `admin` — one path: `POST /qa/v1/insights/rebuild` (Task 16,
//!   [`handlers::rebuild`]).
//! * `collections` — two paths, the flat `OData` collections `GET
//!   /qa/v1/test-results` and `GET /qa/v1/test-case-results` (Task 17).
//! * `dashboard` — two paths: `GET /qa/v1/dashboard` (Task 18,
//!   [`handlers::dashboard`]), the first endpoint here that answers with an
//!   aggregate rather than with rows, and `GET /qa/v1/dashboard/coverage`
//!   (Task 19, [`handlers::dashboard_coverage`]).
//! * `analytics` — six paths: `GET /qa/v1/analytics/overview` and `GET
//!   /qa/v1/analytics/build-tests` (Task 25b, the latter's fold is Task 24's;
//!   controller ruling R10 moved its route here since everything upstream of
//!   it shipped with Task 25), `GET /qa/v1/analytics/export` (Task 26), and
//!   the three plan drill-downs `GET /qa/v1/analytics/plan/tests`, `GET
//!   /qa/v1/analytics/plan/builds` and `GET /qa/v1/analytics/plan/test-history`
//!   (Task 27).
//! * `saved_views` — two paths, four operations: `GET`/`POST
//!   /qa/v1/analytics/views` and `PUT`/`DELETE /qa/v1/analytics/views/{id}`
//!   (Task 28, this tier's first write path).
//! * `collect` — two paths: `POST /qa/v1/analytics/collect`, the authenticated
//!   trigger, and `POST /qa/v1/collect/{repo_id}`, the one anonymous,
//!   HMAC-verified route on this list (Task 30).
//! * `settings` — four paths, ten operations: `GET`/`PUT /qa/v1/settings/jira`
//!   (Task 32), `GET`/`PUT /qa/v1/settings/jira-poller` (Task 35), and
//!   `GET`/`PUT /qa/v1/settings/notifications` plus its `/test`, `/preview`
//!   and `/log` siblings (Task 38). The seventh module, and the first added
//!   in Phase C. Controller ruling R69 founded it here rather than at Task
//!   38, which is where the plan listed the files.
//! * `jira` — two paths: `GET /qa/v1/jira/open-bugs` and
//!   `POST /qa/v1/jira/bugs` (Task 33). The eighth module, added beside the
//!   seven above rather than folded into `settings` — that module's own name
//!   is the registry `qa_jira_bugs`, not the connection settings `settings`
//!   already owns, and `handlers::jira`'s header says why the two collide
//!   just enough to be worth keeping apart.
//!
//! **This paragraph named Task 38 as the other task expected to add a module
//! beside these, and that was already wrong when Task 33 landed**: Task 38's
//! notification-settings routes land in the `settings` module Task 32 founded,
//! not in one of their own — and now they have, above.
//!
//! **The analytics endpoints are the first here whose DTOs are not one-to-one
//! with a contract type.** `qa-insights-sdk` carries no analytics models —
//! nothing cross-gear reads them — so [`dto`]'s analytics half converts from
//! `domain::analytics::aggregates` and `domain::service::analytics` directly, and
//! it is where three of legacy's labels become ids. That module's header names
//! all three.
//!
//! **`OData` is here and only here.** Per D7 it applies to the two flat result
//! collections, which are the tables `cpt-cf-qa-nfr-scale`'s 5M-row target is
//! about. **Every later endpoint on this list answers with an aggregate or a
//! read bounded by its time window, not by a page size** — corrected in the
//! Phase B fix wave (Finding 2; a prior revision of this paragraph claimed
//! "a bounded read" without qualifying what bounds it). `GET
//! /qa/v1/analytics/plan/test-history` returns every run of every test in a
//! plan over its 90-day window, and `lists`, `build-tests`, `plan/tests`,
//! `plan/builds` and the export are likewise unbounded arrays over their own
//! read window — none of them acquires a `$filter`, and none is paged.
//! Whether and how to bound them is a product/NFR decision under
//! `cpt-cf-qa-nfr-scale`, escalated to a human rather than decided here; this
//! wave does **not** add pagination. See [`crate::config`]'s
//! `QaInsightsConfig::max_page_size` doc for the forecast that once promised
//! Tasks 24-27 would consume it — they have all shipped and none does.

pub mod dto;
pub mod error;
pub mod handlers;
pub mod routes;
