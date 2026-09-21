//! [`MailClient`] over a real SMTP relay — the adapter that closes D10.
//!
//! # Why this one does not go through `oagw`, when every other egress does
//!
//! `infra::jira::oagw_client` and [`super::slack_oagw`] both open with the same
//! rule, and it is the right rule: this subsystem's egress contract
//! (`cpt-cf-qa-contract-egress`) routes outbound calls through the Outbound API
//! Gateway, which resolves the credential and controls what leaves the
//! deployment. **`oagw` speaks HTTP. SMTP is not HTTP**, and no amount of
//! configuration makes a request/response proxy carry a multi-round, stateful,
//! server-greets-first conversation that can be upgraded to TLS mid-stream.
//!
//! So this is the subsystem's second direct egress, alongside qa-catalog's git
//! transport, and it is recorded the same way that one is: ADR-0011
//! (`cpt-cf-qa-adr-smtp-egress`), which also records the two consequences that
//! follow from it — this gear resolves an SMTP credential itself (the first
//! credential it ever holds in plaintext), and the destination cannot be
//! constrained by a Kubernetes `NetworkPolicy`, so it is constrained by
//! [`SmtpMailClient::allowed_hosts`] instead.
//!
//! # The ten-second bound is [`super::slack_oagw`]'s, ported for the same reason
//!
//! [`SmtpMailClient::SEND_TIMEOUT`] is `Duration::from_secs(10)`, the value
//! `SlackOagwClient::REQUEST_TIMEOUT` carries and for the reason its module
//! header records: legacy built its notification HTTP client with a
//! ten-second total bound because the run-queue dispatcher awaits a send
//! *inside its tick*, so an unbounded call against a black-holing host stalls
//! the queue rather than losing one notification
//! (`manager/src/services/notifications.rs:52-64`). An SMTP conversation is
//! strictly more exposed to that failure than one HTTP request: the server
//! speaks first, so a host that accepts the TCP connection and then says
//! nothing hangs the caller before a single command is sent.
//!
//! **It is applied twice, deliberately, and the two are not redundant.**
//!
//! * `AsyncSmtpTransportBuilder::timeout` bounds `lettre`'s own TCP connect and
//!   each individual command read (`transport/smtp/client/async_net.rs`).
//! * `tokio::time::timeout` wraps the whole `send`, which is what bounds the
//!   *conversation*: a relay that answers every command in nine seconds
//!   violates no per-command bound and still takes a minute to refuse a
//!   message.
//!
//! # TLS is chosen by the port number, and there is no third option
//!
//! Port 465 is what IANA assigned to submission-over-implicit-TLS and what RFC
//! 8314 §3.3 restored as the preferred submission port: the socket is wrapped
//! before the first byte. Every other port is dialled in the clear and
//! **required** to offer `STARTTLS` — `Tls::Required`, not `Tls::Opportunistic`,
//! so a relay that does not offer it fails instead of quietly sending the
//! credential and the message in the clear (`lettre`'s own doc for
//! `starttls_relay`: *"An error is returned if the connection can't be
//! upgraded. No credentials or emails will be sent to the server, protecting
//! from downgrade attacks."*).
//!
//! **There is deliberately no plaintext mode and no third column to select
//! one.** A `tls: none` setting is a thing an operator sets once to make a
//! stubborn relay work and nobody ever revisits, and this is a path that
//! carries a password. The cost is real and is stated rather than hidden: a
//! deployment whose relay speaks neither implicit TLS nor `STARTTLS` cannot use
//! this adapter at all, and gets a failed send with the relay's own refusal in
//! the audit log rather than an insecure success.
//!
//! # What is *not* here: a `reqwest`, a retry, and a queue
//!
//! No retry: `NotifyService`'s claim lifecycle releases the dedupe claim on a
//! failed send and writes an `OUTCOME_FAILED` audit row, so the run-completed
//! path is already re-attemptable and a retry loop inside a ten-second budget
//! would only make the tick it runs in slower.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::{CredStoreClientV1, SecretRef};
use lettre::message::Mailbox;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncTransport as _, Message, Tokio1Executor};
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::ports::{
    CREDSTORE_REF_SCHEME, MailClient, MailCredentials, MailMessage, SendOutcome,
};

/// The `NotificationConfigDto`/`qa_notification_config` field each refusal
/// names, so an operator is told which box on the settings page to fix rather
/// than that "the email failed".
const HOST_FIELD: &str = "email_smtp_host";
const FROM_FIELD: &str = "email_from";
const RECIPIENTS_FIELD: &str = "email_recipients";
const REF_FIELD: &str = "email_smtp_credstore_ref";

/// How the socket gets its TLS — see this module's header. Two values, because
/// there are two ways a submission relay is reached and no third one this
/// adapter will speak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TlsMode {
    /// TLS before the first byte (RFC 8314 §3.3). Port 465 only.
    Implicit,
    /// Clear connect, then a **required** `STARTTLS` upgrade.
    StartTls,
}

/// The port that means implicit TLS, and the only one.
pub(crate) const IMPLICIT_TLS_PORT: u16 = 465;

/// The TLS mode for `port`.
///
/// A free function over the port rather than a method, and tested directly:
/// this is the whole of the security decision this adapter makes on its own,
/// and it should be readable and falsifiable without building a transport.
#[must_use]
pub(crate) const fn tls_mode_for(port: u16) -> TlsMode {
    if port == IMPLICIT_TLS_PORT {
        TlsMode::Implicit
    } else {
        TlsMode::StartTls
    }
}

/// [`MailClient`] over `lettre`. See this module's header.
pub struct SmtpMailClient {
    /// Where the relay password comes from. `gear::init` resolves this from
    /// `ClientHub`, the same way qa-catalog does for its repository
    /// credentials.
    credstore: Arc<dyn CredStoreClientV1>,
    /// The relay hostnames this **deployment** permits, from
    /// `QaInsightsConfig::smtp_allowed_hosts`.
    ///
    /// # This is the egress control, because the network layer cannot be one
    ///
    /// `email_smtp_host` is a per-tenant column an operator types into the
    /// settings page; it is not known at chart-render time, and a Kubernetes
    /// `NetworkPolicy` matches CIDRs and pod labels, never DNS names. On top of
    /// that, all four qa-platform gears run in one pod, whose egress set
    /// already includes arbitrary git remotes and arbitrary tenant management
    /// nodes — so any egress policy that selected it would have to permit
    /// `0.0.0.0/0` and would constrain nothing. ADR-0011 carries that argument
    /// in full. The allow-list is therefore enforced *here*, where the host is
    /// actually known, and an empty one is how a deployment says "no SMTP" —
    /// `gear::init` then binds
    /// [`UnsupportedMailClient`](super::mail_unsupported::UnsupportedMailClient)
    /// instead of this type, so this field is never empty on a live instance.
    allowed_hosts: Arc<[String]>,
    /// The bound `send` applies. [`Self::SEND_TIMEOUT`] in production, always —
    /// [`Self::plaintext_with_timeout`] exists only so a test can prove the
    /// bound is *applied*, not merely declared, without waiting out ten real
    /// seconds.
    timeout: Duration,
    /// Test-only: dial in the clear, for the mock relay. **Never set in
    /// production** — [`Self::new`] is the only constructor outside `cfg(test)`
    /// and does not take it. The TLS decision this flag bypasses is covered on
    /// its own by [`tls_mode_for`]'s tests; what the mock relay exists to cover
    /// is the conversation, the authentication and the four failure shapes,
    /// none of which TLS changes.
    #[cfg(test)]
    plaintext: bool,
}

impl SmtpMailClient {
    /// The fixed bound this module's header explains, and
    /// `SlackOagwClient::REQUEST_TIMEOUT`'s value.
    pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);

    /// `allowed_hosts` must be non-empty; `gear::init` binds
    /// [`UnsupportedMailClient`](super::mail_unsupported::UnsupportedMailClient)
    /// when it is not, which is the deployment-level "no SMTP" answer.
    #[must_use]
    pub fn new(credstore: Arc<dyn CredStoreClientV1>, allowed_hosts: Vec<String>) -> Self {
        Self {
            credstore,
            allowed_hosts: allowed_hosts.into(),
            timeout: Self::SEND_TIMEOUT,
            #[cfg(test)]
            plaintext: false,
        }
    }

    /// Test-only: a client that dials `127.0.0.1` in the clear with a bound
    /// measured in milliseconds rather than the real ten seconds.
    #[cfg(test)]
    fn plaintext_with_timeout(
        credstore: Arc<dyn CredStoreClientV1>,
        allowed_hosts: Vec<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            credstore,
            allowed_hosts: allowed_hosts.into(),
            timeout,
            plaintext: true,
        }
    }

    /// Whether this deployment permits mail to `host`.
    ///
    /// ASCII-case-insensitive because DNS is, and an operator who typed
    /// `SMTP.Example.Com` into the settings page has named the host the chart
    /// allow-listed as `smtp.example.com`.
    fn host_allowed(&self, host: &str) -> bool {
        self.allowed_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
    }

    /// The relay password, resolved under the **sending tenant's** context.
    ///
    /// `ctx` is why [`MailClient::send`] takes one at all: credstore resolution
    /// is tenant-scoped, and an adapter that passed `SecurityContext::anonymous`
    /// here could only ever read the nil tenant's secrets — which is the
    /// mistake `SlackClient` made and ruling R108 corrected.
    ///
    /// The value is returned as a `String` and lives until the transport is
    /// dropped. That is plaintext credential material inside this process,
    /// which is new for this gear, and ADR-0008
    /// (`cpt-cf-qa-adr-credential-containment`) is what bounds it: **nothing
    /// derived from it is formatted**, here or anywhere below. Note in
    /// particular the `|_|` on the UTF-8 conversion — `FromUtf8Error` carries
    /// the bytes it failed on, so binding and rendering it would print the
    /// secret into a validation message.
    async fn password(
        &self,
        ctx: &SecurityContext,
        reference: &str,
    ) -> Result<String, DomainError> {
        let invalid = |why: &str| DomainError::Validation {
            field: REF_FIELD.to_owned(),
            message: format!("the SMTP credential could not be resolved: {why}"),
        };

        let name = reference
            .strip_prefix(CREDSTORE_REF_SCHEME)
            .unwrap_or(reference);
        let key = SecretRef::new(name)
            .map_err(|_| invalid("it is not a syntactically valid credential-store reference"))?;

        let found = self.credstore.get(ctx, &key).await.map_err(|e| {
            DomainError::Internal(format!("the credential store refused the SMTP secret: {e}"))
        })?;
        // `Ok(None)` is credstore's single anti-enumeration 404 surface: it
        // means "no such secret, or not yours", and the two are deliberately
        // indistinguishable. A `Validation` rather than an `Internal`, because
        // the thing to change is the reference on the settings page.
        let secret = found.ok_or_else(|| {
            invalid("no secret of that name is readable by this tenant; check the reference")
        })?;

        String::from_utf8(secret.value.as_bytes().to_vec()).map_err(|_| {
            invalid("the stored secret is not valid UTF-8 and cannot be an SMTP password")
        })
    }

    /// The `lettre` transport for one send.
    fn transport(
        &self,
        host: &str,
        port: u16,
        credentials: Option<Credentials>,
    ) -> Result<AsyncSmtpTransport<Tokio1Executor>, DomainError> {
        let builder = self.transport_builder(host, port)?;
        let builder = builder.port(port).timeout(Some(self.timeout));
        let builder = match credentials {
            Some(credentials) => builder.credentials(credentials),
            None => builder,
        };
        Ok(builder.build())
    }

    /// The TLS half of [`Self::transport`], split out so the `cfg(test)`
    /// plaintext escape hatch is one branch in one place rather than scattered
    /// through the builder chain.
    ///
    /// `self` is read only by that escape hatch, so outside `cfg(test)` clippy
    /// is right that it is unused -- and taking it anyway is the point: the
    /// method must keep one signature across both cfgs, or [`Self::transport`]
    /// would need a `cfg` of its own at the call site.
    #[cfg_attr(not(test), allow(clippy::unused_self))]
    fn transport_builder(
        &self,
        host: &str,
        port: u16,
    ) -> Result<lettre::transport::smtp::AsyncSmtpTransportBuilder, DomainError> {
        #[cfg(test)]
        if self.plaintext {
            return Ok(
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
                    .tls(lettre::transport::smtp::client::Tls::None),
            );
        }

        let built = match tls_mode_for(port) {
            TlsMode::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(host),
            TlsMode::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host),
        };
        built.map_err(|e| {
            DomainError::Internal(format!(
                "the SMTP transport for {host} could not be built: {e}"
            ))
        })
    }
}

/// Parse one mailbox, naming the settings field it came from.
fn mailbox(field: &str, raw: &str) -> Result<Mailbox, DomainError> {
    raw.trim().parse::<Mailbox>().map_err(|e| {
        // The *address* is the operator's own input, not credential material,
        // so echoing it is what makes the refusal actionable.
        DomainError::Validation {
            field: field.to_owned(),
            message: format!("'{}' is not a valid email address: {e}", raw.trim()),
        }
    })
}

/// Build the one message for `message`.
///
/// The recipient split is `split(',')`, matching legacy's own
/// (`manager/src/services/notifications.rs:144-153`) and the reason
/// [`MailMessage::recipients`] is one string rather than a `Vec`: the operator's
/// line is stored as typed and split here.
fn build_message(message: &MailMessage) -> Result<Message, DomainError> {
    let mut builder = Message::builder()
        .from(mailbox(FROM_FIELD, &message.from)?)
        .subject(message.subject.clone());

    let mut recipients = 0_usize;
    for candidate in message.recipients.split(',') {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        builder = builder.to(mailbox(RECIPIENTS_FIELD, candidate)?);
        recipients += 1;
    }
    if recipients == 0 {
        return Err(DomainError::Validation {
            field: RECIPIENTS_FIELD.to_owned(),
            message: "at least one recipient is required".to_owned(),
        });
    }

    builder.body(message.body.clone()).map_err(|e| {
        DomainError::Internal(format!("the notification email could not be built: {e}"))
    })
}

#[async_trait]
impl MailClient for SmtpMailClient {
    async fn send(
        &self,
        ctx: &SecurityContext,
        message: &MailMessage,
    ) -> Result<SendOutcome, DomainError> {
        let host = message.smtp_host.trim();
        if host.is_empty() {
            return Err(DomainError::Validation {
                field: HOST_FIELD.to_owned(),
                message: "an SMTP host is required".to_owned(),
            });
        }
        if !self.host_allowed(host) {
            // The allowed set is **not** echoed. It is deployment
            // configuration, and a tenant-facing settings error is not where an
            // operator of one tenant learns another deployment-level fact.
            return Err(DomainError::Validation {
                field: HOST_FIELD.to_owned(),
                message: format!(
                    "'{host}' is not an SMTP relay this deployment is permitted to reach; ask the \
                     platform operator to add it to qa-insights' smtp_allowed_hosts"
                ),
            });
        }

        let credentials = match &message.credentials {
            Some(MailCredentials {
                username,
                password_credstore_ref,
            }) => Some(Credentials::new(
                username.clone(),
                self.password(ctx, password_credstore_ref).await?,
            )),
            None => None,
        };

        let email = build_message(message)?;
        let transport = self.transport(host, message.smtp_port, credentials)?;

        // The conversation bound, distinct from the per-command bound handed to
        // the builder above — see this module's header.
        let response = tokio::time::timeout(self.timeout, transport.send(email))
            .await
            .map_err(|_| {
                DomainError::Internal(format!(
                    "the SMTP send to {host} did not complete within {:?}",
                    self.timeout
                ))
            })?
            .map_err(|e| {
                // ADR-0008's one sanctioned exception: text the remote sent
                // back is not derived from our credential material, and
                // suppressing it leaves an operator debugging blind. `lettre`'s
                // `Error` renders the relay's own refusal and never the
                // credentials it was given.
                DomainError::Internal(format!("the SMTP relay {host} refused the message: {e}"))
            })?;

        if response.is_positive() {
            Ok(SendOutcome::Sent)
        } else {
            Err(DomainError::Internal(format!(
                "the SMTP relay {host} answered {:?}",
                response.code()
            )))
        }
    }
}

#[cfg(test)]
#[path = "mail_smtp_tests.rs"]
mod tests;
