//! Reading a live VHI cluster over SSH: `/etc/hci-release` for the version
//! and build, the admin API's own loopback probe for reachability, and
//! `vinfra node list` for the node count.
//!
//! # Only step 1 is fatal
//!
//! A node whose about document does not answer, or whose `vinfra` refuses,
//! still yields a [`qa_product_sdk::observation::ObservationOutcome::Detected`]
//! carrying the version and build read off `/etc/hci-release` -- with
//! `baseUrl`, `storageName` or `nodeCount` simply **omitted**, never
//! defaulted and never blank. Only a missing or unparseable release file
//! makes the whole observation `Failed`: that is the one read every other
//! fact in this module depends on, and an operator who lost `nodeCount`
//! because `vinfra` rejected a stale password should not also lose the
//! version this node otherwise proved it has.
//!
//! # Health travels separately
//!
//! There is no `health_check` here, deliberately: the trait's method takes no
//! [`EnvironmentHandle`], so it has nothing to probe, and VHI leaves its
//! default (`Ok`) in place the same way VHP does. A VHI environment's health
//! is [`health_from`]'s reading of `RELEASE_COMMAND` alone, carried in
//! [`qa_product_sdk::observation::PluginObservation::health`]. It fails
//! *independently* of the environment half -- a node whose release file just
//! became unparseable is still alive -- but a node whose release file is
//! simply absent is classified identically on both channels, via the same
//! [`exec_release_command`] both call.
//!
//! # Why `/etc/hci-release` and not the admin API's own version
//!
//! See [`parse_release`]'s own doc -- the two numbers this module could read
//! disagree, and only one of them is the product's.

use qa_connector_ssh::{SshFailure, SshSession, SshTarget, classify};
use qa_product_sdk::observation::{
    FailureClass, HealthOutcome, HealthState, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation,
};
use qa_product_sdk::plugin::EnvironmentHandle;

use crate::config::{config_port, config_str};
use crate::schemas::{
    BASE_URL_KEY, BUILD_KEY, DEFAULT_SSH_USER, DEFAULT_VINFRA_USERNAME, NODE_COUNT_KEY,
    NODE_HOST_KEY, PRODUCT_VERSION_KEY, RAW_RELEASE_KEY, SSH_PRIVATE_KEY_KEY, SSH_USER_KEY,
    STORAGE_NAME_KEY, VINFRA_PASSWORD_KEY, VINFRA_USERNAME_KEY,
};

/// Fixed failure text -- see this module's `PluginFailure::detail` invariant
/// (never built from a runtime byte).
const NODE_HOST_NOT_CONFIGURED: &str =
    "this environment declares no management node address, so there is nothing to reach";
const KEY_NOT_RESOLVED: &str =
    "this environment's SSH key could not be read from the credential store";
const PASSWORD_NOT_RESOLVED: &str =
    "this environment's vinfra password could not be read from the credential store";
/// The one `read_node_count` failure whose remedy is re-registering a stored
/// value rather than looking at the cluster -- so it is named, not folded into
/// the classified-reason warn every other failure of that step gets.
///
/// It is also where the two halves of this contract meet: `SshSession`
/// refuses these bytes here, and the pytest suite's `lib/vinfra.py` raises on
/// the same bytes rather than silently truncating them, so one stored password
/// cannot be accepted by one side and refused by the other.
const VINFRA_PASSWORD_HAS_A_LINE_BREAK: &str = "this environment's stored vinfra password contains a line break, so it cannot be delivered \
     to the remote command intact: re-register it without one";
/// Not literally true -- see [`exec_release_command`]'s doc -- but it is the
/// fixed text every non-zero exit of `RELEASE_COMMAND` reads as, on both the
/// environment and the health channel.
const NOT_A_VHI_NODE: &str =
    "/etc/hci-release is absent on this node, so it is not a VHI installation";
const RELEASE_UNPARSEABLE: &str =
    "/etc/hci-release did not have the expected shape; its raw text is recorded as rawRelease";

/// Step 1. `cat` rather than a shell test, so an absent file is a non-zero
/// exit the classifier can see rather than empty output it cannot.
const RELEASE_COMMAND: &str = "cat /etc/hci-release";
/// Step 2, on the node's own loopback -- legacy's readiness probe
/// (`hcilib3-py3/lib/hcilib/util/util.py:1005`).
const ABOUT_COMMAND: &str = "curl -ks https://localhost:8888/api/v2/about";
/// Step 3. `VINFRA_PORTAL` and `VINFRA_USERNAME` are prefixed by the caller;
/// `VINFRA_PASSWORD` arrives on stdin.
const NODE_LIST_COMMAND: &str = "vinfra node list -f json";
/// The admin panel's port. `hcilib/subsystems/subsystems.py:1044` builds
/// `https://{host}:8888` and nothing in VHI makes it configurable.
const ADMIN_PORT: u16 = 8888;
/// The scheme every VHI base URL is built with, matching the admin panel's
/// own `https://{host}:8888`.
const BASE_URL_SCHEME: &str = "https://";
/// The field the about document reports the storage cluster's name under.
/// Not to be confused with [`STORAGE_NAME_KEY`], which is the *observed
/// attribute's* key -- this is the raw JSON field on the remote's answer.
const ABOUT_STORAGE_NAME_FIELD: &str = "storage-name";
/// The variable [`SshSession::exec_with_secret_env`] delivers the vinfra
/// password under.
const VINFRA_PASSWORD_VAR: &str = "VINFRA_PASSWORD";

/// What `/etc/hci-release` says, decomposed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub build: String,
}

/// Parse `Virtuozzo Infrastructure 7.4.0 (82)` -- its **first line** only, so
/// a multi-line `cat` of a file that happens to have a well-formed first line
/// is not defeated by whatever parenthesised junk a later line contains.
///
/// # Why this file and not the API
///
/// The backend's `/api/v2/about` also reports a version and a release, and on
/// the live stand they were `7.4.0` and **56** -- the same version, a different
/// number, because `storage-release.release` is the *storage component's*
/// release rather than the product's. Legacy's own version detection reads
/// this file (`hcilib3-py3/lib/hcilib/util/product_info.py:42`, `get_version`
/// at `:83`), so a build number shown on an environment page and a build
/// number legacy gates on are the same number only if it comes from here.
/// Both measured on 2026-09-08.
///
/// # What counts as a version
///
/// At least two dot-separated components, each non-empty and all ASCII
/// digits: `7.4.0` and `7.4.0.1` parse, but `7.4.`, `.`, `beta.rc` and
/// `7.4.0-rc1` do not. Anything looser would let `productVersion` -- which
/// claims [`qa_product_sdk::descriptor::FieldRole::Version`] and becomes the
/// platform's `observed_version` column and a grouping key downstream --
/// carry text no human or query would recognise as a version, which is worse
/// than the `Malformed` failure this parser exists to produce.
///
/// Returns `None` for anything without such a version followed by a
/// parenthesised all-digit build; the caller turns that into
/// `FailureClass::Malformed` and still records the raw line, on the failure's
/// `remote_message` -- see [`exec_release_command`]'s doc for why not on the
/// observed attribute of the same name.
#[must_use]
pub fn parse_release(line: &str) -> Option<Release> {
    // `.lines().next()` rather than the whole string: `rfind` below anchors on
    // the *last* `(`/`)` in whatever it is given, so scanning multiple lines
    // would let a well-formed first line be defeated by an unrelated
    // parenthesis two lines later. An empty `line` has zero lines, so this
    // falls back to `line` itself -- still empty, and still `None` below.
    let first_line = line.lines().next().unwrap_or(line);
    let trimmed = first_line.trim();
    let open = trimmed.rfind('(')?;
    let close = trimmed.rfind(')')?;
    if close < open {
        return None;
    }
    let build = trimmed.get(open + 1..close)?.trim();
    if build.is_empty() || !build.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let version = trimmed
        .get(..open)?
        .split_whitespace()
        .next_back()?
        .to_owned();
    if !is_dotted_numeric_version(&version) {
        return None;
    }
    Some(Release {
        version,
        build: build.to_owned(),
    })
}

/// At least two dot-separated components, each non-empty and all ASCII
/// digits. See [`parse_release`]'s doc for why this is stricter than "contains
/// a dot".
fn is_dotted_numeric_version(version: &str) -> bool {
    let mut components = version.split('.');
    let Some(first) = components.next() else {
        return false;
    };
    if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let mut component_count = 1_usize;
    for component in components {
        if component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        component_count += 1;
    }
    component_count >= 2
}

/// How many nodes `vinfra node list -f json` reported.
///
/// `None` when the output is not a JSON array -- which is what a rejected
/// authorisation looks like, since `vinfra` writes its diagnostic and exits
/// non-zero rather than printing an empty list.
#[must_use]
pub fn node_count(output: &str) -> Option<usize> {
    let parsed: serde_json::Value = serde_json::from_str(output).ok()?;
    parsed.as_array().map(Vec::len)
}

/// Observe the environment this handle points at: its detected attributes
/// and its health, in one call so **one agent** serves both.
///
/// One agent, not one handshake: every [`SshSession::exec`] below spawns its
/// own `ssh`, so this function performs four separate handshakes against the
/// node. What is shared -- and what makes doing both halves here worth it --
/// is the [`SshSession`], and therefore the `ssh-agent` process holding the
/// decrypted key: starting one costs three child processes and up to 40
/// blocking seconds (see [`open_session`]), which is the expensive part, and
/// it is paid once.
///
/// A **value**, never an error: both halves of a failed observation are
/// persisted and shown, because an operator who can see why it failed can fix
/// it and one who sees a blank page cannot.
pub async fn observe(env: &EnvironmentHandle<'_>) -> PluginObservation {
    let (session, host) = match open_session(env).await {
        Ok(pair) => pair,
        Err(failure) => return both_failed(failure),
    };

    // `host` comes straight out of `open_session`'s own successful return,
    // not re-derived from `env.config` -- so there is no second place that
    // could disagree with it, or default it, if it were ever somehow absent.
    let target = ObserveTarget {
        host,
        vinfra_username: config_str(env.config, VINFRA_USERNAME_KEY)
            .unwrap_or(DEFAULT_VINFRA_USERNAME),
    };

    let environment = observe_environment(
        &session,
        &target,
        env,
        RELEASE_COMMAND,
        ABOUT_COMMAND,
        NODE_LIST_COMMAND,
    )
    .await;
    // Independent of the above, deliberately: a node whose release file is
    // unreadable can still answer a health probe, and a cluster that refuses
    // vinfra can still report its version.
    let health = health_from(&session, RELEASE_COMMAND).await;

    PluginObservation {
        environment,
        health,
    }
}

/// Read the configuration and credential material `observe` needs, start a
/// session with it, and hand back the host it used -- so a caller building
/// [`ObserveTarget`] gets it from the one place that already proved it
/// present and non-blank, rather than re-deriving (and having to re-default)
/// it from `env.config` a second time.
///
/// A missing host or an unresolved key is [`FailureClass::Internal`], not
/// [`FailureClass::Unreachable`]: nothing was reached and nothing was read,
/// so the environment's own configuration is what is unusable, not the
/// network.
///
/// # Why this is `async` when nothing here reaches the network
///
/// Starting the session is the blocking part. `SshAgent::start_with_key`
/// spawns three synchronous children and polls two of them with
/// `std::thread::sleep`, for up to 40 uncancellable seconds
/// (`qa_connector_ssh::agent`'s own header). `observe` is awaited by
/// `qa-environments` with no platform timeout around it, on the observation
/// ticker *and* on `POST /environments/{id}/refresh`, so doing that on the
/// tokio worker would stall every other task on that thread and outrun the
/// gear's `stop_timeout`. [`SshSession::open_on_blocking_pool`] is the
/// connector's own wrapper for exactly this, and it is what makes the two
/// cheap pre-flight checks above worth doing first: neither costs a thread.
async fn open_session<'a>(
    env: &EnvironmentHandle<'a>,
) -> Result<(SshSession, &'a str), PluginFailure> {
    let host = config_str(env.config, NODE_HOST_KEY).ok_or_else(|| {
        PluginFailure::classified(FailureClass::Internal, NODE_HOST_NOT_CONFIGURED)
    })?;
    let user = config_str(env.config, SSH_USER_KEY).unwrap_or(DEFAULT_SSH_USER);
    let port = config_port(env.config);
    let key = env
        .resolved(SSH_PRIVATE_KEY_KEY)
        .ok_or_else(|| PluginFailure::classified(FailureClass::Internal, KEY_NOT_RESOLVED))?;

    let target = SshTarget {
        host: host.to_owned(),
        port,
        user: user.to_owned(),
    };
    let session = SshSession::open_on_blocking_pool(target, key)
        .await
        .map_err(|failure| classify(&failure))?;
    Ok((session, host))
}

/// Non-secret configuration [`observe_environment`]'s reads need, resolved
/// once by [`observe`] rather than re-derived by each of [`read_about`] and
/// [`read_node_count`].
struct ObserveTarget<'a> {
    host: &'a str,
    vinfra_username: &'a str,
}

/// The environment half of an observation: the three reads of spec §5, in
/// order.
///
/// Split out of [`observe`] so it can be driven against a real `sshd` fixture
/// without stored credential material -- [`observe`] builds its own session
/// from resolved secrets, while the reads under test here only need a host
/// that answers. `release_command`, `about_command` and `node_list_command`
/// are parameters rather than the free constants they are in production for
/// exactly that reason: a test points `release_command` at a path it
/// controls. `env` is still threaded through (rather than resolving
/// `vinfra_password` into a parameter here) so this file never has to name
/// `credstore_sdk::SecretValue`.
async fn observe_environment(
    session: &SshSession,
    target: &ObserveTarget<'_>,
    env: &EnvironmentHandle<'_>,
    release_command: &str,
    about_command: &str,
    node_list_command: &str,
) -> ObservationOutcome {
    let (release, raw) = match read_release(session, release_command).await {
        Ok(pair) => pair,
        Err(failure) => return ObservationOutcome::Failed(failure),
    };

    let mut attrs = ObservedAttrs::default();
    attrs.set(PRODUCT_VERSION_KEY, release.version);
    attrs.set(BUILD_KEY, release.build);
    attrs.set(RAW_RELEASE_KEY, raw);

    if let Some(about) = read_about(session, about_command, target.host).await {
        attrs.set(BASE_URL_KEY, about.base_url);
        if let Some(storage_name) = about.storage_name {
            attrs.set(STORAGE_NAME_KEY, storage_name);
        }
    }

    if let Some(count) = read_node_count(session, target, env, node_list_command).await {
        attrs.set(NODE_COUNT_KEY, count.to_string());
    }

    ObservationOutcome::Detected(attrs)
}

/// A cheap liveness read, independent of [`observe_environment`]: whether
/// `release_command` still answers over this session. A parameter (like
/// [`observe_environment`]'s three commands) so a test can drive it without a
/// real `/etc/hci-release`, and so it can be asserted against
/// [`read_release`]'s own classification of the identical command.
///
/// Fails independently of the environment half for anything [`parse_release`]
/// would reject -- a node whose release file just became unparseable is still
/// alive, and this function never parses its output -- but agrees with
/// [`read_release`] on a command that does not run at all, since both go
/// through [`exec_release_command`].
async fn health_from(session: &SshSession, release_command: &str) -> HealthOutcome {
    match exec_release_command(session, release_command).await {
        Ok(_) => HealthOutcome::Checked {
            state: HealthState::Ok,
            detail: None,
        },
        Err(failure) => HealthOutcome::Failed(failure),
    }
}

/// A failure that predates either reading: report it on both channels.
fn both_failed(failure: PluginFailure) -> PluginObservation {
    PluginObservation {
        environment: ObservationOutcome::Failed(failure.clone()),
        health: HealthOutcome::Failed(failure),
    }
}

/// Run a command whose only expected failure mode is "the thing being read is
/// not there", classifying accordingly -- shared by [`read_release`] and
/// [`health_from`] so a node with no release file is not reported as
/// `NotFound` on one channel and `Internal` on the other for the exact same
/// underlying read.
///
/// # Why `NotFound`, and why the doc above says it is not literally true
///
/// `ssh`/`sshd` do not distinguish "the remote command exited non-zero
/// because the file is absent" from "...because of a permission error", "...
/// because a directory sits where the file should be", or "...because `cat`
/// itself is missing from `PATH`". All four produce the identical shape this
/// function sees -- [`SshFailure::CommandFailed`] -- and all four currently
/// read as [`NOT_A_VHI_NODE`]. That is a real loss of precision, accepted
/// because the alternative (routing this exit through
/// [`qa_connector_ssh::classify`], which maps `CommandFailed` to
/// `FailureClass::Internal`) is wrong in the overwhelmingly common case -- an
/// absent file -- and because the remote's own diagnostic still reaches
/// `remote_message` regardless of which of the four actually happened, so a
/// human reading the persisted failure is not left guessing.
///
/// # Errors
///
/// [`FailureClass::NotFound`] on a non-zero exit, carrying the remote's own
/// stderr as `remote_message`; whatever [`qa_connector_ssh::classify`] chose
/// for any other transport failure.
async fn exec_release_command(
    session: &SshSession,
    release_command: &str,
) -> Result<String, PluginFailure> {
    session.exec(release_command).await.map_err(|failure| {
        if let SshFailure::CommandFailed { stderr, .. } = failure {
            PluginFailure::classified(FailureClass::NotFound, NOT_A_VHI_NODE)
                .with_remote_message(stderr)
        } else {
            classify(&failure)
        }
    })
}

/// Step 1: read and parse `/etc/hci-release`, returning both the decomposed
/// [`Release`] and the raw (trimmed) line -- the caller records the raw line
/// as `rawRelease` on success, and this function's own `Malformed` failure
/// carries it as `remote_message`, the one sanctioned carrier for text a
/// remote sent back.
///
/// # Errors
///
/// See [`exec_release_command`] for the `NotFound` case; `Malformed` when the
/// command exits zero but [`parse_release`] cannot make sense of the output.
async fn read_release(
    session: &SshSession,
    release_command: &str,
) -> Result<(Release, String), PluginFailure> {
    let output = exec_release_command(session, release_command).await?;
    // `cat`'s trailing newline is not part of what the node "said", and this
    // trimmed value is both parsed and persisted (as `rawRelease` or as a
    // failure's `remote_message`), so trimming once here keeps both in sync.
    let output = output.trim().to_owned();

    match parse_release(&output) {
        Some(release) => Ok((release, output)),
        None => Err(
            PluginFailure::classified(FailureClass::Malformed, RELEASE_UNPARSEABLE)
                .with_remote_message(output),
        ),
    }
}

/// What [`read_about`] found: the admin panel's own base URL and,
/// best-effort, the storage cluster's name.
struct AboutResult {
    base_url: String,
    storage_name: Option<String>,
}

/// Step 2: confirm the admin API answers on the node's own loopback.
///
/// Never a failure -- an about document that does not answer is
/// inconclusive, not an error, and must not turn a good release read into a
/// failed observation. `None` omits `baseUrl` entirely rather than writing it
/// blank, and is logged at `warn` with the classified reason, the same
/// non-fatal-read logging `qa-vhp-product-plugin`'s own `observe.rs` uses for
/// `gateway_hosts`.
///
/// `curl -ks` exits `0` on an HTTP 404 or 500 -- it is not `curl -f` -- so an
/// exit alone does not "confirm reachable" the way this module's header
/// claims. The body must additionally parse as JSON before `baseUrl` is set;
/// an exit-zero response that is not JSON (an nginx error page, an empty
/// body) is treated the same as a failed exec.
async fn read_about(session: &SshSession, about_command: &str, host: &str) -> Option<AboutResult> {
    let output = match session.exec(about_command).await {
        Ok(output) => output,
        Err(failure) => {
            tracing::warn!(
                reason = %classify(&failure),
                "admin API did not answer; baseUrl omitted"
            );
            return None;
        }
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&output) else {
        tracing::warn!("admin API answered with an unparseable body; baseUrl omitted");
        return None;
    };
    Some(AboutResult {
        base_url: format!("{BASE_URL_SCHEME}{host}:{ADMIN_PORT}"),
        storage_name: storage_name_from(&parsed),
    })
}

/// Pull `storage-name` out of the about document's already-parsed JSON,
/// best-effort.
fn storage_name_from(parsed: &serde_json::Value) -> Option<String> {
    parsed
        .get(ABOUT_STORAGE_NAME_FIELD)?
        .as_str()
        .map(ToOwned::to_owned)
}

/// Step 3: count the nodes `vinfra node list -f json` reports.
///
/// Never a failure, for the same reason as [`read_about`]: a cluster that
/// refuses `vinfra` (a stale password, a locked-out user) still answered
/// step 1 and may still answer step 2, and folding this read's failure into
/// the whole observation would throw both away. An unresolved password, or a
/// `vinfra` that itself fails, is logged at `warn` with its classified reason
/// rather than surfaced to the platform.
///
/// `target.host` and `target.vinfra_username` are shell-quoted before being
/// formatted into the remote command: both are operator-authored
/// configuration, not credential material, but they still reach a root shell
/// on the target, and a `node_host` containing `;` or a backtick must not be
/// able to run anything there.
async fn read_node_count(
    session: &SshSession,
    target: &ObserveTarget<'_>,
    env: &EnvironmentHandle<'_>,
    node_list_command: &str,
) -> Option<usize> {
    let Some(password) = env.resolved(VINFRA_PASSWORD_KEY) else {
        tracing::warn!(reason = PASSWORD_NOT_RESOLVED, "vinfra node count omitted");
        return None;
    };

    let prefixed = format!(
        "export VINFRA_PORTAL={host} VINFRA_USERNAME={username}; {node_list_command}",
        host = shell_quote(target.host),
        username = shell_quote(target.vinfra_username),
    );
    match session
        .exec_with_secret_env(VINFRA_PASSWORD_VAR, password, &prefixed)
        .await
    {
        Ok(output) => node_count(&output),
        Err(failure) => {
            log_node_list_failure(&failure);
            None
        }
    }
}

/// Explain a failed step 3 at `warn`, which is the only place it is
/// recoverable from: the failure never reaches the observation (see this
/// module's header -- only step 1 is fatal), so an operator who cannot see
/// this line sees only a missing `nodeCount`.
///
/// The newline refusal is **named** rather than classified. It is about the
/// *stored* password rather than anything the cluster said, and an operator
/// reading "vinfra node list failed" would go looking at the cluster. Every
/// other failure carries the classifier's own reading, whose `Display`
/// appends the remote's own words -- the one text sanctioned to travel.
fn log_node_list_failure(failure: &SshFailure) {
    if matches!(failure, SshFailure::SecretNotDeliverable) {
        tracing::warn!(
            reason = VINFRA_PASSWORD_HAS_A_LINE_BREAK,
            "vinfra node count omitted"
        );
    } else {
        tracing::warn!(
            reason = %classify(failure),
            "vinfra node list failed; nodeCount omitted"
        );
    }
}

/// Single-quote `value` for a POSIX shell, escaping any embedded single quote
/// as `'\''` (close the quote, an escaped literal quote, reopen the quote).
/// Single quotes are the only POSIX quoting form with no special characters
/// to worry about *inside* them -- not `$`, not backticks, not `\` -- so this
/// is safe for arbitrary operator-authored text.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod observe_tests;
