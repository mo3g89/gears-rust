//! Classifying a `kube` failure into fixed, operator-facing text.
//!
//! **Moved out of `infra/observer/` by Task 19b**, which deleted the
//! observation half of that module and kept decision D4's runner-`Secret`
//! writer. Every item here serves the writer: it builds a Kubernetes client
//! from the Argo kubeconfig, and a failure to do so has to reach an operator
//! as something they can act on.
//!
//! # Classified by TYPE, never by message text
//!
//! `kube::Error`'s `Display` is not a stable interface and its wording changes
//! between releases, so a connect failure is recognised through
//! `hyper_util::client::legacy::Error::is_connect()` and a certificate failure
//! by downcasting the cause chain to `rustls::Error`. That is why this crate
//! depends on `hyper-util` and `rustls` directly even though it never calls
//! either to make a request.
//!
//! The text these produce is written into `version_detect_error`, which is
//! published on `EnvironmentDto` and rendered on the environment page, so it
//! must never carry anything credential-derived — the rule the 2026-08-28 leak
//! established (**D-CH-5**).

use kube::config::KubeconfigError;

/// Fixed explanation of why a document could not be used as a kubeconfig.
///
/// Returns a `&'static str` on purpose: a `&'static str` cannot carry input.
/// That is the invariant, expressed as a type rather than as a promise.
pub fn describe_kubeconfig_error(error: &KubeconfigError) -> &'static str {
    match error {
        KubeconfigError::Parse(_) => "it is not valid YAML",
        KubeconfigError::InvalidStructure(_) => {
            "it is valid YAML but does not have a kubeconfig's shape - check that what was \
             stored is a kubeconfig file (`apiVersion: v1`, `kind: Config`, with `clusters`, \
             `contexts` and `users`) and not a certificate, a private key, or some other YAML"
        }
        KubeconfigError::CurrentContextNotSet => {
            "it sets no `current-context`, so there is no context to connect with"
        }
        KubeconfigError::LoadContext(_) => {
            "its `current-context` does not name any context the document defines"
        }
        KubeconfigError::LoadClusterOfContext(_) => {
            "its selected context names a cluster the document does not define"
        }
        KubeconfigError::KindMismatch => "its `kind` is not `Config`",
        KubeconfigError::ApiVersionMismatch => "its `apiVersion` is not `v1`",
        KubeconfigError::MissingClusterUrl => "its selected cluster has no `server` URL",
        KubeconfigError::ParseClusterUrl(_) => "its selected cluster's `server` is not a valid URL",
        KubeconfigError::ParseProxyUrl(_) => {
            "its selected cluster's `proxy-url` is not a valid URL"
        }
        KubeconfigError::LoadCertificateAuthority(_) => {
            "its `certificate-authority`/`certificate-authority-data` could not be loaded \
             (unreadable file, or invalid base64)"
        }
        KubeconfigError::LoadClientCertificate(_) => {
            "its selected user's `client-certificate`/`client-certificate-data` could not be \
             loaded (unreadable file, or invalid base64)"
        }
        KubeconfigError::LoadClientKey(_) => {
            "its selected user's `client-key`/`client-key-data` could not be loaded \
             (unreadable file, or invalid base64)"
        }
        KubeconfigError::ParseCertificates(_) => "its PEM-encoded certificates could not be parsed",
        KubeconfigError::ReadConfig(..) => "it could not be read",
        KubeconfigError::FindPath => "no kubeconfig path could be determined",
    }
}

/// Fixed explanation of why a Kubernetes client could not be built from a
/// config that was itself derived from kubeconfig material.
///
/// `kube::Error`'s TLS variants wrap the very certificate and key bytes the
/// config was built from, so this never formats the error.
pub const CLIENT_BUILD_FAILURE: &str = "its credentials could not be turned into a working Kubernetes client (check that \
     `client-certificate-data`/`client-key-data` are valid PEM and that the \
     `certificate-authority` is a certificate)";

/// Fixed explanation for `Config::infer()`'s failure.
///
/// `InferConfigError`'s `Display` embeds a [`KubeconfigError`] for whatever
/// ambient kubeconfig it tried, so it is never formatted either. The fixed
/// text names the three places `infer` looks, which is the whole diagnostic
/// value the formatted error would have carried.
pub const INFER_FAILURE: &str = "no Kubernetes configuration could be inferred: this process is not running in-cluster \
     (no service-account token at /var/run/secrets/kubernetes.io/serviceaccount), $KUBECONFIG \
     is unset or unusable, and there is no readable ~/.kube/config. Set \
     `qa-environments.argo.kubeconfig_path` to a kubeconfig for the Argo cluster";

/// Walk an error's cause chain, unwrapping a [`std::io::Error`]'s boxed
/// payload along the way.
///
/// `std::error::Error::source` does *not* do this on its own: an
/// `io::Error` built from a custom payload (`io::Error::new`/`io::Error::other`)
/// returns `None` from `.source()` regardless of what it wraps — the payload
/// is reachable only through `io::Error::get_ref`. Measured directly against
/// `kube` 3.1.0 + `hyper-util` 0.1 + `hyper-rustls` 0.27 + `tokio-rustls`
/// 0.26 (the exact stack `kube`'s `rustls-tls` feature resolves to here): a
/// real TLS certificate failure reaches the caller as
/// `kube::Error::Service` → `hyper_util::client::legacy::Error` →
/// `io::Error` → `io::Error` → `rustls::Error`, with three of those four
/// hops being an `io::Error` whose payload plain `.source()` chaining would
/// silently drop. Skipping this unwrap does not make the classification
/// safer — it just makes it wrong, by never finding the `rustls::Error`
/// that is actually there.
fn causes<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> impl Iterator<Item = &'a (dyn std::error::Error + 'static)> {
    let mut next: Option<&'a (dyn std::error::Error + 'static)> = Some(error);
    std::iter::from_fn(move || {
        let current = next.take()?;
        let io_payload: Option<&'a (dyn std::error::Error + 'static)> = current
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            .map(|inner| inner as &(dyn std::error::Error + 'static));
        next = io_payload.or_else(|| current.source());
        Some(current)
    })
}

/// Whether `error`'s cause chain contains a `rustls::Error` — i.e. the TLS
/// handshake itself failed, which is how both a SAN/hostname mismatch and a
/// missing or wrong CA surface (both are `rustls::Error::InvalidCertificate`,
/// just different [`rustls::CertificateError`] variants; this module does
/// not need to tell those two apart to point at both fixes at once).
///
/// This is a fact about the error's *type*, verified by actually failing a
/// handshake against a real, deliberately SAN-mismatched self-signed
/// certificate in this module's tests — not a guess about which variant a
/// given failure "should" produce.
fn is_tls_certificate_failure(error: &kube::Error) -> bool {
    causes(error).any(|cause| cause.downcast_ref::<rustls::Error>().is_some())
}

/// Whether `error`'s cause chain contains a
/// [`hyper_util::client::legacy::Error`] for which
/// [`hyper_util::client::legacy::Error::is_connect`] is true — i.e. the
/// failure happened in `Connect` (DNS, TCP, or the TLS handshake it wraps),
/// as opposed to after a connection was already established.
///
/// `is_connect` alone cannot tell a plain TCP failure (refused, timed out,
/// unresolvable) apart from a TLS failure — both set `ErrorKind::Connect`,
/// confirmed by driving a real connection-refused case and a real
/// SAN-mismatch case through this same client stack and observing both
/// report `is_connect() == true`. Callers must check
/// [`is_tls_certificate_failure`] *first*, and only fall back to this for
/// the plain-transport bucket.
fn is_connect_failure(error: &kube::Error) -> bool {
    causes(error).any(|cause| {
        cause
            .downcast_ref::<hyper_util::client::legacy::Error>()
            .is_some_and(hyper_util::client::legacy::Error::is_connect)
    })
}

/// Fixed explanation for a connect-stage failure that is not a TLS/certificate
/// problem: the transport never got as far as a handshake.
const COULD_NOT_CONNECT: &str = "the API server could not be reached (connection timed out, was refused, or the address \
     could not be resolved); the underlying error is not reported here because it can carry \
     the kubeconfig's own credentials";

/// Fixed explanation for a TLS/certificate verification failure.
///
/// Points at the two things that actually cause this, in order of how often
/// they turn out to be it: the address the kubeconfig connects to (an IP,
/// most often) may simply not be one of the names the cluster's certificate
/// lists, which is not a sign that self-signed certificates are unsupported —
/// only that the kubeconfig should ask for a listed name instead via
/// `tls-server-name`. Failing that, the kubeconfig's own
/// `certificate-authority`/`certificate-authority-data` may not be the CA
/// that actually signed the cluster's certificate.
const TLS_CERTIFICATE_FAILURE: &str = "the connection to the API server failed a TLS certificate check; either the address the \
     kubeconfig connects to is not listed in the cluster certificate's Subject Alternative \
     Names (set `tls-server-name` in the kubeconfig's cluster block to a name the certificate \
     does list), or the kubeconfig's `certificate-authority`/`certificate-authority-data` is \
     not the CA that signed the cluster's certificate; the underlying error is not reported \
     here because it can carry the kubeconfig's own credentials";

/// Fixed explanation for every other non-`Api` failure: reached a connection
/// (or never got as far as attempting one), but for a reason that is neither
/// a TLS/certificate failure nor a plain connect failure — most commonly the
/// exec credential plugin path (`AuthError::AuthExecRun`, reachable through
/// both `kube::Error::Auth` and boxed inside `kube::Error::Service`), whose
/// `Display` prints the plugin's whole stdout/stderr.
const OTHER_TRANSPORT_OR_CREDENTIAL_FAILURE: &str = "the request to the API server failed in a way that was neither a TLS/certificate problem \
     nor a plain connection failure; the underlying error is not reported here because it can \
     carry the kubeconfig's own credentials";

/// Describe a failed API call without formatting anything that could be
/// derived from the kubeconfig used to make it.
///
/// See this module's header for why [`kube::Error::Api`] is the one variant
/// whose payload is interpolated (it is the server's own words) and why every
/// other variant is not.
///
/// A rejection by the server on bad credentials (401/403) needs no separate
/// branch here: `kube-client` folds *every* HTTP client/server-error
/// response — parsed as a `Status` or not — into `Error::Api`
/// (`handle_api_errors` in `kube_client::client`), so it already arrives
/// with the server's own status message and code via the branch below.
///
/// Every other variant is classified by the *type* found in its cause chain
/// ([`is_tls_certificate_failure`], [`is_connect_failure`]), not by matching
/// on `kube::Error`'s message text — a `kube` or TLS-stack upgrade can change
/// wording without silently reclassifying anything here.
pub fn describe_kube_error(error: &kube::Error) -> String {
    match error {
        kube::Error::Api(status) => {
            format!(
                "the API server refused the request: {} (HTTP {})",
                status.message, status.code
            )
        }
        other if is_tls_certificate_failure(other) => TLS_CERTIFICATE_FAILURE.to_owned(),
        other if is_connect_failure(other) => COULD_NOT_CONNECT.to_owned(),
        _ => OTHER_TRANSPORT_OR_CREDENTIAL_FAILURE.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canary is the whole point: every string this module can return is
    /// a `&'static str` written *in this file*, so it is impossible for one
    /// to carry input. This test pins that by feeding a real error built
    /// from canary-bearing input and asserting the classification is
    /// content-free — the same assertion the callers make, made once here on
    /// the primitive.
    const CANARY: &str = "CANARY-9d41b7-kubeconfig-error-classifier-do-not-echo-me";

    #[test]
    fn a_parse_failure_over_canary_bearing_input_is_classified_not_echoed() {
        // Unbalanced bracket: a YAML syntax error, not a shape error.
        let text = format!("not: [valid, {CANARY}");
        let error = kube::config::Kubeconfig::from_yaml(&text)
            .expect_err("this is not valid YAML and must not parse");
        let described = describe_kubeconfig_error(&error);
        assert!(
            !described.contains(CANARY),
            "the classification must not carry the input, got: {described}"
        );
        assert_eq!(described, "it is not valid YAML");
    }

    /// The measured C1 case: a document that is one scalar. serde reports
    /// `invalid type: string "<the whole document>"`, and it is that quoted
    /// scalar which reached the browser.
    #[test]
    fn a_wrong_shape_failure_over_canary_bearing_input_is_classified_not_echoed() {
        let text =
            format!("-----BEGIN EC PRIVATE KEY-----\\n{CANARY}\\n-----END EC PRIVATE KEY-----");
        let error = kube::config::Kubeconfig::from_yaml(&text)
            .expect_err("a bare scalar is not a Kubeconfig and must not parse");
        // Proof that the upstream error really does carry the input, so this
        // test cannot pass by classifying an error that was harmless anyway.
        assert!(
            error.to_string().contains(CANARY),
            "the upstream error is expected to echo its input - if it no longer does, this \
             test has stopped proving anything and should be re-derived, not deleted"
        );
        let described = describe_kubeconfig_error(&error);
        assert!(
            !described.contains(CANARY),
            "the classification must not carry the input, got: {described}"
        );
        assert!(
            described.contains("does not have a kubeconfig's shape"),
            "an operator must still be told what is wrong, got: {described}"
        );
    }

    /// The one variant whose payload is interpolated, and why that is safe:
    /// the text comes from the API server, not from the kubeconfig.
    #[test]
    fn an_api_status_keeps_the_servers_own_words() {
        let error = kube::Error::Api(
            kube::core::Status::failure("namespaces \"virtuozzo\" not found", "NotFound")
                .with_code(404)
                .boxed(),
        );
        let described = describe_kube_error(&error);
        assert!(
            described.contains("namespaces \"virtuozzo\" not found"),
            "legacy's fixable-platform message must survive, got: {described}"
        );
        assert!(
            described.contains("404"),
            "the status code must survive, got: {described}"
        );
    }

    /// A generic boxed failure (no `hyper_util`/`rustls` type anywhere in its
    /// chain) must fall all the way through to the catch-all — proving the
    /// two new, more specific branches do NOT fire on just anything that
    /// happens to arrive via `Service`. `Service` is also the variant that
    /// can box an `AuthError::AuthExecRun`, whose `Display` prints an exec
    /// credential plugin's whole stdout/stderr, so the canary here doubles
    /// as that regression check.
    #[test]
    fn a_transport_or_credential_failure_is_fixed_text() {
        let error = kube::Error::Service(Box::new(std::io::Error::other(CANARY)));
        assert!(
            !is_tls_certificate_failure(&error),
            "a plain io::Error must not be misclassified as a TLS failure"
        );
        assert!(
            !is_connect_failure(&error),
            "a plain io::Error must not be misclassified as a connect failure"
        );
        let described = describe_kube_error(&error);
        assert!(
            !described.contains(CANARY),
            "a boxed inner error must never be formatted, got: {described}"
        );
        assert_eq!(described, OTHER_TRANSPORT_OR_CREDENTIAL_FAILURE);
    }

    /// The measured defect: a real refused TCP connection (nothing listens
    /// on the port) must classify as "could not connect", not the old single
    /// bucket and not the TLS bucket. Real, not synthetic, because
    /// `hyper_util::client::legacy::Error` has no public constructor — the
    /// only way to get a genuine one is to make a real connection attempt
    /// that really fails that way.
    #[tokio::test]
    async fn a_refused_connection_is_classified_as_could_not_connect() {
        // Bind then drop: the port is free but nothing is listening on it,
        // so the very next connect attempt is refused.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding a loopback port for this test");
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let yaml = format!(
            "apiVersion: v1\n\
             kind: Config\n\
             clusters:\n\
             - name: c\n  \
               cluster:\n    \
                 server: https://127.0.0.1:{port}\n    \
                 insecure-skip-tls-verify: true\n\
             contexts:\n\
             - name: c\n  \
               context:\n    \
                 cluster: c\n    \
                 user: u\n\
             current-context: c\n\
             users:\n\
             - name: u\n  \
               user:\n    \
                 token: {CANARY}\n"
        );
        let kubeconfig =
            kube::config::Kubeconfig::from_yaml(&yaml).expect("well-formed test kubeconfig");
        let config = kube::Config::from_custom_kubeconfig(
            kubeconfig,
            &kube::config::KubeConfigOptions::default(),
        )
        .await
        .expect("building a Config from a well-formed kubeconfig");
        let client = kube::Client::try_from(config).expect("building a Client from that Config");

        let request = http::Request::builder()
            .uri(format!("https://127.0.0.1:{port}/api"))
            .body(kube::client::Body::empty())
            .unwrap();
        let error = client
            .send(request)
            .await
            .expect_err("nothing listens on this port; the connection must be refused");

        // Proof this really is a connect-stage refusal and not some other
        // failure this test would then be vacuously classifying.
        assert!(
            format!("{error:?}").contains("ConnectionRefused")
                || format!("{error:?}").contains("Connect"),
            "expected a real connection-refused error, got: {error:?}"
        );
        assert!(
            is_connect_failure(&error),
            "a refused connection must be recognised as a connect failure, got: {error:?}"
        );
        assert!(
            !is_tls_certificate_failure(&error),
            "a refused TCP connection never reaches a TLS handshake, got: {error:?}"
        );

        let described = describe_kube_error(&error);
        assert!(
            !described.contains(CANARY),
            "the token in the kubeconfig must never be echoed, got: {described}"
        );
        assert_eq!(described, COULD_NOT_CONNECT);
    }

    /// The defect the product owner actually hit: a certificate whose SAN
    /// list does not include the address the kubeconfig connects to, with a
    /// CA that verifies fine. Must classify as the TLS/certificate bucket —
    /// not the old single "connection, TLS, or credential failure" bucket,
    /// which is what led them to conclude self-signed certificates were
    /// unsupported.
    ///
    /// Drives a real self-signed TLS server whose certificate deliberately
    /// omits the loopback address, so `rustls` really does fail the
    /// handshake for the SAN reason — `rustls::Error` has no public
    /// constructor either, so this is the only way to produce a genuine one.
    #[tokio::test]
    async fn a_certificate_san_mismatch_is_classified_as_tls_certificate_failure() {
        use rcgen::{CertificateParams, KeyPair, SanType};

        // A cert valid for "not-the-loopback-address.invalid" only - never
        // for 127.0.0.1, which is what the client below actually connects to.
        let mut params =
            CertificateParams::new(vec!["not-the-loopback-address.invalid".to_owned()])
                .expect("building cert params");
        params.subject_alt_names = vec![SanType::DnsName(
            "not-the-loopback-address.invalid".try_into().unwrap(),
        )];
        let key_pair = KeyPair::generate().expect("generating a test key pair");
        let cert = params
            .self_signed(&key_pair)
            .expect("self-signing the test cert");
        let ca_pem = cert.pem();
        let cert_der = cert.der().clone();
        let key_der = key_pair.serialize_der();

        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert_der],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
            )
            .expect("building a test TLS ServerConfig");
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding a loopback port for this test");
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    // The client is expected to abort the handshake once it
                    // rejects our certificate, so a failed accept is normal.
                    if let Err(handshake_error) = acceptor.accept(stream).await {
                        tracing::debug!(%handshake_error, "test TLS server: accept failed (expected)");
                    }
                });
            }
        });

        // The client trusts this exact CA - so if the handshake still
        // fails, it is provably the SAN check and nothing else (a missing
        // or wrong CA would be a different, also-true, cause; here there
        // isn't one).
        let yaml = format!(
            "apiVersion: v1\n\
             kind: Config\n\
             clusters:\n\
             - name: c\n  \
               cluster:\n    \
                 server: https://127.0.0.1:{port}\n    \
                 certificate-authority-data: {ca_b64}\n\
             contexts:\n\
             - name: c\n  \
               context:\n    \
                 cluster: c\n    \
                 user: u\n\
             current-context: c\n\
             users:\n\
             - name: u\n  \
               user:\n    \
                 token: {CANARY}\n",
            ca_b64 = {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(&ca_pem)
            }
        );
        let kubeconfig =
            kube::config::Kubeconfig::from_yaml(&yaml).expect("well-formed test kubeconfig");
        let config = kube::Config::from_custom_kubeconfig(
            kubeconfig,
            &kube::config::KubeConfigOptions::default(),
        )
        .await
        .expect("building a Config from a well-formed kubeconfig");
        let client = kube::Client::try_from(config).expect("building a Client from that Config");

        let request = http::Request::builder()
            .uri(format!("https://127.0.0.1:{port}/api"))
            .body(kube::client::Body::empty())
            .unwrap();
        let error = client
            .send(request)
            .await
            .expect_err("the certificate's SAN list excludes 127.0.0.1; the handshake must fail");

        // Proof this really is the SAN check failing, not some other error
        // this test would then be vacuously classifying.
        let debug = format!("{error:?}");
        assert!(
            debug.contains("NotValidForName") || debug.contains("InvalidCertificate"),
            "expected a real certificate-verification failure, got: {debug}"
        );
        assert!(
            is_tls_certificate_failure(&error),
            "a real SAN mismatch must be recognised as a TLS certificate failure, got: {debug}"
        );

        let described = describe_kube_error(&error);
        assert!(
            !described.contains(CANARY),
            "the token in the kubeconfig must never be echoed, got: {described}"
        );
        assert!(
            !described.contains("127.0.0.1") && !described.contains("not-the-loopback-address"),
            "the classification must not carry addresses from the underlying rustls error, \
             got: {described}"
        );
        assert_eq!(described, TLS_CERTIFICATE_FAILURE);
    }
}
