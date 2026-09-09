//! The pure parsers, against the exact strings measured on the live stand;
//! the pre-flight checks `open_session` makes with no network reached at all;
//! and the orchestration, driven against a real `sshd` fixture so
//! `observe_environment`'s three-step sequence -- and `health_from`'s
//! independent read -- are proved against an actual session rather than
//! against their own copy of the rules.

use credstore_sdk::SecretValue;
use qa_connector_ssh::test_support::{RawBuffer, SshdFixture};
use qa_connector_ssh::{SshSession, SshTarget};
use qa_product_sdk::observation::{FailureClass, HealthOutcome, HealthState, ObservationOutcome};
use qa_product_sdk::plugin::{CredentialSlot, EnvironmentHandle};

use super::{
    ADMIN_PORT, KEY_NOT_RESOLVED, NODE_HOST_NOT_CONFIGURED, ObserveTarget, PASSWORD_NOT_RESOLVED,
    Release, health_from, node_count, observe, observe_environment, open_session, parse_release,
    shell_quote,
};
use crate::schemas::{
    BASE_URL_KEY, BUILD_KEY, DEFAULT_VINFRA_USERNAME, NODE_COUNT_KEY, NODE_HOST_KEY,
    PRODUCT_VERSION_KEY, RAW_RELEASE_KEY, SSH_PORT_KEY, SSH_PRIVATE_KEY_KEY, SSH_USER_KEY,
    STORAGE_NAME_KEY, VINFRA_PASSWORD_KEY,
};

// ── the pure helpers ───────────────────────────────────────────────────────

#[test]
fn the_live_stands_release_line_parses() {
    // Measured on root@10.136.31.154, 2026-09-08.
    let parsed = parse_release("Virtuozzo Infrastructure 7.4.0 (82)").expect("parsed");
    assert_eq!(
        parsed,
        Release {
            version: "7.4.0".to_owned(),
            build: "82".to_owned(),
        }
    );
}

#[test]
fn surrounding_whitespace_and_a_trailing_newline_do_not_matter() {
    let parsed = parse_release("  Virtuozzo Infrastructure 7.4.0 (82)\n").expect("parsed");
    assert_eq!(parsed.version, "7.4.0");
    assert_eq!(parsed.build, "82");
}

#[test]
fn a_four_component_version_keeps_all_four() {
    let parsed = parse_release("Virtuozzo Infrastructure 7.4.0.1 (82)").expect("parsed");
    assert_eq!(parsed.version, "7.4.0.1");
}

#[test]
fn a_line_with_no_parenthesised_build_does_not_parse() {
    assert!(parse_release("Virtuozzo Infrastructure 7.4.0").is_none());
}

#[test]
fn a_non_numeric_build_does_not_parse() {
    assert!(parse_release("Virtuozzo Infrastructure 7.4.0 (beta)").is_none());
}

#[test]
fn an_empty_release_file_does_not_parse() {
    assert!(parse_release("").is_none());
}

#[test]
fn the_live_stands_node_list_counts_three() {
    let json = r#"[
        {"id": "a", "host": "ve0", "is_primary": false, "is_online": true},
        {"id": "b", "host": "ve1", "is_primary": false, "is_online": true},
        {"id": "c", "host": "master", "is_primary": true, "is_online": true}
    ]"#;
    assert_eq!(node_count(json), Some(3));
}

#[test]
fn an_empty_node_list_counts_zero_rather_than_failing() {
    assert_eq!(node_count("[]"), Some(0));
}

#[test]
fn a_non_array_answer_does_not_count() {
    assert_eq!(node_count(r#"{"error": "unauthorized"}"#), None);
}

#[test]
fn unparseable_output_does_not_count() {
    assert_eq!(node_count("vinfra: error: unauthorized"), None);
}

// ── the stricter version rule (findings 7/8) ──────────────────────────────

#[test]
fn a_version_missing_its_final_component_does_not_parse() {
    assert!(parse_release("Virtuozzo Infrastructure 7.4. (82)").is_none());
}

#[test]
fn a_bare_dot_does_not_parse_as_a_version() {
    assert!(parse_release("Virtuozzo Infrastructure . (82)").is_none());
}

#[test]
fn a_non_numeric_dotted_version_does_not_parse() {
    assert!(parse_release("Virtuozzo Infrastructure beta.rc (82)").is_none());
}

#[test]
fn a_version_with_trailing_junk_after_its_last_component_does_not_parse() {
    assert!(parse_release("Virtuozzo Infrastructure 7.4.0-rc1 (82)").is_none());
}

#[test]
fn a_well_formed_first_line_parses_even_when_a_second_line_has_its_own_parens() {
    let parsed =
        parse_release("Virtuozzo Infrastructure 7.4.0 (82)\nsome other junk (not-a-build)")
            .expect("the first line alone must parse");
    assert_eq!(parsed.version, "7.4.0");
    assert_eq!(parsed.build, "82");
}

// ── shell quoting (finding 3) ──────────────────────────────────────────────

#[test]
fn shell_quoting_survives_a_value_containing_a_quote_and_a_semicolon_through_a_real_shell() {
    let malicious = "o'clock; touch /tmp/should-not-exist-vhi-shell-quote-test; echo '";
    let quoted = shell_quote(malicious);
    let script = format!("VAR={quoted}; printf '%s' \"$VAR\"");

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("spawning `sh` must succeed on any host that can run these tests");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        malicious,
        "the quoted value must reach the shell variable byte-for-byte, as inert data"
    );
}

// ── the orchestration ──────────────────────────────────────────────────────

/// The well-formed release line the happy-path tests plant, exactly as
/// measured on the live stand.
const GOOD_RELEASE_LINE: &str = "Virtuozzo Infrastructure 7.4.0 (82)";

/// A command that plants `GOOD_RELEASE_LINE` on stdout without needing a real
/// `/etc/hci-release` -- `observe_environment` takes the release command as a
/// parameter for exactly this reason.
fn good_release_command() -> String {
    format!("printf '%s' '{GOOD_RELEASE_LINE}'")
}

/// The about document exactly as measured on the live stand.
const GOOD_ABOUT_BODY: &str =
    r#"{"storage-release": {"version": "7.4.0", "release": "56"}, "storage-name": "hciHeat"}"#;

fn good_about_command() -> String {
    format!("printf '%s' '{GOOD_ABOUT_BODY}'")
}

/// A three-entry `vinfra node list -f json`, shaped like the live stand's.
const GOOD_NODE_LIST_JSON: &str = r#"[
    {"id": "a", "host": "cms-demo-1-ve0.vstoragedomain", "is_primary": false, "is_online": true},
    {"id": "b", "host": "cms-demo-1-ve1.vstoragedomain", "is_primary": false, "is_online": true},
    {"id": "c", "host": "cms-demo-1-ve2.vstoragedomain", "is_primary": true, "is_online": true}
]"#;

fn good_node_list_command() -> String {
    format!("printf '%s' '{GOOD_NODE_LIST_JSON}'")
}

/// A command that always fails, standing in for a `curl`/`vinfra` a given
/// test is not exercising.
const ALWAYS_FAILS: &str = "false";

fn target(fixture: &SshdFixture) -> SshTarget {
    SshTarget {
        host: "127.0.0.1".to_owned(),
        port: fixture.port(),
        user: fixture.user().to_owned(),
    }
}

fn open(fixture: &SshdFixture) -> SshSession {
    let key = SecretValue::from(fixture.client_key_pem().to_owned());
    SshSession::open(target(fixture), &key).expect("session")
}

fn config_with_host() -> serde_json::Value {
    serde_json::json!({ NODE_HOST_KEY: "cms-demo-1" })
}

fn observe_target() -> ObserveTarget<'static> {
    ObserveTarget {
        host: "cms-demo-1",
        vinfra_username: DEFAULT_VINFRA_USERNAME,
    }
}

fn resolved_password(value: &str) -> Vec<CredentialSlot> {
    vec![CredentialSlot::resolved(
        VINFRA_PASSWORD_KEY,
        "qa/vhi/vinfra_password",
        SecretValue::from(value.to_owned()),
    )]
}

fn no_slots() -> Vec<CredentialSlot> {
    Vec::new()
}

#[tokio::test]
async fn a_well_formed_release_yields_the_version_build_and_raw_line() {
    let Some(fixture) = SshdFixture::start() else {
        return; // no sshd on this host; the fixture said so
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let outcome = observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        ALWAYS_FAILS,
        ALWAYS_FAILS,
    )
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("expected Detected, got {outcome:?}");
    };
    assert_eq!(attrs.get(PRODUCT_VERSION_KEY), Some("7.4.0"));
    assert_eq!(attrs.get(BUILD_KEY), Some("82"));
    assert_eq!(attrs.get(RAW_RELEASE_KEY), Some(GOOD_RELEASE_LINE));
}

#[tokio::test]
async fn a_missing_release_file_fails_as_not_found() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let outcome = observe_environment(
        &session,
        &observe_target(),
        &env,
        "cat /this/path/does/not/exist/on/the/fixture",
        ALWAYS_FAILS,
        ALWAYS_FAILS,
    )
    .await;

    let ObservationOutcome::Failed(failure) = outcome else {
        panic!("expected Failed, got {outcome:?}");
    };
    assert_eq!(failure.class, FailureClass::NotFound);
    assert!(
        failure.remote_message.is_some(),
        "cat's own diagnostic must survive as remote_message"
    );
}

#[tokio::test]
async fn a_gibberish_release_file_fails_as_malformed_without_leaking_its_text_into_detail() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let outcome = observe_environment(
        &session,
        &observe_target(),
        &env,
        "printf '%s' 'not a release line at all'",
        ALWAYS_FAILS,
        ALWAYS_FAILS,
    )
    .await;

    let ObservationOutcome::Failed(failure) = outcome else {
        panic!("expected Failed, got {outcome:?}");
    };
    assert_eq!(failure.class, FailureClass::Malformed);
    let detail = failure.detail.expect("a fixed detail");
    assert!(
        !detail.contains("not a release line"),
        "the raw text must not reach `detail`: {detail}"
    );
    assert_eq!(
        failure.remote_message.as_deref(),
        Some("not a release line at all"),
        "the raw text must still survive, on remote_message"
    );
}

#[tokio::test]
async fn an_about_document_that_does_not_answer_omits_the_base_url_but_still_detects() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let outcome = observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        ALWAYS_FAILS,
        ALWAYS_FAILS,
    )
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("expected Detected, got {outcome:?}");
    };
    assert_eq!(attrs.get(BASE_URL_KEY), None);
    assert_eq!(attrs.get(PRODUCT_VERSION_KEY), Some("7.4.0"));
}

#[tokio::test]
async fn an_about_document_that_exits_zero_with_an_unparseable_body_also_omits_the_base_url() {
    // `curl -ks` exits 0 on a 404/500 body; a body that isn't even JSON must
    // not read as "confirmed reachable" (finding 4 / the small item on
    // `read_about`).
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let outcome = observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        "printf '%s' '<html>not json</html>'",
        ALWAYS_FAILS,
    )
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("expected Detected, got {outcome:?}");
    };
    assert_eq!(attrs.get(BASE_URL_KEY), None);
}

#[tokio::test]
async fn a_well_formed_about_document_sets_the_base_url_and_storage_name() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let outcome = observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        &good_about_command(),
        ALWAYS_FAILS,
    )
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("expected Detected, got {outcome:?}");
    };
    assert_eq!(
        attrs.get(BASE_URL_KEY),
        Some(format!("https://cms-demo-1:{ADMIN_PORT}")).as_deref()
    );
    assert_eq!(attrs.get(STORAGE_NAME_KEY), Some("hciHeat"));
}

#[tokio::test]
async fn a_vinfra_that_exits_non_zero_omits_the_node_count_but_leaves_the_rest_intact() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = resolved_password("s3cr3t-vinfra-password");
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let outcome = observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        ALWAYS_FAILS,
        ALWAYS_FAILS,
    )
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("expected Detected, got {outcome:?}");
    };
    assert_eq!(attrs.get(NODE_COUNT_KEY), None);
    assert_eq!(attrs.get(PRODUCT_VERSION_KEY), Some("7.4.0"));
    assert_eq!(attrs.get(BUILD_KEY), Some("82"));
}

#[tokio::test]
async fn a_well_formed_node_list_sets_the_node_count() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = resolved_password("s3cr3t-vinfra-password");
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let outcome = observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        ALWAYS_FAILS,
        &good_node_list_command(),
    )
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("expected Detected, got {outcome:?}");
    };
    assert_eq!(attrs.get(NODE_COUNT_KEY), Some("3"));
}

/// A `node_host` containing a single quote and a semicolon must not split the
/// remote command into two, nor let the injected text execute -- finding 3.
///
/// The node-list command prints `VINFRA_PORTAL` back wrapped as a one-element
/// JSON array. If the malicious host were not quoted, the shell would split
/// on `;` and run an extra `echo`, whose own stdout would land *before* the
/// `printf`'s and break the JSON the real command produces -- so `node_count`
/// coming back `Some(1)` is itself the proof the command was not split, and
/// not merely that nothing crashed.
#[tokio::test]
async fn a_node_host_containing_shell_metacharacters_does_not_split_the_remote_command() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let malicious_host = "x'; echo INJECTED; echo '";
    let config = serde_json::json!({ NODE_HOST_KEY: malicious_host });
    let slots = resolved_password("s3cr3t-vinfra-password");
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };
    let target = ObserveTarget {
        host: malicious_host,
        vinfra_username: DEFAULT_VINFRA_USERNAME,
    };

    let outcome = observe_environment(
        &session,
        &target,
        &env,
        &good_release_command(),
        ALWAYS_FAILS,
        r#"printf '["%s"]' "$VINFRA_PORTAL""#,
    )
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("expected Detected, got {outcome:?}");
    };
    assert_eq!(
        attrs.get(NODE_COUNT_KEY),
        Some("1"),
        "an injected `;` must not have split the command or leaked extra stdout"
    );
}

// ── open_session: internal, not unreachable (finding 6) ───────────────────

#[tokio::test]
async fn an_absent_node_host_is_internal_not_unreachable() {
    let config = serde_json::json!({});
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let Err(failure) = open_session(&env).await else {
        panic!("must fail without a host");
    };
    assert_eq!(failure.class, FailureClass::Internal);
    assert_eq!(failure.detail, Some(NODE_HOST_NOT_CONFIGURED));
}

#[tokio::test]
async fn a_blank_node_host_is_treated_as_absent() {
    let config = serde_json::json!({ NODE_HOST_KEY: "   " });
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let Err(failure) = open_session(&env).await else {
        panic!("a blank host must not count as set");
    };
    assert_eq!(failure.class, FailureClass::Internal);
    assert_eq!(failure.detail, Some(NODE_HOST_NOT_CONFIGURED));
}

#[tokio::test]
async fn a_host_present_but_no_resolved_key_is_internal_not_unreachable() {
    let config = serde_json::json!({ NODE_HOST_KEY: "cms-demo-1" });
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let Err(failure) = open_session(&env).await else {
        panic!("must fail without a resolved key");
    };
    assert_eq!(failure.class, FailureClass::Internal);
    assert_eq!(failure.detail, Some(KEY_NOT_RESOLVED));
}

// ── health_from: parameterised, and agreeing with read_release ───────────

#[tokio::test]
async fn health_from_reports_ok_when_the_command_answers() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);

    let health = health_from(&session, "true").await;

    assert!(matches!(
        health,
        HealthOutcome::Checked {
            state: HealthState::Ok,
            detail: None,
        }
    ));
}

#[tokio::test]
async fn health_from_classifies_a_missing_command_as_not_found_matching_the_environment_half() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);

    let health = health_from(&session, "cat /this/path/does/not/exist/on/the/fixture").await;

    let HealthOutcome::Failed(failure) = health else {
        panic!("expected Failed, got {health:?}");
    };
    assert_eq!(failure.class, FailureClass::NotFound);
}

// ── observe(): the full wiring, including open_session's positive path ───

#[tokio::test]
async fn observe_end_to_end_never_disagrees_with_itself_about_the_same_command() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    // A real session, opened for real -- this is `open_session`'s positive
    // path, which nothing else in this suite drives. Whether the host this
    // test happens to run on has a real `/etc/hci-release` is deliberately
    // not assumed either way (measured: some do); what must always hold is
    // that both channels -- which read the identical `RELEASE_COMMAND` --
    // agree with each other, which is the property finding 2 exists to
    // guarantee.
    let config = serde_json::json!({
        NODE_HOST_KEY: "127.0.0.1",
        SSH_PORT_KEY: fixture.port().to_string(),
        SSH_USER_KEY: fixture.user(),
    });
    let slots = vec![CredentialSlot::resolved(
        SSH_PRIVATE_KEY_KEY,
        "qa/vhi/ssh_private_key",
        SecretValue::from(fixture.client_key_pem().to_owned()),
    )];
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let observation = observe(&env).await;

    match observation.environment {
        ObservationOutcome::Detected(_) => {
            assert!(
                matches!(
                    observation.health,
                    HealthOutcome::Checked {
                        state: HealthState::Ok,
                        ..
                    }
                ),
                "a Detected environment (the release file parsed) must not report unhealthy: {:?}",
                observation.health
            );
        }
        ObservationOutcome::Failed(ref env_failure)
            if env_failure.class == FailureClass::NotFound =>
        {
            // The command itself failed (RELEASE_COMMAND absent, unreadable,
            // ...): `health_from` runs the identical command, so it must
            // land on the identical classification -- the actual property
            // finding 2 exists to guarantee.
            let HealthOutcome::Failed(ref health_failure) = observation.health else {
                panic!(
                    "a command-level failure must not leave health un-failed: {:?}",
                    observation.health
                );
            };
            assert_eq!(
                env_failure.class, health_failure.class,
                "both channels read the identical RELEASE_COMMAND and must classify a \
                 command failure alike"
            );
        }
        // Task 5 parked this arm as a catch-all and the whole-branch
        // review closed it: `Malformed` is the ONLY other class for which
        // "the command itself succeeded" holds. A `Timeout`, an
        // `Unreachable` or an `Internal` here means the read never
        // completed, and asserting health is `Ok` on one of those would
        // be asserting the opposite of the truth -- so anything else is
        // now a panic rather than a silently mis-justified pass.
        ObservationOutcome::Failed(ref env_failure)
            if env_failure.class == FailureClass::Malformed =>
        {
            // `Malformed` means the command itself succeeded -- only
            // `parse_release` rejected the output -- and `health_from` never
            // parses, so it must still be `Ok`. This asymmetry is by design
            // (see this module's header and
            // `a_malformed_but_readable_release_file_fails_the_environment_
            // while_health_stays_ok` below) and must not be mistaken for the
            // two channels disagreeing.
            assert!(
                matches!(
                    observation.health,
                    HealthOutcome::Checked {
                        state: HealthState::Ok,
                        ..
                    }
                ),
                "environment failure class {:?} means the command itself succeeded, so \
                 health must still be Ok: {:?}",
                env_failure.class,
                observation.health
            );
        }
        ObservationOutcome::Failed(ref env_failure) => panic!(
            "a session that opened against the fixture must fail only as NotFound (the \
             release command did not run) or Malformed (it ran and did not parse); {:?} \
             means the read never completed, and this test's reasoning about health does \
             not apply to it: {:?}",
            env_failure.class, observation.health
        ),
    }
}

/// The divergence this module's header states explicitly: a release file
/// that exists and answers, but does not parse, fails the environment half
/// (`Malformed`) while leaving health `Checked{Ok}` -- `health_from` never
/// parses, it only runs the command. Driven directly against a controlled
/// command (not through `observe()`, whose `RELEASE_COMMAND` is fixed) so
/// this is proven deliberately rather than left to whatever
/// `/etc/hci-release` happens to be on the host running the suite.
#[tokio::test]
async fn a_malformed_but_readable_release_file_fails_the_environment_while_health_stays_ok() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };
    let gibberish_command = "printf '%s' 'not a release line at all'";

    let outcome = observe_environment(
        &session,
        &observe_target(),
        &env,
        gibberish_command,
        ALWAYS_FAILS,
        ALWAYS_FAILS,
    )
    .await;
    let health = health_from(&session, gibberish_command).await;
    #[allow(
        clippy::use_debug,
        reason = "a `--no-capture` run of this test is how the divergence this test exists to \
                  prove is checked by eye; nothing here is credential material"
    )]
    {
        println!("environment: {outcome:?}, health: {health:?}");
    }

    let ObservationOutcome::Failed(env_failure) = outcome else {
        panic!("expected Failed, got {outcome:?}");
    };
    assert_eq!(env_failure.class, FailureClass::Malformed);
    assert!(
        matches!(
            health,
            HealthOutcome::Checked {
                state: HealthState::Ok,
                ..
            }
        ),
        "health must stay Ok: the command succeeded, only parsing failed: {health:?}"
    );
}

// ── the four `warn!` sites, and what they are allowed to carry ────────────
//
// `read_about` and `read_node_count` are deliberately never failures: this
// module's header records that only step 1 is fatal, so a `curl` that does
// not answer and a `vinfra` that refuses both leave the observation
// `Detected` with an attribute omitted. That means the *only* place either
// failure is recoverable from is the log -- and until 2026-09-09 not one test
// asserted what any of the four lines contained. `read_node_count`'s general
// arm formats `classify(&failure)`, whose `Display` appends `remote_message`
// (`qa-product-sdk/src/observation.rs:150-163`), so that line legitimately
// carries a remote's own words; what it may never carry is anything derived
// from a credential. These are the drives that hold both halves of that.
//
// Ported from `qa-vhp-product-plugin`'s `observe_tests.rs`, which pins its own
// two `warn!` sites the same way and for the same stated reason.

/// Planted in an about document this module cannot parse. The body is
/// arbitrary remote content -- an nginx error page, a login form carrying a
/// session cookie -- and nothing is recovered by printing it, so the log line
/// for that case says what happened and nothing more.
const BODY_MARKER: &str = "canary-about-body-must-not-be-logged-4c19f7";

/// Planted on the **second** line of a stored vinfra password: the half
/// `IFS= read -r` would silently drop, and the half a log line must not carry.
const SECOND_LINE_MARKER: &str = "canary-truncated-half-1b7e42";

/// A resolved vinfra password that genuinely reaches the remote command, and
/// must reach no log line.
const PASSWORD_CANARY: &str = "canary-vinfra-password-do-not-log-9d3a51";

/// Run `body` with every `tracing` byte emitted on this thread captured.
///
/// A thread-local default subscriber, not `with_default`'s sync-closure form:
/// the calls are `async` and must stay under this subscriber across every
/// `.await`. `#[tokio::test]` defaults to a current-thread runtime, so the
/// whole call runs on this one thread.
async fn capturing_logs<F, T>(body: F) -> (T, String)
where
    F: std::future::Future<Output = T>,
{
    let buffer = RawBuffer::new();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let value = body.await;
    drop(guard);
    (value, buffer.captured())
}

fn line_containing<'a>(captured: &'a str, needle: &str) -> &'a str {
    captured
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("expected a log line containing `{needle}`, got:\n{captured}"))
}

/// `read_about`'s first `warn!`: the admin API did not answer at all. The line
/// must carry the *classified* reason -- fixed text from the connector -- and
/// the observation must still be `Detected`.
#[tokio::test]
async fn a_failed_about_read_is_logged_with_its_classified_reason() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let (outcome, captured) = capturing_logs(observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        // Exits non-zero with a diagnostic on stderr, so the classifier has
        // something to classify and the line has something to carry.
        "printf '%s' 'curl: (7) Failed to connect' >&2; exit 7",
        ALWAYS_FAILS,
    ))
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("an unanswered about document must not fail the observation");
    };
    assert_eq!(
        attrs.get(BASE_URL_KEY),
        None,
        "omitted, not defaulted and not blank"
    );

    let line = line_containing(&captured, "admin API did not answer");
    assert!(
        line.contains("baseUrl omitted"),
        "the line must say what was lost, or it recovers nothing: {line}"
    );
    assert!(
        line.contains("a command on the host exited non-zero"),
        "the logged reason must be the classifier's own fixed text: {line}"
    );
}

/// `read_about`'s second `warn!`: the command exited zero and the body is not
/// JSON. `curl -ks` is not `curl -f`, so an nginx error page arrives this way.
///
/// The body itself must **not** reach the line: this site has no classified
/// failure to name, so it logs a fixed sentence and nothing else. A body that
/// carried a session cookie would otherwise be logged verbatim.
#[tokio::test]
async fn an_unparseable_about_body_is_logged_without_the_body() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let (outcome, captured) = capturing_logs(observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        &format!("printf '%s' '<html>{BODY_MARKER}</html>'"),
        ALWAYS_FAILS,
    ))
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("an unparseable about body must not fail the observation");
    };
    assert_eq!(attrs.get(BASE_URL_KEY), None);

    line_containing(&captured, "admin API answered with an unparseable body");
    assert!(
        !captured.contains(BODY_MARKER),
        "the remote's body must not be logged: it is arbitrary content from a page this \
         module could not parse, and nothing recovers anything by printing it:\n{captured}"
    );
}

/// `read_node_count`'s unresolved-password `warn!`: the environment has no
/// resolvable vinfra password. The line carries the fixed `PASSWORD_NOT_RESOLVED`
/// text and no reference, no key and no bytes.
#[tokio::test]
async fn an_unresolved_vinfra_password_is_logged_with_fixed_text() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = no_slots();
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let (outcome, captured) = capturing_logs(observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        ALWAYS_FAILS,
        &good_node_list_command(),
    ))
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("an unresolvable password must not fail the observation");
    };
    assert_eq!(attrs.get(NODE_COUNT_KEY), None);

    let line = line_containing(&captured, "vinfra node count omitted");
    assert!(
        line.contains(PASSWORD_NOT_RESOLVED),
        "the reason must be the module's own fixed text: {line}"
    );
}

/// `read_node_count`'s named refusal: a stored password containing a line
/// break cannot be delivered intact, and the operator has to be told *that*
/// rather than "vinfra node list failed", which would send them to the
/// cluster.
///
/// The password's own bytes must not reach the line. The marker below is
/// planted on the second line specifically -- the half `IFS= read -r` would
/// have silently dropped.
#[tokio::test]
async fn a_vinfra_password_containing_a_line_break_is_named_not_classified() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = resolved_password(&format!("first-half\n{SECOND_LINE_MARKER}"));
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let (outcome, captured) = capturing_logs(observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        ALWAYS_FAILS,
        &good_node_list_command(),
    ))
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("a stored password that cannot be delivered must not fail the observation");
    };
    assert_eq!(
        attrs.get(NODE_COUNT_KEY),
        None,
        "nothing was counted, so nothing may be recorded"
    );

    let line = line_containing(&captured, "vinfra node count omitted");
    assert!(
        line.contains("line break"),
        "the operator must be sent to the stored credential, not to the cluster: {line}"
    );
    assert!(
        !captured.contains(SECOND_LINE_MARKER) && !captured.contains("first-half"),
        "no part of the password may be logged:\n{captured}"
    );
}

/// `read_node_count`'s general `warn!`: `vinfra` itself failed. The line
/// formats `classify(&failure)`, whose `Display` appends `remote_message` --
/// so the *remote's* own words travel, by design, and nothing derived from
/// the password does.
///
/// Both halves are asserted: the remote's stderr is present (or this line
/// recovers nothing an operator can act on), and the resolved password --
/// planted as a canary and genuinely delivered to the remote command -- is
/// absent from every byte captured.
#[tokio::test]
async fn a_failed_vinfra_call_logs_the_remotes_words_and_never_the_password() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let session = open(&fixture);
    let config = config_with_host();
    let slots = resolved_password(PASSWORD_CANARY);
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let (outcome, captured) = capturing_logs(observe_environment(
        &session,
        &observe_target(),
        &env,
        &good_release_command(),
        ALWAYS_FAILS,
        // Shaped like the live stand's refusal, so this also drives the
        // `AuthRejected` reading `qa_connector_ssh::classify` now gives a
        // remote that was reached and refused us.
        "printf '%s' 'vinfra: error: unauthorized' >&2; exit 2",
    ))
    .await;

    let ObservationOutcome::Detected(attrs) = outcome else {
        panic!("a refused vinfra must not fail the observation -- only step 1 is fatal");
    };
    assert_eq!(attrs.get(NODE_COUNT_KEY), None);
    assert_eq!(
        attrs.get(PRODUCT_VERSION_KEY),
        Some("7.4.0"),
        "the version step 1 proved must survive step 3's refusal"
    );

    let line = line_containing(&captured, "vinfra node list failed");
    assert!(
        line.contains("username and password"),
        "the classified reason must be the AuthRejected reading, not the generic one: {line}"
    );
    // Asserted against the whole capture, not the one line: `ssh` puts its
    // own `Warning: Permanently added ...` on stderr ahead of the remote's,
    // so `remote_message` is genuinely multi-line and the words that matter
    // are on the second line of the emitted event.
    assert!(
        captured.contains("vinfra: error: unauthorized"),
        "the remote's own words are the one text sanctioned to travel, and they are what an \
         operator acts on:\n{captured}"
    );
    assert!(
        !captured.contains(PASSWORD_CANARY),
        "the resolved vinfra password reached the remote command and must reach no log \
         line:\n{captured}"
    );
}
