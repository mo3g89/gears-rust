use qa_product_sdk::observation::FailureClass;

use super::{SshFailure, classify};

#[test]
fn a_refused_connection_is_unreachable() {
    let failure = SshFailure::from_ssh_stderr(
        255,
        "ssh: connect to host 10.0.0.1 port 22: Connection refused",
    );
    assert_eq!(classify(&failure).class, FailureClass::Unreachable);
}

#[test]
fn an_unresolvable_host_is_unreachable() {
    let failure = SshFailure::from_ssh_stderr(
        255,
        "ssh: Could not resolve hostname nope: Name or service not known",
    );
    assert_eq!(classify(&failure).class, FailureClass::Unreachable);
}

#[test]
fn a_rejected_key_is_auth_rejected() {
    let failure = SshFailure::from_ssh_stderr(255, "root@10.0.0.1: Permission denied (publickey).");
    assert_eq!(classify(&failure).class, FailureClass::AuthRejected);
}

/// A non-zero exit whose stderr says nothing about permissions is the honest
/// `Internal`: something we expected to work did not, and the remote's own
/// words are what an operator has to go on.
#[test]
fn a_non_zero_remote_command_is_internal_and_carries_the_remote_text() {
    let failure = SshFailure::CommandFailed {
        status: 2,
        stderr: "vinfra: error: unable to reach the storage cluster".to_owned(),
    };
    let classified = classify(&failure);
    assert_eq!(classified.class, FailureClass::Internal);
    assert_eq!(
        classified.remote_message.as_deref(),
        Some("vinfra: error: unable to reach the storage cluster"),
        "a remote's own text is the one thing allowed to travel"
    );
}

/// **Reached and refused is `AuthRejected`, whichever connector saw it.**
///
/// `qa-connector-k8s` classifies a cluster's 401/403 that way; this is the
/// same situation one product later, and `FailureClass` is both an
/// environment-page label and a Prometheus label value, so the two must
/// agree. The literal is what the live stand produced (measured 2026-09-08)
/// and is exactly what a rejected `vinfra` login becomes.
#[test]
fn a_remote_command_that_refuses_our_credential_is_auth_rejected() {
    let failure = SshFailure::CommandFailed {
        status: 2,
        stderr: "vinfra: error: unauthorized".to_owned(),
    };
    let classified = classify(&failure);
    assert_eq!(classified.class, FailureClass::AuthRejected);
    assert_eq!(
        classified.remote_message.as_deref(),
        Some("vinfra: error: unauthorized"),
        "the remote's own text still travels, whichever class it landed in"
    );
    assert!(
        classified
            .detail
            .is_some_and(|detail| detail.contains("username and password")),
        "the operator must be sent to the credential that was actually refused, not to \
         their SSH key"
    );
}

/// The phrase match is case-insensitive and matches inside a longer line:
/// nothing guarantees a CLI lower-cases its own diagnostics.
#[test]
fn the_authorisation_phrases_are_matched_case_insensitively() {
    for stderr in [
        "ERROR: Unauthorized",
        "vinfra: error: Permission denied for user admin",
        "HTTP 403 Forbidden",
    ] {
        let classified = classify(&SshFailure::CommandFailed {
            status: 1,
            stderr: stderr.to_owned(),
        });
        assert_eq!(
            classified.class,
            FailureClass::AuthRejected,
            "`{stderr}` reads as a refusal"
        );
    }
}

/// A passphrase-protected key is `Malformed`, not `Internal`: the bytes were
/// read and are unusable as stored, which is exactly what
/// `qa_connector_k8s::classify_kubeconfig` calls `Malformed`.
#[test]
fn an_unusable_stored_key_is_malformed_the_way_an_unusable_kubeconfig_is() {
    assert_eq!(
        classify(&SshFailure::EncryptedKey).class,
        FailureClass::Malformed
    );
}

#[test]
fn a_timeout_is_a_timeout() {
    assert_eq!(classify(&SshFailure::Timeout).class, FailureClass::Timeout);
}

#[test]
fn an_encrypted_key_says_what_to_do() {
    let classified = classify(&SshFailure::EncryptedKey);
    assert!(
        classified.detail.is_some_and(|d| d.contains("passphrase")),
        "the operator has to be told which property of their key is the problem"
    );
}

#[test]
fn no_classified_failure_carries_agent_stderr() {
    // `ssh-add` can quote key material, so its stderr must never become a
    // message. `AgentSetup` therefore carries only a fixed stage name.
    let failure = SshFailure::AgentSetup {
        stage: "add the key to the agent",
    };
    let classified = classify(&failure);
    assert_eq!(classified.remote_message, None);
}
