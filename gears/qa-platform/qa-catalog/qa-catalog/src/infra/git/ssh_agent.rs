//! Short-lived per-sync `ssh-agent`: how an SSH remote is authenticated
//! **without the private key ever becoming a file**.
//!
//! ## Why this module exists
//!
//! ADR-0005 originally deferred SSH support with this reasoning: gix's ssh
//! transport shells out to a system `ssh`, which offers no in-process key
//! injection, so supporting SSH "would mean writing key material to disk".
//! The amended ADR (2026-08-27) records that objection as **resolved rather
//! than accepted**, and this module is the resolution:
//!
//! 1. spawn an `ssh-agent` in foreground mode bound to a socket inside a
//!    private, `0700`, per-sync temporary directory;
//! 2. hand the PEM to `ssh-add -` **on stdin**;
//! 3. point `ssh` at that agent with `-o IdentityAgent=<socket>`.
//!
//! The key therefore travels credstore → memory → a pipe → the agent's
//! memory. It is **never** written to a file, never placed in `argv` (which
//! is world-readable via `/proc/<pid>/cmdline`), never placed in an
//! environment variable (`/proc/<pid>/environ`), and never logged or
//! `Debug`-formatted. `ssh_agent_tests` pins each of those properties; in
//! particular `private_key_never_touches_the_filesystem` is what would fail
//! if someone later "simplified" this to `ssh -i /tmp/key`.
//!
//! ## Why the agent is per-sync and addressed by config, not environment
//!
//! The socket is passed to `ssh` through `core.sshCommand` **on the gix
//! repository object**, not through a process-wide `SSH_AUTH_SOCK`. gix
//! resolves the ssh program per repository
//! (`gix-0.86.0/src/repository/config/mod.rs:91-97` reads `core.sshCommand`
//! into `ssh::connect::Options::command`), so two syncs running concurrently
//! with different keys each get their own agent. An environment variable
//! would be process-global and would race: whichever sync set it last would
//! decide which key *both* syncs authenticated with.
//!
//! ## Lifetime
//!
//! [`SshAgent`] reaps its child in `Drop`, so every exit path — success,
//! error, and unwind — kills the agent and removes its directory. A
//! happy-path `kill()` would leak one agent process per failed sync, which
//! in a long-running gear is an unbounded resource leak.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::domain::error::DomainError;

/// **DELIBERATE SECURITY WEAKENING — host key verification is disabled.**
///
/// Authorised explicitly by the human partner on 2026-08-27 ("i want host
/// key do not checked and all repos can be cloned without errors") and
/// recorded in ADR-0005 as amended. This is the **one** place the policy is
/// expressed; nothing else in the codebase passes host-key options to `ssh`,
/// so tightening it later is a change to this constant alone.
///
/// ## What it means
///
/// `StrictHostKeyChecking=no` makes `ssh` accept — and
/// `UserKnownHostsFile=/dev/null` makes it immediately forget — whatever
/// host key the server presents. Nothing pins the remote's identity, so a
/// clone from a hostile or compromised network path (DNS hijack, BGP
/// hijack, on-path attacker) can be served by an impostor host that
/// qa-catalog will authenticate to and fetch content from. The private key
/// itself is not disclosed by this — the agent only ever performs a
/// signature, and `IdentitiesOnly=yes` restricts which identity is offered
/// — but **the repository content the gear ingests can be attacker-chosen**,
/// and that content drives plan discovery and test execution.
///
/// ## What would have to change to tighten it
///
/// Pin the expected key(s): drop these two options, provision a known-hosts
/// file (from gear config or a credstore-held blob), and pass
/// `-o UserKnownHostsFile=<that file> -o StrictHostKeyChecking=yes`. The
/// per-sync temp dir this module already creates is the natural place to
/// materialize it. Doing so requires an operator-facing way to supply host
/// keys, which is why it is not done here.
const HOST_KEY_VERIFICATION_OPTIONS: [&str; 2] =
    ["StrictHostKeyChecking=no", "UserKnownHostsFile=/dev/null"];

/// How long to wait for `ssh-agent` to create its socket before giving up.
const AGENT_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for `ssh-add` to consume the key before giving up.
const SSH_ADD_TIMEOUT: Duration = Duration::from_secs(30);

/// A running `ssh-agent` holding exactly one identity, reaped on drop.
///
/// Deliberately **not** `Debug`/`Clone`: there is nothing here worth
/// formatting, and the type should never appear in a log line.
pub(super) struct SshAgent {
    /// Owns the socket directory; removed when this struct drops.
    _dir: tempfile::TempDir,
    socket: PathBuf,
    /// The agent identity's **public** key, written to disk so it can be
    /// named by `-o IdentityFile`. See [`SshAgent::export_public_key`] for
    /// why a file is required and why only public material is in it.
    public_key: PathBuf,
    child: Child,
}

impl Drop for SshAgent {
    fn drop(&mut self) {
        // Best-effort on every exit path, including unwind. `kill` then
        // `wait` so the child is reaped rather than left a zombie; both
        // errors are ignored because a drop cannot report and an agent that
        // already exited is exactly the state we want.
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

impl SshAgent {
    /// Start an agent and load `private_key_pem` into it over stdin.
    ///
    /// `private_key_pem` is borrowed, used once, and never stored on the
    /// returned value.
    pub(super) fn start_with_key(private_key_pem: &str) -> Result<Self, DomainError> {
        Self::start_with_key_in(&std::env::temp_dir(), private_key_pem)
    }

    /// [`start_with_key`](Self::start_with_key) with the parent directory
    /// chosen by the caller.
    ///
    /// Exists so the lifecycle tests can assert against a private root
    /// instead of a shared `/tmp`, where tests running in parallel see each
    /// other's agents and a "nothing was left behind" assertion becomes a
    /// race. Production always uses the system temp dir.
    pub(super) fn start_with_key_in(
        root: &Path,
        private_key_pem: &str,
    ) -> Result<Self, DomainError> {
        // Checked before spawning anything: `ssh-add` cannot load an
        // encrypted key non-interactively, and its failure is silent
        // (exit 1, no stderr), which would otherwise surface as an opaque
        // "ssh-add failed".
        if is_encrypted_private_key(private_key_pem) {
            return Err(DomainError::SyncFailed {
                message: "the configured SSH key is protected by a passphrase; qa-catalog \
                          cannot unlock it non-interactively; register a passphrase-less \
                          key instead"
                    .to_owned(),
            });
        }

        let dir = tempfile::Builder::new()
            .prefix("qa-catalog-ssh-")
            .tempdir_in(root)
            .map_err(|e| DomainError::SyncFailed {
                message: format!("failed to create the ssh agent directory: {e}"),
            })?;
        restrict_to_owner(dir.path())?;
        let socket = dir.path().join("agent.sock");
        let public_key = dir.path().join("id.pub");

        // See `ssh_command_for`: the command string is word-split, and a
        // single quote in the path would break the quoting that protects
        // spaces. Fail loudly rather than build a command that would
        // silently authenticate with no agent.
        if socket.to_string_lossy().contains('\'') || public_key.to_string_lossy().contains('\'') {
            return Err(DomainError::SyncFailed {
                message: "the temporary directory path contains a single quote, which \
                          cannot be passed safely to ssh; set TMPDIR to a path without one"
                    .to_owned(),
            });
        }

        // `-D` keeps the agent in the foreground so this `Child` *is* the
        // agent: a forking agent would print a pid we would have to parse
        // and signal by hand, and would escape the `Drop` guard if the
        // parse ever failed.
        let child = Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| DomainError::SyncFailed {
                message: format!(
                    "failed to start ssh-agent ({e}); the runtime image must provide \
                     openssh-client (see ADR-0005 as amended)"
                ),
            })?;

        let agent = Self {
            _dir: dir,
            socket,
            public_key,
            child,
        };
        agent.await_socket()?;
        agent.add_key(private_key_pem)?;
        agent.export_public_key()?;
        Ok(agent)
    }

    /// The agent's socket path.
    ///
    /// Production reads the socket through [`ssh_command`](Self::ssh_command);
    /// this accessor exists for `ssh_agent_tests`, which has to look at the
    /// agent's directory to assert the key never lands in it.
    #[allow(
        dead_code,
        reason = "used by ssh_agent_tests to assert the no-key-on-disk property"
    )]
    pub(super) fn socket(&self) -> &Path {
        &self.socket
    }

    /// The `core.sshCommand` value that routes `ssh` through this agent.
    pub(super) fn ssh_command(&self) -> String {
        ssh_command_for(&self.socket, &self.public_key)
    }

    /// Write the agent identity's **public** key into the agent directory,
    /// so the ssh command can name it with `-o IdentityFile`.
    ///
    /// # Why a file is unavoidable here
    ///
    /// `IdentitiesOnly=yes` does not mean "use the agent". Per
    /// `ssh_config(5)`: *"`IdentityFile` may be used in conjunction with
    /// `IdentitiesOnly` to select which identities in an agent are offered
    /// during authentication."* With `IdentitiesOnly=yes` and **no**
    /// `IdentityFile`, the candidate set is the *default* identity files
    /// only, so an agent key matching none of them is never offered and
    /// authentication fails — measured against a real `sshd`: the agent's
    /// key appeared zero times in `ssh -v` output and the connection ended
    /// `Permission denied (publickey)`, rc=255.
    ///
    /// Worse than the failure: in an image that *does* have a default
    /// identity file, that configuration silently authenticates with **that**
    /// key instead of the tenant's configured one.
    ///
    /// # The security property is preserved
    ///
    /// Only **public** material is written. It is read back out of the agent
    /// with `ssh-add -L` rather than derived from the PEM, so the private key
    /// still never leaves the agent's memory: it is the agent that answers
    /// the challenge, and `ssh -v` labels the identity `explicit agent`
    /// rather than a local key. A public key is not a secret — it is what
    /// the client sends the server in the clear during authentication
    /// anyway.
    ///
    /// The alternative, dropping `IdentitiesOnly=yes`, also authenticates,
    /// but it re-opens exactly the hole above: `ssh` would remain free to
    /// offer any other identity it finds. Keeping `IdentitiesOnly=yes` and
    /// naming the agent's own key is what confines the sync to the tenant's
    /// configured key.
    fn export_public_key(&self) -> Result<(), DomainError> {
        let output = Command::new("ssh-add")
            .arg("-L")
            .env("SSH_AUTH_SOCK", &self.socket)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| DomainError::SyncFailed {
                message: format!(
                    "failed to run ssh-add -L ({e}); the runtime image must provide \
                     openssh-client (see ADR-0005 as amended)"
                ),
            })?;

        if !output.status.success() || output.stdout.is_empty() {
            return Err(DomainError::SyncFailed {
                message: "the ssh agent reported no usable identity after loading the \
                          configured SSH key"
                    .to_owned(),
            });
        }

        std::fs::write(&self.public_key, &output.stdout).map_err(|e| DomainError::SyncFailed {
            message: format!("failed to write the agent public key: {e}"),
        })?;
        Ok(())
    }

    /// Block until the agent has bound its socket.
    fn await_socket(&self) -> Result<(), DomainError> {
        let deadline = Instant::now() + AGENT_SOCKET_TIMEOUT;
        while Instant::now() < deadline {
            if self.socket.exists() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(DomainError::SyncFailed {
            message: "ssh-agent did not create its socket in time".to_owned(),
        })
    }

    /// Feed the PEM to `ssh-add -` on stdin.
    ///
    /// Three things keep this non-interactive and non-leaking:
    /// * the key goes on **stdin**, so it is absent from `argv` and `environ`;
    /// * `SSH_AUTH_SOCK` is set **on this child only**, never on our own
    ///   process, so concurrent syncs cannot see each other's agents;
    /// * `SSH_ASKPASS=/bin/false` with `SSH_ASKPASS_REQUIRE=force` denies
    ///   `ssh-add` any way to prompt — without it a passphrase-protected key
    ///   can reach for `/dev/tty` and block forever.
    fn add_key(&self, private_key_pem: &str) -> Result<(), DomainError> {
        let mut child = Command::new("ssh-add")
            .arg("-")
            .env("SSH_AUTH_SOCK", &self.socket)
            .env("SSH_ASKPASS", "/bin/false")
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env_remove("DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| DomainError::SyncFailed {
                message: format!(
                    "failed to start ssh-add ({e}); the runtime image must provide \
                     openssh-client (see ADR-0005 as amended)"
                ),
            })?;

        // Scoped so the pipe is closed (EOF) before we wait, and so the
        // borrowed material is dropped from this frame promptly.
        {
            let mut stdin = child.stdin.take().ok_or_else(|| DomainError::SyncFailed {
                message: "failed to open a pipe to ssh-add".to_owned(),
            })?;
            // A broken pipe here means ssh-add already exited; the status
            // check below reports that, and the io error text must not be
            // used since it could echo the payload on some platforms.
            stdin.write_all(private_key_pem.as_bytes()).ok();
            if !private_key_pem.ends_with('\n') {
                stdin.write_all(b"\n").ok();
            }
        }

        let status = wait_with_timeout(&mut child, SSH_ADD_TIMEOUT)?;
        if !status {
            // Deliberately terse and material-free: `ssh-add`'s own stderr
            // is discarded above precisely so no part of the key can reach
            // a log or a persisted `sync_error`.
            return Err(DomainError::SyncFailed {
                message: "ssh-add rejected the configured SSH key; it must be a valid, \
                          passphrase-less private key in PEM or OpenSSH format"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

/// Wait for `child`, killing it if it outlives `timeout`. `Ok(true)` means
/// it exited successfully.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<bool, DomainError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    child.kill().ok();
                    child.wait().ok();
                    return Err(DomainError::SyncFailed {
                        message: "ssh-add timed out loading the configured SSH key".to_owned(),
                    });
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                return Err(DomainError::SyncFailed {
                    message: format!("failed to wait for ssh-add: {e}"),
                });
            }
        }
    }
}

/// Build the `core.sshCommand` string that routes `ssh` through the agent
/// listening on `socket`.
///
/// * `IdentityAgent=<socket>` — use *this* agent, not any ambient one.
/// * `IdentitiesOnly=yes` **together with** `IdentityFile=<public key>` —
///   offer only the agent's identity. These two are a **pair and must stay
///   one**: `IdentitiesOnly=yes` alone does not restrict ssh *to the agent*,
///   it restricts ssh to the *configured* identity files, which without an
///   `IdentityFile` means the default `~/.ssh/id_*` set. Alone it therefore
///   both fails to authenticate (the agent key is never offered) and, on a
///   host that has a default key, silently authenticates as the **wrong**
///   identity. See [`SshAgent::export_public_key`] for the measurement.
/// * `BatchMode=yes` — **not optional**: without it `ssh` can block forever
///   on an interactive prompt inside a container with no tty, turning an
///   authentication failure into a hung sync instead of a clean error.
/// * the host-key options — see [`HOST_KEY_VERIFICATION_OPTIONS`].
fn ssh_command_for(socket: &Path, public_key: &Path) -> String {
    // The socket path is single-quoted because this string is *word-split*
    // before it reaches `ssh`: gix runs it through `shell_words::split`
    // (or a real shell, for commands containing shell metacharacters), so
    // an unquoted temp dir containing a space would arrive as two arguments
    // and the agent option would be silently malformed. The directory name
    // itself is ours (`qa-catalog-ssh-…`); the space can only come from an
    // unusual `TMPDIR`. A path containing a single quote cannot be escaped
    // this way and is rejected in `start_with_key_in` instead.
    let mut parts = vec![
        "ssh".to_owned(),
        format!("-o IdentityAgent='{}'", socket.display()),
        format!("-o IdentityFile='{}'", public_key.display()),
        "-o IdentitiesOnly=yes".to_owned(),
        "-o BatchMode=yes".to_owned(),
    ];
    parts.extend(
        HOST_KEY_VERIFICATION_OPTIONS
            .iter()
            .map(|opt| format!("-o {opt}")),
    );
    parts.join(" ")
}

/// `chmod 0700` — the socket directory must not be traversable by other
/// users on the host, since connecting to the agent socket is equivalent to
/// using the key.
#[cfg(unix)]
fn restrict_to_owner(dir: &Path) -> Result<(), DomainError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        DomainError::SyncFailed {
            message: format!("failed to restrict the ssh agent directory: {e}"),
        }
    })
}

#[cfg(not(unix))]
fn restrict_to_owner(_dir: &Path) -> Result<(), DomainError> {
    // ssh-agent's unix-socket protocol is unix-only; the sync path refuses
    // ssh remotes before reaching here on other platforms.
    Ok(())
}

/// Whether `pem` is a passphrase-protected private key.
///
/// Two encodings have to be recognised:
///
/// * **OpenSSH** (`-----BEGIN OPENSSH PRIVATE KEY-----`, what `ssh-keygen`
///   writes by default): the base64 body decodes to `openssh-key-v1\0`
///   followed by a length-prefixed cipher name, which is the literal
///   `none` for an unencrypted key and e.g. `aes256-ctr` otherwise. This is
///   read structurally rather than sniffed, because the encrypted and
///   unencrypted forms are otherwise indistinguishable from the outside.
/// * **Classic PEM** (`BEGIN RSA PRIVATE KEY` and friends): encryption is
///   announced by `Proc-Type: 4,ENCRYPTED` / `DEK-Info:` headers.
fn is_encrypted_private_key(pem: &str) -> bool {
    if pem.contains("Proc-Type:") && pem.contains("ENCRYPTED") {
        return true;
    }
    if pem.contains("DEK-Info:") {
        return true;
    }
    let Some(cipher) = openssh_key_cipher(pem) else {
        // Not an OpenSSH-format key (or unparseable). Unparseable material
        // is not *encrypted*; let ssh-add reject it with its own error.
        return false;
    };
    cipher != "none"
}

/// The cipher name from an `openssh-key-v1` private key blob, if `pem` is
/// one and is well-formed enough to read it.
fn openssh_key_cipher(pem: &str) -> Option<String> {
    use base64::Engine as _;

    const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
    const END: &str = "-----END OPENSSH PRIVATE KEY-----";
    const MAGIC: &[u8] = b"openssh-key-v1\0";

    let start = pem.find(BEGIN)? + BEGIN.len();
    let end = pem[start..].find(END)? + start;
    let body: String = pem[start..end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .ok()?;

    let rest = decoded.strip_prefix(MAGIC)?;
    // 4-byte big-endian length, then that many bytes of cipher name.
    let (len_bytes, rest) = rest.split_at_checked(4)?;
    let len = u32::from_be_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    let name = rest.get(..usize::try_from(len).ok()?)?;
    String::from_utf8(name.to_vec()).ok()
}

#[cfg(test)]
#[path = "ssh_agent_tests.rs"]
mod ssh_agent_tests;

#[cfg(test)]
#[path = "ssh_auth_tests.rs"]
mod ssh_auth_tests;
