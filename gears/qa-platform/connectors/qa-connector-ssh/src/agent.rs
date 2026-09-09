//! Short-lived per-session `ssh-agent`: how an SSH remote is authenticated
//! **without the private key ever becoming a file**.
//!
//! ## Two consumers, since this module moved
//!
//! It was written for `qa-catalog`'s git sync, where the unit is a *sync*,
//! and the vocabulary below still says "per-sync" wherever a git clone is
//! what is being described. Since the move into this connector there is a
//! second consumer with a different unit: [`crate::session::SshSession`],
//! where one agent serves one session's worth of remote commands against a
//! product's management node. Everything here holds for both -- the agent is
//! per-*use*, whatever the use is -- and nothing in this module knows or
//! cares which caller it has.
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
//!    private, `0700`, per-use temporary directory;
//! 2. hand the PEM to `ssh-add -` **on stdin**;
//! 3. point `ssh` at that agent with `-o IdentityAgent=<socket>`.
//!
//! The key therefore travels credstore → memory → a pipe → the agent's
//! memory. It is **never** written to a file, never placed in `argv` (which
//! is world-readable via `/proc/<pid>/cmdline`), never placed in an
//! environment variable (`/proc/<pid>/environ`), and never logged or
//! `Debug`-formatted. `agent_tests` pins each of those properties; in
//! particular `private_key_never_touches_the_filesystem` is what would fail
//! if someone later "simplified" this to `ssh -i /tmp/key`.
//!
//! ## Why the agent is per-use and addressed by config, not environment
//!
//! For `qa-catalog`, the socket is passed to `ssh` through `core.sshCommand`
//! **on the gix repository object**, not through a process-wide
//! `SSH_AUTH_SOCK`. gix resolves the ssh program per repository
//! (`gix-0.86.0/src/repository/config/mod.rs:91-97` reads `core.sshCommand`
//! into `ssh::connect::Options::command`), so two syncs running concurrently
//! with different keys each get their own agent. An environment variable
//! would be process-global and would race: whichever sync set it last would
//! decide which key *both* syncs authenticated with. [`SshSession`] reaches
//! the same property a different way -- it puts `-o IdentityAgent=<socket>`
//! in the argv it spawns `ssh` with -- and the race it avoids is the same
//! one, one environment's key deciding another environment's observation.
//!
//! ## Lifetime
//!
//! [`SshAgent`] reaps its child in `Drop`, so every exit path — success,
//! error, and unwind — kills the agent and removes its directory. A
//! happy-path `kill()` would leak one agent process per failed sync (or per
//! failed observation), which in a long-running gear is an unbounded resource
//! leak.
//!
//! ## THIS MODULE BLOCKS. It must never be called on an async worker.
//!
//! [`SshAgent::start_with_key`] spawns three synchronous `std::process`
//! children and polls two of them with `std::thread::sleep(20ms)`. There is
//! no `.await` point anywhere on that path, and it is bounded only by
//! [`AGENT_SOCKET_TIMEOUT`] (10s) and [`SSH_ADD_TIMEOUT`] (30s) — so its
//! worst case is 40 seconds of **uncancellable** work. Called directly from
//! an `async fn`, that stalls every other task sharing the tokio worker
//! thread, cannot be cancelled by a shutdown token, and outlives the
//! `stop_timeout` of at least one gear that reaches it.
//!
//! Every async caller must therefore go through
//! [`crate::session::SshSession::open_on_blocking_pool`] (which wraps this in
//! `tokio::task::spawn_blocking`) or wrap it itself, as `qa-catalog`'s
//! `infra/git/gix_sync.rs` does at its own call site. **Do not rely on the
//! call sites to remember**: this note exists because the discipline was
//! enforced only by there being a single caller, and evaporated the moment a
//! second one was added.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use credstore_sdk::SecretValue;

use crate::errors::SshFailure;

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
/// connection over a hostile or compromised network path (DNS hijack, BGP
/// hijack, on-path attacker) can be served by an impostor host that this
/// process will authenticate to. No trust-on-first-use memory accumulates
/// either, so a host key that silently changes between uses is never
/// noticed.
///
/// ## The blast radius grew on 2026-09-08; the ruling did not
///
/// **This is a record correction, not a policy change.** The ruling was
/// inherited unchanged when this module moved out of `qa-catalog`, and the
/// paragraph above described only what it meant for a *git clone*, because
/// that was the only consumer when it was written. It now governs
/// [`crate::session::SshSession`] as well, and that is not a clone:
///
/// * **git (`qa-catalog`).** The private key is not disclosed — the agent
///   only ever performs a signature, and `IdentitiesOnly=yes` restricts
///   which identity is offered — but **the repository content the gear
///   ingests can be attacker-chosen**, and that content drives plan
///   discovery and test execution.
/// * **an interactive session (`qa-vhi-product-plugin`).** Strictly worse.
///   [`crate::session::SshSession::exec`] runs commands as the configured
///   user, which for a VHI management node defaults to `root`, so an
///   impostor gets a live root shell's worth of traffic; and
///   [`crate::session::SshSession::exec_with_secret_env`] writes a **stored
///   credential in plaintext** to `ssh`'s stdin, encrypted to whichever host
///   completed the handshake. Where the git case lets an attacker choose
///   what we read, this case hands an attacker the vinfra administrator
///   password — one that is valid against the real cluster. The private key
///   is still not disclosed, for the same unchanged reason.
///
/// Whether to start pinning host keys is the human partner's decision and is
/// being put to them separately. `docs/ADR/0005-cpt-cf-qa-adr-git-egress.md`
/// carries the same correction in full.
///
/// ## What would have to change to tighten it
///
/// Pin the expected key(s): drop these two options, provision a known-hosts
/// file (from gear config or a credstore-held blob), and pass
/// `-o UserKnownHostsFile=<that file> -o StrictHostKeyChecking=yes`. The
/// per-use temp dir this module already creates is the natural place to
/// materialize it. Doing so requires an operator-facing way to supply host
/// keys, which is why it is not done here — and, for the session path, a
/// second question the git path did not have: an environment's expected host
/// key is *per-environment* data, so it belongs on the credential form
/// rather than in gear config.
pub const HOST_KEY_VERIFICATION_OPTIONS: [&str; 2] =
    ["StrictHostKeyChecking=no", "UserKnownHostsFile=/dev/null"];

/// How long to wait for `ssh-agent` to create its socket before giving up.
const AGENT_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for `ssh-add` to consume the key before giving up.
const SSH_ADD_TIMEOUT: Duration = Duration::from_secs(30);

/// A running `ssh-agent` holding exactly one identity, reaped on drop.
///
/// Deliberately **not** `Debug`/`Clone`: there is nothing here worth
/// formatting, and the type should never appear in a log line.
pub struct SshAgent {
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
    /// Start an agent and load `private_key`'s bytes into it over stdin.
    ///
    /// `private_key` is borrowed, read once, and never stored on the
    /// returned value.
    ///
    /// # Errors
    ///
    /// See [`SshFailure`]'s variants: an encrypted key, any step of preparing
    /// or driving `ssh-agent`/`ssh-add`, or a local I/O failure.
    pub fn start_with_key(private_key: &SecretValue) -> Result<Self, SshFailure> {
        Self::start_with_key_in(&std::env::temp_dir(), private_key)
    }

    /// [`start_with_key`](Self::start_with_key) with the parent directory
    /// chosen by the caller.
    ///
    /// Exists so the lifecycle tests can assert against a private root
    /// instead of a shared `/tmp`, where tests running in parallel see each
    /// other's agents and a "nothing was left behind" assertion becomes a
    /// race. Production always uses the system temp dir.
    ///
    /// # Errors
    ///
    /// See [`start_with_key`](Self::start_with_key).
    pub fn start_with_key_in(root: &Path, private_key: &SecretValue) -> Result<Self, SshFailure> {
        let key_bytes = private_key.as_bytes();
        // Borrowed, not converted: `String::from_utf8_lossy` would allocate
        // an un-zeroized `String` holding key-derived bytes on a non-UTF-8
        // key, and that copy would outlive `SecretValue`'s own zeroize-on-drop.
        // A non-UTF-8 key is simply not recognised as encrypted here (never
        // as *unencrypted* either -- see `is_encrypted_private_key`'s own
        // "unparseable" branch) and is left for `ssh-add` to reject on its
        // own terms below, against the original, unmangled bytes.
        let private_key_pem = std::str::from_utf8(key_bytes).unwrap_or("");

        // Checked before spawning anything: `ssh-add` cannot load an
        // encrypted key non-interactively, and its failure is silent
        // (exit 1, no stderr), which would otherwise surface as an opaque
        // "ssh-add failed".
        if is_encrypted_private_key(private_key_pem) {
            return Err(SshFailure::EncryptedKey);
        }

        let dir = tempfile::Builder::new()
            .prefix("qa-connector-ssh-")
            .tempdir_in(root)
            .map_err(|e| SshFailure::Internal {
                stage: "create the ssh agent directory",
                cause: e.to_string(),
            })?;
        restrict_to_owner(dir.path())?;
        let socket = dir.path().join("agent.sock");
        let public_key = dir.path().join("id.pub");

        // See `ssh_command_for`: the command string is word-split, and a
        // single quote in the path would break the quoting that protects
        // spaces. Fail loudly rather than build a command that would
        // silently authenticate with no agent.
        if socket.to_string_lossy().contains('\'') || public_key.to_string_lossy().contains('\'') {
            return Err(SshFailure::AgentSetup {
                stage: "start ssh-agent",
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
            .map_err(|_| SshFailure::AgentSetup {
                stage: "start ssh-agent",
            })?;

        let agent = Self {
            _dir: dir,
            socket,
            public_key,
            child,
        };
        agent.await_socket()?;
        agent.add_key(key_bytes)?;
        agent.export_public_key()?;
        Ok(agent)
    }

    /// The agent's socket path.
    ///
    /// Production reads the socket through [`ssh_command`](Self::ssh_command);
    /// this accessor also exists for `agent_tests`, which has to look at the
    /// agent's directory to assert the key never lands in it.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The path of the agent identity's exported **public** key.
    ///
    /// A caller assembling its own `ssh` invocation (rather than going
    /// through [`ssh_command`](Self::ssh_command)) needs this to pass
    /// alongside `IdentitiesOnly=yes`: per `export_public_key`'s own doc,
    /// that option *without* a paired `IdentityFile` never offers the
    /// agent's key at all -- `ssh` falls back to the *default* identity
    /// files instead, which on a host that has one (this is common) means
    /// authenticating with the wrong identity rather than failing loudly.
    /// Only public material lives at this path; see `export_public_key` for
    /// why writing it to disk does not weaken anything.
    #[must_use]
    pub fn identity_file(&self) -> &Path {
        &self.public_key
    }

    /// The `core.sshCommand` value that routes `ssh` through this agent.
    #[must_use]
    pub fn ssh_command(&self) -> String {
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
    fn export_public_key(&self) -> Result<(), SshFailure> {
        let output = Command::new("ssh-add")
            .arg("-L")
            .env("SSH_AUTH_SOCK", &self.socket)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| SshFailure::AgentSetup {
                stage: "export the public key",
            })?;

        if !output.status.success() || output.stdout.is_empty() {
            return Err(SshFailure::AgentSetup {
                stage: "export the public key",
            });
        }

        std::fs::write(&self.public_key, &output.stdout).map_err(|_| SshFailure::AgentSetup {
            stage: "export the public key",
        })?;
        Ok(())
    }

    /// Block until the agent has bound its socket.
    fn await_socket(&self) -> Result<(), SshFailure> {
        let deadline = Instant::now() + AGENT_SOCKET_TIMEOUT;
        while Instant::now() < deadline {
            if self.socket.exists() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(SshFailure::AgentSetup {
            stage: "wait for the agent socket",
        })
    }

    /// Feed the key's raw bytes to `ssh-add -` on stdin.
    ///
    /// Takes `&[u8]` rather than `&str` so the exact bytes `SecretValue` held
    /// reach `ssh-add` unchanged -- no UTF-8 re-encoding step exists to mangle
    /// a non-UTF-8 key before this point.
    ///
    /// Three things keep this non-interactive and non-leaking:
    /// * the key goes on **stdin**, so it is absent from `argv` and `environ`;
    /// * `SSH_AUTH_SOCK` is set **on this child only**, never on our own
    ///   process, so concurrent syncs cannot see each other's agents;
    /// * `SSH_ASKPASS=/bin/false` with `SSH_ASKPASS_REQUIRE=force` denies
    ///   `ssh-add` any way to prompt — without it a passphrase-protected key
    ///   can reach for `/dev/tty` and block forever.
    fn add_key(&self, private_key_pem: &[u8]) -> Result<(), SshFailure> {
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
            // `ssh-add`'s stderr is discarded above (`Stdio::null()`) rather
            // than captured, and the spawn error `e` is dropped here too:
            // both are folded into one fixed stage name so nothing that
            // could ever quote the key we are about to feed it has a path
            // to a log line or a persisted `sync_error`.
            .map_err(|_| SshFailure::AgentSetup {
                stage: "add the key to the agent",
            })?;

        // Scoped so the pipe is closed (EOF) before we wait, and so the
        // borrowed material is dropped from this frame promptly.
        {
            // `ssh-add`'s stderr is never captured (see above); losing this
            // pipe error too keeps the failure to the one fixed stage name.
            let mut stdin = child.stdin.take().ok_or(SshFailure::AgentSetup {
                stage: "add the key to the agent",
            })?;
            // A broken pipe here means ssh-add already exited; the status
            // check below reports that, and the io error text must not be
            // used since it could echo the payload on some platforms.
            stdin.write_all(private_key_pem).ok();
            if !private_key_pem.ends_with(b"\n") {
                stdin.write_all(b"\n").ok();
            }
        }

        let status = wait_with_timeout(&mut child, SSH_ADD_TIMEOUT)?;
        if !status {
            // Deliberately terse and material-free: `ssh-add`'s own stderr
            // is discarded above precisely so no part of the key can reach
            // a log or a persisted `sync_error`.
            return Err(SshFailure::AgentSetup {
                stage: "add the key to the agent",
            });
        }
        Ok(())
    }
}

/// Wait for `child`, killing it if it outlives `timeout`. `Ok(true)` means
/// it exited successfully.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<bool, SshFailure> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    child.kill().ok();
                    child.wait().ok();
                    // An expired deadline is `Timeout`, not `Internal`: it is
                    // the same condition a remote command exceeding
                    // `session::DEFAULT_TIMEOUT` produces, and
                    // `FailureClass` is an environment-page label and a
                    // Prometheus label value -- one situation must not be
                    // counted under two names depending on which deadline
                    // expired. See `errors`' own header.
                    return Err(SshFailure::Timeout);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                return Err(SshFailure::Internal {
                    stage: "wait for a child process",
                    cause: e.to_string(),
                });
            }
        }
    }
}

/// One `-o` option this transport sets.
///
/// # Why the type exists at all
///
/// The option set has **two** consumers and therefore two renderings: gix
/// takes a `core.sshCommand` *string* (see [`ssh_command_for`]), and
/// [`crate::session::SshSession`] builds an *argv*
/// ([`crate::session::argv_for_parts`]). Until 2026-09-09 each assembled the
/// same five-plus-two options independently, and only the string half was
/// tested — so deleting `BatchMode=yes` from the argv half broke nothing,
/// and a VHI observation in a tty-less pod would have hung instead of
/// erroring. Both renderings now come from [`transport_options`], so an
/// option cannot exist on one side only.
pub(crate) struct TransportOption {
    /// The option's name, as `ssh_config(5)` spells it.
    name: &'static str,
    /// Its value. The only runtime-derived values are the two paths.
    value: String,
    /// Whether [`Self::command_string_setting`] must single-quote the value.
    ///
    /// True for the two paths and nothing else: the command *string* is
    /// word-split before it reaches `ssh`, so a path containing a space would
    /// otherwise arrive as two arguments. In an argv there is nothing to
    /// split and a quote would become part of the value, which is why the two
    /// renderings differ here and only here.
    quote_value_in_command_string: bool,
}

impl TransportOption {
    /// `Name=value` — one argv element, exactly as `ssh -o` takes it.
    pub(crate) fn setting(&self) -> String {
        format!("{}={}", self.name, self.value)
    }

    /// The same, with a path value single-quoted, for a command string that
    /// will be word-split before `ssh` sees it.
    fn command_string_setting(&self) -> String {
        if self.quote_value_in_command_string {
            format!("{}='{}'", self.name, self.value)
        } else {
            self.setting()
        }
    }
}

/// **The** option set this transport authenticates with, in one place.
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
///   authentication failure into a hung sync (or a hung observation) instead
///   of a clean error.
/// * the host-key options — see [`HOST_KEY_VERIFICATION_OPTIONS`].
pub(crate) fn transport_options(socket: &Path, public_key: &Path) -> Vec<TransportOption> {
    let mut options = vec![
        TransportOption {
            name: "IdentityAgent",
            value: socket.display().to_string(),
            quote_value_in_command_string: true,
        },
        TransportOption {
            name: "IdentityFile",
            value: public_key.display().to_string(),
            quote_value_in_command_string: true,
        },
        TransportOption {
            name: "IdentitiesOnly",
            value: "yes".to_owned(),
            quote_value_in_command_string: false,
        },
        TransportOption {
            name: "BatchMode",
            value: "yes".to_owned(),
            quote_value_in_command_string: false,
        },
    ];
    options.extend(HOST_KEY_VERIFICATION_OPTIONS.iter().map(|option| {
        // The constant's entries are already `Name=value`; splitting keeps
        // them one policy statement in one place rather than a name and a
        // value that could be edited apart. A hypothetical entry with no `=`
        // would round-trip as a valueless setting rather than panicking.
        let (name, value) = option.split_once('=').unwrap_or((option, ""));
        TransportOption {
            name,
            value: value.to_owned(),
            quote_value_in_command_string: false,
        }
    }));
    options
}

/// Build the `core.sshCommand` string that routes `ssh` through the agent
/// listening on `socket`. Every option comes from [`transport_options`];
/// this function decides only how they are *spelled* for a consumer that
/// word-splits.
fn ssh_command_for(socket: &Path, public_key: &Path) -> String {
    // The paths are single-quoted because this string is *word-split*
    // before it reaches `ssh`: gix runs it through `shell_words::split`
    // (or a real shell, for commands containing shell metacharacters), so
    // an unquoted temp dir containing a space would arrive as two arguments
    // and the agent option would be silently malformed. The directory name
    // itself is ours (`qa-connector-ssh-…`); the space can only come from an
    // unusual `TMPDIR`. A path containing a single quote cannot be escaped
    // this way and is rejected in `start_with_key_in` instead.
    let mut parts = vec!["ssh".to_owned()];
    parts.extend(
        transport_options(socket, public_key)
            .iter()
            .map(|option| format!("-o {}", option.command_string_setting())),
    );
    parts.join(" ")
}

/// `chmod 0700` — the socket directory must not be traversable by other
/// users on the host, since connecting to the agent socket is equivalent to
/// using the key.
#[cfg(unix)]
fn restrict_to_owner(dir: &Path) -> Result<(), SshFailure> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        SshFailure::Internal {
            stage: "restrict the agent directory to its owner",
            cause: e.to_string(),
        }
    })
}

#[cfg(not(unix))]
fn restrict_to_owner(_dir: &Path) -> Result<(), SshFailure> {
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
#[path = "agent_tests.rs"]
mod agent_tests;

#[cfg(test)]
#[path = "auth_tests.rs"]
mod auth_tests;
