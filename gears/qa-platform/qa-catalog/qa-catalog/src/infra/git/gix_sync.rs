//! gix-based sync engine: the [`RepoSyncPort`] infra adapter (ADR-0005
//! `cpt-cf-qa-adr-git-egress`).
//!
//! ## Sync model
//!
//! One clone per repository at `<repos_dir>/<repo_id>/git` owns the object
//! and ref store. Each branch's content is materialized into its own plain
//! directory at `<repos_dir>/<repo_id>/branches/<branch_dir>` (see
//! `infra::git::layout`). `sync` clones on first use and fetches into the
//! existing clone afterwards, then rewrites the requested branch's snapshot
//! from its tip. The snapshot is cleared and rewritten on every sync — gix's
//! checkout only writes index entries and would otherwise leave files
//! deleted upstream lying around.
//!
//! Branch snapshots are **not** git worktrees: they hold no `.git` and the
//! repository index is never written for them. That is deliberate — the
//! index lives in the shared clone, so writing it per branch would make
//! concurrent materializations of different branches clobber each other.
//! Nothing reads these directories as git repositories; they are content
//! only (plan discovery, `TEST_META` parsing, bundle packing).
//!
//! ## Authentication — two schemes, decided by the URL
//!
//! `qa_test_repositories` carries one `credential_ref` and no auth-mode
//! column, so the URL scheme decides how the resolved material is used
//! (`domain::git_url`).
//!
//! **HTTP(S)** — the material is injected through gix's credential callback
//! and used for basic auth:
//!
//! - `user:secret` material authenticates as `user` with password `secret`;
//! - bare-token material authenticates as user `oauth2` with the token as
//!   the password (GitHub ignores the username for PATs; GitLab accepts
//!   `oauth2` for OAuth/PAT tokens).
//!
//! **SSH** (ADR-0005 as amended 2026-08-27) — the material is a private key
//! PEM. It is loaded into a short-lived per-sync `ssh-agent` over stdin and
//! reached through `-o IdentityAgent=<socket>` (with the
//! `IdentitiesOnly`/`IdentityFile` pair that makes `ssh` actually offer it),
//! so the private key never becomes a file;
//! see [`qa_connector_ssh::agent`] for the mechanism and for the deliberate
//! host-key-verification weakening -- moved out of this crate on 2026-09-08
//! so `qa-vhi-product-plugin` can share it, see that module's own doc for why
//! it moved rather than being copied. An SSH remote with no `credential_ref`
//! is attempted unauthenticated (public repositories over SSH exist) rather
//! than failing early.
//!
//! The ssh command is set as `core.sshCommand` **on the gix repository
//! config**, never as a process-wide `SSH_AUTH_SOCK`: gix resolves the ssh
//! program per repository, so per-repo config keeps concurrent syncs with
//! different keys from racing over one process-global variable.
//!
//! Local transports (`file://`, bare host paths) are blocked upstream by
//! `ReposService` URL validation (ADR-0005); this adapter does not
//! re-validate so tests can drive it against local fixtures.
//!
//! ## Credential non-leakage
//!
//! The credential material is held only by the auth callback closure and is
//! never logged, `Display`ed, or `Debug`-formatted. Error messages returned
//! as [`DomainError::SyncFailed`] or [`DomainError::CredentialRejected`] are
//! built exclusively from gix error chains and never interpolate the credential; repository URLs are
//! validated by `ReposService` to carry no userinfo, and the service
//! additionally sanitizes every persisted sync error (defense in depth).
//!
//! ## Limits
//!
//! Every talk with a remote ends (DESIGN §3.3 "Limits on talking to a
//! remote"). A sync runs under [`SyncLimits::sync_timeout`]: at the deadline
//! the interrupt flag gix checks on every read of the pack and during checkout
//! is set, the adapter waits up to `STOP_GRACE` for the work to stop and
//! answers [`DomainError::RemoteTimedOut`]; a listing runs under
//! [`SyncLimits::ls_refs_timeout`] and is detached at it. A peer that
//! trickles its handshake, a byte inside every stall bound, keeps an
//! abandoned operation's thread for as long as it keeps trickling. That is at
//! most one listing thread per url, one sync thread per repository (holding
//! that repository's clone directory), and one waiter: the next sync of the
//! repository polls for the directory on a thread of its own, but only until
//! its own deadline, so for up to `sync_timeout`. A new listing of a url
//! whose abandoned listing still runs answers at once without a thread; that
//! bound is keyed by the url alone, so it is shared by every repository and
//! every tenant that names the same url. Every
//! ssh remote runs with `BatchMode`, `ConnectTimeout` and `ServerAlive*`, with
//! or without a key, and over HTTP(S) the reqwest backend's own 20 s connect
//! and 30 s per-read stall bounds apply — `http.lowSpeedLimit`,
//! `http.lowSpeedTime` and `gitoxide.http.connectTimeout` are not set on
//! purpose, because that backend ignores them. A fetch may add at most
//! [`SyncLimits::max_fetch_bytes`] to the pack directory and a checkout may
//! write at most [`SyncLimits::max_checkout_bytes`]; either breach is
//! [`DomainError::SyncBudgetExceeded`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::SecretValue;
use gix::bstr::ByteSlice;
use qa_connector_ssh::SshAgent;

use crate::domain::error::DomainError;
use crate::domain::git_url::{RemoteKind, classify_remote};
use crate::domain::ports::repo_sync::{RepoSyncPort, SyncResult};

/// The remote name a cloned working copy tracks (gix's default, same as
/// git's `clone.defaultRemoteName` default).
const REMOTE_NAME: &str = "origin";

/// How long the adapter waits, after setting the interrupt flag, for the work
/// to stop before it answers anyway. Just past reqwest's 30 s per-read stall
/// bound, so a stalled HTTP transfer stops inside it. A peer that trickles its
/// handshake — a byte inside every stall bound — has no interrupt checkpoint:
/// its work is abandoned to its thread, which keeps the repository's
/// `host_locks` entry for as long as the peer keeps trickling. That is one
/// abandoned thread per such repository, plus the next sync of it, which
/// waits for the lock on a thread of its own only until its own deadline
/// ([`wait_for_clone_dir`]). Listings are bounded separately, per url
/// ([`GixSyncEngine::abandoned_listings`]).
const STOP_GRACE: Duration = Duration::from_secs(30);

/// How often a sync waiting for its clone directory checks its interrupt flag.
const LOCK_POLL: Duration = Duration::from_millis(50);

/// ssh bounds, appended to every ssh command. ssh applies `ConnectTimeout` to
/// the TCP connect and to the banner and key exchange; `ServerAlive*` drops a
/// peer that goes silent mid-transfer after about a minute.
const SSH_TRANSPORT_TIMEOUTS: &str =
    "-o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4";

/// The limits every remote operation runs under (DESIGN §3.3 "Limits on
/// talking to a remote"). A zero field disables that limit.
#[derive(Clone, Copy, Debug)]
pub struct SyncLimits {
    /// Deadline of one sync: clone or fetch, then checkout.
    pub sync_timeout: Duration,
    /// Deadline of one branch listing (ls-refs).
    pub ls_refs_timeout: Duration,
    /// Most bytes one clone or fetch may add to the pack directory.
    pub max_fetch_bytes: u64,
    /// Most bytes one branch checkout may write.
    pub max_checkout_bytes: u64,
}

impl Default for SyncLimits {
    fn default() -> Self {
        Self {
            sync_timeout: Duration::from_mins(5),
            ls_refs_timeout: Duration::from_secs(30),
            max_fetch_bytes: 1 << 30,
            max_checkout_bytes: 512 << 20,
        }
    }
}

/// `command` (an agent's ssh command) with the transport bounds appended. ssh
/// takes the first value it sees for an option, and the agent sets none of
/// these, so appending cannot override anything the agent chose.
fn with_transport_timeouts(command: &str) -> String {
    format!("{command} {SSH_TRANSPORT_TIMEOUTS}")
}

/// Per-operation SSH state: a running agent (when the remote is SSH and a
/// key was configured) plus the `core.sshCommand` that routes `ssh` at it.
///
/// Holding the [`SshAgent`] here is what ties the agent's lifetime to the
/// sync operation: every exit path drops this value, and the agent's `Drop`
/// reaps the process and removes its socket directory.
#[derive(Default)]
struct SshSetup {
    /// Whether the remote is ssh at all; an http(s) remote gets no command.
    is_ssh: bool,
    /// `None` for http(s) remotes and for unauthenticated ssh remotes.
    agent: Option<SshAgent>,
}

impl SshSetup {
    /// Prepare SSH authentication for `url`, if it needs any.
    ///
    /// An SSH remote **without** a credential is not an error: public
    /// repositories over SSH exist, and `ssh` may still succeed via host
    /// configuration. It simply gets no agent.
    fn prepare(url: &str, credential: Option<&str>) -> Result<Self, DomainError> {
        if classify_remote(url) != Ok(RemoteKind::Ssh) {
            return Ok(Self::default());
        }
        let Some(pem) = credential else {
            return Ok(Self {
                is_ssh: true,
                agent: None,
            });
        };
        let pem = SecretValue::from(pem);
        Ok(Self {
            is_ssh: true,
            agent: Some(SshAgent::start_with_key(&pem).map_err(|failure| agent_failure(&failure))?),
        })
    }

    /// The ssh command this operation must run: the agent's, or plain `ssh`
    /// for an ssh remote without a key — never gix's bare default, which has
    /// no `BatchMode` and no timeouts. `None` for http(s).
    fn command(&self) -> Option<String> {
        if !self.is_ssh {
            return None;
        }
        Some(match &self.agent {
            Some(agent) => with_transport_timeouts(&agent.ssh_command()),
            None => with_transport_timeouts("ssh"),
        })
    }
}

/// The credential the **HTTP basic-auth** callback is allowed to see.
///
/// `None` for SSH remotes, whatever is configured. On an SSH remote the
/// material is a *private key PEM*, and `split_credential` would happily
/// chop it into a `user:password` pair — so an SSH key could be sent as an
/// HTTP credential.
///
/// This is inert today: gix's ssh transport authenticates through `ssh`
/// and never invokes the credential callback, so the closure is simply
/// never called. It is gated anyway because "inert" here rests on the
/// internals of a dependency's transport dispatch, and the failure it
/// would produce — a private key leaving the process as an `Authorization`
/// header — is exactly the class of leak this whole design exists to
/// prevent. One gate is cheaper than depending on that remaining true.
fn http_credential<'a>(url: &str, credential: Option<&'a str>) -> Option<&'a str> {
    if classify_remote(url) == Ok(RemoteKind::Ssh) {
        return None;
    }
    credential
}

/// gix open options carrying `ssh_command` as `core.sshCommand`, if any.
///
/// `config_overrides` marks the values with `gix_config::Source::Api`, whose
/// kind is `Override` — so they outrank any `core.sshCommand` in the cloned
/// repository's own config and are honored regardless of that config's trust
/// level (`gix::config::is_trusted`). Mirrors what gix's own clone builder
/// does with its `config_overrides` field.
///
/// The `git_binary` permission and `Trust::Full` level reproduce gix's
/// private `open_opts_with_git_binary_config`, which is what
/// `gix::prepare_clone_bare` uses; keeping them identical means adding the
/// override changes nothing else about how the repository is opened.
fn open_options(ssh_command: Option<&str>) -> gix::open::Options {
    use gix::sec::trust::DefaultForLevel as _;

    let mut opts = gix::open::Options::default_for_level(gix::sec::Trust::Full);
    opts.permissions.config.git_binary = true;
    let overrides: Vec<String> = ssh_command
        .map(|cmd| format!("core.sshCommand={cmd}"))
        .into_iter()
        .collect();
    opts.config_overrides(overrides)
}

/// gix-backed [`RepoSyncPort`] implementation. Every call gets its
/// host-clone and branch-snapshot directories from the caller (see
/// `infra::git::layout`; `ReposService`) and runs the blocking gix machinery
/// on the blocking thread pool, under [`SyncLimits`].
// Constructed by the gear bootstrap (`crate::gear`) and injected into
// `ReposService`.
#[derive(Debug, Clone, Default)]
pub struct GixSyncEngine {
    limits: SyncLimits,
    /// One lock per clone directory, held by the blocking work itself. The
    /// caller's repo-tier lock is released when this adapter answers, which
    /// after a deadline can be before an abandoned handshake has ended; this
    /// lock is what still keeps the next sync of that repository out of the
    /// directory until it has.
    host_locks: HostLocks,
    /// How many listings of each url were abandoned at their deadline and are
    /// still running. While one is, a new listing of that url answers at once
    /// instead of starting another thread the same peer can hold. Keyed by the
    /// url alone: every repository and every tenant naming that url shares it.
    abandoned_listings: AbandonedListings,
}

/// The [`GixSyncEngine::abandoned_listings`] registry.
type AbandonedListings = Arc<std::sync::Mutex<HashMap<String, usize>>>;

/// The [`GixSyncEngine::host_locks`] registry.
type HostLocks = Arc<std::sync::Mutex<HashMap<PathBuf, Arc<std::sync::Mutex<()>>>>>;

impl GixSyncEngine {
    #[must_use]
    pub fn new(limits: SyncLimits) -> Self {
        Self {
            limits,
            host_locks: Arc::default(),
            abandoned_listings: Arc::default(),
        }
    }

    /// The lock of `host_dir`. Entries nobody holds are dropped on the way,
    /// so the registry stays as large as the syncs in flight.
    fn host_lock(&self, host_dir: &Path) -> Arc<std::sync::Mutex<()>> {
        let mut locks = self
            .host_locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        Arc::clone(locks.entry(host_dir.to_owned()).or_default())
    }
}

/// Take `lock`, giving up as soon as `interrupt` is set. A predecessor
/// abandoned at its deadline may hold the clone directory for as long as its
/// peer trickles; a sync that blocked in `lock()` could not see its own
/// deadline and would be abandoned too, one more thread per retry.
fn wait_for_clone_dir<'a>(
    lock: &'a std::sync::Mutex<()>,
    interrupt: &AtomicBool,
) -> Option<std::sync::MutexGuard<'a, ()>> {
    loop {
        match lock.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if interrupt.load(Ordering::Relaxed) {
                    return None;
                }
                std::thread::sleep(LOCK_POLL);
            }
        }
    }
}

/// A listing still running inside its deadline.
const LISTING_RUNNING: u8 = 0;
/// A listing the adapter answered for at its deadline and that still runs.
const LISTING_ABANDONED: u8 = 1;
/// A listing whose thread has ended.
const LISTING_FINISHED: u8 = 2;

fn lock_registry(
    registry: &std::sync::Mutex<HashMap<String, usize>>,
) -> std::sync::MutexGuard<'_, HashMap<String, usize>> {
    registry.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Dropped when a listing's thread ends, however it ends: takes the listing
/// out of [`GixSyncEngine::abandoned_listings`] if it was counted there. The
/// state moves under the registry lock on both sides, so a listing that ends
/// just as its deadline fires is counted and uncounted exactly once.
struct ListingFinished {
    registry: AbandonedListings,
    url: String,
    state: Arc<AtomicU8>,
}

impl Drop for ListingFinished {
    fn drop(&mut self) {
        let mut abandoned = lock_registry(&self.registry);
        if self.state.load(Ordering::Relaxed) == LISTING_ABANDONED
            && let Some(count) = abandoned.get_mut(&self.url)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                abandoned.remove(&self.url);
            }
        }
        self.state.store(LISTING_FINISHED, Ordering::Relaxed);
    }
}

/// `deadline` as a timeout message names it: whole seconds as `N s`, anything
/// else as `N ms`, so a sub-second deadline never reads "within 0 s".
fn render_deadline(deadline: Duration) -> String {
    if deadline.subsec_nanos() == 0 {
        format!("{} s", deadline.as_secs())
    } else {
        format!("{} ms", deadline.as_millis())
    }
}

/// Run `work` on the blocking pool with an interrupt flag it passes to gix. At
/// `deadline` the flag is set and the adapter waits up to `grace` for the work
/// to stop — gix checks it on every read of the pack and during checkout — and
/// then answers [`DomainError::RemoteTimedOut`] either way. A zero `deadline`
/// disables it.
async fn run_until_deadline<T: Send + 'static>(
    deadline: Duration,
    grace: Duration,
    what: &'static str,
    work: impl FnOnce(Arc<AtomicBool>) -> Result<T, DomainError> + Send + 'static,
) -> Result<T, DomainError> {
    let interrupt = Arc::new(AtomicBool::new(false));
    let mut handle = tokio::task::spawn_blocking({
        let interrupt = Arc::clone(&interrupt);
        move || work(interrupt)
    });
    let joined = if deadline.is_zero() {
        handle.await
    } else {
        match tokio::time::timeout(deadline, &mut handle).await {
            Ok(joined) => joined,
            Err(_elapsed) => {
                interrupt.store(true, Ordering::Relaxed);
                // Whatever the work did after its deadline does not count:
                // waiting is only so that a work item that does stop has
                // released the clone directory before this answers.
                if tokio::time::timeout(grace, &mut handle).await.is_err() {
                    tracing::warn!(
                        what,
                        "work did not stop within its grace period after the deadline; abandoned to its thread"
                    );
                }
                return Err(DomainError::RemoteTimedOut {
                    message: format!(
                        "{what} did not finish within {} and was stopped",
                        render_deadline(deadline)
                    ),
                });
            }
        }
    };
    joined.map_err(|e| DomainError::Internal(format!("{what} task join error: {e}")))?
}

#[async_trait]
impl RepoSyncPort for GixSyncEngine {
    async fn sync(
        &self,
        url: &str,
        branch: &str,
        credential: Option<&str>,
        host_dir: &Path,
        branch_workdir: &Path,
    ) -> Result<SyncResult, DomainError> {
        let url = url.to_owned();
        let branch = branch.to_owned();
        let credential = credential.map(ToOwned::to_owned);
        let host_dir = host_dir.to_owned();
        let branch_workdir = branch_workdir.to_owned();
        let host_lock = self.host_lock(&host_dir);
        let limits = self.limits;
        run_until_deadline(limits.sync_timeout, STOP_GRACE, "sync", move |interrupt| {
            let Some(_host) = wait_for_clone_dir(&host_lock, &interrupt) else {
                // An abandoned predecessor still holds the directory, and
                // this sync's own deadline came first.
                return Err(DomainError::SyncFailed {
                    message: "interrupted before it started".to_owned(),
                });
            };
            // The lock may have come free just as the deadline fired.
            if interrupt.load(Ordering::Relaxed) {
                return Err(DomainError::SyncFailed {
                    message: "interrupted before it started".to_owned(),
                });
            }
            sync_blocking(
                &url,
                &branch,
                credential.as_deref(),
                &host_dir,
                &branch_workdir,
                &limits,
                &interrupt,
            )
        })
        .await
    }

    async fn list_remote_branches(
        &self,
        url: &str,
        credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError> {
        let deadline = self.limits.ls_refs_timeout;
        let registry = Arc::clone(&self.abandoned_listings);
        if lock_registry(&registry).get(url).is_some_and(|n| *n > 0) {
            // A peer that trickles its handshake holds the abandoned listing's
            // thread for as long as it likes; asking it again would hand it
            // another one.
            return Err(DomainError::RemoteTimedOut {
                message: "a previous ls-refs of this remote has not ended; not listing it again until it has".to_owned(),
            });
        }
        let state = Arc::new(AtomicU8::new(LISTING_RUNNING));
        let listing = tokio::task::spawn_blocking({
            let url = url.to_owned();
            let credential = credential.map(ToOwned::to_owned);
            let finished = ListingFinished {
                registry: Arc::clone(&registry),
                url: url.clone(),
                state: Arc::clone(&state),
            };
            move || {
                let _finished = finished;
                list_remote_branches_blocking(&url, credential.as_deref())
            }
        });
        let joined = if deadline.is_zero() {
            listing.await
        } else {
            match tokio::time::timeout(deadline, listing).await {
                Ok(joined) => joined,
                // The listing keeps its thread until the transport gives up;
                // it owns nothing but its scratch directory. It is counted as
                // abandoned until it ends, unless it ended just now.
                Err(_elapsed) => {
                    let mut abandoned = lock_registry(&registry);
                    if state.load(Ordering::Relaxed) == LISTING_RUNNING {
                        state.store(LISTING_ABANDONED, Ordering::Relaxed);
                        *abandoned.entry(url.to_owned()).or_default() += 1;
                    }
                    return Err(DomainError::RemoteTimedOut {
                        message: format!(
                            "ls-refs did not finish within {}",
                            render_deadline(deadline)
                        ),
                    });
                }
            }
        };
        joined.map_err(|e| DomainError::Internal(format!("ls-refs task join error: {e}")))?
    }
}

/// Clone-or-fetch `url` into `host_dir`, then materialize `branch` into
/// `branch_workdir`.
///
/// A `host_dir` that is missing, unopenable, or tracking a different URL is
/// (re-)cloned from scratch — the clone is derived state, safe to discard.
/// Discarding it also invalidates every branch snapshot beneath it, so the
/// whole repository directory is cleared in that case.
///
/// Runs under `limits`' byte budgets and stops at `interrupt` (set by the
/// deadline, or by the pack budget's watchdog).
fn sync_blocking(
    url: &str,
    branch: &str,
    credential: Option<&str>,
    host_dir: &Path,
    branch_workdir: &Path,
    limits: &SyncLimits,
    interrupt: &Arc<AtomicBool>,
) -> Result<SyncResult, DomainError> {
    // One agent for the whole operation, dropped (and reaped) when this
    // function returns by any path, including an unwind.
    let ssh = SshSetup::prepare(url, credential)?;
    // An ssh remote's material is a private key; it must never reach the
    // basic-auth callback. See `http_credential`.
    let credential = http_credential(url, credential);

    let ssh_command = ssh.command();
    let existing = open_existing(url, host_dir, ssh_command.as_deref());
    if existing.is_none()
        && let Some(repo_root) = host_dir.parent()
        && repo_root.exists()
    {
        // The clone is unusable; drop the repository directory entirely so
        // no stale branch snapshot survives beside a fresh object store.
        std::fs::remove_dir_all(repo_root).map_err(|e| DomainError::SyncFailed {
            message: format!("failed to clear the stale working area: {e}"),
        })?;
    }

    // Started after the stale area is gone, so the baseline is what this
    // fetch starts from.
    let budget = PackBudget::start(host_dir, limits.max_fetch_bytes, Arc::clone(interrupt));
    let fetched = match existing {
        Some(repo) => fetch_existing(&repo, credential, interrupt).map(|branches| (repo, branches)),
        None => clone_host(url, credential, host_dir, ssh_command.as_deref(), interrupt),
    };
    if budget.exceeded() {
        drop(budget);
        drop(fetched);
        // The oversized pack is derived state; leave nothing of it behind.
        if let Some(repo_root) = host_dir.parent() {
            std::fs::remove_dir_all(repo_root).ok();
        }
        return Err(DomainError::SyncBudgetExceeded {
            message: format!(
                "the fetched pack grew past max_fetch_bytes ({} bytes)",
                limits.max_fetch_bytes
            ),
        });
    }
    drop(budget);
    let (repo, branches) = fetched?;

    // Membership is checked against the just-advertised refs, not the local
    // remote-tracking refs: fetch does not prune, so a stale tracking ref may
    // survive a branch deleted upstream.
    if !branches.iter().any(|b| b == branch) {
        return Err(DomainError::SyncFailed {
            message: format!("branch '{branch}' not found on the remote"),
        });
    }

    let head_commit = materialize_branch(
        &repo,
        branch,
        branch_workdir,
        limits.max_checkout_bytes,
        interrupt,
    )?;
    Ok(SyncResult {
        branches,
        head_commit,
    })
}

/// Open the existing clone, provided it tracks `url` as its `origin` fetch
/// remote. Any failure (no repo, corrupt repo, different URL) yields `None`,
/// which makes [`sync_blocking`] discard the working area and re-clone.
fn open_existing(url: &str, workdir: &Path, ssh_command: Option<&str>) -> Option<gix::Repository> {
    // Opened with the ssh override in place, so the fetch that follows uses
    // this operation's agent.
    let repo = gix::open_opts(workdir, open_options(ssh_command)).ok()?;
    let matches = {
        let remote = repo.find_remote(REMOTE_NAME).ok()?;
        let configured = remote.url(gix::remote::Direction::Fetch)?.to_bstring();
        configured == *url
    };
    matches.then_some(repo)
}

/// Bare clone of `url` into `host_dir` — objects and refs only, no worktree.
/// Branch content is materialized separately by [`materialize_branch`].
/// Returns the opened repository and the advertised branch inventory.
fn clone_host(
    url: &str,
    credential: Option<&str>,
    host_dir: &Path,
    ssh_command: Option<&str>,
    interrupt: &AtomicBool,
) -> Result<(gix::Repository, Vec<String>), DomainError> {
    let credential = credential.map(ToOwned::to_owned);

    std::fs::create_dir_all(host_dir).map_err(|e| DomainError::SyncFailed {
        message: format!("failed to create the clone directory: {e}"),
    })?;

    // `gix::prepare_clone_bare` with the ssh override folded into the open
    // options — the config has to be in place before `fetch_only` connects,
    // which is why this is not `repo.config_snapshot_mut()` after the fact.
    let mut prepare = gix::clone::PrepareFetch::new(
        url,
        host_dir,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
        open_options(ssh_command),
    )
    .map_err(|e| sync_err("failed to prepare clone", &e))?
    .configure_connection(move |connection| {
        connection.set_credentials(credential_helper(credential.clone()));
        Ok(())
    });

    let (repo, outcome) = prepare
        .fetch_only(gix::progress::Discard, interrupt)
        .map_err(|e| remote_err("clone failed", &e))?;

    let branches = remote_branch_names(&outcome.ref_map.remote_refs);
    Ok((repo, branches))
}

/// Fetch into the existing clone and return the advertised branch inventory.
/// Materialization is a separate step, so this touches no worktree.
fn fetch_existing(
    repo: &gix::Repository,
    credential: Option<&str>,
    interrupt: &AtomicBool,
) -> Result<Vec<String>, DomainError> {
    let remote = repo
        .find_remote(REMOTE_NAME)
        .map_err(|e| sync_err("failed to resolve the origin remote", &e))?;
    let mut connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| sync_err("failed to connect to the remote", &e))?;
    connection.set_credentials(credential_helper(credential.map(ToOwned::to_owned)));
    let outcome = connection
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(|e| remote_err("fetch negotiation failed", &e))?
        .receive(gix::progress::Discard, interrupt)
        .map_err(|e| sync_err("fetch failed", &e))?;

    Ok(remote_branch_names(&outcome.ref_map.remote_refs))
}

/// Materialize `branch`'s tree from the shared clone into `dest`, returning
/// the tip commit id.
///
/// The repository index is deliberately **not** written: it belongs to the
/// shared clone, and writing it here would make concurrent materializations
/// of different branches clobber one another. `dest` is content only.
///
/// A tree whose blobs add up to more than `max_checkout_bytes` (`0`: no
/// limit) is refused before anything is written.
fn materialize_branch(
    repo: &gix::Repository,
    branch: &str,
    dest: &Path,
    max_checkout_bytes: u64,
    interrupt: &AtomicBool,
) -> Result<String, DomainError> {
    let tracking_ref = format!("refs/remotes/{REMOTE_NAME}/{branch}");
    let commit_id = repo
        .find_reference(tracking_ref.as_str())
        .map_err(|e| sync_err("fetched branch has no remote-tracking ref", &e))?
        .peel_to_id()
        .map_err(|e| sync_err("failed to resolve the branch tip", &e))?
        .detach();

    let tree_id = repo
        .find_object(commit_id)
        .map_err(|e| sync_err("branch tip object missing after fetch", &e))?
        .peel_to_tree()
        .map_err(|e| sync_err("branch tip is not a treeish", &e))?
        .id;

    let index = repo
        .index_from_tree(&tree_id)
        .map_err(|e| sync_err("failed to build the index from the branch tree", &e))?;
    if max_checkout_bytes > 0 {
        let mut total: u64 = 0;
        // A submodule entry is a commit of another repository: not in this
        // object store, and checkout writes nothing of it.
        for entry in index.entries().iter().filter(|e| !e.mode.is_submodule()) {
            let header = repo
                .find_header(entry.id)
                .map_err(|e| sync_err("failed to read a blob header", &e))?;
            total = total.saturating_add(header.size());
            if total > max_checkout_bytes {
                return Err(DomainError::SyncBudgetExceeded {
                    message: format!(
                        "branch '{branch}' checks out more than max_checkout_bytes ({max_checkout_bytes} bytes)"
                    ),
                });
            }
        }
    }

    reset_dir(dest)?;

    if let Err(err) = write_snapshot(repo, index, dest, interrupt) {
        // A partial snapshot must not survive a failed checkout: `sync_error`
        // is repository-scoped, so a later successful sync of a *different*
        // branch would clear it, and `content_root_dir` would then accept
        // this directory as synced content — serving a partial (or empty)
        // plan list instead of `RepoNotSynced`. Removal is best-effort: if
        // it fails too, the sync still returns this error and the recorded
        // `sync_error` gates reads until the next successful sync.
        std::fs::remove_dir_all(dest).ok();
        return Err(err);
    }

    Ok(commit_id.to_string())
}

/// Checkout stage of [`materialize_branch`]: write `index`'s content into
/// the (freshly emptied) `dest`. Split out so the caller can remove the
/// half-written snapshot when any step here fails.
fn write_snapshot(
    repo: &gix::Repository,
    mut index: gix::index::File,
    dest: &Path,
    interrupt: &AtomicBool,
) -> Result<(), DomainError> {
    let mut opts = repo
        .checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)
        .map_err(|e| sync_err("failed to load checkout options", &e))?;
    // `reset_dir` just recreated it empty.
    opts.destination_is_initially_empty = true;

    let objects = repo
        .objects
        .clone()
        .into_arc()
        .map_err(|e| sync_err("failed to open the object database", &e))?;
    gix::worktree::state::checkout(
        &mut index,
        dest,
        objects,
        &gix::progress::Discard,
        &gix::progress::Discard,
        interrupt,
        opts,
    )
    .map_err(|e| sync_err("worktree checkout failed", &e))?;
    Ok(())
}

/// Recreate `dir` empty. Branch snapshots hold no `.git`, so unlike the old
/// single-working-copy model there is nothing to preserve.
fn reset_dir(dir: &Path) -> Result<(), DomainError> {
    let err = |e: std::io::Error| sync_err("failed to reset the branch snapshot", &e);
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(err)?;
    }
    std::fs::create_dir_all(dir).map_err(err)?;
    Ok(())
}

/// List `refs/heads/*` on `url` via a ref-map handshake (ls-refs) against a
/// throwaway scratch repository — no objects are fetched and nothing is
/// written to the repos dir.
fn list_remote_branches_blocking(
    url: &str,
    credential: Option<&str>,
) -> Result<Vec<String>, DomainError> {
    let scratch = tempfile::tempdir().map_err(|e| DomainError::SyncFailed {
        message: format!("failed to create a scratch directory for ls-refs: {e}"),
    })?;
    let ssh = SshSetup::prepare(url, credential)?;
    let credential = http_credential(url, credential);
    gix::init_bare(scratch.path())
        .map_err(|e| sync_err("failed to init the scratch repository", &e))?;
    // Re-opened with the ssh override: `init_bare` takes no open options, and
    // the handshake below needs `core.sshCommand` already resolved.
    let repo = gix::open_opts(scratch.path(), open_options(ssh.command().as_deref()))
        .map_err(|e| sync_err("failed to open the scratch repository", &e))?;
    let remote = repo
        .remote_at(url)
        .map_err(|e| sync_err("invalid remote url", &e))?
        .with_refspecs(["refs/heads/*:refs/heads/*"], gix::remote::Direction::Fetch)
        .map_err(|e| sync_err("failed to configure the branch refspec", &e))?;
    let mut connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| sync_err("failed to connect to the remote", &e))?;
    connection.set_credentials(credential_helper(credential.map(ToOwned::to_owned)));
    let (ref_map, _handshake) = connection
        .ref_map(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(|e| remote_err("ls-refs failed", &e))?;
    Ok(remote_branch_names(&ref_map.remote_refs))
}

/// Stops a fetch whose pack grows past `budget` bytes. gix writes the incoming
/// pack as a temporary file in `<host_dir>/objects/pack`
/// (`gix_pack::Bundle::write_to_directory`) and checks the interrupt flag on
/// every read of it, so a thread that samples that directory and sets the
/// flag bounds the bytes that reach the disk. A zero budget disables it.
struct PackBudget {
    dir: PathBuf,
    baseline: u64,
    budget: u64,
    tripped: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    watcher: Option<std::thread::JoinHandle<()>>,
}

impl PackBudget {
    fn start(host_dir: &Path, budget: u64, interrupt: Arc<AtomicBool>) -> Self {
        let dir = host_dir.join("objects").join("pack");
        let baseline = dir_size(&dir);
        let tripped = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let watcher = (budget > 0).then(|| {
            let (dir, tripped, done) = (dir.clone(), Arc::clone(&tripped), Arc::clone(&done));
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    if dir_size(&dir).saturating_sub(baseline) > budget {
                        tripped.store(true, Ordering::Relaxed);
                        interrupt.store(true, Ordering::Relaxed);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        });
        Self {
            dir,
            baseline,
            budget,
            tripped,
            done,
            watcher,
        }
    }

    /// Checked once more after the fetch: a pack smaller than one sample
    /// interval's transfer can land between two samples.
    fn exceeded(&self) -> bool {
        self.budget > 0
            && (self.tripped.load(Ordering::Relaxed)
                || dir_size(&self.dir).saturating_sub(self.baseline) > self.budget)
    }
}

impl Drop for PackBudget {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(watcher) = self.watcher.take() {
            // The watcher only samples and stores flags; a panic there has
            // nothing to hand back, and `exceeded` reads what it stored.
            watcher.join().ok();
        }
    }
}

/// Total size of the regular files directly in `dir`; `0` when it is absent.
fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.metadata().ok())
            .filter(std::fs::Metadata::is_file)
            .map(|meta| meta.len())
            .sum()
    })
}

/// Branch names (`refs/heads/` stripped) from the refs advertised by the
/// remote during the handshake, sorted. Branch names that are not valid
/// UTF-8 are skipped — the branch cache stores `String`s.
fn remote_branch_names(remote_refs: &[gix::protocol::handshake::Ref]) -> Vec<String> {
    let mut branches: Vec<String> = remote_refs
        .iter()
        .filter_map(|r| {
            let (full_name, _target, _object) = r.unpack();
            full_name
                .to_str()
                .ok()?
                .strip_prefix("refs/heads/")
                .map(ToOwned::to_owned)
        })
        .collect();
    branches.sort_unstable();
    branches.dedup();
    branches
}

/// Build the gix credential callback for one sync operation.
///
/// Always installed (even for `credential: None`) so gix never falls back
/// to system credential helpers or interactive prompts — a headless gear
/// must fail authentication cleanly instead. The closure is the only owner
/// of the material; it is never formatted or logged (see the module docs).
// The Err size is fixed by gix's callback signature (`protocol::Result`);
// nothing to shrink on our side.
#[allow(clippy::result_large_err)]
fn credential_helper(
    credential: Option<String>,
) -> impl FnMut(gix::credentials::helper::Action) -> gix::credentials::protocol::Result + 'static {
    move |action| match action {
        gix::credentials::helper::Action::Get(ctx) => {
            let Some(material) = credential.as_deref() else {
                // No configured credential: report "no identity available".
                return Ok(None);
            };
            let (username, password) = split_credential(material);
            Ok(Some(gix::credentials::protocol::Outcome {
                identity: gix::sec::identity::Account {
                    username,
                    password,
                    oauth_refresh_token: None,
                },
                next: gix::credentials::helper::NextAction::from(ctx),
            }))
        }
        // Nothing to persist or erase — the credential lives in credstore.
        gix::credentials::helper::Action::Store(_) | gix::credentials::helper::Action::Erase(_) => {
            Ok(None)
        }
    }
}

/// Split credential material into HTTP basic-auth username/password (see
/// the module docs for the accepted forms).
fn split_credential(material: &str) -> (String, String) {
    match material.split_once(':') {
        Some((username, password)) => (username.to_owned(), password.to_owned()),
        None => ("oauth2".to_owned(), material.to_owned()),
    }
}

/// [`DomainError::SyncFailed`] carrying `context` plus the gix error chain
/// (gix errors often bury the actionable cause several `source()`s deep).
/// Never called with credential material — see the module docs.
fn sync_err(context: &str, err: &(dyn std::error::Error + 'static)) -> DomainError {
    let mut parts = vec![format!("{context}: {err}")];
    let mut source = err.source();
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    DomainError::SyncFailed {
        message: parts.join(": "),
    }
}

/// [`sync_err`], except that the remote refusing this operation's credential —
/// or demanding one when none is configured, or the transport refusing to send
/// the configured one at all (a credential on a plain `http://` remote is never
/// sent in clear text) — is [`DomainError::CredentialRejected`]: a configuration fault an operator has to
/// fix, not an outage a retry may cure (DESIGN §3.3 "Branch model and the first
/// read of a branch"). Used at the three calls that run the handshake: ls-refs,
/// fetch negotiation, clone.
fn remote_err(context: &str, err: &(dyn std::error::Error + 'static)) -> DomainError {
    match sync_err(context, err) {
        DomainError::SyncFailed { message } if credentials_rejected(err) => {
            DomainError::CredentialRejected { message }
        }
        other => other,
    }
}

/// Whether `err` is the handshake refusing credentials, or the transport
/// refusing to offer them. Every gix layer between
/// the handshake and these call sites is `#[error(transparent)]`, which makes
/// `source()` skip the very value that says so, so the outer types are
/// unwrapped by hand. A gix upgrade that reshapes them fails
/// `a_refused_credential_is_credential_rejected_on_every_handshake_path`.
fn credentials_rejected(err: &(dyn std::error::Error + 'static)) -> bool {
    use gix::protocol::handshake::Error as Handshake;
    use gix::protocol::transport::client::Error as Transport;
    use gix::remote::{fetch::prepare, ref_map};

    fn by_transport(err: &Transport) -> bool {
        matches!(err, Transport::AuthenticationRefused(_))
    }
    fn by_handshake(err: &Handshake) -> bool {
        match err {
            Handshake::InvalidCredentials { .. } | Handshake::EmptyCredentials => true,
            Handshake::Transport(t) => by_transport(t),
            _ => false,
        }
    }
    fn by_ref_map(err: &ref_map::Error) -> bool {
        match err {
            ref_map::Error::Handshake(h) => by_handshake(h),
            ref_map::Error::Transport(t) => by_transport(t),
            _ => false,
        }
    }
    fn by_prepare(err: &prepare::Error) -> bool {
        matches!(err, prepare::Error::RefMap(r) if by_ref_map(r))
    }

    if let Some(e) = err.downcast_ref::<ref_map::Error>() {
        return by_ref_map(e);
    }
    if let Some(e) = err.downcast_ref::<prepare::Error>() {
        return by_prepare(e);
    }
    if let Some(e) = err.downcast_ref::<gix::clone::fetch::Error>() {
        return match e {
            gix::clone::fetch::Error::PrepareFetch(p) => by_prepare(p),
            gix::clone::fetch::Error::RefMap(r) => by_ref_map(r),
            _ => false,
        };
    }
    false
}

/// The ssh agent could not take the configured key. A passphrase-protected key
/// can never be offered — a configuration fault; any other agent failure is
/// this host's, and stays `SyncFailed`.
fn agent_failure(failure: &qa_connector_ssh::SshFailure) -> DomainError {
    // `SshFailure`'s `Display` is written for exactly this: it never renders
    // key material, and `ssh-add`'s stderr -- the one output that could -- is
    // dropped at the source rather than here.
    let message = failure.to_string();
    if matches!(failure, qa_connector_ssh::SshFailure::EncryptedKey) {
        DomainError::CredentialRejected { message }
    } else {
        DomainError::SyncFailed { message }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use super::{
        GixSyncEngine, PackBudget, SshSetup, SyncLimits, agent_failure, remote_err,
        render_deadline, run_until_deadline, split_credential, with_transport_timeouts,
    };
    use crate::domain::error::DomainError;
    use crate::domain::ports::repo_sync::RepoSyncPort;

    /// The deadline sets the interrupt flag, and the adapter waits for the work
    /// to notice it before answering: the work writes into the repository's
    /// working area, and answering earlier would let the next sync in under a
    /// live writer. Bounded: the work gives up on its own after 5 s.
    #[tokio::test]
    async fn a_deadline_interrupts_the_work_and_waits_for_it_to_stop() {
        use std::sync::atomic::Ordering;
        let stopped = std::sync::Arc::new(AtomicBool::new(false));
        let observed = std::sync::Arc::clone(&stopped);
        let started = std::time::Instant::now();
        let err = run_until_deadline(
            std::time::Duration::from_millis(50),
            std::time::Duration::from_secs(5),
            "sync",
            move |interrupt| {
                for _ in 0..500 {
                    if interrupt.load(Ordering::Relaxed) {
                        observed.store(true, Ordering::Relaxed);
                        return Err(DomainError::SyncFailed {
                            message: "interrupted".to_owned(),
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, DomainError::RemoteTimedOut { .. }),
            "got {err:?}"
        );
        assert!(
            stopped.load(Ordering::Relaxed),
            "the answer came after the work stopped"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    /// Work that ignores the flag (a trickling handshake has no checkpoint) is
    /// abandoned after the grace period, not waited on forever.
    #[tokio::test]
    async fn work_that_ignores_the_interrupt_is_abandoned_after_the_grace() {
        let started = std::time::Instant::now();
        let err = run_until_deadline(
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(100),
            "sync",
            |_interrupt| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, DomainError::RemoteTimedOut { message } if message == "sync did not finish within 50 ms and was stopped"),
            "a sub-second deadline is named in ms, never \"within 0 s\"; got {err:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(1500));
    }

    /// A timeout message names its deadline as configured: whole seconds as
    /// `s`, a sub-second or fractional deadline as `ms`.
    #[test]
    fn a_deadline_is_rendered_in_seconds_only_when_it_is_whole_seconds() {
        assert_eq!(render_deadline(Duration::from_secs(45)), "45 s");
        assert_eq!(render_deadline(Duration::from_millis(300)), "300 ms");
        assert_eq!(render_deadline(Duration::from_millis(1500)), "1500 ms");
        assert_eq!(render_deadline(Duration::ZERO), "0 s");
    }

    #[tokio::test]
    async fn work_that_finishes_in_time_is_answered_as_it_ended() {
        let value = run_until_deadline(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(5),
            "sync",
            |_interrupt| Ok(7),
        )
        .await
        .unwrap();
        assert_eq!(value, 7);
    }

    /// A sync whose clone directory is still held — by a predecessor abandoned
    /// at its deadline — does not touch it: it waits for the directory, and
    /// once that has used up its own deadline it ends without starting.
    /// Bounded: the holder lets go after 500 ms.
    #[tokio::test]
    async fn a_sync_stays_out_of_a_clone_directory_a_predecessor_still_holds() {
        let root = tempfile::tempdir().unwrap();
        let host_dir = root.path().join("repo").join("git");
        let workdir = root.path().join("repo").join("branches").join("main");
        let engine = GixSyncEngine::new(SyncLimits {
            sync_timeout: std::time::Duration::from_millis(100),
            ..SyncLimits::default()
        });
        let lock = engine.host_lock(&host_dir);
        let (held, is_held) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = lock.lock().unwrap();
            held.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
        });
        is_held.recv().unwrap();

        // A closed port: a sync that did run would fail at once, not time out.
        let err = engine
            .sync(
                "http://127.0.0.1:1/org/r.git",
                "main",
                None,
                &host_dir,
                &workdir,
            )
            .await
            .unwrap_err();
        holder.join().unwrap();
        assert!(
            matches!(err, DomainError::RemoteTimedOut { .. }),
            "got {err:?}"
        );
        assert!(
            !root.path().join("repo").exists(),
            "the held clone directory was touched"
        );
    }

    /// A sync whose clone directory never comes free — its predecessor's peer
    /// keeps trickling — ends at its own deadline, and so does its thread:
    /// the answer comes long before `STOP_GRACE`, which the adapter would
    /// wait out for a thread that could not see its interrupt. Bounded: the
    /// holder lets go once the answer is in, or after 10 s.
    #[tokio::test]
    async fn a_sync_waiting_for_a_clone_directory_that_never_comes_free_ends_at_its_deadline() {
        let root = tempfile::tempdir().unwrap();
        let host_dir = root.path().join("repo").join("git");
        let workdir = root.path().join("repo").join("branches").join("main");
        let engine = GixSyncEngine::new(SyncLimits {
            sync_timeout: std::time::Duration::from_millis(100),
            ..SyncLimits::default()
        });
        let lock = engine.host_lock(&host_dir);
        let (held, is_held) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _guard = lock.lock().unwrap();
            held.send(()).unwrap();
            // Released by the test, or by the bound if the test panicked first.
            released
                .recv_timeout(std::time::Duration::from_secs(10))
                .ok();
        });
        is_held.recv().unwrap();

        let started = std::time::Instant::now();
        let err = engine
            .sync(
                "http://127.0.0.1:1/org/r.git",
                "main",
                None,
                &host_dir,
                &workdir,
            )
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        release.send(()).unwrap();
        holder.join().unwrap();
        assert!(
            matches!(err, DomainError::RemoteTimedOut { .. }),
            "got {err:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "the waiting sync's thread did not end at its deadline: {elapsed:?}"
        );
        assert!(!root.path().join("repo").exists());
    }

    /// While a listing abandoned at its deadline still runs — its peer holds
    /// it — a new listing of that remote answers at once and opens no
    /// connection, so the peer cannot collect one thread per read or
    /// refresher pass. Once the abandoned listing ends, the remote is listed
    /// again. Bounded: the endpoint runs for at most 5 s and holds what it
    /// accepted only until released.
    #[tokio::test]
    async fn a_remote_whose_abandoned_listing_still_runs_is_not_listed_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let accepted = std::sync::Arc::new(AtomicUsize::new(0));
        let release = std::sync::Arc::new(AtomicBool::new(false));
        {
            let (accepted, release) = (
                std::sync::Arc::clone(&accepted),
                std::sync::Arc::clone(&release),
            );
            std::thread::spawn(move || {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                let mut held = Vec::new();
                while std::time::Instant::now() < until {
                    if release.load(Ordering::Relaxed) {
                        held.clear();
                    }
                    match listener.accept() {
                        Ok((stream, _)) => {
                            accepted.fetch_add(1, Ordering::Relaxed);
                            if !release.load(Ordering::Relaxed) {
                                held.push(stream);
                            }
                        }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                    }
                }
            });
        }
        let engine = GixSyncEngine::new(SyncLimits {
            ls_refs_timeout: std::time::Duration::from_millis(300),
            ..SyncLimits::default()
        });
        let url = format!("http://127.0.0.1:{port}/org/repo.git");

        let first = engine.list_remote_branches(&url, None).await.unwrap_err();
        assert!(
            matches!(first, DomainError::RemoteTimedOut { .. }),
            "got {first:?}"
        );
        let started = std::time::Instant::now();
        let second = engine.list_remote_branches(&url, None).await.unwrap_err();
        assert!(
            matches!(&second, DomainError::RemoteTimedOut { message } if message.contains("has not ended")),
            "got {second:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            accepted.load(Ordering::Relaxed),
            1,
            "the second listing opened a connection"
        );

        release.store(true, Ordering::Relaxed);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(4);
        let third = loop {
            let result = engine.list_remote_branches(&url, None).await;
            match &result {
                Err(DomainError::RemoteTimedOut { message })
                    if message.contains("has not ended") && std::time::Instant::now() < until =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                _ => break result,
            }
        };
        assert!(
            !matches!(&third, Err(DomainError::RemoteTimedOut { message }) if message.contains("has not ended")),
            "the ended listing still blocks the remote: {third:?}"
        );
        assert!(
            accepted.load(Ordering::Relaxed) >= 2,
            "the remote was listed again"
        );
    }

    /// Every ssh remote is bounded and non-interactive — with a key (appended
    /// to the agent's command) and without one (gix would otherwise run bare
    /// `ssh`, without even `BatchMode`).
    #[test]
    fn every_ssh_remote_gets_batch_mode_and_connection_timeouts() {
        let bare = SshSetup::prepare("ssh://git@git.example/org/r.git", None)
            .unwrap()
            .command()
            .expect("an ssh remote without a key still gets a command");
        let keyed =
            with_transport_timeouts("ssh -o 'IdentityAgent=/tmp/a/agent.sock' -o BatchMode=yes");
        for command in [&bare, &keyed] {
            for option in [
                "-o BatchMode=yes",
                "-o ConnectTimeout=15",
                "-o ServerAliveInterval=15",
                "-o ServerAliveCountMax=4",
            ] {
                assert!(command.contains(option), "{command} lacks {option}");
            }
        }
        assert!(
            keyed.starts_with("ssh -o 'IdentityAgent=/tmp/a/agent.sock'"),
            "the agent options stay first: {keyed}"
        );
        assert_eq!(
            SshSetup::prepare("https://git.example/org/r.git", None)
                .unwrap()
                .command(),
            None,
            "http remotes get no ssh command"
        );
    }

    /// The pack budget counts growth of the pack directory from where it
    /// started. Finite: four KiB written against a one-KiB budget.
    #[test]
    fn the_pack_budget_trips_on_growth_past_the_budget_and_not_before() {
        use std::sync::atomic::Ordering;
        let host = tempfile::tempdir().unwrap();
        let pack_dir = host.path().join("objects").join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(pack_dir.join("old.pack"), vec![0u8; 8192]).unwrap();
        let interrupt = std::sync::Arc::new(AtomicBool::new(false));

        let budget = PackBudget::start(host.path(), 1024, std::sync::Arc::clone(&interrupt));
        std::fs::write(pack_dir.join("tmp_pack_small"), vec![0u8; 512]).unwrap();
        assert!(
            !budget.exceeded(),
            "512 bytes of growth is inside the budget, whatever was there before"
        );
        std::fs::write(pack_dir.join("tmp_pack_big"), vec![0u8; 4096]).unwrap();
        assert!(budget.exceeded());
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            interrupt.load(Ordering::Relaxed),
            "the watchdog set the interrupt flag"
        );
    }

    #[test]
    fn split_credential_user_password_pair() {
        assert_eq!(
            split_credential("alice:s3cret"),
            ("alice".to_owned(), "s3cret".to_owned())
        );
    }

    #[test]
    fn split_credential_bare_token_uses_oauth2_username() {
        assert_eq!(
            split_credential("glpat-token"),
            ("oauth2".to_owned(), "glpat-token".to_owned())
        );
    }

    #[test]
    fn split_credential_splits_on_first_colon_only() {
        assert_eq!(
            split_credential("user:pa:ss"),
            ("user".to_owned(), "pa:ss".to_owned())
        );
    }

    fn denied() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "401 Unauthorized")
    }

    /// DESIGN §3.3: the remote refusing the credential it was given is a
    /// configuration fault, on each of the three paths that handshake.
    #[test]
    fn a_refused_credential_is_credential_rejected_on_every_handshake_path() {
        use gix::protocol::handshake::Error as Handshake;
        use gix::remote::{fetch::prepare, ref_map};
        let refused = || Handshake::InvalidCredentials {
            url: "https://git.example/r.git".into(),
            source: denied(),
        };

        let ls_refs = ref_map::Error::Handshake(refused());
        let fetch = prepare::Error::RefMap(ref_map::Error::Handshake(refused()));
        let clone = gix::clone::fetch::Error::PrepareFetch(prepare::Error::RefMap(
            ref_map::Error::Handshake(refused()),
        ));
        for (context, err) in [
            (
                "ls-refs failed",
                &ls_refs as &(dyn std::error::Error + 'static),
            ),
            ("fetch negotiation failed", &fetch),
            ("clone failed", &clone),
        ] {
            let mapped = remote_err(context, err);
            assert!(
                matches!(&mapped, DomainError::CredentialRejected { message } if message.starts_with(context)),
                "{context}: got {mapped:?}"
            );
        }
    }

    /// A remote that demands a credential when none is configured — an http
    /// remote without `credential_ref`, or an ssh remote whose key it refused
    /// (gix asks the callback, which never hands an ssh remote anything) — is
    /// the same configuration fault.
    #[test]
    fn a_demanded_credential_none_is_configured_for_is_credential_rejected() {
        let err = gix::remote::ref_map::Error::Handshake(
            gix::protocol::handshake::Error::EmptyCredentials,
        );
        assert!(matches!(
            remote_err("ls-refs failed", &err),
            DomainError::CredentialRejected { .. }
        ));
    }

    /// A credential the transport will not send — one configured for a plain
    /// `http://` remote, which gix refuses to put on the wire in clear text — is
    /// a configuration fault too: no retry sends it.
    #[test]
    fn a_credential_the_transport_will_not_send_is_credential_rejected() {
        use gix::protocol::transport::client::Error as Transport;
        let refused = || {
            Transport::AuthenticationRefused("Will not send credentials in clear text over http")
        };
        let after_handshake = gix::remote::ref_map::Error::Handshake(
            gix::protocol::handshake::Error::Transport(refused()),
        );
        let direct = gix::remote::ref_map::Error::Transport(refused());
        for err in [&after_handshake, &direct] {
            assert!(
                matches!(
                    remote_err("ls-refs failed", err),
                    DomainError::CredentialRejected { .. }
                ),
                "{err:?}"
            );
        }
    }

    /// Anything else stays `SyncFailed` — the outage a retry may cure.
    #[test]
    fn an_unreachable_remote_stays_sync_failed() {
        let err =
            gix::remote::ref_map::Error::Transport(gix::protocol::transport::client::Error::Io(
                std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused"),
            ));
        assert!(matches!(
            remote_err("ls-refs failed", &err),
            DomainError::SyncFailed { .. }
        ));
    }

    /// A passphrase-protected key can never be offered: a configuration fault.
    /// An agent that could not be started is not.
    #[test]
    fn an_encrypted_ssh_key_is_credential_rejected_and_an_agent_failure_is_not() {
        assert!(matches!(
            agent_failure(&qa_connector_ssh::SshFailure::EncryptedKey),
            DomainError::CredentialRejected { .. }
        ));
        assert!(matches!(
            agent_failure(&qa_connector_ssh::SshFailure::AgentSetup { stage: "spawn" }),
            DomainError::SyncFailed { .. }
        ));
    }
}

#[cfg(test)]
mod ssh_config_tests {
    #![allow(clippy::unwrap_used)]

    use super::open_options;

    /// **The ssh command must reach gix through the repository config, not
    /// the environment.**
    ///
    /// This asserts the mechanism ADR-0005's amendment depends on: gix reads
    /// `core.sshCommand` per repository
    /// (`gix-0.86.0/src/repository/config/mod.rs:91-97`) into
    /// `ssh::connect::Options::command`, which is what lets two syncs with
    /// two different keys run at once. A process-global `SSH_AUTH_SOCK`
    /// would race — whichever sync set it last would decide which key both
    /// used.
    ///
    /// It also pins a non-obvious detail: gix only honors `core.sshCommand`
    /// from a config section it considers *trusted*
    /// (`string_filter(Core::SSH_COMMAND, &mut trusted)`). The override is
    /// applied with `gix_config::Source::Api`, whose kind is `Override`
    /// rather than `Repository`, so it passes that filter regardless of who
    /// owns the repository directory. If that ever stopped holding, SSH auth
    /// would silently fall back to the ambient `ssh` and this test is what
    /// would catch it.
    #[test]
    fn ssh_command_is_applied_to_the_repository_config_not_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        gix::init_bare(dir.path()).unwrap();

        // Whatever ambient agent the host happens to have — a developer
        // workstation usually has one; a container usually does not.
        let ambient_before = std::env::var_os("SSH_AUTH_SOCK");

        let command = "ssh -o IdentityAgent=/tmp/agent-1/agent.sock -o BatchMode=yes";
        let repo = gix::open_opts(dir.path(), open_options(Some(command))).unwrap();

        let resolved = repo
            .ssh_connect_options()
            .unwrap()
            .command
            .expect("core.sshCommand must be visible to gix's ssh transport");
        assert_eq!(
            resolved.to_string_lossy(),
            command,
            "gix must resolve exactly the command we configured, including \
             when an unrelated agent is present in the environment"
        );

        // ...and the adapter reached that result without touching the
        // process environment. Asserting the variable is *absent* would be
        // asserting something about the host, not about this code.
        assert_eq!(
            std::env::var_os("SSH_AUTH_SOCK"),
            ambient_before,
            "the adapter must not modify the process-global SSH_AUTH_SOCK"
        );
    }

    /// Two repositories opened concurrently with different agents each see
    /// their own command — the property that makes concurrent syncs with
    /// different keys safe.
    #[test]
    fn concurrent_repositories_get_independent_ssh_commands() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        gix::init_bare(one.path()).unwrap();
        gix::init_bare(two.path()).unwrap();

        let cmd_one = "ssh -o IdentityAgent=/tmp/agent-one/agent.sock";
        let cmd_two = "ssh -o IdentityAgent=/tmp/agent-two/agent.sock";

        let repo_one = gix::open_opts(one.path(), open_options(Some(cmd_one))).unwrap();
        let repo_two = gix::open_opts(two.path(), open_options(Some(cmd_two))).unwrap();

        assert_eq!(
            repo_one
                .ssh_connect_options()
                .unwrap()
                .command
                .unwrap()
                .to_string_lossy(),
            cmd_one
        );
        assert_eq!(
            repo_two
                .ssh_connect_options()
                .unwrap()
                .command
                .unwrap()
                .to_string_lossy(),
            cmd_two
        );
    }

    /// An http(s) remote gets no `core.sshCommand` at all — the ssh path
    /// must not perturb the transport p1 already used.
    #[test]
    fn http_remotes_get_no_ssh_command_override() {
        let dir = tempfile::tempdir().unwrap();
        gix::init_bare(dir.path()).unwrap();
        let repo = gix::open_opts(dir.path(), open_options(None)).unwrap();
        assert!(
            repo.ssh_connect_options().unwrap().command.is_none(),
            "no ssh command should be configured for a non-ssh sync"
        );
    }
}

#[cfg(test)]
mod ssh_invocation_tests {
    #![allow(clippy::unwrap_used)]

    use super::open_options;

    /// **End-to-end wiring proof, minus a server.**
    ///
    /// The other ssh tests assert the command *string* and that gix
    /// *resolves* it from config. This one asserts gix actually **executes**
    /// it for an `ssh://` remote: a stand-in `ssh` records the argv it was
    /// invoked with, so a break anywhere in the chain (scheme classification,
    /// config override, gix's transport dispatch) shows up here.
    ///
    /// A real clone against a real private remote is not possible in this
    /// suite — there are no credentials, and no sshd to clone from. This is
    /// the closest honest substitute.
    #[test]
    #[cfg(unix)]
    fn gix_actually_invokes_the_configured_ssh_command_for_an_ssh_remote() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let argv_log = dir.path().join("argv.log");
        let fake_ssh = dir.path().join("fake-ssh");

        // Records how it was called, then fails like an unreachable host.
        std::fs::write(
            &fake_ssh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}\nexit 255\n",
                argv_log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let repo_dir = dir.path().join("scratch.git");
        gix::init_bare(&repo_dir).unwrap();

        // The agent socket path deliberately contains a space and is
        // single-quoted, exactly as `qa_connector_ssh::agent::ssh_command_for` (private in that crate) builds it:
        // this proves the quoting survives the word-splitting gix applies,
        // rather than only proving it about our own string formatting.
        let command = format!(
            "{} -o 'IdentityAgent=/tmp/agent e2e/agent.sock' -o BatchMode=yes \
             -o StrictHostKeyChecking=no",
            fake_ssh.display()
        );
        let repo = gix::open_opts(&repo_dir, open_options(Some(&command))).unwrap();

        // Attempt a handshake; it must fail (our fake ssh exits 255), but
        // only *after* gix has run the command.
        let remote = repo
            .remote_at("ssh://git@bitbucket.invalid/team/repo.git")
            .unwrap();
        // `connect` only *sets up* the transport — gix has already run the
        // configured command by this point (its `-G` ssh-variant probe),
        // which is exactly what the argv log below records. Whether connect
        // itself reports Ok or Err is not the subject of this test, so the
        // result is discarded rather than asserted on.
        drop(remote.connect(gix::remote::Direction::Fetch));

        let recorded = std::fs::read_to_string(&argv_log)
            .expect("gix must have executed the configured ssh command");
        // One argv entry per line: the command string is shell-split, so
        // `-o` and its value arrive as separate arguments (which is what
        // real ssh expects).
        let args: Vec<&str> = recorded.lines().collect();

        for expected in [
            // One argument, space and all — the quoting held.
            "IdentityAgent=/tmp/agent e2e/agent.sock",
            "BatchMode=yes",
            "StrictHostKeyChecking=no",
        ] {
            assert!(
                args.contains(&expected),
                "{expected} must reach the real ssh invocation, got: {args:?}"
            );
        }
        assert_eq!(
            args.iter().filter(|a| **a == "-o").count(),
            3,
            "each option must be passed as its own `-o <value>` pair: {args:?}"
        );
        assert!(
            args.iter().any(|a| a.contains("bitbucket.invalid")),
            "the remote host must reach the real ssh invocation, got: {args:?}"
        );
    }
}

#[cfg(test)]
mod http_credential_tests {
    #![allow(clippy::unwrap_used)]

    use super::{http_credential, split_credential};

    /// An SSH remote's private key must never be visible to the HTTP
    /// basic-auth callback, under either SSH syntax.
    #[test]
    fn ssh_remotes_expose_no_credential_to_the_http_callback() {
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----";
        for url in [
            "ssh://git@bitbucket.org/team/repo.git",
            "git@bitbucket.org:team/repo.git",
            "ssh://bitbucket.org:7999/team/repo.git",
        ] {
            assert_eq!(
                http_credential(url, Some(pem)),
                None,
                "{url} must not hand a private key to the basic-auth callback"
            );
        }
    }

    /// ...while http(s) remotes are unaffected.
    #[test]
    fn http_remotes_still_receive_their_credential() {
        for url in [
            "https://github.com/team/repo.git",
            "http://git.internal.example/team/repo.git",
        ] {
            assert_eq!(http_credential(url, Some("user:token")), Some("user:token"));
            assert_eq!(http_credential(url, None), None);
        }
    }

    /// Why the gate matters, stated as an executable fact: without it, a
    /// PEM reaching `split_credential` becomes an HTTP username/password
    /// pair — a private key formatted for an `Authorization` header.
    #[test]
    fn a_pem_would_otherwise_be_split_into_basic_auth_material() {
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nbody\n-----END OPENSSH PRIVATE KEY-----";
        let (user, password) = split_credential(pem);
        assert!(
            user.contains("BEGIN OPENSSH PRIVATE KEY") || password.contains("body"),
            "this test documents the hazard the gate removes; if `split_credential` \
             stops doing this, revisit `http_credential`'s rationale"
        );
    }
}
