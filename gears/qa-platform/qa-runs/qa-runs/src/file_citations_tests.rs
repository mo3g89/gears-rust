//! Every `path/to/file.rs:N` citation in this subsystem must name a file that
//! exists.
//!
//! # The defect class
//!
//! A rename moves a module; the prose that cited it does not move with it. Four
//! of the four Important findings in the Phase B branch review were this, and
//! nothing in the repository saw any of them: two live deploy scripts and three
//! UI source files went on pointing at `platforms_sea_repo.rs` and
//! `env_assembly.rs` for a whole phase after both files ceased to exist.
//!
//! The sibling guard in this crate — `doc_citations_tests.rs` — cannot see this
//! class and says so in its own header: it checks *identifiers*, explicitly not
//! file paths, only inside `qa-runs/src/**`, and only for bare tokens carrying
//! four or more underscores. So this is a second guard rather than an extension
//! of that one.
//!
//! # What is checked, and what is deliberately not
//!
//! * **File existence only. Line numbers are never validated.** A citation is
//!   read for its path; the `:N` is only what makes it recognisable as a
//!   citation. Validating line numbers means every insertion above a cited line
//!   is a test failure, which is noise rather than signal.
//! * **Citing sites are `.rs`, `.ts`, `.tsx` and `.sh` under
//!   `gears/qa-platform`.** Markdown is deliberately excluded: dated documents
//!   legitimately cite files that no longer exist, and the exclusion list that
//!   would be needed to carry them is the same enumerate-a-list method that
//!   produced the misses this guard exists to catch.
//! * **A citation is checked only when it is anchored at a real top-level
//!   directory of `gears/qa-platform`** — which is read from the tree, not
//!   listed here. That single rule is what keeps this guard allowlist-free.
//!   This subsystem's prose cites the legacy VHP tree constantly
//!   (`manager/src/...`, `routes/settings.rs:153`, `vhp-testrunner/...`) and
//!   those paths are unresolvable by construction, because that tree is not in
//!   this repository. They are not exemptions; they are simply not anchored
//!   here, and neither is a bare `some_file.rs:12` with no directory at all.
//!   The cost is real — a bare-basename citation is unchecked — and it is
//!   preferred to a curated list of legacy paths, which would need editing
//!   every time someone cited a new one.
//! * **A `...` elision is honoured.** `qa-runs/.../domain/runvars.rs:454`
//!   resolves on the segment after the last `.../`, still gated on its first
//!   segment being a real root.
//!
//! Because resolution is a path-suffix match, this guard answers "does a file
//! by this name exist in the place this citation says it does", not "is this
//! the file the sentence means". A citation that resolves can still be aimed at
//! the wrong line, and correcting that drift is a reading job, not a test.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Directories that are build output, vendored code or scratch space.
///
/// Not an exemption list for citations — nothing here is ever *cited*; these
/// are simply directories that must not be walked, and walking `node_modules`
/// alone would multiply the file count by two orders of magnitude.
const NOT_SOURCE: &[&str] = &[
    ".git",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
];

/// Extensions this guard both scans and resolves.
const CODE_EXTENSIONS: &[&str] = &["rs", "tsx", "ts", "sh"];

/// `gears/qa-platform`, derived from this crate's own location.
fn subsystem_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("this crate sits two levels under gears/qa-platform")
        .to_owned()
}

/// Every file under `root`, as a `/`-joined path relative to it.
fn all_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !NOT_SOURCE.contains(&name.as_ref()) {
                    walk(&path, root, out);
                }
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// Is this byte allowed inside a cited path?
fn is_path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-')
}

/// Every `path.ext:N` citation in `text`.
///
/// Found by scanning for a `:` followed by a digit and reading backwards
/// through path bytes, so `.tsx` is not mistaken for `.ts` and a bare `12:30`
/// or a `host:8080` is discarded for not ending in a code extension.
fn citations(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    for (colon, _) in text.match_indices(':') {
        if !bytes.get(colon + 1).is_some_and(u8::is_ascii_digit) {
            continue;
        }
        let mut start = colon;
        while start > 0 && is_path_byte(bytes[start - 1]) {
            start -= 1;
        }
        let candidate = &text[start..colon];
        if CODE_EXTENSIONS
            .iter()
            .any(|ext| candidate.ends_with(&format!(".{ext}")))
        {
            found.push(candidate.trim_start_matches("./").to_owned());
        }
    }
    found
}

/// Every file citation anchored in this subsystem names a file that exists.
#[test]
fn every_file_citation_in_this_subsystem_resolves() {
    let root = subsystem_root();
    let files = all_files(&root);
    assert!(
        files.len() > 300,
        "the walk of {} found {} files, which cannot be right — a broken walk \
         would make this whole test vacuous",
        root.display(),
        files.len(),
    );

    let index: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let roots: BTreeSet<String> = fs::read_dir(&root)
        .expect("the subsystem root is readable")
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !NOT_SOURCE.contains(&name.as_str()))
        .collect();
    assert!(
        roots.len() > 3,
        "found {} top-level directories under {}; the anchor rule would check \
         almost nothing",
        roots.len(),
        root.display(),
    );

    let mut dangling: Vec<String> = Vec::new();
    for file in &files {
        let is_code = CODE_EXTENSIONS
            .iter()
            .any(|ext| file.ends_with(&format!(".{ext}")));
        if !is_code {
            continue;
        }
        let Ok(text) = fs::read_to_string(root.join(file)) else {
            continue;
        };
        for cited in citations(&text) {
            // Anchored at a real root, or not this guard's business.
            let Some((first, _)) = cited.split_once('/') else {
                continue;
            };
            if !roots.contains(first) {
                continue;
            }
            let suffix = cited.rsplit_once(".../").map_or(cited.as_str(), |x| x.1);
            let resolves = index
                .iter()
                .any(|known| *known == suffix || known.ends_with(&format!("/{suffix}")));
            if !resolves {
                dangling.push(format!("{file}: `{cited}` names no file in the tree"));
            }
        }
    }
    dangling.sort();
    dangling.dedup();
    assert!(
        dangling.is_empty(),
        "{} file citation(s) name a path that does not exist:\n  {}\n\nA rename \
         moved the file and the prose did not follow. Repoint the citation at \
         the file's new home; do not add an exemption.",
        dangling.len(),
        dangling.join("\n  "),
    );
}
