//! Runner-variable assembly: the environment variables handed to a
//! test-runner pod.
//!
//! `cpt-cf-qa-fr-runs-env-assembly`: static runner variables → pipeline
//! variables → platform variables → run parameters, where later entries
//! override earlier ones of the same name.
//!
//! This module was renamed to free the word "environment" for
//! `qa-environments`' aggregate, which is renamed `Environment` as part of
//! the same change: one word could not mean both that aggregate and the
//! variables assembled here for a test-runner pod in the same gear.
//!
//! Ported from the env build in `submit_workflow`
//! (`manager/src/services/argo.rs:436-521`) and the three `append_*` helpers
//! (`argo.rs:230-272`). **The requirement's one-sentence version omits two
//! positions and one mechanism**, all three verified against the source:
//!
//! * **A fifth position between the platform variables and the run
//!   parameters.** The platform's own base URL is pushed *after* the platform
//!   variables (`argo.rs:493-496`, with the comment "Platform metadata should
//!   win over generic pipeline variables when both are present"), so it
//!   overrides a platform variable of that name while still losing to a run
//!   parameter. It is not a reserved name (plan decision D3), so that last part
//!   is reachable — a launch parameter really can redirect the run's base URL.
//!   **That position is now [`RunVarInputs::plugin_env`]**: since Task 18 the
//!   values in it are a product plugin's, and this module no longer knows their
//!   names.
//! * **`KUBECONFIG` was pushed last of all**, after the run parameters
//!   (`argo.rs:504-521`), so it won outright — and this module asserted the
//!   *ordering* as well as the reserved-name check, so that neither alone held
//!   it up.
//!
//!   **Task 18 collapsed that.** `KUBECONFIG` is one of the variables a plugin
//!   returns from `prepare_run_access`, and a product-agnostic ladder cannot
//!   single it out: nothing here knows which of a plugin's variables names a
//!   mount. So it now sits in `plugin_env` at the fifth position with the rest,
//!   where a run parameter would beat it. **Unreachable, by one mechanism
//!   instead of two:** `KUBECONFIG` is on
//!   [`crate::domain::params::RESERVED_NAMES`], the platform's own floor that
//!   `validate` unions rather than replaces.
//!
//!   **One mechanism, applied at three call sites — not three mechanisms.**
//!   Corrected at the Phase E review (finding I-9), because this sentence is
//!   what makes the trade acceptable and it was counting wrong. The three sites
//!   are `params::validate` (run parameters, this crate) and
//!   `VariablesService::validate_name` reached from the pipeline and
//!   per-environment variable scopes — which are two call sites into **one**
//!   function in qa-environments, kept in step with this crate's list only by
//!   `the_two_reserved_name_lists_must_stay_identical`. So a reader auditing
//!   this after Task 19 is checking one list and one cross-crate pin, not three
//!   independent assurances. `the_reserved_floor_is_what_holds_kubeconfig_up`
//!   is where the property itself is written down.
//! * **The three tiers are not merged the same way.**
//!   `append_platform_variables` (`argo.rs:246-259`) and
//!   `append_run_parameters` (`argo.rs:267-272`, literally a call to the
//!   former) **retain-then-push**: they remove any existing entry of the same
//!   name before appending, a true override. `append_pipeline_variables`
//!   (`argo.rs:230-240`) is a bare `extend` — it removes nothing, so a pipeline
//!   variable colliding with a static one leaves **two** entries in the list.
//!
//! # Why assembling into a map is behaviour-preserving, not a change
//!
//! The source system builds a *list* of `{name, value}` entries and hands it to
//! Kubernetes, which resolves a duplicate name by taking the later entry. The
//! source system says so itself, in `append_platform_variables`' own doc
//! (`argo.rs:242-245`): "Kubernetes 'last wins' would also handle the dedupe,
//! but explicit removal keeps the rendered YAML unambiguous to humans." So the
//! retain is a readability measure over a semantics the runtime already
//! provides, and the extend-only pipeline tier gets the same outcome by the
//! runtime's route rather than by the list's.
//!
//! Which means the pipeline tier *is* observably an override, and a map — where
//! a later insert replaces an earlier one — produces exactly the source
//! system's environment with no duplicate entries left to resolve.
//!
//! **The load-bearing citation, though, is not that comment — it is the frozen
//! user-facing guide.** `../testrunner/docs/guides/run-parameters.md:18-23`
//! documents the chain "static runner vars → global pipeline variables →
//! platform variables → run parameters" under the sentence "where a later entry
//! overrides an earlier one of the same name". Pipeline-over-static is
//! therefore *documented contract*, not an inference from how a container
//! runtime happens to resolve a duplicated list entry. The extend-versus-retain
//! asymmetry is an implementation detail of how the source system achieves it.
//!
//! Checked rather than assumed, because the plan attached a stop-and-report
//! condition to it: the collision is only *reachable* for statics that are not
//! reserved, since pipeline variables go through the same reserved-name check
//! as run parameters (`routes/settings.rs:105-109` → `validate_variable_list`).
//! That leaves `VHP_PROGRESS_URL`, `COLLECT_ONLY` and `VHP_COLLECT_URL`
//! (`argo.rs:438-441`, `:50-59`) as the only statics a pipeline variable can
//! shadow — and for all three, "last wins" is what the source system delivers.
//!
//! # Composition contract — what the dispatcher must do around this
//!
//! 1. **Assembly happens at dispatch, not at launch.** Two tier-1 statics are
//!    not knowable earlier: `TEST_BUNDLE_URL` and `TEST_VERSION` come from the
//!    bundle build (`push_repo_env`, `argo.rs:36-60`), which Task 14 runs after
//!    the force-sync. The run *name* must already be settled, because the
//!    result-callback URL embeds it (`argo.rs:438-441`).
//! 2. **`APP_VERSION` / `APP_BUILD` read the run's own snapshotted columns, not
//!    a live platform lookup** (user decision 2026-08-13, recorded at the head
//!    of the plan's Task 9). Re-deriving them from `platform_id` would let a
//!    platform upgrade silently change a queued run's or a re-run's
//!    `APP_VERSION`.
//! 3. **The caller decides which statics exist; this module does not filter
//!    them.** The source system's blank-guards are applied at each push site
//!    and they are not uniform: `APP_VERSION`, `APP_BUILD`,
//!    `E2E_K8S_NAMESPACE`, `TEST_BUNDLE_URL` and `TEST_VERSION` are skipped
//!    when blank (`argo.rs:461-469`, `:40-49`), while `TEST_FILES` is pushed
//!    **unconditionally, even when empty** (`argo.rs:436`). A blanket
//!    blank-filter over [`RunVarInputs::statics`] would therefore be wrong.
//!
//!    **Since Task 18 this module applies no blank guard at all.** The two
//!    that used to live *inside* the precedence logic were the platform base
//!    URL's (with the `VPADM_BASE_DOMAIN` derived from it) and the observed
//!    namespace's, and both belonged to values only a product could name — so
//!    both moved to the plugin that owns the name, along with the names
//!    themselves. A gear that still guarded `VPADM_BASE_DOMAIN` would still
//!    know what a VHP install is, which is the whole thing this change was
//!    for.
//! 4. **`RP_API_KEY` is not a value in the source system.** It is a
//!    `secretKeyRef` (`argo.rs:443-452`), so it cannot be expressed as an
//!    [`RunVar`] without materialising the secret into the control plane's
//!    memory. It is deliberately absent from this module's model: the executor
//!    port (Task 11) must carry secret references alongside the assembled
//!    environment, and a task that "fixes" this by resolving the secret here
//!    has widened the blast radius of a log line.

use std::collections::BTreeMap;

use qa_runs_sdk::RunParameter;
use uuid::Uuid;

/// One `name=value` entry in a run's environment.
///
/// A named-field struct rather than the `(String, String)` the plan proposed:
/// the two fields have the same type, so a tuple lets a caller write the value
/// where the name goes and get a silently wrong environment. Every tier below
/// is a `Vec<RunVar>` for the same reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunVar {
    pub name: String,
    pub value: String,
}

impl From<RunParameter> for RunVar {
    fn from(parameter: RunParameter) -> Self {
        Self {
            name: parameter.name,
            value: parameter.value,
        }
    }
}

/// The plugin contract's own run variable, converted on the way in.
///
/// Two identical two-field structs across a crate boundary, and a conversion
/// rather than one shared type: `qa_product_sdk` is the contract every plugin
/// author reads and it may not depend on this gear, while this type is the one
/// the precedence ladder is written against. The conversion is where a plugin's
/// declaration becomes a tier — see [`RunVarInputs::plugin_env`].
impl From<qa_product_sdk::access::RunVar> for RunVar {
    fn from(variable: qa_product_sdk::access::RunVar) -> Self {
        Self {
            name: variable.name,
            value: variable.value,
        }
    }
}

impl From<qa_environments_sdk::Variable> for RunVar {
    fn from(variable: qa_environments_sdk::Variable) -> Self {
        Self {
            name: variable.name,
            value: variable.value,
        }
    }
}

/// The two qa-environments tiers, already separated by scope.
///
/// They are kept in one struct, produced by one function, because they are
/// adjacent in the precedence chain and have the same type: two sibling
/// `Vec<RunVar>` fields on [`RunVarInputs`] filled in by hand would let a caller
/// swap them and invert the very rule this module exists to enforce. Build
/// them with [`split_by_scope`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TieredRunVars {
    /// Global pipeline variables — qa-environments rows with
    /// `platform_id = None`.
    pub pipeline: Vec<RunVar>,
    /// The target platform's own variables. More specific than `pipeline`, so
    /// they override it.
    pub platform: Vec<RunVar>,
}

/// Split one `list_variables` response into its two precedence tiers.
///
/// `qa_environments_sdk::Variable` carries the scope in `environment_id`
/// (renamed from `platform_id`) (`None` = global pipeline variable, `Some(_)` =
/// per-platform), and `QaEnvironmentsClient::list_variables` returns both in
/// one list with the comment "Precedence is applied by the caller (qa-runs),
/// not here" — this is that caller.
///
/// A variable scoped to some *other* platform is dropped rather than admitted
/// to either tier. `list_variables` already filters to the requested platform,
/// so this is defence in depth: the failure it prevents is another platform's
/// value silently overriding this run's, which no test downstream would notice.
/// For `environment_id = None` the platform tier is therefore always empty,
/// matching the source system, which does not even issue the query for a run
/// with no platform (`manager/src/services/argo.rs:489-492`).
///
/// Order within each tier is preserved, because within a tier the last entry
/// wins.
#[must_use]
pub fn split_by_scope(
    variables: Vec<qa_environments_sdk::Variable>,
    environment_id: Option<Uuid>,
) -> TieredRunVars {
    let mut tiers = TieredRunVars::default();
    for variable in variables {
        match variable.environment_id {
            None => tiers.pipeline.push(variable.into()),
            Some(id) if Some(id) == environment_id => tiers.platform.push(variable.into()),
            Some(_) => {}
        }
    }
    tiers
}

/// Everything that goes into one run's environment, in precedence order.
///
/// A struct rather than positional arguments because the fields are mutually
/// transposable and transposing two of them silently inverts the precedence
/// rule this module exists to enforce.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunVarInputs {
    /// Runner control variables the control plane sets itself: `TEST_FILES`,
    /// the result-callback URL, `RP_PROJECT`, `APP_VERSION`, `APP_BUILD`,
    /// `PRODUCT_KEY`, `SKIP_TESTS_WITH_BUGS`, and the bundle reference
    /// (`TEST_BUNDLE_URL`, `TEST_VERSION`, and the collect-only pair). Lowest
    /// precedence.
    ///
    /// `E2E_K8S_NAMESPACE` is not in this list and never was in this tier's
    /// *source*: it used to have its own field, fed from the same platform
    /// fetch as the base URL, and since Task 18 it is one of the variables a
    /// product plugin returns — [`Self::plugin_env`], one tier up.
    ///
    /// Every name here is in [`crate::domain::params::RESERVED_NAMES`] except
    /// the result-callback URL and the collect-only pair — see the SECURITY
    /// NOTE there.
    ///
    /// Three of the module docs' composition obligations land on this one
    /// field, and a caller filling it in needs all three:
    ///
    /// * **Obligation 2** — `APP_VERSION` and `APP_BUILD` come from the run's
    ///   own snapshotted columns, never a live platform lookup.
    /// * **Obligation 3** — the caller decides which of these exist. Do not
    ///   blank-filter the list here: `TEST_FILES` is pushed even when empty.
    /// * **Obligation 4** — `RP_API_KEY` is reserved and is deliberately *not*
    ///   in this list, because the source system supplies it as a `secretKeyRef`
    ///   rather than a value. It belongs to the executor port, not here.
    ///
    /// A `Vec<RunVar>` rather than a struct of named `Option<String>` fields,
    /// even though the set is enumerable and fixed. A struct would encode
    /// *membership* — which names exist — but membership is not the rule here:
    /// obligation 3's per-name blank guards are non-uniform, `TEST_FILES` being
    /// pushed even when empty while the rest are skipped when blank
    /// (`argo.rs:436` versus `:461-469`). A struct would have to carry those
    /// guards somewhere anyway, and the natural place — `Option` meaning
    /// "absent" — is exactly the blanket blank-filter obligation 3 forbids. The
    /// list keeps the decision with the caller, which is where the source
    /// system keeps it: at each push site.
    pub statics: Vec<RunVar>,
    /// The two qa-environments tiers. Build with [`split_by_scope`].
    pub variables: TieredRunVars,
    /// What the target environment's **product plugin** contributed:
    /// `RunAccess::env`, verbatim except for the type. Sits *between* the
    /// platform variables and the run parameters — the fifth position described
    /// in the module docs, and exactly where `platform_base_url` used to be.
    ///
    /// Empty for a run with no target environment, which is the source system's
    /// shape too: it issues no platform query at all in that case
    /// (`argo.rs:489-492`).
    ///
    /// **Names and values are the plugin's; the position is the platform's**
    /// (**D8**). This module applies no blank guard to them and derives nothing
    /// from them: a plugin decides whether a value it could not detect is
    /// omitted or blank, and every guard the platform used to apply here
    /// (`E2E_VHP_BASE_URL`'s, `VPADM_BASE_DOMAIN`'s derivation,
    /// `E2E_K8S_NAMESPACE`'s) now lives in the plugin that owns the name, with
    /// its own tests. That is the whole point of the move — a gear that guarded
    /// `VPADM_BASE_DOMAIN` would still know what a VHP install is.
    pub plugin_env: Vec<RunVar>,
    /// Per-launch parameters, already validated by
    /// [`crate::domain::params::validate`]. **Highest precedence**, with no
    /// exception since Task 18 — see the module docs on `KUBECONFIG`.
    pub parameters: Vec<RunVar>,
}

/// Assemble the run's environment.
///
/// `BTreeMap` rather than `HashMap` so the result is deterministically ordered.
/// What that buys: the assembled map is what
/// [`crate::domain::ports::run_executor::RunSpec::env`] carries to the
/// executor, so it is what a run and its re-run are compared by and what this
/// module's own tests assert on entry by entry — and a `HashMap`'s iteration
/// order varies per process, which makes an equality failure unreadable and a
/// spec diff meaningless.
///
/// **The environment is not logged**, which this said it was. It reaches no
/// `tracing` macro anywhere in the gear — `dispatch_spec` builds it and hands
/// it straight to `RunEnv::new` — and that absence is worth keeping: [`RunVar`]
/// derives `Debug` over `value`, so a log line naming the assembled map would
/// print every platform variable and every run parameter in full, which is the
/// tier `qa-environments` holds secret *references* for. The sentence was an
/// invitation to add exactly that line.
///
/// One thing a map discards that the source system's *list* carried: Kubernetes
/// resolves a `$(VAR)` reference in an env value only against entries defined
/// **earlier in the same list**, so list position is semantic there and
/// alphabetical order here is not. Irrelevant today — this gear creates no
/// Kubernetes objects and Task 11's executor port is not Kubernetes — but if
/// that port ever renders a container spec, the flattening is not free and the
/// tier order would have to be re-expressed as list order.
///
/// Names are matched **exactly**, with no case folding, because the source
/// system's override compares them verbatim (`argo.rs:252`). Two
/// differently-cased spellings therefore coexist. That is safe only because
/// [`crate::domain::params::validate`] folds case when detecting duplicates, so
/// a launch cannot create the state through parameters — the two rules are a
/// pair, and neither is safe to relax alone.
#[must_use]
pub fn assemble(inputs: RunVarInputs) -> BTreeMap<String, String> {
    let RunVarInputs {
        statics,
        variables,
        plugin_env,
        parameters,
    } = inputs;

    // Five tiers, in order, each overriding the one before it. The fourth is
    // the product plugin's — the fifth position of the module docs, where
    // `E2E_VHP_BASE_URL` used to be pushed from platform metadata
    // (`argo.rs:493-496`) — and the fifth is the run's own parameters.
    //
    // One loop over an array rather than five insert blocks, now that every
    // tier is a plain `Vec<RunVar>`: the ladder is the array's order, which is
    // the shortest form in which it can be read and the hardest to reorder by
    // accident. What made that impossible before was that three of the entries
    // were single `Option<String>`s carrying their own blank guards, and every
    // one of those guards now belongs to the plugin that owns the name.
    let mut env = BTreeMap::new();
    for tier in [
        statics,
        variables.pipeline,
        variables.platform,
        plugin_env,
        parameters,
    ] {
        for entry in tier {
            env.insert(entry.name, entry.value);
        }
    }
    env
}

/// The collect-only pair, and nothing else.
///
/// Ported from the `collect_only` branch of `push_repo_env`
/// (`manager/src/services/argo.rs:50-59`; the plan cited `:50-57`, which stops
/// one line inside the inner `if` and two lines short of the block):
///
/// ```text
/// if repo.collect_only {
///     env_vars.push(json!({ "name": "COLLECT_ONLY", "value": "true" }));
///     if let Some(url) = repo.collect_url.as_deref().filter(|v| !v.trim().is_empty()) {
///         env_vars.push(json!({ "name": "VHP_COLLECT_URL", "value": url }));
///     }
/// }
/// ```
///
/// # The two names are frozen
///
/// `COLLECT_ONLY` and `VHP_COLLECT_URL` are part of the test-facing contract
/// under `cpt-cf-qa-fr-runner-contract`. Existing test repositories
/// read them by these exact spellings. Do not rename, re-case, or add to them —
/// which is why this returns a map built here rather than taking names from a
/// caller.
///
/// # `COLLECT_ONLY` is unconditional, `VHP_COLLECT_URL` is not
///
/// The asymmetry is the source system's and it is meaningful: the flag is what
/// puts the runner in `pytest --collect-only` mode, so it must be set for a
/// collect run to *be* one, while the URL is only where the counts go. A blank
/// URL therefore yields a run that collects and reports nowhere, rather than a
/// run that executes every test in the repository — which is what pushing
/// neither would produce, and which is the failure mode worth being explicit
/// about.
///
/// # This is a contribution, not a whole environment
///
/// **The plan's Step 3 says the collect branch "sets the two variables and
/// nothing else — no `TEST_FILES`, no platform variables tier", and that is a
/// statement about this function, not about a collect run's environment.**
/// Checked against the source rather than repeated: a collect submission goes
/// through the same `submit_workflow` as every plan run, so it also receives
/// `TEST_FILES` (`argo.rs:436`, pushed unconditionally), `VHP_PROGRESS_URL`
/// (`:438-441`), `TEST_BUNDLE_URL`/`TEST_VERSION` (`:40-49`) and the global
/// pipeline variables (`:486`).
///
/// What it genuinely does *not* receive is the rest, and each omission is a
/// `None` at the call site (`manager/src/services/collect.rs:128-149`): no
/// platform, hence no platform-variables tier (`:489-492` is skipped), no
/// `E2E_VHP_BASE_URL` (`:493`), no `KUBECONFIG` (`:504-521`), no `APP_VERSION`,
/// `APP_BUILD`, `E2E_K8S_NAMESPACE` or `PRODUCT_KEY`, and **no run parameters**
/// (`:144` passes `&[]`). In this gear those absences follow from a collect run
/// carrying no `platform_id` and no parameters rather than from a second
/// assembly path, so [`assemble`] produces them without a special case; see
/// `service::dispatch_spec`, which merges this pair into the statics tier.
///
/// `TEST_FILES` is absent from *both* here, and for a reason that predates
/// collect: this port carries the file list on the execution node rather than
/// in the shared environment, because there is one bundle and one file list per
/// node (`service::dispatch_spec::build_spec`).
///
/// Lowest precedence, like every other static: a pipeline variable of the same
/// name still wins, which this module's header already records as reachable for
/// exactly these two names plus `VHP_PROGRESS_URL`.
#[must_use]
pub fn assemble_collect_vars(collect_url: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(COLLECT_ONLY_VAR.to_owned(), "true".to_owned());
    if !collect_url.trim().is_empty() {
        env.insert(COLLECT_URL_VAR.to_owned(), collect_url.to_owned());
    }
    env
}

/// Puts the runner in `pytest --collect-only` mode. Frozen spelling.
const COLLECT_ONLY_VAR: &str = "COLLECT_ONLY";

/// Where the runner posts its per-file case counts. Frozen spelling.
const COLLECT_URL_VAR: &str = "VHP_COLLECT_URL";

#[cfg(test)]
mod tests {
    //! # What moved out of here at Task 18, and where it went
    //!
    //! Six tests left this module, because the behaviour they pinned did:
    //! `E2E_VHP_BASE_URL`'s blank guard, `VPADM_BASE_DOMAIN`'s derivation (with
    //! its scheme-less and unparseable cases), `E2E_K8S_NAMESPACE`'s source,
    //! and the pair-is-absent case. Every one is now a product plugin's rule
    //! and every one is pinned in `qa-vhp-product-plugin`'s `run_tests.rs` —
    //! `the_base_domain_accompanies_the_base_url`,
    //! `an_unparseable_base_url_yields_the_url_without_a_domain`,
    //! `a_scheme_less_base_url_still_yields_a_bare_domain`,
    //! `blank_observed_attributes_contribute_no_variables`,
    //! `the_observed_namespace_becomes_the_namespace_variable`, and
    //! `a_fully_observed_environment_yields_exactly_the_four_variables`.
    //!
    //! They are recorded here rather than deleted quietly because a reader of
    //! this module will look for them: it is where they were for the whole of
    //! Phases A–D, and "the coverage moved" and "the coverage went" are
    //! indistinguishable from a diff that only shows deletions.
    //!
    //! What stays is everything the *ladder* owns: the tier order, exact name
    //! matching, within-tier order, and the collect pair.

    use super::*;
    use uuid::Uuid;

    fn v(name: &str, value: &str) -> RunVar {
        RunVar {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    fn inputs() -> RunVarInputs {
        RunVarInputs {
            statics: vec![v("TEST_FILES", "a.py,b.py")],
            variables: TieredRunVars::default(),
            plugin_env: vec![],
            parameters: vec![],
        }
    }

    #[test]
    fn statics_alone_pass_through() {
        let env = assemble(inputs());
        assert_eq!(env.get("TEST_FILES").map(String::as_str), Some("a.py,b.py"));
        assert_eq!(env.len(), 1);
    }

    /// The **five**-tier chain, each tier overriding the one before it
    /// (`cpt-cf-qa-fr-runs-env-assembly`; `services/argo.rs:436-499`).
    ///
    /// Five since Task 18, and the new rung is in the middle: a product
    /// plugin's variables beat both variable tiers and lose to a run
    /// parameter, which is exactly the position and precedence
    /// `platform_base_url` held (`argo.rs:493-496`).
    #[test]
    fn each_tier_overrides_the_one_before_it() {
        let mut i = inputs();
        i.statics.push(v("SHARED", "static"));
        let env = assemble(i.clone());
        assert_eq!(env.get("SHARED").map(String::as_str), Some("static"));

        i.variables.pipeline.push(v("SHARED", "pipeline"));
        let env = assemble(i.clone());
        assert_eq!(env.get("SHARED").map(String::as_str), Some("pipeline"));

        i.variables.platform.push(v("SHARED", "platform"));
        let env = assemble(i.clone());
        assert_eq!(env.get("SHARED").map(String::as_str), Some("platform"));

        i.plugin_env.push(v("SHARED", "plugin"));
        let env = assemble(i.clone());
        assert_eq!(
            env.get("SHARED").map(String::as_str),
            Some("plugin"),
            "the product's own value beats a generic pipeline or environment \
             variable of the same name"
        );

        i.parameters.push(v("SHARED", "param"));
        let env = assemble(i);
        assert_eq!(
            env.get("SHARED").map(String::as_str),
            Some("param"),
            "per-launch parameters are the most specific value for this run"
        );
    }

    /// The fifth position, product-neutrally: a plugin's variable beats an
    /// environment variable of the same name and loses to a run parameter
    /// (`services/argo.rs:493-496`, where the value in this position was
    /// `E2E_VHP_BASE_URL`).
    ///
    /// **That name is deliberately not used here.** The value in this tier is
    /// whatever the product's plugin returned, and a test naming VHP's variable
    /// would put a product literal back in the gear this task took it out of.
    /// The end-to-end version, through a real plugin and a real dispatch, is
    /// `service::dispatch_tests`'
    /// `a_run_parameter_overrides_a_plugin_supplied_variable`.
    #[test]
    fn a_plugin_variable_overrides_an_environment_variable_but_not_a_parameter() {
        let mut i = inputs();
        i.variables
            .platform
            .push(v("PRODUCT_ENDPOINT", "from-variable"));
        i.plugin_env.push(v("PRODUCT_ENDPOINT", "from-plugin"));
        let env = assemble(i.clone());
        assert_eq!(
            env.get("PRODUCT_ENDPOINT").map(String::as_str),
            Some("from-plugin")
        );

        i.parameters.push(v("PRODUCT_ENDPOINT", "from-parameter"));
        let env = assemble(i);
        assert_eq!(
            env.get("PRODUCT_ENDPOINT").map(String::as_str),
            Some("from-parameter"),
            "a plugin's name is reserved only if the plugin says so, and this one \
             did not: a parameter overrides it (decision D3's exposure, carried \
             forward)"
        );
    }

    /// **This module no longer blank-guards anything a plugin sends.** A plugin
    /// that returns an empty value means it, and the platform is not entitled
    /// to a second opinion: guessing here is how a gear ends up knowing what a
    /// VHP install is.
    ///
    /// The source system's blank guards have not gone — they moved to the
    /// plugin that owns each name, with its own tests (see this module's test
    /// header). What is asserted here is that this function does not add one.
    #[test]
    fn a_blank_plugin_value_is_the_plugins_decision_not_this_modules() {
        let mut i = inputs();
        i.variables
            .platform
            .push(v("PRODUCT_ENDPOINT", "from-variable"));
        i.plugin_env
            .push(v("PRODUCT_ENDPOINT", String::new().as_str()));
        let env = assemble(i);
        assert_eq!(
            env.get("PRODUCT_ENDPOINT").map(String::as_str),
            Some(""),
            "a plugin that wanted the variable absent would have omitted it"
        );
    }

    /// **The one precedence guarantee Task 18 gave up, written down.**
    ///
    /// `KUBECONFIG` used to be pushed after the run parameters and win
    /// outright, and this module asserted that ordering *as well as* the
    /// reserved-name check so neither alone held it up. It is now one of the
    /// variables a plugin returns, and nothing here can tell it apart from the
    /// others — so at this layer a parameter beats it.
    ///
    /// What stops that is [`crate::domain::params::RESERVED_NAMES`], which
    /// `validate` unions with rather than replaces, and which is refused
    /// case-insensitively at all three write sites — **one** rule reached from
    /// three places, two of them in qa-environments (see the module header's
    /// correction). This test records both halves: the ordering really did
    /// change, and the floor really does refuse the name.
    #[test]
    fn the_reserved_floor_is_what_holds_kubeconfig_up() {
        let mut i = inputs();
        i.plugin_env.push(v("KUBECONFIG", "/.kube/kubeconfig"));
        i.parameters.push(v("KUBECONFIG", "attacker"));
        let env = assemble(i);
        assert_eq!(
            env.get("KUBECONFIG").map(String::as_str),
            Some("attacker"),
            "the ladder no longer protects this name; the reserved floor does"
        );

        let refused = crate::domain::params::validate(
            &[qa_runs_sdk::RunParameter {
                name: "kubeconfig".to_owned(),
                value: "attacker".to_owned(),
            }],
            &qa_product_sdk::access::RunVarContract::default(),
        );
        assert!(
            matches!(
                refused,
                Err(crate::domain::params::ParamError::Reserved { .. })
            ),
            "so a launch can never put that parameter in front of assembly, in \
             any casing, and a plugin reserving nothing cannot change that"
        );
    }

    /// A run with no target environment gets no plugin tier at all — the
    /// source system issues no platform query in that case either
    /// (`argo.rs:489-492`).
    #[test]
    fn a_run_without_an_environment_gets_no_plugin_variables() {
        let env = assemble(inputs());
        assert!(!env.contains_key("KUBECONFIG"));
        assert_eq!(env.len(), 1, "the statics tier and nothing else");
    }

    /// Names are matched exactly, not case-insensitively: the source system's
    /// retain compares `existing["name"] != variable.name` with no case folding
    /// (`services/argo.rs:252`). Two differently-cased spellings therefore
    /// coexist in the environment — which is why the duplicate check in
    /// `params` folds case, so a launch can never create that state through
    /// parameters.
    #[test]
    fn assembly_matches_names_exactly() {
        let mut i = inputs();
        i.variables.platform.push(v("Shared", "mixed"));
        i.parameters.push(v("SHARED", "upper"));
        let env = assemble(i);
        assert_eq!(env.get("Shared").map(String::as_str), Some("mixed"));
        assert_eq!(env.get("SHARED").map(String::as_str), Some("upper"));
    }

    /// Within one tier, a later entry wins — the tier's own list order is
    /// preserved rather than being an arbitrary map insertion
    /// (`append_platform_variables` retains then pushes, in order,
    /// `services/argo.rs:246-259`).
    #[test]
    fn within_a_tier_the_last_entry_wins() {
        let mut i = inputs();
        i.variables.platform.push(v("DUP", "first"));
        i.variables.platform.push(v("DUP", "second"));
        let env = assemble(i);
        assert_eq!(env.get("DUP").map(String::as_str), Some("second"));
    }

    /// The conversion from the run's stored parameters. Trivial, and tested
    /// anyway: `RunVar`'s two fields have the same type, so a transposition
    /// inside the `From` impl compiles and would send every parameter's value
    /// into the environment under the wrong name.
    ///
    /// Its sibling below covers the other `From` impl, which carries the
    /// identical hazard. That one is also caught incidentally by
    /// `the_scope_split_sends_global_variables_to_the_pipeline_tier` — but that
    /// test's name pins scope splitting, so a later edit narrowing it to check
    /// only `platform_id` routing would take the field-order coverage with it
    /// and nothing would say so.
    #[test]
    fn a_run_parameter_converts_field_for_field() {
        let converted = RunVar::from(RunParameter {
            name: "NAME".to_owned(),
            value: "VALUE".to_owned(),
        });
        assert_eq!(converted, v("NAME", "VALUE"));
    }

    /// The same hazard on the qa-environments side. `Variable` has three fields
    /// this module discards and two it keeps, and the two it keeps have the same
    /// type — so the transposition compiles here exactly as it does above.
    #[test]
    fn an_environments_variable_converts_field_for_field() {
        let converted = RunVar::from(qa_environments_sdk::Variable {
            id: Uuid::new_v4(),
            environment_id: Some(Uuid::new_v4()),
            name: "NAME".to_owned(),
            value: "VALUE".to_owned(),
        });
        assert_eq!(converted, v("NAME", "VALUE"));
    }

    // ---------- tier splitting ----------

    fn var(environment_id: Option<Uuid>, name: &str, value: &str) -> qa_environments_sdk::Variable {
        qa_environments_sdk::Variable {
            id: Uuid::new_v4(),
            environment_id,
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    /// `environment_id = None` is a global pipeline variable; `Some(target)` is a
    /// per-platform one (`qa_environments_sdk::Variable`'s own doc). Assigning
    /// the two tiers by hand is the one place a caller could invert the
    /// precedence chain silently, so the split is done here and tested.
    #[test]
    fn the_scope_split_sends_global_variables_to_the_pipeline_tier() {
        let target = Uuid::new_v4();
        let tiers = split_by_scope(
            vec![var(None, "GLOBAL", "g"), var(Some(target), "LOCAL", "l")],
            Some(target),
        );
        assert_eq!(tiers.pipeline, vec![v("GLOBAL", "g")]);
        assert_eq!(tiers.platform, vec![v("LOCAL", "l")]);
    }

    /// Defence in depth over `list_variables`' contract: a variable belonging
    /// to some other platform must never reach the platform tier, where it
    /// would override this run's own values.
    #[test]
    fn the_scope_split_drops_another_platforms_variables() {
        let target = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tiers = split_by_scope(
            vec![var(Some(other), "LOCAL", "wrong-platform")],
            Some(target),
        );
        assert!(tiers.pipeline.is_empty());
        assert!(tiers.platform.is_empty());
    }

    // ---------- collect-only ----------

    /// The two variables are the whole runner-facing contract, and their names
    /// are frozen (`cpt-cf-qa-fr-runner-contract`).
    #[test]
    fn collect_assembly_sets_exactly_the_two_legacy_variables() {
        let env = assemble_collect_vars("https://insights.example/qa/v1/collect/r/main");
        assert_eq!(env.get("COLLECT_ONLY").map(String::as_str), Some("true"));
        assert_eq!(
            env.get("VHP_COLLECT_URL").map(String::as_str),
            Some("https://insights.example/qa/v1/collect/r/main")
        );
        assert_eq!(env.len(), 2, "nothing else belongs in the collect pair");
    }

    /// `VHP_COLLECT_URL` is pushed only when a URL was supplied, and a blank one
    /// is not a URL — `push_repo_env`'s guard is
    /// `.as_deref().filter(|v| !v.trim().is_empty())`
    /// (`manager/src/services/argo.rs:52-58`). `COLLECT_ONLY` is unconditional
    /// inside the branch (`:51`), so a collect run with no report target still
    /// collects and simply reports nowhere.
    #[test]
    fn a_blank_collect_url_pushes_only_the_flag() {
        for blank in ["", "   "] {
            let env = assemble_collect_vars(blank);
            assert_eq!(env.get("COLLECT_ONLY").map(String::as_str), Some("true"));
            assert!(!env.contains_key("VHP_COLLECT_URL"), "blank is absent");
            assert_eq!(env.len(), 1);
        }
    }

    /// A run with no target platform has no platform tier at all — the source
    /// system does not even issue the query (`services/argo.rs:489-492`).
    #[test]
    fn a_platformless_run_has_no_platform_tier() {
        let tiers = split_by_scope(
            vec![
                var(None, "GLOBAL", "g"),
                var(Some(Uuid::new_v4()), "LOCAL", "l"),
            ],
            None,
        );
        assert_eq!(tiers.pipeline, vec![v("GLOBAL", "g")]);
        assert!(tiers.platform.is_empty());
    }
}
