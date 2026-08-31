//! `PlatformsService::run_observation_cycle` tests (Task 8's ticker body),
//! against a REAL in-memory `SQLite` database and real test doubles for
//! credstore and the observer — the same tier `platforms_kubeconfig_tests`
//! and `platforms_tests` use, for the same reasons.
//!
//! Also covers Addition 1 (per-platform `VPADM_NAMESPACE` resolution): both
//! branches of [`crate::domain::service::platforms::pick_vpadm_namespace`]'s
//! precedence, exercised end-to-end through `observe_platform` rather than as
//! a bare unit test of the pure function, so the PEP-scoped variable read
//! (`vpadm_namespace_for`) is what is actually proven wired in.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use qa_environments_sdk::{KubeconfigMaterial, NewPlatform, NewVariable, PlatformPatch};
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

use crate::domain::observation::DetectedPlatform;
use crate::domain::ports::{ObservationOutcome, PlatformObserver};
use crate::test_support::{
    FailingSecretObserver, KeyedObserver, NamespaceRecordingObserver, RecordingCredStore,
    RecordingSecretObserver, build_services_tenant_scoped_with_credstore,
    build_services_tenant_scoped_with_observer, ctx, inmem_db,
};

fn pasted(name: &str, document: &str) -> NewPlatform {
    NewPlatform {
        name: name.to_owned(),
        product_id: None,
        description: None,
        kubeconfig_credstore_ref: None,
        kubeconfig: Some(KubeconfigMaterial::new(document.to_owned())),
        default_branch: None,
        is_default: false,
    }
}

fn by_reference(name: &str, credstore_ref: &str) -> NewPlatform {
    NewPlatform {
        name: name.to_owned(),
        product_id: None,
        description: None,
        kubeconfig_credstore_ref: Some(credstore_ref.to_owned()),
        kubeconfig: None,
        default_branch: None,
        is_default: false,
    }
}

fn detected(version: &str) -> ObservationOutcome {
    ObservationOutcome::Detected(DetectedPlatform {
        version: version.to_owned(),
        build: None,
        raw: version.to_owned(),
        namespace: "virtuozzo".to_owned(),
        base_domain: None,
    })
}

// ---------------------------------------------------------------------------
// One platform's failure must never abort the cycle for the others
// ---------------------------------------------------------------------------

/// A platform whose kubeconfig cannot be resolved must not stop the cycle from
/// reaching the others. `report.attempted == 3` is the load-bearing assertion:
/// if the loop had returned early on the broken platform, this would be less
/// than 3 regardless of where in the (unordered) list that platform landed.
///
/// # Why the counts changed on 2026-08-29
///
/// This test used to assert `failed == 1` / `observed == 2`, because an
/// unresolvable kubeconfig made `observe_platform` return a genuine `Err`
/// *before* any `ObservationOutcome` existed — and nothing was persisted, so
/// the platform read "not yet observed" forever while the reason lived only in
/// a log line. Cluster-health spec section 8; measured on the remote, where
/// seven of eight platforms sat in exactly that state.
///
/// The resolution failure is now folded into the outcome instead, so all three
/// platforms complete a pass and the broken one's reason is persisted where an
/// operator can read it. That makes it behave exactly like a *detection*
/// failure, which has always been a value rather than an error — so `observed`
/// here means "the cycle completed a pass over this platform", which is what it
/// already meant for every failed detection.
///
/// `a_platform_whose_kubeconfig_cannot_be_resolved_says_so_on_its_row` below is
/// what pins the surfacing itself. The cycle-does-not-abort property is still
/// asserted here, by `attempted`.
#[tokio::test]
async fn one_platforms_failure_does_not_abort_the_cycle_for_the_others() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services =
        build_services_tenant_scoped_with_credstore(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    services
        .platforms
        .create_platform(&ctx(tenant), pasted("platform-a", "kubeconfig-a"))
        .await
        .unwrap();
    // Points at a credstore reference nothing ever seeded: `observe_platform`
    // fails to resolve the material and returns a genuine `Err`, before any
    // `ObservationOutcome` is even constructed.
    services
        .platforms
        .create_platform(
            &ctx(tenant),
            by_reference("platform-broken", "never-seeded-in-credstore"),
        )
        .await
        .unwrap();
    services
        .platforms
        .create_platform(&ctx(tenant), pasted("platform-c", "kubeconfig-c"))
        .await
        .unwrap();

    let report = services.platforms.run_observation_cycle().await;

    assert_eq!(
        report.attempted, 3,
        "the cycle must reach every platform regardless of where the broken one sits"
    );
    assert_eq!(
        report.failed, 0,
        "an unresolvable kubeconfig is now recorded as a failed observation, not an error"
    );
    assert_eq!(
        report.observed, 3,
        "every platform completes a pass, the broken one carrying its reason on the row"
    );
}

/// The surfacing this whole change exists for, and the leak it must not cause.
///
/// Two assertions carry the weight:
///
/// * the row says *why*, so the platform page stops reading "not yet observed"
///   for a platform nobody can observe; and
/// * the message does **not** contain the credstore reference. The `DomainError`
///   this replaces names the platform and its `credstore_ref` inline, and
///   `credstore_ref` is the one value banned
///   from `PlatformDto` — under `SharingMode::Tenant` the reference *is* a read
///   path to the kubeconfig. Formatting that error into `version_detect_error`
///   would publish it by the back door to every GET-authorized caller.
///
/// The cluster columns must also stay untouched: nothing contacted a cluster,
/// so claiming `Unreachable` would be the same fabrication the final
/// whole-branch review caught on the feature-off build.
#[tokio::test]
async fn a_platform_whose_kubeconfig_cannot_be_resolved_says_so_on_its_row() {
    const REF: &str = "never-seeded-canary-reference";

    let credstore = Arc::new(RecordingCredStore::new());
    let services =
        build_services_tenant_scoped_with_credstore(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let broken = services
        .platforms
        .create_platform(&ctx(tenant), by_reference("platform-broken", REF))
        .await
        .unwrap();

    services.platforms.run_observation_cycle().await;

    let row = services
        .platforms
        .get_platform(&ctx(tenant), broken.id)
        .await
        .expect("the platform must still be readable");

    let message = row
        .version_detect_error
        .as_deref()
        .expect("an unresolvable kubeconfig must be recorded, not swallowed into a log line");
    assert!(
        message.contains("credential store"),
        "an operator must be told what is wrong, got: {message}"
    );
    assert!(
        !message.contains(REF),
        "the credstore reference must never reach a published column, got: {message}"
    );
    assert!(
        row.version_detected_at.is_some(),
        "the attempt is still an attempt and must be stamped"
    );
    assert!(
        row.cluster.is_none(),
        "nothing contacted a cluster, so no cluster status may be claimed"
    );
}

/// The brief's own illustrative scenario: three platforms, the middle one's
/// *observer* reports `Failed`. Per `observe_platform`'s documented contract
/// a detection failure is a **value**, not an error — so all three still come
/// back `Ok` from `observe_platform`, and this test's job is to prove each
/// one's own outcome (not some other platform's) is what actually landed in
/// its row. `KeyedObserver` keys on the kubeconfig content specifically so
/// this holds regardless of the order `run_observation_cycle` visits them in.
#[tokio::test]
async fn the_middle_platforms_failed_detection_still_lets_the_outer_two_be_recorded() {
    let observer = Arc::new(KeyedObserver::new());
    observer.script("kubeconfig-a", detected("1.2.3"));
    observer.script(
        "kubeconfig-b",
        ObservationOutcome::Failed("namespace \"virtuozzo\" not found".to_owned()),
    );
    observer.script("kubeconfig-c", detected("4.5.6"));

    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn PlatformObserver>,
    );
    let tenant = Uuid::new_v4();

    let a = services
        .platforms
        .create_platform(&ctx(tenant), pasted("platform-a", "kubeconfig-a"))
        .await
        .unwrap();
    let b = services
        .platforms
        .create_platform(&ctx(tenant), pasted("platform-b", "kubeconfig-b"))
        .await
        .unwrap();
    let c = services
        .platforms
        .create_platform(&ctx(tenant), pasted("platform-c", "kubeconfig-c"))
        .await
        .unwrap();

    let report = services.platforms.run_observation_cycle().await;

    assert_eq!(report.attempted, 3);
    assert_eq!(
        report.observed, 3,
        "a Failed *outcome* is still a successfully-persisted Ok, matching \
         observe_platform's own documented contract"
    );
    assert_eq!(report.failed, 0);

    let refreshed_a = services
        .platforms
        .get_platform(&ctx(tenant), a.id)
        .await
        .unwrap();
    assert_eq!(refreshed_a.observed_version.as_deref(), Some("1.2.3"));
    assert!(refreshed_a.version_detect_error.is_none());

    let refreshed_b = services
        .platforms
        .get_platform(&ctx(tenant), b.id)
        .await
        .unwrap();
    assert_eq!(
        refreshed_b.version_detect_error.as_deref(),
        Some("namespace \"virtuozzo\" not found")
    );

    let refreshed_c = services
        .platforms
        .get_platform(&ctx(tenant), c.id)
        .await
        .unwrap();
    assert_eq!(refreshed_c.observed_version.as_deref(), Some("4.5.6"));
    assert!(refreshed_c.version_detect_error.is_none());
}

// ---------------------------------------------------------------------------
// Addition 1: per-platform VPADM_NAMESPACE resolution
// ---------------------------------------------------------------------------

/// An explicit, non-blank `VPADM_NAMESPACE` variable on the platform must win
/// over the `virtuozzo` default.
#[tokio::test]
async fn an_explicit_vpadm_namespace_override_wins() {
    let observer = Arc::new(NamespaceRecordingObserver::new(detected("1.0.0")));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn PlatformObserver>,
    );
    let tenant = Uuid::new_v4();

    let platform = services
        .platforms
        .create_platform(&ctx(tenant), pasted("platform-a", "kubeconfig-a"))
        .await
        .unwrap();

    services
        .variables
        .upsert(
            &ctx(tenant),
            NewVariable {
                platform_id: Some(platform.id),
                name: "VPADM_NAMESPACE".to_owned(),
                value: "  custom-ns  ".to_owned(),
            },
        )
        .await
        .unwrap();

    services
        .platforms
        .observe_platform(&ctx(tenant), platform.id)
        .await
        .unwrap();

    assert_eq!(
        observer.last_namespace().as_deref(),
        Some("custom-ns"),
        "the explicit override must win, trimmed"
    );
}

/// No `VPADM_NAMESPACE` variable at all falls back to `virtuozzo`.
#[tokio::test]
async fn the_default_namespace_applies_when_no_variable_is_set() {
    let observer = Arc::new(NamespaceRecordingObserver::new(detected("1.0.0")));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn PlatformObserver>,
    );
    let tenant = Uuid::new_v4();

    let platform = services
        .platforms
        .create_platform(&ctx(tenant), pasted("platform-a", "kubeconfig-a"))
        .await
        .unwrap();

    services
        .platforms
        .observe_platform(&ctx(tenant), platform.id)
        .await
        .unwrap();

    assert_eq!(observer.last_namespace().as_deref(), Some("virtuozzo"));
}

/// A `VPADM_NAMESPACE` variable that exists but is blank (or only
/// whitespace) does not count as "set" any more than an absent one does —
/// mirrors legacy's `vpadm_namespace_override`, which returns `None` (not
/// `Some("")`) for a blank value.
#[tokio::test]
async fn a_blank_vpadm_namespace_variable_falls_back_to_the_default_too() {
    let observer = Arc::new(NamespaceRecordingObserver::new(detected("1.0.0")));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn PlatformObserver>,
    );
    let tenant = Uuid::new_v4();

    let platform = services
        .platforms
        .create_platform(&ctx(tenant), pasted("platform-a", "kubeconfig-a"))
        .await
        .unwrap();

    services
        .variables
        .upsert(
            &ctx(tenant),
            NewVariable {
                platform_id: Some(platform.id),
                name: "VPADM_NAMESPACE".to_owned(),
                value: "   ".to_owned(),
            },
        )
        .await
        .unwrap();

    services
        .platforms
        .observe_platform(&ctx(tenant), platform.id)
        .await
        .unwrap();

    assert_eq!(observer.last_namespace().as_deref(), Some("virtuozzo"));
}

// ---------------------------------------------------------------------------
// Addition 2: a 409 from the self-heal names the platform and the fix
// ---------------------------------------------------------------------------

/// A shared writer that appends every byte `tracing-subscriber` emits into
/// one buffer. Copied from `infra::observer::kube_observer`'s test module
/// (see its own doc for why a raw buffer is used over `tracing-test`).
#[derive(Clone)]
struct RawBuffer(Arc<Mutex<Vec<u8>>>);

impl RawBuffer {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for RawBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for RawBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The self-heal step's failure must reach the log **with the platform
/// attached**, and must carry whatever operator guidance the observer's own
/// error text already has (`infra::observer::secret_writer::describe_apply_failure`
/// builds that text for a real `409`; this test only proves the ticker's
/// logging does not throw the platform's identity away on the way to the
/// log — see `PlatformsService::self_heal_kubeconfig_secret`'s own doc for the
/// full argument).
#[tokio::test]
async fn a_self_heal_failure_is_logged_with_the_platform_attached() {
    let conflict_message = "secret/qa-platform-x in namespace argo already exists and is not \
                             owned by this writer's field manager (\"qa-environments\"), so \
                             server-side apply was refused with a 409 Conflict; delete it so \
                             qa-environments can take ownership on the next cycle";
    let observer = Arc::new(FailingSecretObserver::new(conflict_message));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        observer as Arc<dyn PlatformObserver>,
    );
    let tenant = Uuid::new_v4();

    let buffer = RawBuffer::new();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_ansi(false)
        .finish();

    // A thread-local default, held across the `.await` below — `#[tokio::test]`
    // defaults to a current-thread runtime, so the whole cycle runs on this
    // one thread and the guard sees everything it logs.
    let guard = tracing::subscriber::set_default(subscriber);

    let platform = services
        .platforms
        .create_platform(
            &ctx(tenant),
            pasted("platform-with-handmade-secret", "kubeconfig-x"),
        )
        .await
        .unwrap();

    let _report = services.platforms.run_observation_cycle().await;
    drop(guard);

    let log = buffer.contents();
    assert!(
        log.contains("platform-with-handmade-secret"),
        "the self-heal failure must name the platform, not read as an anonymous repeating \
         error: {log}"
    );
    assert!(
        log.contains(&platform.id.to_string()),
        "must also carry the platform id: {log}"
    );
    assert!(
        log.contains("409 Conflict"),
        "must surface the observer's own operator guidance rather than swallowing it: {log}"
    );
}

// ---------------------------------------------------------------------------
// Decision D4 on create and on update, not only as a self-heal (final review I1)
// ---------------------------------------------------------------------------

/// Until 2026-08-28 `ensure_kubeconfig_secret` had exactly one production
/// caller — the ticker's self-heal — so a platform created through the UI had
/// no runner `Secret` until the next cycle (default `poll_interval_seconds:
/// 300`, floor 60). That gap IS the ~5-minute `FailedMount` window D4 exists
/// to eliminate, and the spec (§4.6) said "on create, on update, and as a
/// self-heal" the whole time.
///
/// The assertion is at the port because there is nowhere else it could be:
/// the `Secret` is written into the **Argo** cluster, so it appears in no
/// row, no DTO and no response.
#[tokio::test]
async fn creating_a_platform_materialises_its_runner_secret_immediately() {
    let observer = Arc::new(RecordingSecretObserver::new(detected("26.5")));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn PlatformObserver>,
    );
    let tenant = Uuid::new_v4();

    let platform = services
        .platforms
        .create_platform(&ctx(tenant), pasted("created-in-the-ui", "kubeconfig-alpha"))
        .await
        .unwrap();

    let writes = observer.secret_writes();
    assert_eq!(
        writes.len(),
        1,
        "create must write the runner Secret exactly once, not wait for the ticker: {writes:?}"
    );
    assert_eq!(
        writes[0].0, platform.kubeconfig_credstore_ref,
        "the Secret must be keyed on the reference the row ended up with, so its derived name \
         matches what qa-runs' executor mounts"
    );
    assert_eq!(
        writes[0].1,
        b"kubeconfig-alpha".to_vec(),
        "and must carry the material that was actually stored, not a placeholder"
    );
}

/// A registration that names an existing credstore reference instead of
/// pasting a document is the same situation from the runner's point of view:
/// the pod still mounts a `Secret` derived from that reference, and it still
/// has to exist before the first run.
#[tokio::test]
async fn creating_a_platform_by_reference_also_materialises_its_runner_secret() {
    let existing = "team-a-prod-cluster";
    let credstore = Arc::new(RecordingCredStore::new());
    credstore.seed(existing, "kubeconfig-by-reference");

    let observer = Arc::new(RecordingSecretObserver::new(detected("26.5")));
    let services = crate::test_support::build_services_full(
        inmem_db().await,
        Arc::new(crate::test_support::TenantScopedAuthZ),
        Arc::clone(&credstore) as Arc<dyn credstore_sdk::CredStoreClientV1>,
        Arc::clone(&observer) as Arc<dyn PlatformObserver>,
        crate::config::QaEnvironmentsConfig::default().max_variables,
    );
    let tenant = Uuid::new_v4();

    services
        .platforms
        .create_platform(&ctx(tenant), by_reference("registered-by-reference", existing))
        .await
        .unwrap();

    let writes = observer.secret_writes();
    assert_eq!(writes.len(), 1, "a reference-registered platform needs the Secret too: {writes:?}");
    assert_eq!(writes[0].0, existing);
    assert_eq!(writes[0].1, b"kubeconfig-by-reference".to_vec());
}

/// Rotating a kubeconfig without re-applying the `Secret` is worse than never
/// having written one: the runner mounts the OLD material happily and then
/// fails against the platform's cluster, which reads as a broken platform
/// rather than as a stale credential.
#[tokio::test]
async fn replacing_a_platforms_kubeconfig_rewrites_its_runner_secret() {
    let observer = Arc::new(RecordingSecretObserver::new(detected("26.5")));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn PlatformObserver>,
    );
    let tenant = Uuid::new_v4();

    let platform = services
        .platforms
        .create_platform(&ctx(tenant), pasted("rotating-platform", "kubeconfig-old"))
        .await
        .unwrap();

    let updated = services
        .platforms
        .update_platform(
            &ctx(tenant),
            platform.id,
            PlatformPatch {
                kubeconfig: Some(KubeconfigMaterial::new("kubeconfig-new".to_owned())),
                ..PlatformPatch::default()
            },
        )
        .await
        .unwrap();

    let writes = observer.secret_writes();
    assert_eq!(writes.len(), 2, "create wrote one, the rotation must write the second: {writes:?}");
    assert_eq!(
        writes[1].0, updated.kubeconfig_credstore_ref,
        "the second write must use the NEW reference the row now carries"
    );
    assert_eq!(
        writes[1].1,
        b"kubeconfig-new".to_vec(),
        "and the NEW material - a Secret still holding the old bytes is the failure this exists \
         to prevent"
    );
}

/// The narrow half of the same rule: a patch that does not mention the
/// kubeconfig changes neither the `Secret`'s name (derived from the credstore
/// reference) nor its contents, so re-applying it would be a pointless
/// round-trip into another cluster on every rename.
#[tokio::test]
async fn a_patch_that_does_not_touch_the_kubeconfig_writes_no_secret() {
    let observer = Arc::new(RecordingSecretObserver::new(detected("26.5")));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn PlatformObserver>,
    );
    let tenant = Uuid::new_v4();

    let platform = services
        .platforms
        .create_platform(&ctx(tenant), pasted("renamed-platform", "kubeconfig-alpha"))
        .await
        .unwrap();

    services
        .platforms
        .update_platform(
            &ctx(tenant),
            platform.id,
            PlatformPatch {
                name: Some("renamed-platform-v2".to_owned()),
                ..PlatformPatch::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        observer.secret_writes().len(),
        1,
        "only the create's write; a rename must not re-apply the Secret"
    );
}

/// The `Secret` write reaches a **different cluster** than the row it belongs
/// to, so it is the one step in create that can fail for reasons that say
/// nothing about the request. A platform whose row exists and whose `Secret`
/// is missing is recoverable — the ticker re-applies it next cycle — while
/// one that silently vanished is not, so the failure is logged and the create
/// still succeeds.
#[tokio::test]
async fn a_failed_secret_write_does_not_roll_back_the_create() {
    let observer = Arc::new(FailingSecretObserver::new(
        "secret/qa-platform-x in namespace argo already exists ... 409 Conflict",
    ));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        observer as Arc<dyn PlatformObserver>,
    );
    let tenant = Uuid::new_v4();

    let buffer = RawBuffer::new();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let created = services
        .platforms
        .create_platform(&ctx(tenant), pasted("secret-write-fails", "kubeconfig-alpha"))
        .await;
    drop(guard);

    let platform = created.expect("a failed Secret write must not fail the create");

    // And the row really is there to be found afterwards.
    let fetched = services.platforms.get_platform(&ctx(tenant), platform.id).await.unwrap();
    assert_eq!(fetched.id, platform.id);

    let log = buffer.contents();
    assert!(
        log.contains("secret-write-fails"),
        "the swallowed failure must still name the platform: {log}"
    );
    assert!(
        log.contains("409 Conflict"),
        "and must carry the writer's own operator guidance rather than swallowing it: {log}"
    );
    assert!(
        log.contains("create"),
        "and must say which of the three call sites it came from: {log}"
    );
}
