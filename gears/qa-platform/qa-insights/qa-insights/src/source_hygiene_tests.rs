//! Source-level invariants that no compiler or clippy lint states.

use std::path::{Path, PathBuf};

fn rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).expect("readable dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target" || n == "node_modules") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Blank out comments and the contents of string/char literals (newlines kept,
/// so offsets and line numbers still line up), leaving only code.
fn mask_non_code(src: &[char]) -> Vec<char> {
    let mut out = src.to_vec();
    let blank = |out: &mut Vec<char>, from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != '\n' {
                *c = ' ';
            }
        }
    };
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let n = src.len();
    let mut i = 0;
    while i < n {
        let c = src[i];
        if c == '/' && src.get(i + 1) == Some(&'/') {
            let end = src[i..].iter().position(|&c| c == '\n').map_or(n, |p| i + p);
            blank(&mut out, i, end);
            i = end;
        } else if c == '/' && src.get(i + 1) == Some(&'*') {
            let (mut depth, mut j) = (1, i + 2);
            while j < n && depth > 0 {
                if src[j] == '/' && src.get(j + 1) == Some(&'*') {
                    depth += 1;
                    j += 2;
                } else if src[j] == '*' && src.get(j + 1) == Some(&'/') {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut out, i, j);
            i = j;
        } else if c == 'r' && (i == 0 || !is_ident(src[i - 1]) || src[i - 1] == 'b') && {
            let hashes = src[i + 1..].iter().take_while(|&&c| c == '#').count();
            src.get(i + 1 + hashes) == Some(&'"')
        } {
            let hashes = src[i + 1..].iter().take_while(|&&c| c == '#').count();
            let body = i + 2 + hashes;
            let mut j = body;
            while j < n && !(src[j] == '"' && src[j + 1..].iter().take(hashes).all(|&c| c == '#')
                && src[j + 1..].len() >= hashes)
            {
                j += 1;
            }
            let end = (j + 1 + hashes).min(n);
            blank(&mut out, body, j.min(n));
            i = end;
        } else if c == '"' {
            let mut j = i + 1;
            while j < n && src[j] != '"' {
                j += if src[j] == '\\' { 2 } else { 1 };
            }
            blank(&mut out, i + 1, j.min(n));
            i = j + 1;
        } else if c == '\'' {
            // A char literal, unlike a lifetime, closes within a few characters.
            let close = if src.get(i + 1) == Some(&'\\') {
                src[i + 2..].iter().take(10).position(|&c| c == '\'').map(|p| i + 2 + p)
            } else if src.get(i + 2) == Some(&'\'') {
                Some(i + 2)
            } else {
                None
            };
            if let Some(close) = close {
                blank(&mut out, i + 1, close);
                i = close + 1;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Whether `code[from..]`, after optional whitespace, is the keyword `word` as
/// a whole token (not the prefix of an identifier such as `awaited`).
fn follows_keyword(code: &[char], from: usize, word: &str) -> bool {
    let mut k = from;
    while code.get(k).is_some_and(|c| c.is_whitespace()) {
        k += 1;
    }
    let word: Vec<char> = word.chars().collect();
    code.get(k..k + word.len()) == Some(&word[..])
        && !code.get(k + word.len()).is_some_and(|c| c.is_alphanumeric() || *c == '_')
}

/// Line numbers (1-based) where a `debug_assert*!` call — `(…)`, `[…]` or `{…}` —
/// contains, in code, a `?`, a `.await` or a `&mut`, whatever the call's layout.
/// Comments and literals are not code.
fn offenders_in(text: &str) -> Vec<usize> {
    let src: Vec<char> = text.chars().collect();
    let code = mask_non_code(&src);
    let needle: Vec<char> = "debug_assert".chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i + needle.len() <= code.len() {
        if code[i..i + needle.len()] != needle[..] {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + needle.len();
        for suffix in ["_eq", "_ne"] {
            let s: Vec<char> = suffix.chars().collect();
            if code[j..].starts_with(&s) {
                j += s.len();
                break;
            }
        }
        if code.get(j) != Some(&'!') {
            i = j.max(i + 1);
            continue;
        }
        j += 1;
        while code.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        if !matches!(code.get(j), Some('(' | '[' | '{')) {
            i = j.max(i + 1);
            continue;
        }
        // Every bracket kind counts toward one depth: a macro call's delimiters
        // are balanced as a whole, and its argument may nest any of them.
        let (mut depth, mut offends) = (0_usize, false);
        while j < code.len() {
            match code[j] {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                '?' => offends = true,
                '.' if follows_keyword(&code, j + 1, "await") => offends = true,
                '&' if follows_keyword(&code, j + 1, "mut") => offends = true,
                _ => {}
            }
            j += 1;
        }
        if offends {
            found.push(code[..start].iter().filter(|&&c| c == '\n').count() + 1);
        }
        i = j.max(i + 1);
    }
    found
}

/// `debug_assert!` and friends expand to `if cfg!(debug_assertions) { … }`: in a
/// release build the whole argument list is never evaluated, so anything with
/// an effect inside it silently disappears — an early-return `?` (that shipped
/// once: a refused test send answered 200 in production), an `.await` that
/// drives a send or a write, a `&mut` borrow that mutates what the caller reads
/// next. All three delimiter forms of the macro are scanned.
///
/// **Residual:** interior mutability through a `&self` method (`lock`, `set`,
/// `fetch_add`, a channel `send` without `.await`) is not visible in the text
/// and is not caught; such a call inside a debug assertion is a review finding.
#[test]
fn no_debug_assertion_has_an_effect_release_builds_would_drop() {
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    rust_files(&bundle, &mut files);
    let mut offenders = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("utf-8 source");
        for line in offenders_in(&text) {
            offenders.push(format!("{}:{line}", file.display()));
        }
    }
    assert!(offenders.is_empty(), "`?`, `.await` or `&mut` inside a debug assertion:\n{}", offenders.join("\n"));
}

#[test]
fn the_scanner_flags_a_multi_line_call() {
    let src = "fn f() {\n    debug_assert_eq!(\n        result?,\n        SendOutcome::Sent\n    );\n}\n";
    assert_eq!(offenders_in(src), vec![2]);
}

#[test]
fn the_scanner_flags_a_single_line_call() {
    assert_eq!(offenders_in("debug_assert_eq!(r?, 1);"), vec![1]);
}

#[test]
fn the_scanner_ignores_a_question_mark_in_a_string_literal() {
    assert!(offenders_in(r#"debug_assert!(x, "is it ok?");"#).is_empty());
    assert!(offenders_in("debug_assert!(x, r#\"ok? \"quoted\"\"#);").is_empty());
}

#[test]
fn the_scanner_ignores_a_debug_format_specifier() {
    assert!(offenders_in(r#"debug_assert!(matches!(v, Some(_)), "{v:?}");"#).is_empty());
}

#[test]
fn the_scanner_ignores_a_commented_call() {
    assert!(offenders_in("// debug_assert!(a?)\n").is_empty());
    assert!(offenders_in("/* debug_assert!(a?) */\n").is_empty());
}

#[test]
fn the_scanner_flags_an_await() {
    assert_eq!(offenders_in("debug_assert!(client.send(&m).await.is_ok());"), vec![1]);
    assert_eq!(offenders_in("debug_assert!(f() . await);"), vec![1]);
}

#[test]
fn the_scanner_flags_a_mutable_borrow() {
    assert_eq!(offenders_in("debug_assert!(drain(&mut queue).is_empty());"), vec![1]);
}

#[test]
fn the_scanner_flags_brace_and_bracket_forms() {
    assert_eq!(offenders_in("debug_assert!{ r?.is_ok() };"), vec![1]);
    assert_eq!(offenders_in("debug_assert_eq![x.await, 1];"), vec![1]);
    assert_eq!(offenders_in("debug_assert_ne! {\n    r?,\n    0\n};"), vec![1]);
}

#[test]
fn the_scanner_scans_to_the_matching_delimiter_across_kinds() {
    // The `?` after the call is outside it and must not be blamed on it.
    assert!(offenders_in("debug_assert!(v[0] == {1});\nlet x = y?;").is_empty());
    // Nested closers of another kind do not end the call early.
    assert_eq!(offenders_in("debug_assert!({ let a = [1]; a[0] } == g()?);"), vec![1]);
}

#[test]
fn the_scanner_ignores_identifiers_that_only_contain_await() {
    assert!(offenders_in("debug_assert!(self.awaited && x.awaiting());").is_empty());
}

#[test]
fn the_scanner_ignores_a_shared_borrow_and_a_char_literal_delimiter() {
    assert!(offenders_in("debug_assert!(f(&queue) && c != '}' && d != ')');").is_empty());
}
