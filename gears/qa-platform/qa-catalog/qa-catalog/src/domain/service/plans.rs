//! Plan discovery and `TEST_META` reads over synced working copies.
//!
//! ## Legacy-truth: discovery pattern
//!
//! Discovery mirrors the legacy testrunner (`manager/src/services/plans.rs`):
//!
//! - **File-based plans**: every `*.yaml` file directly inside a `plans/`
//!   subdirectory of the content root (flat, non-recursive) — legacy
//!   `scan_file_based_plans`, plans.rs:502-590 (the `plans/` join at :512,
//!   the `.yaml` extension check at :533).
//! - **Legacy directory plans**: `<dir>/plan.yaml` one level under the
//!   content root (plans.rs:621), and `<dir>/<subdir>/plan.yaml` two levels
//!   under it (plans.rs:648) — the second level is only scanned when the
//!   first-level directory has no `plan.yaml` of its own (plans.rs:622-630).
//!
//! Unreadable or invalid plan files are skipped and logged — one bad plan
//! must not hide the rest — mirroring the legacy warn-and-continue behavior.
//!
//! ## Legacy-truth: the frozen `plan.yaml` contract (discovery half)
//!
//! PRD `cpt-cf-qa-fr-migration-runner-contract` freezes the format, so
//! existing repositories run unmodified. Two of the four legacy behaviors
//! that guarantees live here (the other two — optional `tests:` and the 300s
//! `timeout_seconds` default — live in
//! [`crate::domain::parsing::plan_yaml`]):
//!
//! - **Derived test-file lists.** A *directory* plan whose `plan.yaml` omits
//!   `tests:` derives its list by walking `<plan_dir>/tests/**`, keeping
//!   `test_*.py` files whose contents contain `TEST_META` — legacy
//!   `load_plan_from_dir` (plans.rs:685-692) → `list_test_files`
//!   (plans.rs:724-729). *File-based* plans derive nothing and keep an empty
//!   list (legacy `scan_file_based_plans`, plans.rs:501-589). See
//!   [`with_derived_test_files`].
//! - **Deterministic, de-duplicated order.** Legacy sorts the merged list
//!   (plans.rs:117, by plan id) and de-duplicates across the two flavors
//!   (`scan_root`, plans.rs:470-499). [`discover_plans`] sorts by **path** —
//!   this gear's plan identity, since a discovered plan is addressed as
//!   `(repo, branch, path)` — and keeps the first of a duplicate pair, so
//!   the file-based flavor wins as it does in legacy.
//!
//! ## Branch model
//!
//! The sync engine keeps one clone per repository and materializes each
//! branch's content into its own snapshot directory (see
//! `infra::git::layout`). A read for `(repo, branch)` is served from that
//! branch's snapshot when the repository has synced successfully
//! (`last_synced_at` set, `sync_error` clear) and the snapshot exists on
//! disk; anything else is [`DomainError::RepoNotSynced`].

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_catalog_sdk::{Exclusivity, Plan, SOURCE_REPO, TestFileMeta, TestRepository, UniverseTest};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::parsing::case_count::count_test_functions;
use crate::domain::parsing::plan_yaml::{ParsedPlan, normalize_test_path, parse_plan_yaml};
use crate::domain::parsing::test_meta::{ParsedTestMeta, parse_test_meta};
use crate::domain::repos::TestReposRepository;

/// Discovered-plan read service (plans are never persisted — they are
/// materialized from the synced working copy on every read).
#[domain_model]
pub struct PlansService<R: TestReposRepository> {
    db: Arc<DbProvider>,
    repos_repo: Arc<R>,
    repos_dir: PathBuf,
    policy_enforcer: PolicyEnforcer,
}

impl<R: TestReposRepository> PlansService<R> {
    pub fn new(
        db: Arc<DbProvider>,
        repos_repo: Arc<R>,
        repos_dir: PathBuf,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repos_repo,
            repos_dir,
            policy_enforcer,
        }
    }

    /// Resolve the repository (tenancy precheck under its own `TEST_REPO/GET`
    /// scope), require it synced for `branch`, and return the owning product
    /// plus the canonicalized content-root directory.
    async fn synced_content_root(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
    ) -> Result<(Uuid, PathBuf), DomainError> {
        let repo_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::GET, Some(repo_id))
            .await?;

        let conn = self.db.conn()?;
        let repo = self
            .repos_repo
            .get(&conn, &repo_scope, repo_id)
            .await?
            .ok_or(DomainError::NotFound { id: repo_id })?;

        require_synced(&repo, branch)?;
        let root = content_root_dir(&self.repos_dir, &repo, branch)?;
        Ok((repo.product_id, root))
    }
}

// Business logic methods
impl<R: TestReposRepository> PlansService<R> {
    /// Discover every plan in the synced working copy of `(repo_id, branch)`.
    #[instrument(skip(self, ctx), fields(repo_id = %repo_id, branch = %branch))]
    pub async fn list_plans(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
    ) -> Result<Vec<Plan>, DomainError> {
        debug!("Listing discovered plans");

        // Authorization gate for the PLAN resource. Discovery reads no PLAN
        // table (plans are not persisted), so the compiled scope has no query
        // to bind to — the PEP decision itself is what this call enforces.
        // The repository resolve below derives its own TEST_REPO scope.
        let _plan_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLAN, actions::LIST, None)
            .await?;

        let (product_id, root) = self.synced_content_root(ctx, repo_id, branch).await?;

        let discovered = tokio::task::spawn_blocking(move || discover_plans(&root))
            .await
            .map_err(|e| DomainError::Internal(e.to_string()))?;

        let plans = discovered
            .into_iter()
            .map(|(path, parsed)| to_sdk_plan(repo_id, product_id, branch, path, parsed))
            .collect::<Vec<_>>();

        debug!("Discovered {} plans", plans.len());
        Ok(plans)
    }

    /// Load a single plan by its path within the content root.
    #[instrument(skip(self, ctx), fields(repo_id = %repo_id, branch = %branch, path = %path))]
    pub async fn get_plan(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        path: &str,
    ) -> Result<Plan, DomainError> {
        debug!("Getting plan by path");

        let _plan_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLAN, actions::GET, None)
            .await?;

        let (product_id, root) = self.synced_content_root(ctx, repo_id, branch).await?;

        let file = resolve_under_root(&root, path)?.ok_or_else(|| DomainError::PlanNotFound {
            repo_id,
            branch: branch.to_owned(),
            path: path.to_owned(),
        })?;

        let content = match tokio::fs::read_to_string(&file).await {
            Ok(source) => source,
            // Genuinely not there: the caller-facing answer stays 404.
            Err(error) if io_is_absent(&error) => {
                return Err(DomainError::PlanNotFound {
                    repo_id,
                    branch: branch.to_owned(),
                    path: path.to_owned(),
                });
            }
            // Anything else (EACCES, a mid-read IO failure, ...) is a fault,
            // not an absence -- folding it into `PlanNotFound` told an
            // operator with a misconfigured snapshot directory that their
            // plan does not exist. Review finding #6.
            Err(error) => {
                return Err(DomainError::Internal(format!(
                    "plan '{path}' in repository {repo_id} branch '{branch}' could not be \
                     read: {error}"
                )));
            }
        };

        let parsed = parse_plan_yaml(&content)?;
        Ok(to_sdk_plan(
            repo_id,
            product_id,
            branch,
            path.to_owned(),
            parsed,
        ))
    }

    /// Parse `TEST_META` for each of `files` (paths under the content root).
    ///
    /// All paths are validated before any file is read, so a traversal
    /// attempt in any entry rejects the whole request.
    #[instrument(skip(self, ctx, files), fields(repo_id = %repo_id, branch = %branch, files = files.len()))]
    pub async fn get_test_meta(
        &self,
        ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        files: &[String],
    ) -> Result<Vec<TestFileMeta>, DomainError> {
        debug!("Reading TEST_META");

        let _plan_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLAN, actions::GET, None)
            .await?;

        for file in files {
            validate_rel_path("files", file)?;
        }

        let (_, root) = self.synced_content_root(ctx, repo_id, branch).await?;

        let mut metas = Vec::with_capacity(files.len());
        for file in files {
            let resolved = resolve_under_root(&root, file)?
                .ok_or_else(|| DomainError::FileNotFound { path: file.clone() })?;
            let source = match tokio::fs::read_to_string(&resolved).await {
                Ok(source) => source,
                // Genuinely not there: same 404-shaped answer as before.
                Err(error) if io_is_absent(&error) => {
                    return Err(DomainError::FileNotFound { path: file.clone() });
                }
                // Anything else is a fault: an EACCES here used to read as
                // "file not found" and silently drop the file's exclusivity
                // vote. Review finding #7.
                Err(error) => {
                    return Err(DomainError::Internal(format!(
                        "file '{file}' in repository {repo_id} branch '{branch}' could not be \
                         read: {error}"
                    )));
                }
            };
            let parsed = parse_test_meta(&source);
            metas.push(TestFileMeta {
                path: file.clone(),
                title: parsed.title,
                tags: parsed.tags,
                exclusive: Exclusivity::from_option_bool(parsed.exclusive),
                bugs: parsed.bugs,
            });
        }

        Ok(metas)
    }

    /// Every test file reachable from a product's plans, as the analytics
    /// universe sees it.
    ///
    /// This is the read projection qa-insights consumes; see
    /// [`UniverseTest`] for the field mapping and
    /// [`QaCatalogClientV1::list_universe`](qa_catalog_sdk::QaCatalogClientV1::list_universe)
    /// for the contract. Legacy computed the same thing inline over its own
    /// checkout (`manager/src/routes/analytics.rs:805-951`); ADR-0005 puts the
    /// checkout on this side of the gear boundary, so it is computed here and
    /// shipped whole.
    ///
    /// ## One walk, not one per plan
    ///
    /// Every repository's content root is discovered once and every plan's
    /// test files are read on that same traversal, inside a single
    /// `spawn_blocking` per repository. The alternative — qa-insights calling
    /// `list_plans` and then `get_test_meta` per plan — is an N+1 across the
    /// SDK boundary that would make the overview endpoint's latency a
    /// function of plan count, and would re-read the checkout once per plan.
    ///
    /// ## Failure posture: skip the repository, do not fail the call
    ///
    /// A repository that has never synced, or has no snapshot for the
    /// selected branch, contributes nothing and is logged. Legacy behaves the
    /// same way — under a branch filter it drops plan entries that do not
    /// resolve on that branch rather than erroring (`analytics.rs:856-865`) —
    /// and one unsynced repository must not blank the overview for every
    /// other one.
    #[instrument(skip(self, ctx), fields(product_id = ?product_id, branch = ?branch))]
    pub async fn list_universe(
        &self,
        ctx: &SecurityContext,
        product_id: Option<Uuid>,
        branch: Option<&str>,
    ) -> Result<Vec<UniverseTest>, DomainError> {
        debug!("Building the analytics universe");

        // Same PLAN/LIST gate `list_plans` applies: the projection is a plan
        // read, and the repository enumeration below derives its own
        // TEST_REPO/LIST scope. Neither query ever runs with `allow_all`.
        let _plan_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLAN, actions::LIST, None)
            .await?;

        let repo_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::LIST, None)
            .await?;
        let conn = self.db.conn()?;
        let repos = self.repos_repo.list(&conn, &repo_scope).await?;

        let mut universe = Vec::new();
        for repo in repos {
            // Tenancy is enforced by `repo_scope` above — this filter is a
            // *product* filter, never a security boundary. A repository the
            // caller cannot see is already absent from `repos`.
            if product_id.is_some_and(|wanted| repo.product_id != wanted) {
                continue;
            }

            // No branch named => the repository's own default, which is
            // legacy's fallback when no branch is selected
            // (`analytics.rs:814-816`).
            let effective_branch = branch
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(repo.default_branch.as_str())
                .to_owned();

            if require_synced(&repo, &effective_branch).is_err() {
                debug!(repo_id = %repo.id, "Skipping unsynced repository in the universe walk");
                continue;
            }
            // `content_root_dir` answers `RepoNotSynced` for a genuinely
            // absent snapshot (unremarkable — logged at `debug!`, matching
            // this function's failure posture above) and `Internal` for a
            // fault such as an EACCES on the `branches` directory (review
            // finding #26). Either way this repository is still skipped, not
            // failed (same failure posture as `require_synced` above) — what
            // must not happen is telling an operator a repository has never
            // synced, at `debug!`, when it in fact has and the real cause is
            // an unreadable directory that `warn!` would have surfaced.
            let root = match content_root_dir(&self.repos_dir, &repo, &effective_branch) {
                Ok(root) => root,
                Err(DomainError::RepoNotSynced { .. }) => {
                    debug!(
                        repo_id = %repo.id,
                        branch = %effective_branch,
                        "Skipping repository with no snapshot for the selected branch"
                    );
                    continue;
                }
                Err(error) => {
                    warn!(
                        repo_id = %repo.id, branch = %effective_branch, error = %error,
                        "Skipping repository in the universe walk: content root could not be resolved"
                    );
                    continue;
                }
            };

            let repo_id = repo.id;
            let entries = tokio::task::spawn_blocking(move || walk_repo_universe(repo_id, &root))
                .await
                .map_err(|e| DomainError::Internal(e.to_string()))?;
            universe.extend(entries);
        }

        // Legacy sorts by `test_name` alone (`analytics.rs:941`) over values
        // drained from a `HashMap`, so its tie order is whatever the map
        // yielded — i.e. unspecified. The tie-breakers here make the response
        // reproducible without changing which rows appear or their primary
        // ordering.
        universe.sort_by(|a, b| {
            a.test_name
                .cmp(&b.test_name)
                .then_with(|| a.repo_id.cmp(&b.repo_id))
                .then_with(|| a.test_file.cmp(&b.test_file))
        });

        debug!("Universe holds {} test files", universe.len());
        Ok(universe)
    }
}

/// Discover one repository's plans and project every test file they list.
///
/// Blocking (filesystem); run under `spawn_blocking`.
///
/// ## De-duplication, ported from legacy
///
/// Legacy keys its universe map on `(plan.source, plan.repo_id, test_file)`
/// (`analytics.rs:899-903`), so a file listed by two plans in the same
/// repository collapses to **one** row that keeps the FIRST plan's identity
/// and name (`or_insert_with` at `:904-920`). The later occurrences are not
/// discarded outright: they fill in a `component` or `title_alias` the first
/// one lacked and union their tags in (`analytics.rs:922-934`). Both halves
/// are reproduced below — dropping the merge would lose metadata whenever the
/// first plan to mention a file happens to be the one with less of it.
///
/// `source` is constant here, so the key reduces to `(repo_id, test_file)`;
/// `repo_id` is constant within one call, leaving `test_file`.
fn walk_repo_universe(repo_id: Uuid, root: &Path) -> Vec<UniverseTest> {
    let mut by_file: HashMap<String, UniverseTest> = HashMap::new();
    // Insertion order, so the result is a function of the (already sorted)
    // plan and test-file order rather than of hash iteration.
    let mut order: Vec<String> = Vec::new();

    for (plan_path, plan) in discover_plans(root) {
        for test_file in &plan.test_files {
            // `test_files` is already normalized by the plan parser, which
            // ports legacy's `normalize_test_path` byte-for-byte.
            //
            // This function returns `Vec<UniverseTest>`, not a `Result`, so a
            // fault here still cannot fail the whole universe listing (that
            // would turn one bad file into an outage for an analytics read,
            // a bigger behaviour change than any review finding asked for —
            // see finding #28's fix for the same shape). Absence is the only
            // silent case now: an out-of-root escape and any other IO fault
            // are both warned (distinctly from each other and from absence)
            // rather than folded into "absent" (review finding #8; see
            // `read_universe_test_file`'s doc for the three-way split).
            //
            // One read per file, feeding both parsers below — the reason
            // `case_count` lives beside `test_meta`.
            let Some(content) = read_universe_test_file(repo_id, root, test_file) else {
                continue;
            };
            let meta = parse_test_meta(&content);
            let static_case_count =
                u32::try_from(count_test_functions(&content)).unwrap_or(u32::MAX);

            let display_name = meta
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map_or_else(|| fallback_test_name(test_file), ToOwned::to_owned);

            // TEST_META tags unioned with the plan's own, trimmed, blanks
            // dropped. `BTreeSet` because legacy uses one (`analytics.rs:891`)
            // — so the order is sorted, not the file's.
            let tags: BTreeSet<String> = meta
                .tags
                .iter()
                .chain(plan.tags.iter())
                .map(|tag| tag.trim())
                .filter(|tag| !tag.is_empty())
                .map(ToOwned::to_owned)
                .collect();

            let component = component_for(&meta, test_file);

            match by_file.entry(test_file.clone()) {
                std::collections::hash_map::Entry::Occupied(mut existing) => {
                    let entry = existing.get_mut();
                    // Legacy `analytics.rs:922-929`: only fill what is missing.
                    if entry.component.is_none() {
                        entry.component = component;
                    }
                    // Legacy `analytics.rs:927-929`, ported including the fact
                    // that it can never fire: the vacant arm below defaults
                    // `title_alias` to the display name, so it is never `None`
                    // on an existing row. Kept so the merge stays a line-for-
                    // line mirror of legacy's — if the default at insert ever
                    // changes, this is already the correct behavior.
                    if entry.title_alias.is_none() {
                        entry.title_alias.clone_from(&meta.title);
                    }
                    // Legacy `analytics.rs:930-934` unions tags. Quality
                    // vectors get the same treatment: legacy accumulates them
                    // per file across every plan that names the file
                    // (`analytics.rs:871-882`), so a second plan's vectors
                    // must not be dropped either.
                    for tag in tags {
                        if !entry.tags.contains(&tag) {
                            entry.tags.push(tag);
                        }
                    }
                    for vector in meta.quality_vectors {
                        if !entry
                            .quality_vectors
                            .iter()
                            .any(|seen| seen.eq_ignore_ascii_case(&vector))
                        {
                            entry.quality_vectors.push(vector);
                        }
                    }
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    order.push(test_file.clone());
                    slot.insert(UniverseTest {
                        repo_id,
                        plan_path: plan_path.clone(),
                        plan_name: plan.name.clone(),
                        test_file: test_file.clone(),
                        test_name: display_name.clone(),
                        // Legacy `analytics.rs:909`: the title when present,
                        // otherwise the display name — never `None` on a
                        // first insert. `build_alias_map` (`:1738`) registers
                        // it, so a `None` here turns title-named execution
                        // rows into `not_run`.
                        title_alias: meta.title.clone().or(Some(display_name)),
                        component,
                        tags: tags.into_iter().collect(),
                        quality_vectors: meta.quality_vectors,
                        source: SOURCE_REPO.to_owned(),
                        // Legacy parity: never populated there either. See
                        // `UniverseTest::versions`.
                        versions: Vec::new(),
                        static_case_count,
                    });
                }
            }
        }
    }

    order
        .into_iter()
        .filter_map(|file| by_file.remove(&file))
        .collect()
}

/// Resolve `test_file` under `root` and read its content for
/// [`walk_repo_universe`]. `None` means "drop this entry from the universe",
/// for one of three reasons: a plain absence (`Ok(None)`), which stays
/// silent as it always has -- a plan is free to list a file that does not
/// exist on every branch; an out-of-root escape (`Err(Validation)`, from
/// `resolve_under_root`'s `starts_with(root)` check), which is warned as its
/// own specific, actionable shape rather than an ordinary fault; or any
/// other fault (EACCES, a mid-read IO failure), warned with the error so it
/// is not misread as an absence. Split out of `walk_repo_universe` to keep
/// that function under the cognitive-complexity lint's threshold. Review
/// finding #8.
fn read_universe_test_file(repo_id: Uuid, root: &Path, test_file: &str) -> Option<String> {
    let resolved = match resolve_under_root(root, test_file) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(error) => {
            warn_universe_resolve_error(repo_id, test_file, error);
            return None;
        }
    };
    match std::fs::read_to_string(&resolved) {
        Ok(content) => Some(content),
        Err(error) if io_is_absent(&error) => None,
        Err(error) => {
            warn!(
                repo_id = %repo_id, test_file = %test_file, error = %error,
                "Skipping universe test file: could not read file"
            );
            None
        }
    }
}

/// Warn for a [`resolve_under_root`] failure inside [`read_universe_test_file`],
/// distinguishing an out-of-root escape (its own specific, actionable shape)
/// from any other fault (EACCES, ...). Split out to keep that function under
/// the cognitive-complexity lint's threshold.
fn warn_universe_resolve_error(repo_id: Uuid, test_file: &str, error: DomainError) {
    match error {
        DomainError::Validation { .. } => {
            warn!(
                repo_id = %repo_id, test_file = %test_file,
                "Skipping universe test file: escapes the content root"
            );
        }
        error => {
            warn!(
                repo_id = %repo_id, test_file = %test_file, error = %error,
                "Skipping universe test file: could not resolve under content root"
            );
        }
    }
}

/// `TEST_META`'s `component`, else inferred from the path.
///
/// Legacy `infer_component_from_path` (`analytics.rs:1794-1803`): the second
/// segment of a `tests/<component>/...` path, trimmed, and only when the
/// first segment is literally `tests`.
fn component_for(meta: &ParsedTestMeta, test_file: &str) -> Option<String> {
    if let Some(component) = meta.component.clone() {
        return Some(component);
    }
    let mut parts = test_file.split('/');
    match (parts.next(), parts.next()) {
        (Some("tests"), Some(component)) if !component.trim().is_empty() => {
            Some(component.trim().to_owned())
        }
        _ => None,
    }
}

/// Display name for a file with no usable `TEST_META` title.
///
/// Legacy `fallback_test_name` (`analytics.rs:1805-1812`): take the file
/// stem, trim it, strip a leading `test_`, and turn `_` into spaces — so
/// `tests/api/test_login_flow.py` reads as `login flow`.
fn fallback_test_name(test_path: &str) -> String {
    let stem = Path::new(test_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(test_path)
        .trim();
    stem.trim_start_matches("test_").replace('_', " ")
}

fn to_sdk_plan(
    repo_id: Uuid,
    product_id: Uuid,
    branch: &str,
    path: String,
    parsed: ParsedPlan,
) -> Plan {
    Plan {
        repo_id,
        product_id,
        branch: branch.to_owned(),
        path,
        name: parsed.name,
        test_files: parsed.test_files,
        // Always `Some`: the parser applies the legacy 300s default. The SDK
        // field is optional only because a *custom* plan may carry no timeout.
        timeout_seconds: Some(parsed.timeout_seconds),
        tags: parsed.tags,
        validation: parsed.validation,
        exclusive: Exclusivity::from_option_bool(parsed.exclusive),
    }
}

// ---------------------------------------------------------------------------
// Shared working-copy helpers (also used by `super::repos` / `super::bundles`)
// ---------------------------------------------------------------------------

/// Whether an IO error means "this path is not there" as opposed to "this
/// path could not be read".
///
/// The distinction is load-bearing in this module: the not-there answers are
/// `PlanNotFound` / `FileNotFound` / `Ok(None)`, which the REST layer renders
/// 404 and which the exclusivity tier reads as "this file has no opinion". An
/// `EACCES`, a `NotADirectory` or a mid-read IO failure answered the same
/// way, so a misconfigured snapshot directory presented as a missing plan
/// and an unreadable test file silently dropped its exclusivity vote.
///
/// `domain::service::repos` already draws this line at `:297` and `:336`.
/// This is the same rule, named once so the sites below cannot drift.
/// Review findings #6, #7, #8, #26.
fn io_is_absent(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

/// Validate a client-supplied relative path: non-empty, relative, no `..`
/// (or root/prefix) components, no backslashes. Rejects with
/// [`DomainError::Validation`] so `../`-style traversal can never reach the
/// filesystem join.
pub(super) fn validate_rel_path(field: &str, raw: &str) -> Result<(), DomainError> {
    let invalid = |message: &str| DomainError::Validation {
        field: field.to_owned(),
        message: format!("{message}: '{raw}'"),
    };

    if raw.is_empty() {
        return Err(invalid("path must not be empty"));
    }
    // On non-Windows hosts a backslash is not a separator, so "..\\x" would
    // pass the component check below as a single opaque name; reject it
    // outright rather than letting platform differences decide.
    if raw.contains('\\') {
        return Err(invalid("path must not contain backslashes"));
    }

    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(invalid("path must be relative"));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(invalid("path must not traverse outside the repository"));
            }
        }
    }
    Ok(())
}

/// Require the repository's working area to be usable for `branch`.
///
/// Under the multi-branch model any branch is readable once the repository
/// has synced successfully at least once — the per-branch snapshot's actual
/// presence is checked by [`content_root_dir`], which canonicalizes it.
///
/// `sync_error` is repository-scoped, not per-branch: there is no per-branch
/// state store (a deliberate parity choice — the source system's per-branch
/// cache records freshness only, never errors). So a failed sync of branch B
/// also fails reads of an otherwise-healthy branch A until the next
/// successful sync. Accepted; fixing it needs a persisted per-branch table.
pub(super) fn require_synced(repo: &TestRepository, branch: &str) -> Result<(), DomainError> {
    let synced = repo.last_synced_at.is_some() && repo.sync_error.is_none();
    if synced {
        Ok(())
    } else {
        Err(DomainError::RepoNotSynced {
            repo_id: repo.id,
            branch: branch.to_owned(),
        })
    }
}

/// Resolve and canonicalize the content-root directory for `branch`
/// (`<repos_dir>/<repo_id>/branches/<branch_dir>/<content_root>`), verifying
/// it stays inside that branch's snapshot. A missing directory (never synced
/// for this branch, or a wiped data dir) reads as
/// [`DomainError::RepoNotSynced`]; a directory that exists but could not be
/// resolved (EACCES, ...) is [`DomainError::Internal`] instead — that case is
/// a fault, not evidence the branch was never synced. Review finding #26.
pub(super) fn content_root_dir(
    repos_dir: &Path,
    repo: &TestRepository,
    branch: &str,
) -> Result<PathBuf, DomainError> {
    let not_synced = || DomainError::RepoNotSynced {
        repo_id: repo.id,
        branch: branch.to_owned(),
    };

    let workdir = crate::infra::git::layout::branch_workdir(repos_dir, repo.id, branch);
    let root = if repo.content_root.is_empty() {
        workdir.clone()
    } else {
        // Stored value is validated on create, but re-validate on every use:
        // defense in depth against rows written by older code paths.
        validate_rel_path("content_root", &repo.content_root)?;
        workdir.join(&repo.content_root)
    };

    // `RepoNotSynced` is right when the path is genuinely absent (never
    // synced, or a wiped data dir); anything else (EACCES, ...) is a fault
    // and must not be told to the caller as "not synced". Review finding #26.
    let canonical_workdir = match workdir.canonicalize() {
        Ok(path) => path,
        Err(error) if io_is_absent(&error) => return Err(not_synced()),
        Err(error) => {
            return Err(DomainError::Internal(format!(
                "repository {} working directory exists but could not be resolved: {error}",
                repo.id
            )));
        }
    };
    let canonical_root = match root.canonicalize() {
        Ok(path) => path,
        Err(error) if io_is_absent(&error) => return Err(not_synced()),
        Err(error) => {
            return Err(DomainError::Internal(format!(
                "repository {} content root exists but could not be resolved: {error}",
                repo.id
            )));
        }
    };
    if !canonical_root.starts_with(&canonical_workdir) {
        return Err(DomainError::Validation {
            field: "content_root".to_owned(),
            message: "resolves outside the repository working directory".to_owned(),
        });
    }
    if !canonical_root.is_dir() {
        return Err(not_synced());
    }
    Ok(canonical_root)
}

/// Resolve `rel` under the canonicalized `root`, enforcing containment (this
/// also neutralizes symlinks inside the working copy that point outside it).
/// `Ok(None)` means the file does not exist; escapes are a `Validation`
/// error; any other non-absent IO failure (EACCES, ...) is `Internal`
/// (review finding #8).
pub(super) fn resolve_under_root(root: &Path, rel: &str) -> Result<Option<PathBuf>, DomainError> {
    validate_rel_path("path", rel)?;
    let joined = root.join(rel);
    let canonical = match joined.canonicalize() {
        Ok(canonical) => canonical,
        // Only "not there" is `Ok(None)`. An `EACCES` here used to read as
        // "this file does not exist", which the caller turns into a 404 and
        // the exclusivity tier turns into a dropped vote.
        Err(error) if io_is_absent(&error) => return Ok(None),
        Err(error) => {
            return Err(DomainError::Internal(format!(
                "path '{}' could not be resolved: {error}",
                joined.display()
            )));
        }
    };
    if !canonical.starts_with(root) {
        return Err(DomainError::Validation {
            field: "path".to_owned(),
            message: format!("path escapes the repository content root: '{rel}'"),
        });
    }
    Ok(Some(canonical))
}

// ---------------------------------------------------------------------------
// Discovery (blocking; run via `spawn_blocking`)
// ---------------------------------------------------------------------------

/// Discover plans under `root`, returning `(path-relative-to-root, parsed)`
/// pairs, **sorted by path and de-duplicated**. See the module docs for the
/// mirrored legacy pattern and the ordering contract.
fn discover_plans(root: &Path) -> Vec<(String, ParsedPlan)> {
    let mut plans = scan_file_based_plans(root);
    plans.extend(scan_legacy_directory_plans(root));

    // De-duplicate on the plan's identity (its path), keeping the FIRST
    // occurrence — i.e. the file-based flavor wins, exactly as legacy's
    // `scan_root` ordering does (plans.rs:470-499). `<root>/plans/plan.yaml`
    // is the one input both flavors can reach.
    let mut seen = HashSet::new();
    plans.retain(|(path, _)| seen.insert(path.clone()));

    // Deterministic order (`read_dir` is platform- and filesystem-dependent).
    plans.sort_by(|a, b| a.0.cmp(&b.0));
    plans
}

/// `<root>/plans/*.yaml` — flat, non-recursive (legacy plans.rs:502-590).
fn scan_file_based_plans(root: &Path) -> Vec<(String, ParsedPlan)> {
    let plans_root = root.join("plans");
    if !plans_root.is_dir() {
        return Vec::new();
    }

    let Some(entries) = read_dir_or_warn(&plans_root) else {
        return Vec::new();
    };

    let mut plans = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(parsed) = load_plan_file(&path) {
            plans.push((format!("plans/{name}"), parsed));
        }
    }
    plans
}

/// `<root>/<dir>/plan.yaml`, else `<root>/<dir>/<subdir>/plan.yaml`
/// (legacy plans.rs:592-661; depth-2 only when depth-1 has no plan.yaml).
fn scan_legacy_directory_plans(root: &Path) -> Vec<(String, ParsedPlan)> {
    let Some(top_entries) = read_dir_or_warn(root) else {
        return Vec::new();
    };

    let mut plans = Vec::new();
    for top_entry in top_entries.flatten() {
        let top_path = top_entry.path();
        if !top_path.is_dir() {
            continue;
        }
        let Some(top_name) = top_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        let plan_file = top_path.join("plan.yaml");
        if plan_file.is_file() {
            if let Some(parsed) = load_plan_file(&plan_file) {
                let parsed = with_derived_test_files(root, top_name, parsed);
                plans.push((format!("{top_name}/plan.yaml"), parsed));
            }
            continue;
        }

        let Some(sub_entries) = read_dir_or_warn(&top_path) else {
            continue;
        };
        for sub_entry in sub_entries.flatten() {
            let sub_path = sub_entry.path();
            if !sub_path.is_dir() {
                continue;
            }
            let Some(sub_name) = sub_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let sub_plan_file = sub_path.join("plan.yaml");
            if sub_plan_file.is_file()
                && let Some(parsed) = load_plan_file(&sub_plan_file)
            {
                let plan_dir = format!("{top_name}/{sub_name}");
                let parsed = with_derived_test_files(root, &plan_dir, parsed);
                plans.push((format!("{plan_dir}/plan.yaml"), parsed));
            }
        }
    }
    plans
}

/// Derive a *directory* plan's test-file list from disk when its `plan.yaml`
/// carried no `tests:` — legacy `load_plan_from_dir`
/// (testrunner `manager/src/services/plans.rs:685-692`). `plan_dir` is the
/// plan directory's path relative to the content root (`infra`,
/// `product/upgrade`, …); a plan that lists its own files is returned
/// untouched (legacy plans.rs:693-699).
///
/// Only the *directory* flavor derives: file-based `plans/*.yaml` documents
/// keep an empty list, exactly as legacy's `scan_file_based_plans`
/// (plans.rs:501-589) leaves them.
fn with_derived_test_files(root: &Path, plan_dir: &str, mut parsed: ParsedPlan) -> ParsedPlan {
    if parsed.test_files.is_empty() {
        parsed.test_files = list_test_files(root, plan_dir);
    }
    parsed
}

/// Every `test_*.py` file under `<root>/<plan_dir>/tests/**` whose contents
/// contain the literal `TEST_META`, as paths relative to `root`, ascending.
///
/// Legacy `list_test_files` (plans.rs:724-729) → `collect_tests_recursively`
/// (plans.rs:731-770): the name filter is at plans.rs:760, the `TEST_META`
/// content filter at plans.rs:763 (`file_contains_test_meta`, plans.rs:776-780).
/// The content filter is legacy's `require_test_meta`, set from `repo_mode`
/// — always true here: this gear only ever discovers inside a synced
/// repository working copy (legacy's local-plans mode has no counterpart).
///
/// Derived paths go through exactly the same validation a client-supplied
/// path does ([`resolve_under_root`]), so a symlinked subtree inside `tests/`
/// cannot smuggle out-of-root content into a plan.
fn list_test_files(root: &Path, plan_dir: &str) -> Vec<String> {
    let tests_dir = root.join(plan_dir).join("tests");
    let mut files = Vec::new();
    collect_test_files(root, &tests_dir, &format!("{plan_dir}/tests"), &mut files);
    files.sort();
    files
}

/// Recursive half of [`list_test_files`]. `prefix` is `current`'s path
/// relative to `root`.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "frozen contract: legacy matches `test_*` + `.py` case-SENSITIVELY on the raw \
              file name (plans.rs:760), so `TEST_A.PY` is not a test file there either"
)]
fn collect_test_files(root: &Path, current: &Path, prefix: &str, files: &mut Vec<String>) {
    let Some(entries) = read_dir_or_warn(current) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Symlinks are skipped outright (mirrors the bundle walk): a link
        // inside the working copy must not pull outside content into a plan.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_test_files(root, &path, &format!("{prefix}/{name}"), files);
            continue;
        }
        if !file_type.is_file() || !name.starts_with("test_") || !name.ends_with(".py") {
            continue;
        }
        if !file_contains_test_meta(&path) {
            continue;
        }

        let rel = normalize_test_path(&format!("{prefix}/{name}"));
        record_derived_test_file(root, &path, rel, files);
    }
}

/// `std::fs::read_dir`, shared by every directory read in the discovery walk
/// (`collect_test_files`'s recursive test-file walk, `scan_file_based_plans`,
/// both levels of `scan_legacy_directory_plans`).
///
/// None of those callers return `Result` (see each one's own doc comment for
/// why cascading one would be a bigger behaviour change than any finding
/// asked for), so this cannot propagate either. Named once so the rule
/// cannot drift between the four call sites the way it did before review
/// finding #8's fix round 1 -- three of the four warned inconsistently (one
/// not at all, two without the IO error) before being unified here. An
/// absent directory (never materialized) stays a silent empty result;
/// anything else is warned with the error so an unreadable *directory* does
/// not read as "nothing here".
fn read_dir_or_warn(dir: &Path) -> Option<std::fs::ReadDir> {
    match std::fs::read_dir(dir) {
        Ok(entries) => Some(entries),
        Err(error) if io_is_absent(&error) => None,
        Err(error) => {
            warn!(dir = %dir.display(), error = %error, "Skipping unreadable directory");
            None
        }
    }
}

/// Resolve a derived test path back under `root` and record it, for
/// [`collect_test_files`]. Split out to keep that function under the
/// cognitive-complexity lint's threshold.
///
/// Three distinct outcomes, not two: `Ok(None)` means the path is genuinely
/// absent on this branch; `Err(Validation)` (from `resolve_under_root`'s own
/// `starts_with(root)` check, plans.rs:763-768) means it resolved to a
/// symlink escape -- that is what "outside the content root" actually
/// describes, and is worth its own line; anything else (e.g. EACCES) is a
/// plain fault naming the IO error. An earlier version of this fix answered
/// `Ok(None) | Err(_)` with the escape message, which put that label on the
/// absent case and lost it on the real escape (review finding #8, corrected
/// in fix round 1).
fn record_derived_test_file(root: &Path, path: &Path, rel: String, files: &mut Vec<String>) {
    match resolve_under_root(root, &rel) {
        Ok(Some(_)) => files.push(rel),
        Ok(None) => {
            warn!(file = %path.display(), "Skipping derived test path: not present under the content root");
        }
        Err(error) => warn_derived_resolve_error(path, error),
    }
}

/// Warn for a [`resolve_under_root`] failure inside [`record_derived_test_file`],
/// distinguishing an out-of-root escape (its own specific, actionable shape --
/// what "outside the content root" actually describes) from any other fault
/// (e.g. EACCES). Split out to keep that function under the
/// cognitive-complexity lint's threshold.
fn warn_derived_resolve_error(path: &Path, error: DomainError) {
    match error {
        DomainError::Validation { .. } => {
            warn!(file = %path.display(), "Skipping derived test path outside the content root");
        }
        error => {
            warn!(
                file = %path.display(), error = %error,
                "Skipping derived test path: could not resolve under content root"
            );
        }
    }
}

/// Legacy `file_contains_test_meta` (plans.rs:776-780): an unreadable file
/// simply does not qualify -- true only when the file is genuinely absent.
/// This still returns `bool`, not `Result`: propagating would cascade
/// through `list_test_files` and `collect_test_files` up to the plan-parsing
/// call at plans.rs:775, failing an entire plan over one unreadable test
/// file (the same tradeoff `collect_test_files` and `walk_repo_universe`
/// avoid). A non-absent failure (EACCES, ...) is warned instead of silently
/// answering `false`, so it is not misread as "no `TEST_META` here". Review
/// findings #6, #7, #8, #26.
fn file_contains_test_meta(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(content) => content.contains("TEST_META"),
        Err(error) if io_is_absent(&error) => false,
        Err(error) => {
            warn!(file = %path.display(), error = %error, "Could not read test file to check for TEST_META");
            false
        }
    }
}

/// Read + parse one plan file; `None` (with a warning) on any failure —
/// skip-and-log so one bad plan never hides the rest.
fn load_plan_file(path: &Path) -> Option<ParsedPlan> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            warn!(file = %path.display(), error = %err, "Failed to read plan file, skipping");
            return None;
        }
    };
    match parse_plan_yaml(&content) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            warn!(file = %path.display(), error = %err, "Failed to parse plan file, skipping");
            None
        }
    }
}
