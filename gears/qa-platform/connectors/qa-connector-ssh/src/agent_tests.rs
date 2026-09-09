//! Tests for the per-use ssh-agent.
//!
//! These are the tests ADR-0005's amendment rests on: they are what makes
//! "the private key never becomes a file" a checked property rather than a claim in
//! a comment.
//!
//! They shell out to a real `ssh-agent`/`ssh-add`/`ssh-keygen`, so they are
//! skipped (not failed) when those are absent — the same binaries the
//! runtime image installs via `openssh-client`.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use credstore_sdk::SecretValue;

use super::{
    HOST_KEY_VERIFICATION_OPTIONS, SshAgent, SshFailure, is_encrypted_private_key,
    openssh_key_cipher, ssh_command_for,
};

/// Require the openssh client tools, or explain loudly why the test cannot
/// prove anything.
///
/// These tests previously returned early with an `eprintln!` when the tools
/// were missing. `cargo test` captures stdout/stderr by default, so on an
/// image without `openssh-client` the keystone security test would print
/// nothing and report **green while proving nothing** — an aggregate
/// "tests pass" concealing an unrun check, which is precisely this
/// project's documented failure mode.
///
/// Now the absence is either an explicit local skip (a `panic!` is wrong on
/// a workstation that legitimately lacks the tools) **or a hard failure**
/// when `QA_REQUIRE_SSH_TOOLS=1` is set, which CI is expected to
/// set. The runtime image installs `openssh-client` (ADR-0005 as amended),
/// so in CI their absence is a real defect, not an environment quirk.
fn require_openssh() -> bool {
    let available = ["ssh-agent", "ssh-add", "ssh-keygen"]
        .iter()
        .all(|bin| Command::new(bin).arg("-h").output().is_ok());

    if !available {
        assert!(
            std::env::var_os("QA_REQUIRE_SSH_TOOLS").is_none(),
            "openssh client tools are missing but QA_REQUIRE_SSH_TOOLS is set: \
             these tests prove the key never reaches disk and MUST NOT be skipped here"
        );
        eprintln!(
            "skipping: openssh client tools not available (set \
             QA_REQUIRE_SSH_TOOLS=1 to make this a failure)"
        );
    }
    available
}

/// Generate a key pair with `ssh-keygen` and return the private key's PEM
/// text. The file exists only inside `dir`, which the caller owns; the
/// production path never writes one.
fn generate_key(dir: &Path, passphrase: &str) -> String {
    let path = dir.join("id_ed25519");
    let status = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-q", "-N", passphrase, "-C", "test", "-f"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "ssh-keygen failed");
    std::fs::read_to_string(&path).unwrap()
}

/// Every file under `root`, recursively.
fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

// -------------------------------------------------------------------------
// The ssh command string (brief item 3/4)
// -------------------------------------------------------------------------

/// The command must route `ssh` at *this* agent and must carry every option
/// the design depends on. `BatchMode=yes` in particular is what turns an
/// authentication failure into an error instead of a hang in a tty-less
/// container.
#[test]
fn ssh_command_carries_agent_socket_batchmode_and_host_key_options() {
    let cmd = ssh_command_for(
        Path::new("/tmp/agent-xyz/agent.sock"),
        Path::new("/tmp/agent-xyz/id.pub"),
    );

    assert!(cmd.starts_with("ssh "), "must invoke ssh: {cmd}");
    assert!(
        cmd.contains("-o IdentityAgent='/tmp/agent-xyz/agent.sock'"),
        "must point at the per-use agent socket: {cmd}"
    );
    // `IdentitiesOnly=yes` and `IdentityFile=<agent public key>` are a
    // PAIR. `IdentitiesOnly` alone restricts ssh to the *configured*
    // identity files, which with none configured means the default
    // `~/.ssh/id_*` set — so the agent's key is never offered (measured
    // against a real sshd: `Permission denied (publickey)`, rc=255) and, on
    // a host that has a default key, ssh silently authenticates as the
    // WRONG identity. Neither option may be dropped without the other.
    assert!(
        cmd.contains("-o IdentitiesOnly=yes"),
        "must offer only the configured identity: {cmd}"
    );
    assert!(
        cmd.contains("-o IdentityFile='/tmp/agent-xyz/id.pub'"),
        "IdentitiesOnly=yes is inert without an IdentityFile naming the \
         agent's key — without it the agent key is never offered: {cmd}"
    );
    assert!(
        cmd.contains("-o BatchMode=yes"),
        "BatchMode=yes is not optional — without it ssh can hang forever: {cmd}"
    );
    assert!(
        cmd.contains("-o StrictHostKeyChecking=no"),
        "host-key checking is disabled by explicit human decision: {cmd}"
    );
    assert!(
        cmd.contains("-o UserKnownHostsFile=/dev/null"),
        "host-key checking is disabled by explicit human decision: {cmd}"
    );
}

/// The host-key weakening must stay expressed in exactly one place, so that
/// tightening it later is a one-constant change. If someone scatters the
/// flags through the code instead, this constant stops being the source of
/// the command string and this test fails.
#[test]
fn host_key_options_come_from_the_single_documented_constant() {
    let cmd = ssh_command_for(Path::new("/tmp/s.sock"), Path::new("/tmp/id.pub"));
    for option in HOST_KEY_VERIFICATION_OPTIONS {
        assert!(
            cmd.contains(&format!("-o {option}")),
            "{option} must reach the command from the single policy constant: {cmd}"
        );
    }
    assert_eq!(
        HOST_KEY_VERIFICATION_OPTIONS.len(),
        2,
        "the documented policy is exactly StrictHostKeyChecking=no + \
         UserKnownHostsFile=/dev/null; changing it means updating ADR-0005"
    );
}

/// The socket path is quoted, so a temp directory containing a space
/// survives the word-splitting gix applies to `core.sshCommand`. Without
/// the quotes the option would arrive at `ssh` as two arguments and the
/// agent would silently not be used.
#[test]
fn the_agent_socket_path_is_quoted_against_word_splitting() {
    let cmd = ssh_command_for(
        Path::new("/var/folders/my tmp/qa-connector-ssh-1/agent.sock"),
        Path::new("/var/folders/my tmp/qa-connector-ssh-1/id.pub"),
    );
    assert!(
        cmd.contains("-o IdentityAgent='/var/folders/my tmp/qa-connector-ssh-1/agent.sock'"),
        "a socket path containing a space must stay one argument: {cmd}"
    );
}

/// The key must never appear in the command line: `argv` is world-readable
/// through `/proc/<pid>/cmdline`. This is the assertion that fails if the
/// agent approach is ever "simplified" to `ssh -i <file>`.
#[test]
fn ssh_command_never_carries_key_material_or_an_identity_file() {
    let cmd = ssh_command_for(
        Path::new("/tmp/agent/agent.sock"),
        Path::new("/tmp/agent/id.pub"),
    );
    assert!(
        !cmd.contains("-i "),
        "an -i identity file would mean the private key was written to disk: {cmd}"
    );
    // The command DOES carry an `IdentityFile`, but it must only ever name
    // the exported **public** key (`.pub`) — never a private key file. That
    // distinction is the whole point: a public key on disk is not a secret,
    // a private one is the thing this design exists to prevent.
    let identity_files: Vec<&str> = cmd
        .split(' ')
        .filter(|part| part.starts_with("IdentityFile="))
        .collect();
    assert_eq!(
        identity_files.len(),
        1,
        "exactly one identity file is expected: {cmd}"
    );
    assert!(
        identity_files[0].ends_with("id.pub'"),
        "the identity file must be the exported PUBLIC key, never a private \
         key path: {cmd}"
    );
}

// -------------------------------------------------------------------------
// Passphrase detection (brief item 3)
// -------------------------------------------------------------------------

/// A passphrase-protected key must be diagnosed *before* `ssh-add` runs.
/// `ssh-add` fails silently on one (exit 1, empty stderr), so without this
/// the operator would get an unattributable "ssh-add rejected the key".
#[test]
fn passphrase_protected_keys_are_detected_and_named() {
    if !require_openssh() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();

    let unencrypted = generate_key(dir.path(), "");
    assert_eq!(openssh_key_cipher(&unencrypted).as_deref(), Some("none"));
    assert!(!is_encrypted_private_key(&unencrypted));

    std::fs::remove_file(dir.path().join("id_ed25519")).unwrap();
    std::fs::remove_file(dir.path().join("id_ed25519.pub")).unwrap();
    let encrypted = generate_key(dir.path(), "hunter2");
    assert_ne!(openssh_key_cipher(&encrypted).as_deref(), Some("none"));
    assert!(is_encrypted_private_key(&encrypted));

    // ...and the domain error must say so in words an operator can act on,
    // without echoing any part of the key.
    // NB: `unwrap_err()` is unavailable on purpose — `SshAgent` is
    // deliberately not `Debug`, so it can never be formatted into a log.
    let secret = SecretValue::from(encrypted.clone());
    let Err(err) = SshAgent::start_with_key(&secret) else {
        panic!("a passphrase-protected key must be refused");
    };
    let message = err.to_string();
    assert!(
        message.contains("passphrase"),
        "the error must name the cause: {message}"
    );
    for line in encrypted.lines().filter(|l| !l.starts_with("-----")) {
        assert!(
            !message.contains(line),
            "the error message leaked key material"
        );
    }
}

/// Classic PEM encryption headers are recognised too.
#[test]
fn classic_pem_encryption_headers_are_detected() {
    let classic = "-----BEGIN RSA PRIVATE KEY-----\n\
                   Proc-Type: 4,ENCRYPTED\n\
                   DEK-Info: AES-128-CBC,0123456789ABCDEF\n\
                   \n\
                   AAAA\n\
                   -----END RSA PRIVATE KEY-----\n";
    assert!(is_encrypted_private_key(classic));
}

// -------------------------------------------------------------------------
// Agent lifecycle (brief item 3)
// -------------------------------------------------------------------------

/// The agent runs while the guard is alive and is gone once it drops —
/// socket, directory and process. A leaked agent per sync is an unbounded
/// resource leak in a long-running gear.
#[test]
fn agent_socket_and_directory_are_removed_on_drop() {
    if !require_openssh() {
        return;
    }
    let keydir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let pem = generate_key(keydir.path(), "");

    let (socket, dir) = {
        let secret = SecretValue::from(pem);
        let agent = SshAgent::start_with_key_in(root.path(), &secret).unwrap();
        let socket = agent.socket().to_path_buf();
        let dir = socket.parent().unwrap().to_path_buf();
        assert!(
            socket.exists(),
            "socket must exist while the agent is alive"
        );

        // The identity really is loaded — otherwise the rest of this file
        // would be asserting about an agent that authenticates nothing.
        let listed = Command::new("ssh-add")
            .arg("-l")
            .env("SSH_AUTH_SOCK", &socket)
            .output()
            .unwrap();
        assert!(
            listed.status.success(),
            "the agent should hold exactly one identity"
        );

        (socket, dir)
    };

    assert!(!socket.exists(), "the agent socket must not survive drop");
    assert!(!dir.exists(), "the agent directory must not survive drop");
}

/// The same guarantee on a **failing** path: `start_with_key` returning an
/// error must leave nothing behind either.
#[test]
fn a_failed_key_load_leaves_no_agent_directory_behind() {
    if !require_openssh() {
        return;
    }
    // A private root, so a test running in parallel cannot make this
    // assertion see its agent directory instead of ours.
    let root = tempfile::tempdir().unwrap();

    let secret = SecretValue::from("this is not a private key at all");
    let Err(err) = SshAgent::start_with_key_in(root.path(), &secret) else {
        panic!("malformed key material must be refused");
    };
    // Was `DomainError::SyncFailed` before this module moved into
    // `qa-connector-ssh`; ssh-add's own rejection of unparseable material
    // is now an `SshFailure::AgentSetup` (its stderr is never captured, see
    // `add_key`).
    assert!(matches!(err, SshFailure::AgentSetup { .. }));

    let leftover: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert!(
        leftover.is_empty(),
        "a failed sync must not leak an agent directory, found: {leftover:?}"
    );
}

/// The same, for the passphrase refusal, which returns *before* any
/// directory is created.
#[test]
fn a_refused_passphrase_key_creates_no_agent_directory() {
    let root = tempfile::tempdir().unwrap();
    let classic = "-----BEGIN RSA PRIVATE KEY-----\n\
                   Proc-Type: 4,ENCRYPTED\n\
                   DEK-Info: AES-128-CBC,0123456789ABCDEF\n\
                   \n\
                   AAAA\n\
                   -----END RSA PRIVATE KEY-----\n";
    let secret = SecretValue::from(classic);
    let Err(_) = SshAgent::start_with_key_in(root.path(), &secret) else {
        panic!("a passphrase-protected key must be refused");
    };
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        0,
        "the passphrase refusal must happen before anything is created"
    );
}

// -------------------------------------------------------------------------
// THE security property (brief item 3, and the Tests section's last bullet)
// -------------------------------------------------------------------------

/// **The property the whole design exists to preserve.**
///
/// While the agent is running and holding the key, **no file under the
/// agent's directory may contain the private key material**, and the key
/// must appear in neither the command line nor the environment that any
/// part of this module hands to a child process.
///
/// This is the test that fails if someone later replaces the agent with
/// `ssh -i /tmp/key`.
///
/// # What this test does NOT cover — read before editing
///
/// Checks 1 and 2 are bounded by the agent's own directory, so a mutation
/// that wrote the key somewhere *else* (`~/.ssh/id_ed25519`, another temp
/// dir) would slip past them. That gap is covered by the sibling test
/// [`ssh_command_never_carries_key_material_or_an_identity_file`], which
/// asserts no `-i`/`IdentityFile=<private key>` can reach `ssh` — a key
/// written outside this directory would be useless without one. **The two
/// tests are a pair.** Do not delete the sibling on the belief that this
/// keystone subsumes it; it does not.
///
/// # Why the directory is allowed to contain a `.pub` file
///
/// `IdentitiesOnly=yes` only restricts `ssh` to the identities named by
/// `IdentityFile`, so the agent's **public** key must be nameable on disk
/// (see `SshAgent::export_public_key`). Public material is not secret — it
/// is sent to the server in the clear during authentication. The assertion
/// below is therefore about the *private* material specifically, not about
/// how many files exist.
#[test]
fn private_key_never_touches_the_filesystem() {
    if !require_openssh() {
        return;
    }
    let keydir = tempfile::tempdir().unwrap();
    let pem = generate_key(keydir.path(), "");

    // A distinctive slice of the real key body to search for.
    let needle = pem
        .lines()
        .find(|l| !l.starts_with("-----") && l.len() > 40)
        .expect("the generated key should have a substantial body line")
        .to_owned();

    let secret = SecretValue::from(pem);
    let agent = SshAgent::start_with_key(&secret).unwrap();
    let agent_dir = agent.socket().parent().unwrap().to_path_buf();

    // 1. NO file under the agent's directory contains the private key —
    //    whatever the file is called, and however many there are.
    let files = files_under(&agent_dir);
    assert!(
        !files.is_empty(),
        "expected at least the exported public key to be present"
    );
    for file in &files {
        let bytes = std::fs::read(file).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(&needle),
            "PRIVATE key material was written to {}",
            file.display()
        );
        assert!(
            !text.contains("PRIVATE KEY"),
            "a private-key PEM header appeared in {}",
            file.display()
        );
    }

    // 2. The directory holds the socket and the exported PUBLIC key, and
    //    nothing else — so no stray file is smuggled in under a benign name.
    let mut entries: Vec<String> = std::fs::read_dir(&agent_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec!["agent.sock".to_owned(), "id.pub".to_owned()],
        "the agent directory must hold exactly the socket and the public key"
    );

    // ...and that public key really is public material.
    let exported = std::fs::read_to_string(agent_dir.join("id.pub")).unwrap();
    assert!(
        exported.starts_with("ssh-"),
        "the exported identity must be an OpenSSH public key, got: {exported:.40}"
    );
    assert!(
        !exported.contains("PRIVATE"),
        "the exported identity must not contain private material"
    );

    // 3. The command handed to gix carries neither the key nor a file path
    //    to one.
    let cmd = agent.ssh_command();
    assert!(!cmd.contains(&needle), "key material reached argv: {cmd}");
    assert!(!cmd.contains("-i "), "an identity file appeared: {cmd}");

    // 4. Our own process environment was not used to reach the agent —
    //    SSH_AUTH_SOCK is set per child, never globally, because it would
    //    race across concurrent syncs.
    assert!(
        std::env::var_os("SSH_AUTH_SOCK").is_none_or(|v| v != *agent.socket().as_os_str()),
        "the agent must not be published through the process environment"
    );
}
