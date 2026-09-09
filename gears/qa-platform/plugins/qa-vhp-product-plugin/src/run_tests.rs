//! The run-facing half: the mount, the four variables, and the reserved set.
//!
//! # Six of these were `qa-runs`' tests, translated rather than copied verbatim
//!
//! `qa-runs::domain::runvars`' test module pinned six rules that are now this
//! module's alone. **Task 18 deleted every one of the originals** — the names
//! below are cited as history, and `qa-runs`' `doc_citations_tests` allowlist
//! carries the six with that reason. Four rules are called out here; the other
//! two are the observed namespace's source and the blank-attribute case, and
//! they are pinned further down this file:
//!
//! * a variable-tier value of `E2E_VHP_BASE_URL` is overridable by a run
//!   parameter — the name is not reserved (decision D3);
//! * `VPADM_BASE_DOMAIN` accompanies `E2E_VHP_BASE_URL` and shares its
//!   precedence;
//! * an unparseable base URL still yields `E2E_VHP_BASE_URL`, suppressing only
//!   the derived domain;
//! * no target ⇒ neither variable is present.
//!
//! Their *assertions* could not come across unchanged, because the thing they
//! assert on could not: `runvars.rs` asserted about `assemble`, the precedence
//! ladder, which stays in `qa-runs` and which this crate has no dependency on
//! and must not acquire one on. Each rule is therefore re-pinned against the
//! surface this crate actually owns — what [`prepare_run_access`] returns, and
//! what [`env_contract`] reserves.
//!
//! **Corrected at the Phase E review (finding I-10).** This header said the
//! originals "stay in `runvars.rs` and keep testing the ladder until Task 18",
//! and Task 18 has landed: they are gone, and these are the only tests that
//! pin the four rules. It also said "four", where `runvars.rs`' own test header
//! says six moved. The values `runvars.rs` used are still the fixtures here —
//! `https://sv.jele.io` -> `sv.jele.io`, and `"::::"` -> the URL with no
//! domain — but there is nothing left to compare them against, so they are
//! recorded rather than cross-checked.
//!
//! # Every value-pinning test here is whole-value
//!
//! `assert_eq!` on the whole string, never `contains`. Twice in this plan a
//! `contains` assertion was satisfied by a prefix of a mutated value and
//! passed a defect; every pin below was mutated and watched to fail before it
//! was kept.

use qa_product_sdk::access::{MountSpec, RunVarContract, RunnerSpec};
use qa_product_sdk::observation::ObservedAttrs;
use qa_product_sdk::plugin::CredentialSlot;

use super::*;
use crate::schemas::{BASE_DOMAIN_KEY, NAMESPACE_KEY};

/// The reference a dispatching process holds: a credstore key, never bytes.
fn slots() -> Vec<CredentialSlot> {
    vec![CredentialSlot::reference_only(
        KUBECONFIG_KEY,
        "qa/vhp/kubeconfig",
    )]
}

/// An environment observed to be `sv-test`: the base URL as
/// [`crate::observe`] writes it (`https://` + the domain) and the namespace
/// the install was found in.
fn observed_sv_test() -> ObservedAttrs {
    let mut attrs = ObservedAttrs::default();
    attrs.set(BASE_DOMAIN_KEY, "https://sv.jele.io");
    attrs.set(NAMESPACE_KEY, "virtuozzo");
    attrs
}

/// Drive [`prepare_run_access`] against `observed`, with a kubeconfig
/// reference and no configuration.
fn access_for(observed: Option<&ObservedAttrs>) -> RunAccess {
    let slots = slots();
    let config = serde_json::Value::Null;
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed,
    };
    prepare_run_access(&env).expect("a kubeconfig reference is all this needs")
}

/// The value of `name`, or `None` when the plugin did not supply it.
///
/// Whole-value by construction: it returns the value for the caller to
/// `assert_eq!` against, and there is no shape of call that makes a
/// substring match.
fn value_of<'a>(access: &'a RunAccess, name: &str) -> Option<&'a str> {
    let mut found = access
        .env
        .iter()
        .filter(|var| var.name == name)
        .map(|var| var.value.as_str());
    let first = found.next();
    assert!(
        found.next().is_none(),
        "`{name}` was supplied twice, so `value_of` would be reporting one of two values"
    );
    first
}

/// The one mount, destructured. Panics if there is not exactly one, because
/// every assertion below is about *the* kubeconfig mount.
fn only_mount(access: &RunAccess) -> (&str, &str, Option<i32>) {
    let [mount] = access.mounts.as_slice() else {
        panic!("VHP mounts exactly one thing, the kubeconfig");
    };
    match mount {
        MountSpec::Secret {
            credstore_ref,
            path,
            mode,
        } => (credstore_ref.as_str(), path.as_str(), *mode),
        MountSpec::ConfigValue { .. } => panic!(
            "the kubeconfig must be mounted by reference: a `ConfigValue` carries plaintext \
             this function is contractually forbidden to have"
        ),
    }
}

// ── Step 3's equality, which is the point of the task ─────────────────────

/// The mount path and the `KUBECONFIG` value are **one string**.
///
/// `qa-runs`' `KubeconfigMount` documented this as "an obligation the port
/// cannot enforce": there, the mount and the variable were built by different
/// functions from different inputs, and nothing compared them. Here one
/// constant feeds both sites, and this is the test that notices if someone
/// writes the literal a second time.
///
/// Asserted three ways on purpose. The first two are whole-value pins against
/// the literal, so a changed path fails loudly rather than silently agreeing
/// with itself; the third is the equality itself, which is what would break if
/// the two sites ever stopped sharing a constant.
#[test]
fn the_mount_path_and_the_kubeconfig_variable_are_one_string() {
    let access = access_for(Some(&observed_sv_test()));
    let (_, path, _) = only_mount(&access);

    assert_eq!(path, "/.kube/kubeconfig");
    assert_eq!(value_of(&access, "KUBECONFIG"), Some("/.kube/kubeconfig"));
    assert_eq!(
        Some(path),
        value_of(&access, "KUBECONFIG"),
        "a run whose KUBECONFIG points anywhere but the mount has no kubeconfig at all"
    );
}

/// The kubeconfig crosses as a **reference**, at mode `0o400`.
///
/// The `credstore_ref` is the one the handle carried, verbatim: the executor
/// resolves it, and a plugin that invented or decorated a reference would
/// mount the wrong secret or nothing.
#[test]
fn the_kubeconfig_is_mounted_by_reference_read_only() {
    let access = access_for(Some(&observed_sv_test()));

    assert_eq!(
        only_mount(&access),
        ("qa/vhp/kubeconfig", "/.kube/kubeconfig", Some(0o400))
    );
}

/// The bytes are never read, so the access is identical whether or not the
/// caller resolved them.
///
/// `assert_no_leak` compares the two drives itself and is the enforcing gate;
/// the property is restated here because it is a property of *this* function
/// and a reader of this module should be able to see it stated.
///
/// The two comparisons are **not** the same one, and neither reaches inside a
/// secret:
///
/// * The harness compares field by field — run variables keyed by name, mount
///   shapes, the service account — and its own doc records that a
///   `ConfigValue`'s bytes are deliberately excluded, because a divergence
///   report must never print them.
/// * This test compares the whole `Debug` rendering, which is the only
///   comparison available to it: `RunAccess` derives `Debug` and nothing else
///   (`qa-product-sdk/src/access.rs:23`), so there is no `PartialEq` to reach
///   for.
///
/// So what this assertion covers is a divergence in the access's
/// **structure**: a mount present on one drive only, a different mount path or
/// mode, a run variable added, renamed or revalued. A divergence *inside* a
/// secret would not fail it — `MountSpec`'s hand-written `Debug` renders a
/// `ConfigValue`'s bytes as `<redacted>` (`qa-product-sdk/src/access.rs:73-78`)
/// — and covering the redacted interiors is `assert_no_leak`'s canary's job,
/// not this comparison's.
#[test]
fn resolving_the_kubeconfig_changes_nothing_about_the_access() {
    let observed = observed_sv_test();
    let config = serde_json::Value::Null;

    let by_reference = access_for(Some(&observed));

    let resolved = vec![CredentialSlot::resolved(
        KUBECONFIG_KEY,
        "qa/vhp/kubeconfig",
        credstore_sdk::SecretValue::from("apiVersion: v1\nkind: Config\n".to_owned()),
    )];
    let with_plaintext = prepare_run_access(&EnvironmentHandle {
        slots: &resolved,
        config: &config,
        observed: Some(&observed),
    })
    .expect("the resolved shape is the same shape");

    assert_eq!(
        format!("{by_reference:?}"),
        format!("{with_plaintext:?}"),
        "prepare_run_access must build its mount from `credstore_ref` alone"
    );
}

// ── The four rules ported from `qa-runs::domain::runvars` ─────────────────

/// **Rule 2.** `VPADM_BASE_DOMAIN` accompanies `E2E_VHP_BASE_URL`: same
/// source, same position, and the bare host derived from the URL.
///
/// Replaces `runvars.rs`' `the_base_domain_accompanies_the_base_url_and_shares_its_precedence`
/// (deleted by Task 18), with the same fixture. There it read the URL off
/// `platform_base_url`; here it reads it off the `baseDomain` observed
/// attribute, which is where that column's successor lives.
#[test]
fn the_base_domain_accompanies_the_base_url() {
    let access = access_for(Some(&observed_sv_test()));

    assert_eq!(
        value_of(&access, "E2E_VHP_BASE_URL"),
        Some("https://sv.jele.io")
    );
    assert_eq!(value_of(&access, "VPADM_BASE_DOMAIN"), Some("sv.jele.io"));
}

/// **Rule 3.** An unparseable base URL must not silently suppress the value
/// the cluster reported: `E2E_VHP_BASE_URL` still reaches the run, and only
/// the domain derived from it is absent.
///
/// Replaces `runvars.rs`' `an_unparseable_base_url_yields_the_url_without_a_domain_rather_than_failing`
/// (deleted by Task 18), same fixture, and it still must not be an error.
#[test]
fn an_unparseable_base_url_yields_the_url_without_a_domain() {
    let mut attrs = ObservedAttrs::default();
    attrs.set(BASE_DOMAIN_KEY, "::::");
    let access = access_for(Some(&attrs));

    assert_eq!(value_of(&access, "E2E_VHP_BASE_URL"), Some("::::"));
    assert_eq!(value_of(&access, "VPADM_BASE_DOMAIN"), None);
}

/// **Rule 4.** Nothing observed ⇒ neither variable is present, and the call
/// still succeeds.
///
/// `runvars.rs`' `a_run_without_a_platform_gets_no_kubeconfig_entry` (deleted
/// by Task 18) asserted the absence of `E2E_VHP_BASE_URL` for a run with no
/// target. The shape that
/// reaches a plugin is different — the environment exists, it has simply never
/// been observed — but the rule is the same one, and here it has teeth the
/// original did not: `assert_no_leak` drives `prepare_run_access` with
/// `observed: None`, so an implementation that errored on this shape would
/// fail this crate's leak gate rather than merely omit a variable.
///
/// The variables are **omitted**, not defaulted and not blank: a blank
/// `E2E_K8S_NAMESPACE` reaching every test in the run is a worse failure than
/// an absent one, which is the rule `project_roles` already applies to a blank
/// attribute.
#[test]
fn a_never_observed_environment_yields_the_kubeconfig_and_nothing_else() {
    let access = access_for(None);

    assert_eq!(value_of(&access, "KUBECONFIG"), Some("/.kube/kubeconfig"));
    assert_eq!(value_of(&access, "E2E_VHP_BASE_URL"), None);
    assert_eq!(value_of(&access, "VPADM_BASE_DOMAIN"), None);
    assert_eq!(value_of(&access, "E2E_K8S_NAMESPACE"), None);
    assert_eq!(
        access.env.len(),
        1,
        "an unobserved environment contributes no run variable but the mount's own path"
    );
}

/// **Rule 1.** `E2E_VHP_BASE_URL` is not reserved, so a run parameter of that
/// name overrides it (decision D3).
///
/// `runvars.rs`' `the_platform_base_url_overrides_a_platform_variable_but_not_a_parameter`
/// (deleted by Task 18) asserted the override by running the ladder — its
/// product-neutral successor is
/// `a_plugin_variable_overrides_an_environment_variable_but_not_a_parameter`,
/// and the end-to-end one is `dispatch_tests`'
/// `a_run_parameter_overrides_a_plugin_supplied_variable`. The ladder is not
/// this crate's,
/// so what is pinned here is the input the ladder decides from: the name is
/// absent from the reserved set, both as this plugin declares it and after the
/// union with the platform's floor — which is where a run parameter's
/// admissibility is actually checked.
///
/// `VPADM_BASE_DOMAIN` is asserted alongside it because it shares that
/// position and precedence, and reserving it would break the pair.
#[test]
fn the_base_url_pair_is_not_reserved_so_a_run_parameter_can_override_it() {
    let contract = env_contract();
    let union = contract.union_with_floor(&["TEST_FILES", "TEST_BUNDLE_URL", "TEST_VERSION"]);

    assert!(!contract.reserved.contains("E2E_VHP_BASE_URL"));
    assert!(!contract.reserved.contains("VPADM_BASE_DOMAIN"));
    assert!(!union.contains("E2E_VHP_BASE_URL"));
    assert!(!union.contains("VPADM_BASE_DOMAIN"));
}

// ── The rest of the contract ──────────────────────────────────────────────

/// The observed namespace becomes `E2E_K8S_NAMESPACE` — the attribute's whole
/// purpose, in the system this is ported from: auto-filled "so runs get a
/// correct `E2E_K8S_NAMESPACE`, used by tests' Keycloak/credstore
/// auto-discovery".
#[test]
fn the_observed_namespace_becomes_the_namespace_variable() {
    let access = access_for(Some(&observed_sv_test()));

    assert_eq!(value_of(&access, "E2E_K8S_NAMESPACE"), Some("virtuozzo"));
}

/// The whole set, pinned as a set: four names and no fifth.
///
/// The individual pins above would all pass if the plugin also supplied a
/// variable nobody asked for — including one carrying something it read. This
/// is the assertion that cannot.
#[test]
fn a_fully_observed_environment_yields_exactly_the_four_variables() {
    let access = access_for(Some(&observed_sv_test()));

    let mut supplied: Vec<(&str, &str)> = access
        .env
        .iter()
        .map(|var| (var.name.as_str(), var.value.as_str()))
        .collect();
    supplied.sort_unstable();

    assert_eq!(
        supplied,
        vec![
            ("E2E_K8S_NAMESPACE", "virtuozzo"),
            ("E2E_VHP_BASE_URL", "https://sv.jele.io"),
            ("KUBECONFIG", "/.kube/kubeconfig"),
            ("VPADM_BASE_DOMAIN", "sv.jele.io"),
        ]
    );
}

/// A blank observed attribute is *absent*, not an empty override —
/// `project_roles` treats a blank as unset, and this asserts the plugin does
/// not undo that by writing the empty string through.
#[test]
fn blank_observed_attributes_contribute_no_variables() {
    let mut attrs = ObservedAttrs::default();
    attrs.set(BASE_DOMAIN_KEY, "   ");
    attrs.set(NAMESPACE_KEY, "");
    let access = access_for(Some(&attrs));

    assert_eq!(value_of(&access, "E2E_VHP_BASE_URL"), None);
    assert_eq!(value_of(&access, "VPADM_BASE_DOMAIN"), None);
    assert_eq!(value_of(&access, "E2E_K8S_NAMESPACE"), None);
}

/// `base_domain_from_url` prepends `https://` when the input has no `"://"`,
/// so a scheme-less base URL still yields a bare domain. Copied from
/// `runvars.rs`' `a_scheme_less_base_url_still_yields_a_bare_domain` (deleted
/// by Task 18), because
/// the branch is reachable: `baseDomain` is normally written with the scheme,
/// but an operator-corrected or older-format value need not have one.
#[test]
fn a_scheme_less_base_url_still_yields_a_bare_domain() {
    let mut attrs = ObservedAttrs::default();
    attrs.set(BASE_DOMAIN_KEY, "sv.jele.io");
    let access = access_for(Some(&attrs));

    assert_eq!(value_of(&access, "VPADM_BASE_DOMAIN"), Some("sv.jele.io"));
    assert_eq!(value_of(&access, "E2E_VHP_BASE_URL"), Some("sv.jele.io"));
}

/// Port, userinfo and path are stripped; the host alone survives. This is
/// what delegating to `url::Url` buys over splitting on `/`, and it is the
/// stated reason the original does so.
#[test]
fn the_derived_domain_is_the_bare_host() {
    let mut attrs = ObservedAttrs::default();
    attrs.set(BASE_DOMAIN_KEY, "https://user:pw@sv.jele.io:8443/console");
    let access = access_for(Some(&attrs));

    assert_eq!(value_of(&access, "VPADM_BASE_DOMAIN"), Some("sv.jele.io"));
}

/// An environment with no kubeconfig has no access to prepare. Classified,
/// with fixed text that names nothing the environment carries.
#[test]
fn an_environment_without_a_kubeconfig_is_a_classified_failure() {
    let config = serde_json::Value::Null;
    let failure = prepare_run_access(&EnvironmentHandle {
        slots: &[],
        config: &config,
        observed: None,
    })
    .expect_err("there is no reference to mount");

    assert_eq!(
        failure,
        PluginFailure::classified(
            FailureClass::Internal,
            "this environment has no kubeconfig: add one on the environment's credentials form"
        )
    );
}

/// VHP inherits the deployment-wide runner image: every field of the spec is
/// empty. Pinned field by field rather than against `RunnerSpec::default()`,
/// so a future default that grew a value would fail here instead of agreeing
/// with itself.
#[test]
fn the_runner_inherits_the_deployment_image() {
    let spec = runner();

    assert_eq!(spec.image, None);
    assert_eq!(spec.command, Vec::<String>::new());
    assert_eq!(spec.image_pull_policy, None);
    assert_eq!(spec, RunnerSpec::default());
}

/// The reserved set is exactly the two names that are facts about this
/// product's target. Pinned as a whole set: a name added here silently makes
/// a run parameter stop working, and a name removed here silently lets one
/// overwrite a mount path.
#[test]
fn vhp_reserves_the_namespace_and_the_kubeconfig_and_nothing_else() {
    let contract = env_contract();
    let reserved: Vec<&str> = contract.reserved.iter().map(String::as_str).collect();

    assert_eq!(reserved, vec!["E2E_K8S_NAMESPACE", "KUBECONFIG"]);
}

/// The platform's floor survives the union whatever the plugin declares. The
/// plugin owns names and values; it never owns precedence, and it cannot
/// shrink the floor even by returning a contract that mentions none of it.
///
/// Driven with an empty contract as well as VHP's own, because "cannot shrink"
/// is a property of the union and not of what VHP happens to reserve.
#[test]
fn the_platform_floor_survives_whatever_the_plugin_declares() {
    let floor = [
        "TEST_FILES",
        "TEST_BUNDLE_URL",
        "TEST_VERSION",
        "APP_VERSION",
    ];

    for contract in [env_contract(), RunVarContract::default()] {
        let union = contract.union_with_floor(&floor);
        for name in floor {
            assert!(
                union.contains(name),
                "`{name}` left the reserved set when the plugin's contract was unioned into it"
            );
        }
    }
}
