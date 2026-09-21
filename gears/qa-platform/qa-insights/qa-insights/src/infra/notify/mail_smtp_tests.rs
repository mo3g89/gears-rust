//! [`SmtpMailClient`] against a **real SMTP conversation**.
//!
//! # Why there is a mock relay in here and not a `MockTransport`
//!
//! `lettre` ships a stub transport, and every test written against it proves
//! the same thing: that a `Message` was built. The defect this adapter exists
//! to fix was not a formatting bug — it was an adapter that reported success
//! without opening a socket — so a test that never opens one cannot be evidence
//! that mail sends. [`MockRelay`] is therefore a real `TcpListener` speaking
//! real SMTP, and every test below reaches it over `127.0.0.1` or fails trying.
//!
//! What that buys, concretely: the AUTH line these tests assert on is the one
//! that went over the wire, base64 and all, which is the only way to show that
//! the password came out of the credential store and reached the relay. A
//! transport double would have been handed the `Credentials` value and would
//! have proven nothing about either end of that.
//!
//! # The relay speaks in the clear, and what that does and does not weaken
//!
//! [`SmtpMailClient::plaintext_with_timeout`] is `cfg(test)` and is the only
//! way to build a client that does not demand TLS; production has one
//! constructor and it always does. Standing up a TLS relay would mean
//! generating a certificate and teaching the adapter a test-only trust anchor —
//! a second test-only seam, in the security-relevant path, to test the part
//! `rustls` already tests.
//!
//! The decision that seam bypasses is covered on its own, and it is a pure
//! function precisely so that it can be: [`super::tls_mode_for`] is asserted
//! directly, including that **no** port maps to "no TLS". The conversation, the
//! authentication and the four failure shapes are what the relay covers, and
//! TLS changes none of them.

use std::sync::Arc;
use std::time::Duration;

use credstore_sdk::test_util::MockCredStoreClient;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{IMPLICIT_TLS_PORT, SmtpMailClient, TlsMode, tls_mode_for};
use crate::domain::error::DomainError;
use crate::domain::ports::{MailClient, MailCredentials, MailMessage, SendOutcome};

/// The tenant every send below is made as. Never
/// [`SecurityContext::anonymous`]: credstore resolution is tenant-scoped, so an
/// anonymous context would be testing the nil tenant's view.
fn ctx() -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xDEAD))
        .subject_tenant_id(Uuid::from_u128(0x1A11))
        .build()
        .expect("subject_id and subject_tenant_id are both set")
}

/// What [`MockRelay`] does when `AUTH` arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthOutcome {
    /// `235 2.7.0 Authentication succeeded`.
    Accept,
    /// `535 5.7.8 Authentication credentials invalid` — the code a real relay
    /// sends for a wrong password (RFC 4954 §6).
    Reject,
}

/// How the relay behaves for one connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behaviour {
    /// A complete, well-behaved submission conversation.
    Converse(AuthOutcome),
    /// Accept the TCP connection and then say **nothing at all**, ever.
    ///
    /// This is the failure the ten-second bound exists for and the one an SMTP
    /// client is most exposed to: the server speaks first, so a black-holing
    /// relay hangs the caller before a single command can be sent. A timeout
    /// test against an HTTP client would have to stall mid-request; here it
    /// only has to stay quiet.
    Silent,
}

/// A real SMTP server on `127.0.0.1`, for one connection.
struct MockRelay {
    port: u16,
    transcript: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl MockRelay {
    /// Bind, and serve exactly one connection with `behaviour`.
    async fn start(behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral loopback port");
        let port = listener.local_addr().expect("local_addr").port();
        let transcript = Arc::new(Mutex::new(Vec::new()));

        let task = tokio::spawn({
            let transcript = Arc::clone(&transcript);
            async move {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                match behaviour {
                    Behaviour::Silent => {
                        // Hold the connection open and never write. Dropping
                        // the stream would send a FIN and give the client a
                        // clean EOF, which is a *different* failure from the
                        // one this arm exists to produce.
                        std::future::pending::<()>().await;
                    }
                    Behaviour::Converse(auth) => converse(stream, auth, &transcript).await,
                }
            }
        });

        Self {
            port,
            transcript,
            task,
        }
    }

    /// Every command line the client sent, in order. The `DATA` payload is
    /// recorded as one entry prefixed `BODY `.
    fn transcript(&self) -> Vec<String> {
        self.transcript.lock().clone()
    }
}

impl Drop for MockRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One scripted SMTP submission conversation.
///
/// Hand-rolled rather than pulled from a crate: the point of these tests is
/// that *this* adapter drives a real dialogue correctly, and the smallest
/// dependency that would serve is a mail server. Every reply below is the code
/// a real relay sends for that step.
async fn converse(stream: TcpStream, auth: AuthOutcome, transcript: &Arc<Mutex<Vec<String>>>) {
    let (read_half, mut write) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    if write
        .write_all(b"220 mock.test ESMTP ready\r\n")
        .await
        .is_err()
    {
        return;
    }

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let command = line.trim_end_matches(['\r', '\n']).to_owned();
        transcript.lock().push(command.clone());
        let upper = command.to_ascii_uppercase();

        let reply: String = if upper.starts_with("EHLO") {
            // The capability list is what makes the client offer AUTH at all;
            // the last line of a multiline reply uses a space, not a dash.
            "250-mock.test\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n".to_owned()
        } else if upper.starts_with("HELO") {
            "250 mock.test\r\n".to_owned()
        } else if upper.starts_with("AUTH") {
            match auth {
                AuthOutcome::Accept => "235 2.7.0 Authentication succeeded\r\n".to_owned(),
                AuthOutcome::Reject => {
                    "535 5.7.8 Authentication credentials invalid\r\n".to_owned()
                }
            }
        } else if upper.starts_with("MAIL FROM") || upper.starts_with("RCPT TO") {
            "250 2.1.0 Ok\r\n".to_owned()
        } else if upper.starts_with("DATA") {
            if write
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await
                .is_err()
            {
                return;
            }
            let mut body = String::new();
            loop {
                let mut chunk = String::new();
                match reader.read_line(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                if chunk.trim_end_matches(['\r', '\n']) == "." {
                    break;
                }
                body.push_str(&chunk);
            }
            transcript.lock().push(format!("BODY {body}"));
            "250 2.0.0 Ok: queued as MOCK1\r\n".to_owned()
        } else if upper.starts_with("QUIT") {
            // Best effort: the client sent `QUIT` and may already have gone,
            // and the mock has nothing left to do whether the write lands or
            // not. The result is dropped rather than `let _`-bound so that
            // `clippy::let_underscore_must_use` stays on for everything else.
            drop(write.write_all(b"221 2.0.0 Bye\r\n").await);
            return;
        } else {
            "250 2.0.0 Ok\r\n".to_owned()
        };

        if write.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// The credstore reference every authenticated test uses, and the password
/// behind it.
const SMTP_REF: &str = "qa-smtp-password";
const SMTP_PASSWORD: &str = "s3cr3t-relay-password";

/// A client that dials the mock relay in the clear, with a bound short enough
/// that the timeout test finishes in under a second.
fn client(credstore: MockCredStoreClient, timeout: Duration) -> SmtpMailClient {
    SmtpMailClient::plaintext_with_timeout(
        Arc::new(credstore),
        vec!["127.0.0.1".to_owned()],
        timeout,
    )
}

/// A client with a real-world bound, for the tests that never wait it out.
fn ready_client() -> SmtpMailClient {
    client(
        MockCredStoreClient::with_secrets(vec![(SMTP_REF.to_owned(), SMTP_PASSWORD.to_owned())]),
        Duration::from_secs(5),
    )
}

fn message(port: u16, credentials: Option<MailCredentials>) -> MailMessage {
    MailMessage {
        smtp_host: "127.0.0.1".to_owned(),
        smtp_port: port,
        credentials,
        from: "qa-platform@example.com".to_owned(),
        recipients: "alice@example.com, bob@example.com".to_owned(),
        subject: "Run 42 failed".to_owned(),
        body: "Three tests failed.".to_owned(),
    }
}

fn credentials() -> MailCredentials {
    MailCredentials {
        username: "qa-platform@example.com".to_owned(),
        password_credstore_ref: SMTP_REF.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Success
// ---------------------------------------------------------------------------

/// The whole point of the change, end to end: a real socket, a real
/// conversation, and a message the relay accepted.
///
/// Asserts the *envelope* rather than only the return value — `MAIL FROM` and
/// both `RCPT TO` lines — because `Ok(Sent)` would also be the answer if the
/// adapter had sent an empty message to nobody.
#[tokio::test]
async fn a_send_over_an_unauthenticated_relay_reaches_the_wire() {
    let relay = MockRelay::start(Behaviour::Converse(AuthOutcome::Accept)).await;

    let outcome = ready_client()
        .send(&ctx(), &message(relay.port, None))
        .await
        .expect("the relay accepted the message");
    assert_eq!(outcome, SendOutcome::Sent);

    let transcript = relay.transcript();
    assert!(
        transcript
            .iter()
            .any(|l| l.starts_with("MAIL FROM:<qa-platform@example.com>")),
        "envelope sender missing from {transcript:?}"
    );
    assert!(
        transcript
            .iter()
            .any(|l| l.starts_with("RCPT TO:<alice@example.com>")),
        "first recipient missing from {transcript:?}"
    );
    assert!(
        transcript
            .iter()
            .any(|l| l.starts_with("RCPT TO:<bob@example.com>")),
        "second recipient missing from {transcript:?} - the comma split dropped one"
    );
    assert!(
        transcript
            .iter()
            .any(|l| l.starts_with("BODY ") && l.contains("Three tests failed.")),
        "message body never reached DATA in {transcript:?}"
    );
    // No credentials were configured, so none may be offered.
    assert!(
        !transcript.iter().any(|l| l.starts_with("AUTH")),
        "an unauthenticated send must not send AUTH: {transcript:?}"
    );
}

/// The credential path, proven at the only place that can prove it: the bytes
/// on the wire.
///
/// `AUTH PLAIN` carries `\0username\0password`, base64-encoded (RFC 4616). This
/// decodes the line the relay actually received and asserts that the password
/// in it is the one the *credential store* held — which is the difference
/// between "the adapter was handed a `Credentials` value" and "the secret was
/// resolved for this tenant and delivered".
#[tokio::test]
async fn the_relay_password_comes_from_the_credential_store() {
    let relay = MockRelay::start(Behaviour::Converse(AuthOutcome::Accept)).await;

    let outcome = ready_client()
        .send(&ctx(), &message(relay.port, Some(credentials())))
        .await
        .expect("the relay accepted the authenticated message");
    assert_eq!(outcome, SendOutcome::Sent);

    let transcript = relay.transcript();
    let auth = transcript
        .iter()
        .find(|l| l.starts_with("AUTH PLAIN "))
        .expect("AUTH PLAIN was never sent");
    let encoded = auth
        .strip_prefix("AUTH PLAIN ")
        .expect("the prefix was just matched");
    let decoded = decode_base64(encoded.trim());
    let decoded = String::from_utf8(decoded).expect("AUTH PLAIN payload is UTF-8 here");

    assert_eq!(
        decoded,
        format!("\0qa-platform@example.com\0{SMTP_PASSWORD}"),
        "the AUTH payload must carry the credstore value, not a placeholder"
    );
}

// ---------------------------------------------------------------------------
// The four failures
// ---------------------------------------------------------------------------

/// A relay that rejects the credentials fails the send, and the refusal reaches
/// the operator.
///
/// The relay's own `535 5.7.8` text is expected in the message: ADR-0008's one
/// sanctioned exception is text a remote sent back, and without it the audit
/// log would say only "the send failed" for a wrong password.
#[tokio::test]
async fn an_authentication_failure_fails_the_send_and_says_why() {
    let relay = MockRelay::start(Behaviour::Converse(AuthOutcome::Reject)).await;

    let error = ready_client()
        .send(&ctx(), &message(relay.port, Some(credentials())))
        .await
        .expect_err("a rejected credential cannot be a successful send");

    let rendered = error.to_string();
    assert!(
        matches!(error, DomainError::Internal(_)),
        "an authentication rejection is a transport failure, got {error:?}"
    );
    assert!(
        rendered.contains("5.7.8") || rendered.to_lowercase().contains("authentication"),
        "the relay's own refusal must survive into the message, got {rendered}"
    );

    // Nothing may be submitted after a failed AUTH.
    let transcript = relay.transcript();
    assert!(
        !transcript.iter().any(|l| l.starts_with("MAIL FROM")),
        "the envelope was sent despite a failed AUTH: {transcript:?}"
    );
}

/// The bound is *applied*, not merely declared: a relay that accepts the
/// connection and never speaks fails inside the configured timeout instead of
/// hanging until the OS gives up on the TCP session.
///
/// 300ms rather than [`SmtpMailClient::SEND_TIMEOUT`]'s ten seconds, for the
/// reason `SlackOagwClient::with_timeout` exists; the real constant is pinned
/// separately below.
#[tokio::test]
async fn a_silent_relay_fails_within_the_timeout() {
    let relay = MockRelay::start(Behaviour::Silent).await;
    let timeout = Duration::from_millis(300);

    let started = std::time::Instant::now();
    let error = client(MockCredStoreClient::empty(), timeout)
        .send(&ctx(), &message(relay.port, None))
        .await
        .expect_err("a relay that never greets cannot accept a message");
    let elapsed = started.elapsed();

    assert!(
        matches!(error, DomainError::Internal(_)),
        "expected a transport failure, got {error:?}"
    );
    // Generous, because CI schedulers are: the claim is "bounded", not
    // "bounded to the millisecond". An unbounded send against this relay never
    // returns at all, so any finite time here is the signal.
    assert!(
        elapsed < Duration::from_secs(5),
        "the send took {elapsed:?}, which is not the {timeout:?} bound being applied"
    );
    // The relay did receive a connection — otherwise this would be the
    // unreachable-host test wearing a different name.
    assert!(
        relay.transcript().is_empty(),
        "the silent relay must have said nothing"
    );
}

/// A host that refuses the connection fails the send rather than reporting one.
///
/// The port is obtained by binding and then **closing** a listener, which is
/// the only way to name a port that is reliably nobody's on a shared machine;
/// picking a constant would be a test that fails whenever something else is
/// listening.
#[tokio::test]
async fn an_unreachable_relay_fails_the_send() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral loopback port");
        listener.local_addr().expect("local_addr").port()
        // dropped here, so the port stops listening
    };

    let error = ready_client()
        .send(&ctx(), &message(port, None))
        .await
        .expect_err("nothing is listening on that port");
    assert!(
        matches!(error, DomainError::Internal(_)),
        "expected a transport failure, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// What never reaches a socket
// ---------------------------------------------------------------------------

/// The deployment-level allow-list refuses before anything is dialled.
///
/// Checked against a **live relay on an allowed port**, named by a host that is
/// not allow-listed: if the check were missing, this send would succeed, so the
/// test cannot pass vacuously.
#[tokio::test]
async fn a_host_outside_the_allow_list_is_refused_without_dialling() {
    let relay = MockRelay::start(Behaviour::Converse(AuthOutcome::Accept)).await;
    let client = SmtpMailClient::plaintext_with_timeout(
        Arc::new(MockCredStoreClient::empty()),
        vec!["smtp.corp.example".to_owned()],
        Duration::from_secs(5),
    );

    let error = client
        .send(&ctx(), &message(relay.port, None))
        .await
        .expect_err("127.0.0.1 is not on this deployment's allow-list");

    assert!(
        matches!(&error, DomainError::Validation { field, .. } if field == "email_smtp_host"),
        "expected a Validation naming the host field, got {error:?}"
    );
    assert!(
        !error.to_string().contains("smtp.corp.example"),
        "the deployment's allow-list must not be echoed to a tenant: {error}"
    );
    assert!(
        relay.transcript().is_empty(),
        "the refusal happened after connecting: {:?}",
        relay.transcript()
    );
}

/// A reference the credential store cannot resolve is a `Validation` naming the
/// reference field — the thing an operator can fix — and never a send.
#[tokio::test]
async fn an_unresolvable_credential_reference_is_a_validation_error() {
    let relay = MockRelay::start(Behaviour::Converse(AuthOutcome::Accept)).await;

    let error = client(MockCredStoreClient::empty(), Duration::from_secs(5))
        .send(&ctx(), &message(relay.port, Some(credentials())))
        .await
        .expect_err("an empty credential store resolves nothing");

    assert!(
        matches!(&error, DomainError::Validation { field, .. }
            if field == "email_smtp_credstore_ref"),
        "expected a Validation naming the reference field, got {error:?}"
    );
    assert!(
        relay.transcript().is_empty(),
        "the credential was resolved after connecting: {:?}",
        relay.transcript()
    );
}

/// A secret that is not UTF-8 cannot be an SMTP password, and the refusal must
/// not carry the bytes.
///
/// ADR-0008 is the reason for the second assertion: `FromUtf8Error` owns the
/// bytes it failed on, so a `map_err(|e| format!("{e}"))` here would print
/// credential material into a validation message an operator sees.
#[tokio::test]
async fn a_non_utf8_secret_is_refused_without_rendering_it() {
    let relay = MockRelay::start(Behaviour::Converse(AuthOutcome::Accept)).await;
    let planted = vec![0xF0, 0x9F, 0x92, 0xA9, 0xFF, 0xFE];

    let error = client(
        MockCredStoreClient::returning_raw_value(planted.clone()),
        Duration::from_secs(5),
    )
    .send(&ctx(), &message(relay.port, Some(credentials())))
    .await
    .expect_err("a non-UTF-8 secret cannot be an SMTP password");

    let rendered = error.to_string();
    assert!(
        matches!(&error, DomainError::Validation { field, .. }
            if field == "email_smtp_credstore_ref"),
        "expected a Validation naming the reference field, got {error:?}"
    );
    for byte in &planted {
        assert!(
            !rendered.contains(&format!("{byte}")),
            "the refusal rendered a byte of the secret: {rendered}"
        );
    }
}

/// An address the message builder refuses never reaches a socket, and the
/// refusal names the settings field it came from rather than "email".
#[tokio::test]
async fn a_malformed_sender_is_refused_before_dialling() {
    let relay = MockRelay::start(Behaviour::Converse(AuthOutcome::Accept)).await;

    let error = ready_client()
        .send(
            &ctx(),
            &MailMessage {
                from: "not an address".to_owned(),
                ..message(relay.port, None)
            },
        )
        .await
        .expect_err("'not an address' is not a mailbox");

    assert!(
        matches!(&error, DomainError::Validation { field, .. } if field == "email_from"),
        "expected a Validation naming the sender field, got {error:?}"
    );
    assert!(relay.transcript().is_empty());
}

/// A recipient line that is only separators has no recipients, and an email to
/// nobody is a configuration error rather than a successful send.
#[tokio::test]
async fn a_recipient_line_with_no_addresses_is_refused() {
    let relay = MockRelay::start(Behaviour::Converse(AuthOutcome::Accept)).await;

    let error = ready_client()
        .send(
            &ctx(),
            &MailMessage {
                recipients: " , ,, ".to_owned(),
                ..message(relay.port, None)
            },
        )
        .await
        .expect_err("a line of commas names nobody");

    assert!(
        matches!(&error, DomainError::Validation { field, .. } if field == "email_recipients"),
        "expected a Validation naming the recipients field, got {error:?}"
    );
    assert!(relay.transcript().is_empty());
}

// ---------------------------------------------------------------------------
// The two constants
// ---------------------------------------------------------------------------

/// The bound is legacy's own ten seconds and `SlackOagwClient`'s — pinned here
/// so the tests above may use a short one without the real value going
/// unasserted.
#[test]
fn the_send_timeout_is_ten_seconds() {
    assert_eq!(SmtpMailClient::SEND_TIMEOUT, Duration::from_secs(10));
    assert_eq!(
        SmtpMailClient::SEND_TIMEOUT,
        super::super::SlackOagwClient::REQUEST_TIMEOUT,
        "the two egress bounds are one decision; they must not drift apart silently"
    );
}

/// The TLS decision, asserted where the plaintext test seam cannot reach: 465
/// is implicit TLS, and **every** other port — including 25, which is where a
/// reader would expect a plaintext exception to hide — is `STARTTLS`.
#[test]
fn only_port_465_is_implicit_tls_and_nothing_is_plaintext() {
    assert_eq!(tls_mode_for(IMPLICIT_TLS_PORT), TlsMode::Implicit);
    for port in [25_u16, 587, 2525, 1, 464, 466, u16::MAX] {
        assert_eq!(
            tls_mode_for(port),
            TlsMode::StartTls,
            "port {port} must require STARTTLS"
        );
    }
}

// ---------------------------------------------------------------------------
// Test-local base64
// ---------------------------------------------------------------------------

/// Decode a standard base64 string.
///
/// Twelve lines rather than a `base64` dev-dependency this crate does not have:
/// it is used by exactly one assertion, and a test helper that decodes the
/// fixture it is about to compare against is easier to trust when it is visible
/// than when it is a version range.
fn decode_base64(encoded: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    let mut out = Vec::new();
    for byte in encoded.bytes().filter(|b| *b != b'=') {
        let value = ALPHABET
            .iter()
            .position(|c| *c == byte)
            .unwrap_or_else(|| panic!("'{}' is not base64", byte as char));
        buffer = (buffer << 6) | u32::try_from(value).expect("a base64 digit fits in u32");
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xFF).expect("masked to one byte"));
        }
    }
    out
}
