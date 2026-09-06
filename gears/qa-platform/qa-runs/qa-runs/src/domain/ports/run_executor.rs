//! The execution-plane port (`cpt-cf-qa-principle-executor-port`, ADR-0001).
//!
//! Shaped from the source system's submission contract — `submit_workflow`'s
//! argument list and the object it builds
//! (`manager/src/services/argo.rs:397-692`) — and **not** from what the p1 mock
//! finds convenient: feature 2.7 has to satisfy this trait against
//! serverless-runtime, and a port shaped around a mock is a port that has to be
//! redesigned then.
//!
//! # What the source system actually hands an execution
//!
//! Read off `submit_workflow`, five things and no more:
//!
//! 1. **The test file list**, as `TEST_FILES` (`argo.rs:436`) — and per node in
//!    the multi-group path, where each node gets its own `TEST_FILES` on top of
//!    the shared environment (`argo.rs:1220-1248`).
//! 2. **The assembled environment** (`argo.rs:436-499`), which
//!    [`crate::domain::runvars`] already produces.
//! 3. **The kubeconfig secret *reference*** — mounted as a secret volume,
//!    never the material (`argo.rs:504-521`).
//! 4. **A timeout**, as `activeDeadlineSeconds` (`argo.rs:539`).
//! 5. **A bundle reference per execution node** (`argo.rs:1220-1248`;
//!    `RepoRunConfig::bundle_url` is a single `String`, `argo.rs:64-66`).
//!
//! Everything else in that argument list becomes metadata *on* the workflow
//! object rather than input *to* the execution: annotations (`argo.rs:551`
//! onward — app version, build, namespace, platform, product key, the five
//! repository keys `test-version` / `repo-id` / `repo-name` / `source-ref` /
//! `source-ref-kind` (`argo.rs:576-598`), run source, run kind, exclusivity,
//! schedule id, Slack settings, validation) and two labels, `app=vhp-tests`
//! plus `test-plan` carrying `plan.id` (`argo.rs:638-641`). None of it is carried here — see "What is deliberately
//! not carried" below.
//!
//! # Four operations
//!
//! `start` / `cancel` / `watch` / [`list_active`](RunExecutor::list_active).
//! Both specs spell all four: ADR-0001's Consequences list and
//! `cpt-cf-qa-principle-executor-port` (`DESIGN.md:163`), each amended
//! 2026-08-13 for this task after it shipped `list_active` and disclosed the
//! departure. Read either for the full argument; the short form is that
//! [`reconcile_claim`](crate::domain::state_machine::reconcile_claim) needs "is
//! this execution still alive?" for **every** claim on every dispatcher tick,
//! and the source system answers exactly that with a single `list_workflows()`
//! per tick (`manager/src/services/run_dispatcher.rs:353-375`) rather than one
//! call per claim. One [`watch`](RunExecutor::watch) per claim cannot
//! substitute: a stream that fails to open is indistinguishable from an
//! execution that has ended, so it cannot express the fail-safe direction at
//! all.
//!
//! # Secret material never crosses this port
//!
//! The source system supplies `RP_API_KEY` as a Kubernetes `secretKeyRef`
//! rather than a value (`argo.rs:443-452`) and the kubeconfig as a mounted
//! secret volume (`argo.rs:504-521`). [`crate::domain::runvars`]
//! deliberately models neither, and records why: resolving a secret in the
//! control plane widens the blast radius of any log line
//! (`runvars.rs`, composition obligation 4). This port is the other half
//! of that decision — it is where the references travel.
//!
//! Two shapes carry them, and the split is not cosmetic: **their
//! missing-secret behaviour differs, and it differs in the source system.**
//!
//! * [`EnvSource::Secret`] — an environment variable backed by a reference.
//!   The source system marks it `"optional": true` (`argo.rs:449`), so a
//!   reference that does not resolve leaves the variable **unset and the
//!   execution proceeds**.
//! * [`MountSpec::Secret`] — a credential mounted into the run's filesystem,
//!   the target environment's kubeconfig among them. The source system mounts
//!   it as a plain secret volume with no `optional` flag (`argo.rs:506-511`),
//!   whose Kubernetes default is required: an unresolvable reference means the
//!   workload never starts.
//!
//! A per-reference `optional: bool` was considered and rejected. Both
//! behaviours are attached to *which* reference it is, in the only two
//! datapoints that exist, so a flag would generalise past the evidence while
//! leaving 2.7 free to set it wrongly. Encoded in the types, neither can be
//! configured into the other.
//!
//! **What this does and does not prove.** It does not prove that secret
//! material cannot reach an executor: [`SecretRef::new`] takes a `String`, and
//! nothing stops a caller from putting the wrong `String` in it. What it proves
//! is that no *accident* does it — there is no `From<String>`, no `Deref`, and
//! no `Display` by which a plain value flows into a reference position, and the
//! reverse (a reference read as a value) cannot be expressed at all, because
//! [`EnvSource`] forces every consumer to match both arms. The load-bearing
//! guarantee is upstream of the types anyway: **qa-runs never reads a secret's
//! contents**, so there is no material in scope to pass by mistake.
//!
//! # What is deliberately not carried
//!
//! * **The whole `vhp-tests/*` annotation set** (`argo.rs:551` onward). Those
//!   exist because the Argo object was the system of record and the control
//!   plane read run metadata back off it
//!   (`manager/src/services/run_history.rs:216`, `overlay_persisted_with_live`).
//!   Here the `qa_runs` row is (`cpt-cf-qa-principle-db-first-state`), so
//!   shipping that metadata to the executor would create a second copy that can
//!   disagree with the first.
//! * **Per-node timeouts** (`argo.rs:1213-1215`, `custom_plans.rs:755-760`).
//!   The source system derives a node's budget from the distinct plans feeding
//!   it; a qa-runs run has one plan and one deadline
//!   (`cpt-cf-qa-fr-runs-timeout`), so there is no value to put in such a field
//!   and it would be inert. If per-node budgets ever return, this is the field
//!   to add.
//! * **DAG dependencies and a parallelism cap** (`argo.rs:1263-1264`). The
//!   source system *can* express a dependency — an explicit DAG plan's node
//!   carries a `depends` expression onto the Argo task
//!   (`argo.rs:1251-1252`) — but nothing qa-runs inherits produces one: every
//!   node the repository-grouping path emits sets `depends: None`
//!   (`custom_plans.rs:754`, `:775`), and qa-catalog's `CustomPlan` has no DAG
//!   field for the other shape to come from. So nodes here are independent —
//!   see [`RunSpec::nodes`].
//! * **A bundle-less node.** The source system's node bundle is
//!   `Option<String>`, and it stays `None` on two paths: its local
//!   `/test_plans` group, which reads tests baked into the runner image
//!   (`custom_plans.rs:766-781`), and an explicit DAG plan's nodes, which are
//!   built with `bundle_url: None` (`custom_plans.rs:570`) and filled in later
//!   once each node's repository is known (`:814-819`) — so only the first is a
//!   node that genuinely runs without a bundle. qa-catalog has no such source —
//!   every test comes from a bundle — so [`ExecutionNode::bundle_ref`] is not
//!   optional.
//! * **`ttlStrategy`, the runner image, `imagePullPolicy`, host aliases and
//!   DNS-1123 node-name sanitisation** (`argo.rs:540-542`, `:1204-1206`;
//!   `custom_plans.rs:734-750`). Execution-plane deployment details, and
//!   Kubernetes-shaped ones at that.
//!
//! # `watch` is net-new, and that is on purpose
//!
//! The source system does not stream anything: it polls `list_workflows` /
//! `get_workflow` and scrapes pod logs after the fact
//! (`argo.rs:1456-1476`, `:1593`). [`RunExecutor::watch`] is required by
//! `DESIGN.md:427` ("`watch` → execution event/log stream") and by
//! `cpt-cf-qa-nfr-run-duration`, whose allocation is explicit that
//! `watch(execution_id)` must *resume* after a control-plane restart
//! (`DESIGN.md:57`). So this one operation is specified, not ported, and its
//! re-attach requirement is the part 2.7 must not skip.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use tokio::sync::mpsc;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// The access half of the product-plugin contract, re-exported so this port is
/// the one place a consumer looks for the shape a run is dispatched with.
///
/// These are `qa_product_sdk::access`'s types unchanged, not wrappers. A local
/// mirror would be a second definition of an interface a plugin author reads
/// from the SDK, and the two would drift the first time either side gained a
/// field — the failure mode spec §5.3 records for `RunVarContract::names`.
///
/// [`RunAccess`] is what this port's `KubeconfigMount` generalised into
/// (spec §5.2, §7) — Task 17 replaced the field, Task 18 deleted the type: a
/// plugin says which credentials to mount, which variables to add and which
/// service account to assume, and no part of this gear names a product.
pub use qa_product_sdk::access::{MountSpec, RunAccess, RunnerSpec};

use crate::domain::error::DomainError;
use crate::domain::repos::LogResume;
use crate::domain::state_machine::ExecutorOutcome;

/// An opaque handle on one started execution, minted by
/// [`RunExecutor::start`] and persisted as `qa_runs.execution_ref`.
///
/// **A newtype because the source system conflated this with the run name and
/// got away with it.** There, the Argo workflow name *is* the run name — one
/// `wf_name` is used as both (`manager/src/services/argo.rs:420-422`), and
/// cancellation is `terminate_workflow(&name)` taking that same string
/// (`manager/src/routes/runs.rs:1096`). Here they are two different values of
/// the same Rust type living on the same row ([`RunSpec::run_name`] and
/// `Run::execution_ref`), so `cancel(&run.name)` would compile and would either
/// cancel nothing or cancel something else.
///
/// # The tenancy this type does *not* carry
///
/// An `ExecutionRef` is scopeless: the execution plane knows nothing about
/// tenants, and [`RunExecutor::list_active`] deliberately answers across all of
/// them (see its doc). So the rule is on the caller, and it is the same rule
/// [`crate::domain::repos::OwnedRunId`] enforces one layer down:
///
/// > **Build an `ExecutionRef` only from a run row already resolved under the
/// > caller's own scope — never from request input.**
///
/// A `cancel` handler that took an `execution_ref` from the request body would
/// be a cross-tenant cancel, and no type here can stop it, because the
/// execution plane has no scope to check against.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionRef(String);

impl ExecutionRef {
    /// Wrap an executor-minted reference — from [`RunExecutor::start`] or from
    /// the run row that recorded it.
    #[must_use]
    pub fn new(reference: impl Into<String>) -> Self {
        Self(reference.into())
    }

    /// The reference as the executor spelled it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Unwrap for persistence into `qa_runs.execution_ref`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// A reference to secret material the **execution plane** resolves. Never the
/// material.
///
/// Deliberately one opaque string rather than the source system's
/// `{name, key, optional}` triple (`argo.rs:446-450`): that triple is the shape
/// of a Kubernetes `secretKeyRef`, and ADR-0001 removes Kubernetes. The
/// new-world spelling is credstore's, and qa-environments already models a
/// platform's kubeconfig exactly this way — `kubeconfig_credstore_ref: String`,
/// documented "Never the material itself"
/// (`qa-environments-sdk/src/models.rs:14-15`). Two representations of the same
/// reference across one subsystem boundary would need a translation nobody
/// owns.
///
/// There is intentionally no `From<String>`, no `Deref<Target = str>` and no
/// `Display`. Each of the three would let a plain value slide into a reference
/// position, or a reference into a log line that reads like a value.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretRef(String);

impl SecretRef {
    /// Wrap a credstore reference — e.g. `Platform::kubeconfig_credstore_ref`,
    /// or the `ReportPortal` API-key reference from this gear's own config.
    #[must_use]
    pub fn new(reference: impl Into<String>) -> Self {
        Self(reference.into())
    }

    /// The reference, for the adapter that will resolve it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where one environment variable's value comes from.
///
/// The source system's environment is a single heterogeneous list: `RP_API_KEY`
/// is an entry in the *same* `env_vars` vector as `TEST_FILES`, distinguished
/// only by carrying `valueFrom` instead of `value` (`argo.rs:436-452`). One map
/// of this enum is that list; two parallel maps — one of values, one of
/// references — would not be, and would invent a name-collision question the
/// source system does not have.
///
/// Being an enum is what makes a reference and a value unconfusable in the
/// direction that matters: a backend cannot read a [`Self::Secret`] as though
/// it were text without matching the arm and deciding what to do about it, and
/// a new arm added later fails compilation at every consumer.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvSource {
    /// A literal value, as assembled by [`crate::domain::runvars`].
    Value(String),
    /// A reference the execution plane resolves at start. If it does not
    /// resolve, the variable is left unset and the execution proceeds —
    /// the source system's `"optional": true` (`argo.rs:449`).
    Secret(SecretRef),
}

/// The environment one run executes with, values and secret references
/// together.
///
/// A newtype rather than a bare `BTreeMap<String, EnvSource>` for one reason:
/// the precedence between a secret binding and a literal of the same name has
/// exactly one home, [`RunEnv::new`], instead of being re-decided at each
/// dispatch site. Read access is unrestricted ([`RunEnv::entries`]) — it is
/// construction that needs the rule.
///
/// `BTreeMap`, matching [`crate::domain::runvars::assemble`]'s own choice
/// and for the reason stated there: a run and its re-run are compared by this
/// map, and the tests assert on it entry by entry, so a per-process iteration
/// order makes both unreadable. **Not** because it is logged — it is not, and
/// that function's doc says why the absence is worth keeping.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunEnv(BTreeMap<String, EnvSource>);

impl RunEnv {
    /// Combine the assembled literals with the run's secret bindings.
    ///
    /// **A literal of the same name wins over a secret binding**, which is the
    /// source system's order and not an arbitrary tie-break: the `RP_API_KEY`
    /// `secretKeyRef` is pushed at tier 1, third entry in the list
    /// (`argo.rs:443-452`), and every later tier appends after it. Two of the
    /// three later tiers do it by removing any existing entry of that name
    /// first — platform variables (`argo.rs:246-259`) and run parameters
    /// (`:267-272`, literally a call to the former). The pipeline tier does
    /// **not**: `append_pipeline_variables` is a bare `extend`
    /// (`argo.rs:230-240`) that removes nothing, so it leaves two entries and
    /// lets the container runtime's last-wins resolve them — the asymmetry
    /// [`crate::domain::runvars`] documents at length. Either route
    /// delivers the same order, which is why a map is behaviour-preserving. So
    /// in the source system a platform variable named `RP_API_KEY` really would
    /// replace the reference with a literal.
    ///
    /// In the source system that direction is safe because it is
    /// **unreachable**: every secret-backed name is on the reserved list
    /// (`manager/src/routes/settings.rs:15-27`), rejected case-insensitively
    /// (`routes/settings.rs:82`), on all three env write paths — run parameters
    /// (`routes/settings.rs:153`), pipeline variables (`:105-108`) and
    /// **platform** variables, which reach the same check indirectly
    /// (`routes/platforms.rs:473` → `:505 merge_pipeline_variables` →
    /// `routes/settings.rs:243` → `:108`).
    ///
    /// **The port needs two enforcement sites where the source system has one**,
    /// because one process became two gears. Both exist:
    ///
    /// * **Run parameters** — [`crate::domain::params::RESERVED_NAMES`], applied
    ///   by `params::validate`.
    /// * **Pipeline and platform variables** —
    ///   `qa_environments_sdk::RESERVED_VARIABLE_NAMES`, applied by
    ///   qa-environments' `VariablesService::validate_name`.
    ///
    /// The second was **missing until Task 11b** (2026-08-13): an earlier
    /// revision of this paragraph claimed the check was simply "ported", and
    /// Task 11's spec review found that `RESERVED_NAMES` had exactly one
    /// consumer while qa-environments enforced charset and length only — so an
    /// operator with variable-write permission really could have replaced the
    /// credstore reference with a literal, on the very precedence this function
    /// implements. Recorded here because a premise that was false once is worth
    /// naming: two lists in two crates hold this up, and
    /// `the_two_reserved_name_lists_must_stay_identical` is what stops them
    /// drifting apart again.
    ///
    /// The two rules are a pair, which is why
    /// `a_literal_of_the_same_name_replaces_a_secret_binding` asserts the
    /// direction here rather than leaving it implicit in insertion order.
    #[must_use]
    pub fn new(values: BTreeMap<String, String>, secrets: BTreeMap<String, SecretRef>) -> Self {
        let mut env: BTreeMap<String, EnvSource> = secrets
            .into_iter()
            .map(|(name, secret)| (name, EnvSource::Secret(secret)))
            .collect();
        for (name, value) in values {
            env.insert(name, EnvSource::Value(value));
        }
        Self(env)
    }

    /// One variable's source, if the run has it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EnvSource> {
        self.0.get(name)
    }

    /// Every variable, in name order.
    #[must_use]
    pub fn entries(&self) -> &BTreeMap<String, EnvSource> {
        &self.0
    }
}

/// One execution node: a bundle, and the test files to run out of it.
///
/// There is exactly one node per repository group, each carrying its own bundle
/// reference; a single group yields a single node and no synthetic DAG (parity
/// spec §3.4 step 5). The source system does the same — it groups the plan's
/// files by repository, builds one bundle per group
/// (`manager/src/routes/custom_plans.rs:696-712`) and emits one independent
/// node per group (`custom_plans.rs:714-764`), with a single group staying a
/// single container (`custom_plans.rs:784-786`).
///
/// **Nodes are independent.** Every node the grouping path emits carries
/// `depends: None` (`custom_plans.rs:754`, `:775`), so an executor may run them
/// concurrently and must not infer an order from this vector's order. The
/// vector is ordered only so node names and logs are stable — the source system
/// iterates a `BTreeMap` for exactly that reason (`custom_plans.rs:723-724`).
/// (The source system can also express a real dependency, but only for an
/// explicit DAG plan (`manager/src/services/argo.rs:1251-1252`), a shape
/// qa-catalog's `CustomPlan` has no field for. If that ever returns, it returns
/// as a field on this struct, not as an implied ordering of this vector.)
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionNode {
    /// Stable node label, used in logs ([`ExecutionEvent::Log::node`]) and in
    /// per-test attribution ([`TestObservation::node`]).
    ///
    /// Not sanitised to a DNS-1123 label the way the source system's node ids
    /// are (`custom_plans.rs:734-750`) — that constraint came from Argo task
    /// names, and ADR-0001 removes it. An adapter with its own naming rules
    /// sanitises on its own side.
    pub name: String,
    /// Where the node fetches its test content: `TestBundle::storage_ref` from
    /// qa-catalog, an opaque handle into the bundle store.
    ///
    /// Not optional — see the module docs on the source system's bundle-less
    /// local group, which has no counterpart here.
    pub bundle_ref: String,
    /// Test files this node runs, in discovery order.
    ///
    /// **May be empty, and an empty list is not an error.** The source system
    /// pushes `TEST_FILES` unconditionally, even when the joined list is blank
    /// (`argo.rs:436`), where every neighbouring variable is skipped when blank
    /// (`:461-469`) — the same asymmetry [`crate::domain::runvars`]
    /// records as composition obligation 3. An executor that rejected an empty
    /// list would diverge from the source system on a real, if unhappy, launch.
    pub test_files: Vec<String>,
}

/// Everything the execution plane needs to run one run.
///
/// # Why this is not `Clone`, `PartialEq` or `Eq` any more
///
/// [`RunSpec::access`] holds a [`RunAccess`], which refuses `Clone` by design:
/// a [`MountSpec::ConfigValue`] carries a `credstore_sdk::SecretValue`, and
/// cloning one would put a second live copy of a plaintext credential in this
/// process's memory for no consumer's benefit — the SDK's own reasoning, which
/// this port is not free to overrule from the outside. `PartialEq` goes with
/// it: comparing secret bytes is a timing footgun and nothing asks for it.
///
/// The only consumer that needed `Clone` was
/// [`MockRunExecutor::submitted`](crate::infra::executor::mock::MockRunExecutor::submitted),
/// which records what it was handed. It now stores `Arc<RunSpec>`, so a test
/// still reads every field of every submission and no credential is duplicated.
#[domain_model]
#[derive(Debug)]
pub struct RunSpec {
    /// The run's id, so the executor can correlate its observations back to a
    /// row. Not an
    /// [`OwnedRunId`](crate::domain::repos::OwnedRunId): that token
    /// proves a tenant-scoped read happened, which is a repository concern —
    /// the executor performs no scoped access and would only be laundering the
    /// token's meaning.
    pub run_id: Uuid,
    /// The run's human-facing name (`{slug}-{n}`), for the execution's own
    /// labelling. Carried in addition to [`Self::run_id`] because the source
    /// system's executions are identified to operators by exactly this string
    /// — it is the workflow name (`argo.rs:420-422`) — and a backend console
    /// listing nothing but UUIDs is not operable. It is **not** an identifier
    /// this port accepts back: see [`ExecutionRef`].
    pub run_name: String,
    /// One node per repository group. Must not be empty: a run with no nodes
    /// would execute nothing and report success, which is the failure mode
    /// [`DomainError::CorruptState`] exists to prevent one layer down.
    /// [`RunExecutor::start`] rejects it.
    pub nodes: Vec<ExecutionNode>,
    /// The assembled environment plus the run's secret bindings.
    pub env: RunEnv,
    /// How this run reaches its target environment: the credentials to mount,
    /// and the service account (if any) its pod assumes.
    ///
    /// A run with no target environment gets an **empty** access — no mounts,
    /// no service account — which is what the source system does in that case
    /// (it mounts nothing, `argo.rs:504`). `RunAccess` is not an `Option`
    /// because "nothing to mount" is a value it can already express, and an
    /// `Option` would add a second spelling of it.
    ///
    /// [`RunAccess::env`] is **not** read by the executor. Those variables
    /// reach the run through [`Self::env`], where `runvars::assemble` has
    /// already placed them at their tier: the ladder is platform-owned (**D8**)
    /// and an executor that also read this channel would be a second, untiered
    /// path into the environment.
    pub access: RunAccess,
    /// The runner shape for this run's product: image, command, pull policy.
    ///
    /// Every field is optional/empty by default, and that default means
    /// "inherit the deployment-wide `qa-runs.argo.runner_image`,
    /// `runner_command` and `image_pull_policy`" — which is what every run does
    /// today. Per *product*, never per environment (**D11**): a per-environment
    /// image would leave a run's provenance unclear.
    pub runner: RunnerSpec,
    /// Executor-side deadline. A **backstop**, not the guarantee: the source
    /// system relies on it alone (`activeDeadlineSeconds`, `argo.rs:539`),
    /// whereas `cpt-cf-qa-fr-runs-timeout` requires the control plane to
    /// enforce the timeout itself because "enforcement cannot rely on the
    /// execution backend alone" (`PRD.md:402-404`). Both are set, and the
    /// control-plane sweep is the one that must fire first.
    pub timeout_seconds: u64,
}

/// One per-test result, as the executor observed it.
///
/// Mirrors [`crate::domain::repos::NewTestResult`] field for field,
/// plus [`Self::node`] — so a narrower observation would make a column ingest
/// can never populate.
///
/// # The fields come from two different source-system reports
///
/// Recorded because an earlier version of this doc attributed all of them to
/// one, and the correction changes what "the runner reports precisely this
/// set" is evidence of.
///
/// * [`Self::test_file`], [`Self::test_name`], [`Self::status`],
///   [`Self::duration`], [`Self::launch_id`] and [`Self::jira_key`] are the
///   **live progress payload**, `ProgressPayload`, which the runner POSTs per
///   test as it goes (`manager/src/routes/runs.rs:1110-1122`).
/// * [`Self::nodeid`], [`Self::reason`] and [`Self::ticket`] are **not on that
///   payload at all**. They come from the runner's `TEST_CASE` log markers,
///   which the source system base64-decodes into a `RawCase`
///   (`manager/src/services/argo.rs:2921-2930`) and projects onto
///   `models::TestCase` (`:2979-2989`). That is a second, per-test-*function*
///   report, and it is why those three are `Option` while the first group is
///   not: a producer can legitimately have one report and not the other.
///
/// (The earlier doc cited `routes/runs.rs:1109-1121` for the whole set. The
/// line range was also off by one — `ProgressPayload` is `:1110-1122` — and the
/// claim was only ever true of the first group. Corrected rather than
/// extended, because the citation was the sentence's whole load.)
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestObservation {
    /// Which [`ExecutionNode::name`] produced this result.
    pub node: String,
    /// `""` when the executor reports no file. The runner's own payload has it
    /// optional (`routes/runs.rs:1113-1114`), but the column is
    /// `NOT NULL DEFAULT ''`
    /// and absent-versus-empty is a distinction nothing downstream can act on
    /// (see `NewTestResult::test_file`), so the collapse happens here rather
    /// than handing ingest an `Option` whose two arms it must treat alike.
    pub test_file: String,
    pub test_name: String,
    /// The runner's status vocabulary, an **open set** — do not narrow this to
    /// an enum. `PASSED` / `FAILED` / `SKIPPED` / `ERROR` / `XFAIL` / `XPASS`
    /// are mapped by name and anything else is passed through uppercased
    /// (`manager/src/services/argo.rs:2932-2943`), while the live progress
    /// endpoint binds the runner's string entirely raw
    /// (`manager/src/routes/runs.rs:1174`). Same reasoning, and the same open
    /// set, as `NewTestResult::status`; normalisation is ingest's call.
    pub status: String,
    /// The runner's duration text **verbatim**, including extended forms like
    /// `85.06s (0:01:25)` (`argo.rs:3279-3296`).
    pub duration: Option<String>,
    /// `ReportPortal` launch id, when the runner emits a `TEST_LAUNCH_ID`
    /// marker (`argo.rs:2796-2797`, `:2870`).
    pub launch_id: Option<String>,
    /// JIRA issue key for a test skipped against an open bug, when the runner
    /// reports one (`routes/runs.rs:1121`).
    pub jira_key: Option<String>,
    /// The pytest node identifier — e.g.
    /// `tests/authn/test_x.py::TestCase::test_case[tls]`.
    ///
    /// `Option`, and **additive**: an executor that has not been updated
    /// supplies `None` and every path below keeps working, the same property
    /// `#[serde(default)]` gives an additive field on a wire type. The source
    /// system's own case parser declares it exactly this way —
    /// `nodeid: Option<String>` on its `RawCase`
    /// (`manager/src/services/argo.rs:2923`) — and collapses it with
    /// `unwrap_or_default()` only when it builds the stored case (`:2973`),
    /// which is the same boundary ingest collapses it at here.
    ///
    /// # A non-empty `nodeid` means this row is **case**-level. Nothing
    /// enforces that.
    ///
    /// Stated here because this is the field that carries the distinction, and
    /// stated at all because the schema lost it. The source system keeps two
    /// tables — `test_results` per test **file**
    /// (`manager/migrations/001_initial.sql:65`) and `test_case_results` per
    /// test **function** (`:253`) — so a reader there learns the granularity
    /// of a row from which table it came out of, for free and unfalsifiably.
    ///
    /// `qa_run_test_results` is one table holding both
    /// (`m20260818_000005_case_fidelity`, and decision D1 behind it), so the
    /// granularity has to be read off a *value*. The convention is:
    ///
    /// * `nodeid` empty or absent → the row describes a whole test **file**.
    /// * `nodeid` non-empty → the row describes one test **function**, and
    ///   [`Self::test_file`] names the file it sits under.
    ///
    /// This is the same signal the source system's analytics already reads,
    /// in a different spelling: *"A file with no per-case rows (older runner)
    /// contributes one case of its file status"*
    /// (`manager/src/routes/analytics.rs:97-98`) — absence from a second table
    /// there, an empty column here.
    ///
    /// **It is a convention and not a constraint.** `NOT NULL DEFAULT ''`
    /// makes both representable and distinguishes neither; no index, no check
    /// and no type rejects a case-level row with an empty `nodeid` or a
    /// file-level row with a populated one. A consumer that relies on it is
    /// relying on every producer honouring it, which is why it is written
    /// down rather than left to be inferred from the data.
    pub nodeid: Option<String>,
    /// The xfail/skip explanation the runner reported, if any.
    ///
    /// Nullable rather than `''`-defaulted, and the asymmetry with
    /// [`Self::nodeid`] is deliberate: for a reason, absence is real
    /// information — a case with no reason is a case the runner explained
    /// nothing about — so `''` and missing would be one value with two
    /// meanings. Legacy declares it a bare nullable `TEXT`
    /// (`manager/migrations/001_initial.sql:261`) and blanks whitespace-only
    /// values back to `None` before storing (`argo.rs:2986`).
    pub reason: Option<String>,
    /// Case-level bug reference, e.g. `VHP-980`, extracted from an xfail/skip
    /// reason (`manager/migrations/001_initial.sql:262`).
    ///
    /// **Not [`Self::jira_key`].** That one is the *file*-level link legacy
    /// keeps on `test_results` (`001_initial.sql:71`); this is the *case*-level
    /// one, and legacy's analytics renders only this as
    /// `AnalyticsListItem::case_tickets`
    /// (`manager/src/routes/analytics.rs:132`). Both travel, separately.
    pub ticket: Option<String>,
}

/// What the executor could say about per-node failure.
///
/// Three named states rather than the `Option<bool>`
/// [`derive_terminal_state`](crate::domain::state_machine::derive_terminal_state)
/// takes, for the reason
/// [`ClaimExecution`](crate::domain::state_machine::ClaimExecution) is three
/// states rather than two booleans: at a call site `None` reads as "no
/// failure" when it means "no information", and that is exactly the confusion
/// the state machine's doc has to spend a paragraph undoing. Converted at the
/// boundary by [`Self::node_failure`].
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeOutcome {
    /// The executor reports no per-node detail at all. Treated downstream as
    /// "no known node failure", matching the source system's empty node list.
    Unknown,
    /// Every node reported, and none of them failed.
    NoneFailed,
    /// At least one node ended in a failed state — including a node that died
    /// before emitting any result at all, which is the case per-test results
    /// cannot cover (`argo.rs:2218-2227`, `dag_nodes_failed`).
    SomeFailed,
}

impl NodeOutcome {
    /// As
    /// [`derive_terminal_state`](crate::domain::state_machine::derive_terminal_state)
    /// takes it.
    #[must_use]
    pub fn node_failure(self) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::NoneFailed => Some(false),
            Self::SomeFailed => Some(true),
        }
    }
}

/// One observation of an execution's progress.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionEvent {
    /// The execution has begun.
    Started,
    /// A test reported a result.
    TestResult(TestObservation),
    /// A chunk of log output, for the SSE fan-out.
    Log { node: String, line: String },
    /// The execution reached a terminal state. Always last: see
    /// [`RunExecutor::watch`].
    Finished {
        outcome: ExecutorOutcome,
        /// Per-node detail, when the executor has any.
        nodes: NodeOutcome,
        /// Operator-facing reason, when the executor gives one. Lands in
        /// `qa_runs.error`, which is where the source system puts the
        /// workflow's `status.message` (`argo.rs:2280-2283`).
        message: Option<String>,
    },
}

/// The observer's end of one execution's event stream, returned by
/// [`RunExecutor::watch`].
///
/// A concrete type over a channel rather than
/// `Pin<Box<dyn futures::Stream<Item = ExecutionEvent> + Send>>`, which is what
/// this plan originally specified. Three reasons, in order:
///
/// 1. **The contract has one home.** "Ends after `Finished`" and "yields
///    nothing for an execution the backend does not know" are properties of
///    *this* type, documented once, rather than of an anonymous boxed trait
///    object each adapter re-describes.
/// 2. **It is easier to satisfy.** An adapter for 2.7 spawns a forwarder that
///    calls [`ExecutionSink::emit`]; implementing `Stream` by hand, or pulling
///    in a stream-combinator dependency to avoid doing so, is strictly more
///    work for the same behaviour.
/// 3. **No new dependency** — the weakest of the three, and stated precisely:
///    `futures` is not a dependency of *this crate*, and `qa-runs/Cargo.toml`
///    is not a file Task 11 owned. It **is** already in
///    `[workspace.dependencies]` (`Cargo.toml:354`), so adding it would have
///    been one line, not a procurement. This was a scope decision; reasons 1
///    and 2 are the ones that carry the choice.
///
/// Bounded, so a chatty log stream applies backpressure to the adapter instead
/// of growing the control plane's heap.
///
/// **The two exceptions in this module, stated rather than left to be
/// noticed.** This type and [`ExecutionSink`] are the only ones here without
/// `#[domain_model]`, and the `tokio::sync::mpsc` import they need is the only
/// `tokio` path in **this module**. Both are deliberate:
///
/// **Corrected 2026-08-15 by Task 16c.** This said "the only `tokio` path in
/// this gear's whole domain layer", and it was already false when written:
/// `domain::service::admission` imports `tokio::sync::Mutex` for
/// `PlatformLocks`, and `domain::service::watch` now adds `tokio::spawn`. The
/// scoping mattered because the sentence was being cited as the domain layer's
/// `tokio` policy — see `service::watch`'s header, which had to reject the
/// citation it was leaning on. The reasoning below is unaffected and is still
/// the right reasoning; only the census around it was wrong.
/// `#[domain_model]` marks a domain *model* — a value that crosses the
/// boundary — and these are live handles on an in-flight observation, which is
/// the one thing in this file that is not a value. The import is inside the
/// macro's rules rather than around them: it forbids `sqlx`, `sea_orm`,
/// `http`, `axum`, `hyper`, `reqwest`, `tonic`, and the two path prefixes
/// `std::fs` and `tokio::fs` (`libs/toolkit-macros/src/domain_model.rs:18-32`)
/// — `tokio::sync` is not among them, and a channel is a language-level
/// primitive rather than an integration. If that judgement is ever reversed,
/// the fix is a hand-rolled stream trait here, not an infra type in a domain
/// signature.
#[derive(Debug)]
pub struct ExecutionStream {
    rx: mpsc::Receiver<ExecutionEvent>,
}

impl ExecutionStream {
    /// Build a stream and the sink an adapter feeds it through.
    ///
    /// `capacity` is clamped to at least 1 rather than validated, because the
    /// only alternative is a panic on a value that is never meaningful.
    #[must_use]
    pub fn channel(capacity: usize) -> (ExecutionSink, Self) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        (ExecutionSink { tx }, Self { rx })
    }

    /// The next observation, or `None` once the execution has finished and the
    /// adapter has dropped its sink.
    ///
    /// Named `recv` rather than `next` because it is a channel read, not an
    /// iterator step — and because an inherent `next(&mut self)` shadows the
    /// `Iterator` method's name without being one.
    pub async fn recv(&mut self) -> Option<ExecutionEvent> {
        self.rx.recv().await
    }
}

/// The adapter's end of an execution's event stream.
#[derive(Clone, Debug)]
pub struct ExecutionSink {
    tx: mpsc::Sender<ExecutionEvent>,
}

impl ExecutionSink {
    /// Emit one observation, waiting if the observer is behind.
    ///
    /// Returns `false` once the observer has gone away — which is not a
    /// failure and must not be reported as one: the control plane stopping its
    /// observation says nothing about the execution, and an adapter that
    /// errored here would turn a closed SSE connection into a run fault.
    ///
    /// `#[must_use]` because "not a failure" is not "not actionable": an
    /// adapter that ignores it keeps producing events into a channel nobody
    /// reads, for as long as the execution runs.
    #[must_use = "false means the observer has gone; stop forwarding rather than continuing to emit"]
    pub async fn emit(&self, event: ExecutionEvent) -> bool {
        self.tx.send(event).await.is_ok()
    }
}

/// The execution plane, behind a port.
///
/// # How the four operations compose
///
/// The three failure directions below are not independent — together they are
/// the reason a dispatcher outage cannot start a run beside an exclusive one:
///
/// * **[`start`](Self::start) failed** ⇒ no execution exists. The caller
///   records the queue row `failed` and the run terminal, and **must release
///   the platform claim**, or the queue stops draining
///   (`manager/src/services/run_dispatcher.rs:47-58`).
/// * **[`watch`](Self::watch) or [`list_active`](Self::list_active) errored**
///   ⇒ nothing is known. Never read as "the execution is gone": releasing a
///   claim on an unreadable executor is what lets a second run start beside an
///   exclusive one, which is why the source system treats an unreadable Argo as
///   *busy* (`run_dispatcher.rs:237-252`) and skips the whole tick rather than
///   acting on an empty answer (`run_dispatcher.rs:353-359`).
/// * **[`list_active`](Self::list_active) succeeded and omits a reference** ⇒
///   the execution is terminal or forgotten, and the two are not
///   distinguishable. That is
///   [`ClaimExecution::Gone`](crate::domain::state_machine::ClaimExecution::Gone),
///   whose doc says the source system cannot tell them apart either and does
///   not need to.
///
/// Aliveness is [`list_active`](Self::list_active)'s question alone. An empty
/// [`watch`](Self::watch) stream is *not* evidence a run died: it is also what
/// a finished-and-forgotten execution yields.
#[async_trait]
pub trait RunExecutor: Send + Sync {
    /// Submit a run. Returns as soon as the execution is accepted; progress
    /// arrives through [`watch`](Self::watch).
    ///
    /// # Errors
    /// [`DomainError::ExecutorFailed`] when submission itself fails, and when
    /// [`RunSpec::nodes`] is empty. See the trait docs for what the caller owes
    /// the queue on this path.
    async fn start(&self, spec: RunSpec) -> Result<ExecutionRef, DomainError>;

    /// Observe an execution, resuming rather than replaying whatever
    /// `resume` says is already archived.
    ///
    /// The stream ends after [`ExecutionEvent::Finished`], and yields nothing
    /// at all for a reference the executor no longer knows — an empty stream is
    /// "nothing more to say", never "this failed".
    ///
    /// **Re-attachable by design.** Called again after a control-plane restart
    /// it must resume observation of a still-running execution rather than
    /// error: `cpt-cf-qa-nfr-run-duration` requires an 8-hour run to survive a
    /// restart, and `DESIGN.md:57` allocates that to "`watch(execution_id)`
    /// resumes".
    ///
    /// **`resume` is what makes re-attachable mean "picks up where it left
    /// off" rather than "starts over".** Before Task 13 (review finding #50)
    /// this parameter did not exist at all, `MockRunExecutor` satisfied
    /// re-attach by replaying every event from the beginning, and the Argo
    /// adapter did the same for a different reason: it opened every pod log
    /// with no way to skip what it had already sent, and nothing suppressed
    /// the re-sent lines on the way out either — which was a correctness bug
    /// there, not a design choice: `append_log` is a `CONCAT` with no
    /// truncate, replace or offset anywhere in `RunLogsRepository`, so a full
    /// replay through it duplicated the whole archived log on every
    /// re-attach. (The adapter still opens a pod log the same plain way
    /// today — `infra::executor::argo::watch`'s `LineSkip` suppresses the
    /// re-sent lines afterward instead of asking Kubernetes to filter them;
    /// that is a deliberate choice, argued for on `LineSkip`'s own doc, not
    /// the absence this paragraph describes.) `resume` is per-node
    /// (`domain::repos::LogResume`) because the Argo adapter's pods are: each
    /// is a separate log with its own read position. An executor with no
    /// resumable notion of position — the mock's only alternative to
    /// replaying-with-skip — is free to ignore it and replay in full; the
    /// port does not require true resumption, only that an implementation
    /// capable of it uses `resume` rather than discarding it.
    ///
    /// # Errors
    /// [`DomainError::ExecutorFailed`] when the executor cannot be reached.
    async fn watch(
        &self,
        execution_ref: &ExecutionRef,
        resume: LogResume,
    ) -> Result<ExecutionStream, DomainError>;

    /// Request cancellation. Idempotent: cancelling an already-terminal or
    /// entirely unknown execution succeeds.
    ///
    /// Fire-and-forget, matching the source system, whose cancel handler
    /// patches the workflow and writes **no run state at all**
    /// (`manager/src/routes/runs.rs:1092-1106` → `argo.rs:1447-1453`) — the
    /// state change arrives through the normal observation path like any
    /// other.
    ///
    /// # Errors
    /// [`DomainError::ExecutorFailed`] when the request cannot be delivered.
    async fn cancel(&self, execution_ref: &ExecutionRef) -> Result<(), DomainError>;

    /// Execution references the executor currently considers active.
    ///
    /// Used by claim reconciliation
    /// ([`reconcile_claim`](crate::domain::state_machine::reconcile_claim)),
    /// which needs "is this still alive?" for many executions at once — the
    /// source system issues a single `list_workflows()` per tick for exactly
    /// this and builds a membership set from it
    /// (`manager/src/services/run_dispatcher.rs:353-375`) rather than one call
    /// per claim.
    ///
    /// **Not tenant-scoped, and it cannot be**: the execution plane has no
    /// notion of a tenant, so this answers across all of them. It is therefore
    /// only ever safe as a *membership test* against references the control
    /// plane already holds. Enumerating it into any response, log line or
    /// listing would leak other tenants' executions.
    ///
    /// A [`BTreeSet`] rather than a `Vec`: the caller asks membership once per
    /// claim, duplicates are meaningless, and ordered iteration keeps a
    /// diagnostic log line stable.
    ///
    /// # Errors
    /// [`DomainError::ExecutorFailed`]. The dispatcher **skips the tick** on
    /// this error rather than treating the answer as empty — an empty answer
    /// would release every claim at once (`run_dispatcher.rs:353-359`).
    async fn list_active(&self) -> Result<BTreeSet<ExecutionRef>, DomainError>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::state_machine::derive_terminal_state;
    use qa_runs_sdk::{RunResult, RunState};

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    /// The precedence [`RunEnv::new`] documents, asserted rather than left to
    /// insertion order: the source system pushes the `RP_API_KEY` reference at
    /// tier 1 (`argo.rs:443-452`) and every later tier retains-then-pushes over
    /// it (`argo.rs:246-259`).
    #[test]
    fn a_literal_of_the_same_name_replaces_a_secret_binding() {
        let env = RunEnv::new(
            values(&[("RP_API_KEY", "operator-supplied")]),
            [("RP_API_KEY".to_owned(), SecretRef::new("credstore://rp"))]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            env.get("RP_API_KEY"),
            Some(&EnvSource::Value("operator-supplied".to_owned())),
            "parity with the source system's list order; unreachable only \
             because RP_API_KEY is reserved"
        );
    }

    /// The direction that actually matters: a secret binding is *never*
    /// flattened into a value on its way through the port, so nothing that
    /// walks the environment as text can print the material — there is none to
    /// print.
    #[test]
    fn a_secret_binding_stays_a_reference_in_the_assembled_environment() {
        let env = RunEnv::new(
            values(&[("TEST_FILES", "a.py,b.py")]),
            [("RP_API_KEY".to_owned(), SecretRef::new("credstore://rp"))]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            env.get("RP_API_KEY"),
            Some(&EnvSource::Secret(SecretRef::new("credstore://rp")))
        );
        assert!(
            !env.entries()
                .values()
                .any(|source| matches!(source, EnvSource::Value(value) if value.contains("rp"))),
            "no literal carries the reference's text, let alone the material"
        );
    }

    /// A secret-backed name the assembled environment does not mention is
    /// carried through untouched — the ordinary case, since every such name is
    /// reserved.
    #[test]
    fn a_secret_binding_survives_when_no_literal_shares_its_name() {
        let env = RunEnv::new(
            values(&[("TEST_FILES", "")]),
            [("RP_API_KEY".to_owned(), SecretRef::new("credstore://rp"))]
                .into_iter()
                .collect(),
        );
        assert_eq!(env.entries().len(), 2);
        assert!(matches!(
            env.get("RP_API_KEY"),
            Some(EnvSource::Secret(secret)) if secret.as_str() == "credstore://rp"
        ));
    }

    /// The drift guard for [`RunEnv::new`]'s safety premise, which rests on
    /// **two** reserved-name lists in two crates: qa-runs' own
    /// [`crate::domain::params::RESERVED_NAMES`] (run parameters) and
    /// `qa_environments_sdk::RESERVED_VARIABLE_NAMES` (pipeline and platform
    /// variables). The source system has one list for all three write paths
    /// (`../testrunner/manager/src/routes/settings.rs:15-27`); splitting one
    /// process into two gears is what made it two, and nothing but this
    /// assertion connects them.
    ///
    /// It lives here rather than beside either list because *this* is the
    /// function whose correctness argument fails when they diverge — a reader
    /// of that paragraph finds the test that keeps it true.
    ///
    /// Order-sensitive on purpose: both are documented as the source system's
    /// list "name for name and in the same order", so a reordering is a
    /// divergence from the same shared origin and worth failing on.
    #[test]
    fn the_two_reserved_name_lists_must_stay_identical() {
        assert_eq!(
            crate::domain::params::RESERVED_NAMES.as_slice(),
            qa_environments_sdk::RESERVED_VARIABLE_NAMES.as_slice(),
            "the two gears' reserved-name lists have drifted; a name enforced on \
             one env write path and not the others is a name an operator can use \
             to replace a secret reference with a literal"
        );
        // Not implied by the equality above: the two lists could be changed
        // together to drop the one name this module's precedence actually
        // depends on, and stay equal. This names the stake.
        assert!(
            crate::domain::params::RESERVED_NAMES.contains(&"RP_API_KEY"),
            "RP_API_KEY is the name RunEnv::new's precedence argument rests on"
        );
    }

    /// The composition check the port owes
    /// [`derive_terminal_state`](crate::domain::state_machine::derive_terminal_state),
    /// which is three tasks away and takes an `Option<bool>` this enum has to
    /// land on exactly.
    #[test]
    fn node_outcome_maps_onto_the_state_machines_argument() {
        assert_eq!(NodeOutcome::Unknown.node_failure(), None);
        assert_eq!(NodeOutcome::NoneFailed.node_failure(), Some(false));
        assert_eq!(NodeOutcome::SomeFailed.node_failure(), Some(true));
    }

    /// And the composed result, not just the mapping: a green outcome with a
    /// dead node is a `Failed` run, while "no per-node detail" must **not** be
    /// read as a failure. Transposing two arms of
    /// [`NodeOutcome::node_failure`] passes the mapping test's siblings
    /// individually but changes this.
    #[test]
    fn a_dead_node_downgrades_an_otherwise_green_run() {
        let clean = RunResult {
            passed: 1,
            total: 1,
            ..RunResult::default()
        };
        assert_eq!(
            derive_terminal_state(
                ExecutorOutcome::Succeeded,
                clean,
                NodeOutcome::SomeFailed.node_failure()
            ),
            RunState::Failed
        );
        assert_eq!(
            derive_terminal_state(
                ExecutorOutcome::Succeeded,
                clean,
                NodeOutcome::Unknown.node_failure()
            ),
            RunState::Succeeded
        );
        assert_eq!(
            derive_terminal_state(
                ExecutorOutcome::Succeeded,
                clean,
                NodeOutcome::NoneFailed.node_failure()
            ),
            RunState::Succeeded
        );
    }

    /// `ExecutionRef` exists so a run name cannot be passed where an execution
    /// reference belongs. Nothing asserts a compile failure, so this pins the
    /// half that is observable: the wrapper round-trips exactly, and two
    /// different strings are two different references.
    #[test]
    fn an_execution_reference_round_trips_and_compares_by_value() {
        let reference = ExecutionRef::new("mock-execution-1");
        assert_eq!(reference.as_str(), "mock-execution-1");
        assert_ne!(reference, ExecutionRef::new("smoke-tests-1"));
        assert_eq!(reference.into_inner(), "mock-execution-1");
    }

    /// The stream's own contract: events arrive in emission order, and the
    /// stream ends when the adapter drops its sink.
    #[tokio::test]
    async fn the_stream_yields_in_order_and_ends_with_the_sink() {
        let (sink, mut stream) = ExecutionStream::channel(2);
        assert!(sink.emit(ExecutionEvent::Started).await);
        assert!(
            sink.emit(ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Succeeded,
                nodes: NodeOutcome::NoneFailed,
                message: None,
            })
            .await
        );
        drop(sink);

        assert_eq!(stream.recv().await, Some(ExecutionEvent::Started));
        assert!(matches!(
            stream.recv().await,
            Some(ExecutionEvent::Finished { .. })
        ));
        assert_eq!(stream.recv().await, None);
    }

    /// A dropped observer is not an adapter failure — the control plane
    /// stopping its observation says nothing about the execution.
    #[tokio::test]
    async fn emitting_to_a_dropped_observer_reports_false_rather_than_failing() {
        let (sink, stream) = ExecutionStream::channel(1);
        drop(stream);
        assert!(!sink.emit(ExecutionEvent::Started).await);
    }
}
