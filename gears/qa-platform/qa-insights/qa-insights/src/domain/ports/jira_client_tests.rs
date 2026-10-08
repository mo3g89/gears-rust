//! `validate_credstore_ref` accepts exactly what the credential store accepts.

use super::validate_credstore_ref;

/// **One syntax across the platform.** qa-environments and qa-catalog refuse
/// a reference by `credstore_sdk::SecretRef::new` itself; this gear used to
/// accept a `cred://` prefix on top and strip it. Each input is checked against
/// the credential store's own constructor, not against a hand-copied
/// expectation, so the two cannot drift.
#[test]
fn a_reference_is_valid_exactly_when_the_credential_store_accepts_it() {
    let long_ok = "a".repeat(255);
    let long_bad = "a".repeat(256);
    let cases: [&str; 11] = [
        "qa-jira-api-token",
        "kc_staging-2",
        "cred://qa-jira-api-token",
        "cred://",
        "credstore://qa/jira/api-token",
        "qa/jira/api-token",
        "qa:jira:token",
        "has spaces",
        "\u{fc}n\u{ef}code",
        &long_ok,
        &long_bad,
    ];
    for reference in cases {
        let ours = validate_credstore_ref("f", reference).is_ok();
        let store = credstore_sdk::SecretRef::new(reference).is_ok();
        assert_eq!(
            ours, store,
            "`{reference}`: this gear says {ours}, the credential store says {store}"
        );
    }
    assert!(
        validate_credstore_ref("f", "").is_err(),
        "an empty reference is refused, as `SecretRef::new` refuses it"
    );
}
