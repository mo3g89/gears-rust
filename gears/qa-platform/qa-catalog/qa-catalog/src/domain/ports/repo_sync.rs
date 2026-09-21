use async_trait::async_trait;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// Result of syncing one repository.
#[domain_model]
#[derive(Debug, Clone)]
pub struct SyncResult {
    pub branches: Vec<String>,
    /// Commit id of the checked-out branch head.
    ///
    /// **Persisted since `m20260921_000003_repo_head_commit`.**
    /// `ReposService::record_sync_success` writes it onto
    /// `qa_test_repositories.head_commit`, where it becomes the repository's
    /// content revision: the discovery cache keys on it, and the REST surface
    /// publishes it. The `dead_code` allowance this field used to carry is
    /// gone with the reason for it — it now has a production reader, not only
    /// the gix integration test.
    ///
    /// It remains the only observable that distinguishes "checked out the
    /// branch we asked for, at its current tip" from "checked out something
    /// else", which is what that integration test asserts on both the clone
    /// and the fetch path.
    pub head_commit: String,
}

/// Port abstracting the git engine. Implemented in infra (gix; see ADR-0005,
/// Task 9). Consumed by `domain::service::ReposService`.
#[async_trait]
pub trait RepoSyncPort: Send + Sync {
    /// Clone-or-fetch the repository into `host_dir`, then materialize
    /// `branch`'s content into `branch_workdir`, and return branch inventory
    /// + head commit.
    ///
    /// `host_dir` holds one clone per repository (objects + refs) shared by
    /// every branch; `branch_workdir` is a plain content snapshot with no
    /// `.git` of its own. `credential` is the resolved secret material (never
    /// logged), already fetched from credstore by the service.
    async fn sync(
        &self,
        url: &str,
        branch: &str,
        credential: Option<&str>,
        host_dir: &std::path::Path,
        branch_workdir: &std::path::Path,
    ) -> Result<SyncResult, DomainError>;

    /// List remote branches without a full sync. Consumed by
    /// `ReposService::refresh_branches` (the branch-cache refresher
    /// lifecycle task).
    async fn list_remote_branches(
        &self,
        url: &str,
        credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError>;
}
