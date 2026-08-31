//! Run environment assembly.
//!
//! `cpt-cf-qa-fr-runs-env-assembly`: static runner variables → pipeline
//! variables → platform variables → run parameters, where later entries
//! override earlier ones of the same name.
//!
//! Ported from the env build in `submit_workflow`
//! (`manager/src/services/argo.rs:436-521`) and the three `append_*` helpers
//! (`argo.rs:230-272`). **The requirement's one-sentence version omits two
//! positions and one mechanism**, all three verified against the source and all
//! three preserved:
//!
//! * **A fifth position between the platform variables and the run
//!   parameters.** The platform's own base URL is pushed *after* the platform
//!   variables (`argo.rs:493-496`, with the comment "Platform metadata should
//!   win over generic pipeline variables when both are present"), so it
//!   overrides a platform variable of that name while still losing to a run
//!   parameter. It is not a reserved name (plan decision D3), so that last part
//!   is reachable — a launch parameter really can redirect the run's base URL.
//!   `VPADM_BASE_DOMAIN`, the bare host derived from that same URL, is pushed
//!   alongside it in this position, with the same blank guard and the same
//!   override ordering.
//! * **`KUBECONFIG` is pushed last of all**, after the run parameters
//!   (`argo.rs:504-521`), so it wins outright. Reserved, hence unreachable from
//!   a parameter — but the *ordering* is what actually enforces that, not the
//!   reserved list, and both are asserted here.
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
//!    blank-filter over [`EnvInputs::statics`] would therefore be wrong. The
//!    two blank-guards that live *inside* the precedence logic —
//!    [`EnvInputs::platform_base_url`] (and the `VPADM_BASE_DOMAIN` derived
//!    from it) and [`EnvInputs::platform_namespace`] — are applied here,
//!    because that is where the source system applies the first and, for the
//!    second, because its value comes from the same platform fetch as the
//!    first rather than from the caller-assembled statics list.
//! 4. **`RP_API_KEY` is not a value in the source system.** It is a
//!    `secretKeyRef` (`argo.rs:443-452`), so it cannot be expressed as an
//!    [`EnvVar`] without materialising the secret into the control plane's
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
/// is a `Vec<EnvVar>` for the same reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

impl From<RunParameter> for EnvVar {
    fn from(parameter: RunParameter) -> Self {
        Self {
            name: parameter.name,
            value: parameter.value,
        }
    }
}

impl From<qa_environments_sdk::Variable> for EnvVar {
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
/// `Vec<EnvVar>` fields on [`EnvInputs`] filled in by hand would let a caller
/// swap them and invert the very rule this module exists to enforce. Build
/// them with [`split_by_scope`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TieredVariables {
    /// Global pipeline variables — qa-environments rows with
    /// `platform_id = None`.
    pub pipeline: Vec<EnvVar>,
    /// The target platform's own variables. More specific than `pipeline`, so
    /// they override it.
    pub platform: Vec<EnvVar>,
}

/// Split one `list_variables` response into its two precedence tiers.
///
/// `qa_environments_sdk::Variable` carries the scope in `platform_id`
/// (`None` = global pipeline variable, `Some(_)` = per-platform), and
/// `QaEnvironmentsClient::list_variables` returns both in one list with the
/// comment "Precedence is applied by the caller (qa-runs), not here" — this is
/// that caller.
///
/// A variable scoped to some *other* platform is dropped rather than admitted
/// to either tier. `list_variables` already filters to the requested platform,
/// so this is defence in depth: the failure it prevents is another platform's
/// value silently overriding this run's, which no test downstream would notice.
/// For `target_platform_id = None` the platform tier is therefore always empty,
/// matching the source system, which does not even issue the query for a run
/// with no platform (`manager/src/services/argo.rs:489-492`).
///
/// Order within each tier is preserved, because within a tier the last entry
/// wins.
#[must_use]
pub fn split_by_scope(
    variables: Vec<qa_environments_sdk::Variable>,
    target_platform_id: Option<Uuid>,
) -> TieredVariables {
    let mut tiers = TieredVariables::default();
    for variable in variables {
        match variable.platform_id {
            None => tiers.pipeline.push(variable.into()),
            Some(id) if Some(id) == target_platform_id => tiers.platform.push(variable.into()),
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
pub struct EnvInputs {
    /// Runner control variables the control plane sets itself: `TEST_FILES`,
    /// the result-callback URL, `RP_PROJECT`, `APP_VERSION`, `APP_BUILD`,
    /// `PRODUCT_KEY`, `SKIP_TESTS_WITH_BUGS`, and the bundle reference
    /// (`TEST_BUNDLE_URL`, `TEST_VERSION`, and the collect-only pair). Lowest
    /// precedence.
    ///
    /// `E2E_K8S_NAMESPACE` is *not* pushed through this list even though it
    /// shares this tier's precedence: its value comes from the same platform
    /// fetch as [`Self::platform_base_url`] rather than from a value the
    /// caller already has in hand when assembling statics, so it has its own
    /// field, [`Self::platform_namespace`].
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
    /// A `Vec<EnvVar>` rather than a struct of named `Option<String>` fields,
    /// even though the set is enumerable and fixed. A struct would encode
    /// *membership* — which names exist — but membership is not the rule here:
    /// obligation 3's per-name blank guards are non-uniform, `TEST_FILES` being
    /// pushed even when empty while the rest are skipped when blank
    /// (`argo.rs:436` versus `:461-469`). A struct would have to carry those
    /// guards somewhere anyway, and the natural place — `Option` meaning
    /// "absent" — is exactly the blanket blank-filter obligation 3 forbids. The
    /// list keeps the decision with the caller, which is where the source
    /// system keeps it: at each push site.
    pub statics: Vec<EnvVar>,
    /// The two qa-environments tiers. Build with [`split_by_scope`].
    pub variables: TieredVariables,
    /// The target platform's own base URL, from platform metadata. Sits
    /// *between* the platform variables and the run parameters — the fifth
    /// position described in the module docs. Blank is treated as absent.
    /// `VPADM_BASE_DOMAIN`, the bare host, is derived from this same value and
    /// pushed at the same position — see [`assemble`].
    pub platform_base_url: Option<String>,
    /// The target platform's observed Kubernetes namespace. Lowest
    /// precedence, alongside [`Self::statics`] — legacy pushes
    /// `E2E_K8S_NAMESPACE` before the pipeline and platform variable tiers
    /// (`argo.rs:461-469`), so either can still override it. Blank is treated
    /// as absent, matching the source system's guard at that push site.
    pub platform_namespace: Option<String>,
    /// Per-launch parameters, already validated by
    /// [`crate::domain::params::validate`]. Highest precedence except
    /// `KUBECONFIG`.
    pub parameters: Vec<EnvVar>,
    /// Where the executor will mount the kubeconfig. `None` for a run with no
    /// target platform. Applied last of all.
    pub kubeconfig_path: Option<String>,
}

/// Environment variable name for the platform base URL. Named here because
/// this is the only place its precedence position is expressed.
const PLATFORM_BASE_URL_VAR: &str = "E2E_VHP_BASE_URL";

/// Environment variable name for the platform's bare host, derived from
/// [`PLATFORM_BASE_URL_VAR`]. Pushed at the same position, immediately
/// alongside it.
const BASE_DOMAIN_VAR: &str = "VPADM_BASE_DOMAIN";

/// Environment variable name for the platform's observed Kubernetes
/// namespace.
const NAMESPACE_VAR: &str = "E2E_K8S_NAMESPACE";

/// Environment variable naming the mounted kubeconfig.
const KUBECONFIG_VAR: &str = "KUBECONFIG";

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
/// it straight to `RunEnv::new` — and that absence is worth keeping: [`EnvVar`]
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
pub fn assemble(inputs: EnvInputs) -> BTreeMap<String, String> {
    let EnvInputs {
        statics,
        variables,
        platform_base_url,
        platform_namespace,
        parameters,
        kubeconfig_path,
    } = inputs;

    let mut env = BTreeMap::new();
    // Lowest precedence, alongside `statics`: legacy pushes the observed
    // namespace before the pipeline and platform variable tiers
    // (`argo.rs:461-469`), so either can still override it. Blank is absent,
    // matching the source system's guard at that push site.
    if let Some(namespace) = platform_namespace.filter(|ns| !ns.trim().is_empty()) {
        env.insert(NAMESPACE_VAR.to_owned(), namespace);
    }
    for tier in [statics, variables.pipeline, variables.platform] {
        for entry in tier {
            env.insert(entry.name, entry.value);
        }
    }
    // The fifth position: after the platform variables, before the parameters
    // (`argo.rs:493-496`). Blank is absent, not an empty override — the source
    // system's guard is `.filter(|v| !v.trim().is_empty())`, so an unset
    // metadata field must not blank out a platform variable of this name.
    if let Some(base_url) = platform_base_url.filter(|url| !url.trim().is_empty()) {
        // `VPADM_BASE_DOMAIN` shares this position and this blank guard —
        // derived from `base_url`, so it is only ever present when the URL
        // is. An unparseable URL (`base_domain_from_url` returns `None`)
        // still yields `E2E_VHP_BASE_URL`: a bad URL must not silently
        // suppress the value an operator set, only the derived domain.
        if let Some(domain) = base_domain_from_url(&base_url) {
            env.insert(BASE_DOMAIN_VAR.to_owned(), domain);
        }
        env.insert(PLATFORM_BASE_URL_VAR.to_owned(), base_url);
    }
    for entry in parameters {
        env.insert(entry.name, entry.value);
    }
    // Last of all (`argo.rs:504-521`). No blank guard, because the source
    // system has none: the value is a control-plane constant tied to the
    // kubeconfig mount, not user-supplied metadata.
    if let Some(path) = kubeconfig_path {
        env.insert(KUBECONFIG_VAR.to_owned(), path);
    }
    env
}

/// Extracts the bare host (no scheme, port, or path) from a platform's
/// `vhp_base_url`, e.g. `"https://sv.jele.io"` -> `Some("sv.jele.io")`.
///
/// Ported from `base_domain_from_url`
/// (`manager/src/services/argo.rs:2476`). Delegates to `url::Url` rather than
/// hand-rolled string splitting, so IPv6 literals, userinfo
/// (`user:pass@host`), and IDN hosts are handled correctly. `Url::parse`
/// requires a scheme, so a bare host (no `://`) gets a throwaway `https://`
/// prefix purely to make it parseable — the scheme itself is discarded, only
/// `host_str()` is read.
///
/// Returns `None` for blank input or input `Url::parse` cannot make sense of
/// (e.g. `"::::"`) — the caller does not propagate that as an error, because
/// an unparseable `E2E_VHP_BASE_URL` should still reach the run; only the
/// domain derived from it is missing.
fn base_domain_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    url::Url::parse(&with_scheme)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
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
/// under `cpt-cf-qa-fr-migration-runner-contract`. Existing test repositories
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
pub fn assemble_collect_env(collect_url: &str) -> BTreeMap<String, String> {
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
    use super::*;
    use uuid::Uuid;

    fn v(name: &str, value: &str) -> EnvVar {
        EnvVar {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    fn inputs() -> EnvInputs {
        EnvInputs {
            statics: vec![v("TEST_FILES", "a.py,b.py")],
            variables: TieredVariables::default(),
            platform_base_url: None,
            platform_namespace: None,
            parameters: vec![],
            kubeconfig_path: None,
        }
    }

    #[test]
    fn statics_alone_pass_through() {
        let env = assemble(inputs());
        assert_eq!(env.get("TEST_FILES").map(String::as_str), Some("a.py,b.py"));
        assert_eq!(env.len(), 1);
    }

    /// The four-tier chain, each tier overriding the one before it
    /// (`cpt-cf-qa-fr-runs-env-assembly`; `services/argo.rs:436-499`).
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

        i.parameters.push(v("SHARED", "param"));
        let env = assemble(i);
        assert_eq!(
            env.get("SHARED").map(String::as_str),
            Some("param"),
            "per-launch parameters are the most specific value for this run"
        );
    }

    /// The fifth position: platform metadata's base URL is pushed AFTER the
    /// platform variables, so it beats a platform variable of the same name
    /// (`services/argo.rs:493-496`) — but a run parameter still beats it.
    #[test]
    fn the_platform_base_url_overrides_a_platform_variable_but_not_a_parameter() {
        let mut i = inputs();
        i.variables
            .platform
            .push(v("E2E_VHP_BASE_URL", "from-variable"));
        i.platform_base_url = Some("from-metadata".to_owned());
        let env = assemble(i.clone());
        assert_eq!(
            env.get("E2E_VHP_BASE_URL").map(String::as_str),
            Some("from-metadata")
        );

        i.parameters.push(v("E2E_VHP_BASE_URL", "from-parameter"));
        let env = assemble(i);
        assert_eq!(
            env.get("E2E_VHP_BASE_URL").map(String::as_str),
            Some("from-parameter"),
            "E2E_VHP_BASE_URL is not reserved, so a parameter overrides it (decision D3)"
        );
    }

    /// A blank base URL is *absent*, not an empty override: the source system
    /// guards the push with `.filter(|v| !v.trim().is_empty())`
    /// (`services/argo.rs:493`). Without this, an unset metadata field would
    /// silently blank out a platform variable of the same name.
    #[test]
    fn a_blank_platform_base_url_is_treated_as_absent() {
        let mut i = inputs();
        i.variables
            .platform
            .push(v("E2E_VHP_BASE_URL", "from-variable"));
        i.platform_base_url = Some("   ".to_owned());
        let env = assemble(i);
        assert_eq!(
            env.get("E2E_VHP_BASE_URL").map(String::as_str),
            Some("from-variable")
        );
    }

    /// `VPADM_BASE_DOMAIN` shares `E2E_VHP_BASE_URL`'s position and precedence:
    /// both are derived from the platform's metadata and pushed at the fifth
    /// position (spec §4.7).
    #[test]
    fn the_base_domain_accompanies_the_base_url_and_shares_its_precedence() {
        let mut i = inputs();
        i.platform_base_url = Some("https://sv.jele.io".to_owned());
        let env = assemble(i);
        assert_eq!(
            env.get("E2E_VHP_BASE_URL").map(String::as_str),
            Some("https://sv.jele.io")
        );
        assert_eq!(
            env.get("VPADM_BASE_DOMAIN").map(String::as_str),
            Some("sv.jele.io")
        );
    }

    /// `base_domain_from_url` prepends `https://` when the input has no
    /// `"://"`, so a scheme-less base URL still yields a bare domain
    /// (`manager/src/services/argo.rs:2476`).
    #[test]
    fn a_scheme_less_base_url_still_yields_a_bare_domain() {
        let mut i = inputs();
        i.platform_base_url = Some("sv.jele.io".to_owned());
        let env = assemble(i);
        assert_eq!(
            env.get("VPADM_BASE_DOMAIN").map(String::as_str),
            Some("sv.jele.io")
        );
    }

    /// An unparseable base URL must not silently suppress the value an
    /// operator set: `E2E_VHP_BASE_URL` still reaches the run, and only the
    /// domain derived from it is absent.
    #[test]
    fn an_unparseable_base_url_yields_the_url_without_a_domain_rather_than_failing() {
        let mut i = inputs();
        i.platform_base_url = Some("::::".to_owned());
        let env = assemble(i);
        assert_eq!(
            env.get("E2E_VHP_BASE_URL").map(String::as_str),
            Some("::::")
        );
        assert!(!env.contains_key("VPADM_BASE_DOMAIN"));
    }

    /// A blank base URL is absent for `VPADM_BASE_DOMAIN` too, not just
    /// `E2E_VHP_BASE_URL` — there is nothing to derive a domain from.
    #[test]
    fn a_blank_base_url_contributes_neither_variable() {
        let mut i = inputs();
        i.platform_base_url = Some("   ".to_owned());
        let env = assemble(i);
        assert!(!env.contains_key("E2E_VHP_BASE_URL"));
        assert!(!env.contains_key("VPADM_BASE_DOMAIN"));
    }

    /// `E2E_K8S_NAMESPACE` is populated from the platform's `observed_namespace`
    /// — the column's stated purpose in the system being ported from: auto-filled
    /// "so runs get a correct `E2E_K8S_NAMESPACE`, used by tests'
    /// Keycloak/credstore auto-discovery".
    #[test]
    fn the_observed_namespace_becomes_e2e_k8s_namespace() {
        let mut i = inputs();
        i.platform_namespace = Some("virtuozzo".to_owned());
        assert_eq!(
            assemble(i).get("E2E_K8S_NAMESPACE").map(String::as_str),
            Some("virtuozzo")
        );
    }

    /// `KUBECONFIG` is pushed last of all and wins outright
    /// (`services/argo.rs:504-521`). Reserved, so a parameter cannot reach it —
    /// but the ordering is asserted here rather than left to the reserved-list
    /// check, so removing that check cannot silently break this too.
    #[test]
    fn the_kubeconfig_path_wins_outright() {
        let mut i = inputs();
        i.parameters.push(v("KUBECONFIG", "attacker"));
        i.kubeconfig_path = Some("/.kube/kubeconfig".to_owned());
        let env = assemble(i);
        assert_eq!(
            env.get("KUBECONFIG").map(String::as_str),
            Some("/.kube/kubeconfig")
        );
    }

    #[test]
    fn a_run_without_a_platform_gets_no_kubeconfig_entry() {
        let env = assemble(inputs());
        assert!(!env.contains_key("KUBECONFIG"));
        assert!(!env.contains_key("E2E_VHP_BASE_URL"));
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
    /// anyway: `EnvVar`'s two fields have the same type, so a transposition
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
        let converted = EnvVar::from(RunParameter {
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
        let converted = EnvVar::from(qa_environments_sdk::Variable {
            id: Uuid::new_v4(),
            platform_id: Some(Uuid::new_v4()),
            name: "NAME".to_owned(),
            value: "VALUE".to_owned(),
        });
        assert_eq!(converted, v("NAME", "VALUE"));
    }

    // ---------- tier splitting ----------

    fn var(platform_id: Option<Uuid>, name: &str, value: &str) -> qa_environments_sdk::Variable {
        qa_environments_sdk::Variable {
            id: Uuid::new_v4(),
            platform_id,
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    /// `platform_id = None` is a global pipeline variable; `Some(target)` is a
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
    /// are frozen (`cpt-cf-qa-fr-migration-runner-contract`).
    #[test]
    fn collect_assembly_sets_exactly_the_two_legacy_variables() {
        let env = assemble_collect_env("https://insights.example/qa/v1/collect/r/main");
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
            let env = assemble_collect_env(blank);
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
