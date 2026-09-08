//! The one thing a third copy of [`super::emit`] owes the two it was copied
//! from.
//!
//! # Why a parity test and not a shared crate
//!
//! qa-runs, qa-insights and now qa-environments each carry the same panic guard
//! around a metric emission. Two copies were accepted on review; the third is
//! where "copy it again" stops being an answer, because the guard is not
//! decoration — it is what makes "metrics must not change behaviour" true at
//! the call site, and a fix applied to one copy silently leaves the other two
//! unfixed.
//!
//! A shared crate is the structural answer and is **out of this task's scope**:
//! the observability plan puts one there, and creating one would be a
//! cross-gear change of exactly the kind three tasks in this phase have already
//! been told not to make. So the coupling is asserted instead. This test is
//! cheaper than a crate, it fails loudly the moment the three diverge, and it
//! leaves the decision about a shared home to whoever takes it — with the
//! evidence that the three bodies are, today, one function.
//!
//! # Bodies, not files
//!
//! Phase 7's permission-catalog parity test hashes whole files. That shape does
//! **not** transfer here: each gear's `emit` doc argues from its own measured
//! paths — qa-insights' talks about one log line per open bug, this one about
//! one per registered environment — so the docs are deliberately different and
//! a file hash would fail on prose the day it was written. What must not
//! diverge is the executable part, so the executable part is what is compared.
//!
//! # What this test cannot do
//!
//! It reads two files in sibling gear directories, which means a *move* of
//! either file breaks this test rather than being caught by the compiler. That
//! is the price of asserting a cross-crate property from inside one crate, and
//! it is the safe direction: the failure names the path it could not read, and
//! the fix is one line. It is stated here rather than left for somebody to
//! discover.

use std::path::PathBuf;

/// The `domain/service/mod.rs` of each gear that carries a copy of the guard,
/// relative to this crate's manifest directory.
///
/// qa-environments' own path is included: comparing the two siblings to each
/// other while assuming this crate agrees with them is exactly the hole that
/// would let this crate be the one that drifted.
const GUARD_SITES: &[(&str, &str)] = &[
    ("qa-environments", "src/domain/service/mod.rs"),
    ("qa-runs", "../../qa-runs/qa-runs/src/domain/service/mod.rs"),
    (
        "qa-insights",
        "../../qa-insights/qa-insights/src/domain/service/mod.rs",
    ),
];

/// The signature line every copy opens with. Also the marker the body is cut
/// from, so a renamed function fails here rather than silently comparing
/// nothing.
const SIGNATURE: &str =
    "pub(in crate::domain::service) fn emit(silenced: &AtomicBool, record: impl FnOnce()) {";

/// The body of `emit` in one gear's `domain/service/mod.rs`, with each line
/// trimmed and blank lines dropped.
///
/// Trimmed rather than compared raw so that a rustfmt indentation change is not
/// reported as a behaviour change; everything that distinguishes two guards —
/// the ordering, the `catch_unwind`, the latch, the early return — survives
/// trimming.
fn guard_body(gear: &str, relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{gear}'s domain/service/mod.rs must be readable at {}: {error}. If that file \
             moved, this test's GUARD_SITES is what needs updating.",
            path.display()
        )
    });
    let after = source.split_once(SIGNATURE).unwrap_or_else(|| {
        panic!(
            "{gear} no longer declares the shared metric-emission guard with the signature \
             this parity test cuts on. If the signature changed, it changed in one gear and \
             not the others, which is the divergence this test exists to report."
        )
    });
    let (body, _) = after
        .1
        .split_once("\n}")
        .unwrap_or_else(|| panic!("{gear}'s emit body has no closing brace at column zero"));
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// **All three gears run the same guard.**
///
/// The property is not "these files look alike" — it is that a defect found in
/// one gear's guard is a defect in three, and that fixing it in one leaves two
/// live. Comparing the bodies is the cheapest thing that makes the second half
/// visible.
///
/// A guard that has genuinely diverged for a gear-specific reason is a
/// deliberate act, and the failure here is where it gets argued rather than
/// noticed six months later.
#[test]
fn the_guard_is_byte_for_byte_the_guard_the_sibling_gears_run() {
    let bodies: Vec<(&str, String)> = GUARD_SITES
        .iter()
        .map(|(gear, relative)| (*gear, guard_body(gear, relative)))
        .collect();

    let (first_gear, first) = &bodies[0];
    for (gear, body) in &bodies[1..] {
        assert_eq!(
            body, first,
            "{gear}'s metric-emission guard has diverged from {first_gear}'s. The three \
             gears carry one function in three files; a change to one of them is a change \
             the other two have not had."
        );
    }

    // A guard that cut nothing would compare two empty strings and pass, which
    // would make this test decoration in exactly the way the shared-crate
    // question is about. These are the four things every copy must contain.
    for marker in [
        "silenced.load(Ordering::Relaxed)",
        "return;",
        "catch_unwind",
        "silenced.store(true, Ordering::Relaxed)",
    ] {
        assert!(
            first.contains(marker),
            "the extracted guard body is missing `{marker}`, so this comparison is not \
             looking at the guard at all; body was:\n{first}"
        );
    }
}
