//! Remote-URL classification for registered test repositories.
//!
//! One classifier, three consumers — this module exists because **the URL
//! scheme is the only thing that decides how `credential_ref` is
//! interpreted**. `qa_test_repositories` has a single nullable
//! `credential_ref` column and no auth-mode column
//! (`infra/storage/migrations/m20260812_000002_initial.rs:75-88`), so the
//! scheme has to carry that decision, and every place that makes it must
//! agree:
//!
//! 1. `domain::service::repos::validate_repo_url` — accept/reject at create
//!    and update time;
//! 2. `domain::service::repos::ReposService::resolve_credential` — decide
//!    whether `credential_ref` names an SSH key or HTTP basic-auth material;
//! 3. `infra::git::gix_sync` — decide whether to stand up an ssh-agent.
//!
//! A second, subtly different scheme test in any one of those is a security
//! bug (a URL that validates as http but syncs as ssh, or the reverse), so
//! they all call [`classify_remote`].
//!
//! ## Accepted forms (ADR-0005, amended 2026-08-27)
//!
//! * `https://host/path`, `http://host/path` — no userinfo permitted;
//! * `ssh://[user@]host[:port]/path`;
//! * scp-like `[user@]host:path`, e.g. `git@bitbucket.org:team/repo.git`.
//!
//! Everything else is refused: empty input, bare local paths, `file://`,
//! `git://`, Windows drive paths, and a bare hostname with no path.
//!
//! ## Userinfo is scheme-dependent, deliberately
//!
//! For http(s) any userinfo is credential material (`user:token@`, or a bare
//! token as the username) and is refused — credentials belong in credstore.
//! For ssh, `git@host` is the *username* and is mandatory in practice, so a
//! bare `user@` is permitted. `user:password@` stays refused for **both**:
//! an ssh password is still an embedded secret.

/// How a registered remote URL should be reached, and therefore how its
/// `credential_ref` is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKind {
    /// `http(s)://` — `credential_ref` names HTTP basic-auth material.
    Http,
    /// `ssh://` or scp-like — `credential_ref` names an SSH private key.
    Ssh,
}

/// Why a remote URL was refused. Carries no caller input, so the messages
/// are safe to return and to persist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteUrlError {
    Empty,
    UnsupportedScheme,
    EmbeddedCredentials,
}

impl RemoteUrlError {
    /// The operator-facing message for this refusal.
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "must not be empty",
            Self::UnsupportedScheme => {
                "supported repository URLs are https://, http://, ssh:// and scp-like \
                 user@host:path (see ADR-0005 cpt-cf-qa-adr-git-egress)"
            }
            Self::EmbeddedCredentials => {
                "must not embed credentials; store them in credstore and set credential_ref"
            }
        }
    }
}

/// Classify `url` as an accepted remote, or explain why it is refused.
pub fn classify_remote(url: &str) -> Result<RemoteKind, RemoteUrlError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(RemoteUrlError::Empty);
    }

    match url.split_once("://") {
        Some((scheme, rest)) => classify_with_scheme(scheme, rest),
        None => classify_scp_like(url),
    }
}

/// `scheme://rest` — the explicit-scheme forms.
fn classify_with_scheme(scheme: &str, rest: &str) -> Result<RemoteKind, RemoteUrlError> {
    let kind = if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        RemoteKind::Http
    } else if scheme.eq_ignore_ascii_case("ssh") {
        RemoteKind::Ssh
    } else {
        // `file://`, `git://`, and anything else. `file://` in particular
        // would let gix's local transport "sync" an arbitrary host path
        // (including another tenant's working copy under `repos_dir`),
        // turning repo registration into a host-file read via plan
        // discovery. That refusal is the original ADR-0005 boundary and is
        // unchanged by the SSH amendment.
        return Err(RemoteUrlError::UnsupportedScheme);
    };

    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.rsplit_once('@') {
        Some((userinfo, host)) => {
            // A password is an embedded secret under either scheme.
            if userinfo.contains(':') {
                return Err(RemoteUrlError::EmbeddedCredentials);
            }
            // A bare `user@` is credential material over http(s) (a token
            // pasted as the username) but is the login name over ssh.
            if kind == RemoteKind::Http {
                return Err(RemoteUrlError::EmbeddedCredentials);
            }
            host
        }
        None => authority,
    };

    if host.is_empty() {
        return Err(RemoteUrlError::UnsupportedScheme);
    }
    Ok(kind)
}

/// Scheme-less input: scp-like `[user@]host:path`, or a refusal.
///
/// The three things that must not slip through as scp-like are a bare local
/// path (`/var/lib/repo`, `./repo`), a Windows drive path (`C:\repo`), and a
/// bare hostname with no path (`bitbucket.org`).
fn classify_scp_like(url: &str) -> Result<RemoteKind, RemoteUrlError> {
    // Userinfo is whatever precedes the last `@` that comes before the first
    // `/`. Taking the *last* `@` keeps a `@` inside the path (legal) from
    // being read as a userinfo delimiter; bounding it by the first `/` keeps
    // a local path containing `@` from acquiring userinfo.
    let first_slash = url.find('/').unwrap_or(url.len());
    let (userinfo, remainder) = match url[..first_slash].rfind('@') {
        Some(at) => (Some(&url[..at]), &url[at + 1..]),
        None => (None, url),
    };
    if let Some(userinfo) = userinfo {
        if userinfo.is_empty() {
            return Err(RemoteUrlError::UnsupportedScheme);
        }
        // `user:pass@host:path` — refused, as under an explicit scheme.
        if userinfo.contains(':') {
            return Err(RemoteUrlError::EmbeddedCredentials);
        }
    }

    // scp-like splits host from path at the FIRST colon.
    let Some((host, path)) = remainder.split_once(':') else {
        // No colon at all: a bare local path (`/var/lib/repo`, `./repo`) or
        // a bare hostname (`bitbucket.org`). Neither names a remote.
        return Err(RemoteUrlError::UnsupportedScheme);
    };

    // A `/` before the colon means this is a path that happens to contain a
    // colon (`/srv/git:mirror`), not `host:path`.
    if host.is_empty() || host.contains('/') {
        return Err(RemoteUrlError::UnsupportedScheme);
    }
    // Windows drive letter: `C:\repo`, `C:/repo`. A single-character host is
    // never a real remote, so refusing the whole class is safe and needs no
    // separator sniffing.
    if host.len() == 1 && host.as_bytes()[0].is_ascii_alphabetic() {
        return Err(RemoteUrlError::UnsupportedScheme);
    }
    // `host:` with nothing after it names no repository.
    if path.is_empty() {
        return Err(RemoteUrlError::UnsupportedScheme);
    }

    Ok(RemoteKind::Ssh)
}

#[cfg(test)]
mod tests {
    use super::{RemoteKind, RemoteUrlError, classify_remote};

    /// The whole accept/reject policy in one table (ADR-0005 as amended
    /// 2026-08-27). Each row is `(url, expected)`.
    #[test]
    fn classify_remote_policy_table() {
        let cases: &[(&str, Result<RemoteKind, RemoteUrlError>)] = &[
            // --- http(s): accepted, userinfo refused -------------------
            ("https://github.com/team/tests.git", Ok(RemoteKind::Http)),
            ("http://git.example.com/team/tests", Ok(RemoteKind::Http)),
            ("HTTPS://GitHub.com/team/tests.git", Ok(RemoteKind::Http)),
            ("https://host:8443/team/tests.git", Ok(RemoteKind::Http)),
            // A bare token pasted as the username is still a secret.
            (
                "https://ghp_token@github.com/team/tests.git",
                Err(RemoteUrlError::EmbeddedCredentials),
            ),
            (
                "https://user:pass@github.com/team/tests.git",
                Err(RemoteUrlError::EmbeddedCredentials),
            ),
            // --- ssh:// : accepted, `user@` permitted -------------------
            ("ssh://git@bitbucket.org/team/repo.git", Ok(RemoteKind::Ssh)),
            ("ssh://bitbucket.org/team/repo.git", Ok(RemoteKind::Ssh)),
            (
                "ssh://git@bitbucket.org:7999/team/repo.git",
                Ok(RemoteKind::Ssh),
            ),
            ("SSH://git@bitbucket.org/team/repo.git", Ok(RemoteKind::Ssh)),
            // ...but a password is an embedded secret under ssh too.
            (
                "ssh://git:hunter2@bitbucket.org/team/repo.git",
                Err(RemoteUrlError::EmbeddedCredentials),
            ),
            // --- scp-like: the form the human's repos actually use ------
            (
                "git@bitbucket.org:virtuozzocore/vhp-core.git",
                Ok(RemoteKind::Ssh),
            ),
            ("bitbucket.org:team/repo.git", Ok(RemoteKind::Ssh)),
            ("git@github.com:team/repo", Ok(RemoteKind::Ssh)),
            (
                "git:hunter2@bitbucket.org:team/repo.git",
                Err(RemoteUrlError::EmbeddedCredentials),
            ),
            // --- still refused ------------------------------------------
            ("", Err(RemoteUrlError::Empty)),
            ("   ", Err(RemoteUrlError::Empty)),
            (
                "file:///srv/git/repo.git",
                Err(RemoteUrlError::UnsupportedScheme),
            ),
            (
                "git://github.com/team/repo.git",
                Err(RemoteUrlError::UnsupportedScheme),
            ),
            (
                "/var/lib/repos/other-tenant",
                Err(RemoteUrlError::UnsupportedScheme),
            ),
            ("./repo", Err(RemoteUrlError::UnsupportedScheme)),
            ("../../etc", Err(RemoteUrlError::UnsupportedScheme)),
            // A path that merely contains a colon is not `host:path`.
            ("/srv/git:mirror", Err(RemoteUrlError::UnsupportedScheme)),
            // Windows drive paths.
            (
                "C:\\Users\\me\\repo",
                Err(RemoteUrlError::UnsupportedScheme),
            ),
            ("C:/Users/me/repo", Err(RemoteUrlError::UnsupportedScheme)),
            // Bare hostname, no path.
            ("bitbucket.org", Err(RemoteUrlError::UnsupportedScheme)),
            ("bitbucket.org:", Err(RemoteUrlError::UnsupportedScheme)),
            ("https://", Err(RemoteUrlError::UnsupportedScheme)),
        ];

        for (url, expected) in cases {
            assert_eq!(
                classify_remote(url),
                *expected,
                "classify_remote({url:?}) disagreed with the policy table"
            );
        }
    }
}
