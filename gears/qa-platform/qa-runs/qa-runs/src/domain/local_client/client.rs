//! Local adapter: `QaRunsClientV1` over `AppServices`.
//!
//! `DomainError` becomes `QaRunsError` (which *is* `CanonicalError`) through
//! the `From` impl in `domain::error`, so an in-process caller and an HTTP
//! caller get the same status and the same redaction - the disclosure rule is
//! applied once, at one seam, rather than twice.
//!
//! # Status and redaction come from that impl; **attribution does not**
//!
//! Corrected 2026-08-17, having shipped as an unqualified "the same status and
//! the same redaction" that was read as covering the whole body. It does not:
//! `DomainError::Validation` and `DomainError::Forbidden` name no resource, so
//! the exhaustive `match` attributes both to `cf.qa.runs.run.v1~` and the
//! *caller's* surface is what has to say otherwise. `api::rest::handlers` does
//! that with `as_schedule_error` and `as_queue_error`; this adapter is a third
//! surface with the same obligation, and until Task 20's review it discharged
//! none of it - five `.map_err(Into::into)`s handing a schedule's empty-name
//! refusal to an in-process caller as a *run*'s field violation.
//!
//! This is the same seam and the same omission as the commit that moved a name
//! trim out of the DTO layer *because the local client never passes through a
//! DTO*. The trim moved; the mapping did not.

use std::sync::Arc;

use async_trait::async_trait;
use qa_runs_sdk::{
    LaunchOutcome, LaunchRequest, NewSchedule, QaRunsClientV1, QaRunsError, QueueEntry, Run,
    RunResult, RunTestResult, Schedule, ScheduleNotificationSettings,
};
use time::OffsetDateTime;
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error_attribution::{as_queue_error, as_schedule_error};
use crate::gear::ConcreteAppServices;

/// Local implementation of the object-safe `QaRunsClientV1`.
pub struct QaRunsLocalClient {
    services: Arc<ConcreteAppServices>,
}

impl QaRunsLocalClient {
    #[must_use]
    pub(crate) fn new(services: Arc<ConcreteAppServices>) -> Self {
        Self { services }
    }
}

/// The SDK's `limit` as an `OData` page size.
///
/// The contract makes `limit` **mandatory** on both list methods - `qa_runs`
/// grows strictly faster than the queue and never drains, so an unbounded
/// inter-gear call would materialize every run ever executed. The repository
/// clamps it again through `LimitCfg`, so a caller asking for a million gets a
/// page, not a heap.
fn page_of(limit: u32) -> ODataQuery {
    ODataQuery::new().with_limit(u64::from(limit))
}

#[async_trait]
impl QaRunsClientV1 for QaRunsLocalClient {
    // ==================== Runs ====================

    async fn launch(
        &self,
        ctx: &SecurityContext,
        req: LaunchRequest,
    ) -> Result<LaunchOutcome, QaRunsError> {
        self.services
            .launch
            .launch(ctx, req)
            .await
            .map_err(Into::into)
    }

    async fn get_run(&self, ctx: &SecurityContext, id: Uuid) -> Result<Run, QaRunsError> {
        self.services.runs.get(ctx, id).await.map_err(Into::into)
    }

    async fn list_runs(&self, ctx: &SecurityContext, limit: u32) -> Result<Vec<Run>, QaRunsError> {
        // `RunsService::list` returns `RunWithResult` since Task 10 - the run
        // counters the REST run list needs - but the `QaRunsClientV1` contract
        // is `Vec<Run>` and is not this task's to widen: it is consumed by
        // other gears in-process, unlike the REST DTO. Unwrap back to `Run`.
        Ok(self
            .services
            .runs
            .list(ctx, &page_of(limit))
            .await
            .map_err(QaRunsError::from)?
            .items
            .into_iter()
            .map(|item| item.run)
            .collect())
    }

    async fn get_run_result(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<RunResult, QaRunsError> {
        self.services
            .runs
            .get_result(ctx, id)
            .await
            .map_err(Into::into)
    }

    async fn list_runs_finished_since(
        &self,
        ctx: &SecurityContext,
        since: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Run>, QaRunsError> {
        // Not `page_of(limit)`: this is not an `OData` collection read. The
        // sweep's ordering is `(finished_at, id)` ascending and is part of the
        // contract, so it is expressed in the repository query rather than in a
        // cursor a caller could re-sort. The repository clamps `limit` itself.
        self.services
            .runs
            .list_runs_finished_since(ctx, since, limit)
            .await
            .map_err(Into::into)
    }

    async fn list_run_test_results(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Vec<RunTestResult>, QaRunsError> {
        // `test_results` resolves the run under the caller's own scope first,
        // so an unknown run and another tenant's run are the same `not_found`
        // here as everywhere else in this gear.
        Ok(self
            .services
            .runs
            .test_results(ctx, run_id)
            .await
            .map_err(QaRunsError::from)?
            .into_iter()
            .map(RunTestResult::from)
            .collect())
    }

    async fn cancel_run(&self, ctx: &SecurityContext, id: Uuid) -> Result<Run, QaRunsError> {
        self.services.runs.cancel(ctx, id).await.map_err(Into::into)
    }

    async fn rerun(&self, ctx: &SecurityContext, id: Uuid) -> Result<LaunchOutcome, QaRunsError> {
        self.services.runs.rerun(ctx, id).await.map_err(Into::into)
    }

    // ==================== Queue ====================

    async fn list_queue(
        &self,
        ctx: &SecurityContext,
        platform_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<QueueEntry>, QaRunsError> {
        Ok(self
            .services
            .runs
            .queue_page(ctx, platform_id, &page_of(limit))
            .await
            .map_err(|e| as_queue_error(None, e))?
            .items)
    }

    async fn cancel_queued(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
    ) -> Result<(), QaRunsError> {
        self.services
            .runs
            .cancel_queued(ctx, queue_id)
            .await
            .map_err(|e| as_queue_error(Some(queue_id), e))
    }

    /// **Returns the started run, which costs one extra read.**
    ///
    /// `RunsService::force_start` answers the run *id* - the REST endpoint
    /// renders exactly that - but this contract promises a `Run`, so the id is
    /// resolved back through the caller's own scope. Not a formality: the read
    /// is scoped, so a caller who could force-start a row but cannot read its
    /// run gets a not-found here rather than a record they are not entitled to.
    async fn force_start_queued(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
    ) -> Result<Run, QaRunsError> {
        let run_id = self
            .services
            .runs
            .force_start(ctx, queue_id)
            .await
            .map_err(|e| as_queue_error(Some(queue_id), e))?;
        // The read that follows is a **run** read under the caller's own run
        // scope, so its refusals stay run-attributed - the resource the caller
        // is being refused here really is the run, not the row they named.
        self.services
            .runs
            .get(ctx, run_id)
            .await
            .map_err(Into::into)
    }

    // ==================== Schedules ====================
    //
    // Delegation plus `as_schedule_error`, and the reason they are only that is
    // worth one note rather than five: `ScheduleService` performs its own PEP
    // resolve for every repository call, so an in-process caller is scoped by
    // the same `AccessScope` an HTTP caller is, and there is nothing left for
    // this adapter to *check*. What it still owes is attribution, which is what
    // the wrapper is - see this module's header. Where the runs side does more
    // than delegate, it is always because the contract's shape differs from the
    // service's - `force_start_queued` resolves an id back to a `Run`, and the
    // two list methods unwrap a `Page` into a `Vec`. None of the schedule
    // methods have either mismatch.
    //
    // **The wire vocabulary does not appear here.** `exclusive_choice` crosses
    // this seam as the `Option<bool>` the contract declares; the three-token
    // spelling belongs to `api::rest::dto` and to the column, and an in-process
    // caller has no wire to encode for.

    async fn list_schedules(&self, ctx: &SecurityContext) -> Result<Vec<Schedule>, QaRunsError> {
        self.services
            .schedules
            .list(ctx)
            .await
            .map_err(as_schedule_error)
    }

    async fn get_schedule(&self, ctx: &SecurityContext, id: Uuid) -> Result<Schedule, QaRunsError> {
        self.services
            .schedules
            .get(ctx, id)
            .await
            .map_err(as_schedule_error)
    }

    async fn create_schedule(
        &self,
        ctx: &SecurityContext,
        new: NewSchedule,
    ) -> Result<Schedule, QaRunsError> {
        self.services
            .schedules
            .create(ctx, new)
            .await
            .map_err(as_schedule_error)
    }

    /// **A full replace, matching the REST `PUT`.** Every caller-decidable
    /// field comes from `new`, `enabled` included - the contract makes it a
    /// plain `bool`, so an in-process caller cannot omit it any more than an
    /// HTTP one can. `last_fired_tick` is untouched.
    async fn update_schedule(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        new: NewSchedule,
    ) -> Result<Schedule, QaRunsError> {
        self.services
            .schedules
            .update(ctx, id, new)
            .await
            .map_err(as_schedule_error)
    }

    async fn delete_schedule(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaRunsError> {
        self.services
            .schedules
            .delete(ctx, id)
            .await
            .map_err(as_schedule_error)
    }

    /// **Three fields, and nothing else** - `update_schedule` above is the full
    /// replace, and the two do not overlap: a replace leaves these settings
    /// standing, and this leaves everything else standing. `schedule_columns`
    /// records why that split is a property of the `SET` list rather than of the
    /// caller.
    ///
    /// This is the seam D9 exists for: qa-insights reads the settings back off
    /// `Schedule` when a scheduled run changes status (Task 36).
    async fn update_schedule_notifications(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        settings: ScheduleNotificationSettings,
    ) -> Result<Schedule, QaRunsError> {
        self.services
            .schedules
            .update_notifications(ctx, id, settings)
            .await
            .map_err(as_schedule_error)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! The third attribution surface, driven over the same `Fleet` the handler
    //! suites use.
    //!
    //! `api::rest::handlers` has one of these per endpoint. This adapter is the
    //! *other* way into the same services, and it had no such check at all -
    //! which is how five `.map_err(Into::into)`s shipped handing an in-process
    //! caller `cf.qa.runs.run.v1~` for a schedule's field and for a schedule
    //! denial.

    use std::sync::Arc;

    use qa_runs_sdk::{NewSchedule, QaRunsClientV1, RunTarget};
    use toolkit::api::canonical_prelude::Problem;
    use uuid::Uuid;

    use super::QaRunsLocalClient;
    use crate::domain::service::test_support::{Fleet, ctx};

    const TENANT: Uuid = Uuid::from_u128(0x0A11_0000_0000_0001);
    const SCHEDULE_GTS: &str = "cf.qa.runs.schedule.v1~";
    const RUN_GTS: &str = "cf.qa.runs.run.v1~";

    fn payload(name: &str) -> NewSchedule {
        NewSchedule {
            name: name.to_owned(),
            target: RunTarget::Plan {
                repo_id: Uuid::from_u128(0x0B01),
                path: "plans/smoke.yaml".to_owned(),
            },
            platform_id: None,
            branch: Some("main".to_owned()),
            cron: "0 3 * * *".to_owned(),
            exclusive_choice: None,
            enabled: true,
            include_tags: vec![],
            exclude_tags: vec![],
            parameters: vec![],
        }
    }

    fn wire(error: &qa_runs_sdk::QaRunsError) -> (u16, String) {
        let problem = Problem::from_error(error).expect("a problem must serialize");
        let status = problem.status;
        (
            status,
            serde_json::to_string(&problem).expect("a problem must serialize"),
        )
    }

    /// **An in-process caller's schedule refusal names the schedule.**
    ///
    /// `ScheduleService::validate`'s empty-name refusal is a
    /// `DomainError::Validation { field: "name" }`, which names no resource -
    /// so without the wrapper the exhaustive `match` calls it a *run*'s field.
    /// Break-verified: reverting `create_schedule` to `.map_err(Into::into)`
    /// turns this red with `cf.qa.runs.run.v1~` on a schedule's `name`, and
    /// leaves every other test in the crate green.
    #[tokio::test]
    async fn a_field_violation_from_an_in_process_caller_names_the_schedule() {
        let fleet = Fleet::new().await;
        let client = QaRunsLocalClient::new(fleet.instance());

        let error = client
            .create_schedule(&ctx(TENANT), payload("   "))
            .await
            .expect_err("an empty name must be refused");

        let (status, body) = wire(&error);
        assert_eq!(status, 400, "{body}");
        assert!(body.contains(SCHEDULE_GTS), "{body}");
        assert!(!body.contains(RUN_GTS), "{body}");
        assert!(body.contains("name"), "{body}");
    }

    /// And a denial, on every one of the six schedule methods.
    ///
    /// Same reasoning as `handlers::schedules`'s `handler_tests`'
    /// `every_handler_attributes_a_denial_to_the_schedule`: a 403 carries no
    /// field, so the resource type is all an operator has, and a fresh
    /// deployment whose policy has not been taught `qa.schedule` meets this on
    /// every call. Dropping the wrapper from any single method turns this red
    /// for that method alone.
    #[tokio::test]
    async fn every_schedule_method_attributes_a_denial_to_the_schedule() {
        let fleet = Fleet::denying().await;
        let client = Arc::new(QaRunsLocalClient::new(fleet.instance()));
        let id = Uuid::from_u128(0x51);

        let mut refusals: Vec<(&str, qa_runs_sdk::QaRunsError)> = Vec::new();
        refusals.push((
            "list",
            client.list_schedules(&ctx(TENANT)).await.err().unwrap(),
        ));
        refusals.push((
            "get",
            client.get_schedule(&ctx(TENANT), id).await.err().unwrap(),
        ));
        refusals.push((
            "create",
            client
                .create_schedule(&ctx(TENANT), payload("nightly"))
                .await
                .err()
                .unwrap(),
        ));
        refusals.push((
            "update",
            client
                .update_schedule(&ctx(TENANT), id, payload("nightly"))
                .await
                .err()
                .unwrap(),
        ));
        refusals.push((
            "delete",
            client
                .delete_schedule(&ctx(TENANT), id)
                .await
                .err()
                .unwrap(),
        ));
        // The sixth, added with the method itself rather than after it: a
        // schedule method that reached this seam without attribution would tell
        // a caller their *run* permissions were the problem.
        refusals.push((
            "update_notifications",
            client
                .update_schedule_notifications(
                    &ctx(TENANT),
                    id,
                    qa_runs_sdk::ScheduleNotificationSettings::default(),
                )
                .await
                .err()
                .unwrap(),
        ));

        for (method, error) in refusals {
            let (status, body) = wire(&error);
            assert_eq!(status, 403, "{method}: {body}");
            assert!(
                body.contains(SCHEDULE_GTS),
                "{method}: a schedule denial must name the schedule: {body}"
            );
            assert!(
                !body.contains(RUN_GTS),
                "{method}: and must not point at run permissions: {body}"
            );
        }
    }

    /// The queue side of the same seam: `cancel_queued` and
    /// `force_start_queued` address a **row**, so their denials must too.
    #[tokio::test]
    async fn the_queue_methods_attribute_a_denial_to_the_queue_entry() {
        let fleet = Fleet::denying().await;
        let client = QaRunsLocalClient::new(fleet.instance());
        let id = Uuid::from_u128(0x0E01);

        for (method, error) in [
            (
                "cancel_queued",
                client.cancel_queued(&ctx(TENANT), id).await.err().unwrap(),
            ),
            (
                "force_start_queued",
                client
                    .force_start_queued(&ctx(TENANT), id)
                    .await
                    .err()
                    .unwrap(),
            ),
            (
                "list_queue",
                client
                    .list_queue(&ctx(TENANT), None, 10)
                    .await
                    .err()
                    .unwrap(),
            ),
        ] {
            let (status, body) = wire(&error);
            assert_eq!(status, 403, "{method}: {body}");
            assert!(
                body.contains("cf.qa.runs.queue_entry.v1~"),
                "{method}: {body}"
            );
            assert!(!body.contains(RUN_GTS), "{method}: {body}");
        }
    }
}
