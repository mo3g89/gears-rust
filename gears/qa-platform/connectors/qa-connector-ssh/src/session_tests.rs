use credstore_sdk::SecretValue;

use super::{SshSession, SshTarget, argv_for_parts};
use crate::test_support::SshdFixture;
use crate::{HOST_KEY_VERIFICATION_OPTIONS, SshFailure};

fn target(fixture: &SshdFixture) -> SshTarget {
    SshTarget {
        host: "127.0.0.1".to_owned(),
        port: fixture.port(),
        user: fixture.user().to_owned(),
    }
}

#[tokio::test]
async fn a_command_runs_and_its_stdout_comes_back() {
    let Some(fixture) = SshdFixture::start() else {
        return; // no sshd on this host; the fixture said so
    };
    let key = SecretValue::from(fixture.client_key_pem().to_owned());
    let session = SshSession::open(target(&fixture), &key).expect("session");

    let out = session.exec("echo hello").await.expect("exec");

    assert_eq!(out.trim(), "hello");
}

#[tokio::test]
async fn a_non_zero_exit_is_a_command_failure_carrying_the_remote_stderr() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let key = SecretValue::from(fixture.client_key_pem().to_owned());
    let session = SshSession::open(target(&fixture), &key).expect("session");

    let error = session
        .exec("echo boom >&2; exit 3")
        .await
        .expect_err("a non-zero exit must not be Ok");

    match error {
        SshFailure::CommandFailed { status, stderr } => {
            assert_eq!(status, 3);
            assert!(
                stderr.contains("boom"),
                "the remote's own text must survive"
            );
        }
        other => panic!("expected CommandFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn a_rejected_key_is_auth_rejected() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    // A syntactically valid key the fixture's `authorized_keys` does not list.
    let key = SecretValue::from(SshdFixture::unauthorised_key_pem().to_owned());
    let session = SshSession::open(target(&fixture), &key).expect("the agent starts either way");

    let error = session
        .exec("true")
        .await
        .expect_err("the host must refuse");

    assert!(
        matches!(error, SshFailure::AuthRejected),
        "expected AuthRejected, got {error:?}"
    );
}

#[tokio::test]
async fn a_secret_reaches_the_remote_command_without_being_echoed_back() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let key = SecretValue::from(fixture.client_key_pem().to_owned());
    let session = SshSession::open(target(&fixture), &key).expect("session");
    let secret = SecretValue::from("s3cr3t-value".to_owned());

    // The command prints the variable's LENGTH, never the value: a test that
    // echoed the secret would put it in this process's own captured output.
    let out = session
        .exec_with_secret_env("TOKEN", &secret, "printf '%s' \"${#TOKEN}\"")
        .await
        .expect("exec");

    assert_eq!(
        out.trim(),
        "12",
        "the remote shell must see the whole value"
    );
}

#[tokio::test]
async fn the_secret_never_appears_in_the_remote_processs_argv() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let key = SecretValue::from(fixture.client_key_pem().to_owned());
    let session = SshSession::open(target(&fixture), &key).expect("session");
    let secret = SecretValue::from("s3cr3t-value".to_owned());

    // THE PROPERTY THE WHOLE STDIN MECHANISM EXISTS FOR, asserted against the
    // kernel rather than against our own formatting: the remote shell reads
    // its OWN /proc/<pid>/cmdline -- which is what any other user on that host
    // could read -- and hands it back. `argv` there is the command string ssh
    // was given, so if the secret were interpolated into the command (the
    // obvious `VAR=value cmd` spelling) it would be in this output.
    let out = session
        .exec_with_secret_env("TOKEN", &secret, "tr '\\0' ' ' < /proc/$$/cmdline")
        .await
        .expect("exec");

    assert!(
        out.contains("TOKEN"),
        "sanity: the wrapper naming the variable IS expected in argv -- if this \
         fails the test is reading the wrong process and proves nothing"
    );
    assert!(
        !out.contains("s3cr3t-value"),
        "a secret in argv is world-readable through /proc/<pid>/cmdline: {out}"
    );
}

#[tokio::test]
async fn a_secret_containing_a_newline_is_refused_before_anything_is_spawned() {
    // Deliberately NOT gated on `SshdFixture::start()`: the refusal happens
    // before `ssh` is ever spawned, so it must hold whether or not this host
    // has an sshd. The target is unreachable on purpose -- reaching the
    // network at all would mean the refusal came too late.
    let target = SshTarget {
        host: "127.0.0.1".to_owned(),
        port: 1,
        user: "nobody".to_owned(),
    };
    let key = SecretValue::from(SshdFixture::unauthorised_key_pem().to_owned());
    let session =
        SshSession::open(target, &key).expect("the agent starts independently of any sshd");

    let secret = SecretValue::from("line-one\nline-two".to_owned());
    let error = session
        .exec_with_secret_env("TOKEN", &secret, "true")
        .await
        .expect_err("a secret containing a newline must be refused, not silently truncated");

    let rendered = error.to_string();
    assert!(
        !rendered.contains("line-one") && !rendered.contains("line-two"),
        "the rendered error must carry no fragment of the secret: {rendered}"
    );
    assert!(
        matches!(error, SshFailure::SecretNotDeliverable),
        "expected SecretNotDeliverable -- a variant of its own, so a plugin can tell an \
         operator which stored value to re-register: {error:?}"
    );
}

// ── the argv rendering of the transport's option set ──────────────────────
//
// Until 2026-09-09 every property below was asserted against
// `agent::ssh_command_for` and against nothing else, while `argv_for` --
// which is what a *product plugin* actually runs -- assembled the same
// options a second time with no test on it at all. Deleting `BatchMode=yes`
// from it broke nothing, and a VHI observation in a tty-less pod would then
// hang on an interactive prompt instead of failing cleanly. Both renderings
// now come from `agent::transport_options`; these are the assertions that
// keep the argv side honest, and they are deliberately the same four
// `agent_tests` makes of the string side.
//
// They drive `argv_for_parts` rather than `SshSession::argv_for` because
// reaching the method needs a live `ssh-agent`: the method is a one-line
// delegation to this function, so what is pinned here is exactly what it
// returns.

/// Every `-o` option in the argv, as `Name=value` (the `-o` markers dropped).
fn argv_options(argv: &[String]) -> Vec<String> {
    argv.windows(2)
        .filter(|pair| pair[0] == "-o")
        .map(|pair| pair[1].clone())
        .collect()
}

fn sample_argv(command: &str) -> Vec<String> {
    argv_for_parts(
        std::path::Path::new("/tmp/agent-xyz/agent.sock"),
        std::path::Path::new("/tmp/agent-xyz/id.pub"),
        &SshTarget {
            host: "vhi-node.example.com".to_owned(),
            port: 2222,
            user: "root".to_owned(),
        },
        command,
    )
}

/// The argv must point `ssh` at *this* agent and must carry every option the
/// design depends on -- `BatchMode=yes` above all, which is what turns an
/// authentication failure in a tty-less container into an error instead of a
/// hang. Mirrors `agent_tests`'
/// `ssh_command_carries_agent_socket_batchmode_and_host_key_options`.
#[test]
fn the_argv_carries_agent_socket_batchmode_and_host_key_options() {
    let options = argv_options(&sample_argv("true"));

    for expected in [
        "IdentityAgent=/tmp/agent-xyz/agent.sock",
        // `IdentitiesOnly=yes` and `IdentityFile=<agent public key>` are a
        // PAIR: `IdentitiesOnly` alone restricts ssh to the *configured*
        // identity files, which with none configured means the default
        // `~/.ssh/id_*` set -- so the agent's key is never offered, and on a
        // host that has a default key ssh silently authenticates as the
        // WRONG identity. Neither may be dropped without the other.
        "IdentityFile=/tmp/agent-xyz/id.pub",
        "IdentitiesOnly=yes",
        "BatchMode=yes",
        "StrictHostKeyChecking=no",
        "UserKnownHostsFile=/dev/null",
    ] {
        assert!(
            options.iter().any(|option| option == expected),
            "`{expected}` must reach the argv every remote command is run with: {options:?}"
        );
    }
    // The path values are NOT quoted here, unlike the command-string
    // rendering: an argv element is already one word, so a quote would become
    // part of the value ssh parses.
    assert!(
        !options.iter().any(|option| option.contains('\'')),
        "an argv element must not be shell-quoted: {options:?}"
    );
}

/// The host-key weakening must stay expressed in exactly one place, so
/// tightening it later is a one-constant change -- on *both* renderings.
/// Mirrors `agent_tests`' `host_key_options_come_from_the_single_documented_constant`.
#[test]
fn the_argvs_host_key_options_come_from_the_single_documented_constant() {
    let options = argv_options(&sample_argv("true"));

    for option in HOST_KEY_VERIFICATION_OPTIONS {
        assert!(
            options.iter().any(|rendered| rendered == option),
            "{option} must reach the argv from the single policy constant: {options:?}"
        );
    }
    assert_eq!(
        HOST_KEY_VERIFICATION_OPTIONS.len(),
        2,
        "the documented policy is exactly StrictHostKeyChecking=no + \
         UserKnownHostsFile=/dev/null; changing it means updating ADR-0005"
    );
}

/// The key must never appear in the argv: `/proc/<pid>/cmdline` is
/// world-readable. This is the assertion that fails if the agent approach is
/// ever "simplified" to `ssh -i <file>`. Mirrors `agent_tests`'
/// `ssh_command_never_carries_key_material_or_an_identity_file`.
#[test]
fn the_argv_never_carries_key_material_or_a_private_identity_file() {
    let argv = sample_argv("true");

    assert!(
        !argv.iter().any(|part| part == "-i"),
        "an -i identity file would mean the private key was written to disk: {argv:?}"
    );
    let identity_files: Vec<&String> = argv
        .iter()
        .filter(|part| part.starts_with("IdentityFile="))
        .collect();
    assert_eq!(
        identity_files.len(),
        1,
        "exactly one identity file is expected: {argv:?}"
    );
    assert!(
        identity_files[0].ends_with("id.pub"),
        "the identity file must be the exported PUBLIC key, never a private key path: {argv:?}"
    );
}

/// The negative control that gives the argv assertions their teeth, and the
/// argv counterpart of `auth_tests`'
/// `dropping_the_identity_file_breaks_authentication_which_is_why_it_is_there`:
/// **drop `IdentityFile` from the argv a session actually runs, and a real
/// `sshd` must refuse it.**
///
/// Driven against the fixture rather than asserted about the vector, because
/// what is being proved is a *server's* decision: `IdentitiesOnly=yes`
/// restricts `ssh` to the configured identity files, so with none configured
/// the agent's key is never offered at all. The positive half is asserted
/// first, so a failure here can never be "the fixture was broken".
#[tokio::test]
async fn dropping_the_identity_file_from_the_argv_breaks_authentication() {
    let Some(fixture) = SshdFixture::start() else {
        return;
    };
    let key = SecretValue::from(fixture.client_key_pem().to_owned());
    let session = SshSession::open(target(&fixture), &key).expect("session");

    // Positive control: the production argv authenticates.
    session
        .exec("true")
        .await
        .expect("the unmodified argv must authenticate, or the control below proves nothing");

    let argv = session.argv_for("true");
    let mut crippled = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        if argv[index] == "-o"
            && argv
                .get(index + 1)
                .is_some_and(|o| o.starts_with("IdentityFile="))
        {
            index += 2;
            continue;
        }
        crippled.push(argv[index].clone());
        index += 1;
    }
    assert!(
        !crippled
            .iter()
            .any(|part| part.starts_with("IdentityFile=")),
        "the control must actually be missing IdentityFile: {crippled:?}"
    );
    assert!(
        crippled.iter().any(|part| part == "IdentitiesOnly=yes"),
        "the control must keep IdentitiesOnly, which is the point: {crippled:?}"
    );

    let status = std::process::Command::new("ssh")
        .args(&crippled)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ssh must be runnable; the fixture already proved it is");

    assert!(
        !status.success(),
        "IdentitiesOnly=yes without IdentityFile must NOT authenticate with the agent key -- \
         if this ever passes, the pairing is no longer load-bearing and `transport_options` \
         should be revisited"
    );
}
