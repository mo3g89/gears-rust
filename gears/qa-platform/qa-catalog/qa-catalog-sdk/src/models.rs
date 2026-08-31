//! Transport-agnostic models for the qa-catalog contract.

use time::OffsetDateTime;
use uuid::Uuid;

/// Registered git test repository.
#[derive(Clone, Debug, PartialEq)]
pub struct TestRepository {
    pub id: Uuid,
    /// Owning product. Required — repo ownership attributes discovered plans
    /// to a product.
    pub product_id: Uuid,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    /// Subdirectory within the repo that contains test content ("" = root).
    pub content_root: String,
    /// credstore reference for the access credential (SSH key or token). None = public repo.
    pub credential_ref: Option<String>,
    pub last_synced_at: Option<OffsetDateTime>,
    pub sync_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewTestRepository {
    /// Owning product. Required — repo ownership attributes discovered plans
    /// to a product.
    pub product_id: Uuid,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    pub content_root: String,
    pub credential_ref: Option<String>,
}

/// Mutable fields of a registered test repository — full replace, no
/// tri-state patch semantics (`credential_ref: None` *clears* the reference).
///
/// `default_branch` is mutable. It selects the branch used when a caller
/// names none; it does not identify the repository's synced content, so
/// changing it invalidates nothing already materialized.
#[derive(Clone, Debug, PartialEq)]
pub struct TestRepositoryUpdate {
    /// Owning product. Required — repo ownership attributes discovered plans
    /// to a product.
    pub product_id: Uuid,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    /// Subdirectory within the repo that contains test content ("" = root).
    pub content_root: String,
    /// credstore reference for the access credential. None = public repo.
    pub credential_ref: Option<String>,
}

/// Three-state exclusivity: None = inherit (NOT the same as Some(false)).
/// This distinction is load-bearing — see PRD cpt-cf-qa-fr-runs-exclusivity.
pub type ExclusiveFlag = Option<bool>;

/// A plan discovered from a repository's plan.yaml (not persisted — materialized on read).
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub repo_id: Uuid,
    /// Owning product, resolved from the repository that holds this plan.
    pub product_id: Uuid,
    pub branch: String,
    /// Path of the plan.yaml within the content root.
    pub path: String,
    pub name: String,
    pub test_files: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub tags: Vec<String>,
    /// Whether the plan classifies as a *validation* run: the `plan.yaml`
    /// `validation:` bool OR'd with a trimmed, case-insensitively matched
    /// `validation` tag (`manager/src/services/plans.rs:35-39`). Consumed by
    /// qa-runs, which re-homes it as a run classification.
    pub validation: bool,
    pub exclusive: ExclusiveFlag,
}

/// Parsed `TEST_META` for one test file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestFileMeta {
    pub path: String,
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub exclusive: ExclusiveFlag,
    /// JIRA issue keys referenced by the meta block (e.g. "VHP-123").
    pub bugs: Vec<String>,
}

/// One test-file reference inside a [`CustomPlan`].
///
/// # Why `plan_path` exists
///
/// Legacy's equivalent is `CustomPlanTest { plan_id, test_file }`
/// (`manager/src/models.rs:484-487`), whose `plan_id` names the nested
/// `plan.yaml` the file belongs to. That association is load-bearing for
/// exclusivity, not decoration: `custom_plan_tier` groups a custom plan's
/// entries by it, consults each nested plan's own `exclusive:` declaration
/// first, and only falls through to a `TEST_META` scan for the nested plans
/// that declared nothing (`manager/src/services/exclusivity.rs:536-564`,
/// combined at `:579-582`). An earlier version of this model carried
/// `(repo_id, path)` pairs with no plan reference at all, so a custom plan
/// composed of a plan declaring `exclusive: true` whose test files were all
/// silent resolved **parallel** here and **exclusive** there — a destructive
/// suite losing its platform-to-itself guarantee with no error and no warning.
///
/// # Rejected alternative: `plan_id: Uuid`
///
/// The obvious mirror of legacy's field, and it cannot work here. A plan in this
/// port is **not persisted** — [`Plan`] is materialized on read and there is no
/// plans table — so it has no UUID to reference. Its identity is
/// `(repo_id, branch, path)`, exactly what
/// [`QaCatalogClientV1::get_plan`](crate::QaCatalogClientV1::get_plan) takes.
/// `repo_id` is already on this entry and `branch` comes from the launch, so the
/// plan's `path` is the whole of the missing key. A `Uuid` here would be a
/// reference resolvable against nothing.
///
/// # Rejected alternative: a `(Uuid, String, Option<String>)` triple
///
/// It keeps the old positional shape and needs no new type, but the shape is
/// what has to stay backward compatible: every stored `qa_custom_plans.files`
/// row holds a **two**-element array, and `Vec<(Uuid, String, Option<String>)>`
/// rejects one outright. Named fields are what let the storage codec accept both
/// the old and the new payload — see `infra::storage::mapper`'s
/// `custom_plan_entries_from_json`, which owns that contract and is tested
/// against a literal old-shape payload.
///
/// # Required to write, optional to read — and that asymmetry is deliberate
///
/// **This is the read model, and its `plan_path` is `Option` because stored rows
/// legitimately have none.** [`NewCustomPlanEntry`] is the write model and its
/// `plan_path` is a plain `String`: since 2026-08-14 the API refuses an entry
/// that names no plan, because legacy's `CustomPlanTest::plan_id: String` is
/// non-optional and optional-on-write was itself the divergence.
///
/// So `None` means exactly one thing now: **a row written before the field
/// existed**. Nothing reachable through the API can produce another one.
///
/// The asymmetry has a trap in it, and it is the whole risk of the change.
/// `infra::storage::mapper`'s `StoredCustomPlanEntry` **must stay permissive** —
/// its `#[serde(default)] plan_path` is what lets the old two-element array
/// decode at all. Tightening *that* field to match the write type turns every
/// custom plan stored before this change into
/// `Internal("corrupt qa_custom_plans.files …")`, which is the exact failure the
/// compatibility work exists to prevent; a break-test on that mutation is
/// checked in. Three shapes, three different rules, on purpose:
///
/// | type | `plan_path` | why |
/// |---|---|---|
/// | [`NewCustomPlanEntry`] | `String` | legacy requires it; a 400 is better than a silent `None` |
/// | [`CustomPlanEntry`] | `Option<String>` | pre-field rows have none |
/// | `StoredCustomPlanEntry` | `Option<String>`, defaulted | must decode a payload it did not write |
///
/// # What is left of the divergence this field closed
///
/// For an entry whose `plan_path` is `None` there is still no plan tier, so a
/// nested plan declaring `exclusive: true` with silent test files still resolves
/// parallel at tier `TestMeta` instead of exclusive at tier `Plan`. **That now
/// applies only to rows written before the field existed** — it is not an ongoing
/// possibility, because the write path rejects a missing `plan_path`.
///
/// Such a row keeps the old answer until it is re-saved. **A backfill is not
/// available, and not merely unimplemented:** recovering `plan_path` would mean
/// searching the repository for a `plan.yaml` listing that file, and a file may
/// legitimately appear in several plans — so the answer is a guess, and guessing
/// wrong in the permissive direction is the exact failure this field prevents.
/// Legacy never needed one because its `plan_id` was mandatory from the start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomPlanEntry {
    /// The repository holding both the test file and, when named, its plan.
    pub repo_id: Uuid,
    /// Test-file path under the repository's content root. May carry a pytest
    /// node-id selector (`tests/a.py::Case::method`).
    pub path: String,
    /// Path of the `plan.yaml` that lists this file, within the same
    /// repository's content root. `None` = a row written before the field
    /// existed; see this type's "Required to write, optional to read".
    pub plan_path: Option<String>,
}

/// One test-file reference **being written** to a custom plan.
///
/// Identical to [`CustomPlanEntry`] except that `plan_path` is mandatory. Two
/// types rather than one `Option` validated at the service, because the
/// requirement then holds for every caller including ones that bypass the
/// service: `CustomPlansRepository::create` takes a [`NewCustomPlan`] and does no
/// validation of its own, so a service-level check alone is routed around by
/// anything that reaches the repository directly. Here it is unrepresentable.
///
/// # Rejected alternative: keep one entry type and validate
///
/// Fewer types, and it was how the first cut of this worked. Rejected for the
/// bypass above, and because the read/write difference is a *fact about the
/// contract* — two named types say it where a comment on a shared `Option` only
/// describes it. The cost is one conversion, [`From<NewCustomPlanEntry>`], which
/// is a widening and cannot fail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewCustomPlanEntry {
    pub repo_id: Uuid,
    /// Test-file path under the repository's content root. May carry a pytest
    /// node-id selector (`tests/a.py::Case::method`).
    pub path: String,
    /// Path of the `plan.yaml` that lists this file, in the same repository.
    /// **Required** — see [`CustomPlanEntry`]'s "Required to write, optional to
    /// read" for why, and for the storage struct this must *not* be copied onto.
    pub plan_path: String,
}

impl From<NewCustomPlanEntry> for CustomPlanEntry {
    /// A write entry is a read entry with the option filled in. Widening only —
    /// there is deliberately no conversion the other way, because a stored entry
    /// may have no `plan_path` and inventing one is the guess the backfill
    /// argument on [`CustomPlanEntry`] rejects.
    fn from(new: NewCustomPlanEntry) -> Self {
        Self {
            repo_id: new.repo_id,
            path: new.path,
            plan_path: Some(new.plan_path),
        }
    }
}

/// User-composed persisted plan.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomPlan {
    pub id: Uuid,
    pub name: String,
    /// Test-file references — may span repositories, and may name the nested
    /// plan each file belongs to. See [`CustomPlanEntry`].
    pub files: Vec<CustomPlanEntry>,
    pub tags: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewCustomPlan {
    pub name: String,
    /// **Operator-supplied in full, `plan_path` included and mandatory.** Unlike a
    /// discovered plan's `exclusive`, nothing in the catalog can infer which
    /// `plan.yaml` lists a hand-picked file, so the value has to arrive from the
    /// caller — see [`NewCustomPlanEntry`], and [`CustomPlanEntry`] for why the
    /// read model still permits its absence.
    pub files: Vec<NewCustomPlanEntry>,
    pub tags: Vec<String>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Product {
    pub id: Uuid,
    pub name: String,
    /// Durable short code (legacy `Product::key`). Persisted as
    /// `qa_products.product_key`.
    pub key: String,
    pub description: String,
    pub folder: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SshKey {
    pub id: Uuid,
    pub name: String,
    pub credstore_ref: String,
    pub fingerprint: String,
    pub created_at: OffsetDateTime,
}

/// Descriptor of a built ephemeral bundle (blob lives in the bundle store).
#[derive(Clone, Debug, PartialEq)]
pub struct TestBundle {
    pub id: Uuid,
    /// Opaque reference into the bundle store (local path or file-storage ref).
    pub storage_ref: String,
    pub checksum_sha256: String,
    pub size_bytes: u64,
    pub expires_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
}

/// What a bundle should contain — resolved by qa-runs at launch time.
#[derive(Clone, Debug, PartialEq)]
pub struct BundleRequest {
    pub repo_id: Uuid,
    pub branch: String,
    /// Files to include (paths under `content_root`). Empty = whole content root.
    pub files: Vec<String>,
}

/// What to sync, and how hard.
///
/// A struct rather than two positional arguments on
/// [`crate::client::QaCatalogClientV1::sync_repo`]: it matches this trait's
/// dominant idiom (`create_bundle(ctx, req: BundleRequest)`), it names the
/// bare `true` that `sync_repo(ctx, id, branch, true)` leaves opaque at a
/// call site, and a third sync knob can be added without a breaking
/// signature change. DECOMPOSITION 2.2 sanctions either shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncRequest {
    /// Branch to materialize. `None` uses the repository's `default_branch`,
    /// matching the source system (`manager/src/services/test_repos.rs:498-502`).
    pub branch: Option<String>,
    /// Skip (and evict) the freshness TTL entry. The launch path always sets
    /// this: the source system's launch drops the recency marker before
    /// syncing so a cached checkout cannot be returned
    /// (`test_repos.rs:503-508`, called from `routes/runs.rs:649`).
    pub force: bool,
}

/// One test file as the analytics universe sees it.
///
/// A **projection for qa-insights**, not a catalog concept. It exists because
/// analytics needs each file's `component` / `tags` / `quality_vectors` and an
/// expected case count, and the gear split (ADR-0005) puts the repository
/// content on this side of the boundary. Legacy read all of it off the
/// checkout directly — `UniverseTest`, `manager/src/routes/analytics.rs:250-264`,
/// built in the plan walk at `analytics.rs:904-920`.
///
/// # Divergences from legacy's `UniverseTest`, and why
///
/// Verified field-by-field against `analytics.rs:251-264` on 2026-08-18. Legacy
/// carries **eleven** fields; this carries twelve. Four differences, each
/// deliberate:
///
/// 1. **`quality_vectors` is here and not there.** Legacy does not keep them on
///    `UniverseTest` at all — its walk parses them per file and folds them into
///    a *separate* `quality_vectors_by_file` map (`analytics.rs:839`,
///    `:871-882`) which becomes `QualityVectorSummary` (`:938`). That map is
///    built on the same checkout read, which qa-insights does not have, so the
///    per-file vectors have to ride across the boundary on the only row that
///    crosses it. Dropping this field does not fail to compile; it silently
///    empties the overview's quality-vector summary.
/// 2. **`plan_id: Uuid` is replaced by `plan_path` + `repo_id`.** The plan's
///    draft specified `plan_id: Uuid`. There is nothing to put in it: a plan in
///    this port is **not persisted** — [`Plan`] is materialized on read, there
///    is no plans table, and its identity is `(repo_id, branch, path)`. This is
///    the same conclusion [`CustomPlanEntry`] reached and documented, so the
///    same shape is used here rather than minting a synthetic UUID that would
///    resolve against nothing. Legacy's own `plan_id` is not a UUID either: it
///    is a path-derived slug, `format!("{repo}-{dir_path with / → -}")` run
///    through `sanitize_k8s` (`manager/src/services/plans.rs:789-801`), so
///    `plan_path` is closer to legacy's meaning than a `Uuid` would be.
/// 3. **`repo_id` is `Uuid`, not `Option<Uuid>`.** Legacy's is optional because
///    its plan index also holds *local* plans scanned from a plans directory
///    with no repository (`source: "local"`, `manager/src/services/plans.rs:56`).
///    This gear has no local-plans mode — every discovered plan comes out of a
///    synced repository working copy — so an `Option` that is never `None` would
///    only push an `unwrap` onto qa-insights.
/// 4. **`case_count` is renamed `static_case_count`** (and widened from `usize`
///    to `u32` for the wire). Same value; the name says out loud that it is the
///    static estimate, not the collect job's exact count.
///
/// The remaining eight fields — `plan_name`, `test_file`, `test_name`,
/// `title_alias`, `component`, `tags`, `source`, `versions` — match legacy
/// one-for-one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UniverseTest {
    /// The repository the plan and the file both live in.
    ///
    /// Non-optional — see divergence 3 in this type's docs.
    pub repo_id: Uuid,
    /// Path of the `plan.yaml` that lists this file, within the repository's
    /// content root (`plans/smoke.yaml`, `infra/plan.yaml`). Together with
    /// `repo_id` and the branch this is the plan's identity in this gear —
    /// see divergence 2.
    pub plan_path: String,
    /// `plan.yaml`'s `name:` — legacy `UniverseTest::plan_name`
    /// (`analytics.rs:258`, filled from `plan.plan.name` at `analytics.rs:915`).
    pub plan_name: String,
    /// Test-file path relative to the content root, normalized the way legacy
    /// `normalize_test_path` normalizes it.
    pub test_file: String,
    /// Display name: the `TEST_META` title when non-blank, else a name derived
    /// from the file stem (legacy `fallback_test_name`, `analytics.rs:1805-1812`
    /// — strip the `test_` prefix, `_` becomes a space).
    pub test_name: String,
    /// The `TEST_META` title, when the file declares one. **Not decorative**:
    /// it is one of the four alias sources `build_alias_map` registers
    /// (`analytics.rs:1714-1748` — `test_file`, the file stem, `test_name`, and
    /// this),
    /// so an execution row that names a test by its human title still matches
    /// its file. Omitting this field silently turns those rows into `not_run`.
    ///
    /// Legacy defaults it to the display name when there is no title
    /// (`analytics.rs:909`), and that default is preserved.
    pub title_alias: Option<String>,
    /// `TEST_META` `component`, else inferred from a `tests/<component>/...`
    /// path (legacy `infer_component_from_path`, `analytics.rs:1794-1803`).
    pub component: Option<String>,
    /// `TEST_META` tags unioned with the owning plan's tags, trimmed, blanks
    /// dropped, de-duplicated and sorted (legacy `analytics.rs:891-897` — a
    /// `BTreeSet`, hence sorted).
    pub tags: Vec<String>,
    /// `TEST_META` `quality_vectors`, case-folded-deduplicated. Present here
    /// and absent from legacy's struct — see divergence 1.
    pub quality_vectors: Vec<String>,
    /// Where the plan came from — legacy's `UniverseTest::source`
    /// (`analytics.rs:259`, copied from `TestPlanInfo::source` at `:916`).
    ///
    /// **Correction to the plan's draft**, which said the values are `plan`,
    /// `custom_plan` or `git_plan`. They are not: those three are
    /// `RunIntent::run_kind()` values (`manager/src/models.rs:1601`), a
    /// different enum entirely. Legacy's plan `source` takes exactly two
    /// values, `"repo"` and `"local"` (`manager/src/services/plans.rs:98`
    /// and `:56`). This gear only ever discovers inside a synced repository,
    /// so it is always `"repo"` — see [`SOURCE_REPO`].
    pub source: String,
    /// Product versions this test is attributed to; rendered as legacy's
    /// `AnalyticsListItem::versions` (`analytics.rs:119`, via
    /// `sorted_versions_desc` at `:1418`).
    ///
    /// **Always empty, matching legacy.** Legacy never populates it either:
    /// `UniverseTest::versions` is written as `Vec::new()` and never touched
    /// again (`analytics.rs:918`; the underlying `TestPlanInfo::versions` is
    /// likewise `Vec::new()` at `manager/src/services/plans.rs:581` and `:717`).
    /// The field is carried rather than dropped so the analytics response
    /// keeps its shape — an empty list is what legacy renders today, and
    /// inventing values here would be a behavior change, not a fix.
    pub versions: Vec<String>,
    /// `count_test_functions` over the file's content
    /// (`analytics.rs:1865`). Parametrize is **not** expanded; the exact count
    /// lives in qa-insights' `test_case_collect` and wins over this one where
    /// it exists (`analytics.rs:767-779`).
    pub static_case_count: u32,
}

/// The only [`UniverseTest::source`] value this gear produces.
///
/// Legacy's other value, `"local"`, belongs to plans scanned from a plans
/// directory outside any repository (`manager/src/services/plans.rs:56`) — a
/// mode this port does not have, since ADR-0005 confines content to synced
/// repository working copies.
pub const SOURCE_REPO: &str = "repo";
