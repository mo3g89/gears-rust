//! [`RunnerPodResolver`] over qa-runs' `QaRunsClientV1::resolve_runner_pod`.
//!
//! **Looked up on every call, not at init.** qa-runs declares
//! `deps = [qa_environments]`, so qa-runs' client is not in the hub while this
//! gear initialises, and declaring the reverse dependency would make a cycle.
//! This is the same shape as `HubProductPluginResolver`.

use std::sync::Arc;

use async_trait::async_trait;
use qa_runs_sdk::{QaRunsClientV1, QaRunsError, RunnerPodQuery};
use toolkit::client_hub::ClientHub;
use uuid::Uuid;

use crate::domain::ports::{RunnerPodRefusal, RunnerPodResolver, VerifiedRunnerPod};

/// [`RunnerPodResolver`] that asks whichever qa-runs client the hub holds.
pub struct HubRunnerPodResolver {
    client_hub: Arc<ClientHub>,
}

impl HubRunnerPodResolver {
    #[must_use]
    pub const fn new(client_hub: Arc<ClientHub>) -> Self {
        Self { client_hub }
    }
}

#[async_trait]
impl RunnerPodResolver for HubRunnerPodResolver {
    async fn environment_of(&self, pod: &VerifiedRunnerPod) -> Result<Uuid, RunnerPodRefusal> {
        let runs = self
            .client_hub
            .get::<dyn QaRunsClientV1>()
            .map_err(|_| RunnerPodRefusal::Unavailable("qa-runs is not registered"))?;
        let query = RunnerPodQuery {
            tenant_id: pod.tenant_id,
            run_id: pod.run_id,
            workflow_name: pod.workflow_name.clone(),
        };
        match runs.resolve_runner_pod(&query).await {
            Ok(run) => Ok(run.environment_id),
            // qa-runs answers every refusal as `not_found`, deliberately
            // indistinguishable; anything else means it could not answer.
            Err(QaRunsError::NotFound { .. }) => Err(RunnerPodRefusal::NotFound),
            Err(_) => Err(RunnerPodRefusal::Unavailable(
                "qa-runs could not resolve the pod",
            )),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use qa_runs_sdk::{
        FinishedRunCursor, LaunchOutcome, LaunchRequest, NewSchedule, QaRunsClientV1, QaRunsError,
        QueueEntry, Run, RunResult, RunTestResult, RunnerPodQuery, RunnerPodRun, Schedule,
        ScheduleNotificationSettings, ScheduleTick,
    };
    use toolkit::api::canonical_prelude::*;
    use toolkit::client_hub::ClientHub;
    use toolkit_security::SecurityContext;
    use uuid::Uuid;

    use super::HubRunnerPodResolver;
    use crate::domain::ports::{RunnerPodRefusal, RunnerPodResolver};
    use crate::test_support::a_runner_pod;

    /// qa-runs' run resource, spelled as qa-runs spells it, so the fixture
    /// comes out of the builder chain the real client's errors do.
    #[resource_error(gts_id!("cf.qa.runs.run.v1~"))]
    struct FarSide;

    /// A qa-runs client answering `resolve_runner_pod` with one scripted
    /// result and recording the queries it was asked. Every other method is
    /// outside this adapter's contract.
    struct ScriptedRuns {
        answer: fn() -> Result<RunnerPodRun, QaRunsError>,
        asked: Mutex<Vec<RunnerPodQuery>>,
    }

    macro_rules! not_this_adapters {
        () => {
            unimplemented!("HubRunnerPodResolver calls resolve_runner_pod only")
        };
    }

    #[async_trait]
    impl QaRunsClientV1 for ScriptedRuns {
        async fn launch(
            &self,
            _: &SecurityContext,
            _: LaunchRequest,
        ) -> Result<LaunchOutcome, QaRunsError> {
            not_this_adapters!()
        }
        async fn get_run(&self, _: &SecurityContext, _: Uuid) -> Result<Run, QaRunsError> {
            not_this_adapters!()
        }
        async fn list_runs(&self, _: &SecurityContext, _: u32) -> Result<Vec<Run>, QaRunsError> {
            not_this_adapters!()
        }
        async fn get_run_result(
            &self,
            _: &SecurityContext,
            _: Uuid,
        ) -> Result<RunResult, QaRunsError> {
            not_this_adapters!()
        }
        async fn list_runs_finished_since(
            &self,
            _: &SecurityContext,
            _: FinishedRunCursor,
            _: u32,
        ) -> Result<Vec<Run>, QaRunsError> {
            not_this_adapters!()
        }
        async fn list_run_test_results(
            &self,
            _: &SecurityContext,
            _: Uuid,
        ) -> Result<Vec<RunTestResult>, QaRunsError> {
            not_this_adapters!()
        }
        async fn cancel_run(&self, _: &SecurityContext, _: Uuid) -> Result<Run, QaRunsError> {
            not_this_adapters!()
        }
        async fn rerun(&self, _: &SecurityContext, _: Uuid) -> Result<LaunchOutcome, QaRunsError> {
            not_this_adapters!()
        }
        async fn list_queue(
            &self,
            _: &SecurityContext,
            _: Option<Uuid>,
            _: u32,
        ) -> Result<Vec<QueueEntry>, QaRunsError> {
            not_this_adapters!()
        }
        async fn cancel_queued(&self, _: &SecurityContext, _: Uuid) -> Result<(), QaRunsError> {
            not_this_adapters!()
        }
        async fn force_start_queued(
            &self,
            _: &SecurityContext,
            _: Uuid,
        ) -> Result<Run, QaRunsError> {
            not_this_adapters!()
        }
        async fn list_schedules(&self, _: &SecurityContext) -> Result<Vec<Schedule>, QaRunsError> {
            not_this_adapters!()
        }
        async fn get_schedule(
            &self,
            _: &SecurityContext,
            _: Uuid,
        ) -> Result<Schedule, QaRunsError> {
            not_this_adapters!()
        }
        async fn create_schedule(
            &self,
            _: &SecurityContext,
            _: NewSchedule,
        ) -> Result<Schedule, QaRunsError> {
            not_this_adapters!()
        }
        async fn update_schedule(
            &self,
            _: &SecurityContext,
            _: Uuid,
            _: NewSchedule,
        ) -> Result<Schedule, QaRunsError> {
            not_this_adapters!()
        }
        async fn delete_schedule(&self, _: &SecurityContext, _: Uuid) -> Result<(), QaRunsError> {
            not_this_adapters!()
        }
        async fn update_schedule_notifications(
            &self,
            _: &SecurityContext,
            _: Uuid,
            _: ScheduleNotificationSettings,
        ) -> Result<Schedule, QaRunsError> {
            not_this_adapters!()
        }
        async fn list_schedule_ticks(
            &self,
            _: &SecurityContext,
            _: Uuid,
        ) -> Result<Vec<ScheduleTick>, QaRunsError> {
            not_this_adapters!()
        }
        async fn resolve_runner_pod(
            &self,
            query: &RunnerPodQuery,
        ) -> Result<RunnerPodRun, QaRunsError> {
            self.asked.lock().unwrap().push(query.clone());
            (self.answer)()
        }
    }

    fn hub_answering(
        answer: fn() -> Result<RunnerPodRun, QaRunsError>,
    ) -> (Arc<ClientHub>, Arc<ScriptedRuns>) {
        let hub = Arc::new(ClientHub::new());
        let runs = Arc::new(ScriptedRuns {
            answer,
            asked: Mutex::new(Vec::new()),
        });
        hub.register::<dyn QaRunsClientV1>(Arc::clone(&runs) as Arc<dyn QaRunsClientV1>);
        (hub, runs)
    }

    const ENVIRONMENT: Uuid = Uuid::from_u128(0xE1);

    #[tokio::test]
    async fn no_qa_runs_in_the_hub_is_unavailable() {
        let resolver = HubRunnerPodResolver::new(Arc::new(ClientHub::new()));
        let result = resolver.environment_of(&a_runner_pod(Uuid::new_v4())).await;
        assert!(
            matches!(result, Err(RunnerPodRefusal::Unavailable(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_vouched_run_names_its_environment_and_is_asked_with_the_pods_labels() {
        let (hub, runs) = hub_answering(|| {
            Ok(RunnerPodRun {
                environment_id: ENVIRONMENT,
            })
        });
        let pod = a_runner_pod(Uuid::new_v4());

        let result = HubRunnerPodResolver::new(hub).environment_of(&pod).await;

        assert_eq!(result, Ok(ENVIRONMENT));
        let asked = runs.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].tenant_id, pod.tenant_id);
        assert_eq!(asked[0].run_id, pod.run_id);
        assert_eq!(asked[0].workflow_name, pod.workflow_name);
    }

    #[tokio::test]
    async fn not_found_is_a_refusal() {
        let (hub, _runs) = hub_answering(|| {
            Err(FarSide::not_found("no live run".to_owned())
                .with_resource("run")
                .create())
        });
        let result = HubRunnerPodResolver::new(hub)
            .environment_of(&a_runner_pod(Uuid::new_v4()))
            .await;
        assert_eq!(result, Err(RunnerPodRefusal::NotFound));
    }

    #[tokio::test]
    async fn any_other_failure_is_unavailable_not_a_refusal() {
        let (hub, _runs) = hub_answering(|| {
            Err(CanonicalError::service_unavailable()
                .with_detail("the database is down")
                .create())
        });
        let result = HubRunnerPodResolver::new(hub)
            .environment_of(&a_runner_pod(Uuid::new_v4()))
            .await;
        assert!(
            matches!(result, Err(RunnerPodRefusal::Unavailable(_))),
            "{result:?}"
        );
    }
}
