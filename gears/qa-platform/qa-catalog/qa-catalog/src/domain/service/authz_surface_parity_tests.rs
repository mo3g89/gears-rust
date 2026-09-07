//! **All four gears' `authz_surface_tests.rs` must be byte-identical.**
//!
//! # Why the duplication exists
//!
//! Each of the four QA Platform gears is its own crate, and each needs the
//! `access_scope` source scan to run against *its own* `src/` - the guard's
//! whole job is to catch a call site this crate added without an `ENFORCED`
//! entry, so a single copy in one crate would leave the other three
//! unguarded. Sharing the code would mean a new shared dev-dependency crate,
//! and §12 of the review-remediation spec rules cross-gear lifting out of
//! scope for this work.
//!
//! The subsystem already settled this trade once, the same way:
//! `no_api_in_domain_tests.rs` is duplicated across these same four crates and
//! its header defends it in the same terms - a gate installed in one crate
//! leaves the other three unguarded.
//!
//! # Why this test exists
//!
//! Because the cost of that trade is a silent one. A fix to
//! [`blank_comments`](super::tests) or to the forwarded-action resolution,
//! applied in the gear whose test was failing and nowhere else, would leave
//! three anti-drift gates weaker than the one that was fixed, and nothing in
//! the repository would notice. This test is that detector, and it is the only
//! copy: hosting it in each gear would mean four files that differ from each
//! other by construction, which is exactly what it is asserting cannot happen.
//!
//! # What to do when it fails
//!
//! **Port the change to all four copies.** Do not adjust this test, and do not
//! declare one gear's copy the special one - the four gears' enforcement
//! surfaces differ (that is what `authz_surface.rs` is for) but the *scanner*
//! that measures them does not.
//!
//! If a gear directory has moved, this test fails on the unresolved path
//! rather than skipping it. That is deliberate: a moved gear is precisely the
//! change that must not disarm the guard quietly.
//!
//! Review finding #1, fix round 1.

use std::fs;
use std::path::{Path, PathBuf};

/// The four gears whose copies must agree, in the order they are reported.
const GEARS: &[&str] = &["qa-catalog", "qa-environments", "qa-insights", "qa-runs"];

/// The scanner's path inside one gear's crate.
const SCANNER: &str = "src/domain/service/authz_surface_tests.rs";

/// `gears/qa-platform`, derived from this crate's own location.
///
/// From `CARGO_MANIFEST_DIR`, as the scanner itself resolves its source root
/// and for the same reason: a relative path would depend on the directory the
/// test binary was started in.
fn subsystem_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("this crate sits two levels under gears/qa-platform"));
    assert!(
        root.join("qa-catalog").is_dir(),
        "{} is not gears/qa-platform - it has no qa-catalog directory. This crate's \
         location relative to the subsystem root has changed; fix the derivation rather \
         than skipping the check.",
        root.display(),
    );
    root.to_owned()
}

/// A stable fingerprint of `bytes`, for the failure message.
///
/// FNV-1a rather than `DefaultHasher`, so two runs and two gears print
/// comparable numbers. The *verdict* is byte equality, not this - a hash is
/// only what makes the message readable.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The 1-based line at which `left` and `right` first differ.
fn first_difference(left: &[u8], right: &[u8]) -> usize {
    let common = left
        .iter()
        .zip(right.iter())
        .take_while(|(one, two)| one == two)
        .count();
    left.iter()
        .take(common)
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

/// **The four copies of the `access_scope` scanner are byte-identical.**
///
/// Fails naming which gears diverged, each one's length and fingerprint, and
/// the line the difference starts on. See this module's header for why the
/// answer is to port the change rather than to re-baseline.
#[test]
fn all_four_gears_carry_the_same_authz_surface_scanner() {
    let root = subsystem_root();
    let copies: Vec<(&str, Vec<u8>)> = GEARS
        .iter()
        .map(|gear| {
            let path = root.join(gear).join(gear).join(SCANNER);
            let bytes = fs::read(&path).unwrap_or_else(|error| {
                panic!(
                    "cannot read {}: {error}. Every gear must carry this scanner; a missing \
                     or moved copy is a gear whose enforced-surface gate is gone, not a \
                     case for this test to skip.",
                    path.display(),
                )
            });
            assert!(
                bytes.len() > 1_000,
                "{} is {} bytes, which cannot be the scanner - a truncated or stub copy \
                 would make that gear's anti-drift gate vacuous",
                path.display(),
                bytes.len(),
            );
            (*gear, bytes)
        })
        .collect();

    let (reference_gear, reference) = copies
        .first()
        .unwrap_or_else(|| panic!("GEARS is empty; there is nothing to compare"));

    let diverged: Vec<String> = copies
        .iter()
        .skip(1)
        .filter(|(_, bytes)| bytes != reference)
        .map(|(gear, bytes)| {
            format!(
                "{gear}: {} bytes, fingerprint {:#018x}, first differs at line {}",
                bytes.len(),
                fingerprint(bytes),
                first_difference(reference, bytes),
            )
        })
        .collect();

    assert!(
        diverged.is_empty(),
        "{} of the four gears' copies of {SCANNER} differ from {reference_gear}'s \
         ({} bytes, fingerprint {:#018x}):\n  {}\n\nThe four copies are deliberately \
         identical: each gear scans its own src/, so all four need the scanner, and there \
         is no shared test crate to hold one copy. A fix applied to one and not the others \
         leaves three anti-drift gates weaker than the fixed one. Port the change to all \
         four; do not re-baseline this test.",
        diverged.len(),
        reference.len(),
        fingerprint(reference),
        diverged.join("\n  "),
    );
}
