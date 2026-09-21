//! Two rules about file citations in this subsystem: every one must name a
//! file that **exists**, and none that points into **Markdown** may carry a
//! line number.
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
//! * **File existence only. Line numbers are never validated** — by
//!   [`every_file_citation_in_this_subsystem_resolves`]. A citation is read for
//!   its path; the `:N` is only what makes it recognisable as a citation.
//!   Validating line numbers means every insertion above a cited line is a test
//!   failure, which is noise rather than signal. For a citation into *code*
//!   that trade still holds, and the line number stays allowed.
//! * **Into Markdown, a line number is banned outright** — by
//!   [`no_resolvable_markdown_citation_carries_a_line_number`], which is a rule
//!   about the citation's *form* and needs no line to be read. See that test
//!   for why Markdown is the case where the trade goes the other way.
//! * **Citing sites are `.rs`, `.ts`, `.tsx`, `.sh` under `gears/qa-platform`,
//!   plus the top-level `docs/*.md` files** — `DESIGN.md`, `PRD.md`,
//!   `E2E-SCENARIOS.md` and any future direct child of `docs/`.
//!   These three are this workstream's living design record, edited as often
//!   as the code, and their file citations rot the exact way `.rs` citations
//!   do — WS6b's own last commit left one dangling. **Nested Markdown stays
//!   excluded**: `docs/ADR/**`, `docs/features/**` and `docs/.superpowers/**`
//!   (dated planning notes and task reports, never committed) legitimately
//!   cite a legacy tree or a past state of this repository, and the
//!   exclusion list that would be needed to carry them safely is the same
//!   enumerate-a-list method that produced the misses this guard exists to
//!   catch.
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

/// Extensions of the code files this guard scans wholesale and whose
/// citations it resolves.
const CODE_EXTENSIONS: &[&str] = &["rs", "tsx", "ts", "sh"];

/// Extensions a citation's own path may end in to be recognised at all —
/// [`CODE_EXTENSIONS`] plus `.md`, since a citation naming `docs/DESIGN.md`
/// is exactly as real as one naming `gear.rs`. This is broader than
/// [`CODE_EXTENSIONS`] on purpose: *scanning* every `.md` file for citations
/// would pull in dated planning notes (see [`is_scanned_top_level_doc`]), but
/// *recognising* a `.md` target when one is cited from a file this guard does
/// scan is always safe to check.
const CITATION_TARGET_EXTENSIONS: &[&str] = &["rs", "tsx", "ts", "sh", "md"];

/// Is `file` one of the top-level `docs/*.md` files — `docs/DESIGN.md`,
/// `docs/PRD.md`, and so on, but not anything under `docs/ADR/`,
/// `docs/features/` or `docs/.superpowers/`?
///
/// This is the one Markdown carve-in, and it is a carve-*in* rather than an
/// exemption list: it names a location (direct children of `docs/`), not a
/// set of files, so a fourth top-level doc added later is covered without
/// editing this function. Everything nested one directory deeper is a dated
/// document by construction — an ADR records a decision as of its acceptance
/// date, a feature doc a past shape, a `.superpowers/` plan or report a
/// snapshot of a task that has since landed — and validating those would
/// need exactly the curated exemption list this module's own header (above)
/// already refuses to keep. `docs/DESIGN.md`, `docs/PRD.md` and
/// `docs/E2E-SCENARIOS.md` are not dated: they are this
/// workstream's living design record, edited as often as the code they describe, so a
/// dangling citation in one of them is the same defect class as a dangling
/// citation in a `.rs` file, not a legitimate historical reference.
fn is_scanned_top_level_doc(file: &str) -> bool {
    file.strip_prefix("docs/")
        .is_some_and(|rest| {
            !rest.contains('/') && Path::new(rest).extension().is_some_and(|ext| ext == "md")
        })
}

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
        if CITATION_TARGET_EXTENSIONS
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
        if !is_code && !is_scanned_top_level_doc(file) {
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

/// A citation that points **into Markdown** must name a heading, never a line.
///
/// # Why Markdown is the case the sibling guard's trade does not cover
///
/// [`every_file_citation_in_this_subsystem_resolves`] deliberately never
/// validates a line number, because for code the cost of doing so is noise:
/// every insertion above a cited line would fail the build. That reasoning is
/// about *checking* the number. This test is about *writing* one, and for a
/// Markdown target the two come apart:
///
/// * A prose document is reorganised far more often than a function moves, and
///   a moved section takes every line below it with it. `DESIGN.md` §3.8 has
///   been renumbered from §3.7 and shifted repeatedly during this gear's life;
///   `m20260813_000003_initial`'s header records four such drifts and says what
///   each one did — *"it silently repointed a citation at a different rule,
///   which is worse than no citation"*.
/// * A Markdown target has something to cite **instead**: a numbered section
///   and a heading, which is greppable forever and survives every insertion
///   above it. Code has no equivalent, which is why the line number stays
///   allowed there.
///
/// So this is not the line-number validation the sibling guard refuses. It
/// never opens the cited file and never asks what is at line N. It asks only
/// whether the citation is written in a form that can rot, and a `:N` after a
/// `.md` is exactly that form.
///
/// # Scope: resolvable targets only, and that is the whole rule
///
/// A citation is this test's business precisely when the cited document is
/// **in this subsystem** — resolved the same way the sibling guard resolves
/// one, by path-suffix match against the tree. That is not a convenience
/// boundary, it is the boundary of what can be rewritten honestly: a heading
/// citation is only writable by someone who can read the document and see what
/// the heading says. The gear's own policy states the same limit from the
/// other side — *"where a target has no heading, the line number is given
/// together with enough quoted text to re-find it"*.
///
/// Left out by that rule, and each for the same reason:
///
/// * `../testrunner/docs/guides/run-parameters.md:37-43` — the legacy tree,
///   which is not in this repository. The sibling guard excludes the whole of
///   that tree from its own check on identical grounds.
/// * `DECOMPOSITION.md:148`, `plans/2026-08-18-qa-insights-gear.md:330`,
///   `exclusive-runs-and-the-queue.md:117` — documents that no longer exist
///   here, or whose bare basename names several files in the wider repository
///   and none in this subsystem.
///
/// Forcing those into heading form would mean inventing heading text for a
/// document the author cannot open, and a citation that names the wrong
/// heading is worse than the line number it replaced: the line number at least
/// admits it may be stale, where a confident wrong heading does not.
///
/// This is an allowlist-free rule, in the same spirit as the sibling guard's
/// anchor rule — it names a *property* (can this document be read from here?)
/// rather than a list of files, so a Markdown document added later is covered
/// without editing this test.
#[test]
fn no_resolvable_markdown_citation_carries_a_line_number() {
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

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        let is_code = CODE_EXTENSIONS
            .iter()
            .any(|ext| file.ends_with(&format!(".{ext}")));
        if !is_code && !is_scanned_top_level_doc(file) {
            continue;
        }
        let Ok(text) = fs::read_to_string(root.join(file)) else {
            continue;
        };
        for cited in citations(&text) {
            if Path::new(cited.as_str())
                .extension()
                .is_none_or(|ext| ext != "md")
            {
                continue;
            }
            // Resolvable here, exactly as the sibling guard resolves: the
            // cited document is one this repository can be read for its
            // headings. Anything else is out of scope; see this test's doc.
            let suffix = cited.rsplit_once(".../").map_or(cited.as_str(), |x| x.1);
            let resolves = index
                .iter()
                .any(|known| *known == suffix || known.ends_with(&format!("/{suffix}")));
            if resolves {
                offenders.push(format!("{file}: `{cited}:N`"));
            }
        }
    }
    offenders.sort();
    offenders.dedup();
    assert!(
        offenders.is_empty(),
        "{} citation(s) into Markdown carry a line number:\n  {}\n\nCite the \
         section number and heading text instead — `docs/DESIGN.md` §3.4, \
         \"Execution events\" — never a line. A line number that is right today \
         and wrong next week is a trap; a heading is greppable forever. Do not \
         add an exemption, and do not \"helpfully\" restore the numbers.",
        offenders.len(),
        offenders.join("\n  "),
    );
}
