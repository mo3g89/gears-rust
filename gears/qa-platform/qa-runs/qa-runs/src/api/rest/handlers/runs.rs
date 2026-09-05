//! Handlers for `/qa/v1/runs`.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::Stream;
use futures::stream::StreamExt as _;
use http::StatusCode;
use qa_runs_sdk::LaunchOutcome;
use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_security::SecurityContext;
use tracing::{debug, info};
use uuid::Uuid;

use crate::api::rest::dto::{
    BoundaryLimits, LaunchRunReq, QueuedRunDto, RunDetailDto, RunDto, RunLogLineDto, RunResultDto,
    RunTestResultDto,
};
use crate::api::rest::error::RunResourceError;
use crate::api::rest::sse::{KEEP_ALIVE_INTERVAL, MAX_STREAM_DURATION, log_event};
use crate::domain::state_machine::is_terminal;
use crate::infra::ConcreteAppServices;
use crate::infra::logs::{MAX_SUBSCRIBERS_PER_RUN, RunLogBroadcaster};

/// The gear's configured ceilings, layered alongside the services so a handler
/// can validate against them without reaching for global state.
pub type Limits = Extension<BoundaryLimits>;

/// `POST /qa/v1/runs`
///
/// **Two success bodies at two statuses**, which is why this returns the erased
/// [`Response`] rather than a typed `Json<T>`: 200 carries the started run, 202
/// carries the two ids of a queued one. The distinction is the launch contract
/// (`cpt-cf-qa-fr-runs-launch`), not a detail — a CI caller polls on 202 and
/// reads results on 200.
///
/// Mirrors `gears/bss/ledger/ledger/src/api/rest/recognition.rs`, whose
/// `run_response` branches on the domain outcome the same way.
#[tracing::instrument(skip(svc, limits, ctx, req))]
pub async fn launch_run(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Extension(limits): Limits,
    Json(req): Json<LaunchRunReq>,
) -> Response {
    // Boundary validation first: it needs no I/O, so a request that cannot
    // possibly succeed is refused before a git sync or a bundle build.
    let request = match req.into_domain(limits) {
        Ok(request) => request,
        Err(error) => return CanonicalError::from(error).into_response(),
    };

    match svc.launch.launch(&ctx, request).await {
        Ok(outcome) => launch_response(outcome),
        Err(error) => CanonicalError::from(error).into_response(),
    }
}

/// `POST /qa/v1/runs/{id}/rerun`
///
/// The same two-outcome shape as [`launch_run`]: a re-run goes through the one
/// creation path, so it can be admitted inline or queued exactly as a fresh
/// launch can.
#[tracing::instrument(skip(svc, ctx), fields(run.id = %id))]
pub async fn rerun_run(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> Response {
    match svc.runs.rerun(&ctx, id).await {
        Ok(outcome) => launch_response(outcome),
        Err(error) => CanonicalError::from(error).into_response(),
    }
}

/// Render a [`LaunchOutcome`] at the status its variant means.
///
/// One function for both entry points, because the launch and re-run contracts
/// are the same contract and two copies of this `match` would be two places for
/// the status mapping to drift.
fn launch_response(outcome: LaunchOutcome) -> Response {
    match outcome {
        LaunchOutcome::Started { run } => {
            (StatusCode::OK, Json(RunDto::from(*run))).into_response()
        }
        LaunchOutcome::Queued { run_id, queue_id } => (
            StatusCode::ACCEPTED,
            Json(QueuedRunDto { run_id, queue_id }),
        )
            .into_response(),
    }
}

/// `GET /qa/v1/runs`
///
/// The `OData` query is handed to the service untouched; the service resolves
/// the caller's `AccessScope` from the policy enforcer first and the repository
/// composes the two. A `$filter` cannot widen what this returns.
#[tracing::instrument(skip(svc, ctx, query))]
pub async fn list_runs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<RunDto>> {
    // Each item is a `RunWithResult`, not a bare `Run` — Task 10: the run list
    // needs to show `skipped` alongside the verdict, and the row already
    // carries both.
    let page = svc.runs.list(&ctx, &query).await?;
    Ok(Json(page.map_items(RunDto::from)))
}

/// `GET /qa/v1/runs/{id}`
///
/// The run, its counters and its per-test rows in one response — see
/// [`RunDetailDto`] for why they are not three endpoints.
#[tracing::instrument(skip(svc, ctx), fields(run.id = %id))]
pub async fn get_run(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RunDetailDto>> {
    let run = svc.runs.get(&ctx, id).await?;
    let result = svc.runs.get_result(&ctx, id).await?;
    let test_results = svc.runs.test_results(&ctx, id).await?;
    let run_dto = RunDto {
        result: RunResultDto::from(result),
        ..RunDto::from(run)
    };
    Ok(Json(RunDetailDto {
        run: run_dto,
        test_results: test_results
            .into_iter()
            .map(RunTestResultDto::from)
            .collect(),
    }))
}

/// `POST /qa/v1/runs/{id}/cancel`
///
/// 204 rather than the cancelled run: cancel is idempotent on an
/// already-terminal run, so a body would invite a caller to diff two responses
/// that are deliberately identical.
#[tracing::instrument(skip(svc, ctx), fields(run.id = %id))]
pub async fn cancel_run(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.runs.cancel(&ctx, id).await?;
    Ok(no_content().into_response())
}

/// `GET /qa/v1/runs/{id}/logs` — live log lines as SSE.
///
/// Returns the erased [`Response`] rather than a typed body for the reason
/// mini-chat's streaming handler does
/// (`gears/mini-chat/mini-chat/src/api/rest/handlers/messages.rs`): everything
/// before the stream opens must still be able to answer a canonical JSON error,
/// while the success path answers `text/event-stream`.
///
/// # The authorization happens before `subscribe`, and the order is the point
///
/// `RunLogBroadcaster::subscribe` mints a channel entry for **any** UUID and
/// cannot authorize — it holds no repository and no `SecurityContext`, and says
/// so at its own definition. Subscribing first and checking afterwards would let
/// an unauthenticated-for-that-run caller create one map entry per request,
/// keyed by an id they chose, for runs that need not exist. So the run is read
/// under the caller's own `qa.run`/`get` scope first, and a run that is absent
/// *or* another tenant's answers 404 before any channel exists.
///
/// # A finished run is not subscribed to at all
///
/// A terminal run's channel has already been reaped, so nothing will ever be
/// published to it; subscribing would mint a fresh entry that only the
/// connection's own drop cleans up. Opening the log view of a finished run is
/// the *common* case, so this is not an edge: the response is a well-formed but
/// immediately-complete stream, and the archived log is where that run's output
/// lives.
///
/// **It is a check, not a lock.** A run that terminates and is reaped *between*
/// this read and `subscribe` below takes the same path a live run does: the
/// subscription is minted, `recv()` never returns `None` because the reap
/// already happened, and the client holds an idle connection until
/// [`MAX_STREAM_DURATION`] cuts it. The window is one scoped read wide and the
/// cost is one idle connection, so it is not worth a lock over the broadcaster
/// - but "a terminal run is not subscribed to at all" is true only outside it,
/// and `MAX_STREAM_DURATION` is what bounds the difference.
///
/// # The stream is bounded in time
///
/// A subscription ends by itself only when the run is reaped. A run wedged
/// mid-execution never is, so the relay is cut at
/// [`MAX_STREAM_DURATION`]. What that does *not* do is documented on the
/// constant: a legitimately longer run has its stream closed and must reconnect.
///
/// # The ordering is pinned by execution
///
/// [`handler_tests::a_foreign_runs_log_stream_is_refused_before_a_channel_is_consulted`]
/// drives this handler over a real `ConcreteAppServices` from
/// `domain::service::test_support::Fleet` and asserts both halves at once: the
/// 404, and that the broadcaster still holds **no** channel afterwards.
/// Swapping the two statements below turns it red on the second assertion.
///
/// This paragraph used to say no test drove the handler because doing so
/// needed *"the crate-wide test harness that arrives with the gear
/// bootstrap"*. `Fleet::instance` had been returning a wired
/// `ConcreteAppServices` since Task 19; `handlers::schedules`'s own
/// `handler_tests` exists because the same sentence was written there twice. This was its
/// seventh appearance in the gear.
///
/// Still pinned either side of the handler, and still worth keeping because
/// they are properties of the collaborators rather than of the ordering: that
/// `RunLogBroadcaster::subscribe` mints a channel for an id it cannot authorize
/// (`infra::logs::broadcast`,
/// `subscribe_mints_a_channel_for_an_id_it_cannot_authorize`), and that
/// `RunsService::get` refuses another tenant's run
/// (`domain::service::runs`, `reads_are_scoped_to_the_callers_own_tenant`).
///
/// # `logs` is skipped, and it is a cross-tenant disclosure if it is not
///
/// `#[tracing::instrument]` records every argument it is not told to skip -
/// the rule `handlers::schedules::create_schedule` records for its own `Uri`
/// leak. [`RunLogBroadcaster`] derives `Debug` over its channel map, and
/// `Mutex`'s `Debug` prints the data it can lock, so an unskipped `logs`
/// opened a span enumerating **every run streaming on this replica** - other
/// tenants' included. Measured, not reasoned about:
/// `stream_run_logs{logs=RunLogBroadcaster { channels: Mutex { data:
/// {…-0bbb: broadcast::Sender}, … } … }`, inherited by every child span of a
/// request that had named only its own run.
///
/// [`handler_tests::the_log_stream_span_names_only_the_run_that_was_addressed`]
/// is what keeps it skipped; it fails on exactly that rendering.
#[tracing::instrument(skip(svc, ctx, logs), fields(run.id = %id))]
pub async fn stream_run_logs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    // The **concrete** broadcaster, layered beside the services rather than
    // reached through them. `domain::service::LogFanout` is the publishing
    // port and deliberately has no `subscribe`: a subscription hands back
    // `infra::logs::LogSubscription`, and putting that in a domain trait would
    // put an infrastructure type in a domain signature. The gear bootstrap
    // builds one broadcaster and hands it to both - as `dyn LogFanout` for the
    // services, and as itself here.
    Extension(logs): Extension<Arc<RunLogBroadcaster>>,
    Path(id): Path<Uuid>,
) -> Response {
    // The PEP check. `get` resolves a `qa.run` scope and reads under it, so
    // absent and foreign are indistinguishable 404s - the property every read
    // in this gear is written to preserve.
    let run = match svc.runs.get(&ctx, id).await {
        Ok(run) => run,
        Err(error) => return CanonicalError::from(error).into_response(),
    };

    // A FINISHED RUN IS SERVED FROM ITS ARCHIVED LOG, AND FROM THE RETAINED
    // TAIL ONLY WHEN IT HAS NONE.
    //
    // Two revisions ago this was `stream::empty()`, on the stated grounds that
    // the output "is in the archived log" — which was false, because nothing
    // wrote one. The revision after that served the broadcaster's retained
    // tail, which was true but memory-only: `MAX_RETAINED_RUNS` is 32, and the
    // map dies with the process. `authentication-1` showed zero lines for
    // exactly that reason.
    //
    // The fallback is not vestigial. It serves every run that finished before
    // `qa_run_logs` existed — ~192 of them on the remote at the time of
    // writing.
    //
    // Still no channel is minted for a terminal run, so a finished run nobody
    // watched costs nothing here, and the stream stays finite, which is what
    // lets the UI's `EventSource` stop retrying.
    //
    // THE ARCHIVE ARM IS UNBOUNDED, BY A DECISION THAT IS NOT THIS HANDLER'S
    // TO REVISIT.
    //
    // `qa_run_logs.text` has no size cap - the design's §8 records that as a
    // decision made with the risk stated to the user, not an oversight. This
    // branch also `return`s before `logs.subscribe_with_replay` below, so
    // `MAX_SUBSCRIBERS_PER_RUN` does not gate it: nothing stops one caller
    // issuing N concurrent GETs against one large finished run. `lines_as_events`
    // is what keeps that to one resident copy of `text` per request rather than
    // two - `text.lines().map(str::to_owned).collect()` would hold a second,
    // roughly-equal-sized `Vec<String>` alongside it for the life of the
    // response - but one uncapped copy times N concurrent readers is still
    // unbounded. `infra::logs::broadcast::truncation_marker` and
    // `MAX_RETAINED_BYTES_PER_RUN` are the shape a cap would take on this arm
    // if one is ever wanted; none is added here, because that reopens a
    // decision the user made explicitly and is recorded, not one this task
    // found reason to override.
    if is_terminal(run.state) {
        let archived = match svc.runs.archived_log(&ctx, id).await {
            Ok(archived) => archived,
            Err(error) => return CanonicalError::from(error).into_response(),
        };
        return if let Some(log) = archived {
            debug!(
                state = run.state.as_str(),
                lines = log.lines,
                archived = true,
                "log requested for a finished run",
            );
            sse_response(futures::stream::iter(lines_as_events(log.text)))
        } else {
            let replay = logs.replay(id);
            debug!(
                state = run.state.as_str(),
                lines = replay.len(),
                archived = false,
                "log requested for a finished run",
            );
            sse_response(futures::stream::iter(
                replay
                    .into_iter()
                    .map(|line| Ok::<_, Infallible>(sse_event(&line))),
            ))
        };
    }

    // Refused when this run already has `MAX_SUBSCRIBERS_PER_RUN` watchers.
    // A resource-exhausted answer rather than a queue: the cap exists because
    // nothing else bounds how many connections one caller can hold, and making
    // them wait would hold the connection anyway.
    //
    // `subscribe_with_replay` rather than `subscribe`, so a reader that opens the
    // pane after the pod has started printing still gets what it missed. The two
    // halves come out of one lock acquisition, so no line is lost or repeated
    // between them.
    let Some((replay, subscription)) = logs.subscribe_with_replay(id) else {
        return subscriber_cap_reached(id).into_response();
    };
    info!(replayed = replay.len(), "live log subscription opened");
    let live = futures::stream::unfold(subscription, |mut subscription| async move {
        subscription.recv().await.map(|line| (line, subscription))
    });
    let lines = futures::stream::iter(replay).chain(live);

    sse_response(lines.map(|line| Ok::<_, Infallible>(sse_event(&line))))
}

/// Wrap a stream of pre-built events as the SSE response.
fn sse_response<S>(stream: S) -> Response
where
    S: Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    let bounded = stream.take_until(tokio::time::sleep(MAX_STREAM_DURATION));
    Sse::new(bounded)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE_INTERVAL))
        .into_response()
}

/// The refusal a run at its subscriber cap answers.
///
/// A named function rather than an inline builder chain, so the branch is
/// reachable from a test without constructing a request, a database and four
/// cross-gear clients - which is why it had no coverage.
fn subscriber_cap_reached(id: Uuid) -> CanonicalError {
    RunResourceError::resource_exhausted(format!(
        "run {id} already has the maximum number of live log subscribers"
    ))
    .with_quota_violation(
        "max_subscribers_per_run",
        format!("at most {MAX_SUBSCRIBERS_PER_RUN} live log streams per run"),
    )
    .with_resource(id.to_string())
    .create()
}

/// One log line as an SSE `data:` payload.
///
/// # Two independent guards, and only one of them is load-bearing
///
/// `json_data` serializes the payload, and serde escapes control characters -
/// so on the shipped path a newline in a log line becomes `\n` inside a JSON
/// string and cannot terminate an SSE frame. **The frame-forgery risk is
/// therefore not reachable here**, and `api::rest::sse`'s guard is
/// defence-in-depth rather than the thing standing between the execution plane
/// and the browser.
///
/// It is kept because the fallback arm below is not JSON: if serialization ever
/// fails, `Event::data` writes the string raw, and that arm *is* forgeable. The
/// sanitizer is also what applies [`MAX_LINE_BYTES`], which serde does not.
///
/// [`MAX_LINE_BYTES`]: crate::api::rest::sse::MAX_LINE_BYTES
fn sse_event(line: &str) -> Event {
    let payload: RunLogLineDto = log_event(line);
    Event::default()
        .json_data(&payload)
        .unwrap_or_else(|_| Event::default().data(payload.line))
}

/// Turn one archived run's whole text into a lazy sequence of SSE events, one
/// per logical line — without ever holding a second, roughly-text-sized
/// `Vec<String>` of every line alongside it.
///
/// # Why not `text.lines().map(str::to_owned).collect::<Vec<_>>()`
///
/// That is what the terminal branch did until this function replaced it, and
/// it is a real cost rather than a tidy one: `qa_run_logs.text` has no size
/// cap (design §8, a decision made with the risk stated to the user), and
/// this branch is not gated by [`MAX_SUBSCRIBERS_PER_RUN`] — it `return`s
/// before `subscribe_with_replay` is ever reached. A `Vec<String>` collecting
/// every line holds close to `text.len()` bytes again, on top of `text`
/// itself, for the entire life of the SSE response — so N concurrent readers
/// of one large finished run resident roughly `2 * text.len() * N`. This
/// function keeps exactly one copy of `text` alive, wrapped in an [`Arc<str>`]
/// so every line borrows from it rather than copying it, and produces one
/// line's [`Event`] at a time as the stream is polled.
///
/// **This does not cap the archive read** — seeing "one copy instead of two"
/// still leaves one uncapped copy times N readers. A cap on this arm would
/// take the shape [`crate::infra::logs::broadcast::truncation_marker`] and
/// [`crate::infra::logs::broadcast::MAX_RETAINED_BYTES_PER_RUN`] already take
/// on the retained tail; none is added here, because "no cap on
/// `qa_run_logs.text`" is a decision on record, not an oversight this task
/// found reason to override on its own.
///
/// # Re-splitting rather than reusing `str::lines()`
///
/// `str::lines()` borrows from `text`, and a `String` cannot be moved into a
/// closure while something still borrows it — so a hand-rolled scan over byte
/// offsets is what lets the [`Arc<str>`] be captured by value and searched
/// again on each call, matching `str::lines()`'s own rule for what a line is:
/// split on `\n` or `\r\n`, no trailing empty line for a string that ends
/// with one.
fn lines_as_events(text: String) -> impl Iterator<Item = Result<Event, Infallible>> {
    let text: Arc<str> = Arc::from(text);
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        if pos >= text.len() {
            return None;
        }
        let rest = &text[pos..];
        let (line, advance) = match rest.find('\n') {
            Some(newline_at) => {
                // A `\r` immediately before the `\n` is the CRLF terminator
                // and is stripped too; a `\r` anywhere else in the line is
                // ordinary content and is left alone, exactly as
                // `str::lines()` treats it.
                let mut end = newline_at;
                if end > 0 && rest.as_bytes()[end - 1] == b'\r' {
                    end -= 1;
                }
                (&rest[..end], newline_at + 1)
            }
            // The final, unterminated line - `str::lines()` still yields it
            // when it is non-empty, which `pos >= text.len()` above already
            // guarantees here.
            None => (rest, rest.len()),
        };
        let event = sse_event(line);
        pos += advance;
        Some(Ok(event))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "runs_handler_tests.rs"]
mod handler_tests;

#[cfg(test)]
mod tests {
    use super::sse_event;
    use crate::api::rest::sse::MAX_LINE_BYTES;

    /// The 429 branch: a run at its subscriber cap answers
    /// `resource_exhausted`, naming the setting rather than the number alone.
    ///
    /// Only `subscribe()` returning `None` was covered; the handler arm that
    /// turns it into a response had no test, so a branch that returned 500 - or
    /// a bare 200 with an empty stream - would have passed.
    #[test]
    fn a_run_at_its_subscriber_cap_answers_resource_exhausted() {
        use toolkit::api::canonical_prelude::CanonicalError;

        let id = uuid::Uuid::from_u128(0x5AFE);
        let error: CanonicalError = super::subscriber_cap_reached(id);

        assert_eq!(error.status_code(), 429);
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("max_subscribers_per_run"),
            "the refusal must name the setting an operator would change: {rendered}"
        );
        assert!(rendered.contains(&id.to_string()), "{rendered}");
    }

    /// **The sanitizer is applied**, not merely available.
    ///
    /// `api::rest::sse` pins `sanitize_line` as a function thoroughly; nothing
    /// pinned that the boundary calls it. Keeping the `log_event` call and
    /// overwriting its result with the raw line - the mutation this catches -
    /// left the whole suite and clippy green while putting unbounded
    /// execution-plane bytes on the wire.
    ///
    /// Asserted through the rendered event rather than the payload, because
    /// what reaches a client is what `Event` writes.
    #[test]
    fn the_boundary_applies_the_sanitizer_to_what_it_emits() {
        let hostile = format!(
            "start\n\nevent: forged\ndata: {}",
            "x".repeat(MAX_LINE_BYTES)
        );
        let rendered = format!("{:?}", sse_event(&hostile));

        assert!(
            rendered.contains("line truncated"),
            "the byte cap must be applied at the boundary: {rendered}"
        );
        // The sanitized line keeps at most `MAX_LINE_BYTES` bytes *in total*,
        // and the hostile prefix eats some of them - so the full run of `x`
        // cannot survive. Asserting on the payload rather than on a length
        // keeps this immune to however `Event` renders itself.
        assert!(
            !rendered.contains(&"x".repeat(MAX_LINE_BYTES)),
            "an over-long line must not reach the wire whole"
        );
        // The newlines are gone from the payload itself - serde would have
        // escaped them anyway, which is why this is the weaker of the two
        // assertions and the byte cap above is the one that only this layer
        // provides.
        assert!(!rendered.contains("\\n\\nevent:"), "{rendered}");
    }

    /// **`lines_as_events`'s hand-rolled scan must split exactly where
    /// `str::lines()` would**, across every boundary shape that matters: a
    /// trailing newline (no phantom empty line), no trailing newline (the
    /// dangling tail is still a line), a blank line in the middle, a CRLF
    /// pair (stripped as one unit), and a lone `\r` with no following `\n`
    /// (left as ordinary content, not treated as a terminator).
    ///
    /// Counting events is what actually exercises the scan: `sse_event`
    /// sanitizes every line the same way regardless of where it came from, so
    /// a wrong split boundary would still produce syntactically valid events,
    /// just the wrong number of them, merging two lines or inventing an empty
    /// one. That is exactly the class of bug a byte-offset reimplementation of
    /// `str::lines()` can introduce silently.
    #[test]
    fn lines_as_events_splits_exactly_where_str_lines_does() {
        let cases = [
            "",
            "one line, no trailing newline",
            "a\nb\nc",
            "a\nb\nc\n",
            "a\n\nb",
            "a\r\nb\r\nc",
            "a\rb\nc",
            "\n",
        ];
        for text in cases {
            let expected = text.lines().count();
            let actual = super::lines_as_events(text.to_owned()).count();
            assert_eq!(
                actual, expected,
                "text {text:?}: str::lines() yields {expected} lines, \
                 lines_as_events yielded {actual}"
            );
        }
    }

    /// The content of each line survives the scan, not just the count -
    /// including the CRLF terminator being fully stripped and a lone `\r`
    /// mid-line surviving (until `sanitize_line` flattens it to a space, the
    /// same treatment a real `\n` gets).
    #[test]
    fn lines_as_events_preserves_each_lines_content() {
        let events: Vec<_> = super::lines_as_events("alpha\r\nbe\rta\ngamma".to_owned())
            .map(|event| format!("{:?}", event.expect("infallible")))
            .collect();

        assert_eq!(events.len(), 3, "{events:?}");
        assert!(events[0].contains("alpha"), "{events:?}");
        // The lone `\r` in "be\rta" is not a CRLF terminator, so it stays part
        // of one line's content until `sanitize_line` turns it into a space.
        assert!(events[1].contains("be ta"), "{events:?}");
        assert!(events[2].contains("gamma"), "{events:?}");
    }
}
