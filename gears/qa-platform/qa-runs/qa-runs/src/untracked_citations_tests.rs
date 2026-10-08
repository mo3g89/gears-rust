//! No tracked file cites a document the repository does not hold.
//!
//! # Why it exists
//!
//! This subsystem was built against planning material — a parity spec, a
//! controller's rulings, phase rulings, an owner's decision register — that
//! lives in an untracked working directory. A comment that says "parity spec
//! §3.4 rule 2" or "ruling F-13" sends a reader to a document they cannot
//! open, and the labels are not even unique: two different `D2`s and two
//! different `D5`s were cited. A citation names a tracked document's section by
//! number and heading, or an item in the code, or it states the fact.
//!
//! # What it reads
//!
//! Every git-tracked text file under [`SCANNED`], as paragraphs with leading
//! comment markers stripped, so a citation wrapped across two comment lines
//! (`(parity spec` / `/// §3.4 rule 3)`) is still one phrase. Code lines are
//! read too: a test's assertion message carries labels as often as a doc does.
//!
//! # What counts
//!
//! `spec §`, `spec (§` and `spec section <n>` (in either case), `design §`,
//! `design's §` and `design (§` (lower case or capitalised, never `DESIGN §`,
//! which is the tracked `docs/DESIGN.md`), `design <n>.<n>` (any case but
//! `DESIGN`), `§<n> of the … spec`, `review remediation §`,
//! `release-gate item`, and a label — `R<n>` or `D<n>` (one to three digits,
//! optionally followed by one lowercase letter, `R94a`), `U<n>` (one digit),
//! `D-<n>`, `E-<n>`, `F-<n>` — as a whole word or as one hyphen-separated piece
//! of a word (`R3-4`), unless it follows `#` (a hex colour such as `#D00`); a
//! three-part label `<L>-<LETTERS>-<n>` (`D-CH-3`, `D-RLP-6`: a one-letter
//! register prefix, one to four capitals, one to three digits), which spares
//! `UTF-8`, `SHA-256`, `x86-64`, `AES-256-GCM`, `ISO-8859-1`, a ticket key such
//! as `REPO-A-1` and a review's finding id such as `NEW-I-1`; and
//! `ruling <X>` (in either case) where `<X>` is one capital letter, optionally
//! followed by a number (`Ruling C`, `ruling G-4`). A label is allowed only
//! when a tracked document under `gears/qa-platform/docs/` says "labelled
//! `<label>`", which ADR-0001 and ADR-0008 do for `D4`. The allowed set is read
//! on every run, not written down here.
//!
//! # What it does not check
//!
//! `Task <n>`, `fix round <n>`, `finding #<n>` and a review's own finding ids
//! (`finding I-9`, `Critical C-1`) name planning steps whose outcome the
//! sentence states anyway, and `guide lines <n>-<m>` and `manager/src/…` name
//! the legacy system's own tree. Both are left out on purpose and are not this
//! guard's class.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Pathspecs, relative to the repository root.
const SCANNED: [&str; 2] = ["gears/qa-platform", "apps/cf-gears-example-server"];

/// Derived or vendored files, where a citation cannot be fixed, and this file,
/// which names the patterns it looks for.
const SKIPPED: [&str; 4] = [
    "gears/qa-platform/docs/openapi.json",
    "gears/qa-platform/qa-platform-ui/src/api/generated/",
    "gears/qa-platform/qa-platform-ui/package-lock.json",
    "gears/qa-platform/qa-runs/qa-runs/src/untracked_citations_tests.rs",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("qa-runs/qa-runs sits four directories below the repository root")
        .to_path_buf()
}

/// Tracked paths under [`SCANNED`], repository-relative, minus [`SKIPPED`].
/// A `git` that cannot run is a panic naming the cause, never an empty list.
fn tracked(repo: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["ls-files", "-z", "--"])
        .args(SCANNED)
        .current_dir(repo)
        .output()
        .expect("`git` must be runnable: this guard lists tracked files with `git ls-files`");
    assert!(
        out.status.success(),
        "`git ls-files` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| !SKIPPED.iter().any(|s| p.starts_with(s)))
        .collect()
}

/// Paragraphs: each line with its leading comment markers stripped, a line
/// that is empty afterwards ends a paragraph, whitespace collapsed.
fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        let body = line
            .trim_start()
            .trim_start_matches(['/', '!', '*', '#', '-', '{', '}'])
            .trim();
        if body.is_empty() {
            if !current.is_empty() {
                out.push(
                    current
                        .join(" ")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                current.clear();
            }
        } else {
            current.push(body);
        }
    }
    out
}

/// `R86`, `R94a`, `D4`, `U4`, `D-18`, `E-17`, `F-13`.
fn is_label(word: &str) -> bool {
    let digits =
        |s: &str, max: usize| (1..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    let Some(head) = word.chars().next() else {
        return false;
    };
    let rest = &word[head.len_utf8()..];
    // `R94a`: a numbered label with one lowercase letter after it.
    let lettered = |s: &str| {
        s.strip_suffix(|c: char| c.is_ascii_lowercase())
            .is_some_and(|d| digits(d, 3))
    };
    match head {
        'R' => digits(rest, 3) || lettered(rest),
        'D' => {
            digits(rest, 3)
                || lettered(rest)
                || rest.strip_prefix('-').is_some_and(|r| digits(r, 3))
        }
        'U' => digits(rest, 1),
        'E' | 'F' => rest.strip_prefix('-').is_some_and(|r| digits(r, 3)),
        _ => false,
    }
}

/// `D-CH-3`, `D-RLP-6`: a one-letter register prefix, capitals, digits,
/// hyphen-separated — the shape of the cluster-health and run-log decision
/// registers. The prefix is one letter for the same reason every other label
/// form here starts with one (`R86`, `D-18`, `E-17`): that is what a register
/// prefix is, and it is what tells the shape from a ticket key (`REPO-A-1`)
/// or a review's finding id (`NEW-I-1`). `UTF-8`, `SHA-256`, `x86-64`,
/// `AES-256-GCM` and `ISO-8859-1` fail on part count, case or digits where
/// the middle part's capitals belong.
fn is_multipart_label(word: &str) -> bool {
    let parts: Vec<&str> = word.split('-').collect();
    let capitals =
        |s: &str, max: usize| (1..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase());
    let digits = |s: &str| (1..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    parts.len() == 3 && capitals(parts[0], 1) && capitals(parts[1], 4) && digits(parts[2])
}

/// `4.8`, `6.3`: two or more dot-separated numbers.
fn is_section_number(word: &str) -> bool {
    let parts: Vec<&str> = word.split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `C`, `G-4`: one capital letter, optionally followed by a number.
fn is_ruling_name(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(head) = chars.next() else {
        return false;
    };
    let rest = chars.as_str();
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    head.is_ascii_uppercase()
        && rest.len() <= 3
        && rest.bytes().all(|b| b.is_ascii_digit())
        && !(word.len() > 1 && rest.is_empty())
}

/// The phantom citations in one paragraph, each as the text that matched.
fn phantom_citations(paragraph: &str, defined: &BTreeSet<String>) -> Vec<String> {
    let mut hits = Vec::new();
    // Case-insensitive: a sentence may open with "Spec §5.3".
    let lower = paragraph.to_lowercase();
    for phrase in ["spec \u{a7}", "spec (\u{a7}", "release-gate item"] {
        if lower.contains(phrase) {
            hits.push(phrase.to_owned());
        }
    }
    // "design §4.4", "the design's §8", "Design §6": the subsystem's working
    // design, which is untracked. Case-sensitive on purpose: the tracked one
    // is cited as `DESIGN §<n>`.
    if ["design", "Design"].iter().any(|d| {
        [" \u{a7}", "'s \u{a7}", " (\u{a7}"]
            .iter()
            .any(|tail| paragraph.contains(&format!("{d}{tail}")))
    }) {
        hits.push("design \u{a7}".to_owned());
    }
    // "spec section 8": the same citation, spelled out.
    if lower.match_indices("spec section ").any(|(at, m)| {
        lower[at + m.len()..]
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_digit())
    }) {
        hits.push("spec section".to_owned());
    }
    // "review remediation §5.6.2": the remediation spec, named by its title.
    if ["review remediation \u{a7}", "review-remediation \u{a7}"]
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        hits.push("review remediation \u{a7}".to_owned());
    }
    let words: Vec<&str> = paragraph.split_whitespace().collect();
    let bare = |w: &str| {
        w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '\u{a7}')
            .to_owned()
    };
    for (at, word) in words.iter().enumerate() {
        let word = bare(word);
        // "design 4.8", "Design 6.3": the untracked working design by section
        // number. `DESIGN 3.11` is the tracked `docs/DESIGN.md`.
        if word.eq_ignore_ascii_case("design")
            && word != "DESIGN"
            && words
                .get(at + 1)
                .is_some_and(|next| is_section_number(&bare(next)))
        {
            hits.push("design <n>.<n>".to_owned());
        }
        // "§12 of the review-remediation spec": a section of some spec.
        if word
            .strip_prefix('\u{a7}')
            .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()))
            && words.get(at + 1).is_some_and(|w| bare(w) == "of")
            && words
                .iter()
                .skip(at + 2)
                .take(4)
                .any(|w| bare(w).to_lowercase().ends_with("spec"))
        {
            hits.push("\u{a7} of the spec".to_owned());
        }
    }
    // "Ruling C", "ruling G-4": a ruling named by a label the forms below do
    // not cover. `is_label` labels are left to the loop below.
    for pair in words.windows(2) {
        let next = pair[1]
            .trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
            .trim_end_matches("'s")
            .trim_end_matches('\u{2019}');
        if pair[0]
            .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
            .eq_ignore_ascii_case("ruling")
            && is_ruling_name(next)
            && !is_label(next)
        {
            hits.push(format!("ruling {next}"));
        }
    }
    for word in paragraph.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        // A word right after `#` is a hex colour (`#D00`) or an anchor, not a
        // label. `word` is a slice of `paragraph`, so its offset is exact.
        let at = word.as_ptr() as usize - paragraph.as_ptr() as usize;
        if paragraph[..at].ends_with('#') {
            continue;
        }
        // `D-CH-3`: the whole word is the label; its pieces (`D`, `CH`, `3`)
        // are not, which is how this shape slipped past the loop below.
        if is_multipart_label(word) && !defined.contains(word) {
            hits.push(word.to_owned());
            continue;
        }
        let candidates = std::iter::once(word).chain(word.split('-'));
        for candidate in candidates {
            if is_label(candidate) && !defined.contains(candidate) {
                hits.push(candidate.to_owned());
                break;
            }
        }
    }
    hits
}

/// Labels a tracked document defines: every "labelled `<label>`" under
/// `gears/qa-platform/docs/`.
fn defined_labels(repo: &Path, files: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for rel in files
        .iter()
        .filter(|p| p.starts_with("gears/qa-platform/docs/"))
    {
        let Ok(text) = fs::read_to_string(repo.join(rel)) else {
            continue;
        };
        for (at, marker) in text.match_indices("labelled `") {
            if let Some((label, _)) = text[at + marker.len()..].split_once('`') {
                out.insert(label.to_owned());
            }
        }
    }
    out
}

#[test]
fn no_tracked_file_cites_a_document_the_repository_does_not_hold() {
    let repo = repo_root();
    let files = tracked(&repo);
    assert!(
        files.len() > 500,
        "only {} tracked files under {SCANNED:?}; the listing is wrong",
        files.len()
    );
    let defined = defined_labels(&repo, &files);
    assert!(
        defined.contains("D4"),
        "ADR-0001 says the runner-Secret writer is labelled `D4`; reading docs/ found \
         {defined:?}, so the reader is wrong"
    );
    let mut offenders = Vec::new();
    for rel in &files {
        let Ok(text) = fs::read_to_string(repo.join(rel)) else {
            continue;
        };
        for paragraph in paragraphs(&text) {
            for hit in phantom_citations(&paragraph, &defined) {
                offenders.push(format!("{rel}: `{hit}`"));
            }
        }
    }
    let found: BTreeSet<String> = offenders.into_iter().collect();
    assert!(
        found.is_empty(),
        "{} citation(s) of a document this repository does not hold. Cite a tracked \
         document's section by number and heading, or an item in the code, or state the \
         fact:\n  {}",
        found.len(),
        found
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// The matcher over a snippet, with `D4` defined the way the docs define it.
fn hits(s: &str) -> Vec<String> {
    let defined = BTreeSet::from(["D4".to_owned()]);
    paragraphs(s)
        .iter()
        .flat_map(|p| phantom_citations(p, &defined))
        .collect()
}

#[test]
fn the_matcher_sees_each_form_and_spares_the_lookalikes() {
    assert_eq!(
        hits("/// (parity spec\n/// \u{a7}3.4 rule 3)"),
        ["spec \u{a7}"]
    );
    assert_eq!(
        hits("# (spec \u{a7}5.2, ruling D-19)"),
        ["spec \u{a7}", "D-19"]
    );
    assert_eq!(hits("// ruling F-13 and E-17"), ["F-13", "E-17"]);
    assert_eq!(hits("/// # Controller ruling R86 \u{2014} tenant"), ["R86"]);
    assert_eq!(hits("// re-review finding R3-4"), ["R3"]);
    assert_eq!(hits("// user decision U4"), ["U4"]);
    assert_eq!(
        hits("//! a tracked release-gate item"),
        ["release-gate item"]
    );
    assert_eq!(hits("/// Spec \u{a7}5.3 records it"), ["spec \u{a7}"]);
    assert_eq!(hits("/// and the spec (\u{a7}4.6) said"), ["spec (\u{a7}"]);
    assert_eq!(
        hits("// the cluster-health spec section 8 says"),
        ["spec section"]
    );
    assert_eq!(hits("// Cluster-health Spec Section 8"), ["spec section"]);
    assert!(hits("// the spec section that follows").is_empty());
    assert!(hits("// decision D4's runner-Secret writer").is_empty());
    assert!(
        hits(
            "AggregatedMetrics::U64(MetricData::Sum(s)); F64; UTF-8; E2E; x86-64; SHA-256; \
             #D4D4D4; #D00; color: #D000"
        )
        .is_empty()
    );
}

#[test]
fn the_matcher_sees_lettered_labels_named_rulings_and_the_untracked_design() {
    assert_eq!(hits("// audited or silent (R94a)"), ["R94a"]);
    assert_eq!(hits("/// until controller Ruling C's change"), ["ruling C"]);
    assert_eq!(hits("// closes it (ruling G-4)."), ["ruling G-4"]);
    assert_eq!(
        hits("/// prevent (design \u{a7}4.4, Task 15)."),
        ["design \u{a7}"]
    );
    assert_eq!(
        hits("// the design's \u{a7}8 records that"),
        ["design \u{a7}"]
    );
    assert!(hits("/// DESIGN \u{a7}3.3, \"Reads do not serialize\"").is_empty());
}

#[test]
fn the_matcher_sees_a_design_or_spec_section_by_number_and_spares_the_tracked_design() {
    assert_eq!(
        hits("-- never propagated to a caller (design 4.8), which"),
        ["design <n>.<n>"]
    );
    assert_eq!(hits("// Design 6.3 on this tier"), ["design <n>.<n>"]);
    assert_eq!(
        hits("//! and \u{a7}12 of the review-remediation spec rules it out"),
        ["\u{a7} of the spec"]
    );
    assert_eq!(
        hits("*(Added 2026-09-18, QA Platform review remediation \u{a7}5.6.2.)*"),
        ["review remediation \u{a7}"]
    );
    assert!(hits("// DESIGN 3.11's table, and DESIGN 1.2").is_empty());
    assert!(hits("// a design 4 times over; the design. 3 more").is_empty());
    assert!(hits("// \u{a7}2 of the ADR above").is_empty());
    assert!(hits("// the cost of ruling it out, and ruling R-less prose").is_empty());
}

#[test]
fn the_matcher_sees_a_three_part_label_and_spares_the_encodings_and_architectures() {
    assert_eq!(hits("/// outcomes (D-CH-3)."), ["D-CH-3"]);
    assert_eq!(hits("//! established (**D-CH-5**)."), ["D-CH-5"]);
    assert_eq!(hits("/// was D-CH-3's own rule"), ["D-CH-3"]);
    assert_eq!(hits("/// retention (spec D-RLP-1) and"), ["D-RLP-1"]);
    assert_eq!(
        hits("step \"12: the leak canary (X-YZ-12)\""),
        ["X-YZ-12"]
    );
    assert_eq!(hits(" * fallback matters (D-CH-6)."), ["D-CH-6"]);
    assert!(
        hits(
            "// UTF-8, x86-64, SHA-256, E2E, AES-256-GCM, ISO-8859-1, TLS-1.3, D-CH-, CH-3, \
             D-CHECK-ME, D-CHECKS-1, e2e-k8s-1, #D-CH-3, a ticket REPO-A-1, finding NEW-I-1"
        )
        .is_empty()
    );
}
