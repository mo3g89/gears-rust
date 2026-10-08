//! Every migration name a gear's comments cite is a live module, or says it was
//! folded away.
//!
//! Shared by all four qa gears: `qa-runs` declares it as a module, and
//! `qa-insights`, `qa-environments` and `qa-catalog` include this same file with
//! `#[path]`, so there is one implementation to tighten and not four to drift.
//! `CARGO_MANIFEST_DIR` resolves per *including* crate, so each gear checks its
//! own `src/` (and its `-sdk` sibling's) against its own migrations directory.
//!
//! # Why it exists
//!
//! The migration chains were collapsed before the first installation, so a
//! comment that names a squashed migration reads as a file the reader can open
//! and cannot. The general citation guard could not see this class: its
//! bare-token rule is gated on four underscores and these names carry three.
//!
//! # What is scanned, after the fourth review
//!
//! Two tests. The per-gear one walks the filesystem under this gear's `src/`
//! and `tests/` and its `-sdk` sibling's `src/`, so an untracked new file is
//! read during development. The whole-tree one reads every **git-tracked** file
//! under `gears/qa-platform`, `apps/cf-gears-example-server`, the repository
//! `Makefile` and `.github/` -- the plugins, connectors, every `Cargo.toml`, the
//! UI inside and outside `src/`, `deploy/`, `config/`, and every document
//! including `docs/ADR/`. Both spellings of a migration name are recognised --
//! `m20260903_000004` as well as `m20260903_000004_plugin_instance_id_not_null`
//! (both folded into `m20260812_000002_initial` by the docs squash).
//!
//! Each earlier version listed the roots it read, and each review found a root
//! it had not listed: the UI and the short spelling at the second, the plugins
//! and documents at the third, `tests/`, the manifests, the UI's top level, the
//! `Makefile` and `.github/` at the fourth. Listing what git tracks makes a
//! new directory impossible to miss; what is left out is named instead -- the
//! two derived files `docs/openapi.json` and
//! `qa-platform-ui/src/api/generated/openapi.d.ts` (see [`walk_extensions`]).
//! `docs/ADR/` is read like any other document: an ADR that names a migration
//! since folded away says so in the same sentence.
//!
//! # What "marked" means, and why it is a clause and not a distance
//!
//! The first version accepted a citation if `folded into` or `squash` appeared
//! anywhere within 300 characters either side, in a file's comments joined into
//! one string. A genuinely stale name sitting near a legitimately marked one
//! therefore passed, which is the failure a guard exists to prevent. A mark now
//! has to be in **the same sentence** as the name it excuses, in the same
//! contiguous comment block, and **after** it -- so neither the next sentence
//! nor a different comment further down can launder a citation, and a second
//! stale name that follows a mark in one sentence is not covered by it.
//!
//! What remains loose, stated rather than hidden: two stale names in one
//! sentence before a single mark are both accepted (that is how a plural mark
//! reads), and a mark that is about a different migration than the one
//! immediately before it, in the same sentence, still counts. It is a
//! heuristic over prose, not a parse.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Is this a bare, lower-snake-case identifier?
fn is_snake_ident(token: &str) -> bool {
    !token.is_empty()
        && token.starts_with(|c: char| c.is_ascii_lowercase())
        && token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Is this token shaped like a `SeaORM` migration module name,
/// `m<YYYYMMDD>_<NNNNNN>` with the `_<slug>` tail **optional**?
///
/// Not gated on underscore count, which is the point: `m20260818_000006_collect_target`
/// has three underscores and slipped under the four-underscore bare-token rule
/// in the general citation guard, so six citations of a since-squashed migration
/// went unchecked.
///
/// # The slug is optional, and that was the second miss
///
/// The first version of this function required all three parts, so a citation
/// of the **short** form — `m20260903_000004` (folded into
/// `m20260812_000002_initial` by the docs squash), the spelling this
/// subsystem's prose actually reaches for when the slug adds nothing — was
/// not a migration name at all and went unchecked. Roughly twenty of them
/// named migrations squashed before the first installation. A short name is
/// accepted as live when a live module *begins* with it (see
/// [`cites_a_live_migration`]), because `m20260929_000003` and
/// `m20260929_000003_seed_run_completed_notification_claims` are the same file
/// under two spellings, not two files.
pub fn is_migration_name(token: &str) -> bool {
    let mut parts = token.splitn(3, '_');
    let (Some(date), Some(seq)) = (parts.next(), parts.next()) else {
        return false;
    };
    date.len() == 9
        && date.starts_with('m')
        && date[1..].chars().all(|c| c.is_ascii_digit())
        && seq.len() == 6
        && seq.chars().all(|c| c.is_ascii_digit())
        && parts.next().is_none_or(is_snake_ident)
}

/// Does `token` name one of the `live` migration modules, under either
/// spelling — the full module name, or the `m<date>_<seq>` prefix alone?
///
/// The prefix test requires the following character to be `_`, so a citation
/// cannot claim a live module by sharing only part of its sequence number.
fn cites_a_live_migration(token: &str, live: &BTreeSet<String>) -> bool {
    live.contains(token)
        || live
            .iter()
            .any(|name| name.strip_prefix(token).is_some_and(|tail| tail.starts_with('_')))
}

/// The text of each contiguous run of comment lines, markers stripped and
/// whitespace normalised so a mark wrapped onto the next line still reads as
/// one string. A line of code, or a blank line, ends a block.
///
/// Every comment kind, not only doc lines: three of the citations this was
/// written for were in ordinary comments.
///
/// # `/* … */` as well as `//`, because the UI is scanned too
///
/// TypeScript prose in this subsystem uses `JSDoc` — `/** … */` with ` * `
/// continuation lines — as often as `//`, and two migration citations live in
/// one. A block comment is read from its opener to its closer, each line's
/// leading `*` stripped, and the whole run flushed as one block, which is the
/// same unit a run of `///` lines produces. A `/* … */` opened and closed on
/// one line is a block of its own. This is a line-oriented reader, not a
/// lexer: a `/*` inside a string literal would start a block here. That costs
/// a false *acceptance* at worst (extra prose joined into a block), never a
/// false report, because a name only escapes by being followed by a mark.
fn comment_blocks(source: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut in_block_comment = false;
    let flush = |current: &mut Vec<String>, blocks: &mut Vec<String>| {
        if !current.is_empty() {
            let joined = current.join(" ");
            blocks.push(joined.split_whitespace().collect::<Vec<_>>().join(" "));
            current.clear();
        }
    };
    for line in source.lines() {
        let t = line.trim_start();
        if in_block_comment {
            let (text, ends) = match t.split_once("*/") {
                Some((before, _)) => (before, true),
                None => (t, false),
            };
            current.push(text.trim_start_matches('*').to_owned());
            in_block_comment = !ends;
            continue;
        }
        if let Some(rest) = t.strip_prefix("/*") {
            if let Some((body, _)) = rest.split_once("*/") {
                current.push(body.trim_start_matches('*').to_owned());
            } else {
                current.push(rest.trim_start_matches('*').to_owned());
                in_block_comment = true;
            }
            continue;
        }
        match t
            .strip_prefix("//!")
            .or_else(|| t.strip_prefix("///"))
            .or_else(|| t.strip_prefix("//"))
        {
            Some(text) => current.push(text.to_owned()),
            None => flush(&mut current, &mut blocks),
        }
    }
    flush(&mut current, &mut blocks);
    blocks
}

/// Split normalised prose into sentences: a `.`, `?` or `!` followed by a space
/// and an uppercase letter or a backtick (this crate opens sentences with a
/// cited identifier), or by the end of the text. `e.g.`, `i.e.`, `etc.` and
/// `vs.` do not end one.
fn sentences(prose: &str) -> Vec<&str> {
    let bytes = prose.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..bytes.len() {
        if matches!(bytes[i], b'.' | b'?' | b'!') {
            let ends = match bytes.get(i + 1) {
                None => true,
                Some(b' ') => {
                    bytes
                        .get(i + 2)
                        .is_none_or(|c| c.is_ascii_uppercase() || *c == b'`')
                        && !prose[..i].ends_with("e.g")
                        && !prose[..i].ends_with("i.e")
                        && !prose[..i].ends_with("etc")
                        && !prose[..i].ends_with("vs")
                }
                Some(_) => false,
            };
            if ends {
                out.push(&prose[start..=i]);
                start = i + 1;
            }
        }
    }
    if start < prose.len() {
        out.push(&prose[start..]);
    }
    out
}

fn says_folded_away(text: &str) -> bool {
    text.contains("folded into") || text.contains("squash")
}

/// Migration names cited in comment prose that neither exist (`live`) nor are
/// followed, in the same sentence, by a statement that they were folded away
/// (`folded into`, or any form of `squash`).
fn unmarked_migration_citations(source: &str, live: &BTreeSet<String>) -> Vec<String> {
    unmarked_in_blocks(comment_blocks(source), live)
}

/// The same rule over prose already split into blocks: a comment block for
/// source files, a paragraph for Markdown.
fn unmarked_in_blocks(blocks: Vec<String>, live: &BTreeSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for block in blocks {
        for sentence in sentences(&block) {
            let mut offset = 0;
            for token in sentence.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                let start = sentence[offset..]
                    .find(token)
                    .map_or(offset, |i| offset + i);
                offset = start + token.len();
                if !is_migration_name(token) || cites_a_live_migration(token, live) {
                    continue;
                }
                if !says_folded_away(&sentence[offset..]) {
                    out.push(token.to_owned());
                }
            }
        }
    }
    out
}

/// The paragraphs of a Markdown document, whitespace-normalised: a blank line
/// ends one. Every line is prose here, so there is no marker to strip.
fn markdown_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(
                    current
                        .join(" ")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    blocks
}

/// Every `.rs` file under `dir`.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    walk_extensions(dir, &["rs"], out);
}

/// Every file under `dir` whose extension is one of `extensions`.
///
/// `node_modules` and `generated` are never descended into. The first is
/// vendored code that cites nothing here and would multiply the file count by
/// two orders of magnitude; the second holds `openapi.d.ts`, which `npm run
/// generate:api` writes from `docs/openapi.json`, which in turn is written
/// from the Rust doc comments this guard already scans. A citation cannot
/// reach either derived file without passing through a `.rs` file first, so
/// scanning them would report the same defect twice and demand that a fix be
/// applied where it cannot be kept.
fn walk_extensions(dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("a source directory is readable") {
        let path = entry.expect("a readable dir entry").path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "node_modules" || n == "generated")
            {
                continue;
            }
            walk_extensions(&path, extensions, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| extensions.contains(&e))
        {
            out.push(path);
        }
    }
}

/// The four qa gears, each `<name>/<name>/src`. A migration a comment cites may
/// be any of theirs: `qa-insights` cites `qa-runs`' initial migration by name
/// for a column it denormalizes, and that is a live file, not a stale one.
const GEARS: [&str; 4] = ["qa-runs", "qa-insights", "qa-environments", "qa-catalog"];

/// Every live migration module across [`GEARS`].
fn live_migrations() -> BTreeSet<String> {
    let platform = subsystem_root();
    let mut files = Vec::new();
    for gear in GEARS {
        let src = platform.join(gear).join(gear).join("src");
        assert!(
            src.is_dir(),
            "{} is missing; the live set would be partial and the guard would report \
             another gear's real migrations as stale",
            src.display(),
        );
        walk(&src, &mut files);
    }
    files
        .iter()
        .filter(|p| {
            p.parent()
                .and_then(Path::file_name)
                .is_some_and(|d| d == "migrations")
        })
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .filter(|s| is_migration_name(s))
        .map(str::to_owned)
        .collect()
}

/// `gears/qa-platform`, derived from the including crate's own location.
fn subsystem_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("a gear sits two directories below gears/qa-platform")
        .to_path_buf()
}

/// This gear's `src/` and `tests/`, and its `<crate>-sdk` sibling's `src/` when
/// there is one: the SDK crates cite the gear's migrations too.
fn source_roots() -> Vec<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut roots = vec![manifest.join("src")];
    let tests = manifest.join("tests");
    if tests.is_dir() {
        roots.push(tests);
    }
    if let (Some(parent), Some(name)) = (manifest.parent(), manifest.file_name()) {
        let sdk = parent
            .join(format!("{}-sdk", name.to_string_lossy()))
            .join("src");
        if sdk.is_dir() {
            roots.push(sdk);
        }
    }
    roots
}

/// Every migration name this gear's comments cite is a live module, or says it
/// was folded away.
#[test]
fn every_migration_citation_is_live_or_marked() {
    let roots = source_roots();
    let mut files = Vec::new();
    for root in &roots {
        walk(root, &mut files);
    }
    let live = live_migrations();
    assert!(
        !live.is_empty(),
        "no live migration module was found, which cannot be right: the walk \
         or the directory name is wrong and this test would be vacuous",
    );
    let mut unmarked = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).expect("a source file is readable");
        for name in unmarked_migration_citations(&text, &live) {
            let file = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(&path)
                .display();
            unmarked.push(format!("{file}: `{name}`"));
        }
    }
    unmarked.sort();
    unmarked.dedup();
    assert!(
        unmarked.is_empty(),
        "comments cite a migration that does not exist and do not say, in the same \
         sentence and after the name, that it was folded away (write \"(folded into \
         `<live migration>` by the docs squash)\" right after the name):\n  {}",
        unmarked.join("\n  "),
    );
}

/// The repository root, from the including crate's own location.
fn repo_root() -> PathBuf {
    subsystem_root()
        .ancestors()
        .nth(2)
        .expect("gears/qa-platform sits two directories below the repository root")
        .to_path_buf()
}

/// What [`every_migration_citation_in_any_tracked_file_is_live_or_marked`]
/// lists with `git ls-files`, relative to the repository root: the whole
/// subsystem, the example server that links every gear, and the two
/// repository-level places that drive this subsystem's build and CI.
const TRACKED_PATHSPECS: [&str; 4] = [
    "gears/qa-platform",
    "apps/cf-gears-example-server",
    "Makefile",
    ".github",
];

/// Derived from files this guard already reads, so never scanned; see
/// [`walk_extensions`] for why a citation must be fixed at its source.
const DERIVED: [&str; 2] = [
    "gears/qa-platform/docs/openapi.json",
    "gears/qa-platform/qa-platform-ui/src/api/generated/",
];

/// Every **git-tracked** file under [`TRACKED_PATHSPECS`], minus [`DERIVED`].
///
/// Tracked files only, from `git ls-files`, rather than a filesystem walk: a
/// working tree carries untracked notes, a `.venv`, `__pycache__`, pytest
/// caches and `node_modules` under these roots, and a guard over what the
/// repository ships must not fail on a file that is not in it. CI's checkout
/// is a git work tree, so the listing is the same there. A `git` that cannot
/// run is a panic naming the cause, never an empty list the guard would pass
/// over.
fn tracked_files() -> Vec<PathBuf> {
    let repo = repo_root();
    let listing = std::process::Command::new("git")
        .args(["ls-files", "-z", "--"])
        .args(TRACKED_PATHSPECS)
        .current_dir(&repo)
        .output()
        .expect("`git` must be runnable: this guard lists tracked files with `git ls-files`");
    assert!(
        listing.status.success(),
        "`git ls-files` failed in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&listing.stderr),
    );
    listing
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| !DERIVED.iter().any(|d| p.starts_with(d)))
        .map(|p| repo.join(p))
        .collect()
}

/// The same rule over every tracked file of the subsystem, the example
/// server, the `Makefile` and `.github/`.
///
/// # How each file is read
///
/// Rust and TypeScript/JavaScript by their comments ([`comment_blocks`]), as
/// the per-gear test reads `src/`. Everything else — Markdown including
/// `docs/ADR/`, TOML, YAML, Helm templates, shell, Python, Dockerfiles, JSON
/// fixtures — whole, a blank line ending a paragraph: a migration name in a
/// YAML value, a shell `echo` or a Python string is as much a citation as one
/// in a comment, and a comment parser per language would be several readers
/// to keep right instead of one. A file that is not UTF-8 (an image) is
/// skipped.
///
/// # Why `docs/ADR/` is read
///
/// An ADR records a decision as of its date, and a dated document may name a
/// migration that has since been folded away, but it must then say so in the
/// same sentence, as every other document here does. No ADR named a migration
/// when this was written, so including them cost nothing and closed the one
/// exclusion that had no reason a reader could check.
///
/// # Why the `Makefile` and `.github/`
///
/// They drive this subsystem's build and CI and are where a migration name
/// would appear in a target or a step. They hold other subsystems' material
/// too, and the live set is the four qa gears' migrations, so a *qa-shaped*
/// name of another gear's migration there would be reported. None exists; if
/// one appears, narrow the pathspec to the qa targets instead of adding an
/// allowlist.
#[test]
fn every_migration_citation_in_any_tracked_file_is_live_or_marked() {
    let live = live_migrations();
    let repo = repo_root();
    let files = tracked_files();
    assert!(
        files.len() > 500,
        "only {} tracked files were listed; the pathspecs are wrong",
        files.len()
    );
    let mut unmarked = Vec::new();
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let by_comments = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "rs" | "ts" | "tsx" | "js"));
        let names = if by_comments {
            unmarked_migration_citations(&text, &live)
        } else {
            unmarked_in_blocks(markdown_blocks(&text), &live)
        };
        let rel = path.strip_prefix(&repo).unwrap_or(&path).display().to_string();
        for name in names {
            unmarked.push(format!("{rel}: `{name}`"));
        }
    }
    unmarked.sort();
    unmarked.dedup();
    assert!(
        unmarked.is_empty(),
        "a tracked file cites a migration that does not exist and does not say, in the same \
         sentence and after the name, that it was folded away (write \"(folded into \
         `<live migration>` by the docs squash)\" right after the name):\n  {}",
        unmarked.join("\n  "),
    );
}

/// The check catches the three-underscore case it was added for, and accepts a
/// marked or live name.
#[test]
fn the_migration_citation_check_sees_what_it_was_written_for() {
    let live = BTreeSet::from(["m20260813_000003_initial".to_owned()]);
    assert!(is_migration_name("m20260818_000006_collect_target"));
    assert!(!is_migration_name("collect_target_url_column"));
    let bare = "/// see `m20260818_000006_collect_target` for why.";
    assert_eq!(
        unmarked_migration_citations(bare, &live),
        ["m20260818_000006_collect_target"],
    );
    let marked = "/// `m20260818_000006_collect_target` (folded into\n/// `migrations::m20260813_000003_initial` by the docs squash) says why.";
    assert!(unmarked_migration_citations(marked, &live).is_empty());
    let alive = "// the `m20260813_000003_initial` migration";
    assert!(unmarked_migration_citations(alive, &live).is_empty());
}

/// **The second review's own misses.** A short name is a migration name; a
/// short name that prefixes a live module is live; and a `JSDoc` block is read
/// the same way a run of `///` lines is.
///
/// Each of these passed under the first version, which is why roughly twenty
/// citations of squashed migrations and both of the UI's went unseen.
#[test]
fn a_short_name_and_a_block_comment_are_both_read() {
    let live = BTreeSet::from(["m20260929_000003_seed_claims".to_owned()]);

    assert!(is_migration_name("m20260903_000004"));
    assert!(!is_migration_name("m20260903_00000"));
    assert!(!is_migration_name("m2026090_000004"));

    // The short spelling of a live module is that module, not a stale name.
    let short_live = "// filled by `m20260929_000003` on install.\n";
    assert!(unmarked_migration_citations(short_live, &live).is_empty());
    // Sharing a prefix is not enough; the next character must end the pair.
    let near = "// filled by `m20260929_00000` on install.\n";
    assert!(unmarked_migration_citations(near, &live).is_empty());

    // The short spelling of a dead one is reported.
    let short_dead = "// the column `m20260903_000012` dropped.\n";
    assert_eq!(
        unmarked_migration_citations(short_dead, &live),
        ["m20260903_000012"],
    );

    // A JSDoc block is one block, and a mark inside it excuses the name.
    let jsdoc = "/**\n * every product `m20260903_000003` backfilled\n * to VHP.\n */\n";
    assert_eq!(
        unmarked_migration_citations(jsdoc, &live),
        ["m20260903_000003"],
    );
    let jsdoc_marked =
        "/**\n * every product `m20260903_000003`, folded into\n * `m20260812_000002_initial` by the docs squash, backfilled.\n */\n";
    assert!(unmarked_migration_citations(jsdoc_marked, &live).is_empty());

    // And a one-line block comment is a block of its own.
    let one_line = "/* the column `m20260903_000012` dropped. */\n";
    assert_eq!(
        unmarked_migration_citations(one_line, &live),
        ["m20260903_000012"],
    );
}

/// **The exact cases the 300-character window used to pass.** A stale name close
/// to an unrelated, legitimately marked one must still be reported: after the
/// mark in the next sentence, before it in an earlier one, after it in the same
/// sentence, and in a separate comment block that merely sits nearby.
///
/// Each source is built on one line with `\n` rather than as a multi-line
/// literal, because a literal line that begins `///` would itself be read as a
/// comment by the guard walking this very file.
#[test]
fn a_stale_name_near_an_unrelated_mark_is_still_reported() {
    let live = BTreeSet::from(["m20260813_000003_initial".to_owned()]);
    let stale = "m20260101_000009_never_existed";
    let marked = "`m20260818_000006_collect_target` (folded into `m20260813_000003_initial` by the docs squash)";

    // Next sentence.
    let after = format!("/// {marked}.\n/// See also `{stale}` for the column default.\n");
    assert_eq!(unmarked_migration_citations(&after, &live), [stale]);

    // Previous sentence.
    let before = format!("/// The default lives in `{stale}`.\n/// {marked} says why.\n");
    assert_eq!(unmarked_migration_citations(&before, &live), [stale]);

    // Same sentence, after the mark: the mark precedes it and cannot excuse it.
    let same = format!("/// {marked} and also `{stale}`.\n");
    assert_eq!(unmarked_migration_citations(&same, &live), [stale]);

    // A different comment block a few lines away.
    let block = format!("// {marked}.\nfn a() {{}}\n// Column added by `{stale}`.\n");
    assert_eq!(unmarked_migration_citations(&block, &live), [stale]);

    // And the legitimate forms still pass.
    let ok = "/// `m20260818_000006_collect_target` and `m20260818_000005_case_fidelity`\n/// were folded into `m20260813_000003_initial`.\n";
    assert!(unmarked_migration_citations(ok, &live).is_empty());
}

/// **The fourth review's misses, named.** The whole-tree listing reaches a
/// gear's `tests/`, every gear and SDK `Cargo.toml`, the UI outside `src/`,
/// the repository `Makefile`, `.github/` and `docs/ADR/`, and leaves out the
/// two derived files. Each was outside every root the previous version listed.
#[test]
fn the_tracked_file_scan_reaches_every_root_the_fourth_pass_named() {
    let files: Vec<String> = tracked_files()
        .iter()
        .map(|p| {
            p.strip_prefix(repo_root())
                .expect("listed under the repository root")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let reaches = |want: &str| files.iter().any(|f| f == want || f.starts_with(want));
    for want in [
        "gears/qa-platform/qa-runs/qa-runs/tests/",
        "gears/qa-platform/qa-insights/qa-insights/tests/",
        "gears/qa-platform/qa-catalog/qa-catalog/tests/",
        "gears/qa-platform/qa-runs/qa-runs/Cargo.toml",
        "gears/qa-platform/qa-runs/qa-runs-sdk/Cargo.toml",
        "gears/qa-platform/qa-insights/qa-insights/Cargo.toml",
        "gears/qa-platform/qa-environments/qa-environments/Cargo.toml",
        "gears/qa-platform/qa-catalog/qa-catalog/Cargo.toml",
        "gears/qa-platform/plugins/qa-vhp-product-plugin/Cargo.toml",
        "gears/qa-platform/qa-platform-ui/package.json",
        "gears/qa-platform/qa-platform-ui/vite.config.ts",
        "gears/qa-platform/docs/ADR/",
        "Makefile",
        ".github/workflows/",
        "apps/cf-gears-example-server/",
    ] {
        assert!(reaches(want), "the tracked-file scan does not reach {want}");
    }
    for derived in DERIVED {
        assert!(
            !files.iter().any(|f| f.starts_with(derived)),
            "{derived} is derived and must not be scanned (see `walk_extensions`)"
        );
    }
}
