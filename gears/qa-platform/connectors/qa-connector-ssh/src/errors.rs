//! What can go wrong reaching a host over SSH, and how it becomes a
//! [`PluginFailure`].
//!
//! # Two consumers, two shapes
//!
//! `qa-catalog`'s git sync wants a message it can put in
//! `DomainError::SyncFailed`; a product plugin wants a classified
//! [`PluginFailure`] whose `detail` is `&'static str` and therefore cannot be
//! built from runtime bytes. [`SshFailure`] serves both: `Display` for the
//! first, [`classify`] for the second.
//!
//! # What may never travel
//!
//! `ssh-add`'s stderr. It can quote the key it was asked to add, so
//! [`SshFailure::AgentSetup`] carries a fixed stage name and nothing else.
//! `ssh`'s own connect diagnostic and a remote command's stderr are both safe
//! -- neither has ever seen the private key -- and both travel.
//!
//! # The taxonomy agrees with `qa-connector-k8s`, deliberately
//!
//! [`qa_product_sdk::observation::FailureClass`] is not private vocabulary:
//! it is a label on the environment page *and* a Prometheus label value, so
//! the same condition reaching the platform through two different connectors
//! must classify the same way or an operator's dashboard counts one situation
//! twice under two names. Three cases were corrected on 2026-09-09, each to
//! match what `qa_connector_k8s::errors` already did:
//!
//! * **Reached and refused.** A cluster answering `401`/`403` is
//!   `AuthRejected` there; a remote `vinfra` rejecting the stored password
//!   exits non-zero here, which used to be [`FailureClass::Internal`]. It is
//!   now `AuthRejected` when the remote's own stderr says so -- see
//!   [`remote_stderr_reads_as_an_authorisation_refusal`].
//! * **An unusable stored credential.** `classify_kubeconfig` calls a
//!   document it could read but not understand `Malformed`; a
//!   passphrase-protected SSH key is the same situation (the bytes are
//!   there, they are not usable as stored) and is now `Malformed` too, not
//!   `Internal`.
//! * **An expired deadline.** A remote command past `DEFAULT_TIMEOUT` was
//!   already [`FailureClass::Timeout`]; `ssh-add` past `SSH_ADD_TIMEOUT`
//!   raised `Internal` and now raises [`SshFailure::Timeout`] as well.

use std::fmt;

use qa_product_sdk::observation::{FailureClass, PluginFailure};

const ENCRYPTED_KEY: &str = "the configured SSH key is protected by a passphrase, and it cannot be \
                             unlocked non-interactively: register a passphrase-less key instead";
const AGENT_SETUP: &str = "the SSH agent that holds this environment's key could not be started; \
                           the gear's log names the stage that failed";
const UNREACHABLE: &str = "this host could not be reached over SSH: check the address, the port, \
                           and that the node is up";
const AUTH_REJECTED: &str = "this host refused the environment's SSH key: check the key and the \
                             user it is authorised for";
/// Covers both deadlines this connector enforces: a remote command past
/// `session::DEFAULT_TIMEOUT`, and `ssh-add` past `agent::SSH_ADD_TIMEOUT`.
/// "Operation" rather than "command" because the second one never reached a
/// remote at all.
const TIMED_OUT: &str = "the SSH operation did not finish within its deadline";
const COMMAND_FAILED: &str = "a command on the host exited non-zero; the remote's own message is \
                              carried beside this one";
/// The `CommandFailed` reading of a remote that was reached and refused us.
/// Distinct text from [`AUTH_REJECTED`], which is `sshd` refusing the SSH key
/// itself: an operator whose `vinfra` password is stale must not be sent to
/// look at their SSH key.
const REMOTE_AUTH_REJECTED: &str = "a command on the host refused this environment's stored \
                                    credential: check the username and password it is \
                                    registered with; the remote's own message is carried beside \
                                    this one";
const INTERNAL: &str = "the SSH transport failed locally; the gear's log names the stage";
/// The refusal `SshSession::exec_with_secret_env` makes before it spawns
/// anything. `Malformed`, for the same reason [`ENCRYPTED_KEY`] is: the bytes
/// were read and are unusable *as stored*, and the fix is to the credential.
const SECRET_NOT_DELIVERABLE: &str = "the stored value contains a line break, and the remote \
                                      command reads a single line, so it cannot be delivered \
                                      intact: re-register it without one";

/// A failure reaching or driving a host over SSH.
///
/// Deliberately **not** `Clone`: nothing needs a second copy, and the type
/// sits on the path a credential travels.
#[derive(Debug)]
pub enum SshFailure {
    /// The private key is passphrase-protected. Detected before `ssh-add` is
    /// spawned, because its failure on such a key is silent.
    EncryptedKey,
    /// A secret bound for a remote command's environment contains a newline,
    /// which the remote's `IFS= read -r` cannot carry. Detected before `ssh`
    /// is spawned.
    ///
    /// A variant of its own rather than an `Internal { stage }`, because a
    /// caller has to be able to *tell an operator* which credential to fix:
    /// this is the one failure on that path whose remedy is re-registering a
    /// stored value, and `qa-vhi-product-plugin`'s `read_node_count`
    /// distinguishes it by matching this variant rather than by comparing a
    /// stage string.
    SecretNotDeliverable,
    /// The agent could not be prepared. `stage` is a fixed string; **no**
    /// child's stderr is captured here.
    AgentSetup { stage: &'static str },
    /// `ssh` could not reach the host. `cause` is `ssh`'s own diagnostic.
    Unreachable { cause: String },
    /// `ssh` reached the host and the host refused our key.
    AuthRejected,
    /// The command exceeded its deadline.
    Timeout,
    /// The command ran and exited non-zero. `stderr` is the remote's text.
    CommandFailed { status: i32, stderr: String },
    /// Local I/O or process failure. `cause` never contains a child's stderr.
    Internal { stage: &'static str, cause: String },
}

impl SshFailure {
    /// Interpret an `ssh` exit by its own diagnostic.
    ///
    /// `ssh` exits 255 for *every* transport failure -- a refused connection,
    /// an unresolvable host and a rejected key are indistinguishable by exit
    /// code alone -- so the diagnostic text is the only signal there is. The
    /// phrases matched below are OpenSSH's own and are pinned by this module's
    /// tests; an unrecognised 255 becomes `Unreachable`, which is the safer of
    /// the two readings (it tells an operator to check the address rather than
    /// to regenerate a key that may be fine).
    #[must_use]
    pub fn from_ssh_stderr(status: i32, stderr: &str) -> Self {
        if status != 255 {
            return Self::CommandFailed {
                status,
                stderr: stderr.to_owned(),
            };
        }
        if stderr.contains("Permission denied")
            || stderr.contains("Too many authentication failures")
        {
            return Self::AuthRejected;
        }
        Self::Unreachable {
            cause: stderr.to_owned(),
        }
    }
}

impl fmt::Display for SshFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EncryptedKey => f.write_str(ENCRYPTED_KEY),
            Self::SecretNotDeliverable => f.write_str(SECRET_NOT_DELIVERABLE),
            Self::AgentSetup { stage } => {
                write!(f, "ssh agent setup failed while trying to {stage}")
            }
            Self::Unreachable { cause } => write!(f, "ssh could not reach the host: {cause}"),
            Self::AuthRejected => f.write_str(AUTH_REJECTED),
            Self::Timeout => f.write_str(TIMED_OUT),
            Self::CommandFailed { status, stderr } => {
                write!(f, "the remote command exited {status}: {stderr}")
            }
            Self::Internal { stage, cause } => {
                write!(f, "ssh transport failed while trying to {stage}: {cause}")
            }
        }
    }
}

impl std::error::Error for SshFailure {}

/// Classify a transport failure for a product plugin.
///
/// The counterpart of `qa_connector_k8s::classify`. Every `detail` below is a
/// module-level `const`, so no runtime byte can reach one.
#[must_use]
pub fn classify(failure: &SshFailure) -> PluginFailure {
    match failure {
        // `Malformed`, matching `classify_kubeconfig`: the stored bytes were
        // read and are simply not usable in the form they were stored in.
        // The operator's fix is to the credential, not to this gear.
        SshFailure::EncryptedKey => {
            PluginFailure::classified(FailureClass::Malformed, ENCRYPTED_KEY)
        }
        SshFailure::SecretNotDeliverable => {
            PluginFailure::classified(FailureClass::Malformed, SECRET_NOT_DELIVERABLE)
        }
        SshFailure::AgentSetup { .. } => {
            PluginFailure::classified(FailureClass::Internal, AGENT_SETUP)
        }
        SshFailure::Unreachable { .. } => {
            PluginFailure::classified(FailureClass::Unreachable, UNREACHABLE)
        }
        SshFailure::AuthRejected => {
            PluginFailure::classified(FailureClass::AuthRejected, AUTH_REJECTED)
        }
        SshFailure::Timeout => PluginFailure::classified(FailureClass::Timeout, TIMED_OUT),
        // Reached, ran, and refused. `qa-connector-k8s` calls the same
        // situation `AuthRejected` on a 401/403, and the design spec's §5
        // asks for it by name on the `vinfra` step; anything else non-zero
        // stays `Internal`, which is the honest reading of "a command we
        // expected to work did not".
        SshFailure::CommandFailed { stderr, .. } => {
            let (class, detail) = if remote_stderr_reads_as_an_authorisation_refusal(stderr) {
                (FailureClass::AuthRejected, REMOTE_AUTH_REJECTED)
            } else {
                (FailureClass::Internal, COMMAND_FAILED)
            };
            PluginFailure::classified(class, detail).with_remote_message(stderr.clone())
        }
        SshFailure::Internal { .. } => PluginFailure::classified(FailureClass::Internal, INTERNAL),
    }
}

/// Phrases a remote command uses when it was reached and refused us.
///
/// Matched case-insensitively against the remote's **own stderr**, which is
/// the only signal there is: `vinfra` (like most CLIs) exits non-zero for an
/// authorisation refusal and for a genuine internal error alike, with no
/// distinguishing exit code. The set is deliberately narrow -- these are
/// phrases that mean "you are not permitted", not phrases that merely mention
/// a credential -- because the cost of a false positive is telling an operator
/// to rotate a password that is fine.
///
/// The first entry is what the live stand actually produced
/// (`vinfra: error: unauthorized`, measured 2026-09-08).
const AUTHORISATION_REFUSAL_PHRASES: [&str; 6] = [
    "unauthorized",
    "unauthorised",
    "authentication failed",
    "permission denied",
    "access denied",
    "forbidden",
];

/// Whether a non-zero remote command's stderr says we were refused rather
/// than that something broke.
///
/// Lower-cased once rather than per phrase. This reads bytes a *remote* sent
/// and returns a `bool`: nothing derived from them is formatted or stored by
/// this function, and the stderr itself travels only where it already did,
/// on `remote_message`.
#[must_use]
pub fn remote_stderr_reads_as_an_authorisation_refusal(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    AUTHORISATION_REFUSAL_PHRASES
        .iter()
        .any(|phrase| lowered.contains(phrase))
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod errors_tests;
