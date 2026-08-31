//! Run → [`RunSpec`] translation: parity spec §3.4 rules 4, 5 and 6.
//!
//! The half of dispatch that turns a stored run into something the execution plane
//! can accept — force-sync, bundle build, node list, environment assembly — and
//! **touches no queue row and no lease**. Split out of `service::dispatch` when
//! that file reached 2,600 lines, on three grounds:
//!
//! * the two halves share no state: this one talks to qa-catalog,
//!   qa-environments and the executor, while the tick owns `TickReport`, the
//!   `PASS_*` constants and the system-actor pairing;
//! * the tick's ordering argument is only checkable with its five passes
//!   co-located, so **the tick was deliberately not split** — this is the only
//!   seam in the module that cuts along a real boundary;
//! * the split point was chosen while expecting Task 15's re-run to land here.
//!   **It does not, and this line said it would** — corrected 2026-08-14. A re-run
//!   rebuilds a `LaunchRequest` and calls `service::launch::LaunchService::launch`,
//!   the one creation path, so it cannot live on `DispatchService`: the launch
//!   service already holds an `Arc<dyn InlineDispatcher>` which the container wires
//!   to *this* service, so a dispatch service holding a launch service would close
//!   an `Arc` cycle. Re-run is `service::runs::RunsService::rerun`. The third
//!   ground for the split is therefore withdrawn; the first two stand on their
//!   own, and the file is not moved back because doing so would re-create the
//!   2,600-line module for a reason that no longer exists.
//!
//! It is a second `impl DispatchService` block, so nothing about the type's
//! interface changes: `dispatch_one` still calls [`DispatchService::submit`] as a
//! private method, and `Started` still crosses between the two files.

use std::collections::BTreeMap;

use qa_catalog_sdk::{BundleRequest, SyncRequest, TestBundle};
use qa_runs_sdk::{Run, RunTarget};
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use tracing::warn;
use uuid::Uuid;

use super::actions;
use super::dispatch::{DispatchService, Started, environments_error};
use crate::domain::env_assembly::{self, EnvInputs, EnvVar};
use crate::domain::error::DomainError;
use crate::domain::exclusivity::tags_admit;
use crate::domain::ports::run_executor::{
    ExecutionNode, KubeconfigMount, RunEnv, RunSpec, SecretRef,
};
use crate::domain::repos::{QueueRepository, RunsRepository};

/// Where the execution places the resolved kubeconfig **file**.
///
/// One constant, because [`KubeconfigMount::mount_path`] and the `KUBECONFIG`
/// environment entry must be equal and are produced on two different tiers —
/// the port can only name that obligation, and this is where it is discharged:
/// [`DispatchService::build_spec`] reads this value once and puts it in both
/// places. The source system's equivalent is two literals in one `if let`
/// (`manager/src/services/argo.rs:504-521`), where the volume mounts the
/// directory `/.kube` and the variable names the file inside it.
const KUBECONFIG_MOUNT_PATH: &str = "/.kube/kubeconfig";

/// Seconds from `now` until `deadline`, floored at 0.
///
/// The mirror of [`elapsed_seconds`], and floored for the same reason: both
/// instants may come from different clocks, and a deadline a second in the past
/// must read as "no time left" rather than wrapping.
fn seconds_until(deadline: OffsetDateTime, now: OffsetDateTime) -> u64 {
    (deadline - now).whole_seconds().max(0).unsigned_abs()
}

/// The executor-side deadline for a run, in the units [`RunSpec::timeout_seconds`]
/// takes.
///
/// Derived from the run's **recorded** `timeout_at`, never re-resolved: the
/// deadline is absolute and was decided at launch, so a run that waited an hour
/// in the queue gets the hour that is left and not a fresh full budget.
/// `domain::timeout` is public precisely so a *new* decision can reuse the chain;
/// this is not a new decision.
///
/// `0` means "no executor-side deadline", matching this gear's `0`-is-disabled
/// convention for every other limit, and is returned **only** when the run
/// carries no `timeout_at` at all. A run already past its deadline gets `1`
/// rather than `0`, because `0` would silently promote an overdue run to an
/// unbounded one.
///
/// # What this does not guarantee, stated because the port claims the opposite
/// elsewhere
///
/// [`RunSpec::timeout_seconds`]'s doc says the control-plane sweep "is the one
/// that must fire first". With the backstop set to the exact remaining time the
/// two deadlines **coincide**, and since the sweep only runs once per tick the
/// executor's will usually fire first in practice. That does not defeat
/// `cpt-cf-qa-fr-runs-timeout`, whose requirement is that the control plane
/// enforce the timeout itself rather than rely on the backend — both do, and the
/// run reaches `TimedOut` either way. Making the control plane strictly first
/// requires a grace margin added to the backstop, which is a number this task
/// declines to invent; it is recorded for the coordinator instead.
fn executor_deadline(run: &Run, now: OffsetDateTime) -> u64 {
    run.timeout_at
        .map_or(0, |deadline| seconds_until(deadline, now).max(1))
}

/// A node's stable label.
///
/// One node per repository group (parity spec §3.4 step 5), so the group's
/// repository id is the only thing that distinguishes them. Not sanitised to a
/// DNS-1123 label: that constraint came from Argo task names and ADR-0001 removes
/// it, so [`ExecutionNode::name`] says an adapter with its own naming rules
/// sanitises on its own side.
fn node_name(repo_id: Uuid) -> String {
    format!("repo-{repo_id}")
}

/// Group `(repo_id, path)` pairs by repository, deduplicated, in a deterministic
/// order.
///
/// # This is deliberately *not* `launch::group_by_repo`, and the reason is not
/// convenience
///
/// That function is private to `launch` and answers a different question from
/// the same data: it decides whether a custom plan spans enough repositories to
/// need an explicit branch (rule 3's guard). This one decides how many execution
/// nodes there are and what each carries. The two cannot disagree observably,
/// because launch's grouping is never recorded — `NewRun::bundle_ids` is written
/// **empty** and rule 4/5 belong here — so nothing downstream compares them.
/// What both must have is determinism, and both get it from a `BTreeMap` for the
/// reason the source system gives at `manager/src/routes/custom_plans.rs:723-724`:
/// node ids are assigned in sorted order so they are stable between processes.
///
/// Widening launch's function to `pub(super)` was the alternative. It was
/// declined because this task does not own that file and because a shared
/// function would imply the two groupings must stay identical, which is a
/// stronger claim than the truth.
fn group_files_by_repo(files: &[(Uuid, String)]) -> BTreeMap<Uuid, Vec<String>> {
    let mut grouped: BTreeMap<Uuid, Vec<String>> = BTreeMap::new();
    for (repo_id, path) in files {
        // An entry that is empty or whitespace-only is dropped rather than handed
        // to the runner.
        //
        // **This is not parity with legacy's filter, and an earlier version of
        // this comment claimed it was.** Legacy's `.filter(|path|
        // !path.is_empty())` (`manager/src/services/argo.rs:428`) runs on the
        // *output* of `normalize_test_path` (`:427`), which trims, strips a
        // leading `./` and `/`, and rewrites backslashes — so the filter it
        // performs is "empty after normalization", and normalization is the half
        // this port deliberately does not carry (see `build_spec`). The claim was
        // a case of the rule this subsystem keeps relearning: when an identifier
        // is replaced, check what it was carrying *implicitly*.
        //
        // Falsifying inputs, named so the divergence is not rediscovered:
        // **`"./"` and `"/"`**. Legacy normalizes both to `""` and drops them;
        // this keeps them and hands them to the runner, where `./` collects a
        // directory rather than a file. A plan whose `tests:` list contains one is
        // the only way to reach it. Recorded as a follow-up rather than fixed,
        // because the fix is `normalize_test_path` itself.
        if path.trim().is_empty() {
            continue;
        }
        let paths = grouped.entry(*repo_id).or_default();
        // The **un-trimmed** value is pushed, and the dedupe compares un-trimmed,
        // so `" a.py"` and `"a.py"` are two entries. That is legacy's behaviour
        // too, for a different reason: it dedupes on the full node id *before*
        // normalizing (`exclusivity.rs:459-463`) and never dedupes after, so it
        // would emit both as well.
        if !paths.contains(path) {
            paths.push(path.clone());
        }
    }
    grouped
}

/// A static environment entry, skipped when blank.
///
/// The source system's blank guards are per-name and **not** uniform:
/// `APP_VERSION`, `APP_BUILD`, `TEST_VERSION` and the bundle reference are
/// skipped when blank (`manager/src/services/argo.rs:461-469`, `:40-49`) while
/// `TEST_FILES` is pushed unconditionally (`:436`). This helper is used only for
/// the skipped ones, which is why `env_assembly` refuses to blank-filter the list
/// as a whole (its composition obligation 3).
fn push_if_present(into: &mut Vec<EnvVar>, name: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        into.push(EnvVar {
            name: name.to_owned(),
            value: value.to_owned(),
        });
    }
}

fn catalog_error(error: &qa_catalog_sdk::QaCatalogError) -> DomainError {
    DomainError::Catalog(error.to_string())
}

impl<R, Q> DispatchService<R, Q>
where
    R: RunsRepository,
    Q: QueueRepository,
{
    /// Parity spec §3.4 rules 4 and 5, then the submit.
    pub(super) async fn submit(
        &self,
        ctx: &SecurityContext,
        run: &Run,
    ) -> Result<Started, DomainError> {
        // Rule 6 read back, not re-derived. `launch::resolve_branch` is private
        // on purpose: re-resolving would consult the platform and repository
        // defaults again, minutes later and after the admission decision, so an
        // edited `default_branch` would have this sync fetch a different ref than
        // the one the run's exclusivity was resolved from and than the one
        // recorded as its version.
        let branch = run.test_version.clone().ok_or(DomainError::CorruptState {
            what: "run.test_version",
            id: run.id,
            value: String::new(),
        })?;

        let groups = self.resolve_groups(ctx, run, &branch).await?;
        let groups = self.apply_tag_filter(ctx, run, &branch, groups).await?;
        if groups.is_empty() {
            // Legacy answers BAD_REQUEST for a plan with no runnable tests on the
            // branch (`manager/src/routes/runs.rs:678-686`), and again after the
            // tag filter empties the list. A run with no nodes would execute
            // nothing and report success, which is what `RunSpec::nodes` refuses.
            return Err(DomainError::Validation {
                field: "target".to_owned(),
                message: format!(
                    "run {} has no runnable test files on branch {branch}",
                    run.id
                ),
            });
        }

        let mut nodes = Vec::with_capacity(groups.len());
        let mut bundle_ids = Vec::with_capacity(groups.len());
        for (repo_id, files) in &groups {
            self.catalog
                .sync_repo(
                    ctx,
                    *repo_id,
                    SyncRequest {
                        branch: Some(branch.clone()),
                        force: true,
                    },
                )
                .await
                .map_err(|error| catalog_error(&error))?;
            let bundle = self.build_bundle(ctx, *repo_id, &branch, files).await?;
            bundle_ids.push(bundle.id);
            nodes.push(ExecutionNode {
                name: node_name(*repo_id),
                bundle_ref: bundle.storage_ref,
                test_files: files.clone(),
            });
        }

        // Recorded before the submit: a bundle recorded for a run that never
        // started is a dangling reference `expires_at` reclaims, whereas a run
        // that started against bundles nothing recorded is unreproducible. See
        // `RunsRepository::set_bundle_ids`.
        let bundle_scope = self.run_scope(ctx, actions::DISPATCH, Some(run.id)).await?;
        let conn = self.db.conn()?;
        self.runs
            .set_bundle_ids(&conn, &bundle_scope, run.id, &bundle_ids)
            .await?;

        let spec = self.build_spec(ctx, run, nodes).await?;
        let execution_ref = self.executor.start(spec).await?;
        Ok(Started {
            execution_ref,
            started_at: OffsetDateTime::now_utc(),
        })
    }

    /// Build one group's bundle, retrying **once** on failure.
    ///
    /// # Decision D4's bounded retry, and why it is exactly one
    ///
    /// `DECOMPOSITION.md:126` records that reads do not serialize against
    /// snapshot rewrites, and decision D4 accepted that at parity: legacy's
    /// readers walk the checkout with no lock either
    /// (`manager/src/services/test_repos.rs:405-406`, `:452-456` — writers only).
    /// One honest difference in degree survives, and it is the reason this retry
    /// exists: legacy updates a branch directory via `git worktree`, rewriting
    /// files in place, while qa-catalog **clears** the snapshot before rewriting
    /// it. Same class of race, but legacy's window exposes a *partial* read where
    /// qa-catalog's also exposes an *empty* one — and an empty snapshot is
    /// exactly what makes a bundle build fail immediately after this call's own
    /// force-sync.
    ///
    /// So one retry, closing only the widened part. **Not a loop**: a genuinely
    /// unbuildable group — a deleted path, a branch without the files — would
    /// then hang the dispatcher on it instead of failing the run, and the D4
    /// resolution is explicit that no new snapshot machinery is added.
    ///
    /// The retry re-issues the *bundle build*, not the sync: this call already
    /// force-synced, so a second sync would re-open the same window rather than
    /// wait out the first one.
    async fn build_bundle(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        files: &[String],
    ) -> Result<TestBundle, DomainError> {
        let request = BundleRequest {
            repo_id,
            branch: branch.to_owned(),
            files: files.to_vec(),
        };
        match self.catalog.create_bundle(ctx, request.clone()).await {
            Ok(bundle) => Ok(bundle),
            Err(first) => {
                warn!(
                    %repo_id,
                    branch,
                    error = %first,
                    "the bundle build failed immediately after this dispatch's own \
                     force-sync; retrying once (decision D4)",
                );
                self.catalog
                    .create_bundle(ctx, request)
                    .await
                    .map_err(|error| catalog_error(&error))
            }
        }
    }

    /// The files this run executes, by repository.
    ///
    /// Re-read from the catalog rather than stored on the run: the run row records
    /// its *target*, and which files a target contains is a property of the branch
    /// at dispatch time. The source system does the same — `dispatch` replays the
    /// intent through the submit path, which re-reads the plan
    /// (`manager/src/services/run_dispatcher.rs:23-28`, `:180-209`).
    ///
    /// A single-test target needs no plan read at all: the file was validated at
    /// launch and is carried on the run.
    async fn resolve_groups(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        branch: &str,
    ) -> Result<BTreeMap<Uuid, Vec<String>>, DomainError> {
        let files: Vec<(Uuid, String)> = match &run.target {
            RunTarget::Plan { repo_id, path } => {
                let plan = self
                    .catalog
                    .get_plan(ctx, *repo_id, branch, path)
                    .await
                    .map_err(|error| catalog_error(&error))?;
                plan.test_files
                    .into_iter()
                    .map(|file| (*repo_id, file))
                    .collect()
            }
            RunTarget::Test {
                repo_id, test_file, ..
            } => vec![(*repo_id, test_file.clone())],
            RunTarget::CustomPlan { id } => {
                let plan = self
                    .catalog
                    .get_custom_plan(ctx, *id)
                    .await
                    .map_err(|error| catalog_error(&error))?;
                plan.files
                    .into_iter()
                    .map(|entry| (entry.repo_id, entry.path))
                    .collect()
            }
            // A collect run enumerates the **whole branch**, not a named plan:
            // legacy builds `files` as the union of every plan's `test_files`
            // on that branch (`manager/src/services/collect.rs:56-75`, the
            // `BTreeSet` at `:66` and the insert at `:73`), and comments the
            // grouping *"Files = union of every plan's test files on this
            // branch (matches the analytics universe)"*. `list_plans` is
            // qa-catalog's counterpart to legacy's
            // `list_plans_in_repository_root`.
            //
            // **One filter of legacy's is not ported, and it is not the one it
            // looks like.** `collect.rs:72` drops entries whose file is absent
            // on the branch, so *"a stale plan reference cannot fail the whole
            // repo's bundle"* (`:56-58`). That is a **file**-level skip, and it
            // is distinct from the repository-level skip at `:42-48`, where a
            // repo lacking the branch fails `sync_repository_branch_by_id` and
            // is dropped by `run_collect_cycle` (`:168-176`) rather than
            // failing the cycle — that one belongs to the trigger, which is
            // qa-insights' (Task 30).
            //
            // The file-level skip is not reproduced here because it is not
            // collect-specific: `RunTarget::Plan` above has the identical
            // exposure — a `plan.yaml` naming a file that is not on the branch
            // reaches `create_bundle` the same way — and qa-catalog exposes no
            // existence probe to filter with. Recorded rather than silently
            // diverged; closing it is a qa-catalog capability (Task 7), and it
            // must close both kinds at once or it has only moved the hazard.
            RunTarget::Collect { repo_id, .. } => {
                let plans = self
                    .catalog
                    .list_plans(ctx, *repo_id, branch)
                    .await
                    .map_err(|error| catalog_error(&error))?;
                plans
                    .into_iter()
                    .flat_map(|plan| plan.test_files)
                    .map(|file| (*repo_id, file))
                    .collect()
            }
        };
        Ok(group_files_by_repo(&files))
    }

    /// Apply the run's include/exclude tag filter to a plan run's files.
    ///
    /// The source system filters at submit time, from the checkout, and only for
    /// a plan run (`manager/src/routes/runs.rs:686-700`,
    /// `filter_tests_by_tags`). The two other kinds are excluded for the same
    /// reasons `ExclusivitySource` records: a single-test run considers only its
    /// one file and no filter applies
    /// (`manager/src/services/exclusivity.rs:207-224`), and a custom plan has no
    /// tag filter of its own (`exclusivity.rs:552-558`).
    ///
    /// **A tag read that fails fails the dispatch.** This is the one place in
    /// this module where failing open would be the dangerous direction: an
    /// `exclude_tags: ["destructive"]` filter that silently did not apply would
    /// run exactly the tests the caller asked to skip. Legacy reads tags from a
    /// local checkout and cannot fail this way; qa-catalog's `get_test_meta` is a
    /// cross-gear call and can, so the divergence is refusing rather than
    /// guessing.
    ///
    /// `tags_admit` carries the asymmetry the filter needs: a file with no tags is
    /// admitted by an exclude-only filter and rejected by an include filter,
    /// because it cannot prove membership.
    async fn apply_tag_filter(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        branch: &str,
        groups: BTreeMap<Uuid, Vec<String>>,
    ) -> Result<BTreeMap<Uuid, Vec<String>>, DomainError> {
        if !matches!(run.target, RunTarget::Plan { .. }) {
            return Ok(groups);
        }
        if run.include_tags.is_empty() && run.exclude_tags.is_empty() {
            return Ok(groups);
        }

        let mut filtered = BTreeMap::new();
        for (repo_id, files) in groups {
            let metas = self
                .catalog
                .get_test_meta(ctx, repo_id, branch, &files)
                .await
                .map_err(|error| catalog_error(&error))?;
            let kept: Vec<String> = files
                .into_iter()
                .filter(|path| {
                    let tags = metas
                        .iter()
                        .find(|meta| meta.path == *path)
                        .map_or(&[][..], |meta| meta.tags.as_slice());
                    tags_admit(tags, &run.include_tags, &run.exclude_tags)
                })
                .collect();
            if !kept.is_empty() {
                filtered.insert(repo_id, kept);
            }
        }
        Ok(filtered)
    }

    /// Everything the execution plane needs for one run.
    ///
    /// # What is assembled here, and what is deliberately not set
    ///
    /// `env_assembly`'s composition obligation 1 says assembly happens at
    /// dispatch, and obligation 2 that `APP_VERSION`/`APP_BUILD` come from the
    /// run's own snapshotted columns rather than a live platform lookup — both are
    /// honoured. `TEST_VERSION` is the recorded branch.
    ///
    /// **`TEST_FILES` and `TEST_BUNDLE_URL` are not in the shared environment**,
    /// and that is the port's shape rather than an omission: the source system has
    /// one bundle and one file list per *node* (`argo.rs:1220-1248`), and
    /// [`ExecutionNode`] carries both, so putting them in the shared map as well
    /// would create a second copy that disagrees with the node's whenever there
    /// is more than one group.
    ///
    /// **Four things the source system sets and this cannot yet.**
    /// `VHP_PROGRESS_URL`, `RP_PROJECT`/`RP_API_KEY`, `PRODUCT_KEY` and
    /// `SKIP_TESTS_WITH_BUGS` all come from the manager's own configuration or
    /// from `ReportPortal` settings (`argo.rs:436-499`), and this gear has no
    /// config surface for them: Task 16's `QaRunsConfig` declares none, and
    /// inventing knobs a later task must then reconcile is worse than an empty
    /// set. `E2E_K8S_NAMESPACE` is no longer on this list: it now has a source —
    /// the platform's `observed_namespace`, read from the same fetch below that
    /// resolves the kubeconfig mount, alongside `vhp_base_url`
    /// (`E2E_VHP_BASE_URL` / `VPADM_BASE_DOMAIN`). The secrets map handed to
    /// [`RunEnv::new`] is therefore **empty**, so `RP_API_KEY` is not bound.
    /// Recorded as an obligation for Task 16 and feature 2.7, not as a silent
    /// gap.
    ///
    /// **`normalize_test_path` is not applied to the node file lists.** Legacy
    /// trims, strips a leading `./` and `/`, and rewrites backslashes, in that
    /// order, and the order is observable (`argo.rs:2441-2446`; `launch`'s copy
    /// documents why). Duplicating a documented-order string transform into a
    /// second module is the drift this subsystem keeps finding, and the function
    /// is private to a file this task does not own — so only the blank filter
    /// legacy applies beside it is ported (see [`group_files_by_repo`]) and the
    /// rest is reported.
    async fn build_spec(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        nodes: Vec<ExecutionNode>,
    ) -> Result<RunSpec, DomainError> {
        let mut platform_base_url = None;
        let mut platform_namespace = None;
        let kubeconfig = match run.platform_id {
            None => None,
            Some(platform_id) => {
                let platform = self
                    .environments
                    .get_platform(ctx, platform_id)
                    .await
                    .map_err(|error| environments_error(&error))?;
                // The one platform fetch this method makes, reused for
                // `E2E_VHP_BASE_URL`/`VPADM_BASE_DOMAIN` and `E2E_K8S_NAMESPACE`
                // rather than issuing a second lookup for them.
                platform_base_url = platform.vhp_base_url;
                platform_namespace = platform.observed_namespace;
                Some(KubeconfigMount {
                    secret: SecretRef::new(platform.kubeconfig_credstore_ref),
                    // One value, two places: the equality `KubeconfigMount`
                    // documents as an obligation the port cannot enforce.
                    mount_path: KUBECONFIG_MOUNT_PATH.to_owned(),
                })
            }
        };

        let variables = self
            .environments
            .list_variables(ctx, run.platform_id)
            .await
            .map_err(|error| environments_error(&error))?;

        let mut statics = Vec::new();
        push_if_present(&mut statics, "TEST_VERSION", run.test_version.as_deref());
        push_if_present(&mut statics, "APP_VERSION", run.app_version.as_deref());
        push_if_present(&mut statics, "APP_BUILD", run.app_build.as_deref());
        // The collect-only pair, in the statics tier — which is where legacy
        // puts it: `push_repo_env` appends to the same `env_vars` list as
        // `TEST_VERSION`, before the pipeline variables
        // (`manager/src/services/argo.rs:50-59`, called at `:483-485`).
        //
        // **This branch is not enforced by the compiler and is the one that
        // matters most.** `resolve_groups` above has a `RunTarget::Collect` arm
        // because that `match` is exhaustive; this `if` does not, so a collect
        // run whose environment was assembled without it would carry a full
        // `TEST_FILES` node list and no `COLLECT_ONLY` — that is, it would
        // **execute** every test in the repository instead of counting them.
        // `env_assembly::assemble_collect_env` owns the frozen names.
        if let RunTarget::Collect { collect_url, .. } = &run.target {
            statics.extend(
                env_assembly::assemble_collect_env(collect_url)
                    .into_iter()
                    .map(|(name, value)| EnvVar { name, value }),
            );
        }

        let env = env_assembly::assemble(EnvInputs {
            statics,
            variables: env_assembly::split_by_scope(variables, run.platform_id),
            platform_base_url,
            platform_namespace,
            // The *stored* parameters, already normalized by
            // `params::normalize` at launch — a raw `"  FOO  "` would otherwise
            // become a variable literally named `"  FOO  "`.
            parameters: run.parameters.iter().cloned().map(EnvVar::from).collect(),
            kubeconfig_path: kubeconfig.as_ref().map(|mount| mount.mount_path.clone()),
        });

        Ok(RunSpec {
            run_id: run.id,
            run_name: run.name.clone(),
            nodes,
            env: RunEnv::new(env, BTreeMap::new()),
            kubeconfig,
            timeout_seconds: executor_deadline(run, OffsetDateTime::now_utc()),
        })
    }
}
