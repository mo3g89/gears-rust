//! The guard that keeps [`super::ENFORCED`] equal to what the code enforces.
//!
//! # Why a source scan
//!
//! [`super::ENFORCED`] is hand-written, so something has to catch the endpoint
//! that starts enforcing a pair nobody adds to it — an action a caller can be
//! refused and no role can grant. A runtime capture is not available: half the
//! call sites need a live `PDP` to reach, and the pair each one names is
//! decided at compile time anyway. So this scans the crate's own `src/` for
//! `access_scope` calls and checks both directions against the list.
//!
//! Shape copied from `qa-runs/src/file_citations_tests.rs`, this subsystem's
//! other structural source scan, including its rule about the source root: it
//! comes from `CARGO_MANIFEST_DIR`, never from a relative `"src/"`, because a
//! relative path depends on the directory the test binary was started in.
//!
//! # What it parses, and why that survives a reformat
//!
//! Nothing here matches on layout. Comments are blanked so the dozens of
//! places this subsystem's prose writes `access_scope(...)` in a doc comment
//! are invisible; an argument list is found by counting paren depth, so
//! rustfmt moving four arguments onto four lines changes nothing; and the
//! resource and action are read as `resources::*` / `actions::*` names
//! anywhere inside that list rather than as the second and third argument.
//!
//! The one thing it cannot read off a single call site is a *forwarded*
//! action, and most of this crate's call sites are one: each service compiles
//! its scopes through a private helper that takes `action: &str`. Those are
//! resolved through the helper's own callers, in the same file, transitively.
//! [`forwarded_actions`] carries the two shapes and why the resolution is
//! file-local.
//!
//! # Every misreading this scan can make is loud
//!
//! That is the property the whole guard rests on, so it is spelled out. Any
//! `resources::*` name it cannot resolve to a `&str` const, any const name
//! that resolves ambiguously, any argument list that does not close, and any
//! forwarded action it cannot trace **panic** rather than dropping a call site
//! from the measurement. Two further checks close the two ways a call site
//! could otherwise have gone missing quietly:
//! [`assert_every_access_scope_is_a_method_call`] pins the one spelling the
//! scan matches, and the forward test compares the number of call sites
//! reached against `super::EXPECTED_ACCESS_SCOPE_SITES` so a scan that
//! silently stops reading part of the crate fails instead of passing on a
//! smaller set.
//!
//! # This file is duplicated in all four gears
//!
//! Byte for byte, and `authz_surface_parity_tests.rs` in `qa-catalog` is what
//! detects a divergence. See that file's header for why the duplication is
//! correct and what to do when it fails.
//!
//! Review finding #1.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{ENFORCED, EXPECTED_ACCESS_SCOPE_SITES, RESOURCE_TYPES};

/// One `(resource_type, action)` pair the scan measured, and where from.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Measured {
    /// The `PDP` resource type string, as the enforcer was handed it.
    resource: String,
    /// The `PDP` action string.
    action: String,
    /// `src/path/to/file.rs:N` of the `access_scope` call.
    at: String,
}

/// One scanned file: where it is, and its source with comments blanked.
struct Source {
    /// Path relative to the crate root, for an assertion message.
    path: String,
    /// The file's bytes with every comment replaced by spaces.
    code: String,
}

/// This crate's own `src/`, resolved from the manifest.
fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every non-test `.rs` file under `src/`.
///
/// `*_tests.rs` and `test_support.rs` are skipped: those build `PDP` doubles
/// and call the enforcer with fixture pairs, which are not this gear's
/// enforcement surface. Inline `#[cfg(test)]` blocks inside production files
/// *are* scanned — no production file here has one that reaches the enforcer,
/// and one that did would either name a real pair (harmless) or invent one,
/// which is exactly what this guard should refuse to let pass.
fn sources() -> Vec<Source> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<Source>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                walk(&path, root, out);
            } else if is_scanned(&path) {
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let rel = path.strip_prefix(root).unwrap_or(&path);
                out.push(Source {
                    path: format!("src/{}", rel.to_string_lossy().replace('\\', "/")),
                    code: blank_comments(&text),
                });
            }
        }
    }

    let root = source_root();
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    assert!(
        out.len() > 10,
        "the walk of {} found {} source files, which cannot be right - a broken \
         walk would make this whole guard vacuous",
        root.display(),
        out.len(),
    );
    out
}

/// Is this a production `.rs` file this guard reads?
fn is_scanned(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "test_support.rs"
}

/// Is this byte part of a Rust identifier?
const fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// `text` with every comment blanked to spaces, byte offsets preserved.
///
/// Literals are stepped over rather than blanked, so a `//` inside a URL
/// cannot swallow the rest of its line, and a `const NAME: &str = "..."` value
/// is still readable afterwards. Newlines are kept so an offset still maps to
/// the line it was on.
fn blank_comments(text: &str) -> String {
    let raw = text.as_bytes();
    let mut out = raw.to_vec();
    let mut at = 0;
    while at < raw.len() {
        if let Some(end) = literal_end(raw, at) {
            at = end;
        } else if raw[at] == b'/' && raw.get(at + 1) == Some(&b'/') {
            at = blank_line_comment(raw, &mut out, at);
        } else if raw[at] == b'/' && raw.get(at + 1) == Some(&b'*') {
            at = blank_block_comment(raw, &mut out, at);
        } else {
            at += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// Blank a `//` comment; returns the offset of its newline.
fn blank_line_comment(raw: &[u8], out: &mut [u8], from: usize) -> usize {
    let mut at = from;
    while at < raw.len() && raw[at] != b'\n' {
        out[at] = b' ';
        at += 1;
    }
    at
}

/// Blank a (possibly nested) `/* */` comment; returns the offset after it.
fn blank_block_comment(raw: &[u8], out: &mut [u8], from: usize) -> usize {
    let mut at = from;
    let mut depth = 0_usize;
    while at < raw.len() {
        let pair = (raw[at], raw.get(at + 1).copied());
        if pair == (b'/', Some(b'*')) || pair == (b'*', Some(b'/')) {
            depth = if pair.0 == b'/' { depth + 1 } else { depth - 1 };
            out[at] = b' ';
            out[at + 1] = b' ';
            at += 2;
            if depth == 0 {
                return at;
            }
        } else {
            if raw[at] != b'\n' {
                out[at] = b' ';
            }
            at += 1;
        }
    }
    at
}

/// The offset just past the string, raw-string or char literal at `at`.
///
/// `None` when `at` starts none of those — including a lifetime, which opens
/// with the same byte as a char literal.
fn literal_end(raw: &[u8], at: usize) -> Option<usize> {
    match raw[at] {
        b'"' => Some(quoted_end(raw, at)),
        b'r' if !starts_inside_ident(raw, at) => raw_string_end(raw, at),
        b'\'' => char_literal_end(raw, at),
        _ => None,
    }
}

/// Is the byte before `at` part of an identifier?
fn starts_inside_ident(raw: &[u8], at: usize) -> bool {
    at.checked_sub(1)
        .is_some_and(|prev| is_ident_byte(raw[prev]))
}

/// The offset just past the `"`-delimited literal opening at `at`.
fn quoted_end(raw: &[u8], at: usize) -> usize {
    let mut cur = at + 1;
    while cur < raw.len() {
        match raw[cur] {
            b'\\' => cur += 2,
            b'"' => return cur + 1,
            _ => cur += 1,
        }
    }
    raw.len()
}

/// The offset just past the `r#"…"#` literal at `at`, or `None` if there is
/// none there.
fn raw_string_end(raw: &[u8], at: usize) -> Option<usize> {
    let mut hashes = 0;
    while raw.get(at + 1 + hashes) == Some(&b'#') {
        hashes += 1;
    }
    if raw.get(at + 1 + hashes) != Some(&b'"') {
        return None;
    }
    let mut cur = at + 2 + hashes;
    while cur < raw.len() {
        let closes = raw[cur] == b'"'
            && raw.len() >= cur + 1 + hashes
            && raw[cur + 1..cur + 1 + hashes]
                .iter()
                .all(|byte| *byte == b'#');
        if closes {
            return Some(cur + 1 + hashes);
        }
        cur += 1;
    }
    Some(raw.len())
}

/// The offset just past the char literal at `at`, or `None` for a lifetime.
fn char_literal_end(raw: &[u8], at: usize) -> Option<usize> {
    if raw.get(at + 1) == Some(&b'\\') {
        let mut cur = at + 2;
        while cur < raw.len() && raw[cur] != b'\'' {
            cur += 1;
        }
        return Some((cur + 1).min(raw.len()));
    }
    (raw.get(at + 2) == Some(&b'\'')).then_some(at + 3)
}

/// Byte offsets of every whole-token occurrence of `needle` in `code`.
///
/// "Whole token" means the byte before it is not part of an identifier, which
/// is what stops a search for `scope(` matching `access_scope(`.
fn token_positions(code: &str, needle: &str) -> Vec<usize> {
    let raw = code.as_bytes();
    code.match_indices(needle)
        .map(|(start, _)| start)
        .filter(|start| !starts_inside_ident(raw, *start))
        .collect()
}

/// The byte range of the argument text inside the call whose `(` is at `open`.
///
/// Paren depth is counted (stepping over literals) rather than pattern
/// matched, so how the arguments are wrapped across lines does not matter.
fn args_span(code: &str, open: usize) -> Option<(usize, usize)> {
    let raw = code.as_bytes();
    let mut depth = 0_usize;
    let mut at = open;
    while at < raw.len() {
        if let Some(end) = literal_end(raw, at) {
            at = end;
            continue;
        }
        if raw[at] == b'(' {
            depth += 1;
        } else if raw[at] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some((open + 1, at));
            }
        }
        at += 1;
    }
    None
}

/// Every `<module>::SCREAMING_NAME` the text names.
fn qualified(text: &str, module: &str) -> BTreeSet<String> {
    let needle = format!("{module}::");
    token_positions(text, &needle)
        .into_iter()
        .filter_map(|start| {
            let rest = text.get(start + needle.len()..)?;
            let name: String = rest
                .chars()
                .take_while(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || *ch == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// The leading identifier of `text`, and what follows it.
fn split_ident(text: &str) -> Option<(String, &str)> {
    let len = text.bytes().take_while(|byte| is_ident_byte(*byte)).count();
    (len > 0).then(|| (text[..len].to_owned(), &text[len..]))
}

/// Strip each token from the front of `text`, allowing whitespace between.
fn strip_tokens<'a>(text: &'a str, tokens: &[&str]) -> Option<&'a str> {
    let mut rest = text;
    for token in tokens {
        rest = rest.trim_start().strip_prefix(token)?;
    }
    Some(rest)
}

/// Every `const <NAME>: &str = "<value>";` this crate declares, as
/// name -> the set of values declared under that name.
///
/// Whitespace-tolerant rather than line-based, so a declaration rustfmt wraps
/// onto a second line still resolves.
///
/// **A set, not one value, because the key is a bare const name with no module
/// qualification and this crate already declares some names twice.** Keeping
/// every value and refusing the *lookup* of an ambiguous one
/// ([`one_value`]) is what makes a collision a named failure rather than a
/// silent last-file-in-walk-order win: a future `const GET: &str` in an
/// unrelated module would otherwise corrupt a measured action string. Refusing
/// duplicates outright is not available - qa-environments has two today
/// (`CANARY`, `STAMP`, both test-probe strings) and neither is a name this
/// scan ever consults.
fn str_consts(files: &[Source]) -> BTreeMap<String, BTreeSet<String>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in files {
        for start in token_positions(&file.code, "const ") {
            let Some((name, rest)) = file.code.get(start + 6..).and_then(split_ident) else {
                continue;
            };
            let Some(rest) = strip_tokens(rest, &[":", "&str", "=", "\""]) else {
                continue;
            };
            found
                .entry(name)
                .or_default()
                .insert(rest.chars().take_while(|ch| *ch != '"').collect());
        }
    }
    found
}

/// The single value declared under `name`, or a named failure.
///
/// # Panics
///
/// When the name is undeclared, or declared with more than one value - see
/// [`str_consts`] for why the second case has to be a panic and not a pick.
fn one_value(values: Option<&BTreeSet<String>>, what: &str, site: &str) -> String {
    let values = values.unwrap_or_else(|| {
        panic!("{what}, enforced at {site}, is not a `&str` const this scan can read")
    });
    assert_eq!(
        values.len(),
        1,
        "{what}, enforced at {site}, resolves to {} different `&str` consts of that name \
         ({}). The scan keys const names crate-wide and cannot tell them apart; qualify or \
         rename one of them.",
        values.len(),
        values
            .iter()
            .map(|value| format!("`{value}`"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    values.iter().next().cloned().unwrap_or_default()
}

/// Every `resources::*` descriptor, resolved to the `PDP` string it carries.
///
/// This is what the `*_NAME` consts beside each descriptor are for: the string
/// has one declaration and two consumers — the descriptor the enforcer is
/// called with, and [`super::ENFORCED`] — so this scan reads the very literal
/// the `PDP` was handed. `resources::TEST_RESULT_NAME` in
/// `qa-insights/src/domain/service/mod.rs:206` records why the descriptor
/// cannot supply it itself.
fn resource_strings(
    files: &[Source],
    strings: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let head = &[
        ":",
        "ResourceType",
        "=",
        "ResourceType",
        "::",
        "from_static",
        "(",
    ];
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in files {
        for start in token_positions(&file.code, "const ") {
            let Some((name, rest)) = file.code.get(start + 6..).and_then(split_ident) else {
                continue;
            };
            let Some((carrier, _)) = strip_tokens(rest, head)
                .map(str::trim_start)
                .and_then(split_ident)
            else {
                continue;
            };
            if let Some(values) = strings.get(&carrier) {
                found
                    .entry(name)
                    .or_default()
                    .extend(values.iter().cloned());
            }
        }
    }
    found
}

/// The 1-based line `offset` falls on.
fn line_of(code: &str, offset: usize) -> usize {
    code.get(..offset)
        .map_or(0, |head| head.matches('\n').count())
        + 1
}

/// The innermost `fn` whose text region contains `offset`, as `(name, range)`.
///
/// The region runs from one `fn` keyword to the next, which is the whole body
/// for every function this scan has to read: none of them declares a nested
/// `fn` between an `access_scope` call and its own end. Brace matching was the
/// alternative and buys nothing here.
fn enclosing_fn(code: &str, offset: usize) -> Option<(String, (usize, usize))> {
    let marks = token_positions(code, "fn ");
    let start = *marks.iter().rev().find(|mark| **mark < offset)?;
    let end = marks
        .iter()
        .find(|mark| **mark > offset)
        .copied()
        .unwrap_or(code.len());
    let (name, _) = code.get(start + 3..).and_then(split_ident)?;
    Some((name, (start, end)))
}

/// Byte offsets of the `(` of every call to `name`, skipping its declaration.
fn calls_to(code: &str, name: &str) -> Vec<usize> {
    token_positions(code, &format!("{name}("))
        .into_iter()
        .filter(|start| !declares_fn(code, *start))
        .map(|start| start + name.len())
        .collect()
}

/// Is the token at `start` the name in a `fn <name>(` declaration?
fn declares_fn(code: &str, start: usize) -> bool {
    let Some(head) = code.get(..start).map(str::trim_end) else {
        return false;
    };
    let Some(rest) = head.strip_suffix("fn") else {
        return false;
    };
    rest.as_bytes()
        .last()
        .is_none_or(|byte| !is_ident_byte(*byte))
}

/// The actions an `access_scope` call reaches when its action argument is a
/// forwarded name rather than an `actions::*` const.
///
/// Two shapes occur, tried in this order:
///
/// 1. **Decided in the same function.** qa-environments'
///    `VariablesService::upsert` picks `UPDATE` or `CREATE` from a natural-key
///    probe, so the enclosing function's own `actions::*` names are the answer.
/// 2. **A parameter of a per-service scope helper**, whose callers name the
///    const. Resolved through those callers, transitively, because a helper can
///    itself forward — `qa-runs`' `RunsService::read_run` takes an `action` and
///    hands it to `run_scope`.
///
/// The resolution is deliberately **file-local**. `scope` is the helper's name
/// in three different qa-insights services, and a crate-wide match on it would
/// hand `qa.saved_view` the notification settings' actions and vice versa —
/// merging surfaces that a deployment must be able to grant apart. A helper
/// whose callers move to another file makes this guard fail rather than guess,
/// which is the right direction for a gate.
fn forwarded_actions(file: &Source, offset: usize) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut resolved: BTreeSet<String> = BTreeSet::new();
    let mut pending = vec![offset];
    while let Some(at) = pending.pop() {
        let Some((name, region)) = enclosing_fn(&file.code, at) else {
            continue;
        };
        let Some(body) = file.code.get(region.0..region.1) else {
            continue;
        };
        let named = qualified(body, "actions");
        if !named.is_empty() {
            found.extend(named);
        } else if resolved.insert(name.clone()) {
            for call in calls_to(&file.code, &name) {
                match args_span(&file.code, call)
                    .and_then(|(from, to)| file.code.get(from..to))
                    .map(|args| qualified(args, "actions"))
                {
                    Some(inner) if !inner.is_empty() => found.extend(inner),
                    _ => pending.push(call),
                }
            }
        }
    }
    found
}

/// What one pass over this crate's source found.
struct Scan {
    /// Every `(resource_type, action)` pair reached, one row per call site.
    pairs: Vec<Measured>,
    /// How many `.access_scope(` call sites produced them.
    sites: usize,
}

/// Every `(resource_type, action)` pair an `access_scope` call site in this
/// crate's source reaches.
fn scan() -> Scan {
    let files = sources();
    assert_every_access_scope_is_a_method_call(&files);
    let strings = str_consts(&files);
    let types = resource_strings(&files, &strings);
    let mut found = Vec::new();
    let mut sites = 0;
    for file in &files {
        for call in file.code.match_indices(".access_scope(").map(|(at, _)| at) {
            sites += 1;
            let site = format!("{}:{}", file.path, line_of(&file.code, call));
            let args = call_arguments(file, call + ".access_scope".len(), &site);
            let resource = resource_at(&args, &types, &site);
            let mut actions = qualified(&args, "actions");
            if actions.is_empty() {
                actions = forwarded_actions(file, call + ".access_scope".len());
            }
            assert!(
                !actions.is_empty(),
                "could not resolve which action the access_scope call at {site} enforces: \
                 it names no actions::* const, its own function names none, and neither do \
                 that function's callers in the same file. Name the const at the call site, \
                 or teach this scan the new shape - do not leave the pair unmeasured."
            );
            for action in actions {
                found.push(Measured {
                    resource: resource.clone(),
                    action: action_string(&action, &strings, &site),
                    at: site.clone(),
                });
            }
        }
    }
    Scan {
        pairs: found,
        sites,
    }
}

/// **Every `access_scope` in this crate's code is spelled as a method call.**
///
/// The scan matches the one spelling `.access_scope(`, and that is the single
/// direction in which drift could be *silent*: a UFCS call
/// (`PolicyEnforcer::access_scope(&enforcer, ..)`), or a thin wrapper named
/// something else, would enforce a pair the forward test never sees while the
/// reverse test went on passing. Every other misreading this scan can make
/// ends in a panic. So the spelling itself is pinned here.
///
/// `access_scope_with` is caught by the same assertion, through the trailing
/// `(`: it is a *different* PEP entry point (it can turn off
/// `require_constraints`), no gear uses it today, and each gear's
/// `domain::service` header says so about itself. One appearing must break this
/// test rather than be counted as an `access_scope`.
///
/// # Panics
///
/// Naming the file, line and surrounding text of the offending spelling.
fn assert_every_access_scope_is_a_method_call(files: &[Source]) {
    let mut offenders: Vec<String> = Vec::new();
    for file in files {
        for at in file.code.match_indices("access_scope").map(|(at, _)| at) {
            let raw = file.code.as_bytes();
            let method = at.checked_sub(1).is_some_and(|prev| raw[prev] == b'.')
                && raw.get(at + "access_scope".len()) == Some(&b'(');
            if !method {
                let line = line_of(&file.code, at);
                let from = at.saturating_sub(40);
                let to = (at + 60).min(file.code.len());
                let context = file.code.get(from..to).unwrap_or_default().trim();
                offenders.push(format!("{}:{line}: {context}", file.path));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{} `access_scope` occurrence(s) are not spelled `.access_scope(`, so the scan \
         below cannot see them:\n  {}\n\nA UFCS call or an `access_scope_with` compiles a \
         scope this guard does not measure - the one way a new enforced pair can go \
         missing without any test failing. Spell it as a method call, or teach the scan \
         the new entry point; do not relax this assertion.",
        offenders.len(),
        offenders.join("\n  "),
    );
}

/// The argument text of the `access_scope` call whose `(` is at `open`.
///
/// # Panics
///
/// When the argument list does not close, which means this scan's paren
/// counting has lost track of the source rather than that the source is wrong.
fn call_arguments(file: &Source, open: usize, site: &str) -> String {
    let (from, to) = args_span(&file.code, open)
        .unwrap_or_else(|| panic!("the access_scope argument list at {site} does not close"));
    file.code.get(from..to).unwrap_or_default().to_owned()
}

/// The `PDP` resource string the call at `site` names.
///
/// # Panics
///
/// Unless the argument list names exactly one `resources::*` descriptor that
/// resolves to a string — one scope per resource type is the rule every
/// service module here states, and an unresolvable name means a descriptor
/// built some way this scan cannot read.
fn resource_at(args: &str, types: &BTreeMap<String, BTreeSet<String>>, site: &str) -> String {
    let named = qualified(args, "resources");
    assert_eq!(
        named.len(),
        1,
        "the access_scope call at {site} names {} resources::* consts; exactly one is \
         expected, because a scope is compiled for one resource type",
        named.len(),
    );
    let name = named.into_iter().next().unwrap_or_default();
    assert!(
        types.contains_key(&name),
        "resources::{name}, enforced at {site}, is not a `ResourceType::from_static` built \
         from a sibling `&str` const, so this scan cannot read the string the PDP is \
         handed. Declare a `{name}_NAME` const and build the descriptor from it."
    );
    one_value(types.get(&name), &format!("resources::{name}"), site)
}

/// The `PDP` action string for the `actions::<name>` const.
///
/// # Panics
///
/// When the const is not a `&str` declaration this scan could find.
fn action_string(name: &str, strings: &BTreeMap<String, BTreeSet<String>>, site: &str) -> String {
    one_value(strings.get(name), &format!("actions::{name}"), site)
}

/// Render measured pairs for an assertion message.
fn describe(pairs: &[&Measured]) -> String {
    let mut lines: Vec<String> = pairs
        .iter()
        .map(|pair| format!("{} / {} at {}", pair.resource, pair.action, pair.at))
        .collect();
    lines.sort();
    lines.dedup();
    lines.join("\n  ")
}

/// **Every `access_scope` call site is represented in [`super::ENFORCED`].**
///
/// A call site with no entry is an action a caller can be refused and that the
/// permission catalog - which is generated from that list - gives no role a way
/// to grant. Fails with the pair and the file and line that enforces it.
#[test]
fn every_access_scope_call_site_appears_in_the_enforced_list() {
    let measured = scan();
    assert_eq!(
        measured.sites, EXPECTED_ACCESS_SCOPE_SITES,
        "the scan reached {} `.access_scope(` call sites; this crate is recorded as having \
         {}. A scan that reaches fewer sites than the source has passes the check below \
         vacuously, which is why this is an equality and not a floor. If a call site was \
         genuinely added or removed, update EXPECTED_ACCESS_SCOPE_SITES in the sibling \
         module and re-derive ENFORCED; if it was not, the scan has stopped reading part \
         of this crate.",
        measured.sites, EXPECTED_ACCESS_SCOPE_SITES,
    );
    let missing: Vec<&Measured> = measured
        .pairs
        .iter()
        .filter(|pair| !ENFORCED.contains(&(pair.resource.as_str(), pair.action.as_str())))
        .collect();
    assert!(
        missing.is_empty(),
        "{} enforced (resource_type, action) pair(s) are missing from ENFORCED, and so \
         from this gear's permission catalog:\n  {}\n\nAdd them to ENFORCED and to the \
         catalog; a pair the PEP enforces that no permission names is an action no role \
         can grant.",
        missing.len(),
        describe(&missing),
    );
}

/// **Every entry in [`super::ENFORCED`] is reached by an `access_scope` call.**
///
/// The other direction, and the reason this guard cannot pass vacuously: an
/// entry no call site produces is a permission that authorizes nothing, which
/// is the same defect as a missing one seen from the catalog's side.
#[test]
fn every_enforced_pair_is_reached_by_an_access_scope_call_site() {
    let measured: BTreeSet<(String, String)> = scan()
        .pairs
        .into_iter()
        .map(|pair| (pair.resource, pair.action))
        .collect();
    let unreached: Vec<String> = ENFORCED
        .iter()
        .filter(|(resource, action)| {
            !measured.contains(&((*resource).to_owned(), (*action).to_owned()))
        })
        .map(|(resource, action)| format!("{resource} / {action}"))
        .collect();
    assert!(
        unreached.is_empty(),
        "{} entr(y/ies) in ENFORCED are reached by no access_scope call site:\n  {}\n\nEither \
         the call site was deleted - in which case drop the entry and the permission \
         generated from it - or this scan can no longer read it.",
        unreached.len(),
        unreached.join("\n  "),
    );
}

/// **[`super::RESOURCE_TYPES`] is exactly the distinct resource types in
/// [`super::ENFORCED`].**
///
/// It is consumed to register one type schema per resource type, and the `RBAC`
/// role-definition validator resolves a rule's `target_type` through that
/// registry - so a type missing here is a permission no role definition can
/// target, and one that is here but enforced nowhere is a schema for nothing.
#[test]
fn resource_types_lists_exactly_the_resource_types_in_enforced() {
    let distinct: BTreeSet<&str> = ENFORCED.iter().map(|(resource, _)| *resource).collect();
    let declared: BTreeSet<&str> = RESOURCE_TYPES.iter().copied().collect();
    assert_eq!(
        declared, distinct,
        "RESOURCE_TYPES and the resource types in ENFORCED disagree; RESOURCE_TYPES is \
         declared to be the distinct resource types of ENFORCED",
    );
    assert_eq!(
        declared.len(),
        RESOURCE_TYPES.len(),
        "RESOURCE_TYPES names a resource type twice",
    );
}

/// **[`super::ENFORCED`] declares each pair once.**
///
/// A duplicate would become a duplicate permission instance id in the catalog
/// generated from it.
#[test]
fn the_enforced_list_declares_each_pair_once() {
    let distinct: BTreeSet<(&str, &str)> = ENFORCED.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        ENFORCED.len(),
        "ENFORCED has {} entries but only {} distinct pairs",
        ENFORCED.len(),
        distinct.len(),
    );
}
