//! The run handlers driven over a **real** `ConcreteAppServices`.
//!
//! `#[path]`-included from `handlers::runs`, the shape
//! `handlers::schedules`'s `handler_tests` established, and for the same reason:
//! the
//! handlers are plain `async fn`s, so the extractors are built by hand and
//! everything from the handler body inward is exercised without a router or a
//! server.
//!
//! # What this module is for
//!
//! Two properties that only a driven handler can show.
//!
//! * **`stream_run_logs` reads before it subscribes**, and its span does not
//!   enumerate the replica's other runs. Both were documented and neither was
//!   executed; the second was a live cross-tenant disclosure.
//! * **A run endpoint attributes its refusals to the run resource.** The
//!   variant-to-status mapping is exhaustively checked by the compiler in
//!   `api::rest::error`; the endpoint-to-resource-type pairing is checked by
//!   nothing but tests like these, which is how the same defect was found in
//!   six places across three reviews. See `handlers::queue`'s `handler_tests` for
//!   the same loop over the queue surface.
use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use toolkit::api::canonical_prelude::IntoResponse;
use uuid::Uuid;

use super::{cancel_run, get_run, launch_run, rerun_run, stream_run_logs};
use crate::api::rest::dto::{BoundaryLimits, LaunchRunReq, RunTargetDto};
use crate::domain::service::test_support::{Fleet, ctx};
use crate::infra::ConcreteAppServices;
use crate::infra::logs::{MAX_RETAINED_RUNS, MAX_SUBSCRIBERS_PER_RUN, RunLogBroadcaster};

const TENANT: Uuid = Uuid::from_u128(0x0A11_0000_0000_0001);
const OTHER_TENANT: Uuid = Uuid::from_u128(0x0A11_0000_0000_0002);

/// The run resource type every run endpoint must attribute a refusal to.
const RUN_GTS: &str = "cf.qa.runs.run.v1~";

/// Render whatever a handler answered into `(status, body)`.
async fn rendered(response: axum::response::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn launch_payload() -> LaunchRunReq {
    LaunchRunReq {
        target: RunTargetDto {
            kind: "plan".to_owned(),
            repo_id: Some(Uuid::from_u128(0x0B01)),
            path: Some("plans/smoke.yaml".to_owned()),
            test_file: None,
            custom_plan_id: None,
            collect_url: None,
        },
        platform_id: None,
        branch: Some("main".to_owned()),
        include_tags: vec![],
        exclude_tags: vec![],
        parameters: vec![],
        exclusive: None,
        timeout_seconds: None,
    }
}

/// **A log-stream span must name only the run the caller addressed.**
///
/// `#[tracing::instrument]` records every argument it is not told to skip, and
/// `RunLogBroadcaster`'s derived `Debug` walks its channel map - so an
/// unskipped `logs` wrote every run id streaming on the replica, other tenants'
/// included, into a span the whole request inherits.
///
/// Break-verified: removing `logs` from the `skip(...)` list turns this red
/// with the foreign id inside
/// `stream_run_logs{logs=RunLogBroadcaster { channels: Mutex { data: {…} } }}`.
/// Asserted through the captured subscriber output rather than by reading the
/// attribute, because what leaks is what a subscriber writes down.
#[tokio::test]
#[tracing_test::traced_test]
async fn the_log_stream_span_names_only_the_run_that_was_addressed() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let mine = fleet.seed_run(TENANT, "mine").await;

    let logs = Arc::new(RunLogBroadcaster::new(8));
    // Another tenant's run, streaming on this same replica - which is the only
    // thing a shared broadcaster needs for the disclosure to be cross-tenant.
    let theirs = Uuid::from_u128(0x0BBB_0BBB_0BBB_0BBB);
    let _foreign = logs.subscribe(theirs).expect("under the per-run cap");

    let response = stream_run_logs(
        Extension(ctx(TENANT)),
        Extension(services),
        Extension(Arc::clone(&logs)),
        Path(mine.id),
    )
    .await;

    assert_eq!(response.status(), 200);
    assert!(
        logs_contain("live log subscription opened"),
        "premise: the handler must reach the subscribe path, or this asserts nothing"
    );
    assert!(
        !logs_contain(&theirs.to_string()),
        "a caller's own log request must not write another run's id into its span"
    );
}

/// **A finished run's log must be its output, not an empty stream.**
///
/// The behaviour this replaces was the reason run
/// `94978978-fa28-4650-a14d-2ce8f72dff49` read as having produced no logs at all:
/// its pod printed 179 lines, the executor adapter followed and ingested every
/// one (the seven collection-error rows it wrote are in `qa_run_test_results`,
/// and only the marker parser can have produced them), and this endpoint then
/// answered a reader with `futures::stream::empty()` because the run was
/// terminal — on the stated grounds that the output "is in the archived log",
/// which at that time nothing in this gear wrote.
///
/// **Tense corrected 2026-08-31.** That clause read "which nothing in this
/// gear writes", in the present tense, ~380 lines above this file's own
/// `qa_run_logs`-reading tests. An archive now exists and is written on every
/// ingested line; what remains true is the narrower fact this test covers —
/// a run with **no** archived row still has to be served from the retained
/// tail rather than from an empty stream, which is every run that finished
/// before the migration.
///
/// Driven through the real cancel path rather than by writing a terminal row, so
/// what is asserted is the endpoint's behaviour for a run the gear itself
/// finished. Both `publish` calls happen with nobody subscribed, which is the
/// case that used to lose everything.
#[tokio::test]
async fn a_finished_runs_log_is_served_from_the_retained_tail() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let run = fleet.seed_run(TENANT, "finished-run").await;

    let logs = Arc::new(RunLogBroadcaster::new(8));
    logs.publish(run.id, "[repo-x] collected 25 items / 7 errors".to_owned());
    logs.publish(run.id, "[repo-x] runner: pytest exit status 2".to_owned());
    assert_eq!(
        logs.active_channels(),
        0,
        "premise: nothing was watching, or this asserts the live path instead"
    );

    let _cancelled = cancel_run(
        Extension(ctx(TENANT)),
        Extension(Arc::clone(&services)),
        Path(run.id),
    )
    .await
    .expect("a created run must be cancellable")
    .into_response();

    let (status, body) = rendered(
        stream_run_logs(
            Extension(ctx(TENANT)),
            Extension(services),
            Extension(Arc::clone(&logs)),
            Path(run.id),
        )
        .await,
    )
    .await;

    assert_eq!(status, 200);
    assert!(
        body.contains("runner: pytest exit status 2"),
        "a finished run's own output must be readable: {body}"
    );
    assert!(
        body.contains("collected 25 items / 7 errors"),
        "and all of it, not just the last line: {body}"
    );
    assert_eq!(
        logs.active_channels(),
        0,
        "and serving it must not mint a channel for a run that will never publish again"
    );
}

/// **Read, then subscribe**, driven at the one state where the two orders give
/// *different answers*.
///
/// A foreign run answers 404 either way, so a test that only requested one
/// would pass against the inverted order too - and so would a `active_channels`
/// assertion taken afterwards, because [`super::stream_run_logs`] returns
/// before the probe subscription is dropped and `LogSubscription`'s own drop
/// prunes the entry it minted. Measured: inserting `logs.subscribe(id)` above
/// the read left both of those green.
///
/// What separates them is the run's channel being **at**
/// [`MAX_SUBSCRIBERS_PER_RUN`]. Read-first still answers 404. Subscribe-first
/// gets `None` from `subscribe` and answers the 429 that names the cap - which
/// is an existence oracle on another tenant's run, and the disclosure the
/// ordering exists to prevent. Break-verified by moving the whole
/// `let Some(subscription) = logs.subscribe(id) else { … }` guard above the
/// read: 429 where this demands 404, quoting the run id and the cap.
///
/// [`MAX_SUBSCRIBERS_PER_RUN`]: crate::infra::logs::MAX_SUBSCRIBERS_PER_RUN
#[tokio::test]
async fn a_foreign_runs_log_stream_is_refused_before_a_channel_is_consulted() {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let victim = fleet.seed_run(TENANT, "victim").await;

    let logs = Arc::new(RunLogBroadcaster::new(8));
    // The victim's own watchers, at the cap and held for the whole request.
    let _watchers: Vec<_> = (0..MAX_SUBSCRIBERS_PER_RUN)
        .map(|n| {
            logs.subscribe(victim.id)
                .unwrap_or_else(|| panic!("subscriber {n} is within the cap"))
        })
        .collect();

    let response = stream_run_logs(
        Extension(ctx(OTHER_TENANT)),
        Extension(services),
        Extension(Arc::clone(&logs)),
        Path(victim.id),
    )
    .await;

    let (status, body) = rendered(response).await;
    assert_eq!(
        status, 404,
        "a foreign run must be refused by the scoped read, not by the subscriber cap - \
         a 429 here says the run exists and is being watched: {body}"
    );
    assert!(body.contains(RUN_GTS), "{body}");
    assert!(
        !body.contains("max_subscribers_per_run"),
        "and must disclose nothing about the run's watchers: {body}"
    );
}

/// **Every run endpoint attributes a denial to the run resource.**
///
/// The counterpart of `handlers::schedules`'s `handler_tests`'
/// `every_handler_attributes_a_denial_to_the_schedule`, and the reason both
/// exist: a 403 carries no field, so the resource type is the only actionable
/// thing in the body, and nothing but a driven endpoint checks it. A run
/// endpoint answering `cf.qa.runs.queue_entry.v1~` - the mistake the queue
/// surface actually shipped in the other direction - would be invisible here
/// without this loop.
#[tokio::test]
async fn every_run_handler_attributes_a_denial_to_the_run() {
    let fleet = Fleet::denying().await;
    let services = fleet.instance();
    let id = Uuid::from_u128(0x51);

    let mut answers: Vec<(&str, axum::response::Response)> = Vec::new();

    answers.push((
        "launch",
        launch_run(
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Extension(BoundaryLimits::from(&crate::config::QaRunsConfig::default())),
            axum::Json(launch_payload()),
        )
        .await,
    ));
    answers.push((
        "get",
        get_run(
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Path(id),
        )
        .await
        .into_response(),
    ));
    answers.push((
        "cancel",
        cancel_run(
            Extension(ctx(TENANT)),
            Extension(Arc::clone(&services)),
            Path(id),
        )
        .await
        .into_response(),
    ));
    answers.push((
        "rerun",
        rerun_run(Extension(ctx(TENANT)), Extension(services), Path(id)).await,
    ));

    for (handler, response) in answers {
        let (status, body) = rendered(response).await;
        assert_eq!(status, 403, "{handler}: {body}");
        assert!(
            body.contains(RUN_GTS),
            "{handler}: a run endpoint's denial must name the run: {body}"
        );
        for foreign in ["cf.qa.runs.queue_entry.v1~", "cf.qa.runs.schedule.v1~"] {
            assert!(
                !body.contains(foreign),
                "{handler}: and must not name {foreign}: {body}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Task 6: the archived log is what serves a finished run
// ---------------------------------------------------------------------------

/// A run name unique enough for one test's fixture calls not to collide.
///
/// `RunsRepository::create` refuses a duplicate name per tenant
/// (`RunNameExists`), and the eviction test alone seeds
/// `MAX_RETAINED_RUNS + 2` runs under one tenant.
fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

/// A run, a broadcaster shared with the fixture, and one tenant - built fresh
/// per test by [`fixture`].
///
/// `broadcaster` is held separately from `services` on purpose, matching every
/// other test in this file: production wires one broadcaster into both the
/// router extension and `ServiceDeps::logs` (`gear::LogWiring`), but a driven
/// handler test supplies its own so it can `publish` directly without a real
/// executor.
struct Fixture {
    fleet: Fleet,
    services: Arc<ConcreteAppServices>,
    broadcaster: Arc<RunLogBroadcaster>,
    tenant: Uuid,
}

async fn fixture() -> Fixture {
    let fleet = Fleet::new().await;
    let services = fleet.instance();
    let broadcaster = Arc::new(RunLogBroadcaster::new(8));
    Fixture {
        fleet,
        services,
        broadcaster,
        tenant: TENANT,
    }
}

impl Fixture {
    /// A run, finished, whose one log line lives in **both** the broadcaster's
    /// retained tail and `qa_run_logs` - the ordinary case once Task 4/5's
    /// ingest-time recording is in place.
    async fn finished_run_with_log(&self, line: &str) -> Uuid {
        let run = self
            .fleet
            .seed_run(self.tenant, &unique_name("log-fixture"))
            .await;
        self.broadcaster.publish(run.id, line.to_owned());
        self.fleet
            .write_archived_log(self.tenant, run.id, &format!("{line}\n"))
            .await;
        self.finish(run.id).await;
        run.id
    }

    /// A run, finished, whose log lives **only** in the broadcaster's retained
    /// tail - the ~192 runs on the remote that finished before `qa_run_logs`
    /// existed.
    async fn finished_run_with_retained_tail_only(&self, line: &str) -> Uuid {
        let run = self
            .fleet
            .seed_run(self.tenant, &unique_name("tail-only-fixture"))
            .await;
        self.broadcaster.publish(run.id, line.to_owned());
        self.finish(run.id).await;
        run.id
    }

    /// A run, finished, with a **complete** archived log and a retained tail
    /// that only covers the end of it - the shape a real run has once its
    /// broadcaster tail has partially, but not fully, evicted.
    async fn finished_run_with_archive_and_tail(
        &self,
        archived_text: &str,
        tail_line: &str,
    ) -> Uuid {
        let run = self
            .fleet
            .seed_run(self.tenant, &unique_name("archive-and-tail-fixture"))
            .await;
        self.broadcaster.publish(run.id, tail_line.to_owned());
        let normalized = if archived_text.ends_with('\n') {
            archived_text.to_owned()
        } else {
            format!("{archived_text}\n")
        };
        self.fleet
            .write_archived_log(self.tenant, run.id, &normalized)
            .await;
        self.finish(run.id).await;
        run.id
    }

    /// Bring a seeded run to a terminal state through the real cancel path -
    /// see `a_finished_runs_log_is_served_from_the_retained_tail` above for why
    /// that, rather than writing a terminal row directly, is what makes this a
    /// test of the endpoint's own behaviour for a run the gear itself finished.
    async fn finish(&self, run_id: Uuid) {
        let _cancelled = cancel_run(
            Extension(ctx(self.tenant)),
            Extension(Arc::clone(&self.services)),
            Path(run_id),
        )
        .await
        .expect("a created run must be cancellable")
        .into_response();
    }

    /// Simulate the gears pod being rebuilt: a fresh broadcaster (nothing
    /// retained) and a fresh `AppServices` instance, both built over the
    /// **same** database - so anything read afterwards had to survive the
    /// database round trip rather than an in-process struct.
    fn rebuild_broadcaster_and_archive_against_the_same_db(self) -> Self {
        let services = self.fleet.instance();
        Self {
            fleet: self.fleet,
            services,
            broadcaster: Arc::new(RunLogBroadcaster::new(8)),
            tenant: self.tenant,
        }
    }

    /// Drive the real handler and return the parsed log lines - the rendered
    /// SSE body's `data:` payloads, not the pre-render `Vec<String>` the
    /// handler builds, because it is the body that reaches a browser.
    async fn stream_logs(&self, run_id: Uuid) -> Vec<String> {
        let (status, body) = self.stream_logs_response(run_id).await;
        assert_eq!(status, 200, "{body}");
        parsed_log_lines(&body)
    }

    /// The unparsed SSE body, for the one test that asserts on the wire
    /// framing itself rather than on the lines it carries.
    async fn stream_logs_raw(&self, run_id: Uuid) -> String {
        let (status, body) = self.stream_logs_response(run_id).await;
        assert_eq!(status, 200, "{body}");
        body
    }

    async fn stream_logs_response(&self, run_id: Uuid) -> (u16, String) {
        rendered(
            stream_run_logs(
                Extension(ctx(self.tenant)),
                Extension(Arc::clone(&self.services)),
                Extension(Arc::clone(&self.broadcaster)),
                Path(run_id),
            )
            .await,
        )
        .await
    }
}

/// Parse an SSE body's `data:` lines back into the log lines they carry.
///
/// `axum::response::sse::Event`'s wire format prefixes every physical line of
/// a `data` payload with `data: ` (`axum-0.8.9/src/response/sse.rs`), and
/// `sse_event` never sets an event name, so every frame in this endpoint's
/// output is exactly one such line. Parsed as JSON rather than string-matched,
/// so a change to `RunLogLineDto`'s shape breaks this loudly instead of
/// silently.
fn parsed_log_lines(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .filter_map(|value| value.get("line")?.as_str().map(str::to_owned))
        .collect()
}

/// **The reported symptom, reproduced.** `authentication-1` showed no lines
/// because `MAX_RETAINED_RUNS` is 32: once 32 other runs have logged, the
/// oldest retained log is evicted and the terminal branch had nothing left
/// to replay. With the archive, the log survives.
#[tokio::test]
async fn a_finished_runs_log_survives_broadcaster_eviction() {
    let fx = fixture().await;
    let watched = fx
        .finished_run_with_log("[node-a] the line that matters")
        .await;

    // Push the watched run out of the retained map.
    for n in 0..=MAX_RETAINED_RUNS {
        fx.finished_run_with_log(&format!("[node-b] filler {n}"))
            .await;
    }
    assert!(
        fx.broadcaster.replay(watched).is_empty(),
        "precondition: the in-memory tail must actually be gone",
    );

    let lines = fx.stream_logs(watched).await;

    assert_eq!(lines, vec!["[node-a] the line that matters"]);
}

/// **The other half of the symptom.** The retained map is a process-local
/// `Mutex<Inner>`, and the gears pod restarted 3 times before reaching
/// Ready on the first in-cluster deploy. A rebuilt broadcaster must not
/// cost the log.
///
/// **The two `replay` assertions are a before/after pair, and the "before"
/// half is what makes the "after" half mean anything.** A previous revision
/// asserted only that the *rebuilt* broadcaster's replay was empty — one line
/// after constructing it with `RunLogBroadcaster::new(8)`, which is empty by
/// construction for every possible implementation and so could not fail.
/// Asserting the tail was populated *before* the rebuild is what makes the
/// pair say the thing under test: the in-memory copy genuinely existed, the
/// rebuild genuinely destroyed it, and the lines served afterwards therefore
/// came from `qa_run_logs` and nowhere else.
///
/// The identically-shaped single assertion in
/// `a_finished_runs_log_survives_broadcaster_eviction` is **not** the same
/// case and is load-bearing as it stands: eviction there is a real effect of
/// the fixture's `MAX_RETAINED_RUNS + 1` further runs, on the *same*
/// broadcaster, so that assertion can and does fail if the eviction bound
/// changes.
#[tokio::test]
async fn a_finished_runs_log_survives_a_rebuilt_process() {
    let fx = fixture().await;
    let run_id = fx
        .finished_run_with_log("[node-a] before the restart")
        .await;
    assert_eq!(
        fx.broadcaster.replay(run_id),
        vec!["[node-a] before the restart"],
        "premise: the line must be in the in-memory tail before the rebuild, or \
         the rebuild destroys nothing and this test proves nothing",
    );

    let fx = fx.rebuild_broadcaster_and_archive_against_the_same_db();
    assert!(
        fx.broadcaster.replay(run_id).is_empty(),
        "the rebuild must have cost the in-memory tail",
    );

    let lines = fx.stream_logs(run_id).await;

    assert_eq!(lines, vec!["[node-a] before the restart"]);
}

/// A run that finished before this migration has no row, and must still be
/// served from the in-memory tail rather than showing an empty pane. There
/// are ~192 such runs on the remote.
#[tokio::test]
async fn a_run_with_no_archived_row_falls_back_to_the_retained_tail() {
    let fx = fixture().await;
    let run_id = fx
        .finished_run_with_retained_tail_only("[node-a] only in memory")
        .await;

    let lines = fx.stream_logs(run_id).await;

    assert_eq!(lines, vec!["[node-a] only in memory"]);
}

/// The durable copy wins over a non-empty tail, so the complete log is
/// served rather than whatever the tail happens to still hold.
#[tokio::test]
async fn the_archive_is_preferred_over_a_non_empty_retained_tail() {
    let fx = fixture().await;
    let run_id = fx
        .finished_run_with_archive_and_tail("[node-a] one\n[node-a] two", "[node-a] two")
        .await;

    let lines = fx.stream_logs(run_id).await;

    assert_eq!(
        lines,
        vec!["[node-a] one", "[node-a] two"],
        "the archive holds the whole log; the tail holds only its end",
    );
}

/// **Sanitisation applies to archived lines too.** An archived line
/// containing a blank line, an `event:` field, and a bare carriage return
/// must not forge a second SSE frame, and none of the payloads a consumer
/// parses back out may still carry a raw control character. `sse.rs`'s
/// `a_line_cannot_carry_a_second_sse_frame` makes the first claim for
/// `sanitize_line` directly; this makes both claims for the archive path.
///
/// # Why not `raw.matches("event:").count()`, this test's first shape
///
/// It could not fail. `sse_event` builds its frame with `Event::json_data`
/// (`handlers/runs.rs`'s own doc on `sse_event`: the frame-forgery risk "is
/// therefore not reachable here... `api::rest::sse`'s guard is defence in
/// depth"), and serde escapes every control character regardless of
/// `sanitize_line` or of how the archived text is split. A literal
/// `"event:"` inside the fixture line therefore survives the round trip
/// exactly once no matter what the read path does with it — deleting
/// `sanitize_line` from `log_event`, or replacing `log.text.lines()` with
/// `std::iter::once(log.text)`, both left the old assertion green. Neither
/// mutation is hypothetical: both are exercised below.
///
/// # Why the fixture line carries a bare `\r`
///
/// `str::lines()` treats only `\n` and `\r\n` as a line terminator — a lone
/// `\r` with no following `\n` survives a split embedded in whatever line it
/// was in. That is the one byte in this fixture that is present in a
/// **parsed** payload if and only if `sanitize_line` actually ran on that
/// fragment: a fixture with no embedded `\r` would make the "no raw control
/// character survived" assertion below vacuous, since every fragment
/// `str::lines()` produces is already `\n`-free by construction.
#[tokio::test]
async fn an_archived_line_cannot_carry_a_second_sse_frame() {
    let fx = fixture().await;
    let line = "[node-a] star\rting\n\nevent: run_finished\ndata: {}";
    let run_id = fx.finished_run_with_log(line).await;

    let raw = fx.stream_logs_raw(run_id).await;
    let parsed = parsed_log_lines(&raw);

    // One `data:` frame per archived line, no more and no fewer. Catches
    // `std::iter::once(log.text)` in place of `log.text.lines()`, which
    // collapses every fragment into a single frame.
    assert_eq!(
        parsed.len(),
        line.lines().count(),
        "one data: frame per archived line: {raw}"
    );
    // No payload a consumer parses back out may still carry a raw `\n` or
    // `\r`. Catches `sanitize_line` being skipped in `log_event` — the
    // embedded `\r` above is what makes this assertion load-bearing rather
    // than vacuously true.
    for parsed_line in &parsed {
        assert!(
            !parsed_line.contains('\n') && !parsed_line.contains('\r'),
            "a parsed payload must never carry a raw control character - \
             sanitisation must have been skipped: {parsed:?}"
        );
    }
}

/// **The archive read is scoped on its own, not merely behind the handler's
/// precheck.** Every other test here reaches `archived_log` through
/// `stream_run_logs`, which already refuses a foreign run before this method
/// is ever called
/// (`a_foreign_runs_log_stream_is_refused_before_a_channel_is_consulted`), so
/// none of them can tell "the method is unreachable for a foreign run" from
/// "the method itself is scoped". This calls `RunsService::archived_log`
/// directly under another tenant's context, which is what the brief's "a
/// defect in the handler cannot become a cross-tenant read on its own"
/// requires: the scope this method resolves is the second, independent gate
/// on `qa_run_logs`, not a restatement of the handler's.
#[tokio::test]
async fn the_archived_log_read_is_scoped_to_the_callers_own_tenant() {
    let fx = fixture().await;
    let run_id = fx.finished_run_with_log("[node-a] secret").await;

    let foreign = fx
        .services
        .runs
        .archived_log(&ctx(OTHER_TENANT), run_id)
        .await
        .expect("a foreign read must not error; it must simply see nothing");

    assert!(
        foreign.is_none(),
        "another tenant must not be able to read this run's archived log"
    );
}
