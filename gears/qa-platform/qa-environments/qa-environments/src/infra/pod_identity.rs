//! The Kubernetes half of [`PodIdentityVerifier`] (finding #94): a
//! `TokenReview` of the runner's projected token, audience-checked, then a
//! `GET pod` whose uid must equal the reviewed one. The client is the gears
//! pod's in-cluster identity (`Config::infer()`). The chart grants it
//! `tokenreviews: create` cluster-wide and `pods: get` in the Argo namespace.
//! The pod's workflow is its controller `ownerReference`, never a label.

use std::collections::BTreeMap;

use async_trait::async_trait;
use k8s_openapi::api::authentication::v1::{TokenReview, TokenReviewSpec, TokenReviewStatus};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, PostParams};
use kube::{Client, Config};
use uuid::Uuid;

use crate::domain::ports::{PodIdentityRefusal, PodIdentityVerifier, VerifiedRunnerPod};

/// The audience the runner's projected token is minted for. A token for any
/// other audience (the pod's default API token among them) is refused.
pub const AUDIENCE: &str = "qa-platform-runner-credentials";
const POD_NAME_EXTRA: &str = "authentication.kubernetes.io/pod-name";
const POD_UID_EXTRA: &str = "authentication.kubernetes.io/pod-uid";
/// The pod label naming the run's tenant, stamped by qa-runs' Argo executor.
pub const TENANT_LABEL: &str = "qa-platform/tenant-id";
/// The pod label naming the run, stamped by qa-runs' Argo executor.
pub const RUN_LABEL: &str = "qa-platform/run-id";
/// The API group of the Argo `Workflow` that controls a runner pod. The Argo
/// controller stamps every pod it creates with one controller
/// `ownerReference` to its `Workflow` (`argoproj.io/v1alpha1`, `Workflow`).
const WORKFLOW_API_GROUP: &str = "argoproj.io";
const WORKFLOW_KIND: &str = "Workflow";

/// What a `TokenReview` and a `GET pod` must agree on, as plain data, so the
/// decision is testable without an API server.
pub struct ReviewedToken {
    pub username: String,
    pub pod_name: String,
    pub pod_uid: String,
}

/// [`PodIdentityVerifier`] over the Argo cluster's API server.
pub struct KubePodIdentityVerifier {
    namespace: String,
    expected_username: String,
}

impl KubePodIdentityVerifier {
    /// A verifier for runner pods in `namespace` running as
    /// `runner_service_account`.
    #[must_use]
    pub fn new(namespace: String, runner_service_account: &str) -> Self {
        let expected_username =
            format!("system:serviceaccount:{namespace}:{runner_service_account}");
        Self {
            namespace,
            expected_username,
        }
    }

    async fn client() -> Result<Client, PodIdentityRefusal> {
        let config = Config::infer()
            .await
            .map_err(|_| PodIdentityRefusal::Unavailable("no in-cluster Kubernetes config"))?;
        Client::try_from(config).map_err(|_| {
            PodIdentityRefusal::Unavailable("the Kubernetes client could not be built")
        })
    }
}

#[async_trait]
impl PodIdentityVerifier for KubePodIdentityVerifier {
    async fn verify(&self, token: &str) -> Result<VerifiedRunnerPod, PodIdentityRefusal> {
        let client = Self::client().await?;
        let review = TokenReview {
            spec: TokenReviewSpec {
                token: Some(token.to_owned()),
                audiences: Some(vec![AUDIENCE.to_owned()]),
            },
            ..Default::default()
        };
        // Every error is dropped, never formatted: a `kube::Error` may echo
        // the request, and the request carries the token.
        let reviewed = Api::<TokenReview>::all(client.clone())
            .create(&PostParams::default(), &review)
            .await
            .map_err(|_| PodIdentityRefusal::Unavailable("the TokenReview call failed"))?;
        let status = reviewed.status.ok_or(PodIdentityRefusal::Unauthenticated(
            "the TokenReview has no status",
        ))?;
        let reviewed =
            reviewed_token(&status, AUDIENCE).map_err(PodIdentityRefusal::Unauthenticated)?;
        let pod = Api::<Pod>::namespaced(client, &self.namespace)
            .get_opt(&reviewed.pod_name)
            .await
            .map_err(|_| PodIdentityRefusal::Unavailable("the pod lookup failed"))?
            .ok_or(PodIdentityRefusal::Unauthenticated(
                "the reviewed pod does not exist",
            ))?;
        verified_pod(&reviewed, &self.expected_username, &pod)
            .map_err(PodIdentityRefusal::Unauthenticated)
    }
}

/// The one non-empty value `extra` holds under `key`, or `None` when it holds
/// none or several.
fn single(extra: Option<&BTreeMap<String, Vec<String>>>, key: &str) -> Option<String> {
    match extra?.get(key)?.as_slice() {
        [one] if !one.is_empty() => Some(one.clone()),
        _ => None,
    }
}

/// What a `TokenReview`'s status proves: an authenticated token, minted for
/// `audience`, bound to exactly one pod.
pub fn reviewed_token(
    status: &TokenReviewStatus,
    audience: &str,
) -> Result<ReviewedToken, &'static str> {
    if status.authenticated != Some(true) {
        return Err("the token is not authenticated");
    }
    if !status
        .audiences
        .as_ref()
        .is_some_and(|audiences| audiences.iter().any(|a| a == audience))
    {
        return Err("the token is not for this audience");
    }
    let user = status
        .user
        .as_ref()
        .ok_or("the token review names no user")?;
    let extra = user.extra.as_ref();
    Ok(ReviewedToken {
        username: user.username.clone().unwrap_or_default(),
        pod_name: single(extra, POD_NAME_EXTRA).ok_or("the token is not bound to one pod")?,
        pod_uid: single(extra, POD_UID_EXTRA).ok_or("the token is not bound to one pod uid")?,
    })
}

/// The name of the one Argo `Workflow` that is `pod`'s controller owner, or
/// `None` when there is not exactly one.
///
/// The workflow name is the binding to a run (it is compared to the run's
/// `execution_ref`), so it is read from the controller `ownerReference`, which
/// the Argo controller sets and a `Workflow` spec cannot. The
/// `workflows.argoproj.io/workflow` label is not used: Argo applies template
/// metadata labels after its own, so a `Workflow` author can overwrite it.
/// A principal that can create pods directly can still write any
/// `ownerReferences`; ADR-0008 records that residue.
fn controlling_workflow(pod: &Pod) -> Option<String> {
    let mut workflows = pod
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|owner| owner.controller == Some(true))
        .filter(|owner| {
            owner.kind == WORKFLOW_KIND
                && owner
                    .api_version
                    .split_once('/')
                    .is_some_and(|(group, _)| group == WORKFLOW_API_GROUP)
        });
    match (workflows.next(), workflows.next()) {
        (Some(owner), None) if !owner.name.is_empty() => Some(owner.name.clone()),
        _ => None,
    }
}

/// The runner pod `reviewed` names, when `pod` is it: the runner
/// `ServiceAccount`'s token, the reviewed uid (so a pod recreated under the
/// same name does not inherit the token), not finished, labelled with its
/// tenant and run, and controlled by exactly one Argo `Workflow`.
pub fn verified_pod(
    reviewed: &ReviewedToken,
    expected_username: &str,
    pod: &Pod,
) -> Result<VerifiedRunnerPod, &'static str> {
    if reviewed.username != expected_username {
        return Err("the token is not the runner ServiceAccount's");
    }
    if pod.metadata.uid.as_deref() != Some(reviewed.pod_uid.as_str()) {
        return Err("the pod's uid is not the reviewed one");
    }
    let phase = pod.status.as_ref().and_then(|s| s.phase.as_deref());
    if matches!(phase, Some("Succeeded" | "Failed")) {
        return Err("the pod has finished");
    }
    let labels = pod
        .metadata
        .labels
        .as_ref()
        .ok_or("the pod has no labels")?;
    let uuid_label = |key: &str| labels.get(key).and_then(|v| Uuid::parse_str(v).ok());
    Ok(VerifiedRunnerPod {
        pod_name: reviewed.pod_name.clone(),
        tenant_id: uuid_label(TENANT_LABEL).ok_or("the pod has no tenant label")?,
        run_id: uuid_label(RUN_LABEL).ok_or("the pod has no run label")?,
        workflow_name: controlling_workflow(pod)
            .ok_or("the pod is not controlled by exactly one Argo Workflow")?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use k8s_openapi::api::authentication::v1::{TokenReviewStatus, UserInfo};
    use k8s_openapi::api::core::v1::{Pod, PodStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
    use uuid::Uuid;

    use super::{AUDIENCE, ReviewedToken, reviewed_token, verified_pod};

    const USERNAME: &str = "system:serviceaccount:argo:qa-platform-runner";
    const POD: &str = "qa-run-1-abcde-n-0-1234";
    const UID: &str = "0b7a3c1e-0000-4000-8000-000000000001";
    const TENANT: &str = "11111111-1111-4111-8111-111111111111";
    const RUN: &str = "22222222-2222-4222-8222-222222222222";
    const WORKFLOW: &str = "qa-run-1-abcde";

    fn extra(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, vs)| {
                (
                    (*k).to_owned(),
                    vs.iter().map(|v| (*v).to_owned()).collect(),
                )
            })
            .collect()
    }

    fn bound_extra() -> BTreeMap<String, Vec<String>> {
        extra(&[
            ("authentication.kubernetes.io/pod-name", &[POD]),
            ("authentication.kubernetes.io/pod-uid", &[UID]),
        ])
    }

    fn status(
        authenticated: Option<bool>,
        audiences: Option<Vec<&str>>,
        extra: BTreeMap<String, Vec<String>>,
    ) -> TokenReviewStatus {
        TokenReviewStatus {
            authenticated,
            audiences: audiences.map(|a| a.into_iter().map(str::to_owned).collect()),
            user: Some(UserInfo {
                username: Some(USERNAME.to_owned()),
                extra: Some(extra),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn good_status() -> TokenReviewStatus {
        status(Some(true), Some(vec![AUDIENCE]), bound_extra())
    }

    fn reviewed() -> ReviewedToken {
        ReviewedToken {
            username: USERNAME.to_owned(),
            pod_name: POD.to_owned(),
            pod_uid: UID.to_owned(),
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn all_labels() -> BTreeMap<String, String> {
        labels(&[
            ("qa-platform/tenant-id", TENANT),
            ("qa-platform/run-id", RUN),
            ("workflows.argoproj.io/workflow", WORKFLOW),
        ])
    }

    /// The ownerReference the Argo controller stamps on every pod it creates
    /// (`metav1.NewControllerRef(wf, argoproj.io/v1alpha1 Workflow)`).
    fn workflow_owner(name: &str) -> OwnerReference {
        OwnerReference {
            api_version: "argoproj.io/v1alpha1".to_owned(),
            kind: "Workflow".to_owned(),
            name: name.to_owned(),
            uid: "0b7a3c1e-0000-4000-8000-0000000000ff".to_owned(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }
    }

    fn pod(uid: &str, phase: &str, labels: BTreeMap<String, String>) -> Pod {
        Pod {
            metadata: ObjectMeta {
                name: Some(POD.to_owned()),
                uid: Some(uid.to_owned()),
                labels: Some(labels),
                owner_references: Some(vec![workflow_owner(WORKFLOW)]),
                ..Default::default()
            },
            status: Some(PodStatus {
                phase: Some(phase.to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_bound_authenticated_token_for_this_audience_is_reviewed() {
        let reviewed = reviewed_token(&good_status(), AUDIENCE).unwrap();
        assert_eq!(reviewed.username, USERNAME);
        assert_eq!(reviewed.pod_name, POD);
        assert_eq!(reviewed.pod_uid, UID);
    }

    #[test]
    fn an_unauthenticated_token_is_refused() {
        for authenticated in [Some(false), None] {
            let refused = status(authenticated, Some(vec![AUDIENCE]), bound_extra());
            assert!(
                reviewed_token(&refused, AUDIENCE).is_err(),
                "authenticated = {authenticated:?} must be refused"
            );
        }
    }

    #[test]
    fn a_token_for_another_audience_is_refused() {
        for audiences in [
            None,
            Some(vec![]),
            Some(vec!["https://kubernetes.default.svc"]),
        ] {
            let refused = status(Some(true), audiences.clone(), bound_extra());
            assert!(
                reviewed_token(&refused, AUDIENCE).is_err(),
                "audiences = {audiences:?} must be refused"
            );
        }
    }

    #[test]
    fn a_token_not_bound_to_exactly_one_pod_is_refused() {
        let cases = [
            extra(&[("authentication.kubernetes.io/pod-uid", &[UID])]),
            extra(&[("authentication.kubernetes.io/pod-name", &[POD])]),
            extra(&[
                (
                    "authentication.kubernetes.io/pod-name",
                    &[POD, "another-pod"],
                ),
                ("authentication.kubernetes.io/pod-uid", &[UID]),
            ]),
            extra(&[
                ("authentication.kubernetes.io/pod-name", &[POD]),
                ("authentication.kubernetes.io/pod-uid", &[]),
            ]),
            extra(&[
                ("authentication.kubernetes.io/pod-name", &[""]),
                ("authentication.kubernetes.io/pod-uid", &[UID]),
            ]),
        ];
        for extra in cases {
            let refused = status(Some(true), Some(vec![AUDIENCE]), extra.clone());
            assert!(
                reviewed_token(&refused, AUDIENCE).is_err(),
                "extra = {extra:?} must be refused"
            );
        }
    }

    #[test]
    fn a_review_naming_no_user_is_refused() {
        let refused = TokenReviewStatus {
            user: None,
            ..good_status()
        };
        assert!(reviewed_token(&refused, AUDIENCE).is_err());
    }

    #[test]
    fn a_live_labelled_pod_with_the_reviewed_uid_is_verified() {
        for phase in ["Running", "Pending"] {
            let verified = verified_pod(&reviewed(), USERNAME, &pod(UID, phase, all_labels()))
                .unwrap_or_else(|reason| panic!("phase {phase}: {reason}"));
            assert_eq!(verified.pod_name, POD);
            assert_eq!(verified.tenant_id, Uuid::parse_str(TENANT).unwrap());
            assert_eq!(verified.run_id, Uuid::parse_str(RUN).unwrap());
            assert_eq!(verified.workflow_name, WORKFLOW);
        }
    }

    #[test]
    fn another_service_accounts_token_is_refused() {
        let result = verified_pod(
            &reviewed(),
            "system:serviceaccount:argo:default",
            &pod(UID, "Running", all_labels()),
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_pod_whose_uid_is_not_the_reviewed_one_is_refused() {
        let result = verified_pod(
            &reviewed(),
            USERNAME,
            &pod(
                "0b7a3c1e-0000-4000-8000-000000000002",
                "Running",
                all_labels(),
            ),
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_finished_pod_is_refused() {
        for phase in ["Succeeded", "Failed"] {
            let result = verified_pod(&reviewed(), USERNAME, &pod(UID, phase, all_labels()));
            assert!(result.is_err(), "phase {phase} must be refused");
        }
    }

    #[test]
    fn a_pod_missing_a_label_is_refused() {
        for missing in ["qa-platform/tenant-id", "qa-platform/run-id"] {
            let mut labels = all_labels();
            labels.remove(missing);
            let result = verified_pod(&reviewed(), USERNAME, &pod(UID, "Running", labels));
            assert!(result.is_err(), "a pod without {missing} must be refused");
        }
    }

    #[test]
    fn the_workflow_name_is_the_controller_owner_references_not_the_label() {
        let mut labels = all_labels();
        labels.insert(
            "workflows.argoproj.io/workflow".to_owned(),
            "qa-run-victim-zzzzz".to_owned(),
        );
        let verified = verified_pod(&reviewed(), USERNAME, &pod(UID, "Running", labels)).unwrap();
        assert_eq!(verified.workflow_name, WORKFLOW);
    }

    #[test]
    fn a_pod_without_the_workflow_label_but_owned_by_a_workflow_is_verified() {
        let mut labels = all_labels();
        labels.remove("workflows.argoproj.io/workflow");
        let verified = verified_pod(&reviewed(), USERNAME, &pod(UID, "Running", labels)).unwrap();
        assert_eq!(verified.workflow_name, WORKFLOW);
    }

    #[test]
    fn a_pod_not_controlled_by_exactly_one_workflow_is_refused() {
        let not_controller = OwnerReference {
            controller: None,
            ..workflow_owner(WORKFLOW)
        };
        let controller_false = OwnerReference {
            controller: Some(false),
            ..workflow_owner(WORKFLOW)
        };
        let wrong_kind = OwnerReference {
            kind: "WorkflowTemplate".to_owned(),
            ..workflow_owner(WORKFLOW)
        };
        let wrong_group = OwnerReference {
            api_version: "example.com/v1alpha1".to_owned(),
            ..workflow_owner(WORKFLOW)
        };
        let replica_set = OwnerReference {
            api_version: "apps/v1".to_owned(),
            kind: "ReplicaSet".to_owned(),
            ..workflow_owner(WORKFLOW)
        };
        let empty_name = workflow_owner("");
        let cases: Vec<(&str, Option<Vec<OwnerReference>>)> = vec![
            ("label only, no ownerReferences", None),
            ("an empty ownerReferences list", Some(vec![])),
            ("a non-controller Workflow owner", Some(vec![not_controller])),
            ("controller: false", Some(vec![controller_false])),
            ("a controller of the wrong kind", Some(vec![wrong_kind])),
            ("a Workflow of another API group", Some(vec![wrong_group])),
            ("a ReplicaSet controller", Some(vec![replica_set])),
            ("an empty owner name", Some(vec![empty_name])),
            (
                "two Workflow controllers",
                Some(vec![workflow_owner(WORKFLOW), workflow_owner("qa-run-2")]),
            ),
        ];
        for (case, owners) in cases {
            let mut candidate = pod(UID, "Running", all_labels());
            candidate.metadata.owner_references = owners;
            assert!(
                verified_pod(&reviewed(), USERNAME, &candidate).is_err(),
                "{case} must be refused"
            );
        }
    }

    #[test]
    fn a_pod_with_no_labels_is_refused() {
        let mut unlabelled = pod(UID, "Running", BTreeMap::new());
        unlabelled.metadata.labels = None;
        assert!(verified_pod(&reviewed(), USERNAME, &unlabelled).is_err());
    }

    #[test]
    fn a_label_that_is_not_a_uuid_is_refused() {
        for label in ["qa-platform/tenant-id", "qa-platform/run-id"] {
            let mut labels = all_labels();
            labels.insert(label.to_owned(), "not-a-uuid".to_owned());
            let result = verified_pod(&reviewed(), USERNAME, &pod(UID, "Running", labels));
            assert!(result.is_err(), "a non-UUID {label} must be refused");
        }
    }
}
