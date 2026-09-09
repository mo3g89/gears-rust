//! One SSH connection's worth of remote command execution.
//!
//! # How a secret reaches a remote command
//!
//! Not through `argv`: the remote command string becomes the remote process's
//! `argv`, and `/proc/<pid>/cmdline` is world-readable, so anyone with a shell
//! on the target could read a password out of a running `vinfra`.
//!
//! Not through the SSH environment channel either: `sshd` accepts only the
//! variables its `AcceptEnv` lists, which by default is `LANG`/`LC_*`, so the
//! variable would be silently discarded and the command would fail with an
//! authorisation error whose stated cause is a wrong password.
//!
//! So [`SshSession::exec_with_secret_env`] writes the secret to **ssh's
//! stdin** and prefixes the remote command with a read that consumes it:
//!
//! ```text
//! IFS= read -r VAR; export VAR; <command>
//! ```
//!
//! The value then lives only in the remote shell's environment
//! (`/proc/<pid>/environ`, readable by its owner and root -- and the session is
//! root), never in any world-readable surface. Verified against the live VHI
//! stand before this module was written.

use std::process::Stdio;
use std::time::Duration;

use credstore_sdk::SecretValue;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

use crate::agent::SshAgent;
use crate::errors::SshFailure;

/// How long any one remote command may take.
///
/// Sized for the slowest call this connector serves today -- `vinfra node
/// list` against a cluster under load -- with room to spare, and short enough
/// that an observation ticker cannot be wedged by one unreachable host.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(1);

/// Where a session connects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
}

/// A live agent plus the target it authenticates to.
///
/// Not `Clone`: the agent is reaped on drop, and a second handle would outlive
/// its socket.
pub struct SshSession {
    target: SshTarget,
    agent: SshAgent,
    timeout: Duration,
}

impl SshSession {
    /// Start an agent holding `key` and bind it to `target`.
    ///
    /// # This blocks the calling thread, and must not be called on an async worker
    ///
    /// [`SshAgent::start_with_key`] spawns three synchronous child processes
    /// and polls two of them with `std::thread::sleep`, bounded by
    /// `agent.rs`'s own `AGENT_SOCKET_TIMEOUT` and `SSH_ADD_TIMEOUT` -- up to
    /// 40 seconds of *uncancellable* work with no `.await` point in it. On a
    /// tokio worker that stalls every other task on the same thread and
    /// outlives the gear's own `stop_timeout`. Async callers must use
    /// [`Self::open_on_blocking_pool`]; this synchronous constructor stays
    /// for callers that are already on a blocking thread (and for tests).
    ///
    /// # Errors
    ///
    /// Whatever [`SshAgent::start_with_key`] raises. Nothing is connected
    /// here: the first `exec` is what reaches the network.
    pub fn open(target: SshTarget, key: &SecretValue) -> Result<Self, SshFailure> {
        Ok(Self {
            target,
            agent: SshAgent::start_with_key(key)?,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// [`Self::open`], moved onto tokio's blocking pool.
    ///
    /// **The constructor every `async fn` must use.** See [`Self::open`] for
    /// what it is that blocks and for how long. This wrapper is the same
    /// discipline `qa-catalog`'s git sync applies at its own call site
    /// (`infra/git/gix_sync.rs`, `tokio::task::spawn_blocking` around
    /// `SshAgent::start_with_key`); it lives here rather than at each caller
    /// so a second caller cannot forget it -- which is exactly how it was
    /// lost once already.
    ///
    /// `key`'s bytes are copied into a second [`SecretValue`] because
    /// `spawn_blocking` requires `'static` and [`SecretValue`] is
    /// deliberately not `Clone`. The copy owns its own `Vec`, is zeroized on
    /// drop exactly as the original is, and is dropped inside the closure --
    /// so nothing derived from the key outlives this call.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::open`] raises, plus [`SshFailure::Internal`] if the
    /// blocking task itself could not be joined (a panic inside it, or a
    /// runtime shutting down). A `JoinError` names neither the key nor any
    /// child's output.
    pub async fn open_on_blocking_pool(
        target: SshTarget,
        key: &SecretValue,
    ) -> Result<Self, SshFailure> {
        let owned = SecretValue::new(key.as_bytes().to_vec());
        tokio::task::spawn_blocking(move || Self::open(target, &owned))
            .await
            .map_err(|error| SshFailure::Internal {
                stage: "start the ssh agent on the blocking pool",
                cause: error.to_string(),
            })?
    }

    /// Override the per-command deadline.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The exact argument vector a command would be run with.
    ///
    /// Public because a caller (or a reviewer) may need to see exactly what
    /// was run. `command` is the only caller-supplied element, and a secret
    /// must never be part of it -- which is asserted end-to-end against the
    /// kernel by `the_secret_never_appears_in_the_remote_processs_argv`
    /// rather than against this function's own output.
    ///
    /// The options themselves are **not** decided here: they come from
    /// [`crate::agent::transport_options`], the one table
    /// [`SshAgent::ssh_command`]'s own rendering is built from too. See
    /// [`argv_for_parts`], which is where they are assembled and which is
    /// what this crate's tests drive (it needs no running agent).
    #[must_use]
    pub fn argv_for(&self, command: &str) -> Vec<String> {
        argv_for_parts(
            self.agent.socket(),
            self.agent.identity_file(),
            &self.target,
            command,
        )
    }

    /// Run `command` on the target and return its stdout.
    ///
    /// # Errors
    ///
    /// [`SshFailure::Unreachable`] or [`SshFailure::AuthRejected`] for a
    /// transport failure (distinguished by `ssh`'s own diagnostic -- it exits
    /// 255 for both), [`SshFailure::CommandFailed`] for a non-zero remote
    /// exit, [`SshFailure::Timeout`] past the deadline.
    pub async fn exec(&self, command: &str) -> Result<String, SshFailure> {
        self.run(command, None).await
    }

    /// Run `command` with `var` set to `secret` in its environment, delivered
    /// on stdin. See this module's header for why not `argv` and not the SSH
    /// environment channel.
    ///
    /// The remote side of that delivery is `IFS= read -r VAR`, and `read -r`
    /// stops at the **first** newline byte -- so a `secret` containing one
    /// cannot be delivered intact: whatever follows the first newline is
    /// silently dropped, with no error and no diagnostic that names the
    /// value as the cause. Left unchecked, that failure would not surface
    /// here at all; it would surface downstream as an authorisation failure
    /// against the truncated value, pointing an operator at the wrong
    /// credential. So a secret containing a newline is refused up front,
    /// before anything is spawned, rather than delivered truncated.
    ///
    /// The same bytes are refused by the other side of this contract: the
    /// pytest suite's `lib/vinfra.py` raises rather than `strip()`ping a
    /// password file, so one stored value cannot get opposite verdicts from
    /// the two implementations.
    ///
    /// # Errors
    ///
    /// [`SshFailure::SecretNotDeliverable`] if `secret` contains a newline
    /// byte -- a variant of its own so a caller can tell an operator *which*
    /// credential to re-register. Otherwise as [`Self::exec`].
    pub async fn exec_with_secret_env(
        &self,
        var: &str,
        secret: &SecretValue,
        command: &str,
    ) -> Result<String, SshFailure> {
        if secret.as_bytes().contains(&b'\n') {
            return Err(SshFailure::SecretNotDeliverable);
        }
        let wrapped = format!("IFS= read -r {var}; export {var}; {command}");
        self.run(&wrapped, Some(secret)).await
    }

    async fn run(&self, command: &str, stdin: Option<&SecretValue>) -> Result<String, SshFailure> {
        let mut child = Command::new("ssh")
            .args(self.argv_for(command))
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SshFailure::Internal {
                stage: "spawn ssh",
                cause: e.to_string(),
            })?;

        if let Some(secret) = stdin {
            let mut pipe = child.stdin.take().ok_or(SshFailure::Internal {
                stage: "open ssh's stdin",
                cause: "the pipe was not created".to_owned(),
            })?;
            // A trailing newline is what `read` waits for; without it the
            // remote shell blocks until the deadline.
            pipe.write_all(secret.as_bytes())
                .await
                .map_err(|e| SshFailure::Internal {
                    stage: "write the secret to ssh's stdin",
                    cause: e.to_string(),
                })?;
            pipe.write_all(b"\n")
                .await
                .map_err(|e| SshFailure::Internal {
                    stage: "terminate the secret on ssh's stdin",
                    cause: e.to_string(),
                })?;
            drop(pipe);
        }

        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Err(_elapsed) => return Err(SshFailure::Timeout),
            Ok(Err(e)) => {
                return Err(SshFailure::Internal {
                    stage: "wait for ssh",
                    cause: e.to_string(),
                });
            }
            Ok(Ok(output)) => output,
        };

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(SshFailure::from_ssh_stderr(
            output.status.code().unwrap_or(-1),
            &stderr,
        ))
    }
}

/// [`SshSession::argv_for`]'s whole body, as a free function of the four
/// values it actually depends on.
///
/// Split out so the properties every `ssh` invocation must hold -- the agent
/// socket, the identity-file/`IdentitiesOnly` pair, `BatchMode=yes`, and the
/// host-key policy constant -- can be asserted **directly**, with no
/// `ssh-agent` running and no `sshd` to connect to. They previously could
/// not be: reaching `argv_for` needed an [`SshSession`], which needs a live
/// agent, so every one of those assertions existed against
/// [`SshAgent::ssh_command`] alone and this rendering was unpinned.
///
/// Ordering matters only for readability; `ssh` accepts `-o` options in any
/// order. The target and the command are last because that is how the
/// resulting line reads.
#[must_use]
pub fn argv_for_parts(
    socket: &std::path::Path,
    identity_file: &std::path::Path,
    target: &SshTarget,
    command: &str,
) -> Vec<String> {
    let mut argv = Vec::new();
    for option in crate::agent::transport_options(socket, identity_file) {
        argv.push("-o".to_owned());
        // The argv rendering, never the quoted command-string one: here each
        // option is already a single element, so a quote would become part of
        // the value `ssh` parses.
        argv.push(option.setting());
    }
    argv.push("-p".to_owned());
    argv.push(target.port.to_string());
    argv.push(format!("{}@{}", target.user, target.host));
    argv.push(command.to_owned());
    argv
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;
