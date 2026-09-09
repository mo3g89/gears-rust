//! **End-to-end SSH authentication against a real `sshd`.**
//!
//! This suite exists because of a defect that every other test in this
//! module passed: the shipped option string carried `IdentitiesOnly=yes`
//! with no `IdentityFile`, which makes `ssh` ignore the agent's key
//! entirely. Argv-capture tests and in-image agent exercises both went
//! green, because **neither involves a server deciding whether to accept a
//! signature**. Only a real handshake can tell you that.
//!
//! The fixture ([`crate::test_support::SshdFixture`]) is a throwaway `sshd`
//! on an ephemeral port with a temporary host key and a temporary
//! `authorized_keys`, run as the current user. No credentials, no network,
//! no container: it uses the host's own `sshd` binary and shuts it down on
//! drop.
//!
//! These tests run automatically wherever `sshd` and the openssh client
//! tools exist. Where they do not, they skip — unless
//! `QA_REQUIRE_SSH_TOOLS=1` is set, in which case they fail, so a
//! CI image cannot report them green while proving nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;

use credstore_sdk::SecretValue;

use super::SshAgent;
use crate::test_support::SshdFixture;

/// **The test that would have caught C1.**
///
/// Drives the *real* `core.sshCommand` this module produces against a real
/// `sshd`, and requires the handshake to succeed using the agent's identity.
#[test]
fn the_production_ssh_command_authenticates_using_the_agent_key() {
    let Some(server) = SshdFixture::start() else {
        return;
    };

    let secret = SecretValue::from(server.client_key_pem());
    let agent = SshAgent::start_with_key(&secret).unwrap();
    let (code, log) = server.authenticate_with(&agent.ssh_command());

    assert_eq!(
        code, 0,
        "the production ssh command must authenticate with the agent key.\n\
         --- ssh -v output ---\n{log}"
    );
    assert!(
        log.contains("Authenticated to 127.0.0.1"),
        "expected a completed public-key authentication, got:\n{log}"
    );
    // The signature must have come from the AGENT, not from a local private
    // key file: `ssh -v` labels an agent-backed identity `explicit agent`.
    assert!(
        log.contains("explicit agent"),
        "the identity must be served by the agent, not a key file:\n{log}"
    );
}

/// The negative control that gives the test above its teeth: **drop
/// `IdentityFile` and authentication must break.**
///
/// This is C1 exactly. `IdentitiesOnly=yes` restricts `ssh` to the
/// *configured* identity files; with none configured that means the default
/// `~/.ssh/id_*` set, so the agent's key is never offered. If someone
/// removes `IdentityFile` from `ssh_command_for` again, the test above goes
/// red — and this one documents precisely why.
#[test]
fn dropping_the_identity_file_breaks_authentication_which_is_why_it_is_there() {
    let Some(server) = SshdFixture::start() else {
        return;
    };

    let secret = SecretValue::from(server.client_key_pem());
    let agent = SshAgent::start_with_key(&secret).unwrap();

    // The production command with the IdentityFile option stripped out.
    let crippled: String = agent
        .ssh_command()
        .split(" -o ")
        .filter(|part| !part.starts_with("IdentityFile="))
        .collect::<Vec<_>>()
        .join(" -o ");
    assert!(
        !crippled.contains("IdentityFile"),
        "the control must actually be missing IdentityFile: {crippled}"
    );
    assert!(
        crippled.contains("IdentitiesOnly=yes"),
        "the control must keep IdentitiesOnly, which is the point: {crippled}"
    );

    let (code, log) = server.authenticate_with(&crippled);
    assert_ne!(
        code, 0,
        "IdentitiesOnly=yes without IdentityFile must NOT authenticate with the \
         agent key — if this ever passes, the pairing is no longer load-bearing \
         and `ssh_command_for` should be revisited.\n--- ssh -v output ---\n{log}"
    );
}

/// A sanity check on the fixture itself: the key really is the gate.
/// Without the agent, the same server refuses the same user.
#[test]
fn the_fixture_actually_requires_the_key() {
    let Some(server) = SshdFixture::start() else {
        return;
    };

    // No agent, no identities at all.
    let (code, log) = server.authenticate_with(
        "ssh -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes \
         -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null",
    );
    assert_ne!(
        code, 0,
        "a server that accepts an unauthenticated connection would make every \
         other assertion in this file meaningless:\n{log}"
    );
}

/// Writing a scratch file next to the socket must not be how the key gets
/// used: with the agent alive, the exported identity file is public-only,
/// and `ssh` still reports the signature as coming from the agent. This is
/// the security property re-checked at the protocol level rather than by
/// scanning the filesystem.
#[test]
fn the_exported_identity_file_is_public_and_the_agent_signs() {
    let Some(server) = SshdFixture::start() else {
        return;
    };

    let secret = SecretValue::from(server.client_key_pem());
    let agent = SshAgent::start_with_key(&secret).unwrap();
    let agent_dir = agent.socket().parent().unwrap().to_path_buf();
    let exported = std::fs::read_to_string(agent_dir.join("id.pub")).unwrap();

    assert!(exported.starts_with("ssh-"), "not a public key: {exported}");
    assert!(
        !exported.contains("PRIVATE"),
        "private material was exported: {exported}"
    );
    // The private key's body must appear in no file in that directory.
    let needle = server
        .client_key_pem()
        .lines()
        .find(|l| !l.starts_with("-----") && l.len() > 40)
        .unwrap();
    for entry in std::fs::read_dir(&agent_dir).unwrap().flatten() {
        let text =
            String::from_utf8_lossy(&std::fs::read(entry.path()).unwrap_or_default()).into_owned();
        assert!(
            !text.contains(needle),
            "private key material found in {}",
            entry.path().display()
        );
    }

    let (code, log) = server.authenticate_with(&agent.ssh_command());
    assert_eq!(code, 0, "authentication should still succeed:\n{log}");
    assert!(
        log.contains("explicit agent"),
        "the agent, not the exported file, must produce the signature:\n{log}"
    );
    // Keep the writer used, so the import is not flagged in a future edit.
    let mut sink = std::io::sink();
    write!(sink, "{code}").ok();
}
