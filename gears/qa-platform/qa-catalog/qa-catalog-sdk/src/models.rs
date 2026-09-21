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
    /// Commit id the last successful sync materialized; `None` when the
    /// repository has never synced.
    ///
    /// **The content revision, where `last_synced_at` is only the attempt
    /// instant.** Two syncs a day apart that find the same upstream tip
    /// produce two different `last_synced_at` values and one `head_commit`,
    /// which is what makes this the right key for anything that caches work
    /// derived from the working copy.
    pub head_commit: Option<String>,
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

/// A three-state exclusivity declaration: the plan's, the test file's, or the
/// launch request's.
///
/// **`Inherit` is not `Shared`.** `Inherit` means "this tier says nothing, ask
/// the tier below"; `Shared` means "this tier says explicitly: do not take the
/// platform". Collapsing them resolves a destructive suite as parallel, which
/// is the failure `qa_runs`'s `domain::exclusivity` module carries an operator
/// warning for. This distinction is load-bearing — see PRD
/// `cpt-cf-qa-fr-runs-exclusivity`.
///
/// This was `Option<bool>` behind a `pub type ExclusiveFlag` alias. The alias
/// named the concept and enforced nothing: `flag.unwrap_or(false)` compiled
/// and meant "treat inherit as shared". Review findings #10 and #11.
///
/// # No `serde` here, on purpose
///
/// The wire form is `null` / `true` / `false` — unchanged from the
/// `Option<bool>` this replaces, because these values are persisted in
/// `plan.yaml` files and posted by CI callers. But this is a *contract*
/// crate: `qa-catalog-sdk`, like every qa-platform SDK crate, stays free of
/// `serde`/`utoipa`/`http` by a repo-wide rule enforced by review, not by a
/// lint (`qa_runs_sdk`'s crate doc states it in full; the two
/// `de010x_no_*_in_contract` dylint rules are in `Gears.toml`'s skip list
/// because the *tooling* needs migration, not because the rule is relaxed).
/// So this type carries no `Serialize`/`Deserialize` impl — there is nothing
/// here for the compiler to check the wire shape against.
///
/// [`Self::to_option_bool`] and [`Self::from_option_bool`] are the seam every
/// boundary that touches the wire crosses instead. The guarantee is pinned by
/// *name*, at each boundary, rather than by the type: `qa-catalog`'s
/// `domain::parsing::plan_yaml` (`RawPlanYaml`/`ParsedPlan`, deserialized by
/// `serde_saphyr`) on the way in, `qa-catalog`'s `api::rest::dto::PlanDto`
/// (`serde`+`utoipa`) on the way out, and `qa-runs`'s
/// `api::rest::dto::LaunchRunReq` (both directions) are the three boundaries
/// today. `plan_dto_preserves_exclusive_tri_state` and
/// `an_absent_exclusive_stays_inherit_rather_than_becoming_parallel` (plus
/// their wire-form siblings) are what pin those three boundaries; nothing
/// detects a *fourth* boundary being added without calling through this
/// seam; a new one needs its own named test, the same way these were.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Exclusivity {
    /// Nothing declared at this tier; ask the tier below.
    #[default]
    Inherit,
    /// This tier declares: give the run the platform to itself.
    Exclusive,
    /// This tier declares, explicitly, "not exclusive" — distinct from
    /// silence. Still overrides a lower tier's `Exclusive`.
    Shared,
}

impl Exclusivity {
    /// Encode as the `Option<bool>` wire shape this type replaced: `None`
    /// inherits, `Some(true)` is exclusive, `Some(false)` is explicitly
    /// shared. Every caller lives in a gear crate that owns a wire boundary
    /// (see this type's doc); nothing in this crate itself needs the
    /// `Option<bool>` shape.
    #[must_use]
    pub fn to_option_bool(self) -> Option<bool> {
        match self {
            Self::Inherit => None,
            Self::Exclusive => Some(true),
            Self::Shared => Some(false),
        }
    }

    /// [`Self::to_option_bool`]'s inverse. Infallible: every `Option<bool>`
    /// has a reading, which is the whole point of a closed three-state type
    /// over the two-state-plus-null it replaces.
    #[must_use]
    pub fn from_option_bool(value: Option<bool>) -> Self {
        match value {
            None => Self::Inherit,
            Some(true) => Self::Exclusive,
            Some(false) => Self::Shared,
        }
    }
}

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
    /// `validation` tag. Consumed by
    /// qa-runs, which re-homes it as a run classification.
    pub validation: bool,
    pub exclusive: Exclusivity,
}

/// Parsed `TEST_META` for one test file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestFileMeta {
    pub path: String,
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub exclusive: Exclusivity,
    /// JIRA issue keys referenced by the meta block (e.g. "VHP-123").
    pub bugs: Vec<String>,
}

/// One test-file reference inside a [`CustomPlan`].
///
/// # Why `plan_path` exists
///
/// An entry names the nested `plan.yaml` the file belongs to, and that
/// association is load-bearing for exclusivity rather than decoration:
/// `custom_plan_tier` groups a custom plan's entries by it, consults each nested
/// plan's own `exclusive:` declaration first, and only falls through to a
/// `TEST_META` scan for the nested plans that declared nothing. An earlier
/// version of this model carried `(repo_id, path)` pairs with no plan reference
/// at all, so a custom plan composed of a plan declaring `exclusive: true` whose
/// test files were all silent resolved **parallel** — a destructive suite losing
/// its environment-to-itself guarantee with no error and no warning.
///
/// # Rejected alternative: `plan_id: Uuid`
///
/// The obvious shape, and it cannot work here. A plan is **not persisted** —
/// [`Plan`] is materialized on read and there is no
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
/// that names no plan, because an entry without its plan loses the plan tier of
/// exclusivity resolution, and optional-on-write was itself the divergence.
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
/// | [`NewCustomPlanEntry`] | `String` | required; a 400 is better than a silent `None` |
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
    /// Durable short code. Persisted as `qa_products.product_key`.
    pub key: String,
    pub description: String,
    pub folder: Option<String>,
    /// Full GTS instance id of the product plugin that owns this product's
    /// behaviour — the key `qa-catalog`'s resolver hands to
    /// `ClientScope::gts_id`, unchanged. Not the instance *segment*: the
    /// stored value is the plugin spec's type id with the plugin's own
    /// segment appended (`PluginV1::build_registration` composes it), so a
    /// VHP product holds
    /// `gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1`.
    ///
    /// **A plain `String` since Task 20**, which completed the expand/contract
    /// pair: `m20260903_000004_plugin_instance_id_not_null` (folded into `migrations::m20260812_000002_initial` by the docs squash) tightened the
    /// column, so "this product names no plugin" is no longer a state the type
    /// can hold. It was a misconfiguration rather than a mode — nothing about
    /// such a product could be observed or dispatched — and **D6** says every
    /// product names a plugin with no fallback path.
    pub plugin_instance_id: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// The mutable fields of a product, for creation.
///
/// A parameter struct rather than five positional arguments: `create_product`
/// and `update_product` are the only pair in this contract that took bare
/// positional arguments, three of them adjacent `String`s, so transposing
/// `name`, `key` and `description` at a call site compiled silently. Adding
/// `plugin_instance_id` as a sixth would have made that materially worse, so
/// the recorded follow-up (`DECOMPOSITION.md`) is taken here instead. The
/// shape now matches [`NewTestRepository`] and [`NewCustomPlan`].
#[derive(Clone, Debug, PartialEq)]
pub struct NewProduct {
    pub name: String,
    /// Durable short code.
    pub key: String,
    pub description: String,
    /// `None` = the product lives at the root.
    pub folder: Option<String>,
    /// Full GTS instance id of the owning product plugin — see
    /// [`Product::plugin_instance_id`] for what the value is.
    ///
    /// **Required since Task 20** (**D6**), and required *structurally*: this
    /// was `Option<String>` with `create_product` refusing `None`, which is a
    /// rule a caller had to be told rather than a shape it could not express.
    /// The shipped UI could not send the field at all, so every product it
    /// created named no plugin — finding FW-1, and what blocked the `NOT NULL`
    /// migration.
    ///
    /// [`ProductUpdate::plugin_instance_id`] stays optional, and that
    /// asymmetry is deliberate: there, `None` means "leave the stored binding
    /// alone" (ruling D-18), which is a real third state a create does not
    /// have.
    pub plugin_instance_id: String,
}

/// Mutable fields of an existing product — full replace, no tri-state patch
/// semantics, with **one deliberate exception**: `folder: None` moves the
/// product back to the root, but `plugin_instance_id: None` leaves the stored
/// binding alone.
#[derive(Clone, Debug, PartialEq)]
pub struct ProductUpdate {
    pub name: String,
    /// Durable short code.
    pub key: String,
    pub description: String,
    /// `None` = move the product back to the root.
    pub folder: Option<String>,
    /// Full GTS instance id of the owning product plugin — see
    /// [`Product::plugin_instance_id`].
    ///
    /// # `None` means "leave it", not "unbind it"
    ///
    /// The one field on this struct that is not full-replace, and the
    /// asymmetry is the safe direction. Every other field on a product is
    /// something an editor can see and retype; a plugin binding is a
    /// ~100-character GTS instance id that no UI surface offers, and losing
    /// it stops every one of that product's environments being observed and
    /// every one of its runs being dispatched.
    ///
    /// Measured, not hypothetical: the shipped UI's `productReqFromForm`
    /// sends `{name, key, description, folder}` and **cannot** send this
    /// field — it is generated against `docs/api/api.json`, which carries no
    /// `/qa/v1` path at all. Under full replace, an operator editing a
    /// product's description through the Products page silently unbound its
    /// plugin, and recovery meant re-entering an id nothing displays.
    ///
    /// Nothing is lost by refusing to unbind here, because unbinding is not a
    /// state this platform wants: spec decision **D6** is "every product names
    /// a plugin, there is no fallback path", and the contract migration makes
    /// the column `NOT NULL`. A rebind is an ordinary `Some`.
    pub plugin_instance_id: Option<String>,
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
    /// The hex-encoded HMAC tag that authorises
    /// `GET /qa/v1/test-bundles/{id}?sig=...` for exactly this bundle, for as
    /// long as [`Self::expires_at`] says the bundle exists.
    ///
    /// # A transport field, never a stored one
    ///
    /// There is no `download_sig` column and there never will be: the tag is a
    /// pure function of `(id, tenant_id, qa-catalog's signing secret)`,
    /// recomputed on every verification the way qa-insights recomputes its
    /// collect-report `sig`. **It is populated only by
    /// `BundlesService::create_bundle`'s return value** — a `TestBundle` read
    /// back out of the descriptor table (the GC sweep, the download path's own
    /// row read) carries an empty string here, because a row has no signature
    /// in it to map.
    ///
    /// # Why it rides the model at all
    ///
    /// qa-runs' dispatcher is the only consumer: it puts the tag on
    /// `ExecutionNode::bundle_token`, and the Argo adapter renders it into
    /// `TEST_BUNDLE_URL`'s query string. That is the whole journey, and
    /// `create_bundle`'s return value is the one place where the bundle id, the
    /// owning tenant and the signing secret are all in hand at once.
    ///
    /// **Do not log it, and do not put it in `RunSpec::env`.** A run's
    /// parameters are readable back over the runs API; the node-derived
    /// environment is not. See `qa-runs`' `ExecutionNode::bundle_token`.
    pub download_sig: String,
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
    /// Branch to materialize. `None` uses the repository's `default_branch`.
    pub branch: Option<String>,
    /// Skip (and evict) the freshness TTL entry. The launch path always sets
    /// this, dropping the recency marker before syncing so a cached checkout
    /// cannot be returned.
    pub force: bool,
}

/// One test file as the analytics universe sees it.
///
/// A **projection for qa-insights**, not a catalog concept. It exists because
/// analytics needs each file's `component` / `tags` / `quality_vectors` and an
/// expected case count, and the gear split
/// ([ADR-0004](../../docs/ADR/0004-cpt-cf-qa-adr-four-gear-decomposition.md))
/// puts the repository content on this side of the boundary. qa-insights has no
/// checkout of its own, so everything it needs per file has to ride across on
/// this row.
///
/// # Four field choices worth stating
///
/// 1. **`quality_vectors` rides on this row** rather than on a separate
///    per-file map. The vectors are parsed from the same checkout read that
///    builds this projection, and that read happens only on this side of the
///    boundary. Dropping this field does not fail to compile; it silently
///    empties the overview's quality-vector summary.
/// 2. **The plan is identified by `plan_path` + `repo_id`, not a `plan_id`.**
///    A plan is **not persisted** — [`Plan`] is materialized on read, there is
///    no plans table, and its identity is `(repo_id, branch, path)`. This is the
///    same conclusion [`CustomPlanEntry`] reached and documented, so the same
///    shape is used here rather than minting a synthetic UUID that would resolve
///    against nothing.
/// 3. **`repo_id` is `Uuid`, not `Option<Uuid>`.** Every discovered plan comes
///    out of a synced repository working copy — there is no local-plans mode —
///    so an `Option` that is never `None` would only push an `unwrap` onto
///    qa-insights.
/// 4. **The count is named `static_case_count`**, and is `u32` for the wire. The
///    name says out loud that it is the static estimate, not the collect job's
///    exact count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UniverseTest {
    /// The repository the plan and the file both live in.
    ///
    /// Non-optional — see field choice 3 in this type's docs.
    pub repo_id: Uuid,
    /// Path of the `plan.yaml` that lists this file, within the repository's
    /// content root (`plans/smoke.yaml`, `infra/plan.yaml`). Together with
    /// `repo_id` and the branch this is the plan's identity in this gear —
    /// see field choice 2.
    pub plan_path: String,
    /// `plan.yaml`'s `name:`.
    pub plan_name: String,
    /// Test-file path relative to the content root, normalized by
    /// `normalize_test_path`.
    pub test_file: String,
    /// Display name: the `TEST_META` title when non-blank, else a name derived
    /// from the file stem: strip the `test_` prefix, `_` becomes a space.
    pub test_name: String,
    /// The `TEST_META` title, when the file declares one. **Not decorative**:
    /// it is one of the four alias sources `build_alias_map` registers —
    /// `test_file`, the file stem, `test_name`, and this — so an execution row
    /// that names a test by its human title still matches its file. Omitting
    /// this field silently turns those rows into `not_run`.
    ///
    /// It defaults to the display name when the file declares no title.
    pub title_alias: Option<String>,
    /// `TEST_META` `component`, else inferred from a `tests/<component>/...`
    /// path by `infer_component_from_path`.
    pub component: Option<String>,
    /// `TEST_META` tags unioned with the owning plan's tags, trimmed, blanks
    /// dropped, de-duplicated and sorted (a `BTreeSet`, hence sorted).
    pub tags: Vec<String>,
    /// `TEST_META` `quality_vectors`, case-folded-deduplicated. Carried on this
    /// row rather than a separate map — see field choice 1.
    pub quality_vectors: Vec<String>,
    /// Where the plan came from.
    ///
    /// **Not to be confused with a run kind.** `plan`, `custom_plan` and
    /// `git_plan` are `RunIntent::run_kind()` values, a different enum
    /// entirely. This gear only ever discovers plans inside a synced repository,
    /// so this is always `"repo"` — see [`SOURCE_REPO`].
    pub source: String,
    /// Product versions this test is attributed to; rendered as
    /// `AnalyticsListItem::versions`.
    ///
    /// **Always empty.** Nothing populates it: the plan walk has no version
    /// attribution to draw on. The field is carried rather than dropped so the
    /// analytics response keeps its declared shape, and so a producer can be
    /// added without a wire change; inventing values here would be a behaviour
    /// change rather than a fix.
    pub versions: Vec<String>,
    /// `count_test_functions` over the file's content. Parametrize is **not**
    /// expanded; the exact count lives in qa-insights' `test_case_collect` and
    /// wins over this one where it exists.
    pub static_case_count: u32,
}

/// The only [`UniverseTest::source`] value this gear produces.
///
/// There is no second value: every plan is discovered inside a synced
/// repository working copy, so a plans directory outside any repository is not
/// a mode this gear has.
pub const SOURCE_REPO: &str = "repo";

#[cfg(test)]
mod exclusivity_tests {
    use super::Exclusivity;

    /// **The three exclusivity states are three values, not two plus a null.**
    ///
    /// `None` means *inherit* and `Some(false)` means *explicitly shared*, and
    /// the difference decides whether a destructive suite gets the platform to
    /// itself. Both SDKs documented that rule in prose and typed it as
    /// `Option<bool>`, so `flag.unwrap_or(false)` -- which collapses inherit
    /// into shared -- compiled everywhere. `ExclusiveFlag` gave the concept a
    /// name without giving the compiler anything to check. Review findings
    /// #10 and #11.
    #[test]
    fn inherit_and_shared_are_distinguishable_without_convention() {
        assert_ne!(Exclusivity::Inherit, Exclusivity::Shared);
        // The trap the alias permitted: a default that silently means "shared".
        assert_eq!(Exclusivity::default(), Exclusivity::Inherit);
    }

    /// **Not the wire test.** This crate is serde-free by the contract-purity
    /// rule this type's own doc explains, so there is no `Serialize` impl to
    /// serialize here. What this pins is the conversion methods' round trip --
    /// necessary for the wire form to survive, but not sufficient: it cannot
    /// fail if a boundary struct (`PlanDto`, `LaunchRunReq`, ...) forgets to
    /// call through `to_option_bool`/`from_option_bool` at all. The actual
    /// `null`/`true`/`false` wire assertions live where the wire lives --
    /// `qa-catalog`'s `plan_dto_preserves_exclusive_tri_state` and the
    /// `plan_yaml` parsing tests, and `qa-runs`'s
    /// `an_absent_exclusive_stays_inherit_rather_than_becoming_parallel` and
    /// its wire-form siblings.
    #[test]
    fn to_option_bool_and_back_round_trips_through_every_variant() {
        for variant in [
            Exclusivity::Inherit,
            Exclusivity::Exclusive,
            Exclusivity::Shared,
        ] {
            assert_eq!(
                Exclusivity::from_option_bool(variant.to_option_bool()),
                variant
            );
        }
    }

    #[test]
    fn to_option_bool_matches_the_old_option_bool_shape() {
        assert_eq!(Exclusivity::Inherit.to_option_bool(), None);
        assert_eq!(Exclusivity::Exclusive.to_option_bool(), Some(true));
        assert_eq!(Exclusivity::Shared.to_option_bool(), Some(false));
    }

    #[test]
    fn from_option_bool_matches_the_old_option_bool_shape() {
        assert_eq!(Exclusivity::from_option_bool(None), Exclusivity::Inherit);
        assert_eq!(
            Exclusivity::from_option_bool(Some(true)),
            Exclusivity::Exclusive
        );
        assert_eq!(
            Exclusivity::from_option_bool(Some(false)),
            Exclusivity::Shared
        );
    }
}
