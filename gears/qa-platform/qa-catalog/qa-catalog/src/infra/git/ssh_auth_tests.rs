//! **End-to-end SSH authentication against a real `sshd`.**
//!
//! This suite exists because of a defect that every other test in this
//! module passed: the shipped option string carried `IdentitiesOnly=yes`
//! with no `IdentityFile`, which makes `ssh` ignore the agent's key
//! entirely. Argv-capture tests and in-image agent exercises both went
//! green, because **neither involves a server deciding whether to accept a
//! signature**. Only a real handshake can tell you that.
//!
//! The fixture is a throwaway `sshd` on an ephemeral port with a temporary
//! host key and a temporary `authorized_keys`, run as the current user. No
//! credentials, no network, no container: it uses the host's own `sshd`
//! binary and shuts it down on drop.
//!
//! These tests run automatically wherever `sshd` and the openssh client
//! tools exist. Where they do not, they skip — unless
//! `QA_CATALOG_REQUIRE_SSH_TOOLS=1` is set, in which case they fail, so a
//! CI image cannot report them green while proving nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use super::SshAgent;

/// Candidate paths for the `sshd` binary; it is not usually on a user `PATH`.
const SSHD_CANDIDATES: [&str; 3] = ["/usr/sbin/sshd", "/usr/local/sbin/sshd", "/sbin/sshd"];

fn find_sshd() -> Option<PathBuf> {
    SSHD_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Resolve the fixture's prerequisites, or explain why the test cannot
/// prove anything. See the module header for the CI-enforcement contract.
fn require_sshd() -> Option<PathBuf> {
    let sshd = find_sshd();
    if sshd.is_none() {
        assert!(
            std::env::var_os("QA_CATALOG_REQUIRE_SSH_TOOLS").is_none(),
            "sshd was not found but QA_CATALOG_REQUIRE_SSH_TOOLS is set: this suite is \
             what proves SSH authentication actually works and MUST NOT be skipped here"
        );
        eprintln!(
            "skipping: no sshd binary found (set QA_CATALOG_REQUIRE_SSH_TOOLS=1 to make \
             this a failure)"
        );
    }
    sshd
}

/// A running throwaway `sshd`, killed and reaped on drop.
struct SshdFixture {
    child: Child,
    port: u16,
    /// Private key PEM of the identity in `authorized_keys`.
    user_key_pem: String,
    _dir: tempfile::TempDir,
}

impl Drop for SshdFixture {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

impl SshdFixture {
    fn start(sshd: &Path) -> Option<Self> {
        let dir = tempfile::Builder::new()
            .prefix("qa-sshd-")
            .tempdir_in("/tmp")
            .unwrap();
        let path = |name: &str| dir.path().join(name);

        keygen(&path("hostkey"));
        keygen(&path("userkey"));
        std::fs::copy(path("userkey.pub"), path("authorized_keys")).unwrap();
        let user_key_pem = std::fs::read_to_string(path("userkey")).unwrap();

        let port = free_port();
        let config = format!(
            "Port {port}\n\
             ListenAddress 127.0.0.1\n\
             HostKey {hostkey}\n\
             AuthorizedKeysFile {authorized_keys}\n\
             PidFile {pid}\n\
             StrictModes no\n\
             UsePAM no\n\
             PasswordAuthentication no\n\
             KbdInteractiveAuthentication no\n\
             PubkeyAuthentication yes\n\
             LogLevel VERBOSE\n",
            hostkey = path("hostkey").display(),
            authorized_keys = path("authorized_keys").display(),
            pid = path("sshd.pid").display(),
        );
        std::fs::write(path("sshd_config"), config).unwrap();

        // `-D` foreground, so this `Child` is the server and the `Drop`
        // guard above actually reaps it.
        let child = Command::new(sshd)
            .arg("-D")
            .arg("-f")
            .arg(path("sshd_config"))
            .arg("-E")
            .arg(path("sshd.log"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let fixture = Self {
            child,
            port,
            user_key_pem,
            _dir: dir,
        };
        fixture.await_port().then_some(fixture)
    }

    fn await_port(&self) -> bool {
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    /// Run `ssh_command` (a `core.sshCommand` string, exactly as gix would
    /// receive it) against this server. Returns `(exit_code, stderr)`.
    fn authenticate_with(&self, ssh_command: &str) -> (i32, String) {
        let user = std::env::var("USER").unwrap_or_else(|_| "root".to_owned());
        // Through a shell, because that is how the word-split command
        // string is consumed in production.
        let script = format!(
            "{ssh_command} -v -p {port} {user}@127.0.0.1 true",
            port = self.port
        );
        let out = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

fn keygen(path: &Path) {
    let status = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-q", "-N", "", "-f"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "ssh-keygen failed for {}", path.display());
}

/// An unused local port, released before `sshd` binds it.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// **The test that would have caught C1.**
///
/// Drives the *real* `core.sshCommand` this module produces against a real
/// `sshd`, and requires the handshake to succeed using the agent's identity.
#[test]
fn the_production_ssh_command_authenticates_using_the_agent_key() {
    let Some(sshd) = require_sshd() else { return };
    let Some(server) = SshdFixture::start(&sshd) else {
        panic!("failed to start the sshd fixture");
    };

    let agent = SshAgent::start_with_key(&server.user_key_pem).unwrap();
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
    let Some(sshd) = require_sshd() else { return };
    let Some(server) = SshdFixture::start(&sshd) else {
        panic!("failed to start the sshd fixture");
    };

    let agent = SshAgent::start_with_key(&server.user_key_pem).unwrap();

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
    let Some(sshd) = require_sshd() else { return };
    let Some(server) = SshdFixture::start(&sshd) else {
        panic!("failed to start the sshd fixture");
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
    let Some(sshd) = require_sshd() else { return };
    let Some(server) = SshdFixture::start(&sshd) else {
        panic!("failed to start the sshd fixture");
    };

    let agent = SshAgent::start_with_key(&server.user_key_pem).unwrap();
    let agent_dir = agent.socket().parent().unwrap().to_path_buf();
    let exported = std::fs::read_to_string(agent_dir.join("id.pub")).unwrap();

    assert!(exported.starts_with("ssh-"), "not a public key: {exported}");
    assert!(
        !exported.contains("PRIVATE"),
        "private material was exported: {exported}"
    );
    // The private key's body must appear in no file in that directory.
    let needle = server
        .user_key_pem
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
