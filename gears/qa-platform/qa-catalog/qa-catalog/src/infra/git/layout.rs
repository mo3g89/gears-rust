//! On-disk layout of the multi-branch working area.
//!
//! ```text
//! <repos_dir>/<repo_id>/git                     one clone: objects + refs
//! <repos_dir>/<repo_id>/branches/<branch_dir>   per-branch content snapshot
//! ```
//!
//! Branch snapshots are plain directories — no `.git`, no index. They are
//! read-only content materializations, which is what lets many of them share
//! one object store (a real `git worktree` would need its own index, and gix
//! writes the index into the shared repository).

use std::path::{Path, PathBuf};

use aws_lc_rs::digest::{SHA256, digest};
use uuid::Uuid;

/// Directory holding the repository's single clone (objects + refs).
#[must_use]
pub fn host_dir(repos_dir: &Path, repo_id: Uuid) -> PathBuf {
    repos_dir.join(repo_id.to_string()).join("git")
}

/// Directory holding `branch`'s materialized content.
#[must_use]
pub fn branch_workdir(repos_dir: &Path, repo_id: Uuid, branch: &str) -> PathBuf {
    repos_dir
        .join(repo_id.to_string())
        .join("branches")
        .join(branch_dir_name(branch))
}

/// Filesystem-safe directory name for `branch`.
///
/// Normalization is deliberately lossy (slashes and dots become `-`), so a
/// digest of the original name is appended: without it `release/5.0` and
/// `release-5-0` would share one directory and serve each other's content.
#[must_use]
pub fn branch_dir_name(branch: &str) -> String {
    let trimmed = branch.trim();
    let normalized = trimmed
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let base = normalized.trim_matches('-').to_owned();

    // `aws-lc-rs`, the FIPS-validated provider, and not `sha2`, which Dylint's
    // `DE0708` bans. Same algorithm and the same bytes, so a branch directory
    // created before this swap keeps its name and is still found.
    let digest_hex = hex::encode(digest(&SHA256, trimmed.as_bytes()));
    let suffix = &digest_hex[..8];

    if base.is_empty() {
        suffix.to_owned()
    } else {
        format!("{base}-{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_branch_keeps_its_name_plus_digest() {
        let name = branch_dir_name("main");
        assert!(name.starts_with("main-"), "got {name}");
        assert_eq!(name.len(), "main-".len() + 8);
    }

    #[test]
    fn slashes_are_normalized() {
        assert!(branch_dir_name("release/5.0").starts_with("release-5-0-"));
    }

    #[test]
    fn lossy_normalization_does_not_collide() {
        assert_ne!(
            branch_dir_name("release/5.0"),
            branch_dir_name("release-5-0"),
            "the digest suffix must disambiguate names that normalize alike"
        );
    }

    #[test]
    fn is_deterministic() {
        assert_eq!(branch_dir_name("feature/x"), branch_dir_name("feature/x"));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(branch_dir_name("  main  "), branch_dir_name("main"));
    }

    #[test]
    fn name_that_normalizes_to_nothing_still_yields_a_directory() {
        let name = branch_dir_name("///");
        assert_eq!(name.len(), 8, "got {name}");
    }

    #[test]
    fn branch_workdir_nests_under_the_repo_id() {
        let repo_id = Uuid::nil();
        let path = branch_workdir(Path::new("/data/repos"), repo_id, "main");
        let expected_prefix = Path::new("/data/repos")
            .join(repo_id.to_string())
            .join("branches");
        assert!(path.starts_with(&expected_prefix), "got {path:?}");
    }

    #[test]
    fn host_dir_is_a_sibling_of_branches() {
        let repo_id = Uuid::nil();
        assert_eq!(
            host_dir(Path::new("/data/repos"), repo_id),
            Path::new("/data/repos")
                .join(repo_id.to_string())
                .join("git")
        );
    }
}
