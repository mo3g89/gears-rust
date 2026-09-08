//! Test doubles for the Kubernetes API server, published behind the
//! `test-support` feature.
//!
//! # Why these are public API rather than `cfg(test)` scaffolding
//!
//! ADR-0001 puts `kube` and `k8s-openapi` in this crate and nowhere else, so a
//! product plugin cannot build the in-process `tower::Service` double this
//! crate's own tests use: doing so means naming `kube::Client::new` and
//! `kube::client::Body`. Before this module existed the consequence was
//! measured rather than argued — `find_configmap`,
//! [`KubeClient::read_configmap`] and `scan_configmaps` had **zero** coverage
//! workspace-wide, because the crate that owns the double had no reason to
//! call them and the crate with the reason had no way to build one.
//!
//! So the double is exported, and **no signature here names a `kube` type**.
//! A route table is `Fn(&StubRequest) -> (u16, Vec<u8>)`; a fixture body is
//! `Vec<u8>`; a kubeconfig is a `String`. What a caller gets back is a
//! [`KubeClient`], which is this crate's own boundary type.
//!
//! # Two doubles, because two different things need proving
//!
//! * [`KubeClient::from_routes`] answers requests from an in-process
//!   `tower::Service`. No socket, no handshake, no `from_kubeconfig` — the
//!   fastest way to drive a read and assert on what it sent and what it did
//!   with the answer.
//! * [`StubApiServer`] is a real loopback HTTP/1.1 listener, and
//!   [`StubApiServer::kubeconfig`] hands back a *valid* kubeconfig pointing at
//!   it. That is the one shape that lets a caller drive
//!   [`KubeClient::from_kubeconfig`] itself and have it **succeed** — which is
//!   what `qa-product-sdk`'s `assert_no_leak` needs in order to reach a
//!   plugin's success path at all, rather than failing at the client build and
//!   returning.
//!
//! # This module logs nothing, deliberately
//!
//! `assert_no_leak` scans every `tracing` event emitted while it drives a
//! plugin, and it installs a thread-local subscriber — which a task spawned on
//! a current-thread runtime (what `#[tokio::test]` gives) inherits. A stub
//! server that logged the requests it received would therefore put the
//! `Authorization` header it was sent into the very capture the harness is
//! asserting against, and turn a clean plugin into a failing one. Requests are
//! recorded in memory ([`StubApiServer::requests`]) instead.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Node};
use kube::Client;
use kube::core::{ListMeta, ObjectList, ObjectMeta, Status, TypeMeta};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::KubeClient;

/// One request a stub received, reduced to what a route table branches on.
///
/// `query` is kept apart from `path` because it is where a `list` call's
/// `labelSelector`/`fieldSelector` ride, and those are the part of
/// [`KubeClient::scan_configmaps`] that has to stay byte-identical to what
/// legacy sent. A route table that only saw the path could not assert on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubRequest {
    /// The HTTP method, uppercase (`GET`, `POST`, ...).
    pub method: String,
    /// The path, with no query string and no leading origin.
    pub path: String,
    /// The raw query string, exactly as it went on the wire (still
    /// percent-encoded), or `None` when there was none.
    pub query: Option<String>,
}

impl StubRequest {
    /// The request line as one string — `path` with `?query` when there is
    /// one. Convenient for a route table that wants to match both at once.
    #[must_use]
    pub fn target(&self) -> String {
        match &self.query {
            Some(query) => format!("{}?{}", self.path, query),
            None => self.path.clone(),
        }
    }

    /// One query parameter's value, still percent-encoded, or `None` when the
    /// request did not carry it.
    ///
    /// Percent-encoded on purpose: the API server reads what was *sent*, so a
    /// test that decoded first would pass on a selector that went out
    /// malformed. And returned rather than left to `query.contains(..)`,
    /// because a substring match is satisfied by a *prefix* — measured, not
    /// assumed: appending `-MUTATED` to the vpadm label selector left a
    /// `contains` assertion on it green.
    #[must_use]
    pub fn query_param(&self, name: &str) -> Option<&str> {
        let query = self.query.as_deref()?;
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then_some(value)
        })
    }

    fn from_parts(method: &str, target: &str) -> Self {
        let (path, query) = match target.split_once('?') {
            Some((path, query)) => (path.to_owned(), Some(query.to_owned())),
            None => (target.to_owned(), None),
        };
        Self {
            method: method.to_owned(),
            path,
            query,
        }
    }
}

/// A `ConfigMap` fixture, in the two facts a stub answer needs.
///
/// Serialised through `k8s-openapi`'s own type rather than hand-written JSON,
/// so a body a test serves is by construction a body
/// [`KubeClient::find_configmap`] can parse. Hand-written JSON drifts; this
/// cannot.
#[derive(Debug, Clone, Default)]
pub struct StubConfigMap {
    namespace: String,
    name: String,
    data: BTreeMap<String, String>,
}

impl StubConfigMap {
    /// A `ConfigMap` of this name in this namespace, with no data yet.
    #[must_use]
    pub fn new(namespace: &str, name: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            data: BTreeMap::new(),
        }
    }

    /// One `data` entry.
    #[must_use]
    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.data.insert(key.to_owned(), value.to_owned());
        self
    }

    /// Drop the `metadata.namespace` the API server would normally set.
    ///
    /// The shape [`KubeClient::scan_configmaps`]' `unwrap_or_default` exists
    /// for, and the one legacy records as `unknown`.
    #[must_use]
    pub fn without_namespace(mut self) -> Self {
        self.namespace = String::new();
        self
    }

    fn object(&self) -> ConfigMap {
        ConfigMap {
            metadata: ObjectMeta {
                name: Some(self.name.clone()),
                namespace: (!self.namespace.is_empty()).then(|| self.namespace.clone()),
                ..Default::default()
            },
            data: Some(self.data.clone()),
            ..Default::default()
        }
    }

    /// This `ConfigMap` as a `GET` response body.
    ///
    /// # Panics
    ///
    /// If the fixture cannot be serialised, which is a bug in this module
    /// rather than a case a caller can hit.
    #[must_use]
    pub fn body(&self) -> Vec<u8> {
        encode(&self.object())
    }

    /// These `ConfigMap`s as a `LIST` response body.
    ///
    /// # Panics
    ///
    /// If the fixture cannot be serialised, which is a bug in this module
    /// rather than a case a caller can hit.
    #[must_use]
    pub fn list_body(items: &[Self]) -> Vec<u8> {
        encode(&ObjectList {
            types: TypeMeta::list::<ConfigMap>(),
            metadata: ListMeta::default(),
            items: items.iter().map(Self::object).collect(),
        })
    }
}

/// The body the API server sends with a `404`: a `Status` object carrying
/// `reason: NotFound`, which is what `kube` reads to turn a failed `get` into
/// `Ok(None)`.
///
/// `message` is the API server's own words. It reaches
/// [`qa_product_sdk::observation::PluginFailure::remote_message`] on the
/// failure paths, which is the one sanctioned carrier for remote text — so a
/// caller planting a canary in it is asserting about that carrier, not about
/// the plugin.
///
/// # Panics
///
/// If the fixture cannot be serialised, which is a bug in this module.
#[must_use]
pub fn not_found_body(message: &str) -> Vec<u8> {
    encode(&Status::failure(message, "NotFound").with_code(404))
}

/// The body the API server sends with any other error status.
///
/// # Panics
///
/// If the fixture cannot be serialised, which is a bug in this module.
#[must_use]
pub fn api_error_body(code: u16, reason: &str, message: &str) -> Vec<u8> {
    encode(&Status::failure(message, reason).with_code(code))
}

/// The `NodeList` `read_cluster_health` asks for, with no nodes in it.
///
/// A cluster that lists no nodes is `Warning` — a *checked* health, not a
/// failure — which is everything a caller that only needs `node_health` to
/// succeed requires. A caller that asserts on node facts is in this crate and
/// builds its own `Node` fixtures.
///
/// # Panics
///
/// If the fixture cannot be serialised, which is a bug in this module.
#[must_use]
pub fn empty_node_list_body() -> Vec<u8> {
    encode(&ObjectList {
        types: TypeMeta::list::<Node>(),
        metadata: ListMeta::default(),
        items: Vec::<Node>::new(),
    })
}

/// The `NamespaceList` `read_cluster_health` asks for, with no namespaces in
/// it.
///
/// # Panics
///
/// If the fixture cannot be serialised, which is a bug in this module.
#[must_use]
pub fn empty_namespace_list_body() -> Vec<u8> {
    encode(&ObjectList {
        types: TypeMeta::list::<Namespace>(),
        metadata: ListMeta::default(),
        items: Vec::<Namespace>::new(),
    })
}

/// A kubeconfig that resolves, points at `server_url`, and authenticates with
/// `token`.
///
/// The one way to make [`KubeClient::from_kubeconfig`] *succeed* in a test.
/// `insecure-skip-tls-verify` is set so the document needs no CA and no
/// machine trust store to load — with an `http://` server nothing is
/// negotiated anyway, and the flag keeps the fixture from depending on
/// whatever certificates the host happens to have.
#[must_use]
pub fn kubeconfig_for(server_url: &str, token: &str) -> String {
    format!(
        "apiVersion: v1\n\
         kind: Config\n\
         clusters:\n\
         - name: stub\n\
         \x20 cluster:\n\
         \x20   server: {server_url}\n\
         \x20   insecure-skip-tls-verify: true\n\
         contexts:\n\
         - name: stub\n\
         \x20 context:\n\
         \x20   cluster: stub\n\
         \x20   user: stub\n\
         current-context: stub\n\
         users:\n\
         - name: stub\n\
         \x20 user:\n\
         \x20   token: {token}\n"
    )
}

/// `serde_json`, with the one failure mode spelled out.
///
/// `expect` rather than a returned `Result`: every value passed here is built
/// two lines above the call out of `String`s and `BTreeMap`s, so a failure is
/// a defect in this module and not something a test author can hand it.
#[allow(
    clippy::expect_used,
    reason = "a fixture that cannot serialise is a bug in this module, and a test that \
              cannot build its double has nothing useful to report but the panic"
)]
fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("serialising a stub API response fixture")
}

/// The status line's reason phrase, or a fixed placeholder for a code that
/// has none.
fn reason_phrase(code: u16) -> &'static str {
    http::StatusCode::from_u16(code)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Status")
}

impl KubeClient {
    /// A client answered by an in-process route table instead of a cluster.
    ///
    /// `routes` is called once per request and returns the HTTP status and
    /// body to answer with. Nothing is sent over a socket, nothing is
    /// negotiated, and no kubeconfig is involved — use [`StubApiServer`] when
    /// [`Self::from_kubeconfig`] itself has to run.
    ///
    /// The `Fn` is shared across the client's clones, so a table that records
    /// what it was asked (an `Arc<Mutex<Vec<StubRequest>>>` captured by the
    /// closure) sees every request the reads made.
    ///
    /// # Panics
    ///
    /// If `routes` returns a status code that is not a valid HTTP status, or
    /// if the response cannot be assembled — both defects in the route table
    /// rather than in the code under test.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "a route table that answers with an impossible status code is a test bug, \
                  and failing loudly at the stub is clearer than any error the client under \
                  test would report for it"
    )]
    pub fn from_routes<R>(routes: R) -> Self
    where
        R: Fn(&StubRequest) -> (u16, Vec<u8>) + Send + Sync + 'static,
    {
        let routes = Arc::new(routes);
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let routes = Arc::clone(&routes);
            async move {
                let stub = StubRequest::from_parts(
                    request.method().as_str(),
                    request
                        .uri()
                        .path_and_query()
                        .map_or_else(|| request.uri().path().to_owned(), ToString::to_string)
                        .as_str(),
                );
                let (status, body) = routes(&stub);
                let response = http::Response::builder()
                    .status(status)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(kube::client::Body::from(body))
                    .expect("building a stub HTTP response");
                Ok::<_, std::convert::Infallible>(response)
            }
        });
        Self::from_client(Client::new(service, "default"))
    }
}

/// A real HTTP/1.1 listener on loopback, answering from a route table.
///
/// Exists for exactly one reason [`KubeClient::from_routes`] cannot serve: a
/// caller that has to drive [`KubeClient::from_kubeconfig`] *itself* and have
/// it succeed, because the code under test builds its own client from stored
/// material. `qa-product-sdk`'s leak-conformance harness is that caller — it
/// hands a plugin a credential and calls its methods, so the only way to put
/// it on a plugin's success path is to make the credential it plants a working
/// kubeconfig.
///
/// It speaks the subset `kube`'s client actually uses: `GET` with no request
/// body, one response with `content-length`, persistent connections. It is not
/// a general-purpose HTTP server and must not grow into one.
///
/// Dropping it stops the listener.
#[derive(Debug)]
pub struct StubApiServer {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<StubRequest>>>,
    listening: tokio::task::JoinHandle<()>,
}

impl Drop for StubApiServer {
    fn drop(&mut self) {
        self.listening.abort();
    }
}

impl StubApiServer {
    /// Bind a loopback port and start answering from `routes`.
    ///
    /// # Panics
    ///
    /// If loopback cannot be bound, which no test can do anything about.
    #[allow(
        clippy::expect_used,
        reason = "a test that cannot bind loopback cannot run at all; the panic is the report"
    )]
    pub async fn start<R>(routes: R) -> Self
    where
        R: Fn(&StubRequest) -> (u16, Vec<u8>) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("binding a loopback port for the stub API server");
        let address = listener
            .local_addr()
            .expect("reading the stub API server's own address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let listening = tokio::spawn(accept_loop(
            listener,
            Arc::new(routes),
            Arc::clone(&requests),
        ));
        Self {
            address,
            requests,
            listening,
        }
    }

    /// The base URL a kubeconfig should point at.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// A valid kubeconfig for this server, authenticating with `token`.
    ///
    /// `token` is the caller's to choose so that a leak test can make it a
    /// canary: it is credential material this server is handed on every
    /// request, and therefore the thing a careless log line would echo.
    #[must_use]
    pub fn kubeconfig(&self, token: &str) -> String {
        kubeconfig_for(&self.base_url(), token)
    }

    /// Every request this server has answered, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<StubRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

type Routes = Arc<dyn Fn(&StubRequest) -> (u16, Vec<u8>) + Send + Sync>;

async fn accept_loop(
    listener: TcpListener,
    routes: Arc<impl Fn(&StubRequest) -> (u16, Vec<u8>) + Send + Sync + 'static>,
    requests: Arc<Mutex<Vec<StubRequest>>>,
) {
    let routes: Routes = routes;
    // One task per connection, not one at a time: `kube`'s client pools
    // connections and leaves them open, so a server that served a connection
    // to completion before accepting the next would deadlock the moment the
    // client opened a second one.
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(serve_connection(
            stream,
            Arc::clone(&routes),
            Arc::clone(&requests),
        ));
    }
}

/// Answer every request on one connection until the peer goes away.
///
/// Every request `kube` makes here is a `GET` with no body, so the bytes after
/// a request's header block are the start of the next request and never a body
/// this has to consume. A connection whose peer sends anything else is dropped
/// rather than guessed at.
async fn serve_connection(
    mut stream: TcpStream,
    routes: Routes,
    requests: Arc<Mutex<Vec<StubRequest>>>,
) {
    const HEADER_END: &[u8; 4] = b"\r\n\r\n";

    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let head_end = loop {
            if let Some(at) = position_of(&pending, HEADER_END) {
                break at;
            }
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read) => pending.extend_from_slice(&chunk[..read]),
            }
        };
        let head = String::from_utf8_lossy(&pending[..head_end]).into_owned();
        pending.drain(..head_end + HEADER_END.len());

        let Some(request) = parse_request_line(&head) else {
            return;
        };
        requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());

        let (status, body) = routes(&request);
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\n\
             content-type: application/json\r\n\
             content-length: {length}\r\n\
             \r\n",
            reason = reason_phrase(status),
            length = body.len(),
        );
        if stream.write_all(head.as_bytes()).await.is_err()
            || stream.write_all(&body).await.is_err()
        {
            return;
        }
    }
}

fn position_of(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_request_line(head: &str) -> Option<StubRequest> {
    let mut parts = head.lines().next()?.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    Some(StubRequest::from_parts(method, target))
}

/// A shared writer that appends every byte `tracing-subscriber` emits into
/// one buffer, independent of span names or field filters.
///
/// `tracing-test` (the usual choice in this workspace) is deliberately not
/// used for the leak assertions this exists for: it keeps only lines
/// containing the test's span name, so a multi-line value — exactly the shape
/// a leaked kubeconfig has — would survive capture only as its first line,
/// and a leak on any later line would read as "no leak". This buffer has no
/// such hole: it is everything `tracing` wrote, verbatim.
///
/// Public for the same reason the doubles above are: a product plugin
/// asserting that one of *its* log lines carries a classified reason rather
/// than a formatted error needs the same capture, and a second copy of this
/// reasoning in another crate is a second place for it to rot.
#[derive(Clone, Debug, Default)]
pub struct RawBuffer(Arc<Mutex<Vec<u8>>>);

impl RawBuffer {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything written so far, as text. Invalid UTF-8 is replaced rather
    /// than rejected: this is a leak assertion's input, and bytes that did
    /// not decode still have to be looked at.
    #[must_use]
    pub fn captured(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl std::io::Write for RawBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RawBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
