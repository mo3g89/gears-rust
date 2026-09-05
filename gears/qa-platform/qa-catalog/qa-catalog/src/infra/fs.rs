//! Filesystem helpers shared by the gear's local-storage adapters.

use std::io;
use std::path::Path;

/// Create `dir` (and missing parents) owner-only — mode `0700` on unix.
///
/// Both directory trees this gear owns hold data that must not be readable by
/// other local users on a shared host: `repos_dir` contains synced test
/// content (a tenant's private repositories), and `bundles_dir` contains
/// bundle blobs built from it. `create_dir_all`'s default `0777 & ~umask`
/// (typically `0755`) would leave both world-readable.
///
/// Only directories *this call creates* get the mode; a pre-existing
/// directory is left exactly as the operator set it up (this must not
/// silently `chmod` a path it does not own). File modes are likewise
/// untouched — the `0700` parent is what makes the subtree unreachable for
/// other users, since traversal requires the execute bit on every component.
///
/// On non-unix targets this is a plain recursive create (no mode concept).
///
/// # Errors
///
/// Propagates the underlying `std::fs::DirBuilder::create` failure (missing
/// permissions on a parent, a non-directory component in the path, …). An
/// already-existing directory is not an error.
pub fn create_private_dir_all(dir: impl AsRef<Path>) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir.as_ref())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::create_private_dir_all;

    #[test]
    fn creates_nested_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b/c");
        create_private_dir_all(&nested).unwrap();
        assert!(nested.is_dir());
    }

    #[test]
    fn is_idempotent_on_an_existing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        create_private_dir_all(tmp.path()).unwrap();
        create_private_dir_all(tmp.path()).unwrap();
        assert!(tmp.path().is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn created_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("outer/inner");
        create_private_dir_all(&nested).unwrap();

        // Every directory created by this call — not just the leaf — must be
        // owner-only, or the subtree stays traversable through the parent.
        for dir in [tmp.path().join("outer"), nested] {
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "{} must be owner-only, got {mode:o}",
                dir.display()
            );
        }
    }
}
