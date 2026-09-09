//! A throwaway `sshd`, published behind the `test-support` feature.
//!
//! # Why this is public API rather than `cfg(test)` scaffolding
//!
//! This fixture is what [`crate::agent`]'s own `auth_tests` drives to prove
//! `SshAgent` actually authenticates against a real server rather than just
//! producing a command string that looks right (see that module's header for
//! the defect this suite exists to catch). A product plugin building
//! `SshSession` on top of `SshAgent` needs the exact same proof, and it
//! cannot build its own copy without re-deriving everything here: a
//! throwaway host key, a temporary `authorized_keys`, and the port/log
//! plumbing that makes the server observable. So the fixture is exported
//! instead of duplicated, the same choice `qa-connector-k8s::test_support`
//! made for its own stub API server.
//!
//! # Two keys, on purpose
//!
//! [`SshdFixture::start`] generates a keypair it lists in `authorized_keys`
//! (returned by [`SshdFixture::client_key_pem`]), and [`SshdFixture::unauthorised_key_pem`]
//! generates (once, lazily, on first call) a second, valid, unencrypted key
//! that is listed in no fixture's `authorized_keys`. A caller proving an
//! auth-rejected path needs a key this server's own `sshd` will genuinely
//! refuse over the wire -- not a malformed blob `ssh-add` would reject
//! before a connection is ever attempted, which would prove nothing about
//! the rejection path a real host produces.
//!
//! # These tests run wherever `sshd` exists
//!
//! Where it does not, [`SshdFixture::start`] returns `None` and a caller is
//! expected to skip -- unless `QA_REQUIRE_SSH_TOOLS=1` is set, in which case
//! it fails loudly instead, so a CI image cannot report green while proving
//! nothing (the same contract `agent_tests`' `require_openssh` enforces for
//! the agent's own tool dependency). A `sshd` that *is* found but fails to
//! spawn or never reaches a listening state is always a panic, never a
//! `None` -- see [`SshdFixture::start`] for why that distinction matters.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

/// Candidate paths for the `sshd` binary; it is not usually on a user `PATH`.
const SSHD_CANDIDATES: [&str; 3] = ["/usr/sbin/sshd", "/usr/local/sbin/sshd", "/sbin/sshd"];

/// Set to make a missing `sshd` a hard failure instead of a skip. CI is
/// expected to set this so the fixture's absence is a real defect, not an
/// environment quirk.
const REQUIRE_SSH_TOOLS_VAR: &str = "QA_REQUIRE_SSH_TOOLS";

fn find_sshd() -> Option<PathBuf> {
    SSHD_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// A running throwaway `sshd` on an ephemeral port, killed and reaped on
/// drop. Run as the current user, with a temporary host key and a temporary
/// `authorized_keys` listing exactly one generated key.
pub struct SshdFixture {
    child: Child,
    port: u16,
    user: String,
    /// Private key PEM of the identity in `authorized_keys`.
    client_key_pem: String,
    _dir: tempfile::TempDir,
}

impl Drop for SshdFixture {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

impl SshdFixture {
    /// Start a throwaway `sshd`, or return `None` when **no `sshd` binary**
    /// was found on the host.
    ///
    /// `None` means exactly that one thing and nothing else: every other
    /// failure mode (temp dir, `ssh-keygen`, the fixture's own `sshd` failing
    /// to spawn or to reach a listening state) is a panic, never a `None`.
    /// Folding those into the same `None` as "no binary" would let a broken
    /// `sshd` on a CI image -- the likeliest real failure, since the image is
    /// expected to ship the binary -- report this whole suite green while
    /// proving nothing, which is the one thing this fixture exists to
    /// prevent.
    ///
    /// The missing-binary `None` is silent unless `QA_REQUIRE_SSH_TOOLS=1` is
    /// set, in which case it is a panic instead, per this module's header.
    ///
    /// # Panics
    ///
    /// If `QA_REQUIRE_SSH_TOOLS=1` is set and no `sshd` binary was found; if
    /// any step of assembling the fixture (temp dir, `ssh-keygen`,
    /// `authorized_keys`) fails; or if a *found* `sshd` fails to spawn or
    /// never reaches a listening state. All of these are defects in this
    /// module or the host it runs on, never something a caller can act on.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "a fixture that fails to assemble itself has nothing useful to report but \
                  the panic -- there is no caller-actionable error to return"
    )]
    pub fn start() -> Option<Self> {
        let Some(sshd) = find_sshd() else {
            assert!(
                std::env::var_os(REQUIRE_SSH_TOOLS_VAR).is_none(),
                "sshd was not found but {REQUIRE_SSH_TOOLS_VAR} is set: this fixture is what \
                 proves SSH authentication actually works and MUST NOT be skipped here"
            );
            eprintln!(
                "skipping: no sshd binary found (set {REQUIRE_SSH_TOOLS_VAR}=1 to make this \
                 a failure)"
            );
            return None;
        };

        let dir = tempfile::Builder::new()
            .prefix("qa-sshd-")
            .tempdir_in(std::env::temp_dir())
            .expect("creating a temp dir for the sshd fixture");
        let path = |name: &str| dir.path().join(name);

        keygen(&path("hostkey"));
        keygen(&path("userkey"));
        std::fs::copy(path("userkey.pub"), path("authorized_keys"))
            .expect("copying the generated public key into authorized_keys");
        let client_key_pem = std::fs::read_to_string(path("userkey"))
            .expect("reading back the generated private key");
        let user = std::env::var("USER").unwrap_or_else(|_| "root".to_owned());

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
        std::fs::write(path("sshd_config"), config).expect("writing the fixture's sshd_config");

        // `-D` foreground, so this `Child` is the server and the `Drop`
        // guard above actually reaps it.
        //
        // `expect`, not `.ok()?`: the binary was *found* above, so a spawn
        // failure here is a defect (permissions, a broken sshd_config) that
        // `QA_REQUIRE_SSH_TOOLS=1` must catch, not a "no sshd on this host"
        // skip. Folding it into `None` was the C1-class bug this fixture
        // exists to prevent one layer up: a broken sshd reporting green.
        let child = Command::new(&sshd)
            .arg("-D")
            .arg("-f")
            .arg(path("sshd_config"))
            .arg("-E")
            .arg(path("sshd.log"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect(
                "spawning the sshd fixture; the binary was found, so a failure here is a defect",
            );

        let fixture = Self {
            child,
            port,
            user,
            client_key_pem,
            _dir: dir,
        };
        // Same reasoning as the `spawn` above: `sshd` was found and spawned,
        // so failing to reach a listening state is a real defect, not an
        // environment quirk to skip past.
        assert!(
            fixture.await_port(),
            "the sshd fixture never reached a listening state"
        );
        Some(fixture)
    }

    /// The ephemeral port this server listens on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The user this server accepts connections for. It runs as the current
    /// (unprivileged) user, which is the only user it can authenticate a
    /// session for.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The private key PEM of the identity this server's `authorized_keys`
    /// lists.
    #[must_use]
    pub fn client_key_pem(&self) -> &str {
        &self.client_key_pem
    }

    /// A second, valid, unencrypted key that is deliberately **not**
    /// authorised against any fixture this module starts. Loading it into an
    /// agent and authenticating with it must fail at the server, not before
    /// -- which is what makes it useful for proving an auth-rejected path
    /// rather than a malformed-key path.
    ///
    /// Generated once, on first call, with the same `ssh-keygen` this
    /// module's own `authorized_keys` entry comes from -- not checked in as
    /// a literal, so no secret scanner over this repository ever has a real
    /// private key to flag, and no later reader has to re-derive that this
    /// one is inert.
    ///
    /// # Panics
    ///
    /// If `ssh-keygen` fails or its output cannot be read back, the same
    /// defects [`SshdFixture::start`] panics on rather than reporting.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "a host that cannot spawn ssh-keygen cannot run this fixture's tests at all"
    )]
    pub fn unauthorised_key_pem() -> &'static str {
        static KEY: OnceLock<String> = OnceLock::new();
        KEY.get_or_init(|| {
            let dir = tempfile::Builder::new()
                .prefix("qa-sshd-unauth-")
                .tempdir_in(std::env::temp_dir())
                .expect("creating a temp dir for the unauthorised test key");
            let path = dir.path().join("unauthorised");
            keygen(&path);
            std::fs::read_to_string(&path)
                .expect("reading back the generated unauthorised test key")
        })
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

    /// Run `ssh_command` (a `core.sshCommand` string, exactly as gix or a
    /// product plugin would receive it) against this server. Returns
    /// `(exit_code, stderr)`.
    ///
    /// `pub(crate)` rather than exported: only this crate's own `auth_tests`
    /// drives it today. A build with `test-support` on but this crate's own
    /// tests off (any consumer's) never reaches the `#[cfg(test)]` code that
    /// calls it, hence the `allow` below.
    #[allow(
        dead_code,
        reason = "only called from this crate's own #[cfg(test)] auth_tests"
    )]
    #[allow(
        clippy::expect_used,
        reason = "a host that cannot spawn `sh` cannot run this fixture's tests at all"
    )]
    pub(crate) fn authenticate_with(&self, ssh_command: &str) -> (i32, String) {
        // Through a shell, because that is how the word-split command
        // string is consumed in production.
        let script = format!(
            "{ssh_command} -v -p {port} {user}@127.0.0.1 true",
            port = self.port,
            user = self.user,
        );
        let out = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .stdin(Stdio::null())
            .output()
            .expect("spawning `sh` must succeed on any host that can run these tests");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

#[allow(
    clippy::expect_used,
    reason = "a host that cannot spawn ssh-keygen cannot run this fixture's tests at all"
)]
fn keygen(path: &Path) {
    let status = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-q", "-N", "", "-f"])
        .arg(path)
        .status()
        .expect("spawning ssh-keygen for the sshd fixture");
    assert!(status.success(), "ssh-keygen failed for {}", path.display());
}

/// An unused local port, released before `sshd` binds it.
#[allow(
    clippy::expect_used,
    reason = "a host that cannot bind loopback cannot run this fixture's tests at all"
)]
fn free_port() -> u16 {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("binding an ephemeral loopback port");
    listener
        .local_addr()
        .expect("reading back the bound port")
        .port()
}

// ── Log capture, for callers asserting what their own `warn!` lines carry ──

/// Everything `tracing` wrote, verbatim, for a test that has to assert what a
/// log line does — and does not — contain.
///
/// `tracing-test` (the usual choice in this workspace) is deliberately not
/// used for the leak assertions this exists for: it keeps only lines
/// containing the test's span name, so a multi-line value — exactly the shape
/// a leaked private key has — would survive capture only as its first line,
/// and a leak on any later line would read as "no leak". This buffer has no
/// such hole.
///
/// Public, and living beside the `sshd` fixture rather than in each plugin,
/// for the reason `qa_connector_k8s::test_support::RawBuffer` gives for its
/// own copy: a product plugin asserting that one of *its* log lines carries a
/// classified reason rather than a formatted error needs exactly this, and a
/// second copy of the reasoning in another crate is a second place for it to
/// rot. The two are siblings, one per transport.
#[derive(Clone, Debug, Default)]
pub struct RawBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl RawBuffer {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything written so far, as text. Invalid UTF-8 is replaced rather
    /// than rejected: this is a leak assertion's input, and bytes that did
    /// not decode still have to be looked at.
    #[must_use]
    pub fn captured(&self) -> String {
        let bytes = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl std::io::Write for RawBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RawBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
