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
//! as [`DomainError::SyncFailed`] are built exclusively from gix error
//! chains and never interpolate the credential; repository URLs are
//! validated by `ReposService` to carry no userinfo, and the service
//! additionally sanitizes every persisted sync error (defense in depth).

use std::path::Path;
use std::sync::atomic::AtomicBool;

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

/// Per-operation SSH state: a running agent (when the remote is SSH and a
/// key was configured) plus the `core.sshCommand` that routes `ssh` at it.
///
/// Holding the [`SshAgent`] here is what ties the agent's lifetime to the
/// sync operation: every exit path drops this value, and the agent's `Drop`
/// reaps the process and removes its socket directory.
#[derive(Default)]
struct SshSetup {
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
            return Ok(Self::default());
        };
        let pem = SecretValue::from(pem);
        Ok(Self {
            agent: Some(SshAgent::start_with_key(&pem).map_err(|failure| {
                DomainError::SyncFailed {
                    // `SshFailure`'s `Display` is written for exactly this: it never
                    // renders key material, and `ssh-add`'s stderr -- the one output
                    // that could -- is dropped at the source rather than here.
                    message: failure.to_string(),
                }
            })?),
        })
    }

    /// The ssh command this operation must run, if any.
    fn command(&self) -> Option<String> {
        self.agent.as_ref().map(SshAgent::ssh_command)
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

/// gix-backed [`RepoSyncPort`] implementation. Stateless: every call gets
/// its host-clone and branch-snapshot directories from the caller (see
/// `infra::git::layout`; `ReposService`) and runs the blocking gix machinery
/// on the blocking thread pool.
// Constructed by the gear bootstrap (`crate::gear`) and injected into
// `ReposService`.
#[derive(Debug, Clone, Copy, Default)]
pub struct GixSyncEngine;

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
        tokio::task::spawn_blocking(move || {
            sync_blocking(
                &url,
                &branch,
                credential.as_deref(),
                &host_dir,
                &branch_workdir,
            )
        })
        .await
        .map_err(|e| DomainError::Internal(format!("sync task join error: {e}")))?
    }

    async fn list_remote_branches(
        &self,
        url: &str,
        credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError> {
        let url = url.to_owned();
        let credential = credential.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || {
            list_remote_branches_blocking(&url, credential.as_deref())
        })
        .await
        .map_err(|e| DomainError::Internal(format!("ls-refs task join error: {e}")))?
    }
}

/// Clone-or-fetch `url` into `host_dir`, then materialize `branch` into
/// `branch_workdir`.
///
/// A `host_dir` that is missing, unopenable, or tracking a different URL is
/// (re-)cloned from scratch — the clone is derived state, safe to discard.
/// Discarding it also invalidates every branch snapshot beneath it, so the
/// whole repository directory is cleared in that case.
fn sync_blocking(
    url: &str,
    branch: &str,
    credential: Option<&str>,
    host_dir: &Path,
    branch_workdir: &Path,
) -> Result<SyncResult, DomainError> {
    // One agent for the whole operation, dropped (and reaped) when this
    // function returns by any path, including an unwind.
    let ssh = SshSetup::prepare(url, credential)?;
    // An ssh remote's material is a private key; it must never reach the
    // basic-auth callback. See `http_credential`.
    let credential = http_credential(url, credential);

    let ssh_command = ssh.command();
    let (repo, branches) = if let Some(repo) = open_existing(url, host_dir, ssh_command.as_deref())
    {
        let branches = fetch_existing(&repo, credential)?;
        (repo, branches)
    } else {
        // The clone is unusable; drop the repository directory entirely so
        // no stale branch snapshot survives beside a fresh object store.
        if let Some(repo_root) = host_dir.parent()
            && repo_root.exists()
        {
            std::fs::remove_dir_all(repo_root).map_err(|e| DomainError::SyncFailed {
                message: format!("failed to clear the stale working area: {e}"),
            })?;
        }
        clone_host(url, credential, host_dir, ssh_command.as_deref())?
    };

    // Membership is checked against the just-advertised refs, not the local
    // remote-tracking refs: fetch does not prune, so a stale tracking ref may
    // survive a branch deleted upstream.
    if !branches.iter().any(|b| b == branch) {
        return Err(DomainError::SyncFailed {
            message: format!("branch '{branch}' not found on the remote"),
        });
    }

    let head_commit = materialize_branch(&repo, branch, branch_workdir)?;
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
) -> Result<(gix::Repository, Vec<String>), DomainError> {
    let interrupt = AtomicBool::new(false);
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
        .fetch_only(gix::progress::Discard, &interrupt)
        .map_err(|e| sync_err("clone failed", &e))?;

    let branches = remote_branch_names(&outcome.ref_map.remote_refs);
    Ok((repo, branches))
}

/// Fetch into the existing clone and return the advertised branch inventory.
/// Materialization is a separate step, so this touches no worktree.
fn fetch_existing(
    repo: &gix::Repository,
    credential: Option<&str>,
) -> Result<Vec<String>, DomainError> {
    let interrupt = AtomicBool::new(false);

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
        .map_err(|e| sync_err("fetch negotiation failed", &e))?
        .receive(gix::progress::Discard, &interrupt)
        .map_err(|e| sync_err("fetch failed", &e))?;

    Ok(remote_branch_names(&outcome.ref_map.remote_refs))
}

/// Materialize `branch`'s tree from the shared clone into `dest`, returning
/// the tip commit id.
///
/// The repository index is deliberately **not** written: it belongs to the
/// shared clone, and writing it here would make concurrent materializations
/// of different branches clobber one another. `dest` is content only.
fn materialize_branch(
    repo: &gix::Repository,
    branch: &str,
    dest: &Path,
) -> Result<String, DomainError> {
    let interrupt = AtomicBool::new(false);

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

    reset_dir(dest)?;

    if let Err(err) = write_snapshot(repo, &tree_id, dest, &interrupt) {
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

/// Checkout stage of [`materialize_branch`]: write `tree_id`'s content into
/// the (freshly emptied) `dest`. Split out so the caller can remove the
/// half-written snapshot when any step here fails.
fn write_snapshot(
    repo: &gix::Repository,
    tree_id: &gix::ObjectId,
    dest: &Path,
    interrupt: &AtomicBool,
) -> Result<(), DomainError> {
    let mut index = repo
        .index_from_tree(tree_id)
        .map_err(|e| sync_err("failed to build the index from the branch tree", &e))?;
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
        .map_err(|e| sync_err("ls-refs failed", &e))?;
    Ok(remote_branch_names(&ref_map.remote_refs))
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::split_credential;

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
    /// suite — there are no credentials, and the compose stack's
    /// `git-fixture` serves smart-HTTP only (`deploy/compose/git-fixture/serve.sh`
    /// runs `git http-backend` behind a Python CGI server; it has no sshd).
    /// This is the closest honest substitute.
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
