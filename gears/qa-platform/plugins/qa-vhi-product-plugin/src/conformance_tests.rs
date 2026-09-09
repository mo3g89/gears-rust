//! The two gates every product plugin owes the platform.
//!
//! Layer 2 (`validate_schemas`) is a *boot* failure at registration, and
//! Layer 3 (`assert_no_leak`) is the harness that drives a plugin with planted
//! credential material. Both are run here so a bad schema and a leak are
//! caught by this crate's own `cargo test` rather than by a server that will
//! not start or a page that shows a private key.
//!
//! # Two secrets, and one shared canary
//!
//! VHI declares two secrets: `ssh_private_key`
//! ([`qa_product_sdk::descriptor::FieldKind::MultilineSecret`]) and
//! `vinfra_password` ([`qa_product_sdk::descriptor::FieldKind::Secret`]).
//! `Canary::vhp_shaped` already carries exactly those two *kinds* of
//! material — a PEM block and a bearer token — so both tests below build on
//! it rather than adding a `vhi_shaped` constructor. Nothing this crate does
//! reads either field for anything VHI-specific — no signature is checked,
//! no token format is parsed, `validate_credentials` only asks whether the
//! bytes are present and non-blank — so a domain-flavoured canary would
//! probe nothing a generic one does not already reach.
//!
//! # Reaching `observe`'s success path
//!
//! [`assert_no_leak`] synthesises its own `EnvironmentHandle` from the
//! plugin's declared schema, planting a fixed filler string
//! (`"canary-field-node_host"`) under every declared non-secret field by
//! default — which is exactly `node_host`, VHI's SSH target, and
//! `ssh_port`/`ssh_user` besides. VHP's equivalent drive never needed to
//! steer this: its one multiline-secret field, `kubeconfig`, *is* the
//! connection target (the API server's address lives inside the document
//! itself), so planting a real, working document through the harness's own
//! plant mechanism was already enough. VHI's target is three separate
//! plain-text fields the harness has no way to point anywhere in particular
//! — without help, `assert_no_leak` cannot drive VHI past
//! `open_session`'s network failure, which is a real gap: it would leave the
//! one drive that reaches `session.rs`'s and `agent.rs`'s byte-handling
//! paths never actually exercising them.
//!
//! [`Canary::with_config`] closes that gap: it lets a caller steer what
//! [`assert_no_leak`]'s own synthesised `config` carries under a declared
//! non-secret key, instead of the generic filler. Given real values for
//! `node_host`, `ssh_port` and `ssh_user` (pointed at a real
//! [`qa_connector_ssh::test_support::SshdFixture`] session) and the
//! fixture's own real client key as [`Canary::pem`] (a working key can't
//! simultaneously carry a chosen marker string and still authenticate),
//! [`the_vhi_plugin_leaks_nothing_when_a_session_actually_answers`] drives
//! the *same* [`assert_no_leak`] every other plugin's conformance suite
//! drives — inheriting its five-encoding marker scan, its itemised
//! surfaces, the `MountSpec::ConfigValue` secret surface a `Debug`-only
//! check could never see, and the references-only second drive with its
//! divergence check — rather than a hand-rolled reimplementation of any of
//! that.
//!
//! # Why this host, and how a host that isn't VHI-flavoured is handled
//!
//! `observe`'s three remote commands are fixed constants, not something a
//! test can redirect the way `qa-connector-k8s`'s stub API server lets
//! `qa-vhp-product-plugin` redirect a `ConfigMap` read — so `SshdFixture`'s
//! `sshd`, which runs real commands as the user running this test suite on
//! this test suite's own filesystem, only reaches `observe`'s success path
//! (`ObservationOutcome::Detected`) if *this host's own* `/etc/hci-release`
//! happens to parse. That is a real environment assumption, not a given one
//! -- `observe_tests.rs`'s own `observe_end_to_end_...` test states exactly
//! this ("whether the host this test happens to run on has a real
//! `/etc/hci-release` is deliberately not assumed either way"). So this
//! module reads and parses that file itself, *before* driving anything, and
//! skips loudly rather than failing when it is absent or unparseable — a
//! host that could never have supported this drive gets a skip, not a false
//! failure. Do not remove this check: without it, this test would fail on
//! any `sshd`-capable host that is not also shaped like a VHI node (a CI
//! image without this container's own `/etc/hci-release`, most laptops).
//! `curl`'s admin-API probe and `vinfra`'s node-list command are expected to
//! fail on such a host regardless (nothing listens on `:8888`, and `vinfra`
//! is not a working install) — by design, per [`crate::observe`]'s own doc:
//! only the release-file read is fatal, so `baseUrl` and `nodeCount` are
//! simply omitted rather than blocking `Detected`.

use credstore_sdk::SecretValue;
use qa_connector_ssh::test_support::SshdFixture;
use qa_product_sdk::descriptor::validate_schemas;
use qa_product_sdk::observation::ObservationOutcome;
use qa_product_sdk::plugin::{CredentialSlot, EnvironmentHandle, QaProductPluginV1};
use qa_product_sdk::testing::{Canary, assert_no_leak};

use super::VhiProductPlugin;
use crate::schemas::{NODE_HOST_KEY, SSH_PORT_KEY, SSH_PRIVATE_KEY_KEY, SSH_USER_KEY};

/// Layer 2, run here rather than discovered at boot.
///
/// `RegisteredPlugin::new` calls exactly this at registration and refuses the
/// plugin on failure, so a violation is a server that does not start.
#[test]
fn the_declared_schemas_pass_the_registration_check() {
    let plugin = VhiProductPlugin;

    validate_schemas(&plugin.credential_schema(), &plugin.observed_schema()).unwrap();
}

/// Layer 3 on the path where the SSH target is unusable: `node_host` is
/// filled with the harness's generic filler string, so `observe` fails
/// resolving/connecting to it and the surfaces this gates are the early
/// failure ones -- `validate_credentials`, the failed `observe`, and
/// `prepare_run_access`'s two drives (which never touch the network at all,
/// since [`crate::run::prepare_run_access`] reads only `env.config` and
/// `env.credstore_ref`). `Canary::vhp_shaped`'s PEM is deliberately
/// unparseable, so this drive additionally exercises VHI's key-parse-failure
/// branch under all five of [`assert_no_leak`]'s encodings.
///
/// See this module's header for why `Canary::vhp_shaped` is reused rather
/// than a VHI-flavoured constructor.
#[tokio::test]
async fn the_vhi_plugin_leaks_no_credential_material() {
    assert_no_leak(&VhiProductPlugin, &Canary::vhp_shaped()).await;
}

// ── Layer 3 again, this time against a session that actually answers ─────

/// Set to make a host that is not VHI-shaped a hard failure instead of a
/// skip. A CI image built to run this drive is expected to set it, so
/// "provably ran" replaces "assumed to have run".
const REQUIRE_VHI_HOST_VAR: &str = "QA_REQUIRE_VHI_HOST";

/// This host's own `/etc/hci-release`, read and validated *before* anything
/// is driven -- see this module's header for why a host that fails this
/// check must be skipped, loudly, rather than let the drive below fail.
///
/// # Why the skip needed a force switch
///
/// The whole-branch review of 2026-09-09 measured what this gate actually
/// costs: the drive below is the **only** test anywhere that reaches
/// `observe`'s success path, and it is the reason `Canary::with_config` was
/// added to the shared SDK -- and it returns silently on any host without a
/// parseable `/etc/hci-release`, which is this dev container and essentially
/// every CI runner. A gate no environment ever passes is a test that does not
/// exist, reported green.
///
/// The skip itself is still right for a workstation: a `panic!` there would
/// be punishing a laptop for not being a hyperconverged appliance, which is
/// the same argument `agent_tests`' `require_openssh` makes about
/// `openssh-client`. So this follows that module exactly -- an explicit local
/// skip, **or a hard failure** when [`REQUIRE_VHI_HOST_VAR`] is set. A
/// VHI-shaped CI image then becomes provable rather than assumed, and a CI
/// image that stops being VHI-shaped fails loudly instead of quietly
/// un-running this drive.
fn host_looks_like_a_vhi_node() -> bool {
    let looks_like_one = match std::fs::read_to_string("/etc/hci-release") {
        Ok(text) => crate::observe::parse_release(&text).is_some(),
        Err(_) => false,
    };

    assert!(
        looks_like_one || std::env::var_os(REQUIRE_VHI_HOST_VAR).is_none(),
        "this host's /etc/hci-release is absent or unparseable but {REQUIRE_VHI_HOST_VAR} is \
         set: this is the only drive that reaches observe's success path, and it MUST NOT be \
         skipped here"
    );
    looks_like_one
}

/// The drive that reaches `observe`'s success path: one [`assert_no_leak`]
/// call against a [`Canary`] built from [`Canary::vhp_shaped`] but with its
/// `pem` replaced by a real, working SSH key (so the session it is planted
/// under actually authenticates) and `node_host`/`ssh_port`/`ssh_user`
/// steered at a real [`SshdFixture`] session via [`Canary::with_config`].
///
/// Proves the fixture actually ran, rather than this whole drive silently
/// proving nothing, two ways: first, [`host_looks_like_a_vhi_node`] above,
/// which this test only proceeds past when it is `true`; second, an
/// independent `observe` call against the identical target *before* the
/// leak scan, asserting `ObservationOutcome::Detected` explicitly -- so a
/// regression that quietly moved this drive back onto an early-rejection
/// path (an unreachable host, a key `SshSession::open` refuses) fails loudly
/// here rather than passing a leak scan that reached nothing new.
#[tokio::test]
async fn the_vhi_plugin_leaks_nothing_when_a_session_actually_answers() {
    if !host_looks_like_a_vhi_node() {
        eprintln!(
            "skipping: this host's /etc/hci-release is absent or unparseable, so it cannot \
             prove observe's success path is reached (set {REQUIRE_VHI_HOST_VAR}=1 to make \
             this a failure; see this module's header)"
        );
        return;
    }

    let Some(fixture) = SshdFixture::start() else {
        return; // no sshd on this host; the fixture said so
    };

    let config = serde_json::json!({
        NODE_HOST_KEY: "127.0.0.1",
        SSH_PORT_KEY: fixture.port().to_string(),
        SSH_USER_KEY: fixture.user(),
    });
    let real_key = fixture.client_key_pem().to_owned();
    let slots = vec![CredentialSlot::resolved(
        SSH_PRIVATE_KEY_KEY,
        "qa/canary/ssh_private_key",
        SecretValue::from(real_key.clone()),
    )];
    let proof_env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };
    let proof = VhiProductPlugin.observe(&proof_env).await;
    let ObservationOutcome::Detected(_) = &proof.environment else {
        panic!(
            "expected this host's own /etc/hci-release to make this Detected -- got {:?}. \
             This drive exists specifically to reach observe's success path (see this module's \
             header); host_looks_like_a_vhi_node already checked the release file directly, so \
             this failure means something else about the session did not work.",
            proof.environment
        );
    };

    let mut canary = Canary::vhp_shaped();
    // The real key must authenticate, so it cannot also carry a chosen
    // marker string -- its own bytes are the material asserted absent from
    // every surface below, which is the more meaningful check anyway: this
    // is exactly what a resolved SSH key looks like in production.
    canary.pem = real_key;
    let canary = canary
        .with_config(NODE_HOST_KEY, "127.0.0.1")
        .with_config(SSH_PORT_KEY, fixture.port().to_string())
        .with_config(SSH_USER_KEY, fixture.user());

    assert_no_leak(&VhiProductPlugin, &canary).await;
}
